//! Detect stray / misplaced NetcodePlus copies inside a UT4 install.
//!
//! The canonical, engine-correct install location is
//! `<root>/UnrealTournament/Plugins/NetcodePlus/` (see
//! [`crate::install::netcodeplus_dir`]). But a hand-installing player can put
//! the plugin somewhere the engine ALSO loads from — most dangerously
//! `<root>/Engine/Plugins/NetcodePlus/` — which produces **two** NetcodePlus
//! copies loading at once: version conflicts, double-registration, the wrong
//! build winning. The launcher reports the canonical slot as `Missing` and
//! would happily install a second copy, leaving the stray to keep loading.
//!
//! It also finds **misplaced content paks**: the only `.pak` that belongs in
//! `<root>/UnrealTournament/Content/Paks/` is the game's own stock pak (the cooked
//! `UnrealTournament-WindowsNoEditor.pak`; see [`ALLOWED_CONTENT_PAKS`]).
//! Players sometimes drop mod/content paks there by hand (they belong in
//! `Saved/Paks/DownloadedPaks/`, which the launcher manages) — an extra pak in the
//! shipped-content folder loads unconditionally and causes hard-to-diagnose
//! content conflicts and crashes.
//!
//! Finally it finds **leftover install dirs** — `.NetcodePlus.old.*` (a moved-
//! aside previous build) or `.NetcodePlus.staging.*` (an interrupted extraction)
//! in `<root>/UnrealTournament/Plugins/`. The installer normally clears these, but
//! one can linger when it was file-locked by a running game; surfacing it here
//! gives a way to clear it WITHOUT triggering a full plugin reinstall.
//!
//! This module finds those stray copies so the UI can warn and offer a guarded
//! one-click removal. Detection is read-only; removal lives in
//! [`remove_stray`] and is only ever invoked after explicit user confirmation.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The `.pak` filenames that legitimately live in the game's `Content/Paks/`
/// folder. Every OTHER `.pak` there is a misplaced content pak.
///
/// The real stock pak is the cooked name **`UnrealTournament-WindowsNoEditor.pak`**
/// (verified against a live install) — NOT `UnrealTournament.pak`. That distinction
/// is load-bearing: this list is an allowlist, so a wrong/short name here would make
/// the scanner flag the game's OWN pak as a stray and offer to delete it. The plain
/// `UnrealTournament.pak` is kept too in case an editor/other build variant uses it.
/// Compared case-insensitively; extend this if a variant ships another stock pak.
const ALLOWED_CONTENT_PAKS: &[&str] = &[
    "UnrealTournament-WindowsNoEditor.pak",
    "UnrealTournament.pak",
];

/// Why a found path is considered a stray NetcodePlus copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrayKind {
    /// `<root>/Engine/Plugins/NetcodePlus` — the engine loads plugins from
    /// `Engine/Plugins`, so this loads IN ADDITION to a correct install: the
    /// double-load case. The worst one.
    EnginePlugins,
    /// `<root>/UnrealTournament/Plugins/NetcodePlus/NetcodePlus` — extracted one
    /// level too deep; the engine won't load it, and it makes the canonical
    /// slot look `Malformed`.
    NestedTooDeep,
    /// A loose `NetcodePlus.uplugin` sitting directly in
    /// `<root>/UnrealTournament/Plugins/` (archive contents dumped without the
    /// `NetcodePlus/` folder). Not engine-correct.
    LooseInPluginsRoot,
    /// A `.pak` other than a stock game pak ([`ALLOWED_CONTENT_PAKS`]) sitting
    /// directly in `<root>/UnrealTournament/Content/Paks/` — a mod pak hand-dropped
    /// into the shipped-content folder (it belongs in `DownloadedPaks/`). Loads
    /// unconditionally and causes content conflicts / crashes. Unlike the other
    /// kinds this is not a single fixed path — the [`StrayPlugin::path`] names the
    /// specific offending `.pak`.
    ContentPak,
    /// A leftover `.NetcodePlus.old.*` (moved-aside previous build) or
    /// `.NetcodePlus.staging.*` (interrupted extraction) directory in
    /// `<root>/UnrealTournament/Plugins/`. The launcher's own installer normally
    /// clears these, but one can linger if it was file-locked by a running game
    /// (the phantaci report) — and a `.uplugin` still inside it can be picked up
    /// by the engine's recursive plugin scan. Variable path — [`StrayPlugin::path`]
    /// names the specific leftover dir.
    PluginLeftover,
}

