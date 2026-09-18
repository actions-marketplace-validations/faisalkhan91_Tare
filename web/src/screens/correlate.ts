// Correlate (the "Explain" panel): re-project every stored run onto its config knobs
// (cache-control, reasoning effort, cache TTL) and its outcomes (tokens, estimated spend, steps),
// then draw them as one parallel-coordinates line per run — "which configuration choices ride with
// high cost?". A line that sits high on a knob axis AND high on spend is the expensive shape.
// PURE re-projection of already-captured rows: zero new capture, all costs estimated. Backed by the
// framework-free `parcoords` primitive.

import { el } from "../ui/el.js";
import { fmtUsd, toDollarString, fmtTokens, fmtDuration } from "../ui/format.js";
import { dataTable } from "../ui/datatable.js";
import { parcoords, type ParCoordsRow } from "../ui/parcoords.js";
import { costBandLegend } from "../ui/costLegend.js";
import { routePath } from "../ui/store.js";
import { skelRows } from "../ui/skeleton.js";
import { emptyState } from "../ui/empty.js";
import { errorNode } from "../ui/errorNode.js";
import { lensSubtitle } from "../ui/lens.js";
import type { CorrelationRow, TareClient } from "../client.js";

// Reasoning-effort tiers → an ordinal so the knob plots on a numeric axis. Unset/unknown sit at the
// bottom (0); the order mirrors the API's low→max ladder. Kept local — it's a presentation encoding.
const EFFORT_ORDINAL: Record<string, number> = {
  minimal: 1,
  low: 2,
  medium: 3,
  high: 4,
  xhigh: 5,
  max: 6,
};

function effortRank(effort: string | null): number {
  return effort ? (EFFORT_ORDINAL[effort] ?? 0) : 0;
}

/// Tone a run's line by its share of the fleet's max spend — the same cheapest→priciest ramp the
/// comparer uses, so "high on a knob AND cost-high in color" reads as the expensive configuration.
function spendTone(micros: number, maxSpend: number): string {
  const share = maxSpend > 0 ? micros / maxSpend : 0;
  return share >= 0.66 ? "cost-high" : share >= 0.33 ? "cost-warn" : "cost-ok";
}

const dollarCell = (m: number): HTMLElement =>
  el("span", { class: "dollars", text: fmtUsd(m), title: toDollarString(m) });

