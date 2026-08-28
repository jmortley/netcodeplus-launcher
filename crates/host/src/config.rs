//! Competitive `Engine.ini` configuration.
//!
//! Applies a curated performance baseline (the `[SystemSettings]`,
//! `[ConsoleVariables]` and `[/script/engine.renderersettings]` sections)
//! plus a few editable engine knobs (frame-rate cap, smooth frame rate,
//! display gamma) and, when the OpenAL module is installed, the `[Audio]`
//! device override — by **merging** them into the player's existing
//! `Engine.ini`. It also verifies and can repair the
//! `[OnlineSubsystemMcp.*]` master-server sections that a known UT4 bug
//! sometimes wipes.
//!
//! Every other section — online/master-server, login token, replay paths,
//! game-mode history — is preserved verbatim, and the original is backed up
//! once to `Engine.ini.ncpbak` so [`restore`] is a one-click undo. We never
//! overwrite the file wholesale: doing so would strip a player's
//! connectivity and log them out.
//!
//! The same machinery powers the shipped competitive **`Mod.ini` presets**
//! ([`mod_presets`]/[`apply_mod_preset`]): curated NetcodePlus section sets
//! captured from top players' configs, applied as a section merge into
//! `Saved/Config/Mod.ini` with the identical once-only `.ncpbak` backup.
//! Identity sections (`[Identifiers]`, `[OldIdentifiers]`) are stripped from
//! the presets at authoring time and, being unmanaged, survive every apply.

use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;

/// `[ConsoleVariables]` baseline — low quality that still reads clearly.
const CONSOLE_VARIABLES: &str = "\
sg.ViewDistanceQuality=0
sg.AntiAliasingQuality=2
sg.ShadowQuality=0
sg.PostProcessQuality=0
sg.TextureQuality=0
sg.EffectsQuality=0
r.SimpleForwardShading=1
r.DistanceFieldAO=0
r.DistanceFieldShadowing=0
r.Shadow.MaxResolution=512
r.ShadowQuality=0
r.LightFunctionQuality=0
Foliage.MinimumScreenSize=1.01
r.MaterialQualityLevel=0
r.TranslucencyVolumeBlur=0
r.ReflectionEnvironment=0
r.RefractionQuality=0
r.SeparateTranslucency=0
r.LightShaftQuality=0
r.Tonemapper.Quality=0
r.FastBlurThreshold=0
r.BloomDirt=0
r.BloomQuality=0
r.SceneColorFringeQuality=0
r.LensFlareQuality=0
r.DepthOfFieldQuality=0
r.FinishCurrentFrame=0";

/// `[/script/engine.renderersettings]` baseline.
const RENDERER_SETTINGS: &str = "\
r.AmbientOcclusionMipLevelFactor=0
r.AmbientOcclusionMaxQuality=0
r.AmbientOcclusionLevels=0
r.AmbientOcclusionRadiusScale=0.0
r.RenderTargetPoolMin=2.5
r.EyeAdaptationQuality=0
r.Tonemapper.GrainQuantization=0
r.TranslucencyLightingVolumeDim=1
r.Fog=0
r.HBAO=0
r.SSR.Quality=0
r.SSS.Quality=0
r.SceneColorFormat=3
r.DetailMode=1
r.MotionBlurQuality=0
r.PostProcessAAQuality=0
r.EmitterSpawnRateScale=0
r.ParticleLightQuality=0
r.Streaming.PoolSize=4000
r.Streaming.MaxTempMemoryAllowed=64
r.Streaming.UseAllMips=1
r.Streaming.DefragDynamicBounds=1
r.Streaming.MipBias=1.5
r.MaxAnisotropy=0
r.Shadow.PreShadowResolutionFactor=0
r.Shadow.CSM.MaxCascades=1
r.Shadow.RadiusThreshold=0
r.Shadow.DistanceScale=0
r.Shadow.CSM.TransitionScale=0
r.CapsuleShadows=0
r.SkeletalMeshLODBias=0
r.ViewDistanceScale=1
r.DoInitViewsLightingAfterPrepass=1
p.AnimDynamicsNumDebtFrames=1
r.MorphTarget.Mode=1
r.CreateShadersOnLoad=1
r.VirtualTextureReducedMemory=1
r.HZBOcclusion=0
r.SSR.MaxRoughness=0
r.RHICmdBypass=0
r.PostProcessingColorFormat=0";

/// `[SystemSettings]` baseline, minus `net.AllowAsyncLoading` — that line is
/// written separately because it's a per-player toggle (see
/// [`EngineTweaks::allow_async_loading`]): `=0` loads into maps faster but can
/// misbehave in Blitz / flag-run.
const SYSTEM_SETTINGS_REST: &str = "\
r.OneFrameThreadLag=1
fx.GPUSimulationTextureSizeX=16
fx.GPUSimulationTextureSizeY=16
r.ParticleLightQuality=0";

// Section names (inner text, no brackets) and the canonical header to
// create if a section is absent.
const SEC_CONSOLE: &str = "ConsoleVariables";
const HDR_CONSOLE: &str = "[ConsoleVariables]";
const SEC_RENDERER: &str = "/script/engine.renderersettings";
const HDR_RENDERER: &str = "[/script/engine.renderersettings]";
const SEC_ENGINE: &str = "/Script/UnrealTournament.UTGameEngine";
const HDR_ENGINE: &str = "[/Script/UnrealTournament.UTGameEngine]";
const SEC_AUDIO: &str = "Audio";
const HDR_AUDIO: &str = "[Audio]";
const SEC_SYSTEM: &str = "SystemSettings";
const HDR_SYSTEM: &str = "[SystemSettings]";
const SEC_NCP: &str = "NetcodePlus";
const HDR_NCP: &str = "[NetcodePlus]";

/// The community master-server host every `[OnlineSubsystemMcp.*]` section
/// must point `Domain` at for online play to work.
const MASTER_DOMAIN: &str = "master-ut4.timiimit.com";

/// The seven MCP section inner-names that carry the master-server config. A
/// known UT4 bug occasionally wipes these, dropping the player off the
/// community server list.
const MCP_SECTIONS: [&str; 7] = [
    "OnlineSubsystemMcp.BaseServiceMcp",
    "OnlineSubsystemMcp.GameServiceMcp",
    "OnlineSubsystemMcp.AccountServiceMcp",
    "OnlineSubsystemMcp.OnlineFriendsMcp",
    "OnlineSubsystemMcp.PersonaServiceMcp",
    "OnlineSubsystemMcp.OnlineImageServiceMcp",
    "OnlineSubsystemMcp.OnlineContentControlsServiceMcp UnrealTournamentDev",
];

const ENGINE_INI_REL: &str = "UnrealTournament/Saved/Config/WindowsNoEditor/Engine.ini";
const OPENAL_DLL_REL: &str = "Engine/Binaries/Win64/UE4-ALAudio-Win64-Shipping.dll";
const BACKUP_SUFFIX: &str = ".ncpbak";

