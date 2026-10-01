//! Durable forwarding queue. A whole-run lock prevents overlapping daemon/CLI sends.
use std::{
    fs::{File, OpenOptions},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::{
    OffsetDateTime,
    format_description::well_known::{Rfc2822, Rfc3339},
};

use crate::{db, gmail};

pub fn schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(r#"
        CREATE TABLE IF NOT EXISTS forward_config (
            singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
            gmail TEXT NOT NULL,
            after_id INTEGER NOT NULL CHECK(after_id >= 0),
            since TEXT,
            initialized_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE TABLE IF NOT EXISTS deliveries (
            message_id_fk INTEGER PRIMARY KEY REFERENCES messages(id),
            status TEXT NOT NULL CHECK(status IN ('pending','sending','retry','blocked','unknown','sent')),
            attempts INTEGER NOT NULL DEFAULT 0,
            next_attempt_at INTEGER NOT NULL DEFAULT 0,
            error_code TEXT,
            gmail_message_id TEXT,
            sent_at TEXT
        );
        CREATE TABLE IF NOT EXISTS forward_attempts (
            id INTEGER PRIMARY KEY,
            message_id_fk INTEGER NOT NULL REFERENCES deliveries(message_id_fk),
            attempt INTEGER NOT NULL,
            started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            finished_at TEXT,
            outcome TEXT NOT NULL,
            error_code TEXT,
            gmail_message_id TEXT,
            UNIQUE(message_id_fk, attempt)
        );
    "#)?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct Config {
    pub gmail: String,
    pub after_id: i64,
    pub since: Option<String>,
}

pub fn config(conn: &Connection) -> Result<Config> {
    conn.query_row(
        "SELECT gmail, after_id, since FROM forward_config WHERE singleton = 1",
        [],
        |row| {
            Ok(Config {
                gmail: row.get(0)?,
                after_id: row.get(1)?,
                since: row.get(2)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| {
        anyhow::anyhow!(
            "forwarding is not configured; run forward-init with an explicit starting position"
        )
    })
}

pub fn lock(data_dir: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(data_dir.join("forward.lock"))?;
    file.try_lock_exclusive()
        .context("another forwarding/init/retry process holds forward.lock")?;
    Ok(file)
}

pub fn validate_gmail(gmail: &str) -> Result<()> {
    let Some((local, domain)) = gmail.split_once('@') else {
        bail!("expected a personal Gmail address");
    };
    if local.is_empty()
        || !local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
        || !["gmail.com", "googlemail.com"].contains(&domain)
    {
        bail!("expected a lowercase personal Gmail address (@gmail.com or @googlemail.com)");
    }
    Ok(())
}

pub fn initialize(
    conn: &Connection,
    gmail: &str,
    after_id: Option<i64>,
    since: Option<&str>,
    from_now: bool,
) -> Result<Config> {
    validate_gmail(gmail)?;
    if usize::from(after_id.is_some()) + usize::from(since.is_some()) + usize::from(from_now) != 1 {
        bail!("choose exactly one of --after-id, --since, --from-now");
    }
    if let Some(since) = since {
        OffsetDateTime::parse(since, &Rfc3339).context("--since must be RFC3339 with timezone")?;
    }
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let maximum: i64 = tx.query_row("SELECT COALESCE(MAX(id), 0) FROM messages", [], |row| {
        row.get(0)
    })?;
    let after_id = if from_now {
        maximum
    } else {
        after_id.unwrap_or(0)
    };
    if after_id < 0 || after_id > maximum {
        bail!("--after-id must be between 0 and current maximum imported id ({maximum})");
    }
    let since = if from_now {
        Some(OffsetDateTime::now_utc().format(&Rfc3339)?)
    } else {
        since.map(str::to_owned)
    };
    tx.execute(
        "INSERT INTO forward_config (singleton, gmail, after_id, since) VALUES (1, ?1, ?2, ?3)",
        params![gmail, after_id, since],
    )
    .context(
        "forwarding configuration already exists; it is immutable to protect delivery history",
    )?;
    tx.commit()?;
    config(conn)
}

const CANDIDATES: &str = r#"
    SELECT m.id, m.subject, m.sender_address, m.sent_at, d.status,
           COALESCE(d.attempts, 0), COALESCE(d.next_attempt_at, 0) AS next_attempt_at, d.error_code,
           d.gmail_message_id, r.mime IS NOT NULL
    FROM messages m CROSS JOIN forward_config c
    LEFT JOIN deliveries d ON d.message_id_fk = m.id
    LEFT JOIN raw_messages r ON r.message_id_fk = m.id
    WHERE m.id > c.after_id
      AND (c.since IS NULL OR julianday(m.sent_at) >= julianday(c.since))
    ORDER BY m.id
"#;

#[derive(Debug, Serialize)]
pub struct Candidate {
    pub id: i64,
    pub subject: String,
    pub sender: Option<String>,
    pub original_date: Option<String>,
    pub status: String,
    pub attempts: i64,
    pub next_attempt_at: i64,
    pub error_code: Option<String>,
    pub gmail_message_id: Option<String>,
    pub mime_saved: bool,
    pub attachment_names: Vec<String>,
    pub forwarding_message_id: String,
    pub eligible_now: bool,
}

pub fn candidates(
    conn: &Connection,
    limit: usize,
    include_finished: bool,
    now: i64,
) -> Result<Vec<Candidate>> {
    let cfg = config(conn)?;
    let sql = format!(
        "SELECT * FROM ({CANDIDATES}) WHERE (?1 OR status IS NULL OR (status IN ('pending', 'retry') AND next_attempt_at <= ?3)) LIMIT ?2"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params![include_finished, limit as i64, now], |row| {
        Ok(Candidate {
            id: row.get(0)?,
            subject: row.get(1)?,
            sender: row.get(2)?,
            original_date: row.get(3)?,
            status: row
                .get::<_, Option<String>>(4)?
                .unwrap_or_else(|| "pending".into()),
            attempts: row.get(5)?,
            next_attempt_at: row.get(6)?,
            error_code: row.get(7)?,
            gmail_message_id: row.get(8)?,
            mime_saved: row.get(9)?,
            attachment_names: Vec::new(),
            forwarding_message_id: String::new(),
            eligible_now: false,
        })
    })?;
    let mut rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    for row in &mut rows {
        let message = db::get_message(conn, row.id)?.context("candidate disappeared")?;
        row.attachment_names = message
            .attachments
            .iter()
            .map(|a| a.filename.clone())
            .collect();
        row.forwarding_message_id = forwarding_message_id(&message.stable_key, &cfg.gmail);
        row.eligible_now = row.mime_saved
            && ["pending", "retry"].contains(&row.status.as_str())
            && row.next_attempt_at <= now;
    }
    Ok(rows)
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn print_status(conn: &Connection, limit: usize, all: bool) -> Result<()> {
    println!("{}", serde_json::to_string(&config(conn)?)?);
    for row in candidates(conn, limit, all, unix_now())? {
        println!("{}", serde_json::to_string(&row)?);
    }
    let (total, selected, missing_date): (i64, i64, i64) = conn.query_row(
        "SELECT COUNT(*), SUM(CASE WHEN m.id > c.after_id AND (c.since IS NULL OR julianday(m.sent_at) >= julianday(c.since)) THEN 1 ELSE 0 END), SUM(CASE WHEN julianday(m.sent_at) IS NULL THEN 1 ELSE 0 END) FROM messages m CROSS JOIN forward_config c",
        [], |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0), row.get::<_, Option<i64>>(2)?.unwrap_or(0)))
    )?;
    println!(
        "range total_imported={total} selected={selected} outside_range={} missing_or_invalid_date={missing_date}",
        total - selected
    );
    Ok(())
}

pub enum SendOutcome {
    Sent(String),
    Retryable(String),
    Blocked(String),
    Unknown(String),
}
pub trait Sender {
    fn send(&mut self, mime: &[u8]) -> SendOutcome;
}

#[derive(Debug, Default, Serialize)]
pub struct RunStats {
    pub sent: usize,
    pub retry: usize,
    pub blocked: usize,
    pub unknown: usize,
}

fn recover_interrupted(conn: &Connection) -> Result<()> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    tx.execute("UPDATE forward_attempts SET outcome = 'unknown', error_code = 'process_interrupted', finished_at = CURRENT_TIMESTAMP WHERE outcome = 'sending'", [])?;
    tx.execute("UPDATE deliveries SET status = 'unknown', error_code = 'process_interrupted' WHERE status = 'sending'", [])?;
    tx.commit()?;
    Ok(())
}

/// Caller must hold forward.lock for the entire call, including the network request.
pub fn run_with_sender(
    conn: &Connection,
    sender: &mut impl Sender,
    limit: usize,
    now: i64,
) -> Result<RunStats> {
    let cfg = config(conn)?;
    recover_interrupted(conn)?;
    conn.execute(r#"INSERT OR IGNORE INTO deliveries (message_id_fk, status)
        SELECT m.id, 'pending' FROM messages m CROSS JOIN forward_config c
        WHERE m.id > c.after_id AND (c.since IS NULL OR julianday(m.sent_at) >= julianday(c.since))"#, [])?;
    // Select due rows before applying the limit, so blocked rows cannot starve new mail.
    let mut stmt = conn.prepare("SELECT message_id_fk FROM deliveries WHERE status IN ('pending', 'retry') AND next_attempt_at <= ?1 ORDER BY message_id_fk LIMIT ?2")?;
    let ids = stmt
        .query_map(params![now, limit as i64], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut stats = RunStats::default();
    for id in ids {
        let mime = match build_mime(conn, id, &cfg.gmail) {
            Ok(mime) => mime,
            Err(_) => {
                conn.execute("UPDATE deliveries SET status = 'blocked', error_code = 'mime_missing_invalid_or_oversize' WHERE message_id_fk = ?1", [id])?;
                stats.blocked += 1;
                continue;
            }
        };
        let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let changed = tx.execute("UPDATE deliveries SET status = 'sending', attempts = attempts + 1, error_code = NULL WHERE message_id_fk = ?1 AND status IN ('pending', 'retry')", [id])?;
        if changed == 0 {
            continue;
        }
        let attempt: i64 = tx.query_row(
            "SELECT attempts FROM deliveries WHERE message_id_fk = ?1",
            [id],
            |row| row.get(0),
        )?;
        tx.execute("INSERT INTO forward_attempts (message_id_fk, attempt, outcome) VALUES (?1, ?2, 'sending')", params![id, attempt])?;
        tx.commit()?; // Durable intent before any mail leaves the process.
        let outcome = sender.send(&mime);
        let (status, error, remote_id) = match outcome {
            SendOutcome::Sent(remote_id) => {
                stats.sent += 1;
                ("sent", None, Some(remote_id))
            }
            SendOutcome::Retryable(error) => {
                stats.retry += 1;
                ("retry", Some(error), None)
            }
            SendOutcome::Blocked(error) => {
                stats.blocked += 1;
                ("blocked", Some(error), None)
            }
            SendOutcome::Unknown(error) => {
                stats.unknown += 1;
                ("unknown", Some(error), None)
            }
        };
        let backoff = (60i64 * 2i64.pow((attempt - 1).clamp(0, 6) as u32)).min(3600);
        let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        tx.execute("UPDATE deliveries SET status = ?2, next_attempt_at = ?3, error_code = ?4, gmail_message_id = ?5, sent_at = CASE WHEN ?2 = 'sent' THEN CURRENT_TIMESTAMP ELSE NULL END WHERE message_id_fk = ?1", params![id, status, now + backoff, error, remote_id])?;
        tx.execute("UPDATE forward_attempts SET outcome = ?3, error_code = ?4, gmail_message_id = ?5, finished_at = CURRENT_TIMESTAMP WHERE message_id_fk = ?1 AND attempt = ?2", params![id, attempt, status, error, remote_id])?;
        tx.commit()?;
        println!("forward id={id} status={status}");
    }
    Ok(stats)
}

pub fn run(conn: &Connection, data_dir: &Path, limit: usize, dry_run: bool) -> Result<()> {
    if dry_run {
        print_status(conn, limit, false)?;
        // Validate MIME too, without credentials, queue changes, or network traffic.
        for row in candidates(conn, limit, false, unix_now())? {
            if ["pending", "retry"].contains(&row.status.as_str()) {
                println!(
                    "mime id={} valid={}",
                    row.id,
                    build_mime(conn, row.id, &config(conn)?.gmail).is_ok()
                );
            }
        }
        return Ok(());
    }
    let _lock = lock(data_dir)?;
    let cfg = config(conn)?;
    let mut sender = gmail::GmailSender::connect(&cfg.gmail)?;
    let stats = run_with_sender(conn, &mut sender, limit, unix_now())?;
    println!("{}", serde_json::to_string(&stats)?);
    Ok(())
}

pub fn retry(conn: &Connection, id: i64, accept_duplicate_risk: bool) -> Result<()> {
    recover_interrupted(conn)?;
    let status: String = conn
        .query_row(
            "SELECT status FROM deliveries WHERE message_id_fk = ?1",
            [id],
            |row| row.get(0),
        )
        .context("no delivery history for this id")?;
    if status == "unknown" && !accept_duplicate_risk {
        bail!(
            "delivery result is unknown; check Gmail first, then use --accept-duplicate-risk to explicitly allow a possible duplicate"
        );
    }
    if !["retry", "blocked", "unknown"].contains(&status.as_str()) {
        bail!(
            "only retry, blocked, or unknown deliveries can be retried; sent history is never reset"
        );
    }
    conn.execute("UPDATE deliveries SET status = 'pending', next_attempt_at = 0, error_code = NULL WHERE message_id_fk = ?1", [id])?;
    Ok(())
}

fn forwarding_message_id(stable_key: &str, gmail: &str) -> String {
    let digest = Sha256::digest(format!("{gmail}\n{stable_key}"));
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("<hiro-maild.{hex}@hiro-maild.local>")
}

fn encoded_text(value: &str) -> String {
    // RFC2047 words stay below 75 bytes, even for long/untrusted subjects.
    let clean = value.replace(['\r', '\n'], " ");
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    for ch in clean.chars() {
        if chunk.len() + ch.len_utf8() > 42 {
            chunks.push(format!("=?UTF-8?B?{}?=", STANDARD.encode(&chunk)));
            chunk.clear();
        }
        chunk.push(ch);
    }
    if !chunk.is_empty() {
        chunks.push(format!("=?UTF-8?B?{}?=", STANDARD.encode(chunk)));
    }
    chunks.join("\r\n ")
}

fn base64_lines(bytes: &[u8]) -> String {
    let encoded = STANDARD.encode(bytes);
    encoded
        .as_bytes()
        .chunks(76)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join("\r\n")
}

pub fn build_mime(conn: &Connection, id: i64, gmail: &str) -> Result<Vec<u8>> {
    validate_gmail(gmail)?;
    let message = db::get_message(conn, id)?.context("message missing")?;
    let raw: Vec<u8> = conn
        .query_row(
            "SELECT mime FROM raw_messages WHERE message_id_fk = ?1",
            [id],
            |row| row.get(0),
        )
        .context("original MIME missing; resync Thunderbird before forwarding")?;
    // Fixed conservative cap, including MIME/base64 overhead and the original .eml copy.
    if raw.len() > 18 * 1024 * 1024 {
        bail!("original MIME exceeds forwarding cap");
    }
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| (p, p + 4))
        .or_else(|| {
            raw.windows(2)
                .position(|w| w == b"\n\n")
                .map(|p| (p, p + 2))
        })
        .context("MIME header separator missing")?;
    let headers = std::str::from_utf8(&raw[..split.0]).context("non-UTF8 MIME headers")?;
    let mut content_headers = String::new();
    let mut retain = false;
    let mut has_type = false;
    for line in headers.lines() {
        if !line.starts_with([' ', '\t']) {
            let name = line
                .split_once(':')
                .map(|(name, _)| name.to_ascii_lowercase())
                .unwrap_or_default();
            retain = name.starts_with("content-");
            has_type |= name == "content-type";
        }
        if retain {
            content_headers.push_str(line.trim_end_matches('\r'));
            content_headers.push_str("\r\n");
        }
    }
    if !has_type {
        content_headers.push_str("Content-Type: text/plain; charset=us-ascii\r\n");
    }
    let mut boundary = format!("hiro-maild-{}", gmail::random_token()?);
    while raw
        .windows(boundary.len())
        .any(|w| w == boundary.as_bytes())
    {
        boundary = format!("hiro-maild-{}", gmail::random_token()?);
    }
    let metadata = format!(
        "Forwarded from Thunderbird local mail.\nOriginal From: {} <{}>\nOriginal Date: {}\nOriginal Subject: {}\nOriginal Message-ID: {}\nThe complete local original is attached as original.eml.\n\n",
        message.sender_name.as_deref().unwrap_or_default(),
        message.sender_address.as_deref().unwrap_or("unknown"),
        message.sent_at.as_deref().unwrap_or("unknown"),
        message.subject,
        message.message_id.as_deref().unwrap_or("none")
    );
    let mut output = format!("From: {gmail}\r\nTo: {gmail}\r\nDate: {}\r\nMessage-ID: {}\r\nSubject: {}\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n--{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n--{boundary}\r\n{content_headers}\r\n",
        OffsetDateTime::now_utc().format(&Rfc2822)?, forwarding_message_id(&message.stable_key, gmail),
        encoded_text(&format!("[広大メール] {}", message.subject)), base64_lines(metadata.as_bytes())).into_bytes();
    output.extend_from_slice(&raw[split.1..]);
    output.extend_from_slice(format!("\r\n--{boundary}\r\nContent-Type: application/octet-stream; name=\"original.eml\"\r\nContent-Disposition: attachment; filename=\"original.eml\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n--{boundary}--\r\n", base64_lines(&raw)).as_bytes());
    if output.len() > 34 * 1024 * 1024 {
        bail!("composed MIME exceeds conservative Gmail cap");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::importer;
    use mail_parser::MimeHeaders;
    use mail_parser::{MessageParser, mailbox::mbox::MessageIterator};
    use std::collections::VecDeque;

    struct FakeSender {
        outcomes: VecDeque<SendOutcome>,
        submitted: Vec<Vec<u8>>,
    }
    impl FakeSender {
        fn new(outcomes: Vec<SendOutcome>) -> Self {
            Self {
                outcomes: outcomes.into(),
                submitted: Vec::new(),
            }
        }
    }
    impl Sender for FakeSender {
        fn send(&mut self, mime: &[u8]) -> SendOutcome {
            self.submitted.push(mime.to_vec());
            self.outcomes
                .pop_front()
                .expect("unexpected send, or duplicate")
        }
    }
    fn setup() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        std::fs::create_dir(&store).unwrap();
        std::fs::copy("tests/fixtures/sample.mbox", store.join("Inbox")).unwrap();
        std::fs::write(store.join("Inbox.msf"), b"").unwrap();
        let conn = db::open(&dir.path().join("db.sqlite3")).unwrap();
        importer::sync_store(&conn, &store, &dir.path().join("attachments")).unwrap();
        (dir, conn)
    }
    fn init(conn: &Connection, id: i64) {
        initialize(conn, "student@gmail.com", Some(id), None, false).unwrap();
    }

    #[test]
    fn original_attachment_body_and_eml_bytes_are_preserved() {
        let (_dir, conn) = setup();
        let mime = build_mime(&conn, 2, "student@gmail.com").unwrap();
        let message = MessageParser::default().parse(&mime).unwrap();
        assert!(message.body_text(0).unwrap().contains("Student Office"));
        assert!(
            message
                .body_text(1)
                .unwrap()
                .contains("添付を確認してください")
        );
        let attachments: Vec<_> = message.attachments().collect();
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].attachment_name(), Some("notice.txt"));
        assert_eq!(
            attachments[0].contents(),
            "提出期限：2026年9月20日".as_bytes()
        );
        assert_eq!(attachments[1].attachment_name(), Some("original.eml"));
        let fixture = std::fs::File::open("tests/fixtures/sample.mbox").unwrap();
        let original = MessageIterator::new(std::io::BufReader::new(fixture))
            .nth(1)
            .unwrap()
            .unwrap();
        assert_eq!(attachments[1].contents(), original.contents());
        assert_eq!(
            message.from().unwrap().first().unwrap().address(),
            Some("student@gmail.com")
        );
        assert_eq!(
            message.to().unwrap().first().unwrap().address(),
            Some("student@gmail.com")
        );
    }

    #[test]
    fn html_inline_cid_binary_and_attached_message_are_preserved() {
        let (dir, conn) = setup();
        let original = b"From: sender@example.com\r\nTo: student@hiroshima-u.ac.jp\r\nDate: Mon, 7 Sep 2026 14:00:00 +0900\r\nMessage-ID: <rich@example.com>\r\nSubject: rich\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=outer\r\n\r\n--outer\r\nContent-Type: multipart/related; boundary=inner\r\n\r\n--inner\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p><img src=\"cid:image\">html body</p>\r\n--inner\r\nContent-Type: image/png\r\nContent-ID: <image>\r\nContent-Disposition: inline; filename=pic.png\r\nContent-Transfer-Encoding: base64\r\n\r\nAAECA/8=\r\n--inner--\r\n--outer\r\nContent-Type: message/rfc822\r\n\r\nFrom: nested@example.com\r\nSubject: attached\r\n\r\nNested message\r\n--outer--\r\n";
        let mut mbox = b"From sender@example.com Mon Sep 7 14:00:00 2026\n".to_vec();
        mbox.extend_from_slice(original);
        std::fs::write(dir.path().join("store/Rich"), &mbox).unwrap();
        std::fs::write(dir.path().join("store/Rich.msf"), b"").unwrap();
        importer::sync_store(
            &conn,
            &dir.path().join("store"),
            &dir.path().join("attachments"),
        )
        .unwrap();
        let mime = build_mime(&conn, 3, "student@gmail.com").unwrap();
        let parsed = MessageParser::default().parse(&mime).unwrap();
        assert!(
            (0..parsed.html_body_count())
                .any(|i| parsed.body_html(i).unwrap().contains("cid:image"))
        );
        assert!(
            parsed
                .parts
                .iter()
                .any(|part| part.contents() == b"\x00\x01\x02\x03\xff")
        );
        assert!(parsed.parts.iter().any(|part| part.is_message()));
        let saved: Vec<u8> = conn
            .query_row(
                "SELECT mime FROM raw_messages WHERE message_id_fk=3",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            parsed
                .attachments()
                .find(|a| a.attachment_name() == Some("original.eml"))
                .unwrap()
                .contents(),
            saved
        );
    }

    #[test]
    fn explicit_range_is_required_and_since_compares_timezone_instants() {
        let (_dir, conn) = setup();
        assert!(config(&conn).is_err());
        assert!(initialize(&conn, "student@gmail.com", None, None, false).is_err());
        assert!(initialize(&conn, "student@gmail.com", None, Some("invalid"), false).is_err());
        initialize(
            &conn,
            "student@gmail.com",
            None,
            Some("2026-09-07T03:30:00Z"),
            false,
        )
        .unwrap();
        assert_eq!(
            candidates(&conn, 20, false, 0)
                .unwrap()
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            [2]
        );
        assert!(initialize(&conn, "student@gmail.com", Some(0), None, false).is_err());
    }

    #[test]
    fn from_now_excludes_existing_and_late_backlog_unknown_dates() {
        let (_dir, conn) = setup();
        let cfg = initialize(&conn, "student@gmail.com", None, None, true).unwrap();
        assert_eq!(cfg.after_id, 2);
        assert!(candidates(&conn, 20, false, 0).unwrap().is_empty());
        for (key, date) in [
            ("late-old", Some("2020-01-01T00:00:00Z")),
            ("no-date", None),
            ("new", Some("2099-01-01T00:00:00Z")),
        ] {
            conn.execute("INSERT INTO messages (stable_key,subject,body_text,raw_sha256,source_path,sent_at) VALUES (?1,'test','','hash','test',?2)", params![key,date]).unwrap();
        }
        assert_eq!(
            candidates(&conn, 20, false, 0)
                .unwrap()
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            [5]
        );
    }

    #[test]
    fn dry_run_does_not_create_delivery_history_or_need_credentials() {
        let (dir, conn) = setup();
        init(&conn, 0);
        run(&conn, dir.path(), 20, true).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM deliveries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM forward_attempts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn retry_backoff_restart_and_resync_never_repeat_success() {
        let (dir, conn) = setup();
        init(&conn, 1);
        let mut sender = FakeSender::new(vec![
            SendOutcome::Retryable("connection_failed".into()),
            SendOutcome::Sent("gmail-id".into()),
        ]);
        let _lock = lock(dir.path()).unwrap();
        assert_eq!(
            run_with_sender(&conn, &mut sender, 20, 1000).unwrap().retry,
            1
        );
        assert_eq!(
            run_with_sender(&conn, &mut sender, 20, 1059).unwrap().sent,
            0
        );
        drop(conn);
        let conn = db::open(&dir.path().join("db.sqlite3")).unwrap();
        assert_eq!(
            run_with_sender(&conn, &mut sender, 20, 1060).unwrap().sent,
            1
        );
        importer::sync_store(
            &conn,
            &dir.path().join("store"),
            &dir.path().join("attachments"),
        )
        .unwrap();
        assert_eq!(
            run_with_sender(&conn, &mut sender, 20, 5000).unwrap().sent,
            0
        );
        assert_eq!(sender.submitted.len(), 2);
        let id = |mime: &[u8]| {
            MessageParser::default()
                .parse(mime)
                .unwrap()
                .message_id()
                .unwrap()
                .to_owned()
        };
        assert_eq!(id(&sender.submitted[0]), id(&sender.submitted[1]));
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM forward_attempts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
        assert!(retry(&conn, 2, true).is_err());
    }

    #[test]
    fn unknown_and_interrupted_sends_require_explicit_duplicate_risk_acceptance() {
        let (dir, conn) = setup();
        init(&conn, 1);
        let _lock = lock(dir.path()).unwrap();
        let mut sender = FakeSender::new(vec![
            SendOutcome::Unknown("timeout".into()),
            SendOutcome::Sent("remote".into()),
        ]);
        assert_eq!(
            run_with_sender(&conn, &mut sender, 20, 0).unwrap().unknown,
            1
        );
        run_with_sender(&conn, &mut sender, 20, 9999).unwrap();
        assert_eq!(sender.submitted.len(), 1);
        assert!(retry(&conn, 2, false).is_err());
        retry(&conn, 2, true).unwrap();
        // Simulate a crash after durable intent and before recording the send result.
        conn.execute(
            "UPDATE deliveries SET status='sending', attempts=attempts+1 WHERE message_id_fk=2",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO forward_attempts(message_id_fk,attempt,outcome) VALUES(2,2,'sending')",
            [],
        )
        .unwrap();
        run_with_sender(&conn, &mut sender, 20, 10000).unwrap();
        assert_eq!(sender.submitted.len(), 1);
        assert!(retry(&conn, 2, false).is_err());
        retry(&conn, 2, true).unwrap();
        assert_eq!(
            run_with_sender(&conn, &mut sender, 20, 10001).unwrap().sent,
            1
        );
    }

    #[test]
    fn missing_legacy_mime_is_blocked_then_backfilled_on_read_only_resync() {
        let (dir, conn) = setup();
        init(&conn, 0);
        conn.execute("DELETE FROM raw_messages WHERE message_id_fk=1", [])
            .unwrap();
        let before = std::fs::read(dir.path().join("store/Inbox")).unwrap();
        let mut sender = FakeSender::new(vec![
            SendOutcome::Sent("second".into()),
            SendOutcome::Sent("first".into()),
        ]);
        let _lock = lock(dir.path()).unwrap();
        assert_eq!(
            run_with_sender(&conn, &mut sender, 1, 0).unwrap().blocked,
            1
        );
        assert_eq!(run_with_sender(&conn, &mut sender, 1, 0).unwrap().sent, 1);
        importer::sync_store(
            &conn,
            &dir.path().join("store"),
            &dir.path().join("attachments"),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("store/Inbox")).unwrap(),
            before
        );
        retry(&conn, 1, false).unwrap();
        assert_eq!(run_with_sender(&conn, &mut sender, 1, 0).unwrap().sent, 1);
    }

    #[test]
    fn concurrent_forwarders_cannot_claim_same_queue() {
        let dir = tempfile::tempdir().unwrap();
        let first = lock(dir.path()).unwrap();
        assert!(lock(dir.path()).is_err());
        drop(first);
        assert!(lock(dir.path()).is_ok());
    }

    #[test]
    fn dry_run_limit_matches_due_queue_even_when_earlier_rows_are_held() {
        let (_dir, conn) = setup();
        init(&conn, 0);
        conn.execute(
            "INSERT INTO deliveries (message_id_fk,status) VALUES (1,'unknown')",
            [],
        )
        .unwrap();
        let rows = candidates(&conn, 1, false, 1000).unwrap();
        assert_eq!(rows[0].id, 2);
        conn.execute(
            "INSERT INTO deliveries (message_id_fk,status,next_attempt_at) VALUES (2,'retry',1060)",
            [],
        )
        .unwrap();
        assert!(candidates(&conn, 1, false, 1059).unwrap().is_empty());
        assert_eq!(candidates(&conn, 1, false, 1060).unwrap()[0].id, 2);
        assert_eq!(candidates(&conn, 20, true, 1059).unwrap().len(), 2);
    }

    #[test]
    fn no_message_id_local_flag_changes_do_not_cause_another_send() {
        let (dir, conn) = setup();
        init(&conn, 2);
        let mbox = "From sender@example.com Mon Sep 7 14:00:00 2026\nFrom: sender@example.com\nDate: Mon, 7 Sep 2026 14:00:00 +0900\nSubject: no id\nX-Mozilla-Status: 0000\nX-Mozilla-Status2: 00000000\nX-Mozilla-Keys: tag1\nContent-Type: text/plain\n\nBody without Message-ID\n";
        let path = dir.path().join("store/NoId");
        std::fs::write(&path, mbox).unwrap();
        std::fs::write(dir.path().join("store/NoId.msf"), b"").unwrap();
        importer::sync_store(
            &conn,
            &dir.path().join("store"),
            &dir.path().join("attachments"),
        )
        .unwrap();
        let _lock = lock(dir.path()).unwrap();
        let mut sender = FakeSender::new(vec![SendOutcome::Sent("sent-once".into())]);
        assert_eq!(run_with_sender(&conn, &mut sender, 20, 0).unwrap().sent, 1);
        std::fs::write(
            &path,
            mbox.replace("Status: 0000", "Status: 0001")
                .replace("tag1", "tag2"),
        )
        .unwrap();
        let sync = importer::sync_store(
            &conn,
            &dir.path().join("store"),
            &dir.path().join("attachments"),
        )
        .unwrap();
        assert_eq!(sync.imported_messages, 0);
        assert_eq!(
            run_with_sender(&conn, &mut sender, 20, 10000).unwrap().sent,
            0
        );
        assert_eq!(sender.submitted.len(), 1);
    }

    #[test]
    fn header_injection_is_encoded_and_destination_is_validated() {
        assert!(validate_gmail("student@gmail.com\r\nBcc: other@example.com").is_err());
        let value = encoded_text("subject\r\nBcc: other@example.com");
        assert!(!value.contains("\r\nBcc:"));
        for line in encoded_text(&"長い件名".repeat(100)).lines() {
            assert!(line.len() <= 75);
        }
    }

    #[test]
    fn forwarding_uses_complete_mime_instead_of_truncated_index_text() {
        let (_dir, conn) = setup();
        let body = "本文".repeat(110_000);
        let raw = format!(
            "From: sender@example.com\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}"
        );
        conn.execute(
            "UPDATE raw_messages SET mime=?1 WHERE message_id_fk=1",
            [raw.as_bytes()],
        )
        .unwrap();
        let mime = build_mime(&conn, 1, "student@gmail.com").unwrap();
        let parsed = MessageParser::default().parse(&mime).unwrap();
        assert_eq!(parsed.body_text(1).unwrap().trim_end(), body);
    }

    #[test]
    fn oversize_original_is_held_without_submitting_truncated_mail() {
        let (dir, conn) = setup();
        init(&conn, 1);
        let raw = vec![b'a'; 18 * 1024 * 1024 + 1];
        conn.execute(
            "UPDATE raw_messages SET mime=?1 WHERE message_id_fk=2",
            [raw],
        )
        .unwrap();
        let _lock = lock(dir.path()).unwrap();
        let mut sender = FakeSender::new(vec![]);
        assert_eq!(
            run_with_sender(&conn, &mut sender, 20, 0).unwrap().blocked,
            1
        );
        assert!(sender.submitted.is_empty());
    }
}
