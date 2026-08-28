//! F1 + F2: fetch/verify the signed update manifest, then plan the update.
//!
//! [`fetch_and_verify_manifest`] (F1) is the first live consumer of the
//! compiled-in trust root ([`crate::trust_root`]). It fetches the manifest JSON
//! and its detached minisign `.minisig` over HTTPS, verifies the signature
//! against the root public key, enforces the freshness / minimum-launcher /
//! replay-sequence / pak-filename invariants (all inside [`ncp_manifest::
//! Manifest::load_and_verify`]), and — on success — advances the persisted
//! replay floor.
//!
//! [`compute_plan`] (F2) re-runs that same verified fetch and then diffs the
//! verified channel against the local install snapshot via [`ncp_planner::plan`]
//! to produce the set of downloads / removals needed.
//!
//! # Trust boundary
//!
//! The manifest URLs are **untrusted**: integrity comes only from the
//! signature, never from the host or TLS. Results are summarised into small
//! serializable structs for the webview; the full manifest is **not** handed to
//! JS. Each command independently re-fetches and re-verifies in Rust — nothing
//! trusts a value that round-tripped through the frontend — and the eventual
//! apply step (F3) does the same before writing a single byte.
//!
//! # Scope (F2)
//!
//! The planner currently models **paks** only. The NetcodePlus *plugin* (a
//! folder, not a single `.pak`) and launcher self-update are not yet in the
//! manifest schema — they need a `kind: plugin | pak | launcher` field — so
//! [`compute_plan`] plans the channel's paks. A channel with no paks yields a
//! clean no-op plan, which is the expected state today.

use serde::Serialize;
use tauri::AppHandle;

use crate::commands::{active_mod_paks_dir, state_path};
use crate::trust_root;

/// Where the signed manifest and its detached signature live.
///
/// A fixed GitHub release tag on the launcher repo holds two assets:
/// `manifest.json` and `manifest.json.minisig`. GitHub 302-redirects asset
/// downloads to its CDN; the net client (rustls + bundled webpki roots)
/// follows that fine — proven by the 2026-05-29 fetch→verify spike, and again
/// end-to-end against the production key on 2026-05-31. The host is untrusted
/// regardless; the signature is the gate.
///
/// Baked into the binary: changing it is a breaking change requiring a new
/// launcher build, exactly like the trust-root key it is paired with.
const MANIFEST_URL: &str =
    "https://github.com/jmortley/netcodeplus-launcher/releases/download/updates-latest/manifest.json";
const MANIFEST_SIG_URL: &str =
    "https://github.com/jmortley/netcodeplus-launcher/releases/download/updates-latest/manifest.json.minisig";

/// Fetch the manifest + signature, verify against the trust root, and advance
/// the persisted replay floor — the shared core of F1 and F2.
///
/// Returns the verified [`ncp_manifest::Manifest`] together with the persisted
/// [`ncp_host::LauncherState`] (already reflecting the advanced sequence floor,
/// so callers can read `channel` / `local_install` / `opted_out` from it). The
/// state file is written only when the floor actually moved.
///
/// Keeping this private means the verified manifest never leaves Rust; the
/// public commands return summaries built from it.
pub(crate) async fn fetch_verify(
    app: &AppHandle,
) -> Result<
    (
        ncp_manifest::Manifest,
        ncp_host::LauncherState,
        String,
        String,
    ),
    String,
> {
    // Load persisted state for the channel + the replay floor. A missing state
    // file is a legitimate first run (floor 0, default channel).
    let path = state_path(app)?;
    let mut state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let min_sequence = state.highest_manifest_sequence;

    // Fetch the manifest JSON and its detached signature. Both are small; the
    // bounded fetch caps the body so a hostile host cannot stream us gigabytes.
    // The URLs are untrusted — the signature is the gate.
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

    // Verify signature-first, then all invariants, against the trust root.
    // `min_sequence` arms the replay/downgrade defense; the current launcher
    // version arms the min-launcher gate.
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

    // The manifest verified and passed every invariant. Advance the replay
    // floor and persist, so a later run rejects any lower-sequence replay.
    // Persist only when the floor actually moved — an idempotent re-fetch of the
    // same sequence needs no write.
    if state.record_manifest_sequence(manifest.sequence) {
        ncp_host::state::write(&path, &state).map_err(|e| e.to_string())?;
    }

    // Return the raw JSON + signature too: the elevated installer re-verifies
    // them against the trust root itself (it must not trust a caller-supplied
    // hash). Callers that don't need them ignore the last two fields.
    Ok((manifest, state, json, sig))
}

/// Outcome of a manifest check, summarised for the UI.
///
/// Deliberately small: it carries verification metadata and per-channel
/// availability, never the raw manifest.
#[derive(Debug, Serialize)]
pub struct ManifestCheckResult {
    /// Monotonic publish sequence of the verified manifest.
    pub sequence: u64,
    /// RFC 3339 expiry instant carried by the manifest.
    pub expires_at: String,
    /// Channel the launcher is currently tracking (from persisted state).
    pub channel: String,
    /// `true` if that channel exists in the verified manifest.
    pub channel_present: bool,
    /// Number of paks the current channel advertises (0 if the channel is
    /// absent). Informational only — the actual plan is [`compute_plan`].
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
    let (manifest, state, _, _) = fetch_verify(&app).await?;
    let channel = state.channel;
    let channel_entry = manifest.channels.get(&channel);
    Ok(ManifestCheckResult {
        sequence: manifest.sequence,
        expires_at: manifest.expires_at.to_rfc3339(),
        channel_present: channel_entry.is_some(),
        channel_pak_count: channel_entry.map_or(0, |c| c.paks.len()),
        channel,
    })
}

/// One pak the plan would download, summarised for the UI.
#[derive(Debug, Serialize)]
pub struct PlanDownload {
    /// Stable pak id.
    pub id: String,
    /// Target filename in the install.
    pub filename: String,
    /// Version the manifest offers.
    pub version: String,
    /// Download size in bytes (for a progress estimate / confirm prompt).
    pub size_bytes: u64,
    /// `"missing"` (not installed) or `"hash_mismatch"` (installed but stale).
    pub reason: &'static str,
    /// Whether the pak is required (cannot be opted out of).
    pub required: bool,
}

/// One pak the plan would remove, summarised for the UI.
#[derive(Debug, Serialize)]
pub struct PlanRemove {
    /// Stable pak id.
    pub id: String,
    /// Filename currently on disk.
    pub filename: String,
    /// `"not_in_channel"` (gone from the manifest) or `"opted_out"`.
    pub reason: &'static str,
}

/// A computed update plan, summarised for the UI.
#[derive(Debug, Serialize)]
pub struct PlanResult {
    /// Channel the plan was computed for.
    pub channel: String,
    /// `true` if no downloads and no removals are needed.
    pub up_to_date: bool,
    /// Paks to download.
    pub to_download: Vec<PlanDownload>,
    /// Paks to remove.
    pub to_remove: Vec<PlanRemove>,
    /// Count of paks already at the desired version + hash.
    pub keep_count: usize,
    /// Total bytes to download across `to_download`.
    pub total_download_bytes: u64,
}

/// Fetch + verify the manifest, then compute the update plan for the current
/// channel by diffing it against the local install snapshot.
///
/// Re-verifies independently (it does not trust any prior call's result), then
/// calls [`ncp_planner::plan`]. Planner errors (unknown channel, an opt-out of
/// a required or unknown pak, a broken dependency) are surfaced as stringified
/// errors. The local install snapshot and the opt-out set come from persisted
/// state.
///
/// Scope: paks only — see the module docs. The eventual F3 apply re-fetches and
/// re-verifies before installing anything.
#[tauri::command]
pub async fn compute_plan(app: AppHandle) -> Result<PlanResult, String> {
    let (manifest, state, _, _) = fetch_verify(&app).await?;
    let channel = state.channel.clone();

    let plan = ncp_planner::plan(&manifest, &channel, &state.local_install, &state.opted_out)
        .map_err(|e| e.to_string())?;

    Ok(PlanResult {
        channel,
        up_to_date: plan.is_no_op(),
        to_download: plan.to_download.iter().map(plan_download).collect(),
        to_remove: plan.to_remove.iter().map(plan_remove).collect(),
        keep_count: plan.to_keep.len(),
        total_download_bytes: plan.total_download_bytes(),
    })
}

// ===================================================================
// Pak maintainer (NetcodePlus content paks) — keeps the client's mod
// paks current in the per-user DownloadedPaks dir, so a player has the
// content (sounds etc.) regardless of whether a server pushes it.
//
// Paks live in ONE global per-user directory
// (`ncp_host::default_mod_paks_dir`), not per UT4 install, so unlike the
// plugin these commands take no `roots`. The plan is hash-based against
// the signed manifest; the apply step refuses while UT4 is running (a
// mounted pak is locked) and writes each pak under its canonical engine
// filename so it coincides with — rather than duplicates — a server push.
// ===================================================================

