//! Deterministic static SVG renderer for a FlamegraphModel.
//! No Canvas/WebGL, no browser API, no clock, no RNG, no env, no map iteration.
//! Integer pixel arithmetic only → byte-stable output. Shared (by spec) with the
//! web UI via a TypeScript port that produces identical bytes.

use crate::flame_diff::{FlameDiffModel, FlameDiffNode};
use crate::flamegraph::{FlamegraphModel, FlamegraphNode};
use crate::model::CacheClass;
use crate::money::MicroUsd;
use crate::trend::TrendReport;

const WIDTH: i64 = 960;
const ROW_H: i64 = 30;
const PAD: i64 = 12;
const LEGEND_H: i64 = 28;

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

fn depth_of(node: &FlamegraphNode) -> i64 {
    1 + node.children.iter().map(depth_of).max().unwrap_or(0)
}

/// Largest-remainder split of `total` pixels across child token weights.
fn split_px(total: i64, weights: &[u64]) -> Vec<i64> {
    let sum: u128 = weights.iter().map(|&w| w as u128).sum();
    if sum == 0 || total <= 0 {
        return vec![0; weights.len()];
    }
    let t = total as u128;
    let mut base = Vec::with_capacity(weights.len());
    let mut rem: Vec<(u128, usize)> = Vec::with_capacity(weights.len());
    let mut used: u128 = 0;
    for (i, &w) in weights.iter().enumerate() {
        let num = t * w as u128;
        base.push((num / sum) as i64);
        rem.push((num % sum, i));
        used += num / sum;
    }
    let mut leftover = t - used;
    rem.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for &(_, idx) in &rem {
        if leftover == 0 {
            break;
        }
        base[idx] += 1;
        leftover -= 1;
    }
    base
}

fn fill_for(node: &FlamegraphNode) -> &'static str {
    match node.cache_class {
        Some(c) => c.color(),
        None => "#d9dce3", // neutral container
    }
}

fn render_node(node: &FlamegraphNode, x: i64, w: i64, depth: i64, out: &mut String) {
    let y = PAD + depth * ROW_H;
    let h = ROW_H - 2;
    let dollars = MicroUsd(node.micros).to_dollar_string();
    let title = format!("{} · {} tok · {}", node.name, node.tokens, dollars);
    out.push_str(&format!(
        "<g><rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" fill=\"{fill}\" stroke=\"#ffffff\" stroke-width=\"1\"/><title>{title}</title>",
        fill = fill_for(node),
        title = esc(&title),
    ));
    // Label only if it plausibly fits (~7px per char).
    let max_chars = ((w - 8).max(0) / 7) as usize;
    if max_chars >= 3 {
        let mut label = node.name.clone();
        if label.chars().count() > max_chars {
            label = label
                .chars()
                .take(max_chars.saturating_sub(1))
                .collect::<String>()
                + "…";
        }
        out.push_str(&format!(
            "<text x=\"{tx}\" y=\"{ty}\" font-family=\"monospace\" font-size=\"11\" fill=\"#1c1c1c\">{label}</text>",
            tx = x + 4,
            ty = y + 14,
            label = esc(&label),
        ));
    }
    out.push_str("</g>");

    // Children fill this node's width proportionally to tokens.
    if !node.children.is_empty() {
        let weights: Vec<u64> = node.children.iter().map(|c| c.tokens).collect();
        let widths = split_px(w, &weights);
        let mut cx = x;
        for (child, cw) in node.children.iter().zip(widths) {
            render_node(child, cx, cw, depth + 1, out);
            cx += cw;
        }
    }
}

fn render_legend(y: i64, out: &mut String) {
    let items = [
        CacheClass::Fresh,
        CacheClass::CacheWrite5m,
        CacheClass::CacheWrite1h,
        CacheClass::CacheRead,
        CacheClass::Output,
        CacheClass::Reasoning,
    ];
    let mut x = PAD;
    for c in items {
        out.push_str(&format!(
            "<rect x=\"{x}\" y=\"{y}\" width=\"12\" height=\"12\" fill=\"{fill}\"/><text x=\"{tx}\" y=\"{ty}\" font-family=\"monospace\" font-size=\"10\" fill=\"#1c1c1c\">{label}</text>",
            fill = c.color(),
            tx = x + 16,
            ty = y + 10,
            label = esc(c.as_str()),
        ));
        // Advance by Unicode scalar count (NOT UTF-8 byte len) so this matches the TS port's
        // `Array.from(...).length` for any multibyte label and the SVG stays byte-identical.
        x += 16 + (c.as_str().chars().count() as i64) * 7 + 16;
    }
}

