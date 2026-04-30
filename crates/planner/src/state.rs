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
