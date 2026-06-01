//! Post-update housekeeping: detect when the launcher has been updated to a new
//! exe (a newer version started from a different path than the one last
//! recorded) and create a fresh Desktop shortcut pointing at it.
//!
//! This pairs with the notify-only update flow. After the user downloads and
//! runs a newer launcher, the previous copy and any shortcut to it are stale;
//! the launcher offers to point a new Desktop shortcut at the new exe and to
//! remove the outdated one. Shortcut creation is Windows-only (the only target
//! that uses `.lnk` files); other platforms get an `Unsupported` stub so the
//! crate still builds (CI runs on Linux).

use std::path::{Path, PathBuf};

use semver::Version;

/// Errors creating a desktop shortcut.
#[derive(Debug, thiserror::Error)]
pub enum ShortcutError {
    /// The shortcut target file does not exist.
    #[error("the shortcut target does not exist: {0}")]
    TargetMissing(PathBuf),
    /// The Desktop folder could not be located.
    #[error("could not locate the Desktop folder")]
    NoDesktop,
    /// A path could not be represented as UTF-8 for the shortcut writer.
    #[error("path is not valid UTF-8: {0}")]
    NonUtf8Path(PathBuf),
    /// Writing the `.lnk` failed.
    #[error("failed to write the shortcut: {0}")]
    Write(String),
    /// Shortcut creation was attempted on a non-Windows platform.
    #[error("creating desktop shortcuts is only supported on Windows")]
    Unsupported,
}

/// True if `a` and `b` are different exe paths. Case-insensitive on Windows
/// (NTFS is case-insensitive), exact elsewhere. Pure (no filesystem access), so
/// the recorded-vs-running comparison is deterministic and testable.
#[must_use]
fn paths_differ(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        !a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
    }
    #[cfg(not(windows))]
    {
        a != b
    }
}

/// Decide whether the running launcher is a post-update move and, if so, return
/// the previous launcher exe to offer removing.
///
/// Returns `Some(old_path)` only when ALL hold: a path + version were
/// previously recorded, the recorded version is **strictly older** than
/// `current_version` (a genuine upgrade — so running two copies of the same
/// build never nags, nor offers to delete a same-version dev rebuild), and the
/// recorded path differs from the one running now. Pure: the caller must
/// confirm the returned path still exists on disk before acting on it.
#[must_use]
pub fn detect_outdated_launcher(
    current_path: &Path,
    current_version: &Version,
    recorded_path: Option<&str>,
    recorded_version: Option<&str>,
) -> Option<PathBuf> {
    let recorded_path = PathBuf::from(recorded_path?);
    let recorded_version = Version::parse(recorded_version?).ok()?;
    if recorded_version >= *current_version {
        return None;
    }
    if !paths_differ(&recorded_path, current_path) {
        return None;
    }
    Some(recorded_path)
}

/// Whether a recorded `pending_old_launcher_path` should be discarded rather
/// than offered for cleanup. Pure (the caller supplies whether the file still
/// exists), so the decision is deterministic and testable.
///
/// A pending old-launcher is stale when any of these hold:
/// - its file is gone (`exists == false`) — nothing left to remove;
/// - it is the launcher running right now (same path) — you can't tidy yourself
///   away, which is exactly what happens when an older build is re-launched
///   after a newer one had recorded it as the "old" copy;
/// - this run is **older** than the previous one (`current_version <
///   previous_version`) — the user went back to an earlier build, so the
///   "you just updated, here's the old one" prompt no longer applies.
#[must_use]
pub fn is_stale_pending(
    pending_path: &Path,
    exists: bool,
    current_path: &Path,
    current_version: &Version,
    previous_version: Option<&str>,
) -> bool {
    if !exists {
        return true;
    }
    if !paths_differ(pending_path, current_path) {
        return true;
    }
    if let Some(prev) = previous_version.and_then(|v| Version::parse(v).ok()) {
        if *current_version < prev {
            return true;
        }
    }
    false
}

