// Tare Beam component. Deterministic fixtures for all four modes, asserting the
// acceptance: normative DOM, no lane mixes units or claims useful spend, partitions sum, the unpriced
// gap is detached, and semantics work without color (patterns + outline text). Byte-stable output.

import { describe, it, expect } from "vitest";
import { tareBeam, type BeamModel } from "../src/ui/tareBeam.js";

// ---- deterministic fixtures ------------------------------------------------------------------------

const PULSE: BeamModel = {
  mode: "pulse",
  title: "Today's spend",
  unit: "usd",
  segments: [
    { key: "recoverable", label: "Capped recoverable", value: 400_000, tone: "cost-warn" },
    { key: "remainder", label: "Remainder", value: 1_600_000, tone: "cost-ok" },
  ],
  gap: { label: "Unpriced usage", tokens: 82_000, share: 0.12 },
};

const RUN: BeamModel = {
  mode: "run",
  title: "Run profile",
  unit: "tokens",
  segments: [
    { key: "system", label: "System prompt", value: 12_000, tone: "cat-1" },
    { key: "tools", label: "Tool output", value: 6_000, tone: "cat-2" },
    { key: "output", label: "Model output", value: 2_000, tone: "cat-3" },
  ],
};

const COMPARE: BeamModel = {
  mode: "compare",
  title: "A vs baseline B",
  unit: "usd",
  segments: [
    { key: "cheaper", label: "Cache reuse", value: -300_000, tone: "cost-ok" },
    { key: "costlier", label: "Larger context", value: 120_000, tone: "cost-high" },
  ],
};

const OPTIMIZE: BeamModel = {
  mode: "optimize",
  title: "Expected savings",
  unit: "usd",
  segments: [
    { key: "open", label: "Open", value: 500_000, tone: "cost-warn" },
    { key: "applied", label: "Applied", value: 300_000, tone: "cost-ok" },
    { key: "verifying", label: "Verifying", value: 100_000, tone: "cat-4" },
  ],
  measured: { label: "Observed reduction", value: 220_000 },
};

const widthsPct = (root: HTMLElement): number[] =>
  Array.from(root.querySelectorAll(".tare-beam-track .tare-beam-seg")).map((s) =>
    parseFloat((s as HTMLElement).style.width)
  );

describe("Tare Beam — normative DOM + shared contract", () => {
  it("emits the normative section/heading/track/outline with a labelled, aria-hidden track", () => {
    const b = tareBeam(PULSE);
    expect(b.tagName).toBe("SECTION");
    expect(b.classList.contains("tare-beam")).toBe(true);
    const titleId = b.querySelector("h2")!.id;
    expect(b.getAttribute("aria-labelledby")).toBe(titleId);
    const track = b.querySelector(".tare-beam-track")!;
    expect(track.getAttribute("aria-hidden")).toBe("true"); // decorative; outline is the truth
    expect(b.querySelector("ol.tare-beam-outline")).toBeTruthy();
  });

  it("is byte-stable for a given model (deterministic id, no counters/Date/random)", () => {
    expect(tareBeam(PULSE).outerHTML).toBe(tareBeam(PULSE).outerHTML);
    expect(tareBeam(RUN).outerHTML).toBe(tareBeam(RUN).outerHTML);
  });

  it("ranks the outline by magnitude, descending", () => {
    const rows = Array.from(tareBeam(RUN).querySelectorAll(".tare-beam-outline .tare-beam-row"));
    const labels = rows.map((r) => r.textContent);
    expect(labels[0]).toContain("System prompt"); // 12000
    expect(labels[1]).toContain("Tool output"); // 6000
    expect(labels[2]).toContain("Model output"); // 2000
  });

  it("every segment carries a pattern class so semantics survive with no color", () => {
    const b = tareBeam(RUN);
    for (const seg of Array.from(b.querySelectorAll(".tare-beam-track .tare-beam-seg"))) {
      expect(/beam-pattern-\d/.test(seg.className)).toBe(true);
    }
    // The outline text alone conveys label + exact value + share (color-independent).
    expect(b.querySelector(".tare-beam-row")!.textContent).toMatch(/System prompt · 12,000 · \d/);
  });

  it("uses buttons only when selecting changes the analysis; plain text otherwise", () => {
    expect(tareBeam(RUN).querySelector("button.tare-beam-row")).toBeNull();
    const withSelect = tareBeam({ ...RUN, onSelect: () => {} });
    const btns = withSelect.querySelectorAll("button.tare-beam-row");
    expect(btns.length).toBe(RUN.segments.length);
  });
});

