//! Web OAuth and encrypted credential vault. Provider responses are never logged.
use crate::{forward, gmail};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use reqwest::{Url, blocking::Client};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{env, io::Read, path::PathBuf};

#[derive(Clone)]
pub struct Config {
    pub public_url: Url,
    pub data_dir: PathBuf,
    pub owner_gmail: String,
    pub token_key: [u8; 32],
    pub google_client_id: String,
    pub google_client_secret: String,
    pub microsoft_client_id: String,
    pub microsoft_client_secret: String,
    pub microsoft_tenant: String,
    pub interval: u64,
}
impl Config {
    pub fn from_env(data_dir: PathBuf) -> Result<Self> {
        let public_url = Url::parse(
            &env::var("HIRO_MAILD_PUBLIC_URL")
                .or_else(|_| env::var("RENDER_EXTERNAL_URL"))
                .context("set HIRO_MAILD_PUBLIC_URL to the web service origin")?,
        )?;
        if public_url.path() != "/"
            || public_url.query().is_some()
            || public_url.fragment().is_some()
            || !public_url.username().is_empty()
            || public_url.password().is_some()
            || !(public_url.scheme() == "https"
                || (public_url.scheme() == "http"
                    && matches!(public_url.host_str(), Some("127.0.0.1" | "localhost"))))
        {
            bail!(
                "HIRO_MAILD_PUBLIC_URL must be an HTTPS origin (HTTP loopback allowed for local development)"
            );
        }
        let owner_gmail = env::var("HIRO_MAILD_OWNER_GMAIL")
            .context("set HIRO_MAILD_OWNER_GMAIL; this deployment is for one owner")?
            .to_lowercase();
        forward::validate_gmail(&owner_gmail)?;
        let key = env::var("HIRO_MAILD_TOKEN_KEY")
            .context("set a persistent random HIRO_MAILD_TOKEN_KEY of at least 32 characters")?;
        if key.len() < 32 {
            bail!("HIRO_MAILD_TOKEN_KEY must contain at least 32 random characters");
        }
        let microsoft_tenant =
            env::var("MICROSOFT_TENANT_ID").unwrap_or_else(|_| "organizations".into());
        if microsoft_tenant.is_empty()
            || !microsoft_tenant
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            bail!("invalid Microsoft tenant ID");
        }
        let interval = env::var("HIRO_MAILD_CLOUD_INTERVAL_SECONDS")
            .unwrap_or_else(|_| "60".into())
            .parse()?;
        if !(30..=3600).contains(&interval) {
            bail!("cloud interval must be 30..3600 seconds");
        }
        Ok(Self {
            public_url,
            data_dir,
            owner_gmail,
            token_key: Sha256::digest(key.as_bytes()).into(),
            google_client_id: env::var("GOOGLE_CLIENT_ID").unwrap_or_default(),
            google_client_secret: env::var("GOOGLE_CLIENT_SECRET").unwrap_or_default(),
            microsoft_client_id: env::var("MICROSOFT_CLIENT_ID").unwrap_or_default(),
            microsoft_client_secret: env::var("MICROSOFT_CLIENT_SECRET").unwrap_or_default(),
            microsoft_tenant,
            interval,
        })
    }
    pub fn origin(&self) -> String {
        self.public_url.origin().ascii_serialization()
    }
    pub fn callback(&self, provider: &str) -> String {
        format!("{}/auth/{provider}/callback", self.origin())
    }
    pub fn ready(&self, provider: &str) -> bool {
        match provider {
            "google" => !self.google_client_id.is_empty() && !self.google_client_secret.is_empty(),
            "microsoft" => {
                !self.microsoft_client_id.is_empty() && !self.microsoft_client_secret.is_empty()
            }
            _ => false,
        }
    }
    fn client_credentials(&self, provider: &str) -> Result<(&str, &str)> {
        if !self.ready(provider) {
            bail!("OAuth provider is not configured");
        }
        match provider {
            "google" => Ok((&self.google_client_id, &self.google_client_secret)),
            "microsoft" => Ok((&self.microsoft_client_id, &self.microsoft_client_secret)),
            _ => bail!("unsupported provider"),
        }
    }
    fn token_url(&self, provider: &str) -> Result<String> {
        match provider {
            "google" => Ok("https://oauth2.googleapis.com/token".into()),
            "microsoft" => Ok(format!(
                "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
                self.microsoft_tenant
            )),
            _ => bail!("unsupported provider"),
        }
    }
}

