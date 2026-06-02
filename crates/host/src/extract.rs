//! Generic, safe ZIP extraction for the **game installer** download.
//!
//! Separate from [`crate::plugin_install`], which is NetcodePlus-specific (it
//! validates `.uplugin` + `Binaries/` and does an atomic folder swap). The game
//! installer is a plain archive we unpack wholesale into a user-writable folder,
//! then hand off to its own self-elevating installer exe — so this module just
//! extracts every entry and locates that exe.
//!
//! # Trust + safety
//!
//! The ZIP bytes are SHA-256-verified against the signed manifest by
//! [`crate::disk`]-checked download (see `installer.rs`) before this module sees
//! them, so authenticity is settled. The job here is to extract them *safely*:
//! the shared zip-slip guards in [`crate::zip_safety`] reject any entry that
//! would escape the destination, and we never create symlinks (a link entry
//! can't redirect a later write).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::zip_safety::{is_safe_entry, joined_is_within};

/// Failure modes for [`extract_zip`] / [`total_uncompressed_size`].
#[derive(Debug, Error)]
pub enum ExtractError {
    /// A ZIP entry name was unsafe (absolute, `..`, drive/UNC, or reserved /
    /// control characters) — a zip-slip attempt. Extraction is aborted.
    #[error("unsafe path in archive: {0:?}")]
    UnsafeEntry(String),

    /// Error opening or reading the ZIP.
    #[error("could not read the installer archive: {0}")]
    Zip(#[from] zip::result::ZipError),

    /// Filesystem error during extraction.
    #[error("extraction I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Result alias for extraction.
pub type Result<T> = std::result::Result<T, ExtractError>;

/// How often [`extract_zip`] reports progress — throttled so a multi-GB unpack
/// emits a few updates per second rather than one per file.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

/// Sum of every entry's uncompressed size: the total bytes extraction will
/// write, used for a free-space check and as the progress denominator. Reads
/// only the central directory, so it's cheap even for a ~10 GB archive.
///
/// # Errors
/// [`ExtractError::Io`] / [`ExtractError::Zip`] if the archive can't be opened
/// or its directory read.
pub fn total_uncompressed_size(zip_path: &Path) -> Result<u64> {
    let file = fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut total = 0u64;
    for i in 0..archive.len() {
        total = total.saturating_add(archive.by_index(i)?.size());
    }
    Ok(total)
}

/// Extract every entry of the ZIP at `zip_path` into `dest`, enforcing the
/// shared zip-slip guard and reporting `(written, total)` uncompressed-byte
/// progress (throttled). Pass `total` from [`total_uncompressed_size`] so the
/// caller can free-space-check first without re-opening the archive.
///
/// Directory entries create dirs; file entries write files; symlinks are never
/// created.
///
/// # Errors
/// [`ExtractError::UnsafeEntry`] on a zip-slip attempt (aborts immediately),
/// otherwise the underlying zip/IO error. On error, whatever was written so far
/// is left for the caller to clean up.
pub fn extract_zip(
    zip_path: &Path,
    dest: &Path,
    total: u64,
    on_progress: impl Fn(u64, u64),
) -> Result<()> {
    let file = fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    fs::create_dir_all(dest)?;
    let mut written: u64 = 0;
    let mut last = Instant::now();
    on_progress(0, total);

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();

        if !is_safe_entry(&name) {
            return Err(ExtractError::UnsafeEntry(name));
        }
        let Some(out_path) = joined_is_within(dest, &name) else {
            return Err(ExtractError::UnsafeEntry(name));
        };

        if entry.is_dir() || name.ends_with('/') || name.ends_with('\\') {
            fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::File::create(&out_path)?;
        written += io::copy(&mut entry, &mut out)?;
        if last.elapsed() >= PROGRESS_INTERVAL {
            on_progress(written, total);
            last = Instant::now();
        }
    }
    on_progress(written, total);
    Ok(())
}

/// Find the installer's entry-point executable under `dir` (a freshly extracted
/// installer tree). Heuristic, deliberately robust to the version-stamped
/// wrapper folder the archive carries (e.g. `UT4 Installer - v1.1.0/`): pick the
/// shallowest `*.exe` whose file name contains "installer" (case-insensitive)
/// and which is NOT inside a `Resources` directory.
///
/// That name rule lands on `UT4_Installer.exe` while the `Resources` exclusion
/// is belt-and-suspenders against the bundled redistributables (DirectX
/// `DXSETUP.exe`, the VC++ `vcredist_*.exe`) — neither of which contains
/// "installer" anyway. Returns `None` when nothing matches; the caller then
/// tells the user to open the folder and run the installer manually.
#[must_use]
pub fn find_installer_exe(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<PathBuf> = None;
    let mut best_depth = usize::MAX;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if is_installer_exe(&path) {
                let depth = path.components().count();
                if depth < best_depth {
                    best_depth = depth;
                    best = Some(path);
                }
            }
        }
    }
    best
}

