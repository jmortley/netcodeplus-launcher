# UT4 Community Launcher

A small, source-available launcher for Unreal Tournament 4 community play — sign
in, launch, find a game, keep your community content up to date. Built so the
update path can be **cryptographically verified end to end** instead of trusted
on faith.

> **Status: pre-release / beta.** Detection, login, launch, server browser,
> stats, config, and community features are live. The signed auto-update flow
> (plugin / launcher / paks) is wired but waiting on the hardware signing-key
> ceremony — see [Security](#security). Repo, crate, and exe are still named
> `netcodeplus-launcher`; the app's display name is "UT4 Community Launcher."
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
- **Server browser** — live hubs and matches grouped the way the in-game/site
  browser shows them, with one-click Join or Spectate.
- **PUGs** — link your account once, then join/leave the iCTF PUG queue and jump
  straight into the match from the launcher.
- **Your stats** — link your ut4stats.com profile to see per-mode rating,
  K/D, per-weapon accuracy, and recent matches (with links back to ut4stats).
- **Performance config** — optionally merge a competitive `Engine.ini` baseline
  (FPS cap, gamma, render cvars, OpenAL, async-loading toggle). It **backs up
  your ini first** and never touches your login/online settings; one-click
  restore. Also silently repairs the master-server config that a known UT4 bug
  occasionally wipes.
- **News + Discord** — community announcements and one-click invites.

## Install & run

Download the release `.exe` and run it — no installer, no admin rights. It runs
at your normal user privilege.

First time: sign in, let it detect your UT4 install, and you're set. To use PUGs
or stats, link your account once (the in-app steps walk you through it).

## Security

The whole point of the project. The trust model:

- **Source-available.** Read what it does before you run it.
- **No bundled secrets, no telemetry.** It talks to the UT4 master server,
  ut4stats.com, and the PUG bot — nothing else, nothing in the background.
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

The verify-before-install machinery is built and tested, but the production
signing key isn't provisioned yet, so the **auto-update feature is not live in
this beta** — everything else is. Code signing for the released `.exe`
(SmartScreen/AV reputation) is planned alongside it.

## Reporting issues

This is a beta — bug reports and security concerns are welcome. The launcher
version is shown in-app for reports. For anything security-sensitive, see
`SECURITY.md`.

## Build (for the curious)

`npm run tauri build -- --no-bundle`. Toolchain notes and the post-build icon
step are in the contributor docs.

## License

Apache 2.0. See `LICENSE` and `NOTICE`.
