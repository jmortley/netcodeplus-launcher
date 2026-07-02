//! UT4-OpenAL installation: overlay the audio module onto a UT4 install and
//! stage OpenAL Soft's per-user config.
//!
//! The release zip (hash-pinned in the signed manifest, hosted on the
//! third-party GitHub release) carries:
//!
//! - `Win64/**` — `OpenAL32.dll`, `UE4-ALAudio-Win64-Shipping.dll` and the
//!   `AL_Sounds/` tree, which belong verbatim in
//!   `<root>/Engine/Binaries/Win64/` (next to `UE4-Win64-Shipping.exe`).
//! - `OpenAL-44100Hz.zip` / `OpenAL-48000Hz.zip` — nested zips, one per sound
//!   card sample rate, each holding `alsoft.ini` + `OpenAL/HRTF/*.mhr`, which
//!   belong in the user's roaming config dir (`%AppData%` on Windows).
//! - `alsoft-config/`, `Readme.txt`, … — tooling/docs the launcher ignores.
//!
//! Trust: the caller downloads + SHA-256-verifies the zip against the signed
//! manifest **before** calling in here (exactly like the plugin/pak flows), so
//! this module's job is only safe placement — every entry passes the shared
//! zip-slip guards, and an existing `alsoft.ini` is preserved (users tune HRTF
//! selection and `period_size` by hand; stomping that on reinstall would undo
//! their setup).
//!
//! The remaining manual step from the release's Readme — the `[Audio]`
//! `AudioDeviceModuleName=ALAudio` Engine.ini line — is already covered by
//! [`crate::config::apply`], which writes it whenever
//! [`crate::config::openal_installed`] turns true.

use std::fs;
use std::io::{self, Read};
use std::path::Path;

use thiserror::Error;

use crate::zip_safety::{is_safe_entry, joined_is_within};

/// Failure modes for [`install_openal_zip`].
#[derive(Debug, Error)]
pub enum OpenalInstallError {
    /// A ZIP entry name was unsafe (absolute, `..`, drive/UNC, or reserved /
    /// control characters) — a zip-slip attempt. Installation is aborted.
    #[error("unsafe path in archive: {0:?}")]
    UnsafeEntry(String),

    /// The archive doesn't look like the UT4-OpenAL release the manifest
    /// described (e.g. no `Win64/` payload, or no config zip for the requested
    /// sample rate). Guards against a re-rolled asset with a new layout being
    /// half-applied.
    #[error("unexpected UT4-OpenAL archive layout: {0}")]
    MissingPayload(String),

    /// The requested sample rate isn't one the release ships a config for.
    #[error("unsupported sample rate {0} Hz — expected 44100 or 48000")]
    UnsupportedSampleRate(u32),

    /// The install root has no `Engine/Binaries/Win64` — not a UT4 tree.
    #[error("not a UT4 install: {0:?} has no Engine/Binaries/Win64")]
    NotAnInstall(std::path::PathBuf),

