mod db;
mod discovery;
mod extract;
mod importer;
mod mcp;
mod triage;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "hiro-maild", version, about = "Read-only Hiroshima University mail collector")]
struct Cli {
    /// Directory containing hiro-maild.sqlite3 and extracted attachments.
    #[arg(long, global = true, env = "HIRO_MAILD_DATA_DIR")]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
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
        Command::Doctor => {
            discovery::run_doctor(&data_dir)?;
        }
        Command::Sync { store } => {
            let store = match store {
                Some(path) => path,
                None => discovery::select_hiroshima_store()?,
            };
            let stats = importer::sync_store(&conn, &store, &data_dir.join("attachments"))?;
            println!(
                "scanned_files={} parsed_messages={} imported_messages={} skipped_existing={} parse_errors={} attachments_saved={}",
                stats.scanned_files,
                stats.parsed_messages,
                stats.imported_messages,
                stats.skipped_existing,
                stats.parse_errors,
                stats.attachments_saved
            );
        }
        Command::List { limit, untriaged } => {
            for row in db::list_messages(&conn, limit, untriaged)? {
                println!("{}", serde_json::to_string(&row)?);
            }
        }
        Command::Serve { bind } => {
            drop(conn);
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .context("failed to create Tokio runtime")?;
            runtime.block_on(mcp::serve(database_path, bind))?;
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
