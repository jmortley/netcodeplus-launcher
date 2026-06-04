//! NetcodePlus launcher Tauri shell.
//!
//! Wires up plugins (opener, dialog) and registers the
//! [`commands`] module's handlers with the Tauri runtime. All real
//! logic lives in the workspace's `ncp-*` crates; this file just
//! glues them to the webview.

mod auth;
mod commands;
mod elevated;
mod installer;
mod trust_root;
mod updates;

pub use elevated::run_elevated_install;

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
            commands::game_requires_admin,
            commands::clear_game_requires_admin,
            commands::launch_game_elevated,
            commands::load_state,
            commands::save_launch_prefs,
            commands::save_ut4stats_link,
            commands::ut4stats_search,
            commands::ut4stats_summary,
            commands::ut4stats_trends,
            commands::launcher_news,
            commands::list_servers,
            commands::open_external,
            commands::pug_action,
            commands::pug_status,
            commands::pug_spectate,
            commands::save_launcher_token,
            commands::engine_config_state,
            commands::openal_status,
            commands::ulticross_status,
            commands::launcher_update_housekeeping,
            commands::create_launcher_shortcut,
            commands::delete_old_launcher,
            commands::dismiss_launcher_cleanup,
            commands::apply_engine_config,
            commands::restore_engine_config,
            commands::repair_master_server,
            commands::clear_engine_ini_readonly,
            commands::reveal_netcodeplus_folder,
            commands::reveal_plugins_folder,
            commands::reveal_openal_folder,
            commands::launcher_version,
            installer::game_installer_info,
            installer::default_download_dir,
            installer::download_game_installer,
            installer::install_game,
            installer::cancel_game_download,
            installer::reveal_path,
            auth::ut4_login,
            auth::ut4_auth_status,
            auth::resolve_player_names,
            auth::ut4_logout,
            auth::ut4_prepare_launch,
            updates::fetch_and_verify_manifest,
            updates::compute_plan,
            updates::plugin_status,
            updates::install_plugin,
            updates::scan_strays,
            updates::remove_stray_plugin,
            updates::launcher_update_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
