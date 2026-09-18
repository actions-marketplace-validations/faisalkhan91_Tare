//! `tare-receipt v1` — a portable, recomputable cost receipt.
//!
//! A receipt embeds the captured per-step six-axis usage vectors (+ model id + payload-free
//! request shape) alongside the rendered `Report` and optional flamegraph SVG, plus the pricing
//! provenance. A recipient runs `verify` OFFLINE, with no network and no trust in the sender: it
//! re-derives the `Report` from the embedded vectors and the recipient's own pricing table,
//! checks the totals and the Σrows ≤ total invariant, regenerates the flamegraph with the
//! byte-identical renderer and diffs it, and recomputes the content digest.
//!
//! `canon` uses keyless FNV. The digest is a **deterministic
//! recomputation checksum, not a tamper-proof cryptographic seal** — it detects accidental
//! corruption and catches an edit that wasn't propagated everywhere, but a motivated forger who
//! recomputes the digest can mint a consistent receipt.
//!
//! A receipt is counts, weights, hashes, and short labels only — exactly the payload-free
//! data Tare already persists. The embedded runs are typed `RunRecord`s (no payload text field
//! exists in the type); a `max_private` receipt additionally carries no flamegraph (node names
//! embed the model + adapter labels).

use crate::attribute::{build_report, Report};
use crate::canon::fnv1a_64;
use crate::confidence::EstimateConfidence;
use crate::flamegraph::build_flamegraph;
use crate::model::RunRecord;
use crate::pricing::PricingTable;
use crate::svg;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const RECEIPT_VERSION: &str = "tare-receipt-v1";
const FULL_RECEIPT_WITH_FLAMEGRAPH: &str = "payload-free run metadata (provider/model/vendor pricing ids, optional short correlation labels/hashes) + structural weights + six-axis usage vectors + integer micro-USD + flamegraph; never request/response payload";
const FULL_RECEIPT_NO_FLAMEGRAPH: &str = "payload-free run metadata (provider/model/vendor pricing ids, optional short correlation labels/hashes) + structural weights + six-axis usage vectors + integer micro-USD; no flamegraph or request/response payload";
const PRIVATE_RECEIPT_CONTENTS: &str = "provider/model/vendor pricing identities + step order + six-axis usage vectors + integer micro-USD; correlation labels, hashes, timing, structural weights, flamegraph, and request/response payloads omitted";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub receipt_version: String,
    pub tare_version: String,
    pub pricing_version: String,
    pub effective_date: String,
    pub privacy_policy_id: String,
    pub profile: String,
    pub scope: String,
    /// Exactly what the receipt is attested to contain (claimed precisely, not "nothing sensitive").
    pub contains: String,
    /// Whether a labeled flamegraph is embedded. Provider/model pricing identities in `runs` are
    /// independent of this flag because verification requires them.
    pub labels_included: bool,
    /// The rendered report (causes + integer micro-USD), as the sender computed it.
    pub report: Report,
    /// The captured runs — payload-free vectors that make the report RE-DERIVABLE.
    pub runs: Vec<RunRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flamegraph_svg: Option<String>,
    /// Estimate-confidence at attest time: pricing freshness, unpriced share, coverage.
    /// HONEST METADATA — `verify` does NOT recompute it; it self-documents the data quality the
    /// dollars were computed under ("pricing 2026.06.01, 92% coverage, 3% unpriced"). It IS in the
    /// digest, so it's tamper-evident like everything else, but it never gates verification.
    pub confidence: EstimateConfidence,
    /// Keyless FNV-1a over the canonical bytes of everything above (NOT a cryptographic seal).
    pub digest: u64,
}

/// Top-level keys a receipt may carry. Verification also compares the complete nested JSON shape
/// against its typed serialization, so unknown fields cannot be smuggled inside runs or reports.
pub const RECEIPT_KEYS: &[&str] = &[
    "receipt_version",
    "tare_version",
    "pricing_version",
    "effective_date",
    "privacy_policy_id",
    "profile",
    "scope",
    "contains",
    "labels_included",
    "report",
    "runs",
    "flamegraph_svg",
    "confidence",
    "digest",
];

