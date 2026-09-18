//! The Tauri 2 desktop shell + tray. Display only — every value comes from the core
//! adapter in `lib.rs`. Compiled solely under `--features gui`; never launched, bundled,
//! signed, or WebDriver-tested by the build loop or scripts/ci.sh.

use std::process::{Child, Command};
use std::sync::Mutex;
use tare_core::flamegraph::FlamegraphModel;
use tare_core::model::TodaySpend;
use tauri::menu::{AboutMetadata, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Runtime, State, WindowEvent};
use tauri_plugin_notification::NotificationExt;

/// The primary navigation destinations, mirroring the web rail's SECTIONS shape (main.ts) so the
/// native Go menu / tray and the in-app sidebar stay in sync. `(menu id, label, route)`
/// — the id is `nav:<route>` and the route is the hash the web `navigate()` consumes. Repointed to
/// the three canonical Router-v2 workspaces: the legacy Monitor/Investigate/
/// Act destinations collapse into Pulse / Investigate / Optimize, which the web shell renders via the
/// lazy workspace adapters.
const NAV_DESTINATIONS: &[(&str, &str, &str)] = &[
    ("nav:pulse", "Pulse", "pulse"),
    ("nav:investigate", "Investigate", "investigate"),
    ("nav:optimize", "Optimize", "optimize"),
];

/// The navigation payload a menu/tray item id maps to — the contract the web shell (`bootTauri.ts`)
/// consumes (`nav:<route>` → destination; `run:<id>` → run-scoped Runs; `view:<hash>` → hash).
/// Pure + AppHandle-free so it's unit-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
enum NavTarget {
    /// A saved view carries a full in-app hash (route + query) → navigate by hash directly.
    Hash(String),
    /// A primary destination (`nav:`) or a run-scoped Runs open (`run:`) → `{route, param}`.
    Route {
        route: String,
        param: Option<String>,
    },
}

/// Pure mapping of a menu/tray item id → its navigation payload, or `None` when the id isn't a
/// navigation id. No window/emit side effects — those live in `handle_nav_event`.
fn nav_target(id: &str) -> Option<NavTarget> {
    if let Some(hash) = id.strip_prefix("view:") {
        return Some(NavTarget::Hash(hash.to_string()));
    }
    if let Some(route) = id.strip_prefix("nav:") {
        return Some(NavTarget::Route {
            route: route.to_string(),
            param: None,
        });
    }
    if let Some(run) = id.strip_prefix("run:") {
        return Some(NavTarget::Route {
            route: "runs".to_string(),
            param: Some(run.to_string()),
        });
    }
    None
}

fn handle_nav_event<R: Runtime>(app: &AppHandle<R>, id: &str) -> bool {
    let Some(target) = nav_target(id) else {
        return false;
    };
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
    let payload = match target {
        NavTarget::Hash(hash) => serde_json::json!({ "hash": hash }),
        NavTarget::Route { route, param } => serde_json::json!({ "route": route, "param": param }),
    };
    let _ = app.emit("navigate", payload);
    true
}

/// Build the full tray menu: two disabled status readout rows (budget + capture) at the
/// top, then Open, the nav destinations, and Quit. Rebuilt each refresh tick so the
/// readouts stay current.
fn build_tray_menu<R: Runtime>(
    handle: &AppHandle<R>,
    status: &crate::TrayStatus,
    recent_runs: &[String],
    saved_views: &[crate::SavedInvestigationMenuEntry],
) -> tauri::Result<tauri::menu::Menu<R>> {
    let budget = MenuItemBuilder::with_id("status:budget", &status.budget_row)
        .enabled(false)
        .build(handle)?;
    let capture = MenuItemBuilder::with_id("status:capture", &status.capture_row)
        .enabled(false)
        .build(handle)?;
    let open_i = MenuItemBuilder::with_id("open", "Open Tare").build(handle)?;
    let quit_i = MenuItemBuilder::with_id("quit", "Quit Tare").build(handle)?;
    let mut b = MenuBuilder::new(handle)
        .item(&budget)
        .item(&capture)
        .separator()
        .item(&open_i)
        .separator();
    for (id, label, _route) in NAV_DESTINATIONS {
        b = b.item(&MenuItemBuilder::with_id(*id, *label).build(handle)?);
    }
    // Recent runs: a live-refreshed group; each opens that run on the Runs screen.
    let mut recent = SubmenuBuilder::new(handle, "Recent runs");
    if recent_runs.is_empty() {
        recent = recent.item(
            &MenuItemBuilder::with_id("run:none", "No runs yet")
                .enabled(false)
                .build(handle)?,
        );
    } else {
        for run in recent_runs {
            recent =
                recent.item(&MenuItemBuilder::with_id(format!("run:{run}"), run).build(handle)?);
        }
    }
    b = b.item(&recent.build()?);
    // Saved investigations: projected directly from shared SQLite; each opens its compact v2
    // id through the canonical workspace route. The legacy file is migration-only now.
    let mut views = SubmenuBuilder::new(handle, "Views");
    if saved_views.is_empty() {
        views = views.item(
            &MenuItemBuilder::with_id("view:none", "No saved investigations")
                .enabled(false)
                .build(handle)?,
        );
    } else {
        for v in saved_views {
            views = views.item(
                &MenuItemBuilder::with_id(
                    format!("view:{}", crate::saved_investigation_hash(v)),
                    &v.label,
                )
                .build(handle)?,
            );
        }
    }
    b = b.item(&views.build()?);
    b.separator().item(&quit_i).build()
}

fn install_tray<R: Runtime>(
    handle: &AppHandle<R>,
    status: &crate::TrayStatus,
    recent_runs: &[String],
    saved_views: &[crate::SavedInvestigationMenuEntry],
) -> bool {
    let menu = match build_tray_menu(handle, status, recent_runs, saved_views) {
        Ok(menu) => menu,
        Err(error) => {
            eprintln!("tare: cannot build tray menu: {error}");
            return false;
        }
    };
    let result = TrayIconBuilder::with_id(TRAY_ID)
        .title(status.title.clone())
        .tooltip("Tare")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            let id = event.id().as_ref();
            if handle_nav_event(app, id) {
                return;
            }
            match id {
                "open" => {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
                "quit" => app.exit(0),
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if let Some(window) = tray.app_handle().get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        })
        .build(handle);
    match result {
        Ok(_) => true,
        Err(error) => {
            eprintln!("tare: cannot create tray icon: {error}");
            false
        }
    }
}

/// Tray icon id, so the refresh timer can fetch and update it.
const TRAY_ID: &str = "tare-tray";
/// How often the tray readout refreshes. The menu-bar extra is a glance surface, not a live tail.
const TRAY_REFRESH_SECS: u64 = 30;
/// How many recent runs to list in the tray's Recent-runs group.
const RECENT_RUNS_IN_TRAY: usize = 5;
const DEFAULT_HTTP_PORT: u16 = 8788;
const DEFAULT_OTLP_PORT: u16 = 4318;

/// Persisted window geometry v2: LOGICAL normal bounds (DPI-independent; the
/// un-maximized/un-fullscreen size), the scale factor + monitor name at save time, and whether the
/// window was maximized. `version` distinguishes the legacy v1 record (`{w,h,x,y}` physical, no
/// version), which is migrated on read; serde defaults fill the fields v1 lacked. Manual persistence
/// (avoids a plugin that may not be in the offline cargo cache).
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
struct WinState {
    #[serde(default = "win_state_v1")]
    version: u32,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    #[serde(default)]
    scale: f64,
    #[serde(default)]
    monitor: Option<String>,
    #[serde(default)]
    maximized: bool,
}
fn win_state_v1() -> u32 {
    1
}

/// A logical rectangle — a monitor work area or a window's normal bounds — for the clamp logic.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// Parse a persisted window-state file: a v2 record directly, or a migrated v1 `{w,h,x,y}` physical
/// record (stamped v2, scale defaulted to 1.0 since v1 stored no scale — a minor size drift on HiDPI
/// that the work-area clamp keeps on-screen). `None` on unusable/too-small data.
fn parse_win_state(json: &str) -> Option<WinState> {
    let mut st: WinState = serde_json::from_str(json).ok()?;
    if !(st.w > 200.0 && st.h > 200.0) {
        return None;
    }
    if st.version < 2 {
        st.version = 2;
        if st.scale <= 0.0 {
            st.scale = 1.0;
        }
    }
    Some(st)
}

/// Clamp a logical window rect so it stays on a visible monitor work area — never stranded on a
/// since-disconnected display. If the window's top-left is on no monitor, recenter it
/// on the first (primary) monitor's work area, shrinking to fit. Empty monitor list → unchanged.
fn clamp_to_work_area(win: Rect, monitors: &[Rect]) -> Rect {
    let monitors: Vec<Rect> = monitors
        .iter()
        .copied()
        .filter(|monitor| monitor.w > 0.0 && monitor.h > 0.0)
        .collect();
    if monitors.is_empty() {
        return win;
    }
    // Prefer the monitor containing the draggable top-left; otherwise recover onto the primary.
    let monitor = monitors
        .iter()
        .find(|m| win.x >= m.x && win.y >= m.y && win.x < m.x + m.w && win.y < m.y + m.h)
        .copied()
        .unwrap_or(monitors[0]);
    let w = win.w.min(monitor.w);
    let h = win.h.min(monitor.h);
    let was_on_monitor = win.x >= monitor.x
        && win.y >= monitor.y
        && win.x < monitor.x + monitor.w
        && win.y < monitor.y + monitor.h;
    let (x, y) = if was_on_monitor {
        (
            win.x.clamp(monitor.x, monitor.x + monitor.w - w),
            win.y.clamp(monitor.y, monitor.y + monitor.h - h),
        )
    } else {
        (
            monitor.x + (monitor.w - w) / 2.0,
            monitor.y + (monitor.h - h) / 2.0,
        )
    };
    Rect { x, y, w, h }
}

