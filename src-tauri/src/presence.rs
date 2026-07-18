//! Discord Rich Presence — on by default (Settings toggle to disable), presence-only.
//!
//! Broadcasts the launcher user's PUG state (idle / in queue / live) to their
//! Discord profile via the local Discord client's IPC socket. No network calls,
//! no Game SDK — a pure-Rust IPC client. The frontend is the source of truth for
//! state and pushes updates via [`set_discord_presence`] on each transition; this
//! module just translates them into Discord activity payloads and owns the single
//! IPC connection.
//!
//! Everything here is best-effort and fails silently: if Discord isn't running or
//! the IPC handshake fails, the commands no-op so the launcher UI is never
//! affected. The connection reconnects lazily on the next push.
//!
//! Privacy: disabled by default. The enabled flag is persisted in the launcher
//! state file ([`set_discord_presence_enabled`]); turning it off clears and drops
//! the activity immediately.

use std::sync::{Mutex, OnceLock};

use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};
use serde::Deserialize;

/// Public Discord **Application (client) ID** for the launcher. NOT a secret — it
/// identifies the app whose uploaded art assets (`np_logo`, `mode_ictf`, …) the
/// presence references. Set in the Discord Developer Portal
/// (<https://discord.com/developers>). A value of `"0"` makes presence a silent
/// no-op (see [`ensure_connected`]).
const DISCORD_APP_ID: &str = "1513302712911921365";

/// Large image asset key uploaded in the Discord Developer Portal.
const LARGE_IMAGE: &str = "np_logo";
const LARGE_TEXT: &str = "UT4 · NetcodePlus";

struct Presence {
    enabled: bool,
    client: Option<DiscordIpcClient>,
}

static PRESENCE: OnceLock<Mutex<Presence>> = OnceLock::new();

fn cell() -> &'static Mutex<Presence> {
    PRESENCE.get_or_init(|| {
        Mutex::new(Presence {
            enabled: false,
            client: None,
        })
    })
}

/// Activity payload from the frontend — mirrors the launcher's PUG state machine.
#[derive(Debug, Default, Deserialize)]
pub struct ActivityInput {
    /// "idle" | "queued" | "live" | "spectating" (unknown → idle).
    pub kind: String,
    #[serde(default)]
    pub mode: Option<String>,
    /// Free-text detail line, e.g. the live server "NYC1 — CTF-Face".
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub players: Option<u32>,
    #[serde(default)]
    pub max_players: Option<u32>,
    /// Community name (e.g. "UTPugs", "Instagib Nation") appended to the details
    /// line so the profile shows WHICH community's queue you're in. Optional —
    /// absent renders exactly as before.
    #[serde(default)]
    pub community: Option<String>,
    /// Epoch seconds the current activity started, for Discord's elapsed timer.
    #[serde(default)]
    pub started_unix: Option<i64>,
}

/// Human label for a mode key ("ictf" → "iCTF"). Accepts the UTPugs queue keys
/// (wipe/elim/duel) alongside the iCTF/legacy ones.
fn mode_label(mode: Option<&str>) -> String {
    match mode.map(str::to_ascii_lowercase).as_deref() {
        Some("ictf") => "iCTF".to_string(),
        Some("ctf") => "CTF".to_string(),
        Some("duel") => "Duel".to_string(),
        Some("wipe" | "wipeout" | "carnage") => "Wipeout".to_string(),
        Some("elim" | "elimination") => "Elimination".to_string(),
        Some("elimplus") => "Elim+".to_string(),
        Some("blitz") => "Blitz".to_string(),
        Some(other) if !other.is_empty() => other.to_uppercase(),
        _ => "PUG".to_string(),
    }
}

/// Small image asset key for a mode ("ictf" → "mode_ictf"). The UTPugs short keys
/// are canonicalised onto the uploaded asset names (wipe → `mode_wipeout`, elim →
/// `mode_elimplus`) so the icon resolves; an unknown mode just yields no icon.
fn mode_asset(mode: Option<&str>) -> String {
    let canon = match mode.map(str::to_ascii_lowercase).as_deref() {
        Some("wipe" | "carnage") => Some("wipeout".to_string()),
        Some("elim") => Some("elimplus".to_string()),
        Some(m) if !m.is_empty() => Some(m.to_string()),
        _ => None,
    };
    match canon {
        Some(m) => format!("mode_{m}"),
        None => "idle".to_string(),
    }
}

