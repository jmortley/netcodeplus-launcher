//! Tauri command handlers.
//!
//! Each `#[tauri::command]` is a thin wrapper that exposes a single
//! function from one of the workspace crates to the webview. Errors
//! are stringified at the boundary because the webview only handles
//! Display-formatted messages.

use std::path::Path;

use ncp_host::UtInstall;

/// Try to autodetect the UT4 install on this machine.
///
/// Returns `null` (in JS) if no candidate path matches; the UI is
/// then expected to drive a folder picker and call
/// [`check_install`].
#[tauri::command]
pub fn detect_install() -> Option<UtInstall> {
    ncp_host::detect()
}

/// Validate that `path` (or one of its ancestors — so picking the
/// `Engine/Binaries/Win64/` subfolder still works) is a UT4 install.
///
/// Returns `null` for any path that does not contain
/// `Engine/Binaries/Win64/UE4-Win64-Shipping.exe` plus
/// `UnrealTournament/Content/Paks/` somewhere up the tree.
#[tauri::command]
pub fn check_install(path: String) -> Option<UtInstall> {
    let mod_paks_dir = ncp_host::default_mod_paks_dir()?;
    ncp_host::check_install(Path::new(&path), mod_paks_dir)
}

/// Launcher version (from `Cargo.toml`).
///
/// Surfaced in the UI so users can include it in bug reports.
#[tauri::command]
pub fn launcher_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
