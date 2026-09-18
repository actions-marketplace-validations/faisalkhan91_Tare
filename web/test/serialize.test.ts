// Analysis URL codec + AnalysisState store + baseline derivation.
// Acceptance: state round-trips Unicode and survives workspace navigation; focus is transient;
// invalid/oversized URLs fail VISIBLY; baseline labels/sample counts are honest.

import { describe, it, expect } from "vitest";
import {
  AnalysisUrlError,
  base64urlDecode,
  base64urlEncode,
  decodeAnalysisQuery,
  decodeCohort,
  decodeFilters,
  encodeAnalysisQuery,
  encodeCohort,
  encodeFilters,
  encodeOutcome,
  parseOutcome,
  parsePricing,
  encodePricing,
  MAX_HASH_LEN,
  type AnalysisUrlState,
} from "../src/analysis/serialize.js";
import {
  createAnalysisStore,
  serializeAnalysisHash,
  hydrateFromQuery,
  analysisToUrlState,
} from "../src/analysis/store.js";
import {
  priorWindow,
  priorWindowBaseline,
  pinnedRunBaseline,
  explicitCohortBaseline,
  baselineLabel,
} from "../src/analysis/state.js";
import type { AnalysisState } from "../src/analysis/state.js";
import type { CohortSpec } from "../src/analysis/types.js";

const SCOPE: CohortSpec = {
  from: "2026-05-01",
  to: "2026-05-07",
  timezone: "America/Los_Angeles",
  entity: "run",
  filters: [{ op: "eq", dimension: "model", value: "claude-opus-4-8" }],
  pricing: { mode: "effective_dated" },
  metric: "spend_micros",
  normalization: "absolute",
  outcome_denominator: null,
};

function state(): AnalysisState {
  return {
    workspace: "investigate",
    scope: structuredClone(SCOPE),
    selection: null,
    baseline: null,
    match: { kind: "aggregate_only" },
    focus: { pane: "canvas", highlighted: null },
    comparison: [],
    pinned: null,
  };
}

describe("base64url", () => {
  it("round-trips Unicode, is padding-free and URL-safe", () => {
    for (const s of ["", "hello", "café ☕ 日本語 — αβγ", "a/b+c=d?e&f"]) {
      const enc = base64urlEncode(s);
      expect(enc).not.toMatch(/[+/=]/); // URL-safe alphabet, no padding
      expect(base64urlDecode(enc)).toBe(s);
    }
  });
  it("is deterministic", () => {
    expect(base64urlEncode("日本語")).toBe(base64urlEncode("日本語"));
  });
  it("fails visibly on a malformed payload", () => {
    expect(() => base64urlDecode("!!!not-base64!!!")).toThrow(AnalysisUrlError);
  });
});

describe("outcome and pricing codec", () => {
  it("round-trips work_unit and metered outcomes", () => {
    expect(parseOutcome(encodeOutcome({ kind: "work_unit", name: "PRs merged" }))).toEqual({
      kind: "work_unit",
      name: "PRs merged",
    });
    expect(parseOutcome("metered:lines_added_per1k")).toEqual({
      kind: "metered",
      metered: "lines_added_per1k",
    });
  });
  it("rejects unknown metered kinds and malformed outcomes visibly", () => {
    expect(() => parseOutcome("metered:bogus")).toThrow(/unknown metered outcome/);
    expect(() => parseOutcome("unit:")).toThrow(/empty/);
    expect(() => parseOutcome("garbage")).toThrow(AnalysisUrlError);
  });
  it("round-trips pricing modes and rejects unknown", () => {
    expect(parsePricing("effective_dated")).toEqual({ mode: "effective_dated" });
    expect(parsePricing("latest")).toEqual({ mode: "latest" });
    expect(parsePricing(encodePricing({ mode: "as_of", date: "2026-01-01" }))).toEqual({
      mode: "as_of",
      date: "2026-01-01",
    });
    expect(() => parsePricing("someday")).toThrow(/unknown pricing/);
  });
});

