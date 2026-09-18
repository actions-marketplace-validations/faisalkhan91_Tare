//! Session rollup: promote the agent TASK above individual runs. A long agent task spans many
//! `api_request` steps (often many runs); grouping by the owning session/conversation id answers
//! "what did this task cost?" as one receipt. Pure function of stored steps + pricing; integer
//! micro-USD; deterministic (BTreeMap order). Steps with no session id fall back to their run id,
//! so every step belongs to exactly one session bucket and totals reconcile with the report.

use crate::account::cost_usage;
use crate::model::{Provider, RequestShape, RunRecord, UsageTokens};
use crate::pricing::PricingTable;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRow {
    pub session: String,
    pub runs: u32,
    pub steps: u32,
    pub tokens: u64,
    pub micros: i64,
    /// Distinct tool (component) labels seen in this session.
    pub tools: u32,
    /// Distinct agent (parent) labels seen in this session.
    pub agents: u32,
    /// Mean micro-USD per step (integer; 0 when no steps).
    pub micros_per_step: i64,
    /// Fresh (uncached) input tokens per step, in turn order — the context-growth curve. A rising
    /// curve means you're paying full price on an ever-larger prefix instead of cache-reading it.
    pub input_curve: Vec<u64>,
    /// First turn (1-based) where the cached prefix stopped being a cache hit (a turn read cache
    /// but a later turn reads none) — the cache-erosion point. `None` if history stayed cached.
    pub cache_erosion_turn: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionReport {
    pub rows: Vec<SessionRow>,
    pub total_micros: i64,
    pub pricing_version: String,
    pub estimated: bool,
}

#[derive(Default)]
struct Bucket {
    runs: BTreeSet<String>,
    steps: u32,
    tokens: u64,
    micros: i64,
    tools: BTreeSet<String>,
    agents: BTreeSet<String>,
    /// (fresh_input, cache_read) per step in encounter order — for the context-growth curve.
    curve: Vec<(u64, u64)>,
}

/// First 1-based turn where a step reads no cache after an earlier step did read cache — i.e. the
/// cached prefix stopped being a hit and the agent is now paying fresh input on the whole history.
fn cache_erosion_turn(curve: &[(u64, u64)]) -> Option<u32> {
    let mut saw_cache = false;
    for i in 0..curve.len() {
        let cache_read = curve[i].1;
        if cache_read > 0 {
            saw_cache = true;
        } else if saw_cache {
            // Flag erosion UNLESS the very next turn recovers: a lone miss that
            // immediately reads cache again is normal churn, not context bloat. But a miss that
            // persists — or ends the session (no next turn) — is genuine erosion, so a last-turn miss
            // still counts (unwrap_or true).
            let recovered_next = curve.get(i + 1).map(|(_, c)| *c > 0).unwrap_or(false);
            if !recovered_next {
                return Some(u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1));
            }
        }
    }
    None
}

