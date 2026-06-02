//! Free-disk-space probe — used to warn before the large game-installer
//! download (~10 GB) instead of failing partway through.

use std::path::Path;

/// Bytes available to the caller on the volume containing `dir` (walking up to
/// its nearest existing ancestor, since a not-yet-created download folder still
/// sits on a real volume). `None` when it can't be determined — the caller
/// should then proceed rather than block on an unknown.
#[cfg(windows)]
#[allow(unsafe_code)]
#[must_use]
pub fn available_space(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let mut probe = dir;
    while !probe.exists() {
        probe = probe.parent()?;
    }

    let wide: Vec<u16> = probe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free_to_caller: u64 = 0;
    // SAFETY: `wide` is a NUL-terminated UTF-16 path; `&mut free_to_caller` is a
    // valid u64 out-param. The other two out-params are optional (NULL is allowed
    // by the API) and unused. GetDiskFreeSpaceExW only reads the path and writes
    // the free-bytes value.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_to_caller,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        None
    } else {
        Some(free_to_caller)
    }
}

/// Non-Windows stub: free space isn't probed (the launcher targets Windows).
#[cfg(not(windows))]
#[must_use]
pub fn available_space(_dir: &Path) -> Option<u64> {
    None
}