describe("filters and cohort encoding (f / sel / base)", () => {
  it("round-trips arbitrary filters (Unicode preserved; canonical In-value ordering)", () => {
    const filters: CohortSpec["filters"] = [
      { op: "eq", dimension: "session", value: "café ☕ 日本語" },
      { op: "in", dimension: "model", values: ["claude-opus-4-8", "z", "a"] },
    ];
    const decoded = decodeFilters(encodeFilters(filters));
    // Canonical form: filters sorted by key, In values sorted (mirrors the Rust canonicalization).
    expect(decoded).toEqual([
      { op: "eq", dimension: "session", value: "café ☕ 日本語" },
      { op: "in", dimension: "model", values: ["a", "claude-opus-4-8", "z"] },
    ]);
  });
  it("encodes deterministically regardless of filter input order", () => {
    const a: CohortSpec["filters"] = [
      { op: "eq", dimension: "provider", value: "anthropic" },
      { op: "gte_micros", value: 1000 },
    ];
    const b: CohortSpec["filters"] = [a[1], a[0]];
    expect(encodeFilters(a)).toBe(encodeFilters(b));
  });
  it("round-trips a full CohortSpec through sel/base", () => {
    expect(decodeCohort(encodeCohort(SCOPE), "sel")).toEqual(SCOPE);
  });
  it("fails visibly when a sel/base payload is not a CohortSpec", () => {
    expect(() => decodeCohort(base64urlEncode('{"nope":1}'), "sel")).toThrow(/not a CohortSpec/);
  });
});

describe("query encode and decode", () => {
  it("round-trips the full analysis URL state", () => {
    const url: AnalysisUrlState = {
      from: "2026-05-01",
      to: "2026-05-07",
      tz: "America/Los_Angeles",
      entity: "step",
      metric: "tokens",
      norm: "per_outcome",
      pricing: { mode: "as_of", date: "2026-01-01" },
      outcome: { kind: "metered", metered: "successful_runs" },
      sheet: "trust",
      view: "facets",
      filters: [{ op: "eq", dimension: "model", value: "claude-opus-4-8" }],
      selection: SCOPE,
    };
    expect(decodeAnalysisQuery(encodeAnalysisQuery(url))).toEqual(url);
  });
  it("rejects unknown enum values visibly (no silent guess)", () => {
    expect(() => decodeAnalysisQuery({ entity: "cluster" })).toThrow(/unknown entity/);
    expect(() => decodeAnalysisQuery({ metric: "dollars" })).toThrow(/unknown metric/);
    expect(() => decodeAnalysisQuery({ norm: "sideways" })).toThrow(/unknown norm/);
  });
  it("ignores unrecognized keys (router/other features own them)", () => {
    expect(decodeAnalysisQuery({ mode: "now", tz: "UTC" })).toEqual({ tz: "UTC" });
  });
  it("omits empty filter arrays (nothing to share)", () => {
    expect(encodeAnalysisQuery({ filters: [] }).f).toBeUndefined();
  });
  it("round-trips an explicit whole-scope selection marker", () => {
    const query = encodeAnalysisQuery({ selection: null });
    expect(query.sel).toBe("none");
    expect(decodeAnalysisQuery(query).selection).toBeNull();

    const base = state();
    base.selection = SCOPE;
    expect(hydrateFromQuery(base, query).selection).toBeNull();
  });
});

describe("AnalysisState store", () => {
  it("durable state survives workspace navigation; focus is transient (reset)", () => {
    const store = createAnalysisStore(state());
    store.setSelection(structuredClone(SCOPE));
    store.setFocus({ pane: "inspector", inspectorTab: "steps" });
    store.navigateWorkspace("optimize");
    const s = store.get();
    expect(s.workspace).toBe("optimize");
    expect(s.selection).toEqual(SCOPE); // durable — survived
    expect(s.scope).toEqual(SCOPE); // durable — survived
    expect(s.focus).toEqual({ pane: "canvas", highlighted: null }); // transient — reset
  });
  it("durable() omits the transient focus", () => {
    const store = createAnalysisStore(state());
    store.setFocus({ pane: "rail" });
    expect("focus" in store.durable()).toBe(false);
  });
  it("notifies subscribers on change and stops after unsubscribe", () => {
    const store = createAnalysisStore(state());
    const seen: string[] = [];
    const off = store.subscribe((s) => seen.push(s.workspace));
    store.navigateWorkspace("pulse");
    off();
    store.navigateWorkspace("optimize");
    expect(seen).toEqual(["investigate", "pulse"]); // initial + one change, none after unsubscribe
  });
  it("hydrates scope from a URL query and round-trips back", () => {
    const q = encodeAnalysisQuery(analysisToUrlState(state()));
    const hydrated = hydrateFromQuery(state(), q);
    expect(hydrated.scope).toEqual(SCOPE);
  });
  it("hydrates legacy base= as an explicit baseline instead of dropping it on a fresh link", () => {
    const baseline = { ...SCOPE, from: "2026-04-01", to: "2026-04-07" };
    const q = encodeAnalysisQuery({ baseline });
    const hydrated = hydrateFromQuery(state(), q);
    expect(hydrated.baseline).toEqual({
      kind: "explicit_cohort",
      label: "Explicit cohort",
      cohort: baseline,
      sampleCount: undefined,
    });
  });
  it("round-trips the baseline rule and measured sample count with the baseline cohort", () => {
    const original = state();
    original.baseline = priorWindowBaseline(
      { ...SCOPE, from: "2026-05-01", to: "2026-05-07" },
      12
    );
    const q = encodeAnalysisQuery(analysisToUrlState(original));
    expect(q.base_kind).toBe("prior_window");
    expect(q.base_n).toBe("12");
    const hydrated = hydrateFromQuery(state(), q);
    expect(hydrated.baseline).toEqual(original.baseline);
  });
});

