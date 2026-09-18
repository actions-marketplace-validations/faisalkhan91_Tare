// Router v2 redirect matrix + default-screen migration.
//
// `redirectRoute` is a PURE function: legacy Route -> canonical Route (name + segments + query),
// preserving relevant query parameters and selected run IDs, and never producing a placeholder /
// unknown route. Every row of the redirect table and every current route in docs/ROUTES.md maps
// here to its exact canonical state.
//
// This module is the redirect engine used by web dispatch and native navigation. It covers legacy
// route names as well as the canonical workspace roots.

import type { Route } from "./store.js";

/// The canonical workspace roots after Router v2.
export type CanonicalName = "pulse" | "investigate" | "optimize" | "onboarding";

/// Build a canonical Route from segments + an implied query, merged OVER the preserved legacy query
/// (implied keys win; unrelated legacy keys — `by`, `runs`, `outcome`, … — are preserved).
function canon(
  segments: string[],
  implied: Record<string, string>,
  legacyQuery?: Record<string, string>,
  drop: string[] = []
): Route {
  const merged: Record<string, string> = { ...(legacyQuery ?? {}) };
  for (const k of drop) delete merged[k];
  Object.assign(merged, implied);
  const r: Route = { name: segments[0], segments };
  if (segments.length > 1) r.param = segments.slice(1).join("/");
  if (Object.keys(merged).length) r.query = merged;
  return r;
}

/// Redirect a legacy route to its canonical Router v2 state. Returns the input unchanged when it is
/// already canonical. Preserves query parameters and run IDs; emits no placeholder.
export function redirectRoute(route: Route): Route {
  const q = route.query;

  // Best-effort: the old Overview `Segments` tab, if ever hand-encoded as a query.
  if (route.name === "overview" && q?.tab === "segments") {
    return canon(["investigate"], { view: "facets" }, q, ["tab"]);
  }

  switch (route.name) {
    // Monitor -> Pulse
    case "live":
      return canon(["pulse"], { mode: "now" }, q);
    case "overview":
      return canon(["pulse"], {}, q);

    // Investigate workspace
    case "runs":
      // A run id (param) -> nested canonical run route; the bare list -> entity=runs.
      return route.param
        ? canon(["investigate", "run", route.param], {}, q)
        : canon(["investigate"], { entity: "runs" }, q);
    case "sessions":
      return canon(["investigate"], { entity: "sessions" }, q);
    case "trends":
      // Preserve `?by=` (timeline dimension); add mode=timeline.
      return canon(["investigate"], { mode: "timeline" }, q);
    case "segments":
      return canon(["investigate"], { view: "facets" }, q);
    case "correlate":
      return canon(["investigate"], { mode: "distinguish" }, q);
    case "lineage":
      return canon(["investigate"], { entity: "templates", mode: "lineage" }, q);
    case "units":
      // Defaults for the units view; any named work-unit `outcome=` in the legacy query is preserved.
      return canon(
        ["investigate"],
        { view: "units", metric: "spend_micros", norm: "per_outcome" },
        q
      );
    case "compare":
    case "diff":
      // Preserve the `?runs=` id list in the canonical compare route.
      return canon(["investigate", "compare"], {}, q);

    // Act -> Optimize
    case "experiments":
    case "whatif":
      return canon(["optimize"], { view: "scenarios" }, q);
    case "advise":
      return canon(["optimize"], { type: "cache" }, q);

    // Trust / utility sheets -> Pulse sheets
    case "receipts":
      return canon(["pulse"], { sheet: "trust" }, q);
    case "pricing":
      return canon(["pulse"], { sheet: "trust", view: "pricing" }, q);
    case "connect":
      return canon(["pulse"], { sheet: "capture" }, q);
    case "settings":
      return canon(["pulse"], { sheet: "settings" }, q);

    default:
      // Already canonical (pulse/investigate/optimize/onboarding) or an unknown route: leave as-is.
      // (Unknown routes are handled by the dispatch layer's placeholder, not the redirect matrix.)
      return route;
  }
}

/// Migrate a stored `defaultScreen()` value to its canonical workspace. Legacy values
/// map to pulse/investigate/optimize; already-canonical values pass through; anything unknown falls
/// back to `pulse` (the cold-start default).
export function migrateDefaultScreen(value: string): CanonicalName {
  switch (value) {
    case "live":
    case "overview":
    case "pulse":
      return "pulse";
    case "runs":
    case "sessions":
    case "trends":
    case "investigate":
      return "investigate";
    case "optimize":
    case "experiments":
      return "optimize";
    case "onboarding":
      return "onboarding";
    default:
      return "pulse";
  }
}
