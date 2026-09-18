// Investigate workspace. Verifies the acceptance: filters match cohort
// semantics, IDs/notes search stays privacy-safe (identifiers only), the result table has keyboard +
// sorting (shared dataTable), and scope is never reset by switching modes.

import { describe, it, expect } from "vitest";
import {
  renderInvestigate,
  loadResults,
  baseCohort,
  filterToken,
} from "../src/workspaces/investigate.js";
import { fakeClient } from "./fakeClient.js";
import { createAnalysisStore, initialAnalysisState } from "../src/analysis/store.js";
import { hydrateWorkspaceAnalysis } from "../src/shell/workbench.js";
import { decodeAnalysisQuery } from "../src/analysis/serialize.js";
import { parseHash, type Route } from "../src/ui/store.js";
import type { CohortSpec } from "../src/analysis/types.js";
import { EMPTY_TREND } from "./fakeClient.js";

const prov = { attribution: "coarse", counts_only: false } as never;

function client(over: Record<string, unknown> = {}) {
  return fakeClient({
    resolveCohort: async (spec) => ({
      data: {
        run_ids: ["r1", "r2"],
        run_count: 2,
        step_count: 5,
        total_micros: 9_000_000,
        entity_rows:
          spec.entity === "step"
            ? [
                { entity: { run_id: "r1", step_ordinal: 0 }, matched_micros: 4_000_000, whole_entity_micros: 4_000_000, matched_step_count: 1 },
                { entity: { run_id: "r2", step_ordinal: 2 }, matched_micros: 1_000_000, whole_entity_micros: 1_000_000, matched_step_count: 1 },
              ]
            : [
                { entity: { run_id: "r1" }, matched_micros: 6_000_000, whole_entity_micros: 6_000_000, matched_step_count: 3 },
                { entity: { run_id: "r2" }, matched_micros: 3_000_000, whole_entity_micros: 3_000_000, matched_step_count: 2 },
              ],
      },
      provenance: prov,
      ...(over.resolve as object),
    }),
    facetCohort: async (req) => ({
      data: {
        dimension: req.dimension,
        rows: [
          { value: "refactor-agent", selection_support: 5, baseline_support: 5, selection_micros: 7_000_000, baseline_micros: 7_000_000, selection_support_share_pct: 60, baseline_support_share_pct: 60, selection_spend_share_pct: 70, baseline_spend_share_pct: 70, selection_missing_pct: 0, baseline_missing_pct: 0, delta_support_share_points: 0 },
          { value: "classify", selection_support: 2, baseline_support: 2, selection_micros: 2_000_000, baseline_micros: 2_000_000, selection_support_share_pct: 20, baseline_support_share_pct: 20, selection_spend_share_pct: 20, baseline_spend_share_pct: 20, selection_missing_pct: 0, baseline_missing_pct: 0, delta_support_share_points: 0 },
        ],
      },
      provenance: prov,
    }),
    trend: async () => ({ ...EMPTY_TREND, days: ["2026-07-10", "2026-07-11"], series: [{ key: "total", per_day: [3_000_000, 5_000_000], total_micros: 8_000_000 }] }),
    searchCohort: async () => ({
      data: { entities: (over.searchEntities as never[]) ?? [{ run_id: "r1" }], truncated: false },
      provenance: prov,
    }),
    ...(over.client as object),
  });
}

const route = (query: Record<string, string> = {}, segments = ["investigate"]): Route => ({
  name: "investigate",
  segments,
  query,
});

async function render(query?: Record<string, string>): Promise<HTMLElement> {
  const root = document.createElement("div");
  await renderInvestigate(root, client(), route(query));
  return root;
}

