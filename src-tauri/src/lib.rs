//! NetcodePlus launcher Tauri shell.
//!
//! Wires up plugins (opener, dialog) and registers the
//! [`commands`] module's handlers with the Tauri runtime. All real
//! logic lives in the workspace's `ncp-*` crates; this file just
//! glues them to the webview.

mod commands;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::detect_installs,
            commands::check_install,
            commands::affinity_presets,
            commands::launch_game,
            commands::load_state,
            commands::save_launch_prefs,
            commands::save_ut4stats_link,
            commands::ut4stats_search,
            commands::ut4stats_summary,
            commands::open_external,
            commands::pug_action,
            commands::save_launcher_token,
            commands::engine_config_state,
            commands::openal_status,
            commands::apply_engine_config,
            commands::restore_engine_config,
            commands::launcher_version,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