pub fn schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(r#"
        CREATE TABLE IF NOT EXISTS cloud_credentials(provider TEXT PRIMARY KEY, email TEXT NOT NULL, encrypted BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS cloud_sessions(hash TEXT PRIMARY KEY, csrf TEXT NOT NULL, expires INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS cloud_oauth(hash TEXT PRIMARY KEY, provider TEXT NOT NULL, encrypted BLOB NOT NULL, expires INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS cloud_mailbox(singleton INTEGER PRIMARY KEY CHECK(singleton=1), identity TEXT NOT NULL, email TEXT NOT NULL);
    "#)?;
    Ok(())
}

pub fn hash(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(value.as_bytes()))
}

pub fn seal(config: &Config, context: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new((&config.token_key).into());
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|_| anyhow::anyhow!("randomness unavailable"))?;
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: bytes,
                aad: context.as_bytes(),
            },
        )
        .map_err(|_| anyhow::anyhow!("credential encryption failed"))?;
    Ok([nonce.as_slice(), ciphertext.as_slice()].concat())
}
fn unseal(config: &Config, context: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() < 28 {
        bail!("encrypted credential is invalid");
    }
    ChaCha20Poly1305::new((&config.token_key).into())
        .decrypt(
            Nonce::from_slice(&bytes[..12]),
            Payload {
                msg: &bytes[12..],
                aad: context.as_bytes(),
            },
        )
        .map_err(|_| {
            anyhow::anyhow!("credential decryption failed; verify the persistent encryption key")
        })
}

#[derive(Serialize, Deserialize)]
struct Credential {
    refresh_token: String,
}

pub fn save_credential(
    conn: &Connection,
    config: &Config,
    provider: &str,
    email: &str,
    refresh_token: &str,
) -> Result<()> {
    let encrypted = seal(
        config,
        &format!("credential:{provider}:{email}"),
        &serde_json::to_vec(&Credential {
            refresh_token: refresh_token.into(),
        })?,
    )?;
    conn.execute("INSERT INTO cloud_credentials(provider,email,encrypted) VALUES(?1,?2,?3) ON CONFLICT(provider) DO UPDATE SET email=excluded.email,encrypted=excluded.encrypted", params![provider,email,encrypted])?;
    Ok(())
}

pub fn connection_email(conn: &Connection, provider: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT email FROM cloud_credentials WHERE provider=?1",
            [provider],
            |r| r.get(0),
        )
        .optional()?)
}

pub fn access_token(conn: &Connection, config: &Config, provider: &str) -> Result<String> {
    let (email, encrypted): (String, Vec<u8>) = conn
        .query_row(
            "SELECT email,encrypted FROM cloud_credentials WHERE provider=?1",
            [provider],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .context("provider is not connected")?;
    let credential: Credential = serde_json::from_slice(&unseal(
        config,
        &format!("credential:{provider}:{email}"),
        &encrypted,
    )?)?;
    let (id, secret) = config.client_credentials(provider)?;
    let token = token_request(
        &gmail::http_client()?,
        &config.token_url(provider)?,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", id),
            ("client_secret", secret),
            ("refresh_token", &credential.refresh_token),
        ],
    )?;
    if let Some(refresh) = token.refresh_token {
        save_credential(conn, config, provider, &email, &refresh)?;
    }
    Ok(token.access_token)
}

#[derive(Serialize, Deserialize)]
struct Pending {
    verifier: String,
    browser_hash: String,
    session_hash: Option<String>,
}

