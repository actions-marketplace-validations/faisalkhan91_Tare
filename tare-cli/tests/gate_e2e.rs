//! `tare gate` — CI cost-gate: spend cap + baseline regression check.

use std::path::{Path, PathBuf};
use tare_cli::ShapeGateOpts;
use tare_core::attribute::Report;
use tare_core::model::Provider;
use tare_core::{ingest_step, PricingTable};
use tare_store::Store;

fn fx() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("fixtures")
}
fn read(rel: &str) -> Vec<u8> {
    std::fs::read(fx().join(rel)).unwrap()
}
fn pricing() -> PricingTable {
    PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml")).unwrap()
}

#[test]
fn gate_fails_closed_on_an_empty_store() {
    // A gate WITH assertions against a store that has no captured spend must FAIL,
    // not pass green at $0 (a wrong/missing --db path or un-synced CI checkout is a setup error).
    let db = std::env::temp_dir().join(format!("tare-gate-empty-{}.db", std::process::id()));
    let db = db.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&db);
    let _store = Store::open(&db).unwrap(); // schema only, zero steps
    let p = pricing();
    // A spend cap against an empty store FAILS closed.
    let (passed, summary) = tare_cli::gate(
        &db,
        Some(10_000_000_000),
        &p,
        None,
        false,
        None,
        None,
        &ShapeGateOpts::default(),
    )
    .unwrap();
    assert!(!passed, "empty store must not pass a gate with assertions");
    assert!(
        summary.contains("empty/missing store"),
        "explains why it failed"
    );
    // With NO assertions at all, an empty store is not an error (nothing was asked of it).
    assert!(
        tare_cli::gate(
            &db,
            None,
            &p,
            None,
            false,
            None,
            None,
            &ShapeGateOpts::default()
        )
        .unwrap()
        .0,
        "no assertions → empty store passes (nothing to check)"
    );
    let _ = std::fs::remove_file(&db);
}

#[test]
fn gate_spend_cap_and_regression() {
    let db = std::env::temp_dir().join(format!("tare-gate-{}.db", std::process::id()));
    let db = db.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&db);
    let store = Store::open(&db).unwrap();
    // Some real spend (a 5m cache write + output).
    let s = ingest_step(
        "r",
        1,
        Provider::Anthropic,
        &read("anthropic_stream/request.json"),
        &read("anthropic_stream/response.sse"),
    )
    .unwrap();
    store.record_step(&s, "2026-06-25").unwrap();
    let p = pricing();

    // Generous cap passes; $0 cap fails.
    assert!(
        tare_cli::gate(
            &db,
            Some(10_000_000_000),
            &p,
            None,
            false,
            None,
            None,
            &ShapeGateOpts::default(),
        )
        .unwrap()
        .0
    );
    assert!(
        !tare_cli::gate(
            &db,
            Some(1),
            &p,
            None,
            false,
            None,
            None,
            &ShapeGateOpts::default(),
        )
        .unwrap()
        .0
    );

    // Baseline regression: a cheaper baseline => current is a regression => fail.
    let cheaper = Report {
        pricing_version: "fixture-2026.06".into(),
        effective_date: "2026-06-01".into(),
        estimated: true,
        total_micros: 0,
        rows: vec![],
        unpriced: Vec::new(),
        privacy_policy_id: None,
        profile: None,
        attribution_confidence: None,
    };
    let base = std::env::temp_dir().join(format!("tare-gate-base-{}.json", std::process::id()));
    std::fs::write(&base, serde_json::to_string(&cheaper).unwrap()).unwrap();
    assert!(
        !tare_cli::gate(
            &db,
            None,
            &p,
            Some(&base.to_string_lossy()),
            true,
            None,
            None,
            &ShapeGateOpts::default(),
        )
        .unwrap()
        .0
    );
    // Without --fail-on-regression, the baseline is informational only => pass.
    assert!(
        tare_cli::gate(
            &db,
            None,
            &p,
            Some(&base.to_string_lossy()),
            false,
            None,
            None,
            &ShapeGateOpts::default(),
        )
        .unwrap()
        .0
    );

    // A resolved git baseline participates in the gate decision, not only comment rendering.
    let (passed, summary) = tare_cli::gate_with_baseline_total(
        &db,
        None,
        &p,
        None,
        true,
        None,
        None,
        &ShapeGateOpts::default(),
        Some(0),
    )
    .unwrap();
    assert!(!passed);
    assert!(summary.contains("regression check : FAIL"));

    // Asking for a regression decision without any baseline is a configuration failure.
    let (passed, summary) = tare_cli::gate(
        &db,
        None,
        &p,
        None,
        true,
        None,
        None,
        &ShapeGateOpts::default(),
    )
    .unwrap();
    assert!(!passed);
    assert!(summary.contains("requires --baseline"));

    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(&base);
}

