// Optimize workspace. The lifecycle queue is built from the additive
// SavingsLedgerV2 plus the persisted savings-action table. Only `applied`/`dismissed` are persisted;
// Verifying / Observed reduction / Not observed are re-derived through the cohort verification API on
// every load. Every action posts the detector-owned exact cohort snapshot — never the looser active
// workspace scope — while route filters keep the shared analysis scope intact.

import { el } from "../ui/el.js";
import { skelRows } from "../ui/skeleton.js";
import { errorNode } from "../ui/errorNode.js";
import { emptyState } from "../ui/empty.js";
import { fmtUsd, humanizeKey, toDollarString } from "../ui/format.js";
import { showToast } from "../ui/toast.js";
import { routeHash, routePath, setRouteQuery, type Route } from "../ui/store.js";
import { tareBeam } from "../ui/tareBeam.js";
import { cohortHash } from "../analysis/serialize.js";
import { investigationFromState } from "../analysis/investigation.js";
import { serializeAnalysisHash } from "../analysis/store.js";
import type { AnalysisState } from "../analysis/state.js";
import type {
  CohortSpec,
  SavingsAction,
  SavingsActionRequest,
  SavingsVerifyResult,
} from "../analysis/types.js";
import type {
  Opportunity,
  OpportunityV2,
  SavingsLedger,
  TareClient,
} from "../client.js";
import type { WorkspaceContext } from "../shell/workbench.js";
import { renderOptimizeScenarios } from "./optimizeScenarios.js";
import { renderOpportunityEvidence } from "./optimizeEvidence.js";
// Shared module (NOT the Investigate workspace) so the two analysis-context strips can't drift back
// into printing raw wire identifiers like `spend_micros`, without dragging one workspace's module into
// the other's graph.
import { metricLabel, normalizationLabel } from "../analysis/labels.js";
import {
  renderSavingsVerification,
  verificationPresentationState,
} from "./optimizeVerification.js";

type DerivedState =
  | "open"
  | "applied"
  | "verifying"
  | "observed_reduction"
  | "not_observed"
  | "dismissed";
type LifecycleView = DerivedState;

interface VerificationState {
  result?: SavingsVerifyResult;
  failed?: true;
}

interface LifecycleRow {
  key: string;
  opportunity?: OpportunityV2;
  legacy?: Opportunity;
  action?: SavingsAction;
  verification?: VerificationState;
  state: DerivedState;
  expectedMicros: number;
}

const VIEW_ORDER: LifecycleView[] = [
  "open",
  "applied",
  "verifying",
  "observed_reduction",
  "not_observed",
  "dismissed",
];

const VIEW_LABEL: Record<LifecycleView, string> = {
  open: "Open",
  applied: "Applied",
  verifying: "Verifying",
  observed_reduction: "Observed reduction",
  not_observed: "Not observed",
  dismissed: "Dismissed",
};

const VIEW_SLUG: Record<LifecycleView, string> = {
  open: "open",
  applied: "applied",
  verifying: "verifying",
  observed_reduction: "observed-reduction",
  not_observed: "not-observed",
  dismissed: "dismissed",
};

const KIND_LABEL: Record<string, string> = {
  loop: "Retry loop",
  failure: "Failed steps",
  cache: "Prompt cache",
  "context-bloat": "Cache erosion",
  rightsizing: "Right-size model",
  "model-swap": "Cheaper model",
};

function activeView(raw: string | undefined): LifecycleView {
  if (raw === "observed-reduction" || raw === "observed_reduction") return "observed_reduction";
  if (raw === "not-observed" || raw === "not_observed") return "not_observed";
  return VIEW_ORDER.includes(raw as LifecycleView) ? (raw as LifecycleView) : "open";
}

function actionKey(opportunityKey: string, hash: string): string {
  return `${opportunityKey}\u001f${hash}`;
}

