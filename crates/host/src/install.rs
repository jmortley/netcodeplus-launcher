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
    // Walk up to the nearest ancestor that is a COMPLETE UT4 install — one with
    // both the shipping exe AND the UT4 content-paks dir. Requiring both (rather
    // than stopping at the first ancestor that merely has the exe) means a
    // half-build in the way — e.g. a bare `WindowsServer\` that carries the exe
    // but no `UnrealTournament/Content/Paks` — doesn't shadow a real install
    // rooted higher up. A generic UE4 tree with no UT4 paks anywhere in the
    // ancestry still resolves to nothing, so "UE4 but not UT4" is still rejected.
    let root = picked.ancestors().find(|p| is_ut4_install_root(p))?;
    Some(UtInstall {
        root: root.to_path_buf(),
        executable: ue4_shipping_exe(root),
        launch_args: default_launch_args(),
        content_paks_dir: ut4_content_paks_dir(root),
        mod_paks_dir,
    })
}

/// The user's Downloads folder — the default location offered by the
/// game-installer download picker. A known user-writable spot, so users aren't
/// nudged toward a protected folder (where the un-elevated download is refused).
/// `None` if it can't be resolved.
#[must_use]
pub fn default_download_dir() -> Option<PathBuf> {
    dirs::download_dir()
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

// ===================================================================
// Play-install detection, driven by desktop shortcuts.
//
// Real players install UT4 via ut4ever.org or the Epic Games Launcher,
// both of which drop a desktop shortcut whose target is the *shipping*
// client (`UE4-Win64-Shipping.exe`). Editor / source trees are only
// ever launched through a `UE4Editor.exe` shortcut, so keying off the
// shortcut's target executable cleanly separates "the game the user
// plays" from developer/editor builds we don't care about — something
// directory probing alone cannot do, since a source checkout also
// contains a built shipping exe.
// ===================================================================

/// The shipping client executable — the marker of a *play* install
/// (as opposed to `UE4Editor.exe`, which marks an editor/source tree).
const SHIPPING_EXE: &str = "UE4-Win64-Shipping.exe";

/// Whether the NetcodePlus plugin is present and well-formed inside a
/// given UT4 install.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetcodePlusStatus {
    /// `…/Plugins/NetcodePlus/` exists with `NetcodePlus.uplugin` and
    /// `Binaries/` — the layout the engine actually loads.
    Installed,
    /// No `NetcodePlus` folder under the install's `Plugins/` dir.
    Missing,
    /// The folder exists but is incomplete — missing the `.uplugin` or
    /// `Binaries/`. Typically a half-extracted copy or one nested a
    /// level too deep (`Plugins/NetcodePlus/NetcodePlus/`).
    Malformed,
}

/// Canonical install location for the NetcodePlus plugin within a UT4
/// install root: `<root>/UnrealTournament/Plugins/NetcodePlus/`.
#[must_use]
pub fn netcodeplus_dir(root: &Path) -> PathBuf {
    root.join(GAME_NAME).join("Plugins").join("NetcodePlus")
}

/// The UT4 game `Plugins` directory under an install root
/// (`<root>/UnrealTournament/Plugins/`) — where drop-in plugin folders such as
/// UltiCross go.
#[must_use]
pub fn plugins_dir(root: &Path) -> PathBuf {
    root.join(GAME_NAME).join("Plugins")
}

/// Classify whether (and how) NetcodePlus is installed in `root`.
#[must_use]
pub fn netcodeplus_status(root: &Path) -> NetcodePlusStatus {
    let dir = netcodeplus_dir(root);
    if !dir.is_dir() {
        return NetcodePlusStatus::Missing;
    }
    let well_formed = dir.join("NetcodePlus.uplugin").is_file() && dir.join("Binaries").is_dir();
    if well_formed {
        NetcodePlusStatus::Installed
    } else {
        NetcodePlusStatus::Malformed
    }
}