impl StrayKind {
    /// A short, non-technical explanation for the UI.
    #[must_use]
    pub fn explanation(self) -> &'static str {
        match self {
            StrayKind::EnginePlugins => {
                "A copy of NetcodePlus is in the engine's plugin folder. The game \
                 loads it from there too, so you can end up running two different \
                 versions at once — which causes version mismatches and crashes."
            }
            StrayKind::NestedTooDeep => {
                "NetcodePlus was unpacked one folder too deep, so the game can't \
                 load it correctly."
            }
            StrayKind::LooseInPluginsRoot => {
                "NetcodePlus files were unpacked loose instead of into their own \
                 NetcodePlus folder, so the game can't load them correctly."
            }
            StrayKind::ContentPak => {
                "A content pak is in the game's Paks folder, where only the game's \
                 own stock pak belongs. Extra paks here load no matter what and can \
                 cause content conflicts and crashes — the launcher keeps mod paks \
                 in the separate DownloadedPaks folder instead."
            }
            StrayKind::PluginLeftover => {
                "A leftover NetcodePlus folder from a previous update is still in \
                 your Plugins folder. It's normally removed automatically, but one \
                 can get left behind if the game was open during an update — it can \
                 clutter or confuse the plugin, so it's safe to remove."
            }
        }
    }
}

/// A stray NetcodePlus copy found in an install, with enough context for the UI
/// to warn and (on confirmation) remove it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrayPlugin {
    /// What kind of misplacement this is.
    pub kind: StrayKind,
    /// The path that should be removed (a directory, except
    /// [`StrayKind::LooseInPluginsRoot`] which reports the `Plugins` dir whose
    /// loose files are the problem — see [`remove_stray`] for how that case is
    /// handled).
    pub path: PathBuf,
}

/// The directories the engine treats as plugin search roots within a UT4
/// install, where a stray `NetcodePlus/` folder would actually load.
fn engine_plugins_dir(root: &Path) -> PathBuf {
    root.join("Engine").join("Plugins").join("NetcodePlus")
}

/// `<root>/UnrealTournament/Plugins` — the correct plugins parent.
fn game_plugins_dir(root: &Path) -> PathBuf {
    root.join("UnrealTournament").join("Plugins")
}

/// Whether `name` is a `.pak` that does NOT belong in the game's `Content/Paks`
/// folder — i.e. a `.pak` (any case) whose filename is not in
/// [`ALLOWED_CONTENT_PAKS`] (compared case-insensitively, since the Windows
/// filesystem is case-insensitive).
fn is_misplaced_content_pak(name: &str) -> bool {
    let is_pak = std::path::Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pak"));
    is_pak
        && !ALLOWED_CONTENT_PAKS
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(name))
}

/// Every misplaced content pak sitting DIRECTLY in `<root>/…/Content/Paks/`
/// (non-recursive), sorted by path for a stable order. The single source of
/// truth for both [`scan_strays`] and the [`remove_stray`] safety re-check, so a
/// removal can only ever target a path this scan actually flags.
fn misplaced_content_paks(root: &Path) -> Vec<PathBuf> {
    let dir = crate::install::ut4_content_paks_dir(root);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paks: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter(|e| is_misplaced_content_pak(&e.file_name().to_string_lossy()))
        .map(|e| e.path())
        .collect();
    paks.sort();
    paks
}

/// Whether `name` is one of the launcher's own leftover install dirs — a
/// moved-aside previous build (`.NetcodePlus.old.*`) or an interrupted
/// extraction (`.NetcodePlus.staging.*`). Mirrors the prefixes
/// `crate::plugin_install::sweep_leftovers` cleans, so the two never disagree.
fn is_plugin_leftover_name(name: &str) -> bool {
    name.starts_with(".NetcodePlus.old.") || name.starts_with(".NetcodePlus.staging.")
}

