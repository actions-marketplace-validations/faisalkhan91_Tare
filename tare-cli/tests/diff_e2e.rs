//! `tare diff` reads two `tare report --json` files and computes the delta.

use tare_core::attribute::{Report, TrimRow};

fn row(cause: &str, micros: i64) -> TrimRow {
    TrimRow {
        cause: cause.to_string(),
        detail: String::new(),
        tokens: 0,
        micros,
        projected_saved_micros: 0,
    }
}

fn report(total: i64, rows: Vec<TrimRow>) -> Report {
    Report {
        pricing_version: "fixture-2026.06".into(),
        effective_date: "2026-06-01".into(),
        estimated: true,
        total_micros: total,
        rows,
        unpriced: Vec::new(),
        privacy_policy_id: None,
        profile: None,
        attribution_confidence: None,
    }
}

#[test]
fn diff_files_computes_delta() {
    let dir = std::env::temp_dir();
    let before = dir.join(format!("tare-diff-before-{}.json", std::process::id()));
    let after = dir.join(format!("tare-diff-after-{}.json", std::process::id()));

    std::fs::write(
        &before,
        serde_json::to_string(&report(
            100_000,
            vec![
                row("bloated-system-prompt", 80_000),
                row("retry-loop", 20_000),
            ],
        ))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        &after,
        serde_json::to_string(&report(
            30_000,
            vec![
                row("retry-loop", 20_000),
                row("verbose-tool-output", 10_000),
            ],
        ))
        .unwrap(),
    )
    .unwrap();

    let d = tare_cli::diff_files(&before.to_string_lossy(), &after.to_string_lossy()).unwrap();
    assert_eq!(d.delta_micros, -70_000);
    assert_eq!(d.rows[0].cause, "bloated-system-prompt"); // biggest change
    assert_eq!(d.rows[0].delta_micros, -80_000);

    let text = tare_cli::render_diff_text(&d);
    assert!(text.contains("Total: $0.100000 -> $0.030000"));
    assert!(text.contains("-70%"));

    let _ = std::fs::remove_file(&before);
    let _ = std::fs::remove_file(&after);
}
