//! Tauri command handlers for **editor install** management (Phase 0).
//!
//! Register, list, rename, launch, and forget UT4 editor installs. Each is a
//! thin wrapper over [`ncp_host::editor`], persisting the registry in the same
//! `state.json` the rest of the launcher uses. Every command re-derives paths
//! from the install `root` (never trusts a webview-supplied exe path), mirroring
//! `commands::launch_game_elevated`.

use std::path::Path;

use ncp_host::{EditorInstall, EditorPluginAction, SyncSource, SyncedPlugin};

use crate::commands::state_path;

/// Whole ms since the Unix epoch (the timestamp convention used across state).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn action_str(a: EditorPluginAction) -> &'static str {
    match a {
        EditorPluginAction::Install => "install",
        EditorPluginAction::Update => "update",
        EditorPluginAction::UpToDate => "up_to_date",
        EditorPluginAction::PinnedLocalDev => "pinned_local_dev",
    }
}

fn source_str(s: &SyncSource) -> &'static str {
    match s {
        SyncSource::Signed { .. } => "signed",
        SyncSource::LocalDev { .. } => "local_dev",
    }
}

/// Path to a synced plugin's own `UE4Editor.modules` inside an editor install.
fn plugin_modules_path(editor_root: &Path, plugin: &str) -> std::path::PathBuf {
    ncp_host::editor_plugin_dir(editor_root, plugin)
        .join("Binaries")
        .join("Win64")
        .join("UE4Editor.modules")
}

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

// ---- Phase 1: editor-plugin sync -------------------------------------------

/// One editor plugin's status for a registered editor install, for the UI. Rows
/// come from the union of the signed manifest's `editor_plugins`, the build
/// tree's sideloadable plugins, and what's already synced — so dev sideloads are
/// offered even with an empty manifest.
#[derive(serde::Serialize)]
pub struct EditorPluginStatusDto {
    /// Plugin dir name (e.g. `"NetcodePlus"`).
    pub plugin: String,
    /// `"install" | "update" | "up_to_date" | "pinned_local_dev" | "sideload_only"`.
    pub action: String,
    /// `"signed" | "local_dev"`, or `null` when not installed.
    pub source: Option<String>,
    /// Installed build number, or `null` when not installed.
    pub installed_version: Option<u32>,
    /// The manifest's advertised build number, or `null` when the plugin isn't in
    /// the signed manifest (sideload-only).
    pub available_version: Option<u32>,
    /// Whether this plugin can be sideloaded from the registered build tree.
    pub sideloadable: bool,
    /// Whether the manifest build's engine differs from this install's (warn).
    pub engine_mismatch: bool,
    /// Optional "what's new" link for the available build.
    pub notes_url: Option<String>,
}

/// Outcome of installing one editor plugin.
#[derive(serde::Serialize)]
pub struct EditorPluginInstallOutcome {
    /// Plugin dir name.
    pub plugin: String,
    /// `"installed" | "skipped" | "failed"`.
    pub result: String,
    /// Human-readable detail.
    pub detail: String,
}

/// Register (or, with an empty/whitespace path, clear) the local UT4 build tree
/// used as the source for dev sideloads. Validates the folder holds a `Plugins/`
/// dir. Returns the stored path (empty string when cleared).
#[tauri::command]
pub fn set_build_tree(app: tauri::AppHandle, path: String) -> Result<String, String> {
    let state_file = state_path(&app)?;
    let mut state = ncp_host::state::read(&state_file)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let trimmed = path.trim();
    if trimmed.is_empty() {
        state.build_tree = None;
        ncp_host::state::write(&state_file, &state).map_err(|e| e.to_string())?;
        return Ok(String::new());
    }
    let root = ncp_host::resolve_build_tree(Path::new(trimmed)).ok_or_else(|| {
        format!(
            "{trimmed} isn't a UT4 build tree — pick the project dir that contains \
             Plugins/ (e.g. …\\UnrealTournament\\UnrealTournament), its Plugins folder, \
             or the workspace root"
        )
    })?;
    let stored = root.to_string_lossy().to_string();
    state.build_tree = Some(root);
    ncp_host::state::write(&state_file, &state).map_err(|e| e.to_string())?;
    Ok(stored)
}

/// The registered build-tree path, or `null` if none is set.
#[tauri::command]
pub fn get_build_tree(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let state = ncp_host::state::read(&state_path(&app)?)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    Ok(state.build_tree.map(|p| p.to_string_lossy().to_string()))
}

