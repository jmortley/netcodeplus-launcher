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

use crate::commands::state_path;
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
async fn fetch_verify(
    app: &AppHandle,
) -> Result<(ncp_manifest::Manifest, ncp_host::LauncherState), String> {
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

    Ok((manifest, state))
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
    let (manifest, state) = fetch_verify(&app).await?;
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
    let (manifest, state) = fetch_verify(&app).await?;
    let channel = state.channel.clone();

    let plan = ncp_planner::plan(&manifest, &channel, &state.local_install, &state.opted_out)
        .map_err(|e| e.to_string())?;

    let to_download = plan
        .to_download
        .iter()
        .map(|a| PlanDownload {
            id: a.id.clone(),
            filename: a.manifest_pak.pak_filename.clone(),
            version: a.manifest_pak.version.to_string(),
            size_bytes: a.manifest_pak.size_bytes,
            reason: match a.reason {
                ncp_planner::DownloadReason::Missing => "missing",
                ncp_planner::DownloadReason::HashMismatch => "hash_mismatch",
            },
            required: a.manifest_pak.required,
        })
        .collect();

    let to_remove = plan
        .to_remove
        .iter()
        .map(|a| PlanRemove {
            id: a.id.clone(),
            filename: a.local_pak.pak_filename.clone(),
            reason: match a.reason {
                ncp_planner::RemoveReason::NotInChannel => "not_in_channel",
                ncp_planner::RemoveReason::OptedOut => "opted_out",
            },
        })
        .collect();

    Ok(PlanResult {
        channel,
        up_to_date: plan.is_no_op(),
        to_download,
        to_remove,
        keep_count: plan.to_keep.len(),
        total_download_bytes: plan.total_download_bytes(),
    })
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
    ncp_planner::plan_plugin(manifest_plugin, folder_present, recorded)
}

/// Report the NetcodePlus plugin status for every detected install, by
/// re-verifying the manifest and diffing each install's recorded/​on-disk state
/// against the channel's plugin entry. Read-only (no download, no write).
#[tauri::command]
pub async fn plugin_status(app: AppHandle) -> Result<PluginStatusResult, String> {
    let (manifest, state) = fetch_verify(&app).await?;
    let channel = manifest.channels.get(&state.channel);
    let entry = channel.and_then(|c| c.plugin.as_ref());
    let available_version = entry.map(|e| e.version);

    let installs: Vec<PluginInstallStatus> = ncp_host::detect_installs()
        .into_iter()
        .map(|d| {
            let root = d.install.root;
            let action = decide_for_install(channel, &root, &state);
            let installed_version = state
                .installed_plugins
                .get(&root.to_string_lossy().to_string())
                .map(|p| p.version);
            PluginInstallStatus {
                root: root.to_string_lossy().into_owned(),
                action: plugin_action_str(&action),
                installed_version,
                available_version,
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
#[tauri::command]
pub async fn install_plugin(app: AppHandle) -> Result<Vec<PluginInstallOutcome>, String> {
    use ncp_planner::PluginAction;

    let (manifest, mut state) = fetch_verify(&app).await?;
    let channel = manifest.channels.get(&state.channel);
    let Some(entry) = channel.and_then(|c| c.plugin.as_ref()).cloned() else {
        return Err("This channel does not offer a NetcodePlus plugin.".into());
    };

    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    // Stage the download once in a temp dir; the same verified ZIP is reused for
    // every install that needs it (all installs get identical bytes).
    let tmp_dir = std::env::temp_dir();
    let zip_path = tmp_dir.join(format!("ncp-plugin-{}.zip", std::process::id()));

    let mut outcomes: Vec<PluginInstallOutcome> = Vec::new();
    let mut downloaded = false;
    let mut state_dirty = false;

    for d in ncp_host::detect_installs() {
        let root = d.install.root;
        let root_key = root.to_string_lossy().to_string();
        let action = decide_for_install(channel, &root, &state);

        match action {
            PluginAction::UpToDate { version } => outcomes.push(PluginInstallOutcome {
                root: root_key,
                result: "skipped",
                detail: format!("already up to date (build {version})"),
            }),
            PluginAction::None => outcomes.push(PluginInstallOutcome {
                root: root_key,
                result: "skipped",
                detail: "channel offers no plugin".into(),
            }),
            PluginAction::DowngradeBlocked { installed, offered } => {
                outcomes.push(PluginInstallOutcome {
                    root: root_key,
                    result: "skipped",
                    detail: format!(
                    "installed build {installed} is newer than offered {offered} — not downgrading"
                ),
                })
            }
            PluginAction::Install { .. } | PluginAction::Update { .. } => {
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
                            },
                        );
                        state_dirty = true;
                        outcomes.push(PluginInstallOutcome {
                            root: root_key,
                            result: "installed",
                            detail: format!("build {}", entry.version),
                        });
                    }
                    Err(e) => outcomes.push(PluginInstallOutcome {
                        root: root_key,
                        result: "failed",
                        detail: e.to_string(),
                    }),
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
