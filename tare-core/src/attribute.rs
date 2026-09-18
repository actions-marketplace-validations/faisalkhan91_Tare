//! Attribution: turn runs into a ranked "trim here -> save $X" report.
//! Pure function of step records + pricing. No clock/RNG/map-iteration dependence.
//! Causes: retry-loop, verbose-tool-output, bloated-system-prompt, cache-read-vs-write.
//!
//! Cause precedence (so rows account DISJOINT token sets and `Σ rows.micros ≤ total`):
//! redundant retry steps are attributed wholly to `retry-loop`; the other three causes
//! are computed only over the surviving (non-redundant) steps, and operate on disjoint
//! token classes (system-fresh / tool_result-fresh / cache-write).

use crate::account::{allocate_step, cost_usage};
use crate::model::{CacheClass, Component, RunRecord, StepRecord, TodaySpend};
use crate::money::MicroUsd;
use crate::pricing::{ModelRates, PricingTable};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrimRow {
    pub cause: String,
    pub detail: String,
    pub tokens: u64,
    /// Estimated dollars (micro-USD) currently spent on this cause.
    pub micros: i64,
    /// Estimated dollars (micro-USD) that could be saved by trimming.
    pub projected_saved_micros: i64,
}

/// A (provider, model) the pricing table can't price — so its tokens are NOT in `total_micros`.
/// Surfacing this prevents a new or renamed model from silently making spend look like approximately $0.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnpricedModel {
    pub provider: String,
    pub model: String,
    pub token_total: u64,
    pub step_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub pricing_version: String,
    pub effective_date: String,
    /// Every dollar figure here is ESTIMATED, never billed/actual.
    pub estimated: bool,
    pub total_micros: i64,
    pub rows: Vec<TrimRow>,
    /// Models with no bundled price: their tokens are EXCLUDED from `total_micros`. Empty (and
    /// omitted from JSON) in the normal case, so existing goldens are unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unpriced: Vec<UnpricedModel>,
    /// Privacy policy under which this report was produced. Omitted from JSON when unset
    /// (so `build_report` output — and its goldens — are unchanged); the CLI stamps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy_policy_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// `coarse` when a MAJORITY of priced spend was captured out-of-band (OTel/log) without
    /// prompt-component weights/system-hash — so the component-level causes (bloated-system-prompt,
    /// verbose-tool-output) under-attribute and the report carries a `coarse-attribution` row
    /// instead of reading as a misleading empty "nothing to optimize". `None` = exact (omitted from
    /// JSON, so exact-capture goldens are unchanged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution_confidence: Option<String>,
}

/// Collect the (provider, model) pairs that `pricing` can't price, with their token totals.
/// Deterministic through BTreeMap key order; independent of the process-global warn-once set.
pub fn unpriced_models(runs: &[RunRecord], pricing: &PricingTable) -> Vec<UnpricedModel> {
    unpriced_models_dated(runs, pricing, &BTreeMap::new())
}

/// As [`unpriced_models`], but pricing each run as-of its capture day from `day_by_run` (run_id →
/// `YYYY-MM-DD`) — so a model unpriced at a run's contemporaneous edition is detected correctly on a
/// multi-edition table. Empty map → byte-identical to the plain lookup.
pub fn unpriced_models_dated(
    runs: &[RunRecord],
    pricing: &PricingTable,
    day_by_run: &BTreeMap<String, String>,
) -> Vec<UnpricedModel> {
    let mut by_model: BTreeMap<(String, String), (u64, u32)> = BTreeMap::new();
    for run in runs {
        let on = day_by_run.get(&run.run_id).map(String::as_str);
        for step in &run.steps {
            if rates_for(step, pricing, on).is_none() {
                let e = by_model
                    .entry((step.provider.as_str().to_string(), step.model.clone()))
                    .or_insert((0, 0));
                e.0 =
                    e.0.saturating_add(step.usage.total())
                        .saturating_add(step.usage.audio_input)
                        .saturating_add(step.usage.audio_output);
                e.1 = e.1.saturating_add(1);
            }
        }
    }
    by_model
        .into_iter()
        .map(
            |((provider, model), (token_total, step_count))| UnpricedModel {
                provider,
                model,
                token_total,
                step_count,
            },
        )
        .collect()
}

impl Report {
    /// Stamp the active privacy policy onto the report (for the `tare report` footer).
    pub fn with_privacy(mut self, policy: &crate::privacy::PrivacyPolicy) -> Self {
        self.privacy_policy_id = Some(policy.policy_id());
        self.profile = Some(policy.effective_profile().as_str().to_string());
        self
    }
}

