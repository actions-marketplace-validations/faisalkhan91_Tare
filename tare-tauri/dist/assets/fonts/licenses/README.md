# Self-hosted font licenses

Tare self-hosts its typefaces as OFL WOFF2 files under `web/assets/fonts/`, with each family's
license text committed alongside them in `web/assets/fonts/licenses/`:

- **Archivo SemiCondensed** — compact instrument headings/labels
- **Atkinson Hyperlegible Next** — UI and prose
- **IBM Plex Mono** — amounts, IDs, axes, receipts, aligned numeric tables

The build copies `web/assets/` to `dist/assets/` (and into both embedded targets via
`scripts/build-ui.sh`). The build-manifest test (`web/test/buildManifest.test.ts`) asserts that
every local font/CSS/script URL referenced from `index.html` / `index.tauri.html` resolves to a real
source asset, so a linked-but-missing font (or an unlinked orphan) fails CI.

This directory is the committed license source copied into both embedded frontends. Subset or
replace a font only when its license remains present and the build stays reproducible.
