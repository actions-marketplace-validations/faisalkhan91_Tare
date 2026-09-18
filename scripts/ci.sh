#!/usr/bin/env bash
# Tare CI gate — runs the entire verifiable product offline, on loopback only.
# No network, no API key, no display. Exits 0 only when everything is green.
#
# Network discipline:
#   - TARE_NETWORK_GUARD=loopback makes the proxy refuse any non-loopback upstream,
#     and the proxy test suite asserts a non-loopback connect fails loudly.
#   - cargo runs with --offline (deps must already be cached); the test suite makes
#     only 127.0.0.1 connections.
# Out of scope here (per design): tauri build/dev/tauri-driver/bundling/signing — the shipped desktop
# bundle is built by .github/workflows/release.yml, not this gate. The `--features gui-test` shell
# compile+tests (clippy + the IPC-seam round-trip) DO run below, but are storm-tier and individually
# skippable with TARE_SKIP_GUI_LINT=1 (see docs/DESKTOP_TESTING.md).

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# shellcheck disable=SC1090
source "$HOME/.cargo/env" 2>/dev/null || true

export TARE_NETWORK_GUARD=loopback
export CARGO_TERM_COLOR=always

step() { printf '\n==> %s\n' "$1"; }

step "rustfmt --check"
cargo fmt --all -- --check

step "desktop icon source + release bundle assets"
python3 - <<'PY'
import json
from pathlib import Path

root = Path("tare-tauri")
source = Path("assets/app-icon.svg")
if not source.is_file() or source.stat().st_size == 0:
    raise SystemExit(f"ERROR: missing non-empty desktop icon source: {source}")

config = json.loads((root / "tauri.release.conf.json").read_text())
required = [root / path for path in config["bundle"]["icon"]]
# `generate_context!` loads this conventional source icon even when bundle.active=false.
required.append(root / "icons/icon.png")
missing = [str(path) for path in required if not path.is_file() or path.stat().st_size == 0]
if missing:
    raise SystemExit("ERROR: missing non-empty desktop icon asset(s): " + ", ".join(missing))
PY

step "release metadata + hermetic installer contract"
python3 scripts/check-release-metadata.py
bash scripts/install.test.sh

step "clippy (workspace, default/headless features, warnings = errors)"
cargo clippy --workspace --all-targets --offline -- -D warnings

step "cargo build (workspace, incl. tare-tauri headless compile)"
cargo build --workspace --offline

step "cargo test (workspace, network guard on)"
cargo test --workspace --offline

step "feature-gated subsystems (webhook egress sink + gui shell) compile & lint"
# These features are off by default, so the workspace steps above never touch them; lint/test them
# explicitly so a whole subsystem can't silently rot. The webhook sink is pure Rust (always runs).
cargo test -p tare-daemon --features webhooks --offline
cargo clippy -p tare-daemon --features webhooks --all-targets --offline -- -D warnings
# The gui shell needs the platform WebView/-sys libs (always present on macOS). On a headless box
# without them, set TARE_SKIP_GUI_LINT=1 to skip just this step. Run under `gui-test` (= gui +
# tauri/test) so the gui-gated unit tests (nav-payload contract, and the IPC-seam invoke
# round-trip that pins camelCase→snake_case args + the string-vs-object return contract) both run. This
# is the single WORST storm trigger in the tree (it links the whole tauri/wry/WebKit stack), so it is
# individually skippable and is NOT part of the tight-loop dev gate — see docs/DESKTOP_TESTING.md.
if [ "${TARE_SKIP_GUI_LINT:-0}" != "1" ]; then
  cargo clippy -p tare-tauri --features gui-test --offline -- -D warnings
  cargo test -p tare-tauri --features gui-test --offline
else
  echo "  (skipped gui shell lint: TARE_SKIP_GUI_LINT=1)"
fi

step "web UI typecheck + tests (vitest + jsdom, no browser, no network)"
(
  cd web
  if [ ! -d node_modules ]; then
    npm install --prefer-offline --no-audit --no-fund
  fi
  npm run typecheck
  npm test
)

