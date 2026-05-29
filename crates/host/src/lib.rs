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

mod error;
pub mod install;
pub mod launch;
pub mod state;

pub use crate::error::{Result, StateError};
pub use crate::install::{
    check_install, default_mod_paks_dir, detect, detect_installs, netcodeplus_dir,
    netcodeplus_status, play_install_from_shortcut, DetectSource, DetectedInstall, LaunchProfile,
    NetcodePlusStatus, UtInstall,
};
pub use crate::launch::{
    affinity_presets, launch, logical_cpus, parse_mask_hex, AffinityPreset, LaunchOptions, Priority,
};
pub use crate::state::{LauncherState, DEFAULT_CHANNEL};