pub fn begin(
    conn: &Connection,
    config: &Config,
    provider: &str,
    browser: &str,
    session: Option<String>,
) -> Result<String> {
    let (id, _) = config.client_credentials(provider)?;
    let state = gmail::random_token()?;
    let verifier = gmail::random_token()?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut url = match provider {
        "google" => Url::parse("https://accounts.google.com/o/oauth2/v2/auth")?,
        "microsoft" => Url::parse(&format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/authorize",
            config.microsoft_tenant
        ))?,
        _ => bail!("unsupported provider"),
    };
    let scopes = if provider == "google" {
        "openid email https://www.googleapis.com/auth/gmail.send"
    } else {
        "offline_access https://graph.microsoft.com/User.Read https://graph.microsoft.com/Mail.Read"
    };
    url.query_pairs_mut().extend_pairs([
        ("client_id", id),
        ("redirect_uri", &config.callback(provider)),
        ("response_type", "code"),
        ("scope", scopes),
        ("state", &state),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
    ]);
    if provider == "google" {
        url.query_pairs_mut().extend_pairs([
            ("access_type", "offline"),
            ("prompt", "consent"),
            ("login_hint", &config.owner_gmail),
        ]);
    } else {
        url.query_pairs_mut()
            .append_pair("prompt", "select_account");
    }
    let encrypted = seal(
        config,
        &format!("oauth:{provider}:{}", hash(&state)),
        &serde_json::to_vec(&Pending {
            verifier,
            browser_hash: hash(browser),
            session_hash: session,
        })?,
    )?;
    conn.execute(
        "DELETE FROM cloud_oauth WHERE expires < ?1",
        [forward::unix_now()],
    )?;
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM cloud_oauth", [], |r| r.get(0))?;
    if count >= 100 {
        bail!("too many pending sign-ins; try later");
    }
    conn.execute(
        "INSERT INTO cloud_oauth(hash,provider,encrypted,expires) VALUES(?1,?2,?3,?4)",
        params![hash(&state), provider, encrypted, forward::unix_now() + 600],
    )?;
    Ok(url.into())
}

fn consume_pending(
    conn: &Connection,
    config: &Config,
    provider: &str,
    state: &str,
    browser: &str,
    session: Option<&str>,
) -> Result<Pending> {
    let tx = conn.unchecked_transaction()?;
    let encrypted: Vec<u8> = tx
        .query_row(
            "SELECT encrypted FROM cloud_oauth WHERE hash=?1 AND provider=?2 AND expires>=?3",
            params![hash(state), provider, forward::unix_now()],
            |r| r.get(0),
        )
        .context("sign-in expired or state mismatch")?;
    let pending: Pending = serde_json::from_slice(&unseal(
        config,
        &format!("oauth:{provider}:{}", hash(state)),
        &encrypted,
    )?)?;
    if pending.browser_hash != hash(browser) || pending.session_hash.as_deref() != session {
        bail!("sign-in browser/session mismatch");
    }
    tx.execute("DELETE FROM cloud_oauth WHERE hash=?1", [hash(state)])?;
    tx.commit()?;
    Ok(pending)
}

#[derive(Deserialize)]
struct Token {
    access_token: String,
    refresh_token: Option<String>,
    scope: Option<String>,
}

fn token_request(client: &Client, endpoint: &str, form: &[(&str, &str)]) -> Result<Token> {
    let response = client
        .post(endpoint)
        .form(form)
        .send()
        .map_err(|_| anyhow::anyhow!("OAuth connection failed"))?;
    if !response.status().is_success() {
        bail!("OAuth request rejected; reconnect or check application consent");
    }
    serde_json::from_slice(&bounded_bytes(response, 1024 * 1024)?)
        .map_err(|_| anyhow::anyhow!("invalid OAuth response"))
}