    /// Error opening or reading the ZIP.
    #[error("could not read the UT4-OpenAL archive: {0}")]
    Zip(#[from] zip::result::ZipError),

    /// Filesystem error during placement.
    #[error("UT4-OpenAL install I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Result alias for OpenAL installation.
pub type Result<T> = std::result::Result<T, OpenalInstallError>;

/// What [`install_openal_zip`] did, for the UI to report.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct OpenalInstallSummary {
    /// Files placed under `<root>/Engine/Binaries/Win64/` (DLLs + AL_Sounds).
    pub binaries_files: usize,
    /// Whether a fresh `alsoft.ini` was written to the config dir.
    pub alsoft_ini_written: bool,
    /// Whether an existing `alsoft.ini` was found and deliberately kept.
    pub alsoft_ini_kept: bool,
    /// HRTF (`.mhr`) files placed under the config dir.
    pub hrtf_files: usize,
}

/// The user's roaming config dir (`%AppData%` on Windows) — where OpenAL
/// Soft reads `alsoft.ini` and `OpenAL/HRTF/`. `None` if it can't be resolved.
#[must_use]
pub fn roaming_config_dir() -> Option<std::path::PathBuf> {
    dirs::config_dir()
}

/// Install the (already-verified) UT4-OpenAL release zip at `zip_path`:
/// `Win64/**` into `<root>/Engine/Binaries/Win64/`, and the nested
/// `OpenAL-<sample_rate>Hz.zip`'s contents into `config_dir` (the user's
/// roaming config dir — `%AppData%`), keeping any existing `alsoft.ini`.
///
/// The caller is responsible for verifying the zip against the signed manifest
/// first and for ensuring the game is not running (the DLL overlay replaces
/// files the game holds open).
///
/// # Errors
/// See [`OpenalInstallError`]. Layout problems (missing `Win64/` payload or
/// config zip) are detected **before** anything is written; mid-write I/O
/// errors are not transactional — files already placed stay (they're inert
/// without the Engine.ini `[Audio]` line, which only [`crate::config::apply`]
/// writes once detection succeeds).
pub fn install_openal_zip(
    zip_path: &Path,
    root: &Path,
    config_dir: &Path,
    sample_rate: u32,
) -> Result<OpenalInstallSummary> {
    if !matches!(sample_rate, 44_100 | 48_000) {
        return Err(OpenalInstallError::UnsupportedSampleRate(sample_rate));
    }
    let binaries_dir = crate::config::openal_dir(root); // <root>/Engine/Binaries/Win64
    if !binaries_dir.is_dir() {
        return Err(OpenalInstallError::NotAnInstall(root.to_path_buf()));
    }

    let file = fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    // Read + parse the nested per-sample-rate config zip up front, BEFORE any
    // write: if the archive doesn't carry the requested config (a re-rolled
    // release with a new layout), the install must fail with nothing placed —
    // otherwise the DLL overlay would land, detection would turn true, and the
    // Engine.ini [Audio] line could be enabled with no OpenAL config staged.
    let inner_name = format!("OpenAL-{sample_rate}Hz.zip");
    let mut inner_bytes = Vec::new();
    {
        let mut inner_entry = archive.by_name(&inner_name).map_err(|_| {
            OpenalInstallError::MissingPayload(format!("no {inner_name} in the archive"))
        })?;
        inner_entry.read_to_end(&mut inner_bytes)?;
    }
    let mut inner = zip::ZipArchive::new(io::Cursor::new(inner_bytes))?;

    // Pass 1: the Win64/ overlay → <root>/Engine/Binaries/Win64/.
    let mut binaries_files = 0usize;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();
        let Some(rest) = name.strip_prefix("Win64/") else {
            continue;
        };
        if rest.is_empty() {
            continue; // the Win64/ dir entry itself
        }
        if !is_safe_entry(&name) {
            return Err(OpenalInstallError::UnsafeEntry(name));
        }
        let Some(out_path) = joined_is_within(&binaries_dir, rest) else {
            return Err(OpenalInstallError::UnsafeEntry(name));
        };
        if entry.is_dir() || rest.ends_with('/') {
            fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::File::create(&out_path)?;
        io::copy(&mut entry, &mut out)?;
        binaries_files += 1;
    }
    if binaries_files == 0 {
        return Err(OpenalInstallError::MissingPayload(
            "no Win64/ entries (DLLs + AL_Sounds) in the archive".into(),
        ));
    }

    // Pass 2: the (pre-validated) config zip → the roaming config dir.
    let existing_alsoft = config_dir.join("alsoft.ini").is_file();
    let mut alsoft_ini_written = false;
    let mut hrtf_files = 0usize;
    for i in 0..inner.len() {
        let mut entry = inner.by_index(i)?;
        let name = entry.name().to_string();
        if !is_safe_entry(&name) {
            return Err(OpenalInstallError::UnsafeEntry(name));
        }
        let Some(out_path) = joined_is_within(config_dir, &name) else {
            return Err(OpenalInstallError::UnsafeEntry(name));
        };
        if entry.is_dir() || name.ends_with('/') {
            fs::create_dir_all(&out_path)?;
            continue;
        }
        // Preserve a user's tuned alsoft.ini (HRTF choice, period_size).
        if name.eq_ignore_ascii_case("alsoft.ini") && existing_alsoft {
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::File::create(&out_path)?;
        io::copy(&mut entry, &mut out)?;
        if name.eq_ignore_ascii_case("alsoft.ini") {
            alsoft_ini_written = true;
        } else if name.to_ascii_lowercase().ends_with(".mhr") {
            hrtf_files += 1;
        }
    }
    if !alsoft_ini_written && !existing_alsoft {
        return Err(OpenalInstallError::MissingPayload(format!(
            "{inner_name} carried no alsoft.ini"
        )));
    }

    Ok(OpenalInstallSummary {
        binaries_files,
        alsoft_ini_written,
        alsoft_ini_kept: existing_alsoft,
        hrtf_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    /// Build an in-memory zip from `(name, bytes)` entries.
    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            let mut w = zip::ZipWriter::new(io::Cursor::new(&mut buf));
            for (name, bytes) in entries {
                if name.ends_with('/') {
                    w.add_directory(*name, opts).unwrap();
                } else {
                    w.start_file(*name, opts).unwrap();
                    w.write_all(bytes).unwrap();
                }
            }
            w.finish().unwrap();
        }
        buf
    }

    /// A minimal but shape-faithful UT4-OpenAL release zip.
    fn sample_release_zip(dir: &Path) -> std::path::PathBuf {
        let inner = zip_bytes(&[
            ("alsoft.ini", b"hrtf = true\n"),
            ("OpenAL/HRTF/irc_1037_48000.mhr", b"mhr-data"),
            ("OpenAL/HRTF/CIAIR_48000.mhr", b"mhr-data-2"),
        ]);
        let outer = zip_bytes(&[
            ("Readme.txt", b"docs"),
            ("Win64/", b""),
            ("Win64/OpenAL32.dll", b"openal32"),
            ("Win64/UE4-ALAudio-Win64-Shipping.dll", b"alaudio"),
            ("Win64/AL_Sounds/Sniper/Primary/shot.dat", b"pew"),
            ("OpenAL-48000Hz.zip", &inner),
        ]);
        let path = dir.join("UT4-OpenAL.zip");
        fs::write(&path, outer).unwrap();
        path
    }

    /// A root that passes the Engine/Binaries/Win64 existence check.
    fn ut4_root(dir: &Path) -> std::path::PathBuf {
        let root = dir.join("UnrealTournament");
        fs::create_dir_all(root.join("Engine/Binaries/Win64")).unwrap();
        root
    }

    #[test]
    fn installs_dlls_sounds_and_config() {
        let tmp = TempDir::new().unwrap();
        let zip = sample_release_zip(tmp.path());
        let root = ut4_root(tmp.path());
        let cfg = tmp.path().join("Roaming");
        fs::create_dir_all(&cfg).unwrap();

        let summary = install_openal_zip(&zip, &root, &cfg, 48_000).unwrap();
        assert_eq!(summary.binaries_files, 3);
        assert!(summary.alsoft_ini_written);
        assert!(!summary.alsoft_ini_kept);
        assert_eq!(summary.hrtf_files, 2);

        let bin = root.join("Engine/Binaries/Win64");
        assert_eq!(fs::read(bin.join("OpenAL32.dll")).unwrap(), b"openal32");
        assert_eq!(
            fs::read(bin.join("UE4-ALAudio-Win64-Shipping.dll")).unwrap(),
            b"alaudio"
        );
        assert_eq!(
            fs::read(bin.join("AL_Sounds/Sniper/Primary/shot.dat")).unwrap(),
            b"pew"
        );
        assert_eq!(fs::read(cfg.join("alsoft.ini")).unwrap(), b"hrtf = true\n");
        assert!(cfg.join("OpenAL/HRTF/irc_1037_48000.mhr").is_file());
        // The detection the config panel keys on now succeeds.
        assert!(crate::config::openal_installed(&root));
    }

    #[test]
    fn keeps_existing_alsoft_ini() {
        let tmp = TempDir::new().unwrap();
        let zip = sample_release_zip(tmp.path());
        let root = ut4_root(tmp.path());
        let cfg = tmp.path().join("Roaming");
        fs::create_dir_all(&cfg).unwrap();
        fs::write(cfg.join("alsoft.ini"), b"period_size = 64\n").unwrap();

        let summary = install_openal_zip(&zip, &root, &cfg, 48_000).unwrap();
        assert!(!summary.alsoft_ini_written);
        assert!(summary.alsoft_ini_kept);
        // The user's tuned config survives; HRTF data still lands.
        assert_eq!(
            fs::read(cfg.join("alsoft.ini")).unwrap(),
            b"period_size = 64\n"
        );
        assert_eq!(summary.hrtf_files, 2);
    }

    #[test]
    fn rejects_missing_sample_rate_zip() {
        let tmp = TempDir::new().unwrap();
        let zip = sample_release_zip(tmp.path()); // only ships 48000
        let root = ut4_root(tmp.path());
        let cfg = tmp.path().join("Roaming");
        fs::create_dir_all(&cfg).unwrap();
        let err = install_openal_zip(&zip, &root, &cfg, 44_100).unwrap_err();
        assert!(
            matches!(err, OpenalInstallError::MissingPayload(_)),
            "{err}"
        );
    }

    #[test]
    fn rejects_unsupported_sample_rate() {
        let tmp = TempDir::new().unwrap();
        let zip = sample_release_zip(tmp.path());
        let root = ut4_root(tmp.path());
        let err = install_openal_zip(&zip, &root, tmp.path(), 96_000).unwrap_err();
        assert!(matches!(
            err,
            OpenalInstallError::UnsupportedSampleRate(96_000)
        ));
    }

    #[test]
    fn rejects_non_ut4_root() {
        let tmp = TempDir::new().unwrap();
        let zip = sample_release_zip(tmp.path());
        let err =
            install_openal_zip(&zip, &tmp.path().join("nope"), tmp.path(), 48_000).unwrap_err();
        assert!(matches!(err, OpenalInstallError::NotAnInstall(_)));
    }

    #[test]
    fn rejects_zip_slip_in_win64_tree() {
        let tmp = TempDir::new().unwrap();
        // A valid config zip so the up-front layout check passes and the walk
        // reaches the hostile Win64/ entry.
        let inner = zip_bytes(&[("alsoft.ini", b"hrtf = true\n")]);
        let evil = zip_bytes(&[
            ("Win64/../../evil.dll", b"x"),
            ("OpenAL-48000Hz.zip", &inner),
        ]);
        let path = tmp.path().join("evil.zip");
        fs::write(&path, evil).unwrap();
        let root = ut4_root(tmp.path());
        let err = install_openal_zip(&path, &root, tmp.path(), 48_000).unwrap_err();
        assert!(matches!(err, OpenalInstallError::UnsafeEntry(_)), "{err}");
        assert!(!tmp.path().join("evil.dll").exists());
    }

    #[test]
    fn rejects_zip_slip_in_nested_config_zip() {
        let tmp = TempDir::new().unwrap();
        let inner = zip_bytes(&[("../evil.ini", b"x")]);
        let outer = zip_bytes(&[
            ("Win64/OpenAL32.dll", b"openal32"),
            ("OpenAL-48000Hz.zip", &inner),
        ]);
        let path = tmp.path().join("evil-inner.zip");
        fs::write(&path, outer).unwrap();
        let root = ut4_root(tmp.path());
        let cfg = tmp.path().join("Roaming");
        fs::create_dir_all(&cfg).unwrap();
        let err = install_openal_zip(&path, &root, &cfg, 48_000).unwrap_err();
        assert!(matches!(err, OpenalInstallError::UnsafeEntry(_)), "{err}");
        assert!(!tmp.path().join("evil.ini").exists());
    }

    #[test]
    fn missing_config_zip_places_nothing() {
        // The layout check runs BEFORE any write: asking for a rate the
        // archive doesn't ship must leave the install root untouched (a
        // half-applied overlay would flip detection true with no config).
        let tmp = TempDir::new().unwrap();
        let zip = sample_release_zip(tmp.path()); // only ships 48000
        let root = ut4_root(tmp.path());
        let cfg = tmp.path().join("Roaming");
        fs::create_dir_all(&cfg).unwrap();
        let err = install_openal_zip(&zip, &root, &cfg, 44_100).unwrap_err();
        assert!(
            matches!(err, OpenalInstallError::MissingPayload(_)),
            "{err}"
        );
        assert!(
            !root.join("Engine/Binaries/Win64/OpenAL32.dll").exists(),
            "overlay must not be placed when the config zip is missing"
        );
        assert!(!crate::config::openal_installed(&root));
    }

    #[test]
    fn rejects_archive_without_win64_payload() {
        let tmp = TempDir::new().unwrap();
        let empty = zip_bytes(&[("Readme.txt", b"nothing here")]);
        let path = tmp.path().join("empty.zip");
        fs::write(&path, empty).unwrap();
        let root = ut4_root(tmp.path());
        let err = install_openal_zip(&path, &root, tmp.path(), 48_000).unwrap_err();
        assert!(matches!(err, OpenalInstallError::MissingPayload(_)));
    }
}
