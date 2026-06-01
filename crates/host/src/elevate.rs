//! Windows UAC elevation for the one operation that needs it: installing the
//! plugin into a protected location like `C:\Program Files\...`.
//!
//! The launcher runs at user level by design (no admin). Most installs are
//! user-writable and need none of this. But an Epic-default UT4 install lives
//! under Program Files, where writing requires elevation. Rather than run the
//! whole launcher (and therefore the game) as admin, we elevate **only** the
//! install step: spawn our own exe with an `--elevated-install` subcommand via
//! `ShellExecuteExW`'s `runas` verb, which raises a single UAC prompt, wait for
//! it, and read its exit code. The elevated child does the extract-and-swap and
//! nothing else — no network, no game launch.
//!
//! [`dir_writable`] is the probe the parent uses to decide whether elevation is
//! even needed (skip the prompt entirely for user-writable installs).

use std::path::Path;

/// Whether `dir` is writable by the current (unelevated) process — probed by
/// creating and immediately deleting a uniquely-named temp file inside it.
///
/// Used to decide if a plugin install needs elevation: a user-writable install
/// dir installs silently; a Program Files one comes back `false` and the caller
/// routes through [`run_elevated`]. A non-existent dir counts as not writable
/// here (the caller creates dirs separately).
#[must_use]
pub fn dir_writable(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(format!(".ncp-write-probe.{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Errors from [`run_elevated`].
#[derive(Debug, thiserror::Error)]
pub enum ElevateError {
    /// The user dismissed the UAC prompt (chose "No"). Distinct so the UI can
    /// say "you declined the admin prompt" rather than a scary error.
    #[error("the administrator prompt was declined")]
    Cancelled,
    /// `ShellExecuteExW` failed to launch the elevated process (Win32 error).
    #[error("could not start the elevated helper (os error {0})")]
    Launch(u32),
    /// The elevated process started but we could not obtain its handle or exit
    /// code.
    #[error("lost track of the elevated helper process")]
    LostProcess,
    /// Only on non-Windows builds, where elevation is not implemented.
    #[error("elevation is only supported on Windows")]
    Unsupported,
}

/// Encode `s` as a NUL-terminated UTF-16 buffer for the `*W` Win32 APIs.
#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Quote a single argument per the `CommandLineToArgvW` rules, so the elevated
/// child's `std::env::args()` reconstructs it exactly. Critical because the
/// install roots contain spaces (e.g. `C:\Program Files\UnrealTournament`).
///
/// Rule: wrap in double quotes; a run of N backslashes immediately before a `"`
/// (or the closing quote) must be doubled to 2N, and a literal `"` becomes
/// `\"`. (See MSVC's argv parsing.)
#[cfg(windows)]
fn quote_arg(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => {
                backslashes += 1;
            }
            '"' => {
                // Double the preceding backslashes, then escape the quote.
                for _ in 0..(backslashes * 2 + 1) {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            }
            _ => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push(ch);
            }
        }
    }
    // Trailing backslashes precede the closing quote → double them.
    for _ in 0..(backslashes * 2) {
        out.push('\\');
    }
    out.push('"');
    out
}

/// Join args into a single `ShellExecuteExW` parameter string with correct
/// quoting. (`ShellExecuteExW` takes one parameter string, not an argv.)
#[cfg(windows)]
fn join_params(args: &[String]) -> String {
    args.iter()
        .map(|a| quote_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Relaunch `exe` elevated (UAC) with `args`, wait for it, and return its exit
/// code.
///
/// Raises one UAC prompt. The spawned process runs as administrator, so the
/// caller must pass a subcommand that does **only** the privileged work and
/// exits — never the normal GUI/game path.
///
/// # Errors
/// [`ElevateError::Cancelled`] if the user declines the prompt;
/// [`ElevateError::Launch`] / [`ElevateError::LostProcess`] on Win32 failures.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn run_elevated(exe: &Path, args: &[String]) -> Result<u32, ElevateError> {
    use std::os::raw::c_void;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, WaitForSingleObject, INFINITE,
    };
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW};

    // Win32 constants (stable values; defined inline to avoid extra feature
    // surface). SEE_MASK_NOCLOSEPROCESS keeps hProcess valid so we can wait on
    // it; SW_HIDE because the elevated helper is headless. ERROR_CANCELLED is
    // what GetLastError returns when the user clicks "No" on the UAC prompt.
    const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
    const SW_HIDE: i32 = 0;
    const ERROR_CANCELLED: u32 = 1223;

    let verb = wide("runas");
    let file = wide(&exe.to_string_lossy());
    let params = wide(&join_params(args));

    // Zero-initialise the struct, then set the fields we use. All pointer
    // fields point into `verb`/`file`/`params`, which live until end of fn.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_HIDE;

    // SAFETY: `info` is fully zero-initialised then populated; cbSize is the
    // real struct size; lpVerb/lpFile/lpParameters point to NUL-terminated
    // UTF-16 buffers (`verb`/`file`/`params`) that outlive this call. We pass a
    // valid `&mut info`. ShellExecuteExW reads the struct and, with
    // SEE_MASK_NOCLOSEPROCESS, writes a process handle into `info.hProcess`.
    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok == 0 {
        // SAFETY: GetLastError takes no args and only reads thread-local state.
        let err = unsafe { GetLastError() };
        return Err(if err == ERROR_CANCELLED {
            ElevateError::Cancelled
        } else {
            ElevateError::Launch(err)
        });
    }

    let handle = info.hProcess;
    if handle.is_null() {
        return Err(ElevateError::LostProcess);
    }

    // SAFETY: `handle` is a live process handle (SEE_MASK_NOCLOSEPROCESS asked
    // ShellExecuteExW to leave it open). WaitForSingleObject only reads it.
    let waited = unsafe { WaitForSingleObject(handle, INFINITE) };
    let mut code: u32 = 0;
    let got = if waited == WAIT_OBJECT_0 {
        // SAFETY: `handle` still live; `&mut code` is a valid u32 out-param that
        // GetExitCodeProcess writes the child's exit code into.
        unsafe { GetExitCodeProcess(handle, &mut code) }
    } else {
        0
    };
    // SAFETY: closing the handle we own; not used afterwards.
    unsafe { CloseHandle(handle as *mut c_void) };

    if got == 0 {
        return Err(ElevateError::LostProcess);
    }
    Ok(code)
}

