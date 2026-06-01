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
//! This module finds those stray copies so the UI can warn and offer a guarded
//! one-click removal. Detection is read-only; removal lives in
//! [`remove_stray`] and is only ever invoked after explicit user confirmation.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
/// repopulate the correct folder).
///
/// # Errors
/// [`StrayRemoveError::NotAStray`] if the path does not match a recomputed
/// stray location, or [`StrayRemoveError::Io`] on a filesystem error.
pub fn remove_stray(root: &Path, stray: &StrayPlugin) -> Result<(), StrayRemoveError> {
    let expected = match stray.kind {
        StrayKind::EnginePlugins => engine_plugins_dir(root),
        StrayKind::NestedTooDeep => game_plugins_dir(root)
            .join("NetcodePlus")
            .join("NetcodePlus"),
        StrayKind::LooseInPluginsRoot => game_plugins_dir(root).join("NetcodePlus.uplugin"),
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
}