/// Aggregate spend by owning session across `runs`. A step's session is its `shape.session`, or
/// its run id when absent (the inline-proxy path has no session concept). Rows sorted by micros
/// desc, then session asc (deterministic).
pub fn sessions(runs: &[RunRecord], pricing: &PricingTable) -> SessionReport {
    let mut buckets: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut total = 0i64;
    for run in runs {
        for step in &run.steps {
            let key = step
                .shape
                .session
                .clone()
                .unwrap_or_else(|| run.run_id.clone());
            let micros = pricing
                .lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
                .map(|r| cost_usage(&step.usage, r, &step.shape).total.micros())
                .unwrap_or(0);
            let b = buckets.entry(key).or_default();
            b.runs.insert(run.run_id.clone());
            b.steps = b.steps.saturating_add(1);
            b.tokens = b.tokens.saturating_add(step.usage.total());
            b.micros = b.micros.saturating_add(micros);
            if let Some(t) = &step.shape.component_label {
                b.tools.insert(t.clone());
            }
            if let Some(a) = &step.shape.parent_label {
                b.agents.insert(a.clone());
            }
            b.curve
                .push((step.usage.fresh_input, step.usage.cache_read));
            total = total.saturating_add(micros);
        }
    }
    let mut rows: Vec<SessionRow> = buckets
        .into_iter()
        .map(|(session, b)| SessionRow {
            session,
            runs: u32::try_from(b.runs.len()).unwrap_or(u32::MAX),
            steps: b.steps,
            tokens: b.tokens,
            micros: b.micros,
            tools: b.tools.len() as u32,
            agents: b.agents.len() as u32,
            micros_per_step: if b.steps > 0 {
                b.micros / b.steps as i64
            } else {
                0
            },
            cache_erosion_turn: cache_erosion_turn(&b.curve),
            input_curve: b.curve.iter().map(|(fresh, _)| *fresh).collect(),
        })
        .collect();
    rows.sort_by(|a, b| b.micros.cmp(&a.micros).then(a.session.cmp(&b.session)));
    SessionReport {
        rows,
        total_micros: total,
        pricing_version: pricing.version.clone(),
        estimated: true,
    }
}

