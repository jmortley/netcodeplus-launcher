//! Tauri command handlers.
//!
//! Each `#[tauri::command]` is a thin wrapper exposing one function from
//! the workspace crates to the webview. Errors are stringified at the
//! boundary because the webview only handles Display-formatted messages.

use std::path::Path;

use ncp_host::{
    AffinityPreset, DetectSource, DetectedInstall, LaunchOptions, LaunchProfile, Priority,
};

/// Enumerate all UT4 *play* installs on this machine.
///
/// Desktop-shortcut driven — each shortcut's target is resolved to a
/// shipping client, so editor/source trees (only ever launched via a
/// `UE4Editor.exe` shortcut) are ignored — with a directory-probe
/// fallback when no shortcut resolves. Each install carries how it was
/// found, whether NetcodePlus is installed there, and one launch profile
/// per distinct shortcut variant. Empty array if nothing is found.
#[tauri::command]
pub fn detect_installs() -> Vec<DetectedInstall> {
    ncp_host::detect_installs()
}

/// Validate a user-picked folder (or one of its ancestors) as a UT4
/// install, returning it with `Manual` provenance, NetcodePlus status,
/// and a single `Default` launch profile. `null` if not a UT4 install.
#[tauri::command]
pub fn check_install(path: String) -> Option<DetectedInstall> {
    let mod_paks_dir = ncp_host::default_mod_paks_dir()?;
    let install = ncp_host::check_install(Path::new(&path), mod_paks_dir)?;
    let netcodeplus = ncp_host::netcodeplus_status(&install.root);
    let profiles = vec![LaunchProfile {
        label: "Default".to_string(),
        args: install.launch_args.clone(),
    }];
    Some(DetectedInstall {
        install,
        source: DetectSource::Manual,
        netcodeplus,
        profiles,
    })
}

/// CPU-affinity presets for this machine (all cores / exclude CPU 0 /
/// exclude CPU 0 & 1). The UI shows these plus a custom hex field.
#[tauri::command]
pub fn affinity_presets() -> Vec<AffinityPreset> {
    ncp_host::affinity_presets()
}

/// Launch the game with the chosen executable, launch args, priority, and
/// optional CPU affinity (hex mask string; empty/None = all cores).
#[tauri::command]
pub fn launch_game(
    executable: String,
    args: Vec<String>,
    priority: String,
    affinity_mask_hex: Option<String>,
) -> Result<(), String> {
    let affinity_mask = match affinity_mask_hex {
        Some(s) => {
            ncp_host::parse_mask_hex(&s).map_err(|e| format!("invalid affinity mask: {e}"))?
        }
        None => None,
    };
    let opts = LaunchOptions {
        priority: if priority.eq_ignore_ascii_case("high") {
            Priority::High
        } else {
            Priority::Normal
        },
        affinity_mask,
    };
    ncp_host::launch(Path::new(&executable), &args, &opts).map_err(|e| e.to_string())
}

/// Launcher version (from `Cargo.toml`). Surfaced in the UI for bug reports.
#[tauri::command]
pub fn launcher_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
