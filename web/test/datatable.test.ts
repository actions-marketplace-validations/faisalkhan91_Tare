import { describe, it, expect } from "vitest";
import { dataTable, type Column } from "../src/ui/datatable.js";

interface Row {
  id: string;
  spend: number;
}
const rows: Row[] = [
  { id: "alpha", spend: 30 },
  { id: "bravo", spend: 10 },
  { id: "charlie", spend: 20 },
];
const cols: Column<Row>[] = [
  { key: "id", label: "ID", sortValue: (r) => r.id, cell: (r) => r.id },
  { key: "spend", label: "Spend", numeric: true, sortValue: (r) => r.spend, cell: (r) => String(r.spend) },
];

function ids(t: HTMLElement): string[] {
  return Array.from(t.querySelectorAll("tbody tr")).map((tr) => tr.querySelector("td")!.textContent!);
}

describe("dataTable", () => {
  it("applies the initial sort (desc) deterministically", () => {
    const t = dataTable(rows, cols, { rowKey: (r) => r.id, initialSort: { key: "spend", dir: "desc" } });
    expect(ids(t)).toEqual(["alpha", "charlie", "bravo"]); // 30, 20, 10
  });

  it("reuses row node identity across a keyed re-sort", () => {
    const t = dataTable(rows, cols, { rowKey: (r) => r.id, initialSort: { key: "spend", dir: "desc" } });
    // Grab the <tr> for "charlie" (2nd row at 20).
    const charlieBefore = Array.from(t.querySelectorAll("tbody tr")).find(
      (tr) => tr.querySelector("td")!.textContent === "charlie"
    )!;
    // Re-sort ascending — order changes, but charlie's <tr> node must be the SAME instance (moved,
    // not rebuilt), so scroll/focus/selection stay stable.
    const spendHeader = Array.from(t.querySelectorAll("th.sortable")).find((h) =>
      h.textContent!.startsWith("Spend")
    ) as HTMLElement;
    spendHeader.click();
    const charlieAfter = Array.from(t.querySelectorAll("tbody tr")).find(
      (tr) => tr.querySelector("td")!.textContent === "charlie"
    )!;
    expect(charlieAfter).toBe(charlieBefore); // same DOM node, reordered
  });

  it("toggles sort direction on header click", () => {
    const t = dataTable(rows, cols, { rowKey: (r) => r.id, initialSort: { key: "spend", dir: "desc" } });
    const spendHeader = Array.from(t.querySelectorAll("th.sortable")).find((h) =>
      h.textContent!.startsWith("Spend")
    ) as HTMLElement;
    spendHeader.click(); // desc -> asc
    expect(ids(t)).toEqual(["bravo", "charlie", "alpha"]); // 10, 20, 30
  });

  it("filters via the search box (substring) when search is provided", () => {
    const t = dataTable(rows, cols, { rowKey: (r) => r.id, search: (r) => r.id });
    const input = t.querySelector("input.dt-search") as HTMLInputElement;
    input.value = "a"; // alpha, bravo, charlie all contain 'a'... narrow to 'ph'
    input.value = "ph";
    input.dispatchEvent(new Event("input"));
    // debounced; flush by calling again synchronously isn't possible — assert after a tick.
    return new Promise<void>((resolve) => {
      setTimeout(() => {
        expect(ids(t)).toEqual(["alpha"]);
        resolve();
      }, 150);
    });
  });

  it("shows an explicit zero-match row instead of a blank body", () => {
    const t = dataTable(rows, cols, { rowKey: (r) => r.id, search: (r) => r.id });
    const input = t.querySelector("input.dt-search") as HTMLInputElement;
    input.value = "zzz-nothing-matches";
    input.dispatchEvent(new Event("input"));
    return new Promise<void>((resolve) => {
      setTimeout(() => {
        const bodyRows = t.querySelectorAll("tbody tr");
        expect(bodyRows.length).toBe(1);
        expect(t.querySelector("tbody .empty")?.textContent).toBe(
          "No rows match your filter (0 of 3)"
        );
        resolve();
      }, 150);
    });
  });

  it("filters via quick-filter chips (AND-narrow, toggle off restores)", () => {
    const t = dataTable(rows, cols, {
      rowKey: (r) => r.id,
      chips: [
        { label: "≥ 20", predicate: (r) => r.spend >= 20 },
        { label: "a-name", predicate: (r) => r.id.includes("a") },
      ],
    });
    const chips = t.querySelectorAll(".dt-chip");
    expect(chips.length).toBe(2);
    // Engage "≥ 20": alpha (30) + charlie (20).
    (chips[0] as HTMLButtonElement).click();
    expect(chips[0].getAttribute("aria-pressed")).toBe("true");
    expect(ids(t).sort()).toEqual(["alpha", "charlie"]);
    // Also engage "a-name": AND -> alpha + charlie both contain "a"... charlie has an 'a' too.
    (chips[1] as HTMLButtonElement).click();
    expect(ids(t).sort()).toEqual(["alpha", "charlie"]);
    // Toggle the first chip back off -> only the a-name filter remains (alpha, bravo, charlie).
    (chips[0] as HTMLButtonElement).click();
    expect(chips[0].getAttribute("aria-pressed")).toBe("false");
    expect(ids(t).sort()).toEqual(["alpha", "bravo", "charlie"]);
  });

  it("regex toggle switches to pattern matching", () => {
    const t = dataTable(rows, cols, { rowKey: (r) => r.id, search: (r) => r.id });
    const input = t.querySelector("input.dt-search") as HTMLInputElement;
    const re = t.querySelector(".dt-regex input[type=checkbox]") as HTMLInputElement;
    expect(re.getAttribute("aria-label")).toBe("Use regular expression");
    re.checked = true;
    re.dispatchEvent(new Event("change"));
    input.value = "^b"; // anchored
    input.dispatchEvent(new Event("input"));
    return new Promise<void>((resolve) => {
      setTimeout(() => {
        expect(ids(t)).toEqual(["bravo"]);
        resolve();
      }, 150);
    });
  });

  it("renders no search control when search is not configured", () => {
    const t = dataTable(rows, cols, { rowKey: (r) => r.id });
    expect(t.querySelector("input.dt-search")).toBeNull();
  });

  it("reveals a directional cue only when columns overflow", () => {
    const t = dataTable(rows, cols, { rowKey: (r) => r.id });
    const cue = t.querySelector<HTMLElement>(".datatable-overflow-cue")!;
    expect(cue.hidden).toBe(true);
    Object.defineProperties(t, {
      clientWidth: { configurable: true, value: 300 },
      scrollWidth: { configurable: true, value: 620 },
      scrollLeft: { configurable: true, writable: true, value: 0 },
    });
    t.dispatchEvent(new Event("scroll"));
    expect(cue.hidden).toBe(false);
    expect(cue.textContent).toBe("Scroll for more columns →");
    t.scrollLeft = 320;
    t.dispatchEvent(new Event("scroll"));
    expect(cue.textContent).toBe("← Scroll for earlier columns");
  });

  it("adds no data-nav-id when keyboard nav is not opted in", () => {
    const t = dataTable(rows, cols, { rowKey: (r) => r.id });
    expect(t.querySelector("tbody tr[data-nav-id]")).toBeNull();
  });

  it("keyboard nav: j selects, Enter activates, survives a re-sort", () => {
    let activated: string | null = null;
    const t = dataTable(rows, cols, {
      rowKey: (r) => r.id,
      initialSort: { key: "spend", dir: "desc" }, // alpha(30), charlie(20), bravo(10)
      onActivate: (id) => (activated = id),
    });
    const press = (k: string) => t.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true }));
    expect(t.querySelector('tbody tr[data-nav-id="alpha"]')).toBeTruthy();
    press("j"); // → first row in current order (alpha)
    expect(t.querySelector("tbody tr.nav-selected")!.getAttribute("data-nav-id")).toBe("alpha");
    press("j"); // → charlie
    expect(t.querySelector("tbody tr.nav-selected")!.getAttribute("data-nav-id")).toBe("charlie");
    press("Enter");
    expect(activated).toBe("charlie");
    // Re-sort ascending; the selection stays highlighted by id, not by row position.
    const spendHeader = Array.from(t.querySelectorAll("th.sortable")).find((h) =>
      h.textContent?.includes("Spend")
    ) as HTMLElement;
    spendHeader.click(); // desc → asc
    expect(t.querySelector("tbody tr.nav-selected")!.getAttribute("data-nav-id")).toBe("charlie");
    press("Escape");
    expect(t.querySelector("tbody tr.nav-selected")).toBeNull();
  });
});
