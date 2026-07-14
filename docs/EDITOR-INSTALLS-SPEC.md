# Editor install management — implementation spec (Phase 0/1)

Implementation-ready companion to `EDITOR-INSTALLS-DESIGN.md`. Ships as launcher
**1.7.0** (combined: Editor tab + both install paths + first signed
`editor-plugins-latest`). Signatures below are the proposed shape, grounded in
the current 1.6.6 layout; file anchors point at the code to mirror or change.

Conventions: paths are Windows; a UT4 editor "root" is the folder containing
`Engine\` and `UnrealTournament\` (e.g. `C:\LAEditorUT4\UnrealTournamentEditor`).
Editor project = `UnrealTournament`, `.uproject` at
`<root>\UnrealTournament\UnrealTournament.uproject`, editor exe at
`<root>\Engine\Binaries\Win64\UE4Editor.exe`.

---

## Phase 0 — Editor tab: register + launch multiple editors

**Code-only. No manifest change. Ships in the 1.7.0 roll.**

### 0.1 `crates/host/src/editor.rs` (new — mirror `install.rs`)

```rust
pub struct EditorInstall {                     // Serialize/Deserialize; lives in host, referenced by state.rs
    pub root: PathBuf,
    pub label: String,
    pub editor_exe: PathBuf,                    // <root>\Engine\Binaries\Win64\UE4Editor.exe
    pub project: PathBuf,                       // <root>\UnrealTournament\UnrealTournament.uproject
    pub engine_build_id: Option<String>,
    pub engine_changelist: Option<u64>,
    pub launch_args: Vec<String>,
    pub added_at: DateTime<Utc>,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub synced_plugins: HashMap<String, SyncedPlugin>,   // see design §5.2 (Phase 1 populates)
}

pub enum EditorError { NotEditorRoot(PathBuf), NoProject(PathBuf), Io(io::Error) }

/// Walk up from a picked path to the nearest editor root; validate + read stamps.
pub fn check_editor_install(picked: &Path) -> Result<EditorInstall, EditorError>;

/// Structural gate: UE4Editor.exe present AND the .uproject present.
fn is_editor_root(root: &Path) -> bool;        // editor_exe(root).is_file() && uproject(root).is_file()
pub fn editor_exe(root: &Path) -> PathBuf;     // <root>\Engine\Binaries\Win64\UE4Editor.exe
fn uproject(root: &Path) -> PathBuf;           // <root>\UnrealTournament\UnrealTournament.uproject

/// Parse <root>\Engine\Binaries\Win64\UE4Editor.modules for "BuildId" + "Changelist".
/// (Editor installs have no Engine\Build\Build.version — this is the CL source.)
pub fn read_engine_stamp(root: &Path) -> (Option<String>, Option<u64>);

/// ["UnrealTournament","-log","-ddc=noshared","-d3d11","-sm5"] — the community convention
/// (installer.rs:462 EDITOR_ARGS today). Project name derived from the .uproject stem.
pub fn default_editor_args(project_name: &str) -> Vec<String>;

/// Plain spawn (std::process::Command), cwd = editor_exe parent. NO priority/affinity
/// (that is game-launch only; mirror the existing launch_editor spawn, installer.rs:736).
pub fn launch_editor_install(inst: &EditorInstall) -> Result<(), LaunchError>;
```

Reuse from `install.rs`: the walk-up-to-root idea (`check_install`, install.rs:93)
and `group_by_root`. Note play-detection deliberately **excludes** editor trees
(install.rs:137-146), so this is orthogonal — no collision with `detect_installs`.

### 0.2 `crates/host/src/state.rs`

```rust
// add to LauncherState (state.rs:34), back-compat:
#[serde(default)]
pub editor_installs: HashMap<String, EditorInstall>,   // keyed by root path string

// helpers (mirror the installed_plugins access pattern, state.rs:146):
impl LauncherState {
    pub fn upsert_editor_install(&mut self, e: EditorInstall);   // key = e.root.to_string_lossy()
    pub fn remove_editor_install(&mut self, root: &str) -> bool; // registry only — never deletes files
    pub fn get_editor_install(&self, root: &str) -> Option<&EditorInstall>;
}
```

Persist via the existing atomic `state::write` (state.rs:396); read via
`state::read` (state.rs:369).

### 0.3 `src-tauri/src/commands.rs` (or new `editor_commands.rs`, registered in `lib.rs`)

```rust
#[tauri::command] pub fn list_editor_installs(app: AppHandle) -> Vec<EditorInstallDto>;
#[tauri::command] pub fn add_editor_install(app: AppHandle, path: String, label: Option<String>)
    -> Result<EditorInstallDto, String>;   // check_editor_install(picked) -> upsert -> write
