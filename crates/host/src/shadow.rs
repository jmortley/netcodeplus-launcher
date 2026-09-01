//! Pre-launch UT4AC shadow-module detection.
//!
//! Field incident (2026-08-31): a player's canonical
//! `UnrealTournament/Plugins/UT4AC/Binaries/Win64` DLLs matched a working
//! install byte-for-byte, yet the game died at boot with
//! `??0IUT4ACClientTelemetryProvider@@QEAA@XZ — Entry Point Not Found`. An
//! ancient `UE4-UT4AC-Win64-Shipping.dll` under a **different** UT4
//! installation's `Engine\Binaries\Win64` satisfied the client module's
//! import first, and the stale module lacks the newer export. Updating the
//! canonical plugin folder could never repair that machine.
//!
//! Why these locations can win over the canonical plugin folder:
//!
//! * `Engine\Binaries\Win64` is the folder of `UE4-Win64-Shipping.exe`
//!   itself — the **application directory**, the first place the Windows
//!   loader looks when it resolves a DLL's imports by bare name. A UT4AC
//!   module there beats every later search location unconditionally.
//! * `UnrealTournament\Binaries\Win64` and plugin `Binaries` folders are
//!   scanned by UE4's `FModuleManager` when it maps module names to DLL
//!   paths; a same-named module in a scanned folder can be bound instead of
//!   the canonical one.
//! * `Engine\Plugins\**` is mounted by the engine in addition to the game's
//!   `Plugins` folder (the same double-load surface [`crate::stray`] guards
//!   for NetcodePlus).
//! * Directories on `PATH` are the loader's final fallback, and the
//!   launcher's child process inherits the launcher's `PATH`.
//!
//! [`scan_ut4ac_shadows`] enumerates exactly those locations for the
//! selected install — plus the same fixed locations of every **other** UT4
//! installation the launcher knows about (`candidate_roots` +
//! shortcut-detected installs) — and classifies every hit against the
//! canonical plugin DLLs by SHA-256. It never walks arbitrary drives.
//! [`sanitize_child_path`] strips other installations' directories out of
//! the `PATH` handed to the game, so a selected D: install cannot inherit
//! DLL search paths from an old C: install. [`remove_ut4ac_shadow`] deletes
//! one flagged file with the same recompute-don't-trust gate
//! [`crate::stray::remove_stray`] uses: the webview's echoed path is only an
//! index into a fresh scan, never a free-form delete target.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The two UT4AC shipping modules, by exact basename. `UT4AC` is the
/// server/shared module that exports the telemetry-provider interface;
/// `UT4ACClient` imports it — which is why a stale `UT4AC` copy anywhere on
/// the search path kills the canonical client DLL at bind time.
pub const UT4AC_MODULE_DLLS: [&str; 2] = [
    "UE4-UT4AC-Win64-Shipping.dll",
    "UE4-UT4ACClient-Win64-Shipping.dll",
];

/// Where a duplicate UT4AC module was found, in decreasing order of how
/// directly it can win DLL resolution for the launched game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShadowLocation {
    /// `<root>\Engine\Binaries\Win64` — the game exe's own folder. The
    /// loader's first search location; beats the canonical plugin folder
    /// unconditionally.
    EngineBinaries,
    /// `<root>\UnrealTournament\Binaries\Win64` — scanned by the engine's
    /// module search ahead of plugin folders.
    ProjectBinaries,
    /// Anywhere under `<root>\Engine\Plugins` — mounted by the engine in
    /// addition to the game `Plugins` folder.
    EnginePluginTree,
    /// Anywhere under `<root>\UnrealTournament\Plugins` other than the
    /// canonical `UT4AC\Binaries\Win64` files themselves (a copy inside
    /// another plugin's folder, a renamed/backup plugin copy, …).
    GamePluginTree,
    /// A directory on the `PATH` the game will inherit. The loader's final
    /// fallback for bare-name imports.
    PathEntry,
    /// A `PATH` directory that [`sanitize_child_path`] removes from the
    /// child environment because it belongs to a different known UT4
    /// install — reported so the user can see what was neutralised.
    StrippedPathEntry,
    /// A fixed location of a **different** UT4 installation known to the
    /// launcher. Cannot influence this launch (its directories are stripped
    /// from the child `PATH`), but launching that installation will hit the
    /// same crash until it is repaired.
    OtherInstall,
}

