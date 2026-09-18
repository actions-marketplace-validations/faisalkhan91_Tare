// Desktop entry point. External module (CSP `script-src 'self'`-safe) that wires the shell to the
// Rust core via Tauri `invoke` (requires `withGlobalTauri: true` so `window.__TAURI__` exists).
import { mountApp } from "./main.js";
import { createTauriClient } from "./tauriClient.js";
import { cachingClient } from "./ui/cachingClient.js";
import { navigate, parseHash, routePath } from "./ui/store.js";
import { applyOsClass, preferredMaterial } from "./ui/os.js";
import { wireSystemAccent } from "./ui/systemAccent.js";
// Native Go menu / tray navigation: the Rust shell emits a
// `navigate` event carrying `{route, param?}` (param opens a specific run), `{hash}` (a saved view's
// full route+query), or a bare route string (legacy/resilience). Notification activation uses the
// same payload contract on `notification-action`; delivery/permission/dedupe remain native concerns.
// Turn both into in-app navigation so menu, settings, and notification links share one route spine.
// Best-effort — absent on the browser transport.
export function wireNativeNav(listen) {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const l = listen ?? globalThis.__TAURI__?.event?.listen;
    if (typeof l !== "function")
        return;
    const navigateRoute = (route, param) => {
        // Tray/menu recent-run items still use the stable `{route:"runs", param}` native payload.
        // Land them on the canonical Run Profile so the rail selection, breadcrumb, and shared
        // investigation state stay coherent; keep bare `runs` as the compatibility list route.
        if (route === "runs" && param) {
            window.location.hash = routePath(["investigate", "run", param]);
            return;
        }
        if (route === "settings" || route === "connect" || route === "pricing" || route === "receipts") {
            const current = parseHash(window.location.hash);
            if (current.name === "pulse" || current.name === "investigate" || current.name === "optimize") {
                const query = {
                    ...(current.query ?? {}),
                    sheet: route === "settings" ? "settings" : route === "connect" ? "capture" : "trust",
                };
                if (route === "pricing") {
                    if (query.view && query.view !== "pricing")
                        query.workspace_view = query.view;
                    query.view = "pricing";
                }
                window.location.hash = routePath(current.segments, query);
                return;
            }
        }
        navigate(route, param);
    };
    const handle = (e) => {
        const p = e.payload;
        if (typeof p === "string" && p) {
            navigateRoute(p);
        }
        else if (p && typeof p === "object") {
            const o = p;
            if (typeof o.hash === "string" && o.hash.startsWith("#/")) {
                window.location.hash = o.hash; // a saved view carries its full route+query
            }
            else if (typeof o.route === "string" && o.route) {
                navigateRoute(o.route, typeof o.param === "string" ? o.param : undefined);
            }
        }
    };
    l("navigate", handle);
    l("notification-action", handle);
}
// Bridge native menu actions to the WebView: the Rust menu handler emits an event per
// action (View ▸ Command Palette → `open-palette`); re-dispatch each as a `tare:<action>` DOM event
// the mounted app listens for. Best-effort — absent on the browser transport (there the ⌘K/Ctrl-K
// hotkey opens the palette). Theme is no longer a bridged action: it is
// System/Light/Dark in Settings → Appearance only, so there is no `toggle-theme` bridge.
export function wireMenuActions(listen) {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const l = listen ?? globalThis.__TAURI__?.event?.listen;
    if (typeof l !== "function")
        return;
    // `find` bridges the macOS Edit▸Find menu item to the in-webview find bar.
    for (const action of ["open-palette", "find"]) {
        l(action, () => window.dispatchEvent(new CustomEvent(`tare:${action}`)));
    }
}
// Reflect native fullscreen into a `data-fullscreen` flag on <html> so the macOS
// traffic-light reserve can collapse when the OS hides the controls. Best-effort via the Tauri window
// API (getCurrentWindow().isFullscreen() re-checked on resize); absent on the browser transport.
export function wireFullscreen(getWin) {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const w = globalThis.__TAURI__?.window;
    const cur = getWin?.() ?? w?.getCurrentWindow?.() ?? w?.getCurrent?.();
    if (!cur?.isFullscreen)
        return;
    const sync = () => {
        cur
            .isFullscreen()
            .then((fs) => {
            if (fs)
                document.documentElement.dataset.fullscreen = "true";
            else
                delete document.documentElement.dataset.fullscreen;
        })
            .catch(() => { });
    };
    cur.onResized?.(() => sync());
    sync();
}
// Reflect the OS accessibility traits the WebView already honors via media queries onto <html> so
// CSS can scope shell chrome to them. High contrast → `data-high-contrast`
// (drives flat, higher-border surfaces + disables vibrancy); Reduce Transparency → flat material.
// (Reduced MOTION is handled directly by the `prefers-reduced-motion` CSS media queries — no attr
// needed.) Returns the resolved traits so the caller can fold them into the material choice.
export function applyA11yTraits(root = document.documentElement) {
    const mm = (q) => typeof window !== "undefined" &&
        typeof window.matchMedia === "function" &&
        window.matchMedia(q).matches;
    const reduceTransparency = mm("(prefers-reduced-transparency: reduce)");
    const highContrast = mm("(prefers-contrast: more)") || mm("(forced-colors: active)");
    if (highContrast)
        root.dataset.highContrast = "true";
    else
        delete root.dataset.highContrast;
    return { reduceTransparency, highContrast };
}
// Reflect window active/inactive onto `data-inactive` so the shell can quiet
// its chrome when the window loses focus — a native app cue. Best-effort via the Tauri window focus
// event; absent on the browser transport. `getWin` injectable for tests.
export function wireWindowActive(getWin, root = document.documentElement) {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const w = globalThis.__TAURI__?.window;
    const cur = getWin?.() ?? w?.getCurrentWindow?.() ?? w?.getCurrent?.();
    const set = (focused) => {
        if (focused)
            delete root.dataset.inactive;
        else
            root.dataset.inactive = "true";
    };
    set(true); // assume active on load
    cur?.onFocusChanged?.((e) => set(!!e.payload));
}
// Consume the native pre-boot ShellInit snapshot: the Rust init-script set
// data-os / data-material + `globalThis.__TARE_SHELL_INIT__` at document-start (before first paint).
// Apply its authoritative traffic-light reserve to the CSS var the shell chrome uses; the desktop.css
// 96px default stands in when no snapshot was injected (browser transport). Best-effort.
export function consumeShellInit(root = document.documentElement) {
    const init = globalThis
        .__TARE_SHELL_INIT__;
    if (init && typeof init.traffic_light_reserve_px === "number") {
        root.style.setProperty("--traffic-light-reserve", `${init.traffic_light_reserve_px}px`);
    }
}
const root = document.getElementById("app");
if (root) {
    consumeShellInit(); // apply the authoritative traffic-light reserve before chrome lays out
    const os = applyOsClass(); // consumes the injected data-os (falls back to UA only off-native)
    // Reflect OS accessibility traits and fold them into the material:
    // vibrancy only on macOS+Tauri when the user has NOT turned on Reduce Transparency OR Increase
    // Contrast; otherwise the flat, high-legibility surface.
    const { reduceTransparency, highContrast } = applyA11yTraits();
    document.documentElement.dataset.material = preferredMaterial(os, true, reduceTransparency, highContrast);
    wireWindowActive(); // quiet the chrome when the window is inactive
    wireNativeNav();
    wireSystemAccent(); // Drive affordances from the OS accent emitted by the Rust shell.
    wireMenuActions(); // Bridge native View-menu actions into the WebView.
    wireFullscreen(); // Collapse the macOS traffic-light reserve in native fullscreen.
    try {
        // Pulse has no alternate dashboard state to seed, so boot mounts straight into the shell.
        mountApp(root, cachingClient(createTauriClient())).catch((e) => {
            root.textContent = "Failed to start: " + String(e);
        });
    }
    catch (e) {
        root.textContent = "Failed to start: " + String(e);
    }
}
