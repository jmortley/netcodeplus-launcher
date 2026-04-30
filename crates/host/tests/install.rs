//! Integration tests for [`ncp_host::install`].
//!
//! [`ncp_host::install::detect`] depends on real env vars and
//! filesystem state, so tests focus on
//! [`ncp_host::install::check_install`] against synthetic UT4 layouts
//! built in [`tempfile::TempDir`]s.

use std::fs;
use std::path::{Path, PathBuf};

use ncp_host::{check_install, UtInstall};
use tempfile::TempDir;

const FAKE_MOD_PAKS: &str = "C:/fake/Documents/UnrealTournament/Saved/Paks/DownloadedPaks";

fn fake_mod_paks_dir() -> PathBuf {
    PathBuf::from(FAKE_MOD_PAKS)
}

/// Lay down the minimum file/dir structure for a UE4-shipped UT4
/// install:
///
/// - `<root>/Engine/Binaries/Win64/UE4-Win64-Shipping.exe`
/// - `<root>/UnrealTournament/Content/Paks/`
fn build_fake_install(root: &Path) {
    let win64 = root.join("Engine").join("Binaries").join("Win64");
    let content_paks = root.join("UnrealTournament").join("Content").join("Paks");
    fs::create_dir_all(&win64).unwrap();
    fs::create_dir_all(&content_paks).unwrap();
    fs::write(win64.join("UE4-Win64-Shipping.exe"), b"fake exe").unwrap();
}

// ---------- positive shapes -----------------------------------------

#[test]
fn check_install_finds_a_complete_layout_at_install_root() {
    let tmp = TempDir::new().unwrap();
    build_fake_install(tmp.path());

    let install: UtInstall =
        check_install(tmp.path(), fake_mod_paks_dir()).expect("layout matches");
    assert_eq!(install.root, tmp.path());
    assert!(install.executable.ends_with("UE4-Win64-Shipping.exe"));
    assert!(install.executable.is_file());
    assert!(install.content_paks_dir.is_dir());
    assert_eq!(install.mod_paks_dir, fake_mod_paks_dir());
    assert_eq!(
        install.launch_args,
        vec![
            "UnrealTournament".to_string(),
            "-epicapp=UnrealTournamentDev".to_string(),
            "-epicenv=Prod".to_string(),
            "-EpicPortal".to_string(),
        ]
    );
}

#[test]
fn check_install_walks_up_when_user_picks_win64_subfolder() {
    // Simulates the common UX mistake: user picks
    // `<root>/Engine/Binaries/Win64/` (the folder containing the
    // exe) rather than the install root.
    let tmp = TempDir::new().unwrap();
    build_fake_install(tmp.path());

    let win64 = tmp.path().join("Engine").join("Binaries").join("Win64");
    let install = check_install(&win64, fake_mod_paks_dir()).expect("walk-up should resolve");
    assert_eq!(install.root, tmp.path());
}

#[test]
fn check_install_walks_up_when_user_picks_engine_subfolder() {
    let tmp = TempDir::new().unwrap();
    build_fake_install(tmp.path());

    let engine = tmp.path().join("Engine");
    let install = check_install(&engine, fake_mod_paks_dir()).expect("walk-up should resolve");
    assert_eq!(install.root, tmp.path());
}

// ---------- negative shapes -----------------------------------------

#[test]
fn check_install_returns_none_when_executable_missing() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(
        tmp.path()
            .join("UnrealTournament")
            .join("Content")
            .join("Paks"),
    )
    .unwrap();
    // Engine/Binaries/Win64/UE4-Win64-Shipping.exe deliberately not created
    assert!(check_install(tmp.path(), fake_mod_paks_dir()).is_none());
}

#[test]
fn check_install_returns_none_when_content_paks_dir_missing() {
    // The exe is there (so it could be UE4) but the UT4-specific
    // content paks dir isn't — so it isn't a UT4 install.
    let tmp = TempDir::new().unwrap();
    let win64 = tmp.path().join("Engine").join("Binaries").join("Win64");
    fs::create_dir_all(&win64).unwrap();
    fs::write(win64.join("UE4-Win64-Shipping.exe"), b"fake").unwrap();
    // UnrealTournament/Content/Paks/ deliberately not created
    assert!(check_install(tmp.path(), fake_mod_paks_dir()).is_none());
}

#[test]
fn check_install_returns_none_when_executable_is_a_directory() {
    // Defensive: an attacker (or a stray mkdir) shouldn't be able to
    // pass validation by replacing the exe with a directory.
    let tmp = TempDir::new().unwrap();
    let win64 = tmp.path().join("Engine").join("Binaries").join("Win64");
    fs::create_dir_all(&win64).unwrap();
    fs::create_dir(win64.join("UE4-Win64-Shipping.exe")).unwrap();
    fs::create_dir_all(
        tmp.path()
            .join("UnrealTournament")
            .join("Content")
            .join("Paks"),
    )
    .unwrap();
    assert!(check_install(tmp.path(), fake_mod_paks_dir()).is_none());
}

#[test]
fn check_install_returns_none_for_empty_dir() {
    let tmp = TempDir::new().unwrap();
    assert!(check_install(tmp.path(), fake_mod_paks_dir()).is_none());
}

#[test]
fn check_install_returns_none_for_nonexistent_path() {
    let path = Path::new(r"C:\This\Path\Definitely\Does\Not\Exist\Anywhere");
    assert!(check_install(path, fake_mod_paks_dir()).is_none());
}

// ---------- detect() smoke tests ------------------------------------

#[test]
fn detect_does_not_panic() {
    // Can't assert what `detect()` returns (machine-dependent), but
    // it must always return cleanly.
    let _ = ncp_host::detect();
}

#[test]
fn default_mod_paks_dir_returns_documents_subpath() {
    // On Windows this should always resolve. On other OSes it usually
    // resolves too, but we don't strictly require it (Linux/Proton
    // support is post-v1).
    if let Some(mod_paks) = ncp_host::default_mod_paks_dir() {
        let s = mod_paks.to_string_lossy();
        assert!(s.contains("UnrealTournament"), "got: {s}");
        assert!(s.contains("DownloadedPaks"), "got: {s}");
    }
}
