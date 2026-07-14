# Editor install management — design & roadmap

Manage **UT4 editor installs** from the launcher: register them, launch them,
and keep their plugin binaries current from **signed editor-plugin releases**.

> Status: **investigation complete, not started.** Investigation done
> 2026-07-13 against launcher `1.6.6` and the live two-tree setup on this box.
> No code written yet. Ships as **launcher 1.7.0** (§7); all decisions
> resolved (§5.1, §8).

## The core reality

The plugin author (phantaci) **builds** the UT4 fork in `C:\UnrealTournament`
but **runs** the editor from a separate stock install at
`C:\LAEditorUT4\UnrealTournamentEditor\`, launched as:

```
Engine\Binaries\Win64\UE4Editor.exe UnrealTournament -log -ddc=noshared -d3d11 -sm5
```

That install has its own `Plugins\` and its own `Content\` tree. Today the two
trees are kept in step **entirely by hand** — there is *no* sync mechanism (see
§1). Freshly-built plugin DLLs are copied across manually; project Content
drifts silently in both directions. This feature replaces the manual plugin copy
with a launcher-managed, **signed** editor-plugin channel, and makes the editor
installs first-class (register + launch). Project **Content is explicitly out of
scope** — the author manages it by hand (§6.7).

---

## 1. There is no existing sync mechanism

Confirmed by disk search + the author: no scheduled task, no script under
`C:\dev` or `C:\UnrealTournament`, and no robocopy/xcopy job references the
LAEditor install or copies plugins. DLLs land in the editor tree because they
are **copied by hand** — a plain copy preserves timestamps, which is exactly why
synced DLLs look byte- *and* mtime-identical across trees. The sync engine is
**net-new**; it is not a wrapper over existing tooling.

## 2. The launcher is already half-scaffolded

The editor is **not** a greenfield concept in the launcher:

- `Manifest.editor_installer` already exists (`crates/manifest/src/schema.rs:93`,
  a `GameInstaller`) — the signed, hash-pinned UT4 editor zip.
- A full editor **download → extract → launch** flow already ships:
  `editor_installer_info` / `download_editor` / `install_editor` /
  `launch_editor` (`src-tauri/src/installer.rs:490,521,648,736`).
- `launch_editor` spawns `UE4Editor.exe` with
  `EDITOR_ARGS = ["UnrealTournament","-log","-ddc=noshared","-d3d11","-sm5"]`
  (`installer.rs:462`) — **byte-for-byte the author's launch command.**

But that flow is **one-shot and registry-less**: it uses global statics
`VERIFIED_EDITOR` / `EDITOR_EXE` (`installer.rs:451,455`) set by a download it
performed *this session*. It has **no persistence**, no notion of a
pre-existing install like LAEditor, and no drift/sync. The work is therefore
**promote the one-shot flow into a registered, multi-install manager**, then add
a signed editor-plugin channel — not build from scratch.

## 3. Two-tree reality (ground truth, 2026-07-13)

### 3.1 The trees run different engines

| | Build `C:\UnrealTournament` | Editor `C:\LAEditorUT4\…Editor` |
|---|---|---|
| Engine changelist | 3525109 | **3525360** |
| Engine BuildId | `cc4a0b0a-2c5d-4b59-8749-e3efbfe6620a` | `7a4ea563-ab00-4f14-8e9c-20ca0c9aa851` |
| `Engine\Build\Build.version` | present (`++UT+Main`) | **absent** |

`CLAUDE.md`'s "CL-3525360" describes the **editor**, not the build tree. Every
plugin `.modules` in *both* trees carries the **build tree's** BuildId
(`cc4a0b0a` / CL 3525109) — so freshly-built DLLs are stamped for engine
`cc4a0b0a` but run inside `7a4ea563`. **4.15 tolerates this** (the author runs it
daily), but it is the reason engine BuildId must be tracked and gated (§5.3).

### 3.2 The `.modules` file is the load-gate — not the DLL

Each editor module is loaded via its `UE4Editor.modules` manifest
(`{ Changelist, BuildId, Modules: { <Module>: "UE4Editor-<Module>.dll" } }`).
A DLL present *without* a matching `.modules` may silently fail to load. The DLL
and its `.modules` are an **atomic set** — never copy/ship one without the other.

### 3.3 Per-plugin state (the five named plugins)

| Plugin | Editor-target DLL state | Note |
|---|---|---|
| UTVehicles | in sync (byte+mtime identical) | fully synced |
| NetcodePlus | in sync (today's build) | a stale **orphan** `UE4-NetcodePlus.dll` (May) sits in the editor tree — cruft, not drift |
| LiandriMapForge | in sync | module is `MapForgeBridge` — **name ≠ plugin name** |
| TeamArena | month-stale + missing `.modules` | **DEPRECATED** — folded into NetcodePlus (ElimPlus); a husk, do not sync |
| UTEditorPlus | editor-**only**, no build-tree binaries | own BuildId `671b905e…`; install-authoritative |

Consequence: with TeamArena excluded, **every *active* plugin (NetcodePlus,
UTVehicles, LiandriMapForge) is currently in sync** — because the author copies
them promptly. So plugin sync's value is **convenience + safety + distribution
to others**, not fixing a live break.

### 3.4 Six rules the data dictates

1. **mtime+size is a trustworthy fast key** — manual copy preserves mtime; drift
   shows as diverging mtimes.
2. **Only `UE4Editor-*.dll` + `UE4Editor.modules` matter** to the editor. The
   `UE4-*` / `UE4Server-*` variants are game/server; shipping them into an editor
   install is what created the NetcodePlus orphan.
3. **DLL + `.modules` are an atomic set** (§3.2).
4. **PDBs are 28–57 MB each** → never ship them in editor-plugin releases;
   PDB handling is a local-debug concern only.
5. **Editor-only plugins (UTEditorPlus) are install-authoritative** — never
   "pull from build."
6. **Glob `UE4Editor-*.dll`; don't assume basename = plugin name**
   (MapForgeBridge).

## 4. Reuse surface (launcher architecture map)

| Need | Reuse | Where |
|---|---|---|
| Editor download/verify + launch (author's exact args) | `editor_installer`, `download_editor`/`install_editor`, `launch_editor` | `schema.rs:93`, `installer.rs:462,521,648,736` |
| Detect+validate an install by structure; editor trees deliberately excluded from *play* detection | `detect_installs`/`check_install`/`is_ut4_install_root` | `install.rs:314,93,511,137-146` |
| Per-root persisted registry + atomic state I/O | `installed_plugins: HashMap<String, InstalledPlugin>`, `state::{read,write}` | `state.rs:146,369,396` |
| Fast compare key (explicitly to avoid re-hash / cloud-placeholder hydration) | `PakStamp { size_bytes, mtime_ms }` | `state.rs:293` |
| Plugin install (staging `.staging.<pid>` → atomic swap → `.old.<pid>` sweep, zip-slip guarded) | `install_plugin_zip` | `plugin_install.rs:203,219,242` |
| Drift kernel (order-independent per-file SHA-256 tree fingerprint) | `plugin_content_hash`/`plugin_zip_content_hash`/`combine_fingerprint`/`file_sha256_hex` | `plugin_install.rs:425,452,473,143` |
| Per-artifact decision model | `plan_plugin` + `PluginAction {Install,Update,UpToDate,DowngradeBlocked}` | `planner/plugin.rs:84,22` |
| Multi-install fan-out; safe delete (re-derive-before-delete) | `resolve_roots`; `remove_stray` | `updates.rs:856`, `stray.rs:318` |
| Download + separate streaming verify | `download`/`download_resumable`/`hash_file` | `net/src/download.rs:56,206,297` |
| Tree extract + editor-exe locate | `extract_zip`, `find_editor_exe` | `extract.rs:80,165` |
| UI: multi-install picker + tab pattern | `state.installs[]`/`selInstall`, `#install-sel` (shown when >1), `switchView` | `main.ts:341,343,1599,5913` |

