//! Read-only Graph ingestion plus the existing durable Gmail forwarding queue.
use crate::{
    cloud_auth::{self, Config},
    db, forward, gmail, importer,
};
use anyhow::{Context, Result, bail};
use reqwest::{Url, blocking::Client};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub fn schema(conn: &Connection) -> Result<()> {
    cloud_auth::schema(conn)?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS cloud_settings (
            singleton INTEGER PRIMARY KEY CHECK(singleton=1),
            start_at TEXT, enabled INTEGER NOT NULL DEFAULT 0,
            graph_cursor TEXT, last_sync INTEGER, last_error TEXT
        );
        INSERT OR IGNORE INTO cloud_settings(singleton) VALUES(1);
        CREATE TABLE IF NOT EXISTS graph_imports (
            graph_id TEXT PRIMARY KEY, message_id_fk INTEGER NOT NULL REFERENCES messages(id)
        );
    "#,
    )?;
    Ok(())
}

#[derive(Serialize)]
pub struct Settings {
    pub start_at: Option<String>,
    pub enabled: bool,
    pub last_sync: Option<i64>,
    pub last_error: Option<String>,
}
pub fn settings(conn: &Connection) -> Result<Settings> {
    Ok(conn.query_row(
        "SELECT start_at,enabled,last_sync,last_error FROM cloud_settings WHERE singleton=1",
        [],
        |r| {
            Ok(Settings {
                start_at: r.get(0)?,
                enabled: r.get(1)?,
                last_sync: r.get(2)?,
                last_error: r.get(3)?,
            })
        },
    )?)
}

pub fn initialize(conn: &Connection, config: &Config, since: Option<&str>) -> Result<()> {
    if cloud_auth::connection_email(conn, "google")?.as_deref() != Some(&config.owner_gmail)
        || cloud_auth::connection_email(conn, "microsoft")?.is_none()
    {
        bail!("connect both accounts before setting the starting position");
    }
    if settings(conn)?.start_at.is_some() {
        bail!("starting position is already set and cannot be reset");
    }
    let date = match since {
        Some(value) => OffsetDateTime::parse(value, &Rfc3339).context("invalid starting date")?,
        None => OffsetDateTime::now_utc().replace_nanosecond(0)?,
    };
    if date > OffsetDateTime::now_utc() {
        bail!("starting date cannot be in the future");
    }
    let since = date.to_offset(time::UtcOffset::UTC).format(&Rfc3339)?;
    let maximum: i64 =
        conn.query_row("SELECT COALESCE(MAX(id),0) FROM messages", [], |r| r.get(0))?;
    // Forward newly imported Graph messages regardless of original sender Date.
    // The Graph ingestion boundary below uses Microsoft's receivedDateTime.
    let tx = conn.unchecked_transaction()?;
    if conn.query_row("SELECT COUNT(*) FROM forward_config", [], |r| {
        r.get::<_, i64>(0)
    })? != 0
    {
        bail!("use a separate data directory for web mode; local forwarding is already configured");
    }
    tx.execute(
        "INSERT INTO forward_config(singleton,gmail,after_id,since) VALUES(1,?1,?2,NULL)",
        params![config.owner_gmail, maximum],
    )?;
    tx.execute(
        "UPDATE cloud_settings SET start_at=?1 WHERE singleton=1",
        [since],
    )?;
    tx.commit()?;
    Ok(())
}

#[derive(Deserialize)]
pub struct DeltaPage {
    pub value: Vec<GraphMessage>,
    #[serde(rename = "@odata.nextLink")]
    pub next: Option<String>,
    #[serde(rename = "@odata.deltaLink")]
    pub delta: Option<String>,
}
#[derive(Deserialize)]
pub struct GraphMessage {
    pub id: String,
    #[serde(rename = "receivedDateTime")]
    pub received_at: Option<String>,
    #[serde(rename = "@removed")]
    pub removed: Option<serde_json::Value>,
}

