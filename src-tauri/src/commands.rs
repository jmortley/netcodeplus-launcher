//! Tauri command handlers.
//!
//! Each `#[tauri::command]` is a thin wrapper exposing one function from
//! the workspace crates to the webview. Errors are stringified at the
//! boundary because the webview only handles Display-formatted messages.

use std::path::{Path, PathBuf};

use ncp_host::{
    AffinityPreset, DetectSource, DetectedInstall, LaunchOptions, LaunchProfile, LauncherState,
    Priority,
};
use tauri::Manager;

/// Enumerate all UT4 *play* installs on this machine.
///
/// Desktop-shortcut driven — each shortcut's target is resolved to a
/// shipping client, so editor/source trees (only ever launched via a
/// `UE4Editor.exe` shortcut) are ignored — with a directory-probe
/// fallback when no shortcut resolves. Each install carries how it was
/// found, whether NetcodePlus is installed there, and one launch profile
/// per distinct shortcut variant. Empty array if nothing is found.
#[tauri::command]
pub fn detect_installs() -> Vec<DetectedInstall> {
    ncp_host::detect_installs()
}

/// Validate a user-picked folder (or one of its ancestors) as a UT4
/// install, returning it with `Manual` provenance, NetcodePlus status,
/// and a single `Default` launch profile. `null` if not a UT4 install.
#[tauri::command]
pub fn check_install(path: String) -> Option<DetectedInstall> {
    let mod_paks_dir = ncp_host::default_mod_paks_dir()?;
    let install = ncp_host::check_install(Path::new(&path), mod_paks_dir)?;
    let netcodeplus = ncp_host::netcodeplus_status(&install.root);
    let profiles = vec![LaunchProfile {
        label: "Default".to_string(),
        args: install.launch_args.clone(),
    }];
    Some(DetectedInstall {
        install,
        source: DetectSource::Manual,
        netcodeplus,
        profiles,
    })
}

/// CPU-affinity presets for this machine (all cores / exclude CPU 0 /
/// exclude CPU 0 & 1). The UI shows these plus a custom hex field.
#[tauri::command]
pub fn affinity_presets() -> Vec<AffinityPreset> {
    ncp_host::affinity_presets()
}

/// Launch the game with the chosen executable, launch args, priority, and
/// optional CPU affinity (hex mask string; empty/None = all cores).
#[tauri::command]
pub fn launch_game(
    app: tauri::AppHandle,
    executable: String,
    args: Vec<String>,
    priority: String,
    affinity_mask_hex: Option<String>,
    window_action: String,
) -> Result<(), String> {
    let affinity_mask = match affinity_mask_hex {
        Some(s) => {
            ncp_host::parse_mask_hex(&s).map_err(|e| format!("invalid affinity mask: {e}"))?
        }
        None => None,
    };
    let opts = LaunchOptions {
        priority: if priority.eq_ignore_ascii_case("high") {
            Priority::High
        } else {
            Priority::Normal
        },
        affinity_mask,
    };
    ncp_host::launch(Path::new(&executable), &args, &opts).map_err(|e| e.to_string())?;

    // Game launched — apply the user's window preference. Best-effort: a window
    // action must never fail an otherwise-successful launch. The game is spawned
    // detached, so exiting the launcher does not kill it.
    match window_action.as_str() {
        "close" => app.exit(0),
        "minimize" => {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.minimize();
            }
        }
        _ => {} // "none" / unknown — leave the launcher open.
    }
    Ok(())
}

/// Path to the persistent launcher state file in the per-app config dir.
pub(crate) fn state_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("state.json"))
}

/// Load persisted launcher state (defaults on first run).
#[tauri::command]
pub fn load_state(app: tauri::AppHandle) -> Result<LauncherState, String> {
    let path = state_path(&app)?;
    ncp_host::state::read(&path)
        .map(Option::unwrap_or_default)
        .map_err(|e| e.to_string())
}

