// Applied-change cohort verification. The stored action
// determines cohort/baseline/match semantics; SavingsVerifyResult supplies measured spends, counts,
// completeness, and outcome. Wording never upgrades an aggregate association into causal savings.

import { el } from "../ui/el.js";
import { fmtSignedUsd, fmtUsd, humanizeKey, toDollarString } from "../ui/format.js";
import { addDays } from "../ui/range.js";
import type { SavingsAction, SavingsVerifyResult } from "../analysis/types.js";

export type VerificationLifecycleState =
  | "verifying"
  | "observed_reduction"
  | "not_observed";

interface VerificationWindows {
  intervention: string;
  beforeFrom: string;
  beforeTo: string;
  afterFrom: string;
  afterTo: string;
  usedDatePrefixFallback: boolean;
}

function localCalendarDate(instant: string, timezone: string): {
  date: string;
  usedDatePrefixFallback: boolean;
} {
  try {
    const parsed = new Date(instant);
    if (!Number.isFinite(parsed.getTime())) throw new Error("invalid instant");
    const parts = new Intl.DateTimeFormat("en-US", {
      timeZone: timezone,
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
    }).formatToParts(parsed);
    const value = (kind: Intl.DateTimeFormatPartTypes): string | undefined =>
      parts.find((part) => part.type === kind)?.value;
    const year = value("year");
    const month = value("month");
    const day = value("day");
    if (!year || !month || !day) throw new Error("missing date part");
    return { date: `${year}-${month}-${day}`, usedDatePrefixFallback: false };
  } catch {
    const fallback = /^\d{4}-\d{2}-\d{2}/.exec(instant)?.[0] ?? "unavailable";
    return { date: fallback, usedDatePrefixFallback: true };
  }
}

/// Mirror the verifier's normative default: seven equal local-calendar-day windows around acted_at,
/// excluding the intervention day. Exported so DST/timezone behavior is locked by focused tests.
export function verificationWindows(action: SavingsAction): VerificationWindows {
  const local = localCalendarDate(action.acted_at, action.cohort.timezone);
  if (local.date === "unavailable") {
    return {
      intervention: local.date,
      beforeFrom: local.date,
      beforeTo: local.date,
      afterFrom: local.date,
      afterTo: local.date,
      usedDatePrefixFallback: true,
    };
  }
  return {
    intervention: local.date,
    beforeFrom: addDays(local.date, -7),
    beforeTo: addDays(local.date, -1),
    afterFrom: addDays(local.date, 1),
    afterTo: addDays(local.date, 7),
    usedDatePrefixFallback: local.usedDatePrefixFallback,
  };
}

/// The server contract derives status from completeness + the measured result. Re-derive it at the
/// presentation boundary so an incomplete response can never be shown as an observed outcome.
export function verificationLifecycleState(
  result: SavingsVerifyResult
): VerificationLifecycleState {
  if (!result.complete) return "verifying";
  return result.observed_reduction_micros > 0
    ? "observed_reduction"
    : "not_observed";
}

/// Formula consistency gates whether a response may contribute an observed outcome. A missing
/// stored-baseline side or arithmetic mismatch remains visible in the panel but is not promoted.
export function verificationFormulaMatches(
  action: SavingsAction,
  result: SavingsVerifyResult
): boolean {
  let computed: number;
  if (action.baseline) {
    if (
      result.baseline_before_micros == null ||
      result.baseline_after_micros == null
    ) {
      return false;
    }
    computed =
      (result.selection_before_micros - result.selection_after_micros) +
      (result.baseline_after_micros - result.baseline_before_micros);
  } else {
    computed = result.selection_before_micros - result.selection_after_micros;
  }
  return computed === result.observed_reduction_micros;
}

export function verificationPresentationState(
  action: SavingsAction,
  result: SavingsVerifyResult
): VerificationLifecycleState | "applied" {
  return verificationFormulaMatches(action, result)
    ? verificationLifecycleState(result)
    : "applied";
}

function matchRuleText(action: SavingsAction): string {
  switch (action.match.kind) {
    case "aggregate_only":
      return "Aggregate only · units are not like-for-like matched";
    case "workload_key":
      return `Workload key${action.match.key ? ` · ${action.match.key}` : ""}`;
    case "template_lineage":
      return `Template lineage · ${action.match.hash}`;
  }
}

function outcomeText(action: SavingsAction): string {
  const denominator = action.outcome_denominator;
  if (!denominator) return "None stored; no causal outcome-denominator claim";
  return denominator.kind === "work_unit"
    ? `Work unit · ${denominator.name}`
    : `Metered · ${humanizeKey(denominator.metered)}`;
}

