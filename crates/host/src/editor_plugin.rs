//! Install a plugin's **editor-target** binaries into a registered UT4 editor
//! install — either from a signed `editor-plugins-latest` zip (the default,
//! SHA-256-verified against the manifest) or, for the author's own iteration,
//! sideloaded straight from the local build tree (unsigned).
//!
//! Mechanically identical to [`crate::plugin_install`] — stage into a temp
//! sibling, validate, atomic swap, roll back on failure — reusing its zip-slip
//! guard, fingerprint, and reboot-delete primitives, but parameterized by plugin
//! dir name and targeting `<editor_root>/UnrealTournament/Plugins/<plugin>/`. The
//! shipping game-plugin path in `plugin_install` is left untouched.
//!
//! Because the install is a whole-dir atomic swap, syncing a plugin's editor
//! binaries also clears any stale over-copied game/server DLLs from the editor
//! tree — the installed folder ends up as exactly the shipped/staged set.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ncp_manifest::EditorPluginEntry;

use crate::editor::{SyncSource, SyncedPlugin};
use crate::fs_util::{annotate, rename_with_retry};
use crate::plugin_install::{
    collect_files_under, combine_fingerprint, extract_into, file_sha256_hex, norm_plugin_rel,
    reader_sha256_hex, remove_leftover_dir, PluginInstallError, Result,
};

/// The UT4 project/game folder name, shared with [`crate::install`].
const GAME_NAME: &str = "UnrealTournament";

/// What to do about one editor plugin in a registered editor install, given the
/// signed manifest entry and what's currently synced there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorPluginAction {
    /// Not synced into this editor tree — offer to install.
    Install,
    /// A newer signed build is available than the one installed.
    Update,
    /// Installed and current (installed build >= manifest build).
    UpToDate,
    /// Sideloaded from the local build tree — pinned; never auto-updated back to
    /// the signed release, and never auto-replaced.
    PinnedLocalDev,
}

/// The decision for one editor plugin: what to do, plus whether the manifest
/// build's engine differs from the target install's (a non-blocking warning).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorPluginDecision {
    /// The recommended action.
    pub action: EditorPluginAction,
    /// The manifest build advertises an engine `BuildId` that differs from the
    /// target editor install's — a compat WARNING only (4.15 tolerates it, so the
    /// launcher still installs; it just surfaces the mismatch).
    pub engine_mismatch: bool,
}

/// Decide what to do about one editor plugin.
///
/// `installed` is the recorded sync state (`None` = not installed), `entry` the
/// signed manifest build, and `install_engine_build_id` the target editor
/// install's own engine BuildId (from its `UE4Editor.modules`).
///
/// A `LocalDev` sideload is always [`EditorPluginAction::PinnedLocalDev`] (a
/// deliberate dev build is never nagged back to the signed release). A signed
/// install updates only when the manifest build is strictly newer, so a manifest
/// rollback never triggers a downgrade.
#[must_use]
pub fn plan_editor_plugin(
    installed: Option<&SyncedPlugin>,
    entry: &EditorPluginEntry,
    install_engine_build_id: Option<&str>,
) -> EditorPluginDecision {
    let engine_mismatch = match (entry.engine_build_id.as_deref(), install_engine_build_id) {
        (Some(manifest_id), Some(install_id)) => !manifest_id.eq_ignore_ascii_case(install_id),
        _ => false,
    };
    let action = match installed {
        None => EditorPluginAction::Install,
        Some(sp) => match sp.source {
            SyncSource::LocalDev { .. } => EditorPluginAction::PinnedLocalDev,
            SyncSource::Signed { .. } => {
                if sp.version < entry.version {
                    EditorPluginAction::Update
                } else {
                    EditorPluginAction::UpToDate
                }
            }
        },
    };
    EditorPluginDecision {
        action,
        engine_mismatch,
    }
}

/// `<editor_root>/UnrealTournament/Plugins/<plugin>/` — where a plugin's editor
/// binaries live inside an editor install.
#[must_use]
pub fn plugin_dir(editor_root: &Path, plugin: &str) -> PathBuf {
    editor_root.join(GAME_NAME).join("Plugins").join(plugin)
}