/// Every leftover `.NetcodePlus.old.*` / `.NetcodePlus.staging.*` DIRECTORY
/// sitting directly in `<root>/UnrealTournament/Plugins/` (non-recursive), sorted
/// for a stable order. Single source of truth for both [`scan_strays`] and the
/// [`remove_stray`] safety re-check.
fn plugin_leftover_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(game_plugins_dir(root)) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| is_plugin_leftover_name(&e.file_name().to_string_lossy()))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs
}

/// Scan a UT4 install `root` for stray / misplaced NetcodePlus copies.
///
/// Read-only. Returns every stray found (there can be more than one). The
/// canonical `<root>/UnrealTournament/Plugins/NetcodePlus/` install is NOT
/// reported here — that is [`crate::install::netcodeplus_status`]'s job.
#[must_use]
pub fn scan_strays(root: &Path) -> Vec<StrayPlugin> {
    let mut strays = Vec::new();

    // 1. Engine/Plugins/NetcodePlus — the double-load case.
    let engine = engine_plugins_dir(root);
    if engine.is_dir() {
        strays.push(StrayPlugin {
            kind: StrayKind::EnginePlugins,
            path: engine,
        });
    }

    let plugins = game_plugins_dir(root);

    // 2. UnrealTournament/Plugins/NetcodePlus/NetcodePlus — nested too deep.
    let nested = plugins.join("NetcodePlus").join("NetcodePlus");
    if nested.is_dir() {
        strays.push(StrayPlugin {
            kind: StrayKind::NestedTooDeep,
            path: nested,
        });
    }

    // 3. A loose NetcodePlus.uplugin directly in UnrealTournament/Plugins/
    //    (no enclosing NetcodePlus/ folder). Report the .uplugin path.
    let loose = plugins.join("NetcodePlus.uplugin");
    if loose.is_file() {
        strays.push(StrayPlugin {
            kind: StrayKind::LooseInPluginsRoot,
            path: loose,
        });
    }

    // 4. Any .pak other than UnrealTournament.pak in Content/Paks — one stray per
    //    offending file (there can be several).
    for pak in misplaced_content_paks(root) {
        strays.push(StrayPlugin {
            kind: StrayKind::ContentPak,
            path: pak,
        });
    }

    // 5. Leftover .NetcodePlus.old.* / .staging.* dirs the installer couldn't
    //    clear (e.g. locked by a running game) — one stray per leftover.
    for leftover in plugin_leftover_dirs(root) {
        strays.push(StrayPlugin {
            kind: StrayKind::PluginLeftover,
            path: leftover,
        });
    }

    strays
}

