//! Tauri command handlers for **editor install** management (Phase 0).
//!
//! Register, list, rename, launch, and forget UT4 editor installs. Each is a
//! thin wrapper over [`ncp_host::editor`], persisting the registry in the same
//! `state.json` the rest of the launcher uses. Every command re-derives paths
//! from the install `root` (never trusts a webview-supplied exe path), mirroring
//! `commands::launch_game_elevated`.

use std::path::Path;

use ncp_host::EditorInstall;

use crate::commands::state_path;

/// List the registered editor installs, sorted by label (case-insensitive).
/// Empty until the user registers one.
#[tauri::command]
pub fn list_editor_installs(app: tauri::AppHandle) -> Result<Vec<EditorInstall>, String> {
    let path = state_path(&app)?;
    let state = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let mut installs: Vec<EditorInstall> = state.editor_installs.into_values().collect();
    installs.sort_by_key(|e| e.label.to_lowercase());
    Ok(installs)
}

/// Register the editor install rooted at (or above) `path`.
///
/// Validates the folder is a UT4 editor tree, reads its engine BuildId/CL, and
/// persists it keyed by root. Re-registering an existing root refreshes the
/// engine stamp while preserving its original registration time, last-sync
/// record, custom launch args, and label (unless a new `label` is given).
#[tauri::command]
pub fn add_editor_install(
    app: tauri::AppHandle,
    path: String,
    label: Option<String>,
) -> Result<EditorInstall, String> {
    let mut inst = ncp_host::check_editor_install(Path::new(&path)).map_err(|e| e.to_string())?;
    let key = inst.root.to_string_lossy().to_string();

    let state_file = state_path(&app)?;
    let mut state = ncp_host::state::read(&state_file)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    // Preserve stable fields when re-registering the same root.
    if let Some(existing) = state.editor_installs.get(&key) {
        inst.added_at_ms = existing.added_at_ms;
        inst.last_sync_at_ms = existing.last_sync_at_ms;
        inst.launch_args = existing.launch_args.clone();
        inst.label = existing.label.clone();
    }
    // An explicit label always wins.
    if let Some(l) = label {
        let l = l.trim();
        if !l.is_empty() {
            inst.label = l.to_string();
        }
    }

    state.editor_installs.insert(key, inst.clone());
    ncp_host::state::write(&state_file, &state).map_err(|e| e.to_string())?;
    Ok(inst)
}

/// Forget an editor install (registry only — never touches files on disk).
#[tauri::command]
pub fn remove_editor_install(app: tauri::AppHandle, root: String) -> Result<(), String> {
    let state_file = state_path(&app)?;
    let mut state = ncp_host::state::read(&state_file)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    state.editor_installs.remove(&root);
    ncp_host::state::write(&state_file, &state).map_err(|e| e.to_string())
}

/// Rename a registered editor install.
#[tauri::command]
pub fn set_editor_label(app: tauri::AppHandle, root: String, label: String) -> Result<(), String> {
    let label = label.trim().to_string();
    if label.is_empty() {
        return Err("the editor name can't be empty".into());
    }
    let state_file = state_path(&app)?;
    let mut state = ncp_host::state::read(&state_file)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let inst = state
        .editor_installs
        .get_mut(&root)
        .ok_or("that editor install isn't registered")?;
    inst.label = label;
    ncp_host::state::write(&state_file, &state).map_err(|e| e.to_string())
}

/// Launch a registered editor install.
///
/// Re-validates the `root` from scratch (so a moved/deleted editor fails cleanly
/// rather than launching a stale path), then spawns `UE4Editor.exe` with the
/// standard args — honouring any saved per-install launch-arg override.
#[tauri::command]
pub fn launch_editor_install(app: tauri::AppHandle, root: String) -> Result<(), String> {
    let inst = ncp_host::check_editor_install(Path::new(&root)).map_err(|e| e.to_string())?;

    // Prefer the registered install's saved launch args (a future per-install
    // override); fall back to the freshly-derived defaults.
    let saved_args = state_path(&app)
        .ok()
        .and_then(|p| ncp_host::state::read(&p).ok().flatten())
        .and_then(|s| s.editor_installs.get(&root).map(|e| e.launch_args.clone()));

    let inst = EditorInstall {
        launch_args: saved_args.unwrap_or(inst.launch_args),
        ..inst
    };
    ncp_host::launch_editor_install(&inst).map_err(|e| format!("couldn't start the editor: {e}"))
}
