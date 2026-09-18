import { describe, it, expect, afterEach } from "vitest";
import { installSlashSearch } from "../src/ui/slashSearch.js";

let off: (() => void) | null = null;
afterEach(() => {
  off?.();
  off = null;
  document.body.innerHTML = "";
});

function withSearch(): HTMLInputElement {
  const input = document.createElement("input");
  input.type = "search";
  document.body.appendChild(input);
  return input;
}

describe('installSlashSearch ("/" focuses search)', () => {
  it('focuses the search input on "/" and prevents the default', () => {
    const search = withSearch();
    off = installSlashSearch();
    const ev = new KeyboardEvent("keydown", { key: "/", cancelable: true });
    window.dispatchEvent(ev);
    expect(document.activeElement).toBe(search);
    expect(ev.defaultPrevented).toBe(true);
  });

  it('does not hijack "/" typed inside a text field', () => {
    withSearch();
    const text = document.createElement("input");
    text.type = "text";
    document.body.appendChild(text);
    text.focus();
    off = installSlashSearch();
    const ev = new KeyboardEvent("keydown", { key: "/", cancelable: true });
    text.dispatchEvent(ev); // event target is the focused text field
    expect(document.activeElement).toBe(text); // still in the text field
    expect(ev.defaultPrevented).toBe(false);
  });

  it("ignores Cmd//Ctrl-/ (modifier held)", () => {
    withSearch();
    off = installSlashSearch();
    const ev = new KeyboardEvent("keydown", { key: "/", metaKey: true, cancelable: true });
    window.dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(false);
  });

  it('lets "/" through when the screen has no search input', () => {
    off = installSlashSearch();
    const ev = new KeyboardEvent("keydown", { key: "/", cancelable: true });
    expect(() => window.dispatchEvent(ev)).not.toThrow();
    expect(ev.defaultPrevented).toBe(false);
  });

  it("unsubscribes cleanly", () => {
    const search = withSearch();
    const stop = installSlashSearch();
    stop();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "/", cancelable: true }));
    expect(document.activeElement).not.toBe(search);
  });
});
