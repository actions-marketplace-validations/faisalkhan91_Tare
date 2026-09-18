//! Provider-invoice reconciliation: import a user-supplied provider invoice/usage
//! CSV and compare its per-model token/cost totals against Tare's estimate, surfacing the delta and
//! the coverage gap (models billed but not captured, and vice-versa). **Nothing leaves the box** —
//! this is a pure, offline function of the CSV text + the already-computed per-model estimate.
//!
//! The CSV schema is tolerant: a header row names the columns, and we accept the common aliases
//! providers actually emit (`model`; a dollar column `cost`/`amount`/`amount_usd`/`usd`/`total`;
//! and optional `input_tokens`/`output_tokens` with their aliases). Dollars are converted to integer
//! micro-USD at this boundary — nothing downstream sees floating-point money. Rows for the same model
//! are summed, so a monthly export with one line per day reconciles correctly.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One model's aggregated figures from the invoice CSV.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceLine {
    pub model: String,
    /// Billed cost in micro-USD (dollars from the CSV × 1e6, summed across rows).
    pub cost_micros: i64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// One reconciled model: invoice vs estimate, with the signed delta.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconRow {
    pub model: String,
    pub invoice_micros: i64,
    pub estimated_micros: i64,
    /// `estimated − invoice` (positive = Tare over-estimates vs the bill).
    pub delta_micros: i64,
    /// Signed delta as basis points of the invoice (`delta × 10_000 / invoice`); 0 when no invoice
    /// figure to divide by. Basis points keep it integer.
    pub delta_bps: i64,
    /// Present on the invoice CSV.
    pub on_invoice: bool,
    /// Present in Tare's estimate (captured spend).
    pub on_estimate: bool,
    /// How the invoice line was aligned to the estimate key: `exact` (identical name),
    /// `fuzzy` (matched by normalized name — e.g. invoice SKU "Claude 3.5 Sonnet" → id
    /// "claude-3-5-sonnet-…" — a LOWER-confidence alignment, surfaced so the user can eyeball it), or
    /// `none` (a one-sided row: an uncovered invoice model or an unbilled estimate model).
    pub matched: String,
}

/// The full reconciliation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconReport {
    /// One row per model seen on either side, sorted by absolute delta desc (biggest gap first).
    pub rows: Vec<ReconRow>,
    pub invoice_total_micros: i64,
    pub estimated_total_micros: i64,
    /// `estimated_total − invoice_total`.
    pub delta_micros: i64,
    /// Models billed on the invoice that Tare never captured (a capture coverage gap).
    pub uncovered_invoice_models: Vec<String>,
    /// Models Tare estimated that don't appear on the invoice (e.g. a different billing period, or
    /// a self-hosted overlay the provider never bills).
    pub unbilled_estimate_models: Vec<String>,
}

/// Parse RFC-4180-style CSV records, including escaped quotes and newlines inside quoted fields.
fn parse_csv_records(csv: &str) -> Result<Vec<Vec<String>>, String> {
    let mut records = Vec::new();
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut closed_quote = false;
    let mut chars = csv.chars().peekable();
    while let Some(c) = chars.next() {
        if closed_quote {
            match c {
                ',' => {
                    fields.push(std::mem::take(&mut field));
                    closed_quote = false;
                }
                '\n' => {
                    fields.push(std::mem::take(&mut field));
                    records.push(std::mem::take(&mut fields));
                    closed_quote = false;
                }
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    fields.push(std::mem::take(&mut field));
                    records.push(std::mem::take(&mut fields));
                    closed_quote = false;
                }
                value if value.is_whitespace() => {}
                _ => return Err("CSV has characters after a closing quote".into()),
            }
            continue;
        }
        match c {
            '"' => {
                if in_quotes && chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else if in_quotes {
                    in_quotes = false;
                    closed_quote = true;
                } else if field.is_empty() {
                    in_quotes = true;
                } else {
                    return Err("CSV has an unexpected quote in an unquoted field".into());
                }
            }
            ',' if !in_quotes => {
                fields.push(std::mem::take(&mut field));
            }
            '\n' if !in_quotes => {
                fields.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut fields));
            }
            '\r' if !in_quotes => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                fields.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut fields));
            }
            _ => field.push(c),
        }
    }
    if in_quotes {
        return Err("CSV has an unterminated quoted field".into());
    }
    if closed_quote || !field.is_empty() || !fields.is_empty() {
        fields.push(field);
        records.push(fields);
    }
    Ok(records)
}