/// How a found module compares to the canonical plugin DLL of the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShadowVerdict {
    /// Byte-identical to the canonical DLL (same SHA-256). Harmless today,
    /// but it still wins resolution and turns stale on the next update.
    Identical,
    /// Differs from the canonical DLL — the exact incident shape.
    Stale,
    /// The canonical DLL is missing or unreadable, so the copy cannot be
    /// compared. Treated like [`ShadowVerdict::Stale`] for blocking.
    Unknown,
}

/// One duplicate UT4AC module, fully described for the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowFinding {
    /// The module basename (one of [`UT4AC_MODULE_DLLS`], original casing
    /// as found on disk).
    pub basename: String,
    /// Absolute path as found.
    pub path: PathBuf,
    /// Resolved (canonicalised) path, when the OS could resolve it —
    /// otherwise a copy of `path`. Shown so junction/subst tricks can't
    /// disguise a location.
    pub resolved_path: PathBuf,
    pub location: ShadowLocation,
    pub verdict: ShadowVerdict,
    /// SHA-256 of the found file (empty when unreadable).
    pub sha256: String,
    /// File size in bytes (0 when unreadable).
    pub size: u64,
    /// Modification time as seconds since the Unix epoch, when available.
    /// The UI formats it; keeping it numeric avoids a date dependency here.
    pub modified_unix: Option<u64>,
    /// Plain-English explanation of why this copy can shadow (or, for
    /// non-blocking locations, why it is still worth fixing).
    pub reason: String,
    /// True when this copy can still win DLL resolution for the launched
    /// game and is not byte-identical — launch must refuse.
    pub blocks_launch: bool,
}

/// Errors from [`remove_ut4ac_shadow`].
#[derive(Debug)]
pub enum ShadowRemoveError {
    /// The path is not among the findings a fresh scan produces right now —
    /// a stale panel, a repaired file, or a tampered payload. Nothing was
    /// deleted.
    NotAShadow,
    /// The finding (or a directory on the way to it) is a symlink/junction —
    /// refused, mirroring [`crate::stray`]'s reparse-point rule.
    UnsafePath,
    /// Filesystem error from the delete itself (most commonly
    /// access-denied under `Program Files`).
    Io(std::io::Error),
}

impl std::fmt::Display for ShadowRemoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAShadow => {
                write!(f, "that file is not one the current shadow scan flags")
            }
            Self::UnsafePath => write!(f, "refused: the path crosses a symlink/junction"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<std::io::Error> for ShadowRemoveError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Case-folded, canonicalised comparison key for a path. Canonicalisation
/// resolves junctions/symlinks and drive-letter casing; the `\\?\` prefix and
/// trailing separators are stripped so keys compare cleanly. Falls back to
/// the lexical path when the file does not exist (still useful for prefix
/// tests against roots that do).
fn norm_key(p: &Path) -> String {
    let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = canon.to_string_lossy().to_ascii_lowercase();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    s.trim_end_matches(['\\', '/']).to_string()
}

fn same_path(a: &Path, b: &Path) -> bool {
    norm_key(a) == norm_key(b)
}

/// True when `key` (a [`norm_key`]) lies at or under the directory whose
/// norm key is `root_key`.
fn key_under(key: &str, root_key: &str) -> bool {
    key == root_key
        || (key.starts_with(root_key)
            && matches!(key.as_bytes().get(root_key.len()), Some(b'\\') | Some(b'/')))
}

fn engine_binaries_dir(root: &Path) -> PathBuf {
    root.join("Engine").join("Binaries").join("Win64")
}

fn project_binaries_dir(root: &Path) -> PathBuf {
    root.join("UnrealTournament").join("Binaries").join("Win64")
}

/// `<root>/UnrealTournament/Plugins/UT4AC/Binaries/Win64` — where the two
/// canonical DLLs live.
#[must_use]
pub fn ut4ac_canonical_binaries_dir(root: &Path) -> PathBuf {
    crate::plugin_install::ut4ac_dir(root)
        .join("Binaries")
        .join("Win64")
}

fn is_ut4ac_module_name(name: &OsStr) -> Option<&'static str> {
    let name = name.to_string_lossy();
    UT4AC_MODULE_DLLS
        .iter()
        .find(|dll| name.eq_ignore_ascii_case(dll))
        .copied()
}