/// Persist the user's launch preferences (chosen install, profile,
/// priority, affinity) so they are restored on the next run.
#[tauri::command]
pub fn save_launch_prefs(
    app: tauri::AppHandle,
    install_path: Option<String>,
    profile_label: Option<String>,
    priority: String,
    affinity_mask_hex: Option<String>,
    window_action: String,
) -> Result<(), String> {
    let path = state_path(&app)?;
    let mut state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    state.install_path = install_path.map(PathBuf::from);
    state.launch_profile_label = profile_label;
    state.launch_window_action = match window_action.as_str() {
        "close" => "close".to_string(),
        "none" => "none".to_string(),
        _ => "minimize".to_string(),
    };
    state.launch_priority = if priority.eq_ignore_ascii_case("high") {
        Priority::High
    } else {
        Priority::Normal
    };
    // Validate the mask parses, but persist the (trimmed) hex string;
    // empty/None means "all cores".
    state.affinity_mask_hex = match affinity_mask_hex {
        Some(s) if !s.trim().is_empty() => {
            ncp_host::parse_mask_hex(&s).map_err(|e| format!("invalid affinity mask: {e}"))?;
            Some(s.trim().to_string())
        }
        _ => None,
    };
    ncp_host::state::write(&path, &state).map_err(|e| e.to_string())
}

/// Save (or clear, by passing null) the linked ut4stats.com player.
#[tauri::command]
pub fn save_ut4stats_link(
    app: tauri::AppHandle,
    playerid: Option<String>,
    playername: Option<String>,
) -> Result<(), String> {
    let path = state_path(&app)?;
    let mut state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    state.ut4stats_playerid = playerid.filter(|s| !s.is_empty());
    state.ut4stats_playername = playername.filter(|s| !s.is_empty());
    ncp_host::state::write(&path, &state).map_err(|e| e.to_string())
}

/// Save (or clear, by passing null) the per-user PUG launcher token.
#[tauri::command]
pub fn save_launcher_token(app: tauri::AppHandle, token: Option<String>) -> Result<(), String> {
    let path = state_path(&app)?;
    let mut state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    state.launcher_token = token
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    ncp_host::state::write(&path, &state).map_err(|e| e.to_string())
}

const UT4STATS_BASE: &str = "https://ut4stats.com";
const UT4STATS_MAX: u64 = 256 * 1024;

