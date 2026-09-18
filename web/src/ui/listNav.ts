// Keyboard list navigation: the master-detail core loop for any in-page list —
// j/k (and ↑/↓) move a selection, Enter activates, Esc clears. Framework-free, jsdom-testable.
//
// Single-source selection: the selected item's id lives wherever `getSelected`/`setSelected` point
// (the caller's store/closure), NEVER in scattered DOM `:focus`. Selection is keyed by a stable
// `data-nav-id`, not DOM index, so it survives re-sort / live-update / full re-render — call
// `applySelection` after each render to re-highlight whatever the source still holds.

export interface ListNavOptions {
  /// Current selection id from the single source of truth, or null when nothing is selected.
  getSelected: () => string | null;
  /// Persist the new selection. The caller may re-render; the highlight is re-applied here + on render.
  setSelected: (id: string | null) => void;
  /// Enter / master→detail on the selected id.
  onActivate?: (id: string) => void;
  /// Extra Esc behavior beyond clearing selection (e.g. collapse a detail pane).
  onEscape?: () => void;
  /// Set `role="listbox"` on the container (default true). Pass false when the container already has
  /// meaningful semantics (e.g. a `<table>` wrapper) so we don't override them.
  applyRole?: boolean;
  /// The FULL ordered id list, for a VIRTUALIZED list where only a window of rows is in the DOM.
  /// When provided, j/k traverse the whole list (not just rendered rows); `setSelected`
  /// is expected to scroll + re-render so the newly-selected id lands in the window before it's
  /// focused. Omit for a fully-rendered list (uses the DOM row order, unchanged).
  orderedIds?: () => string[];
}

/// The stable ids of the currently-rendered rows, in DOM order.
function rowIds(container: HTMLElement): string[] {
  return Array.from(container.querySelectorAll<HTMLElement>("[data-nav-id]")).map(
    (r) => r.dataset.navId ?? ""
  );
}

/// Re-highlight the row whose `data-nav-id === id` and clear the rest. Idempotent; call after every
/// re-render so a live-update or re-sort keeps the selection visible (the id is stable, the node new).
export function applySelection(container: HTMLElement, id: string | null): void {
  container.querySelectorAll<HTMLElement>("[data-nav-id]").forEach((r) => {
    const on = id !== null && r.dataset.navId === id;
    r.classList.toggle("nav-selected", on);
    r.setAttribute("aria-selected", on ? "true" : "false");
  });
}

function focusRow(container: HTMLElement, id: string): void {
  const row = Array.from(container.querySelectorAll<HTMLElement>("[data-nav-id]")).find(
    (r) => r.dataset.navId === id
  );
  try {
    row?.focus?.();
    row?.scrollIntoView?.({ block: "nearest" });
  } catch {
    /* jsdom focus/scroll may no-op */
  }
}

/// Attach j/k/↑/↓/Enter/Esc to `container`. Returns an unsubscribe. Rows are any descendants with a
/// `data-nav-id`. Typing in an input/textarea inside the container is never hijacked.
export function attachListNav(container: HTMLElement, opts: ListNavOptions): () => void {
  const move = (delta: number): void => {
    // Virtualized lists traverse the FULL ordered id list; fully-rendered lists use the DOM order.
    const ids = opts.orderedIds ? opts.orderedIds() : rowIds(container);
    if (ids.length === 0) return;
    const cur = opts.getSelected();
    const idx = cur ? ids.indexOf(cur) : -1;
    // Entering an unselected list: j lands on the first row, k on the last (natural direction).
    const next =
      idx === -1
        ? delta > 0
          ? 0
          : ids.length - 1
        : Math.min(ids.length - 1, Math.max(0, idx + delta));
    const id = ids[next];
    // setSelected may scroll + re-render (virtualized) so the row is in the window before we focus it.
    opts.setSelected(id);
    applySelection(container, id);
    focusRow(container, id);
  };

  const handler = (e: KeyboardEvent): void => {
    const t = e.target as HTMLElement | null;
    // Arrows/typing belong to a focused text field or <select> (cursor / option nav) — never hijack.
    const inTextField =
      !!t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.tagName === "SELECT" || t.isContentEditable);
    // Enter belongs to a focused actionable control (button/link/etc.) so its native action fires —
    // otherwise the row's pin/baseline buttons and run-id links couldn't be keyboard-activated.
    const onControl = !!t?.closest?.("button, a, input, textarea, select, [contenteditable=true]");
    switch (e.key) {
      case "j":
      case "ArrowDown":
        if (inTextField) return;
        e.preventDefault();
        move(1);
        break;
      case "k":
      case "ArrowUp":
        if (inTextField) return;
        e.preventDefault();
        move(-1);
        break;
      case "Enter": {
        if (onControl) return; // let the focused control activate natively
        const cur = opts.getSelected();
        const known = opts.orderedIds ? opts.orderedIds() : rowIds(container);
        if (cur && known.includes(cur)) {
          e.preventDefault();
          opts.onActivate?.(cur);
        }
        break;
      }
      case "Escape":
        e.preventDefault();
        opts.setSelected(null);
        applySelection(container, null);
        opts.onEscape?.();
        break;
    }
  };

  container.addEventListener("keydown", handler as EventListener);
  // The container must be focusable to receive key events when a row isn't itself focused.
  if (!container.hasAttribute("tabindex")) container.setAttribute("tabindex", "0");
  if (opts.applyRole !== false) container.setAttribute("role", "listbox");
  // Re-apply whatever the source already holds (survives a re-render that re-attaches).
  applySelection(container, opts.getSelected());
  return () => container.removeEventListener("keydown", handler as EventListener);
}
