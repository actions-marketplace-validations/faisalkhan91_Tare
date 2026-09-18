// Pulse workspace top hierarchy. Verifies the acceptance: loads from the
// canonical burn-rate + coverage APIs, all figures share one scope, exactly one spend answer,
// forecast/baseline are labelled, and the trust strip keeps honest coverage language.

import { describe, it, expect, vi } from "vitest";
import { renderPulse, buildAttentionRows, pulseBeamModel } from "../src/workspaces/pulse.js";
import { tareBeam } from "../src/ui/tareBeam.js";
import type { SavingsLedger } from "../src/client.js";
import { renderInvestigate } from "../src/workspaces/investigate.js";
import { createAnalysisStore, initialAnalysisState } from "../src/analysis/store.js";
import { decodeAnalysisQuery } from "../src/analysis/serialize.js";
import { hydrateWorkspaceAnalysis, renderWorkspace } from "../src/shell/workbench.js";
import { fakeClient } from "./fakeClient.js";
import { parseHash, type Route } from "../src/ui/store.js";
import type { CohortSpec } from "../src/analysis/types.js";
import type { SavedInvestigationV2 } from "../src/analysis/investigation.js";

const ROUTE: Route = { name: "pulse", segments: ["pulse"] };

const BURN = {
  run_rate_micros_per_day: 2_000_000,
  effective_rate_micros_per_day: 2_000_000,
  spent_micros: 20_000_000,
  active_days: 10,
  daily_spend_micros: [
    1_000_000,
    1_500_000,
    1_600_000,
    1_800_000,
    1_900_000,
    2_000_000,
    2_100_000,
    2_400_000,
    2_600_000,
    3_100_000,
  ],
  days_elapsed: 10,
  days_in_period: 30,
  projected_micros: 60_000_000,
  projected_low_micros: 52_000_000,
  projected_high_micros: 68_000_000,
  cap_micros: 80_000_000,
  on_track: true,
  headroom_days: 12,
  period: "month" as const,
  period_start: "2026-07-01",
  as_of: "2026-07-10",
  period_end: "2026-07-30",
};

function client(over: Record<string, unknown> = {}) {
  return fakeClient({
    burnrate: async () => ({ ...BURN, ...(over.burn as object) }),
    coverage: async () => ({
      status: "green",
      has_proxy: true,
      has_otel: false,
      blind_sources: [],
      sources: [],
      ...((over.cov ?? over.coverage) as object),
    }),
    ...(over.anomalies ? { anomalies: async () => over.anomalies as never } : {}),
    ...(over.budget ? { budget: async () => over.budget as never } : {}),
    ...(over.failures ? { failures: async () => over.failures as never } : {}),
    ...(over.loops ? { loops: async () => over.loops as never } : {}),
    ...(over.savings ? { savings: async () => over.savings as never } : {}),
    ...((over.client ?? {}) as object),
  });
}

async function render(over?: Record<string, unknown>): Promise<HTMLElement> {
  const root = document.createElement("div");
  await renderPulse(root, client(over), ROUTE);
  return root;
}

