// A shared legend mapping the three cost tones (cost-ok / cost-warn / cost-high) to their meaning and
// DIRECTION. Charts that tint marks by share of spend — scatter dots, parcoords lines —
// show this so the color→band mapping, and which end is expensive, isn't left to guesswork. Before
// this, correlate/compare said "colored by spend" without ever stating which color was the costly one.
// Framework-free; presentation-only.

import { el } from "./el.js";

/// The three-swatch cost legend. `by` names what the color encodes (default "share of spend"), used in
/// the accessible name so a screen-reader user gets the same mapping the swatches give a sighted one.
export function costBandLegend(by = "share of spend"): HTMLElement {
  const item = (tone: string, label: string): HTMLElement =>
    el("span", { class: "cost-legend-item" }, [
      el("span", { class: `cost-legend-swatch ${tone}`, "aria-hidden": "true" }),
      el("span", { text: label }),
    ]);
  return el(
    "div",
    { class: "cost-legend caption", role: "img", "aria-label": `Color key by ${by}: cheaper, mid, then costlier` },
    [item("cost-ok", "cheaper"), item("cost-warn", "mid"), item("cost-high", "costlier")]
  );
}