pub trait GraphReader {
    /// None indicates expired delta state. All methods are read-only.
    fn page(&mut self, url: &str) -> Result<Option<DeltaPage>>;
    fn mime(&mut self, id: &str) -> Result<Option<Vec<u8>>>;
}

pub struct HttpGraph {
    client: Client,
    token: String,
}
impl HttpGraph {
    pub fn new(token: String) -> Result<Self> {
        Ok(Self {
            client: gmail::http_client()?,
            token,
        })
    }
}

pub fn validate_graph_link(value: &str) -> Result<Url> {
    let url = Url::parse(value)?;
    if url.scheme() != "https"
        || url.host_str() != Some("graph.microsoft.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !(url.path().starts_with("/v1.0/me/mailFolders/")
            || url.path().starts_with("/v1.0/me/mailFolders("))
        || !url.path().ends_with("/messages/delta")
    {
        bail!("invalid Graph delta link");
    }
    Ok(url)
}
impl GraphReader for HttpGraph {
    fn page(&mut self, url: &str) -> Result<Option<DeltaPage>> {
        let response = self
            .client
            .get(validate_graph_link(url)?)
            .bearer_auth(&self.token)
            .header("Prefer", "IdType=\"ImmutableId\", odata.maxpagesize=50")
            .send()
            .map_err(|_| anyhow::anyhow!("Graph connection failed"))?;
        if response.status().as_u16() == 410 {
            return Ok(None);
        }
        if !response.status().is_success() {
            bail!("Graph read rejected; check consent or reconnect university mail");
        }
        serde_json::from_slice(&cloud_auth::bounded_bytes(response, 2 * 1024 * 1024)?)
            .map(Some)
            .map_err(|_| anyhow::anyhow!("invalid Graph delta response"))
    }
    fn mime(&mut self, id: &str) -> Result<Option<Vec<u8>>> {
        let mut url = Url::parse("https://graph.microsoft.com")?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid Graph base URL"))?
            .extend(["v1.0", "me", "messages", id, "$value"]);
        let response = self
            .client
            .get(url)
            .bearer_auth(&self.token)
            .header("Prefer", "IdType=\"ImmutableId\"")
            .send()
            .map_err(|_| anyhow::anyhow!("Graph MIME connection failed"))?;
        if response.status().as_u16() == 404 {
            return Ok(None);
        } // moved/deleted by the mailbox owner
        if !response.status().is_success() {
            bail!("Graph MIME read rejected");
        }
        Ok(Some(cloud_auth::bounded_bytes(response, 32 * 1024 * 1024)?))
    }
}

#[derive(Default, Serialize)]
pub struct SyncStats {
    pub imported: usize,
    pub existing: usize,
    pub excluded: usize,
}

fn initial_link(start: &str) -> Result<String> {
    let mut url =
        Url::parse("https://graph.microsoft.com/v1.0/me/mailFolders/inbox/messages/delta")?;
    url.query_pairs_mut().extend_pairs([
        ("$select", "id,receivedDateTime"),
        ("$filter", &format!("receivedDateTime ge {start}")),
    ]);
    Ok(url.into())
}

