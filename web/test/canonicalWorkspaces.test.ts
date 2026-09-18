// Canonical workspace routing + lazy loading. Canonical routes render real content, the initial boot
// module does not statically import any workspace, and legacy routes keep working through redirects.

import { describe, it, expect, beforeEach } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { mountApp } from "../src/main.js";
import { fakeClient } from "./fakeClient.js";
import { setOnboarded } from "../src/ui/prefs.js";
import type { TareClient } from "../src/client.js";

function golden(name: string): string {
  return readFileSync(resolve(process.cwd(), "../tare-core/tests/golden", name), "utf8");
}

function populated(): TareClient {
  return fakeClient({
    listRuns: async () => ["run-a", "run-b"],
    flamegraph: async () => JSON.parse(golden("flamegraph_bloated.json")),
    report: async () => JSON.parse(golden("report.json")),
    trend: async () => JSON.parse(golden("trend.json")),
    today: async () => ({ total_micros: 4_210_000, pricing_version: "x", effective_date: "d" }),
    runStatus: async (id: string) => ({ run_id: id, micros: 1_920_000, steps: 3, top_cause: "bloated-system-prompt" }),
    runStatuses: async () => [
      { run_id: "run-a", micros: 1_920_000, steps: 3, top_cause: "bloated-system-prompt" },
      { run_id: "run-b", micros: 1_920_000, steps: 3, top_cause: "bloated-system-prompt" },
    ],
    explain: async () => "Mostly an uncached system prompt.",
    rollup: async () => ({
      dimension: "step",
      rows: [{ label: "planner", runs: 1, steps: 3, tokens: 1000, micros: 500000, micros_per_call: 166666 }],
      total_micros: 500000,
      pricing_version: "x",
      estimated: true,
    }),
    advise: async () => [],
    whatif: async () => ({ baseline_micros: 1000, recommendations: [], estimated: true, approximate: true }),
    config: async () => ({ budget: { max_spend_usd: 10 }, privacy: {}, providers: {}, proxy: {} }),
    anomalies: async () => [],
  });
}

// Canonical routes and the query variants their redirect targets carry.
const CANONICAL = [
  "#/pulse",
  "#/pulse?mode=now",
  "#/investigate",
  "#/investigate?entity=sessions",
  "#/investigate?mode=timeline",
  "#/investigate?view=facets",
  "#/investigate/run/run-a",
  "#/investigate/compare",
  "#/optimize",
  "#/optimize?view=scenarios",
  "#/optimize?type=cache",
  "#/pulse?sheet=settings",
  "#/pulse?sheet=capture",
  "#/pulse?sheet=trust",
  "#/pulse?sheet=trust&view=pricing",
];

