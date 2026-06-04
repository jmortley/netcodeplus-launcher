import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { confirm, open } from "@tauri-apps/plugin-dialog";

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

// Notify-only launcher self-update status from `launcher_update_status`.
interface LauncherUpdateResult {
  update_available: boolean;
  current_version: string;
  available_version: string | null;
  url: string | null;
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
  accuracy: { date: string | null; sniper: number | null; lg: number | null }[];
  form: { games: number; wins: number; losses: number; streak: number; results: string[] };
  rating: { date: string | null; rating: number }[];
}

// The Stats "Trends" mode selector — the six modes from the ut4stats game-mode
// dropdown. Keys match the player_trends endpoint's mode keys.
const TREND_MODES: { key: string; label: string }[] = [
  { key: "elimplus", label: "Team Arena (ElimPlus)" },
  { key: "ctf", label: "CTF" },
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
  iCTF: "ctf",
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
  ut4: null as Ut4Auth | null,
  trendMode: "" as string,
};

function escape(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
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

async function doInstallPlugin(): Promise<void> {
  const btn = document.getElementById("plugin-update-btn") as HTMLButtonElement | null;
  const status = document.getElementById("plugin-status");
  if (btn) btn.disabled = true;
  if (status) status.textContent = "Downloading and installing… this can take a moment.";
  try {
    const outcomes = await invoke<PluginInstallOutcome[]>("install_plugin");
    const installed = outcomes.filter((o) => o.result === "installed").length;
    const failed = outcomes.filter((o) => o.result === "failed");
    if (failed.length) {
      // Show the real per-install error and DON'T re-render — re-rendering
      // rebuilds #plugin-panel and would wipe this message before it's read.
      // Keep the button enabled so the user can retry after fixing the cause
      // (e.g. close the game, run as admin for a Program Files install).
      if (status)
        status.innerHTML = `<span class="warn">Update failed: ${escape(
          failed.map((f) => f.detail).join("; "),
        )}</span>`;
      if (btn) btn.disabled = false;
      return;
    }
    if (status) {
      status.innerHTML = `<span class="ok">✓ NetcodePlus updated in ${installed} install${installed === 1 ? "" : "s"}.</span>`;
    }
    // Success — re-detect so the install badges reflect the new state, then
    // refresh the status card: loadStatusData re-fetches plugin_status and
    // re-renders #dash-status, so its "update available" line flips to
    // "up to date" once the install lands.
    state.installs = await invoke<DetectedInstall[]>("detect_installs");
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
    // reflect the fixed state.
    state.installs = await invoke<DetectedInstall[]>("detect_installs");
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
    statusCache.plugin = await invoke<PluginStatusResult>("plugin_status");
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
  renderHomeHero();
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
  const p = statusCache.plugin;
  if (p && p.plugin_offered) {
    const ver = p.available_version != null ? ` (build ${p.available_version})` : "";
    if (p.any_update_needed) {
      pluginUpdate = true;
      const n = p.installs.filter((i) => i.action === "install" || i.action === "update").length;
      lines.push(
        `<div class="statline"><span class="warn">↑</span><span>NetcodePlus update available${escape(
          ver,
        )} — ${n} install${n === 1 ? "" : "s"}.</span></div>
        <button id="plugin-update-btn" type="button" class="btn btn-sm">Update NetcodePlus</button>
        <div id="plugin-status" class="launch-status"></div>`,
      );
    } else {
      lines.push(
        `<div class="statline"><span class="ok">✓</span><span>NetcodePlus up to date${escape(ver)}.</span></div>`,
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
      executable: di.install.executable,
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
  state.ut4 = { logged_in: false, username: null, display_name: null };
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
  state.ut4 = { logged_in: false, username: null, display_name: null };
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
  const sniperVals = acc.map((a) => a.sniper);
  const lgVals = acc.map((a) => a.lg);
  const lastVal = (vals: (number | null)[]): number | null =>
    vals.reduce<number | null>((prev, v) => (v != null ? v : prev), null);
  const sLast = lastVal(sniperVals);
  const lLast = lastVal(lgVals);

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

  const accBlock = acc.length
    ? `<div class="trend-row"><span class="trend-label">Sniper</span>${sparkline(
        sniperVals,
        "#f5a623",
      )}<span class="trend-val">${sLast != null ? `${sLast}%` : "—"}</span></div>
       <div class="trend-row"><span class="trend-label">Lightning</span>${sparkline(
         lgVals,
         "#4f8bff",
       )}<span class="trend-val">${lLast != null ? `${lLast}%` : "—"}</span></div>`
    : `<div class="src">No sniper/lightning shots recorded in this mode.</div>`;

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
    const [installs, presets, prefs, ut4] = await Promise.all([
      invoke<DetectedInstall[]>("detect_installs"),
      invoke<AffinityPreset[]>("affinity_presets"),
      invoke<LauncherState>("load_state"),
      // A credential-store hiccup must not block startup — treat as signed out.
      invoke<Ut4Auth>("ut4_auth_status").catch(() => null),
    ]);
    state.installs = installs;
    state.presets = presets;
    state.selInstall = 0;
    state.ut4 = ut4;
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
    void renderConfig();
    void renderAddons();
    void renderLauncherUpdate();
    void renderLauncherCleanup();
    void renderNews();
    void renderGameInstall();
    void loadStatusData();
    renderCommunityLinks();
    if (state.launcherToken) {
      void pollPugStatus();
      startPugPolling();
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
  if (token) {
    void pollPugStatus();
    startPugPolling();
  } else {
    stopPugPolling();
  }
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
}

async function connectToPug(server: string, password: string) {
  await connectTo(server, password, document.getElementById("pug-status"));
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

// Caret handler: toggle a match's roster in its row-status slot. Names need the
// signed-in user's session (the account endpoint is auth-gated), so signed-out
// users get a prompt to sign in instead.
async function toggleRoster(server: string, slot: HTMLElement | null) {
  if (!slot) return;
  if (slot.dataset.roster === "1") {
    slot.dataset.roster = "";
    slot.innerHTML = "";
    return;
  }
  slot.dataset.roster = "1";
  const ids = matchPlayerIds(server);
  if (!ids.length) {
    slot.innerHTML = `<span class="src">No players to list.</span>`;
    return;
  }
  if (!state.ut4?.logged_in) {
    slot.innerHTML = `<span class="src">Sign in on the Home tab to see who's playing.</span>`;
    return;
  }
  slot.innerHTML = `<span class="src">Loading players…</span>`;
  const missing = ids.filter((id) => !playerNames.has(id));
  if (missing.length) {
    try {
      const resolved = await invoke<{ id: string; name: string }[]>("resolve_player_names", {
        ids: missing,
      });
      for (const r of resolved) playerNames.set(r.id, r.name);
    } catch (err) {
      const msg = String(err);
      slot.innerHTML = msg.includes("RELOGIN_REQUIRED")
        ? `<span class="src">Your session expired — sign in again on the Home tab.</span>`
        : `<span class="warn">Couldn't load players: ${escape(msg)}</span>`;
      return;
    }
  }
  // Toggled closed while the lookup was in flight — don't clobber.
  if (slot.dataset.roster !== "1") return;
  const names = ids.map((id) => playerNames.get(id) || id.slice(0, 8));
  slot.innerHTML = `<div class="roster">${names
    .map((n) => `<span class="roster-name">${escape(n)}</span>`)
    .join("")}</div>`;
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
}

interface GameServerEntry {
  serverName?: string;
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

// Friendly label for the UT4 match state; unknown/blank states show nothing.
function matchState(s: string | undefined): string {
  switch (s) {
    case "InProgress":
      return "in progress";
    case "WaitingToStart":
    case "CountdownToBegin":
      return "warming up";
    case "WaitingPostMatch":
    case "MatchEnteringOvertime":
      return "ending";
    default:
      return "";
  }
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

function trustLabel(level: number | undefined): string {
  switch (level) {
    case 0:
      return `<span class="ok" title="Epic-trusted">Epic</span>`;
    case 1:
      return `<span class="src" title="Trusted">Trusted</span>`;
    default:
      return `<span class="warn" title="Community / untrusted">Custom</span>`;
  }
}

let serverCache: GameServerEntry[] = [];
let serversShowEmpty = false;
let serversFetching = false;

async function renderServers() {
  if (serversFetching) return;
  serversFetching = true;
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

// Render from the cached list, so the "show empty" toggle re-filters without a
// re-fetch. Groups every live match under its parent hub — like the Hubs tab on
// ut4.timiimit.com. A match links to its hub by shared IP first (the reliable
// signal — UT_HUBGUID_s on an instance rarely equals its hub's advertised GUID),
// then by UT_HUBGUID_s as a fallback; matches that resolve to no listed hub fall
// into a standalone "Other matches" group.
function renderServerList() {
  const hubs = serverCache.filter((s) => isLobby(s) && srvHasAddr(s));
  const instances = serverCache.filter((s) => !isLobby(s) && srvHasAddr(s));

  // Index hubs for instance->hub resolution: by IP (primary) and GUID (fallback).
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

  // Per-row connect status lands in a slot keyed by the row's server address, so
  // the "Launched — connecting…" / error message appears right under the row the
  // user clicked instead of in one shared box at the bottom of the panel. The
  // slot is empty (no reserved height) until a click fills it.
  const statusId = (server: string): string => `srv-status-${server.replace(/[^a-zA-Z0-9]/g, "-")}`;

  // A nested live-match row (mode · map · players · state) with connect actions
  // and its own status slot directly beneath.
  const matchRow = (s: GameServerEntry): string => {
    const server = `${s.serverAddress}:${s.serverPort}`;
    const p = srvPlayers(s);
    const max = Number(s.attributes?.UT_MAXPLAYERS_i ?? s.maxPublicPlayers ?? 0);
    const mode = prettyMode(String(s.attributes?.GAMEMODE_s ?? ""));
    const map = String(s.attributes?.MAPNAME_s ?? "");
    const st = matchState(s.attributes?.UT_MATCHSTATE_s);
    const locked = ((s.attributes?.UT_SERVERFLAGS_i ?? 0) & 1) !== 0;
    const sub =
      `${escape(mode || "match")}${map ? ` · ${escape(map)}` : ""} · ${p}/${max} players` +
      (st ? ` · ${st}` : "") +
      (locked ? ` · <span title="Password protected">🔒</span>` : "");
    const lockAttr = locked ? ` data-locked="1"` : "";
    const rosterBtn =
      p > 0
        ? `<button class="server-roster" type="button" data-server="${escape(server)}" title="Show players">▾</button>`
        : "";
    return `<div style="padding:0 2px 0 16px;border-bottom:1px solid var(--row-sep)">
        <div style="display:flex;justify-content:space-between;align-items:center;gap:12px;padding:7px 0">
          <div class="src" style="min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${sub}</div>
          <span style="flex:none;display:flex;gap:6px;align-items:center">
            ${rosterBtn}
            <button class="server-spectate" type="button" data-server="${escape(server)}"${lockAttr}>Spectate</button>
            <button class="server-join" type="button" data-server="${escape(server)}"${lockAttr}>Join</button>
          </span>
        </div>
        <div id="${statusId(server)}" class="row-status"></div>
      </div>`;
  };

  // A hub block: header (name, trust, live-match count, lobby occupancy) plus its
  // nested matches. The header's Join button enters the hub lobby itself, with a
  // status slot right under the header.
  const hubBlock = (h: GameServerEntry): string => {
    const kids = (childrenOf.get(h) ?? [])
      .filter((s) => serversShowEmpty || srvPlayers(s) > 0)
      .sort((a, b) => srvPlayers(b) - srvPlayers(a));
    const name = String(h.attributes?.UT_SERVERNAME_s || h.serverName || "Hub");
    const server = `${h.serverAddress}:${h.serverPort}`;
    const lobby = srvPlayers(h);
    const live = kids.length;
    const header = `<div style="display:flex;justify-content:space-between;align-items:center;gap:12px;padding:9px 2px;border-bottom:1px solid var(--hub-sep)">
        <div style="min-width:0">
          <div style="font-weight:600;overflow:hidden;text-overflow:ellipsis;white-space:nowrap"><span class="ok" style="font-size:0.78em;border:1px solid currentColor;border-radius:3px;padding:0 4px;margin-right:6px">HUB</span>${escape(name)} &nbsp;${trustLabel(h.attributes?.UT_SERVERTRUSTLEVEL_i)}</div>
          <div class="src">${live} live match${live === 1 ? "" : "es"} · ${lobby} in lobby</div>
        </div>
        <span style="flex:none"><button class="server-join" type="button" data-server="${escape(server)}">Join hub</button></span>
      </div>
      <div id="${statusId(server)}" class="row-status"></div>`;
    const body = kids.length
      ? kids.map(matchRow).join("")
      : `<div class="src" style="padding:7px 2px 7px 16px">No live matches right now.</div>`;
    return `<div style="margin-bottom:6px">${header}${body}</div>`;
  };

  // Show a hub if it has live matches, lobby players, or the toggle is on.
  const visibleHubs = hubs
    .filter((h) => {
      const kids = childrenOf.get(h) ?? [];
      return serversShowEmpty || srvPlayers(h) > 0 || kids.some((s) => srvPlayers(s) > 0);
    })
    .sort((a, b) => {
      const act = (h: GameServerEntry) =>
        (childrenOf.get(h) ?? []).reduce((n, s) => n + srvPlayers(s), 0) + srvPlayers(h);
      return act(b) - act(a);
    });

  const visibleStandalone = standalone
    .filter((s) => serversShowEmpty || srvPlayers(s) > 0)
    .sort((a, b) => srvPlayers(b) - srvPlayers(a));

  const liveMatchCount =
    visibleHubs.reduce(
      (n, h) => n + (childrenOf.get(h) ?? []).filter((s) => srvPlayers(s) > 0).length,
      0,
    ) + visibleStandalone.filter((s) => srvPlayers(s) > 0).length;

  const sections: string[] = visibleHubs.map(hubBlock);
  if (visibleStandalone.length) {
    sections.push(
      `<div style="margin-bottom:6px"><div class="src" style="padding:9px 2px;border-bottom:1px solid var(--hub-sep);font-weight:600">Other matches</div>${visibleStandalone
        .map(matchRow)
        .join("")}</div>`,
    );
  }
  const body = sections.join("");

  const hint = state.ut4?.logged_in
    ? ""
    : ` · <span class="warn">sign in on the Home tab to join</span>`;
  serversPanel.innerHTML = `
    <div class="controls" style="justify-content:space-between;align-items:center">
      <span>${visibleHubs.length} hub${visibleHubs.length === 1 ? "" : "s"} · ${liveMatchCount} live match${liveMatchCount === 1 ? "" : "es"}${hint}</span>
      <span style="display:flex;align-items:center;gap:12px">
        <label style="font-weight:normal;display:flex;align-items:center;gap:5px"><input type="checkbox" id="servers-empty"${serversShowEmpty ? " checked" : ""}/> show empty</label>
        <button id="servers-refresh" type="button">Refresh</button>
      </span>
    </div>
    <div style="max-height:60vh;overflow:auto">${
      body || `<p>No ${serversShowEmpty ? "servers online" : "live matches or populated hubs"} right now.</p>`
    }</div>`;

  document.getElementById("servers-refresh")?.addEventListener("click", () => void renderServers());
  document.getElementById("servers-empty")?.addEventListener("change", (e) => {
    serversShowEmpty = (e.target as HTMLInputElement).checked;
    renderServerList();
  });
  serversPanel.querySelectorAll<HTMLButtonElement>(".server-join").forEach((btn) => {
    btn.addEventListener("click", () => {
      const server = btn.dataset.server;
      if (!server) return;
      const status = document.getElementById(statusId(server));
      if (btn.dataset.locked) {
        promptServerPassword(status, (pw) => void connectTo(server, pw, status));
      } else {
        void connectTo(server, "", status);
      }
    });
  });
  serversPanel.querySelectorAll<HTMLButtonElement>(".server-spectate").forEach((btn) => {
    btn.addEventListener("click", () => {
      const server = btn.dataset.server;
      if (!server) return;
      const status = document.getElementById(statusId(server));
      if (btn.dataset.locked) {
        promptServerPassword(status, (pw) => void connectTo(`${server}?SpectatorOnly=1`, pw, status));
      } else {
        void connectTo(`${server}?SpectatorOnly=1`, "", status);
      }
    });
  });
  serversPanel.querySelectorAll<HTMLButtonElement>(".server-roster").forEach((btn) => {
    btn.addEventListener("click", () => {
      const server = btn.dataset.server;
      if (server) void toggleRoster(server, document.getElementById(statusId(server)));
    });
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
  panel.innerHTML = `
    <div class="game-install">
      <div><strong>Don't have UT4 yet?</strong> The launcher downloads the community installer (v${escape(
        info.version,
      )}, ${gb} GB) to a folder you choose, verifies it, unpacks it, and runs it. <strong>You pick where UT4 actually installs in the installer's own window</strong> (with a Windows admin prompt) — so for the download just use a normal folder like your <strong>Downloads</strong> (needs ~${needGb} GB free), not Program Files.</div>
      ${
        info.dotnet_ok
          ? ""
          : `<div class="dotnet-note">⚠ This installer also needs the <strong>.NET Desktop Runtime 6</strong> (a free Microsoft component) — without it, it won't start. <button class="link-btn" type="button" data-extlink="https://dotnet.microsoft.com/download/dotnet/6.0">Get the .NET Runtime</button> — grab <strong>.NET Desktop Runtime 6.0 &rsaquo; Windows x64</strong>; you can install it while UT4 downloads.</div>`
      }
      <div class="game-install-actions">
        <button id="game-download-btn" type="button">Download &amp; Install UT4</button>
      </div>
      <div id="game-install-status" class="launch-status"></div>
    </div>`;
  document
    .getElementById("game-download-btn")
    ?.addEventListener("click", () => void startGameDownload());
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
// its own "Try again" button on failure.
async function startGameInstall(zipPath: string): Promise<void> {
  const status = document.getElementById("game-install-status");
  if (status) status.innerHTML = gameProgressSkeleton(false);

  if (gameDownloadUnlisten) {
    gameDownloadUnlisten();
    gameDownloadUnlisten = null;
  }
  gameDownloadUnlisten = await attachGameProgress();

  try {
    const res = await invoke<{ installer_dir: string; exe_path: string }>("install_game", {
      zipPath,
    });
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
// Delegated click handlers (survive re-renders):
// - the "✓ NetcodePlus installed" badge opens that install's plugin folder;
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
  const ext = t?.closest<HTMLElement>("[data-extlink]");
  if (ext?.dataset.extlink) openExternal(ext.dataset.extlink);
});

void showVersion();
void loadAll();
