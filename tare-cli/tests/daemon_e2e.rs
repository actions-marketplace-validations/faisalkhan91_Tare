//! `tare daemon` anomaly delivery: fire once across ticks via the persistent fired set.

use std::path::{Path, PathBuf};
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
fn an_anomaly_fires_once_across_two_ticks() {
    let db = std::env::temp_dir().join(format!("tare-daemon-{}.db", std::process::id()));
    let db = db.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&db);
    let store = Store::open(&db).unwrap();
    // A flat baseline of cheap days, then a large spike — a detectable Total anomaly.
    for (i, date) in [
        "2026-06-18",
        "2026-06-19",
        "2026-06-20",
        "2026-06-21",
        "2026-06-22",
    ]
    .iter()
    .enumerate()
    {
        let s = ingest_step(
            format!("c{i}"),
            1,
            Provider::Openai,
            &read("openai_nonstream/request.json"),
            &read("openai_nonstream/response.json"),
        )
        .unwrap();
        store.record_step(&s, date).unwrap();
    }
    // A MATERIALLY large spike: many bloated runs on one day, so its dollar impact clears the
    // materiality bar (suppresses minor spikes — a few cents wouldn't, and shouldn't, alarm).
    for r in 0..25u32 {
        for i in 1..=3u32 {
            let s = ingest_step(
                format!("spike{r}"),
                i,
                Provider::Anthropic,
                &read(&format!("bloated_system_prompt/step{i}.request.json")),
                &read(&format!("bloated_system_prompt/step{i}.response.json")),
            )
            .unwrap();
            store.record_step(&s, "2026-06-23").unwrap();
        }
    }
    drop(store);
    let p = pricing();

    // First tick delivers the spike; the second tick delivers nothing new (fire-once).
    let first = tare_cli::anomaly_delivery_tick(&db, &p, 5, 50).unwrap();
    assert!(!first.is_empty(), "the spike day should fire an anomaly");
    assert!(matches!(
        first[0].subject,
        tare_core::alert::AlertSubject::Anomaly { .. }
    ));
    // Detection -> resolution: the alert carries the materiality + a remediation
    // suggestion from the savings ledger (the bloated spike yields recoverable waste).
    assert!(
        first[0].message.contains("Likely waste →"),
        "alert should attach a savings-ledger remediation: {}",
        first[0].message
    );
    let second = tare_cli::anomaly_delivery_tick(&db, &p, 5, 50).unwrap();
    assert!(second.is_empty(), "the same anomaly must not fire twice");

    let _ = std::fs::remove_file(&db);
}
