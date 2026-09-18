import type {
  AlertRule,
  Anomaly,
  BurnRate,
  PeriodBudget,
  TodaySpend,
} from "./client.js";
import { fmtUsd } from "./ui/format.js";

export interface AlertInputs {
  today: TodaySpend;
  budget: PeriodBudget;
  burnrate?: BurnRate;
  anomalies: Anomaly[];
  anomaliesByWindow?: Record<string, Anomaly[]>;
  capturedEvents: number;
  day: string;
}

export interface AlertNotice {
  key: string;
  message: string;
  level: "warn" | "over" | "anomaly";
}

function dollarsToMicros(value: number | undefined): number | null {
  if (value == null || !Number.isFinite(value) || value < 0) return null;
  const micros = Math.round(value * 1_000_000);
  return Number.isSafeInteger(micros) ? micros : null;
}

function normalizedKind(value: string): string {
  return value.toLowerCase().replace(/[^a-z]/g, "");
}

function ruleIdentity(rule: AlertRule): string {
  return [
    rule.metric,
    rule.threshold ?? "",
    rule.kind ?? "",
    rule.window_days ?? "",
    rule.min_events ?? "",
  ].join(":");
}

function anomalyNotices(
  anomalies: Anomaly[],
  prefix: string,
  messagePrefix: string,
  includeMinor: boolean
): AlertNotice[] {
  return anomalies
    .filter((anomaly) => includeMinor || anomaly.materiality !== "minor")
    .map((anomaly) => ({
      key: `${prefix}:${anomaly.date}:${anomaly.series_key}:${normalizedKind(anomaly.kind)}`,
      message: `${messagePrefix}${anomaly.series_key} on ${anomaly.date}`,
      level: "anomaly" as const,
    }));
}

/** Evaluate built-in and configured local alerts against one monitor snapshot. */
export function evaluateAlertNotices(rules: AlertRule[], input: AlertInputs): AlertNotice[] {
  const notices: AlertNotice[] = [];

  // Built-in period-budget crossing. At 100% the stronger notice subsumes the configured warning.
  if (input.budget.cap_micros > 0 && input.budget.pct >= 100) {
    notices.push({
      key: `builtin:budget:over:${input.day}:${input.budget.period}:${input.budget.cap_micros}`,
      message: `${input.budget.period} budget reached ${input.budget.pct}% (${fmtUsd(input.budget.spent_micros)})`,
      level: "over",
    });
  } else if (
    input.budget.cap_micros > 0 &&
    input.budget.pct >= input.budget.warn_pct
  ) {
    notices.push({
      key: `builtin:budget:warn:${input.day}:${input.budget.period}:${input.budget.cap_micros}`,
      message: `${input.budget.period} budget reached ${input.budget.pct}% (${fmtUsd(input.budget.spent_micros)})`,
      level: "warn",
    });
  }

  // Match the daemon's noise policy: built-in alerts skip minor anomalies. A configured anomaly
  // rule can opt into a particular kind, including minor occurrences.
  notices.push(...anomalyNotices(input.anomalies, "builtin:anomaly", "Spend anomaly: ", false));

  rules.forEach((rule) => {
    if (rule.min_events != null && input.capturedEvents < rule.min_events) return;
    const identity = ruleIdentity(rule);
    if (rule.metric === "today_spend") {
      const threshold = dollarsToMicros(rule.threshold);
      if (threshold != null && input.today.total_micros >= threshold) {
        notices.push({
          key: `rule:${identity}:${input.day}`,
          message: `Today spend ${fmtUsd(input.today.total_micros)} reached your ${fmtUsd(threshold)} alert`,
          level: "warn",
        });
      }
      return;
    }
    if (rule.metric === "period_pct") {
      if (rule.threshold != null && input.budget.pct >= rule.threshold) {
        notices.push({
          key: `rule:${identity}:${input.day}`,
          message: `${input.budget.period} budget reached ${input.budget.pct}% (custom ${rule.threshold}% alert)`,
          level: input.budget.pct >= 100 ? "over" : "warn",
        });
      }
      return;
    }
    if (rule.metric === "run_rate") {
      const threshold = dollarsToMicros(rule.threshold);
      const rate = input.burnrate?.run_rate_micros_per_day;
      if (threshold != null && rate != null && rate >= threshold) {
        notices.push({
          key: `rule:${identity}:${input.day}`,
          message: `Run rate ${fmtUsd(rate)}/day reached your ${fmtUsd(threshold)}/day alert`,
          level: "warn",
        });
      }
      return;
    }
    if (rule.metric === "anomaly_kind") {
      const wanted = normalizedKind(rule.kind ?? "any");
      const source =
        rule.window_days == null
          ? input.anomalies
          : (input.anomaliesByWindow?.[String(rule.window_days)] ?? []);
      const matching = source.filter(
        (anomaly) => wanted === "any" || normalizedKind(anomaly.kind) === wanted
      );
      notices.push(
        ...anomalyNotices(matching, `rule:${identity}`, "Configured anomaly: ", true)
      );
    }
  });

  return notices;
}

/** Return only never-seen notice keys and keep a bounded newest-first persistence list. */
export function selectNewAlertNotices(
  notices: AlertNotice[],
  seenKeys: readonly string[],
  maxKeys = 256
): { fresh: AlertNotice[]; seenKeys: string[] } {
  const seen = new Set(seenKeys);
  const fresh: AlertNotice[] = [];
  for (const notice of notices) {
    if (seen.has(notice.key)) continue;
    seen.add(notice.key);
    fresh.push(notice);
  }
  const next = [...fresh.map((notice) => notice.key), ...seenKeys.filter((key) => seen.has(key))];
  return { fresh, seenKeys: [...new Set(next)].slice(0, Math.max(1, maxKeys)) };
}