/// Editable engine knobs surfaced in the launcher UI.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EngineTweaks {
    /// `FrameRateCap` in `[/Script/UnrealTournament.UTGameEngine]`.
    pub frame_rate_cap: f64,
    /// `bSmoothFrameRate`.
    pub smooth_frame_rate: bool,
    /// `DisplayGamma` (higher = brighter; 3.0 is the competitive baseline).
    pub display_gamma: f64,
    /// `net.AllowAsyncLoading` in `[SystemSettings]`. The competitive baseline
    /// is off (`=0`, faster map loads) but it can break Blitz / flag-run, so
    /// it's a per-player toggle. `true` writes `=1`, `false` writes `=0`.
    pub allow_async_loading: bool,
    /// `MaxChannels` in `[Audio]` — how many sounds UT4 plays at once. The
    /// engine default is 32 with no virtualization: once the pool is full,
    /// quiet one-shots (a jump pad behind you, a distant rocket load) are
    /// dropped outright. Busy NetcodePlus fights can exceed 32, so this is a
    /// per-player knob; values are clamped to
    /// [`MAX_AUDIO_CHANNELS_MIN`]..=[`MAX_AUDIO_CHANNELS_MAX`] on apply.
    /// Applies to both the stock XAudio2 device and the UT4-OpenAL module.
    pub max_audio_channels: u32,
    /// `UnfocusedVolumeMultiplier` in `[Audio]` — how loud the game stays when
    /// minimized / alt-tabbed. The engine default is 0.0 (full mute on focus
    /// loss); 1.0 keeps match audio at full volume so a tabbed-out player
    /// still hears the pug start. Latched once at game boot — a change needs a
    /// UT4 restart. Clamped to 0.0..=1.0 on apply; honored by both stock
    /// XAudio2 and the UT4-OpenAL module (both consume the app volume
    /// multiplier the focus handler sets).
    pub unfocused_volume: f64,

    /// `ncp.HighPollingMouseCoalesce` in `[ConsoleVariables]` — **experimental**,
    /// NetcodePlus-only, default off.
    ///
    /// UE4.15 routes every mouse packet through Slate's hit-testing and widget
    /// routing before the viewport sums them into one MouseX/MouseY for the
    /// frame. At 4000-8000 Hz that per-packet work is pure overhead while the
    /// game has the cursor captured and hidden. With this on, NetcodePlus sums
    /// the deltas itself and submits them once per frame, skipping the routing.
    ///
    /// It does **not** reduce input latency — the batch is submitted in the same
    /// frame, microseconds earlier — and it does not alter the deltas or the
    /// sample count. **At 1000 Hz the saving is a wash**; it is measurable only
    /// at 4-8 kHz and only while the mouse is moving. It stays out of the way of
    /// menus, the editor, mouse smoothing, unfocused windows, visible-cursor
    /// modes and absolute pointers (tablet / RDP / VM), all of which keep stock
    /// Slate behaviour.
    ///
    /// Written as a cvar rather than a plain ini key: a `[ConsoleVariables]`
    /// entry for a not-yet-registered variable is held by the engine and applied
    /// when the plugin registers it (`ConfigCacheIni.cpp` `OnSetCVarFromIniEntry`
    /// creates an `ECVF_Unregistered` placeholder for exactly this case), so it
    /// reaches a plugin cvar that comes up after startup. Older NetcodePlus
    /// builds without the cvar simply ignore it.
    pub high_polling_mouse: bool,
}

/// Lower clamp for [`EngineTweaks::max_audio_channels`] — below the engine
/// default would only make sounds drop more.
pub const MAX_AUDIO_CHANNELS_MIN: u32 = 32;
/// Upper clamp for [`EngineTweaks::max_audio_channels`] — the engine's own
/// hard cap (`MAX_AUDIOCHANNELS` = 64); the audio device ignores anything
/// above it, so writing more would just be a lie in the ini.
pub const MAX_AUDIO_CHANNELS_MAX: u32 = 64;

impl Default for EngineTweaks {
    fn default() -> Self {
        Self {
            frame_rate_cap: 360.0,
            smooth_frame_rate: false,
            display_gamma: 3.0,
            allow_async_loading: false,
            max_audio_channels: 32,
            unfocused_volume: 0.0,
            high_polling_mouse: false,
        }
    }
}

/// What the UI needs to render the config card: whether there is an ini to
/// act on, whether a restore point exists, and the current editable values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigState {
    /// Whether `Engine.ini` exists yet (the game has been run once).
    pub ini_exists: bool,
    /// Whether a `.ncpbak` restore point exists.
    pub has_backup: bool,
    /// Whether the `[OnlineSubsystemMcp.*]` master-server sections are intact
    /// (false = wiped by the known bug; the online server browser is broken).
    pub master_server_ok: bool,
    /// Current editable values read from the ini (defaults if absent).
    pub tweaks: EngineTweaks,
    /// Whether `Engine.ini` is marked read-only. Apply writes the file, so a
    /// read-only ini makes it fail — the UI warns up front. Some players set it
    /// read-only on purpose to lock their config, so it's surfaced with an
    /// opt-in "make writable" rather than auto-cleared.
    pub engine_ini_read_only: bool,
}

/// Failure modes for the config operations.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The OS Documents directory could not be located.
    #[error("could not locate the Documents folder")]
    NoDocumentsDir,
    /// `Engine.ini` does not exist — the game has not been run yet.
    #[error("Engine.ini not found at {0} — launch UT4 once, then apply")]
    IniNotFound(PathBuf),
    /// Restore was requested but no backup exists.
    #[error("no backup to restore at {0}")]
    NoBackup(PathBuf),
    /// An unknown `Mod.ini` preset id was requested.
    #[error("unknown Mod.ini preset: {0}")]
    UnknownPreset(String),
    /// Underlying filesystem error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// An atomic config write failed even after retries — usually antivirus
    /// (e.g. Windows Defender) blocking the launcher from writing in the config
    /// folder, a read-only file, or OneDrive mid-sync racing the temp file.
    #[error(
        "couldn't save {path} after several attempts — antivirus (e.g. Windows \
         Defender) may be blocking the launcher from writing there, the file may \
         be read-only, or OneDrive may be mid-sync. Add an exclusion for the \
         launcher (or that folder), then try again. ({source})"
    )]
    AtomicWrite {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Result alias for the config operations.
pub type ConfigResult<T> = std::result::Result<T, ConfigError>;

/// Per-user `Engine.ini` path
/// (`…/Documents/UnrealTournament/Saved/Config/WindowsNoEditor/Engine.ini`).
#[must_use]
pub fn engine_ini_path() -> Option<PathBuf> {
    dirs::document_dir().map(|d| d.join(ENGINE_INI_REL))
}

/// True if UT4-OpenAL is installed under `root` (its shipping ALAudio
/// module DLL sits next to the game exe).
#[must_use]
pub fn openal_installed(root: &Path) -> bool {
    root.join(OPENAL_DLL_REL).is_file()
}

/// The directory UT4-OpenAL's shipping DLL belongs in
/// (`<root>/Engine/Binaries/Win64/`) — next to the engine binaries, NOT under
/// `Plugins`. Where a player drops the extracted `UE4-ALAudio-…` DLL.
#[must_use]
pub fn openal_dir(root: &Path) -> PathBuf {
    root.join("Engine").join("Binaries").join("Win64")
}

fn backup_path(ini: &Path) -> PathBuf {
    let mut s = ini.as_os_str().to_owned();
    s.push(BACKUP_SUFFIX);
    PathBuf::from(s)
}

/// Read the current config state for the UI.
#[must_use]
pub fn read_state(ini: &Path) -> ConfigState {
    let has_backup = backup_path(ini).is_file();
    match std::fs::read_to_string(ini) {
        Ok(text) => ConfigState {
            ini_exists: true,
            has_backup,
            master_server_ok: master_server_intact(&text),
            tweaks: read_tweaks(&text),
            engine_ini_read_only: is_read_only(ini),
        },
        // No readable ini yet — the UI shows "launch UT4 once"; master-server
        // state is irrelevant, so report it as fine to avoid a false alarm.
        Err(_) => ConfigState {
            ini_exists: false,
            has_backup,
            master_server_ok: true,
            tweaks: EngineTweaks::default(),
            engine_ini_read_only: false,
        },
    }
}

/// Whether `path` exists and is marked read-only (best-effort; false if its
/// metadata can't be read).
fn is_read_only(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.permissions().readonly())
        .unwrap_or(false)
}

/// Clear the read-only attribute on `ini` so [`apply`] can write it. A no-op if
/// it's already writable. Opt-in (the UI offers it) because some players mark
/// the file read-only deliberately.
///
/// # Errors
/// [`ConfigError::Io`] if metadata can't be read or permissions can't be set.
// Clearing the read-only attribute is the whole point, on Windows (the platform
// the launcher targets), where `set_readonly(false)` simply removes that flag.
// The clippy lint warns this would make a file world-writable on Unix, which
// doesn't apply to the Windows UT4 config path.
#[allow(clippy::permissions_set_readonly_false)]
pub fn clear_read_only(ini: &Path) -> ConfigResult<()> {
    let mut perms = std::fs::metadata(ini)?.permissions();
    if perms.readonly() {
        perms.set_readonly(false);
        std::fs::set_permissions(ini, perms)?;
    }
    Ok(())
}

