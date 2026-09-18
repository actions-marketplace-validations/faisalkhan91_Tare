// Cross-language parity for the canonical CohortSpec. These fixtures and
// their expected canonical JSON + hash are ASSERTED IDENTICALLY in the Rust golden
// (`tare_core::cohort::tests::golden_canonical_and_hash_for_parity`). If the two ever diverge, one
// side's canonicalization drifted.
import { describe, it, expect } from "vitest";
import { canonicalCohortJson, cohortHash, fnv1a64 } from "../src/analysis/serialize.js";
import type { CohortSpec } from "../src/analysis/types.js";

// The exact spec the Rust golden builds.
const GOLDEN_SPEC: CohortSpec = {
  from: "2026-05-01",
  to: null,
  timezone: "America/Los_Angeles",
  entity: "step",
  filters: [
    { op: "in", dimension: "model", values: ["claude-sonnet-4-6", "claude-opus-4-8"] },
    { op: "eq", dimension: "provider", value: "anthropic" },
    { op: "gte_micros", value: 1000 },
  ],
  pricing: { mode: "effective_dated" },
  metric: "spend_micros",
  normalization: "per_run",
  outcome_denominator: null,
};

const GOLDEN_CANONICAL =
  '{"entity":"step","filters":[{"dimension":"provider","op":"eq","value":"anthropic"},{"op":"gte_micros","value":1000},{"dimension":"model","op":"in","values":["claude-opus-4-8","claude-sonnet-4-6"]}],"from":"2026-05-01","metric":"spend_micros","normalization":"per_run","outcome_denominator":null,"pricing":{"mode":"effective_dated"},"timezone":"America/Los_Angeles","to":null}';
const GOLDEN_HASH = "f07f3446ba18d3c7";

describe("canonical CohortSpec Rust↔TypeScript parity", () => {
  it("produces the byte-identical canonical JSON and hash the Rust golden asserts", () => {
    expect(canonicalCohortJson(GOLDEN_SPEC)).toBe(GOLDEN_CANONICAL);
    expect(cohortHash(GOLDEN_SPEC)).toBe(GOLDEN_HASH);
  });

  it("is invariant to filter order and In-value order/duplicates", () => {
    const a: CohortSpec = {
      ...GOLDEN_SPEC,
      filters: [
        { op: "in", dimension: "model", values: ["b", "a", "a"] },
        { op: "eq", dimension: "provider", value: "anthropic" },
        { op: "eq", dimension: "provider", value: "anthropic" },
      ],
    };
    const b: CohortSpec = {
      ...GOLDEN_SPEC,
      filters: [
        { op: "eq", dimension: "provider", value: "anthropic" },
        { op: "in", dimension: "model", values: ["a", "b"] },
      ],
    };
    expect(canonicalCohortJson(a)).toBe(canonicalCohortJson(b));
    expect(cohortHash(a)).toBe(cohortHash(b));
    // A different value changes the hash.
    const c: CohortSpec = { ...b, filters: [...b.filters, { op: "tag", value: "anomaly" }] };
    expect(cohortHash(c)).not.toBe(cohortHash(b));
  });

  it("sorts + dedupes step_refs by (run_id, step_ordinal)", () => {
    const spec: CohortSpec = {
      ...GOLDEN_SPEC,
      filters: [
        {
          op: "step_refs",
          refs: [
            { run_id: "r2", step_ordinal: 1 },
            { run_id: "r1", step_ordinal: 2 },
            { run_id: "r1", step_ordinal: 2 },
          ],
        },
      ],
    };
    expect(canonicalCohortJson(spec)).toContain(
      '"refs":[{"run_id":"r1","step_ordinal":2},{"run_id":"r2","step_ordinal":1}]',
    );
  });

  it("fnv1a64 matches the FNV offset basis for empty input", () => {
    expect(fnv1a64(new Uint8Array())).toBe(0xcbf29ce484222325n);
  });
});
