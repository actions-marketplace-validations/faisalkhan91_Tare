import { describe, it, expect } from "vitest";
import { attachListNav, applySelection } from "../src/ui/listNav.js";

function list(ids: string[]): HTMLElement {
  const c = document.createElement("div");
  for (const id of ids) {
    const r = document.createElement("div");
    r.dataset.navId = id;
    r.textContent = id;
    c.appendChild(r);
  }
  return c;
}
function key(target: HTMLElement, k: string): void {
  target.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true }));
}
const selectedId = (c: HTMLElement): string | null =>
  c.querySelector(".nav-selected")?.getAttribute("data-nav-id") ?? null;

describe("listNav keyboard behavior", () => {
  it("j/↓ moves next, k/↑ prev; from empty j→first, k→last; clamps at ends", () => {
    let sel: string | null = null;
    const c = list(["a", "b", "c"]);
    attachListNav(c, { getSelected: () => sel, setSelected: (id) => (sel = id) });
    key(c, "j");
    expect(sel).toBe("a");
    key(c, "j");
    expect(sel).toBe("b");
    key(c, "ArrowDown");
    expect(sel).toBe("c");
    key(c, "j");
    expect(sel).toBe("c"); // clamp at end
    key(c, "k");
    expect(sel).toBe("b");
    key(c, "ArrowUp");
    expect(sel).toBe("a");
    sel = null;
    key(c, "k");
    expect(sel).toBe("c"); // re-entry from cleared → last
  });

  it("keeps j/k focus-scoped to the list (WCAG 2.1.4)", () => {
    // The handler binds to the container (not document), so a keydown targeting an element OUTSIDE
    // the list never reaches it — single-char shortcuts are active only when the component has focus,
    // satisfying SC 2.1.4 Character Key Shortcuts (Level A) option 3.
    let sel: string | null = null;
    const c = list(["a", "b", "c"]);
    const outside = document.createElement("button");
    document.body.append(c, outside);
    attachListNav(c, { getSelected: () => sel, setSelected: (id) => (sel = id) });
    key(outside, "j"); // focus/event outside the list
    expect(sel).toBe(null); // no traversal — the list didn't hijack the global 'j'
    key(c, "j"); // event within the list
    expect(sel).toBe("a"); // now it moves
    c.remove();
    outside.remove();
  });

  it("highlights exactly the selected row (nav-selected + aria-selected)", () => {
    let sel: string | null = null;
    const c = list(["a", "b"]);
    attachListNav(c, { getSelected: () => sel, setSelected: (id) => (sel = id) });
    key(c, "j");
    expect(selectedId(c)).toBe("a");
    expect(c.querySelector('[data-nav-id="a"]')!.getAttribute("aria-selected")).toBe("true");
    key(c, "j");
    expect(selectedId(c)).toBe("b");
    expect(c.querySelector('[data-nav-id="a"]')!.classList.contains("nav-selected")).toBe(false);
  });

  it("Enter activates the selected id; Esc clears + fires onEscape", () => {
    let sel: string | null = "b";
    let activated: string | null = null;
    let escaped = false;
    const c = list(["a", "b", "c"]);
    attachListNav(c, {
      getSelected: () => sel,
      setSelected: (id) => (sel = id),
      onActivate: (id) => (activated = id),
      onEscape: () => (escaped = true),
    });
    key(c, "Enter");
    expect(activated).toBe("b");
    key(c, "Escape");
    expect(sel).toBeNull();
    expect(escaped).toBe(true);
    expect(selectedId(c)).toBeNull();
  });

  it("selection is id-keyed — survives a re-sort / re-render", () => {
    let sel: string | null = null;
    const c = list(["a", "b", "c"]);
    attachListNav(c, { getSelected: () => sel, setSelected: (id) => (sel = id) });
    key(c, "j");
    key(c, "j");
    expect(sel).toBe("b");
    // Re-render in a new order; the source still holds "b".
    c.replaceChildren();
    for (const id of ["c", "b", "a"]) {
      const r = document.createElement("div");
      r.dataset.navId = id;
      c.appendChild(r);
    }
    applySelection(c, sel);
    expect(selectedId(c)).toBe("b"); // highlight followed the id, not the index
    key(c, "j"); // from "b" (now DOM index 1) → "a" (index 2)
    expect(sel).toBe("a");
  });

  it("does not hijack Enter/arrows aimed at a focused control inside a row", () => {
    let sel: string | null = "a";
    let activated = 0;
    const c = list(["a", "b"]);
    const btn = document.createElement("button");
    c.querySelector('[data-nav-id="a"]')!.appendChild(btn);
    const dropdown = document.createElement("select");
    c.querySelector('[data-nav-id="b"]')!.appendChild(dropdown);
    attachListNav(c, { getSelected: () => sel, setSelected: (id) => (sel = id), onActivate: () => activated++ });
    // Enter on a focused row button → the button acts natively, the list does NOT activate.
    key(btn, "Enter");
    expect(activated).toBe(0);
    // ArrowDown on a focused <select> → native option nav, not a list move.
    key(dropdown, "ArrowDown");
    expect(sel).toBe("a");
    // Enter on the container itself (no control focused) still activates the list.
    key(c, "Enter");
    expect(activated).toBe(1);
  });

  it("does not hijack typing in an input inside the list", () => {
    let sel: string | null = null;
    const c = list(["a", "b"]);
    const input = document.createElement("input");
    c.appendChild(input);
    attachListNav(c, { getSelected: () => sel, setSelected: (id) => (sel = id) });
    key(input, "j");
    expect(sel).toBeNull();
  });
});
