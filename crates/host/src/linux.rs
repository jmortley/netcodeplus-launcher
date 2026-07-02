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
    /// Extra environment variables to set for the child (from the Lutris game's
    /// `system.env`). Passed as literal `key=value` pairs — never shell-expanded.
    /// Empty for a plain (non-Lutris) wine launch.
    pub env: Vec<(String, String)>,
    /// A command **prefix** to wrap the launch in (from Lutris `system.prefix_command`,
    /// e.g. `["taskset", "-c", "0-5,12-17"]`). When non-empty, the process actually
    /// spawned is `wrapper[0]` with args `wrapper[1..] ++ [program] ++ args`. Empty
    /// for an unwrapped launch.
    pub wrapper: Vec<String>,
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
        env: Vec::new(),
        wrapper: Vec::new(),
    })
}

/// The in-prefix UT4 install **root** for a Wine `prefix` — the path to hand the
/// (platform-agnostic) installer/detection code, exactly as a Windows root. UT4's
/// Lutris install sits at the standard Windows location inside `drive_c`, so this
/// is `<prefix>/drive_c/Program Files/UnrealTournament`. Pure path join; pass it to
/// [`crate::install::netcodeplus_dir`] / [`crate::install::check_install`] / the
/// plugin installer unchanged. Useful when a Lutris config gives only the prefix
/// (no `exe` to walk up from).
#[must_use]
pub fn ut4_root_from_prefix(prefix: &Path) -> PathBuf {
    prefix
        .join(DRIVE_C)
        .join("Program Files")
        .join("UnrealTournament")
}

/// Where the NetcodePlus plugin folder lives inside a Wine `prefix`:
/// `<prefix>/drive_c/Program Files/UnrealTournament/UnrealTournament/Plugins/NetcodePlus`.
/// Composed from [`ut4_root_from_prefix`] + [`crate::install::netcodeplus_dir`] so
/// the plugin subpath stays single-sourced with the Windows path. The plugin
/// installs as a whole-folder **copy** (Wine chokes on symlinks); [`crate::
/// plugin_install`] already copies, so installing with
/// `root = ut4_root_from_prefix(prefix)` is correct on Linux with no changes to
/// the install path logic.
#[must_use]
pub fn plugin_install_path(prefix: &Path) -> PathBuf {
    crate::install::netcodeplus_dir(&ut4_root_from_prefix(prefix))
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

/// Every directory Lutris might keep per-game `.yml` configs in. The classic
/// location is `~/.config/lutris/games`, but real installs also use
/// `~/.local/share/lutris/games` (seen on the dogfooding box — no `~/.config/
/// lutris` at all) and the Flatpak sandbox path. Search all of them.
#[must_use]
pub fn lutris_games_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        lutris_games_dir(home),
        home.join(".local")
            .join("share")
            .join("lutris")
            .join("games"),
        home.join(".var")
            .join("app")
            .join("net.lutris.Lutris")
            .join("data")
            .join("lutris")
            .join("games"),
    ]
}

/// All Lutris game `.yml` config paths found across [`lutris_games_dirs`].
#[cfg(not(windows))]
fn lutris_game_ymls() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for dir in lutris_games_dirs(&home) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("yml") {
                out.push(path);
            }
        }
    }
    out
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
    let mut roots: Vec<PathBuf> = Vec::new();
    for path in lutris_game_ymls() {
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

// ── Lutris launch fidelity (runner + env + explicit prefix) ──────────────────
//
// Detection above finds *where* UT4 is. Launching it *faithfully* needs three
// more things Lutris records per game:
//   • the Wine runner it configured (`wine.version`, e.g. a GE-Proton build that
//     lives under Steam's `compatibilitytools.d`, not the Lutris runners dir),
//   • the `WINEPREFIX` — which is `game.prefix`, an explicit path that is very
//     often OUTSIDE the game folder (games commonly install to a plain Linux dir,
//     not inside `drive_c`), so the launch prefix must come from the config and
//     NOT be derived by walking up from the exe, and
//   • `system.env` toggles (DXVK/esync/…) and a `system.prefix_command` wrapper
//     (e.g. `taskset` for CPU pinning).
//
// The parse + assembly is pure (text/paths in, plan out) so it unit-tests on any
// host against real fixtures; the only not(windows) I/O is the games-dir walk and
// the runner-binary existence probe.

/// A UT4 launch as configured in a Lutris game `.yml` — everything needed to
/// spawn the game with the same runner / env / prefix Lutris would use.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LutrisLaunch {
    /// `game.exe` — the Windows `.exe` (may live anywhere on the Linux FS).
    pub exe: Option<PathBuf>,
    /// `game.args`, split into tokens (the game's own launch args).
    pub args: Vec<String>,
    /// `game.prefix` — the explicit WINEPREFIX (authoritative; never derived).
    pub prefix: Option<PathBuf>,
    /// `wine.version` — the runner name, resolved to a wine binary at launch.
    pub runner: Option<String>,
    /// `system.env` — extra environment variables (literal `key=value`, never
    /// shell-expanded).
    pub env: Vec<(String, String)>,
    /// `system.prefix_command`, split into tokens — a wrapper like `taskset …`.
    pub prefix_command: Vec<String>,
}

