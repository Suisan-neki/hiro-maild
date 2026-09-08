use std::{
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use mail_parser::{mailbox::mbox::MessageIterator, Message, MessageParser, MimeHeaders};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::{db, extract};

#[derive(Debug, Default)]
pub struct SyncStats {
    pub scanned_files: usize,
    pub parsed_messages: usize,
    pub imported_messages: usize,
    pub skipped_existing: usize,
    pub parse_errors: usize,
    pub attachments_saved: usize,
}

pub fn sync_store(conn: &Connection, store: &Path, attachments_root: &Path) -> Result<SyncStats> {
    if !store.is_dir() {
        anyhow::bail!("Thunderbird store is not a directory: {}", store.display());
    }
    std::fs::create_dir_all(attachments_root)?;

    let mut stats = SyncStats::default();
    for file in discover_mbox_files(store) {
        stats.scanned_files += 1;
        if let Err(error) = import_mbox(conn, &file, attachments_root, &mut stats) {
            eprintln!("warning: failed to import {}: {error:#}", file.display());
            stats.parse_errors += 1;
        }
    }
    Ok(stats)
}

fn discover_mbox_files(store: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in WalkDir::new(store).follow_links(false).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_file() || path.extension().is_some() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|v| v.to_str()) else {
            continue;
        };
        if name.starts_with('.') || name.eq_ignore_ascii_case("lock") {
            continue;
        }
        let msf = path.with_file_name(format!("{name}.msf"));
        if msf.is_file() {
            files.push(path.to_path_buf());
        }
    }
    files.sort();
    files
}

fn import_mbox(
    conn: &Connection,
    path: &Path,
    attachments_root: &Path,
    stats: &mut SyncStats,
) -> Result<()> {
    let file = File::open(path).with_context(|| format!("open mbox {}", path.display()))?;
    let reader = BufReader::new(file);

    for raw_message in MessageIterator::new(reader) {
        let raw_message = match raw_message {
            Ok(value) => value,
            Err(error) => {
                eprintln!("warning: malformed mbox entry in {}: {error}", path.display());
                stats.parse_errors += 1;
                continue;
            }
        };
        stats.parsed_messages += 1;

        let raw = raw_message.contents();
        let Some(message) = MessageParser::default().parse(raw) else {
            stats.parse_errors += 1;
            continue;
        };

        match import_message(conn, path, raw, &message, attachments_root) {
            Ok(ImportOutcome::Imported { attachments }) => {
                stats.imported_messages += 1;
                stats.attachments_saved += attachments;
            }
            Ok(ImportOutcome::Existing) => stats.skipped_existing += 1,
            Err(error) => {
                eprintln!("warning: failed to parse message in {}: {error:#}", path.display());
                stats.parse_errors += 1;
            }
        }
    }
    Ok(())
}

enum ImportOutcome {
    Imported { attachments: usize },
    Existing,
}

fn import_message(
    conn: &Connection,
    source_path: &Path,
    raw: &[u8],
    message: &Message<'_>,
    attachments_root: &Path,
) -> Result<ImportOutcome> {
    let raw_sha256 = format!("{:x}", Sha256::digest(raw));
    let message_id = message.message_id().map(clean_message_id);
    let stable_key = message_id
        .as_deref()
        .map(|id| format!("mid:{id}"))
        .unwrap_or_else(|| format!("sha256:{raw_sha256}"));

    let subject = message.subject().unwrap_or("(no subject)");
    let (sender_name, sender_address) = sender(message);
    let sent_at = message.date().map(|date| date.to_rfc3339());
    let body_text = collect_body_text(message);
    let source_path = source_path.to_string_lossy();

    let new = db::NewMessage {
        stable_key: &stable_key,
        message_id: message_id.as_deref(),
        subject,
        sender_name: sender_name.as_deref(),
        sender_address: sender_address.as_deref(),
        sent_at: sent_at.as_deref(),
        body_text: &body_text,
        raw_sha256: &raw_sha256,
        source_path: &source_path,
    };

    let tx = conn.unchecked_transaction()?;
    let Some(db_id) = db::insert_message(&tx, &new)? else {
        tx.commit()?;
        return Ok(ImportOutcome::Existing);
    };

    let message_attachment_dir = attachments_root.join(db_id.to_string());
    let import_result = (|| -> Result<usize> {
        std::fs::create_dir_all(&message_attachment_dir)?;

        let mut count = 0usize;
        for (ordinal, attachment) in message.attachments().enumerate() {
            if attachment.is_message() {
                continue;
            }
            let original_name = attachment
                .attachment_name()
                .map(str::to_string)
                .unwrap_or_else(|| format!("attachment-{}.bin", ordinal + 1));
            let safe_name = extract::sanitize_filename(&original_name);
            let disk_name = format!("{:02}_{}", ordinal + 1, safe_name);
            let output = message_attachment_dir.join(disk_name);
            std::fs::write(&output, attachment.contents())?;

            let content_type = attachment.content_type().map(|ct| match ct.subtype() {
                Some(subtype) if !subtype.is_empty() => format!("{}/{}", ct.ctype(), subtype),
                _ => ct.ctype().to_string(),
            });
            let extracted = extract::extract_text(&safe_name, attachment.contents());
            db::insert_attachment(
                &tx,
                db_id,
                ordinal,
                &original_name,
                content_type.as_deref(),
                &output.to_string_lossy(),
                attachment.contents().len(),
                extracted.as_deref(),
            )?;
            count += 1;
        }
        Ok(count)
    })();

    match import_result {
        Ok(count) => {
            tx.commit()?;
            Ok(ImportOutcome::Imported { attachments: count })
        }
        Err(error) => {
            drop(tx); // rolls back the database insert
            let _ = std::fs::remove_dir_all(&message_attachment_dir);
            Err(error)
        }
    }
}

fn clean_message_id(value: &str) -> String {
    value
        .trim()
        .trim_matches(|ch| ch == '<' || ch == '>')
        .to_string()
}

fn sender(message: &Message<'_>) -> (Option<String>, Option<String>) {
    let Some(addresses) = message.from() else {
        return (None, message.return_address().map(str::to_string));
    };
    let Some(first) = addresses.first() else {
        return (None, message.return_address().map(str::to_string));
    };
    (
        first.name().map(str::to_string),
        first.address().map(str::to_string),
    )
}

fn collect_body_text(message: &Message<'_>) -> String {
    let mut parts = Vec::new();
    for pos in 0..message.text_body_count() {
        if let Some(body) = message.body_text(pos) {
            let body = body.trim();
            if !body.is_empty() {
                parts.push(body.to_string());
            }
        }
    }
    extract::truncate_chars(&parts.join("\n\n"), 200_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_id_is_normalized() {
        assert_eq!(clean_message_id(" <abc@example.com> "), "abc@example.com");
    }
}