/// Render the model to a self-contained, byte-stable SVG string.
pub fn render_svg(model: &FlamegraphModel) -> String {
    let depth = depth_of(&model.root);
    let legend_y = PAD + depth * ROW_H + 8;
    let height = legend_y + LEGEND_H;
    let mut out = String::new();
    out.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{W}\" height=\"{H}\" viewBox=\"0 0 {W} {H}\" font-family=\"monospace\">",
        W = WIDTH + 2 * PAD,
        H = height,
    ));
    out.push_str(&format!(
        "<rect x=\"0\" y=\"0\" width=\"{W}\" height=\"{H}\" fill=\"#ffffff\"/>",
        W = WIDTH + 2 * PAD,
        H = height,
    ));
    if model.root.tokens == 0 {
        out.push_str("<text x=\"12\" y=\"24\" font-size=\"12\">No usage</text></svg>");
        return out;
    }
    render_node(&model.root, PAD, WIDTH, 0, &mut out);
    render_legend(legend_y, &mut out);
    out.push_str("</svg>");
    out
}

// ---- Flame diff (node-level regression, red costlier / blue cheaper) ----

fn diff_depth(node: &FlameDiffNode) -> i64 {
    1 + node.children.iter().map(diff_depth).max().unwrap_or(0)
}

/// Diverging fill for a diff node. Absolute mode scales the delta by the larger tree total; share
/// mode uses the basis-point delta directly. Fixed shades keep the SVG byte-stable.
fn diff_fill(node: &FlameDiffNode, normalized: bool, scale_total: i64) -> &'static str {
    // Signed intensity in basis points: how big is this node's delta on the chosen axis?
    let signed_bps = if normalized {
        node.delta_bps
    } else if scale_total > 0 {
        (node.delta_micros as i128 * 10_000 / scale_total as i128) as i64
    } else {
        0
    };
    const STRONG: i64 = 500; // ≥5% shift → strong shade
    match signed_bps {
        b if b >= STRONG => "#c0392b", // strong red — much costlier in B
        b if b > 0 => "#e6a5ae",       // mild red
        0 => "#d9dce3",                // neutral — unchanged
        b if b > -STRONG => "#a5c4e6", // mild blue
        _ => "#2d6da8",                // strong blue — much cheaper in B
    }
}

fn render_diff_node(
    node: &FlameDiffNode,
    x: i64,
    w: i64,
    depth: i64,
    normalized: bool,
    scale_total: i64,
    out: &mut String,
) {
    let y = PAD + depth * ROW_H;
    let h = ROW_H - 2;
    let da = MicroUsd(node.micros_a).to_dollar_string();
    let db = MicroUsd(node.micros_b).to_dollar_string();
    let sign = if node.delta_micros > 0 {
        "+"
    } else {
        "" // negatives already carry '-'
    };
    let delta = MicroUsd(node.delta_micros).to_dollar_string();
    let title = format!("{} · A {} → B {} · Δ {sign}{delta}", node.name, da, db);
    out.push_str(&format!(
        "<g><rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" fill=\"{fill}\" stroke=\"#ffffff\" stroke-width=\"1\"/><title>{title}</title>",
        fill = diff_fill(node, normalized, scale_total),
        title = esc(&title),
    ));
    let max_chars = ((w - 8).max(0) / 7) as usize;
    if max_chars >= 3 {
        let mut label = node.name.clone();
        if label.chars().count() > max_chars {
            label = label
                .chars()
                .take(max_chars.saturating_sub(1))
                .collect::<String>()
                + "…";
        }
        out.push_str(&format!(
            "<text x=\"{tx}\" y=\"{ty}\" font-family=\"monospace\" font-size=\"11\" fill=\"#1c1c1c\">{label}</text>",
            tx = x + 4,
            ty = y + 14,
            label = esc(&label),
        ));
    }
    out.push_str("</g>");

    // Lay children out by the LARGER of each side's tokens, so nodes added OR removed in B are
    // visible (a removed node has tokens_b == 0 but still occupies its A width).
    if !node.children.is_empty() {
        let weights: Vec<u64> = node
            .children
            .iter()
            .map(|c| c.tokens_a.max(c.tokens_b))
            .collect();
        let widths = split_px(w, &weights);
        let mut cx = x;
        for (child, cw) in node.children.iter().zip(widths) {
            render_diff_node(child, cx, cw, depth + 1, normalized, scale_total, out);
            cx += cw;
        }
    }
}