/// Leading-space count — Lutris writes 2-space-indented YAML.
fn leading_spaces(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ').count()
}

/// Split a trimmed `key: value` line on its first colon. A bare `key:` yields an
/// empty value.
fn split_kv(trimmed: &str) -> (&str, &str) {
    match trimmed.split_once(':') {
        Some((k, v)) => (k.trim(), v.trim()),
        None => (trimmed, ""),
    }
}

/// Strip one layer of matched surrounding quotes.
fn unquote(v: &str) -> &str {
    let v = v.trim();
    v.strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .or_else(|| {
            v.strip_prefix('\'')
                .and_then(|inner| inner.strip_suffix('\''))
        })
        .unwrap_or(v)
}

/// Unquoted, whitespace-split tokens of a scalar (for `args` / `prefix_command`).
/// Empty when the scalar is blank. MVP tokeniser: Lutris writes plain space-
/// separated flags, so no shell-grade quote handling is needed.
fn split_tokens(value: &str) -> Vec<String> {
    unquote(value)
        .split_whitespace()
        .map(String::from)
        .collect()
}

/// Parse a full Lutris game `.yml` into a [`LutrisLaunch`].
///
/// A small dependency-free reader that tracks the current top-level block
/// (`game:` / `system:` / `wine:`) and the one nested map we care about
/// (`system.env`). Anything else is ignored. Malformed input degrades to
/// `Default` and the caller falls back to a plain wine launch.
#[must_use]
pub fn parse_lutris_launch(contents: &str) -> LutrisLaunch {
    let mut out = LutrisLaunch::default();
    let mut section = "";
    let mut in_env = false;
    let mut env_indent = 0usize;
    for line in contents.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indent = leading_spaces(line);
        let (key, value) = split_kv(line.trim());
        if indent == 0 {
            section = key;
            in_env = false;
            continue;
        }
        // A line at or shallower than the `env:` key ends the env sub-block.
        if in_env && indent <= env_indent {
            in_env = false;
        }
        match section {
            "game" => match key {
                "exe" => out.exe = clean_scalar(value),
                "prefix" => out.prefix = clean_scalar(value),
                "args" => out.args = split_tokens(value),
                _ => {}
            },
            "wine" => {
                if key == "version" {
                    let v = unquote(value);
                    if !v.is_empty() {
                        out.runner = Some(v.to_string());
                    }
                }
            }
            "system" => {
                if in_env {
                    let v = unquote(value);
                    if !key.is_empty() {
                        out.env.push((key.to_string(), v.to_string()));
                    }
                } else if key == "env" && value.is_empty() {
                    in_env = true;
                    env_indent = indent;
                } else if key == "prefix_command" {
                    out.prefix_command = split_tokens(value);
                }
            }
            _ => {}
        }
    }
    out
}

/// Directories that hold named Wine/Proton runners, most-specific first. A
/// runner named in `wine.version` is looked up as a subdir of one of these.
#[must_use]
pub fn runner_search_dirs(home: &Path) -> Vec<PathBuf> {
    [
        ".local/share/lutris/runners/wine",
        ".local/share/Steam/compatibilitytools.d",
        ".steam/root/compatibilitytools.d",
        ".steam/steam/compatibilitytools.d",
        ".var/app/com.valvesoftware.Steam/data/Steam/compatibilitytools.d",
    ]
    .iter()
    .map(|rel| home.join(rel))
    .collect()
}

