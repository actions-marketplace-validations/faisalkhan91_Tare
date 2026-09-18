# Playwright E2E harness

The fixture-backed Playwright suite exercises the built frontend in two projects. Both serve
`web/dist` on loopback and intercept API requests with `e2e/fixtures.ts`; neither project starts a
live backend or touches the local Tare database.

- **`chromium`** loads `index.html` and covers Pulse, Investigate, Run Profile, Compare, Optimize,
  utility sheets, onboarding, routing, responsive behavior, accessibility, and theme changes.
- **`desktop-stub`** loads `index.tauri.html` behind `e2e/tauriStub.ts`. It covers desktop boot,
  desktop CSS geometry, invoke-client selection, and native-event bridges. It does not emulate the
  platform compositor or real IPC.

See [the desktop testing guide](../../docs/DESKTOP_TESTING.md) for the complete automated/native
coverage boundary.

## Run

```bash
npm ci
npm run test:e2e

# Focused runs
npx playwright test --project=desktop-stub
npx playwright test --project=chromium
npx playwright test e2e/theme.spec.ts
npx playwright show-report
```

`playwright.config.ts` rebuilds the frontend, copies `index.tauri.html` into the temporary served
tree, and starts a loopback static server. Failures retain the configured trace, screenshot, and
video artifacts; retries are enabled only in CI.

## Browser cache

The JavaScript dependency and Chromium revision are pinned by `web/package-lock.json`. Cache the
matching browser once on a networked machine:

```bash
npx playwright install chromium
```

Offline and CI runs set `PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1`. If Playwright is updated, refresh the
browser cache for the revision named by `node_modules/playwright-core/browsers.json` before relying
on the offline gate.

Playwright uses the platform cache by default (`~/Library/Caches/ms-playwright` on macOS and
`~/.cache/ms-playwright` on Linux) or the directory selected by `PLAYWRIGHT_BROWSERS_PATH`.

## Fixture behavior

Tests may mutate only their in-memory fixture controller. Apply/dismiss/unaccept transitions,
verification windows, capture recovery, and scenario results are deterministic and do not imply
provider calls, native notification delivery, or operating-system background behavior.

Do not spawn the real `tare` binary from this suite. `scripts/ci.sh` enforces that constraint because
a live process could read local transcripts or write the user's store.

## CI

`scripts/ci.sh` runs both projects with browser downloads disabled. A host that intentionally lacks
the cached browser may set `TARE_SKIP_E2E=1`; release verification should not skip it.

Native cold-start timing, WKWebView rendering, window chrome, and OS integration remain manual
release checks documented in [the desktop testing guide](../../docs/DESKTOP_TESTING.md).
