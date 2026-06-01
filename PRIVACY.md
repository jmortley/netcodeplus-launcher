# Privacy

The UT4 Community Launcher collects **no analytics and no telemetry**, has no
ads or trackers, and never phones home in the background. It only talks to the
network when a feature you used needs it (sign in, browse servers, view stats,
join a PUG, check for updates). This document spells out exactly what is stored
on your machine and what is sent where — the launcher is source-available, so
you can verify every claim here against the code.

## What stays on your machine

- **`state.json`** — a plain-text JSON file in your per-user app config folder
  (`%APPDATA%\org.netcodeplus.launcher\state.json`). It holds your preferences
  and non-secret identifiers: detected UT4 install path, launch profile /
  priority / CPU-affinity / window choice, your UT4 username + display name +
  account id, your linked ut4stats player id + name, your PUG launcher token,
  the installed NetcodePlus plugin version per install, and the update
  bookkeeping (manifest sequence floor, last launcher path/version). It is
  **local only** — never uploaded, never synced by the launcher.
- **Windows Credential Manager** — your UT4 **login refresh token** (the one
  secret) is stored here under the service name `netcodeplus-launcher`, kept out
  of `state.json` and out of any OneDrive-synced folder. Your **password is
  never stored** — it is used once to sign in and then discarded; each launch
  mints a short-lived, single-use exchange code for the game.

To clear local data: **Sign out** in the launcher (deletes the refresh token and
cached account info), **unlink** your ut4stats profile / clear your PUG token in
the UI, and/or delete `state.json`.

## What the launcher connects to, and why

| Destination | When | What is sent |
|---|---|---|
| **GitHub releases** (`github.com`) | On startup, to check for updates | Nothing about you — a plain `GET` of public, signed files (the update manifest + the NetcodePlus plugin download). |
| **UT4 master server** (`master-ut4.timiimit.com`) | When you sign in, launch, or open the Servers tab | Your username + password **once** at sign-in (over TLS), then only a rotating refresh token; the server browser list is fetched anonymously (no login). |
| **ut4stats.com** | When you view your stats (if linked) or on startup for News | Your public ut4stats player id when fetching your own stats; nothing identifying for News. |
| **PUG bot** (`ut4stats.com`, launcher API) | Only if you've set a PUG launcher token — to join/leave/spectate and poll queue status (every few seconds while the token is set) | Your PUG launcher token (in a header, over HTTPS); the bot resolves your identity from it. |
| **Your browser** | When you click a Discord / "Get UT4" / release / source link | The launcher just opens the URL in your default browser (HTTPS only); it sends nothing itself. |

That's the complete list. There is no other outbound connection.

## What it does not do

- No analytics, usage metrics, crash reporting, or telemetry.
- No advertising or third-party trackers.
- No background daemon, tray agent, or timer that contacts a server when you
  aren't actively using a feature (the only recurring call is PUG queue polling,
  and only while you have a PUG token set).
- No selling, sharing, or aggregating of your data — there is no server that
  receives any of it beyond the community services listed above.

## Transport security

Network requests use TLS validated against **bundled root certificates**
(rustls + webpki) rather than the OS trust store, and downloaded update
artifacts are additionally verified against a signature in the signed manifest
before use (see [`README.md` → Security](README.md#security)).

## Beta + contact

This is a beta; this document describes current behavior and may change as
features land. Questions or concerns: see [`SECURITY.md`](SECURITY.md) for
anything security-sensitive, otherwise open an issue.