#[tauri::command] pub fn remove_editor_install(app: AppHandle, root: String) -> Result<(), String>;
#[tauri::command] pub fn set_editor_label(app: AppHandle, root: String, label: String) -> Result<(), String>;
#[tauri::command] pub fn launch_editor_install(app: AppHandle, root: String) -> Result<(), String>;
    // re-derive: check_editor_install(root) fresh (never trust the stored exe path) -> launch_editor_install
```

`EditorInstallDto` = serde view (root, label, engine_build_id, engine_changelist,
last_sync_at, `plugins: Vec<EditorPluginStatusDto>` [empty in Phase 0]). **Every
command re-derives paths from `root`** (mirror `launch_game_elevated`,
commands.rs:154). `path` in `add_editor_install` comes only from the native
folder dialog (below).

### 0.4 Frontend

`index.html` — sidebar (index.html:56-85): add
`<button class="nav" data-nav="editor">Editor</button>`; content (index.html:88-222):
add `<section id="view-editor" class="view">` with `#editor-installs` list +
`#editor-register` button. `switchView` (main.ts:5913) already handles the new
`data-nav`/`view-` pair generically.

`src/main.ts`:
- state: `editorInstalls: EditorInstallDto[]`, `selEditor: string|null` (root key).
- `renderEditor()` — list rows: label, engine CL/BuildId, last-sync, **Launch**
  (`launch_editor_install`), **Remove** (`remove_editor_install`, confirm — files
  untouched), rename (`set_editor_label`). Empty-state → "Register editor…".
- `registerEditor()` — open the tauri folder dialog (same plugin used for the
  game-install Manual pick), pass result to `add_editor_install`.
- load `list_editor_installs` on init + on tab activation.

**Phase 0 done** = register LAEditor, one-click launch with the exact args, all
editors persisted. Solves the "which install is running" confusion.

---

## Phase 1 — Signed editor-plugin channel + both install paths

**Ships signed: manifest entries + one `editor-plugins-latest` release + the sign
ceremony.**

### 1.1 `crates/manifest/src/schema.rs`

```rust
// add to Manifest (schema.rs:28), back-compat (old launchers ignore it):
#[serde(default, skip_serializing_if = "HashMap::is_empty")]
pub editor_plugins: HashMap<String, EditorPluginEntry>,   // key = plugin dir name

pub struct EditorPluginEntry {                 // = PluginEntry (schema.rs:218) + engine stamp
    pub version: u32,                          // integer build number
    pub url: String,                           // editor-plugins-latest asset (untrusted; sha256 is the gate)
    pub sha256: Sha256Digest,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub notes_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub engine_build_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub engine_changelist: Option<u64>,
}
```

Tests (mirror schema.rs:572-648): round-trip; `editor_plugins` absent →
deserializes empty; unknown field tolerated; an entry with/without the engine
stamp both verify. `cargo test -p ncp-manifest` must stay green. **The set of
plugins the launcher installs is manifest-driven** (the `editor_plugins` keys) —
no hardcoded list; adding UTEditorPlus later is a manifest entry + a zip.

### 1.2 `crates/host` — parameterize the NetcodePlus-specific plumbing

Today plugin status/fingerprint/install assume the `NetcodePlus` dir (netcodeplus_status
install.rs:185; `plugin_content_hash` plugin_install.rs:425; `install_plugin_zip`
plugin_install.rs:203). Thread a plugin name through:

