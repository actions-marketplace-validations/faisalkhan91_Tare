// Calibrated Bench cohort transport parity. The HTTP and Tauri clients
// call the SAME shared tare-cli `cohort_*_api` fns server-side, so given one backend payload both
// clients must surface byte-identical `AnalysisResponse` data. These tests pin the endpoint/command
// wiring (paths, command names, camelCase `body` arg) and assert client-layer equivalence.

import { describe, it, expect, vi, afterEach } from "vitest";
import { createHttpClient } from "../src/httpClient.js";
import { createTauriClient } from "../src/tauriClient.js";
import type {
  AnalysisResponse,
  CohortCompareRequest,
  CohortCompareResult,
  CohortFacetRequest,
  CohortFacetResult,
  CohortResolveResult,
  CohortSearchRequest,
  CohortSearchResult,
  CohortSpec,
  CohortTimelineRequest,
  CohortTimelineResult,
  ExperimentRequest,
  ExperimentResult,
} from "../src/analysis/types.js";
import type { AnomalyWhy, AnomalyWhyRequest, FlameDiffModel } from "../src/client.js";

const SPEC: CohortSpec = {
  timezone: "UTC",
  entity: "run",
  filters: [],
  pricing: { mode: "effective_dated" },
  metric: "spend_micros",
  normalization: "absolute",
};

// One canonical backend payload per endpoint, shared by BOTH transports so equivalence is testable.
const RESOLVE: AnalysisResponse<CohortResolveResult> = {
  data: { run_ids: ["r1"], run_count: 1, step_count: 3, total_micros: 4210, entity_rows: [] },
  provenance: {
    refreshed_at: "2026-07-10T00:00:00Z",
    scope: SPEC,
    capture_sources: ["claude_code"],
    coverage_status: "unknown",
    component_fidelity: "coarse",
    pricing_edition: { version: "v1", effective_date: "2026-07-01", mode: "effective" },
    allocation_method: "provider_counts",
    value_class: "derived",
    assumptions: ["legacy_date_bucket"],
  },
};
const FACETS: AnalysisResponse<CohortFacetResult> = {
  data: { dimension: "model", rows: [] },
  provenance: RESOLVE.provenance,
};
const COMPARE: AnalysisResponse<CohortCompareResult> = {
  data: {
    selection: RESOLVE.data,
    baseline: RESOLVE.data,
    total_delta_micros: 0,
    volume_delta_micros: 0,
    size_delta_micros: 0,
    efficiency_delta_micros: 0,
    compatibility_warnings: ["aggregate_only"],
  },
  provenance: RESOLVE.provenance,
};
const SEARCH: AnalysisResponse<CohortSearchResult> = {
  data: { entities: [{ run_id: "r1" }], truncated: false },
  provenance: RESOLVE.provenance,
};
const TIMELINE: AnalysisResponse<CohortTimelineResult> = {
  data: {
    days: ["2026-07-10"],
    run_count: 1,
    unit: "tokens",
    series: [{ key: "total", points: [{ day: "2026-07-10", value: 1200, support_count: 1 }] }],
    config_events: [{
      occurred_at: "2026-07-10T12:00:00Z",
      day: "2026-07-10",
      source: "settings",
      changed_fields: ["capture.mode"],
    }],
  },
  provenance: RESOLVE.provenance,
};

const FACET_REQ: CohortFacetRequest = { selection: SPEC, baseline: SPEC, dimension: "model" };
const COMPARE_REQ: CohortCompareRequest = {
  selection: SPEC,
  baseline: SPEC,
  match: { kind: "aggregate_only" },
};
const SEARCH_REQ: CohortSearchRequest = { cohort: SPEC, query: "r1" };
const TIMELINE_REQ: CohortTimelineRequest = {
  cohort: { ...SPEC, from: "2026-07-10", to: "2026-07-10", metric: "tokens" },
  group: "total",
};