step "E2E journeys (Playwright, fixture-backed, offline-cached chromium): browser + desktop twins"
# Mandatory Pulse/Investigate/Optimize journey + responsive/a11y gates,
# plus the desktop-stub project (the served twin of the Tauri WebView — index.tauri.html behind a
# __TAURI__ stub; see docs/DESKTOP_TESTING.md). `npm run test:e2e` runs BOTH projects. The suite builds
# web/dist and serves it against deterministic fixtures — no live backend, loopback only. The pinned chromium (rev 1228 via @playwright/test 1.61.1) MUST be cached before CI depends on
# this step (see web/e2e/README.md); PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 keeps the run fully offline and
# makes a missing browser fail fast with the expected revision rather than silently downloading. On a
# headless box with no cached browser, set TARE_SKIP_E2E=1 to skip just this step (verified separately,
# like the gui shell lint). Failures emit trace/screenshot/video + an HTML report (playwright.config.ts).
if [ "${TARE_SKIP_E2E:-0}" != "1" ]; then
  (
    cd web
    if [ ! -d node_modules ]; then
      npm install --prefer-offline --no-audit --no-fund
    fi
    PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm run test:e2e
  )
else
  echo "  (skipped browser E2E: TARE_SKIP_E2E=1)"
fi

step "tests stay hermetic (never shell out to a real tare serve/backfill/connect)"
# AI-config + real-DB safety invariant (tare desktop-testing layer): spawning the real `tare` binary in
# a browser/unit test would read ~/.claude transcripts (capture is ON by default) and write the real
# store. Tests must never do that — the browser suite uses static fixtures and the gui.rs IPC round-trip
# hands each command a throwaway temp DB. Flag any child-process spawn that runs `tare` (comments and
# UI-string assertions like toContain("tare connect") are not spawns, so they don't trip this).
if grep -REn '(execSync|execFileSync|execFile|spawnSync|spawn|exec)\s*\(' web/e2e web/test \
     --include='*.ts' --include='*.mjs' --include='*.js' 2>/dev/null | grep -w tare; then
  echo "ERROR: a browser/unit test spawns the real 'tare' binary — it would read ~/.claude (capture on" >&2
  echo "       by default) and write the real DB. Use static fixtures / a temp DB instead." >&2
  exit 1
fi

step "role-token contrast + visual guards (both themes, WCAG floors + severity/accent separation)"
# Offline OKLCH contrast verifier: asserts every role clears its WCAG bar in BOTH
# themes, severity ramps stay lightness-separated, and the brand accent is hue-separated from every
# severity color. Exits non-zero on any regression (TOTAL FAILS > 0). The forced-color guards for
# data/accent/warning/Beam are CSS-source lint tests run by `npm test` above (forcedColors.test.ts).
python3 scripts/verify-contrast.py

step "web app builds + embedded UI assets are reproducible and committed"
(
  bash scripts/check-ui-sync.test.sh
  bash scripts/build-ui.sh
  # Default (strict) path: the fresh build must leave the committed asset trees clean/staged, i.e.
  # a real drift means uncommitted generated output. This is the CI-authoritative check.
  #
  # Local path for agents NOT authorized to stage/commit (TARE_SKIP_ASSET_GIT_DRIFT=1): the strict
  # git check can't pass without staging even when the assets are perfectly in sync, so verify the
  # embeds match the fresh web/dist byte-for-byte instead (git-independent). Never stage or commit
  # merely to satisfy the strict check. See scripts/check-ui-sync.sh and docs/DESKTOP_TESTING.md.
  if [ "${TARE_SKIP_ASSET_GIT_DRIFT:-0}" = "1" ]; then
    echo "  (TARE_SKIP_ASSET_GIT_DRIFT=1: byte-comparing embeds via check-ui-sync.sh, skipping git-drift)"
    bash scripts/check-ui-sync.sh
  elif ! git diff --quiet -- tare-cli/assets/ui tare-tauri/dist; then
    echo "ERROR: embedded UI assets drifted from a fresh web build; commit the rebuilt assets." >&2
    git --no-pager diff --stat -- tare-cli/assets/ui tare-tauri/dist >&2
    exit 1
  fi
)

step "no machine-local privacy salt/config is tracked"
if git ls-files --error-unmatch tare.toml >/dev/null 2>&1 || git ls-files '*.salt' | grep -q .; then
  echo "ERROR: a privacy salt/config file (tare.toml / *.salt) is tracked — it must stay local." >&2
  exit 1
fi

step "hero SVG is reproducible and committed"
cargo run --offline -q -p tare-cli --bin tare -- demo --svg assets/hero-flamegraph.svg
if ! git diff --quiet -- assets/hero-flamegraph.svg; then
  echo "ERROR: assets/hero-flamegraph.svg changed when regenerated — commit the update." >&2
  exit 1
fi

printf '\n\033[32mALL GREEN\033[0m — capture->parse->attribute->cost->store->render verified offline.\n'
