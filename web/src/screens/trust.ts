// Trust & Pricing utility content. This is the detailed
// honesty surface behind the compact trust strips that remain attached to analytical figures.
// Channel health, capture completeness, priced-token share, component fidelity, reconciliation,
// and receipt verification stay separate so none can over-claim what another proves.

import { el } from "../ui/el.js";
import { icon } from "../ui/icon.js";
import { errorNode } from "../ui/errorNode.js";
import { fmtPct, fmtSignedUsd, fmtTokens, fmtUsd, humanizeKey } from "../ui/format.js";
import { routePath, type Route } from "../ui/store.js";
import { renderPricing } from "./pricing.js";
import { renderReceiptVerifier } from "./receipts.js";
import type { WorkspaceContext } from "../shell/workbench.js";
import type {
  Coverage,
  EstimateConfidence,
  PricingInfo,
  Reconciliation,
  TareClient,
} from "../client.js";
import type { AnalysisProvenance, AnalysisResponse, CohortResolveResult } from "../analysis/types.js";

function trustHref(route: Route, pricing = false): string {
  const query: Record<string, string> = { ...(route.query ?? {}), sheet: "trust" };
  if (pricing) {
    if (query.view && query.view !== "pricing" && !query.workspace_view) {
      query.workspace_view = query.view;
    }
    query.view = "pricing";
  } else if (query.view === "pricing") {
    if (query.workspace_view) query.view = query.workspace_view;
    else delete query.view;
    delete query.workspace_view;
  }
  return routePath(route.segments, query);
}

function trustNav(route: Route, pricing: boolean): HTMLElement {
  const summary = el("a", {
    class: "trust-nav-summary",
    href: trustHref(route),
    text: "Trust overview",
    ...(pricing ? {} : { "aria-current": "page" }),
  });
  const catalog = el("a", {
    class: "trust-nav-pricing",
    href: trustHref(route, true),
    text: "Pricing catalog",
    ...(pricing ? { "aria-current": "page" } : {}),
  });
  return el("nav", { class: "trust-nav", "aria-label": "Trust sections" }, [summary, catalog]);
}

function pricingOverview(info: PricingInfo, confidence: EstimateConfidence): HTMLElement {
  const age = confidence.pricing_age_days === 1 ? "1 day old" : `${confidence.pricing_age_days} days old`;
  return el("section", { class: "section trust-overview", "data-trust-section": "overview" }, [
    el("h2", { text: "Trust overview" }),
    el("p", {
      class: "caption",
      text: "Every dollar in Tare is an estimate from captured usage × effective-dated pricing. It is never a provider invoice.",
    }),
    el("p", { class: "trust-keyline" }, [
      el("strong", { text: `Pricing edition ${info.version}` }),
      el("span", { class: "sub", text: ` · effective ${info.effective_date} · ${age}` }),
    ]),
    info.note ? el("p", { class: "caption sub", text: info.note }) : false,
    el("p", { class: "caption sub", text: "Bundled locally and read without a network request." }),
  ]);
}

function coverageSection(coverage: Coverage): HTMLElement {
  const status =
    coverage.status === "green"
      ? "Capture healthy"
      : coverage.status === "amber"
        ? "Capture degraded"
        : coverage.status === "red"
          ? "Capture down"
          : "No capture configured";
  const tone =
    coverage.status === "green"
      ? "cost-ok"
      : coverage.status === "amber"
        ? "cost-warn"
        : coverage.status === "red"
          ? "cost-high"
          : "muted";
  const section = el("section", { class: "section", "data-trust-section": "capture" }, [
    el("h2", { text: "Capture health" }),
    el("p", { class: tone }, [
      icon(coverage.status === "green" ? "dot" : "warning", {
        size: 13,
        label: coverage.status === "green" ? undefined : "Warning",
      }),
      ` ${status}`,
    ]),
    el("p", {
      class: "caption",
      text: "Channel health reports whether local data is flowing. Channel health does not prove complete capture and never supplies a completeness percentage.",
    }),
  ]);
  if (coverage.sources.length === 0) {
    section.appendChild(el("p", { class: "sub", text: "No local capture source has produced a cost step yet." }));
  } else {
    section.appendChild(
      el("table", { class: "data trust-source-table" }, [
        el("thead", {}, [
          el("tr", {}, [
            el("th", { scope: "col", text: "Source" }),
            el("th", { scope: "col", class: "num", text: "Cost steps" }),
            el("th", { scope: "col", text: "Last captured day" }),
            el("th", { scope: "col", text: "Heartbeat" }),
          ]),
        ]),
        el(
          "tbody",
          {},
          coverage.sources.map((source) =>
            el("tr", {}, [
              el("td", { text: source.source }),
              el("td", { class: "num", text: fmtTokens(source.steps) }),
              el("td", { text: source.last_day || "Unknown" }),
              el("td", { text: source.heartbeat ? "Seen" : "Not observed" }),
            ])
          )
        ),
      ])
    );
  }
  if (coverage.blind_sources.length > 0) {
    section.appendChild(
      el("p", { class: "unpriced" }, [
        icon("warning", { size: 14, label: "Warning" }),
        ` ${coverage.blind_sources.join(", ")}: heartbeat activity was seen without cost steps. That usage is missing from dollar totals; it is not $0.`,
      ])
    );
  }
  return section;
}

