use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct MessageRow {
    pub id: i64,
    pub stable_key: String,
    pub message_id: Option<String>,
    pub subject: String,
    pub sender_name: Option<String>,
    pub sender_address: Option<String>,
    pub sent_at: Option<String>,
    pub body_text: String,
    pub source_path: String,
    pub attachments: Vec<AttachmentRow>,
    pub triage: Option<TriageRow>,
}

#[derive(Debug, Serialize)]
pub struct MessageSummary {
    pub id: i64,
    pub subject: String,
    pub sender_name: Option<String>,
    pub sender_address: Option<String>,
    pub sent_at: Option<String>,
    pub preview: String,
    pub attachment_names: Vec<String>,
    pub triage: Option<TriageRow>,
}

#[derive(Debug, Serialize)]
pub struct AttachmentRow {
    pub id: i64,
    pub filename: String,
    pub content_type: Option<String>,
    pub saved_path: String,
    pub size_bytes: i64,
    pub extracted_text: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TriageRow {
    pub importance: String,
    pub category: String,
    pub summary: String,
    pub requires_action: bool,
    pub action: Option<String>,
    pub deadline: Option<String>,
    pub calendar_title: Option<String>,
    pub calendar_start: Option<String>,
    pub calendar_end: Option<String>,
    pub calendar_location: Option<String>,
    pub confidence: f64,
}

#[derive(Debug)]
pub struct NewMessage<'a> {
    pub stable_key: &'a str,
    pub message_id: Option<&'a str>,
    pub subject: &'a str,
    pub sender_name: Option<&'a str>,
    pub sender_address: Option<&'a str>,
    pub sent_at: Option<&'a str>,
    pub body_text: &'a str,
    pub raw_sha256: &'a str,
    pub source_path: &'a str,
}

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)
        .with_context(|| format!("failed to open SQLite database: {}", path.display()))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS messages (
            id              INTEGER PRIMARY KEY,
            stable_key      TEXT NOT NULL UNIQUE,
            message_id      TEXT,
            subject         TEXT NOT NULL,
            sender_name     TEXT,
            sender_address  TEXT,
            sent_at         TEXT,
            body_text       TEXT NOT NULL,
            raw_sha256      TEXT NOT NULL,
            source_path     TEXT NOT NULL,
            imported_at     TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        CREATE INDEX IF NOT EXISTS idx_messages_sent_at ON messages(sent_at);
        CREATE INDEX IF NOT EXISTS idx_messages_message_id ON messages(message_id);

        CREATE TABLE IF NOT EXISTS attachments (
            id              INTEGER PRIMARY KEY,
            message_id_fk   INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
            ordinal         INTEGER NOT NULL,
            filename        TEXT NOT NULL,
            content_type    TEXT,
            saved_path      TEXT NOT NULL,
            size_bytes      INTEGER NOT NULL,
            extracted_text  TEXT,
            UNIQUE(message_id_fk, ordinal)
        );

        CREATE TABLE IF NOT EXISTS triage (
            message_id_fk       INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
            importance          TEXT NOT NULL,
            category            TEXT NOT NULL,
            summary             TEXT NOT NULL,
            requires_action     INTEGER NOT NULL,
            action              TEXT,
            deadline            TEXT,
            calendar_title      TEXT,
            calendar_start      TEXT,
            calendar_end        TEXT,
            calendar_location   TEXT,
            confidence          REAL NOT NULL,
            raw_json            TEXT NOT NULL,
            triaged_at          TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        "#,
    )?;
    Ok(conn)
}

pub fn insert_message(conn: &Connection, message: &NewMessage<'_>) -> Result<Option<i64>> {
    let changed = conn.execute(
        r#"
        INSERT OR IGNORE INTO messages
            (stable_key, message_id, subject, sender_name, sender_address, sent_at, body_text, raw_sha256, source_path)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        "#,
        params![
            message.stable_key,
            message.message_id,
            message.subject,
            message.sender_name,
            message.sender_address,
            message.sent_at,
            message.body_text,
            message.raw_sha256,
            message.source_path,
        ],
    )?;

    if changed == 0 {
        return Ok(None);
    }
    Ok(Some(conn.last_insert_rowid()))
}

pub fn insert_attachment(
    conn: &Connection,
    message_id: i64,
    ordinal: usize,
    filename: &str,
    content_type: Option<&str>,
    saved_path: &str,
    size_bytes: usize,
    extracted_text: Option<&str>,
) -> Result<()> {
    conn.execute(
        r#"
        INSERT INTO attachments
            (message_id_fk, ordinal, filename, content_type, saved_path, size_bytes, extracted_text)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        "#,
        params![
            message_id,
            ordinal as i64,
            filename,
            content_type,
            saved_path,
            size_bytes as i64,
            extracted_text,
        ],
    )?;
    Ok(())
}

pub fn list_messages(conn: &Connection, limit: usize, untriaged: bool) -> Result<Vec<MessageRow>> {
    let sql = if untriaged {
        r#"
        SELECT m.id
          FROM messages m
          LEFT JOIN triage t ON t.message_id_fk = m.id
         WHERE t.message_id_fk IS NULL
         ORDER BY COALESCE(m.sent_at, m.imported_at) DESC
         LIMIT ?1
        "#
    } else {
        r#"
        SELECT m.id
          FROM messages m
         ORDER BY COALESCE(m.sent_at, m.imported_at) DESC
         LIMIT ?1
        "#
    };

    let ids = query_ids(conn, sql, limit as i64)?;
    load_messages(conn, ids)
}

