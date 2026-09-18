// Investigate cohort inspector. "Explain a cohort": distinguishing
// dimensions (facet lift / support / spend / missing), volume/size/efficiency decomposition,
// provenance, and representative runs (largest / most-anomalous / median). Everything reflects ONE
// authoritative selection cohort and its baseline — the inspector never invents a second scope.
//
// Honesty contract: distinguishing factors are ASSOCIATION, never called "cause";
// the aggregate-only comparison always surfaces its confounding warning; numbers come straight from
// the cohort APIs (no client-side re-derivation of spend). Framework-free + jsdom-testable — the
// ranking + representative math are pure exports.

import { el } from "../ui/el.js";
import { fmtUsd, fmtTokens, fmtPct } from "../ui/format.js";
import { routePath } from "../ui/store.js";
import type { TareClient } from "../client.js";
import type {
  CohortSpec,
  CohortDimension,
  FacetRow,
  CohortEntitySummary,
  AnalysisProvenance,
} from "../analysis/types.js";

// Dimensions probed for distinguishing factors — the entity-independent ones worth surfacing.
const DISTINGUISH_DIMS: CohortDimension[] = ["model", "provider", "tool", "session", "template"];

/// One distinguishing factor: a dimension value the selection over-represents vs its baseline. Carries
/// association metrics (lift / support / spend share / missing rate) — NOT a causal claim.
export interface Factor {
  dimension: CohortDimension;
  value: string;
  lift: number; // selection spend-share ÷ baseline spend-share (>1 = over-represented)
  support: number; // runs/steps in the selection carrying this value
  micros: number; // selection spend for this value
  missingPct: number; // share of the selection missing this dimension (honest coverage)
  deltaSharePoints: number; // selection − baseline support-share points (the ranking key)
}

/// Rank facet rows into distinguishing factors: keep values the SELECTION over-represents
/// (delta_support_share_points > 0), ranked by that delta. Pure + deterministic. Exported for tests.
export function rankFactors(dimension: CohortDimension, rows: FacetRow[], topN = 3): Factor[] {
  return rows
    .filter((r) => r.delta_support_share_points > 0 && r.selection_support > 0)
    .sort((a, b) => b.delta_support_share_points - a.delta_support_share_points)
    .slice(0, topN)
    .map((r) => ({
      dimension,
      value: r.value || "(none)",
      lift: r.lift_ratio ?? (r.baseline_spend_share_pct > 0 ? r.selection_spend_share_pct / r.baseline_spend_share_pct : 0),
      support: r.selection_support,
      micros: r.selection_micros,
      missingPct: r.selection_missing_pct,
      deltaSharePoints: r.delta_support_share_points,
    }));
}

/// Representative runs of a cohort: the LARGEST contributor, the MOST-ANOMALOUS (furthest from
/// the mean spend), and the MEDIAN. Pure. Returns null for an empty cohort.
export interface Representatives {
  largest: CohortEntitySummary;
  mostAnomalous: CohortEntitySummary;
  median: CohortEntitySummary;
}
export function representatives(entities: CohortEntitySummary[]): Representatives | null {
  if (entities.length === 0) return null;
  const sorted = [...entities].sort((a, b) => b.matched_micros - a.matched_micros);
  const mean = entities.reduce((s, e) => s + e.matched_micros, 0) / entities.length;
  const mostAnomalous = entities.reduce(
    (best, e) => (Math.abs(e.matched_micros - mean) > Math.abs(best.matched_micros - mean) ? e : best),
    entities[0]
  );
  return { largest: sorted[0], mostAnomalous, median: sorted[Math.floor(sorted.length / 2)] };
}

function runLink(runId: string, query: Record<string, string>): HTMLElement {
  return el("a", {
    class: "insp-run",
    href: routePath(["investigate", "run", runId], query),
    text: runId,
  });
}

function repRow(label: string, e: CohortEntitySummary, query: Record<string, string>): HTMLElement {
  const id = e.entity.step_ordinal != null ? `${e.entity.run_id}#${e.entity.step_ordinal}` : e.entity.run_id;
  return el("li", { class: "insp-rep" }, [
    el("span", { class: "insp-rep-label sub", text: label }),
    runLink(e.entity.run_id, query),
    el("span", { class: "insp-rep-spend num", text: fmtUsd(e.matched_micros), title: id }),
  ]);
}

