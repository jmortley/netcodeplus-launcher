//! UT4 **editor** install detection, registration, and launch.
//!
//! An *editor* install is distinct from a *play* install (a stock shipping
//! client — see [`crate::install`]). It is launched through `UE4Editor.exe` and
//! is the tree a mapper or plugin author actually authors in. The canonical case
//! is the two-tree setup: build the UT4 fork in one tree, but run the editor from
//! a separate stock install that carries its own `Plugins/` and `Content/`.
//!
//! Layout on disk:
//!
//! ```text
//! <root>/
//!   Engine/Binaries/Win64/UE4Editor.exe          <- the executable we launch
//!   Engine/Binaries/Win64/UE4Editor.modules      <- engine BuildId + Changelist
//!   UnrealTournament/UnrealTournament.uproject    <- the project
//! ```
//!
//! Play-install detection deliberately *excludes* editor trees (it keys on the
//! shipping exe / a `UE4Editor.exe` shortcut is skipped — see
//! [`crate::install::play_install_from_shortcut`]), so this module is orthogonal:
//! registering an editor install never collides with `detect_installs`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::debug;

/// The UT4 project/game folder name, shared with [`crate::install`].
const GAME_NAME: &str = "UnrealTournament";

/// A registered UT4 editor install the launcher can inventory and launch.
///
/// Persisted in [`crate::state::LauncherState::editor_installs`], keyed by
/// [`Self::root`] as a string. Every added-later field carries `#[serde(default)]`
/// so an older state file loads cleanly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorInstall {
    /// The install root — the parent of `Engine/` and `UnrealTournament/`
    /// (e.g. `C:\LAEditorUT4\UnrealTournamentEditor`).
    pub root: PathBuf,

    /// User-facing name. Defaults to the root folder name; renamable.
    pub label: String,

    /// `<root>/Engine/Binaries/Win64/UE4Editor.exe` — the only exe
    /// [`launch_editor_install`] will start.
    pub editor_exe: PathBuf,

    /// `<root>/UnrealTournament/UnrealTournament.uproject`.
    pub project: PathBuf,

    /// Engine build GUID parsed from `Engine/Binaries/Win64/UE4Editor.modules`.
    /// The compatibility key a synced plugin's own `.modules` is matched against
    /// (Phase 1). `None` if the file is absent or unparseable.
    #[serde(default)]
    pub engine_build_id: Option<String>,

    /// Engine changelist from the same `.modules` file. Editor installs carry no
    /// `Engine/Build/Build.version`, so this is the CL source for an install.
    #[serde(default)]
    pub engine_changelist: Option<u64>,

    /// Launch arguments — defaults to [`default_editor_args`]; stored so a future
    /// per-install override survives.
    pub launch_args: Vec<String>,

    /// When the install was registered (whole ms since the Unix epoch), matching
    /// the timestamp convention of [`crate::state::PakStamp`]. `0` on old records.
    #[serde(default)]
    pub added_at_ms: u64,

    /// When plugins were last synced into this install (Phase 1). `None` until a
    /// sync runs.
    #[serde(default)]
    pub last_sync_at_ms: Option<u64>,
}

/// Why a picked folder was rejected as an editor install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorError {
    /// Neither the picked path nor any ancestor is a UT4 editor root (no
    /// `UE4Editor.exe` + `.uproject` pair found).
    NotEditorRoot(PathBuf),
}

impl std::fmt::Display for EditorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditorError::NotEditorRoot(p) => write!(
                f,
                "{} isn't a UT4 editor install (no UE4Editor.exe + UnrealTournament.uproject)",
                p.display()
            ),
        }
    }
}

impl std::error::Error for EditorError {}

/// `<root>/Engine/Binaries/Win64/UE4Editor.exe`.
#[must_use]
pub fn editor_exe(root: &Path) -> PathBuf {
    root.join("Engine")
        .join("Binaries")
        .join("Win64")
        .join("UE4Editor.exe")
}

/// `<root>/UnrealTournament/UnrealTournament.uproject`.
#[must_use]
fn uproject(root: &Path) -> PathBuf {
    root.join(GAME_NAME).join(format!("{GAME_NAME}.uproject"))
}

/// `<root>/Engine/Binaries/Win64/UE4Editor.modules` — the engine module manifest
/// carrying the canonical `BuildId` + `Changelist` for this install.
#[must_use]
fn engine_modules(root: &Path) -> PathBuf {
    root.join("Engine")
        .join("Binaries")
        .join("Win64")
        .join("UE4Editor.modules")
}

/// A UT4 editor install root: `UE4Editor.exe` under `Engine/Binaries/Win64/` AND
/// the `UnrealTournament.uproject`. Requiring both separates a real editor tree
/// from a bare UE4 editor or a play install (which has no `.uproject` and a
/// shipping exe, not `UE4Editor.exe`).
#[must_use]
fn is_editor_root(root: &Path) -> bool {
    editor_exe(root).is_file() && uproject(root).is_file()
}

