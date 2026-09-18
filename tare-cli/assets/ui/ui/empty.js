// Intentional empty states: instead of a flat "No data" line, guide a new user to the
// next action. Capture is the setup utility, but Claude Code can capture with zero setup. Token-only +
// reduced-motion-safe.
import { el } from "./el.js";
import { routeHash } from "./store.js";
/// A centered empty state: glyph + title + muted hint + (unless suppressed) a primary CTA. The default
/// CTA points at Capture so an empty data view becomes an onboarding ramp rather than a dead end.
export function emptyState(title, hint, opts = {}) {
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