/// Render the inspector for ONE selection cohort vs its baseline. Async: pulls facets (per dimension),
/// the aggregate compare decomposition, and the resolved entity rows for representatives — all from the
/// cohort APIs, all scoped to the same selection.
export async function renderInspector(
  client: TareClient,
  selection: CohortSpec,
  baseline: CohortSpec,
  query: Record<string, string> = {},
  options: { comparisonReady?: boolean } = {}
): Promise<HTMLElement> {
  const comparisonReady = options.comparisonReady ?? true;
  const aside = el("aside", { class: "inv-inspector", "aria-label": "Cohort inspector" }, [
    el("h2", { class: "subhead", text: comparisonReady ? "Explain this cohort" : "About this scope" }),
  ]);

  // Resolve the selection once (entity rows for representatives + its provenance).
  const resolved = await client.resolveCohort({ ...selection, entity: "run" });
  const reps = representatives(resolved.data.entity_rows);

  // With neither Selection A nor Baseline B set, comparing the whole scope to itself only produced
  // a screenful of $0.00 deltas. Give the user a useful next step and skip seven redundant API calls;
  // representatives + provenance below still describe the current scope.
  if (!comparisonReady) {
    aside.appendChild(
      el("section", { class: "insp-start" }, [
        el("h3", { class: "subhead sub", text: "Start with a selection" }),
        el("p", {
          class: "caption sub",
          text: "Choose a model or provider filter, or set Selection A. Then this pane will explain what differs from the full scope or Baseline B.",
        }),
      ])
    );
  } else {
    // --- Distinguishing factors (association, NOT cause) ---
    const factorsSec = el("section", { class: "insp-factors" }, [
      el("h3", { class: "subhead sub", text: "Distinguishing factors" }),
      el("p", { class: "caption sub", text: "What this cohort over-represents vs the rest — an association, not a proven cause." }),
    ]);
    const facetResults = await Promise.all(
      DISTINGUISH_DIMS.map((d) => client.facetCohort({ selection, baseline, dimension: d }).then((r) => rankFactors(d, r.data.rows)).catch(() => [] as Factor[]))
    );
    const factors = facetResults.flat().sort((a, b) => b.deltaSharePoints - a.deltaSharePoints).slice(0, 6);
    if (factors.length === 0) {
      factorsSec.appendChild(el("p", { class: "caption sub", text: "No dimension distinguishes this cohort from the baseline." }));
    } else {
      const list = el("ul", { class: "insp-factor-list" });
      for (const f of factors) {
        list.appendChild(
          el("li", { class: "insp-factor" }, [
            el("span", { class: "insp-factor-name", text: `${f.dimension} = ${f.value}` }),
            el("span", { class: "insp-factor-metrics sub num" }, [
              el("span", { text: `lift ${f.lift.toFixed(1)}×` }),
              el("span", { text: ` · ${fmtTokens(f.support)} runs` }),
              el("span", { text: ` · ${fmtUsd(f.micros)}` }),
              el("span", { text: ` · ${fmtPct(Math.round(f.missingPct))} missing`, title: "Share of the cohort missing this dimension" }),
            ]),
          ])
        );
      }
      factorsSec.appendChild(list);
    }
    aside.appendChild(factorsSec);

    // --- Volume / size / efficiency decomposition (aggregate_only → always confounding-warned) ---
    try {
      const cmp = await client.compareCohort({ selection, baseline, match: { kind: "aggregate_only" } });
      const d = cmp.data;
      const decomp = el("section", { class: "insp-decomp" }, [
        el("h3", { class: "subhead sub", text: "Why the spend differs" }),
        el("dl", { class: "insp-decomp-dl" }, [
          el("dt", { text: "Volume" }),
          el("dd", { class: "num", text: fmtUsd(d.volume_delta_micros) }),
          el("dt", { text: "Size" }),
          el("dd", { class: "num", text: fmtUsd(d.size_delta_micros) }),
          el("dt", { text: "Efficiency" }),
          el("dd", { class: "num", text: fmtUsd(d.efficiency_delta_micros) }),
          el("dt", { text: "Total" }),
          el("dd", { class: "num", text: fmtUsd(d.total_delta_micros) }),
        ]),
        // aggregate_only comparisons are confounded — say so, honestly.
        el("p", { class: "caption sub insp-confound", text: "Aggregate comparison — volume/size/efficiency split is approximate and can be confounded; match by workload for a like-for-like read." }),
      ]);
      for (const w of d.compatibility_warnings) decomp.appendChild(el("p", { class: "caption sub", text: w }));
      aside.appendChild(decomp);
    } catch {
      /* decomposition unavailable — skip the section rather than block the inspector */
    }
  }

  // --- Representative runs ---
  const repsSec = el("section", { class: "insp-reps" }, [el("h3", { class: "subhead sub", text: "Representative runs" })]);
  if (reps) {
    repsSec.appendChild(
      el("ul", { class: "insp-rep-list" }, [
        repRow("Largest", reps.largest, query),
        repRow("Most anomalous", reps.mostAnomalous, query),
        repRow("Median", reps.median, query),
      ])
    );
  } else {
    repsSec.appendChild(el("p", { class: "caption sub", text: "No runs in this cohort." }));
  }
  aside.appendChild(repsSec);

  // --- Provenance (honest coverage / allocation / fidelity) ---
  aside.appendChild(provenanceBlock(resolved.provenance));
  return aside;
}

/// Compact provenance summary — coverage, allocation method, component fidelity, priced share, and any
/// assumptions. Honest: never claims full fidelity it doesn't have.
function provenanceBlock(p: AnalysisProvenance): HTMLElement {
  const sec = el("section", { class: "insp-prov" }, [
    el("h3", { class: "subhead sub", text: "Provenance" }),
    el("p", { class: "caption sub" }, [
      el("span", { text: `Coverage: ${p.coverage_status}` }),
      el("span", { text: ` · allocation: ${p.allocation_method}` }),
      el("span", { text: ` · fidelity: ${p.component_fidelity}` }),
      ...(p.priced_token_share_pct != null ? [el("span", { text: ` · ${fmtPct(Math.round(p.priced_token_share_pct))} priced` })] : []),
    ]),
  ]);
  for (const a of p.assumptions) sec.appendChild(el("p", { class: "caption sub", text: a }));
  return sec;
}
