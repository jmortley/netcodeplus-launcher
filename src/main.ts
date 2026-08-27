import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { confirm, open } from "@tauri-apps/plugin-dialog";
import { getCurrent, onOpenUrl } from "@tauri-apps/plugin-deep-link";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";

interface UtInstall {
  root: string;
  executable: string;
  launch_args: string[];
  content_paks_dir: string;
  mod_paks_dir: string;
}

type NetcodePlusStatus = "installed" | "missing" | "malformed";
type DetectSource = "desktop_shortcut" | "probe" | "lutris" | "manual";

interface LaunchProfile {
  label: string;
  args: string[];
}

interface DetectedInstall {
  install: UtInstall;
  source: DetectSource;
  netcodeplus: NetcodePlusStatus;
  profiles: LaunchProfile[];
}

// Per-install NetcodePlus plugin status from the `plugin_status` command.
type PluginAction = "none" | "install" | "update" | "up_to_date" | "downgrade_blocked";
interface PluginInstallStatus {
  root: string;
  action: PluginAction;
  installed_version: number | null;
  available_version: number | null;
  // Whether a well-formed NetcodePlus folder is on disk. Distinguishes a manual
  // (present-but-unrecorded → action "install") plugin from a missing one, so the
  // launcher verifies the bytes instead of nagging "not installed".
  present: boolean;
  // Whether this install has a content-fingerprint baseline (drift is checkable
  // locally). false for a hand-install or a pre-fingerprint record → the launcher
  // runs verify_plugin once to establish ground truth.
  baselined: boolean;
}
interface PluginStatusResult {
  plugin_offered: boolean;
  available_version: number | null;
  installs: PluginInstallStatus[];
  any_update_needed: boolean;
  // Release-notes URL for the offered build (manifest `notes_url`); absent on
  // manifests authored before the field — the UI falls back to the plugin's
  // releases page.
  notes_url?: string | null;
}
interface PluginInstallOutcome {
  root: string;
  result: "installed" | "skipped" | "failed";
  detail: string;
}
// Per-install result of `verify_plugin` (baseline a present-but-unverified install).
interface PluginVerifyOutcome {
  root: string;
  result: "current" | "outdated" | "skipped" | "failed";
  detail: string;
}

// One pak the plan would download/remove — mirrors Rust PlanDownload / PlanRemove.
interface PlanDownload {
  id: string;
  filename: string;
  version: string;
  size_bytes: number;
  reason: "missing" | "hash_mismatch";
  required: boolean;
}
interface PlanRemove {
  id: string;
  filename: string;
  reason: "not_in_channel" | "opted_out";
}
// NetcodePlus content-pak status from `pak_status` — drives the dash "Update paks"
// row. `installed_count === 0` selects the "Install paks" vs "Update paks" button
// label + message (the on-PLAY pak prompt was removed in 1.5.2; see launch()).
interface PakStatusResult {
  channel: string;
  paks_offered: boolean;
  up_to_date: boolean;
  installed_count: number;
  to_download: PlanDownload[];
  to_remove: PlanRemove[];
  keep_count: number;
  total_download_bytes: number;
  catalogue: PakChoice[];
}

// One row of the pak checkbox list. `required` paks render as a ticked, disabled
// box — the backend refuses to opt out of them, so the UI only reflects that.
interface PakChoice {
  id: string;
  filename: string;
  version: string;
  size_bytes: number;
  required: boolean;
  opted_out: boolean;
  installed: boolean;
}
interface PakInstallOutcome {
  id: string;
  filename: string;
  result: "installed" | "removed" | "renamed" | "failed";
  detail: string;
}

// Launcher self-update status from `launcher_update_status`. When
// `can_auto_update` is true the manifest carries a signed SHA-256 + size, so the
// launcher can download + verify + relaunch the new exe itself; otherwise it's
// notify-only (the user fetches + runs it from `url`).
interface LauncherUpdateResult {
  update_available: boolean;
  current_version: string;
  available_version: string | null;
  url: string | null;
  can_auto_update: boolean;
}

// Post-update housekeeping status from `launcher_update_housekeeping`.
interface HousekeepingResult {
  old_launcher_path: string | null;
  current_version: string;
  // True only when a desktop "UT4 Community Launcher.lnk" exists but points at a
  // different (old) exe — drives the optional "Update desktop shortcut" button.
  shortcut_needs_update: boolean;
}

// Stray (misplaced) NetcodePlus copies from `scan_strays`.
interface StrayReport {
  kind: string;
  explanation: string;
  path: string;
}
interface InstallStrays {
  root: string;
  strays: StrayReport[];
}

interface AffinityPreset {
  label: string;
  mask_hex: string;
}

interface LauncherState {
  install_path: string | null;
  launch_profile_label: string | null;
  launch_priority: "normal" | "high" | "real_time";
  affinity_mask_hex: string | null;
  launch_window_action: string;
  ut4stats_playerid: string | null;
  ut4stats_playername: string | null;
  launcher_token: string | null;
  utpugs_launcher_token: string | null;
  unrealpugs_launcher_token: string | null;
  discord_presence_enabled?: boolean;
  // Linux-only explicit Wine/Proton launch override; null/absent = auto-detect.
  linux_launch?: LinuxLaunch | null;
  // Linux-only: true = use GPU (DMABUF) webview rendering; false/absent = the
  // launcher disables it (WEBKIT_DISABLE_DMABUF_RENDERER=1) to avoid a white screen.
  linux_gpu_accel?: boolean | null;
  // Linux-only: true = "gaming mode" — while the game runs, a detached watchdog
  // keeps its window fullscreen+focused and temporarily hides the Ubuntu dock.
  linux_gaming_mode?: boolean | null;
}

// (Linux) Explicit Wine/Proton launch override the user set in Settings.
interface LinuxLaunch {
  prefix: string | null;
  wine: string | null;
}

// (Linux) A Wine/Proton runner discovered on this machine, for the dropdown.
interface WineRunner {
  name: string;
  wine: string;
}

// Integrity facts for the UT4 installer, from the signed manifest (trust anchor
// for a user-downloaded file from the third-party UT4Ever host).
interface GameInstallerIntegrity {
  available: boolean;
  version: string;
  url: string;
  sha256: string;
  size_bytes: number;
}

// Result of verifying a downloaded installer against the signed manifest hash.
interface VerifyDownloadResult {
  matched: boolean;
  sha256: string;
  expected: string;
  size_bytes: number;
}

// (Linux) The resolved wine launch preview from resolve_linux_launch.
interface ResolvedWineLaunch {
  program: string;
  args: string[];
  wineprefix: string;
  cwd: string;
  env: [string, string][];
  wrapper: string[];
}

interface PlayerSearchResult {
  id: string;
  name: string;
}

interface PlayerSummary {
  playerid: string;
  playername: string;
  flag: string;
  totals: { games: number; kills: number; deaths: number; kd: number; damage: number };
  accuracy: Record<string, number | null>;
  ratings: { mode: string; rating: number; rd: number; games: number; last_played: string | null }[];
  recent: {
    mode: string;
    map: string;
    server: string;
    played_at: string;
    result: string;
    delta: number;
    match_id: number | null;
  }[];
}

// Per-mode trends from the `ut4stats_trends` command (player_trends_api).
interface PlayerTrends {
  mode: string;
  has_elo: boolean;
  accuracy: { date: string | null; sniper: number | null; lg: number | null; ig: number | null }[];
  form: { games: number; wins: number; losses: number; streak: number; results: string[] };
  rating: { date: string | null; rating: number }[];
}

// The Stats "Trends" mode selector — the six modes from the ut4stats game-mode
// dropdown. Keys match the player_trends endpoint's mode keys.
const TREND_MODES: { key: string; label: string }[] = [
  { key: "elimplus", label: "Team Arena (ElimPlus)" },
  { key: "ctf", label: "CTF" },
  { key: "ictf", label: "iCTF (5v5)" },
  { key: "blitz", label: "Blitz" },
  { key: "wipeout", label: "Wipeout" },
  { key: "duel", label: "Duel" },
  { key: "elimination", label: "Elimination" },
];
// NCRating mode string -> trend mode key, for defaulting the selector to the
// player's most-played rated mode (iCTF folds into the CTF tab).
const NC_MODE_TO_KEY: Record<string, string> = {
  ElimPlus: "elimplus",
  CTF: "ctf",
  iCTF: "ictf",
  Wipeout: "wipeout",
  Duel: "duel",
};

interface EngineTweaks {
  frame_rate_cap: number;
  smooth_frame_rate: boolean;
  display_gamma: number;
  allow_async_loading: boolean;
  max_audio_channels: number;
  unfocused_volume: number;
}

interface ConfigState {
  ini_exists: boolean;
  has_backup: boolean;
  master_server_ok: boolean;
  tweaks: EngineTweaks;
  engine_ini_read_only: boolean;
}

interface ModIniState {
  ini_exists: boolean;
  has_backup: boolean;
  read_only: boolean;
}

interface ModPresetInfo {
  id: string;
  label: string;
  blurb: string;
}

interface NewsItem {
  title: string;
  body: string;
  pinned: boolean;
  date: string;
}

interface PugStatus {
  state: "idle" | "queued" | "readycheck" | "starting" | "live";
  players?: number;
  max_players?: number;
  server?: string;
  password?: string;
  pug_id?: number;
  /** Which pickup/mode a `queued` status is actually for (UTPugs is multi-mode).
   *  Absent on older bots → callers fall back to assuming the selected mode. */
  mode?: string;
  // readycheck — the PUG filled and the check-in ("ready up") step is running.
  // This is the Discord-outage backup: the player can ready up from the launcher
  // instead of the Discord button. Absent on bots that don't expose readycheck
  // yet → the state simply never appears and nothing changes (graceful).
  ready_count?: number;
  ready_needed?: number;
  you_readied?: boolean;
  /** Whole seconds left in the check-in window. Used for urgency wording only —
   *  we deliberately do NOT render a ticking countdown (see the ready-up UI). */
  seconds_left?: number;
}

// One pickup's fill from a bot's tokenless /queues endpoint.
interface QueueRow {
  mode: string;
  players?: number;
  max_players?: number;
}
type PugCommunity = "Instagib Nation" | "UTPugs";
// A near-full queue surfaced on HOME (community-tagged, display label resolved).
interface FillingQueue {
  community: PugCommunity;
  mode: string; // launcher mode key (ictf / wipe / elim / duel) for joining
  label: string; // display label (iCTF / Wipeout / …)
  players: number;
  max: number;
}

interface Ut4Auth {
  logged_in: boolean;
  username: string | null;
  display_name: string | null;
  // The player's UT4ID (Epic account GUID) — null when signed out, or for a
  // pre-account-id-capture session that must re-login once to populate it.
  account_id: string | null;
  // Set by ut4_login only when the sign-in replaced a different account that was
  // already signed in: the previous account's display name (for the switch warning).
  switched_from?: string | null;
}

const homeHero = document.getElementById("home-hero")!;
const advancedPanel = document.getElementById("advanced-launch")!;
const pickButton = document.getElementById("pick-dir") as HTMLButtonElement;
const versionLabel = document.getElementById("version")!;
const statsPanel = document.getElementById("stats-panel")!;
const configPanel = document.getElementById("config-panel")!;
const modiniPanel = document.getElementById("modini-panel")!;
const newsPanel = document.getElementById("news-panel")!;
const serversPanel = document.getElementById("servers-panel")!;

// A registered UT4 editor install (mirrors ncp_host::editor::EditorInstall).
interface EditorInstall {
  root: string;
  label: string;
  editor_exe: string;
  project: string;
  engine_build_id: string | null;
  engine_changelist: number | null;
  launch_args: string[];
  added_at_ms: number;
  last_sync_at_ms: number | null;
}

// Per-plugin sync status for an editor install (mirrors EditorPluginStatusDto).
interface EditorPluginStatus {
  plugin: string;
  action: "install" | "update" | "up_to_date" | "pinned_local_dev" | "sideload_only" | "present";
  source: "signed" | "local_dev" | null;
  installed_version: number | null;
  available_version: number | null;
  sideloadable: boolean;
  engine_mismatch: boolean;
  notes_url: string | null;
}
interface EditorPluginOutcome {
  plugin: string;
  result: string;
  detail: string;
}

const state = {
  installs: [] as DetectedInstall[],
  editorInstalls: [] as EditorInstall[],
  presets: [] as AffinityPreset[],
  selInstall: 0,
  profileLabel: null as string | null,
  priority: "normal" as "normal" | "high" | "real_time",
  affinityHex: "",
  launchWindowAction: "minimize" as "minimize" | "close" | "none",
  linkedId: null as string | null,
  linkedName: null as string | null,
  launcherToken: null as string | null,
  pugStatus: null as PugStatus | null,
  // Which Instagib Nation (UT4IGBot) queue the player is acting on — iCTF
  // (default) or Elim. The token is shared across the community's modes; the
  // selector just scopes join/leave/status to one queue.
  igMode: "ictf" as "ictf" | "elim",
  // UTPugs (autopug) — a second community with its own per-user token, its own
  // status, and several modes (the user picks one). `utpugsConfigured` mirrors
  // the Rust `utpugs_configured()` (false hides the whole UTPugs section).
  utpugsToken: null as string | null,
  utpugsMode: "wipe" as string,
  utpugsStatus: null as PugStatus | null,
  utpugsConfigured: false,
  // UnrealPUGs (skandalouz's PugApi bot) — a third community with its own per-user
  // token. BLITZ-only for now; the bot tracks no live servers, so no connect/
  // spectate/ready — just join/leave/queue-status. `unrealpugsConfigured` mirrors
  // the Rust `unrealpugs_configured()` (false hides the whole section).
  unrealpugsToken: null as string | null,
  unrealpugsStatus: null as PugStatus | null,
  unrealpugsConfigured: false,
  // Live PUGs anyone can spectate (from the tokenless bot /live endpoint) —
  // drives the HOME "watch to learn" banner. Empty when nothing is live.
  livePugs: [] as SpectatePug[],
  // Near-full PUG queues (from the tokenless bot /queues endpoints) — drives the
  // HOME "queue filling — join" nudge. Empty when nothing is close to filling.
  fillingQueues: [] as FillingQueue[],
  ut4: null as Ut4Auth | null,
  trendMode: "" as string,
  // Discord Rich Presence (default ON; mirrors discord_presence_enabled —
  // an explicit off from the Settings toggle is persisted and respected).
  discordPresence: true,
};

function escape(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

// ---- install + launch -----------------------------------------------------

function sourceText(source: DetectSource): string {
  switch (source) {
    case "desktop_shortcut":
      return "found via your desktop shortcut(s)";
    case "probe":
      return "found by scanning common install locations";
    case "lutris":
      return "found in your Lutris config";
    case "manual":
      return "the folder you picked";
  }
}

// Where "what's new in this plugin build" lives. Prefer the manifest's signed
// notes_url (per-build precision, must be https for the gated opener); fall
// back to the plugin's releases page, which always shows the current build's
// notes.
const PLUGIN_NOTES_FALLBACK = "https://github.com/jmortley/NetcodePlusUT4/releases/tag/plugin-latest";
function pluginNotesUrl(): string {
  const u = (statusCache.plugin?.notes_url ?? "").trim();
  return u.startsWith("https://") ? u : PLUGIN_NOTES_FALLBACK;
}

function netcodeplusBadge(
  status: NetcodePlusStatus,
  root?: string,
  outdated = false,
  availVer?: number | null,
): string {
  switch (status) {
    case "installed":
      // Update-aware: an installed-but-OUTDATED build must NOT show a reassuring
      // green "✓ installed" — the version gate kicks an outdated client off current
      // servers, so the hero has to say "update first" (with a one-click Update),
      // not imply ready-to-play. Matches the "update available" card below it.
      if (outdated) {
        // The big primary button below is now the UPDATE action (see
        // renderHomeHero), so the badge is just the reason — no separate button.
        const v = availVer != null ? ` (build ${availVer})` : "";
        return (
          `<span class="warn">⬆&nbsp;NetcodePlus update required${escape(v)} — update before you can play</span>` +
          ` <button class="card-link" data-extlink="${escape(pluginNotesUrl())}" type="button">see what's new</button>`
        );
      }
      // With a root, the badge is a link that opens the plugin folder.
      return root
        ? `<button class="ncp-reveal" type="button" data-root="${escape(root)}" title="Open the NetcodePlus folder">✓ NetcodePlus installed</button>`
        : `<span class="ok">✓ NetcodePlus installed</span>`;
    case "missing":
      return `<span class="warn">NetcodePlus is not installed in this UT4 install</span>`;
    case "malformed":
      return `<span class="warn">NetcodePlus folder looks broken — missing <code>.uplugin</code> or <code>Binaries/</code></span>`;
  }
}

// Re-detect installs after a state change (a plugin install or a stray fix)
// WITHOUT dropping a manually-picked install that auto-detection can't find.
// detect_installs() only returns shortcut/probe hits, so a bare assignment would
// clobber a manual pick at a non-standard path (e.g. D:\NoLibrary\…). Re-validate
// any prior root that detection missed and keep it.
async function refetchInstallsPreservingManual(): Promise<void> {
  const prevRoots = state.installs.map((d) => d.install.root);
  const detected = await invoke<DetectedInstall[]>("detect_installs");
  const known = detected.slice();
  for (const root of prevRoots) {
    if (known.some((d) => d.install.root === root)) continue;
    try {
      const re = await invoke<DetectedInstall | null>("check_install", { path: root });
      if (re) known.push(re);
    } catch (err) {
      console.error("re-validating an install failed:", err);
    }
  }
  state.installs = known;
  if (state.selInstall >= state.installs.length) state.selInstall = 0;
}

// Single-flight: the dash, hero, AND top-bar UPDATE buttons all funnel here, but
// only the dash button self-disables — so without this guard a stray double-click
// (e.g. the top-bar UPDATE) fires two concurrent install_plugin runs that race on
// the same-PID temp download/staging paths, not just waste a download.
let installInFlight = false;
async function doInstallPlugin(force = false): Promise<void> {
  if (installInFlight) return;
  installInFlight = true;
  const btn = (document.getElementById("plugin-update-btn") ??
    document.getElementById("plugin-reinstall-btn")) as HTMLButtonElement | null;
  const status = document.getElementById("plugin-status");
  if (btn) btn.disabled = true;
  if (status) status.textContent = "Downloading and installing… this can take a moment.";
  try {
    const outcomes = await invoke<PluginInstallOutcome[]>("install_plugin", {
      roots: state.installs.map((d) => d.install.root),
      force,
    });
    const installed = outcomes.filter((o) => o.result === "installed").length;
    const failed = outcomes.filter((o) => o.result === "failed");
    if (failed.length) {
      // Show the real per-install error in the dash AND surface it in the hero
      // (the update may have been kicked off from the hero's UPDATE button, which
      // is otherwise left stuck on "Updating…"). Don't re-render the dash
      // (#plugin-panel) — that would wipe this message; the hero is a separate
      // element. surfaceHeroInstallError restores a clickable hero UPDATE so the
      // user can retry after fixing the cause (e.g. close the game / File
      // Explorer, run as admin for a Program Files install).
      const msg = `Install failed: ${failed.map((f) => f.detail).join("; ")}`;
      if (status) status.innerHTML = `<span class="warn">${escape(msg)}</span>`;
      if (btn) btn.disabled = false;
      surfaceHeroInstallError(msg);
      return;
    }
    if (installed === 0) {
      // Nothing was installed. Distinguish "already up to date" (benign — there
      // were installs, all skipped) from "no UT4 install found to act on" (the
      // real failure for a non-standard install not picked in Settings).
      if (btn) btn.disabled = false;
      if (outcomes.length) {
        if (status) status.innerHTML = `<span class="ok">✓ NetcodePlus is already up to date.</span>`;
        // Re-render the hero (its button was left on "Updating…") and refresh the
        // status so an actually-current install flips the hero back to PLAY.
        renderHomeHero();
        void loadStatusData();
      } else {
        const msg =
          "No UT4 install was found to set up. Pick your install folder in the Settings tab, then try again.";
        if (status) status.innerHTML = `<span class="warn">${escape(msg)}</span>`;
        surfaceHeroInstallError(msg);
      }
      return;
    }
    if (status) {
      status.innerHTML = `<span class="ok">✓ NetcodePlus installed in ${installed} install${installed === 1 ? "" : "s"}.</span>`;
    }
    // Success — re-detect so the install badges reflect the new state (preserving
    // a manually-picked install auto-detection can't find), then refresh the
    // status card: loadStatusData re-fetches plugin_status and re-renders
    // #dash-status, so its "install"/"update" line flips to "up to date".
    await refetchInstallsPreservingManual();
    renderHomeHero();
    renderAdvanced();
    void loadStatusData();
  } catch (err) {
    const msg = `Update failed: ${String(err)}`;
    if (status) status.innerHTML = `<span class="warn">${escape(msg)}</span>`;
    if (btn) btn.disabled = false;
    surfaceHeroInstallError(msg);
    console.error("install_plugin failed:", err);
  } finally {
    installInFlight = false;
  }
}

// After a plugin install kicked off from the hero UPDATE button fails (or is a
// no-op), that button is left stuck on a disabled "Updating…". Rebuild the hero
// so it becomes a fresh, clickable UPDATE again, then echo the failure into the
// hero's launch-status line — so the error is visible at the top, not only in
// the dash card. The dash (#plugin-panel) is untouched, so its copy survives.
function surfaceHeroInstallError(message: string): void {
  renderHomeHero();
  const ls = document.getElementById("launch-status");
  if (ls) ls.innerHTML = `<span class="warn">${escape(message)}</span>`;
}

// The pak checkbox panel is a <details>, which loses its open state every time
// the dash re-renders — and toggling a box re-renders. Remember it (plus the
// last confirmation line) so the panel doesn't collapse under the user mid-edit.
let pakChoicesOpen = false;
let pakChoiceMsg = "";

function pakLabel(id: string): string {
  return HUB_PAK_LABELS[id] ?? id;
}

// The pak picker: which paks the launcher keeps up to date. Required paks render
// ticked + disabled (the backend rejects opting out of them regardless). Shown
// whenever the channel offers paks, including when everything is up to date —
// otherwise a settled user would have no way to reach these at all.
function pakChoicesHtml(pk: PakStatusResult): string {
  if (!pk.catalogue.length) return "";
  const rows = pk.catalogue
    .map((c) => {
      const mb = Math.round(c.size_bytes / (1024 * 1024));
      const tag = c.required
        ? `<span class="pak-tag pak-tag-req">Required</span>`
        : `<span class="pak-tag">Recommended</span>`;
      const state = c.installed
        ? ""
        : `<span class="muted"> · not installed</span>`;
      return `<label class="pak-choice">
        <input type="checkbox" data-pak="${escape(c.id)}"${c.opted_out ? "" : " checked"}${
          c.required ? " disabled" : ""
        } />
        <span class="pak-choice-name">${escape(pakLabel(c.id))}</span>
        ${tag}
        <span class="muted">${mb} MB</span>${state}
      </label>`;
    })
    .join("");
  return `<details class="pak-choices"${pakChoicesOpen ? " open" : ""}>
    <summary>Choose paks (${pk.catalogue.length})</summary>
    <p class="src">Required paks are needed to play the NetcodePlus modes. The rest are strongly recommended — unticking one just stops the launcher updating it; the copy you already have stays where it is.</p>
    <div class="pak-choice-list">${rows}</div>
    <div id="pak-choice-status" class="launch-status">${pakChoiceMsg}</div>
  </details>`;
}

// Tick / untick one pak. Checked = "keep this updated" = NOT opted out.
async function onPakChoiceToggle(cb: HTMLInputElement): Promise<void> {
  const id = cb.dataset.pak;
  if (!id) return;
  const wanted = cb.checked;
  cb.disabled = true;
  try {
    await invoke("set_pak_opt_out", { pakId: id, optedOut: !wanted });
    statusCache.paks = await invoke<PakStatusResult>("pak_status");
    pakChoiceMsg = wanted
      ? `<span class="ok">✓ ${escape(pakLabel(id))} will be kept up to date.</span>`
      : `<span class="ok">${escape(pakLabel(id))} left as-is — the launcher won't update it, and your existing file stays.</span>`;
  } catch (err) {
    cb.checked = !wanted; // the write failed, so put the box back
    pakChoiceMsg = `<span class="warn">${escape(String(err))}</span>`;
    console.error("set_pak_opt_out failed:", err);
  } finally {
    cb.disabled = false;
  }
  await renderDashStatus();
}

// Single-flight guard: concurrent doInstallPaks runs would race on the same-PID
// staging files in the paks dir. The dash "Update paks" button is the only caller
// now — the on-PLAY pak prompt was removed in 1.5.2 (see launch()).
let pakInstallInFlight = false;

// Download + install/update the NetcodePlus content paks via `install_paks`,
// reporting coarse per-pak progress into `sink` as `pak-install-progress` events
// arrive. Returns true when nothing failed. Refreshes the cached pak status +
// dash card so the row flips to "up to date". The Rust side refuses while UT4 is
// running (a mounted pak is locked) — that surfaces here as a failure string.
async function doInstallPaks(sink: HTMLElement | null): Promise<boolean> {
  if (pakInstallInFlight) return false;
  pakInstallInFlight = true;
  if (sink) sink.textContent = "Preparing…";
  let unlisten: UnlistenFn | null = null;
  try {
    unlisten = await listen<{ done: number; total: number; filename: string }>(
      "pak-install-progress",
      (e) => {
        const { done, total, filename } = e.payload;
        if (!sink) return;
        sink.textContent = filename
          ? `Downloading paks… (${done + 1} of ${total}: ${filename})`
          : `Finishing up…`;
      },
    );
    const outcomes = await invoke<PakInstallOutcome[]>("install_paks");
    const failed = outcomes.filter((o) => o.result === "failed");
    const changed = outcomes.filter((o) => o.result !== "failed").length;
    if (sink) {
      if (failed.length) {
        sink.innerHTML = `<span class="warn">Some paks failed: ${escape(
          failed.map((f) => `${f.filename} (${f.detail})`).join("; "),
        )}</span>`;
      } else if (changed > 0) {
        sink.innerHTML = `<span class="ok">✓ NetcodePlus content paks installed.</span>`;
      } else {
        sink.innerHTML = `<span class="ok">✓ Content paks already up to date.</span>`;
      }
    }
    // Refresh cached status so a later render is accurate.
    try {
      statusCache.paks = await invoke<PakStatusResult>("pak_status");
    } catch (err) {
      console.error("pak_status refresh failed:", err);
    }
    // Only re-render the dash on success: re-rendering rebuilds #dash-status and
    // would wipe a per-pak failure detail just written into the dash #pak-status
    // sink (and re-arm the button with no context). On a partial failure the
    // message stays put until the next natural refresh (focus / loadStatusData).
    if (!failed.length) void renderDashStatus();
    return failed.length === 0;
  } catch (err) {
    if (sink) sink.innerHTML = `<span class="warn">Pak update failed: ${escape(String(err))}</span>`;
    console.error("install_paks failed:", err);
    return false;
  } finally {
    if (unlisten) unlisten();
    pakInstallInFlight = false;
  }
}

// Warn about misplaced files inside a UT4 install: a stray NetcodePlus copy
// (e.g. a hand-install dropped into Engine/Plugins, which double-loads) OR a
// content pak dropped into the game's Content/Paks folder (where only
// UnrealTournament.pak belongs). Renders prominent warnings into #stray-panel
// with a confirm-gated "Fix this" that removes the offender. Silent when
// everything is in the right place. Aimed at non-tech-savvy testers, so the copy
// is plain-English and the destructive action requires an OS confirm.
async function renderStrays(): Promise<void> {
  const panel = document.getElementById("stray-panel");
  if (!panel) return;
  // Windows-only: scan_strays checks Windows plugin locations. No-op on Linux.
  if (platformOs !== "windows") {
    panel.innerHTML = "";
    return;
  }
  let found: InstallStrays[];
  try {
    found = await invoke<InstallStrays[]>("scan_strays");
  } catch (err) {
    console.error("scan_strays failed:", err);
    panel.innerHTML = "";
    return;
  }
  if (found.length === 0) {
    panel.innerHTML = "";
    return;
  }
  // Flatten to a list of (root, stray) rows with stable indices for handlers.
  const rows: { root: string; stray: StrayReport }[] = [];
  for (const inst of found) {
    for (const stray of inst.strays) rows.push({ root: inst.root, stray });
  }
  panel.innerHTML = `
    <div class="stray-card">
      <div class="stray-title">⚠ Files are in the wrong place</div>
      ${rows
        .map(
          (r, i) => `<div class="stray-row">
            <div>${escape(r.stray.explanation)}</div>
            <div class="src">${escape(r.stray.path)}</div>
            <button class="stray-fix" type="button" data-i="${i}">Fix this</button>
            <span class="stray-status" data-status="${i}"></span>
          </div>`,
        )
        .join("")}
    </div>`;
  panel.querySelectorAll<HTMLButtonElement>(".stray-fix").forEach((btn) => {
    btn.addEventListener("click", () => {
      const r = rows[Number(btn.dataset.i)];
      if (r) void fixStray(r.root, r.stray, btn);
    });
  });
}

async function fixStray(root: string, stray: StrayReport, btn: HTMLButtonElement): Promise<void> {
  const ok = await confirm(
    `${stray.explanation}\n\nRemove this misplaced file?\n\n${stray.path}`,
    { title: "Remove misplaced file", kind: "warning" },
  );
  if (!ok) return;
  const statusEl = btn.parentElement?.querySelector<HTMLElement>(".stray-status");
  btn.disabled = true;
  if (statusEl) statusEl.textContent = "Removing…";
  try {
    await invoke("remove_stray_plugin", { root, kind: stray.kind, path: stray.path });
    if (statusEl) statusEl.innerHTML = `<span class="ok">✓ removed</span>`;
    // Re-detect + re-render so the badge, status card, and stray list all
    // reflect the fixed state (preserving a manually-picked install).
    await refetchInstallsPreservingManual();
    renderHome();
    renderAdvanced();
    void loadStatusData();
  } catch (err) {
    // The launcher couldn't delete it — most often because it sits in a protected
    // install (Program Files), where an unprivileged delete is denied. We do NOT
    // try to elevate and delete ourselves (that's a privilege-escalation footgun);
    // instead offer to open the folder so the user removes it via the OS shell,
    // which handles any elevation through Windows' own trusted prompt.
    if (statusEl) {
      statusEl.innerHTML =
        `<span class="warn">${escape(String(err))} </span>` +
        `<button class="stray-open" type="button">Open folder</button>`;
      statusEl
        .querySelector<HTMLButtonElement>(".stray-open")
        ?.addEventListener("click", () => {
          void invoke("reveal_in_folder", { path: stray.path }).catch((e) =>
            console.error("reveal_in_folder failed:", e),
          );
        });
    }
    btn.disabled = false;
    console.error("remove_stray_plugin failed:", err);
  }
}

function selectedProfileIndex(di: DetectedInstall): number {
  if (state.profileLabel) {
    const saved = di.profiles.findIndex((p) => p.label === state.profileLabel);
    if (saved >= 0) return saved;
  }
  const nonDx12 = di.profiles.findIndex((p) => !p.args.some((a) => /-dx12/i.test(a)));
  return nonDx12 >= 0 ? nonDx12 : 0;
}

function eqHex(a: string, b: string): boolean {
  return a.toUpperCase() === b.toUpperCase();
}

function render() {
  renderHome();
  renderAdvanced();
}

// Cached ut4stats summary for the dashboard "Your stats" card — fetched once by
// fetchSummary so a Home re-render doesn't re-hit the network.
let lastSummary: PlayerSummary | null = null;

// Cached manifest-derived statuses for the dashboard "NetcodePlus & updates"
// card. plugin_status / launcher_update_status / game_installer_info each
// re-verify the signed manifest in Rust (a network fetch), so they're loaded
// once by loadStatusData() rather than on every render().
const statusCache: {
  plugin: PluginStatusResult | null;
  launcher: LauncherUpdateResult | null;
  paks: PakStatusResult | null;
  dotnetAvailable: boolean;
  dotnetOk: boolean;
} = { plugin: null, launcher: null, paks: null, dotnetAvailable: false, dotnetOk: true };

// Roots already run through verify_plugin this session — a present-but-unbaselined
// install is checked once, never re-downloaded on every status refresh.
const verifiedRoots = new Set<string>();

// Establish a fingerprint baseline for any present install the launcher can't yet
// vouch for: a hand-install (no record), or a record written before fingerprints
// existed. verify_plugin downloads the pinned ZIP once and records the build's
// expected fingerprint, so the very next status read reads the install correctly —
// up to date when the bytes match, outdated when they've been hand-swapped to a
// different build — instead of a false "not installed" / false "up to date". No-op
// (and no download) unless such an install exists. Mutates statusCache.plugin in
// place so the next render is right.
async function baselineUnverifiedInstalls(): Promise<void> {
  const p = statusCache.plugin;
  if (!p) return;
  const roots = p.installs
    .filter((i) => i.present && !i.baselined)
    .map((i) => i.root)
    .filter((r) => !verifiedRoots.has(r));
  if (roots.length === 0) return;
  roots.forEach((r) => verifiedRoots.add(r)); // dedupe concurrent passes
  try {
    await invoke<PluginVerifyOutcome[]>("verify_plugin", { roots });
    statusCache.plugin = await invoke<PluginStatusResult>("plugin_status", {
      roots: state.installs.map((d) => d.install.root),
    });
  } catch (err) {
    // A transient failure (e.g. the verify download dropped) must NOT burn the
    // one-shot guard — nothing was recorded, so let a later refresh retry these
    // roots rather than leaving an install stuck mis-reported.
    roots.forEach((r) => verifiedRoots.delete(r));
    console.error("verify_plugin failed:", err);
  }
}

// Re-check the manifest-backed version status (plugin + launcher) on focus at most
// this often — enough that a freshly published release shows without a restart,
// infrequent enough not to hammer the manifest.
const STATUS_REFRESH_MS = 5 * 60 * 1000;
// Timestamp of the last SUCCESSFUL plugin_status. A flaky startup check (network
// not ready yet) leaves this 0, so the focus handler keeps retrying until it lands.
let statusLastOk = 0;

// invoke with a couple of backoff retries — for the manifest-backed startup calls,
// so a cold-start network hiccup self-heals instead of leaving the update prompt
// blank until the user restarts the launcher (the reported "relaunch makes it
// show" symptom). Succeeds on the first try in the normal case (no added latency).
async function invokeWithRetry<T>(
  cmd: string,
  args?: Record<string, unknown>,
  tries = 3,
): Promise<T> {
  let lastErr: unknown;
  for (let i = 0; i < tries; i++) {
    if (i > 0) await new Promise((r) => setTimeout(r, 800 * 2 ** (i - 1)));
    try {
      return await invoke<T>(cmd, args);
    } catch (err) {
      lastErr = err;
      console.error(`${cmd} attempt ${i + 1}/${tries} failed:`, err);
    }
  }
  throw lastErr;
}

let statusLoading = false;
// Single-flight wrapper: focus re-checks (onLauncherVisible) can otherwise stack
// overlapping version checks — most visibly on a never-succeeded cold start, where
// every focus re-fires the retry chain. Concurrent callers are coalesced into one.
async function loadStatusData(): Promise<void> {
  if (statusLoading) return;
  statusLoading = true;
  try {
    await loadStatusDataInner();
  } finally {
    statusLoading = false;
  }
}

// Fetch the manifest-backed statuses once, cache them, then refresh the status
// card. Called at startup and after a plugin update. Each source is independent:
// one failing doesn't block the others (the card just omits that line).
async function loadStatusDataInner(): Promise<void> {
  try {
    statusCache.plugin = await invokeWithRetry<PluginStatusResult>("plugin_status", {
      roots: state.installs.map((d) => d.install.root),
    });
    statusLastOk = Date.now();
  } catch (err) {
    console.error("plugin_status failed after retries:", err);
  }
  // Before rendering, baseline any present install we can't yet vouch for, so it
  // reads correctly (up to date / outdated) instead of a false "not installed" or
  // a stale "up to date". Updates statusCache.plugin in place.
  await baselineUnverifiedInstalls();
  try {
    statusCache.launcher = await invokeWithRetry<LauncherUpdateResult>("launcher_update_status");
  } catch (err) {
    console.error("launcher_update_status failed after retries:", err);
  }
  // Content paks live in one per-user dir (no per-install roots), so this needs
  // no args. Powers the dash "Update paks" row.
  try {
    statusCache.paks = await invokeWithRetry<PakStatusResult>("pak_status");
  } catch (err) {
    console.error("pak_status failed after retries:", err);
  }
  try {
    const gi = await invoke<GameInstallerInfo>("game_installer_info");
    statusCache.dotnetAvailable = gi.available;
    statusCache.dotnetOk = gi.dotnet_ok;
  } catch (err) {
    console.error("game_installer_info failed:", err);
  }
  void renderDashStatus();
  // Re-render the hero too: it renders once (green "installed") before this async
  // status load finishes, so without this the badge never learns the build is
  // outdated. statusCache.plugin is now populated, so the badge flips to the
  // "update available" state.
  renderHomeHero();
}

// The HOME dashboard: greeting, the play hero, and the at-a-glance cards. Every
// card's render swallows its own errors (→ empty/placeholder), so one failing
// data source never breaks Home.
function renderHome() {
  const greet = document.getElementById("dash-greeting");
  if (greet) {
    const name = state.ut4?.display_name ?? state.ut4?.username;
    greet.textContent = name ? `Welcome back, ${name}` : "UT4 Community Launcher";
  }
  // The subtitle carries the signed-in player's UT4ID (the value they paste to
  // link Discord bots and stats sites) as a copyable chip. Signed out → a prompt.
  const sub = document.getElementById("dash-sub");
  if (sub) {
    const u = state.ut4;
    if (u?.logged_in && u.account_id) {
      const id = u.account_id;
      sub.innerHTML =
        `<span class="chip ut4id-chip" title="Your UT4ID — paste it to link Discord bots and stats sites">` +
        `<span class="lbl">UT4ID</span>` +
        `<b class="mono">${escape(id)}</b>` +
        `<button type="button" class="copy-btn" data-copy="${escape(id)}">Copy</button>` +
        `</span>`;
    } else if (u?.logged_in) {
      // Signed in but the id wasn't captured (session predates account-id
      // capture) — one re-login fills it in.
      sub.textContent = "Sign in again to see your UT4ID";
    } else {
      sub.textContent = "Sign in to see your UT4ID";
    }
  }
  renderHomeHero();
  renderHomeReadycheck();
  renderHomeLivePug();
  void renderStrays();
  renderDashStats();
  void renderDashServers();
  void renderDashStatus();
  renderDashCommunity();
  renderDashAccount();
}

// The hero: game art + NetcodePlus badge + the big PLAY, or onboarding when no
// install is detected. The launch-status line and admin-warning slot live
// directly beneath it.
// Whether the selected install's NetcodePlus is outdated (the version gate would
// kick it). Drives both the hero and the top-bar PLAY→UPDATE gating.
function selectedPluginOutdated(): boolean {
  const di = state.installs[state.selInstall];
  if (!di) return false;
  return (
    statusCache.plugin?.installs.find((i) => i.root === di.install.root)?.action === "update"
  );
}

// Keep the always-visible top-bar quick-launch in sync with the hero: a plain
// PLAY when current, an amber UPDATE when the selected install is outdated — so it
// can't be a second way to launch into a version-gate kick.
function renderTopbarPlay(): void {
  const btn = document.getElementById("topbar-play");
  if (!btn) return;
  const outdated = selectedPluginOutdated();
  btn.classList.toggle("topbar-update", outdated);
  btn.innerHTML = outdated
    ? `<svg viewBox="0 0 24 24" class="ico" aria-hidden="true"><path d="M12 4l7 7h-4v9h-6v-9H5z" fill="currentColor" /></svg>UPDATE`
    : `<svg viewBox="0 0 24 24" class="ico" aria-hidden="true"><path d="M8 5v14l11-7z" fill="currentColor" /></svg>PLAY`;
}

function renderHomeHero() {
  renderTopbarPlay();
  if (state.installs.length === 0) {
    homeHero.className = "";
    homeHero.innerHTML = `
      <div class="play-hero">
        <div class="play-hero-overlay">
          <div class="play-title">Unreal Tournament</div>
          <div class="play-sub warn">No UT4 install detected yet</div>
          <div class="hero-cta">
            <button id="pick-install" type="button" class="launch-primary">Locate my UT4 install →</button>
          </div>
        </div>
      </div>
      <p class="src" style="margin-top:0.6rem">Already have UT4? We look for a desktop shortcut — if there isn't one, open <strong>Settings</strong> and pick your <code>UnrealTournament</code> folder. Don't have the game yet? Use <strong>Download &amp; Install UT4</strong> below.</p>`;
    document.getElementById("pick-install")?.addEventListener("click", () => switchView("settings"));
    return;
  }
  if (state.selInstall >= state.installs.length) state.selInstall = 0;
  const di = state.installs[state.selInstall];
  // Is THIS install's plugin out of date? (statusCache.plugin loads async via
  // loadStatusData, which re-calls renderHomeHero — so the badge flips to the
  // update state once the status is known.)
  const pluginInst = statusCache.plugin?.installs.find((i) => i.root === di.install.root);
  const pluginOutdated = pluginInst?.action === "update";
  const pluginAvail = statusCache.plugin?.available_version ?? null;
  // When the build is outdated the big primary button BECOMES the update — not a
  // PLAY that drops the player onto a server the version gate kicks them off of.
  // (Community ask: outdated players ignored a separate "update" notice and hit
  // PLAY anyway, so PLAY itself turns into UPDATE until they're current.)
  const primaryBtn = pluginOutdated
    ? `<button id="hero-update-btn" type="button" class="launch-primary launch-update">⬆&nbsp;&nbsp;UPDATE NETCODEPLUS</button>`
    : `<button id="launch-btn" type="button" class="launch-primary">▶&nbsp;&nbsp;PLAY</button>`;
  homeHero.className = "";
  homeHero.innerHTML = `
    <div class="play-hero">
      <div class="play-hero-overlay">
        <div class="play-title">Unreal Tournament</div>
        <div class="play-sub">${netcodeplusBadge(di.netcodeplus, di.install.root, pluginOutdated, pluginAvail)}</div>
        <div class="hero-cta">
          ${primaryBtn}
          <span class="hero-meta">${escape(di.install.root)}</span>
        </div>
      </div>
    </div>
    <div id="admin-warn-panel"></div>
    <div id="launch-status" class="launch-status"></div>`;
  if (pluginOutdated) {
    const upd = document.getElementById("hero-update-btn") as HTMLButtonElement | null;
    upd?.addEventListener("click", () => {
      upd.disabled = true;
      upd.textContent = "Updating…";
      void doInstallPlugin();
    });
  } else {
    (document.getElementById("launch-btn") as HTMLButtonElement | null)?.addEventListener(
      "click",
      () => void launch(),
    );
  }
  void renderAdminWarning();
}

// A bot mode name -> display label (e.g. "ictf" -> "iCTF").
function pugModeLabel(mode?: string): string {
  if (!mode) return "PUG";
  const m = mode.toLowerCase();
  if (m === "ictf") return "iCTF";
  if (m === "elim") return "Elim";
  return mode;
}

// The Instagib Nation (UT4IGBot) PUG modes the launcher offers — the `mode` key
// sent on join/leave/status, matched against the bot's `modes` table. Mirrors
// the Rust IGBOT_MODES allowlist; keep the two in sync.
const IGBOT_MODES: { key: "ictf" | "elim"; label: string }[] = [
  { key: "ictf", label: "iCTF" },
  { key: "elim", label: "Elim" },
];

// HOME "live PUG — watch it" banner. Tokenless spectate-to-learn: shows any
// live PUG (from the bot's /live endpoint, polled into state.livePugs) so a
// brand-new user can watch a game in one click BEFORE linking a token. Renders
// nothing — and self-clears — when no PUG is live (or the endpoint isn't there
// yet), so it never gets in the way.
function renderHomeLivePug(): void {
  const el = document.getElementById("home-live-pug");
  if (!el) return;
  const pugs = state.livePugs;
  if (!pugs.length) {
    el.className = "";
    el.innerHTML = "";
    return;
  }
  const p = pugs[0];
  const map = p.map ? `<span class="live-pug-map"> · ${escape(p.map)}</span>` : "";
  const more =
    pugs.length > 1 ? ` <span class="live-pug-more">+${pugs.length - 1} more live</span>` : "";
  el.className = "live-pug-banner";
  el.innerHTML = `
    <span class="live-dot" aria-hidden="true"></span>
    <div class="live-pug-text">
      <b>${escape(pugModeLabel(p.mode))} Pick Up Game (PUG) live now</b>${map}
      <span class="live-pug-sub">Watch it to learn the mode — no sign-up needed.${more}</span>
    </div>
    <button id="home-spectate" type="button" class="btn live-pug-watch">▶&nbsp;&nbsp;Watch live</button>
    <div id="home-spectate-status" class="launch-status live-pug-status"></div>`;
  document.getElementById("home-spectate")?.addEventListener("click", () => void watchLivePug());
}

// "Watch live" on the HOME banner. Spectates immediately when there's one live
// PUG; shows a picker when there are several. Uses the already-polled
// state.livePugs (no token needed); spectatePug connects with ?SpectatorOnly=1.
async function watchLivePug(): Promise<void> {
  const status = document.getElementById("home-spectate-status");
  const pugs = state.livePugs;
  if (!pugs.length) {
    if (status) status.textContent = "No live PUG right now.";
    return;
  }
  if (pugs.length === 1) {
    await spectatePug(pugs[0], status);
    return;
  }
  if (status) {
    status.innerHTML =
      `<div>${pugs.length} live games — pick one to watch:</div>` +
      `<div class="discord-btns">${pugs
        .map(
          (p, i) =>
            `<button class="spec-pick" type="button" data-i="${i}">${escape(pugModeLabel(p.mode))}${
              p.map ? ` · ${escape(p.map)}` : ""
            } · #${p.pug_id}</button>`,
        )
        .join("")}</div>`;
    status.querySelectorAll<HTMLButtonElement>(".spec-pick").forEach((btn) => {
      btn.addEventListener("click", () => {
        const p = pugs[Number(btn.dataset.i)];
        if (p) void spectatePug(p, status);
      });
    });
  }
}

// HOME "queue filling — join" nudge. Surfaces a PUG queue that's close to full
// (e.g. 7/10, 6/8) so people jump in to start it. Fed by the tokenless bot
// /queues endpoints (state.fillingQueues). Renders nothing — and self-clears —
// when nothing's near-full (or the endpoint isn't deployed), so it never gets in
// the way. The fullest queue leads; the rest collapse into "+N more filling".
function renderHomeQueuePug(): void {
  const el = document.getElementById("home-queue-pug");
  if (!el) return;
  const qs = state.fillingQueues;
  if (!qs.length) {
    el.className = "";
    el.innerHTML = "";
    return;
  }
  // Fullest first: smallest gap to max, then most players.
  const sorted = [...qs].sort((a, b) => a.max - a.players - (b.max - b.players) || b.players - a.players);
  const q = sorted[0];
  const toGo = q.max - q.players;
  const more =
    sorted.length > 1 ? ` <span class="live-pug-more">+${sorted.length - 1} more filling</span>` : "";
  // Reuses the live-pug banner layout/box; a 🔥 (not the live dot) marks it as a
  // filling queue rather than a live game.
  el.className = "live-pug-banner queue-pug-banner";
  el.innerHTML = `
    <span class="queue-fire" aria-hidden="true">🔥</span>
    <div class="live-pug-text">
      <b>${escape(q.label)} PUG filling — ${q.players}/${q.max}</b>
      <span class="live-pug-sub">${toGo} more to start · ${escape(q.community)}.${more}</span>
    </div>
    <button id="home-queue-join" type="button" class="btn live-pug-watch">Join</button>
    <div id="home-queue-status" class="launch-status live-pug-status"></div>`;
  document.getElementById("home-queue-join")?.addEventListener("click", () => void joinFillingQueue(q));
}

// Join a filling queue from HOME. Switches to the Community tab (where the full
// PUG UI + status live) and fires the join through the existing per-community
// path — which carries the token gate, the rate-limit, and proper status. With
// no token it just lands on the link prompt there.
async function joinFillingQueue(q: FillingQueue): Promise<void> {
  switchView("community");
  if (q.community === "UTPugs") {
    if (!state.utpugsToken) {
      renderUtpugs();
      return;
    }
    state.utpugsMode = q.mode;
    renderUtpugs(); // reflect the picked mode before joining
    await utpugsPug("joinpug");
  } else {
    if (!state.launcherToken) {
      renderPug();
      return;
    }
    // Target the mode the nudge surfaced, not whatever's selected (pug() sends
    // state.igMode) — mirrors the UTPugs branch above.
    state.igMode = q.mode === "elim" ? "elim" : "ictf";
    renderPug();
    await pug("joinpug");
  }
}

// ---- PUG ready-up (the Discord-outage backup) ------------------------------
// When a PUG fills, the bot runs a "check-in": every player must confirm they're
// here or the PUG cancels. That confirm normally happens on a Discord message —
// so when Discord is down, PUGs die. The bot's FastAPI is independent of the
// Discord gateway, so the launcher lets the player ready up directly. The
// `readycheck` state (from pug_status) drives a prominent, impossible-to-miss
// banner + a native notification (for a minimized launcher) + an audio cue.

// Which community a ready-up acts on — selects the token + the Tauri command.
type PugReadyCommunity = "ictf" | "utpugs";

// The community currently in a readycheck, if any (a player is only ever in one
// at a time). iCTF wins a tie, but in practice only one is ever filling.
function activeReadycheck(): { community: PugReadyCommunity; st: PugStatus } | null {
  if (state.pugStatus?.state === "readycheck") return { community: "ictf", st: state.pugStatus };
  if (state.utpugsConfigured && state.utpugsStatus?.state === "readycheck")
    return { community: "utpugs", st: state.utpugsStatus };
  return null;
}

// Shared markup for the ready-up banner, used on HOME and in both Community-tab
// sections. `scope` (home | ictf | utpugs) keeps the button/status ids unique so
// every surface can be on screen at once. All content is static text + integers
// (no remote strings) — no escaping needed.
function readycheckInnerHtml(st: PugStatus, scope: "home" | "ictf" | "utpugs"): string {
  const readied = st.you_readied === true;
  const count = st.ready_count ?? 0;
  const needed = st.ready_needed ?? 0;
  const others = Math.max(0, needed - count);
  const progress = needed > 0 ? ` · ${count}/${needed} ready` : "";
  const headline = readied ? "You're ready ✓" : "✅ Ready up — your PUG is filling!";
  const sub = readied
    ? others > 0
      ? `Waiting for ${others} more to ready up${progress}`
      : "Everyone's ready — the match is starting…"
    : `Confirm you're here so the match can start — works even if Discord is down.${progress}`;
  const btn = readied
    ? `<button id="readyup-btn-${scope}" type="button" class="btn readyup-btn readyup-done" disabled>✓ Readied</button>`
    : `<button id="readyup-btn-${scope}" type="button" class="btn readyup-btn">✅&nbsp;&nbsp;Ready up</button>`;
  return `
    <span class="readycheck-bang" aria-hidden="true">⚡</span>
    <div class="live-pug-text">
      <b>${headline}</b>
      <span class="live-pug-sub">${sub}</span>
    </div>
    ${btn}
    <div id="readyup-status-${scope}" class="launch-status live-pug-status"></div>`;
}

