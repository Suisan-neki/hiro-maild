//! Same-origin, single-owner setup UI. No MCP endpoint is exposed in web mode.
use crate::{
    cloud,
    cloud_auth::{self, Config},
    db, forward, gmail,
};
use anyhow::Result;
use axum::{
    Json, Router,
    extract::Request,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

#[derive(Clone)]
pub struct App {
    pub config: Arc<Config>,
}
impl App {
    fn conn(&self) -> Result<Connection> {
        let conn = db::open(&self.config.data_dir.join("hiro-maild.sqlite3"))?;
        cloud::schema(&conn)?;
        Ok(conn)
    }
}
#[derive(Debug)]
struct Error(StatusCode, &'static str);
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
type WebResult<T> = std::result::Result<T, Error>;
fn internal(_: anyhow::Error) -> Error {
    Error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "処理に失敗しました。接続設定を確認し、もう一度お試しください。",
    )
}
fn provider(value: &str) -> WebResult<()> {
    if ["google", "microsoft"].contains(&value) {
        Ok(())
    } else {
        Err(Error(StatusCode::NOT_FOUND, "接続先がありません。"))
    }
}
fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            (key == name).then(|| value.to_owned())
        })
}
fn cookie_name(config: &Config, kind: &str) -> String {
    let prefix = if config.public_url.scheme() == "https" {
        "__Host-"
    } else {
        ""
    };
    format!("{prefix}hiro_{kind}")
}
fn cookie_header(config: &Config, kind: &str, value: &str, age: u64) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={age}{}",
        cookie_name(config, kind),
        if config.public_url.scheme() == "https" {
            "; Secure"
        } else {
            ""
        }
    ))
    .expect("opaque random cookies are ASCII")
}
fn session(conn: &Connection, config: &Config, headers: &HeaderMap) -> WebResult<(String, String)> {
    let token = cookie(headers, &cookie_name(config, "session")).ok_or(Error(
        StatusCode::UNAUTHORIZED,
        "Gmailでログインしてください。",
    ))?;
    let hash = cloud_auth::hash(&token);
    let csrf: Option<String> = conn
        .query_row(
            "SELECT csrf FROM cloud_sessions WHERE hash=?1 AND expires>=?2",
            params![hash, forward::unix_now()],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| internal(e.into()))?;
    Ok((
        hash,
        csrf.ok_or(Error(
            StatusCode::UNAUTHORIZED,
            "ログインの期限が切れました。",
        ))?,
    ))
}
fn origin(config: &Config, headers: &HeaderMap) -> WebResult<()> {
    if headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        != Some(config.origin().as_str())
    {
        return Err(Error(
            StatusCode::FORBIDDEN,
            "この画面から操作してください。",
        ));
    }
    Ok(())
}
fn mutation(conn: &Connection, config: &Config, headers: &HeaderMap) -> WebResult<String> {
    origin(config, headers)?;
    let (hash, csrf) = session(conn, config, headers)?;
    if headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
        != Some(csrf.as_str())
    {
        return Err(Error(
            StatusCode::FORBIDDEN,
            "画面を再読み込みしてから操作してください。",
        ));
    }
    Ok(hash)
}

pub fn router(app: App) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(javascript))
        .route("/style.css", get(styles))
        .route("/healthz", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/api/status", get(status))
        .route("/api/preview", get(preview))
        .route("/api/start", post(start))
        .route("/api/sync", post(sync))
        .route("/api/enabled", post(enabled))
        .route("/api/retry", post(retry))
        .route("/api/logout", post(logout))
        .route("/auth/{provider}/start", post(auth_start))
        .route("/auth/{provider}/callback", get(auth_callback))
        .layer(DefaultBodyLimit::max(32 * 1024))
        .layer(middleware::from_fn(security_headers))
        .with_state(app)
}
async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("content-security-policy",HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"));
    response
}
async fn index() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}
async fn javascript() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../web/app.js"),
    )
}
async fn styles() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../web/style.css"),
    )
}

