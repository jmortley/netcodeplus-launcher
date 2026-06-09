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
    .map_err(pug_error)
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
pub async fn utpugs_action(action: String, mode: String, token: String) -> Result<String, String> {
    if token.trim().is_empty() {
        return Err(
            "Set your UTPugs launcher token first (run /launchertoken in the UTPugs Discord).".into(),
        );
    }
    check_autopug_mode(&mode)?;
    let url = autopug_url("/pug_action")?;
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

    Ok(HousekeepingResult {
        old_launcher_path: state.pending_old_launcher_path,
        current_version: current_version.to_string(),
        shortcut_needs_update: ncp_host::desktop_shortcut_is_stale(&current_path),
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

/// Clear the read-only attribute on Engine.ini so the competitive config can be
/// applied. Opt-in (the UI offers it) — some players set it read-only on purpose.
#[tauri::command]
pub fn clear_engine_ini_readonly() -> Result<(), String> {
    ncp_host::config::clear_read_only(&engine_ini()?).map_err(|e| e.to_string())
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
pub fn repair_master_server() -> Result<bool, String> {
    ncp_host::config::repair_master_server(&engine_ini()?).map_err(|e| e.to_string())
}
