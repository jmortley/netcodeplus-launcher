//! Install the NetcodePlus plugin into a UT4 install by extracting a verified
//! ZIP into `<root>/UnrealTournament/Plugins/NetcodePlus/`.
//!
//! # Trust + safety
//!
//! The ZIP bytes are already SHA-256-verified against the signed manifest by
//! [`ncp_net::download`] before this module sees them — so authenticity is
//! settled. This module's job is to extract them **safely**: a malicious or
//! buggy archive must never write outside the destination directory.
//!
//! - **Zip-slip guard.** Every entry name is validated with an OS-independent
//!   predicate (explicit char/component rules, not `std::path` — a Linux build
//!   must reject `..\evil` the same way the Windows client would). Absolute
//!   paths, `..` traversal, drive/UNC prefixes, and reserved/control chars are
//!   rejected; on any bad entry the whole install aborts and the existing
//!   plugin is left untouched.
//! - **Atomic-ish swap.** Extraction goes to a temp sibling dir; only after it
//!   validates well-formed (`NetcodePlus.uplugin` + `Binaries/`) is the live
//!   folder replaced (old moved aside, new moved in, old removed). On any
//!   failure the temp dir is removed and the live folder is restored.
//! - **No symlinks.** Directory entries create dirs; file entries write files.
//!   We never create links, so a link entry can't redirect a later write.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::fs_util::{annotate, rename_with_retry};
use crate::install::netcodeplus_dir;
use crate::zip_safety::{is_safe_entry, joined_is_within};

/// Failure modes for [`install_plugin_zip`].
#[derive(Debug, Error)]
pub enum PluginInstallError {
    /// The destination install root has no `UnrealTournament/Plugins` parent we
    /// can stage alongside (essentially never on a real install).
    #[error("install root has no usable Plugins directory: {0}")]
    NoPluginsDir(PathBuf),

    /// A ZIP entry name was unsafe (absolute, `..`, drive/UNC, or reserved /
    /// control characters) — a zip-slip attempt. The install is aborted.
    #[error("unsafe path in plugin archive: {0:?}")]
    UnsafeEntry(String),

    /// The archive extracted, but the result is not a well-formed plugin
    /// (missing `NetcodePlus.uplugin` or `Binaries/`). Likely the wrong zip
    /// layout — the contents must sit at the archive root, not under a wrapping
    /// `NetcodePlus/` directory.
    #[error("extracted archive is not a valid NetcodePlus plugin (missing .uplugin or Binaries/)")]
    NotAPlugin,

    /// Error opening or reading the ZIP.
    #[error("could not read plugin archive: {0}")]
    Zip(#[from] zip::result::ZipError),

    /// The ZIP's SHA-256 did not match the expected (signed-manifest) digest.
    /// Surfaced by [`install_plugin_zip_verified`] — the elevated install child
    /// re-checks integrity itself rather than trusting the parent's verdict.
    #[error("plugin archive hash mismatch: expected {expected}, got {got}")]
    HashMismatch {
        /// The hex digest from the signed manifest.
        expected: String,
        /// The hex digest actually computed from the ZIP on disk.
        got: String,
    },

    /// Filesystem error during extraction or the swap.
    #[error("plugin install I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Result alias for plugin installation.
pub type Result<T> = std::result::Result<T, PluginInstallError>;

/// Best-effort removal of `.NetcodePlus.staging.*` / `.NetcodePlus.old.*`
/// leftovers in `plugins_dir` from interrupted prior runs. Failures are ignored
/// (a leftover we cannot delete simply stays; the install uses a PID-unique
/// staging name so it does not collide).
fn sweep_leftovers(plugins_dir: &Path) {
    let Ok(entries) = fs::read_dir(plugins_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".NetcodePlus.staging.") || name.starts_with(".NetcodePlus.old.") {
            remove_leftover_dir(&entry.path());
        }
    }
}

/// Remove a `.NetcodePlus.old.*` / `.staging.*` leftover tree, best-effort. A
/// plain recursive delete fails when a plugin DLL an OPEN UT4 still has memory-
/// mapped keeps its file locked until the game exits — which is how these
/// leftovers accumulate. On Windows, fall back to a reboot-time delete of what
/// remained (files, then dirs deepest-first, then the root) so a still-LOCKED
/// leftover clears at next boot instead of lingering forever. The reboot-delete
/// records into HKLM (needs admin), so it is only effective in the elevated
/// Program-Files install pass — where it is the safety net for a file locked even
/// under elevation; a plain UN-locked admin-owned leftover is just removed
/// outright there. Unelevated, both the delete and the schedule are best-effort.
fn remove_leftover_dir(path: &Path) {
    if fs::remove_dir_all(path).is_ok() || !path.exists() {
        return;
    }
    #[cfg(windows)]
    schedule_tree_delete_on_reboot(path);
}

/// Schedule every file, then every directory (deepest-first), then `root` itself
/// for delete-on-reboot, so the boot-time pass removes a directory's contents
/// before the directory. Best-effort per entry.
#[cfg(windows)]
fn schedule_tree_delete_on_reboot(root: &Path) {
    fn collect(dir: &Path, files: &mut Vec<PathBuf>, dirs: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect(&p, files, dirs);
                dirs.push(p);
            } else {
                files.push(p);
            }
        }
    }
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    collect(root, &mut files, &mut dirs);
    for entry in files.iter().chain(dirs.iter()) {
        let _ = crate::shortcut::schedule_delete_on_reboot(entry);
    }
    let _ = crate::shortcut::schedule_delete_on_reboot(root);
}

