// Lens crate root.

mod adapters;
mod agent_activity;
mod event_id;
mod ipc;
mod pricing;
mod project_resolver;
mod storage;

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

            app.manage(ipc::LensState::new(db));
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