/// SHA-256 of each canonical DLL under `root`, by [`UT4AC_MODULE_DLLS`]
/// index. `None` = missing/unreadable (verdicts become
/// [`ShadowVerdict::Unknown`]).
fn canonical_shas(root: &Path) -> [Option<String>; 2] {
    let dir = ut4ac_canonical_binaries_dir(root);
    let sha = |dll: &str| crate::plugin_install::file_sha256_hex(&dir.join(dll)).ok();
    [sha(UT4AC_MODULE_DLLS[0]), sha(UT4AC_MODULE_DLLS[1])]
}

/// Other UT4 installations this launcher can already see: the shortcut-scan
/// detections plus the fixed candidate roots ([`crate::install`]), validated
/// as complete play installs, minus `selected_root`. This is the bounded
/// "installations known to the launcher" set — no drive walking.
#[must_use]
pub fn known_other_roots(selected_root: &Path) -> Vec<PathBuf> {
    let selected = norm_key(selected_root);
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(selected);
    let mut out = Vec::new();
    let mut push = |root: PathBuf| {
        if seen.insert(norm_key(&root)) {
            out.push(root);
        }
    };
    for detected in crate::install::detect_installs() {
        push(detected.install.root);
    }
    for candidate in crate::install::candidate_roots() {
        if crate::install::is_ut4_install_root(&candidate) {
            push(candidate);
        }
    }
    out
}

/// Build one finding for the module file at `path`, comparing it against the
/// canonical SHA for its basename. Returns `None` for the canonical file
/// itself.
fn build_finding(
    path: &Path,
    canonical_dir: &Path,
    canonical: &[Option<String>; 2],
    location: ShadowLocation,
    reason: &str,
) -> Option<ShadowFinding> {
    let basename = path.file_name()?;
    let dll = is_ut4ac_module_name(basename)?;
    if same_path(path, &canonical_dir.join(dll)) {
        return None;
    }
    let (sha256, size, modified_unix) = match std::fs::metadata(path) {
        Ok(meta) => (
            crate::plugin_install::file_sha256_hex(path).unwrap_or_default(),
            meta.len(),
            meta.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs()),
        ),
        Err(_) => (String::new(), 0, None),
    };
    let canonical_sha = &canonical[usize::from(dll == UT4AC_MODULE_DLLS[1])];
    let verdict = match canonical_sha {
        Some(c) if !sha256.is_empty() && *c == sha256 => ShadowVerdict::Identical,
        Some(_) => ShadowVerdict::Stale,
        None => ShadowVerdict::Unknown,
    };
    let shadow_capable = !matches!(
        location,
        ShadowLocation::OtherInstall | ShadowLocation::StrippedPathEntry
    );
    Some(ShadowFinding {
        basename: basename.to_string_lossy().into_owned(),
        path: path.to_path_buf(),
        resolved_path: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        location,
        verdict,
        sha256,
        size,
        modified_unix,
        reason: reason.to_string(),
        blocks_launch: shadow_capable && verdict != ShadowVerdict::Identical,
    })
}

/// Check one directory (non-recursively) for the two module basenames.
fn check_dir(
    out: &mut Vec<ShadowFinding>,
    dir: &Path,
    canonical_dir: &Path,
    canonical: &[Option<String>; 2],
    location: ShadowLocation,
    reason: &str,
) {
    for dll in UT4AC_MODULE_DLLS {
        let path = dir.join(dll);
        if path.is_file() {
            out.extend(build_finding(
                &path,
                canonical_dir,
                canonical,
                location,
                reason,
            ));
        }
    }
}

