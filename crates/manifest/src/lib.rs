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
//!    minimum-launcher, replay-protection (`sequence >= min_sequence`),
//!    and pak-filename-safety (no path traversal) invariants.

mod error;
mod schema;

use chrono::{DateTime, Utc};
use minisign_verify::{PublicKey, Signature};
use semver::Version;
use tracing::debug;

pub use crate::error::{Error, Result};
pub use crate::schema::{
    Channel, GameInstaller, HexError, LauncherEntry, Manifest, ManifestPak, PluginEntry,
    Sha256Digest, SUPPORTED_SCHEMA_VERSION,
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
    /// - [`Error::UnsafePakFilename`] if any pak's `pak_filename` is not a
    ///   single flat filename safe to write into the install directory
    ///   (path-traversal guard).
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

        // Path-traversal guard: the installer joins each `pak_filename` onto
        // the paks/plugins directory to place the downloaded bytes, so a name
        // containing a separator or `..` could escape the install tree. Enforce
        // this behind the signature — a mistaken or compromised signing run
        // must not be able to direct a write outside the game directory.
        for (channel_name, channel) in &manifest.channels {
            for (pak_id, pak) in &channel.paks {
                if !is_safe_pak_filename(&pak.pak_filename) {
                    return Err(Error::UnsafePakFilename {
                        channel: channel_name.clone(),
                        pak_id: pak_id.clone(),
                        filename: pak.pak_filename.clone(),
                    });
                }
            }
        }

        Ok(manifest)
    }
}

/// Returns `true` only when `name` is a single, flat filename safe to join
/// onto the install directory: no path separators, no `.`/`..` traversal, no
/// drive/UNC prefix, and no reserved or control characters.
///
/// Checked with explicit character rules rather than [`std::path`] so the
/// verdict is identical regardless of the OS the launcher is built or run on —
/// a Linux build does not treat `\` or `:` as path-significant, but the Windows
/// client that consumes this manifest does, so `..\\evil` must be rejected on
/// both.
fn is_safe_pak_filename(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    // Separators (`/` `\`), the Windows drive/ADS colon, and the reserved
    // wildcard/redirection set. `is_control` additionally covers NUL and other
    // control bytes.
    const FORBIDDEN: &[char] = &['/', '\\', ':', '<', '>', '"', '|', '?', '*'];
    !name
        .chars()
        .any(|c| FORBIDDEN.contains(&c) || c.is_control())
}

#[cfg(test)]
mod tests {
    use super::is_safe_pak_filename;

    #[test]
    fn accepts_plain_filenames() {
        for ok in [
            "UTPlus-WindowsNoEditor.pak",
            "ncutplus.pak",
            "NetcodePlus_1.2.3.zip",
            "a.b.c.pak",
            "..leadingdots.pak",
        ] {
            assert!(is_safe_pak_filename(ok), "should accept {ok:?}");
        }
    }

    #[test]
    fn rejects_traversal_separators_and_reserved() {
        for bad in [
            "",
            ".",
            "..",
            "../evil.pak",
            "..\\evil.pak",
            "a/b.pak",
            "a\\b.pak",
            "/etc/cron.d/evil",
            "C:\\Windows\\System32\\evil.dll",
            "\\\\unc\\share\\x.pak",
            "evil.pak\0.png",
            "name:stream",
            "weird*.pak",
        ] {
            assert!(!is_safe_pak_filename(bad), "should reject {bad:?}");
        }
    }
}