/// Pak status for the current channel, summarised for the UI's Play-gate.
///
/// Drives which prompt the launcher shows on PLAY: when nothing is installed
/// yet (`installed_count == 0`) it's a first-time "get the paks" prompt; when
/// some are installed but `to_download` is non-empty it's an "out of date"
/// prompt. `up_to_date` short-circuits both.
#[derive(Debug, Serialize)]
pub struct PakStatusResult {
    /// Channel the status was computed for.
    pub channel: String,
    /// `true` when the channel advertises at least one pak.
    pub paks_offered: bool,
    /// `true` when no downloads and no removals are needed.
    pub up_to_date: bool,
    /// How many paks are currently recorded as installed (0 ⇒ first-time user).
    pub installed_count: usize,
    /// Paks to download (with reason + size), reusing the plan summary shape.
    pub to_download: Vec<PlanDownload>,
    /// Paks to remove because they are gone from the channel.
    ///
    /// Opt-out removals are deliberately NOT listed: unticking a pak stops the
    /// launcher maintaining it, it does not delete the file the player already
    /// has (their hub may still be serving that version). Leaving them in would
    /// also pin `up_to_date` to false forever, since the removal never happens.
    pub to_remove: Vec<PlanRemove>,
    /// Count already at the desired version + hash.
    pub keep_count: usize,
    /// Total bytes to download across `to_download`.
    pub total_download_bytes: u64,
    /// Every pak the channel offers, with its required / opted-out / installed
    /// state — drives the pak checkbox list. Sorted required-first then by id,
    /// so the UI does not have to.
    pub catalogue: Vec<PakChoice>,
}

/// One row of the pak checkbox list.
#[derive(Debug, Serialize)]
pub struct PakChoice {
    /// Stable pak id (the manifest key) — what `set_pak_opt_out` takes.
    pub id: String,
    /// Filename in the paks dir.
    pub filename: String,
    /// Manifest version string.
    pub version: String,
    /// Download size.
    pub size_bytes: u64,
    /// `true` ⇒ the user cannot untick it. The planner rejects opting out of a
    /// required pak outright (`PlanError::CannotOptOutOfRequired`), so this is
    /// enforced below the UI, not just in it.
    pub required: bool,
    /// `true` ⇒ the user has unticked it, so it is no longer maintained.
    pub opted_out: bool,
    /// `true` ⇒ recorded as installed on this machine.
    pub installed: bool,
}

/// Reconcile the recorded local install against what's actually in the paks dir
/// before planning, using a cheap size+mtime **stamp** (never a re-hash, so a
/// OneDrive cloud-only placeholder is left un-hydrated):
///
/// 1. **Forget** any recorded pak whose file is gone (deleted) OR whose size /
///    mtime no longer matches the stamp recorded at install — i.e. a third party
///    (a server's redirect) overwrote it in place with a different version. The
///    pak then re-plans as Missing and is restored to the manifest's version,
///    instead of the stale record falsely reporting "up to date". A record with
///    no stamp (legacy) falls back to an existence check and is stamped in step 2.
/// 2. **Adopt** any manifest pak already present whose bytes match the signed
///    sha256 but isn't recorded yet (a server pushed it into the same
///    DownloadedPaks dir, or a prior install), and **stamp** any recorded pak
///    that lacks one — so future in-place overwrites are caught. The adopt hash
///    is the only disk read here, and only for a present-but-unrecorded pak; a
///    pak already recorded current is checked by stat alone.
///
/// Returns `true` if the recorded state changed (caller persists).
fn reconcile_paks_with_disk(
    channel: &ncp_manifest::Channel,
    mod_paks_dir: &std::path::Path,
    state: &mut ncp_host::LauncherState,
) -> bool {
    let mut dirty = false;

    // 1. Forget gone / overwritten-in-place paks (metadata stat only).
    let forget: Vec<String> = state
        .local_install
        .paks
        .iter()
        .filter_map(|(id, rec)| {
            match ncp_host::pak_file_stamp(mod_paks_dir, &rec.pak_filename) {
                None => Some(id.clone()), // file gone
                Some((size, mtime_ms)) => match state.pak_stamps.get(id) {
                    // A stamped record whose file no longer matches it changed
                    // underneath us (a server pushed a different version).
                    Some(s) if s.size_bytes != size || s.mtime_ms != mtime_ms => Some(id.clone()),
                    _ => None, // matches, or legacy unstamped (kept, stamped below)
                },
            }
        })
        .collect();
    for id in forget {
        state.local_install.paks.remove(&id);
        state.pak_stamps.remove(&id);
        dirty = true;
    }

    // 2. Adopt present-matching-but-unrecorded paks, then stamp every recorded
    //    pak that still lacks a stamp (a fresh adoption, or a legacy record).
    for (id, pak) in &channel.paks {
        let recorded_current = state
            .local_install
            .paks
            .get(id)
            .is_some_and(|l| l.sha256 == pak.sha256);
        if !recorded_current
            && ncp_host::pak_on_disk_digest(mod_paks_dir, &pak.pak_filename) == Some(pak.sha256)
        {
            state.local_install.paks.insert(
                id.clone(),
                ncp_planner::LocalPak {
                    pak_filename: pak.pak_filename.clone(),
                    version: pak.version.clone(),
                    sha256: pak.sha256,
                },
            );
            dirty = true;
        }
        // Stamp a recorded-but-unstamped pak off its RECORDED filename (what's
        // actually on disk), not the manifest filename — they differ for a pak the
        // manifest renamed but that's still on disk under the old name, and the
        // forget pass above keys off the recorded filename too.
        let unstamped_filename = (!state.pak_stamps.contains_key(id))
            .then(|| {
                state
                    .local_install
                    .paks
                    .get(id)
                    .map(|l| l.pak_filename.clone())
            })
            .flatten();
        if let Some(fname) = unstamped_filename {
            if let Some((size, mtime_ms)) = ncp_host::pak_file_stamp(mod_paks_dir, &fname) {
                state.pak_stamps.insert(
                    id.clone(),
                    ncp_host::PakStamp {
                        size_bytes: size,
                        mtime_ms,
                    },
                );
                dirty = true;
            }
        }
    }
    dirty
}

/// Report the NetcodePlus pak status for the current channel by re-verifying the
/// manifest, reconciling the recorded install against what's actually on disk
/// (so an already-present matching pak isn't re-offered), then diffing against
/// the channel's paks. Adopting a present pak persists the record; otherwise no
/// write.
#[tauri::command]
pub async fn pak_status(app: AppHandle) -> Result<PakStatusResult, String> {
    let (manifest, mut state, _, _) = fetch_verify(&app).await?;
    let channel = state.channel.clone();
    let paks_offered = manifest
        .channels
        .get(&channel)
        .is_some_and(|c| !c.paks.is_empty());

    // Reflect what's actually on disk before planning: forget paks whose file
    // was deleted (so they re-offer) and adopt any present pak that already
    // matches the manifest (so they're not re-downloaded). Persist only on change.
    if let (Some(ch), Some(dir)) = (manifest.channels.get(&channel), active_mod_paks_dir(&app)) {
        if reconcile_paks_with_disk(ch, &dir, &mut state) {
            let path = state_path(&app)?;
            ncp_host::state::write(&path, &state).map_err(|e| e.to_string())?;
        }
    }

    // Drop any opt-out of a pak the manifest now marks REQUIRED before planning.
    // The planner treats that combination as a hard error, so a stale opt-out
    // left over from a manifest where the pak was optional would make every
    // subsequent pak_status fail — self-heal instead of erroring at the user.
    if prune_required_opt_outs(&manifest, &channel, &mut state) {
        let path = state_path(&app)?;
        ncp_host::state::write(&path, &state).map_err(|e| e.to_string())?;
    }

    let plan = ncp_planner::plan(&manifest, &channel, &state.local_install, &state.opted_out)
        .map_err(|e| e.to_string())?;

    // Count only THIS channel's paks the user actually has on disk (recorded) —
    // not the raw global map — so the Play-gate picks "first-time" vs "out of
    // date" off the current channel, ignoring any unrelated leftover record.
    let installed_count = manifest.channels.get(&channel).map_or(0, |c| {
        c.paks
            .keys()
            .filter(|id| state.local_install.paks.contains_key(*id))
            .count()
    });

    // Unticking a pak stops us maintaining it; it does not delete what the
    // player already has. So opt-out removals are dropped here and never reach
    // the UI or `up_to_date` (see PakStatusResult::to_remove).
    let to_remove: Vec<PlanRemove> = plan
        .to_remove
        .iter()
        .filter(|a| !matches!(a.reason, ncp_planner::RemoveReason::OptedOut))
        .map(plan_remove)
        .collect();

    let mut catalogue: Vec<PakChoice> = manifest
        .channels
        .get(&channel)
        .map(|c| {
            c.paks
                .iter()
                .map(|(id, p)| PakChoice {
                    id: id.clone(),
                    filename: p.pak_filename.clone(),
                    version: p.version.to_string(),
                    size_bytes: p.size_bytes,
                    required: p.required,
                    opted_out: state.opted_out.contains(id),
                    installed: state.local_install.paks.contains_key(id),
                })
                .collect()
        })
        .unwrap_or_default();
    // Required first, then alphabetical — a stable order regardless of the
    // manifest's HashMap iteration order, which is not deterministic.
    catalogue.sort_by(|a, b| b.required.cmp(&a.required).then_with(|| a.id.cmp(&b.id)));

    Ok(PakStatusResult {
        channel,
        paks_offered,
        up_to_date: plan.to_download.is_empty() && to_remove.is_empty(),
        installed_count,
        to_download: plan.to_download.iter().map(plan_download).collect(),
        to_remove,
        keep_count: plan.to_keep.len(),
        total_download_bytes: plan.total_download_bytes(),
        catalogue,
    })
}

