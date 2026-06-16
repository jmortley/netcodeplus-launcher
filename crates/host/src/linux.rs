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

// ── Lutris / Wine-prefix detection ──────────────────────────────────────────
//
// On Linux the UT4 install lives inside a Wine prefix, so the launcher can't use
// the Windows registry/shortcut probes — it discovers the install from Lutris.
// Lutris records each game in `~/.config/lutris/games/<slug>-<id>.yml` with the
// Windows `exe` and the Wine `prefix` as Linux paths — the precise, high-
// confidence signal. The YAML field extraction here is pure (string -> paths) so
// it unit-tests on any host (Windows dev box + CI); the thin filesystem walk that
// feeds it into `check_install` is the only `not(windows)` piece.

/// A UT4 install located from a Lutris game config: the Windows `exe` and the
/// Wine `prefix`, both as Linux paths. Either may be absent in a sparse config.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LutrisGame {
    /// `game.exe` — absolute Linux path to the Windows `.exe` inside `drive_c`.
    pub exe: Option<PathBuf>,
    /// `game.prefix` — the Wine prefix root (the dir that contains `drive_c`).
    pub prefix: Option<PathBuf>,
}

/// Strip surrounding matched quotes + whitespace from a scalar YAML value;
/// `None` if what remains is empty.
fn clean_scalar(value: &str) -> Option<PathBuf> {
    let v = value.trim();
    let v = v
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .or_else(|| {
            v.strip_prefix('\'')
                .and_then(|inner| inner.strip_suffix('\''))
        })
        .unwrap_or(v);
    (!v.is_empty()).then(|| PathBuf::from(v))
}

/// Extract `game.exe` / `game.prefix` from the contents of a Lutris game `.yml`.
///
/// A deliberately small, dependency-free reader: Lutris writes these as plain
/// `key: value` scalars (absolute paths, occasionally quoted) indented under a
/// top-level `game:` block, so we track that block and pull the two keys. Keys in
/// other top-level blocks (`system:`, `wine:`) are ignored. This covers the real-
/// world configs; a malformed/exotic file just yields `None`s and the caller
/// falls back to the manual folder-pick. (If we later need full YAML fidelity,
/// swap this for `serde_yml` — kept dependency-free for now.)
#[must_use]
pub fn parse_lutris_game_yaml(contents: &str) -> LutrisGame {
    let mut game = LutrisGame::default();
    let mut in_game_block = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // A top-level key starts in column 0 (no leading space/tab).
        let at_top_level = line.chars().next().is_some_and(|c| c != ' ' && c != '\t');
        if at_top_level {
            // Entering `game:`, or leaving it for another top-level block.
            in_game_block = trimmed == "game:";
            continue;
        }
        if !in_game_block {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("exe:") {
            game.exe = clean_scalar(value);
        } else if let Some(value) = trimmed.strip_prefix("prefix:") {
            game.prefix = clean_scalar(value);
        }
    }
    game
}

/// The best single path to hand [`crate::install::check_install`] for a Lutris
/// game: the `exe` (precise — `check_install` walks up to the install root), else
/// the prefix's `drive_c` as a starting point. `None` if neither is known.
#[must_use]
pub fn lutris_install_probe(game: &LutrisGame) -> Option<PathBuf> {
    match &game.exe {
        Some(exe) => Some(exe.clone()),
        None => game.prefix.as_ref().map(|prefix| prefix.join(DRIVE_C)),
    }
}

/// Lutris's per-game config directory: `<home>/.config/lutris/games`.
#[must_use]
pub fn lutris_games_dir(home: &Path) -> PathBuf {
    home.join(".config").join("lutris").join("games")
}

