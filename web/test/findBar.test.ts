// In-webview Find bar: the cross-platform replacement for the dead Cmd/Ctrl-F.
// Verifies the full acceptance — opens, highlights + counts matches, next/prev cycles, Enter/Esc, and
// closing restores the DOM byte-for-byte — plus the desktop-only hotkey gate.

import { describe, it, expect, afterEach } from "vitest";
import { openFind, closeFind, installFindHotkey } from "../src/ui/findBar.js";

function seedMain(html: string): HTMLElement {
  const main = document.createElement("main");
  main.id = "main";
  main.innerHTML = html;
  document.body.appendChild(main);
  return main;
}

afterEach(() => {
  closeFind();
  document.body.querySelector("#main")?.remove();
  document.getElementById("tare-find-bar")?.remove();
});

const bar = () => document.getElementById("tare-find-bar");
const input = () => bar()?.querySelector<HTMLInputElement>(".find-input") ?? null;
const type = (q: string) => {
  const i = input()!;
  i.value = q;
  i.dispatchEvent(new Event("input"));
};

describe("find bar", () => {
  it("opens over #main, focuses the input, and is idempotent", () => {
    seedMain("<p>alpha beta gamma</p>");
    openFind();
    expect(bar()).toBeTruthy();
    expect(document.activeElement).toBe(input());
    openFind(); // second call must not stack a second bar
    expect(document.querySelectorAll("#tare-find-bar").length).toBe(1);
  });

  it("highlights every case-insensitive match and reports n/N", () => {
    seedMain("<p>Cache cache CACHE miss</p>");
    openFind();
    type("cache");
    const hits = document.querySelectorAll("mark.find-hit");
    expect(hits.length).toBe(3);
    expect(bar()!.querySelector(".find-status")!.textContent).toBe("1/3");
    // First hit is the current one.
    expect(document.querySelectorAll("mark.find-hit.current").length).toBe(1);
    expect(hits[0].classList.contains("current")).toBe(true);
  });

  it("next/prev cycles the current match (wrapping), via buttons and Enter/Shift+Enter", () => {
    seedMain("<p>x x x</p>");
    openFind();
    type("x");
    const cur = () => Array.from(document.querySelectorAll("mark.find-hit")).findIndex((m) => m.classList.contains("current"));
    expect(cur()).toBe(0);
    (bar()!.querySelector(".find-next") as HTMLButtonElement).click();
    expect(cur()).toBe(1);
    // Enter advances; Shift+Enter retreats; both wrap.
    input()!.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(cur()).toBe(2);
    input()!.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(cur()).toBe(0); // wrapped forward
    input()!.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", shiftKey: true, bubbles: true }));
    expect(cur()).toBe(2); // wrapped backward
  });

  it("reports No results and adds no marks when nothing matches", () => {
    seedMain("<p>alpha</p>");
    openFind();
    type("zzz");
    expect(document.querySelectorAll("mark.find-hit").length).toBe(0);
    expect(bar()!.querySelector(".find-status")!.textContent).toBe("No results");
  });

  it("does not search the chrome outside #main", () => {
    const rail = document.createElement("nav");
    rail.textContent = "needle in the rail";
    document.body.appendChild(rail);
    seedMain("<p>needle in main</p>");
    openFind();
    type("needle");
    // Only the #main occurrence is highlighted; the rail is untouched.
    expect(document.querySelectorAll("mark.find-hit").length).toBe(1);
    expect(document.querySelector("#main mark.find-hit")).toBeTruthy();
    rail.remove();
  });

  it("Esc closes the bar and restores the DOM byte-for-byte (no mark residue)", () => {
    const main = seedMain("<p>find me here</p>");
    const before = main.innerHTML;
    openFind();
    type("me");
    expect(document.querySelectorAll("mark.find-hit").length).toBe(1);
    input()!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(bar()).toBeNull();
    expect(document.querySelectorAll("mark.find-hit").length).toBe(0);
    expect(main.innerHTML).toBe(before); // original markup restored exactly
  });

  it("hotkey is desktop-gated: Mod+F opens on desktop, is left to native find in the browser", () => {
    seedMain("<p>text</p>");
    // Browser (isDesktop=false): no listener installed, Ctrl+F does nothing here.
    const offBrowser = installFindHotkey(window, "windows", false);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "f", ctrlKey: true }));
    expect(bar()).toBeNull();
    offBrowser();
    // Desktop (isDesktop=true): Ctrl+F opens the bar.
    const offDesktop = installFindHotkey(window, "windows", true);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "f", ctrlKey: true }));
    expect(bar()).toBeTruthy();
    offDesktop();
  });
});
