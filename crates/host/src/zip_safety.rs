//! Zip-slip guards shared by every archive extractor in this crate.
//!
//! Both the plugin installer ([`crate::plugin_install`]) and the game-installer
//! extractor ([`crate::extract`]) unpack untrusted-*layout* archives — the bytes
//! are SHA-256-verified against the signed manifest before extraction, but a
//! malicious or buggy archive could still name an entry that escapes the
//! destination directory. These two predicates are the single source of truth
//! for "is this entry name safe to join under the destination", so a fix here
//! protects every extractor at once.
//!
//! Deliberately OS-independent: the verdict must be identical whether the
//! launcher is built/run on Windows or Linux, because the same Windows-authored
//! archives feed both.

use std::path::{Component, Path, PathBuf};

/// `true` only when `name` is a flat, relative, safe path to join under the
/// destination: no absolute root, no `..` parent escape, no drive/UNC prefix,
/// no reserved/control characters. Forward and back slashes are both treated as
/// separators (a ZIP authored on Windows may use `\`), and each component is
/// checked. Empty names and bare `.`/`..` are rejected.
pub(crate) fn is_safe_entry(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    // A leading `/` or `\`, or a Windows drive (`C:`) / UNC (`\\`) prefix, means
    // absolute — reject outright.
    let bytes = name.as_bytes();
    if bytes[0] == b'/' || bytes[0] == b'\\' {
        return false;
    }
    if name.len() >= 2 && bytes[1] == b':' {
        return false; // drive-letter absolute
    }
    // Split on BOTH separators and validate every component.
    for comp in name.split(['/', '\\']) {
        if comp.is_empty() {
            // Empty component = a `//`, trailing slash, etc. A trailing slash on
            // a directory entry is normal, so treat empty as a skip-safe
            // separator artifact, NOT a failure.
            continue;
        }
        if comp == ".." || comp == "." {
            return false;
        }
        if comp.contains(['<', '>', '"', '|', '?', '*', ':']) {
            return false;
        }
        if comp.chars().any(char::is_control) {
            return false;
        }
    }
    true
}

/// Defence-in-depth: independently confirm the resolved join stays under
/// `base`, catching anything [`is_safe_entry`] somehow let through. This must be
/// sound on its OWN (not relying on the earlier check), so it re-rejects
/// absolute / drive-prefixed entries rather than letting a leading separator be
/// silently dropped by component-splitting.
pub(crate) fn joined_is_within(base: &Path, entry: &str) -> Option<PathBuf> {
    let bytes = entry.as_bytes();
    // Absolute (leading separator) or drive-letter prefix → reject.
    if entry.is_empty() || bytes[0] == b'/' || bytes[0] == b'\\' {
        return None;
    }
    if entry.len() >= 2 && bytes[1] == b':' {
        return None;
    }
    let mut out = base.to_path_buf();
    for comp in entry.split(['/', '\\']).filter(|c| !c.is_empty()) {
        match Path::new(comp).components().next() {
            // Only ever a plain child name is allowed — no `..`, `.`, root, or
            // prefix component.
            Some(Component::Normal(c)) => out.push(c),
            _ => return None,
        }
    }
    // The final path must still be a descendant of base.
    out.starts_with(base).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_and_absolute_entries() {
        for bad in [
            "../evil.txt",
            "..\\evil.txt",
            "/etc/passwd",
            "\\\\unc\\share\\x",
            "C:\\Windows\\x",
            "a/../../b",
            "foo/../../../bar",
            "bad\0name",
            "name:stream",
        ] {
            assert!(!is_safe_entry(bad), "should reject {bad:?}");
        }
    }

    #[test]
    fn accepts_normal_entries() {
        for ok in [
            "NetcodePlus.uplugin",
            "Binaries/Win64/UE4-NetcodePlus.dll",
            "Binaries\\Win64\\UE4-NetcodePlus.dll",
            "Resources/Icon128.png",
            "Binaries/",
            "UT4 Installer - v1.1.0/UT4_Installer.exe",
        ] {
            assert!(is_safe_entry(ok), "should accept {ok:?}");
        }
    }

    #[test]
    fn joined_within_rejects_escape_and_accepts_child() {
        let base = Path::new("C:/games/Plugins/.staging");
        // Legitimate nested file resolves under base.
        assert!(joined_is_within(base, "Binaries/Win64/x.dll").is_some());
        assert!(joined_is_within(base, "NetcodePlus.uplugin").is_some());
        // Every escape shape must be independently rejected by THIS function,
        // not relying on is_safe_entry having run first.
        for bad in [
            "../escape",
            "a/../../b",
            "/abs",
            "\\abs",
            "C:\\Windows",
            "C:/Windows",
            "",
        ] {
            assert!(
                joined_is_within(base, bad).is_none(),
                "should reject {bad:?}"
            );
        }
    }
}
