//! Errors for state-file I/O.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Result alias used by [`crate::state`].
pub type Result<T> = std::result::Result<T, StateError>;

/// Failure modes for reading or writing the persistent launcher
/// state file.
#[derive(Debug, Error)]
pub enum StateError {
    /// The provided path has no parent directory, so we cannot stage
    /// an atomic-replace temp file alongside it.
    #[error("state file path '{0}' has no parent directory")]
    NoParentDir(PathBuf),

    /// Filesystem error while reading or writing.
    #[error("state file I/O error: {0}")]
    Io(#[from] io::Error),

    /// The state file's bytes did not parse as valid JSON matching
    /// [`crate::state::LauncherState`].
    #[error("state file JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
}