async fn status(State(app): State<App>, headers: HeaderMap) -> WebResult<Json<Value>> {
    let conn = app.conn().map_err(internal)?;
    let Ok((_, csrf)) = session(&conn, &app.config, &headers) else {
        return Ok(Json(
            json!({"authenticated":false,"google_ready":app.config.ready("google")}),
        ));
    };
    let mut counts = json!({"sent":0,"pending":0,"retry":0,"blocked":0,"unknown":0,"sending":0});
    let mut stmt = conn
        .prepare("SELECT status,COUNT(*) FROM deliveries GROUP BY status")
        .map_err(|e| internal(e.into()))?;
    for row in stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .map_err(|e| internal(e.into()))?
    {
        let (name, count) = row.map_err(|e| internal(e.into()))?;
        counts[&name] = json!(count);
    }
    let pending:i64=conn.query_row("SELECT COUNT(*) FROM messages m LEFT JOIN deliveries d ON d.message_id_fk=m.id WHERE d.message_id_fk IS NULL",[],|r|r.get(0)).map_err(|e|internal(e.into()))?;
    counts["pending"] = json!(counts["pending"].as_i64().unwrap_or(0) + pending);
    Ok(Json(
        json!({"authenticated":true,"csrf":csrf,"gmail":app.config.owner_gmail,
        "university":cloud_auth::connection_email(&conn,"microsoft").map_err(internal)?,
        "microsoft_ready":app.config.ready("microsoft"),"settings":cloud::settings(&conn).map_err(internal)?,"counts":counts}),
    ))
}

async fn preview(State(app): State<App>, headers: HeaderMap) -> WebResult<Json<Value>> {
    let conn = app.conn().map_err(internal)?;
    session(&conn, &app.config, &headers)?;
    if cloud::settings(&conn).map_err(internal)?.start_at.is_none() {
        return Ok(Json(json!({"targets":[],"history":[]})));
    }
    let targets = forward::candidates(&conn, 50, false, forward::unix_now()).map_err(internal)?;
    // Latest IDs first for the history; old sent rows must not hide recent held messages.
    let mut stmt=conn.prepare("SELECT d.message_id_fk,d.status,d.error_code,d.gmail_message_id,m.subject,m.stable_key FROM deliveries d JOIN messages m ON m.id=d.message_id_fk ORDER BY message_id_fk DESC LIMIT 30").map_err(|e|internal(e.into()))?;
    let history=stmt.query_map([],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"status":r.get::<_,String>(1)?,"error_code":r.get::<_,Option<String>>(2)?,"gmail_message_id":r.get::<_,Option<String>>(3)?,"subject":r.get::<_,String>(4)?,"forwarding_message_id":forward::forwarding_message_id(&r.get::<_,String>(5)?,&app.config.owner_gmail)}))).map_err(|e|internal(e.into()))?.collect::<rusqlite::Result<Vec<_>>>().map_err(|e|internal(e.into()))?;
    Ok(Json(json!({"targets":targets,"history":history})))
}

