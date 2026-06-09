# UT4 Community Launcher

A small, source-available launcher for Unreal Tournament 4 community play — sign
in, launch, find a game, keep your community content up to date. Built so the
update path can be **cryptographically verified end to end** instead of trusted
on faith.

> **Status: pre-release / beta.** Detection, login, launch, server browser,
> stats, config, community features, and the **signed auto-update flow** are all
> live — NetcodePlus and the launcher itself update through a manifest signed
> with the production key (offline, hardware-protected) and verified before
> install (see [Security](#security)). Still to come: code-signing the released
> `.exe` (SmartScreen/AV reputation) and the community-pak install path. Repo,
> crate, and exe are still named `netcodeplus-launcher`; the app's display name
> is "UT4 Community Launcher."
>
> *Not affiliated with Epic Games or the Unreal Tournament brand.*

## Why it exists

Getting set up for community UT4 used to mean hand-running random PowerShell
scripts to drop NetcodePlus into the right folder — and it broke constantly.
Wrong install path, nested folders, half-extracted zips: we burned far too many
screen-share sessions walking people through fixing their installs one at a
time. We needed something foolproof.

So this does the fiddly parts for you: finds your UT4 install, puts community
content where it belongs, signs you in, and launches — one click, no scripts,
no folder archaeology. And because it's the thing that downloads and installs
files onto your machine, the update path is built to be **cryptographically
verifiable end to end** (no bundled secrets, no telemetry, source public), so
"foolproof to set up" never turns into "easy to compromise."

## What it does

- **Sign in to UT4** against the community master server, so you don't need the
  in-game login window. Standard Epic-style OAuth.
- **Detect your install and launch** with your preferred profile, process
  priority, and CPU affinity — one click.
- **Don't have UT4 yet?** Pull the full community game installer straight from the
  launcher — it downloads the ~10 GB package, **verifies it**, and unpacks it, so
  a newcomer goes from nothing to in-game without hunting for files.
- **Keeps NetcodePlus current** — checks a cryptographically signed manifest on
  launch and updates the plugin across all your installs in one click (every
  download verified against the manifest before it's written). The launcher
  **updates itself the same way** — downloads, verifies, and relaunches into the
  new build — and **warns you to update before playing** if your install is on an
  old build, since community servers require a matching version. See
  [Security](#security).
- **Server browser** — live hubs and matches grouped the way the in-game/site
  browser shows them, with one-click Join or Spectate.
- **PUGs (pick-up games)** — join/leave queues across communities (Instagib
  Nation iCTF, UTPugs Wipeout/Elimination/Duel) and jump straight into the match.
  Home flags when a queue is **filling up** so you can hop in, and you can
  **watch a live PUG** to learn a mode with no sign-up. Link your account once
  for the modes that need it (the in-app steps walk you through it).
- **Discord Rich Presence** *(opt-in, off by default)* — show your current PUG
  status on your Discord profile, with a button friends can click to grab the
  launcher. Toggle it in Settings.
- **Your stats** — link your ut4stats.com profile to see per-mode rating,
  K/D, per-weapon accuracy, and recent matches (with links back to ut4stats).
- **Performance config + add-ons** — optionally merge a competitive `Engine.ini`
  baseline (FPS cap, gamma, render cvars, OpenAL, async-loading toggle). It
  **backs up your ini first** and never touches your login/online settings;
  one-click restore. Also silently repairs the master-server config that a known
  UT4 bug occasionally wipes, and points you to optional community add-ons
  (UltiCross crosshairs, UT4-OpenAL) without ever installing them silently.
- **News + Discord** — community announcements and one-click invites.

## Install & run

Download the release `.exe` and run it — no installer, no admin rights. It runs
at your normal user privilege. **Windows 10 or 11.**

**The one time you'll see an administrator (UAC) prompt** is when it updates the
NetcodePlus plugin and your UT4 lives in a protected folder like
`C:\Program Files\…` — Windows requires elevation to write there. The launcher
asks once, installs the update, and drops straight back to normal privilege; it
never runs the game itself as administrator. If your UT4 is in a normal,
user-writable location you won't be prompted at all, and declining the prompt
just leaves that install untouched (any other installs still update). Updating
the **launcher itself** never needs admin — you download and run the new build
like any other app.

First time: sign in, let it detect your UT4 install, and you're set. To use PUGs
or stats, link your account once (the in-app steps walk you through it).

> **Window blank / nothing shows up?** The launcher renders through the
> **WebView2 Runtime**, which ships with Windows 11 and Windows Update on
> Windows 10 — but some debloated/custom Windows images (Atlas, ReviOS, etc.)
> strip it out. Install the *Evergreen WebView2 Runtime* from Microsoft
> (<https://developer.microsoft.com/microsoft-edge/webview2/>) and relaunch.

## Security

The whole point of the project. The trust model:

- **Source-available.** Read what it does before you run it.
- **No bundled secrets, no telemetry.** It talks to the UT4 master server,
  ut4stats.com, the PUG bot, and GitHub (for signed update checks) — nothing
  else, nothing in the background.
- **Signed updates (the core design).** A single Ed25519 **public** key is
  compiled into the binary. Update manifests are signed **offline** with the
  matching private key, which lives on a hardware token and never touches a
  build server. Every downloaded artifact is checked against a SHA-256 in the
  signed manifest **before** it's installed; an `expires_at` bound and a
  monotonic sequence number block replay/downgrade. So even if a download host
  is compromised, a swapped file is rejected.
- **Your credentials stay yours.** Your password is never stored. The login
  refresh token lives in the **Windows Credential Manager** only — never in a
  config file, never synced to OneDrive. Each launch mints a short-lived
  exchange code for the game.
- **Hardened runtime.** TLS pins bundled roots (rustls + webpki) rather than
  trusting the OS store; a strict CSP and minimal dependency surface
  (vanilla TypeScript front end); the Rust workspace denies `unsafe` code with
  a single audited exception (setting CPU affinity).

**What "verified" does and doesn't mean for paks.** A signature guarantees
**integrity** — the file you get is the file that was signed. It does **not**
vouch for the **intent** of third-party content. Because community paks are
authored and hosted by others (on a host the launcher author doesn't control)
and UT4 paks can carry game logic, **pak auto-updates are off by default and
opt-in per source**, with the risk spelled out in-app. The NetcodePlus plugin
and the launcher itself — both built and signed from source we control — update
by default.

### Beta status, in plain terms

The signed verify-before-install update flow is **live**: the production signing
key is provisioned (offline, protected by a hardware security key), and
NetcodePlus + launcher updates are published through it and checked against the
compiled-in key before anything is written. Two pieces are still in progress —
**code-signing the released `.exe`** (so Windows SmartScreen/AV trust it without
a warning; an OSS code-signing cert is in the pipeline) and the **community-pak
install path** (not shipped yet, and pak updates stay opt-in by design even once
it lands — see above). Everything else is live.

## Privacy

No accounts beyond your existing UT4 login, and **no telemetry, no analytics, no
ads, no background phone-home** — the launcher reaches the network only when a
feature you're using needs it.

- **What stays on your machine.** Your preferences and non-secret identifiers
  live in a local `state.json`. Your login **refresh token** is kept in the
  **Windows Credential Manager** — never in a file, never synced to OneDrive —
  and your **password is never stored** at all.
- **What it talks to, and when.** The UT4 master server (sign-in + server
  browser), ut4stats.com (your stats + News), the PUG bot over HTTPS (only if
  you've set a PUG token), and GitHub (update checks) — nothing else, nothing in
  the background.
- **No tracking.** Nothing is sold, shared, or aggregated; there is no server
  that receives usage data, and there are no third-party trackers.

Full detail — every stored field and every network destination, with exactly
what's sent — is in [`PRIVACY.md`](PRIVACY.md).

## Reporting issues

This is a beta — bug reports and security concerns are welcome. The launcher
version is shown in-app for reports. For anything security-sensitive, see
`SECURITY.md`.

## Build (for the curious)

`npm run tauri build -- --no-bundle`. Toolchain notes and the post-build icon
step are in the contributor docs.

## License

Apache 2.0. See `LICENSE` and `NOTICE`.
