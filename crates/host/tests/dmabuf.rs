//! Integration tests for [`ncp_host::apply_webview_dmabuf_default`] (Linux only).
//!
//! The function reads process-global environment (`XDG_CONFIG_HOME` via
//! `dirs::config_dir()`, `WEBKIT_DISABLE_DMABUF_RENDERER`) and mutates the
//! latter, so every scenario runs inside ONE `#[test]` — the parallel test
//! runner would race otherwise. This file is its own test binary, so no other
//! tests share the process environment.

#![cfg(target_os = "linux")]

use ncp_host::{apply_webview_dmabuf_default, state, LauncherState};
use tempfile::TempDir;

const VAR: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
const APP_ID: &str = "org.netcodeplus.launcher";

/// Point `dirs::config_dir()` at a fresh tempdir and clear `VAR`, so each
/// scenario starts from a first-run environment.
fn fresh_config() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::env::set_var("XDG_CONFIG_HOME", dir.path());
    std::env::remove_var(VAR);
    dir
}

/// Seed `<config>/<APP_ID>/state.json` with the given `linux_gpu_accel`.
fn seed_state(dir: &TempDir, gpu_accel: Option<bool>) {
    let app_dir = dir.path().join(APP_ID);
    std::fs::create_dir_all(&app_dir).expect("app config dir");
    let s = LauncherState {
        linux_gpu_accel: gpu_accel,
        ..LauncherState::default()
    };
    state::write(&app_dir.join("state.json"), &s).expect("write state");
}

#[test]
fn dmabuf_default_and_precedence() {
    // First run: no state.json at all -> renderer disabled by default.
    let _d = fresh_config();
    apply_webview_dmabuf_default(APP_ID);
    assert_eq!(std::env::var(VAR).as_deref(), Ok("1"), "no state file");

    // Saved state without an opt-in (None) -> disabled.
    let d = fresh_config();
    seed_state(&d, None);
    apply_webview_dmabuf_default(APP_ID);
    assert_eq!(
        std::env::var(VAR).as_deref(),
        Ok("1"),
        "linux_gpu_accel: None"
    );

    // Explicit opt-out (Some(false)) -> disabled.
    let d = fresh_config();
    seed_state(&d, Some(false));
    apply_webview_dmabuf_default(APP_ID);
    assert_eq!(
        std::env::var(VAR).as_deref(),
        Ok("1"),
        "linux_gpu_accel: false"
    );

    // Opt-in (Some(true)) -> GPU path stays on, var left unset.
    let d = fresh_config();
    seed_state(&d, Some(true));
    apply_webview_dmabuf_default(APP_ID);
    assert!(std::env::var_os(VAR).is_none(), "linux_gpu_accel: true");

    // A pre-existing env var always wins, even over a saved opt-out:
    // the user forcing "0" on the CLI must not be flipped to "1".
    let d = fresh_config();
    seed_state(&d, Some(false));
    std::env::set_var(VAR, "0");
    apply_webview_dmabuf_default(APP_ID);
    assert_eq!(
        std::env::var(VAR).as_deref(),
        Ok("0"),
        "explicit env override"
    );

    // ...and the inverse direction: a pre-set "1" with a saved opt-in must
    // survive — the opt-in path may only *not set* the var, never clear it.
    let d = fresh_config();
    seed_state(&d, Some(true));
    std::env::set_var(VAR, "1");
    apply_webview_dmabuf_default(APP_ID);
    assert_eq!(
        std::env::var(VAR).as_deref(),
        Ok("1"),
        "env override + opt-in"
    );

    // Corrupt state.json -> read error is swallowed, safe default applies.
    let d = fresh_config();
    let app_dir = d.path().join(APP_ID);
    std::fs::create_dir_all(&app_dir).expect("app config dir");
    std::fs::write(app_dir.join("state.json"), b"{ not json").expect("corrupt file");
    apply_webview_dmabuf_default(APP_ID);
    assert_eq!(std::env::var(VAR).as_deref(), Ok("1"), "corrupt state file");
}