pub fn finish(
    conn: &Connection,
    config: &Config,
    provider: &str,
    state: &str,
    browser: &str,
    session: Option<&str>,
    code: &str,
) -> Result<()> {
    let pending = consume_pending(conn, config, provider, state, browser, session)?;
    let (id, secret) = config.client_credentials(provider)?;
    let client = gmail::http_client()?;
    let token = token_request(
        &client,
        &config.token_url(provider)?,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", id),
            ("client_secret", secret),
            ("code", code),
            ("redirect_uri", &config.callback(provider)),
            ("code_verifier", &pending.verifier),
        ],
    )?;
    verify_scope(provider, token.scope.as_deref().unwrap_or_default())?;
    let refresh = token
        .refresh_token
        .context("offline access not granted; authorize again")?;
    let (email, mailbox_id) = if provider == "google" {
        #[derive(Deserialize)]
        struct GoogleIdentity {
            email: String,
            email_verified: bool,
        }
        let response = client
            .get("https://openidconnect.googleapis.com/v1/userinfo")
            .bearer_auth(&token.access_token)
            .send()
            .map_err(|_| anyhow::anyhow!("Google account check failed"))?;
        if !response.status().is_success() {
            bail!("Google account check rejected");
        }
        let identity: GoogleIdentity =
            serde_json::from_slice(&bounded_bytes(response, 1024 * 1024)?)
                .map_err(|_| anyhow::anyhow!("invalid Google identity"))?;
        validate_owner(config, &identity.email, identity.email_verified)?;
        (identity.email.to_lowercase(), None)
    } else {
        #[derive(Deserialize)]
        struct MicrosoftIdentity {
            id: String,
            mail: Option<String>,
            #[serde(rename = "userPrincipalName")]
            principal: String,
        }
        let response = client
            .get("https://graph.microsoft.com/v1.0/me?$select=id,mail,userPrincipalName")
            .bearer_auth(&token.access_token)
            .send()
            .map_err(|_| anyhow::anyhow!("university account check failed"))?;
        if !response.status().is_success() {
            bail!("university account check rejected");
        }
        let identity: MicrosoftIdentity =
            serde_json::from_slice(&bounded_bytes(response, 1024 * 1024)?)
                .map_err(|_| anyhow::anyhow!("invalid university identity"))?;
        let email = identity
            .mail
            .filter(|email| university_email(email))
            .unwrap_or(identity.principal)
            .to_lowercase();
        if !university_email(&email) || identity.id.is_empty() {
            bail!("only Hiroshima University mail accounts are supported");
        }
        (email, Some(identity.id))
    };
    let tx = conn.unchecked_transaction()?;
    if let Some(identity) = mailbox_id {
        bind_mailbox(&tx, &identity, &email)?;
    }
    save_credential(&tx, config, provider, &email, &refresh)?;
    tx.commit()?;
    Ok(())
}

fn verify_scope(provider: &str, scope: &str) -> Result<()> {
    let valid = match provider {
        "google" => scope
            .split_whitespace()
            .any(|s| s == "https://www.googleapis.com/auth/gmail.send"),
        "microsoft" => scope.split_whitespace().any(|s| {
            s.rsplit('/')
                .next()
                .unwrap_or_default()
                .eq_ignore_ascii_case("Mail.Read")
        }),
        _ => false,
    };
    if !valid {
        bail!("required mail permission was not granted");
    }
    Ok(())
}
fn validate_owner(config: &Config, email: &str, verified: bool) -> Result<()> {
    if !verified || !email.eq_ignore_ascii_case(&config.owner_gmail) {
        bail!("this service is restricted to its configured owner");
    }
    Ok(())
}
fn university_email(email: &str) -> bool {
    email.to_lowercase().ends_with("@hiroshima-u.ac.jp")
        && email.bytes().filter(|&b| b == b'@').count() == 1
        && !email.chars().any(char::is_whitespace)
}
fn bind_mailbox(conn: &Connection, identity: &str, email: &str) -> Result<()> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT identity FROM cloud_mailbox WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if existing
        .as_deref()
        .is_some_and(|existing| existing != identity)
    {
        bail!(
            "a different university mailbox is already bound; keep the existing delivery history"
        );
    }
    conn.execute("INSERT INTO cloud_mailbox(singleton,identity,email) VALUES(1,?1,?2) ON CONFLICT(singleton) DO UPDATE SET email=excluded.email", params![identity,email])?;
    Ok(())
}

/// Read bounded API JSON/MIME without allowing an unexpectedly large response allocation.
pub fn bounded_bytes(response: reqwest::blocking::Response, maximum: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    response
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("provider response interrupted"))?;
    if bytes.len() > maximum {
        bail!("provider response exceeds the import size cap");
    }
    Ok(bytes)
}