/// The digest input: every field except `digest` itself, serialized in declaration order.
/// Both `attest` and `verify` build this identically, so no zeroing/round-trip dance is needed.
#[derive(Serialize)]
struct DigestInput<'a> {
    receipt_version: &'a str,
    tare_version: &'a str,
    pricing_version: &'a str,
    effective_date: &'a str,
    privacy_policy_id: &'a str,
    profile: &'a str,
    scope: &'a str,
    contains: &'a str,
    labels_included: bool,
    report: &'a Report,
    runs: &'a [RunRecord],
    flamegraph_svg: &'a Option<String>,
    confidence: &'a EstimateConfidence,
}

fn digest_of(r: &Receipt) -> u64 {
    let input = DigestInput {
        receipt_version: &r.receipt_version,
        tare_version: &r.tare_version,
        pricing_version: &r.pricing_version,
        effective_date: &r.effective_date,
        privacy_policy_id: &r.privacy_policy_id,
        profile: &r.profile,
        scope: &r.scope,
        contains: &r.contains,
        labels_included: r.labels_included,
        report: &r.report,
        runs: &r.runs,
        flamegraph_svg: &r.flamegraph_svg,
        confidence: &r.confidence,
    };
    // serde_json on a struct is deterministic (declaration field order, no floats here).
    let bytes = serde_json::to_vec(&input).unwrap_or_default();
    fnv1a_64(&bytes)
}

/// Build a `tare-receipt v1` over `runs` already redacted for `report`'s policy. `with_flamegraph`
/// embeds the SVG (caller passes `false` for the max_private profile). Pure: no clock, no I/O.
pub fn attest(
    runs: &[RunRecord],
    report: &Report,
    pricing: &PricingTable,
    tare_version: &str,
    scope: &str,
    with_flamegraph: bool,
    confidence: EstimateConfidence,
) -> Receipt {
    let is_max_private = report.profile.as_deref() == Some("max_private");
    let (runs, report, scope) = if is_max_private {
        let runs = redact_max_private_runs(runs);
        let report = build_report(&runs, pricing)
            .with_privacy(&crate::privacy::PrivacyPolicy::max_private());
        let scope = if runs.len() == 1 {
            "single-run"
        } else {
            "all-runs"
        };
        (runs, report, scope.to_string())
    } else {
        (runs.to_vec(), report.clone(), scope.to_string())
    };
    let flamegraph_svg = if with_flamegraph && !is_max_private && runs.len() == 1 {
        Some(svg::render_svg(&build_flamegraph(&runs[0], pricing)))
    } else {
        None
    };
    let labels_included = flamegraph_svg.is_some();
    let contains = if is_max_private {
        PRIVATE_RECEIPT_CONTENTS.to_string()
    } else if labels_included {
        FULL_RECEIPT_WITH_FLAMEGRAPH.to_string()
    } else {
        FULL_RECEIPT_NO_FLAMEGRAPH.to_string()
    };
    let mut r = Receipt {
        receipt_version: RECEIPT_VERSION.to_string(),
        tare_version: tare_version.to_string(),
        pricing_version: report.pricing_version.clone(),
        effective_date: report.effective_date.clone(),
        privacy_policy_id: report.privacy_policy_id.clone().unwrap_or_default(),
        profile: report.profile.clone().unwrap_or_default(),
        scope,
        contains,
        labels_included,
        report,
        runs,
        flamegraph_svg,
        confidence,
        digest: 0,
    };
    r.digest = digest_of(&r);
    r
}

