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

use std::path::Path;
use storage::Database;
use tauri::{Emitter, Manager};

/// Default projects.yaml shipped with the binary. Written to app_data_dir on
/// first launch if no user override exists, then ProjectResolver loads from
/// that on-disk copy so the user can edit it freely.
const DEFAULT_PROJECTS_YAML: &str = include_str!("../../projects.yaml");

/// Default pricing.yaml shipped with the binary. Same bootstrap pattern.
const DEFAULT_PRICING_YAML: &str = include_str!("../../pricing.yaml");

/// Bootstrap a config file in app_data_dir: if it doesn't exist, write the
/// bundled default. Returns the on-disk path either way. Errors silently fall
/// back to writing nothing — caller decides whether to use empty defaults.
fn bootstrap_config_file(app_data_dir: &Path, filename: &str, default_contents: &str) -> std::path::PathBuf {
    let target = app_data_dir.join(filename);
    if !target.exists() {
        match std::fs::write(&target, default_contents) {
            Ok(_) => eprintln!("[lens] Wrote default {} to {}", filename, target.display()),
            Err(e) => eprintln!("[lens] WARN: could not write default {}: {}", filename, e),
        }
    }
    target
}

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

            // Bootstrap user-editable config files. First launch writes the
            // shipped defaults; subsequent launches read whatever's on disk
            // so user edits persist.
            let projects_yaml_path =
                bootstrap_config_file(&data_dir, "projects.yaml", DEFAULT_PROJECTS_YAML);
            let pricing_yaml_path =
                bootstrap_config_file(&data_dir, "pricing.yaml", DEFAULT_PRICING_YAML);

            let state = ipc::LensState::new(db);
            // Clone the shared Arc<Mutex<Database>> for the background ingestion
            // task to use. Tauri's State<T> uses its own internal Arc; this
            // second clone lets the spawned task hold a parallel reference.
            let db_for_ingestion = state.db.clone();
            app.manage(state);

            // Keep a handle so the spawned task can emit completion events
            // back to the React frontend (which subscribes via @tauri-apps/api/event).
            let app_handle = app.handle().clone();

            // Spawn backfill in a background thread. spawn_blocking is the
            // right call: backfill() is synchronous (locks the Mutex around
            // each file's storage write), so it must NOT run on the tokio
            // single-threaded executor that the UI also uses.
            tauri::async_runtime::spawn_blocking(move || {
                use adapters::claude_code::ClaudeCodeAdapter;
                use ingestion::{IngestionPipeline, PipelineConfig};
                use pricing::PricingTable;
                use project_resolver::ProjectResolver;

                // Load real ProjectResolver + PricingTable from the bootstrapped
                // YAML. Either failure falls back to ::empty() with a loud log line —
                // events still ingest, they just bucket to "Uncategorized" and skip
                // cost computation. Better degraded than missing.
                let project_resolver = match ProjectResolver::load_from_path(&projects_yaml_path)
                {
                    Ok(r) => {
                        eprintln!("[lens] Loaded projects.yaml from {}", projects_yaml_path.display());
                        r
                    }
                    Err(e) => {
                        eprintln!(
                            "[lens] WARN: could not load projects.yaml ({}); cwd → project resolution disabled, all events bucket to Uncategorized",
                            e
                        );
                        ProjectResolver::empty()
                    }
                };
                let pricing = match PricingTable::load_from_path(&pricing_yaml_path) {
                    Ok(p) => {
                        eprintln!("[lens] Loaded pricing.yaml from {}", pricing_yaml_path.display());
                        p
                    }
                    Err(e) => {
                        eprintln!(
                            "[lens] WARN: could not load pricing.yaml ({}); cost computation disabled",
                            e
                        );
                        PricingTable::empty()
                    }
                };

                let adapter = ClaudeCodeAdapter {
                    project_resolver,
                    pricing,
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
                        let duration_s = started.elapsed().as_secs_f32();
                        eprintln!(
                            "[lens] Backfill complete in {:.1}s: {} files scanned ({} skipped active, {} skipped subagent), {} inserted, {} updated, {} unchanged, {} recoverable, {} fatal",
                            duration_s,
                            report.files_scanned,
                            report.files_skipped_active,
                            report.files_skipped_subagent,
                            report.events_inserted,
                            report.events_updated,
                            report.events_unchanged,
                            report.recoverable_issues,
                            report.fatal_issues,
                        );
                        // Notify the React frontend so it can refetch the
                        // timeline + counters without waiting for the next
                        // 2s app-status poll. Payload mirrors BackfillReport.
                        let payload = serde_json::json!({
                            "duration_s": duration_s,
                            "files_scanned": report.files_scanned,
                            "events_inserted": report.events_inserted,
                            "events_updated": report.events_updated,
                            "events_unchanged": report.events_unchanged,
                            "recoverable_issues": report.recoverable_issues,
                            "fatal_issues": report.fatal_issues,
                        });
                        let _ = app_handle.emit("lens:backfill-complete", payload);
                    }
                    Err(e) => {
                        eprintln!("[lens] Backfill failed: {}", e);
                        let _ = app_handle.emit(
                            "lens:backfill-error",
                            serde_json::json!({ "error": e.to_string() }),
                        );
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
