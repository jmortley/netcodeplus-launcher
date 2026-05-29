//! Launching the UT4 game with optional process priority and CPU
//! affinity.
//!
//! We shell out to `cmd /C start "" [/high] [/affinity <hexmask>] <exe>
//! <args…>` — the exact mechanism a hand-written `.bat` uses — rather
//! than calling `SetProcessAffinityMask` / `SetPriorityClass` directly,
//! because those require `unsafe` FFI and this crate (and the workspace)
//! keep `unsafe_code = "deny"`. The trade-off: the launcher does not get
//! a handle to the game process (fire-and-forget), which is fine for a
//! launcher.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Windows process priority class to launch with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// Normal priority — `start` with no priority flag.
    #[default]
    Normal,
    /// High priority — `start /high` (what a competitive player's bat
    /// typically uses).
    High,
}

/// How to launch the game.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LaunchOptions {
    /// Process priority class.
    #[serde(default)]
    pub priority: Priority,
    /// CPU affinity mask (set bits = allowed logical processors).
    /// `None` = no affinity restriction (all cores).
    #[serde(default)]
    pub affinity_mask: Option<u64>,
}

/// Number of logical processors, for computing affinity presets.
#[must_use]
pub fn logical_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// Mask with one bit set per logical CPU.
#[must_use]
pub fn all_cpus_mask(cpus: usize) -> u64 {
    if cpus >= 64 {
        u64::MAX
    } else {
        (1u64 << cpus) - 1
    }
}

/// A named affinity choice for the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AffinityPreset {
    /// Display label.
    pub label: String,
    /// Uppercase hex mask without a `0x` prefix. An **empty string**
    /// means "no affinity" (all cores) — what `start` does with no
    /// `/affinity` flag.
    pub mask_hex: String,
}

/// Affinity presets computed for this machine's core count: all cores,
/// exclude CPU 0, exclude CPU 0 & 1. The UI shows these plus a custom
/// hex field.
#[must_use]
pub fn affinity_presets() -> Vec<AffinityPreset> {
    let n = logical_cpus();
    let all = all_cpus_mask(n);
    let mut presets = vec![AffinityPreset {
        label: format!("All cores ({n})"),
        mask_hex: String::new(),
    }];
    if n > 1 {
        presets.push(AffinityPreset {
            label: "Exclude CPU 0".to_string(),
            mask_hex: format!("{:X}", all & !0b1),
        });
    }
    if n > 2 {
        presets.push(AffinityPreset {
            label: "Exclude CPU 0 & 1".to_string(),
            mask_hex: format!("{:X}", all & !0b11),
        });
    }
    presets
}

/// Parse a hex affinity mask string. Empty / whitespace → `None` (all
/// cores). Accepts an optional `0x` prefix.
///
/// # Errors
/// Returns the underlying [`std::num::ParseIntError`] if the non-empty
/// input is not valid hex.
pub fn parse_mask_hex(s: &str) -> Result<Option<u64>, std::num::ParseIntError> {
    let t = s.trim();
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    if t.is_empty() {
        return Ok(None);
    }
    Ok(Some(u64::from_str_radix(t, 16)?))
}

/// Build the argument vector passed to `cmd /C` (i.e. everything after
/// `/C`): `start "" [/high] [/affinity MASK] <exe> <args…>`. Exposed for
/// testing; [`launch`] calls it.
#[must_use]
pub fn start_args(exe: &Path, args: &[String], opts: &LaunchOptions) -> Vec<String> {
    // "start" then an empty window-title slot (so a quoted exe path is
    // treated as the command, not the title).
    let mut v: Vec<String> = vec!["start".to_string(), String::new()];
    if matches!(opts.priority, Priority::High) {
        v.push("/high".to_string());
    }
    if let Some(mask) = opts.affinity_mask {
        v.push("/affinity".to_string());
        v.push(format!("{mask:X}"));
    }
    v.push(exe.display().to_string());
    v.extend(args.iter().cloned());
    v
}

/// Launch the game. The working directory is set to the executable's
/// folder (UE4 resolves some relative paths against cwd, and the bat
/// `cd`s there first).
///
/// # Errors
/// Returns the spawn error if the launch process cannot start.
#[cfg(windows)]
pub fn launch(exe: &Path, args: &[String], opts: &LaunchOptions) -> std::io::Result<()> {
    use std::process::Command;
    let cwd = exe.parent().unwrap_or(exe);
    Command::new("cmd")
        .arg("/C")
        .args(start_args(exe, args, opts))
        .current_dir(cwd)
        .spawn()?;
    Ok(())
}

/// Non-Windows fallback: spawn directly (priority/affinity flags are
/// Windows-specific and ignored). Linux/Proton support is post-v1.
///
/// # Errors
/// Returns the spawn error if the game cannot start.
#[cfg(not(windows))]
pub fn launch(exe: &Path, args: &[String], _opts: &LaunchOptions) -> std::io::Result<()> {
    use std::process::Command;
    let cwd = exe.parent().unwrap_or(exe);
    Command::new(exe).args(args).current_dir(cwd).spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn exclude_first_two_cores_on_12_matches_ffc() {
        // The user's bat uses /affinity FFC on a 12-thread CPU.
        assert_eq!(format!("{:X}", all_cpus_mask(12) & !0b11), "FFC");
    }

    #[test]
    fn all_cpus_mask_handles_small_and_large() {
        assert_eq!(all_cpus_mask(1), 0b1);
        assert_eq!(all_cpus_mask(4), 0b1111);
        assert_eq!(all_cpus_mask(64), u64::MAX);
    }

    #[test]
    fn parse_mask_hex_variants() {
        assert_eq!(parse_mask_hex("FFC").unwrap(), Some(0xFFC));
        assert_eq!(parse_mask_hex("0xffc").unwrap(), Some(0xFFC));
        assert_eq!(parse_mask_hex("   ").unwrap(), None);
        assert_eq!(parse_mask_hex("").unwrap(), None);
        assert!(parse_mask_hex("nothex").is_err());
    }

    #[test]
    fn start_args_high_with_affinity() {
        let exe = PathBuf::from(
            r"C:\Program Files\UnrealTournament\Engine\Binaries\Win64\UE4-Win64-Shipping.exe",
        );
        let args = vec!["UnrealTournament".to_string(), "-EpicPortal".to_string()];
        let opts = LaunchOptions {
            priority: Priority::High,
            affinity_mask: Some(0xFFC),
        };
        let v = start_args(&exe, &args, &opts);
        assert_eq!(v[0], "start");
        assert_eq!(v[1], ""); // empty title slot
        assert!(v.contains(&"/high".to_string()));
        let i = v.iter().position(|s| s == "/affinity").unwrap();
        assert_eq!(v[i + 1], "FFC");
        assert!(v.iter().any(|s| s.ends_with("UE4-Win64-Shipping.exe")));
        assert_eq!(v.last().unwrap(), "-EpicPortal");
    }

    #[test]
    fn start_args_normal_no_affinity_is_minimal() {
        let exe = PathBuf::from("UE4-Win64-Shipping.exe");
        let opts = LaunchOptions::default();
        let v = start_args(&exe, &["UnrealTournament".to_string()], &opts);
        assert!(!v.iter().any(|s| s == "/high"));
        assert!(!v.iter().any(|s| s == "/affinity"));
        // start, "", exe, arg
        assert_eq!(v.len(), 4);
    }
}