pub fn get_message(conn: &Connection, id: i64) -> Result<Option<MessageRow>> {
    let base = conn
        .query_row(
            r#"
            SELECT id, stable_key, message_id, subject, sender_name, sender_address,
                   sent_at, body_text, source_path
              FROM messages
             WHERE id = ?1
            "#,
            [id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                ))
            },
        )
        .optional()?;

    let Some((id, stable_key, message_id, subject, sender_name, sender_address, sent_at, body_text, source_path)) = base else {
        return Ok(None);
    };

    Ok(Some(MessageRow {
        id,
        stable_key,
        message_id,
        subject,
        sender_name,
        sender_address,
        sent_at,
        body_text,
        source_path,
        attachments: attachments_for(conn, id)?,
        triage: triage_for(conn, id)?,
    }))
}

pub fn recent_summaries(conn: &Connection, limit: usize) -> Result<Vec<MessageSummary>> {
    let ids = query_ids(
        conn,
        r#"
        SELECT id
          FROM messages
         ORDER BY COALESCE(sent_at, imported_at) DESC
         LIMIT ?1
        "#,
        limit as i64,
    )?;
    summarize_messages(conn, ids)
}

pub fn search_summaries(conn: &Connection, query: &str, limit: usize) -> Result<Vec<MessageSummary>> {
    let pattern = format!("%{query}%");
    let mut stmt = conn.prepare(
        r#"
        SELECT DISTINCT m.id
          FROM messages m
          LEFT JOIN attachments a ON a.message_id_fk = m.id
         WHERE m.subject LIKE ?1
            OR m.body_text LIKE ?1
            OR COALESCE(m.sender_name, '') LIKE ?1
            OR COALESCE(m.sender_address, '') LIKE ?1
            OR COALESCE(a.filename, '') LIKE ?1
            OR COALESCE(a.extracted_text, '') LIKE ?1
         ORDER BY COALESCE(m.sent_at, m.imported_at) DESC
         LIMIT ?2
        "#,
    )?;
    let ids = stmt
        .query_map(params![pattern, limit as i64], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    summarize_messages(conn, ids)
}

fn query_ids(conn: &Connection, sql: &str, limit: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([limit], |row| row.get::<_, i64>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn load_messages(conn: &Connection, ids: Vec<i64>) -> Result<Vec<MessageRow>> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(message) = get_message(conn, id)? {
            out.push(message);
        }
    }
    Ok(out)
}

fn summarize_messages(conn: &Connection, ids: Vec<i64>) -> Result<Vec<MessageSummary>> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(message) = get_message(conn, id)? {
            out.push(MessageSummary {
                id: message.id,
                subject: message.subject,
                sender_name: message.sender_name,
                sender_address: message.sender_address,
                sent_at: message.sent_at,
                preview: preview(&message.body_text, 700),
                attachment_names: message
                    .attachments
                    .iter()
                    .map(|attachment| attachment.filename.clone())
                    .collect(),
                triage: message.triage,
            });
        }
    }
    Ok(out)
}

fn preview(input: &str, max_chars: usize) -> String {
    let compact = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        compact
    } else {
        compact.chars().take(max_chars).collect::<String>() + "…"
    }
}

fn attachments_for(conn: &Connection, message_id: i64) -> Result<Vec<AttachmentRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT id, filename, content_type, saved_path, size_bytes, extracted_text
          FROM attachments
         WHERE message_id_fk = ?1
         ORDER BY ordinal ASC
        "#,
    )?;
    let rows = stmt.query_map([message_id], |row| {
        Ok(AttachmentRow {
            id: row.get(0)?,
            filename: row.get(1)?,
            content_type: row.get(2)?,
            saved_path: row.get(3)?,
            size_bytes: row.get(4)?,
            extracted_text: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn triage_for(conn: &Connection, message_id: i64) -> Result<Option<TriageRow>> {
    conn.query_row(
        r#"
        SELECT importance, category, summary, requires_action, action, deadline,
               calendar_title, calendar_start, calendar_end, calendar_location, confidence
          FROM triage
         WHERE message_id_fk = ?1
        "#,
        [message_id],
        |row| {
            Ok(TriageRow {
                importance: row.get(0)?,
                category: row.get(1)?,
                summary: row.get(2)?,
                requires_action: row.get::<_, i64>(3)? != 0,
                action: row.get(4)?,
                deadline: row.get(5)?,
                calendar_title: row.get(6)?,
                calendar_start: row.get(7)?,
                calendar_end: row.get(8)?,
                calendar_location: row.get(9)?,
                confidence: row.get(10)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

pub fn save_triage(conn: &Connection, message_id: i64, triage: &TriageRow, raw_json: &str) -> Result<()> {
    conn.execute(
        r#"
        INSERT INTO triage (
            message_id_fk, importance, category, summary, requires_action, action, deadline,
            calendar_title, calendar_start, calendar_end, calendar_location, confidence, raw_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
        ON CONFLICT(message_id_fk) DO UPDATE SET
            importance = excluded.importance,
            category = excluded.category,
            summary = excluded.summary,
            requires_action = excluded.requires_action,
            action = excluded.action,
            deadline = excluded.deadline,
            calendar_title = excluded.calendar_title,
            calendar_start = excluded.calendar_start,
            calendar_end = excluded.calendar_end,
            calendar_location = excluded.calendar_location,
            confidence = excluded.confidence,
            raw_json = excluded.raw_json,
            triaged_at = CURRENT_TIMESTAMP
        "#,
        params![
            message_id,
            triage.importance,
            triage.category,
            triage.summary,
            triage.requires_action as i64,
            triage.action,
            triage.deadline,
            triage.calendar_title,
            triage.calendar_start,
            triage.calendar_end,
            triage.calendar_location,
            triage.confidence,
            raw_json,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_is_compact_and_bounded() {
        assert_eq!(preview("a\n  b\t c", 100), "a b c");
        assert_eq!(preview("abcdef", 3), "abc…");
    }
}
