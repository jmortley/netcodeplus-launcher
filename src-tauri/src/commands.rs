//! Tauri command handlers.
//!
//! Each `#[tauri::command]` is a thin wrapper exposing one function from
//! the workspace crates to the webview. Errors are stringified at the
//! boundary because the webview only handles Display-formatted messages.

use std::path::{Path, PathBuf};

use ncp_host::{
    AffinityPreset, DetectSource, DetectedInstall, LaunchOptions, LaunchProfile, LauncherState,
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
/// Dev/QA affordance: when `NCP_SIMULATE_NO_INSTALL=1` (or `true`) is set in the
/// environment, detection reports nothing and folder-validation refuses — so a
/// developer can preview the brand-new-user experience (the Linux "Getting UT4"
/// panel, the onboarding, etc.) on a machine that actually has UT4 installed.
/// Purely an env toggle; ships inert (off unless explicitly set).
fn simulate_no_install() -> bool {
    std::env::var("NCP_SIMULATE_NO_INSTALL")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[tauri::command]
pub fn detect_installs() -> Vec<DetectedInstall> {
    if simulate_no_install() {
        return Vec::new();
    }
    ncp_host::detect_installs()
}

/// Validate a user-picked folder (or one of its ancestors) as a UT4
/// install, returning it with `Manual` provenance, NetcodePlus status,
/// and a single `Default` launch profile. `null` if not a UT4 install.
#[tauri::command]
pub fn check_install(path: String) -> Option<DetectedInstall> {
    if simulate_no_install() {
        return None;
    }
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
    env: Option<std::collections::HashMap<String, String>>,
) -> Result<(), String> {
    let affinity_mask = match affinity_mask_hex {
        Some(s) => {
            ncp_host::parse_mask_hex(&s).map_err(|e| format!("invalid affinity mask: {e}"))?
        }
        None => None,
    };
    let opts = LaunchOptions {
        priority: ncp_host::priority_from_label(&priority),
        affinity_mask,
    };
    // Linux only: an explicit Wine/Proton override (prefix + runner) the user set
    // in Settings takes precedence over Lutris auto-detection. `None` on Windows
    // and for the auto-detect happy path.
    let linux_launch = ncp_host::state::read(&state_path(&app)?)
        .ok()
        .flatten()
        .and_then(|s| s.linux_launch);
    // Extra environment for the child. Carries the login credential when the
    // installed plugin picks it up from there instead of the command line —
    // the command line is logged and put in crash reports, the environment is not.
    let env: Vec<(String, String)> = env.unwrap_or_default().into_iter().collect();
    ncp_host::launch(
        Path::new(&executable),
        &args,
        &opts,
        linux_launch.as_ref(),
        &env,
    )
    .map_err(|e| e.to_string())?;

    // Linux gaming mode (opt-in): spawn the detached watchdog that keeps the
    // game window fullscreen+focused and hides the GNOME dock while playing.
    // BEFORE the window action below — "close" exits this process, and the
    // watchdog must already be spawned (it survives the launcher's exit).
    // Best-effort and a no-op off Linux: must never fail a successful launch.
    if let Ok(path) = state_path(&app) {
        crate::gaming_mode::engage_after_launch(&path);
    }

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

/// Whether the game exe is flagged "Run as administrator" (the per-user compat
/// layer), which blocks the launcher's normal launch with os error 740.
/// Read-only — drives the up-front warning on the Launch tab.
#[tauri::command]
pub fn game_requires_admin(executable: String) -> bool {
    ncp_host::compat::requires_admin(Path::new(&executable))
}

/// Clear the "Run as administrator" compat flag on the game exe so it launches
/// normally — the recommended fix for the os-740 case. HKCU only, no elevation.
#[tauri::command]
pub fn clear_game_requires_admin(executable: String) -> Result<(), String> {
    ncp_host::compat::clear_requires_admin(Path::new(&executable)).map_err(|e| e.to_string())
}

/// Per-target outcome of [`repair_client_caches`] — human-ready strings the UI
/// prints verbatim ("cleared" / "already clean" / "failed: …").
#[derive(Debug, serde::Serialize)]
pub struct CacheRepairOutcome {
    /// The per-user CEF `webcache` under Documents.
    pub webcache: String,
    /// The selected install's `PersistentDownloadDir/EMS` cache.
    pub ems: String,
}

/// The "delete webcache" folklore crash fix as a button: clear the two
/// regenerable client caches (see `ncp_host::repair`). Per-target failures are
/// reported in the outcome rather than failing the whole command, so one locked
/// file still tells the player what did and didn't clear.
///
/// # Errors
/// Only when UT4 is running — the caches are open then, and a partial delete of
/// a live cache is exactly the corruption this button exists to fix.
#[tauri::command]
pub fn repair_client_caches(root: Option<String>) -> Result<CacheRepairOutcome, String> {
    if shipping_client_running() {
        return Err("Close Unreal Tournament, then try again.".into());
    }
    fn outcome(result: std::io::Result<bool>) -> String {
        match result {
            Ok(true) => "cleared".into(),
            Ok(false) => "already clean".into(),
            Err(e) => format!("failed: {e}"),
        }
    }
    let webcache = match ncp_host::webcache_dir() {
        Some(dir) => outcome(ncp_host::clear_cache_dir(&dir)),
        None => "failed: no Documents folder on this system".into(),
    };
    let ems = match root.as_deref() {
        Some(r) if !r.is_empty() => {
            outcome(ncp_host::clear_cache_dir(&ncp_host::ems_dir(Path::new(r))))
        }
        _ => "skipped — no install selected".into(),
    };
    Ok(CacheRepairOutcome { webcache, ems })
}

/// Launch the game ELEVATED (one UAC prompt) — an escape hatch for setups that
/// insist on it. NOT recommended: it runs the game as admin, and because it goes
/// through the elevated-launch primitive the CPU priority/affinity knobs do not
/// apply. Fire-and-forget: that primitive waits on the child, so it runs on a
/// detached thread and the command returns once the UAC prompt is handled.
///
/// Defense-in-depth: the elevated target is resolved from the install `root`
/// (re-validated here via [`ncp_host::check_install`]), NOT from a frontend-
/// supplied exe path. So even an injected IPC call can only ever elevate a
/// genuine UT4 shipping client under a real install — never an arbitrary exe.
/// (The CSP `script-src 'self'` + `withGlobalTauri:false` already keep injected
/// markup from reaching `invoke` at all; this closes the primitive regardless.)
#[tauri::command]
pub fn launch_game_elevated(
    app: tauri::AppHandle,
    root: String,
    args: Vec<String>,
    window_action: String,
) -> Result<(), String> {
    let mod_paks_dir =
        ncp_host::default_mod_paks_dir().ok_or("could not locate your mod paks directory")?;
    let install = ncp_host::check_install(Path::new(&root), mod_paks_dir)
        .ok_or("that isn't a UT4 install — can't elevate-launch it")?;
    let exe = install.executable;
    if !exe.is_file() {
        return Err("the game executable was not found".into());
    }
    std::thread::spawn(move || {
        let _ = ncp_host::run_elevated(&exe, &args);
    });
    match window_action.as_str() {
        "close" => app.exit(0),
        "minimize" => {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.minimize();
            }
        }
        _ => {}
    }
    Ok(())
}

/// Whether a process whose image name matches the given game executable is
/// currently running. The connect flow uses this to offer the in-game `open`
/// console command instead of spawning a second UT4 when one is already open.
///
/// Windows reports process names *without* the `.exe` suffix (and sysinfo
/// follows), while the install path carries it — so both ends are lowercased
/// and stripped of a trailing `.exe` before comparing.
#[tauri::command]
pub fn is_game_running(executable: String) -> bool {
    let Some(name) = Path::new(&executable).file_name() else {
        return false;
    };
    let target = name.to_string_lossy().to_ascii_lowercase();
    let target = target.trim_end_matches(".exe");
    let mut sys = sysinfo::System::new();
    sys.refresh_processes();
    sys.processes()
        .values()
        .any(|p| p.name().to_ascii_lowercase().trim_end_matches(".exe") == target)
}

/// Whether any UT4 shipping client (`UE4-Win64-Shipping.exe`) is currently
/// running. Pak install/removal must refuse while it is, because the engine
/// holds an open handle on every mounted pak — replacing one then fails with a
/// sharing violation. The image name is identical across installs, so a single
/// fixed-name check covers them all.
pub(crate) fn shipping_client_running() -> bool {
    is_game_running("UE4-Win64-Shipping.exe".to_string())
}

/// Host OS, so the frontend can hide Windows-only surfaces on Linux (the .NET
/// gate, the Windows game-installer flow, the "run as administrator" escape hatch,
/// and stray-plugin scanning). Compile-time — the launcher binary is per-platform.
#[derive(serde::Serialize)]
pub struct PlatformInfo {
    /// `"windows"` | `"linux"` | `"other"`.
    pub os: String,
}

#[tauri::command]
pub fn platform_info() -> PlatformInfo {
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "other"
    };
    PlatformInfo { os: os.to_string() }
}

/// Path to the persistent launcher state file in the per-app config dir.
pub(crate) fn state_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("state.json"))
}

