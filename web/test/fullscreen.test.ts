import { describe, it, expect, afterEach } from "vitest";
import { wireFullscreen } from "../src/bootTauri.js";

afterEach(() => delete document.documentElement.dataset.fullscreen);

function fakeWin(fs: boolean) {
  let onResized: (() => void) | null = null;
  return {
    win: {
      isFullscreen: () => Promise.resolve(fs),
      onResized: (cb: () => void) => {
        onResized = cb;
      },
    },
    fire: () => onResized?.(),
  };
}

describe("wireFullscreen", () => {
  it("sets data-fullscreen=true when the window is fullscreen", async () => {
    const f = fakeWin(true);
    wireFullscreen(() => f.win);
    await new Promise((r) => setTimeout(r, 0));
    expect(document.documentElement.dataset.fullscreen).toBe("true");
  });

  it("leaves the flag unset when not fullscreen, and re-syncs on resize", async () => {
    let fs = false;
    let onResized: (() => void) | null = null;
    const win = {
      isFullscreen: () => Promise.resolve(fs),
      onResized: (cb: () => void) => {
        onResized = cb;
      },
    };
    wireFullscreen(() => win);
    await new Promise((r) => setTimeout(r, 0));
    expect(document.documentElement.dataset.fullscreen).toBeUndefined();
    // Enter fullscreen → the resize event re-syncs the flag.
    fs = true;
    (onResized as (() => void) | null)?.();
    await new Promise((r) => setTimeout(r, 0));
    expect(document.documentElement.dataset.fullscreen).toBe("true");
  });

  it("is a no-op with no Tauri window (browser transport)", () => {
    expect(() => wireFullscreen(() => undefined)).not.toThrow();
  });
});
