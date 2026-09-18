// Optimize scenarios: one local-first workbench over the existing
// cache advice, model what-if, captured cost/quality frontier, and scoped CostExperiment API.
// Quick advice/frontier endpoints are compatibility-era whole-store views and are labelled as such;
// the experiment builder is the authoritative scoped path. Every trial reprices stored counts only:
// it never executes a model, reads payload text, or predicts counterfactual quality.
import { el } from "../ui/el.js";
import { emptyState } from "../ui/empty.js";
import { errorNode } from "../ui/errorNode.js";
import { fmtSignedUsd, fmtTokens, fmtUsd, humanizeKey, toDollarString, } from "../ui/format.js";
import { routePath } from "../ui/store.js";
import { initialAnalysisState } from "../analysis/store.js";
const AS_CAPTURED = "*as-captured*";
function valueOf(result) {
    return result.status === "fulfilled" ? result.value : null;
}
function cohortPeriod(cohort) {
    if (cohort.from && cohort.to) {
        return cohort.from === cohort.to ? cohort.from : `${cohort.from}–${cohort.to}`;
    }
    if (cohort.from)
        return `from ${cohort.from}`;
    if (cohort.to)
        return `through ${cohort.to}`;
    return "all captured dates";
}
function canonicalValues(raw) {
    return [...new Set(raw.split(/[\n,]/).map((value) => value.trim()).filter(Boolean))]
        .filter((value) => value !== AS_CAPTURED)
        .sort((a, b) => a.localeCompare(b));
}
function addCandidate(input, value) {
    const values = canonicalValues(`${input.value},${value}`);
    input.value = values.join(", ");
    input.focus();
}
function lifecycleHref(route) {
    return routePath(route.segments, {
        ...(route.query ?? {}),
        type: "",
        view: "open",
    });
}
function scenarioContext(cohort, source, resolved) {
    const section = el("section", {
        class: "scenario-provenance",
        "aria-labelledby": "scenario-provenance-h",
    });
    section.append(el("div", { class: "scenario-section-head" }, [
        el("div", {}, [
            el("p", { class: "eyebrow", text: "Experiment input" }),
            el("h2", { id: "scenario-provenance-h", class: "subhead", text: `${source} provenance` }),
        ]),
        el("span", {
            class: "scenario-scope-badge",
            text: `${cohort.entity} · ${cohort.filters.length} filter${cohort.filters.length === 1 ? "" : "s"}`,
        }),
    ]), el("p", {
        class: "caption scenario-scope-line",
        text: `${cohortPeriod(cohort)} · ${cohort.timezone} · ${cohort.metric} / ${cohort.normalization}`,
    }));
    if (!resolved) {
        section.appendChild(el("p", {
            class: "optimize-row-warning",
            text: "Cohort provenance is temporarily unavailable. Scenario execution still validates the cohort server-side and will fail visibly rather than guessing.",
        }));
        return section;
    }
    const { data, provenance } = resolved;
    const priced = provenance.priced_token_share_pct == null
        ? "priced-token share unknown"
        : `${Number(provenance.priced_token_share_pct.toFixed(1))}% of tokens priced`;
    section.append(el("dl", { class: "scenario-provenance-grid" }, [
        el("div", {}, [el("dt", { text: "Resolved input" }), el("dd", { text: `${data.run_count} runs · ${data.step_count} steps` })]),
        el("div", {}, [el("dt", { text: "Captured spend" }), el("dd", { class: "num", text: fmtUsd(data.total_micros), title: toDollarString(data.total_micros) })]),
        el("div", {}, [el("dt", { text: "Coverage" }), el("dd", { text: `${humanizeKey(provenance.coverage_status)} · ${priced}` })]),
        el("div", {}, [el("dt", { text: "Fidelity" }), el("dd", { text: humanizeKey(provenance.component_fidelity) })]),
        el("div", {}, [el("dt", { text: "Capture sources" }), el("dd", { text: provenance.capture_sources.join(", ") || "None reported" })]),
        el("div", {}, [el("dt", { text: "Value basis" }), el("dd", { text: `${humanizeKey(provenance.value_class)} · ${humanizeKey(provenance.allocation_method)}` })]),
    ]), el("p", {
        class: "caption",
        text: `Pricing ${provenance.pricing_edition.version} · effective ${provenance.pricing_edition.effective_date} · ${humanizeKey(provenance.pricing_edition.mode)} mode · refreshed ${provenance.refreshed_at}`,
    }));
    if (provenance.assumptions.length > 0) {
        section.appendChild(el("ul", { class: "scenario-assumptions", "aria-label": "Scenario input assumptions" }, provenance.assumptions.map((assumption) => el("li", { class: "caption", text: assumption }))));
    }
    return section;
}
function renderCacheAdvice(host, advice, decache) {
    const section = el("section", { class: "scenario-source", "aria-labelledby": "scenario-cache-h" }, [
        el("div", { class: "scenario-section-head" }, [
            el("div", {}, [
                el("p", { class: "eyebrow", text: "Quick input · compatibility view" }),
                el("h2", { id: "scenario-cache-h", class: "subhead", text: "Prompt-cache advice" }),
            ]),
            el("button", {
                type: "button",
                class: "btn",
                text: "Use cache axis",
                "data-scenario-use-cache": "true",
                onClick: () => {
                    decache.checked = true;
                    decache.focus();
                },
            }),
        ]),
        el("p", {
            class: "caption",
            text: "Whole captured store, not silently narrowed to the active Selection. Advice estimates stable-prefix caching; the cache axis below runs the scoped counts-only counterfactual.",
        }),
    ]);
    if (advice === null) {
        section.appendChild(errorNode("Prompt-cache advice is temporarily unavailable.", "advice unavailable"));
    }
    else if (advice.length === 0) {
        section.appendChild(el("p", { class: "caption", text: "No stable repeated prefix currently meets the advice detector's threshold." }));
    }
    else {
        const rows = [...advice]
            .sort((a, b) => Math.max(b.save_5m_micros, b.save_1h_micros) - Math.max(a.save_5m_micros, a.save_1h_micros)
            || `${a.provider ?? ""}/${a.model}`.localeCompare(`${b.provider ?? ""}/${b.model}`))
            .slice(0, 5);
        section.appendChild(el("table", { class: "data scenario-compact-table" }, [
            el("thead", {}, [el("tr", {}, [
                    el("th", { text: "Model" }),
                    el("th", { class: "num", text: "Prefix" }),
                    el("th", { class: "num", text: "5m estimate" }),
                    el("th", { text: "Advice" }),
                ])]),
            el("tbody", {}, rows.map((row) => el("tr", {}, [
                el("td", { text: row.provider ? `${row.provider}/${row.model}` : row.model }),
                el("td", { class: "num", text: fmtTokens(row.system_tokens) }),
                el("td", { class: "num", text: fmtUsd(row.save_5m_micros), title: toDollarString(row.save_5m_micros) }),
                el("td", { text: row.recommend }),
            ]))),
        ]));
    }
    host.replaceChildren(section);
}
function renderWhatIfBody(host, result, modelInput) {
    if (result === null) {
        host.replaceChildren(errorNode("Model what-if is temporarily unavailable.", "what-if unavailable"));
        return;
    }
    if (result.recommendations.length === 0) {
        host.replaceChildren(el("p", { class: "caption", text: "No priced alternative model is available for this compatibility view." }));
        return;
    }
    const rows = [...result.recommendations]
        .sort((a, b) => a.delta_micros - b.delta_micros || a.to_model.localeCompare(b.to_model))
        .slice(0, 6);
    host.replaceChildren(el("p", { class: "caption", text: `Whole-store baseline ${fmtUsd(result.baseline_micros)} · every model swap is an approximate tokenizer-preserving reprice.` }), el("table", { class: "data scenario-compact-table" }, [
        el("thead", {}, [el("tr", {}, [
                el("th", { text: "Candidate" }),
                el("th", { class: "num", text: "Estimated change" }),
                el("th", { text: "Compatibility" }),
                el("th", {}, [el("span", { class: "sr-only", text: "Action" })]),
            ])]),
        el("tbody", {}, rows.map((row) => el("tr", {}, [
            el("td", { text: `${row.to_provider}/${row.to_model}` }),
            el("td", { class: `num ${row.delta_micros < 0 ? "save" : row.delta_micros > 0 ? "cost-high" : ""}`, text: fmtSignedUsd(row.delta_micros) }),
            el("td", { text: row.approximate_cross_provider ? "Approximate · cross-provider" : "Approximate · same provider" }),
            el("td", {}, [el("button", {
                    type: "button",
                    class: "btn",
                    text: "Add model",
                    "data-scenario-model": `${row.to_provider}/${row.to_model}`,
                    onClick: () => addCandidate(modelInput, `${row.to_provider}/${row.to_model}`),
                })]),
        ]))),
    ]));
}
function modelWhatIfPanel(client, initial, modelInput) {
    const body = el("div", { class: "scenario-source-body" });
    renderWhatIfBody(body, initial, modelInput);
    const cross = el("input", {
        type: "checkbox",
        "aria-label": "Include cross-provider model candidates",
    });
    cross.addEventListener("change", () => {
        body.replaceChildren(el("p", { class: "skeleton", text: "Repricing captured counts…" }));
        void client.whatif(cross.checked).then((result) => renderWhatIfBody(body, result, modelInput), () => renderWhatIfBody(body, null, modelInput));
    });
    return el("section", { class: "scenario-source", "aria-labelledby": "scenario-model-h" }, [
        el("div", { class: "scenario-section-head" }, [
            el("div", {}, [
                el("p", { class: "eyebrow", text: "Quick input · compatibility view" }),
                el("h2", { id: "scenario-model-h", class: "subhead", text: "Model what-if" }),
            ]),
            el("label", { class: "scenario-inline-control" }, [cross, el("span", { text: "Cross-provider" })]),
        ]),
        el("p", {
            class: "caption",
            text: "Whole captured store, not the active Selection. Add a candidate below to run the authoritative scoped experiment; different tokenizers make every model swap approximate.",
        }),
        body,
    ]);
}
function frontierPanel(frontier, context) {
    const section = el("section", { class: "scenario-source", "aria-labelledby": "scenario-frontier-h" }, [
        el("div", { class: "scenario-section-head" }, [
            el("div", {}, [
                el("p", { class: "eyebrow", text: "Captured evidence · compatibility view" }),
                el("h2", { id: "scenario-frontier-h", class: "subhead", text: "Cost–quality frontier" }),
            ]),
        ]),
        el("p", {
            class: "caption",
            text: "Whole captured store. These are real captured runs, so they can hand off to Compare. Hypothetical experiment cells never masquerade as runs.",
        }),
    ]);
    if (frontier === null) {
        section.appendChild(errorNode("The captured frontier is temporarily unavailable.", "frontier unavailable"));
        return section;
    }
    if (frontier.points.length === 0) {
        section.appendChild(el("p", { class: "caption", text: "No captured runs are available to plot or compare." }));
        return section;
    }
    const selected = new Set();
    const compare = el("a", {
        class: "btn",
        href: routePath(["investigate", "compare"]),
        text: "Compare selected captured runs",
        "aria-disabled": "true",
        "data-scenario-compare": "true",
    });
    const updateCompare = () => {
        const ids = [...selected].sort((a, b) => a.localeCompare(b));
        compare.setAttribute("href", ids.length >= 2
            ? routePath(["investigate", "compare"], { runs: ids.join(",") })
            : routePath(["investigate", "compare"]));
        if (ids.length >= 2)
            compare.removeAttribute("aria-disabled");
        else
            compare.setAttribute("aria-disabled", "true");
    };
    compare.addEventListener("click", (event) => {
        if (selected.size < 2) {
            event.preventDefault();
            return;
        }
        const ids = [...selected].sort((a, b) => a.localeCompare(b));
        context?.analysis.setComparison(ids.map((id) => ({ kind: "run", id, label: id })));
        context?.analysis.navigateWorkspace("investigate");
    });
    const points = [...frontier.points].sort((a, b) => a.cost_micros - b.cost_micros || a.run_id.localeCompare(b.run_id));
    const body = el("tbody");
    for (const point of points) {
        const checkbox = el("input", {
            type: "checkbox",
            "aria-label": `Select ${point.run_id} for Compare`,
            "data-frontier-run": point.run_id,
        });
        checkbox.addEventListener("change", () => {
            if (checkbox.checked)
                selected.add(point.run_id);
            else
                selected.delete(point.run_id);
            updateCompare();
        });
        body.appendChild(el("tr", { class: point.on_frontier ? "on-frontier" : "" }, [
            el("td", {}, [el("label", { class: "scenario-frontier-select" }, [checkbox])]),
            el("td", { text: point.run_id }),
            el("td", { class: "num", text: fmtUsd(point.cost_micros), title: toDollarString(point.cost_micros) }),
            el("td", { class: "num", text: point.quality == null ? "Unscored" : String(point.quality) }),
            el("td", {
                text: point.quality == null && frontier.has_quality
                    ? "Unscored · not quality-ranked"
                    : point.on_frontier ? "Frontier" : "Dominated",
            }),
        ]));
    }
    section.append(el("div", { class: "table-scroll" }, [
        el("table", { class: "data scenario-compact-table" }, [
            el("thead", {}, [el("tr", {}, [
                    el("th", { text: "Compare" }),
                    el("th", { text: "Run" }),
                    el("th", { class: "num", text: "Estimated cost" }),
                    el("th", { class: "num", text: "Your quality" }),
                    el("th", { text: "Frontier state" }),
                ])]),
            body,
        ]),
    ]), el("div", { class: "scenario-frontier-actions" }, [
        compare,
        el("span", {
            class: "caption",
            text: `${frontier.estimated ? "Estimated pricing" : "Recorded pricing"} · ${frontier.pricing_version} · ${frontier.has_quality ? "user quality scores present" : "pure-cost frontier; quality unscored"}`,
        }),
    ]));
    return section;
}
function optionalInteger(input, label) {
    if (input.value.trim() === "")
        return {};
    const value = Number(input.value);
    if (!Number.isSafeInteger(value))
        return { error: `${label} must be a whole-number quality score.` };
    return { value };
}
function isBaselineCell(result, index) {
    return result.cells[index]?.coords.every((coord) => {
        if (coord.axis === "cache_strategy")
            return coord.value === false;
        return coord.value === null;
    }) ?? false;
}
function renderExperimentResult(host, result) {
    if (result.cells.length === 0) {
        host.replaceChildren(emptyState("No priced experiment cells", "The quality gate may exclude every captured run, or every requested target may be unpriced. Tare does not fabricate a $0 result."));
        return;
    }
    const pareto = new Set(result.pareto);
    const summary = el("dl", { class: "scenario-result-summary" }, [
        el("div", {}, [el("dt", { text: "As captured" }), el("dd", { class: "num", text: fmtUsd(result.baseline_micros), title: toDollarString(result.baseline_micros) })]),
        el("div", {}, [el("dt", { text: "Lowest estimate" }), el("dd", { class: "num", text: fmtUsd(result.best_micros), title: toDollarString(result.best_micros) })]),
        el("div", {}, [el("dt", { text: "Estimated reduction" }), el("dd", { class: "num", text: fmtUsd(result.best_saving_micros), title: toDollarString(result.best_saving_micros) })]),
    ]);
    const rows = result.cells.map((cell, index) => {
        const baseline = isBaselineCell(result, index);
        const state = [baseline ? "As captured" : "", pareto.has(index) ? "Pareto" : ""]
            .filter(Boolean)
            .join(" · ") || "Candidate";
        const delta = cell.cost_micros - result.baseline_micros;
        return el("tr", {
            "data-scenario-cell": String(index),
            "data-approximate": String(cell.approximate),
        }, [
            el("td", { text: cell.label.join(" · ") || "as captured" }),
            el("td", { class: "num", text: fmtUsd(cell.cost_micros), title: toDollarString(cell.cost_micros) }),
            el("td", { class: `num ${delta < 0 ? "save" : delta > 0 ? "cost-high" : ""}`, text: fmtSignedUsd(delta) }),
            el("td", { text: cell.approximate ? "Approximate tokenizer reprice" : "Counts-preserving reprice" }),
            el("td", { text: state }),
        ]);
    });
    host.replaceChildren(el("section", { class: "scenario-results", "aria-labelledby": "scenario-results-h" }, [
        el("div", { class: "scenario-section-head" }, [
            el("div", {}, [
                el("p", { class: "eyebrow", text: "Deterministic grid result" }),
                el("h2", { id: "scenario-results-h", class: "subhead", text: "Counterfactual estimates" }),
            ]),
            el("span", { class: "scenario-scope-badge", text: `${result.cells.length} cell${result.cells.length === 1 ? "" : "s"}` }),
        ]),
        summary,
        el("div", { class: "table-scroll" }, [
            el("table", { class: "data scenario-results-table" }, [
                el("thead", {}, [el("tr", {}, [
                        el("th", { text: "Configuration" }),
                        el("th", { class: "num", text: "Estimated cost" }),
                        el("th", { class: "num", text: "Change vs captured" }),
                        el("th", { text: "Compatibility" }),
                        el("th", { text: "Set" }),
                    ])]),
                el("tbody", {}, rows),
            ]),
        ]),
        el("p", {
            class: "caption scenario-result-rule",
            text: `${result.estimated ? "Estimated from captured usage counts" : "Recorded values"} · pricing ${result.pricing_version}. Model swaps preserve captured token counts and are approximate because tokenizers differ. Cache/pricing cells recompose or reprice the same stored counts. Unpriced targets are omitted, never shown as $0.`,
        }),
        el("p", {
            class: "caption scenario-result-rule",
            text: "A counterfactual cell is not a captured run and cannot be sent to structural Compare. Use the captured frontier above for a real-run Compare handoff.",
        }),
    ]));
}
function experimentBuilder(cohort, client, modelInput, snapshotsInput, decache) {
    const minQuality = el("input", {
        id: "scenario-quality-min",
        type: "number",
        step: "1",
        inputmode: "numeric",
        placeholder: "No minimum",
    });
    const maxQuality = el("input", {
        id: "scenario-quality-max",
        type: "number",
        step: "1",
        inputmode: "numeric",
        placeholder: "No maximum",
    });
    const status = el("p", {
        class: "caption scenario-run-status",
        role: "status",
        "aria-live": "polite",
    });
    const resultHost = el("div", { class: "scenario-result-host" });
    const run = el("button", {
        type: "button",
        class: "btn primary",
        text: "Run offline experiment",
        "data-scenario-run": "true",
    });
    run.addEventListener("click", () => {
        const models = canonicalValues(modelInput.value);
        const snapshots = canonicalValues(snapshotsInput.value);
        const invalidSnapshot = snapshots.find((value) => !/^\d{4}-\d{2}-\d{2}$/.test(value));
        if (invalidSnapshot) {
            status.textContent = `Pricing snapshot ${invalidSnapshot} must use YYYY-MM-DD.`;
            snapshotsInput.focus();
            return;
        }
        const axes = [];
        if (models.length > 0)
            axes.push({ kind: "model", values: [AS_CAPTURED, ...models] });
        if (snapshots.length > 0) {
            axes.push({ kind: "pricing_snapshot", values: [AS_CAPTURED, ...snapshots] });
        }
        if (decache.checked)
            axes.push({ kind: "cache_strategy", values: [AS_CAPTURED, "decache"] });
        if (axes.length === 0) {
            status.textContent = "Choose at least one model, pricing snapshot, or the no-cache axis.";
            modelInput.focus();
            return;
        }
        const min = optionalInteger(minQuality, "Minimum quality");
        const max = optionalInteger(maxQuality, "Maximum quality");
        if (min.error || max.error) {
            status.textContent = min.error ?? max.error ?? "Invalid quality constraint.";
            return;
        }
        if (min.value != null && max.value != null && min.value > max.value) {
            status.textContent = "Minimum quality cannot exceed maximum quality.";
            return;
        }
        const request = {
            cohort,
            experiment: { axes },
            ...(min.value == null && max.value == null
                ? {}
                : { quality_constraint: { ...(min.value == null ? {} : { min: min.value }), ...(max.value == null ? {} : { max: max.value }) } }),
        };
        const cells = axes.reduce((count, axis) => count * axis.values.length, 1);
        run.disabled = true;
        run.setAttribute("aria-busy", "true");
        status.textContent = `Repricing ${cells} cell${cells === 1 ? "" : "s"} from stored counts…`;
        resultHost.replaceChildren(el("p", { class: "skeleton", text: "Running deterministic local grid…" }));
        void client.runExperiment(request).then((result) => {
            status.textContent = `Completed ${result.cells.length} priced cell${result.cells.length === 1 ? "" : "s"}.`;
            renderExperimentResult(resultHost, result);
        }, (error) => {
            status.textContent = "Experiment failed. No result was inferred; revise the cohort or axes and retry.";
            resultHost.replaceChildren(errorNode("Couldn't run the offline experiment.", error, {
                actions: [{ label: "Retry", primary: true, run: () => run.click() }],
            }));
        }).finally(() => {
            run.disabled = false;
            run.removeAttribute("aria-busy");
        });
    });
    const form = el("section", { class: "scenario-builder", "aria-labelledby": "scenario-builder-h" }, [
        el("div", { class: "scenario-section-head" }, [
            el("div", {}, [
                el("p", { class: "eyebrow", text: "Scoped CostExperiment" }),
                el("h2", { id: "scenario-builder-h", class: "subhead", text: "Build a counterfactual grid" }),
            ]),
            el("span", { class: "scenario-scope-badge", text: "Local · counts only" }),
        ]),
        el("p", {
            class: "scenario-safety-statement",
            text: "This never calls a model or reads captured payload text. It exhaustively reprices stored usage counts under the declared axes.",
        }),
        el("div", { class: "scenario-fields" }, [
            el("label", { for: "scenario-models" }, [
                el("span", { text: "Model candidates" }),
                modelInput,
                el("span", { class: "caption", text: "Comma-separated pricing model IDs. As-captured is included automatically; swaps are approximate." }),
            ]),
            el("label", { for: "scenario-snapshots" }, [
                el("span", { text: "Pricing snapshots" }),
                snapshotsInput,
                el("span", { class: "caption", text: "Comma-separated YYYY-MM-DD editions. As-captured is included automatically." }),
            ]),
            el("label", { class: "scenario-check" }, [
                decache,
                el("span", {}, [
                    el("span", { text: "Include no-cache counterfactual" }),
                    el("span", { class: "caption", text: "Folds cache reads and writes back into fresh input using the captured counts." }),
                ]),
            ]),
        ]),
        el("fieldset", { class: "scenario-quality" }, [
            el("legend", { text: "Captured-run quality gate (optional)" }),
            el("label", { for: "scenario-quality-min" }, [el("span", { text: "Minimum" }), minQuality]),
            el("label", { for: "scenario-quality-max" }, [el("span", { text: "Maximum" }), maxQuality]),
            el("p", { class: "caption", text: "The gate includes only captured runs with an ingested score inside the bounds. It never predicts the quality of a hypothetical cell; unscored runs are excluded when gated." }),
        ]),
        el("div", { class: "scenario-run-row" }, [run, status]),
        el("p", {
            class: "caption",
            text: "The backend sorts cells deterministically, omits unpriced targets, and reports the Pareto set. Re-running the same cohort, pricing table, axes, and quality gate yields the same result.",
        }),
    ]);
    return el("div", { class: "scenario-experiment-stack" }, [form, resultHost]);
}
export async function renderOptimizeScenarios(root, client, route, context) {
    root.replaceChildren(el("p", { class: "skeleton", text: "Loading scenario inputs…" }));
    const state = context?.analysis.get() ?? initialAnalysisState("optimize");
    const cohort = state.selection ?? state.scope;
    const source = state.selection ? "Selection A" : "Scope";
    const [resolvedResult, adviceResult, whatIfResult, frontierResult] = await Promise.allSettled([
        client.resolveCohort(cohort),
        client.advise(),
        client.whatif(false),
        client.frontier(),
    ]);
    const resolved = valueOf(resolvedResult);
    const advice = valueOf(adviceResult);
    const whatIf = valueOf(whatIfResult);
    const frontier = valueOf(frontierResult);
    const modelInput = el("input", {
        id: "scenario-models",
        type: "text",
        autocomplete: "off",
        placeholder: "claude-haiku-4-5, gpt-5-mini",
    });
    const snapshotsInput = el("input", {
        id: "scenario-snapshots",
        type: "text",
        autocomplete: "off",
        placeholder: "2026-06-01, 2026-07-01",
    });
    const decache = el("input", {
        id: "scenario-decache",
        type: "checkbox",
    });
    const cacheHost = el("div");
    renderCacheAdvice(cacheHost, advice, decache);
    root.replaceChildren(el("section", { class: "optimize-workspace optimize-scenarios", "aria-label": "Optimize scenarios" }, [
        el("header", { class: "optimize-header" }, [
            el("div", {}, [
                el("p", { class: "eyebrow", text: "Offline counterfactual laboratory" }),
                // Not an <h1>: the breadcrumb leaf is the page heading (main.ts setBreadcrumb), so a second
                // <h1> here gave every canonical workspace TWO h1s — and on Scenarios they even disagreed
                // ("Optimize" in the crumb, "Scenarios" here). Same class, so the visual treatment is
                // unchanged; only the heading semantics are fixed.
                el("p", { class: "optimize-title", text: "Scenarios" }),
                el("p", {
                    class: "optimize-lede",
                    text: "Hold captured work constant, vary declared cost axes, and keep estimates, compatibility, and captured evidence visibly separate.",
                }),
            ]),
            el("a", { class: "btn", href: lifecycleHref(route), text: "← Opportunity lifecycle" }),
        ]),
        scenarioContext(cohort, source, resolved),
        el("div", { class: "scenario-source-grid" }, [
            modelWhatIfPanel(client, whatIf, modelInput),
            cacheHost,
        ]),
        frontierPanel(frontier, context),
        experimentBuilder(cohort, client, modelInput, snapshotsInput, decache),
    ]));
}
