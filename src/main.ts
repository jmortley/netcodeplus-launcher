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

const installStatus = document.getElementById("install-status")!;
const pickButton = document.getElementById("pick-dir") as HTMLButtonElement;
const versionLabel = document.getElementById("version")!;

const state: {
  installs: DetectedInstall[];
  presets: AffinityPreset[];
  selInstall: number;
} = { installs: [], presets: [], selInstall: 0 };

function escape(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

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

/** Prefer a profile that isn't the dx12 variant as the default selection. */
function defaultProfileIndex(di: DetectedInstall): number {
  const i = di.profiles.findIndex((p) => !p.args.some((a) => /-dx12/i.test(a)));
  return i >= 0 ? i : 0;
}

function optionList(items: string[], selected: number): string {
  return items
    .map((label, i) => `<option value="${i}"${i === selected ? " selected" : ""}>${escape(label)}</option>`)
    .join("");
}

function launchControls(di: DetectedInstall): string {
  const profOpts = di.profiles.map(
    (p, i) => `<option value="${i}"${i === defaultProfileIndex(di) ? " selected" : ""}>${escape(p.label)}</option>`,
  ).join("");
  const affOpts =
    state.presets
      .map((p) => `<option value="${escape(p.mask_hex)}">${escape(p.label)}</option>`)
      .join("") + `<option value="__custom__">Custom…</option>`;
  return `
    <div class="controls">
      <label>Launch profile
        <select id="profile-sel">${profOpts}</select>
      </label>
      <label>Priority
        <select id="priority-sel">
          <option value="normal" selected>Normal</option>
          <option value="high">High</option>
        </select>
      </label>
      <label>CPU affinity
        <select id="affinity-sel">${affOpts}</select>
      </label>
      <label id="affinity-hex-wrap" style="display:none">Mask (hex)
        <input id="affinity-hex" type="text" placeholder="e.g. FFC" spellcheck="false" />
      </label>
      <button id="launch-btn" type="button">Launch UT4</button>
      <div id="launch-status" class="launch-status"></div>
    </div>
  `;
}

function render() {
  if (state.installs.length === 0) {
    installStatus.innerHTML = `
      <div class="warn">
        No UT4 install detected from your desktop shortcuts.
        Click <em>Pick install folder</em> below and choose your
        <code>UnrealTournament</code> directory.
      </div>`;
    return;
  }
  if (state.selInstall >= state.installs.length) state.selInstall = 0;
  const di = state.installs[state.selInstall];

  const installPicker =
    state.installs.length > 1
      ? `<label>Install
           <select id="install-sel">${optionList(
             state.installs.map((d) => d.install.root),
             state.selInstall,
           )}</select>
         </label>`
      : "";

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
    ${launchControls(di)}
  `;
  wire();
}

function wire() {
  const installSel = document.getElementById("install-sel") as HTMLSelectElement | null;
  installSel?.addEventListener("change", () => {
    state.selInstall = Number(installSel.value);
    render();
  });

  const affSel = document.getElementById("affinity-sel") as HTMLSelectElement | null;
  const hexWrap = document.getElementById("affinity-hex-wrap");
  affSel?.addEventListener("change", () => {
    if (hexWrap) hexWrap.style.display = affSel.value === "__custom__" ? "" : "none";
  });

  const btn = document.getElementById("launch-btn") as HTMLButtonElement | null;
  btn?.addEventListener("click", () => void launch());
}

async function launch() {
  const di = state.installs[state.selInstall];
  const profileSel = document.getElementById("profile-sel") as HTMLSelectElement;
  const prioritySel = document.getElementById("priority-sel") as HTMLSelectElement;
  const affSel = document.getElementById("affinity-sel") as HTMLSelectElement;
  const hexInput = document.getElementById("affinity-hex") as HTMLInputElement | null;
  const status = document.getElementById("launch-status")!;

  const profile = di.profiles[Number(profileSel.value)];
  const affinityMaskHex =
    affSel.value === "__custom__" ? (hexInput?.value.trim() ?? "") : affSel.value;

  status.textContent = "Launching…";
  try {
    await invoke("launch_game", {
      executable: di.install.executable,
      args: profile.args,
      priority: prioritySel.value,
      affinityMaskHex,
    });
    status.innerHTML = `<span class="ok">Launched: ${escape(profile.label)} (${escape(prioritySel.value)} priority${
      affinityMaskHex ? `, affinity ${escape(affinityMaskHex)}` : ""
    })</span>`;
  } catch (err) {
    status.innerHTML = `<span class="warn">Launch failed: ${escape(String(err))}</span>`;
    console.error("launch_game failed:", err);
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

async function loadInstalls() {
  try {
    const [installs, presets] = await Promise.all([
      invoke<DetectedInstall[]>("detect_installs"),
      invoke<AffinityPreset[]>("affinity_presets"),
    ]);
    state.installs = installs;
    state.presets = presets;
    state.selInstall = 0;
    render();
  } catch (err) {
    installStatus.innerHTML = `<div class="warn">Detection failed: ${escape(String(err))}</div>`;
    console.error("detect_installs failed:", err);
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
    render();
  } catch (err) {
    installStatus.innerHTML = `<div class="warn">Validation failed: ${escape(String(err))}</div>`;
    console.error("check_install failed:", err);
  }
}

pickButton.addEventListener("click", () => void pickDir());
void showVersion();
void loadInstalls();