function wireReadycheck(
  container: HTMLElement,
  scope: "home" | "ictf" | "utpugs",
  community: PugReadyCommunity,
): void {
  container
    .querySelector<HTMLButtonElement>(`#readyup-btn-${scope}`)
    ?.addEventListener("click", () => void pugReady(scope, community));
}

// HOME ready-up banner — the headline of the whole feature, placed above the
// live/queue banners because it's the most time-sensitive thing the launcher can
// show. Renders whichever community is in a readycheck; self-clears otherwise.
function renderHomeReadycheck(): void {
  const el = document.getElementById("home-readycheck");
  if (!el) return;
  const rc = activeReadycheck();
  if (!rc) {
    el.className = "";
    el.innerHTML = "";
    return;
  }
  el.className = "readycheck-banner";
  el.innerHTML = readycheckInnerHtml(rc.st, "home");
  wireReadycheck(el, "home", rc.community);
}

// Ready up via the bot's FastAPI (Discord-independent). Marks the player ready
// in the active check-in; once everyone's in, the bot resolves it and the PUG
// proceeds without Discord. `scope` selects which banner's button/status to
// drive (home / ictf / utpugs — several can be on screen); `community` selects
// the token + endpoint (iCTF's pug_ready vs UTPugs' utpugs_ready).
async function pugReady(scope: "home" | "ictf" | "utpugs", community: PugReadyCommunity): Promise<void> {
  const token = community === "utpugs" ? state.utpugsToken : state.launcherToken;
  if (!token?.trim()) return;
  const cmd = community === "utpugs" ? "utpugs_ready" : "pug_ready";
  const status = document.getElementById(`readyup-status-${scope}`);
  const btn = document.getElementById(`readyup-btn-${scope}`) as HTMLButtonElement | null;
  if (btn) btn.disabled = true;
  if (status) status.textContent = "Readying up…";
  try {
    const raw = await invoke<string>(cmd, { token });
    // The /launcher_ready response: "readied" (+ ready counts) or "no_readycheck".
    const res = JSON.parse(raw) as {
      state: string;
      ready_count?: number;
      ready_needed?: number;
    };
    if (res.state === "readied") {
      // Reflect immediately rather than waiting for the 5 s poll: mark
      // you_readied on the right community's status and refresh all surfaces.
      const target = community === "utpugs" ? state.utpugsStatus : state.pugStatus;
      if (target && target.state === "readycheck") {
        target.you_readied = true;
        if (res.ready_count !== undefined) target.ready_count = res.ready_count;
        if (res.ready_needed !== undefined) target.ready_needed = res.ready_needed;
      }
      renderHomeReadycheck();
      renderPug();
      renderUtpugs();
      const others = Math.max(0, (res.ready_needed ?? 0) - (res.ready_count ?? 0));
      const s = document.getElementById(`readyup-status-${scope}`);
      if (s)
        s.innerHTML = `<span class="ok">Readied ✓${
          others > 0 ? ` — waiting for ${others} more` : " — starting…"
        }</span>`;
    } else if (res.state === "no_readycheck") {
      // The check-in already ended (passed, cancelled, or you weren't in one).
      if (status) status.textContent = "No ready-up needed right now.";
      if (community === "utpugs") void pollUtpugsStatus();
      else void pollPugStatus(); // resync to the real current state
    } else if (status) {
      status.textContent = raw;
    }
  } catch (err) {
    console.error("pug_ready failed:", err);
    if (isPugTokenError(err)) {
      if (community === "utpugs") handleUtpugsTokenError();
      else handlePugTokenError();
    } else if (status) {
      // Most likely the bot hasn't deployed /launcher_ready yet, or a transient
      // blip — let them retry (or fall back to readying up in Discord).
      status.innerHTML = `<span class="warn">Couldn't ready up: ${escape(
        String(err),
      )}. Try again, or ready up in Discord.</span>`;
      if (btn) btn.disabled = false;
    }
  }
}

// Native desktop notification when the ready-check starts — so a MINIMIZED
// launcher still alerts the player (the direct replacement for the Discord ping
// they'd normally get). Best-effort: asks for permission once; if denied, the
// in-app banner alone carries it.
async function notifyReadycheck(_st: PugStatus): Promise<void> {
  try {
    let granted = await isPermissionGranted();
    if (!granted) granted = (await requestPermission()) === "granted";
    if (!granted) return;
    sendNotification({
      title: "✅ PUG ready up!",
      body: "Your PUG filled — open the launcher and ready up so the match can start.",
    });
  } catch (err) {
    console.error("ready-up notification failed:", err);
  }
}

