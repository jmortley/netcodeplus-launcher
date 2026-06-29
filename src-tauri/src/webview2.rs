//! Pre-flight check for the Microsoft Edge WebView2 runtime.
//!
//! On Windows a Tauri app renders its entire UI inside the Edge **WebView2**
//! runtime (`msedgewebview2.exe`). If that runtime is absent or broken — common
//! on debloated/custom Windows images (Atlas, ReviOS, …) that strip it — Tauri
//! fails to create the webview and the process exits. Because we build with
//! `windows_subsystem = "windows"`, that failure goes to a stderr nobody sees,
//! so the user just sees the launcher "do nothing". This module detects the
//! missing runtime BEFORE we touch Tauri and shows a native message box (the
//! webview can't draw one) pointing at the runtime download, instead of dying
//! silently. It is the only thing that can communicate this failure, since the
//! in-app updater can't help a machine whose webview won't render.
//!
//! No-op on every non-Windows target.

/// Microsoft's "Evergreen" WebView2 Runtime download page.
#[cfg(windows)]
const WEBVIEW2_DOWNLOAD_URL: &str = "https://developer.microsoft.com/microsoft-edge/webview2/";

/// Verify the WebView2 runtime is present. On Windows, if it is missing, pop a
/// native dialog with the download link, best-effort open that page in the
/// user's default browser (their browser works — it's WebView2 that's missing),
/// and exit cleanly. Must be called before building Tauri. No-op off Windows.
#[cfg(windows)]
pub fn ensure_webview2_runtime() {
    // Test/support hatch: `NCP_FORCE_WEBVIEW2_MISSING=1` (or `true`) forces the
    // missing-runtime path on a machine that HAS the runtime, so the failure UX
    // (dialog + download-page open + clean exit) can be dogfooded without a
    // debloated image. Unset / any other value → normal detection.
    let forced_missing = std::env::var("NCP_FORCE_WEBVIEW2_MISSING")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if forced_missing || !webview2_runtime_present() {
        warn_missing_and_exit();
    }
}

#[cfg(not(windows))]
pub fn ensure_webview2_runtime() {}

/// UTF-16, NUL-terminated — the form the Win32 `*W` APIs expect.
#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// True if the Evergreen WebView2 Runtime is registered (per-machine or
/// per-user). Microsoft's documented detection: the runtime records its version
/// under `EdgeUpdate\Clients\{F3017226-…}` as `pv`. Per-machine lives in
/// `HKLM\…\WOW6432Node` (EdgeUpdate is a 32-bit-registered app, even on x64),
/// per-user in `HKCU`. A present, non-"0.0.0.0" `pv` in either ⇒ installed.
/// <https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution#detect-if-a-webview2-runtime-is-already-installed>
#[cfg(windows)]
fn webview2_runtime_present() -> bool {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    const CLIENT: &str = "Microsoft\\EdgeUpdate\\Clients\\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
    let machine = format!("SOFTWARE\\WOW6432Node\\{}", CLIENT);
    let user = format!("Software\\{}", CLIENT);

    let installed = |pv: String| !pv.is_empty() && pv != "0.0.0.0";
    read_pv(HKEY_LOCAL_MACHINE, &machine).is_some_and(installed)
        || read_pv(HKEY_CURRENT_USER, &user).is_some_and(installed)
}

/// Read the `pv` (version) REG_SZ value under `subkey`, or `None` if absent.
#[cfg(windows)]
#[allow(unsafe_code)] // One contained RegGetValueW call: all pointers are local,
                      // the output buffer is fixed-size with the length passed in
                      // bytes, and the result is validated before use.
fn read_pv(hkey: windows_sys::Win32::System::Registry::HKEY, subkey: &str) -> Option<String> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_SZ};

    let subkey_w = to_wide(subkey);
    let value_w = to_wide("pv");
    // A version string ("120.0.2210.91") is short; 128 u16 is generous. `cb` is
    // in BYTES (in: capacity, out: bytes written including the NUL terminator).
    let mut buf = [0u16; 128];
    let mut cb = std::mem::size_of_val(&buf) as u32;

    // SAFETY: `subkey_w`/`value_w` are NUL-terminated wide strings kept alive for
    // the call; `buf`/`cb` describe a correctly sized local output buffer; the
    // type-out pointer is null (unused). RegGetValueW writes at most `cb` bytes.
    let rc = unsafe {
        RegGetValueW(
            hkey,
            subkey_w.as_ptr(),
            value_w.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut cb,
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }

    // `cb` = bytes written including the trailing NUL. Convert to a u16 count and
    // drop the NUL, clamped to the buffer in case of a short/odd report.
    let len = (cb as usize / std::mem::size_of::<u16>())
        .saturating_sub(1)
        .min(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// Show the "WebView2 required" dialog, open the download page best-effort, and
/// exit. Never returns.
#[cfg(windows)]
#[allow(unsafe_code)] // Contained MessageBoxW/ShellExecuteW calls with local,
                      // NUL-terminated wide-string args and null HWND/dirs.
fn warn_missing_and_exit() -> ! {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
    };

    let caption = to_wide("NetcodePlus Launcher \u{2014} WebView2 required");
    let text = to_wide(&format!(
        "The Microsoft Edge WebView2 Runtime is required to run the NetcodePlus \
         launcher, and it is missing from this PC.\n\n\
         This is common on debloated / custom Windows images (Atlas, ReviOS, \u{2026}) \
         that strip it out. It has nothing to do with your default browser.\n\n\
         Click OK to open the download page, then install the \"Evergreen \
         Standalone Installer\" (x64) and relaunch:\n\n{}",
        WEBVIEW2_DOWNLOAD_URL
    ));

    // SAFETY: null owner HWND; `caption`/`text` are local NUL-terminated wide
    // strings that outlive the call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
        );
    }

    // Best-effort: open the download page in the default browser. The browser
    // works (it's WebView2 that's missing), so this is the one-click fix path.
    let op = to_wide("open");
    let url = to_wide(WEBVIEW2_DOWNLOAD_URL);
    // SAFETY: null owner HWND / params / dir; `op`/`url` are local NUL-terminated
    // wide strings that outlive the call. Return value (HINSTANCE) intentionally
    // ignored — failure to open the browser is non-fatal.
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            url.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // SW_SHOWNORMAL
        );
    }

    std::process::exit(1);
}
