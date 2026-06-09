//! Launching the UT4 game with optional process priority and CPU
//! affinity.
//!
//! The game executable is spawned **directly** via
//! [`std::process::Command`] — never through `cmd /C start` — so the exe
//! path and launch args reach `CreateProcess` as a structured argument
//! vector that no shell ever re-parses. That closes the command-injection
//! surface a `cmd` line would open (shell metacharacters in an arg can't
//! break out).
//!
//! Priority is applied safely at creation time via the
//! `HIGH_PRIORITY_CLASS` creation flag. CPU affinity is the one knob with
//! no safe-Rust path: pinning a *child* process needs a single
//! `SetProcessAffinityMask` FFI call, which we make behind a documented
//! `#[allow(unsafe_code)]` — the workspace default stays
//! `unsafe_code = "deny"`. The launch is otherwise fire-and-forget: we use
//! the child handle only to set affinity, then drop it (the game keeps
//! running after the launcher exits).

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

/// Launch the game. The working directory is set to the executable's
/// folder (UE4 resolves some relative paths against cwd, and the bat
/// `cd`s there first). Priority is applied at creation; affinity, when
/// requested, is pinned on the spawned child.
///
/// # Errors
/// Returns the spawn error if the game process cannot start, or the OS
/// error if pinning CPU affinity fails.
#[cfg(windows)]
pub fn launch(exe: &Path, args: &[String], opts: &LaunchOptions) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    // HIGH_PRIORITY_CLASS (winbase.h) — passed as a creation flag so the
    // game starts at high priority: the safe equivalent of `start /high`.
    const HIGH_PRIORITY_CLASS: u32 = 0x0000_0080;

    let cwd = exe.parent().unwrap_or(exe);
    let mut command = Command::new(exe);
    command.args(args).current_dir(cwd);
    if matches!(opts.priority, Priority::High) {
        command.creation_flags(HIGH_PRIORITY_CLASS);
    }
    let child = command.spawn()?;
    if let Some(mask) = opts.affinity_mask {
        set_process_affinity(&child, mask)?;
    }
    Ok(())
}

/// Pin a freshly spawned child process to the given CPU affinity mask.
///
/// There is no safe-Rust API to set a *child* process's affinity, and
/// routing through a shell (`cmd /C start /affinity`) would reintroduce a
/// command-injection surface, so we make the one `SetProcessAffinityMask`
/// FFI call directly. This is the single audited exception to the
/// workspace-wide `unsafe_code = "deny"`.
#[cfg(windows)]
#[allow(unsafe_code)]
fn set_process_affinity(child: &std::process::Child, mask: u64) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Threading::SetProcessAffinityMask;

    // SAFETY: `child` is borrowed for the duration of the call, so its
    // process handle is live and valid. `SetProcessAffinityMask` only reads
    // the handle and the integer mask — it dereferences nothing on our side,
    // so there is no aliasing, lifetime, or initialisation hazard.
    let ok = unsafe { SetProcessAffinityMask(child.as_raw_handle() as _, mask as usize) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Non-Windows launch. On Linux, UT4 is the **Windows** build under Wine/Lutris
/// (see [`crate::linux`]), so when the exe lives inside a Wine prefix we run it
/// **through Wine** against that prefix; otherwise we fall back to a direct spawn
/// (a genuine native build, or a test). The Windows-only priority/affinity knobs
/// are ignored.
///
/// # Errors
/// Returns the spawn error if the game (or Wine) cannot start.
#[cfg(not(windows))]
pub fn launch(exe: &Path, args: &[String], _opts: &LaunchOptions) -> std::io::Result<()> {
    use std::process::Command;

    if let Some(plan) = crate::linux::plan_wine_launch(exe, args, wine_binary().as_deref()) {
        Command::new(&plan.program)
            .args(&plan.args)
            .env("WINEPREFIX", &plan.wineprefix)
            .current_dir(&plan.cwd)
            .spawn()?;
    } else {
        let cwd = exe.parent().unwrap_or(exe);
        Command::new(exe).args(args).current_dir(cwd).spawn()?;
    }
    Ok(())
}

/// The wine binary to launch through, honouring a `WINE` environment override so
/// a user (or a Lutris wrapper) can point at a specific build; defaults to `wine`
/// on `PATH`.
#[cfg(not(windows))]
fn wine_binary() -> Option<String> {
    std::env::var("WINE").ok().filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