/// Apply the competitive baseline plus the editable knobs to `ini`,
/// merging into the existing file. The pristine original is backed up once
/// before the first write. `set_openal_audio` controls whether the
/// `[Audio]` OpenAL override is written (caller passes the result of
/// [`openal_installed`]).
///
/// # Errors
/// [`ConfigError::IniNotFound`] if the ini does not exist, or
/// [`ConfigError::Io`] on a filesystem error.
pub fn apply(ini: &Path, tweaks: &EngineTweaks, set_openal_audio: bool) -> ConfigResult<()> {
    let text = read_ini_with_backup(ini)?;

    let mut ini_file = IniFile::parse(&text);
    ini_file.replace_body(SEC_CONSOLE, HDR_CONSOLE, CONSOLE_VARIABLES);
    ini_file.replace_body(SEC_RENDERER, HDR_RENDERER, RENDERER_SETTINGS);
    let system_body = format!(
        "net.AllowAsyncLoading={}\n{SYSTEM_SETTINGS_REST}",
        u8::from(tweaks.allow_async_loading),
    );
    ini_file.replace_body(SEC_SYSTEM, HDR_SYSTEM, &system_body);
    set_tweak_keys(&mut ini_file, tweaks);
    if set_openal_audio {
        ini_file.set_key(SEC_AUDIO, HDR_AUDIO, "AudioDeviceModuleName", "ALAudio");
    }

    write_atomic(ini, &ini_file.render())
}

/// Save ONLY the editable knobs into `ini`, leaving everything else — the
/// competitive-baseline sections included — exactly as the player has it.
/// Same backup-once semantics as [`apply`], so Restore covers both paths.
///
/// # Errors
/// [`ConfigError::IniNotFound`] if the ini does not exist, or
/// [`ConfigError::Io`] on a filesystem error.
pub fn save_tweaks(ini: &Path, tweaks: &EngineTweaks) -> ConfigResult<()> {
    let text = read_ini_with_backup(ini)?;

    let mut ini_file = IniFile::parse(&text);
    set_tweak_keys(&mut ini_file, tweaks);
    // apply() carries this inside its [SystemSettings] body replace; here the
    // knob is merged on its own, leaving the rest of the section alone.
    ini_file.set_key(
        SEC_SYSTEM,
        HDR_SYSTEM,
        "net.AllowAsyncLoading",
        if tweaks.allow_async_loading { "1" } else { "0" },
    );

    write_atomic(ini, &ini_file.render())
}

/// Read the ini for a mutating operation, backing up the pristine original
/// exactly once so Restore always returns to the pre-launcher config no
/// matter how many times Apply/Save run.
fn read_ini_with_backup(ini: &Path) -> ConfigResult<String> {
    let text = match std::fs::read_to_string(ini) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Err(ConfigError::IniNotFound(ini.to_path_buf()))
        }
        Err(e) => return Err(e.into()),
    };
    let backup = backup_path(ini);
    if !backup.exists() {
        write_atomic(&backup, &text)?;
    }
    Ok(text)
}

/// The editable knobs shared by [`apply`] and [`save_tweaks`] (async loading
/// is handled by each caller: baseline body vs standalone key).
fn set_tweak_keys(ini_file: &mut IniFile, tweaks: &EngineTweaks) {
    ini_file.set_key(
        SEC_ENGINE,
        HDR_ENGINE,
        "FrameRateCap",
        &format!("{:.6}", tweaks.frame_rate_cap),
    );
    ini_file.set_key(
        SEC_ENGINE,
        HDR_ENGINE,
        "bSmoothFrameRate",
        bool_str(tweaks.smooth_frame_rate),
    );
    ini_file.set_key(
        SEC_ENGINE,
        HDR_ENGINE,
        "DisplayGamma",
        &format!("{:.6}", tweaks.display_gamma),
    );
    // Voice-pool size applies to every audio device (XAudio2 and OpenAL), so
    // it is written regardless of the OpenAL override.
    let channels = tweaks
        .max_audio_channels
        .clamp(MAX_AUDIO_CHANNELS_MIN, MAX_AUDIO_CHANNELS_MAX);
    ini_file.set_key(SEC_AUDIO, HDR_AUDIO, "MaxChannels", &channels.to_string());
    // Background (alt-tab) volume — also device-independent, see the field doc.
    let bg = tweaks.unfocused_volume.clamp(0.0, 1.0);
    ini_file.set_key(
        SEC_AUDIO,
        HDR_AUDIO,
        "UnfocusedVolumeMultiplier",
        &format!("{bg:.6}"),
    );
    // Experimental NetcodePlus cvar. Written as an individual key AFTER apply()'s
    // wholesale [ConsoleVariables] replace above, so the competitive baseline
    // cannot silently drop the player's choice.
    ini_file.set_key(
        SEC_CONSOLE,
        HDR_CONSOLE,
        "ncp.HighPollingMouseCoalesce",
        if tweaks.high_polling_mouse { "1" } else { "0" },
    );
}

/// Restore `ini` from its `.ncpbak` backup.
///
/// # Errors
/// [`ConfigError::NoBackup`] if no backup exists, or [`ConfigError::Io`].
pub fn restore(ini: &Path) -> ConfigResult<()> {
    let backup = backup_path(ini);
    let text = match std::fs::read_to_string(&backup) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Err(ConfigError::NoBackup(backup)),
        Err(e) => return Err(e.into()),
    };
    write_atomic(ini, &text)
}

// ===================================================================
// Competitive Mod.ini presets
// ===================================================================

/// A shipped competitive `Mod.ini` preset: a curated set of NetcodePlus
/// sections captured from a top player's config, sanitized at authoring
/// time (no identity, consent, or machine-state sections).
pub struct ModPreset {
    /// Stable id the UI passes back to [`apply_mod_preset`].
    pub id: &'static str,
    /// Human label, e.g. `iCTF (Tox)`.
    pub label: &'static str,
    /// One-line description for the UI card.
    pub blurb: &'static str,
    /// The preset's ini text. Only its sections are applied; preamble
    /// comment lines are ignored by the merge.
    ini: &'static str,
}

/// The shipped presets, in display order.
static MOD_PRESETS: [ModPreset; 2] = [
    ModPreset {
        id: "ictf-tox",
        label: "iCTF (Tox)",
        blurb: "tOx-X's instagib CTF setup — team colours, hitsounds, \
                invisible IG skin, kill sounds.",
        ini: include_str!("presets/mod_ictf_tox.ini"),
    },
    ModPreset {
        id: "dueler-phantaci",
        label: "Dueler (Phantaci)",
        blurb: "phantaci's duel setup — bright forced models, Quake \
                hitsounds, minimal gibs and ragdolls.",
        ini: include_str!("presets/mod_dueler_phantaci.ini"),
    },
];

/// The shipped competitive `Mod.ini` presets, in display order.
#[must_use]
pub fn mod_presets() -> &'static [ModPreset] {
    &MOD_PRESETS
}

/// What the UI needs to render the Mod.ini presets card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModIniState {
    /// Whether `Mod.ini` exists yet. Applying a preset creates it if not
    /// (unlike `Engine.ini`, the plugin fills in the rest on next run).
    pub ini_exists: bool,
    /// Whether a `.ncpbak` restore point exists.
    pub has_backup: bool,
    /// Whether `Mod.ini` is read-only (apply would fail).
    pub read_only: bool,
}

/// Read the current `Mod.ini` state for the UI.
#[must_use]
pub fn read_mod_state(ini: &Path) -> ModIniState {
    ModIniState {
        ini_exists: ini.is_file(),
        has_backup: backup_path(ini).is_file(),
        read_only: is_read_only(ini),
    }
}

// -------------------------------------------------------------------
// NetcodePlus join waits (Mod.ini `[NetcodePlus]`)
// -------------------------------------------------------------------

/// How long the plugin holds a launcher Join while your account data
/// downloads, in seconds. Both land in `[NetcodePlus]` in `Mod.ini`, which is
/// where every other NetcodePlus knob lives.
///
/// Clicking Join launches the game with `-ncpconnect=<server>`, and the plugin
/// waits for your cloud profile — keybinds and account data — before it
/// travels. Travelling early is what drops a player into a match with default
/// binds, which the next profile save then writes back over the cloud copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinWaitTweaks {
    /// `ConnectProfileWaitSeconds` — the budget once the download has actually
    /// started. Measured on a healthy sign-in: login alone takes ~7.4s and the
    /// profile lands around 8–12s, so the 45s default is ~4–5x headroom.
    pub profile_wait_seconds: u32,
    /// `ConnectNoSignalWaitSeconds` — the budget when the download never even
    /// starts (offline, or sign-in did not get far enough to ask). Nothing is
    /// coming, so this one is deliberately shorter.
    pub no_signal_wait_seconds: u32,
}