/// Remove from `state.opted_out` any id the channel now marks required (or that
/// the channel no longer offers at all). Returns `true` when the set changed and
/// the caller should persist. Keeps the launcher self-healing across a manifest
/// that promotes an optional pak to required.
fn prune_required_opt_outs(
    manifest: &ncp_manifest::Manifest,
    channel: &str,
    state: &mut ncp_host::LauncherState,
) -> bool {
    let Some(ch) = manifest.channels.get(channel) else {
        return false;
    };
    let before = state.opted_out.len();
    state
        .opted_out
        .retain(|id| ch.paks.get(id).is_some_and(|p| !p.required));
    state.opted_out.len() != before
}

/// Tick / untick one optional pak.
///
/// Unticking records the id in `opted_out`, which makes the planner stop
/// offering updates for it; the file already on disk is left alone. Ticking it
/// again resumes maintenance and re-downloads if it has since changed.
///
/// # Errors
/// Rejects an unknown pak id, and refuses to untick a pak the manifest marks
/// required — the planner would otherwise fail every later plan outright.
#[tauri::command]
pub async fn set_pak_opt_out(
    app: AppHandle,
    pak_id: String,
    opted_out: bool,
) -> Result<(), String> {
    let (manifest, mut state, _, _) = fetch_verify(&app).await?;
    let channel = state.channel.clone();
    let pak = manifest
        .channels
        .get(&channel)
        .and_then(|c| c.paks.get(&pak_id))
        .ok_or_else(|| format!("unknown pak '{pak_id}' in channel '{channel}'"))?;

    if opted_out && pak.required {
        return Err(format!("'{pak_id}' is required and cannot be turned off"));
    }

    let changed = if opted_out {
        state.opted_out.insert(pak_id)
    } else {
        state.opted_out.remove(&pak_id)
    };
    if changed {
        let path = state_path(&app)?;
        ncp_host::state::write(&path, &state).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Map a planner [`ncp_planner::DownloadAction`] to the UI summary.
fn plan_download(a: &ncp_planner::DownloadAction) -> PlanDownload {
    PlanDownload {
        id: a.id.clone(),
        filename: a.manifest_pak.pak_filename.clone(),
        version: a.manifest_pak.version.to_string(),
        size_bytes: a.manifest_pak.size_bytes,
        reason: match a.reason {
            ncp_planner::DownloadReason::Missing => "missing",
            ncp_planner::DownloadReason::HashMismatch => "hash_mismatch",
        },
        required: a.manifest_pak.required,
    }
}

/// Map a planner [`ncp_planner::RemoveAction`] to the UI summary.
fn plan_remove(a: &ncp_planner::RemoveAction) -> PlanRemove {
    PlanRemove {
        id: a.id.clone(),
        filename: a.local_pak.pak_filename.clone(),
        reason: match a.reason {
            ncp_planner::RemoveReason::NotInChannel => "not_in_channel",
            ncp_planner::RemoveReason::OptedOut => "opted_out",
        },
    }
}

/// Per-pak outcome of an [`install_paks`] run.
#[derive(Debug, Serialize)]
pub struct PakInstallOutcome {
    /// Stable pak id.
    pub id: String,
    /// Filename acted on.
    pub filename: String,
    /// `"installed"` | `"removed"` | `"renamed"` | `"failed"`.
    pub result: &'static str,
    /// Human-readable detail (version installed, or the error).
    pub detail: String,
}

/// `pak-install-progress` event payload — coarse, per-pak progress so the UI can
/// show "Installing paks… (2 of 6: WipeoutMutator)". `done` counts paks finished
/// (downloaded + placed) before `filename` started.
#[derive(Clone, Serialize)]
struct PakInstallProgress {
    done: usize,
    total: usize,
    filename: String,
}

fn emit_pak_progress(app: &AppHandle, done: usize, total: usize, filename: &str) {
    use tauri::Emitter;
    let _ = app.emit(
        "pak-install-progress",
        PakInstallProgress {
            done,
            total,
            filename: filename.to_string(),
        },
    );
}

/// How many times to (re)try a single pak's download + install before giving up.
/// Covers a transient network drop or a brief filesystem lock — an AV scan, or
/// OneDrive syncing the paks dir while several large paks land at once (the dir
/// is under the user's Documents, which may be OneDrive-redirected). Three
/// attempts × `install_pak`'s internal ~1.5s rename backoff gives a lock ~4.5s to
/// clear, by which time the sibling paks have finished syncing.
const PAK_INSTALL_ATTEMPTS: usize = 3;

/// Per-run-unique staging filename for a pak download: `.<name>.incoming.<pid>.
/// <nanos>`. The nanos nonce means a verified-but-uninstalled staging file
/// orphaned by a killed prior process can never be mistaken — via the
/// `!staged.exists()` skip-download check — for a fresh download and installed
/// without re-verification, even if the OS reuses the PID. Mirrors `ncp_net`'s
/// own `.partial.<pid>.<nanos>` staging.
fn staging_name(pak_filename: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(".{pak_filename}.incoming.{}.{nanos}", std::process::id())
}

/// Best-effort removal of leftover `.<name>.incoming.*` staging files from a
/// prior interrupted install (a process killed between download and placement).
/// The frontend single-flights install_paks and the single-instance launcher
/// rules out a second process, so sweeping all of them at the start is safe — it
/// just keeps orphaned ~100 MB staging files from piling up in the paks dir.
fn sweep_pak_staging(mod_paks_dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(mod_paks_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') && name.contains(".incoming.") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Download `pak` (streaming SHA-256 + size verified against the signed manifest)
/// and place it under its canonical filename in `mod_paks_dir`, retrying a
/// transient failure up to [`PAK_INSTALL_ATTEMPTS`] times.
///
/// A *download* failure re-fetches. An *install* (rename) failure retries the
/// placement with the already-verified staged file still in hand — no
/// re-download — so a brief lock on the destination clears without re-pulling
/// ~120 MB. Returns `Ok(())` once the verified bytes are in place, or
/// `Err(detail)` with the last failure after exhausting the retries. Never leaves
/// a staging file behind.
async fn download_and_install_pak(
    client: &ncp_net::Client,
    pak: &ncp_manifest::ManifestPak,
    mod_paks_dir: &std::path::Path,
) -> std::result::Result<(), String> {
    // Stage as a sibling in the paks dir so the final rename is same-filesystem.
    // The per-run nonce means a stale staging file from a killed prior process is
    // never picked up as if it were this run's verified download.
    let staged = mod_paks_dir.join(staging_name(&pak.pak_filename));
    let mut last = String::new();
    for _ in 0..PAK_INSTALL_ATTEMPTS {
        // (Re)download only when we don't already hold a verified staged file —
        // so a retry after an install/rename lock doesn't re-fetch the bytes.
        if !staged.exists() {
            if let Err(e) =
                ncp_net::download(client, &pak.url, pak.sha256, pak.size_bytes, &staged).await
            {
                last = format!("download/verify failed: {e}");
                let _ = std::fs::remove_file(&staged);
                continue;
            }
        }
        match ncp_host::install_pak(&staged, mod_paks_dir, &pak.pak_filename) {
            Ok(()) => return Ok(()),
            // Keep the verified staged file so the next attempt retries the
            // placement (a brief OneDrive/AV lock on the dest clears) without
            // re-downloading; `install_pak` already backs off internally.
            Err(e) => last = format!("install failed: {e}"),
        }
    }
    let _ = std::fs::remove_file(&staged);
    Err(last)
}

/// Bring the local NetcodePlus paks into agreement with the signed manifest:
/// download + install everything `plan` says is missing or stale, rename a pak
/// the manifest renamed, and remove paks no longer in the channel (or opted
/// out). Records each change in the persisted local-install snapshot.
///
/// **Refuses while UT4 is running** — the engine holds an open handle on every
/// mounted pak, so a replace would fail with a sharing violation. The PLAY-gate
/// calls this with the game closed (it runs *before* launch), and we re-check in
/// Rust so a stale frontend can't drive a doomed write.
///
/// Each pak is independent and is retried a few times on a transient failure
/// (see [`download_and_install_pak`]) before it records a `"failed"` outcome; a
/// failure on one never aborts the rest, and successful changes are still
/// persisted — so a Play-gate "Install & Play" proceeds with whatever installed,
/// and any still-failed pak is picked up on the next run (no manual re-click for
/// a one-off blip).
#[tauri::command]
pub async fn install_paks(app: AppHandle) -> Result<Vec<PakInstallOutcome>, String> {
    let (manifest, mut state, _, _) = fetch_verify(&app).await?;
    let channel = state.channel.clone();

    // A mounted pak is locked — refuse rather than fight the file lock.
    if crate::commands::shipping_client_running() {
        return Err(
            "Close Unreal Tournament first — its content paks can't be updated while it's running."
                .into(),
        );
    }

    let Some(mod_paks_dir) = active_mod_paks_dir(&app) else {
        return Err("Couldn't locate your Documents folder to install paks into.".into());
    };
    // Clear any leftover staging files from an interrupted prior install.
    sweep_pak_staging(&mod_paks_dir);

    let mut state_dirty = false;
    // Reconcile with disk first: forget deleted / overwritten paks (so they
    // re-download) and adopt already-present matching ones (so a cold install
    // doesn't re-pull a pak the user already has).
    if let Some(ch) = manifest.channels.get(&channel) {
        if reconcile_paks_with_disk(ch, &mod_paks_dir, &mut state) {
            state_dirty = true;
        }
    }

    let plan = ncp_planner::plan(&manifest, &channel, &state.local_install, &state.opted_out)
        .map_err(|e| e.to_string())?;

    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    let mut outcomes: Vec<PakInstallOutcome> = Vec::new();

    // 1. Renames — the planner's "same content, new filename" keep case. Cheap
    //    (no download): rename in place and fix the recorded filename.
    for keep in &plan.to_keep {
        if keep.local_pak.pak_filename == keep.manifest_pak.pak_filename {
            continue;
        }
        match ncp_host::rename_pak(
            &mod_paks_dir,
            &keep.local_pak.pak_filename,
            &keep.manifest_pak.pak_filename,
        ) {
            Ok(()) => {
                if let Some(rec) = state.local_install.paks.get_mut(&keep.id) {
                    rec.pak_filename = keep.manifest_pak.pak_filename.clone();
                }
                state_dirty = true;
                outcomes.push(PakInstallOutcome {
                    id: keep.id.clone(),
                    filename: keep.manifest_pak.pak_filename.clone(),
                    result: "renamed",
                    detail: format!("renamed from {}", keep.local_pak.pak_filename),
                });
            }
            Err(e) => outcomes.push(PakInstallOutcome {
                id: keep.id.clone(),
                filename: keep.manifest_pak.pak_filename.clone(),
                result: "failed",
                detail: format!("rename failed: {e}"),
            }),
        }
    }

    // 2. Downloads — verify against the signed sha256 + size, then place the
    //    verified file under its canonical name.
    let total = plan.to_download.len();
    for (i, action) in plan.to_download.iter().enumerate() {
        let pak = &action.manifest_pak;
        emit_pak_progress(&app, i, total, &pak.pak_filename);

        match download_and_install_pak(&client, pak, &mod_paks_dir).await {
            Ok(()) => {
                state.local_install.paks.insert(
                    action.id.clone(),
                    ncp_planner::LocalPak {
                        pak_filename: pak.pak_filename.clone(),
                        version: pak.version.clone(),
                        sha256: pak.sha256,
                    },
                );
                // Stamp the just-written file (size + mtime) so a later in-place
                // overwrite by a server redirect is caught without re-hashing.
                if let Some((size, mtime_ms)) =
                    ncp_host::pak_file_stamp(&mod_paks_dir, &pak.pak_filename)
                {
                    state.pak_stamps.insert(
                        action.id.clone(),
                        ncp_host::PakStamp {
                            size_bytes: size,
                            mtime_ms,
                        },
                    );
                }
                state_dirty = true;
                outcomes.push(PakInstallOutcome {
                    id: action.id.clone(),
                    filename: pak.pak_filename.clone(),
                    result: "installed",
                    detail: format!("v{}", pak.version),
                });
            }
            Err(detail) => outcomes.push(PakInstallOutcome {
                id: action.id.clone(),
                filename: pak.pak_filename.clone(),
                result: "failed",
                detail,
            }),
        }
    }
    emit_pak_progress(&app, total, total, "");

    // 3. Removes — paks GONE FROM THE CHANNEL only.
    //
    // An opt-out is not a delete: unticking a pak means "stop maintaining this",
    // and the copy already on disk stays because the player's hub may still be
    // serving that exact version. Deleting it would break them on that hub for a
    // choice that reads as "don't bother me about updates".
    for action in plan
        .to_remove
        .iter()
        .filter(|a| !matches!(a.reason, ncp_planner::RemoveReason::OptedOut))
    {
        match ncp_host::remove_pak(&mod_paks_dir, &action.local_pak.pak_filename) {
            Ok(()) => {
                state.local_install.paks.remove(&action.id);
                state.pak_stamps.remove(&action.id);
                state_dirty = true;
                outcomes.push(PakInstallOutcome {
                    id: action.id.clone(),
                    filename: action.local_pak.pak_filename.clone(),
                    result: "removed",
                    detail: "no longer in channel".into(),
                });
            }
            Err(e) => outcomes.push(PakInstallOutcome {
                id: action.id.clone(),
                filename: action.local_pak.pak_filename.clone(),
                result: "failed",
                detail: format!("remove failed: {e}"),
            }),
        }
    }

    if state_dirty {
        // Persist the pak changes WITHOUT clobbering anything written during this
        // (multi-minute) install: re-read the current state and overwrite only the
        // pak fields, rather than saving this command's minutes-old whole-state
        // snapshot — which would silently revert a login, a presence toggle, or a
        // focus-triggered pak_status that wrote state meanwhile. install_paks is
        // authoritative for the current channel's pak records, so its view of
        // local_install + pak_stamps is the one to keep.
        let path = state_path(&app)?;
        let mut fresh = ncp_host::state::read(&path)
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        fresh.local_install = state.local_install;
        fresh.pak_stamps = state.pak_stamps;
        ncp_host::state::write(&path, &fresh).map_err(|e| e.to_string())?;
    }

    Ok(outcomes)
}

/// Remove ONE launcher-installed pak from `DownloadedPaks` on demand — the
/// "revert so the hub can serve its own version" UI action. Drops the file and
/// forgets the recorded install so the next server that offers this content is
/// free to send its own copy.
///
/// **Refuses while UT4 is running** — the same file-lock reason `install_paks`
/// refuses: the engine holds an open handle on every mounted pak, so a delete
/// would fail with a sharing violation.
///
/// Idempotent: if `pak_id` isn't in the local-install snapshot it's already
/// gone, so this returns `Ok(())` without touching disk.
#[tauri::command]
pub async fn remove_installed_pak(app: AppHandle, pak_id: String) -> Result<(), String> {
    let path = state_path(&app)?;
    let mut state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    // Not recorded as installed by the launcher — treat as already-removed so a
    // double-click / stale UI can't error.
    let Some(local) = state.local_install.paks.get(&pak_id) else {
        return Ok(());
    };
    let filename = local.pak_filename.clone();

    // A mounted pak is locked — refuse rather than fight the file lock.
    if crate::commands::shipping_client_running() {
        return Err("Close UT4 to change content paks.".into());
    }

    let Some(mod_paks_dir) = active_mod_paks_dir(&app) else {
        return Err("Couldn't locate your Documents folder to remove the pak from.".into());
    };

    ncp_host::remove_pak(&mod_paks_dir, &filename).map_err(|e| e.to_string())?;

    state.local_install.paks.remove(&pak_id);
    state.pak_stamps.remove(&pak_id);
    ncp_host::state::write(&path, &state).map_err(|e| e.to_string())?;

    Ok(())
}

// ===================================================================
// Plugin update (NetcodePlus) — across ALL detected installs.
// ===================================================================

/// The plugin status of one detected install, for the UI.
#[derive(Debug, Serialize)]
pub struct PluginInstallStatus {
    /// Install root path (the key used in state's `installed_plugins`).
    pub root: String,
    /// `"none"` | `"install"` | `"update"` | `"up_to_date"` | `"downgrade_blocked"`
    /// — the [`ncp_planner::PluginAction`] discriminant for this install.
    pub action: &'static str,
    /// Build number currently recorded as installed here, if any.
    pub installed_version: Option<u32>,
    /// Build number the manifest offers (None when the channel has no plugin).
    pub available_version: Option<u32>,
    /// Whether a well-formed `Plugins/NetcodePlus/` folder is on disk right now.
    /// Lets the UI tell a hand-installed (present but unrecorded → `action ==
    /// "install"`) plugin apart from a genuinely missing one, so it can verify
    /// the on-disk bytes instead of nagging "not installed". See `verify_plugin`.
    pub present: bool,
    /// Whether this install has a content-fingerprint baseline recorded (so drift
    /// can be checked locally). `false` for a hand-install (no record) or a record
    /// written before fingerprints existed — the UI runs `verify_plugin` once for
    /// a present-but-unbaselined install to establish ground truth.
    pub baselined: bool,
}

/// Overall plugin status across all installs, for the UI's plugin card.
#[derive(Debug, Serialize)]
pub struct PluginStatusResult {
    /// `true` when the current channel advertises a plugin at all.
    pub plugin_offered: bool,
    /// The offered build number, if any.
    pub available_version: Option<u32>,
    /// Per-install status (one entry per detected UT4 install).
    pub installs: Vec<PluginInstallStatus>,
    /// `true` when at least one install needs an install/update.
    pub any_update_needed: bool,
    /// Release-notes URL for the offered build (the manifest entry's
    /// `notes_url`), if it carries one. The UI shows a "what's new" link when
    /// an install/update is pending; `None` → the UI falls back to the
    /// plugin's releases page.
    pub notes_url: Option<String>,
}

/// Map a [`ncp_planner::PluginAction`] to its UI discriminant + the installed
/// version it carries (if any).
fn plugin_action_str(a: &ncp_planner::PluginAction) -> &'static str {
    use ncp_planner::PluginAction::*;
    match a {
        None => "none",
        Install { .. } => "install",
        Update { .. } => "update",
        UpToDate { .. } => "up_to_date",
        DowngradeBlocked { .. } => "downgrade_blocked",
    }
}

/// Resolve the install roots to act on. When the webview supplies its known
/// install roots (`state.installs`, which includes a manually-picked install
/// that auto-detection can't find), validate each through `check_install` — so
/// we only ever act on a real UT4 tree, never an arbitrary path — and
/// de-duplicate on the canonical root. With no roots (older frontend / empty
/// list), fall back to auto-detection, preserving the original behaviour.
fn resolve_roots(roots: Option<Vec<String>>) -> Vec<std::path::PathBuf> {
    match roots {
        Some(rs) if !rs.is_empty() => {
            let Some(mod_paks_dir) = ncp_host::default_mod_paks_dir() else {
                return Vec::new();
            };
            let mut out: Vec<std::path::PathBuf> = Vec::new();
            for r in rs {
                if let Some(install) =
                    ncp_host::check_install(std::path::Path::new(&r), mod_paks_dir.clone())
                {
                    if !out.contains(&install.root) {
                        out.push(install.root);
                    }
                }
            }
            out
        }
        _ => ncp_host::detect_installs()
            .into_iter()
            .map(|d| d.install.root)
            .collect(),
    }
}

/// Decide the plugin action for one install against the verified channel,
/// without doing any I/O beyond the on-disk folder check.
fn decide_for_install(
    channel: Option<&ncp_manifest::Channel>,
    root: &std::path::Path,
    state: &ncp_host::LauncherState,
) -> ncp_planner::PluginAction {
    let manifest_plugin = channel.and_then(|c| c.plugin.as_ref());
    let folder_present = matches!(
        ncp_host::netcodeplus_status(root),
        ncp_host::NetcodePlusStatus::Installed
    );
    let recorded = state
        .installed_plugins
        .get(&root.to_string_lossy().to_string());
    // Fingerprint the on-disk plugin only when there's a record to validate and
    // the folder is present — so a hand-swapped build is caught (its bytes no
    // longer match what we installed) without hashing files on a fresh install.
    let on_disk_hash = if folder_present && recorded.is_some() {
        ncp_host::plugin_content_hash(root)
    } else {
        None
    };
    ncp_planner::plan_plugin(
        manifest_plugin,
        folder_present,
        recorded,
        on_disk_hash.as_deref(),
    )
}

/// Report the NetcodePlus plugin status for every detected install, by
/// re-verifying the manifest and diffing each install's recorded/​on-disk state
/// against the channel's plugin entry. Read-only (no download, no write).
#[tauri::command]
pub async fn plugin_status(
    app: AppHandle,
    roots: Option<Vec<String>>,
) -> Result<PluginStatusResult, String> {
    let (manifest, state, _, _) = fetch_verify(&app).await?;
    let channel = manifest.channels.get(&state.channel);
    let entry = channel.and_then(|c| c.plugin.as_ref());
    let available_version = entry.map(|e| e.version);
    let notes_url = entry.and_then(|e| e.notes_url.clone());

    let installs: Vec<PluginInstallStatus> = resolve_roots(roots)
        .into_iter()
        .map(|root| {
            let action = decide_for_install(channel, &root, &state);
            let recorded = state
                .installed_plugins
                .get(&root.to_string_lossy().to_string());
            let installed_version = recorded.map(|p| p.version);
            let baselined = recorded.is_some_and(|p| p.content_hash.is_some());
            let present = matches!(
                ncp_host::netcodeplus_status(&root),
                ncp_host::NetcodePlusStatus::Installed
            );
            PluginInstallStatus {
                root: root.to_string_lossy().into_owned(),
                action: plugin_action_str(&action),
                installed_version,
                available_version,
                present,
                baselined,
            }
        })
        .collect();

    let any_update_needed = installs
        .iter()
        .any(|i| matches!(i.action, "install" | "update"));

    Ok(PluginStatusResult {
        plugin_offered: entry.is_some(),
        available_version,
        installs,
        any_update_needed,
        notes_url,
    })
}

/// Result of an [`install_plugin`] run, per install.
#[derive(Debug, Serialize)]
pub struct PluginInstallOutcome {
    /// Install root acted on.
    pub root: String,
    /// `"installed"` | `"skipped"` (already up to date / no plugin / downgrade
    /// blocked) | `"failed"`.
    pub result: &'static str,
    /// Human-readable detail (version installed, why skipped, or the error).
    pub detail: String,
}

/// Install or update the NetcodePlus plugin in **every** detected install that
/// needs it.
///
/// Re-verifies the manifest in Rust (never trusts a prior call), then for each
/// detected install decides via [`ncp_planner::plan_plugin`]. For installs that
/// need it, downloads the plugin ZIP (streaming SHA-256 + size verified against
/// the signed manifest), extracts it with the zip-slip-guarded installer, and
/// records the per-root install state. Up-to-date / downgrade-blocked / no-op
/// installs are skipped. A failure on one install does not abort the others.
///
/// `force` = a user-triggered **Reinstall**: re-extract the manifest's plugin
/// even into installs the launcher already considers up-to-date (or would refuse
/// as a downgrade). This is the repair path for a hand-swapped or corrupted
/// on-disk plugin the version record can't see. Channels that offer no plugin
/// are still skipped (there is nothing to reinstall).
#[tauri::command]
pub async fn install_plugin(
    app: AppHandle,
    roots: Option<Vec<String>>,
    force: bool,
) -> Result<Vec<PluginInstallOutcome>, String> {
    use ncp_planner::PluginAction;

    let (manifest, mut state, manifest_json, manifest_sig) = fetch_verify(&app).await?;
    let channel = manifest.channels.get(&state.channel);
    let Some(entry) = channel.and_then(|c| c.plugin.as_ref()).cloned() else {
        return Err("This channel does not offer a NetcodePlus plugin.".into());
    };

    // Don't swap the plugin out from under a running game: the moved-aside copy
    // (`.NetcodePlus.old.*`) stays file-locked until UT4 exits, so the cleanup
    // can't remove it and the leftover lingers (often admin-owned, in Program
    // Files). Refuse up front — mirrors the pak-install guard.
    if crate::commands::shipping_client_running() {
        return Err("Close UT4 to update the NetcodePlus plugin.".into());
    }

    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    // Stage the download once in a temp dir; the same verified ZIP is reused for
    // every install that needs it (all installs get identical bytes).
    let tmp_dir = std::env::temp_dir();
    let zip_path = tmp_dir.join(format!("ncp-plugin-{}.zip", std::process::id()));

    let mut outcomes: Vec<PluginInstallOutcome> = Vec::new();
    let mut downloaded = false;
    let mut state_dirty = false;
    // Roots whose user-level install hit access-denied (Program Files etc.) —
    // batched into ONE elevated pass after the user-writable installs.
    let mut needs_elevation: Vec<std::path::PathBuf> = Vec::new();

    for root in resolve_roots(roots) {
        let root_key = root.to_string_lossy().to_string();
        let action = decide_for_install(channel, &root, &state);

        match action {
            PluginAction::UpToDate { version } if !force => outcomes.push(PluginInstallOutcome {
                root: root_key,
                result: "skipped",
                detail: format!("already up to date (build {version})"),
            }),
            PluginAction::None => outcomes.push(PluginInstallOutcome {
                root: root_key,
                result: "skipped",
                detail: "channel offers no plugin".into(),
            }),
            PluginAction::DowngradeBlocked { installed, offered } if !force => {
                outcomes.push(PluginInstallOutcome {
                    root: root_key,
                    result: "skipped",
                    detail: format!(
                    "installed build {installed} is newer than offered {offered} — not downgrading"
                ),
                })
            }
            // Install / Update — or a forced Reinstall over an up-to-date or
            // downgrade-blocked install. The body installs `entry` (the manifest's
            // plugin) regardless of which action routed here.
            _ => {
                // Download once, lazily, on the first install that needs it.
                if !downloaded {
                    if let Err(e) = ncp_net::download(
                        &client,
                        &entry.url,
                        entry.sha256,
                        entry.size_bytes,
                        &zip_path,
                    )
                    .await
                    {
                        // A download/verify failure is fatal for the whole run —
                        // no install can proceed without verified bytes.
                        let _ = std::fs::remove_file(&zip_path);
                        return Err(format!("plugin download/verify failed: {e}"));
                    }
                    downloaded = true;
                }
                match ncp_host::install_plugin_zip(&zip_path, &root) {
                    Ok(()) => {
                        state.installed_plugins.insert(
                            root_key.clone(),
                            ncp_planner::InstalledPlugin {
                                version: entry.version,
                                sha256: entry.sha256,
                                // Fingerprint the just-installed bytes so a later
                                // hand-swap to a different build is detectable.
                                content_hash: ncp_host::plugin_content_hash(&root),
                            },
                        );
                        state_dirty = true;
                        outcomes.push(PluginInstallOutcome {
                            root: root_key,
                            result: "installed",
                            detail: format!("build {}", entry.version),
                        });
                    }
                    // Access-denied = a protected location (Program Files). Defer
                    // to one elevated pass rather than failing; any other error is
                    // a genuine failure for this root.
                    Err(e) if is_permission_denied(&e) => needs_elevation.push(root.clone()),
                    Err(e) => outcomes.push(PluginInstallOutcome {
                        root: root_key,
                        result: "failed",
                        detail: e.to_string(),
                    }),
                }
            }
        }
    }

    // Elevated pass: one UAC prompt installs the plugin into every protected
    // root at once. The elevated child re-verifies the ZIP's SHA-256 itself.
    if !needs_elevation.is_empty() {
        // Stage the already-verified manifest + signature as files for the
        // elevated child to re-verify against the trust root itself (it must not
        // trust a caller-supplied hash). Public data; removed right after.
        let tmp = std::env::temp_dir();
        let pid = std::process::id();
        let manifest_path = tmp.join(format!("ncp-elev-{pid}.json"));
        let sig_path = tmp.join(format!("ncp-elev-{pid}.json.minisig"));
        let staged = std::fs::write(&manifest_path, manifest_json.as_bytes())
            .and_then(|()| std::fs::write(&sig_path, manifest_sig.as_bytes()));
        let elevate_result = match staged {
            Ok(()) => elevate_install(
                &zip_path,
                &manifest_path,
                &sig_path,
                &state.channel,
                &needs_elevation,
            ),
            Err(e) => Err(format!(
                "could not stage the manifest for the administrator install: {e}"
            )),
        };
        let _ = std::fs::remove_file(&manifest_path);
        let _ = std::fs::remove_file(&sig_path);
        match elevate_result {
            Ok(()) => {
                for root in &needs_elevation {
                    state.installed_plugins.insert(
                        root.to_string_lossy().to_string(),
                        ncp_planner::InstalledPlugin {
                            version: entry.version,
                            sha256: entry.sha256,
                            content_hash: ncp_host::plugin_content_hash(root),
                        },
                    );
                    state_dirty = true;
                    outcomes.push(PluginInstallOutcome {
                        root: root.to_string_lossy().into_owned(),
                        result: "installed",
                        detail: format!("build {} (administrator)", entry.version),
                    });
                }
            }
            Err(detail) => {
                for root in &needs_elevation {
                    outcomes.push(PluginInstallOutcome {
                        root: root.to_string_lossy().into_owned(),
                        result: "failed",
                        detail: detail.clone(),
                    });
                }
            }
        }
    }

    // Clean up the staged download and persist any recorded installs.
    let _ = std::fs::remove_file(&zip_path);
    if state_dirty {
        let path = state_path(&app)?;
        ncp_host::state::write(&path, &state).map_err(|e| e.to_string())?;
    }

    Ok(outcomes)
}

/// Result of a [`verify_plugin`] run, per install.
#[derive(Debug, Serialize)]
pub struct PluginVerifyOutcome {
    /// Install root acted on.
    pub root: String,
    /// `"current"` (on-disk bytes are the manifest build), `"outdated"` (present
    /// but a different/older build → an update is needed), `"skipped"` (already
    /// verified, or nothing on disk), or `"failed"`. Either way a fingerprint
    /// baseline is recorded so subsequent checks are local (no download).
    pub result: &'static str,
    /// Human-readable detail.
    pub detail: String,
}

/// The build number to record when baselining a present install: the manifest
/// build when the on-disk bytes match it, otherwise any prior recorded build — so
/// a genuinely-newer local build isn't clobbered down (which would defeat
/// [`ncp_planner::PluginAction::DowngradeBlocked`]) and we never assert an
/// unverified build downstream (e.g. the PUG bot's reported client build).
fn baseline_version(matches: bool, manifest_version: u32, prior_version: Option<u32>) -> u32 {
    if matches {
        manifest_version
    } else {
        prior_version.unwrap_or(manifest_version)
    }
}

#[cfg(test)]
mod baseline_version_tests {
    use super::baseline_version;

    #[test]
    fn keeps_a_newer_local_build_and_doesnt_fabricate() {
        // On-disk bytes ARE the manifest build → record it.
        assert_eq!(baseline_version(true, 327, Some(325)), 327);
        assert_eq!(baseline_version(true, 327, None), 327);
        // Mismatch → keep a prior version, incl. a NEWER local build (so
        // DowngradeBlocked survives) and an older one stays honestly old.
        assert_eq!(baseline_version(false, 327, Some(328)), 328);
        assert_eq!(baseline_version(false, 327, Some(325)), 325);
        // Mismatch, no prior (a fresh hand-install) → the manifest build is the
        // only number we have; plan_plugin reports it as an update anyway.
        assert_eq!(baseline_version(false, 327, None), 327);
    }
}

/// Establish a content-fingerprint baseline for a present NetcodePlus install the
/// launcher can't yet vouch for — one it never installed (no record), or one
/// recorded before fingerprints existed (a legacy record). Without this, a build
/// hand-swapped to an OLDER one whose recorded ZIP digest still "matches" the
/// manifest reads as falsely up to date, and a hand-installed CURRENT build reads
/// as falsely "not installed".
///
/// Downloads the manifest-pinned ZIP ONCE (streaming SHA-256 + size verified —
/// the same trust path as install), derives the build's EXPECTED on-disk
/// fingerprint from it ([`ncp_host::plugin_zip_content_hash`]), and records that
/// as each install's baseline `{version, sha256, content_hash = expected}`.
/// [`ncp_planner::plan_plugin`] then reports up to date when the on-disk bytes
/// match the baseline and "update" when they don't — so an outdated swap shows as
/// outdated and a current manual install as current, both on first launch with no
/// reinstall. The installed FILES are never touched, only the launcher's record.
/// Installs already baselined to the current build, or with no folder on disk, are
/// skipped (no download for them).
#[tauri::command]
pub async fn verify_plugin(
    app: AppHandle,
    roots: Option<Vec<String>>,
) -> Result<Vec<PluginVerifyOutcome>, String> {
    let (manifest, mut state, _, _) = fetch_verify(&app).await?;
    let channel = manifest.channels.get(&state.channel);
    let Some(entry) = channel.and_then(|c| c.plugin.as_ref()).cloned() else {
        return Ok(Vec::new()); // channel offers no plugin → nothing to verify
    };

    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    let zip_path = std::env::temp_dir().join(format!("ncp-verify-{}.zip", std::process::id()));

    let mut outcomes: Vec<PluginVerifyOutcome> = Vec::new();
    let mut downloaded = false;
    // The build's expected on-disk fingerprint, derived from the verified ZIP once.
    let mut expected_hash: Option<String> = None;
    let mut state_dirty = false;

    for root in resolve_roots(roots) {
        let root_key = root.to_string_lossy().to_string();

        // Only a present, well-formed folder can be verified; a missing one is a
        // genuine install (don't download just to "verify" nothing).
        if !matches!(
            ncp_host::netcodeplus_status(&root),
            ncp_host::NetcodePlusStatus::Installed
        ) {
            outcomes.push(PluginVerifyOutcome {
                root: root_key,
                result: "skipped",
                detail: "no NetcodePlus folder on disk".into(),
            });
            continue;
        }
        // Already baselined to the current build → nothing to prove. It has a
        // fingerprint AND that build is the manifest's, so plan_plugin tracks any
        // later drift locally from here, no download.
        if state
            .installed_plugins
            .get(&root_key)
            .is_some_and(|rec| rec.sha256 == entry.sha256 && rec.content_hash.is_some())
        {
            outcomes.push(PluginVerifyOutcome {
                root: root_key,
                result: "skipped",
                detail: "already verified".into(),
            });
            continue;
        }

        // Download the verified ZIP lazily, once, and derive the build's EXPECTED
        // on-disk fingerprint from it.
        if !downloaded {
            if let Err(e) = ncp_net::download(
                &client,
                &entry.url,
                entry.sha256,
                entry.size_bytes,
                &zip_path,
            )
            .await
            {
                let _ = std::fs::remove_file(&zip_path);
                return Err(format!("plugin download/verify failed: {e}"));
            }
            downloaded = true;
            expected_hash = ncp_host::plugin_zip_content_hash(&zip_path);
        }

        // We need the build's expected fingerprint to baseline. If the ZIP
        // couldn't be fingerprinted (shouldn't happen for a signed, verified
        // plugin ZIP), DON'T record a content_hash:None baseline — that would
        // read as "up to date" without ever proving content. Skip and leave the
        // install unbaselined so a later run retries.
        let Some(expected) = expected_hash.clone() else {
            outcomes.push(PluginVerifyOutcome {
                root: root_key,
                result: "failed",
                detail: "could not read the plugin ZIP fingerprint".into(),
            });
            continue;
        };

        // Record the build's EXPECTED fingerprint as this install's baseline.
        // plan_plugin reports up to date iff the on-disk bytes match it, so an
        // install hand-swapped to an older build (recorded ZIP digest still
        // "matches" the manifest) now reads as outdated rather than up to date.
        let matches = ncp_host::plugin_matches_zip(&zip_path, &root).unwrap_or(false);
        // Record the manifest build number ONLY when the on-disk bytes are it.
        // Otherwise keep any prior recorded version: don't assert a build for bytes
        // we've proven are NOT it (a fabricated build would be forwarded to the PUG
        // bot), and don't clobber a genuinely-newer local build down to the
        // manifest's (which would silently defeat the DowngradeBlocked guard).
        let prior_version = state.installed_plugins.get(&root_key).map(|r| r.version);
        let version = baseline_version(matches, entry.version, prior_version);
        state.installed_plugins.insert(
            root_key.clone(),
            ncp_planner::InstalledPlugin {
                version,
                sha256: entry.sha256,
                content_hash: Some(expected),
            },
        );
        state_dirty = true;
        outcomes.push(PluginVerifyOutcome {
            root: root_key,
            result: if matches { "current" } else { "outdated" },
            detail: if matches {
                format!("on-disk plugin is build {}", entry.version)
            } else {
                format!(
                    "on-disk plugin is not build {} — update needed",
                    entry.version
                )
            },
        });
    }

    let _ = std::fs::remove_file(&zip_path);
    if state_dirty {
        let path = state_path(&app)?;
        ncp_host::state::write(&path, &state).map_err(|e| e.to_string())?;
    }
    Ok(outcomes)
}

/// Whether a plugin-install error is an OS permission denial (the signal that a
/// root lives in a protected location and needs elevation).
///
/// Checks both the portable `PermissionDenied` kind AND the raw Windows
/// `ERROR_ACCESS_DENIED` (5), because `fs::rename` / `remove_dir_all` on a
/// protected dir don't always map to the portable kind — we must not misclassify
/// a Program Files denial as a hard failure (it would skip elevation).
pub(crate) fn is_permission_denied(e: &ncp_host::PluginInstallError) -> bool {
    const ERROR_ACCESS_DENIED: i32 = 5;
    matches!(
        e,
        ncp_host::PluginInstallError::Io(io)
            if io.kind() == std::io::ErrorKind::PermissionDenied
                || io.raw_os_error() == Some(ERROR_ACCESS_DENIED)
    )
}

/// Run the elevated install helper for `roots` (one UAC prompt). Re-launches
/// this exe with `--elevated-install`, passing the verified ZIP plus the signed
/// manifest + detached signature (as file paths) and the channel. The elevated
/// child re-verifies the manifest against the compiled-in trust root itself and
/// takes the expected hash from the verified manifest, so it trusts none of
/// these arguments for integrity — they only tell it *what* to verify. Returns
/// `Ok(())` only if the child reports every root installed (exit code 0); maps a
/// declined prompt and a non-zero exit to a user-facing error string.
fn elevate_install(
    zip_path: &std::path::Path,
    manifest_path: &std::path::Path,
    sig_path: &std::path::Path,
    channel: &str,
    roots: &[std::path::PathBuf],
) -> std::result::Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate launcher exe: {e}"))?;
    let mut args = vec![
        "--elevated-install".to_string(),
        "--zip".to_string(),
        zip_path.to_string_lossy().into_owned(),
        "--manifest".to_string(),
        manifest_path.to_string_lossy().into_owned(),
        "--sig".to_string(),
        sig_path.to_string_lossy().into_owned(),
        "--channel".to_string(),
        channel.to_string(),
    ];
    for root in roots {
        args.push("--root".to_string());
        args.push(root.to_string_lossy().into_owned());
    }
    match ncp_host::run_elevated(&exe, &args) {
        Ok(0) => Ok(()),
        Ok(n) => Err(format!(
            "the administrator install reported {n} install(s) failed — \
             close Unreal Tournament and any File Explorer window showing the \
             plugin folder, then try again"
        )),
        Err(ncp_host::ElevateError::Cancelled) => Err(
            "you declined the administrator prompt, so the protected install \
                 was not updated"
                .to_string(),
        ),
        Err(e) => Err(e.to_string()),
    }
}

// ===================================================================
// Stray NetcodePlus detection — misplaced copies that double-load or
// fail to load. Filesystem-only; no manifest/network involved.
// ===================================================================

/// One install's stray findings, for the UI.
#[derive(Debug, Serialize)]
pub struct InstallStrays {
    /// Install root scanned.
    pub root: String,
    /// Strays found in this install (empty when clean).
    pub strays: Vec<StrayReport>,
}

/// A stray copy, summarised for the UI (the kind, a plain-English reason, and
/// the path — echoed back to `remove_stray_plugin` unchanged).
#[derive(Debug, Serialize)]
pub struct StrayReport {
    /// `"engine_plugins"` | `"nested_too_deep"` | `"loose_in_plugins_root"`.
    pub kind: String,
    /// Non-technical explanation to show the user.
    pub explanation: String,
    /// Path that would be removed (round-tripped to remove_stray_plugin).
    pub path: String,
}

fn stray_kind_str(k: ncp_host::StrayKind) -> &'static str {
    match k {
        ncp_host::StrayKind::EnginePlugins => "engine_plugins",
        ncp_host::StrayKind::NestedTooDeep => "nested_too_deep",
        ncp_host::StrayKind::LooseInPluginsRoot => "loose_in_plugins_root",
        ncp_host::StrayKind::ContentPak => "content_pak",
        ncp_host::StrayKind::PluginLeftover => "plugin_leftover",
    }
}

fn stray_kind_from_str(s: &str) -> Option<ncp_host::StrayKind> {
    match s {
        "engine_plugins" => Some(ncp_host::StrayKind::EnginePlugins),
        "nested_too_deep" => Some(ncp_host::StrayKind::NestedTooDeep),
        "loose_in_plugins_root" => Some(ncp_host::StrayKind::LooseInPluginsRoot),
        "content_pak" => Some(ncp_host::StrayKind::ContentPak),
        "plugin_leftover" => Some(ncp_host::StrayKind::PluginLeftover),
        _ => None,
    }
}

/// Scan every detected install for stray / misplaced NetcodePlus copies.
/// Read-only. Returns only installs that actually have strays.
#[tauri::command]
pub fn scan_strays() -> Vec<InstallStrays> {
    ncp_host::detect_installs()
        .into_iter()
        .filter_map(|d| {
            let strays: Vec<StrayReport> = ncp_host::scan_strays(&d.install.root)
                .into_iter()
                .map(|s| StrayReport {
                    kind: stray_kind_str(s.kind).to_string(),
                    explanation: s.kind.explanation().to_string(),
                    path: s.path.to_string_lossy().into_owned(),
                })
                .collect();
            (!strays.is_empty()).then(|| InstallStrays {
                root: d.install.root.to_string_lossy().into_owned(),
                strays,
            })
        })
        .collect()
}

/// Remove one stray copy after the user confirmed it in the UI.
///
/// `root` + `kind` identify the stray; `ncp_host::remove_stray` recomputes the
/// expected stray location(s) under `root` for that kind — a fixed path for the
/// three plugin kinds, or a fresh re-scan of `Content/Paks` (`content_pak`) /
/// `Plugins/` (`plugin_leftover`) for the variable kinds — and refuses anything
/// else, so the webview cannot direct a delete to an arbitrary location. The
/// `path` from the scan is passed through for the safety re-check.
#[tauri::command]
pub fn remove_stray_plugin(root: String, kind: String, path: String) -> Result<(), String> {
    let kind = stray_kind_from_str(&kind).ok_or_else(|| "unknown stray kind".to_string())?;
    // A running game can hold these files locked (a mounted content pak, or a
    // leftover/loaded plugin DLL), so the delete would fail with a cryptic OS
    // sharing-violation. Refuse up front with a plain-English message — mirrors the
    // pak/plugin install guards so the whole delete surface behaves consistently.
    if crate::commands::shipping_client_running() {
        return Err("Close Unreal Tournament, then try again.".into());
    }
    let stray = ncp_host::StrayPlugin {
        kind,
        path: std::path::PathBuf::from(path),
    };
    ncp_host::remove_stray(std::path::Path::new(&root), &stray).map_err(|e| e.to_string())
}

// ===================================================================
// Launcher self-update. Compares the platform's advertised launcher
// version (Windows: `launcher`, the exe; Linux: `launcher_linux`, the
// AppImage) to this build. When the entry carries the signed SHA-256 +
// size, the launcher downloads, verifies, and relaunches itself
// (sibling exe on Windows, in-place AppImage swap on Linux); otherwise
// it is NOTIFY-ONLY — the UI hands the user a download URL and they
// fetch + run the new launcher themselves, the same trust path as
// their first install.
// ===================================================================

/// Whether a newer launcher is available, summarised for the UI banner.
///
/// Notify-only: `url` is a release page the user opens in their browser (via
/// the https-gated opener command), never an auto-applied download. The silent
/// state (`update_available: false`, `url: None`) covers both "the manifest
/// advertises no launcher" and "the advertised version is not newer than this
/// build".
#[derive(Debug, Serialize)]
pub struct LauncherUpdateResult {
    /// `true` when the manifest advertises a strictly newer launcher version.
    pub update_available: bool,
    /// This build's own version (`CARGO_PKG_VERSION`).
    pub current_version: String,
    /// The version the manifest advertises, if it advertises a launcher at all.
    pub available_version: Option<String>,
    /// HTTPS URL to download the new launcher — set only when `update_available`
    /// is true. The direct exe asset when `can_auto_update`, else a release page.
    pub url: Option<String>,
    /// `true` when the advertised launcher entry carries a SHA-256 + size, so the
    /// launcher can download + verify + relaunch the new exe itself
    /// ([`download_and_apply_launcher_update`]). `false` ⇒ notify-only (the user
    /// fetches and runs it from `url` themselves) — the back-compat path for
    /// manifests authored before the hash-pin hardening.
    pub can_auto_update: bool,
}

/// Check whether the signed manifest advertises a newer launcher than this
/// build (notify-only self-update).
///
/// Re-verifies the manifest in Rust via [`fetch_verify`] — the same trust path
/// as every other command, so nothing trusts a round-tripped value — then
/// compares the top-level [`ncp_manifest::LauncherEntry::version`] to this
/// build's `CARGO_PKG_VERSION` with semver, surfacing the download URL only
/// when the manifest's version is strictly greater.
///
/// This is meant to fire the banner *before* the manifest's
/// `min_launcher_version` hard-gate (enforced inside `load_and_verify`) would
/// reject an outdated launcher with `LauncherTooOld` — so a user sees "update
/// available" rather than a wall, as long as the launcher is still new enough
/// to parse and verify the manifest at all.
#[tauri::command]
pub async fn launcher_update_status(app: AppHandle) -> Result<LauncherUpdateResult, String> {
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|e| format!("launcher version is not valid semver: {e}"))?;
    let (manifest, _state, _, _) = fetch_verify(&app).await?;

    // Each platform reads only its OWN entry: `launcher` is the Windows exe,
    // `launcher_linux` the AppImage. Selecting here means a Linux build is
    // never offered a Windows exe (or vice versa) — a manifest with only the
    // Windows entry is simply silent on Linux.
    #[cfg(windows)]
    let advertised = manifest.launcher;
    #[cfg(not(windows))]
    let advertised = manifest.launcher_linux;

    Ok(match advertised {
        Some(entry) if entry.version > current => {
            // Verified self-update is possible only when the entry carries BOTH
            // the digest and the size — the integrity material the download path
            // enforces. Either missing ⇒ notify-only.
            let can_auto_update = entry.sha256.is_some() && entry.size_bytes.is_some();
            // It also requires a writable install dir — the in-place swap can't
            // rename+replace the binary otherwise. Matches the backend gate in
            // the apply path (which errors on an unwritable dir), so the button
            // is hidden (notify-only) instead of offered-then-failing.
            // - Linux: the target is the AppImage; a deb install, dev run, or an
            //   AppImage in a root-owned spot like /opt stays notify-only.
            // - Windows: the target is the exe; a relocated install under
            //   Program Files stays notify-only.
            #[cfg(not(windows))]
            let can_auto_update = can_auto_update
                && running_appimage()
                    .as_deref()
                    .and_then(std::path::Path::parent)
                    .is_some_and(ncp_host::dir_writable);
            #[cfg(windows)]
            let can_auto_update = can_auto_update
                && std::env::current_exe()
                    .ok()
                    .as_deref()
                    .and_then(std::path::Path::parent)
                    .is_some_and(ncp_host::dir_writable);
            LauncherUpdateResult {
                update_available: true,
                current_version: current.to_string(),
                available_version: Some(entry.version.to_string()),
                url: Some(entry.url),
                can_auto_update,
            }
        }
        Some(entry) => LauncherUpdateResult {
            update_available: false,
            current_version: current.to_string(),
            available_version: Some(entry.version.to_string()),
            url: None,
            can_auto_update: false,
        },
        None => LauncherUpdateResult {
            update_available: false,
            current_version: current.to_string(),
            available_version: None,
            url: None,
            can_auto_update: false,
        },
    })
}