```rust
pub fn plugin_dir(root: &Path, plugin: &str) -> PathBuf;   // <root>\UnrealTournament\Plugins\<plugin>

/// Order-independent SHA-256 tree fingerprint over the load-bearing editor files:
/// <plugin>.uplugin + Binaries\Win64\UE4Editor-*.dll + UE4Editor.modules (+ Content\** if any).
/// Reuse combine_fingerprint (plugin_install.rs:473) + file_sha256_hex (:143).
pub fn editor_plugin_content_hash(root: &Path, plugin: &str) -> io::Result<String>;
pub fn editor_plugin_zip_content_hash(zip: &Path) -> io::Result<String>;   // same, from a zip

/// Install a verified editor-plugin zip into <editor_root>\UnrealTournament\Plugins\<plugin>.
/// Reuse install_plugin_zip's staging (.<plugin>.staging.<pid>) -> validate -> swap live aside
/// (.<plugin>.old.<pid>) -> rename -> sweep, with rollback (plugin_install.rs:203-260).
pub fn install_editor_plugin_zip(editor_root: &Path, plugin: &str, zip: &Path,
    expected_sha256: &Sha256Digest) -> Result<(), PluginInstallError>;
```

Dev sideload (assemble from the build tree instead of a verified zip):

```rust
/// Copy the atomic editor set for <plugin> from the build tree into a staging dir:
/// <build>\Plugins\<plugin>\{<plugin>.uplugin, Binaries\Win64\UE4Editor-*.dll,
/// Binaries\Win64\UE4Editor.modules, Content\**}. Skips UE4-*/UE4Server-* and PDBs.
pub fn stage_editor_plugin_from_build(build_root: &Path, plugin: &str, staging: &Path) -> io::Result<()>;

/// Same swap-into-live as the zip path, but from a prepared staging dir (no signature).
pub fn install_editor_plugin_from_dir(editor_root: &Path, plugin: &str, staging: &Path)
    -> Result<(), PluginInstallError>;
```

### 1.3 `crates/planner` — editor-plugin decision (mirror `plan_plugin`, planner/plugin.rs:84)

```rust
pub enum EditorPluginAction { Install, Update, UpToDate, PinnedLocalDev, None }

pub struct EditorPluginDecision {
    pub action: EditorPluginAction,
    pub engine_mismatch: bool,     // entry.engine_build_id != install.engine_build_id (WARN, non-blocking)
}

pub fn plan_editor_plugin(
    installed: Option<&SyncedPlugin>,
    entry: &EditorPluginEntry,
    install_engine_build_id: Option<&str>,
) -> EditorPluginDecision;
```

Rules: `installed.source == LocalDev` → `PinnedLocalDev` (never auto-suggest
replacing a sideload); else compare recorded `version`/`content_hash` vs `entry`
→ Install/Update/UpToDate; overlay `engine_mismatch` from the stamps.

### 1.4 `src-tauri` commands

```rust
#[tauri::command] pub fn set_build_tree(app, path: String) -> Result<(), String>;
    // validate: Engine\Build\Build.version + Plugins\ exist; persist build_tree + its engine stamp

#[tauri::command] pub async fn editor_plugin_status(app, root: String)
    -> Result<Vec<EditorPluginStatusDto>, String>;
    // for each manifest editor_plugins key: plan_editor_plugin(...) + source + versions + engine_mismatch

#[tauri::command] pub async fn install_editor_plugins(app, root: String, plugins: Option<Vec<String>>)
    -> Result<InstallSummary, String>;
    // SIGNED path: for each (selected|all) manifest entry -> ncp_net::download(url, sha256, size, tmp)
    // (download.rs:56, inline verify) -> install_editor_plugin_zip -> record SyncedPlugin{source:Signed}
    // -> set last_sync_at. Progress via existing plugin-download events.

#[tauri::command] pub async fn sideload_editor_plugin(app, root: String, plugin: String)
    -> Result<(), String>;
    // DEV path: requires build_tree set; stage_editor_plugin_from_build -> install_editor_plugin_from_dir
    // -> record SyncedPlugin{source:LocalDev, source_stamp}. UI marks it unsigned.
```

Signed install verifies each zip inline (`ncp_net::download` enforces sha256+size,
download.rs:56) — no unsigned bytes reach an editor tree. Re-derive `editor_root`
from `root` via `check_editor_install`. Engine mismatch warns, never blocks.

### 1.5 Frontend (Editor tab plugin panel)

Per selected editor install, render from `editor_plugin_status`:
- one row per plugin: name, **action badge** (Install / Update / Up-to-date /
  **Dev-pinned**), **source** ("signed v327" | "dev sideload"), an
  engine-mismatch ⚠ when set, and an Install/Update button.