/// Resolve a header name to a canonical column, tolerating the aliases providers use.
fn canonical_column(header: &str) -> Option<&'static str> {
    // Normalize: lowercase, drop parens/`$`, spaces & dashes → `_`, collapse repeats, trim `_`.
    let mut h = String::new();
    for c in header
        .trim()
        .trim_start_matches('\u{feff}')
        .to_ascii_lowercase()
        .chars()
    {
        match c {
            '(' | ')' | '$' => {}
            ' ' | '-' | '_' => {
                if !h.ends_with('_') {
                    h.push('_');
                }
            }
            _ => h.push(c),
        }
    }
    let h = h.trim_matches('_');
    match h {
        "model" | "model_id" | "model_name" | "sku" | "line_item" => Some("model"),
        "cost" | "amount" | "amount_usd" | "usd" | "cost_usd" | "total" | "total_usd" | "spend" => {
            Some("cost")
        }
        "input_tokens" | "input" | "prompt_tokens" | "in_tokens" | "context_tokens" => {
            Some("input_tokens")
        }
        "output_tokens" | "output" | "completion_tokens" | "out_tokens" | "generated_tokens" => {
            Some("output_tokens")
        }
        _ => None,
    }
}

/// Parse a dollar amount (possibly `$1,234.56`, a negative credit, or accounting parentheses) into
/// exact integer micro-USD. More than six decimal places round half away from zero.
fn dollars_to_micros(raw: &str) -> Option<i64> {
    let mut raw = raw.trim().trim_matches('"');
    let parenthesized = raw.starts_with('(') && raw.ends_with(')');
    if parenthesized {
        raw = raw.get(1..raw.len().checked_sub(1)?)?;
    }
    let mut cleaned: String = raw
        .chars()
        .filter(|c| !matches!(c, '$' | ',' | '"') && !c.is_whitespace())
        .collect();
    if cleaned.is_empty() {
        return None;
    }
    let explicit_negative = cleaned.starts_with('-');
    if matches!(cleaned.as_bytes().first(), Some(b'-' | b'+')) {
        cleaned.remove(0);
    }
    if cleaned.is_empty() || (parenthesized && explicit_negative) {
        return None;
    }
    let mut parts = cleaned.split('.');
    let whole = parts.next()?;
    let fraction = parts.next().unwrap_or("");
    if parts.next().is_some()
        || (whole.is_empty() && fraction.is_empty())
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let whole = if whole.is_empty() {
        0
    } else {
        whole.parse::<i128>().ok()?
    };
    let mut micros = whole.checked_mul(1_000_000)?;
    let mut fractional_micros = 0i128;
    for index in 0..6 {
        fractional_micros *= 10;
        fractional_micros +=
            i128::from(fraction.as_bytes().get(index).copied().unwrap_or(b'0') - b'0');
    }
    micros = micros.checked_add(fractional_micros)?;
    if fraction
        .as_bytes()
        .get(6)
        .is_some_and(|digit| *digit >= b'5')
    {
        micros = micros.checked_add(1)?;
    }
    if parenthesized || explicit_negative {
        micros = micros.checked_neg()?;
    }
    i64::try_from(micros).ok()
}

fn token_count(raw: &str) -> Option<u64> {
    let cleaned: String = raw
        .trim()
        .trim_matches('"')
        .chars()
        .filter(|c| *c != ',' && !c.is_whitespace())
        .collect();
    if cleaned.is_empty() {
        Some(0)
    } else if cleaned.bytes().all(|byte| byte.is_ascii_digit()) {
        cleaned.parse().ok()
    } else {
        None
    }
}

