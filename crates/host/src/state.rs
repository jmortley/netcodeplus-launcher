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