// ---- context-bloat waste: a session that stopped hitting the cache ----

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBloatRow {
    /// Owning session/conversation id (or run id fallback) whose prefix stopped being cache-read.
    pub session: String,
    /// 1-based turn where cache reads collapsed to ~0 after having been cached (erosion point).
    pub erosion_turn: u32,
    /// Fresh-input tokens paid from the erosion turn onward (the prefix being re-sent uncached).
    pub fresh_tokens_after: u64,
    /// Recoverable micro-USD (an UPPER BOUND, projected): every post-erosion fresh-input token
    /// priced at the (fresh − cache_read) rate gap — the money re-caching the prefix would recover,
    /// assuming all that fresh input is re-cacheable stable prefix (genuinely-new content is
    /// included, so this over-estimates). Always >= 0.
    pub micros: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBloatReport {
    pub rows: Vec<ContextBloatRow>,
    pub total_micros: i64,
    pub pricing_version: String,
    pub estimated: bool,
}

/// One wasted-cache-write finding: a cached prefix (`system_hash`) that was WRITTEN to the cache
/// (paying the 1.25×/2× write premium) but never subsequently READ within the captured window — so
/// the premium bought nothing. Measured: both the write and the absence of a read are
/// captured facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WastedWriteRow {
    /// Pricing-provider identity. Cache prefixes are backend- and model-scoped, even when their
    /// captured content hash happens to be identical.
    pub provider: String,
    /// Representative model of the prefix (the write's own model).
    pub model: String,
    /// Cache-write tokens that were never read back.
    pub write_tokens: u64,
    /// Recoverable micro-USD: the write PREMIUM over what those tokens would have cost as fresh
    /// input (`write_cost − fresh_cost`). Dropping `cache_control` on a single-use prefix recovers it.
    pub micros: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WastedWriteReport {
    pub rows: Vec<WastedWriteRow>,
    pub total_micros: i64,
    pub pricing_version: String,
    pub estimated: bool,
}

/// Find cache prefixes that were written but never read. Groups steps by
/// `(provider, model, system_hash)`; a group with cache writes and ZERO cache reads paid the write
/// premium for nothing. The recoverable figure is the write premium over the equivalent fresh-input
/// cost. Pure, integer.
pub fn wasted_cache_write(runs: &[RunRecord], pricing: &PricingTable) -> WastedWriteReport {
    // Per backend/model/prefix: (reads, write_tokens, premium_micros). A content hash alone is not a
    // cache identity: two providers can see the same prompt while maintaining unrelated caches.
    type PrefixKey = (String, String, u64);
    let mut by_prefix: BTreeMap<PrefixKey, (u64, u64, i64)> = BTreeMap::new();
    for run in runs {
        for step in &run.steps {
            let Some(hash) = step.shape.system_hash else {
                continue; // no stable-prefix identity to group on
            };
            let provider = step
                .provider
                .pricing_key(step.shape.vendor.as_deref())
                .into_owned();
            let e = by_prefix
                .entry((provider, step.model.clone(), hash))
                .or_insert((0, 0, 0));
            e.0 = e.0.saturating_add(step.usage.cache_read);
            let writes = step.usage.cache_write(); // saturating add (a corrupt count can't wrap to 0)
            if writes == 0 {
                continue;
            }
            if let Some(r) =
                pricing.lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
            {
                // Premium = what the write actually cost − what those tokens would cost as fresh input.
                let as_write = cost_usage(
                    &UsageTokens {
                        cache_write_5m: step.usage.cache_write_5m,
                        cache_write_1h: step.usage.cache_write_1h,
                        ..Default::default()
                    },
                    r,
                    &step.shape,
                )
                .total
                .micros();
                let as_fresh = cost_usage(
                    &UsageTokens {
                        fresh_input: writes,
                        ..Default::default()
                    },
                    r,
                    &step.shape,
                )
                .total
                .micros();
                e.1 = e.1.saturating_add(writes);
                e.2 = e.2.saturating_add(as_write.saturating_sub(as_fresh).max(0));
            }
        }
    }
    let mut rows: Vec<WastedWriteRow> = by_prefix
        .into_iter()
        .filter(|(_, (reads, writes, premium))| *reads == 0 && *writes > 0 && *premium > 0)
        .map(
            |((provider, model, _), (_, write_tokens, micros))| WastedWriteRow {
                provider,
                model,
                write_tokens,
                micros,
            },
        )
        .collect();
    rows.sort_by(|a, b| {
        b.micros
            .cmp(&a.micros)
            .then(a.provider.cmp(&b.provider))
            .then(a.model.cmp(&b.model))
    });
    let total_micros = rows
        .iter()
        .fold(0i64, |sum, row| sum.saturating_add(row.micros));
    WastedWriteReport {
        rows,
        total_micros,
        pricing_version: pricing.version.clone(),
        estimated: true,
    }
}

/// Detect context-bloat waste: sessions whose prompt prefix WAS being cache-read but then eroded to
/// fresh-input (a rising-prefix / cache-miss regression). For each such session, price the
/// fresh-input tokens paid from the erosion turn onward at the *difference* between the fresh-input
/// and cache-read rates — i.e. the dollars re-caching that stable prefix would recover. Pure
/// function of stored steps + pricing; clock-free; integer micro-USD.
pub fn context_bloat_waste(runs: &[RunRecord], pricing: &PricingTable) -> ContextBloatReport {
    // Group steps per session in encounter order, keeping just what detection + pricing need:
    // (provider, model, fresh_input, cache_read, shape). The shape is carried because cost_usage
    // takes one by reference (it currently ignores it, but we don't rely on that).
    type StepLite = (Provider, String, u64, u64, RequestShape);
    let mut by_session: BTreeMap<String, Vec<StepLite>> = BTreeMap::new();
    for run in runs {
        for step in &run.steps {
            let key = step
                .shape
                .session
                .clone()
                .unwrap_or_else(|| run.run_id.clone());
            by_session.entry(key).or_default().push((
                step.provider,
                step.model.clone(),
                step.usage.fresh_input,
                step.usage.cache_read,
                step.shape.clone(),
            ));
        }
    }

    let mut rows: Vec<ContextBloatRow> = Vec::new();
    for (session, steps) in by_session {
        let curve: Vec<(u64, u64)> = steps.iter().map(|(_, _, f, c, _)| (*f, *c)).collect();
        let Some(turn) = cache_erosion_turn(&curve) else {
            continue;
        };
        let mut micros = 0i64;
        let mut toks = 0u64;
        for (i, (provider, model, fresh, cache_read, shape)) in steps.iter().enumerate() {
            // Count only turns at/after erosion that are STILL uncached. Once
            // cache reads resume (cache_read > 0) the prefix recovered — that turn's fresh input is
            // not erosion waste. The old code priced ALL fresh input after the first miss, even on
            // recovered turns, over-claiming recoverable spend.
            if (i as u32) + 1 < turn || *fresh == 0 || *cache_read > 0 {
                continue;
            }
            if let Some(r) = pricing.lookup(*provider, shape.vendor.as_deref(), model) {
                let as_fresh = cost_usage(
                    &UsageTokens {
                        fresh_input: *fresh,
                        ..Default::default()
                    },
                    r,
                    shape,
                )
                .total
                .micros();
                let as_cache = cost_usage(
                    &UsageTokens {
                        cache_read: *fresh,
                        ..Default::default()
                    },
                    r,
                    shape,
                )
                .total
                .micros();
                micros = micros.saturating_add(as_fresh.saturating_sub(as_cache).max(0));
                toks = toks.saturating_add(*fresh);
            }
        }
        if micros > 0 {
            rows.push(ContextBloatRow {
                session,
                erosion_turn: turn,
                fresh_tokens_after: toks,
                micros,
            });
        }
    }
    rows.sort_by(|a, b| b.micros.cmp(&a.micros).then(a.session.cmp(&b.session)));
    let total_micros = rows
        .iter()
        .map(|r| r.micros)
        .fold(0i64, i64::saturating_add);
    ContextBloatReport {
        rows,
        total_micros,
        pricing_version: pricing.version.clone(),
        estimated: true,
    }
}

// ---- loop / retry waste, rolled up across runs by the offending tool/agent ----

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopWasteRow {
    /// Offending tool (component) else agent (parent) else `unlabeled`.
    pub label: String,
    /// Redundant re-issues (every occurrence of an identical request after the first).
    pub redundant_steps: u32,
    pub tokens: u64,
    pub micros: i64,
    /// Longest identical-request streak seen in a single run (1 = no repeat).
    pub max_repeat: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopWasteReport {
    pub rows: Vec<LoopWasteRow>,
    pub total_micros: i64,
    pub total_redundant_steps: u32,
    pub pricing_version: String,
    pub estimated: bool,
}

/// Dollars burned re-issuing identical requests, attributed to the tool/subagent that looped.
/// A step is "redundant" when an identical `request_hash` already appeared earlier in the same
/// run (the existing per-run retry-loop rule), rolled up here across all runs. `projected_saved`
/// equals the redundant spend (capping the loop would save exactly that).
pub fn loop_waste(runs: &[RunRecord], pricing: &PricingTable) -> LoopWasteReport {
    use std::collections::BTreeMap as Map;
    #[derive(Default)]
    struct W {
        redundant: u32,
        tokens: u64,
        micros: i64,
        max_repeat: u32,
    }
    let mut by_label: BTreeMap<String, W> = BTreeMap::new();
    let mut total = 0i64;
    let mut total_redundant = 0u32;
    for run in runs {
        let mut seen: Map<u64, u32> = Map::new(); // request_hash -> occurrences so far
        for step in &run.steps {
            let Some(hash) = step.shape.request_hash else {
                continue; // no hash (max_private) -> can't detect a loop
            };
            let occ = seen.entry(hash).or_insert(0);
            *occ = occ.saturating_add(1);
            let label = step
                .shape
                .component_label
                .clone()
                .or_else(|| step.shape.parent_label.clone())
                .unwrap_or_else(|| "unlabeled".to_string());
            let w = by_label.entry(label).or_default();
            w.max_repeat = w.max_repeat.max(*occ);
            if *occ > 1 {
                // This occurrence is a redundant re-issue.
                let micros = pricing
                    .lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
                    .map(|r| cost_usage(&step.usage, r, &step.shape).total.micros())
                    .unwrap_or(0);
                w.redundant = w.redundant.saturating_add(1);
                w.tokens = w.tokens.saturating_add(step.usage.total());
                w.micros = w.micros.saturating_add(micros);
                total = total.saturating_add(micros);
                total_redundant = total_redundant.saturating_add(1);
            }
        }
    }
    let mut rows: Vec<LoopWasteRow> = by_label
        .into_iter()
        .filter(|(_, w)| w.redundant > 0) // only labels that actually looped
        .map(|(label, w)| LoopWasteRow {
            label,
            redundant_steps: w.redundant,
            tokens: w.tokens,
            micros: w.micros,
            max_repeat: w.max_repeat,
        })
        .collect();
    rows.sort_by(|a, b| b.micros.cmp(&a.micros).then(a.label.cmp(&b.label)));
    LoopWasteReport {
        rows,
        total_micros: total,
        total_redundant_steps: total_redundant,
        pricing_version: pricing.version.clone(),
        estimated: true,
    }
}

// ---- cost of failure: dollars on errored / refused steps, by offending tool/agent ----

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureWasteRow {
    pub label: String,
    /// Steps that ended in a provider error or a refusal.
    pub failed_steps: u32,
    pub tokens: u64,
    pub micros: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureWasteReport {
    pub rows: Vec<FailureWasteRow>,
    pub total_micros: i64,
    pub total_failed_steps: u32,
    /// Failed spend as a percent of all captured spend (0–100, integer).
    pub pct_of_spend: i64,
    pub pricing_version: String,
    pub estimated: bool,
}

/// Dollars that bought nothing: tokens on steps that ended in a provider error or a refusal
/// (`StepRecord::is_retry_worthy_failure`), attributed to the offending tool/agent. A failed tool
/// call still costs its input tokens; a refusal consumes input + reasoning for zero useful output.
pub fn failure_waste(runs: &[RunRecord], pricing: &PricingTable) -> FailureWasteReport {
    let mut by_label: BTreeMap<String, (u32, u64, i64)> = BTreeMap::new();
    let mut failed_micros = 0i64;
    let mut all_micros = 0i64;
    let mut total_failed = 0u32;
    for run in runs {
        for step in &run.steps {
            let micros = pricing
                .lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
                .map(|r| cost_usage(&step.usage, r, &step.shape).total.micros())
                .unwrap_or(0);
            all_micros = all_micros.saturating_add(micros);
            if step.is_retry_worthy_failure() {
                let label = step
                    .shape
                    .component_label
                    .clone()
                    .or_else(|| step.shape.parent_label.clone())
                    .unwrap_or_else(|| "unlabeled".to_string());
                let e = by_label.entry(label).or_default();
                e.0 = e.0.saturating_add(1);
                e.1 = e.1.saturating_add(step.usage.total());
                e.2 = e.2.saturating_add(micros);
                failed_micros = failed_micros.saturating_add(micros);
                total_failed = total_failed.saturating_add(1);
            }
        }
    }
    let mut rows: Vec<FailureWasteRow> = by_label
        .into_iter()
        .map(|(label, (failed_steps, tokens, micros))| FailureWasteRow {
            label,
            failed_steps,
            tokens,
            micros,
        })
        .collect();
    rows.sort_by(|a, b| b.micros.cmp(&a.micros).then(a.label.cmp(&b.label)));
    let pct_of_spend = if all_micros > 0 {
        ((i128::from(failed_micros) * 100) / i128::from(all_micros)) as i64
    } else {
        0
    };
    FailureWasteReport {
        rows,
        total_micros: failed_micros,
        total_failed_steps: total_failed,
        pct_of_spend,
        pricing_version: pricing.version.clone(),
        estimated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Provider, StepMeta, StepRecord};

    fn pricing() -> PricingTable {
        PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml")).unwrap()
    }

    // Build a real (priced, nonzero) step from a captured fixture, then stamp session/tool/run.
    fn step(run_id: &str, ord: u32, session: Option<&str>, tool: Option<&str>) -> StepRecord {
        let req = include_bytes!("../../fixtures/openai_nonstream/request.json");
        let resp = include_bytes!("../../fixtures/openai_nonstream/response.json");
        let meta = StepMeta {
            workload_key: None,
            component_label: tool.map(String::from),
            ..Default::default()
        };
        let mut s = crate::ingest_step_ct(
            run_id,
            ord,
            Provider::Openai,
            req,
            resp,
            None,
            &crate::PrivacyPolicy::default(),
            &meta,
            None,
            0,
        )
        .unwrap();
        s.shape.session = session.map(String::from);
        s
    }

    #[test]
    fn wasted_cache_write_flags_write_without_read() {
        // input $3/Mtok, 5m-write $3.75/Mtok (1.25×) → premium $0.75/Mtok over fresh.
        let pr = PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[
              {"provider":"anthropic","model_id":"m","input_micro_per_mtok":3000000,
               "output_micro_per_mtok":15000000,"cache_read_micro_per_mtok":300000,
               "cache_write_5m_micro_per_mtok":3750000,"cache_write_1h_micro_per_mtok":6000000}]}"#,
        )
        .unwrap();
        let mk = |hash: u64, w5: u64, cread: u64| StepRecord {
            run_id: "r".into(),
            step_ordinal: 1,
            provider: Provider::Anthropic,
            model: "m".into(),
            usage: UsageTokens {
                cache_write_5m: w5,
                cache_read: cread,
                ..Default::default()
            },
            shape: RequestShape {
                model: "m".into(),
                provider: Provider::Anthropic,
                stream: false,
                ttl: crate::model::CacheTtl::FiveMin,
                has_cache_control: true,
                cached_component: None,
                system_hash: Some(hash),
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
        };
        // prefix 1: written, never read → wasted. prefix 2: written then read → NOT wasted.
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![
                mk(1, 1_000_000, 0),
                mk(2, 1_000_000, 0),
                mk(2, 0, 1_000_000),
            ],
        }];
        let rep = wasted_cache_write(&runs, &pr);
        assert_eq!(
            rep.rows.len(),
            1,
            "only prefix 1 (write, no read) is wasted"
        );
        assert_eq!(rep.rows[0].write_tokens, 1_000_000);
        assert_eq!(
            rep.rows[0].micros, 750_000,
            "premium = (3.75 − 3.00) × 1M tok"
        );
        assert_eq!(rep.rows[0].provider, "anthropic");
        assert_eq!(rep.total_micros, 750_000);

        // An unrelated backend reading the same content hash must not make Anthropic's unused
        // write look reused; cache identity is provider/model scoped.
        let anthropic_write = mk(3, 1_000_000, 0);
        let mut openai_read = mk(3, 0, 1_000_000);
        openai_read.provider = Provider::Openai;
        openai_read.shape.provider = Provider::Openai;
        let isolated = wasted_cache_write(
            &[RunRecord {
                run_id: "r".into(),
                steps: vec![anthropic_write, openai_read],
            }],
            &pr,
        );
        assert_eq!(isolated.rows.len(), 1);
        assert_eq!(isolated.rows[0].provider, "anthropic");
    }

    #[test]
    fn run_outcome_split_separates_successful_from_failed_runs() {
        // run-ok ends clean; run-bad's terminal step is a provider error -> failed.
        let mut bad = step("run-bad", 1, None, None);
        bad.stop_reason = Some("provider_error".into());
        let runs = vec![
            RunRecord {
                run_id: "run-ok".into(),
                steps: vec![step("run-ok", 1, None, None)],
            },
            RunRecord {
                run_id: "run-bad".into(),
                steps: vec![bad],
            },
        ];
        let split = run_outcome_split(&runs, &pricing());
        assert_eq!(split.successful_runs, 1);
        assert_eq!(split.failed_runs, 1);
        assert_eq!(split.success_rate_pct, Some(50));
        // cost-per-successful = ALL spend / the 1 successful run (failed work still cost money).
        assert_eq!(split.cost_per_successful_micros, Some(split.total_micros));
    }

    #[test]
    fn groups_runs_into_one_session_and_counts_tools() {
        let runs = vec![
            RunRecord {
                run_id: "run-a".into(),
                steps: vec![
                    step("run-a", 1, Some("task-1"), Some("Bash")),
                    step("run-a", 2, Some("task-1"), Some("Read")),
                ],
            },
            RunRecord {
                run_id: "run-b".into(),
                steps: vec![step("run-b", 1, Some("task-1"), Some("Bash"))],
            },
        ];
        let rep = sessions(&runs, &pricing());
        assert_eq!(rep.rows.len(), 1);
        let row = &rep.rows[0];
        assert_eq!(row.session, "task-1");
        assert_eq!(row.runs, 2); // spans both runs
        assert_eq!(row.steps, 3);
        assert_eq!(row.tools, 2); // Bash + Read
        assert!(row.micros > 0);
    }

    #[test]
    fn loop_waste_attributes_redundant_reissues_to_the_offending_tool() {
        // Three identical requests (same fixture -> same request_hash) from one tool: the 2nd and
        // 3rd are redundant; max_repeat is 3.
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![
                step("r", 1, None, Some("Bash")),
                step("r", 2, None, Some("Bash")),
                step("r", 3, None, Some("Bash")),
            ],
        }];
        let rep = loop_waste(&runs, &pricing());
        assert_eq!(rep.rows.len(), 1);
        assert_eq!(rep.rows[0].label, "Bash");
        assert_eq!(rep.rows[0].redundant_steps, 2);
        assert_eq!(rep.rows[0].max_repeat, 3);
        assert_eq!(rep.total_redundant_steps, 2);
        assert!(rep.total_micros > 0);
    }

    #[test]
    fn failure_waste_sums_errored_and_refused_steps_by_tool() {
        let mut bad = step("r", 1, None, Some("Bash"));
        bad.stop_reason = Some("provider_error".into());
        let mut refused = step("r", 2, None, Some("WebSearch"));
        refused.stop_reason = Some("refusal".into());
        let ok = step("r", 3, None, Some("Bash")); // succeeds -> not counted
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![bad, refused, ok],
        }];
        let rep = failure_waste(&runs, &pricing());
        assert_eq!(rep.total_failed_steps, 2);
        assert!(rep.total_micros > 0);
        assert!(rep.pct_of_spend > 0 && rep.pct_of_spend <= 100);
        // Two distinct offending tools.
        assert_eq!(rep.rows.len(), 2);
        assert!(rep
            .rows
            .iter()
            .any(|r| r.label == "Bash" && r.failed_steps == 1));
    }

    #[test]
    fn context_growth_curve_flags_cache_erosion() {
        // Turn 1 reads cache; turn 2 reads none and pays fresh input on a bigger prefix.
        let mut a = step("r", 1, Some("t"), None);
        a.usage.cache_read = 500;
        a.usage.fresh_input = 100;
        let mut b = step("r", 2, Some("t"), None);
        b.usage.cache_read = 0;
        b.usage.fresh_input = 800;
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![a, b],
        }];
        let rep = sessions(&runs, &pricing());
        let row = &rep.rows[0];
        assert_eq!(row.input_curve, vec![100, 800]);
        assert_eq!(row.cache_erosion_turn, Some(2));
    }

    #[test]
    fn context_bloat_waste_prices_the_recache_recovery() {
        // Turn 1 reads cache (cheap); turn 2 erodes — 800 fresh-input tokens re-sent uncached.
        let mut a = step("r", 1, Some("t"), None);
        a.usage.cache_read = 500;
        a.usage.fresh_input = 100;
        let mut b = step("r", 2, Some("t"), None);
        b.usage.cache_read = 0;
        b.usage.fresh_input = 800;
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![a, b],
        }];
        let p = pricing();
        let rep = context_bloat_waste(&runs, &p);
        assert_eq!(rep.rows.len(), 1);
        let row = &rep.rows[0];
        assert_eq!(row.session, "t");
        assert_eq!(row.erosion_turn, 2);
        // Only the post-erosion fresh tokens count (turn 2's 800; turn 1 still cached).
        assert_eq!(row.fresh_tokens_after, 800);
        // Recoverable = pricing 800 tokens at (fresh − cache_read) rate > 0, and strictly less than
        // pricing all 800 as pure fresh input (cache-read isn't free).
        assert!(row.micros > 0);
        let r = p
            .lookup(Provider::Openai, None, &runs[0].steps[1].model)
            .unwrap();
        let shape = runs[0].steps[1].shape.clone();
        let full_fresh = cost_usage(
            &UsageTokens {
                fresh_input: 800,
                ..Default::default()
            },
            r,
            &shape,
        )
        .total
        .micros();
        assert!(row.micros <= full_fresh);
        assert_eq!(rep.total_micros, row.micros);

        // No erosion -> no row (a steadily-cached session is not bloat).
        let mut c = step("r2", 1, Some("t2"), None);
        c.usage.cache_read = 500;
        c.usage.fresh_input = 100;
        let clean = vec![RunRecord {
            run_id: "r2".into(),
            steps: vec![c],
        }];
        assert!(context_bloat_waste(&clean, &p).rows.is_empty());
    }

    #[test]
    fn sessionless_steps_fall_back_to_run_id() {
        let runs = vec![RunRecord {
            run_id: "solo".into(),
            steps: vec![step("solo", 1, None, None)],
        }];
        let rep = sessions(&runs, &pricing());
        assert_eq!(rep.rows[0].session, "solo");
    }
}

