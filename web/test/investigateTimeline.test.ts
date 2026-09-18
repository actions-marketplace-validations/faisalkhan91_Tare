// The timeline brush is built from scoped cohort responses,
// quarantines the still-open local calendar day, and writes one durable Selection A + measured
// equal-window Baseline B into the canonical analysis state/URL.

import { describe, expect, it } from "vitest";
import { createAnalysisStore, initialAnalysisState } from "../src/analysis/store.js";
import { renderInvestigate } from "../src/workspaces/investigate.js";
import {
  loadInvestigateTimeline,
  quarantineIncompleteDays,
} from "../src/workspaces/investigateTimeline.js";
import { fakeClient, fakeProvenance } from "./fakeClient.js";
import type { CohortSpec, SavingsAction } from "../src/analysis/types.js";
import type { Route } from "../src/ui/store.js";

const SCOPE: CohortSpec = {
  from: "2026-07-10",
  to: "2026-07-12",
  timezone: "America/Los_Angeles",
  entity: "run",
  filters: [{ op: "eq", dimension: "model", value: "gpt-4o" }],
  pricing: { mode: "effective_dated" },
  metric: "spend_micros",
  normalization: "absolute",
  outcome_denominator: null,
};

function timelineClient() {
  const days = (from: string, to: string): string[] => {
    const out: string[] = [];
    const cursor = new Date(`${from}T00:00:00Z`);
    const end = new Date(`${to}T00:00:00Z`);
    while (cursor <= end) {
      out.push(cursor.toISOString().slice(0, 10));
      cursor.setUTCDate(cursor.getUTCDate() + 1);
    }
    return out;
  };
  return fakeClient({
    trend: async () => ({
      dimension: "total",
      from: "2026-07-10",
      to: "2026-07-12",
      days: ["2026-07-10", "2026-07-11", "2026-07-12"],
      series: [],
      pricing_version: "fixture",
      estimated: true,
    }),
    resolveCohort: async (spec) => {
      const day = Number((spec.to ?? "").slice(-2));
      const runCount = day <= 10 ? 4 : 10;
      return {
        data: {
          run_ids: Array.from({ length: runCount }, (_, i) => `${spec.from}-r${i}`),
          run_count: runCount,
          step_count: runCount,
          total_micros: day * 1_000_000,
          entity_rows: [],
        },
        provenance: fakeProvenance(spec),
      };
    },
    timelineCohort: async (req) => {
      const range = days(req.cohort.from!, req.cohort.to!);
      const runCount = Number(req.cohort.to!.slice(-2)) <= 10 ? 4 : 10;
      return {
        data: {
          days: range,
          run_count: runCount,
          unit: "estimated_micro_usd" as const,
          series: [{
            key: "total",
            points: range.map((day) => ({
              day,
              value: Number(day.slice(-2)) * 1_000_000,
              support_count: runCount,
            })),
          }],
          config_events: range.includes("2026-07-11") ? [{
            occurred_at: "2026-07-11T16:30:00Z",
            day: "2026-07-11",
            source: "settings",
            changed_fields: ["capture.mode"],
          }] : [],
        },
        provenance: fakeProvenance(req.cohort),
      };
    },
    savingsActions: async () => [
      {
        opportunity_key: "cache:model:gpt-4o",
        cohort_hash: "abc",
        status: "applied",
        acted_at: "2026-07-11T16:00:00Z",
        cohort: SCOPE,
        match: { kind: "aggregate_only" },
        metric: "spend_micros",
        normalization: "absolute",
        compatibility_warnings: [],
      } satisfies SavingsAction,
    ],
  });
}

