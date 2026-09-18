// Tauri-backed TareClient: the desktop window talks to the Rust core via `invoke(...)` instead
// of the loopback HTTP read API. The shared shell (main.ts) is identical across both — only the
// client differs. Commands that return a JSON string are parsed; object commands pass through.
// `explain` returns a raw narrative string (not JSON).
import { upgradeInvestigationEntityRefs } from "./analysis/investigation.js";
function defaultInvoke() {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const inv = globalThis.__TAURI__?.core?.invoke;
    if (!inv)
        throw new Error("Tauri invoke bridge unavailable");
    return inv;
}
export function createTauriClient(invoke = defaultInvoke()) {
    // A command whose Rust return is a JSON string -> parse to T.
    const j = async (cmd, args) => JSON.parse((await invoke(cmd, args)));
    return {
        listRuns: () => invoke("list_runs"),
        flamegraph: (runId) => invoke("run_flamegraph", { runId }),
        profile: (runId, sort, topN) => invoke("run_profile", { runId, sort, topN }),
        frontier: () => invoke("cost_frontier", {}),
        saveQuality: (runId, score, source = "cli") => invoke("set_run_quality", { runId, score, source }),
        report: () => j("report"),
        trend: (q) => j("trend", { by: q.by, from: q.from, to: q.to }),
        anomalies: (q) => j("anomalies", { by: q?.by, window: q?.window, threshold: q?.threshold }),
        costRegressions: (q) => j("cost_regressions", { window: q?.window, threshold: q?.threshold }),
        today: async () => {
            const t = (await invoke("today_spend"));
            return {
                run_count: t.run_count ?? 0,
                total_micros: t.total_micros,
                pricing_version: t.pricing_version ?? "",
                effective_date: t.effective_date ?? "",
            };
        },
        runStatus: (runId) => j("run_status", { runId }),
        runStatuses: () => j("run_statuses", {}),
        runMeta: (runId) => j("run_meta", { runId }),
        runSteps: (runId) => j("run_steps", { runId }),
        transcript: (runId, step) => j("transcript", { runId, step }),
        recentSteps: (n) => j("recent_steps", { n }),
        explain: (runId) => invoke("explain", { runId }),
        sessionAutopsy: (runId, median) => j("session_autopsy", { runId, median }),
        advise: () => j("advise"),
        savings: () => j("savings"),
        actionPlan: () => j("action_plan"),
        cacheLedger: () => j("cache_ledger"),
        reasoning: () => j("reasoning"),
        effectiveness: () => j("effectiveness"),
        confidence: () => j("confidence"),
        whatif: (crossProvider) => j("whatif", { crossProvider }),
        diff: (a, b) => j("diff", { a, b }),
        flameDiff: (a, b, normalized = false) => j("flame_diff", { a, b, normalized }),
        pricing: () => j("pricing"),
        receipt: (runId, maxPrivate = false) => j("receipt", { runId, maxPrivate }),
        rollup: (by, filter) => j("rollup", { by, filterBy: filter?.by, filter: filter?.label }),
        punchcard: () => j("punchcard"),
        heatmap: () => j("heatmap"),
        sessions: () => j("sessions"),
        correlate: () => j("correlate"),
        lineages: () => j("lineages"),
        units: () => j("units"),
        sessionsLive: () => j("sessions_live"),
        vendorToday: () => j("vendor_today"),
        burnrate: (range) => j("burnrate", { range }),
        coverage: () => j("coverage"),
        reconcile: (day) => j("reconcile", { day }),
        loops: () => j("loops"),
        failures: () => j("failures"),
        lenses: () => j("lenses"),
        budget: () => j("budget"),
        sandwich: (component) => j("sandwich", { component }),
        otlpStatus: () => j("otlp_status"),
        exportRun: (runId, format) => invoke("export", { runId, format }),
        config: () => j("get_config"),
        saveConfig: async (config) => {
            await invoke("save_config", { configJson: JSON.stringify(config) });
        },
        canSaveConfig: () => true,
        canControlProxy: () => true,
        proxyStatus: () => j("proxy_status"),
        proxyStart: (port) => j("start_proxy", { port }),
        proxyStop: () => j("stop_proxy"),
        canNotify: () => true,
        notify: async (title, body) => {
            await invoke("notify", { title, body });
        },
        // Window-close-to-background preference: the desktop reads/writes background.json
        // via the Rust commands; the close policy consumes it on the next close.
        canBackgroundOnClose: () => true,
        getBackgroundOnClose: () => invoke("get_background_on_close"),
        setBackgroundOnClose: async (enabled) => {
            await invoke("set_background_on_close", { enabled });
        },
        // Run notes: invoke the desktop commands (adapter fns in tare-tauri).
        getRunNote: (runId) => j("run_note", { runId }),
        saveRunNote: async (note) => {
            await invoke("save_run_note", { noteJson: JSON.stringify(note) });
        },
        deleteRunNote: async (runId) => {
            await invoke("delete_run_note", { runId });
        },
        runsByTag: (tag) => j("runs_by_tag", { tag }),
        starredRuns: () => j("starred_runs"),
        acknowledgeAnomaly: async (key) => {
            await invoke("acknowledge_anomaly", { key });
        },
        seedDemo: () => invoke("seed_demo").then((r) => (typeof r === "string" ? r : "demo")),
        purgeTranscripts: async () => {
            await invoke("transcript_purge");
        },
        configOrigins: () => j("config_origins"),
        // Calibrated Bench cohort analysis: the commands delegate to the SAME shared
        // tare-cli cohort_*_json fns the HTTP routes call, so desktop and browser return equivalent
        // data. Each command returns the AnalysisResponse JSON string, parsed by `j`. The typed request
        // is passed as a JSON `body` string (the command re-parses it into the Rust DTO).
        resolveCohort: (spec) => j("cohort_resolve", { body: JSON.stringify(spec) }),
        facetCohort: (req) => j("cohort_facets", { body: JSON.stringify(req) }),
        compareCohort: (req) => j("cohort_compare", { body: JSON.stringify(req) }),
        searchCohort: (req) => j("cohort_search", { body: JSON.stringify(req) }),
        timelineCohort: (req) => j("cohort_timeline", { body: JSON.stringify(req) }),
        anomalyWhy: (req) => j("anomaly_why", { body: JSON.stringify(req) }),
        runExperiment: (req) => j("experiment", { body: JSON.stringify(req) }),
        listInvestigations: () => j("list_investigations").then((rows) => rows.map(upgradeInvestigationEntityRefs)),
        saveInvestigation: async (inv) => {
            await invoke("save_investigation", { body: JSON.stringify(inv) });
        },
        deleteInvestigation: async (id) => {
            await invoke("delete_investigation", { id });
        },
        acceptSavings: async (req) => {
            await invoke("savings_accept", { body: JSON.stringify(req) });
        },
        dismissSavings: async (req) => {
            await invoke("savings_dismiss", { body: JSON.stringify(req) });
        },
        unacceptSavings: async (id) => {
            await invoke("savings_unaccept", { body: JSON.stringify(id) });
        },
        savingsActions: () => j("savings_actions"),
        verifySavings: (req) => j("savings_verify", { body: JSON.stringify(req) }),
    };
}