/// Lower clamp for both waits. Below this a healthy-but-slow sign-in would be
/// cut off, which re-creates the default-binds bug the wait exists to prevent.
pub const JOIN_WAIT_MIN: u32 = 10;
/// Upper clamp for both waits — past three minutes it is indistinguishable
/// from a hang.
pub const JOIN_WAIT_MAX: u32 = 180;

impl Default for JoinWaitTweaks {
    fn default() -> Self {
        Self {
            profile_wait_seconds: 45,
            no_signal_wait_seconds: 25,
        }
    }
}

/// What the UI needs to render the join-wait card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinWaitState {
    /// Whether `Mod.ini` exists yet. Saving creates it if not — the plugin
    /// fills in everything else on the next run.
    pub ini_exists: bool,
    /// Whether `Mod.ini` is read-only (saving would fail).
    pub read_only: bool,
    /// Current values, falling back to [`JoinWaitTweaks::default`] per key for
    /// anything absent or unparseable — the same fallback the plugin applies.
    pub tweaks: JoinWaitTweaks,
}

/// Read one `[NetcodePlus]` integer key, or `None` when absent/unparseable.
/// Values are stored as UE4 floats (`45.000000`), so parse permissively and
/// round, rather than demanding an integer literal.
fn ncp_key(file: &IniFile, key: &str) -> Option<u32> {
    let sec = file.find(SEC_NCP)?;
    sec.body.iter().rev().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        if !k.trim().eq_ignore_ascii_case(key) {
            return None;
        }
        let n = v.trim().parse::<f64>().ok()?;
        if n.is_finite() && n >= 0.0 {
            Some(n.round() as u32)
        } else {
            None
        }
    })
}

/// Read the current join waits for the UI. A missing `Mod.ini` is not an
/// error — it just means the plugin has never been configured, so the
/// defaults are what is in force.
#[must_use]
pub fn read_join_wait(ini: &Path) -> JoinWaitState {
    let text = std::fs::read_to_string(ini).unwrap_or_default();
    let file = IniFile::parse(&text);
    let d = JoinWaitTweaks::default();
    JoinWaitState {
        ini_exists: ini.is_file(),
        read_only: is_read_only(ini),
        tweaks: JoinWaitTweaks {
            profile_wait_seconds: ncp_key(&file, "ConnectProfileWaitSeconds")
                .unwrap_or(d.profile_wait_seconds)
                .clamp(JOIN_WAIT_MIN, JOIN_WAIT_MAX),
            no_signal_wait_seconds: ncp_key(&file, "ConnectNoSignalWaitSeconds")
                .unwrap_or(d.no_signal_wait_seconds)
                .clamp(JOIN_WAIT_MIN, JOIN_WAIT_MAX),
        },
    }
}

/// Write the two join waits into `[NetcodePlus]`, merging them as individual
/// keys so every other setting in the section — and every other section — is
/// left verbatim. A missing `Mod.ini` starts from empty (same as
/// [`apply_mod_preset`]); the pristine original is backed up once, so
/// [`restore`] on the same path undoes this too. Values are clamped to
/// [`JOIN_WAIT_MIN`]..=[`JOIN_WAIT_MAX`], matching the plugin's own clamp.
///
/// # Errors
/// [`ConfigError::Io`]/[`ConfigError::AtomicWrite`] on filesystem errors.
pub fn save_join_wait(ini: &Path, tweaks: &JoinWaitTweaks) -> ConfigResult<()> {
    let existing = match std::fs::read_to_string(ini) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    if !existing.is_empty() {
        let backup = backup_path(ini);
        if !backup.exists() {
            write_atomic(&backup, &existing)?;
        }
    }

    let mut file = IniFile::parse(&existing);
    for (key, value) in [
        (
            "ConnectProfileWaitSeconds",
            tweaks
                .profile_wait_seconds
                .clamp(JOIN_WAIT_MIN, JOIN_WAIT_MAX),
        ),
        (
            "ConnectNoSignalWaitSeconds",
            tweaks
                .no_signal_wait_seconds
                .clamp(JOIN_WAIT_MIN, JOIN_WAIT_MAX),
        ),
    ] {
        file.set_key(SEC_NCP, HDR_NCP, key, &value.to_string());
    }
    write_atomic(ini, &file.render())
}

/// Apply a shipped preset to `ini` as a **section merge**: every section
/// the preset defines replaces the same-named section in the player's file
/// (created if absent); all other sections — `[Identifiers]`, personal
/// tweaks the preset doesn't cover — are preserved verbatim. A missing
/// `Mod.ini` (plugin never configured) starts from empty. The pristine
/// original is backed up once before the first write, so [`restore`] on
/// the same path is a one-click undo.
///
/// # Errors
/// [`ConfigError::UnknownPreset`] for an id not in [`mod_presets`], or
/// [`ConfigError::Io`]/[`ConfigError::AtomicWrite`] on filesystem errors.
pub fn apply_mod_preset(ini: &Path, preset_id: &str) -> ConfigResult<()> {
    let preset = mod_presets()
        .iter()
        .find(|p| p.id == preset_id)
        .ok_or_else(|| ConfigError::UnknownPreset(preset_id.to_string()))?;

    let existing = match std::fs::read_to_string(ini) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };

    // Back up the pristine original exactly once — only when there is one
    // (a freshly-created Mod.ini leaves nothing to restore to).
    if !existing.is_empty() {
        let backup = backup_path(ini);
        if !backup.exists() {
            write_atomic(&backup, &existing)?;
        }
    }

    let mut file = IniFile::parse(&existing);
    for sec in &IniFile::parse(preset.ini).sections {
        file.replace_body(
            section_inner(&sec.header),
            &sec.header,
            &sec.body.join("\n"),
        );
    }
    write_atomic(ini, &file.render())
}

/// True if every required `[OnlineSubsystemMcp.*]` section is present and
/// points `Domain` at the community master server.
fn master_server_intact(text: &str) -> bool {
    let file = IniFile::parse(text);
    MCP_SECTIONS.iter().all(|name| {
        file.find(name).is_some_and(|sec| {
            sec.body.iter().any(|l| {
                l.split_once('=').is_some_and(|(k, v)| {
                    k.trim().eq_ignore_ascii_case("Domain")
                        && v.trim().eq_ignore_ascii_case(MASTER_DOMAIN)
                })
            })
        })
    })
}

/// Silently ensure all `[OnlineSubsystemMcp.*]` sections exist and point at
/// the community master server, repairing the known wipe bug (which
/// otherwise breaks login *and* the server browser entirely). Returns
/// whether a change was made. Purely additive — it only writes back the
/// seven master-server sections — so it takes no backup and is safe to run
/// automatically on every launch. A missing ini (game never run) is a no-op.
///
/// # Errors
/// [`ConfigError::Io`] on a filesystem error.
pub fn repair_master_server(ini: &Path) -> ConfigResult<bool> {
    let text = match std::fs::read_to_string(ini) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    if master_server_intact(&text) {
        return Ok(false);
    }
    let mut file = IniFile::parse(&text);
    for name in MCP_SECTIONS {
        let header = format!("[{name}]");
        file.set_key(name, &header, "Domain", MASTER_DOMAIN);
        file.set_key(name, &header, "Protocol", "https");
    }
    write_atomic(ini, &file.render())?;
    Ok(true)
}

const fn bool_str(b: bool) -> &'static str {
    if b {
        "True"
    } else {
        "False"
    }
}