/// Rates for a step, optionally as-of a date: `on = Some(day)` selects the pricing
/// edition in effect on the run's capture day (per-run AsOf for multi-edition tables) via
/// `lookup_as_of`, which preserves the override/overlay/suffix chain; `on = None` is the plain
/// first-row lookup (byte-identical to before).
fn rates_for<'a>(
    step: &StepRecord,
    pricing: &'a PricingTable,
    on: Option<&str>,
) -> Option<&'a ModelRates> {
    pricing.lookup_as_of(step.provider, step.shape.vendor.as_deref(), &step.model, on)
}

fn system_fresh_tokens(step: &StepRecord, rates: &ModelRates) -> u64 {
    allocate_step(&step.usage, rates, &step.shape)
        .iter()
        .filter(|a| a.component == Component::System && a.class == CacheClass::Fresh)
        .map(|a| a.tokens)
        .fold(0u64, u64::saturating_add)
}

fn tool_result_fresh(step: &StepRecord, rates: &ModelRates) -> (u64, i64) {
    allocate_step(&step.usage, rates, &step.shape)
        .iter()
        // Fresh-class only (mirrors system_fresh_tokens): a cached tool-result would otherwise be
        // counted here AND in the cache-read/write row — a double-count.
        .filter(|a| a.component == Component::ToolResult && a.class == CacheClass::Fresh)
        .fold((0u64, 0i64), |(tokens, micros), allocation| {
            (
                tokens.saturating_add(allocation.tokens),
                micros.saturating_add(allocation.micros.micros()),
            )
        })
}

pub fn total_micros(runs: &[RunRecord], pricing: &PricingTable) -> i64 {
    total_micros_dated(runs, pricing, &BTreeMap::new())
}

/// Estimated micro-USD for ONE step, priced as-of `on` (its capture day for effective-dated pricing,
/// or `None` for the latest edition). Vendor- and date-aware (reuses the same rating path as
/// `total_micros_dated`). Unpriced `(provider, model)` -> 0. Exposed for the cohort resolve engine,
/// which sums scoped per-step contributions.
pub fn step_micros(step: &StepRecord, pricing: &PricingTable, on: Option<&str>) -> i64 {
    rates_for(step, pricing, on)
        .map(|r| cost_usage(&step.usage, r, &step.shape).total.micros())
        .unwrap_or(0)
}

/// As [`total_micros`], but pricing each run as-of its capture day from `day_by_run`.
/// Empty map → byte-identical. Used by `build_report_dated` so the report total reconciles with its
/// per-run-AsOf causes.
pub fn total_micros_dated(
    runs: &[RunRecord],
    pricing: &PricingTable,
    day_by_run: &BTreeMap<String, String>,
) -> i64 {
    let mut total = 0i64;
    for run in runs {
        let on = day_by_run.get(&run.run_id).map(String::as_str);
        for step in &run.steps {
            if let Some(r) = rates_for(step, pricing, on) {
                total =
                    total.saturating_add(cost_usage(&step.usage, r, &step.shape).total.micros());
            }
        }
    }
    total
}

pub fn today_spend(runs: &[RunRecord], pricing: &PricingTable) -> TodaySpend {
    TodaySpend {
        run_count: u32::try_from(runs.len()).unwrap_or(u32::MAX),
        total_micros: total_micros(runs, pricing),
        pricing_version: pricing.version.clone(),
        effective_date: pricing.effective_date.clone(),
    }
}

pub fn build_report(runs: &[RunRecord], pricing: &PricingTable) -> Report {
    build_report_dated(runs, pricing, &BTreeMap::new())
}