/// True if the UltiCross crosshair plugin is installed under `root`.
///
/// Keyed on its compiled shipping module
/// (`<root>/UnrealTournament/Plugins/UltiCross/Binaries/Win64/UE4-UltiCross-Win64-Shipping.dll`)
/// — the load-bearing artifact the shipping client actually loads, the same
/// "is the shipping DLL present" signal used for UT4-OpenAL. UltiCross also
/// ships a content pak, but a stray empty plugin folder wouldn't carry this
/// DLL, so it is the most reliable single check. Used only to *recommend*
/// UltiCross to players who lack it; the launcher never installs or edits it.
#[must_use]
pub fn ulticross_installed(root: &Path) -> bool {
    root.join(GAME_NAME)
        .join("Plugins")
        .join("UltiCross")
        .join("Binaries")
        .join("Win64")
        .join("UE4-UltiCross-Win64-Shipping.dll")
        .is_file()
}

/// How a [`DetectedInstall`] was found. Surfaced in the UI because
/// "found via your desktop shortcut" carries very different confidence
/// from a directory guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectSource {
    /// Resolved from desktop shortcut(s) — the preferred, high-confidence
    /// signal. The individual shortcut names live in
    /// [`DetectedInstall::profiles`].
    DesktopShortcut,
    /// Found by probing common install directories (fallback when no
    /// shortcut points at a shipping client).
    Probe,
    /// Resolved from a Lutris game config (Linux) — high confidence: it carries
    /// the exact exe, prefix, and Wine runner the user configured.
    Lutris,
    /// Supplied directly by the user through the folder picker.
    Manual,
}

/// A named launch configuration for an install — typically one per
/// desktop-shortcut variant (e.g. plain, `UU`, `dx12`), so the user can
/// pick which set of launch args to run with rather than the launcher
/// guessing. Probe/manual installs get a single `"Default"` profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchProfile {
    /// Display name — the shortcut's name, or `"Default"`.
    pub label: String,
    /// Full launch args: program name + flags, e.g.
    /// `["UnrealTournament", "-epicapp=...", "-EpicPortal"]`.
    pub args: Vec<String>,
}

/// A detected play install plus its provenance, NetcodePlus status, and
/// the launch profiles available for it — the unit the UI renders so the
/// user can confirm "yes, that's my game" and pick how to launch it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedInstall {
    /// The validated UT4 install.
    pub install: UtInstall,
    /// How we found it.
    pub source: DetectSource,
    /// Whether NetcodePlus is correctly installed in this install.
    pub netcodeplus: NetcodePlusStatus,
    /// Launch profiles (one per distinct shortcut variant, or a single
    /// `"Default"` for probe/manual). Sorted by label.
    pub profiles: Vec<LaunchProfile>,
}

/// Resolve a desktop-shortcut *target* into a play install, or `None`.
///
/// Returns `None` for anything that is not the shipping client — most
/// importantly `UE4Editor.exe`, which is how editor/source trees (a
/// developer's checkout, an Epic editor install) are launched. The
/// shortcut's own `args` become the install's launch args, so the exact
/// variant the user runs (`-dx12`, etc.) is preserved rather than
/// guessed at.
#[must_use]
pub fn play_install_from_shortcut(
    target: &Path,
    args: &str,
    mod_paks_dir: PathBuf,
) -> Option<UtInstall> {
    let is_shipping = target
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case(SHIPPING_EXE));
    if !is_shipping {
        trace!(target = %target.display(), "shortcut target is not the shipping client; skipping");
        return None;
    }
    // `check_install` walks up from the exe to the install root and
    // confirms the UT4 content-paks dir exists.
    let mut install = check_install(target, mod_paks_dir)?;
    let parsed: Vec<String> = args.split_whitespace().map(String::from).collect();
    if !parsed.is_empty() {
        install.launch_args = parsed;
    }
    Some(install)
}

