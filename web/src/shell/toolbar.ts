// Toolbar / topbar helpers. Extracted from main.ts so the shell owns the
// toolbar surface. The full contextual toolbar (scope/metric/normalization controls) builds out in
// this is the per-route primary action, its first tenant.

import { el } from "../ui/el.js";
import { routePath, type Route } from "../ui/store.js";

/// The primary "next move" action for a route, shown in the topbar — or `null` when a route has no
/// single obvious companion action.
export function topbarActionFor(route: Route): HTMLElement | null {
  switch (route.name) {
    case "investigate":
      if (route.segments.length > 1) return null;
      return el("a", {
        class: "topbar-action",
        href: routePath(["investigate", "compare"], route.query),
        text: "Compare runs",
      });
    case "optimize":
      if (route.query?.view === "scenarios") return null;
      return el("a", {
        class: "topbar-action",
        href: routePath(["optimize"], { ...(route.query ?? {}), view: "scenarios" }),
        text: "Open scenarios",
      });
    default:
      return null;
  }
}
