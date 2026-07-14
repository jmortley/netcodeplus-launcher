//! Persistent launcher state file.
//!
//! Stores everything the launcher needs to remember between runs:
//! which UT4 install to talk to, which channel the user is on, what
//! paks are currently installed, and which optional paks the user has
//! opted out of.
//!
//! Writes are atomic: bytes go to a sibling temp file first, then
//! [`tempfile::NamedTempFile::persist`] does a rename-over-existing
//! that the OS treats as atomic on the same filesystem. This means a
//! crash mid-write either leaves the old state file intact or
//! replaces it with the new one — never a half-written hybrid.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use ncp_planner::{InstalledPlugin, LocalInstall};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use tracing::debug;

use crate::editor::EditorInstall;
use crate::error::{Result, StateError};
use crate::launch::{LinuxLaunchSettings, Priority};

/// Default channel name selected on first run.
pub const DEFAULT_CHANNEL: &str = "stable";

/// Persistent launcher state.
///
/// Every field has a sensible default so a missing or partial state
/// file degrades to "first run" behaviour rather than failing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LauncherState {
    /// Detected or user-picked UT4 install root. `None` means the
    /// launcher has never identified a working install — UI should
    /// drive a folder picker.
    #[serde(default)]
    pub install_path: Option<PathBuf>,

    /// Channel name from the manifest the user is currently on.
    #[serde(default = "default_channel")]
    pub channel: String,

    /// Snapshot of paks currently on disk (populated after a
    /// successful install run).
    #[serde(default)]
    pub local_install: LocalInstall,

    /// Per-pak install fingerprints (size + mtime), keyed by pak id and paired
    /// with [`Self::local_install`]. Lets a status check notice an installed pak
    /// that was overwritten in place (a server redirect pushing a different
    /// version) without re-hashing it — see [`PakStamp`].
    #[serde(default)]
    pub pak_stamps: HashMap<String, PakStamp>,

    /// Optional pak ids the user has opted out of.
    #[serde(default)]
    pub opted_out: HashSet<String>,

    /// Display label of the launch profile the user last chose, so it
    /// can be re-selected on the next run.
    #[serde(default)]
    pub launch_profile_label: Option<String>,

    /// Process priority to launch the game with.
    #[serde(default)]
    pub launch_priority: Priority,

    /// CPU affinity as an uppercase hex string (no `0x`). `None` or empty
    /// = all cores. Stored as a string, not `u64`, so masks wider than
    /// 2^53 survive the JSON ↔ JS-number round-trip in the webview.
    #[serde(default)]
    pub affinity_mask_hex: Option<String>,

    /// What the launcher window does once a game launches: `"minimize"`
    /// (default), `"close"` (quit the launcher entirely), or `"none"` (stay
    /// open). The game is spawned detached, so closing the launcher never kills
    /// the running game.
    #[serde(default = "default_window_action")]
    pub launch_window_action: String,

    /// ut4stats.com player id the user linked (one-time), for fetching
    /// their stats panel. `None` = not linked.
    #[serde(default)]
    pub ut4stats_playerid: Option<String>,

    /// Display name of the linked ut4stats player (cached for the UI so
    /// it can show "linked as X" without a network round-trip).
    #[serde(default)]
    pub ut4stats_playername: Option<String>,

    /// Per-user PUG token issued by the bot's `/launchertoken` command and
    /// pasted in once. Sent as the `launcher-token` header so the bot can
    /// resolve the player and queue them. `None` = not set.
    #[serde(default)]
    pub launcher_token: Option<String>,

    /// Per-user PUG token for the UTPugs community (autopug). PUG tokens are
    /// **per-community** (each community's bot only knows its own players), so
    /// UTPugs gets its own token slot independent of [`Self::launcher_token`]
    /// (the IGBot/iCTF one). `None` = not linked.
    #[serde(default)]
    pub utpugs_launcher_token: Option<String>,

    /// Per-user PUG token for the UnrealPUGs community (skandalouz's PugApi bot).
    /// Per-community like [`Self::utpugs_launcher_token`]. `None` = not linked.
    #[serde(default)]
    pub unrealpugs_launcher_token: Option<String>,

    /// Whether Discord Rich Presence is enabled (opt-in; default off). When on,
    /// the launcher broadcasts the user's PUG state to their Discord profile.
    /// See `src-tauri/src/presence.rs`.
    #[serde(default)]
    pub discord_presence_enabled: bool,

    /// UT4 master-server account username the user logged in with, used as the
    /// `-AUTH_LOGIN` value at launch and for the UI. NOT a secret — the refresh
    /// token (the secret) lives in the OS credential store, never here. `None` =
    /// not logged in.
    #[serde(default)]
    pub ut4_username: Option<String>,

    /// Display name of the logged-in UT4 account, cached for the UI. `None` =
    /// not logged in.
    #[serde(default)]
    pub ut4_display_name: Option<String>,

    /// Canonical UT4 account id (the master server's `account.ID`), used as the
    /// game's `-epicuserid` launch arg so it boots straight into this account
    /// instead of the account picker. NOT a secret. `None` until the user logs
    /// in with a launcher build that captures it (pre-capture sessions stay
    /// `None`, and the launch path then omits the arg).
    #[serde(default)]
    pub ut4_account_id: Option<String>,

    /// Recorded NetcodePlus plugin install state, **keyed by UT4 install root
    /// path** (the `DetectedInstall.install.root` string). A player can have
    /// several installs that drift to different plugin builds, so each is
    /// tracked independently. Written when the launcher installs the plugin
    /// into a given root; read back to decide up-to-date vs needs-update. The
    /// on-disk `.uplugin` has no reliable version, so this is the version
    /// source of truth.
    #[serde(default)]
    pub installed_plugins: HashMap<String, InstalledPlugin>,

    /// Registered UT4 **editor** installs, keyed by root path string (e.g.
    /// `C:\LAEditorUT4\UnrealTournamentEditor`). Distinct from play installs and
    /// from [`Self::installed_plugins`]: an editor tree is the one a mapper/author
    /// authors in, launched via `UE4Editor.exe`. Populated when the user registers
    /// an editor from the Editor tab; Phase 1 records synced-plugin state per entry.
    #[serde(default)]
    pub editor_installs: HashMap<String, EditorInstall>,

    /// The local UT4 **build tree** project dir (the one holding `Plugins/`, e.g.
    /// `C:\UnrealTournament\UnrealTournament`), used as the source for dev
    /// **sideloads** of freshly-built editor-plugin binaries into a registered
    /// editor install. `None` until the author points the launcher at it. A
    /// Windows dev-only convenience; irrelevant to players.
    #[serde(default)]
    pub build_tree: Option<PathBuf>,

    /// Highest manifest `sequence` the launcher has ever accepted.
    /// Persisted so a later run rejects any manifest carrying a lower
    /// sequence — an active-attacker replay/downgrade of an older,
    /// still-signed, still-unexpired manifest. Starts at 0 (first run
    /// accepts any sequence).
    #[serde(default)]
    pub highest_manifest_sequence: u64,

    /// Absolute path of the launcher exe that last recorded itself as the
    /// installed build (from `std::env::current_exe()`), paired with
    /// [`Self::installed_launcher_version`]. Used to detect a post-update move:
    /// when a launcher of a *higher* version runs from a *different* path, the
    /// recorded path is the now-outdated copy. `None` until a build with this
    /// tracking has run once — so the cleanup prompt is forward-looking and
    /// cannot point at a pre-tracking old build.
    #[serde(default)]
    pub installed_launcher_path: Option<String>,

    /// Version (`CARGO_PKG_VERSION`) recorded with [`Self::installed_launcher_path`].
    #[serde(default)]
    pub installed_launcher_version: Option<String>,

    /// Path of a previous, now-outdated launcher exe the user may want to
    /// delete, set when a post-update move is detected. Surfaced by the
    /// housekeeping UI and cleared once the user removes it or dismisses the
    /// prompt. The delete command only ever removes THIS backend-recorded path,
    /// never one supplied by the webview.
    #[serde(default)]
    pub pending_old_launcher_path: Option<String>,

    /// Onboarding / feature-discovery state — which one-time surfaces the user
    /// has completed or dismissed. Keyed by stable string IDs (never version
    /// numbers), so an item shows exactly once regardless of which launcher
    /// versions the user skips. Defaults to a pristine "nothing seen, first run
    /// unresolved" so a missing/old state file behaves correctly.
    #[serde(default)]
    pub onboarding: OnboardingState,

    /// Linux-only explicit Wine/Proton launch override (prefix + runner) for
    /// setups where Lutris auto-detection can't supply them. `None` = auto-detect
    /// (the default and the happy path). See [`LinuxLaunchSettings`].
    #[serde(default)]
    pub linux_launch: Option<LinuxLaunchSettings>,

    /// Linux-only: whether to let WebKitGTK use its GPU (DMABUF) rendering path.
    /// `Some(true)` = the user opted back into GPU rendering. `Some(false)`/`None`
    /// (the default) = the launcher forces `WEBKIT_DISABLE_DMABUF_RENDERER=1` at
    /// startup, because that renderer white-screens on some AMD/Wayland stacks
    /// (`EGL_BAD_PARAMETER`). An explicit `WEBKIT_DISABLE_DMABUF_RENDERER` in the
    /// environment overrides this either way. Read app-handle-free in `run()`
    /// before the webview forks, so a change takes effect on the next launch.
    #[serde(default)]
    pub linux_gpu_accel: Option<bool>,

    /// Linux-only "gaming mode": while the game runs, a detached watchdog keeps
    /// the game window genuinely fullscreen-and-focused (GNOME/mutter on X11 can
    /// leave a Wine fullscreen window unfocused/`HIDDEN`, drawing the top bar over
    /// the game and blocking fullscreen unredirection) and temporarily disables
    /// the Ubuntu dock, restoring it when the game exits. `Some(true)` = on;
    /// `Some(false)`/`None` (the default) = off. Read at game-launch time.
    #[serde(default)]
    pub linux_gaming_mode: Option<bool>,
}

