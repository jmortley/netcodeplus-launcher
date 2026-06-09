# Linux support — design & roadmap

Native Linux launcher for the **Lutris/Wine** UT4 audience.

## The core reality

On Linux, UT4 is **not** a native game — it's the **Windows build run under
Wine/Lutris**. Confirmed in the field: the running game logs
`Base directory: C:/…` (it lives inside the Wine prefix's `drive_c`; the real
Linux root would show as `Z:`), and the Lutris client is a **Shipping** build
that loads `UE4-NetcodePlus-Win64-Shipping.dll` (not the Development
`UE4-NetcodePlus.dll`).

So the Linux launcher is a **native Linux app that manages a Wine prefix** — it
does *not* run the game natively. Concretely:

- **Launch** = run the Windows exe **through Wine** against its prefix
  (`WINEPREFIX=<prefix> wine UT4.exe …`), not a direct spawn (a PE binary can't
  exec on Linux). Our launch args (`-ncpconnect=…`, auth) forward unchanged.
- **Plugin** lives at `<prefix>/drive_c/Program Files/UnrealTournament/UnrealTournament/Plugins/NetcodePlus/`
  — **copy, not symlink** (Wine quirk). Our release zip already carries both the
  Shipping + Development Win64 DLLs, so a whole-folder install is correct, and the
  version gate reports the same `NETCODE_PLUGIN_VERSION` as a Windows client.
- Verify success in `<prefix>/drive_c/…/Saved/Logs/UnrealTournament.log` →
  `LogLoad: netcodeplus loaded`.

## It already half-runs on Linux

- The **Rust workspace cross-compiles to Linux** (CI runs `clippy`/`test` on
  `ubuntu-latest`); every platform module has `#[cfg(not(windows))]` stubs.
- **Credentials are already cross-platform** — the `keyring` crate with
  `linux-native` (kernel keyutils). Note: keyutils can be session-volatile;
  consider the secret-service backend for refresh-token persistence.
- Tauri renders via **WebKitGTK** on Linux instead of WebView2.

This is an **adaptation, not a rewrite**.

## What's Windows-specific (the porting surface)

| Module | Linux status / work |
|---|---|
| `launch` | ✅ **done** — runs through Wine (`crate::linux`); Lutris-env fidelity is a follow-up |
| `linux` (new) | ✅ pure Wine path/argv helpers, unit-tested |
| `shortcut` / detect | `.lnk`/registry → finds nothing. Need Lutris/Wine-prefix detection. MVP: the existing **manual folder-pick** |
| `install` / `plugin_install` | path-join logic works; just target the in-prefix path + ensure copy semantics |
| `elevate` | UAC/`runas` — N/A on Linux (prefix is user-owned); stub stays a no-op |
| `dotnet` | .NET gate for the Windows installer — N/A; gate the UI off |
| `disk` | free-space via WinAPI → statvfs on Linux |
| `compat` | RUNASADMIN registry flag — Windows-only; gated off |
| self-update | downloads/relaunches an `.exe` → ship + swap an **AppImage** (same signed-manifest hash-pin model) |

## Packaging — "deb vs others"

Don't fragment per-distro. Tauri's bundler emits Linux targets directly:

- **AppImage = primary.** One self-contained binary that runs on Ubuntu/Fedora/
  Arch/etc., no install, and self-updates via the same download-and-replace model
  the launcher already uses. This *is* the "deb vs others" answer.
- **`.deb` = nice-to-have** for Ubuntu/Debian/Mint/Pop (apt + desktop
  integration). Tauri emits it from the same build.
- **Skip Flatpak** initially: its sandbox fights reaching into the Lutris/Wine
  prefix and spawning `wine`/`lutris`, which is the launcher's core job.
- Runtime dep: `webkit2gtk-4.1` (present on Ubuntu).

**Don't** try to run the existing Windows `.exe` under Wine — the UI is WebView2,
and WebView2/Edge-under-Wine is broken (blank window). The native WebKitGTK build
is the path.

## Testing strategy (given Tailscale-only / no easy display)

Most of the work is headless-testable; only the final visual/game pass needs
pixels.

- **Tier 1 — no display (do all of this remotely/CI):** `cargo test`/`clippy`,
  the plugin-copy-into-prefix, Lutris-prefix detection (config parsing), the
  launch-argv assembly. Write the host logic as **pure functions behind
  `cargo test`** — they run on the Windows dev box *and* CI's `ubuntu-latest`.
- **Tier 2 — headless smoke (SSH):** `xvfb-run ./UT4-Launcher.AppImage` →
  confirms it starts + renders to a virtual framebuffer without crashing;
  screenshot it.
- **Tier 3 — needs a display:** real GUI interaction + actually launching the
  game → VNC/RDP into the box's desktop over Tailscale, or in person. The game
  already runs there via Lutris, so the prefix is known-good.

GitHub `ubuntu-latest` runners can build the **AppImage as a CI artifact** — so
the box is only needed for the Wine-specific + visual reality.

## Roadmap

1. ✅ **Launch through Wine** (`ncp-host::linux` + the `cfg(not(windows))` glue),
   test-covered. — *PR #6.*
2. **Lutris/Wine-prefix detection** — parse `~/.config/lutris/games/*.yml`
   (+ common prefix scan) → the UT4 root inside `drive_c`. Pure-function +
   tests; MVP falls back to the manual folder-pick.
3. **Plugin-into-prefix** — confirm `install`/`plugin_install` target the
   in-prefix path with copy semantics; integration-test against a fixture tree.
4. **AppImage packaging + CI artifact** — Tauri Linux bundle; `webkit2gtk-4.1`;
   add a CI job that uploads the AppImage.
5. **Self-update for Linux** — a Linux `launcher` asset + the AppImage swap path
   (reuse the hash-pin model).
6. **UI gating** — hide the Windows-installer / .NET / elevation paths on Linux.
7. **Lutris-env fidelity** — launch via the game's configured runner/DXVK
   (detect Lutris's wine bin + env) instead of system `wine`.
8. **Visual/game validation** — `xvfb` smoke + VNC/in-person on the Ubuntu box.

## Status

Increment 1 (launch) is done and **verified Linux-clippy-clean for `ncp-host`**
from the Windows dev box (cross-compile; the full app — `ncp-net`/`src-tauri` —
needs real Linux). **Heads-up:** `main`'s CI is pre-existing red on
`clippy`/`audit`/`fmt` (manual ff merges, not CI-gated); enabling the Linux build
is also a good prompt to clear that debt.
