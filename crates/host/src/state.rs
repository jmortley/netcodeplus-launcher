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

use std::collections::HashSet;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use ncp_planner::LocalInstall;
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

    /// Highest manifest `sequence` the launcher has ever accepted.
    /// Persisted so a later run rejects any manifest carrying a lower
    /// sequence — an active-attacker replay/downgrade of an older,
    /// still-signed, still-unexpired manifest. Starts at 0 (first run
    /// accepts any sequence).
    #[serde(default)]
    pub highest_manifest_sequence: u64,
}

fn default_channel() -> String {
    DEFAULT_CHANNEL.to_string()
}

impl Default for LauncherState {
    fn default() -> Self {
        Self {
            install_path: None,
            channel: default_channel(),
            local_install: LocalInstall::default(),
            opted_out: HashSet::new(),
            launch_profile_label: None,
            launch_priority: Priority::default(),
            affinity_mask_hex: None,
            ut4stats_playerid: None,
            ut4stats_playername: None,
            launcher_token: None,
            ut4_username: None,
            ut4_display_name: None,
            highest_manifest_sequence: 0,
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
    }
}
