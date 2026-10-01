mod cloud;
mod cloud_auth;
mod daemon;
mod db;
mod discovery;
mod extract;
mod forward;
mod gmail;
mod importer;
mod mcp;
mod triage;
mod web;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "hiro-maild",
    version,
    about = "Read-only Hiroshima University mail collector with Gmail forwarding and web setup"
)]
struct Cli {
    /// Directory containing hiro-maild.sqlite3 and extracted attachments.
    #[arg(long, global = true, env = "HIRO_MAILD_DATA_DIR")]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Serve the single-owner web UI and read-only Microsoft Graph forwarding worker.
    Web {
        #[arg(long, default_value = "127.0.0.1:8080")]
        bind: String,
    },
    /// Authorize personal Gmail and store OAuth credentials in Mac Keychain. Sends no mail.
    GmailAuth {
        #[arg(long)]
        gmail: String,
        /// Downloaded Google OAuth Desktop app JSON, kept outside the repository.
        #[arg(long)]
        client_json: PathBuf,
    },
    /// Persist an explicit starting position. Does not import or send mail.
    ForwardInit {
        #[arg(long)]
        gmail: String,
        /// Exclude all currently imported messages and messages dated before initialization.
        #[arg(long, group = "start")]
        from_now: bool,
        /// Include original Date >= this RFC3339 instant. Unknown dates are excluded.
        #[arg(long, group = "start")]
        since: Option<String>,
        /// Include import IDs strictly greater than this ID (0 explicitly includes all).
        #[arg(long, group = "start")]
        after_id: Option<i64>,
    },
    /// Forward eligible mail using Gmail API, or inspect targets without any network calls.
    Forward {
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value_t = 20, value_parser = forward_limit)]
        limit: usize,
    },
    /// Show starting position and delivery states, including completed messages.
    ForwardStatus {
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Queue a failed/held delivery for another attempt. Does not send immediately.
    ForwardRetry {
        #[arg(long)]
        id: i64,
        #[arg(long)]
        accept_duplicate_risk: bool,
    },
    /// Inspect the local machine and report what is ready/missing.
    Doctor,

    /// Import messages from a Thunderbird IMAP local store. Never writes to Thunderbird.
    Sync {
        /// Thunderbird account store, e.g. .../Profiles/xxxx.default/ImapMail/outlook.office365.com
        #[arg(long, env = "HIRO_MAILD_THUNDERBIRD_STORE")]
        store: Option<PathBuf>,
    },

    /// Print recently imported messages as JSON Lines.
    List {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Show only messages that do not yet have AI triage data.
        #[arg(long)]
        untriaged: bool,
    },

    /// Serve a read-only MCP endpoint backed by the local SQLite database.
    Serve {
        /// Local address for the MCP Streamable HTTP server.
        #[arg(long, default_value = "127.0.0.1:8000", env = "HIRO_MAILD_MCP_BIND")]
        bind: String,
    },

    /// Periodically import Thunderbird mail and serve the read-only MCP endpoint.
    Daemon {
        /// Thunderbird account store. Auto-detected when omitted.
        #[arg(long, env = "HIRO_MAILD_THUNDERBIRD_STORE")]
        store: Option<PathBuf>,
        /// Local address for the MCP Streamable HTTP server.
        #[arg(long, default_value = "127.0.0.1:8000", env = "HIRO_MAILD_MCP_BIND")]
        bind: String,
        /// Seconds between local Thunderbird imports. Minimum 30 seconds.
        #[arg(long, default_value_t = 300, env = "HIRO_MAILD_SYNC_INTERVAL_SECONDS")]
        interval_seconds: u64,
        /// Also forward eligible mail after every sync; requires forward-init and gmail-auth.
        #[arg(long)]
        forward: bool,
        /// Inspect forwarding candidates each cycle without sending or accessing credentials.
        #[arg(long, requires = "forward")]
        forward_dry_run: bool,
        #[arg(long, default_value_t = 20, value_parser = forward_limit)]
        forward_limit: usize,
    },

    /// Triage untriaged messages with the OpenAI Responses API.
    Triage {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Model ID. Defaults to a cost-efficient current model.
        #[arg(long, default_value = "gpt-5.6-luna", env = "HIRO_MAILD_OPENAI_MODEL")]
        model: String,
        /// Print the request payload without sending it.
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let data_dir = cli.data_dir.unwrap_or_else(discovery::default_data_dir);
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("failed to create data directory: {}", data_dir.display()))?;

    let database_path = data_dir.join("hiro-maild.sqlite3");
    let conn = db::open(&database_path)?;

    match cli.command {
        Command::Web { bind } => {
            drop(conn);
            runtime()?.block_on(web::run(data_dir, bind))?;
        }
        Command::GmailAuth { gmail, client_json } => gmail::authenticate(&gmail, &client_json)?,
        Command::ForwardInit {
            gmail,
            from_now,
            since,
            after_id,
        } => {
            let _lock = forward::lock(&data_dir)?;
            let config = forward::initialize(&conn, &gmail, after_id, since.as_deref(), from_now)?;
            println!("{}", serde_json::to_string(&config)?);
        }
        Command::Forward { dry_run, limit } => forward::run(&conn, &data_dir, limit, dry_run)?,
        Command::ForwardStatus { limit } => forward::print_status(&conn, limit, true)?,
        Command::ForwardRetry {
            id,
            accept_duplicate_risk,
        } => {
            let _lock = forward::lock(&data_dir)?;
            forward::retry(&conn, id, accept_duplicate_risk)?;
            println!("forward id={id} queued; no mail sent by this command");
        }
        Command::Doctor => {
            discovery::run_doctor(&data_dir)?;
        }
        Command::Sync { store } => {
            let store = resolve_store(store)?;
            print_sync(importer::sync_store(
                &conn,
                &store,
                &data_dir.join("attachments"),
            )?);
        }
        Command::List { limit, untriaged } => {
            for row in db::list_messages(&conn, limit, untriaged)? {
                println!("{}", serde_json::to_string(&row)?);
            }
        }
        Command::Serve { bind } => {
            drop(conn);
            runtime()?.block_on(mcp::serve(database_path, bind))?;
        }
        Command::Daemon {
            store,
            bind,
            interval_seconds,
            forward,
            forward_dry_run,
            forward_limit,
        } => {
            if interval_seconds < 30 {
                bail!("--interval-seconds must be at least 30");
            }
            let store = resolve_store(store)?;
            if forward {
                crate::forward::config(&conn)?;
            }
            let attachments_root = data_dir.join("attachments");
            print_sync(importer::sync_store(&conn, &store, &attachments_root)?);
            let forwarding = forward.then_some(daemon::ForwardOptions {
                data_dir: data_dir.clone(),
                dry_run: forward_dry_run,
                limit: forward_limit,
            });
            if let Some(options) = &forwarding {
                if let Err(error) =
                    crate::forward::run(&conn, &options.data_dir, options.limit, options.dry_run)
                {
                    eprintln!("warning: initial forwarding failed: {error:#}");
                }
            }
            drop(conn);
            runtime()?.block_on(daemon::run(
                database_path,
                attachments_root,
                store,
                bind,
                interval_seconds,
                forwarding,
            ))?;
        }
        Command::Triage {
            limit,
            model,
            dry_run,
        } => {
            triage::triage_pending(&conn, limit, &model, dry_run)?;
        }
    }

    Ok(())
}

fn resolve_store(store: Option<PathBuf>) -> Result<PathBuf> {
    match store {
        Some(path) => Ok(path),
        None => discovery::select_hiroshima_store(),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to create Tokio runtime")
}

fn print_sync(stats: importer::SyncStats) {
    println!(
        "sync scanned_files={} parsed_messages={} imported_messages={} skipped_existing={} parse_errors={} attachments_saved={}",
        stats.scanned_files,
        stats.parsed_messages,
        stats.imported_messages,
        stats.skipped_existing,
        stats.parse_errors,
        stats.attachments_saved
    );
}

fn forward_limit(input: &str) -> std::result::Result<usize, String> {
    let limit = input
        .parse::<usize>()
        .map_err(|_| "expected a number from 1 to 200".to_string())?;
    if !(1..=200).contains(&limit) {
        return Err("forwarding limit must be between 1 and 200".into());
    }
    Ok(limit)
}
