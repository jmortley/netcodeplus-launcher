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

use std::path::{Path, PathBuf};

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
    /// Real-time priority (`REALTIME_PRIORITY_CLASS`). NOT recommended: it can
    /// starve the OS (input, audio, networking), and it only takes full effect
    /// when the launcher runs **elevated** — Windows silently caps a non-elevated
    /// process to `High` (no error). Serialises as `"real_time"`.
    RealTime,
}

/// Map a UI/IPC priority label to a [`Priority`]. Case-insensitive; accepts the
/// canonical serde strings (`"normal"`, `"high"`, `"real_time"`) plus the lenient
/// alias `"realtime"`. Anything unrecognised maps to [`Priority::Normal`] — the
/// safe default. The stringly-typed launch IPC boundary calls this.
#[must_use]
pub fn priority_from_label(s: &str) -> Priority {
    if s.eq_ignore_ascii_case("high") {
        Priority::High
    } else if s.eq_ignore_ascii_case("real_time") || s.eq_ignore_ascii_case("realtime") {
        Priority::RealTime
    } else {
        Priority::Normal
    }
}

/// Linux-only explicit Wine/Proton launch override for setups where Lutris
/// auto-detection can't supply the prefix/runner (a manually-picked install, an
/// exotic prefix, etc.). Configured in the "Wine / Proton" settings panel and
/// persisted in [`crate::state::LauncherState`]. When `prefix` is set it takes
/// precedence over Lutris config at launch; `None`/empty means "auto-detect".
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LinuxLaunchSettings {
    /// The `WINEPREFIX` — the directory that contains `drive_c`. Required for the
    /// override to apply.
    #[serde(default)]
    pub prefix: Option<PathBuf>,
    /// Path to the wine binary to run through (a runner's `files/bin/wine`, or a
    /// system wine). `None` = use `wine` on `PATH`.
    #[serde(default)]
    pub wine: Option<PathBuf>,
}

#[cfg(not(windows))]
impl LinuxLaunchSettings {
    /// Build a launch plan from these settings, or `None` when no prefix is set
    /// (so the caller falls through to auto-detection).
    fn plan(&self, exe: &Path, args: &[String]) -> Option<crate::linux::WineLaunch> {
        let prefix = self.prefix.as_deref()?;
        Some(crate::linux::plan_manual_launch(
            exe,
            args,
            prefix,
            self.wine.as_deref(),
        ))
    }
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
pub fn launch(
    exe: &Path,
    args: &[String],
    opts: &LaunchOptions,
    _linux: Option<&LinuxLaunchSettings>,
) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    // HIGH_PRIORITY_CLASS / REALTIME_PRIORITY_CLASS (winbase.h) — passed as a
    // creation flag so the game starts at the chosen priority: the safe
    // equivalent of `start /high` (or `/realtime`). REALTIME requires
    // SeIncreaseBasePriorityPrivilege; a non-elevated launcher is silently
    // downgraded to HIGH by Windows (no error), so "real-time" only truly
    // applies when the launcher itself runs as administrator.
    const HIGH_PRIORITY_CLASS: u32 = 0x0000_0080;
    const REALTIME_PRIORITY_CLASS: u32 = 0x0000_0100;

    let cwd = exe.parent().unwrap_or(exe);
    let mut command = Command::new(exe);
    command.args(args).current_dir(cwd);
    match opts.priority {
        Priority::High => {
            command.creation_flags(HIGH_PRIORITY_CLASS);
        }
        Priority::RealTime => {
            command.creation_flags(REALTIME_PRIORITY_CLASS);
        }
        Priority::Normal => {}
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
pub fn launch(
    exe: &Path,
    args: &[String],
    _opts: &LaunchOptions,
    linux: Option<&LinuxLaunchSettings>,
) -> std::io::Result<()> {
    use std::process::Command;

    if let Some(plan) = resolve_wine_launch(exe, args, linux) {
        spawn_wine(&plan)?;
    } else {
        // No prefix anywhere (not a Wine setup, or a genuine native build/test).
        let cwd = exe.parent().unwrap_or(exe);
        Command::new(exe).args(args).current_dir(cwd).spawn()?;
    }
    Ok(())
}

/// Resolve how the game should be launched on Linux, in precedence order:
/// 1. an explicit user [`LinuxLaunchSettings`] override (prefix + runner);
/// 2. the game's Lutris config (explicit WINEPREFIX + configured runner + env +
///    `prefix_command` — the game commonly lives OUTSIDE the prefix, so it can't
///    be derived from the exe);
/// 3. an exe that lives inside a `drive_c`, run through system/`WINE` wine.
///
/// `None` means "not a Wine launch" — the caller should spawn the exe directly.
/// Shared by [`launch`] and the settings UI's resolved-command preview.
#[cfg(not(windows))]
#[must_use]
pub fn resolve_wine_launch(
    exe: &Path,
    args: &[String],
    linux: Option<&LinuxLaunchSettings>,
) -> Option<crate::linux::WineLaunch> {
    if let Some(plan) = linux.and_then(|s| s.plan(exe, args)) {
        return Some(plan);
    }
    if let Some(plan) = crate::linux::find_lutris_launch(exe, args) {
        return Some(plan);
    }
    crate::linux::plan_wine_launch(exe, args, wine_binary().as_deref())
}

/// Spawn a resolved [`crate::linux::WineLaunch`], applying its `WINEPREFIX`, extra
/// env vars (as literal `key=value` — never shell-expanded), working dir, and an
/// optional command wrapper (`prefix_command`, e.g. `taskset …`). Fire-and-forget:
/// the child is detached so the game outlives the launcher.
#[cfg(not(windows))]
fn spawn_wine(plan: &crate::linux::WineLaunch) -> std::io::Result<()> {
    use std::process::Command;

    let mut command = if plan.wrapper.is_empty() {
        Command::new(&plan.program)
    } else {
        // `wrapper[0] wrapper[1..] <wine> <wine-args>`
        let mut c = Command::new(&plan.wrapper[0]);
        c.args(&plan.wrapper[1..]);
        c.arg(&plan.program);
        c
    };
    command
        .args(&plan.args)
        .env("WINEPREFIX", &plan.wineprefix)
        .envs(plan.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&plan.cwd);
    command.spawn()?;
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

    #[test]
    fn priority_serialises_snake_case() {
        // The frontend <select> value + the persisted state must agree with these
        // exact strings (the launch IPC boundary round-trips through them).
        assert_eq!(
            serde_json::to_string(&Priority::Normal).unwrap(),
            "\"normal\""
        );
        assert_eq!(serde_json::to_string(&Priority::High).unwrap(), "\"high\"");
        assert_eq!(
            serde_json::to_string(&Priority::RealTime).unwrap(),
            "\"real_time\""
        );
        assert_eq!(
            serde_json::from_str::<Priority>("\"real_time\"").unwrap(),
            Priority::RealTime
        );
    }

    #[test]
    fn priority_from_label_maps_all() {
        assert_eq!(priority_from_label("high"), Priority::High);
        assert_eq!(priority_from_label("HIGH"), Priority::High);
        assert_eq!(priority_from_label("real_time"), Priority::RealTime);
        assert_eq!(priority_from_label("realtime"), Priority::RealTime);
        assert_eq!(priority_from_label("RealTime"), Priority::RealTime);
        assert_eq!(priority_from_label("normal"), Priority::Normal);
        assert_eq!(priority_from_label(""), Priority::Normal);
        assert_eq!(priority_from_label("garbage"), Priority::Normal);
    }
}
