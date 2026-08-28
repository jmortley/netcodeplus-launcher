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
pub fn create_desktop_shortcut(target: &Path, name: &str) -> Result<PathBuf, ShortcutError> {
    create_desktop_shortcut_with(target, name, None, None)
}

/// [`create_desktop_shortcut`] with the full shortcut shape: launch
/// `arguments` and a `working_dir`, both written into the `.lnk`. Needed for
/// targets that don't run bare — UE4Editor.exe without its project argument
/// opens a project browser instead of the UT4 editor.
///
/// # Errors
/// Same as [`create_desktop_shortcut`].
#[cfg(windows)]
pub fn create_desktop_shortcut_with(
    target: &Path,
    name: &str,
    arguments: Option<&str>,
    working_dir: Option<&Path>,
) -> Result<PathBuf, ShortcutError> {
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
    let mut sl =
        mslnk::ShellLink::new(target_str).map_err(|e| ShortcutError::Write(e.to_string()))?;
    if let Some(args) = arguments {
        sl.set_arguments(Some(args.to_string()));
    }
    if let Some(dir) = working_dir {
        let dir_str = dir
            .to_str()
            .ok_or_else(|| ShortcutError::NonUtf8Path(dir.to_path_buf()))?;
        sl.set_working_dir(Some(dir_str.to_string()));
    }
    sl.create_lnk(lnk_str)
        .map_err(|e| ShortcutError::Write(e.to_string()))?;
    Ok(lnk)
}

/// Non-Windows stub: there are no `.lnk` shortcuts to create.
#[cfg(not(windows))]
pub fn create_desktop_shortcut_with(
    _target: &Path,
    _name: &str,
    _arguments: Option<&str>,
    _working_dir: Option<&Path>,
) -> Result<PathBuf, ShortcutError> {
    Err(ShortcutError::Unsupported)
}

/// File name (without the `.lnk` extension) of the canonical Desktop shortcut the
/// launcher manages — shared by the writer (the caller of
/// [`create_desktop_shortcut`]) and the repoint below, so the two can't drift
/// apart.
pub const LAUNCHER_SHORTCUT_NAME: &str = "UT4 Community Launcher";

/// Outcome of [`repoint_launcher_shortcut_if_present`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutRepoint {
    /// No canonical `UT4 Community Launcher.lnk` on the Desktop — the user
    /// launches the exe directly (or named their shortcut differently), so
    /// there is nothing to touch and no icon is created.
    NoShortcut,
    /// The canonical shortcut existed and was (re)written to point at the exe
    /// running now — overwriting the same file, never a second icon.
    Repointed,
    /// The canonical shortcut existed but couldn't be rewritten (e.g. it's
    /// locked or the Desktop isn't writable). The caller may surface a manual
    /// "update shortcut" affordance.
    Failed,
}

/// Keep the canonical Desktop shortcut (`UT4 Community Launcher.lnk`) pointing at
/// the exe running now — but only if it already exists.
///
/// Deliberately does NOT read the existing shortcut's target: the `.lnk` the
/// launcher writes (via `mslnk`) stores its target in the ID-list / relative
/// path, which the read-side `lnk` crate can't resolve, so a target-comparison
/// is unreliable. Instead this overwrites the *same* file in place when present
/// — idempotent (the target is stable across in-place updates, so a rewrite is
/// usually a no-op change) and structurally incapable of creating a second
/// icon. A user with no canonical shortcut is left alone ([`Self::NoShortcut`]).
///
/// This heals a shortcut left stale by the old versioned-sibling update scheme
/// (its target exe was deleted); with the in-place swap the target no longer
/// moves, so future launches just rewrite the same path.
#[cfg(windows)]
pub fn repoint_launcher_shortcut_if_present(current_exe: &Path) -> ShortcutRepoint {
    let Some(desktop) = dirs::desktop_dir() else {
        return ShortcutRepoint::NoShortcut;
    };
    let lnk = desktop.join(format!("{LAUNCHER_SHORTCUT_NAME}.lnk"));
    if !lnk.is_file() {
        return ShortcutRepoint::NoShortcut;
    }
    match create_desktop_shortcut(current_exe, LAUNCHER_SHORTCUT_NAME) {
        Ok(_) => ShortcutRepoint::Repointed,
        Err(_) => ShortcutRepoint::Failed,
    }
}

/// File name of the freedesktop `.desktop` entry the launcher manages on Linux
/// — the app-grid / dock entry. Deterministic: it is the AppImage's embedded
/// desktop file (named after the `netcodeplus-launcher` binary), copied into
/// `~/.local/share/applications/` when the AppImage is desktop-integrated.
#[cfg(not(windows))]
const LINUX_DESKTOP_ENTRY: &str = "netcodeplus-launcher.desktop";

/// The freedesktop `Exec` field code (`%u`, `%U`, `%f`, `%F`) at the end of an
/// `Exec=` value, if any — preserved verbatim when the path is rewritten so the
/// `ncp://` URL still reaches the launcher.
#[cfg(not(windows))]
fn exec_field_code(exec_value: &str) -> Option<&'static str> {
    ["%u", "%U", "%f", "%F"]
        .into_iter()
        .find(|code| exec_value.trim_end().ends_with(code))
}

