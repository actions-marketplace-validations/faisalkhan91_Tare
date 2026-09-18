#!/usr/bin/env bash
# Verify the embedded UI assets match a fresh web build, byte-for-byte, without relying on Git.
# `scripts/build-ui.sh` rewrites the committed asset trees under `tare-cli/assets/ui` and
# `tare-tauri/dist`; this script provides a direct tree comparison that also works before staging.
#
# What it checks:
#   1. tare-cli/assets/ui        == web/dist                                (browser embed)
#   2. tare-tauri/dist           == web/dist with index.html replaced by
#                                    web/index.tauri.html                   (desktop embed)
# Both comparisons fail on ANY missing, extra, or changed file.
#
# Usage:
#   bash scripts/build-ui.sh          # produce web/dist and copy into both embed targets
#   bash scripts/check-ui-sync.sh     # confirm the copies match web/dist (+ the tauri index swap)
#
# Directory overrides (used by scripts/check-ui-sync.test.sh to exercise pass/fail hermetically;
# default to the real repo paths):
#   CHECK_UI_DIST         web build output        (default web/dist)
#   CHECK_UI_CLI          browser embed target    (default tare-cli/assets/ui)
#   CHECK_UI_TAURI        desktop embed target    (default tare-tauri/dist)
#   CHECK_UI_TAURI_INDEX  desktop index.html src  (default web/index.tauri.html)
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
dist="${CHECK_UI_DIST:-$here/web/dist}"
cli="${CHECK_UI_CLI:-$here/tare-cli/assets/ui}"
tauri="${CHECK_UI_TAURI:-$here/tare-tauri/dist}"
tauri_index="${CHECK_UI_TAURI_INDEX:-$here/web/index.tauri.html}"

if [ ! -d "$dist" ]; then
  echo "check-ui-sync: web build output not found at $dist — run scripts/build-ui.sh first" >&2
  exit 1
fi
if [ ! -f "$tauri_index" ]; then
  echo "check-ui-sync: desktop index not found at $tauri_index" >&2
  exit 1
fi

# Playwright temporarily adds this desktop-only entry beside the browser build so its static test
# server can exercise both targets. It is never a shipping asset. Refuse a byte-for-byte match that
# merely copied the stale test file into both embeds; `npm run build` must start from a clean dist.
for tree in "$dist" "$cli" "$tauri"; do
  if [ -f "$tree/index.tauri.html" ]; then
    echo "check-ui-sync: FAIL test-only index.tauri.html leaked into $tree" >&2
    exit 1
  fi
done

# Recursively compare two trees, reporting missing ("Only in <expected>"), extra
# ("Only in <actual>"), and changed ("Files ... differ") files. `diff -rq` exits non-zero on any
# difference; we surface its report and mark the run failed.
fail=0
compare_trees() {
  local expected="$1" actual="$2" label="$3"
  if [ ! -d "$actual" ]; then
    echo "check-ui-sync: FAIL $label — embed target $actual does not exist" >&2
    fail=1
    return
  fi
  local out
  if out="$(diff -rq "$expected" "$actual" 2>&1)"; then
    echo "check-ui-sync: OK   $label matches web/dist"
  else
    echo "check-ui-sync: FAIL $label differs from the web build (missing/extra/changed files):" >&2
    echo "$out" | sed 's/^/    /' >&2
    fail=1
  fi
}

# 1) Browser embed: exact copy of web/dist.
compare_trees "$dist" "$cli" "tare-cli/assets/ui"

# 2) Desktop embed: web/dist with the Tauri index substituted for index.html.
tmp="$(mktemp -d "${TMPDIR:-/tmp}/tare-ui-sync.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
cp -R "$dist/." "$tmp/"
cp "$tauri_index" "$tmp/index.html"
compare_trees "$tmp" "$tauri" "tare-tauri/dist"

if [ "$fail" -ne 0 ]; then
  echo "check-ui-sync: embedded UI assets are OUT OF SYNC — re-run scripts/build-ui.sh" >&2
  exit 1
fi
echo "check-ui-sync: all embedded UI assets are in sync with the web build."