/// Per-plugin sync status for a registered editor install, from the signed
/// manifest's `editor_plugins` vs what's recorded synced. Empty when the manifest
/// advertises no editor plugins.
#[tauri::command]
pub async fn editor_plugin_status(
    app: tauri::AppHandle,
    root: String,
) -> Result<Vec<EditorPluginStatusDto>, String> {
    use std::collections::{BTreeSet, HashSet};

    let inst = ncp_host::check_editor_install(Path::new(&root)).map_err(|e| e.to_string())?;
    let install_bid = inst.engine_build_id.clone();

    let (manifest, state, _, _) = crate::updates::fetch_verify(&app).await?;
    let synced = state
        .editor_installs
        .get(&root)
        .map(|e| e.synced_plugins.clone())
        .unwrap_or_default();

    // Plugins buildable in the registered build tree (have an editor DLL there).
    let buildable: HashSet<String> = state
        .build_tree
        .as_deref()
        .map(ncp_host::build_tree_plugins)
        .unwrap_or_default()
        .into_iter()
        .collect();

    // Plugins actually PRESENT on disk in this editor install (with an editor
    // DLL) — the plugins dir is `<root>/UnrealTournament/Plugins`, which is what
    // build_tree_plugins scans given the project dir.
    let project_dir = inst.project.parent().unwrap_or(&inst.root).to_path_buf();
    let present: HashSet<String> = ncp_host::build_tree_plugins(&project_dir)
        .into_iter()
        .collect();

    // Rows = manifest ∪ already-synced ∪ (present in the editor AND buildable) —
    // so the panel shows the plugins actually in this editor (correctly statused,
    // not "not installed") plus anything the signed manifest offers, WITHOUT the
    // build-only server/util plugins (ServerShield, StatSQL, …) that aren't here.
    let mut names: BTreeSet<String> = BTreeSet::new();
    names.extend(manifest.editor_plugins.keys().cloned());
    names.extend(synced.keys().cloned());
    names.extend(present.intersection(&buildable).cloned());

    let mut out = Vec::with_capacity(names.len());
    for plugin in names {
        let entry = manifest.editor_plugins.get(&plugin);
        let installed = synced.get(&plugin);
        let is_present = present.contains(&plugin);
        let (action, available_version, engine_mismatch, notes_url) = match entry {
            Some(e) => {
                let d = ncp_host::plan_editor_plugin(installed, e, install_bid.as_deref());
                (
                    action_str(d.action).to_string(),
                    Some(e.version),
                    d.engine_mismatch,
                    e.notes_url.clone(),
                )
            }
            None => {
                // Not in the signed manifest.
                let a = match installed.map(|s| &s.source) {
                    Some(SyncSource::LocalDev { .. }) => "pinned_local_dev",
                    Some(SyncSource::Signed { .. }) => "up_to_date",
                    // On disk but the launcher didn't install it (hand-copied) →
                    // "present"; buildable-but-absent → "sideload_only".
                    None if is_present => "present",
                    None => "sideload_only",
                };
                (a.to_string(), None, false, None)
            }
        };
        out.push(EditorPluginStatusDto {
            plugin: plugin.clone(),
            action,
            source: installed.map(|s| source_str(&s.source).to_string()),
            installed_version: installed.map(|s| s.version),
            available_version,
            sideloadable: buildable.contains(&plugin),
            engine_mismatch,
            notes_url,
        });
    }
    Ok(out)
}

