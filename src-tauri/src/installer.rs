//! Verified download of the full UT4 game installer, for users who don't have
//! the game yet.
//!
//! The installer (~10 GB) is hosted by a third party (UT4Ever) the launcher
//! doesn't control, so trust works exactly like a pak: the signed manifest
//! carries the expected SHA-256, and a download whose bytes don't match is
//! rejected. Because the file is huge, the transfer **resumes** on interruption,
//! reports **progress** (emitted as `game-download-progress` events), can be
//! **cancelled**, and is **verified** in a final hash pass before the `.part` is
//! renamed into place. The launcher hands the user the verified zip; it never
//! runs it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::updates::fetch_verify;

/// Set by [`cancel_game_download`], polled by the download loop. One installer
/// download runs at a time, so a single flag suffices.
static CANCEL: AtomicBool = AtomicBool::new(false);

/// What the UI needs to offer the download (or hide it).
#[derive(Debug, Serialize)]
pub struct GameInstallerInfo {
    /// Whether the signed manifest advertises a game installer at all.
    pub available: bool,
    /// Display version (e.g. `"1.1.0"`); empty when none.
    pub version: String,
    /// Declared size in bytes (0 when none).
    pub size_bytes: u64,
}

/// `game-download-progress` event payload. `phase` is `"download"` then
/// `"verify"`.
#[derive(Clone, Serialize)]
struct Progress {
    phase: &'static str,
    done: u64,
    total: u64,
}

fn emit(app: &AppHandle, phase: &'static str, done: u64, total: u64) {
    let _ = app.emit("game-download-progress", Progress { phase, done, total });
}

/// Sanitise a manifest-supplied version into a filename-safe fragment.
fn safe_version(v: &str) -> String {
    v.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Whether the signed manifest advertises a game installer, and its
/// version/size (for the "Download UT4" panel). Verifies the manifest like every
/// other check.
#[tauri::command]
pub async fn game_installer_info(app: AppHandle) -> Result<GameInstallerInfo, String> {
    let (manifest, ..) = fetch_verify(&app).await?;
    Ok(match manifest.game_installer {
        Some(gi) => GameInstallerInfo {
            available: true,
            version: gi.version,
            size_bytes: gi.size_bytes,
        },
        None => GameInstallerInfo {
            available: false,
            version: String::new(),
            size_bytes: 0,
        },
    })
}

/// Cancel an in-progress game-installer download. The partial file is kept so a
/// later download resumes from it.
#[tauri::command]
pub fn cancel_game_download() {
    CANCEL.store(true, Ordering::Relaxed);
}

/// Download the UT4 installer into `dir`, verifying it against the signed
/// manifest's SHA-256. Resumable, cancellable, and progress-reported via
/// `game-download-progress` events. Returns the path of the verified `.zip`.
///
/// The user picks `dir` (to choose a drive with room); the launcher checks free
/// space first, downloads to `<dir>/UT4-Installer-<ver>.zip.part` (resuming any
/// existing partial), verifies the full digest, then renames to `.zip`. It does
/// not run the installer.
#[tauri::command]
pub async fn download_game_installer(app: AppHandle, dir: String) -> Result<String, String> {
    let (manifest, ..) = fetch_verify(&app).await?;
    let installer = manifest
        .game_installer
        .ok_or("the update manifest has no game installer")?;

    let dir = PathBuf::from(&dir);
    if !dir.is_dir() {
        return Err("pick an existing folder to download into".into());
    }
    if !ncp_host::dir_writable(&dir) {
        return Err(
            "can't write to that folder — pick one you own (e.g. your Downloads), not a drive root or a protected system folder"
                .into(),
        );
    }
    let final_path = dir.join(format!(
        "UT4-Installer-{}.zip",
        safe_version(&installer.version)
    ));
    let part = PathBuf::from(format!("{}.part", final_path.to_string_lossy()));

    // Free-space guard: need ~the file's size (the `.part` becomes the `.zip` via
    // rename, so no extra copy). Only block when the figure is actually readable.
    if let Some(free) = ncp_host::disk::available_space(&dir) {
        let needed = installer.size_bytes.saturating_add(512 * 1024 * 1024); // +0.5 GB
        if free < needed {
            return Err(format!(
                "not enough free space on that drive — need ~{:.1} GB, have {:.1} GB",
                installer.size_bytes as f64 / 1e9,
                free as f64 / 1e9,
            ));
        }
    }

    CANCEL.store(false, Ordering::Relaxed);
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;

    // Download — resumable, cancellable, progress-reported.
    let app_dl = app.clone();
    ncp_net::download_resumable(
        &client,
        &installer.url,
        installer.size_bytes,
        &part,
        &CANCEL,
        move |done, total| emit(&app_dl, "download", done, total),
    )
    .await
    .map_err(|e| match e {
        ncp_net::NetError::Cancelled => "cancelled".to_string(),
        ncp_net::NetError::Io(io)
            if io.raw_os_error() == Some(5) || io.kind() == std::io::ErrorKind::PermissionDenied =>
        {
            "couldn't write the download to that folder — it may be write-protected or blocked by antivirus. Try a different folder.".to_string()
        }
        other => other.to_string(),
    })?;

    // Final verification pass against the signed digest.
    let app_v = app.clone();
    let digest = ncp_net::hash_file(&part, move |done, total| {
        emit(&app_v, "verify", done, total)
    })
    .await
    .map_err(|e| e.to_string())?;
    if digest != installer.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(
            "the download failed verification (its contents don't match the signed manifest) — it was discarded"
                .into(),
        );
    }

    // Verified — commit the `.part` to the final name.
    std::fs::rename(&part, &final_path).map_err(|e| e.to_string())?;
    Ok(final_path.to_string_lossy().into_owned())
}

/// Reveal a downloaded file by opening its containing folder (the user runs the
/// installer themselves). Opens a real directory only — never a file, no exec
/// surface.
#[tauri::command]
pub fn reveal_path(app: AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let p = Path::new(&path);
    let parent = p
        .parent()
        .filter(|d| d.is_dir())
        .ok_or("no folder to open")?;
    app.opener()
        .open_path(parent.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}
