import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

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

interface AffinityPreset {
  label: string;
  mask_hex: string;
}

interface LauncherState {
  install_path: string | null;
  launch_profile_label: string | null;
  launch_priority: "normal" | "high";
  affinity_mask_hex: string | null;
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
  recent: { mode: string; map: string; server: string; played_at: string; result: string; delta: number }[];
}

interface EngineTweaks {
  frame_rate_cap: number;
  smooth_frame_rate: boolean;
  display_gamma: number;
}

interface ConfigState {
  ini_exists: boolean;
  has_backup: boolean;
  master_server_ok: boolean;
  tweaks: EngineTweaks;
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

const launchPanel = document.getElementById("launch-panel")!;
const advancedPanel = document.getElementById("advanced-launch")!;
const pickButton = document.getElementById("pick-dir") as HTMLButtonElement;
const versionLabel = document.getElementById("version")!;
const statsPanel = document.getElementById("stats-panel")!;
const configPanel = document.getElementById("config-panel")!;
const newsPanel = document.getElementById("news-panel")!;

const state = {
  installs: [] as DetectedInstall[],
  presets: [] as AffinityPreset[],
  selInstall: 0,
  profileLabel: null as string | null,
  priority: "normal" as "normal" | "high",
  affinityHex: "",
  linkedId: null as string | null,
  linkedName: null as string | null,
  launcherToken: null as string | null,
  pugStatus: null as PugStatus | null,
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

function netcodeplusBadge(status: NetcodePlusStatus): string {
  switch (status) {
    case "installed":
      return `<span class="ok">✓ NetcodePlus installed here</span>`;
    case "missing":
      return `<span class="warn">NetcodePlus is not installed in this UT4 install</span>`;
    case "malformed":
      return `<span class="warn">NetcodePlus folder looks broken — missing <code>.uplugin</code> or <code>Binaries/</code></span>`;
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
  renderLaunch();
  renderAdvanced();
}

// Launch tab: clean and end-user — just the game and a big Launch button.
// Everything technical lives in the Advanced tab.
function renderLaunch() {
  if (state.installs.length === 0) {
    launchPanel.innerHTML = `
      <div class="play-card">
        <div class="play-hero">
          <div class="play-hero-overlay">
            <div class="play-title">Unreal Tournament</div>
          </div>
        </div>
        <p class="warn">No install detected.</p>
        <p>Don't have the game yet? Get it from the UT4Ever installer, then reopen the launcher.</p>
        <button id="get-ut4" type="button" class="launch-primary">Get UT4</button>
      </div>`;
    document.getElementById("get-ut4")?.addEventListener("click", () =>
      openExternal("https://ut4ever.org/installer"),
    );
    return;
  }
  if (state.selInstall >= state.installs.length) state.selInstall = 0;
  const di = state.installs[state.selInstall];
  launchPanel.innerHTML = `
    <div class="play-card">
      <div class="play-hero">
        <div class="play-hero-overlay">
          <div class="play-title">Unreal Tournament</div>
          <div class="play-sub">${netcodeplusBadge(di.netcodeplus)}</div>
        </div>
      </div>
      <button id="launch-btn" type="button" class="launch-primary">▶&nbsp;&nbsp;Launch</button>
      <div id="launch-status" class="launch-status"></div>
    </div>`;
  (document.getElementById("launch-btn") as HTMLButtonElement | null)?.addEventListener(
    "click",
    () => void launch(),
  );
}

// Advanced tab: power-user knobs — install selection, launch profile,
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
      <div class="ncp">${netcodeplusBadge(di.netcodeplus)}</div>
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
  }).catch((err) => console.error("save_launch_prefs failed:", err));
}

