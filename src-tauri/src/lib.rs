// Lens crate root.

// Lens internal modules. Most are private; specific items are re-exported below
// for use by examples/* and integration tests.
pub mod adapters;
pub mod agent_activity;
pub mod event_id;
pub mod ingestion;
mod ipc;
pub mod pricing;
pub mod project_resolver;
pub mod storage;

use storage::Database;
use tauri::Manager;

/// Greet command — kept from the Tauri scaffold as a Rust↔React IPC bridge
/// smoke test. The placeholder UI calls this to confirm the bridge works
/// before any real Lens data exists. Will be removed once the timeline UI
/// supersedes the placeholder.
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // Open / initialize the Lens SQLite database.
            //
            // V1: per-user file under macOS app data dir
            // (~/Library/Application Support/com.cfelmer.lens/lens.db).
            // Falls back to an in-memory DB if directory creation fails — keeps
            // the app running with degraded persistence rather than panicking
            // at startup. The UI will report the in-memory state via
            // get_app_status so it's not silent.
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("could not resolve app_data_dir: {}", e))?;

            std::fs::create_dir_all(&data_dir).ok();
            let db_path = data_dir.join("lens.db");

            let db = match Database::open_path(&db_path) {
                Ok(db) => {
                    eprintln!("[lens] Opened SQLite database at {}", db_path.display());
                    db
                }
                Err(e) => {
                    eprintln!(
                        "[lens] WARN: could not open {} ({}). Falling back to in-memory store.",
                        db_path.display(),
                        e
                    );
                    Database::open_in_memory().expect("in-memory DB init must succeed")
                }
            };

            let state = ipc::LensState::new(db);
            // Clone the shared Arc<Mutex<Database>> for the background ingestion
            // task to use. Tauri's State<T> uses its own internal Arc; this
            // second clone lets the spawned task hold a parallel reference.
            let db_for_ingestion = state.db.clone();
            app.manage(state);

            // Spawn backfill in a background thread. spawn_blocking is the
            // right call: backfill() is synchronous (locks the Mutex around
            // each file's storage write), so it must NOT run on the tokio
            // single-threaded executor that the UI also uses.
            tauri::async_runtime::spawn_blocking(move || {
                use adapters::claude_code::ClaudeCodeAdapter;
                use ingestion::{IngestionPipeline, PipelineConfig};
                use pricing::PricingTable;
                use project_resolver::ProjectResolver;

                // V1: empty ProjectResolver + PricingTable. The user's
                // projects.yaml/pricing.yaml-based config lands in V1.x.
                // Today all cwds bucket to "Uncategorized" and costs are
                // not computed, but the timeline still renders real events.
                let adapter = ClaudeCodeAdapter {
                    project_resolver: ProjectResolver::empty(),
                    pricing: PricingTable::empty(),
                };
                let pipeline = IngestionPipeline::new(
                    db_for_ingestion,
                    vec![Box::new(adapter)],
                    PipelineConfig::defaults_for_claude_code(),
                );

                eprintln!("[lens] Starting backfill of ~/.claude/projects...");
                let started = std::time::Instant::now();
                match pipeline.backfill() {
                    Ok(report) => {
                        eprintln!(
                            "[lens] Backfill complete in {:.1}s: {} files scanned ({} skipped active, {} skipped subagent), {} inserted, {} updated, {} unchanged, {} recoverable, {} fatal",
                            started.elapsed().as_secs_f32(),
                            report.files_scanned,
                            report.files_skipped_active,
                            report.files_skipped_subagent,
                            report.events_inserted,
                            report.events_updated,
                            report.events_unchanged,
                            report.recoverable_issues,
                            report.fatal_issues,
                        );
                    }
                    Err(e) => {
                        eprintln!("[lens] Backfill failed: {}", e);
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            ipc::get_timeline,
            ipc::get_event_detail,
            ipc::count_events_per_project,
            ipc::count_issues_per_project,
            ipc::recent_issues,
            ipc::get_app_status,
            ipc::insert_demo_event,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
