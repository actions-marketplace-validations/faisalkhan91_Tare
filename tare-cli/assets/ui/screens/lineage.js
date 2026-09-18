// Lineage: a prompt/config lineage is a user-named, ordered set of prompt-component
// VERSIONS — each an immutable content-addressed fingerprint. This view plots cost-per-run for each
// version so you can answer "did my rewrite (v3) hold quality at lower cost than v2?". One section
// per configured lineage; versions stay in declared order (v1→v2→v3 reads left-to-right), each with
// a proportional $/run bar toned cheapest→priciest. Pure re-projection of stored rows; estimated.
import { el } from "../ui/el.js";
import { fmtUsd, toDollarString, fmtTokens } from "../ui/format.js";
import { emptyState } from "../ui/empty.js";
import { errorNode } from "../ui/errorNode.js";
import { skelRows } from "../ui/skeleton.js";
import { lensSubtitle } from "../ui/lens.js";
/// One lineage as a section: a per-version row reusing the cost-driver grid (label | proportional
/// $/run bar | total | meta). The cheapest observed version (by $/run) is flagged as the win.
function lineageSection(rep) {
    const seen = rep.rows.filter((r) => r.runs > 0);
    const maxPerRun = Math.max(1, ...rep.rows.map((r) => r.micros_per_run));
    const minPerRun = seen.length ? Math.min(...seen.map((r) => r.micros_per_run)) : 0;
    const versionRow = (r) => {
        const share = r.micros_per_run / maxPerRun;
        const isCheapest = r.runs > 0 && r.micros_per_run === minPerRun && seen.length > 1;
        // The bar and the bold number beside it must encode the SAME measure: both are
        // $/run (what this screen ranks on). Total cost + run count move into the meta line.
        const meta = r.runs === 0
            ? "not captured yet"
            : `${fmtUsd(r.cost_micros)} total · ${r.runs} run${r.runs === 1 ? "" : "s"} · ${fmtTokens(r.tokens)} tokens${isCheapest ? " · lowest $/run" : ""}`;
        return el("div", { class: "driver-row" }, [
            el("span", { class: "driver-label", text: r.label, title: `fingerprint ${r.hash}` }),
            el("div", { class: "driver-bar-track" }, [
                el("div", { class: "driver-bar", style: `width:${Math.round(share * 100)}%` }),
            ]),
            el("span", {
                class: "driver-cost dollars",
                text: r.runs === 0 ? "Not captured" : `${fmtUsd(r.micros_per_run)}/run`,
                title: toDollarString(r.micros_per_run),
            }),
            el("span", { class: "driver-share", text: meta }),
        ]);
    };
    return el("section", { class: "section" }, [
        el("h2", {}, [rep.name]),
        el("p", {
            class: "caption",
            text: `Estimated cost per run across ${rep.rows.length} version${rep.rows.length === 1 ? "" : "s"} (each version is an immutable prompt fingerprint). Lower $/run at held quality is the win. Pricing ${rep.pricing_version}.`,
        }),
        ...rep.rows.map(versionRow),
    ]);
}
export async function renderLineage(root, client) {
    const subtitle = "Cost per prompt/config version: did a rewrite hold quality at lower cost?";
    root.replaceChildren(lensSubtitle(subtitle), el("section", { class: "section" }, [el("h2", {}, ["Lineage"]), skelRows(4)]));
    let reps;
    try {
        reps = await client.lineages();
    }
    catch (e) {
        root.replaceChildren(lensSubtitle(subtitle), el("section", { class: "section" }, [
            el("h2", {}, ["Lineage"]),
            errorNode("Couldn't load prompt lineages.", e, {
                actions: [
                    { label: "Retry", primary: true, run: () => renderLineage(root, client) },
                    { label: "Back to Investigate", href: "#/investigate" },
                ],
            }),
        ]));
        return;
    }
    if (reps.length === 0) {
        root.replaceChildren(lensSubtitle(subtitle), el("section", { class: "section" }, [
            el("h2", {}, ["Lineage"]),
            emptyState("No lineages configured", "Add [[lineage]] entries to tare.toml. Bind version labels (v1, v2…) to prompt-component fingerprints; then this view compares their cost-per-run.", { noAction: true }),
        ]));
        return;
    }
    root.replaceChildren(lensSubtitle(subtitle), ...reps.map(lineageSection));
}
