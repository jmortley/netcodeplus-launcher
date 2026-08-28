//! Headless privileged workers that write into a protected UT4 install
//! location (e.g. `Program Files`): the NetcodePlus plugin
//! (`--elevated-install`) and the UT4-OpenAL binaries overlay
//! (`--elevated-install-openal`). The unelevated parent relaunches the
//! launcher via `runas` (one UAC prompt), and `main` intercepts the flag
//! BEFORE Tauri starts so the elevated instance is headless and does ONLY the
//! privileged extract — never the GUI or the game (which must not run as
//! admin).
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

/// Run the elevated UT4-OpenAL worker: the **binaries overlay only**
/// (`Win64/**` → `<root>/Engine/Binaries/Win64/`). The `%AppData%` config half
/// stays in the unelevated parent — the roaming dir is user-writable, and
/// keeping the elevated surface to the one write that needs it is the point.
///
/// Args: `--zip <path>`, `--manifest <path>`, `--sig <path>`, `--root <path>`,
/// `--rate <44100|48000>`. Same trust boundary as the plugin worker: the
/// manifest + signature are re-verified against the compiled-in trust root
/// (with the persisted replay floor), the expected ZIP hash comes from the
/// VERIFIED manifest's `openal` entry (never a caller argument), the ZIP is
/// re-hashed, and the root must be a genuine UT4 install. Exit `0` = installed.
/// Run the elevated **UT4AC** worker: extract the anti-cheat module into
/// `<root>/UnrealTournament/Plugins/UT4AC/`. Needed because the default UT4
/// install lives under Program Files, which an unelevated launcher cannot
/// write — the plugin installer has always deferred to an elevated pass on
/// `ERROR_ACCESS_DENIED`, and the UT4AC path shipped without inheriting it
/// (fixed in 1.7.6).
///
/// Args: `--zip <path>`, `--manifest <path>`, `--sig <path>`, `--root <path>`.
///
/// Identical trust boundary to the plugin and OpenAL workers, and for the same
/// reason: the manifest + detached signature are re-verified here against the
/// compiled-in trust root (with the persisted replay floor), the expected ZIP
/// digest is taken from the VERIFIED manifest's `anticheat.ut4ac` entry rather
/// than any argument, the ZIP is re-hashed before extraction, and the target
/// must be a genuine UT4 install root. A local caller who can invoke this
/// worker therefore still cannot make it write bytes of their choosing into a
/// protected directory. Exit `0` = installed.
pub fn run_elevated_install_ut4ac(args: &[String]) -> i32 {
    let mut zip: Option<String> = None;
    let mut manifest_path: Option<String> = None;
    let mut sig_path: Option<String> = None;
    let mut root: Option<String> = None;
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
            "--root" => {
                root = args.get(i + 1).cloned();
                i += 2;
            }
            _ => i += 1,
        }
    }

    let log_path = std::env::temp_dir().join("ncp-elevated-ut4ac.log");
    let mut log = String::new();

    let (Some(zip), Some(manifest_path), Some(sig_path), Some(root)) =
        (zip, manifest_path, sig_path, root)
    else {
        log.push_str(
            "elevated-ut4ac: missing --zip/--manifest/--sig/--root
",
        );
        return finish(&log_path, &log, 125);
    };
    log.push_str(&format!(
        "elevated-ut4ac: zip={zip} root={root}
"
    ));

    let json = match std::fs::read(&manifest_path) {
        Ok(b) => b,
        Err(e) => {
            log.push_str(&format!(
                "cannot read manifest {manifest_path}: {e}
"
            ));
            return finish(&log_path, &log, 125);
        }
    };
    let sig = match std::fs::read_to_string(&sig_path) {
        Ok(s) => s,
        Err(e) => {
            log.push_str(&format!(
                "cannot read signature {sig_path}: {e}
"
            ));
            return finish(&log_path, &log, 125);
        }
    };
    let current_version = match semver::Version::parse(env!("CARGO_PKG_VERSION")) {
        Ok(v) => v,
        Err(e) => {
            log.push_str(&format!(
                "launcher version is not valid semver: {e}
"
            ));
            return finish(&log_path, &log, 125);
        }
    };
    let manifest = match ncp_manifest::Manifest::load_and_verify(
        &json,
        &sig,
        &crate::trust_root::public_key(),
        chrono::Utc::now(),
        &current_version,
        persisted_replay_floor(),
    ) {
        Ok(m) => m,
        Err(e) => {
            log.push_str(&format!(
                "manifest verification FAILED: {e}
"
            ));
            return finish(&log_path, &log, 125);
        }
    };
    let Some(entry) = manifest.anticheat.get("ut4ac") else {
        log.push_str(
            "verified manifest advertises no anticheat.ut4ac entry
",
        );
        return finish(&log_path, &log, 125);
    };
    let expected_sha = entry.sha256.to_string();
    log.push_str(&format!(
        "verified manifest seq={} ut4ac={} sha={expected_sha}
",
        manifest.sequence, entry.version
    ));

    // Only ever write into a genuine UT4 install root, exactly as the plugin
    // worker does — an attacker cannot fabricate one somewhere they could not
    // already write.
    let mod_paks = ncp_host::default_mod_paks_dir().unwrap_or_default();
    let root_path = Path::new(&root);
    if ncp_host::check_install(root_path, mod_paks).is_none() {
        log.push_str(&format!(
            "REJECTED (not a UT4 install root): {root}
"
        ));
        return finish(&log_path, &log, 125);
    }
    match ncp_host::install_ut4ac_zip_verified(Path::new(&zip), root_path, &expected_sha) {
        Ok(()) => {
            log.push_str(&format!(
                "ok: {root}
"
            ));
            finish(&log_path, &log, 0)
        }
        Err(e) => {
            log.push_str(&format!(
                "FAILED: {root}: {e}
"
            ));
            finish(&log_path, &log, 1)
        }
    }
}

