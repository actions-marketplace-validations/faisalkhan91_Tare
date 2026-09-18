//! `tare attest` and offline `tare verify` — recomputable cost receipt.

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

fn seed(db: &str) {
    let store = Store::open(db).unwrap();
    let s = ingest_step(
        "r1",
        1,
        Provider::Anthropic,
        &read("anthropic_stream/request.json"),
        &read("anthropic_stream/response.sse"),
    )
    .unwrap();
    store.record_step(&s, "2026-06-25").unwrap();
}

#[test]
fn attest_then_verify_round_trips() {
    let db = std::env::temp_dir().join(format!("tare-rcpt-{}.db", std::process::id()));
    let db = db.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&db);
    seed(&db);
    let p = pricing();

    let json = tare_cli::attest_receipt(&db, Some("r1"), false, &p).unwrap();
    let path = std::env::temp_dir().join(format!("tare-rcpt-{}.json", std::process::id()));
    std::fs::write(&path, &json).unwrap();
    let out = tare_cli::verify_receipt(&path.to_string_lossy(), &p).unwrap();
    assert!(out.contains("receipt OK"));
    assert!(out.contains("flamegraph re-rendered+matched: true"));

    // A hand-edited dollar figure must fail verification.
    let tampered = json.replace("\"scope\"", "\"scope_x\"");
    let bad = std::env::temp_dir().join(format!("tare-rcpt-bad-{}.json", std::process::id()));
    std::fs::write(&bad, &tampered).unwrap();
    assert!(tare_cli::verify_receipt(&bad.to_string_lossy(), &p).is_err());

    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&bad);
}

#[test]
fn max_private_receipt_has_no_flamegraph_but_verifies() {
    let db = std::env::temp_dir().join(format!("tare-rcptp-{}.db", std::process::id()));
    let db = db.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&db);
    seed(&db);
    let p = pricing();

    let json = tare_cli::attest_receipt(&db, Some("r1"), true, &p).unwrap();
    assert!(
        !json.contains("flamegraph_svg"),
        "max_private carries no flamegraph"
    );
    let path = std::env::temp_dir().join(format!("tare-rcptp-{}.json", std::process::id()));
    std::fs::write(&path, &json).unwrap();
    let out = tare_cli::verify_receipt(&path.to_string_lossy(), &p).unwrap();
    assert!(out.contains("flamegraph re-rendered+matched: false"));

    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(&path);
}
