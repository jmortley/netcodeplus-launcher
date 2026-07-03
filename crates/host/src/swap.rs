//! In-place binary self-update swap, shared by the **Windows exe** and the
//! **Linux AppImage** self-updaters.
//!
//! Both replace the running launcher at its OWN on-disk path rather than
//! writing a new, differently-named file. That is the whole point: a desktop
//! shortcut, taskbar pin, or `.desktop` entry keeps pointing at the same path
//! across every update, so it never goes stale and the user never accumulates
//! duplicate/broken icons. Windows allows renaming a running image file (the
//! lock blocks overwriting its bytes, not moving the directory entry), so the
//! same rename dance works on both platforms.
//!
//! Sequence: the verified download lands at `<binary>.update`; the running
//! binary is renamed aside to `<binary>.old` (a rollback point, swept on the
//! next start); `<binary>.update` is renamed onto the original path. All three
//! are same-directory renames, so each step is atomic and the path is never
//! left dangling — on the final rename failing, the old build is restored.

use std::path::{Path, PathBuf};

use thiserror::Error;

/// The swap's sibling paths: `(<binary>.update, <binary>.old)`.
#[must_use]
pub fn swap_paths(binary: &Path) -> (PathBuf, PathBuf) {
    let base = binary.as_os_str().to_string_lossy();
    (
        PathBuf::from(format!("{base}.update")),
        PathBuf::from(format!("{base}.old")),
    )
}

/// Failure modes for [`apply_binary_swap`]. Every variant leaves the original
/// path holding a runnable binary (the old build), except
/// [`Self::IntoPlaceNotRestored`] — the double-fault where even restoring the
/// old build failed, which names where the old file still lives.
#[derive(Debug, Error)]
pub enum SwapError {
    /// Setting the exec bit on the verified download failed (unix/AppImage).
    #[error("couldn't make the new launcher executable: {0}")]
    MakeExecutable(std::io::Error),
    /// Moving the current binary aside to `.old` failed.
    #[error("couldn't move the current launcher aside: {0}")]
    MoveAside(std::io::Error),
    /// Moving the new build into place failed; the old build WAS restored.
    #[error("couldn't move the new launcher into place (the old build was restored): {0}")]
    IntoPlaceRestored(std::io::Error),
    /// Moving the new build into place failed AND the restore failed too — the
    /// original path is empty; the old build survives at `old`.
    #[error(
        "couldn't move the new launcher into place, and putting the old build back failed too — your launcher is still at {old:?} (rename it back): {swap}"
    )]
    IntoPlaceNotRestored {
        /// Where the old (still runnable) build sits.
        old: PathBuf,
        /// The error that failed the swap.
        swap: std::io::Error,
    },
}

