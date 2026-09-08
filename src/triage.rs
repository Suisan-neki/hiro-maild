use std::env;

use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{db, extract};

const MAX_BODY_CHARS: usize = 60_000;
const MAX_ATTACHMENT_CHARS: usize = 60_000;

#[derive(Debug, Deserialize)]
struct TriagePayload {
    importance: String,
    category: String,
    summary: String,
    requires_action: bool,
    action: Option<String>,
    deadline: Option<String>,
    calendar: CalendarPayload,
    confidence: f64,
}

#[derive(Debug, Deserialize)]
struct CalendarPayload {
    title: Option<String>,
    start: Option<String>,
    end: Option<String>,
    location: Option<String>,
}

pub fn triage_pending(conn: &Connection, limit: usize, model: &str, dry_run: bool) -> Result<()> {
    let messages = db::list_messages(conn, limit, true)?;
    if messages.is_empty() {
        println!("no untriaged messages");
        return Ok(());
    }

    let api_key = if dry_run {
        env::var("OPENAI_API_KEY").unwrap_or_default()
    } else {
        env::var("OPENAI_API_KEY").context("OPENAI_API_KEY is required for triage")?
    };

    let client = Client::builder().build()?;
    for message in messages {
        let request = build_request(&message, model);
        if dry_run {
            println!("{}", serde_json::to_string_pretty(&request)?);
            continue;
        }

        let response = client
            .post("https://api.openai.com/v1/responses")
            .bearer_auth(&api_key)
            .json(&request)
            .send()
            .context("OpenAI Responses API request failed")?;

        let status = response.status();
        let raw_response: Value = response.json().context("invalid OpenAI API JSON response")?;
        if !status.is_success() {
            bail!("OpenAI API returned {status}: {raw_response}");
        }

        let text = response_output_text(&raw_response)
            .context("OpenAI response contained no output text")?;
        let parsed: TriagePayload = serde_json::from_str(&text)
            .with_context(|| format!("invalid structured triage JSON: {text}"))?;

        let row = db::TriageRow {
            importance: parsed.importance,
            category: parsed.category,
            summary: parsed.summary,
            requires_action: parsed.requires_action,
            action: parsed.action,
            deadline: parsed.deadline,
            calendar_title: parsed.calendar.title,
            calendar_start: parsed.calendar.start,
            calendar_end: parsed.calendar.end,
            calendar_location: parsed.calendar.location,
            confidence: parsed.confidence.clamp(0.0, 1.0),
        };
        db::save_triage(conn, message.id, &row, &text)?;
        println!("triaged id={} subject={:?}", message.id, message.subject);
    }

    Ok(())
}

fn build_request(message: &db::MessageRow, model: &str) -> Value {
    let attachments = message
        .attachments
        .iter()
        .map(|attachment| {
            json!({
                "filename": attachment.filename,
                "content_type": attachment.content_type,
                "extracted_text": attachment.extracted_text.as_deref().map(|text| extract::truncate_chars(text, MAX_ATTACHMENT_CHARS)),
            })
        })
        .collect::<Vec<_>>();

    let mail = json!({
        "subject": message.subject,
        "from_name": message.sender_name,
        "from_address": message.sender_address,
        "sent_at": message.sent_at,
        "body": extract::truncate_chars(&message.body_text, MAX_BODY_CHARS),
        "attachments": attachments,
    });

    json!({
        "model": model,
        "store": false,
        "reasoning": { "effort": "low" },
        "instructions": concat!(
            "You triage university email for its recipient. Return Japanese summaries. ",
            "Do not invent deadlines, dates, actions, locations, or event times. Use null when not explicitly supported. ",
            "requires_action is true only when the recipient should do something. ",
            "importance: urgent only for time-critical items with a near deadline or serious consequence; high for materially important academic/admin actions; otherwise normal or low. ",
            "category must be one of academic, admin, research, career, event, finance, account, other. ",
            "Calendar fields are only for concrete scheduled events suitable for a personal calendar. ISO-8601 with offset when present. ",
            "confidence is 0 to 1."
        ),
        "input": [{
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": format!("Triage this email and its extracted attachments:\n{}", serde_json::to_string_pretty(&mail).unwrap())
            }]
        }],
        "text": {
            "format": {
                "type": "json_schema",
                "name": "university_mail_triage",
                "strict": true,
                "schema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "importance": { "type": "string", "enum": ["low", "normal", "high", "urgent"] },
                        "category": { "type": "string", "enum": ["academic", "admin", "research", "career", "event", "finance", "account", "other"] },
                        "summary": { "type": "string" },
                        "requires_action": { "type": "boolean" },
                        "action": { "type": ["string", "null"] },
                        "deadline": { "type": ["string", "null"] },
                        "calendar": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "title": { "type": ["string", "null"] },
                                "start": { "type": ["string", "null"] },
                                "end": { "type": ["string", "null"] },
                                "location": { "type": ["string", "null"] }
                            },
                            "required": ["title", "start", "end", "location"]
                        },
                        "confidence": { "type": "number", "minimum": 0, "maximum": 1 }
                    },
                    "required": ["importance", "category", "summary", "requires_action", "action", "deadline", "calendar", "confidence"]
                }
            }
        }
    })
}

fn response_output_text(response: &Value) -> Option<String> {
    for output in response.get("output")?.as_array()? {
        for content in output.get("content")?.as_array()? {
            if content.get("type")?.as_str()? == "output_text" {
                if let Some(text) = content.get("text").and_then(Value::as_str) {
                    return Some(text.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_output_text() {
        let value = json!({"output":[{"content":[{"type":"output_text","text":"{\"ok\":true}"}]}]});
        assert_eq!(response_output_text(&value).as_deref(), Some("{\"ok\":true}"));
    }
}
