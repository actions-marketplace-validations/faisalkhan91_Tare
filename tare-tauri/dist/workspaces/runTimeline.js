// Run Profile timing evidence.
//
// Unix nanoseconds stay BigInt from parsing through layout arithmetic: converting an epoch instant
// to Number would lose precision before the first subtraction. A step becomes a timed span only when
// both decimal-string bounds are valid and ordered. Everything else remains captured Step order.
function decimalNanos(value) {
    if (!value || !/^\d+$/.test(value))
        return null;
    try {
        return BigInt(value);
    }
    catch {
        return null;
    }
}
function overlaps(a, b) {
    // Strict overlap: adjacent spans and zero-duration instants are not concurrent intervals.
    return a.end > a.start && b.end > b.start && a.start < b.end && b.start < a.end;
}
function countOverlaps(sorted) {
    const spans = sorted.filter((row) => row.end > row.start);
    let count = 0;
    for (let index = 0; index < spans.length; index++) {
        const end = spans[index].end;
        let low = index + 1;
        let high = spans.length;
        while (low < high) {
            const middle = Math.floor((low + high) / 2);
            if (spans[middle].start < end)
                low = middle + 1;
            else
                high = middle;
        }
        count += Math.max(0, low - index - 1);
    }
    return count;
}
function sameTrace(a, b) {
    return Boolean(a.trace_id && b.trace_id && a.trace_id === b.trace_id);
}
function isNested(a, b) {
    if (!sameTrace(a, b) || !a.span_id || !b.span_id)
        return false;
    return a.parent_span_id === b.span_id || b.parent_span_id === a.span_id;
}
function isSiblingConcurrency(a, b) {
    // Overlapping siblings in the same trace have both temporal AND ancestry evidence. Parent-child
    // overlap is nesting, not concurrency, and two unrelated wall-clock intervals are never upgraded.
    return Boolean(sameTrace(a, b) &&
        a.span_id &&
        b.span_id &&
        a.span_id !== b.span_id &&
        a.parent_span_id &&
        a.parent_span_id === b.parent_span_id);
}
function ratioPct(part, whole) {
    if (whole <= 0n || part <= 0n)
        return 0;
    // Hundredths of one percent are ample for pixel layout while keeping the division in BigInt.
    return Number((part * 10000n) / whole) / 100;
}
/** Build the evidence model without inventing timestamps, duration, ancestry, or concurrency. */
export function buildRunTimeline(steps) {
    const captured = [...steps].sort((a, b) => a.ordinal - b.ordinal);
    const parsed = [];
    for (const step of captured) {
        const start = decimalNanos(step.start_unix_nano);
        const end = decimalNanos(step.end_unix_nano);
        if (start == null || end == null || end < start)
            continue;
        parsed.push({ step, start, end });
    }
    if (parsed.length === 0) {
        return {
            mode: "step_order",
            orderedSteps: captured,
            timedSteps: [],
            timedByOrdinal: new Map(),
            timedCount: 0,
            totalCount: captured.length,
            origin: null,
            end: null,
            duration: 0n,
            overlapCount: 0,
            nestedCount: 0,
            concurrentCount: 0,
        };
    }
    const origin = parsed.reduce((min, row) => (row.start < min ? row.start : min), parsed[0].start);
    const end = parsed.reduce((max, row) => (row.end > max ? row.end : max), parsed[0].end);
    const duration = end - origin;
    const timed = parsed
        .map(({ step, start, end: stepEnd }) => ({
        step,
        start,
        end: stepEnd,
        offset: start - origin,
        duration: stepEnd - start,
        leftPct: ratioPct(start - origin, duration),
        widthPct: ratioPct(stepEnd - start, duration),
    }))
        .sort((a, b) => (a.start < b.start ? -1 : a.start > b.start ? 1 : a.step.ordinal - b.step.ordinal));
    const timedByOrdinal = new Map(timed.map((row) => [row.step.ordinal, row]));
    const overlapCount = countOverlaps(timed);
    // Exact concurrency count in O(n log n): only unique sibling span IDs grouped by trace+parent.
    const siblingGroups = new Map();
    for (const row of timed) {
        const step = row.step;
        if (!step.trace_id || !step.parent_span_id || !step.span_id)
            continue;
        const key = `${step.trace_id}\u0000${step.parent_span_id}`;
        const group = siblingGroups.get(key) ?? new Map();
        if (!group.has(step.span_id))
            group.set(step.span_id, row);
        siblingGroups.set(key, group);
    }
    let concurrentCount = 0;
    for (const group of siblingGroups.values()) {
        concurrentCount += countOverlaps([...group.values()].sort((a, b) => a.start < b.start ? -1 : a.start > b.start ? 1 : a.step.ordinal - b.step.ordinal));
    }
    // Direct parent-child nesting is linear once trace/span identity is indexed.
    const byTraceSpan = new Map();
    for (const row of timed) {
        if (row.step.trace_id && row.step.span_id) {
            byTraceSpan.set(`${row.step.trace_id}\u0000${row.step.span_id}`, row);
        }
    }
    let nestedCount = 0;
    for (const row of timed) {
        const step = row.step;
        if (!step.trace_id || !step.parent_span_id)
            continue;
        const parent = byTraceSpan.get(`${step.trace_id}\u0000${step.parent_span_id}`);
        if (parent && overlaps(row, parent))
            nestedCount++;
    }
    const untimed = captured.filter((step) => !timedByOrdinal.has(step.ordinal));
    return {
        mode: "timeline",
        orderedSteps: [...timed.map((row) => row.step), ...untimed],
        timedSteps: timed,
        timedByOrdinal,
        timedCount: timed.length,
        totalCount: captured.length,
        origin,
        end,
        duration,
        overlapCount,
        nestedCount,
        concurrentCount,
    };
}
/** Compact elapsed label for offsets/durations; it never formats an epoch instant as a JS Number. */
export function formatTimelineNanos(nanos) {
    if (nanos <= 0n)
        return "0 ms";
    if (nanos < 1000000n)
        return "<1 ms";
    const micros = nanos / 1000n;
    if (micros < 1000000n) {
        const wholeMs = micros / 1000n;
        const tenth = (micros % 1000n) / 100n;
        return tenth > 0n ? `${wholeMs}.${tenth} ms` : `${wholeMs} ms`;
    }
    const tenths = nanos / 100000000n;
    if (tenths < 600n)
        return `${tenths / 10n}.${tenths % 10n} s`;
    const wholeSeconds = nanos / 1000000000n;
    const minutes = wholeSeconds / 60n;
    const seconds = wholeSeconds % 60n;
    return seconds > 0n ? `${minutes} min ${seconds} s` : `${minutes} min`;
}
export function relatedSteps(model, ordinal, relation) {
    const selected = model.timedByOrdinal.get(ordinal);
    if (!selected)
        return [];
    return model.timedSteps
        .filter((candidate) => {
        if (candidate.step.ordinal === ordinal || !overlaps(selected, candidate))
            return false;
        if (relation === "nested")
            return isNested(selected.step, candidate.step);
        if (relation === "concurrent")
            return isSiblingConcurrency(selected.step, candidate.step);
        return true;
    })
        .map((candidate) => candidate.step.ordinal)
        .sort((a, b) => a - b);
}
