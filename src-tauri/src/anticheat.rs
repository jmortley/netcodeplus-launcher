//! UT4AC opt-in distribution commands (docs/ANTICHEAT-OPTIN-DESIGN.md).
//!
//! The whole feature is dormant until a signed manifest carries an `anticheat`
//! block (Phase 3 activation) — `anticheat_status` reports `not_offered` and
//! the Add-ons card stays hidden. Consent is the launcher-state record
//! ([`ncp_host::AnticheatConsent`]): absent record = the launcher never
//! downloads a byte of the module; a `consent_rev` bump in the manifest pauses
//! updates and returns the card to review state. Install/uninstall reuse the
//! NetcodePlus plugin-install machinery generalized by destination
//! (`ncp_host::install_ut4ac_zip` / `remove_ut4ac`) and refuse while
//! `UE4-Win64-Shipping.exe` runs, like every sibling that touches game files.

use std::path::Path;

use serde::Serialize;
use tauri::AppHandle;

use crate::commands::state_path;
use crate::updates::fetch_verify;

/// The single supported module id in [`ncp_manifest::Manifest::anticheat`].
const AC_ID: &str = "ut4ac";

/// Everything the Add-ons card needs to render its four states.
#[derive(Debug, Serialize)]
pub struct AnticheatStatus {
    /// `false` until the live manifest carries the `anticheat.ut4ac` block —
    /// the card renders nothing and the feature does not exist.
    pub offered: bool,
    /// One of: `not_offered`, `not_installed`, `installed_current`,
    /// `update_available`, `review_required`, `waiting_for_plugin`.
    pub state: &'static str,
    /// Manifest-side module facts (None/0 when not offered).
    pub version: Option<String>,
    pub size_bytes: u64,
    pub consent_rev: u32,
    pub min_plugin_version: u32,
    pub disclosure_note: Option<String>,
    pub notes_url: Option<String>,
    /// Local facts.
    pub installed_version: Option<String>,
    pub consented_rev: Option<u32>,
    pub plugin_build: Option<u32>,
}

fn not_offered() -> AnticheatStatus {
    AnticheatStatus {
        offered: false,
        state: "not_offered",
        version: None,
        size_bytes: 0,
        consent_rev: 0,
        min_plugin_version: 0,
        disclosure_note: None,
        notes_url: None,
        installed_version: None,
        consented_rev: None,
        plugin_build: None,
    }
}

/// The installed NetcodePlus build recorded for `root`, for the one-way
/// `min_plugin_version` gate (resolved TBD-3).
fn plugin_build_for(state: &ncp_host::LauncherState, root: &str) -> Option<u32> {
    state.installed_plugins.get(root).map(|p| p.version)
}

/// Report the UT4AC offer/consent/install state for the Add-ons card.
///
/// # Errors
/// Manifest fetch/verify failures (network, signature); local state errors.
#[tauri::command]
pub async fn anticheat_status(
    app: AppHandle,
    root: Option<String>,
) -> Result<AnticheatStatus, String> {
    let (manifest, state, _, _) = fetch_verify(&app).await?;
    let Some(entry) = manifest.anticheat.get(AC_ID) else {
        return Ok(not_offered());
    };

    let dir_present = root
        .as_deref()
        .is_some_and(|r| ncp_host::ut4ac_dir(Path::new(r)).is_dir());
    let plugin_build = root.as_deref().and_then(|r| plugin_build_for(&state, r));
    let plugin_ok = plugin_build.is_some_and(|b| b >= entry.min_plugin_version);
    let consent = state.anticheat_consent.as_ref();

    let st = match consent {
        // Never consented — the launcher has never downloaded a byte.
        None => "not_installed",
        // Monitoring scope moved past the recorded consent: pause updates,
        // re-ask. The installed (old) version keeps working meanwhile.
        Some(c) if c.consent_rev < entry.consent_rev => "review_required",
        // Consent current but bytes stale (new build) or folder hand-deleted —
        // both are a plain update/reinstall under the SAME consent, gated on
        // the plugin being new enough for this module version.
        Some(c) if !dir_present || c.installed_sha256 != entry.sha256.to_string() => {
            if plugin_ok {
                "update_available"
            } else {
                "waiting_for_plugin"
            }
        }
        Some(_) => "installed_current",
    };

    Ok(AnticheatStatus {
        offered: true,
        state: st,
        version: Some(entry.version.clone()),
        size_bytes: entry.size_bytes,
        consent_rev: entry.consent_rev,
        min_plugin_version: entry.min_plugin_version,
        disclosure_note: entry.disclosure_note.clone(),
        notes_url: entry.notes_url.clone(),
        installed_version: consent.map(|c| c.installed_version.clone()),
        consented_rev: consent.map(|c| c.consent_rev),
        plugin_build,
    })
}

