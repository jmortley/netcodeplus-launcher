//! Error type for manifest loading and verification.

use chrono::{DateTime, Utc};
use semver::Version;
use thiserror::Error;

/// Result alias used by all fallible operations in this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Failure modes for [`crate::Manifest::load_and_verify`].
///
/// Variants are ordered by the verification pipeline stage they
/// surface from: signature parsing → signature verification → JSON
/// parsing → schema/freshness/launcher invariants.
#[derive(Debug, Error)]
pub enum Error {
    /// The signature blob could not be parsed at all (not in minisign
    /// `.minisig` format, base64 invalid, missing trusted-comment
    /// line, etc.).
    ///
    /// Distinct from [`Self::SignatureInvalid`]: this means the
    /// `.minisig` file itself is corrupt, not that it failed to verify.
    #[error("signature file is malformed: {0}")]
    SignatureMalformed(String),

    /// The signature parses cleanly but does not verify against the
    /// supplied public key. Either the manifest bytes were tampered
    /// with after signing, or the signature was made with a different
    /// key than the launcher trusts.
    #[error("signature does not verify against the configured public key")]
    SignatureInvalid,

    /// The manifest bytes verified, but they could not be parsed as a
    /// JSON manifest matching the schema.
    #[error("manifest JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    /// The manifest's `schema_version` is not one this crate
    /// understands.
    #[error("unsupported schema_version {got} (this crate supports {supported})")]
    SchemaVersionUnsupported {
        /// Schema version the manifest claimed.
        got: u32,
        /// Schema version this crate supports.
        supported: u32,
    },

    /// The manifest's `expires_at` is at or before `now`, so the
    /// manifest must not be acted upon even though it verifies.
    #[error("manifest expired at {expires_at} (now: {now})")]
    Expired {
        /// Expiry timestamp from the manifest.
        expires_at: DateTime<Utc>,
        /// Current time as reported by the caller.
        now: DateTime<Utc>,
    },

    /// The launcher is older than the manifest's
    /// `min_launcher_version`. The launcher must refuse to apply the
    /// manifest and prompt the user to upgrade.
    #[error("launcher version {current} is older than required {required}")]
    LauncherTooOld {
        /// Minimum launcher version the manifest requires.
        required: Version,
        /// Launcher version currently running.
        current: Version,
    },

    /// The manifest's `sequence` is lower than the highest the launcher
    /// has already accepted: an attempt to replay or downgrade to an
    /// older manifest, rejected even though its signature and
    /// `expires_at` are still valid.
    #[error("manifest sequence {got} is older than the last accepted sequence {minimum}")]
    SequenceTooOld {
        /// Sequence number carried by the manifest under test.
        got: u64,
        /// Highest sequence the launcher has previously accepted (the
        /// replay floor).
        minimum: u64,
    },

    /// A pak's `pak_filename` is not a single, flat filename. The installer
    /// joins this onto the UT4 paks/plugins directory to decide where the
    /// downloaded bytes land, so a value containing a path separator, a `..`
    /// parent reference, a drive/UNC prefix, or a reserved/control character
    /// could redirect the write outside the install tree (path traversal).
    /// Rejected even when the manifest is otherwise validly signed — a single
    /// mistaken or compromised signing run must not be able to write outside
    /// the game directory.
    #[error(
        "channel {channel:?} pak {pak_id:?}: unsafe pak_filename {filename:?} \
         (must be a flat filename with no path separators or '..')"
    )]
    UnsafePakFilename {
        /// Channel the offending pak belongs to.
        channel: String,
        /// Channel-local id of the offending pak.
        pak_id: String,
        /// The rejected filename.
        filename: String,
    },
}
