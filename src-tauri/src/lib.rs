//! NetcodePlus launcher Tauri shell.
//!
//! Wires up plugins (opener, dialog) and registers the
//! [`commands`] module's handlers with the Tauri runtime. All real
//! logic lives in the workspace's `ncp-*` crates; this file just
//! glues them to the webview.

mod auth;
mod commands;
mod trust_root;

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
            commands::launcher_news,
            commands::list_servers,
            commands::open_external,
            commands::pug_action,
            commands::pug_status,
            commands::pug_spectate,
            commands::save_launcher_token,
            commands::engine_config_state,
            commands::openal_status,
            commands::apply_engine_config,
            commands::restore_engine_config,
            commands::repair_master_server,
            commands::launcher_version,
            auth::ut4_login,
            auth::ut4_auth_status,
            auth::ut4_logout,
            auth::ut4_prepare_launch,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