/// `launcher-download-progress` event payload. `phase` is `"download"` then
/// `"verify"` — mirrors the game-installer download so the UI renders the same
/// kind of progress bar.
#[derive(Clone, Serialize)]
struct LauncherDownloadProgress {
    phase: &'static str,
    done: u64,
    total: u64,
}

fn emit_progress(app: &AppHandle, phase: &'static str, done: u64, total: u64) {
    use tauri::Emitter;
    let _ = app.emit(
        "launcher-download-progress",
        LauncherDownloadProgress { phase, done, total },
    );
}

/// (Linux) The on-disk AppImage this process is running from, when it is one —
/// the only valid in-place self-update target. Comes from the `APPIMAGE` env
/// var the AppImage runtime sets (`current_exe()` would resolve into the
/// transient FUSE mount instead). `None` on a deb install or a dev run.
#[cfg(not(windows))]
fn running_appimage() -> Option<std::path::PathBuf> {
    let env = std::env::var("APPIMAGE").ok();
    let p = ncp_host::linux::appimage_path(env.as_deref())?;
    p.is_file().then_some(p)
}

/// Stream `url` to `<dest>.part` (size-enforced, progress on the
/// `launcher-download-progress` channel), hash the result against the SIGNED
/// digest, and commit the verified bytes to `dest`. Shared by both platforms'
/// in-place swap apply paths (`dest` is `<binary>.update`). On any failure the
/// partial/unverified file is removed and nothing is run.
async fn download_verified_launcher(
    app: &AppHandle,
    url: &str,
    expected_size: u64,
    expected_sha: ncp_manifest::Sha256Digest,
    dest: &std::path::Path,
) -> Result<(), String> {
    let part = std::path::PathBuf::from(format!("{}.part", dest.to_string_lossy()));
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;

    // Download — size-enforced, progress-reported. Not cancellable, and any
    // failure discards the partial rather than resuming it later: a launcher
    // binary is a few MB, so re-downloading beats managing stale partials.
    let app_dl = app.clone();
    static NEVER_CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    ncp_net::download_resumable(&client, url, expected_size, &part, &NEVER_CANCEL, {
        move |done, total| emit_progress(&app_dl, "download", done, total)
    })
    .await
    .map_err(|e| {
        let _ = std::fs::remove_file(&part);
        format!("couldn't download the new launcher: {e}")
    })?;

    // Final verification against the SIGNED digest. A mismatch ⇒ a swapped or
    // corrupted binary; discard it and refuse to run anything.
    let app_v = app.clone();
    let digest = ncp_net::hash_file(&part, move |done, total| {
        emit_progress(&app_v, "verify", done, total)
    })
    .await
    .map_err(|e| e.to_string())?;
    if digest != expected_sha {
        let _ = std::fs::remove_file(&part);
        return Err(
            "the downloaded launcher failed verification (its contents don't match the signed manifest) — it was discarded, nothing was run"
                .into(),
        );
    }

    // Verified — commit the `.part` to its final name.
    std::fs::rename(&part, dest).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        format!("couldn't finalize the downloaded launcher: {e}")
    })
}

