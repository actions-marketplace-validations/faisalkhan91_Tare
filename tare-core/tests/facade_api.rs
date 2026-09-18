//! Facade-API stability golden. Locks the embeddable `tare-core` surface — and only that
//! surface — so an internal refactor can't silently change the public accounting contract, while
//! leaving the rest of the crate's pub items free to evolve.
//!
//! Two layers:
//!  1. Type-checked bindings: each facade symbol is bound to an explicit type. A signature change
//!     fails to compile.
//!  2. A reviewable signature snapshot (`golden/facade_api.txt`). Regenerate intentionally with
//!     `UPDATE_SNAPSHOTS=1 cargo test -p tare-core --test facade_api` and eyeball the diff.

use std::path::Path;
use tare_core::account::CostBreakdown;
use tare_core::{
    account_from_usage, MicroUsd, ModelRates, PricingTable, Provider, RunRecord, UsageTokens,
};

/// The allowlisted facade signatures. Hand-maintained; the bindings below prove each is truthful.
const FACADE_SIGNATURES: &[&str] = &[
    "fn account_from_usage(Provider, &str, &UsageTokens, &PricingTable) -> Option<CostBreakdown>",
    "struct CostBreakdown { fresh: MicroUsd, cache_write: MicroUsd, cache_read: MicroUsd, output: MicroUsd, total: MicroUsd }",
    "fn PricingTable::from_json_str(&str) -> Result<PricingTable, String>",
    "fn MicroUsd::to_dollar_string(&self) -> String",
];

#[test]
fn facade_signatures_are_type_locked() {
    // If any of these signatures change, this test stops compiling — that's the gate.
    let _f: fn(Provider, &str, &UsageTokens, &PricingTable) -> Option<CostBreakdown> =
        account_from_usage;
    let _p: fn(&str) -> Result<PricingTable, String> = PricingTable::from_json_str;
    // Field access locks the struct shape.
    let c = CostBreakdown {
        fresh: MicroUsd(0),
        cache_write: MicroUsd(0),
        cache_read: MicroUsd(0),
        output: MicroUsd(0),
        total: MicroUsd(1),
    };
    let _t: MicroUsd = c.total;
    let _s: String = c.total.to_dollar_string();
}

#[test]
fn facade_signature_snapshot_matches_golden() {
    let golden = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join("facade_api.txt");
    let rendered = format!("{}\n", FACADE_SIGNATURES.join("\n"));
    if std::env::var("UPDATE_SNAPSHOTS").is_ok() {
        std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
        std::fs::write(&golden, &rendered).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&golden)
        .expect("facade_api.txt golden missing — regenerate with UPDATE_SNAPSHOTS=1");
    assert_eq!(
        rendered, want,
        "facade API changed — review and regenerate the golden"
    );
}

#[test]
fn facade_equals_the_full_pipeline_for_a_known_vector() {
    // The example/embedder path must agree with the proxy/report path to the micro-USD.
    let pricing =
        PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml")).unwrap();
    let usage = UsageTokens {
        fresh_input: 1_234,
        cache_write_5m: 0,
        cache_write_1h: 0,
        cache_read: 0,
        output: 567,
        reasoning: 0,
        audio_input: 0,
        audio_output: 0,
    };
    let facade = account_from_usage(Provider::Anthropic, "claude-opus-4-8", &usage, &pricing)
        .expect("priced");

    // Build the same single-step run the proxy would persist and report on.
    let shape = tare_core::wire::anthropic_request_shape(
        br#"{"model":"claude-opus-4-8","messages":[]}"#,
        &tare_core::PrivacyPolicy::default(),
    )
    .unwrap();
    let run = RunRecord {
        run_id: "f".into(),
        steps: vec![tare_core::StepRecord {
            run_id: "f".into(),
            step_ordinal: 1,
            provider: Provider::Anthropic,
            model: "claude-opus-4-8".into(),
            usage,
            shape,
            stop_reason: Some("end_turn".into()),
            duration_ms: 0,
            start_unix_nano: None,
            trace_id: None,
            span_id: None,
            parent_span_id: None,
        }],
    };
    let report = tare_core::attribute::build_report(std::slice::from_ref(&run), &pricing);
    assert_eq!(facade.total.micros(), report.total_micros);

    // Sanity: the bound ModelRates type is the real one (keeps the import load-bearing).
    let _rates: Option<&ModelRates> = pricing.lookup(Provider::Anthropic, None, "claude-opus-4-8");
}
