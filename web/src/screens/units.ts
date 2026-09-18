// Units of work: dev-meaningful DENOMINATORS. A user declares units — a task, a PR, a
// feature — as retroactive match rules ([[unit]] in tare.toml), and Tare buckets already-captured
// runs into them so you can see cost-per-unit ("what did this buy me?"). One row per unit + an
// explicit "unbucketed" row for runs no rule claimed (coverage honesty). Pure re-projection;
// estimated. Reuses the cost-driver grid (label | proportional bar | total | meta).

import { el } from "../ui/el.js";
import { fmtUsd, toDollarString, fmtTokens } from "../ui/format.js";
import { emptyState } from "../ui/empty.js";
import { errorNode } from "../ui/errorNode.js";
import { skelRows } from "../ui/skeleton.js";
import { lensSubtitle } from "../ui/lens.js";
import type { UnitReport, UnitRow, TareClient } from "../client.js";

export async function renderUnits(root: HTMLElement, client: TareClient): Promise<void> {
  const subtitle = "Spend bucketed into dev-meaningful units of work: cost per feature, fix, or task.";
  root.replaceChildren(
    lensSubtitle(subtitle),
    el("section", { class: "section" }, [el("h2", {}, ["Units of work"]), skelRows(4)])
  );

  let rep: UnitReport;
  try {
    rep = await client.units();
  } catch (e) {
    root.replaceChildren(
      lensSubtitle(subtitle),
      el("section", { class: "section" }, [
        el("h2", {}, ["Units of work"]),
        errorNode("Couldn't load work units.", e, {
          actions: [
            { label: "Retry", primary: true, run: () => renderUnits(root, client) },
            { label: "Back to Investigate", href: "#/investigate" },
          ],
        }),
      ])
    );
    return;
  }

  const declared = rep.rows.filter((r) => r.name !== "unbucketed");
  if (declared.length === 0) {
    root.replaceChildren(
      lensSubtitle(subtitle),
      el("section", { class: "section" }, [
        el("h2", {}, ["Units of work"]),
        emptyState(
          "No units of work configured",
          "Add [[unit]] entries to tare.toml, each with a name and a match rule (sessions, commits, or a run-id prefix). Then this view shows cost per task, PR, or feature.",
          { noAction: true }, // config task in tare.toml, not a connect ramp
        ),
      ])
    );
    return;
  }

  const maxCost = Math.max(1, ...rep.rows.map((r) => r.cost_micros));
  const unitRow = (r: UnitRow): HTMLElement => {
    const share = r.cost_micros / maxCost;
    const isUnbucketed = r.name === "unbucketed";
    const meta =
      r.runs === 0
        ? "no runs"
        : `${fmtUsd(r.micros_per_run)}/run · ${r.runs} run${r.runs === 1 ? "" : "s"} · ${fmtTokens(r.tokens)} tokens`;
    return el("div", { class: `driver-row${isUnbucketed ? " is-muted" : ""}` }, [
      el("span", { class: "driver-label", text: r.name }),
      el("div", { class: "driver-bar-track" }, [
        el("div", { class: "driver-bar", style: `width:${Math.round(share * 100)}%` }),
      ]),
      el("span", { class: "driver-cost dollars", text: fmtUsd(r.cost_micros), title: toDollarString(r.cost_micros) }),
      el("span", { class: "driver-share", text: meta }),
    ]);
  };

  root.replaceChildren(
    lensSubtitle(subtitle),
    el("section", { class: "section" }, [
      el("h2", {}, ["Units of work"]),
      el("p", {
        class: "caption",
        text: `Estimated spend per unit of work. Each run is counted once (first matching rule). ${fmtUsd(rep.total_micros)} total, ${rep.unbucketed_runs} run${rep.unbucketed_runs === 1 ? "" : "s"} unbucketed. Pricing ${rep.pricing_version}.`,
      }),
      ...rep.rows.map(unitRow),
    ])
  );
}