/// Errors from [`remove_stray`].
#[derive(Debug, thiserror::Error)]
pub enum StrayRemoveError {
    /// The path to remove is not one this module produced — refuse, so a
    /// webview-supplied path can never direct a delete somewhere arbitrary.
    #[error("refused to remove a path that is not a recognised stray location")]
    NotAStray,
    /// Filesystem error during removal.
    #[error("could not remove stray plugin: {0}")]
    Io(#[from] std::io::Error),
}

/// Remove a single stray copy, re-validating that `stray.path` is exactly one
/// of the stray shapes [`scan_strays`] produces for `root` before deleting
/// anything.
///
/// This re-derivation is the safety gate: the caller (a Tauri command) passes a
/// `root` + a `StrayPlugin`, but we DO NOT trust the path — we recompute the
/// expected stray path for the claimed `kind` under `root` and only proceed if
/// it matches exactly. So even a tampered webview payload can only ever delete
/// a genuine stray-NetcodePlus location inside a real install, never an
/// arbitrary path.
///
/// [`StrayKind::EnginePlugins`] and [`StrayKind::NestedTooDeep`] remove a
/// directory tree; [`StrayKind::LooseInPluginsRoot`] removes just the loose
/// `NetcodePlus.uplugin` file (it cannot safely guess which other loose files
/// belonged to the plugin, so it clears the marker and lets a clean install
/// repopulate the correct folder). [`StrayKind::ContentPak`] and
/// [`StrayKind::PluginLeftover`] have VARIABLE paths, so instead of recomputing a
/// fixed location they re-derive the check by re-scanning: the delete proceeds
/// only if the path is one the current scan still flags — ContentPak removes the
/// one offending `.pak` (never the game's own `UnrealTournament.pak`, never a path
/// outside `Content/Paks`); PluginLeftover removes the one leftover dir (only a
/// `.NetcodePlus.old.*` / `.staging.*` directly in `Plugins/`).
///
/// # Errors
/// [`StrayRemoveError::NotAStray`] if the path does not match a recomputed
/// stray location, or [`StrayRemoveError::Io`] on a filesystem error.
pub fn remove_stray(root: &Path, stray: &StrayPlugin) -> Result<(), StrayRemoveError> {
    // ContentPak is not a single fixed location: re-scan Content/Paks and only
    // delete `stray.path` if it is one of the paks that scan CURRENTLY flags as
    // misplaced (right folder, `.pak`, not the allowlisted game pak). Same
    // "recompute, don't trust the webview payload" gate the fixed-path kinds get
    // below — a tampered path can't escape the flagged set.
    if stray.kind == StrayKind::ContentPak {
        if !misplaced_content_paks(root).contains(&stray.path) {
            return Err(StrayRemoveError::NotAStray);
        }
        std::fs::remove_file(&stray.path)?;
        return Ok(());
    }

    // PluginLeftover is also a variable path: re-scan the Plugins folder and only
    // delete `stray.path` if it is one of the leftover dirs scan CURRENTLY flags
    // (`.NetcodePlus.old.*` / `.staging.*` directly in Plugins/). A still file-
    // locked leftover (game open) surfaces the OS error to the UI ("close UT4").
    if stray.kind == StrayKind::PluginLeftover {
        if !plugin_leftover_dirs(root).contains(&stray.path) {
            return Err(StrayRemoveError::NotAStray);
        }
        std::fs::remove_dir_all(&stray.path)?;
        return Ok(());
    }

    let expected = match stray.kind {
        StrayKind::EnginePlugins => engine_plugins_dir(root),
        StrayKind::NestedTooDeep => game_plugins_dir(root)
            .join("NetcodePlus")
            .join("NetcodePlus"),
        StrayKind::LooseInPluginsRoot => game_plugins_dir(root).join("NetcodePlus.uplugin"),
        StrayKind::ContentPak | StrayKind::PluginLeftover => {
            unreachable!("variable-path kinds handled above")
        }
    };
    if stray.path != expected {
        return Err(StrayRemoveError::NotAStray);
    }

    match stray.kind {
        StrayKind::LooseInPluginsRoot => {
            if expected.is_file() {
                std::fs::remove_file(&expected)?;
            }
        }
        _ => {
            if expected.is_dir() {
                std::fs::remove_dir_all(&expected)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn mk_dir(p: &Path) {
        fs::create_dir_all(p).unwrap();
    }

    #[test]
    fn clean_install_has_no_strays() {
        let tmp = TempDir::new().unwrap();
        // Correct install only.
        mk_dir(
            &tmp.path()
                .join("UnrealTournament")
                .join("Plugins")
                .join("NetcodePlus")
                .join("Binaries"),
        );
        assert!(scan_strays(tmp.path()).is_empty());
    }

    #[test]
    fn detects_engine_plugins_stray() {
        let tmp = TempDir::new().unwrap();
        mk_dir(&engine_plugins_dir(tmp.path()));
        let strays = scan_strays(tmp.path());
        assert_eq!(strays.len(), 1);
        assert_eq!(strays[0].kind, StrayKind::EnginePlugins);
    }

    #[test]
    fn detects_nested_too_deep() {
        let tmp = TempDir::new().unwrap();
        mk_dir(
            &game_plugins_dir(tmp.path())
                .join("NetcodePlus")
                .join("NetcodePlus"),
        );
        let strays = scan_strays(tmp.path());
        assert!(strays.iter().any(|s| s.kind == StrayKind::NestedTooDeep));
    }

    #[test]
    fn detects_loose_uplugin() {
        let tmp = TempDir::new().unwrap();
        mk_dir(&game_plugins_dir(tmp.path()));
        fs::write(
            game_plugins_dir(tmp.path()).join("NetcodePlus.uplugin"),
            b"{}",
        )
        .unwrap();
        let strays = scan_strays(tmp.path());
        assert!(strays
            .iter()
            .any(|s| s.kind == StrayKind::LooseInPluginsRoot));
    }

    #[test]
    fn remove_engine_stray_deletes_tree() {
        let tmp = TempDir::new().unwrap();
        let dir = engine_plugins_dir(tmp.path());
        mk_dir(&dir.join("Binaries"));
        let stray = StrayPlugin {
            kind: StrayKind::EnginePlugins,
            path: dir.clone(),
        };
        remove_stray(tmp.path(), &stray).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn remove_rejects_mismatched_path() {
        let tmp = TempDir::new().unwrap();
        // A path that is NOT the recomputed stray location for the claimed kind.
        let evil = StrayPlugin {
            kind: StrayKind::EnginePlugins,
            path: tmp.path().join("Engine").join("Binaries"),
        };
        assert!(matches!(
            remove_stray(tmp.path(), &evil),
            Err(StrayRemoveError::NotAStray)
        ));
    }

    #[test]
    fn remove_loose_deletes_only_the_uplugin() {
        let tmp = TempDir::new().unwrap();
        let plugins = game_plugins_dir(tmp.path());
        mk_dir(&plugins);
        let uplugin = plugins.join("NetcodePlus.uplugin");
        fs::write(&uplugin, b"{}").unwrap();
        let stray = StrayPlugin {
            kind: StrayKind::LooseInPluginsRoot,
            path: uplugin.clone(),
        };
        remove_stray(tmp.path(), &stray).unwrap();
        assert!(!uplugin.exists());
        assert!(plugins.is_dir(), "the Plugins dir itself must survive");
    }

    fn content_paks_dir(root: &Path) -> PathBuf {
        crate::install::ut4_content_paks_dir(root)
    }

    #[test]
    fn detects_misplaced_content_pak_only() {
        let tmp = TempDir::new().unwrap();
        let paks = content_paks_dir(tmp.path());
        mk_dir(&paks);
        // The REAL stock pak is the cooked `-WindowsNoEditor` name (this is the
        // regression guard: the scanner must NOT flag the game's own pak). The
        // plain name is allowlisted too; a `.sig` is not a `.pak`.
        fs::write(paks.join("UnrealTournament-WindowsNoEditor.pak"), b"game").unwrap();
        fs::write(paks.join("UnrealTournament.pak"), b"game2").unwrap();
        fs::write(paks.join("UnrealTournament.sig"), b"sig").unwrap();
        // A hand-dropped mod pak is the only stray.
        fs::write(paks.join("NCWepMut-WindowsNoEditor.pak"), b"mod").unwrap();

        let pak_strays: Vec<_> = scan_strays(tmp.path())
            .into_iter()
            .filter(|s| s.kind == StrayKind::ContentPak)
            .collect();
        assert_eq!(pak_strays.len(), 1, "only the mod pak, never a stock pak");
        assert_eq!(
            pak_strays[0].path,
            paks.join("NCWepMut-WindowsNoEditor.pak")
        );
    }

    #[test]
    fn allowlists_game_pak_case_insensitively_and_catches_uppercase_ext() {
        let tmp = TempDir::new().unwrap();
        let paks = content_paks_dir(tmp.path());
        mk_dir(&paks);
        // Different case of the allowlisted name must NOT be flagged.
        fs::write(paks.join("unrealtournament.pak"), b"game").unwrap();
        // An uppercase `.PAK` extension on a mod pak still counts.
        fs::write(paks.join("Mod-WindowsNoEditor.PAK"), b"mod").unwrap();

        let pak_strays: Vec<_> = scan_strays(tmp.path())
            .into_iter()
            .filter(|s| s.kind == StrayKind::ContentPak)
            .collect();
        assert_eq!(pak_strays.len(), 1);
        assert_eq!(pak_strays[0].path, paks.join("Mod-WindowsNoEditor.PAK"));
    }

    #[test]
    fn remove_content_pak_deletes_only_that_file() {
        let tmp = TempDir::new().unwrap();
        let paks = content_paks_dir(tmp.path());
        mk_dir(&paks);
        let game = paks.join("UnrealTournament-WindowsNoEditor.pak");
        let mod_pak = paks.join("SomeMod-WindowsNoEditor.pak");
        fs::write(&game, b"game").unwrap();
        fs::write(&mod_pak, b"mod").unwrap();

        let stray = StrayPlugin {
            kind: StrayKind::ContentPak,
            path: mod_pak.clone(),
        };
        remove_stray(tmp.path(), &stray).unwrap();
        assert!(!mod_pak.exists(), "the misplaced pak is removed");
        assert!(game.is_file(), "the game's own pak must survive");
    }

    #[test]
    fn remove_content_pak_refuses_the_game_pak() {
        let tmp = TempDir::new().unwrap();
        let paks = content_paks_dir(tmp.path());
        mk_dir(&paks);
        // The real cooked stock pak — a payload claiming it is a stray must be
        // refused (deleting it would brick the install).
        let game = paks.join("UnrealTournament-WindowsNoEditor.pak");
        fs::write(&game, b"game").unwrap();

        let evil = StrayPlugin {
            kind: StrayKind::ContentPak,
            path: game.clone(),
        };
        assert!(matches!(
            remove_stray(tmp.path(), &evil),
            Err(StrayRemoveError::NotAStray)
        ));
        assert!(game.is_file(), "the game pak must not be deleted");
    }

    #[test]
    fn remove_content_pak_refuses_a_path_outside_the_paks_dir() {
        let tmp = TempDir::new().unwrap();
        mk_dir(&content_paks_dir(tmp.path()));
        // A `.pak` path that is NOT inside Content/Paks must be refused, even
        // though the name isn't allowlisted.
        let outside = tmp.path().join("evil.pak");
        fs::write(&outside, b"x").unwrap();
        let evil = StrayPlugin {
            kind: StrayKind::ContentPak,
            path: outside.clone(),
        };
        assert!(matches!(
            remove_stray(tmp.path(), &evil),
            Err(StrayRemoveError::NotAStray)
        ));
        assert!(outside.is_file(), "a path outside Paks must not be deleted");
    }

    #[test]
    fn detects_plugin_leftover_dirs_but_not_a_real_install() {
        let tmp = TempDir::new().unwrap();
        let plugins = game_plugins_dir(tmp.path());
        // The correct install must NOT be flagged as a leftover.
        mk_dir(&plugins.join("NetcodePlus").join("Binaries"));
        // Two leftovers the installer couldn't clear + one unrelated plugin.
        mk_dir(&plugins.join(".NetcodePlus.old.4242"));
        mk_dir(&plugins.join(".NetcodePlus.staging.99"));
        mk_dir(&plugins.join("UltiCross"));

        let leftovers: Vec<_> = scan_strays(tmp.path())
            .into_iter()
            .filter(|s| s.kind == StrayKind::PluginLeftover)
            .map(|s| s.path)
            .collect();
        assert_eq!(leftovers.len(), 2);
        assert!(leftovers.contains(&plugins.join(".NetcodePlus.old.4242")));
        assert!(leftovers.contains(&plugins.join(".NetcodePlus.staging.99")));
    }

    #[test]
    fn remove_plugin_leftover_deletes_only_that_dir() {
        let tmp = TempDir::new().unwrap();
        let plugins = game_plugins_dir(tmp.path());
        let leftover = plugins.join(".NetcodePlus.old.4242");
        mk_dir(&leftover.join("Binaries"));
        let good = plugins.join("NetcodePlus");
        mk_dir(&good.join("Binaries"));

        let stray = StrayPlugin {
            kind: StrayKind::PluginLeftover,
            path: leftover.clone(),
        };
        remove_stray(tmp.path(), &stray).unwrap();
        assert!(!leftover.exists(), "the leftover dir is removed");
        assert!(good.is_dir(), "the real install must survive");
    }

    #[test]
    fn remove_plugin_leftover_refuses_a_non_leftover_dir() {
        let tmp = TempDir::new().unwrap();
        let plugins = game_plugins_dir(tmp.path());
        // The real install dir is not a leftover — a payload naming it is refused.
        let good = plugins.join("NetcodePlus");
        mk_dir(&good.join("Binaries"));
        let evil = StrayPlugin {
            kind: StrayKind::PluginLeftover,
            path: good.clone(),
        };
        assert!(matches!(
            remove_stray(tmp.path(), &evil),
            Err(StrayRemoveError::NotAStray)
        ));
        assert!(good.is_dir(), "the real install must not be deleted");
    }
}
