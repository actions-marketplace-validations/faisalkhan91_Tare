// Lens subtitle: a one-line framing under a screen's title declaring that this screen is
// a *lens* on the one shared primitive — the cause-attributed flamegraph (run → step → prompt
// component → cache class). Every screen slices the same attribution; none is a separate feature.
// Uses its own `.lens` class (distinct from `.caption`) so it never collides with screen captions.

import { el } from "./el.js";

export function lensSubtitle(text: string): HTMLElement {
  return el("p", { class: "lens", text });
}