/// The bounds to persist as the NORMAL window state. While maximized/fullscreen the OS reports the
/// transient full-screen bounds, which must NOT overwrite the remembered normal size;
/// return the previous normal bounds in that case, else the current bounds.
fn normal_bounds(current: Rect, maximized: bool, fullscreen: bool, prev_normal: Rect) -> Rect {
    if maximized || fullscreen {
        prev_normal
    } else {
        current
    }
}

fn win_state_path(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|d| d.join("window.json"))
}

/// The platform, for the close/lifecycle policy. Variants are constructed per
/// compile target (`current_os_kind`) + exercised across targets in the policy matrix test; on any
/// single build the other targets' variants look unconstructed, hence `allow(dead_code)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
enum OsKind {
    Macos,
    Windows,
    Linux,
}

/// What to do when the user closes the main window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseAction {
    /// Quit the app (the window closes normally; capture keeps running in the separate daemon).
    Exit,
    /// Keep running with the window hidden — recoverable via the Dock (macOS) or the tray.
    HideToTray,
}

/// The compile-target platform.
fn current_os_kind() -> OsKind {
    #[cfg(target_os = "macos")]
    {
        OsKind::Macos
    }
    #[cfg(target_os = "windows")]
    {
        OsKind::Windows
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        OsKind::Linux
    }
}

/// The window-close policy. macOS keeps the app running on close
/// (the Dock reactivates the hidden window even with no tray). Windows and Linux EXIT by default;
/// they hide-to-tray only when the user has explicitly opted into background operation AND a tray
/// actually exists to restore from, so a hidden window is never stranded. Linux never depends on a
/// tray for recovery.
fn close_policy(os: OsKind, has_tray: bool, background_pref: bool) -> CloseAction {
    match os {
        OsKind::Macos => CloseAction::HideToTray,
        OsKind::Windows | OsKind::Linux => {
            if background_pref && has_tray {
                CloseAction::HideToTray
            } else {
                CloseAction::Exit
            }
        }
    }
}

/// The user's explicit "keep running in the background on close" preference. Persisted beside
/// the window state; defaults to `false` (exit-by-default) when unset/unreadable. Written by the
/// desktop Settings toggle.
fn background_on_close(app: &tauri::AppHandle) -> bool {
    win_state_path(app)
        .map(|p| p.with_file_name("background.json"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<bool>(s.trim()).ok())
        .unwrap_or(false)
}

/// Read the persisted "keep running in the background on close" preference for the Settings toggle
/// so the control can render its current state. Mirrors `background_on_close`'s
/// default (`false` = exit-by-default).
#[tauri::command(async)]
fn get_background_on_close(app: tauri::AppHandle) -> Result<bool, String> {
    Ok(background_on_close(&app))
}

/// Persist the "keep running in the background on close" preference — the WRITE path
/// the reader `background_on_close`/`close_policy` consume on the next close. Stored as a bare JSON
/// bool in `background.json` beside the window state. macOS ignores it because it always keeps
/// running; the desktop Settings toggle only surfaces it on Windows/Linux.
#[tauri::command(async)]
fn set_background_on_close(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    let Some(p) = win_state_path(&app).map(|p| p.with_file_name("background.json")) else {
        return Err("no app config dir".into());
    };
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("create app config directory {}: {e}", dir.display()))?;
    }
    std::fs::write(&p, if enabled { "true" } else { "false" })
        .map_err(|e| format!("write background.json: {e}"))?;
    Ok(())
}

/// The macOS traffic-light safe leading region in px, or `None` to collapse it. Uses the measured
/// region when available; **96px is the fallback**; fullscreen
/// collapses to `None` because macOS auto-hides the controls (the reserve would be dead space).
fn traffic_light_reserve_px(fullscreen: bool, measured: Option<f64>) -> Option<f64> {
    if fullscreen {
        None
    } else {
        Some(measured.unwrap_or(96.0))
    }
}

/// The authoritative pre-boot init-script. Injected via
/// `initialization_script`, it runs at document-start — before prepaint.js / bootTauri.js — and sets
/// `data-os` + `data-material` on `<html>` plus a `globalThis.__TARE_SHELL_INIT__` snapshot the web
/// boot consumes (never overwriting them with UA sniffing). OS + material are compile-known; the
/// leading traffic-light reserve is the 96px fallback here (a real objc measurement + runtime metrics
/// refinement is the platform matrix). Windows/Linux carry a 0 reserve — their
/// controls are the native trailing bar, not a leading inset.
fn shell_init_script() -> String {
    let (os, material, reserve) = if cfg!(target_os = "macos") {
        (
            "macos",
            "vibrancy",
            traffic_light_reserve_px(false, None).unwrap_or(96.0),
        )
    } else if cfg!(target_os = "windows") {
        ("windows", "mica", 0.0)
    } else {
        ("linux", "flat", 0.0)
    };
    format!(
        "(function(){{var r=document.documentElement;r.dataset.os={os:?};r.dataset.material={material:?};\
         globalThis.__TARE_SHELL_INIT__={{os:{os:?},material:{material:?},traffic_light_reserve_px:{reserve}}};}})();"
    )
}

/// Build the main window HIDDEN and platform-appropriate. Replaces the static
/// auto-created config window so the app can restore geometry BEFORE showing (no center-then-jump)
/// and never double-decorates. Common: title, 1100×720 default, min inner size ~420 wide (the desktop
/// floor; the interior stays usable to 330 for Windows Snap), `accept_first_mouse`. Per-platform
/// decoration/material: macOS gets the overlay titlebar (hidden title, inset traffic lights,
/// under-window vibrancy); Windows/Linux keep the NATIVE OS titlebar and decorations, so Windows Snap
/// controls stay native and there is no duplicate titlebar. The window builder validates the config;
/// runtime decoration/material + Snap behavior are the real-platform matrix.
fn build_main_window(app: &tauri::App) -> tauri::Result<()> {
    let mut builder =
        tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("index.html".into()))
            .title("Tare")
            .inner_size(1100.0, 720.0)
            .min_inner_size(420.0, 520.0)
            .visible(false)
            .accept_first_mouse(true)
            // Authoritative pre-boot traits: runs at document-start, BEFORE any
            // page script (incl. prepaint.js), so data-os/data-material + the __TARE_SHELL_INIT__
            // snapshot are set before first paint and boot consumes them instead of UA-sniffing.
            .initialization_script(shell_init_script());

    #[cfg(target_os = "macos")]
    {
        builder = builder
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true)
            .traffic_light_position(tauri::LogicalPosition::new(14.0, 16.0))
            .effects(
                tauri::window::EffectsBuilder::new()
                    .effect(tauri::window::Effect::UnderWindowBackground)
                    .state(tauri::window::EffectState::FollowsWindowActiveState)
                    .build(),
            );
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Native OS titlebar + decorations on Windows/Linux (no overlay, no second titlebar).
        builder = builder.decorations(true);
    }

    builder.build()?;
    Ok(())
}

/// Restore the saved window geometry onto the (hidden) main window, or CENTER it on first run / an
/// off-monitor save — always leaving it correctly positioned so the caller can `show()` without a
/// visible jump.
fn restore_window(app: &tauri::AppHandle) {
    let (Some(p), Some(w)) = (win_state_path(app), app.get_webview_window("main")) else {
        return;
    };
    let restored = std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| parse_win_state(&s))
        .map(|st| {
            // Clamp the saved LOGICAL normal bounds to a visible monitor work area (v2): a window
            // saved on a since-disconnected display, or that no longer fits, is never stranded.
            let clamped = clamp_to_work_area(
                Rect {
                    x: st.x,
                    y: st.y,
                    w: st.w,
                    h: st.h,
                },
                &monitor_work_areas(&w),
            );
            let _ = w.set_size(tauri::LogicalSize::new(clamped.w, clamped.h));
            let _ = w.set_position(tauri::LogicalPosition::new(clamped.x, clamped.y));
            // Re-maximize if that was the saved state (after positioning, so un-maximize restores the
            // clamped normal bounds).
            if st.maximized {
                let _ = w.maximize();
            }
        })
        .is_some();
    // First run (no valid saved geometry): center before the caller shows it.
    if !restored {
        let _ = w.center();
    }
}

/// The connected monitors' work areas as LOGICAL rects (DPI-normalized), for the clamp logic. Empty
/// on query failure (clamp then no-ops, leaving the OS placement).
fn monitor_work_areas<R: Runtime>(w: &tauri::WebviewWindow<R>) -> Vec<Rect> {
    let Ok(monitors) = w.available_monitors() else {
        return Vec::new();
    };
    monitors
        .iter()
        .map(|m| {
            let wa = m.work_area();
            let s = m.scale_factor().max(0.1);
            Rect {
                x: wa.position.x as f64 / s,
                y: wa.position.y as f64 / s,
                w: wa.size.width as f64 / s,
                h: wa.size.height as f64 / s,
            }
        })
        .collect()
}

