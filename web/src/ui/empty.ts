// Intentional empty states: instead of a flat "No data" line, guide a new user to the
// next action. Capture is the setup utility, but Claude Code can capture with zero setup. Token-only +
// reduced-motion-safe.

import { el } from "./el.js";
import { routeHash } from "./store.js";

export interface EmptyStateOpts {
  /// A single glyph (paired with text, never the only signal).
  glyph?: string;
  /// Primary action label; defaults to the setup hub.
  actionLabel?: string;
  /// Where the action links (a route hash). Defaults to the Capture utility.
  actionHref?: string;
  /// Suppress the CTA entirely. Use when no in-app action fits — e.g. a config-missing
  /// state whose real next step is editing `tare.toml` on disk. A wrong Capture CTA on a
  /// config task is a dead end, worse than no CTA.
  noAction?: boolean;
}

/// A centered empty state: glyph + title + muted hint + (unless suppressed) a primary CTA. The default
/// CTA points at Capture so an empty data view becomes an onboarding ramp rather than a dead end.
export function emptyState(title: string, hint: string, opts: EmptyStateOpts = {}): HTMLElement {
  return el("div", { class: "empty-state" }, [
    el("div", { class: "empty-glyph", text: opts.glyph ?? "○" }),
    el("p", { class: "empty-title", text: title }),
    el("p", { class: "empty-hint", text: hint }),
    ...(opts.noAction
      ? []
      : [
          el("a", {
            class: "btn primary",
            href: opts.actionHref ?? routeHash("connect"),
            text: opts.actionLabel ?? "Open Capture →",
          }),
        ]),
  ]);
}