/// Read the editable values out of `[/Script/UnrealTournament.UTGameEngine]`,
/// `[SystemSettings]` and `[Audio]`.
///
/// `bSmoothFrameRate` ABSENT is special: the engine then runs its BaseEngine.ini
/// default — smoothing ON, clamped to the 22–62 fps `SmoothedFrameRateRange`.
/// A fresh or player-reset `Engine.ini` therefore smooths (and caps at ~62 fps)
/// even though our editable-default is `false`, so an absent key is reported as
/// `true`: the UI checkbox must tell the truth or the player sees "unchecked"
/// while the game visibly smooths (field report 2026-08-23). Saving then writes
/// an explicit `False`, which is exactly the correction the player wants.
fn read_tweaks(text: &str) -> EngineTweaks {
    let mut t = EngineTweaks::default();
    let mut saw_smooth = false;
    let mut in_engine = false;
    let mut in_system = false;
    let mut in_audio = false;
    let mut in_console = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') && l.ends_with(']') {
            let inner = section_inner(l);
            in_engine = inner.eq_ignore_ascii_case(SEC_ENGINE);
            in_system = inner.eq_ignore_ascii_case(SEC_SYSTEM);
            in_audio = inner.eq_ignore_ascii_case(SEC_AUDIO);
            in_console = inner.eq_ignore_ascii_case(SEC_CONSOLE);
            continue;
        }
        let Some((k, v)) = l.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        if in_engine {
            if k.eq_ignore_ascii_case("FrameRateCap") {
                if let Ok(n) = v.parse() {
                    t.frame_rate_cap = n;
                }
            } else if k.eq_ignore_ascii_case("bSmoothFrameRate") {
                t.smooth_frame_rate = v.eq_ignore_ascii_case("true");
                saw_smooth = true;
            } else if k.eq_ignore_ascii_case("DisplayGamma") {
                if let Ok(n) = v.parse() {
                    t.display_gamma = n;
                }
            }
        } else if in_system && k.eq_ignore_ascii_case("net.AllowAsyncLoading") {
            t.allow_async_loading = v == "1";
        } else if in_audio && k.eq_ignore_ascii_case("MaxChannels") {
            if let Ok(n) = v.parse() {
                t.max_audio_channels = n;
            }
        } else if in_audio && k.eq_ignore_ascii_case("UnfocusedVolumeMultiplier") {
            if let Ok(n) = v.parse() {
                t.unfocused_volume = n;
            }
        } else if in_console && k.eq_ignore_ascii_case("ncp.HighPollingMouseCoalesce") {
            t.high_polling_mouse = v == "1";
        }
    }
    if !saw_smooth {
        // Engine-effective value for a missing key (see doc comment above).
        t.smooth_frame_rate = true;
    }
    t
}

fn section_inner(header: &str) -> &str {
    header
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim()
}

/// Max attempts for an atomic write. Antivirus (Windows Defender) real-time
/// scanning, OneDrive sync, or a transient file lock can make the just-created
/// temp file briefly vanish or be denied (os error 2/5/32); retrying lets a
/// momentary race clear. A consistent block (a read-only file, or Defender
/// quarantining every attempt) still fails — with a clearer error.
const ATOMIC_WRITE_ATTEMPTS: u32 = 5;

fn write_atomic(path: &Path, contents: &str) -> ConfigResult<()> {
    let parent = path.parent().ok_or_else(|| {
        ConfigError::Io(std::io::Error::new(
            ErrorKind::InvalidInput,
            "ini path has no parent directory",
        ))
    })?;
    std::fs::create_dir_all(parent)?;

    let mut last = std::io::Error::other("atomic write did not run");
    for attempt in 1..=ATOMIC_WRITE_ATTEMPTS {
        match try_write_atomic(parent, path, contents) {
            Ok(()) => return Ok(()),
            Err(e) => {
                let transient = is_transient_write_error(&e);
                last = e;
                if transient && attempt < ATOMIC_WRITE_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(120 * u64::from(attempt)));
                } else {
                    break;
                }
            }
        }
    }
    Err(ConfigError::AtomicWrite {
        path: path.to_path_buf(),
        source: last,
    })
}

/// One atomic-write attempt: temp file in the target dir → write → fsync →
/// rename over the target.
fn try_write_atomic(parent: &Path, path: &Path, contents: &str) -> std::io::Result<()> {
    let mut tmp = NamedTempFile::new_in(parent)?;
    tmp.write_all(contents.as_bytes())?;
    tmp.as_file_mut().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Filesystem errors that are commonly transient on Windows when antivirus,
/// OneDrive, or another process races a just-created file: not-found,
/// access-denied, sharing-violation. Worth a retry; a persistent cause fails
/// after the attempts with a clearer [`ConfigError::AtomicWrite`].
fn is_transient_write_error(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(2) | Some(5) | Some(32))
        || matches!(e.kind(), ErrorKind::NotFound | ErrorKind::PermissionDenied)
}

/// A minimal, order-preserving UE4 ini model: a list of sections, each a
/// header line plus its raw body lines. Unmanaged sections are carried
/// through untouched (including duplicate keys and comments); only the
/// sections we manage are rewritten.
struct IniFile {
    /// Lines before the first `[section]` header (usually empty).
    preamble: Vec<String>,
    sections: Vec<Section>,
}

struct Section {
    header: String,
    body: Vec<String>,
}

impl IniFile {
    fn parse(text: &str) -> Self {
        let mut preamble = Vec::new();
        let mut sections: Vec<Section> = Vec::new();
        for line in text.lines() {
            let t = line.trim();
            if t.starts_with('[') && t.ends_with(']') {
                sections.push(Section {
                    header: t.to_string(),
                    body: Vec::new(),
                });
            } else if let Some(cur) = sections.last_mut() {
                cur.body.push(line.to_string());
            } else {
                preamble.push(line.to_string());
            }
        }
        Self { preamble, sections }
    }

    fn render(&self) -> String {
        let mut out = String::new();
        for l in &self.preamble {
            out.push_str(l);
            out.push('\n');
        }
        for s in &self.sections {
            // Enforce exactly one blank line before each section header (UE4
            // convention). Trailing blank lines are trimmed from each body
            // below and re-inserted here, so spacing stays consistent even
            // for sections the launcher created.
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push('\n');
            }
            out.push_str(&s.header);
            out.push('\n');
            let end = s
                .body
                .iter()
                .rposition(|l| !l.trim().is_empty())
                .map_or(0, |i| i + 1);
            for l in &s.body[..end] {
                out.push_str(l);
                out.push('\n');
            }
        }
        out
    }

    fn find_mut(&mut self, name: &str) -> Option<&mut Section> {
        self.sections
            .iter_mut()
            .find(|s| section_inner(&s.header).eq_ignore_ascii_case(name))
    }

    fn find(&self, name: &str) -> Option<&Section> {
        self.sections
            .iter()
            .find(|s| section_inner(&s.header).eq_ignore_ascii_case(name))
    }

    /// Replace a whole section body with `body` (newline-separated lines),
    /// creating the section with `canonical_header` if absent.
    fn replace_body(&mut self, name: &str, canonical_header: &str, body: &str) {
        let lines: Vec<String> = body.lines().map(String::from).collect();
        if let Some(sec) = self.find_mut(name) {
            sec.body = lines;
        } else {
            self.sections.push(Section {
                header: canonical_header.to_string(),
                body: lines,
            });
        }
    }

    /// Set `key=value` in a section: overwrite the first occurrence, drop
    /// any duplicates, append if missing, creating the section if absent.
    fn set_key(&mut self, name: &str, canonical_header: &str, key: &str, value: &str) {
        let line = format!("{key}={value}");
        let Some(sec) = self.find_mut(name) else {
            self.sections.push(Section {
                header: canonical_header.to_string(),
                body: vec![line],
            });
            return;
        };
        let mut seen = false;
        let mut new_body = Vec::with_capacity(sec.body.len() + 1);
        for l in sec.body.drain(..) {
            if line_key_eq(&l, key) {
                if !seen {
                    new_body.push(line.clone());
                    seen = true;
                }
                // duplicates dropped
            } else {
                new_body.push(l);
            }
        }
        if !seen {
            new_body.push(line);
        }
        sec.body = new_body;
    }
}