/// Enumerate every UT4 *play* install on this machine.
///
/// Desktop shortcuts (resolved to a shipping client) are the primary,
/// high-confidence signal — they are how players actually launch the
/// game, and they exclude editor/source trees, which only ever have a
/// `UE4Editor.exe` shortcut. Directory probing is a fallback used **only
/// when no shortcut resolves**, so a developer box (with a built shipping
/// exe inside a source checkout) doesn't get that tree mistaken for a
/// play install.
///
/// Installs are grouped by root: multiple shortcuts for one install
/// (`…`, `… UU`, `… dx12`) become one entry with one [`LaunchProfile`]
/// each, so the user picks which to launch with instead of the launcher
/// guessing.
#[must_use]
pub fn detect_installs() -> Vec<DetectedInstall> {
    let Some(mod_paks_dir) = default_mod_paks_dir() else {
        return Vec::new();
    };

    // Each hit is one (install, launch-profile) pair.
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut hits: Vec<(UtInstall, LaunchProfile)> = Vec::new();

    #[cfg(windows)]
    hits.extend(shortcuts::detect(&mod_paks_dir));

    if !hits.is_empty() {
        return group_by_root(hits, DetectSource::DesktopShortcut);
    }

    // On Linux the high-confidence signal is the Lutris game config (exe + prefix
    // + runner), not desktop shortcuts. Resolve those before falling back to the
    // (empty, on non-Windows) directory probe.
    #[cfg(not(windows))]
    {
        hits.extend(crate::linux::detect_lutris_hits(&mod_paks_dir));
        if !hits.is_empty() {
            return group_by_root(hits, DetectSource::Lutris);
        }
    }

    // Fall back to probing ONLY when no shortcut pointed at a play
    // install. Never mix probing in alongside shortcuts, or a source
    // tree's shipping build would surface next to the real install.
    for path in candidate_roots() {
        if let Some(install) = check_install(&path, mod_paks_dir.clone()) {
            let profile = LaunchProfile {
                label: "Default".to_string(),
                args: install.launch_args.clone(),
            };
            hits.push((install, profile));
        }
    }
    group_by_root(hits, DetectSource::Probe)
}

/// Group `(install, profile)` hits into one [`DetectedInstall`] per root,
/// accumulating distinct launch profiles. A single UT4 install commonly
/// has several shortcuts pointing at it — they collapse to one entry with
/// one profile per distinct arg set.
#[must_use]
fn group_by_root(
    hits: Vec<(UtInstall, LaunchProfile)>,
    source: DetectSource,
) -> Vec<DetectedInstall> {
    let mut out: Vec<DetectedInstall> = Vec::new();
    for (install, profile) in hits {
        match out.iter_mut().find(|d| d.install.root == install.root) {
            Some(existing) => {
                if !existing.profiles.iter().any(|p| p.args == profile.args) {
                    existing.profiles.push(profile);
                }
            }
            None => {
                let netcodeplus = netcodeplus_status(&install.root);
                out.push(DetectedInstall {
                    install,
                    source,
                    netcodeplus,
                    profiles: vec![profile],
                });
            }
        }
    }
    // Stable profile order for the UI.
    for d in &mut out {
        d.profiles.sort_by(|a, b| a.label.cmp(&b.label));
    }
    out
}

/// Windows desktop-shortcut resolution (`.lnk` parsing via the pure-Rust
/// `lnk` crate — no `unsafe`, no COM).
#[cfg(windows)]
mod shortcuts {
    use std::path::{Path, PathBuf};

    use lnk::encoding::WINDOWS_1252;
    use lnk::ShellLink;
    use tracing::{debug, trace};

    use super::{play_install_from_shortcut, LaunchProfile, UtInstall};

    /// Desktop directories to scan: the user's Desktop (resolved via the
    /// known-folders API, so OneDrive redirection is handled) plus the
    /// all-users Public desktop.
    fn desktop_dirs() -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(d) = dirs::desktop_dir() {
            dirs.push(d);
        }
        if let Ok(public) = std::env::var("PUBLIC") {
            dirs.push(PathBuf::from(public).join("Desktop"));
        }
        dirs.sort();
        dirs.dedup();
        dirs
    }

    /// Parse one `.lnk` into `(label, target, args)`. `None` if it can't
    /// be parsed or carries no resolvable target path.
    fn resolve(path: &Path) -> Option<(String, PathBuf, String)> {
        let link = match ShellLink::open(path, WINDOWS_1252) {
            Ok(l) => l,
            Err(e) => {
                trace!(path = %path.display(), error = %e, "skipping unparseable .lnk");
                return None;
            }
        };
        let target = link.link_target()?;
        let args = link
            .string_data()
            .command_line_arguments()
            .clone()
            .unwrap_or_default();
        let label = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("shortcut")
            .to_string();
        Some((label, PathBuf::from(target), args))
    }

    /// Resolve every desktop shortcut that points at a UT4 shipping
    /// client into an `(install, launch-profile)` pair. The shortcut's
    /// name becomes the profile label and its args the profile args.
    pub(super) fn detect(mod_paks_dir: &Path) -> Vec<(UtInstall, LaunchProfile)> {
        let mut out = Vec::new();
        for dir in desktop_dirs() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let p = entry.path();
                let is_lnk = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("lnk"));
                if !is_lnk {
                    continue;
                }
                let Some((label, target, args)) = resolve(&p) else {
                    continue;
                };
                if let Some(install) =
                    play_install_from_shortcut(&target, &args, mod_paks_dir.to_path_buf())
                {
                    debug!(root = %install.root.display(), label = %label, "play install from shortcut");
                    let profile = LaunchProfile {
                        label,
                        args: install.launch_args.clone(),
                    };
                    out.push((install, profile));
                }
            }
        }
        out
    }
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

