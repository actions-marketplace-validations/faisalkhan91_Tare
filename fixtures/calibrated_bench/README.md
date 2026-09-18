# Calibrated Bench fixture

A deterministic, multi-week dataset for developing and reviewing the **Calibrated Bench**
workbench. It lets a reviewer exercise the current behavior against one stable database instead of
live capture.

- **Generator:** `tare-cli/examples/seed_calibrated_bench.rs` (registered `[[example]]`).
- **Builder (pure, testable):** `tare_core::calibrated_bench` — store-free, clock-free, RNG-free.
- **The generated `.db` is NOT committed.** It is reproducible from the command below and lands in
  `/tmp` by default.

## Recreate the database

```bash
cargo run -p tare-cli --example seed_calibrated_bench --offline -- \
  --db /tmp/tare-calibrated-bench.db
```

The command is idempotent: it removes any existing file at `--db` (and its `-wal`/`-shm` siblings)
first, so re-running always produces a byte-for-byte identical database. Point another app or the
viewer at the resulting file.

## What it covers

- **32 days**, `2026-05-01` … `2026-06-01`, with a quieter weekend rhythm.
- **5 providers** (`anthropic`, `openai`, `gemini`, `openai_compatible`/`groq`, `local`) and
  **8 models**.
- **4 prompt templates** (distinct `system_hash` values) and **6 sessions**.
- **Priced and unpriced usage:** `local` runs are unpriced by the bundled table and surface as
  token usage, never `$0` (honesty invariant).
- **All six cache classes:** fresh, 5m cache write, 1h cache write, cache read, output, reasoning —
  plus multimodal audio input.
- **Retry loops** (a `provider_error` followed by a same-`request_hash`, `attempt=2` re-issue) and
  **refusals**.
- **User quality scores** on a subset of runs (0–100), plus tags/notes and a few starred runs.
- **A cost anomaly** on `2026-05-20`: volume and spend spike far above the surrounding baseline,
  concentrated on the flagship model.
- **An active-session tail:** recent `session_activity` beats so Pulse shows live work.

## Expected totals (stable golden)

These are asserted in `tare-core` (`calibrated_bench::tests::totals_match_recorded_golden`) and in
the store round-trip test (`tare-store/tests/calibrated_bench_roundtrip.rs`). Token counts are
**pricing-edition-independent**, so they remain stable as `pricing/pricing.json` updates. Priced
spend is deliberately not asserted here: it is an estimate the app computes against the
effective-dated pricing table. If the generator changes intentionally, update these numbers, the
Rust golden literals, and this table in the same change.

| Metric | Value |
| --- | ---: |
| Distinct dates | 32 |
| Total runs | 281 |
| Total steps | 520 |
| Priced runs | 246 |
| Unpriced runs (`local`) | 35 |
| Error steps (`provider_error`) | 13 |
| Refusal steps | 13 |
| Retry steps (`attempt > 1`) | 13 |
| Runs with a quality score | 94 |
| Active sessions (tail) | 3 |
| Distinct providers | 5 |
| Distinct models | 8 |
| Distinct templates | 4 |
| Anomaly date | 2026-05-20 |

Token totals (counts):

| Token class | Count |
| --- | ---: |
| Fresh input | 2,967,548 |
| Cache write 5m | 1,395,405 |
| Cache write 1h | 1,017,300 |
| Cache read | 15,174,540 |
| Output | 1,047,211 |
| Reasoning | 134,592 |
| Audio input | 46,005 |
| Audio output | 0 |