/// The active install's `(root, override_prefix, override_wine)` from saved state —
/// the inputs both Linux in-prefix path resolvers (mod-paks, Engine.ini) need.
#[cfg(not(windows))]
fn linux_active_ctx(
    app: &tauri::AppHandle,
) -> Option<(Option<PathBuf>, Option<PathBuf>, Option<PathBuf>)> {
    let state = state_path(app)
        .and_then(|p| ncp_host::state::read(&p).map_err(|e| e.to_string()))
        .ok()
        .flatten()?;
    Some((
        state.install_path.clone(),
        state.linux_launch.as_ref().and_then(|l| l.prefix.clone()),
        state.linux_launch.as_ref().and_then(|l| l.wine.clone()),
    ))
}

/// Resolve the mod-paks directory for the active install.
///
/// On Linux the game runs under Wine and reads downloaded paks from INSIDE its
/// prefix (`…/drive_c/users/<user>/Documents/UnrealTournament/Saved/Paks/
/// DownloadedPaks`), not the Linux `~/Documents` that `default_mod_paks_dir`
/// returns — so prefer the in-prefix dir (an explicit Wine/Proton override prefix
/// wins, else the Lutris prefix for the saved install). Falls back to the global
/// location on Windows, or when no prefix is discoverable.
pub(crate) fn active_mod_paks_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    #[cfg(not(windows))]
    {
        if let Some((root, prefix, wine)) = linux_active_ctx(app) {
            if let Some(dir) = ncp_host::linux::linux_mod_paks_dir(
                root.as_deref(),
                prefix.as_deref(),
                wine.as_deref(),
            ) {
                return Some(dir);
            }
        }
    }
    #[cfg(windows)]
    let _ = app;
    ncp_host::default_mod_paks_dir()
}

/// Resolve the `Engine.ini` path for the active install — the in-prefix
/// `…/Saved/Config/WindowsNoEditor/Engine.ini` the Wine-run game reads on Linux,
/// else the Linux `~/Documents` location (Windows, or no discoverable prefix).
pub(crate) fn active_engine_ini(app: &tauri::AppHandle) -> Option<PathBuf> {
    #[cfg(not(windows))]
    {
        if let Some((root, prefix, wine)) = linux_active_ctx(app) {
            if let Some(ini) = ncp_host::linux::linux_engine_ini(
                root.as_deref(),
                prefix.as_deref(),
                wine.as_deref(),
            ) {
                return Some(ini);
            }
        }
    }
    #[cfg(windows)]
    let _ = app;
    ncp_host::config::engine_ini_path()
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
    state.launch_priority = ncp_host::priority_from_label(&priority);
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

/// (Linux) Save the explicit Wine/Proton launch override — the WINEPREFIX and the
/// wine binary to launch through. Passing an empty/absent `prefix` clears the
/// override (revert to Lutris auto-detection). `wine` empty/absent = system wine.
#[tauri::command]
pub fn save_linux_launch(
    app: tauri::AppHandle,
    prefix: Option<String>,
    wine: Option<String>,
) -> Result<(), String> {
    let path = state_path(&app)?;
    let mut state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let prefix = prefix
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    state.linux_launch = prefix.map(|p| ncp_host::LinuxLaunchSettings {
        prefix: Some(PathBuf::from(p)),
        wine: wine
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from),
    });
    ncp_host::state::write(&path, &state).map_err(|e| e.to_string())
}

/// (Linux) Save whether to use GPU-accelerated (DMABUF) webview rendering.
/// `false` (the default) makes `run()` force `WEBKIT_DISABLE_DMABUF_RENDERER=1` on
/// the next launch — the safe default that avoids the AMD/Wayland white-screen
/// (`EGL_BAD_PARAMETER`). Takes effect after a restart: WebKitGTK reads the env var
/// once, when the webview process forks at startup.
#[tauri::command]
pub fn save_linux_gpu_accel(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    let path = state_path(&app)?;
    let mut state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    state.linux_gpu_accel = Some(enabled);
    ncp_host::state::write(&path, &state).map_err(|e| e.to_string())
}

/// (Linux) Save whether "gaming mode" is on: while the game runs, a detached
/// watchdog keeps its window fullscreen+focused (fixes the GNOME top bar / dock
/// drawing over the game and restores fullscreen unredirection) and temporarily
/// disables the Ubuntu dock. Read at game-launch time — no restart needed; the
/// next launch picks it up.
#[tauri::command]
pub fn save_linux_gaming_mode(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    let path = state_path(&app)?;
    let mut state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    state.linux_gaming_mode = Some(enabled);
    ncp_host::state::write(&path, &state).map_err(|e| e.to_string())
}

/// (Linux) Wine/Proton runners installed on this machine (Lutris runners + Steam
/// compatibilitytools.d), each resolved to its wine binary — for the settings
/// dropdown. Empty on Windows / when none are found.
#[tauri::command]
pub fn list_wine_runners() -> Vec<ncp_host::linux::WineRunner> {
    #[cfg(not(windows))]
    {
        ncp_host::linux::list_wine_runners()
    }
    #[cfg(windows)]
    {
        Vec::new()
    }
}