describe("Investigate — cohort filter semantics", () => {
  it("keeps plural Investigate view modes out of singular CohortSpec entity hydration", async () => {
    const analysis = createAnalysisStore(initialAnalysisState());
    await expect(hydrateWorkspaceAnalysis(analysis, "investigate", route({ entity: "runs" }))).resolves.toBeUndefined();
    expect(analysis.get().scope.entity).toBe("run");
  });

  it("maps query params to CohortFilters with the right semantics", () => {
    const c = baseCohort({ model: "gpt-4o", tag: "prod", minspend: "0.5", from: "2026-07-01", tz: "UTC" });
    expect(c.filters).toContainEqual({ op: "eq", dimension: "model", value: "gpt-4o" });
    expect(c.filters).toContainEqual({ op: "tag", value: "prod" });
    expect(c.filters).toContainEqual({ op: "gte_micros", value: 500_000 }); // $0.5 → micros
    expect(c.from).toBe("2026-07-01"); // scope carried
    expect(c.timezone).toBe("UTC");
  });

  it("filterToken describes each filter honestly", () => {
    expect(filterToken({ op: "eq", dimension: "model", value: "x" }).label).toBe("model = x");
    expect(filterToken({ op: "tag", value: "p" }).label).toBe("tag: p");
    expect(filterToken({ op: "gte_micros", value: 500_000 }).label).toMatch(/spend ≥ \$0\.50/);
  });

  it("renders removable filter tokens for the active cohort filters", async () => {
    const root = await render({ model: "gpt-4o", tag: "prod" });
    const chips = root.querySelectorAll(".inv-filter");
    expect(chips.length).toBe(2);
    expect(root.textContent).toContain("model = gpt-4o");
    expect(root.querySelector(".inv-filter-x")).toBeTruthy(); // removable (scope-preserving)
  });
});

