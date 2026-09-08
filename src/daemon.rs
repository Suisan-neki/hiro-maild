use std::{path::PathBuf, time::Duration};

use anyhow::Result;

use crate::{db, importer, mcp};

pub async fn run(
    database_path: PathBuf,
    attachments_root: PathBuf,
    store: PathBuf,
    bind: String,
    interval_seconds: u64,
) -> Result<()> {
    let sync_database = database_path.clone();
    let sync_task = tokio::spawn(async move {
        let interval = Duration::from_secs(interval_seconds);
        loop {
            tokio::time::sleep(interval).await;

            let database_path = sync_database.clone();
            let attachments_root = attachments_root.clone();
            let store = store.clone();
            let outcome = tokio::task::spawn_blocking(move || -> Result<()> {
                let conn = db::open(&database_path)?;
                let stats = importer::sync_store(&conn, &store, &attachments_root)?;
                println!(
                    "sync scanned_files={} parsed_messages={} imported_messages={} skipped_existing={} parse_errors={} attachments_saved={}",
                    stats.scanned_files,
                    stats.parsed_messages,
                    stats.imported_messages,
                    stats.skipped_existing,
                    stats.parse_errors,
                    stats.attachments_saved
                );
                Ok(())
            })
            .await;

            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(error)) => eprintln!("warning: scheduled sync failed: {error:#}"),
                Err(error) => eprintln!("warning: scheduled sync task failed: {error}"),
            }
        }
    });

    let result = mcp::serve(database_path, bind).await;
    sync_task.abort();
    let _ = sync_task.await;
    result
}
