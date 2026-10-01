//! Pure MIME composition shared by the native app and browser WebAssembly.
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use mail_parser::MessageParser;
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc2822};

#[derive(Serialize)]
pub struct Metadata {
    pub stable_key: String,
    pub subject: String,
    pub sender_name: String,
    pub sender_address: String,
    pub sent_at: String,
    pub message_id: String,
}

pub fn inspect(raw: &[u8]) -> Result<Metadata> {
    let message = MessageParser::default()
        .parse(raw)
        .context("invalid original MIME")?;
    let message_id = message
        .message_id()
        .unwrap_or_default()
        .trim()
        .trim_matches(['<', '>'])
        .to_string();
    let from = message.from().and_then(|a| a.first());
    Ok(Metadata {
        stable_key: if message_id.is_empty() {
            format!("sha256:{}", stable_content_hash(raw))
        } else {
            format!("mid:{message_id}")
        },
        subject: message.subject().unwrap_or("(no subject)").to_string(),
        sender_name: from.and_then(|a| a.name()).unwrap_or_default().to_string(),
        sender_address: from
            .and_then(|a| a.address())
            .or(message.return_address())
            .unwrap_or("unknown")
            .to_string(),
        sent_at: message
            .date()
            .map(|d| d.to_rfc3339())
            .unwrap_or_else(|| "unknown".to_string()),
        message_id,
    })
}

pub fn validate_gmail(gmail: &str) -> Result<()> {
    let Some(local) = gmail.strip_suffix("@gmail.com") else {
        bail!("personal Gmail address required")
    };
    if local.is_empty()
        || !local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
    {
        bail!("invalid Gmail address")
    }
    Ok(())
}

pub fn stable_content_hash(raw: &[u8]) -> String {
    // Thunderbird rewrites these local flags on read/compaction. They must not create
    // a new identity for messages without Message-ID. Keep the stored MIME untouched.
    let mut digest = Sha256::new();
    let mut headers = true;
    let mut skip = false;
    for line in raw.split_inclusive(|byte| *byte == b'\n') {
        if headers {
            let trimmed = line.strip_suffix(b"\n").unwrap_or(line);
            let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);
            if trimmed.is_empty() {
                headers = false;
                skip = false;
            } else if !line.starts_with(b" ") && !line.starts_with(b"\t") {
                let name = line.split(|byte| *byte == b':').next().unwrap_or_default();
                skip = [
                    b"x-mozilla-status".as_slice(),
                    b"x-mozilla-status2",
                    b"x-mozilla-keys",
                ]
                .iter()
                .any(|ignored| name.eq_ignore_ascii_case(ignored));
            }
        }
        if !skip {
            digest.update(line);
        }
    }
    digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn forwarding_message_id(stable_key: &str, gmail: &str) -> String {
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

pub fn compose(
    raw: &[u8],
    message: &Metadata,
    gmail: &str,
    nonce: &str,
    timestamp: OffsetDateTime,
) -> Result<Vec<u8>> {
    validate_gmail(gmail)?;
    if nonce.len() < 16
        || nonce.len() > 80
        || !nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        bail!("invalid MIME boundary")
    }
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
    let boundary = format!("hiro-maild-{}", nonce.to_string());
    if raw
        .windows(boundary.len())
        .any(|w| w == boundary.as_bytes())
    {
        bail!("MIME boundary collision");
    }
    let metadata = format!(
        "Forwarded from Hiroshima University mail.\nOriginal From: {} <{}>\nOriginal Date: {}\nOriginal Subject: {}\nOriginal Message-ID: {}\nThe complete original is attached as original.eml.\n\n",
        message.sender_name.as_str(),
        message.sender_address.as_str(),
        message.sent_at.as_str(),
        message.subject,
        message.message_id.as_str()
    );
    let mut output = format!("From: {gmail}\r\nTo: {gmail}\r\nDate: {}\r\nMessage-ID: {}\r\nSubject: {}\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n--{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n--{boundary}\r\n{content_headers}\r\n",
        timestamp.format(&Rfc2822)?, forwarding_message_id(&message.stable_key, gmail),
        encoded_text(&format!("[広大メール] {}", message.subject)), base64_lines(metadata.as_bytes())).into_bytes();
    output.extend_from_slice(&raw[split.1..]);
    output.extend_from_slice(format!("\r\n--{boundary}\r\nContent-Type: application/octet-stream; name=\"original.eml\"\r\nContent-Disposition: attachment; filename=\"original.eml\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n--{boundary}--\r\n", base64_lines(raw)).as_bytes());
    if output.len() > 34 * 1024 * 1024 {
        bail!("composed MIME exceeds conservative Gmail cap");
    }
    Ok(output)
}

#[cfg(feature = "browser")]
mod browser {
    use super::*;
    use wasm_bindgen::prelude::*;
    #[wasm_bindgen]
    pub fn inspect_mime(raw: &[u8]) -> Result<String, JsValue> {
        let m = inspect(raw).map_err(|_| JsValue::from_str("invalid original MIME"))?;
        serde_json::to_string(&m).map_err(|_| JsValue::from_str("invalid MIME metadata"))
    }
    #[wasm_bindgen]
    pub fn compose_mime(
        raw: &[u8],
        gmail: &str,
        nonce: &str,
        unix_seconds: i64,
    ) -> Result<Vec<u8>, JsValue> {
        let m = inspect(raw).map_err(|_| JsValue::from_str("invalid original MIME"))?;
        let timestamp = OffsetDateTime::from_unix_timestamp(unix_seconds)
            .map_err(|_| JsValue::from_str("invalid timestamp"))?;
        compose(raw, &m, gmail, nonce, timestamp).map_err(|e| JsValue::from_str(&e.to_string()))
    }
    #[wasm_bindgen]
    pub fn forward_id(stable_key: &str, gmail: &str) -> String {
        forwarding_message_id(stable_key, gmail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encoded_subject_cannot_inject_headers_or_overflow_words() {
        assert!(!encoded_text("subject\r\nBcc: other@example.com").contains("\r\nBcc:"));
        for line in encoded_text(&"長い件名".repeat(100)).lines() {
            assert!(line.len() <= 75);
        }
    }
    #[test]
    fn invalid_destination_and_boundary_are_rejected() {
        let raw = b"Subject: example\r\n\r\nbody";
        let meta = inspect(raw).unwrap();
        let now = OffsetDateTime::from_unix_timestamp(0).unwrap();
        assert!(
            compose(
                raw,
                &meta,
                "a@gmail.com\r\nBcc: a@example.com",
                "0123456789abcdef",
                now
            )
            .is_err()
        );
        assert!(compose(raw, &meta, "a@gmail.com", "unsafe\r\nboundary", now).is_err());
        let raw = b"Subject: example\r\n\r\nhiro-maild-0123456789abcdef";
        assert!(compose(raw, &meta, "a@gmail.com", "0123456789abcdef", now).is_err());
    }
    #[test]
    fn no_message_id_identity_survives_local_flag_changes() {
        let a = b"X-Mozilla-Status: 0000\r\nSubject: hi\r\n\r\nbody";
        let b = b"X-Mozilla-Status: 0001\r\nSubject: hi\r\n\r\nbody";
        assert_eq!(
            inspect(a).unwrap().stable_key,
            inspect(b).unwrap().stable_key
        );
        assert!(inspect(a).unwrap().stable_key.starts_with("sha256:"));
    }
}
