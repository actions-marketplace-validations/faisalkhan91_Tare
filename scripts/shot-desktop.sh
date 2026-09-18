#!/usr/bin/env bash
# Capture the REAL native macOS desktop window (traffic lights + titlebar chrome + content) of the
# Tare Tauri app — for checking titlebar alignment, window chrome, and anything the webview-only
# capture can't see.
#
# Why this and not playwright/webview capture: playwright (and Electron/Tauri capturePage) only
# render the WEB content — they CANNOT show the native macOS traffic lights or the titlebar overlay,
# so alignment bugs between the custom titlebar and the native window controls are invisible to them.
# The only faithful capture is an OS-level window grab: macOS `screencapture` of the running window.
#
# Method: launch tare-desktop against an ISOLATED temp db (never your real ~/.tare/tare.db), read the
# window rect via AppleScript (needs Accessibility permission, already granted in a normal login),
# then `screencapture -R<rect>` (retina, no dependencies). For per-ROUTE *content* sweeps across all
# screens, prefer the faster webview route (the Playwright e2e helper under web/e2e) — this
# script is for the native chrome.
#
# Usage: scripts/shot-desktop.sh [OUTPUT_PNG] [DB_PATH]
#   OUTPUT_PNG  where to write (default: /tmp/tare-desktop.png)
#   DB_PATH     db to show (default: a temp demo-seeded db under $TMPDIR)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-/tmp/tare-desktop.png}"
DB="${2:-}"
BIN="$ROOT/target/dev-fast/tare-desktop"; [ -x "$BIN" ] || BIN="$ROOT/target/debug/tare-desktop"
[ -x "$BIN" ] || { echo "no tare-desktop binary — build with: cargo build -p tare-tauri --features gui --profile dev-fast"; exit 1; }

TMP="$(mktemp -d)"; export TARE_CONFIG="$TMP/tare.toml"   # temp config so we never touch the user's
if [ -z "$DB" ]; then
  DB="$TMP/shot.db"; "$ROOT/target/debug/tare" demo --db "$DB" >/dev/null 2>&1 || true
fi
export TARE_DB="$DB"

"$BIN" >/dev/null 2>&1 &
APP=$!
trap 'kill "$APP" 2>/dev/null || true; rm -rf "$TMP"' EXIT
sleep 7
osascript -e 'tell application "System Events" to tell process "tare-desktop" to set frontmost to true' >/dev/null 2>&1 || true
sleep 1
BOUNDS="$(osascript -e 'tell application "System Events" to tell process "tare-desktop" to get {position, size} of window 1' 2>/dev/null)"
read -r X Y W H < <(echo "$BOUNDS" | tr -d ' ' | awk -F, '{print $1, $2, $3, $4}')
[ -n "${W:-}" ] || { echo "could not read window bounds (is the window open + Accessibility granted?)"; exit 1; }
screencapture -x -R"${X},${Y},${W},${H}" "$OUT"
echo "captured native window -> $OUT (${W}x${H} @2x)"