#[cfg(test)]
pub fn test_config(dir: &std::path::Path) -> Config {
    Config {
        public_url: Url::parse("http://127.0.0.1:8080").unwrap(),
        data_dir: dir.into(),
        owner_gmail: "test@gmail.com".into(),
        token_key: [42; 32],
        google_client_id: "fake-google".into(),
        google_client_secret: "fake-secret".into(),
        microsoft_client_id: "fake-ms".into(),
        microsoft_client_secret: "fake-secret".into(),
        microsoft_tenant: "organizations".into(),
        interval: 60,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (tempfile::TempDir, Connection, Config) {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());
        let conn = Connection::open_in_memory().unwrap();
        schema(&conn).unwrap();
        (dir, conn, config)
    }
    fn state(url: &str) -> String {
        Url::parse(url)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .into_owned()
    }

    #[test]
    fn credentials_are_encrypted_and_bound_to_account_and_key() {
        let (_dir, conn, mut config) = setup();
        save_credential(
            &conn,
            &config,
            "google",
            "test@gmail.com",
            "fake-refresh-never-send",
        )
        .unwrap();
        let blob: Vec<u8> = conn
            .query_row("SELECT encrypted FROM cloud_credentials", [], |r| r.get(0))
            .unwrap();
        assert!(!String::from_utf8_lossy(&blob).contains("fake-refresh-never-send"));
        let context = "credential:google:test@gmail.com";
        let clear = unseal(&config, context, &blob).unwrap();
        assert_eq!(
            serde_json::from_slice::<Credential>(&clear)
                .unwrap()
                .refresh_token,
            "fake-refresh-never-send"
        );
        assert!(unseal(&config, "credential:microsoft:test@gmail.com", &blob).is_err());
        let mut tampered = blob.clone();
        tampered[15] ^= 1;
        assert!(unseal(&config, context, &tampered).is_err());
        config.token_key[0] ^= 1;
        assert!(unseal(&config, context, &blob).is_err());
    }
    #[test]
    fn oauth_state_is_pkce_browser_session_provider_bound_and_single_use() {
        let (_dir, conn, config) = setup();
        let url = begin(
            &conn,
            &config,
            "microsoft",
            "browser-a",
            Some("session-a".into()),
        )
        .unwrap();
        let parsed = Url::parse(&url).unwrap();
        let query = parsed
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(query["code_challenge_method"], "S256");
        assert!(query["scope"].contains("Mail.Read"));
        assert!(!query["scope"].contains("ReadWrite"));
        let state = state(&url);
        assert!(
            consume_pending(
                &conn,
                &config,
                "microsoft",
                &state,
                "browser-b",
                Some("session-a")
            )
            .is_err()
        );
        assert!(
            consume_pending(
                &conn,
                &config,
                "microsoft",
                &state,
                "browser-a",
                Some("session-b")
            )
            .is_err()
        );
        assert!(
            consume_pending(
                &conn,
                &config,
                "google",
                &state,
                "browser-a",
                Some("session-a")
            )
            .is_err()
        );
        let pending = consume_pending(
            &conn,
            &config,
            "microsoft",
            &state,
            "browser-a",
            Some("session-a"),
        )
        .unwrap();
        assert_eq!(
            URL_SAFE_NO_PAD.encode(Sha256::digest(pending.verifier.as_bytes())),
            query["code_challenge"]
        );
        assert!(
            consume_pending(
                &conn,
                &config,
                "microsoft",
                &state,
                "browser-a",
                Some("session-a")
            )
            .is_err()
        );
        let state = state_of_new(&conn, &config);
        conn.execute("UPDATE cloud_oauth SET expires=0", [])
            .unwrap();
        assert!(consume_pending(&conn, &config, "google", &state, "browser-a", None).is_err());
    }
    fn state_of_new(conn: &Connection, config: &Config) -> String {
        state(&begin(conn, config, "google", "browser-a", None).unwrap())
    }
    #[test]
    fn scope_owner_and_mailbox_checks_reject_unintended_accounts() {
        let (_dir, conn, config) = setup();
        assert!(validate_owner(&config, "TEST@gmail.com", true).is_ok());
        assert!(validate_owner(&config, "test@gmail.com", false).is_err());
        assert!(validate_owner(&config, "other@gmail.com", true).is_err());
        assert!(verify_scope("microsoft", "User.Read Mail.Read").is_ok());
        assert!(verify_scope("microsoft", "Mail.ReadWrite").is_err());
        assert!(verify_scope("google", "openid email").is_err());
        assert!(university_email("student@hiroshima-u.ac.jp"));
        assert!(!university_email("student@hiroshima-u.ac.jp.evil.test"));
        assert!(!university_email("attacker@example.com@hiroshima-u.ac.jp"));
        bind_mailbox(&conn, "account-1", "student@hiroshima-u.ac.jp").unwrap();
        bind_mailbox(&conn, "account-1", "renamed@hiroshima-u.ac.jp").unwrap();
        assert!(bind_mailbox(&conn, "account-2", "other@hiroshima-u.ac.jp").is_err());
    }
}
