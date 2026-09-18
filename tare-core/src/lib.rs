//! Tare core — the testable product.
//!
//! Pure functions only: bytes + pricing -> usage -> cost -> attribution -> flamegraph
//! model -> SVG / speedscope. Zero I/O, no clock, no RNG, no network. Everything the
//! CLI and GUI surface is reachable here and is byte-deterministic.

pub mod account;
pub mod action_plan;
pub mod advise;
pub mod advisory;
pub mod alert;
pub mod anomaly;
pub mod attribute;
pub mod autopsy;
pub mod bisect;
pub mod budget;
pub mod burnrate;
pub mod cache_health;
pub mod cache_ledger;
pub mod calendar;
pub mod calibrated_bench;
pub mod canon;
pub mod cohort;
pub mod confidence;
pub mod config;
pub mod correlation;
pub mod cost_regression;
pub mod demo;
pub mod diff;
pub mod digest;
pub mod effectiveness;
pub mod estimate;
pub mod experiment;
pub mod explain;
pub mod flame_diff;
pub mod flamegraph;
pub mod git;
pub mod heatmap;
pub mod hooks;
pub mod lenses;
pub mod lineage;
pub mod model;
pub mod money;
pub mod otel;
pub mod pricing;
pub mod privacy;
pub mod process;
pub mod prometheus;
pub mod punchcard;
pub mod realization;
pub mod reasoning;
pub mod receipt;
pub mod reconcile;
pub mod redact;
pub mod rollup;
pub mod savings;
pub mod session;
pub mod shape_gate;
pub mod share;
pub mod speedscope;
pub mod sse;
pub mod streaks;
pub mod svg;
pub mod tokenizer;
pub mod transcript;
pub mod trend;
pub mod tz;
pub mod whatif;
pub mod wire;
pub mod workunit;

pub use model::{
    CacheClass, CacheTtl, Component, Provider, RunRecord, StepRecord, TodaySpend, UsageTokens,
};
pub use money::MicroUsd;
pub use pricing::{ModelRates, PricingTable};
pub use privacy::PrivacyPolicy;

/// **Embeddable accounting facade.** The semver-stable surface for reusing Tare's
/// integer-money costing kernel without the proxy or CLI. Cost a provider-reported usage
/// vector with no raw bytes and no re-tokenization; `None` for an unpriced model (never $0).
/// Pair with the re-exported [`Provider`], [`UsageTokens`], [`PricingTable`], [`MicroUsd`], and
/// [`account::CostBreakdown`]. The signature is locked by the facade-API golden test.
pub use account::{account_from_usage, CostBreakdown};

/// Parse one captured request/response into a `StepRecord` under the default (strict) policy.
pub fn ingest_step(
    run_id: impl Into<String>,
    step_ordinal: u32,
    provider: Provider,
    request_bytes: &[u8],
    response_bytes: &[u8],
) -> Result<StepRecord, String> {
    ingest_step_ct(
        run_id,
        step_ordinal,
        provider,
        request_bytes,
        response_bytes,
        None,
        &PrivacyPolicy::default(),
        &model::StepMeta::default(),
        None,
        0,
    )
}