/// As [`build_report`], but pricing each run as-of its capture day from `day_by_run` (run_id →
/// `YYYY-MM-DD`), so a multi-edition table reprices each run at its contemporaneous edition — honest
/// per-run AsOf. ALL rate lookups here (the cause loops, the total via
/// `total_micros_dated`, and the degraded-attribution fallback) consult the same `on` date, so the
/// total reconciles with the causes. An empty map → byte-identical to the plain report.
pub fn build_report_dated(
    runs: &[RunRecord],
    pricing: &PricingTable,
    day_by_run: &BTreeMap<String, String>,
) -> Report {
    let mut retry_tokens = 0u64;
    let mut retry_micros = 0i64;
    let mut retry_count = 0u64;
    let mut retry_after_failure = 0u64;

    let mut bloat_tokens = 0u64;
    let mut bloat_micros = 0i64;
    let mut bloat_saved = 0i64;

    let mut verbose_tokens = 0u64;
    let mut verbose_micros = 0i64;

    // cache-read-vs-write tracks only the WASTED premium (writes never read back), so the
    // row is an additive trim-target rather than the full — partly productive — write cost.
    let mut wasted_write_tokens = 0u64;
    let mut cache_saved = 0i64;

    for run in runs {
        // As-of date for this run: every rate lookup below uses it, so causes reprice at
        // the run's contemporaneous edition. `None` (empty map) → plain first-row lookup.
        let on = day_by_run.get(&run.run_id).map(String::as_str);
        // Order by step_ordinal WITHOUT deep-cloning every StepRecord: sort borrowed
        // refs instead. `.copied()` below hands each loop a `&StepRecord`, identical to before.
        let mut ordered: Vec<&StepRecord> = run.steps.iter().collect();
        ordered.sort_by_key(|s| s.step_ordinal);

        // --- retry-loop: identical request_hash issued more than once ---
        // The 2nd+ occurrence is "redundant"; its WHOLE cost is the retry-loop spend, and
        // it is excluded from the other causes so accounting stays disjoint.
        // Track the previous step per request hash so a re-issue can consult whether the
        // PRIOR response actually failed (clear retry waste) vs succeeded (maybe best-of-N).
        let mut last: BTreeMap<u64, &StepRecord> = BTreeMap::new();
        let mut redundant: BTreeSet<u32> = BTreeSet::new();
        for step in ordered.iter().copied() {
            let Some(rates) = rates_for(step, pricing, on) else {
                continue;
            };
            // No request hash (max_private profile) -> retry detection unavailable for this
            // step; it can neither be a retry nor anchor one.
            let Some(hash) = step.shape.request_hash else {
                continue;
            };
            if let Some(prev) = last.get(&hash) {
                redundant.insert(step.step_ordinal);
                retry_count = retry_count.saturating_add(1);
                if prev.is_retry_worthy_failure() {
                    retry_after_failure = retry_after_failure.saturating_add(1);
                }
                retry_tokens = retry_tokens.saturating_add(step.usage.total());
                retry_micros = retry_micros
                    .saturating_add(cost_usage(&step.usage, rates, &step.shape).total.micros());
            }
            last.insert(hash, step);
        }
        let survivors = || {
            ordered
                .iter()
                .copied()
                .filter(|s| !redundant.contains(&s.step_ordinal))
        };

        // --- bloated-system-prompt: identical uncached system re-sent across steps ---
        // Group by (model, system content hash) — NOT byte length — among survivors
        // without cache_control. Repeats beyond the first are cacheable.
        let mut groups: BTreeMap<(String, u64), Vec<&StepRecord>> = BTreeMap::new();
        for step in survivors() {
            if step.shape.has_cache_control {
                continue;
            }
            let Some(hash) = step.shape.system_hash else {
                continue;
            };
            groups
                .entry((step.model.clone(), hash))
                .or_default()
                .push(step);
        }
        for (_key, steps) in groups {
            if steps.len() < 2 {
                continue;
            }
            for step in steps.iter().skip(1) {
                let Some(rates) = rates_for(step, pricing, on) else {
                    continue;
                };
                let sys_tok = system_fresh_tokens(step, rates);
                if sys_tok == 0 {
                    continue;
                }
                let fresh = MicroUsd::for_tokens(sys_tok, rates.micro_per_mtok(CacheClass::Fresh));
                let read =
                    MicroUsd::for_tokens(sys_tok, rates.micro_per_mtok(CacheClass::CacheRead));
                bloat_tokens = bloat_tokens.saturating_add(sys_tok);
                bloat_micros = bloat_micros.saturating_add(fresh.micros());
                bloat_saved =
                    bloat_saved.saturating_add(fresh.micros().saturating_sub(read.micros()));
            }
        }

        // --- verbose-tool-output: tool_result input tokens (survivors only) ---
        for step in survivors() {
            if let Some(rates) = rates_for(step, pricing, on) {
                let (t, m) = tool_result_fresh(step, rates);
                verbose_tokens = verbose_tokens.saturating_add(t);
                verbose_micros = verbose_micros.saturating_add(m);
            }
        }

        // --- cache-read-vs-write: writes never read back are wasted premium ---
        let mut run_write = 0u64;
        let mut run_read = 0u64;
        // (tokens, total write cost, fresh-equivalent cost) per write step.
        let mut write_steps: Vec<(u64, i64, i64)> = Vec::new();
        for step in survivors() {
            let Some(rates) = rates_for(step, pricing, on) else {
                continue;
            };
            run_read = run_read.saturating_add(step.usage.cache_read);
            let cw = step.usage.cache_write();
            if cw > 0 {
                let write_cost = MicroUsd::for_tokens(
                    step.usage.cache_write_5m,
                    rates.micro_per_mtok(CacheClass::CacheWrite5m),
                )
                .micros()
                .saturating_add(
                    MicroUsd::for_tokens(
                        step.usage.cache_write_1h,
                        rates.micro_per_mtok(CacheClass::CacheWrite1h),
                    )
                    .micros(),
                );
                let fresh_equiv =
                    MicroUsd::for_tokens(cw, rates.micro_per_mtok(CacheClass::Fresh)).micros();
                run_write = run_write.saturating_add(cw);
                write_steps.push((cw, write_cost, fresh_equiv));
            }
        }
        let wasted = run_write.saturating_sub(run_read);
        if wasted > 0 {
            wasted_write_tokens = wasted_write_tokens.saturating_add(wasted);
            let weights: Vec<u64> = write_steps.iter().map(|(cw, _, _)| *cw).collect();
            let split = crate::account::apportion(wasted, &weights);
            for ((cw, write_cost, fresh_equiv), wt) in write_steps.iter().zip(split) {
                if wt == 0 || *cw == 0 {
                    continue;
                }
                // Premium paid on this step's writes, prorated to the wasted (unread) tokens.
                // Clamp at 0: defends against a (misconfigured) write rate below the input rate.
                let premium = write_cost.saturating_sub(*fresh_equiv).max(0);
                let prorated = ((premium as i128) * (wt as i128) / (*cw as i128)) as i64;
                cache_saved = cache_saved.saturating_add(prorated);
            }
        }
    }

    let mut rows: Vec<TrimRow> = Vec::new();
    if retry_micros > 0 || retry_tokens > 0 {
        rows.push(TrimRow {
            cause: "retry-loop".into(),
            detail: format!(
                "{retry_count} identical request(s) re-issued ({retry_after_failure} after a failed/declined response); \
                 re-issues after success may be intentional (best-of-N)."
            ),
            tokens: retry_tokens,
            micros: retry_micros,
            projected_saved_micros: retry_micros,
        });
    }
    if bloat_micros > 0 {
        rows.push(TrimRow {
            cause: "bloated-system-prompt".into(),
            detail: "Identical large system prompt re-sent uncached across steps; cache the stable prefix.".into(),
            tokens: bloat_tokens,
            micros: bloat_micros,
            projected_saved_micros: bloat_saved,
        });
    }
    if verbose_micros > 0 {
        rows.push(TrimRow {
            cause: "verbose-tool-output".into(),
            detail: "Tool-result payloads dominate input tokens; trim or summarize them.".into(),
            tokens: verbose_tokens,
            micros: verbose_micros,
            projected_saved_micros: verbose_micros,
        });
    }
    if cache_saved > 0 {
        rows.push(TrimRow {
            cause: "cache-read-vs-write".into(),
            detail: "Cache writes never read back; the write premium was paid for nothing.".into(),
            tokens: wasted_write_tokens,
            micros: cache_saved,
            projected_saved_micros: cache_saved,
        });
    }

    let total = total_micros_dated(runs, pricing, day_by_run);
    // Invariant #10: the disjoint CAUSE rows account disjoint token sets, so they never exceed the
    // total. (Asserted before the coarse-attribution summary row, which overlaps by design.)
    let rows_sum = rows
        .iter()
        .fold(0i64, |acc, r| acc.saturating_add(r.micros));
    debug_assert!(
        rows_sum <= total,
        "Σ rows.micros must not exceed total_micros"
    );

    // Degraded-attribution fallback: steps captured out-of-band (OTel/log) carry no
    // prompt-component weights/system-hash, so the component causes above can't see them and the
    // report would read as a misleading empty "nothing to optimize". Sum their priced spend; when
    // it's the MAJORITY, flag the report coarse and add one model-level `coarse-attribution` row
    // (informational — projected_saved 0, excluded from the disjoint assert above).
    let mut degraded_micros = 0i64;
    let mut degraded_tokens = 0u64;
    let mut degraded_steps = 0u32;
    for run in runs {
        let on = day_by_run.get(&run.run_id).map(String::as_str);
        for step in &run.steps {
            if step.shape.system_hash.is_none() && step.shape.weights.is_empty() {
                if let Some(rates) = pricing.lookup_as_of(
                    step.provider,
                    step.shape.vendor.as_deref(),
                    &step.model,
                    on,
                ) {
                    degraded_micros = degraded_micros
                        .saturating_add(cost_usage(&step.usage, rates, &step.shape).total.micros());
                    degraded_tokens = degraded_tokens
                        .saturating_add(step.usage.total())
                        .saturating_add(step.usage.audio_input)
                        .saturating_add(step.usage.audio_output);
                    degraded_steps = degraded_steps.saturating_add(1);
                }
            }
        }
    }
    let coarse = degraded_micros > 0 && degraded_micros.saturating_mul(2) >= total;
    if coarse {
        rows.push(TrimRow {
            cause: "coarse-attribution".into(),
            detail: format!(
                "{degraded_steps} step(s) captured out-of-band (OTel/log) without prompt-component detail — component-level causes can't be computed for them; showing model-level spend only."
            ),
            tokens: degraded_tokens,
            micros: degraded_micros,
            projected_saved_micros: 0,
        });
    }

    rows.sort_by(|a, b| {
        b.projected_saved_micros
            .cmp(&a.projected_saved_micros)
            .then(a.cause.cmp(&b.cause))
    });

    Report {
        pricing_version: pricing.version.clone(),
        effective_date: pricing.effective_date.clone(),
        estimated: true,
        total_micros: total,
        rows,
        unpriced: unpriced_models_dated(runs, pricing, day_by_run),
        privacy_policy_id: None,
        profile: None,
        attribution_confidence: if coarse { Some("coarse".into()) } else { None },
    }
}