function payloadFor(pathOrCmd: string): string {
  if (pathOrCmd.includes("resolve")) return JSON.stringify(RESOLVE);
  if (pathOrCmd.includes("facets") || pathOrCmd.includes("facet")) return JSON.stringify(FACETS);
  if (pathOrCmd.includes("compare")) return JSON.stringify(COMPARE);
  if (pathOrCmd.includes("search")) return JSON.stringify(SEARCH);
  if (pathOrCmd.includes("timeline")) return JSON.stringify(TIMELINE);
  return "{}";
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("cohort HTTP transport", () => {
  it("POSTs each request to its /__tare/cohort/* endpoint and parses the envelope", async () => {
    const calls: Array<[string, unknown]> = [];
    vi.stubGlobal("fetch", async (url: string, init?: RequestInit) => {
      calls.push([url, init?.body ? JSON.parse(init.body as string) : undefined]);
      return new Response(payloadFor(url));
    });
    const c = createHttpClient("http://127.0.0.1:8788");

    expect((await c.resolveCohort(SPEC)).data.total_micros).toBe(4210);
    expect((await c.facetCohort(FACET_REQ)).data.dimension).toBe("model");
    expect((await c.compareCohort(COMPARE_REQ)).data.compatibility_warnings).toEqual([
      "aggregate_only",
    ]);
    expect((await c.searchCohort(SEARCH_REQ)).data.entities[0].run_id).toBe("r1");
    expect((await c.timelineCohort(TIMELINE_REQ)).data.unit).toBe("tokens");

    expect(calls[0][0]).toBe("http://127.0.0.1:8788/__tare/cohort/resolve");
    expect(calls[0][1]).toEqual(SPEC); // the spec is POSTed verbatim (snake_case wire form)
    expect(calls[1][0]).toContain("/__tare/cohort/facets");
    expect(calls[2][0]).toContain("/__tare/cohort/compare");
    expect(calls[3][0]).toContain("/__tare/cohort/search");
    expect(calls[4]).toEqual(["http://127.0.0.1:8788/__tare/cohort/timeline", TIMELINE_REQ]);
  });

  it("surfaces a non-2xx cohort error", async () => {
    vi.stubGlobal("fetch", async () => new Response('{"error":"timezone is empty"}', { status: 400 }));
    const c = createHttpClient();
    await expect(c.resolveCohort(SPEC)).rejects.toThrow(/HTTP 400/);
  });
});

describe("cohort Tauri transport", () => {
  it("invokes each command with a JSON body arg and parses the envelope", async () => {
    const calls: Array<[string, unknown]> = [];
    const invoke = async (cmd: string, args?: Record<string, unknown>) => {
      calls.push([cmd, args]);
      return payloadFor(cmd);
    };
    const c = createTauriClient(invoke);

    expect((await c.resolveCohort(SPEC)).data.run_count).toBe(1);
    expect((await c.facetCohort(FACET_REQ)).data.dimension).toBe("model");
    expect((await c.compareCohort(COMPARE_REQ)).data.total_delta_micros).toBe(0);
    expect((await c.searchCohort(SEARCH_REQ)).data.truncated).toBe(false);
    expect((await c.timelineCohort(TIMELINE_REQ)).data.config_events[0].changed_fields).toEqual(["capture.mode"]);

    expect(calls.map((x) => x[0])).toEqual([
      "cohort_resolve",
      "cohort_facets",
      "cohort_compare",
      "cohort_search",
      "cohort_timeline",
    ]);
    // The request DTO is passed as a JSON `body` string the Rust command re-parses.
    expect(calls[0][1]).toEqual({ body: JSON.stringify(SPEC) });
    expect(calls[2][1]).toEqual({ body: JSON.stringify(COMPARE_REQ) });
    expect(calls[4][1]).toEqual({ body: JSON.stringify(TIMELINE_REQ) });
  });
});

describe("cohort HTTP/Tauri equivalence", () => {
  it("both transports surface identical data for the same backend payload", async () => {
    vi.stubGlobal("fetch", async (url: string) => new Response(payloadFor(url)));
    const http = createHttpClient();
    const tauri = createTauriClient(async (cmd: string) => payloadFor(cmd));

    // Same server payload → identical parsed AnalysisResponse across transports (acceptance).
    expect(await http.resolveCohort(SPEC)).toEqual(await tauri.resolveCohort(SPEC));
    expect(await http.facetCohort(FACET_REQ)).toEqual(await tauri.facetCohort(FACET_REQ));
    expect(await http.compareCohort(COMPARE_REQ)).toEqual(await tauri.compareCohort(COMPARE_REQ));
    expect(await http.searchCohort(SEARCH_REQ)).toEqual(await tauri.searchCohort(SEARCH_REQ));
    const timelineMatrix: CohortTimelineRequest[] = [
      ...(["spend_micros", "tokens"] as const).flatMap((metric) =>
        (["absolute", "per_run", "share_of_selection", "per_outcome"] as const).map((normalization) => ({
          cohort: {
            ...TIMELINE_REQ.cohort,
            metric,
            normalization,
            outcome_denominator: normalization === "per_outcome"
              ? { kind: "metered" as const, metered: "successful_runs" as const }
              : null,
          },
          group: "total" as const,
        }))
      ),
      { cohort: { ...TIMELINE_REQ.cohort, metric: "cache_hit_rate", normalization: "absolute" }, group: "total" },
      { cohort: TIMELINE_REQ.cohort, group: "provider" },
      { cohort: TIMELINE_REQ.cohort, group: "model" },
      { cohort: TIMELINE_REQ.cohort, group: "cause" },
    ];
    for (const req of timelineMatrix) {
      expect(await http.timelineCohort(req)).toEqual(await tauri.timelineCohort(req));
    }
  });
});

// Scoped anomaly explanation: a bare AnomalyWhy[] over both transports.
const WHY: AnomalyWhy[] = [
  {
    series_key: "total",
    total_delta_micros: 4210,
    volume_micros: 4000,
    size_micros: 210,
    efficiency_micros: 0,
    headline: "spend up on more requests",
    bisect_hint: "tare bisect ...",
  },
];
const WHY_REQ: AnomalyWhyRequest = { dimension: "total", scope: SPEC };

describe("anomaly-why transport", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("HTTP posts to /__tare/anomaly_why and parses the array", async () => {
    const calls: Array<[string, unknown]> = [];
    vi.stubGlobal("fetch", async (url: string, init?: RequestInit) => {
      calls.push([url, init?.body ? JSON.parse(init.body as string) : undefined]);
      return new Response(JSON.stringify(WHY));
    });
    const c = createHttpClient("http://127.0.0.1:8788");
    const rows = await c.anomalyWhy(WHY_REQ);
    expect(rows[0].headline).toBe("spend up on more requests");
    expect(calls[0][0]).toBe("http://127.0.0.1:8788/__tare/anomaly_why");
    expect(calls[0][1]).toEqual(WHY_REQ); // scope + dimension POSTed verbatim
  });

  it("Tauri invokes anomaly_why with a JSON body arg", async () => {
    const calls: Array<[string, unknown]> = [];
    const c = createTauriClient(async (cmd: string, args?: Record<string, unknown>) => {
      calls.push([cmd, args]);
      return JSON.stringify(WHY);
    });
    const rows = await c.anomalyWhy(WHY_REQ);
    expect(rows[0].volume_micros).toBe(4000);
    expect(calls[0][0]).toBe("anomaly_why");
    expect(calls[0][1]).toEqual({ body: JSON.stringify(WHY_REQ) });
  });

  it("both transports surface identical rows for the same payload", async () => {
    vi.stubGlobal("fetch", async () => new Response(JSON.stringify(WHY)));
    const http = createHttpClient();
    const tauri = createTauriClient(async () => JSON.stringify(WHY));
    expect(await http.anomalyWhy(WHY_REQ)).toEqual(await tauri.anomalyWhy(WHY_REQ));
  });
});

