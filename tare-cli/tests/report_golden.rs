//! `tare report` over a store loaded from fixtures: text + --json + --today,
//! with the ranked trim-list order pinned (computed from pricing.fixture.toml).

use std::path::{Path, PathBuf};
use tare_core::model::Provider;
use tare_core::{attribute, ingest_step, PricingTable};
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

fn load_store(db: &str, date: &str) {
    let steps: &[(&str, u32, Provider, &str, &str)] = &[
        (
            "retry",
            1,
            Provider::Openai,
            "retry_loop_3x/attempt1.request.json",
            "retry_loop_3x/attempt1.response.json",
        ),
        (
            "retry",
            2,
            Provider::Openai,
            "retry_loop_3x/attempt2.request.json",
            "retry_loop_3x/attempt2.response.json",
        ),
        (
            "retry",
            3,
            Provider::Openai,
            "retry_loop_3x/attempt3.request.json",
            "retry_loop_3x/attempt3.response.json",
        ),
        (
            "bloat",
            1,
            Provider::Anthropic,
            "bloated_system_prompt/step1.request.json",
            "bloated_system_prompt/step1.response.json",
        ),
        (
            "bloat",
            2,
            Provider::Anthropic,
            "bloated_system_prompt/step2.request.json",
            "bloated_system_prompt/step2.response.json",
        ),
        (
            "bloat",
            3,
            Provider::Anthropic,
            "bloated_system_prompt/step3.request.json",
            "bloated_system_prompt/step3.response.json",
        ),
        (
            "verbose",
            1,
            Provider::Anthropic,
            "verbose_tool_output/request.json",
            "verbose_tool_output/response.json",
        ),
        (
            "stream",
            1,
            Provider::Anthropic,
            "anthropic_stream/request.json",
            "anthropic_stream/response.sse",
        ),
    ];
    let store = Store::open(db).unwrap();
    for (run, ord, provider, req, resp) in steps {
        let s = ingest_step(*run, *ord, *provider, &read(req), &read(resp)).unwrap();
        store.record_step(&s, date).unwrap();
    }
}

#[test]
fn report_text_json_and_today_filter() {
    let tmp = std::env::temp_dir().join(format!("tare-report-{}.db", std::process::id()));
    let db = tmp.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&db);
    let today = tare_cli::today_utc();
    load_store(&db, &today);

    let pricing = pricing();
    let report = tare_cli::report_for(&db, false, &pricing).unwrap();

    // All four causes present, ranked by projected savings desc.
    let order: Vec<&str> = report.rows.iter().map(|r| r.cause.as_str()).collect();
    assert_eq!(
        order,
        vec![
            "bloated-system-prompt",
            "cache-read-vs-write",
            "verbose-tool-output",
            "retry-loop"
        ]
    );
    let mut saved: Vec<i64> = report
        .rows
        .iter()
        .map(|r| r.projected_saved_micros)
        .collect();
    let sorted = {
        let mut s = saved.clone();
        s.sort_by(|a, b| b.cmp(a));
        s
    };
    assert_eq!(saved, sorted, "rows must be ranked by projected_saved desc");
    saved.dedup();

    // Text rendering is deterministic and carries the estimated/pricing labels.
    let text = tare_cli::render_report_text(&report);
    assert!(text.contains("estimated (pricing fixture-2026.06"));
    assert!(text.contains("Total estimated spend:"));
    for cause in [
        "retry-loop",
        "bloated-system-prompt",
        "verbose-tool-output",
        "cache-read-vs-write",
    ] {
        assert!(text.contains(cause), "text missing {cause}");
    }

    // --json equals a direct core report over the same runs, stamped with the active privacy
    // profile (the CLI report carries privacy_policy_id + profile; core build_report omits it).
    let runs = Store::open(&db).unwrap().load_runs().unwrap();
    let direct = attribute::build_report(&runs, &pricing)
        .with_privacy(&tare_cli::resolve_privacy().unwrap());
    assert_eq!(
        serde_json::to_string(&report).unwrap(),
        serde_json::to_string(&direct).unwrap()
    );
    // The stamped report names the active profile.
    assert_eq!(report.profile.as_deref(), Some("strict_counts"));

    // --today (all rows recorded today) matches the unfiltered report.
    let today_report = tare_cli::report_for(&db, true, &pricing).unwrap();
    assert_eq!(today_report.total_micros, report.total_micros);
    assert_eq!(today_report.rows.len(), report.rows.len());

    let _ = std::fs::remove_file(&db);
}
