//! Storage benchmark: does the interactive cohort UI benefit from a materialized
//! dimension index, or is scanning `shape_json` fast enough?
//!
//! It compares two strategies for the same facet questions over a realistic step corpus (the
//! `tare_core::calibrated_bench` fixture replicated to several sizes):
//!
//!   * **scan** — the status quo: read every step and its `shape_json` (via `Store::load_runs`,
//!     which deserializes each shape) and aggregate a dimension in Rust. Every facet re-reads and
//!     re-parses the whole corpus.
//!   * **index** — the `step_dimensions` normalized table + covering index; a facet is
//!     a single grouped/keyed SQL lookup. The one-time backfill cost is measured separately.
//!
//! Ignored (wall-clock, allocates a temp DB) so it never runs in the default gate. Run:
//!
//! ```bash
//! cargo test -p tare-store --release --offline cohort_dimensions_benchmark -- --ignored --nocapture
//! ```
//!
//! It also ASSERTS both strategies return identical facet results, so the comparison is honest and
//! the test fails loudly if the candidate index ever diverges from the scan.

use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tare_core::calibrated_bench;
use tare_core::cohort::{
    CohortDimension, CohortEntity, CohortFilter, CohortMetric, CohortSpec, Normalization,
    PricingMode,
};
use tare_core::model::{CacheTtl, StepRecord};
use tare_core::pricing::PricingTable;
use tare_store::Store;

/// Replication factors over the ~520-step fixture → approximate corpus sizes to benchmark.
const FACTORS: &[usize] = &[1, 20, 100];
/// Timed iterations per measurement (after a warmup).
const ITERS: u32 = 25;
/// The headline dimension to facet on (present on every fixture step).
const DIM: &str = "session";

/// The allow-listed dimensions present on the fixture shapes, materialized into the candidate
/// index. Returns `(dimension, value)` pairs for one step.
fn step_dimensions(step: &StepRecord) -> Vec<(&'static str, String)> {
    let s = &step.shape;
    let mut out: Vec<(&'static str, String)> = Vec::new();
    if let Some(v) = &s.session {
        out.push(("session", v.clone()));
    }
    if let Some(v) = &s.effort {
        out.push(("effort", v.clone()));
    }
    if let Some(v) = &s.mcp_server {
        out.push(("mcp_server", v.clone()));
    }
    if let Some(v) = &s.vendor {
        out.push(("vendor", v.clone()));
    }
    if let Some(v) = &s.commit {
        out.push(("commit", v.clone()));
    }
    if let Some(v) = &s.author {
        out.push(("author", v.clone()));
    }
    if let Some(h) = s.system_hash {
        out.push(("template", format!("{h:016x}")));
    }
    out.push((
        "ttl",
        match s.ttl {
            CacheTtl::FiveMin => "5m".to_string(),
            CacheTtl::OneHour => "1h".to_string(),
        },
    ));
    out
}

/// Median of a set of durations (odd/even both handled; input is copied+sorted).
fn median(mut v: Vec<Duration>) -> Duration {
    v.sort_unstable();
    v[v.len() / 2]
}

/// Time `f` for `ITERS` iterations after one warmup; returns the median iteration duration.
fn bench<T>(mut f: impl FnMut() -> T) -> Duration {
    black_box(f());
    let mut samples = Vec::with_capacity(ITERS as usize);
    for _ in 0..ITERS {
        let t = Instant::now();
        black_box(f());
        samples.push(t.elapsed());
    }
    median(samples)
}

fn temp_db(tag: &str) -> String {
    let p = std::env::temp_dir().join(format!("tare-cohort-bench-{tag}.db"));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", p.display()));
    }
    p.display().to_string()
}