#[test]
fn gate_localizes_the_regression_day_and_driver() {
    // A1: a red gate names the day the regression entered + the driving cause, via bisect over
    // the daily Total series — not just "spend went up".
    let db = std::env::temp_dir().join(format!("tare-loc-{}.db", std::process::id()));
    let db = db.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&db);
    let store = Store::open(&db).unwrap();
    // Four cheap days, then a pricey spike on the fifth.
    for (i, date) in ["2026-06-20", "2026-06-21", "2026-06-22", "2026-06-23"]
        .iter()
        .enumerate()
    {
        let s = ingest_step(
            format!("cheap{i}"),
            1,
            Provider::Openai,
            &read("openai_nonstream/request.json"),
            &read("openai_nonstream/response.json"),
        )
        .unwrap();
        store.record_step(&s, date).unwrap();
    }
    // Bloated anthropic run on the spike day (much larger bill).
    for i in 1..=3u32 {
        let s = ingest_step(
            "spike",
            i,
            Provider::Anthropic,
            &read(&format!("bloated_system_prompt/step{i}.request.json")),
            &read(&format!("bloated_system_prompt/step{i}.response.json")),
        )
        .unwrap();
        store.record_step(&s, "2026-06-24").unwrap();
    }
    drop(store);
    let p = pricing();
    // Baseline = a cheap run; current total (incl. the spike) regresses => fail + localization.
    let (passed, summary) = tare_cli::gate(
        &db,
        None,
        &p,
        None,
        true,
        None,
        Some("cheap0"),
        &ShapeGateOpts::default(),
    )
    .unwrap();
    assert!(!passed);
    assert!(
        summary.contains("regression entered 2026-06-24"),
        "summary should localize the spike day, got:\n{summary}"
    );
    assert!(summary.contains("driven by"), "summary names the driver");
    let _ = std::fs::remove_file(&db);
}

#[test]
fn gate_shape_assertions_pass_and_fail() {
    // A3: payload-free structural assertions over the captured token vectors.
    let db = std::env::temp_dir().join(format!("tare-shape-{}.db", std::process::id()));
    let db = db.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&db);
    let store = Store::open(&db).unwrap();
    // A bloated, uncached system prompt re-sent across three steps (run "cur"); a single-step
    // baseline run "base".
    for i in 1..=3u32 {
        let s = ingest_step(
            "cur",
            i,
            Provider::Anthropic,
            &read(&format!("bloated_system_prompt/step{i}.request.json")),
            &read(&format!("bloated_system_prompt/step{i}.response.json")),
        )
        .unwrap();
        store.record_step(&s, "2026-06-25").unwrap();
    }
    let b = ingest_step(
        "base",
        1,
        Provider::Anthropic,
        &read("bloated_system_prompt/step1.request.json"),
        &read("bloated_system_prompt/step1.response.json"),
    )
    .unwrap();
    store.record_step(&b, "2026-06-20").unwrap();
    drop(store);
    let p = pricing();

    let opts = |f: &dyn Fn(&mut ShapeGateOpts)| {
        let mut o = ShapeGateOpts::default();
        f(&mut o);
        o
    };
    // --max-system-prompt-tokens: a huge cap passes, a 1-token cap fails.
    let (ok, _) = tare_cli::gate(
        &db,
        None,
        &p,
        None,
        false,
        None,
        None,
        &opts(&|o| o.max_system_prompt_tokens = Some(100_000_000)),
    )
    .unwrap();
    assert!(ok, "generous system-prompt cap should pass");
    let (ok, sum) = tare_cli::gate(
        &db,
        None,
        &p,
        None,
        false,
        None,
        None,
        &opts(&|o| o.max_system_prompt_tokens = Some(1)),
    )
    .unwrap();
    assert!(!ok && sum.contains("max-system-prompt-tokens 1"));

    // --require-cache-read-ratio: this run has no cache traffic -> vacuously satisfied.
    let (ok, _) = tare_cli::gate(
        &db,
        None,
        &p,
        None,
        false,
        None,
        None,
        &opts(&|o| o.require_cache_read_ratio_pct = Some(90)),
    )
    .unwrap();
    assert!(ok, "no cache traffic -> ratio vacuously satisfied");

    // --max-component-growth-pct vs the single-step baseline: System tripled -> fail; and
    // without a baseline run it fails closed.
    let (ok, sum) = tare_cli::gate(
        &db,
        None,
        &p,
        None,
        false,
        None,
        Some("base"),
        &opts(&|o| o.max_component_growth_pct = Some(50)),
    )
    .unwrap();
    assert!(!ok && sum.contains("max-component-growth-pct"));
    let (ok, sum) = tare_cli::gate(
        &db,
        None,
        &p,
        None,
        false,
        None,
        None,
        &opts(&|o| o.max_component_growth_pct = Some(50)),
    )
    .unwrap();
    assert!(!ok && sum.contains("requires --baseline-run"));

    let _ = std::fs::remove_file(&db);
}

