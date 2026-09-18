// Pure Run Profile screen models. These transforms reshape the already-loaded
// flamegraph for interactive Chronological / Aggregated / Sandwich views. They never touch the
// byte-stable Rust/export renderers: only `renderSvgThemed` receives their output.
function stableCompare(a, b) {
    return a < b ? -1 : a > b ? 1 : 0;
}
function stepOrdinal(name) {
    const match = /^step\s+(\d+)(?:\s|·|$)/i.exec(name.trim());
    return match ? Number(match[1]) : null;
}
function stripStepOrdinal(name) {
    return name
        .trim()
        .replace(/^step\s+\d+\s*(?:·\s*)?/i, "")
        .trim() || "Step";
}
export function normalizeProfileLabel(label) {
    return label.normalize("NFKC").trim().replace(/\s+/g, " ").toLowerCase();
}
function kindFor(node, depth) {
    if (depth === 0)
        return "run";
    if (depth === 1)
        return "step";
    if (node.cache_class)
        return "cache_class";
    if (depth === 2)
        return "component";
    return "frame";
}
function displayFor(node, kind) {
    return kind === "step" ? stripStepOrdinal(node.name) : node.name.trim() || "Unlabeled";
}
function keyFor(kind, label, cacheClass) {
    return JSON.stringify([kind, normalizeProfileLabel(label), cacheClass ?? ""]);
}
function mergeReferences(groups) {
    const byKey = new Map();
    for (const refs of groups) {
        for (const ref of refs)
            byKey.set(`${ref.run_id}\u0000${ref.step_ordinal}`, ref);
    }
    return [...byKey.values()].sort((a, b) => stableCompare(a.run_id, b.run_id) || a.step_ordinal - b.step_ordinal);
}
function annotateNode(node, runId, depth, inheritedStep) {
    const kind = kindFor(node, depth);
    const ownStep = kind === "step" ? stepOrdinal(node.name) : null;
    const currentStep = ownStep ?? inheritedStep;
    const children = node.children.map((child) => annotateNode(child, runId, depth + 1, currentStep));
    const display = displayFor(node, kind);
    const references = currentStep == null
        ? mergeReferences(children.map((child) => child.references))
        : [{ run_id: runId, step_ordinal: currentStep }];
    const childMicros = node.children.reduce((sum, child) => sum + child.micros, 0);
    const childTokens = node.children.reduce((sum, child) => sum + child.tokens, 0);
    return {
        ...node,
        node_kind: kind,
        normalized_label: normalizeProfileLabel(display),
        display_label: display,
        frame_key: keyFor(kind, display, node.cache_class),
        calls: kind === "run" ? Math.max(1, references.length) : 1,
        self_micros: Math.max(0, node.micros - childMicros),
        self_tokens: Math.max(0, node.tokens - childTokens),
        references,
        children,
    };
}
export function chronologicalProfile(model) {
    return { ...model, root: annotateNode(model.root, model.run_id, 0, null) };
}
function activeWeight(node, weight) {
    return weight === "cost" ? node.micros : node.tokens;
}
function sortFrames(frames, weight) {
    return frames.sort((a, b) => activeWeight(b, weight) - activeWeight(a, weight) ||
        stableCompare(a.normalized_label, b.normalized_label) ||
        stableCompare(a.frame_key, b.frame_key));
}
function mergeSiblings(frames, weight) {
    const groups = new Map();
    for (const frame of frames) {
        const group = groups.get(frame.frame_key) ?? [];
        group.push(frame);
        groups.set(frame.frame_key, group);
    }
    const merged = [];
    for (const [frameKey, group] of groups) {
        const labels = group.map((frame) => frame.display_label).sort(stableCompare);
        const display = labels[0];
        const calls = group.reduce((sum, frame) => sum + frame.calls, 0);
        const base = group[0];
        const children = mergeSiblings(group.flatMap((frame) => frame.children), weight);
        merged.push({
            ...base,
            name: calls > 1 ? `${display} · ${calls} calls` : display,
            display_label: display,
            frame_key: frameKey,
            tokens: group.reduce((sum, frame) => sum + frame.tokens, 0),
            micros: group.reduce((sum, frame) => sum + frame.micros, 0),
            self_tokens: group.reduce((sum, frame) => sum + frame.self_tokens, 0),
            self_micros: group.reduce((sum, frame) => sum + frame.self_micros, 0),
            calls,
            references: mergeReferences(group.map((frame) => frame.references)),
            children,
        });
    }
    return sortFrames(merged, weight);
}
function aggregateAnnotated(model, weight) {
    const children = mergeSiblings(model.root.children, weight);
    return {
        ...model,
        root: {
            ...model.root,
            calls: Math.max(1, model.root.references.length),
            children,
        },
    };
}
/**
 * Merge only sibling frames with the same FrameKey, then recurse within each merged parent. This is
 * deliberately path-aware: an identically named component beneath a different model/caller remains
 * distinct because it is never placed in the same sibling group.
 */