/// Caller holds forward.lock for the complete sync/forward cycle.
pub fn sync_with(
    conn: &Connection,
    config: &Config,
    reader: &mut impl GraphReader,
) -> Result<SyncStats> {
    let start = settings(conn)?
        .start_at
        .context("set a starting position first")?;
    let boundary = OffsetDateTime::parse(&start, &Rfc3339)?;
    let cursor: Option<String> = conn.query_row(
        "SELECT graph_cursor FROM cloud_settings WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    let mut link = cursor.unwrap_or(initial_link(&start)?);
    let mut stats = SyncStats::default();
    for _ in 0..5 {
        // Continue remaining pages on the next cycle rather than monopolize the server.
        let Some(page) = reader.page(&link)? else {
            conn.execute(
                "UPDATE cloud_settings SET graph_cursor=NULL WHERE singleton=1",
                [],
            )?;
            return Ok(stats); // replay the original explicit range next cycle; local identities deduplicate
        };
        for item in page.value {
            if item.removed.is_some() {
                stats.excluded += 1;
                continue;
            } // never delete university/local data
            let known: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM graph_imports WHERE graph_id=?1)",
                [&item.id],
                |r| r.get(0),
            )?;
            if known {
                stats.existing += 1;
                continue;
            }
            let received = item
                .received_at
                .as_deref()
                .and_then(|date| OffsetDateTime::parse(date, &Rfc3339).ok());
            if !received.is_some_and(|date| date >= boundary) {
                stats.excluded += 1;
                continue;
            }
            let Some(raw) = reader.mime(&item.id)? else {
                stats.excluded += 1;
                continue;
            };
            let id = importer::import_raw(
                conn,
                &raw,
                std::path::Path::new("graph:inbox"),
                &config.data_dir.join("attachments"),
            )?;
            conn.execute(
                "INSERT OR IGNORE INTO graph_imports(graph_id,message_id_fk) VALUES(?1,?2)",
                params![item.id, id],
            )?;
            stats.imported += 1;
        }
        let done = page.next.is_none();
        let cursor = page
            .next
            .or(page.delta)
            .context("Graph response omitted its continuation link")?;
        validate_graph_link(&cursor)?;
        conn.execute("UPDATE cloud_settings SET graph_cursor=?1,last_sync=?2,last_error=NULL WHERE singleton=1",params![cursor,forward::unix_now()])?;
        if done {
            break;
        }
        link = cursor;
    }
    Ok(stats)
}

pub fn cycle(config: &Config, send_if_enabled: bool) -> Result<SyncStats> {
    let _lock = forward::lock(&config.data_dir)?;
    let conn = db::open(&config.data_dir.join("hiro-maild.sqlite3"))?;
    schema(&conn)?;
    forward::recover_interrupted(&conn)?;
    let state = settings(&conn)?;
    if state.start_at.is_none() {
        return Ok(SyncStats::default());
    }
    let token = cloud_auth::access_token(&conn, config, "microsoft")?;
    let stats = sync_with(&conn, config, &mut HttpGraph::new(token)?)?;
    // Pause may be changed during network work; check it again before every submission.
    if send_if_enabled && settings(&conn)?.enabled {
        let gmail_email =
            cloud_auth::connection_email(&conn, "google")?.context("Gmail not connected")?;
        if gmail_email != config.owner_gmail || forward::config(&conn)?.gmail != config.owner_gmail
        {
            bail!("configured recipient differs from service owner");
        }
        let token = cloud_auth::access_token(&conn, config, "google")?;
        forward::run_with_sender(
            &conn,
            &mut CloudSender {
                conn: &conn,
                inner: gmail::GmailSender::from_access_token(token)?,
            },
            20,
            forward::unix_now(),
        )?;
    }
    Ok(stats)
}

