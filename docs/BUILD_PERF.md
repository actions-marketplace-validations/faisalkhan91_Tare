# Desktop development and build performance

The Tauri GUI pulls in the platform WebView stack, so a cold native build is much heavier than a
frontend change or a headless Rust check. Use the smallest loop that exercises the code you changed.

## Frontend changes

Run the frontend directly, then regenerate the two committed embeds:

```bash
npm --prefix web run typecheck
npm --prefix web test
bash scripts/build-ui.sh
bash scripts/check-ui-sync.sh
```

Install dependencies first with `npm --prefix web ci` when `web/node_modules` is absent. Frontend
edits do not require recompiling the native shell.

## Rust command and adapter changes

The default `tare-tauri` feature set excludes the platform GUI stack:

```bash
cargo check -p tare-tauri --offline
cargo test -p tare-tauri --offline
```

Use this loop for transport-independent command logic. Run the GUI seam tests only when native
command registration or argument/return mapping changes:

```bash
cargo test -p tare-tauri --features gui-test --offline
```

## Native-shell changes

Launch the real desktop app when changing window creation, menus, tray behavior, native events, or
platform appearance:

```bash
bash scripts/build-ui.sh
cargo run -p tare-tauri --features gui --profile dev-fast
```

Keep the same profile and feature set during an iteration so Cargo can reuse compiled dependencies.
Avoid `cargo clean` unless the build cache is corrupt or disk cleanup is the actual goal.

## Build profiles

The workspace profiles in `Cargo.toml` reduce local GUI build cost:

- `dev` keeps line-table debug information and incremental compilation.
- `dev-fast` removes debug information and strips it from the executable for the quickest native
  run loop.
- Build dependencies use optimization so frequently executed procedural macros and build scripts
  do not remain unoptimized.

Use the ordinary `dev` profile while debugging and `dev-fast` while checking behavior. For a
release-equivalent result, follow [RELEASE.md](RELEASE.md).

The complete repository gate remains `bash scripts/ci.sh`; see
[DESKTOP_TESTING.md](DESKTOP_TESTING.md) for its automated and manual coverage boundaries.
