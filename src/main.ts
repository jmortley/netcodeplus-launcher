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

const installStatus = document.getElementById("install-status")!;
const pickButton = document.getElementById("pick-dir") as HTMLButtonElement;
const versionLabel = document.getElementById("version")!;
const statsPanel = document.getElementById("stats-panel")!;

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
  if (state.installs.length === 0) {
    installStatus.innerHTML = `
      <div class="warn">No UT4 install detected from your desktop shortcuts.</div>
      <p>If you don't have UnrealTournament yet, install it with the UT4Ever
      installer, then reopen the launcher:</p>
      <button id="get-ut4" type="button">Get UT4 — ut4ever.org/installer</button>
      <p>Already installed? Click <em>Pick install folder</em> below and choose your
      <code>UnrealTournament</code> directory.</p>`;
    document.getElementById("get-ut4")?.addEventListener("click", () =>
      openExternal("https://ut4ever.org/installer"),
    );
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

  const heading =
    state.installs.length === 1
      ? `<div class="ok">We think your game is installed here:</div>`
      : `<div class="warn">Found ${state.installs.length} UT4 installs — pick the one you play:</div>`;

  installStatus.innerHTML = `
    ${heading}
    ${installPicker}
    <div class="install-card">
      <div><strong>${escape(di.install.root)}</strong></div>
      <div class="src">${sourceText(di.source)}</div>
      <div class="ncp">${netcodeplusBadge(di.netcodeplus)}</div>
    </div>
    <div class="controls">
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
      <button id="launch-btn" type="button">Launch UT4</button>
      <div id="launch-status" class="launch-status"></div>
    </div>
  `;
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

  (document.getElementById("launch-btn") as HTMLButtonElement | null)?.addEventListener("click", () => void launch());
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
    render();
    renderStats();
    renderPug();
  } catch (err) {
    installStatus.innerHTML = `<div class="warn">Detection failed: ${escape(String(err))}</div>`;
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
      installStatus.innerHTML = `
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
    installStatus.innerHTML = `<div class="warn">Validation failed: ${escape(String(err))}</div>`;
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
  try {
    await invoke("save_launcher_token", { token });
  } catch (err) {
    console.error("save_launcher_token failed:", err);
  }
  renderPug();
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
  pugControls.innerHTML = `
    <p>Queue for iCTF <button id="pug-token-clear" type="button" class="link-btn">change token</button></p>
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

pickButton.addEventListener("click", () => void pickDir());
void showVersion();
void loadAll();
