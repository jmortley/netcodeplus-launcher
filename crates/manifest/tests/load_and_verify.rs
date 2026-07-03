//! Integration tests for [`ncp_manifest::Manifest::load_and_verify`].
//!
//! These tests own the only signing path in the crate: [`minisign`]
//! lives in `[dev-dependencies]` so the production binary only links
//! against `minisign-verify`.

use std::collections::HashMap;
use std::io::Cursor;

use chrono::{DateTime, Duration, Utc};
use minisign::{sign, KeyPair, PublicKeyBox, SignatureBox};
use minisign_verify::PublicKey;
use ncp_manifest::{
    Channel, Error, LauncherEntry, Manifest, ManifestPak, Sha256Digest, SUPPORTED_SCHEMA_VERSION,
};
use semver::{Version, VersionReq};

// ---------- helpers --------------------------------------------------

struct TestKeys {
    keypair: KeyPair,
    verify_pk: PublicKey,
}

fn fresh_keys() -> TestKeys {
    // Unencrypted keys give us an in-memory secret key that can be
    // passed directly to `sign()` without a password unlock step.
    let kp = KeyPair::generate_unencrypted_keypair().expect("keypair generation should not fail");
    let pk_box: PublicKeyBox = kp.pk.to_box().expect("pk to box");
    let verify_pk =
        PublicKey::decode(&pk_box.to_string()).expect("pubkey roundtrip should not fail");
    TestKeys {
        keypair: kp,
        verify_pk,
    }
}

fn sign_blob(data: &[u8], keys: &TestKeys) -> String {
    let cursor = Cursor::new(data);
    let sig_box: SignatureBox =
        sign(None, &keys.keypair.sk, cursor, None, None).expect("signing should not fail");
    sig_box.into_string()
}

fn current_launcher() -> Version {
    Version::parse("1.0.0").expect("static version literal parses")
}

fn sample_manifest(now: DateTime<Utc>) -> Manifest {
    let mut paks = HashMap::new();
    paks.insert(
        "ncutplus".to_string(),
        ManifestPak {
            version: Version::parse("1.0.0").unwrap(),
            pak_filename: "UTPlus-WindowsNoEditor.pak".to_string(),
            url: "https://example.test/UTPlus-WindowsNoEditor.pak".to_string(),
            sha256: Sha256Digest::from_hex(
                "0000000000000000000000000000000000000000000000000000000000000001",
            )
            .unwrap(),
            size_bytes: 191_268_000,
            load_order: 100,
            required: true,
            depends_on: HashMap::new(),
        },
    );

    let mut channels = HashMap::new();
    channels.insert("stable".to_string(), Channel { paks, plugin: None });

    Manifest {
        schema_version: SUPPORTED_SCHEMA_VERSION,
        generated_at: now - Duration::minutes(5),
        expires_at: now + Duration::days(7),
        sequence: 1,
        min_launcher_version: Version::parse("0.1.0").unwrap(),
        channels,
        launcher: None,
        game_installer: None,
        editor_installer: None,
        openal: None,
        launcher_linux: None,
    }
}

// ---------- happy-path round trip ------------------------------------

#[test]
fn valid_manifest_round_trips() {
    let now = Utc::now();
    let keys = fresh_keys();
    let manifest = sample_manifest(now);
    let json = serde_json::to_vec(&manifest).unwrap();
    let sig = sign_blob(&json, &keys);

    let loaded =
        Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 0)
            .expect("manifest should verify");
    assert_eq!(loaded, manifest);
}

#[test]
fn dependent_pak_with_versionreq_round_trips() {
    let now = Utc::now();
    let keys = fresh_keys();
    let mut manifest = sample_manifest(now);
    {
        let stable = manifest.channels.get_mut("stable").unwrap();
        let mut deps = HashMap::new();
        deps.insert(
            "ncutplus".to_string(),
            VersionReq::parse(">=1.0.0").unwrap(),
        );
        stable.paks.insert(
            "elimplus".to_string(),
            ManifestPak {
                version: Version::parse("0.5.0").unwrap(),
                pak_filename: "ElimPlusMutator-WindowsNoEditor.pak".to_string(),
                url: "https://example.test/ElimPlusMutator-WindowsNoEditor.pak".to_string(),
                sha256: Sha256Digest::from_hex(
                    "0000000000000000000000000000000000000000000000000000000000000002",
                )
                .unwrap(),
                size_bytes: 115_361_000,
                load_order: 200,
                required: false,
                depends_on: deps,
            },
        );
    }
    let json = serde_json::to_vec(&manifest).unwrap();
    let sig = sign_blob(&json, &keys);

    let loaded =
        Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 0)
            .expect("manifest with dep should verify");
    assert_eq!(loaded, manifest);
}

