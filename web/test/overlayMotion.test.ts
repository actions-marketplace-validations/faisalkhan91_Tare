import { afterEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { createUtilitySheet } from "../src/ui/sheet.js";

afterEach(() => {
  document.body.replaceChildren();
  vi.unstubAllGlobals();
});

describe("edge-panel motion lifecycle", () => {
  it("commits the opening state for a frame before settling open", async () => {
    let frame: FrameRequestCallback | undefined;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    const sheet = createUtilitySheet("Trust & pricing", () => undefined);
    document.body.appendChild(sheet.backdrop);

    expect(sheet.backdrop.dataset.motionState).toBe("opening");
    await Promise.resolve();
    expect(frame).toBeTypeOf("function");
    frame?.(0);
    expect(sheet.backdrop.dataset.motionState).toBe("open");
  });

  it("plays the closing state before invoking route-owned teardown", async () => {
    let closes = 0;
    const sheet = createUtilitySheet("Settings", () => { closes += 1; });
    document.body.appendChild(sheet.backdrop);
    sheet.dialog.style.transitionProperty = "transform";
    sheet.dialog.style.transitionDuration = "180ms";
    sheet.dialog.style.transitionDelay = "0ms";
    const close = sheet.dialog.querySelector<HTMLButtonElement>(".utility-sheet-close")!;

    close.click();
    expect(sheet.backdrop.dataset.motionState).toBe("closing");
    expect(closes).toBe(0);

    const ended = new Event("transitionend", { bubbles: true });
    Object.defineProperty(ended, "propertyName", { value: "transform" });
    sheet.dialog.dispatchEvent(ended);
    expect(closes).toBe(1);

    // Repeated close signals and a late transition event cannot tear down twice.
    close.click();
    sheet.dialog.dispatchEvent(ended);
    expect(closes).toBe(1);
  });

  it("tears down on the next microtask when CSS motion is unavailable", async () => {
    let closes = 0;
    const sheet = createUtilitySheet("Capture", () => { closes += 1; });
    document.body.appendChild(sheet.backdrop);
    sheet.dialog.querySelector<HTMLButtonElement>(".utility-sheet-close")!.click();
    expect(closes).toBe(0);
    await Promise.resolve();
    expect(closes).toBe(1);
  });
});

describe("edge-panel motion styling", () => {
  const components = readFileSync(resolve(process.cwd(), "src/ui/components.css"), "utf8");
  const shell = readFileSync(resolve(process.cwd(), "src/ui/shell.css"), "utf8");
  const tokens = readFileSync(resolve(process.cwd(), "src/ui/tokens.css"), "utf8");

  it("mirrors a restrained transform from the panel's physical edge", () => {
    expect(components).toMatch(/data-motion-state="opening"[\s\S]*?translateX\(24px\)/);
    expect(shell).toMatch(/data-nav-drawer-state[\s\S]*?translateX\(-24px\)/);
  });

  it("keeps the panel opaque and the app behind it paint-stable", () => {
    expect(components).not.toMatch(/utility-sheet[^}]*opacity\s*:/);
    expect(components).not.toMatch(/utility-sheet[^}]*will-change\s*:/);
    expect(components).not.toMatch(/utility-sheet-backdrop[^}]*transition\s*:/);
    expect(shell).not.toMatch(/nav-drawer-backdrop[^}]*transition\s*:/);
    expect(shell).not.toMatch(/data-nav-drawer-state[^}]*will-change\s*:/);
  });

  it("uses semantic enter/exit timing and globally honors reduced motion", () => {
    expect(tokens).toMatch(/--dur-panel-enter:\s*240ms/);
    expect(tokens).toMatch(/--dur-panel-exit:\s*180ms/);
    expect(tokens).toMatch(/@media\s*\(prefers-reduced-motion:\s*reduce\)[\s\S]*?transition:\s*none !important/);
  });
});