function derivedState(action: SavingsAction | undefined, verification?: VerificationState): DerivedState {
  if (!action) return "open";
  if (action.status === "dismissed") return "dismissed";
  if (verification?.result) {
    return verificationPresentationState(action, verification.result);
  }
  // The snapshot is safely persisted even when verification itself is temporarily unavailable.
  return "applied";
}

function expectedMicros(opportunity: OpportunityV2 | undefined, action: SavingsAction | undefined): number {
  return (
    action?.expected_point_micros ??
    action?.expected_low_micros ??
    opportunity?.recoverable_micros ??
    0
  );
}

async function loadVerifications(
  client: TareClient,
  actions: SavingsAction[]
): Promise<Map<string, VerificationState>> {
  const rows = await Promise.all(
    actions
      .filter((action) => action.status === "applied")
      .map(async (action): Promise<[string, VerificationState]> => {
        const key = actionKey(action.opportunity_key, action.cohort_hash);
        try {
          const result = await client.verifySavings({
            opportunity_key: action.opportunity_key,
            cohort_hash: action.cohort_hash,
          });
          return [key, { result }];
        } catch {
          return [key, { failed: true }];
        }
      })
  );
  return new Map(rows);
}

function buildRows(
  ledger: SavingsLedger,
  actions: SavingsAction[],
  verifications: Map<string, VerificationState>
): LifecycleRow[] {
  const byIdentity = new Map(
    actions.map((action) => [actionKey(action.opportunity_key, action.cohort_hash), action])
  );
  const consumed = new Set<string>();
  const rows: LifecycleRow[] = [];
  const v2 = ledger.opportunities_v2 ?? [];

  for (const opportunity of v2) {
    const key = actionKey(opportunity.opportunity_key, cohortHash(opportunity.cohort_snapshot));
    const action = byIdentity.get(key);
    const verification = action ? verifications.get(key) : undefined;
    if (action) consumed.add(key);
    rows.push({
      key,
      opportunity,
      action,
      verification,
      state: derivedState(action, verification),
      expectedMicros: expectedMicros(opportunity, action),
    });
  }

  // An action remains traceable even if its detector no longer emits the opportunity in today's
  // ledger. Its stored exact cohort/range is the source of truth; do not silently drop the history.
  for (const action of actions) {
    const key = actionKey(action.opportunity_key, action.cohort_hash);
    if (consumed.has(key)) continue;
    const verification = verifications.get(key);
    rows.push({
      key,
      action,
      verification,
      state: derivedState(action, verification),
      expectedMicros: expectedMicros(undefined, action),
    });
  }

  // One-release compatibility: retain unmatched v1 detector rows as visible Open work. They have no
  // stable key/cohort snapshot, so lifecycle mutation is deliberately unavailable rather than guessed.
  const v2Labels = new Set(v2.map((row) => `${row.kind}\u001f${row.label}`));
  ledger.opportunities.forEach((legacy, index) => {
    if (v2Labels.has(`${legacy.kind}\u001f${legacy.label}`)) return;
    rows.push({
      key: `legacy-${index}-${legacy.kind}-${legacy.label}`,
      legacy,
      state: "open",
      expectedMicros: legacy.recoverable_micros,
    });
  });

  return rows.sort((a, b) => b.expectedMicros - a.expectedMicros || a.key.localeCompare(b.key));
}

function rowsForView(rows: LifecycleRow[], view: LifecycleView): LifecycleRow[] {
  // Applied is an umbrella view over every persisted applied snapshot. The row's pill still states
  // its current derived outcome (Verifying / Observed reduction / Not observed).
  if (view === "applied") return rows.filter((row) => row.action?.status === "applied");
  return rows.filter((row) => row.state === view);
}

function cohortPeriod(cohort: CohortSpec): string {
  if (cohort.from && cohort.to) {
    return cohort.from === cohort.to ? cohort.from : `${cohort.from}–${cohort.to}`;
  }
  if (cohort.from) return `from ${cohort.from}`;
  if (cohort.to) return `through ${cohort.to}`;
  return "all captured dates";
}