struct CloudSender<'a> {
    conn: &'a Connection,
    inner: gmail::GmailSender,
}
impl forward::Sender for CloudSender<'_> {
    fn should_continue(&self) -> bool {
        settings(self.conn).is_ok_and(|settings| settings.enabled)
    }
    fn send(&mut self, mime: &[u8]) -> forward::SendOutcome {
        forward::Sender::send(&mut self.inner, mime)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mail_parser::{MessageParser, MimeHeaders};
    use std::collections::{HashMap, VecDeque};

    struct FakeGraph {
        pages: VecDeque<Result<Option<DeltaPage>>>,
        mimes: HashMap<String, Vec<u8>>,
        requested: Vec<String>,
        fetched: Vec<String>,
    }
    impl GraphReader for FakeGraph {
        fn page(&mut self, url: &str) -> Result<Option<DeltaPage>> {
            self.requested.push(url.into());
            self.pages
                .pop_front()
                .expect("unexpected Graph page request")
        }
        fn mime(&mut self, id: &str) -> Result<Option<Vec<u8>>> {
            self.fetched.push(id.into());
            Ok(self.mimes.get(id).cloned())
        }
    }
    fn message(id: &str, received: &str) -> GraphMessage {
        GraphMessage {
            id: id.into(),
            received_at: Some(received.into()),
            removed: None,
        }
    }
    fn page(value: Vec<GraphMessage>, cursor: &str, next: bool) -> Result<Option<DeltaPage>> {
        Ok(Some(DeltaPage {
            value,
            next: next.then(|| cursor.into()),
            delta: (!next).then(|| cursor.into()),
        }))
    }
    const CURSOR: &str =
        "https://graph.microsoft.com/v1.0/me/mailFolders/inbox/messages/delta?$deltatoken=fake";
    fn raw(id: &str) -> Vec<u8> {
        format!("From: University <notice@hiroshima-u.ac.jp>\r\nTo: student@hiroshima-u.ac.jp\r\nDate: Tue, 01 Sep 2020 01:00:00 +0000\r\nMessage-ID: <{id}@example.test>\r\nSubject: University notice\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=sample\r\n\r\n--sample\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n本文です\r\n--sample\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=notice.bin\r\nContent-Transfer-Encoding: base64\r\n\r\nAAEC/w==\r\n--sample--\r\n").into_bytes()
    }
    fn setup() -> (tempfile::TempDir, Connection, Config) {
        let dir = tempfile::tempdir().unwrap();
        let config = cloud_auth::test_config(dir.path());
        let conn = db::open(&dir.path().join("hiro-maild.sqlite3")).unwrap();
        schema(&conn).unwrap();
        cloud_auth::save_credential(&conn, &config, "google", &config.owner_gmail, "fake-google")
            .unwrap();
        cloud_auth::save_credential(
            &conn,
            &config,
            "microsoft",
            "student@hiroshima-u.ac.jp",
            "fake-ms",
        )
        .unwrap();
        (dir, conn, config)
    }
    fn reader(pages: Vec<Result<Option<DeltaPage>>>, mimes: Vec<(&str, Vec<u8>)>) -> FakeGraph {
        FakeGraph {
            pages: pages.into(),
            mimes: mimes
                .into_iter()
                .map(|(id, raw)| (id.into(), raw))
                .collect(),
            requested: vec![],
            fetched: vec![],
        }
    }

    #[test]
    fn explicit_start_uses_received_time_preserves_full_mime_and_replays_without_duplicates() {
        let (_dir, conn, config) = setup();
        assert!(settings(&conn).unwrap().start_at.is_none());
        initialize(&conn, &config, Some("2026-09-30T09:00:00+09:00")).unwrap();
        assert!(!settings(&conn).unwrap().enabled);
        assert!(initialize(&conn, &config, None).is_err());
        let original = raw("new");
        let mut graph = reader(
            vec![page(
                vec![
                    message("old", "2020-01-01T00:00:00Z"),
                    message("new", "2026-09-30T00:00:00Z"),
                    GraphMessage {
                        id: "removed".into(),
                        received_at: None,
                        removed: Some(serde_json::json!({"reason":"deleted"})),
                    },
                ],
                CURSOR,
                false,
            )],
            vec![("new", original.clone())],
        );
        let stats = sync_with(&conn, &config, &mut graph).unwrap();
        assert_eq!(stats.imported, 1);
        assert_eq!(stats.excluded, 2);
        assert!(graph.requested[0].contains("receivedDateTime+ge+2026-09-30T00%3A00%3A00Z"));
        assert_eq!(graph.fetched, ["new"]);
        let targets = forward::candidates(&conn, 50, false, 0).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].attachment_names, ["notice.bin"]);
        let saved: Vec<u8> = conn
            .query_row("SELECT mime FROM raw_messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(saved, original);
        let output = forward::build_mime(&conn, targets[0].id, &config.owner_gmail).unwrap();
        let parsed = MessageParser::default().parse(&output).unwrap();
        assert!(
            parsed
                .attachments()
                .any(|part| part.attachment_name() == Some("notice.bin")
                    && part.contents() == [0, 1, 2, 255])
        );
        assert!(
            parsed
                .attachments()
                .any(|part| part.attachment_name() == Some("original.eml")
                    && part.contents() == original)
        );
        let mut changed = reader(
            vec![page(
                vec![
                    message("new", "2026-09-30T00:00:00Z"),
                    message("another-id", "2026-09-30T00:01:00Z"),
                ],
                CURSOR,
                false,
            )],
            vec![("another-id", original)],
        );
        let stats = sync_with(&conn, &config, &mut changed).unwrap();
        assert_eq!(stats.existing, 1);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM deliveries", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        ); // dry-run ingestion never queues or sends
        assert_eq!(changed.requested[0], CURSOR);
    }
    #[test]
    fn from_now_excludes_backlog_and_future_start_is_rejected() {
        let (_dir, conn, config) = setup();
        assert!(initialize(&conn, &config, Some("2099-01-01T00:00:00Z")).is_err());
        initialize(&conn, &config, None).unwrap();
        let mut graph = reader(
            vec![page(
                vec![
                    message("old", "2020-01-01T00:00:00Z"),
                    GraphMessage {
                        id: "undated".into(),
                        received_at: None,
                        removed: None,
                    },
                ],
                CURSOR,
                false,
            )],
            vec![],
        );
        assert_eq!(sync_with(&conn, &config, &mut graph).unwrap().excluded, 2);
        assert!(graph.fetched.is_empty());
    }
    #[test]
    fn page_failure_and_expired_cursor_can_resume_without_reimporting() {
        let (dir, conn, config) = setup();
        initialize(&conn, &config, Some("2020-01-01T00:00:00Z")).unwrap();
        let next =
            "https://graph.microsoft.com/v1.0/me/mailFolders/inbox/messages/delta?$skiptoken=fake";
        let mut graph = reader(
            vec![
                page(vec![message("one", "2026-01-01T00:00:00Z")], next, true),
                Err(anyhow::anyhow!("offline")),
            ],
            vec![("one", raw("one"))],
        );
        assert!(sync_with(&conn, &config, &mut graph).is_err());
        drop(conn);
        let conn = db::open(&dir.path().join("hiro-maild.sqlite3")).unwrap();
        let mut graph = reader(vec![Ok(None)], vec![]);
        sync_with(&conn, &config, &mut graph).unwrap();
        assert_eq!(graph.requested[0], next);
        let mut graph = reader(
            vec![page(
                vec![
                    message("one", "2026-01-01T00:00:00Z"),
                    message("two", "2026-01-01T00:00:00Z"),
                ],
                CURSOR,
                false,
            )],
            vec![("two", raw("two"))],
        );
        let stats = sync_with(&conn, &config, &mut graph).unwrap();
        assert_eq!(stats.existing, 1);
        assert_eq!(stats.imported, 1);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
    #[test]
    fn continuation_url_cannot_disclose_bearer_token_to_another_host() {
        assert!(validate_graph_link(CURSOR).is_ok());
        for url in [
            "https://example.com/v1.0/me/mailFolders/inbox/messages/delta",
            "https://graph.microsoft.com.evil.test/v1.0/me/mailFolders/inbox/messages/delta",
            "http://graph.microsoft.com/v1.0/me/mailFolders/inbox/messages/delta",
            "https://graph.microsoft.com/v1.0/me/sendMail",
            "https://user@graph.microsoft.com/v1.0/me/mailFolders/inbox/messages/delta",
        ] {
            assert!(validate_graph_link(url).is_err(), "{url}");
        }
    }
}
