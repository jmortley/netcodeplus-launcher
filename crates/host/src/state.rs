//! Persistent launcher state file.
//!
//! Stores everything the launcher needs to remember between runs:
//! which UT4 install to talk to, which channel the user is on, what
//! paks are currently installed, and which optional paks the user has
//! opted out of.
//!
//! Writes are atomic: bytes go to a sibling temp file first, then
//! [`tempfile::NamedTempFile::persist`] does a rename-over-existing
//! that the OS treats as atomic on the same filesystem. This means a
//! crash mid-write either leaves the old state file intact or
//! replaces it with the new one — never a half-written hybrid.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use ncp_planner::{InstalledPlugin, LocalInstall};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use tracing::debug;

use crate::error::{Result, StateError};
use crate::launch::Priority;

/// Default channel name selected on first run.
pub const DEFAULT_CHANNEL: &str = "stable";

/// Persistent launcher state.
///
/// Every field has a sensible default so a missing or partial state
/// file degrades to "first run" behaviour rather than failing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LauncherState {
    /// Detected or user-picked UT4 install root. `None` means the
    /// launcher has never identified a working install — UI should
    /// drive a folder picker.
    #[serde(default)]
    pub install_path: Option<PathBuf>,

    /// Channel name from the manifest the user is currently on.
    #[serde(default = "default_channel")]
    pub channel: String,

    /// Snapshot of paks currently on disk (populated after a
    /// successful install run).
    #[serde(default)]
    pub local_install: LocalInstall,

    /// Per-pak install fingerprints (size + mtime), keyed by pak id and paired
    /// with [`Self::local_install`]. Lets a status check notice an installed pak
    /// that was overwritten in place (a server redirect pushing a different
    /// version) without re-hashing it — see [`PakStamp`].
    #[serde(default)]
    pub pak_stamps: HashMap<String, PakStamp>,

    /// Optional pak ids the user has opted out of.
    #[serde(default)]
    pub opted_out: HashSet<String>,

    /// Display label of the launch profile the user last chose, so it
    /// can be re-selected on the next run.
    #[serde(default)]
    pub launch_profile_label: Option<String>,

    /// Process priority to launch the game with.
    #[serde(default)]
    pub launch_priority: Priority,

    /// CPU affinity as an uppercase hex string (no `0x`). `None` or empty
    /// = all cores. Stored as a string, not `u64`, so masks wider than
    /// 2^53 survive the JSON ↔ JS-number round-trip in the webview.
    #[serde(default)]
    pub affinity_mask_hex: Option<String>,

    /// What the launcher window does once a game launches: `"minimize"`
    /// (default), `"close"` (quit the launcher entirely), or `"none"` (stay
    /// open). The game is spawned detached, so closing the launcher never kills
    /// the running game.
    #[serde(default = "default_window_action")]
    pub launch_window_action: String,

    /// ut4stats.com player id the user linked (one-time), for fetching
    /// their stats panel. `None` = not linked.
    #[serde(default)]
    pub ut4stats_playerid: Option<String>,

    /// Display name of the linked ut4stats player (cached for the UI so
    /// it can show "linked as X" without a network round-trip).
    #[serde(default)]
    pub ut4stats_playername: Option<String>,

    /// Per-user PUG token issued by the bot's `/launchertoken` command and
    /// pasted in once. Sent as the `launcher-token` header so the bot can
    /// resolve the player and queue them. `None` = not set.
    #[serde(default)]
    pub launcher_token: Option<String>,

    /// Per-user PUG token for the UTPugs community (autopug). PUG tokens are
    /// **per-community** (each community's bot only knows its own players), so
    /// UTPugs gets its own token slot independent of [`Self::launcher_token`]
    /// (the IGBot/iCTF one). `None` = not linked.
    #[serde(default)]
    pub utpugs_launcher_token: Option<String>,

    /// Whether Discord Rich Presence is enabled (opt-in; default off). When on,
    /// the launcher broadcasts the user's PUG state to their Discord profile.
    /// See `src-tauri/src/presence.rs`.
    #[serde(default)]
    pub discord_presence_enabled: bool,

    /// UT4 master-server account username the user logged in with, used as the
    /// `-AUTH_LOGIN` value at launch and for the UI. NOT a secret — the refresh
    /// token (the secret) lives in the OS credential store, never here. `None` =
    /// not logged in.
    #[serde(default)]
    pub ut4_username: Option<String>,

    /// Display name of the logged-in UT4 account, cached for the UI. `None` =
    /// not logged in.
    #[serde(default)]
    pub ut4_display_name: Option<String>,

    /// Canonical UT4 account id (the master server's `account.ID`), used as the
    /// game's `-epicuserid` launch arg so it boots straight into this account
    /// instead of the account picker. NOT a secret. `None` until the user logs
    /// in with a launcher build that captures it (pre-capture sessions stay
    /// `None`, and the launch path then omits the arg).
    #[serde(default)]
    pub ut4_account_id: Option<String>,

    /// Recorded NetcodePlus plugin install state, **keyed by UT4 install root
    /// path** (the `DetectedInstall.install.root` string). A player can have
    /// several installs that drift to different plugin builds, so each is
    /// tracked independently. Written when the launcher installs the plugin
    /// into a given root; read back to decide up-to-date vs needs-update. The
    /// on-disk `.uplugin` has no reliable version, so this is the version
    /// source of truth.
    #[serde(default)]
    pub installed_plugins: HashMap<String, InstalledPlugin>,

    /// Highest manifest `sequence` the launcher has ever accepted.
    /// Persisted so a later run rejects any manifest carrying a lower
    /// sequence — an active-attacker replay/downgrade of an older,
    /// still-signed, still-unexpired manifest. Starts at 0 (first run
    /// accepts any sequence).
    #[serde(default)]
    pub highest_manifest_sequence: u64,

    /// Absolute path of the launcher exe that last recorded itself as the
    /// installed build (from `std::env::current_exe()`), paired with
    /// [`Self::installed_launcher_version`]. Used to detect a post-update move:
    /// when a launcher of a *higher* version runs from a *different* path, the
    /// recorded path is the now-outdated copy. `None` until a build with this
    /// tracking has run once — so the cleanup prompt is forward-looking and
    /// cannot point at a pre-tracking old build.
    #[serde(default)]
    pub installed_launcher_path: Option<String>,

    /// Version (`CARGO_PKG_VERSION`) recorded with [`Self::installed_launcher_path`].
    #[serde(default)]
    pub installed_launcher_version: Option<String>,

    /// Path of a previous, now-outdated launcher exe the user may want to
    /// delete, set when a post-update move is detected. Surfaced by the
    /// housekeeping UI and cleared once the user removes it or dismisses the
    /// prompt. The delete command only ever removes THIS backend-recorded path,
    /// never one supplied by the webview.
    #[serde(default)]
    pub pending_old_launcher_path: Option<String>,
}

