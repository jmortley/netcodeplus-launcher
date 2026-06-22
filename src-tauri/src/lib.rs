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
mod presence;
mod trust_root;
mod updates;

pub use elevated::run_elevated_install;

/// If we were relaunched by the verified self-update
/// ([`crate::updates::download_and_apply_launcher_update`]), the previous
/// launcher passed `--post-update-wait <pid>`. Wait (bounded) for that process
/// to exit BEFORE Tauri is built — otherwise the single-instance plugin would
/// see the still-alive predecessor holding the lock, defer to it, and exit,
/// aborting the handoff. Once the old process is gone the lock is free and we
/// start normally as the sole instance.
///
/// No-op when the flag is absent (a normal launch). The predecessor calls
/// `exit(0)` immediately after spawning us, so this returns in well under a
/// second in practice; the loop cap stops a stuck old process hanging startup.
#[cfg(desktop)]
fn wait_for_predecessor_exit() {
    let args: Vec<String> = std::env::args().collect();
    let Some(pid) = args
        .iter()
        .position(|a| a == "--post-update-wait")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse::<usize>().ok())
    else {
        return;
    };
    use sysinfo::{Pid, System};
    let target = Pid::from(pid);
    let mut sys = System::new();
    for _ in 0..100 {
        sys.refresh_processes();
        if sys.process(target).is_none() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // A self-update relaunch must let its predecessor release the single-instance
    // lock first — wait for it before building anything.
    #[cfg(desktop)]
    wait_for_predecessor_exit();

    let mut builder = tauri::Builder::default();

    // single-instance MUST be registered first. Its `deep-link` feature routes an
    // `ncp://` link opened while we're already running into THIS instance (the URL
    // then arrives via the deep-link plugin's onOpenUrl in the frontend); here we
    // just bring the existing window forward.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            use tauri::Manager;
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }));
    }

    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_deep_link::init())
        // Native desktop notification for the PUG ready-up prompt, so a minimized
        // launcher still alerts the player when their PUG fills (the direct
        // Discord-ping replacement during an outage). See src/main.ts.
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            // Register the `ncp` scheme at runtime — dev needs this; packaged
            // builds also get it from the installer. Non-fatal so a registry
            // hiccup can't block startup.
            #[cfg(any(windows, target_os = "linux"))]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let _ = app.deep_link().register_all();
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::detect_installs,
            commands::check_install,
            commands::affinity_presets,
            commands::launch_game,
            commands::game_requires_admin,
            commands::clear_game_requires_admin,
            commands::launch_game_elevated,
            commands::is_game_running,
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
            commands::pug_ready,
            commands::pug_spectate,
            commands::pug_live,
            commands::pug_queues,
            commands::utpugs_configured,
            commands::utpugs_action,
            commands::utpugs_status,
            commands::utpugs_ready,
            commands::utpugs_spectate,
            commands::utpugs_queues,
            commands::save_launcher_token,
            commands::save_utpugs_token,
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
            presence::set_discord_presence,
            presence::set_discord_presence_enabled,
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
            updates::verify_plugin,
            updates::scan_strays,
            updates::remove_stray_plugin,
            updates::launcher_update_status,
            updates::download_and_apply_launcher_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