fn save_window(window: &tauri::WebviewWindow) {
    let Some(p) = win_state_path(window.app_handle()) else {
        return;
    };
    let (Ok(sz), Ok(pos)) = (window.outer_size(), window.outer_position()) else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0).max(0.1);
    // Current bounds as LOGICAL (DPI-independent) so a restore on a differently-scaled monitor keeps
    // the same apparent size.
    let current = Rect {
        x: pos.x as f64 / scale,
        y: pos.y as f64 / scale,
        w: sz.width as f64 / scale,
        h: sz.height as f64 / scale,
    };
    let maximized = window.is_maximized().unwrap_or(false);
    let fullscreen = window.is_fullscreen().unwrap_or(false);
    // Ignore the transient maximized/fullscreen bounds: keep the previously-saved NORMAL bounds so an
    // un-maximize restores the right size (v2).
    let prev = std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| parse_win_state(&s));
    let prev_normal = prev
        .as_ref()
        .map(|st| Rect {
            x: st.x,
            y: st.y,
            w: st.w,
            h: st.h,
        })
        .unwrap_or(current);
    let normal = normal_bounds(current, maximized, fullscreen, prev_normal);
    let monitor = window
        .current_monitor()
        .ok()
        .flatten()
        .and_then(|m| m.name().cloned());
    let st = WinState {
        version: 2,
        x: normal.x,
        y: normal.y,
        w: normal.w,
        h: normal.h,
        scale,
        monitor,
        maximized,
    };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(&st) {
        let _ = std::fs::write(&p, json);
    }
}

/// The active capture proxy: either a child this app owns or an existing verified Tare server the
/// app attached to. Attached servers are never killed on stop/exit.
enum ProxyConnection {
    Owned { child: Child, port: u16 },
    Attached { port: u16 },
}

impl ProxyConnection {
    fn port(&self) -> u16 {
        match self {
            Self::Owned { port, .. } | Self::Attached { port } => *port,
        }
    }
}

#[derive(Default)]
struct ProxyState(Mutex<Option<ProxyConnection>>);

/// Resolve the `tare` CLI binary: `TARE_BIN` override, else a sibling of the desktop binary (the
/// shipped layout — CLI + desktop are built from one workspace and installed side by side, incl. the
/// macOS `.app/Contents/MacOS` and `.../Resources` bundle dirs), else bare `tare` on `PATH`.
///
/// The PATH fallback is a LAST resort and is logged: a `tare` on `PATH` may be an unrelated or
/// version-skewed build (for example, a stale PATH `tare` predating the `capture`
/// subcommand, so the desktop's fire-and-forget `capture sync` silently no-oped with "unknown
/// command capture"). We can't verify a foreign binary's version, so we surface the fallback instead
/// of driving it silently.
fn tare_bin() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("TARE_BIN") {
        if !path.trim().is_empty() {
            return path.into();
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // Co-located CLI: the dev/target layout (sibling), and both macOS `.app` bundle dirs
            // (`Contents/MacOS/tare` next to the desktop exe, and `Contents/Resources/tare`).
            for cand in [dir.join("tare"), dir.join("../Resources/tare")] {
                if cand.is_file() {
                    return cand;
                }
            }
        }
    }
    eprintln!(
        "tare-desktop: no co-located `tare` CLI found next to the app; falling back to `tare` on \
         PATH — this may be a version-skewed build. Set TARE_BIN to pin the matching CLI."
    );
    "tare".into()
}

fn proxy_status_json(running: bool, port: u16) -> String {
    let url = if running {
        format!("http://127.0.0.1:{port}")
    } else {
        String::new()
    };
    serde_json::json!({ "running": running, "port": port, "url": url }).to_string()
}

fn verified_tare_server(port: u16) -> bool {
    crate::fetch_otlp_status(port).is_some()
}

/// Refresh a tracked proxy and return its live port. Exited owned children are reaped and cleared;
/// attached servers are re-probed so the UI never reports a stale attachment as running.
fn live_proxy_port(connection: &mut Option<ProxyConnection>) -> Result<Option<u16>, String> {
    let Some(current) = connection.as_mut() else {
        return Ok(None);
    };
    let live = match current {
        ProxyConnection::Owned { child, .. } => child
            .try_wait()
            .map_err(|error| format!("inspect proxy process: {error}"))?
            .is_none(),
        ProxyConnection::Attached { port } => verified_tare_server(*port),
    };
    if live {
        Ok(Some(current.port()))
    } else {
        connection.take();
        Ok(None)
    }
}

/// The user's macOS accent color as an sRGB hex, read from the `AppleAccentColor` global preference
/// — the same value System Settings ▸ Appearance writes. Reading the preference avoids
/// linking AppKit just to fetch one color (and the fragile main-thread NSColor dance). Absent = the
/// default "multicolor" (blue); -1 = graphite; 0..=6 = the seven named accents. The web side clamps
/// whatever hex arrives to the AA contrast floor, so an unexpected value degrades safely to brass.
/// Returns None only if `defaults` can't be run at all. UNVALIDATED[macos]: the emitted value needs a
/// real launch to eyeball; the mapping itself is a pure table.
#[cfg(target_os = "macos")]
fn macos_accent_hex() -> Option<String> {
    let out = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleAccentColor"])
        .output()
        .ok()?;
    let hex = if out.status.success() {
        match String::from_utf8_lossy(&out.stdout).trim().parse::<i32>() {
            Ok(-1) => "#8c8c8c", // graphite
            Ok(0) => "#ff5257",  // red
            Ok(1) => "#f7821b",  // orange
            Ok(2) => "#ffc501",  // yellow
            Ok(3) => "#62ba46",  // green
            Ok(4) => "#007aff",  // blue
            Ok(5) => "#953d96",  // purple
            Ok(6) => "#f74f9e",  // pink
            _ => "#007aff",
        }
    } else {
        "#007aff" // preference absent → default "multicolor" (blue)
    };
    Some(hex.to_string())
}

/// Raise an OS notification through Tauri's cross-platform native plugin. Notifications are
/// deliberately suppressed while the main window has focus: the webview already presents the same
/// alert in-app, and showing both is noisy. Title/body are app-generated counts/labels — never raw
/// model payload.
#[tauri::command(async)]
fn notify(app: AppHandle, title: String, body: String) -> Result<(), String> {
    if app
        .get_webview_window("main")
        .and_then(|window| window.is_focused().ok())
        .unwrap_or(false)
    {
        return Ok(());
    }

    app.notification()
        .builder()
        .title(title)
        .body(body)
        .auto_cancel()
        .show()
        .map_err(|error| error.to_string())
}

#[tauri::command(async)]
fn proxy_status(state: State<ProxyState>) -> Result<String, String> {
    let mut connection = state.0.lock().map_err(|e| e.to_string())?;
    Ok(match live_proxy_port(&mut connection)? {
        Some(port) => proxy_status_json(true, port),
        None => proxy_status_json(false, 0),
    })
}

#[tauri::command(async)]
fn start_proxy(
    state: State<ProxyState>,
    db: Option<String>,
    port: Option<u16>,
) -> Result<String, String> {
    let mut connection = state.0.lock().map_err(|e| e.to_string())?;
    if live_proxy_port(&mut connection)?.is_some() {
        return Err("proxy already running".into());
    }
    let port = match port {
        Some(port) => port,
        None => configured_ports()?.0,
    };
    if port == 0 {
        return Err("proxy port must be between 1 and 65535".into());
    }
    // Parity with auto_start_capture: if a daemon already holds the port (an installed
    // always-on service, or a manual serve), ATTACH to it — don't spawn a child that instantly dies on
    // bind and gets recorded as "running" (a silent dead-child). We don't own it, so we won't reap it.
    if port_is_live(port) {
        if verified_tare_server(port) {
            *connection = Some(ProxyConnection::Attached { port });
            return Ok(proxy_status_json(true, port));
        }
        return Err(format!(
            "port {port} is occupied by a service that is not a Tare server"
        ));
    }
    let db = db.unwrap_or_else(default_db);
    let child = spawn_serve(&db, port)?;
    *connection = Some(ProxyConnection::Owned { child, port });
    Ok(proxy_status_json(true, port))
}

/// Spawn `tare serve` as a loopback-guarded child. Shared by the manual Start button and auto-start.
fn spawn_serve(db: &str, port: u16) -> Result<Child, String> {
    let binary = tare_bin();
    Command::new(&binary)
        .arg("serve")
        .arg("--db")
        .arg(db)
        .arg("--port")
        .arg(port.to_string())
        .env("TARE_NETWORK_GUARD", "loopback")
        // The app is launched from Finder (CWD `/`), so hand the spawned serve the stable-root config
        // + db explicitly; otherwise it would read a nonexistent `/tare.toml` and ignore user config.
        .env("TARE_CONFIG", crate::config_path())
        .env("TARE_DB", db)
        .spawn()
        .map_err(|e| format!("start proxy ({}): {e}", binary.display()))
}

/// True if something is already listening on loopback `port` — so auto-start ATTACHES to an existing
/// daemon (e.g. an always-on `tare service`, or a manual `tare serve`) instead of double-binding it.
fn port_is_live(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(250)).is_ok()
}

/// Make capture live the moment the app opens, with zero manual steps. If a daemon is
/// already listening we ATTACH (we don't own it, so we never stop it on window close); otherwise we
/// spawn our own child `tare serve` and record it so `stop_proxy` can reap it. Either way the webview
/// talks to 127.0.0.1:port. Best-effort: a failure just means the user can Start it manually.
/// Run `tare capture sync` so the OS login item is reconciled to
/// `[capture].mode`: always_on installs+activates it, app_only/off remove it. Called at
/// startup and after a Settings mode change. Waiting prevents the auto-start probe from racing an
/// always-on service installation and spawning a second server on the same port.
fn sync_capture_service() -> Result<(), String> {
    let binary = tare_bin();
    let status = Command::new(&binary)
        .arg("capture")
        .arg("sync")
        .env("TARE_CONFIG", crate::config_path())
        .env("TARE_DB", default_db())
        .status()
        .map_err(|error| format!("start capture sync ({}): {error}", binary.display()))?;
    if !status.success() {
        return Err(format!("capture sync exited with {status}"));
    }
    Ok(())
}