- **Sync all** → `install_editor_plugins(root, null)` with a progress bar (reuse
  the `launcher-download-progress`/plugin progress listeners).
- **Sideload from build tree…** — shown only when a build tree is registered,
  clearly labelled *unsigned / dev* → `sideload_editor_plugin`. Guarded confirm.

### 1.6 Packaging — `tools/pack-editor-plugins.ps1` (new; house style = `tools/pak-authoring.ps1`)

```powershell
param(
  [string]$BuildTree = "C:\UnrealTournament\UnrealTournament",
  [string]$OutDir    = "$env:TEMP\ncp-editor-plugins",
  [string[]]$Plugins = @("NetcodePlus","UTVehicles","LiandriMapForge")
)
# For each plugin, into a flat staging dir collect:
#   <plugin>.uplugin
#   Binaries\Win64\UE4Editor-*.dll          (skip UE4-* / UE4Server-* / *.pdb)
#   Binaries\Win64\UE4Editor.modules        (the load-gate — MUST accompany the DLL)
#   Content\**                              (only if the plugin has its own Content)
# Zip flat-root (no wrapper dir), matching the game-plugin recipe:
#   [System.IO.Compression.ZipFile]::CreateFromDirectory($stage,$out,'Optimal',$false)
# Emit, per plugin (for the manifest editor_plugins entry):
#   sha256  = (Get-FileHash $out -Algorithm SHA256).Hash.ToLower()
#   size    = (Get-Item $out).Length
#   engine_build_id / engine_changelist = read from that plugin's UE4Editor.modules
```

Output: `NetcodePlus-editor-<ver>.zip`, `UTVehicles-editor-<ver>.zip`,
`MapForgeBridge-editor-<ver>.zip` (+ their sha256/size/stamp lines).

### 1.7 Manifest + sign ceremony delta (extends `reference_launcher_release_runbook`)

Combined 1.7.0 ship, one manifest/seq:
1. Bump the 4 version files to 1.7.0 (Cargo.toml / tauri.conf.json / package.json;
   Cargo.lock on build). Build the bare exe (`--no-bundle`), hash-pin the launcher
   entry as usual.
2. Run `tools/pack-editor-plugins.ps1` → upload **all** zips to one release:
   `gh release upload editor-plugins-latest <zips> --repo jmortley/netcodeplus-launcher --clobber`
   (create the release once if absent).
3. Add/update the manifest `editor_plugins` map: one entry per plugin
   (`version`, `url` = the release asset, `sha256`, `size_bytes`,
   `engine_build_id`, `engine_changelist`).
4. **No server-gate** (editor-only — no `NCPlusVersionGate`). Bump `sequence`
   from the **live** manifest (never a doc number). Validate with the real
   deserializer (`cargo test -p ncp-manifest`).
5. Division of labour unchanged: author bumps build numbers + runs the single
   `rsign sign`; Claude edits/verifies/publishes. Verify vs the baked-in trust
   key, expect "prehashed".

### 1.8 Verify checklist

- `cargo test -p ncp-manifest` (schema round-trip + back-compat) · `cargo check`
  workspace · `npx tsc --noEmit`.
- Manual: register LAEditor → **Sync all** against a staged test manifest → confirm
  `UE4Editor-*.dll` + `UE4Editor.modules` land as an atomic set and the editor
  loads the plugins (`…\UnrealTournament\Saved\Logs\UnrealTournament.log` →
  `LogLoad: netcodeplus loaded`). Then **sideload** one plugin → confirm it shows
  `dev sideload` + `Dev-pinned` and isn't nagged to "update".
- Engine-mismatch: point an entry's `engine_build_id` at a bogus value → confirm ⚠
  shows but install still proceeds.

---

## Task order (both phases)

1. schema `editor_plugins` + `EditorPluginEntry` + tests (unblocks manifest work).
2. host `editor.rs` + state field + `read_engine_stamp`.
3. Phase 0 commands + Editor tab UI + launch → **shippable slice**.
4. host plugin-plumbing parameterization + `SyncSource`/`SyncedPlugin`.
5. planner `plan_editor_plugin`; Phase 1 commands (signed + sideload + build tree).
6. Editor-tab plugin panel + progress.
7. `tools/pack-editor-plugins.ps1`; manifest/runbook delta; 1.7.0 combined ship.
