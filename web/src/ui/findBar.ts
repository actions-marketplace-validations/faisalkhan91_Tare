// In-webview text Find. Tauri exposes no built-in find-in-page and the three native
// webview find APIs (WKWebView performTextFinderAction, WebView2 CoreWebView2.Find, webkitgtk
// FindController) are per-platform + runtime-only, so a dead Cmd/Ctrl-F is replaced by ONE
// cross-platform find bar the app fully owns: identical, testable behavior on every desktop target
// (and the browser), routed through the command registry as `action:find`.
//
// It searches the analytical content region (#main) — never the chrome — by walking text nodes and
// wrapping matches in <mark class="find-hit">; next/prev cycles the current hit, Esc closes and
// restores the DOM byte-for-byte (all marks unwrapped + parents normalized), so the find bar leaves no
// residue in exported/inspected markup. Framework-free, jsdom-testable.

import { el } from "./el.js";
import { icon } from "./icon.js";
import { matchShortcut } from "./keymap.js";
import { osClass, type OsClass } from "./os.js";

const BAR_ID = "tare-find-bar";
// Never descend into these while collecting searchable text: the bar itself, and non-text/trusted SVG.
const SKIP = new Set(["SCRIPT", "STYLE", "SVG", "MARK"]);

interface FindState {
  root: HTMLElement;
  doc: Document;
  bar: HTMLElement;
  input: HTMLInputElement;
  status: HTMLElement;
  hits: HTMLElement[];
  current: number;
}

let state: FindState | null = null;

/// Remove every highlight mark and restore the original text nodes (parent.normalize merges the split
/// text back), leaving the DOM exactly as it was before the search.
function clearHighlights(st: FindState): void {
  for (const mark of Array.from(st.root.querySelectorAll("mark.find-hit"))) {
    const parent = mark.parentNode;
    if (!parent) continue;
    parent.replaceChild(st.doc.createTextNode(mark.textContent ?? ""), mark);
    (parent as Element).normalize?.();
  }
  st.hits = [];
  st.current = -1;
}

/// Wrap every case-insensitive occurrence of `query` within a single text node in <mark>, returning the
/// created marks in document order. No-op for empty queries.
function highlightTextNode(st: FindState, node: Text, needle: string): HTMLElement[] {
  const text = node.nodeValue ?? "";
  const hay = text.toLowerCase();
  const q = needle.toLowerCase();
  let from = 0;
  let idx = hay.indexOf(q, from);
  if (idx < 0) return [];
  const frag = st.doc.createDocumentFragment();
  const marks: HTMLElement[] = [];
  while (idx >= 0) {
    if (idx > from) frag.appendChild(st.doc.createTextNode(text.slice(from, idx)));
    const mark = el("mark", { class: "find-hit" }, [text.slice(idx, idx + needle.length)]);
    frag.appendChild(mark);
    marks.push(mark);
    from = idx + needle.length;
    idx = hay.indexOf(q, from);
  }
  if (from < text.length) frag.appendChild(st.doc.createTextNode(text.slice(from)));
  node.parentNode?.replaceChild(frag, node);
  return marks;
}

/// Collect the searchable text nodes under the root, skipping the bar + non-text containers.
function textNodes(st: FindState): Text[] {
  const walker = st.doc.createTreeWalker(st.root, NodeFilter.SHOW_TEXT, {
    acceptNode: (n: Node) => {
      if (!n.nodeValue || !n.nodeValue.trim()) return NodeFilter.FILTER_REJECT;
      for (let p = n.parentElement; p; p = p.parentElement) {
        if (p.id === BAR_ID || SKIP.has(p.tagName)) return NodeFilter.FILTER_REJECT;
      }
      return NodeFilter.FILTER_ACCEPT;
    },
  });
  const out: Text[] = [];
  let n = walker.nextNode();
  while (n) {
    out.push(n as Text);
    n = walker.nextNode();
  }
  return out;
}

/// Mark the hit at `current` as the active one (scrolled into view) and update the "n/N" status.
function focusCurrent(st: FindState): void {
  st.root.querySelectorAll("mark.find-hit.current").forEach((m) => m.classList.remove("current"));
  if (st.hits.length === 0) {
    st.status.textContent = st.input.value ? "No results" : "";
    return;
  }
  const hit = st.hits[st.current];
  hit.classList.add("current");
  if (typeof hit.scrollIntoView === "function") hit.scrollIntoView({ block: "center" });
  st.status.textContent = `${st.current + 1}/${st.hits.length}`;
}

