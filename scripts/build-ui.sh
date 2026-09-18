#!/usr/bin/env bash
# Build the web UI (tsc emit) and sync it into the committed asset dir that `tare-cli` embeds
# (so `tare serve` ships a self-contained shell with no runtime file dependency). Reproducible:
# ci.sh runs this then `git diff --quiet` on the asset dir to catch drift.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
cd "$here/web"
[ -d node_modules ] || npm install --prefer-offline --no-audit --no-fund
npm run build
# Browser target: tare-cli embeds this (index.html bootstraps the HTTP client).
dest="$here/tare-cli/assets/ui"
rm -rf "$dest"
mkdir -p "$dest"
cp -R dist/. "$dest/"

# Desktop target: Tauri serves this (same module tree; index bootstraps the invoke client).
tauri="$here/tare-tauri/dist"
rm -rf "$tauri"
mkdir -p "$tauri"
cp -R dist/. "$tauri/"
cp index.tauri.html "$tauri/index.html"

echo "build-ui: synced web/dist -> tare-cli/assets/ui and tare-tauri/dist"