/// Resolve a runner name to its wine binary by probing known layouts under each
/// search dir. `exists` reports file presence (injected for testability). GE-
/// Proton/Proton builds keep wine at `files/bin/wine`; plain Lutris wine builds at
/// `bin/wine`. Returns the first hit; `None` → caller falls back to system `wine`.
#[must_use]
pub fn locate_runner_wine(
    runner: &str,
    search_dirs: &[PathBuf],
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    for dir in search_dirs {
        let base = dir.join(runner);
        let candidates = [
            base.join("files").join("bin").join("wine"),
            base.join("bin").join("wine"),
            base.join("files").join("bin").join("wine64"),
        ];
        if let Some(hit) = candidates.into_iter().find(|c| exists(c)) {
            return Some(hit);
        }
    }
    None
}

/// Assemble a spawnable [`WineLaunch`] from a parsed [`LutrisLaunch`], the exe to
/// run, the caller's full arg list (game args + any dynamic `-ncpconnect=…`), and
/// the resolved runner wine binary (`None` → system `wine`).
///
/// The `WINEPREFIX` is taken from `cfg.prefix` (authoritative). If the config
/// lacks one, we fall back to deriving it from the exe (in-`drive_c` installs),
/// then to the exe's folder — so a sparse config still launches.
#[must_use]
pub fn plan_lutris_launch(
    cfg: &LutrisLaunch,
    exe: &Path,
    args: &[String],
    wine_bin: Option<&Path>,
) -> WineLaunch {
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(exe.to_string_lossy().into_owned());
    argv.extend(args.iter().cloned());
    let wineprefix = cfg.prefix.clone().unwrap_or_else(|| {
        wine_prefix_of(exe).unwrap_or_else(|| exe.parent().unwrap_or(exe).to_path_buf())
    });
    let program = wine_bin
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "wine".to_string());
    WineLaunch {
        program,
        args: argv,
        wineprefix,
        cwd: exe.parent().unwrap_or(exe).to_path_buf(),
        env: cfg.env.clone(),
        wrapper: cfg.prefix_command.clone(),
    }
}

/// Resolve a runner name to a wine binary on this machine (real filesystem).
#[cfg(not(windows))]
#[must_use]
pub fn resolve_runner_wine(runner: &str) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    locate_runner_wine(runner, &runner_search_dirs(&home), |p| p.is_file())
}

/// Compare two paths, canonicalising through symlinks/`.`/`..` when both resolve.
#[cfg(not(windows))]
fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    matches!(
        (std::fs::canonicalize(a), std::fs::canonicalize(b)),
        (Ok(ca), Ok(cb)) if ca == cb
    )
}

/// Find the Lutris config whose `game.exe` matches `exe` and assemble a ready-to-
/// spawn [`WineLaunch`] for it (runner wine + explicit prefix + env + wrapper).
/// `None` when no Lutris config matches — the caller then tries the in-prefix wine
/// plan, then a direct spawn.
#[cfg(not(windows))]
#[must_use]
pub fn find_lutris_launch(exe: &Path, args: &[String]) -> Option<WineLaunch> {
    for path in lutris_game_ymls() {
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let cfg = parse_lutris_launch(&contents);
        let Some(cfg_exe) = cfg.exe.as_deref() else {
            continue;
        };
        if !same_path(cfg_exe, exe) {
            continue;
        }
        let wine_bin = cfg.runner.as_deref().and_then(resolve_runner_wine);
        return Some(plan_lutris_launch(&cfg, exe, args, wine_bin.as_deref()));
    }
    None
}

