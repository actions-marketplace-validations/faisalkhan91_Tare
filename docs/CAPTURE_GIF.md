# Reproducing app imagery

Tare keeps deterministic product imagery in the repository and treats native animation as optional
release material.

## Hero flamegraph

The README hero is generated from the offline demo and checked by `scripts/ci.sh`:

```bash
cargo run --offline -q -p tare-cli --bin tare -- \
  demo --svg assets/hero-flamegraph.svg
```

## README screenshots

The committed Pulse, Run Profile, and Optimize screenshots are captured from the built UI against
fixture-backed responses:

```bash
npm --prefix web ci                  # only when dependencies are absent
bash scripts/gen-ui-shots.sh
```

The script runs the `desktop-light` cases in `web/e2e/page-by-page.spec.ts` and writes:

- `assets/screenshots/pulse.png`
- `assets/screenshots/run-profile.png`
- `assets/screenshots/optimize.png`

It needs the Playwright Chromium revision associated with the lockfile to be present in the local
browser cache. See [the E2E runner guide](../web/e2e/README.md). Screenshots are reviewed visually;
raster output is not expected to be byte-identical across operating systems.

## Optional native recording

A GIF or video of the real desktop shell needs a display and is intentionally outside the headless
gate. To record against deterministic data:

```bash
cargo run -p tare-cli --example seed_calibrated_bench --offline -- \
  --db /tmp/tare-calibrated-bench.db
bash scripts/build-ui.sh
TARE_DB=/tmp/tare-calibrated-bench.db \
  cargo run -p tare-tauri --features gui --profile dev-fast
```

Capture one short workflow such as Pulse → Investigate → Run Profile or an Optimize opportunity and
verification. Keep estimates and fixture data visible so the recording does not imply live provider
activity.

Do not add display-dependent recording to `scripts/ci.sh`. Commit a GIF or video only when a
maintained document references it; otherwise keep it outside the repository.
