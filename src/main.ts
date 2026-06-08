import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { confirm, open } from "@tauri-apps/plugin-dialog";
import { getCurrent, onOpenUrl } from "@tauri-apps/plugin-deep-link";

interface UtInstall {
  root: string;
  executable: string;
  launch_args: string[];
  content_paks_dir: string;
  mod_paks_dir: string;
}

type NetcodePlusStatus = "installed" | "missing" | "malformed";
type DetectSource = "desktop_shortcut" | "probe" | "manual";

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
}
interface PluginStatusResult {
  plugin_offered: boolean;
  available_version: number | null;
  installs: PluginInstallStatus[];
  any_update_needed: boolean;
}
interface PluginInstallOutcome {
  root: string;
  result: "installed" | "skipped" | "failed";
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
  launch_priority: "normal" | "high";
  affinity_mask_hex: string | null;
  launch_window_action: string;
  ut4stats_playerid: string | null;
  ut4stats_playername: string | null;
  launcher_token: string | null;
  utpugs_launcher_token: string | null;
  discord_presence_enabled?: boolean;
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
}

interface ConfigState {
  ini_exists: boolean;
  has_backup: boolean;
  master_server_ok: boolean;
  tweaks: EngineTweaks;
  engine_ini_read_only: boolean;
}

interface NewsItem {
  title: string;
  body: string;
  pinned: boolean;
  date: string;
}

interface PugStatus {
  state: "idle" | "queued" | "starting" | "live";
  players?: number;
  max_players?: number;
  server?: string;
  password?: string;
  pug_id?: number;
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
const newsPanel = document.getElementById("news-panel")!;
const serversPanel = document.getElementById("servers-panel")!;

const state = {
  installs: [] as DetectedInstall[],
  presets: [] as AffinityPreset[],
  selInstall: 0,
  profileLabel: null as string | null,
  priority: "normal" as "normal" | "high",
  affinityHex: "",
  launchWindowAction: "minimize" as "minimize" | "close" | "none",
  linkedId: null as string | null,
  linkedName: null as string | null,
  launcherToken: null as string | null,
  pugStatus: null as PugStatus | null,
  // UTPugs (autopug) — a second community with its own per-user token, its own
  // status, and several modes (the user picks one). `utpugsConfigured` mirrors
  // the Rust `utpugs_configured()` (false hides the whole UTPugs section).
  utpugsToken: null as string | null,
  utpugsMode: "wipe" as string,
  utpugsStatus: null as PugStatus | null,
  utpugsConfigured: false,
  // Live PUGs anyone can spectate (from the tokenless bot /live endpoint) —
  // drives the HOME "watch to learn" banner. Empty when nothing is live.
  livePugs: [] as SpectatePug[],
  ut4: null as Ut4Auth | null,
  trendMode: "" as string,
  // Discord Rich Presence opt-in (default off; mirrors discord_presence_enabled).
  discordPresence: false,
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
    case "manual":
      return "the folder you picked";
  }
}

