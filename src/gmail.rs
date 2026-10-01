//! Gmail transport and Mac-only OAuth credential storage. Never log remote bodies/tokens.
use std::time::Duration;

use anyhow::{Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::blocking::Client;
use serde::Deserialize;

use crate::forward::{SendOutcome, Sender};

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const SEND_URL: &str = "https://gmail.googleapis.com/gmail/v1/users/me/messages/send";
const SCOPES: &str = "https://www.googleapis.com/auth/gmail.send openid email";

pub fn http_client() -> Result<Client> {
    let builder = Client::builder()
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        // messages.send has no idempotency key. Never allow hidden transport retries.
        .retry(reqwest::retry::never());
    #[cfg(test)]
    let builder = builder.no_proxy();
    Ok(builder.build()?)
}

#[derive(Deserialize, serde::Serialize)]
struct Credentials {
    client_id: String,
    client_secret: String,
    refresh_token: String,
    gmail: String,
}

#[derive(Deserialize)]
struct Token {
    access_token: String,
    refresh_token: Option<String>,
    scope: Option<String>,
}

pub struct GmailSender {
    client: Client,
    access_token: String,
}

impl GmailSender {
    pub fn from_access_token(access_token: String) -> Result<Self> {
        Ok(Self {
            client: http_client()?,
            access_token,
        })
    }
    pub fn connect(gmail: &str) -> Result<Self> {
        let credentials = load_credentials(gmail)?;
        let client = http_client()?;
        let response = client
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", credentials.client_id.as_str()),
                ("client_secret", credentials.client_secret.as_str()),
                ("refresh_token", credentials.refresh_token.as_str()),
            ])
            .send()
            .map_err(|_| anyhow::anyhow!("OAuth refresh connection failed; no mail submitted"))?;
        if !response.status().is_success() {
            bail!(
                "OAuth refresh rejected (HTTP {}); run gmail-auth again; no mail submitted",
                response.status().as_u16()
            );
        }
        let token: Token = response
            .json()
            .map_err(|_| anyhow::anyhow!("invalid OAuth refresh response"))?;
        validate_identity(&client, &token.access_token, gmail)?;
        Ok(Self {
            client,
            access_token: token.access_token,
        })
    }
}

impl Sender for GmailSender {
    fn send(&mut self, mime: &[u8]) -> SendOutcome {
        self.submit(mime, SEND_URL)
    }
}

impl GmailSender {
    fn submit(&mut self, mime: &[u8], endpoint: &str) -> SendOutcome {
        let response = self
            .client
            .post(endpoint)
            .bearer_auth(&self.access_token)
            .json(&serde_json::json!({"raw": URL_SAFE_NO_PAD.encode(mime)}))
            .send();
        match response {
            Err(error) if error.is_connect() => SendOutcome::Retryable("connection_failed".into()),
            // A timeout/reset may occur after Google accepted the mail.
            Err(_) => SendOutcome::Unknown("transport_result_unknown".into()),
            Ok(response) => {
                let status = response.status().as_u16();
                if (200..300).contains(&status) {
                    #[derive(Deserialize)]
                    struct Sent {
                        id: String,
                    }
                    match response.json::<Sent>() {
                        Ok(sent) if !sent.id.is_empty() => SendOutcome::Sent(sent.id),
                        _ => SendOutcome::Unknown("success_response_unreadable".into()),
                    }
                } else {
                    classify_http(status)
                }
            }
        }
    }
}

fn classify_http(status: u16) -> SendOutcome {
    match status {
        429 => SendOutcome::Retryable("http_429".into()),
        // Conservatively hold 5xx too: this non-idempotent operation may have completed.
        408 | 500..=599 => SendOutcome::Unknown(format!("http_{status}")),
        _ => SendOutcome::Blocked(format!("http_{status}")),
    }
}

