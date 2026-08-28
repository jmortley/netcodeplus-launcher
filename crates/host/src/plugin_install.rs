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

/// Best-effort removal of `.{name}.staging.*` / `.{name}.old.*` leftovers in
/// `plugins_dir` from interrupted prior runs. Failures are ignored (a leftover
/// we cannot delete simply stays; installs use a PID-unique staging name so
/// they do not collide).
fn sweep_leftovers_for(plugins_dir: &Path, plugin_name: &str) {
    let staging_prefix = format!(".{plugin_name}.staging.");
    let old_prefix = format!(".{plugin_name}.old.");
    let Ok(entries) = fs::read_dir(plugins_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&staging_prefix) || name.starts_with(&old_prefix) {
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
pub(crate) fn remove_leftover_dir(path: &Path) {
    // Windows: if removal fails and the tree is still there, it's locked (a
    // mmap'd DLL held by the running game) — queue a reboot-time delete so the
    // leftover clears at next boot instead of lingering forever.
    #[cfg(windows)]
    if fs::remove_dir_all(path).is_err() && path.exists() {
        schedule_tree_delete_on_reboot(path);
    }
    // Elsewhere there is no reboot-delete queue; a best-effort remove is all we
    // can do, and a locked leftover simply isn't a thing outside Windows.
    #[cfg(not(windows))]
    let _ = fs::remove_dir_all(path);
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
pub(crate) fn reader_sha256_hex<R: io::Read>(mut reader: R) -> io::Result<String> {
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
    // <root>/UnrealTournament/Plugins/NetcodePlus
    install_plugin_dir_zip(zip_path, &netcodeplus_dir(root), "NetcodePlus")
}

/// The UT4AC anti-cheat plugin directory under a game install root:
/// `<root>/UnrealTournament/Plugins/UT4AC`. A standard sibling UE4 plugin —
/// present = the engine loads it, absent = engine-guaranteed inert
/// (docs/ANTICHEAT-OPTIN-DESIGN.md §5).
#[must_use]
pub fn ut4ac_dir(root: &Path) -> PathBuf {
    crate::install::plugins_dir(root).join("UT4AC")
}

/// Install the UT4AC module zip (already sha-verified by the caller against
/// the signed manifest) into [`ut4ac_dir`], with the same staging/validation/
/// atomic-swap guarantees as the NetcodePlus installer.
///
/// # Errors
/// See [`PluginInstallError`]; on any error the existing UT4AC folder (if any)
/// is left as it was.
pub fn install_ut4ac_zip(zip_path: &Path, root: &Path) -> Result<()> {
    install_plugin_dir_zip(zip_path, &ut4ac_dir(root), "UT4AC")
}

/// Remove the UT4AC plugin folder entirely — the uninstall half of the opt-in
/// contract. `Ok(true)` = removed, `Ok(false)` = was already absent. The
/// caller clears the consent record alongside.
///
/// # Errors
/// Any filesystem error other than the folder not existing (e.g. a file locked
/// by a running game — the command layer refuses while UT4 runs for exactly
/// this reason).
pub fn remove_ut4ac(root: &Path) -> io::Result<bool> {
    match fs::remove_dir_all(ut4ac_dir(root)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Install a plugin-folder zip into an arbitrary sibling plugin directory —
/// the [`install_plugin_zip`] machinery generalized by destination, shared with
/// the UT4AC anti-cheat installer (docs/ANTICHEAT-OPTIN-DESIGN.md §5). Same
/// guarantees: zip-slip guard, temp-sibling staging, `{name}.uplugin` +
/// `Binaries/` well-formedness validation, atomic swap with rollback.
pub(crate) fn install_plugin_dir_zip(
    zip_path: &Path,
    dest: &Path,
    plugin_name: &str,
) -> Result<()> {
    let plugins_dir = dest
        .parent()
        .ok_or_else(|| PluginInstallError::NoPluginsDir(dest.to_path_buf()))?
        .to_path_buf();
    fs::create_dir_all(&plugins_dir)?;

    // Sweep any leftover staging/backup dirs from a previous interrupted run
    // (e.g. a crash, or an earlier failed elevated attempt). Best-effort: a
    // leftover we cannot remove is not fatal here. Doing this lets an elevated
    // run clean up an admin-owned leftover a prior elevated run left behind.
    sweep_leftovers_for(&plugins_dir, plugin_name);

    // Stage into a temp sibling dir so a half-extraction never touches the live
    // folder. Unique-ish name; cleaned up on every exit path.
    let staging = plugins_dir.join(format!(".{plugin_name}.staging.{}", std::process::id()));
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
    if !staging.join(format!("{plugin_name}.uplugin")).is_file()
        || !staging.join("Binaries").is_dir()
    {
        let _ = fs::remove_dir_all(&staging);
        return Err(PluginInstallError::NotAPlugin);
    }

    // Swap: move any existing folder aside, move staging into place, remove the
    // old. If the final move fails, restore the old folder. Each fs op is
    // annotated so a failure names the exact step (the bare io::Error otherwise
    // just says "Access is denied" with no indication of which path).
    let backup = plugins_dir.join(format!(".{plugin_name}.old.{}", std::process::id()));
    let had_existing = dest.exists();
    if had_existing {
        if backup.exists() {
            fs::remove_dir_all(&backup).map_err(|e| annotate(e, "remove stale backup", &backup))?;
        }
        rename_with_retry(dest, &backup).map_err(|e| annotate(e, "move existing aside", dest))?;
    }
    match rename_with_retry(&staging, dest)
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
                let _ = fs::rename(&backup, dest);
            }
            let _ = fs::remove_dir_all(&staging);
            Err(PluginInstallError::Io(e))
        }
    }
}

/// Extract every entry of the ZIP at `zip_path` into `staging`, enforcing the
/// zip-slip guard. Directory entries create dirs; file entries create files.
pub(crate) fn extract_into(zip_path: &Path, staging: &Path) -> Result<()> {
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
pub(crate) fn norm_plugin_rel(rel: &str) -> String {
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

/// Environment variable the launcher uses to hand the one-shot Epic exchange
/// code to the game instead of putting it on the command line.
///
/// The engine writes the whole command line into `Saved/Logs/UnrealTournament.log`
/// and into the `CommandLine` property of every crash report, and players post
/// those files publicly when asking for help — which leaked a live login
/// credential. NetcodePlus builds that support this read the variable during
/// module startup and append the value to the command line in memory, where the
/// engine's already-written logging copies can no longer pick it up.
pub const AUTH_ENV_VAR: &str = "NCP_AUTH_PASSWORD";

/// Whether the NetcodePlus build installed under `root` takes the login code
/// from [`AUTH_ENV_VAR`] rather than `-AUTH_PASSWORD`.
///
/// Probed by searching the plugin's shipping **client** DLL for the variable
/// name, which the plugin's `TEXT("NCP_AUTH_PASSWORD")` literal leaves in the
/// binary as UTF-16LE. (Linux runs the same Windows build under Wine, so the
/// same probe answers for both platforms.)
///
/// Deliberately not a version check: client re-rolls ship under the same build
/// number, so the version genuinely cannot distinguish a build with the handoff
/// from one without — and being wrong in the optimistic direction means the code
/// never reaches the game and the player silently fails to log in. Every doubt
/// (no plugin, unreadable file, no match) answers `false`, which is just the
/// long-standing command-line behaviour.
#[must_use]
pub fn plugin_supports_env_auth(root: &Path) -> bool {
    let dir = netcodeplus_dir(root).join("Binaries").join("Win64");
    let needle: Vec<u8> = AUTH_ENV_VAR
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let Ok(entries) = fs::read_dir(&dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("dll"))
        {
            continue;
        }
        // Only the client DLL decides this: it is the one the game loads. An
        // editor/server DLL left behind from another build must not vote.
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if name.starts_with("ue4editor") || name.starts_with("ue4server") {
            continue;
        }
        if let Ok(bytes) = fs::read(&path) {
            if bytes.windows(needle.len()).any(|w| w == needle.as_slice()) {
                return true;
            }
        }
    }
    false
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
pub(crate) fn combine_fingerprint(mut parts: Vec<(String, String)>) -> Option<String> {
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
pub(crate) fn collect_files_under(
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

    /// Write a plugin folder whose client DLL contains `dll_bytes`, and return the
    /// install root. Mirrors what the installer lays down.
    fn plugin_folder_with_client_dll(tmp: &Path, dll_bytes: &[u8]) -> PathBuf {
        let root = tmp.join("UT4");
        let win64 = netcodeplus_dir(&root).join("Binaries").join("Win64");
        fs::create_dir_all(&win64).unwrap();
        fs::write(
            netcodeplus_dir(&root).join("NetcodePlus.uplugin"),
            br#"{"VersionName":"2.0"}"#,
        )
        .unwrap();
        fs::write(win64.join("UE4-NetcodePlus-Win64-Shipping.dll"), dll_bytes).unwrap();
        root
    }

    /// The variable name as it appears inside a compiled UE4 binary: the plugin's
    /// `TEXT("NCP_AUTH_PASSWORD")` literal is UTF-16LE.
    fn utf16_needle() -> Vec<u8> {
        AUTH_ENV_VAR
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect()
    }

    #[test]
    fn env_auth_probe_finds_the_marker_in_the_client_dll() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let mut dll = b"...arbitrary binary padding...".to_vec();
        dll.extend_from_slice(&utf16_needle());
        dll.extend_from_slice(b"...more padding...");
        let root = plugin_folder_with_client_dll(tmp.path(), &dll);
        assert!(plugin_supports_env_auth(&root));
    }

    #[test]
    fn env_auth_probe_says_no_for_an_older_build_or_no_plugin() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();

        // A build without the handoff: the code must still go on the command line,
        // or the player cannot log in at all.
        let root = plugin_folder_with_client_dll(tmp.path(), b"an older build, no marker");
        assert!(!plugin_supports_env_auth(&root));

        // No plugin installed at all.
        assert!(!plugin_supports_env_auth(&tmp.path().join("empty")));
    }

    #[test]
    fn env_auth_probe_ignores_editor_and_server_dlls() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        // Client DLL is an old build; editor/server DLLs left over from a newer one
        // must not make us skip the command-line argument the client still needs.
        let root = plugin_folder_with_client_dll(tmp.path(), b"an older build, no marker");
        let win64 = netcodeplus_dir(&root).join("Binaries").join("Win64");
        for name in [
            "UE4Editor-NetcodePlus.dll",
            "UE4Server-NetcodePlus-Win64-Shipping.dll",
        ] {
            fs::write(win64.join(name), utf16_needle()).unwrap();
        }
        assert!(!plugin_supports_env_auth(&root));
    }

    /// UT4AC artifact shape: `UT4AC.uplugin` + `Binaries/**` at the zip root.
    fn make_ut4ac_zip(path: &Path) {
        use std::io::Write;
        let mut buf: Vec<u8> = Vec::new();
        {
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            let mut w = zip::ZipWriter::new(io::Cursor::new(&mut buf));
            w.start_file("UT4AC.uplugin", opts).unwrap();
            w.write_all(br#"{"VersionName":"1.0.9"}"#).unwrap();
            w.add_directory("Binaries/Win64/", opts).unwrap();
            w.start_file("Binaries/Win64/UE4-UT4ACClient-Win64-Shipping.dll", opts)
                .unwrap();
            w.write_all(b"AC-client-bytes").unwrap();
            w.finish().unwrap();
        }
        fs::write(path, &buf).unwrap();
    }

    #[test]
    fn ut4ac_installs_to_its_own_plugin_dir_and_uninstalls_cleanly() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let zip_path = tmp.path().join("UT4AC.zip");
        make_ut4ac_zip(&zip_path);
        let root = tmp.path().join("UT4");

        install_ut4ac_zip(&zip_path, &root).unwrap();
        let dir = ut4ac_dir(&root);
        assert!(dir.join("UT4AC.uplugin").is_file());
        assert!(dir
            .join("Binaries/Win64/UE4-UT4ACClient-Win64-Shipping.dll")
            .is_file());
        // Sibling of NetcodePlus, not inside it.
        assert_eq!(
            dir.parent().unwrap(),
            netcodeplus_dir(&root).parent().unwrap()
        );

        // Uninstall = the folder is gone; absent = Ok(false), not an error.
        assert!(remove_ut4ac(&root).unwrap());
        assert!(!dir.exists());
        assert!(!remove_ut4ac(&root).unwrap());
    }

    #[test]
    fn ut4ac_install_rejects_a_zip_without_its_uplugin() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let zip_path = tmp.path().join("NotAC.zip");
        // A NetcodePlus-shaped zip is NOT a valid UT4AC artifact: the
        // validation is name-specific, so a mixed-up upload cannot land in
        // the anti-cheat slot.
        make_plugin_zip(&zip_path, br#"{"VersionName":"2.0"}"#, b"DLL");
        let root = tmp.path().join("UT4");

        let err = install_ut4ac_zip(&zip_path, &root).unwrap_err();
        assert!(matches!(err, PluginInstallError::NotAPlugin));
        assert!(!ut4ac_dir(&root).exists());
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