pub fn run_elevated_install_openal(args: &[String]) -> i32 {
    let mut zip: Option<String> = None;
    let mut manifest_path: Option<String> = None;
    let mut sig_path: Option<String> = None;
    let mut root: Option<String> = None;
    let mut rate: Option<u32> = None;
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
            "--root" => {
                root = args.get(i + 1).cloned();
                i += 2;
            }
            "--rate" => {
                rate = args.get(i + 1).and_then(|r| r.parse().ok());
                i += 2;
            }
            _ => i += 1,
        }
    }

    let log_path = std::env::temp_dir().join("ncp-elevated-openal.log");
    let mut log = String::new();

    let (Some(zip), Some(manifest_path), Some(sig_path), Some(root), Some(rate)) =
        (zip, manifest_path, sig_path, root, rate)
    else {
        log.push_str("elevated-install-openal: missing --zip/--manifest/--sig/--root/--rate\n");
        return finish(&log_path, &log, 125);
    };
    log.push_str(&format!(
        "elevated-install-openal: zip={zip} root={root} rate={rate}\n"
    ));

    // (1) Re-verify the manifest + signature against the compiled-in trust
    // root; the expected hash comes from the VERIFIED manifest's openal entry.
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
        persisted_replay_floor(),
    ) {
        Ok(m) => m,
        Err(e) => {
            log.push_str(&format!("manifest verification FAILED: {e}\n"));
            return finish(&log_path, &log, 125);
        }
    };
    let Some(openal) = manifest.openal.as_ref() else {
        log.push_str("verified manifest advertises no openal entry\n");
        return finish(&log_path, &log, 125);
    };
    let expected_sha = openal.sha256.to_string();
    log.push_str(&format!(
        "verified manifest seq={} openal {} sha={expected_sha}\n",
        manifest.sequence, openal.version
    ));

    // (2) Only ever write into a genuine UT4 install root.
    let mod_paks = ncp_host::default_mod_paks_dir().unwrap_or_default();
    let root_path = Path::new(&root);
    if ncp_host::check_install(root_path, mod_paks).is_none() {
        log.push_str(&format!("REJECTED (not a UT4 install root): {root}\n"));
        return finish(&log_path, &log, 125);
    }
    match ncp_host::install_openal_binaries_verified(
        Path::new(&zip),
        root_path,
        rate,
        &expected_sha,
    ) {
        Ok(n) => {
            log.push_str(&format!("ok: {n} files into {root}\n"));
            finish(&log_path, &log, 0)
        }
        Err(e) => {
            log.push_str(&format!("FAILED: {e}\n"));
            finish(&log_path, &log, 1)
        }
    }
}
