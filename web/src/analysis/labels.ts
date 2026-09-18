// Human labels for the analysis DTO's enum values.
//
// These live here — not in a workspace — because Investigate and Optimize both render the analysis
// context strip. Importing them from one workspace into the other would defeat the lazy-workspace
// split (opening Optimize would pull Investigate's whole module into the graph), and duplicating them
// is how the strips drifted apart in the first place.
//
// The strips used to interpolate the raw wire values, so both read
// "Scope · all captured dates · UTC · spend_micros / absolute" — leaking an internal serde identifier
// and its storage unit into the one line whose job is telling the user what they're looking at
// The switches are exhaustive, so adding a metric or normalization to the DTO is
// a compile error here rather than a new leak.

import type { CohortMetric, Normalization } from "./types.js";

export function metricLabel(metric: CohortMetric): string {
  switch (metric) {
    case "spend_micros":
      return "Est. spend";
    case "tokens":
      return "Tokens";
    case "cache_hit_rate":
      return "Cache hit rate";
  }
}

export function normalizationLabel(norm: Normalization): string {
  switch (norm) {
    case "absolute":
      return "absolute";
    case "share_of_selection":
      return "share of selection";
    case "per_run":
      return "per run";
    case "per_outcome":
      return "per outcome";
  }
}
