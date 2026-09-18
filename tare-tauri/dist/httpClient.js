// A real TareClient backed by the loopback read API that `tare serve` exposes
// (GET /__tare/{runs,flamegraph,report,trend}). The browser viewer uses this; the desktop
// build can instead provide a Tauri-`invoke` client. All data is the Rust core's — estimated,
// counts only.
import { upgradeInvestigationEntityRefs } from "./analysis/investigation.js";
export function createHttpClient(base = "") {
    async function getJson(path) {
        const res = await fetch(`${base}${path}`);
        if (!res.ok) {
            throw new Error(`tare read API ${path}: HTTP ${res.status}`);
        }
        return (await res.json());
    }
    // Loopback POST — the write API (run notes). Same-origin as the served UI.
    async function postJson(path, body) {
        const res = await fetch(`${base}${path}`, {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify(body),
        });
        if (!res.ok) {
            throw new Error(`tare write API ${path}: HTTP ${res.status}`);
        }
        return (await res.json());
    }
    return {
        listRuns: () => getJson("/__tare/runs"),
        flamegraph: (runId) => getJson(`/__tare/flamegraph?run=${encodeURIComponent(runId)}`),
        sessionAutopsy: (runId, median) => {
            const m = median != null ? `&median=${Math.trunc(median)}` : "";
            return getJson(`/__tare/session_autopsy?id=${encodeURIComponent(runId)}${m}`);
        },
        profile: (runId, sort, topN) => {
            const p = new URLSearchParams({ run: runId });
            if (sort)
                p.set("sort", sort);
            if (topN != null)
                p.set("top", String(topN));
            return getJson(`/__tare/profile?${p.toString()}`);
        },
        frontier: () => getJson("/__tare/frontier"),
        saveQuality: (runId, score, source = "cli") => postJson("/__tare/quality", { run_id: runId, score, source }).then(() => undefined),
        report: () => getJson("/__tare/report"),
        trend: (q) => {
            const qs = trendQs(q);
            return getJson(`/__tare/trend${qs ? `?${qs}` : ""}`);
        },
        anomalies: (q = {}) => {
            const qs = trendQs(q);
            return getJson(`/__tare/anomalies${qs ? `?${qs}` : ""}`);
        },
        costRegressions: (q = {}) => {
            const p = new URLSearchParams();
            if (q.window != null)
                p.set("window", String(q.window));
            if (q.threshold != null)
                p.set("threshold", String(q.threshold));
            const qs = p.toString();
            return getJson(`/__tare/cost_regressions${qs ? `?${qs}` : ""}`);
        },
        today: () => getJson("/__tare/today"),
        runStatus: (runId) => getJson(`/__tare/run_status?run=${encodeURIComponent(runId)}`),
        runStatuses: () => getJson("/__tare/run_statuses"),
        runMeta: (runId) => getJson(`/__tare/run_meta?run=${encodeURIComponent(runId)}`),
        runSteps: (runId) => getJson(`/__tare/run_steps?run=${encodeURIComponent(runId)}`),
        transcript: (runId, step) => getJson(`/__tare/transcript?run=${encodeURIComponent(runId)}&step=${step}`),
        recentSteps: (n) => getJson(`/__tare/recent_steps${n ? `?n=${n}` : ""}`),
        explain: (runId) => getJson(`/__tare/explain?run=${encodeURIComponent(runId)}`).then((r) => r.explain),
        advise: () => getJson("/__tare/advise"),
        savings: () => getJson("/__tare/savings"),
        actionPlan: () => getJson("/__tare/action_plan"),
        cacheLedger: () => getJson("/__tare/cache_ledger"),
        reasoning: () => getJson("/__tare/reasoning"),
        effectiveness: () => getJson("/__tare/effectiveness"),
        confidence: () => getJson("/__tare/confidence"),
        whatif: (crossProvider) => getJson(`/__tare/whatif${crossProvider ? "?cross_provider=1" : ""}`),
        diff: (a, b) => getJson(`/__tare/diff?a=${encodeURIComponent(a)}&b=${encodeURIComponent(b)}`),
        flameDiff: (a, b, normalized = false) => getJson(`/__tare/flame_diff?a=${encodeURIComponent(a)}&b=${encodeURIComponent(b)}&normalized=${normalized}`),
        pricing: () => getJson("/__tare/pricing"),
        receipt: (runId, maxPrivate = false) => getJson(`/__tare/receipt?run=${encodeURIComponent(runId)}${maxPrivate ? "&profile=max_private" : ""}`),
        rollup: (by, filter) => getJson(`/__tare/rollup?by=${encodeURIComponent(by)}` +
            (filter
                ? `&filter_by=${encodeURIComponent(filter.by)}&filter=${encodeURIComponent(filter.label)}`
                : "")),
        punchcard: () => getJson("/__tare/punchcard"),
        heatmap: () => getJson("/__tare/heatmap"),
        sessions: () => getJson("/__tare/sessions"),
        correlate: () => getJson("/__tare/correlate"),
        lineages: () => getJson("/__tare/lineage"),
        units: () => getJson("/__tare/units"),
        sessionsLive: () => getJson("/__tare/sessions_live"),
        vendorToday: () => getJson("/__tare/vendor_today"),
        burnrate: (range) => getJson(`/__tare/burnrate${range ? `?range=${encodeURIComponent(range)}` : ""}`),
        coverage: () => getJson("/__tare/coverage"),
        reconcile: (day) => getJson(`/__tare/reconcile${day ? `?day=${encodeURIComponent(day)}` : ""}`),
        loops: () => getJson("/__tare/loops"),
        failures: () => getJson("/__tare/failures"),
        lenses: () => getJson("/__tare/lenses"),
        budget: () => getJson("/__tare/budget"),
        sandwich: (component) => getJson(`/__tare/sandwich?component=${encodeURIComponent(component)}`),
        exportRun: async (runId, format) => {
            const res = await fetch(`${base}/__tare/export?run=${encodeURIComponent(runId)}&format=${encodeURIComponent(format)}`);
            if (!res.ok)
                throw new Error(`HTTP ${res.status}`);
            return res.text();
        },
        config: () => getJson("/__tare/config"),
        saveConfig: () => Promise.reject(new Error("Editing capture settings needs the desktop app (or edit tare.toml directly). App preferences still work here.")),
        canSaveConfig: () => false,
        // The browser app is already served by a running proxy; it doesn't control one.
        canControlProxy: () => false,
        proxyStatus: () => Promise.resolve({ running: true, port: 0, url: base || "" }),
        proxyStart: () => Promise.reject(new Error("proxy control is desktop-only")),
        proxyStop: () => Promise.reject(new Error("proxy control is desktop-only")),
        // OS notifications are a desktop affordance; the browser uses in-app toasts only.
        canNotify: () => false,
        notify: () => Promise.resolve(),
        // The browser has no window-close-to-tray; the toggle is hidden and writes are no-ops.
        canBackgroundOnClose: () => false,
        getBackgroundOnClose: () => Promise.resolve(false),
        setBackgroundOnClose: () => Promise.resolve(),
        otlpStatus: () => getJson("/__tare/otlp_status"),
        getRunNote: (runId) => getJson(`/__tare/run_note?run_id=${encodeURIComponent(runId)}`),
        saveRunNote: (note) => postJson("/__tare/notes", note).then(() => undefined),
        deleteRunNote: (runId) => postJson("/__tare/notes/delete", { run_id: runId }).then(() => undefined),
        runsByTag: (tag) => getJson(`/__tare/notes_by_tag?tag=${encodeURIComponent(tag)}`),
        starredRuns: () => getJson("/__tare/starred_runs"),
        acknowledgeAnomaly: (key) => postJson("/__tare/acknowledge", { key }).then(() => undefined),
        seedDemo: () => postJson("/__tare/demo", {}).then(() => "demo"),
        purgeTranscripts: () => postJson("/__tare/transcript_purge", {}).then(() => undefined),
        configOrigins: () => Promise.resolve({}),
        // Calibrated Bench cohort analysis: POST the typed request, receive the
        // AnalysisResponse{data,provenance} envelope. The server wraps the shared cohort_*_json fns.
        resolveCohort: (spec) => postJson("/__tare/cohort/resolve", spec),
        facetCohort: (req) => postJson("/__tare/cohort/facets", req),
        compareCohort: (req) => postJson("/__tare/cohort/compare", req),
        searchCohort: (req) => postJson("/__tare/cohort/search", req),
        timelineCohort: (req) => postJson("/__tare/cohort/timeline", req),
        anomalyWhy: (req) => postJson("/__tare/anomaly_why", req),
        runExperiment: (req) => postJson("/__tare/experiment", req),
        listInvestigations: () => getJson("/__tare/investigations").then((rows) => rows.map(upgradeInvestigationEntityRefs)),
        saveInvestigation: (inv) => postJson("/__tare/investigations", inv).then(() => undefined),
        deleteInvestigation: (id) => postJson("/__tare/investigations/delete", { id }).then(() => undefined),
        acceptSavings: (req) => postJson("/__tare/savings/accept", req).then(() => undefined),
        dismissSavings: (req) => postJson("/__tare/savings/dismiss", req).then(() => undefined),
        unacceptSavings: (id) => postJson("/__tare/savings/unaccept", id).then(() => undefined),
        savingsActions: () => getJson("/__tare/savings/actions"),
        verifySavings: (req) => postJson("/__tare/savings/verify", req),
    };
}
function trendQs(q) {
    const p = new URLSearchParams();
    if (q.by)
        p.set("by", q.by);
    if (q.from)
        p.set("from", q.from);
    if (q.to)
        p.set("to", q.to);
    if (q.window != null)
        p.set("window", String(q.window));
    if (q.threshold != null)
        p.set("threshold", String(q.threshold));
    return p.toString();
}