/// (Linux) Resolve what the launcher WOULD spawn for `executable` + `args` under
/// the current saved override (else Lutris auto-detect): returns the wine program,
/// full argv, WINEPREFIX, and any wrapper — for the settings panel's "resolved
/// command" preview and to pre-fill the fields with known-good values. `null` when
/// nothing resolves (not a Wine setup) or on Windows.
#[tauri::command]
pub fn resolve_linux_launch(
    app: tauri::AppHandle,
    executable: String,
    args: Vec<String>,
) -> Option<ncp_host::linux::WineLaunch> {
    #[cfg(not(windows))]
    {
        let linux_launch = ncp_host::state::read(&state_path(&app).ok()?)
            .ok()
            .flatten()
            .and_then(|s| s.linux_launch);
        ncp_host::launch::resolve_wine_launch(Path::new(&executable), &args, linux_launch.as_ref())
    }
    #[cfg(windows)]
    {
        let _ = (app, executable, args);
        None
    }
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

/// Save (or clear, by passing null) the per-user PUG launcher token (IGBot/iCTF).
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

/// Save (or clear, by passing null) the per-user UTPugs PUG launcher token.
/// Stored separately from the iCTF token — PUG tokens are per-community.
#[tauri::command]
pub fn save_utpugs_token(app: tauri::AppHandle, token: Option<String>) -> Result<(), String> {
    let path = state_path(&app)?;
    let mut state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    state.utpugs_launcher_token = token
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    ncp_host::state::write(&path, &state).map_err(|e| e.to_string())
}

/// Save (or clear, by passing null) the per-user UnrealPUGs PUG launcher token.
/// Per-community, stored separately from the iCTF/UTPugs tokens.
#[tauri::command]
pub fn save_unrealpugs_token(app: tauri::AppHandle, token: Option<String>) -> Result<(), String> {
    let path = state_path(&app)?;
    let mut state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    state.unrealpugs_launcher_token = token
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

/// Fetch a player's per-mode trends from ut4stats.com's public
/// `/api/player_trends/<id>/?mode=<m>` endpoint (sniper/lightning accuracy
/// series, form/streak, and the rating curve on ELO modes). Returns the raw
/// JSON. `mode` is one of the launcher mode keys (elimplus/ctf/blitz/wipeout/
/// duel/elimination); it is URL-encoded defensively even though it is ours.
#[tauri::command]
pub async fn ut4stats_trends(playerid: String, mode: String) -> Result<String, String> {
    let url = format!(
        "{UT4STATS_BASE}/api/player_trends/{playerid}/?mode={}",
        encode_q(mode.trim())
    );
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

/// The 6 NetcodePlus content paks on UT Custom Content, as
/// `(manifest-key, UTCC content id)` pairs. Order is stable so the
/// aggregation is deterministic; the manifest key is what the frontend keys on.
const HUB_PAK_CONTENT: &[(&str, u32)] = &[
    ("elimplus", 1996),
    ("wipeout", 2036),
    ("ncutplus", 852),
    ("instagibncp", 2037),
    ("sdom", 2040),
    ("ncwepmut", 2042),
];

/// A UTCC content payload is a few hundred KB of JSON at most; cap generously.
const UTCC_CONTENT_MAX: u64 = 10 * 1024 * 1024;

/// One hub's recorded version of a single pak, in the aggregated output.
struct HubPakVersion {
    /// The version display name the hub serves.
    version: String,
    /// True when the hub tracks the pak's latest version (either it opted into
    /// `useLatest`, or the version it pins equals the pak's current latest name).
    is_latest: bool,
}

/// Fetch one UTCC content payload and fold it into the running aggregation.
///
/// `latest` collects each pak's current latest-version display name (keyed by
/// manifest key). `hubs` collects, per hub NAME, that hub's version of each pak.
/// Both are mutated in place. Everything is parsed defensively from an untrusted
/// external payload: a missing / oddly-typed field skips just that entry rather
/// than failing — an individual malformed server never derails the pak.
fn fold_utcc_content(
    body: &str,
    pak_key: &str,
    latest: &mut std::collections::BTreeMap<String, String>,
    hubs: &mut std::collections::BTreeMap<String, HubEntry>,
) {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(body) else {
        return;
    };

    // Map version id -> display name for this pak.
    let mut ver_names: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
    if let Some(vers) = root.get("versions").and_then(|v| v.as_array()) {
        for v in vers {
            let (Some(id), Some(name)) = (
                v.get("id").and_then(serde_json::Value::as_i64),
                v.get("name").and_then(serde_json::Value::as_str),
            ) else {
                continue;
            };
            ver_names.insert(id, name.to_string());
        }
    }

    // The pak's "latest" display name = the name of content.latestVersion.id.
    let latest_name = root
        .get("content")
        .and_then(|c| c.get("latestVersion"))
        .and_then(|lv| lv.get("id"))
        .and_then(serde_json::Value::as_i64)
        .and_then(|id| ver_names.get(&id).cloned());
    if let Some(ref name) = latest_name {
        latest.insert(pak_key.to_string(), name.clone());
    }

    let Some(servers) = root.get("servers").and_then(|s| s.as_array()) else {
        return;
    };
    for srv in servers {
        let Some(server) = srv.get("server") else {
            continue;
        };
        let Some(name) = server.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let server_type = server
            .get("serverType")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        let use_latest = srv
            .get("useLatest")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        // Resolve the hub's pinned version display name from its first listed
        // version id. `useLatest` hubs may not list one — fall back to latest.
        let pinned_name = srv
            .get("versions")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(serde_json::Value::as_i64)
            .and_then(|id| ver_names.get(&id).cloned());

        let version = match (pinned_name, use_latest, latest_name.as_ref()) {
            (Some(v), _, _) => v,
            (None, true, Some(l)) => l.clone(),
            // No resolvable version for this hub+pak — nothing meaningful to show.
            _ => continue,
        };

        let is_latest = use_latest || latest_name.as_deref() == Some(version.as_str());

        let entry = hubs.entry(name.to_string()).or_insert_with(|| HubEntry {
            name: name.to_string(),
            server_type: server_type.clone(),
            paks: std::collections::BTreeMap::new(),
        });
        // First non-empty server_type wins (payloads agree per hub in practice).
        if entry.server_type.is_empty() && !server_type.is_empty() {
            entry.server_type = server_type;
        }
        entry
            .paks
            .insert(pak_key.to_string(), HubPakVersion { version, is_latest });
    }
}

/// One hub in the aggregated output, accumulating its per-pak versions across
/// all 6 content payloads. Keyed by pak key so the paks list sorts naturally.
struct HubEntry {
    name: String,
    server_type: String,
    paks: std::collections::BTreeMap<String, HubPakVersion>,
}

/// Aggregate the 6 NetcodePlus content paks on UT Custom Content into a compact
/// hub -> pak-version view for the "which hub serves which pak version" UI.
///
/// Fetches each content payload from UTCC (same net path as [`list_servers`]),
/// resolves each hub's pinned version to its display name, and folds them into a
/// per-hub aggregation. External JSON is parsed defensively — a malformed field
/// skips just that entry, and a single pak's fetch failing skips only that pak;
/// only an all-six failure returns `Err`.
///
/// Returns a JSON string of shape:
/// ```json
/// {"latest": {"elimplus": "3.27.4", ...},
///  "hubs": [{"name": "...", "server_type": "HUB",
///            "paks": [{"pak": "elimplus", "version": "3.27.4", "is_latest": true}, ...]}]}
/// ```
/// `hubs` is sorted by name (case-insensitive); each hub's `paks` by pak key.
#[tauri::command]
pub async fn hub_pak_versions() -> Result<String, String> {
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;

    let mut latest: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let mut hubs: std::collections::BTreeMap<String, HubEntry> = std::collections::BTreeMap::new();
    let mut any_ok = false;

    for (pak_key, id) in HUB_PAK_CONTENT {
        let url = format!("https://utcustomcontent.com/content/{id}/data");
        match ncp_net::fetch_text(&client, &url, UTCC_CONTENT_MAX).await {
            Ok(body) => {
                any_ok = true;
                fold_utcc_content(&body, pak_key, &mut latest, &mut hubs);
            }
            // A single pak's fetch failing skips only that pak — never the whole
            // command. Only an all-six failure is an error (below).
            Err(_) => continue,
        }
    }

    if !any_ok {
        return Err("Couldn't reach UT Custom Content for any content pak.".into());
    }

    // Sort hubs case-insensitively by name; sort each hub's paks by pak key
    // (BTreeMap iteration already yields sorted keys).
    let mut hub_list: Vec<&HubEntry> = hubs.values().collect();
    hub_list.sort_by_key(|h| h.name.to_lowercase());

    let hubs_json: Vec<serde_json::Value> = hub_list
        .into_iter()
        .map(|h| {
            let paks: Vec<serde_json::Value> = h
                .paks
                .iter()
                .map(|(pak, v)| {
                    serde_json::json!({
                        "pak": pak,
                        "version": v.version,
                        "is_latest": v.is_latest,
                    })
                })
                .collect();
            serde_json::json!({
                "name": h.name,
                "server_type": h.server_type,
                "paks": paks,
            })
        })
        .collect();

    let out = serde_json::json!({
        "latest": latest,
        "hubs": hubs_json,
    });
    Ok(out.to_string())
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

/// Open the OS file browser with `path` selected, so the user can remove a
/// misplaced stray file themselves — the safe fallback when the launcher can't
/// delete it unprivileged (most often because it lives in a Program-Files
/// install). This is an UNPRIVILEGED shell action that only OPENS a window; the
/// launcher never performs a privileged delete. Deleting from a protected folder
/// in Explorer triggers Windows' OWN elevation prompt via the trusted shell —
/// which is exactly why we hand off here instead of elevating ourselves (a
/// bespoke elevated delete is a privilege-escalation footgun; see the launcher
/// handover notes).
#[tauri::command]
pub fn reveal_in_folder(path: String) -> Result<(), String> {
    let p = Path::new(&path);
    #[cfg(windows)]
    {
        // `explorer /select,<path>` highlights the file in its folder. Spawned via
        // Command (no shell, so the path can't inject). `explorer` returns a
        // nonzero exit even on success, so spawn and don't inspect status.
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", p.display()))
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("couldn't open the folder: {e}"))
    }
    #[cfg(not(windows))]
    {
        // Best-effort open the containing directory (the stray panel is
        // Windows-only, so this is not reached from the UI today).
        let dir = p.parent().unwrap_or(p);
        std::process::Command::new("xdg-open")
            .arg(dir)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("couldn't open the folder: {e}"))
    }
}

/// UT4IGBot launcher PUG endpoint (HTTPS via the ut4stats.com Apache
/// reverse-proxy to the bot's FastAPI app; the proxied path is TLS-terminated).
const BOT_PUG_URL: &str = "https://ut4stats.com/launcher/pug_action";

/// The PUG modes the Instagib Nation bot (UT4IGBot) offers via the launcher.
/// `mode` is forwarded verbatim into the bot's JSON body (it resolves it against
/// its `modes` table), so the launcher only ever relays a value the bot
/// advertises. iCTF is the long-standing default; `elim` was added alongside it.
const IGBOT_MODES: &[&str] = &["ictf", "elim"];

/// Reject any Instagib Nation `mode` the bot doesn't advertise, before it's
/// relayed. Mirrors [`check_autopug_mode`] for the iCTF community.
fn check_igbot_mode(mode: &str) -> Result<(), String> {
    if IGBOT_MODES.contains(&mode) {
        Ok(())
    } else {
        Err(format!("unknown Instagib Nation mode '{mode}'"))
    }
}

/// Map a bot PUG-call error to a user-facing message, turning an HTTP 401 (a
/// missing / unrecognized / revoked launcher token) into clear guidance rather
/// than a raw status. The "/launchertoken" phrasing is also what the frontend
/// keys on to drop the dead token and fall back to the link-token prompt.
fn pug_error(e: ncp_net::NetError) -> String {
    match e {
        ncp_net::NetError::HttpStatus { status: 401, .. } => {
            "Your launcher token wasn't recognized — run /launchertoken in the Discord and paste the new token.".to_string()
        }
        other => other.to_string(),
    }
}

/// Send a PUG queue action (`joinpug` / `leavepug` / `listpug`) to the bot,
/// authenticated by the player's per-user `token` (issued by the bot's
/// `/launchertoken` command). The bot resolves the player from the token, so
/// the launcher sends NO `ut4_id` -- a token only ever queues its owner.
/// Returns the bot's status message JSON for the UI to display.
#[tauri::command]
pub async fn pug_action(
    action: String,
    mode: String,
    token: String,
    build: Option<u32>,
) -> Result<String, String> {
    if token.trim().is_empty() {
        return Err(
            "Set your launcher token first (run /launchertoken in the Instagib Nation Discord)."
                .into(),
        );
    }
    check_igbot_mode(&mode)?;
    // `plugin_version` (the player's installed NetcodePlus build) lets the bot
    // reject a stale client from the queue before teams form. null = an older
    // launcher; the server version gate still backstops those.
    let body =
        serde_json::json!({ "action": action, "mode": mode, "plugin_version": build }).to_string();
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    ncp_net::post_json(
        &client,
        BOT_PUG_URL,
        &[("launcher-token", token.trim())],
        body,
        64 * 1024,
    )
    .await
    .map_err(pug_error)
}

/// UT4IGBot launcher PUG status endpoint (HTTPS via the Apache reverse-proxy).
const BOT_STATUS_URL: &str = "https://ut4stats.com/launcher/pug_status";

/// Poll the player's PUG status (queued / live + the live server's connect
/// info) for `mode`, authenticated by the per-user launcher `token`. Returns the
/// bot's status JSON so the UI can show a one-click CONNECT when the PUG starts.
/// `mode` scopes the queue branch (the bot reports the right queue's fill); the
/// readycheck/live branches are mode-agnostic (resolved by the player).
#[tauri::command]
pub async fn pug_status(mode: String, token: String) -> Result<String, String> {
    if token.trim().is_empty() {
        return Err("no launcher token set".into());
    }
    check_igbot_mode(&mode)?;
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    ncp_net::post_json(
        &client,
        BOT_STATUS_URL,
        &[("launcher-token", token.trim())],
        serde_json::json!({ "mode": mode }).to_string(),
        64 * 1024,
    )
    .await
    .map_err(pug_error)
}

/// UT4IGBot launcher ready-up endpoint (HTTPS via the Apache reverse-proxy).
const BOT_READY_URL: &str = "https://ut4stats.com/launcher/ready";

/// Ready up for a filling PUG's check-in — the Discord-outage backup. When a PUG
/// fills, players normally click Ready on the Discord check-in message; if
/// Discord is down they can't, and the PUG cancels. This marks the player ready
/// via the bot's FastAPI (independent of the Discord gateway), authenticated by
/// the per-user launcher `token`. Returns the bot's JSON
/// (`{state:"readied", pug_id, ready_count, ready_needed, you_readied,
/// seconds_left}` or `{state:"no_readycheck"}`). A bot that hasn't deployed this
/// endpoint yet 404s → surfaced as an error the caller can show.
#[tauri::command]
pub async fn pug_ready(token: String) -> Result<String, String> {
    if token.trim().is_empty() {
        return Err("no launcher token set".into());
    }
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    ncp_net::post_json(
        &client,
        BOT_READY_URL,
        &[("launcher-token", token.trim())],
        "{}".to_string(),
        64 * 1024,
    )
    .await
    .map_err(pug_error)
}

/// UT4IGBot launcher spectate endpoint (HTTPS via the Apache reverse-proxy).
const BOT_SPECTATE_URL: &str = "https://ut4stats.com/launcher/spectate";

/// Ask the bot for the live PUGs the launcher can spectate, authenticated by the
/// per-user launcher `token`. Returns the bot's JSON
/// (`{state:"live", pugs:[{pug_id, server, password, mode, map}, …]}` or
/// `{state:"none"}`); the UI connects with `?SpectatorOnly=1` (a picker when
/// there's more than one).
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
    .map_err(pug_error)
}

/// UT4IGBot tokenless live-PUG endpoint (HTTPS via the Apache reverse-proxy).
const BOT_LIVE_URL: &str = "https://ut4stats.com/launcher/live";

/// Ask the bot for every live PUG that can be spectated, WITHOUT a launcher
/// token. Powers the HOME "live PUG — watch it" banner so a brand-new user can
/// spectate-to-learn before linking a token. Read-only on the bot side; same
/// JSON as [`pug_spectate`] (`{state:"live", pugs:[…]}` / `{state:"none"}`). The
/// UI connects with `?SpectatorOnly=1`.
#[tauri::command]
pub async fn pug_live() -> Result<String, String> {
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    ncp_net::post_json(&client, BOT_LIVE_URL, &[], "{}".to_string(), 64 * 1024)
        .await
        .map_err(|e| e.to_string())
}

/// UT4IGBot tokenless queue-counts endpoint (HTTPS via the Apache reverse-proxy).
const BOT_QUEUES_URL: &str = "https://ut4stats.com/launcher/queues";

/// Ask the bot for current PUG queue fill counts, WITHOUT a launcher token.
/// Powers the HOME "queue filling" nudge so anyone sees a near-full queue and
/// can jump in. Read-only; returns the bot's JSON
/// (`{queues:[{mode, players, max_players}, …]}`). A bot that hasn't deployed
/// this endpoint yet 404s → the caller swallows it and shows nothing.
#[tauri::command]
pub async fn pug_queues() -> Result<String, String> {
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    ncp_net::post_json(&client, BOT_QUEUES_URL, &[], "{}".to_string(), 64 * 1024)
        .await
        .map_err(|e| e.to_string())
}

// ===================================================================
// UTPugs (autopug) PUG API — a SECOND community, compiled in alongside
// the IGBot/iCTF endpoints above. autopug implements the same launcher
// PUG contract (LAUNCHER-PUG-API-SPEC-v1) at its OWN HTTPS base, and runs
// SEVERAL modes (Wipeout / Elimination / CTF / Duel), so `mode` is a
// parameter here rather than hardcoded. Same trust model as the iCTF base:
// the launcher only ever talks to a base baked into this signed binary,
// never a webview-supplied endpoint. (The signed-manifest communities[]
// registry — architecture C in the spec — generalises this without a
// rebuild; this hardcoded second community is the right-sized step for now.)
// ===================================================================

/// autopug's launcher PUG API base URL (HTTPS, **no trailing slash**). The
/// launcher appends `/pug_action`, `/pug_status`, `/spectate`; ut4pugs.us's
/// Apache reverse-proxies each `/launcher/<op>` → `127.0.0.1:9100/launcher_<op>`
/// (autopug's aiohttp app). Empty string would disable UTPugs
/// ([`utpugs_configured`] → `false`, UI hides the section, commands refuse).
const AUTOPUG_BASE: &str = "https://ut4pugs.us/launcher";

/// The PUG modes autopug offers — the `mode` key sent on join/leave/list and
/// status, matched against autopug's Discord pickup queue **names** (exact, then
/// substring, in `launcher_api._find_channel_pickup`). The launcher accepts only
/// these (defensive: the value is forwarded into the bot's JSON body, so we never
/// relay an arbitrary mode).
///
/// CTF is intentionally absent: autopug's `pickup_rules.json` has no CTF
/// host-rule, so a CTF pickup would auto-host with the elimination fallback. Add
/// `"ctf"` here (and to the frontend `UTPUGS_MODES`) once that rule exists.
/// `duel` lives in a separate Discord channel, which is fine — `_find_channel_
/// pickup` scans every channel's pickups, and member resolution is guild-wide.
const AUTOPUG_MODES: &[&str] = &["wipe", "elim", "duel"];

/// Whether UTPugs PUGs are wired up in this build (the base URL is set). The UI
/// shows the UTPugs section only when this is true.
#[tauri::command]
pub fn utpugs_configured() -> bool {
    !AUTOPUG_BASE.is_empty()
}

/// Build an autopug endpoint URL, refusing if the base isn't configured.
fn autopug_url(path: &str) -> Result<String, String> {
    if AUTOPUG_BASE.is_empty() {
        return Err("UTPugs PUGs aren't enabled in this launcher build yet.".into());
    }
    Ok(format!("{AUTOPUG_BASE}{path}"))
}

/// Reject any `mode` autopug doesn't advertise, before it reaches the bot.
fn check_autopug_mode(mode: &str) -> Result<(), String> {
    if AUTOPUG_MODES.contains(&mode) {
        Ok(())
    } else {
        Err(format!("unknown UTPugs mode '{mode}'"))
    }
}

/// UTPugs join/leave/list for `mode` (one of wipe/elim/ctf/duel), authenticated
/// by the per-user UTPugs `token`. Mirrors [`pug_action`] but against autopug and
/// with a real mode parameter (autopug runs several queues).
#[tauri::command]
pub async fn utpugs_action(
    action: String,
    mode: String,
    token: String,
    build: Option<u32>,
) -> Result<String, String> {
    if token.trim().is_empty() {
        return Err(
            "Set your UTPugs launcher token first (run /launchertoken in the UTPugs Discord)."
                .into(),
        );
    }
    check_autopug_mode(&mode)?;
    let url = autopug_url("/pug_action")?;
    let body =
        serde_json::json!({ "action": action, "mode": mode, "plugin_version": build }).to_string();
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    ncp_net::post_json(
        &client,
        &url,
        &[("launcher-token", token.trim())],
        body,
        64 * 1024,
    )
    .await
    .map_err(pug_error)
}

/// Poll the caller's UTPugs status for `mode` (queued / starting / live + the
/// live server's connect info). Mirrors [`pug_status`]; sends `mode` so autopug
/// reports the right queue. Drives the per-mode one-click CONNECT.
#[tauri::command]
pub async fn utpugs_status(mode: String, token: String) -> Result<String, String> {
    if token.trim().is_empty() {
        return Err("no launcher token set".into());
    }
    check_autopug_mode(&mode)?;
    let url = autopug_url("/pug_status")?;
    let body = serde_json::json!({ "mode": mode }).to_string();
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    ncp_net::post_json(
        &client,
        &url,
        &[("launcher-token", token.trim())],
        body,
        64 * 1024,
    )
    .await
    .map_err(pug_error)
}

/// Ask autopug for the live UTPugs PUGs the caller can spectate (across ALL
/// modes — the response carries each pug's mode). Mirrors [`pug_spectate`];
/// mode-agnostic. The UI connects with `?SpectatorOnly=1`.
#[tauri::command]
pub async fn utpugs_spectate(token: String) -> Result<String, String> {
    if token.trim().is_empty() {
        return Err("no launcher token set".into());
    }
    let url = autopug_url("/spectate")?;
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    ncp_net::post_json(
        &client,
        &url,
        &[("launcher-token", token.trim())],
        "{}".to_string(),
        64 * 1024,
    )
    .await
    .map_err(pug_error)
}

/// Ready up for a filling UTPugs pickup's check-in (autopug's Discord-outage
/// backup). Mirrors [`pug_ready`] for the second community. Mode-agnostic: a
/// player is only ever in one pickup's check-in, which the bot resolves from the
/// token. Returns autopug's JSON (`{state:"readied", …}` / `{state:"no_readycheck"}`).
#[tauri::command]
pub async fn utpugs_ready(token: String) -> Result<String, String> {
    if token.trim().is_empty() {
        return Err("no launcher token set".into());
    }
    let url = autopug_url("/ready")?;
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    ncp_net::post_json(
        &client,
        &url,
        &[("launcher-token", token.trim())],
        "{}".to_string(),
        64 * 1024,
    )
    .await
    .map_err(pug_error)
}

/// Ask autopug for current UTPugs queue fill counts across modes, WITHOUT a
/// token. Mirrors [`pug_queues`] for the second community. Returns autopug's
/// JSON (`{queues:[{mode, players, max_players}, …]}`); the UI keeps only the
/// near-full ones for the HOME nudge.
#[tauri::command]
pub async fn utpugs_queues() -> Result<String, String> {
    let url = autopug_url("/queues")?;
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    ncp_net::post_json(&client, &url, &[], "{}".to_string(), 64 * 1024)
        .await
        .map_err(|e| e.to_string())
}

// ---- UnrealPUGs (skandalouz's PugApi bot, the third community) --------------
//
// Multi-tenant Spring bot: the community is the 5-char `clientId` baked into the
// path (`/launcher/upugs`). Reduced surface vs UTPugs — join / leave / queue-
// status only; the bot tracks no live game servers yet, so spectate/live/ready
// are deferred bot-side and we don't wire them. Auth is the same per-player
// `launcher-token` header. Empty base disables the section.
//
// ⚠️ `:7081` is an unusual port to pin into a signed release — confirm with
// skandalouz it's permanent (or gets proxied to 443) before shipping.
const UNREALPUGS_BASE: &str = "https://www.hkant.nl:7081/launcher/upugs";

/// Modes UnrealPUGs offers via the launcher. The `mode` is sent verbatim and
/// `check_unrealpugs_mode` gates it with an EXACT (case-sensitive) match — so the
/// frontend must send the literal uppercase name (`UNREALPUGS_MODE = "BLITZ"` in
/// main.ts). The bot itself is case-lenient, but our own guard is not, so a
/// lowercase "blitz" would be rejected here before ever reaching the bot.
/// BLITZ-only for now — the community also runs CTF/ICTF/2TDM; add them here + in
/// the frontend list when we widen past Blitz.
const UNREALPUGS_MODES: &[&str] = &["BLITZ"];

/// Whether UnrealPUGs PUGs are wired up in this build (base URL set). The UI
/// shows the section only when true.
#[tauri::command]
pub fn unrealpugs_configured() -> bool {
    !UNREALPUGS_BASE.is_empty()
}

/// Build an UnrealPUGs endpoint URL, refusing if the base isn't configured.
fn unrealpugs_url(path: &str) -> Result<String, String> {
    if UNREALPUGS_BASE.is_empty() {
        return Err("UnrealPUGs PUGs aren't enabled in this launcher build yet.".into());
    }
    Ok(format!("{UNREALPUGS_BASE}{path}"))
}

/// Reject any `mode` UnrealPUGs doesn't advertise, before it reaches the bot.
fn check_unrealpugs_mode(mode: &str) -> Result<(), String> {
    if UNREALPUGS_MODES.contains(&mode) {
        Ok(())
    } else {
        Err(format!("unknown UnrealPUGs mode '{mode}'"))
    }
}

/// UnrealPUGs join/leave/list for `mode`, authenticated by the per-user token.
/// Mirrors [`utpugs_action`]; the bot requires `mode` on join/leave.
#[tauri::command]
pub async fn unrealpugs_action(
    action: String,
    mode: String,
    token: String,
) -> Result<String, String> {
    if token.trim().is_empty() {
        return Err(
            "Set your UnrealPUGs launcher token first (run launchertoken in the UnrealPUGs Discord)."
                .into(),
        );
    }
    check_unrealpugs_mode(&mode)?;
    let url = unrealpugs_url("/pug_action")?;
    let body = serde_json::json!({ "action": action, "mode": mode }).to_string();
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    ncp_net::post_json(
        &client,
        &url,
        &[("launcher-token", token.trim())],
        body,
        64 * 1024,
    )
    .await
    .map_err(pug_error)
}

/// Poll the caller's UnrealPUGs status for `mode` (idle / queued / starting).
/// Mirrors [`utpugs_status`]. The bot never reports `live` (no server tracking),
/// so there's no CONNECT surface — join/leave/queue-status only.
#[tauri::command]
pub async fn unrealpugs_status(mode: String, token: String) -> Result<String, String> {
    if token.trim().is_empty() {
        return Err("no launcher token set".into());
    }
    check_unrealpugs_mode(&mode)?;
    let url = unrealpugs_url("/pug_status")?;
    let body = serde_json::json!({ "mode": mode }).to_string();
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    ncp_net::post_json(
        &client,
        &url,
        &[("launcher-token", token.trim())],
        body,
        64 * 1024,
    )
    .await
    .map_err(pug_error)
}

/// Launcher version (from `Cargo.toml`). Surfaced in the UI for bug reports.
#[tauri::command]
pub fn launcher_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The active install's Engine.ini path (in-prefix on Linux, `~/Documents` on
/// Windows), or an error if it can't be located.
fn engine_ini(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    active_engine_ini(app).ok_or_else(|| "could not locate your Engine.ini".to_string())
}

/// Read the player's current competitive-config state: whether the ini
/// exists, whether a restore point exists, and the current editable values.
#[tauri::command]
pub fn engine_config_state(app: tauri::AppHandle) -> Result<ncp_host::config::ConfigState, String> {
    Ok(ncp_host::config::read_state(&engine_ini(&app)?))
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
    /// Whether the canonical Desktop shortcut exists but points at a different
    /// (old) exe — so the UI offers "Update desktop shortcut". `false` when there
    /// is no such shortcut or it already points here, so a user who launches the
    /// exe directly is never nagged to make one.
    pub shortcut_needs_update: bool,
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
    // The version running at the previous launch, captured before we overwrite
    // it below — lets us spot a downgrade (an older build being run again).
    let prev_version = state.installed_launcher_version.clone();

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

    // Revalidate any pending old-launcher record: drop it if its file is gone,
    // if it points at the launcher running right now (you can't tidy yourself
    // away — this is what surfaced when an older build was re-run after a newer
    // one had recorded it as the "old" copy), or if this run is older than the
    // previous one (the user went back to an earlier build, so the post-update
    // prompt no longer applies).
    if let Some(p) = state.pending_old_launcher_path.clone() {
        let p_path = Path::new(&p);
        if ncp_host::is_stale_pending(
            p_path,
            p_path.is_file(),
            &current_path,
            &current_semver,
            prev_version.as_deref(),
        ) {
            state.pending_old_launcher_path = None;
        }
    }

    // Always record where/which build is running now.
    state.installed_launcher_path = Some(current_path.to_string_lossy().into_owned());
    state.installed_launcher_version = Some(current_version.to_string());
    ncp_host::state::write(&path, &state).map_err(|e| e.to_string())?;

    // Keep the canonical "UT4 Community Launcher.lnk" pointed at the exe running
    // now — overwriting that same file in place (never a second icon), and only
    // if it already exists (a direct-exe user is left alone). This heals a
    // shortcut left stale by the old versioned-sibling scheme; with the in-place
    // swap the target no longer moves, so it's an idempotent rewrite thereafter.
    let repoint = ncp_host::repoint_launcher_shortcut_if_present(&current_path);

    Ok(HousekeepingResult {
        old_launcher_path: state.pending_old_launcher_path,
        current_version: current_version.to_string(),
        // The manual "Update desktop shortcut" button only shows if the in-place
        // repoint above actually failed (locked file / unwritable Desktop).
        // Windows-only: the button's `create_launcher_shortcut` command writes a
        // `.lnk`, which is a stub on Linux — so a Failed Linux `.desktop` repoint
        // must NOT surface a button that could only ever error.
        shortcut_needs_update: cfg!(windows) && repoint == ncp_host::ShortcutRepoint::Failed,
    })
}

/// Create a Desktop shortcut ("UT4 Community Launcher.lnk") pointing at the
/// running launcher exe. Returns the shortcut path on success.
#[tauri::command]
pub fn create_launcher_shortcut() -> Result<String, String> {
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let lnk = ncp_host::create_desktop_shortcut(&current, ncp_host::LAUNCHER_SHORTCUT_NAME)
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
/// Outcome of [`delete_old_launcher`], so the UI can word the result correctly.
#[derive(Debug, serde::Serialize)]
pub struct DeleteOldLauncherResult {
    /// The old exe was locked (it is probably still running), so the removal was
    /// scheduled for the next reboot via `MoveFileEx` rather than done now.
    pub scheduled_for_reboot: bool,
}

/// True for the filesystem errors a still-running exe produces: Windows locks a
/// running image file, so deleting it fails with a sharing violation (os error
/// 32) or access-denied (os error 5).
fn is_lock_error(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(5) | Some(32))
        || e.kind() == std::io::ErrorKind::PermissionDenied
}

#[tauri::command]
pub fn delete_old_launcher(app: tauri::AppHandle) -> Result<DeleteOldLauncherResult, String> {
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
        return Ok(DeleteOldLauncherResult {
            scheduled_for_reboot: false,
        });
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

    let scheduled_for_reboot = match std::fs::remove_file(&old_path) {
        Ok(()) => false,
        // The old exe is probably still open, so Windows has its image file
        // locked. Schedule the delete for the next reboot rather than failing.
        Err(e) if is_lock_error(&e) => {
            ncp_host::schedule_delete_on_reboot(&old_path).map_err(|e| e.to_string())?;
            true
        }
        Err(e) => return Err(e.to_string()),
    };
    state.pending_old_launcher_path = None;
    ncp_host::state::write(&path, &state).map_err(|e| e.to_string())?;
    Ok(DeleteOldLauncherResult {
        scheduled_for_reboot,
    })
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

/// View of the user's onboarding state for the webview, returned by
/// [`onboarding_status`]. `fresh_install` is the one-time classification: true
/// only for a genuine fresh install that hasn't completed the first-run
/// walkthrough (Surface 1). Existing users get `fresh_install = false` and rely
/// on `seen` to gate the catch-up / what's-new surfaces.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OnboardingView {
    pub fresh_install: bool,
    pub completed: bool,
    pub seen: Vec<String>,
}

/// Resolve and return the user's onboarding state.
///
/// Performs the one-time fresh-install-vs-existing-user classification (keyed on
/// whether a `state.json` existed before this run) and persists it, so the
/// decision survives restarts and a quit mid-walkthrough. Call this EARLY in
/// startup — before any other command writes the state file (housekeeping etc.),
/// which would otherwise create the file and mask a genuine fresh install.
#[tauri::command]
pub fn onboarding_status(app: tauri::AppHandle) -> Result<OnboardingView, String> {
    let path = state_path(&app)?;
    let loaded = ncp_host::state::read(&path).map_err(|e| e.to_string())?;
    let file_existed = loaded.is_some();
    let mut state = loaded.unwrap_or_default();

    let was_resolved = state.onboarding.first_run_resolved;
    let fresh_install = state.onboarding.resolve_first_run(file_existed);
    // Only the first resolution mutates state; skip a needless write afterwards.
    if !was_resolved {
        ncp_host::state::write(&path, &state).map_err(|e| e.to_string())?;
    }

    Ok(OnboardingView {
        fresh_install,
        completed: state.onboarding.completed,
        seen: state.onboarding.seen.iter().cloned().collect(),
    })
}

/// Record one or more discovery-item IDs as seen/dismissed (Surface 2/3).
#[tauri::command]
pub fn mark_features_seen(app: tauri::AppHandle, ids: Vec<String>) -> Result<(), String> {
    let path = state_path(&app)?;
    let mut state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    state.onboarding.mark_seen(ids);
    ncp_host::state::write(&path, &state).map_err(|e| e.to_string())
}

/// Mark the first-run walkthrough finished (Surface 1) and record its item IDs
/// as seen, so they never resurface as catch-up / what's-new.
#[tauri::command]
pub fn complete_onboarding(app: tauri::AppHandle, ids: Vec<String>) -> Result<(), String> {
    let path = state_path(&app)?;
    let mut state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    state.onboarding.complete(ids);
    ncp_host::state::write(&path, &state).map_err(|e| e.to_string())
}

/// Reset onboarding to pristine state so the flows can be replayed for testing.
/// Touches ONLY the `onboarding` block — install path, tokens, paks, etc. are
/// left intact (a reset can never lose real user state).
#[tauri::command]
pub fn reset_onboarding(app: tauri::AppHandle) -> Result<(), String> {
    let path = state_path(&app)?;
    let mut state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    state.onboarding.reset();
    ncp_host::state::write(&path, &state).map_err(|e| e.to_string())
}

/// Engine.ini edits while UT4 is running are futile: the game holds its config
/// in memory and rewrites the whole `[UTGameEngine]` section over ours — the
/// in-game Settings OK button calls `GEngine->SaveConfig()` +
/// `UTEngine->SaveConfig()`, and the first-run path saves it too. Whatever we
/// wrote is silently reverted, which players then report as "the launcher
/// keeps re-enabling smooth framerate" (field report 2026-08-23). Same guard
/// the Mod.ini preset flow already uses, for the same clobber reason.
fn refuse_engine_edit_while_running() -> Result<(), String> {
    if shipping_client_running() {
        return Err(
            "Close UT4 first — the game rewrites Engine.ini from its own settings, \
             which would silently undo these changes."
                .to_string(),
        );
    }
    Ok(())
}

/// Apply the competitive Engine.ini baseline plus the editable knobs,
/// merging into the existing ini (backing it up first). Refused while UT4
/// runs (see [`refuse_engine_edit_while_running`]).
// One flat parameter per panel field, mirroring the UI — a nested struct arg
// would force the frontend to know Rust field casing for no reader benefit.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub fn apply_engine_config(
    app: tauri::AppHandle,
    frame_rate_cap: f64,
    smooth_frame_rate: bool,
    display_gamma: f64,
    allow_async_loading: bool,
    max_audio_channels: u32,
    unfocused_volume: f64,
    high_polling_mouse: bool,
    set_openal_audio: bool,
) -> Result<(), String> {
    refuse_engine_edit_while_running()?;
    let tweaks = ncp_host::config::EngineTweaks {
        frame_rate_cap,
        smooth_frame_rate,
        display_gamma,
        allow_async_loading,
        max_audio_channels,
        unfocused_volume,
        high_polling_mouse,
    };
    ncp_host::config::apply(&engine_ini(&app)?, &tweaks, set_openal_audio)
        .map_err(|e| e.to_string())
}

