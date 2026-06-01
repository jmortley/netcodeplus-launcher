//! Headless privileged worker that installs the NetcodePlus plugin into a
//! protected location (e.g. `Program Files`). The unelevated parent relaunches
//! the launcher via `runas` (one UAC prompt) with `--elevated-install`, and
//! `main` intercepts that BEFORE Tauri starts so this runs headless and does
//! ONLY the privileged extract — never the GUI or the game (which must not run
//! as admin).
//!
//! # Trust boundary (why this re-verifies everything)
//!
//! This worker runs at HIGH integrity (admin) but is invoked by a LOWER
//! integrity caller, so it must trust NOTHING the caller hands it. A local
//! attacker with user-level code execution could run
//! `launcher.exe --elevated-install --zip <theirs> --root <chosen> …` and, if
//! the user approves the UAC prompt, obtain an admin-privileged write of
//! attacker-controlled bytes — a local privilege-escalation primitive. So the
//! worker re-establishes trust from first principles rather than trusting its
//! arguments:
//!
//! 1. It re-verifies the passed manifest + detached signature against the
//!    **compiled-in trust root** ([`crate::trust_root`]) and takes the expected
//!    plugin SHA-256 from the *verified* manifest — never from a caller-supplied
//!    hash. Attacker bytes can't match a manifest they can't sign.
//! 2. It only writes into roots that are **genuine UT4 installs** on disk
//!    ([`ncp_host::check_install`]) — an attacker can't fabricate one in a
//!    protected location they couldn't already write to.
//!
//! It does NO networking and NO state writes: it reads the already-downloaded
//! manifest / signature / zip as files (re-hashing the zip against the verified
//! manifest closes the verify→install TOCTOU) and writes only the plugin
//! folders. Keeping the elevated surface this small is the whole point of
//! elevating the install rather than the launcher.

use std::path::Path;

/// Write the diagnostic log and return `code`. The elevated child is a
/// GUI-subsystem process, so its stderr is invisible to the parent; the log in
/// `%TEMP%` keeps a failed elevated install diagnosable after the fact.
fn finish(log_path: &Path, log: &str, code: i32) -> i32 {
    let _ = std::fs::write(log_path, log);
    code
}

/// The Tauri app identifier — must match `tauri.conf.json`. Used to locate the
/// persisted state file independently, since the worker has no `AppHandle`.
const APP_IDENTIFIER: &str = "org.netcodeplus.launcher";

/// Read the persisted replay floor (`highest_manifest_sequence`) from the
/// launcher's own state file, located the way Tauri's `app_config_dir` does
/// (`%APPDATA%\<identifier>\state.json`). Returns 0 if it can't be read (first
/// run / absent) — the same baseline the parent uses.
///
/// The worker reads this **itself** rather than taking it as an argument: a
/// caller-supplied floor would be attacker-controlled, defeating the point. It
/// enforces the same floor the parent does, so a local caller can't downgrade
/// the plugin to an older, signed-but-superseded build by invoking the worker
/// directly. (A local attacker who can also edit the user-writable state file
/// could still lower the floor — that is a broader limitation of where the
/// floor lives, not specific to this path; the floor is primarily a defense
/// against network replay, where the worker is not the attack surface.)
fn persisted_replay_floor() -> u64 {
    std::env::var_os("APPDATA")
        .map(|appdata| {
            std::path::PathBuf::from(appdata)
                .join(APP_IDENTIFIER)
                .join("state.json")
        })
        .and_then(|p| ncp_host::state::read(&p).ok().flatten())
        .map(|s| s.highest_manifest_sequence)
        .unwrap_or(0)
}

