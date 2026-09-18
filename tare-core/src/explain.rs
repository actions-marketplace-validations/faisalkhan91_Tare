//! Deterministic plain-language narrative of a cost report — for a human in a hurry or an agent
//! reading its own spend mid-session (via the MCP tool). PURE: a template over `attribute::Report`
//! (counts/integer-micros only) — NO LLM, NO clock, NO RNG. Byte-stable, so it can be a golden.
//!
//! Kept Rust-only by design: byte-identical natural-language prose across Rust + TS is far
//! harder to hold than SVG, so the web viewer consumes this via HTTP/MCP rather than re-porting.

use crate::attribute::Report;
use crate::money::{percent_of, MicroUsd};
use crate::savings::SavingsLedger;

/// One-paragraph narrative of `report` + the savings `ledger`. Lists each cause with its share of
/// spend, then the per-category opportunities and spend-bounded capped potential used by the web
/// Optimize screen and MCP. Notes any unpriced models excluded from the total.
pub fn explain(report: &Report, ledger: &SavingsLedger) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Estimated spend: {} (pricing {}). All figures are estimates.\n",
        MicroUsd(report.total_micros).to_dollar_string_2dp(),
        report.pricing_version
    ));

    if report.rows.is_empty() {
        out.push_str("No attributable waste was found in this window.\n");
    } else {
        out.push_str("Where it's going (largest savings first):\n");
        for row in &report.rows {
            let pct = percent_of(row.micros, report.total_micros);
            out.push_str(&format!(
                "- {}: {} ({}% of spend). {} Trimming could save ~{}.\n",
                row.cause,
                MicroUsd(row.micros).to_dollar_string_2dp(),
                pct,
                row.detail,
                MicroUsd(row.projected_saved_micros).to_dollar_string_2dp(),
            ));
        }
    }

    // Per-category opportunities from the SAME ledger the web/MCP surface. Categories can overlap;
    // the headline below is capped by spend and is not a deduplicated sum.
    if !ledger.opportunities.is_empty() {
        out.push_str("Potential opportunities (largest first; categories may overlap):\n");
        for o in ledger.opportunities.iter().take(5) {
            out.push_str(&format!(
                "- {} · {}: ~{} ({}).\n",
                o.kind,
                o.label,
                MicroUsd(o.recoverable_micros).to_dollar_string_2dp(),
                o.confidence,
            ));
        }
        out.push_str(&format!(
            "Capped potential ~{} of {} estimated — Savings Index {}/100.\n",
            MicroUsd(ledger.total_recoverable_micros).to_dollar_string_2dp(),
            MicroUsd(ledger.total_spend_micros).to_dollar_string_2dp(),
            ledger.savings_index,
        ));
    }

    if !report.unpriced.is_empty() {
        let tokens = report
            .unpriced
            .iter()
            .map(|u| u.token_total)
            .fold(0u64, u64::saturating_add);
        let models: Vec<String> = report
            .unpriced
            .iter()
            .map(|u| format!("{}/{}", u.provider, u.model))
            .collect();
        out.push_str(&format!(
            "Note: {tokens} tokens on unpriced model(s) [{}] are NOT included above.\n",
            models.join(", ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribute::{Report, TrimRow, UnpricedModel};
    use crate::savings::{Opportunity, SavingsLedger};

    fn ledger() -> SavingsLedger {
        SavingsLedger {
            opportunities: vec![Opportunity {
                kind: "cache".into(),
                label: "system prompt".into(),
                recoverable_micros: 40_000,
                confidence: "projected".into(),
                fix_text: "Add cache_control.".into(),
                effort: "S".into(),
            }],
            total_recoverable_micros: 40_000,
            total_spend_micros: 100_000,
            savings_index: 60,
            pricing_version: "fixture-2026.06".into(),
            estimated: true,
        }
    }

    fn empty_ledger() -> SavingsLedger {
        SavingsLedger {
            opportunities: vec![],
            total_recoverable_micros: 0,
            total_spend_micros: 0,
            savings_index: 100,
            pricing_version: "fixture-2026.06".into(),
            estimated: true,
        }
    }

    fn report() -> Report {
        Report {
            pricing_version: "fixture-2026.06".into(),
            effective_date: "2026-06-01".into(),
            estimated: true,
            total_micros: 100_000, // $0.10
            rows: vec![TrimRow {
                cause: "bloated-system-prompt".into(),
                detail: "Cache the stable prefix.".into(),
                tokens: 6000,
                micros: 60_000, // $0.06, 60%
                projected_saved_micros: 54_000,
            }],
            unpriced: vec![UnpricedModel {
                provider: "openai".into(),
                model: "gpt-future".into(),
                token_total: 1234,
                step_count: 2,
            }],
            privacy_policy_id: None,
            profile: None,
            attribution_confidence: None,
        }
    }

    #[test]
    fn narrative_is_deterministic_and_uses_2dp_plus_percent() {
        let a = explain(&report(), &ledger());
        assert_eq!(a, explain(&report(), &ledger())); // deterministic
        assert!(a.contains("Estimated spend: $0.10"));
        assert!(a.contains("bloated-system-prompt: $0.06 (60% of spend)"));
        assert!(a.contains("Potential opportunities (largest first; categories may overlap):"));
        assert!(a.contains("cache · system prompt: ~$0.04 (projected)"));
        assert!(a.contains("Capped potential ~$0.04 of $0.10 estimated — Savings Index 60/100."));
        // The old naive-sum TOTAL line is gone (per-row "Trimming could save" detail remains).
        assert!(!a.contains("Addressing all of the above"));
        assert!(a.contains("gpt-future")); // unpriced surfaced
    }

    #[test]
    fn empty_report_reads_cleanly() {
        let mut r = report();
        r.rows.clear();
        r.unpriced.clear();
        let a = explain(&r, &empty_ledger());
        assert!(a.contains("No attributable waste"));
        assert!(!a.contains("To recover")); // no opportunities -> no recovery section
    }
}