function analysisContext(state: AnalysisState | undefined): HTMLElement {
  const scope = state?.scope;
  const selection = state?.selection;
  const baseline = state?.baseline;
  return el(
    "div",
    { class: "optimize-analysis-context", role: "group", "aria-label": "Analysis context" },
    [
      el("span", {
        class: "caption",
        text: scope
          ? `Scope · ${cohortPeriod(scope)} · ${scope.timezone} · ${metricLabel(scope.metric)} / ${normalizationLabel(scope.normalization)}`
          : "Scope · Current savings ledger",
      }),
      el("span", {
        class: "caption",
        text: selection
          ? `Selection A · ${selection.filters.length} filter${selection.filters.length === 1 ? "" : "s"} · ${cohortPeriod(selection)}`
          : "Selection A · Whole scope",
      }),
      el("span", {
        class: "caption",
        text: `Baseline B · ${baseline?.label ?? "Not selected"}`,
      }),
    ]
  );
}

function summaryFigure(label: string, value: number, detail: string, className: string): HTMLElement {
  return el("div", { class: `optimize-summary ${className}` }, [
    el("dt", { text: label }),
    el("dd", {
      class: "num",
      text: fmtUsd(value),
      title: toDollarString(value),
    }),
    el("p", { class: "caption", text: detail }),
  ]);
}

function summaries(
  ledger: SavingsLedger,
  rows: LifecycleRow[]
): { node: HTMLElement; capped: number; applied: number; observed: number } {
  const capped = ledger.capped_potential_micros ?? ledger.total_recoverable_micros;
  const appliedFallback = rows
    .filter((row) => row.action?.status === "applied")
    .reduce((sum, row) => sum + Math.max(0, row.expectedMicros), 0);
  const observedFallback = rows.reduce((sum, row) => {
    const verification = row.verification?.result;
    return row.state === "observed_reduction" && verification
      ? sum + Math.max(0, verification.observed_reduction_micros)
      : sum;
  }, 0);
  // The action table + cohort verifier are authoritative. Older additive-ledger producers shipped
  // these two optional fields as hard-coded zero placeholders even after lifecycle persistence landed;
  // using them would erase real applied/observed state. Keep the UI correct across that compatibility
  // window by deriving the live figures from the exact records already loaded for this queue.
  const applied = appliedFallback;
  const observed = observedFallback;
  const hasAggregateAssociation = rows.some(
    (row) =>
      row.state === "observed_reduction" &&
      row.action?.match.kind === "aggregate_only"
  );
  const node = el("section", { class: "optimize-summary-section", "aria-labelledby": "optimize-summary-h" }, [
    el("h2", { id: "optimize-summary-h", class: "subhead", text: "Savings ledger" }),
    el("dl", { class: "optimize-summaries" }, [
      summaryFigure(
        "Capped potential",
        capped,
        "Estimated current opportunity, bounded by spend; categories may overlap.",
        "potential"
      ),
      summaryFigure(
        "Applied exposure",
        applied,
        "Sum of projected points on applied snapshots; cohorts can overlap and this is not observed.",
        "applied"
      ),
      summaryFigure(
        hasAggregateAssociation ? "Observed reduction / association" : "Observed reduction",
        observed,
        hasAggregateAssociation
          ? "Sum of positive action-local measurements; aggregate-only rows are associations, and cohorts can overlap."
          : "Sum of positive action-local measurements; overlapping cohorts can overlap here too.",
        "observed"
      ),
    ]),
    el("p", {
      class: "caption optimize-summary-rule",
      text: "Do not add or subtract these figures: potential and applied are estimates; observed reduction is a separate measurement.",
    }),
  ]);
  return { node, capped, applied, observed };
}

function laneValue(rows: LifecycleRow[], predicate: (row: LifecycleRow) => boolean): number {
  return rows
    .filter(predicate)
    .reduce((sum, row) => sum + Math.max(0, row.expectedMicros), 0);
}

