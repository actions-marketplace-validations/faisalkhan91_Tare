#!/usr/bin/env bash
# Hermetic tests for scripts/check-ui-sync.sh. Builds throwaway fixture trees in a
# temp dir and drives check-ui-sync.sh through its directory overrides, asserting it PASSES when the
# embeds match and FAILS on a missing, extra, or changed file — including the Tauri index swap.
# Touches no repo file. Run: bash scripts/check-ui-sync.test.sh
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
script="$here/scripts/check-ui-sync.sh"
work="$(mktemp -d "${TMPDIR:-/tmp}/tare-ui-sync-test.XXXXXX")"
trap 'rm -rf "$work"' EXIT

pass=0
fail=0
# run <expect: ok|err> <description>  — sets up env-overridden dirs already prepared by the caller.
check() {
  local expect="$1" desc="$2"
  local rc=0
  CHECK_UI_DIST="$DIST" CHECK_UI_CLI="$CLI" CHECK_UI_TAURI="$TAURI" \
    CHECK_UI_TAURI_INDEX="$TIDX" bash "$script" >/dev/null 2>&1 || rc=$?
  if { [ "$expect" = "ok" ] && [ "$rc" -eq 0 ]; } || { [ "$expect" = "err" ] && [ "$rc" -ne 0 ]; }; then
    echo "  PASS: $desc (exit $rc)"
    pass=$((pass + 1))
  else
    echo "  FAIL: $desc — expected $expect, got exit $rc"
    fail=$((fail + 1))
  fi
}

# Build a clean, in-sync fixture: dist + an exact CLI copy + a Tauri copy with the index swapped.
setup_clean() {
  rm -rf "$work"/*
  DIST="$work/dist"; CLI="$work/cli"; TAURI="$work/tauri"; TIDX="$work/index.tauri.html"
  mkdir -p "$DIST/screens"
  printf 'browser index\n' >"$DIST/index.html"
  printf 'console.log(1)\n' >"$DIST/main.js"
  printf 'body{}\n' >"$DIST/screens/app.css"
  printf 'desktop index\n' >"$TIDX"
  # CLI = exact copy of dist.
  cp -R "$DIST" "$CLI"
  # Tauri = copy of dist with index.html replaced by the desktop index.
  cp -R "$DIST" "$TAURI"
  cp "$TIDX" "$TAURI/index.html"
}

echo "check-ui-sync.test: scenarios"

setup_clean
check ok "clean build: CLI and Tauri both match web/dist"

setup_clean
rm "$CLI/main.js"
check err "missing file in CLI embed is detected"

setup_clean
printf 'stray\n' >"$CLI/extra.js"
check err "extra file in CLI embed is detected"

setup_clean
printf 'console.log(2)\n' >"$CLI/main.js"
check err "changed file in CLI embed is detected"

setup_clean
# Tauri index must be the DESKTOP index; a stale browser index.html must fail.
printf 'browser index\n' >"$TAURI/index.html"
check err "wrong Tauri index.html (browser instead of desktop) is detected"

setup_clean
printf 'body{color:red}\n' >"$TAURI/screens/app.css"
check err "changed non-index file in Tauri embed is detected"

setup_clean
for tree in "$DIST" "$CLI" "$TAURI"; do
  printf 'desktop test entry\n' >"$tree/index.tauri.html"
done
check err "matching embeds still reject a leaked Playwright-only desktop entry"

setup_clean
rm -rf "$TAURI"
check err "missing Tauri embed dir is detected"

setup_clean
rm -rf "$DIST"
check err "missing web/dist is detected"

echo "check-ui-sync.test: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
