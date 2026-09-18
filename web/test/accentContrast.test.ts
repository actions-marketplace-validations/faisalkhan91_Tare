import { describe, it, expect } from "vitest";
import {
  luminance,
  contrastRatio,
  hexToOklch,
  parseOklch,
  oklchToCss,
  clampAccentL,
  pickOnAccent,
  type Oklch,
} from "../src/ui/accentContrast.js";

// Ground-truth contrast ratios computed by scripts/verify-contrast.py's own lum/cr on the brand
// tokens. This cross-check is the guarantee that the runtime TS port agrees with the
// offline Python proof — the same "two impls, cross-verified" discipline the SVG export split uses.
const PY: Array<[string, Oklch, Oklch, number]> = [
  ["brass dark accent/surf", { L: 0.78, C: 0.13, H: 78 }, { L: 0.205, C: 0.009, H: 85 }, 8.822323],
  ["brass dark on-accent/accent", { L: 0.2, C: 0.03, H: 80 }, { L: 0.78, C: 0.13, H: 78 }, 8.929478],
  ["cool dark accent/surf", { L: 0.76, C: 0.115, H: 205 }, { L: 0.205, C: 0.009, H: 235 }, 8.695272],
  ["violet dark accent/surf", { L: 0.56, C: 0.2, H: 292 }, { L: 0.205, C: 0.009, H: 300 }, 3.537032],
  ["lime dark accent/surf", { L: 0.86, C: 0.18, H: 124 }, { L: 0.205, C: 0.009, H: 110 }, 12.150167],
  ["brass light accent/surf", { L: 0.62, C: 0.13, H: 72 }, { L: 1.0, C: 0, H: 0 }, 3.734298],
  ["brass light text/surf", { L: 0.27, C: 0.012, H: 85 }, { L: 1.0, C: 0, H: 0 }, 15.070188],
];

describe("accentContrast: cross-check vs verify-contrast.py", () => {
  for (const [label, fg, bg, expected] of PY) {
    it(`matches Python cr for ${label}`, () => {
      expect(contrastRatio(fg, bg)).toBeCloseTo(expected, 4);
    });
  }
  it("luminance is monotonic in L", () => {
    expect(luminance({ L: 0.2, C: 0, H: 0 })).toBeLessThan(luminance({ L: 0.8, C: 0, H: 0 }));
  });
});

describe("hexToOklch / parseOklch / oklchToCss", () => {
  it("white and black round-trip to the expected lightness poles", () => {
    expect(hexToOklch("#ffffff")!.L).toBeCloseTo(1, 2);
    expect(hexToOklch("#000000")!.L).toBeCloseTo(0, 2);
  });
  it("a saturated blue lands in the blue hue arc with real chroma", () => {
    const c = hexToOklch("#0a84ff")!; // macOS default blue accent
    expect(c.C).toBeGreaterThan(0.1);
    expect(c.H).toBeGreaterThan(230);
    expect(c.H).toBeLessThan(290);
  });
  it("rejects garbage and short/# forms", () => {
    expect(hexToOklch("nope")).toBeNull();
    expect(hexToOklch("#fff")!.L).toBeCloseTo(1, 2);
  });
  it("parses an oklch() token (as from a computed --surface)", () => {
    expect(parseOklch("oklch(0.205 0.009 85)")).toEqual({ L: 0.205, C: 0.009, H: 85 });
    expect(parseOklch("oklch(0.62 0.13 72 / 0.5)")!.L).toBeCloseTo(0.62, 3);
    expect(parseOklch("not a color")).toBeNull();
  });
  it("oklchToCss emits a valid oklch string", () => {
    expect(oklchToCss({ L: 0.5, C: 0.1, H: 200 })).toBe("oklch(0.5000 0.1000 200.00)");
  });
});

describe("clampAccentL", () => {
  const darkSurface: Oklch = { L: 0.205, C: 0.009, H: 85 };
  it("passes a high-contrast accent through unchanged", () => {
    const bright: Oklch = { L: 0.78, C: 0.13, H: 78 };
    expect(clampAccentL(bright, darkSurface, 3)).toEqual(bright);
  });
  it("raises a too-dark accent until it clears the floor against a dark surface", () => {
    const dim: Oklch = { L: 0.28, C: 0.13, H: 78 }; // barely above the surface → low contrast
    const fixed = clampAccentL(dim, darkSurface, 3)!;
    expect(fixed).not.toBeNull();
    expect(fixed.L).toBeGreaterThan(dim.L);
    expect(contrastRatio(fixed, darkSurface)).toBeGreaterThanOrEqual(3);
    expect(fixed.H).toBe(dim.H); // hue + chroma preserved
    expect(fixed.C).toBe(dim.C);
  });
  it("lowers a too-light accent against a light surface", () => {
    const lightSurface: Oklch = { L: 1, C: 0, H: 0 };
    const pale: Oklch = { L: 0.9, C: 0.13, H: 100 };
    const fixed = clampAccentL(pale, lightSurface, 3)!;
    expect(fixed.L).toBeLessThan(pale.L);
    expect(contrastRatio(fixed, lightSurface)).toBeGreaterThanOrEqual(3);
  });
});

describe("pickOnAccent", () => {
  it("picks black ink on a light accent and white ink on a dark accent", () => {
    expect(pickOnAccent({ L: 0.85, C: 0.15, H: 100 })).toEqual({ L: 0, C: 0, H: 0 }); // light lime → black
    expect(pickOnAccent({ L: 0.35, C: 0.15, H: 280 })).toEqual({ L: 1, C: 0, H: 0 }); // dark violet → white
  });
});
