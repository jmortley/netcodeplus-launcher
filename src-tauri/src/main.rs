// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Elevated install entry point. When the launcher relaunches itself with
    // `runas` to install the plugin into a protected location (e.g. Program
    // Files), it passes `--elevated-install`. We intercept that BEFORE Tauri
    // starts so the elevated instance is headless and does ONLY the privileged
    // extract — never the GUI or the game (which must not run as admin).
    //
    // The worker lives in the library crate (so it can reach the compiled-in
    // trust root) and re-verifies the manifest + signature itself rather than
    // trusting any argument from the lower-integrity caller. See
    // `elevated::run_elevated_install`.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--elevated-install") {
        std::process::exit(netcodeplus_launcher_lib::run_elevated_install(&args));
    }
    // Same pattern for the UT4-OpenAL binaries overlay into a protected install.
    if args.iter().any(|a| a == "--elevated-install-openal") {
        std::process::exit(netcodeplus_launcher_lib::run_elevated_install_openal(&args));
    }
    // Same handoff for the UT4AC module: the default install lives under
    // Program Files, so the unelevated write is denied (os error 5).
    if args.iter().any(|a| a == "--elevated-install-ut4ac") {
        std::process::exit(netcodeplus_launcher_lib::run_elevated_install_ut4ac(&args));
    }

    netcodeplus_launcher_lib::run()
}