function countText(
  matched: number | undefined,
  unmatched: number | undefined,
  aggregateOnly: boolean
): string {
  if (matched == null || unmatched == null) return "Match/exclusion counts unavailable";
  if (aggregateOnly) {
    return `${matched.toLocaleString("en-US")} like-for-like matched · ${unmatched.toLocaleString("en-US")} unmatched; spend remains full-cohort aggregate`;
  }
  return `${matched.toLocaleString("en-US")} matched · ${unmatched.toLocaleString("en-US")} excluded`;
}

function windowCard(
  title: string,
  period: string,
  micros: number | undefined,
  matched: number | undefined,
  unmatched: number | undefined,
  aggregateOnly: boolean
): HTMLElement {
  return el("section", { class: "optimize-verification-window" }, [
    el("h5", { text: title }),
    el("p", { class: "caption", text: period }),
    el("p", {
      class: micros == null ? "optimize-verification-spend" : "num optimize-verification-spend",
      text: micros == null ? "Spend unavailable from verifier" : fmtUsd(micros),
      ...(micros == null ? {} : { title: toDollarString(micros) }),
    }),
    el("p", {
      class: "caption optimize-verification-counts",
      text: countText(matched, unmatched, aggregateOnly),
    }),
  ]);
}

function formulaDetails(
  action: SavingsAction,
  result: SavingsVerifyResult
): { node: HTMLElement; computed?: number } {
  const hasStoredBaseline = action.baseline != null;
  const bb = result.baseline_before_micros;
  const ba = result.baseline_after_micros;
  if (hasStoredBaseline && (bb == null || ba == null)) {
    return {
      node: el("section", { class: "optimize-verification-formula" }, [
        el("h5", { text: "Adjusted observed reduction formula" }),
        el("p", {
          class: "optimize-row-warning",
          text: "The stored action has a baseline, but baseline spend is missing from the verifier. No adjusted-result formula can be verified.",
        }),
      ]),
    };
  }

  const computed = hasStoredBaseline
    ? (result.selection_before_micros - result.selection_after_micros) +
      (ba! - bb!)
    : result.selection_before_micros - result.selection_after_micros;
  const label = hasStoredBaseline
    ? "Adjusted observed reduction formula"
    : "Unadjusted observed change formula";
  const expression = hasStoredBaseline
    ? `(${fmtUsd(result.selection_before_micros)} − ${fmtUsd(result.selection_after_micros)}) + (${fmtUsd(ba!)} − ${fmtUsd(bb!)}) = ${fmtSignedUsd(computed)}`
    : `${fmtUsd(result.selection_before_micros)} − ${fmtUsd(result.selection_after_micros)} = ${fmtSignedUsd(computed)}`;
  const section = el("section", { class: "optimize-verification-formula" }, [
    el("h5", { text: label }),
    el("p", { class: "num", text: expression }),
    el("p", {
      class: "caption",
      text: hasStoredBaseline
        ? "Difference-in-differences: selection change plus the baseline's opposite change."
        : "No baseline was stored, so this is an unadjusted before/after association and may reflect an overall trend.",
    }),
  ]);
  if (computed !== result.observed_reduction_micros) {
    section.appendChild(
      el("p", {
        class: "optimize-row-warning",
        text: `Verifier contract mismatch: the response reports ${fmtSignedUsd(result.observed_reduction_micros)}, but its window values compute ${fmtSignedUsd(computed)}. Do not use this outcome until the response is corrected.`,
      })
    );
  }
  return { node: section, computed };
}

function statusStatement(
  action: SavingsAction,
  result: SavingsVerifyResult,
  state: VerificationLifecycleState,
  formulaMatches: boolean
): HTMLElement {
  const aggregateOnly = action.match.kind === "aggregate_only";
  if (!formulaMatches) {
    return el("p", {
      class: "optimize-verification-outcome optimize-row-warning",
      text: "Verification response is internally inconsistent. No observed outcome is claimed.",
    });
  }
  if (state === "verifying") {
    return el("p", {
      class: "optimize-verification-outcome",
      text: "Verifying · The after window is incomplete in captured data. No observed outcome is claimed yet.",
    });
  }
  if (state === "not_observed") {
    return el("p", {
      class: "optimize-verification-outcome",
      text: `Not observed · No positive reduction was measured (${fmtSignedUsd(result.observed_reduction_micros)}). This does not prove the change had no effect.`,
    });
  }
  return el("p", {
    class: "optimize-verification-outcome",
    text: aggregateOnly
      ? `Observed association · ${fmtUsd(result.observed_reduction_micros)} lower under aggregate-only before/after arithmetic. Units were not matched; this is not a causal saving.`
      : `Observed reduction · ${fmtUsd(result.observed_reduction_micros)} in the stored matched cohort. This measurement does not by itself establish causality.`,
  });
}