fn validate_identity(client: &Client, access_token: &str, gmail: &str) -> Result<()> {
    #[derive(Deserialize)]
    struct Identity {
        email: String,
        email_verified: bool,
    }
    let response = client
        .get("https://openidconnect.googleapis.com/v1/userinfo")
        .bearer_auth(access_token)
        .send()
        .map_err(|_| {
            anyhow::anyhow!("account verification connection failed; no mail submitted")
        })?;
    if !response.status().is_success() {
        bail!("account verification rejected; run gmail-auth again");
    }
    let identity: Identity = response
        .json()
        .map_err(|_| anyhow::anyhow!("invalid account verification response"))?;
    if !identity.email_verified || !identity.email.eq_ignore_ascii_case(gmail) {
        bail!("authenticated Google account differs from configured Gmail; no mail submitted");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn entry(gmail: &str) -> Result<keyring::Entry> {
    keyring::Entry::new("hiro-maild.gmail", gmail)
        .map_err(|_| anyhow::anyhow!("could not access Mac Keychain"))
}

#[cfg(target_os = "macos")]
fn load_credentials(gmail: &str) -> Result<Credentials> {
    let secret = entry(gmail)?.get_password().map_err(|_| {
        anyhow::anyhow!("Gmail credentials unavailable in Mac Keychain; run gmail-auth")
    })?;
    let credentials: Credentials = serde_json::from_str(&secret)
        .map_err(|_| anyhow::anyhow!("invalid credentials in Mac Keychain; run gmail-auth"))?;
    if credentials.gmail != gmail {
        bail!("Keychain account mismatch");
    }
    Ok(credentials)
}

#[cfg(not(target_os = "macos"))]
fn load_credentials(_: &str) -> Result<Credentials> {
    bail!("live Gmail forwarding currently requires Mac Keychain; dry-run works on every platform")
}

pub fn random_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| anyhow::anyhow!("system randomness unavailable"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(target_os = "macos")]
pub fn authenticate(gmail: &str, client_json: &std::path::Path) -> Result<()> {
    let bytes = std::fs::read(client_json)
        .map_err(|_| anyhow::anyhow!("cannot read OAuth desktop client JSON"))?;
    authenticate_bytes(gmail, &bytes, |url| {
        println!("Open this URL in your browser on this Mac (expires in 5 minutes):\n{url}");
    })?;
    println!("OAuth credentials saved in Mac Keychain. No email sent.");
    Ok(())
}

/// Local UI input stays in memory; only verified credentials enter Mac Keychain.
#[cfg(target_os = "macos")]
pub fn authenticate_bytes(gmail: &str, bytes: &[u8], show_url: impl FnOnce(String)) -> Result<()> {
    use sha2::{Digest, Sha256};
    use std::{
        io::{Read, Write},
        net::TcpListener,
        time::Instant,
    };

    crate::forward::validate_gmail(gmail)?;
    #[derive(Deserialize)]
    struct DesktopClient {
        client_id: String,
        client_secret: String,
    }
    #[derive(Deserialize)]
    struct ClientFile {
        installed: DesktopClient,
    }
    let config: ClientFile = serde_json::from_slice(bytes).map_err(|_| {
        anyhow::anyhow!("expected downloaded Desktop app client JSON with installed section")
    })?;
    let client = http_client()?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let redirect = format!("http://127.0.0.1:{}/", listener.local_addr()?.port());
    let state = random_token()?;
    let verifier = random_token()?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut url = reqwest::Url::parse("https://accounts.google.com/o/oauth2/v2/auth")?;
    url.query_pairs_mut().extend_pairs([
        ("client_id", config.installed.client_id.as_str()),
        ("redirect_uri", &redirect),
        ("response_type", "code"),
        ("scope", SCOPES),
        ("state", &state),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("login_hint", gmail),
    ]);
    show_url(url.to_string());
    let deadline = Instant::now() + Duration::from_secs(300);
    let code = loop {
        if Instant::now() >= deadline {
            bail!("OAuth browser callback timed out");
        }
        let (mut stream, _) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(_) => bail!("OAuth callback listener failed"),
        };
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let mut request = Vec::new();
        // Bounded, read only the request headers. Never log the authorization code.
        let mut byte = [0u8; 1];
        while request.len() < 16_384 {
            match stream.read(&mut byte) {
                Ok(1) => request.push(byte[0]),
                _ => break,
            }
            if request.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let request = String::from_utf8_lossy(&request);
        let target = request
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("GET "))
            .and_then(|line| line.strip_suffix(" HTTP/1.1"));
        let callback = target
            .and_then(|target| reqwest::Url::parse(&format!("http://127.0.0.1{target}")).ok());
        let result = callback.as_ref().and_then(|url| callback_code(url, &state));
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nCache-Control: no-store\r\nConnection: close\r\n\r\nReturn to the hiro-maild browser window or terminal.\n");
        if let Some(result) = result {
            break result?;
        }
    };
    let response = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &config.installed.client_id),
            ("client_secret", &config.installed.client_secret),
            ("redirect_uri", &redirect),
            ("code_verifier", &verifier),
        ])
        .send()
        .map_err(|_| anyhow::anyhow!("OAuth token connection failed"))?;
    if !response.status().is_success() {
        bail!(
            "OAuth code exchange rejected (HTTP {})",
            response.status().as_u16()
        );
    }
    let token: Token = response
        .json()
        .map_err(|_| anyhow::anyhow!("invalid OAuth token response"))?;
    let scopes = token.scope.as_deref().unwrap_or_default();
    if !scopes
        .split_whitespace()
        .any(|scope| scope == "https://www.googleapis.com/auth/gmail.send")
    {
        bail!("Gmail send permission was not granted");
    }
    validate_identity(&client, &token.access_token, gmail)?;
    let credentials = Credentials {
        client_id: config.installed.client_id,
        client_secret: config.installed.client_secret,
        refresh_token: token
            .refresh_token
            .ok_or_else(|| anyhow::anyhow!("offline refresh token not granted; authorize again"))?,
        gmail: gmail.into(),
    };
    entry(gmail)?
        .set_password(&serde_json::to_string(&credentials)?)
        .map_err(|_| anyhow::anyhow!("could not save OAuth credentials in Mac Keychain"))?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn authenticate_bytes(_: &str, _: &[u8], _: impl FnOnce(String)) -> Result<()> {
    bail!("Gmail authentication requires macOS; no credentials were written")
}