**Genuinely new** (confirmed absent): an engine CL/BuildId reader for installs
(source found: `<root>\Engine\Binaries\Win64\UE4Editor.modules`); a top-level
`editor_plugins` manifest section; and plugin status/plan/install are hardcoded
to the `NetcodePlus` dir today and must be **parameterized by plugin dir + module
glob**. `stray.rs` is a single-tree, hardcoded-pattern scanner — **not** a
tree-vs-tree differ; drift, if ever added, builds on the fingerprint kernel.

## 5. Design

### 5.1 Locked decisions

| Decision | Effect |
|---|---|
| **Dedicated Editor tab** | `data-nav="editor"` + `view-editor`, the `switchView` pattern |
| **Sync via signed plugin releases** | editor plugin binaries ship as **signed, hash-pinned releases** through the existing manifest/signing pipeline — **not** raw-copied from the build tree |
| **Don't touch Content** | project Content sync *and* drift detection are **out of scope** |
| **Register multiple editors** | multi-install registry from day one (author + future mappers) |
| **Both install paths** | default = signed `editor-plugins-latest`; plus a marked **dev sideload** ("install from local build tree", unsigned, author's box) for fast iteration (§5.3.1) |
| **Single release** | one `editor-plugins-latest` GitHub release holds all editor zips; plugins versioned independently in `editor_plugins` |
| **Release target** | launcher **1.7.0** — combined ship (tab + both paths + first signed release), one manifest/seq |

### 5.2 Data model (`crates/host/src/state.rs`, mirroring `installed_plugins`)

```rust
// added to LauncherState — back-compat via #[serde(default)]
pub editor_installs: HashMap<String, EditorInstall>,   // keyed by root path string

pub struct EditorInstall {
    pub root: PathBuf,               // C:\LAEditorUT4\UnrealTournamentEditor
    pub label: String,               // "LAEditor"
    pub editor_exe: PathBuf,         // <root>\Engine\Binaries\Win64\UE4Editor.exe
    pub project: PathBuf,            // <root>\UnrealTournament\UnrealTournament.uproject
    pub engine_build_id: Option<String>,   // from <root>\Engine\Binaries\Win64\UE4Editor.modules
    pub engine_changelist: Option<u64>,
    pub launch_args: Vec<String>,    // default = EDITOR_ARGS
    pub added_at: DateTime<Utc>,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub synced_plugins: HashMap<String, SyncedPlugin>,  // keyed by plugin dir name
}

pub struct SyncedPlugin {
    pub module_dll: String,          // "UE4Editor-NetcodePlus.dll" (module ≠ plugin name)
    pub source: SyncSource,          // how it got here — see §5.3.1
    pub version: u32,                // editor-plugin release build number (Signed), recorded at install
    pub build_id: Option<String>,    // from the plugin's UE4Editor.modules
    pub changelist: Option<u64>,
    pub dll_stamp: PakStamp,         // reuse PakStamp — the installed editor DLL
    pub content_hash: Option<String>,// combine_fingerprint over the plugin's editor files (drift)
    pub synced_at: DateTime<Utc>,
}

pub enum SyncSource {
    Signed { release_version: u32 },   // from editor-plugins-latest, sha256-verified against the manifest
    LocalDev { build_tree: PathBuf, source_stamp: PakStamp },  // unsigned sideload, author's box; pinned
}
```

Engine CL/BuildId per install is parsed from
`<root>\Engine\Binaries\Win64\UE4Editor.modules`. Do **not** trust
`<plugin>.uplugin` `VersionName` — it is never bumped; the recorded release
`version` is the source of truth (same as `installed_plugins`).

### 5.3 Signed editor-plugin channel

Editor plugin binaries are distributed exactly like the game plugin
(`channels.stable.plugin`, `PluginEntry`), but as a **top-level map** (editor
installs are not game-channel-scoped):

```rust
// crates/manifest/src/schema.rs — new top-level field on Manifest (back-compat)
#[serde(default, skip_serializing_if = "HashMap::is_empty")]
pub editor_plugins: HashMap<String, EditorPluginEntry>,   // key = plugin dir name

pub struct EditorPluginEntry {   // = PluginEntry (schema.rs:218) + engine stamp
    pub version: u32,            // integer build number
    pub url: String,
    pub sha256: Sha256Digest,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_build_id: Option<String>,   // engine this artifact was built against
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_changelist: Option<u64>,
}
```

**Artifacts** — one editor-target zip per active plugin, flat-root:
`<Plugin>.uplugin` + `Binaries/Win64/UE4Editor-<Module>.dll` +
`Binaries/Win64/UE4Editor.modules` (+ the plugin's own `Content/` if it has any —
distinct from *project* Content, which stays out of scope). **No PDBs.** Built
with `ZipFile::CreateFromDirectory($src,$out,'Optimal',$false)` (no wrapper
dir), as the game-plugin zips already are.

**Install** reuses `install_plugin_zip` unchanged (different root + plugin dir):
staging `.<Plugin>.staging.<pid>` → validate → move live aside to
`.<Plugin>.old.<pid>` → atomic rename → sweep old, with rollback. Status / plan /
drift reuse `plugin_status` / `plan_plugin` / `content_hash`, **parameterized**
from the current hardcoded `NetcodePlus` to arbitrary plugin dir + `UE4Editor-*`
module glob.

**No server-gate.** Unlike a game-plugin roll (which needs the server `.so`
deployed first or `NCPlusVersionGate` kicks players), editor plugins are
single-user offline — an editor-plugin release publishes **independently**, no
coordination.

**Signing** extends the existing runbook plugin step: build → package each
editor zip → upload to GitHub → hash-pin (`sha256`+`size_bytes`) into the
`editor_plugins` map → **one** YubiKey `rsign sign` for the whole manifest.
Division of labour holds — author bumps/builds/signs; Claude edits/verifies/
publishes. See `reference_launcher_release_runbook` (memory) / this repo's
signing docs.

#### 5.3.1 Dev sideload (local build tree)

For the author's own fast iteration, a **marked, unsigned** path installs an
active plugin's editor binaries **directly from the build tree**
(`<build>\Plugins\<P>\Binaries\Win64\UE4Editor-*.dll` + `UE4Editor.modules`, plus
the plugin's own `Content/` if any) into a registered editor install — no
package/sign/publish round-trip. It reuses the same staging→swap install
(assembling the atomic set from a local dir instead of a verified zip), is gated
to a user-picked local folder, touches no network and no privilege, and is
**clearly labelled "dev sideload — unsigned, local build"** in the UI. Sideloaded
plugins record `SyncSource::LocalDev` and are **pinned**: the launcher never nags
to "update" a deliberately-sideloaded newer dev build back to the signed release,
and never auto-replaces one. This is the escape hatch; the signed channel remains
the default and the **only** path mappers ever see.

### 5.4 Engine BuildId compat gate

This is what makes "track engine CL" load-bearing. An editor-plugin release is
built against a specific engine (`engine_build_id`/`engine_changelist` in the
entry). A registered editor install advertises its own BuildId (parsed from
`UE4Editor.modules`). On install/update the launcher **compares and warns on
mismatch** — it does **not block** (4.15 empirically tolerates the current
`cc4a0b0a` vs `7a4ea563` gap). This protects mappers whose editor is a different
engine build than the one the author packaged against.

### 5.5 Launch integration

`launch_editor` already spawns `UE4Editor.exe` with the correct args. Change:
target a **registered `EditorInstall`'s** `editor_exe`/`launch_args` instead of
the one-shot global. Add `launch_editor_install(root)` that re-validates via an
editor `check_install` and spawns. Effectively done.

### 5.6 UI surface

A dedicated **Editor** sidebar entry (`data-nav="editor"` + `<section
id="view-editor">`, the `switchView` pattern at `main.ts:5913`). Contents:

- **Installs list** — registered `EditorInstall`s, each with engine CL/BuildId,
  last-sync, and a **Launch** button; a folder-pick to register a new install;
  the `>1 → picker` pattern (`main.ts:1599`) if a selection context is needed.
- **Plugins panel** per install — each active editor plugin's state (installed /
  update available / up-to-date / missing), an **Update** action, and the
  engine-BuildId-mismatch warning.

The current one-shot editor flow migrates here from the Addons
`editor-install-section` (`main.ts:4902`).

### 5.7 Safety & signing

- The install/verify path is the **signed** manifest flow — hash-pinned,
  YubiKey-signed, verified against the baked-in trust key. No unsigned bytes are
  installed into an editor tree (subject to the §8 dev-sideload decision).
- All backend commands **re-derive paths from the registered `root`** — never
  trust a webview-supplied path (mirror `launch_game_elevated` / `remove_stray`).
- Any delete (deprecated-husk cleanup) is **un-elevated only**, with the Explorer
  "Open folder" fallback on protected paths (`reveal_in_folder`, `ae2a4cf`). The
  abandoned elevated stray-delete worker (junction/TOCTOU EoP) stays abandoned —
  see `security_launcher_elevated_stray_delete` (memory).

## 6. Explicitly out of scope

- **Project Content** (`UnrealTournament\Content\`): no sync, no drift
  detection, no copies. The author manages it by hand. This is a deliberate
  decision, not an omission — see the drift data in the appendix for what is
  being left alone (editor tree has ~1,250 more RestrictedAssets and 20× the
  Blueprints; the `SK_VH_Scorpion_001_Physics` case).
- **Deprecated plugins** (TeamArena, TeamArenaMinimalGamemode): never published,
  never synced. Their leftover DLLs are cruft-cleanup candidates only (§5.7).

## 7. Phased plan

**Release target: launcher 1.7.0** (minor bump from `1.6.6`), a combined ship —
editor tab + both install paths + the first signed `editor-plugins-latest` — in
one manifest/sign/publish. Bump the four version files and pick the sequence per
`reference_launcher_release_runbook` (always bump from the *live* manifest, never
a number written here). Phase 0's code can land first if a split is preferred.

**Phase 0 — Editor tab: register + launch multiple editors.** `editor_installs`
map; `editor_check_install` (validate `UE4Editor.exe`+`.uproject`, read engine
BuildId/CL); commands `add`/`list`/`remove`/`launch_editor_install`; dedicated
Editor tab with folder-pick registration + per-install Launch. **Code-only —
ships in a normal signed launcher roll, no manifest change.** Kills the
"which install is running" confusion and gives one-click launch of each editor.

**Phase 1 — Signed editor-plugin channel.** `editor_plugins` map +
`EditorPluginEntry`; per-plugin editor zips packaged, published, hash-pinned,
signed; `plugin_status`/`plan_plugin`/`install_plugin_zip` parameterized per
plugin + editor install; BuildId-mismatch warning; per-install Plugins panel.
**Ships signed — manifest entries + new signed editor-plugin releases + sign
ceremony.** This is the feature.

**Phase 2 — mappers & cleanup (optional).** Onboarding a mapper to the signed
editor-plugin set; deprecated-husk leftover cleanup (un-elevated + Explorer
fallback); optionally publish UTEditorPlus as an editor-only release.

Plugins to publish as editor releases: **NetcodePlus, UTVehicles,
LiandriMapForge** (active). UTEditorPlus optional (editor-only). TeamArena
family: never.

## 8. Resolved decisions

1. **Both install paths** — default signed `editor-plugins-latest`, plus a
   marked dev sideload from the local build tree (§5.3.1). Not signed-only.
2. **Single release** — one `editor-plugins-latest` GitHub release holds all
   editor zips; plugins versioned independently in the `editor_plugins` map.
3. **Release target** — launcher **1.7.0**, combined ship (§7): editor tab +
   both install paths + first signed `editor-plugins-latest`, one manifest/seq
   (next sequence after the live floor — check the live manifest per the
   runbook).

## Appendix — raw inventory (2026-07-13)

**Engine BuildIds:** build `cc4a0b0a-2c5d-4b59-8749-e3efbfe6620a` (CL 3525109,
`++UT+Main`); LAEditor `7a4ea563-ab00-4f14-8e9c-20ca0c9aa851` (CL 3525360, no
`Build.version`). All plugin `.modules` carry `cc4a0b0a` / CL 3525109 except
UTEditorPlus (`671b905e-d01a-4a32-8246-5e27eadff419`, built 2025-06-24).

**Active-plugin editor DLLs** (`UE4Editor-<Module>.dll`): NetcodePlus 6,032,896 B
@ 2026-07-13 18:13 (build==editor); UTVehicles 438,272 B @ 2026-07-13 19:39
(build==editor); MapForgeBridge 341,504 B @ 2026-07-13 18:17 (build==editor).
TeamArena editor DLL: build 693,760 B @ 2026-03-22 vs editor 677,888 B @
2026-02-17, editor missing `UE4Editor.modules` (deprecated — ignore).

**Content drift (out of scope, documented):** Blueprints 37 (build) vs 1,059
(editor); BlueprintsOG 0 vs 759; RestrictedAssets 21,784 vs 23,035. Scorpion
folder `RestrictedAssets\Proto\UT3_Vehicles\VH_Scorpion\Meshes\`:
`SK_VH_Scorpion_001_Physics.uasset` (2,664 B, 2026-03-30) exists **only** in the
editor tree; `SK_VH_Scorpion_001.uasset` differs (build 1,055,830 B / 2023 vs
editor 1,212,112 B / 2026-03-30, re-saved).