export function renderSavingsVerification(
  action: SavingsAction,
  result: SavingsVerifyResult
): HTMLElement {
  const windows = verificationWindows(action);
  const state = verificationLifecycleState(result);
  const aggregateOnly = action.match.kind === "aggregate_only";
  const formula = formulaDetails(action, result);
  const formulaMatches = verificationFormulaMatches(action, result);
  const panel = el("section", {
    class: "optimize-verification",
    "aria-label": "Applied-change cohort verification",
  }, [
    el("div", { class: "optimize-verification-head" }, [
      el("div", {}, [
        el("p", { class: "eyebrow", text: "Stored action · measured separately from expected savings" }),
        el("h4", { text: "Applied-change verification" }),
      ]),
      el("span", {
        class: `optimize-verification-completeness ${result.complete ? "is-complete" : "is-incomplete"}`,
        text: result.complete ? "Complete window" : "Incomplete window",
      }),
    ]),
    statusStatement(action, result, state, formulaMatches),
    el("dl", { class: "optimize-verification-basis" }, [
      el("div", {}, [el("dt", { text: "Intervention day" }), el("dd", { text: `${windows.intervention} · excluded · ${action.cohort.timezone}` })]),
      el("div", {}, [el("dt", { text: "Window rule" }), el("dd", { text: "7 equal local-calendar days before and after" })]),
      el("div", {}, [el("dt", { text: "Match rule" }), el("dd", { text: matchRuleText(action) })]),
      el("div", {}, [el("dt", { text: "Outcome denominator" }), el("dd", { text: outcomeText(action) })]),
      el("div", {}, [
        el("dt", { text: "Quality guardrail" }),
        el("dd", {
          text: action.quality_guardrail == null
            ? "Not stored; no quality pass/fail claim"
            : `${action.quality_guardrail} stored threshold; verifier returns no quality measurement, so pass/fail is unavailable`,
        }),
      ]),
    ]),
    el("div", { class: "optimize-verification-windows" }, [
      windowCard(
        "Selection · before",
        `${windows.beforeFrom}–${windows.beforeTo}`,
        result.selection_before_micros,
        result.matched_before,
        result.unmatched_before,
        aggregateOnly
      ),
      windowCard(
        "Selection · after",
        `${windows.afterFrom}–${windows.afterTo}`,
        result.selection_after_micros,
        result.matched_after,
        result.unmatched_after,
        aggregateOnly
      ),
    ]),
  ]);

  if (action.baseline) {
    panel.appendChild(
      el("div", { class: "optimize-verification-baseline-head" }, [
        el("h5", { text: `Baseline · ${action.baseline.label}` }),
        el("p", {
          class: "caption",
          text: `${humanizeKey(action.baseline.kind)}${action.baseline.sample_count == null ? "" : ` · ${action.baseline.sample_count} snapshot samples`}`,
        }),
      ])
    );
    panel.appendChild(
      el("div", { class: "optimize-verification-windows" }, [
        windowCard(
          "Baseline · before",
          `${windows.beforeFrom}–${windows.beforeTo}`,
          result.baseline_before_micros,
          result.baseline_matched_before,
          result.baseline_unmatched_before,
          aggregateOnly
        ),
        windowCard(
          "Baseline · after",
          `${windows.afterFrom}–${windows.afterTo}`,
          result.baseline_after_micros,
          result.baseline_matched_after,
          result.baseline_unmatched_after,
          aggregateOnly
        ),
      ])
    );
  } else {
    panel.appendChild(
      el("p", {
        class: "optimize-row-warning",
        text: "No baseline was stored. The result is an unadjusted selection association and may reflect an overall trend.",
      })
    );
  }
  panel.appendChild(formula.node);

  if (windows.usedDatePrefixFallback) {
    panel.appendChild(
      el("p", {
        class: "optimize-row-warning",
        text: "The cohort-local intervention date could not be formatted in this runtime; window labels use the stored UTC date prefix as a compatibility fallback.",
      })
    );
  }
  const derivedStatus = verificationLifecycleState(result);
  if (result.status !== derivedStatus) {
    panel.appendChild(
      el("p", {
        class: "optimize-row-warning",
        text: `Verifier status mismatch: response said ${humanizeKey(result.status)}, while completeness and measured change require ${humanizeKey(derivedStatus)}. The required derived state is shown.`,
      })
    );
  }
  const warnings = new Set([
    ...action.compatibility_warnings,
    ...result.compatibility_warnings,
  ]);
  for (const warning of warnings) {
    panel.appendChild(el("p", { class: "optimize-row-warning", text: warning }));
  }
  return panel;
}