/// Prepare a receipt that honors `max_private` even when it exports runs captured under a less
/// restrictive profile. Pricing identities are the irreducible minimum for recomputation; all
/// other identity, fingerprint, structural-weight, and timing fields are discarded.
fn redact_max_private_runs(runs: &[RunRecord]) -> Vec<RunRecord> {
    runs.iter()
        .enumerate()
        .map(|(run_index, source)| {
            let run_id = format!("private-run-{}", run_index + 1);
            let mut run = source.clone();
            run.run_id.clone_from(&run_id);
            for step in &mut run.steps {
                step.run_id.clone_from(&run_id);
                step.shape.model.clone_from(&step.model);
                step.shape.provider = step.provider;
                step.shape.stream = false;
                step.shape.ttl = crate::model::CacheTtl::FiveMin;
                step.shape.has_cache_control = false;
                step.shape.cached_component = None;
                step.shape.system_hash = None;
                step.shape.weights.clear();
                step.shape.request_hash = None;
                step.shape.step_label = None;
                step.shape.component_label = None;
                step.shape.parent_label = None;
                step.shape.attempt = None;
                step.shape.session = None;
                step.shape.workload_key = None;
                step.shape.effort = None;
                step.shape.mcp_server = None;
                // `vendor` is intentionally retained: it participates in price lookup for
                // OpenAI-compatible models just as provider + model do.
                step.shape.commit = None;
                step.shape.author = None;
                step.stop_reason = None;
                step.duration_ms = 0;
                step.start_unix_nano = None;
                step.trace_id = None;
                step.span_id = None;
                step.parent_span_id = None;
            }
            run
        })
        .collect()
}

/// A successful verification's recomputed facts (for display).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyReport {
    pub scope: String,
    pub pricing_version: String,
    pub recomputed_total_micros: i64,
    pub rows: usize,
    pub flamegraph_checked: bool,
    pub digest: u64,
    /// When the supplied table's version differed from the receipt's, the `effective_date` used to
    /// reproduce the figure by repricing as of that day. `None` = exact-version recompute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconciled_as_of: Option<String>,
}

fn policy_id_for_profile(profile: &str) -> Option<String> {
    use crate::privacy::PrivacyPolicy;
    Some(match profile {
        "strict_counts" => PrivacyPolicy::strict_counts().policy_id(),
        "fingerprint" => PrivacyPolicy::fingerprint("").policy_id(),
        "max_private" => PrivacyPolicy::max_private().policy_id(),
        "max_inspect" => PrivacyPolicy::max_inspect().policy_id(),
        _ => return None,
    })
}