/// Save ONLY the editable knobs (frame-rate cap, smooth, gamma, async
/// loading, MaxChannels) into Engine.ini — no competitive baseline, no
/// audio-device override. Same backup-once semantics as apply, and the same
/// running-game refusal.
// Same flat-argument shape as apply_engine_config above: tauri deserializes
// command args individually, so a params struct would change the JS call site
// for no gain.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub fn save_engine_tweaks(
    app: tauri::AppHandle,
    frame_rate_cap: f64,
    smooth_frame_rate: bool,
    display_gamma: f64,
    allow_async_loading: bool,
    max_audio_channels: u32,
    unfocused_volume: f64,
    high_polling_mouse: bool,
) -> Result<(), String> {
    refuse_engine_edit_while_running()?;
    let tweaks = ncp_host::config::EngineTweaks {
        frame_rate_cap,
        smooth_frame_rate,
        display_gamma,
        allow_async_loading,
        max_audio_channels,
        unfocused_volume,
        high_polling_mouse,
    };
    ncp_host::config::save_tweaks(&engine_ini(&app)?, &tweaks).map_err(|e| e.to_string())
}

/// Restore Engine.ini from the launcher's `.ncpbak` backup. Same running-game
/// refusal as apply/save, for the same clobber reason.
#[tauri::command]
pub fn restore_engine_config(app: tauri::AppHandle) -> Result<(), String> {
    refuse_engine_edit_while_running()?;
    ncp_host::config::restore(&engine_ini(&app)?).map_err(|e| e.to_string())
}