// Hierarchical run-pair flame diff — distinct from the row-level `diff`.
const FLAME: FlameDiffModel = {
  run_a: "A",
  run_b: "B",
  normalized: true,
  total_a_micros: 100,
  total_b_micros: 140,
  root: {
    name: "root",
    tokens_a: 10,
    tokens_b: 14,
    micros_a: 100,
    micros_b: 140,
    delta_micros: 40,
    share_a_bps: 10000,
    share_b_bps: 10000,
    delta_bps: 0,
    children: [],
  },
};

describe("flame-diff transport", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("HTTP GETs /__tare/flame_diff with the explicit pair + normalized flag", async () => {
    const calls: string[] = [];
    vi.stubGlobal("fetch", async (url: string) => {
      calls.push(url);
      return new Response(JSON.stringify(FLAME));
    });
    const c = createHttpClient("http://127.0.0.1:8788");
    const d = await c.flameDiff("A", "B", true);
    expect(d.root.delta_micros).toBe(40);
    expect(calls[0]).toBe(
      "http://127.0.0.1:8788/__tare/flame_diff?a=A&b=B&normalized=true"
    );
    // Defaults to non-normalized (absolute dollars) when the flag is omitted.
    await c.flameDiff("A", "B");
    expect(calls[1]).toContain("normalized=false");
  });

  it("Tauri invokes flame_diff with a/b/normalized args", async () => {
    const calls: Array<[string, unknown]> = [];
    const c = createTauriClient(async (cmd: string, args?: Record<string, unknown>) => {
      calls.push([cmd, args]);
      return JSON.stringify(FLAME);
    });
    await c.flameDiff("A", "B", true);
    expect(calls[0][0]).toBe("flame_diff");
    expect(calls[0][1]).toEqual({ a: "A", b: "B", normalized: true });
  });

  it("both transports surface an equivalent FlameDiffModel", async () => {
    vi.stubGlobal("fetch", async () => new Response(JSON.stringify(FLAME)));
    const http = createHttpClient();
    const tauri = createTauriClient(async () => JSON.stringify(FLAME));
    expect(await http.flameDiff("A", "B", true)).toEqual(
      await tauri.flameDiff("A", "B", true)
    );
  });
});