/// Download the manifest's advertised launcher exe, verify it against the
/// signed SHA-256 + size, then relaunch into the verified copy.
///
/// This is the hash-pinned counterpart to [`launcher_update_status`]'s
/// notify-only path. The whole point is that the digest lives **inside the
/// YubiKey-signed manifest**, so verifying the download against it makes the
/// signature transitively protect the binary — a swapped or tampered exe fails
/// the SHA-256 check and is discarded, never run.
///
/// Flow:
/// 1. Re-verify the manifest in Rust (never trust a round-tripped value), and
///    require an advertised launcher that is strictly newer than this build and
///    carries both `sha256` and `size_bytes`.
/// 2. Stream `url` to `<binary>.update.part` with size enforcement + progress
///    events, then hash it in a final pass and reject anything whose digest ≠
///    the signed one. Commit the verified bytes to `<binary>.update`.
/// 3. Swap it **in place** ([`ncp_host::apply_binary_swap`]): rename the running
///    binary aside to `<binary>.old`, rename `<binary>.update` onto the original
///    path. Both platforms do this — Windows allows renaming a running image
///    (the lock blocks overwriting its bytes, not moving the directory entry),
///    and an AppImage's FUSE mount holds the open inode across the rename. The
///    binary keeps its own on-disk path, so a shortcut / taskbar pin /
///    `.desktop` entry never goes stale and no duplicate icon is ever created.
/// 4. Spawn the swapped-in binary with `--post-update-wait <our-pid>` (so it
///    waits for us to release the single-instance lock) and exit. The next
///    start sweeps the `.old` rollback file.
///
/// Notify-only fallbacks (the button isn't offered, re-checked here for a stale
/// frontend): an AppImage in a non-writable dir like `/opt`, or a launcher not
/// running as an AppImage on Linux (a deb install).
#[tauri::command]
pub async fn download_and_apply_launcher_update(app: AppHandle) -> Result<(), String> {
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|e| format!("launcher version is not valid semver: {e}"))?;
    let (manifest, _state, _, _) = fetch_verify(&app).await?;
    // Same per-platform entry selection as `launcher_update_status`.
    #[cfg(windows)]
    let advertised = manifest.launcher;
    #[cfg(not(windows))]
    let advertised = manifest.launcher_linux;
    let entry = advertised.ok_or("the update manifest advertises no launcher for this platform")?;
    if entry.version <= current {
        return Err("this launcher is already up to date".into());
    }
    // Verified self-update requires BOTH integrity fields. Absence is the
    // notify-only path (the UI never offers this button there), but re-check in
    // Rust so a stale frontend can't drive an unverified download.
    let (Some(expected_sha), Some(expected_size)) = (entry.sha256, entry.size_bytes) else {
        return Err(
            "this update can't be applied automatically (the manifest has no verified download) — use the download link instead"
                .into(),
        );
    };

    // The self-update target: the running binary's own on-disk path. Windows
    // can rename a running image (the lock blocks overwriting its bytes, not
    // moving the directory entry), so BOTH platforms swap in place — the path
    // never changes, so a shortcut / pin / .desktop entry never goes stale.
    #[cfg(windows)]
    let target = std::env::current_exe().map_err(|e| e.to_string())?;
    #[cfg(not(windows))]
    let target = running_appimage().ok_or(
        "this launcher isn't running as an AppImage — download the new version manually instead",
    )?;

    // The install dir must be writable to stage + swap. On Windows this is
    // usually true (the launcher installs to a user folder); an AppImage in a
    // root-owned dir like /opt is notify-only (the UI never offers the button
    // there, but re-check for a stale frontend).
    if !target.parent().is_some_and(ncp_host::dir_writable) {
        return Err(
            "the folder holding this launcher isn't writable — download the new version manually instead"
                .into(),
        );
    }
    let dir = target
        .parent()
        .ok_or("can't locate the launcher's folder")?
        .to_path_buf();
    let (update_path, _old_path) = ncp_host::swap_paths(&target);

    download_verified_launcher(&app, &entry.url, expected_size, expected_sha, &update_path).await?;

    // The in-place swap (optional exec bit → current aside to `.old` → verified
    // file onto the original path, rollback + staging cleanup on failure) lives
    // in ncp_host so both CI runners exercise it for real. `executable` only
    // applies on unix (the AppImage); it's a no-op on Windows.
    #[cfg(windows)]
    let executable = false;
    #[cfg(not(windows))]
    let executable = true;
    ncp_host::apply_binary_swap(&target, &update_path, executable).map_err(|e| e.to_string())?;

    // Hand off: start the swapped-in binary (same path as before), telling it to
    // wait for THIS process to release the single-instance lock, then exit. A
    // failed spawn is surfaced, not exited on — the swap already happened, so
    // the user just starts the (updated) launcher themselves; the old build sits
    // at `.old` until the next start's sweep tidies it.
    let pid = std::process::id();
    std::process::Command::new(&target)
        .arg("--post-update-wait")
        .arg(pid.to_string())
        .current_dir(&dir)
        .spawn()
        .map_err(|e| {
            format!("the update was applied, but the new launcher couldn't be started automatically — start it yourself: {e}")
        })?;

    app.exit(0);
    Ok(())
}