/// Parse a provider invoice/usage CSV into per-model [`InvoiceLine`]s. Returns an error only when the
/// header lacks required columns or a populated numeric cell is malformed. Unknown columns, blank
/// lines, and rows without a model are ignored; malformed money is never silently turned into $0.
pub fn parse_invoice_csv(csv: &str) -> Result<Vec<InvoiceLine>, String> {
    let mut records = parse_csv_records(csv)?
        .into_iter()
        .filter(|record| record.iter().any(|field| !field.trim().is_empty()));
    let header = records.next().ok_or("empty CSV (no header row)")?;
    let cols: Vec<Option<&'static str>> = header.iter().map(|h| canonical_column(h)).collect();
    for name in ["model", "cost", "input_tokens", "output_tokens"] {
        if cols.iter().filter(|column| **column == Some(name)).count() > 1 {
            return Err(format!("CSV header has duplicate `{name}` columns"));
        }
    }
    let idx_of = |name: &str| cols.iter().position(|c| *c == Some(name));
    let model_i = idx_of("model").ok_or("CSV header has no recognizable `model` column")?;
    let cost_i = idx_of("cost").ok_or("CSV header has no recognizable cost/amount column")?;
    let in_i = idx_of("input_tokens");
    let out_i = idx_of("output_tokens");

    let mut by_model: BTreeMap<String, InvoiceLine> = BTreeMap::new();
    for (record_index, f) in records.enumerate() {
        let row_number = record_index + 2;
        let Some(model) = f.get(model_i) else {
            continue;
        };
        let model = model.trim().to_string();
        if model.is_empty() {
            continue;
        }
        if model.chars().count() > 256 {
            return Err(format!("CSV row {row_number} model exceeds 256 characters"));
        }
        let cost = f
            .get(cost_i)
            .and_then(|s| dollars_to_micros(s))
            .ok_or_else(|| format!("CSV row {row_number} has an invalid cost"))?;
        let input = in_i
            .and_then(|i| f.get(i))
            .map(|value| {
                token_count(value)
                    .ok_or_else(|| format!("CSV row {row_number} has invalid input tokens"))
            })
            .transpose()?
            .unwrap_or(0);
        let output = out_i
            .and_then(|i| f.get(i))
            .map(|value| {
                token_count(value)
                    .ok_or_else(|| format!("CSV row {row_number} has invalid output tokens"))
            })
            .transpose()?
            .unwrap_or(0);
        let e = by_model.entry(model.clone()).or_insert(InvoiceLine {
            model,
            cost_micros: 0,
            input_tokens: 0,
            output_tokens: 0,
        });
        e.cost_micros = e
            .cost_micros
            .checked_add(cost)
            .ok_or_else(|| format!("CSV row {row_number} overflows the model cost total"))?;
        e.input_tokens = e
            .input_tokens
            .checked_add(input)
            .ok_or_else(|| format!("CSV row {row_number} overflows the input-token total"))?;
        e.output_tokens = e
            .output_tokens
            .checked_add(output)
            .ok_or_else(|| format!("CSV row {row_number} overflows the output-token total"))?;
    }
    Ok(by_model.into_values().collect())
}

/// Normalize a model name for fuzzy invoice matching: lowercase, keep only [a-z0-9].
/// "Claude 3.5 Sonnet" and "claude-3-5-sonnet-20241022" both normalize to a common prefix
/// ("claude35sonnet"), so an invoice SKU/line-item name lines up with the API id family. Deterministic.
fn normalize_model(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Merge two match kinds for a key, preferring the higher confidence (exact > fuzzy > none).
fn stronger_match(a: &'static str, b: &'static str) -> &'static str {
    match (a, b) {
        _ if a == "exact" || b == "exact" => "exact",
        _ if a == "fuzzy" || b == "fuzzy" => "fuzzy",
        _ => "none",
    }
}

