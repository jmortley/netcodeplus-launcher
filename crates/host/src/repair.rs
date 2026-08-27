//! Client cache repair — the "delete webcache" folklore fix as a button.
//!
//! Two caches corrupt in the field and cause silent crash-to-desktop on join,
//! worst on stripped-Windows machines with no crash reporting (DGLUXMODEL
//! incident, 2026-08-22):
//!
//! - `Documents\UnrealTournament\Saved\webcache` — the embedded CEF browser
//!   cache (Cookies, IndexedDB, ChromeDWriteFontCache). A crash mid-write
//!   corrupts it; the game then dies loading the web UI with no error.
//! - `<install>\UnrealTournament\PersistentDownloadDir\EMS` — chunk/content
//!   download cache under the install root.
//!
//! Both are pure caches the game regenerates on next launch, so deletion is
//! always safe **while the game is closed** — the running-game refusal lives
//! at the command layer with its siblings ([`shipping_client_running`]-style).
//!
//! [`clear_cache_dir`] hard-refuses any directory whose leaf is not one of the
//! two known cache names: these deletions are recursive, and a refactor that
//! ever pointed this at `Saved/` proper must fail loudly, not recurse quietly.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Leaf directory names [`clear_cache_dir`] is willing to delete.
const CLEARABLE_CACHE_LEAVES: [&str; 2] = ["webcache", "EMS"];

/// The per-user CEF web cache: `<Documents>/UnrealTournament/Saved/webcache`.
/// Resolved via the Documents known folder (OneDrive-redirect aware), the same
/// way [`crate::install::default_mod_paks_dir`] finds `DownloadedPaks`.
/// `None` when the platform reports no Documents folder.
#[must_use]
pub fn webcache_dir() -> Option<PathBuf> {
    let docs = dirs::document_dir()?;
    Some(docs.join("UnrealTournament").join("Saved").join("webcache"))
}

/// The per-install EMS download cache:
/// `<root>/UnrealTournament/PersistentDownloadDir/EMS`.
#[must_use]
pub fn ems_dir(install_root: &Path) -> PathBuf {
    install_root
        .join("UnrealTournament")
        .join("PersistentDownloadDir")
        .join("EMS")
}

/// Recursively delete one known cache directory.
///
/// Returns `Ok(true)` when the directory existed and was removed, `Ok(false)`
/// when it was already absent (both count as "cache is clean"). Any other I/O
/// failure propagates so the UI can show it — a locked file inside means the
/// deletion was partial and the player should retry with the game closed.
///
/// # Errors
/// - [`io::ErrorKind::InvalidInput`] if `dir`'s leaf name is not a known cache
///   name — this function refuses to be a general recursive delete.
/// - Any filesystem error from the removal itself.
pub fn clear_cache_dir(dir: &Path) -> io::Result<bool> {
    let leaf_ok = dir
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| CLEARABLE_CACHE_LEAVES.contains(&n));
    if !leaf_ok {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to delete non-cache directory: {}", dir.display()),
        ));
    }
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ems_dir_is_under_persistent_download_dir() {
        let d = ems_dir(Path::new("C:/Games/UT4"));
        let s = d.to_string_lossy().replace('\\', "/");
        assert_eq!(s, "C:/Games/UT4/UnrealTournament/PersistentDownloadDir/EMS");
    }

    #[test]
    fn clears_an_existing_cache_and_reports_absent_as_clean() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("webcache");
        fs::create_dir_all(cache.join("Cache_Data")).unwrap();
        fs::write(cache.join("Cookies"), b"crumbs").unwrap();

        assert!(clear_cache_dir(&cache).unwrap(), "existed -> removed");
        assert!(!cache.exists());
        assert!(!clear_cache_dir(&cache).unwrap(), "absent -> already clean");
    }

    #[test]
    fn refuses_to_delete_a_directory_that_is_not_a_known_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let precious = tmp.path().join("Saved");
        fs::create_dir_all(&precious).unwrap();

        let err = clear_cache_dir(&precious).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(precious.exists(), "refusal must not touch the directory");
    }

    #[test]
    fn ems_leaf_is_clearable_too() {
        let tmp = tempfile::tempdir().unwrap();
        let ems = tmp.path().join("EMS");
        fs::create_dir_all(&ems).unwrap();
        assert!(clear_cache_dir(&ems).unwrap());
    }
}