describe("Investigate timeline — honest calendar and scoped data", () => {
  it("quarantines the current/future local day instead of presenting partial totals as complete", () => {
    const split = quarantineIncompleteDays(
      ["2026-07-15", "2026-07-16", "2026-07-17"],
      "America/Los_Angeles",
      new Date("2026-07-16T18:00:00Z")
    );
    expect(split.complete).toEqual(["2026-07-15"]);
    expect(split.quarantined).toEqual(["2026-07-16", "2026-07-17"]);
    expect(split.localToday).toBe("2026-07-16");
  });

  it("loads observed and equal prior-window lines from the filtered cohort and annotates persisted interventions", async () => {
    const seen: CohortSpec[] = [];
    const base = timelineClient();
    const client = fakeClient({
      ...base,
      timelineCohort: async (req) => {
        seen.push(req.cohort);
        return base.timelineCohort(req);
      },
    });
    const model = await loadInvestigateTimeline(client, SCOPE, null, "total", new Date("2026-07-20T12:00:00Z"));
    expect(model.days).toEqual(["2026-07-10", "2026-07-11", "2026-07-12"]);
    expect(model.baselineDays).toEqual(["2026-07-07", "2026-07-08", "2026-07-09"]);
    expect(model.series[0].observed).toEqual([10_000_000, 11_000_000, 12_000_000]);
    expect(model.series[0].comparison).toEqual([7_000_000, 8_000_000, 9_000_000]);
    expect(model.selectionSampleCount).toBe(10);
    expect(model.baselineSampleCount).toBe(4);
    expect(model.annotations).toEqual([
      expect.objectContaining({ date: "2026-07-11", kind: "config_change" }),
      expect.objectContaining({ date: "2026-07-11", kind: "intervention" }),
    ]);
    expect(seen).toHaveLength(2);
    expect(seen.every((spec) => spec.filters[0]?.op === "eq")).toBe(true);
  });

  it("preserves unavailable outcome denominators as null points with a visible reason", async () => {
    const scope: CohortSpec = {
      ...SCOPE,
      normalization: "per_outcome",
      outcome_denominator: { kind: "metered", metered: "pull_requests" },
    };
    const base = timelineClient();
    const client = fakeClient({
      ...base,
      timelineCohort: async (req) => {
        const response = await base.timelineCohort(req);
        return {
          ...response,
          data: {
            ...response.data,
            unit: "estimated_micro_usd_per_outcome" as const,
            series: response.data.series.map((series) => ({
              ...series,
              points: series.points.map((point) => ({
                ...point,
                value: null,
                unavailable_reason: "metered outcome denominator is unavailable for a filtered cohort",
              })),
            })),
          },
        };
      },
    });
    const model = await loadInvestigateTimeline(client, scope, null, "total", new Date("2026-07-20T12:00:00Z"));
    expect(model.series[0].observed).toEqual([null, null, null]);
    expect(model.unavailableReasons).toEqual([
      "metered outcome denominator is unavailable for a filtered cohort",
    ]);
  });
});

describe("Investigate timeline — canonical brush state", () => {
  it("brushes complete days into Selection A and a measured prior Baseline B that survive the URL", async () => {
    window.location.hash = "#/investigate?mode=timeline";
    const analysis = createAnalysisStore(initialAnalysisState("investigate"));
    analysis.setScope(SCOPE);
    const root = document.createElement("div");
    const route: Route = {
      name: "investigate",
      segments: ["investigate"],
      query: { mode: "timeline" },
    };

    await renderInvestigate(root, timelineClient(), route, { analysis });
    expect(root.querySelector(".inv-timeline-chart")).toBeTruthy();
    expect(root.querySelector<HTMLSelectElement>(".inv-timeline-metric")?.value).toBe("spend_micros");
    expect(root.querySelector<HTMLSelectElement>(".inv-timeline-group")?.value).toBe("total");

    const start = root.querySelector<HTMLInputElement>(".inv-brush-start")!;
    const end = root.querySelector<HTMLInputElement>(".inv-brush-end")!;
    start.value = "1";
    end.value = "2";
    start.dispatchEvent(new Event("input"));
    end.dispatchEvent(new Event("input"));
    root.querySelector<HTMLButtonElement>(".inv-brush-apply")!.click();

    await new Promise((resolve) => setTimeout(resolve, 0));
    const state = analysis.get();
    expect(state.selection?.from).toBe("2026-07-11");
    expect(state.selection?.to).toBe("2026-07-12");
    expect(state.selection?.filters).toEqual(SCOPE.filters);
    expect(state.baseline).toMatchObject({
      kind: "prior_window",
      label: "Prior window · 4 runs",
      sampleCount: 4,
      cohort: { from: "2026-07-09", to: "2026-07-10", timezone: "America/Los_Angeles" },
    });
    expect(window.location.hash).toContain("mode=timeline");
    expect(window.location.hash).toContain("sel=");
    expect(window.location.hash).toContain("base=");
    expect(window.location.hash).toContain("base_kind=prior_window");
    expect(window.location.hash).toContain("base_n=4");
  });
});