#[cfg(test)]
mod dated_pricing_tests {
    use super::*;
    use crate::model::{CacheTtl, Provider, RequestShape, StepRecord, UsageTokens};
    use crate::pricing::PricingTable;
    use std::collections::BTreeMap;

    fn run(run_id: &str, model: &str, input: u64) -> RunRecord {
        RunRecord {
            run_id: run_id.into(),
            steps: vec![StepRecord {
                run_id: run_id.into(),
                step_ordinal: 1,
                provider: Provider::Anthropic,
                model: model.into(),
                usage: UsageTokens {
                    fresh_input: input,
                    ..Default::default()
                },
                shape: RequestShape {
                    model: model.into(),
                    provider: Provider::Anthropic,
                    stream: false,
                    ttl: CacheTtl::FiveMin,
                    has_cache_control: false,
                    cached_component: None,
                    system_hash: None,
                    weights: vec![],
                    request_hash: Some(0),
                    step_label: None,
                    component_label: None,
                    parent_label: None,
                    attempt: None,
                    session: None,
                    workload_key: None,
                    effort: None,
                    mcp_server: None,
                    vendor: None,
                    commit: None,
                    author: None,
                },
                stop_reason: None,
                duration_ms: 0,
                start_unix_nano: None,
                trace_id: None,
                span_id: None,
                parent_span_id: None,
            }],
        }
    }

