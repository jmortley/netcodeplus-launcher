//! Plugin update decision.
//!
//! Pure logic, mirroring [`crate::plan`] but for the single NetcodePlus
//! plugin a channel may advertise. Given the manifest's optional
//! [`ncp_manifest::PluginEntry`], whether the plugin folder is currently
//! present on disk, and the launcher's recorded [`InstalledPlugin`] state for
//! that install, decide what (if anything) to do.
//!
//! The decision is **hash-based**, matching how paks are handled: the manifest
//! ZIP's SHA-256 is the integrity + up-to-date gate. The integer version is a
//! display value and a downgrade guard. No I/O, no panics.

use serde::{Deserialize, Serialize};

use ncp_manifest::PluginEntry;

use crate::state::InstalledPlugin;

/// What should happen to the NetcodePlus plugin in one install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PluginAction {
    /// The channel offers no plugin (or the manifest predates plugin support).
    /// Leave any installed plugin untouched.
    None,
    /// No plugin is recorded as installed here (fresh install, or the folder is
    /// missing/malformed) — download + extract the manifest's plugin.
    Install {
        /// Build number being installed.
        version: u32,
    },
    /// A plugin is installed but its recorded ZIP digest differs from the
    /// manifest's — new bytes are published, so re-download + replace.
    Update {
        /// Currently-installed build number.
        from_version: u32,
        /// Build number being installed.
        to_version: u32,
    },
    /// The installed plugin's recorded digest already matches the manifest —
    /// nothing to do.
    UpToDate {
        /// The up-to-date build number.
        version: u32,
    },
    /// The manifest's plugin build is OLDER than what's installed (a publishing
    /// mistake or an attempted downgrade). Refuse — never install an older
    /// build over a newer one. The signature guarantees authenticity, not that
    /// a rollback is intended.
    DowngradeBlocked {
        /// Currently-installed (newer) build number.
        installed: u32,
        /// Manifest's (older) build number.
        offered: u32,
    },
}

/// Decide the plugin action for one install.
///
/// - `manifest_plugin`: the channel's advertised plugin, if any.
/// - `folder_present`: whether `Plugins/NetcodePlus/` is currently installed
///   and well-formed in this install (the launcher passes the result of its
///   `NetcodePlusStatus::Installed` check). A `false` here forces a (re)install
///   even if state claims a version, because the folder is gone or broken.
/// - `recorded`: the launcher's recorded install state for THIS root, if any.
///
/// The decision matrix:
/// - no manifest plugin → [`PluginAction::None`]
/// - folder missing/malformed → [`PluginAction::Install`] (regardless of record)
/// - recorded digest == manifest digest → [`PluginAction::UpToDate`]
/// - recorded version  >  manifest version → [`PluginAction::DowngradeBlocked`]
/// - otherwise (different digest, manifest newer-or-equal-version) →
///   [`PluginAction::Update`]
/// - present on disk but nothing recorded → [`PluginAction::Install`] (the
///   launcher can't vouch for unknown bytes it didn't place; reinstall to a
///   known-good, recorded state)
/// - `on_disk_hash`: the freshly-computed fingerprint of the load-bearing files
///   currently on disk ([`ncp_host::plugin_content_hash`]), or `None` if it
///   couldn't be computed. Lets the decision catch an install whose recorded ZIP
///   digest still matches the manifest but whose on-disk bytes have been
///   hand-swapped to a different build → that reads as [`PluginAction::Update`],
///   not [`PluginAction::UpToDate`].
#[must_use]
pub fn plan_plugin(
    manifest_plugin: Option<&PluginEntry>,
    folder_present: bool,
    recorded: Option<&InstalledPlugin>,
    on_disk_hash: Option<&str>,
) -> PluginAction {
    let Some(entry) = manifest_plugin else {
        return PluginAction::None;
    };

    // A missing/malformed folder always means install, no matter what state
    // claims — the bytes aren't actually there (or are broken).
    if !folder_present {
        return PluginAction::Install {
            version: entry.version,
        };
    }

    match recorded {
        // Present on disk but we have no record of installing it → treat as a
        // fresh install so we converge to known-good, recorded bytes.
        None => PluginAction::Install {
            version: entry.version,
        },
        // Recorded digest matches the manifest AND the on-disk content still
        // matches what we installed → genuinely up to date. A content mismatch
        // (a hand-swapped build) falls through to Update below.
        Some(rec) if rec.sha256 == entry.sha256 && content_intact(rec, on_disk_hash) => {
            PluginAction::UpToDate {
                version: entry.version,
            }
        }
        Some(rec) if rec.version > entry.version => PluginAction::DowngradeBlocked {
            installed: rec.version,
            offered: entry.version,
        },
        Some(rec) => PluginAction::Update {
            from_version: rec.version,
            to_version: entry.version,
        },
    }
}

