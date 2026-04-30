import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

interface UtInstall {
  root: string;
  executable: string;
  launch_args: string[];
  content_paks_dir: string;
  mod_paks_dir: string;
}

const installStatus = document.getElementById("install-status")!;
const pickButton = document.getElementById("pick-dir") as HTMLButtonElement;
const versionLabel = document.getElementById("version")!;

function escape(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function renderInstall(install: UtInstall | null) {
  if (install) {
    const args = install.launch_args.map(escape).join(" ");
    installStatus.innerHTML = `
      <div class="ok">UT4 install detected.</div>
      <dl>
        <dt>Root</dt><dd>${escape(install.root)}</dd>
        <dt>Executable</dt><dd>${escape(install.executable)}</dd>
        <dt>Launch args</dt><dd><code>${args}</code></dd>
        <dt>Content paks</dt><dd>${escape(install.content_paks_dir)}</dd>
        <dt>Mod paks (write target)</dt><dd>${escape(install.mod_paks_dir)}</dd>
      </dl>
    `;
  } else {
    installStatus.innerHTML = `
      <div class="warn">
        No UT4 install autodetected.
        Click <em>Pick install folder</em> below and choose your
        <code>UnrealTournament</code> directory (the one that contains
        <code>Engine/</code> and <code>UnrealTournament/</code>).
        Picking a deeper subfolder is fine — the launcher walks up to
        find the real root.
      </div>
    `;
  }
}

async function showVersion() {
  try {
    const version = await invoke<string>("launcher_version");
    versionLabel.textContent = `v${version}`;
  } catch (err) {
    versionLabel.textContent = "(version unknown)";
    console.error("launcher_version failed:", err);
  }
}

async function autodetect() {
  try {
    const result = await invoke<UtInstall | null>("detect_install");
    renderInstall(result);
  } catch (err) {
    installStatus.innerHTML = `<div class="warn">Detection failed: ${escape(String(err))}</div>`;
    console.error("detect_install failed:", err);
  }
}

async function pickDir() {
  let picked: string | string[] | null;
  try {
    picked = await open({
      directory: true,
      multiple: false,
      title: "Choose your UT4 install folder",
    });
  } catch (err) {
    console.error("dialog open failed:", err);
    return;
  }
  if (!picked) return;
  const dirPath = Array.isArray(picked) ? picked[0] : picked;

  try {
    const result = await invoke<UtInstall | null>("check_install", { path: dirPath });
    if (!result) {
      installStatus.innerHTML = `
        <div class="warn">
          The folder <code>${escape(dirPath)}</code> does not look like a UT4 install.
          Expected to find <code>Engine/Binaries/Win64/UE4-Win64-Shipping.exe</code>
          and <code>UnrealTournament/Content/Paks/</code> at or above this path.
        </div>
      `;
      return;
    }
    renderInstall(result);
  } catch (err) {
    installStatus.innerHTML = `<div class="warn">Validation failed: ${escape(String(err))}</div>`;
    console.error("check_install failed:", err);
  }
}

pickButton.addEventListener("click", () => {
  void pickDir();
});

void showVersion();
void autodetect();