    fn dated_table() -> PricingTable {
        // "m": $3/Mtok input (base) → $6/Mtok from 2026-07-01.
        PricingTable::from_toml_str(
            r#"
version = "d"
effective_date = "2026-01-01"
[[model]]
provider = "anthropic"
model_id = "m"
input_micro_per_mtok = 3000000
output_micro_per_mtok = 0
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
[[model]]
provider = "anthropic"
model_id = "m"
effective_date = "2026-07-01"
input_micro_per_mtok = 6000000
output_micro_per_mtok = 0
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
"#,
        )
        .unwrap()
    }

    #[test]
    fn build_report_dated_reprices_each_run_at_its_capture_day() {
        let pricing = dated_table();
        let runs = vec![run("old", "m", 1_000_000), run("new", "m", 1_000_000)];
        // Plain report: both runs price at the first-row edition ($3) → 6M total (byte-identical default).
        assert_eq!(build_report(&runs, &pricing).total_micros, 6_000_000);
        // Per-run AsOf: old@June → $3 (3M), new@July → $6 (6M) → 9M total, reconciling with the causes.
        let mut days = BTreeMap::new();
        days.insert("old".to_string(), "2026-06-15".to_string());
        days.insert("new".to_string(), "2026-07-15".to_string());
        assert_eq!(
            build_report_dated(&runs, &pricing, &days).total_micros,
            9_000_000
        );
        // total_micros_dated agrees (the total feeds off the same day map).
        assert_eq!(total_micros_dated(&runs, &pricing, &days), 9_000_000);
    }
}