export function aggregatedProfile(model, weight) {
    return aggregateAnnotated(chronologicalProfile(model), weight);
}
export function profileFrames(model) {
    const frames = [];
    const walk = (frame) => {
        frames.push(frame);
        frame.children.forEach(walk);
    };
    walk(model.root);
    return frames;
}
export function componentChoices(model, weight = "cost") {
    // Accumulate raw reference arrays per group and merge ONCE at the end (below), not on every
    // frame — merging inside the loop rebuilds+sorts the whole accumulated reference list at each
    // step, turning an O(n) aggregation into O(n²) for a component label repeated across many steps
    // (This is what previously made a 50k+-step run's Run Profile hang.)
    const groups = new Map();
    for (const frame of profileFrames(chronologicalProfile(model))) {
        if (frame.node_kind !== "component")
            continue;
        const current = groups.get(frame.frame_key);
        if (current) {
            current.calls += frame.calls;
            current.micros += frame.micros;
            current.tokens += frame.tokens;
            current.refGroups.push(frame.references);
            if (stableCompare(frame.display_label, current.label) < 0)
                current.label = frame.display_label;
        }
        else {
            groups.set(frame.frame_key, {
                label: frame.display_label,
                calls: frame.calls,
                micros: frame.micros,
                tokens: frame.tokens,
                refGroups: [frame.references],
            });
        }
    }
    const choices = [...groups.entries()].map(([frame_key, group]) => ({
        frame_key,
        label: group.label,
        calls: group.calls,
        micros: group.micros,
        tokens: group.tokens,
        references: mergeReferences(group.refGroups),
    }));
    return choices.sort((a, b) => (weight === "cost" ? b.micros - a.micros : b.tokens - a.tokens) ||
        stableCompare(a.label, b.label) ||
        stableCompare(a.frame_key, b.frame_key));
}
function matchingComponents(frame, frameKey) {
    const found = [];
    const walk = (candidate) => {
        if (candidate.node_kind === "component" && candidate.frame_key === frameKey) {
            found.push(candidate);
            return;
        }
        candidate.children.forEach(walk);
    };
    frame.children.forEach(walk);
    return found;
}
/** Build a caller → selected component → callee subtree, then aggregate equal caller paths. */
export function sandwichProfile(model, componentFrameKey, weight) {
    const chronological = chronologicalProfile(model);
    const callers = [];
    for (const caller of chronological.root.children) {
        const selected = matchingComponents(caller, componentFrameKey);
        if (selected.length === 0)
            continue;
        callers.push({
            ...caller,
            tokens: selected.reduce((sum, frame) => sum + frame.tokens, 0),
            micros: selected.reduce((sum, frame) => sum + frame.micros, 0),
            self_tokens: 0,
            self_micros: 0,
            references: mergeReferences(selected.map((frame) => frame.references)),
            children: selected,
        });
    }
    if (callers.length === 0)
        return null;
    const choice = componentChoices(model, weight).find((candidate) => candidate.frame_key === componentFrameKey);
    const root = {
        ...chronological.root,
        name: `Sandwich · ${choice?.label ?? "selected component"}`,
        display_label: choice?.label ?? "Selected component",
        tokens: callers.reduce((sum, caller) => sum + caller.tokens, 0),
        micros: callers.reduce((sum, caller) => sum + caller.micros, 0),
        self_tokens: 0,
        self_micros: 0,
        references: mergeReferences(callers.map((caller) => caller.references)),
        children: callers,
    };
    return aggregateAnnotated({ ...chronological, root }, weight);
}
function focusFrame(frame, frameKey) {
    if (frame.frame_key === frameKey)
        return frame;
    const children = frame.children
        .map((child) => focusFrame(child, frameKey))
        .filter((child) => child !== null);
    return children.length > 0 ? { ...frame, children } : null;
}
function hideFrame(frame, frameKey) {
    return {
        ...frame,
        children: frame.children
            .filter((child) => child.frame_key !== frameKey)
            .map((child) => hideFrame(child, frameKey)),
    };
}
function ignoreChildren(children, frameKey) {
    const out = [];
    for (const child of children) {
        const descendants = ignoreChildren(child.children, frameKey);
        if (child.frame_key === frameKey)
            out.push(...descendants);
        else
            out.push({ ...child, children: descendants });
    }
    return out;
}
function recalculateVisible(frame) {
    const children = frame.children.map(recalculateVisible);
    const references = mergeReferences([
        ...(frame.self_micros > 0 || frame.self_tokens > 0 ? [frame.references] : []),
        ...children.map((child) => child.references),
    ]);
    return {
        ...frame,
        tokens: frame.self_tokens + children.reduce((sum, child) => sum + child.tokens, 0),
        micros: frame.self_micros + children.reduce((sum, child) => sum + child.micros, 0),
        references,
        calls: frame.node_kind === "run" ? Math.max(1, references.length) : frame.calls,
        children,
    };
}
export function applyFrameAction(model, action, frameKey) {
    if (action === "hide") {
        return { ...model, root: recalculateVisible(hideFrame(model.root, frameKey)) };
    }
    if (action === "ignore") {
        return {
            ...model,
            root: recalculateVisible({
                ...model.root,
                children: ignoreChildren(model.root.children, frameKey),
            }),
        };
    }
    const focused = focusFrame(model.root, frameKey);
    return {
        ...model,
        root: recalculateVisible(focused ?? { ...model.root, children: [] }),
    };
}
function sandwichRole(depth, sandwich) {
    if (!sandwich)
        return null;
    if (depth === 1)
        return "caller";
    if (depth === 2)
        return "selected";
    return "callee";
}
export function profileTableRows(model, weight, tableMode, query = "", sandwich = false) {
    const rows = [];
    const walk = (frame, depth, path) => {
        frame.children.forEach((child, index) => {
            const id = `${path}/${encodeURIComponent(child.frame_key)}:${index}`;
            rows.push({
                id,
                depth: depth + 1,
                role: sandwichRole(depth + 1, sandwich),
                frame_key: child.frame_key,
                node_kind: child.node_kind,
                label: child.display_label,
                calls: child.calls,
                self_micros: child.self_micros,
                cum_micros: child.micros,
                self_tokens: child.self_tokens,
                cum_tokens: child.tokens,
                cost_per_call_micros: Math.round(child.micros / Math.max(1, child.calls)),
                references: child.references,
                frame: child,
            });
            walk(child, depth + 1, id);
        });
    };
    walk(model.root, 0, "root");
    const normalizedQuery = normalizeProfileLabel(query);
    const filtered = normalizedQuery
        ? rows.filter((row) => normalizeProfileLabel(`${row.label} ${row.node_kind} ${row.role ?? ""}`).includes(normalizedQuery))
        : rows;
    const metric = (row) => {
        if (weight === "cost")
            return tableMode === "flat" ? row.self_micros : row.cum_micros;
        return tableMode === "flat" ? row.self_tokens : row.cum_tokens;
    };
    return filtered.sort((a, b) => metric(b) - metric(a) || stableCompare(a.label, b.label) || stableCompare(a.id, b.id));
}