fn render_diff_legend(y: i64, out: &mut String) {
    let items = [
        ("#c0392b", "much costlier"),
        ("#e6a5ae", "costlier"),
        ("#d9dce3", "unchanged"),
        ("#a5c4e6", "cheaper"),
        ("#2d6da8", "much cheaper"),
    ];
    let mut x = PAD;
    for (fill, label) in items {
        out.push_str(&format!(
            "<rect x=\"{x}\" y=\"{y}\" width=\"12\" height=\"12\" fill=\"{fill}\"/><text x=\"{tx}\" y=\"{ty}\" font-family=\"monospace\" font-size=\"10\" fill=\"#1c1c1c\">{label}</text>",
            tx = x + 16,
            ty = y + 10,
            label = esc(label),
        ));
        x += 16 + (label.chars().count() as i64) * 7 + 16;
    }
}

/// Render a `FlameDiffModel` to a self-contained, byte-stable diff SVG: red where B
/// got costlier, blue where cheaper, keyed on absolute Δ or (with `normalized`) share Δ.
pub fn render_diff_svg(model: &FlameDiffModel) -> String {
    let depth = diff_depth(&model.root);
    let legend_y = PAD + depth * ROW_H + 8;
    let height = legend_y + LEGEND_H;
    let scale_total = model.total_a_micros.max(model.total_b_micros);
    let mut out = String::new();
    out.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{W}\" height=\"{H}\" viewBox=\"0 0 {W} {H}\" font-family=\"monospace\">",
        W = WIDTH + 2 * PAD,
        H = height,
    ));
    out.push_str(&format!(
        "<rect x=\"0\" y=\"0\" width=\"{W}\" height=\"{H}\" fill=\"#ffffff\"/>",
        W = WIDTH + 2 * PAD,
        H = height,
    ));
    if model.root.tokens_a == 0 && model.root.tokens_b == 0 {
        out.push_str("<text x=\"12\" y=\"24\" font-size=\"12\">No usage</text></svg>");
        return out;
    }
    render_diff_node(
        &model.root,
        PAD,
        WIDTH,
        0,
        model.normalized,
        scale_total,
        &mut out,
    );
    render_diff_legend(legend_y, &mut out);
    out.push_str("</svg>");
    out
}

// ---- Trend (stacked daily bars) — byte-identical with web/src/trendSvg.ts ----

const TREND_PLOT_H: i64 = 240;
const TREND_AXIS_H: i64 = 16;

/// Stable palette keyed by sorted series index (no theme/RNG dependence).
fn trend_color(i: usize) -> &'static str {
    const PALETTE: &[&str] = &[
        "#4e79a7", "#f28e2b", "#59a14f", "#e15759", "#b07aa1", "#9c755f", "#76b7b2", "#edc948",
    ];
    PALETTE[i % PALETTE.len()]
}