/// Install (or update/reinstall) the UT4AC module. **The click behind the
/// disclosure is the consent act** — `consent_rev` is the revision the UI just
/// showed, and it must equal the live manifest's revision: if the manifest
/// moved between render and click, the player agreed to stale text and this
/// refuses rather than record a consent they never saw.
///
/// # Errors
/// Game running; module not offered; consent-rev race; plugin too old
/// (`min_plugin_version`); download/verify/install failures.
#[tauri::command]
pub async fn anticheat_install(
    app: AppHandle,
    root: String,
    consent_rev: u32,
) -> Result<AnticheatStatus, String> {
    if crate::commands::shipping_client_running() {
        return Err("Close Unreal Tournament, then try again.".into());
    }
    let (manifest, state, _, _) = fetch_verify(&app).await?;
    let Some(entry) = manifest.anticheat.get(AC_ID).cloned() else {
        return Err("UT4AC is not currently offered by the update manifest.".into());
    };
    if consent_rev != entry.consent_rev {
        return Err(
            "The UT4AC disclosure changed while this page was open — review the updated one and try again."
                .into(),
        );
    }
    let plugin_ok = plugin_build_for(&state, &root).is_some_and(|b| b >= entry.min_plugin_version);
    if !plugin_ok {
        return Err(format!(
            "This UT4AC version needs NetcodePlus build {} or newer — update the plugin first.",
            entry.min_plugin_version
        ));
    }

    // Download → sha-verify → atomic install, the standard chain.
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    let zip_path = std::env::temp_dir().join(format!("ncp-ut4ac-{}.zip", std::process::id()));
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
        return Err(format!("UT4AC download/verify failed: {e}"));
    }
    let install_result = ncp_host::install_ut4ac_zip(&zip_path, Path::new(&root));
    let _ = std::fs::remove_file(&zip_path);
    install_result.map_err(|e| e.to_string())?;

    // Record consent AFTER a successful install, against a FRESH state read so
    // a concurrent write during the download isn't clobbered (the install_paks
    // lost-update lesson).
    let path = state_path(&app)?;
    let mut fresh = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    fresh.anticheat_consent = Some(ncp_host::AnticheatConsent {
        granted_at: chrono::Utc::now().to_rfc3339(),
        consent_rev: entry.consent_rev,
        installed_version: entry.version.clone(),
        installed_sha256: entry.sha256.to_string(),
    });
    ncp_host::state::write(&path, &fresh).map_err(|e| e.to_string())?;

    anticheat_status(app, Some(root)).await
}

/// Uninstall UT4AC: delete `Plugins/UT4AC/` and clear the consent record —
/// one click, total, per the opt-in contract. Idempotent.
///
/// # Errors
/// Game running (locked files), or a filesystem error mid-delete.
#[tauri::command]
pub async fn anticheat_uninstall(app: AppHandle, root: String) -> Result<AnticheatStatus, String> {
    if crate::commands::shipping_client_running() {
        return Err("Close Unreal Tournament, then try again.".into());
    }
    ncp_host::remove_ut4ac(Path::new(&root)).map_err(|e| e.to_string())?;

    let path = state_path(&app)?;
    let mut fresh = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    fresh.anticheat_consent = None;
    ncp_host::state::write(&path, &fresh).map_err(|e| e.to_string())?;

    anticheat_status(app, Some(root)).await
}
