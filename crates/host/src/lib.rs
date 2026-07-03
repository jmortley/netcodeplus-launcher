//! Host-system I/O for the NetcodePlus launcher.
//!
//! This is the first crate in the workspace that touches the
//! filesystem. It owns two narrow concerns:
//!
//! - [`state`]: read and write the persistent launcher state file
//!   (`installed.json`-style), with atomic write semantics.
//! - [`install`]: detect a local UT4 install and validate that a
//!   candidate root looks plausible.
//!
//! Network I/O lives elsewhere (HTTPS fetch, pak download —
//! later phases); UI lives in the Tauri shell. This crate is purely
//! a thin, well-tested boundary between the on-disk world and the
//! pure-logic crates [`ncp_manifest`](https://docs.rs/ncp-manifest)
//! and [`ncp_planner`](https://docs.rs/ncp-planner).

pub mod compat;
pub mod config;
pub mod disk;
pub mod dotnet;
pub mod elevate;
mod error;
pub mod extract;
mod fs_util;
pub mod install;
pub mod launch;
// Linux (Wine/Lutris) support — pure path/argv helpers. Compiled on every
// platform: the tests run on the Windows dev box + CI, and the Tauri commands
// `list_wine_runners` / `resolve_linux_launch` name `WineRunner` / `WineLaunch`
// in their (un-gated) signatures, so the types must resolve on Windows too.
pub mod linux;
pub mod openal_install;
pub mod pak_install;
pub mod plugin_install;
pub mod shortcut;
pub mod state;
pub mod stray;
pub mod swap;
mod zip_safety;

pub use crate::dotnet::windowsdesktop_runtime_present;
pub use crate::elevate::{dir_writable, run_elevated, shell_launch, ElevateError};
pub use crate::error::{Result, StateError};
pub use crate::extract::{
    extract_zip, find_editor_exe, find_installer_exe, total_uncompressed_size, ExtractError,
};
pub use crate::install::{
    check_install, default_download_dir, default_mod_paks_dir, detect, detect_installs,
    netcodeplus_dir, netcodeplus_status, play_install_from_shortcut, plugins_dir,
    ulticross_installed, DetectSource, DetectedInstall, LaunchProfile, NetcodePlusStatus,
    UtInstall,
};
pub use crate::launch::{
    affinity_presets, launch, logical_cpus, parse_mask_hex, AffinityPreset, LaunchOptions,
    LinuxLaunchSettings, Priority,
};
pub use crate::openal_install::{
    install_openal_binaries, install_openal_binaries_verified, install_openal_config,
    install_openal_zip, openal_binaries_file_count, OpenalConfigSummary, OpenalInstallError,
    OpenalInstallSummary,
};
pub use crate::pak_install::{
    install_pak, pak_file_stamp, pak_on_disk_digest, remove_pak, rename_pak,
};
pub use crate::plugin_install::{
    install_plugin_zip, install_plugin_zip_verified, plugin_content_hash, plugin_matches_zip,
    plugin_zip_content_hash, PluginInstallError,
};
pub use crate::shortcut::{
    create_desktop_shortcut, create_desktop_shortcut_with, desktop_shortcut_is_stale,
    detect_outdated_launcher, is_stale_pending, schedule_delete_on_reboot, ShortcutError,
    LAUNCHER_SHORTCUT_NAME,
};
pub use crate::state::{LauncherState, OnboardingState, PakStamp, DEFAULT_CHANNEL};
pub use crate::stray::{remove_stray, scan_strays, StrayKind, StrayPlugin, StrayRemoveError};
pub use crate::swap::{apply_binary_swap, swap_paths, SwapError};