/// Render a `TrendReport` to a self-contained, byte-stable SVG (stacked bar per day).
/// Mirrors `render_svg`'s envelope exactly so the TypeScript port matches byte-for-byte.
pub fn render_trend_svg(report: &TrendReport) -> String {
    let n = report.days.len();
    let legend_y = PAD + TREND_PLOT_H + TREND_AXIS_H + 8;
    let height = legend_y + LEGEND_H;
    let total_w = WIDTH + 2 * PAD;
    let mut out = String::new();
    out.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{W}\" height=\"{H}\" viewBox=\"0 0 {W} {H}\" font-family=\"monospace\">",
        W = total_w,
        H = height,
    ));
    out.push_str(&format!(
        "<rect x=\"0\" y=\"0\" width=\"{W}\" height=\"{H}\" fill=\"#ffffff\"/>",
        W = total_w,
        H = height,
    ));

    // Per-day totals and the scale max (i128 intermediate for bar heights).
    let day_total = |i: usize| -> i64 {
        report
            .series
            .iter()
            .filter_map(|s| s.per_day.get(i).copied())
            .fold(0i64, i64::saturating_add)
    };
    let max_total = (0..n).map(day_total).max().unwrap_or(0);
    if n == 0 || max_total == 0 {
        out.push_str("<text x=\"12\" y=\"24\" font-size=\"12\">No usage</text></svg>");
        return out;
    }

    let widths = split_px(WIDTH, &vec![1u64; n]);
    let baseline = PAD + TREND_PLOT_H;
    let mut x = PAD;
    #[allow(clippy::needless_range_loop)] // i indexes widths, days, and each series.per_day
    for i in 0..n {
        let col_w = widths[i];
        let mut acc = 0i64; // stacked pixels so far for this day
        for (si, s) in report.series.iter().enumerate() {
            let val = s.per_day.get(i).copied().unwrap_or(0);
            if val <= 0 {
                continue;
            }
            let seg = ((val as i128) * (TREND_PLOT_H as i128) / (max_total as i128)) as i64;
            if seg <= 0 {
                continue;
            }
            let y = baseline - acc - seg;
            let dollars = MicroUsd(val).to_dollar_string();
            let title = format!("{} · {} · {}", report.days[i], s.key, dollars);
            out.push_str(&format!(
                "<g><rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{seg}\" fill=\"{fill}\"/><title>{title}</title></g>",
                w = col_w,
                fill = trend_color(si),
                title = esc(&title),
            ));
            acc += seg;
        }
        x += col_w;
    }

    // Axis: label the first and last day (avoids clutter / byte bloat on wide windows).
    let axis_y = baseline + 12;
    let first = &report.days[0];
    let last = &report.days[n - 1];
    out.push_str(&format!(
        "<text x=\"{tx}\" y=\"{axis_y}\" font-family=\"monospace\" font-size=\"10\" fill=\"#1c1c1c\">{label}</text>",
        tx = PAD,
        label = esc(first),
    ));
    if n > 1 {
        out.push_str(&format!(
            "<text x=\"{tx}\" y=\"{axis_y}\" font-family=\"monospace\" font-size=\"10\" fill=\"#1c1c1c\" text-anchor=\"end\">{label}</text>",
            tx = PAD + WIDTH,
            label = esc(last),
        ));
    }

    // Legend: one swatch per series, in sorted order.
    let mut lx = PAD;
    for (si, s) in report.series.iter().enumerate() {
        out.push_str(&format!(
            "<rect x=\"{lx}\" y=\"{legend_y}\" width=\"12\" height=\"12\" fill=\"{fill}\"/><text x=\"{tx}\" y=\"{ty}\" font-family=\"monospace\" font-size=\"10\" fill=\"#1c1c1c\">{label}</text>",
            fill = trend_color(si),
            tx = lx + 16,
            ty = legend_y + 10,
            label = esc(&s.key),
        ));
        // Unicode scalar count (matching TypeScript `Array.from(key).length`), not UTF-8 bytes.
        lx += 16 + (s.key.chars().count() as i64) * 7 + 16;
    }
    out.push_str("</svg>");
    out
}

#[cfg(test)]
mod diff_tests {
    use super::*;
    use crate::flame_diff::flame_diff;
    use crate::flamegraph::build_flamegraph;
    use crate::{build_runs, ingest_step, model::Provider, pricing::PricingTable};

    fn pricing() -> PricingTable {
        PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml")).unwrap()
    }

    #[test]
    fn diff_svg_is_deterministic_and_colors_a_regression() {
        let req = include_bytes!("../../fixtures/bloated_system_prompt/step1.request.json");
        let resp = include_bytes!("../../fixtures/bloated_system_prompt/step1.response.json");
        let a = build_flamegraph(
            &build_runs(vec![
                ingest_step("a", 1, Provider::Anthropic, req, resp).unwrap()
            ])[0],
            &pricing(),
        );
        // B is the same task issued twice → strictly costlier at the root (a red regression).
        let b = build_flamegraph(
            &build_runs(vec![
                ingest_step("b", 1, Provider::Anthropic, req, resp).unwrap(),
                ingest_step("b", 2, Provider::Anthropic, req, resp).unwrap(),
            ])[0],
            &pricing(),
        );
        let svg1 = render_diff_svg(&flame_diff(&a, &b, false));
        let svg2 = render_diff_svg(&flame_diff(&a, &b, false));
        assert_eq!(svg1, svg2, "byte-stable");
        assert!(svg1.starts_with("<svg"));
        assert!(svg1.contains("much cheaper"), "legend present");
        // The legend paints every shade once; a real regression paints ≥1 more red NODE rect.
        let reds = svg1.matches("#c0392b").count();
        assert!(
            reds >= 2,
            "a strong-red regression node is painted (got {reds})"
        );
        // Self-diff has no red/blue NODES — those colors appear ONLY in the legend (once each).
        let self_svg = render_diff_svg(&flame_diff(&a, &a, false));
        assert_eq!(self_svg.matches("#c0392b").count(), 1, "red only in legend");
        assert_eq!(
            self_svg.matches("#2d6da8").count(),
            1,
            "blue only in legend"
        );
    }
}
