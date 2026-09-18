import { describe, it, expect, beforeEach } from "vitest";
import { applySystemAccent, systemAccentEligible } from "../src/ui/systemAccent.js";

// A fresh <html>-like element carrying a dark surface, so readSurface has something to clamp against
// (jsdom won't resolve --surface from a stylesheet).
function darkRoot(): HTMLElement {
  const root = document.createElement("html");
  root.style.setProperty("--surface", "oklch(0.205 0.009 85)");
  return root;
}

// brass is the committed brand accent, so the OS-accent bridge is OPT-IN and OFF by
// default; it activates only when the "tare-os-accent" pref is "on".
const optIn = (): void => localStorage.setItem("tare-os-accent", "on");

describe("systemAccent bridge (opt-in)", () => {
  beforeEach(() => localStorage.clear());

  it("is off by default and eligible only once opted in", () => {
    expect(systemAccentEligible()).toBe(false); // brass is the brand out of the box
    optIn();
    expect(systemAccentEligible()).toBe(true);
  });

  it("applies a clamped OS accent to the affordance tokens when opted in", () => {
    optIn();
    const root = darkRoot();
    expect(applySystemAccent("#0a84ff", root)).toBe(true);
    expect(root.dataset.systemAccent).toBe("on");
    expect(root.style.getPropertyValue("--accent-system")).toMatch(/^oklch\(/);
    expect(root.style.getPropertyValue("--on-accent-system")).toMatch(/^oklch\(/);
    expect(root.style.getPropertyValue("--focus-ring")).toMatch(/^oklch\(/);
  });

  it("does NOT apply by default (brass stays the brand)", () => {
    const root = darkRoot();
    expect(applySystemAccent("#0a84ff", root)).toBe(false);
    expect(root.dataset.systemAccent).toBeUndefined();
    expect(root.style.getPropertyValue("--accent-system")).toBe("");
  });

  it("clears the bridge on a null accent (e.g. Reduce-Transparency / no OS accent)", () => {
    optIn();
    const root = darkRoot();
    applySystemAccent("#0a84ff", root);
    expect(root.dataset.systemAccent).toBe("on");
    applySystemAccent(null, root);
    expect(root.dataset.systemAccent).toBeUndefined();
    expect(root.style.getPropertyValue("--focus-ring")).toBe("");
  });

  it("no-ops (stays brass) on a garbage accent or with no surface to clamp against", () => {
    optIn();
    const root = darkRoot();
    expect(applySystemAccent("not-a-hex", root)).toBe(false);
    const noSurface = document.createElement("html");
    expect(applySystemAccent("#0a84ff", noSurface)).toBe(false);
  });
});
