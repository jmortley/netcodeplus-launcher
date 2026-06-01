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
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

use crate::install::netcodeplus_dir;

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

/// `true` only when `name` is a flat, relative, safe path to join under the
/// destination: no absolute root, no `..` parent escape, no drive/UNC prefix,
/// no reserved/control characters. Forward and back slashes are both treated as
/// separators (a ZIP authored on Windows may use `\`), and each component is
/// checked. Empty names and bare `.`/`..` are rejected.
///
/// OS-independent on purpose: the verdict must be identical whether the
/// launcher is built/run on Windows or Linux, because the same archive feeds
/// the Windows game client.
fn is_safe_entry(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    // A leading `/` or `\`, or a Windows drive (`C:`) / UNC (`\\`) prefix, means
    // absolute — reject outright.
    let bytes = name.as_bytes();
    if bytes[0] == b'/' || bytes[0] == b'\\' {
        return false;
    }
    if name.len() >= 2 && bytes[1] == b':' {
        return false; // drive-letter absolute
    }
    // Split on BOTH separators and validate every component.
    for comp in name.split(['/', '\\']) {
        if comp.is_empty() {
            // Empty component = a `//`, trailing slash, etc. Trailing slash on a
            // directory entry is normal, so allow a single trailing empty by
            // only rejecting interior empties? Simpler + safe: a directory entry
            // is handled by the caller trimming; here treat empty as a skip-safe
            // separator artifact, NOT a failure.
            continue;
        }
        if comp == ".." || comp == "." {
            return false;
        }
        if comp.contains(['<', '>', '"', '|', '?', '*', ':']) {
            return false;
        }
        if comp.chars().any(char::is_control) {
            return false;
        }
    }
    true
}

/// Defence-in-depth: independently confirm the resolved join stays under
/// `base`, catching anything `is_safe_entry` somehow let through. This must be
/// sound on its OWN (not relying on the earlier check), so it re-rejects
/// absolute / drive-prefixed entries rather than letting a leading separator be
/// silently dropped by component-splitting.
fn joined_is_within(base: &Path, entry: &str) -> Option<PathBuf> {
    let bytes = entry.as_bytes();
    // Absolute (leading separator) or drive-letter prefix → reject.
    if entry.is_empty() || bytes[0] == b'/' || bytes[0] == b'\\' {
        return None;
    }
    if entry.len() >= 2 && bytes[1] == b':' {
        return None;
    }
    let mut out = base.to_path_buf();
    for comp in entry.split(['/', '\\']).filter(|c| !c.is_empty()) {
        match Path::new(comp).components().next() {
            // Only ever a plain child name is allowed — no `..`, `.`, root, or
            // prefix component.
            Some(Component::Normal(c)) => out.push(c),
            _ => return None,
        }
    }
    // The final path must still be a descendant of base.
    out.starts_with(base).then_some(out)
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
    // old. If the final move fails, restore the old folder.
    let backup = plugins_dir.join(format!(".NetcodePlus.old.{}", std::process::id()));
    let had_existing = dest.exists();
    if had_existing {
        if backup.exists() {
            fs::remove_dir_all(&backup)?;
        }
        fs::rename(&dest, &backup)?;
    }
    match fs::rename(&staging, &dest) {
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
    fn rejects_traversal_and_absolute_entries() {
        for bad in [
            "../evil.txt",
            "..\\evil.txt",
            "/etc/passwd",
            "\\\\unc\\share\\x",
            "C:\\Windows\\x",
            "a/../../b",
            "foo/../../../bar",
            "bad\0name",
            "name:stream",
        ] {
            assert!(!is_safe_entry(bad), "should reject {bad:?}");
        }
    }

    #[test]
    fn accepts_normal_plugin_entries() {
        for ok in [
            "NetcodePlus.uplugin",
            "Binaries/Win64/UE4-NetcodePlus.dll",
            "Binaries\\Win64\\UE4-NetcodePlus.dll",
            "Resources/Icon128.png",
            "Binaries/",
        ] {
            assert!(is_safe_entry(ok), "should accept {ok:?}");
        }
    }

    #[test]
    fn joined_within_rejects_escape_and_accepts_child() {
        let base = Path::new("C:/games/Plugins/.staging");
        // Legitimate nested file resolves under base.
        assert!(joined_is_within(base, "Binaries/Win64/x.dll").is_some());
        assert!(joined_is_within(base, "NetcodePlus.uplugin").is_some());
        // Every escape shape must be independently rejected by THIS function,
        // not relying on is_safe_entry having run first.
        for bad in [
            "../escape",
            "a/../../b",
            "/abs",
            "\\abs",
            "C:\\Windows",
            "C:/Windows",
            "",
        ] {
            assert!(
                joined_is_within(base, bad).is_none(),
                "should reject {bad:?}"
            );
        }
    }
}