/// Escape a filesystem path for embedding inside a **double-quoted** freedesktop
/// `Exec` argument. Per the Desktop Entry Spec, `"`, backtick, `$` and `\` must
/// each be escaped with a preceding backslash. The single pass over the original
/// chars escapes the backslash itself correctly (each reserved char, including
/// `\`, gets exactly one `\` prepended). A path is only forbidden `/` and NUL, so
/// these chars are otherwise legal and reachable.
#[cfg(not(windows))]
fn escape_quoted_exec(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        if matches!(ch, '\\' | '"' | '$' | '`') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// (Linux) Rewrite of a `.desktop` file's contents so the `[Desktop Entry]`
/// group's `Exec=` points at `appimage`, or `None` when it already does (or
/// there is no such line). Only the main `[Desktop Entry]` group is touched —
/// never a `[Desktop Action …]` sub-group. The path is written double-quoted
/// with the spec's reserved chars escaped (see [`escape_quoted_exec`]) and any
/// trailing field code preserved; every other line (Icon, StartupWMClass,
/// Categories, MimeType, comments …) is left byte-for-byte intact. Pure — no
/// filesystem access — so it unit-tests on any host.
#[cfg(not(windows))]
#[must_use]
fn desktop_entry_repointed(appimage: &Path, existing: &str) -> Option<String> {
    let appimage = appimage.to_str()?; // a non-UTF-8 path can't be a .desktop Exec
    let mut out: Vec<String> = Vec::new();
    let mut in_main_group = false;
    let mut changed = false;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_main_group = trimmed == "[Desktop Entry]";
            out.push(line.to_string());
            continue;
        }
        if in_main_group {
            if let Some(value) = line.strip_prefix("Exec=") {
                let quoted = escape_quoted_exec(appimage);
                let new_line = match exec_field_code(value) {
                    Some(code) => format!("Exec=\"{quoted}\" {code}"),
                    None => format!("Exec=\"{quoted}\""),
                };
                changed |= new_line != line;
                out.push(new_line);
                continue;
            }
        }
        out.push(line.to_string());
    }
    if !changed {
        return None;
    }
    let mut s = out.join("\n");
    if existing.ends_with('\n') {
        s.push('\n');
    }
    Some(s)
}

/// Atomically overwrite `path` with `contents` (temp file in the same dir +
/// rename), preserving the file's existing permission bits.
#[cfg(not(windows))]
fn overwrite_desktop_entry(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let orig_perms = std::fs::metadata(path).map(|m| m.permissions()).ok();
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(contents.as_bytes())?;
    tmp.as_file().sync_all()?;
    let file = tmp.persist(path).map_err(|e| e.error)?;
    if let Some(perms) = orig_perms {
        let _ = file.set_permissions(perms);
    }
    Ok(())
}

