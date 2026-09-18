// Define-at-point-of-use glossary. A single source of truth for the app's cost/token
// vocabulary + an inline info affordance so a term can carry its own definition wherever it appears —
// revealed on click/focus (not hover-only), keyboard-accessible, and framework-free. Foundational:
// screens call `defineTerm("cache read")` instead of leaving jargon undefined at first use.
import { el } from "./el.js";
/// The canonical definitions. Keys are lowercased; look-ups are case-insensitive. Keep each to one
/// plain sentence — the goal is a first-encounter gloss, not documentation.
export const GLOSSARY = {
    "fresh input": "Input tokens sent uncached this turn. The model reads them in full and you pay the full input rate.",
    "cache read": "Input tokens served from the provider's prompt cache at a large discount, because you re-sent an identical prefix.",
    "cache write": "Input tokens written into the cache this turn. This is a one-time surcharge so later turns can read them cheaply.",
    output: "Tokens the model generated, billed at the higher output rate.",
    reasoning: "Hidden 'thinking' tokens some models emit before the answer; billed as output tokens.",
    ttl: "How long a cached prefix lives before it must be re-written. Anthropic offers a 5-minute and a 1-hour tier.",
    "cache erosion": "When a changing prefix keeps invalidating the cache, so you pay the cache write surcharge repeatedly instead of cheap reads.",
    prefix: "The leading, unchanging part of a prompt (system prompt + tool definitions) that the cache keys on.",
    frontier: "The runs where nothing else is both cheaper and higher quality. This is the efficient edge of the cost/quality trade-off.",
    "blended $/1m tokens": "Total spend divided by total tokens, scaled to 1M. This is a single efficiency number across a mix of input/output/cache tokens.",
    estimated: "Computed from captured token counts and a pricing table. Tare never sees your provider invoice, so every figure is an estimate.",
    flamegraph: "A stacked chart where each bar sits inside the bar above it (run → step → prompt component). On screen, width is estimated COST by default — so the widest path is where the money went — with a Tokens mode that weights by token share instead. Exported/report images are always token-weighted.",
};
/// The definition for a term (case-insensitive), or `undefined` if not in the glossary.
export function definitionOf(term) {
    return GLOSSARY[term.trim().toLowerCase()];
}
// Deterministic unique ids for aria wiring (no clock/random — a simple monotonic counter).
let uid = 0;
/// An inline term with a define-at-point-of-use affordance: the `label` text followed by a small ⓘ
/// button that toggles a definition panel (click or keyboard). Unknown terms render as plain text with
/// no dangling affordance, so a typo never leaves an empty popover. `aria-expanded` + `aria-controls`
/// wire the button to the panel; the panel is a `role="tooltip"`, hidden until opened.
export function defineTerm(term, label = term) {
    const def = definitionOf(term);
    if (!def)
        return el("span", { text: label });
    uid += 1;
    const panelId = `glossary-def-${uid}`;
    const panel = el("span", {
        class: "glossary-def",
        role: "tooltip",
        id: panelId,
        hidden: "",
        text: def,
    });
    const btn = el("button", {
        class: "glossary-info",
        type: "button",
        "aria-label": `Define: ${term}`,
        "aria-expanded": "false",
        "aria-controls": panelId,
        text: "ⓘ", // ⓘ
    });
    const setOpen = (open) => {
        btn.setAttribute("aria-expanded", open ? "true" : "false");
        if (open)
            panel.removeAttribute("hidden");
        else
            panel.setAttribute("hidden", "");
    };
    btn.addEventListener("click", () => setOpen(btn.getAttribute("aria-expanded") !== "true"));
    // Esc closes when focus is within the term (keyboard dismissal).
    btn.addEventListener("keydown", (e) => {
        if (e.key === "Escape")
            setOpen(false);
    });
    return el("span", { class: "glossary-term" }, [label, btn, panel]);
}