function lifecycleBeam(rows: LifecycleRow[]): HTMLElement {
  const open = laneValue(rows, (row) => row.state === "open");
  const verifying = laneValue(rows, (row) => row.state === "verifying");
  const applied = laneValue(
    rows,
    (row) => row.action?.status === "applied" && row.state !== "verifying"
  );
  const observed = rows.reduce((sum, row) => {
    const result = row.verification?.result;
    return row.state === "observed_reduction" && result
      ? sum + Math.max(0, result.observed_reduction_micros)
      : sum;
  }, 0);
  const beam = tareBeam({
    mode: "optimize",
    title: "Expected savings by lifecycle state",
    unit: "usd",
    segments: [
      { key: "open", label: "Open", value: open, tone: "cat-1" },
      { key: "applied", label: "Applied", value: applied, tone: "cat-2" },
      { key: "verifying", label: "Verifying", value: verifying, tone: "cat-4" },
    ],
    measured: { label: "Observed reduction / association (separate measured marker)", value: observed },
    onSelect: (key) => setRouteQuery({ view: VIEW_SLUG[key as "open" | "applied" | "verifying"] }),
  });
  beam.appendChild(
    el("p", {
      class: "caption optimize-beam-rule",
      text: "The expected lane is partitioned by mutually exclusive states. Observed reduction is measured separately and is never subtracted from it.",
    })
  );
  return beam;
}

function statePill(
  state: DerivedState,
  verification?: VerificationState,
  action?: SavingsAction
): HTMLElement {
  const glyph = {
    open: "○",
    applied: "◆",
    verifying: "◐",
    observed_reduction: "✓",
    not_observed: "—",
    dismissed: "×",
  }[state];
  const detail =
    state === "applied" && verification?.failed
      ? "Applied · verification unavailable"
      : state === "observed_reduction" && action?.match.kind === "aggregate_only"
        ? "Observed association"
      : VIEW_LABEL[state];
  return el("span", { class: `optimize-state state-${state.replace(/_/g, "-")}` }, [
    el("span", { "aria-hidden": "true", text: glyph }),
    el("span", { text: detail }),
  ]);
}

function baselineDto(state: AnalysisState | undefined): SavingsActionRequest["baseline"] {
  const baseline = state?.baseline;
  if (!baseline) return undefined;
  return {
    kind: baseline.kind,
    label: baseline.label,
    cohort: baseline.cohort,
    ...(baseline.sampleCount === undefined ? {} : { sample_count: baseline.sampleCount }),
  };
}

function actionRequest(opportunity: OpportunityV2, state: AnalysisState | undefined): SavingsActionRequest {
  return {
    opportunity_key: opportunity.opportunity_key,
    cohort: opportunity.cohort_snapshot,
    ...(baselineDto(state) ? { baseline: baselineDto(state) } : {}),
    match: state?.match ?? { kind: "aggregate_only" },
    metric: opportunity.cohort_snapshot.metric,
    normalization: opportunity.cohort_snapshot.normalization,
    ...(opportunity.cohort_snapshot.outcome_denominator
      ? { outcome_denominator: opportunity.cohort_snapshot.outcome_denominator }
      : {}),
    expected_point_micros: opportunity.recoverable_micros,
  };
}

let evidenceSaveSequence = 0;
let evidenceInspectorSequence = 0;