/// Clear the read-only attribute on Engine.ini so the competitive config can be
/// applied. Opt-in (the UI offers it) — some players set it read-only on purpose.
#[tauri::command]
pub fn clear_engine_ini_readonly(app: tauri::AppHandle) -> Result<(), String> {
    ncp_host::config::clear_read_only(&engine_ini(&app)?).map_err(|e| e.to_string())
}

/// The active install's `Mod.ini` path — NetcodePlus reads it from the
/// `Saved/Config` directory that also holds the per-platform Engine.ini
/// folder, so it is derived from [`engine_ini`] (`…/Config/<Platform>NoEditor/
/// Engine.ini` → `…/Config/Mod.ini`) and works for both the Windows
/// Documents layout and the Linux in-prefix layout.
fn mod_ini(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let engine = engine_ini(app)?;
    let config_dir = engine
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "could not locate your Mod.ini".to_string())?;
    Ok(config_dir.join("Mod.ini"))
}

/// Serializable descriptor of a shipped Mod.ini preset for the UI.
#[derive(Debug, serde::Serialize)]
pub struct ModPresetInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub blurb: &'static str,
}

/// The shipped competitive Mod.ini presets, in display order.
#[tauri::command]
pub fn mod_preset_list() -> Vec<ModPresetInfo> {
    ncp_host::config::mod_presets()
        .iter()
        .map(|p| ModPresetInfo {
            id: p.id,
            label: p.label,
            blurb: p.blurb,
        })
        .collect()
}

