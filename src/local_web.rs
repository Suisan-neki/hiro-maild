//! Loopback-only UI over the existing read-only Thunderbird importer and durable queue.
use crate::{db, discovery, forward, gmail, importer};
use anyhow::{Result, bail};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct Activity {
    busy: bool,
    message: String,
    auth_url: Option<String>,
    gmail: Option<String>,
    previewed: bool,
    // Validate on explicit preview, not on every two-second status poll.
    mime_valid: BTreeMap<i64, bool>,
}
#[derive(Clone)]
pub struct App {
    data: PathBuf,
    host: String,
    origin: String,
    csrf: String,
    activity: Arc<Mutex<Activity>>,
}
impl App {
    fn conn(&self) -> Result<rusqlite::Connection> {
        let conn = db::open(&self.data.join("hiro-maild.sqlite3"))?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS local_ui_config (singleton INTEGER PRIMARY KEY CHECK(singleton=1), store TEXT NOT NULL)")?;
        Ok(conn)
    }
    fn store(&self, conn: &rusqlite::Connection) -> Result<PathBuf> {
        Ok(PathBuf::from(conn.query_row(
            "SELECT store FROM local_ui_config WHERE singleton=1",
            [],
            |r| r.get::<_, String>(0),
        )?))
    }
}

pub fn router(app: App) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../local-web/index.html")) }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../local-web/app.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../local-web/style.css"),
                )
            }),
        )
        .route("/api/status", get(status))
        .route("/api/action", post(action))
        .layer(DefaultBodyLimit::max(32 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}
async fn guard(State(app): State<App>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    // Exact Host protects even GETs against DNS rebinding. No CORS or external bind.
    let host_ok =
        headers.get(header::HOST).and_then(|v| v.to_str().ok()) == Some(app.host.as_str());
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    let cross_site =
        headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) == Some("cross-site");
    let post_ok = request.method() != axum::http::Method::POST
        || (origin == Some(app.origin.as_str())
            && headers.get("x-csrf-token").and_then(|v| v.to_str().ok())
                == Some(app.csrf.as_str()));
    if !host_ok || cross_site || !post_ok || origin.is_some_and(|v| v != app.origin) {
        return (
            StatusCode::FORBIDDEN,
            "このMacのhiro-maild画面から操作してください。",
        )
            .into_response();
    }
    let mut response = next.run(request).await;
    for (name, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        (
            "content-security-policy",
            "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
        ),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    response
}
async fn status(State(app): State<App>) -> Result<Json<Value>, (StatusCode, &'static str)> {
    let snapshot = tokio::task::spawn_blocking(move || -> Result<Value> {
        let conn = app.conn()?;
        let config = forward::config(&conn).ok();
        let targets = if config.is_some() { forward::candidates(&conn, 50, false, forward::unix_now())? } else {vec![]};
        let validity = app.activity.lock().unwrap().mime_valid.clone();
        let targets = targets.into_iter().map(|row| {
            let valid = validity.get(&row.id);
            json!({"mail":row,"mime_valid":valid})
        }).collect::<Vec<_>>();
        let mut query = conn.prepare("SELECT d.message_id_fk,m.subject,d.status,d.error_code,m.stable_key FROM deliveries d JOIN messages m ON m.id=d.message_id_fk ORDER BY d.message_id_fk DESC LIMIT 50")?;
        let history = query.query_map([], |r| Ok(json!({
            "id":r.get::<_,i64>(0)?,"subject":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,
            "error":r.get::<_,Option<String>>(3)?,
            "forward_id":config.as_ref().map(|c|r.get::<_,String>(4).map(|key|forward::forwarding_message_id(&key,&c.gmail))).transpose()?
        })))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let activity = app.activity.lock().unwrap();
        Ok(json!({"csrf":app.csrf,"store":app.store(&conn).ok(),"stores":discovery::find_hiroshima_stores(),
            "start":config,"targets":targets,"history":history,"busy":activity.busy,
            "message":activity.message,"auth_url":activity.auth_url,"gmail":activity.gmail,
            "previewed":activity.previewed}))
    }).await.map_err(|_|(StatusCode::INTERNAL_SERVER_ERROR,"画面の状態を取得できません。"))?
        .map_err(|_|(StatusCode::INTERNAL_SERVER_ERROR,"保存先とデータベースを確認してください。"))?;
    Ok(Json(snapshot))
}

#[derive(Deserialize)]
struct Action {
    op: String,
    store: Option<PathBuf>,
    gmail: Option<String>,
    // A Desktop app JSON selected by the user, transported only to loopback.
    client_json: Option<Value>,
    mode: Option<String>,
    since: Option<String>,
    id: Option<i64>,
    #[serde(default)]
    accept_duplicate_risk: bool,
}
async fn action(State(app): State<App>, Json(input): Json<Action>) -> Response {
    if ![
        "configure",
        "auth",
        "initialize",
        "preview",
        "send",
        "retry",
    ]
    .contains(&input.op.as_str())
    {
        return (StatusCode::BAD_REQUEST, "操作が不正です。").into_response();
    }
    {
        let mut activity = app.activity.lock().unwrap();
        if activity.busy {
            return (StatusCode::CONFLICT, "処理中です。終了をお待ちください。").into_response();
        }
        activity.busy = true;
        activity.message = "処理中です。この画面を開いたままお待ちください。".into();
        activity.auth_url = None;
    }
    // Never log request bodies, JSON credentials, OAuth URLs, or remote error bodies.
    tokio::spawn(async move {
        let worker = app.clone();
        let result = tokio::task::spawn_blocking(move || process(&worker, input)).await;
        let mut activity = app.activity.lock().unwrap();
        activity.busy = false;
        activity.auth_url = None;
        activity.message = match result {
            Ok(Ok(message)) => message,
            Ok(Err(error)) => error,
            Err(_) => "処理が中断されました。再起動後に履歴を確認してください。".into(),
        };
    });
    Json(json!({"ok":true})).into_response()
}
fn process(app: &App, input: Action) -> Result<String, String> {
    let op = input.op.clone();
    let perform = || -> Result<String> {
        let conn = app.conn()?;
        let _lock = forward::lock(&app.data)?;
        match input.op.as_str() {
            "configure" => {
                let store = input
                    .store
                    .ok_or_else(|| anyhow::anyhow!("missing store"))?
                    .canonicalize()?;
                if !store.is_dir() {
                    bail!("not a directory");
                }
                if forward::config(&conn).is_ok() {
                    bail!("start already configured");
                }
                conn.execute("INSERT INTO local_ui_config(singleton,store) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET store=excluded.store",[store.to_string_lossy().as_ref()])?;
                Ok("Thunderbirdの保存先を設定しました。転送開始位置を設定してください。".into())
            }
            "auth" => {
                let address = input
                    .gmail
                    .ok_or_else(|| anyhow::anyhow!("missing Gmail"))?
                    .to_lowercase();
                forward::validate_gmail(&address)?;
                if forward::config(&conn).is_ok_and(|c| c.gmail != address) {
                    bail!("account mismatch");
                }
                app.activity.lock().unwrap().gmail = None;
                if let Some(client) = input.client_json {
                    gmail::authenticate_bytes(&address, &serde_json::to_vec(&client)?, |url| {
                        let mut activity = app.activity.lock().unwrap();
                        activity.auth_url = Some(url);
                        activity.message = "「Googleで認証する」を開き、同意を完了してください。認証だけでは送信しません。".into();
                    })?;
                } else {
                    // Reuse only this account's existing verified Keychain credentials.
                    gmail::GmailSender::connect(&address)?;
                }
                app.activity.lock().unwrap().gmail = Some(address);
                Ok("Gmailに接続しました。認証だけでは送信していません。".into())
            }
            "initialize" => {
                import_ready(app, &conn)?; // Baseline import precedes the immutable from-now boundary.
                let address = input
                    .gmail
                    .ok_or_else(|| anyhow::anyhow!("missing Gmail"))?
                    .to_lowercase();
                let now = input.mode.as_deref() == Some("now");
                let since = if input.mode.as_deref() == Some("since") {
                    input.since.as_deref()
                } else {
                    None
                };
                forward::initialize(&conn, &address, None, since, now)?;
                app.activity.lock().unwrap().previewed = false;
                Ok(
                    "開始位置を保存しました。次に「対象を確認（送信なし）」を押してください。"
                        .into(),
                )
            }
            "preview" => {
                let config = forward::config(&conn)?;
                let count = import_ready(app, &conn)?;
                let validity = forward::candidates(&conn, 50, false, forward::unix_now())?
                    .into_iter()
                    .map(|row| {
                        (
                            row.id,
                            forward::build_mime(&conn, row.id, &config.gmail).is_ok(),
                        )
                    })
                    .collect();
                let mut activity = app.activity.lock().unwrap();
                activity.mime_valid = validity;
                activity.previewed = true;
                Ok(format!(
                    "ローカル取り込み {count} 件。対象を確認しました。メールは送信していません。"
                ))
            }
            "send" => {
                let config = forward::config(&conn)?;
                {
                    let activity = app.activity.lock().unwrap();
                    if !activity.previewed || activity.gmail.as_deref() != Some(&config.gmail) {
                        bail!("preview or Gmail connection missing");
                    }
                }
                import_ready(app, &conn)?;
                let mut sender = gmail::GmailSender::connect(&config.gmail)?;
                let stats = forward::run_with_sender(&conn, &mut sender, 20, forward::unix_now())?;
                Ok(format!(
                    "転送 {} 件・再試行待ち {} 件・保留 {} 件・結果不明 {} 件。",
                    stats.sent, stats.retry, stats.blocked, stats.unknown
                ))
            }
            "retry" => {
                forward::retry(
                    &conn,
                    input.id.ok_or_else(|| anyhow::anyhow!("missing id"))?,
                    input.accept_duplicate_risk,
                )?;
                Ok("再試行を予約しました。まだ送信していません。".into())
            }
            _ => unreachable!(),
        }
    };
    perform().map_err(|_|match op.as_str() {
        "configure" => "保存先を設定できません。フォルダーを確認してください。開始位置設定後は変更できません。",
        "auth" => "Gmailに接続できません。初回はDesktop appのJSONを選択し、Googleの送信権限に同意してください。",
        "initialize" => "開始位置を設定できません。Gmailの宛先・日時・Thunderbirdの完全同期を確認してください。開始位置は一度だけ設定できます。",
        "preview" => "取り込めません。保存先にmboxと対応する.msfがあることを確認し、Thunderbirdの同期完了後に再試行してください。",
        "send" => "転送を完了できません。送信なし確認・Gmail接続・Thunderbirdの同期状態を確認してください。履歴も確認してから再試行してください。",
        _ => "再試行できません。結果不明のメールはGmailで確認してから重複リスクへの同意が必要です。",
    }.into())
}
fn import_ready(app: &App, conn: &rusqlite::Connection) -> Result<usize> {
    let stats = importer::sync_store(conn, &app.store(conn)?, &app.data.join("attachments"))?;
    // Empty/incomplete stores must not silently initialize or send a partial snapshot.
    if stats.scanned_files == 0 || stats.parse_errors != 0 {
        bail!("no stable complete mbox snapshot");
    }
    Ok(stats.imported_messages)
}
pub async fn run(data: PathBuf, bind: String, store: Option<PathBuf>) -> Result<()> {
    let address: SocketAddr = bind.parse()?;
    if !address.ip().is_loopback() {
        bail!("local-web only accepts a loopback address");
    }
    let listener = tokio::net::TcpListener::bind(address).await?;
    let address = listener.local_addr()?;
    let app = App {
        data,
        host: address.to_string(),
        origin: format!("http://{address}"),
        csrf: gmail::random_token()?,
        activity: Arc::default(),
    };
    let conn = app.conn()?;
    let _lock = forward::lock(&app.data)?;
    forward::recover_interrupted(&conn)?;
    if let (Some(requested), Ok(saved)) = (store.as_ref(), app.store(&conn)) {
        if requested.canonicalize()? != saved.canonicalize()? {
            bail!(
                "local-web store differs from the saved store; keep the original data directory and store"
            );
        }
    }
    if app.store(&conn).is_err() {
        if let Some(path) = store.or_else(|| discovery::select_hiroshima_store().ok()) {
            let path = path.canonicalize()?;
            conn.execute(
                "INSERT INTO local_ui_config(singleton,store) VALUES(1,?1)",
                [path.to_string_lossy().as_ref()],
            )?;
        }
    }
    drop(_lock);
    drop(conn);
    println!(
        "Open {} on this Mac. No automatic forwarding; Thunderbird must synchronize first.",
        app.origin
    );
    axum::serve(listener, router(app)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    fn setup() -> (tempfile::TempDir, App, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let store = dir.path().join("store");
        std::fs::create_dir(&data).unwrap();
        std::fs::create_dir(&store).unwrap();
        std::fs::copy("tests/fixtures/sample.mbox", store.join("Inbox")).unwrap();
        std::fs::write(store.join("Inbox.msf"), b"").unwrap();
        let app = App {
            data,
            host: "127.0.0.1:8082".into(),
            origin: "http://127.0.0.1:8082".into(),
            csrf: "test-csrf".into(),
            activity: Arc::default(),
        };
        process(&app, input(json!({"op":"configure","store":store}))).unwrap();
        (dir, app, store)
    }
    fn input(value: Value) -> Action {
        serde_json::from_value(value).unwrap()
    }
    fn initialize(app: &App, mode: &str, since: Option<&str>) {
        process(
            app,
            input(json!({"op":"initialize","gmail":"student@gmail.com","mode":mode,"since":since})),
        )
        .unwrap();
    }
    #[test]
    fn initial_baseline_and_late_resync_exclude_old_mail_without_sending() {
        let (_dir, app, store) = setup();
        initialize(&app, "now", None);
        let conn = app.conn().unwrap();
        assert_eq!(forward::config(&conn).unwrap().after_id, 2);
        process(&app, input(json!({"op":"preview"}))).unwrap();
        assert!(
            forward::candidates(&conn, 50, false, forward::unix_now())
                .unwrap()
                .is_empty()
        );
        // A previously unseen old message arriving in a later local sync is also excluded.
        let old = std::fs::read_to_string(store.join("Inbox"))
            .unwrap()
            .replace("test-1@example.com", "late-old@example.com")
            .replace("test-2@example.com", "late-old-2@example.com");
        std::fs::write(store.join("Inbox"), old).unwrap();
        process(&app, input(json!({"op":"preview"}))).unwrap();
        assert!(
            forward::candidates(&conn, 50, false, forward::unix_now())
                .unwrap()
                .is_empty()
        );
        let deliveries: i64 = conn
            .query_row("SELECT COUNT(*) FROM deliveries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(deliveries, 0);
    }
    #[test]
    fn explicit_range_preview_preserves_attachment_and_local_store() {
        let (_dir, app, store) = setup();
        let before = std::fs::read(store.join("Inbox")).unwrap();
        initialize(&app, "since", Some("2026-09-07T12:30:00+09:00"));
        process(&app, input(json!({"op":"preview"}))).unwrap();
        let conn = app.conn().unwrap();
        let targets = forward::candidates(&conn, 50, false, forward::unix_now()).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].attachment_names, vec!["notice.txt"]);
        assert!(forward::build_mime(&conn, targets[0].id, "student@gmail.com").is_ok());
        assert_eq!(std::fs::read(store.join("Inbox")).unwrap(), before);
        assert!(process(&app, input(json!({"op":"send"}))).is_err()); // No OAuth/network permitted.
        assert!(process(&app, input(json!({"op":"configure","store":store}))).is_err());
        assert!(
            process(
                &app,
                input(json!({"op":"initialize","gmail":"other@gmail.com","mode":"now"}))
            )
            .is_err()
        );
    }
    #[test]
    fn failed_send_retry_and_restart_do_not_duplicate_deliveries() {
        struct Fake {
            calls: usize,
        }
        impl forward::Sender for Fake {
            fn send(&mut self, _: &[u8]) -> forward::SendOutcome {
                self.calls += 1;
                if self.calls == 1 {
                    forward::SendOutcome::Retryable("connection_failed".into())
                } else {
                    forward::SendOutcome::Sent("fake-message".into())
                }
            }
        }
        let (_dir, app, _store) = setup();
        initialize(&app, "since", Some("2026-09-07T12:30:00+09:00"));
        let conn = app.conn().unwrap();
        let mut sender = Fake { calls: 0 };
        let now = forward::unix_now();
        let _lock = forward::lock(&app.data).unwrap();
        assert_eq!(
            forward::run_with_sender(&conn, &mut sender, 20, now)
                .unwrap()
                .retry,
            1
        );
        assert_eq!(
            forward::run_with_sender(&conn, &mut sender, 20, now + 1)
                .unwrap()
                .sent,
            0
        );
        assert_eq!(
            forward::run_with_sender(&conn, &mut sender, 20, now + 60)
                .unwrap()
                .sent,
            1
        );
        drop(_lock);
        drop(conn);
        process(&app, input(json!({"op":"preview"}))).unwrap();
        let reopened = app.conn().unwrap();
        let _lock = forward::lock(&app.data).unwrap();
        assert_eq!(
            forward::run_with_sender(&reopened, &mut sender, 20, now + 120)
                .unwrap()
                .sent,
            0
        );
        assert_eq!(sender.calls, 2);
    }
    #[test]
    fn incomplete_store_cannot_initialize_and_unknown_requires_explicit_risk() {
        let (_dir, app, store) = setup();
        std::fs::remove_file(store.join("Inbox.msf")).unwrap();
        assert!(
            process(
                &app,
                input(json!({"op":"initialize","gmail":"student@gmail.com","mode":"now"}))
            )
            .is_err()
        );
        assert!(forward::config(&app.conn().unwrap()).is_err());
        std::fs::write(store.join("Inbox.msf"), b"").unwrap();
        initialize(&app, "since", Some("2026-09-07T12:30:00+09:00"));
        let conn = app.conn().unwrap();
        conn.execute(
            "INSERT INTO deliveries(message_id_fk,status) VALUES(2,'unknown')",
            [],
        )
        .unwrap();
        assert!(process(&app, input(json!({"op":"retry","id":2}))).is_err());
        process(
            &app,
            input(json!({"op":"retry","id":2,"accept_duplicate_risk":true})),
        )
        .unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT status FROM deliveries WHERE message_id_fk=2",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "pending"
        );
    }
    #[tokio::test]
    async fn localhost_host_origin_and_csrf_are_required() {
        let (_dir, app, _) = setup();
        for (host, origin, csrf) in [
            ("evil.example", "http://127.0.0.1:8082", "test-csrf"),
            ("127.0.0.1:8082", "https://evil.example", "test-csrf"),
            ("127.0.0.1:8082", "http://127.0.0.1:8082", "incorrect"),
        ] {
            let request = Request::builder()
                .method("POST")
                .uri("/api/action")
                .header("host", host)
                .header("origin", origin)
                .header("x-csrf-token", csrf)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"op":"send"}"#))
                .unwrap();
            assert_eq!(
                router(app.clone()).oneshot(request).await.unwrap().status(),
                StatusCode::FORBIDDEN
            );
        }
        let request = Request::builder()
            .uri("/api/status")
            .header("host", "127.0.0.1:8082")
            .body(Body::empty())
            .unwrap();
        let response = router(app).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    }
}
