# UT4AC opt-in distribution — design

Distribute the **UT4AC anti-cheat client module** through the launcher as a
strictly **opt-in** add-on: a player who never asks for it never receives a
byte of it, a player who opts in gets sha-verified auto-updates from the
signed manifest, and a player who opts out again is left with a provably
clean install.

> Status: **design draft — four decisions pending (§8).** Drafted 2026-08-23
> against launcher `1.7.4` + the `perf-config-fixes` branch. No code written.
> The §8 TBDs must be resolved with phantaci before §5 (install mechanics)
> is implementable; everything else is buildable as specced.

## The core reality

UT4AC is **closed source** and the rest of the stack is not. The community
runs an open-source plugin (NetcodePlus) delivered by an open-source launcher
over a signed manifest — that openness is the trust story, and the anti-cheat
must not silently spend it. The author's position is explicit: **players must
never be forced to run closed-source code.** At the same time, rated matches
and cups need an enforcement story, and "DM this Discord user for a DLL" is
neither auditable nor updatable.

The launcher already owns every hard part of this problem for other features:
a signed, sequence-numbered manifest with sha-pinned artifacts; an atomic
sha-verified installer with rollback (`editor_plugin.rs`); per-item consent
UI (the 1.7.4 "Choose paks" picker); a running-game guard
(`shipping_client_running()`); and an Add-ons tab whose whole purpose is
"optional extras the player explicitly chooses" (UT4-OpenAL lives there).
This design is mostly wiring those parts together around one new thing:
**a consent record with a revision number.**

## 1. Principles (locked)

1. **Absent by default.** No AC files on disk, no consent recorded, no nag,
   no onboarding step. The card sits on the Add-ons tab and waits.
2. **The Install click is the consent act**, and it sits behind a disclosure
   the player actually sees (§3.2). Consent is recorded locally with the
   manifest's `consent_rev` at the time of the click.
3. **Updates ride the consent; scope changes re-ask.** Same `consent_rev` →
   silent sha-verified auto-update, like the plugin. A `consent_rev` bump
   (the module starts monitoring something new) → the card returns to a
   "review & re-approve" state and the OLD version keeps running until the
   player approves. Never expand monitoring under an old yes.
4. **Uninstall is one click** and total: delete the module file(s), clear the
   consent record. The open-source loader (§5) makes "absent = inert"
   verifiable by anyone.
5. **Enforcement is server-side and per-match, never client-side coercion**
   (§6). Opting out costs access to AC-required matches — nothing else.
6. **Same trust chain as everything else**: artifact on the NetcodePlusUT4
   releases page, sha256 + size pinned in the rsign-signed manifest, verified
   before the atomic install. No side channels.

## 2. Reuse surface

| Need | Existing part | Where |
|---|---|---|
| Tamper-proof delivery | signed manifest, sequence + sha pinning | `crates/manifest` |
| Optional manifest block, absent-tolerant | `editor_plugins` pattern + back-compat test | `crates/manifest/src/schema.rs` |
| Atomic verified install + rollback | editor-plugin installer (sibling, validate, swap) | `crates/host/src/editor_plugin.rs` |
| Refuse while game runs | `shipping_client_running()` | `src-tauri/src/commands.rs` |
| Opt-in surface | Add-ons tab (UT4-OpenAL precedent) | `src/main.ts` |
| Consent persistence | launcher settings / onboarding-completion store | `src-tauri` state |
| Player-facing status | plugin_status / editor_plugin_status DTO pattern | `crates/planner`, `src-tauri` |

## 3. Consent model

### 3.1 The record

Stored in launcher state (not in the game tree, so an uninstall/reinstall of
UT4 does not resurrect consent):

```
anticheat_consent: {
  granted_at: <iso8601>,
  consent_rev: <u32 from the manifest entry at grant time>,
  installed_version: <semver string>,
}
```

Absent record = never consented = the launcher never downloads the artifact.
Cleared on uninstall. `consent_rev` mismatch vs the live manifest = updates
paused + card shows "UT4AC wants to monitor something new — review".

### 3.2 The disclosure

Shown on the card BEFORE the Install button does anything, and re-shown on a
`consent_rev` bump with a diff-style "what changed" note. Content (final text
TBD-4 feeds this):

- UT4AC is **closed source**, unlike everything else the launcher installs,
  and why (anti-cheat efficacy dies with source access).
- **What it monitors** while a match is running (fire events, render/memory
  floor enforcement — the concrete list comes from the current spec).
- **What leaves the machine, and where it goes** (TBD-4: endpoint + retention).
- What it does NOT do (no browsing data, no keylogging, no background service
  — it runs only while UT4 runs; adjust to the truth of TBD-1).
- Versioned: this disclosure text is part of the repo, hash-referenced in
  release notes, so "what did I agree to" always has an answer.

## 4. Distribution & integrity

New **optional** manifest block, shaped like `editor_plugins`:

```jsonc
"anticheat": {
  "ut4ac": {
    "version": "2.1.0",
    "url": "https://github.com/jmortley/NetcodePlusUT4/releases/download/ut4ac-latest/<artifact>",
    "sha256": "…",
    "size_bytes": 1234567,
    "consent_rev": 1,
    "min_plugin_version": 329,        // see TBD-3
    "disclosure_note": "initial release"   // one line shown on rev bumps
  }
}
```

