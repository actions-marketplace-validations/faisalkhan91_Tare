//! Snapshot + integration tests over the committed fixtures.
//! Golden files live in `tests/golden/`. Regenerate with `UPDATE_SNAPSHOTS=1 cargo test`.

use std::fs;
use std::path::{Path, PathBuf};
use tare_core::flamegraph::build_flamegraph;
use tare_core::model::{Provider, UsageTokens};
use tare_core::{attribute, build_runs, ingest_step, speedscope, svg, PricingTable};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("fixtures")
}

fn read(rel: &str) -> Vec<u8> {
    fs::read(fixtures_dir().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn fixture_pricing() -> PricingTable {
    let s = include_str!("../../pricing/pricing.fixture.toml");
    PricingTable::from_toml_str(s).unwrap()
}

fn check_snapshot(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(name);
    if std::env::var("UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing golden {name}; run UPDATE_SNAPSHOTS=1 cargo test"));
    assert_eq!(actual, expected, "snapshot mismatch for {name}");
}

/// One captured step: (provider, request_rel, response_rel).
type Item = (Provider, &'static str, &'static str);
/// A scenario: (run_id, ordered steps).
type Scenario = (&'static str, Vec<Item>);

/// All scenarios as (run_id, [(provider, request_rel, response_rel)]).
fn scenarios() -> Vec<Scenario> {
    vec![
        (
            "anthropic_nonstream",
            vec![(
                Provider::Anthropic,
                "anthropic_nonstream/request.json",
                "anthropic_nonstream/response.json",
            )],
        ),
        (
            "anthropic_stream",
            vec![(
                Provider::Anthropic,
                "anthropic_stream/request.json",
                "anthropic_stream/response.sse",
            )],
        ),
        (
            "anthropic_thinking",
            vec![(
                Provider::Anthropic,
                "anthropic_thinking/request.json",
                "anthropic_thinking/response.json",
            )],
        ),
        (
            "cache_two_turn",
            vec![
                (
                    Provider::Anthropic,
                    "anthropic_cache_two_turn/turn1.request.json",
                    "anthropic_cache_two_turn/turn1.response.sse",
                ),
                (
                    Provider::Anthropic,
                    "anthropic_cache_two_turn/turn2.request.json",
                    "anthropic_cache_two_turn/turn2.response.sse",
                ),
            ],
        ),
        (
            "openai_nonstream",
            vec![(
                Provider::Openai,
                "openai_nonstream/request.json",
                "openai_nonstream/response.json",
            )],
        ),
        (
            "openai_stream_usage",
            vec![(
                Provider::Openai,
                "openai_stream_usage/request.json",
                "openai_stream_usage/response.sse",
            )],
        ),
        (
            "openai_stream_no_usage",
            vec![(
                Provider::Openai,
                "openai_stream_no_usage/request.json",
                "openai_stream_no_usage/response.sse",
            )],
        ),
        (
            "retry_loop_3x",
            vec![
                (
                    Provider::Openai,
                    "retry_loop_3x/attempt1.request.json",
                    "retry_loop_3x/attempt1.response.json",
                ),
                (
                    Provider::Openai,
                    "retry_loop_3x/attempt2.request.json",
                    "retry_loop_3x/attempt2.response.json",
                ),
                (
                    Provider::Openai,
                    "retry_loop_3x/attempt3.request.json",
                    "retry_loop_3x/attempt3.response.json",
                ),
            ],
        ),
        (
            "bloated_system_prompt",
            vec![
                (
                    Provider::Anthropic,
                    "bloated_system_prompt/step1.request.json",
                    "bloated_system_prompt/step1.response.json",
                ),
                (
                    Provider::Anthropic,
                    "bloated_system_prompt/step2.request.json",
                    "bloated_system_prompt/step2.response.json",
                ),
                (
                    Provider::Anthropic,
                    "bloated_system_prompt/step3.request.json",
                    "bloated_system_prompt/step3.response.json",
                ),
            ],
        ),
        (
            "verbose_tool_output",
            vec![(
                Provider::Anthropic,
                "verbose_tool_output/request.json",
                "verbose_tool_output/response.json",
            )],
        ),
    ]
}

fn all_runs() -> Vec<tare_core::RunRecord> {
    let mut steps = Vec::new();
    for (run_id, items) in scenarios() {
        for (ord, (provider, req, resp)) in items.into_iter().enumerate() {
            let s = ingest_step(run_id, ord as u32 + 1, provider, &read(req), &read(resp))
                .unwrap_or_else(|e| panic!("ingest {run_id} step {ord}: {e}"));
            steps.push(s);
        }
    }
    build_runs(steps)
}

#[test]
fn every_accounting_axis_is_nonzero() {
    let runs = all_runs();
    let mut agg = UsageTokens::default();
    for r in &runs {
        for s in &r.steps {
            agg.fresh_input += s.usage.fresh_input;
            agg.cache_write_5m += s.usage.cache_write_5m;
            agg.cache_write_1h += s.usage.cache_write_1h;
            agg.cache_read += s.usage.cache_read;
            agg.output += s.usage.output;
            agg.reasoning += s.usage.reasoning;
        }
    }
    assert!(agg.fresh_input > 0, "fresh");
    assert!(agg.cache_write() > 0, "cache_write");
    assert!(agg.cache_read > 0, "cache_read");
    assert!(agg.output > 0, "output");
    assert!(agg.reasoning > 0, "reasoning");
}

#[test]
fn flamegraph_model_and_svg_are_byte_stable() {
    let runs = all_runs();
    let pricing = fixture_pricing();
    let bloat = runs
        .iter()
        .find(|r| r.run_id == "bloated_system_prompt")
        .unwrap();

    let model = build_flamegraph(bloat, &pricing);
    // Determinism: building twice yields identical bytes.
    let model2 = build_flamegraph(bloat, &pricing);
    let json1 = serde_json::to_string_pretty(&model).unwrap();
    let json2 = serde_json::to_string_pretty(&model2).unwrap();
    assert_eq!(json1, json2);

    let svg1 = svg::render_svg(&model);
    let svg2 = svg::render_svg(&model2);
    assert_eq!(svg1, svg2);

    check_snapshot("flamegraph_bloated.json", &json1);
    check_snapshot("flamegraph_bloated.svg", &svg1);
}

#[test]
fn unpriced_models_are_surfaced_not_silently_zero() {
    // A model the table can't price contributes 0 to the total but MUST appear in `unpriced`
    // Otherwise a new or renamed model could make spend look like approximately $0.
    let pricing = fixture_pricing();
    let req = br#"{"model":"claude-future-99","messages":[]}"#;
    let resp = br#"{"usage":{"input_tokens":1000,"output_tokens":50},"stop_reason":"end_turn"}"#;
    let step = ingest_step("r", 1, Provider::Anthropic, req, resp).unwrap();
    let runs = build_runs(vec![step]);
    let report = attribute::build_report(&runs, &pricing);
    assert_eq!(
        report.total_micros, 0,
        "unpriced tokens excluded from total"
    );
    assert_eq!(report.unpriced.len(), 1);
    assert_eq!(report.unpriced[0].model, "claude-future-99");
    assert_eq!(report.unpriced[0].token_total, 1050);
    assert_eq!(report.unpriced[0].step_count, 1);
    // A fully-priced report leaves `unpriced` empty (so JSON/goldens are unchanged).
    assert!(attribute::build_report(&all_runs(), &pricing)
        .unpriced
        .is_empty());
}

#[test]
fn coarse_attribution_for_otel_degraded_capture_else_exact() {
    // an out-of-band OTel capture carries no prompt-component weights/system-hash, so
    // the report is flagged `coarse` and gets a model-level coarse-attribution row instead of a
    // misleadingly-empty "nothing to optimize".
    use tare_core::otel::{ingest_otlp_json, otel_steps_to_records};
    use tare_core::privacy::PrivacyPolicy;
    use tare_core::RunRecord;
    let fixture = read("otel/genai_trace.otlp.json");
    let recs = otel_steps_to_records(
        &ingest_otlp_json(&fixture).unwrap(),
        &PrivacyPolicy::default(),
    );
    let run = RunRecord {
        run_id: recs[0].run_id.clone(),
        steps: recs,
    };
    let coarse = attribute::build_report(&[run], &fixture_pricing());
    assert_eq!(coarse.attribution_confidence.as_deref(), Some("coarse"));
    let row = coarse.rows.iter().find(|r| r.cause == "coarse-attribution");
    assert!(
        row.is_some(),
        "degraded capture gets a coarse-attribution row"
    );
    assert_eq!(
        row.unwrap().projected_saved_micros,
        0,
        "coarse row is informational, not savings"
    );

    // An exact (proxy) capture with system_hash/weights stays exact — flag omitted (goldens stable).
    assert_eq!(
        attribute::build_report(&all_runs(), &fixture_pricing()).attribution_confidence,
        None
    );
}

#[test]
fn report_matches_golden() {
    let runs = all_runs();
    let pricing = fixture_pricing();
    let report = attribute::build_report(&runs, &pricing);
    assert!(report.estimated);
    assert_eq!(report.pricing_version, "fixture-2026.06");

    // Sanity: retry-loop must be present (3 identical openai requests => 2 redundant).
    let retry = report
        .rows
        .iter()
        .find(|r| r.cause == "retry-loop")
        .expect("retry-loop row");
    assert!(retry.projected_saved_micros > 0);
    // Bloated system present and saves the in/read-rate delta.
    assert!(report
        .rows
        .iter()
        .any(|r| r.cause == "bloated-system-prompt"));
    // Verbose tool output present.
    assert!(report.rows.iter().any(|r| r.cause == "verbose-tool-output"));
    // Disjoint accounting: causes never sum to more than the total spend.
    let row_sum: i64 = report.rows.iter().map(|r| r.micros).sum();
    assert!(
        row_sum <= report.total_micros,
        "Σ rows.micros {row_sum} must not exceed total {}",
        report.total_micros
    );

    let json = serde_json::to_string_pretty(&report).unwrap();
    check_snapshot("report.json", &json);
}

#[test]
fn trend_model_and_svg_are_byte_stable() {
    use tare_core::trend::{trend, DatedRun, TrendDimension};
    let runs = all_runs();
    let pricing = fixture_pricing();
    let pick = |id: &str| runs.iter().find(|r| r.run_id == id).unwrap().clone();
    // Mixed-provider, multi-day window with a gap day (2026-06-21).
    let dated = vec![
        DatedRun {
            date: "2026-06-20".into(),
            run: pick("openai_nonstream"),
        },
        DatedRun {
            date: "2026-06-22".into(),
            run: pick("bloated_system_prompt"),
        },
        DatedRun {
            date: "2026-06-22".into(),
            run: pick("retry_loop_3x"),
        },
    ];
    let report = trend(
        &dated,
        "2026-06-20",
        "2026-06-23",
        &pricing,
        TrendDimension::ByProvider,
    );
    // Determinism: building twice yields identical bytes.
    let report2 = trend(
        &dated,
        "2026-06-20",
        "2026-06-23",
        &pricing,
        TrendDimension::ByProvider,
    );
    let json1 = serde_json::to_string_pretty(&report).unwrap();
    assert_eq!(json1, serde_json::to_string_pretty(&report2).unwrap());
    let svg1 = svg::render_trend_svg(&report);
    assert_eq!(svg1, svg::render_trend_svg(&report2));

    // Per-day Σ series ≤ window total per day is implied; here just pin the goldens.
    check_snapshot("trend.json", &json1);
    check_snapshot("trend.svg", &svg1);
}

#[test]
fn otel_ingest_export_round_trip_golden() {
    use tare_core::otel::{
        export_otlp_json, ingest_otlp_json, otel_steps_to_records, validate_otlp_structure,
    };
    use tare_core::privacy::PrivacyPolicy;
    use tare_core::RunRecord;
    let fixture = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("fixtures/otel/genai_trace.otlp.json"),
    )
    .unwrap();
    let steps = ingest_otlp_json(&fixture).unwrap();
    assert_eq!(steps.len(), 2);
    let recs = otel_steps_to_records(&steps, &PrivacyPolicy::default());
    let run = RunRecord {
        run_id: recs[0].run_id.clone(),
        steps: recs,
    };
    let exported = export_otlp_json(&run, &fixture_pricing(), "test-version");
    validate_otlp_structure(&exported).unwrap();
    let json = serde_json::to_string_pretty(&exported).unwrap();
    check_snapshot("run_export.otlp.json", &json);
}

#[test]
fn codex_sse_event_golden_steprecord_math() {
    // a committed Codex OTLP/JSON logs fixture pins the StepRecord token math so a
    // parser change can't silently re-attribute Codex spend. Two response.completed events (one
    // with cache+reasoning, one without) and a response.created that MUST be filtered out.
    use tare_core::otel::ingest_otlp_logs_json;
    use tare_core::privacy::PrivacyPolicy;
    let bytes = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("fixtures/otel/codex_response_completed.otlp.json"),
    )
    .unwrap();
    let recs = ingest_otlp_logs_json(&bytes, &PrivacyPolicy::default()).unwrap();
    assert_eq!(
        recs.len(),
        2,
        "only response.completed events yield cost rows"
    );

    // Step 1: cached read + reasoning. fresh = input - cached; Codex cache is read-only.
    let s1 = &recs[0];
    assert_eq!(s1.run_id, "conv-golden", "grouped by conversation id");
    assert_eq!(s1.provider, Provider::Openai);
    assert_eq!(s1.model, "gpt-5-mini");
    assert_eq!(s1.step_ordinal, 1);
    assert_eq!(s1.usage.fresh_input, 3400, "5200 input - 1800 cached");
    assert_eq!(s1.usage.cache_read, 1800);
    assert_eq!(
        s1.usage.cache_write(),
        0,
        "Responses API cache is read-only"
    );
    assert_eq!(s1.usage.output, 640);
    assert_eq!(s1.usage.reasoning, 410);
    assert!(
        s1.usage.reasoning <= s1.usage.output,
        "reasoning is a subset of output tokens"
    );

    // Step 2: no cache, no reasoning -> fresh = full input, cache_read/reasoning zero.
    let s2 = &recs[1];
    assert_eq!(s2.step_ordinal, 2);
    assert_eq!(s2.usage.fresh_input, 900);
    assert_eq!(s2.usage.cache_read, 0);
    assert_eq!(s2.usage.reasoning, 0);
    assert_eq!(s2.usage.output, 120);
}