#[test]
#[ignore = "release-mode storage benchmark; run explicitly with --ignored --nocapture"]
fn cohort_dimensions_benchmark() {
    let bench_data = calibrated_bench::build();

    println!("\ncohort_dimensions_benchmark — scan(shape_json) vs materialized step_dimensions");
    println!("dimension = {DIM}, iters = {ITERS}, corpus = calibrated_bench fixture × factor\n");
    println!(
        "{:>6} {:>9} {:>9} {:>13} {:>13} {:>13} {:>13} {:>12}",
        "factor",
        "steps",
        "dim_rows",
        "scan_facet",
        "idx_facet",
        "scan_point",
        "idx_point",
        "backfill"
    );
    println!("{}", "-".repeat(96));

    for &factor in FACTORS {
        // --- build the replicated corpus in memory ---
        let mut rows: Vec<(StepRecord, String, Option<u8>)> = Vec::new();
        for r in 0..factor {
            for sr in &bench_data.runs {
                for step in &sr.run.steps {
                    let mut s = step.clone();
                    s.run_id = format!("{}#{r}", s.run_id);
                    rows.push((s, sr.date.clone(), sr.hour));
                }
            }
        }
        let total_steps = rows.len();
        // A representative point-lookup target: the first step's session value.
        let target = rows
            .iter()
            .find_map(|(s, _, _)| s.shape.session.clone())
            .expect("fixture has session values");

        // --- seed the scan corpus (the real steps table with shape_json) ---
        let scan_db = temp_db(&format!("scan-{factor}"));
        let store = Store::open(&scan_db).expect("open scan store");
        store
            .record_transcript_steps(&rows, &[], Some("bench"), Some("bench"))
            .expect("seed scan corpus");

        // --- build the candidate step_dimensions index in a separate DB; measure backfill ---
        let idx_db = temp_db(&format!("idx-{factor}"));
        let conn = Connection::open(&idx_db).expect("open index db");
        conn.execute_batch(
            "CREATE TABLE step_dimensions (
               run_id TEXT NOT NULL,
               step_ordinal INTEGER NOT NULL,
               dimension TEXT NOT NULL,
               value TEXT NOT NULL,
               PRIMARY KEY (run_id, step_ordinal, dimension, value)
             );
             CREATE INDEX step_dimensions_lookup
               ON step_dimensions (dimension, value, run_id, step_ordinal);",
        )
        .expect("create step_dimensions");

        let backfill_start = Instant::now();
        conn.execute_batch("BEGIN").unwrap();
        {
            let mut stmt = conn
                .prepare("INSERT OR IGNORE INTO step_dimensions VALUES (?1,?2,?3,?4)")
                .unwrap();
            for (step, _, _) in &rows {
                for (dim, val) in step_dimensions(step) {
                    stmt.execute(rusqlite::params![step.run_id, step.step_ordinal, dim, val])
                        .unwrap();
                }
            }
        }
        conn.execute_batch("COMMIT").unwrap();
        let backfill = backfill_start.elapsed();
        let dim_rows: u64 = conn
            .query_row("SELECT COUNT(*) FROM step_dimensions", [], |r| r.get(0))
            .unwrap();

        // --- SCAN facet: reload + reparse every shape, group by session ---
        let scan_facet_counts = || {
            let runs = store.load_runs().expect("load_runs");
            let mut counts: BTreeMap<String, u64> = BTreeMap::new();
            for run in &runs {
                for s in &run.steps {
                    if let Some(v) = &s.shape.session {
                        *counts.entry(v.clone()).or_default() += 1;
                    }
                }
            }
            counts
        };
        let scan_facet = bench(scan_facet_counts);

        // --- INDEX facet: one grouped SQL lookup ---
        let index_facet_counts = || {
            let mut stmt = conn
                .prepare(
                    "SELECT value, COUNT(*) FROM step_dimensions WHERE dimension=?1 GROUP BY value",
                )
                .unwrap();
            let mut counts: BTreeMap<String, u64> = BTreeMap::new();
            let mapped = stmt
                .query_map(rusqlite::params![DIM], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
                })
                .unwrap();
            for row in mapped {
                let (v, c) = row.unwrap();
                counts.insert(v, c);
            }
            counts
        };
        let idx_facet = bench(index_facet_counts);

        // Correctness: both strategies must agree on the facet.
        assert_eq!(
            scan_facet_counts(),
            index_facet_counts(),
            "scan and index disagree on the {DIM} facet at factor {factor}"
        );

        // --- point lookup: resolve the candidate steps for one dimension value ---
        let scan_point = bench(|| {
            let runs = store.load_runs().expect("load_runs");
            let mut n = 0u64;
            for run in &runs {
                for s in &run.steps {
                    if s.shape.session.as_deref() == Some(target.as_str()) {
                        n += 1;
                    }
                }
            }
            n
        });
        let idx_point = bench(|| {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM step_dimensions WHERE dimension=?1 AND value=?2",
                    rusqlite::params![DIM, target],
                    |r| r.get(0),
                )
                .unwrap();
            n as u64
        });

        println!(
            "{:>6} {:>9} {:>9} {:>13} {:>13} {:>13} {:>13} {:>12}",
            factor,
            total_steps,
            dim_rows,
            fmt(scan_facet),
            fmt(idx_facet),
            fmt(scan_point),
            fmt(idx_point),
            fmt(backfill),
        );

        // Cleanup temp DBs.
        drop(store);
        drop(conn);
        for db in [&scan_db, &idx_db] {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{db}{suffix}"));
            }
        }
    }
    println!(
        "\nlegend: *_facet = group spend/count by {DIM}; *_point = resolve one {DIM} value; \
         times are median of {ITERS} iters. backfill = one-time materialization of all dims.\n"
    );
}