/// Connect lazily; returns false (and leaves `client` None) if presence isn't
/// configured or Discord is unreachable.
fn ensure_connected(p: &mut Presence) -> bool {
    if DISCORD_APP_ID == "0" {
        return false; // not configured yet — see DISCORD_APP_ID docs
    }
    if p.client.is_none() {
        if let Ok(mut client) = DiscordIpcClient::new(DISCORD_APP_ID) {
            if client.connect().is_ok() {
                p.client = Some(client);
            }
        }
    }
    p.client.is_some()
}

/// Push a new activity to Discord. No-op when disabled or Discord is unreachable.
#[tauri::command]
pub fn set_discord_presence(input: ActivityInput) {
    let mut guard = match cell().lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if !guard.enabled || !ensure_connected(&mut guard) {
        return;
    }

    // Build owned strings first so they outlive the borrowing Activity.
    let label = mode_label(input.mode.as_deref());
    let small_image = mode_asset(input.mode.as_deref());
    // " · UTPugs" / " · Instagib Nation" suffix on the details line, when a
    // community was supplied — so the profile shows which queue you're in.
    let community_suffix = input
        .community
        .as_deref()
        .filter(|c| !c.is_empty())
        .map(|c| format!(" · {c}"))
        .unwrap_or_default();
    let (details, state_line, small_text): (String, String, String) = match input.kind.as_str() {
        "queued" => (
            format!("{label} PUG{community_suffix}"),
            format!(
                "In queue — {}/{}",
                input.players.unwrap_or(0),
                input.max_players.unwrap_or(10)
            ),
            label.clone(),
        ),
        "live" => (
            format!("Playing {label}{community_suffix}"),
            input.detail.clone().unwrap_or_default(),
            label.clone(),
        ),
        "spectating" => (
            "Spectating a PUG".to_string(),
            input.detail.clone().unwrap_or_default(),
            "Spectating".to_string(),
        ),
        _ => (
            "In the launcher".to_string(),
            String::new(),
            "Idle".to_string(),
        ),
    };

    let mut act = activity::Activity::new();
    if !details.is_empty() {
        act = act.details(&details);
    }
    if !state_line.is_empty() {
        act = act.state(&state_line);
    }

    let mut assets = activity::Assets::new()
        .large_image(LARGE_IMAGE)
        .large_text(LARGE_TEXT);
    if !small_image.is_empty() {
        assets = assets.small_image(&small_image).small_text(&small_text);
    }
    act = act.assets(assets);

    // Party size renders "(6 of 10)" only with both an id and a size.
    if let (Some(cur), Some(max)) = (input.players, input.max_players) {
        if max > 0 {
            act = act.party(
                activity::Party::new()
                    .id("ncp-pug")
                    .size([cur as i32, max as i32]),
            );
        }
    }

    if let Some(ts) = input.started_unix {
        act = act.timestamps(activity::Timestamps::new().start(ts));
    }

    // One public button on every state — turns the "In queue" card into a one-click
    // download funnel (and feeds SmartScreen reputation + public-visibility signals).
    // Both args are &'static str, so no borrow-lifetime conflict with the owned
    // detail/state strings above. Discord hides your OWN buttons from your self-view —
    // they render only on others' view of your profile, so test with a second account.
    // Max 2 buttons; we ship one. Never put a "Watch live" button here with a PUG
    // password — RP buttons are public to all profile viewers and would leak it.
    act = act.buttons(vec![activity::Button::new(
        "Get the Launcher",
        "https://github.com/jmortley/netcodeplus-launcher/releases/latest",
    )]);

    // A failed push usually means Discord went away — drop the client so the next
    // call reconnects.
    if let Some(client) = guard.client.as_mut() {
        if client.set_activity(act).is_err() {
            let _ = client.close();
            guard.client = None;
        }
    }
}

/// Enable/disable presence and persist the choice to the launcher state file.
/// Disabling clears + drops any active presence immediately.
#[tauri::command]
pub fn set_discord_presence_enabled(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    {
        let mut guard = cell().lock().map_err(|_| "presence lock poisoned")?;
        guard.enabled = enabled;
        if !enabled {
            if let Some(mut client) = guard.client.take() {
                let _ = client.clear_activity();
                let _ = client.close();
            }
        }
    }

    // Persist alongside the rest of the launcher state (mirrors save_launcher_token).
    let path = crate::commands::state_path(&app)?;
    let mut state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    state.discord_presence_enabled = enabled;
    ncp_host::state::write(&path, &state).map_err(|e| e.to_string())
}