/// True if ini line `l` assigns `key` (case-insensitive, ignores
/// surrounding whitespace). Blank/comment lines never match.
fn line_key_eq(l: &str, key: &str) -> bool {
    l.split_once('=')
        .is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
[Core.System]
Paths=../../../Engine/Content

[/Script/UnrealTournament.UTGameEngine]
FrameRateCap=120.000000
bSmoothFrameRate=True
bFirstRun=True
DisplayGamma=1.000000

[ConsoleVariables]
sg.TextureQuality=3

[Audio]
AudioDeviceModuleName=ALAudio
AudioDeviceModuleName=ALAudio

[OnlineSubsystemMcp.BaseServiceMcp]
Domain=master-ut4.timiimit.com
Protocol=https
";

    fn apply_to(text: &str, set_audio: bool) -> String {
        let mut f = IniFile::parse(text);
        f.replace_body(SEC_CONSOLE, HDR_CONSOLE, CONSOLE_VARIABLES);
        f.replace_body(SEC_RENDERER, HDR_RENDERER, RENDERER_SETTINGS);
        // Mirror apply()'s default tweaks: async loading off.
        f.replace_body(
            SEC_SYSTEM,
            HDR_SYSTEM,
            &format!("net.AllowAsyncLoading=0\n{SYSTEM_SETTINGS_REST}"),
        );
        f.set_key(SEC_ENGINE, HDR_ENGINE, "FrameRateCap", "360.000000");
        f.set_key(SEC_ENGINE, HDR_ENGINE, "bSmoothFrameRate", "False");
        f.set_key(SEC_ENGINE, HDR_ENGINE, "DisplayGamma", "3.000000");
        if set_audio {
            f.set_key(SEC_AUDIO, HDR_AUDIO, "AudioDeviceModuleName", "ALAudio");
        }
        // Mirror apply()'s unconditional voice-pool write (default tweaks).
        f.set_key(SEC_AUDIO, HDR_AUDIO, "MaxChannels", "32");
        f.set_key(
            SEC_AUDIO,
            HDR_AUDIO,
            "UnfocusedVolumeMultiplier",
            "0.000000",
        );
        f.render()
    }

    #[test]
    fn preserves_unmanaged_online_section() {
        let out = apply_to(SAMPLE, true);
        assert!(out.contains("[OnlineSubsystemMcp.BaseServiceMcp]"));
        assert!(out.contains("Domain=master-ut4.timiimit.com"));
        assert!(out.contains("Paths=../../../Engine/Content"));
    }

    #[test]
    fn replaces_managed_console_and_adds_renderer() {
        let out = apply_to(SAMPLE, true);
        // Old console value is gone, baseline is in.
        assert!(!out.contains("sg.TextureQuality=3"));
        assert!(out.contains("sg.TextureQuality=0"));
        // Renderer section was absent in SAMPLE and gets created.
        assert!(out.contains("[/script/engine.renderersettings]"));
        assert!(out.contains("r.Streaming.PoolSize=4000"));
    }

    #[test]
    fn sets_engine_keys_and_preserves_siblings() {
        let out = apply_to(SAMPLE, true);
        assert!(out.contains("FrameRateCap=360.000000"));
        assert!(out.contains("bSmoothFrameRate=False"));
        assert!(out.contains("DisplayGamma=3.000000"));
        // A non-managed key in the same section survives.
        assert!(out.contains("bFirstRun=True"));
        // Old values are gone (no dupes left behind).
        assert!(!out.contains("FrameRateCap=120.000000"));
        assert_eq!(out.matches("FrameRateCap=").count(), 1);
    }

    #[test]
    fn audio_key_is_deduped_when_set() {
        let out = apply_to(SAMPLE, true);
        // SAMPLE had AudioDeviceModuleName twice; after apply, exactly one.
        assert_eq!(out.matches("AudioDeviceModuleName=ALAudio").count(), 1);
    }

    #[test]
    fn audio_section_untouched_when_openal_absent() {
        // With set_audio=false we never write the DEVICE key; whatever the
        // user already had is left exactly as-is (we don't add or remove).
        // MaxChannels is a separate, always-written knob.
        let out = apply_to(SAMPLE, false);
        assert_eq!(out.matches("AudioDeviceModuleName=ALAudio").count(), 2);
    }

    #[test]
    fn render_round_trips_an_untouched_file() {
        // SAMPLE is already blank-separated with no trailing blanks, so the
        // normalising renderer reproduces it exactly.
        let f = IniFile::parse(SAMPLE);
        assert_eq!(f.render(), SAMPLE);
    }

    #[test]
    fn render_inserts_blank_line_between_adjacent_sections() {
        let f = IniFile::parse("[A]\nk=v\n[B]\nx=y\n");
        assert_eq!(f.render(), "[A]\nk=v\n\n[B]\nx=y\n");
    }

    #[test]
    fn rendered_sections_are_each_blank_separated() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Engine.ini");
        std::fs::write(&ini, SAMPLE).unwrap();
        repair_master_server(&ini).unwrap();
        let out = std::fs::read_to_string(&ini).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if i > 0 && line.starts_with('[') {
                assert_eq!(
                    lines[i - 1],
                    "",
                    "section `{line}` not preceded by a blank line"
                );
            }
        }
    }

    #[test]
    fn read_tweaks_reads_current_values() {
        let t = read_tweaks(SAMPLE);
        assert_eq!(t.frame_rate_cap, 120.0);
        assert!(t.smooth_frame_rate);
        assert_eq!(t.display_gamma, 1.0);
    }

    #[test]
    fn read_tweaks_defaults_when_absent() {
        let t = read_tweaks("[Core.System]\nPaths=x\n");
        // Absent `bSmoothFrameRate` reports the ENGINE-effective value (true:
        // BaseEngine.ini smooths + clamps to ~62 fps when the key is missing —
        // see read_tweaks). Everything else reports the editable defaults.
        assert!(t.smooth_frame_rate);
        assert_eq!(
            EngineTweaks {
                smooth_frame_rate: false,
                ..t
            },
            EngineTweaks::default()
        );
    }

    #[test]
    fn read_tweaks_explicit_smooth_false_reads_false() {
        let ini = "[/Script/UnrealTournament.UTGameEngine]\nbSmoothFrameRate=False\n";
        assert!(!read_tweaks(ini).smooth_frame_rate);
    }

    #[test]
    fn unfocused_volume_round_trips_and_clamps() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Engine.ini");
        std::fs::write(&ini, SAMPLE).unwrap();

        let tweaks = EngineTweaks {
            unfocused_volume: 0.3,
            ..EngineTweaks::default()
        };
        save_tweaks(&ini, &tweaks).unwrap();
        let out = std::fs::read_to_string(&ini).unwrap();
        assert!(out.contains("UnfocusedVolumeMultiplier=0.300000"));
        assert_eq!(read_tweaks(&out).unfocused_volume, 0.3);

        // Out-of-range values clamp into the engine's meaningful 0..=1 band.
        let loud = EngineTweaks {
            unfocused_volume: 4.0,
            ..EngineTweaks::default()
        };
        save_tweaks(&ini, &loud).unwrap();
        let out = std::fs::read_to_string(&ini).unwrap();
        assert!(out.contains("UnfocusedVolumeMultiplier=1.000000"));
        assert_eq!(out.matches("UnfocusedVolumeMultiplier=").count(), 1);
    }

    #[test]
    fn apply_then_restore_round_trips_via_disk() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Engine.ini");
        std::fs::write(&ini, SAMPLE).unwrap();

        apply(&ini, &EngineTweaks::default(), true).unwrap();
        let applied = std::fs::read_to_string(&ini).unwrap();
        assert!(applied.contains("DisplayGamma=3.000000"));
        assert!(applied.contains("r.Streaming.PoolSize=4000"));
        // Backup holds the pristine original.
        assert_eq!(std::fs::read_to_string(backup_path(&ini)).unwrap(), SAMPLE);

        restore(&ini).unwrap();
        assert_eq!(std::fs::read_to_string(&ini).unwrap(), SAMPLE);
    }

    #[test]
    fn apply_on_missing_ini_errors() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Engine.ini");
        let err = apply(&ini, &EngineTweaks::default(), false).unwrap_err();
        assert!(matches!(err, ConfigError::IniNotFound(_)));
    }

    #[test]
    fn restore_without_backup_errors() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Engine.ini");
        std::fs::write(&ini, SAMPLE).unwrap();
        let err = restore(&ini).unwrap_err();
        assert!(matches!(err, ConfigError::NoBackup(_)));
    }

    #[test]
    fn backup_is_not_overwritten_on_second_apply() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Engine.ini");
        std::fs::write(&ini, SAMPLE).unwrap();

        apply(&ini, &EngineTweaks::default(), true).unwrap();
        // Second apply with different values must not clobber the pristine
        // backup with the already-modified file.
        let tweaks = EngineTweaks {
            frame_rate_cap: 240.0,
            smooth_frame_rate: true,
            display_gamma: 2.0,
            ..EngineTweaks::default()
        };
        apply(&ini, &tweaks, true).unwrap();
        assert_eq!(std::fs::read_to_string(backup_path(&ini)).unwrap(), SAMPLE);
    }

    #[test]
    fn high_polling_cvar_survives_the_competitive_baseline() {
        // apply() replaces the whole [ConsoleVariables] body with the fixed
        // competitive set, so the knob is only safe because set_tweak_keys runs
        // AFTER that. If the ordering is ever swapped, this catches it.
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Engine.ini");
        std::fs::write(&ini, SAMPLE).unwrap();

        let on = EngineTweaks {
            high_polling_mouse: true,
            ..EngineTweaks::default()
        };
        apply(&ini, &on, false).unwrap();
        let out = std::fs::read_to_string(&ini).unwrap();
        assert!(out.contains("ncp.HighPollingMouseCoalesce=1"));
        assert!(read_tweaks(&out).high_polling_mouse);

        // ...and turning it back off writes the explicit 0 rather than dropping
        // the key, so the cvar is actively set to stock instead of left dangling
        // from a previous run.
        let off = EngineTweaks::default();
        apply(&ini, &off, false).unwrap();
        let out = std::fs::read_to_string(&ini).unwrap();
        assert!(out.contains("ncp.HighPollingMouseCoalesce=0"));
        assert!(!read_tweaks(&out).high_polling_mouse);
    }

    #[test]
    fn high_polling_defaults_off_when_absent() {
        // An ini that predates the knob must read as OFF, never as on.
        assert!(!read_tweaks(SAMPLE).high_polling_mouse);
    }

    #[test]
    fn save_tweaks_writes_only_the_knobs() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Engine.ini");
        std::fs::write(&ini, SAMPLE).unwrap();

        let tweaks = EngineTweaks {
            frame_rate_cap: 470.0,
            smooth_frame_rate: false,
            display_gamma: 2.5,
            allow_async_loading: true,
            max_audio_channels: 48,
            unfocused_volume: 1.0,
            high_polling_mouse: true,
        };
        save_tweaks(&ini, &tweaks).unwrap();
        let out = std::fs::read_to_string(&ini).unwrap();

        // The knobs landed…
        assert!(out.contains("FrameRateCap=470.000000"));
        assert!(out.contains("DisplayGamma=2.500000"));
        assert!(out.contains("MaxChannels=48"));
        assert!(out.contains("UnfocusedVolumeMultiplier=1.000000"));
        assert!(out.contains("net.AllowAsyncLoading=1"));
        // …but none of the competitive baseline did (SAMPLE has no
        // [ConsoleVariables] body from us and Save must not add one),
        assert!(!out.contains("r.Streaming.PoolSize"));
        assert!(!out.contains("r.OneFrameThreadLag"));
        // …the device override is Apply's job,
        assert_eq!(out.matches("AudioDeviceModuleName=ALAudio").count(), 2);
        // …and the pristine original was backed up for Restore.
        assert_eq!(std::fs::read_to_string(backup_path(&ini)).unwrap(), SAMPLE);
        assert_eq!(read_tweaks(&out).max_audio_channels, 48);
    }

    #[test]
    fn save_tweaks_on_missing_ini_errors() {
        let dir = tempfile::tempdir().unwrap();
        let err =
            save_tweaks(&dir.path().join("Engine.ini"), &EngineTweaks::default()).unwrap_err();
        assert!(matches!(err, ConfigError::IniNotFound(_)));
    }

    #[test]
    fn applies_max_audio_channels_regardless_of_openal() {
        // Default tweaks write the stock 32 explicitly, with or without the
        // OpenAL device override.
        let with_openal = apply_to(SAMPLE, true);
        assert!(with_openal.contains("MaxChannels=32"));
        // No OpenAL and no pre-existing [Audio] section: the voice-pool knob
        // still lands, and no device override appears.
        let without = apply_to("[Core.System]\nPaths=x\n", false);
        assert!(without.contains("MaxChannels=32"));
        assert!(!without.contains("AudioDeviceModuleName"));
    }

    #[test]
    fn max_audio_channels_round_trips_and_clamps() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Engine.ini");
        std::fs::write(&ini, SAMPLE).unwrap();

        let tweaks = EngineTweaks {
            max_audio_channels: 64,
            ..EngineTweaks::default()
        };
        apply(&ini, &tweaks, true).unwrap();
        let out = std::fs::read_to_string(&ini).unwrap();
        assert!(out.contains("MaxChannels=64"));
        assert_eq!(read_tweaks(&out).max_audio_channels, 64);

        // Out-of-range values clamp instead of writing something the audio
        // device would choke on.
        let too_big = EngineTweaks {
            max_audio_channels: 999,
            ..EngineTweaks::default()
        };
        apply(&ini, &too_big, true).unwrap();
        assert!(std::fs::read_to_string(&ini)
            .unwrap()
            .contains(&format!("MaxChannels={MAX_AUDIO_CHANNELS_MAX}")));

        let too_small = EngineTweaks {
            max_audio_channels: 8,
            ..EngineTweaks::default()
        };
        apply(&ini, &too_small, true).unwrap();
        assert!(std::fs::read_to_string(&ini)
            .unwrap()
            .contains(&format!("MaxChannels={MAX_AUDIO_CHANNELS_MIN}")));
    }

    #[test]
    fn applies_system_settings_section() {
        let out = apply_to(SAMPLE, true);
        assert!(out.contains("[SystemSettings]"));
        // Default tweaks => async loading off (competitive baseline).
        assert!(out.contains("net.AllowAsyncLoading=0"));
        assert!(out.contains("r.OneFrameThreadLag=1"));
    }

    // ── NetcodePlus join waits ─────────────────────────────────────────

    #[test]
    fn join_wait_defaults_when_ini_missing() {
        let dir = tempfile::tempdir().unwrap();
        let st = read_join_wait(&dir.path().join("Mod.ini"));
        assert!(!st.ini_exists);
        assert_eq!(st.tweaks, JoinWaitTweaks::default());
    }

    #[test]
    fn join_wait_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Mod.ini");
        save_join_wait(
            &ini,
            &JoinWaitTweaks {
                profile_wait_seconds: 60,
                no_signal_wait_seconds: 20,
            },
        )
        .unwrap();

        let st = read_join_wait(&ini);
        assert!(st.ini_exists);
        assert_eq!(st.tweaks.profile_wait_seconds, 60);
        assert_eq!(st.tweaks.no_signal_wait_seconds, 20);
    }

    #[test]
    fn join_wait_clamps_on_save_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Mod.ini");
        save_join_wait(
            &ini,
            &JoinWaitTweaks {
                profile_wait_seconds: 9_999,
                no_signal_wait_seconds: 0,
            },
        )
        .unwrap();
        let st = read_join_wait(&ini);
        assert_eq!(st.tweaks.profile_wait_seconds, JOIN_WAIT_MAX);
        assert_eq!(st.tweaks.no_signal_wait_seconds, JOIN_WAIT_MIN);
    }

    #[test]
    fn join_wait_merges_keys_leaving_everything_else_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Mod.ini");
        std::fs::write(
            &ini,
            "[Identifiers]
IDArray=abc123_Someone

[NetcodePlus]
ElimMidGameShuffle=True
",
        )
        .unwrap();

        save_join_wait(&ini, &JoinWaitTweaks::default()).unwrap();
        let out = std::fs::read_to_string(&ini).unwrap();

        // Identity and the section's other NetcodePlus knobs survive — this is
        // a key merge, not a section replace like a preset apply.
        assert!(out.contains("IDArray=abc123_Someone"));
        assert!(out.contains("ElimMidGameShuffle=True"));
        assert!(out.contains("ConnectProfileWaitSeconds=45"));
        assert!(out.contains("ConnectNoSignalWaitSeconds=25"));
        // Pristine original backed up once, so Restore undoes this too.
        assert!(backup_path(&ini).is_file());
    }

    #[test]
    fn join_wait_reads_ue4_float_values() {
        // The plugin reads these with GetFloat and UE4 rewrites its own keys in
        // float form — "50.000000" must read back as 50, not fall to the default.
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Mod.ini");
        std::fs::write(
            &ini,
            "[NetcodePlus]
ConnectProfileWaitSeconds=50.000000
ConnectNoSignalWaitSeconds=30.000000
",
        )
        .unwrap();
        let st = read_join_wait(&ini);
        assert_eq!(st.tweaks.profile_wait_seconds, 50);
        assert_eq!(st.tweaks.no_signal_wait_seconds, 30);
    }

    #[test]
    fn join_wait_ignores_unparseable_values() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Mod.ini");
        std::fs::write(
            &ini,
            "[NetcodePlus]
ConnectProfileWaitSeconds=soon
",
        )
        .unwrap();
        let st = read_join_wait(&ini);
        assert_eq!(
            st.tweaks.profile_wait_seconds,
            JoinWaitTweaks::default().profile_wait_seconds
        );
    }

    // ── Mod.ini presets ────────────────────────────────────────────────

    const MOD_SAMPLE: &str = "\
[Identifiers]
IDArray=abc123_Someone

