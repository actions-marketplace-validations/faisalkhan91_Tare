// A reusable, framework-free data table: click-to-sort headers (stable tiebreak so output is
// byte-deterministic in tests), an optional debounced search box (substring, with a regex toggle
// mirroring W&B), and per-column cell renderers. The backbone for the Runs table, the comparer,
// Sessions, and the Live top-spenders. Presentation-only.
import { el } from "./el.js";
import { attachListNav, applySelection } from "./listNav.js";
/// Build a sortable/searchable table over `rows`. Returns a container element; all interaction is
/// internal (re-renders the tbody in place).
export function dataTable(rows, cols, opts) {
    let sortKey = opts.initialSort?.key;
    let sortDir = opts.initialSort?.dir ?? "desc";
    let query = "";
    let regex = false;
    const activeChips = new Set(); // indices of engaged quick-filter chips
    // Row windowing: opt-in via opts.rowHeight. When on, only a window of rows renders.
    const windowed = (opts.rowHeight ?? 0) > 0;
    const rowH = opts.rowHeight ?? 28;
    const OVERSCAN = 10;
    const DEFAULT_VISIBLE_ROWS = 40; // fallback screenful when the viewport has no layout (jsdom)
    let viewport = null;
    let tableEl = null;
    let lastVisible = []; // the full sorted+filtered set (for orderedIds + scroll math)
    const tbody = el("tbody", {});
    const headRow = el("tr", {});
    const thFor = (c) => {
        const arrow = sortKey === c.key ? (sortDir === "asc" ? " ▲" : " ▼") : "";
        const thAttrs = {
            class: c.numeric ? "num" : "",
            ...(c.ariaLabel ? { "aria-label": c.ariaLabel } : {}),
        };
        const th = c.label === "" && c.ariaLabel
            ? el("th", thAttrs, [el("span", { class: "sr-only", text: c.ariaLabel })])
            : el("th", { ...thAttrs, text: `${c.label}${arrow}` });
        if (c.sortValue) {
            th.classList.add("sortable");
            // Keyboard + screen-reader accessible sorting: focusable, Enter/Space to
            // sort, and aria-sort reflecting the current column so AT announces the order.
            th.tabIndex = 0;
            th.setAttribute("aria-sort", sortKey === c.key ? (sortDir === "asc" ? "ascending" : "descending") : "none");
            th.setAttribute("title", `Sort by ${c.label}`);
            const doSort = () => {
                if (sortKey === c.key)
                    sortDir = sortDir === "asc" ? "desc" : "asc";
                else {
                    sortKey = c.key;
                    sortDir = "desc";
                }
                redraw();
            };
            th.addEventListener("click", doSort);
            th.addEventListener("keydown", (e) => {
                if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    doSort();
                }
            });
        }
        return th;
    };
    function matchesSearch(row) {
        if (!opts.search || query === "")
            return true;
        const hay = opts.search(row);
        if (regex) {
            try {
                return new RegExp(query, "i").test(hay);
            }
            catch {
                return true; // an in-progress/invalid regex shouldn't hide everything
            }
        }
        return hay.toLowerCase().includes(query.toLowerCase());
    }
    function matches(row) {
        if (!matchesSearch(row))
            return false;
        // Every engaged chip must pass (AND) — chips narrow, they don't widen.
        for (const i of activeChips) {
            if (!opts.chips?.[i]?.predicate(row))
                return false;
        }
        return true;
    }
    function sorted(filtered) {
        const col = cols.find((c) => c.key === sortKey);
        if (!col?.sortValue)
            return filtered;
        const sv = col.sortValue;
        const dir = sortDir === "asc" ? 1 : -1;
        // Decorate-sort-undecorate: evaluate each accessor once (O(n)) rather than inside the
        // comparator (O(n log n)); result is identical to the per-pair compare.
        const decorated = filtered.map((row) => ({ row, k: sv(row), rk: opts.rowKey(row) }));
        decorated.sort((a, b) => {
            let cmp;
            if (typeof a.k === "number" && typeof b.k === "number")
                cmp = a.k - b.k;
            else
                cmp = String(a.k).localeCompare(String(b.k));
            // Stable tiebreak by row key (always ascending — not affected by dir).
            return cmp !== 0 ? cmp * dir : a.rk.localeCompare(b.rk);
        });
        return decorated.map((d) => d.row);
    }
    // Keyboard core-loop selection: the selected row's stable key, kept here so it
    // survives the table's own sort/search/chip re-renders (re-applied by id after each redraw).
    let selected = null;
    // Keyed diff-and-patch: cache each row's <tr> by its stable key so a re-sort /
    // search / chip toggle REUSES the same node (just refreshing its cells + reordering) instead of
    // tearing down and rebuilding every row. This keeps row node identity — so scroll position, focus,
    // and the keyboard selection stay stable — and cuts DOM churn on large tables. The rendered markup
    // is byte-identical to the old teardown path (no key attribute is added to the DOM; the cache is
    // JS-side), so goldens are unaffected.
    const rowNodes = new Map();
    function renderRow(row) {
        const key = opts.rowKey(row);
        const cells = cols.map((c) => el("td", { class: c.numeric ? "num" : "" }, [c.cell(row)]));
        const cached = rowNodes.get(key);
        if (cached) {
            cached.replaceChildren(...cells); // reuse the node identity, refresh contents
            return cached;
        }
        // `data-nav-id` only when keyboard nav is opted in — display-only tables stay byte-identical.
        const tr = el("tr", opts.onActivate ? { "data-nav-id": key } : {}, cells);
        rowNodes.set(key, tr);
        return tr;
    }
    function redraw() {
        headRow.replaceChildren(...cols.map(thFor));
        const visible = sorted(rows.filter(matches));
        // Zero-match (a filter hid everything, but the table isn't empty) → an explicit row rather than
        // a blank body, so the user knows the filter — not a load failure — is the cause.
        if (visible.length === 0 && rows.length > 0) {
            tbody.replaceChildren(el("tr", { class: "empty-row" }, [
                el("td", { class: "empty", colspan: String(cols.length) }, [
                    `No rows match your filter (0 of ${rows.length})`,
                ]),
            ]));
            return;
        }
        lastVisible = visible;
        if (windowed) {
            windowRows(visible);
            return;
        }
        const nodes = visible.map(renderRow);
        tbody.replaceChildren(...nodes); // reorders reused nodes; appends new ones
        // Evict cached nodes for rows no longer visible so the cache can't grow unbounded.
        const visKeys = new Set(visible.map(opts.rowKey));
        for (const k of [...rowNodes.keys()]) {
            if (!visKeys.has(k))
                rowNodes.delete(k);
        }
        // Re-highlight the selected row by id after the redraw (survives sort/search/chip changes).
        if (opts.onActivate)
            applySelection(container, selected);
    }
    /// A sized, inert spacer row that reserves the height of the off-window rows so the scrollbar
    /// reflects the FULL list. aria-hidden + presentation so it's invisible to SR + the row count.
    function spacer(h) {
        return el("tr", { class: "dt-spacer", "aria-hidden": "true", role: "presentation" }, [
            el("td", { colspan: String(cols.length), style: `height:${Math.round(h)}px;padding:0;border:0` }),
        ]);
    }
    /// Render only the rows visible in the scroll viewport (+ overscan): a bounded window with sized
    /// spacers above/below. aria-rowcount/aria-rowindex make position discoverable to
    /// screen readers despite the windowing; sort/filter/selection all operate over the full set.
    function windowRows(visible) {
        const n = visible.length;
        const vpH = (viewport?.clientHeight || 0) || rowH * DEFAULT_VISIBLE_ROWS;
        const per = Math.max(1, Math.ceil(vpH / rowH));
        const windowSize = per + 2 * OVERSCAN;
        const scrollTop = viewport?.scrollTop ?? 0;
        const start = Math.max(0, Math.min(Math.max(0, n - windowSize), Math.floor(scrollTop / rowH) - OVERSCAN));
        const end = Math.min(n, start + windowSize);
        tableEl?.setAttribute("aria-rowcount", String(n + 1)); // +1 for the header row (aria row 1)
        const nodes = [];
        if (start > 0)
            nodes.push(spacer(start * rowH));
        for (let i = start; i < end; i++) {
            const tr = renderRow(visible[i]);
            tr.setAttribute("aria-rowindex", String(i + 2)); // header is aria row 1, data rows follow
            nodes.push(tr);
        }
        if (end < n)
            nodes.push(spacer((n - end) * rowH));
        tbody.replaceChildren(...nodes);
        // Evict cached nodes outside the window so the DOM + cache stay bounded.
        const winKeys = new Set(visible.slice(start, end).map(opts.rowKey));
        for (const k of [...rowNodes.keys()])
            if (!winKeys.has(k))
                rowNodes.delete(k);
        if (opts.onActivate)
            applySelection(container, selected);
    }
    const container = el("div", { class: "datatable" });
    if (opts.search) {
        const input = el("input", {
            class: "dt-search",
            type: "search",
            placeholder: opts.searchPlaceholder ?? "Filter…",
            "aria-label": "Filter rows",
        });
        let timer;
        input.addEventListener("input", () => {
            query = input.value;
            clearTimeout(timer);
            timer = setTimeout(redraw, 120); // debounce
        });
        const reToggle = el("label", { class: "dt-regex", title: "Match as a regular expression" }, [
            (() => {
                const cb = el("input", { type: "checkbox", "aria-label": "Use regular expression" });
                cb.addEventListener("change", () => {
                    regex = cb.checked;
                    redraw();
                });
                return cb;
            })(),
            ".*",
        ]);
        container.appendChild(el("div", { class: "dt-controls" }, [input, reToggle]));
    }
    // Quick-filter chips: one toggle per data-derived predicate. Aria-pressed reflects
    // engagement; clicking re-filters live.
    if (opts.chips && opts.chips.length > 0) {
        const chipRow = el("div", { class: "dt-chips" });
        opts.chips.forEach((c, i) => {
            const btn = el("button", {
                class: "dt-chip",
                "aria-pressed": "false",
                text: c.label,
                onClick: () => {
                    if (activeChips.has(i))
                        activeChips.delete(i);
                    else
                        activeChips.add(i);
                    btn.setAttribute("aria-pressed", String(activeChips.has(i)));
                    redraw();
                },
            });
            chipRow.appendChild(btn);
        });
        container.appendChild(chipRow);
    }
    tableEl = el("table", {}, [el("thead", {}, [headRow]), tbody]);
    if (windowed) {
        // A bounded scroll viewport owns the windowing: scrollTop drives which
        // rows render. Redraw on scroll so new rows page in as the user scrolls.
        viewport = el("div", { class: "dt-viewport" });
        viewport.appendChild(tableEl);
        viewport.addEventListener("scroll", () => redraw());
        container.appendChild(viewport);
    }
    else {
        container.appendChild(tableEl);
    }
    // Horizontal overflow is otherwise invisible until someone happens to swipe. Keep a reusable,
    // measured cue with the table primitive so every consuming page gets the same affordance. A
    // windowed table scrolls horizontally inside its viewport; a regular table uses the container.
    const horizontalScroller = viewport ?? container;
    const overflowCue = el("p", {
        class: "datatable-overflow-cue",
        role: "status",
        "aria-atomic": "true",
        hidden: "",
    });
    container.appendChild(overflowCue);
    let overflowCueState = "";
    const updateOverflowCue = () => {
        const maxScroll = Math.max(0, horizontalScroller.scrollWidth - horizontalScroller.clientWidth);
        if (maxScroll <= 1) {
            overflowCue.hidden = true;
            overflowCueState = "";
            return;
        }
        const atStart = horizontalScroller.scrollLeft <= 1;
        const atEnd = horizontalScroller.scrollLeft >= maxScroll - 1;
        const nextState = atStart ? "forward" : atEnd ? "back" : "both";
        overflowCue.hidden = false;
        if (nextState === overflowCueState)
            return;
        overflowCueState = nextState;
        overflowCue.textContent = atStart
            ? "Scroll for more columns →"
            : atEnd
                ? "← Scroll for earlier columns"
                : "← More columns →";
    };
    horizontalScroller.addEventListener("scroll", updateOverflowCue, { passive: true });
    if (typeof ResizeObserver !== "undefined") {
        const observer = new ResizeObserver(() => {
            if (!container.isConnected) {
                observer.disconnect();
                return;
            }
            updateOverflowCue();
        });
        observer.observe(horizontalScroller);
        observer.observe(tableEl);
    }
    if (typeof requestAnimationFrame === "function")
        requestAnimationFrame(updateOverflowCue);
    else
        queueMicrotask(updateOverflowCue);
    redraw();
    if (opts.onActivate) {
        // Wire the keyboard core loop on the container (keydown bubbles from any row/link; the search
        // input is never hijacked). Don't relabel the container as a listbox — the <table> keeps its
        // native semantics (applyRole: false).
        const activate = opts.onActivate;
        attachListNav(container, {
            getSelected: () => selected,
            setSelected: (id) => {
                selected = id;
                // Virtualized: scroll the selection into the window + re-render so it's in the DOM to focus.
                if (windowed && id && viewport) {
                    const idx = lastVisible.findIndex((r) => opts.rowKey(r) === id);
                    if (idx >= 0) {
                        const vpH = viewport.clientHeight || rowH * DEFAULT_VISIBLE_ROWS;
                        const top = idx * rowH;
                        if (top < viewport.scrollTop || top + rowH > viewport.scrollTop + vpH) {
                            viewport.scrollTop = Math.max(0, top - Math.floor(vpH / 2));
                        }
                        redraw();
                    }
                }
            },
            onActivate: (id) => activate(id),
            applyRole: false,
            // Traverse the FULL set (not just the rendered window) when windowed.
            orderedIds: windowed ? () => lastVisible.map(opts.rowKey) : undefined,
        });
    }
    return container;
}
