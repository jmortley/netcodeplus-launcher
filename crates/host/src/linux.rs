//! Linux support.
//!
//! On Linux, UT4 is the **Windows** build run under **Wine/Lutris**, not a native
//! Linux game. This is confirmed in the field: the running game logs
//! `Base directory: C:/…` (it lives inside the Wine prefix's `drive_c`, not the
//! Linux root, which Wine would map to `Z:`), and the Lutris client is a
//! *Shipping* build that loads `UE4-NetcodePlus-Win64-Shipping.dll`. So:
//!
//! - **Launching** means invoking the Windows `.exe` through Wine against its
//!   prefix — a PE binary can't be `exec`'d directly on Linux.
//! - The **plugin** lives inside the prefix at
//!   `<prefix>/drive_c/…/UnrealTournament/Plugins/NetcodePlus/` (the same Win64
//!   DLLs as on Windows; our release zip already carries both the Shipping and
//!   Development builds, so a whole-folder install is correct).
//!
//! These helpers are pure path/argv logic with **no Linux-only syscalls**, so
//! they unit-test on any host (including the Windows dev box and CI). The actual
//! `spawn` lives in [`crate::launch`] behind `cfg(not(windows))`.

use std::path::{Path, PathBuf};

/// Wine's name for the C: drive — a directory directly under the prefix root.
const DRIVE_C: &str = "drive_c";

/// Given a path **inside** a Wine prefix (e.g. the game exe under
/// `<prefix>/drive_c/…`), return the **prefix root** — the ancestor directory
/// that directly contains `drive_c`. `None` if the path isn't inside a prefix
/// (no `drive_c` ancestor), e.g. a path under the real Linux filesystem.
#[must_use]
pub fn wine_prefix_of(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|a| {
            a.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case(DRIVE_C))
        })
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

/// A resolved Wine launch — exactly what the caller should spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WineLaunch {
    /// The wine binary to run (`"wine"` on `PATH`, or an absolute path to e.g. a
    /// Lutris-managed build).
    pub program: String,
    /// argv passed to wine: the exe path first, then the game's own args. Wine
    /// accepts a Linux path to a Windows exe and forwards the rest to it (so
    /// `-ncpconnect=…` and the auth args pass straight through).
    pub args: Vec<String>,
    /// Value for the `WINEPREFIX` environment variable — the prefix root.
    pub wineprefix: PathBuf,
    /// Working directory (the exe's folder; UE4 resolves some relative paths
    /// against cwd).
    pub cwd: PathBuf,
}

/// Build a Wine launch plan for a Windows `exe` that lives inside a Wine prefix.
///
/// `wine_bin` overrides the default `wine` on `PATH` (so a caller can point at a
/// Lutris-managed build for runtime fidelity). Returns `None` if `exe` isn't
/// inside a recognisable prefix — the caller then falls back to a direct spawn.
///
/// NOTE: this runs the system/`WINE`-selected wine, which may differ from the
/// exact runner + DXVK Lutris configured for the game. Launching *through Lutris*
/// (`lutris lutris:rungame/<slug>`) for full fidelity is a follow-up; it needs
/// per-game slug discovery and doesn't forward dynamic args like `-ncpconnect`,
/// so Wine is the right primitive for the connect flow.
#[must_use]
pub fn plan_wine_launch(exe: &Path, args: &[String], wine_bin: Option<&str>) -> Option<WineLaunch> {
    let wineprefix = wine_prefix_of(exe)?;
    let cwd = exe.parent().unwrap_or(exe).to_path_buf();
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(exe.to_string_lossy().into_owned());
    argv.extend(args.iter().cloned());
    Some(WineLaunch {
        program: wine_bin.unwrap_or("wine").to_string(),
        args: argv,
        wineprefix,
        cwd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_prefix_from_exe_under_drive_c() {
        let exe = Path::new(
            "/home/barry/Games/ut4/drive_c/Program Files/UnrealTournament/UnrealTournament/Binaries/Win64/UnrealTournament.exe",
        );
        assert_eq!(
            wine_prefix_of(exe),
            Some(PathBuf::from("/home/barry/Games/ut4"))
        );
    }

    #[test]
    fn none_when_not_inside_a_prefix() {
        // A real Linux path (Wine would map this under Z:, never drive_c).
        assert_eq!(
            wine_prefix_of(Path::new("/home/barry/Downloads/UT4/foo.exe")),
            None
        );
    }

    #[test]
    fn drive_c_match_is_case_insensitive() {
        assert_eq!(
            wine_prefix_of(Path::new("/p/DRIVE_C/x/y.exe")),
            Some(PathBuf::from("/p"))
        );
    }

    #[test]
    fn wine_plan_exe_first_then_args_with_prefix_and_cwd() {
        let exe =
            Path::new("/games/ut/drive_c/UT/UnrealTournament/Binaries/Win64/UnrealTournament.exe");
        let args = vec!["-ncpconnect=1.2.3.4:7777".to_string(), "-log".to_string()];
        let plan = plan_wine_launch(exe, &args, None).expect("inside a prefix");
        assert_eq!(plan.program, "wine");
        assert_eq!(plan.wineprefix, PathBuf::from("/games/ut"));
        assert_eq!(plan.args[0], exe.to_string_lossy());
        assert_eq!(
            &plan.args[1..],
            &["-ncpconnect=1.2.3.4:7777".to_string(), "-log".to_string()][..]
        );
        assert_eq!(plan.cwd, exe.parent().unwrap());
    }

    #[test]
    fn wine_plan_honours_a_custom_wine_binary() {
        let exe = Path::new("/p/drive_c/a/b.exe");
        let plan = plan_wine_launch(exe, &[], Some("/opt/lutris/wine/bin/wine")).unwrap();
        assert_eq!(plan.program, "/opt/lutris/wine/bin/wine");
    }

    #[test]
    fn wine_plan_is_none_outside_a_prefix() {
        assert!(plan_wine_launch(Path::new("/home/x/foo.exe"), &[], None).is_none());
    }
}
