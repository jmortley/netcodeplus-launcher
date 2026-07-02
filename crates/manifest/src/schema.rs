//! Manifest schema types.
//!
//! All types here are pure data — no I/O, no clock access. The only
//! moving part is the [`Sha256Digest`] wrapper, which carries its own
//! hex encoding/decoding.

use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Utc};
use semver::{Version, VersionReq};
use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};
use thiserror::Error;

/// Schema version this crate understands.
///
/// A manifest carrying any other [`Manifest::schema_version`] is
/// rejected by [`crate::Manifest::load_and_verify`].
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Top-level signed manifest.
///
/// Always produced and consumed via
/// [`crate::Manifest::load_and_verify`]; constructing one in memory
/// (e.g. in tests) is also fine but skips signature checking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// On-wire schema version. Currently fixed at
    /// [`SUPPORTED_SCHEMA_VERSION`].
    pub schema_version: u32,

    /// Wall-clock time at which the publishing tool produced the
    /// manifest. Used for diagnostics; the launcher does not enforce
    /// `generated_at <= now`, since clock skew between publisher and
    /// client is normal.
    pub generated_at: DateTime<Utc>,

    /// Wall-clock time after which the manifest must not be acted
    /// upon, even if its signature is still cryptographically valid.
    /// This bounds the window during which an attacker who has
    /// captured a stale-but-signed manifest can replay it.
    pub expires_at: DateTime<Utc>,

    /// Monotonic publish counter: every newly published manifest carries
    /// a strictly higher value than the previous one. The launcher
    /// records the highest `sequence` it has ever accepted and rejects
    /// any manifest carrying a lower one. This closes the replay/downgrade
    /// window that `expires_at` alone leaves open — within the expiry
    /// window an active network attacker can otherwise serve an older,
    /// still-validly-signed manifest to force a downgrade.
    pub sequence: u64,

    /// Minimum launcher version required to interpret the manifest.
    /// A launcher older than this must refuse to apply the manifest
    /// and prompt the user to upgrade the launcher first.
    pub min_launcher_version: Version,

    /// Map from channel name (e.g. `"stable"`, `"testing"`) to channel
    /// contents. A launcher chooses one channel at runtime and ignores
    /// the others.
    pub channels: HashMap<String, Channel>,

    /// The latest launcher build, if the manifest advertises one. Top-level
    /// (one launcher for everyone), not per-channel. `None` (the default, for
    /// back-compat with manifests authored before launcher-update support)
    /// means "no launcher update info" and the launcher shows nothing.
    ///
    /// Drives self-update: the launcher compares its own version to
    /// [`LauncherEntry::version`] and, if older, surfaces an update banner. When
    /// the entry also carries [`LauncherEntry::sha256`] + [`LauncherEntry::
    /// size_bytes`], the launcher downloads the new exe itself, verifies it
    /// against that signed digest, and relaunches into it; otherwise it falls
    /// back to **notify-only** (a link to [`LauncherEntry::url`] the user fetches
    /// and runs themselves).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher: Option<LauncherEntry>,

    /// A full UT4 game installer for users who don't have the game yet.
    /// Top-level (one for everyone), `#[serde(default)]` → `None` for back-compat.
    /// Like a pak, the host is untrusted: integrity comes from
    /// [`GameInstaller::sha256`] in the signed manifest, verified after download.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_installer: Option<GameInstaller>,

    /// The UT4 **editor** distribution (a plain zipped tree, no installer exe —
    /// the launcher extracts it and the user runs `UE4Editor.exe`). Same trust
    /// model and entry shape as [`Self::game_installer`]: third-party host
    /// (UT4Ever), integrity pinned by the signed digest. `#[serde(default)]` →
    /// `None`, so manifests authored before editor support — and launchers built
    /// before it — interoperate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_installer: Option<GameInstaller>,

    /// The UT4-OpenAL audio module (HRTF positional audio, by Main.exe): a zip
    /// whose `Win64/` tree overlays `<root>/Engine/Binaries/Win64/` plus nested
    /// per-sample-rate OpenAL Soft config zips. Hosted on a third-party GitHub
    /// release; integrity comes from the signed digest, exactly like a pak.
    /// `#[serde(default)]` → `None` for back-compat in both directions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openal: Option<GameInstaller>,
}