/// Current Mod.ini state for the presets card.
#[tauri::command]
pub fn mod_config_state(app: tauri::AppHandle) -> Result<ncp_host::config::ModIniState, String> {
    Ok(ncp_host::config::read_mod_state(&mod_ini(&app)?))
}

/// Apply a shipped competitive Mod.ini preset (section merge; the pristine
/// original is backed up once). Refused while UT4 runs — the game rewrites
/// Mod.ini on exit and would clobber the preset.
#[tauri::command]
pub fn apply_mod_preset(app: tauri::AppHandle, preset_id: String) -> Result<(), String> {
    if shipping_client_running() {
        return Err("Close UT4 to apply a Mod.ini preset.".to_string());
    }
    ncp_host::config::apply_mod_preset(&mod_ini(&app)?, &preset_id).map_err(|e| e.to_string())
}

/// Current NetcodePlus join waits for the Settings card. A missing Mod.ini is
/// not an error — the defaults are then what is in force.
#[tauri::command]
pub fn join_wait_state(app: tauri::AppHandle) -> Result<ncp_host::config::JoinWaitState, String> {
    Ok(ncp_host::config::read_join_wait(&mod_ini(&app)?))
}

/// Save the two join waits into `[NetcodePlus]` in Mod.ini. Refused while UT4
/// runs for the same reason as a preset apply — the game rewrites Mod.ini on
/// exit and would clobber the values.
#[tauri::command]
pub fn save_join_wait(
    app: tauri::AppHandle,
    profile_wait_seconds: u32,
    no_signal_wait_seconds: u32,
) -> Result<(), String> {
    if shipping_client_running() {
        return Err("Close UT4 to change your join settings.".to_string());
    }
    let tweaks = ncp_host::config::JoinWaitTweaks {
        profile_wait_seconds,
        no_signal_wait_seconds,
    };
    ncp_host::config::save_join_wait(&mod_ini(&app)?, &tweaks).map_err(|e| e.to_string())
}

