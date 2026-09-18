// Opportunity evidence inspector. Every opportunity claim below
// is read from OpportunityV2, its persisted normative action snapshot, or the provenance-bearing
// exact-cohort resolve. Missing range/coverage/recurrence evidence stays visibly unavailable.

import { el, type Attrs } from "../ui/el.js";
import { fmtUsd, humanizeKey, toDollarString } from "../ui/format.js";
import type {
  AnalysisResponse,
  CohortFilter,
  CohortResolveResult,
  CohortSpec,
  SavingsAction,
  SavingsActionRequest,
} from "../analysis/types.js";
import type { OpportunityV2 } from "../client.js";

type ActionSnapshot = SavingsAction | SavingsActionRequest;

export interface OpportunityEvidenceOptions {
  opportunity: OpportunityV2;
  snapshot: ActionSnapshot;
  resolved?: AnalysisResponse<CohortResolveResult>;
  resolutionFailed?: boolean;
  drillAttributes: Attrs;
}

function cohortWindow(cohort: CohortSpec): string {
  if (cohort.from && cohort.to) {
    return cohort.from === cohort.to ? cohort.from : `${cohort.from}–${cohort.to}`;
  }
  if (cohort.from) return `From ${cohort.from}; no end date supplied`;
  if (cohort.to) return `Through ${cohort.to}; no start date supplied`;
  return "Not bounded; evidence covers all captured dates";
}

function filterText(filter: CohortFilter): string {
  switch (filter.op) {
    case "eq":
      return `${humanizeKey(filter.dimension)} = ${filter.value}`;
    case "in":
      return `${humanizeKey(filter.dimension)} in {${filter.values.join(", ")}}`;
    case "gte_micros":
      return `Spend at least ${fmtUsd(filter.value)}`;
    case "lte_micros":
      return `Spend at most ${fmtUsd(filter.value)}`;
    case "run_ids":
      return `Run IDs: ${filter.ids.join(", ")}`;
    case "step_refs":
      return `Steps: ${filter.refs.map((ref) => `${ref.run_id}#${ref.step_ordinal}`).join(", ")}`;
    case "tag":
      return `Tag = ${filter.value}`;
    case "quality_range":
      return `Quality ${filter.min ?? "unbounded"}–${filter.max ?? "unbounded"}`;
  }
}

function pricingText(cohort: CohortSpec): string {
  return cohort.pricing.mode === "as_of"
    ? `As of ${cohort.pricing.date}`
    : humanizeKey(cohort.pricing.mode);
}

function outcomeText(cohort: CohortSpec): string {
  const outcome = cohort.outcome_denominator;
  if (!outcome) return "None";
  return outcome.kind === "work_unit"
    ? `Work unit · ${outcome.name}`
    : `Metered · ${humanizeKey(outcome.metered)}`;
}

function rangeText(opportunity: OpportunityV2, snapshot: ActionSnapshot): string {
  const low = snapshot.expected_low_micros;
  const point = snapshot.expected_point_micros ?? opportunity.recoverable_micros;
  const high = snapshot.expected_high_micros;
  if (low != null || high != null) {
    return [
      low == null ? "Conservative not supplied" : `Conservative ${fmtUsd(low)}`,
      `point ${fmtUsd(point)}`,
      high == null ? "high not supplied" : `high ${fmtUsd(high)}`,
      "saved action snapshot",
    ].join(" · ");
  }
  return `Point ${fmtUsd(opportunity.recoverable_micros)} · conservative/high not supplied by detector`;
}