describe("1,800-character fallback", () => {
  it("inlines a small hash, requires save when oversized, never truncates", () => {
    const small = serializeAnalysisHash(["investigate"], state());
    expect(small.kind).toBe("inline");

    // A huge In filter blows past the budget.
    const huge = state();
    huge.scope = {
      ...huge.scope,
      filters: [{ op: "in", dimension: "session", values: Array.from({ length: 400 }, (_, i) => `session-${i}-${"x".repeat(10)}`) }],
    };
    const over = serializeAnalysisHash(["investigate"], huge);
    expect(over.kind).toBe("requires_save");
    if (over.kind === "requires_save") expect(over.reason).toContain(String(MAX_HASH_LEN));

    // With an investigation id the hash is compact regardless of state size.
    const byId = serializeAnalysisHash(["investigate"], huge, "inv-123");
    expect(byId.kind).toBe("inline");
    if (byId.kind === "inline") {
      expect(byId.hash).toContain("investigation=inv-123");
      expect(byId.hash.length).toBeLessThanOrEqual(MAX_HASH_LEN);
    }

    const timeline = serializeAnalysisHash(
      ["investigate"],
      state(),
      undefined,
      { mode: "timeline", group: "model" }
    );
    expect(timeline.kind).toBe("inline");
    if (timeline.kind === "inline") {
      expect(timeline.hash).toContain("mode=timeline");
      expect(timeline.hash).toContain("group=model");
    }

    const savedTimeline = serializeAnalysisHash(
      ["investigate"],
      huge,
      "inv-timeline",
      { mode: "timeline" }
    );
    expect(savedTimeline).toEqual({
      kind: "inline",
      hash: "#/investigate?investigation=inv-timeline&mode=timeline",
    });
  });
});

describe("baseline derivation", () => {
  it("prior_window is the preceding equal-length window, preserving non-date settings", () => {
    expect(priorWindow("2026-05-01", "2026-05-07")).toEqual({ from: "2026-04-24", to: "2026-04-30" });
    const b = priorWindowBaseline(SCOPE, 12)!;
    expect(b.cohort.from).toBe("2026-04-24");
    expect(b.cohort.to).toBe("2026-04-30");
    expect(b.cohort.filters).toEqual(SCOPE.filters); // non-date settings preserved
    expect(b.cohort.metric).toBe(SCOPE.metric);
    expect(b.label).toBe("Prior window · 12 runs"); // honest: rule + measured count
  });
  it("prior_window is null for an unbounded scope", () => {
    expect(priorWindowBaseline({ ...SCOPE, from: null, to: null })).toBeNull();
  });
  it("pinned_run narrows to a single run id", () => {
    const b = pinnedRunBaseline(SCOPE, "run-42");
    expect(b.cohort.filters).toEqual([{ op: "run_ids", ids: ["run-42"] }]);
    expect(b.label).toBe("Pinned run"); // no count yet -> rule only (never invents a number)
  });
  it("explicit_cohort is carried verbatim, not rewritten", () => {
    const custom = { ...SCOPE, from: "2020-01-01", to: "2020-12-31" };
    expect(explicitCohortBaseline(custom, 5).cohort).toEqual(custom);
  });
  it("baselineLabel states the rule alone until a count is measured", () => {
    expect(baselineLabel("prior_window")).toBe("Prior window");
    expect(baselineLabel("prior_window", 0)).toBe("Prior window · 0 runs");
  });
});
