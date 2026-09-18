//! Per-session "cost autopsy": scope tare's EXISTING cost + waste analysis to one session
//! (run) and package it for the run-detail drill. NO new analysis, NO tokenizer, NO proxy — it reuses
//! [`crate::account::cost_from_usage`] for the exact per-class decomposition and
//! [`crate::savings::savings`] for the recoverable-waste opportunities, over just this run.
//!
//! Design (grilled 2026-07-05): the money in `classes`/`total_micros` is MEASURED (usage-count x price
//! and their exact sum). The `headline` follows a fallback ladder — biggest recoverable WASTE, else an
//! expensive-vs-baseline structural driver (descriptive, with a tentative lever, NO "save $Y" claim),
//! else honestly `Efficient` — so a healthy session (where the biggest cost class is the *cheap*
//! cache-read) never gets a misleading "biggest cost = waste" headline. Opportunity kinds are gated to
//! those a Claude Code user can actually act on (Claude Code owns the system prompt / tools / cache
//! breakpoints, so `cache`/`breakpoint` advice is suppressed).

use crate::account::cost_from_usage;
use crate::model::{CacheClass, RunRecord};
use crate::money::MicroUsd;
use crate::pricing::PricingTable;
use crate::savings::{savings, Opportunity};
use serde::Serialize;

/// Opportunity kinds a Claude Code end-user can actually act on. `cache`/`breakpoint` are excluded:
/// they presume the user places `cache_control` markers, which Claude Code owns — surfacing them as
/// "recoverable" would be a misleading number.
const ACTIONABLE_KINDS: &[&str] = &[
    "loop",
    "failure",
    "wasted-write",
    "context-bloat",
    "model-swap",
    "rightsizing",
];

/// One additive cost class; the classes sum to the exact session total.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ClassCost {
    /// `fresh_input` | `cache_read` | `cache_write` | `output`.
    pub class: &'static str,
    pub micros: i64,
}

/// The single line the autopsy leads with — picked by the fallback ladder, never inventing waste.
/// Adjacently tagged so the web can switch on `kind` (`waste` | `structural_driver` | `efficient`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum Headline {
    /// There is measured/actionable recoverable waste — lead with the top opportunity (hard $).
    Waste(Opportunity),
    /// No waste, but the session is expensive vs the user's median: name the dominant cost driver and
    /// a TENTATIVE structural lever (descriptive, no "save $Y" — see grill decision 8).
    StructuralDriver {
        class: &'static str,
        micros: i64,
        /// This session's total as a percent of the baseline median (e.g. 300 = 3x).
        vs_median_pct: u32,
        lever: String,
    },
    /// Nothing to change — honest.
    Efficient,
}

/// A session's cost breakdown + recoverable-waste opportunities + the chosen headline, for the drill.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionAutopsy {
    pub run_id: String,
    /// Exact session total (== the sum of `classes`).
    pub total_micros: i64,
    /// Additive classes (fresh_input / cache_read / cache_write / output); sum == `total_micros`.
    pub classes: Vec<ClassCost>,
    /// Thinking spend — a SUB-figure of `output` (billed as output), surfaced for context, never added.
    pub reasoning_micros: i64,
    /// Cache-read share of the input TOKENS (0..100) — high is GOOD (cheap re-carry). `0` when no input.
    pub cache_hit_pct: u8,
    /// This session's total as a percent of the baseline median session (e.g. 300 = 3x); `None` when
    /// no baseline was supplied or the median is 0.
    pub vs_median_pct: Option<u32>,
    /// Attribution fidelity, so the UI can label honestly and never fake one lane up:
    /// `"component"` when the request bodies were seen (proxy weights present → a within-prompt
    /// component split is available), else `"cost_class"` — exact cost-class + waste, but NO
    /// within-prompt component split (the out-of-band JSONL/OTLP ceiling). The $ totals + waste flags
    /// are exact either way; only the within-fresh split differs.
    pub fidelity: &'static str,
    /// The chosen headline (fallback ladder).
    pub headline: Headline,
    /// Actionable recoverable-waste opportunities for THIS session (gated + sorted desc by $), each
    /// with its own `confidence` + fix.
    pub opportunities: Vec<Opportunity>,
}