#[cfg(any(target_os = "macos", test))]
fn callback_code(url: &reqwest::Url, expected_state: &str) -> Option<Result<String>> {
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
    if url.path() != "/" || pairs.get("state").map(|s| s.as_ref()) != Some(expected_state) {
        return None;
    }
    Some(if pairs.contains_key("error") {
        Err(anyhow::anyhow!("Google authorization was denied"))
    } else {
        pairs
            .get("code")
            .filter(|code| !code.is_empty())
            .map(|code| code.to_string())
            .ok_or_else(|| anyhow::anyhow!("authorization callback had no code"))
    })
}

#[cfg(not(target_os = "macos"))]
pub fn authenticate(_: &str, _: &std::path::Path) -> Result<()> {
    bail!("gmail-auth requires macOS; no credentials were written")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_requires_state_and_expected_path() {
        let url = reqwest::Url::parse("http://127.0.0.1/?code=secret&state=expected").unwrap();
        assert!(callback_code(&url, "wrong").is_none());
        assert_eq!(callback_code(&url, "expected").unwrap().unwrap(), "secret");
        let denied =
            reqwest::Url::parse("http://127.0.0.1/?error=access_denied&state=expected").unwrap();
        assert!(callback_code(&denied, "expected").unwrap().is_err());
    }
    #[test]
    fn non_idempotent_server_errors_are_held() {
        assert!(matches!(classify_http(429), SendOutcome::Retryable(_)));
        assert!(matches!(classify_http(503), SendOutcome::Unknown(_)));
        assert!(matches!(classify_http(401), SendOutcome::Blocked(_)));
    }

    #[test]
    fn gmail_http_payload_and_failure_results_use_only_a_loopback_mock() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };
        for status in [200, 429, 503, 0] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}/messages/send", listener.local_addr().unwrap());
            let mock = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") && request.len() < 16384 {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let headers = String::from_utf8(request).unwrap().to_lowercase();
                assert!(headers.starts_with("post /messages/send http/1.1"));
                let length: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                let mut body = vec![0u8; length];
                stream.read_exact(&mut body).unwrap();
                let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(
                    URL_SAFE_NO_PAD
                        .decode(json["raw"].as_str().unwrap())
                        .unwrap(),
                    b"From: fake@gmail.com\r\n\r\nbody with \xff"
                );
                if status != 0 {
                    let body = if status == 200 {
                        r#"{"id":"mock-id"}"#
                    } else {
                        "{}"
                    };
                    write!(stream, "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                }
                drop(stream);
                listener.set_nonblocking(true).unwrap();
                // Keep listening briefly so an unexpected hidden retry is observable.
                std::thread::sleep(Duration::from_millis(100));
                assert_eq!(
                    listener.accept().unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
            });
            let mut sender = GmailSender {
                client: http_client().unwrap(),
                access_token: "fake-test-token".into(),
            };
            let outcome = sender.submit(b"From: fake@gmail.com\r\n\r\nbody with \xff", &endpoint);
            match status {
                200 => assert!(matches!(outcome, SendOutcome::Sent(id) if id == "mock-id")),
                429 => assert!(matches!(outcome, SendOutcome::Retryable(_))),
                _ => assert!(matches!(outcome, SendOutcome::Unknown(_))),
            }
            mock.join().unwrap();
        }
    }
}
