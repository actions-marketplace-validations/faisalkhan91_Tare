# Testing the desktop app

The browser viewer and Tauri desktop app render the same frontend. Most desktop behavior is tested
without launching a native window; only compositor and operating-system integration remain manual.

## Coverage layers

| Layer | Command | Covers |
| --- | --- | --- |
| Frontend unit tests | `npm --prefix web test` | Desktop CSS rules, command registry, Tauri client and stub contracts, route/state behavior |
| Browser journeys | `npm --prefix web run test:e2e` | Built browser and desktop-stub workflows against deterministic fixtures |
| Headless command logic | `cargo test -p tare-tauri --offline` | Transport-independent Rust command behavior |
| Native invoke seam | `cargo test -p tare-tauri --features gui-test --offline` | Real dispatcher argument mapping and return shapes without opening a window |
| Native release check | Manual checklist below | WKWebView rendering, native chrome, menus, tray, notifications, and window lifecycle |

`scripts/ci.sh` runs the automated layers. Hosts without platform WebView development libraries may
set `TARE_SKIP_GUI_LINT=1`; hosts without the lockfile-pinned Chromium cache may set
`TARE_SKIP_E2E=1`. Official release checks should exercise both.

For individual Playwright projects and browser-cache setup, see
[the E2E runner guide](../web/e2e/README.md).

## Command seam

Three definitions must remain synchronized:

- `web/src/tauriClient.ts` contains the commands the desktop frontend invokes and their return
  classification.
- `tare-tauri/src/gui.rs` registers the native handlers with `tauri::generate_handler!`.
- `web/e2e/tauriStub.ts` provides deterministic responses for the served desktop twin.

`web/test/tauriStubContract.test.ts` checks that the client, native registry, and stub agree. Do not
document or assert a fixed command count; the contract test derives it from the current sources.

## Hermeticity and local-data safety

The Playwright suite builds `web/dist`, serves it on loopback, and intercepts read API calls with
fixtures. It does not start `tare`, scan transcript directories, modify agent configuration, or open
the real SQLite store.

The GUI seam tests use temporary paths for commands that need storage and avoid commands with
operating-system side effects. `scripts/ci.sh` also rejects frontend tests that spawn the real Tare
binary.

## Desktop served twin

The `desktop-stub` Playwright project loads `web/index.tauri.html` behind a stubbed
`window.__TAURI__`. It verifies desktop-only boot, computed desktop CSS, navigation events, command
palette and find events, system accent handling, and the macOS traffic-light reserve as rendered
geometry.

This is not a WKWebView emulator. Chromium cannot verify native traffic lights, vibrancy over the
wallpaper, title-bar drag hit testing, platform font rasterization, or actual IPC transport.

## Manual native checklist

Build current frontend assets and launch the real app:

```bash
bash scripts/build-ui.sh
cargo run -p tare-tauri --features gui --profile dev-fast
```

Before a desktop release, verify on each supported platform:

- The window paints in the correct theme without an initial white or dark flash.
- Minimum-size, maximized, full-screen, and restored windows keep controls visible and usable.
- macOS traffic lights clear the web title area; drag regions move the window and do not swallow
  control interaction.
- Platform material or translucency is limited to shell chrome and falls back to an opaque surface
  for Reduce Transparency, increased contrast, forced colors, and inactive windows.
- Native menus, tray actions, command palette, Find, and recent-run navigation reach the intended
  in-app state.
- Close, reopen, quit, background capture, and saved window geometry follow platform conventions.
- Notifications request permission appropriately, deduplicate, activate the relevant view, and
  withdraw stale entries where supported.
- One command crosses the real frontend/native IPC boundary successfully.
- VoiceOver, Narrator, or Orca can identify the primary navigation, current workspace, controls,
  sheets, status, and chart alternatives.

Record platform-specific failures in the release notes or issue tracker rather than weakening the
automated browser assertions.

## Embedded UI synchronization

`scripts/build-ui.sh` builds `web/dist` and replaces both committed embeds:

- `tare-cli/assets/ui` for the browser viewer
- `tare-tauri/dist` for the desktop app, with the Tauri-specific `index.html`

Verify them with:

```bash
bash scripts/build-ui.sh
bash scripts/check-ui-sync.sh
```

The strict CI path also checks that regeneration leaves no uncommitted embed changes. In an already
dirty working tree, `TARE_SKIP_ASSET_GIT_DRIFT=1 bash scripts/ci.sh` uses byte comparison instead.

## `withGlobalTauri`

`tare-tauri/tauri.conf.json` intentionally enables `withGlobalTauri`. The frontend invokes native
commands through `globalThis.__TAURI__` in `web/src/tauriClient.ts` and wires native events in
`web/src/bootTauri.ts`; it does not depend on `@tauri-apps/api`.

Keep the content-security policy self-only and the capability allowlist minimal. Disabling the
global without first migrating those call sites breaks desktop IPC.
