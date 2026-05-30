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

/// `[SystemSettings]` baseline. `net.AllowAsyncLoading=0` loads into maps
/// faster but can misbehave in Blitz / flag-run — surfaced as a UI caveat.
const SYSTEM_SETTINGS: &str = "\
net.AllowAsyncLoading=0
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
}

impl Default for EngineTweaks {
    fn default() -> Self {
        Self {
            frame_rate_cap: 360.0,
            smooth_frame_rate: false,
            display_gamma: 3.0,
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
    /// Underlying filesystem error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
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
        },
        // No readable ini yet — the UI shows "launch UT4 once"; master-server
        // state is irrelevant, so report it as fine to avoid a false alarm.
        Err(_) => ConfigState {
            ini_exists: false,
            has_backup,
            master_server_ok: true,
            tweaks: EngineTweaks::default(),
        },
    }
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
    let text = match std::fs::read_to_string(ini) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Err(ConfigError::IniNotFound(ini.to_path_buf()))
        }
        Err(e) => return Err(e.into()),
    };

    // Back up the pristine original exactly once, so Restore always returns
    // to the pre-launcher config no matter how many times Apply runs.
    let backup = backup_path(ini);
    if !backup.exists() {
        write_atomic(&backup, &text)?;
    }

    let mut ini_file = IniFile::parse(&text);
    ini_file.replace_body(SEC_CONSOLE, HDR_CONSOLE, CONSOLE_VARIABLES);
    ini_file.replace_body(SEC_RENDERER, HDR_RENDERER, RENDERER_SETTINGS);
    ini_file.replace_body(SEC_SYSTEM, HDR_SYSTEM, SYSTEM_SETTINGS);
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
    if set_openal_audio {
        ini_file.set_key(SEC_AUDIO, HDR_AUDIO, "AudioDeviceModuleName", "ALAudio");
    }

    write_atomic(ini, &ini_file.render())
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

/// Ensure all `[OnlineSubsystemMcp.*]` sections exist and point at the
/// community master server, repairing the known wipe bug. Returns whether a
/// change was needed. Backs the ini up once before writing.
///
/// # Errors
/// [`ConfigError::IniNotFound`] if the ini does not exist, or
/// [`ConfigError::Io`] on a filesystem error.
pub fn repair_master_server(ini: &Path) -> ConfigResult<bool> {
    let text = match std::fs::read_to_string(ini) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Err(ConfigError::IniNotFound(ini.to_path_buf()))
        }
        Err(e) => return Err(e.into()),
    };
    if master_server_intact(&text) {
        return Ok(false);
    }
    let backup = backup_path(ini);
    if !backup.exists() {
        write_atomic(&backup, &text)?;
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

/// Read the three editable values out of `[/Script/UnrealTournament.UTGameEngine]`.
fn read_tweaks(text: &str) -> EngineTweaks {
    let mut t = EngineTweaks::default();
    let mut in_section = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') && l.ends_with(']') {
            in_section = section_inner(l).eq_ignore_ascii_case(SEC_ENGINE);
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((k, v)) = l.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        if k.eq_ignore_ascii_case("FrameRateCap") {
            if let Ok(n) = v.parse() {
                t.frame_rate_cap = n;
            }
        } else if k.eq_ignore_ascii_case("bSmoothFrameRate") {
            t.smooth_frame_rate = v.eq_ignore_ascii_case("true");
        } else if k.eq_ignore_ascii_case("DisplayGamma") {
            if let Ok(n) = v.parse() {
                t.display_gamma = n;
            }
        }
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

fn write_atomic(path: &Path, contents: &str) -> ConfigResult<()> {
    let parent = path.parent().ok_or_else(|| {
        ConfigError::Io(std::io::Error::new(
            ErrorKind::InvalidInput,
            "ini path has no parent directory",
        ))
    })?;
    std::fs::create_dir_all(parent)?;
    let mut tmp = NamedTempFile::new_in(parent)?;
    tmp.write_all(contents.as_bytes())?;
    tmp.as_file_mut().sync_all()?;
    tmp.persist(path).map_err(|e| ConfigError::Io(e.error))?;
    Ok(())
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
            out.push_str(&s.header);
            out.push('\n');
            for l in &s.body {
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
        f.replace_body(SEC_SYSTEM, HDR_SYSTEM, SYSTEM_SETTINGS);
        f.set_key(SEC_ENGINE, HDR_ENGINE, "FrameRateCap", "360.000000");
        f.set_key(SEC_ENGINE, HDR_ENGINE, "bSmoothFrameRate", "False");
        f.set_key(SEC_ENGINE, HDR_ENGINE, "DisplayGamma", "3.000000");
        if set_audio {
            f.set_key(SEC_AUDIO, HDR_AUDIO, "AudioDeviceModuleName", "ALAudio");
        }
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
        // With set_audio=false we never write the key; whatever the user
        // already had is left exactly as-is (we don't add or remove).
        let out = apply_to(SAMPLE, false);
        assert_eq!(out.matches("AudioDeviceModuleName=ALAudio").count(), 2);
    }

    #[test]
    fn render_round_trips_an_untouched_file() {
        let f = IniFile::parse(SAMPLE);
        assert_eq!(f.render(), SAMPLE);
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
        assert_eq!(t, EngineTweaks::default());
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
        };
        apply(&ini, &tweaks, true).unwrap();
        assert_eq!(std::fs::read_to_string(backup_path(&ini)).unwrap(), SAMPLE);
    }

    #[test]
    fn applies_system_settings_section() {
        let out = apply_to(SAMPLE, true);
        assert!(out.contains("[SystemSettings]"));
        assert!(out.contains("net.AllowAsyncLoading=0"));
        assert!(out.contains("r.OneFrameThreadLag=1"));
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
}
