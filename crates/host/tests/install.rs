//! Integration tests for [`ncp_host::install`].
//!
//! [`ncp_host::install::detect`] itself depends on real env vars and
//! filesystem state, so these tests focus on
//! [`ncp_host::install::check_install`] against synthetic UT4 layouts
//! built in [`tempfile::TempDir`]s.

use std::fs;
use std::path::Path;

use ncp_host::{check_install, UtInstall};
use tempfile::TempDir;

fn build_fake_install(root: &Path) {
    let win64 = root.join("UnrealTournament").join("Binaries").join("Win64");
    let paks = root.join("UnrealTournament").join("Content").join("Paks");
    fs::create_dir_all(&win64).unwrap();
    fs::create_dir_all(&paks).unwrap();
    fs::write(win64.join("UnrealTournament.exe"), b"fake exe").unwrap();
}

#[test]
fn check_install_finds_a_complete_layout() {
    let tmp = TempDir::new().unwrap();
    build_fake_install(tmp.path());

    let install: UtInstall = check_install(tmp.path()).expect("layout matches");
    assert_eq!(install.root, tmp.path());
    assert!(install.executable.ends_with("UnrealTournament.exe"));
    assert!(
        install
            .executable
            .to_string_lossy()
            .contains("UnrealTournament/Binaries/Win64")
            || install
                .executable
                .to_string_lossy()
                .contains(r"UnrealTournament\Binaries\Win64")
    );
    assert!(install.paks_dir.ends_with("Paks"));
    assert!(install.executable.is_file());
    assert!(install.paks_dir.is_dir());
}

#[test]
fn check_install_returns_none_when_executable_is_missing() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(
        tmp.path()
            .join("UnrealTournament")
            .join("Content")
            .join("Paks"),
    )
    .unwrap();
    // Win64 dir + exe deliberately not created
    assert!(check_install(tmp.path()).is_none());
}

#[test]
fn check_install_returns_none_when_paks_dir_is_missing() {
    let tmp = TempDir::new().unwrap();
    let win64 = tmp
        .path()
        .join("UnrealTournament")
        .join("Binaries")
        .join("Win64");
    fs::create_dir_all(&win64).unwrap();
    fs::write(win64.join("UnrealTournament.exe"), b"fake").unwrap();
    // paks dir deliberately not created
    assert!(check_install(tmp.path()).is_none());
}

#[test]
fn check_install_returns_none_when_executable_is_a_directory() {
    // Defensive: an attacker who can write to the install dir might
    // try to swap a file for a directory. is_file() rejects this.
    let tmp = TempDir::new().unwrap();
    let win64 = tmp
        .path()
        .join("UnrealTournament")
        .join("Binaries")
        .join("Win64");
    fs::create_dir_all(&win64).unwrap();
    fs::create_dir(win64.join("UnrealTournament.exe")).unwrap();
    fs::create_dir_all(
        tmp.path()
            .join("UnrealTournament")
            .join("Content")
            .join("Paks"),
    )
    .unwrap();
    assert!(check_install(tmp.path()).is_none());
}

#[test]
fn check_install_returns_none_for_empty_dir() {
    let tmp = TempDir::new().unwrap();
    assert!(check_install(tmp.path()).is_none());
}

#[test]
fn check_install_returns_none_for_nonexistent_path() {
    let path = Path::new(r"C:\This\Path\Definitely\Does\Not\Exist\Anywhere");
    assert!(check_install(path).is_none());
}

#[test]
fn detect_does_not_panic() {
    // We can't assert what `detect()` returns because it depends on
    // the host machine, but it must always return cleanly.
    let _ = ncp_host::detect();
}