#[derive(Deserialize)]
struct Start {
    mode: String,
    since: Option<String>,
}
async fn start(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<Start>,
) -> WebResult<Json<Value>> {
    let conn = app.conn().map_err(internal)?;
    mutation(&conn, &app.config, &headers)?;
    let _lock = forward::lock(&app.config.data_dir).map_err(|_| {
        Error(
            StatusCode::CONFLICT,
            "同期中です。少し待ってから設定してください。",
        )
    })?;
    let since = match input.mode.as_str() {
        "now" => None,
        "since" => Some(input.since.as_deref().ok_or(Error(
            StatusCode::BAD_REQUEST,
            "開始日時を指定してください。",
        ))?),
        _ => return Err(Error(StatusCode::BAD_REQUEST, "開始方法を選んでください。")),
    };
    cloud::initialize(&conn, &app.config, since).map_err(|_| {
        Error(
            StatusCode::BAD_REQUEST,
            "開始位置を設定できません。両方の接続、日時、設定済みの範囲を確認してください。",
        )
    })?;
    Ok(Json(json!({"ok":true})))
}
async fn sync(State(app): State<App>, headers: HeaderMap) -> WebResult<Json<Value>> {
    let conn = app.conn().map_err(internal)?;
    mutation(&conn, &app.config, &headers)?;
    drop(conn);
    let config = app.config.clone();
    let outcome = tokio::task::spawn_blocking(move || cloud::cycle(&config, false))
        .await
        .map_err(|_| {
            Error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "同期処理が終了しました。もう一度お試しください。",
            )
        })?;
    Ok(Json(
        json!({"ok":true,"sync":outcome.map_err(|_|Error(StatusCode::BAD_GATEWAY,"大学メールを取得できません。アプリへの同意と接続を確認してください。"))?}),
    ))
}
#[derive(Deserialize)]
struct Enabled {
    enabled: bool,
}
async fn enabled(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<Enabled>,
) -> WebResult<Json<Value>> {
    let conn = app.conn().map_err(internal)?;
    mutation(&conn, &app.config, &headers)?;
    if cloud::settings(&conn).map_err(internal)?.start_at.is_none() {
        return Err(Error(
            StatusCode::BAD_REQUEST,
            "転送開始位置を設定してください。",
        ));
    }
    if input.enabled
        && cloud::settings(&conn)
            .map_err(internal)?
            .last_sync
            .is_none()
    {
        return Err(Error(
            StatusCode::BAD_REQUEST,
            "送信前に対象を確認してください。",
        ));
    }
    conn.execute(
        "UPDATE cloud_settings SET enabled=?1 WHERE singleton=1",
        [input.enabled],
    )
    .map_err(|e| internal(e.into()))?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
struct Retry {
    id: i64,
    #[serde(default)]
    accept_duplicate_risk: bool,
}
async fn retry(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<Retry>,
) -> WebResult<Json<Value>> {
    let conn = app.conn().map_err(internal)?;
    mutation(&conn, &app.config, &headers)?;
    let _lock = forward::lock(&app.config.data_dir).map_err(|_| {
        Error(
            StatusCode::CONFLICT,
            "転送中です。少し待ってから再試行してください。",
        )
    })?;
    forward::retry(&conn, input.id, input.accept_duplicate_risk).map_err(|_| {
        Error(
            StatusCode::BAD_REQUEST,
            "再試行できません。結果不明の場合はGmailを確認し、重複リスクを了承してください。",
        )
    })?;
    Ok(Json(json!({"ok":true})))
}
async fn logout(State(app): State<App>, headers: HeaderMap) -> WebResult<Response> {
    let conn = app.conn().map_err(internal)?;
    let hash = mutation(&conn, &app.config, &headers)?;
    conn.execute("DELETE FROM cloud_sessions WHERE hash=?1", [hash])
        .map_err(|e| internal(e.into()))?;
    let mut response = Json(json!({"ok":true})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        cookie_header(&app.config, "session", "", 0),
    );
    Ok(response)
}

async fn auth_start(
    State(app): State<App>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> WebResult<Response> {
    provider(&name)?;
    origin(&app.config, &headers)?;
    let conn = app.conn().map_err(internal)?;
    let session = if name == "microsoft" {
        Some(mutation(&conn, &app.config, &headers)?)
    } else {
        None
    };
    if !app.config.ready(&name) {
        return Err(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "管理者がログイン設定を準備中です。",
        ));
    }
    let browser = gmail::random_token().map_err(internal)?;
    let url = cloud_auth::begin(&conn, &app.config, &name, &browser, session).map_err(internal)?;
    let mut response = Json(json!({"url":url})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        cookie_header(&app.config, "oauth", &browser, 600),
    );
    Ok(response)
}
#[derive(Deserialize)]
struct Callback {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
}
async fn auth_callback(
    State(app): State<App>,
    Path(name): Path<String>,
    Query(input): Query<Callback>,
    headers: HeaderMap,
) -> WebResult<Response> {
    provider(&name)?;
    let conn = app.conn().map_err(internal)?;
    let session = if name == "microsoft" {
        Some(session(&conn, &app.config, &headers)?.0)
    } else {
        None
    };
    drop(conn);
    let browser = cookie(&headers, &cookie_name(&app.config, "oauth")).unwrap_or_default();
    if input.error.is_some() || input.state.is_none() || input.code.is_none() || browser.is_empty()
    {
        return Ok(Redirect::to("/?notice=auth_failed").into_response());
    }
    let config = app.config.clone();
    let provider_name = name.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<()> {
        let conn = db::open(&config.data_dir.join("hiro-maild.sqlite3"))?;
        let _lock = forward::lock(&config.data_dir)?;
        cloud_auth::finish(
            &conn,
            &config,
            &provider_name,
            &input.state.unwrap(),
            &browser,
            session.as_deref(),
            &input.code.unwrap(),
        )
    })
    .await;
    let success = matches!(result, Ok(Ok(())));
    let mut response = Redirect::to(if success {
        "/?notice=connected"
    } else {
        "/?notice=auth_failed"
    })
    .into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        cookie_header(&app.config, "oauth", "", 0),
    );
    if success && name == "google" {
        let conn = app.conn().map_err(internal)?;
        let token = gmail::random_token().map_err(internal)?;
        let csrf = gmail::random_token().map_err(internal)?;
        conn.execute(
            "DELETE FROM cloud_sessions WHERE expires < ?1",
            [forward::unix_now()],
        )
        .map_err(|e| internal(e.into()))?;
        conn.execute(
            "INSERT INTO cloud_sessions(hash,csrf,expires) VALUES(?1,?2,?3)",
            params![cloud_auth::hash(&token), csrf, forward::unix_now() + 86400],
        )
        .map_err(|e| internal(e.into()))?;
        response.headers_mut().append(
            header::SET_COOKIE,
            cookie_header(&app.config, "session", &token, 86400),
        );
    }
    Ok(response)
}

