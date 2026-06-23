//! Small filesystem helpers shared by the plugin and pak install sinks.
//!
//! Both sinks place freshly-downloaded bytes over a possibly-existing file or
//! folder on Windows, where a transient lock (an AV mid-scan, File Explorer, a
//! OneDrive sync pass, the game still closing) turns a plain `fs::rename` into a
//! spurious access-denied / sharing-violation. These helpers ride out those
//! transient locks, clear a read-only attribute that would block a replace, and
//! annotate a bare I/O error with the step + path that failed.

use std::io;
use std::path::Path;

/// Whether an error is a transient file-lock worth retrying (vs. a hard
/// failure). Covers the portable `PermissionDenied` plus the raw Windows
/// `ERROR_ACCESS_DENIED` (5) and `ERROR_SHARING_VIOLATION` (32).
pub(crate) fn is_transient_lock(e: &io::Error) -> bool {
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    e.kind() == io::ErrorKind::PermissionDenied
        || matches!(
            e.raw_os_error(),
            Some(ERROR_ACCESS_DENIED) | Some(ERROR_SHARING_VIOLATION)
        )
}

/// `fs::rename`, retried briefly on a transient lock. Renaming a path Windows
/// has an open handle on (File Explorer showing the folder, an AV mid-scan of a
/// freshly-written file, OneDrive syncing it, the game still closing) fails with
/// access-denied / sharing-violation; those clear in moments, so a few short
/// backoff retries ride through them without making the user do anything. A
/// persistent failure (e.g. the game actually running) still surfaces after the
/// last attempt. ~1.5s worst case (50+100+200+400+800ms).
pub(crate) fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    const BACKOFFS_MS: [u64; 5] = [50, 100, 200, 400, 800];
    let mut last = match std::fs::rename(from, to) {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    for delay in BACKOFFS_MS {
        // Only retry transient lock errors; a non-lock failure won't fix itself.
        if !is_transient_lock(&last) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(delay));
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Wrap an `io::Error` with which step failed and on which path, so a bare
/// "Access is denied" becomes actionable ("move pak into place: <path>: …").
pub(crate) fn annotate(e: io::Error, step: &str, path: &Path) -> io::Error {
    io::Error::new(e.kind(), format!("{step}: {}: {e}", path.display()))
}

/// Clear the read-only attribute on `path` so an atomic rename-over can replace
/// it. Best-effort by design: an absent `path` (nothing to replace yet) is
/// `Ok(())`, and only a genuine metadata/permission I/O error propagates.
///
/// `set_readonly(false)` here is deliberately the **Windows** semantic — it
/// clears `FILE_ATTRIBUTE_READONLY` so the existing pak can be deleted/replaced
/// (Windows refuses both on a read-only file). The clippy lint warns this makes
/// a file world-writable *on Unix*; that's not a concern here — pak install is
/// Windows-only for v1, and the target is a file the launcher owns inside the
/// user's own Documents tree.
#[allow(clippy::permissions_set_readonly_false)]
pub(crate) fn clear_readonly(path: &Path) -> io::Result<()> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut perms = meta.permissions();
    if perms.readonly() {
        perms.set_readonly(false);
        std::fs::set_permissions(path, perms)?;
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

    #[test]
    fn clear_readonly_is_ok_for_absent_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        // No file at this path → nothing to clear, must not error.
        clear_readonly(&tmp.path().join("nope.pak")).unwrap();
    }

    #[test]
    fn clear_readonly_makes_a_readonly_file_writable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("ro.pak");
        std::fs::write(&f, b"x").unwrap();
        let mut perms = std::fs::metadata(&f).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&f, perms).unwrap();
        assert!(std::fs::metadata(&f).unwrap().permissions().readonly());

        clear_readonly(&f).unwrap();
        assert!(!std::fs::metadata(&f).unwrap().permissions().readonly());
    }
}
