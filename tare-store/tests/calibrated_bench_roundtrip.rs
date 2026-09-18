//! Round-trip test for the Calibrated Bench fixture: the deterministic dataset in
//! `tare_core::calibrated_bench` must persist into a real store without any allowlist/insert error,
//! and read back with exactly the expected run/step counts and annotations.

use tare_core::calibrated_bench;
use tare_store::Store;

#[test]
fn fixture_persists_and_reads_back_with_expected_totals() {
    let bench = calibrated_bench::build();
    let store = Store::open_in_memory().expect("open in-memory store");

    // Persist every step; a shape that violated the privacy allowlist would error here.
    for sr in &bench.runs {
        for step in &sr.run.steps {
            store
                .record_step_with_time(
                    step,
                    &sr.date,
                    sr.hour,
                    None,
                    Some(sr.profile),
                    Some(sr.source),
                )
                .expect("record step");
        }
        if let Some(score) = sr.quality {
            store
                .set_run_quality(
                    &sr.run.run_id,
                    score,
                    "cli",
                    calibrated_bench::SEED_UPDATED_AT,
                )
                .expect("set quality");
        }
        if !sr.tags.is_empty() || !sr.note.is_empty() || sr.starred {
            let tags_json = serde_json::to_string(&sr.tags).unwrap();
            store
                .upsert_run_note(
                    &sr.run.run_id,
                    &tags_json,
                    sr.note,
                    sr.starred,
                    calibrated_bench::SEED_UPDATED_AT,
                )
                .expect("upsert note");
        }
    }
    for s in &bench.sessions {
        store
            .record_session_beat(s.source, s.session, s.last_unix, s.last_model)
            .expect("session beat");
    }

    // Read back: run and step counts must match the golden totals exactly.
    let runs = store.load_runs().expect("load runs");
    assert_eq!(runs.len(), bench.totals.total_runs, "run count");
    let steps: usize = runs.iter().map(|r| r.steps.len()).sum();
    assert_eq!(steps, bench.totals.total_steps, "step count");

    // Quality/note annotations round-trip on a known anomaly-day run.
    let q = store
        .run_quality("cb-2026-05-20-00")
        .expect("query quality");
    assert!(q.is_some(), "anomaly run has a user quality score");
    let note = store.load_run_note("cb-2026-05-20-00").expect("query note");
    let note = note.expect("anomaly run has a note");
    assert!(
        note.tags.iter().any(|t| t == "anomaly"),
        "anomaly tag persisted: {:?}",
        note.tags
    );
}
