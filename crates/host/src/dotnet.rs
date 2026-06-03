//! Detect whether the .NET **Windows Desktop Runtime** is installed.
//!
//! The UT4 community installer (`UT4_Installer.exe`) is a .NET WinForms app, so
//! it won't start without a matching Windows Desktop Runtime — which mostly
//! bites Windows 10 users who don't already have it. The launcher uses this to
//! warn + link to Microsoft's download up front, rather than letting the
//! installer dead-end with its own ".NET is required" error.

/// Whether a `Microsoft.WindowsDesktop.App` shared framework with the given
/// `major` version is installed (the framework a .NET `<major>` desktop app
/// needs). Checks the canonical x64 install location
/// `%ProgramFiles%\dotnet\shared\Microsoft.WindowsDesktop.App\<major>.*`.
#[cfg(windows)]
#[must_use]
pub fn windowsdesktop_runtime_present(major: u32) -> bool {
    let Ok(pf) = std::env::var("ProgramFiles") else {
        return false;
    };
    runtime_present_in(std::path::Path::new(&pf), major)
}

/// Testable core: is there a `<major>.x` subdirectory under
/// `<program_files>/dotnet/shared/Microsoft.WindowsDesktop.App`? The `<major>.`
/// prefix (note the dot) avoids matching e.g. `16.x` when asked for major 1.
#[cfg(any(windows, test))]
fn runtime_present_in(program_files: &std::path::Path, major: u32) -> bool {
    let dir = program_files
        .join("dotnet")
        .join("shared")
        .join("Microsoft.WindowsDesktop.App");
    let prefix = format!("{major}.");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.path().is_dir()
            && e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(&prefix))
    })
}

/// Non-Windows stub: not applicable (the installer is Windows-only), so report
/// present to avoid a spurious warning on dev/CI builds.
#[cfg(not(windows))]
#[must_use]
pub fn windowsdesktop_runtime_present(_major: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::runtime_present_in;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn detects_requested_major_only() {
        let tmp = TempDir::new().unwrap();
        let base = tmp
            .path()
            .join("dotnet")
            .join("shared")
            .join("Microsoft.WindowsDesktop.App");
        fs::create_dir_all(base.join("6.0.36")).unwrap();
        fs::create_dir_all(base.join("8.0.1")).unwrap();
        assert!(runtime_present_in(tmp.path(), 6));
        assert!(runtime_present_in(tmp.path(), 8));
        assert!(!runtime_present_in(tmp.path(), 7));
        // major 1 must not match the "16"/"6"/"8" folders via a loose prefix.
        assert!(!runtime_present_in(tmp.path(), 1));
    }

    #[test]
    fn absent_when_no_dotnet_dir() {
        let tmp = TempDir::new().unwrap();
        assert!(!runtime_present_in(tmp.path(), 6));
    }
}