/// The latest launcher build advertised by a [`Manifest`].
///
/// Carries the new version, where to get it, and — when present — the integrity
/// material to verify a self-downloaded replacement:
///
/// - With [`Self::sha256`] **and** [`Self::size_bytes`], the launcher downloads
///   [`Self::url`] itself, enforces the size, verifies the SHA-256 against this
///   (signed) digest, and relaunches into the verified exe. Because the digest
///   lives inside the YubiKey-signed manifest, the signature transitively
///   protects the binary: a swapped or tampered exe fails the check.
/// - Without them (older manifests), the launcher falls back to **notify-only**:
///   it just surfaces a banner linking to [`Self::url`] for the user to download
///   and run themselves — the same trust path as their first install.
///
/// Both integrity fields are optional and `#[serde(default)]` so manifests
/// authored before the hash-pin hardening still parse (→ notify-only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LauncherEntry {
    /// Latest launcher version. The launcher compares this to its own
    /// `CARGO_PKG_VERSION`; a strictly greater value means an update is
    /// available. Semver (the launcher itself is semver-versioned, unlike the
    /// integer-versioned plugin).
    pub version: Version,

    /// HTTPS URL to download the new launcher from. With [`Self::sha256`] set
    /// this should be the **direct exe asset** (the launcher streams + verifies
    /// it); without it, a human-facing release page the user opens in a browser.
    /// **Untrusted** when auto-updating — integrity comes from [`Self::sha256`].
    pub url: String,

    /// SHA-256 digest of the launcher exe bytes. When present (with
    /// [`Self::size_bytes`]) it enables verified self-update: a downloaded exe
    /// whose bytes do not produce this digest is rejected. `None` on older
    /// manifests → notify-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<Sha256Digest>,

    /// Declared size of the launcher exe in bytes; a download whose length
    /// differs is aborted before hashing. Pairs with [`Self::sha256`]; `None` on
    /// older manifests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

/// A full UT4 game installer advertised by a [`Manifest`], for users who don't
/// have the game yet. Top-level (one for everyone), not per-channel.
///
/// Distributed by a third party (UT4Ever) on a host the launcher does not
/// control, so — exactly like a pak — integrity comes from [`Self::sha256`] in
/// the signed manifest, never from TLS or the host's reputation. It is a large
/// (multi-GB) download: the launcher streams it with resume + progress, verifies
/// the full digest, and hands the verified file to the user (it never runs the
/// installer itself).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameInstaller {
    /// Display version (e.g. `"1.1.0"`). A free-form label — the third party's
    /// own versioning, not compared as semver.
    pub version: String,
    /// HTTPS URL to download from. **Untrusted** — integrity comes from
    /// [`Self::sha256`].
    pub url: String,
    /// SHA-256 digest of the installer file bytes; a download that does not
    /// produce this digest is rejected.
    pub sha256: Sha256Digest,
    /// Declared size in bytes; a download whose length differs is aborted.
    pub size_bytes: u64,
}

/// One update channel inside a [`Manifest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    /// The paks belonging to this channel, keyed by stable internal
    /// id (e.g. `"ncutplus"`, `"elimplus"`).
    ///
    /// The id is what [`ManifestPak::depends_on`] entries refer to and
    /// what the launcher uses to track a logical pak across filename
    /// changes.
    pub paks: HashMap<String, ManifestPak>,

    /// The NetcodePlus plugin build for this channel, if the channel ships
    /// one. `None` (the default, for back-compat with manifests authored
    /// before plugin support) means the channel offers no plugin update and
    /// the launcher leaves any installed plugin untouched.
    ///
    /// The plugin is a *folder* (`.uplugin` + `Binaries/`), distributed as a
    /// zip and extracted into `<root>/UnrealTournament/Plugins/NetcodePlus/`,
    /// so it is modelled separately from the single-file [`ManifestPak`]s —
    /// notably it carries an integer [`PluginEntry::version`] rather than a
    /// semver one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<PluginEntry>,
}