fn validate_receipt_metadata(r: &Receipt) -> Result<(), String> {
    if r.scope.trim().is_empty() {
        return Err("receipt scope must not be empty".into());
    }
    if r.report.pricing_version != r.pricing_version || r.report.effective_date != r.effective_date
    {
        return Err("receipt pricing metadata disagrees with the embedded report metadata".into());
    }
    if r.report.profile.as_deref().unwrap_or_default() != r.profile
        || r.report.privacy_policy_id.as_deref().unwrap_or_default() != r.privacy_policy_id
    {
        return Err("receipt privacy metadata disagrees with the embedded report metadata".into());
    }
    if r.profile.is_empty() != r.privacy_policy_id.is_empty() {
        return Err(
            "receipt privacy profile and policy id must either both be set or both be empty".into(),
        );
    }
    if !r.profile.is_empty() {
        let expected = policy_id_for_profile(&r.profile)
            .ok_or_else(|| format!("receipt has unknown privacy profile {:?}", r.profile))?;
        if r.privacy_policy_id != expected {
            return Err("receipt privacy policy id does not match its profile".into());
        }
    }
    if r.labels_included != r.flamegraph_svg.is_some() {
        return Err(
            "receipt labels_included must agree with whether a flamegraph is embedded".into(),
        );
    }
    let expected_contents = if r.profile == "max_private" {
        PRIVATE_RECEIPT_CONTENTS
    } else if r.labels_included {
        FULL_RECEIPT_WITH_FLAMEGRAPH
    } else {
        FULL_RECEIPT_NO_FLAMEGRAPH
    };
    if r.contains != expected_contents {
        return Err(
            "receipt contains-description does not match its actual profile and content".into(),
        );
    }
    if !r.report.estimated || !r.confidence.estimated {
        return Err("receipt dollar and confidence metadata must be marked estimated".into());
    }
    if !(0..=100).contains(&r.confidence.unpriced_token_share_pct)
        || r.confidence
            .coverage_share_pct
            .is_some_and(|share| !(0..=100).contains(&share))
    {
        return Err("receipt confidence percentages must be between 0 and 100".into());
    }
    let confidence = crate::confidence::confidence(
        r.confidence.pricing_age_days,
        r.confidence.unpriced_token_share_pct,
        r.confidence.coverage_status,
        r.confidence.coverage_share_pct,
    );
    if confidence.label != r.confidence.label {
        return Err("receipt confidence label disagrees with its component values".into());
    }

    let mut run_ids = BTreeSet::new();
    for (run_index, run) in r.runs.iter().enumerate() {
        if run.run_id.is_empty() || !run_ids.insert(run.run_id.as_str()) {
            return Err("receipt runs must have non-empty, unique ids".into());
        }
        let mut ordinals = BTreeSet::new();
        for step in &run.steps {
            if step.run_id != run.run_id {
                return Err("receipt step run id disagrees with its parent run".into());
            }
            if !ordinals.insert(step.step_ordinal) {
                return Err("receipt step ordinals must be unique within a run".into());
            }
            if step.shape.model != step.model || step.shape.provider != step.provider {
                return Err(
                    "receipt step pricing identity disagrees with its request shape".into(),
                );
            }
        }

        if r.profile == "max_private" {
            let expected_run_id = format!("private-run-{}", run_index + 1);
            if run.run_id != expected_run_id {
                return Err("max_private receipt carries a source run identifier".into());
            }
            for step in &run.steps {
                let shape = &step.shape;
                let carries_private_metadata = shape.stream
                    || shape.ttl != crate::model::CacheTtl::FiveMin
                    || shape.has_cache_control
                    || shape.cached_component.is_some()
                    || shape.system_hash.is_some()
                    || !shape.weights.is_empty()
                    || shape.request_hash.is_some()
                    || shape.step_label.is_some()
                    || shape.component_label.is_some()
                    || shape.parent_label.is_some()
                    || shape.attempt.is_some()
                    || shape.session.is_some()
                    || shape.workload_key.is_some()
                    || shape.effort.is_some()
                    || shape.mcp_server.is_some()
                    || shape.commit.is_some()
                    || shape.author.is_some()
                    || step.stop_reason.is_some()
                    || step.duration_ms != 0
                    || step.start_unix_nano.is_some()
                    || step.trace_id.is_some()
                    || step.span_id.is_some()
                    || step.parent_span_id.is_some();
                if carries_private_metadata {
                    return Err(
                        "max_private receipt carries correlation, hash, weight, or timing metadata"
                            .into(),
                    );
                }
            }
        }
    }
    if r.profile == "max_private" {
        let expected_scope = if r.runs.len() == 1 {
            "single-run"
        } else {
            "all-runs"
        };
        if r.scope != expected_scope || r.labels_included || r.flamegraph_svg.is_some() {
            return Err("max_private receipt scope or flamegraph metadata is not redacted".into());
        }
    }
    Ok(())
}