// ---------- launcher entry (hash-pinned self-update) -----------------

#[test]
fn launcher_entry_with_sha256_survives_verification() {
    // A manifest advertising a hash-pinned launcher must verify and preserve the
    // integrity fields through load_and_verify — this is what the launcher reads
    // to decide it can self-download + verify its own replacement.
    let now = Utc::now();
    let keys = fresh_keys();
    let mut manifest = sample_manifest(now);
    manifest.launcher = Some(LauncherEntry {
        version: Version::parse("1.4.1").unwrap(),
        url: "https://example.test/UT4-Community-Launcher-1.4.1.exe".to_string(),
        sha256: Some(
            Sha256Digest::from_hex(
                "0000000000000000000000000000000000000000000000000000000000000099",
            )
            .unwrap(),
        ),
        size_bytes: Some(12_345_678),
    });
    let json = serde_json::to_vec(&manifest).unwrap();
    let sig = sign_blob(&json, &keys);

    let loaded =
        Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 0)
            .expect("manifest with a hash-pinned launcher should verify");
    let launcher = loaded.launcher.expect("launcher entry present");
    assert_eq!(launcher.version, Version::parse("1.4.1").unwrap());
    assert_eq!(launcher.size_bytes, Some(12_345_678));
    assert_eq!(
        launcher.sha256.expect("digest present").to_string(),
        "0000000000000000000000000000000000000000000000000000000000000099"
    );
}

#[test]
fn launcher_entry_without_sha256_verifies_as_notify_only() {
    // Back-compat: a launcher entry authored before the hash-pin hardening (no
    // sha256/size_bytes) must still verify, with both integrity fields absent so
    // the launcher stays on the notify-only path.
    let now = Utc::now();
    let keys = fresh_keys();
    let mut manifest = sample_manifest(now);
    manifest.launcher = Some(LauncherEntry {
        version: Version::parse("1.4.1").unwrap(),
        url: "https://github.com/jmortley/netcodeplus-launcher/releases/tag/v1.4.1".to_string(),
        sha256: None,
        size_bytes: None,
    });
    let json = serde_json::to_vec(&manifest).unwrap();
    let sig = sign_blob(&json, &keys);

    let loaded =
        Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 0)
            .expect("notify-only launcher entry should verify");
    let launcher = loaded.launcher.expect("launcher entry present");
    assert!(launcher.sha256.is_none());
    assert!(launcher.size_bytes.is_none());
}

// ---------- pak filename safety --------------------------------------

fn set_ncutplus_filename(manifest: &mut Manifest, filename: &str) {
    manifest
        .channels
        .get_mut("stable")
        .unwrap()
        .paks
        .get_mut("ncutplus")
        .unwrap()
        .pak_filename = filename.to_string();
}

#[test]
fn pak_filename_with_parent_traversal_is_rejected() {
    let now = Utc::now();
    let keys = fresh_keys();
    let mut manifest = sample_manifest(now);
    // Classic traversal: escape the paks dir and drop a DLL into System32.
    set_ncutplus_filename(&mut manifest, "..\\..\\..\\Windows\\System32\\evil.dll");
    let json = serde_json::to_vec(&manifest).unwrap();
    let sig = sign_blob(&json, &keys);

    let err = Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 0)
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsafePakFilename { .. }),
        "got: {err:?}"
    );
}

#[test]
fn pak_filename_with_absolute_path_is_rejected() {
    let now = Utc::now();
    let keys = fresh_keys();
    let mut manifest = sample_manifest(now);
    set_ncutplus_filename(&mut manifest, "/etc/cron.d/evil");
    let json = serde_json::to_vec(&manifest).unwrap();
    let sig = sign_blob(&json, &keys);

    let err = Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 0)
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsafePakFilename { .. }),
        "got: {err:?}"
    );
}

// ---------- signature failures ---------------------------------------

#[test]
fn signature_made_with_wrong_key_is_rejected() {
    let now = Utc::now();
    let signing_keys = fresh_keys();
    let trusted_keys = fresh_keys(); // distinct key
    let manifest = sample_manifest(now);
    let json = serde_json::to_vec(&manifest).unwrap();
    let sig = sign_blob(&json, &signing_keys);

    let err = Manifest::load_and_verify(
        &json,
        &sig,
        &trusted_keys.verify_pk,
        now,
        &current_launcher(),
        0,
    )
    .unwrap_err();
    assert!(matches!(err, Error::SignatureInvalid), "got {err:?}");
}

