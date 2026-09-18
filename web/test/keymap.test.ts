import { describe, it, expect } from "vitest";
import { matchShortcut, formatShortcut, type Chorded } from "../src/ui/keymap.js";

const ev = (o: Partial<Chorded> & { key: string }): Chorded => ({
  metaKey: false,
  ctrlKey: false,
  shiftKey: false,
  altKey: false,
  ...o,
});

describe("matchShortcut (Mod token)", () => {
  it("maps Mod to Command on macOS, requiring Ctrl to be UP", () => {
    expect(matchShortcut(ev({ key: "k", metaKey: true }), "Mod+K", "macos")).toBe(true);
    // The bug being fixed: Ctrl-K on macOS must NOT match Mod+K.
    expect(matchShortcut(ev({ key: "k", ctrlKey: true }), "Mod+K", "macos")).toBe(false);
    // Both held (e.g. a stray combo) also fails — Ctrl must be up.
    expect(matchShortcut(ev({ key: "k", metaKey: true, ctrlKey: true }), "Mod+K", "macos")).toBe(false);
  });

  it("maps Mod to Control on Windows/Linux, requiring Meta to be UP", () => {
    for (const os of ["windows", "linux", "other"] as const) {
      expect(matchShortcut(ev({ key: "k", ctrlKey: true }), "Mod+K", os)).toBe(true);
      expect(matchShortcut(ev({ key: "k", metaKey: true }), "Mod+K", os)).toBe(false);
    }
  });

  it("is case-insensitive on the key and matches when Shift is or isn't part of the chord", () => {
    expect(matchShortcut(ev({ key: "K", metaKey: true }), "Mod+K", "macos")).toBe(true);
    expect(matchShortcut(ev({ key: "p", metaKey: true, shiftKey: true }), "Mod+Shift+P", "macos")).toBe(true);
    // Shift held but not requested → no match.
    expect(matchShortcut(ev({ key: "k", metaKey: true, shiftKey: true }), "Mod+K", "macos")).toBe(false);
    // Shift requested but not held → no match.
    expect(matchShortcut(ev({ key: "p", metaKey: true }), "Mod+Shift+P", "macos")).toBe(false);
  });

  it("a modifier-less chord requires no platform modifier held", () => {
    expect(matchShortcut(ev({ key: "/" }), "/", "macos")).toBe(true);
    expect(matchShortcut(ev({ key: "/", metaKey: true }), "/", "macos")).toBe(false);
    expect(matchShortcut(ev({ key: "/", ctrlKey: true }), "/", "windows")).toBe(false);
  });
});

describe("formatShortcut", () => {
  it("renders tight glyphs on macOS", () => {
    expect(formatShortcut("Mod+K", "macos")).toBe("⌘K");
    expect(formatShortcut("Mod+Shift+P", "macos")).toBe("⇧⌘P");
    expect(formatShortcut("Mod+,", "macos")).toBe("⌘,");
  });

  it("renders spaced words on Windows/Linux", () => {
    expect(formatShortcut("Mod+K", "windows")).toBe("Ctrl K");
    expect(formatShortcut("Mod+Shift+P", "linux")).toBe("Shift Ctrl P");
    expect(formatShortcut("Mod+K", "other")).toBe("Ctrl K");
  });
});