function evidenceLink(
  cohort: CohortSpec,
  label: string,
  client: TareClient,
  context: WorkspaceContext | undefined,
  status: HTMLElement
): Record<string, string | ((event: Event) => void)> {
  if (!context) return { href: routeHash("investigate") };
  const next = {
    ...context.analysis.get(),
    workspace: "investigate" as const,
    selection: cohort,
  };
  const serialized = serializeAnalysisHash(["investigate"], next);
  if (serialized.kind === "inline") {
    return {
      href: serialized.hash,
      onClick: (event: Event) => {
        const pointer = event as MouseEvent;
        if (
          pointer.button !== 0 ||
          pointer.metaKey ||
          pointer.ctrlKey ||
          pointer.shiftKey ||
          pointer.altKey
        ) {
          return;
        }
        event.preventDefault();
        context.analysis.setSelection(cohort);
        context.analysis.navigateWorkspace("investigate");
        window.location.hash = serialized.hash;
      },
    };
  }

  const id = `optimize-${cohortHash(cohort)}-${Date.now().toString(36)}-${(++evidenceSaveSequence).toString(36)}`;
  const saved = serializeAnalysisHash(["investigate"], next, id);
  if (saved.kind !== "inline") return { href: routeHash("investigate") };
  let saving = false;
  return {
    href: routeHash("investigate"),
    title: "This large exact cohort will be saved locally before it opens.",
    onClick: (event: Event) => {
      event.preventDefault();
      if (saving) return;
      saving = true;
      const link = event.currentTarget as HTMLElement | null;
      link?.setAttribute("aria-busy", "true");
      const now = new Date().toISOString();
      void client
        .saveInvestigation(investigationFromState(id, `Optimize evidence · ${label}`, next, now))
        .then(() => {
          context.analysis.setSelection(cohort);
          context.analysis.navigateWorkspace("investigate");
          window.location.hash = saved.hash;
        })
        .catch(() => {
          status.textContent = "Couldn't save this large exact cohort. Nothing opened; retry.";
        })
        .finally(() => {
          saving = false;
          link?.removeAttribute("aria-busy");
        });
    },
  };
}

function copyFix(text: string, status: HTMLElement): void {
  const pending = navigator.clipboard?.writeText?.(text);
  if (!pending) {
    status.textContent = "Clipboard access is unavailable. Select the fix text and copy it manually.";
    return;
  }
  void pending.then(
    () => showToast("Fix copied", "info"),
    () => {
      status.textContent = "Couldn't copy the fix. Select the text and copy it manually.";
    }
  );
}

function actionButton(
  label: string,
  action: string,
  run: () => Promise<void>,
  status: HTMLElement
): HTMLElement {
  return el("button", {
    type: "button",
    class: "btn",
    text: label,
    "data-action": action,
    onClick: (event: Event) => {
      const button = event.currentTarget as HTMLButtonElement;
      button.disabled = true;
      button.setAttribute("aria-busy", "true");
      status.textContent = `${label}…`;
      void run().catch(() => {
        button.disabled = false;
        button.removeAttribute("aria-busy");
        status.textContent = `Couldn't ${label.toLowerCase()}. Nothing changed; retry.`;
      });
    },
  });
}

function rowTitle(row: LifecycleRow): string {
  return row.opportunity?.label ?? row.legacy?.label ?? humanizeKey(row.action?.opportunity_key ?? "Historical opportunity");
}

function rowKind(row: LifecycleRow): string {
  const kind = row.opportunity?.kind ?? row.legacy?.kind;
  return kind ? (KIND_LABEL[kind] ?? humanizeKey(kind)) : "Stored opportunity";
}

function rangeText(row: LifecycleRow): string {
  const action = row.action;
  if (action?.expected_low_micros != null && action.expected_high_micros != null) {
    const point = action.expected_point_micros != null ? ` · point ${fmtUsd(action.expected_point_micros)}` : "";
    return `${fmtUsd(action.expected_low_micros)}–${fmtUsd(action.expected_high_micros)} expected${point}`;
  }
  return `${fmtUsd(row.expectedMicros)} expected point estimate`;
}

