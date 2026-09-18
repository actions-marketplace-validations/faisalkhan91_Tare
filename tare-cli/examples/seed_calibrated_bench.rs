//! Seed the deterministic Calibrated Bench fixture database.
//!
//! Writes the fixture built by [`tare_core::calibrated_bench::build`] into a SQLite store so a
//! reviewer can compare old and new UI behavior against one stable dataset. The generator and its
//! expected totals are committed; the generated `.db` is NOT (it lands in `/tmp`, is reproducible,
//! and is listed in `.gitignore`).
//!
//! Usage (see `fixtures/calibrated_bench/README.md`):
//!
//! ```bash
//! cargo run -p tare-cli --example seed_calibrated_bench --offline -- \
//!   --db /tmp/tare-calibrated-bench.db
//! ```
//!
//! The command is idempotent: it removes any existing file at `--db` first, so re-running always
//! produces an identical database.

use tare_core::calibrated_bench;
use tare_store::Store;

const DEFAULT_DB: &str = "/tmp/tare-calibrated-bench.db";

fn main() {
    if let Err(e) = run() {
        eprintln!("seed_calibrated_bench: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let db = parse_db_arg().unwrap_or_else(|| DEFAULT_DB.to_string());

    // Fresh, deterministic recreation: clear the target (and its WAL/SHM siblings) so the DB is
    // exactly the fixture, never a superset of a previous seed. Missing files are fine.
    for suffix in ["", "-wal", "-shm"] {
        let path = format!("{db}{suffix}");
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("remove {path}: {e}")),
        }
    }

    let bench = calibrated_bench::build();
    let store = Store::open(&db)?;

    for sr in &bench.runs {
        for step in &sr.run.steps {
            store.record_step_with_time(
                step,
                &sr.date,
                sr.hour,
                None,
                Some(sr.profile),
                Some(sr.source),
            )?;
        }
        if let Some(score) = sr.quality {
            store.set_run_quality(
                &sr.run.run_id,
                score,
                "cli",
                calibrated_bench::SEED_UPDATED_AT,
            )?;
        }
        if !sr.tags.is_empty() || !sr.note.is_empty() || sr.starred {
            let tags_json =
                serde_json::to_string(&sr.tags).map_err(|e| format!("tags json: {e}"))?;
            store.upsert_run_note(
                &sr.run.run_id,
                &tags_json,
                sr.note,
                sr.starred,
                calibrated_bench::SEED_UPDATED_AT,
            )?;
        }
    }

    for s in &bench.sessions {
        store.record_session_beat(s.source, s.session, s.last_unix, s.last_model)?;
    }

    let t = &bench.totals;
    println!("Seeded Calibrated Bench fixture → {db}");
    println!(
        "  window        : {} … +{} days",
        calibrated_bench::START_DATE,
        calibrated_bench::DAYS
    );
    println!("  runs / steps  : {} / {}", t.total_runs, t.total_steps);
    println!(
        "  priced/unpriced runs : {} / {}",
        t.priced_runs, t.unpriced_runs
    );
    println!(
        "  providers/models/templates : {} / {} / {}",
        t.distinct_providers, t.distinct_models, t.distinct_templates
    );
    println!(
        "  failures      : {} errors, {} refusals, {} retries",
        t.error_steps, t.refusal_steps, t.retry_steps
    );
    println!("  quality runs  : {}", t.quality_runs);
    println!("  active sessions (tail) : {}", t.sessions);
    println!("  anomaly day   : {}", t.anomaly_date);
    println!(
        "  tokens (counts, pricing-edition-independent): fresh={} cw5m={} cw1h={} cache_read={} output={} reasoning={} audio_in={}",
        t.fresh_input, t.cache_write_5m, t.cache_write_1h, t.cache_read, t.output, t.reasoning, t.audio_input
    );
    println!(
        "  priced spend is an ESTIMATE computed by the app against the effective-dated table."
    );
    Ok(())
}

/// Minimal `--db <path>` parser (the example has no clap dependency).
fn parse_db_arg() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--db" => return args.next(),
            other => {
                if let Some(v) = other.strip_prefix("--db=") {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}