/// Create (overwriting any existing) a Desktop shortcut named `<name>.lnk`
/// pointing at `target`. Returns the path of the written `.lnk`.
///
/// # Errors
/// [`ShortcutError::TargetMissing`] if `target` is not a file,
/// [`ShortcutError::NoDesktop`] if the Desktop folder can't be found,
/// [`ShortcutError::NonUtf8Path`] for a non-UTF-8 target/shortcut path,
/// [`ShortcutError::Write`] on a write failure, and
/// [`ShortcutError::Unsupported`] on non-Windows platforms.
#[cfg(windows)]
pub fn create_desktop_shortcut(target: &Path, name: &str) -> Result<PathBuf, ShortcutError> {
    if !target.is_file() {
        return Err(ShortcutError::TargetMissing(target.to_path_buf()));
    }
    let target_str = target
        .to_str()
        .ok_or_else(|| ShortcutError::NonUtf8Path(target.to_path_buf()))?;
    let desktop = dirs::desktop_dir().ok_or(ShortcutError::NoDesktop)?;
    let lnk = desktop.join(format!("{name}.lnk"));
    let lnk_str = lnk
        .to_str()
        .ok_or_else(|| ShortcutError::NonUtf8Path(lnk.clone()))?;
    let sl = mslnk::ShellLink::new(target_str).map_err(|e| ShortcutError::Write(e.to_string()))?;
    sl.create_lnk(lnk_str)
        .map_err(|e| ShortcutError::Write(e.to_string()))?;
    Ok(lnk)
}

/// Non-Windows stub: there are no `.lnk` shortcuts to create.
#[cfg(not(windows))]
pub fn create_desktop_shortcut(_target: &Path, _name: &str) -> Result<PathBuf, ShortcutError> {
    Err(ShortcutError::Unsupported)
}

/// File name (without the `.lnk` extension) of the canonical Desktop shortcut the
/// launcher manages — shared by the writer (the caller of
/// [`create_desktop_shortcut`]) and the staleness check below, so the two can't
/// drift apart.
pub const LAUNCHER_SHORTCUT_NAME: &str = "UT4 Community Launcher";

/// Whether the canonical Desktop shortcut (`UT4 Community Launcher.lnk`) exists
/// but points somewhere other than `current_exe` — i.e. it went stale after a
/// notify-update and is worth offering to repoint.
///
/// Returns `false` when there is no such shortcut (the user launches the exe
/// directly, or named their shortcut differently — we must not nag them to make
/// one), when it already points here, or when its target can't be read. The
/// repoint itself is [`create_desktop_shortcut`], which overwrites that same name.
#[cfg(windows)]
#[must_use]
pub fn desktop_shortcut_is_stale(current_exe: &Path) -> bool {
    use lnk::encoding::WINDOWS_1252;
    use lnk::ShellLink;

    let Some(desktop) = dirs::desktop_dir() else {
        return false;
    };
    let lnk = desktop.join(format!("{LAUNCHER_SHORTCUT_NAME}.lnk"));
    if !lnk.is_file() {
        return false;
    }
    let Ok(link) = ShellLink::open(&lnk, WINDOWS_1252) else {
        return false;
    };
    let Some(target) = link.link_target() else {
        return false;
    };
    // Stale = the shortcut points at a different exe than the one running now.
    paths_differ(&PathBuf::from(target), current_exe)
}

/// Non-Windows stub: there are no `.lnk` shortcuts to evaluate.
#[cfg(not(windows))]
#[must_use]
pub fn desktop_shortcut_is_stale(_current_exe: &Path) -> bool {
    false
}

/// Schedule `path` for deletion on the next reboot.
///
/// The fallback for removing the previous launcher exe when it can't be deleted
/// now because it is locked — Windows locks a running image file, so if the old
/// launcher is still open, `remove_file` fails. `MoveFileExW(path, NULL,
/// MOVEFILE_DELAY_UNTIL_REBOOT)` records the delete in the registry's
/// `PendingFileRenameOperations`, which the kernel applies during early boot,
/// before the file is locked again. No elevation is needed for an exe the user
/// could otherwise manage.
///
/// # Errors
/// The underlying OS error (as [`std::io::Error`]) if the API call fails, or an
/// `Unsupported` error on non-Windows targets.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn schedule_delete_on_reboot(path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_DELAY_UNTIL_REBOOT};

    // NUL-terminated UTF-16, encoded straight from the OS string (no lossy
    // round-trip through UTF-8) for the `*W` API.
    let wide_path: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: `wide_path` is a NUL-terminated UTF-16 buffer that outlives the
    // call. The destination is NULL — with MOVEFILE_DELAY_UNTIL_REBOOT that
    // records a delete (not a rename) in PendingFileRenameOperations, applied
    // during early boot. MoveFileExW reads the path and returns no handle or
    // allocation we must own.
    let ok = unsafe {
        MoveFileExW(
            wide_path.as_ptr(),
            std::ptr::null(),
            MOVEFILE_DELAY_UNTIL_REBOOT,
        )
    };
    if ok == 0 {
        // SAFETY: GetLastError takes no args and only reads thread-local state.
        let err = unsafe { GetLastError() };
        return Err(std::io::Error::from_raw_os_error(err as i32));
    }
    Ok(())
}

