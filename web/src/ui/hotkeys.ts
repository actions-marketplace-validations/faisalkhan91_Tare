// Leader-key hotkey router: vim-style "g o"-type chords for go-to and actions, the
// keyboard complement to ⌘K. Framework-free, jsdom-testable. Deliberately handles only MULTI-key
// chords (a leader like `g` then a letter) so it never competes with single-key list navigation
// (j/k/Enter, owned by listNav) or with modifier shortcuts (⌘K, owned by the palette hotkey).

import { isEditableTarget } from "../commands/contexts.js";

export interface Hotkey {
  /// Space-separated key sequence, lowercase, e.g. "g o" (press g, then o).
  seq: string;
  /// Human label for the ⌘K row / help ("Go to Overview").
  label: string;
  run: () => void;
}

export type ChordStatus = "exact" | "prefix" | "none";

/// Bare modifier keydowns (lowercased `KeyboardEvent.key`) — these are not chord interruptions, so a
/// modifier pressed mid-chord must not clear the pending buffer.
const MODIFIER_KEYS = new Set(["shift", "control", "alt", "meta", "capslock", "altgraph", "fn"]);

/// Pure chord matcher: given the bindings and the current key buffer, is it a complete binding, a
/// prefix of one (keep waiting), or a dead end? Exact wins over prefix.
export function chordState(
  bindings: Hotkey[],
  buffer: string[]
): { status: ChordStatus; binding?: Hotkey } {
  if (buffer.length === 0) return { status: "none" };
  const chord = buffer.join(" ");
  const exact = bindings.find((b) => b.seq === chord);
  if (exact) return { status: "exact", binding: exact };
  const prefix = bindings.some((b) => b.seq.startsWith(chord + " "));
  return { status: prefix ? "prefix" : "none" };
}

export interface HotkeysOptions {
  /// Chord timeout in ms — a partial chord clears if the next key doesn't arrive in time.
  timeoutMs?: number;
  /// Injectable timer (tests pass a manual clock); defaults to window setTimeout/clearTimeout.
  now?: () => number;
}

/// Install the router on `win`. Returns an unsubscribe. Ignores keystrokes while an input/textarea/
/// contenteditable is focused, and any keystroke with a modifier held (so ⌘K et al. pass through).
export function installHotkeys(
  bindings: Hotkey[],
  win: Window = window,
  opts: HotkeysOptions = {}
): () => void {
  const timeoutMs = opts.timeoutMs ?? 900;
  let buffer: string[] = [];
  let timer: ReturnType<typeof setTimeout> | null = null;
  const clear = (): void => {
    buffer = [];
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  };
  const arm = (): void => {
    if (timer !== null) clearTimeout(timer);
    timer = setTimeout(clear, timeoutMs);
  };

  const handler = (e: KeyboardEvent): void => {
    if (e.metaKey || e.ctrlKey || e.altKey) return; // leave modifier shortcuts (⌘K) alone
    // Central focus-scope guard: never intercept unscoped keys in a text field.
    if (isEditableTarget(e.target)) return;
    const k = e.key.toLowerCase();
    if (k === "escape") {
      clear();
      return;
    }
    // Only single printable keys form chords. A non-char key (Enter/Tab/Arrow/…) mid-chord is a real
    // interruption → abort the pending chord (matching the dead-end reset below); but a bare modifier
    // keydown (Shift/Control/…) is not an interruption and must not clear a chord in progress.
    if (k.length !== 1) {
      if (!MODIFIER_KEYS.has(k)) clear();
      return;
    }
    buffer.push(k);
    const { status, binding } = chordState(bindings, buffer);
    if (status === "exact" && binding) {
      e.preventDefault();
      clear();
      binding.run();
      return;
    }
    if (status === "prefix") {
      e.preventDefault(); // swallow the leader so it doesn't leak to the page
      arm();
      return;
    }
    // Dead end: reset. If this key itself begins a binding, start a fresh buffer from it — and, like
    // the prefix branch, swallow it so the live leader doesn't leak to the page.
    clear();
    if (bindings.some((b) => b.seq.startsWith(k + " "))) {
      e.preventDefault();
      buffer = [k];
      arm();
    }
  };

  win.addEventListener("keydown", handler as EventListener);
  return () => {
    clear();
    win.removeEventListener("keydown", handler as EventListener);
  };
}
