//! Update planner for the NetcodePlus launcher.
//!
//! Pure functions only: given a verified [`ncp_manifest::Manifest`],
//! a chosen channel, the current [`LocalInstall`] state, and a set
//! of opted-out optional pak ids, [`plan`] returns an [`UpdatePlan`]
//! describing which paks to download, remove, and keep.
//!
//! No I/O, no clock, no panics — this crate is fully unit-testable
//! without mocks.
//!
//! # Quick example
//!
//! ```no_run
//! use std::collections::HashSet;
//!
//! use ncp_manifest::Manifest;
//! use ncp_planner::{plan, LocalInstall};
//!
//! # fn run(manifest: &Manifest, local: &LocalInstall) -> ncp_planner::Result<()> {
//! let opted_out = HashSet::new();
//! let update = plan(manifest, "stable", local, &opted_out)?;
//! if update.is_no_op() {
//!     println!("already up to date");
//! } else {
//!     println!(
//!         "{} downloads ({} bytes), {} removes",
//!         update.to_download.len(),
//!         update.total_download_bytes(),
//!         update.to_remove.len(),
//!     );
//! }
//! # Ok(())
//! # }
//! ```

mod error;
mod plan;
mod plugin;
mod state;

pub use crate::error::{PlanError, Result};
pub use crate::plan::{
    plan, DownloadAction, DownloadReason, KeepAction, RemoveAction, RemoveReason, UpdatePlan,
};
pub use crate::plugin::{plan_plugin, PluginAction};
pub use crate::state::{InstalledPlugin, LocalInstall, LocalPak};