#[test]
fn gate_no_retry_loops_detects_a_reissue() {
    // A3: an identical request re-issued within a run is a retry loop -> fail.
    let db = std::env::temp_dir().join(format!("tare-retry-{}.db", std::process::id()));
    let db = db.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&db);
    let store = Store::open(&db).unwrap();
    for i in 1..=2u32 {
        let s = ingest_step(
            "loopy",
            i,
            Provider::Openai,
            &read("openai_nonstream/request.json"),
            &read("openai_nonstream/response.json"),
        )
        .unwrap();
        store.record_step(&s, "2026-06-25").unwrap();
    }
    drop(store);
    let p = pricing();
    let o = ShapeGateOpts {
        no_retry_loops: true,
        ..Default::default()
    };
    let (ok, sum) = tare_cli::gate(&db, None, &p, None, false, None, None, &o).unwrap();
    assert!(!ok, "a re-issued identical request is a retry loop");
    assert!(sum.contains("no-retry-loops") && sum.contains("retry loop detected"));
    let _ = std::fs::remove_file(&db);
}

#[test]
fn diff_and_gate_from_stored_runs() {
    // K7a: diff two stored runs and gate against a baseline RUN id (no saved JSON file).
    let db = std::env::temp_dir().join(format!("tare-k7a-{}.db", std::process::id()));
    let db = db.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&db);
    let store = Store::open(&db).unwrap();
    // "cheap": one openai call. "pricey": the cache-write anthropic stream.
    let cheap = ingest_step(
        "cheap",
        1,
        Provider::Openai,
        &read("openai_nonstream/request.json"),
        &read("openai_nonstream/response.json"),
    )
    .unwrap();
    store.record_step(&cheap, "2026-06-25").unwrap();
    let pricey = ingest_step(
        "pricey",
        1,
        Provider::Anthropic,
        &read("anthropic_stream/request.json"),
        &read("anthropic_stream/response.sse"),
    )
    .unwrap();
    store.record_step(&pricey, "2026-06-25").unwrap();
    drop(store);
    let p = pricing();

    // diff_runs(cheap -> pricey) is a positive delta.
    let d = tare_cli::diff_runs(&db, "cheap", "pricey", &p).unwrap();
    assert!(d.delta_micros > 0, "pricey run costs more than cheap");

    // gate --baseline-run cheap, current store total includes pricey => regression => fail.
    let (passed, _) = tare_cli::gate(
        &db,
        None,
        &p,
        None,
        true,
        None,
        Some("cheap"),
        &ShapeGateOpts::default(),
    )
    .unwrap();
    assert!(!passed, "current total regressed vs the cheap baseline run");

    let _ = std::fs::remove_file(&db);
}