describe("Pulse workspace — scope, one answer, forecast, trust", () => {
  it("scopes every figure to one period named in the scope bar", async () => {
    const root = await render();
    expect(root.querySelector(".pulse-scopebar")?.textContent).toContain("Pulse");
    expect(root.querySelector(".pulse-scope")?.textContent).toContain("Month to date · projected through Jul 30");
    const ranges = root.querySelectorAll(".pulse-range-option");
    expect(Array.from(ranges).map((range) => range.textContent)).toEqual(["Day", "Week", "MTD", "YTD", "Custom"]);
    expect(root.querySelector('.pulse-range-option[aria-current="true"]')?.textContent).toBe("MTD");
    // The answer is scoped to the same period — figures share scope.
    expect(root.querySelector(".pulse-answer-label")?.textContent).toContain("this month");
  });

  it("loads the URL-backed preset and exposes durable links for every range", async () => {
    const burnrate = vi.fn(async () => ({
      ...BURN,
      period: "year" as const,
      period_start: "2026-01-01",
      period_end: "2026-12-31",
      days_in_period: 365,
    }));
    const root = document.createElement("div");
    const route = parseHash("#/pulse?range=year");
    await renderPulse(root, client({ client: { burnrate } }), route);

    expect(burnrate).toHaveBeenCalledWith("year");
    expect(root.querySelector('.pulse-range-option[aria-current="true"]')?.textContent).toBe("YTD");
    expect(root.querySelector<HTMLAnchorElement>('.pulse-range-option[aria-label="Show today spend"]')?.getAttribute("href")).toBe("#/pulse?range=day");
    expect(root.querySelector<HTMLAnchorElement>(".pulse-range-custom")?.getAttribute("href")).toBe("#/investigate?mode=timeline");
  });

  it("includes the Now feed (active sessions + recent steps) with honest liveness", async () => {
    const root = await render();
    const now = root.querySelector(".pulse-now")!;
    expect(now).toBeTruthy();
    expect(now.querySelector(".pulse-now-live")?.textContent).toMatch(/recency-derived/);
  });

  it("shows exactly ONE spend answer (the projected period figure), no duplicate", async () => {
    const root = await render();
    const heroes = root.querySelectorAll(".stat.display");
    expect(heroes.length).toBe(1);
    expect(heroes[0].textContent).toBe("$60"); // rounded display; exact value remains in title/table
    expect(heroes[0].getAttribute("title")).toContain("$60.00");
  });

  it("classifies the whole pace range against the cap instead of only the point projection", async () => {
    const onTrack = await render();
    expect(onTrack.querySelector(".pulse-delta")?.textContent).toMatch(/Recent-pace scenarios stay \$12 under the \$80 cap/);
    const crossing = await render({ burn: { projected_low_micros: 72_000_000, projected_micros: 79_000_000, projected_high_micros: 88_000_000 } });
    expect(crossing.querySelector(".pulse-delta")?.className).toContain("cost-warn");
    expect(crossing.querySelector(".pulse-delta")?.textContent).toMatch(/At risk.*cross the \$80 cap/);
    // Entire range over cap → high-risk tone + minimum overage.
    const over = await render({ burn: { on_track: false, projected_micros: 90_000_000, projected_low_micros: 88_000_000, projected_high_micros: 96_000_000 } });
    const delta = over.querySelector(".pulse-delta")!;
    expect(delta.className).toContain("cost-high");
    expect(delta.textContent).toMatch(/exceed the \$80 cap by at least \$8/);
    expect(delta.querySelector("svg")).toBeTruthy(); // labelled warning icon
  });

  it("labels deterministic pace scenarios explicitly and avoids false precision", async () => {
    const root = await render();
    const label = root.querySelector(".pulse-band-label")!;
    expect(label.textContent).toMatch(/Recent-pace scenarios \$52–\$68 · 20 days remaining/);
    expect(label.getAttribute("title")).toMatch(/not a confidence interval/i);
  });

  it("renders real daily cumulative history, directly labelled pace scenarios, dates, and accessible data", async () => {
    const root = await render();
    const charts = root.querySelectorAll(".pulse-forecast-line svg");
    expect(charts).toHaveLength(4); // independently authored ultrawide, wide, medium, and mobile geometries
    expect(charts[0].classList.contains("pulse-chart-ultrawide")).toBe(true);
    expect(charts[1].classList.contains("pulse-chart-wide")).toBe(true);
    expect(charts[2].classList.contains("pulse-chart-medium")).toBe(true);
    expect(charts[3].classList.contains("pulse-chart-compact")).toBe(true);
    expect(charts[0].getAttribute("viewBox")).toBe("0 0 1440 300");
    expect(charts[3].getAttribute("viewBox")).toBe("0 0 360 270");
    const svg = charts[1];
    expect(svg).toBeTruthy();
    expect(svg.getAttribute("aria-label")).toMatch(/captured-spend projection/i);
    expect(svg.querySelector("title")).toBeNull(); // no whole-SVG native hover box
    expect(svg.querySelector("desc")?.textContent).toMatch(/Captured spend from Jul 1 through Jul 10 is \$20\.00/i);
    expect(svg.querySelector("desc")?.textContent).toMatch(/not a probability interval/i);
    const observed = svg.querySelector(".pulse-observed")!;
    expect(observed.tagName.toLowerCase()).toBe("path");
    expect((observed.getAttribute("d")?.match(/\bV\b/g) ?? [])).toHaveLength(10); // one actual step per day
    expect(svg.querySelector(".pulse-forecast")).toBeTruthy();
    expect(svg.querySelector(".pulse-band")?.tagName.toLowerCase()).toBe("polygon");
    expect(svg.querySelectorAll(".pulse-band-edge")).toHaveLength(2); // visible boundaries, not fill alone
    expect(svg.querySelector(".pulse-today-label")?.textContent).toBe("Today · Jul 10");
    expect(svg.querySelector(".pulse-current-value")?.textContent).toBe("Captured $20");
    expect(svg.querySelector(".pulse-label-typical")?.textContent).toBe("Typical pace · $60");
    expect(svg.querySelector(".pulse-label-high")?.textContent).toBe("High pace · $68");
    expect(svg.querySelector(".pulse-label-low")?.textContent).toBe("Low pace · $52");
    expect(svg.querySelectorAll(".pulse-gridline")).toHaveLength(5);
    expect(Array.from(svg.querySelectorAll(".pulse-y-tick")).map((n) => n.textContent)).toContain("$0");
    expect(svg.textContent).toContain("Jul 1");
    expect(svg.textContent).toContain("Jul 30");
    expect(svg.textContent).not.toMatch(/Observed.*Forecast.*Estimated range/); // direct labels replace the legend
    expect(svg.outerHTML).toContain("var(--data-ink)");
    expect(svg.outerHTML).not.toContain("var(--accent)"); // data is never the brand accent
    const toggle = root.querySelector<HTMLButtonElement>(".pulse-chart-data-toggle")!;
    const details = root.querySelector<HTMLElement>(".pulse-chart-data-panel")!;
    expect(toggle.textContent).toMatch(/data & methodology/i);
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(details.hidden).toBe(true);
    toggle.click();
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(details.hidden).toBe(false);
    expect(details.textContent).toMatch(/not a confidence interval/i);
    expect(details.querySelectorAll("tbody tr")).toHaveLength(14); // 10 days + low/typical/high + cap
    expect(details.querySelector("tbody tr:last-child td:last-child")?.textContent).toBe("$80.00");
  });

  it("inspects exact captured and projected values with the keyboard", async () => {
    const root = await render();
    const svg = root.querySelector<SVGSVGElement>(".pulse-chart-wide")!;
    const tooltip = root.querySelector<HTMLElement>(".pulse-chart-tooltip")!;

    svg.dispatchEvent(new FocusEvent("focus"));
    expect(tooltip.hidden).toBe(false);
    expect(tooltip.textContent).toMatch(/Jul 10, 2026.*Captured cumulative.*\$20\.00/s);
    expect(svg.querySelector(".pulse-inspector")?.classList.contains("is-active")).toBe(true);

    svg.dispatchEvent(new KeyboardEvent("keydown", { key: "End", bubbles: true }));
    expect(tooltip.textContent).toMatch(/Jul 30, 2026.*Projected.*Typical pace.*\$60\.00/s);
    expect(tooltip.textContent).toMatch(/Low pace.*\$52\.00.*High pace.*\$68\.00.*Cap pace.*\$80\.00/s);

    svg.dispatchEvent(new KeyboardEvent("keydown", { key: "Home", bubbles: true }));
    expect(tooltip.textContent).toMatch(/Jul 1, 2026.*Captured cumulative.*\$1\.00.*Spend that day.*\$1\.00/s);

    svg.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(tooltip.hidden).toBe(true);
  });

  it("treats Day as captured spend, without a fabricated forecast or cap setup", async () => {
    const root = await render({ burn: {
      period: "day",
      period_start: "2026-07-10",
      as_of: "2026-07-10",
      period_end: "2026-07-10",
      days_in_period: 1,
      days_elapsed: 1,
      active_days: 1,
      daily_spend_micros: [3_100_000],
      spent_micros: 3_100_000,
      projected_micros: 3_100_000,
      projected_low_micros: 3_100_000,
      projected_high_micros: 3_100_000,
      cap_micros: 0,
    } });
    expect(root.querySelector(".pulse-answer-label")?.textContent).toBe("Captured spend today");
    expect(root.querySelector(".stat.display")?.textContent).toBe("$3");
    expect(root.querySelector(".pulse-forecast-line .pulse-forecast")).toBeNull();
    expect(root.querySelector(".pulse-label-typical")).toBeNull();
    expect(root.querySelector(".pulse-support")).toBeNull();
    expect(root.textContent).not.toMatch(/More active days|Set cap/);
  });

  it("trust strip states honest capture health + channels", async () => {
    const green = await render();
    const trust = green.querySelector(".pulse-trust")!;
    expect(trust.textContent).toMatch(/Capture healthy.*Coverage/);
    expect(trust.getAttribute("title")).toContain("channels: proxy");
    expect(trust.closest(".pulse-chart-footer")).toBeTruthy();
  });

  it("surfaces blind-source spend honestly (heartbeats but no cost steps → not in totals)", async () => {
    const root = await render({ cov: { status: "amber", blind_sources: ["codex"] } });
    expect(root.querySelector(".pulse-projection-warning")).toBeNull(); // trust owns capture health
    const delta = root.querySelector(".pulse-delta")!;
    expect(delta.className).toContain("muted");
    expect(delta.className).not.toContain("cost-ok");
    expect(delta.textContent).toMatch(/based on captured data/);
    expect(root.querySelector(".pulse-trust .unpriced")).toBeNull(); // no duplicate warning beside the graph
    const attention = root.querySelector(".pulse-attention .attn-capture")!;
    expect(attention.textContent).toContain("codex");
    expect(attention.textContent).toMatch(/missing from the totals, not zero/);
  });

  it("no budget cap → honest active-day context, setup action, and no promised cap series", async () => {
    const root = await render({ burn: { cap_micros: 0 } });
    const support = root.querySelector(".pulse-support")!;
    const items = support.querySelectorAll(".pulse-support-item");
    expect(items).toHaveLength(3);
    expect(items[0].textContent).toMatch(/Typical active day\$2/);
    expect(items[1].textContent).toMatch(/Active days10 \/ 10/);
    expect(items[1].querySelector("dd")?.getAttribute("title")).toMatch(/10 active days out of 10 elapsed days/);
    expect(items[2].textContent).toMatch(/Monthly capNot setSet cap/);
    expect(support.querySelector<HTMLAnchorElement>(".pulse-set-cap")?.href).toContain("sheet=settings");
    expect(root.querySelector(".pulse-scope")?.textContent).not.toMatch(/cap pace/i);
    expect(root.querySelector(".pulse-cap")).toBeNull();
  });
});

