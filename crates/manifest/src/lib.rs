//! Signed manifest types and verification for the NetcodePlus launcher.
//!
//! This crate is deliberately I/O-free so it can be unit-tested
//! without mocks. Networking, the filesystem, and the system clock
//! are the caller's concern; this crate is handed bytes and asked to
//! either return a verified [`Manifest`] or a typed [`Error`].
//!
//! # Verification flow
//!
//! [`Manifest::load_and_verify`] is the only entry point and follows
//! a strict signature-first order:
//!
//! 1. Decode the minisign `.minisig` blob.
//! 2. Verify it against `public_key` over `json_bytes`. **No JSON
//!    parsing happens until this step succeeds**, so a malformed
//!    payload paired with a tampered signature never reaches
//!    `serde_json`.
//! 3. Parse the JSON.
//! 4. Enforce schema version, freshness (`expires_at > now`),
//!    minimum-launcher, and replay-protection (`sequence >=
//!    min_sequence`) invariants.

mod error;
mod schema;

use chrono::{DateTime, Utc};
use minisign_verify::{PublicKey, Signature};
use semver::Version;
use tracing::debug;

pub use crate::error::{Error, Result};
pub use crate::schema::{
    Channel, HexError, Manifest, ManifestPak, Sha256Digest, SUPPORTED_SCHEMA_VERSION,
};

impl Manifest {
    /// Verify a signed manifest and return the parsed value.
    ///
    /// `signature_text` is the full text of a minisign `.minisig`
    /// file (including the `untrusted comment` and `trusted comment`
    /// lines). Hashed-mode signatures are required; legacy signatures
    /// are rejected.
    ///
    /// # Errors
    ///
    /// - [`Error::SignatureMalformed`] if `signature_text` is not a
    ///   parseable minisign `.minisig` blob.
    /// - [`Error::SignatureInvalid`] if the signature parses but does
    ///   not verify against `public_key`.
    /// - [`Error::Json`] if the bytes verified but are not a valid
    ///   manifest JSON document.
    /// - [`Error::SchemaVersionUnsupported`] for unrecognised
    ///   `schema_version`.
    /// - [`Error::Expired`] if `expires_at <= now`.
    /// - [`Error::LauncherTooOld`] if a newer launcher is required.
    /// - [`Error::SequenceTooOld`] if `sequence < min_sequence`, i.e. an
    ///   older manifest replayed by an active attacker.
    ///
    /// `min_sequence` is the highest manifest `sequence` the launcher has
    /// previously accepted (persist it across runs); pass `0` on first
    /// run to accept any sequence.
    pub fn load_and_verify(
        json_bytes: &[u8],
        signature_text: &str,
        public_key: &PublicKey,
        now: DateTime<Utc>,
        current_launcher_version: &Version,
        min_sequence: u64,
    ) -> Result<Self> {
        // 1. Decode and verify the signature first. Any failure here
        //    short-circuits before serde_json sees a single byte.
        let signature = Signature::decode(signature_text)
            .map_err(|err| Error::SignatureMalformed(err.to_string()))?;
        public_key
            .verify(json_bytes, &signature, false)
            .map_err(|_| Error::SignatureInvalid)?;
        debug!(json_len = json_bytes.len(), "manifest signature verified");

        // 2. Parse the (now-trusted) JSON.
        let manifest: Manifest = serde_json::from_slice(json_bytes)?;

        // 3. Enforce invariants. Cheapest check first.
        if manifest.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(Error::SchemaVersionUnsupported {
                got: manifest.schema_version,
                supported: SUPPORTED_SCHEMA_VERSION,
            });
        }
        if manifest.expires_at <= now {
            return Err(Error::Expired {
                expires_at: manifest.expires_at,
                now,
            });
        }
        if manifest.min_launcher_version > *current_launcher_version {
            return Err(Error::LauncherTooOld {
                required: manifest.min_launcher_version.clone(),
                current: current_launcher_version.clone(),
            });
        }
        // Anti-replay / anti-downgrade: reject a manifest older than the
        // highest sequence we have already accepted, even though its
        // signature and `expires_at` still check out.
        if manifest.sequence < min_sequence {
            return Err(Error::SequenceTooOld {
                got: manifest.sequence,
                minimum: min_sequence,
            });
        }

        Ok(manifest)
    }
}