/// Onboarding / feature-discovery persisted state. See [`LauncherState::onboarding`].
///
/// `completed` gates the first-run walkthrough (Surface 1). `first_run_resolved`
/// records that the launcher has decided ONCE whether this user is a genuine
/// fresh install vs a pre-onboarding existing user, so the decision survives a
/// quit mid-walkthrough and is never re-evaluated. `seen` holds the stable IDs of
/// discovery items the user has dismissed; a pending item is simply one whose ID
/// is absent. Every field defaults, so an old state file (no `onboarding` key)
/// loads as a pristine, unresolved onboarding.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingState {
    /// True once the first-run walkthrough has finished or been skipped, or once
    /// a pre-onboarding existing user has been back-stamped (so they get the
    /// one-time catch-up, never a full re-onboard).
    #[serde(default)]
    pub completed: bool,

    /// Whether the fresh-install-vs-existing-user classification has been made.
    /// Guards the one-time existing-user back-stamp so it runs exactly once.
    #[serde(default)]
    pub first_run_resolved: bool,

    /// Stable IDs of discovery items the user has seen and dismissed.
    #[serde(default)]
    pub seen: HashSet<String>,
}

impl OnboardingState {
    /// Resolve, exactly once, whether this user should see the first-run
    /// walkthrough (Surface 1). `file_existed` is whether a `state.json` was on
    /// disk when the launcher started this session: a genuine fresh install has
    /// none, while a user who predates onboarding has one (without an
    /// `onboarding` block). On the first call we record the decision and, for a
    /// pre-existing user, mark onboarding `completed` so they skip Surface 1 and
    /// instead get the one-time catch-up. Returns `true` when Surface 1 should
    /// run (a fresh install that hasn't completed it). Idempotent thereafter.
    pub fn resolve_first_run(&mut self, file_existed: bool) -> bool {
        if !self.first_run_resolved {
            self.first_run_resolved = true;
            if file_existed {
                // Existing, pre-onboarding user — never re-onboard the whole app.
                self.completed = true;
            }
        }
        !self.completed
    }