/// Walk a plugin tree looking for the module basenames at any depth (bounded;
/// never through symlinks/junctions). Plugin trees are shallow in shipping
/// installs, so the depth cap is generous headroom, not a real limit.
fn walk_for_modules(
    out: &mut Vec<ShadowFinding>,
    tree: &Path,
    canonical_dir: &Path,
    canonical: &[Option<String>; 2],
    location: ShadowLocation,
    reason: &str,
) {
    const MAX_DEPTH: usize = 6;
    let mut stack: Vec<(PathBuf, usize)> = vec![(tree.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if file_type.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
            } else if is_ut4ac_module_name(&entry.file_name()).is_some() {
                out.extend(build_finding(
                    &path,
                    canonical_dir,
                    canonical,
                    location,
                    reason,
                ));
            }
        }
    }
}

/// The `PATH` entries that [`sanitize_child_path`] would drop for this
/// selection: entries at or under any known **other** UT4 install root.
fn split_path_entries(
    path_env: &OsStr,
    other_root_keys: &[String],
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut kept = Vec::new();
    let mut stripped = Vec::new();
    for entry in std::env::split_paths(path_env) {
        if entry.as_os_str().is_empty() {
            continue;
        }
        let key = norm_key(&entry);
        if other_root_keys.iter().any(|rk| key_under(&key, rk)) {
            stripped.push(entry);
        } else {
            kept.push(entry);
        }
    }
    (kept, stripped)
}

/// Scan every DLL-search location the launched game will actually receive —
/// plus the fixed locations of other known installs — for duplicate UT4AC
/// modules. `other_roots` is typically [`known_other_roots`]; `path_env` is
/// the `PATH` the child will inherit (the launcher's own).
#[must_use]
pub fn scan_ut4ac_shadows(
    root: &Path,
    other_roots: &[PathBuf],
    path_env: Option<&OsStr>,
) -> Vec<ShadowFinding> {
    let canonical_dir = ut4ac_canonical_binaries_dir(root);
    let canonical = canonical_shas(root);
    let mut out = Vec::new();

    check_dir(
        &mut out,
        &engine_binaries_dir(root),
        &canonical_dir,
        &canonical,
        ShadowLocation::EngineBinaries,
        "sits in the game executable's own folder — Windows resolves DLL imports \
         there before the real plugin folder",
    );
    check_dir(
        &mut out,
        &project_binaries_dir(root),
        &canonical_dir,
        &canonical,
        ShadowLocation::ProjectBinaries,
        "sits in UnrealTournament\\Binaries\\Win64, which the engine's module \
         search reads before plugin folders",
    );
    walk_for_modules(
        &mut out,
        &root.join("Engine").join("Plugins"),
        &canonical_dir,
        &canonical,
        ShadowLocation::EnginePluginTree,
        "a copy under Engine\\Plugins — the engine mounts plugins from there in \
         addition to the real install",
    );
    walk_for_modules(
        &mut out,
        &crate::install::plugins_dir(root),
        &canonical_dir,
        &canonical,
        ShadowLocation::GamePluginTree,
        "a copy outside the real UT4AC plugin folder — the engine's module scan \
         can bind it instead of the installed one",
    );

    let other_root_keys: Vec<String> = other_roots.iter().map(|r| norm_key(r)).collect();
    if let Some(path_env) = path_env {
        let (kept, stripped) = split_path_entries(path_env, &other_root_keys);
        for entry in kept {
            check_dir(
                &mut out,
                &entry,
                &canonical_dir,
                &canonical,
                ShadowLocation::PathEntry,
                "its folder is on PATH, which the game inherits — the loader falls \
                 back to PATH when resolving module imports",
            );
        }
        for entry in stripped {
            check_dir(
                &mut out,
                &entry,
                &canonical_dir,
                &canonical,
                ShadowLocation::StrippedPathEntry,
                "was on PATH via another UT4 installation; the launcher now strips \
                 that folder from the game's PATH at launch",
            );
        }
    }

    for other in other_roots {
        let reason = format!(
            "found in a different UT4 installation ({}) — it cannot affect this \
             launch, but launching that installation will crash until it is removed",
            other.display()
        );
        check_dir(
            &mut out,
            &engine_binaries_dir(other),
            &canonical_dir,
            &canonical,
            ShadowLocation::OtherInstall,
            &reason,
        );
        check_dir(
            &mut out,
            &project_binaries_dir(other),
            &canonical_dir,
            &canonical,
            ShadowLocation::OtherInstall,
            &reason,
        );
    }

    out
}

