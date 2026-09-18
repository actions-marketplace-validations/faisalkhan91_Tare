import { describe, it, expect, afterEach } from "vitest";
import { installCommandKey } from "../src/ui/commandKey.js";

let off: (() => void) | null = null;
afterEach(() => {
  off?.();
  off = null;
  document.body.innerHTML = "";
});

describe('installCommandKey (":" opens the command surface)', () => {
  it('opens on ":" and prevents the default', () => {
    let opens = 0;
    off = installCommandKey(() => opens++);
    const ev = new KeyboardEvent("keydown", { key: ":", shiftKey: true, cancelable: true });
    window.dispatchEvent(ev);
    expect(opens).toBe(1);
    expect(ev.defaultPrevented).toBe(true);
  });

  it('does not hijack ":" typed inside a text field', () => {
    let opens = 0;
    const input = document.createElement("input");
    input.type = "text";
    document.body.appendChild(input);
    input.focus();
    off = installCommandKey(() => opens++);
    const ev = new KeyboardEvent("keydown", { key: ":", shiftKey: true, cancelable: true });
    input.dispatchEvent(ev);
    expect(opens).toBe(0);
    expect(ev.defaultPrevented).toBe(false);
  });

  it("ignores Cmd/Ctrl/Alt-: (a real chord, not the terminal key)", () => {
    let opens = 0;
    off = installCommandKey(() => opens++);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: ":", ctrlKey: true }));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: ":", metaKey: true }));
    expect(opens).toBe(0);
  });

  it("unsubscribes cleanly", () => {
    let opens = 0;
    const stop = installCommandKey(() => opens++);
    stop();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: ":", shiftKey: true }));
    expect(opens).toBe(0);
  });
});