/// As [`ingest_step`], but threads the response `Content-Type` to the wire parser,
/// applies an explicit privacy policy, stamps optional adapter correlation labels, and
/// accepts a `model_hint` (e.g. a Gemini model id parsed from the URL path) used ONLY when the
/// request/response carried no model of its own — so the pricing key resolves instead of
/// defaulting to "unknown" and silently pricing to $0.
#[allow(clippy::too_many_arguments)]
pub fn ingest_step_ct(
    run_id: impl Into<String>,
    step_ordinal: u32,
    provider: Provider,
    request_bytes: &[u8],
    response_bytes: &[u8],
    content_type: Option<&str>,
    policy: &PrivacyPolicy,
    meta: &model::StepMeta,
    model_hint: Option<&str>,
    duration_ms: u64,
) -> Result<StepRecord, String> {
    let (mut shape, usage, stop_reason) = wire::parse_step_with_content_type(
        provider,
        request_bytes,
        response_bytes,
        content_type,
        policy,
    )?;
    // Fall back to the caller's model hint only when the wire gave us nothing usable. Normalize and
    // bound path-derived input exactly like a wire model id. An explicit body/response model wins.
    if shape.model.is_empty() || shape.model == "unknown" {
        if let Some(hint) = model_hint {
            if let Some(model) = wire::normalize_model_id(provider, hint) {
                shape.model = model;
            }
        }
    }
    // Additive correlation labels (opaque, already truncated by the proxy).
    shape.step_label = meta.step_label.clone();
    shape.component_label = meta.component_label.clone();
    shape.parent_label = meta.parent_label.clone();
    shape.attempt = meta.attempt;
    // User-provided workload key — stamped from the header, never payload.
    shape.workload_key = meta.workload_key.clone();
    // Explicit vendor label (proxy x-tare-vendor) — only meaningful for OpenAI-compatible steps,
    // harmless otherwise; it's the pricing dimension paired with the model.
    if meta.vendor.is_some() {
        shape.vendor = meta.vendor.clone();
    }
    // Git attribution, stamped by the edge when config-gated capture is on.
    shape.commit = meta.commit.clone();
    shape.author = meta.author.clone();
    let model = shape.model.clone();
    Ok(StepRecord {
        run_id: run_id.into(),
        step_ordinal,
        provider,
        model,
        usage,
        shape,
        // Stop reason is a provider-controlled status token; clamp to a short label so an
        // adversarial/oversized value can't smuggle bulk text into the persisted TEXT column.
        stop_reason: stop_reason.map(|s| s.chars().take(64).collect()),
        // Observed local latency measured at the edge; suppressed to 0 by the privacy opt-out.
        duration_ms: if policy.should_record_latency() {
            duration_ms
        } else {
            0
        },
        // The proxy path has no OTLP timestamps or span identity — those come only from the
        // out-of-band OTel path. Proxy/legacy steps are step-order-only.
        start_unix_nano: None,
        trace_id: None,
        span_id: None,
        parent_span_id: None,
    })
}

/// Parse only the request into a zero-usage `StepRecord`. Used when the response could
/// not be parsed (compressed/error/partial) so the step is still accounted for — the
/// ordinal is never silently lost — rather than dropped.
pub fn ingest_request_only(
    run_id: impl Into<String>,
    step_ordinal: u32,
    provider: Provider,
    request_bytes: &[u8],
    stop_reason: &str,
    policy: &PrivacyPolicy,
) -> Option<StepRecord> {
    let shape = wire::parser_for(provider)
        .request_shape(request_bytes, policy)
        .ok()?;
    Some(StepRecord {
        run_id: run_id.into(),
        step_ordinal,
        provider,
        model: shape.model.clone(),
        usage: UsageTokens::default(),
        shape,
        stop_reason: Some(stop_reason.to_string()),
        // Error/placeholder steps have no meaningful round-trip latency.
        duration_ms: 0,
        start_unix_nano: None,
        trace_id: None,
        span_id: None,
        parent_span_id: None,
    })
}

/// Group steps into runs by `run_id`, preserving first-seen run order and
/// sorting steps within a run by ordinal.
pub fn build_runs(steps: Vec<StepRecord>) -> Vec<RunRecord> {
    let mut order: Vec<String> = Vec::new();
    let mut map: std::collections::BTreeMap<String, RunRecord> = std::collections::BTreeMap::new();
    for s in steps {
        if !map.contains_key(&s.run_id) {
            order.push(s.run_id.clone());
            map.insert(s.run_id.clone(), RunRecord::new(s.run_id.clone()));
        }
        map.get_mut(&s.run_id).unwrap().steps.push(s);
    }
    order
        .into_iter()
        .map(|id| {
            let mut r = map.remove(&id).unwrap();
            r.steps.sort_by_key(|s| s.step_ordinal); // one sort per run — the second pass was redundant
            r
        })
        .collect()
}
