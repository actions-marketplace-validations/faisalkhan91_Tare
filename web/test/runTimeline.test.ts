import { describe, expect, it } from "vitest";
import type { RunStep } from "../src/client.js";
import { buildRunTimeline, formatTimelineNanos, relatedSteps } from "../src/workspaces/runTimeline.js";

function step(ordinal: number, timing: Partial<RunStep> = {}): RunStep {
  return {
    ordinal,
    provider: "anthropic",
    model: "model",
    fresh_input: 10,
    cache_read: 0,
    cache_write: 0,
    output: 2,
    reasoning: 0,
    tokens: 12,
    micros: 1_000,
    stop_reason: null,
    ...timing,
  };
}

describe("Run Profile timestamp evidence", () => {
  it("keeps legacy, malformed, and end-before-start rows in captured Step order", () => {
    const model = buildRunTimeline([
      step(3),
      step(1, { start_unix_nano: "not-a-decimal", end_unix_nano: "20" }),
      step(2, { start_unix_nano: "30", end_unix_nano: "20" }),
    ]);
    expect(model.mode).toBe("step_order");
    expect(model.orderedSteps.map((row) => row.ordinal)).toEqual([1, 2, 3]);
    expect(model.overlapCount).toBe(0);
    expect(model.concurrentCount).toBe(0);
  });

  it("subtracts huge decimal-string epochs in BigInt and retains partial timing coverage", () => {
    const base = 18_446_744_073_709_551_615n;
    const model = buildRunTimeline([
      step(3),
      step(2, { start_unix_nano: String(base + 300_000_000n), end_unix_nano: String(base + 500_000_000n) }),
      step(1, { start_unix_nano: String(base), end_unix_nano: String(base + 100_000_000n) }),
    ]);
    expect(model.mode).toBe("timeline");
    expect(model.timedCount).toBe(2);
    expect(model.totalCount).toBe(3);
    expect(model.duration).toBe(500_000_000n);
    expect(model.orderedSteps.map((row) => row.ordinal)).toEqual([1, 2, 3]);
    expect(model.timedByOrdinal.get(2)?.leftPct).toBe(60);
    expect(model.timedByOrdinal.get(2)?.widthPct).toBe(40);
  });

  it("distinguishes timestamp overlap, nested spans, and relationship-backed sibling concurrency", () => {
    const common = { trace_id: "trace", parent_span_id: "root" };
    const model = buildRunTimeline([
      step(1, { ...common, span_id: "a", start_unix_nano: "100", end_unix_nano: "300" }),
      step(2, { ...common, span_id: "b", start_unix_nano: "200", end_unix_nano: "400" }),
      step(3, { trace_id: "trace", span_id: "child", parent_span_id: "a", start_unix_nano: "220", end_unix_nano: "250" }),
      step(4, { trace_id: "other", span_id: "x", start_unix_nano: "230", end_unix_nano: "240" }),
    ]);
    expect(model.overlapCount).toBe(6);
    expect(model.concurrentCount).toBe(1);
    expect(model.nestedCount).toBe(1);
    expect(relatedSteps(model, 2, "concurrent")).toEqual([1]);
  });

  it("does not count adjacent spans or zero-duration timestamp instants as overlap", () => {
    const model = buildRunTimeline([
      step(1, { start_unix_nano: "100", end_unix_nano: "200" }),
      step(2, { start_unix_nano: "200", end_unix_nano: "300" }),
      step(3, { start_unix_nano: "150", end_unix_nano: "150" }),
    ]);
    expect(model.overlapCount).toBe(0);
  });

  it("formats elapsed evidence without converting epoch nanoseconds to Number", () => {
    expect(formatTimelineNanos(0n)).toBe("0 ms");
    expect(formatTimelineNanos(500_000n)).toBe("<1 ms");
    expect(formatTimelineNanos(12_300_000n)).toBe("12.3 ms");
    expect(formatTimelineNanos(2_500_000_000n)).toBe("2.5 s");
    expect(formatTimelineNanos(125_000_000_000n)).toBe("2 min 5 s");
  });
});