/// (Linux) Keep the managed app-grid / dock `.desktop` entry
/// (`~/.local/share/applications/netcodeplus-launcher.desktop`) pointed at the
/// AppImage running now — but only if it already exists. Heals the case where
/// the AppImage was moved or re-downloaded to a new path: the freedesktop entry
/// keeps its old `Exec=`, so the dock/"show apps" icon launches the stale (or
/// vanished) binary. Overwrites the *same* file in place (never a second icon).
///
/// The passed `_current_exe` is ignored: inside an AppImage `current_exe()` is
/// the transient FUSE mount (`/tmp/.mount_…`), so the real path is taken from
/// the `APPIMAGE` env var instead. When not running as an AppImage (a `.deb`
/// install or a dev build) there is nothing we own to repoint — returns
/// [`ShortcutRepoint::NoShortcut`], so a packaged `.desktop` is never rewritten.
/// The `ncp://` handler entry is repointed separately by the deep-link plugin's
/// `register_all()` on every startup, so it is not touched here.
#[cfg(not(windows))]
pub fn repoint_launcher_shortcut_if_present(_current_exe: &Path) -> ShortcutRepoint {
    let Some(appimage) = crate::linux::appimage_path(std::env::var("APPIMAGE").ok().as_deref())
    else {
        return ShortcutRepoint::NoShortcut; // not an AppImage — nothing we manage
    };
    let Some(entry) = dirs::data_dir().map(|d| d.join("applications").join(LINUX_DESKTOP_ENTRY))
    else {
        return ShortcutRepoint::NoShortcut;
    };
    if !entry.is_file() {
        return ShortcutRepoint::NoShortcut; // desktop-integrated users only
    }
    let Ok(existing) = std::fs::read_to_string(&entry) else {
        return ShortcutRepoint::Failed;
    };
    match desktop_entry_repointed(&appimage, &existing) {
        None => ShortcutRepoint::Repointed, // already correct — a no-op success
        Some(updated) => match overwrite_desktop_entry(&entry, &updated) {
            Ok(()) => ShortcutRepoint::Repointed,
            Err(_) => ShortcutRepoint::Failed,
        },
    }
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

    // ---------- Linux .desktop repoint (pure logic) ---------------------

    #[cfg(not(windows))]
    const SAMPLE_DESKTOP: &str = "[Desktop Entry]\n\
Type=Application\n\
Name=UT4 Community Launcher\n\
Comment=NetcodePlus update launcher and game runner.\n\
Exec=/home/jeremy/Applications/UT4-Community-Launcher.AppImage %u\n\
Icon=netcodeplus-launcher\n\
StartupWMClass=netcodeplus-launcher\n\
Terminal=false\n\
Categories=Game;\n\
MimeType=x-scheme-handler/ncp;\n";

    #[cfg(not(windows))]
    #[test]
    fn desktop_repoint_rewrites_only_exec_and_preserves_field_code() {
        let out = desktop_entry_repointed(
            Path::new("/home/jeremy/Downloads/UT4-Community-Launcher-x86_64.AppImage"),
            SAMPLE_DESKTOP,
        )
        .expect("path changed, so a rewrite is expected");
        assert!(out
            .contains("Exec=\"/home/jeremy/Downloads/UT4-Community-Launcher-x86_64.AppImage\" %u"));
        // The stale path is gone; every other key is untouched.
        assert!(!out.contains("/home/jeremy/Applications/"));
        for keep in [
            "Icon=netcodeplus-launcher",
            "StartupWMClass=netcodeplus-launcher",
            "Categories=Game;",
            "MimeType=x-scheme-handler/ncp;",
        ] {
            assert!(out.contains(keep), "must preserve: {keep}");
        }
        assert!(out.ends_with('\n'), "trailing newline preserved");
    }

    #[cfg(not(windows))]
    #[test]
    fn desktop_repoint_is_noop_when_already_current() {
        let current = "[Desktop Entry]\nExec=\"/apps/cur.AppImage\" %u\nIcon=x\n";
        assert_eq!(
            desktop_entry_repointed(Path::new("/apps/cur.AppImage"), current),
            None,
            "already pointing here → no rewrite"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn desktop_repoint_adds_quotes_for_a_path_with_spaces() {
        let existing = "[Desktop Entry]\nExec=/old/app %u\n";
        let out = desktop_entry_repointed(Path::new("/home/a b/UT4 Launcher.AppImage"), existing)
            .expect("changed");
        assert!(out.contains("Exec=\"/home/a b/UT4 Launcher.AppImage\" %u"));
    }

    #[cfg(not(windows))]
    #[test]
    fn desktop_repoint_escapes_reserved_chars_in_the_path() {
        // A path with the freedesktop-reserved chars ("`, `$`, backtick, `\`)
        // must be escaped inside the double quotes, or the Exec is unparseable —
        // the very stale/broken-launch class this repoint exists to fix.
        let existing = "[Desktop Entry]\nExec=/old/app %u\n";
        let out =
            desktop_entry_repointed(Path::new(r#"/home/a"b/$x/`c/d\e/UT4.AppImage"#), existing)
                .expect("changed");
        let exec = out
            .lines()
            .find(|l| l.starts_with("Exec="))
            .expect("has Exec");
        assert_eq!(
            exec, r#"Exec="/home/a\"b/\$x/\`c/d\\e/UT4.AppImage" %u"#,
            "each reserved char gets exactly one backslash; got: {exec}"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn desktop_repoint_without_field_code_stays_without_one() {
        let existing = "[Desktop Entry]\nExec=/old/app\n";
        let out =
            desktop_entry_repointed(Path::new("/new/app.AppImage"), existing).expect("changed");
        assert!(
            out.contains("Exec=\"/new/app.AppImage\"\n")
                || out.trim_end().ends_with("\"/new/app.AppImage\"")
        );
        assert!(!out.contains("%u"), "must not invent a field code");
    }

    #[cfg(not(windows))]
    #[test]
    fn desktop_repoint_ignores_exec_in_action_groups() {
        // A per-action Exec (in a [Desktop Action …] group) must NOT be rewritten
        // — only the main [Desktop Entry] Exec is the launcher path.
        let existing = "[Desktop Entry]\n\
Exec=/old/app.AppImage %u\n\
Actions=New;\n\
\n\
[Desktop Action New]\n\
Name=New Window\n\
Exec=/old/app.AppImage --new\n";
        let out =
            desktop_entry_repointed(Path::new("/new/app.AppImage"), existing).expect("changed");
        assert!(
            out.contains("Exec=\"/new/app.AppImage\" %u"),
            "main Exec repointed"
        );
        assert!(
            out.contains("Exec=/old/app.AppImage --new"),
            "action Exec left untouched"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn desktop_repoint_none_when_no_exec_line() {
        let existing = "[Desktop Entry]\nName=Launcher\nIcon=x\n";
        assert_eq!(
            desktop_entry_repointed(Path::new("/new/app.AppImage"), existing),
            None
        );
    }
}