function lifecycleRow(
  row: LifecycleRow,
  client: TareClient,
  route: Route,
  context: WorkspaceContext | undefined,
  root: HTMLElement,
  status: HTMLElement
): HTMLElement {
  const opportunity = row.opportunity;
  const legacy = row.legacy;
  const cohort = opportunity?.cohort_snapshot ?? row.action?.cohort;
  const label = rowTitle(row);
  const article = el("li", {
    class: "optimize-lifecycle-row",
    "data-lifecycle-state": row.state,
  });
  article.appendChild(
    el("div", { class: "optimize-row-head" }, [
      el("div", {}, [
        el("p", { class: "eyebrow", text: rowKind(row) }),
        el("h3", { class: "optimize-row-title", text: label }),
      ]),
      statePill(row.state, row.verification, row.action),
    ])
  );
  article.appendChild(
    el("p", {
      class: "optimize-row-estimate num",
      text: rangeText(row),
      title: toDollarString(row.expectedMicros),
    })
  );

  if (opportunity) {
    article.appendChild(
      el("p", {
        class: "caption",
        text: `${opportunity.affected_run_count} affected runs · ${opportunity.affected_step_count} affected steps · ${opportunity.evidence_method} · ${humanizeKey(opportunity.confidence)} confidence · effort ${opportunity.effort}`,
      })
    );
  }
  if (cohort) {
    article.appendChild(
      el("p", {
        class: "caption optimize-row-cohort",
        text: `Exact cohort · ${cohortPeriod(cohort)} · ${cohort.timezone} · ${cohort.filters.length} filter${cohort.filters.length === 1 ? "" : "s"}`,
      })
    );
  }
  if (opportunity?.quality_risk) {
    article.appendChild(el("p", { class: "optimize-row-warning", text: `Quality risk · ${opportunity.quality_risk}` }));
  }
  if (legacy) {
    article.appendChild(
      el("p", {
        class: "optimize-row-warning",
        text: "Lifecycle actions need v2 exact-cohort evidence. This compatibility row remains visible, but Tare will not invent an action identity.",
      })
    );
  }
  if (row.verification?.failed) {
    article.appendChild(
      el("p", {
        class: "optimize-row-warning",
        text: "Verification is temporarily unavailable. The applied snapshot is preserved; retry this view.",
      })
    );
  }
  if (!(row.action?.status === "applied" && row.verification?.result)) {
    for (const warning of row.action?.compatibility_warnings ?? []) {
      article.appendChild(el("p", { class: "optimize-row-warning", text: warning }));
    }
  }
  if (row.action?.status === "applied" && row.verification?.result) {
    article.appendChild(renderSavingsVerification(row.action, row.verification.result));
  }

  const actions = el("div", { class: "optimize-row-actions" });
  // One immutable-by-convention DTO feeds both the visible action snapshot and the eventual write.
  // This prevents the inspector and Apply/Dismiss paths from independently reconstructing scope.
  const normativeRequest = opportunity
    ? actionRequest(opportunity, context?.analysis.get())
    : undefined;
  let evidenceInspector: HTMLElement | undefined;
  if (opportunity) {
    const inspectorId = `optimize-evidence-${++evidenceInspectorSequence}`;
    const inspector = el("section", {
      id: inspectorId,
      class: "optimize-evidence-inspector",
      "aria-label": `Evidence for ${label}`,
      hidden: true,
    });
    let loaded = false;
    const inspectButton = el("button", {
      type: "button",
      class: "btn",
      text: "Inspect evidence",
      "data-action": "inspect-evidence",
      "aria-controls": inspectorId,
      "aria-expanded": "false",
      onClick: (event: Event) => {
        const button = event.currentTarget as HTMLButtonElement;
        const opening = inspector.hasAttribute("hidden");
        if (!opening) {
          inspector.setAttribute("hidden", "");
          button.setAttribute("aria-expanded", "false");
          button.textContent = "Inspect evidence";
          return;
        }
        inspector.removeAttribute("hidden");
        button.setAttribute("aria-expanded", "true");
        button.textContent = "Hide evidence";
        if (loaded) return;
        loaded = true;
        inspector.replaceChildren(
          el("p", {
            class: "caption optimize-evidence-loading",
            role: "status",
            text: "Resolving the exact cohort and its provenance…",
          })
        );
        const exactCohort = opportunity.cohort_snapshot;
        const drillAttributes = evidenceLink(exactCohort, label, client, context, status);
        void client.resolveCohort(exactCohort).then(
          (resolved) => {
            inspector.replaceChildren(
              renderOpportunityEvidence({
                opportunity,
                snapshot: row.action ?? normativeRequest!,
                resolved,
                drillAttributes,
              })
            );
          },
          () => {
            loaded = false;
            inspector.replaceChildren(
              renderOpportunityEvidence({
                opportunity,
                snapshot: row.action ?? normativeRequest!,
                resolutionFailed: true,
                drillAttributes,
              })
            );
          }
        );
      },
    });
    actions.appendChild(inspectButton);
    evidenceInspector = inspector;
  } else if (cohort) {
    actions.appendChild(
      el("a", {
        class: "btn",
        text: "Open stored cohort",
        ...evidenceLink(cohort, label, client, context, status),
      })
    );
  }
  const fix = opportunity?.fix_text ?? legacy?.fix_text;
  if (fix) {
    actions.appendChild(
      el("button", {
        type: "button",
        class: "btn",
        text: "Copy fix",
        "data-action": "copy",
        onClick: () => copyFix(fix, status),
      })
    );
  }
  if (opportunity && !row.action) {
    actions.append(
      actionButton(
        "Mark applied",
        "apply",
        async () => {
          await client.acceptSavings(normativeRequest!);
          showToast("Marked applied against the exact cohort", "info");
          await renderOptimize(root, client, route, context);
        },
        status
      ),
      actionButton(
        "Dismiss",
        "dismiss",
        async () => {
          await client.dismissSavings(normativeRequest!);
          showToast("Dismissed for this exact cohort", "info");
          await renderOptimize(root, client, route, context);
        },
        status
      )
    );
  } else if (row.action) {
    const label = row.action.status === "dismissed" ? "Restore to Open" : "Remove applied state";
    actions.appendChild(
      actionButton(
        label,
        "unaccept",
        async () => {
          await client.unacceptSavings({
            opportunity_key: row.action!.opportunity_key,
            cohort_hash: row.action!.cohort_hash,
          });
          showToast(label === "Restore to Open" ? "Restored to Open" : "Applied state removed", "info");
          await renderOptimize(root, client, route, context);
        },
        status
      )
    );
  }
  article.appendChild(actions);
  if (evidenceInspector) article.appendChild(evidenceInspector);
  return article;
}