/// Percent-encode a query-string value (keeps the RFC 3986 unreserved set).
fn encode_q(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Search ut4stats.com players by name via the site's public
/// `/api/player_search/` endpoint. Returns the raw JSON (`[{id, name}, …]`).
#[tauri::command]
pub async fn ut4stats_search(query: String) -> Result<String, String> {
    if query.trim().len() < 2 {
        return Ok("[]".to_string());
    }
    let url = format!(
        "{UT4STATS_BASE}/api/player_search/?q={}",
        encode_q(query.trim())
    );
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    ncp_net::fetch_text(&client, &url, UT4STATS_MAX)
        .await
        .map_err(|e| e.to_string())
}

/// Fetch a player's stats summary from ut4stats.com's public
/// `/api/player_summary/<id>/` endpoint. Returns the raw JSON.
#[tauri::command]
pub async fn ut4stats_summary(playerid: String) -> Result<String, String> {
    let url = format!("{UT4STATS_BASE}/api/player_summary/{playerid}/");
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    ncp_net::fetch_text(&client, &url, UT4STATS_MAX)
        .await
        .map_err(|e| e.to_string())
}

/// Fetch the launcher news feed from ut4stats.com's public
/// `/api/launcher_news/` endpoint (admin-managed announcements). Raw JSON.
#[tauri::command]
pub async fn launcher_news() -> Result<String, String> {
    let url = format!("{UT4STATS_BASE}/api/launcher_news/");
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    ncp_net::fetch_text(&client, &url, UT4STATS_MAX)
        .await
        .map_err(|e| e.to_string())
}

/// UT4 master-server game-server list (the same list the in-game browser shows).
const MASTER_SERVERS_URL: &str =
    "https://master-ut4.timiimit.com/ut/api/matchmaking/session/matchMakingRequest";
/// A few-dozen-server list is tens of KB; cap generously.
const SERVER_LIST_MAX: u64 = 4 * 1024 * 1024;

/// Fetch the live game-server list from timiimit's master server. The listing
/// endpoint is anonymous (no login needed to browse; auth is only required to
/// *join*). Returns the raw `GameServer[]` JSON for the UI to render.
#[tauri::command]
pub async fn list_servers() -> Result<String, String> {
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    ncp_net::fetch_text(&client, MASTER_SERVERS_URL, SERVER_LIST_MAX)
        .await
        .map_err(|e| e.to_string())
}

/// Open an external HTTPS URL in the user's default handler (browser /
/// Discord app). Used for the community Discord invite links. Routed
/// through `tauri-plugin-opener`, which hands the URL to the OS default
/// handler without going through a shell — so there is no command-injection
/// surface. We still require HTTPS as defence in depth, since the value
/// originates in the webview.
#[tauri::command]
pub fn open_external(app: tauri::AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    if !url.starts_with("https://") {
        return Err("refused to open a non-https URL".to_string());
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

/// UT4IGBot launcher PUG endpoint (HTTPS via the ut4stats.com Apache
/// reverse-proxy to the bot's FastAPI app; the proxied path is TLS-terminated).
const BOT_PUG_URL: &str = "https://ut4stats.com/launcher/pug_action";

/// Send a PUG queue action (`joinpug` / `leavepug` / `listpug`) to the bot,
/// authenticated by the player's per-user `token` (issued by the bot's
/// `/launchertoken` command). The bot resolves the player from the token, so
/// the launcher sends NO `ut4_id` -- a token only ever queues its owner.
/// Returns the bot's status message JSON for the UI to display.
#[tauri::command]
pub async fn pug_action(action: String, token: String) -> Result<String, String> {
    if token.trim().is_empty() {
        return Err(
            "Set your launcher token first (run /launchertoken in the UTPugs Discord).".into(),
        );
    }
    let body = serde_json::json!({ "action": action, "mode": "ictf" }).to_string();
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    ncp_net::post_json(
        &client,
        BOT_PUG_URL,
        &[("launcher-token", token.trim())],
        body,
        64 * 1024,
    )
    .await
    .map_err(|e| e.to_string())
}

/// UT4IGBot launcher PUG status endpoint (HTTPS via the Apache reverse-proxy).
const BOT_STATUS_URL: &str = "https://ut4stats.com/launcher/pug_status";

/// Poll the player's PUG status (queued / live + the live server's connect
/// info), authenticated by the per-user launcher `token`. Returns the bot's
/// status JSON so the UI can show a one-click CONNECT when the PUG starts.
#[tauri::command]
pub async fn pug_status(token: String) -> Result<String, String> {
    if token.trim().is_empty() {
        return Err("no launcher token set".into());
    }
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    ncp_net::post_json(
        &client,
        BOT_STATUS_URL,
        &[("launcher-token", token.trim())],
        "{}".to_string(),
        64 * 1024,
    )
    .await
    .map_err(|e| e.to_string())
}

/// UT4IGBot launcher spectate endpoint (HTTPS via the Apache reverse-proxy).
const BOT_SPECTATE_URL: &str = "https://ut4stats.com/launcher/spectate";

/// Ask the bot for the current live PUG's server so the launcher can spectate
/// it, authenticated by the per-user launcher `token`. Returns the bot's JSON
/// (`{state:"live", server, password}` or `{state:"none"}`); the UI then
/// connects with `?SpectatorOnly=1`.
#[tauri::command]
pub async fn pug_spectate(token: String) -> Result<String, String> {
    if token.trim().is_empty() {
        return Err("no launcher token set".into());
    }
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    ncp_net::post_json(
        &client,
        BOT_SPECTATE_URL,
        &[("launcher-token", token.trim())],
        "{}".to_string(),
        64 * 1024,
    )
    .await
    .map_err(|e| e.to_string())
}

/// Launcher version (from `Cargo.toml`). Surfaced in the UI for bug reports.
#[tauri::command]
pub fn launcher_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Per-user Engine.ini path, or an error if Documents can't be located.
fn engine_ini() -> Result<PathBuf, String> {
    ncp_host::config::engine_ini_path()
        .ok_or_else(|| "could not locate your Documents folder".to_string())
}

/// Read the player's current competitive-config state: whether the ini
/// exists, whether a restore point exists, and the current editable values.
#[tauri::command]
pub fn engine_config_state() -> Result<ncp_host::config::ConfigState, String> {
    Ok(ncp_host::config::read_state(&engine_ini()?))
}

/// Whether UT4-OpenAL is installed in `root` — gates the `[Audio]` OpenAL
/// override so players without it don't get broken audio.
#[tauri::command]
pub fn openal_status(root: String) -> bool {
    ncp_host::config::openal_installed(Path::new(&root))
}

/// Whether the UltiCross crosshair plugin is installed in `root`. Read-only;
/// drives an Advanced-tab recommendation for players who don't have it. The
/// launcher never installs or modifies UltiCross.
#[tauri::command]
pub fn ulticross_status(root: String) -> bool {
    ncp_host::ulticross_installed(Path::new(&root))
}

// ===================================================================
// Post-update housekeeping — after the notify-only update flow, help the
// user delete the outdated launcher and create a fresh desktop shortcut.
// ===================================================================

/// What the post-update housekeeping prompt should show.
#[derive(Debug, serde::Serialize)]
pub struct HousekeepingResult {
    /// Path of the previous, now-outdated launcher exe the user can remove, if
    /// one was detected and still exists. `None` = nothing to clean up.
    pub old_launcher_path: Option<String>,
    /// This build's version, for the prompt copy.
    pub current_version: String,
}

/// Record the running launcher's path + version, detect a post-update move, and
/// report whether an outdated previous launcher is around to clean up.
///
/// Run once on startup. When a build with a HIGHER version than the recorded
/// one starts from a DIFFERENT path, the recorded path is the outdated copy: it
/// is stashed in `pending_old_launcher_path` so [`delete_old_launcher`] removes
/// exactly that (never a webview-supplied path). A pending entry whose file has
/// since gone is cleared. The running path/version are always recorded so the
/// next update is detectable and the prompt doesn't re-fire for this build.
#[tauri::command]
pub fn launcher_update_housekeeping(app: tauri::AppHandle) -> Result<HousekeepingResult, String> {
    let current_path = std::env::current_exe().map_err(|e| e.to_string())?;
    let current_version = env!("CARGO_PKG_VERSION");
    let current_semver = semver::Version::parse(current_version).map_err(|e| e.to_string())?;

    let path = state_path(&app)?;
    let mut state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    // Detect a genuine upgrade that started from a different location.
    if let Some(old) = ncp_host::detect_outdated_launcher(
        &current_path,
        &current_semver,
        state.installed_launcher_path.as_deref(),
        state.installed_launcher_version.as_deref(),
    ) {
        if old.is_file() {
            state.pending_old_launcher_path = Some(old.to_string_lossy().into_owned());
        }
    }

    // A previously-pending old launcher that has since been removed is resolved.
    if let Some(p) = &state.pending_old_launcher_path {
        if !Path::new(p).is_file() {
            state.pending_old_launcher_path = None;
        }
    }

    // Always record where/which build is running now.
    state.installed_launcher_path = Some(current_path.to_string_lossy().into_owned());
    state.installed_launcher_version = Some(current_version.to_string());
    ncp_host::state::write(&path, &state).map_err(|e| e.to_string())?;

    Ok(HousekeepingResult {
        old_launcher_path: state.pending_old_launcher_path,
        current_version: current_version.to_string(),
    })
}

/// Create a Desktop shortcut ("UT4 Community Launcher.lnk") pointing at the
/// running launcher exe. Returns the shortcut path on success.
#[tauri::command]
pub fn create_launcher_shortcut() -> Result<String, String> {
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let lnk = ncp_host::create_desktop_shortcut(&current, "UT4 Community Launcher")
        .map_err(|e| e.to_string())?;
    Ok(lnk.to_string_lossy().into_owned())
}

/// Delete the previous, now-outdated launcher exe recorded in
/// `pending_old_launcher_path`, then clear that record.
///
/// Only ever deletes the backend-recorded path (never a value from the
/// webview), and only when it is a regular `.exe` file that is not the launcher
/// currently running — so a tampered state file can't turn this into an
/// arbitrary-file delete.
#[tauri::command]
pub fn delete_old_launcher(app: tauri::AppHandle) -> Result<(), String> {
    let path = state_path(&app)?;
    let mut state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let old = state
        .pending_old_launcher_path
        .clone()
        .ok_or("there is no previous launcher recorded to remove")?;
    let old_path = PathBuf::from(&old);

    // Already gone — treat as resolved.
    if !old_path.is_file() {
        state.pending_old_launcher_path = None;
        ncp_host::state::write(&path, &state).map_err(|e| e.to_string())?;
        return Ok(());
    }
    // Safety: must be a regular .exe, and not the running launcher.
    let is_exe = old_path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"));
    if !is_exe {
        return Err("refused to remove a path that is not an .exe".into());
    }
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let same_as_running = match (
        std::fs::canonicalize(&old_path),
        std::fs::canonicalize(&current),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => old_path == current,
    };
    if same_as_running {
        return Err("refused to remove the launcher that is currently running".into());
    }

    std::fs::remove_file(&old_path).map_err(|e| e.to_string())?;
    state.pending_old_launcher_path = None;
    ncp_host::state::write(&path, &state).map_err(|e| e.to_string())?;
    Ok(())
}

/// Dismiss the post-update cleanup prompt without deleting anything (clears the
/// pending old-launcher record).
#[tauri::command]
pub fn dismiss_launcher_cleanup(app: tauri::AppHandle) -> Result<(), String> {
    let path = state_path(&app)?;
    let mut state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    state.pending_old_launcher_path = None;
    ncp_host::state::write(&path, &state).map_err(|e| e.to_string())?;
    Ok(())
}

/// Apply the competitive Engine.ini baseline plus the editable knobs,
/// merging into the existing ini (backing it up first).
#[tauri::command]
pub fn apply_engine_config(
    frame_rate_cap: f64,
    smooth_frame_rate: bool,
    display_gamma: f64,
    allow_async_loading: bool,
    set_openal_audio: bool,
) -> Result<(), String> {
    let tweaks = ncp_host::config::EngineTweaks {
        frame_rate_cap,
        smooth_frame_rate,
        display_gamma,
        allow_async_loading,
    };
    ncp_host::config::apply(&engine_ini()?, &tweaks, set_openal_audio).map_err(|e| e.to_string())
}

/// Restore Engine.ini from the launcher's `.ncpbak` backup.
#[tauri::command]
pub fn restore_engine_config() -> Result<(), String> {
    ncp_host::config::restore(&engine_ini()?).map_err(|e| e.to_string())
}

/// Verify and, if needed, repair the `[OnlineSubsystemMcp.*]` master-server
/// sections that a UT4 bug sometimes wipes. Returns whether a repair ran.
#[tauri::command]
pub fn repair_master_server() -> Result<bool, String> {
    ncp_host::config::repair_master_server(&engine_ini()?).map_err(|e| e.to_string())
}