/// If `dir` isn't itself an editor root but exactly one of its immediate children
/// is, return that child. Handles the common case of picking the *parent* folder
/// that contains the editor tree (e.g. `C:\LAEditorUT4` holding
/// `C:\LAEditorUT4\UnrealTournamentEditor`). Returns `None` for zero matches or
/// two-plus (ambiguous — the caller should pick the specific one).
fn find_child_editor_root(dir: &Path) -> Option<PathBuf> {
    let mut found: Option<PathBuf> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let child = entry.path();
        if child.is_dir() && is_editor_root(&child) {
            if found.is_some() {
                return None; // more than one editor install under `dir` — ambiguous
            }
            found = Some(child);
        }
    }
    found
}

/// The standard UT4 editor launch arguments — the community's long-standing
/// shortcut convention: the project name (a bare `UE4Editor.exe` opens a project
/// browser instead of UT4), a log window, no shared DDC, and the D3D11/SM5 path
/// this 4.15-era editor is happiest on. Mirrors `installer::EDITOR_ARGS`.
#[must_use]
pub fn default_editor_args() -> Vec<String> {
    vec![
        GAME_NAME.to_string(),
        "-log".to_string(),
        "-ddc=noshared".to_string(),
        "-d3d11".to_string(),
        "-sm5".to_string(),
    ]
}

