import { describe, it, expect } from "vitest";
import { receiptStatement } from "../src/ui/receiptStatement.js";

describe("receiptStatement", () => {
  it("typesets the verify result as an itemized ledger", () => {
    const dl = receiptStatement({
      pricing_version: "fixture-2026.06",
      recomputed_total_micros: 1_920_000,
      rows: 3,
      flamegraph_checked: true,
    });
    const rows = dl.querySelectorAll(".receipt-row");
    expect(rows.length).toBe(4);
    expect(dl.textContent).toContain("Recomputed total");
    expect(dl.querySelector(".receipt-row .num")?.textContent).toBe("$1.92");
    expect(dl.textContent).toContain("fixture-2026.06");
    expect(dl.textContent).toContain("matched");
  });

  it('shows "not checked" when the flamegraph was not verified', () => {
    const dl = receiptStatement({
      pricing_version: "v",
      recomputed_total_micros: 0,
      rows: 0,
      flamegraph_checked: false,
    });
    expect(dl.textContent).toContain("not checked");
  });
});