/// Rewrite the child `PATH`: drop every entry at or under a known **other**
/// UT4 install root, so the selected installation cannot inherit DLL search
/// paths from an old one. Entries under the selected root, and unrelated
/// entries, are kept in order. Returns `None` when nothing changed (callers
/// then leave the inherited environment alone).
#[must_use]
pub fn sanitize_child_path(
    path_env: &OsStr,
    selected_root: &Path,
    other_roots: &[PathBuf],
) -> Option<OsString> {
    let selected = norm_key(selected_root);
    let other_root_keys: Vec<String> = other_roots
        .iter()
        .map(|r| norm_key(r))
        .filter(|k| *k != selected)
        .collect();
    let (kept, stripped) = split_path_entries(path_env, &other_root_keys);
    if stripped.is_empty() {
        return None;
    }
    std::env::join_paths(kept).ok()
}

/// Delete one flagged duplicate. `target` must match (path-normalised) a
/// finding of a **fresh** scan with the same inputs — the recompute gate that
/// keeps a tampered webview payload from deleting anything else. Read-only
/// attributes are cleared on a first access-denied and the delete retried
/// once; a persistent denial (Program Files without elevation) surfaces as
/// [`ShadowRemoveError::Io`] so the UI can fall back to revealing the file.
///
/// # Errors
/// [`ShadowRemoveError::NotAShadow`] when the path is not currently flagged,
/// [`ShadowRemoveError::UnsafePath`] for symlinked targets, or
/// [`ShadowRemoveError::Io`] from the filesystem.
pub fn remove_ut4ac_shadow(
    root: &Path,
    other_roots: &[PathBuf],
    path_env: Option<&OsStr>,
    target: &Path,
) -> Result<(), ShadowRemoveError> {
    let findings = scan_ut4ac_shadows(root, other_roots, path_env);
    let finding = findings
        .iter()
        .find(|f| same_path(&f.path, target))
        .ok_or(ShadowRemoveError::NotAShadow)?;
    match std::fs::symlink_metadata(&finding.path) {
        Ok(meta) if meta.file_type().is_symlink() => return Err(ShadowRemoveError::UnsafePath),
        Ok(_) => {}
        Err(e) => return Err(ShadowRemoveError::Io(e)),
    }
    // When the finding lies under a known install root, refuse if anything
    // between that root and the file is a reparse point — same rule as stray
    // removal, so a junction can't redirect the delete.
    let governing_root = std::iter::once(root)
        .chain(other_roots.iter().map(PathBuf::as_path))
        .find(|r| key_under(&norm_key(&finding.path), &norm_key(r)));
    if let Some(governing_root) = governing_root {
        if crate::stray::path_crosses_reparse_point(governing_root, &finding.path) {
            return Err(ShadowRemoveError::UnsafePath);
        }
    }
    match std::fs::remove_file(&finding.path) {
        Ok(()) => Ok(()),
        #[cfg(windows)]
        #[allow(clippy::permissions_set_readonly_false)] // Windows attribute bit, not Unix mode bits
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            // On Windows the FILE_ATTRIBUTE_READONLY bit (old backup tools set
            // it) also reports PermissionDenied on delete; clear it and retry
            // once before giving up. Windows-only: on Unix, delete rights live
            // on the directory, and clearing readonly would only make the file
            // world-writable (clippy::permissions_set_readonly_false).
            if let Ok(meta) = std::fs::metadata(&finding.path) {
                let mut perms = meta.permissions();
                if perms.readonly() {
                    perms.set_readonly(false);
                    if std::fs::set_permissions(&finding.path, perms).is_ok() {
                        return std::fs::remove_file(&finding.path).map_err(ShadowRemoveError::Io);
                    }
                }
            }
            Err(ShadowRemoveError::Io(e))
        }
        Err(e) => Err(ShadowRemoveError::Io(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    const CANON_A: &[u8] = b"canonical UT4AC bytes";
    const CANON_B: &[u8] = b"canonical UT4ACClient bytes";

    /// Lay down a minimal complete play install with canonical UT4AC DLLs.
    fn make_root(dir: &Path) {
        let win64 = engine_binaries_dir(dir);
        fs::create_dir_all(&win64).unwrap();
        fs::write(win64.join("UE4-Win64-Shipping.exe"), b"exe").unwrap();
        fs::create_dir_all(dir.join("UnrealTournament").join("Content").join("Paks")).unwrap();
        let canonical = ut4ac_canonical_binaries_dir(dir);
        fs::create_dir_all(&canonical).unwrap();
        fs::write(canonical.join(UT4AC_MODULE_DLLS[0]), CANON_A).unwrap();
        fs::write(canonical.join(UT4AC_MODULE_DLLS[1]), CANON_B).unwrap();
    }

    #[test]
    fn clean_root_has_no_findings() {
        let tmp = TempDir::new().unwrap();
        make_root(tmp.path());
        assert!(scan_ut4ac_shadows(tmp.path(), &[], None).is_empty());
    }

    #[test]
    fn stale_copy_in_engine_binaries_blocks() {
        let tmp = TempDir::new().unwrap();
        make_root(tmp.path());
        let stale = engine_binaries_dir(tmp.path()).join(UT4AC_MODULE_DLLS[0]);
        fs::write(&stale, b"ancient bytes").unwrap();
        let findings = scan_ut4ac_shadows(tmp.path(), &[], None);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].location, ShadowLocation::EngineBinaries);
        assert_eq!(findings[0].verdict, ShadowVerdict::Stale);
        assert!(findings[0].blocks_launch);
        assert!(!findings[0].sha256.is_empty());
        assert_eq!(findings[0].size, b"ancient bytes".len() as u64);
    }

    #[test]
    fn identical_copy_reported_but_does_not_block() {
        let tmp = TempDir::new().unwrap();
        make_root(tmp.path());
        fs::write(
            engine_binaries_dir(tmp.path()).join(UT4AC_MODULE_DLLS[1]),
            CANON_B,
        )
        .unwrap();
        let findings = scan_ut4ac_shadows(tmp.path(), &[], None);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].verdict, ShadowVerdict::Identical);
        assert!(!findings[0].blocks_launch);
    }

    #[test]
    fn missing_canonical_makes_copy_unknown_and_blocking() {
        let tmp = TempDir::new().unwrap();
        make_root(tmp.path());
        fs::remove_file(ut4ac_canonical_binaries_dir(tmp.path()).join(UT4AC_MODULE_DLLS[0]))
            .unwrap();
        let project = project_binaries_dir(tmp.path());
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join(UT4AC_MODULE_DLLS[0]), b"whatever").unwrap();
        let findings = scan_ut4ac_shadows(tmp.path(), &[], None);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].location, ShadowLocation::ProjectBinaries);
        assert_eq!(findings[0].verdict, ShadowVerdict::Unknown);
        assert!(findings[0].blocks_launch);
    }

    #[test]
    fn engine_plugin_tree_copy_found_at_depth() {
        let tmp = TempDir::new().unwrap();
        make_root(tmp.path());
        let nested = tmp
            .path()
            .join("Engine")
            .join("Plugins")
            .join("UT4AC")
            .join("Binaries")
            .join("Win64");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join(UT4AC_MODULE_DLLS[0]), b"old engine-plugins copy").unwrap();
        let findings = scan_ut4ac_shadows(tmp.path(), &[], None);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].location, ShadowLocation::EnginePluginTree);
        assert!(findings[0].blocks_launch);
    }

    #[test]
    fn game_plugin_tree_copy_found_and_canonical_files_skipped() {
        let tmp = TempDir::new().unwrap();
        make_root(tmp.path());
        let inside_other = crate::install::plugins_dir(tmp.path())
            .join("NetcodePlus")
            .join("Binaries")
            .join("Win64");
        fs::create_dir_all(&inside_other).unwrap();
        fs::write(inside_other.join(UT4AC_MODULE_DLLS[1]), b"misplaced").unwrap();
        let findings = scan_ut4ac_shadows(tmp.path(), &[], None);
        // Only the misplaced copy — never the two canonical files.
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].location, ShadowLocation::GamePluginTree);
    }

    #[test]
    fn path_entry_blocks_and_other_install_entry_reports_as_stripped() {
        let tmp = TempDir::new().unwrap();
        make_root(tmp.path());
        let other = TempDir::new().unwrap();
        make_root(other.path());
        let other_engine = engine_binaries_dir(other.path());
        fs::write(other_engine.join(UT4AC_MODULE_DLLS[0]), b"ancient").unwrap();
        let tools = TempDir::new().unwrap();
        fs::write(tools.path().join(UT4AC_MODULE_DLLS[0]), b"tools copy").unwrap();

        let path_env =
            std::env::join_paths([tools.path().to_path_buf(), other_engine.clone()]).unwrap();
        let others = vec![other.path().to_path_buf()];
        let findings = scan_ut4ac_shadows(tmp.path(), &others, Some(&path_env));

        let tools_hit = findings
            .iter()
            .find(|f| f.location == ShadowLocation::PathEntry)
            .expect("tools PATH entry flagged");
        assert!(tools_hit.blocks_launch);
        let stripped_hit = findings
            .iter()
            .find(|f| f.location == ShadowLocation::StrippedPathEntry)
            .expect("other-install PATH entry flagged as stripped");
        assert!(!stripped_hit.blocks_launch);
        // The same file also shows as an OtherInstall fixed-location finding.
        assert!(findings
            .iter()
            .any(|f| f.location == ShadowLocation::OtherInstall && !f.blocks_launch));
    }

    #[test]
    fn sanitize_drops_other_root_entries_and_keeps_the_rest() {
        let tmp = TempDir::new().unwrap();
        make_root(tmp.path());
        let other = TempDir::new().unwrap();
        make_root(other.path());
        let unrelated = TempDir::new().unwrap();

        let original = std::env::join_paths([
            unrelated.path().to_path_buf(),
            engine_binaries_dir(other.path()),
            engine_binaries_dir(tmp.path()),
        ])
        .unwrap();
        let sanitized =
            sanitize_child_path(&original, tmp.path(), &[other.path().to_path_buf()])
                .expect("one entry must be stripped");
        let kept: Vec<PathBuf> = std::env::split_paths(&sanitized).collect();
        assert_eq!(
            kept,
            vec![
                unrelated.path().to_path_buf(),
                engine_binaries_dir(tmp.path())
            ]
        );

        // Nothing to strip -> None (leave the environment alone).
        let clean = std::env::join_paths([unrelated.path().to_path_buf()]).unwrap();
        assert!(sanitize_child_path(&clean, tmp.path(), &[other.path().to_path_buf()]).is_none());
    }

    #[test]
    fn remove_deletes_only_current_findings() {
        let tmp = TempDir::new().unwrap();
        make_root(tmp.path());
        let stale = engine_binaries_dir(tmp.path()).join(UT4AC_MODULE_DLLS[0]);
        fs::write(&stale, b"ancient").unwrap();

        // A path the scan does not flag is refused — including the canonical DLL.
        let canonical = ut4ac_canonical_binaries_dir(tmp.path()).join(UT4AC_MODULE_DLLS[0]);
        assert!(matches!(
            remove_ut4ac_shadow(tmp.path(), &[], None, &canonical),
            Err(ShadowRemoveError::NotAShadow)
        ));
        assert!(canonical.is_file());

        remove_ut4ac_shadow(tmp.path(), &[], None, &stale).unwrap();
        assert!(!stale.exists());
        assert!(scan_ut4ac_shadows(tmp.path(), &[], None).is_empty());
    }

    #[test]
    fn remove_clears_readonly_copies() {
        let tmp = TempDir::new().unwrap();
        make_root(tmp.path());
        let stale = engine_binaries_dir(tmp.path()).join(UT4AC_MODULE_DLLS[1]);
        fs::write(&stale, b"ancient").unwrap();
        let mut perms = fs::metadata(&stale).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&stale, perms).unwrap();
        remove_ut4ac_shadow(tmp.path(), &[], None, &stale).unwrap();
        assert!(!stale.exists());
    }
}
