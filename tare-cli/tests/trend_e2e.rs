//! `tare trend` over a store loaded with dated fixtures: window resolution, dimensions,
//! JSON/SVG/text outputs, and the empty-DB message.

use std::path::{Path, PathBuf};
use tare_core::model::Provider;
use tare_core::trend::TrendDimension;
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
fn trend_window_dimensions_and_outputs() {
    let tmp = std::env::temp_dir().join(format!("tare-trend-{}.db", std::process::id()));
    let db = tmp.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&db);

    // Empty DB -> no window.
    assert!(
        tare_cli::trend_for(&db, None, None, TrendDimension::Total, &pricing())
            .unwrap()
            .is_none()
    );

    {
        let store = Store::open(&db).unwrap();
        let oai = (
            Provider::Openai,
            "openai_nonstream/request.json",
            "openai_nonstream/response.json",
        );
        let bloat = (
            Provider::Anthropic,
            "bloated_system_prompt/step1.request.json",
            "bloated_system_prompt/step1.response.json",
        );
        for (i, (date, (p, req, resp))) in [("2026-06-20", oai), ("2026-06-22", bloat)]
            .into_iter()
            .enumerate()
        {
            let s = ingest_step(format!("r{i}"), 1, p, &read(req), &read(resp)).unwrap();
            store.record_step(&s, date).unwrap();
        }
    }

    // Explicit window, by provider: dense 3-day axis, two providers present.
    let report = tare_cli::trend_for(
        &db,
        Some("2026-06-20"),
        Some("2026-06-22"),
        TrendDimension::ByProvider,
        &pricing(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(report.days.len(), 3);
    let keys: Vec<&str> = report.series.iter().map(|s| s.key.as_str()).collect();
    assert!(keys.contains(&"anthropic") && keys.contains(&"openai"));

    // Window auto-clamps to the data bounds when no flags given.
    let auto = tare_cli::trend_for(&db, None, None, TrendDimension::Total, &pricing())
        .unwrap()
        .unwrap();
    assert!(!auto.days.is_empty());

    // Text + SVG renderers are non-empty and labelled.
    let text = tare_cli::render_trend_text(&report);
    assert!(text.contains("Tare trend — estimated"));
    let svg = tare_core::svg::render_trend_svg(&report);
    assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));

    let _ = std::fs::remove_file(&db);
}
