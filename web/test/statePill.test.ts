import { describe, it, expect } from "vitest";
import { statePill, runState } from "../src/ui/statePill.js";

describe("statePill", () => {
  it("renders a glyph + label with a severity class (glyph never hue-only)", () => {
    const p = statePill("working");
    expect(p.classList.contains("state-pill")).toBe(true);
    expect(p.classList.contains("state-ok")).toBe(true);
    expect(p.querySelector(".state-glyph")?.textContent).toBe("●");
    expect(p.textContent).toContain("Working"); // sentence case
  });

  it("maps run states to severities", () => {
    expect(statePill("over-cap").classList.contains("state-error")).toBe(true);
    expect(statePill("unpriced").classList.contains("state-warn")).toBe(true);
    expect(statePill("ended").classList.contains("state-neutral")).toBe(true);
  });

  it("defaults the tooltip to a DEFINITION, not a repeat of the label", () => {
    const unpriced = statePill("unpriced");
    // The hover explains the state instead of echoing "Unpriced".
    expect(unpriced.getAttribute("title")).toContain("No price found");
    expect(unpriced.getAttribute("title")).not.toBe("Unpriced");
    expect(statePill("settled").getAttribute("title")).toBe("Priced and finalized.");
    // An explicit title still overrides.
    expect(statePill("settled", "custom").getAttribute("title")).toBe("custom");
  });

  it("maps local-service states to severities", () => {
    expect(statePill("running").classList.contains("state-ok")).toBe(true);
    expect(statePill("degraded").classList.contains("state-warn")).toBe(true);
    const stopped = statePill("stopped");
    expect(stopped.classList.contains("state-neutral")).toBe(true);
    expect(stopped.textContent).toContain("Stopped");
  });
});

describe("runState", () => {
  it("prioritizes errored > over-cap > unpriced > settled", () => {
    // errored wins even when also over cap / unpriced.
    expect(runState({ micros: 9_000_000, unpriced: true, errored: true }, 1_000_000)).toBe("errored");
    // over cap beats unpriced.
    expect(runState({ micros: 2_000_000, unpriced: true }, 1_000_000)).toBe("over-cap");
    // unpriced when under cap.
    expect(runState({ micros: 500_000, unpriced: true }, 1_000_000)).toBe("unpriced");
    // no cap (0) never triggers over-cap.
    expect(runState({ micros: 9_000_000 }, 0)).toBe("settled");
    expect(runState({ micros: 500_000 }, 1_000_000)).toBe("settled");
  });
});