#[test]
fn payload_tampered_after_signing_is_rejected() {
    let now = Utc::now();
    let keys = fresh_keys();
    let manifest = sample_manifest(now);
    let json = serde_json::to_vec(&manifest).unwrap();
    let sig = sign_blob(&json, &keys);

    let mut tampered = json.clone();
    let last = tampered.last_mut().expect("json non-empty");
    *last = last.wrapping_add(1);

    let err = Manifest::load_and_verify(
        &tampered,
        &sig,
        &keys.verify_pk,
        now,
        &current_launcher(),
        0,
    )
    .unwrap_err();
    assert!(matches!(err, Error::SignatureInvalid), "got {err:?}");
}

#[test]
fn malformed_signature_is_rejected_before_json_parse() {
    // Both the JSON and the signature are garbage. The crate should
    // surface the *signature* error, not a JSON error, proving we
    // never reach serde_json before signature verification passes.
    let now = Utc::now();
    let keys = fresh_keys();
    let json = b"this is not even json".to_vec();
    let bogus_sig = "not a minisign signature";

    let err = Manifest::load_and_verify(
        &json,
        bogus_sig,
        &keys.verify_pk,
        now,
        &current_launcher(),
        0,
    )
    .unwrap_err();
    assert!(
        matches!(err, Error::SignatureMalformed(_)),
        "expected SignatureMalformed, got {err:?}"
    );
}

// ---------- freshness (expires_at) -----------------------------------

#[test]
fn expired_manifest_is_rejected_even_when_signature_is_valid() {
    let now = Utc::now();
    let keys = fresh_keys();
    let mut manifest = sample_manifest(now);
    manifest.expires_at = now - Duration::seconds(1);
    let json = serde_json::to_vec(&manifest).unwrap();
    let sig = sign_blob(&json, &keys);

    let err = Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 0)
        .unwrap_err();
    assert!(matches!(err, Error::Expired { .. }), "got {err:?}");
}

#[test]
fn manifest_expiring_exactly_now_is_rejected() {
    // Boundary: the spec is `expires_at > now`; equality is rejected.
    let now = Utc::now();
    let keys = fresh_keys();
    let mut manifest = sample_manifest(now);
    manifest.expires_at = now;
    let json = serde_json::to_vec(&manifest).unwrap();
    let sig = sign_blob(&json, &keys);

    let err = Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 0)
        .unwrap_err();
    assert!(matches!(err, Error::Expired { .. }), "got {err:?}");
}

#[test]
fn future_dated_generated_at_is_accepted() {
    // Publisher clock skewed ahead of client. Should still verify so
    // long as expires_at is in the future.
    let now = Utc::now();
    let keys = fresh_keys();
    let mut manifest = sample_manifest(now);
    manifest.generated_at = now + Duration::minutes(10);
    let json = serde_json::to_vec(&manifest).unwrap();
    let sig = sign_blob(&json, &keys);

    let loaded =
        Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 0)
            .expect("future generated_at should be accepted");
    assert_eq!(loaded.generated_at, manifest.generated_at);
}

// ---------- min_launcher_version -------------------------------------

#[test]
fn launcher_older_than_min_required_is_rejected() {
    let now = Utc::now();
    let keys = fresh_keys();
    let mut manifest = sample_manifest(now);
    manifest.min_launcher_version = Version::parse("2.0.0").unwrap();
    let json = serde_json::to_vec(&manifest).unwrap();
    let sig = sign_blob(&json, &keys);

    let err = Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 0)
        .unwrap_err();
    assert!(matches!(err, Error::LauncherTooOld { .. }), "got {err:?}");
}

#[test]
fn launcher_at_exactly_min_required_is_accepted() {
    let now = Utc::now();
    let keys = fresh_keys();
    let mut manifest = sample_manifest(now);
    manifest.min_launcher_version = current_launcher();
    let json = serde_json::to_vec(&manifest).unwrap();
    let sig = sign_blob(&json, &keys);

    Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 0)
        .expect("launcher at exactly min should be accepted");
}

#[test]
fn launcher_newer_than_min_required_is_accepted() {
    let now = Utc::now();
    let keys = fresh_keys();
    let mut manifest = sample_manifest(now);
    manifest.min_launcher_version = Version::parse("0.1.0").unwrap();
    let json = serde_json::to_vec(&manifest).unwrap();
    let sig = sign_blob(&json, &keys);

    Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 0)
        .expect("newer launcher should be accepted");
}