describe("Pulse composition — no hero/trust void", () => {
  it("is a content-driven vertical flow, not a coupled two-column head (no void by construction)", async () => {
    const { readFileSync } = await import("node:fs");
    const { resolve } = await import("node:path");
    const ws = readFileSync(resolve(process.cwd(), "src/ui/workspaces.css"), "utf8");
    const pulseRule = ws.match(/(?:^|\n)\.pulse\s*\{([^}]*)\}/)?.[1] ?? "";
    expect(pulseRule).toMatch(/flex-direction:\s*column/); // vertical flow — no compact-hero-beside-tall-panel
    expect(pulseRule).not.toMatch(/grid-template-columns/); // never a coupled two-column head
    // The answer and the drivers are siblings in that one flowing column — drivers never wait on a
    // taller trust sibling (the old statement-head void).
    const root = await render();
    const pulse = root.querySelector(".pulse")!;
    const kids = Array.from(pulse.children);
    const answerIdx = kids.findIndex((k) => k.classList.contains("pulse-answer"));
    const driversIdx = kids.findIndex((k) => k.classList.contains("pulse-drivers"));
    expect(answerIdx).toBeGreaterThanOrEqual(0);
    expect(driversIdx).toBeGreaterThan(answerIdx); // drivers follow the hero directly in the flow
  });

  it("keeps trust a compact strip with full details ONE ACTION away", async () => {
    const root = await render();
    const trust = root.querySelector(".pulse-trust")!;
    // Compact: one status plus one route to the owning trust surface, not a warning panel.
    expect(trust.querySelector(".pulse-trust-status.caption")).toBeTruthy();
    const details = trust.querySelector<HTMLAnchorElement>(".pulse-trust-details")!;
    expect(details).toBeTruthy();
    expect(details.getAttribute("href")).toContain("sheet=trust"); // scoped details in one click
    expect(details.textContent).toBe("Coverage");
    expect(details.getAttribute("aria-label")).toMatch(/coverage and pricing/i);
  });

  it("stays balanced across empty / partial / full coverage variants (trust always visible)", async () => {
    for (const cov of [
      { status: "none", has_proxy: false, has_otel: false, blind_sources: [] },
      { status: "amber", has_proxy: true, has_otel: false, blind_sources: ["codex"] },
      { status: "green", has_proxy: true, has_otel: true, blind_sources: [] },
    ]) {
      const root = await render({ cov });
      const trust = root.querySelector(".pulse-trust")!;
      expect(trust, `trust visible for ${cov.status}`).toBeTruthy();
      // Trust summary + the answer both present in every variant — layout never collapses/voids.
      expect(root.querySelector(".pulse-answer .stat.display")).toBeTruthy();
      expect(trust.querySelector(".pulse-trust-details")).toBeTruthy();
    }
  });
});

