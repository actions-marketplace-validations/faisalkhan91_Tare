#!/usr/bin/env bash
# Reproducible font vendoring. Downloads the three Calibrated Bench OFL
# typefaces as VERBATIM WOFF2 (no subsetting — no subsetting toolchain is assumed, so verbatim keeps
# the vendoring reproducible from pinned, immutable CDN URLs) plus each family's OFL license text into
# web/assets/fonts/. Re-running is idempotent: it re-fetches the same pinned versions and overwrites.
#
# Source: Fontsource packages on the jsDelivr CDN (immutable @version URLs; each package ships the
# upstream OFL license). Latin subset only (the app UI is Latin); extend here if that changes.
#
# Usage:  bash scripts/fetch-fonts.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FILES="$ROOT/web/assets/fonts/files"
LIC="$ROOT/web/assets/fonts/licenses"
mkdir -p "$FILES" "$LIC"

CDN="https://cdn.jsdelivr.net/npm"

# Pinned package versions (bump deliberately; the manifest test guards the referenced files).
IBM="@fontsource/ibm-plex-mono@5.0.13"
ATK="@fontsource/atkinson-hyperlegible-next@5.2.5"
ARCHIVO="@fontsource-variable/archivo@5.2.6"

fetch() { # url dest
  echo "  ↓ $2"
  curl -fsSL "$1" -o "$2"
}

echo "Fetching WOFF2 files → $FILES"
# IBM Plex Mono — amounts, IDs, axes, receipts, aligned numeric tables (400 body, 600 emphasis).
fetch "$CDN/$IBM/files/ibm-plex-mono-latin-400-normal.woff2" "$FILES/ibm-plex-mono-latin-400-normal.woff2"
fetch "$CDN/$IBM/files/ibm-plex-mono-latin-600-normal.woff2" "$FILES/ibm-plex-mono-latin-600-normal.woff2"
# Atkinson Hyperlegible Next — UI and prose (400 body, 600 medium, 700 bold).
fetch "$CDN/$ATK/files/atkinson-hyperlegible-next-latin-400-normal.woff2" "$FILES/atkinson-hyperlegible-next-latin-400-normal.woff2"
fetch "$CDN/$ATK/files/atkinson-hyperlegible-next-latin-600-normal.woff2" "$FILES/atkinson-hyperlegible-next-latin-600-normal.woff2"
fetch "$CDN/$ATK/files/atkinson-hyperlegible-next-latin-700-normal.woff2" "$FILES/atkinson-hyperlegible-next-latin-700-normal.woff2"
# Archivo (variable: wght + wdth) — compact instrument headings/labels; SemiCondensed via wdth axis.
fetch "$CDN/$ARCHIVO/files/archivo-latin-standard-normal.woff2" "$FILES/archivo-latin-standard-normal.woff2"

echo "Fetching OFL licenses → $LIC"
fetch "$CDN/$IBM/LICENSE" "$LIC/IBMPlexMono-OFL.txt"
fetch "$CDN/$ATK/LICENSE" "$LIC/AtkinsonHyperlegibleNext-OFL.txt"
fetch "$CDN/$ARCHIVO/LICENSE" "$LIC/Archivo-OFL.txt"

echo "Done. Pinned versions: $IBM  $ATK  $ARCHIVO"