// A short two-tone chime for the ready-up alert. WebAudio, no asset. Per
// feedback_no_pickup_timers we use an audio cue for urgency, never a ticking
// pickup-style countdown. Best-effort — silent if the browser blocks audio.
function playReadyChime(): void {
  try {
    const Ctx =
      window.AudioContext ??
      (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
    if (!Ctx) return;
    const ctx = new Ctx();
    const now = ctx.currentTime;
    [880, 1320].forEach((freq, i) => {
      const osc = ctx.createOscillator();
      const gain = ctx.createGain();
      osc.type = "sine";
      osc.frequency.value = freq;
      const t = now + i * 0.16;
      gain.gain.setValueAtTime(0.0001, t);
      gain.gain.exponentialRampToValueAtTime(0.25, t + 0.02);
      gain.gain.exponentialRampToValueAtTime(0.0001, t + 0.15);
      osc.connect(gain).connect(ctx.destination);
      osc.start(t);
      osc.stop(t + 0.16);
    });
    window.setTimeout(() => void ctx.close().catch(() => {}), 600);
  } catch (err) {
    console.error("ready chime failed:", err);
  }
}

// "Your stats" card — top ratings + last couple of matches from the linked
// ut4stats profile (cached in lastSummary). Prompts to link when none is set.
function renderDashStats(): void {
  const el = document.getElementById("dash-stats");
  if (!el) return;
  if (!state.linkedId) {
    el.innerHTML = `
      <h3>Your stats</h3>
      <p class="src">Link your ut4stats.com profile to see your ratings and recent matches at a glance.</p>
      <button class="card-link" data-nav-to="stats" type="button">Link a profile →</button>`;
    return;
  }
  const s = lastSummary;
  if (!s) {
    el.innerHTML = `
      <h3><span class="grow">Your stats</span><button class="card-link" data-nav-to="stats" type="button">View full stats →</button></h3>
      <p class="src">Loading ${escape(state.linkedName ?? "your stats")}…</p>`;
    return;
  }
  // Show modes the player has actually played (drops 0-game seeded ratings like
  // AbsElim) rather than a flat top-4 by rating — otherwise a real but lower mode
  // like iCTF gets pushed off the card. Cap at 6 so the row doesn't run away.
  const rated = s.ratings.filter((r) => r.games > 0).slice(0, 6);
  const chips = rated.length
    ? rated
        .map((r) => `<span class="chip"><span class="lbl">${escape(r.mode)}</span><b>${r.rating}</b></span>`)
        .join("")
    : `<span class="src">No rated matches yet.</span>`;
  const recent = s.recent
    .slice(0, 2)
    .map((m) => {
      const cls = m.result === "win" ? "ok" : "muted";
      return `<div class="drow"><span class="grow">${escape(m.mode)}${
        m.map ? ` · ${escape(m.map)}` : ""
      }</span><span class="${cls}">${escape(m.result)}</span><span class="pop">${escape(m.played_at)}</span></div>`;
    })
    .join("");
  el.innerHTML = `
    <h3><span class="grow">Your stats</span><button class="card-link" data-nav-to="stats" type="button">View full stats →</button></h3>
    <div class="me-name">${escape(s.playername)}</div>
    <div class="chips">${chips}</div>
    ${recent}`;
}

// "Live now" card — live-match and player counts + the top populated matches,
// from the shared server cache (fetched once; the Servers tab refreshes it).
async function renderDashServers(): Promise<void> {
  const el = document.getElementById("dash-servers");
  if (!el) return;
  if (!serverCache.length && !serversFetching) {
    serversFetching = true;
    try {
      serverCache = JSON.parse(await invoke<string>("list_servers")) as GameServerEntry[];
    } catch (err) {
      console.error("list_servers (dash) failed:", err);
    } finally {
      serversFetching = false;
    }
  }
  const withAddr = serverCache.filter(srvHasAddr);
  const matches = withAddr.filter((s) => !isLobby(s));
  const populated = matches
    .filter((s) => srvPlayers(s) > 0)
    .sort((a, b) => srvPlayers(b) - srvPlayers(a));
  const players = matches.reduce((n, s) => n + srvPlayers(s), 0);
  const top = populated
    .slice(0, 3)
    .map((s) => {
      const mode = prettyMode(String(s.attributes?.GAMEMODE_s ?? ""));
      const map = String(s.attributes?.MAPNAME_s ?? "");
      const p = srvPlayers(s);
      const max = Number(s.attributes?.UT_MAXPLAYERS_i ?? s.maxPublicPlayers ?? 0);
      return `<div class="drow"><span class="grow">${escape(mode)}${
        map ? `<span class="map"> · ${escape(map)}</span>` : ""
      }</span><span class="pop">${p}/${max}</span></div>`;
    })
    .join("");
  el.innerHTML = `
    <h3><span class="grow">Live now</span><button class="card-link" data-nav-to="servers" type="button">Browse all →</button></h3>
    <div class="statline"><span class="big-num">${populated.length}</span><span class="muted">live match${
      populated.length === 1 ? "" : "es"
    } ·</span><span class="big-num">${players}</span><span class="muted">player${players === 1 ? "" : "s"}</span></div>
    ${top || `<p class="src">No live matches right now.</p>`}`;
}

// "NetcodePlus & updates" card — plugin / launcher / .NET / UltiCross rollup.
// Reads the cached manifest statuses (no network on re-render); UltiCross is a
// cheap local check done live. Offers the one-click plugin update inline.
async function renderDashStatus(): Promise<void> {
  const el = document.getElementById("dash-status");
  if (!el) return;
  if (statusCache.plugin === null && statusCache.launcher === null) {
    el.innerHTML = `<h3>NetcodePlus &amp; updates</h3><p class="src">Checking for updates…</p>`;
    return;
  }
  const lines: string[] = [];
  let pluginUpdate = false;
  let pluginUpToDate = false;
  let paksUpdate = false;
  const p = statusCache.plugin;
  if (p && p.plugin_offered) {
    const ver = p.available_version != null ? ` (build ${p.available_version})` : "";
    if (p.installs.length === 0) {
      // No known UT4 install to check against — never claim "up to date" when
      // nothing is installed (e.g. the game is at a non-standard path that
      // auto-detection can't find and hasn't been picked in Settings yet).
      lines.push(
        `<div class="statline"><span class="muted">○</span><span>No UT4 install detected — pick your folder in <button class="card-link" data-nav-to="settings" type="button">Settings</button> to set up NetcodePlus.</span></div>`,
      );
    } else if (p.any_update_needed) {
      pluginUpdate = true;
      const needInstall = p.installs.filter((i) => i.action === "install").length;
      const needUpdate = p.installs.filter((i) => i.action === "update").length;
      const n = needInstall + needUpdate;
      const freshInstall = needUpdate === 0;
      const msg = freshInstall
        ? `NetcodePlus is not installed${ver} — install it in ${n} UT4 install${n === 1 ? "" : "s"}.`
        : `NetcodePlus update available${ver} — ${n} install${n === 1 ? "" : "s"}.`;
      lines.push(
        `<div class="statline"><span class="warn">↑</span><span>${escape(msg)} <button class="card-link" data-extlink="${escape(pluginNotesUrl())}" type="button">See what's new</button></span></div>
        <button id="plugin-update-btn" type="button" class="btn btn-sm">${freshInstall ? "Install NetcodePlus" : "Update NetcodePlus"}</button>
        <div id="plugin-status" class="launch-status"></div>`,
      );
    } else {
      pluginUpToDate = true;
      lines.push(
        `<div class="statline"><span class="ok">✓</span><span>NetcodePlus up to date${escape(ver)}.</span></div>
        <button id="plugin-reinstall-btn" type="button" class="link-btn">Reinstall</button>
        <div id="plugin-status" class="launch-status"></div>`,
      );
    }
  }
  const pk = statusCache.paks;
  if (pk && pk.paks_offered) {
    if (pk.up_to_date) {
      lines.push(
        `<div class="statline"><span class="ok">✓</span><span>NetcodePlus content paks up to date.</span></div>`,
      );
    } else {
      paksUpdate = true;
      const missing = pk.installed_count === 0;
      const mb = Math.round(pk.total_download_bytes / (1024 * 1024));
      const n = pk.to_download.length;
      const msg = missing
        ? `NetcodePlus content paks not installed — get them (~${mb} MB) so you always have the right sounds and content.`
        : `${n} NetcodePlus pak${n === 1 ? "" : "s"} out of date (~${mb} MB).`;
      lines.push(
        `<div class="statline"><span class="warn">↑</span><span>${escape(msg)}</span></div>
        <button id="pak-update-btn" type="button" class="btn btn-sm">${missing ? "Install paks" : "Update paks"}</button>
        <div id="pak-status" class="launch-status"></div>`,
      );
    }
    lines.push(pakChoicesHtml(pk));
  }
  const lu = statusCache.launcher;
  if (lu) {
    lines.push(
      lu.update_available && lu.url
        ? `<div class="statline"><span class="warn">↑</span><span>Launcher ${escape(
            lu.available_version ?? "",
          )} available (you have v${escape(lu.current_version)}) — see the banner above.</span></div>`
        : `<div class="statline"><span class="ok">✓</span><span>Launcher v${escape(
            lu.current_version,
          )} — latest.</span></div>`,
    );
  }
  if (platformOs === "windows" && statusCache.dotnetAvailable) {
    lines.push(
      statusCache.dotnetOk
        ? `<div class="statline"><span class="ok">✓</span><span>.NET Desktop Runtime detected.</span></div>`
        : `<div class="statline"><span class="warn">⚠</span><span>.NET Desktop Runtime 6 not detected — <button class="card-link" data-extlink="https://dotnet.microsoft.com/download/dotnet/6.0" type="button">get it</button> (the UT4 installer needs it).</span></div>`,
    );
  }
  const root = state.installs[state.selInstall]?.install.root;
  if (root) {
    try {
      const hasUlti = await invoke<boolean>("ulticross_status", { root });
      lines.push(
        hasUlti
          ? `<div class="statline"><span class="ok">✓</span><span>UltiCross add-on installed.</span></div>`
          : `<div class="statline"><span class="muted">○</span><span>UltiCross not installed — <button class="card-link" data-nav-to="addons" type="button">add-ons</button>.</span></div>`,
      );
    } catch (err) {
      console.error("ulticross_status (dash) failed:", err);
    }
  }
  el.innerHTML = `<h3>NetcodePlus &amp; updates</h3>${lines.join("")}`;
  if (pluginUpdate) {
    document.getElementById("plugin-update-btn")?.addEventListener("click", () => void doInstallPlugin());
  }
  if (pluginUpToDate) {
    // Reinstall = force the current manifest build over whatever's on disk — the
    // repair path when the launcher thinks it's up to date but the actual binary
    // is a manual/dev/corrupted one (which the version record can't see).
    const av = statusCache.plugin?.available_version;
    const buildTxt = av != null ? ` (build ${av})` : "";
    document.getElementById("plugin-reinstall-btn")?.addEventListener("click", () => {
      void (async () => {
        const ok = await confirm(
          `Reinstall NetcodePlus${buildTxt}? This re-downloads the current build and overwrites the plugin files in your UT4 install — including any manual or dev build you've placed there.`,
          { title: "Reinstall NetcodePlus", kind: "warning", okLabel: "Reinstall", cancelLabel: "Cancel" },
        );
        if (ok) await doInstallPlugin(true);
      })();
    });
  }
  if (paksUpdate) {
    document.getElementById("pak-update-btn")?.addEventListener("click", () => {
      void doInstallPaks(document.getElementById("pak-status"));
    });
  }
  const choices = el.querySelector<HTMLDetailsElement>("details.pak-choices");
  if (choices) {
    choices.addEventListener("toggle", () => {
      pakChoicesOpen = choices.open;
      if (!choices.open) pakChoiceMsg = ""; // don't resurrect a stale line on reopen
    });
    el.querySelectorAll<HTMLInputElement>("input[data-pak]").forEach((cb) => {
      cb.addEventListener("change", () => void onPakChoiceToggle(cb));
    });
  }
}

// "Community & PUGs" card — current iCTF PUG state (Connect when live) plus the
// community Discord links (COMMUNITIES). Links use data-extlink → the
// https-gated opener via the delegated click handler.
function renderDashCommunity(): void {
  const el = document.getElementById("dash-community");
  if (!el) return;
  const st = state.pugStatus;
  let pugLine = "";
  if (state.launcherToken && st) {
    // Label by the actual mode (the bot reports it on queued; fall back to the
    // selected mode) so an Elim PUG isn't mislabelled "iCTF" on the dashboard.
    const lbl = `${escape(pugModeLabel(st.mode ?? state.igMode))} PUG`;
    if (st.state === "live" && st.server) {
      pugLine = `<div class="drow"><span class="grow"><b>${lbl}</b><span class="map"> · live now</span></span><button id="dash-pug-connect" type="button" class="btn btn-sm pug-connect">▶ Connect</button></div>`;
    } else if (st.state === "starting") {
      pugLine = `<div class="drow"><span class="grow"><b>${lbl}</b><span class="map"> · starting…</span></span></div>`;
    } else if (st.state === "queued") {
      pugLine = `<div class="drow"><span class="grow"><b>${lbl}</b><span class="map"> · in queue</span></span><span class="pop">${
        st.players ?? 0
      }/${st.max_players ?? 10}</span></div>`;
    }
  }
  const links = COMMUNITIES.map(
    (c) => `<button class="btn btn-discord" data-extlink="${escape(c.url)}" type="button">${escape(c.name)}</button>`,
  ).join("");
  el.innerHTML = `
    <h3><span class="grow">Community &amp; PUGs</span><button class="card-link" data-nav-to="community" type="button">Open →</button></h3>
    ${pugLine}
    <div class="btn-row">${links}</div>`;
  if (st?.state === "live" && st.server) {
    const server = st.server;
    const password = st.password ?? "";
    document
      .getElementById("dash-pug-connect")
      ?.addEventListener("click", () => void connectToPug(server, password));
  }
}

// Account area on Home: the sign-in form when signed out, or the signed-in
// identity + log-out. The top-bar Sign In scrolls/focuses here.
function renderDashAccount(): void {
  const el = document.getElementById("dash-account");
  if (!el) return;
  el.innerHTML = `<div class="dash-card">${ut4AccountHtml()}</div>`;
  wireUt4Account();
}

// Settings tab (sidebar): power-user knobs — install selection, launch profile,
// process priority, CPU affinity. (Performance config renders below it.)
function renderAdvanced() {
  if (state.installs.length === 0) {
    advancedPanel.innerHTML = `<p>No UT4 install detected. Pick your <code>UnrealTournament</code> folder below.</p>`;
    return;
  }
  if (state.selInstall >= state.installs.length) state.selInstall = 0;
  const di = state.installs[state.selInstall];
  const profIdx = selectedProfileIndex(di);
  state.profileLabel = di.profiles[profIdx]?.label ?? null;

  const installPicker =
    state.installs.length > 1
      ? `<label>Install<select id="install-sel">${state.installs
          .map(
            (d, i) =>
              `<option value="${i}"${i === state.selInstall ? " selected" : ""}>${escape(d.install.root)}</option>`,
          )
          .join("")}</select></label>`
      : "";

  const profOpts = di.profiles
    .map((p, i) => `<option value="${i}"${i === profIdx ? " selected" : ""}>${escape(p.label)}</option>`)
    .join("");

  const customAffinity = !state.presets.some((p) => eqHex(p.mask_hex, state.affinityHex));
  const affOpts =
    state.presets
      .map(
        (p) =>
          `<option value="${escape(p.mask_hex)}"${
            !customAffinity && eqHex(p.mask_hex, state.affinityHex) ? " selected" : ""
          }>${escape(p.label)}</option>`,
      )
      .join("") + `<option value="__custom__"${customAffinity ? " selected" : ""}>Custom…</option>`;

  advancedPanel.innerHTML = `
    <div class="install-card">
      <div><strong>${escape(di.install.root)}</strong></div>
      <div class="src">${sourceText(di.source)}</div>
      <div class="ncp">${netcodeplusBadge(di.netcodeplus, di.install.root)}</div>
    </div>
    <div class="controls">
      ${installPicker}
      <label>Launch profile<select id="profile-sel">${profOpts}</select></label>
      <label>Priority
        <select id="priority-sel">
          <option value="normal"${state.priority === "normal" ? " selected" : ""}>Normal</option>
          <option value="high"${state.priority === "high" ? " selected" : ""}>High</option>
          <option value="real_time"${state.priority === "real_time" ? " selected" : ""}>Real-time (not recommended)</option>
        </select>
      </label>
      <div id="realtime-warn" class="warn" style="display:${state.priority === "real_time" ? "" : "none"};font-size:0.85em;margin:-0.3rem 0 0.2rem">⚠️ Real-time can make Windows unresponsive (input, audio and networking compete with the game) and only takes full effect when the launcher runs as administrator — otherwise Windows caps it at High.</div>
      <label>When the game launches
        <select id="onlaunch-sel">
          <option value="minimize"${state.launchWindowAction === "minimize" ? " selected" : ""}>Minimize launcher</option>
          <option value="close"${state.launchWindowAction === "close" ? " selected" : ""}>Close launcher</option>
          <option value="none"${state.launchWindowAction === "none" ? " selected" : ""}>Keep launcher open</option>
        </select>
      </label>
      <label>CPU affinity<select id="affinity-sel">${affOpts}</select></label>
      <label id="affinity-hex-wrap" style="display:${customAffinity ? "" : "none"}">Mask (hex)
        <input id="affinity-hex" type="text" placeholder="e.g. FFC" spellcheck="false" value="${
          customAffinity ? escape(state.affinityHex) : ""
        }" />
      </label>
      <label class="discord-rpc-toggle"><input id="discord-rpc" type="checkbox"${
        state.discordPresence ? " checked" : ""
      } /> Show my PUG status on Discord (Rich Presence)</label>
    </div>
    ${platformOs === "linux" ? wineProtonPanel() : ""}`;
  wire();
  if (platformOs === "linux") wireWineProton(di);
}

// (Linux) "Wine / Proton" settings panel: an explicit prefix + runner override
// for setups where Lutris auto-detection can't supply them. Empty (Auto-detect)
// is the default and the happy path.
function wineProtonPanel(): string {
  // Selection reflects the saved override: a matching runner, System wine, a
  // custom path, or Auto-detect when there's no override.
  const savedWine = linuxLaunch?.wine ?? null;
  const matchByPath = savedWine ? wineRunners.find((r) => r.wine === savedWine) : undefined;
  let sel = "__auto__";
  if (linuxLaunch) sel = matchByPath ? matchByPath.wine : savedWine ? "__custom__" : "__system__";
  const runnerOpts = wineRunners
    .map((r) => `<option value="${escape(r.wine)}"${sel === r.wine ? " selected" : ""}>${escape(r.name)}</option>`)
    .join("");
  const custom = sel === "__custom__";
  return `
    <div class="controls wine-proton" id="wine-proton" style="margin-top:1rem;border-top:1px solid var(--line,#333);padding-top:0.8rem">
      <div><strong>Wine / Proton (Linux)</strong></div>
      <p class="src" style="margin-top:0">By default the launcher auto-detects your prefix and runner from Lutris. Override them here to launch your own way.</p>
      <label>Runner
        <select id="wine-runner-sel">
          <option value="__auto__"${sel === "__auto__" ? " selected" : ""}>Auto-detect (Lutris)</option>
          <option value="__system__"${sel === "__system__" ? " selected" : ""}>System wine</option>
          ${runnerOpts}
          <option value="__custom__"${custom ? " selected" : ""}>Custom path…</option>
        </select>
      </label>
      <label id="wine-custom-wrap" style="display:${custom ? "" : "none"}">Wine binary path
        <input id="wine-custom" type="text" spellcheck="false" placeholder="/path/to/wine" value="${
          custom ? escape(savedWine ?? "") : ""
        }" />
      </label>
      <label>WINEPREFIX (the folder containing <code>drive_c</code>)
        <span style="display:flex;gap:0.4rem">
          <input id="wine-prefix" type="text" spellcheck="false" style="flex:1" placeholder="/home/you/Games/ut4-prefix" value="${escape(
            linuxLaunch?.prefix ?? "",
          )}" />
          <button id="wine-prefix-browse" type="button">Browse…</button>
        </span>
      </label>
      <span style="display:flex;gap:0.4rem;align-items:center">
        <button id="wine-save" type="button" class="launch-primary">Save override</button>
        <button id="wine-clear" type="button"${linuxLaunch ? "" : " disabled"}>Clear (auto-detect)</button>
      </span>
      <div id="wine-resolved" class="src" style="white-space:pre-wrap;word-break:break-all"></div>
      <label style="display:flex;align-items:flex-start;gap:0.5rem;margin-top:0.9rem;cursor:pointer">
        <input id="linux-gpu-accel" type="checkbox"${linuxGpuAccel ? " checked" : ""} style="margin-top:0.2rem" />
        <span>Use GPU-accelerated rendering
          <span class="src" style="display:block">Faster, but uncheck it if the launcher opens to a blank / white window (a WebKitGTK issue on some AMD/Wayland setups). Restart the launcher to apply.</span>
        </span>
      </label>
      <label style="display:flex;align-items:flex-start;gap:0.5rem;margin-top:0.9rem;cursor:pointer">
        <input id="linux-gaming-mode" type="checkbox"${linuxGamingMode ? " checked" : ""} style="margin-top:0.2rem" />
        <span>Gaming mode: keep UT4 fullscreen, hide the dock
          <span class="src" style="display:block">While the game runs, a helper keeps the game's window truly fullscreen and focused (stops the GNOME top bar / dock drawing over the game and restores full performance) and temporarily disables the Ubuntu dock, bringing it back when you quit. GNOME on X11; applies from the next game launch. You can still close the launcher after launching.</span>
        </span>
      </label>
    </div>`;
}

function wireWineProton(di: DetectedInstall) {
  const runnerSel = document.getElementById("wine-runner-sel") as HTMLSelectElement | null;
  const customWrap = document.getElementById("wine-custom-wrap");
  const customInput = document.getElementById("wine-custom") as HTMLInputElement | null;
  const prefixInput = document.getElementById("wine-prefix") as HTMLInputElement | null;
  const browse = document.getElementById("wine-prefix-browse");
  const saveBtn = document.getElementById("wine-save");
  const clearBtn = document.getElementById("wine-clear");
  const resolved = document.getElementById("wine-resolved");

  const profile = di.profiles.find((p) => p.label === state.profileLabel) ?? di.profiles[selectedProfileIndex(di)];

  const refreshResolved = async () => {
    if (!resolved) return;
    try {
      const plan = await invoke<ResolvedWineLaunch | null>("resolve_linux_launch", {
        executable: di.install.executable,
        args: profile?.args ?? [],
      });
      if (!plan) {
        resolved.textContent =
          "No launch resolves yet — set a WINEPREFIX (and runner) above, or ensure this install has a Lutris config.";
        return;
      }
      const argv = [...plan.wrapper, plan.program, ...plan.args];
      resolved.textContent = `Will run:\nWINEPREFIX=${plan.wineprefix}\n${argv.join(" ")}`;
    } catch (err) {
      resolved.textContent = `Could not resolve launch: ${String(err)}`;
    }
  };
  void refreshResolved();

  runnerSel?.addEventListener("change", () => {
    if (customWrap) customWrap.style.display = runnerSel.value === "__custom__" ? "" : "none";
    if (runnerSel.value === "__custom__") customInput?.focus();
  });

  browse?.addEventListener("click", async () => {
    try {
      const dir = await open({ directory: true, multiple: false, title: "Select the Wine prefix (the folder containing drive_c)" });
      if (typeof dir === "string" && prefixInput) prefixInput.value = dir;
    } catch (err) {
      console.error("prefix dialog open failed:", err);
    }
  });

  saveBtn?.addEventListener("click", async () => {
    const choice = runnerSel?.value ?? "__auto__";
    if (choice === "__auto__") {
      // Auto-detect = no override; same as Clear.
      await saveLinuxLaunch(null, null);
      return;
    }
    const prefix = prefixInput?.value.trim() ?? "";
    if (!prefix) {
      if (resolved) resolved.textContent = "A WINEPREFIX is required to override the launch. Pick the folder that contains drive_c.";
      prefixInput?.focus();
      return;
    }
    let wine: string | null = null;
    if (choice === "__system__") wine = null;
    else if (choice === "__custom__") wine = customInput?.value.trim() || null;
    else wine = choice; // a runner's wine path
    await saveLinuxLaunch(prefix, wine);
  });

  clearBtn?.addEventListener("click", () => saveLinuxLaunch(null, null));

  // Webview rendering toggle (persisted to state; applied on next launcher start).
  const gpuAccel = document.getElementById("linux-gpu-accel") as HTMLInputElement | null;
  gpuAccel?.addEventListener("change", async () => {
    const enabled = gpuAccel.checked;
    try {
      await invoke("save_linux_gpu_accel", { enabled });
      linuxGpuAccel = enabled;
    } catch (err) {
      console.error("save_linux_gpu_accel failed:", err);
      gpuAccel.checked = linuxGpuAccel; // revert the UI to the persisted value
    }
  });

  // Gaming-mode toggle (persisted to state; read at game-launch time).
  const gamingMode = document.getElementById("linux-gaming-mode") as HTMLInputElement | null;
  gamingMode?.addEventListener("change", async () => {
    const enabled = gamingMode.checked;
    try {
      await invoke("save_linux_gaming_mode", { enabled });
      linuxGamingMode = enabled;
    } catch (err) {
      console.error("save_linux_gaming_mode failed:", err);
      gamingMode.checked = linuxGamingMode; // revert the UI to the persisted value
    }
  });

  async function saveLinuxLaunch(prefix: string | null, wine: string | null) {
    try {
      await invoke("save_linux_launch", { prefix, wine });
      linuxLaunch = prefix ? { prefix, wine } : null;
      renderAdvanced(); // re-render so selection + Clear-button state reflect the save
    } catch (err) {
      console.error("save_linux_launch failed:", err);
      if (resolved) resolved.textContent = `Save failed: ${String(err)}`;
    }
  }
}

function wire() {
  const installSel = document.getElementById("install-sel") as HTMLSelectElement | null;
  installSel?.addEventListener("change", () => {
    state.selInstall = Number(installSel.value);
    state.profileLabel = null;
    render();
    persist();
  });

  const profileSel = document.getElementById("profile-sel") as HTMLSelectElement | null;
  profileSel?.addEventListener("change", () => {
    const di = state.installs[state.selInstall];
    state.profileLabel = di.profiles[Number(profileSel.value)]?.label ?? null;
    persist();
  });

  const prioritySel = document.getElementById("priority-sel") as HTMLSelectElement | null;
  const realtimeWarn = document.getElementById("realtime-warn");
  prioritySel?.addEventListener("change", async () => {
    const choice = prioritySel.value as "normal" | "high" | "real_time";
    // Real-time needs an explicit opt-in: it can starve the OS and only truly
    // applies when the launcher runs elevated. Confirm before persisting; revert
    // the dropdown if the user backs out.
    if (choice === "real_time") {
      const ok = await confirm(
        "Real-time priority is NOT recommended. It can make Windows unresponsive — input, audio and networking have to compete with the game — and it only takes full effect if you run the launcher as administrator (otherwise Windows caps it at High). Use it anyway?",
        { title: "Real-time priority", kind: "warning", okLabel: "Use real-time", cancelLabel: "Cancel" },
      );
      if (!ok) {
        prioritySel.value = state.priority; // back to the previous choice
        return;
      }
    }
    state.priority = choice;
    if (realtimeWarn) realtimeWarn.style.display = choice === "real_time" ? "" : "none";
    persist();
  });

  const onLaunchSel = document.getElementById("onlaunch-sel") as HTMLSelectElement | null;
  onLaunchSel?.addEventListener("change", () => {
    state.launchWindowAction = onLaunchSel.value as "minimize" | "close" | "none";
    persist();
  });

  const affSel = document.getElementById("affinity-sel") as HTMLSelectElement | null;
  const hexWrap = document.getElementById("affinity-hex-wrap");
  const hexInput = document.getElementById("affinity-hex") as HTMLInputElement | null;
  affSel?.addEventListener("change", () => {
    if (affSel.value === "__custom__") {
      if (hexWrap) hexWrap.style.display = "";
      state.affinityHex = hexInput?.value.trim() ?? "";
      hexInput?.focus();
    } else {
      if (hexWrap) hexWrap.style.display = "none";
      state.affinityHex = affSel.value;
    }
    persist();
  });
  hexInput?.addEventListener("change", () => {
    state.affinityHex = hexInput.value.trim();
    persist();
  });

  const rpc = document.getElementById("discord-rpc") as HTMLInputElement | null;
  rpc?.addEventListener("change", () => {
    state.discordPresence = rpc.checked;
    void invoke("set_discord_presence_enabled", { enabled: rpc.checked }).catch((err) =>
      console.error("set_discord_presence_enabled failed:", err),
    );
    updateDiscordPresence();
  });
}

function persist() {
  const di = state.installs[state.selInstall];
  if (!di) return;
  void invoke("save_launch_prefs", {
    installPath: di.install.root,
    profileLabel: state.profileLabel,
    priority: state.priority,
    affinityMaskHex: state.affinityHex || null,
    windowAction: state.launchWindowAction,
  }).catch((err) => console.error("save_launch_prefs failed:", err));
}

async function launch() {
  // A dash "Update paks" install in progress holds DownloadedPaks open and must
  // finish before the game mounts those paks — launching now would race it and
  // lock the paks it hasn't placed yet. Refuse and let it finish, then press Play
  // again.
  if (pakInstallInFlight) {
    const ls = document.getElementById("launch-status");
    if (ls)
      ls.innerHTML = `<span class="warn">NetcodePlus paks are still installing — Play will be ready once that finishes.</span>`;
    return;
  }
  const di = state.installs[state.selInstall];
  // If UT4 is already running, pressing Play would spawn a second instance.
  // Warn and let them launch anyway (Play has no server target, so there's no
  // `open` command to hand back — just guard the accidental double-launch).
  let gameRunning = false;
  try {
    gameRunning = await invoke<boolean>("is_game_running", {
      executable: di.install.executable,
    });
  } catch (err) {
    console.error("is_game_running failed:", err);
  }
  if (gameRunning) {
    const go = await confirm("UT4 is already running. Launch another instance anyway?", {
      title: "UT4 already running",
      kind: "warning",
      okLabel: "Launch anyway",
      cancelLabel: "Cancel",
    });
    if (!go) return;
  }
  // Pre-play NetcodePlus check: if this install's plugin is missing or out of
  // date, warn before launching — players otherwise join NetcodePlus servers on
  // the wrong build. Non-blocking: Install/Update now runs the installer and
  // cancels the launch; Play anyway proceeds. Skipped when status isn't known yet.
  const pluginInst = statusCache.plugin?.installs.find((i) => i.root === di.install.root);
  if (pluginInst && (pluginInst.action === "install" || pluginInst.action === "update")) {
    const avail = statusCache.plugin?.available_version;
    const verText = avail != null ? ` (build ${avail})` : "";
    const outdated = pluginInst.action === "update";
    const doUpdate = await confirm(
      outdated
        ? `A newer NetcodePlus is available${verText} — you're on build ${pluginInst.installed_version ?? "?"}. Joining a NetcodePlus server on an old build can cause problems. Update before playing?`
        : `NetcodePlus isn't installed${verText} for this UT4 install — NetcodePlus servers require it. Install before playing?`,
      {
        title: outdated ? "NetcodePlus update available" : "NetcodePlus not installed",
        kind: "warning",
        okLabel: outdated ? "Update now" : "Install now",
        cancelLabel: "Play anyway",
      },
    );
    if (doUpdate) {
      void doInstallPlugin();
      return;
    }
  }
  // NOTE (1.5.2): the launcher deliberately does NOT prompt to install/update the
  // content paks on PLAY. Auto-pushing the LATEST pak breaks play on a hub whose
  // owner hasn't updated yet — the player's newer local pak mismatches the hub's
  // older one and they can't join. Paks are optional (never required to launch),
  // pak management is user-initiated (dash "Update paks"), and the Servers tab
  // surfaces each hub's pak version so a mismatch is visible/fixable per-hub.
  const profile =
    di.profiles.find((p) => p.label === state.profileLabel) ?? di.profiles[selectedProfileIndex(di)];
  const status = document.getElementById("launch-status")!;

  status.textContent = "Launching…";
  let auth: Ut4AuthLaunch;
  try {
    auth = await ut4AuthArgs(di.install.root);
  } catch (err) {
    if (handleReloginError(err, null)) return;
    status.innerHTML = `<span class="warn">UT4 login failed: ${escape(String(err))}</span>`;
    return;
  }
  try {
    await invoke("launch_game", {
      executable: di.install.executable,
      args: [...profile.args, ...auth.args],
      priority: state.priority,
      affinityMaskHex: state.affinityHex || null,
      windowAction: state.launchWindowAction,
      env: auth.env,
    });
    persist();
    status.innerHTML = `<span class="ok">Launched: ${escape(profile.label)} (${escape(
      state.priority === "real_time" ? "real-time" : state.priority,
    )} priority${state.affinityHex ? `, affinity ${escape(state.affinityHex)}` : ""})</span>`;
  } catch (err) {
    const msg = String(err);
    if (msg.includes("740") || msg.toLowerCase().includes("elevation")) {
      status.innerHTML = `<span class="warn">Launch failed: Windows says the game needs administrator. It's likely set to "Run as administrator" — use the notice above to clear that flag (recommended), or launch as admin.</span>`;
      void renderAdminWarning();
    } else {
      status.innerHTML = `<span class="warn">Launch failed: ${escape(msg)}</span>`;
    }
    console.error("launch_game failed:", err);
  }
}

// Up-front warning when the game exe is set to "Run as administrator" (the
// os-740 cause). Offers the recommended one-click fix (clear the flag) and a
// warned escape hatch (launch elevated anyway). Silent when not set; a check
// failure just leaves the slot empty.
async function renderAdminWarning(): Promise<void> {
  const panel = document.getElementById("admin-warn-panel");
  if (!panel) return;
  // "Run as administrator" is a Windows compat flag — N/A on Linux (the Wine
  // prefix is user-owned), so this warning + the elevation escape hatch are hidden.
  if (platformOs !== "windows") {
    panel.innerHTML = "";
    return;
  }
  const exe = state.installs[state.selInstall]?.install.executable;
  if (!exe) {
    panel.innerHTML = "";
    return;
  }
  let requires = false;
  try {
    requires = await invoke<boolean>("game_requires_admin", { executable: exe });
  } catch (err) {
    console.error("game_requires_admin failed:", err);
    panel.innerHTML = "";
    return;
  }
  if (!requires) {
    panel.innerHTML = "";
    return;
  }
  panel.innerHTML = `
    <div class="alert">
      <div>⚠ Your UT4 is set to <strong>"Run as administrator"</strong>, which stops one-click Launch — Windows won't let the launcher start it without elevation.</div>
      <div class="cleanup-actions">
        <button id="admin-fix" type="button">Fix it (recommended)</button>
        <button id="admin-run" type="button" class="link-btn">Launch as admin anyway</button>
      </div>
      <div id="admin-warn-status" class="launch-status"></div>
    </div>`;
  document.getElementById("admin-fix")?.addEventListener("click", () => void doClearAdmin(exe));
  document.getElementById("admin-run")?.addEventListener("click", () => void doLaunchElevated());
}

async function doClearAdmin(exe: string): Promise<void> {
  const status = document.getElementById("admin-warn-status");
  try {
    await invoke("clear_game_requires_admin", { executable: exe });
    if (status) status.innerHTML = `<span class="ok">✓ Cleared — Launch should work normally now.</span>`;
    void renderAdminWarning(); // re-detect; the warning clears itself
  } catch (err) {
    if (status) status.innerHTML = `<span class="warn">Couldn't change it: ${escape(String(err))}</span>`;
    console.error("clear_game_requires_admin failed:", err);
  }
}

async function doLaunchElevated(): Promise<void> {
  const ok = await confirm(
    'Run the game as administrator? Not recommended — it can interfere with overlays, and the launcher’s CPU priority/affinity won’t apply. The cleaner option is "Fix it", which clears the flag so the game launches normally.',
    { title: "Launch as administrator", kind: "warning" },
  );
  if (!ok) return;
  const di = state.installs[state.selInstall];
  const profile =
    di.profiles.find((p) => p.label === state.profileLabel) ?? di.profiles[selectedProfileIndex(di)];
  const status = document.getElementById("admin-warn-status");
  let auth: Ut4AuthLaunch;
  try {
    // No root argument on purpose: an elevated launch is created by the UAC
    // broker, which does not inherit this process's environment, so the login
    // code has to travel on the command line here even though that logs it.
    auth = await ut4AuthArgs();
  } catch (err) {
    if (handleReloginError(err, null)) return;
    if (status) status.innerHTML = `<span class="warn">UT4 login failed: ${escape(String(err))}</span>`;
    return;
  }
  try {
    await invoke("launch_game_elevated", {
      // Pass the install root (an id the backend re-validates), not the exe path
      // — the backend resolves the executable itself (defense-in-depth).
      root: di.install.root,
      args: [...profile.args, ...auth.args],
      windowAction: state.launchWindowAction,
    });
    if (status) status.innerHTML = `<span class="ok">Launching as administrator…</span>`;
  } catch (err) {
    if (status) status.innerHTML = `<span class="warn">Elevated launch failed: ${escape(String(err))}</span>`;
    console.error("launch_game_elevated failed:", err);
  }
}

// ---- UT4 account login ----------------------------------------------------

// Account block for the Launch card: a sign-in form when signed out, or the
// signed-in identity + a log-out link.
function ut4AccountHtml(): string {
  const a = state.ut4;
  if (a?.logged_in) {
    return `<div class="ut4-account" style="margin-top:14px">
      <span class="ok">UT4: signed in as <strong>${escape(a.display_name ?? a.username ?? "player")}</strong></span>
      &nbsp;·&nbsp;<button id="ut4-logout" type="button" class="link-btn">log out</button>
    </div>`;
  }
  return `<div class="ut4-account" style="margin-top:14px">
    <p class="src"><strong>Optional</strong> — sign in so the launcher logs you in directly (skips the in-game login window, avoids the account picker, and enables one-click PUG connect).
      Use the same username &amp; password as your account at
      <button id="ut4-site-link" type="button" class="link-btn">ut4.timiimit.com</button> — the same login the game itself uses.
      <strong>Just your username and password — you do not need any auth code from the website.</strong></p>
    <div class="controls">
      <input id="ut4-user" type="text" placeholder="UT4 username" autocomplete="username" spellcheck="false" />
      <input id="ut4-pass" type="password" placeholder="password" autocomplete="current-password" />
      <button id="ut4-login" type="button">Sign in</button>
    </div>
    <p class="src ut4-secure-note">🔒 Your password goes straight to ut4.timiimit.com over HTTPS and is
      <strong>never saved</strong>. The launcher keeps only a revocable session token, in Windows
      Credential Manager — not in any file, and not synced anywhere.
      <button id="ut4-src-link" type="button" class="link-btn">It's open source — read exactly what it does.</button></p>
    <div id="ut4-auth-status" class="launch-status"></div>
  </div>`;
}

function wireUt4Account() {
  document.getElementById("ut4-login")?.addEventListener("click", () => void ut4Login());
  document.getElementById("ut4-logout")?.addEventListener("click", () => void ut4Logout());
  document
    .getElementById("ut4-site-link")
    ?.addEventListener("click", () => openExternal("https://ut4.timiimit.com/"));
  document
    .getElementById("ut4-src-link")
    ?.addEventListener("click", () =>
      openExternal("https://github.com/jmortley/netcodeplus-launcher/blob/main/src-tauri/src/auth.rs"),
    );
}

async function ut4Login() {
  const userEl = document.getElementById("ut4-user") as HTMLInputElement | null;
  const passEl = document.getElementById("ut4-pass") as HTMLInputElement | null;
  const status = document.getElementById("ut4-auth-status");
  const username = userEl?.value.trim() ?? "";
  const password = passEl?.value ?? "";
  if (passEl) passEl.value = ""; // never retain the password in the DOM
  if (!username || !password) {
    if (status) status.innerHTML = `<span class="warn">Enter your UT4 username and password.</span>`;
    return;
  }
  if (status) status.textContent = "Signing in…";
  try {
    state.ut4 = await invoke<Ut4Auth>("ut4_login", { username, password });
    renderHome();
    renderTopbarAuth();
    // Guard against an accidental login to the wrong account (e.g. the wrong
    // saved entry from a password manager): if this replaced a different account
    // that was signed in, confirm the switch before it can reach a game launch.
    if (state.ut4.switched_from) {
      const now = state.ut4.display_name ?? username;
      const prev = state.ut4.switched_from;
      const keep = await confirm(
        `Now signed in as ${now} (was ${prev}). Keep this account? "Sign out" reverts it so you can sign in as ${prev} again.`,
        { title: "Switched account", kind: "warning", okLabel: "Keep", cancelLabel: "Sign out" },
      );
      if (!keep) await ut4Logout();
    }
  } catch (err) {
    if (status) status.innerHTML = `<span class="warn">${escape(String(err))}</span>`;
  }
}

async function ut4Logout() {
  try {
    await invoke("ut4_logout");
  } catch (err) {
    console.error("ut4_logout failed:", err);
  }
  state.ut4 = { logged_in: false, username: null, display_name: null, account_id: null };
  renderHome();
  renderTopbarAuth();
}

// The launch args + extra environment that log the game in via the launcher's
// session; both empty when signed out (the game then shows its own login
// window). Throws "RELOGIN_REQUIRED" when the stored session has expired.
type Ut4AuthLaunch = { args: string[]; env: Record<string, string> };

// `root` is the install the game will be launched from — it decides whether the
// exchange code can travel in the environment (see below). Omit it to force the
// command-line form, which is what the elevated launch path must do: a UAC
// launch goes through ShellExecute and does NOT inherit our environment.
async function ut4AuthArgs(root?: string): Promise<Ut4AuthLaunch> {
  if (!state.ut4?.logged_in) return { args: [], env: {} };
  const a = await invoke<{ username: string; exchange_code: string; account_id: string }>(
    "ut4_prepare_launch",
  );
  const args = [`-AUTH_LOGIN=${a.username}`, `-AUTH_TYPE=exchangecode`];
  const env: Record<string, string> = {};

  // The exchange code is a live login credential, and UE4 writes the whole
  // command line into Saved/Logs/UnrealTournament.log and into every crash
  // report — files players routinely post in public channels. Plugin builds that
  // read the code from the environment get it that way and it never lands on
  // disk. Older builds have no such pickup, so they still need the argument or
  // the player simply fails to log in; ask the installed binary which it is.
  let viaEnv = false;
  if (root) {
    try {
      viaEnv = await invoke<boolean>("ut4_env_auth_supported", { root });
    } catch (err) {
      console.error("ut4_env_auth_supported failed:", err);
      viaEnv = false;
    }
  }
  if (viaEnv) {
    env.NCP_AUTH_PASSWORD = a.exchange_code;
  } else {
    args.push(`-AUTH_PASSWORD=${a.exchange_code}`);
  }
  // -epicuserid names the active account so the game boots into it instead of
  // showing the account picker. Only sent when captured (a session from before
  // account-id capture leaves it empty — the user re-logs in once to populate
  // it). account_id is a hex GUID, so no launch-arg quoting concern.
  // (-epicusername / -epiclocale deliberately omitted: a display name can
  // contain spaces our argv launch path would truncate, and both are cosmetic;
  // the account picker is driven by the id.)
  if (a.account_id) {
    args.push(`-epicuserid=${a.account_id}`);
  }
  return { args, env };
}

// On an expired session, flip to signed-out and re-render the login form. If a
// status element is given (e.g. the PUG controls on another tab) the message
// goes there; otherwise into the freshly-rendered account form. Returns true
// when it handled a relogin, so the caller aborts the launch.
function handleReloginError(err: unknown, statusEl: HTMLElement | null): boolean {
  if (!String(err).includes("RELOGIN_REQUIRED")) return false;
  state.ut4 = { logged_in: false, username: null, display_name: null, account_id: null };
  renderHome();
  const msg = "Your UT4 session expired — sign in again on the Home tab, then try again.";
  if (statusEl) {
    statusEl.innerHTML = `<span class="warn">${escape(msg)}</span>`;
  } else {
    const s = document.getElementById("ut4-auth-status");
    if (s) s.innerHTML = `<span class="warn">${escape(msg)}</span>`;
  }
  return true;
}

// ---- ut4stats player panel ------------------------------------------------

function renderLinkUI(message = "") {
  statsPanel.innerHTML = `
    ${message ? `<div class="warn">${message}</div>` : ""}
    <div>Link your <strong>ut4stats.com</strong> profile to show your stats here.</div>
    <div class="controls">
      <label>Find your player
        <input id="ut4-search" type="text" placeholder="type your in-game name…" spellcheck="false" autocomplete="off" />
      </label>
      <div id="ut4-results"></div>
    </div>`;
  const input = document.getElementById("ut4-search") as HTMLInputElement;
  let timer: number | undefined;
  input.addEventListener("input", () => {
    window.clearTimeout(timer);
    timer = window.setTimeout(() => void doSearch(input.value), 250);
  });
  input.focus();
}

async function doSearch(query: string) {
  const results = document.getElementById("ut4-results");
  if (!results) return;
  if (query.trim().length < 2) {
    results.innerHTML = "";
    return;
  }
  try {
    const raw = await invoke<string>("ut4stats_search", { query });
    const list = JSON.parse(raw) as PlayerSearchResult[];
    if (list.length === 0) {
      results.innerHTML = `<div class="src">No players match “${escape(query)}”.</div>`;
      return;
    }
    results.innerHTML = list
      .map(
        (p) =>
          `<button type="button" class="ut4-pick" data-id="${escape(p.id)}" data-name="${escape(p.name)}">${escape(
            p.name,
          )}</button>`,
      )
      .join("");
    results.querySelectorAll<HTMLButtonElement>(".ut4-pick").forEach((btn) => {
      btn.addEventListener("click", () => void linkPlayer(btn.dataset.id!, btn.dataset.name!));
    });
  } catch (err) {
    results.innerHTML = `<div class="warn">Search failed: ${escape(String(err))}</div>`;
    console.error("ut4stats_search failed:", err);
  }
}

async function linkPlayer(id: string, name: string) {
  state.linkedId = id;
  state.linkedName = name;
  try {
    await invoke("save_ut4stats_link", { playerid: id, playername: name });
  } catch (err) {
    console.error("save_ut4stats_link failed:", err);
  }
  renderPug();
  await fetchSummary(id);
}

async function unlink() {
  state.linkedId = null;
  state.linkedName = null;
  try {
    await invoke("save_ut4stats_link", { playerid: null, playername: null });
  } catch (err) {
    console.error("save_ut4stats_link (unlink) failed:", err);
  }
  renderLinkUI();
  renderPug();
}

async function fetchSummary(id: string) {
  statsPanel.innerHTML = `<div class="src">Loading ${escape(state.linkedName ?? "stats")}…</div>`;
  try {
    const raw = await invoke<string>("ut4stats_summary", { playerid: id });
    lastSummary = JSON.parse(raw) as PlayerSummary;
    renderSummary(lastSummary);
  } catch (err) {
    lastSummary = null;
    renderSummary(null, String(err));
  }
  renderDashStats();
}

function renderStats() {
  if (state.linkedId) {
    void fetchSummary(state.linkedId);
  } else {
    renderLinkUI();
  }
}

function renderSummary(s: PlayerSummary | null, error?: string) {
  if (!s) {
    statsPanel.innerHTML = `
      <div class="warn">Couldn't load stats for ${escape(state.linkedName ?? "this player")}.</div>
      <div class="src">${escape(error ?? "")}</div>
      <button id="ut4-change" type="button">Change profile</button>`;
    document.getElementById("ut4-change")?.addEventListener("click", () => void unlink());
    return;
  }

  const ratings = s.ratings.length
    ? s.ratings.map((r) => `<span class="rating"><b>${escape(r.mode)}</b> ${r.rating} <span class="src">(${r.games}g)</span></span>`).join("")
    : `<span class="src">No rated matches yet.</span>`;

  const acc = Object.entries(s.accuracy)
    .filter(([, v]) => v !== null)
    .map(([w, v]) => `${escape(w)} ${v}%`)
    .join(" · ");

  const recent = s.recent.length
    ? `<ul class="recent">${s.recent
        .map((m) => {
          const cls = m.result === "win" ? "ok" : m.result === "loss" ? "warn" : "src";
          const sign = m.delta > 0 ? "+" : "";
          const mapText = escape(m.map || "?");
          // Correlated matches link to their ut4stats.com summary page; the
          // rest render as plain text. The URL is same-origin to ut4stats and
          // opened via the opener plugin (open_external requires https://).
          const mapCell =
            m.match_id != null
              ? `<button class="match-link link-btn" type="button" data-url="${escape(
                  `https://ut4stats.com/match_summary/${m.match_id}/`,
                )}" title="View this match on ut4stats.com">${mapText} ↗</button>`
              : mapText;
          return `<li><span class="${cls}">${escape(m.result)}</span> ${sign}${m.delta} · ${escape(
            m.mode,
          )} · ${mapCell} <span class="src">${escape(m.played_at)}</span></li>`;
        })
        .join("")}</ul>`
    : "";

  // Default the Trends selector to the player's most-played rated mode (falls
  // back to CTF); once the user picks a mode it sticks for the session.
  if (!state.trendMode) {
    const top = [...s.ratings].sort((a, b) => b.games - a.games)[0];
    state.trendMode = (top && NC_MODE_TO_KEY[top.mode]) || "ctf";
  }
  const modeOpts = TREND_MODES.map(
    (m) => `<option value="${m.key}"${m.key === state.trendMode ? " selected" : ""}>${escape(m.label)}</option>`,
  ).join("");

  statsPanel.innerHTML = `
    <div class="stat-head">
      <strong>${escape(s.playername)}</strong>
      <span class="src">${escape(s.flag || "")}</span>
      <button id="ut4-refresh" type="button" class="link-btn">refresh</button>
      <button id="ut4-change" type="button" class="link-btn">change</button>
    </div>
    <div class="ratings">${ratings}</div>
    <dl>
      <dt>Record</dt><dd>K/D ${s.totals.kd} · ${s.totals.kills}/${s.totals.deaths} · ${s.totals.games} games</dd>
      ${acc ? `<dt>Accuracy</dt><dd>${acc}</dd>` : ""}
    </dl>
    ${recent ? `<div class="src">Recent rated matches</div>${recent}` : ""}
    <div class="trends">
      <div class="trends-head"><strong>Trends</strong>
        <select id="trend-mode" class="trend-mode">${modeOpts}</select>
      </div>
      <div id="trend-body"></div>
    </div>
  `;
  document.getElementById("ut4-change")?.addEventListener("click", () => void unlink());
  // Re-pull the summary on demand: it's otherwise only fetched at startup, so
  // ratings / recent matches would stay stale until the launcher is reopened.
  document.getElementById("ut4-refresh")?.addEventListener("click", () => {
    if (state.linkedId) void fetchSummary(state.linkedId);
  });
  statsPanel.querySelectorAll<HTMLButtonElement>(".match-link").forEach((btn) => {
    btn.addEventListener("click", () => {
      const url = btn.dataset.url;
      if (url) void openExternal(url);
    });
  });
  const trendSel = document.getElementById("trend-mode") as HTMLSelectElement | null;
  trendSel?.addEventListener("change", () => {
    state.trendMode = trendSel.value;
    void renderTrends(state.trendMode);
  });
  void renderTrends(state.trendMode);
}

// Render the per-mode Trends block (sniper/LG accuracy sparklines, form +
// streak, and the rating curve on ELO modes) into #trend-body. Pulls from the
// signed-manifest-free public `ut4stats_trends` endpoint; a failure just shows
// a quiet note (never blocks the Stats panel).
async function renderTrends(mode: string): Promise<void> {
  const body = document.getElementById("trend-body");
  if (!body) return;
  if (!state.linkedId) {
    body.innerHTML = "";
    return;
  }
  body.innerHTML = `<div class="src">Loading trends…</div>`;
  let t: PlayerTrends;
  try {
    t = JSON.parse(await invoke<string>("ut4stats_trends", { playerid: state.linkedId, mode }));
  } catch (err) {
    body.innerHTML = `<div class="src">Couldn't load trends: ${escape(String(err))}</div>`;
    console.error("ut4stats_trends failed:", err);
    return;
  }
  const acc = t.accuracy ?? [];
  // Headline = mean hit% across the window's tracked games (each already past the
  // server-side ≥10-shot floor), so it reads as "your accuracy over these matches"
  // rather than just the latest game. The sparkline still shows the per-game trend.
  const meanVal = (vals: (number | null)[]): number | null => {
    const xs = vals.filter((v): v is number => v != null);
    if (xs.length === 0) return null;
    return Math.round((xs.reduce((a, b) => a + b, 0) / xs.length) * 10) / 10;
  };
  // One accuracy row per weapon that actually has data in this mode (sniper/LG
  // in regular modes, IG in instagib/iCTF); empty weapons are hidden.
  const accRow = (label: string, vals: (number | null)[], color: string): string => {
    const lv = meanVal(vals);
    if (lv == null) return "";
    return `<div class="trend-row"><span class="trend-label">${label}</span>${sparkline(vals, color)}<span class="trend-val">${lv}%</span></div>`;
  };

  const f = t.form;
  const pips = (f?.results ?? [])
    .map((r) => `<span class="pip pip-${r === "W" ? "w" : r === "L" ? "l" : "n"}">${escape(r)}</span>`)
    .join("");
  const streakTxt = f && f.streak
    ? ` · ${f.streak > 0 ? `${f.streak}-win streak` : `${-f.streak}-loss streak`}`
    : "";
  const formLine =
    f && f.games
      ? `<div class="form-line"><b class="ok">${f.wins}W</b>–<b class="warn">${f.losses}L</b>${escape(
          streakTxt,
        )} <span class="pips">${pips}</span></div>`
      : `<div class="src">No recent matches in this mode.</div>`;

  const accBlock =
    accRow("Sniper", acc.map((a) => a.sniper), "#f5a623") +
      accRow("Lightning", acc.map((a) => a.lg), "#4f8bff") +
      accRow("IG", acc.map((a) => a.ig), "#c084fc") ||
    `<div class="src">No sniper / lightning / instagib shots recorded in this mode.</div>`;

  const r = t.rating ?? [];
  const ratingBlock = t.has_elo
    ? r.length > 1
      ? `<div class="trend-row"><span class="trend-label">Rating</span>${sparkline(
          r.map((x) => x.rating),
          "#4ade80",
        )}<span class="trend-val">${r[r.length - 1].rating}</span></div>`
      : `<div class="trend-row"><span class="trend-label">Rating</span><span class="src">not enough rated games</span></div>`
    : "";

  body.innerHTML = `${formLine}${accBlock}${ratingBlock}`;
}