function evidenceFacts(options: OpportunityEvidenceOptions): HTMLElement {
  const { opportunity, resolved, resolutionFailed, snapshot } = options;
  const currentSpend = resolved?.data.total_micros;
  const share =
    currentSpend != null && currentSpend > 0
      ? `${Number(((opportunity.recoverable_micros / currentSpend) * 100).toFixed(1))}% of resolved exact-cohort spend`
      : resolutionFailed
        ? "Unavailable; exact cohort could not be resolved"
        : "Unavailable; resolved exact-cohort spend is zero";
  const spend =
    currentSpend == null
      ? "Unavailable; exact cohort could not be resolved"
      : fmtUsd(currentSpend);

  return el("dl", { class: "optimize-evidence-facts" }, [
    el("div", {}, [
      el("dt", { text: "Affected current spend" }),
      el("dd", {
        class: currentSpend == null ? "" : "num",
        text: spend,
        ...(currentSpend == null ? {} : { title: toDollarString(currentSpend) }),
      }),
    ]),
    el("div", {}, [
      el("dt", { text: "Savings range" }),
      el("dd", { text: rangeText(opportunity, snapshot) }),
    ]),
    el("div", {}, [
      el("dt", { text: "Percentage of scoped spend" }),
      el("dd", { text: share }),
    ]),
    el("div", {}, [
      el("dt", { text: "Recurrence window" }),
      el("dd", { text: cohortWindow(opportunity.cohort_snapshot) }),
    ]),
  ]);
}

function evidenceReferences(opportunity: OpportunityV2, drillAttributes: Attrs): HTMLElement {
  const runRefs = opportunity.affected_run_ids;
  const stepRefs = opportunity.affected_steps;
  const section = el("section", { class: "optimize-evidence-section" }, [
    el("h5", { text: "Affected evidence" }),
    el("p", {
      class: "caption",
      text: `${opportunity.affected_run_count} affected runs · ${opportunity.affected_step_count} affected steps · ${opportunity.evidence_method}`,
    }),
  ]);

  const refs = el("div", { class: "optimize-reference-groups" });
  const runDetails = el("details", {}, [
    el("summary", {
      text: `Run references · ${runRefs.length} of ${opportunity.affected_run_count} inline`,
    }),
  ]);
  if (runRefs.length > 0) {
    runDetails.appendChild(
      el(
        "ul",
        { class: "optimize-reference-list" },
        runRefs.map((runId) => el("li", {}, [el("code", { text: runId })]))
      )
    );
  } else {
    runDetails.appendChild(el("p", { class: "caption", text: "No inline run references supplied." }));
  }
  refs.appendChild(runDetails);

  const stepDetails = el("details", {}, [
    el("summary", {
      text: `Step references · ${stepRefs.length} of ${opportunity.affected_step_count} inline`,
    }),
  ]);
  if (stepRefs.length > 0) {
    stepDetails.appendChild(
      el(
        "ul",
        { class: "optimize-reference-list" },
        stepRefs.map((ref) =>
          el("li", {}, [el("code", { text: `${ref.run_id}#${ref.step_ordinal}` })])
        )
      )
    );
  } else {
    stepDetails.appendChild(el("p", { class: "caption", text: "No inline step references supplied." }));
  }
  refs.appendChild(stepDetails);
  section.appendChild(refs);

  if (opportunity.evidence_truncated) {
    section.appendChild(
      el("p", {
        class: "optimize-row-warning",
        text: "Inline evidence is truncated. Counts remain complete; use the exact cohort drill-through for the complete set.",
      })
    );
  }
  section.appendChild(
    el("a", {
      class: "btn",
      text: opportunity.evidence_truncated
        ? "Inspect complete exact cohort"
        : "Open exact cohort in Investigate",
      "data-action": "drill-evidence",
      ...drillAttributes,
    })
  );
  return section;
}

