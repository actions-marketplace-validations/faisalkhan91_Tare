# Contributing to Tare

Thanks for your interest in Tare. It is a local-first cost profiler for AI agents. Contributions,
bug reports, and ideas are welcome.

## Ground rules

- Be kind and constructive; see [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).
- For security issues, **do not** open a public issue — follow [`SECURITY.md`](SECURITY.md).
- Open an issue to discuss non-trivial changes before sending a large PR.

## Workspace layout

Tare is a Rust workspace plus a TypeScript/HTML frontend:

- `tare-core` — parsing, cost/attribution, flamegraph + SVG rendering (pure, deterministic).
- `tare-store` — local SQLite store and migrations.
- `tare-cli` — CLI, local read API, OTLP receiver; embeds the built UI.
- `tare-proxy`, `tare-mcp`, `tare-daemon` — optional capture/integration surfaces.
- `tare-tauri` — desktop shell (embeds the same built UI).
- `web/` — the frontend **source of truth** (TypeScript).

## The UI source-of-truth rule (important)

`web/src/**` and `web/index*.html` are the **only** place you edit UI code.
`tare-cli/assets/ui/**` and `tare-tauri/dist/**` are **generated build outputs** — never edit them
by hand. After changing anything under `web/`, regenerate the embeds:

```sh
bash scripts/build-ui.sh        # tsc + copy web/dist into both embed targets
bash scripts/check-ui-sync.sh   # byte-parity check; CI enforces this
```

Hand-editing an embed file will be caught by the sync gate and fail CI.

## Building and testing

Rust type-check (fast, safe on any machine):

```sh
cargo check --workspace
```

Frontend tests (run from the `web/` directory):

```sh
cd web && npx vitest run
```

Full offline gate (builds UI, checks sync, runs the Rust + web suites):

```sh
bash scripts/ci.sh
```

The gate is designed to run without external access after Cargo, npm, and Playwright dependencies
are cached. Tests use no accounts or provider calls. New features should degrade gracefully when
offline, and tests must not reach the network.

## Pull requests

- Keep PRs focused; separate mechanical/refactor changes from behavior changes.
- Run `cargo check --workspace` and the web tests before pushing.
- If you touched the UI, confirm `bash scripts/build-ui.sh && bash scripts/check-ui-sync.sh` is
  clean and commit the regenerated embeds.
- Describe what you changed and how you verified it.

## License

By contributing, you agree that your contributions are licensed under the MIT License
(see [`LICENSE`](LICENSE)).