function viewTabs(rows: LifecycleRow[], active: LifecycleView, route: Route): HTMLElement {
  const tabs = el("nav", {
    class: "optimize-views",
    "aria-label": "Opportunity lifecycle",
  });
  for (const view of VIEW_ORDER) {
    const count = rowsForView(rows, view).length;
    tabs.appendChild(
      el("a", {
        class: `optimize-view${view === active ? " active" : ""}`,
        href: routePath(route.segments, { ...(route.query ?? {}), view: VIEW_SLUG[view] }),
        ...(view === active ? { "aria-current": "page" } : {}),
        "data-lifecycle-view": VIEW_SLUG[view],
      }, [
        el("span", { text: VIEW_LABEL[view] }),
        el("span", { class: "num optimize-view-count", text: count }),
      ])
    );
  }
  return el("div", { class: "optimize-view-switcher" }, [
    tabs,
    el("p", {
      class: "optimize-view-scroll-hint caption sub",
      text: "Swipe or scroll for more lifecycle states →",
    }),
  ]);
}

function queue(
  rows: LifecycleRow[],
  view: LifecycleView,
  client: TareClient,
  route: Route,
  context: WorkspaceContext | undefined,
  root: HTMLElement,
  status: HTMLElement
): HTMLElement {
  const section = el("section", { class: "optimize-queue", "aria-labelledby": "optimize-queue-h" }, [
    el("div", { class: "optimize-queue-head" }, [
      el("h2", { id: "optimize-queue-h", class: "subhead", text: VIEW_LABEL[view] }),
      el("a", {
        class: "btn",
        href: routePath(route.segments, { ...(route.query ?? {}), view: "scenarios" }),
        text: "Scenarios",
      }),
    ]),
  ]);
  const visible = rowsForView(rows, view);
  if (visible.length === 0) {
    section.appendChild(
      emptyState(
        `No ${VIEW_LABEL[view].toLowerCase()} opportunities`,
        view === "open"
          ? "Nothing is currently waiting for action in this ledger scope."
          : "No opportunity currently has this lifecycle state.",
        view === "open"
          ? undefined
          : { actionLabel: "View Open →", actionHref: routePath(route.segments, { ...(route.query ?? {}), view: "open" }) }
      )
    );
    return section;
  }
  const list = el("ol", { class: "optimize-lifecycle-list" });
  for (const row of visible) {
    list.appendChild(lifecycleRow(row, client, route, context, root, status));
  }
  section.appendChild(list);
  return section;
}

