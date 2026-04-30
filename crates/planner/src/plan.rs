//! [`UpdatePlan`], the action types it contains, and [`plan`] — the
//! single entry point that builds one.

use std::collections::HashSet;

use ncp_manifest::{Manifest, ManifestPak};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::error::{PlanError, Result};
use crate::state::{LocalInstall, LocalPak};

/// Categorised set of actions to bring a [`LocalInstall`] into
/// agreement with one channel of a [`Manifest`].
///
/// All actions are described declaratively; the I/O layer is free to
/// confirm them with the user, then execute downloads in parallel and
/// installs in load-order. The [`Vec`]s are sorted by `id` for
/// deterministic display.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePlan {
    /// Paks the launcher must fetch from the network and install.
    pub to_download: Vec<DownloadAction>,
    /// Paks present locally that should be deleted.
    pub to_remove: Vec<RemoveAction>,
    /// Paks already at the desired version and hash.
    pub to_keep: Vec<KeepAction>,
}

impl UpdatePlan {
    /// `true` when no downloads and no removes are needed — the local
    /// install already matches the manifest for this channel.
    #[must_use]
    pub fn is_no_op(&self) -> bool {
        self.to_download.is_empty() && self.to_remove.is_empty()
    }

    /// Total bytes that will need to be fetched, summed across
    /// `to_download`. Useful for confirmation prompts and progress
    /// estimates.
    #[must_use]
    pub fn total_download_bytes(&self) -> u64 {
        self.to_download
            .iter()
            .map(|a| a.manifest_pak.size_bytes)
            .sum()
    }
}

/// One pak that must be downloaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadAction {
    /// Stable pak id (matches the manifest channel key).
    pub id: String,
    /// Why this download is needed.
    pub reason: DownloadReason,
    /// Manifest entry for the pak — provides URL, sha256, size,
    /// version, and target filename.
    pub manifest_pak: ManifestPak,
    /// The previously-installed state. `Some` for
    /// [`DownloadReason::HashMismatch`], `None` for
    /// [`DownloadReason::Missing`].
    pub previous: Option<LocalPak>,
}

/// Why a pak is being downloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadReason {
    /// The pak is not currently installed.
    Missing,
    /// The pak is installed but the bytes on disk do not match the
    /// manifest's `sha256`.
    HashMismatch,
}

/// One pak that must be removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveAction {
    /// Stable pak id.
    pub id: String,
    /// Why this pak is being removed.
    pub reason: RemoveReason,
    /// The local state being deleted — provides filename, version
    /// and hash for confirmation prompts.
    pub local_pak: LocalPak,
}

/// Why a pak is being removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoveReason {
    /// The pak is not present in the requested channel (or in any
    /// channel of the current manifest).
    NotInChannel,
    /// The pak is in the channel but the user opted out of it.
    OptedOut,
}

/// One pak being kept as-is.
///
/// Both `local_pak` and `manifest_pak` are included so the installer
/// can detect a filename change (same id, same hash, different name)
/// and rename the file rather than re-download it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeepAction {
    /// Stable pak id.
    pub id: String,
    /// What is currently on disk for this id.
    pub local_pak: LocalPak,
    /// What the manifest currently says about this id.
    pub manifest_pak: ManifestPak,
}