/// Run the elevated install worker.
///
/// Args: `--zip <path>`, `--manifest <path>`, `--sig <path>`, `--channel
/// <name>`, and one or more `--root <install-root>`. Returns a process exit
/// code: `0` = every requested root installed; otherwise the number of roots
/// that failed or were rejected (capped at 125). The unelevated parent records
/// state only on `0`.
pub fn run_elevated_install(args: &[String]) -> i32 {
    // Tiny `--flag value` parser (repeated `--root` allowed); avoids a CLI dep
    // in the elevated path.
    let mut zip: Option<String> = None;
    let mut manifest_path: Option<String> = None;
    let mut sig_path: Option<String> = None;
    let mut channel: Option<String> = None;
    let mut roots: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--zip" => {
                zip = args.get(i + 1).cloned();
                i += 2;
            }
            "--manifest" => {
                manifest_path = args.get(i + 1).cloned();
                i += 2;
            }
            "--sig" => {
                sig_path = args.get(i + 1).cloned();
                i += 2;
            }
            "--channel" => {
                channel = args.get(i + 1).cloned();
                i += 2;
            }
            "--root" => {
                if let Some(r) = args.get(i + 1) {
                    roots.push(r.clone());
                }
                i += 2;
            }
            _ => i += 1,
        }
    }

    let log_path = std::env::temp_dir().join("ncp-elevated-install.log");
    let mut log = String::new();

    let (Some(zip), Some(manifest_path), Some(sig_path), Some(channel)) =
        (zip, manifest_path, sig_path, channel)
    else {
        log.push_str("elevated-install: missing --zip/--manifest/--sig/--channel\n");
        return finish(&log_path, &log, 125);
    };
    if roots.is_empty() {
        log.push_str("elevated-install: no --root given\n");
        return finish(&log_path, &log, 125);
    }
    log.push_str(&format!(
        "elevated-install: zip={zip} channel={channel} roots={roots:?}\n"
    ));

    // (1) Re-verify the manifest + signature against the compiled-in trust root,
    // then take the expected plugin hash from the VERIFIED manifest — not from
    // any caller argument. Read both as files: no networking in this worker.
    let json = match std::fs::read(&manifest_path) {
        Ok(b) => b,
        Err(e) => {
            log.push_str(&format!("cannot read manifest {manifest_path}: {e}\n"));
            return finish(&log_path, &log, 125);
        }
    };
    let sig = match std::fs::read_to_string(&sig_path) {
        Ok(s) => s,
        Err(e) => {
            log.push_str(&format!("cannot read signature {sig_path}: {e}\n"));
            return finish(&log_path, &log, 125);
        }
    };
    let current_version = match semver::Version::parse(env!("CARGO_PKG_VERSION")) {
        Ok(v) => v,
        Err(e) => {
            log.push_str(&format!("launcher version is not valid semver: {e}\n"));
            return finish(&log_path, &log, 125);
        }
    };
    let manifest = match ncp_manifest::Manifest::load_and_verify(
        &json,
        &sig,
        &crate::trust_root::public_key(),
        chrono::Utc::now(),
        &current_version,
        // Enforce the SAME replay floor the parent does (read independently from
        // the persisted state), so a local caller can't downgrade the plugin to
        // an older signed-but-superseded build by invoking this worker directly.
        persisted_replay_floor(),
    ) {
        Ok(m) => m,
        Err(e) => {
            log.push_str(&format!("manifest verification FAILED: {e}\n"));
            return finish(&log_path, &log, 125);
        }
    };
    let Some(plugin) = manifest
        .channels
        .get(&channel)
        .and_then(|c| c.plugin.as_ref())
    else {
        log.push_str(&format!("channel '{channel}' advertises no plugin\n"));
        return finish(&log_path, &log, 125);
    };
    let expected_sha = plugin.sha256.to_string();
    log.push_str(&format!(
        "verified manifest seq={} plugin build={} sha={expected_sha}\n",
        manifest.sequence, plugin.version
    ));

    // (2) Only ever write into a genuine UT4 install root. An attacker can't
    // fabricate one in a protected location they can't already write to.
    let mod_paks = ncp_host::default_mod_paks_dir().unwrap_or_default();
    let zip_path = Path::new(&zip);
    let mut failed = 0i32;
    for root in &roots {
        let root_path = Path::new(root);
        if ncp_host::check_install(root_path, mod_paks.clone()).is_none() {
            log.push_str(&format!("REJECTED (not a UT4 install root): {root}\n"));
            failed += 1;
            continue;
        }
        match ncp_host::install_plugin_zip_verified(zip_path, root_path, &expected_sha) {
            Ok(()) => log.push_str(&format!("ok: {root}\n")),
            Err(e) => {
                log.push_str(&format!("FAILED: {root}: {e}\n"));
                failed += 1;
            }
        }
    }
    log.push_str(&format!("done: {failed} failed\n"));
    finish(&log_path, &log, failed.min(125))
}