/// Cheap install fingerprint of one pak: its size + mtime at the moment the
/// launcher installed or adopted it.
///
/// Compared — never re-hashed — on each status check to notice an installed pak
/// that was overwritten in place (a server's redirect pushing a different
/// version), so the launcher can restore the manifest's version. A metadata stat
/// is used rather than a content hash specifically so a OneDrive cloud-only
/// placeholder is not hydrated (which a full re-hash would force).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PakStamp {
    /// File size in bytes at install/adopt time.
    pub size_bytes: u64,
    /// File mtime as whole milliseconds since the Unix epoch at install/adopt.
    pub mtime_ms: u64,
}

fn default_channel() -> String {
    DEFAULT_CHANNEL.to_string()
}

fn default_window_action() -> String {
    "minimize".to_string()
}

impl Default for LauncherState {
    fn default() -> Self {
        Self {
            install_path: None,
            channel: default_channel(),
            local_install: LocalInstall::default(),
            pak_stamps: HashMap::new(),
            opted_out: HashSet::new(),
            launch_profile_label: None,
            launch_priority: Priority::default(),
            affinity_mask_hex: None,
            launch_window_action: default_window_action(),
            ut4stats_playerid: None,
            ut4stats_playername: None,
            launcher_token: None,
            utpugs_launcher_token: None,
            discord_presence_enabled: false,
            ut4_username: None,
            ut4_display_name: None,
            ut4_account_id: None,
            installed_plugins: HashMap::new(),
            highest_manifest_sequence: 0,
            installed_launcher_path: None,
            installed_launcher_version: None,
            pending_old_launcher_path: None,
        }
    }
}