[Hitsounds.Enemy]
Volume=1.000000
Pitch=1.000000
SoundID=Old Sound

[MyCustomSection]
Keep=1
";

    #[test]
    fn mod_preset_merge_replaces_only_its_sections() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Mod.ini");
        std::fs::write(&ini, MOD_SAMPLE).unwrap();

        apply_mod_preset(&ini, "ictf-tox").unwrap();
        let out = std::fs::read_to_string(&ini).unwrap();

        // Unmanaged sections survive verbatim — identity above all.
        assert!(out.contains("[Identifiers]"));
        assert!(out.contains("IDArray=abc123_Someone"));
        assert!(out.contains("[MyCustomSection]"));
        assert!(out.contains("Keep=1"));
        // The preset-owned section was replaced wholesale.
        assert!(!out.contains("SoundID=Old Sound"));
        assert!(!out.contains("Volume=1.000000"));
        assert!(out.contains("[TeamSkins.Enable]"));
    }

    #[test]
    fn mod_preset_creates_missing_ini_without_backup() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Mod.ini");

        apply_mod_preset(&ini, "dueler-phantaci").unwrap();
        assert!(ini.is_file());
        assert!(!backup_path(&ini).exists());
        let out = std::fs::read_to_string(&ini).unwrap();
        assert!(out.contains("[ForceModels]"));
        // Preamble comments from the preset file must not leak into the
        // player's Mod.ini.
        assert!(!out.contains("SECTION MERGE"));
    }

    #[test]
    fn mod_preset_backup_taken_once_then_restores() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Mod.ini");
        std::fs::write(&ini, MOD_SAMPLE).unwrap();

        apply_mod_preset(&ini, "ictf-tox").unwrap();
        apply_mod_preset(&ini, "dueler-phantaci").unwrap();
        // The pristine pre-preset original is what the backup holds.
        assert_eq!(
            std::fs::read_to_string(backup_path(&ini)).unwrap(),
            MOD_SAMPLE
        );
        restore(&ini).unwrap();
        assert_eq!(std::fs::read_to_string(&ini).unwrap(), MOD_SAMPLE);
    }

    #[test]
    fn mod_preset_unknown_id_errors() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Mod.ini");
        let err = apply_mod_preset(&ini, "nope").unwrap_err();
        assert!(matches!(err, ConfigError::UnknownPreset(_)));
    }

    #[test]
    fn shipped_mod_presets_are_sane_and_sanitized() {
        let mut seen = std::collections::HashSet::new();
        for p in mod_presets() {
            assert!(seen.insert(p.id), "duplicate preset id {}", p.id);
            let parsed = IniFile::parse(p.ini);
            assert!(
                parsed.sections.len() > 3,
                "preset {} has too few sections",
                p.id
            );
            // The sanitization contract: nothing identity-, consent-, or
            // machine-state-shaped may ever ship in a preset. Scan what the
            // merge actually applies — the parsed sections — not the raw
            // file, whose preamble comments legitimately DOCUMENT what was
            // stripped.
            let applied: String = parsed
                .sections
                .iter()
                .map(|s| format!("{}\n{}\n", s.header, s.body.join("\n")))
                .collect();
            let lower = applied.to_lowercase();
            for banned in [
                "[identifiers]",
                "[oldidentifiers]",
                "idarray",
                "[hubtools]",
                "[logosplash]",
                "[botrestrictions]",
                "takescreenshot",
                "bseenfirstrunmenu",
                "hasaccepted",
                "highresscreenshotpostmatch",
            ] {
                assert!(
                    !lower.contains(banned),
                    "preset {} contains banned token {banned}",
                    p.id
                );
            }
        }
    }

    #[test]
    fn allow_async_loading_toggle_writes_one() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Engine.ini");
        std::fs::write(&ini, SAMPLE).unwrap();
        let tweaks = EngineTweaks {
            allow_async_loading: true,
            ..EngineTweaks::default()
        };
        apply(&ini, &tweaks, false).unwrap();
        let out = std::fs::read_to_string(&ini).unwrap();
        assert!(out.contains("net.AllowAsyncLoading=1"));
        assert!(!out.contains("net.AllowAsyncLoading=0"));
        // The rest of the baseline is still written.
        assert!(out.contains("r.OneFrameThreadLag=1"));
    }

    #[test]
    fn read_tweaks_reads_allow_async_loading() {
        let on = "[SystemSettings]\nnet.AllowAsyncLoading=1\n";
        assert!(read_tweaks(on).allow_async_loading);
        let off = "[SystemSettings]\nnet.AllowAsyncLoading=0\n";
        assert!(!read_tweaks(off).allow_async_loading);
        // Absent => default off.
        assert!(!read_tweaks("[Core.System]\nPaths=x\n").allow_async_loading);
    }

    fn with_all_mcp(base: &str) -> String {
        let mut s = base.to_string();
        for name in MCP_SECTIONS {
            s.push_str(&format!(
                "[{name}]\nDomain={MASTER_DOMAIN}\nProtocol=https\n"
            ));
        }
        s
    }

    #[test]
    fn master_server_intact_detects_missing_and_present() {
        // SAMPLE only has BaseServiceMcp, so the required set is incomplete.
        assert!(!master_server_intact(SAMPLE));
        assert!(master_server_intact(&with_all_mcp("[X]\nk=v\n")));
    }

    #[test]
    fn repair_master_server_adds_missing_sections() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Engine.ini");
        std::fs::write(&ini, SAMPLE).unwrap();

        assert!(repair_master_server(&ini).unwrap());
        let out = std::fs::read_to_string(&ini).unwrap();
        assert!(master_server_intact(&out));
        for name in MCP_SECTIONS {
            assert!(out.contains(&format!("[{name}]")), "missing {name}");
        }
    }

    #[test]
    fn repair_master_server_noop_when_already_intact() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Engine.ini");
        std::fs::write(&ini, with_all_mcp("[X]\nk=v\n")).unwrap();
        assert!(!repair_master_server(&ini).unwrap());
    }

    #[test]
    fn transient_write_errors_are_classified() {
        use std::io::Error;
        // Windows: 2 = not found, 5 = access denied, 32 = sharing violation —
        // codes antivirus / OneDrive / a file lock surface; all retryable.
        assert!(is_transient_write_error(&Error::from_raw_os_error(2)));
        assert!(is_transient_write_error(&Error::from_raw_os_error(5)));
        assert!(is_transient_write_error(&Error::from_raw_os_error(32)));
        assert!(is_transient_write_error(&Error::new(
            ErrorKind::NotFound,
            "x"
        )));
        // A genuinely non-transient error is not retried.
        assert!(!is_transient_write_error(&Error::new(
            ErrorKind::InvalidData,
            "x"
        )));
    }

    #[test]
    fn clear_read_only_makes_a_readonly_ini_writable() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Engine.ini");
        std::fs::write(&ini, "x").unwrap();
        let mut perms = std::fs::metadata(&ini).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&ini, perms).unwrap();
        assert!(is_read_only(&ini));
        clear_read_only(&ini).unwrap();
        assert!(!is_read_only(&ini));
    }
}