/// The configured capture mode. An invalid present config is an error so capture does not silently
/// start under a less restrictive default.
fn capture_mode() -> Result<tare_core::config::CaptureMode, String> {
    tare_core::config::TareConfig::load(&crate::config_path()).map(|c| c.capture.mode)
}

fn auto_start_capture(state: &ProxyState, db: &str, port: u16) {
    let mode = match capture_mode() {
        Ok(mode) => mode,
        Err(error) => {
            eprintln!("tare: cannot read capture mode; refusing to auto-start: {error}");
            return;
        }
    };
    if mode == tare_core::config::CaptureMode::Off {
        eprintln!("tare: capture mode is off — not starting a daemon");
        return;
    }
    let Ok(mut connection) = state.0.lock() else {
        return;
    };
    match live_proxy_port(&mut connection) {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(error) => {
            eprintln!("tare: cannot inspect the existing capture process: {error}");
            return;
        }
    }
    if port_is_live(port) {
        if verified_tare_server(port) {
            eprintln!("tare: attaching to the capture daemon already on 127.0.0.1:{port}");
            *connection = Some(ProxyConnection::Attached { port });
        } else {
            eprintln!("tare: port {port} is occupied by a non-Tare service; capture not started");
        }
        return;
    }
    match spawn_serve(db, port) {
        Ok(child) => {
            *connection = Some(ProxyConnection::Owned { child, port });
            eprintln!("tare: started capture daemon on 127.0.0.1:{port}");
        }
        Err(e) => eprintln!("tare: could not auto-start capture: {e}"),
    }
}

#[tauri::command(async)]
fn stop_proxy(state: State<ProxyState>) -> Result<String, String> {
    let mut connection = state.0.lock().map_err(|e| e.to_string())?;
    if let Some(ProxyConnection::Owned { child, .. }) = connection.as_mut() {
        if child
            .try_wait()
            .map_err(|error| format!("inspect proxy process: {error}"))?
            .is_none()
        {
            child
                .kill()
                .map_err(|error| format!("stop proxy process: {error}"))?;
            child
                .wait()
                .map_err(|error| format!("reap proxy process: {error}"))?;
        }
    }
    connection.take();
    Ok(proxy_status_json(false, 0))
}

fn default_db() -> String {
    if let Ok(path) = std::env::var("TARE_DB") {
        if !path.trim().is_empty() {
            return path;
        }
    }
    match tare_core::config::TareConfig::load(&crate::config_path()) {
        Ok(config) => {
            if let Some(path) = config.proxy.db.filter(|path| !path.trim().is_empty()) {
                return path;
            }
        }
        Err(error) => eprintln!(
            "tare: cannot read configured database path ({error}); using the stable default"
        ),
    }
    let home = std::env::var("HOME")
        .ok()
        .filter(|path| !path.trim().is_empty())
        .unwrap_or_else(|| ".".to_string());
    format!("{home}/.tare/tare.db")
}

fn configured_ports() -> Result<(u16, u16), String> {
    let config = tare_core::config::TareConfig::load(&crate::config_path())?;
    Ok((
        config.proxy.port.unwrap_or(DEFAULT_HTTP_PORT),
        config.proxy.otlp_port.unwrap_or(DEFAULT_OTLP_PORT),
    ))
}

/// Today's date for the tray/today filter — delegates to the shared local-day logic so the
/// desktop and CLI agree (honors `[ui] tz_offset_minutes` / `TARE_TZ_OFFSET_MINUTES`).
fn today_utc() -> Result<String, String> {
    crate::today_local()
}

fn current_tray_status(active_http_port: Option<u16>) -> crate::TrayStatus {
    let result = (|| {
        let (configured_http_port, otlp_port) = configured_ports()?;
        let http_port = active_http_port.unwrap_or(configured_http_port);
        let date = today_utc()?;
        crate::tray_status(&default_db(), &date, http_port, otlp_port)
    })();
    match result {
        Ok(status) => status,
        Err(error) => {
            eprintln!("tare: cannot refresh tray status: {error}");
            crate::TrayStatus {
                title: "⚠ Tare · status unavailable".to_string(),
                budget_row: "Budget: unavailable".to_string(),
                capture_row: "Capture: unavailable".to_string(),
                attention: true,
            }
        }
    }
}

#[tauri::command(async)]
fn today_spend(db: Option<String>) -> Result<TodaySpend, String> {
    let db = db.unwrap_or_else(default_db);
    crate::today_spend_view(&db, &today_utc()?)
}