/// Non-Windows stub: elevation is Windows-only (Linux/Proton support is post-v1
/// and uses a native install path anyway).
///
/// # Errors
/// Always [`ElevateError::Unsupported`].
#[cfg(not(windows))]
pub fn run_elevated(_exe: &Path, _args: &[String]) -> Result<u32, ElevateError> {
    Err(ElevateError::Unsupported)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn quote_plain_arg() {
        assert_eq!(quote_arg("hello"), "\"hello\"");
    }

    #[test]
    fn quote_path_with_spaces() {
        assert_eq!(
            quote_arg(r"C:\Program Files\UnrealTournament"),
            "\"C:\\Program Files\\UnrealTournament\""
        );
    }

    #[test]
    fn quote_trailing_backslash_is_doubled() {
        // A trailing backslash before the closing quote must be doubled so the
        // child doesn't read it as escaping the quote.
        assert_eq!(quote_arg(r"C:\dir\"), "\"C:\\dir\\\\\"");
    }

    #[test]
    fn quote_embedded_quote_is_escaped() {
        assert_eq!(quote_arg(r#"a"b"#), "\"a\\\"b\"");
    }

    #[test]
    fn join_params_quotes_each() {
        let args = vec!["--root".to_string(), r"C:\Program Files\X".to_string()];
        assert_eq!(join_params(&args), "\"--root\" \"C:\\Program Files\\X\"");
    }
}

#[cfg(test)]
mod writable_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn writable_dir_probes_true() {
        let tmp = TempDir::new().unwrap();
        assert!(dir_writable(tmp.path()));
    }

    #[test]
    fn missing_dir_probes_false() {
        let tmp = TempDir::new().unwrap();
        assert!(!dir_writable(&tmp.path().join("does-not-exist")));
    }
}