function fidelityLine(provenance: AnalysisProvenance): string {
  switch (provenance.component_fidelity) {
    case "component":
      return "Component fidelity — captured prompt-component structure supports within-prompt attribution.";
    case "cost_class":
      return "Cost-class fidelity — input/output/cache classes are known, but within-prompt components are not.";
    default:
      return "Coarse fidelity — spend is allocated from provider-level counts; component claims are unavailable.";
  }
}

function provenanceSection(response: AnalysisResponse<CohortResolveResult>): HTMLElement {
  const { data, provenance } = response;
  const coverage =
    provenance.coverage_status === "unknown"
      ? "Completeness unknown — this scope has no defensible external denominator. No percentage is claimed."
      : provenance.coverage_status === "partial"
        ? "Capture is demonstrably partial — known gaps mean some usage is absent from the estimate."
        : "Capture is full against the stored denominator for this scope; this is separate from channel health.";
  const priced =
    provenance.priced_token_share_pct == null
      ? "Priced-token share unknown — this scope has no nonzero token denominator."
      : provenance.priced_token_share_pct >= 100
        ? "100% of captured tokens in this scope are priced. This does not prove complete capture."
        : `${fmtPct(provenance.priced_token_share_pct)} of captured tokens are priced; the remaining ${fmtPct(100 - provenance.priced_token_share_pct)} is usage-only and excluded from dollar totals.`;
  const section = el("section", { class: "section", "data-trust-section": "provenance" }, [
    el("h2", { text: "Scoped provenance" }),
    el("p", {
      class: "caption",
      text: `Current scope resolves to ${data.run_count} captured ${data.run_count === 1 ? "run" : "runs"} and ${data.step_count} ${data.step_count === 1 ? "step" : "steps"}. Scoped priced estimate: ${fmtUsd(data.total_micros)}.`,
    }),
    el("p", { class: "trust-provenance-line", text: coverage }),
    el("p", { class: "trust-provenance-line", text: priced }),
    el("p", { class: "trust-provenance-line", text: fidelityLine(provenance) }),
    el("p", {
      class: "caption sub",
      text: `Sources: ${provenance.capture_sources.join(", ") || "none recorded"} · allocation: ${provenance.allocation_method} · value class: ${provenance.value_class}`,
    }),
    el("p", {
      class: "caption sub",
      text: `Pricing ${provenance.pricing_edition.version}, effective ${provenance.pricing_edition.effective_date}, mode ${humanizeKey(provenance.pricing_edition.mode)} · refreshed ${provenance.refreshed_at}`,
    }),
  ]);
  for (const assumption of provenance.assumptions) {
    section.appendChild(el("p", { class: "caption sub trust-assumption", text: `Assumption: ${assumption}` }));
  }
  return section;
}