/// The shipped resolve path (`Store::resolve_cohort`, which narrows to the
/// indexed candidate run set via `step_dimensions`/`steps` before pricing) must resolve an
/// interactive facet SELECTION under the 100ms budget at the benchmarked corpus sizes. Unlike
/// `cohort_dimensions_benchmark` (which times the raw scan-vs-index STRATEGY), this times the real
/// end-to-end `resolve_cohort` so the acceptance is measured on the shipped code. Ignored / release.
#[test]
#[ignore = "release-mode resolve-latency benchmark; run explicitly with --ignored --nocapture"]
fn resolve_latency_meets_interactive_budget() {
    let bench_data = calibrated_bench::build();
    let pricing = PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml"))
        .expect("fixture pricing");
    const BUDGET: Duration = Duration::from_millis(100);

    // Pick the RAREST and MOST-COMMON session across base runs. The rare one models an interactive
    // facet DRILL (a selected value narrows to a small candidate set and should stay within the
    // 100ms interaction budget); the common one is a broad
    // full-corpus aggregation that necessarily prices most steps and scales with matched volume, so
    // it is REPORTED as a stress reference, not asserted against the interactive budget.
    let mut freq: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for sr in &bench_data.runs {
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for step in &sr.run.steps {
            if let Some(s) = &step.shape.session {
                seen.insert(s.as_str());
            }
        }
        for s in seen {
            *freq.entry(s.to_string()).or_default() += 1;
        }
    }
    let drill = freq
        .iter()
        .min_by_key(|(_, &c)| c)
        .map(|(s, _)| s.clone())
        .expect("fixture has session values");
    let broad = freq
        .iter()
        .max_by_key(|(_, &c)| c)
        .map(|(s, _)| s.clone())
        .expect("fixture has session values");

    println!("\nresolve_latency_meets_interactive_budget — Store::resolve_cohort (indexed) vs 100ms budget");
    println!(
        "dimension = {DIM}, iters = {ITERS}, corpus = calibrated_bench × factor; drill='{drill}' broad='{broad}'\n"
    );
    println!(
        "{:>6} {:>9} {:>13} {:>13} {:>10}",
        "factor", "steps", "facet_drill", "broad_scan", "budget"
    );
    println!("{}", "-".repeat(58));

    let spec_for = |value: &str| CohortSpec {
        from: None,
        to: None,
        timezone: "UTC".into(),
        entity: CohortEntity::Run,
        filters: vec![CohortFilter::Eq {
            dimension: CohortDimension::Session,
            value: value.to_string(),
        }],
        pricing: PricingMode::Latest,
        metric: CohortMetric::SpendMicros,
        normalization: Normalization::Absolute,
        outcome_denominator: None,
    };

    for &factor in FACTORS {
        let mut rows: Vec<(StepRecord, String, Option<u8>)> = Vec::new();
        for r in 0..factor {
            for sr in &bench_data.runs {
                for step in &sr.run.steps {
                    let mut s = step.clone();
                    s.run_id = format!("{}#{r}", s.run_id);
                    rows.push((s, sr.date.clone(), sr.hour));
                }
            }
        }
        let total_steps = rows.len();

        let db = temp_db(&format!("resolve-{factor}"));
        let store = Store::open(&db).expect("open resolve store");
        store
            .record_transcript_steps(&rows, &[], Some("bench"), Some("bench"))
            .expect("seed corpus");

        let drill_spec = spec_for(&drill);
        let broad_spec = spec_for(&broad);
        let drill_t = bench(|| {
            store
                .resolve_cohort(&drill_spec, &pricing)
                .expect("resolve")
        });
        let broad_t = bench(|| {
            store
                .resolve_cohort(&broad_spec, &pricing)
                .expect("resolve")
        });

        println!(
            "{:>6} {:>9} {:>13} {:>13} {:>10}",
            factor,
            total_steps,
            fmt(drill_t),
            fmt(broad_t),
            fmt(BUDGET)
        );
        // The interactive facet drill MUST meet the budget at every benchmarked size; the broad
        // full-corpus aggregation is reported only (it is not an interactive navigation/preview).
        assert!(
            drill_t < BUDGET,
            "indexed facet drill at factor {factor} ({total_steps} steps) took {} (>= 100ms budget)",
            fmt(drill_t)
        );

        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{db}{suffix}"));
        }
    }
    println!();
}

/// Human-friendly duration: µs under 1ms, else ms with two decimals.
fn fmt(d: Duration) -> String {
    let us = d.as_secs_f64() * 1_000_000.0;
    if us < 1000.0 {
        format!("{us:.1}µs")
    } else {
        format!("{:.2}ms", us / 1000.0)
    }
}
