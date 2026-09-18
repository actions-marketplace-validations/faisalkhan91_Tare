//! `tare share`: a single-file, self-contained, **redacted** HTML one-pager of a
//! cost picture — the Savings Ledger (ranked recoverable-$ worklist) plus an optional inline
//! flamegraph SVG. Meant to be handed to a teammate or pasted into a doc.
//!
//! Redaction is INHERENT, not bolted on: Tare's store only ever holds counts, labels, hashes, and
//! integer dollars — never a prompt/response payload — and this renderer draws
//! exclusively from those already-aggregated structures. The output is fully self-contained: inline
//! CSS only, NO `<script>`, and NO external URLs (no `http(s)://`, no `<link>`/remote `<img>`), so it
//! renders offline and can't phone home. Pure + deterministic: the "generated" label is passed in
//! (clock-free core), and the SVG is already byte-stable.

use crate::experiment::ExperimentResult;
use crate::money::MicroUsd;
use crate::savings::SavingsLedger;

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Render a self-contained redacted HTML report. `title` and `generated_label` are free text (a
/// date/run scope the caller supplies); `flamegraph_svg`, if present, is inlined verbatim (it is
/// already a byte-stable, self-contained `<svg>` from [`crate::svg`]).
pub fn render_share_html(
    title: &str,
    ledger: &SavingsLedger,
    generated_label: &str,
    flamegraph_svg: Option<&str>,
    experiment: Option<&ExperimentResult>,
) -> String {
    let mut out = String::new();
    out.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    out.push_str(&format!("<title>{}</title>", esc(title)));
    // Inline style only — nothing loaded from the network.
    out.push_str(
        "<style>\
         :root{color-scheme:light dark}\
         body{font:14px/1.5 -apple-system,Segoe UI,Roboto,sans-serif;margin:2rem auto;max-width:52rem;padding:0 1rem;color:#1c1c1c;background:#fff}\
         h1{font-size:1.4rem;margin:0 0 .25rem}\
         .sub{color:#666;margin:0 0 1.5rem}\
         .kpis{display:flex;gap:1.5rem;margin:1rem 0}\
         .kpi{background:#f4f5f7;border-radius:8px;padding:.75rem 1rem}\
         .kpi b{display:block;font-size:1.3rem}\
         table{border-collapse:collapse;width:100%;margin:1rem 0}\
         th,td{text-align:left;padding:.4rem .6rem;border-bottom:1px solid #eee}\
         td.num,th.num{text-align:right;font-variant-numeric:tabular-nums}\
         .tag{font-size:.75rem;color:#555;background:#eef;border-radius:4px;padding:.1rem .4rem}\
         .fix{color:#444;font-size:.85rem}\
         footer{color:#888;font-size:.8rem;margin-top:2rem;border-top:1px solid #eee;padding-top:.75rem}\
         svg{max-width:100%;height:auto;border:1px solid #eee;border-radius:8px}\
         </style></head><body>",
    );
    out.push_str(&format!("<h1>{}</h1>", esc(title)));
    out.push_str(&format!(
        "<p class=\"sub\">Estimated · generated {} · pricing {}</p>",
        esc(generated_label),
        esc(&ledger.pricing_version)
    ));

    // KPIs: total spend, spend-bounded capped potential, savings index.
    out.push_str("<div class=\"kpis\">");
    out.push_str(&format!(
        "<div class=\"kpi\">spend<b>{}</b></div>",
        MicroUsd(ledger.total_spend_micros).to_dollar_string_2dp()
    ));
    out.push_str(&format!(
        "<div class=\"kpi\">capped potential<b>{}</b></div>",
        MicroUsd(ledger.total_recoverable_micros).to_dollar_string_2dp()
    ));
    out.push_str(&format!(
        "<div class=\"kpi\">savings index<b>{}</b></div>",
        ledger.savings_index
    ));
    out.push_str("</div>");

    // Savings ledger table.
    out.push_str("<h2>Where the money is recoverable</h2>");
    if ledger.opportunities.is_empty() {
        out.push_str("<p>No recoverable opportunities found — nothing to trim.</p>");
    } else {
        out.push_str(
            "<table><thead><tr><th>kind</th><th>what</th><th class=\"num\">recoverable</th>\
             <th>confidence</th></tr></thead><tbody>",
        );
        for o in &ledger.opportunities {
            out.push_str(&format!(
                "<tr><td><span class=\"tag\">{}</span></td><td>{}<div class=\"fix\">{}</div></td>\
                 <td class=\"num\">{}</td><td>{}</td></tr>",
                esc(&o.kind),
                esc(&o.label),
                esc(&o.fix_text),
                MicroUsd(o.recoverable_micros).to_dollar_string_2dp(),
                esc(&o.confidence),
            ));
        }
        out.push_str("</tbody></table>");
    }

    // CostExperiment: the offline reprice-over-axes grid + the cheapest-config proof.
    if let Some(exp) = experiment {
        out.push_str("<h2>Cost experiment — cheapest equal-quality config</h2>");
        out.push_str(&format!(
            "<p class=\"sub\">Baseline {} · best {} · saves {}{}</p>",
            MicroUsd(exp.baseline_micros).to_dollar_string_2dp(),
            MicroUsd(exp.best_micros).to_dollar_string_2dp(),
            MicroUsd(exp.best_saving_micros).to_dollar_string_2dp(),
            if exp.approximate {
                " · APPROXIMATE (cross-model tokenizer caveat)"
            } else {
                ""
            }
        ));
        out.push_str(
            "<p class=\"fix\">Quality is a user-supplied constraint, never computed — with no quality \
             signal the cost×quality frontier degrades to the cheapest cell(s), highlighted below.</p>",
        );
        out.push_str(
            "<table><thead><tr><th>config</th><th class=\"num\">cost</th>\
             <th class=\"num\">Δ vs baseline</th><th>notes</th></tr></thead><tbody>",
        );
        for (i, cell) in exp.cells.iter().enumerate() {
            let delta = cell.cost_micros - exp.baseline_micros;
            let on_frontier = exp.pareto.contains(&i);
            out.push_str(&format!(
                "<tr{row_attr}><td>{}{star}</td><td class=\"num\">{}</td><td class=\"num\">{}{}</td><td>{}</td></tr>",
                esc(&cell.label.join(", ")),
                MicroUsd(cell.cost_micros).to_dollar_string_2dp(),
                if delta > 0 { "+" } else { "" },
                MicroUsd(delta).to_dollar_string_2dp(),
                if cell.approximate { "<span class=\"tag\">approx</span>" } else { "" },
                row_attr = if on_frontier { " style=\"background:#eafaf0\"" } else { "" },
                star = if on_frontier { " ★" } else { "" },
            ));
        }
        out.push_str("</tbody></table>");
    }

    if let Some(svg) = flamegraph_svg {
        out.push_str("<h2>Cost flamegraph</h2>");
        out.push_str(svg); // already a self-contained, byte-stable <svg>
    }

    out.push_str(
        "<footer>Generated locally by Tare. Every figure is an ESTIMATE recomputed from captured \
         token counts — no prompt or response content is included.</footer>",
    );
    out.push_str("</body></html>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::savings::{Opportunity, SavingsLedger};

    fn ledger() -> SavingsLedger {
        SavingsLedger {
            opportunities: vec![Opportunity {
                kind: "loop".into(),
                label: "agent<script>x".into(), // adversarial label → must be escaped
                recoverable_micros: 1_250_000,
                confidence: "measured".into(),
                fix_text: "cap it".into(),
                effort: "S".into(),
            }],
            total_recoverable_micros: 1_250_000,
            total_spend_micros: 5_000_000,
            savings_index: 75,
            pricing_version: "2026.06.01".into(),
            estimated: true,
        }
    }

    #[test]
    fn share_html_is_self_contained_and_escapes_labels() {
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>";
        let html = render_share_html("My Run", &ledger(), "2026-07-02", Some(svg), None);
        // Structure.
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<title>My Run</title>"));
        assert!(html.contains("$5.00") && html.contains("$1.25")); // spend + recoverable KPIs
        assert!(html.contains("savings index<b>75</b>"));
        assert!(html.contains("<span class=\"tag\">loop</span>"));
        // The inline flamegraph SVG is embedded.
        assert!(html.contains("<rect/>"));
        // Adversarial label is HTML-escaped (no live <script> injected).
        assert!(html.contains("agent&lt;script&gt;x"));
        assert!(!html.contains("agent<script>x"));
        // Self-contained: no scripts, and the only network-looking token is the SVG's xmlns
        // namespace URI (not a fetchable resource). No stylesheet/img/script loads.
        assert!(!html.contains("<script"));
        assert!(!html.contains("src=\"http"));
        assert!(!html.contains("<link"));
    }

    #[test]
    fn empty_ledger_still_renders_a_valid_page() {
        let mut l = ledger();
        l.opportunities.clear();
        l.total_recoverable_micros = 0;
        let html = render_share_html("Empty", &l, "2026-07-02", None, None);
        assert!(html.contains("nothing to trim"));
        assert!(html.ends_with("</body></html>"));
    }

    #[test]
    fn embeds_a_cost_experiment_with_cheapest_config_proof() {
        use crate::experiment::{ExperimentCell, ExperimentResult};
        let exp = ExperimentResult {
            cells: vec![
                ExperimentCell {
                    coords: vec![],
                    label: vec!["model=claude-haiku-4-5".into()],
                    cost_micros: 400_000,
                    quality: None,
                    approximate: true,
                },
                ExperimentCell {
                    coords: vec![],
                    label: vec!["model=*as-captured*".into()],
                    cost_micros: 1_000_000,
                    quality: None,
                    approximate: false,
                },
            ],
            pareto: vec![0], // cheapest cell
            baseline_micros: 1_000_000,
            best_micros: 400_000,
            best_saving_micros: 600_000,
            pricing_version: "2026.06.01".into(),
            estimated: true,
            approximate: true,
        };
        let html = render_share_html("Exp", &ledger(), "2026-07-02", None, Some(&exp));
        assert!(html.contains("Cost experiment"));
        assert!(html.contains("saves $0.60"));
        assert!(html.contains("APPROXIMATE"));
        assert!(
            html.contains("★"),
            "cheapest cell is starred on the frontier"
        );
        assert!(html.contains("claude-haiku-4-5"));
        // Δ vs baseline shown for the pricier as-captured row.
        assert!(html.contains("+$0.00") || html.contains("-$0.60"));
    }
}
