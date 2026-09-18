import { describe, it, expect } from "vitest";
import { chordState, installHotkeys, type Hotkey } from "../src/ui/hotkeys.js";

const binds = (log: string[]): Hotkey[] => [
  { seq: "g o", label: "Overview", run: () => log.push("overview") },
  { seq: "g r", label: "Runs", run: () => log.push("runs") },
];

describe("chordState (pure matcher)", () => {
  const b = binds([]);
  it("classifies exact / prefix / dead-end", () => {
    expect(chordState(b, ["g"]).status).toBe("prefix");
    expect(chordState(b, ["g", "o"]).status).toBe("exact");
    expect(chordState(b, ["g", "o"]).binding?.label).toBe("Overview");
    expect(chordState(b, ["g", "x"]).status).toBe("none");
    expect(chordState(b, ["x"]).status).toBe("none");
    expect(chordState(b, []).status).toBe("none");
  });
});

describe("installHotkeys (leader chords)", () => {
  const dispatch = (el: EventTarget, key: string, mods: Partial<KeyboardEvent> = {}): void => {
    el.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, ...mods }));
  };

  it("runs a chord (g then o) and not a dead-end (g then x)", () => {
    const log: string[] = [];
    const off = installHotkeys(binds(log), window);
    dispatch(window, "g");
    dispatch(window, "o");
    expect(log).toEqual(["overview"]);
    dispatch(window, "g");
    dispatch(window, "x"); // dead end → nothing
    expect(log).toEqual(["overview"]);
    dispatch(window, "g");
    dispatch(window, "r");
    expect(log).toEqual(["overview", "runs"]);
    off();
  });

  it("ignores chords while typing in an input, and modifier combos (⌘K passes through)", () => {
    const log: string[] = [];
    const off = installHotkeys(binds(log), window);
    const input = document.createElement("input");
    document.body.appendChild(input);
    dispatch(input, "g");
    dispatch(input, "o"); // typed into the input, not a chord
    expect(log).toEqual([]);
    dispatch(window, "g", { metaKey: true }); // modifier held → not a leader
    dispatch(window, "o");
    expect(log).toEqual([]);
    input.remove();
    off();
  });

  it("does not fire single non-leader keys (j/k stay with list nav)", () => {
    const log: string[] = [];
    const off = installHotkeys(binds(log), window);
    dispatch(window, "j");
    dispatch(window, "k");
    expect(log).toEqual([]);
    off();
  });

  it("an intervening real key (ArrowDown) aborts a pending chord; a bare modifier does not", () => {
    const log: string[] = [];
    const off = installHotkeys(binds(log), window);
    dispatch(window, "g");
    dispatch(window, "ArrowDown"); // real interruption → abort
    dispatch(window, "o");
    expect(log).toEqual([]); // chord did NOT complete across the interruption
    // A bare modifier keydown mid-chord is not an interruption.
    dispatch(window, "g");
    dispatch(window, "Shift");
    dispatch(window, "o");
    expect(log).toEqual(["overview"]);
    off();
  });

  it("unsubscribe stops handling", () => {
    const log: string[] = [];
    const off = installHotkeys(binds(log), window);
    off();
    dispatch(window, "g");
    dispatch(window, "o");
    expect(log).toEqual([]);
  });
});