describe("Investigate — ranked results per mode (canonical APIs)", () => {
  it("Runs mode ranks runs by matched spend (resolveCohort)", async () => {
    const rows = await loadResults(client(), "runs", baseCohort({}), {});
    expect(rows.map((r) => r.key)).toEqual(["r1", "r2"]); // r1 $6 > r2 $3
    expect(rows[0].dollars).toBe(6_000_000);
    expect(rows[0].href).toMatch(/#\/investigate\/run\/r1/); // canonical multi-segment run route
  });

  it("Steps mode ranks steps (resolveCohort step grain)", async () => {
    const rows = await loadResults(client(), "steps", baseCohort({}), {});
    expect(rows[0].key).toBe("r1#0");
    expect(rows[0].label).toMatch(/step 0/);
  });

  it("Sessions mode ranks groups with support counts (facetCohort)", async () => {
    const rows = await loadResults(client(), "sessions", baseCohort({}), {});
    expect(rows[0].label).toBe("refactor-agent");
    expect(rows[0].support).toBe(5);
    expect(rows[0].href).toMatch(/entity=runs/); // drilling a group narrows to Runs, scope preserved
    expect(rows[0].href).toMatch(/session=refactor-agent/);
  });

  it("Time mode ranks days by total spend (trend daily totals)", async () => {
    const rows = await loadResults(client(), "time", baseCohort({}), {});
    expect(rows[0].label).toBe("2026-07-11"); // $5 > $3
    expect(rows[0].dollars).toBe(5_000_000);
  });
});

describe("Investigate — modes, search, table", () => {
  it("keeps scope, Selection A, and Baseline B visibly named in the analysis context", async () => {
    const root = document.createElement("div");
    const analysis = createAnalysisStore(initialAnalysisState("investigate"));
    const scope = baseCohort({ from: "2026-07-01", to: "2026-07-14", tz: "America/Los_Angeles" });
    analysis.setScope(scope);
    analysis.setSelection({ ...scope, from: "2026-07-10", to: "2026-07-10", filters: [{ op: "eq", dimension: "model", value: "gpt-4o" }] });
    analysis.setBaseline({
      kind: "explicit_cohort",
      label: "Explicit cohort · 12 runs",
      cohort: { ...scope, from: "2026-06-17", to: "2026-06-30" },
      sampleCount: 12,
    });
    await renderInvestigate(root, client(), route(), { analysis });
    const context = root.querySelector(".inv-analysis-context")!;
    expect(context.getAttribute("aria-label")).toBe("Analysis context");
    expect(context.querySelector(".inv-scope")?.textContent).toContain("2026-07-01–2026-07-14");
    expect(context.querySelector(".inv-selection")?.textContent).toMatch(/Selection A.*1 filter.*2026-07-10/);
    expect(context.querySelector(".inv-baseline")?.textContent).toContain("Baseline B · Explicit cohort · 12 runs");
  });

  it("exposes semantic entity, canvas, inspector, resize, and narrow-stack controls", async () => {
    const root = await render();
    expect(root.querySelector("[data-adaptive-panes]")).toBeTruthy();
    expect(Array.from(root.querySelectorAll("[data-pane]")).map((p) => p.getAttribute("data-pane"))).toEqual([
      "entities",
      "canvas",
      "inspector",
    ]);
    expect(root.querySelectorAll('[role="separator"]')).toHaveLength(2);
    expect(root.querySelector('[data-pane-back]')?.textContent).toMatch(/Back/);
    expect(root.querySelector('[data-pane-title]')).toBeTruthy();
    expect(root.querySelector('[data-pane-forward]')).toBeTruthy();
  });

  it("offers all five entity modes; switching a mode preserves the scope (never resets it)", async () => {
    const root = await render({ from: "2026-07-01", to: "2026-07-31", tz: "UTC" });
    const modes = Array.from(root.querySelectorAll<HTMLAnchorElement>(".inv-mode"));
    expect(modes.map((m) => m.textContent)).toEqual(["Runs", "Sessions", "Templates", "Steps", "Time"]);
    expect(modes.map((m) => m.tabIndex)).toEqual([0, -1, -1, -1, -1]);
    expect(modes.every((m) => m.getAttribute("aria-controls") === "investigate-canvas")).toBe(true);
    // Every mode link carries the scope query unchanged — only entity changes.
    for (const m of modes) {
      const href = m.getAttribute("href")!;
      expect(href).toContain("from=2026-07-01");
      expect(href).toContain("to=2026-07-31");
      expect(href).toMatch(/entity=/);
    }

    // Manual activation lets arrows inspect the choices without triggering a potentially slow load.
    modes[0].dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    expect(modes.map((m) => m.tabIndex)).toEqual([-1, 0, -1, -1, -1]);
    expect(modes[1].getAttribute("aria-selected")).toBe("false");
    modes[1].dispatchEvent(new KeyboardEvent("keydown", { key: "End", bubbles: true }));
    expect(modes[4].tabIndex).toBe(0);
  });

  it("uses the persistent left pane for result context and scope-preserving model/provider facets", async () => {
    const root = await render({ from: "2026-07-01", to: "2026-07-31", tz: "UTC" });
    await new Promise((resolve) => setTimeout(resolve, 0)); // supplementary facet requests settle
    const facts = root.querySelector(".inv-result-facts")!;
    expect(facts.textContent).toContain("2 runs");
    expect(facts.textContent).toContain("$9.00");
    expect(facts.textContent).toContain("67%"); // largest run is $6 of the listed $9

    const quick = root.querySelector(".inv-quick-facets")!;
    expect(quick.textContent).toMatch(/Build Selection A/);
    expect(quick.textContent).toMatch(/Models/);
    expect(quick.textContent).toMatch(/Providers/);
    const links = Array.from(quick.querySelectorAll<HTMLAnchorElement>(".inv-facet-row"));
    expect(links.length).toBeGreaterThanOrEqual(2);
    expect(links[0].textContent).toMatch(/refactor-agent.*\$7\.00.*5 runs/);
    expect(links[0].href).toContain("from=2026-07-01");
    expect(links[0].href).toContain("to=2026-07-31");
    expect(links.some((link) => link.getAttribute("href")?.includes("model=refactor-agent"))).toBe(true);
    expect(links.some((link) => link.getAttribute("href")?.includes("provider=refactor-agent"))).toBe(true);
  });

  it("turns a model facet into the actual reloadable Selection A and Clear removes it", async () => {
    const analysis = createAnalysisStore(initialAnalysisState("investigate"));
    const resolved: CohortSpec[] = [];
    const c = client();
    const resolve = c.resolveCohort;
    c.resolveCohort = async (spec) => {
      resolved.push(structuredClone(spec));
      return resolve(spec);
    };
    const initialHash = window.location.hash;
    try {
      const root = document.createElement("div");
      const initialRoute = route({ from: "2026-07-01", to: "2026-07-31", tz: "UTC" });
      await hydrateWorkspaceAnalysis(analysis, "investigate", initialRoute);
      await renderInvestigate(root, c, initialRoute, { analysis });
      await new Promise((resolveFacets) => setTimeout(resolveFacets, 0));
      const model = root.querySelector<HTMLAnchorElement>(
        '.inv-quick-facet[aria-label="Filter by model"] .inv-facet-row'
      )!;
      const destination = parseHash(model.getAttribute("href")!);
      expect(decodeAnalysisQuery(destination.query ?? {}).selection?.filters).toContainEqual({
        op: "eq",
        dimension: "model",
        value: "refactor-agent",
      });

      model.dispatchEvent(new MouseEvent("click", { bubbles: true, button: 0 }));
      expect(analysis.get().selection?.filters).toContainEqual({
        op: "eq",
        dimension: "model",
        value: "refactor-agent",
      });
      expect(window.location.hash).toContain("sel=");

      const filtered = document.createElement("div");
      await hydrateWorkspaceAnalysis(analysis, "investigate", parseHash(window.location.hash));
      await renderInvestigate(filtered, c, parseHash(window.location.hash), { analysis });
      await new Promise((resolveFacets) => setTimeout(resolveFacets, 0));
      expect(resolved[resolved.length - 1]?.filters).toContainEqual({
        op: "eq",
        dimension: "model",
        value: "refactor-agent",
      });
      expect(filtered.querySelector(".inv-selection")?.textContent).toMatch(/Selection A.*1 filter/);
      expect(filtered.querySelector(".inv-quick-facets")?.textContent).toMatch(/Refine Selection A/);

      const clear = filtered.querySelector<HTMLAnchorElement>(
        '.inv-quick-facet[aria-label="Filter by model"] .inv-facet-clear'
      )!;
      clear.dispatchEvent(new MouseEvent("click", { bubbles: true, button: 0 }));
      expect(analysis.get().selection).toBeNull();
      expect(decodeAnalysisQuery(parseHash(window.location.hash).query ?? {}).selection).toBeNull();
    } finally {
      window.history.replaceState(null, "", initialHash || "#/pulse");
    }
  });

  it("search is privacy-safe: identifiers only, never payload/prompt text", async () => {
    const root = await render();
    const input = root.querySelector<HTMLInputElement>(".inv-search")!;
    expect(input.getAttribute("aria-label")).toMatch(/never payload/i);
    expect(input.placeholder).toBe("Search IDs, labels, tags, notes…");
    const privacy = root.querySelector(`#${input.getAttribute("aria-describedby")}`);
    expect(privacy?.textContent).toMatch(/prompt and response text are never searched/i);
    // Server search is invoked with the identifying fields only (id/label/hash/tag/note/model/config).
    let fields: string[] = [];
    const c = fakeClient({
      resolveCohort: (client() as never)["resolveCohort"],
      searchCohort: async (req) => {
        fields = req.fields ?? [];
        return { data: { entities: [{ run_id: "r1" }], truncated: false }, provenance: prov };
      },
    });
    const r2 = document.createElement("div");
    document.body.appendChild(r2);
    await renderInvestigate(r2, c, route());
    const si = r2.querySelector<HTMLInputElement>(".inv-search")!;
    si.value = "r1";
    si.dispatchEvent(new Event("input"));
    await new Promise((res) => setTimeout(res, 260)); // debounce
    expect(fields).toEqual(["id", "label", "hash", "tag", "note", "model", "config"]);
    expect(fields).not.toContain("payload");
    r2.remove();
  });

  it("result table has sortable headers + the keyboard core loop (shared dataTable)", async () => {
    const root = await render();
    const table = root.querySelector(".inv-results table")!;
    expect(table).toBeTruthy();
    expect(table.querySelector("th.sortable")).toBeTruthy(); // sortable columns
    // dataTable's keyboard loop stamps data-nav-id on rows when onActivate is provided.
    expect(root.querySelector(".inv-results [data-nav-id]")).toBeTruthy();
    // Ranked bars by default (data in ink, not the brand accent).
    expect(root.querySelector(".inv-bar-fill")).toBeTruthy();
  });
});