/// Reconcile parsed invoice lines against Tare's per-model estimated micro-USD (e.g. from
/// `rollup(.., RollupDim::Model)`). Pure, integer, deterministic. Invoice model names are aligned to
/// estimate keys by exact name, else an UNAMBIGUOUS normalized-name match so real
/// provider SKU names don't produce false coverage gaps; ambiguous / unmatched names stay explicit
/// gaps, and every reconciled row is tagged `exact`/`fuzzy` so a fuzzy alignment stays visible.
pub fn reconcile(
    invoice: &[InvoiceLine],
    estimated_by_model: &BTreeMap<String, i64>,
) -> ReconReport {
    // Normalized index of estimate keys, for fuzzy resolution.
    let est_norm: Vec<(String, &str)> = estimated_by_model
        .keys()
        .map(|k| (normalize_model(k), k.as_str()))
        .collect();
    // Resolve an invoice model name → (estimate key it reconciles under, match kind).
    let resolve = |inv_model: &str| -> (String, &'static str) {
        if estimated_by_model.contains_key(inv_model) {
            return (inv_model.to_string(), "exact");
        }
        let ni = normalize_model(inv_model);
        // Very short family labels ("GPT", "Claude") are not specific enough even when the
        // current estimate happens to contain only one candidate.
        if ni.len() >= 8 {
            let cands: Vec<&str> = est_norm
                .iter()
                .filter(|(ne, _)| ne.starts_with(ni.as_str()) || ni.starts_with(ne.as_str()))
                .map(|(_, k)| *k)
                .collect();
            if cands.len() == 1 {
                return (cands[0].to_string(), "fuzzy"); // unambiguous only — else keep it a gap
            }
        }
        (inv_model.to_string(), "none")
    };

    // Aggregate invoice cost under the resolved key, tracking the strongest match kind per key.
    let mut inv_by_key: BTreeMap<String, i64> = BTreeMap::new();
    let mut match_of_key: BTreeMap<String, &'static str> = BTreeMap::new();
    for l in invoice {
        let (key, kind) = resolve(&l.model);
        let slot = inv_by_key.entry(key.clone()).or_insert(0);
        *slot = slot.saturating_add(l.cost_micros);
        let cur = match_of_key.get(&key).copied().unwrap_or("none");
        match_of_key.insert(key, stronger_match(cur, kind));
    }

    // Union of resolved invoice keys + estimate keys.
    let mut models: BTreeMap<&str, ()> = BTreeMap::new();
    for k in inv_by_key.keys() {
        models.insert(k.as_str(), ());
    }
    for m in estimated_by_model.keys() {
        models.insert(m.as_str(), ());
    }

    let mut rows = Vec::new();
    let (mut inv_total, mut est_total) = (0i64, 0i64);
    let mut uncovered = Vec::new();
    let mut unbilled = Vec::new();
    for model in models.keys() {
        let invoice_micros = inv_by_key.get(*model).copied().unwrap_or(0);
        let estimated_micros = estimated_by_model.get(*model).copied().unwrap_or(0);
        let on_invoice = inv_by_key.contains_key(*model);
        let on_estimate = estimated_by_model.contains_key(*model);
        inv_total = inv_total.saturating_add(invoice_micros);
        est_total = est_total.saturating_add(estimated_micros);
        let delta = estimated_micros.saturating_sub(invoice_micros);
        let delta_bps = if invoice_micros != 0 {
            // Clamp into i64 rather than a truncating `as` (module's saturating discipline).
            (delta as i128 * 10_000 / invoice_micros as i128)
                .clamp(i64::MIN as i128, i64::MAX as i128) as i64
        } else {
            0
        };
        if on_invoice && !on_estimate {
            uncovered.push(model.to_string());
        }
        if on_estimate && !on_invoice {
            unbilled.push(model.to_string());
        }
        // Two-sided rows carry their match kind; one-sided rows (pure gap) are `none`.
        let matched = if on_invoice && on_estimate {
            match_of_key.get(*model).copied().unwrap_or("exact")
        } else {
            "none"
        };
        rows.push(ReconRow {
            model: model.to_string(),
            invoice_micros,
            estimated_micros,
            delta_micros: delta,
            delta_bps,
            on_invoice,
            on_estimate,
            matched: matched.to_string(),
        });
    }
    rows.sort_by(|a, b| {
        b.delta_micros
            .unsigned_abs()
            .cmp(&a.delta_micros.unsigned_abs())
            .then(a.model.cmp(&b.model))
    });
    ReconReport {
        rows,
        invoice_total_micros: inv_total,
        estimated_total_micros: est_total,
        delta_micros: est_total.saturating_sub(inv_total),
        uncovered_invoice_models: uncovered,
        unbilled_estimate_models: unbilled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_tolerant_csv_with_aliases_and_sums_by_model() {
        let csv = "\
Model,Amount (USD),Input Tokens,Output Tokens
claude-opus-4-8,\"$1,234.50\",1000000,200000
claude-opus-4-8,10.00,5000,1000
gpt-5,\"$99.99\",300000,50000
";
        let lines = parse_invoice_csv(csv).unwrap();
        assert_eq!(lines.len(), 2);
        let opus = lines.iter().find(|l| l.model == "claude-opus-4-8").unwrap();
        // 1234.50 + 10.00 = 1244.50 -> 1_244_500_000 micro-USD; tokens summed.
        assert_eq!(opus.cost_micros, 1_244_500_000);
        assert_eq!(opus.input_tokens, 1_005_000);
        assert_eq!(opus.output_tokens, 201_000);
        let gpt = lines.iter().find(|l| l.model == "gpt-5").unwrap();
        assert_eq!(gpt.cost_micros, 99_990_000);
    }

    #[test]
    fn missing_required_columns_is_an_error() {
        assert!(parse_invoice_csv("date,notes\n2026-07-01,hi").is_err());
        assert!(parse_invoice_csv("").is_err());
    }

    #[test]
    fn malformed_invoice_values_fail_instead_of_becoming_zero() {
        for csv in [
            "model,cost\nm,not-money\n",
            "model,cost\nm,1e100\n",
            "model,cost,input_tokens\nm,1.00,nope\n",
            "model,cost\n\"unterminated,1.00\n",
            "model,cost,total\nm,1.00,1.00\n",
        ] {
            assert!(
                parse_invoice_csv(csv).is_err(),
                "unexpectedly parsed {csv:?}"
            );
        }
    }

    #[test]
    fn parses_money_exactly_with_rounding_credits_and_multiline_fields() {
        let csv = "\u{feff}model,cost,input_tokens\n\"model\nlabel\",0.0000005,\"1,000\"\ncredit,($1.25),0\n";
        let lines = parse_invoice_csv(csv).unwrap();
        let model = lines
            .iter()
            .find(|line| line.model == "model\nlabel")
            .unwrap();
        assert_eq!(model.cost_micros, 1);
        assert_eq!(model.input_tokens, 1_000);
        assert_eq!(
            lines
                .iter()
                .find(|line| line.model == "credit")
                .unwrap()
                .cost_micros,
            -1_250_000
        );
    }

    #[test]
    fn reconciles_delta_and_coverage_gaps() {
        let invoice = vec![
            InvoiceLine {
                model: "claude-opus-4-8".into(),
                cost_micros: 1_000_000, // $1.00 billed
                input_tokens: 0,
                output_tokens: 0,
            },
            InvoiceLine {
                model: "gpt-5".into(),
                cost_micros: 500_000, // billed but never captured -> coverage gap
                input_tokens: 0,
                output_tokens: 0,
            },
        ];
        let mut est = BTreeMap::new();
        est.insert("claude-opus-4-8".to_string(), 1_100_000i64); // Tare estimates $1.10
        est.insert("ollama/llama3".to_string(), 42_000i64); // local overlay, never billed

        let r = reconcile(&invoice, &est);
        assert_eq!(r.invoice_total_micros, 1_500_000);
        assert_eq!(r.estimated_total_micros, 1_142_000);
        assert_eq!(r.delta_micros, -358_000);
        // Opus: estimate 1_100_000 − invoice 1_000_000 = +100_000 = +1000 bps (+10%).
        let opus = r
            .rows
            .iter()
            .find(|x| x.model == "claude-opus-4-8")
            .unwrap();
        assert_eq!(opus.delta_micros, 100_000);
        assert_eq!(opus.delta_bps, 1_000);
        assert!(opus.on_invoice && opus.on_estimate);
        // Coverage gaps.
        assert_eq!(r.uncovered_invoice_models, vec!["gpt-5".to_string()]);
        assert_eq!(
            r.unbilled_estimate_models,
            vec!["ollama/llama3".to_string()]
        );
        // Biggest absolute delta first (gpt-5's −500_000 beats opus's +100_000).
        assert_eq!(r.rows[0].model, "gpt-5");
    }

    fn line(model: &str, cost: i64) -> InvoiceLine {
        InvoiceLine {
            model: model.into(),
            cost_micros: cost,
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    #[test]
    fn fuzzy_matches_invoice_sku_names_to_dated_model_ids() {
        // A real provider SKU ("Claude 3.5 Sonnet") lines up with the dated API id via normalized
        // prefix — one reconciled row, not two false gaps.
        let invoice = vec![line("Claude 3.5 Sonnet", 900_000)];
        let mut est = BTreeMap::new();
        est.insert("claude-3-5-sonnet-20241022".to_string(), 1_000_000i64);
        let r = reconcile(&invoice, &est);
        assert_eq!(r.rows.len(), 1);
        let row = &r.rows[0];
        assert_eq!(row.model, "claude-3-5-sonnet-20241022");
        assert_eq!(row.matched, "fuzzy");
        assert!(row.on_invoice && row.on_estimate);
        assert_eq!(row.invoice_micros, 900_000);
        assert_eq!(row.estimated_micros, 1_000_000);
        assert_eq!(row.delta_micros, 100_000);
        assert!(r.uncovered_invoice_models.is_empty());
        assert!(r.unbilled_estimate_models.is_empty());
    }

    #[test]
    fn ambiguous_fuzzy_stays_an_explicit_gap() {
        // "Claude" prefixes multiple ids → ambiguous → NOT fuzzy-matched; stays an honest gap.
        let invoice = vec![line("Claude", 500_000)];
        let mut est = BTreeMap::new();
        est.insert("claude-opus-4-8".to_string(), 100_000i64);
        est.insert("claude-3-5-sonnet-20241022".to_string(), 200_000i64);
        let r = reconcile(&invoice, &est);
        assert!(r.uncovered_invoice_models.contains(&"Claude".to_string()));
        assert_eq!(r.unbilled_estimate_models.len(), 2);
        assert!(r.rows.iter().all(|x| x.matched != "fuzzy"));

        let one_estimate = BTreeMap::from([("claude-opus-4-8".to_string(), 100_000)]);
        let still_too_vague = reconcile(&[line("Claude", 50_000)], &one_estimate);
        assert_eq!(still_too_vague.rows.len(), 2);
        assert!(still_too_vague.rows.iter().all(|row| row.matched == "none"));
    }

    #[test]
    fn exact_name_is_tagged_exact() {
        let invoice = vec![line("gpt-4o", 300_000)];
        let mut est = BTreeMap::new();
        est.insert("gpt-4o".to_string(), 320_000i64);
        let r = reconcile(&invoice, &est);
        let row = r.rows.iter().find(|x| x.model == "gpt-4o").unwrap();
        assert_eq!(row.matched, "exact");
    }

    #[test]
    fn extreme_signed_deltas_sort_without_panicking() {
        let invoice = vec![line("normal", 1)];
        let estimated =
            BTreeMap::from([("minimum".to_string(), i64::MIN), ("normal".to_string(), 0)]);
        let report = reconcile(&invoice, &estimated);
        assert_eq!(report.rows[0].model, "minimum");
        assert_eq!(report.rows[0].delta_micros, i64::MIN);
    }
}