impl LauncherState {
    /// Advance the manifest replay floor to `sequence` when it exceeds the
    /// highest previously accepted, returning `true` if the floor moved.
    /// Call after a manifest verifies, then persist the state, so a later
    /// run rejects any lower-sequence (replayed) manifest. A no-op for
    /// equal or lower values.
    pub fn record_manifest_sequence(&mut self, sequence: u64) -> bool {
        if sequence > self.highest_manifest_sequence {
            self.highest_manifest_sequence = sequence;
            true
        } else {
            false
        }
    }
}

/// Read state from `path`.
///
/// Returns `Ok(None)` if the file does not exist (legitimate first
/// run). Other I/O errors and JSON parse errors are surfaced.
///
/// # Errors
///
/// - [`StateError::Io`] for filesystem errors other than `NotFound`.
/// - [`StateError::Json`] if the file exists but is not valid JSON
///   matching [`LauncherState`].
pub fn read(path: &Path) -> Result<Option<LauncherState>> {
    match fs::read(path) {
        Ok(bytes) => {
            let state: LauncherState = serde_json::from_slice(&bytes)?;
            debug!(path = %path.display(), "loaded state file");
            Ok(Some(state))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {
            debug!(path = %path.display(), "no state file present (first run)");
            Ok(None)
        }
        Err(e) => Err(StateError::Io(e)),
    }
}

/// Write `state` to `path` atomically.
///
/// Writes JSON to a sibling temp file in the same directory, fsyncs
/// it, then renames over `path`. If `path`'s parent directory does
/// not yet exist it is created (recursively).
///
/// # Errors
///
/// - [`StateError::NoParentDir`] if `path` has no parent component.
/// - [`StateError::Io`] for filesystem errors at any step.
/// - [`StateError::Json`] if `state` cannot be serialised (essentially
///   never happens for these types but is surfaced for completeness).
pub fn write(path: &Path, state: &LauncherState) -> Result<()> {
    let json = serde_json::to_vec_pretty(state)?;
    let parent = path
        .parent()
        .ok_or_else(|| StateError::NoParentDir(path.to_path_buf()))?;
    fs::create_dir_all(parent)?;

    let mut tmp = NamedTempFile::new_in(parent)?;
    tmp.write_all(&json)?;
    tmp.as_file_mut().sync_all()?;
    tmp.persist(path).map_err(|e| StateError::Io(e.error))?;

    debug!(
        path = %path.display(),
        bytes = json.len(),
        "wrote state file atomically"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_manifest_sequence_advances_only_upward() {
        let mut state = LauncherState::default();
        assert_eq!(state.highest_manifest_sequence, 0);
        assert!(state.record_manifest_sequence(5));
        assert_eq!(state.highest_manifest_sequence, 5);
        // Equal does not advance.
        assert!(!state.record_manifest_sequence(5));
        assert_eq!(state.highest_manifest_sequence, 5);
        // Lower (a replay attempt) does not advance.
        assert!(!state.record_manifest_sequence(3));
        assert_eq!(state.highest_manifest_sequence, 5);
        // Higher advances the floor.
        assert!(state.record_manifest_sequence(6));
        assert_eq!(state.highest_manifest_sequence, 6);
    }

    #[test]
    fn highest_manifest_sequence_defaults_to_zero_for_old_state_files() {
        // State files written before this field existed must still load,
        // with the floor defaulting to 0.
        let state: LauncherState = serde_json::from_str(r#"{"channel":"stable"}"#).unwrap();
        assert_eq!(state.highest_manifest_sequence, 0);
        assert_eq!(state.installed_launcher_path, None);
        assert_eq!(state.installed_launcher_version, None);
        assert_eq!(state.pending_old_launcher_path, None);
    }
}