/// Swap the (already hash-verified) `update` file onto `binary`'s own path:
/// optionally set its exec bit, rename `binary` aside to `<binary>.old`, then
/// rename `update` onto `binary`.
///
/// `executable` sets the unix `0o755` bit before the swap (needed for an
/// AppImage; a no-op on Windows). `update` normally comes from [`swap_paths`]
/// (a same-directory sibling, so the renames are atomic); it is a parameter so
/// tests can stage it elsewhere (e.g. another filesystem, to force the rename
/// to fail).
///
/// Failure handling: the staged `update` is removed on every failure path (a
/// verified-but-unapplied binary must not linger), and if the final rename
/// fails the old build is restored so the original path never dangles — the
/// error says whether that restore worked.
///
/// # Errors
/// See [`SwapError`].
pub fn apply_binary_swap(binary: &Path, update: &Path, executable: bool) -> Result<(), SwapError> {
    let (_, old) = swap_paths(binary);

    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(update, std::fs::Permissions::from_mode(0o755)) {
            let _ = std::fs::remove_file(update);
            return Err(SwapError::MakeExecutable(e));
        }
    }
    #[cfg(not(unix))]
    let _ = executable; // no exec bit on Windows

    if let Err(e) = std::fs::rename(binary, &old) {
        let _ = std::fs::remove_file(update);
        return Err(SwapError::MoveAside(e));
    }
    if let Err(e) = std::fs::rename(update, binary) {
        let _ = std::fs::remove_file(update);
        return match std::fs::rename(&old, binary) {
            Ok(()) => Err(SwapError::IntoPlaceRestored(e)),
            Err(_) => Err(SwapError::IntoPlaceNotRestored { old, swap: e }),
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn stage(dir: &Path) -> (PathBuf, PathBuf) {
        let binary = dir.join("launcher.bin");
        fs::write(&binary, b"old-build").unwrap();
        let (update, _) = swap_paths(&binary);
        fs::write(&update, b"new-build").unwrap();
        (binary, update)
    }

    #[test]
    fn swap_paths_are_same_dir_siblings() {
        let b = Path::new("/a/b/Launcher.exe");
        let (update, old) = swap_paths(b);
        assert_eq!(update, Path::new("/a/b/Launcher.exe.update"));
        assert_eq!(old, Path::new("/a/b/Launcher.exe.old"));
        assert_eq!(update.parent(), b.parent());
        assert_eq!(old.parent(), b.parent());
    }

    #[test]
    fn happy_path_swaps_and_keeps_rollback() {
        let tmp = TempDir::new().unwrap();
        let (binary, update) = stage(tmp.path());
        apply_binary_swap(&binary, &update, false).unwrap();
        assert_eq!(fs::read(&binary).unwrap(), b"new-build");
        let (_, old) = swap_paths(&binary);
        assert_eq!(fs::read(&old).unwrap(), b"old-build");
        assert!(!update.exists());
    }

    #[cfg(unix)]
    #[test]
    fn executable_sets_the_exec_bit() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let (binary, update) = stage(tmp.path());
        apply_binary_swap(&binary, &update, true).unwrap();
        let mode = fs::metadata(&binary).unwrap().permissions().mode();
        assert_ne!(mode & 0o111, 0, "exec bit must be set, got {mode:o}");
    }

    #[test]
    fn stale_old_from_a_prior_swap_is_overwritten() {
        let tmp = TempDir::new().unwrap();
        let (binary, update) = stage(tmp.path());
        let (_, old) = swap_paths(&binary);
        fs::write(&old, b"stale-old").unwrap();
        apply_binary_swap(&binary, &update, false).unwrap();
        assert_eq!(fs::read(&old).unwrap(), b"old-build");
    }

    #[test]
    fn missing_update_leaves_the_binary_untouched() {
        let tmp = TempDir::new().unwrap();
        let binary = tmp.path().join("launcher.bin");
        fs::write(&binary, b"old-build").unwrap();
        let (update, _) = swap_paths(&binary);
        // Non-executable swap: the first op is the rename-aside, which needs the
        // update to exist for the final rename — force failure by leaving it
        // absent so the second rename fails and rolls back.
        let err = apply_binary_swap(&binary, &update, false).unwrap_err();
        assert!(
            matches!(err, SwapError::IntoPlaceRestored(_)),
            "expected rollback, got {err}"
        );
        assert_eq!(fs::read(&binary).unwrap(), b"old-build");
        let (_, old) = swap_paths(&binary);
        assert!(!old.exists(), ".old should be cleared by the rollback");
    }

    #[cfg(unix)]
    #[test]
    fn executable_missing_update_fails_at_chmod() {
        let tmp = TempDir::new().unwrap();
        let binary = tmp.path().join("launcher.bin");
        fs::write(&binary, b"old-build").unwrap();
        let (update, _) = swap_paths(&binary);
        let err = apply_binary_swap(&binary, &update, true).unwrap_err();
        assert!(matches!(err, SwapError::MakeExecutable(_)), "{err}");
        assert_eq!(fs::read(&binary).unwrap(), b"old-build");
    }

    #[cfg(unix)]
    #[test]
    fn failed_final_rename_restores_the_old_build() {
        // Force rename #2 to fail with EXDEV by staging the update on a
        // different filesystem (tmpfs). Skip where /dev/shm isn't a usable
        // tmpfs (macOS); the Linux CI runner has it.
        use std::os::unix::fs::MetadataExt;
        let shm = Path::new("/dev/shm");
        if !shm.is_dir() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let binary = tmp.path().join("launcher.bin");
        fs::write(&binary, b"old-build").unwrap();
        let update = shm.join(format!("ncp-swap-test-{}.update", std::process::id()));
        fs::write(&update, b"new-build").unwrap();

        let result = apply_binary_swap(&binary, &update, false);
        let _ = fs::remove_file(&update);
        if tmp.path().metadata().unwrap().dev() == shm.metadata().unwrap().dev() {
            return; // same filesystem after all — EXDEV can't be forced here
        }
        let err = result.unwrap_err();
        assert!(matches!(err, SwapError::IntoPlaceRestored(_)), "{err}");
        assert_eq!(fs::read(&binary).unwrap(), b"old-build");
        let (_, old) = swap_paths(&binary);
        assert!(!old.exists());
    }
}