export async function renderOptimize(
  root: HTMLElement,
  client: TareClient,
  route: Route,
  context?: WorkspaceContext
): Promise<void> {
  const query = route.query ?? {};
  // Compatibility redirects remain real content during the promised release window, but converge
  // on the scoped Scenario workbench instead of splitting advice/frontier/experiments into pages.
  if (query.view === "scenarios" || query.type === "cache") {
    return renderOptimizeScenarios(root, client, route, context);
  }

  root.replaceChildren(skelRows());
  let ledger: SavingsLedger;
  let actions: SavingsAction[];
  try {
    [ledger, actions] = await Promise.all([client.savings(), client.savingsActions()]);
  } catch (error) {
    root.replaceChildren(
      errorNode("Couldn't load the Optimize lifecycle. Check that capture is running.", error, {
        actions: [
          { label: "Retry", primary: true, run: () => renderOptimize(root, client, route, context) },
          { label: "Back to Pulse", href: "#/pulse" },
        ],
      })
    );
    return;
  }
  const verifications = await loadVerifications(client, actions);
  const rows = buildRows(ledger, actions, verifications);
  const view = activeView(query.view);
  const state = context?.analysis.get();
  const totals = summaries(ledger, rows);
  const status = el("p", {
    class: "caption optimize-action-status",
    role: "status",
    "aria-live": "polite",
  });

  root.replaceChildren(
    el("section", { class: "optimize-workspace", "aria-label": "Optimize" }, [
      el("header", { class: "optimize-header" }, [
        el("div", {}, [
          el("p", { class: "eyebrow", text: "Verifiable work queue" }),
          // Not an <h1>: the breadcrumb leaf is the page heading (main.ts setBreadcrumb), so a second
      // <h1> here gave every canonical workspace TWO h1s — and on Scenarios they even disagreed
      // ("Optimize" in the crumb, "Scenarios" here). Same class, so the visual treatment is
      // unchanged; only the heading semantics are fixed.
      el("p", { class: "optimize-title", text: "Optimize" }),
          el("p", {
            class: "optimize-lede",
            text: "Act on estimated opportunities, preserve the exact cohort, then measure what changed without calling association causal savings.",
          }),
        ]),
        el("p", {
          class: "caption optimize-pricing",
          text: `${ledger.estimated ? "Estimated pricing" : "Recorded pricing"} · ${ledger.pricing_version}`,
        }),
      ]),
      analysisContext(state),
      viewTabs(rows, view, route),
      status,
      queue(rows, view, client, route, context, root, status),
      el("details", { class: "optimize-analytics" }, [
        el("summary", {}, [
          el("span", { text: "Ledger analytics" }),
          el("span", { class: "num caption", text: `${fmtUsd(totals.capped)} capped potential` }),
        ]),
        el("div", { class: "optimize-analytics-body" }, [totals.node, lifecycleBeam(rows)]),
      ]),
    ])
  );
}
