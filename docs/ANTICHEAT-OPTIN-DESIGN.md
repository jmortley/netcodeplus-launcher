# UT4AC opt-in distribution — design

Distribute the **UT4AC anti-cheat client module** through the launcher as a
strictly **opt-in** add-on: a player who never asks for it never receives a
byte of it, a player who opts in gets sha-verified auto-updates from the
signed manifest, and a player who opts out again is left with a provably
clean install.

> Status: **decisions resolved — implementable.** Drafted 2026-08-23; §8
> resolved with phantaci 2026-08-27 (answers inline) and §5 rewritten against
> the on-disk reality: UT4AC is a standard sibling UE4 plugin, so the engine's
> own plugin system is the loader and no custom loader exists or is needed.
> One input still owed before the disclosure text ships: the evidence
> retention window (§8 TBD-4 note).

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

## 5. Install mechanics — RESOLVED (rewritten 2026-08-27 against dogfood reality)

UT4AC is a **standard sibling UE4 plugin**, not a module NetcodePlus loads.
Ground truth from the dogfood artifact (`UT4AC.zip`, hand-unzipped by
phantaci — today's only dogfooder):

- Install location: `<root>/UnrealTournament/Plugins/UT4AC/` — a normal
  plugin folder beside NetcodePlus.
- Artifact shape: a plugin-folder zip — `UT4AC.uplugin` at the root plus
  `Binaries/**` (Win64 client/server/editor DLLs, Linux server `.so`; the
  dogfood zip also carries PDBs — trimming them is a Phase-3 packaging call,
  the installer doesn't care). Two modules: `UT4AC` (Runtime, Default phase,
  Win+Linux — the review-only server/evidence side) and `UT4ACClient`
  (Runtime, PostEngineInit, Win64-only). `EnabledByDefault: true`; declares
  plugin dependencies on UnrealTournament + NetcodePlus.
- **The engine's plugin system is the loader.** Folder present → UE4 loads
  it like any other plugin; folder absent → nothing loads, engine-guaranteed.
  That is a *stronger* absent-=-inert story than the drafted custom loader:
  it rests on stock engine behavior anyone can verify, not on our code.
  NetcodePlus's only role is **reporting** — AC presence + version in the
  version-gate handshake so servers can enforce (§6).
- Launcher install = download → sha-verify → extract via the NetcodePlus
  plugin-install machinery **generalized by destination** (`plugin_install.rs`:
  zip-slip guards, temp-sibling staging, well-formedness validation
  [`UT4AC.uplugin` + `Binaries/`], atomic swap, rollback) — not
  `editor_plugin.rs`, which targets the editor build tree. Uninstall =
  delete `Plugins/UT4AC/` + clear the consent record. Both refuse while
  `UE4-Win64-Shipping.exe` runs.
- One artifact serves client and server (house style, like the NetcodePlus
  zip): hub operators install the same zip by hand server-side; a client
  only loads the Win64 client module.
- Note: the dogfood tree carries `Config/DefaultUT4AC.ini` but the zip does
  NOT — config is compiled-default today. If a config file ever joins the
  artifact, extend the validation rule alongside.

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

## 8. Open questions — RESOLVED with phantaci, 2026-08-27

- **TBD-1 — load mechanism today.** *Resolved:* there is no loader and none
  is needed. phantaci is the only dogfooder and hand-unzips `UT4AC.zip` into
  `Plugins/UT4AC/`; the engine's plugin system loads it. Phase 1 therefore
  shrinks to the **handshake presence/version report** in NetcodePlus (§5,
  §6) — no loading code anywhere.
- **TBD-2 — artifact shape.** *Resolved:* a plugin-folder zip —
  `UT4AC.uplugin` + `Binaries/**` (two modules; full layout in §5). No
  config sidecar in the artifact today. Validation = uplugin + Binaries
  present, the same well-formedness rule as the NetcodePlus zip.
- **TBD-3 — version coupling.** *Resolved:* **one-way** — the manifest's
  `min_plugin_version` is sufficient; the plugin does not refuse an older AC
  module.
- **TBD-4 — telemetry disclosure.** *Resolved (endpoint + identifier):*
  evidence goes to the **ut4stats Django backend** (the `UT4ACEvidenceBundle`
  / `UT4ACPlayerEvidence` ingestion on `modernize-django42`, staff-only
  review views), keyed to the player's Epic account id — the same identifier
  every match stat already uses. **Still owed: the retention window** — the
  disclosure text must state how long evidence is kept, and that number is
  phantaci's call before Phase 3 activation (not blocking Phase 2 code).

## 9. Phased plan

1. **Phase 0 — decisions.** ✅ Resolved 2026-08-27 (§8). Remaining sliver:
   the retention number for the disclosure text, owed before Phase 3.
2. **Phase 1 — handshake report** (NetcodePlus repo; shrunk by TBD-1): AC
   presence + version in the version-gate handshake so servers can enforce.
   No loader — the engine loads the sibling plugin itself. Ships dormant in
   a normal plugin roll.
3. **Phase 2 — launcher**: manifest schema block (+ absent-tolerance test),
   consent store, installer/uninstaller commands (plugin-install machinery
   generalized by destination + running-game guard), Add-ons card with the
   four states. Ships in a normal launcher release; invisible without the
   manifest block.
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