/// Discover UT4 install roots from Lutris game configs.
///
/// Reads every `*.yml` under [`lutris_games_dir`], extracts the exe/prefix, and
/// resolves each to a real UT4 install root via [`crate::install::check_install`]
/// (which validates the tree, so non-UT4 Lutris games are skipped). Deduped.
/// Returns empty when Lutris isn't installed or nothing resolves — the UI then
/// falls back to the manual folder-pick. `mod_paks_dir` is threaded into the
/// resolved [`crate::install::UtInstall`] exactly as the Windows path does.
///
/// NOTE: a prefix-only config (no `exe`) can't be resolved here — `check_install`
/// walks *up* from the probe, and the install sits *below* `drive_c`. Those fall
/// to the manual pick; a downward prefix scan is a follow-up (roadmap step 2).
#[cfg(not(windows))]
#[must_use]
pub fn detect_lutris_ut4_roots(mod_paks_dir: &Path) -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(lutris_games_dir(&home)) else {
        return Vec::new();
    };
    let mut roots: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yml") {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(probe) = lutris_install_probe(&parse_lutris_game_yaml(&contents)) else {
            continue;
        };
        if let Some(install) = crate::install::check_install(&probe, mod_paks_dir.to_path_buf()) {
            if !roots.contains(&install.root) {
                roots.push(install.root);
            }
        }
    }
    roots
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

    // ── Lutris detection ────────────────────────────────────────────────────

    const SAMPLE_LUTRIS_YML: &str = "\
game:
  arch: win64
  exe: /home/barry/Games/ut/drive_c/Program Files/UnrealTournament/UnrealTournament/Binaries/Win64/UnrealTournament.exe
  prefix: /home/barry/Games/ut
  working_dir: /home/barry/Games/ut/drive_c
system:
  env:
    exe: /should/be/ignored
wine:
  version: lutris-GE-Proton8-26-x86_64
";

    #[test]
    fn parses_exe_and_prefix_from_the_game_block() {
        let g = parse_lutris_game_yaml(SAMPLE_LUTRIS_YML);
        assert_eq!(
            g.exe.as_deref(),
            Some(Path::new(
                "/home/barry/Games/ut/drive_c/Program Files/UnrealTournament/UnrealTournament/Binaries/Win64/UnrealTournament.exe"
            ))
        );
        assert_eq!(g.prefix.as_deref(), Some(Path::new("/home/barry/Games/ut")));
    }

    #[test]
    fn ignores_keys_outside_the_game_block() {
        // The `exe:` nested under system.env must NOT be captured.
        let g = parse_lutris_game_yaml(SAMPLE_LUTRIS_YML);
        assert_ne!(g.exe.as_deref(), Some(Path::new("/should/be/ignored")));
    }

    #[test]
    fn strips_matched_quotes_around_values() {
        let yml = "game:\n  exe: \"/q/drive_c/UT/x.exe\"\n  prefix: '/q'\n";
        let g = parse_lutris_game_yaml(yml);
        assert_eq!(g.exe.as_deref(), Some(Path::new("/q/drive_c/UT/x.exe")));
        assert_eq!(g.prefix.as_deref(), Some(Path::new("/q")));
    }

    #[test]
    fn missing_game_fields_yield_default() {
        let g = parse_lutris_game_yaml("game:\n  arch: win64\nsystem: {}\n");
        assert_eq!(g, LutrisGame::default());
    }

    #[test]
    fn probe_prefers_exe_then_falls_back_to_prefix_drive_c() {
        let with_exe = LutrisGame {
            exe: Some(PathBuf::from("/p/drive_c/UT/u.exe")),
            prefix: Some(PathBuf::from("/p")),
        };
        assert_eq!(
            lutris_install_probe(&with_exe),
            Some(PathBuf::from("/p/drive_c/UT/u.exe"))
        );

        let prefix_only = LutrisGame {
            exe: None,
            prefix: Some(PathBuf::from("/p")),
        };
        assert_eq!(
            lutris_install_probe(&prefix_only),
            Some(PathBuf::from("/p/drive_c"))
        );

        assert_eq!(lutris_install_probe(&LutrisGame::default()), None);
    }

    #[test]
    fn games_dir_is_under_config_lutris() {
        assert_eq!(
            lutris_games_dir(Path::new("/home/barry")),
            PathBuf::from("/home/barry/.config/lutris/games")
        );
    }
}