/// The NetcodePlus plugin build advertised by a [`Channel`].
///
/// Distributed as a zip of the plugin folder and verified against
/// [`Self::sha256`] before extraction — the download URL is **untrusted**,
/// integrity comes from the signed manifest. The update decision is
/// hash-based (install when the installed zip's digest differs from this
/// one), matching how paks are handled; [`Self::version`] is the human-facing
/// build number used for display and as a downgrade guard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginEntry {
    /// Monotonic plugin build number (the project's single-incrementing
    /// version, e.g. `324` — matching the compile-time `NETCODE_PLUGIN_VERSION`
    /// the plugin uses for compatibility gating). Not semver: the plugin has
    /// always been versioned as one increasing integer.
    pub version: u32,

    /// HTTPS URL the plugin zip can be downloaded from.
    ///
    /// **Untrusted** — integrity comes from [`Self::sha256`] inside the signed
    /// manifest, never from TLS or the host's reputation.
    pub url: String,

    /// SHA-256 digest of the plugin **zip** bytes. The launcher refuses to
    /// extract a download whose bytes do not produce this digest, and treats a
    /// mismatch between this and the recorded installed digest as "update
    /// available".
    pub sha256: Sha256Digest,

    /// Declared size of the zip in bytes. A download whose length differs
    /// should be aborted before hashing (cheap resource-exhaustion guard).
    pub size_bytes: u64,
}

/// A single pak entry within a [`Channel`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestPak {
    /// Semantic version of this pak release.
    pub version: Version,

    /// Filename written into the UT4 paks/plugins directory.
    pub pak_filename: String,

    /// HTTPS URL the pak bytes can be downloaded from.
    ///
    /// This URL is **untrusted** — integrity comes from
    /// [`Self::sha256`] inside the signed manifest, never from TLS or
    /// from the host's reputation.
    pub url: String,

    /// SHA-256 digest of the pak bytes.
    ///
    /// The launcher refuses to install a pak whose downloaded bytes
    /// do not produce this digest.
    pub sha256: Sha256Digest,

    /// Declared size in bytes.
    ///
    /// A download whose `Content-Length` (or observed body length)
    /// differs from this should be aborted before hashing — protects
    /// against trivial resource-exhaustion attempts that serve
    /// gigabytes for a pak meant to be megabytes.
    pub size_bytes: u64,

    /// Engine load order; higher numbers override lower at game
    /// runtime.
    pub load_order: u32,

    /// `true` ⇒ the launcher refuses to launch the game without this
    /// pak; `false` ⇒ the user can opt out.
    pub required: bool,

    /// Other paks (by stable id, within the same channel) this pak
    /// requires, with a [`semver::VersionReq`] constraint each.
    /// Empty when there are no inter-pak dependencies.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub depends_on: HashMap<String, VersionReq>,
}

/// Typed wrapper around a 32-byte SHA-256 digest.
///
/// Serialises as a 64-character lowercase hex string when emitted as
/// JSON, and refuses to deserialise from any string of the wrong
/// length or with non-hex characters. Round-tripping always
/// canonicalises to lowercase.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    /// Wrap a raw 32-byte digest.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the underlying bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Decode a 64-character hex string (upper or lower case) into a
    /// digest.
    ///
    /// # Errors
    ///
    /// - [`HexError::WrongLength`] if the input is not exactly 64
    ///   characters long.
    /// - [`HexError::InvalidHex`] if any character is not a valid hex
    ///   digit.
    pub fn from_hex(s: &str) -> Result<Self, HexError> {
        if s.len() != 64 {
            return Err(HexError::WrongLength { got: s.len() });
        }
        let mut out = [0u8; 32];
        hex::decode_to_slice(s, &mut out).map_err(|err| HexError::InvalidHex(err.to_string()))?;
        Ok(Self(out))
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Sha256Digest")
            .field(&self.to_string())
            .finish()
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct HexVisitor;
        impl Visitor<'_> for HexVisitor {
            type Value = Sha256Digest;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a 64-character hex SHA-256 digest")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Sha256Digest::from_hex(v).map_err(de::Error::custom)
            }
        }
        deserializer.deserialize_str(HexVisitor)
    }
}