// Minimal inline SVG sparkline over the non-null values (evenly spaced),
// auto-scaled to its own min/max. Fewer than two points renders a dash.
function sparkline(vals: (number | null)[], color: string): string {
  const pts = vals.filter((v): v is number => v != null);
  if (pts.length < 2) return `<span class="spark-empty">—</span>`;
  const w = 130;
  const h = 26;
  const pad = 3;
  const min = Math.min(...pts);
  const max = Math.max(...pts);
  const range = max - min || 1;
  const step = (w - pad * 2) / (pts.length - 1);
  const d = pts
    .map((v, i) => {
      const x = pad + i * step;
      const y = pad + (h - pad * 2) * (1 - (v - min) / range);
      return `${i === 0 ? "M" : "L"}${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");
  return `<svg class="spark" viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" aria-hidden="true"><path d="${d}" fill="none" stroke="${color}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
}

// ---- startup --------------------------------------------------------------

function applyPrefs(prefs: LauncherState) {
  state.priority =
    prefs.launch_priority === "high"
      ? "high"
      : prefs.launch_priority === "real_time"
        ? "real_time"
        : "normal";
  state.affinityHex = prefs.affinity_mask_hex ?? "";
  state.launchWindowAction =
    prefs.launch_window_action === "close" || prefs.launch_window_action === "none"
      ? prefs.launch_window_action
      : "minimize";
  state.profileLabel = prefs.launch_profile_label;
  state.linkedId = prefs.ut4stats_playerid;
  state.linkedName = prefs.ut4stats_playername;
  state.launcherToken = prefs.launcher_token;
  state.utpugsToken = prefs.utpugs_launcher_token;
  state.unrealpugsToken = prefs.unrealpugs_launcher_token;
  state.discordPresence = prefs.discord_presence_enabled ?? true;
  linuxLaunch = prefs.linux_launch ?? null;
  linuxGpuAccel = prefs.linux_gpu_accel ?? false;
  linuxGamingMode = prefs.linux_gaming_mode ?? false;
  if (prefs.install_path) {
    const i = state.installs.findIndex((d) => d.install.root === prefs.install_path);
    if (i >= 0) state.selInstall = i;
  }
}

async function showVersion() {
  try {
    const v = `v${await invoke<string>("launcher_version")}`;
    versionLabel.textContent = v;
    const about = document.getElementById("about-version");
    if (about) about.textContent = v;
  } catch (err) {
    versionLabel.textContent = "(version unknown)";
    console.error("launcher_version failed:", err);
  }
}

interface PlatformInfo {
  os: "windows" | "linux" | "other";
}
// Host OS (from the platform_info command), fetched once in loadAll's barrier
// before the render fan-out — drives hiding Windows-only surfaces on Linux (game
// installer, .NET gate, admin/elevation, stray scan). Defaults to "windows" so a
// (near-impossible) fetch failure preserves the desktop UI.
let platformOs: PlatformInfo["os"] = "windows";

// (Linux) Wine/Proton runners for the Settings dropdown, and the current saved
// launch override (null = auto-detect via Lutris). Populated in loadAll on Linux.
let wineRunners: WineRunner[] = [];
let linuxLaunch: LinuxLaunch | null = null;
// Linux-only: mirrors LauncherState.linux_gpu_accel. false = the launcher forces
// WEBKIT_DISABLE_DMABUF_RENDERER=1 (safe default); true = user opted into the GPU
// path. Applied at startup, so a change needs a launcher restart to take effect.
let linuxGpuAccel = false;
// Linux-only: mirrors LauncherState.linux_gaming_mode — the fullscreen/dock
// watchdog toggle. Read at game-launch time, so no restart is needed.
let linuxGamingMode = false;

async function loadAll() {
  try {
    const [installs, presets, prefs, ut4, utpugsConfigured, unrealpugsConfigured, platformInfo, onbView] =
      await Promise.all([
      invoke<DetectedInstall[]>("detect_installs"),
      invoke<AffinityPreset[]>("affinity_presets"),
      invoke<LauncherState>("load_state"),
      // A credential-store hiccup must not block startup — treat as signed out.
      invoke<Ut4Auth>("ut4_auth_status").catch(() => null),
      // Whether this build has the UTPugs base URL wired in (false hides it).
      invoke<boolean>("utpugs_configured").catch(() => false),
      // Whether this build has the UnrealPUGs base URL wired in (false hides it).
      invoke<boolean>("unrealpugs_configured").catch(() => false),
      // Host OS → hide Windows-only surfaces on Linux (default windows on failure).
      invoke<PlatformInfo>("platform_info").catch(() => ({ os: "windows" as const })),
      // Onboarding classification. Resolved in this same load barrier so it
      // persists the one-time first-run decision BEFORE the render fan-out below
      // triggers launcher_update_housekeeping — which would otherwise create the
      // state file and mask a genuine fresh install. A failure just skips surfaces.
      invoke<OnboardingView>("onboarding_status").catch(() => null),
    ]);
    state.installs = installs;
    state.presets = presets;
    state.selInstall = 0;
    state.ut4 = ut4;
    state.utpugsConfigured = utpugsConfigured;
    state.unrealpugsConfigured = unrealpugsConfigured;
    platformOs = platformInfo.os;
    // Editor-install management is Windows-only — reveal its (hidden-by-default)
    // nav tab there. It stays hidden on Linux, where editor trees aren't a thing.
    if (platformOs === "windows") {
      const navEditor = document.getElementById("nav-editor");
      if (navEditor) navEditor.style.display = "";
    }
    // (Linux) discover installed Wine/Proton runners for the Settings dropdown.
    if (platformOs === "linux") {
      wineRunners = await invoke<WineRunner[]>("list_wine_runners").catch(() => []);
    }
    applyPrefs(prefs);
    // A manually-picked install that auto-detection doesn't find would otherwise
    // be lost on restart (detect_installs overwrote state.installs, so the saved
    // path isn't in it and applyPrefs couldn't re-select it). Re-validate the
    // saved path and add it back if it's still a UT4 install that wasn't detected.
    if (prefs.install_path && !state.installs.some((d) => d.install.root === prefs.install_path)) {
      try {
        const re = await invoke<DetectedInstall | null>("check_install", {
          path: prefs.install_path,
        });
        if (re) {
          state.installs.push(re);
          state.selInstall = state.installs.length - 1;
        }
      } catch (err) {
        console.error("re-validating the saved install failed:", err);
      }
    }
    void autoFixMasterServer();
    render();
    renderTopbarAuth();
    renderStats();
    renderPug();
    renderUtpugs();
    renderUnrealpugs();
    // Arm Discord presence from the persisted opt-in, then push initial state.
    if (state.discordPresence) {
      void invoke("set_discord_presence_enabled", { enabled: true }).catch(() => {});
      updateDiscordPresence();
    }
    // ncp://connect deep links (cold-start URL + live listener).
    void wireDeepLinks();
    void renderConfig();
    void renderModIni();
    void renderAddons();
    void renderLauncherUpdate();
    void renderLauncherCleanup();
    void renderNews();
    void renderGameInstall();
    void loadStatusData();
    renderCommunityLinks();
    renderCommunityVideos();
    // Feature-discovery surfaces (first-run walkthrough / what's-new / catch-up),
    // shown over the now-rendered UI. Skipped silently if classification failed.
    if (onbView) void renderOnboarding(onbView);
    // Tokenless HOME nudges: live-PUG banner + "queue filling" (no token needed).
    void pollLivePugs();
    void pollQueues();
    startLivePolling();
    if (!visibilityWired) {
      visibilityWired = true;
      document.addEventListener("visibilitychange", onLauncherVisible);
    }
    if (state.launcherToken) {
      void pollPugStatus();
      startPugPolling();
    }
    if (state.utpugsConfigured && state.utpugsToken) {
      void pollUtpugsStatus();
      startUtpugsPolling();
    }
    if (state.unrealpugsConfigured && state.unrealpugsToken) {
      void pollUnrealpugsStatus();
      startUnrealpugsPolling();
    }
  } catch (err) {
    homeHero.innerHTML = `<div class="warn">Detection failed: ${escape(String(err))}</div>`;
    console.error("startup load failed:", err);
  }
}

async function pickDir() {
  let picked: string | string[] | null;
  try {
    picked = await open({ directory: true, multiple: false, title: "Choose your UT4 install folder" });
  } catch (err) {
    console.error("dialog open failed:", err);
    return;
  }
  if (!picked) return;
  const dirPath = Array.isArray(picked) ? picked[0] : picked;

  try {
    const result = await invoke<DetectedInstall | null>("check_install", { path: dirPath });
    if (!result) {
      advancedPanel.innerHTML = `
        <div class="warn">
          <code>${escape(dirPath)}</code> does not look like a UT4 install.
          Expected <code>Engine/Binaries/Win64/UE4-Win64-Shipping.exe</code> and
          <code>UnrealTournament/Content/Paks/</code> at or above this path.
        </div>`;
      return;
    }
    state.installs = [result];
    state.selInstall = 0;
    state.profileLabel = null;
    render();
    persist();
    // Refresh the manifest-backed status card against the newly-picked install so
    // the HOME "NetcodePlus & updates" line matches the hero/Settings immediately
    // (it reads cached statusCache, populated at startup before this pick).
    void loadStatusData();
  } catch (err) {
    advancedPanel.innerHTML = `<div class="warn">Validation failed: ${escape(String(err))}</div>`;
    console.error("check_install failed:", err);
  }
}

// ---- community / discord --------------------------------------------------

const UTPUGS_DISCORD = "https://discord.gg/utpugs";
const ICTF_DISCORD = "https://discord.gg/EcUMZjFY3q";

// Community Discords surfaced on the Home dashboard card and the Community tab.
// Add an entry { name, url } to list a new community in both places.
interface Community {
  name: string;
  url: string;
}
const COMMUNITIES: Community[] = [
  { name: "UTPugs Discord", url: UTPUGS_DISCORD },
  { name: "Instagib Nation", url: ICTF_DISCORD },
  { name: "Unreal Pugs (EU)", url: "https://discord.gg/unrealpugs" },
  { name: "Phoenix Germany", url: "https://discord.gg/qCSm4YjCeU" },
  { name: "Unreal Carnage", url: "https://discord.gg/UpGhtAa" },
  { name: "UT4 LATAM", url: "https://discord.gg/Ufn3KChExR" },
];

// Fill the Community tab's Discord buttons from COMMUNITIES (the Home card does
// the same inline). Buttons carry data-extlink → the https-gated opener via the
// delegated click handler, so no per-button wiring is needed.
function renderCommunityLinks(): void {
  const el = document.getElementById("community-links");
  if (!el) return;
  el.innerHTML = COMMUNITIES.map(
    (c) => `<button class="btn btn-discord" data-extlink="${escape(c.url)}" type="button">${escape(c.name)}</button>`,
  ).join("");
}

// Community video highlights (tutorials / tributes / frag movies). Thumbnails are
// bundled locally (public/videos/<id>.jpg) so the shipped launcher makes NO runtime
// request to Google — clicking a card opens the video externally via open_external.
interface Video {
  id: string;
  title: string;
  by: string;
  tag: string;
}
const VIDEOS: Video[] = [
  { id: "DStf9pEWfaI", title: "UT4 Movement Tutorial", by: "HateBreeD", tag: "How to play" },
  { id: "EHoZMQsHAW8", title: "UT4 Legend Demon1_", by: "Demon1", tag: "Tribute" },
  { id: "DqxerotwzfA", title: "phantaci — Your Majesty", by: "phantasi ut", tag: "Frags" },
  { id: "1hlAqRPBnFM", title: "ZNATCH — 4K Fragmovie", by: "Flikswich", tag: "Frags" },
];

// Fill the Community tab's video grid. Cards carry data-extlink → the delegated
// https-gated opener (same path as the Discord buttons), so no per-card wiring.
function renderCommunityVideos(): void {
  const el = document.getElementById("community-videos");
  if (!el) return;
  el.innerHTML = VIDEOS.map(
    (v) =>
      `<button class="video-card" type="button" data-extlink="https://www.youtube.com/watch?v=${escape(v.id)}" title="Open on YouTube">` +
      `<span class="video-thumb"><img src="/videos/${escape(v.id)}.jpg" alt="" loading="lazy" /><span class="video-tag">${escape(v.tag)}</span><span class="video-play">▶</span></span>` +
      `<span class="video-meta"><span class="video-title">${escape(v.title)}</span><span class="video-by">${escape(v.by)}</span></span>` +
      `</button>`,
  ).join("");
}

function openExternal(url: string) {
  void invoke("open_external", { url }).catch((err) => console.error("open_external failed:", err));
}

// ---- in-launcher PUG join (UT4IGBot) --------------------------------------

const pugControls = document.getElementById("pug-controls")!;

// Set when a bot call reports the launcher token is missing / unrecognized /
// revoked (HTTP 401) — drives a one-line warning above the re-link prompt so the
// user knows why they're back there. Cleared when a token is saved.
let pugTokenRejected = false;

// Whether `err` from a bot PUG call is a token problem (vs. a transient network
// error) — keyed on the 401 and the "/launchertoken" guidance that both the
// bot-mapped 401 and the empty-token errors carry.
function isPugTokenError(err: unknown): boolean {
  const m = String(err).toLowerCase();
  return m.includes("launchertoken") || m.includes("no launcher token") || /\b401\b/.test(m);
}

// A token problem from any bot call: drop the dead token (which stops polling
// and re-renders the link-token prompt) and flag why, instead of leaving a token
// that keeps erroring or surfacing a raw HTTP 401.
function handlePugTokenError(): void {
  pugTokenRejected = true;
  void saveLauncherToken(null);
}

// Throttle the spammy join/leave toggles so a user can't flood the bot (and the
// Discord channel it posts join/leave notices to) by rapidly clicking Join/Leave.
// Read-only actions (listpug / Queue status) aren't gated. State-based, not a DOM
// disable, so it survives the 5 s poll re-render. Keyed per community. The bot also
// rate-limits server-side; this just keeps the launcher a good citizen.
const PUG_TOGGLE_COOLDOWN_MS = 4000;
const pugToggleGate: Record<string, { inFlight: boolean; lastAt: number }> = {};
function pugToggleAllowed(key: string, action: string, status: HTMLElement | null): boolean {
  if (action !== "joinpug" && action !== "leavepug") return true; // only gate toggles
  const g = (pugToggleGate[key] ??= { inFlight: false, lastAt: 0 });
  if (g.inFlight) return false; // a request is already in flight — ignore the click
  const wait = PUG_TOGGLE_COOLDOWN_MS - (Date.now() - g.lastAt);
  if (wait > 0) {
    if (status) status.textContent = `Easy — wait ${Math.ceil(wait / 1000)}s before changing the queue again.`;
    return false;
  }
  return true;
}

// NetcodePlus state for the install the player will launch from — drives both the
// pre-join gate and the `build` we report to the bot.
function selectedNcp(): { build: number | null; outdated: boolean; available: number | null } {
  const di = state.installs[state.selInstall];
  const inst = di ? statusCache.plugin?.installs.find((i) => i.root === di.install.root) : undefined;
  return {
    build: inst?.installed_version ?? null,
    // "update" = behind the manifest, "install" = missing. Either one means the
    // NetcodePlus server version gate would kick this client, so it can't PUG.
    outdated: inst?.action === "update" || inst?.action === "install",
    available: statusCache.plugin?.available_version ?? null,
  };
}

// Pre-join enforcement: a stale/missing NetcodePlus can't queue — otherwise the
// player joins, the server version gate kicks them ~10s in, and the PUG is a man
// short. A Join on an outdated install kicks off the one-click update instead; the
// player Joins again once it's current. Returns true when the join is BLOCKED.
function ncpBlocksJoin(status: HTMLElement | null): boolean {
  const ncp = selectedNcp();
  if (!ncp.outdated) return false;
  const want = ncp.available ? ` (build ${ncp.available})` : "";
  if (status) {
    status.innerHTML = `<span class="warn">NetcodePlus is out of date${escape(want)} — updating now; Join again once it finishes. Outdated clients get kicked from PUG servers.</span>`;
  }
  void doInstallPlugin(); // run the update; the re-Join proceeds when it's current
  return true;
}

async function pug(action: "joinpug" | "leavepug" | "listpug") {
  // Defensive: never POST an empty token. renderPug already gates the buttons on
  // a token, but this stops a stale render from firing a guaranteed 401.
  if (!state.launcherToken?.trim()) {
    renderPug();
    return;
  }
  const status = document.getElementById("pug-status");
  if (!pugToggleAllowed("ictf", action, status)) return;
  // Force-updated-to-PUG: a stale NetcodePlus can't queue — update first.
  if (action === "joinpug" && ncpBlocksJoin(status)) return;
  const gate = pugToggleGate["ictf"];
  const toggle = action === "joinpug" || action === "leavepug";
  if (toggle) {
    gate.inFlight = true;
    gate.lastAt = Date.now();
  }
  if (status) status.textContent = action === "listpug" ? "Checking queue…" : "Working…";
  try {
    const raw = await invoke<string>("pug_action", {
      action,
      mode: state.igMode,
      token: state.launcherToken ?? "",
      build: selectedNcp().build,
    });
    let msg = raw;
    try {
      msg = (JSON.parse(raw) as { message?: string }).message ?? raw;
    } catch {
      /* response wasn't JSON; show it raw */
    }
    if (status) status.textContent = msg;
  } catch (err) {
    console.error("pug_action failed:", err);
    if (isPugTokenError(err)) {
      handlePugTokenError();
    } else if (status) {
      status.innerHTML = `<span class="warn">${escape(String(err))}</span>`;
    }
  } finally {
    if (toggle) gate.inFlight = false;
  }
}

async function saveLauncherToken(token: string | null) {
  state.launcherToken = token;
  if (token) pugTokenRejected = false;
  if (!token) state.pugStatus = null;
  try {
    await invoke("save_launcher_token", { token });
  } catch (err) {
    console.error("save_launcher_token failed:", err);
  }
  renderPug();
  renderHomeReadycheck(); // clears the HOME banner when the token (and status) is dropped
  updateDiscordPresence();
  if (token) {
    void pollPugStatus();
    startPugPolling();
  } else {
    stopPugPolling();
  }
}

// ---- Discord Rich Presence (opt-in) ---------------------------------------
// Translate the launcher's PUG state into a Discord activity push, across BOTH
// communities the user may be linked to (iCTF + UTPugs). The backend no-ops when
// the toggle is off or Discord isn't running; deduped here so the 5 s polls don't
// spam the IPC. See src-tauri/src/presence.rs.
let lastPresenceKey = "";

// Rank PUG states so the most "advanced" one wins when the user is queued in more
// than one community at once: a live/starting game beats a different community's
// queue, which beats idle.
function pugStateRank(s: string | undefined): number {
  return s === "live" ? 4 : s === "starting" ? 3 : s === "readycheck" ? 2 : s === "queued" ? 1 : 0;
}

function updateDiscordPresence(): void {
  if (!state.discordPresence) return;

  // Each community the user is linked to is a candidate; pick the most-advanced
  // state so the profile reflects whichever PUG actually matters right now.
  const candidates: { community: string; mode: string; st: PugStatus }[] = [];
  if (state.launcherToken && state.pugStatus) {
    // Instagib Nation is multi-mode now — reflect the actual queue (the bot
    // reports st.mode when queued; else the selected mode), not a hardcoded iCTF.
    const igMode = (state.pugStatus.mode ?? state.igMode).toLowerCase();
    candidates.push({ community: "Instagib Nation", mode: igMode, st: state.pugStatus });
  }
  if (state.utpugsConfigured && state.utpugsToken && state.utpugsStatus) {
    // Reflect the mode the bot says we're actually queued in (st.mode), not the
    // dropdown — so presence is right even if the user is browsing another tab.
    const utMode =
      state.utpugsStatus.state === "queued" && state.utpugsStatus.mode
        ? normUtpugsMode(state.utpugsStatus.mode)
        : state.utpugsMode;
    candidates.push({ community: "UTPugs", mode: utMode, st: state.utpugsStatus });
  }
  if (state.unrealpugsConfigured && state.unrealpugsToken && state.unrealpugsStatus) {
    // BLITZ-only for now; the bot doesn't report a mode on status, so it's fixed.
    candidates.push({ community: "UnrealPUGs", mode: "blitz", st: state.unrealpugsStatus });
  }
  let best: { community: string; mode: string; st: PugStatus } | null = null;
  for (const c of candidates) {
    if (!best || pugStateRank(c.st.state) > pugStateRank(best.st.state)) best = c;
  }

  let input: {
    kind: string;
    mode?: string;
    detail?: string;
    players?: number;
    max_players?: number;
    community?: string;
  };
  if (best && best.st.state === "live") {
    input = { kind: "live", mode: best.mode, detail: best.st.server ?? undefined, community: best.community };
  } else if (best && best.st.state === "starting") {
    input = { kind: "live", mode: best.mode, detail: "starting…", community: best.community };
  } else if (best && best.st.state === "readycheck") {
    // Ready-up in progress — show it as a queued state with the ready progress.
    input = {
      kind: "queued",
      mode: best.mode,
      players: best.st.ready_count ?? 0,
      max_players: best.st.ready_needed ?? 10,
      community: best.community,
    };
  } else if (best && best.st.state === "queued") {
    input = {
      kind: "queued",
      mode: best.mode,
      players: best.st.players ?? 0,
      max_players: best.st.max_players ?? 10,
      community: best.community,
    };
  } else {
    input = { kind: "idle" };
  }
  const key = JSON.stringify(input);
  if (key === lastPresenceKey) return;
  lastPresenceKey = key;
  void invoke("set_discord_presence", { input }).catch((err) =>
    console.error("set_discord_presence failed:", err),
  );
}

function renderPug() {
  if (!state.launcherToken) {
    pugControls.innerHTML = `
      ${
        pugTokenRejected
          ? `<p class="warn">Your launcher token wasn't recognized — it may not be linked yet, or it was reset. Re-link it below.</p>`
          : ""
      }
      <p>To queue iCTF or Elim PUGs here, run <code>/launchertoken</code> in the Instagib Nation Discord and paste the token it DMs you:</p>
      <div class="controls">
        <label>Launcher token
          <input id="pug-token" type="password" placeholder="paste your /launchertoken value" spellcheck="false" autocomplete="off" />
        </label>
        <button id="pug-token-save" type="button">Save token</button>
      </div>`;
    document.getElementById("pug-token-save")?.addEventListener("click", () => {
      const v = (document.getElementById("pug-token") as HTMLInputElement).value.trim();
      if (v) void saveLauncherToken(v);
    });
    return;
  }
  // Instagib Nation runs more than one queue (iCTF + Elim), so the player picks
  // which to act on. The token is shared across the community's modes; the
  // selector just scopes join/leave/status. Shown in every post-token branch.
  const modeLabel = pugModeLabel(state.igMode);
  const modeOpts = IGBOT_MODES.map(
    (m) =>
      `<option value="${m.key}"${m.key === state.igMode ? " selected" : ""}>${escape(m.label)}</option>`,
  ).join("");
  const modeBar = `
    <div class="utpugs-modebar">
      <label>Mode <select id="pug-mode">${modeOpts}</select></label>
      <button id="pug-token-clear" type="button" class="link-btn">change token</button>
    </div>`;

  const st = state.pugStatus;
  // Readycheck / live / starting are global to the player (resolved by the bot
  // from the token, not the selected mode), so they show under whichever mode is
  // selected — the Discord-outage backup and an active game must never hide
  // behind the dropdown.
  if (st && st.state === "readycheck") {
    // PUG filled — the ready-up step is running. This is the Discord-outage
    // backup: ready up here and the match starts without touching Discord.
    pugControls.innerHTML = `${modeBar}<div id="pug-readycheck" class="readycheck-banner readycheck-inline"></div><div id="pug-status" class="launch-status"></div>`;
    const rc = document.getElementById("pug-readycheck")!;
    rc.innerHTML = readycheckInnerHtml(st, "ictf");
    wireReadycheck(rc, "ictf", "ictf");
    wirePugModeBar();
    return;
  }
  let block: string;
  if (st && st.state === "live" && st.server) {
    block = `
      <p class="ok">🎮 Your ${escape(modeLabel)} PUG is live!</p>
      <p class="src">Server <code>${escape(st.server)}</code>${
        st.password ? ` · password <code>${escape(st.password)}</code>` : ""
      }</p>
      <button id="pug-connect" type="button" class="launch-primary pug-connect">▶&nbsp;&nbsp;Connect to PUG</button>`;
  } else if (st && st.state === "starting") {
    // PUG is live but the game server is still spinning up (NYC ~90s). Show a
    // disabled button; the 5 s poll re-renders the moment it flips to "live",
    // so the user can't launch into a server that isn't listening yet.
    block = `
      <p class="ok">🛰️ Your ${escape(modeLabel)} PUG is starting…</p>
      <p class="src">Server spinning up — Connect unlocks the moment it's ready (~90s).</p>
      <button type="button" class="launch-primary pug-connect" disabled>▶&nbsp;&nbsp;Starting…</button>`;
  } else {
    const queueLine =
      st && st.state === "queued"
        ? `In queue — ${st.players ?? 0}/${st.max_players ?? 10}`
        : `Queue for ${escape(modeLabel)}`;
    block = `
      <p>${queueLine}</p>
      <div class="discord-btns">
        <button id="pug-join" type="button">Join ${escape(modeLabel)} PUG</button>
        <button id="pug-leave" type="button">Leave</button>
        <button id="pug-refresh" type="button">Queue status</button>
        <button id="pug-spectate" type="button">Spectate live game</button>
      </div>`;
  }

  pugControls.innerHTML = `${modeBar}${block}<div id="pug-status" class="launch-status"></div>`;
  wirePugModeBar();
  if (st && st.state === "live" && st.server) {
    const server = st.server;
    const password = st.password ?? "";
    document
      .getElementById("pug-connect")
      ?.addEventListener("click", () => void connectToPug(server, password));
  }
  document.getElementById("pug-join")?.addEventListener("click", () => void pug("joinpug"));
  document.getElementById("pug-leave")?.addEventListener("click", () => void pug("leavepug"));
  document.getElementById("pug-refresh")?.addEventListener("click", () => void pug("listpug"));
  document.getElementById("pug-spectate")?.addEventListener("click", () => void spectate());
}

// Wire the Instagib Nation mode dropdown + the change-token link (shared by every
// post-token render branch). Switching mode clears the old mode's status so the
// UI doesn't show a stale queue while the first poll for the new mode lands.
function wirePugModeBar(): void {
  const sel = document.getElementById("pug-mode") as HTMLSelectElement | null;
  sel?.addEventListener("change", () => {
    state.igMode = sel.value === "elim" ? "elim" : "ictf";
    state.pugStatus = null;
    renderPug();
    pugLastFetch = Date.now(); // claim this poll so the interval doesn't double-fire
    void pollPugStatus();
  });
  document.getElementById("pug-token-clear")?.addEventListener("click", () => void saveLauncherToken(null));
}

// Shared connect path: resolve install + profile, attach the UT4 auth args and
// the -ncpconnect target, then launch (which minimizes the launcher). Used by
// both the PUG Connect button and the server browser. `status`, if given, gets
// progress / error messages.
async function connectTo(server: string, password: string, status: HTMLElement | null) {
  const di = state.installs[state.selInstall];
  if (!di) {
    if (status)
      status.innerHTML = `<span class="warn">No UT4 install detected — pick your folder in the Settings tab first.</span>`;
    return;
  }
  const profile =
    di.profiles.find((p) => p.label === state.profileLabel) ?? di.profiles[selectedProfileIndex(di)];
  const connectUrl = password ? `${server}?Password=${password}` : server;
  // The actual launch: resolve auth args, attach -ncpconnect, spawn the game.
  const doLaunch = async () => {
    let auth: Ut4AuthLaunch;
    try {
      auth = await ut4AuthArgs(di.install.root);
    } catch (err) {
      if (handleReloginError(err, status)) return;
      if (status) status.innerHTML = `<span class="warn">UT4 login failed: ${escape(String(err))}</span>`;
      return;
    }
    const args = [...profile.args, ...auth.args, `-ncpconnect=${connectUrl}`];
    if (status) status.textContent = "Connecting…";
    try {
      await invoke("launch_game", {
        executable: di.install.executable,
        args,
        priority: state.priority,
        affinityMaskHex: state.affinityHex || null,
        windowAction: state.launchWindowAction,
        env: auth.env,
      });
      if (status) status.innerHTML = `<span class="ok">Launched — connecting to ${escape(server)}…</span>`;
    } catch (err) {
      if (status) status.innerHTML = `<span class="warn">Launch failed: ${escape(String(err))}</span>`;
      console.error("connect launch failed:", err);
    }
  };

  // If UT4 is already running, a second Connect would spawn another game window
  // (the multi-instance footgun). Offer the in-game console command instead —
  // paste it with the ~ key — with a Copy button and an explicit escape hatch.
  let alreadyRunning = false;
  try {
    alreadyRunning = await invoke<boolean>("is_game_running", {
      executable: di.install.executable,
    });
  } catch (err) {
    console.error("is_game_running failed:", err);
  }
  if (alreadyRunning && status) {
    const openCmd = `open ${connectUrl}`;
    status.innerHTML =
      `<div><span class="warn">UT4 is already running.</span> Paste this in the game console (press the ~ key):</div>` +
      `<span class="chip ut4id-chip" style="margin:6px 0;">` +
      `<b class="mono">${escape(openCmd)}</b>` +
      `<button type="button" class="copy-btn" data-copy="${escape(openCmd)}">Copy</button>` +
      `</span>` +
      `<div><button type="button" class="link-btn" id="connect-anyway">Launch a new instance anyway</button></div>`;
    status
      .querySelector<HTMLButtonElement>("#connect-anyway")
      ?.addEventListener("click", () => void doLaunch());
    return;
  }
  await doLaunch();
}

async function connectToPug(server: string, password: string) {
  await connectTo(server, password, document.getElementById("pug-status"));
}

// ---- ncp://connect deep link ----------------------------------------------
// External "connect to this server" intents (ut4stats / Discord / the Discord RP
// Join button) arrive as ncp://connect?server=ip:port&password=..&spectate=1. The
// URL is UNTRUSTED (any web page or message can craft one), so we validate it
// strictly, confirm with the user, and only ever feed a clean host:port into the
// existing connect path. We append ?SpectatorOnly=1 ourselves — raw query options
// are never passed through. See src-tauri/src/lib.rs for the scheme registration.
let deepLinkWired = false;
// De-dupe guard for deep links. A single cold-start link is frequently delivered
// TWICE — once via getCurrent() and again via the onOpenUrl listener — and the
// ut4stats bounce page both auto-fires ncp:// and offers a manual Connect button.
// Without this, the duplicate runs a second connectTo and launches a second,
// racing game instance that often isn't on the server (the "it opens the game but
// not the server" report). Ignore a repeat of the same URL within a short window.
let lastConnectUri = "";
let lastConnectAt = 0;

async function handleConnectUri(raw: string): Promise<void> {
  const now = Date.now();
  if (raw === lastConnectUri && now - lastConnectAt < 5000) return;
  lastConnectUri = raw;
  lastConnectAt = now;

  let u: URL;
  try {
    u = new URL(raw);
  } catch {
    return;
  }
  if (u.protocol !== "ncp:") return;
  // Accept both ncp://connect?… (host) and ncp:connect?… (path) forms.
  const action = (u.host || u.pathname.replace(/^\/+/, "")).toLowerCase();
  if (action !== "connect") return;

  const server = (u.searchParams.get("server") ?? "").trim();
  const password = (u.searchParams.get("password") ?? "").trim();
  const spectate = u.searchParams.get("spectate") === "1";

  // host:port only — rejects anything that could inject extra launch args.
  if (!/^[A-Za-z0-9.\-]+:\d{1,5}$/.test(server)) {
    console.warn("ncp://connect rejected — bad server:", server);
    return;
  }
  if (password.length > 64 || /[^A-Za-z0-9_\-]/.test(password)) {
    console.warn("ncp://connect rejected — bad password");
    return;
  }

  // Surface the window and the Community tab FIRST — the link often arrives while
  // we're minimized after a prior launch, so the confirm must come up on a visible
  // window, and connectTo's status/errors need a visible #pug-status to land in
  // (otherwise a failure like "UT4 already running" or "no install" is silent).
  try {
    await getCurrentWindow().unminimize();
    await getCurrentWindow().setFocus();
  } catch {
    /* non-fatal */
  }
  switchView("community");

  const ok = await confirm(`${spectate ? "Spectate" : "Connect to"} ${server}?`, {
    title: "UT4 Community Launcher",
    kind: "warning",
  });
  if (!ok) return;

  const target = spectate ? `${server}?SpectatorOnly=1` : server;
  await connectTo(target, password, document.getElementById("pug-status"));
}

// Handle a cold-start URL + register the live listener, exactly once.
async function wireDeepLinks(): Promise<void> {
  if (deepLinkWired) return;
  deepLinkWired = true;
  try {
    const startUrls = await getCurrent();
    if (startUrls && startUrls.length) void handleConnectUri(startUrls[0]);
  } catch (err) {
    console.error("deep-link getCurrent failed:", err);
  }
  try {
    await onOpenUrl((urls) => {
      if (urls.length) void handleConnectUri(urls[0]);
    });
  } catch (err) {
    console.error("deep-link onOpenUrl failed:", err);
  }
}

// For a password-protected server (UT_SERVERFLAGS_i bit 0x1), reveal a password
// field in the row's status slot and connect with whatever the user types. UT4
// webviews have no window.prompt, so this is an inline input rather than a dialog.
function promptServerPassword(status: HTMLElement | null, connect: (password: string) => void) {
  if (!status) {
    connect("");
    return;
  }
  status.innerHTML = `
    <span class="row-pw">
      <input type="password" class="row-pw-input" placeholder="server password" autocomplete="off" spellcheck="false" />
      <button type="button" class="row-pw-go">Connect</button>
    </span>`;
  const input = status.querySelector<HTMLInputElement>(".row-pw-input");
  const go = () => connect(input?.value ?? "");
  status.querySelector<HTMLButtonElement>(".row-pw-go")?.addEventListener("click", go);
  input?.addEventListener("keydown", (e) => {
    if (e.key === "Enter") go();
  });
  input?.focus();
}

// Resolved account-id -> display name, cached for the session (names rarely
// change, and a player seen in two matches resolves once).
const playerNames = new Map<string, string>();

// Account IDs of everyone in a match (public + private slots), from the cached
// server list keyed by address:port.
function matchPlayerIds(server: string): string[] {
  const e = serverCache.find((s) => `${s.serverAddress}:${s.serverPort}` === server);
  return [...(e?.publicPlayers ?? []), ...(e?.privatePlayers ?? [])];
}

interface SpectatePug {
  pug_id: number;
  server: string;
  password: string;
  mode?: string;
  map?: string;
}

// Spectate a live PUG (any game — you don't have to be in it). Asks the bot for
// the live PUGs; connects directly if there's one, shows a picker if several.
// Connects with ?SpectatorOnly=1 so a spectator never takes a player slot.
async function spectate() {
  const status = document.getElementById("pug-status");
  if (status) status.textContent = "Finding live games…";
  let st: { state: string; pugs?: SpectatePug[] };
  try {
    st = JSON.parse(await invoke<string>("pug_spectate", { token: state.launcherToken ?? "" }));
  } catch (err) {
    console.error("pug_spectate failed:", err);
    if (isPugTokenError(err)) handlePugTokenError();
    else if (status) status.innerHTML = `<span class="warn">${escape(String(err))}</span>`;
    return;
  }
  const pugs = st.pugs ?? [];
  if (st.state !== "live" || pugs.length === 0) {
    if (status) status.textContent = "No live PUG to spectate right now.";
    return;
  }
  if (pugs.length === 1) {
    await spectatePug(pugs[0], status);
    return;
  }
  // Multiple live games — let the user pick.
  if (status) {
    status.innerHTML =
      `<div>${pugs.length} live games — pick one to spectate:</div>` +
      `<div class="discord-btns">${pugs
        .map(
          (p, i) =>
            `<button class="spec-pick" type="button" data-i="${i}">${escape(p.mode || "PUG")}${
              p.map ? ` · ${escape(p.map)}` : ""
            } · #${p.pug_id}</button>`,
        )
        .join("")}</div>`;
    status.querySelectorAll<HTMLButtonElement>(".spec-pick").forEach((btn) => {
      btn.addEventListener("click", () => {
        const p = pugs[Number(btn.dataset.i)];
        if (p) void spectatePug(p, status);
      });
    });
  }
}

async function spectatePug(p: SpectatePug, status: HTMLElement | null) {
  const target = p.password
    ? `${p.server}?Password=${p.password}?SpectatorOnly=1`
    : `${p.server}?SpectatorOnly=1`;
  await connectTo(target, "", status);
}

// ---- in-launcher PUG join (UTPugs / autopug, multi-mode) -------------------
// A second community alongside iCTF. autopug runs several queues, so the user
// picks a mode; everything else mirrors the iCTF flow but against the UTPugs
// commands and the separate per-user UTPugs token. The whole section is hidden
// when this build has no UTPugs base URL wired in (state.utpugsConfigured).

// autopug's PUG modes (key = the Discord pickup queue name it matches). CTF is
// omitted until autopug has a CTF host-rule (see commands.rs AUTOPUG_MODES) —
// keep this list in lockstep with that Rust allowlist.
const UTPUGS_MODES: { key: string; label: string }[] = [
  { key: "wipe", label: "Wipeout" },
  { key: "elim", label: "Elimination" },
  { key: "duel", label: "Duel" },
];

let utpugsTokenRejected = false;

function utpugsModeLabel(key: string): string {
  return UTPUGS_MODES.find((m) => m.key === key)?.label ?? key.toUpperCase();
}

// Normalize a bot-reported pickup name to a launcher mode key (wipe/elim/duel).
// Pickup names already match the keys today, but map the gametype synonyms too so
// a rename (carnage→wipe, elimination→elim, wipeout) still resolves.
function normUtpugsMode(s?: string): string {
  const m = (s ?? "").toLowerCase();
  if (m.includes("carnage") || m.includes("wipe")) return "wipe";
  if (m.includes("elim")) return "elim";
  if (m.includes("duel")) return "duel";
  return m;
}

// A token problem on any UTPugs call: drop the dead token (stops polling,
// re-renders the link prompt) and flag why — mirrors handlePugTokenError.
function handleUtpugsTokenError(): void {
  utpugsTokenRejected = true;
  void saveUtpugsToken(null);
}

async function utpugsPug(action: "joinpug" | "leavepug" | "listpug") {
  if (!state.utpugsToken?.trim()) {
    renderUtpugs();
    return;
  }
  const status = document.getElementById("utpugs-status");
  if (!pugToggleAllowed("utpugs", action, status)) return;
  // Force-updated-to-PUG: a stale NetcodePlus can't queue — update first.
  if (action === "joinpug" && ncpBlocksJoin(status)) return;
  const gate = pugToggleGate["utpugs"];
  const toggle = action === "joinpug" || action === "leavepug";
  if (toggle) {
    gate.inFlight = true;
    gate.lastAt = Date.now();
  }
  if (status) status.textContent = action === "listpug" ? "Checking queue…" : "Working…";
  try {
    const raw = await invoke<string>("utpugs_action", {
      action,
      mode: state.utpugsMode,
      token: state.utpugsToken ?? "",
      build: selectedNcp().build,
    });
    let msg = raw;
    try {
      msg = (JSON.parse(raw) as { message?: string }).message ?? raw;
    } catch {
      /* response wasn't JSON; show it raw */
    }
    if (status) status.textContent = msg;
  } catch (err) {
    console.error("utpugs_action failed:", err);
    if (isPugTokenError(err)) handleUtpugsTokenError();
    else if (status) status.innerHTML = `<span class="warn">${escape(String(err))}</span>`;
  } finally {
    if (toggle) gate.inFlight = false;
  }
}

async function saveUtpugsToken(token: string | null) {
  state.utpugsToken = token;
  if (token) utpugsTokenRejected = false;
  if (!token) state.utpugsStatus = null;
  try {
    await invoke("save_utpugs_token", { token });
  } catch (err) {
    console.error("save_utpugs_token failed:", err);
  }
  renderUtpugs();
  renderHomeReadycheck(); // clears the HOME banner if it was showing a UTPugs readycheck
  updateDiscordPresence();
  if (token) {
    void pollUtpugsStatus();
    startUtpugsPolling();
  } else {
    stopUtpugsPolling();
  }
}

async function utpugsSpectate() {
  const status = document.getElementById("utpugs-status");
  if (status) status.textContent = "Finding live games…";
  let st: { state: string; pugs?: SpectatePug[] };
  try {
    st = JSON.parse(await invoke<string>("utpugs_spectate", { token: state.utpugsToken ?? "" }));
  } catch (err) {
    console.error("utpugs_spectate failed:", err);
    if (isPugTokenError(err)) handleUtpugsTokenError();
    else if (status) status.innerHTML = `<span class="warn">${escape(String(err))}</span>`;
    return;
  }
  const pugs = st.pugs ?? [];
  if (st.state !== "live" || pugs.length === 0) {
    if (status) status.textContent = "No live UTPugs game to spectate right now.";
    return;
  }
  if (pugs.length === 1) {
    await spectatePug(pugs[0], status);
    return;
  }
  if (status) {
    status.innerHTML =
      `<div>${pugs.length} live games — pick one to spectate:</div>` +
      `<div class="discord-btns">${pugs
        .map(
          (p, i) =>
            `<button class="spec-pick" type="button" data-i="${i}">${escape(p.mode || "PUG")}${
              p.map ? ` · ${escape(p.map)}` : ""
            } · #${p.pug_id}</button>`,
        )
        .join("")}</div>`;
    status.querySelectorAll<HTMLButtonElement>(".spec-pick").forEach((btn) => {
      btn.addEventListener("click", () => {
        const p = pugs[Number(btn.dataset.i)];
        if (p) void spectatePug(p, status);
      });
    });
  }
}

async function connectToUtpugs(server: string, password: string) {
  await connectTo(server, password, document.getElementById("utpugs-status"));
}

function renderUtpugs(): void {
  const section = document.getElementById("utpugs-section");
  const el = document.getElementById("utpugs-controls");
  if (!section || !el) return;
  // Not wired up in this build → hide the whole section, heading and all.
  if (!state.utpugsConfigured) {
    section.style.display = "none";
    return;
  }
  section.style.display = "";

  if (!state.utpugsToken) {
    el.innerHTML = `
      ${
        utpugsTokenRejected
          ? `<p class="warn">Your UTPugs token wasn't recognized — it may not be linked yet, or it was reset. Re-link it below.</p>`
          : ""
      }
      <p>Queue UTPugs PUGs — Wipeout, Elimination, Duel — here. Run <code>/launchertoken</code> in the UTPugs Discord and paste the token it DMs you:</p>
      <div class="controls">
        <label>Launcher token
          <input id="utpugs-token" type="password" placeholder="paste your /launchertoken value" spellcheck="false" autocomplete="off" />
        </label>
        <button id="utpugs-token-save" type="button">Save token</button>
      </div>`;
    document.getElementById("utpugs-token-save")?.addEventListener("click", () => {
      const v = (document.getElementById("utpugs-token") as HTMLInputElement).value.trim();
      if (v) void saveUtpugsToken(v);
    });
    return;
  }

  const modeLabel = utpugsModeLabel(state.utpugsMode);
  const modeOpts = UTPUGS_MODES.map(
    (m) =>
      `<option value="${escape(m.key)}"${m.key === state.utpugsMode ? " selected" : ""}>${escape(m.label)}</option>`,
  ).join("");
  const modeBar = `
    <div class="utpugs-modebar">
      <label>Mode <select id="utpugs-mode">${modeOpts}</select></label>
      <button id="utpugs-token-clear" type="button" class="link-btn">change token</button>
    </div>`;

  const st = state.utpugsStatus;
  // Readycheck is global to the player (one pickup's check-in at a time) and
  // carries its own mode, so show it regardless of the selected tab — the
  // Discord-outage backup must never be hidden behind the Mode dropdown.
  if (st && st.state === "readycheck") {
    el.innerHTML = `${modeBar}<div id="utpugs-readycheck" class="readycheck-banner readycheck-inline"></div><div id="utpugs-status" class="launch-status"></div>`;
    const rc = document.getElementById("utpugs-readycheck")!;
    rc.innerHTML = readycheckInnerHtml(st, "utpugs");
    wireReadycheck(rc, "utpugs", "utpugs");
    const rcModeSel = document.getElementById("utpugs-mode") as HTMLSelectElement | null;
    rcModeSel?.addEventListener("change", () => {
      state.utpugsMode = rcModeSel.value;
      renderUtpugs();
    });
    document
      .getElementById("utpugs-token-clear")
      ?.addEventListener("click", () => void saveUtpugsToken(null));
    return;
  }
  // `queued` is per-mode. If the bot tells us which pickup it's for (st.mode) and
  // that's NOT the selected mode, don't render it as this mode's queue — the user is
  // queued elsewhere. Older bots omit st.mode → fall back to assuming the selection.
  // live/starting carry no mode and are global (you can connect from any tab).
  const queuedMode = st?.state === "queued" && st.mode ? normUtpugsMode(st.mode) : null;
  const queuedHere = st?.state === "queued" && (queuedMode === null || queuedMode === state.utpugsMode);
  const queuedElsewhere = queuedMode !== null && queuedMode !== state.utpugsMode;
  let block: string;
  if (st && st.state === "live" && st.server) {
    block = `
      <p class="ok">🎮 Your ${escape(modeLabel)} PUG is live!</p>
      <p class="src">Server <code>${escape(st.server)}</code>${
        st.password ? ` · password <code>${escape(st.password)}</code>` : ""
      }</p>
      <button id="utpugs-connect" type="button" class="launch-primary pug-connect">▶&nbsp;&nbsp;Connect to PUG</button>`;
  } else if (st && st.state === "starting") {
    block = `
      <p class="ok">🛰️ Your ${escape(modeLabel)} PUG is starting…</p>
      <p class="src">Server spinning up — Connect unlocks the moment it's ready (~90s).</p>
      <button type="button" class="launch-primary pug-connect" disabled>▶&nbsp;&nbsp;Starting…</button>`;
  } else {
    const queueLine = queuedHere
      ? `In queue — ${st?.players ?? 0}/${st?.max_players ?? 10}`
      : `Queue for ${escape(modeLabel)}`;
    // When queued in a different mode, say so instead of silently showing this
    // tab as empty — so the player knows where they actually are.
    const elsewhereNote = queuedElsewhere
      ? `<p class="src">You're in the ${escape(utpugsModeLabel(queuedMode as string))} queue — switch Mode to manage it.</p>`
      : "";
    block = `
      <p>${queueLine}</p>
      ${elsewhereNote}
      <div class="discord-btns">
        <button id="utpugs-join" type="button">Join ${escape(modeLabel)} PUG</button>
        <button id="utpugs-leave" type="button">Leave</button>
        <button id="utpugs-refresh" type="button">Queue status</button>
        <button id="utpugs-spectate" type="button">Spectate live game</button>
      </div>`;
  }

  el.innerHTML = `${modeBar}${block}<div id="utpugs-status" class="launch-status"></div>`;

  const modeSel = document.getElementById("utpugs-mode") as HTMLSelectElement | null;
  modeSel?.addEventListener("change", () => {
    state.utpugsMode = modeSel.value;
    // Status is per-mode — clear it so we don't show the old mode's queue while
    // the first poll for the new mode is in flight.
    state.utpugsStatus = null;
    renderUtpugs();
    void pollUtpugsStatus();
  });
  document
    .getElementById("utpugs-token-clear")
    ?.addEventListener("click", () => void saveUtpugsToken(null));
  if (st && st.state === "live" && st.server) {
    const server = st.server;
    const password = st.password ?? "";
    document
      .getElementById("utpugs-connect")
      ?.addEventListener("click", () => void connectToUtpugs(server, password));
  }
  document.getElementById("utpugs-join")?.addEventListener("click", () => void utpugsPug("joinpug"));
  document.getElementById("utpugs-leave")?.addEventListener("click", () => void utpugsPug("leavepug"));
  document.getElementById("utpugs-refresh")?.addEventListener("click", () => void utpugsPug("listpug"));
  document.getElementById("utpugs-spectate")?.addEventListener("click", () => void utpugsSpectate());
}

// ── Adaptive poll cadence ────────────────────────────────────────────────────
// The status pollers used to hit the bot every 5s forever (even idle/minimized),
// and the live-banner + queue-nudge poll ran every 8s always-on — a lot of needless
// traffic. Now a cheap 5s base tick wakes up but only HITS the network when the
// per-state cadence is due: fast while engaged (queued/readycheck/starting) so a
// minimized launcher still catches the ready-up; slow when idle; slowest when idle
// AND hidden. The banner/nudge poll on their own slow cadence and pause while hidden.
const POLL_TICK_MS = 5000; // base wakeup; no network unless a cadence is due
const POLL_ACTIVE_MS = 10000; // queued / readycheck / starting (even when hidden)
const POLL_IDLE_MS = 45000; // not in a queue, window visible
const POLL_HIDDEN_IDLE_MS = 300000; // idle AND minimized
const LIVE_POLL_MS = 180000; // HOME live-PUG banner
const QUEUES_POLL_MS = 45000; // HOME "queue filling" nudge

let pugLastFetch = 0;
let utpugsLastFetch = 0;
let unrealpugsLastFetch = 0;
let liveLastFetch = 0;
let queuesLastFetch = 0;
let visibilityWired = false;

// "Engaged" = in a queue or a ready-up is live — state can change fast and the
// readycheck desktop notification must still fire when minimized, so we stay on
// the fast cadence regardless of visibility.
function pollEngaged(s: string | null | undefined): boolean {
  return s === "queued" || s === "readycheck" || s === "starting";
}

function statusPollDue(lastFetch: number, engaged: boolean): boolean {
  const cadence = engaged ? POLL_ACTIVE_MS : document.hidden ? POLL_HIDDEN_IDLE_MS : POLL_IDLE_MS;
  return Date.now() - lastFetch >= cadence;
}

// Becoming visible again: refresh the visible surfaces immediately rather than
// waiting out the idle/hidden cadence (reset the per-poll clocks to match).
function onLauncherVisible(): void {
  if (document.hidden) return;
  const now = Date.now();
  liveLastFetch = now;
  queuesLastFetch = now;
  void pollLivePugs();
  void pollQueues();
  if (state.launcherToken) {
    pugLastFetch = now;
    void pollPugStatus();
  }
  if (state.utpugsToken && state.utpugsConfigured) {
    utpugsLastFetch = now;
    void pollUtpugsStatus();
  }
  if (state.unrealpugsToken && state.unrealpugsConfigured) {
    unrealpugsLastFetch = now;
    void pollUnrealpugsStatus();
  }
  // Re-check the plugin/launcher version on focus when the startup check never
  // landed (flaky cold start → statusCache.plugin still null) or it's gone stale —
  // so a freshly published release shows without a full restart, and a failed
  // first check self-heals. Rate-limited via statusLastOk so refocusing the window
  // doesn't re-hit the manifest each time.
  if (statusCache.plugin === null || now - statusLastOk >= STATUS_REFRESH_MS) {
    void loadStatusData();
  }
}

let utpugsPollTimer: number | undefined;

function startUtpugsPolling() {
  if (utpugsPollTimer !== undefined) return;
  utpugsLastFetch = Date.now();
  utpugsPollTimer = window.setInterval(() => {
    if (statusPollDue(utpugsLastFetch, pollEngaged(state.utpugsStatus?.state))) {
      utpugsLastFetch = Date.now();
      void pollUtpugsStatus();
    }
  }, POLL_TICK_MS);
}

function stopUtpugsPolling() {
  if (utpugsPollTimer !== undefined) {
    clearInterval(utpugsPollTimer);
    utpugsPollTimer = undefined;
  }
}

async function pollUtpugsStatus() {
  if (!state.utpugsToken || !state.utpugsConfigured) return;
  try {
    const next = JSON.parse(
      await invoke<string>("utpugs_status", { mode: state.utpugsMode, token: state.utpugsToken }),
    ) as PugStatus;
    const prev = state.utpugsStatus;
    state.utpugsStatus = next;
    updateDiscordPresence();
    // Same alert-on-transition as iCTF: entering readycheck fires the desktop
    // notification + chime once, so a minimized launcher still grabs the player.
    if (next.state === "readycheck" && prev?.state !== "readycheck") {
      void notifyReadycheck(next);
      playReadyChime();
    }
    if (pugStatusChanged(prev, next)) {
      renderUtpugs();
      renderHomeReadycheck();
    }
  } catch (err) {
    console.error("utpugs_status failed:", err);
    if (isPugTokenError(err)) handleUtpugsTokenError();
  }
}

// ---- in-launcher PUG join (UnrealPUGs / skandalouz's PugApi, 3rd community) ─
// A reduced mirror of the UTPugs section: this bot tracks no live game servers,
// so there is no connect/spectate/ready — only join / leave / queue-status.
// Blitz-only for now; the mode string must be exactly what the Rust allowlist
// (commands.rs UNREALPUGS_MODES) and the bot expect — an EXACT-match uppercase
// "BLITZ" (the bot is case-lenient, but our own guard is not). The whole section
// is hidden when this build has no UnrealPUGs base URL wired in.
const UNREALPUGS_MODE = "BLITZ";

let unrealpugsTokenRejected = false;
let unrealpugsActionInFlight = false;

// A token problem on any UnrealPUGs call: drop the dead token (stops polling,
// re-renders the link prompt) and flag why — mirrors handleUtpugsTokenError.
function handleUnrealpugsTokenError(): void {
  unrealpugsTokenRejected = true;
  void saveUnrealpugsToken(null);
}

async function saveUnrealpugsToken(token: string | null) {
  state.unrealpugsToken = token;
  if (token) unrealpugsTokenRejected = false;
  if (!token) state.unrealpugsStatus = null;
  try {
    await invoke("save_unrealpugs_token", { token });
  } catch (err) {
    console.error("save_unrealpugs_token failed:", err);
  }
  renderUnrealpugs();
  updateDiscordPresence();
  if (token) {
    void pollUnrealpugsStatus();
    startUnrealpugsPolling();
  } else {
    stopUnrealpugsPolling();
  }
}

async function unrealpugsPug(action: "joinpug" | "leavepug" | "listpug") {
  const token = state.unrealpugsToken?.trim();
  if (!token) {
    renderUnrealpugs();
    return;
  }
  const status = document.getElementById("unrealpugs-status");
  const toggle = action === "joinpug" || action === "leavepug";
  // Simple single-flight on join/leave so a double-click can't fire two toggles.
  if (toggle) {
    if (unrealpugsActionInFlight) return;
    unrealpugsActionInFlight = true;
  }
  if (status) status.textContent = action === "listpug" ? "Checking queue…" : "Working…";
  try {
    const raw = await invoke<string>("unrealpugs_action", {
      action,
      mode: UNREALPUGS_MODE,
      token,
    });
    let msg = raw;
    try {
      msg = (JSON.parse(raw) as { message?: string }).message ?? raw;
    } catch {
      /* response wasn't JSON; show it raw */
    }
    if (status) status.textContent = msg;
    // Reflect the queue change immediately rather than waiting for the next poll.
    void pollUnrealpugsStatus();
  } catch (err) {
    console.error("unrealpugs_action failed:", err);
    if (isPugTokenError(err)) handleUnrealpugsTokenError();
    else if (status) status.innerHTML = `<span class="warn">${escape(String(err))}</span>`;
  } finally {
    if (toggle) unrealpugsActionInFlight = false;
  }
}

function renderUnrealpugs(): void {
  const section = document.getElementById("unrealpugs-section");
  const el = document.getElementById("unrealpugs-controls");
  if (!section || !el) return;
  // Not wired up in this build → hide the whole section, heading and all.
  if (!state.unrealpugsConfigured) {
    section.style.display = "none";
    return;
  }
  section.style.display = "";

  if (!state.unrealpugsToken) {
    el.innerHTML = `
      ${
        unrealpugsTokenRejected
          ? `<p class="warn">Your UnrealPUGs token wasn't recognized — it may not be linked yet, or it was reset. Re-link it below.</p>`
          : ""
      }
      <p>Queue UnrealPUGs Blitz here. Run <code>launchertoken</code> in the UnrealPUGs Discord and paste the token it DMs you:</p>
      <div class="controls">
        <label>Launcher token
          <input id="unrealpugs-token" type="password" placeholder="paste your launchertoken value" spellcheck="false" autocomplete="off" />
        </label>
        <button id="unrealpugs-token-save" type="button">Save token</button>
      </div>`;
    document.getElementById("unrealpugs-token-save")?.addEventListener("click", () => {
      const v = (document.getElementById("unrealpugs-token") as HTMLInputElement).value.trim();
      if (v) void saveUnrealpugsToken(v);
    });
    return;
  }

  const st = state.unrealpugsStatus;
  let block: string;
  if (st && st.state === "starting") {
    block = `<p class="ok">🛰️ Your Blitz PUG is starting — picking teams…</p>`;
  } else {
    const queued = st?.state === "queued";
    const queueLine = queued
      ? `In queue — ${st?.players ?? 0}/${st?.max_players ?? 10}`
      : `Queue for Blitz`;
    block = `
      <p>${queueLine}</p>
      <div class="discord-btns">
        <button id="unrealpugs-join" type="button">Join Blitz PUG</button>
        <button id="unrealpugs-leave" type="button">Leave</button>
        <button id="unrealpugs-refresh" type="button">Queue status</button>
      </div>`;
  }
  el.innerHTML = `
    <div class="utpugs-modebar">
      <span class="src">Blitz</span>
      <button id="unrealpugs-token-clear" type="button" class="link-btn">change token</button>
    </div>
    ${block}<div id="unrealpugs-status" class="launch-status"></div>`;

  document
    .getElementById("unrealpugs-token-clear")
    ?.addEventListener("click", () => void saveUnrealpugsToken(null));
  document
    .getElementById("unrealpugs-join")
    ?.addEventListener("click", () => void unrealpugsPug("joinpug"));
  document
    .getElementById("unrealpugs-leave")
    ?.addEventListener("click", () => void unrealpugsPug("leavepug"));
  document
    .getElementById("unrealpugs-refresh")
    ?.addEventListener("click", () => void unrealpugsPug("listpug"));
}

let unrealpugsPollTimer: number | undefined;

function startUnrealpugsPolling() {
  if (unrealpugsPollTimer !== undefined) return;
  unrealpugsLastFetch = Date.now();
  unrealpugsPollTimer = window.setInterval(() => {
    if (statusPollDue(unrealpugsLastFetch, pollEngaged(state.unrealpugsStatus?.state))) {
      unrealpugsLastFetch = Date.now();
      void pollUnrealpugsStatus();
    }
  }, POLL_TICK_MS);
}

function stopUnrealpugsPolling() {
  if (unrealpugsPollTimer !== undefined) {
    clearInterval(unrealpugsPollTimer);
    unrealpugsPollTimer = undefined;
  }
}

async function pollUnrealpugsStatus() {
  if (!state.unrealpugsToken || !state.unrealpugsConfigured) return;
  try {
    const next = JSON.parse(
      await invoke<string>("unrealpugs_status", {
        mode: UNREALPUGS_MODE,
        token: state.unrealpugsToken,
      }),
    ) as PugStatus;
    const prev = state.unrealpugsStatus;
    state.unrealpugsStatus = next;
    updateDiscordPresence();
    if (pugStatusChanged(prev, next)) renderUnrealpugs();
  } catch (err) {
    console.error("unrealpugs_status failed:", err);
    if (isPugTokenError(err)) handleUnrealpugsTokenError();
  }
}

// ---- server browser -------------------------------------------------------

interface ServerAttrs {
  UT_SERVERNAME_s?: string;
  MAPNAME_s?: string;
  GAMEMODE_s?: string;
  UT_MAXPLAYERS_i?: number;
  UT_PLAYERONLINE_i?: number;
  UT_SERVERTRUSTLEVEL_i?: number;
  UT_NUMMATCHES_i?: number;
  UT_GAMEINSTANCE_i?: number;
  UT_MATCHSTATE_s?: string;
  UT_HUBGUID_s?: string;
  // Bitmask; bit 0x1 set = password-protected (the in-game browser's lock icon).
  UT_SERVERFLAGS_i?: number;
  // Per-instance GUID; the key into a hub's UU_CUSTOMMATCHNAMES_s title list.
  UT_SERVERINSTANCEGUID_s?: string;
  // On a HUB: newline-joined "<instanceGUID>:<host match title>" for its matches.
  UU_CUSTOMMATCHNAMES_s?: string;
  // Comma-separated forced-mutator list (e.g. "AntiCheatV3,MutStatSQL").
  UU_FORCEDMUTATORS_s?: string;
  // Match clock, seconds (populated while InProgress; 0 = none / round-based).
  UT_MATCHELAPSEDTIME_i?: number;
  UT_MATCHDURATION_i?: number;
}

interface GameServerEntry {
  serverName?: string;
  // Display name of the session owner (host); a match-title fallback.
  ownerName?: string;
  serverAddress?: string;
  serverPort?: number;
  totalPlayers?: number;
  maxPublicPlayers?: number;
  started?: boolean;
  // Account-ID hashes of joined players (names aren't in the session — resolved
  // on demand via resolve_player_names for the roster expander).
  publicPlayers?: string[];
  privatePlayers?: string[];
  attributes?: ServerAttrs;
}

// A hub lobby (UTLobbyGameMode, or a session sitting on the UT-Entry menu map)
// has no live match of its own; its matches advertise as separate sessions and
// are grouped back under the hub by shared IP (see renderServerList).
function isLobby(s: GameServerEntry): boolean {
  return (
    /lobby/i.test(s.attributes?.GAMEMODE_s ?? "") ||
    (s.attributes?.MAPNAME_s ?? "") === "UT-Entry"
  );
}

// Turn a gamemode class path into something human ("…UTLobbyGameMode" → "Hub",
// "…NCP-IGCTF_C" → "IGCTF", "…UTCTFGameMode" → "CTF").
function prettyMode(gamemode: string): string {
  if (!gamemode) return "—";
  if (/lobby/i.test(gamemode)) return "Hub";
  const seg = gamemode.split(/[./]/).pop() ?? gamemode;
  return (
    seg
      .replace(/_C$/, "")
      .replace(/GameMode$/i, "")
      .replace(/^NCP-?/i, "")
      .replace(/^UT/, "") || seg
  );
}

let serverCache: GameServerEntry[] = [];
let serversShowEmpty = false;
let serversFetching = false;
// Two-pane browser view state (local; survives re-render, reset only by refetch).
let serversMode: "hubs" | "servers" = "hubs";
let selectedHubId: string | null = null;
let selectedMatchId: string | null = null;
let serversSearch = "";

// ---- hub content-pak versions (UTCC) -------------------------------------
// Which NCP content-pak version each hub runs, from utcustomcontent.com (the
// hub_pak_versions command aggregates the 6 paks' server lists). Surfaced in the
// hub detail so a player can SEE when a hub lags the latest pak — the case where
// their newer local pak would otherwise stop them joining — and revert their copy.
interface HubPakVersion {
  pak: string;
  version: string;
  is_latest: boolean;
}
interface HubPakEntry {
  name: string;
  server_type: string;
  paks: HubPakVersion[];
}
interface HubPakData {
  latest: Record<string, string>;
  hubs: HubPakEntry[];
}
let hubPakData: HubPakData | null = null;
let hubPakFetched = false;

const HUB_PAK_LABELS: Record<string, string> = {
  elimplus: "ElimPlus",
  wipeout: "Wipeout",
  ncutplus: "UTPlus",
  instagibncp: "Instagib",
  sdom: "Sdom",
  ncwepmut: "Weapons",
  ncstockweapons: "Stock Weapons",
  mutannouncers: "Announcers",
};

// Normalize a hub name so UTCC's list matches the live server browser (strip
// bracket tags / punctuation / case): "[PHX] PHOENIX GERMANY" ↔ "phx phoenix germany".
function normHubName(s: string): string {
  return s
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, " ")
    .trim();
}

// Fetch the hub→pak-version map once per session (it changes rarely), then
// re-render the browser so the detail pane picks it up. Best-effort: a failure
// (UTCC down) just leaves the pak panel hidden — the server browser still works.
async function loadHubPakData() {
  if (hubPakFetched) return;
  try {
    hubPakData = JSON.parse(await invoke<string>("hub_pak_versions")) as HubPakData;
    hubPakFetched = true;
    if (document.getElementById("srv-search")) renderServerList();
  } catch (err) {
    console.error("hub_pak_versions failed:", err);
  }
}

// The selected hub's NCP pak versions — laggards (older than latest) flagged amber
// with a per-pak "revert" that removes the user's newer local copy so the hub can
// serve its own. Empty when the hub isn't in UTCC's list or data hasn't loaded.
function hubPakPanel(hub: GameServerEntry | null): string {
  if (!hub || !hubPakData) return "";
  const name = String(hub.attributes?.UT_SERVERNAME_s || hub.serverName || "").trim();
  if (!name) return "";
  const entry = hubPakData.hubs.find((h) => normHubName(h.name) === normHubName(name));
  if (!entry || !entry.paks.length) return "";

  // ElimPlus, UTPlus and WipeoutMutator cross-reference each other in Blueprint, so
  // a version mismatch in ANY one breaks the set. Check + revert them as ONE cluster
  // (only where its anchor — ElimPlus — actually runs; UTPlus on a non-ElimPlus hub
  // stays its own badge) and surface it under a single "ElimPlus" label.
  const PAK_GROUPS: { label: string; anchor: string; members: string[] }[] = [
    { label: "ElimPlus", anchor: "elimplus", members: ["elimplus", "ncutplus", "wipeout"] },
  ];
  const byKey = new Map(entry.paks.map((p) => [p.pak, p] as [string, HubPakVersion]));
  const grouped = new Set<string>();
  const badges: string[] = [];
  let anyBehind = false;

  const latestOf = (k: string): string | undefined => hubPakData?.latest?.[k];
  const okBadge = (label: string, ver: string): string =>
    `<span class="hubpak ok" title="matches the latest content">${escape(label)} ${escape(ver)}</span>`;
  const behindBadge = (
    label: string,
    ver: string,
    latest: string | undefined,
    revertKeys: string[],
  ): string => {
    const latestV = latest ? ` · latest ${escape(latest)}` : "";
    return `<span class="hubpak warn" title="this hub runs an older ${escape(label)} than the latest — a version mismatch can stop you joining">${escape(label)} ${escape(ver)}${latestV} <button type="button" class="link-btn hubpak-revert" data-paks="${escape(revertKeys.join(","))}">revert</button></span>`;
  };

  // Clusters first: flagged when ANY present member is behind; revert clears the
  // WHOLE cluster's local paks (a partial revert would re-break the BP refs).
  for (const g of PAK_GROUPS) {
    if (!byKey.has(g.anchor)) continue;
    g.members.filter((m) => byKey.has(m)).forEach((m) => grouped.add(m));
    const behind = g.members.some((m) => byKey.has(m) && !byKey.get(m)!.is_latest);
    const head = byKey.get(g.anchor)!;
    if (behind) {
      anyBehind = true;
      badges.push(behindBadge(g.label, head.version, latestOf(g.anchor), g.members));
    } else {
      badges.push(okBadge(g.label, head.version));
    }
  }
  // Remaining paks (Weapons / Instagib / Sdom) as their own single badges.
  for (const p of entry.paks) {
    if (grouped.has(p.pak)) continue;
    const label = HUB_PAK_LABELS[p.pak] ?? p.pak;
    if (p.is_latest) {
      badges.push(okBadge(label, p.version));
    } else {
      anyBehind = true;
      badges.push(behindBadge(label, p.version, latestOf(p.pak), [p.pak]));
    }
  }

  const note = anyBehind
    ? `<div class="src">Amber = this hub runs an older pak than the latest. If you can't join here, “revert” removes your newer local copies for that set so the hub serves its own versions (UT4 must be closed).</div>`
    : "";
  return `<div class="srv-hubpaks"><div class="srv-hubpaks-h">NetcodePlus content on this hub</div><div class="srv-hubpak-list">${badges.join("")}</div>${note}</div>`;
}

// Remove the user's local copies of a pak set so a lagging hub can serve its own —
// the per-hub fix for the "newer local pak blocks a not-yet-updated hub" case. Takes
// a SET because cross-referenced paks (the ElimPlus cluster) must move together, or a
// surviving newer pak re-breaks the BP refs. remove_installed_pak is idempotent, so
// listing a pak the user doesn't have installed is a harmless no-op.
async function revertHubPak(pakIds: string[]) {
  const labels = pakIds.map((id) => HUB_PAK_LABELS[id] ?? id);
  const human =
    labels.length > 1
      ? `${labels.slice(0, -1).join(", ")} and ${labels[labels.length - 1]}`
      : labels[0];
  const plural = pakIds.length > 1;
  const ok = await confirm(
    `Remove your ${human} content pak${plural ? "s" : ""}?${
      plural ? " These cross-reference each other, so they're removed together." : ""
    } This deletes your local copies from DownloadedPaks so a hub running older versions can serve its own consistent set — the fix when a newer local pak stops you joining a not-yet-updated hub. UT4 must be closed. You can reinstall anytime from the "Update paks" button on Home.`,
    { title: `Revert ${human}`, kind: "warning", okLabel: "Remove", cancelLabel: "Cancel" },
  );
  if (!ok) return;
  const status = document.getElementById("srv-detail-status");
  if (status) status.textContent = `Removing ${human}…`;
  try {
    for (const id of pakIds) {
      await invoke("remove_installed_pak", { pakId: id });
    }
    if (status)
      status.innerHTML = `<span class="ok">✓ Removed ${escape(human)} — the hub will serve its own version${plural ? "s" : ""} when you join.</span>`;
    try {
      statusCache.paks = await invoke<PakStatusResult>("pak_status");
    } catch {
      /* dash refresh is best-effort */
    }
  } catch (err) {
    console.error("remove_installed_pak failed:", err);
    if (status) status.innerHTML = `<span class="warn">${escape(String(err))}</span>`;
  }
}

async function renderServers() {
  if (serversFetching) return;
  serversFetching = true;
  serversPanel.classList.remove("srv-host");
  serversPanel.innerHTML = `<p>Loading the server list…</p>`;
  try {
    serverCache = JSON.parse(await invoke<string>("list_servers")) as GameServerEntry[];
  } catch (err) {
    serversPanel.innerHTML = `<div class="warn">Couldn't load the server list: ${escape(String(err))}</div>`;
    serversFetching = false;
    return;
  }
  serversFetching = false;
  renderServerList();
  void loadHubPakData();
}

// Live player count for a session (in-game players for an instance, lobby
// occupancy for a hub).
function srvPlayers(s: GameServerEntry): number {
  return Number(s.totalPlayers ?? s.attributes?.UT_PLAYERONLINE_i ?? 0) || 0;
}

// A session has a real, connectable address.
function srvHasAddr(s: GameServerEntry): boolean {
  return !!s.serverAddress && s.serverAddress !== "0.0.0.0" && !!s.serverPort;
}

// Render the two-pane master/detail server browser from the cached list, so the
// toggle / search / "show empty" controls re-filter without a re-fetch. Hubs go
// in the left (master) pane; the selected hub's live matches (instances) go in
// the right (detail) pane. Instances link to their hub by shared IP first (the
// reliable signal — UT_HUBGUID_s on an instance rarely equals its hub's
// advertised GUID), then by UT_HUBGUID_s as a fallback.

// Friendly gamemode name for a match card ("…UTDMGameMode" → "Deathmatch"),
// falling back to the terse prettyMode() for anything unmapped. iCTF is split
// out from CTF by sniffing the forced-mutator list for instagib.
function friendlyMode(gamemode: string, mutators: string): string {
  const seg = (gamemode || "").split(/[./]/).pop() ?? "";
  const ig = /instagib/i.test(mutators || "");
  if (/NCP-?IGCTF/i.test(seg)) return "Instagib CTF";
  if (/UTDMGameMode/i.test(seg)) return "Deathmatch";
  if (/Elimination_113/i.test(seg)) return "Elimination";
  if (/ElimPlus/i.test(seg)) return "Elim+";
  if (/WipeoutPlus/i.test(seg)) return "Wipeout";
  if (/UTCTFGameMode/i.test(seg)) return ig ? "Instagib CTF" : "CTF";
  if (/UTFlagRunGame/i.test(seg)) return "Blitz";
  if (/UTDuelGame/i.test(seg)) return "Duel";
  if (/UTTeamGameMode/i.test(seg)) return "Team DM";
  return prettyMode(gamemode);
}

// Title-case match state for a card ("WaitingToStart" → "Waiting to Start").
function matchStateLabel(s: string | undefined): string {
  switch (s) {
    case "InProgress":
      return "In Progress";
    case "WaitingToStart":
      return "Waiting to Start";
    case "CountdownToBegin":
      return "Starting…";
    case "WaitingPostMatch":
    case "MatchEnteringOvertime":
      return "Ending";
    default:
      return "";
  }
}

function secondsToClock(n: number): string {
  const t = Math.max(0, Math.floor(n));
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, "0")}`;
}

// Match clock string, or null when there's nothing meaningful to show. Timed
// modes show elapsed / limit; round-based modes (duration 0) show elapsed only —
// which is why the Elimination card reads "4:32" with no "/ limit".
function matchClock(a: ServerAttrs | undefined): string | null {
  if (!a) return null;
  const st = a.UT_MATCHSTATE_s;
  const el = Number(a.UT_MATCHELAPSEDTIME_i ?? 0) || 0;
  const dur = Number(a.UT_MATCHDURATION_i ?? 0) || 0;
  if (st === "WaitingToStart" || st === "CountdownToBegin") {
    return dur > 0 ? `0:00 / ${secondsToClock(dur)}` : "0:00";
  }
  if (dur > 0) return `${secondsToClock(el)} / ${secondsToClock(dur)}`;
  if (el > 0) return secondsToClock(el);
  return null;
}

// Build a global instanceGUID -> host match title map from every hub's
// UU_CUSTOMMATCHNAMES_s ("<instanceGUID>:<host title>" entries, newline-joined).
// Instance GUIDs are globally unique, so one map serves both panes.
function customNameMap(hubs: GameServerEntry[]): Map<string, string> {
  const m = new Map<string, string>();
  for (const h of hubs) {
    const raw = h.attributes?.UU_CUSTOMMATCHNAMES_s;
    if (!raw) continue;
    for (const line of raw.split(/\r?\n/)) {
      const i = line.indexOf(":");
      if (i > 0) m.set(line.slice(0, i).toUpperCase(), line.slice(i + 1));
    }
  }
  return m;
}

// A match's display title: the host's custom name (keyed by instance GUID), else
// the match's own broadcast name (UT_SERVERNAME_s), else "<owner>'s match", else
// "" (the card then promotes "<Mode> in <Map>").
function matchTitle(inst: GameServerEntry, names: Map<string, string>): string {
  const guid = String(inst.attributes?.UT_SERVERINSTANCEGUID_s ?? "").toUpperCase();
  const custom = guid ? names.get(guid) : undefined;
  if (custom && custom.trim()) return custom.trim();
  // The match instance broadcasts its OWN name (UT_SERVERNAME_s): the host-set
  // match title, or the auto "<Mode> on <Map>". Prefer it over the generic owner
  // fallback — for a match with no hub-level custom-name entry this is exactly
  // what stock UT4's browser shows (e.g. "Again!" or "Deathmatch on DM-Tempest"),
  // whereas the owner is a bare dedicated-server id like "[DS]2e111ee27c62-489".
  const serverName = String(inst.attributes?.UT_SERVERNAME_s ?? "").trim();
  if (serverName) return serverName;
  if (inst.ownerName && inst.ownerName.trim()) return `${inst.ownerName.trim()}'s match`;
  return "";
}