/// `(install, launch-profile)` hits from Lutris configs, for [`crate::install::
/// detect_installs`]. Each resolvable game becomes one hit; the profile carries
/// the Lutris-configured `game.args` (falling back to the install default).
#[cfg(not(windows))]
#[must_use]
pub fn detect_lutris_hits(
    mod_paks_dir: &Path,
) -> Vec<(crate::install::UtInstall, crate::install::LaunchProfile)> {
    let mut hits = Vec::new();
    for path in lutris_game_ymls() {
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let cfg = parse_lutris_launch(&contents);
        let Some(probe) = cfg
            .exe
            .clone()
            .or_else(|| cfg.prefix.as_ref().map(|p| p.join(DRIVE_C)))
        else {
            continue;
        };
        let Some(install) = crate::install::check_install(&probe, mod_paks_dir.to_path_buf())
        else {
            continue;
        };
        let args = if cfg.args.is_empty() {
            install.launch_args.clone()
        } else {
            cfg.args.clone()
        };
        hits.push((
            install,
            crate::install::LaunchProfile {
                label: "Lutris".to_string(),
                args,
            },
        ));
    }
    hits
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

    // ── in-prefix install paths ──────────────────────────────────────────────

    #[test]
    fn ut4_root_from_prefix_is_the_in_prefix_windows_location() {
        assert_eq!(
            ut4_root_from_prefix(Path::new("/home/barry/Games/ut")),
            PathBuf::from("/home/barry/Games/ut/drive_c/Program Files/UnrealTournament")
        );
    }

    #[test]
    fn plugin_install_path_is_the_canonical_in_prefix_plugin_dir() {
        let prefix = Path::new("/home/barry/Games/ut");
        let plugin = plugin_install_path(prefix);
        assert_eq!(
            plugin,
            PathBuf::from(
                "/home/barry/Games/ut/drive_c/Program Files/UnrealTournament/UnrealTournament/Plugins/NetcodePlus"
            )
        );
        // Genuinely inside the prefix (not an absolute path elsewhere).
        assert!(plugin.starts_with(prefix));
    }

    #[test]
    fn plugin_install_path_round_trips_back_to_the_prefix() {
        // The plugin dir is inside the prefix, so wine_prefix_of recovers it.
        let prefix = PathBuf::from("/p/games/ut4");
        assert_eq!(wine_prefix_of(&plugin_install_path(&prefix)), Some(prefix));
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

    #[test]
    fn games_dirs_cover_config_and_data_locations() {
        let dirs = lutris_games_dirs(Path::new("/home/barry"));
        // The classic ~/.config location AND the ~/.local/share location (the
        // dogfooding box only has the latter) must both be searched.
        assert!(dirs.contains(&PathBuf::from("/home/barry/.config/lutris/games")));
        assert!(dirs.contains(&PathBuf::from("/home/barry/.local/share/lutris/games")));
    }

    // ── Lutris launch fidelity ───────────────────────────────────────────────

    /// The REAL config from the dogfooding box (`ut4-1780693210.yml`): the game
    /// installed OUTSIDE the prefix, a GE-Proton runner under Steam's
    /// compatibilitytools.d, a `taskset` prefix_command, and no `system.env`.
    const REAL_LUTRIS_YML: &str = "\
game:
  args: UnrealTournament -epicapp=UnrealTournamentDev -epicenv=Prod -EpicPortal
  exe: /home/jeremy/Games/UT4/Engine/Binaries/Win64/UE4-Win64-Shipping.exe
  prefix: /home/jeremy/Games/ut4-prefix
  working_dir: ''
system:
  prefix_command: taskset -c 0-5,12-17
  resolution: 2560x1440
wine:
  battleye: false
  eac: false
  fsr: false
  version: GE-Proton10-34
";

    #[test]
    fn parses_the_real_dogfood_lutris_config() {
        let c = parse_lutris_launch(REAL_LUTRIS_YML);
        assert_eq!(
            c.exe.as_deref(),
            Some(Path::new(
                "/home/jeremy/Games/UT4/Engine/Binaries/Win64/UE4-Win64-Shipping.exe"
            ))
        );
        assert_eq!(
            c.args,
            vec![
                "UnrealTournament",
                "-epicapp=UnrealTournamentDev",
                "-epicenv=Prod",
                "-EpicPortal"
            ]
        );
        assert_eq!(
            c.prefix.as_deref(),
            Some(Path::new("/home/jeremy/Games/ut4-prefix"))
        );
        assert_eq!(c.runner.as_deref(), Some("GE-Proton10-34"));
        assert_eq!(c.prefix_command, vec!["taskset", "-c", "0-5,12-17"]);
        assert!(c.env.is_empty(), "real config has no system.env");
    }

    #[test]
    fn parses_system_env_and_keeps_it_out_of_game_fields() {
        // A config WITH a system.env block (DXVK/esync toggles) — the env keys
        // must be captured, and an `exe:` nested under system.env must NOT leak
        // into game.exe (that key belongs to a different block/depth).
        let yml = "\
game:
  exe: /games/ut/UE4.exe
  prefix: /games/ut-prefix
system:
  env:
    DXVK_HUD: fps
    WINEESYNC: '1'
    exe: /should/not/leak
  prefix_command: gamemoderun
wine:
  version: lutris-GE-Proton8-26-x86_64
";
        let c = parse_lutris_launch(yml);
        assert_eq!(c.exe.as_deref(), Some(Path::new("/games/ut/UE4.exe")));
        assert_eq!(
            c.env,
            vec![
                ("DXVK_HUD".to_string(), "fps".to_string()),
                ("WINEESYNC".to_string(), "1".to_string()),
                ("exe".to_string(), "/should/not/leak".to_string()),
            ],
            "env captures every key under system.env, quotes stripped"
        );
        assert_eq!(c.prefix_command, vec!["gamemoderun"]);
        assert_eq!(c.runner.as_deref(), Some("lutris-GE-Proton8-26-x86_64"));
    }

    #[test]
    fn locate_runner_wine_prefers_proton_files_bin_layout() {
        let dirs = vec![
            PathBuf::from("/home/j/.local/share/lutris/runners/wine"),
            PathBuf::from("/home/j/.local/share/Steam/compatibilitytools.d"),
        ];
        let wanted = PathBuf::from(
            "/home/j/.local/share/Steam/compatibilitytools.d/GE-Proton10-34/files/bin/wine",
        );
        let found = locate_runner_wine("GE-Proton10-34", &dirs, |p| p == wanted);
        assert_eq!(found, Some(wanted));
    }

    #[test]
    fn locate_runner_wine_finds_plain_lutris_bin_layout() {
        let dirs = vec![PathBuf::from("/r/wine")];
        let wanted = PathBuf::from("/r/wine/lutris-7.2/bin/wine");
        assert_eq!(
            locate_runner_wine("lutris-7.2", &dirs, |p| p == wanted),
            Some(wanted)
        );
    }

    #[test]
    fn locate_runner_wine_none_when_missing() {
        let dirs = vec![PathBuf::from("/r/wine")];
        assert_eq!(locate_runner_wine("nope", &dirs, |_| false), None);
    }

    #[test]
    fn runner_search_dirs_include_lutris_and_steam_compat() {
        let dirs = runner_search_dirs(Path::new("/home/j"));
        assert!(dirs.contains(&PathBuf::from("/home/j/.local/share/lutris/runners/wine")));
        assert!(dirs.contains(&PathBuf::from(
            "/home/j/.local/share/Steam/compatibilitytools.d"
        )));
    }

    #[test]
    fn plan_lutris_launch_uses_the_explicit_prefix_not_exe_derivation() {
        // The whole point: the exe is NOT inside a drive_c, so deriving the prefix
        // from it would fail — the plan must take WINEPREFIX from game.prefix.
        let cfg = parse_lutris_launch(REAL_LUTRIS_YML);
        let exe = Path::new("/home/jeremy/Games/UT4/Engine/Binaries/Win64/UE4-Win64-Shipping.exe");
        assert_eq!(
            wine_prefix_of(exe),
            None,
            "sanity: exe is not inside a drive_c prefix"
        );
        let connect = vec!["-ncpconnect=1.2.3.4:7777".to_string()];
        let all_args: Vec<String> = cfg.args.iter().cloned().chain(connect).collect();
        let wine = PathBuf::from(
            "/home/jeremy/.local/share/Steam/compatibilitytools.d/GE-Proton10-34/files/bin/wine",
        );
        let plan = plan_lutris_launch(&cfg, exe, &all_args, Some(&wine));

        assert_eq!(plan.program, wine.to_string_lossy());
        assert_eq!(
            plan.wineprefix,
            PathBuf::from("/home/jeremy/Games/ut4-prefix")
        );
        assert_eq!(plan.args[0], exe.to_string_lossy());
        assert_eq!(plan.args.last().unwrap(), "-ncpconnect=1.2.3.4:7777");
        assert_eq!(plan.wrapper, vec!["taskset", "-c", "0-5,12-17"]);
        assert_eq!(plan.cwd, exe.parent().unwrap());
    }

    #[test]
    fn plan_lutris_launch_falls_back_to_system_wine_without_a_runner() {
        let cfg = LutrisLaunch {
            prefix: Some(PathBuf::from("/p")),
            ..Default::default()
        };
        let plan = plan_lutris_launch(&cfg, Path::new("/g/UE4.exe"), &[], None);
        assert_eq!(plan.program, "wine");
        assert_eq!(plan.wineprefix, PathBuf::from("/p"));
        assert!(plan.wrapper.is_empty());
    }
}