function netcodeplusBadge(status: NetcodePlusStatus, root?: string): string {
  switch (status) {
    case "installed":
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

async function doInstallPlugin(force = false): Promise<void> {
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
      // Show the real per-install error and DON'T re-render — re-rendering
      // rebuilds #plugin-panel and would wipe this message before it's read.
      // Keep the button enabled so the user can retry after fixing the cause
      // (e.g. close the game, run as admin for a Program Files install).
      if (status)
        status.innerHTML = `<span class="warn">Install failed: ${escape(
          failed.map((f) => f.detail).join("; "),
        )}</span>`;
      if (btn) btn.disabled = false;
      return;
    }
    if (installed === 0) {
      // Nothing was installed. Distinguish "already up to date" (benign — there
      // were installs, all skipped) from "no UT4 install found to act on" (the
      // real failure for a non-standard install not picked in Settings).
      if (status) {
        status.innerHTML = outcomes.length
          ? `<span class="ok">✓ NetcodePlus is already up to date.</span>`
          : `<span class="warn">No UT4 install was found to set up. Pick your install folder in the Settings tab, then try again.</span>`;
      }
      if (btn) btn.disabled = false;
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
    if (status) status.innerHTML = `<span class="warn">Update failed: ${escape(String(err))}</span>`;
    if (btn) btn.disabled = false;
    console.error("install_plugin failed:", err);
  }
}

// Warn about stray / misplaced NetcodePlus copies (e.g. a hand-install dropped
// into Engine/Plugins, which double-loads). Renders prominent warnings into
// #stray-panel with a confirm-gated "Fix this" that removes the stray. Silent
// when everything is in the right place. Aimed at non-tech-savvy testers, so
// the copy is plain-English and the destructive action requires an OS confirm.
async function renderStrays(): Promise<void> {
  const panel = document.getElementById("stray-panel");
  if (!panel) return;
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
      <div class="stray-title">⚠ NetcodePlus is in the wrong place</div>
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
    `${stray.explanation}\n\nRemove this misplaced copy?\n\n${stray.path}`,
    { title: "Fix NetcodePlus install", kind: "warning" },
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
    if (statusEl) statusEl.innerHTML = `<span class="warn">Couldn't remove: ${escape(String(err))}</span>`;
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
  dotnetAvailable: boolean;
  dotnetOk: boolean;
} = { plugin: null, launcher: null, dotnetAvailable: false, dotnetOk: true };

// Fetch the manifest-backed statuses once, cache them, then refresh the status
// card. Called at startup and after a plugin update. Each source is independent:
// one failing doesn't block the others (the card just omits that line).
async function loadStatusData(): Promise<void> {
  try {
    statusCache.plugin = await invoke<PluginStatusResult>("plugin_status", {
      roots: state.installs.map((d) => d.install.root),
    });
  } catch (err) {
    console.error("plugin_status failed:", err);
  }
  try {
    statusCache.launcher = await invoke<LauncherUpdateResult>("launcher_update_status");
  } catch (err) {
    console.error("launcher_update_status failed:", err);
  }
  try {
    const gi = await invoke<GameInstallerInfo>("game_installer_info");
    statusCache.dotnetAvailable = gi.available;
    statusCache.dotnetOk = gi.dotnet_ok;
  } catch (err) {
    console.error("game_installer_info failed:", err);
  }
  void renderDashStatus();
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
function renderHomeHero() {
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
  homeHero.className = "";
  homeHero.innerHTML = `
    <div class="play-hero">
      <div class="play-hero-overlay">
        <div class="play-title">Unreal Tournament</div>
        <div class="play-sub">${netcodeplusBadge(di.netcodeplus, di.install.root)}</div>
        <div class="hero-cta">
          <button id="launch-btn" type="button" class="launch-primary">▶&nbsp;&nbsp;PLAY</button>
          <span class="hero-meta">${escape(di.install.root)}</span>
        </div>
      </div>
    </div>
    <div id="admin-warn-panel"></div>
    <div id="launch-status" class="launch-status"></div>`;
  (document.getElementById("launch-btn") as HTMLButtonElement | null)?.addEventListener(
    "click",
    () => void launch(),
  );
  void renderAdminWarning();
}

// A bot mode name -> display label (e.g. "ictf" -> "iCTF").
function pugModeLabel(mode?: string): string {
  if (!mode) return "PUG";
  return mode.toLowerCase() === "ictf" ? "iCTF" : mode;
}

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
  const chips = s.ratings.length
    ? s.ratings
        .slice(0, 4)
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
        `<div class="statline"><span class="warn">↑</span><span>${escape(msg)}</span></div>
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
  if (statusCache.dotnetAvailable) {
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
    if (st.state === "live" && st.server) {
      pugLine = `<div class="drow"><span class="grow"><b>iCTF PUG</b><span class="map"> · live now</span></span><button id="dash-pug-connect" type="button" class="btn btn-sm pug-connect">▶ Connect</button></div>`;
    } else if (st.state === "starting") {
      pugLine = `<div class="drow"><span class="grow"><b>iCTF PUG</b><span class="map"> · starting…</span></span></div>`;
    } else if (st.state === "queued") {
      pugLine = `<div class="drow"><span class="grow"><b>iCTF PUG</b><span class="map"> · in queue</span></span><span class="pop">${
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
        </select>
      </label>
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
    </div>`;
  wire();
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
  prioritySel?.addEventListener("change", () => {
    state.priority = prioritySel.value as "normal" | "high";
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
  const profile =
    di.profiles.find((p) => p.label === state.profileLabel) ?? di.profiles[selectedProfileIndex(di)];
  const status = document.getElementById("launch-status")!;

  status.textContent = "Launching…";
  let authArgs: string[];
  try {
    authArgs = await ut4AuthArgs();
  } catch (err) {
    if (handleReloginError(err, null)) return;
    status.innerHTML = `<span class="warn">UT4 login failed: ${escape(String(err))}</span>`;
    return;
  }
  try {
    await invoke("launch_game", {
      executable: di.install.executable,
      args: [...profile.args, ...authArgs],
      priority: state.priority,
      affinityMaskHex: state.affinityHex || null,
      windowAction: state.launchWindowAction,
    });
    persist();
    status.innerHTML = `<span class="ok">Launched: ${escape(profile.label)} (${escape(state.priority)} priority${
      state.affinityHex ? `, affinity ${escape(state.affinityHex)}` : ""
    })</span>`;
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
  let authArgs: string[];
  try {
    authArgs = await ut4AuthArgs();
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
      args: [...profile.args, ...authArgs],
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

// Returns the -AUTH_* args that log the game in via the launcher's session, or
// [] when signed out (the game then shows its own login window). Throws
// "RELOGIN_REQUIRED" when the stored session has expired.
async function ut4AuthArgs(): Promise<string[]> {
  if (!state.ut4?.logged_in) return [];
  const a = await invoke<{ username: string; exchange_code: string; account_id: string }>(
    "ut4_prepare_launch",
  );
  const args = [
    `-AUTH_LOGIN=${a.username}`,
    `-AUTH_PASSWORD=${a.exchange_code}`,
    `-AUTH_TYPE=exchangecode`,
  ];
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
  return args;
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
  const lastVal = (vals: (number | null)[]): number | null =>
    vals.reduce<number | null>((prev, v) => (v != null ? v : prev), null);
  // One accuracy row per weapon that actually has data in this mode (sniper/LG
  // in regular modes, IG in instagib/iCTF); empty weapons are hidden.
  const accRow = (label: string, vals: (number | null)[], color: string): string => {
    const lv = lastVal(vals);
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
  state.priority = prefs.launch_priority === "high" ? "high" : "normal";
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
  state.discordPresence = prefs.discord_presence_enabled ?? false;
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

async function loadAll() {
  try {
    const [installs, presets, prefs, ut4, utpugsConfigured] = await Promise.all([
      invoke<DetectedInstall[]>("detect_installs"),
      invoke<AffinityPreset[]>("affinity_presets"),
      invoke<LauncherState>("load_state"),
      // A credential-store hiccup must not block startup — treat as signed out.
      invoke<Ut4Auth>("ut4_auth_status").catch(() => null),
      // Whether this build has the UTPugs base URL wired in (false hides it).
      invoke<boolean>("utpugs_configured").catch(() => false),
    ]);
    state.installs = installs;
    state.presets = presets;
    state.selInstall = 0;
    state.ut4 = ut4;
    state.utpugsConfigured = utpugsConfigured;
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
    // Arm Discord presence from the persisted opt-in, then push initial state.
    if (state.discordPresence) {
      void invoke("set_discord_presence_enabled", { enabled: true }).catch(() => {});
      updateDiscordPresence();
    }
    // ncp://connect deep links (cold-start URL + live listener).
    void wireDeepLinks();
    void renderConfig();
    void renderAddons();
    void renderLauncherUpdate();
    void renderLauncherCleanup();
    void renderNews();
    void renderGameInstall();
    void loadStatusData();
    renderCommunityLinks();
    renderCommunityVideos();
    // Tokenless live-PUG banner: always polled (no token needed to watch).
    void pollLivePugs();
    startLivePolling();
    if (state.launcherToken) {
      void pollPugStatus();
      startPugPolling();
    }
    if (state.utpugsConfigured && state.utpugsToken) {
      void pollUtpugsStatus();
      startUtpugsPolling();
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

async function pug(action: "joinpug" | "leavepug" | "listpug") {
  // Defensive: never POST an empty token. renderPug already gates the buttons on
  // a token, but this stops a stale render from firing a guaranteed 401.
  if (!state.launcherToken?.trim()) {
    renderPug();
    return;
  }
  const status = document.getElementById("pug-status");
  if (status) status.textContent = action === "listpug" ? "Checking queue…" : "Working…";
  try {
    const raw = await invoke<string>("pug_action", { action, token: state.launcherToken ?? "" });
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
  return s === "live" ? 3 : s === "starting" ? 2 : s === "queued" ? 1 : 0;
}

function updateDiscordPresence(): void {
  if (!state.discordPresence) return;

  // Each community the user is linked to is a candidate; pick the most-advanced
  // state so the profile reflects whichever PUG actually matters right now.
  const candidates: { community: string; mode: string; st: PugStatus }[] = [];
  if (state.launcherToken && state.pugStatus) {
    candidates.push({ community: "Instagib Nation", mode: "ictf", st: state.pugStatus });
  }
  if (state.utpugsConfigured && state.utpugsToken && state.utpugsStatus) {
    candidates.push({ community: "UTPugs", mode: state.utpugsMode, st: state.utpugsStatus });
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
      <p>To queue iCTF PUGs here, run <code>/launchertoken</code> in the Instagib Nation Discord and paste the token it DMs you:</p>
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
  const st = state.pugStatus;
  if (st && st.state === "live" && st.server) {
    pugControls.innerHTML = `
      <p class="ok">🎮 Your iCTF PUG is live!</p>
      <p class="src">Server <code>${escape(st.server)}</code>${
        st.password ? ` · password <code>${escape(st.password)}</code>` : ""
      }</p>
      <button id="pug-connect" type="button" class="launch-primary pug-connect">▶&nbsp;&nbsp;Connect to PUG</button>
      <button id="pug-token-clear" type="button" class="link-btn">change token</button>
      <div id="pug-status" class="launch-status"></div>`;
    document
      .getElementById("pug-connect")
      ?.addEventListener("click", () => void connectToPug(st.server ?? "", st.password ?? ""));
    document.getElementById("pug-token-clear")?.addEventListener("click", () => void saveLauncherToken(null));
    return;
  }
  if (st && st.state === "starting") {
    // PUG is live but the game server is still spinning up (NYC ~90s). Show a
    // disabled button; the 5 s poll re-renders the moment it flips to "live",
    // so the user can't launch into a server that isn't listening yet.
    pugControls.innerHTML = `
      <p class="ok">🛰️ Your iCTF PUG is starting…</p>
      <p class="src">Server spinning up — Connect unlocks the moment it's ready (~90s).</p>
      <button type="button" class="launch-primary pug-connect" disabled>▶&nbsp;&nbsp;Starting…</button>
      <button id="pug-token-clear" type="button" class="link-btn">change token</button>
      <div id="pug-status" class="launch-status"></div>`;
    document.getElementById("pug-token-clear")?.addEventListener("click", () => void saveLauncherToken(null));
    return;
  }

  const queueLine =
    st && st.state === "queued"
      ? `In queue — ${st.players ?? 0}/${st.max_players ?? 10}`
      : "Queue for iCTF";
  pugControls.innerHTML = `
    <p>${queueLine} <button id="pug-token-clear" type="button" class="link-btn">change token</button></p>
    <div class="discord-btns">
      <button id="pug-join" type="button">Join iCTF PUG</button>
      <button id="pug-leave" type="button">Leave</button>
      <button id="pug-refresh" type="button">Queue status</button>
      <button id="pug-spectate" type="button">Spectate live game</button>
    </div>
    <div id="pug-status" class="launch-status"></div>`;
  document.getElementById("pug-join")?.addEventListener("click", () => void pug("joinpug"));
  document.getElementById("pug-leave")?.addEventListener("click", () => void pug("leavepug"));
  document.getElementById("pug-refresh")?.addEventListener("click", () => void pug("listpug"));
  document.getElementById("pug-spectate")?.addEventListener("click", () => void spectate());
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
    let authArgs: string[];
    try {
      authArgs = await ut4AuthArgs();
    } catch (err) {
      if (handleReloginError(err, status)) return;
      if (status) status.innerHTML = `<span class="warn">UT4 login failed: ${escape(String(err))}</span>`;
      return;
    }
    const args = [...profile.args, ...authArgs, `-ncpconnect=${connectUrl}`];
    if (status) status.textContent = "Connecting…";
    try {
      await invoke("launch_game", {
        executable: di.install.executable,
        args,
        priority: state.priority,
        affinityMaskHex: state.affinityHex || null,
        windowAction: state.launchWindowAction,
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

async function handleConnectUri(raw: string): Promise<void> {
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

  const ok = await confirm(`${spectate ? "Spectate" : "Connect to"} ${server}?`, {
    title: "UT4 Community Launcher",
    kind: "warning",
  });
  if (!ok) return;

  // The link may have arrived while we were minimized — surface the window.
  try {
    await getCurrentWindow().unminimize();
    await getCurrentWindow().setFocus();
  } catch {
    /* non-fatal */
  }

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
  if (status) status.textContent = action === "listpug" ? "Checking queue…" : "Working…";
  try {
    const raw = await invoke<string>("utpugs_action", {
      action,
      mode: state.utpugsMode,
      token: state.utpugsToken ?? "",
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
    const queueLine =
      st && st.state === "queued"
        ? `In queue — ${st.players ?? 0}/${st.max_players ?? 10}`
        : `Queue for ${escape(modeLabel)}`;
    block = `
      <p>${queueLine}</p>
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

let utpugsPollTimer: number | undefined;

function startUtpugsPolling() {
  if (utpugsPollTimer !== undefined) return;
  utpugsPollTimer = window.setInterval(() => void pollUtpugsStatus(), 5000);
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
    if (!prev || prev.state !== next.state || prev.server !== next.server || prev.players !== next.players) {
      renderUtpugs();
    }
  } catch (err) {
    console.error("utpugs_status failed:", err);
    if (isPugTokenError(err)) handleUtpugsTokenError();
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
// "<owner>'s match", else "" (the card then promotes "<Mode> in <Map>").
function matchTitle(inst: GameServerEntry, names: Map<string, string>): string {
  const guid = String(inst.attributes?.UT_SERVERINSTANCEGUID_s ?? "").toUpperCase();
  const custom = guid ? names.get(guid) : undefined;
  if (custom && custom.trim()) return custom.trim();
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
  pugPollTimer = window.setInterval(() => void pollPugStatus(), 5000);
}

function stopPugPolling() {
  if (pugPollTimer !== undefined) {
    clearInterval(pugPollTimer);
    pugPollTimer = undefined;
  }
}

async function pollPugStatus() {
  if (!state.launcherToken) return;
  try {
    const next = JSON.parse(await invoke<string>("pug_status", { token: state.launcherToken })) as PugStatus;
    const prev = state.pugStatus;
    state.pugStatus = next;
    updateDiscordPresence();
    // Re-render only on a meaningful change, so the 5 s poll doesn't clobber
    // the status line / token input.
    if (!prev || prev.state !== next.state || prev.server !== next.server || prev.players !== next.players) {
      renderPug();
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

// Always-on (token-independent) poll of the bot's tokenless /live endpoint.
function startLivePolling() {
  if (livePollTimer !== undefined) return;
  livePollTimer = window.setInterval(() => void pollLivePugs(), 8000);
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

// ---- recommended add-ons (Advanced) ----------------------------------------

// Recommends optional community plugins for the selected install, mirroring the
// UT4-OpenAL recommendation: detect the shipping DLL and, if it's missing, link
// out to the project so the player installs it themselves (the launcher never
// installs these). Currently just UltiCross — parameterized custom crosshairs.
async function renderAddons(): Promise<void> {
  const panel = document.getElementById("addons-panel");
  if (!panel) return;
  const root = state.installs[state.selInstall]?.install.root;
  if (!root) {
    panel.innerHTML = `<p class="src">Detect a UT4 install to see recommended add-ons.</p>`;
    return;
  }
  let hasUlti = false;
  try {
    hasUlti = await invoke<boolean>("ulticross_status", { root });
  } catch (err) {
    console.error("ulticross_status failed:", err);
    panel.innerHTML = "";
    return;
  }
  const ulti = hasUlti
    ? `<div class="ok">✓ UltiCross detected — fully customizable crosshairs (type <code>ulticross</code> in the console).</div>`
    : `<p class="src">UltiCross not detected — get <button id="get-ulticross" type="button" class="link-btn">UltiCross</button> for fully customizable crosshairs, then unzip it into your <button id="open-plugins" type="button" class="link-btn">Plugins folder</button> and relaunch.</p>`;
  panel.innerHTML = `
    <p>Optional community plugins for this install — the launcher only checks whether you have them.</p>
    ${ulti}`;
  document
    .getElementById("get-ulticross")
    ?.addEventListener("click", () => openExternal("https://github.com/aldehir/UT4-UltiCross"));
  document.getElementById("open-plugins")?.addEventListener("click", () => void revealPlugins(root));
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
    : `<p class="src">OpenAL not detected — get <button id="get-openal" type="button" class="link-btn">UT4-OpenAL</button> for HRTF positional audio, then drop its DLL into ${openalDest} and relaunch. The audio override is skipped without it.</p>`;

  const t = cfg.tweaks;
  const readOnlyWarn = cfg.engine_ini_read_only
    ? `<div class="alert">⚠ Your <code>Engine.ini</code> is read-only, so Apply can't write to it. If you set it read-only on purpose, leave it; otherwise <button id="cfg-make-writable" type="button" class="link-btn">make it writable</button> and apply again.</div>`
    : "";
  configPanel.innerHTML = `
    <p>Applies a <strong>complete, competitively-tuned <code>Engine.ini</code> baseline</strong> — high FPS, still readable. The controls below let you <strong>customize a few parts</strong> of that config; the rest is applied as-is. Your existing <code>Engine.ini</code> is backed up before the first apply, and your online/login settings are left untouched.</p>
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
    </div>
    <p class="src">Leave <strong>async loading</strong> on if you play Blitz (flag run) — it avoids load hitches that mode is prone to. Turn it off for slightly faster map loads in other modes (the competitive default).</p>
    ${audioLine}
    <div class="discord-btns">
      <button id="cfg-apply" type="button">Apply competitive config</button>
      <button id="cfg-restore" type="button"${cfg.has_backup ? "" : " disabled"}>Restore backup</button>
    </div>
    <div id="cfg-status" class="launch-status"></div>`;

  document.getElementById("get-openal")?.addEventListener("click", () =>
    openExternal("https://github.com/main-exe/UT4-OpenAL/"),
  );
  if (root) {
    document.getElementById("open-openal")?.addEventListener("click", () => void revealOpenal(root));
  }
  document.getElementById("cfg-apply")?.addEventListener("click", () => void applyConfig(openal));
  document.getElementById("cfg-restore")?.addEventListener("click", () => void restoreConfig());
  document.getElementById("cfg-make-writable")?.addEventListener("click", () => void doClearReadonly());

  if (flash) {
    const s = document.getElementById("cfg-status");
    if (s) s.innerHTML = `<span class="${flash.cls}">${escape(flash.text)}</span>`;
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

async function renderGameInstall(): Promise<void> {
  const panel = document.getElementById("game-install-panel");
  if (!panel) return;
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

async function applyConfig(setOpenalAudio: boolean) {
  const fps = Number((document.getElementById("cfg-fps") as HTMLInputElement).value);
  const gamma = Number((document.getElementById("cfg-gamma") as HTMLInputElement).value);
  const smooth = (document.getElementById("cfg-smooth") as HTMLInputElement).checked;
  const allowAsync = (document.getElementById("cfg-async") as HTMLInputElement).checked;
  try {
    await invoke("apply_engine_config", {
      frameRateCap: Number.isFinite(fps) ? fps : 360,
      smoothFrameRate: smooth,
      displayGamma: Number.isFinite(gamma) ? gamma : 3,
      allowAsyncLoading: allowAsync,
      setOpenalAudio,
    });
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

void showVersion();
void loadAll();