/// Whether `path` is an installer entry-point exe: `.exe` extension, file name
/// contains "installer" (case-insensitive), and no path component is
/// `Resources` (where the bundled redistributables live).
fn is_installer_exe(path: &Path) -> bool {
    let is_exe = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"));
    if !is_exe {
        return false;
    }
    let name_has_installer = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().contains("installer"));
    if !name_has_installer {
        return false;
    }
    // Reject anything under a `Resources` directory (DXSETUP, vcredist).
    !path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("Resources"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    /// Touch `path`, creating parents, with the given bytes.
    fn touch(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn finds_installer_excluding_resources_and_redists() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // The real layout: one version-stamped wrapper folder.
        let wrap = root.join("UT4 Installer - v1.1.0");
        touch(&wrap.join("UT4_Installer.exe"), b"MZ");
        touch(&wrap.join("Resources/dx/DXSETUP.exe"), b"MZ");
        touch(&wrap.join("Resources/vcredist_x64_12.0.exe"), b"MZ");
        touch(&wrap.join("Engine/Paks/data.pak"), b"x");

        let found = find_installer_exe(root).expect("should find the installer");
        assert_eq!(found.file_name().unwrap(), "UT4_Installer.exe");
    }

    #[test]
    fn no_installer_exe_returns_none() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Only a redistributable exists, and only under Resources → not an entry.
        touch(&root.join("Wrap/Resources/SomeInstaller.exe"), b"MZ");
        touch(&root.join("Wrap/data.bin"), b"x");
        assert!(find_installer_exe(root).is_none());
    }

    #[test]
    fn extract_zip_round_trips_with_progress() {
        // Build a tiny STORED zip in memory (no compression-method dependency).
        let mut buf: Vec<u8> = Vec::new();
        {
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            let mut w = zip::ZipWriter::new(io::Cursor::new(&mut buf));
            w.add_directory("Wrap/", opts).unwrap();
            w.start_file("Wrap/UT4_Installer.exe", opts).unwrap();
            w.write_all(b"MZ-installer").unwrap();
            w.start_file("Wrap/Resources/note.txt", opts).unwrap();
            w.write_all(b"hello").unwrap();
            w.finish().unwrap();
        }
        let tmp = TempDir::new().unwrap();
        let zip_path = tmp.path().join("installer.zip");
        fs::write(&zip_path, &buf).unwrap();

        let total = total_uncompressed_size(&zip_path).unwrap();
        assert_eq!(total, (b"MZ-installer".len() + b"hello".len()) as u64);

        let dest = tmp.path().join("unpacked");
        // `extract_zip` takes `Fn` (matching the net crate's progress
        // convention), so capture via Cell rather than mutating directly.
        let last_done = std::cell::Cell::new(0u64);
        let last_total = std::cell::Cell::new(0u64);
        extract_zip(&zip_path, &dest, total, |done, t| {
            last_done.set(done);
            last_total.set(t);
        })
        .unwrap();

        assert_eq!(
            fs::read(dest.join("Wrap/UT4_Installer.exe")).unwrap(),
            b"MZ-installer"
        );
        assert_eq!(
            fs::read(dest.join("Wrap/Resources/note.txt")).unwrap(),
            b"hello"
        );
        // Final progress reports the full total.
        assert_eq!(last_done.get(), total);
        assert_eq!(last_total.get(), total);
        // And the finder still resolves the entry-point exe inside the result.
        assert_eq!(
            find_installer_exe(&dest).unwrap().file_name().unwrap(),
            "UT4_Installer.exe"
        );
    }
}