#[tauri::command(async)]
fn list_runs(db: Option<String>) -> Result<Vec<String>, String> {
    crate::list_run_ids(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn run_flamegraph(db: Option<String>, run_id: String) -> Result<FlamegraphModel, String> {
    crate::run_flamegraph_view(&db.unwrap_or_else(default_db), &run_id)
}

#[tauri::command(async)]
fn run_profile(
    db: Option<String>,
    run_id: String,
    sort: Option<String>,
    top_n: Option<usize>,
) -> Result<tare_core::flamegraph::ProfileTable, String> {
    crate::run_profile_view(&db.unwrap_or_else(default_db), &run_id, sort, top_n)
}

#[tauri::command(async)]
fn cost_frontier(db: Option<String>) -> Result<tare_core::experiment::Frontier, String> {
    crate::run_frontier_view(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn report(db: Option<String>) -> Result<String, String> {
    crate::report_json(&db.unwrap_or_else(default_db))
}

// Calibrated Bench cohort analysis: each command delegates to the SAME
// transport-agnostic `crate::cohort_*_api` fn that the HTTP `/__tare/cohort/*` routes call, so the
// desktop and browser transports return equivalent data for a given request. `body` is the JSON
// request DTO (CohortSpec / CohortFacetRequest / CohortCompareRequest / CohortSearchRequest /
// CohortTimelineRequest); the
// `(status, message)` error is flattened to the message string Tauri commands surface.
#[tauri::command(async)]
fn cohort_resolve(db: Option<String>, body: String) -> Result<String, String> {
    crate::cohort_resolve_json(&db.unwrap_or_else(default_db), body.as_bytes())
}

#[tauri::command(async)]
fn cohort_facets(db: Option<String>, body: String) -> Result<String, String> {
    crate::cohort_facets_json(&db.unwrap_or_else(default_db), body.as_bytes())
}

#[tauri::command(async)]
fn cohort_compare(db: Option<String>, body: String) -> Result<String, String> {
    crate::cohort_compare_json(&db.unwrap_or_else(default_db), body.as_bytes())
}

#[tauri::command(async)]
fn cohort_search(db: Option<String>, body: String) -> Result<String, String> {
    crate::cohort_search_json(&db.unwrap_or_else(default_db), body.as_bytes())
}

#[tauri::command(async)]
fn cohort_timeline(db: Option<String>, body: String) -> Result<String, String> {
    crate::cohort_timeline_json(&db.unwrap_or_else(default_db), body.as_bytes())
}

// Scoped anomaly explanation: same shared `Store::anomaly_why` the HTTP route
// calls, so desktop and browser return identical decomposition rows. `body` is an AnomalyWhyRequest.
#[tauri::command(async)]
fn anomaly_why(db: Option<String>, body: String) -> Result<String, String> {
    crate::anomaly_why_json(&db.unwrap_or_else(default_db), body.as_bytes())
}

#[tauri::command(async)]
fn trend(
    db: Option<String>,
    from: Option<String>,
    to: Option<String>,
    by: Option<String>,
) -> Result<String, String> {
    crate::trend_json(
        &db.unwrap_or_else(default_db),
        from.as_deref(),
        to.as_deref(),
        by.as_deref().unwrap_or("total"),
    )
}

#[tauri::command(async)]
fn run_steps(db: Option<String>, run_id: String) -> Result<String, String> {
    crate::run_steps_json(&db.unwrap_or_else(default_db), &run_id)
}

#[tauri::command(async)]
fn recent_steps(db: Option<String>, n: Option<u32>) -> Result<String, String> {
    crate::recent_steps_json(&db.unwrap_or_else(default_db), n.unwrap_or(12))
}

#[tauri::command(async)]
fn burnrate(db: Option<String>, range: Option<String>) -> Result<String, String> {
    crate::burnrate_json_for_range(&db.unwrap_or_else(default_db), range.as_deref())
}

#[tauri::command(async)]
fn coverage(db: Option<String>) -> Result<String, String> {
    crate::coverage_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn run_meta(db: Option<String>, run_id: String) -> Result<String, String> {
    crate::run_meta_json(&db.unwrap_or_else(default_db), &run_id)
}

#[tauri::command(async)]
fn run_status(db: Option<String>, run_id: String) -> Result<String, String> {
    crate::run_status_json(&db.unwrap_or_else(default_db), &run_id)
}

#[tauri::command(async)]
fn run_statuses(db: Option<String>) -> Result<String, String> {
    crate::run_statuses_json(&db.unwrap_or_else(default_db))
}

// Run notes: the desktop side of the local-only annotations write path.
#[tauri::command(async)]
fn run_note(db: Option<String>, run_id: String) -> Result<String, String> {
    crate::run_note_json(&db.unwrap_or_else(default_db), &run_id)
}

#[tauri::command(async)]
fn save_run_note(db: Option<String>, note_json: String) -> Result<(), String> {
    crate::save_run_note(&db.unwrap_or_else(default_db), &note_json)
}

#[tauri::command(async)]
fn set_run_quality(
    db: Option<String>,
    run_id: String,
    score: i64,
    source: Option<String>,
) -> Result<(), String> {
    crate::set_run_quality(
        &db.unwrap_or_else(default_db),
        &run_id,
        score,
        source.as_deref().unwrap_or("ui"),
    )
}

#[tauri::command(async)]
fn delete_run_note(db: Option<String>, run_id: String) -> Result<(), String> {
    crate::delete_run_note(&db.unwrap_or_else(default_db), &run_id)
}

#[tauri::command(async)]
fn runs_by_tag(db: Option<String>, tag: String) -> Result<String, String> {
    crate::runs_by_tag_json(&db.unwrap_or_else(default_db), &tag)
}

#[tauri::command(async)]
fn starred_runs(db: Option<String>) -> Result<String, String> {
    crate::starred_runs_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn explain(db: Option<String>, run_id: String) -> Result<String, String> {
    crate::explain_view(&db.unwrap_or_else(default_db), &run_id)
}

#[tauri::command(async)]
fn session_autopsy(
    db: Option<String>,
    run_id: String,
    median: Option<i64>,
) -> Result<String, String> {
    crate::session_autopsy_json(&db.unwrap_or_else(default_db), &run_id, median)
}

#[tauri::command(async)]
fn transcript(db: Option<String>, run_id: String, step: u32) -> Result<String, String> {
    crate::transcript_json(&db.unwrap_or_else(default_db), &run_id, step)
}

#[tauri::command(async)]
fn transcript_purge(db: Option<String>) -> Result<(), String> {
    crate::transcript_purge_all(&db.unwrap_or_else(default_db)).map(|_| ())
}

#[tauri::command(async)]
fn advise(db: Option<String>) -> Result<String, String> {
    crate::advise_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn savings(db: Option<String>) -> Result<String, String> {
    crate::savings_json(&db.unwrap_or_else(default_db))
}

// Savings action lifecycle: same shared Store the HTTP routes use, so desktop
// and browser share one lifecycle table. `body` is a SavingsActionRequest (accept/dismiss) or a
// SavingsActionIdentity (unaccept).
#[tauri::command(async)]
fn savings_accept(db: Option<String>, body: String) -> Result<(), String> {
    crate::savings_accept_json(&db.unwrap_or_else(default_db), body.as_bytes())
}

#[tauri::command(async)]
fn savings_dismiss(db: Option<String>, body: String) -> Result<(), String> {
    crate::savings_dismiss_json(&db.unwrap_or_else(default_db), body.as_bytes())
}

#[tauri::command(async)]
fn savings_unaccept(db: Option<String>, body: String) -> Result<(), String> {
    crate::savings_unaccept_json(&db.unwrap_or_else(default_db), body.as_bytes())
}

#[tauri::command(async)]
fn savings_actions(db: Option<String>) -> Result<String, String> {
    crate::savings_actions_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn savings_verify(db: Option<String>, body: String) -> Result<String, String> {
    crate::savings_verify_json(&db.unwrap_or_else(default_db), body.as_bytes())
}

#[tauri::command(async)]
fn action_plan(db: Option<String>) -> Result<String, String> {
    crate::action_plan_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn cache_ledger(db: Option<String>) -> Result<String, String> {
    crate::cache_ledger_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn reasoning(db: Option<String>) -> Result<String, String> {
    crate::reasoning_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn effectiveness(db: Option<String>) -> Result<String, String> {
    crate::effectiveness_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn confidence(db: Option<String>) -> Result<String, String> {
    crate::confidence_json(&db.unwrap_or_else(default_db), &today_utc()?)
}

#[tauri::command(async)]
fn whatif(db: Option<String>, cross_provider: Option<bool>) -> Result<String, String> {
    crate::whatif_json(
        &db.unwrap_or_else(default_db),
        cross_provider.unwrap_or(false),
    )
}

#[tauri::command(async)]
fn anomalies(
    db: Option<String>,
    by: Option<String>,
    window: Option<usize>,
    threshold: Option<i64>,
) -> Result<String, String> {
    // Mirror the HTTP read API: honor by/window/threshold, falling back to tare.toml [anomaly]
    // defaults then the built-in 7-day / 50% so desktop and browser agree.
    let cfg = tare_core::config::TareConfig::load(&crate::config_path())?;
    let window = window.or(cfg.anomaly.window).unwrap_or(7);
    let threshold = threshold.or(cfg.anomaly.threshold).unwrap_or(50);
    crate::anomalies_json(
        &db.unwrap_or_else(default_db),
        by.as_deref().unwrap_or("total"),
        window,
        threshold,
    )
}

#[tauri::command(async)]
fn cost_regressions(
    db: Option<String>,
    window: Option<usize>,
    threshold: Option<i64>,
) -> Result<String, String> {
    // Outcome-aware unit-cost regressions, sharing the [anomaly] window/threshold.
    let cfg = tare_core::config::TareConfig::load(&crate::config_path())?;
    let window = window.or(cfg.anomaly.window).unwrap_or(7);
    let threshold = threshold.or(cfg.anomaly.threshold).unwrap_or(50);
    crate::cost_regressions_json(&db.unwrap_or_else(default_db), window, threshold)
}

#[tauri::command(async)]
fn diff(db: Option<String>, a: String, b: String) -> Result<String, String> {
    crate::diff_json(&db.unwrap_or_else(default_db), &a, &b)
}

// Hierarchical node-level flame diff over an explicit run pair: same shared
// `Store::flame_diff` the HTTP route calls. Distinct from the row-level report `diff` above.
#[tauri::command(async)]
fn flame_diff(
    db: Option<String>,
    a: String,
    b: String,
    normalized: Option<bool>,
) -> Result<String, String> {
    crate::flame_diff_json(
        &db.unwrap_or_else(default_db),
        &a,
        &b,
        normalized.unwrap_or(false),
    )
}

// Offline counterfactual cost experiment over a cohort: same shared
// `Store::experiment` the HTTP route calls. `body` is an ExperimentRequest.
#[tauri::command(async)]
fn experiment(db: Option<String>, body: String) -> Result<String, String> {
    crate::experiment_json(&db.unwrap_or_else(default_db), body.as_bytes())
}

// Durable saved investigations: the SAME shared store the HTTP routes use, so
// desktop and browser share one source of truth. `body` is the full SavedInvestigation DTO.
#[tauri::command(async)]
fn list_investigations(db: Option<String>) -> Result<String, String> {
    crate::list_investigations_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn save_investigation(db: Option<String>, body: String) -> Result<(), String> {
    crate::upsert_investigation_json(&db.unwrap_or_else(default_db), body.as_bytes())
}

#[tauri::command(async)]
fn delete_investigation(db: Option<String>, id: String) -> Result<(), String> {
    crate::delete_investigation(&db.unwrap_or_else(default_db), &id)
}

#[tauri::command(async)]
fn pricing(_db: Option<String>) -> Result<String, String> {
    crate::pricing_json()
}

#[tauri::command(async)]
fn receipt(
    db: Option<String>,
    run_id: String,
    max_private: Option<bool>,
) -> Result<String, String> {
    crate::receipt_json(
        &db.unwrap_or_else(default_db),
        &run_id,
        max_private.unwrap_or(false),
    )
}

#[tauri::command(async)]
fn rollup(
    db: Option<String>,
    by: Option<String>,
    filter_by: Option<String>,
    filter: Option<String>,
) -> Result<String, String> {
    crate::rollup_json(
        &db.unwrap_or_else(default_db),
        by.as_deref().unwrap_or("step"),
        filter_by.as_deref(),
        filter.as_deref(),
    )
}

#[tauri::command(async)]
fn punchcard(db: Option<String>) -> Result<String, String> {
    crate::punchcard_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn heatmap(db: Option<String>) -> Result<String, String> {
    crate::heatmap_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn sessions(db: Option<String>) -> Result<String, String> {
    crate::sessions_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn correlate(db: Option<String>) -> Result<String, String> {
    crate::correlate_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn lineages(db: Option<String>) -> Result<String, String> {
    crate::lineages_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn units(db: Option<String>) -> Result<String, String> {
    crate::units_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn sessions_live(state: State<ProxyState>, db: Option<String>) -> Result<String, String> {
    // Prefer a running serve's in-memory table (freshest) over loopback HTTP; fall back to the
    // durable mirror in the DB so the live view isn't blank when no serve is reachable.
    let http_port = {
        let g = state.0.lock().map_err(|e| e.to_string())?;
        match g.as_ref().map(ProxyConnection::port) {
            Some(port) => port,
            None => configured_ports()?.0,
        }
    };
    if let Some(live) = crate::fetch_sessions_live(http_port) {
        return Ok(live);
    }
    crate::sessions_live_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn reconcile(db: Option<String>, day: Option<String>) -> Result<String, String> {
    let day = match day {
        Some(day) => day,
        None => today_utc()?,
    };
    crate::reconcile_json(&db.unwrap_or_else(default_db), &day)
}

#[tauri::command(async)]
fn vendor_today(state: State<ProxyState>, db: Option<String>) -> Result<String, String> {
    // Claude Code's own reported spend today (vendor cross-check). Prefer a running serve, else
    // read the durable metered series from the DB for the local day.
    let http_port = {
        let g = state.0.lock().map_err(|e| e.to_string())?;
        match g.as_ref().map(ProxyConnection::port) {
            Some(port) => port,
            None => configured_ports()?.0,
        }
    };
    if let Some(v) = crate::fetch_vendor_today(http_port) {
        return Ok(v);
    }
    crate::vendor_today_json(&db.unwrap_or_else(default_db), &today_utc()?)
}

#[tauri::command(async)]
fn loops(db: Option<String>) -> Result<String, String> {
    crate::loops_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn failures(db: Option<String>) -> Result<String, String> {
    crate::failures_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn lenses(db: Option<String>) -> Result<String, String> {
    crate::lenses_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn budget(db: Option<String>) -> Result<String, String> {
    crate::budget_json(&db.unwrap_or_else(default_db))
}

#[tauri::command(async)]
fn sandwich(db: Option<String>, component: Option<String>) -> Result<String, String> {
    crate::sandwich_json(
        &db.unwrap_or_else(default_db),
        component.as_deref().unwrap_or("system"),
    )
}

#[tauri::command(async)]
fn otlp_status(state: State<ProxyState>, db: Option<String>) -> Result<String, String> {
    let db = db.unwrap_or_else(default_db);
    // Prefer a running serve's live status (it carries the last-event timestamp). Use the
    // desktop-managed proxy port if known, else the default 8788. Falls back to a DB-derived
    // count (age unknown) when no serve is reachable.
    let http_port = {
        let g = state.0.lock().map_err(|e| e.to_string())?;
        match g.as_ref().map(ProxyConnection::port) {
            Some(port) => port,
            None => configured_ports()?.0,
        }
    };
    if let Some(live) = crate::fetch_otlp_status(http_port) {
        return Ok(live);
    }
    crate::otlp_status_json(&db, configured_ports()?.1)
}

#[tauri::command(async)]
fn export(db: Option<String>, run_id: String, format: String) -> Result<String, String> {
    crate::export_view(&db.unwrap_or_else(default_db), &run_id, &format)
}

#[tauri::command(async)]
fn get_config() -> Result<String, String> {
    crate::config_get_json()
}

#[tauri::command(async)]
fn save_config(db: Option<String>, config_json: String) -> Result<String, String> {
    let res = crate::config_save_json_at_with_event(
        &crate::config_path(),
        &db.unwrap_or_else(default_db),
        &config_json,
        &crate::cohort_now(),
    )?;
    // A saved [capture].mode change must take effect now: reconcile the login item.
    if let Err(error) = sync_capture_service() {
        eprintln!("tare: saved config, but capture service sync failed: {error}");
    }
    Ok(res)
}

#[tauri::command(async)]
fn config_origins() -> Result<String, String> {
    crate::config_origins_json(&crate::config_path())
}

#[tauri::command(async)]
fn acknowledge_anomaly(key: String) -> Result<(), String> {
    crate::acknowledge_anomaly(&key)
}

/// Seed the bundled sample run so onboarding can leave the user on a populated Overview.
#[tauri::command(async)]
fn seed_demo(db: Option<String>) -> Result<String, String> {
    crate::seed_demo(&db.unwrap_or_else(default_db))
}

pub fn run() {
    tauri::Builder::default()
        .manage(ProxyState::default())
        // Native OS notifications: register the plugin so its Rust API is
        // available; the `notify` command routes through it. No frontend capability is granted —
        // notifications are raised Rust-side, not via the plugin's JS commands.
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            today_spend,
            list_runs,
            run_flamegraph,
            run_profile,
            cost_frontier,
            report,
            cohort_resolve,
            cohort_facets,
            cohort_compare,
            cohort_search,
            cohort_timeline,
            anomaly_why,
            flame_diff,
            experiment,
            list_investigations,
            save_investigation,
            savings_accept,
            savings_dismiss,
            savings_unaccept,
            savings_actions,
            savings_verify,
            delete_investigation,
            trend,
            run_status,
            run_statuses,
            run_meta,
            burnrate,
            coverage,
            run_steps,
            recent_steps,
            run_note,
            save_run_note,
            set_run_quality,
            delete_run_note,
            runs_by_tag,
            starred_runs,
            acknowledge_anomaly,
            explain,
            session_autopsy,
            transcript,
            transcript_purge,
            advise,
            savings,
            action_plan,
            cache_ledger,
            reasoning,
            effectiveness,
            confidence,
            whatif,
            anomalies,
            cost_regressions,
            seed_demo,
            diff,
            pricing,
            receipt,
            get_config,
            save_config,
            config_origins,
            rollup,
            punchcard,
            heatmap,
            sessions,
            correlate,
            lineages,
            units,
            sessions_live,
            vendor_today,
            reconcile,
            loops,
            failures,
            lenses,
            budget,
            sandwich,
            otlp_status,
            export,
            proxy_status,
            start_proxy,
            stop_proxy,
            get_background_on_close,
            set_background_on_close,
            notify
        ])
        .setup(|app| {
            // Build the main window HIDDEN + platform-appropriate FIRST, before the
            // menus/tray reference it; it is shown only after geometry is restored (see the tail).
            build_main_window(app)?;
            let handle = app.handle();

            // Reconcile the OS login item to [capture].mode, then start/attach the daemon for this
            // session. always_on: sync installs+activates the login item
            // and the probe attaches to it; app_only: sync removes any login item and we spawn a child;
            // off: sync removes it and auto_start skips. Best-effort — the app "just works" on open.
            if let Err(error) = sync_capture_service() {
                eprintln!("tare: capture service sync failed: {error}");
            }
            match configured_ports() {
                Ok((http_port, _)) => {
                    auto_start_capture(&app.state::<ProxyState>(), &default_db(), http_port)
                }
                Err(error) => eprintln!(
                    "tare: cannot read configured capture ports; refusing to auto-start: {error}"
                ),
            }

            // Native menu is macOS-ONLY (platform decision): macOS keeps one
            // complete global menu bar; Windows and Linux do NOT attach a second visible Tauri menu
            // bar — the rail Commands row is their single in-app menu surface (Linux F10 opens it,
            // wired web-side in main.ts). This is what removes the duplicate command surface. Every
            // menu id is a shared registry id (`nav:<route>` / `action:*`), same as the web palette.
            #[cfg(target_os = "macos")]
            {
                // The Edit submenu's predefined items are what make ⌘C/⌘V/⌘A work in the webview (a Tauri
                // webview without them silently breaks copy/paste). Preferences (⌘,) emits `nav:settings`,
                // reusing the same menu→navigate contract as Go. About is curated with name + version.
                let prefs = MenuItemBuilder::with_id("nav:settings", "Settings…")
                    .accelerator("CmdOrCtrl+,")
                    .build(handle)?;
                let app_menu = SubmenuBuilder::new(handle, "Tare")
                    .about(Some(AboutMetadata {
                        name: Some("Tare".into()),
                        version: Some(env!("CARGO_PKG_VERSION").into()),
                        ..Default::default()
                    }))
                    .separator()
                    .item(&prefs)
                    .separator()
                    .hide()
                    .hide_others()
                    .show_all()
                    .separator()
                    .quit()
                    .build()?;
                // In-page Find: Tauri has no built-in find and the native per-platform
                // find APIs are runtime-only, so ⌘F is wired to the in-webview find bar. The menu owns
                // ⌘F on desktop and emits `find`; the bridge relays it to the mounted app (which also
                // installs a desktop-gated Mod+F hotkey — the bar de-dupes if both fire).
                let find_item = MenuItemBuilder::with_id("action:find", "Find…")
                    .accelerator("CmdOrCtrl+F")
                    .build(handle)?;
                let edit = SubmenuBuilder::new(handle, "Edit")
                    .undo()
                    .redo()
                    .separator()
                    .cut()
                    .copy()
                    .paste()
                    .select_all()
                    .separator()
                    .item(&find_item)
                    .build()?;
                // Promote the command palette into View with its ⌘K accelerator so the menu is the
                // discoverable stand-in for every action. The menu owns ⌘K on desktop
                // and emits `open-palette`; the web installPaletteHotkey stays as the browser fallback
                // (the overlay de-dupes if both ever fire).
                let palette_item =
                    MenuItemBuilder::with_id("action:open-palette", "Command Palette…")
                        .accelerator("CmdOrCtrl+K")
                        .build(handle)?;
                // Theme is System/Light/Dark in Settings → Appearance only: the
                // native "Toggle Theme" item is removed. A discoverable "Appearance Settings…" item routes
                // to Settings via the standard nav path (`nav:settings` → handle_nav_event), so the OS menu
                // exposes where appearance lives without owning a toggle.
                let appearance_item =
                    MenuItemBuilder::with_id("nav:settings", "Appearance Settings…")
                        .build(handle)?;
                let view = SubmenuBuilder::new(handle, "View")
                    .item(&palette_item)
                    .item(&appearance_item)
                    .separator()
                    .fullscreen()
                    .build()?;
                // Go: native navigation mirroring the web rail. Each item emits `navigate`.
                let mut go = SubmenuBuilder::new(handle, "Go");
                for (id, label, _route) in NAV_DESTINATIONS {
                    go = go.item(&MenuItemBuilder::with_id(*id, *label).build(handle)?);
                }
                let go = go.build()?;
                let window = SubmenuBuilder::new(handle, "Window")
                    .minimize()
                    .separator()
                    .close_window()
                    .build()?;
                // Help (macOS standard; the OS adds its search field). Its one functional entry opens the
                // contextual command palette — the app's command-discovery surface — via the same
                // shared action id, so Help works without a second command surface.
                let help_search =
                    MenuItemBuilder::with_id("action:open-palette", "Search Commands…")
                        .build(handle)?;
                let help = SubmenuBuilder::new(handle, "Help")
                    .item(&help_search)
                    .build()?;
                let menu = MenuBuilder::new(handle)
                    .items(&[&app_menu, &edit, &view, &go, &window, &help])
                    .build()?;
                app.set_menu(menu)?;
                // Menu-bar clicks land here (the tray has its own handler below).
                app.on_menu_event(|app, event| {
                    let id = event.id().as_ref();
                    // Actions live in the WebView: reveal the window and relay the action as
                    // an event the mounted app performs (open the palette). Everything else is a navigate.
                    if id == "action:open-palette" {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                        let _ = app.emit("open-palette", ());
                        return;
                    }
                    // In-page find: reveal the window and relay `find` to the webview's
                    // find bar (Tauri has no native find; the bridge dispatches tare:find).
                    if id == "action:find" {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                        let _ = app.emit("find", ());
                        return;
                    }
                    // Theme toggle removed: "Appearance Settings…" (nav:settings) falls through
                    // to handle_nav_event like any other navigation.
                    handle_nav_event(app, id);
                });
            } // end #[cfg(target_os = "macos")] native menu

            // Tray = menu-bar extra: a glanceable status readout (today spend, budget, and capture)
            // plus navigation and a right-click menu; left-click reveals the window. Closing the
            // window hides to the tray and keeps monitoring.
            let active_http_port = app
                .state::<ProxyState>()
                .0
                .lock()
                .ok()
                .and_then(|connection| connection.as_ref().map(ProxyConnection::port));
            let status = current_tray_status(active_http_port);
            let menu_db = default_db();
            let recent = crate::recent_run_ids(&menu_db, RECENT_RUNS_IN_TRAY);
            let views = crate::saved_investigations_for_menu(&menu_db);
            // Capture whether the tray actually built. On a tray-less desktop (e.g.
            // GNOME 40+ with no StatusNotifier) hiding-to-tray would strand the window unrecoverably,
            // so this feeds the per-OS close semantics below. Menu and icon failures are logged and
            // leave the normal window usable instead of aborting desktop startup.
            let tray_ok = install_tray(handle, &status, &recent, &views);

            // Refresh the tray readout on a slow timer. The store read happens off the
            // main thread; only the tray title/menu update is marshalled back onto it (macOS
            // requires UI mutation on the main thread).
            let timer_handle = handle.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(TRAY_REFRESH_SECS));
                let active_http_port = timer_handle
                    .state::<ProxyState>()
                    .0
                    .lock()
                    .ok()
                    .and_then(|connection| connection.as_ref().map(ProxyConnection::port));
                let status = current_tray_status(active_http_port);
                let menu_db = default_db();
                let recent = crate::recent_run_ids(&menu_db, RECENT_RUNS_IN_TRAY);
                let views = crate::saved_investigations_for_menu(&menu_db);
                let app = timer_handle.clone();
                let _ = timer_handle.run_on_main_thread(move || {
                    if let Some(tray) = app.tray_by_id(TRAY_ID) {
                        let _ = tray.set_title(Some(status.title.clone()));
                        if let Ok(menu) = build_tray_menu(&app, &status, &recent, &views) {
                            let _ = tray.set_menu(Some(menu));
                        }
                    }
                });
            });

            // Restore the saved window geometry, THEN show; never center/show first and jump later.
            // Hide-to-tray on close and persist on move/resize.
            if let Some(win) = app.get_webview_window("main") {
                restore_window(handle);
                let _ = win.show();
                let saver = win.clone();
                let has_tray = tray_ok;
                win.on_window_event(move |event| match event {
                    WindowEvent::CloseRequested { api, .. } => {
                        // One policy for every platform: macOS hides-to-tray (Dock
                        // reactivates even with no tray); Windows/Linux EXIT by default and hide-to-tray
                        // only on an explicit background preference WITH a real tray — so a hidden
                        // window is never stranded. Capture keeps running in the separate daemon on any
                        // path. Runtime behavior is covered by the real-platform test matrix.
                        let bg = background_on_close(saver.app_handle());
                        match close_policy(current_os_kind(), has_tray, bg) {
                            CloseAction::HideToTray => {
                                api.prevent_close();
                                let _ = saver.hide();
                            }
                            // Let the window close normally; Tauri exits when the last window closes.
                            CloseAction::Exit => {}
                        }
                    }
                    WindowEvent::Resized(_) | WindowEvent::Moved(_) => save_window(&saver),
                    _ => {}
                });
            }
            // Emit the OS accent so the WebView's system-accent bridge can tint affordances to it.
            // macOS only for now (Windows UISettings / Linux XDG portal are
            // hardware-gated); the web clamps it to the AA floor or falls back to brass.
            #[cfg(target_os = "macos")]
            if let Some(hex) = macos_accent_hex() {
                let _ = app.handle().emit("system-accent", hex);
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tare desktop")
        .run(|app, event| {
            // App-only teardown: on ANY quit path (Cmd+Q, tray Quit, app.exit), reap
            // the daemon WE spawned so it isn't orphaned. An attached external daemon is left alone.
            if let tauri::RunEvent::Exit = event {
                if let Some(ProxyConnection::Owned { mut child, .. }) = app
                    .state::<ProxyState>()
                    .0
                    .lock()
                    .ok()
                    .and_then(|mut g| g.take())
                {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
            // macOS dock-icon click / Cmd-Tab with the window hidden-to-tray: re-show + focus, so a
            // Regular-activation app (decision 9) resurfaces instead of doing nothing.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::{
        clamp_to_work_area, close_policy, nav_target, normal_bounds, parse_win_state, port_is_live,
        shell_init_script, traffic_light_reserve_px, verified_tare_server, CloseAction, NavTarget,
        OsKind, Rect,
    };

    #[test]
    fn close_policy_matrix_never_strands_a_window() {
        use CloseAction::{Exit, HideToTray};
        use OsKind::{Linux, Macos, Windows};
        // macOS keeps the app running on close regardless of tray/preference — the Dock reactivates.
        for &tray in &[true, false] {
            for &bg in &[true, false] {
                assert_eq!(
                    close_policy(Macos, tray, bg),
                    HideToTray,
                    "macOS tray={tray} bg={bg}"
                );
            }
        }
        // Windows/Linux EXIT by default; hide-to-tray ONLY with an explicit background pref AND a real
        // tray to restore from (never a hidden, unrecoverable window).
        for &os in &[Windows, Linux] {
            assert_eq!(close_policy(os, true, true), HideToTray); // opted in + tray → background
            assert_eq!(close_policy(os, false, true), Exit); // opted in but NO tray → exit (no strand)
            assert_eq!(close_policy(os, true, false), Exit); // tray but not opted in → exit by default
            assert_eq!(close_policy(os, false, false), Exit); // default
        }
    }

    #[test]
    fn win_state_v1_migrates_to_v2_logical() {
        // Legacy v1 `{w,h,x,y}` physical (no version/scale/monitor/maximized) → v2, scale defaulted.
        let st = parse_win_state(r#"{"w":1100,"h":720,"x":100,"y":50}"#).unwrap();
        assert_eq!(st.version, 2);
        assert_eq!((st.x, st.y, st.w, st.h), (100.0, 50.0, 1100.0, 720.0));
        assert_eq!(st.scale, 1.0);
        assert!(!st.maximized);
        // A v2 record round-trips its richer fields.
        let v2 = parse_win_state(
            r#"{"version":2,"x":10.0,"y":20.0,"w":800.0,"h":600.0,"scale":2.0,"monitor":"DELL","maximized":true}"#,
        )
        .unwrap();
        assert_eq!(v2.version, 2);
        assert_eq!(v2.scale, 2.0);
        assert!(v2.maximized);
        assert_eq!(v2.monitor.as_deref(), Some("DELL"));
        // Unusable (too small) → None (fall back to center).
        assert!(parse_win_state(r#"{"w":10,"h":10,"x":0,"y":0}"#).is_none());
        assert!(parse_win_state("not json").is_none());
    }

    #[test]
    fn clamp_keeps_windows_on_a_visible_monitor() {
        let primary = Rect {
            x: 0.0,
            y: 0.0,
            w: 1440.0,
            h: 900.0,
        };
        let secondary = Rect {
            x: 1440.0,
            y: 0.0,
            w: 1920.0,
            h: 1080.0,
        };
        let monitors = [primary, secondary];
        // On-screen (top-left inside the primary work area) → unchanged.
        let win = Rect {
            x: 100.0,
            y: 80.0,
            w: 800.0,
            h: 600.0,
        };
        assert_eq!(clamp_to_work_area(win, &monitors), win);
        // On the secondary monitor (mixed-DPI logical origin) → still on-screen, unchanged.
        let on_secondary = Rect {
            x: 1600.0,
            y: 100.0,
            w: 900.0,
            h: 700.0,
        };
        assert_eq!(clamp_to_work_area(on_secondary, &monitors), on_secondary);
        // A partially off-screen window whose draggable corner is still visible is pulled fully
        // inside that monitor without changing its size.
        let partially_offscreen = Rect {
            x: 1200.0,
            y: 700.0,
            w: 800.0,
            h: 600.0,
        };
        assert_eq!(
            clamp_to_work_area(partially_offscreen, &monitors),
            Rect {
                x: 640.0,
                y: 300.0,
                w: 800.0,
                h: 600.0,
            }
        );
        // Disconnected monitor: saved top-left off ALL monitors → recentered on the primary, fit.
        let stranded = Rect {
            x: 5000.0,
            y: 5000.0,
            w: 800.0,
            h: 600.0,
        };
        let fixed = clamp_to_work_area(stranded, &monitors);
        assert_eq!(
            fixed,
            Rect {
                x: (1440.0 - 800.0) / 2.0,
                y: (900.0 - 600.0) / 2.0,
                w: 800.0,
                h: 600.0
            }
        );
        // A window larger than the primary shrinks to fit when recentered.
        let huge = Rect {
            x: -9000.0,
            y: -9000.0,
            w: 3000.0,
            h: 2000.0,
        };
        let shrunk = clamp_to_work_area(huge, &monitors);
        assert_eq!((shrunk.w, shrunk.h), (1440.0, 900.0));
        // No monitors (query failure) → leave the OS placement untouched.
        assert_eq!(clamp_to_work_area(win, &[]), win);
    }

    #[test]
    fn normal_bounds_ignores_transient_maximized_fullscreen() {
        let current = Rect {
            x: 0.0,
            y: 0.0,
            w: 2560.0,
            h: 1440.0,
        }; // maximized/fullscreen extent
        let prev = Rect {
            x: 200.0,
            y: 150.0,
            w: 1000.0,
            h: 700.0,
        }; // remembered normal size
        assert_eq!(normal_bounds(current, false, false, prev), current); // normal → current
        assert_eq!(normal_bounds(current, true, false, prev), prev); // maximized → keep normal
        assert_eq!(normal_bounds(current, false, true, prev), prev); // fullscreen → keep normal
    }

    #[test]
    fn traffic_light_reserve_uses_96_fallback_and_collapses_in_fullscreen() {
        // The measured value wins; 96px is the fallback; fullscreen collapses.
        assert_eq!(traffic_light_reserve_px(false, Some(78.0)), Some(78.0));
        assert_eq!(traffic_light_reserve_px(false, None), Some(96.0)); // fallback
        assert_eq!(traffic_light_reserve_px(true, Some(78.0)), None); // fullscreen collapses
        assert_eq!(traffic_light_reserve_px(true, None), None);
    }

    #[test]
    fn shell_init_script_injects_os_material_and_snapshot() {
        // The authoritative pre-boot script sets data-os/data-material + the __TARE_SHELL_INIT__
        // snapshot the web boot consumes. Compile-target-specific: this asserts the fields injected
        // by the active target; other targets are covered by the platform matrix.
        let s = shell_init_script();
        assert!(s.contains("dataset.os="));
        assert!(s.contains("dataset.material="));
        assert!(s.contains("globalThis.__TARE_SHELL_INIT__="));
        assert!(s.contains("traffic_light_reserve_px:"));
        #[cfg(target_os = "macos")]
        {
            assert!(s.contains("\"macos\""));
            assert!(s.contains("\"vibrancy\""));
            assert!(s.contains("traffic_light_reserve_px:96")); // 96 fallback on macOS
        }
    }

    #[test]
    fn port_is_live_detects_a_listening_socket_and_a_closed_one() {
        // the connect-probe that decides attach-vs-spawn. A bound loopback port reads
        // live (→ attach to the existing daemon, don't double-bind); a released one reads dead (→ spawn).
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let live_port = listener.local_addr().unwrap().port();
        assert!(port_is_live(live_port), "a bound port is detected as live");

        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let closed_port = probe.local_addr().unwrap().port();
        drop(probe); // release it — nothing listens now
        assert!(!port_is_live(closed_port), "a closed port is not live");
    }

    #[test]
    fn proxy_attachment_requires_a_tare_status_response() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        fn serve_once(body: &'static str) -> (u16, std::thread::JoinHandle<()>) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let handle = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                loop {
                    let mut chunk = [0u8; 256];
                    let read = stream.read(&mut chunk).unwrap();
                    request.extend_from_slice(&chunk[..read]);
                    if read == 0 || request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let response = format!(
                    "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
                stream.shutdown(std::net::Shutdown::Write).unwrap();
            });
            (port, handle)
        }

        let (unrelated_port, unrelated_server) = serve_once(r#"{"events":1}"#);
        assert!(!verified_tare_server(unrelated_port));
        unrelated_server.join().unwrap();

        let (tare_port, tare_server) = serve_once(
            r#"{"listening":true,"port":4318,"events":1,"last_event_unix":1,"age_seconds":0}"#,
        );
        assert!(verified_tare_server(tare_port));
        tare_server.join().unwrap();
    }

    #[test]
    fn nav_target_maps_the_web_shell_navigation_contract() {
        // `nav:<route>` → a primary destination with no param.
        assert_eq!(
            nav_target("nav:overview"),
            Some(NavTarget::Route {
                route: "overview".into(),
                param: None
            })
        );
        // `run:<id>` → the Runs screen scoped to that run.
        assert_eq!(
            nav_target("run:abc-123"),
            Some(NavTarget::Route {
                route: "runs".into(),
                param: Some("abc-123".into())
            })
        );
        // `view:<hash>` → navigate by full in-app hash (route + query).
        assert_eq!(
            nav_target("view:#/trends?range=30d"),
            Some(NavTarget::Hash("#/trends?range=30d".into()))
        );
        // A run id containing a colon keeps everything after the first prefix intact.
        assert_eq!(
            nav_target("run:2026:06:29:s1"),
            Some(NavTarget::Route {
                route: "runs".into(),
                param: Some("2026:06:29:s1".into())
            })
        );
        // Non-navigation ids (an action item, empty, bare) map to nothing.
        assert_eq!(nav_target("action:export"), None);
        assert_eq!(nav_target(""), None);
        assert_eq!(nav_target("overview"), None);
    }
}

// Real invoke round-trip over the Tauri IPC dispatcher (tare desktop-testing layer). This is the ONLY
// automated coverage of the seam GLUE the served-twin Playwright stub necessarily fakes: Tauri's
// camelCase→snake_case argument mapping and the string-vs-object return contract. It runs on the mock
// runtime (no WKWebView window is spawned), so it needs no display — but it compiles the full tauri/wry
// tree, so it is STORM-TIER: gated behind `gui-test` (which adds `tauri/test`) and run once in CI, never
// in a tight local loop (see scripts/ci.sh and docs/DESKTOP_TESTING.md). Isolation: every command here
// either ignores the DB (pricing) or is handed a throwaway temp DB path via its arg — so a test never
// touches the real store or ~/.claude.
#[cfg(all(test, feature = "gui-test"))]
mod ipc_seam_tests {
    use super::*;
    use tauri::ipc::{CallbackFn, InvokeBody};
    use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY};
    use tauri::webview::InvokeRequest;
    use tauri::WebviewWindowBuilder;

    fn request(cmd: &str, args: serde_json::Value) -> InvokeRequest {
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: if cfg!(any(windows, target_os = "android")) {
                "http://tauri.localhost"
            } else {
                "tauri://localhost"
            }
            .parse()
            .unwrap(),
            body: InvokeBody::Json(args),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        }
    }

    #[test]
    fn invoke_honors_the_arg_and_return_contract() {
        let tmp_db = std::env::temp_dir()
            .join(format!("tare-ipc-seam-{}.db", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let _ = std::fs::remove_file(&tmp_db);

        let app = mock_builder()
            .invoke_handler(tauri::generate_handler![pricing, list_runs, explain])
            .build(mock_context(noop_assets()))
            .expect("mock app builds");
        let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("mock webview builds");

        // (1) string-vs-object contract, happy path: `pricing` returns a JSON *string* (tauriClient's
        // `j<T>` does JSON.parse), NOT an object. It reads no DB, so it is deterministic.
        let priced = get_ipc_response(&webview, request("pricing", serde_json::json!({})))
            .expect("pricing succeeds")
            .deserialize::<String>()
            .expect("pricing returns a JSON string, not an object");
        serde_json::from_str::<serde_json::Value>(&priced).expect("pricing's string is valid JSON");

        // (2) object (non-string) return: `list_runs` returns a bare array, proving object commands
        // are NOT wrapped in a JSON string the way the `j<T>` set is. Keep this runtime-neutral:
        // AppHandle commands bind to the production Wry runtime and cannot be registered on Tauri's
        // MockRuntime, while this command exercises the same real IPC serialization boundary.
        let runs = get_ipc_response(
            &webview,
            request("list_runs", serde_json::json!({ "db": tmp_db })),
        )
        .expect("list_runs succeeds")
        .deserialize::<Vec<String>>()
        .expect("returns an array, not a JSON string");
        assert!(runs.is_empty(), "the isolated store starts empty");

        // (3) camelCase→snake_case arg mapping: the JS client sends `runId`; the Rust param is `run_id`.
        // Point db at a throwaway temp path (never the real store) and assert the command ROUND-TRIPS —
        // a broken mapping would surface as an "invalid args" deserialization error naming runId/run_id,
        // NOT a domain error over the (missing) db.
        if let Err(v) = get_ipc_response(
            &webview,
            request(
                "explain",
                serde_json::json!({ "db": tmp_db, "runId": "no-such-run" }),
            ),
        ) {
            let msg = v.to_string();
            assert!(
                !msg.contains("invalid args") && !msg.contains("missing required key"),
                "explain camelCase arg mapping failed (arg error, not a domain error): {msg}"
            );
        }
        let _ = std::fs::remove_file(&tmp_db);
    }
}
