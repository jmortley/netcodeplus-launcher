//! Integration tests for [`ncp_host::state`].
//!
//! All tests use [`tempfile::TempDir`] so nothing touches the user's
//! real config dir.

use std::collections::{HashMap, HashSet};
use std::fs;

use ncp_host::{state, LauncherState, Priority, StateError};
use ncp_manifest::Sha256Digest;
use ncp_planner::{InstalledPlugin, LocalInstall, LocalPak};
use semver::Version;
use tempfile::TempDir;

// ---------- helpers --------------------------------------------------

fn sample_state() -> LauncherState {
    let mut paks = HashMap::new();
    paks.insert(
        "ncutplus".to_string(),
        LocalPak {
            pak_filename: "UTPlus-WindowsNoEditor.pak".to_string(),
            version: Version::parse("1.0.0").expect("valid"),
            sha256: Sha256Digest::from_bytes([1u8; 32]),
        },
    );
    paks.insert(
        "elimplus".to_string(),
        LocalPak {
            pak_filename: "ElimPlusMutator-WindowsNoEditor.pak".to_string(),
            version: Version::parse("0.5.0").expect("valid"),
            sha256: Sha256Digest::from_bytes([2u8; 32]),
        },
    );
    let mut opted_out = HashSet::new();
    opted_out.insert("legacy".to_string());

    let mut installed_plugins = HashMap::new();
    installed_plugins.insert(
        "C:/Games/UnrealTournament".to_string(),
        InstalledPlugin {
            version: 324,
            sha256: Sha256Digest::from_bytes([3u8; 32]),
        },
    );

    LauncherState {
        install_path: Some("C:/Games/UnrealTournament".into()),
        channel: "stable".to_string(),
        local_install: LocalInstall { paks },
        opted_out,
        launch_profile_label: Some("Unreal Tournament 4 UU".to_string()),
        launch_priority: Priority::High,
        affinity_mask_hex: Some("FFC".to_string()),
        launch_window_action: "close".to_string(),
        ut4stats_playerid: Some("76561190000000000".to_string()),
        ut4stats_playername: Some("phantaci".to_string()),
        launcher_token: Some("tok_example".to_string()),
        ut4_username: Some("phantaci".to_string()),
        ut4_display_name: Some("phantaci".to_string()),
        ut4_account_id: Some("64bf8c6d81004e88823d577abe157373".to_string()),
        installed_plugins,
        highest_manifest_sequence: 42,
    }
}

// ---------- read -----------------------------------------------------

#[test]
fn read_missing_file_returns_none() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("installed.json");
    let result = state::read(&path).unwrap();
    assert!(result.is_none());
}

#[test]
fn read_corrupt_json_returns_json_error() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("installed.json");
    fs::write(&path, b"{ this is not valid json").unwrap();
    let err = state::read(&path).unwrap_err();
    assert!(matches!(err, StateError::Json(_)), "got {err:?}");
}

#[test]
fn read_empty_file_returns_json_error() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("installed.json");
    fs::write(&path, b"").unwrap();
    let err = state::read(&path).unwrap_err();
    assert!(matches!(err, StateError::Json(_)), "got {err:?}");
}

#[test]
fn read_partial_json_uses_defaults_for_missing_fields() {
    // A minimal state file with only `install_path` should still
    // parse — channel falls back to DEFAULT_CHANNEL, local_install
    // and opted_out are empty.
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("installed.json");
    fs::write(&path, br#"{"install_path":"C:/Games/UT"}"#).unwrap();
    let state = state::read(&path).unwrap().expect("file exists");
    assert_eq!(
        state.install_path.as_deref(),
        Some(std::path::Path::new("C:/Games/UT"))
    );
    assert_eq!(state.channel, ncp_host::DEFAULT_CHANNEL);
    assert!(state.local_install.paks.is_empty());
    assert!(state.opted_out.is_empty());
}

// ---------- write ----------------------------------------------------

#[test]
fn write_then_read_round_trips() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("installed.json");
    let original = sample_state();
    state::write(&path, &original).unwrap();
    let read_back = state::read(&path).unwrap().unwrap();
    assert_eq!(read_back, original);
}

#[test]
fn write_creates_missing_parent_directories() {
    let tmp = TempDir::new().unwrap();
    let path = tmp
        .path()
        .join("nested")
        .join("config")
        .join("installed.json");
    state::write(&path, &sample_state()).unwrap();
    assert!(path.exists(), "state file should be created");
    let read_back = state::read(&path).unwrap().unwrap();
    assert_eq!(read_back, sample_state());
}

#[test]
fn write_overwrites_existing_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("installed.json");
    state::write(&path, &LauncherState::default()).unwrap();
    let updated = sample_state();
    state::write(&path, &updated).unwrap();
    let read_back = state::read(&path).unwrap().unwrap();
    assert_eq!(read_back, updated);
}

#[test]
fn write_does_not_leave_temp_files_behind() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("installed.json");
    state::write(&path, &sample_state()).unwrap();
    let entries: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "only the persisted file should remain, got: {entries:?}"
    );
    assert_eq!(entries[0], "installed.json");
}

#[test]
fn write_then_overwrite_does_not_leave_temp_files() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("installed.json");
    state::write(&path, &LauncherState::default()).unwrap();
    state::write(&path, &sample_state()).unwrap();
    state::write(&path, &LauncherState::default()).unwrap();
    let entries: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(entries.len(), 1, "got: {entries:?}");
}

#[test]
fn default_state_round_trips() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("installed.json");
    state::write(&path, &LauncherState::default()).unwrap();
    let read_back = state::read(&path).unwrap().unwrap();
    assert_eq!(read_back, LauncherState::default());
    assert_eq!(read_back.channel, ncp_host::DEFAULT_CHANNEL);
}
