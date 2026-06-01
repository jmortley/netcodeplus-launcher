// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Elevated install entry point. When the launcher relaunches itself with
    // `runas` to install the plugin into a protected location (e.g. Program
    // Files), it passes `--elevated-install`. We intercept that BEFORE Tauri
    // starts so the elevated instance is headless and does ONLY the privileged
    // extract — never the GUI or the game (which must not run as admin).
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--elevated-install") {
        std::process::exit(run_elevated_install(&args));
    }

    netcodeplus_launcher_lib::run()
}

/// Headless privileged worker: re-verify the plugin ZIP and extract it into
/// each requested install root.
///
/// Args (all required): `--zip <path>`, `--sha256 <hex>` (from the signed
/// manifest), and one or more `--root <install-root>`. Returns a process exit
/// code: `0` = every root installed; otherwise the number of roots that failed
/// (capped at 125, the conventional max exit code). The unelevated parent reads
/// this code and records state only on `0`.
///
/// This worker does NO networking and NO state writes — it re-hashes the
/// already-downloaded ZIP itself (closing the verify→install TOCTOU) and writes
/// only the plugin folders. Keeping the elevated surface this small is the point
/// of elevating just the install rather than the whole launcher.
fn run_elevated_install(args: &[String]) -> i32 {
    // Tiny `--flag value` parser (repeated `--root` allowed). Avoids pulling a
    // CLI dep into the elevated path.
    let mut zip: Option<String> = None;
    let mut sha256: Option<String> = None;
    let mut roots: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--zip" => {
                zip = args.get(i + 1).cloned();
                i += 2;
            }
            "--sha256" => {
                sha256 = args.get(i + 1).cloned();
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

    let (Some(zip), Some(sha256)) = (zip, sha256) else {
        eprintln!("elevated-install: missing --zip or --sha256");
        return 125;
    };
    if roots.is_empty() {
        eprintln!("elevated-install: no --root given");
        return 125;
    }

    let zip_path = std::path::Path::new(&zip);
    let mut failed = 0i32;
    for root in &roots {
        match ncp_host::install_plugin_zip_verified(zip_path, std::path::Path::new(root), &sha256) {
            Ok(()) => eprintln!("elevated-install: ok {root}"),
            Err(e) => {
                eprintln!("elevated-install: FAILED {root}: {e}");
                failed += 1;
            }
        }
    }
    failed.min(125)
}