/// Compute an [`UpdatePlan`] for one channel of a verified manifest.
///
/// `opted_out` is the set of pak ids the user has explicitly opted
/// out of. Every id in that set must (1) exist in the channel and
/// (2) have `required: false`; otherwise the function returns an
/// error so the UI can surface the inconsistency.
///
/// # Errors
///
/// - [`PlanError::ChannelNotFound`] if `channel_name` is not in the
///   manifest.
/// - [`PlanError::OptOutOfUnknownPak`] if `opted_out` contains an id
///   that is not in the channel.
/// - [`PlanError::CannotOptOutOfRequired`] if `opted_out` contains a
///   pak whose `required` flag is `true`.
/// - [`PlanError::UnknownDependency`] if a kept pak's `depends_on`
///   references an id that is not in the channel at all.
/// - [`PlanError::DependencyVersionMismatch`] if a kept pak's
///   `depends_on` constraint is not satisfied by the channel's
///   version of the dep.
/// - [`PlanError::OptOutBreaksDependency`] if a kept pak depends on
///   a pak that the user has opted out of.
pub fn plan(
    manifest: &Manifest,
    channel_name: &str,
    local: &LocalInstall,
    opted_out: &HashSet<String>,
) -> Result<UpdatePlan> {
    // 1. Look up the requested channel.
    let channel = manifest
        .channels
        .get(channel_name)
        .ok_or_else(|| PlanError::ChannelNotFound(channel_name.to_string()))?;

    // 2. Validate the opt-out set: every id must exist and must not
    //    be required.
    for id in opted_out {
        let Some(pak) = channel.paks.get(id) else {
            return Err(PlanError::OptOutOfUnknownPak {
                id: id.clone(),
                channel: channel_name.to_string(),
            });
        };
        if pak.required {
            return Err(PlanError::CannotOptOutOfRequired { id: id.clone() });
        }
    }

    // 3. Validate dependencies. For each pak we intend to keep
    //    (everything in the channel minus the opt-outs), every
    //    `depends_on` entry must reference a kept pak with a
    //    satisfying version.
    for (id, pak) in &channel.paks {
        if opted_out.contains(id) {
            continue;
        }
        for (dep_id, req) in &pak.depends_on {
            let Some(dep) = channel.paks.get(dep_id) else {
                return Err(PlanError::UnknownDependency {
                    pak: id.clone(),
                    dep: dep_id.clone(),
                    channel: channel_name.to_string(),
                });
            };
            if opted_out.contains(dep_id) {
                return Err(PlanError::OptOutBreaksDependency {
                    pak: id.clone(),
                    dep: dep_id.clone(),
                });
            }
            if !req.matches(&dep.version) {
                return Err(PlanError::DependencyVersionMismatch {
                    pak: id.clone(),
                    dep: dep_id.clone(),
                    req: req.clone(),
                    got: dep.version.clone(),
                });
            }
        }
    }

    // 4. Categorise actions.
    let mut to_download: Vec<DownloadAction> = Vec::new();
    let mut to_keep: Vec<KeepAction> = Vec::new();
    let mut to_remove: Vec<RemoveAction> = Vec::new();

    for (id, manifest_pak) in &channel.paks {
        if opted_out.contains(id) {
            // Opted-out paks are handled by the local-pak loop below
            // (if present locally, they get removed; if absent, they
            // simply don't appear).
            continue;
        }
        match local.paks.get(id) {
            None => to_download.push(DownloadAction {
                id: id.clone(),
                reason: DownloadReason::Missing,
                manifest_pak: manifest_pak.clone(),
                previous: None,
            }),
            Some(local_pak) if local_pak.sha256 == manifest_pak.sha256 => {
                to_keep.push(KeepAction {
                    id: id.clone(),
                    local_pak: local_pak.clone(),
                    manifest_pak: manifest_pak.clone(),
                });
            }
            Some(local_pak) => to_download.push(DownloadAction {
                id: id.clone(),
                reason: DownloadReason::HashMismatch,
                manifest_pak: manifest_pak.clone(),
                previous: Some(local_pak.clone()),
            }),
        }
    }

    for (id, local_pak) in &local.paks {
        let in_channel = channel.paks.contains_key(id);
        let kept = in_channel && !opted_out.contains(id);
        if !kept {
            to_remove.push(RemoveAction {
                id: id.clone(),
                reason: if in_channel {
                    RemoveReason::OptedOut
                } else {
                    RemoveReason::NotInChannel
                },
                local_pak: local_pak.clone(),
            });
        }
    }

    // 5. Sort for deterministic output (HashMap iteration order is not
    //    stable, and tests + UIs both want consistent ordering).
    to_download.sort_by(|a, b| a.id.cmp(&b.id));
    to_keep.sort_by(|a, b| a.id.cmp(&b.id));
    to_remove.sort_by(|a, b| a.id.cmp(&b.id));

    debug!(
        download = to_download.len(),
        keep = to_keep.len(),
        remove = to_remove.len(),
        "computed update plan"
    );

    Ok(UpdatePlan {
        to_download,
        to_remove,
        to_keep,
    })
}
