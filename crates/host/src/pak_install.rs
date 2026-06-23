//! Install / remove a single NetcodePlus content pak in the user's UT4
//! mod-paks directory (`…/Documents/UnrealTournament/Saved/Paks/DownloadedPaks/`,
//! from [`crate::install::default_mod_paks_dir`]).
//!
//! Unlike the plugin — a *folder* swapped atomically — a pak is one `*.pak`
//! file dropped alongside other community paks in a shared directory, so this
//! installs it **per-file**: the caller hands us a staging file whose bytes
//! [`ncp_net::download`] already SHA-256-verified against the signed manifest,
//! and we atomically rename it over the final name. We clear a read-only
//! attribute on any existing copy first and ride out transient locks (the dir
//! is commonly OneDrive-synced) via [`crate::fs_util`]. Sibling paks are never
//! touched.
//!
//! Writing the pak under its *canonical* manifest filename means a launcher copy
//! lands at the exact path the engine's own redirect-downloader uses, so a
//! server that pushes the same pak just re-writes the same file rather than
//! creating a second, divergent copy.
//!
//! The caller MUST have already confirmed UT4 is not running — a mounted pak is
//! locked and cannot be replaced — this module does not check.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::fs_util::{annotate, clear_readonly, rename_with_retry};

/// Final on-disk path for `pak_filename` inside `dest_dir`.
///
/// `pak_filename` is a bare filename from the signed manifest, whose
/// verification already rejects separators / traversal
/// (`is_safe_pak_filename`), so the join can only ever land directly inside
/// `dest_dir`.
fn pak_path(dest_dir: &Path, pak_filename: &str) -> PathBuf {
    dest_dir.join(pak_filename)
}

/// Move an already-verified pak `staged` into `dest_dir` as `pak_filename`,
/// atomically replacing any existing copy.
///
/// `staged` must be SHA-256-verified by the caller ([`ncp_net::download`]) and
/// should be a sibling inside `dest_dir` so the final rename is same-filesystem
/// (hence atomic). On success the staging file no longer exists — it *became*
/// the final pak. On failure `dest_dir/pak_filename` is left exactly as it was,
/// and cleaning up `staged` is the caller's responsibility.
///
/// # Errors
/// A filesystem error creating `dest_dir`, clearing a read-only target, or
/// renaming into place (after the transient-lock retries in
/// [`crate::fs_util::rename_with_retry`]).
pub fn install_pak(staged: &Path, dest_dir: &Path, pak_filename: &str) -> io::Result<()> {
    fs::create_dir_all(dest_dir).map_err(|e| annotate(e, "create paks dir", dest_dir))?;
    let dest = pak_path(dest_dir, pak_filename);
    // A previously-installed (or server-pushed) pak may be read-only; clear it
    // so the rename can replace it. Best-effort: an absent target is fine.
    clear_readonly(&dest).map_err(|e| annotate(e, "clear read-only on existing pak", &dest))?;
    rename_with_retry(staged, &dest).map_err(|e| annotate(e, "move pak into place", &dest))
}

/// Remove `pak_filename` from `dest_dir`, tolerating an already-absent file.
///
/// Clears a read-only attribute first (a server-pushed pak can land read-only).
///
/// # Errors
/// A filesystem error other than the file already being gone.
pub fn remove_pak(dest_dir: &Path, pak_filename: &str) -> io::Result<()> {
    let dest = pak_path(dest_dir, pak_filename);
    let _ = clear_readonly(&dest);
    match fs::remove_file(&dest) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(annotate(e, "remove pak", &dest)),
    }
}

/// Rename an installed pak from `from_filename` to `to_filename` within
/// `dest_dir` — the planner's "same content, new filename" keep case, so the
/// launcher renames in place rather than re-downloading identical bytes.
///
/// A no-op (`Ok`) when the names are equal or the source is absent: there is
/// nothing to move, and the caller corrects its recorded state regardless.
///
/// # Errors
/// A filesystem error other than the source being absent.
pub fn rename_pak(dest_dir: &Path, from_filename: &str, to_filename: &str) -> io::Result<()> {
    if from_filename == to_filename {
        return Ok(());
    }
    let from = pak_path(dest_dir, from_filename);
    if !from.exists() {
        return Ok(());
    }
    let to = pak_path(dest_dir, to_filename);
    let _ = clear_readonly(&to);
    rename_with_retry(&from, &to).map_err(|e| annotate(e, "rename pak", &to))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Write `bytes` to a staging file inside `dir` and return its path.
    fn staged(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn install_pak_places_into_a_fresh_dir() {
        let tmp = TempDir::new().unwrap();
        let paks = tmp.path().join("DownloadedPaks"); // does not exist yet
        let src = staged(tmp.path(), ".Elim.incoming", b"pak-bytes");

        install_pak(&src, &paks, "ElimPlusMutator-WindowsNoEditor.pak").unwrap();

        let dest = paks.join("ElimPlusMutator-WindowsNoEditor.pak");
        assert_eq!(fs::read(&dest).unwrap(), b"pak-bytes");
        assert!(
            !src.exists(),
            "staging file should have been moved, not copied"
        );
    }

    #[test]
    fn install_pak_replaces_a_readonly_existing_pak() {
        let tmp = TempDir::new().unwrap();
        let paks = tmp.path().join("DownloadedPaks");
        fs::create_dir_all(&paks).unwrap();

        // An existing, read-only pak (as a server push or prior install can leave).
        let dest = paks.join("Wipeout.pak");
        fs::write(&dest, b"old").unwrap();
        let mut perms = fs::metadata(&dest).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&dest, perms).unwrap();

        let src = staged(&paks, ".Wipeout.incoming", b"new-bytes");
        install_pak(&src, &paks, "Wipeout.pak").unwrap();

        assert_eq!(fs::read(&dest).unwrap(), b"new-bytes");
    }

    #[test]
    fn remove_pak_tolerates_absent_and_deletes_present() {
        let tmp = TempDir::new().unwrap();
        let paks = tmp.path().join("DownloadedPaks");
        fs::create_dir_all(&paks).unwrap();

        // Absent → Ok, no error.
        remove_pak(&paks, "ghost.pak").unwrap();

        // Present → deleted.
        let f = paks.join("real.pak");
        fs::write(&f, b"x").unwrap();
        remove_pak(&paks, "real.pak").unwrap();
        assert!(!f.exists());
    }

    #[test]
    fn rename_pak_moves_when_present_and_noops_otherwise() {
        let tmp = TempDir::new().unwrap();
        let paks = tmp.path().join("DownloadedPaks");
        fs::create_dir_all(&paks).unwrap();

        // Equal names → no-op.
        rename_pak(&paks, "same.pak", "same.pak").unwrap();

        // Absent source → no-op (no error).
        rename_pak(&paks, "absent.pak", "whatever.pak").unwrap();

        // Present source → moved to the new name.
        fs::write(paks.join("old-name.pak"), b"content").unwrap();
        rename_pak(&paks, "old-name.pak", "new-name.pak").unwrap();
        assert!(!paks.join("old-name.pak").exists());
        assert_eq!(fs::read(paks.join("new-name.pak")).unwrap(), b"content");
    }
}