/// Install/update the requested editor plugins (or all advertised ones) into a
/// registered editor install from the signed `editor_plugins` — each zip is
/// downloaded + SHA-256-verified against the manifest, then swapped in. Records a
/// `Signed` [`SyncedPlugin`] per success. Pinned dev sideloads and up-to-date
/// plugins are skipped.
#[tauri::command]
pub async fn install_editor_plugins(
    app: tauri::AppHandle,
    root: String,
    plugins: Option<Vec<String>>,
) -> Result<Vec<EditorPluginInstallOutcome>, String> {
    let inst = ncp_host::check_editor_install(Path::new(&root)).map_err(|e| e.to_string())?;
    let editor_root = inst.root.clone();
    let install_bid = inst.engine_build_id.clone();

    let (manifest, mut state, _, _) = crate::updates::fetch_verify(&app).await?;
    if manifest.editor_plugins.is_empty() {
        return Err("The manifest advertises no editor plugins.".into());
    }
    if !state.editor_installs.contains_key(&root) {
        return Err("Register this editor install first.".into());
    }

    let mut wanted: Vec<String> = match plugins {
        Some(list) if !list.is_empty() => list,
        _ => manifest.editor_plugins.keys().cloned().collect(),
    };
    wanted.sort();

    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    let tmp_dir = std::env::temp_dir();
    let mut outcomes: Vec<EditorPluginInstallOutcome> = Vec::new();
    let mut state_dirty = false;

    for plugin in wanted {
        let Some(entry) = manifest.editor_plugins.get(&plugin) else {
            outcomes.push(EditorPluginInstallOutcome {
                plugin,
                result: "skipped".into(),
                detail: "not advertised in the manifest".into(),
            });
            continue;
        };

        // Decide before touching disk — never overwrite a pinned dev sideload.
        let installed = state
            .editor_installs
            .get(&root)
            .and_then(|e| e.synced_plugins.get(&plugin));
        let action = ncp_host::plan_editor_plugin(installed, entry, install_bid.as_deref()).action;
        match action {
            EditorPluginAction::PinnedLocalDev => {
                outcomes.push(EditorPluginInstallOutcome {
                    plugin,
                    result: "skipped".into(),
                    detail: "dev sideload — pinned".into(),
                });
                continue;
            }
            EditorPluginAction::UpToDate => {
                outcomes.push(EditorPluginInstallOutcome {
                    plugin,
                    result: "skipped".into(),
                    detail: format!("up to date (build {})", entry.version),
                });
                continue;
            }
            EditorPluginAction::Install | EditorPluginAction::Update => {}
        }

        let zip = tmp_dir.join(format!("ncp-editor-{plugin}-{}.zip", std::process::id()));
        if let Err(e) =
            ncp_net::download(&client, &entry.url, entry.sha256, entry.size_bytes, &zip).await
        {
            let _ = std::fs::remove_file(&zip);
            outcomes.push(EditorPluginInstallOutcome {
                plugin,
                result: "failed".into(),
                detail: format!("download/verify failed: {e}"),
            });
            continue;
        }

        let res = ncp_host::install_editor_plugin_zip(
            &editor_root,
            &plugin,
            &zip,
            &entry.sha256.to_string(),
        );
        let _ = std::fs::remove_file(&zip);
        match res {
            Ok(()) => {
                let (build_id, changelist) =
                    ncp_host::read_modules_stamp(&plugin_modules_path(&editor_root, &plugin));
                let synced = SyncedPlugin {
                    source: SyncSource::Signed {
                        release_version: entry.version,
                    },
                    version: entry.version,
                    build_id,
                    changelist,
                    content_hash: ncp_host::editor_plugin_content_hash(&editor_root, &plugin),
                    synced_at_ms: now_ms(),
                };
                if let Some(ei) = state.editor_installs.get_mut(&root) {
                    ei.synced_plugins.insert(plugin.clone(), synced);
                    ei.last_sync_at_ms = Some(now_ms());
                }
                state_dirty = true;
                outcomes.push(EditorPluginInstallOutcome {
                    plugin,
                    result: "installed".into(),
                    detail: format!("build {}", entry.version),
                });
            }
            Err(e) => outcomes.push(EditorPluginInstallOutcome {
                plugin,
                result: "failed".into(),
                detail: e.to_string(),
            }),
        }
    }

    if state_dirty {
        ncp_host::state::write(&state_path(&app)?, &state).map_err(|e| e.to_string())?;
    }
    Ok(outcomes)
}

/// Dev-only: sideload one plugin's freshly-built editor binaries from the
/// registered build tree straight into an editor install (unsigned). Records a
/// pinned `LocalDev` [`SyncedPlugin`] so it is never nagged back to a signed build.
#[tauri::command]
pub fn sideload_editor_plugin(
    app: tauri::AppHandle,
    root: String,
    plugin: String,
) -> Result<(), String> {
    let inst = ncp_host::check_editor_install(Path::new(&root)).map_err(|e| e.to_string())?;
    let editor_root = inst.root.clone();

    let state_file = state_path(&app)?;
    let mut state = ncp_host::state::read(&state_file)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let Some(build_tree) = state.build_tree.clone() else {
        return Err("Set your build tree first (Editor tab → Build tree).".into());
    };
    if !state.editor_installs.contains_key(&root) {
        return Err("Register this editor install first.".into());
    }

    ncp_host::sideload_editor_plugin_from_build(&build_tree, &editor_root, &plugin)
        .map_err(|e| format!("sideload failed: {e}"))?;

    let (build_id, changelist) =
        ncp_host::read_modules_stamp(&plugin_modules_path(&editor_root, &plugin));
    let synced = SyncedPlugin {
        source: SyncSource::LocalDev {
            build_tree: build_tree.clone(),
        },
        version: 0,
        build_id,
        changelist,
        content_hash: ncp_host::editor_plugin_content_hash(&editor_root, &plugin),
        synced_at_ms: now_ms(),
    };
    if let Some(ei) = state.editor_installs.get_mut(&root) {
        ei.synced_plugins.insert(plugin, synced);
        ei.last_sync_at_ms = Some(now_ms());
    }
    ncp_host::state::write(&state_file, &state).map_err(|e| e.to_string())
}