- **Absent block = feature invisible.** Same additive back-compat rule (and
  test) as `editor_plugins`: every manifest shipped so far parses to an empty
  map. Ship the launcher first; publishing the block activates the card —
  the same dormant-activation trick the editor-plugins channel uses.
- Artifact hosted on the **NetcodePlusUT4 releases page** like the plugin
  zip; sha256 verified after download, before the atomic swap.
- The launcher never installs a version whose `min_plugin_version` exceeds
  the installed plugin — it shows "waiting for plugin update" instead.

## 5. Install mechanics — blocked on TBD-1/TBD-2

The intended architecture (to confirm against dogfood reality):

- **The open-source plugin is the loader.** NetcodePlus probes ONE canonical
  path (proposal: `Plugins/NetcodePlus/AC/`), loads the module if present,
  logs `[UT4AC] loaded <file> version <v>` to the client log, and is a clean
  no-op when the directory is absent. The loader being open source is the
  transparency story: anyone can read exactly when and how the closed code is
  invoked, and verify that absence means inert.
- The launcher's install = download → sha-verify → atomic write into that
  directory (reuse the editor-plugin machinery: temp sibling, validate,
  swap, roll back on failure). Uninstall = delete the directory + clear
  consent. Both refuse while `UE4-Win64-Shipping.exe` runs.
- Windows-only at first (the module is a Windows DLL; Linux is out of scope
  until UT4AC itself is).

## 6. Server-side enforcement (why opt-in stays real)

Mirrors the `NCPlusVersionGate` advisor pattern:

- The plugin reports AC presence + version alongside its own version
  handshake. Servers/rulesets carry an `ac_required` flag (cups, rated pugs).
- A player without UT4AC joining an AC-required match gets a clear whisper —
  *"This match requires UT4AC. It's an optional install in the launcher's
  Add-ons tab."* — and is moved to spectate or declined at match join,
  exactly how version mismatches are handled today.
- Casual/unrated servers never check. The player who opts out loses rated
  privileges, not the game.
- Version skew (player has AC but older than the hub requires) reuses the
  same whisper with "update via the launcher".

## 7. UI surface

One card on the **Add-ons tab**, four states:

1. **Not installed** — name, one-line summary, `Learn more / Install` (Install
   opens the §3.2 disclosure with an explicit confirm).
2. **Installed & current** — version, `Installed ✓`, `Uninstall`, link to the
   disclosure as-agreed.
3. **Update pending, same consent_rev** — no state; it just updates like the
   plugin, with a line in the update feed.
4. **Consent rev bumped** — amber "review & re-approve" state: shows
   `disclosure_note` + full new disclosure; old version keeps working until
   approved; `Uninstall` always available.

No mention on the dashboard, in onboarding, or in what's-new until installed
— discoverability is the Add-ons tab plus server whispers (§6), which reach
exactly the players who have a reason to care.

## 8. Open questions (TBD — resolve with phantaci before §5)

- **TBD-1 — load mechanism today.** How do dogfood testers run UT4AC right
  now: does the plugin already contain a loader (and at what path), or is it
  attached some other way? Decides whether §5's loader is new plugin work
  (targeting 329) or already exists.
- **TBD-2 — artifact shape.** One DLL or several files? Any config/sidecar
  that belongs in the artifact? Decides the zip layout + validation rules.
- **TBD-3 — version coupling.** Must the AC protocol version gate against the
  plugin version (schema/proto coupling), i.e. is `min_plugin_version`
  sufficient, or is it a two-way constraint (plugin also refuses too-old AC)?
- **TBD-4 — telemetry disclosure.** Exact endpoint(s) the module reports to,
  what is stored, for how long, and under what identifier — the disclosure
  text (§3.2) cannot be written honestly without this.

## 9. Phased plan

1. **Phase 0 — decisions.** Resolve §8. Write the disclosure text and commit
   it to the repo (it version-controls the promise).
2. **Phase 1 — plugin loader** (NetcodePlus repo, if TBD-1 says it's needed):
   canonical path probe + logged load + presence in the version handshake.
   Ships dormant in a normal plugin roll; inert without files.
3. **Phase 2 — launcher**: manifest schema block (+ absent-tolerance test),
   consent store, installer/uninstaller commands (reusing editor-plugin
   machinery + running-game guard), Add-ons card with the four states.
   Ships in a normal launcher release; invisible without the manifest block.
4. **Phase 3 — activation**: upload the artifact to the NetcodePlusUT4
   releases page, add the `anticheat` block, bump sequence, sign, publish.
   Feature goes live for every launcher ≥ Phase 2 with zero further installs.
5. **Phase 4 — enforcement**: hubs mark AC-required rulesets; whisper flow.

## 10. Explicitly out of scope

- Any auto- or default-installation of the module, under any flag.
- Bundling the module inside the plugin zip or a required pak (it must never
  arrive as a side effect of something else).
- Linux/Proton support (revisit when UT4AC has a story there).
- Kernel-mode anything. UT4AC is a user-mode module loaded by the game.
- Server-side UT4AC distribution (hub operators are a different, manual
  audience).