// Small inline icons (static markup — no user data, no escaping needed).
const SRV_ICON = {
  person: `<svg viewBox="0 0 16 16" width="13" height="13" fill="currentColor" aria-hidden="true"><circle cx="8" cy="4.3" r="3"/><path d="M2 14.5C2 11.4 4.7 9.3 8 9.3s6 2.1 6 5.2v.5H2z"/></svg>`,
  pad: `<svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor" aria-hidden="true"><path d="M7 8h10a4 4 0 0 1 4 4v1.6A3.4 3.4 0 0 1 14.7 15L14 14.2h-4l-.7.8A3.4 3.4 0 0 1 3 13.6V12a4 4 0 0 1 4-4Zm-.3 2.6v1.1H5.6v1.1h1.1v1.1h1.1v-1.1h1.1v-1.1H7.8v-1.1H6.7Zm9 .1a.85.85 0 1 0 0 1.7.85.85 0 0 0 0-1.7Zm1.6 1.6a.85.85 0 1 0 0 1.7.85.85 0 0 0 0-1.7Z"/></svg>`,
  clock: `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.4" aria-hidden="true"><circle cx="8" cy="8" r="6"/><path d="M8 4.6V8l2.4 1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>`,
  lock: `<svg viewBox="0 0 16 16" width="13" height="13" fill="currentColor" aria-hidden="true"><path d="M4.6 7V5a3.4 3.4 0 0 1 6.8 0v2H12a1 1 0 0 1 1 1v4.8a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V8a1 1 0 0 1 1-1h.6Zm1.4 0h4V5a2 2 0 1 0-4 0v2Z"/></svg>`,
  search: `<svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><circle cx="7" cy="7" r="4.4"/><path d="M10.4 10.4 14 14" stroke-linecap="round"/></svg>`,
  refresh: `<svg viewBox="0 0 16 16" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="M13.4 8a5.4 5.4 0 1 1-1.55-3.8" stroke-linecap="round"/><path d="M13.9 2.4v2.7h-2.7" stroke-linecap="round" stroke-linejoin="round"/></svg>`,
  arrow: `<svg viewBox="0 0 100 100" width="118" height="118" fill="none" stroke="currentColor" stroke-width="5" aria-hidden="true"><path d="M14 50h64M78 50 54 27M78 50 54 73" stroke-linecap="round" stroke-linejoin="round"/></svg>`,
};

