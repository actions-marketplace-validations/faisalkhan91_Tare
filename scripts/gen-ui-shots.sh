#!/usr/bin/env bash
# Renders the real-UI screenshots the README embeds: assets/screenshots/{pulse,run-profile,optimize}.png.
# Fully offline and deterministic. It drives the existing fixture-backed Playwright page sweep
# (web/e2e/page-by-page.spec.ts), which loads the BUILT web UI (web/dist) with the read API stubbed
# by e2e/fixtures.ts — no live backend, no network, no real db. Same harness ci.sh already runs, so the
# shots are the actual product, not a mock.
#
# Why this and not shot-desktop.sh: shot-desktop.sh grabs the native macOS window (traffic lights +
# titlebar) and needs a display + a cargo-built app; it is manual, not reproducible. This path renders
# the same web surfaces headlessly from committed fixtures, so anyone can regenerate the README shots
# with one command.
#
# Usage: scripts/gen-ui-shots.sh
# Requires the cached chromium (rev pinned by @playwright/test) — see web/e2e/README.md for the
# offline-cache contract. Skips the download at run time (PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/assets/screenshots"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Render every desktop-light surface into the temp dir (light theme, 1440x900 viewport).
( cd "$ROOT/web" && TARE_PAGE_SCREENSHOT_DIR="$TMP" PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 \
    npx playwright test e2e/page-by-page.spec.ts --project=chromium -g "desktop-light" --reporter=line )

# Curated set: one shot per primary workspace (Pulse / Investigate / Optimize).
declare -a MAP=(
  "desktop-light-pulse.png|pulse.png"
  "desktop-light-run-profile-profile.png|run-profile.png"
  "desktop-light-optimize-open.png|optimize.png"
)

mkdir -p "$OUT"
for entry in "${MAP[@]}"; do
  src="${entry%%|*}"; dst="${entry##*|}"
  [ -f "$TMP/$src" ] || { echo "gen-ui-shots: expected shot missing: $src" >&2; exit 1; }
  cp "$TMP/$src" "$OUT/$dst"
  # Report dimensions when Pillow is available (house style; optional dependency).
  python3 - "$OUT/$dst" 2>/dev/null <<'PY' || echo "wrote $OUT/$dst"
import sys
from PIL import Image
p = sys.argv[1]
w, h = Image.open(p).size
print(f"wrote {p} ({w}x{h})")
PY
done
