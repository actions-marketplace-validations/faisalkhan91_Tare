// Pricing / Models catalog: a browsable, sortable, searchable table of every model in
// the bundled pricing table with its $/1M-token rates + provenance, plus the unpriced models seen
// in this store. Pure read of bundled, never-network data — a reference tool, and the home for the
// estimate-transparency story (what Tare thinks each model costs).
import { el } from "../ui/el.js";
import { errorNode } from "../ui/errorNode.js";
import { fmtUsd, fmtTokens, toDollarString } from "../ui/format.js";
import { dataTable } from "../ui/datatable.js";
import { lensSubtitle } from "../ui/lens.js";
import { emptyState } from "../ui/empty.js";
import { defineTerm } from "../ui/glossary.js";
const rate = (m) => el("span", { class: "dollars num", text: fmtUsd(m), title: toDollarString(m) });
export async function renderPricing(root, client) {
    root.replaceChildren(lensSubtitle("What Tare thinks each model costs: the bundled rate table used for estimates."));
    let info;
    try {
        info = await client.pricing();
    }
    catch (e) {
        root.appendChild(errorNode("Couldn't load pricing. Check that capture is running.", e, {
            actions: [{ label: "Retry", primary: true, run: () => renderPricing(root, client) }],
        }));
        return;
    }
    const models = info.models ?? [];
    const provenance = el("p", {
        class: "caption",
        text: `Pricing table ${info.version}, effective ${info.effective_date}. Rates are $ per 1M tokens (estimate; bundled, never fetched).${info.note ? ` ${info.note}` : ""}`,
    });
    // Inline key for the encoded rate columns: cache read vs write, the 5m/1h cache
    // LIFETIMES, and what a Tier is — defined in place rather than left as bare header abbreviations.
    const legend = el("p", { class: "caption sub" }, [
        "Columns: ",
        defineTerm("cache read", "Cache read"),
        " is served from cache at a discount; Cache write is the surcharge to store a prefix, priced per ",
        defineTerm("ttl", "cache lifetime"),
        ": 5m (5-minute) or 1h (1-hour). Tier is the model's size/context class.",
    ]);
    const section = el("section", { class: "section" }, [
        el("h2", {}, ["Model pricing"]),
        provenance,
        legend,
    ]);
    if (models.length === 0) {
        section.appendChild(emptyState("No bundled pricing", "The pricing table reported no models.", { actionLabel: "Back to Pulse →", actionHref: "#/pulse" }));
    }
    else {
        const cols = [
            { key: "model_id", label: "Model", sortValue: (r) => r.model_id, cell: (r) => r.model_id },
            { key: "provider", label: "Provider", sortValue: (r) => r.provider, cell: (r) => el("span", { class: "sub", text: r.provider }) },
            { key: "tier", label: "Tier", sortValue: (r) => r.tier, cell: (r) => el("span", { class: "sub", text: r.tier }) },
            { key: "input", label: "Input $/1M", numeric: true, sortValue: (r) => r.input_micro_per_mtok, cell: (r) => rate(r.input_micro_per_mtok) },
            { key: "output", label: "Output $/1M", numeric: true, sortValue: (r) => r.output_micro_per_mtok, cell: (r) => rate(r.output_micro_per_mtok) },
            { key: "cache_read", label: "Cache read $/1M", numeric: true, sortValue: (r) => r.cache_read_micro_per_mtok, cell: (r) => rate(r.cache_read_micro_per_mtok) },
            { key: "cache_5m", label: "Cache write 5m $/1M", numeric: true, sortValue: (r) => r.cache_write_5m_micro_per_mtok, cell: (r) => rate(r.cache_write_5m_micro_per_mtok) },
            { key: "cache_1h", label: "Cache write 1h $/1M", numeric: true, sortValue: (r) => r.cache_write_1h_micro_per_mtok, cell: (r) => rate(r.cache_write_1h_micro_per_mtok) },
        ];
        section.appendChild(dataTable(models, cols, {
            rowKey: (r) => `${r.provider}/${r.model_id}`,
            search: (r) => `${r.provider} ${r.model_id} ${r.tier}`,
            searchPlaceholder: "Filter models…",
            initialSort: { key: "input", dir: "desc" },
        }));
    }
    root.appendChild(section);
    // "Unpriced models seen in your store" — from the report (already computed). Their spend is
    // usage-only until a row is added to pricing.json (honest estimate-transparency).
    try {
        const report = await client.report();
        if (report.unpriced && report.unpriced.length > 0) {
            const rows = report.unpriced.map((u) => el("tr", {}, [
                el("td", { text: `${u.provider}/${u.model}` }),
                el("td", { class: "num", text: fmtTokens(u.token_total) }),
            ]));
            root.appendChild(el("section", { class: "section" }, [
                el("h2", {}, ["Unpriced models seen in your store"]),
                el("p", { class: "caption", text: "Captured but absent from the table, so their spend is unpriced and not included in totals. Add a row to pricing.json to price them." }),
                el("table", { class: "data" }, [
                    el("thead", {}, [el("tr", {}, [el("th", { text: "Model" }), el("th", { text: "Tokens", class: "num" })])]),
                    el("tbody", {}, rows),
                ]),
            ]));
        }
    }
    catch {
        /* unpriced section is best-effort */
    }
}