/// Read `(BuildId, Changelist)` from `<root>/Engine/Binaries/Win64/UE4Editor.modules`.
/// Both are `None` if the file is missing or not the expected JSON — a missing
/// stamp is informational only (it never blocks registration or launch).
#[must_use]
pub fn read_engine_stamp(root: &Path) -> (Option<String>, Option<u64>) {
    let Ok(bytes) = std::fs::read(engine_modules(root)) else {
        return (None, None);
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return (None, None);
    };
    let build_id = value
        .get("BuildId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let changelist = value.get("Changelist").and_then(serde_json::Value::as_u64);
    (build_id, changelist)
}

/// Whole milliseconds since the Unix epoch, matching [`crate::state::PakStamp`]'s
/// timestamp convention. `0` if the clock is before the epoch (never in practice).
#[must_use]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Validate that `picked` resolves to a UT4 editor install root, returning a
/// fully-populated [`EditorInstall`].
///
/// Resolution tries, in order: `picked` itself or any **ancestor** (so picking a
/// subfolder such as the `UnrealTournament\` project dir still works, like
/// [`crate::install::check_install`]); then one level **down** — a lone direct
/// child of `picked` that is an editor root (so picking the *parent* folder that
/// holds the editor tree, e.g. `C:\LAEditorUT4` containing `…\UnrealTournamentEditor`,
/// also works). The label defaults to the root folder name.
///
/// # Errors
///
/// [`EditorError::NotEditorRoot`] if neither `picked`, an ancestor, nor a single
/// direct child is a UT4 editor root (a parent holding *several* editor installs
/// is treated as not-a-root — pick the specific one).
pub fn check_editor_install(picked: &Path) -> Result<EditorInstall, EditorError> {
    let root: PathBuf = picked
        .ancestors()
        .find(|p| is_editor_root(p))
        .map(Path::to_path_buf)
        .or_else(|| find_child_editor_root(picked))
        .ok_or_else(|| EditorError::NotEditorRoot(picked.to_path_buf()))?;

    let (engine_build_id, engine_changelist) = read_engine_stamp(&root);
    let label = root
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("UT4 Editor")
        .to_string();

    debug!(
        root = %root.display(),
        build_id = ?engine_build_id,
        changelist = ?engine_changelist,
        "validated editor install"
    );

    Ok(EditorInstall {
        root: root.clone(),
        label,
        editor_exe: editor_exe(&root),
        project: uproject(&root),
        engine_build_id,
        engine_changelist,
        launch_args: default_editor_args(),
        added_at_ms: now_ms(),
        last_sync_at_ms: None,
    })
}

/// Start the editor for `inst`: a plain, user-level spawn of [`EditorInstall::editor_exe`]
/// with its [`EditorInstall::launch_args`], cwd set to the exe's folder. No
/// elevation (the editor is a normal user program), mirroring `installer::launch_editor`.
///
/// # Errors
///
/// The underlying [`std::process::Command::spawn`] error if the exe is missing or
/// cannot be started.
pub fn launch_editor_install(inst: &EditorInstall) -> std::io::Result<()> {
    let work = inst.editor_exe.parent().unwrap_or(inst.root.as_path());
    std::process::Command::new(&inst.editor_exe)
        .args(&inst.launch_args)
        .current_dir(work)
        .spawn()?;
    debug!(root = %inst.root.display(), "launched editor install");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Lay down a minimal editor tree at `root`: the editor exe, the project, and
    /// (optionally) an engine `.modules` carrying a BuildId + Changelist.
    fn build_editor_install(root: &Path, modules: Option<&str>) {
        let win64 = root.join("Engine").join("Binaries").join("Win64");
        fs::create_dir_all(&win64).unwrap();
        fs::write(win64.join("UE4Editor.exe"), b"fake editor").unwrap();
        if let Some(json) = modules {
            fs::write(win64.join("UE4Editor.modules"), json).unwrap();
        }
        let proj = root.join(GAME_NAME);
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("UnrealTournament.uproject"), b"{}").unwrap();
    }

    #[test]
    fn check_resolves_a_valid_editor_root_with_engine_stamp() {
        let tmp = TempDir::new().unwrap();
        build_editor_install(
            tmp.path(),
            Some(
                r#"{"Changelist":3525360,"BuildId":"7a4ea563-ab00-4f14-8e9c-20ca0c9aa851","Modules":{}}"#,
            ),
        );

        let inst = check_editor_install(tmp.path()).expect("valid editor root should resolve");
        assert_eq!(inst.root, tmp.path());
        assert_eq!(inst.editor_exe, editor_exe(tmp.path()));
        assert_eq!(
            inst.engine_build_id.as_deref(),
            Some("7a4ea563-ab00-4f14-8e9c-20ca0c9aa851")
        );
        assert_eq!(inst.engine_changelist, Some(3525360));
        assert_eq!(
            inst.launch_args.first().map(String::as_str),
            Some(GAME_NAME)
        );
        assert!(inst.launch_args.contains(&"-ddc=noshared".to_string()));
    }

    #[test]
    fn check_climbs_up_from_a_subfolder_to_the_root() {
        let tmp = TempDir::new().unwrap();
        build_editor_install(tmp.path(), None);
        // Pick the project subfolder; must resolve up to the real root.
        let picked = tmp.path().join(GAME_NAME);
        let inst = check_editor_install(&picked).expect("should climb to the editor root");
        assert_eq!(inst.root, tmp.path());
        // No .modules laid down → stamp is absent, but registration still succeeds.
        assert!(inst.engine_build_id.is_none());
        assert!(inst.engine_changelist.is_none());
    }

    #[test]
    fn check_descends_one_level_into_a_parent_folder() {
        // Pick the PARENT (like C:\LAEditorUT4) — resolve into the sole child root
        // (C:\LAEditorUT4\UnrealTournamentEditor). This is the real-world case.
        let tmp = TempDir::new().unwrap();
        let editor_root = tmp.path().join("UnrealTournamentEditor");
        build_editor_install(&editor_root, None);
        let inst = check_editor_install(tmp.path())
            .expect("should descend into the single child editor root");
        assert_eq!(inst.root, editor_root);
    }

    #[test]
    fn check_parent_with_several_editor_children_is_ambiguous() {
        // Two editor installs under the picked parent → ambiguous, rejected so the
        // user picks the specific one rather than getting an arbitrary pick.
        let tmp = TempDir::new().unwrap();
        build_editor_install(&tmp.path().join("EditorA"), None);
        build_editor_install(&tmp.path().join("EditorB"), None);
        assert!(matches!(
            check_editor_install(tmp.path()),
            Err(EditorError::NotEditorRoot(_))
        ));
    }

    #[test]
    fn check_rejects_a_non_editor_tree() {
        // Has the exe but no .uproject → not an editor install.
        let tmp = TempDir::new().unwrap();
        let win64 = tmp.path().join("Engine").join("Binaries").join("Win64");
        fs::create_dir_all(&win64).unwrap();
        fs::write(win64.join("UE4Editor.exe"), b"fake editor").unwrap();
        assert!(matches!(
            check_editor_install(tmp.path()),
            Err(EditorError::NotEditorRoot(_))
        ));
    }

    #[test]
    fn read_engine_stamp_tolerates_missing_and_garbage() {
        let tmp = TempDir::new().unwrap();
        // Missing file.
        assert_eq!(read_engine_stamp(tmp.path()), (None, None));
        // Garbage, non-JSON.
        let win64 = tmp.path().join("Engine").join("Binaries").join("Win64");
        fs::create_dir_all(&win64).unwrap();
        fs::write(win64.join("UE4Editor.modules"), b"not json {{").unwrap();
        assert_eq!(read_engine_stamp(tmp.path()), (None, None));
    }

    #[test]
    fn editor_install_round_trips_through_json() {
        let tmp = TempDir::new().unwrap();
        build_editor_install(tmp.path(), None);
        let inst = check_editor_install(tmp.path()).unwrap();
        let json = serde_json::to_string(&inst).unwrap();
        let back: EditorInstall = serde_json::from_str(&json).unwrap();
        assert_eq!(inst, back);
    }

    #[test]
    fn editor_install_deserializes_without_the_optional_fields() {
        // A record written before engine_build_id / added_at_ms existed must load.
        let json = r#"{
            "root":"C:\\LAEditorUT4\\UnrealTournamentEditor",
            "label":"LAEditor",
            "editor_exe":"C:\\LAEditorUT4\\UnrealTournamentEditor\\Engine\\Binaries\\Win64\\UE4Editor.exe",
            "project":"C:\\LAEditorUT4\\UnrealTournamentEditor\\UnrealTournament\\UnrealTournament.uproject",
            "launch_args":["UnrealTournament","-log"]
        }"#;
        let inst: EditorInstall = serde_json::from_str(json).unwrap();
        assert_eq!(inst.label, "LAEditor");
        assert!(inst.engine_build_id.is_none());
        assert_eq!(inst.added_at_ms, 0);
        assert!(inst.last_sync_at_ms.is_none());
    }
}
