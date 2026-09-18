import { describe, it, expect } from "vitest";
import {
  fuzzyMatch,
  filterCommands,
  openPalette,
  installPaletteHotkey,
  type Command,
} from "../src/ui/palette.js";

describe("palette fuzzy", () => {
  it("subsequence-matches case-insensitively", () => {
    expect(fuzzyMatch("orun", "Open run · abc")).toBe(true);
    expect(fuzzyMatch("trends", "Go to Trends")).toBe(true);
    expect(fuzzyMatch("zzz", "Overview")).toBe(false);
    expect(fuzzyMatch("", "anything")).toBe(true);
  });
  it("filters the command list", () => {
    const cmds: Command[] = [
      { id: "a", title: "Go to Live", run: () => {} },
      { id: "b", title: "Go to Trends", run: () => {} },
    ];
    expect(filterCommands(cmds, "trend").map((c) => c.id)).toEqual(["b"]);
  });
  it("searches section, subtitle, and hidden aliases", () => {
    const cmds: Command[] = [
      {
        id: "saved",
        title: "Spend review",
        subtitle: "Investigate · 2026-07-01–2026-07-07",
        section: "Saved views",
        keywords: "cohort",
        run: () => {},
      },
    ];
    expect(filterCommands(cmds, "saved")).toHaveLength(1);
    expect(filterCommands(cmds, "2026-07")).toHaveLength(1);
    expect(filterCommands(cmds, "cohort")).toHaveLength(1);
  });
});

describe("openPalette", () => {
  it("renders items, filters on input, activates on Enter, closes on Escape", () => {
    let ran = "";
    const cmds: Command[] = [
      { id: "live", title: "Go to Live", run: () => { ran = "live"; } },
      { id: "trends", title: "Go to Trends", run: () => { ran = "trends"; } },
    ];
    const overlay = openPalette(cmds);
    expect(document.querySelector(".palette-overlay")).toBeTruthy();
    expect(overlay.querySelectorAll(".palette-item").length).toBe(2);

    // Filter to one.
    const input = overlay.querySelector("input") as HTMLInputElement;
    input.value = "trend";
    input.dispatchEvent(new Event("input"));
    expect(overlay.querySelectorAll(".palette-item").length).toBe(1);

    // Enter activates the (only) item and closes.
    overlay.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter" }));
    expect(ran).toBe("trends");
    expect(document.querySelector(".palette-overlay")).toBeNull();
  });

  it("shows a chord hint per row when provided", () => {
    const cmds: Command[] = [
      { id: "overview", title: "Go to Overview", run: () => {}, hint: "g o" },
      { id: "diff", title: "Go to Diff", run: () => {} }, // no hint
    ];
    const overlay = openPalette(cmds);
    const items = overlay.querySelectorAll(".palette-item");
    expect(items[0].querySelector(".palette-item-hint")?.textContent).toBe("g o");
    expect(items[0].querySelector(".palette-item-title")?.textContent).toBe("Go to Overview");
    expect(items[1].querySelector(".palette-item-hint")).toBeNull();
    overlay.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
  });

  it("renders labelled groups and row metadata", () => {
    const overlay = openPalette([
      { id: "pulse", title: "Go to Pulse", section: "Primary navigation", run: () => {} },
      {
        id: "run",
        title: "run-42",
        subtitle: "claude-sonnet-4 · 2026-07-14",
        section: "Recent runs",
        run: () => {},
      },
    ]);
    expect(Array.from(overlay.querySelectorAll(".palette-group-label")).map((node) => node.textContent)).toEqual([
      "Primary navigation",
      "Recent runs",
    ]);
    expect(overlay.querySelector(".palette-item-subtitle")?.textContent).toContain("claude-sonnet-4");
    overlay.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
  });

  it("arrow-down moves selection; Escape closes without running", () => {
    let ran = false;
    const cmds: Command[] = [
      { id: "a", title: "Alpha", run: () => { ran = true; } },
      { id: "b", title: "Beta", run: () => { ran = true; } },
    ];
    const overlay = openPalette(cmds);
    overlay.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown" }));
    expect(overlay.querySelectorAll(".palette-item")[1].classList.contains("active")).toBe(true);
    overlay.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(document.querySelector(".palette-overlay")).toBeNull();
    expect(ran).toBe(false);
  });

  it("Home/End jump the selection to first/last", () => {
    const cmds: Command[] = [
      { id: "a", title: "Alpha", run: () => {} },
      { id: "b", title: "Beta", run: () => {} },
      { id: "c", title: "Gamma", run: () => {} },
    ];
    const overlay = openPalette(cmds);
    const items = () => overlay.querySelectorAll(".palette-item");
    overlay.dispatchEvent(new KeyboardEvent("keydown", { key: "End" }));
    expect(items()[2].classList.contains("active")).toBe(true);
    overlay.dispatchEvent(new KeyboardEvent("keydown", { key: "Home" }));
    expect(items()[0].classList.contains("active")).toBe(true);
    overlay.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
  });

  it("exposes modal dialog semantics: aria-modal, label, activedescendant, traps Tab", () => {
    const overlay = openPalette([
      { id: "a", title: "Alpha", run: () => {} },
      { id: "b", title: "Beta", run: () => {} },
    ]);
    expect(overlay.getAttribute("role")).toBe("dialog");
    expect(overlay.getAttribute("aria-modal")).toBe("true");
    expect(overlay.getAttribute("aria-label")).toBe("Command palette");
    const input = overlay.querySelector("input") as HTMLInputElement;
    // Active option is advertised via aria-activedescendant + aria-selected.
    expect(input.getAttribute("aria-activedescendant")).toBe("palette-opt-0");
    expect(overlay.querySelector("#palette-opt-0")?.getAttribute("aria-selected")).toBe("true");
    expect(overlay.querySelector("#palette-opt-1")?.getAttribute("aria-selected")).toBe("false");
    // Tab is trapped (default prevented) rather than escaping the modal.
    const ev = new KeyboardEvent("keydown", { key: "Tab", cancelable: true });
    overlay.dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(true);
    overlay.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
  });

  it("restores focus to the opener element on close (a11y)", () => {
    const opener = document.createElement("button");
    document.body.appendChild(opener);
    opener.focus();
    expect(document.activeElement).toBe(opener);

    const overlay = openPalette([{ id: "a", title: "Alpha", run: () => {} }]);
    overlay.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(document.activeElement).toBe(opener);
    opener.remove();
  });
});

describe("installPaletteHotkey (OS-aware Mod+K)", () => {
  it("on macOS opens on ⌘K but NOT Ctrl-K (leaves the native readline chord alone)", () => {
    let opens = 0;
    const off = installPaletteHotkey(() => opens++, window, "macos");
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "k", metaKey: true }));
    expect(opens).toBe(1);
    // Ctrl-K on macOS must pass through to the OS, not summon the palette.
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "k", ctrlKey: true }));
    expect(opens).toBe(1);
    off();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "k", metaKey: true }));
    expect(opens).toBe(1);
  });

  it("on Windows/Linux opens on Ctrl-K but NOT ⌘/Meta-K", () => {
    let opens = 0;
    const off = installPaletteHotkey(() => opens++, window, "windows");
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "k", ctrlKey: true }));
    expect(opens).toBe(1);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "k", metaKey: true }));
    expect(opens).toBe(1);
    off();
  });
});