    /// Record one or more discovery-item IDs as seen/dismissed.
    pub fn mark_seen<I, S>(&mut self, ids: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.seen.extend(ids.into_iter().map(Into::into));
    }

    /// Mark the first-run walkthrough finished and record its item IDs as seen,
    /// so they never resurface as "what's new" or catch-up.
    pub fn complete<I, S>(&mut self, ids: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.completed = true;
        self.mark_seen(ids);
    }

    /// Reset to pristine state so the flows can be replayed for testing.
    pub fn reset(&mut self) {
        *self = OnboardingState::default();
    }
}

/// Cheap install fingerprint of one pak: its size + mtime at the moment the
/// launcher installed or adopted it.
///
/// Compared — never re-hashed — on each status check to notice an installed pak
/// that was overwritten in place (a server's redirect pushing a different
/// version), so the launcher can restore the manifest's version. A metadata stat
/// is used rather than a content hash specifically so a OneDrive cloud-only
/// placeholder is not hydrated (which a full re-hash would force).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PakStamp {
    /// File size in bytes at install/adopt time.
    pub size_bytes: u64,
    /// File mtime as whole milliseconds since the Unix epoch at install/adopt.
    pub mtime_ms: u64,
}

fn default_channel() -> String {
    DEFAULT_CHANNEL.to_string()
}