function confidenceSection(opportunity: OpportunityV2): HTMLElement {
  const section = el("section", { class: "optimize-evidence-section" }, [
    el("h5", { text: "Method and confidence basis" }),
    el("dl", { class: "optimize-evidence-pairs" }, [
      el("dt", { text: "Evidence method" }),
      el("dd", { text: opportunity.evidence_method }),
      el("dt", { text: "Detector confidence" }),
      el("dd", { text: humanizeKey(opportunity.confidence) }),
      el("dt", { text: "Effort" }),
      el("dd", { text: opportunity.effort }),
      el("dt", { text: "Quality risk" }),
      el("dd", { text: opportunity.quality_risk ?? "None supplied by detector" }),
    ]),
    el("h6", { text: "Detector assumptions" }),
  ]);
  if (opportunity.assumptions.length === 0) {
    section.appendChild(el("p", { class: "caption", text: "No assumptions supplied by detector." }));
  } else {
    section.appendChild(
      el(
        "ul",
        { class: "optimize-assumption-list" },
        opportunity.assumptions.map((assumption) => el("li", { text: assumption }))
      )
    );
  }
  return section;
}

function exactCohortSection(cohort: CohortSpec): HTMLElement {
  const section = el("section", { class: "optimize-evidence-section" }, [
    el("h5", { text: "Exact cohort snapshot" }),
    el("dl", { class: "optimize-evidence-pairs" }, [
      el("dt", { text: "Dates" }),
      el("dd", { text: cohortWindow(cohort) }),
      el("dt", { text: "Timezone" }),
      el("dd", { text: cohort.timezone }),
      el("dt", { text: "Entity" }),
      el("dd", { text: humanizeKey(cohort.entity) }),
      el("dt", { text: "Metric" }),
      el("dd", { text: humanizeKey(cohort.metric) }),
      el("dt", { text: "Normalization" }),
      el("dd", { text: humanizeKey(cohort.normalization) }),
      el("dt", { text: "Outcome denominator" }),
      el("dd", { text: outcomeText(cohort) }),
      el("dt", { text: "Pricing mode" }),
      el("dd", { text: pricingText(cohort) }),
    ]),
    el("h6", { text: `Filters · ${cohort.filters.length}` }),
  ]);
  if (cohort.filters.length === 0) {
    section.appendChild(el("p", { class: "caption", text: "No filters; the captured date scope is used." }));
  } else {
    section.appendChild(
      el(
        "ul",
        { class: "optimize-cohort-filter-list" },
        cohort.filters.map((filter) => el("li", { text: filterText(filter) }))
      )
    );
  }
  return section;
}

function actionSnapshotSection(snapshot: ActionSnapshot): HTMLElement {
  const stored = "status" in snapshot;
  const match = snapshot.match.kind === "aggregate_only"
    ? "Aggregate only"
    : snapshot.match.kind === "workload_key"
      ? `Workload key${snapshot.match.key ? ` · ${snapshot.match.key}` : ""}`
      : `Template lineage · ${snapshot.match.hash}`;
  const section = el("section", { class: "optimize-evidence-section" }, [
    el("h5", { text: stored ? "Stored action snapshot" : "Action snapshot to persist" }),
    el("p", {
      class: "caption",
      text: stored
        ? `${humanizeKey(snapshot.status)} at ${snapshot.acted_at}`
        : "Mark applied and Dismiss both persist this normative request; neither uses the looser workspace scope.",
    }),
    el("dl", { class: "optimize-evidence-pairs" }, [
      el("dt", { text: "Opportunity key" }),
      el("dd", {}, [el("code", { text: snapshot.opportunity_key })]),
      el("dt", { text: "Metric / normalization" }),
      el("dd", { text: `${humanizeKey(snapshot.metric)} / ${humanizeKey(snapshot.normalization)}` }),
      el("dt", { text: "Exact action cohort" }),
      el("dd", {
        text: `${cohortWindow(snapshot.cohort)} · ${snapshot.cohort.timezone} · ${snapshot.cohort.filters.length} filter${snapshot.cohort.filters.length === 1 ? "" : "s"}`,
      }),
      el("dt", { text: "Match rule" }),
      el("dd", { text: match }),
      el("dt", { text: "Baseline" }),
      el("dd", {
        text: snapshot.baseline
          ? `${snapshot.baseline.label}${snapshot.baseline.sample_count == null ? "" : ` · ${snapshot.baseline.sample_count} samples`} · ${cohortWindow(snapshot.baseline.cohort)} · ${snapshot.baseline.cohort.timezone}`
          : "None supplied",
      }),
      el("dt", { text: "Outcome denominator" }),
      el("dd", {
        text: snapshot.outcome_denominator == null
          ? "None"
          : snapshot.outcome_denominator.kind === "work_unit"
            ? `Work unit · ${snapshot.outcome_denominator.name}`
            : `Metered · ${humanizeKey(snapshot.outcome_denominator.metered)}`,
      }),
      el("dt", { text: "Quality guardrail" }),
      el("dd", {
        text: snapshot.quality_guardrail == null ? "Not supplied" : String(snapshot.quality_guardrail),
      }),
    ]),
  ]);
  return section;
}

