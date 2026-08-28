//! Integration test for [`ncp_host::repoint_launcher_shortcut_if_present`] on
//! Linux — the full I/O path (reads `APPIMAGE` + `XDG_DATA_HOME`, rewrites the
//! `.desktop` in place). Reproduces the real field bug: the app-grid / dock
//! entry kept an old `Exec=` after the AppImage was moved to a new path.
//!
//! `APPIMAGE`/`XDG_DATA_HOME` are process-global, so all scenarios run inside
//! ONE `#[test]`; this is its own test binary, so nothing else shares the env.

#![cfg(target_os = "linux")]

use ncp_host::{repoint_launcher_shortcut_if_present, ShortcutRepoint};
use std::path::Path;
use tempfile::TempDir;

const STALE: &str = "[Desktop Entry]\n\
Type=Application\n\
Name=UT4 Community Launcher\n\
Comment=NetcodePlus update launcher and game runner.\n\
Exec=/home/jeremy/Applications/UT4-Community-Launcher.AppImage %u\n\
Icon=netcodeplus-launcher\n\
StartupWMClass=netcodeplus-launcher\n\
Terminal=false\n\
Categories=Game;\n\
MimeType=x-scheme-handler/ncp;\n";

const CURRENT: &str = "/home/jeremy/Downloads/UT4-Community-Launcher-x86_64.AppImage";

/// Write `contents` to `<data_home>/applications/netcodeplus-launcher.desktop`,
/// pointing `XDG_DATA_HOME` at a fresh tempdir, and return (tempdir, entry path).
fn seed_entry(contents: &str) -> (TempDir, std::path::PathBuf) {
    let d = TempDir::new().expect("tempdir");
    std::env::set_var("XDG_DATA_HOME", d.path());
    let apps = d.path().join("applications");
    std::fs::create_dir_all(&apps).expect("mkdir applications");
    let entry = apps.join("netcodeplus-launcher.desktop");
    std::fs::write(&entry, contents).expect("write entry");
    (d, entry)
}

#[test]
fn repoint_end_to_end() {
    // Stale entry + running as an AppImage -> rewritten in place.
    let (_d, entry) = seed_entry(STALE);
    std::env::set_var("APPIMAGE", CURRENT);
    // The passed path is deliberately ignored (would be the FUSE mount live).
    let r = repoint_launcher_shortcut_if_present(Path::new("/tmp/.mount_ignored/AppRun"));
    assert_eq!(r, ShortcutRepoint::Repointed, "stale entry should repoint");
    let after = std::fs::read_to_string(&entry).unwrap();
    assert!(
        after.contains(&format!("Exec=\"{CURRENT}\" %u")),
        "Exec now points at the current AppImage: {after}"
    );
    assert!(!after.contains("/Applications/"), "stale path gone");
    assert!(
        after.contains("Icon=netcodeplus-launcher"),
        "other keys kept"
    );
    assert!(
        after.contains("MimeType=x-scheme-handler/ncp;"),
        "handler kept"
    );

    // Running again is idempotent — already correct, no byte change.
    let before = std::fs::read_to_string(&entry).unwrap();
    assert_eq!(
        repoint_launcher_shortcut_if_present(Path::new("/ignored")),
        ShortcutRepoint::Repointed
    );
    assert_eq!(
        std::fs::read_to_string(&entry).unwrap(),
        before,
        "second run must not rewrite an already-correct entry"
    );

    // No managed entry present -> NoShortcut (never create one uninvited).
    let d2 = TempDir::new().unwrap();
    std::env::set_var("XDG_DATA_HOME", d2.path());
    assert_eq!(
        repoint_launcher_shortcut_if_present(Path::new("/ignored")),
        ShortcutRepoint::NoShortcut
    );

    // Not an AppImage (APPIMAGE unset, e.g. a .deb/dev run) -> NoShortcut even
    // when an entry exists, so a packaged .desktop is never rewritten.
    let (_d3, _entry3) = seed_entry(STALE);
    std::env::remove_var("APPIMAGE");
    assert_eq!(
        repoint_launcher_shortcut_if_present(Path::new("/ignored")),
        ShortcutRepoint::NoShortcut
    );
}