/// Descriptive structural lever for the dominant cost class — tentative, tradeoff-honest, never a
/// bare "save $Y" (grill decision 8: hard $ is reserved for measured waste).
fn lever_for(class: &str) -> String {
    match class {
        "cache_read" => "Most of the cost is re-reading a large context each turn. If the later turns didn't need the early context, `/clear` between tasks or a subagent for exploration would cut it — but splitting a genuinely-connected task can cost more re-establishing it.".to_string(),
        "output" => "Output/generation dominates. Lowering thinking effort on simple turns (`/effort`) or a smaller model (`/model`) for routine work would cut it.".to_string(),
        "fresh_input" => "Large uncached input dominates. If a stable prefix repeats across turns it may not be getting cached; otherwise this is inherent to the prompts you send.".to_string(),
        "cache_write" => "Cache writes dominate — a volatile prefix may be breaking the cache each turn (paying the 1.25-2x write premium instead of the 0.1x read). This is often Claude Code's prompt construction, not directly fixable.".to_string(),
        _ => "This is the session's dominant cost driver.".to_string(),
    }
}

/// Build the autopsy for one session. `baseline_median_micros` is the user's median session cost (for
/// the vs-median reference); pass `None` if unknown. Unpriced steps contribute $0 — an honest GAP.
///
/// GRAIN: this is RUN-grained — one `RunRecord`. For Claude Code JSONL, `session_id ==
/// run_id` (empirically 1:1 across sampled real sessions), so a run IS a session and the run-detail
/// header is honestly "this run's" autopsy. KNOWN LIMITATION: a capture source where one session spans
/// multiple `run_id`s (some proxy/OTLP setups) would have this describe only one of them — gathering
/// all of a session's runs before analysis is deferred until such a source is observed.
pub fn session_autopsy(
    run: &RunRecord,
    pricing: &PricingTable,
    baseline_median_micros: Option<i64>,
) -> SessionAutopsy {
    let mut fresh = MicroUsd::ZERO;
    let mut cache_read = MicroUsd::ZERO;
    let mut cache_write = MicroUsd::ZERO;
    let mut output = MicroUsd::ZERO;
    let mut reasoning = MicroUsd::ZERO;
    // Token sums for the cache-hit rate (tokens, not dollars).
    let mut cache_read_tokens: u64 = 0;
    // Full input-token total across steps (fresh + cache_write + cache_read = total_prompt) — the
    // honest denominator for the cache-hit rate; omitting cache_write overstated it.
    let mut input_prompt_tokens: u64 = 0;
    for step in &run.steps {
        let Some(rates) = pricing.lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
        else {
            continue; // unpriced → GAP (0 contribution)
        };
        let cb = cost_from_usage(&step.usage, rates);
        fresh += cb.fresh;
        cache_read += cb.cache_read;
        cache_write += cb.cache_write;
        output += cb.output;
        cache_read_tokens = cache_read_tokens.saturating_add(step.usage.cache_read);
        input_prompt_tokens = input_prompt_tokens.saturating_add(step.usage.total_prompt());
        // Reasoning is a subset of `output`; price it at the tier-resolved output rate for an "of which
        // thinking" sub-figure — NOT added to the total.
        let resolved = rates.for_input(step.usage.total_prompt());
        reasoning += MicroUsd::for_tokens(
            step.usage.reasoning,
            resolved.micro_per_mtok(CacheClass::Reasoning),
        );
    }
    let classes = vec![
        ClassCost {
            class: "fresh_input",
            micros: fresh.micros(),
        },
        ClassCost {
            class: "cache_read",
            micros: cache_read.micros(),
        },
        ClassCost {
            class: "cache_write",
            micros: cache_write.micros(),
        },
        ClassCost {
            class: "output",
            micros: output.micros(),
        },
    ];
    let total_micros = classes
        .iter()
        .fold(0i64, |sum, class| sum.saturating_add(class.micros));

    // Cache-read share of ALL input tokens (fresh + cache_write + cache_read), high = good/cheap
    // re-carry. Denominator includes cache_write so the rate isn't overstated.
    // checked_div → 0 when no input.
    let cache_hit_pct = (u128::from(cache_read_tokens) * 100)
        .checked_div(u128::from(input_prompt_tokens))
        .unwrap_or(0)
        .min(100) as u8;

    // vs-median (integer percent), only when a positive baseline is supplied.
    let vs_median_pct = baseline_median_micros.filter(|m| *m > 0).map(|m| {
        let pct = (i128::from(total_micros.max(0)) * 100) / i128::from(m);
        u32::try_from(pct).unwrap_or(u32::MAX)
    });

    // Actionable, waste-only opportunities (gated), already sorted desc by recoverable $ by savings().
    let opportunities: Vec<Opportunity> = savings(std::slice::from_ref(run), pricing)
        .opportunities
        .into_iter()
        .filter(|o| ACTIONABLE_KINDS.contains(&o.kind.as_str()) && o.recoverable_micros > 0)
        .collect();

    // Fallback ladder: MEASURED waste → expensive-vs-baseline structural driver → efficient. Only a
    // `measured`-confidence opportunity owns the headline with a hard $ (grill decision 8); approximate
    // ones (e.g. a model-swap suggestion) remain in the list as considerations but never the lede.
    let headline = if let Some(top) = opportunities.iter().find(|o| o.confidence == "measured") {
        Headline::Waste(top.clone())
    } else if let Some(vmp) = vs_median_pct.filter(|p| *p > 150) {
        // Expensive vs the user's median with no recoverable waste → name the dominant driver.
        match classes
            .iter()
            .filter(|c| c.micros > 0)
            .max_by_key(|c| c.micros)
        {
            Some(d) => Headline::StructuralDriver {
                class: d.class,
                micros: d.micros,
                vs_median_pct: vmp,
                lever: lever_for(d.class),
            },
            None => Headline::Efficient,
        }
    } else {
        Headline::Efficient
    };

    // Fidelity: a within-prompt component split is only possible when a lane saw the request bodies
    // (proxy → non-empty shape.weights). Out-of-band lanes (JSONL/OTLP) are cost-class + waste only.
    let fidelity = if run.steps.iter().any(|s| !s.shape.weights.is_empty()) {
        "component"
    } else {
        "cost_class"
    };

    SessionAutopsy {
        run_id: run.run_id.clone(),
        total_micros,
        classes,
        reasoning_micros: reasoning.micros(),
        cache_hit_pct,
        vs_median_pct,
        fidelity,
        headline,
        opportunities,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CacheTtl, Provider, RequestShape, StepRecord, UsageTokens};

    fn pricing() -> PricingTable {
        PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml")).unwrap()
    }

    fn step(ordinal: u32, model: &str, usage: UsageTokens) -> StepRecord {
        StepRecord {
            run_id: "sess-1".into(),
            step_ordinal: ordinal,
            provider: Provider::Anthropic,
            model: model.into(),
            usage,
            shape: RequestShape {
                model: model.into(),
                provider: Provider::Anthropic,
                stream: false,
                ttl: CacheTtl::FiveMin,
                has_cache_control: true,
                cached_component: None,
                system_hash: Some(1),
                weights: vec![],
                request_hash: Some(ordinal as u64),
                step_label: None,
                component_label: None,
                parent_label: None,
                attempt: None,
                session: Some("sess-1".into()),
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
        }
    }

    fn run(steps: Vec<StepRecord>) -> RunRecord {
        RunRecord {
            run_id: "sess-1".into(),
            steps,
        }
    }

    #[test]
    fn classes_sum_to_the_exact_total_and_reasoning_is_a_subfigure_of_output() {
        let r = run(vec![
            step(
                1,
                "claude-opus-4-8",
                UsageTokens {
                    fresh_input: 2_000,
                    cache_write_5m: 10_000,
                    output: 500,
                    ..Default::default()
                },
            ),
            step(
                2,
                "claude-opus-4-8",
                UsageTokens {
                    fresh_input: 300,
                    cache_read: 10_000,
                    output: 800,
                    reasoning: 400,
                    ..Default::default()
                },
            ),
        ]);
        let a = session_autopsy(&r, &pricing(), None);
        assert_eq!(
            a.classes.iter().map(|c| c.micros).sum::<i64>(),
            a.total_micros,
            "classes reconcile to exact total"
        );
        let output = a
            .classes
            .iter()
            .find(|c| c.class == "output")
            .unwrap()
            .micros;
        assert!(
            a.reasoning_micros > 0 && a.reasoning_micros <= output,
            "reasoning is a sub-figure of output"
        );
        // Cache-hit % is cache_read over ALL input tokens: 10000 / (fresh 2300 + cache_write 10000 +
        // cache_read 10000) = 10000/22300 = 44%. The denominator now includes the 10000 cache_write
        // tokens (they ARE billed input), so the rate isn't overstated (iolq #8 — was 81% omitting them).
        assert_eq!(
            a.cache_hit_pct, 44,
            "cache-hit % over full input tokens, incl. cache_write"
        );
        // Out-of-band (empty weights) → cost-class fidelity, never faked up to a component split.
        assert_eq!(a.fidelity, "cost_class");
    }

    #[test]
    fn healthy_session_is_efficient_not_headlined_as_cache_read_waste() {
        // Big cache_read (the cheap, healthy part) dominates cost, but there is NO recoverable waste
        // and no baseline → must be Efficient, NOT a "biggest cost = cache_read" waste headline.
        let r = run(vec![step(
            1,
            "claude-opus-4-8",
            UsageTokens {
                fresh_input: 200,
                cache_read: 500_000,
                output: 300,
                ..Default::default()
            },
        )]);
        let a = session_autopsy(&r, &pricing(), None);
        assert_eq!(
            a.headline,
            Headline::Efficient,
            "no waste + no baseline → efficient, never cache_read-as-waste"
        );
    }

    #[test]
    fn expensive_vs_median_without_waste_gets_a_structural_driver_headline() {
        // 4x the median, cache_read-dominated, no waste → structural driver (descriptive lever, no $).
        let r = run(vec![step(
            1,
            "claude-opus-4-8",
            UsageTokens {
                fresh_input: 200,
                cache_read: 800_000,
                output: 400,
                ..Default::default()
            },
        )]);
        let median = 50_000; // this session is far above it
        let a = session_autopsy(&r, &pricing(), Some(median));
        match a.headline {
            Headline::StructuralDriver {
                class,
                vs_median_pct,
                ref lever,
                ..
            } => {
                assert_eq!(class, "cache_read");
                assert!(vs_median_pct > 150);
                assert!(
                    lever.contains("/clear") || lever.contains("context"),
                    "tentative structural lever, not a $ claim"
                );
            }
            other => panic!("expected StructuralDriver, got {other:?}"),
        }
    }

    #[test]
    fn cache_and_breakpoint_advice_are_suppressed_unpriced_steps_are_gaps() {
        // An unpriced model → GAP (0), and a clean run yields no actionable opportunities.
        let s = step(
            1,
            "totally-unknown-model-xyz",
            UsageTokens {
                fresh_input: 1_000,
                output: 100,
                ..Default::default()
            },
        );
        let a = session_autopsy(&run(vec![s]), &pricing(), None);
        assert_eq!(
            a.total_micros, 0,
            "unpriced model is a GAP, never fabricated"
        );
        assert!(
            a.opportunities
                .iter()
                .all(|o| ACTIONABLE_KINDS.contains(&o.kind.as_str())),
            "only actionable kinds surface"
        );
        assert_eq!(a.headline, Headline::Efficient);
    }
}
