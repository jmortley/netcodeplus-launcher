//! UT4 install detection.
//!
//! [`detect`] tries a list of well-known candidate paths on the
//! current OS and returns the first one whose layout looks like a
//! real UT4 install. [`check_install`] validates a single candidate
//! and is the right entry point after a folder picker.
//!
//! Validation is intentionally narrow: a path counts as a UT4 install
//! iff `<root>/UnrealTournament/Binaries/Win64/UnrealTournament.exe`
//! exists *and* `<root>/UnrealTournament/Content/Paks/` exists. Both
//! are required because the launcher needs the pak directory to
//! install into and the executable to launch.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, trace};

/// A validated UT4 install on the local machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UtInstall {
    /// The top-level directory containing `Engine/`, `UnrealTournament/`, etc.
    pub root: PathBuf,
    /// Path to `UnrealTournament.exe` — the launch target.
    pub executable: PathBuf,
    /// Directory the launcher writes pak files into.
    pub paks_dir: PathBuf,
}

/// Try to autodetect the UT4 install on this machine.
///
/// Returns `None` if no candidate path matches; the caller should
/// then prompt the user via a folder picker and pass the chosen
/// directory to [`check_install`].
#[must_use]
pub fn detect() -> Option<UtInstall> {
    for path in candidate_roots() {
        trace!(path = %path.display(), "checking install candidate");
        if let Some(install) = check_install(&path) {
            debug!(install = ?install, "detected UT4 install");
            return Some(install);
        }
    }
    None
}

/// Check whether `root` looks like a UT4 install.
///
/// Returns the populated [`UtInstall`] if both the executable and the
/// paks directory exist, otherwise `None`. Symlinks are followed
/// (via [`std::path::Path::is_file`] / [`std::path::Path::is_dir`]).
#[must_use]
pub fn check_install(root: &Path) -> Option<UtInstall> {
    let executable = root
        .join("UnrealTournament")
        .join("Binaries")
        .join("Win64")
        .join("UnrealTournament.exe");
    let paks_dir = root.join("UnrealTournament").join("Content").join("Paks");
    if executable.is_file() && paks_dir.is_dir() {
        Some(UtInstall {
            root: root.to_path_buf(),
            executable,
            paks_dir,
        })
    } else {
        None
    }
}

#[cfg(target_os = "windows")]
fn candidate_roots() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    // Env-var-anchored locations (user's actual ProgramFiles etc.).
    let env_anchored: &[(&str, &[&str])] = &[
        ("ProgramFiles", &["Epic Games", "UnrealTournament"]),
        ("ProgramFiles(x86)", &["Epic Games", "UnrealTournament"]),
        ("ProgramFiles", &["UnrealTournament"]),
        ("LOCALAPPDATA", &["UnrealTournament"]),
    ];
    for (env_var, sub) in env_anchored {
        if let Ok(base) = std::env::var(env_var) {
            let mut p = PathBuf::from(base);
            for s in *sub {
                p.push(s);
            }
            candidates.push(p);
        }
    }

    // Drive-root locations — common manual-extract destinations for
    // ut4ever-distributed builds.
    for drive in ["C:", "D:", "E:", "F:"] {
        candidates.push(PathBuf::from(format!("{drive}\\UnrealTournament")));
        candidates.push(PathBuf::from(format!("{drive}\\Games\\UnrealTournament")));
    }

    candidates
}

#[cfg(not(target_os = "windows"))]
fn candidate_roots() -> Vec<PathBuf> {
    // Linux/Proton support is post-v1; non-Windows hosts get an empty
    // candidate list, so [`detect`] falls back to the picker.
    Vec::new()
}
