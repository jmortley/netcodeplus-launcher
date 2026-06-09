//! The Windows app-compatibility "Run as administrator" flag for the game exe.
//!
//! When `UE4-Win64-Shipping.exe` carries the `RUNASADMIN` compatibility layer
//! (set via the exe's Properties → Compatibility, or by a "run games as admin"
//! guide), Windows refuses to start it from the launcher's normal
//! `CreateProcess`, failing with os error 740 (`ERROR_ELEVATION_REQUIRED`). The
//! launcher detects this up front and offers to clear the flag (recommended) or
//! launch elevated anyway.
//!
//! The flag lives per-user under `HKCU\…\AppCompatFlags\Layers`, keyed by the
//! exe's full path, with a value like `~ DISABLEDXMAXIMIZEDWINDOWEDMODE
//! RUNASADMIN`. Clearing it needs no elevation (it's the user's own hive).

#[cfg(windows)]
use std::path::Path;

// Gated to match its only users (`has_runasadmin`/`strip_runasadmin` below);
// otherwise it's dead code on a non-test non-Windows build (e.g. the Linux build).
#[cfg(any(windows, test))]
const RUNASADMIN: &str = "RUNASADMIN";

#[cfg(windows)]
const LAYERS_KEY: &str = r"Software\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\Layers";

/// Whether an `AppCompatFlags\Layers` value contains the `RUNASADMIN` flag.
#[cfg(any(windows, test))]
fn has_runasadmin(value: &str) -> bool {
    value
        .split_whitespace()
        .any(|t| t.eq_ignore_ascii_case(RUNASADMIN))
}

/// Strip `RUNASADMIN` from a layer value, keeping any other flags. Returns
/// `None` when nothing meaningful remains (empty, or just the leading `~`
/// marker) — signalling the whole value should be deleted rather than rewritten.
#[cfg(any(windows, test))]
fn strip_runasadmin(value: &str) -> Option<String> {
    let kept: Vec<&str> = value
        .split_whitespace()
        .filter(|t| !t.eq_ignore_ascii_case(RUNASADMIN))
        .collect();
    if kept.is_empty() || kept.iter().all(|t| *t == "~") {
        None
    } else {
        Some(kept.join(" "))
    }
}

/// Whether the per-user compat layer for `exe` forces it to run as
/// administrator (so the launcher's normal launch hits os error 740).
#[cfg(windows)]
#[must_use]
pub fn requires_admin(exe: &Path) -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let Some(name) = exe.to_str() else {
        return false;
    };
    let Ok(layers) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(LAYERS_KEY) else {
        return false;
    };
    match layers.get_value::<String, _>(name) {
        Ok(value) => has_runasadmin(&value),
        Err(_) => false,
    }
}

/// Remove the `RUNASADMIN` flag from `exe`'s per-user compat layer (preserving
/// any other flags; deleting the value entirely if `RUNASADMIN` was the only
/// one). A no-op if it isn't set. HKCU only, so no elevation is required.
///
/// # Errors
/// Any registry error other than "key/value not found".
#[cfg(windows)]
pub fn clear_requires_admin(exe: &Path) -> std::io::Result<()> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::RegKey;
    let name = exe.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "exe path is not valid UTF-8",
        )
    })?;
    let layers = match RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(LAYERS_KEY, KEY_READ | KEY_WRITE)
    {
        Ok(k) => k,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let value: String = match layers.get_value(name) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    match strip_runasadmin(&value) {
        Some(remaining) => layers.set_value(name, &remaining),
        None => layers.delete_value(name),
    }
}

/// Non-Windows stub: there is no compat-layer flag to read.
#[cfg(not(windows))]
#[must_use]
pub fn requires_admin(_exe: &std::path::Path) -> bool {
    false
}

/// Non-Windows stub: nothing to clear.
#[cfg(not(windows))]
pub fn clear_requires_admin(_exe: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_and_strips_runasadmin() {
        assert!(has_runasadmin(
            "~ DISABLEDXMAXIMIZEDWINDOWEDMODE RUNASADMIN"
        ));
        assert!(has_runasadmin("~ runasadmin")); // case-insensitive
        assert!(!has_runasadmin("~ DISABLEDXMAXIMIZEDWINDOWEDMODE"));

        // Other flags are preserved.
        assert_eq!(
            strip_runasadmin("~ DISABLEDXMAXIMIZEDWINDOWEDMODE RUNASADMIN").as_deref(),
            Some("~ DISABLEDXMAXIMIZEDWINDOWEDMODE")
        );
        // Only RUNASADMIN (with or without the marker) → delete the whole value.
        assert_eq!(strip_runasadmin("~ RUNASADMIN"), None);
        assert_eq!(strip_runasadmin("RUNASADMIN"), None);
    }
}
