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
pub mod pak_install;
pub mod plugin_install;
pub mod shortcut;
pub mod state;
pub mod stray;
mod zip_safety;

pub use crate::dotnet::windowsdesktop_runtime_present;
pub use crate::elevate::{dir_writable, run_elevated, shell_launch, ElevateError};
pub use crate::error::{Result, StateError};
pub use crate::extract::{extract_zip, find_installer_exe, total_uncompressed_size, ExtractError};
pub use crate::install::{
    check_install, default_download_dir, default_mod_paks_dir, detect, detect_installs,
    netcodeplus_dir, netcodeplus_status, play_install_from_shortcut, plugins_dir,
    ulticross_installed, DetectSource, DetectedInstall, LaunchProfile, NetcodePlusStatus,
    UtInstall,
};
pub use crate::launch::{
    affinity_presets, launch, logical_cpus, parse_mask_hex, AffinityPreset, LaunchOptions, Priority,
};
pub use crate::pak_install::{
    install_pak, pak_file_stamp, pak_on_disk_digest, remove_pak, rename_pak,
};
pub use crate::plugin_install::{
    install_plugin_zip, install_plugin_zip_verified, plugin_content_hash, plugin_matches_zip,
    plugin_zip_content_hash, PluginInstallError,
};
pub use crate::shortcut::{
    create_desktop_shortcut, desktop_shortcut_is_stale, detect_outdated_launcher, is_stale_pending,
    schedule_delete_on_reboot, ShortcutError, LAUNCHER_SHORTCUT_NAME,
};
pub use crate::state::{LauncherState, OnboardingState, PakStamp, DEFAULT_CHANNEL};
pub use crate::stray::{remove_stray, scan_strays, StrayKind, StrayPlugin, StrayRemoveError};
