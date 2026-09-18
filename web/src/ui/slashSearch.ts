// "/" focuses the page's search input. Chosen over Mod+F so it never fights the
// browser/webview native find-in-page (⌘F/Ctrl-F), and it matches the existing literal-key
// vocabulary (j/k list nav, g-chords). Ignored while a text field is focused (so typing "/" in a
// filter works normally) and when a modifier is held. Framework-free, jsdom-testable.

/// Install the "/" → focus-search handler on `win`, resolving the search input from `doc`. Returns an
/// unsubscribe. If the current screen has no search input, "/" is left alone (not swallowed).
export function installSlashSearch(win: Window = window, doc: Document = document): () => void {
  const handler = (e: KeyboardEvent): void => {
    if (e.key !== "/" || e.metaKey || e.ctrlKey || e.altKey) return;
    const t = e.target as HTMLElement | null;
    if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
    const search = doc.querySelector<HTMLInputElement>('input[type="search"]');
    if (!search) return; // no search surface here → let "/" through
    e.preventDefault();
    search.focus();
    search.select();
  };
  win.addEventListener("keydown", handler as EventListener);
  return () => win.removeEventListener("keydown", handler as EventListener);
}
