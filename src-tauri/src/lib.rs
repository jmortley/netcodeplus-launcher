//! NetcodePlus launcher Tauri shell.
//!
//! Wires up plugins (opener, dialog) and registers the
//! [`commands`] module's handlers with the Tauri runtime. All real
//! logic lives in the workspace's `ncp-*` crates; this file just
//! glues them to the webview.

mod auth;
mod commands;
mod editor_commands;
mod elevated;
mod gaming_mode;
mod installer;
mod presence;
mod trust_root;
mod updates;
mod webview2;

pub use elevated::{run_elevated_install, run_elevated_install_openal};

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

/// Sweep in-place self-update leftovers beside the running launcher binary:
/// `<binary>.old` (the swapped-out previous build) and the `.update` /
/// `.update.part` staging files an interrupted apply can leave. Best-effort;
/// runs here in `run()` — not in a frontend-invoked command — so a stale or
/// hostile webview can't keep a stale runnable binary around by simply never
/// asking.
///
/// The binary is `$APPIMAGE` on Linux (the real on-disk path, not the FUSE
/// mount) and `current_exe()` on Windows. On Windows the swapped-out `.old`
/// exe may still be image-locked immediately after the handoff; if a plain
/// delete fails, fall back to a reboot-time delete so it can't linger.
#[cfg(desktop)]
fn sweep_update_leftovers() {
    #[cfg(not(windows))]
    let binary = ncp_host::linux::appimage_path(std::env::var("APPIMAGE").ok().as_deref());
    #[cfg(windows)]
    let binary = std::env::current_exe().ok();
    let Some(binary) = binary else {
        return;
    };
    let (update, old) = ncp_host::swap_paths(&binary);
    let part = std::path::PathBuf::from(format!("{}.part", update.to_string_lossy()));
    for stale in [&old, &update, &part] {
        if stale.is_file() && std::fs::remove_file(stale).is_err() {
            // Only `.old` can be persistently image-locked (the just-exited
            // predecessor's exe not yet released) — queue THAT for a reboot-time
            // delete so it can't linger as a stale runnable binary. A failed
            // delete of `.update`/`.update.part` is transient (e.g. an AV
            // scan); leave those for the next start.
            #[cfg(windows)]
            if **stale == old {
                let _ = ncp_host::schedule_delete_on_reboot(stale);
            }
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // A `--gaming-mode-watchdog` invocation is a headless helper process, not a
    // launcher start — it runs its watch loop and exits here, before Tauri (and
    // its single-instance plugin, which would otherwise defer to the running
    // launcher and kill the helper) is ever touched. No-op off Linux.
    gaming_mode::handle_watchdog_flag();

    // WebView2 is what renders our entire UI on Windows. If the runtime is missing
    // (debloated images strip it), Tauri would fail to create the window and exit
    // silently with no error — show a native dialog with the download link instead.
    // No-op off Windows; must run before we touch Tauri.
    webview2::ensure_webview2_runtime();

    // Windows taskbar identity: we deliberately do NOT set an explicit
    // AppUserModelID. The in-place self-update keeps the exe at the SAME path, so
    // Windows' implicit path-derived AUMID is already stable across updates and
    // pinned taskbar icons keep grouping on their own. 1.6.2 added an explicit
    // AUMID here and it ORPHANED every existing implicit-AUMID pin (the Microsoft-
    // documented duplicate-icon failure mode — SetCurrentProcessExplicitAppUserModelID
    // without also stamping the pinned .lnk's System.AppUserModel.ID); reverted in
    // 1.6.3. If an explicit AUMID is ever genuinely needed (a launcher/host process
    // split, or a movable install path), adopt it PROPERLY: stamp the identical
    // System.AppUserModel.ID onto the shortcut at install/first-run and accept a
    // one-time re-pin — setting it only in-process is worse than not setting it.

    // A self-update relaunch must let its predecessor release the single-instance
    // lock first — wait for it before building anything.
    #[cfg(desktop)]
    wait_for_predecessor_exit();

    // Then tidy that predecessor's swapped-out build (and any stale staging
    // files) — after the wait, so the handoff has fully settled.
    #[cfg(desktop)]
    sweep_update_leftovers();

    // Linux: disable WebKitGTK's DMABUF renderer by default (it white-screens on
    // some AMD/Wayland stacks) unless the user set the env var or opted into GPU
    // rendering in Settings. Must run before the webview forks; no-op off Linux.
    ncp_host::apply_webview_dmabuf_default("org.netcodeplus.launcher");

    // Linux: if a crashed gaming-mode session left the dock disabled (its marker
    // file survives), restore it — the watchdog normally does this on game exit.
    gaming_mode::startup_recovery();

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
            commands::save_linux_launch,
            commands::save_linux_gpu_accel,
            commands::save_linux_gaming_mode,
            commands::list_wine_runners,
            commands::resolve_linux_launch,
            installer::game_installer_integrity,
            installer::verify_game_download,
            commands::save_ut4stats_link,
            commands::ut4stats_search,
            commands::ut4stats_summary,
            commands::ut4stats_trends,
            commands::launcher_news,
            commands::list_servers,
            commands::hub_pak_versions,
            commands::open_external,
            commands::reveal_in_folder,
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
            commands::unrealpugs_configured,
            commands::unrealpugs_action,
            commands::unrealpugs_status,
            commands::save_launcher_token,
            commands::save_utpugs_token,
            commands::save_unrealpugs_token,
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
            commands::mod_preset_list,
            commands::mod_config_state,
            commands::apply_mod_preset,
            commands::restore_mod_config,
            commands::onboarding_status,
            commands::mark_features_seen,
            commands::complete_onboarding,
            commands::reset_onboarding,
            commands::reveal_netcodeplus_folder,
            commands::reveal_plugins_folder,
            commands::reveal_openal_folder,
            commands::launcher_version,
            commands::platform_info,
            presence::set_discord_presence,
            presence::set_discord_presence_enabled,
            installer::game_installer_info,
            installer::default_download_dir,
            installer::download_game_installer,
            installer::install_game,
            installer::cancel_game_download,
            installer::reveal_path,
            installer::editor_installer_info,
            installer::download_editor,
            installer::install_editor,
            installer::cancel_editor_download,
            installer::launch_editor,
            editor_commands::list_editor_installs,
            editor_commands::add_editor_install,
            editor_commands::remove_editor_install,
            editor_commands::set_editor_label,
            editor_commands::launch_editor_install,
            editor_commands::set_build_tree,
            editor_commands::get_build_tree,
            editor_commands::editor_plugin_status,
            editor_commands::install_editor_plugins,
            editor_commands::sideload_editor_plugin,
            installer::openal_info,
            installer::install_openal,
            auth::ut4_login,
            auth::ut4_auth_status,
            auth::resolve_player_names,
            auth::ut4_logout,
            auth::ut4_prepare_launch,
            updates::fetch_and_verify_manifest,
            updates::compute_plan,
            updates::pak_status,
            updates::install_paks,
            updates::remove_installed_pak,
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