// ---- run-outcome split: cost-per-SUCCESSFUL-run ----

/// Cost split by run outcome: a run that ended in a retry-worthy failure (its terminal step is a
/// provider error / refusal) bought nothing, so cost-per-SUCCESSFUL-run reads differently from raw
/// $/run. No new capture — `stop_reason` is already stored. Pure, integer.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunOutcomeSplit {
    pub successful_runs: u32,
    pub failed_runs: u32,
    pub total_micros: i64,
    /// Total spend / successful runs (None when no run succeeded).
    pub cost_per_successful_micros: Option<i64>,
    /// Successful / total runs, integer percent (None when there are no runs).
    pub success_rate_pct: Option<i64>,
}

/// Classify each run by its TERMINAL step's stop_reason (clean vs retry-worthy failure) and report
/// cost-per-successful-run + the success rate. A run with no steps counts as neither.
pub fn run_outcome_split(runs: &[RunRecord], pricing: &PricingTable) -> RunOutcomeSplit {
    let mut successful = 0u32;
    let mut failed = 0u32;
    let mut total = 0i64;
    for run in runs {
        let run_micros: i64 = run
            .steps
            .iter()
            .map(|s| {
                pricing
                    .lookup(s.provider, s.shape.vendor.as_deref(), &s.model)
                    .map(|r| cost_usage(&s.usage, r, &s.shape).total.micros())
                    .unwrap_or(0)
            })
            .sum();
        total = total.saturating_add(run_micros);
        match run.steps.last() {
            Some(last) if last.is_retry_worthy_failure() => failed += 1,
            Some(_) => successful += 1,
            None => {}
        }
    }
    let total_runs = successful + failed;
    RunOutcomeSplit {
        successful_runs: successful,
        failed_runs: failed,
        total_micros: total,
        cost_per_successful_micros: (successful > 0).then(|| total / successful as i64),
        success_rate_pct: (total_runs > 0).then(|| (successful as i64 * 100) / total_runs as i64),
    }
}