/// Errors produced when parsing a hex SHA-256 string.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum HexError {
    /// The input was not exactly 64 characters long.
    #[error("expected 64 hex chars, got {got}")]
    WrongLength {
        /// The actual length of the offending input.
        got: usize,
    },
    /// One or more characters were not valid hex digits.
    #[error("invalid hex: {0}")]
    InvalidHex(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_hex_round_trip_lowercase() {
        let s = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let d = Sha256Digest::from_hex(s).unwrap();
        assert_eq!(d.to_string(), s);
    }

    #[test]
    fn from_hex_accepts_uppercase_and_canonicalises_to_lower() {
        let upper = "ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789";
        let lower = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        let from_upper = Sha256Digest::from_hex(upper).unwrap();
        let from_lower = Sha256Digest::from_hex(lower).unwrap();
        assert_eq!(from_upper, from_lower);
        assert_eq!(from_upper.to_string(), lower);
    }

    #[test]
    fn from_hex_rejects_short_input() {
        assert_eq!(
            Sha256Digest::from_hex("abcd").unwrap_err(),
            HexError::WrongLength { got: 4 },
        );
    }

    #[test]
    fn from_hex_rejects_long_input() {
        let too_long = "a".repeat(65);
        assert_eq!(
            Sha256Digest::from_hex(&too_long).unwrap_err(),
            HexError::WrongLength { got: 65 },
        );
    }

    #[test]
    fn from_hex_rejects_non_hex_chars() {
        let bad = "z".repeat(64);
        assert!(matches!(
            Sha256Digest::from_hex(&bad).unwrap_err(),
            HexError::InvalidHex(_)
        ));
    }

    #[test]
    fn serde_round_trip() {
        let d = Sha256Digest::from_bytes([42u8; 32]);
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(
            json,
            "\"2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a\""
        );
        let parsed: Sha256Digest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, d);
    }

    #[test]
    fn deserialize_rejects_wrong_length() {
        let json = "\"abcd\"";
        let err = serde_json::from_str::<Sha256Digest>(json).unwrap_err();
        assert!(err.to_string().contains("64"), "got: {err}");
    }

    #[test]
    fn deserialize_rejects_non_hex() {
        let json = format!("\"{}\"", "z".repeat(64));
        let err = serde_json::from_str::<Sha256Digest>(&json).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("hex"), "got: {err}");
    }

    #[test]
    fn debug_includes_hex() {
        let d = Sha256Digest::from_bytes([0u8; 32]);
        let s = format!("{d:?}");
        assert!(s.contains("00000000"), "got: {s}");
    }

    #[test]
    fn channel_without_plugin_field_parses() {
        // Back-compat: a channel authored before plugin support (no `plugin`
        // key) must deserialise with `plugin: None`.
        let json = r#"{ "paks": {} }"#;
        let ch: Channel = serde_json::from_str(json).unwrap();
        assert!(ch.plugin.is_none());
    }

    #[test]
    fn channel_with_plugin_round_trips_with_integer_version() {
        let json = r#"{
            "paks": {},
            "plugin": {
                "version": 324,
                "url": "https://example.invalid/NetcodePlus-324.zip",
                "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "size_bytes": 1048576
            }
        }"#;
        let ch: Channel = serde_json::from_str(json).unwrap();
        let plugin = ch.plugin.as_ref().expect("plugin entry present");
        assert_eq!(plugin.version, 324);
        assert_eq!(plugin.size_bytes, 1_048_576);

        // Round-trips back to JSON and re-parses identically.
        let reparsed: Channel = serde_json::from_str(&serde_json::to_string(&ch).unwrap()).unwrap();
        assert_eq!(reparsed, ch);
    }

    #[test]
    fn manifest_without_launcher_field_parses() {
        // Back-compat: a manifest authored before launcher-update support (no
        // top-level `launcher` key) must deserialise with `launcher: None`.
        let json = r#"{
            "schema_version": 1,
            "generated_at": "2026-05-31T00:00:00Z",
            "expires_at": "2027-05-31T00:00:00Z",
            "sequence": 1,
            "min_launcher_version": "0.1.0",
            "channels": {}
        }"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        assert!(m.launcher.is_none());
    }

    #[test]
    fn manifest_with_launcher_round_trips() {
        let json = r#"{
            "schema_version": 1,
            "generated_at": "2026-05-31T00:00:00Z",
            "expires_at": "2027-05-31T00:00:00Z",
            "sequence": 2,
            "min_launcher_version": "0.1.0",
            "channels": {},
            "launcher": {
                "version": "0.2.0",
                "url": "https://github.com/jmortley/netcodeplus-launcher/releases/tag/v0.2.0"
            }
        }"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        let launcher = m.launcher.as_ref().expect("launcher entry present");
        assert_eq!(launcher.version, semver::Version::new(0, 2, 0));
        // Back-compat: a launcher entry without sha256/size_bytes parses with
        // both integrity fields absent → the launcher stays notify-only.
        assert!(launcher.sha256.is_none());
        assert!(launcher.size_bytes.is_none());
        let reparsed: Manifest = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(reparsed, m);
    }

    #[test]
    fn launcher_with_sha256_and_size_round_trips() {
        // The hash-pinned form: a launcher entry carrying the integrity material
        // that enables verified self-update. Must parse and round-trip.
        let json = r#"{
            "schema_version": 1,
            "generated_at": "2026-05-31T00:00:00Z",
            "expires_at": "2027-05-31T00:00:00Z",
            "sequence": 3,
            "min_launcher_version": "0.1.0",
            "channels": {},
            "launcher": {
                "version": "1.4.1",
                "url": "https://example.invalid/UT4-Community-Launcher-1.4.1.exe",
                "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "size_bytes": 12345678
            }
        }"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        let launcher = m.launcher.as_ref().expect("launcher entry present");
        assert_eq!(launcher.version, semver::Version::new(1, 4, 1));
        assert_eq!(launcher.size_bytes, Some(12_345_678));
        assert_eq!(
            launcher.sha256.expect("digest present").to_string(),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        let reparsed: Manifest = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(reparsed, m);
    }

    #[test]
    fn manifest_without_editor_or_openal_parses() {
        // Back-compat: every manifest published before editor/OpenAL support
        // (no `editor_installer` / `openal` keys) must deserialise with both
        // `None` — and, symmetrically, 1.5.x launchers ignore the new keys
        // (serde's default tolerates unknown fields; nothing here derives
        // `deny_unknown_fields`).
        let json = r#"{
            "schema_version": 1,
            "generated_at": "2026-07-02T00:00:00Z",
            "expires_at": "2027-07-02T00:00:00Z",
            "sequence": 35,
            "min_launcher_version": "0.1.0",
            "channels": {}
        }"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        assert!(m.editor_installer.is_none());
        assert!(m.openal.is_none());
    }

    #[test]
    fn manifest_with_editor_and_openal_round_trips() {
        let json = r#"{
            "schema_version": 1,
            "generated_at": "2026-07-02T00:00:00Z",
            "expires_at": "2027-07-02T00:00:00Z",
            "sequence": 36,
            "min_launcher_version": "0.1.0",
            "channels": {},
            "editor_installer": {
                "version": "2023-03-06",
                "url": "https://example.invalid/UnrealTournamentEditor.zip",
                "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "size_bytes": 33313033101
            },
            "openal": {
                "version": "2023-10-21",
                "url": "https://example.invalid/UT4-OpenAL.zip",
                "sha256": "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
                "size_bytes": 16878227
            }
        }"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        let editor = m.editor_installer.as_ref().expect("editor entry present");
        assert_eq!(editor.version, "2023-03-06");
        assert_eq!(editor.size_bytes, 33_313_033_101);
        let openal = m.openal.as_ref().expect("openal entry present");
        assert_eq!(openal.version, "2023-10-21");
        let reparsed: Manifest = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(reparsed, m);
    }

    #[test]
    fn plugin_rejects_non_integer_version() {
        // The plugin version is a single incrementing integer, not semver — a
        // "2.0"-style string must be rejected.
        let json = r#"{
            "paks": {},
            "plugin": {
                "version": "2.0",
                "url": "https://example.invalid/x.zip",
                "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "size_bytes": 1
            }
        }"#;
        assert!(serde_json::from_str::<Channel>(json).is_err());
    }
}
