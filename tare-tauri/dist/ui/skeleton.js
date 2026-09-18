// Shaped loading skeletons: structural placeholders that mirror the layout the data
// will fill, so the screen doesn't collapse-then-expand on every navigation. The shimmer is
// reduced-motion-aware (gated in app.css); under reduced motion the blocks are simply static.
import { el } from "./el.js";
// Every skeleton carries aria-busy="true" so assistive tech announces "busy"
// while data loads instead of reading a pile of empty placeholder blocks as content.
/// A single card-sized placeholder.
export function skelCard() {
    return el("div", { class: "skeleton-block skel-card", "aria-busy": "true" });
}
/// A row of `n` card placeholders (matches a `.cards` row).
export function skelCardRow(n = 3) {
    return el("div", { class: "cards", "aria-busy": "true" }, Array.from({ length: n }, skelCard));
}
/// `n` line placeholders (a table/list loading state).
export function skelRows(n = 6) {
    return el("div", { class: "skel-rows", "aria-busy": "true" }, Array.from({ length: n }, () => el("div", { class: "skeleton-block skel-row" })));
}
/// A generic screen skeleton: a summary row and a block of result rows.
export function skelScreen() {
    return el("section", {
        class: "section",
        role: "status",
        "aria-label": "Loading content",
        "aria-busy": "true",
    }, [skelCardRow(3), skelRows(6)]);
}