async function launch() {
  const di = state.installs[state.selInstall];
  const profile =
    di.profiles.find((p) => p.label === state.profileLabel) ?? di.profiles[selectedProfileIndex(di)];
  const status = document.getElementById("launch-status")!;

  status.textContent = "Launching…";
  try {
    await invoke("launch_game", {
      executable: di.install.executable,
      args: profile.args,
      priority: state.priority,
      affinityMaskHex: state.affinityHex || null,
    });
    persist();
    status.innerHTML = `<span class="ok">Launched: ${escape(profile.label)} (${escape(state.priority)} priority${
      state.affinityHex ? `, affinity ${escape(state.affinityHex)}` : ""
    })</span>`;
  } catch (err) {
    status.innerHTML = `<span class="warn">Launch failed: ${escape(String(err))}</span>`;
    console.error("launch_game failed:", err);
  }
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
    renderSummary(JSON.parse(raw) as PlayerSummary);
  } catch (err) {
    renderSummary(null, String(err));
  }
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
          return `<li><span class="${cls}">${escape(m.result)}</span> ${sign}${m.delta} · ${escape(m.mode)} · ${escape(
            m.map || "?",
          )} <span class="src">${escape(m.played_at)}</span></li>`;
        })
        .join("")}</ul>`
    : "";

  statsPanel.innerHTML = `
    <div class="stat-head">
      <strong>${escape(s.playername)}</strong>
      <span class="src">${escape(s.flag || "")}</span>
      <button id="ut4-change" type="button" class="link-btn">change</button>
    </div>
    <div class="ratings">${ratings}</div>
    <dl>
      <dt>Record</dt><dd>K/D ${s.totals.kd} · ${s.totals.kills}/${s.totals.deaths} · ${s.totals.games} games</dd>
      ${acc ? `<dt>Accuracy</dt><dd>${acc}</dd>` : ""}
    </dl>
    ${recent ? `<div class="src">Recent rated matches</div>${recent}` : ""}
  `;
  document.getElementById("ut4-change")?.addEventListener("click", () => void unlink());
}

// ---- startup --------------------------------------------------------------

function applyPrefs(prefs: LauncherState) {
  state.priority = prefs.launch_priority === "high" ? "high" : "normal";
  state.affinityHex = prefs.affinity_mask_hex ?? "";
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
    versionLabel.textContent = `v${await invoke<string>("launcher_version")}`;
  } catch (err) {
    versionLabel.textContent = "(version unknown)";
    console.error("launcher_version failed:", err);
  }
}

async function loadAll() {
  try {
    const [installs, presets, prefs] = await Promise.all([
      invoke<DetectedInstall[]>("detect_installs"),
      invoke<AffinityPreset[]>("affinity_presets"),
      invoke<LauncherState>("load_state"),
    ]);
    state.installs = installs;
    state.presets = presets;
    state.selInstall = 0;
    applyPrefs(prefs);
    void autoFixMasterServer();
    render();
    renderStats();
    renderPug();
    void renderConfig();
    void renderNews();
    if (state.launcherToken) {
      void pollPugStatus();
      startPugPolling();
    }
  } catch (err) {
    launchPanel.innerHTML = `<div class="warn">Detection failed: ${escape(String(err))}</div>`;
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

function openExternal(url: string) {
  void invoke("open_external", { url }).catch((err) => console.error("open_external failed:", err));
}

document.getElementById("join-utpugs")?.addEventListener("click", () => openExternal(UTPUGS_DISCORD));
document.getElementById("join-ictf")?.addEventListener("click", () => openExternal(ICTF_DISCORD));

// ---- in-launcher PUG join (UT4IGBot) --------------------------------------

const pugControls = document.getElementById("pug-controls")!;

async function pug(action: "joinpug" | "leavepug" | "listpug") {
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
    if (status) status.innerHTML = `<span class="warn">${escape(String(err))}</span>`;
    console.error("pug_action failed:", err);
  }
}

async function saveLauncherToken(token: string | null) {
  state.launcherToken = token;
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
  }
}

function renderPug() {
  if (!state.launcherToken) {
    pugControls.innerHTML = `
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
    </div>
    <div id="pug-status" class="launch-status"></div>`;
  document.getElementById("pug-join")?.addEventListener("click", () => void pug("joinpug"));
  document.getElementById("pug-leave")?.addEventListener("click", () => void pug("leavepug"));
  document.getElementById("pug-refresh")?.addEventListener("click", () => void pug("listpug"));
  document.getElementById("pug-token-clear")?.addEventListener("click", () => void saveLauncherToken(null));
}

async function connectToPug(server: string, password: string) {
  const status = document.getElementById("pug-status");
  const di = state.installs[state.selInstall];
  if (!di) {
    if (status)
      status.innerHTML = `<span class="warn">No UT4 install detected — pick your folder in the Advanced tab first.</span>`;
    return;
  }
  const profile =
    di.profiles.find((p) => p.label === state.profileLabel) ?? di.profiles[selectedProfileIndex(di)];
  const connectUrl = password ? `${server}?Password=${password}` : server;
  const args = [...profile.args, `-ncpconnect=${connectUrl}`];
  if (status) status.textContent = "Launching into the PUG…";
  try {
    await invoke("launch_game", {
      executable: di.install.executable,
      args,
      priority: state.priority,
      affinityMaskHex: state.affinityHex || null,
    });
    if (status) status.innerHTML = `<span class="ok">Launched — connecting to ${escape(server)}.</span>`;
  } catch (err) {
    if (status) status.innerHTML = `<span class="warn">Launch failed: ${escape(String(err))}</span>`;
    console.error("connect launch failed:", err);
  }
}

let pugPollTimer: number | undefined;

function startPugPolling() {
  if (pugPollTimer !== undefined) return;
  pugPollTimer = window.setInterval(() => void pollPugStatus(), 5000);
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

  const audioLine = openal
    ? `<div class="ok">✓ UT4-OpenAL detected — its audio module will be enabled.</div>`
    : `<p class="src">OpenAL not detected — get <button id="get-openal" type="button" class="link-btn">UT4-OpenAL</button> for HRTF positional audio. The audio override is skipped without it.</p>`;

  const t = cfg.tweaks;
  configPanel.innerHTML = `
    <p>Competitive graphics: high FPS, still readable. Your <code>Engine.ini</code> is backed up before the first apply; online and login settings are left untouched.</p>
    <p class="src">Note: <code>net.AllowAsyncLoading=0</code> loads into maps faster but can cause issues in Blitz (flag run).</p>
    <div class="controls">
      <label>Frame rate cap
        <input id="cfg-fps" type="number" min="0" step="10" value="${escape(String(t.frame_rate_cap))}" />
      </label>
      <label class="cfg-check"><input id="cfg-smooth" type="checkbox"${t.smooth_frame_rate ? " checked" : ""} /> Smooth frame rate</label>
      <label>Display gamma (brightness)
        <input id="cfg-gamma" type="number" min="1" max="5" step="0.1" value="${escape(String(t.display_gamma))}" />
      </label>
    </div>
    ${audioLine}
    <div class="discord-btns">
      <button id="cfg-apply" type="button">Apply competitive config</button>
      <button id="cfg-restore" type="button"${cfg.has_backup ? "" : " disabled"}>Restore backup</button>
    </div>
    <div id="cfg-status" class="launch-status"></div>`;

  document.getElementById("get-openal")?.addEventListener("click", () =>
    openExternal("https://github.com/main-exe/UT4-OpenAL/"),
  );
  document.getElementById("cfg-apply")?.addEventListener("click", () => void applyConfig(openal));
  document.getElementById("cfg-restore")?.addEventListener("click", () => void restoreConfig());

  if (flash) {
    const s = document.getElementById("cfg-status");
    if (s) s.innerHTML = `<span class="${flash.cls}">${escape(flash.text)}</span>`;
  }
}

async function applyConfig(setOpenalAudio: boolean) {
  const fps = Number((document.getElementById("cfg-fps") as HTMLInputElement).value);
  const gamma = Number((document.getElementById("cfg-gamma") as HTMLInputElement).value);
  const smooth = (document.getElementById("cfg-smooth") as HTMLInputElement).checked;
  try {
    await invoke("apply_engine_config", {
      frameRateCap: Number.isFinite(fps) ? fps : 360,
      smoothFrameRate: smooth,
      displayGamma: Number.isFinite(gamma) ? gamma : 3,
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

// ---- news (pinned to the Launch tab) ---------------------------------------

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

document.querySelectorAll<HTMLButtonElement>(".tab").forEach((btn) => {
  btn.addEventListener("click", () => {
    const name = btn.dataset.tab;
    document
      .querySelectorAll<HTMLElement>(".tab")
      .forEach((b) => b.classList.toggle("active", b === btn));
    document
      .querySelectorAll<HTMLElement>(".tab-panel")
      .forEach((p) => p.classList.toggle("active", p.id === `tab-${name}`));
  });
});

pickButton.addEventListener("click", () => void pickDir());
void showVersion();
void loadAll();