/// List the plugin dir names in a build tree that have an editor DLL built
/// (`Plugins/<name>/Binaries/Win64/UE4Editor-*.dll`) — i.e. the plugins that can
/// be sideloaded from it. Sorted + deduped; empty if the tree has no `Plugins/`.
/// This is what lets the Editor tab offer dev sideloads even with an empty
/// (or absent) signed `editor_plugins` manifest.
#[must_use]
pub fn build_tree_plugins(build_tree: &Path) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let Ok(entries) = fs::read_dir(build_tree.join("Plugins")) else {
        return names;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let has_editor_dll = fs::read_dir(dir.join("Binaries").join("Win64"))
            .map(|rd| {
                rd.flatten().any(|f| {
                    let n = f.file_name();
                    let n = n.to_string_lossy();
                    n.starts_with("UE4Editor-") && n.ends_with(".dll")
                })
            })
            .unwrap_or(false);
        if has_editor_dll {
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Stock UT4 plugins that ship with the editor (Epic's, plus vendor/sample ones).
/// Excluded from the auto-discovered sideload list so the Editor panel shows the
/// author's plugins — not CorsairRGB / RazerChroma / WebMRecord / samples. A
/// plugin explicitly published in the signed manifest is shown regardless.
const STOCK_PLUGINS: &[&str] = &[
    "CorsairRGB",
    "RazerChroma",
    "WebMRecord",
    "Substance",
    "SampleGameMode",
    "SampleMutator",
    "PackageContent",
    "ContentOnly",
    "Online",
    "CustomBot",
    "NotForLicensees",
];

/// Whether `plugin` is a stock UT4 plugin that ships with the editor (see
/// [`STOCK_PLUGINS`]). Case-insensitive.
#[must_use]
pub fn is_stock_plugin(plugin: &str) -> bool {
    STOCK_PLUGINS.iter().any(|s| s.eq_ignore_ascii_case(plugin))
}

/// Order-independent SHA-256 fingerprint of the editor plugin on disk under
/// `editor_root`: its `<plugin>.uplugin` plus every file under `Binaries/`, each
/// SHA-256'd and combined in sorted relative-path order. Equals the fingerprint
/// [`zip_content_hash`] derives from that build's zip, so drift is detectable.
/// `None` if the plugin folder is absent or unreadable.
#[must_use]
pub fn content_hash(editor_root: &Path, plugin: &str) -> Option<String> {
    let dir = plugin_dir(editor_root, plugin);
    if !dir.is_dir() {
        return None;
    }
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let uplugin = dir.join(format!("{plugin}.uplugin"));
    if uplugin.is_file() {
        files.push((norm_plugin_rel(&format!("{plugin}.uplugin")), uplugin));
    }
    collect_files_under(&dir.join("Binaries"), &dir, &mut files).ok()?;
    let mut parts: Vec<(String, String)> = Vec::with_capacity(files.len());
    for (rel, path) in files {
        parts.push((rel, file_sha256_hex(&path).ok()?));
    }
    combine_fingerprint(parts)
}

/// The EXPECTED [`content_hash`] for an editor-plugin build, computed from its
/// verified zip (the root `.uplugin` + every `Binaries/` entry) rather than an
/// on-disk folder — equal to a clean install's folder fingerprint. `None` if the
/// zip can't be read or carries no plugin files.
#[must_use]
pub fn zip_content_hash(zip_path: &Path) -> Option<String> {
    let file = fs::File::open(zip_path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let mut parts: Vec<(String, String)> = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).ok()?;
        let name = entry.name().to_string();
        if entry.is_dir() || name.ends_with('/') || name.ends_with('\\') {
            continue;
        }
        let rel = norm_plugin_rel(&name);
        // A flat-root editor-plugin zip: the <plugin>.uplugin at the top level plus
        // the Binaries/ tree. Match either (any top-level *.uplugin).
        let is_root_uplugin = rel.ends_with(".uplugin") && !rel.contains('/');
        if is_root_uplugin || rel.starts_with("binaries/") {
            let fh = reader_sha256_hex(&mut entry).ok()?;
            parts.push((rel, fh));
        }
    }
    combine_fingerprint(parts)
}

/// Install a **verified** editor-plugin zip into
/// `<editor_root>/UnrealTournament/Plugins/<plugin>/`, re-checking its SHA-256
/// against `expected_sha256_hex` first (defense-in-depth: the caller downloaded +
/// verified, but we re-hash before touching disk). Atomic swap: stage → validate
/// (`<plugin>.uplugin` + `Binaries/`) → move live aside → move new in → remove
/// old, rolling back on failure.
///
/// # Errors
/// [`PluginInstallError`] — hash mismatch, unsafe archive entry, malformed result,
/// or a filesystem/zip error. On any error the live folder is left as it was.
pub fn install_zip(
    editor_root: &Path,
    plugin: &str,
    zip_path: &Path,
    expected_sha256_hex: &str,
) -> Result<()> {
    let got = file_sha256_hex(zip_path)?;
    if !got.eq_ignore_ascii_case(expected_sha256_hex) {
        return Err(PluginInstallError::HashMismatch {
            expected: expected_sha256_hex.to_string(),
            got,
        });
    }
    let dest = plugin_dir(editor_root, plugin);
    let staging = fresh_staging(&dest, plugin)?;
    if let Err(e) = extract_into(zip_path, &staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }
    swap_into_place(&staging, &dest, plugin)
}

/// Sideload a plugin's editor binaries straight from a local build tree into an
/// editor install — the author's unsigned dev path. Copies `<plugin>.uplugin`,
/// each `Binaries/Win64/UE4Editor-*.dll` + `UE4Editor.modules`, and the plugin's
/// `Content/` (if any) — skipping the `UE4-*`/`UE4Server-*` game/server DLL
/// variants and `.pdb` symbols — then swaps them into place like [`install_zip`].
///
/// `build_root` is the build tree's project dir (the one holding `Plugins/`, e.g.
/// `C:\UnrealTournament\UnrealTournament`).
///
/// # Errors
/// [`PluginInstallError`] — no editor DLL found in the build tree for `plugin`
/// ([`PluginInstallError::NotAPlugin`]), or a filesystem error. The live folder is
/// left as it was on failure.
pub fn sideload_from_build(build_root: &Path, editor_root: &Path, plugin: &str) -> Result<()> {
    let dest = plugin_dir(editor_root, plugin);
    let staging = fresh_staging(&dest, plugin)?;
    if let Err(e) = stage_from_build(build_root, plugin, &staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }
    swap_into_place(&staging, &dest, plugin)
}

/// Create a fresh `.<plugin>.staging.<pid>` sibling of `dest`, sweeping this
/// plugin's leftover staging/backup dirs first. Returns the staging path.
fn fresh_staging(dest: &Path, plugin: &str) -> Result<PathBuf> {
    let plugins_dir = dest
        .parent()
        .ok_or_else(|| PluginInstallError::NoPluginsDir(dest.to_path_buf()))?
        .to_path_buf();
    fs::create_dir_all(&plugins_dir)?;
    sweep_leftovers(&plugins_dir, plugin);
    let staging = plugins_dir.join(format!(".{plugin}.staging.{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;
    Ok(staging)
}

/// Copy a plugin's editor-target file set from the build tree into `staging`:
/// `<plugin>.uplugin`, every `Binaries/Win64/UE4Editor-*.dll` + `UE4Editor.modules`
/// (NOT the `UE4-*`/`UE4Server-*` game/server variants, NOT `.pdb`), and the
/// plugin's `Content/` tree if present.
fn stage_from_build(build_root: &Path, plugin: &str, staging: &Path) -> Result<()> {
    let src = build_root.join("Plugins").join(plugin);

    let uplugin = src.join(format!("{plugin}.uplugin"));
    if uplugin.is_file() {
        fs::copy(&uplugin, staging.join(format!("{plugin}.uplugin")))?;
    }

    // Binaries/Win64: the editor DLLs + the .modules only.
    let src_win64 = src.join("Binaries").join("Win64");
    let dst_win64 = staging.join("Binaries").join("Win64");
    let mut copied_editor_dll = false;
    if let Ok(entries) = fs::read_dir(&src_win64) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let keep = (name.starts_with("UE4Editor-") && name.ends_with(".dll"))
                || name == "UE4Editor.modules";
            if keep {
                fs::create_dir_all(&dst_win64)?;
                fs::copy(entry.path(), dst_win64.join(name.as_ref()))?;
                if name.ends_with(".dll") {
                    copied_editor_dll = true;
                }
            }
        }
    }
    // No editor DLL → there's nothing to sync for this plugin (e.g. it wasn't
    // built for the editor target, or the name was wrong).
    if !copied_editor_dll {
        return Err(PluginInstallError::NotAPlugin);
    }

    // The plugin's own Content/, if any (distinct from the project's Content).
    let src_content = src.join("Content");
    if src_content.is_dir() {
        copy_tree(&src_content, &staging.join("Content"))?;
    }
    Ok(())
}

/// Recursively copy `src` dir into `dst` as real-file copies (no symlinks).
fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Validate `staging` is a well-formed plugin (`<plugin>.uplugin` + `Binaries/`),
/// then atomically swap it into `dest` (move live aside to `.<plugin>.old.<pid>`,
/// move staging in, remove old; roll back on failure). Mirrors
/// [`crate::plugin_install::install_plugin_zip`]'s swap.
fn swap_into_place(staging: &Path, dest: &Path, plugin: &str) -> Result<()> {
    if !staging.join(format!("{plugin}.uplugin")).is_file() || !staging.join("Binaries").is_dir() {
        let _ = fs::remove_dir_all(staging);
        return Err(PluginInstallError::NotAPlugin);
    }
    let plugins_dir = dest.parent().unwrap_or(dest);
    let backup = plugins_dir.join(format!(".{plugin}.old.{}", std::process::id()));
    let had_existing = dest.exists();
    if had_existing {
        if backup.exists() {
            fs::remove_dir_all(&backup).map_err(|e| annotate(e, "remove stale backup", &backup))?;
        }
        rename_with_retry(dest, &backup).map_err(|e| annotate(e, "move existing aside", dest))?;
    }
    match rename_with_retry(staging, dest).map_err(|e| annotate(e, "move new into place", staging))
    {
        Ok(()) => {
            if had_existing {
                remove_leftover_dir(&backup);
            }
            Ok(())
        }
        Err(e) => {
            if had_existing {
                let _ = fs::rename(&backup, dest);
            }
            let _ = fs::remove_dir_all(staging);
            Err(PluginInstallError::Io(e))
        }
    }
}

/// Best-effort removal of this plugin's `.<plugin>.staging.*` / `.<plugin>.old.*`
/// leftovers in `plugins_dir` from interrupted prior runs.
fn sweep_leftovers(plugins_dir: &Path, plugin: &str) {
    let Ok(entries) = fs::read_dir(plugins_dir) else {
        return;
    };
    let staging_prefix = format!(".{plugin}.staging.");
    let old_prefix = format!(".{plugin}.old.");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&staging_prefix) || name.starts_with(&old_prefix) {
            remove_leftover_dir(&entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    /// A flat-root editor-plugin zip: `<plugin>.uplugin` + one editor DLL + the
    /// `.modules`, STORED (no compression-method dependency).
    fn make_editor_zip(path: &Path, plugin: &str, dll: &[u8]) {
        let mut buf: Vec<u8> = Vec::new();
        {
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            let mut w = zip::ZipWriter::new(io::Cursor::new(&mut buf));
            w.start_file(format!("{plugin}.uplugin"), opts).unwrap();
            w.write_all(br#"{"FriendlyName":"x"}"#).unwrap();
            w.add_directory("Binaries/Win64/", opts).unwrap();
            w.start_file(format!("Binaries/Win64/UE4Editor-{plugin}.dll"), opts)
                .unwrap();
            w.write_all(dll).unwrap();
            w.start_file("Binaries/Win64/UE4Editor.modules", opts)
                .unwrap();
            w.write_all(br#"{"BuildId":"x","Modules":{}}"#).unwrap();
            w.finish().unwrap();
        }
        fs::write(path, &buf).unwrap();
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(bytes);
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn install_zip_lands_files_and_folder_fingerprint_equals_the_zip() {
        let tmp = TempDir::new().unwrap();
        let zip = tmp.path().join("NetcodePlus-editor.zip");
        make_editor_zip(&zip, "NetcodePlus", b"editor-dll-327");
        let zbytes = fs::read(&zip).unwrap();
        let editor_root = tmp.path().join("Editor");

        install_zip(&editor_root, "NetcodePlus", &zip, &sha256_hex(&zbytes)).unwrap();

        let dir = plugin_dir(&editor_root, "NetcodePlus");
        assert!(dir.join("NetcodePlus.uplugin").is_file());
        let dll = dir.join("Binaries/Win64/UE4Editor-NetcodePlus.dll");
        assert_eq!(fs::read(&dll).unwrap(), b"editor-dll-327");
        assert!(dir.join("Binaries/Win64/UE4Editor.modules").is_file());

        // The whole drift story relies on this equality.
        assert_eq!(
            content_hash(&editor_root, "NetcodePlus"),
            zip_content_hash(&zip),
            "a clean install's folder fingerprint must equal the zip's"
        );
    }

    #[test]
    fn install_zip_rejects_a_bad_hash() {
        let tmp = TempDir::new().unwrap();
        let zip = tmp.path().join("x.zip");
        make_editor_zip(&zip, "UTVehicles", b"dll");
        let err = install_zip(
            &tmp.path().join("Editor"),
            "UTVehicles",
            &zip,
            &"0".repeat(64),
        )
        .unwrap_err();
        assert!(matches!(err, PluginInstallError::HashMismatch { .. }));
    }

    #[test]
    fn install_zip_replaces_and_clears_stale_game_dll() {
        // A prior editor tree with an over-copied game DLL; installing the
        // editor-only zip must swap the whole dir, dropping the stray.
        let tmp = TempDir::new().unwrap();
        let editor_root = tmp.path().join("Editor");
        let dir = plugin_dir(&editor_root, "NetcodePlus");
        fs::create_dir_all(dir.join("Binaries/Win64")).unwrap();
        fs::write(
            dir.join("Binaries/Win64/UE4-NetcodePlus.dll"),
            b"stale-game",
        )
        .unwrap();

        let zip = tmp.path().join("z.zip");
        make_editor_zip(&zip, "NetcodePlus", b"editor-dll");
        let zbytes = fs::read(&zip).unwrap();
        install_zip(&editor_root, "NetcodePlus", &zip, &sha256_hex(&zbytes)).unwrap();

        assert!(
            !dir.join("Binaries/Win64/UE4-NetcodePlus.dll").exists(),
            "the whole-dir swap must clear the stale game DLL"
        );
        assert!(dir
            .join("Binaries/Win64/UE4Editor-NetcodePlus.dll")
            .is_file());
    }

    #[test]
    fn sideload_from_build_copies_editor_files_only() {
        let tmp = TempDir::new().unwrap();
        // A build tree: Plugins/UTVehicles with editor + game + server + pdb.
        let build = tmp.path().join("Build");
        let win64 = build.join("Plugins/UTVehicles/Binaries/Win64");
        fs::create_dir_all(&win64).unwrap();
        fs::write(build.join("Plugins/UTVehicles/UTVehicles.uplugin"), b"{}").unwrap();
        fs::write(win64.join("UE4Editor-UTVehicles.dll"), b"ed").unwrap();
        fs::write(win64.join("UE4Editor.modules"), b"{}").unwrap();
        fs::write(win64.join("UE4Editor-UTVehicles.pdb"), b"pdb").unwrap();
        fs::write(win64.join("UE4-UTVehicles-Win64-Shipping.dll"), b"game").unwrap();
        fs::write(
            win64.join("UE4Server-UTVehicles-Win64-Shipping.dll"),
            b"srv",
        )
        .unwrap();

        let editor_root = tmp.path().join("Editor");
        sideload_from_build(&build, &editor_root, "UTVehicles").unwrap();

        let dst = plugin_dir(&editor_root, "UTVehicles").join("Binaries/Win64");
        assert!(dst.join("UE4Editor-UTVehicles.dll").is_file());
        assert!(dst.join("UE4Editor.modules").is_file());
        // game/server/pdb are NOT copied.
        assert!(!dst.join("UE4Editor-UTVehicles.pdb").exists());
        assert!(!dst.join("UE4-UTVehicles-Win64-Shipping.dll").exists());
        assert!(!dst.join("UE4Server-UTVehicles-Win64-Shipping.dll").exists());
    }

    #[test]
    fn sideload_from_build_without_an_editor_dll_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let build = tmp.path().join("Build");
        let win64 = build.join("Plugins/TeamArena/Binaries/Win64");
        fs::create_dir_all(&win64).unwrap();
        fs::write(build.join("Plugins/TeamArena/TeamArena.uplugin"), b"{}").unwrap();
        // Only a game DLL — no UE4Editor-*.dll.
        fs::write(win64.join("UE4-TeamArena-Win64-Shipping.dll"), b"game").unwrap();
        let err = sideload_from_build(&build, &tmp.path().join("Editor"), "TeamArena").unwrap_err();
        assert!(matches!(err, PluginInstallError::NotAPlugin));
    }

    #[test]
    fn build_tree_plugins_lists_only_editor_built_ones() {
        let tmp = TempDir::new().unwrap();
        let bt = tmp.path();
        // NetcodePlus: has an editor DLL → listed.
        let np = bt.join("Plugins/NetcodePlus/Binaries/Win64");
        fs::create_dir_all(&np).unwrap();
        fs::write(np.join("UE4Editor-NetcodePlus.dll"), b"x").unwrap();
        // TeamArena: only a game DLL → excluded.
        let ta = bt.join("Plugins/TeamArena/Binaries/Win64");
        fs::create_dir_all(&ta).unwrap();
        fs::write(ta.join("UE4-TeamArena-Win64-Shipping.dll"), b"x").unwrap();
        // Online: no Binaries at all → excluded.
        fs::create_dir_all(bt.join("Plugins/Online")).unwrap();

        assert_eq!(build_tree_plugins(bt), vec!["NetcodePlus".to_string()]);
        // A tree with no Plugins/ → empty.
        assert!(build_tree_plugins(&tmp.path().join("nope")).is_empty());
    }

    #[test]
    fn is_stock_plugin_matches_case_insensitively() {
        assert!(is_stock_plugin("CorsairRGB"));
        assert!(is_stock_plugin("razerchroma"));
        assert!(is_stock_plugin("WebMRecord"));
        // The author's plugins are not stock.
        assert!(!is_stock_plugin("NetcodePlus"));
        assert!(!is_stock_plugin("UTVehicles"));
        assert!(!is_stock_plugin("LiandriMapForge"));
    }

    fn entry(version: u32, engine_build_id: Option<&str>) -> EditorPluginEntry {
        EditorPluginEntry {
            version,
            url: "https://x.invalid/z.zip".into(),
            sha256: ncp_manifest::Sha256Digest::from_bytes([0u8; 32]),
            size_bytes: 1,
            notes_url: None,
            engine_build_id: engine_build_id.map(str::to_string),
            engine_changelist: None,
        }
    }

    fn signed(version: u32) -> SyncedPlugin {
        SyncedPlugin {
            source: SyncSource::Signed {
                release_version: version,
            },
            version,
            build_id: None,
            changelist: None,
            content_hash: None,
            synced_at_ms: 0,
        }
    }

    fn localdev() -> SyncedPlugin {
        SyncedPlugin {
            source: SyncSource::LocalDev {
                build_tree: PathBuf::from("C:/Build"),
            },
            version: 0,
            build_id: None,
            changelist: None,
            content_hash: None,
            synced_at_ms: 0,
        }
    }

    #[test]
    fn plan_install_update_uptodate_and_pin() {
        use EditorPluginAction::*;
        // Not installed → Install.
        assert_eq!(
            plan_editor_plugin(None, &entry(327, None), None).action,
            Install
        );
        // Older signed → Update.
        assert_eq!(
            plan_editor_plugin(Some(&signed(326)), &entry(327, None), None).action,
            Update
        );
        // Equal or newer signed → UpToDate (a manifest rollback never downgrades).
        assert_eq!(
            plan_editor_plugin(Some(&signed(327)), &entry(327, None), None).action,
            UpToDate
        );
        assert_eq!(
            plan_editor_plugin(Some(&signed(328)), &entry(327, None), None).action,
            UpToDate
        );
        // A LocalDev sideload is pinned regardless of version.
        assert_eq!(
            plan_editor_plugin(Some(&localdev()), &entry(999, None), None).action,
            PinnedLocalDev
        );
    }

    #[test]
    fn plan_flags_engine_mismatch_only_when_both_known_and_differ() {
        // Differing BuildIds → warn.
        assert!(
            plan_editor_plugin(None, &entry(327, Some("cc4a0b0a")), Some("7a4ea563"))
                .engine_mismatch
        );
        // Same (case-insensitive) → no warn.
        assert!(
            !plan_editor_plugin(None, &entry(327, Some("CC4A0B0A")), Some("cc4a0b0a"))
                .engine_mismatch
        );
        // Either side unknown → no warn (can't compare).
        assert!(!plan_editor_plugin(None, &entry(327, None), Some("7a4ea563")).engine_mismatch);
        assert!(!plan_editor_plugin(None, &entry(327, Some("cc4a0b0a")), None).engine_mismatch);
    }
}
