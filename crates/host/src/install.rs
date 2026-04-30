//! UT4 install detection.
//!
//! UT4 ships as a stock UE4 game, so the actual layout on disk is:
//!
//! ```text
//! <install root>/
//!   Engine/
//!     Binaries/
//!       Win64/
//!         UE4-Win64-Shipping.exe        <- the executable
//!   UnrealTournament/
//!     Content/
//!       Paks/                            <- shipped game content (read-only)
//! ```
//!
//! Mod paks (downloaded community content, including ours) live
//! **outside** the install, under the user's Documents folder:
//!
//! ```text
//! %USERPROFILE%/Documents/UnrealTournament/Saved/Paks/DownloadedPaks/
//! ```
//!
//! [`detect`] walks a list of common install locations. [`check_install`]
//! validates a single picked path; it walks parent directories so a
//! user who picks the deeper `Engine/Binaries/Win64/` subfolder still
//! resolves cleanly to the install root.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, trace};

const GAME_NAME: &str = "UnrealTournament";

/// A validated UT4 install on the local machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UtInstall {
    /// The top-level install directory (the parent of `Engine/` and
    /// `UnrealTournament/`).
    pub root: PathBuf,

    /// Path to `Engine/Binaries/Win64/UE4-Win64-Shipping.exe`.
    pub executable: PathBuf,

    /// CLI arguments to pass to [`Self::executable`] when launching
    /// UT4. Epic-installed builds need
    /// `UnrealTournament -epicapp=UnrealTournamentDev -epicenv=Prod
    /// -EpicPortal`; non-Epic distributions may need a different set.
    pub launch_args: Vec<String>,

    /// `<root>/UnrealTournament/Content/Paks/` — the game's shipped
    /// content paks. Read-only; included so callers can sanity-check
    /// the install really has UT4 content.
    pub content_paks_dir: PathBuf,

    /// `~/Documents/UnrealTournament/Saved/Paks/DownloadedPaks/` —
    /// the directory the launcher writes mod paks into.
    ///
    /// Created on demand by the installer; not required to exist for
    /// the install to validate.
    pub mod_paks_dir: PathBuf,
}

/// Try to autodetect the UT4 install on this machine.
///
/// Returns `None` if no candidate path matches; the caller should
/// then prompt the user via a folder picker and pass the chosen
/// directory to [`check_install`].
#[must_use]
pub fn detect() -> Option<UtInstall> {
    let mod_paks_dir = default_mod_paks_dir()?;
    for path in candidate_roots() {
        trace!(path = %path.display(), "checking install candidate");
        if let Some(install) = check_install(&path, mod_paks_dir.clone()) {
            debug!(install = ?install, "detected UT4 install");
            return Some(install);
        }
    }
    None
}

/// Validate that `picked` (or one of its ancestors) is a UT4 install
/// root.
///
/// The walk-up means a user who picks `Engine/Binaries/Win64/` (the
/// folder containing the executable) gets resolved to the actual
/// install root rather than rejected.
///
/// `mod_paks_dir` is supplied by the caller because it depends on
/// the user's Documents folder, which doesn't fit the "pure
/// validation" framing of this function.
#[must_use]
pub fn check_install(picked: &Path, mod_paks_dir: PathBuf) -> Option<UtInstall> {
    let root = picked.ancestors().find(|p| has_ue4_shipping_exe(p))?;
    let executable = ue4_shipping_exe(root);
    let content_paks_dir = root.join(GAME_NAME).join("Content").join("Paks");
    if !content_paks_dir.is_dir() {
        // We found UE4-Win64-Shipping.exe but no UT-specific content
        // paks dir — so this is a UE4 install, but not UT4. Reject.
        return None;
    }
    Some(UtInstall {
        root: root.to_path_buf(),
        executable,
        launch_args: default_launch_args(),
        content_paks_dir,
        mod_paks_dir,
    })
}

/// `~/Documents/UnrealTournament/Saved/Paks/DownloadedPaks/`,
/// resolved via the OS's known-folders API. Returns `None` if the
/// Documents folder cannot be located (vanishingly rare on Windows;
/// possible on stripped-down Linux setups).
#[must_use]
pub fn default_mod_paks_dir() -> Option<PathBuf> {
    let docs = dirs::document_dir()?;
    Some(
        docs.join(GAME_NAME)
            .join("Saved")
            .join("Paks")
            .join("DownloadedPaks"),
    )
}

/// Default CLI arguments for an Epic-Launcher-installed UT4 build.
fn default_launch_args() -> Vec<String> {
    vec![
        GAME_NAME.to_string(),
        "-epicapp=UnrealTournamentDev".to_string(),
        "-epicenv=Prod".to_string(),
        "-EpicPortal".to_string(),
    ]
}

fn ue4_shipping_exe(root: &Path) -> PathBuf {
    root.join("Engine")
        .join("Binaries")
        .join("Win64")
        .join("UE4-Win64-Shipping.exe")
}

fn has_ue4_shipping_exe(root: &Path) -> bool {
    ue4_shipping_exe(root).is_file()
}

#[cfg(target_os = "windows")]
fn candidate_roots() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

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
