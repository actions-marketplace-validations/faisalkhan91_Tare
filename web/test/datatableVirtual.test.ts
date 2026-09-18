// dataTable row windowing. A 1,000-row fixture must keep a bounded DOM (≤~80 rows),
// preserve sort/filter, expose accessible aria-rowcount/aria-rowindex (position despite windowing),
// keep keyboard nav traversing the FULL set (scrolling the selection into the window), and NOT change
// byte output for non-windowed tables.

import { describe, it, expect } from "vitest";
import { dataTable, type Column } from "../src/ui/datatable.js";

interface Row {
  id: string;
  n: number;
}
const rows = (count: number): Row[] => Array.from({ length: count }, (_, i) => ({ id: `r${i}`, n: count - i }));
const cols: Array<Column<Row>> = [
  { key: "id", label: "ID", sortValue: (r) => r.id, cell: (r) => r.id },
  { key: "n", label: "N", numeric: true, sortValue: (r) => r.n, cell: (r) => String(r.n) },
];
const dataRows = (t: HTMLElement) => t.querySelectorAll("tbody tr[data-nav-id]");

describe("dataTable windowing", () => {
  it("keeps a bounded DOM (≤~80 rows) for a 1,000-row fixture", () => {
    const t = dataTable(rows(1000), cols, { rowKey: (r) => r.id, rowHeight: 36, onActivate: () => {} });
    document.body.appendChild(t);
    const rendered = dataRows(t).length;
    expect(rendered).toBeGreaterThan(0);
    expect(rendered).toBeLessThanOrEqual(80);
    // Sized spacers reserve the off-window height so the scrollbar still spans all 1,000 rows.
    expect(t.querySelectorAll("tbody tr.dt-spacer").length).toBeGreaterThanOrEqual(1);
    t.remove();
  });

  it("exposes accessible aria-rowcount + aria-rowindex (position despite windowing)", () => {
    const t = dataTable(rows(1000), cols, { rowKey: (r) => r.id, rowHeight: 36, onActivate: () => {} });
    document.body.appendChild(t);
    expect(t.querySelector("table")?.getAttribute("aria-rowcount")).toBe("1001"); // 1000 data + header
    const first = dataRows(t)[0];
    expect(first.getAttribute("aria-rowindex")).toBe("2"); // header is aria row 1
    t.remove();
  });

  it("sort still operates over the FULL set, not just the window", () => {
    const t = dataTable(rows(1000), cols, { rowKey: (r) => r.id, rowHeight: 36, onActivate: () => {} });
    document.body.appendChild(t);
    // Sort by N ascending (click → desc, click again → asc) → the smallest N (row r999, n=1) becomes
    // the GLOBAL first row — proving the sort spans the full set, not just the rendered window.
    const nHeader = Array.from(t.querySelectorAll("th")).find((h) => h.textContent?.startsWith("N"))!;
    nHeader.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    nHeader.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(dataRows(t)[0].getAttribute("data-nav-id")).toBe("r999");
    t.remove();
  });

  it("keyboard nav traverses the full set: j from an off-window selection scrolls it into view", () => {
    let activated: string | null = null;
    const t = dataTable(rows(1000), cols, { rowKey: (r) => r.id, rowHeight: 36, onActivate: (id) => (activated = id) });
    document.body.appendChild(t);
    // Select a row far past the initial window, then Enter activates it (full-set traversal).
    // Simulate many j presses to walk beyond the window edge.
    for (let i = 0; i < 90; i++) t.dispatchEvent(new KeyboardEvent("keydown", { key: "j", bubbles: true }));
    t.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(activated).toBe("r89"); // 90 downward steps from the top landed on the 90th row (r89)
    // …and that row is actually rendered in the window (scrolled into view), still ≤~80 in the DOM.
    expect(t.querySelector('tbody tr[data-nav-id="r89"]')).toBeTruthy();
    expect(dataRows(t).length).toBeLessThanOrEqual(80);
    t.remove();
  });

  it("does NOT window (byte-identical) when rowHeight is omitted", () => {
    const plain = dataTable(rows(200), cols, { rowKey: (r) => r.id });
    // No windowing → every row rendered, no spacers, no viewport.
    expect(plain.querySelectorAll("tbody tr").length).toBe(200);
    expect(plain.querySelector(".dt-spacer")).toBeNull();
    expect(plain.querySelector(".dt-viewport")).toBeNull();
    expect(plain.querySelector("table")?.hasAttribute("aria-rowcount")).toBe(false);
  });
});