// ---- hover popovers (mutators / players) ----------------------------------
// Webviews have no rich native tooltips, so these are custom positioned divs:
// ~1.5s delay for a card's mutators, ~0.5s for a player roster.
let srvPopEl: HTMLDivElement | null = null;
let srvPopTimer: number | undefined;

function srvPopover(): HTMLDivElement {
  if (!srvPopEl) {
    srvPopEl = document.createElement("div");
    srvPopEl.className = "srv-popover";
    srvPopEl.style.display = "none";
    document.body.appendChild(srvPopEl);
  }
  return srvPopEl;
}

function hideSrvPopover(): void {
  if (srvPopTimer !== undefined) {
    clearTimeout(srvPopTimer);
    srvPopTimer = undefined;
  }
  if (srvPopEl) {
    srvPopEl.style.display = "none";
    srvPopEl.dataset.for = "";
  }
}

// Place the popover just under its anchor, clamped to the window bounds.
function placeSrvPopover(el: HTMLElement, anchor: HTMLElement): void {
  const r = anchor.getBoundingClientRect();
  el.style.display = "block";
  const pr = el.getBoundingClientRect();
  let top = r.bottom + 6;
  let left = r.left;
  if (top + pr.height > window.innerHeight - 8) top = r.top - pr.height - 6;
  if (left + pr.width > window.innerWidth - 8) left = window.innerWidth - pr.width - 8;
  el.style.top = `${Math.max(8, top)}px`;
  el.style.left = `${Math.max(8, left)}px`;
}

function openMutatorPopover(anchor: HTMLElement, mutators: string): void {
  const list = mutators
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  if (!list.length) return;
  const el = srvPopover();
  el.dataset.for = "mutators";
  el.innerHTML =
    `<div class="srv-pop-title">Mutators</div>` +
    list.map((m) => `<div class="srv-pop-row">${escape(m)}</div>`).join("");
  placeSrvPopover(el, anchor);
}

async function openPlayersPopover(anchor: HTMLElement, server: string): Promise<void> {
  const el = srvPopover();
  el.dataset.for = server;
  const ids = matchPlayerIds(server);
  const head = `<div class="srv-pop-title">Players</div>`;
  if (!ids.length) {
    el.innerHTML = head + `<div class="srv-pop-row src">No players.</div>`;
    placeSrvPopover(el, anchor);
    return;
  }
  if (!state.ut4?.logged_in) {
    el.innerHTML = head + `<div class="srv-pop-row src">Sign in to see players.</div>`;
    placeSrvPopover(el, anchor);
    return;
  }
  el.innerHTML = head + `<div class="srv-pop-row src">Loading…</div>`;
  placeSrvPopover(el, anchor);
  const missing = ids.filter((id) => !playerNames.has(id));
  if (missing.length) {
    try {
      const resolved = await invoke<{ id: string; name: string }[]>("resolve_player_names", {
        ids: missing,
      });
      for (const r of resolved) playerNames.set(r.id, r.name);
    } catch {
      /* leave the unresolved ids as short hashes */
    }
  }
  // Moved on / hidden while resolving — don't clobber a newer popover.
  if (el.dataset.for !== server || el.style.display === "none") return;
  const names = ids.map((id) => playerNames.get(id) || id.slice(0, 8));
  el.innerHTML = head + names.map((n) => `<div class="srv-pop-row">${escape(n)}</div>`).join("");
  placeSrvPopover(el, anchor);
}

// ---- the two-pane browser -------------------------------------------------
function renderServerList(): void {
  hideSrvPopover();
  const hubs = serverCache.filter((s) => isLobby(s) && srvHasAddr(s));
  const instances = serverCache.filter((s) => !isLobby(s) && srvHasAddr(s));

  // Index hubs for instance->hub resolution: by IP (primary), GUID (fallback).
  const hubByIp = new Map<string, GameServerEntry>();
  const hubByGuid = new Map<string, GameServerEntry>();
  for (const h of hubs) {
    const ip = h.serverAddress ?? "";
    if (ip && !hubByIp.has(ip)) hubByIp.set(ip, h);
    const guid = h.attributes?.UT_HUBGUID_s;
    if (guid) hubByGuid.set(String(guid), h);
  }

  // Bucket each instance under its hub; leftovers are standalone.
  const childrenOf = new Map<GameServerEntry, GameServerEntry[]>();
  const standalone: GameServerEntry[] = [];
  for (const inst of instances) {
    const guid = inst.attributes?.UT_HUBGUID_s;
    const hub =
      (inst.serverAddress ? hubByIp.get(inst.serverAddress) : undefined) ??
      (guid ? hubByGuid.get(String(guid)) : undefined);
    if (hub) {
      const arr = childrenOf.get(hub) ?? [];
      arr.push(inst);
      childrenOf.set(hub, arr);
    } else {
      standalone.push(inst);
    }
  }

  const names = customNameMap(hubs);
  const q = serversSearch.trim().toLowerCase();
  const hubId = (h: GameServerEntry): string => `${h.serverAddress}:${h.serverPort}`;
  const matchId = (s: GameServerEntry): string => `${s.serverAddress}:${s.serverPort}`;

  // Visible hubs: populated (or show-empty), then name-search, busiest first.
  const hubActivity = (h: GameServerEntry): number =>
    (childrenOf.get(h) ?? []).reduce((n, s) => n + srvPlayers(s), 0) + srvPlayers(h);
  const visibleHubs = hubs
    .filter((h) => {
      const kids = childrenOf.get(h) ?? [];
      return serversShowEmpty || srvPlayers(h) > 0 || kids.some((s) => srvPlayers(s) > 0);
    })
    .filter(
      (h) => !q || String(h.attributes?.UT_SERVERNAME_s || h.serverName || "").toLowerCase().includes(q),
    )
    .sort((a, b) => hubActivity(b) - hubActivity(a));

  // Resolve the selected hub (keep selection across re-renders if it persists).
  let selectedHub = visibleHubs.find((h) => hubId(h) === selectedHubId) ?? null;
  if (!selectedHub && visibleHubs.length) selectedHub = visibleHubs[0];
  selectedHubId = selectedHub ? hubId(selectedHub) : null;

  // Detail-pane matches: selected hub's matches (HUBS mode) or all matches
  // (SERVERS mode), filtered by show-empty + search, busiest first.
  const cardMatch = (s: GameServerEntry): boolean => {
    if (!serversShowEmpty && srvPlayers(s) <= 0) return false;
    if (!q) return true;
    const a = s.attributes;
    const hay =
      `${matchTitle(s, names)} ${friendlyMode(String(a?.GAMEMODE_s ?? ""), String(a?.UU_FORCEDMUTATORS_s ?? ""))} ${a?.MAPNAME_s ?? ""}`.toLowerCase();
    return hay.includes(q);
  };
  const detailMatches = (
    serversMode === "servers" ? [...standalone] : selectedHub ? (childrenOf.get(selectedHub) ?? []) : []
  )
    .filter(cardMatch)
    .sort((a, b) => srvPlayers(b) - srvPlayers(a));

  // Default the selected card to the first one shown.
  if (!detailMatches.some((s) => matchId(s) === selectedMatchId)) {
    selectedMatchId = detailMatches.length ? matchId(detailMatches[0]) : null;
  }

  // ---- markup ----
  const hubRow = (h: GameServerEntry): string => {
    const id = hubId(h);
    const name = String(h.attributes?.UT_SERVERNAME_s || h.serverName || "Hub").trim();
    const trust = h.attributes?.UT_SERVERTRUSTLEVEL_i;
    const badge =
      trust === 0 || trust === 1
        ? `<span class="srv-badge" title="${trust === 0 ? "Epic-trusted" : "Trusted"} hub">XP</span>`
        : "";
    const live = (childrenOf.get(h) ?? []).length;
    const players = hubActivity(h);
    return `<div class="srv-hub${id === selectedHubId ? " sel" : ""}" data-hub="${escape(id)}">
        <div class="srv-hub-name">${badge}${escape(name)}</div>
        <div class="srv-hub-stats">
          <span class="srv-stat" title="live matches">${SRV_ICON.pad}${live}</span>
          <span class="srv-stat" title="players">${SRV_ICON.person}${players}</span>
        </div>
      </div>`;
  };

  const card = (s: GameServerEntry): string => {
    const id = matchId(s);
    const a = s.attributes;
    const muts = String(a?.UU_FORCEDMUTATORS_s ?? "");
    const p = srvPlayers(s);
    const max = Number(a?.UT_MAXPLAYERS_i ?? s.maxPublicPlayers ?? 0);
    const full = max > 0 && p >= max;
    const locked = ((a?.UT_SERVERFLAGS_i ?? 0) & 1) !== 0;
    const mode = friendlyMode(String(a?.GAMEMODE_s ?? ""), muts);
    const map = String(a?.MAPNAME_s ?? "");
    const sub = map ? `${mode} in ${map}` : mode;
    const title = matchTitle(s, names);
    const showSub = title !== "";
    const line1 = escape(title || sub);
    let stateText = matchStateLabel(a?.UT_MATCHSTATE_s);
    if (locked && stateText) stateText += " -- private";
    const clock = matchClock(a);
    const countIcon = locked ? SRV_ICON.lock : SRV_ICON.person;
    return `<div class="srv-card${id === selectedMatchId ? " sel" : ""}" data-match="${escape(id)}"${
      locked ? ` data-locked="1"` : ""
    } data-muts="${escape(muts)}">
        <div class="srv-card-main">
          <div class="srv-card-title">${line1}</div>
          ${showSub ? `<div class="srv-card-sub">${escape(sub)}</div>` : ""}
          ${stateText ? `<div class="srv-card-state">${escape(stateText)}</div>` : ""}
        </div>
        <div class="srv-card-side">
          <span class="srv-count ${full ? "srv-count-full" : "srv-count-open"}" data-server="${escape(id)}">${countIcon}${p} / ${max || "?"}</span>
          ${clock ? `<span class="srv-clock">${SRV_ICON.clock}${escape(clock)}</span>` : ""}
        </div>
      </div>`;
  };

  const totalHubs = hubs.length;
  const totalMatches = standalone.length;

  const masterBody =
    serversMode === "servers"
      ? `<div class="srv-empty-arrow">${SRV_ICON.arrow}<div class="src">Matches are listed on the right →</div></div>`
      : `<div class="srv-list">${
          visibleHubs.length
            ? visibleHubs.map(hubRow).join("")
            : `<p class="src" style="padding:10px 12px">No hubs ${serversShowEmpty ? "online" : "with players"} right now.</p>`
        }</div>`;

  const detailBody = `<div class="srv-list"><div class="srv-cards">${
    detailMatches.length
      ? detailMatches.map(card).join("")
      : `<p class="src" style="padding:12px 14px">${
          serversMode === "servers" ? "No matches" : selectedHub ? "No live matches in this hub" : "Select a hub"
        } right now.</p>`
  }</div></div>`;

  const signedOut = state.ut4?.logged_in ? "" : `<span class="warn">sign in on the Home tab to join</span>`;

  serversPanel.classList.add("srv-host");
  serversPanel.innerHTML = `
    <div class="srv-browser">
      <div class="srv-header">
        <div class="srv-toggle">
          <button type="button" data-mode="hubs" class="${serversMode === "hubs" ? "on" : ""}">HUBS (${visibleHubs.length}/${totalHubs})</button>
          <button type="button" data-mode="servers" class="${serversMode === "servers" ? "on" : ""}">SERVERS (${totalMatches})</button>
        </div>
        <label class="srv-search">${SRV_ICON.search}<input id="srv-search" type="text" placeholder="Search…" autocomplete="off" spellcheck="false" value="${escape(serversSearch)}" /></label>
        <label class="srv-show-empty"><input id="srv-empty" type="checkbox" ${serversShowEmpty ? "checked" : ""}/> Show Empty</label>
        <button id="srv-refresh" class="srv-refresh" type="button" title="Refresh">${SRV_ICON.refresh}</button>
      </div>
      <div class="srv-panes">
        <div class="srv-master">
          ${masterBody}
          <div class="srv-actions master">
            <button id="srv-hub-join" type="button"${selectedHub && serversMode === "hubs" ? "" : " disabled"}>Join</button>
            <div id="srv-master-status" class="row-status"></div>
          </div>
        </div>
        <div class="srv-detail">
          ${serversMode === "hubs" ? hubPakPanel(selectedHub) : ""}
          ${detailBody}
          <div class="srv-actions detail">
            <div id="srv-detail-status" class="row-status">${signedOut}</div>
            <button id="srv-match-join" type="button"${selectedMatchId ? "" : " disabled"}>Join</button>
            <button id="srv-match-spectate" type="button"${selectedMatchId ? "" : " disabled"}>Spectate</button>
          </div>
        </div>
      </div>
    </div>`;

  // ---- wiring ----
  const masterStatus = (): HTMLElement | null => document.getElementById("srv-master-status");
  const detailStatus = (): HTMLElement | null => document.getElementById("srv-detail-status");

  // Per-pak "revert" in the hub pak panel — remove the user's newer local copy so
  // a lagging hub serves its own version.
  serversPanel.querySelectorAll<HTMLButtonElement>(".hubpak-revert").forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      const paks = (btn.dataset.paks ?? "").split(",").filter(Boolean);
      if (paks.length) void revertHubPak(paks);
    });
  });

  serversPanel.querySelectorAll<HTMLButtonElement>(".srv-toggle button").forEach((btn) => {
    btn.addEventListener("click", () => {
      const m = btn.dataset.mode === "servers" ? "servers" : "hubs";
      if (m !== serversMode) {
        serversMode = m;
        renderServerList();
      }
    });
  });

  const searchEl = document.getElementById("srv-search") as HTMLInputElement | null;
  searchEl?.addEventListener("input", () => {
    serversSearch = searchEl.value;
    renderServerList();
    const again = document.getElementById("srv-search") as HTMLInputElement | null;
    if (again) {
      again.focus();
      again.selectionStart = again.selectionEnd = again.value.length;
    }
  });

  document.getElementById("srv-empty")?.addEventListener("change", (e) => {
    serversShowEmpty = (e.target as HTMLInputElement).checked;
    renderServerList();
  });
  document.getElementById("srv-refresh")?.addEventListener("click", () => void renderServers());

  // Hub selection (master).
  serversPanel.querySelectorAll<HTMLElement>(".srv-hub").forEach((row) => {
    row.addEventListener("click", () => {
      const id = row.dataset.hub;
      if (!id || id === selectedHubId) return;
      selectedHubId = id;
      selectedMatchId = null;
      renderServerList();
    });
  });

  // Card selection (detail) — lightweight highlight, no full re-render.
  serversPanel.querySelectorAll<HTMLElement>(".srv-card").forEach((cardEl) => {
    const muts = cardEl.dataset.muts || "";
    cardEl.addEventListener("click", () => {
      selectedMatchId = cardEl.dataset.match ?? null;
      serversPanel.querySelectorAll(".srv-card.sel").forEach((c) => c.classList.remove("sel"));
      cardEl.classList.add("sel");
      const j = document.getElementById("srv-match-join") as HTMLButtonElement | null;
      const sp = document.getElementById("srv-match-spectate") as HTMLButtonElement | null;
      if (j) j.disabled = false;
      if (sp) sp.disabled = false;
    });
    // Mutators popover on card hover (~1.5s).
    cardEl.addEventListener("mouseenter", () => {
      hideSrvPopover();
      if (muts) srvPopTimer = window.setTimeout(() => openMutatorPopover(cardEl, muts), 1500);
    });
    cardEl.addEventListener("mouseleave", () => hideSrvPopover());
    // Players popover on the player count (~0.5s) — overrides the mutators timer.
    const count = cardEl.querySelector<HTMLElement>(".srv-count");
    if (count) {
      count.addEventListener("mouseenter", () => {
        if (srvPopTimer !== undefined) clearTimeout(srvPopTimer);
        const server = count.dataset.server || "";
        srvPopTimer = window.setTimeout(() => void openPlayersPopover(count, server), 500);
      });
      count.addEventListener("mouseleave", () => {
        if (srvPopTimer !== undefined) clearTimeout(srvPopTimer);
        hideSrvPopover();
        if (muts) srvPopTimer = window.setTimeout(() => openMutatorPopover(cardEl, muts), 1500);
      });
    }
  });
  serversPanel
    .querySelectorAll<HTMLElement>(".srv-list")
    .forEach((l) => l.addEventListener("scroll", () => hideSrvPopover()));

  // Hub Join (master action) — enters the selected hub's lobby.
  document.getElementById("srv-hub-join")?.addEventListener("click", () => {
    const h = visibleHubs.find((x) => hubId(x) === selectedHubId);
    if (!h) return;
    const server = hubId(h);
    const status = masterStatus();
    const locked = ((h.attributes?.UT_SERVERFLAGS_i ?? 0) & 1) !== 0;
    if (locked) promptServerPassword(status, (pw) => void connectTo(server, pw, status));
    else void connectTo(server, "", status);
  });

  // Match Join / Spectate (detail actions) act on the selected card.
  const selectedMatch = (): GameServerEntry | undefined =>
    instances.find((s) => matchId(s) === selectedMatchId);
  document.getElementById("srv-match-join")?.addEventListener("click", () => {
    const s = selectedMatch();
    if (!s) return;
    const server = matchId(s);
    const status = detailStatus();
    const locked = ((s.attributes?.UT_SERVERFLAGS_i ?? 0) & 1) !== 0;
    if (locked) promptServerPassword(status, (pw) => void connectTo(server, pw, status));
    else void connectTo(server, "", status);
  });
  document.getElementById("srv-match-spectate")?.addEventListener("click", () => {
    const s = selectedMatch();
    if (!s) return;
    const server = matchId(s);
    const status = detailStatus();
    const locked = ((s.attributes?.UT_SERVERFLAGS_i ?? 0) & 1) !== 0;
    if (locked)
      promptServerPassword(status, (pw) => void connectTo(`${server}?SpectatorOnly=1`, pw, status));
    else void connectTo(`${server}?SpectatorOnly=1`, "", status);
  });
}

let pugPollTimer: number | undefined;

function startPugPolling() {
  if (pugPollTimer !== undefined) return;
  pugLastFetch = Date.now();
  pugPollTimer = window.setInterval(() => {
    if (statusPollDue(pugLastFetch, pollEngaged(state.pugStatus?.state))) {
      pugLastFetch = Date.now();
      void pollPugStatus();
    }
  }, POLL_TICK_MS);
}

function stopPugPolling() {
  if (pugPollTimer !== undefined) {
    clearInterval(pugPollTimer);
    pugPollTimer = undefined;
  }
}

// Whether two consecutive pug_status snapshots differ enough to re-render —
// includes the readycheck sub-fields so a ready-count tick refreshes the banner.
function pugStatusChanged(prev: PugStatus | null, next: PugStatus): boolean {
  if (!prev) return true;
  return (
    prev.state !== next.state ||
    prev.server !== next.server ||
    prev.players !== next.players ||
    prev.ready_count !== next.ready_count ||
    prev.you_readied !== next.you_readied
  );
}

async function pollPugStatus() {
  if (!state.launcherToken) return;
  try {
    const next = JSON.parse(
      await invoke<string>("pug_status", { mode: state.igMode, token: state.launcherToken }),
    ) as PugStatus;
    const prev = state.pugStatus;
    state.pugStatus = next;
    updateDiscordPresence();
    // Entering readycheck (the PUG just filled) is the alert moment — fire the
    // desktop notification + chime once, on the transition, so a minimized
    // launcher still grabs the player. The in-app banner then carries it.
    if (next.state === "readycheck" && prev?.state !== "readycheck") {
      void notifyReadycheck(next);
      playReadyChime();
    }
    // Re-render only on a meaningful change, so the 5 s poll doesn't clobber
    // the status line / token input.
    if (pugStatusChanged(prev, next)) {
      renderPug();
      renderHomeReadycheck();
    }
  } catch (err) {
    console.error("pug_status failed:", err);
    // A revoked/invalid token shows up here too (the 5 s poll) — drop it so the
    // poll stops and the UI returns to the link prompt instead of erroring on.
    if (isPugTokenError(err)) handlePugTokenError();
  }
}

// ---- tokenless live-PUG banner poll ---------------------------------------

let livePollTimer: number | undefined;
// Key of the live set currently on screen, so the poll only re-renders the
// banner when the set actually changes (and never clobbers an open picker).
let lastLiveKey = "";

// Always-on (token-independent) poll of the bot's tokenless /live + /queues
// endpoints — powers the HOME live banner and the "queue filling" nudge.
function startLivePolling() {
  if (livePollTimer !== undefined) return;
  liveLastFetch = Date.now();
  queuesLastFetch = Date.now();
  livePollTimer = window.setInterval(() => {
    if (document.hidden) return; // banner + nudge don't poll while minimized
    const now = Date.now();
    if (now - liveLastFetch >= LIVE_POLL_MS) {
      liveLastFetch = now;
      void pollLivePugs();
    }
    if (now - queuesLastFetch >= QUEUES_POLL_MS) {
      queuesLastFetch = now;
      void pollQueues();
    }
  }, POLL_TICK_MS);
}

// Refresh the live-PUG set for the HOME banner. Tokenless, read-only. A failure
// (the /live endpoint not deployed yet, or a transient network blip) is
// swallowed and leaves the banner as-is — it must never break Home.
async function pollLivePugs(): Promise<void> {
  let pugs: SpectatePug[];
  try {
    const st = JSON.parse(await invoke<string>("pug_live")) as {
      state: string;
      pugs?: SpectatePug[];
    };
    pugs = st.state === "live" ? (st.pugs ?? []) : [];
  } catch (err) {
    console.error("pug_live failed:", err);
    return;
  }
  const key = pugs.map((p) => `${p.pug_id}:${p.server}`).join(",");
  state.livePugs = pugs;
  if (key !== lastLiveKey) {
    lastLiveKey = key;
    renderHomeLivePug();
  }
}

// ---- tokenless "queue filling" poll ---------------------------------------

// Surface near-full queues (within this many of max) on HOME. 7/10, 8/10, 6/8,
// 7/8, even 1/2 duel all qualify; 0/N and full queues don't.
const NEAR_FULL_GAP = 3;
let lastQueueKey = "";

function pushIfFilling(acc: FillingQueue[], community: PugCommunity, label: string, r: QueueRow): void {
  const players = r.players ?? 0;
  const max = r.max_players ?? 0;
  if (max > 0 && players > 0 && players < max && max - players <= NEAR_FULL_GAP) {
    acc.push({ community, mode: r.mode, label, players, max });
  }
}

// Poll both communities' tokenless /queues endpoints, keep only the near-full
// ones, and refresh the HOME nudge. Read-only, no token. Each call is swallowed
// on failure (endpoint not deployed / transient blip) so Home never breaks; if
// neither returns a near-full queue the nudge self-clears.
async function pollQueues(): Promise<void> {
  const next: FillingQueue[] = [];
  try {
    const st = JSON.parse(await invoke<string>("pug_queues")) as { queues?: QueueRow[] };
    for (const r of st.queues ?? []) pushIfFilling(next, "Instagib Nation", pugModeLabel(r.mode), r);
  } catch (err) {
    console.error("pug_queues failed:", err);
  }
  if (state.utpugsConfigured) {
    try {
      const st = JSON.parse(await invoke<string>("utpugs_queues")) as { queues?: QueueRow[] };
      for (const r of st.queues ?? []) {
        const key = normUtpugsMode(r.mode);
        pushIfFilling(next, "UTPugs", utpugsModeLabel(key), { ...r, mode: key });
      }
    } catch (err) {
      console.error("utpugs_queues failed:", err);
    }
  }
  const key = next.map((q) => `${q.community}:${q.mode}:${q.players}/${q.max}`).join(",");
  state.fillingQueues = next;
  if (key !== lastQueueKey) {
    lastQueueKey = key;
    renderHomeQueuePug();
  }
}

// ---- recommended add-ons (Advanced) ----------------------------------------

// Optional community add-ons for the selected install. UltiCross stays
// detect + link-out (unzip it yourself); UT4-OpenAL is a one-click install when
// the signed manifest advertises it (download → SHA-256 verify → place), with
// the link-out kept as the fallback for older manifests / Linux. The UT4
// Editor section below is install-independent: a verified 31 GB download +
// unpack for mappers/modders.
async function renderAddons(): Promise<void> {
  const panel = document.getElementById("addons-panel");
  if (!panel) return;
  const root = state.installs[state.selInstall]?.install.root;

  let addons = `<p class="src">Detect a UT4 install to see recommended add-ons.</p>`;
  let hasUlti = false;
  let hasOpenal = false;
  if (root) {
    try {
      hasUlti = await invoke<boolean>("ulticross_status", { root });
    } catch (err) {
      console.error("ulticross_status failed:", err);
    }
    try {
      hasOpenal = await invoke<boolean>("openal_status", { root });
    } catch (err) {
      console.error("openal_status failed:", err);
    }
    const ulti = hasUlti
      ? `<div class="ok">✓ UltiCross detected — fully customizable crosshairs (type <code>ulticross</code> in the console).</div>`
      : `<p class="src">UltiCross not detected — get <button id="get-ulticross" type="button" class="link-btn">UltiCross</button> for fully customizable crosshairs, then unzip it into your <button id="open-plugins" type="button" class="link-btn">Plugins folder</button> and relaunch.</p>`;
    // Paint the manual-install fallback immediately; upgradeOpenalSection swaps
    // in the one-click installer once the manifest answers (that's a network
    // round-trip — the panel must not block on it).
    const openal = hasOpenal
      ? `<div id="openal-detected" class="ok">✓ UT4-OpenAL detected — HRTF positional audio. Enable it via <strong>Settings → Apply competitive config</strong>.</div>`
      : `<div id="openal-section"><p class="src">UT4-OpenAL not detected — get <button id="get-openal-manual" type="button" class="link-btn">UT4-OpenAL</button> for HRTF positional audio, then drop its DLLs into <code>Engine\\Binaries\\Win64</code> and relaunch.</p></div>`;
    addons = `
      <p>Optional community add-ons for this install.</p>
      ${ulti}
      ${openal}`;
  }
  panel.innerHTML = `
    ${addons}
    <div id="editor-install-section"></div>`;
  if (root) {
    document
      .getElementById("get-ulticross")
      ?.addEventListener("click", () => openExternal("https://github.com/aldehir/UT4-UltiCross"));
    document
      .getElementById("open-plugins")
      ?.addEventListener("click", () => void revealPlugins(root));
    document
      .getElementById("get-openal-manual")
      ?.addEventListener("click", () => openExternal("https://github.com/main-exe/UT4-OpenAL/"));
    if (!hasOpenal && platformOs === "windows") void upgradeOpenalSection(root);
  }
  void renderEditorInstall();
}

// Swap the manual OpenAL fallback for the one-click installer once the signed
// manifest confirms it advertises the release. No-ops (leaving the working
// fallback in place) when the fetch fails, the manifest lacks the entry, or
// the panel re-rendered meanwhile.
async function upgradeOpenalSection(root: string): Promise<void> {
  const info = await invoke<AddonEntryInfo>("openal_info").catch(() => null);
  const section = document.getElementById("openal-section");
  if (!section || !info?.available) return;
  const mb = (info.size_bytes / 1e6).toFixed(0);
  section.innerHTML = `
    <div class="game-install">
      <div><strong>UT4-OpenAL</strong> — HRTF positional audio by Main.exe (hear exactly where sounds come from). The launcher downloads the release (${mb} MB), verifies it against the signed manifest, puts the audio module into this install, and stages the OpenAL config for your sound card's sample rate. Your existing <code>alsoft.ini</code> tuning is kept.</div>
      <div class="game-install-actions">
        <label>Sample rate
          <select id="openal-rate">
            <option value="48000" selected>48,000 Hz (Windows default)</option>
            <option value="44100">44,100 Hz</option>
          </select>
        </label>
        <button id="openal-install-btn" type="button">Install UT4-OpenAL</button>
        <button id="get-openal-manual" type="button" class="link-btn">Project page</button>
      </div>
      <div id="openal-install-status" class="launch-status"></div>
    </div>`;
  document
    .getElementById("get-openal-manual")
    ?.addEventListener("click", () => openExternal("https://github.com/main-exe/UT4-OpenAL/"));
  document
    .getElementById("openal-install-btn")
    ?.addEventListener("click", () => void installOpenal(root));
}

// One-click UT4-OpenAL install: download + verify + place, then re-render so
// the ✓ state (and the config panel's audio line) reflect the new detection.
async function installOpenal(root: string): Promise<void> {
  const status = document.getElementById("openal-install-status");
  const rate = Number(
    (document.getElementById("openal-rate") as HTMLSelectElement | null)?.value ?? "48000",
  );
  const btn = document.getElementById("openal-install-btn") as HTMLButtonElement | null;
  if (btn) btn.disabled = true;
  if (status) status.innerHTML = `<span class="src">Downloading and verifying UT4-OpenAL…</span>`;
  try {
    const s = await invoke<{
      binaries_files: number;
      alsoft_ini_written: boolean;
      alsoft_ini_kept: boolean;
      hrtf_files: number;
    }>("install_openal", { root, sampleRate: rate });
    const kept = s.alsoft_ini_kept
      ? " Your existing alsoft.ini tuning was kept."
      : "";
    // Re-render both surfaces; report under the fresh ✓-detected line.
    await renderAddons();
    await renderConfig({
      text: "UT4-OpenAL installed — apply the config to enable its audio module.",
      cls: "ok",
    });
    document
      .getElementById("openal-detected")
      ?.insertAdjacentHTML(
        "afterend",
        `<div class="ok">Installed ${s.binaries_files} audio files + ${s.hrtf_files} HRTF profiles.${kept} Now click <strong>Apply competitive config</strong> in Settings so the game loads it.</div>`,
      );
  } catch (err) {
    if (btn) btn.disabled = false;
    const st = document.getElementById("openal-install-status");
    if (st) st.innerHTML = `<span class="warn">${escape(String(err))}</span>`;
  }
}

// ---- performance config ---------------------------------------------------

async function renderConfig(flash?: { text: string; cls: "ok" | "warn" }) {
  let cfg: ConfigState;
  try {
    cfg = await invoke<ConfigState>("engine_config_state");
  } catch (err) {
    configPanel.innerHTML = `<div class="warn">${escape(String(err))}</div>`;
    return;
  }

  if (!cfg.ini_exists) {
    configPanel.innerHTML = `
      <p>A competitive graphics config tuned for high FPS that still looks decent.</p>
      <div class="warn">No <code>Engine.ini</code> yet — launch UT4 once so it's created, then reopen the launcher.</div>`;
    return;
  }

  let openal = false;
  const root = state.installs[state.selInstall]?.install.root;
  if (root) {
    try {
      openal = await invoke<boolean>("openal_status", { root });
    } catch (err) {
      console.error("openal_status failed:", err);
    }
  }

  const openalDest = root
    ? `<button id="open-openal" type="button" class="link-btn">Engine\\Binaries\\Win64</button>`
    : `<code>Engine\\Binaries\\Win64</code>`;
  const audioLine = openal
    ? `<div class="ok">✓ UT4-OpenAL detected — its audio module will be enabled.</div>`
    : platformOs === "windows" && root
      ? `<p class="src">OpenAL not detected — get <button class="link-btn" data-nav-to="addons" type="button">UT4-OpenAL on the Add-ons tab</button> for HRTF positional audio. The audio override is skipped without it.</p>`
      : `<p class="src">OpenAL not detected — get <button id="get-openal" type="button" class="link-btn">UT4-OpenAL</button> for HRTF positional audio, then drop its DLL into ${openalDest} and relaunch. The audio override is skipped without it.</p>`;

  const t = cfg.tweaks;
  const readOnlyWarn = cfg.engine_ini_read_only
    ? `<div class="alert">⚠ Your <code>Engine.ini</code> is read-only, so Apply can't write to it. If you set it read-only on purpose, leave it; otherwise <button id="cfg-make-writable" type="button" class="link-btn">make it writable</button> and apply again.</div>`
    : "";
  configPanel.innerHTML = `
    <p><strong>Save settings</strong> writes just the controls below into your <code>Engine.ini</code> — nothing else changes. <strong>Apply competitive config</strong> additionally lays down the <strong>complete, competitively-tuned baseline</strong> — high FPS, still readable. Either way your existing <code>Engine.ini</code> is backed up before the first write, and your online/login settings are left untouched. <strong>Close UT4 before saving</strong> — the game rewrites these same keys from its own Settings menu, and resetting your <code>Engine.ini</code> turns frame smoothing back on (an engine default that caps you near 62&nbsp;fps until it's explicitly off).</p>
    ${readOnlyWarn}
    <div class="controls">
      <label>Frame rate cap
        <input id="cfg-fps" type="number" min="0" step="10" value="${escape(String(t.frame_rate_cap))}" />
      </label>
      <label class="cfg-check"><input id="cfg-smooth" type="checkbox"${t.smooth_frame_rate ? " checked" : ""} /> Smooth frame rate</label>
      <label>Display gamma (brightness)
        <input id="cfg-gamma" type="number" min="1" max="5" step="0.1" value="${escape(String(t.display_gamma))}" />
      </label>
      <label class="cfg-check"><input id="cfg-async" type="checkbox"${t.allow_async_loading ? " checked" : ""} /> Allow async loading (Blitz / flag-run safe)</label>
      <label>Alt-tab audio volume (0 = mute, 1 = full)
        <input id="cfg-bgvol" type="number" min="0" max="1" step="0.1" value="${escape(String(t.unfocused_volume))}" />
      </label>
      <label>Max sounds at once (MaxChannels)
        <select id="cfg-voices">${voiceOptions(t.max_audio_channels)}</select>
      </label>
    </div>
    <p class="src">Leave <strong>async loading</strong> on if you play Blitz (flag run) — it avoids load hitches that mode is prone to. Turn it off for slightly faster map loads in other modes (the competitive default).</p>
    <p class="src">UT4 plays at most <strong>MaxChannels</strong> sounds at once and silently drops the quietest extras — in busy fights that can be the jump pad or rocket load behind you. <strong>48</strong> is a safe bump if you notice missing sounds; 64 if they persist. Slightly more CPU per step up.</p>
    ${audioLine}
    <div class="discord-btns">
      <button id="cfg-save" type="button">Save settings</button>
      <button id="cfg-apply" type="button">Apply competitive config</button>
      <button id="cfg-restore" type="button"${cfg.has_backup ? "" : " disabled"}>Restore backup</button>
    </div>
    <div id="cfg-status" class="launch-status"></div>
    <h4 style="margin-top:18px">Repair</h4>
    <p class="src">Crashing on join, or the in-game menus stopped loading? Two game caches can corrupt after a crash (the embedded browser's <code>webcache</code> and the <code>EMS</code> download cache). Deleting them is safe — UT4 rebuilds both on the next launch. Close the game first.</p>
    <div class="discord-btns">
      <button id="cfg-repair-caches" type="button">Repair client caches</button>
    </div>
    <div id="repair-status" class="launch-status"></div>`;

  document.getElementById("get-openal")?.addEventListener("click", () =>
    openExternal("https://github.com/main-exe/UT4-OpenAL/"),
  );
  if (root) {
    document.getElementById("open-openal")?.addEventListener("click", () => void revealOpenal(root));
  }
  document.getElementById("cfg-save")?.addEventListener("click", () => void saveTweaks());
  document.getElementById("cfg-apply")?.addEventListener("click", () => void applyConfig(openal));
  document.getElementById("cfg-restore")?.addEventListener("click", () => void restoreConfig());
  document.getElementById("cfg-make-writable")?.addEventListener("click", () => void doClearReadonly());
  document.getElementById("cfg-repair-caches")?.addEventListener("click", () => void doRepairCaches());

  if (flash) {
    const s = document.getElementById("cfg-status");
    if (s) s.innerHTML = `<span class="${flash.cls}">${escape(flash.text)}</span>`;
  }
}