// Offline cost experiment: a counterfactual grid + Pareto set over both
// transports, complementing the read-only captured frontier.
const EXP_REQ: ExperimentRequest = {
  cohort: SPEC,
  experiment: { axes: [{ kind: "cache_strategy", values: ["*as-captured*", "decache"] }] },
  quality_constraint: { min: 90 },
};
const EXP_RESULT: ExperimentResult = {
  cells: [
    { coords: [{ axis: "cache_strategy", value: false }], label: ["as-captured"], cost_micros: 100, approximate: false },
    { coords: [{ axis: "cache_strategy", value: true }], label: ["decache"], cost_micros: 140, approximate: false },
  ],
  pareto: [0],
  baseline_micros: 100,
  best_micros: 100,
  best_saving_micros: 0,
  pricing_version: "v1",
  estimated: true,
  approximate: false,
};

describe("experiment transport", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("HTTP posts the ExperimentRequest to /__tare/experiment", async () => {
    const calls: Array<[string, unknown]> = [];
    vi.stubGlobal("fetch", async (url: string, init?: RequestInit) => {
      calls.push([url, init?.body ? JSON.parse(init.body as string) : undefined]);
      return new Response(JSON.stringify(EXP_RESULT));
    });
    const c = createHttpClient("http://127.0.0.1:8788");
    const r = await c.runExperiment(EXP_REQ);
    expect(r.best_saving_micros).toBe(0);
    expect(r.pareto).toEqual([0]);
    expect(calls[0][0]).toBe("http://127.0.0.1:8788/__tare/experiment");
    expect(calls[0][1]).toEqual(EXP_REQ); // cohort + axis grid + quality gate POSTed verbatim
  });

  it("Tauri invokes experiment with a JSON body arg", async () => {
    const calls: Array<[string, unknown]> = [];
    const c = createTauriClient(async (cmd: string, args?: Record<string, unknown>) => {
      calls.push([cmd, args]);
      return JSON.stringify(EXP_RESULT);
    });
    const r = await c.runExperiment(EXP_REQ);
    expect(r.cells.length).toBe(2);
    expect(calls[0][0]).toBe("experiment");
    expect(calls[0][1]).toEqual({ body: JSON.stringify(EXP_REQ) });
  });

  it("both transports surface an equivalent ExperimentResult", async () => {
    vi.stubGlobal("fetch", async () => new Response(JSON.stringify(EXP_RESULT)));
    const http = createHttpClient();
    const tauri = createTauriClient(async () => JSON.stringify(EXP_RESULT));
    expect(await http.runExperiment(EXP_REQ)).toEqual(await tauri.runExperiment(EXP_REQ));
  });
});