/// Restore Mod.ini from the launcher's `.ncpbak` backup. Same running-game
/// guard as apply, for the same clobber reason.
#[tauri::command]
pub fn restore_mod_config(app: tauri::AppHandle) -> Result<(), String> {
    if shipping_client_running() {
        return Err("Close UT4 to restore your Mod.ini.".to_string());
    }
    ncp_host::config::restore(&mod_ini(&app)?).map_err(|e| e.to_string())
}

/// Open an install's NetcodePlus plugin folder in the OS file manager. Only ever
/// opens a real *directory* under the given install root — never an arbitrary
/// webview path, and never a file, so there is no exec surface.
#[tauri::command]
pub fn reveal_netcodeplus_folder(app: tauri::AppHandle, root: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = ncp_host::netcodeplus_dir(Path::new(&root));
    if !dir.is_dir() {
        return Err("the NetcodePlus folder isn't there to open".to_string());
    }
    app.opener()
        .open_path(dir.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}

/// Open the UT4 `Plugins` folder for an install so the user can drop in a plugin
/// like UltiCross. Validates the root is a real UT4 install (never an arbitrary
/// webview path), creates the standard `Plugins` dir if a fresh install lacks one
/// (best-effort), and opens a directory only — never a file.
#[tauri::command]
pub fn reveal_plugins_folder(app: tauri::AppHandle, root: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let root_path = Path::new(&root);
    let dir = ncp_host::plugins_dir(root_path);
    // Guard: only operate on a real UT4 layout (the `<root>/UnrealTournament`
    // game dir must exist), so a bad root can't make us create folders in an
    // arbitrary location.
    let Some(game_dir) = dir.parent().filter(|p| p.is_dir()).map(|p| p.to_path_buf()) else {
        return Err("that doesn't look like a UT4 install".to_string());
    };
    // A fresh install may lack a Plugins folder; create it so there is a
    // destination. Best-effort — a Program Files install may need elevation, in
    // which case we fall back to opening the game folder above it.
    if !dir.is_dir() {
        let _ = std::fs::create_dir_all(&dir);
    }
    let target = if dir.is_dir() { dir } else { game_dir };
    app.opener()
        .open_path(target.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}

/// Open the `Engine\Binaries\Win64` folder for an install — where UT4-OpenAL's
/// shipping DLL goes (next to the engine binaries, not under `Plugins`). Opens a
/// real directory under the install root only, never a file.
#[tauri::command]
pub fn reveal_openal_folder(app: tauri::AppHandle, root: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = ncp_host::config::openal_dir(Path::new(&root));
    if !dir.is_dir() {
        return Err(
            "the Engine\\Binaries\\Win64 folder isn't there — is this a UT4 install?".to_string(),
        );
    }
    app.opener()
        .open_path(dir.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}

/// Verify and, if needed, repair the `[OnlineSubsystemMcp.*]` master-server
/// sections that a UT4 bug sometimes wipes. Returns whether a repair ran.
#[tauri::command]
pub fn repair_master_server(app: tauri::AppHandle) -> Result<bool, String> {
    ncp_host::config::repair_master_server(&engine_ini(&app)?).map_err(|e| e.to_string())
}