#[test]
fn trend_svg_nonascii_keys_byte_stable() {
    // Locks the legend-advance Unicode-scalar fix: a multibyte series key must render
    // identically in Rust and, through the cross-implementation web test, TypeScript.
    use tare_core::trend::{TrendReport, TrendSeries};
    let report = TrendReport {
        dimension: "by_model".into(),
        from: "2026-06-20".into(),
        to: "2026-06-21".into(),
        days: vec!["2026-06-20".into(), "2026-06-21".into()],
        series: vec![
            TrendSeries {
                key: "café-modèle".into(),
                per_day: vec![1500, 0],
                total_micros: 1500,
            },
            TrendSeries {
                key: "日本語モデル".into(),
                per_day: vec![0, 900],
                total_micros: 900,
            },
        ],
        pricing_version: "fixture-2026.06".into(),
        estimated: true,
    };
    let svg = svg::render_trend_svg(&report);
    assert_eq!(svg, svg::render_trend_svg(&report)); // deterministic
    check_snapshot(
        "trend_nonascii.json",
        &serde_json::to_string_pretty(&report).unwrap(),
    );
    check_snapshot("trend_nonascii.svg", &svg);
}

#[test]
fn speedscope_export_validates() {
    let runs = all_runs();
    let pricing = fixture_pricing();
    let bloat = runs
        .iter()
        .find(|r| r.run_id == "bloated_system_prompt")
        .unwrap();
    let model = build_flamegraph(bloat, &pricing);
    let exported = speedscope::export(&model, "0.1.0");
    speedscope::validate_structure(&exported).expect("valid speedscope");
    let json = serde_json::to_string_pretty(&exported).unwrap();
    check_snapshot("speedscope_bloated.json", &json);
}