fn default_window_action() -> String {
    "minimize".to_string()
}

impl Default for LauncherState {
    fn default() -> Self {
        Self {
            install_path: None,
            channel: default_channel(),
            local_install: LocalInstall::default(),
            pak_stamps: HashMap::new(),
            opted_out: HashSet::new(),
            launch_profile_label: None,
            launch_priority: Priority::default(),
            affinity_mask_hex: None,
            launch_window_action: default_window_action(),
            ut4stats_playerid: None,
            ut4stats_playername: None,
            launcher_token: None,
            utpugs_launcher_token: None,
            unrealpugs_launcher_token: None,
            discord_presence_enabled: false,
            ut4_username: None,
            ut4_display_name: None,
            ut4_account_id: None,
            installed_plugins: HashMap::new(),
            editor_installs: HashMap::new(),
            build_tree: None,
            highest_manifest_sequence: 0,
            installed_launcher_path: None,
            installed_launcher_version: None,
            pending_old_launcher_path: None,
            onboarding: OnboardingState::default(),
            linux_launch: None,
            linux_gpu_accel: None,
            linux_gaming_mode: None,
        }
    }
}

impl LauncherState {
    /// Advance the manifest replay floor to `sequence` when it exceeds the
    /// highest previously accepted, returning `true` if the floor moved.
    /// Call after a manifest verifies, then persist the state, so a later
    /// run rejects any lower-sequence (replayed) manifest. A no-op for
    /// equal or lower values.
    pub fn record_manifest_sequence(&mut self, sequence: u64) -> bool {
        if sequence > self.highest_manifest_sequence {
            self.highest_manifest_sequence = sequence;
            true
        } else {
            false
        }
    }
}

