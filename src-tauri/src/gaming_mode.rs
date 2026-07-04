//! (Linux) "Gaming mode": while the game runs, keep its window genuinely
//! fullscreen-and-focused and keep the Ubuntu dock out of the way.
//!
//! Why this exists (diagnosed live on GNOME 46 / X11, 2026-07-04): mutter can
//! leave the Wine/Proton fullscreen game window **unfocused and
//! `_NET_WM_STATE_HIDDEN`** (`_NET_ACTIVE_WINDOW` = 0) even though the game
//! renders and plays fine via Wine's input grab. Consequences: the GNOME top
//! bar draws over the game, the dock's edge pressure barrier reveals it over
//! the game, and mutter composites every frame instead of unredirecting the
//! fullscreen window (extra latency). Sending `_NET_ACTIVE_WINDOW` and then
//! re-adding `_NET_WM_STATE_FULLSCREEN` (activation drops it) fixes all three
//! — that is exactly what the watchdog does.
//!
//! Lifecycle: [`engage_after_launch`] runs in the launcher right after the
//! game spawns. It optionally disables the Ubuntu dock (GNOME only, marker
//! file written FIRST so a crash can never strand the dock disabled) and
//! spawns THIS SAME BINARY detached with `--gaming-mode-watchdog`.
//! [`handle_watchdog_flag`] intercepts that flag in `run()` before Tauri (and
//! its single-instance plugin) is touched, so the watchdog is a headless
//! helper process: it outlives the launcher (the "close launcher on game
//! launch" preference keeps working), waits for the game window, repairs its
//! state, and restores the dock when the game exits. A younger watchdog
//! defer-waits on an older one (start-time/pid tie-break) and takes the
//! session over if the older one exits first. [`startup_recovery`] re-enables
//! the dock at the next launcher start if a crash left the marker behind.
//!
//! Everything here is best-effort: a missing `gnome-extensions`, a Wayland
//! session, or an X error must never break a launch. The repair logic is
//! deliberately conservative — see [`repair_window_state`]: raising a hidden
//! window happens only until the game is first shown correctly (the `established`
//! latch) and only when nothing (or the launcher) holds focus, and FULLSCREEN
//! is re-added only to an already ~monitor-sized focused window — so it can
//! never fight a player who alt-tabs away (including to a second Proton game),
//! minimizes after startup, or intentionally plays windowed. "Monitor-sized" is
//! measured against the window's actual monitor via RandR, not the combined
//! multi-head X root. The poll is adaptive: fast (sub-second) while a window is
//! in the wrong state so startup and map-change repairs are near-instant, slow
//! once it's confirmed fullscreen+focused.

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use sysinfo::{Pid, ProcessRefreshKind, System, UpdateKind};
    use x11rb::connection::Connection;
    use x11rb::protocol::randr::ConnectionExt as _;
    use x11rb::protocol::xproto::{
        Atom, AtomEnum, ClientMessageEvent, ConnectionExt, EventMask, Window, CLIENT_MESSAGE_EVENT,
    };
    use x11rb::rust_connection::RustConnection;

    const WATCHDOG_FLAG: &str = "--gaming-mode-watchdog";
    const DOCK_EXTENSION: &str = "ubuntu-dock@ubuntu.com";
    const APP_IDENTIFIER: &str = "org.netcodeplus.launcher";
    /// Marker recording that WE disabled the dock (never re-enable a dock the
    /// user keeps disabled themselves). Lives in the app config dir.
    const DOCK_MARKER: &str = "gaming-mode-dock-disabled";
    /// How long the watchdog waits for the game window to appear before giving
    /// up (slow prefix warm-up, shader compile).
    const APPEAR_TIMEOUT: Duration = Duration::from_secs(300);
    /// Poll cadence. Fast while the window is in a wrong state (so startup and
    /// post-map-change repairs land in well under a second instead of the
    /// several-second flash a 2 s poll produced); slow once it's confirmed
    /// fullscreen+focused, so a stable session barely wakes.
    const POLL_FAST: Duration = Duration::from_millis(400);
    const POLL_SLOW: Duration = Duration::from_secs(2);
    /// The game window must stay gone THIS long before we treat it as game exit.
    /// Time-based (not tick-based) so it's independent of the adaptive poll rate:
    /// UE4 destroys/recreates its X window across display-mode changes, and the
    /// splash-to-game transition leaves a gap — a brief disappearance must not
    /// end the session and restore the dock mid-game.
    const EXIT_GRACE: Duration = Duration::from_secs(12);
    /// Focus-steal (activation) is only attempted BEFORE the game has ever been
    /// shown fullscreen+focused (the [`RepairCtx::established`] latch), and at
    /// most [`MAX_ACTIVATIONS`] times. Once the game has appeared correctly even
    /// once, every later HIDDEN/unfocused state is the player's own choice — an
    /// alt-tab (mutter transiently reports the game HIDDEN with nothing focused)
    /// or a minimize — so the watchdog must never yank focus back. Activation
    /// only exists to rescue the startup "window comes up minimized/unfocused"
    /// bug; a later map change drops FULLSCREEN WITHOUT hiding the window, which
    /// the re-fullscreen path (no budget, monitor-size-guarded) handles.
    const MAX_ACTIVATIONS: u32 = 5;
    /// A window counts as "meant to be fullscreen" when it covers at least this
    /// fraction of its MONITOR in BOTH dimensions (see [`monitor_sizes`] — using
    /// the whole multi-monitor X screen here would wrongly reject a single-head
    /// fullscreen game). Anti-hijack guard for the FULLSCREEN re-add: a
    /// deliberately-windowed (small) game never matches; only a game already
    /// ~monitor-sized that lost the FULLSCREEN atom gets it back.
    const FULLSCREEN_FRAC_NUM: u64 = 9;
    const FULLSCREEN_FRAC_DEN: u64 = 10;
    /// Absolute minimum window area (px²) for a game-window candidate, to skip
    /// the same-class helper windows a Proton game spawns (1×1 IME, tiny Input).
    /// A real game window is orders of magnitude larger; this is monitor-agnostic
    /// so it's correct on any head count.
    const MIN_CANDIDATE_AREA: u64 = 256 * 256;

    pub fn handle_watchdog_flag() {
        if std::env::args().any(|a| a == WATCHDOG_FLAG) {
            watchdog_main();
            std::process::exit(0);
        }
    }

    pub fn engage_after_launch(state_path: &std::path::Path) {
        if !gaming_mode_enabled(Some(state_path)) {
            return;
        }
        secure_dock();
        if !spawn_watchdog() {
            // No watchdog means nothing would ever restore the dock on game
            // exit — undo immediately rather than waiting for the next
            // launcher start's recovery.
            restore_dock_if_marked();
        }
    }

    pub fn startup_recovery() {
        let Some(marker) = marker_path() else { return };
        if !marker.exists() {
            return;
        }
        // A live watchdog will restore the dock itself when the game exits.
        if any_rival_watchdog() {
            return;
        }
        restore_dock_if_marked();
    }

    // ---------- settings / dock ----------

    /// The saved `linux_gaming_mode` opt-in. `state_path` is used when the
    /// caller has one (the launcher command); the watchdog resolves the state
    /// file app-handle-free via the config dir, like the DMABUF default does.
    fn gaming_mode_enabled(state_path: Option<&std::path::Path>) -> bool {
        let owned;
        let path = match state_path {
            Some(p) => p,
            None => {
                let Some(p) = dirs::config_dir().map(|d| d.join(APP_IDENTIFIER).join("state.json"))
                else {
                    return false;
                };
                owned = p;
                &owned
            }
        };
        ncp_host::state::read(path)
            .ok()
            .flatten()
            .and_then(|s| s.linux_gaming_mode)
            .unwrap_or(false)
    }

    /// Disable the dock for the session, marker-first: a marker without a
    /// disable is benign (restore is an idempotent `gnome-extensions enable`),
    /// but a disable without a marker would strand the dock disabled forever —
    /// every restore path is keyed on the marker.
    fn secure_dock() {
        if !is_gnome() || !dock_extension_enabled() {
            return;
        }
        let Some(marker) = marker_path() else { return };
        if std::fs::write(&marker, b"").is_err() {
            return; // no recovery path without the marker -> don't disable
        }
        if !set_dock_enabled(false) {
            let _ = std::fs::remove_file(&marker);
        }
    }

    fn restore_dock_if_marked() {
        let Some(marker) = marker_path() else { return };
        if marker.exists() {
            set_dock_enabled(true);
            let _ = std::fs::remove_file(&marker);
        }
    }

    fn is_gnome() -> bool {
        std::env::var("XDG_CURRENT_DESKTOP")
            .map(|d| d.to_ascii_lowercase().contains("gnome"))
            .unwrap_or(false)
    }

    fn dock_extension_enabled() -> bool {
        Command::new("gnome-extensions")
            .args(["list", "--enabled"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(DOCK_EXTENSION))
            .unwrap_or(false)
    }

    fn set_dock_enabled(enabled: bool) -> bool {
        Command::new("gnome-extensions")
            .args([if enabled { "enable" } else { "disable" }, DOCK_EXTENSION])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn marker_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join(APP_IDENTIFIER).join(DOCK_MARKER))
    }

    // ---------- watchdog process ----------

    fn spawn_watchdog() -> bool {
        // The on-disk AppImage when packaged that way (a fresh runtime mount
        // with its own lifecycle — current_exe would point into THIS process's
        // FUSE mount, which goes away when the launcher exits), else the real
        // binary (deb install / dev build).
        let bin = ncp_host::linux::appimage_path(std::env::var("APPIMAGE").ok().as_deref())
            .or_else(|| std::env::current_exe().ok());
        let Some(bin) = bin else { return false };
        Command::new(bin)
            .arg(WATCHDOG_FLAG)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
    }

    /// A `/proc` scan of flag-carrying processes. `cmd` must be requested
    /// EXPLICITLY: sysinfo 0.30's plain `refresh_processes()` never populates
    /// argv, which would make every check here silently return false.
    /// Excludes this process and its ancestors — under AppImage packaging the
    /// runtime wrapper (our parent, which mounts the FUSE image and waits)
    /// carries the same watchdog argv.
    fn rival_scan() -> (System, Pid, HashSet<Pid>) {
        let me = Pid::from_u32(std::process::id());
        let mut sys = System::new();
        sys.refresh_processes_specifics(ProcessRefreshKind::new().with_cmd(UpdateKind::Always));
        let mut exclude: HashSet<Pid> = HashSet::from([me]);
        let mut cur = me;
        while let Some(parent) = sys.process(cur).and_then(sysinfo::Process::parent) {
            if !exclude.insert(parent) || exclude.len() > 32 {
                break; // pid-reuse cycle guard / absurd depth
            }
            cur = parent;
        }
        (sys, me, exclude)
    }

    fn any_rival_watchdog() -> bool {
        let (sys, _, exclude) = rival_scan();
        sys.processes().iter().any(|(pid, proc_)| {
            !exclude.contains(pid) && proc_.cmd().iter().any(|a| a == WATCHDOG_FLAG)
        })
    }

    /// A rival watchdog STRICTLY OLDER than this process (start-time, then pid
    /// — a total order, so two watchdogs spawned simultaneously can never
    /// mutually defer and both exit).
    fn older_rival_watchdog() -> bool {
        let (sys, me, exclude) = rival_scan();
        let my_key = (
            sys.process(me)
                .map(sysinfo::Process::start_time)
                .unwrap_or(0),
            me.as_u32(),
        );
        sys.processes().iter().any(|(pid, proc_)| {
            !exclude.contains(pid)
                && proc_.cmd().iter().any(|a| a == WATCHDOG_FLAG)
                && (proc_.start_time(), pid.as_u32()) < my_key
        })
    }

    fn watchdog_main() {
        // Single active watchdog: while an older one lives it owns the session
        // (it keeps watching ANY matching game window, including one we were
        // spawned for). Wait rather than exit — if it ends in the gap between
        // our game's spawn and its window appearing, take the session over.
        let mut deferred = false;
        while older_rival_watchdog() {
            deferred = true;
            std::thread::sleep(Duration::from_secs(10));
        }
        if deferred {
            // The previous watchdog restored the dock and cleared the marker
            // on ITS exit; re-secure for the session we now own — unless the
            // user toggled the feature off in the meantime.
            if !gaming_mode_enabled(None) {
                return;
            }
            secure_dock();
        }
        watch_game_window();
        // Game over (or it never appeared, or X is unreachable): restore the
        // dock if it was disabled for this session.
        restore_dock_if_marked();
    }

    /// Pids of live UT4 shipping clients, matched on argv (the kernel's comm
    /// name is truncated to 15 chars). Used to pick OUR game's window out of
    /// the class candidates.
    fn ue4_pids() -> HashSet<u32> {
        let mut sys = System::new();
        sys.refresh_processes_specifics(ProcessRefreshKind::new().with_cmd(UpdateKind::Always));
        sys.processes()
            .iter()
            .filter(|(_, p)| {
                p.cmd()
                    .iter()
                    .any(|a| a.to_ascii_lowercase().contains("ue4-win64-shipping"))
            })
            .map(|(pid, _)| pid.as_u32())
            .collect()
    }

    // ---------- X11 / EWMH ----------

    struct Atoms {
        net_client_list: Atom,
        net_active_window: Atom,
        net_wm_state: Atom,
        net_wm_state_fullscreen: Atom,
        net_wm_state_hidden: Atom,
        net_wm_pid: Atom,
    }

    /// Session-level repair bookkeeping (created once, when the game window is
    /// first seen, never reset).
    struct RepairCtx {
        /// Focus-steals spent so far (hard cap [`MAX_ACTIVATIONS`]).
        activations: u32,
        /// Set the first time the game is observed fullscreen+focused. Once set,
        /// activation is disabled for the rest of the session, so alt-tabbing or
        /// minimizing is never fought. See [`MAX_ACTIVATIONS`].
        established: bool,
    }

    fn intern(conn: &RustConnection, name: &str) -> Option<Atom> {
        Some(
            conn.intern_atom(false, name.as_bytes())
                .ok()?
                .reply()
                .ok()?
                .atom,
        )
    }

    /// Poll until the game window has been seen and then stays gone for
    /// [`EXIT_GRACE`] (game exit), repairing its WM state along the way. Returns
    /// early if X is unreachable (Wayland-only session, no DISPLAY) or the window
    /// never appears within [`APPEAR_TIMEOUT`].
    fn watch_game_window() {
        let Ok((conn, screen_num)) = x11rb::connect(None) else {
            return;
        };
        let root = conn.setup().roots[screen_num].root;
        let Some(atoms) = (|| {
            Some(Atoms {
                net_client_list: intern(&conn, "_NET_CLIENT_LIST")?,
                net_active_window: intern(&conn, "_NET_ACTIVE_WINDOW")?,
                net_wm_state: intern(&conn, "_NET_WM_STATE")?,
                net_wm_state_fullscreen: intern(&conn, "_NET_WM_STATE_FULLSCREEN")?,
                net_wm_state_hidden: intern(&conn, "_NET_WM_STATE_HIDDEN")?,
                net_wm_pid: intern(&conn, "_NET_WM_PID")?,
            })
        })() else {
            return;
        };

        let started = Instant::now();
        let mut ctx: Option<RepairCtx> = None;
        let mut last_seen = started;
        let mut ever_seen = false;
        let mut poll = POLL_FAST;
        loop {
            std::thread::sleep(poll);
            match find_game_window(&conn, root, &atoms) {
                Some(win) => {
                    ever_seen = true;
                    last_seen = Instant::now();
                    // Session-level budget: created once, on first sighting.
                    let ctx = ctx.get_or_insert(RepairCtx {
                        activations: 0,
                        established: false,
                    });
                    let good = repair_window_state(&conn, root, &atoms, win, ctx);
                    // Stay fast until the window is confirmed good, then back off.
                    poll = if good { POLL_SLOW } else { POLL_FAST };
                }
                None => {
                    poll = POLL_FAST;
                    if ever_seen && last_seen.elapsed() > EXIT_GRACE {
                        return; // game exited
                    }
                    if !ever_seen && started.elapsed() > APPEAR_TIMEOUT {
                        return; // never showed up
                    }
                }
            }
        }
    }

    /// The game window: among `_NET_CLIENT_LIST` windows with a game
    /// `WM_CLASS`, the **largest** one owned by a live UT4 shipping process.
    /// Two filters, both load-bearing:
    /// - **PID** (`_NET_WM_PID` ∈ live UE4 pids): `steam_proton` is Proton-Wine's
    ///   class for EVERY non-Steam Proton window on the desktop (another
    ///   launcher's game, a winedbg dialog), so class alone is too broad.
    /// - **Size** (≥ [`MIN_CANDIDATE_AREA`]): a single Proton game spawns many
    ///   same-class helper windows — a 1×1 IME window, a tiny "Input" window — so
    ///   picking "newest" or "first" would latch onto a 1×1 child. An ABSOLUTE
    ///   floor, not a screen fraction: a screen-relative floor computed off the
    ///   whole (possibly multi-monitor) X root would wrongly reject a real
    ///   single-head game window. The genuine game window then wins on max area.
    fn find_game_window(conn: &RustConnection, root: Window, atoms: &Atoms) -> Option<Window> {
        let reply = conn
            .get_property(
                false,
                root,
                atoms.net_client_list,
                AtomEnum::WINDOW,
                0,
                4096,
            )
            .ok()?
            .reply()
            .ok()?;
        let windows: Vec<Window> = reply.value32()?.collect();
        let ue4 = ue4_pids();
        // Class-matched, big-enough windows, tagged by whether a live UT4
        // process owns them.
        let candidates: Vec<(Window, bool, u64)> = windows
            .into_iter()
            .filter(|&w| {
                wm_class_of(conn, w)
                    .map(|c| ncp_host::linux::is_game_window_class(&c))
                    .unwrap_or(false)
            })
            .map(|w| {
                let (ww, wh) = window_dims(conn, w);
                let is_ue4 = wm_pid_of(conn, w, atoms)
                    .map(|p| ue4.contains(&p))
                    .unwrap_or(false);
                (w, is_ue4, ww as u64 * wh as u64)
            })
            .filter(|&(_, _, area)| area >= MIN_CANDIDATE_AREA)
            .collect();
        // Prefer the largest UT4-owned window; fall back to the largest
        // class+size match if none expose a matching `_NET_WM_PID`.
        candidates
            .iter()
            .filter(|&&(_, is_ue4, _)| is_ue4)
            .max_by_key(|&&(_, _, area)| area)
            .or_else(|| candidates.iter().max_by_key(|&&(_, _, area)| area))
            .map(|&(w, _, _)| w)
    }

    /// A window's current width and height in pixels, `(0, 0)` if unavailable.
    fn window_dims(conn: &RustConnection, win: Window) -> (u16, u16) {
        conn.get_geometry(win)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|g| (g.width, g.height))
            .unwrap_or((0, 0))
    }

    /// Per-monitor `(width, height)` from RandR — so "screen-sized" is measured
    /// against the monitor a window occupies, not the whole multi-head X root
    /// (a fullscreen game on one head is far smaller than the combined desktop).
    /// Falls back to the root screen size if RandR is unavailable (single-head
    /// setups then still work; multi-head without RandR is vanishingly rare).
    fn monitor_sizes(conn: &RustConnection, root: Window) -> Vec<(u16, u16)> {
        if let Some(reply) = conn
            .randr_get_monitors(root, true)
            .ok()
            .and_then(|c| c.reply().ok())
        {
            let mons: Vec<(u16, u16)> =
                reply.monitors.iter().map(|m| (m.width, m.height)).collect();
            if !mons.is_empty() {
                return mons;
            }
        }
        conn.get_geometry(root)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|g| vec![(g.width, g.height)])
            .unwrap_or_default()
    }

    /// Whether `win`'s size covers ≥ [`FULLSCREEN_FRAC_NUM`]/[`FULLSCREEN_FRAC_DEN`]
    /// of ANY monitor in both dimensions — the "meant to be fullscreen" test.
    fn is_monitor_sized(conn: &RustConnection, root: Window, win: Window) -> bool {
        let (ww, wh) = window_dims(conn, win);
        let (ww, wh) = (ww as u64, wh as u64);
        monitor_sizes(conn, root).into_iter().any(|(mw, mh)| {
            ww * FULLSCREEN_FRAC_DEN >= mw as u64 * FULLSCREEN_FRAC_NUM
                && wh * FULLSCREEN_FRAC_DEN >= mh as u64 * FULLSCREEN_FRAC_NUM
        })
    }

    /// A window's `WM_CLASS` (both nul-separated fields, joined) — missing
    /// property means "not our window".
    fn wm_class_of(conn: &RustConnection, win: Window) -> Option<String> {
        let reply = conn
            .get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)
            .ok()?
            .reply()
            .ok()?;
        if reply.value.is_empty() {
            return None;
        }
        Some(String::from_utf8_lossy(&reply.value).replace('\0', " "))
    }

    fn wm_pid_of(conn: &RustConnection, win: Window, atoms: &Atoms) -> Option<u32> {
        conn.get_property(false, win, atoms.net_wm_pid, AtomEnum::CARDINAL, 0, 1)
            .ok()?
            .reply()
            .ok()?
            .value32()?
            .next()
    }

    fn window_states(conn: &RustConnection, win: Window, atoms: &Atoms) -> Vec<Atom> {
        conn.get_property(false, win, atoms.net_wm_state, AtomEnum::ATOM, 0, 64)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().map(Iterator::collect))
            .unwrap_or_default()
    }

    fn active_window(conn: &RustConnection, root: Window, atoms: &Atoms) -> Window {
        conn.get_property(false, root, atoms.net_active_window, AtomEnum::WINDOW, 0, 1)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().and_then(|mut v| v.next()))
            .unwrap_or(0)
    }

    /// One repair step per poll tick. Returns `true` when the window is already
    /// in the desired state (fullscreen, not hidden, and the active window) so
    /// the caller can back off to the slow poll. Two independent repairs, each
    /// with an anti-hijack guard so the watchdog never fights the player:
    ///
    /// - **Raise** — the window is minimized/`HIDDEN` while nothing is focused or
    ///   the launcher is (the broken-launch state: the game comes up minimized
    ///   with `_NET_ACTIVE_WINDOW` = 0). Activate it. Only BEFORE the game has
    ///   ever been shown fullscreen+focused ([`RepairCtx::established`]) and
    ///   bounded to [`MAX_ACTIVATIONS`] — so once the game has appeared correctly
    ///   once, alt-tabbing (mutter briefly reports the game HIDDEN with nothing
    ///   focused) or minimizing is never fought. Deliberately does NOT treat
    ///   "some other window is focused" as free either: that is the player's
    ///   deliberate choice.
    /// - **Re-fullscreen** — the window is focused (or nothing else is) and
    ///   ~monitor-sized but lost the `_NET_WM_STATE_FULLSCREEN` atom. UE4 drops it
    ///   on a resolution/mode change (map load), leaving a focused non-fullscreen
    ///   window that mutter no longer unredirects, so the top bar draws over the
    ///   game. Re-add it. The monitor-size guard is the anti-hijack: a
    ///   deliberately-windowed (small) game never matches, and an alt-tabbed
    ///   player has focus on some other window (so `active != win`).
    fn repair_window_state(
        conn: &RustConnection,
        root: Window,
        atoms: &Atoms,
        win: Window,
        ctx: &mut RepairCtx,
    ) -> bool {
        let states = window_states(conn, win, atoms);
        let hidden = states.contains(&atoms.net_wm_state_hidden);
        let fullscreen = states.contains(&atoms.net_wm_state_fullscreen);
        let active = active_window(conn, root, atoms);
        if fullscreen && !hidden && active == win {
            ctx.established = true; // shown correctly at least once
            return true; // already correct — nothing to do
        }

        let active_is_launcher = active != 0
            && wm_class_of(conn, active)
                .map(|c| c.to_ascii_lowercase().contains("netcodeplus-launcher"))
                .unwrap_or(false);
        // Focus is not a deliberate elsewhere-choice by the player only when
        // nothing at all is focused, or the launcher is. NOT when another window
        // (even a game-class one) holds focus — that is the player's choice.
        let focus_free = active == 0 || active_is_launcher;

        // Raise ONLY during the initial establish phase. After the game has been
        // shown fullscreen+focused once, a HIDDEN+nothing-focused state is the
        // player alt-tabbing or minimizing — never fight it.
        if !ctx.established && hidden && focus_free && ctx.activations < MAX_ACTIVATIONS {
            // data.l = [source, timestamp, requestor's active window, 0, 0];
            // source 2 = direct user action (trusted, like wmctrl -a).
            send_root_message(conn, root, win, atoms.net_active_window, [2, 0, 0, 0, 0]);
            ctx.activations += 1;
            return false;
        }

        if !fullscreen
            && !hidden
            && (active == win || active == 0)
            && is_monitor_sized(conn, root, win)
        {
            // data.l = [_NET_WM_STATE_ADD, state atom, 0, source, 0].
            send_root_message(
                conn,
                root,
                win,
                atoms.net_wm_state,
                [1, atoms.net_wm_state_fullscreen, 0, 2, 0],
            );
        }
        false
    }

    fn send_root_message(
        conn: &RustConnection,
        root: Window,
        win: Window,
        message_type: Atom,
        data: [u32; 5],
    ) {
        let event = ClientMessageEvent {
            response_type: CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window: win,
            type_: message_type,
            data: data.into(),
        };
        let _ = conn.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        );
        let _ = conn.flush();
    }
}

#[cfg(target_os = "linux")]
pub use imp::{engage_after_launch, handle_watchdog_flag, startup_recovery};

#[cfg(not(target_os = "linux"))]
pub fn handle_watchdog_flag() {}

#[cfg(not(target_os = "linux"))]
pub fn engage_after_launch(_state_path: &std::path::Path) {}

#[cfg(not(target_os = "linux"))]
pub fn startup_recovery() {}