pub async fn run(data_dir: PathBuf, bind: String) -> Result<()> {
    let config = Arc::new(Config::from_env(data_dir)?);
    let conn = db::open(&config.data_dir.join("hiro-maild.sqlite3"))?;
    cloud::schema(&conn)?;
    if cloud_auth::connection_email(&conn, "google")?
        .is_some_and(|email| email != config.owner_gmail)
    {
        anyhow::bail!(
            "the persisted Gmail owner cannot be changed; keep the existing delivery history"
        );
    }
    drop(conn);
    let worker_config = config.clone();
    let worker = tokio::spawn(async move {
        loop {
            let config = worker_config.clone();
            let result = tokio::task::spawn_blocking(move || cloud::cycle(&config, true)).await;
            if !matches!(result, Ok(Ok(_))) {
                if let Ok(conn) = db::open(&worker_config.data_dir.join("hiro-maild.sqlite3")) {
                    let _=conn.execute("UPDATE cloud_settings SET last_error='接続または同期に失敗しました。再接続とアプリへの同意を確認してください。' WHERE singleton=1",[]);
                }
                eprintln!("cloud_cycle status=failed");
            }
            tokio::time::sleep(std::time::Duration::from_secs(worker_config.interval)).await;
        }
    });
    let bind = match std::env::var("PORT") {
        Ok(port) => format!("0.0.0.0:{}", port.parse::<u16>()?),
        Err(_) => bind,
    };
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    println!("web listening on {bind}");
    let result = axum::serve(listener, router(App { config }))
        .with_graceful_shutdown(shutdown())
        .await;
    worker.abort();
    let _ = worker.await;
    result?;
    Ok(())
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler");
        tokio::select! { _=tokio::signal::ctrl_c()=>{}, _=terminate.recv()=>{} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use reqwest::Url;
    use tower::ServiceExt;

    fn setup() -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().unwrap();
        let app = App {
            config: Arc::new(cloud_auth::test_config(dir.path())),
        };
        let conn = app.conn().unwrap();
        conn.execute(
            "INSERT INTO cloud_sessions(hash,csrf,expires) VALUES(?1,'fake-csrf',?2)",
            params![cloud_auth::hash("fake-session"), forward::unix_now() + 1000],
        )
        .unwrap();
        (dir, app)
    }
    async fn request(
        app: &App,
        path: &str,
        body: Option<Value>,
        session: bool,
        csrf: bool,
        origin: &str,
    ) -> Response {
        let mut request = Request::builder().uri(path);
        if session {
            request = request.header(header::COOKIE, "hiro_session=fake-session");
        }
        if csrf {
            request = request.header("x-csrf-token", "fake-csrf");
        }
        let body = if let Some(body) = body {
            request = request
                .method("POST")
                .header(header::ORIGIN, origin)
                .header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        } else {
            Body::empty()
        };
        router(app.clone())
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap()
    }
    async fn json_body(response: Response) -> Value {
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
    }

    #[tokio::test]
    async fn anonymous_users_cannot_read_mail_or_change_settings() {
        let (_dir, app) = setup();
        let response = request(&app, "/api/status", None, false, false, "").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
        let body = json_body(response).await;
        assert_eq!(body["authenticated"], false);
        assert!(body.get("gmail").is_none());
        assert!(body.get("csrf").is_none());
        assert_eq!(
            request(&app, "/api/preview", None, false, false, "")
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                &app,
                "/api/enabled",
                Some(json!({"enabled":true})),
                false,
                false,
                &app.config.origin()
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                &app,
                "/auth/microsoft/start",
                Some(json!({})),
                false,
                false,
                &app.config.origin()
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    #[tokio::test]
    async fn mutations_require_origin_session_csrf_and_preview_before_enable() {
        let (_dir, app) = setup();
        let body = Some(json!({"enabled":true}));
        assert_eq!(
            request(
                &app,
                "/api/enabled",
                body.clone(),
                true,
                true,
                "https://attacker.test"
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(
                &app,
                "/api/enabled",
                body.clone(),
                true,
                false,
                &app.config.origin()
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(
                &app,
                "/api/enabled",
                body.clone(),
                true,
                true,
                &app.config.origin()
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        let conn = app.conn().unwrap();
        cloud_auth::save_credential(
            &conn,
            &app.config,
            "google",
            &app.config.owner_gmail,
            "fake",
        )
        .unwrap();
        cloud_auth::save_credential(
            &conn,
            &app.config,
            "microsoft",
            "student@hiroshima-u.ac.jp",
            "fake",
        )
        .unwrap();
        assert_eq!(
            request(
                &app,
                "/api/start",
                Some(json!({"mode":"now"})),
                true,
                true,
                &app.config.origin()
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            request(
                &app,
                "/api/enabled",
                body.clone(),
                true,
                true,
                &app.config.origin()
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        conn.execute(
            "UPDATE cloud_settings SET last_sync=?1",
            [forward::unix_now()],
        )
        .unwrap();
        assert_eq!(
            request(&app, "/api/enabled", body, true, true, &app.config.origin())
                .await
                .status(),
            StatusCode::OK
        );
        assert!(cloud::settings(&conn).unwrap().enabled);
        assert_eq!(
            request(
                &app,
                "/api/enabled",
                Some(json!({"enabled":false})),
                true,
                true,
                &app.config.origin()
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert!(!cloud::settings(&conn).unwrap().enabled);
        assert_eq!(
            request(
                &app,
                "/api/logout",
                Some(json!({})),
                true,
                true,
                &app.config.origin()
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            request(&app, "/api/preview", None, true, false, "")
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    #[test]
    fn production_cookies_are_host_only_secure_and_http_only() {
        let (_dir, app) = setup();
        let mut config = (*app.config).clone();
        config.public_url = Url::parse("https://hiro-maild.example.test").unwrap();
        let header = cookie_header(&config, "session", "opaque", 86400);
        let cookie = header.to_str().unwrap();
        assert!(cookie.starts_with("__Host-hiro_session="));
        assert!(cookie.contains("; Secure"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(!cookie.contains("Domain="));
    }
}