/// Non-Windows stub: there is no reboot-time delete queue to schedule onto.
#[cfg(not(windows))]
pub fn schedule_delete_on_reboot(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "scheduling a delete on reboot is only supported on Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn detects_a_genuine_upgrade_from_a_new_path() {
        let old = detect_outdated_launcher(
            Path::new(r"C:\Users\me\Downloads\launcher-0.2.0.exe"),
            &v("0.2.0"),
            Some(r"C:\old\launcher-0.1.0.exe"),
            Some("0.1.0"),
        );
        assert_eq!(old, Some(PathBuf::from(r"C:\old\launcher-0.1.0.exe")));
    }

    #[test]
    fn no_prompt_for_same_version_different_path() {
        // Two copies of the same version (e.g. a dev rebuild) must not be
        // treated as an update — it would otherwise offer to delete the dev exe.
        assert!(detect_outdated_launcher(
            Path::new(r"C:\b\launcher.exe"),
            &v("0.2.0"),
            Some(r"C:\a\launcher.exe"),
            Some("0.2.0"),
        )
        .is_none());
    }

    #[test]
    fn no_prompt_for_same_path() {
        // Higher version but identical path = an in-place run, nothing to clean.
        assert!(detect_outdated_launcher(
            Path::new(r"C:\a\launcher.exe"),
            &v("0.2.0"),
            Some(r"C:\a\launcher.exe"),
            Some("0.1.0"),
        )
        .is_none());
    }

    #[test]
    fn no_prompt_on_first_run() {
        // Nothing recorded yet (a pre-tracking build, or a fresh state file).
        assert!(
            detect_outdated_launcher(Path::new(r"C:\a\launcher.exe"), &v("0.2.0"), None, None)
                .is_none()
        );
    }

    #[test]
    fn no_prompt_on_downgrade() {
        // Running an OLDER build than recorded is not an upgrade.
        assert!(detect_outdated_launcher(
            Path::new(r"C:\b\launcher.exe"),
            &v("0.1.0"),
            Some(r"C:\a\launcher.exe"),
            Some("0.2.0"),
        )
        .is_none());
    }

    #[cfg(windows)]
    #[test]
    fn path_compare_is_case_insensitive_on_windows() {
        assert!(!paths_differ(
            Path::new(r"C:\A\Launcher.exe"),
            Path::new(r"c:\a\launcher.exe")
        ));
    }

    #[test]
    fn pending_is_stale_when_file_is_gone() {
        assert!(is_stale_pending(
            Path::new(r"C:\old\launcher-0.2.2.exe"),
            false, // file no longer on disk
            Path::new(r"C:\new\launcher-0.2.4.exe"),
            &v("0.2.4"),
            Some("0.2.3"),
        ));
    }

    #[test]
    fn pending_is_stale_when_it_is_the_running_launcher() {
        // The reported bug: run 0.2.3 (records the 0.2.2 exe as pending), then
        // run that same 0.2.2 exe again — it must not offer to remove itself.
        assert!(is_stale_pending(
            Path::new(r"C:\Users\me\Desktop\launcher-0.2.2.exe"),
            true,
            Path::new(r"C:\Users\me\Desktop\launcher-0.2.2.exe"),
            &v("0.2.2"),
            Some("0.2.3"),
        ));
    }

    #[cfg(windows)]
    #[test]
    fn pending_running_match_is_case_insensitive() {
        assert!(is_stale_pending(
            Path::new(r"C:\A\Launcher.exe"),
            true,
            Path::new(r"c:\a\launcher.exe"),
            &v("0.2.2"),
            Some("0.2.3"),
        ));
    }

    #[test]
    fn pending_is_stale_on_version_regression_from_a_different_path() {
        // Ran a newer build, ignored cleanup, then ran an OLDER build from a
        // different path: the "you just updated" card must not resurface.
        assert!(is_stale_pending(
            Path::new(r"C:\a\launcher-0.2.2.exe"),
            true,
            Path::new(r"C:\b\launcher-0.2.2.exe"),
            &v("0.2.2"),
            Some("0.2.3"),
        ));
    }

    #[test]
    fn pending_survives_a_genuine_forward_update() {
        // The normal case: on the new build, the recorded old exe is still a
        // valid cleanup target.
        assert!(!is_stale_pending(
            Path::new(r"C:\old\launcher-0.2.3.exe"),
            true,
            Path::new(r"C:\new\launcher-0.2.4.exe"),
            &v("0.2.4"),
            Some("0.2.3"),
        ));
    }

    #[test]
    fn pending_survives_when_no_previous_version_recorded() {
        // No previous version to regress against; a different, existing path is
        // a legitimate cleanup target.
        assert!(!is_stale_pending(
            Path::new(r"C:\old\launcher-0.2.3.exe"),
            true,
            Path::new(r"C:\new\launcher-0.2.4.exe"),
            &v("0.2.4"),
            None,
        ));
    }
}