function reconciliationSection(reconciliation: Reconciliation): HTMLElement {
  const section = el("section", { class: "section", "data-trust-section": "reconciliation" }, [
    el("h2", { text: "Reconciliation" }),
    el("p", {
      class: "caption",
      text: "Vendor-reported values are a cross-check, never added to Tare's estimate and never treated as an invoice.",
    }),
  ]);
  if (!reconciliation.has_vendor) {
    section.appendChild(
      el("p", {
        class: "sub",
        text: `Reconciliation unavailable for ${reconciliation.day}: no vendor-reported metric was captured. The cross-check is unknown, not $0.`,
      })
    );
    return section;
  }
  if (reconciliation.rows.length === 0) {
    section.appendChild(
      el("p", { class: "sub", text: "A vendor metric was captured, but there are no comparable priced rows. No reconciliation percentage is claimed." })
    );
    return section;
  }
  const allReconciled = reconciliation.rows.every((row) => row.cause === "ok");
  section.appendChild(
    el("p", {
      class: allReconciled ? "cost-ok" : "cost-warn",
      text: allReconciled
        ? "100% of captured priced tokens reconciled against the available vendor cross-check."
        : "The available cross-check differs or has gaps. Tare's effective-dated estimate remains the primary figure.",
    })
  );
  section.appendChild(
    el("table", { class: "data trust-reconciliation-table" }, [
      el("thead", {}, [
        el("tr", {}, [
          el("th", { scope: "col", text: "Model" }),
          el("th", { scope: "col", class: "num", text: "Tare estimate" }),
          el("th", { scope: "col", class: "num", text: "Vendor cross-check" }),
          el("th", { scope: "col", class: "num", text: "Difference" }),
          el("th", { scope: "col", text: "Status" }),
        ]),
      ]),
      el(
        "tbody",
        {},
        reconciliation.rows.map((row) =>
          el("tr", {}, [
            el("td", { text: row.model }),
            el("td", { class: "num", text: fmtUsd(row.estimate_micros) }),
            el("td", { class: "num", text: fmtUsd(row.vendor_micros) }),
            el("td", { class: "num", text: fmtSignedUsd(row.delta_micros) }),
            el("td", { text: humanizeKey(row.cause) }),
          ])
        )
      ),
    ])
  );
  section.appendChild(
    el("p", {
      class: "caption sub",
      text: `Day ${reconciliation.day} · pricing ${reconciliation.pricing_version} · estimate ${fmtUsd(reconciliation.estimate_total_micros)} · vendor cross-check ${fmtUsd(reconciliation.vendor_total_micros)} · difference ${fmtSignedUsd(reconciliation.delta_total_micros)}.`,
    })
  );
  return section;
}

function fulfilled<T>(result: PromiseSettledResult<T>): T | null {
  return result.status === "fulfilled" ? result.value : null;
}

export async function renderTrust(
  root: HTMLElement,
  client: TareClient,
  route: Route,
  context: WorkspaceContext
): Promise<void> {
  root.replaceChildren(el("p", { class: "skeleton", text: "Loading trust details…" }));
  const pricingView = route.query?.view === "pricing";
  root.setAttribute("data-trust-view", pricingView ? "pricing" : "overview");
  const nav = trustNav(route, pricingView);
  if (pricingView) {
    const catalog = el("div", { class: "trust-pricing-catalog" });
    root.replaceChildren(nav, catalog);
    await renderPricing(catalog, client);
    return;
  }

  const scope = context.analysis.get().scope;
  const [pricingResult, confidenceResult, coverageResult, provenanceResult, reconciliationResult] =
    await Promise.allSettled([
      client.pricing(),
      client.confidence(),
      client.coverage(),
      client.resolveCohort(scope),
      client.reconcile(),
    ] as const);
  const pricing = fulfilled(pricingResult);
  const confidence = fulfilled(confidenceResult);
  const coverage = fulfilled(coverageResult);
  const provenance = fulfilled(provenanceResult);
  const reconciliation = fulfilled(reconciliationResult);

  const children: HTMLElement[] = [nav];
  if (pricing && confidence) children.push(pricingOverview(pricing, confidence));
  else children.push(errorNode("Couldn't load pricing freshness. Dollar estimates cannot be verified right now.", pricingResult.status === "rejected" ? pricingResult.reason : confidenceResult.status === "rejected" ? confidenceResult.reason : undefined));
  if (coverage) children.push(coverageSection(coverage));
  else children.push(errorNode("Couldn't load local capture health. Completeness remains unknown.", coverageResult.status === "rejected" ? coverageResult.reason : undefined));
  if (provenance) children.push(provenanceSection(provenance));
  else children.push(errorNode("Couldn't resolve provenance for the current scope. No completeness claim is available.", provenanceResult.status === "rejected" ? provenanceResult.reason : undefined));
  if (reconciliation) children.push(reconciliationSection(reconciliation));
  else children.push(errorNode("Couldn't load the vendor reconciliation cross-check. Tare's estimate remains separate.", reconciliationResult.status === "rejected" ? reconciliationResult.reason : undefined));

  const receipt = el("div", { class: "trust-receipt" });
  children.push(receipt);
  root.replaceChildren(...children);
  await renderReceiptVerifier(receipt, client, {
    showLens: false,
    showPricing: false,
    initialRunId: route.query?.run,
  });
}
