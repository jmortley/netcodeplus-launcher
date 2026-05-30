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
    executable: String,
    args: Vec<String>,
    priority: String,
    affinity_mask_hex: Option<String>,
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
    ncp_host::launch(Path::new(&executable), &args, &opts).map_err(|e| e.to_string())
}

/// Path to the persistent launcher state file in the per-app config dir.
fn state_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
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
) -> Result<(), String> {
    let path = state_path(&app)?;
    let mut state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    state.install_path = install_path.map(PathBuf::from);
    state.launch_profile_label = profile_label;
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

/// UT4IGBot launcher PUG endpoint (FastAPI on :9999).
const BOT_PUG_URL: &str = "http://ut4stats.com:9999/launcher_pug_action";

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

/// Apply the competitive Engine.ini baseline plus the editable knobs,
/// merging into the existing ini (backing it up first).
#[tauri::command]
pub fn apply_engine_config(
    frame_rate_cap: f64,
    smooth_frame_rate: bool,
    display_gamma: f64,
    set_openal_audio: bool,
) -> Result<(), String> {
    let tweaks = ncp_host::config::EngineTweaks {
        frame_rate_cap,
        smooth_frame_rate,
        display_gamma,
    };
    ncp_host::config::apply(&engine_ini()?, &tweaks, set_openal_audio).map_err(|e| e.to_string())
}

/// Restore Engine.ini from the launcher's `.ncpbak` backup.
#[tauri::command]
pub fn restore_engine_config() -> Result<(), String> {
    ncp_host::config::restore(&engine_ini()?).map_err(|e| e.to_string())
}
