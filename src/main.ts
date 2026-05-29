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
}

const installStatus = document.getElementById("install-status")!;
const pickButton = document.getElementById("pick-dir") as HTMLButtonElement;
const versionLabel = document.getElementById("version")!;

const state = {
  installs: [] as DetectedInstall[],
  presets: [] as AffinityPreset[],
  selInstall: 0,
  profileLabel: null as string | null,
  priority: "normal" as "normal" | "high",
  affinityHex: "", // "" = all cores
};

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

/** Index of the currently-selected install's profile, honoring a saved
 * choice, else preferring a non-dx12 variant. */
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
      <div class="warn">
        No UT4 install detected from your desktop shortcuts.
        Click <em>Pick install folder</em> below and choose your
        <code>UnrealTournament</code> directory.
      </div>`;
    return;
  }
  if (state.selInstall >= state.installs.length) state.selInstall = 0;
  const di = state.installs[state.selInstall];
  const profIdx = selectedProfileIndex(di);
  state.profileLabel = di.profiles[profIdx]?.label ?? null;

  const installPicker =
    state.installs.length > 1
      ? `<label>Install
           <select id="install-sel">${state.installs
             .map(
               (d, i) =>
                 `<option value="${i}"${i === state.selInstall ? " selected" : ""}>${escape(
                   d.install.root,
                 )}</option>`,
             )
             .join("")}</select>
         </label>`
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
    state.profileLabel = null; // let the new install pick its default
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

  const btn = document.getElementById("launch-btn") as HTMLButtonElement | null;
  btn?.addEventListener("click", () => void launch());
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

function applyPrefs(prefs: LauncherState) {
  state.priority = prefs.launch_priority === "high" ? "high" : "normal";
  state.affinityHex = prefs.affinity_mask_hex ?? "";
  state.profileLabel = prefs.launch_profile_label;
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

pickButton.addEventListener("click", () => void pickDir());
void showVersion();
void loadAll();
