// Global analysis time-range: a workspace-level window the Analyze screens share. Pure
// date math — NO wall-clock read; windows are anchored on the data's last captured day (passed in),
// the same way Overview's rolling cards work, so results stay deterministic and reproducible.
/// Topbar picker options (order = display order).
export const RANGE_OPTIONS = [
    { key: "7d", label: "Last 7 days" },
    { key: "30d", label: "Last 30 days" },
    { key: "90d", label: "Last 90 days" },
    { key: "custom", label: "Custom…" },
];
/// Add `delta` days to a `YYYY-MM-DD` date string, returning `YYYY-MM-DD`. Pure UTC arithmetic on an
/// explicit date (NOT a clock read), so DST / local-tz drift can't shift the result.
export function addDays(ymd, delta) {
    const [y, m, d] = ymd.split("-").map(Number);
    const dt = new Date(Date.UTC(y, m - 1, d + delta));
    return dt.toISOString().slice(0, 10);
}
/// Resolve a range selection to a concrete `{from, to}` window, anchored on `anchorTo` (the data's
/// last captured day). 7d/30d/90d count back inclusively from the anchor; custom passes its explicit
/// dates through (falling back to the anchor when unset).
export function rangeToWindow(p, anchorTo) {
    switch (p.key) {
        case "7d":
            return { from: addDays(anchorTo, -6), to: anchorTo };
        case "30d":
            return { from: addDays(anchorTo, -29), to: anchorTo };
        case "90d":
            return { from: addDays(anchorTo, -89), to: anchorTo };
        case "custom":
        default:
            return { from: p.from ?? anchorTo, to: p.to ?? anchorTo };
    }
}