/// Whether the on-disk plugin content still matches what the launcher recorded
/// at install. Decisive only when BOTH the recorded fingerprint and the freshly
/// computed on-disk fingerprint are known; a missing fingerprint (a record from
/// before the field existed, or an unreadable folder) can't prove drift, so we
/// trust the ZIP-digest record as before.
fn content_intact(rec: &InstalledPlugin, on_disk_hash: Option<&str>) -> bool {
    match (rec.content_hash.as_deref(), on_disk_hash) {
        (Some(recorded), Some(disk)) => recorded.eq_ignore_ascii_case(disk),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ncp_manifest::Sha256Digest;

    fn entry(version: u32, sha: u8) -> PluginEntry {
        PluginEntry {
            version,
            url: "https://example.invalid/p.zip".to_string(),
            sha256: Sha256Digest::from_bytes([sha; 32]),
            size_bytes: 100,
        }
    }

    fn recorded(version: u32, sha: u8) -> InstalledPlugin {
        InstalledPlugin {
            version,
            sha256: Sha256Digest::from_bytes([sha; 32]),
            content_hash: None,
        }
    }

    fn recorded_with_hash(version: u32, sha: u8, hash: &str) -> InstalledPlugin {
        InstalledPlugin {
            version,
            sha256: Sha256Digest::from_bytes([sha; 32]),
            content_hash: Some(hash.to_string()),
        }
    }

    #[test]
    fn no_manifest_plugin_is_none() {
        assert_eq!(plan_plugin(None, true, None, None), PluginAction::None);
        assert_eq!(
            plan_plugin(None, false, Some(&recorded(1, 1)), None),
            PluginAction::None
        );
    }

    #[test]
    fn missing_folder_forces_install_even_with_record() {
        // Folder gone but state claims v3 installed → still Install (bytes
        // aren't there).
        assert_eq!(
            plan_plugin(Some(&entry(3, 9)), false, Some(&recorded(3, 9)), None),
            PluginAction::Install { version: 3 }
        );
    }

    #[test]
    fn present_but_unrecorded_is_install() {
        assert_eq!(
            plan_plugin(Some(&entry(3, 9)), true, None, None),
            PluginAction::Install { version: 3 }
        );
    }

    #[test]
    fn matching_digest_is_up_to_date() {
        assert_eq!(
            plan_plugin(Some(&entry(3, 9)), true, Some(&recorded(3, 9)), None),
            PluginAction::UpToDate { version: 3 }
        );
    }

    #[test]
    fn different_digest_newer_version_is_update() {
        assert_eq!(
            plan_plugin(Some(&entry(4, 7)), true, Some(&recorded(3, 9)), None),
            PluginAction::Update {
                from_version: 3,
                to_version: 4
            }
        );
    }

    #[test]
    fn same_version_different_digest_is_update() {
        // Re-published same build number with different bytes (e.g. a rebuild):
        // digest differs, version not lower → Update, not blocked.
        assert_eq!(
            plan_plugin(Some(&entry(3, 8)), true, Some(&recorded(3, 9)), None),
            PluginAction::Update {
                from_version: 3,
                to_version: 3
            }
        );
    }

    #[test]
    fn older_manifest_version_is_downgrade_blocked() {
        assert_eq!(
            plan_plugin(Some(&entry(2, 7)), true, Some(&recorded(5, 9)), None),
            PluginAction::DowngradeBlocked {
                installed: 5,
                offered: 2
            }
        );
    }

    #[test]
    fn matching_digest_and_content_is_up_to_date() {
        // Recorded fingerprint matches what's on disk now → genuinely current.
        assert_eq!(
            plan_plugin(
                Some(&entry(3, 9)),
                true,
                Some(&recorded_with_hash(3, 9, "abc")),
                Some("abc"),
            ),
            PluginAction::UpToDate { version: 3 }
        );
    }

    #[test]
    fn content_drift_is_update_even_when_digest_matches() {
        // Same ZIP digest recorded, but the on-disk bytes were hand-swapped to a
        // different build → not up to date; prompt an update back to the manifest.
        assert_eq!(
            plan_plugin(
                Some(&entry(3, 9)),
                true,
                Some(&recorded_with_hash(3, 9, "expected")),
                Some("swapped"),
            ),
            PluginAction::Update {
                from_version: 3,
                to_version: 3
            }
        );
    }

    #[test]
    fn legacy_record_without_fingerprint_trusts_digest() {
        // A record from before content_hash existed (None) can't prove drift →
        // falls back to the digest comparison (up to date).
        assert_eq!(
            plan_plugin(
                Some(&entry(3, 9)),
                true,
                Some(&recorded(3, 9)),
                Some("anything")
            ),
            PluginAction::UpToDate { version: 3 }
        );
    }
}