/// Re-run the search for the current input value: clear old marks, highlight all matches, select the
/// first (preserving the current index when possible so typing doesn't jump you back to the top).
function runSearch(st: FindState, keepIndex = false): void {
  const prev = st.current;
  clearHighlights(st);
  const q = st.input.value;
  if (q) {
    for (const node of textNodes(st)) st.hits.push(...highlightTextNode(st, node, q));
  }
  st.current = st.hits.length === 0 ? -1 : keepIndex ? Math.min(Math.max(prev, 0), st.hits.length - 1) : 0;
  focusCurrent(st);
}

function step(st: FindState, delta: number): void {
  if (st.hits.length === 0) return;
  st.current = (st.current + delta + st.hits.length) % st.hits.length;
  focusCurrent(st);
}

/// Close the find bar: clear highlights, remove the bar, and return focus to the content.
export function closeFind(): void {
  if (!state) return;
  const st = state;
  clearHighlights(st);
  st.bar.remove();
  state = null;
  if (typeof st.root.focus === "function") {
    try {
      st.root.focus();
    } catch {
      /* jsdom focus may no-op */
    }
  }
}

/// Open (or refocus) the find bar over the analytical content. Idempotent — a second call just
/// refocuses the existing input. `root`/`doc` are injectable for tests.
export function openFind(opts: { root?: HTMLElement; doc?: Document } = {}): void {
  const doc = opts.doc ?? document;
  const root = opts.root ?? doc.getElementById("main") ?? doc.body;
  if (state) {
    state.input.focus();
    state.input.select();
    return;
  }

  const input = el("input", {
    type: "text",
    class: "find-input",
    "aria-label": "Find in page",
    placeholder: "Find",
  }) as HTMLInputElement;
  const status = el("span", { class: "find-status", "aria-live": "polite" });
  const prevBtn = el("button", { class: "find-nav find-prev", "aria-label": "Previous match", title: "Previous (⇧⏎)" }, [
    icon("chevron", { size: 14 }),
  ]);
  const nextBtn = el("button", { class: "find-nav find-next", "aria-label": "Next match", title: "Next (⏎)" }, [
    icon("chevron", { size: 14 }),
  ]);
  const closeBtn = el("button", { class: "find-nav find-close", "aria-label": "Close find", title: "Close (Esc)" }, [
    icon("close", { size: 12 }),
  ]);
  const bar = el("div", { id: BAR_ID, class: "find-bar", role: "search" }, [input, status, prevBtn, nextBtn, closeBtn]);

  const st: FindState = { root, doc, bar, input, status, hits: [], current: -1 };
  state = st;

  input.addEventListener("input", () => runSearch(st, true));
  input.addEventListener("keydown", (e) => {
    const ke = e as KeyboardEvent;
    if (ke.key === "Enter") {
      ke.preventDefault();
      step(st, ke.shiftKey ? -1 : 1);
    } else if (ke.key === "Escape") {
      ke.preventDefault();
      closeFind();
    }
  });
  prevBtn.addEventListener("click", () => step(st, -1));
  nextBtn.addEventListener("click", () => step(st, 1));
  closeBtn.addEventListener("click", () => closeFind());

  doc.body.appendChild(bar);
  input.focus();
}

/// Install the Find hotkey (logical `Mod+F` — ⌘F on macOS, Ctrl-F elsewhere), returning an
/// unsubscribe. Gated to `isDesktop`: in a real browser Cmd/Ctrl-F is the native find-in-page and must
/// not be hijacked, matching the slash-search rationale; the desktop WebView has no native
/// find, so the bar fills the gap. `os`/`isDesktop` are injectable for tests.
export function installFindHotkey(
  win: Window = window,
  os: OsClass = osClass(typeof navigator !== "undefined" ? navigator.userAgent : ""),
  isDesktop: boolean = typeof (globalThis as { __TAURI__?: unknown }).__TAURI__ !== "undefined"
): () => void {
  if (!isDesktop) return () => {};
  const handler = (e: KeyboardEvent): void => {
    if (matchShortcut(e, "Mod+F", os)) {
      e.preventDefault();
      openFind();
    }
  };
  win.addEventListener("keydown", handler as EventListener);
  return () => win.removeEventListener("keydown", handler as EventListener);
}