/// `<root>/UnrealTournament/Content/Paks/` — the shipped game content.
/// `pub(crate)` so [`crate::stray`] can scan it for misplaced content paks.
pub(crate) fn ut4_content_paks_dir(root: &Path) -> PathBuf {
    root.join(GAME_NAME).join("Content").join("Paks")
}

/// A complete UT4 *play* install root: the shipping exe under `Engine/Binaries/`
/// AND the UT4 content-paks dir. The paks check is what separates UT4 from a
/// bare UE4 build (or a half-extracted / server tree that has the exe but none
/// of the shipped content).
fn is_ut4_install_root(root: &Path) -> bool {
    has_ue4_shipping_exe(root) && ut4_content_paks_dir(root).is_dir()
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

// Unit tests live in the library test binary (`ncp_host-*.exe`), not the
// `install` integration binary — which matters on Windows, where UAC's
// installer-detection heuristic refuses to launch any `install*.exe`.
#[cfg(test)]
mod detect_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn fake_mod_paks() -> PathBuf {
        PathBuf::from("C:/fake/Documents/UnrealTournament/Saved/Paks/DownloadedPaks")
    }

    /// Lay down a minimal shipping UT4 install at `root`.
    fn build_play_install(root: &Path) {
        let win64 = root.join("Engine").join("Binaries").join("Win64");
        fs::create_dir_all(&win64).unwrap();
        fs::write(win64.join(SHIPPING_EXE), b"fake exe").unwrap();
        fs::create_dir_all(root.join(GAME_NAME).join("Content").join("Paks")).unwrap();
    }

    fn shipping_target(root: &Path) -> PathBuf {
        root.join("Engine")
            .join("Binaries")
            .join("Win64")
            .join(SHIPPING_EXE)
    }

    #[test]
    fn shortcut_to_shipping_client_resolves_with_its_own_args() {
        let tmp = TempDir::new().unwrap();
        build_play_install(tmp.path());

        let install = play_install_from_shortcut(
            &shipping_target(tmp.path()),
            "UnrealTournament -epicapp=UnrealTournamentDev -epicenv=Prod -EpicPortal -dx12",
            fake_mod_paks(),
        )
        .expect("shipping client should resolve to a play install");

        assert_eq!(install.root, tmp.path());
        assert_eq!(
            install.launch_args.first().map(String::as_str),
            Some("UnrealTournament")
        );
        assert!(
            install.launch_args.contains(&"-dx12".to_string()),
            "should preserve the shortcut's own args: {:?}",
            install.launch_args
        );
    }

    #[test]
    fn shortcut_to_editor_is_rejected() {
        // A UE4Editor.exe shortcut (dev/source tree) must never be taken
        // as a play install, even when the surrounding layout is valid.
        let tmp = TempDir::new().unwrap();
        build_play_install(tmp.path());
        let editor = tmp
            .path()
            .join("Engine")
            .join("Binaries")
            .join("Win64")
            .join("UE4Editor.exe");
        fs::write(&editor, b"fake editor").unwrap();

        assert!(
            play_install_from_shortcut(&editor, "UnrealTournament -log", fake_mod_paks()).is_none(),
            "editor target must not resolve to a play install"
        );
    }

    #[test]
    fn check_install_climbs_past_a_half_build_to_the_real_root() {
        // A complete UT4 install with a half-build nested inside it: a bare
        // `WindowsServer\` that has the shipping exe but NO Content/Paks. Picking
        // the half-build (or anything under it) must resolve UP to the complete
        // root, not stop at the half-build and reject.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        build_play_install(root);

        let nested = root.join("WindowsServer");
        let win64 = nested.join("Engine").join("Binaries").join("Win64");
        fs::create_dir_all(&win64).unwrap();
        fs::write(win64.join(SHIPPING_EXE), b"fake exe").unwrap();

        let install =
            check_install(&nested, fake_mod_paks()).expect("should climb to the complete root");
        assert_eq!(install.root, root);
    }

    #[test]
    fn check_install_rejects_a_lone_half_build() {
        // Exe present but no UT4 content-paks anywhere in the ancestry → this is
        // a UE4 build, not UT4. Still rejected (the climb finds no complete root).
        let tmp = TempDir::new().unwrap();
        let win64 = tmp.path().join("Engine").join("Binaries").join("Win64");
        fs::create_dir_all(&win64).unwrap();
        fs::write(win64.join(SHIPPING_EXE), b"fake exe").unwrap();
        assert!(check_install(tmp.path(), fake_mod_paks()).is_none());
    }

    #[test]
    fn netcodeplus_status_reports_missing_malformed_and_installed() {
        let tmp = TempDir::new().unwrap();
        build_play_install(tmp.path());

        // Missing: no plugin folder yet.
        assert_eq!(netcodeplus_status(tmp.path()), NetcodePlusStatus::Missing);

        // Malformed: folder exists but lacks the .uplugin / Binaries.
        let dir = netcodeplus_dir(tmp.path());
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(netcodeplus_status(tmp.path()), NetcodePlusStatus::Malformed);

        // Installed: add the .uplugin and Binaries/.
        fs::write(dir.join("NetcodePlus.uplugin"), b"{}").unwrap();
        fs::create_dir_all(dir.join("Binaries")).unwrap();
        assert_eq!(netcodeplus_status(tmp.path()), NetcodePlusStatus::Installed);
    }

    #[test]
    fn ulticross_installed_detects_the_shipping_dll() {
        let tmp = TempDir::new().unwrap();
        build_play_install(tmp.path());

        // Absent until the shipping DLL exists.
        assert!(!ulticross_installed(tmp.path()));

        // A bare plugin folder (no shipping DLL) must NOT count as installed.
        let win64 = tmp
            .path()
            .join(GAME_NAME)
            .join("Plugins")
            .join("UltiCross")
            .join("Binaries")
            .join("Win64");
        fs::create_dir_all(&win64).unwrap();
        assert!(!ulticross_installed(tmp.path()));

        // Present once the shipping module DLL is in place.
        fs::write(win64.join("UE4-UltiCross-Win64-Shipping.dll"), b"fake dll").unwrap();
        assert!(ulticross_installed(tmp.path()));
    }

    #[test]
    fn group_by_root_collapses_and_collects_distinct_profiles() {
        let tmp = TempDir::new().unwrap();
        build_play_install(tmp.path());
        let install = check_install(&shipping_target(tmp.path()), fake_mod_paks()).unwrap();

        // Three shortcuts, same root: two distinct arg sets + one dup.
        let plain = LaunchProfile {
            label: "Unreal Tournament 4".into(),
            args: vec!["UnrealTournament".into(), "-EpicPortal".into()],
        };
        let dx12 = LaunchProfile {
            label: "Unreal Tournament 4 dx12".into(),
            args: vec![
                "UnrealTournament".into(),
                "-EpicPortal".into(),
                "-dx12".into(),
            ],
        };
        let plain_dup = LaunchProfile {
            label: "UT4 copy".into(),
            args: plain.args.clone(),
        };

        let grouped = group_by_root(
            vec![
                (install.clone(), plain),
                (install.clone(), dx12),
                (install, plain_dup),
            ],
            DetectSource::DesktopShortcut,
        );

        assert_eq!(grouped.len(), 1, "one root => one entry");
        let profiles = &grouped[0].profiles;
        assert_eq!(profiles.len(), 2, "duplicate arg set deduped: {profiles:?}");
        // Sorted by label: "Unreal Tournament 4" before "…dx12".
        assert_eq!(profiles[0].label, "Unreal Tournament 4");
        assert!(profiles[1].args.contains(&"-dx12".to_string()));
    }

    #[test]
    fn detect_installs_does_not_panic() {
        // Machine-dependent result; must always return cleanly.
        let _ = detect_installs();
    }
}