function provenanceSection(resolved: AnalysisResponse<CohortResolveResult> | undefined): HTMLElement {
  const section = el("section", { class: "optimize-evidence-section" }, [
    el("h5", { text: "Resolve provenance" }),
  ]);
  if (!resolved) {
    section.appendChild(
      el("p", {
        class: "optimize-row-warning",
        text: "Exact-cohort resolution failed. Current spend, spend share, coverage, and pricing provenance are unavailable; no completeness claim is made.",
      })
    );
    return section;
  }

  const provenance = resolved.provenance;
  const priced = provenance.priced_token_share_pct == null
    ? "Not reported; unpriced-token share is unknown"
    : `${Number(provenance.priced_token_share_pct.toFixed(1))}% priced · ${Number((100 - provenance.priced_token_share_pct).toFixed(1))}% usage-only and excluded from dollars`;
  section.append(
    el("dl", { class: "optimize-evidence-pairs" }, [
      el("dt", { text: "Resolved entities" }),
      el("dd", { text: `${resolved.data.run_count} runs · ${resolved.data.step_count} steps` }),
      el("dt", { text: "Coverage" }),
      el("dd", {
        text: provenance.coverage_status === "unknown"
          ? "Unknown; no defensible completeness denominator"
          : humanizeKey(provenance.coverage_status),
      }),
      el("dt", { text: "Priced tokens" }),
      el("dd", { text: priced }),
      el("dt", { text: "Capture sources" }),
      el("dd", { text: provenance.capture_sources.join(", ") || "None reported" }),
      el("dt", { text: "Value basis" }),
      el("dd", { text: `${humanizeKey(provenance.value_class)} · ${humanizeKey(provenance.allocation_method)}` }),
      el("dt", { text: "Component fidelity" }),
      el("dd", { text: humanizeKey(provenance.component_fidelity) }),
      el("dt", { text: "Pricing edition" }),
      el("dd", {
        text: `${provenance.pricing_edition.version} · effective ${provenance.pricing_edition.effective_date} · ${humanizeKey(provenance.pricing_edition.mode)}`,
      }),
      el("dt", { text: "Refreshed" }),
      el("dd", { text: provenance.refreshed_at }),
    ])
  );
  if (provenance.assumptions.length > 0) {
    section.append(
      el("h6", { text: "Provenance assumptions" }),
      el(
        "ul",
        { class: "optimize-assumption-list" },
        provenance.assumptions.map((assumption) => el("li", { text: assumption }))
      )
    );
  }
  return section;
}

export function renderOpportunityEvidence(options: OpportunityEvidenceOptions): HTMLElement {
  const { opportunity, resolved, resolutionFailed, drillAttributes, snapshot } = options;
  return el("div", { class: "optimize-evidence-body" }, [
    el("h4", { class: "optimize-evidence-title", text: "Evidence and action inspector" }),
    evidenceFacts({ opportunity, resolved, resolutionFailed, drillAttributes, snapshot }),
    evidenceReferences(opportunity, drillAttributes),
    confidenceSection(opportunity),
    exactCohortSection(opportunity.cohort_snapshot),
    actionSnapshotSection(snapshot),
    provenanceSection(resolved),
  ]);
}