/// Verify a receipt OFFLINE against the recipient's `pricing`. Returns the recomputed facts on
/// success, or a precise reason on failure. Pure and deterministic.
pub fn verify(json: &str, pricing: &PricingTable) -> Result<VerifyReport, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("receipt json: {e}"))?;
    let obj = v.as_object().ok_or("receipt must be a JSON object")?;
    for k in obj.keys() {
        if !RECEIPT_KEYS.contains(&k.as_str()) {
            return Err(format!("receipt carries non-allowlisted key {k:?}"));
        }
    }
    // Typed round trip: no smuggled fields, and the type has no payload-text field.
    let r: Receipt = serde_json::from_value(v.clone()).map_err(|e| format!("receipt: {e}"))?;
    let typed = serde_json::to_value(&r).map_err(|e| format!("receipt serialization: {e}"))?;
    if typed != v {
        return Err(
            "receipt JSON does not match the closed typed shape (unknown or noncanonical field)"
                .into(),
        );
    }

    if r.receipt_version != RECEIPT_VERSION {
        return Err(format!(
            "unsupported receipt version {:?} (this build verifies {RECEIPT_VERSION})",
            r.receipt_version
        ));
    }
    validate_receipt_metadata(&r)?;
    // 1) Content digest (corruption / un-propagated-edit check).
    let want = digest_of(&r);
    if want != r.digest {
        return Err(format!(
            "digest mismatch: recomputed {want:#018x} != embedded {:#018x} (receipt was altered)",
            r.digest
        ));
    }
    // 2) Pricing reconciliation. An exact-version table recomputes directly — the common
    // case, byte-identical to before. A DIFFERENT table is accepted only if repricing as-of the
    // receipt's own `effective_date` reproduces the figure: a version-pinned historical receipt
    // reconciles against a current multi-edition table at CONTEMPORANEOUS rates, and an incompatible
    // table still fails honestly on the total check below. Never a wrong number over an honest gap.
    let reconciled_as_of = if pricing.version == r.pricing_version {
        None
    } else {
        Some(r.effective_date.clone())
    };
    let dated;
    let table: &PricingTable = match &reconciled_as_of {
        None => pricing,
        Some(date) => {
            dated = pricing.as_of(date);
            &dated
        }
    };
    // 3) Re-derive the report from the embedded vectors and check it equals the embedded report.
    let recomputed = build_report(&r.runs, table);
    if recomputed.total_micros != r.report.total_micros {
        let how = match &reconciled_as_of {
            None => String::new(),
            Some(date) => format!(
                " (repriced as-of {date} from table {:?}; that edition doesn't reproduce the receipt)",
                pricing.version
            ),
        };
        return Err(format!(
            "total mismatch: embedded {} != recomputed {}{how} (vectors don't support the claimed cost)",
            r.report.total_micros, recomputed.total_micros
        ));
    }
    if recomputed.rows != r.report.rows
        || recomputed.unpriced != r.report.unpriced
        || recomputed.attribution_confidence != r.report.attribution_confidence
    {
        return Err("attribution mismatch: recomputed rows differ from the embedded report".into());
    }
    // 4) Recompute and check that the sum of disjoint cause rows does not exceed the total. The
    // `coarse-attribution` row is an informational coverage subtotal that deliberately overlaps
    // specific causes (for example, unread cache writes), so including it here rejects valid
    // out-of-band and max-private receipts.
    let row_sum: i128 = recomputed
        .rows
        .iter()
        .filter(|row| row.cause != "coarse-attribution")
        .map(|row| row.micros as i128)
        .sum();
    if row_sum > recomputed.total_micros as i128 {
        return Err(format!(
            "Σrows ({row_sum}) exceeds total ({}) — accounting is not disjoint",
            recomputed.total_micros
        ));
    }
    // 5) Regenerate the flamegraph with the byte-identical renderer and diff it (#3 load-bearing).
    let mut flamegraph_checked = false;
    if let Some(embedded_svg) = &r.flamegraph_svg {
        if r.runs.len() != 1 {
            return Err("flamegraph receipts cover exactly one run".into());
        }
        let regen = svg::render_svg(&build_flamegraph(&r.runs[0], table));
        if &regen != embedded_svg {
            return Err(
                "flamegraph mismatch: regenerated SVG differs from the embedded one".into(),
            );
        }
        flamegraph_checked = true;
    }
    Ok(VerifyReport {
        scope: r.scope,
        pricing_version: r.pricing_version,
        recomputed_total_micros: recomputed.total_micros,
        rows: recomputed.rows.len(),
        flamegraph_checked,
        digest: r.digest,
        reconciled_as_of,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest_step;
    use crate::model::{CacheTtl, Provider, RequestShape, StepRecord, UsageTokens};

    fn pricing() -> PricingTable {
        PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml")).unwrap()
    }
    fn conf() -> EstimateConfidence {
        // fresh table, 3% unpriced, full coverage with a measured denominator
        crate::confidence::confidence(12, 3, crate::confidence::CoverageStatus::Full, Some(100))
    }
    fn fixture(rel: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("fixtures")
                .join(rel),
        )
        .unwrap()
    }
    fn one_run() -> Vec<RunRecord> {
        vec![RunRecord {
            run_id: "r1".into(),
            steps: vec![ingest_step(
                "r1",
                1,
                Provider::Anthropic,
                &fixture("anthropic_stream/request.json"),
                &fixture("anthropic_stream/response.sse"),
            )
            .unwrap()],
        }]
    }

    #[test]
    fn round_trips_attest_then_verify() {
        let runs = one_run();
        let report = build_report(&runs, &pricing());
        let r = attest(&runs, &report, &pricing(), "test", "run:r1", true, conf());
        let json = serde_json::to_string(&r).unwrap();
        let v = verify(&json, &pricing()).unwrap();
        assert_eq!(v.recomputed_total_micros, report.total_micros);
        assert!(
            v.flamegraph_checked,
            "full receipt regenerates+diffs the SVG"
        );
    }

    // date-keyed provenance — a version-pinned historical receipt reproduces its exact
    // figure against a current multi-edition table by repricing as-of its own effective_date.
    fn m_step() -> StepRecord {
        StepRecord {
            run_id: "r1".into(),
            step_ordinal: 1,
            provider: Provider::Anthropic,
            model: "m".into(),
            usage: UsageTokens {
                fresh_input: 1_000_000,
                ..Default::default()
            },
            shape: RequestShape {
                model: "m".into(),
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
        }
    }
    const OLD_TABLE: &str = r#"
version = "2026-06-01"
effective_date = "2026-06-01"
[[model]]
provider = "anthropic"
model_id = "m"
input_micro_per_mtok = 3000000
output_micro_per_mtok = 0
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
"#;

    #[test]
    fn reconciles_a_historical_receipt_against_a_newer_multi_edition_table() {
        let runs = vec![RunRecord {
            run_id: "r1".into(),
            steps: vec![m_step()],
        }];
        let old = PricingTable::from_toml_str(OLD_TABLE).unwrap();
        let report = build_report(&runs, &old);
        assert_eq!(report.total_micros, 3_000_000, "1M input @ $3/Mtok");
        let r = attest(&runs, &report, &old, "test", "run:r1", false, conf());
        let json = serde_json::to_string(&r).unwrap();

        // Current table: the newer 2026-07-01 edition is listed FIRST, so a naive first-row lookup
        // would pick the wrong $6 rate — only as-of repricing reproduces the $3 historical figure.
        let multi = PricingTable::from_toml_str(
            r#"
version = "2026-07-01"
effective_date = "2026-07-01"
[[model]]
provider = "anthropic"
model_id = "m"
effective_date = "2026-07-01"
input_micro_per_mtok = 6000000
output_micro_per_mtok = 0
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
[[model]]
provider = "anthropic"
model_id = "m"
effective_date = "2026-06-01"
input_micro_per_mtok = 3000000
output_micro_per_mtok = 0
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
"#,
        )
        .unwrap();
        let v = verify(&json, &multi).unwrap();
        assert_eq!(
            v.recomputed_total_micros, 3_000_000,
            "reproduced at contemporaneous rates"
        );
        assert_eq!(v.reconciled_as_of.as_deref(), Some("2026-06-01"));
        assert_eq!(
            v.pricing_version, "2026-06-01",
            "reports the receipt's own pinned version"
        );
    }

    #[test]
    fn a_table_without_the_contemporaneous_edition_fails_honestly() {
        let runs = vec![RunRecord {
            run_id: "r1".into(),
            steps: vec![m_step()],
        }];
        let old = PricingTable::from_toml_str(OLD_TABLE).unwrap();
        let report = build_report(&runs, &old);
        let r = attest(&runs, &report, &old, "test", "run:r1", false, conf());
        let json = serde_json::to_string(&r).unwrap();
        // A newer-only table can't reprice back to 2026-06-01 → honest total mismatch, not a wrong OK.
        let newer_only = PricingTable::from_toml_str(
            r#"
version = "2026-07-01"
effective_date = "2026-07-01"
[[model]]
provider = "anthropic"
model_id = "m"
input_micro_per_mtok = 6000000
output_micro_per_mtok = 0
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
"#,
        )
        .unwrap();
        let err = verify(&json, &newer_only).unwrap_err();
        assert!(err.contains("total mismatch"), "got: {err}");
        assert!(
            err.contains("repriced as-of 2026-06-01"),
            "names the provenance attempt: {err}"
        );
    }

    #[test]
    fn a_hand_edited_total_fails_verify() {
        let runs = one_run();
        let report = build_report(&runs, &pricing());
        let r = attest(&runs, &report, &pricing(), "test", "run:r1", true, conf());
        let mut v: serde_json::Value = serde_json::to_value(&r).unwrap();
        // Tamper the claimed total but leave the digest (forger forgot to recompute it).
        v["report"]["total_micros"] = serde_json::json!(1);
        let err = verify(&v.to_string(), &pricing()).unwrap_err();
        assert!(err.contains("digest mismatch"), "got: {err}");
    }

    #[test]
    fn a_consistent_total_edit_fails_on_recompute() {
        // Even if a forger recomputes the digest, the vectors no longer support the cost.
        let runs = one_run();
        let report = build_report(&runs, &pricing());
        let r = attest(&runs, &report, &pricing(), "test", "run:r1", false, conf());
        let mut tampered = r.clone();
        tampered.report.total_micros = r.report.total_micros + 999_999;
        tampered.digest = digest_of(&tampered); // re-seal consistently
        let json = serde_json::to_string(&tampered).unwrap();
        let err = verify(&json, &pricing()).unwrap_err();
        assert!(err.contains("total mismatch"), "got: {err}");
    }

    #[test]
    fn max_private_receipt_verifies_and_carries_no_flamegraph() {
        let mut runs = one_run();
        let source_model = runs[0].steps[0].model.clone();
        let step = &mut runs[0].steps[0];
        step.shape.step_label = Some("private-step-label".into());
        step.shape.session = Some("private-session".into());
        step.shape.commit = Some("private-commit".into());
        step.trace_id = Some("private-trace".into());
        step.duration_ms = 123;
        let report = build_report(&runs, &pricing())
            .with_privacy(&crate::privacy::PrivacyPolicy::max_private());
        let r = attest(&runs, &report, &pricing(), "test", "run:r1", false, conf());
        assert!(r.flamegraph_svg.is_none());
        assert_eq!(r.scope, "single-run");
        assert_eq!(r.runs[0].run_id, "private-run-1");
        assert_eq!(r.runs[0].steps[0].model, source_model);
        assert_eq!(
            r.runs[0].steps[0].provider, runs[0].steps[0].provider,
            "pricing identities must remain recomputable"
        );
        assert!(r.runs[0].steps[0].shape.step_label.is_none());
        assert!(r.runs[0].steps[0].shape.session.is_none());
        assert!(r.runs[0].steps[0].shape.commit.is_none());
        assert!(r.runs[0].steps[0].trace_id.is_none());
        assert_eq!(r.runs[0].steps[0].duration_ms, 0);
        let serialized = serde_json::to_string(&r).unwrap();
        for secret in [
            "private-step-label",
            "private-session",
            "private-commit",
            "private-trace",
            "run:r1",
        ] {
            assert!(!serialized.contains(secret), "leaked {secret}");
        }
        let v = verify(&serialized, &pricing()).unwrap();
        assert!(!v.flamegraph_checked);
    }

    #[test]
    fn verifier_rejects_false_content_and_confidence_claims() {
        let runs = one_run();
        let report = build_report(&runs, &pricing());
        let mut receipt = attest(&runs, &report, &pricing(), "test", "run:r1", false, conf());
        receipt.contains = "nothing sensitive".into();
        receipt.digest = digest_of(&receipt);
        let error = verify(&serde_json::to_string(&receipt).unwrap(), &pricing()).unwrap_err();
        assert!(error.contains("contains-description"), "got: {error}");

        let mut receipt = attest(&runs, &report, &pricing(), "test", "run:r1", false, conf());
        receipt.confidence.label = "high".into();
        receipt.confidence.pricing_age_days = 999;
        receipt.digest = digest_of(&receipt);
        let error = verify(&serde_json::to_string(&receipt).unwrap(), &pricing()).unwrap_err();
        assert!(error.contains("confidence label"), "got: {error}");
    }

    #[test]
    fn non_allowlisted_key_fails_scan() {
        let runs = one_run();
        let report = build_report(&runs, &pricing());
        let r = attest(&runs, &report, &pricing(), "test", "run:r1", false, conf());
        let mut v: serde_json::Value = serde_json::to_value(&r).unwrap();
        v["leak"] = serde_json::json!("payload");
        assert!(verify(&v.to_string(), &pricing())
            .unwrap_err()
            .contains("non-allowlisted"));
    }

    #[test]
    fn nested_unknown_field_fails_closed_shape_scan() {
        let runs = one_run();
        let report = build_report(&runs, &pricing());
        let receipt = attest(&runs, &report, &pricing(), "test", "run:r1", false, conf());
        let mut value = serde_json::to_value(&receipt).unwrap();
        value["runs"][0]["steps"][0]["payload"] = serde_json::json!("must not be ignored");
        let error = verify(&value.to_string(), &pricing()).unwrap_err();
        assert!(error.contains("closed typed shape"), "got: {error}");
    }

    #[test]
    fn receipt_metadata_must_be_internally_consistent() {
        let runs = one_run();
        let report = build_report(&runs, &pricing());
        let mut receipt = attest(&runs, &report, &pricing(), "test", "run:r1", false, conf());
        receipt.report.pricing_version = "different".into();
        receipt.digest = digest_of(&receipt);
        let error = verify(&serde_json::to_string(&receipt).unwrap(), &pricing()).unwrap_err();
        assert!(error.contains("pricing metadata"), "got: {error}");

        let mut receipt = attest(&runs, &report, &pricing(), "test", "run:r1", true, conf());
        receipt.labels_included = false;
        receipt.digest = digest_of(&receipt);
        let error = verify(&serde_json::to_string(&receipt).unwrap(), &pricing()).unwrap_err();
        assert!(error.contains("labels_included"), "got: {error}");
    }

    #[test]
    fn multi_run_receipt_does_not_claim_a_missing_flamegraph() {
        let mut runs = one_run();
        let mut second = runs[0].clone();
        second.run_id = "r2".into();
        for step in &mut second.steps {
            step.run_id = "r2".into();
        }
        runs.push(second);
        let report = build_report(&runs, &pricing());
        let receipt = attest(&runs, &report, &pricing(), "test", "all-runs", true, conf());
        assert!(receipt.flamegraph_svg.is_none());
        assert!(!receipt.labels_included);
        assert!(!receipt.contains.contains(" + flamegraph"));
    }
}
