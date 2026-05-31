//! F1: fetch and verify the signed update manifest.
//!
//! This is the first live consumer of the compiled-in trust root
//! ([`crate::trust_root`]). It fetches the manifest JSON and its detached
//! minisign `.minisig` over HTTPS, verifies the signature against the root
//! public key, enforces the freshness / minimum-launcher / replay-sequence /
//! pak-filename invariants (all inside [`ncp_manifest::Manifest::
//! load_and_verify`]), and — on success — advances the persisted replay floor.
//!
//! # Trust boundary
//!
//! The manifest URLs are **untrusted**: integrity comes only from the
//! signature, never from the host or TLS. The verified result is summarised
//! into a small [`ManifestCheckResult`] for the webview; the full manifest is
//! **not** handed to JS. The eventual apply step (F3) re-fetches and
//! re-verifies in Rust and never trusts a value that round-tripped through the
//! frontend.

use serde::Serialize;
use tauri::AppHandle;

use crate::commands::state_path;
use crate::trust_root;

/// Where the signed manifest and its detached signature live.
///
/// A fixed GitHub release tag on the launcher repo holds two assets:
/// `manifest.json` and `manifest.json.minisig`. GitHub 302-redirects asset
/// downloads to its CDN; the net client (rustls + bundled webpki roots)
/// follows that fine — proven by the 2026-05-29 fetch→verify spike. The host
/// is untrusted regardless; the signature is the gate.
///
/// Baked into the binary: changing it is a breaking change requiring a new
/// launcher build, exactly like the trust-root key it is paired with.
const MANIFEST_URL: &str =
    "https://github.com/jmortley/netcodeplus-launcher/releases/download/updates-latest/manifest.json";
const MANIFEST_SIG_URL: &str =
    "https://github.com/jmortley/netcodeplus-launcher/releases/download/updates-latest/manifest.json.minisig";

/// Outcome of a manifest check, summarised for the UI.
///
/// Deliberately small: it carries verification metadata and per-channel
/// availability, never the raw manifest. F2/F3 recompute against a freshly
/// re-verified manifest in Rust rather than trusting anything echoed back from
/// the webview.
#[derive(Debug, Serialize)]
pub struct ManifestCheckResult {
    /// Monotonic publish sequence of the verified manifest.
    pub sequence: u64,
    /// `true` if this manifest advanced the persisted replay floor (i.e. it is
    /// newer than any previously accepted). `false` for an idempotent re-fetch
    /// of the same sequence.
    pub advanced: bool,
    /// RFC 3339 expiry instant carried by the manifest.
    pub expires_at: String,
    /// Channel the launcher is currently tracking (from persisted state).
    pub channel: String,
    /// `true` if that channel exists in the verified manifest.
    pub channel_present: bool,
    /// Number of paks the current channel advertises (0 if the channel is
    /// absent). Informational only — the actual plan is F2's job.
    pub channel_pak_count: usize,
}

/// Fetch the signed update manifest, verify it against the compiled-in trust
/// root, enforce all manifest invariants, and advance the persisted replay
/// floor on success.
///
/// Returns a [`ManifestCheckResult`] summary on success. Any verification
/// failure (bad signature, expired, replayed/downgraded sequence, unsupported
/// schema, unsafe pak filename) or transport error is surfaced as a stringified
/// error for the webview — the manifest is never partially trusted.
#[tauri::command]
pub async fn fetch_and_verify_manifest(app: AppHandle) -> Result<ManifestCheckResult, String> {
    // 1. Load persisted state for the channel + the replay floor. A missing
    //    state file is a legitimate first run (floor 0, default channel).
    let path = state_path(&app)?;
    let mut state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let min_sequence = state.highest_manifest_sequence;

    // 2. Fetch the manifest JSON and its detached signature. Both are small;
    //    the bounded fetch caps the body so a hostile host cannot stream us
    //    gigabytes. The URLs are untrusted — the signature is the gate.
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    let json = ncp_net::fetch_text(&client, MANIFEST_URL, ncp_net::DEFAULT_MAX_MANIFEST_BYTES)
        .await
        .map_err(|e| e.to_string())?;
    let sig = ncp_net::fetch_text(
        &client,
        MANIFEST_SIG_URL,
        ncp_net::DEFAULT_MAX_MANIFEST_BYTES,
    )
    .await
    .map_err(|e| e.to_string())?;

    // 3. Verify signature-first, then all invariants, against the trust root.
    //    `min_sequence` arms the replay/downgrade defense; the current launcher
    //    version arms the min-launcher gate.
    let current_version = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|e| format!("launcher version is not valid semver: {e}"))?;
    let manifest = ncp_manifest::Manifest::load_and_verify(
        json.as_bytes(),
        &sig,
        &trust_root::public_key(),
        chrono::Utc::now(),
        &current_version,
        min_sequence,
    )
    .map_err(|e| e.to_string())?;

    // 4. The manifest verified and passed every invariant. Advance the replay
    //    floor and persist, so a later run rejects any lower-sequence replay.
    //    Persist only when the floor actually moved — an idempotent re-fetch of
    //    the same sequence needs no write.
    let advanced = state.record_manifest_sequence(manifest.sequence);
    if advanced {
        ncp_host::state::write(&path, &state).map_err(|e| e.to_string())?;
    }

    // 5. Summarise for the UI (never the raw manifest).
    let channel = state.channel.clone();
    let channel_entry = manifest.channels.get(&channel);
    Ok(ManifestCheckResult {
        sequence: manifest.sequence,
        advanced,
        expires_at: manifest.expires_at.to_rfc3339(),
        channel_present: channel_entry.is_some(),
        channel_pak_count: channel_entry.map_or(0, |c| c.paks.len()),
        channel,
    })
}