describe("Tare Beam — unit-lane + partition invariants", () => {
  it("partition segments sum to ~100% of the lane width (mutually exclusive, sum to total)", () => {
    const sum = widthsPct(tareBeam(RUN)).reduce((a, w) => a + w, 0);
    expect(sum).toBeCloseTo(100, 1);
  });

  it("a lane never mixes units — every value renders in the model's single unit", () => {
    // Pulse lane is USD: every outline row shows a $ value, none shows a raw token count.
    const rows = Array.from(tareBeam(PULSE).querySelectorAll(".tare-beam-outline .tare-beam-row"));
    for (const r of rows) expect(r.textContent).toMatch(/\$/);
    // Run lane is tokens: grouped integers, never a $.
    for (const r of Array.from(tareBeam(RUN).querySelectorAll(".tare-beam-row")))
      expect(r.textContent).not.toContain("$");
  });

  it("does not label the remainder 'useful spend' (Tare cannot infer utility)", () => {
    expect(tareBeam(PULSE).textContent!.toLowerCase()).not.toContain("useful");
  });
});

describe("Tare Beam — pulse: detached unpriced gap", () => {
  it("renders the unpriced usage OUTSIDE the lane, in tokens/share, never as a dollar segment", () => {
    const b = tareBeam(PULSE);
    const gap = b.querySelector(".tare-beam-gap")!;
    expect(gap).toBeTruthy();
    // The gap is a sibling of the track/outline, not inside them.
    expect(gap.closest(".tare-beam-track")).toBeNull();
    expect(gap.closest(".tare-beam-outline")).toBeNull();
    expect(gap.textContent).toContain("82,000 tokens");
    expect(gap.textContent).toContain("12.0% of tokens");
    expect(gap.textContent).toContain("not priced");
    expect(gap.textContent).not.toContain("$"); // never a fabricated dollar value
    // Only the two priced segments are in the lane — the gap is not one of them.
    expect(b.querySelectorAll(".tare-beam-track .tare-beam-seg").length).toBe(2);
  });

  it("renders a share-ONLY gap (pulse has no absolute unpriced count) without fabricating '0 tokens'", () => {
    const shareOnly: BeamModel = { ...PULSE, gap: { label: "Unpriced usage", share: 0.081 } };
    const gap = tareBeam(shareOnly).querySelector(".tare-beam-gap")!;
    expect(gap.textContent).toContain("8.1% of tokens");
    expect(gap.textContent).toContain("not priced");
    expect(gap.textContent).not.toContain("0 tokens"); // no fabricated absolute count
    expect(gap.textContent).not.toContain("$");
  });
});

describe("Tare Beam — compare: zero-centered delta", () => {
  it("centers the track and marks cheaper/costlier with signed, non-color labels", () => {
    const b = tareBeam(COMPARE);
    expect(b.querySelector(".tare-beam-track--centered")).toBeTruthy();
    expect(b.querySelector(".tare-beam-center")).toBeTruthy();
    expect(b.querySelector(".tare-beam-seg.beam-cheaper")).toBeTruthy();
    expect(b.querySelector(".tare-beam-seg.beam-costlier")).toBeTruthy();
    const text = b.querySelector(".tare-beam-outline")!.textContent!;
    expect(text).toContain("cheaper");
    expect(text).toContain("costlier");
  });
});

describe("Tare Beam — optimize: observed reduction is a SEPARATE marker", () => {
  it("keeps observed reduction out of the additive lane (not a segment, not in the outline)", () => {
    const b = tareBeam(OPTIMIZE);
    const measured = b.querySelector(".tare-beam-measured")!;
    expect(measured).toBeTruthy();
    expect(measured.textContent).toContain("Observed reduction");
    expect(measured.textContent).toContain("$0.22");
    // The lane partitions only the projected statuses; observed reduction is not one of the 3 segments.
    expect(b.querySelectorAll(".tare-beam-track .tare-beam-seg").length).toBe(3);
    const outline = b.querySelector(".tare-beam-outline")!.textContent!;
    expect(outline).not.toContain("Observed reduction");
    // The three status segments sum to the lane.
    const sum = widthsPct(b).reduce((a, w) => a + w, 0);
    expect(sum).toBeCloseTo(100, 1);
  });
});
