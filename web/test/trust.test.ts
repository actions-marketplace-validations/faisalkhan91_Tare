// Trust/Pricing utility sheet. The sheet keeps channel health,
// analytical completeness, priced-token share, fidelity, reconciliation, and receipts distinct.

import { describe, it, expect } from "vitest";
import { renderTrust } from "../src/screens/trust.js";
import { createAnalysisStore, initialAnalysisState } from "../src/analysis/store.js";
import { parseHash } from "../src/ui/store.js";
import { fakeClient, fakeProvenance } from "./fakeClient.js";
import type { TareClient } from "../src/client.js";
import type { WorkspaceContext } from "../src/shell/workbench.js";

function context(): WorkspaceContext {
  return { analysis: createAnalysisStore(initialAnalysisState()) };
}

function populated(overrides: Partial<TareClient> = {}): TareClient {
  return fakeClient({
    pricing: async () => ({
      version: "2026.07.10",
      effective_date: "2026-07-10",
      note: "Bundled fixture edition.",
      models: [
        {
          provider: "anthropic",
          model_id: "claude-sonnet-4-6",
          tier: "standard",
          input_micro_per_mtok: 3_000_000,
          output_micro_per_mtok: 15_000_000,
          cache_read_micro_per_mtok: 300_000,
          cache_write_5m_micro_per_mtok: 3_750_000,
          cache_write_1h_micro_per_mtok: 6_000_000,
        },
      ],
    }),
    confidence: async () => ({
      pricing_age_days: 6,
      unpriced_token_share_pct: 18,
      coverage_status: "unknown",
      label: "medium",
      estimated: true,
    }),
    coverage: async () => ({
      status: "amber",
      has_proxy: true,
      has_otel: false,
      blind_sources: ["codex"],
      sources: [
        { source: "proxy", steps: 12, last_day: "2026-07-15", heartbeat: false },
        { source: "codex", steps: 0, last_day: "2026-07-15", heartbeat: true },
      ],
    }),
    resolveCohort: async (scope) => ({
      data: { run_ids: ["run-a"], run_count: 1, step_count: 12, total_micros: 4_200_000, entity_rows: [] },
      provenance: {
        ...fakeProvenance(scope),
        capture_sources: ["proxy"],
        coverage_status: "unknown",
        priced_token_share_pct: 82,
        component_fidelity: "cost_class",
        pricing_edition: { version: "2026.07.10", effective_date: "2026-07-10", mode: "effective" },
        allocation_method: "derived",
        value_class: "derived",
        assumptions: ["No external capture denominator is available."],
      },
    }),
    reconcile: async () => ({
      day: "2026-07-15",
      pricing_version: "2026.07.10",
      rows: [
        {
          model: "claude-sonnet-4-6",
          estimate_micros: 4_200_000,
          vendor_micros: 4_000_000,
          delta_micros: 200_000,
          tokens: 20_000,
          cause: "mismatch",
        },
      ],
      estimate_total_micros: 4_200_000,
      vendor_total_micros: 4_000_000,
      delta_total_micros: 200_000,
      has_vendor: true,
    }),
    listRuns: async () => ["run-a"],
    receipt: async () => ({
      receipt: {},
      verify: {
        scope: "run:run-a",
        pricing_version: "2026.07.10",
        recomputed_total_micros: 4_200_000,
        rows: 12,
        flamegraph_checked: true,
        digest: 42,
      },
    }),
    ...overrides,
  });
}

describe("Trust utility content", () => {
  it("separates capture health, completeness, pricing share, fidelity, and reconciliation", async () => {
    const root = document.createElement("div");
    await renderTrust(root, populated(), parseHash("#/pulse?sheet=trust"), context());

    expect(Array.from(root.querySelectorAll("h2")).map((h) => h.textContent)).toEqual([
      "Trust overview",
      "Capture health",
      "Scoped provenance",
      "Reconciliation",
      "Cost receipt",
    ]);
    expect(root.textContent).toContain("Pricing edition 2026.07.10");
    expect(root.textContent).toContain("6 days old");
    expect(root.textContent).toContain("Capture degraded");
    expect(root.textContent).toContain("Channel health does not prove complete capture");
    expect(root.textContent).toContain("Completeness unknown");
    expect(root.textContent).toContain("No percentage is claimed");
    expect(root.textContent).toContain("82% of captured tokens are priced");
    expect(root.textContent).toContain("remaining 18% is usage-only");
    expect(root.textContent).toContain("Cost-class fidelity");
    expect(root.textContent).toContain("derived");
    expect(root.textContent).toContain("Vendor-reported values are a cross-check, never added to Tare's estimate");
    expect(root.textContent).toContain("Mismatch");
    expect(root.textContent).not.toMatch(/100% (capture|attributed)/i);
  });

  it("attests and verifies a selected run offline inside the Trust sheet", async () => {
    const root = document.createElement("div");
    await renderTrust(root, populated(), parseHash("#/pulse?run=run-a&sheet=trust"), context());
    const run = root.querySelector(".trust-receipt select") as HTMLSelectElement;
    expect(run.value).toBe("run-a");
    (root.querySelector(".trust-receipt button") as HTMLButtonElement).click();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(root.querySelector(".trust-receipt .receipt-ledger")?.textContent).toContain("$4.20");
    expect(root.querySelector(".trust-receipt .receipt-ledger")?.textContent).toContain("matched");
    expect(root.textContent).toContain("estimate, not an invoice");
  });

  it("states unavailable reconciliation as unknown rather than a fabricated zero", async () => {
    const root = document.createElement("div");
    await renderTrust(
      root,
      populated({
        reconcile: async () => ({
          day: "2026-07-15",
          pricing_version: "2026.07.10",
          rows: [],
          estimate_total_micros: 0,
          vendor_total_micros: 0,
          delta_total_micros: 0,
          has_vendor: false,
        }),
      }),
      parseHash("#/pulse?sheet=trust"),
      context()
    );
    const reconciliation = root.querySelector("[data-trust-section='reconciliation']")!;
    expect(reconciliation.textContent).toContain("Reconciliation unavailable");
    expect(reconciliation.textContent).toContain("unknown, not $0");
    expect(reconciliation.textContent).not.toContain("$0.00");
  });

  it("deep-links the full pricing catalog and keeps unpriced usage outside dollar totals", async () => {
    const root = document.createElement("div");
    await renderTrust(
      root,
      populated({
        report: async () => ({
          pricing_version: "2026.07.10",
          effective_date: "2026-07-10",
          estimated: true,
          total_micros: 0,
          rows: [],
          unpriced: [{ provider: "local", model: "future-model", token_total: 9000, step_count: 1 }],
        }),
      }),
      parseHash("#/investigate?entity=runs&sheet=trust&view=pricing"),
      context()
    );
    expect(root.getAttribute("data-trust-view")).toBe("pricing");
    expect(root.textContent).toContain("Model pricing");
    expect(root.textContent).toContain("claude-sonnet-4-6");
    expect(root.textContent).toContain("local/future-model");
    expect(root.textContent).toContain("not included in totals");
    expect(root.querySelector<HTMLAnchorElement>(".trust-nav-summary")?.getAttribute("href")).toContain("sheet=trust");
  });
});
