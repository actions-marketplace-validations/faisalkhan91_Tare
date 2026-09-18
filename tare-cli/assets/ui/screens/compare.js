// Calibrated Compare: a fixed Baseline B and ordered run/cohort/version candidates whose summary
// matrix comes directly from the matched cohort API. The legacy N-run cause matrix, scatter, and
// parallel-coordinates evidence stay below the summary during compatibility; hypothetical scenarios
// remain in Optimize because they do not have an authoritative CohortSpec comparison contract.
import { el, clear } from "../ui/el.js";
import { errorNode } from "../ui/errorNode.js";
import { fmtSignedUsd, fmtUsd, toDollarString, fmtTokens } from "../ui/format.js";
import { routePath, currentQuery, setRouteQuery } from "../ui/store.js";
import { emptyState } from "../ui/empty.js";
import { lensSubtitle } from "../ui/lens.js";
import { scatter } from "../ui/scatter.js";
import { parcoords } from "../ui/parcoords.js";
import { costBandLegend } from "../ui/costLegend.js";
import { microsPerMtok } from "../ui/metrics.js";
import { flameDiffTree } from "../ui/flameDiffView.js";
import { explicitCohortBaseline, pinnedRunBaseline } from "../analysis/state.js";
/// Spend rows within epsilon of each other across all runs are "the same" — hidden by the
/// Differences-only toggle. $0.001 (1000 micro-USD) absorbs rounding without hiding real gaps.
const EPSILON_MICROS = 1000;
const counted = (count, singular, plural = `${singular}s`) => `${count} ${count === 1 ? singular : plural}`;
const COMPARE_MODES = ["runs", "cohorts", "versions", "scenarios"];
function defaultScope(query) {
    return {
        from: query.from ?? null,
        to: query.to ?? null,
        timezone: query.tz ?? "UTC",
        entity: "run",
        filters: [],
        pricing: { mode: "effective_dated" },
        metric: "spend_micros",
        normalization: "absolute",
        outcome_denominator: null,
    };
}
function replaceIdentityFilter(scope, filter, removeTemplate = false) {
    return {
        ...scope,
        filters: [
            ...scope.filters.filter((existing) => {
                if (existing.op === "run_ids" || existing.op === "step_refs")
                    return false;
                if (removeTemplate &&
                    (existing.op === "eq" || existing.op === "in") &&
                    existing.dimension === "template")
                    return false;
                return true;
            }),
            filter,
        ],
    };
}
function matchFromState(query, stored) {
    if (query.match === "aggregate_only")
        return { kind: "aggregate_only" };
    if (query.match === "workload_key") {
        return { kind: "workload_key", key: query.match_value ?? "" };
    }
    if (query.match === "template_lineage") {
        return { kind: "template_lineage", hash: query.match_value ?? "" };
    }
    return stored ?? { kind: "aggregate_only" };
}
function modeControl(mode) {
    const select = el("select", { "aria-label": "Compare mode" }, COMPARE_MODES.map((value) => el("option", {
        value,
        selected: value === mode,
        text: value.charAt(0).toUpperCase() + value.slice(1),
    })));
    select.addEventListener("change", () => setRouteQuery({ mode: select.value }));
    return el("label", { class: "compare-mode-control" }, [
        el("span", { text: "Compare mode" }),
        select,
    ]);
}
function formatPercent(value) {
    if (value === undefined || !Number.isFinite(value))
        return "Unavailable";
    return `${Number(value.toFixed(2))}%`;
}
function runCountLabel(value) {
    return `${value} ${value === 1 ? "run" : "runs"}`;
}
function compareScrollRegion(label, table) {
    return el("div", {
        class: "table-scroll compare-scroll-region",
        role: "region",
        tabindex: "0",
        "aria-label": label,
    }, [
        el("span", { class: "compare-scroll-cue caption sub", text: "Scroll for more columns →" }),
        table,
    ]);
}
function comparisonSummary(baseline, candidates, results, normalization) {
    const baselineResult = results[0]?.baseline;
    const candidateCells = (pick) => results.map(pick);
    const row = (label, baselineValue, values) => el("tr", { "data-metric": label.toLowerCase().replace(/ /g, "-") }, [
        el("th", { scope: "row", text: label }),
        el("td", { class: "num", text: baselineValue }),
        ...values.map((value) => el("td", { class: "num", text: value })),
    ]);
    const multipliers = candidateCells((result) => result.baseline.total_micros === 0
        ? "Unavailable"
        : `${(result.selection.total_micros / result.baseline.total_micros).toFixed(2)}×`);
    return el("section", {
        class: "section compare-summary",
        "data-normalization": normalization,
    }, [
        el("h2", { text: "Summary metric matrix" }),
        el("p", {
            class: "caption",
            text: "Estimated spend · absolute. Values come directly from the matched cohort comparison API; no workload matching or normalization is inferred.",
        }),
        compareScrollRegion("Comparison summary; scroll for candidate columns", el("table", { class: "data compare-summary-matrix" }, [
            el("thead", {}, [el("tr", {}, [
                    el("th", { text: "Metric" }),
                    el("th", { text: `Baseline B · ${baseline.label}` }),
                    ...candidates.map((candidate) => el("th", { text: `Candidate · ${candidate.label}` })),
                ])]),
            el("tbody", {}, [
                row("Sample count", baselineResult ? runCountLabel(baselineResult.run_count) : "Unavailable", candidateCells((result) => runCountLabel(result.selection.run_count))),
                row("Estimated spend", baselineResult ? fmtUsd(baselineResult.total_micros) : "Unavailable", candidateCells((result) => fmtUsd(result.selection.total_micros))),
                row("Absolute delta", "Reference", candidateCells((result) => fmtSignedUsd(result.total_delta_micros))),
                row("Percent delta", "Reference", candidateCells((result) => formatPercent(result.total_delta_pct))),
                row("Multiplier", "Reference", multipliers),
                row("Compatibility", "Reference", candidateCells((result) => result.compatibility_warnings.length === 0
                    ? "Compatible"
                    : `${result.compatibility_warnings.length} warning${result.compatibility_warnings.length === 1 ? "" : "s"}`)),
            ]),
        ])),
        ...results.flatMap((result, index) => result.compatibility_warnings.map((warning) => el("p", {
            class: "compare-warning trust-warn",
            text: `${candidates[index].label}: ${warning}`,
        }))),
    ]);
}
function comparedRuns(candidates, results) {
    const byId = new Map();
    const add = (runId, role) => {
        const existing = byId.get(runId);
        if (existing) {
            if (!existing.roles.includes(role))
                existing.roles.push(role);
        }
        else {
            byId.set(runId, { runId, roles: [role] });
        }
    };
    for (const runId of results[0]?.baseline.run_ids ?? [])
        add(runId, "Baseline B");
    results.forEach((result, index) => {
        for (const runId of result.selection.run_ids)
            add(runId, `Candidate · ${candidates[index].label}`);
    });
    return [...byId.values()];
}
function qualitySourceLabel(source) {
    if (source === "ci")
        return "CI";
    if (source === "header")
        return "capture header";
    if (source === "cli")
        return "CLI";
    if (source === "ui")
        return "UI";
    return source;
}
function capturedFrontierSection(candidates, results, frontier, optimizeHref) {
    const points = new Map(frontier?.points.map((point) => [point.run_id, point]) ?? []);
    const runs = comparedRuns(candidates, results);
    const rows = runs.map(({ runId, roles }) => {
        const point = points.get(runId);
        const quality = point?.quality == null
            ? "Unscored"
            : point.quality_source
                ? `${point.quality} · ${qualitySourceLabel(point.quality_source)} · user-supplied`
                : `${point.quality} · source unavailable · user-supplied`;
        return el("tr", { "data-frontier-run": runId }, [
            el("td", { text: roles.join(" · ") }),
            el("td", { class: "cause", text: runId }),
            el("td", {
                class: "num",
                text: point ? fmtUsd(point.cost_micros) : "Unavailable",
                title: point ? toDollarString(point.cost_micros) : undefined,
            }),
            el("td", { class: "num", text: point ? quality : "Unavailable" }),
            el("td", {
                text: point
                    ? point.quality == null && frontier?.has_quality
                        ? "Unscored · not quality-ranked"
                        : point.on_frontier ? "Frontier" : "Dominated"
                    : "Not returned · row retained",
            }),
        ]);
    });
    return el("section", { class: "section compare-frontier" }, [
        el("h2", { text: "Captured cost–quality evidence" }),
        el("p", {
            class: "caption",
            text: "One row per resolved run. Costs are whole-run estimates from the captured, whole-store frontier and may differ from the scoped matrix. Quality is shown exactly as user-supplied with its recorded source; Tare never averages, grades, infers, or normalizes it.",
        }),
        ...(rows.length === 0 ? [el("p", {
                class: "caption",
                text: "No resolved run rows are available. No cohort quality value was synthesized.",
            })] : [compareScrollRegion("Compared-run quality table", el("table", { class: "data compare-frontier-table" }, [
                el("thead", {}, [el("tr", {}, [
                        el("th", { text: "Comparison role" }),
                        el("th", { text: "Run" }),
                        el("th", { class: "num", text: "Estimated whole-run cost" }),
                        el("th", { class: "num", text: "Your quality · source" }),
                        el("th", { text: "Whole-store frontier state" }),
                    ])]),
                el("tbody", {}, rows),
            ]))]),
        el("div", { class: "compare-frontier-footer" }, [
            el("span", {
                class: "caption",
                text: frontier
                    ? `${frontier.estimated ? "Estimated pricing" : "Recorded pricing"} · ${frontier.pricing_version} · ${frontier.has_quality ? "user-supplied quality present" : "pure-cost frontier; quality unscored"}`
                    : "Captured frontier unavailable; resolved comparison rows remain visible.",
            }),
            el("a", {
                class: "btn compare-optimize-link",
                href: optimizeHref,
                text: "Explore scenarios in Optimize →",
            }),
        ]),
    ]);
}
function missingCount(total, missingPct) {
    if (!Number.isFinite(missingPct))
        return 0;
    return Math.max(0, Math.min(total, Math.round(total * missingPct / 100)));
}
function trialGroupingSection(baseline, candidates, results, facets) {
    const rows = [];
    facets.forEach((facet, index) => {
        const result = results[index];
        const candidate = candidates[index];
        if (!facet) {
            rows.push(el("tr", {}, [
                el("td", { text: candidate.label }),
                el("td", { text: "Grouping unavailable" }),
                el("td", { class: "num", text: "Unavailable" }),
                el("td", { class: "num", text: "Unavailable" }),
                el("td", { text: "Rows retained · retry grouping" }),
            ]));
            return;
        }
        if (facet.rows.length === 0) {
            const baselineEntities = baseline.cohort.entity === "step"
                ? result.baseline.step_count
                : result.baseline.run_count;
            const selectionEntities = candidate.cohort.entity === "step"
                ? result.selection.step_count
                : result.selection.run_count;
            rows.push(el("tr", {}, [
                el("td", { text: candidate.label }),
                el("td", { text: "No workload key captured" }),
                el("td", { class: "num", text: String(baselineEntities) }),
                el("td", { class: "num", text: String(selectionEntities) }),
                el("td", { text: "Unmatched · missing key" }),
            ]));
            return;
        }
        for (const row of facet.rows) {
            const status = row.selection_support > 0 && row.baseline_support > 0
                ? row.selection_support > 1 || row.baseline_support > 1
                    ? "Matched repeated trials"
                    : "Matched workload key"
                : row.selection_support > 0
                    ? "Candidate-only · unmatched"
                    : "Baseline-only · unmatched";
            rows.push(el("tr", {}, [
                el("td", { text: candidate.label }),
                el("td", { class: "cause", text: row.value }),
                el("td", { class: "num", text: String(row.baseline_support) }),
                el("td", { class: "num", text: String(row.selection_support) }),
                el("td", { text: status }),
            ]));
        }
        const first = facet.rows[0];
        const baselineEntities = baseline.cohort.entity === "step"
            ? result.baseline.step_count
            : result.baseline.run_count;
        const selectionEntities = candidate.cohort.entity === "step"
            ? result.selection.step_count
            : result.selection.run_count;
        const baselineMissing = missingCount(baselineEntities, first.baseline_missing_pct);
        const selectionMissing = missingCount(selectionEntities, first.selection_missing_pct);
        if (baselineMissing > 0 || selectionMissing > 0) {
            rows.push(el("tr", { class: "compare-unmatched-row" }, [
                el("td", { text: candidate.label }),
                el("td", { text: "Missing workload key" }),
                el("td", { class: "num", text: String(baselineMissing) }),
                el("td", { class: "num", text: String(selectionMissing) }),
                el("td", { text: "Unmatched · missing key" }),
            ]));
        }
    });
    return el("section", { class: "section compare-trial-groups" }, [
        el("h2", { text: "Repeated-trial workload groups" }),
        el("p", {
            class: "caption",
            text: "Entity support is grouped by captured workload key for each candidate versus Baseline B. Support can overlap when an entity carries multiple keys. Missing and one-sided keys stay visible as unmatched rows; counts do not normalize or invent quality.",
        }),
        compareScrollRegion("Workload match groups", el("table", { class: "data compare-trial-table" }, [
            el("thead", {}, [el("tr", {}, [
                    el("th", { text: "Candidate" }),
                    el("th", { text: "Workload / trial group" }),
                    el("th", { class: "num", text: "Baseline B support" }),
                    el("th", { class: "num", text: "Candidate support" }),
                    el("th", { text: "Match state" }),
                ])]),
            el("tbody", {}, rows),
        ])),
    ]);
}
function representativePairSection(candidates, results, query) {
    const baselineRuns = [...new Set(results.flatMap((result) => result.baseline.run_ids))];
    const candidateRoles = new Map();
    results.forEach((result, index) => {
        for (const runId of result.selection.run_ids) {
            const roles = candidateRoles.get(runId) ?? [];
            if (!roles.includes(candidates[index].label))
                roles.push(candidates[index].label);
            candidateRoles.set(runId, roles);
        }
    });
    const requestedBaseline = baselineRuns.includes(query.pair_baseline)
        ? query.pair_baseline
        : baselineRuns.length === 1 ? baselineRuns[0] : "";
    const requestedCandidate = candidateRoles.has(query.pair_candidate)
        ? query.pair_candidate
        : candidateRoles.size === 1 ? [...candidateRoles.keys()][0] : "";
    const baselineSelect = el("select", { "aria-label": "Baseline representative run" }, [
        el("option", { value: "", selected: requestedBaseline === "", text: "Choose Baseline B run" }),
        ...baselineRuns.map((runId) => el("option", {
            value: runId,
            selected: requestedBaseline === runId,
            text: runId,
        })),
    ]);
    const candidateSelect = el("select", { "aria-label": "Candidate representative run" }, [
        el("option", { value: "", selected: requestedCandidate === "", text: "Choose candidate run" }),
        ...[...candidateRoles].map(([runId, roles]) => el("option", {
            value: runId,
            selected: requestedCandidate === runId,
            text: `${roles.join(" / ")} · ${runId}`,
        })),
    ]);
    const update = () => setRouteQuery({
        pair_baseline: baselineSelect.value,
        pair_candidate: candidateSelect.value,
    });
    baselineSelect.addEventListener("change", update);
    candidateSelect.addEventListener("change", update);
    const ready = requestedBaseline !== "" && requestedCandidate !== "";
    return el("section", {
        class: "section compare-pair-picker",
        "data-pair-ready": String(ready),
        "data-pair-baseline": requestedBaseline,
        "data-pair-candidate": requestedCandidate,
    }, [
        el("h2", { text: "Representative run pair" }),
        el("p", {
            class: "caption",
            text: "Structural flame comparison is run-pair only. Multi-run cohorts require one explicit representative from each side; this picker never synthesizes an aggregate flame tree.",
        }),
        el("div", { class: "compare-pair-controls" }, [
            el("label", {}, [el("span", { text: "Baseline B run" }), baselineSelect]),
            el("span", { "aria-hidden": "true", text: "→" }),
            el("label", {}, [el("span", { text: "Candidate run" }), candidateSelect]),
        ]),
        el("p", {
            class: "caption compare-pair-status",
            role: "status",
            text: ready
                ? `Explicit pair: ${requestedBaseline} → ${requestedCandidate}`
                : "Choose one resolved run from each side before structural comparison is enabled.",
        }),
    ]);
}
/// Build the cause × run matrix from each run's `{cause -> micros}` breakdown. Causes are the
/// union across runs (missing = 0); rows are sorted by total spend across runs, descending.
export function compareMatrix(runIds, perRun) {
    const causes = new Set();
    for (const id of runIds)
        for (const c of Object.keys(perRun[id] ?? {}))
            causes.add(c);
    const rows = [...causes].map((cause) => {
        const values = runIds.map((id) => perRun[id]?.[cause] ?? 0);
        return { cause, values, total: values.reduce((s, v) => s + v, 0) };
    });
    rows.sort((a, b) => b.total - a.total || a.cause.localeCompare(b.cause));
    return rows;
}
/// True when every run's value for a row is within epsilon of the others (nothing to compare).
export function rowIsFlat(values) {
    if (values.length < 2)
        return true;
    return Math.max(...values) - Math.min(...values) <= EPSILON_MICROS;
}
/// Per-row diverging fill (diff.ts scale): cheapest cell teal, most-expensive red, saturation by
/// distance from the row midpoint. Flat rows render neutral.
export function cellFill(value, min, max) {
    if (max - min <= EPSILON_MICROS)
        return "transparent";
    const t = (value - min) / (max - min); // 0 = cheapest, 1 = most expensive
    if (t <= 0.5) {
        const sat = (0.5 - t) * 2;
        // Cheaper → cost-ok token, faded by saturation (theme-aware).
        return `color-mix(in srgb, var(--cost-ok) ${(0.15 + 0.55 * sat) * 100}%, transparent)`;
    }
    const sat = (t - 0.5) * 2;
    // Pricier → cost-high token, faded by saturation.
    return `color-mix(in srgb, var(--cost-high) ${(0.15 + 0.55 * sat) * 100}%, transparent)`;
}
export function decompositionParts(result) {
    const volume = result.volume_delta_micros;
    const size = result.size_delta_micros;
    const efficiency = result.efficiency_delta_micros;
    const total = result.total_delta_micros;
    const dir = Math.sign(total);
    const reversals = [];
    const check = (name, value) => {
        if (dir !== 0 && value !== 0 && Math.sign(value) !== dir)
            reversals.push(name);
    };
    check("volume", volume);
    check("size", size);
    check("efficiency", efficiency);
    return { volume, size, efficiency, total, sumsExactly: volume + size + efficiency === total, reversals };
}
export function causeDeltas(diff, limit = 6) {
    const nonzero = diff.rows.filter((r) => r.delta_micros !== 0);
    const increases = nonzero
        .filter((r) => r.delta_micros > 0)
        .sort((a, b) => b.delta_micros - a.delta_micros || a.cause.localeCompare(b.cause))
        .slice(0, limit);
    const decreases = nonzero
        .filter((r) => r.delta_micros < 0)
        .sort((a, b) => a.delta_micros - b.delta_micros || a.cause.localeCompare(b.cause))
        .slice(0, limit);
    const dir = Math.sign(diff.delta_micros);
    const reversals = dir === 0 ? [] : nonzero.filter((r) => Math.sign(r.delta_micros) !== dir);
    return { increases, decreases, reversals };
}
/// Human direction for a signed spend delta — units stay estimated spend; sign is never implicit.
function deltaDirection(value) {
    return value > 0 ? "costlier" : value < 0 ? "cheaper" : "no change";
}
const REFERENCE_MODES = ["baseline", "previous"];
function referenceControl(mode) {
    const select = el("select", { "aria-label": "Decompose relative to" }, REFERENCE_MODES.map((value) => el("option", {
        value,
        selected: value === mode,
        text: value === "baseline" ? "Baseline B" : "Previous candidate",
    })));
    select.addEventListener("change", () => setRouteQuery({ reference: select.value }));
    return el("label", { class: "compare-reference-control" }, [
        el("span", { text: "Decompose vs" }),
        select,
    ]);
}
function decompositionSection(candidates, results, reference, baselineLabel) {
    const blocks = candidates.map((candidate, index) => {
        const parts = decompositionParts(results[index]);
        const referenceLabel = reference === "previous"
            ? index === 0 ? `Baseline B · ${baselineLabel}` : `Previous · ${candidates[index - 1].label}`
            : `Baseline B · ${baselineLabel}`;
        const componentRow = (label, name, value) => el("tr", {
            "data-component": name,
            ...(parts.reversals.includes(name) ? { "data-reversal": "true", class: "compare-reversal" } : {}),
        }, [
            el("th", { scope: "row", text: label }),
            el("td", { class: "num dollars", text: fmtSignedUsd(value), title: toDollarString(value) }),
            el("td", { text: deltaDirection(value) }),
            el("td", { text: parts.reversals.includes(name) ? "↔ moves against the total" : "" }),
        ]);
        // The verification line proves the client-side invariant to the reader; a contract regression
        // (should never happen — Rust folds the remainder into efficiency) is surfaced loudly, not hidden.
        const verification = parts.sumsExactly
            ? el("p", { class: "caption compare-decomp-check", "data-sums-exactly": "true",
                text: `Volume + size + efficiency = ${fmtSignedUsd(parts.total)} — exactly the total delta.` })
            : el("p", { class: "trust-warn compare-decomp-check", "data-sums-exactly": "false",
                text: `Components (${fmtSignedUsd(parts.volume + parts.size + parts.efficiency)}) do not sum to the reported total (${fmtSignedUsd(parts.total)}); showing both — do not trust this decomposition.` });
        return el("div", { class: "compare-decomp-block", "data-candidate": candidate.id }, [
            el("h3", { text: `${candidate.label} vs ${referenceLabel}` }),
            compareScrollRegion(`Cost decomposition for ${candidate.label}`, el("table", { class: "data compare-decomp-table" }, [
                el("thead", {}, [el("tr", {}, [
                        el("th", { text: "Component" }),
                        el("th", { class: "num", text: "Δ estimated spend" }),
                        el("th", { text: "Direction" }),
                        el("th", { text: "Note" }),
                    ])]),
                el("tbody", {}, [
                    componentRow("Volume", "volume", parts.volume),
                    componentRow("Size", "size", parts.size),
                    componentRow("Efficiency", "efficiency", parts.efficiency),
                ]),
                el("tfoot", {}, [el("tr", { "data-metric": "total" }, [
                        el("th", { scope: "row", text: "Total delta" }),
                        el("td", { class: "num dollars", text: fmtSignedUsd(parts.total), title: toDollarString(parts.total) }),
                        el("td", { text: deltaDirection(parts.total) }),
                        el("td", { text: "" }),
                    ])]),
            ])),
            verification,
        ]);
    });
    return el("section", { class: "section compare-decomposition" }, [
        el("h2", { text: "Volume / size / efficiency decomposition" }),
        el("p", {
            class: "caption",
            text: "Deterministic cohort arithmetic — a DIFFERENT lens from the cause report diff below. Volume is the entity-count change at baseline size/mix, size is the tokens-per-entity change at baseline price/mix, and efficiency is the residual model/cache/unit-price effect. The three sum exactly to the total delta; units are estimated spend and every sign is explicit.",
        }),
        ...blocks,
    ]);
}
// Structural flame diff: callable ONLY for an explicit single-run pair. When no pair
// is resolved (multi-run cohorts without an explicit representative choice), it renders nothing — the
// representative-pair picker above already prompts the user; we never synthesize an aggregate tree.
async function flameDiffSection(client, pairBaseline, pairCandidate, opts) {
    if (!pairBaseline || !pairCandidate)
        return "";
    let model;
    try {
        model = await client.flameDiff(pairBaseline, pairCandidate, opts.mode === "normalized");
    }
    catch (error) {
        return errorNode("Couldn't load the structural flame diff for the chosen pair.", error);
    }
    return flameDiffTree(model, opts);
}
async function causeDeltaSection(client, pairBaseline, pairCandidate) {
    if (!pairBaseline || !pairCandidate)
        return "";
    let diff;
    try {
        diff = await client.diff(pairBaseline, pairCandidate);
    }
    catch (error) {
        return errorNode("Couldn't load the cause report diff for the chosen representative pair.", error);
    }
    const view = causeDeltas(diff);
    const rankList = (title, rows, kind) => el("div", { class: "compare-cause-rank", "data-rank": kind }, [
        el("h3", { text: title }),
        rows.length === 0
            ? el("p", { class: "caption", text: "None." })
            : el("ol", { class: "compare-cause-list" }, rows.map((row) => {
                const reversal = view.reversals.includes(row);
                return el("li", { ...(reversal ? { class: "compare-reversal", "data-reversal": "true" } : {}) }, [
                    el("span", { class: "cause", text: row.cause }),
                    el("span", { class: "num dollars", text: fmtSignedUsd(row.delta_micros), title: toDollarString(row.delta_micros) }),
                    el("span", { class: "sub", text: `${fmtUsd(row.micros_before)} → ${fmtUsd(row.micros_after)} · ${deltaDirection(row.delta_micros)}${reversal ? " · ↔ against the run total" : ""}` }),
                ]);
            })),
    ]);
    return el("section", { class: "section compare-cause-diff", "data-pair": `${pairBaseline}→${pairCandidate}` }, [
        el("h2", { text: "Cause report diff" }),
        el("p", {
            class: "caption",
            text: `Report-level per-cause deltas for the representative pair ${pairBaseline} → ${pairCandidate} (estimated spend). This is the existing cause/report diff — kept DISTINCT from the cohort decomposition above; it explains which named causes moved, not the volume/size/efficiency split. Run total ${fmtSignedUsd(diff.delta_micros)} (${deltaDirection(diff.delta_micros)}).`,
        }),
        el("div", { class: "compare-cause-cols" }, [
            rankList("Largest increases", view.increases, "increase"),
            rankList("Largest decreases", view.decreases, "decrease"),
        ]),
        ...(view.reversals.length > 0
            ? [el("p", { class: "caption compare-cause-reversal-note",
                    text: `${view.reversals.length} cause${view.reversals.length === 1 ? "" : "s"} moved against the run's overall direction (marked ↔) — a countervailing change, not the headline.` })]
            : []),
    ]);
}
export async function renderCompare(root, client, routeOrParam, context) {
    const route = typeof routeOrParam === "object" ? routeOrParam : undefined;
    const query = route?.query ?? currentQuery();
    const state = context?.analysis.get();
    const scope = state?.scope ?? defaultScope(query);
    const requestedMode = query.mode;
    const mode = requestedMode && COMPARE_MODES.includes(requestedMode)
        ? requestedMode
        : "runs";
    const modeBar = modeControl(mode);
    if (mode === "scenarios") {
        root.replaceChildren(modeBar, lensSubtitle("Scenarios are evaluated in Optimize, where assumptions and counterfactual provenance stay visible."), el("section", { class: "section compare-scenario-handoff" }, [
            el("h2", { text: "Scenarios are evaluated in Optimize" }),
            el("p", {
                text: "The cohort comparison API has no authoritative CohortSpec contract for hypothetical scenario cells. No comparison values were synthesized.",
            }),
            el("a", { href: routePath(["optimize"]), text: "Open Optimize →" }),
        ]));
        return;
    }
    let baseline;
    let candidates = [];
    let runIds = [];
    let orderKey;
    if (mode === "runs") {
        runIds = (query.runs ?? "")
            .split(",")
            .map((value) => value.trim())
            .filter(Boolean);
        if (runIds.length === 0 && state?.baseline?.kind === "pinned_run") {
            const baselineRun = state.baseline.cohort.filters.find((f) => f.op === "run_ids");
            const id = baselineRun?.op === "run_ids" ? baselineRun.ids[0] : undefined;
            if (id)
                runIds = [id, ...state.comparison.filter((ref) => ref.kind === "run").map((ref) => ref.id)];
        }
        orderKey = "runs";
        if (runIds.length >= 2) {
            baseline = {
                id: runIds[0],
                label: runIds[0],
                cohort: replaceIdentityFilter(scope, { op: "run_ids", ids: [runIds[0]] }),
            };
            candidates = runIds.slice(1).map((id) => ({
                id,
                label: id,
                cohort: replaceIdentityFilter(scope, { op: "run_ids", ids: [id] }),
            }));
        }
    }
    else if (mode === "versions") {
        let versions = (query.versions ?? "")
            .split(",")
            .map((value) => value.trim())
            .filter(Boolean);
        if (versions.length === 0) {
            const baselineTemplate = state?.baseline?.cohort.filters.find((filter) => filter.op === "eq" && filter.dimension === "template");
            const baselineId = baselineTemplate?.op === "eq" ? baselineTemplate.value : undefined;
            const durableCandidates = state?.comparison
                .filter((ref) => ref.kind === "template")
                .map((ref) => ref.id) ?? [];
            versions = baselineId ? [baselineId, ...durableCandidates] : [];
        }
        orderKey = "versions";
        if (versions.length >= 2) {
            baseline = {
                id: versions[0],
                label: versions[0],
                cohort: replaceIdentityFilter(scope, { op: "eq", dimension: "template", value: versions[0] }, true),
            };
            candidates = versions.slice(1).map((id) => ({
                id,
                label: id,
                cohort: replaceIdentityFilter(scope, { op: "eq", dimension: "template", value: id }, true),
            }));
        }
    }
    else if (state?.baseline) {
        const selection = state.selection ?? state.scope;
        baseline = {
            id: state.baseline.label,
            label: state.baseline.label,
            cohort: state.baseline.cohort,
        };
        candidates = [{ id: "selection-a", label: "Selection A", cohort: selection }];
    }
    if (!baseline || candidates.length === 0) {
        const title = mode === "cohorts"
            ? "Set Baseline B to compare cohorts"
            : mode === "versions"
                ? "Pick at least two template versions"
                : "Pick at least two runs to compare";
        const detail = mode === "cohorts"
            ? "Investigate keeps Selection A (or the whole scope) fixed against your durable Baseline B."
            : mode === "versions"
                ? "Add ordered template hashes to Compare; the first stays fixed as Baseline B."
                : "In Investigate, select at least two runs, then choose 'Compare selected'.";
        root.replaceChildren(modeBar, emptyState(title, detail, mode === "runs"
            ? { actionLabel: "Choose runs in Investigate →", actionHref: routePath(["investigate"], { entity: "runs" }) }
            : undefined));
        return;
    }
    const match = matchFromState(query, state?.match);
    const matchSelect = el("select", { "aria-label": "Match rule" }, [
        el("option", { value: "aggregate_only", selected: match.kind === "aggregate_only", text: "Aggregate only" }),
        el("option", { value: "workload_key", selected: match.kind === "workload_key", text: "Workload key" }),
        el("option", { value: "template_lineage", selected: match.kind === "template_lineage", text: "Template lineage" }),
    ]);
    const matchValue = el("input", {
        "aria-label": match.kind === "template_lineage" ? "Template lineage hash" : "Workload key",
        value: match.kind === "workload_key" ? match.key : match.kind === "template_lineage" ? match.hash : "",
        placeholder: match.kind === "template_lineage" ? "Template hash" : "Workload key",
    });
    const updateMatch = (kind, value) => {
        const next = kind === "workload_key"
            ? { kind: "workload_key", key: value }
            : kind === "template_lineage"
                ? { kind: "template_lineage", hash: value }
                : { kind: "aggregate_only" };
        context?.analysis.set({ match: next });
        setRouteQuery({ match: next.kind, match_value: next.kind === "aggregate_only" ? "" : value });
    };
    matchSelect.addEventListener("change", () => updateMatch(matchSelect.value, matchValue.value.trim()));
    matchValue.addEventListener("change", () => updateMatch(matchSelect.value, matchValue.value.trim()));
    const matchControls = el("div", { class: "compare-match-controls" }, [
        el("label", {}, [el("span", { text: "Match rule" }), matchSelect]),
        ...(match.kind === "aggregate_only" ? [] : [matchValue]),
    ]);
    root.replaceChildren(el("p", {
        class: "skeleton",
        text: `Comparing Baseline B with ${candidates.length} candidate${candidates.length === 1 ? "" : "s"}…`,
    }));
    let results;
    try {
        results = await Promise.all(candidates.map(async (candidate) => (await client.compareCohort({ selection: candidate.cohort, baseline: baseline.cohort, match })).data));
    }
    catch (error) {
        root.replaceChildren(modeBar, errorNode("Couldn't build the matched comparison. Adjust the cohort or match rule if it keeps failing.", error, {
            actions: [
                { label: "Retry", primary: true, run: () => renderCompare(root, client, route, context) },
                { label: "Back to Investigate", href: "#/investigate" },
            ],
        }));
        return;
    }
    // Decomposition reference: "baseline" decomposes every candidate against the fixed
    // Baseline B (reusing `results`); "previous" decomposes each candidate against the one before it
    // (candidate[0] still vs Baseline B) — a sequential/chained view. The summary matrix above stays a
    // fixed-Baseline-B contract; only the decomposition honors this toggle, so its per-candidate
    // baselines are naturally expressed and nothing is silently re-based.
    const reference = query.reference === "previous" ? "previous" : "baseline";
    let decompositionResults = results;
    let decompositionError = null;
    if (reference === "previous" && candidates.length > 1) {
        try {
            decompositionResults = await Promise.all(candidates.map(async (candidate, index) => {
                const baseCohort = index === 0 ? baseline.cohort : candidates[index - 1].cohort;
                return (await client.compareCohort({ selection: candidate.cohort, baseline: baseCohort, match })).data;
            }));
        }
        catch (error) {
            decompositionError = error;
            decompositionResults = results; // fall back to the baseline-relative view, surfaced below
        }
    }
    // Captured quality and workload grouping are evidence companions to the authoritative comparison
    // result. A failure never drops a resolved run or fabricates a replacement value: the sections
    // below retain explicit unavailable/unmatched rows.
    const evidence = await Promise.allSettled([
        client.frontier(),
        ...candidates.map((candidate) => client.facetCohort({
            selection: candidate.cohort,
            baseline: baseline.cohort,
            dimension: "workload_key",
        })),
    ]);
    const frontier = evidence[0]?.status === "fulfilled" ? evidence[0].value : null;
    const trialFacets = candidates.map((_, index) => {
        const result = evidence[index + 1];
        return result?.status === "fulfilled"
            ? result.value.data
            : null;
    });
    const baselineRunCount = results[0]?.baseline.run_count;
    if (mode === "runs") {
        context?.analysis.setBaseline(pinnedRunBaseline(scope, baseline.id, baselineRunCount));
        context?.analysis.setComparison(candidates.map((candidate) => ({
            kind: "run", id: candidate.id, label: candidate.label,
        })));
    }
    else if (mode === "versions") {
        context?.analysis.setBaseline(explicitCohortBaseline(baseline.cohort, baselineRunCount));
        context?.analysis.setComparison(candidates.map((candidate) => ({
            kind: "template", id: candidate.id, label: candidate.label,
        })));
    }
    const setCandidateOrder = (next) => {
        if (!orderKey)
            return;
        setRouteQuery({ [orderKey]: [baseline.id, ...next.map((candidate) => candidate.id)].join(",") });
        const kind = mode === "versions" ? "template" : "run";
        context?.analysis.setComparison(next.map((candidate) => ({
            kind, id: candidate.id, label: candidate.label,
        })));
    };
    const baselineChipLabel = mode === "runs"
        ? `Pinned run · ${runCountLabel(baselineRunCount ?? 0)} · ${baseline.label}`
        : mode === "versions"
            ? `Explicit cohort · ${runCountLabel(baselineRunCount ?? 0)} · ${baseline.label}`
            : baseline.label.includes("runs")
                ? baseline.label
                : `${baseline.label} · ${runCountLabel(baselineRunCount ?? 0)}`;
    const chips = el("div", { class: "compare-chips" }, [
        el("span", { class: "compare-chip baseline chip", text: `Baseline B · ${baselineChipLabel} · fixed` }),
        ...candidates.map((candidate, index) => el("span", { class: "compare-chip candidate chip" }, [
            el("span", { text: `Candidate · ${candidate.label} · ${runCountLabel(results[index].selection.run_count)}` }),
            ...(orderKey ? [
                el("button", {
                    type: "button",
                    "aria-label": `Move candidate ${candidate.label} left`,
                    disabled: index === 0,
                    onClick: () => {
                        if (index === 0)
                            return;
                        const next = [...candidates];
                        [next[index - 1], next[index]] = [next[index], next[index - 1]];
                        setCandidateOrder(next);
                    },
                }, ["←"]),
                el("button", {
                    type: "button",
                    "aria-label": `Move candidate ${candidate.label} right`,
                    disabled: index === candidates.length - 1,
                    onClick: () => {
                        if (index === candidates.length - 1)
                            return;
                        const next = [...candidates];
                        [next[index], next[index + 1]] = [next[index + 1], next[index]];
                        setCandidateOrder(next);
                    },
                }, ["→"]),
                el("button", {
                    type: "button",
                    "aria-label": `Remove candidate ${candidate.label}`,
                    onClick: () => setCandidateOrder(candidates.filter((_, candidateIndex) => candidateIndex !== index)),
                }, ["×"]),
            ] : []),
        ])),
    ]);
    const normalizationLabel = scope.normalization.replace(/_/g, " ");
    const metricLabel = scope.metric.replace(/_/g, " ");
    const basisDisclosure = scope.metric !== "spend_micros" || scope.normalization !== "absolute"
        ? el("p", {
            class: "compare-basis-warning trust-warn",
            text: `The active ${metricLabel} / ${normalizationLabel} was not applied to this matrix. It remains Estimated spend · absolute; change views explicitly instead of silently normalizing.`,
        })
        : el("p", {
            class: "caption",
            text: "The active view already uses estimated spend / absolute; no additional normalization was applied.",
        });
    const summary = comparisonSummary(baseline, candidates, results, scope.normalization);
    const pairPicker = representativePairSection(candidates, results, query);
    const trialGroups = trialGroupingSection(baseline, candidates, results, trialFacets);
    // the two DISTINCT explanatory lenses. The cohort decomposition (volume/size/
    // efficiency, summing exactly to the total delta) is aggregate arithmetic over the whole matched
    // cohort; the cause report diff is the existing per-cause ReportDiff for a single representative run
    // pair. They are never merged. The decomposition honors the baseline/previous reference toggle.
    // On a previous-mode fetch failure we fell back to the baseline-relative results, so label them as
    // Baseline B (matching the data shown), and let `decompositionNote` explain why previous was dropped.
    const decomposition = decompositionSection(candidates, decompositionResults, decompositionError ? "baseline" : reference, baseline.label);
    const decompositionNote = decompositionError
        ? errorNode("Couldn't decompose against the previous candidate; showing the Baseline B decomposition instead.", decompositionError)
        : null;
    const baselineRunsForPair = [...new Set(results.flatMap((result) => result.baseline.run_ids))];
    const candidateRunsForPair = [...new Set(results.flatMap((result) => result.selection.run_ids))];
    const pairBaseline = baselineRunsForPair.includes(query.pair_baseline)
        ? query.pair_baseline
        : baselineRunsForPair.length === 1 ? baselineRunsForPair[0] : "";
    const pairCandidate = candidateRunsForPair.includes(query.pair_candidate)
        ? query.pair_candidate
        : candidateRunsForPair.length === 1 ? candidateRunsForPair[0] : "";
    const causeDiff = await causeDeltaSection(client, pairBaseline, pairCandidate);
    // Structural flame diff: view state from the query so it deep-links deterministically.
    const flameOpts = {
        mode: query.flame_mode === "normalized" ? "normalized" : "absolute",
        diffOnly: query.flame_diff_only === "1",
        focusPath: (query.flame_focus ?? "")
            .split(".")
            .filter((s) => s !== "")
            .map((s) => Number(s))
            .filter((n) => Number.isInteger(n) && n >= 0),
    };
    const flameDiff = await flameDiffSection(client, pairBaseline, pairCandidate, flameOpts);
    const optimizeQuery = { ...query, view: "scenarios" };
    for (const key of [
        "mode", "runs", "versions", "match", "match_value", "pair_baseline", "pair_candidate",
    ])
        delete optimizeQuery[key];
    const frontierSection = capturedFrontierSection(candidates, results, frontier, routePath(["optimize"], optimizeQuery));
    const comparisonHeader = [
        modeBar,
        chips,
        matchControls,
        referenceControl(reference),
        basisDisclosure,
        summary,
        decomposition,
        decompositionNote,
        trialGroups,
        pairPicker,
        causeDiff,
        flameDiff,
        frontierSection,
    ].filter((node) => node !== null && node !== "");
    if (mode !== "runs") {
        root.replaceChildren(lensSubtitle(`${counted(candidates.length, `${mode === "cohorts" ? "cohort" : "version"} candidate`)} compared against fixed Baseline B.`), ...comparisonHeader);
        return;
    }
    // Each run's per-cause breakdown via a self-diff (before == after == that run's spend). The diff
    // endpoint already builds a one-run report internally, so this needs no new endpoint.
    let perRun;
    try {
        const breakdowns = await Promise.all(runIds.map(async (id) => {
            const d = await client.diff(id, id);
            const m = {};
            for (const r of d.rows)
                m[r.cause] = r.micros_before;
            return [id, m];
        }));
        perRun = Object.fromEntries(breakdowns);
    }
    catch (e) {
        root.replaceChildren(lensSubtitle(`${counted(runIds.length, "run")} compared side by side: where their costs diverge.`), ...comparisonHeader, errorNode("The matched summary is available, but the legacy cause evidence couldn't load.", e));
        return;
    }
    const matrix = compareMatrix(runIds, perRun);
    let diffOnly = false;
    const table = el("div", {
        class: "table-scroll compare-scroll-region",
        role: "region",
        tabindex: "0",
        "aria-label": "Per-cause comparison; scroll for run columns",
    }, [el("span", { class: "compare-scroll-cue caption sub", text: "Scroll for more run columns →" })]);
    const render = () => {
        const rows = diffOnly ? matrix.filter((r) => !rowIsFlat(r.values)) : matrix;
        const head = el("tr", {}, [
            el("th", { text: "Cause" }),
            ...runIds.map((id) => el("th", {}, [el("a", { href: routePath(["investigate", "run", id]), text: id, class: "sub" })])),
        ]);
        const body = rows.map((r) => {
            const min = Math.min(...r.values);
            const max = Math.max(...r.values);
            return el("tr", {}, [
                el("td", { class: "cause", text: r.cause }),
                ...r.values.map((v) => {
                    const td = el("td", { class: "dollars num", text: fmtUsd(v), title: toDollarString(v) });
                    td.style.background = cellFill(v, min, max);
                    return td;
                }),
            ]);
        });
        clear(table);
        if (rows.length === 0) {
            table.appendChild(el("p", { class: "empty", text: "No differing causes. These runs spent identically." }));
        }
        else {
            table.appendChild(el("table", { class: "data compare-matrix" }, [el("thead", {}, [head]), el("tbody", {}, body)]));
        }
    };
    const toggle = el("input", { type: "checkbox", "aria-label": "Differences only" });
    toggle.addEventListener("change", () => {
        diffOnly = toggle.checked;
        render();
    });
    render();
    // Cost-vs-tokens scatter: each run by tokens (x) vs estimated spend (y), colored by
    // blended $/1M tokens. Runs high for their token count pay more per token — the efficiency frontier
    // made visual. Best-effort (needs per-run tokens); omitted if unavailable.
    let scatterSection = "";
    try {
        const statuses = await Promise.all(runIds.map((id) => client.runStatus(id)));
        const withTok = statuses.filter((s) => (s.tokens ?? 0) > 0 && s.micros > 0);
        if (withTok.length >= 2) {
            const minRate = Math.min(...withTok.map((s) => microsPerMtok(s.micros, s.tokens ?? 0)));
            const points = withTok.map((s) => {
                const rate = microsPerMtok(s.micros, s.tokens ?? 0);
                const tone = rate > minRate * 1.5 ? "cost-high" : rate > minRate * 1.15 ? "cost-warn" : "cost-ok";
                return {
                    x: s.tokens ?? 0,
                    y: s.micros,
                    tone,
                    label: `${s.run_id}: ${fmtTokens(s.tokens ?? 0)} tokens, ${fmtUsd(s.micros)} (${fmtUsd(rate)}/1M tokens)`,
                };
            });
            scatterSection = el("section", { class: "section" }, [
                el("h2", {}, ["Cost vs tokens"]),
                el("p", {
                    class: "caption",
                    text: "Each run by tokens (x) and estimated spend (y), colored by blended $/1M tokens (green cheapest). A point high for its token count pays more per token. This makes the cost-efficiency frontier visible.",
                }),
                // y is micro-USD, so it MUST carry formatY: without it the compact tick formatter turned a
                // $3.40 run into "3.4M" on an axis labelled "Spend" — a wrong number, and
                // the same defect already fixed on the Experiments frontier's x axis.
                scatter(points, {
                    formatY: (m) => fmtUsd(m),
                    xLabel: "Tokens →",
                    yLabel: "Spend (USD) ↑",
                    ariaLabel: "Cost versus tokens scatter, one point per run",
                }),
            ]);
        }
    }
    catch {
        /* scatter is best-effort — the matrix is the primary view */
    }
    // Parallel-coordinates across cost dimensions: reveals "expensive runs have high
    // cache-miss AND high output share" at a glance. Derives 5 dims per run from its steps.
    let parcoordsSection = "";
    try {
        const AXES = ["tokens", "output %", "cache-miss %", "$/1M tokens", "spend"];
        const perRunSteps = await Promise.all(runIds.map((id) => client.runSteps(id)));
        const rows = runIds
            .map((id, i) => {
            const steps = perRunSteps[i];
            const tokens = steps.reduce((a, s) => a + s.tokens, 0);
            const micros = steps.reduce((a, s) => a + s.micros, 0);
            const output = steps.reduce((a, s) => a + s.output, 0);
            const fresh = steps.reduce((a, s) => a + s.fresh_input, 0);
            const cacheRead = steps.reduce((a, s) => a + s.cache_read, 0);
            const inputSeen = fresh + cacheRead;
            return {
                id,
                tokens,
                micros,
                values: [
                    tokens,
                    tokens > 0 ? (output / tokens) * 100 : 0,
                    inputSeen > 0 ? (fresh / inputSeen) * 100 : 0,
                    microsPerMtok(micros, tokens),
                    micros,
                ],
            };
        })
            .filter((r) => r.tokens > 0);
        if (rows.length >= 2) {
            const maxSpend = Math.max(...rows.map((r) => r.micros));
            const lines = rows.map((r) => {
                const share = maxSpend > 0 ? r.micros / maxSpend : 0;
                const tone = share >= 0.66 ? "cost-high" : share >= 0.33 ? "cost-warn" : "cost-ok";
                return { label: `${r.id}: ${fmtUsd(r.micros)}, ${fmtTokens(r.tokens)} tok`, values: r.values, tone };
            });
            parcoordsSection = el("section", { class: "section" }, [
                el("h2", {}, ["Cost dimensions"]),
                el("p", {
                    class: "caption",
                    text: "Each run as a line across cost dimensions (each axis scaled to this run set), colored by total spend. Lines high on several axes at once, such as cache-miss and output share, are the expensive shape. Hover to isolate a run.",
                }),
                parcoords(AXES, lines, {
                    ariaLabel: "Parallel coordinates across cost dimensions, one line per run",
                    // Per-axis min/max ticks in each axis's own unit. Axis order: tokens, output %,
                    // cache-miss %, $/1M tok, spend.
                    axisFormat: (i, v) => i === 0 ? fmtTokens(v) : i === 1 || i === 2 ? `${Math.round(v)}%` : fmtUsd(v),
                }),
                costBandLegend("total spend"), // "colored by spend" now states which end is expensive
            ]);
        }
    }
    catch {
        /* parcoords is best-effort — the matrix is the primary view */
    }
    root.replaceChildren(lensSubtitle(`${counted(runIds.length, "run")} compared side by side: where their costs diverge.`), ...comparisonHeader, el("section", { class: "section" }, [
        el("h2", {}, [`Compare ${counted(runIds.length, "run")}`]),
        el("p", { class: "caption", text: "Per-cause spend across the selected runs (estimated). Each row is colored from cheapest to most expensive; pick the run that spends least on a cause." }),
        el("label", { class: "diff-controls" }, [toggle, el("span", { text: " Differences only" })]),
        table,
    ]), scatterSection, parcoordsSection);
}
