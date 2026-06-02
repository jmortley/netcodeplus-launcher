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

/// Where [`install_game`] unpacked the installer and which exe it launched, so
/// the UI can offer "open folder".
#[derive(Debug, Serialize)]
pub struct InstallGameResult {
    /// The folder the archive was unpacked into.
    pub installer_dir: String,
    /// The installer executable that was launched.
    pub exe_path: String,
}

/// Unpack a previously downloaded + verified installer `.zip` beside itself and
/// launch its installer, which self-elevates (UAC) to install the game.
///
/// The launcher itself never elevates: it only writes the extracted files into
/// the same user-owned folder the zip is in (guaranteed writable — the download
/// step rejected unwritable folders), then asks the OS to start
/// `UT4_Installer.exe`. That exe is marked `requireAdministrator`, so Windows
/// raises the elevation prompt for *it* — a plain spawn would fail os-740.
///
/// Emits `game-download-progress` events with phase `"extract"`. Returns the
/// unpack folder + launched exe path. The verified `.zip` is kept (the user can
/// delete it, and the unpack folder, once UT4 is installed).
#[tauri::command]
pub async fn install_game(app: AppHandle, zip_path: String) -> Result<InstallGameResult, String> {
    let zip = PathBuf::from(&zip_path);
    if !zip.is_file() {
        return Err("the downloaded installer is missing — download it again".into());
    }
    let parent = zip
        .parent()
        .ok_or("the installer has no containing folder")?
        .to_path_buf();
    // Unpack into a sibling folder named after the zip (e.g. `UT4-Installer-1.1.0`).
    let stem = zip
        .file_stem()
        .unwrap_or_else(|| std::ffi::OsStr::new("UT4-Installer"));
    let dest = parent.join(stem);

    // Total uncompressed size — for the free-space check and the progress total.
    let total = ncp_host::total_uncompressed_size(&zip).map_err(|e| e.to_string())?;

    // Free-space guard on the extract drive (same volume as the zip): unpacking
    // the ~10 GB archive needs roughly its uncompressed size again, on top of
    // the zip already on disk.
    if let Some(free) = ncp_host::disk::available_space(&parent) {
        let needed = total.saturating_add(512 * 1024 * 1024); // +0.5 GB headroom
        if free < needed {
            return Err(format!(
                "not enough free space to unpack the installer — need ~{:.1} GB more on that drive, have {:.1} GB. Free up space (you can delete the downloaded .zip once UT4 is installed) and try again.",
                total as f64 / 1e9,
                free as f64 / 1e9,
            ));
        }
    }

    // Extraction (~10 GB, synchronous) and the modal UAC wait inside
    // `shell_launch` must not run on the async executor — do them on a blocking
    // thread. Progress events still flow (emit is thread-agnostic).
    let app_bg = app.clone();
    let handle = tauri::async_runtime::spawn_blocking(
        move || -> Result<InstallGameResult, String> {
            // Start clean: clear any partial unpack from a previous attempt.
            if dest.exists() {
                std::fs::remove_dir_all(&dest)
                    .map_err(|e| format!("couldn't clear the previous unpack folder: {e}"))?;
            }

            ncp_host::extract_zip(&zip, &dest, total, |done, total| {
                emit(&app_bg, "extract", done, total);
            })
            .map_err(|e| e.to_string())?;

            let exe = ncp_host::find_installer_exe(&dest).ok_or_else(|| {
            "couldn't find the installer program inside the archive — open the folder and run it manually".to_string()
        })?;
            let work = exe.parent().unwrap_or(&dest).to_path_buf();

            ncp_host::shell_launch(&exe, &work).map_err(|e| match e {
            ncp_host::ElevateError::Cancelled => {
                "you declined the Windows admin prompt — the installer didn't start. Click Install UT4 again when you're ready.".to_string()
            }
            other => other.to_string(),
        })?;

            Ok(InstallGameResult {
                installer_dir: dest.to_string_lossy().into_owned(),
                exe_path: exe.to_string_lossy().into_owned(),
            })
        },
    );

    match handle.await {
        Ok(res) => res,
        Err(e) => Err(format!("the install step failed to run: {e}")),
    }
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
