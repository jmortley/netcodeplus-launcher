//! Verified download of the full UT4 game installer, for users who don't have
//! the game yet.
//!
//! The installer (~10 GB) is hosted by a third party (UT4Ever) the launcher
//! doesn't control, so trust works exactly like a pak: the signed manifest
//! carries the expected SHA-256, and a download whose bytes don't match is
//! rejected. Because the file is huge, the transfer **resumes** on interruption,
//! reports **progress** (emitted as `game-download-progress` events), can be
//! **cancelled**, and is **verified** in a final hash pass before the `.part` is
//! renamed into place. One click then unpacks the verified zip and launches the
//! installer, which self-elevates (UAC) to install the game — the launcher
//! itself never elevates (see [`install_game`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::updates::fetch_verify;

/// Set by [`cancel_game_download`], polled by the download loop. One installer
/// download runs at a time, so a single flag suffices.
static CANCEL: AtomicBool = AtomicBool::new(false);

/// Path of the most recently downloaded **and verified** installer zip, recorded
/// by [`download_game_installer`] and consumed by [`install_game`].
///
/// Defense-in-depth: [`install_game`] unpacks this zip and shell-launches the
/// installer it contains (which self-elevates via UAC), so the path must NOT
/// come from the frontend — an injected IPC call could otherwise point it at an
/// arbitrary attacker-staged zip. By taking the path from this backend record
/// (set only after the SHA-256 verification passes), the unpack+elevate path can
/// only ever act on bytes the launcher itself verified against the signed
/// manifest. (The CSP + `withGlobalTauri:false` already gate `invoke`; this
/// closes the primitive on its own merits.)
static VERIFIED_INSTALLER: Mutex<Option<PathBuf>> = Mutex::new(None);

/// What the UI needs to offer the download (or hide it).
#[derive(Debug, Serialize)]
pub struct GameInstallerInfo {
    /// Whether the signed manifest advertises a game installer at all.
    pub available: bool,
    /// Display version (e.g. `"1.1.0"`); empty when none.
    pub version: String,
    /// Declared size in bytes (0 when none).
    pub size_bytes: u64,
    /// Whether the .NET Desktop Runtime the installer needs is present (it's a
    /// .NET WinForms app and won't start without it). The UI shows a non-blocking
    /// "get the .NET runtime" note when this is false — common on Windows 10.
    pub dotnet_ok: bool,
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
    // The UT4 installer (`UT4_Installer.exe`) is a .NET 6 WinForms app.
    let dotnet_ok = ncp_host::windowsdesktop_runtime_present(6);
    Ok(match manifest.game_installer {
        Some(gi) => GameInstallerInfo {
            available: true,
            version: gi.version,
            size_bytes: gi.size_bytes,
            dotnet_ok,
        },
        None => GameInstallerInfo {
            available: false,
            version: String::new(),
            size_bytes: 0,
            dotnet_ok,
        },
    })
}

/// The integrity facts for the UT4 installer from the **signed** manifest, so a
/// user who downloads it themselves (e.g. on Linux, from the third-party UT4Ever
/// host) can trust the bytes. The SHA-256 here comes from the launcher's own
/// minisign-verified manifest — not from the download host — so it's a trust
/// anchor the host can't forge.
#[derive(Debug, Serialize)]
pub struct GameInstallerIntegrity {
    /// Whether the signed manifest advertises a game installer at all.
    pub available: bool,
    /// Display version (e.g. `"1.1.0"`).
    pub version: String,
    /// The download URL the manifest points at (untrusted host — integrity is the
    /// SHA-256, never the URL).
    pub url: String,
    /// Expected SHA-256, lowercase hex — pinned in the signed manifest.
    pub sha256: String,
    /// Expected size in bytes.
    pub size_bytes: u64,
}

/// Expose the signed manifest's game-installer URL + SHA-256 + size, so the UI can
/// show users exactly what to download and the hash to trust it against.
#[tauri::command]
pub async fn game_installer_integrity(app: AppHandle) -> Result<GameInstallerIntegrity, String> {
    let (manifest, ..) = fetch_verify(&app).await?;
    Ok(match manifest.game_installer {
        Some(gi) => GameInstallerIntegrity {
            available: true,
            version: gi.version,
            url: gi.url,
            sha256: gi.sha256.to_string(),
            size_bytes: gi.size_bytes,
        },
        None => GameInstallerIntegrity {
            available: false,
            version: String::new(),
            url: String::new(),
            sha256: String::new(),
            size_bytes: 0,
        },
    })
}

