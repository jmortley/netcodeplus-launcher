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
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Wrap an `io::Error` with which swap step failed and on which path, so a bare
/// "Access is denied" becomes actionable ("move existing aside: <path>: …").
fn annotate(e: io::Error, step: &str, path: &Path) -> io::Error {
    io::Error::new(e.kind(), format!("{step}: {}: {e}", path.display()))
}

/// `fs::rename`, retried briefly on a transient lock. Renaming a directory
/// Windows has an open handle on (File Explorer showing the folder, an AV
/// mid-scan of a freshly-extracted DLL, the game still closing) fails with
/// access-denied / sharing-violation; those clear in moments, so a few short
/// backoff retries ride through them without making the user do anything. A
/// persistent failure (e.g. the game actually running) still surfaces after the
/// last attempt. ~1.5s worst case (50+100+200+400+800ms).
fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    const BACKOFFS_MS: [u64; 5] = [50, 100, 200, 400, 800];
    let mut last = match fs::rename(from, to) {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    for delay in BACKOFFS_MS {
        // Only retry transient lock errors; a non-lock failure won't fix itself.
        if !is_transient_lock(&last) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(delay));
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Whether an error is a transient file-lock worth retrying (vs. a hard
/// failure). Covers the portable `PermissionDenied` plus the raw Windows
/// `ERROR_ACCESS_DENIED` (5) and `ERROR_SHARING_VIOLATION` (32).
fn is_transient_lock(e: &io::Error) -> bool {
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    e.kind() == io::ErrorKind::PermissionDenied
        || matches!(
            e.raw_os_error(),
            Some(ERROR_ACCESS_DENIED) | Some(ERROR_SHARING_VIOLATION)
        )
}

/// Compute the lowercase hex SHA-256 of the file at `path`, streaming so a
/// large ZIP is not fully buffered.
fn file_sha256_hex(path: &Path) -> io::Result<String> {
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
                let _ = fs::remove_dir_all(&backup); // best-effort cleanup
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_lock_classifier() {
        // Retryable: permission-denied + raw access-denied(5) / sharing(32).
        assert!(is_transient_lock(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
        assert!(is_transient_lock(&io::Error::from_raw_os_error(5)));
        assert!(is_transient_lock(&io::Error::from_raw_os_error(32)));
        // Not retryable: not-found, and an unrelated raw error.
        assert!(!is_transient_lock(&io::Error::from(
            io::ErrorKind::NotFound
        )));
        assert!(!is_transient_lock(&io::Error::from_raw_os_error(2)));
    }
}
