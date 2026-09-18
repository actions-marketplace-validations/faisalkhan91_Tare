// Rail navigation model. The sidebar's information architecture — extracted
// from main.ts so the shell owns its nav model. The rail's DOM build-out and the contextual Commands
// entry expand here in; this module is the data + helpers they build on.

import type { IconName } from "../ui/icon.js";

export interface NavItem {
  name: string;
  label: string;
  icon?: IconName;
}
export interface NavSection {
  title: string;
  items: NavItem[];
}

export interface LensRoute extends NavItem {
  /// Canonical child-workspace destination. Advanced lenses are capabilities, not legacy peers.
  href: string;
  description: string;
}

// IA reframed to the user's real WORKFLOW, not internal ontology. Three zones named for
// what you're doing: Monitor (is spend happening now / at a glance), Investigate (drill into where it
// went), Act (cut the bill). Low-frequency utilities stay in the footer; Diff/Receipts are contextual
// (from Runs / the palette). The Correlate/Lineage/Units lenses are DEMOTED off the rail (below) —
// they're re-projections of the one attribution rollup, reached via the palette rather than as peer
// destinations.
export const SECTIONS: NavSection[] = [
  {
    title: "Monitor",
    items: [
      // Live + Overview cut over to the single Pulse workspace. Old #/live and
      // #/overview links still resolve — they redirect to Pulse — but the rail shows one destination.
      { name: "pulse", label: "Pulse", icon: "pulse" },
    ],
  },
  {
    title: "Investigate",
    items: [
      // The canonical Investigate workspace, which supersedes the separate Runs / Sessions / Trends
      // screens (their entity/timeline modes live inside it). It had NO rail entry at all until now,
      // which had two visible consequences: the sidebar showed no active item anywhere in the
      // workspace — including Run Profile and Compare — and, because `baseLabel` resolves names via
      // `allNav()`, the breadcrumb rendered the raw route id lowercase ("investigate", and a literal
      // "compare" as the page <h1>).
      { name: "investigate", label: "Investigate", icon: "investigate" },
    ],
  },
  {
    title: "Act",
    items: [{ name: "optimize", label: "Optimize", icon: "optimize" }],
  },
];
// Low-frequency child lenses remain discoverable in the palette without reviving the old parallel
// route hierarchy. Each entry points directly at its canonical Investigate/Optimize state. Runs,
// Sessions, Templates, Steps, and Timeline are already first-class modes inside Investigate; they
// are intentionally not repeated here as legacy destinations.
export const LENS_ROUTES: LensRoute[] = [
  {
    name: "correlations",
    label: "Configuration correlations",
    href: "#/investigate?mode=distinguish",
    description: "Relate captured settings to spend",
  },
  {
    name: "lineage",
    label: "Prompt lineage",
    href: "#/investigate?entity=templates&mode=lineage",
    description: "Compare prompt and configuration versions",
  },
  {
    name: "units",
    label: "Work units",
    href: "#/investigate?metric=spend_micros&norm=per_outcome&view=units",
    description: "See cost per feature, fix, or task",
  },
  {
    name: "scenarios",
    label: "Offline scenarios",
    href: "#/optimize?view=scenarios",
    description: "Model changes before applying them",
  },
];
export const FOOTER: NavItem[] = [
  { name: "connect", label: "Capture", icon: "capture" },
  { name: "pricing", label: "Trust & pricing", icon: "trust" },
  { name: "settings", label: "Settings", icon: "settings" },
];

/// Every routable rail destination (sections + footer), for palette/goto wiring.
export function allNav(): NavItem[] {
  return [...SECTIONS.flatMap((s) => s.items), ...FOOTER];
}

/// The rail section title that owns `name`, if any (used for breadcrumb zone context).
export function sectionFor(name: string): string | undefined {
  return SECTIONS.find((s) => s.items.some((i) => i.name === name))?.title;
}