/// Compute the lowercase hex SHA-256 of the file at `path`, streaming so a
/// large ZIP is not fully buffered. Shared with the OpenAL elevated-install
/// path, which re-verifies its ZIP the same way.
pub(crate) fn file_sha256_hex(path: &Path) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Lowercase hex SHA-256 of everything an arbitrary reader yields (e.g. a ZIP
/// entry stream), streamed so a large file is not fully buffered.
fn reader_sha256_hex<R: io::Read>(mut reader: R) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    io::copy(&mut reader, &mut hasher)?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Re-verify the ZIP's SHA-256 against `expected_sha256_hex`, then install it
/// into `root`. This is what the **elevated** install child calls: it does NOT
/// trust that the (unelevated) parent already verified the bytes — it re-hashes
/// the file itself, closing the window between the parent's verify and the
/// privileged extract. `expected_sha256_hex` originates from the signed
/// manifest and is passed across the elevation boundary as an argument.
///
/// # Errors
/// [`PluginInstallError::HashMismatch`] if the digest differs, otherwise the
/// same errors as [`install_plugin_zip`].
pub fn install_plugin_zip_verified(
    zip_path: &Path,
    root: &Path,
    expected_sha256_hex: &str,
) -> Result<()> {
    let got = file_sha256_hex(zip_path)?;
    if !got.eq_ignore_ascii_case(expected_sha256_hex) {
        return Err(PluginInstallError::HashMismatch {
            expected: expected_sha256_hex.to_string(),
            got,
        });
    }
    install_plugin_zip(zip_path, root)
}

/// Extract the already-verified plugin ZIP at `zip_path` into
/// `<root>/UnrealTournament/Plugins/NetcodePlus/`, replacing any existing
/// install atomically-ish (old moved aside, restored on failure).
///
/// Returns `Ok(())` only when the extracted tree is a well-formed plugin.
///
/// # Errors
/// See [`PluginInstallError`] — unsafe archive entries, malformed result, or
/// filesystem/zip errors. On any error the live plugin folder is left as it
/// was before the call.
pub fn install_plugin_zip(zip_path: &Path, root: &Path) -> Result<()> {
    let dest = netcodeplus_dir(root); // <root>/UnrealTournament/Plugins/NetcodePlus
    let plugins_dir = dest
        .parent()
        .ok_or_else(|| PluginInstallError::NoPluginsDir(dest.clone()))?
        .to_path_buf();
    fs::create_dir_all(&plugins_dir)?;

    // Sweep any leftover staging/backup dirs from a previous interrupted run
    // (e.g. a crash, or an earlier failed elevated attempt). Best-effort: a
    // leftover we cannot remove is not fatal here. Doing this lets an elevated
    // run clean up an admin-owned leftover a prior elevated run left behind.
    sweep_leftovers(&plugins_dir);

    // Stage into a temp sibling dir so a half-extraction never touches the live
    // folder. Unique-ish name; cleaned up on every exit path.
    let staging = plugins_dir.join(format!(".NetcodePlus.staging.{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;

    // Extract; on any failure scrub staging and bail without touching `dest`.
    if let Err(e) = extract_into(zip_path, &staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }

    // The contents must be a well-formed plugin (files at the archive root, so
    // .uplugin + Binaries/ land directly in staging).
    if !staging.join("NetcodePlus.uplugin").is_file() || !staging.join("Binaries").is_dir() {
        let _ = fs::remove_dir_all(&staging);
        return Err(PluginInstallError::NotAPlugin);
    }

    // Swap: move any existing folder aside, move staging into place, remove the
    // old. If the final move fails, restore the old folder. Each fs op is
    // annotated so a failure names the exact step (the bare io::Error otherwise
    // just says "Access is denied" with no indication of which path).
    let backup = plugins_dir.join(format!(".NetcodePlus.old.{}", std::process::id()));
    let had_existing = dest.exists();
    if had_existing {
        if backup.exists() {
            fs::remove_dir_all(&backup).map_err(|e| annotate(e, "remove stale backup", &backup))?;
        }
        rename_with_retry(&dest, &backup).map_err(|e| annotate(e, "move existing aside", &dest))?;
    }
    match rename_with_retry(&staging, &dest)
        .map_err(|e| annotate(e, "move new into place", &staging))
    {
        Ok(()) => {
            if had_existing {
                remove_leftover_dir(&backup); // best-effort; reboot-delete a locked leftover
            }
            Ok(())
        }
        Err(e) => {
            // Roll back: put the old folder back, drop staging.
            if had_existing {
                let _ = fs::rename(&backup, &dest);
            }
            let _ = fs::remove_dir_all(&staging);
            Err(PluginInstallError::Io(e))
        }
    }
}

/// Extract every entry of the ZIP at `zip_path` into `staging`, enforcing the
/// zip-slip guard. Directory entries create dirs; file entries create files.
fn extract_into(zip_path: &Path, staging: &Path) -> Result<()> {
    let file = fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();

        if !is_safe_entry(&name) {
            return Err(PluginInstallError::UnsafeEntry(name));
        }
        let Some(out_path) = joined_is_within(staging, &name) else {
            return Err(PluginInstallError::UnsafeEntry(name));
        };

        if entry.is_dir() || name.ends_with('/') || name.ends_with('\\') {
            fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::File::create(&out_path)?;
        io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

/// Whether the NetcodePlus plugin currently on disk under `root` is the current
/// build — i.e. every FILE entry in the verified ZIP at `zip_path` exists under
/// `<root>/UnrealTournament/Plugins/NetcodePlus/` with an identical SHA-256, AND
/// the install carries no EXTRA load-bearing files this build doesn't ship.
///
/// This is how the launcher recognises a plugin the user installed BY HAND (no
/// recorded version) as already-current, without a destructive reinstall: the
/// manifest pins the ZIP's hash, so a content match proves the on-disk bytes are
/// this build. A single missing or differing tracked file → `Ok(false)` (a
/// different / older build → genuinely needs an update).
///
/// Extra on-disk files OFF the load path (a user's `notes.txt`, leftover content)
/// are tolerated. But an extra file under `Binaries/`, or a stray `.uplugin` /
/// `.dll` / `.modules` / `.pdb` anywhere, fails the match: adopt asserts "this
/// install IS this build", and such a file means the engine could load bytes this
/// build doesn't ship (e.g. an official build hand-overlaid on top of an
/// older/dev one). Unlike [`install_plugin_zip`] — which extracts into a fresh
/// dir and atomically swaps, guaranteeing exact contents — adopt inspects a
/// folder it didn't place, so it checks for stray load-bearing files explicitly.
///
/// The ZIP bytes are assumed already SHA-256-verified against the signed manifest
/// by the caller ([`ncp_net::download`]). The same zip-slip guard as extraction
/// maps each entry to a path strictly within the plugin dir.
///
/// # Errors
/// An unsafe archive entry, or a ZIP/IO read error. A *missing* on-disk file is
/// NOT an error — it is simply a non-match (`Ok(false)`).
pub fn plugin_matches_zip(zip_path: &Path, root: &Path) -> Result<bool> {
    use std::collections::HashSet;

    let dest = netcodeplus_dir(root); // <root>/UnrealTournament/Plugins/NetcodePlus
    let file = fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    // The build's file set as plugin-root-relative lowercase '/'-paths, so the
    // extra-file walk below can tell shipped files from strays.
    let mut shipped: HashSet<String> = HashSet::new();

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();

        // Directory entries carry no content to compare.
        if entry.is_dir() || name.ends_with('/') || name.ends_with('\\') {
            continue;
        }
        if !is_safe_entry(&name) {
            return Err(PluginInstallError::UnsafeEntry(name));
        }
        let Some(on_disk) = joined_is_within(&dest, &name) else {
            return Err(PluginInstallError::UnsafeEntry(name));
        };

        // A file this build ships that isn't on disk → not this build.
        if !on_disk.is_file() {
            return Ok(false);
        }
        let want = reader_sha256_hex(&mut entry)?;
        let got = file_sha256_hex(&on_disk)?;
        if !want.eq_ignore_ascii_case(&got) {
            return Ok(false);
        }
        shipped.insert(norm_plugin_rel(&name));
    }

    // An install that carries extra load-bearing files this build doesn't ship
    // isn't purely this build → don't adopt it.
    if has_extra_load_bearing_file(&dest, &dest, &shipped)? {
        return Ok(false);
    }
    Ok(true)
}

/// Normalise a plugin-relative path to lowercase forward-slash form so ZIP entry
/// names and on-disk paths compare equal across separators and case.
fn norm_plugin_rel(rel: &str) -> String {
    rel.replace('\\', "/").to_ascii_lowercase()
}

/// Recursively: does `dir` (under plugin root `base`) hold any FILE not in
/// `shipped` that is load-bearing — under `Binaries/`, or a `.uplugin` / `.dll` /
/// `.modules` / `.pdb` anywhere? Such a file means the on-disk install is not
/// purely the shipped build.
fn has_extra_load_bearing_file(
    base: &Path,
    dir: &Path,
    shipped: &std::collections::HashSet<String>,
) -> Result<bool> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            if has_extra_load_bearing_file(base, &path, shipped)? {
                return Ok(true);
            }
            continue;
        }
        let rel_path = path.strip_prefix(base).unwrap_or(&path);
        let rel = norm_plugin_rel(&rel_path.to_string_lossy());
        if shipped.contains(&rel) {
            continue;
        }
        let load_bearing = rel.starts_with("binaries/")
            || rel.ends_with(".uplugin")
            || rel.ends_with(".dll")
            || rel.ends_with(".modules")
            || rel.ends_with(".pdb");
        if load_bearing {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A stable fingerprint of the load-bearing files on disk for the NetcodePlus
/// plugin under `root`: the `.uplugin` plus every file under `Binaries/`, each
/// SHA-256'd and combined in sorted relative-path order.
///
/// Recorded when the launcher installs/adopts a build (a clean install lays the
/// folder out as exactly the manifest ZIP). Re-hashing later equals the recorded
/// value iff the bytes are untouched — so a hand-swap of the plugin to a
/// different build (an older `Binaries/` dropped in while the manifest is
/// unchanged) is detectable even though the recorded ZIP digest still "matches"
/// the manifest. `None` when the plugin folder is absent or unreadable.
#[must_use]
pub fn plugin_content_hash(root: &Path) -> Option<String> {
    let dir = netcodeplus_dir(root);
    if !dir.is_dir() {
        return None;
    }
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let uplugin = dir.join("NetcodePlus.uplugin");
    if uplugin.is_file() {
        files.push((norm_plugin_rel("NetcodePlus.uplugin"), uplugin));
    }
    collect_files_under(&dir.join("Binaries"), &dir, &mut files).ok()?;
    let mut parts: Vec<(String, String)> = Vec::with_capacity(files.len());
    for (rel, path) in files {
        parts.push((rel, file_sha256_hex(&path).ok()?));
    }
    combine_fingerprint(parts)
}

/// The build's EXPECTED [`plugin_content_hash`], computed from its verified ZIP
/// (the `.uplugin` + every `Binaries/` entry) instead of an on-disk folder.
///
/// Equals `plugin_content_hash` of a clean install of the same ZIP — the
/// installer extracts exactly these entries — so the launcher can baseline an
/// install it didn't place (a hand-install, or a record written before
/// fingerprints existed) against the manifest build, then detect drift locally.
/// `None` if the ZIP can't be read or carries no plugin files.
#[must_use]
pub fn plugin_zip_content_hash(zip_path: &Path) -> Option<String> {
    let file = fs::File::open(zip_path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let mut parts: Vec<(String, String)> = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).ok()?;
        let name = entry.name().to_string();
        if entry.is_dir() || name.ends_with('/') || name.ends_with('\\') {
            continue;
        }
        let rel = norm_plugin_rel(&name);
        if rel == "netcodeplus.uplugin" || rel.starts_with("binaries/") {
            let fh = reader_sha256_hex(&mut entry).ok()?;
            parts.push((rel, fh));
        }
    }
    combine_fingerprint(parts)
}

/// Combine `(relative-path, file-SHA-256-hex)` pairs into one stable fingerprint,
/// independent of input order. `None` for an empty set (no plugin files found).
fn combine_fingerprint(mut parts: Vec<(String, String)>) -> Option<String> {
    use sha2::{Digest, Sha256};
    if parts.is_empty() {
        return None;
    }
    parts.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (rel, fh) in &parts {
        hasher.update(rel.as_bytes());
        hasher.update([0u8]);
        hasher.update(fh.as_bytes());
        hasher.update([b'\n']);
    }
    Some(
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}

/// Recursively collect `(normalised-relpath, path)` for every FILE under `dir`,
/// relative to `base`.
///
/// A missing `dir` (e.g. no `Binaries/`) is fine — nothing to add. Any OTHER read
/// error PROPAGATES, so a partially-unreadable tree makes [`plugin_content_hash`]
/// return `None` (fail-safe: fall back to the recorded ZIP digest) rather than
/// fingerprint a partial file set and report a false "outdated".
fn collect_files_under(
    dir: &Path,
    base: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> io::Result<()> {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in rd {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            collect_files_under(&path, base, out)?;
        } else if ft.is_file() {
            if let Ok(rel) = path.strip_prefix(base) {
                out.push((norm_plugin_rel(&rel.to_string_lossy()), path));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal well-formed plugin ZIP (`.uplugin` + a `Binaries/` file),
    /// STORED so the test has no compression-method dependency.
    fn make_plugin_zip(path: &Path, uplugin: &[u8], dll: &[u8]) {
        use std::io::Write;
        let mut buf: Vec<u8> = Vec::new();
        {
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            let mut w = zip::ZipWriter::new(io::Cursor::new(&mut buf));
            w.start_file("NetcodePlus.uplugin", opts).unwrap();
            w.write_all(uplugin).unwrap();
            w.add_directory("Binaries/Win64/", opts).unwrap();
            w.start_file("Binaries/Win64/UE4-NetcodePlus.dll", opts)
                .unwrap();
            w.write_all(dll).unwrap();
            w.finish().unwrap();
        }
        fs::write(path, &buf).unwrap();
    }

    /// Linux (Wine-prefix) install: pointing the installer at the in-prefix UT4
    /// root (`<prefix>/drive_c/Program Files/UnrealTournament`) lands the plugin at
    /// the canonical in-prefix location as a real-file COPY — Wine chokes on
    /// symlinks, and the installer already copies. Pure-fs, so it runs on the
    /// Windows dev box + CI; the `linux` module is compiled here under `cfg(test)`.
    #[test]
    fn installs_into_a_wine_prefix_at_the_in_prefix_path() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let zip_path = tmp.path().join("NetcodePlus.zip");
        make_plugin_zip(&zip_path, br#"{"VersionName":"2.0"}"#, b"DLL-bytes-v327");

        // Install at the in-prefix UT4 root, exactly as the Linux launch path would.
        let prefix = tmp.path().join("Games").join("ut4");
        let root = crate::linux::ut4_root_from_prefix(&prefix);
        install_plugin_zip(&zip_path, &root).unwrap();

        // Landed at the canonical in-prefix plugin dir (single-sourced with Windows).
        let plugin = crate::linux::plugin_install_path(&prefix);
        assert_eq!(plugin, netcodeplus_dir(&root));
        let uplugin = plugin.join("NetcodePlus.uplugin");
        let dll = plugin.join("Binaries/Win64/UE4-NetcodePlus.dll");
        assert!(uplugin.is_file(), "the .uplugin should exist in-prefix");
        assert!(dll.is_file(), "the Win64 DLL should exist in-prefix");
        assert_eq!(fs::read(&dll).unwrap(), b"DLL-bytes-v327");

        // Real files, not symlinks (Wine requires copies).
        assert!(!fs::symlink_metadata(&uplugin)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(!fs::symlink_metadata(&dll).unwrap().file_type().is_symlink());
    }

    #[test]
    fn plugin_matches_zip_detects_a_manual_install_of_the_same_build() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let zip_path = tmp.path().join("NetcodePlus.zip");
        make_plugin_zip(&zip_path, br#"{"VersionName":"2.0"}"#, b"DLL-bytes-v327");

        // Simulate a hand-extracted install: the same bytes, but no launcher record.
        let root = tmp.path().join("game");
        install_plugin_zip(&zip_path, &root).unwrap();
        assert!(plugin_matches_zip(&zip_path, &root).unwrap());

        // A tampered/older tracked file → not a match.
        let dll = netcodeplus_dir(&root).join("Binaries/Win64/UE4-NetcodePlus.dll");
        fs::write(&dll, b"DLL-bytes-v326").unwrap();
        assert!(!plugin_matches_zip(&zip_path, &root).unwrap());

        // A missing tracked file → not a match (and not an error).
        fs::remove_file(&dll).unwrap();
        assert!(!plugin_matches_zip(&zip_path, &root).unwrap());

        // An extra file OFF the load path does not break a match.
        install_plugin_zip(&zip_path, &root).unwrap();
        fs::write(netcodeplus_dir(&root).join("notes.txt"), b"unrelated").unwrap();
        assert!(plugin_matches_zip(&zip_path, &root).unwrap());

        // But an extra LOAD-BEARING file (a stray DLL under Binaries/, or a second
        // .uplugin) DOES break it — adopt must not bless a build overlaid on a
        // leftover one.
        let stray = netcodeplus_dir(&root).join("Binaries/Win64/UE4-Old.dll");
        fs::write(&stray, b"stale").unwrap();
        assert!(!plugin_matches_zip(&zip_path, &root).unwrap());
        fs::remove_file(&stray).unwrap();
        assert!(plugin_matches_zip(&zip_path, &root).unwrap());
        fs::write(netcodeplus_dir(&root).join("Old.uplugin"), b"{}").unwrap();
        assert!(!plugin_matches_zip(&zip_path, &root).unwrap());
    }

    #[test]
    fn zip_and_folder_fingerprints_match_for_a_clean_install() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let zip_path = tmp.path().join("NetcodePlus.zip");
        make_plugin_zip(&zip_path, br#"{"VersionName":"2.0"}"#, b"DLL-v327");
        let root = tmp.path().join("game");
        install_plugin_zip(&zip_path, &root).unwrap();

        // The whole migration relies on these being equal: the launcher baselines
        // a hand-installed plugin against the ZIP-derived fingerprint, then checks
        // the folder fingerprint against it.
        let from_zip = plugin_zip_content_hash(&zip_path);
        assert!(from_zip.is_some());
        assert_eq!(
            from_zip,
            plugin_content_hash(&root),
            "a clean install's folder fingerprint must equal the ZIP's"
        );

        // Swapping a tracked binary moves the on-disk fingerprint away from it.
        let dll = netcodeplus_dir(&root).join("Binaries/Win64/UE4-NetcodePlus.dll");
        fs::write(&dll, b"DLL-v326").unwrap();
        assert_ne!(
            plugin_content_hash(&root),
            from_zip,
            "a swapped build must not match the ZIP fingerprint"
        );
    }
}