/// Result of verifying a user-supplied file against the signed manifest's
/// game-installer SHA-256.
#[derive(Debug, Serialize)]
pub struct VerifyDownloadResult {
    /// True when the file's SHA-256 equals the manifest's pinned digest.
    pub matched: bool,
    /// The file's actual SHA-256 (lowercase hex).
    pub sha256: String,
    /// The expected SHA-256 from the signed manifest (lowercase hex).
    pub expected: String,
    /// The file's size in bytes.
    pub size_bytes: u64,
}

/// Verify a file the user downloaded (the UT4 installer zip) against the signed
/// manifest's pinned SHA-256. Streams the file through the same hasher the
/// launcher's own download uses; the expected digest comes from the
/// minisign-verified manifest, so a match means the bytes are exactly what the
/// launcher author signed — regardless of where they were downloaded from.
#[tauri::command]
pub async fn verify_game_download(
    app: AppHandle,
    path: String,
) -> Result<VerifyDownloadResult, String> {
    let (manifest, ..) = fetch_verify(&app).await?;
    let installer = manifest
        .game_installer
        .ok_or("the update manifest has no game installer to verify against")?;
    let p = PathBuf::from(&path);
    if !p.is_file() {
        return Err("that file doesn't exist — pick the downloaded installer zip".into());
    }
    let size = std::fs::metadata(&p).map_err(|e| e.to_string())?.len();
    let digest = ncp_net::hash_file(&p, |_, _| {})
        .await
        .map_err(|e| e.to_string())?;
    Ok(VerifyDownloadResult {
        matched: digest == installer.sha256,
        sha256: digest.to_string(),
        expected: installer.sha256.to_string(),
        size_bytes: size,
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
            "can't download there — that's a protected folder. This is only a temporary download spot; you choose where UT4 actually installs later, in the installer's own window. Pick a normal folder you own like your Downloads — not Program Files or a drive root."
                .into(),
        );
    }
    let final_path = dir.join(format!(
        "UT4-Installer-{}.zip",
        safe_version(&installer.version)
    ));
    let part = PathBuf::from(format!("{}.part", final_path.to_string_lossy()));

    // Free-space guard: one click downloads AND unpacks, so the drive needs room
    // for both the ~10 GB zip and its extracted copy (a stored archive — unpacked
    // is ~the same size), i.e. roughly twice the download plus headroom. Only
    // block when the figure is actually readable.
    if let Some(free) = ncp_host::disk::available_space(&dir) {
        let needed = installer
            .size_bytes
            .saturating_mul(2)
            .saturating_add(512 * 1024 * 1024); // download + unpack + 0.5 GB
        if free < needed {
            return Err(format!(
                "not enough free space on that drive — downloading and installing UT4 needs ~{:.1} GB, have {:.1} GB",
                needed as f64 / 1e9,
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

    // Verified — commit the `.part` to the final name, and record it as THE
    // installer `install_game` may act on (so the unpack+elevate path never
    // trusts a frontend-supplied path).
    std::fs::rename(&part, &final_path).map_err(|e| e.to_string())?;
    *VERIFIED_INSTALLER.lock().unwrap() = Some(final_path.clone());
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
///
/// Takes NO path: it acts only on the installer [`download_game_installer`]
/// verified this session (recorded in [`VERIFIED_INSTALLER`]). A frontend-
/// supplied path is deliberately not accepted — see that static's docs.
#[tauri::command]
pub async fn install_game(app: AppHandle) -> Result<InstallGameResult, String> {
    let zip = VERIFIED_INSTALLER
        .lock()
        .unwrap()
        .clone()
        .ok_or("download the UT4 installer first — there's no verified download to install")?;
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

/// The user's Downloads folder, used as the default location for the
/// download-folder picker — a known user-writable spot that steers users away
/// from protected folders (where an un-elevated download would be refused).
/// `None` if it can't be resolved.
#[tauri::command]
pub fn default_download_dir() -> Option<String> {
    ncp_host::default_download_dir().map(|p| p.to_string_lossy().into_owned())
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
