//! Local install state: the planner's view of what is on disk.

use std::collections::HashMap;

use ncp_manifest::Sha256Digest;
use semver::Version;
use serde::{Deserialize, Serialize};

/// Snapshot of the paks currently installed on disk.
///
/// Produced by the launcher's I/O layer (e.g. by reading a state file
/// like `installed.json` next to the paks directory). The planner
/// consumes it as opaque data; no I/O happens here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalInstall {
    /// Currently-installed paks, keyed by stable id (matching the
    /// manifest's id keying).
    pub paks: HashMap<String, LocalPak>,
}

/// One installed pak's recorded state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalPak {
    /// Filename on disk inside the paks directory.
    pub pak_filename: String,

    /// Version recorded at install time. Sourced from whatever
    /// manifest the file was installed against.
    pub version: Version,

    /// SHA-256 digest of the bytes on disk at install time. The
    /// installer should re-hash on read to detect bit-rot or
    /// tampering before calling the planner — but mismatches against
    /// the manifest are caught by the planner regardless.
    pub sha256: Sha256Digest,
}

/// One installed NetcodePlus plugin's recorded state, per UT4 install root.
///
/// The launcher records this when *it* installs the plugin into a given root,
/// so a later check can decide up-to-date vs needs-update by comparing the
/// recorded ZIP digest to the manifest's. The on-disk `.uplugin` carries no
/// reliable version (its `VersionName` is the upstream value and is never
/// bumped), so this recorded state — not folder inspection — is the version
/// source of truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledPlugin {
    /// Plugin build number recorded at install time (the manifest's integer
    /// `PluginEntry::version`).
    pub version: u32,

    /// SHA-256 of the plugin **ZIP** the launcher installed (the manifest's
    /// `PluginEntry::sha256`). Compared against the manifest to decide whether
    /// an update is needed — a difference means new bytes are published.
    pub sha256: Sha256Digest,
}