// ---------- schema version -------------------------------------------

#[test]
fn schema_version_zero_is_rejected() {
    let now = Utc::now();
    let keys = fresh_keys();
    let mut manifest = sample_manifest(now);
    manifest.schema_version = 0;
    let json = serde_json::to_vec(&manifest).unwrap();
    let sig = sign_blob(&json, &keys);

    let err = Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 0)
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::SchemaVersionUnsupported {
                got: 0,
                supported: 1
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn schema_version_two_is_rejected() {
    let now = Utc::now();
    let keys = fresh_keys();
    let mut manifest = sample_manifest(now);
    manifest.schema_version = 2;
    let json = serde_json::to_vec(&manifest).unwrap();
    let sig = sign_blob(&json, &keys);

    let err = Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 0)
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::SchemaVersionUnsupported {
                got: 2,
                supported: 1
            }
        ),
        "got {err:?}"
    );
}

// ---------- json invariants ------------------------------------------

#[test]
fn unparseable_json_with_valid_signature_returns_json_error() {
    // Sign non-JSON bytes; signature passes; then JSON parse fails.
    // Proves the error pipeline doesn't conflate sig and parse stages.
    let now = Utc::now();
    let keys = fresh_keys();
    let json = b"not json at all".to_vec();
    let sig = sign_blob(&json, &keys);

    let err = Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 0)
        .unwrap_err();
    assert!(matches!(err, Error::Json(_)), "got {err:?}");
}

#[test]
fn unknown_fields_are_ignored_for_forwards_compat() {
    // Manifests produced by a newer publisher may carry fields this
    // crate does not know about. Ignoring unknown fields keeps old
    // launchers working with newer manifests as long as the
    // schema_version still matches.
    let now = Utc::now();
    let keys = fresh_keys();
    let manifest = sample_manifest(now);
    let mut value: serde_json::Value = serde_json::to_value(&manifest).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("future_field".to_string(), serde_json::json!("hello"));
    let json = serde_json::to_vec(&value).unwrap();
    let sig = sign_blob(&json, &keys);

    let loaded =
        Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 0)
            .expect("unknown fields should be ignored");
    // The known fields are preserved; the unknown one is dropped.
    assert_eq!(loaded.schema_version, manifest.schema_version);
    assert_eq!(loaded.channels.len(), manifest.channels.len());
}

#[test]
fn missing_required_field_is_a_json_error() {
    let now = Utc::now();
    let keys = fresh_keys();
    let manifest = sample_manifest(now);
    let mut value: serde_json::Value = serde_json::to_value(&manifest).unwrap();
    value.as_object_mut().unwrap().remove("expires_at");
    let json = serde_json::to_vec(&value).unwrap();
    let sig = sign_blob(&json, &keys);

    let err = Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 0)
        .unwrap_err();
    assert!(matches!(err, Error::Json(_)), "got {err:?}");
}

// ---------- sequence (replay / downgrade) ----------------------------

#[test]
fn sequence_below_floor_is_rejected() {
    // An older, still-validly-signed, unexpired manifest replayed by an
    // active attacker: signature and expires_at pass, but the sequence is
    // below the highest the launcher has already accepted.
    let now = Utc::now();
    let keys = fresh_keys();
    let mut manifest = sample_manifest(now);
    manifest.sequence = 5;
    let json = serde_json::to_vec(&manifest).unwrap();
    let sig = sign_blob(&json, &keys);

    let err = Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 6)
        .unwrap_err();
    assert!(
        matches!(err, Error::SequenceTooOld { got: 5, minimum: 6 }),
        "got {err:?}"
    );
}

#[test]
fn sequence_equal_to_floor_is_accepted() {
    // Re-fetching the current manifest (same sequence) is idempotent, not
    // a downgrade.
    let now = Utc::now();
    let keys = fresh_keys();
    let mut manifest = sample_manifest(now);
    manifest.sequence = 5;
    let json = serde_json::to_vec(&manifest).unwrap();
    let sig = sign_blob(&json, &keys);

    Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 5)
        .expect("equal sequence should be accepted");
}

#[test]
fn sequence_above_floor_is_accepted() {
    let now = Utc::now();
    let keys = fresh_keys();
    let mut manifest = sample_manifest(now);
    manifest.sequence = 7;
    let json = serde_json::to_vec(&manifest).unwrap();
    let sig = sign_blob(&json, &keys);

    Manifest::load_and_verify(&json, &sig, &keys.verify_pk, now, &current_launcher(), 6)
        .expect("newer sequence should be accepted");
}