/// Read state from `path`.
///
/// Returns `Ok(None)` if the file does not exist (legitimate first
/// run). Other I/O errors and JSON parse errors are surfaced.
///
/// # Errors
///
/// - [`StateError::Io`] for filesystem errors other than `NotFound`.
/// - [`StateError::Json`] if the file exists but is not valid JSON
///   matching [`LauncherState`].
pub fn read(path: &Path) -> Result<Option<LauncherState>> {
    match fs::read(path) {
        Ok(bytes) => {
            let state: LauncherState = serde_json::from_slice(&bytes)?;
            debug!(path = %path.display(), "loaded state file");
            Ok(Some(state))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {
            debug!(path = %path.display(), "no state file present (first run)");
            Ok(None)
        }
        Err(e) => Err(StateError::Io(e)),
    }
}

/// Write `state` to `path` atomically.
///
/// Writes JSON to a sibling temp file in the same directory, fsyncs
/// it, then renames over `path`. If `path`'s parent directory does
/// not yet exist it is created (recursively).
///
/// # Errors
///
/// - [`StateError::NoParentDir`] if `path` has no parent component.
/// - [`StateError::Io`] for filesystem errors at any step.
/// - [`StateError::Json`] if `state` cannot be serialised (essentially
///   never happens for these types but is surfaced for completeness).
pub fn write(path: &Path, state: &LauncherState) -> Result<()> {
    let json = serde_json::to_vec_pretty(state)?;
    let parent = path
        .parent()
        .ok_or_else(|| StateError::NoParentDir(path.to_path_buf()))?;
    fs::create_dir_all(parent)?;

    let mut tmp = NamedTempFile::new_in(parent)?;
    tmp.write_all(&json)?;
    tmp.as_file_mut().sync_all()?;
    tmp.persist(path).map_err(|e| StateError::Io(e.error))?;

    debug!(
        path = %path.display(),
        bytes = json.len(),
        "wrote state file atomically"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_manifest_sequence_advances_only_upward() {
        let mut state = LauncherState::default();
        assert_eq!(state.highest_manifest_sequence, 0);
        assert!(state.record_manifest_sequence(5));
        assert_eq!(state.highest_manifest_sequence, 5);
        // Equal does not advance.
        assert!(!state.record_manifest_sequence(5));
        assert_eq!(state.highest_manifest_sequence, 5);
        // Lower (a replay attempt) does not advance.
        assert!(!state.record_manifest_sequence(3));
        assert_eq!(state.highest_manifest_sequence, 5);
        // Higher advances the floor.
        assert!(state.record_manifest_sequence(6));
        assert_eq!(state.highest_manifest_sequence, 6);
    }

    #[test]
    fn highest_manifest_sequence_defaults_to_zero_for_old_state_files() {
        // State files written before this field existed must still load,
        // with the floor defaulting to 0.
        let state: LauncherState = serde_json::from_str(r#"{"channel":"stable"}"#).unwrap();
        assert_eq!(state.highest_manifest_sequence, 0);
        assert_eq!(state.installed_launcher_path, None);
        assert_eq!(state.installed_launcher_version, None);
        assert_eq!(state.pending_old_launcher_path, None);
    }

    #[test]
    fn onboarding_defaults_pristine_for_old_state_files() {
        // A state file with no `onboarding` key (any pre-onboarding build) loads
        // with a pristine, unresolved onboarding — not an error.
        let state: LauncherState = serde_json::from_str(r#"{"channel":"stable"}"#).unwrap();
        assert!(!state.onboarding.completed);
        assert!(!state.onboarding.first_run_resolved);
        assert!(state.onboarding.seen.is_empty());
    }

    #[test]
    fn resolve_first_run_fresh_install_shows_walkthrough() {
        // No state file existed (file_existed = false) → genuine fresh install →
        // Surface 1 should run, and stays pending until completed.
        let mut ob = OnboardingState::default();
        assert!(ob.resolve_first_run(false));
        assert!(ob.first_run_resolved);
        assert!(!ob.completed);
        // A quit mid-walkthrough (re-resolve on next launch, file now exists)
        // must NOT flip the user to "existing" — Surface 1 keeps showing.
        assert!(ob.resolve_first_run(true));
        assert!(!ob.completed);
    }

    #[test]
    fn resolve_first_run_existing_user_skips_walkthrough() {
        // A state file existed (pre-onboarding user) → back-stamp completed,
        // skip Surface 1 (they get the one-time catch-up instead). Idempotent.
        let mut ob = OnboardingState::default();
        assert!(!ob.resolve_first_run(true));
        assert!(ob.first_run_resolved);
        assert!(ob.completed);
        assert!(!ob.resolve_first_run(true));
        assert!(!ob.resolve_first_run(false));
    }

    #[test]
    fn complete_marks_walkthrough_ids_seen() {
        let mut ob = OnboardingState::default();
        ob.complete(["competitive-config", "content-paks"]);
        assert!(ob.completed);
        assert!(ob.seen.contains("competitive-config"));
        assert!(ob.seen.contains("content-paks"));
    }

    #[test]
    fn mark_seen_then_reset_clears_everything() {
        let mut ob = OnboardingState::default();
        ob.resolve_first_run(true);
        ob.mark_seen(["rich-presence"]);
        assert!(ob.completed || ob.first_run_resolved || !ob.seen.is_empty());
        ob.reset();
        assert_eq!(ob, OnboardingState::default());
    }
}
