use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::db;

const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 50;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RecentMailArgs {
    /// Maximum number of messages to return (1-50). Defaults to 10.
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchMailArgs {
    /// Text to search across subject, sender, body, attachment names, and extracted attachment text.
    pub query: String,
    /// Maximum number of matches to return (1-50). Defaults to 10.
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetMailArgs {
    /// Local hiro-maild message ID returned by recent_mail or search_mail.
    pub id: i64,
}

#[derive(Clone)]
pub struct MailMcp {
    db_path: Arc<PathBuf>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl MailMcp {
    pub fn new(db_path: Arc<PathBuf>) -> Self {
        Self {
            db_path,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "List recent Hiroshima University emails. Returns metadata, a short body preview, attachment names, and any previously stored triage. This tool is read-only and does not mark mail as read."
    )]
    fn recent_mail(
        &self,
        Parameters(args): Parameters<RecentMailArgs>,
    ) -> Result<CallToolResult, McpError> {
        let limit = bounded_limit(args.limit)?;
        let conn = db::open(&self.db_path).map_err(mcp_internal)?;
        let rows = db::recent_summaries(&conn, limit).map_err(mcp_internal)?;
        json_result(&rows)
    }

    #[tool(
        description = "Search locally synchronized Hiroshima University emails by text in subject, sender, body, attachment name, or extracted attachment text. Returns compact matches only. Read-only."
    )]
    fn search_mail(
        &self,
        Parameters(args): Parameters<SearchMailArgs>,
    ) -> Result<CallToolResult, McpError> {
        let query = args.query.trim();
        if query.is_empty() {
            return Err(McpError::invalid_params("query must be non-empty", None));
        }
        if query.chars().count() > 500 {
            return Err(McpError::invalid_params("query is too long", None));
        }
        let limit = bounded_limit(args.limit)?;
        let conn = db::open(&self.db_path).map_err(mcp_internal)?;
        let rows = db::search_summaries(&conn, query, limit).map_err(mcp_internal)?;
        json_result(&rows)
    }

    #[tool(
        description = "Get one Hiroshima University email by hiro-maild message ID, including full locally parsed body text and extracted text from attachments. Local filesystem paths are intentionally omitted. Read-only."
    )]
    fn get_mail(
        &self,
        Parameters(args): Parameters<GetMailArgs>,
    ) -> Result<CallToolResult, McpError> {
        if args.id <= 0 {
            return Err(McpError::invalid_params("id must be positive", None));
        }
        let conn = db::open(&self.db_path).map_err(mcp_internal)?;
        let Some(message) = db::get_message(&conn, args.id).map_err(mcp_internal)? else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                json!({"found": false, "id": args.id}).to_string(),
            )]));
        };

        let attachments = message
            .attachments
            .iter()
            .map(|attachment| {
                json!({
                    "filename": attachment.filename,
                    "content_type": attachment.content_type,
                    "size_bytes": attachment.size_bytes,
                    "extracted_text": attachment.extracted_text,
                })
            })
            .collect::<Vec<_>>();

        let value = json!({
            "found": true,
            "id": message.id,
            "message_id": message.message_id,
            "subject": message.subject,
            "sender_name": message.sender_name,
            "sender_address": message.sender_address,
            "sent_at": message.sent_at,
            "body_text": message.body_text,
            "attachments": attachments,
            "triage": message.triage,
        });
        json_result(&value)
    }
}

#[tool_handler]
impl ServerHandler for MailMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "Read-only access to the user's locally synchronized Hiroshima University email. Prefer recent_mail/search_mail before get_mail so only relevant full message bodies are retrieved. No tool can send, delete, move, or mark email as read."
                    .to_string(),
            )
    }
}

pub async fn serve(db_path: PathBuf, bind: String) -> Result<()> {
    let cancellation = CancellationToken::new();
    let shared_db = Arc::new(db_path);
    let service = StreamableHttpService::new(
        {
            let shared_db = Arc::clone(&shared_db);
            move || Ok(MailMcp::new(Arc::clone(&shared_db)))
        },
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_cancellation_token(cancellation.child_token()),
    );

    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("failed to bind MCP server to {bind}"))?;
    println!("hiro-maild MCP: http://{bind}/mcp");
    println!("read_only: true");

    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            cancellation.cancel();
        })
        .await
        .context("MCP HTTP server failed")?;
    Ok(())
}

fn bounded_limit(value: Option<usize>) -> Result<usize, McpError> {
    let value = value.unwrap_or(DEFAULT_LIMIT);
    if value == 0 || value > MAX_LIMIT {
        Err(McpError::invalid_params(
            format!("limit must be between 1 and {MAX_LIMIT}"),
            None,
        ))
    } else {
        Ok(value)
    }
}

fn json_result<T: serde::Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string_pretty(value).map_err(mcp_internal)?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

fn mcp_internal(error: impl std::fmt::Display) -> McpError {
    McpError::internal_error(error.to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_is_bounded() {
        assert_eq!(bounded_limit(None).unwrap(), 10);
        assert_eq!(bounded_limit(Some(50)).unwrap(), 50);
        assert!(bounded_limit(Some(0)).is_err());
        assert!(bounded_limit(Some(51)).is_err());
    }
}