// The "delete webcache" folklore fix as a button: clears the CEF webcache
// (per-user Documents tree) and the selected install's EMS download cache.
// Both regenerate on next launch; the backend refuses while UT4 runs.
async function doRepairCaches(): Promise<void> {
  const s = document.getElementById("repair-status");
  const ok = await confirm(
    "Delete UT4's web cache and EMS download cache? Both are rebuilt automatically the next time the game starts — saved settings, binds, and stats are not touched. UT4 must be closed.",
    { title: "Repair client caches", kind: "warning", okLabel: "Repair", cancelLabel: "Cancel" },
  );
  if (!ok) return;
  try {
    const out = await invoke<{ webcache: string; ems: string }>("repair_client_caches", {
      root: state.installs[state.selInstall]?.install.root ?? null,
    });
    const cls = out.webcache.startsWith("failed") || out.ems.startsWith("failed") ? "warn" : "ok";
    if (s) s.innerHTML = `<span class="${cls}">webcache: ${escape(out.webcache)} &nbsp;·&nbsp; EMS: ${escape(out.ems)}</span>`;
  } catch (err) {
    if (s) s.innerHTML = `<span class="warn">${escape(String(err))}</span>`;
    console.error("repair_client_caches failed:", err);
  }
}

async function doClearReadonly(): Promise<void> {
  try {
    await invoke("clear_engine_ini_readonly");
    await renderConfig({ text: "Engine.ini is writable now — apply when ready.", cls: "ok" });
  } catch (err) {
    await renderConfig({ text: `Couldn't change it: ${String(err)}`, cls: "warn" });
  }
}

// Open the selected install's NetcodePlus plugin folder (the badge link). The
// backend opens only a real folder under that install root, never an arbitrary
// path — so this can't be turned into opening an attacker-chosen file.
async function revealNcp(root: string): Promise<void> {
  try {
    await invoke("reveal_netcodeplus_folder", { root });
  } catch (err) {
    console.error("reveal_netcodeplus_folder failed:", err);
  }
}

// Open the UT4 Plugins folder (UltiCross's destination). Backend validates the
// install root and opens a real directory only — never an arbitrary path.
async function revealPlugins(root: string): Promise<void> {
  try {
    await invoke("reveal_plugins_folder", { root });
  } catch (err) {
    console.error("reveal_plugins_folder failed:", err);
  }
}

// Open Engine\Binaries\Win64 (UT4-OpenAL's destination — next to the engine
// binaries, not under Plugins). Same backend folder-only safety.
async function revealOpenal(root: string): Promise<void> {
  try {
    await invoke("reveal_openal_folder", { root });
  } catch (err) {
    console.error("reveal_openal_folder failed:", err);
  }
}

// ---- game installer (verified download for users without UT4) --------------

interface GameInstallerInfo {
  available: boolean;
  version: string;
  size_bytes: number;
  dotnet_ok: boolean;
}

let gameDownloadUnlisten: UnlistenFn | null = null;

// (Linux) "Getting UT4" guidance shown when no install is detected. On Linux the
// launcher manages the plugin + launches through Wine/Proton, but does NOT install
// the base game (the UT4Ever installer is a Windows .NET 6 WinForms app). This
// panel points the user at the two working paths and offers a rescan. Verified on
// the dogfooding box: the installer needs .NET 6 in the prefix, and the download's
// inner "UnrealTournament (…).zip" extracts to the game tree (no installer needed).
function renderLinuxGetGame(panel: HTMLElement): void {
  panel.innerHTML = `
    <div class="game-install">
      <div><strong>Don't have UT4 yet?</strong> On Linux the launcher manages NetcodePlus and launches UT4 through <strong>Wine/Proton</strong> — it doesn't install the base game itself. Install UT4 into a Wine/Proton prefix, then this launcher auto-detects it.</div>
      <ol class="linux-getgame" style="margin:0.6rem 0 0.4rem;padding-left:1.2rem;line-height:1.5">
        <li><strong>Get the game</strong> from <button class="card-link" data-extlink="https://ut4ever.org" type="button">ut4ever.org</button> — the community "UT4 Installer" (~10&nbsp;GB).</li>
        <li><strong>Install it with Lutris:</strong> <em>Lutris → + → Install a game from an executable file</em>, point it at <code>UT4_Installer.exe</code>, and use a <strong>Proton</strong> runner (e.g. GE-Proton). It's a <strong>.NET&nbsp;6</strong> app, so the prefix needs two things (verified working): add the runtime with <code>winetricks dotnetdesktop6</code>, and launch with <code>DOTNET_SYSTEM_GLOBALIZATION_INVARIANT=1</code> set (otherwise it dies on a Wine ICU error).</li>
        <li><em>Prefer to skip .NET entirely?</em> The download is just a zip: extract the inner <code>UnrealTournament&nbsp;(…).zip</code> into a folder and add <code>…/Engine/Binaries/Win64/UE4-Win64-Shipping.exe</code> to Lutris under a Proton runner. No installer, no .NET needed.</li>
        <li><strong>Come back and rescan</strong> — the launcher finds your Lutris game automatically. Once it's detected, you can fine-tune the prefix + runner in <em>Settings → Wine&nbsp;/&nbsp;Proton</em>.</li>
      </ol>
      <div class="ut4-integrity" style="margin:0.7rem 0;padding:0.55rem 0.7rem;border:1px solid var(--line,#333);border-radius:6px">
        <div><strong>Trust what you download.</strong> ut4ever is a third-party host, but the launcher pins the installer's SHA-256 in its <em>signed</em> update manifest — so you can confirm your download is byte-for-byte what we signed, regardless of the host.</div>
        <div id="ut4-hash" class="src" style="margin-top:0.35rem;font-family:monospace;word-break:break-all">Loading the expected hash…</div>
        <div class="game-install-actions" style="margin-top:0.4rem">
          <button id="ut4-verify-btn" type="button">Verify a downloaded file…</button>
        </div>
        <div id="ut4-verify-status" class="launch-status"></div>
      </div>
      <div class="game-install-actions">
        <button class="card-link" data-extlink="https://ut4ever.org" type="button">Open ut4ever.org</button>
        <button id="linux-rescan-btn" type="button">Rescan for UT4</button>
      </div>
      <div id="game-install-status" class="launch-status"></div>
    </div>`;

  // Show the signed-manifest hash so users can verify their ut4ever download.
  void invoke<GameInstallerIntegrity>("game_installer_integrity")
    .then((info) => {
      const el = document.getElementById("ut4-hash");
      if (!el) return;
      if (!info.available) {
        el.textContent = "";
        return;
      }
      const gb = (info.size_bytes / 1e9).toFixed(2);
      el.innerHTML = `SHA-256 (v${escape(info.version)}, ${gb}&nbsp;GB):<br>${escape(info.sha256)}`;
    })
    .catch(() => {
      const el = document.getElementById("ut4-hash");
      if (el) el.textContent = "";
    });

  document.getElementById("ut4-verify-btn")?.addEventListener("click", async () => {
    const status = document.getElementById("ut4-verify-status");
    let file: string | string[] | null;
    try {
      file = await open({ directory: false, multiple: false, title: "Select the downloaded UT4 installer zip" });
    } catch (err) {
      console.error("verify dialog open failed:", err);
      return;
    }
    if (typeof file !== "string") return;
    if (status) status.textContent = "Hashing the file… a ~10 GB file takes a moment.";
    try {
      const r = await invoke<VerifyDownloadResult>("verify_game_download", { path: file });
      if (!status) return;
      status.innerHTML = r.matched
        ? `<span style="color:var(--ok,#4caf50)">✓ Verified — this file matches the hash the launcher signed. Safe to install.</span>`
        : `<span class="warn">✗ Does NOT match the signed hash — don't use this file. Got <code>${escape(r.sha256.slice(0, 16))}…</code>, expected <code>${escape(r.expected.slice(0, 16))}…</code></span>`;
    } catch (err) {
      if (status) status.innerHTML = `<span class="warn">Verify failed: ${escape(String(err))}</span>`;
    }
  });

  document.getElementById("linux-rescan-btn")?.addEventListener("click", async () => {
    const status = document.getElementById("game-install-status");
    if (status) status.textContent = "Scanning for a UT4 install…";
    try {
      await refetchInstallsPreservingManual();
      if (state.installs.length > 0) {
        render(); // found it — re-render swaps this panel out for the real UI
      } else if (status) {
        status.innerHTML = `<span class="warn">Still no UT4 install found. Make sure UT4 is added to Lutris (it needs a valid <code>UnrealTournament</code> game folder), then rescan.</span>`;
      }
    } catch (err) {
      if (status) status.innerHTML = `<span class="warn">Rescan failed: ${escape(String(err))}</span>`;
    }
  });
}

async function renderGameInstall(): Promise<void> {
  const panel = document.getElementById("game-install-panel");
  if (!panel) return;
  // The bundled UT4 installer is a Windows-only .NET 6 flow. On Linux we can't run
  // it, so instead of a blank surface, guide the user to install UT4 into a Wine/
  // Proton prefix (which this launcher then auto-detects). Only shown when no
  // install is detected yet — once UT4 is found there's nothing to get.
  if (platformOs !== "windows") {
    if (state.installs.length > 0) {
      panel.innerHTML = "";
      return;
    }
    renderLinuxGetGame(panel);
    return;
  }
  let info: GameInstallerInfo;
  try {
    info = await invoke<GameInstallerInfo>("game_installer_info");
  } catch (err) {
    console.error("game_installer_info failed:", err);
    panel.innerHTML = "";
    return;
  }
  if (!info.available) {
    panel.innerHTML = "";
    return;
  }
  const gb = (info.size_bytes / 1e9).toFixed(1);
  const needGb = ((info.size_bytes * 2) / 1e9).toFixed(0);
  // Gate the ~10 GB download behind the .NET Desktop Runtime check: the UT4
  // installer is a .NET 6 WinForms app, so without the runtime it dies on a
  // cryptic error AFTER the user has already downloaded everything. Require
  // .NET first (with a re-check) so newcomers never hit that wall.
  const actions = info.dotnet_ok
    ? `<div class="game-install-actions">
        <button id="game-download-btn" type="button">Download &amp; Install UT4</button>
      </div>`
    : `<div class="dotnet-note">
        <div><strong>One quick prerequisite first.</strong> The UT4 installer needs the free <strong>.NET Desktop Runtime 6.0 (x64)</strong> from Microsoft — install that, then re-check here. (No point downloading ${gb} GB until it's ready — the installer just errors out without it.)</div>
        <div class="game-install-actions">
          <button id="dotnet-get-btn" type="button" data-extlink="https://dotnet.microsoft.com/download/dotnet/6.0">Get .NET Desktop Runtime 6</button>
          <button id="dotnet-recheck-btn" type="button" class="link-btn">I've installed it — re-check</button>
        </div>
      </div>`;
  panel.innerHTML = `
    <div class="game-install">
      <div><strong>Don't have UT4 yet?</strong> The launcher downloads the community installer (v${escape(
        info.version,
      )}, ${gb} GB) to a folder you choose, verifies it, unpacks it, and runs it. <strong>You pick where UT4 actually installs in the installer's own window</strong> (with a Windows admin prompt) — so for the download just use a normal folder like your <strong>Downloads</strong> (needs ~${needGb} GB free), not Program Files.</div>
      ${actions}
      <div id="game-install-status" class="launch-status"></div>
    </div>`;
  if (info.dotnet_ok) {
    document
      .getElementById("game-download-btn")
      ?.addEventListener("click", () => void startGameDownload());
  } else {
    document
      .getElementById("dotnet-recheck-btn")
      ?.addEventListener("click", () => void recheckDotnet());
  }
}

// Re-check for the .NET Desktop Runtime after the user says they installed it:
// re-render to swap in the Download button when present, or show a targeted hint
// (the usual miss is installing the SDK / ASP.NET runtime, or x86) when not.
async function recheckDotnet(): Promise<void> {
  const status = document.getElementById("game-install-status");
  if (status) status.innerHTML = `<span class="src">Re-checking for .NET…</span>`;
  let ok = false;
  try {
    ok = (await invoke<GameInstallerInfo>("game_installer_info")).dotnet_ok;
  } catch (err) {
    console.error("game_installer_info (re-check) failed:", err);
  }
  if (ok) {
    void renderGameInstall();
  } else {
    const s = document.getElementById("game-install-status");
    if (s)
      s.innerHTML = `<span class="warn">Still not detected. Make sure you installed the <strong>.NET Desktop Runtime 6.0 — x64</strong> (not the SDK or the ASP.NET runtime), then re-check.</span>`;
  }
}

// Phase → verb for the shared progress bar (download → verify → extract).
function gamePhaseLabel(phase: string): string {
  if (phase === "verify") return "Verifying";
  if (phase === "extract") return "Unpacking";
  return "Downloading";
}

// Progress-bar markup, optionally with a Cancel button (download only — the
// unpack step isn't cancellable).
function gameProgressSkeleton(withCancel: boolean): string {
  return `
    <div class="game-progress"><div id="game-progress-bar" class="game-progress-bar"></div></div>
    <div class="src"><span id="game-progress-label">Starting…</span>${
      withCancel ? ` <button id="game-cancel-btn" type="button" class="link-btn">Cancel</button>` : ""
    }</div>`;
}

// Attach the shared `game-download-progress` listener; both download and unpack
// emit it (distinguished by `phase`). Returns the unlisten handle.
async function attachGameProgress(): Promise<UnlistenFn> {
  return await listen<{ phase: string; done: number; total: number }>(
    "game-download-progress",
    (e) => {
      const { phase, done, total } = e.payload;
      const pct = total > 0 ? Math.floor((done / total) * 100) : 0;
      const bar = document.getElementById("game-progress-bar");
      if (bar) bar.style.width = `${pct}%`;
      const label = document.getElementById("game-progress-label");
      if (label) {
        label.textContent = `${gamePhaseLabel(phase)} ${pct}% — ${(done / 1e9).toFixed(2)} / ${(
          total / 1e9
        ).toFixed(2)} GB`;
      }
    },
  );
}

async function startGameDownload(): Promise<void> {
  const status = document.getElementById("game-install-status");
  // The picked folder is only a temporary download/unpack spot — the real
  // install location is chosen later, in the installer itself. Default to the
  // Downloads folder and steer away from protected folders like Program Files.
  const defaultDir = await invoke<string | null>("default_download_dir").catch(() => null);
  const dir = await open({
    directory: true,
    defaultPath: defaultDir ?? undefined,
    title: "Pick a folder to download UT4 into (e.g. Downloads, not Program Files)",
  });
  if (typeof dir !== "string") return; // folder picker cancelled

  // Render the progress UI once; the event listener only updates the bar + label.
  if (status) status.innerHTML = gameProgressSkeleton(true);
  document
    .getElementById("game-cancel-btn")
    ?.addEventListener("click", () => void invoke("cancel_game_download"));

  if (gameDownloadUnlisten) {
    gameDownloadUnlisten();
    gameDownloadUnlisten = null;
  }
  gameDownloadUnlisten = await attachGameProgress();

  try {
    const path = await invoke<string>("download_game_installer", { dir });
    // One-click: roll straight into unpack + launch.
    await startGameInstall(path);
  } catch (err) {
    const msg = String(err);
    if (status) {
      status.innerHTML = msg.includes("cancelled")
        ? `<span class="warn">Download cancelled — it'll resume where it stopped if you start again.</span>`
        : `<span class="warn">${escape(msg)}</span>`;
    }
  } finally {
    if (gameDownloadUnlisten) {
      gameDownloadUnlisten();
      gameDownloadUnlisten = null;
    }
  }
}

// Unpack the verified zip and launch its installer (which self-elevates via the
// Windows admin prompt). Shows "Unpacking" progress, then the launch result.
// Reached automatically right after a verified download (one-click), and from
// its own "Try again" button on failure. `zipPath` is kept only for the
// "Open folder" reveal — the backend resolves the zip to unpack from its own
// verified record (install_game takes no path).
async function startGameInstall(zipPath: string): Promise<void> {
  const status = document.getElementById("game-install-status");
  if (status) status.innerHTML = gameProgressSkeleton(false);

  if (gameDownloadUnlisten) {
    gameDownloadUnlisten();
    gameDownloadUnlisten = null;
  }
  gameDownloadUnlisten = await attachGameProgress();

  try {
    const res = await invoke<{ installer_dir: string; exe_path: string }>("install_game");
    if (status) {
      status.innerHTML = `<span class="ok">✓ Installer launched — follow it in its own window.</span>
        You'll get a Windows admin prompt, and you choose where UT4 installs there.
        <strong>When it finishes, click "Find my install"</strong> so the launcher picks up your new game (or just reopen the launcher). You can delete the downloaded files afterwards.
        <div class="game-install-actions">
          <button id="game-find-install" type="button">Find my install</button>
          <button id="game-reveal-btn" type="button" class="link-btn">Open folder</button>
        </div>`;
    }
    document
      .getElementById("game-find-install")
      ?.addEventListener("click", () => void findMyInstall());
    document
      .getElementById("game-reveal-btn")
      ?.addEventListener("click", () => void invoke("reveal_path", { path: res.exe_path }));
  } catch (err) {
    if (status) {
      status.innerHTML = `<span class="warn">${escape(String(err))}</span>
        <div class="game-install-actions">
          <button id="game-install-btn" type="button">Try again</button>
          <button id="game-reveal-btn" type="button" class="link-btn">Open folder</button>
        </div>`;
      document
        .getElementById("game-install-btn")
        ?.addEventListener("click", () => void startGameInstall(zipPath));
      document
        .getElementById("game-reveal-btn")
        ?.addEventListener("click", () => void invoke("reveal_path", { path: zipPath }));
    }
  } finally {
    if (gameDownloadUnlisten) {
      gameDownloadUnlisten();
      gameDownloadUnlisten = null;
    }
  }
}

// Re-scan for installs after the user finishes the external UT4 installer. The
// installer runs as its own (elevated) process, so the launcher can't know when
// it's done — the user triggers this when it is. It's a soft "reopen":
// `loadAll` re-detects everything and re-renders (pug polling is idempotent, so
// re-running it is safe), so a freshly installed game appears without an actual
// restart.
async function findMyInstall(): Promise<void> {
  const status = document.getElementById("game-install-status");
  if (status) status.innerHTML = `<span class="src">Scanning for your UT4 install…</span>`;
  await loadAll();
  // loadAll rebuilt the panel; report into the fresh status element.
  const s = document.getElementById("game-install-status");
  if (!s) return;
  s.innerHTML =
    state.installs.length > 0
      ? `<span class="ok">✓ Found your UT4 install — it's ready to play on the Home tab.</span>`
      : `<span class="warn">No UT4 install detected yet. If the installer just finished, give it a moment and click again — or use <strong>Settings → Pick install folder</strong> to point at where you installed UT4.</span>`;
}

// ---- UT4 Editor (verified download + unpack, Add-ons tab) -------------------

// Manifest entry facts for an optional add-on download (editor / OpenAL).
interface AddonEntryInfo {
  available: boolean;
  version: string;
  size_bytes: number;
}

let editorDownloadUnlisten: UnlistenFn | null = null;

// The editor's own progress channel (so it can't fight the game installer's
// bar): same phase → verb mapping, editor-prefixed element ids.
async function attachEditorProgress(): Promise<UnlistenFn> {
  return await listen<{ phase: string; done: number; total: number }>(
    "editor-download-progress",
    (e) => {
      const { phase, done, total } = e.payload;
      const pct = total > 0 ? Math.floor((done / total) * 100) : 0;
      const bar = document.getElementById("editor-progress-bar");
      if (bar) bar.style.width = `${pct}%`;
      const label = document.getElementById("editor-progress-label");
      if (label) {
        label.textContent = `${gamePhaseLabel(phase)} ${pct}% — ${(done / 1e9).toFixed(2)} / ${(
          total / 1e9
        ).toFixed(2)} GB`;
      }
    },
  );
}

function editorProgressSkeleton(withCancel: boolean): string {
  return `
    <div class="game-progress"><div id="editor-progress-bar" class="game-progress-bar"></div></div>
    <div class="src"><span id="editor-progress-label">Starting…</span>${
      withCancel
        ? ` <button id="editor-cancel-btn" type="button" class="link-btn">Cancel</button>`
        : ""
    }</div>`;
}

// The UT4 Editor section on the Add-ons tab: a verified ~31 GB download that
// unpacks to a ready-to-run editor tree (no installer exe, no admin prompt).
// Windows-only, and only when the signed manifest advertises it.
async function renderEditorInstall(): Promise<void> {
  const section = document.getElementById("editor-install-section");
  if (!section) return;
  if (platformOs !== "windows") {
    section.innerHTML = "";
    return;
  }
  const info = await invoke<AddonEntryInfo>("editor_installer_info").catch(() => null);
  if (!info?.available) {
    section.innerHTML = "";
    return;
  }
  const gb = (info.size_bytes / 1e9).toFixed(0);
  section.innerHTML = `
    <h2>UT4 Editor</h2>
    <div class="game-install">
      <div><strong>Make maps and mods.</strong> The launcher downloads the community UT4 Editor (${gb} GB) to a folder you choose, verifies it against the signed manifest, and unpacks it there (~38 GB unpacked — the drive needs room for both while it installs). No installer to click through; when it's done you launch <code>UE4Editor.exe</code> straight from here.</div>
      <div class="game-install-actions">
        <button id="editor-download-btn" type="button">Download &amp; unpack the UT4 Editor</button>
      </div>
      <div id="editor-install-status" class="launch-status"></div>
    </div>`;
  document
    .getElementById("editor-download-btn")
    ?.addEventListener("click", () => void startEditorDownload());
}

async function startEditorDownload(): Promise<void> {
  const status = document.getElementById("editor-install-status");
  // In-flight guard: a second click during the multi-hour download would
  // re-open the picker and race the backend (which also single-flights).
  const btn = document.getElementById("editor-download-btn") as HTMLButtonElement | null;
  if (btn?.disabled) return;
  const defaultDir = await invoke<string | null>("default_download_dir").catch(() => null);
  const dir = await open({
    directory: true,
    defaultPath: defaultDir ?? undefined,
    title: "Pick where the UT4 Editor should live (needs ~70 GB free while installing)",
  });
  if (typeof dir !== "string") return; // folder picker cancelled

  if (btn) btn.disabled = true;
  if (status) status.innerHTML = editorProgressSkeleton(true);
  document
    .getElementById("editor-cancel-btn")
    ?.addEventListener("click", () => void invoke("cancel_editor_download"));

  if (editorDownloadUnlisten) {
    editorDownloadUnlisten();
    editorDownloadUnlisten = null;
  }
  editorDownloadUnlisten = await attachEditorProgress();

  try {
    await invoke<string>("download_editor", { dir });
    // One-click: roll straight into the unpack.
    await startEditorInstall();
  } catch (err) {
    const msg = String(err);
    if (status) {
      status.innerHTML = msg.includes("cancelled")
        ? `<span class="warn">Download cancelled — it'll resume where it stopped if you start again.</span>`
        : `<span class="warn">${escape(msg)}</span>`;
    }
  } finally {
    const b = document.getElementById("editor-download-btn") as HTMLButtonElement | null;
    if (b) b.disabled = false;
    if (editorDownloadUnlisten) {
      editorDownloadUnlisten();
      editorDownloadUnlisten = null;
    }
  }
}

// Unpack the verified editor zip beside itself (phase "extract" on the editor
// progress channel), then offer Launch / Open folder. Reached automatically
// after a verified download, and from its own "Try again" on failure.
async function startEditorInstall(): Promise<void> {
  const status = document.getElementById("editor-install-status");
  if (status) status.innerHTML = editorProgressSkeleton(false);

  if (editorDownloadUnlisten) {
    editorDownloadUnlisten();
    editorDownloadUnlisten = null;
  }
  editorDownloadUnlisten = await attachEditorProgress();

  try {
    const res = await invoke<{
      editor_dir: string;
      exe_path: string | null;
      shortcut_created: boolean;
    }>("install_editor");
    if (status) {
      const launchBtn = res.exe_path
        ? `<button id="editor-launch-btn" type="button">Launch the editor</button>`
        : "";
      const shortcut = res.shortcut_created
        ? ` A <strong>UT4 Editor</strong> shortcut was added to your desktop.`
        : "";
      status.innerHTML = `<span class="ok">✓ UT4 Editor installed.</span>${shortcut}
        You can delete the downloaded .zip next to it to get the download's disk space back. First startup takes a while (it compiles shaders).
        <div class="game-install-actions">
          ${launchBtn}
          <button id="editor-open-btn" type="button" class="link-btn">Open folder</button>
        </div>`;
    }
    const reportErr = (err: unknown) => {
      const s = document.getElementById("editor-install-status");
      if (s) s.innerHTML = `<span class="warn">${escape(String(err))}</span>`;
    };
    document
      .getElementById("editor-launch-btn")
      ?.addEventListener("click", () => void invoke("launch_editor").catch(reportErr));
    document
      .getElementById("editor-open-btn")
      ?.addEventListener("click", () =>
        // reveal_path opens the PARENT of the given path — the exe's folder,
        // or (via the joined dummy leaf) the editor dir itself.
        void invoke("reveal_path", { path: res.exe_path ?? `${res.editor_dir}\\x` }).catch(
          reportErr,
        ),
      );
  } catch (err) {
    if (status) {
      status.innerHTML = `<span class="warn">${escape(String(err))}</span>
        <div class="game-install-actions">
          <button id="editor-retry-btn" type="button">Try again</button>
        </div>`;
      document
        .getElementById("editor-retry-btn")
        ?.addEventListener("click", () => void startEditorInstall());
    }
  } finally {
    if (editorDownloadUnlisten) {
      editorDownloadUnlisten();
      editorDownloadUnlisten = null;
    }
  }
}

// Options for the MaxChannels select: the stock/bumped presets, plus the
// current ini value as its own entry when someone hand-tuned something else
// (so rendering the card never silently changes their setting).
function voiceOptions(current: number): string {
  const presets = [32, 48, 64];
  const values = presets.includes(current) ? presets : [...presets, current].sort((a, b) => a - b);
  return values
    .map((v) => {
      const label = v === 32 ? "32 (stock)" : v === 48 ? "48 (recommended bump)" : String(v);
      return `<option value="${v}"${v === current ? " selected" : ""}>${label}</option>`;
    })
    .join("");
}

// The current values of the editable controls, defaults filled in for
// anything unparsable. Shared by Save (knobs only) and Apply (full baseline).
function readTweakInputs() {
  const fps = Number((document.getElementById("cfg-fps") as HTMLInputElement).value);
  const gamma = Number((document.getElementById("cfg-gamma") as HTMLInputElement).value);
  const voices = Number((document.getElementById("cfg-voices") as HTMLSelectElement).value);
  const bgvol = Number((document.getElementById("cfg-bgvol") as HTMLInputElement).value);
  return {
    frameRateCap: Number.isFinite(fps) ? fps : 360,
    smoothFrameRate: (document.getElementById("cfg-smooth") as HTMLInputElement).checked,
    displayGamma: Number.isFinite(gamma) ? gamma : 3,
    allowAsyncLoading: (document.getElementById("cfg-async") as HTMLInputElement).checked,
    maxAudioChannels: Number.isFinite(voices) ? voices : 32,
    unfocusedVolume: Number.isFinite(bgvol) ? bgvol : 0,
  };
}

async function saveTweaks() {
  try {
    await invoke("save_engine_tweaks", readTweakInputs());
    await renderConfig({ text: "Settings saved. Restart UT4 for them to take effect.", cls: "ok" });
  } catch (err) {
    const s = document.getElementById("cfg-status");
    if (s) s.innerHTML = `<span class="warn">${escape(String(err))}</span>`;
    console.error("save_engine_tweaks failed:", err);
  }
}

async function applyConfig(setOpenalAudio: boolean) {
  try {
    await invoke("apply_engine_config", { ...readTweakInputs(), setOpenalAudio });
    await renderConfig({ text: "Applied. Restart UT4 for it to take effect.", cls: "ok" });
  } catch (err) {
    const s = document.getElementById("cfg-status");
    if (s) s.innerHTML = `<span class="warn">${escape(String(err))}</span>`;
    console.error("apply_engine_config failed:", err);
  }
}

async function restoreConfig() {
  try {
    await invoke("restore_engine_config");
    await renderConfig({ text: "Restored your previous Engine.ini.", cls: "ok" });
  } catch (err) {
    const s = document.getElementById("cfg-status");
    if (s) s.innerHTML = `<span class="warn">${escape(String(err))}</span>`;
    console.error("restore_engine_config failed:", err);
  }
}

// Competitive Mod.ini presets card — curated NetcodePlus configs from top
// players, applied through `apply_mod_preset` as a section merge (identity
// and unrelated sections untouched; `.ncpbak` backup once; refused while
// UT4 runs because the game rewrites Mod.ini on exit).
async function renderModIni(flash?: { text: string; cls: "ok" | "warn" }) {
  let st: ModIniState;
  let presets: ModPresetInfo[];
  try {
    st = await invoke<ModIniState>("mod_config_state");
    presets = await invoke<ModPresetInfo[]>("mod_preset_list");
  } catch (err) {
    modiniPanel.innerHTML = `<div class="warn">${escape(String(err))}</div>`;
    return;
  }

  const readOnlyWarn = st.read_only
    ? `<div class="alert">⚠ Your <code>Mod.ini</code> is read-only, so Apply can't write to it. Clear the read-only flag in Explorer first if you want a preset.</div>`
    : "";
  const rows = presets
    .map(
      (p) => `
    <div class="discord-btns"><button type="button" data-preset="${escape(p.id)}">Apply ${escape(p.label)}</button></div>
    <p class="src">${escape(p.blurb)}</p>`,
    )
    .join("");
  modiniPanel.innerHTML = `
    <p>One-click <strong>NetcodePlus <code>Mod.ini</code> presets</strong> from top players — hitsounds, forced models and team colours, gib/ragdoll and visibility settings. A preset replaces only the sections it defines; your identity and any other tweaks are left untouched, and your original is backed up before the first apply. Close UT4 first — the game rewrites <code>Mod.ini</code> on exit.</p>
    ${readOnlyWarn}
    ${rows}
    <div class="discord-btns">
      <button id="modini-restore" type="button"${st.has_backup ? "" : " disabled"}>Restore Mod.ini backup</button>
    </div>
    <div id="modini-status" class="launch-status"></div>`;

  for (const el of modiniPanel.querySelectorAll<HTMLButtonElement>("button[data-preset]")) {
    const id = el.dataset.preset ?? "";
    const label = presets.find((p) => p.id === id)?.label ?? id;
    el.addEventListener("click", () => void applyModPreset(id, label));
  }
  document
    .getElementById("modini-restore")
    ?.addEventListener("click", () => void restoreModIni());

  if (flash) {
    const s = document.getElementById("modini-status");
    if (s) s.innerHTML = `<span class="${flash.cls}">${escape(flash.text)}</span>`;
  }
}

async function applyModPreset(id: string, label: string): Promise<void> {
  try {
    await invoke("apply_mod_preset", { presetId: id });
    await renderModIni({
      text: `Applied ${label}. It takes effect next time UT4 starts.`,
      cls: "ok",
    });
  } catch (err) {
    const s = document.getElementById("modini-status");
    if (s) s.innerHTML = `<span class="warn">${escape(String(err))}</span>`;
    console.error("apply_mod_preset failed:", err);
  }
}

async function restoreModIni(): Promise<void> {
  try {
    await invoke("restore_mod_config");
    await renderModIni({ text: "Restored your previous Mod.ini.", cls: "ok" });
  } catch (err) {
    const s = document.getElementById("modini-status");
    if (s) s.innerHTML = `<span class="warn">${escape(String(err))}</span>`;
    console.error("restore_mod_config failed:", err);
  }
}

async function autoFixMasterServer() {
  // Silent on startup: the known UT4 bug wipes the master-server config,
  // which breaks login and the server browser entirely. Repair it without
  // bothering the user (a launcher should just work) — only log if we did.
  try {
    const changed = await invoke<boolean>("repair_master_server");
    if (changed) console.info("Restored missing master-server config in Engine.ini.");
  } catch (err) {
    console.error("master-server auto-repair failed:", err);
  }
}

// ---- launcher self-update (notify-only) ------------------------------------

// Compares the signed manifest's advertised launcher version to this build (in
// Rust) and, when newer, shows a banner + a button that opens the release page
// so the user downloads + runs the new launcher themselves — there is no
// in-place exe swap (that needs code signing). Surfaces this BEFORE the
// manifest's min_launcher_version hard-gate would reject an old launcher
// outright. Silent when current; a network/verify hiccup just leaves the slot
// empty (the game still launches fine).
async function renderLauncherUpdate(): Promise<void> {
  const panel = document.getElementById("launcher-update-panel");
  if (!panel) return;
  let st: LauncherUpdateResult;
  try {
    st = await invoke<LauncherUpdateResult>("launcher_update_status");
  } catch (err) {
    console.error("launcher_update_status failed:", err);
    panel.innerHTML = "";
    return;
  }
  if (!st.update_available || !st.url) {
    panel.innerHTML = "";
    return;
  }
  const url = st.url;
  const newer = st.available_version ? ` ${escape(st.available_version)}` : "";

  if (!st.can_auto_update) {
    // Notify-only (manifest with no verified download): just link out, exactly
    // as before. The user downloads + runs the new launcher themselves.
    panel.innerHTML = `
      <div class="launcher-update">
        <div class="launcher-update-text">A newer launcher${newer} is available — you have v${escape(
          st.current_version,
        )}. Download and run it to update.</div>
        <button id="launcher-update-btn" type="button">Get the update</button>
      </div>`;
    document
      .getElementById("launcher-update-btn")
      ?.addEventListener("click", () => openExternal(url));
    return;
  }

  // Verified self-update: the launcher downloads the new exe, checks it against
  // the SHA-256 in the signed manifest, and relaunches into it. The manual link
  // stays as a fallback if the in-app apply fails.
  panel.innerHTML = `
    <div class="launcher-update">
      <div class="launcher-update-text">A newer launcher${newer} is available — you have v${escape(
        st.current_version,
      )}. The launcher can download it, verify it against the signed manifest, and restart into it.</div>
      <div class="launcher-update-actions">
        <button id="launcher-update-apply" type="button">Download &amp; apply update</button>
        <button id="launcher-update-manual" type="button" class="link-btn">Download manually</button>
      </div>
      <div id="launcher-update-progress"></div>
    </div>`;
  document
    .getElementById("launcher-update-manual")
    ?.addEventListener("click", () => openExternal(url));
  document
    .getElementById("launcher-update-apply")
    ?.addEventListener("click", () => void applyLauncherUpdate());
}

// Phase → verb for the launcher self-update progress bar.
function launcherPhaseLabel(phase: string): string {
  return phase === "verify" ? "Verifying" : "Downloading";
}

// Download + verify + relaunch into the new launcher (the verified self-update).
// On success the backend spawns the verified new exe and exits, so this normally
// never returns; on failure the error is shown and the "Download manually"
// button (left in the DOM) remains as the fallback.
async function applyLauncherUpdate(): Promise<void> {
  const apply = document.getElementById("launcher-update-apply") as HTMLButtonElement | null;
  const progress = document.getElementById("launcher-update-progress");
  if (apply) apply.disabled = true;
  if (progress) {
    progress.innerHTML = `
      <div class="game-progress"><div id="launcher-progress-bar" class="game-progress-bar"></div></div>
      <div class="src"><span id="launcher-progress-label">Starting…</span></div>`;
  }
  const unlisten = await listen<{ phase: string; done: number; total: number }>(
    "launcher-download-progress",
    (e) => {
      const { phase, done, total } = e.payload;
      const pct = total > 0 ? Math.floor((done / total) * 100) : 0;
      const bar = document.getElementById("launcher-progress-bar");
      if (bar) bar.style.width = `${pct}%`;
      const label = document.getElementById("launcher-progress-label");
      if (label) {
        label.textContent = `${launcherPhaseLabel(phase)} ${pct}% — ${(done / 1e6).toFixed(1)} / ${(
          total / 1e6
        ).toFixed(1)} MB`;
      }
    },
  );
  try {
    await invoke("download_and_apply_launcher_update");
    // Reached only if the process didn't exit (it normally relaunches + exits).
    const label = document.getElementById("launcher-progress-label");
    if (label) label.textContent = "Update verified — restarting…";
  } catch (err) {
    console.error("download_and_apply_launcher_update failed:", err);
    if (progress) progress.innerHTML = `<span class="warn">${escape(String(err))}</span>`;
    if (apply) apply.disabled = false;
  } finally {
    unlisten();
  }
}

