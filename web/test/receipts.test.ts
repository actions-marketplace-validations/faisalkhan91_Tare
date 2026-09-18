import { describe, it, expect } from "vitest";
import { renderReceiptVerifier } from "../src/screens/receipts.js";
import { fakeClient } from "./fakeClient.js";

describe("receipt verifier", () => {
  it("attests a run and shows the offline recomputation result", async () => {
    const client = fakeClient({
      listRuns: async () => ["r1"],
      pricing: async () => ({
        version: "fixture-2026.06",
        effective_date: "2026-06-01",
        note: null,
      }),
      receipt: async (runId) => ({
        receipt: {},
        verify: {
          scope: `run:${runId}`,
          pricing_version: "fixture-2026.06",
          recomputed_total_micros: 1_920_000,
          rows: 2,
          flamegraph_checked: true,
          digest: 123,
        },
      }),
    });
    const root = document.createElement("div");
    await renderReceiptVerifier(root, client);
    expect(root.textContent).toContain("fixture-2026.06");

    (root.querySelector(".btn") as HTMLButtonElement).click();
    await new Promise((resolve) => setTimeout(resolve, 0));

    const ledger = root.querySelector(".receipt-ledger");
    expect(ledger?.textContent).toContain("Recomputed total");
    expect(ledger?.querySelector(".num")?.textContent).toBe("$1.92");
    expect(root.textContent).toContain("no network required");
  });

  it("surfaces a visible error when attestation fails", async () => {
    const client = fakeClient({
      listRuns: async () => ["r1"],
      receipt: async () => {
        throw new Error("no such run");
      },
    });
    const root = document.createElement("div");
    await renderReceiptVerifier(root, client);
    (root.querySelector("button") as HTMLButtonElement).click();
    await new Promise((r) => setTimeout(r, 0));
    const err = root.querySelector(".error");
    // Primary line is plain language; the raw error is demoted to the hover title.
    expect(err?.textContent).toContain("Couldn't attest this run");
    expect(err?.getAttribute("title")).toContain("no such run");
  });

  it("shows the empty state and no run selector when there are no runs", async () => {
    const root = document.createElement("div");
    await renderReceiptVerifier(root, fakeClient({ listRuns: async () => [] }));
    expect(root.querySelector(".empty-state")?.textContent).toContain("No runs to attest yet"); // emptyState
    expect(root.querySelector("select")).toBeNull();
  });

  it("distinguishes a run-list outage from genuinely no runs", async () => {
    const root = document.createElement("div");
    await renderReceiptVerifier(
      root,
      fakeClient({
        listRuns: async () => {
          throw new Error("capture service unreachable");
        },
      })
    );
    // Load failure → inline error, NOT the empty ("nothing to attest") state. Shared errorNode
    // contract: plain message visible, raw exception demoted to the hover title.
    expect(root.querySelector(".error")?.textContent).toContain("load failure");
    expect(root.querySelector(".error")?.getAttribute("title")).toContain("capture service unreachable");
    expect(root.querySelector(".empty")).toBeNull();
    expect(root.querySelector("select")).toBeNull();
  });

  it("omits the Pricing section when pricing lookup fails", async () => {
    const client = fakeClient({
      listRuns: async () => ["r1"],
      pricing: async () => {
        throw new Error("pricing unavailable");
      },
    });
    const root = document.createElement("div");
    await renderReceiptVerifier(root, client);
    expect(
      Array.from(root.querySelectorAll("h2")).some((h) => h.textContent === "Pricing")
    ).toBe(false);
    // The receipt section still renders.
    expect(
      Array.from(root.querySelectorAll("h2")).some((h) => h.textContent === "Cost receipt")
    ).toBe(true);
  });

  it("passes max_private=true to receipt() when the toggle is checked", async () => {
    const calls: Array<[string, boolean | undefined]> = [];
    const client = fakeClient({
      listRuns: async () => ["r1"],
      receipt: async (runId, maxPrivate) => {
        calls.push([runId, maxPrivate]);
        return {
          receipt: {},
          verify: {
            scope: "run:r1",
            pricing_version: "x",
            recomputed_total_micros: 0,
            rows: 0,
            flamegraph_checked: false,
            digest: 0,
          },
        };
      },
    });
    const root = document.createElement("div");
    await renderReceiptVerifier(root, client);
    (root.querySelector('input[type="checkbox"]') as HTMLInputElement).checked = true;
    (root.querySelector("button") as HTMLButtonElement).click();
    await new Promise((r) => setTimeout(r, 0));
    expect(calls).toEqual([["r1", true]]);
    // Typeset statement: flamegraph row reads "not checked" when not re-rendered.
    expect(root.querySelector(".receipt-ledger")?.textContent).toContain("not checked");
  });
});
