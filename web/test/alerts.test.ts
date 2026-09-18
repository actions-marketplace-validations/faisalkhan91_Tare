import { describe, expect, it } from "vitest";
import { evaluateAlertNotices, selectNewAlertNotices, type AlertInputs } from "../src/alerts.js";

const input = (overrides: Partial<AlertInputs> = {}): AlertInputs => ({
  today: { run_count: 2, total_micros: 4_000_000, pricing_version: "p", effective_date: "d" },
  budget: {
    period: "month",
    spent_micros: 8_000_000,
    cap_micros: 10_000_000,
    warn_pct: 80,
    pct: 80,
    status: "warn",
  },
  burnrate: {
    run_rate_micros_per_day: 2_000_000,
    effective_rate_micros_per_day: 2_000_000,
    spent_micros: 2_000_000,
    active_days: 1,
    daily_spend_micros: [2_000_000],
    days_elapsed: 1,
    days_in_period: 30,
    projected_micros: 60_000_000,
    projected_low_micros: 0,
    projected_high_micros: 0,
    cap_micros: 10_000_000,
    on_track: false,
    headroom_days: 1,
    period: "month",
    period_start: "2026-09-01",
    as_of: "2026-09-01",
    period_end: "2026-09-30",
  },
  anomalies: [],
  capturedEvents: 5,
  day: "2026-09-17",
  ...overrides,
});

describe("local alert evaluation", () => {
  it("emits the built-in budget warning and upgrades it at 100 percent", () => {
    expect(evaluateAlertNotices([], input())[0].level).toBe("warn");
    const over = input({ budget: { ...input().budget, pct: 110, status: "over" } });
    const notices = evaluateAlertNotices([], over);
    expect(notices).toHaveLength(1);
    expect(notices[0].level).toBe("over");
  });

  it("uses the configured periodic-budget warning threshold", () => {
    const budget = { ...input().budget, warn_pct: 90, pct: 89, status: "ok" };
    expect(evaluateAlertNotices([], input({ budget }))).toEqual([]);
    expect(
      evaluateAlertNotices([], input({ budget: { ...budget, pct: 90, status: "warn" } }))
    ).toHaveLength(1);
  });

  it("evaluates dollar, percentage, rate, event-floor, and anomaly-kind rules", () => {
    const anomaly = {
      date: "2026-09-17",
      series_key: "anthropic/m",
      kind: "NewSeries",
      value_micros: 1,
      baseline_micros: 0,
      materiality: "minor",
    };
    const notices = evaluateAlertNotices(
      [
        { metric: "today_spend", threshold: 3 },
        { metric: "period_pct", threshold: 75 },
        { metric: "run_rate", threshold: 1.5 },
        { metric: "today_spend", threshold: 1, min_events: 10 },
        { metric: "anomaly_kind", kind: "new_series" },
      ],
      input({ anomalies: [anomaly] })
    );
    // Built-in minor anomaly is suppressed; the configured kind still opts into it.
    expect(notices.filter((notice) => notice.key.startsWith("rule:"))).toHaveLength(4);
    expect(notices.some((notice) => notice.message.includes("Configured anomaly"))).toBe(true);
  });

  it("honors an anomaly rule's requested window", () => {
    const anomaly = {
      date: "2026-09-17",
      series_key: "total",
      kind: "Spike",
      value_micros: 2,
      baseline_micros: 1,
      materiality: "minor",
    };
    const notices = evaluateAlertNotices(
      [{ metric: "anomaly_kind", kind: "spike", window_days: 14 }],
      input({ anomaliesByWindow: { "14": [anomaly] } })
    );
    expect(notices.some((notice) => notice.message.includes("Configured anomaly"))).toBe(true);
  });

  it("deduplicates repeated polls and bounds persistent history", () => {
    const notices = evaluateAlertNotices([], input());
    const first = selectNewAlertNotices(notices, [], 2);
    expect(first.fresh).toHaveLength(1);
    expect(selectNewAlertNotices(notices, first.seenKeys, 2).fresh).toHaveLength(0);
    const later = selectNewAlertNotices(
      [
        { key: "b", message: "b", level: "warn" },
        { key: "c", message: "c", level: "warn" },
      ],
      first.seenKeys,
      2
    );
    expect(later.seenKeys).toEqual(["b", "c"]);
  });

  it("keeps configured-rule identities stable when rules are reordered", () => {
    const rules = [
      { metric: "today_spend", threshold: 1 },
      { metric: "period_pct", threshold: 70 },
    ];
    const first = evaluateAlertNotices(rules, input())
      .filter((notice) => notice.key.startsWith("rule:"))
      .map((notice) => notice.key)
      .sort();
    const reordered = evaluateAlertNotices([...rules].reverse(), input())
      .filter((notice) => notice.key.startsWith("rule:"))
      .map((notice) => notice.key)
      .sort();
    expect(reordered).toEqual(first);
  });
});