// ---- post-update housekeeping (remove old launcher + make a shortcut) ------

// After the user downloads + runs a newer launcher from a new spot, the old exe
// and any shortcut to it are stale. This offers to drop a fresh desktop shortcut
// to the new exe and remove the outdated copy. The backend only ever deletes the
// path IT recorded (never one from here). Silent until an update is detected;
// the card persists across runs until the user removes the old one or dismisses.
async function renderLauncherCleanup(): Promise<void> {
  const panel = document.getElementById("launcher-cleanup-panel");
  if (!panel) return;
  let st: HousekeepingResult;
  try {
    st = await invoke<HousekeepingResult>("launcher_update_housekeeping");
  } catch (err) {
    console.error("launcher_update_housekeeping failed:", err);
    panel.innerHTML = "";
    return;
  }
  if (!st.old_launcher_path) {
    panel.innerHTML = "";
    return;
  }
  // Only offer to fix the shortcut when a stale "UT4 Community Launcher.lnk"
  // actually points at the old exe — not for users who launch the exe directly.
  const shortcutBtn = st.shortcut_needs_update
    ? `<button id="cleanup-shortcut" type="button">Update desktop shortcut</button>`
    : "";
  panel.innerHTML = `
    <div class="cleanup-card">
      <div class="cleanup-text">You're now on v${escape(st.current_version)}. Tidy up the old launcher:</div>
      <div class="src">Previous version still at: ${escape(st.old_launcher_path)}</div>
      <div class="cleanup-actions">
        ${shortcutBtn}
        <button id="cleanup-delete" type="button">Remove old launcher</button>
        <button id="cleanup-dismiss" type="button" class="link-btn">Dismiss</button>
      </div>
      <div id="cleanup-status" class="launch-status"></div>
    </div>`;
  document.getElementById("cleanup-shortcut")?.addEventListener("click", () => void doUpdateShortcut());
  document.getElementById("cleanup-delete")?.addEventListener("click", () => void doDeleteOldLauncher());
  document.getElementById("cleanup-dismiss")?.addEventListener("click", () => void doDismissCleanup());
}

async function doUpdateShortcut(): Promise<void> {
  const status = document.getElementById("cleanup-status");
  try {
    const lnk = await invoke<string>("create_launcher_shortcut");
    if (status) status.innerHTML = `<span class="ok">✓ Desktop shortcut updated.</span>`;
    console.info("shortcut updated:", lnk);
  } catch (err) {
    if (status) status.innerHTML = `<span class="warn">Couldn't update the shortcut: ${escape(String(err))}</span>`;
    console.error("create_launcher_shortcut failed:", err);
  }
}

async function doDeleteOldLauncher(): Promise<void> {
  const ok = await confirm(
    "Remove the old launcher? This deletes the previous version's .exe. Your settings and login stay — they're shared.",
    { title: "Remove old launcher", kind: "warning" },
  );
  if (!ok) return;
  const status = document.getElementById("cleanup-status");
  try {
    const res = await invoke<{ scheduled_for_reboot: boolean }>("delete_old_launcher");
    // Keep the card up so the user can still create a shortcut — only the
    // removal is done. Re-rendering would clear the whole card (the pending
    // old-launcher record is gone now), which is what hid the shortcut button.
    document.getElementById("cleanup-delete")?.remove();
    if (status) {
      status.innerHTML = res.scheduled_for_reboot
        ? `<span class="ok">✓ It's still open, so it'll be removed automatically after your next restart.</span>`
        : `<span class="ok">✓ Old launcher removed.</span>`;
    }
  } catch (err) {
    const msg = String(err);
    // A locked/denied failure means the old launcher is still running and the
    // reboot-delete fallback couldn't be scheduled either — tell the user plainly.
    const locked = /os error (?:5|32)\b|denied/i.test(msg);
    if (status) {
      status.innerHTML = locked
        ? `<span class="warn">Couldn't remove it — the old launcher is probably still open. Close it and try again.</span>`
        : `<span class="warn">Couldn't remove it: ${escape(msg)}</span>`;
    }
    console.error("delete_old_launcher failed:", err);
  }
}

async function doDismissCleanup(): Promise<void> {
  try {
    await invoke("dismiss_launcher_cleanup");
  } catch (err) {
    console.error("dismiss_launcher_cleanup failed:", err);
  }
  void renderLauncherCleanup();
}

// ---- news (shown on the Home dashboard) ------------------------------------

async function renderNews() {
  let items: NewsItem[];
  try {
    items = JSON.parse(await invoke<string>("launcher_news")) as NewsItem[];
  } catch (err) {
    console.error("launcher_news failed:", err);
    newsPanel.innerHTML = "";
    return;
  }
  if (!Array.isArray(items) || items.length === 0) {
    newsPanel.innerHTML = "";
    return;
  }
  newsPanel.innerHTML = `
    <div class="news">
      <div class="news-head">News</div>
      ${items
        .map(
          (n) => `<article class="news-item">
            <div class="news-item-head">${n.pinned ? `<span class="news-pin">★</span> ` : ""}<strong>${escape(
              n.title,
            )}</strong><span class="news-date">${escape(n.date)}</span></div>
            <div class="news-body">${escape(n.body)}</div>
          </article>`,
        )
        .join("")}
    </div>`;
}

// ---- editor installs -------------------------------------------------------

// Set a transient message above the editor list (errors, hints). Empty clears it.
function setEditorMsg(html: string): void {
  const el = document.getElementById("editor-msg");
  if (el) el.innerHTML = html;
}

// Fetch + paint the registered editor installs (with a build-tree row and, per
// install, an async-loaded plugin sync panel). Called when the Editor tab opens
// and after any register/remove/sync.
let editorBuildTree: string | null = null;
async function renderEditor(): Promise<void> {
  const panel = document.getElementById("editor-panel");
  if (!panel) return;
  let installs: EditorInstall[];
  try {
    [installs, editorBuildTree] = await Promise.all([
      invoke<EditorInstall[]>("list_editor_installs"),
      invoke<string | null>("get_build_tree").catch(() => null),
    ]);
  } catch (err) {
    panel.innerHTML = `<div class="warn">Couldn't load editor installs: ${escape(String(err))}</div>`;
    return;
  }
  state.editorInstalls = installs;

  const btLabel = editorBuildTree
    ? `<code>${escape(editorBuildTree)}</code>`
    : `<span class="muted">not set — needed for dev sideload</span>`;
  const buildRow = `
    <div class="status" style="display:flex;justify-content:space-between;align-items:center;gap:12px;margin-bottom:12px">
      <div style="min-width:0"><strong>Build tree</strong> ${btLabel}</div>
      <button type="button" data-ed-action="set-build-tree">${editorBuildTree ? "Change…" : "Set…"}</button>
    </div>`;

  if (installs.length === 0) {
    panel.innerHTML =
      buildRow +
      `<p class="muted">No editor installs registered yet. Click <strong>Register editor…</strong> and choose your UT4 editor folder (the one containing <code>Engine\\</code> and <code>UnrealTournament\\</code>).</p>`;
    return;
  }
  panel.innerHTML =
    buildRow +
    installs
      .map((e, i) => {
        const cl = e.engine_changelist != null ? `CL ${e.engine_changelist}` : "CL unknown";
        const bid = e.engine_build_id ? escape(e.engine_build_id.slice(0, 8)) : "—";
        const root = escape(e.root);
        return `
      <div class="status" style="margin-bottom:8px">
        <div style="display:flex;justify-content:space-between;align-items:center;gap:12px">
          <div style="min-width:0">
            <div><strong>${escape(e.label)}</strong></div>
            <div class="muted" style="font-size:.85em;word-break:break-all"><code>${root}</code></div>
            <div class="muted" style="font-size:.8em">${cl} · engine ${bid}</div>
          </div>
          <div style="display:flex;gap:8px;flex-shrink:0">
            <button type="button" data-ed-action="launch" data-ed-root="${root}">Launch</button>
            <button type="button" class="link-btn" data-ed-action="remove" data-ed-root="${root}">Remove</button>
          </div>
        </div>
        <div id="editor-plugins-${i}" style="margin-top:10px;border-top:1px solid var(--line,#333);padding-top:8px">
          <span class="muted" style="font-size:.85em">Loading plugins…</span>
        </div>
      </div>`;
      })
      .join("");

  installs.forEach((e, i) => void loadEditorPlugins(e.root, i));
}

// Fetch + render one install's editor-plugin sync panel into its container.
async function loadEditorPlugins(root: string, idx: number): Promise<void> {
  const box = document.getElementById(`editor-plugins-${idx}`);
  if (!box) return;
  let rows: EditorPluginStatus[];
  try {
    rows = await invoke<EditorPluginStatus[]>("editor_plugin_status", { root });
  } catch (err) {
    box.innerHTML = `<span class="warn" style="font-size:.85em">Plugin status unavailable: ${escape(String(err))}</span>`;
    return;
  }
  if (rows.length === 0) {
    box.innerHTML = `<span class="muted" style="font-size:.85em">No editor plugins yet — set a <strong>Build tree</strong> above to sideload freshly-built ones, or wait for a signed release.</span>`;
    return;
  }
  const eroot = escape(root);
  const anyActionable = rows.some(
    (r) => r.available_version != null && (r.action === "install" || r.action === "update"),
  );
  box.innerHTML =
    `<div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:6px">
      <strong style="font-size:.9em">Plugins</strong>
      ${
        anyActionable
          ? `<button type="button" data-ep-action="sync-all" data-ep-root="${eroot}">Sync all</button>`
          : `<span class="muted" style="font-size:.8em">all up to date</span>`
      }
    </div>` + rows.map((r) => editorPluginRow(root, r)).join("");
}

// One plugin row: name + a status/action on the right, plus an optional dev
// Sideload button when a build tree is registered.
function editorPluginRow(root: string, r: EditorPluginStatus): string {
  const eroot = escape(root);
  const ep = escape(r.plugin);
  let right: string;
  if (r.action === "update") {
    right = `<span class="muted" style="font-size:.8em">build ${r.installed_version} → ${r.available_version}</span> <button type="button" data-ep-action="sync-one" data-ep-root="${eroot}" data-ep-plugin="${ep}">Update</button>`;
  } else if (r.action === "install") {
    right = `<span class="muted" style="font-size:.8em">not installed · build ${r.available_version}</span> <button type="button" data-ep-action="sync-one" data-ep-root="${eroot}" data-ep-plugin="${ep}">Install</button>`;
  } else if (r.action === "up_to_date") {
    right = `<span class="muted" style="font-size:.8em">✓ ${r.installed_version != null ? `build ${r.installed_version}` : "installed"}${r.source === "local_dev" ? " (dev)" : ""}</span>`;
  } else if (r.action === "pinned_local_dev") {
    right = `<span class="muted" style="font-size:.8em">dev sideload — pinned</span>`;
  } else if (r.action === "present") {
    // On disk already, but not launcher-tracked (e.g. hand-copied earlier).
    right = `<span class="muted" style="font-size:.8em">✓ installed</span>`;
  } else {
    // sideload_only — buildable, not on disk in this editor.
    right = `<span class="muted" style="font-size:.8em">not installed</span>`;
  }
  const mismatch = r.engine_mismatch
    ? ` <span class="warn" style="font-size:.72em" title="built against a different engine build than this install">⚠ engine</span>`
    : "";
  const sideload = r.sideloadable
    ? ` <button type="button" class="link-btn" data-ep-action="sideload" data-ep-root="${eroot}" data-ep-plugin="${ep}">Sideload</button>`
    : "";
  return `<div style="display:flex;justify-content:space-between;align-items:center;gap:8px;padding:2px 0">
    <div style="min-width:0"><strong style="font-size:.9em">${ep}</strong>${mismatch}</div>
    <div style="display:flex;gap:6px;align-items:center;flex-shrink:0">${right}${sideload}</div>
  </div>`;
}

// Pick a folder and register it as the build tree (dev sideload source).
async function pickBuildTree(): Promise<void> {
  setEditorMsg("");
  let picked: string | string[] | null;
  try {
    picked = await open({ directory: true, multiple: false, title: "Choose your UT4 build tree (the folder containing Plugins/)" });
  } catch (err) {
    console.error("dialog open failed:", err);
    return;
  }
  if (!picked) return;
  const path = Array.isArray(picked) ? picked[0] : picked;
  try {
    await invoke("set_build_tree", { path });
    await renderEditor();
  } catch (err) {
    setEditorMsg(`<div class="warn">${escape(String(err))}</div>`);
  }
}

// Install/update signed editor plugins (all, or one) into an install.
async function syncEditorPlugins(root: string, plugins: string[] | null): Promise<void> {
  setEditorMsg(`<div class="status" style="font-size:.85em">Syncing editor plugins…</div>`);
  try {
    const outcomes = await invoke<EditorPluginOutcome[]>("install_editor_plugins", { root, plugins });
    const failed = outcomes.filter((o) => o.result === "failed");
    setEditorMsg(
      failed.length
        ? `<div class="warn">${failed.map((o) => `${escape(o.plugin)}: ${escape(o.detail)}`).join("<br>")}</div>`
        : "",
    );
    await renderEditor();
  } catch (err) {
    setEditorMsg(`<div class="warn">${escape(String(err))}</div>`);
  }
}

// Dev sideload one plugin's freshly-built editor binaries from the build tree.
async function sideloadEditorPluginUi(root: string, plugin: string): Promise<void> {
  setEditorMsg(`<div class="status" style="font-size:.85em">Sideloading ${escape(plugin)} from the build tree…</div>`);
  try {
    await invoke("sideload_editor_plugin", { root, plugin });
    setEditorMsg("");
    await renderEditor();
  } catch (err) {
    setEditorMsg(`<div class="warn">${escape(String(err))}</div>`);
  }
}

// Pick a folder and register it as an editor install.
async function registerEditorInstall(): Promise<void> {
  setEditorMsg("");
  let picked: string | string[] | null;
  try {
    picked = await open({ directory: true, multiple: false, title: "Choose your UT4 editor install folder" });
  } catch (err) {
    console.error("dialog open failed:", err);
    return;
  }
  if (!picked) return;
  const path = Array.isArray(picked) ? picked[0] : picked;
  try {
    await invoke<EditorInstall>("add_editor_install", { path, label: null });
    await renderEditor();
  } catch (err) {
    setEditorMsg(
      `<div class="warn"><code>${escape(path)}</code> isn't a UT4 editor install — it needs <code>Engine/Binaries/Win64/UE4Editor.exe</code> and <code>UnrealTournament/UnrealTournament.uproject</code>. (${escape(String(err))})</div>`,
    );
  }
}

async function launchEditorInstall(root: string): Promise<void> {
  setEditorMsg("");
  try {
    await invoke("launch_editor_install", { root });
  } catch (err) {
    setEditorMsg(`<div class="warn">Couldn't launch the editor: ${escape(String(err))}</div>`);
  }
}

async function removeEditorInstall(root: string): Promise<void> {
  const ok = await confirm(
    `Forget this editor install?\n\n${root}\n\nThis only removes it from the launcher — no files on disk are touched.`,
    { title: "Remove editor install", kind: "warning" },
  );
  if (!ok) return;
  try {
    await invoke("remove_editor_install", { root });
    await renderEditor();
  } catch (err) {
    setEditorMsg(`<div class="warn">${escape(String(err))}</div>`);
  }
}

// Delegated actions — all Editor-tab buttons are re-rendered on each refresh.
document.getElementById("view-editor")?.addEventListener("click", (ev) => {
  const btn = (ev.target as HTMLElement | null)?.closest<HTMLElement>(
    "[data-ed-action], [data-ep-action]",
  );
  if (!btn) return;
  const edAction = btn.dataset.edAction;
  const epAction = btn.dataset.epAction;
  const root = btn.dataset.edRoot ?? btn.dataset.epRoot ?? "";
  if (edAction === "set-build-tree") return void pickBuildTree();
  if (edAction === "launch" && root) return void launchEditorInstall(root);
  if (edAction === "remove" && root) return void removeEditorInstall(root);
  if (epAction === "sync-all" && root) return void syncEditorPlugins(root, null);
  if (epAction === "sync-one" && root) return void syncEditorPlugins(root, [btn.dataset.epPlugin ?? ""]);
  if (epAction === "sideload" && root) return void sideloadEditorPluginUi(root, btn.dataset.epPlugin ?? "");
});
document
  .getElementById("editor-register")
  ?.addEventListener("click", () => void registerEditorInstall());

// ---- tabs ------------------------------------------------------------------

function switchView(name: string): void {
  document
    .querySelectorAll<HTMLElement>(".nav")
    .forEach((b) => b.classList.toggle("active", b.dataset.nav === name));
  document
    .querySelectorAll<HTMLElement>(".view")
    .forEach((v) => v.classList.toggle("active", v.id === `view-${name}`));
  // Load the live server list whenever Servers is opened (cheap + fresh).
  if (name === "servers") void renderServers();
  // Load registered editor installs whenever the Editor tab is opened.
  if (name === "editor") void renderEditor();
}
document.querySelectorAll<HTMLButtonElement>(".nav").forEach((btn) => {
  btn.addEventListener("click", () => switchView(btn.dataset.nav ?? "home"));
});

// Reflect UT4 auth state in the top-bar Sign In button.
function renderTopbarAuth(): void {
  const btn = document.getElementById("topbar-signin");
  const label = document.getElementById("topbar-signin-label");
  if (!btn || !label) return;
  const a = state.ut4;
  if (a?.logged_in) {
    label.textContent = a.display_name ?? a.username ?? "Account";
    btn.classList.add("signed-in");
  } else {
    label.textContent = "Sign In";
    btn.classList.remove("signed-in");
  }
}

// Top-bar PLAY: jump to Home (where launch status + admin warnings render) and
// launch with the current settings. With no install yet, Home shows onboarding.
document.getElementById("topbar-play")?.addEventListener("click", () => {
  switchView("home");
  // Outdated → run the update (same as the hero's UPDATE), never launch into a
  // version-gate kick. Checked at click time so it's right even mid-render.
  if (selectedPluginOutdated()) {
    void doInstallPlugin();
    return;
  }
  if (state.installs.length > 0) void launch();
});
// Top-bar Sign In: open Home and focus the account form (where sign-in lives).
document.getElementById("topbar-signin")?.addEventListener("click", () => {
  switchView("home");
  const userEl = document.getElementById("ut4-user") as HTMLInputElement | null;
  userEl?.scrollIntoView({ block: "center" });
  userEl?.focus();
});

// Custom window controls (the window is frameless — decorations: false).
document.getElementById("win-min")?.addEventListener("click", () => void getCurrentWindow().minimize());
document.getElementById("win-close")?.addEventListener("click", () => void getCurrentWindow().close());

// Maximize / restore toggle. The icon + tooltip reflect the live state, kept in
// sync on every resize so OS-driven maximize (double-click drag region, Win+Up,
// edge snap) flips the glyph too — not just our button.
const winMaxBtn = document.getElementById("win-max");
async function syncMaxIcon(): Promise<void> {
  try {
    const max = await getCurrentWindow().isMaximized();
    winMaxBtn?.classList.toggle("maximized", max);
    if (winMaxBtn) winMaxBtn.title = max ? "Restore" : "Maximize";
  } catch (err) {
    console.error("isMaximized failed:", err);
  }
}
winMaxBtn?.addEventListener("click", () => {
  void getCurrentWindow()
    .toggleMaximize()
    .then(syncMaxIcon)
    .catch((err) => console.error("toggleMaximize failed:", err));
});
void getCurrentWindow().onResized(() => void syncMaxIcon());
void syncMaxIcon();

// Light/dark theme toggle (persisted in localStorage for this prototype).
function applyTheme(theme: string): void {
  document.documentElement.setAttribute("data-theme", theme === "light" ? "light" : "dark");
}
applyTheme(localStorage.getItem("ncp-theme") ?? "dark");
document.getElementById("theme-toggle")?.addEventListener("click", () => {
  const next = document.documentElement.getAttribute("data-theme") === "light" ? "dark" : "light";
  applyTheme(next);
  localStorage.setItem("ncp-theme", next);
});

pickButton.addEventListener("click", () => void pickDir());

// Copy text to the clipboard and flash a brief "Copied!" on the triggering
// button. Uses navigator.clipboard — allowed under our CSP (script-src governs
// script loading, not the Clipboard API; the Tauri webview is a secure context
// and the copy is a user gesture). Used by the HOME UT4ID chip.
async function copyToClipboard(text: string, btn: HTMLButtonElement): Promise<void> {
  const restore = btn.textContent;
  try {
    await navigator.clipboard.writeText(text);
    btn.textContent = "Copied!";
    btn.classList.add("copied");
  } catch {
    btn.textContent = "Copy failed";
  }
  window.setTimeout(() => {
    btn.textContent = restore;
    btn.classList.remove("copied");
  }, 1200);
}

// Delegated click handlers (survive re-renders):
// - the "✓ NetcodePlus installed" badge opens that install's plugin folder;
// - any [data-copy] button copies its value (the HOME UT4ID chip);
// - any [data-extlink] button (the About tab) opens its URL via the https-gated
//   opener.
document.addEventListener("click", (e) => {
  const t = e.target as HTMLElement | null;
  const ncp = t?.closest<HTMLElement>(".ncp-reveal");
  if (ncp?.dataset.root) {
    void revealNcp(ncp.dataset.root);
    return;
  }
  const nav = t?.closest<HTMLElement>("[data-nav-to]");
  if (nav?.dataset.navTo) {
    switchView(nav.dataset.navTo);
    return;
  }
  const copy = t?.closest<HTMLElement>("[data-copy]");
  if (copy?.dataset.copy) {
    void copyToClipboard(copy.dataset.copy, copy as HTMLButtonElement);
    return;
  }
  const ext = t?.closest<HTMLElement>("[data-extlink]");
  if (ext?.dataset.extlink) openExternal(ext.dataset.extlink);
});

// ===========================================================================
// Onboarding / feature discovery — Surfaces 1 (first-run walkthrough),
// 2 (what's new) and 3 (one-time catch-up). Driven entirely by persisted
// seen-IDs (see the Rust `OnboardingState`); version numbers never gate, so a
// user who skips releases still sees each unseen item exactly once. NOTHING here
// writes Engine.ini or game config without an explicit click — the
// competitive-config "Apply" routes through `apply_engine_config`, which backs
// up Engine.ini first (same path as the Settings → Performance config button).
// ===========================================================================

interface OnboardingView {
  fresh_install: boolean;
  completed: boolean;
  seen: string[];
}

type OnbSurface = "firstRun" | "whatsNew" | "catchUp";

interface DiscoveryItem {
  id: string;
  surfaces: OnbSurface[];
  title: string;
  /** Static, trusted HTML for the card body. */
  body: string;
  /** Highlights-only deep-link button to a nav view. */
  navTo?: string;
  navLabel?: string;
  /** Render the framed competitive-config choice (an Apply button) in the body. */
  competitiveConfig?: boolean;
  /** Optional eligibility gate; absent = always eligible. */
  eligible?: () => boolean;
}

// The framed competitive-config copy — identical in Surface 1 and Surface 3 per
// the requirement: explicit choice, stated tradeoff, opt-in, backup called out.
const COMPETITIVE_CONFIG_BODY = `
  <p>UT4's default graphics aren't tuned for competitive play. The launcher can
  apply a competitively-tuned <code>Engine.ini</code> baseline — <strong>high,
  stable FPS and a cleaner, more readable image</strong>, at the cost of some
  <strong>visual fidelity</strong> (simpler effects and lighting). Recommended
  for the smoothest frametimes and netcode feel.</p>
  <p class="src">Your existing <code>Engine.ini</code> is <strong>backed up
  first</strong> and restorable in one click. Nothing changes unless you apply —
  skip if you prefer your current graphics. Playing Blitz/flag-run? You can keep
  async loading on afterwards in <strong>Settings → Performance config</strong>.</p>`;

const DISCOVERY_ITEMS: DiscoveryItem[] = [
  {
    id: "install-location",
    surfaces: ["firstRun"],
    title: "Your UT4 install",
    body: `<p>The launcher auto-detects your Unreal Tournament 4 install and
      launches it in one click. If it didn't find yours, point it at the folder
      in <strong>Settings → Pick install folder</strong> — it looks for
      <code>Engine/Binaries/Win64/</code> and
      <code>UnrealTournament/Content/Paks/</code>.</p>`,
  },
  {
    id: "netcodeplus-plugin",
    surfaces: ["firstRun"],
    title: "NetcodePlus, kept up to date",
    body: `<p>NetcodePlus adds improved netcode and hit registration, Wipeout,
      ElimPlus, an improved CTF, ncHUD and higher FPS. The launcher installs it
      and keeps it current automatically — the update path is cryptographically
      verified end to end, so you never trust a random download.</p>`,
  },
  {
    id: "content-paks",
    surfaces: ["firstRun", "catchUp"],
    title: "Content paks stay current",
    body: `<p>Some servers expect NetcodePlus content paks (sounds, modes). The
      launcher downloads, verifies and keeps them in sync, so you're not left
      with missing sounds when a server doesn't push a pak. No action needed —
      it just keeps them current.</p>`,
  },
  {
    id: "competitive-config",
    surfaces: ["firstRun", "catchUp"],
    title: "Apply the competitive config?",
    body: COMPETITIVE_CONFIG_BODY,
    competitiveConfig: true,
  },
  {
    id: "rich-presence",
    surfaces: ["firstRun", "catchUp"],
    title: "Show your PUG status on Discord",
    body: `<p>Discord Rich Presence broadcasts your PUG state to your Discord
      profile — handy for letting friends see you're queued or in a match.
      New installs have it on by default; the Settings toggle always
      decides.</p>`,
    navTo: "settings",
    navLabel: "Open Settings",
  },
  {
    id: "stats",
    surfaces: ["firstRun", "catchUp"],
    title: "See your stats in the launcher",
    body: `<p>Link your ut4stats.com profile once and your stats panel shows up
      right here — ratings, trends and recent matches without leaving the
      launcher.</p>`,
    navTo: "stats",
    navLabel: "Link your profile",
  },
  // --- "What's new" (Surface 2) -------------------------------------------
  // Add ONE entry per release with a NEW stable id and surfaces: ["whatsNew"].
  // Each shows exactly once to every user who hasn't seen it (gated on the seen
  // set, never the version), so someone who skips two versions still sees each
  // unseen item once and never re-sees a dismissed one. After a release an item
  // can be moved into a firstRun card for newcomers. Leave empty when a release
  // has nothing high-signal — an empty what's-new surface shows nothing.
  // Example:
  //   {
  //     id: "instagib-selector",
  //     surfaces: ["whatsNew"],
  //     title: "Pick your Instagib queue",
  //     body: `<p>Choose iCTF or Elim before you queue …</p>`,
  //   },
  {
    id: "rich-presence-default-on",
    surfaces: ["whatsNew"],
    title: "Show your PUG status on Discord",
    body: `<p>Discord Rich Presence — your queue/match status on your Discord
      profile — is now <strong>on by default for new installs</strong>. Your
      install keeps its current setting, so if you want friends to see when
      you're queued, flip it on in Settings.</p>`,
    navTo: "settings",
    navLabel: "Open Settings",
  },
];

// Vite replaces import.meta.env at build time; cast keeps tsc happy without a
// vite/client reference. true in `npm run dev`, false in a packaged build.
const IS_DEV = Boolean((import.meta as { env?: { DEV?: boolean } }).env?.DEV);

function closeOnboarding(): void {
  document.getElementById("onb-backdrop")?.remove();
}

// Create (or replace) the modal overlay and return its card element to fill in.
function openOnboarding(): HTMLElement {
  closeOnboarding();
  const backdrop = document.createElement("div");
  backdrop.id = "onb-backdrop";
  backdrop.className = "onb-backdrop";
  const card = document.createElement("div");
  card.className = "onb-card";
  card.setAttribute("role", "dialog");
  card.setAttribute("aria-modal", "true");
  backdrop.appendChild(card);
  document.body.appendChild(backdrop);
  return card;
}

// Apply the competitive baseline through the SAME backed-up path as the config
// panel (apply_engine_config → ncp_host::config::apply, which writes .ncpbak
// before touching Engine.ini). Only ever reached from an explicit click.
async function applyCompetitiveConfigFromOnboarding(statusEl: HTMLElement | null): Promise<void> {
  try {
    const cfg = await invoke<{ ini_exists: boolean }>("engine_config_state");
    if (!cfg.ini_exists) {
      if (statusEl)
        statusEl.innerHTML = `<span class="warn">No <code>Engine.ini</code> yet — launch UT4 once so it's created, then apply from Settings → Performance config.</span>`;
      return;
    }
    await invoke("apply_engine_config", {
      frameRateCap: 360,
      smoothFrameRate: false,
      displayGamma: 3,
      allowAsyncLoading: false,
      maxAudioChannels: 32,
      unfocusedVolume: 0,
      setOpenalAudio: false,
    });
    if (statusEl)
      statusEl.innerHTML = `<span class="ok">Applied — your previous Engine.ini was backed up. Restart UT4 for it to take effect.</span>`;
    void renderConfig();
  } catch (err) {
    if (statusEl) statusEl.innerHTML = `<span class="warn">${escape(String(err))}</span>`;
    console.error("apply_engine_config (onboarding) failed:", err);
  }
}

// Surface 1 — first-run guided walkthrough (stepper over the firstRun items).
function runFirstRunWalkthrough(): void {
  const steps = DISCOVERY_ITEMS.filter((i) => i.surfaces.includes("firstRun"));
  if (steps.length === 0) return;
  // On completion, suppress every currently-known discovery item from later
  // catch-up/what's-new — the newcomer has now been introduced to all of them.
  const completedIds = DISCOVERY_ITEMS.filter(
    (i) => i.surfaces.includes("firstRun") || i.surfaces.includes("catchUp"),
  ).map((i) => i.id);
  let idx = 0;
  const card = openOnboarding();

  const finish = (): void => {
    void invoke("complete_onboarding", { ids: completedIds }).catch((err) =>
      console.error("complete_onboarding failed:", err),
    );
    closeOnboarding();
  };

  const renderStep = (): void => {
    const item = steps[idx];
    const isLast = idx === steps.length - 1;
    const dots = steps
      .map((_, i) => `<span class="onb-dot${i === idx ? " on" : ""}"></span>`)
      .join("");
    const advanceLabel = item.competitiveConfig
      ? isLast
        ? "Finish"
        : "Continue →"
      : isLast
        ? "Finish"
        : "Next →";
    card.innerHTML = `
      <div class="onb-head">
        <span class="onb-kicker">Getting started · ${idx + 1} of ${steps.length}</span>
        <button class="onb-x" id="onb-skip" type="button">Skip tour</button>
      </div>
      <h2 class="onb-title">${escape(item.title)}</h2>
      <div class="onb-body">${item.body}</div>
      <div class="onb-status" id="onb-status"></div>
      <div class="onb-foot">
        <div class="onb-dots">${dots}</div>
        <div class="onb-actions">
          ${item.competitiveConfig ? `<button id="onb-apply" type="button">Apply competitive config</button>` : ""}
          ${idx > 0 ? `<button class="link-btn" id="onb-back" type="button">Back</button>` : ""}
          <button id="onb-next" type="button">${advanceLabel}</button>
        </div>
      </div>`;
    document.getElementById("onb-skip")?.addEventListener("click", finish);
    document.getElementById("onb-back")?.addEventListener("click", () => {
      if (idx > 0) {
        idx--;
        renderStep();
      }
    });
    document.getElementById("onb-next")?.addEventListener("click", () => {
      if (isLast) finish();
      else {
        idx++;
        renderStep();
      }
    });
    document
      .getElementById("onb-apply")
      ?.addEventListener("click", () =>
        void applyCompetitiveConfigFromOnboarding(document.getElementById("onb-status")),
      );
  };
  renderStep();
}

// Surfaces 2 + 3 — "what's new" / one-time catch-up. A single dismissible list of
// pending items; closing marks every shown item seen so none re-appear.
function renderHighlights(items: DiscoveryItem[]): void {
  if (items.length === 0) return;
  const shownIds = items.map((i) => i.id);
  const card = openOnboarding();
  const anyNew = items.some((i) => i.surfaces.includes("whatsNew"));
  const heading = anyNew ? "What's new" : "Tips you might have missed";
  const intro = anyNew
    ? "New since you were last here:"
    : "A few launcher features that are easy to miss — set up once, then you're done.";

  const minis = items
    .map((item) => {
      const action = item.competitiveConfig
        ? `<button type="button" data-apply="${item.id}">Apply competitive config</button>`
        : item.navTo
          ? `<button class="link-btn" type="button" data-nav-to="${item.navTo}" data-close-onb="1">${escape(item.navLabel ?? "Open")} →</button>`
          : "";
      return `
        <div class="onb-mini">
          <h3>${escape(item.title)}</h3>
          <div class="onb-body">${item.body}</div>
          <div class="onb-status" id="onb-mini-status-${item.id}"></div>
          ${action ? `<div class="onb-mini-foot">${action}</div>` : ""}
        </div>`;
    })
    .join("");

  card.innerHTML = `
    <div class="onb-head">
      <span class="onb-kicker">${escape(heading)}</span>
      <button class="onb-x" id="onb-done" type="button">Done</button>
    </div>
    <p class="onb-intro">${escape(intro)}</p>
    <div class="onb-minis">${minis}</div>
    <div class="onb-foot onb-foot-end">
      <button id="onb-done2" type="button">Done</button>
    </div>`;

  const done = (): void => {
    // Once dismissed, none re-appear — mark every shown item seen.
    void invoke("mark_features_seen", { ids: shownIds }).catch((err) =>
      console.error("mark_features_seen failed:", err),
    );
    closeOnboarding();
  };
  document.getElementById("onb-done")?.addEventListener("click", done);
  document.getElementById("onb-done2")?.addEventListener("click", done);
  card.querySelectorAll<HTMLElement>("[data-apply]").forEach((btn) => {
    btn.addEventListener("click", () =>
      void applyCompetitiveConfigFromOnboarding(
        document.getElementById(`onb-mini-status-${btn.dataset.apply}`),
      ),
    );
  });
  // A deep-link click counts as "seen" + closes (the document handler at the
  // bottom of this file then switches view via data-nav-to).
  card.querySelectorAll<HTMLElement>("[data-close-onb]").forEach((btn) => {
    btn.addEventListener("click", done);
  });
}

// Decide which surface (if any) to show for this user.
function renderOnboarding(view: OnboardingView): void {
  if (view.fresh_install) {
    runFirstRunWalkthrough();
    return;
  }
  const seen = new Set(view.seen);
  const pending = DISCOVERY_ITEMS.filter(
    (i) =>
      (i.surfaces.includes("whatsNew") || i.surfaces.includes("catchUp")) &&
      !seen.has(i.id) &&
      (i.eligible ? i.eligible() : true),
  );
  renderHighlights(pending);
}

// Reset affordance (decision B+A): a visible button in dev builds; in a packaged
// build it's revealed by tapping the About version label 5× within 2s — a
// deliberate, never-accidental gesture. Reset clears only onboarding state, then
// replays the first-run walkthrough immediately so flows can be re-tested without
// reinstalling (catch-up replays on the next normal launch if not completed).
function wireOnboardingReset(): void {
  const ver = document.getElementById("about-version");
  const host = ver?.closest(".about") ?? ver?.parentElement;
  if (!ver || !host) return;

  const wrap = document.createElement("div");
  wrap.className = "onb-reset";
  wrap.style.display = IS_DEV ? "block" : "none";
  wrap.innerHTML = `<button id="onb-reset-btn" type="button" class="link-btn">Reset tips &amp; onboarding${IS_DEV ? " (dev)" : ""}</button>
    <span class="src onb-reset-note" id="onb-reset-note"></span>`;
  host.appendChild(wrap);

  document.getElementById("onb-reset-btn")?.addEventListener("click", () => {
    void (async () => {
      try {
        await invoke("reset_onboarding");
        const note = document.getElementById("onb-reset-note");
        if (note) note.textContent = " Reset — replaying the first-run walkthrough.";
        runFirstRunWalkthrough();
      } catch (err) {
        console.error("reset_onboarding failed:", err);
      }
    })();
  });

  if (!IS_DEV) {
    let taps = 0;
    let timer = 0;
    ver.addEventListener("click", () => {
      taps += 1;
      window.clearTimeout(timer);
      timer = window.setTimeout(() => (taps = 0), 2000);
      if (taps >= 5) {
        taps = 0;
        wrap.style.display = "block";
      }
    });
  }
}

void showVersion();
void loadAll();
wireOnboardingReset();
