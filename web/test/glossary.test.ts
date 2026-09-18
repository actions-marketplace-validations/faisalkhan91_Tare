import { describe, it, expect } from "vitest";
import { GLOSSARY, definitionOf, defineTerm } from "../src/ui/glossary.js";

describe("glossary definitions at point of use", () => {
  it("looks up definitions case-insensitively; unknown → undefined", () => {
    expect(definitionOf("Cache Read")).toBe(GLOSSARY["cache read"]);
    expect(definitionOf("  TTL  ")).toBe(GLOSSARY["ttl"]);
    expect(definitionOf("not-a-term")).toBeUndefined();
  });

  it("defineTerm renders an accessible, click-toggled definition (not hover-only)", () => {
    const node = defineTerm("cache read");
    expect(node.textContent).toContain("cache read");
    const btn = node.querySelector("button.glossary-info") as HTMLButtonElement;
    const panel = node.querySelector(".glossary-def") as HTMLElement;
    expect(btn).toBeTruthy();
    expect(panel).toBeTruthy();
    // Wired for a screen reader + collapsed by default.
    expect(btn.getAttribute("aria-controls")).toBe(panel.id);
    expect(btn.getAttribute("aria-label")).toBe("Define: cache read");
    expect(btn.getAttribute("aria-expanded")).toBe("false");
    expect(panel.hasAttribute("hidden")).toBe(true);
    expect(panel.textContent).toContain("prompt cache");
    // Click reveals it; click again hides.
    btn.click();
    expect(btn.getAttribute("aria-expanded")).toBe("true");
    expect(panel.hasAttribute("hidden")).toBe(false);
    btn.click();
    expect(panel.hasAttribute("hidden")).toBe(true);
  });

  it("supports a custom label and gives each instance a unique panel id", () => {
    const a = defineTerm("frontier", "the frontier");
    const b = defineTerm("frontier", "frontier");
    expect(a.textContent).toContain("the frontier");
    const idA = a.querySelector(".glossary-def")!.id;
    const idB = b.querySelector(".glossary-def")!.id;
    expect(idA).not.toBe(idB); // no duplicate ids when the same term appears twice
  });

  it("renders an unknown term as plain text — no dangling affordance", () => {
    const node = defineTerm("totally-unknown", "Totally unknown");
    expect(node.textContent).toBe("Totally unknown");
    expect(node.querySelector("button")).toBeNull();
  });
});