describe("canonical workspaces render lazily", () => {
  beforeEach(() => setOnboarded(true));

  for (const hash of CANONICAL) {
    it(`${hash} renders an adapter, not a placeholder`, async () => {
      location.hash = hash;
      const root = document.createElement("div");
      document.body.appendChild(root);
      await mountApp(root, populated());
      await new Promise((r) => setTimeout(r, 10)); // settle the lazy import + async screen body
      const main = root.querySelector(".main")!;
      expect(main).toBeTruthy();
      expect(main.textContent?.length).toBeGreaterThan(0);
      expect(main.querySelector(".error"), `${hash} showed an error pane`).toBeNull();
      expect(main.textContent ?? "", `${hash} hit the not-found placeholder`).not.toMatch(
        /page not found/i
      );
      root.remove();
      location.hash = "";
    });
  }

  it("the boot module does not statically import a workspace; the workbench loads them lazily", () => {
    const mainSrc = readFileSync(resolve(process.cwd(), "src/main.ts"), "utf8");
    const workbenchSrc = readFileSync(resolve(process.cwd(), "src/shell/workbench.ts"), "utf8");
    // No static `import ... from ".../workspaces/..."` anywhere in the boot module.
    expect(mainSrc).not.toMatch(/^\s*import\s[^\n]*["'][^"']*workspaces\//m);
    // The workbench is the one place that pulls workspaces — via dynamic import().
    expect(workbenchSrc).toMatch(/import\(\s*["']\.\.\/workspaces\//);
    expect(mainSrc).not.toMatch(/import\s+\{\s*renderSettings\s*\}/);
    expect(workbenchSrc).toMatch(/import\(\s*["']\.\.\/screens\/settings/);
    expect(mainSrc).not.toMatch(/import\s+\{\s*renderConnect\s*\}/);
    expect(workbenchSrc).toMatch(/import\(\s*["']\.\.\/screens\/capture/);
    expect(mainSrc).not.toMatch(/import\s+\{\s*render(?:Pricing|Receipts)\s*\}/);
    expect(workbenchSrc).toMatch(/import\(\s*["']\.\.\/screens\/trust/);
  });

  it("renders Settings as a modal utility sheet and Escape returns to the current workspace", async () => {
    location.hash = "#/investigate?entity=runs&sheet=settings";
    const root = document.createElement("div");
    document.body.appendChild(root);
    await mountApp(root, populated());
    await new Promise((resolve) => setTimeout(resolve, 20));
    const dialog = root.querySelector('[role="dialog"][aria-modal="true"]') as HTMLElement;
    expect(dialog).toBeTruthy();
    expect(dialog.getAttribute("aria-labelledby")).toBeTruthy();
    expect(dialog.querySelector("h1")?.textContent).toBe("Settings");
    expect(dialog.querySelector("button")?.textContent).toContain("Close");
    dialog.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(location.hash).toContain("#/investigate?");
    expect(location.hash).toContain("entity=runs");
    expect(location.hash).not.toContain("sheet=");
    expect(location.hash).toContain("from=");
    expect(root.querySelector('[role="dialog"]')).toBeNull();
    root.remove();
  });

  it("redirects the legacy Settings route to the canonical Pulse sheet", async () => {
    location.hash = "#/settings";
    const root = document.createElement("div");
    document.body.appendChild(root);
    await mountApp(root, populated());
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(location.hash).toBe("#/pulse?sheet=settings");
    expect(root.querySelector('[role="dialog"][aria-modal="true"]')).toBeTruthy();
    root.remove();
  });

  it("opens Settings from the keyboard without leaving the current workspace", async () => {
    location.hash = "#/optimize?view=scenarios";
    const root = document.createElement("div");
    document.body.appendChild(root);
    await mountApp(root, populated());
    window.dispatchEvent(new KeyboardEvent("keydown", { key: ",", ctrlKey: true, bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(location.hash).toBe("#/optimize?sheet=settings&view=scenarios");
    expect(root.querySelector('[role="dialog"][aria-modal="true"]')).toBeTruthy();
    root.remove();
  });

  it("renders Trust as a modal utility sheet and Escape preserves the current workspace", async () => {
    location.hash = "#/investigate?entity=runs&sheet=trust";
    const root = document.createElement("div");
    document.body.appendChild(root);
    await mountApp(root, populated());
    await new Promise((resolve) => setTimeout(resolve, 20));
    const dialog = root.querySelector('[role="dialog"][aria-modal="true"]') as HTMLElement;
    expect(dialog.querySelector("h1")?.textContent).toBe("Trust & pricing");
    expect(dialog.textContent).toContain("Trust overview");
    dialog.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(location.hash).toContain("#/investigate?");
    expect(location.hash).toContain("entity=runs");
    expect(location.hash).not.toContain("sheet=");
    expect(location.hash).toContain("from=");
    root.remove();
  });

  it("renders Capture as a modal utility sheet and Escape preserves the current workspace", async () => {
    location.hash = "#/optimize?sheet=capture&type=cache";
    const root = document.createElement("div");
    document.body.appendChild(root);
    await mountApp(root, populated());
    await new Promise((resolve) => setTimeout(resolve, 20));
    const dialog = root.querySelector('[role="dialog"][aria-modal="true"]') as HTMLElement;
    expect(dialog.querySelector("h1")?.textContent).toBe("Capture");
    expect(dialog.textContent).toContain("Current capture");
    dialog.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(location.hash).toBe("#/optimize?type=cache");
    root.remove();
  });

  for (const [legacy, canonical] of [
    ["#/connect", "#/pulse?sheet=capture"],
    ["#/receipts", "#/pulse?sheet=trust"],
    ["#/pricing", "#/pulse?sheet=trust&view=pricing"],
  ]) {
    it(`redirects ${legacy} to ${canonical}`, async () => {
      location.hash = legacy;
      const root = document.createElement("div");
      document.body.appendChild(root);
      await mountApp(root, populated());
      await new Promise((resolve) => setTimeout(resolve, 20));
      expect(location.hash).toBe(canonical);
      expect(root.querySelector('[role="dialog"][aria-modal="true"]')).toBeTruthy();
      root.remove();
    });
  }
});