export async function renderCorrelate(root: HTMLElement, client: TareClient): Promise<void> {
  const subtitle = "Spend projected onto the config knobs that drive it: which settings cost you.";
  root.replaceChildren(
    lensSubtitle(subtitle),
    el("section", { class: "section" }, [el("h2", {}, ["Correlate"]), skelRows(6)])
  );

  let rows: CorrelationRow[];
  let pricingVersion = "";
  try {
    const rep = await client.correlate();
    rows = rep.rows;
    pricingVersion = rep.pricing_version;
  } catch (e) {
    root.replaceChildren(
      lensSubtitle(subtitle),
      el("section", { class: "section" }, [
        el("h2", {}, ["Correlate"]),
        errorNode("Couldn't load configuration correlations.", e, {
          actions: [
            { label: "Retry", primary: true, run: () => renderCorrelate(root, client) },
            { label: "Back to Investigate", href: "#/investigate" },
          ],
        }),
      ])
    );
    return;
  }

  if (rows.length === 0) {
    root.replaceChildren(
      lensSubtitle(subtitle),
      el("section", { class: "section" }, [
        el("h2", {}, ["Correlate"]),
        emptyState("No runs to correlate yet", "Capture a few runs and this panel maps which config choices ride with high cost."),
      ])
    );
    return;
  }

  const maxSpend = Math.max(...rows.map((r) => r.cost_micros));
  // Latency is often suppressed in the clock-free core (0). Only offer that axis when it carries
  // signal, so we never plot a flat, meaningless line for every run.
  const hasLatency = rows.some((r) => r.duration_ms > 0);

  const AXES = ["cache control", "effort", "TTL", "tokens", "$ spend", "steps", ...(hasLatency ? ["latency"] : [])];
  const lines: ParCoordsRow[] = rows.map((r) => {
    const values = [
      r.cache_control ? 1 : 0,
      effortRank(r.effort),
      r.ttl === "1h" ? 1 : 0,
      r.tokens,
      r.cost_micros,
      r.steps,
      ...(hasLatency ? [r.duration_ms] : []),
    ];
    return {
      label: `${r.run_id} · ${r.model} · ${fmtUsd(r.cost_micros)}, ${fmtTokens(r.tokens)} tokens`,
      values,
      tone: spendTone(r.cost_micros, maxSpend),
    };
  });

  // The plot needs ≥2 lines to compare shapes; with a single run the table below still tells the
  // story, so we only render the chart when it's meaningful.
  const plotSection =
    rows.length >= 2
      ? el("section", { class: "section" }, [
          el("h2", {}, ["Config and cost"]),
          el("p", {
            class: "caption",
            text: "Each run is one line across its config knobs and outcomes (every axis scaled to this run set), colored by spend. A line high on a knob, such as cache control, effort, or 1h TTL, and high on $ spend is the expensive configuration. Hover to isolate a run.",
          }),
          parcoords(AXES, lines, {
            ariaLabel: "Parallel coordinates: config knobs versus cost, one line per run",
            // Per-axis min/max ticks. Axis order: cache-ctrl, effort, TTL, tokens, $ spend,
            // steps, [latency]. Binary/rank knobs (0/1/2) get "" — a numeric min/max is meaningless there.
            axisFormat: (i, v) =>
              i === 3 ? fmtTokens(v) : i === 4 ? fmtUsd(v) : i === 5 ? String(Math.round(v)) : AXES[i] === "latency" ? fmtDuration(v) : "",
          }),
          // "colored by spend" now says which end is expensive.
          costBandLegend("$ spend"),
        ])
      : "";

  const table = dataTable<CorrelationRow>(
    rows,
    [
      {
        key: "run_id",
        label: "Run",
        sortValue: (r) => r.run_id,
        cell: (r) => el("a", {
          class: "run-link",
          href: routePath(["investigate", "run", r.run_id]),
          text: r.run_id,
        }),
      },
      { key: "model", label: "Model", sortValue: (r) => r.model, cell: (r) => r.model || "Unknown model" },
      {
        key: "cache_control",
        label: "Cache control",
        sortValue: (r) => (r.cache_control ? 1 : 0),
        cell: (r) => (r.cache_control ? "On" : "Off"),
      },
      {
        key: "effort",
        label: "Effort",
        sortValue: (r) => effortRank(r.effort),
        cell: (r) => r.effort ?? "Not set",
      },
      { key: "ttl", label: "TTL", sortValue: (r) => r.ttl, cell: (r) => r.ttl },
      {
        key: "tokens",
        label: "Tokens",
        numeric: true,
        sortValue: (r) => r.tokens,
        cell: (r) => fmtTokens(r.tokens),
      },
      {
        key: "cost",
        label: "Spend (est)",
        numeric: true,
        sortValue: (r) => r.cost_micros,
        cell: (r) => dollarCell(r.cost_micros),
      },
      {
        key: "steps",
        label: "Steps",
        numeric: true,
        sortValue: (r) => r.steps,
        cell: (r) => String(r.steps),
      },
    ],
    {
      rowKey: (r) => r.run_id,
      search: (r) => `${r.run_id} ${r.model} ${r.effort ?? ""}`,
      searchPlaceholder: "Filter runs…",
      initialSort: { key: "cost", dir: "desc" },
      onActivate: (runId) => {
        window.location.hash = routePath(["investigate", "run", runId]);
      },
    }
  );

  root.replaceChildren(
    lensSubtitle(subtitle),
    plotSection,
    el("section", { class: "section" }, [
      el("h2", {}, ["Runs by configuration"]),
      el("p", {
        class: "caption",
        text: `Every captured run with its config knobs and estimated outcomes, sorted by spend. Estimated${pricingVersion ? `. Pricing ${pricingVersion}` : ""}.`,
      }),
      table,
    ])
  );
}