describe("Pulse attention queue", () => {
  const SOURCES = {
    anomalies: [
      { date: "2026-07-10", series_key: "model:gpt-4o", kind: "spike", value_micros: 5_000_000, baseline_micros: 1_000_000 },
      { date: "2026-07-11", series_key: "provider:openai", kind: "spike", value_micros: 1_200_000, baseline_micros: 1_000_000 },
      { date: "2026-07-12", series_key: "model:haiku", kind: "drop", value_micros: 100_000, baseline_micros: 900_000 }, // fell → skipped
    ],
    budget: { period: "month", spent_micros: 90_000_000, cap_micros: 80_000_000, warn_pct: 80, pct: 112, status: "over" },
    failures: { rows: [], total_micros: 2_000_000, total_failed_steps: 7, pct_of_spend: 4, pricing_version: "x", estimated: true },
    loops: { rows: [], total_micros: 500_000, total_redundant_steps: 3, pricing_version: "x", estimated: true },
    coverage: { status: "amber" as const, has_proxy: true, has_otel: false, blind_sources: ["codex"], sources: [] },
  };

  it("ranks rows by dollar impact, skips fallen spend, and floats the capture gap to the end", () => {
    const rows = buildAttentionRows(SOURCES as never);
    // Anomaly delta 4.0 (gpt-4o) > over-cap 10.0? cap over = 90-80 = 10.0 → budget first; then failures 2.0,
    // loops 0.5, anomaly openai 0.2; capture gap (no dollar) last.
    expect(rows[0].kind).toBe("budget"); // $10 over cap
    expect(rows[1].kind).toBe("anomaly"); // gpt-4o +$4
    expect(rows[rows.length - 1].kind).toBe("capture"); // no dollar → last
    expect(rows.find((row) => row.kind === "anomaly")?.href).toBe("#/investigate?mode=timeline");
    expect(rows.find((row) => row.kind === "budget")?.href).toBe("#/pulse?settings=data&sheet=settings");
    expect(rows.find((row) => row.kind === "failure")?.href).toBe("#/optimize");
    expect(rows.find((row) => row.kind === "loop")?.href).toBe("#/optimize");
    expect(rows.find((row) => row.kind === "capture")?.href).toBe("#/pulse?sheet=capture");
    // The dropped anomaly (spend fell) is not an attention row.
    expect(rows.some((r) => r.sentence.includes("haiku"))).toBe(false);
    // Dollar-bearing rows are strictly non-increasing.
    const dollars = rows.filter((r) => r.dollars >= 0).map((r) => r.dollars);
    expect(dollars).toEqual([...dollars].sort((a, b) => b - a));
  });

  it("renders plain-language sentence rows (not cards) that deep-link to evidence", async () => {
    const root = await render(SOURCES);
    const rows = root.querySelectorAll(".pulse-attention .pulse-attn-row");
    expect(rows.length).toBe(6); // 2 risen anomalies + budget + failure + loop + capture (haiku dropped → skipped)
    // Every row is an anchor with a deep-link href and a sentence; none is a .card.
    for (const r of Array.from(rows)) {
      expect(r.tagName).toBe("A");
      expect(r.getAttribute("href")).toMatch(/^#\//);
      expect(r.querySelector(".pulse-attn-text")?.textContent?.length).toBeGreaterThan(10);
      expect(r.classList.contains("card")).toBe(false);
    }
  });

  it("shows a capture gap honestly as uncaptured, never a fabricated $0", async () => {
    const root = await render(SOURCES);
    const cap = Array.from(root.querySelectorAll(".attn-capture"))[0];
    expect(cap.textContent).toMatch(/missing from the totals, not zero/);
    expect(cap.querySelector(".pulse-attn-dollars")?.textContent).toBe("uncaptured");
  });

  it("empty period → an explicit 'nothing needs attention', not a blank card", async () => {
    const root = await render(); // fakeClient defaults: no anomalies/waste
    expect(root.querySelector(".pulse-attention")?.textContent).toMatch(/Nothing needs attention/);
  });
});

describe("Pulse controllable drivers", () => {
  const LEDGER = {
    opportunities: [
      { kind: "cache", label: "Enable 1h cache on gpt-4o", recoverable_micros: 3_000_000, confidence: "projected", fix_text: "Set cache_control to 1h", effort: "S" },
      { kind: "model-swap", label: "Swap classify step to haiku", recoverable_micros: 8_000_000, confidence: "approximate", fix_text: "Route the classify step to haiku", effort: "M" },
    ],
    total_recoverable_micros: 11_000_000,
    total_spend_micros: 60_000_000,
    savings_index: 40,
    pricing_version: "x",
    estimated: true,
  };

  it("ranks drivers by recoverable dollars, each with a quantified action + confidence + effort", async () => {
    const root = await render({ savings: LEDGER });
    const rows = root.querySelectorAll(".pulse-drivers .pulse-drv-row");
    expect(rows.length).toBe(2);
    // Ranked by recoverable desc: model-swap ($8) before cache ($3).
    expect(rows[0].querySelector(".pulse-drv-label")?.textContent).toContain("haiku");
    // Every row carries the estimate + confidence + effort — never an unlabeled promise.
    for (const r of Array.from(rows)) {
      expect(r.querySelector(".pulse-drv-save")?.textContent).toMatch(/recoverable/);
      expect(r.querySelector(".pulse-drv-conf")?.textContent).toMatch(/(measured|projected|approximate) · effort [SML]/);
      expect(r.querySelector(".pulse-drv-fix")?.textContent?.length).toBeGreaterThan(5);
      expect(r.getAttribute("href")).toMatch(/^#\//);
    }
  });

  it("frames the ledger honestly as estimated capped potential, not a guaranteed floor", async () => {
    const root = await render({ savings: LEDGER });
    expect(root.querySelector(".pulse-drivers .caption")?.textContent).toMatch(/capped potential.*not a guaranteed floor/);
  });
});

describe("Pulse → shared Selection A", () => {
  it("sets an anomaly cohort in the store + sel URL, and Investigate consumes the hydrated selection", async () => {
    window.location.hash = "";
    const analysis = createAnalysisStore(initialAnalysisState());
    const setSelection = vi.spyOn(analysis, "setSelection");
    const root = document.createElement("div");
    await renderPulse(
      root,
      client({
        anomalies: [
          {
            date: "2026-07-10",
            series_key: "gpt-4o",
            kind: "spike",
            value_micros: 5_000_000,
            baseline_micros: 1_000_000,
          },
        ],
      }),
      ROUTE,
      { analysis }
    );

    (root.querySelector(".attn-anomaly") as HTMLAnchorElement).click();
    const expected: CohortSpec = {
      ...initialAnalysisState().scope,
      from: "2026-07-10",
      to: "2026-07-10",
      filters: [{ op: "eq", dimension: "model", value: "gpt-4o" }],
    };
    expect(setSelection).toHaveBeenCalledWith(expected);

    const selectedRoute = parseHash(window.location.hash);
    expect(selectedRoute.segments).toEqual(["investigate"]);
    expect(selectedRoute.query?.mode).toBe("timeline");
    expect(selectedRoute.query?.sel).toBeTruthy();
    expect(decodeAnalysisQuery(selectedRoute.query ?? {}).selection).toEqual(expected);

    // Simulate a fresh recipient opening the shared link: URL hydration, not the original in-memory
    // click, must provide the same Selection A to Investigate's cohort request and visible header.
    const fresh = createAnalysisStore(initialAnalysisState());
    await hydrateWorkspaceAnalysis(fresh, "investigate", selectedRoute);
    let resolved: CohortSpec | undefined;
    const investigateClient = fakeClient({
      resolveCohort: async (spec) => {
        resolved = spec;
        return {
          data: {
            run_ids: [],
            run_count: 0,
            step_count: 0,
            total_micros: 0,
            entity_rows: [],
          },
          provenance: {} as never,
        };
      },
    });
    const investigateRoot = document.createElement("div");
    await renderInvestigate(investigateRoot, investigateClient, selectedRoute, { analysis: fresh });
    expect(resolved).toEqual(expected);
    expect(investigateRoot.querySelector(".inv-selection")?.textContent).toMatch(/Selection A.*1 filter.*2026-07-10/);
  });

  it("maps a controllable driver to its exact v2 affected cohort before navigating", async () => {
    window.location.hash = "";
    const analysis = createAnalysisStore(initialAnalysisState());
    const setSelection = vi.spyOn(analysis, "setSelection");
    const affected: CohortSpec = {
      ...initialAnalysisState().scope,
      filters: [{ op: "run_ids", ids: ["r1", "r2"] }],
    };
    const opportunity = {
      kind: "cache",
      label: "Enable 1h cache on gpt-4o",
      recoverable_micros: 3_000_000,
      confidence: "projected",
      fix_text: "Set cache_control to 1h",
      effort: "S",
    };
    const root = document.createElement("div");
    await renderPulse(
      root,
      client({
        savings: {
          opportunities: [opportunity],
          opportunities_v2: [
            {
              ...opportunity,
              opportunity_key: "cache:abc",
              affected_run_count: 2,
              affected_step_count: 2,
              affected_run_ids: ["r1", "r2"],
              affected_steps: [],
              evidence_truncated: false,
              evidence_method: "affected runs",
              cohort_snapshot: affected,
              assumptions: [],
            },
          ],
          total_recoverable_micros: 3_000_000,
          total_spend_micros: 10_000_000,
          savings_index: 30,
          pricing_version: "x",
          estimated: true,
        },
      }),
      ROUTE,
      { analysis }
    );

    (root.querySelector(".pulse-drv-row") as HTMLAnchorElement).click();
    expect(setSelection).toHaveBeenCalledWith(affected);
    const selectedRoute = parseHash(window.location.hash);
    expect(selectedRoute.segments).toEqual(["investigate", "run", "r1"]);
    expect(decodeAnalysisQuery(selectedRoute.query ?? {}).selection).toEqual(affected);
  });

  it("keeps utility remedies in Pulse and waste evidence in Optimize", async () => {
    const analysis = createAnalysisStore(initialAnalysisState());
    const root = document.createElement("div");
    await renderPulse(root, client({
      budget: {
        period: "month", spent_micros: 90_000_000, cap_micros: 80_000_000,
        warn_pct: 80, pct: 112, status: "over",
      },
      failures: {
        rows: [], total_micros: 2_000_000, total_failed_steps: 2,
        pct_of_spend: 4, pricing_version: "x", estimated: true,
      },
      loops: {
        rows: [], total_micros: 1_000_000, total_redundant_steps: 1,
        pricing_version: "x", estimated: true,
      },
      cov: { status: "red", blind_sources: ["codex"] },
    }), ROUTE, { analysis });

    expect(root.querySelector(".attn-budget")?.getAttribute("href")).toBe("#/pulse?settings=data&sheet=settings");
    expect(root.querySelector(".attn-capture")?.getAttribute("href")).toBe("#/pulse?sheet=capture");
    for (const kind of ["failure", "loop"]) {
      const destination = parseHash(root.querySelector(`.attn-${kind}`)?.getAttribute("href") ?? "");
      expect(destination.segments).toEqual(["optimize"]);
      expect(decodeAnalysisQuery(destination.query ?? {}).selection).toEqual(initialAnalysisState().scope);
    }
  });
});

describe("Pulse over-budget Selection A links", () => {
  const opportunity = {
    kind: "cache",
    label: "Large exact affected cohort",
    recoverable_micros: 3_000_000,
    confidence: "projected",
    fix_text: "Apply the measured cache policy",
    effort: "S",
  };

  function largeSelection(): CohortSpec {
    return {
      ...initialAnalysisState().scope,
      filters: [
        {
          op: "run_ids",
          ids: Array.from({ length: 300 }, (_, i) => `affected-run-${String(i).padStart(4, "0")}`),
        },
      ],
    };
  }

  function largeLedger(selection: CohortSpec) {
    const ids = selection.filters[0].op === "run_ids" ? selection.filters[0].ids : [];
    return {
      opportunities: [opportunity],
      opportunities_v2: [
        {
          ...opportunity,
          opportunity_key: "cache:large",
          affected_run_count: ids.length,
          affected_step_count: ids.length,
          affected_run_ids: ids,
          affected_steps: [],
          evidence_truncated: false,
          evidence_method: "exact affected runs",
          cohort_snapshot: selection,
          assumptions: [],
        },
      ],
      total_recoverable_micros: 3_000_000,
      total_spend_micros: 10_000_000,
      savings_index: 30,
      pricing_version: "x",
      estimated: true,
    };
  }

  it("persists without truncation, navigates by investigation id, and fresh-loads Selection A", async () => {
    window.location.hash = "#/pulse";
    const selection = largeSelection();
    const persisted = new Map<string, SavedInvestigationV2>();
    const c = client({
      savings: largeLedger(selection),
      client: {
        saveInvestigation: async (investigation: SavedInvestigationV2) => {
          persisted.set(investigation.id, structuredClone(investigation));
        },
        listInvestigations: async () => [...persisted.values()],
      },
    });
    const active = createAnalysisStore(initialAnalysisState());
    const root = document.createElement("div");
    await renderPulse(root, c, ROUTE, { analysis: active });

    const link = root.querySelector(".pulse-drv-row") as HTMLAnchorElement;
    expect(link.title).toMatch(/saved locally/i);
    link.click();
    await vi.waitFor(() => expect(window.location.hash).toContain("investigation="));

    const route = parseHash(window.location.hash);
    const id = route.query?.investigation;
    expect(id).toBeTruthy();
    expect(route.segments).toEqual(["investigate", "run", "affected-run-0000"]);
    expect(route.query?.sel).toBeUndefined();
    const saved = persisted.get(id!);
    expect(saved?.state.selection).toEqual(selection);
    expect((saved?.state.selection?.filters[0] as { ids: string[] }).ids).toHaveLength(300);
    expect("focus" in (saved?.state ?? {})).toBe(false);

    // A fresh store has no in-memory selection. The compact URL must load the persisted record before
    // Investigate renders, producing the exact same 300-id cohort rather than a truncated substitute.
    const fresh = createAnalysisStore(initialAnalysisState());
    const freshRoot = document.createElement("div");
    await renderWorkspace("investigate", freshRoot, c, route, { analysis: fresh });
    expect(fresh.get().selection).toEqual(selection);
    expect(freshRoot.querySelector(".run-profile")).toBeTruthy();
  });

  it("shows a save failure and leaves both route and analysis state untouched", async () => {
    window.location.hash = "#/pulse";
    const selection = largeSelection();
    const c = client({
      savings: largeLedger(selection),
      client: {
        saveInvestigation: async () => {
          throw new Error("store unavailable");
        },
      },
    });
    const analysis = createAnalysisStore(initialAnalysisState());
    const root = document.createElement("div");
    await renderPulse(root, c, ROUTE, { analysis });
    (root.querySelector(".pulse-drv-row") as HTMLAnchorElement).click();

    await vi.waitFor(() => expect(root.querySelector(".pulse-selection-status .error")).toBeTruthy());
    expect(root.querySelector(".pulse-selection-status")?.textContent).toMatch(/was not opened/i);
    expect(window.location.hash).toBe("#/pulse");
    expect(analysis.get().selection).toBeNull();
    expect(analysis.get().workspace).toBe("pulse");
  });

  it("shows a load failure without rendering a misleading Investigate cohort", async () => {
    const analysis = createAnalysisStore(initialAnalysisState());
    const root = document.createElement("div");
    const route = parseHash("#/investigate?investigation=missing");
    const c = fakeClient({
      listInvestigations: async () => {
        throw new Error("store unavailable");
      },
    });

    await renderWorkspace("investigate", root, c, route, { analysis });
    expect(root.querySelector(".error")?.textContent).toMatch(/Couldn't open the saved investigation/);
    expect(root.querySelector(".investigate")).toBeNull();
    expect(analysis.get()).toEqual(initialAnalysisState());
  });
});

// The Pulse spend-anatomy Beam never mixes units, matches scoped totals, exposes the outline, and
// holds unpriced usage outside the dollar lane.
describe("Pulse Beam spend anatomy", () => {
  const LEDGER: SavingsLedger = {
    opportunities: [],
    total_recoverable_micros: 3_000_000,
    total_spend_micros: 10_000_000,
    savings_index: 70,
    pricing_version: "x",
    estimated: true,
    capped_potential_micros: 3_000_000,
  };

  it("uses one money lane whose segments sum to the scoped priced spend (no drift, no unit mixing)", () => {
    const m = pulseBeamModel(LEDGER, 8)!;
    expect(m).not.toBeNull();
    expect(m.unit).toBe("usd"); // one unit throughout — the gap lives outside the lane
    const sum = m.segments.reduce((a, s) => a + s.value, 0);
    expect(sum).toBe(LEDGER.total_spend_micros); // recoverable + remainder = scoped spend
    expect(m.segments.map((s) => s.key)).toEqual(["recoverable", "remainder"]);
  });

  it("holds unpriced usage as a detached token-SHARE marker, never a dollar segment", () => {
    const m = pulseBeamModel(LEDGER, 8)!;
    expect(m.gap).toEqual({ label: "Unpriced usage", share: 0.08 });
    expect(m.gap?.tokens).toBeUndefined(); // pulse has only the share; no fabricated count
    // Rendered: the gap sits outside the lane and outline, and shows no dollar figure.
    const b = tareBeam(m);
    const gap = b.querySelector(".tare-beam-gap")!;
    expect(gap.closest(".tare-beam-track")).toBeNull();
    expect(b.querySelector("ol.tare-beam-outline")).toBeTruthy(); // outline exposed
    expect(gap.textContent).not.toContain("$");
  });

  it("clamps capped potential to spend so remainder never goes negative", () => {
    const m = pulseBeamModel({ ...LEDGER, capped_potential_micros: 99_000_000 }, 0)!;
    const recoverable = m.segments.find((s) => s.key === "recoverable")!;
    const remainder = m.segments.find((s) => s.key === "remainder")!;
    expect(recoverable.value).toBe(LEDGER.total_spend_micros); // clamped ≤ spend
    expect(remainder.value).toBe(0);
    expect(m.gap).toBeUndefined(); // 0% unpriced → no marker
  });

  it("returns null when there is no priced spend to partition (no empty fabricated lane)", () => {
    expect(pulseBeamModel({ ...LEDGER, total_spend_micros: 0 }, 5)).toBeNull();
  });
});
