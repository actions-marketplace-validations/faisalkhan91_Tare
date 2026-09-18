//! Thin Tauri command adapter over `tare-core` + `tare-store`. The desktop shell is a
//! display layer only — all logic lives in the core and is reachable headlessly. The
//! adapter functions below are always compiled (and unit-tested); the actual Tauri
//! window + tray live behind the `gui` feature so the default workspace build stays
//! headless and offline-fast.

use tare_core::flamegraph::{build_flamegraph, FlamegraphModel};
use tare_core::model::TodaySpend;
use tare_core::money::MicroUsd;
use tare_core::trend::{TrendDimension, TrendReport};
use tare_core::{attribute, calendar, PricingTable};
use tare_store::{cached_runs, Store};

pub const SHIPPED_PRICING: &str = include_str!("../../pricing/pricing.json");
const IPC_BODY_CAP: usize = 256 * 1024;

fn ensure_ipc_body_size(body: &[u8]) -> Result<(), String> {
    if body.len() > IPC_BODY_CAP {
        Err("request body exceeds 256 KiB".into())
    } else {
        Ok(())
    }
}

/// The desktop app's config file path: `TARE_CONFIG` env, else the stable root
/// `$HOME/.tare/tare.toml`, co-located with the DB (`$HOME/.tare/tare.db`). The app is launched from
/// Finder with CWD `/`, so a relative `tare.toml` would never be found — reads AND writes must go
/// through this so the user's config is actually honored and persisted.
pub fn config_path() -> String {
    if let Ok(path) = std::env::var("TARE_CONFIG") {
        if !path.trim().is_empty() {
            return path;
        }
    }
    let home = std::env::var("HOME")
        .ok()
        .filter(|path| !path.trim().is_empty())
        .unwrap_or_else(|| ".".to_string());
    format!("{home}/.tare/tare.toml")
}

/// Load a pricing table from a `.toml`/`.json` path. Mirrors `tare_cli::load_pricing`'s file branch;
/// tare-tauri deliberately does not depend on tare-cli, so the few lines are duplicated rather than
/// the dependency rule broken.
fn load_pricing_file(path: &str) -> Result<PricingTable, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("read pricing {path}: {e}"))?;
    if std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"))
    {
        PricingTable::from_toml_str(&s)
    } else {
        PricingTable::from_json_str(&s)
    }
}

fn pricing() -> Result<PricingTable, String> {
    pricing_at(&config_path())
}

fn pricing_at(path: &str) -> Result<PricingTable, String> {
    // Desktop parity with the CLI: assemble via the ONE shared with_config, so the
    // self-hosted overlay, per-model overrides, AND reprice mode are ALL applied identically — the
    // desktop previously dropped overrides + reprice mode, diverging every dollar figure from the CLI.
    //
    // Memoized: this ran on EVERY command — re-parsing the ~shipped pricing JSON,
    // re-reading + re-parsing tare.toml from disk, and rebuilding the table each time (serve builds
    // its read_pricing ONCE; the desktop didn't). The config-applied table only changes when
    // tare.toml changes, so key the cache on that file's (mtime,len) signature. Correctness: any edit
    // to tare.toml flips the signature and rebuilds, so overrides/reprice-mode changes are honored.
    use std::sync::{Mutex, OnceLock};
    // Cache entry: the tare.toml (mtime, len) signature paired with the table assembled for it.
    // Named to keep the nested `OnceLock<Mutex<Option<…>>>` under clippy's type-complexity bar
    // — purely a readability/lint refactor, identical runtime behavior.
    // The signature covers tare.toml AND the `[proxy].pricing` file it may point at, so editing
    // either one rebuilds the table.
    type PricingCacheKey = (String, (u128, u64), Option<(String, (u128, u64))>);
    type PricingCacheEntry = (PricingCacheKey, PricingTable);
    static CACHE: OnceLock<Mutex<Option<PricingCacheEntry>>> = OnceLock::new();
    // Config is parsed every call (a small TOML) because the pricing path lives in it and is part of
    // the cache key; the expensive work — parsing the pricing table and applying with_config — stays
    // memoized.
    let cfg = tare_core::config::TareConfig::load(path)?;
    let key = (
        path.to_string(),
        file_sig(path),
        cfg.proxy
            .pricing
            .as_deref()
            .map(|pricing_path| (pricing_path.to_string(), file_sig(pricing_path))),
    );
    let cell = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cell.lock() {
        if let Some((cached_key, table)) = guard.as_ref() {
            if *cached_key == key {
                return Ok(table.clone());
            }
        }
    }
    // Base rates: the user's `[proxy].pricing` table when configured, else the bundled one. The
    // desktop previously had NO way to load a user-supplied table (`--pricing` is a `tare serve`
    // flag), so self-hosted `provider=local` models — whose only pricing mechanism this is — were
    // permanently unpriced on the desktop while the CLI priced them. An
    // unreadable or malformed configured file is an error: silently substituting bundled rates can
    // make self-hosted models appear free or apply the wrong provider rates.
    let table = match cfg.proxy.pricing.as_deref() {
        Some(path) => load_pricing_file(path)?,
        None => PricingTable::from_json_str(SHIPPED_PRICING)
            .map_err(|error| format!("bundled pricing is invalid: {error}"))?,
    };
    let table = table.with_config(&cfg);
    if let Ok(mut guard) = cell.lock() {
        *guard = Some((key, table.clone()));
    }
    Ok(table)
}

/// (mtime_nanos, len) of a file, or (0, 0) if absent — a cheap change signature for memoization.
fn file_sig(path: &str) -> (u128, u64) {
    match std::fs::metadata(path) {
        Ok(m) => {
            let nanos = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            (nanos, m.len())
        }
        Err(_) => (0, 0),
    }
}

/// Today's spend view-model for the tray / header.
pub fn today_spend_view(db_path: &str, date: &str) -> Result<TodaySpend, String> {
    let store = Store::open(db_path)?;
    store.today_spend(date, &pricing()?)
}

/// The flamegraph model for a run (serde-serializable; returned verbatim to the web UI).
pub fn run_flamegraph_view(db_path: &str, run_id: &str) -> Result<FlamegraphModel, String> {
    let store = Store::open(db_path)?;
    let run = store
        .load_run(run_id)?
        .ok_or_else(|| format!("run `{run_id}` not found"))?;
    Ok(build_flamegraph(&run, &pricing()?))
}

/// Per-session cost autopsy JSON, desktop parity with the CLI read route: exact class
/// decomposition + gated waste opportunities + fallback-ladder headline for one session. `median` (the
/// user's median session cost, which the UI has from the sessions list) powers the vs-median reference.
pub fn session_autopsy_json(
    db_path: &str,
    run_id: &str,
    median: Option<i64>,
) -> Result<String, String> {
    let store = Store::open(db_path)?;
    let run = store
        .load_run(run_id)?
        .ok_or_else(|| format!("run `{run_id}` not found"))?;
    let autopsy = tare_core::autopsy::session_autopsy(&run, &pricing()?, median);
    serde_json::to_string(&autopsy).map_err(|e| e.to_string())
}

/// Flat/cum profile table over a run's flamegraph. `sort` is "flat" | "cum"
/// (default cum); `top_n` truncates after ranking.
pub fn run_profile_view(
    db_path: &str,
    run_id: &str,
    sort: Option<String>,
    top_n: Option<usize>,
) -> Result<tare_core::flamegraph::ProfileTable, String> {
    let sort = match sort.as_deref() {
        Some("flat") => tare_core::flamegraph::ProfileSort::Flat,
        Some("cum") | None => tare_core::flamegraph::ProfileSort::Cum,
        Some(other) => return Err(format!("invalid profile sort {other:?}")),
    };
    if top_n == Some(0) {
        return Err("profile row limit must be at least 1".into());
    }
    let model = run_flamegraph_view(db_path, run_id)?;
    Ok(tare_core::flamegraph::profile_table(&model, sort, top_n))
}

/// Cost×quality frontier across all stored runs, joining per-run cost with the
/// ingested quality scalar. Pure read; no re-execution, no payload.
pub fn run_frontier_view(db_path: &str) -> Result<tare_core::experiment::Frontier, String> {
    let store = Store::open(db_path)?;
    let runs = cached_runs(db_path)?;
    let observations = store.all_run_quality()?;
    let quality: std::collections::BTreeMap<String, i64> = observations
        .iter()
        .map(|q| (q.run_id.clone(), q.score))
        .collect();
    let sources: std::collections::BTreeMap<String, String> = observations
        .into_iter()
        .map(|q| (q.run_id, q.source))
        .collect();
    Ok(
        tare_core::experiment::cost_quality_frontier_with_provenance(
            &runs,
            &pricing()?,
            &quality,
            &sources,
        ),
    )
}

/// All known run ids (newest persisted last).
pub fn list_run_ids(db_path: &str) -> Result<Vec<String>, String> {
    // Just the ids: a runs-table-only SELECT, not a full steps-scan reconstruction of
    // every run. all_run_ids is rowid ASC (newest last), matching what list consumers expect.
    Store::open(db_path)?.all_run_ids()
}

/// The `n` most recently persisted run ids, newest first — for the tray/menu recent-runs group.
/// Best-effort: an unopenable store yields an empty list rather than erroring the
/// menu build.
pub fn recent_run_ids(db_path: &str, n: usize) -> Vec<String> {
    Store::open(db_path)
        .and_then(|s| s.recent_run_ids(n))
        .unwrap_or_default()
}

/// Max entries surfaced in the native Views submenu (v2 investigations, newest-updated first).
const SAVED_VIEWS_MAX: usize = 20;

/// Minimal v2 investigation projection consumed by the native Views submenu. It comes directly from
/// the shared SQLite source of truth and navigates via a compact investigation id; no analysis state
/// or transient focus is copied into native memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedInvestigationMenuEntry {
    pub id: String,
    pub label: String,
    pub workspace: String,
}

/// Best-effort v2 menu projection, newest-updated first. Invalid opaque rows are skipped rather than
/// producing a native item that cannot be routed; the typed web/store boundary normally prevents
/// them, but the store intentionally persists the DTO opaquely.
pub fn saved_investigations_for_menu(db_path: &str) -> Vec<SavedInvestigationMenuEntry> {
    Store::open(db_path)
        .and_then(|store| store.list_investigations())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| {
            let id = row.get("id")?.as_str()?;
            let label = row.get("label")?.as_str()?;
            let workspace = row.get("state")?.get("workspace")?.as_str()?;
            if id.is_empty()
                || label.is_empty()
                || !matches!(workspace, "pulse" | "investigate" | "optimize")
            {
                return None;
            }
            Some(SavedInvestigationMenuEntry {
                id: id.to_string(),
                label: label.to_string(),
                workspace: workspace.to_string(),
            })
        })
        .take(SAVED_VIEWS_MAX)
        .collect()
}

fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(*byte as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Canonical native-menu destination for one v2 investigation. Query encoding is byte-wise UTF-8
/// percent encoding so migrated ids that themselves contain `#/…?…` cannot escape the id value.
pub fn saved_investigation_hash(entry: &SavedInvestigationMenuEntry) -> String {
    format!(
        "#/{workspace}?investigation={id}",
        workspace = entry.workspace,
        id = encode_query_value(&entry.id)
    )
}

/// Seed the bundled sample run into the store (onboarding activation), so a first-run
/// desktop user who hasn't captured anything yet still lands on a populated Overview. Shares the
/// core sample run + fixed date with the CLI's `tare demo`, and marks it `demo` (SAMPLE banner).
/// Idempotent: the demo run's fixed id means re-seeding overwrites rather than duplicates.
pub fn seed_demo(db_path: &str) -> Result<String, String> {
    let run = tare_core::demo::demo_run()?;
    let store = Store::open(db_path)?;
    for step in &run.steps {
        store.record_step_with_policy(
            step,
            tare_core::demo::DEMO_DATE,
            None,
            Some("demo"),
            Some("demo"),
        )?;
    }
    Ok(run.run_id)
}

/// The full trim report (reused by the web UI's trim panel).
pub fn report_json(db_path: &str) -> Result<String, String> {
    let report = attribute::build_report(&cached_runs(db_path)?, &pricing()?);
    serde_json::to_string(&report).map_err(|e| e.to_string())
}

// ---- Calibrated Bench cohort analysis ------------------------------
//
// The desktop adapter delegates to the SHARED `Store::cohort_*_response` envelope builders — the
// identical parse → engine → honest provenance → JSON logic the tare-cli HTTP routes call — so the
// desktop and browser transports return equivalent data for a given request. The store is clock-free,
// so this (clock-owning) edge stamps `refreshed_at`. The `(status, message)` error is flattened to
// the message string Tauri commands surface (HTTP status has no meaning over the invoke bridge).

/// RFC3339 UTC `refreshed_at` for a cohort response, from the system clock.
fn cohort_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    calendar::rfc3339_utc(secs)
}

/// Resolve a `CohortSpec` (JSON body) → enveloped `AnalysisResponse` JSON.
pub fn cohort_resolve_json(db_path: &str, body: &[u8]) -> Result<String, String> {
    Store::open(db_path)?
        .cohort_resolve_response(body, &pricing()?, &cohort_now())
        .map_err(|(_, m)| m)
}

/// Facet a selection vs baseline over one dimension → enveloped JSON.
pub fn cohort_facets_json(db_path: &str, body: &[u8]) -> Result<String, String> {
    Store::open(db_path)?
        .cohort_facets_response(body, &pricing()?, &cohort_now())
        .map_err(|(_, m)| m)
}

/// Compare selection vs baseline (volume/size/efficiency deltas) → enveloped JSON.
pub fn cohort_compare_json(db_path: &str, body: &[u8]) -> Result<String, String> {
    Store::open(db_path)?
        .cohort_compare_response(body, &pricing()?, &cohort_now())
        .map_err(|(_, m)| m)
}

/// Allow-listed cross-run search within a cohort → enveloped JSON.
pub fn cohort_search_json(db_path: &str, body: &[u8]) -> Result<String, String> {
    Store::open(db_path)?
        .cohort_search_response(body, &pricing()?, &cohort_now())
        .map_err(|(_, m)| m)
}

/// Dense scoped daily metrics + persisted local config markers → enveloped JSON.
pub fn cohort_timeline_json(db_path: &str, body: &[u8]) -> Result<String, String> {
    let cfg = tare_core::config::TareConfig::load(&config_path())?;
    Store::open(db_path)?
        .cohort_timeline_response(body, &pricing()?, &cohort_now(), &cfg.unit)
        .map_err(|(_, m)| m)
}

/// Scoped anomaly explanation: delegates to the SHARED `Store::anomaly_why`
/// (resolve scope → detect → deterministic volume/size/efficiency decomposition) the HTTP route also
/// calls, so desktop and browser return identical rows.
pub fn anomaly_why_json(db_path: &str, body: &[u8]) -> Result<String, String> {
    ensure_ipc_body_size(body)?;
    let req: tare_core::anomaly::AnomalyWhyRequest =
        serde_json::from_slice(body).map_err(|e| format!("bad AnomalyWhyRequest: {e}"))?;
    let whys = Store::open(db_path)?.anomaly_why(&req, &pricing()?)?;
    serde_json::to_string(&whys).map_err(|e| e.to_string())
}

/// Hierarchical node-level flame diff over an explicit run pair: delegates to
/// the SHARED `Store::flame_diff` the HTTP route also calls, so desktop and browser return identical
/// models. Distinct from the row-level report `diff` command.
pub fn flame_diff_json(
    db_path: &str,
    a: &str,
    b: &str,
    normalized: bool,
) -> Result<String, String> {
    let d = Store::open(db_path)?.flame_diff(a, b, normalized, &pricing()?)?;
    serde_json::to_string(&d).map_err(|e| e.to_string())
}

/// Offline counterfactual cost experiment over a cohort: delegates to the
/// SHARED `Store::experiment` the HTTP route also calls, so desktop and browser return identical
/// grids. `body` is an ExperimentRequest (cohort + axis grid + optional quality constraint).
pub fn experiment_json(db_path: &str, body: &[u8]) -> Result<String, String> {
    ensure_ipc_body_size(body)?;
    let req: tare_core::experiment::ExperimentRequest =
        serde_json::from_slice(body).map_err(|e| format!("bad ExperimentRequest: {e}"))?;
    let result = Store::open(db_path)?.experiment(&req, &pricing()?)?;
    serde_json::to_string(&result).map_err(|e| e.to_string())
}

// ---- Saved investigations: same shared store the HTTP routes use ----

/// List saved investigations as a JSON array of DTOs (newest-updated first).
pub fn list_investigations_json(db_path: &str) -> Result<String, String> {
    let v = Store::open(db_path)?.list_investigations()?;
    serde_json::to_string(&v).map_err(|e| e.to_string())
}

/// Upsert one investigation from its full DTO JSON. The store enforces the size caps.
pub fn upsert_investigation_json(db_path: &str, body: &[u8]) -> Result<(), String> {
    ensure_ipc_body_size(body)?;
    let dto: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("bad SavedInvestigation: {e}"))?;
    Store::open(db_path)?.upsert_investigation(&dto)
}

/// Delete a saved investigation by id (idempotent).
pub fn delete_investigation(db_path: &str, id: &str) -> Result<(), String> {
    Store::open(db_path)?.delete_investigation(id)
}

/// Spend trend over a window, as a `TrendReport`. Clock-free: when `from`/`to` are omitted the
/// window defaults to the last 14 days *of recorded data* (upper bound = latest run date), so
/// it never depends on the wall clock. Empty store -> an empty report.
pub fn trend_view(
    db_path: &str,
    from: Option<&str>,
    to: Option<&str>,
    by: &str,
) -> Result<TrendReport, String> {
    let store = Store::open(db_path)?;
    let dim = TrendDimension::parse(by).ok_or_else(|| format!("invalid trend dimension {by:?}"))?;
    let pricing = pricing()?;
    let Some((from, to)) = store.resolve_trend_window(from, to)? else {
        // No data yet: a well-formed empty report.
        return Ok(TrendReport {
            dimension: dim.as_str().to_string(),
            from: String::new(),
            to: String::new(),
            days: Vec::new(),
            series: Vec::new(),
            pricing_version: pricing.version,
            estimated: true,
        });
    };
    store.trend_in_range(&from, &to, &pricing, dim)
}

/// `trend_view` serialized to JSON (returned verbatim to the web UI / read endpoint).
pub fn trend_json(
    db_path: &str,
    from: Option<&str>,
    to: Option<&str>,
    by: &str,
) -> Result<String, String> {
    serde_json::to_string(&trend_view(db_path, from, to, by)?).map_err(|e| e.to_string())
}

/// Settled per-run status: `{run_id, micros, steps, top_cause}` (mirrors the read API / MCP).
pub fn run_status_json(db_path: &str, run_id: &str) -> Result<String, String> {
    let store = Store::open(db_path)?;
    let run = store
        .load_run(run_id)?
        .ok_or_else(|| format!("run `{run_id}` not found"))?;
    let p = pricing()?;
    let report = attribute::build_report(std::slice::from_ref(&run), &p);
    // Lifecycle flags for the runs-table state pill: mirror the serve endpoint.
    let unpriced = run.steps.iter().any(|s| {
        s.usage.total() > 0
            && p.lookup(s.provider, s.shape.vendor.as_deref(), &s.model)
                .is_none()
    });
    let errored = run
        .steps
        .last()
        .map(|s| s.is_retry_worthy_failure())
        .unwrap_or(false);
    serde_json::to_string(&serde_json::json!({
        "run_id": run_id,
        "micros": report.total_micros,
        "steps": run.steps.len(),
        "top_cause": report.rows.first().map(|r| r.cause.clone()),
        "tokens": run.steps.iter().fold(0u64, |total, step| total.saturating_add(step.usage.total())),
        "last_model": run.steps.last().map(|s| s.model.clone()).unwrap_or_default(),
        "unpriced": unpriced,
        "errored": errored,
    }))
    .map_err(|e| e.to_string())
}

/// Batch of every run's status in ONE pass over the cached run set: the Runs list
/// fetches all rows in a single call instead of a per-run round-trip (an N+1 that also re-opened the
/// DB N times). Same per-run object shape as `run_status_json`.
pub fn run_statuses_json(db_path: &str) -> Result<String, String> {
    let runs = tare_store::cached_runs(db_path)?;
    let p = pricing()?;
    let out: Vec<serde_json::Value> = runs
        .iter()
        .map(|run| {
            let report = attribute::build_report(std::slice::from_ref(run), &p);
            let unpriced = run.steps.iter().any(|s| {
                s.usage.total() > 0
                    && p.lookup(s.provider, s.shape.vendor.as_deref(), &s.model)
                        .is_none()
            });
            let errored = run
                .steps
                .last()
                .map(|s| s.is_retry_worthy_failure())
                .unwrap_or(false);
            serde_json::json!({
                "run_id": run.run_id,
                "micros": report.total_micros,
                "steps": run.steps.len(),
                "top_cause": report.rows.first().map(|r| r.cause.clone()),
                "tokens": run.steps.iter().fold(0u64, |total, step| total.saturating_add(step.usage.total())),
                "last_model": run.steps.last().map(|s| s.model.clone()).unwrap_or_default(),
                "unpriced": unpriced,
                "errored": errored,
            })
        })
        .collect();
    serde_json::to_string(&out).map_err(|e| e.to_string())
}

/// Ordered per-step timeline (counts + cost) for a run's Inspect tab.
pub fn run_steps_json(db_path: &str, run_id: &str) -> Result<String, String> {
    let store = Store::open(db_path)?;
    let run = store
        .load_run(run_id)?
        .ok_or_else(|| format!("run `{run_id}` not found"))?;
    let p = pricing()?;
    let steps: Vec<serde_json::Value> = run
        .steps
        .iter()
        .map(|s| {
            let micros = p
                .lookup(s.provider, s.shape.vendor.as_deref(), &s.model)
                .map(|r| {
                    tare_core::account::cost_usage(&s.usage, r, &s.shape)
                        .total
                        .micros()
                })
                .unwrap_or(0);
            serde_json::json!({
                "ordinal": s.step_ordinal,
                "provider": s.provider.as_str(),
                "model": s.model,
                "fresh_input": s.usage.fresh_input,
                "cache_read": s.usage.cache_read,
                "cache_write": s.usage.cache_write(),
                "output": s.usage.output,
                "reasoning": s.usage.reasoning,
                "tokens": s.usage.total(),
                "micros": micros,
                "stop_reason": s.stop_reason,
                "duration_ms": s.duration_ms,
                // Timing/span fields: decimal strings, null when absent (step-order-only
                // capture). end is derived from start + duration, never persisted redundantly.
                "start_unix_nano": s.start_unix_nano.map(|n| n.to_decimal_string()),
                "end_unix_nano": s.end_unix_nano().map(|n| n.to_decimal_string()),
                "trace_id": s.trace_id,
                "span_id": s.span_id,
                "parent_span_id": s.parent_span_id,
                "anatomy": tare_core::model::prompt_anatomy(&s.shape),
            })
        })
        .collect();
    serde_json::to_string(&steps).map_err(|e| e.to_string())
}

/// The live activity tail — most recent steps across all runs, newest first.
pub fn recent_steps_json(db_path: &str, n: u32) -> Result<String, String> {
    if !(1..=50).contains(&n) {
        return Err("recent step count must be between 1 and 50".into());
    }
    let store = Store::open(db_path)?;
    let p = pricing()?;
    let out: Vec<serde_json::Value> = store
        .recent_steps(n)?
        .iter()
        .map(|s| {
            let micros = p
                .lookup(s.provider, s.shape.vendor.as_deref(), &s.model)
                .map(|r| {
                    tare_core::account::cost_usage(&s.usage, r, &s.shape)
                        .total
                        .micros()
                })
                .unwrap_or(0);
            serde_json::json!({
                "run_id": s.run_id,
                "ordinal": s.step_ordinal,
                "provider": s.provider.as_str(),
                "model": s.model,
                "fresh_input": s.usage.fresh_input,
                "cache_read": s.usage.cache_read,
                "cache_write": s.usage.cache_write(),
                "output": s.usage.output,
                "reasoning": s.usage.reasoning,
                "tokens": s.usage.total(),
                "micros": micros,
                "stop_reason": s.stop_reason,
                "duration_ms": s.duration_ms,
            })
        })
        .collect();
    serde_json::to_string(&out).map_err(|e| e.to_string())
}

// ---- run notes: local-only annotations, mirrored for the desktop adapter ----

/// The user's note for a run as JSON (`null` when none).
pub fn run_note_json(db_path: &str, run_id: &str) -> Result<String, String> {
    let note = Store::open(db_path)?.load_run_note(run_id)?;
    serde_json::to_string(&note).map_err(|e| e.to_string())
}

/// Attach a user quality scalar to a run (desktop side; mirrors the serve
/// `/__tare/quality` write). Stores the number + its provenance, never grades it. `updated` is a
/// unix-secs stamp (the app may read the clock; the pure core never does).
pub fn set_run_quality(
    db_path: &str,
    run_id: &str,
    score: i64,
    source: &str,
) -> Result<(), String> {
    if !matches!(source, "cli" | "header" | "ci" | "ui") {
        return Err("source must be one of: cli | header | ci | ui".into());
    }
    if !(0..=100).contains(&score) {
        return Err("score must be an integer from 0 to 100".into());
    }
    let updated = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string();
    let store = Store::open(db_path)?;
    if store.load_run(run_id)?.is_none() {
        return Err("cannot score an unknown run".into());
    }
    store.set_run_quality(run_id, score, source, &updated)
}

/// Upsert a run's note from `{run_id, tags, note_text, starred}` JSON. Input types, tag syntax,
/// duplicates, request size, and run existence are checked before writing.
pub fn save_run_note(db_path: &str, note_json: &str) -> Result<(), String> {
    ensure_ipc_body_size(note_json.as_bytes())?;
    let v: serde_json::Value =
        serde_json::from_str(note_json).map_err(|e| format!("bad json: {e}"))?;
    if !v.is_object() {
        return Err("request body must be a JSON object".into());
    }
    let run_id = v.get("run_id").and_then(|x| x.as_str()).unwrap_or("");
    if run_id.is_empty() || run_id.len() > 128 || run_id.chars().any(char::is_control) {
        return Err("run_id required (1-128 bytes, no control characters)".to_string());
    }
    let note_text = match v.get("note_text") {
        Some(value) => value.as_str().ok_or("note_text must be a string")?,
        None => "",
    };
    let starred = match v.get("starred") {
        Some(value) => value.as_bool().ok_or("starred must be a boolean")?,
        None => false,
    };
    let mut tags: Vec<String> = Vec::new();
    let mut unique_tags = std::collections::BTreeSet::new();
    if let Some(value) = v.get("tags") {
        let arr = value.as_array().ok_or("tags must be an array of strings")?;
        for tag in arr {
            let tag = tag.as_str().ok_or("tags must be strings")?;
            if tag.is_empty()
                || tag.len() > 32
                || !tag
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                || !unique_tags.insert(tag)
            {
                return Err("each tag must be a unique 1-32 byte [A-Za-z0-9_-] value".to_string());
            }
            tags.push(tag.to_string());
        }
    }
    if tags.len() > 24 {
        return Err("too many tags (max 24)".to_string());
    }
    let tags_json = serde_json::to_string(&tags).map_err(|e| format!("serialize tags: {e}"))?;
    let updated = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string();
    let store = Store::open(db_path)?;
    if store.load_run(run_id)?.is_none() {
        return Err("cannot annotate an unknown run".into());
    }
    store.upsert_run_note(run_id, &tags_json, note_text, starred, &updated)
}

/// Purge a run's note.
pub fn delete_run_note(db_path: &str, run_id: &str) -> Result<(), String> {
    Store::open(db_path)?.delete_run_note(run_id)
}

/// Run ids tagged with `tag`, as a JSON array.
pub fn runs_by_tag_json(db_path: &str, tag: &str) -> Result<String, String> {
    let ids = Store::open(db_path)?.notes_by_tag(tag)?;
    serde_json::to_string(&ids).map_err(|e| e.to_string())
}

/// Starred run ids, as a JSON array.
pub fn starred_runs_json(db_path: &str) -> Result<String, String> {
    let ids = Store::open(db_path)?.list_starred()?;
    serde_json::to_string(&ids).map_err(|e| e.to_string())
}

/// Plain-language narrative for a run (reuses the core explainer).
pub fn explain_view(db_path: &str, run_id: &str) -> Result<String, String> {
    let store = Store::open(db_path)?;
    let run = store
        .load_run(run_id)?
        .ok_or_else(|| format!("run `{run_id}` not found"))?;
    let p = pricing()?;
    let runs = std::slice::from_ref(&run);
    let report = attribute::build_report(runs, &p);
    let ledger = tare_core::savings::savings(runs, &p);
    Ok(tare_core::explain::explain(&report, &ledger))
}

/// Sibling path of the SEPARATE transcript store: `tare.db` → `tare.transcripts.db`.
/// Mirrors `tare_cli::transcript_db_path` (the adapter reimplements over store rather than depending
/// on tare-cli), keeping the payload-free counts DB physically apart from any captured bodies.
fn transcript_db_path(db_path: &str) -> String {
    let base = db_path.strip_suffix(".db").unwrap_or(db_path);
    format!("{base}.transcripts.db")
}

/// One step's redacted request/response bodies (Inspect layer), serialized as
/// `{"req":…,"resp":…}` or JSON `null` when nothing was captured (the default profile stores no
/// bodies; only `max_inspect` does). Returns `null` when the transcript store doesn't exist yet, so
/// the UI shows an honest "not captured" state rather than erroring.
pub fn transcript_json(db_path: &str, run_id: &str, step_ordinal: u32) -> Result<String, String> {
    if step_ordinal == 0 {
        return Err("step ordinal must be at least 1".into());
    }
    let tpath = transcript_db_path(db_path);
    let body = match std::fs::metadata(&tpath) {
        Ok(_) => tare_store::TranscriptStore::open(&tpath)?.get_by_step(run_id, step_ordinal)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("inspect transcript store {tpath}: {error}")),
    };
    let dto = body.map(|(req, resp, truncated)| {
        serde_json::json!({ "req": req, "resp": resp, "truncated": truncated })
    });
    serde_json::to_string(&dto).map_err(|e| e.to_string())
}

/// One-click opt-out: purge EVERY captured transcript from the sibling store. Idempotent
/// (a no-op when the store doesn't exist yet); never touches the counts DB. Returns rows removed.
pub fn transcript_purge_all(db_path: &str) -> Result<usize, String> {
    let tpath = transcript_db_path(db_path);
    match std::fs::metadata(&tpath) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("inspect transcript store {tpath}: {error}")),
    }
    tare_store::TranscriptStore::open(&tpath)?.purge_all()
}

/// Prompt-cache advice (estimated), serialized for the web UI.
pub fn advise_json(db_path: &str) -> Result<String, String> {
    let advice = tare_core::advise::advise(&cached_runs(db_path)?, &pricing()?);
    serde_json::to_string(&advice).map_err(|e| e.to_string())
}

/// The unified Savings Ledger as JSON, now the additive `SavingsLedgerV2`: a superset of the v1
/// ledger, so old consumers keep working while new ones get the
/// OpportunityV2 evidence rows + capped/applied/observed totals. Desktop parity with /__tare/savings.
pub fn savings_json(db_path: &str) -> Result<String, String> {
    let pricing = pricing()?;
    let lifecycle = Store::open(db_path)?.savings_lifecycle_totals(&pricing)?;
    let led =
        tare_core::savings::savings_v2_with_lifecycle(&cached_runs(db_path)?, &pricing, lifecycle);
    serde_json::to_string(&led).map_err(|e| e.to_string())
}

// ---- Savings action lifecycle: same shared store the HTTP routes use ----

/// Apply (`status="applied"`) or dismiss (`status="dismissed"`) a savings action. `body` is a
/// SavingsActionRequest; `acted_at` is stamped here (the store is clock-free). The `(status,message)`
/// error is flattened to the message string Tauri commands surface (409/404 lose their code here).
fn savings_action(db_path: &str, body: &[u8], status: &str) -> Result<(), String> {
    ensure_ipc_body_size(body)?;
    let req: tare_core::savings::SavingsActionRequest =
        serde_json::from_slice(body).map_err(|e| format!("bad SavingsActionRequest: {e}"))?;
    Store::open(db_path)?
        .put_savings_action(&req, status, &cohort_now())
        .map_err(|(_, m)| m)
}

pub fn savings_accept_json(db_path: &str, body: &[u8]) -> Result<(), String> {
    savings_action(db_path, body, "applied")
}

pub fn savings_dismiss_json(db_path: &str, body: &[u8]) -> Result<(), String> {
    savings_action(db_path, body, "dismissed")
}

/// Unaccept: remove the action row by identity. `body` is a SavingsActionIdentity.
pub fn savings_unaccept_json(db_path: &str, body: &[u8]) -> Result<(), String> {
    ensure_ipc_body_size(body)?;
    let id: tare_core::savings::SavingsActionIdentity =
        serde_json::from_slice(body).map_err(|e| format!("bad SavingsActionIdentity: {e}"))?;
    Store::open(db_path)?
        .delete_savings_action(&id)
        .map_err(|(_, m)| m)
}

/// The persisted savings actions (applied/dismissed) as JSON, each with compatibility warnings.
pub fn savings_actions_json(db_path: &str) -> Result<String, String> {
    let actions = Store::open(db_path)?.list_savings_actions()?;
    serde_json::to_string(&actions).map_err(|e| e.to_string())
}

/// Cohort-scoped observed-reduction verification: same shared store the HTTP
/// route uses. `body` is a SavingsVerifyRequest.
pub fn savings_verify_json(db_path: &str, body: &[u8]) -> Result<String, String> {
    ensure_ipc_body_size(body)?;
    let req: tare_core::savings::SavingsVerifyRequest =
        serde_json::from_slice(body).map_err(|e| format!("bad SavingsVerifyRequest: {e}"))?;
    let res = Store::open(db_path)?
        .verify_savings_action(&req, &pricing()?)
        .map_err(|(_, m)| m)?;
    serde_json::to_string(&res).map_err(|e| e.to_string())
}

/// The unified Action Plan as JSON — desktop parity with `/__tare/action_plan`.
pub fn action_plan_json(db_path: &str) -> Result<String, String> {
    let plan = tare_core::action_plan::action_plan(&cached_runs(db_path)?, &pricing()?);
    serde_json::to_string(&plan).map_err(|e| e.to_string())
}

/// Realized cache-savings ledger as JSON — desktop parity with `/__tare/cache_ledger`.
pub fn cache_ledger_json(db_path: &str) -> Result<String, String> {
    let led = tare_core::cache_ledger::cache_ledger(&cached_runs(db_path)?, &pricing()?);
    serde_json::to_string(&led).map_err(|e| e.to_string())
}

/// Reasoning/thinking-token breakout as JSON — parity with `/__tare/reasoning`.
pub fn reasoning_json(db_path: &str) -> Result<String, String> {
    let r = tare_core::reasoning::reasoning_breakout(&cached_runs(db_path)?, &pricing()?);
    serde_json::to_string(&r).map_err(|e| e.to_string())
}

/// Estimate-Confidence as JSON: pricing freshness + unpriced + coverage -> a label.
pub fn confidence_json(db_path: &str, today: &str) -> Result<String, String> {
    let runs = cached_runs(db_path)?;
    let p = pricing()?;
    let report = attribute::build_report(&runs, &p);
    let total_tokens = tare_core::lenses::lenses(&runs, &p).total_tokens;
    let unpriced_tokens = report
        .unpriced
        .iter()
        .fold(0u64, |total, row| total.saturating_add(row.token_total));
    let unpriced_share = unpriced_tokens
        .saturating_mul(100)
        .checked_div(total_tokens)
        .unwrap_or(0) as i64;
    let age = calendar::parse_date(&p.effective_date)
        .zip(calendar::parse_date(today))
        .map(|(eff, now)| now - eff)
        .unwrap_or(0);
    // Coverage is honestly `unknown` — out-of-band capture has no denominator; never
    // claim full coverage. Mirrors tare-cli `confidence_over`.
    serde_json::to_string(&tare_core::confidence::confidence(
        age,
        unpriced_share,
        tare_core::confidence::CoverageStatus::Unknown,
        None,
    ))
    .map_err(|e| e.to_string())
}

/// Cost-effectiveness (dollars per outcome) over all captured days as JSON.
pub fn effectiveness_json(db_path: &str) -> Result<String, String> {
    let store = Store::open(db_path)?;
    let mut eff = tare_core::effectiveness::effectiveness(
        &store.metered_outcomes("0001-01-01", "9999-12-31")?,
    );
    let split = tare_core::session::run_outcome_split(&cached_runs(db_path)?, &pricing()?);
    eff.cost_per_successful_run_micros = split.cost_per_successful_micros;
    eff.run_success_rate_pct = split.success_rate_pct;
    serde_json::to_string(&eff).map_err(|e| e.to_string())
}

/// What-if recommendations (cheaper priced models, ranked). Every row stays approximate.
pub fn whatif_json(db_path: &str, cross_provider: bool) -> Result<String, String> {
    let rec = tare_core::whatif::recommend(&cached_runs(db_path)?, &pricing()?, cross_provider);
    serde_json::to_string(&rec).map_err(|e| e.to_string())
}

/// Deterministic spend anomalies over a trend window + dimension (desktop parity with the HTTP
/// read API, which honors `by`/`window`/`threshold`). `by` selects the trend dimension
/// (total/provider/model/cause); unknown values are rejected.
pub fn anomalies_json(
    db_path: &str,
    by: &str,
    window: usize,
    threshold_pct: i64,
) -> Result<String, String> {
    if window == 0 {
        return Err("anomaly window must be at least 1".into());
    }
    if threshold_pct < 0 {
        return Err("anomaly threshold must not be negative".into());
    }
    let trend = trend_view(db_path, None, None, by)?;
    // Mirror `tare_cli::anomalies_for` exactly. Bare `detect` is
    // `detect_filtered(.., NoiseFilters::default())` and applies no acknowledgement filter, so the
    // desktop showed anomalies the CLI and the web deliberately suppress: the user's `[anomaly]`
    // noise floors were ignored, and every anomaly they had explicitly acknowledged as a false
    // positive came back. Both default to permissive/empty, so this is a no-op unless configured.
    let cfg = tare_core::config::TareConfig::load(&config_path())?;
    let found = tare_core::anomaly::filter_acknowledged(
        tare_core::anomaly::detect_filtered(
            &trend,
            window,
            threshold_pct,
            cfg.anomaly.noise_filters(),
        ),
        &cfg.anomaly.acknowledged,
    );
    serde_json::to_string(&found).map_err(|e| e.to_string())
}

/// Outcome-aware cost-effectiveness regressions: days whose $/outcome jumped above its
/// trailing-window baseline. Desktop parity with the `/__tare/cost_regressions` read API.
pub fn cost_regressions_json(
    db_path: &str,
    window: usize,
    threshold_pct: i64,
) -> Result<String, String> {
    if window == 0 {
        return Err("regression window must be at least 1".into());
    }
    if threshold_pct < 0 {
        return Err("regression threshold must not be negative".into());
    }
    let store = Store::open(db_path)?;
    let days = store.outcomes_by_day("0001-01-01", "9999-12-31")?;
    let found = tare_core::cost_regression::detect(&days, window, threshold_pct);
    serde_json::to_string(&found).map_err(|e| e.to_string())
}

/// Per-cause + total diff between two stored runs.
pub fn diff_json(db_path: &str, a: &str, b: &str) -> Result<String, String> {
    let store = Store::open(db_path)?;
    let pricing = pricing()?;
    let load = |id: &str| -> Result<_, String> {
        let run = store
            .load_run(id)?
            .ok_or_else(|| format!("run `{id}` not found"))?;
        Ok(attribute::build_report(
            std::slice::from_ref(&run),
            &pricing,
        ))
    };
    let d = tare_core::diff::diff_reports(&load(a)?, &load(b)?);
    serde_json::to_string(&d).map_err(|e| e.to_string())
}

/// Spend grouped by a correlation label (step/component/parent) — the Segments view.
///
/// `filter_by`/`filter` power the progressive filter-preserving drill: when both are
/// present + valid, restrict to the steps under that parent bucket before bucketing by `by` (e.g.
/// `by=session, filter_by=template, filter=template#…` → "how did this template's spend split across
/// sessions?"). Mirrors the serve `/__tare/rollup` route so the desktop and browser drill identically.
pub fn rollup_json(
    db_path: &str,
    by: &str,
    filter_by: Option<&str>,
    filter: Option<&str>,
) -> Result<String, String> {
    let dim = tare_core::rollup::RollupDim::parse(by)
        .ok_or_else(|| format!("invalid rollup dimension {by:?}"))?;
    let filter = match (filter_by, filter) {
        (Some(raw), Some(label)) => {
            let dimension = tare_core::rollup::RollupDim::parse(raw)
                .ok_or_else(|| format!("invalid rollup filter dimension {raw:?}"))?;
            if label.is_empty() {
                return Err("rollup filter must not be empty".into());
            }
            Some((dimension, label))
        }
        (None, None) => None,
        _ => return Err("rollup filter_by and filter must be supplied together".into()),
    };
    let rep = tare_core::rollup::rollup_filtered(&cached_runs(db_path)?, &pricing()?, dim, filter);
    serde_json::to_string(&rep).map_err(|e| e.to_string())
}

/// Day×hour spend punchcard for the desktop — mirrors the serve `/__tare/punchcard`
/// route and the `tare punchcard` CLI, all delegating to the shared `punchcard_from_runs` core so the
/// three surfaces agree. Runs with no stored hour are an honest GAP (excluded).
pub fn punchcard_json(db_path: &str) -> Result<String, String> {
    let store = Store::open(db_path)?;
    let day_hours: std::collections::HashMap<String, (String, Option<u8>)> = store
        .run_day_hours()?
        .into_iter()
        .map(|(id, date, hour)| (id, (date, hour)))
        .collect();
    let m =
        tare_core::punchcard::punchcard_from_runs(&cached_runs(db_path)?, &day_hours, &pricing()?);
    serde_json::to_string(&m).map_err(|e| e.to_string())
}

/// Calendar spend heatmap for the desktop — mirrors the serve `/__tare/heatmap` route
/// and `tare heatmap`, all delegating to the shared `heatmap_from_runs` core. Full history (no window).
pub fn heatmap_json(db_path: &str) -> Result<String, String> {
    let store = Store::open(db_path)?;
    let Some((from, to)) = store.run_date_bounds()? else {
        return serde_json::to_string(&tare_core::heatmap::calendar_heatmap(&[]))
            .map_err(|e| e.to_string());
    };
    let dated: Vec<(tare_core::model::RunRecord, String)> = store
        .load_dated_runs_in_range(&from, &to)?
        .into_iter()
        .map(|d| (d.run, d.date))
        .collect();
    let m = tare_core::heatmap::heatmap_from_runs(&dated, &pricing()?, None);
    serde_json::to_string(&m).map_err(|e| e.to_string())
}

/// Today's date in the user's local calendar day (UTC + configured offset), matching the CLI so
/// the desktop's "today" and a `tare report --today` agree. Offset: `TARE_TZ_OFFSET_MINUTES` env
/// > `[ui] tz_offset_minutes` in tare.toml > 0 (UTC).
pub fn today_local() -> Result<String, String> {
    let env_offset = match std::env::var("TARE_TZ_OFFSET_MINUTES") {
        Ok(raw) => {
            let offset = raw
                .parse::<i64>()
                .map_err(|_| format!("TARE_TZ_OFFSET_MINUTES must be an integer, got {raw:?}"))?;
            if !(-840..=840).contains(&offset) {
                return Err("TARE_TZ_OFFSET_MINUTES must be between -840 and 840".into());
            }
            Some(offset)
        }
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err("TARE_TZ_OFFSET_MINUTES is not valid Unicode".into())
        }
    };
    let offset = match env_offset {
        Some(offset) => offset,
        None => tare_core::config::TareConfig::load(&config_path())?
            .ui
            .tz_offset_minutes
            .unwrap_or(0),
    };
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    Ok(tare_core::calendar::civil_date_for(secs, offset))
}

/// Live running-sessions from the DURABLE mirror, classified here so the desktop has
/// a fallback when no `tare serve` is reachable to ask its in-memory table. Windows match the
/// receiver: working <=15s, idle <=5m, else ended.
pub fn sessions_live_json(db_path: &str) -> Result<String, String> {
    let store = Store::open(db_path)?;
    let rows = store.load_session_activity()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Per-session lifetime cost so the desktop list can show $ and float by spend. Polled on the
    // Live cadence, so it reads through the shared run cache — a hit unless new steps
    // landed since the last poll, instead of a full rescan every tick.
    let costs: std::collections::HashMap<String, i64> =
        tare_core::session::sessions(&cached_runs(db_path)?, &pricing()?)
            .rows
            .into_iter()
            .map(|r| (r.session, r.micros))
            .collect();
    let mut live: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(source, session, last_unix, events, last_model)| {
            let age = now.saturating_sub(last_unix);
            let state = if age <= 15 {
                "working"
            } else if age <= 300 {
                "idle"
            } else {
                "ended"
            };
            let micros = costs.get(&session).copied().unwrap_or(0);
            serde_json::json!({
                "session": session, "source": source, "state": state,
                "last_seen_age_s": age, "events": events, "last_model": last_model,
                "micros": micros,
                // `phase` (working-vs-waiting) is in-memory only in the receiver; the durable
                // mirror doesn't persist it, so the desktop DB-fallback reports it empty.
                "phase": "",
            })
        })
        .collect();
    live.sort_by_key(|v| {
        v.get("last_seen_age_s")
            .and_then(|x| x.as_u64())
            .unwrap_or(0)
    });
    serde_json::to_string(&live).map_err(|e| e.to_string())
}

pub fn sessions_json(db_path: &str) -> Result<String, String> {
    let rep = tare_core::session::sessions(&cached_runs(db_path)?, &pricing()?);
    serde_json::to_string(&rep).map_err(|e| e.to_string())
}

pub fn correlate_json(db_path: &str) -> Result<String, String> {
    let store = Store::open(db_path)?;
    let rep = tare_core::correlation::correlate_runs_dated(
        &cached_runs(db_path)?,
        &pricing()?,
        &store.run_days()?,
    );
    serde_json::to_string(&rep).map_err(|e| e.to_string())
}

/// Every configured prompt/config lineage projected onto cost-per-run. Reads
/// `[[lineage]]` from tare.toml; empty when none are declared.
pub fn lineages_json(db_path: &str) -> Result<String, String> {
    let cfg = tare_core::config::TareConfig::load(&config_path())?;
    let runs = cached_runs(db_path)?;
    let pricing = pricing()?;
    let reps: Vec<_> = cfg
        .lineage
        .iter()
        .map(|lineage| tare_core::lineage::lineage_report(&runs, &pricing, lineage))
        .collect();
    serde_json::to_string(&reps).map_err(|e| e.to_string())
}

/// Captured runs bucketed into the configured units of work, cost per unit. Reads
/// `[[unit]]` from tare.toml; the report is empty (just an `unbucketed` row) when none are declared.
pub fn units_json(db_path: &str) -> Result<String, String> {
    let cfg = tare_core::config::TareConfig::load(&config_path())?;
    let runs = cached_runs(db_path)?;
    let rep = tare_core::workunit::unit_report(&runs, &pricing()?, &cfg.unit);
    serde_json::to_string(&rep).map_err(|e| e.to_string())
}

pub fn loops_json(db_path: &str) -> Result<String, String> {
    let rep = tare_core::session::loop_waste(&cached_runs(db_path)?, &pricing()?);
    serde_json::to_string(&rep).map_err(|e| e.to_string())
}

pub fn failures_json(db_path: &str) -> Result<String, String> {
    let rep = tare_core::session::failure_waste(&cached_runs(db_path)?, &pricing()?);
    serde_json::to_string(&rep).map_err(|e| e.to_string())
}

pub fn lenses_json(db_path: &str) -> Result<String, String> {
    let rep = tare_core::lenses::lenses(&cached_runs(db_path)?, &pricing()?);
    serde_json::to_string(&rep).map_err(|e| e.to_string())
}

pub fn sandwich_json(db_path: &str, component: &str) -> Result<String, String> {
    let rep = tare_core::lenses::component_sandwich(&cached_runs(db_path)?, &pricing()?, component);
    serde_json::to_string(&rep).map_err(|e| e.to_string())
}

pub fn budget_json(db_path: &str) -> Result<String, String> {
    // Mirror the CLI: read [budget] period/cap/warn from tare.toml, sum the period's trend.
    let cfg = tare_core::config::TareConfig::load(&config_path())?;
    let period = cfg.budget.period.clone().unwrap_or_else(|| "month".into());
    let cap = cfg
        .budget
        .period_max_spend_usd
        .and_then(tare_core::config::dollars_to_micros)
        .unwrap_or(0);
    let warn = cfg.budget.warn_pct.unwrap_or(80);
    let start = period_start(&period, &crate::today_local()?)?;
    let trend = trend_view(db_path, None, None, "total")?;
    let spent: i64 = trend
        .series
        .first()
        .map(|s| {
            trend
                .days
                .iter()
                .zip(s.per_day.iter())
                .filter(|(d, _)| d.as_str() >= start.as_str())
                .map(|(_, v)| *v)
                .fold(0i64, i64::saturating_add)
        })
        .unwrap_or(0);
    let rep = tare_core::budget::period_status(&period, spent, cap, warn);
    serde_json::to_string(&rep).map_err(|e| e.to_string())
}

/// Period start from a local `YYYY-MM-DD`: "week" backs up to Sunday, "month" → the 1st.
fn period_start(period: &str, today: &str) -> Result<String, String> {
    let days = tare_core::calendar::parse_date(today)
        .ok_or_else(|| format!("invalid local date {today:?}"))?;
    match period {
        "month" => Ok(format!("{}-01", &today[..7])),
        "week" => {
            // 1970-01-01 was a Thursday; Sunday is weekday zero.
            let weekday = (days + 4).rem_euclid(7);
            Ok(tare_core::calendar::format_date(days - weekday))
        }
        _ => Err(format!("invalid budget period {period:?}")),
    }
}

/// Out-of-band OTLP receiver status for the Connect screen (desktop parity with the HTTP read
/// API). The live last-event timestamp lives in the running `tare serve` process, so the desktop
/// derives the captured-event count from the store (always accurate) and reports the receiver
/// port; `age_seconds` is null on this transport.
pub fn otlp_status_json(db_path: &str, port: u16) -> Result<String, String> {
    let store = Store::open(db_path)?;
    let events: u64 = store
        .source_counts()?
        .iter()
        .filter(|(k, _)| k.starts_with("otel"))
        .map(|(_, v)| *v)
        .fold(0u64, u64::saturating_add);
    // DB-derived fallback: reached only when no live serve answered fetch_otlp_status, so there is no
    // reachable receiver — report listening=false honestly, alongside the historical DB event count.
    // A live receiver's real flag comes from the serve read route, not here.
    Ok(serde_json::json!({
        "listening": false,
        "port": port,
        "events": events,
        "last_event_unix": 0,
        "age_seconds": serde_json::Value::Null,
    })
    .to_string())
}

/// Extract the body of an HTTP/1.0 response, but only on a `200` status line.
fn http_body(resp: &str) -> Option<&str> {
    let (head, body) = resp.split_once("\r\n\r\n")?;
    (head.lines().next()?.split_whitespace().nth(1)? == "200").then_some(body)
}

/// Best-effort, dependency-free loopback HTTP GET (http only, short timeout). Returns the response
/// body on 200, else None. Used to read a running `tare serve`'s live status over loopback.
fn http_get_loopback(port: u16, path: &str) -> Option<String> {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&address, Duration::from_millis(500)).ok()?;
    let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = s.set_write_timeout(Some(Duration::from_millis(500)));
    write!(
        s,
        "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
    let mut bytes = Vec::new();
    s.take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return None;
    }
    let buf = String::from_utf8(bytes).ok()?;
    http_body(&buf).map(str::to_string)
}

/// Live OTLP receiver status from a running `tare serve` on `http_port` (carries the last-event
/// timestamp). Returns None if no serve is reachable or the body isn't the expected JSON.
pub fn fetch_otlp_status(http_port: u16) -> Option<String> {
    let body = http_get_loopback(http_port, "/__tare/otlp_status")?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    let valid = v
        .get("listening")
        .and_then(serde_json::Value::as_bool)
        .is_some()
        && v.get("port")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|port| u16::try_from(port).is_ok() && port > 0)
        && v.get("events")
            .and_then(serde_json::Value::as_u64)
            .is_some()
        && v.get("last_event_unix")
            .and_then(serde_json::Value::as_u64)
            .is_some()
        && v.get("age_seconds")
            .is_some_and(|age| age.is_null() || age.as_u64().is_some());
    valid.then_some(body)
}

/// Live running sessions from a serve's in-memory activity table. Desktop reads the freshest state
/// over loopback HTTP; returns `[]` when no serve is reachable.
pub fn fetch_sessions_live(http_port: u16) -> Option<String> {
    let body = http_get_loopback(http_port, "/__tare/sessions_live")?;
    serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .filter(|v| v.is_array())
        .map(|_| body)
}

/// Claude Code's OWN reported cost/tokens for a day (vendor metrics), as a JSON cross-check — read
/// from the durable metered series. Mirrors the `/__tare/vendor_today` HTTP shape.
pub fn vendor_today_json(db_path: &str, day: &str) -> Result<String, String> {
    let store = Store::open(db_path)?;
    let (cost_micros, tokens) = store.metered_totals(day)?;
    serde_json::to_string(&serde_json::json!({
        "cost_micros": cost_micros,
        "tokens": tokens,
        "day": day,
        "available": cost_micros > 0 || tokens > 0,
    }))
    .map_err(|e| e.to_string())
}

/// Capture-coverage / blind-spot report. Mirrors the CLI's coverage_for over the store:
/// which sources feed cost steps vs only emit heartbeats. Clock-free.
pub fn coverage_json(db_path: &str) -> Result<String, String> {
    let store = Store::open(db_path)?;
    let by_source = store.coverage_by_source()?;
    let heartbeats: std::collections::BTreeSet<String> = store
        .load_session_activity()?
        .into_iter()
        .map(|(source, _, _, _, _)| source)
        .collect();
    let total_steps = by_source
        .iter()
        .map(|(_, count, _)| *count)
        .fold(0u64, u64::saturating_add);
    let feeding: std::collections::BTreeSet<&str> = by_source
        .iter()
        .filter(|(_, n, _)| *n > 0)
        .map(|(s, _, _)| s.as_str())
        .collect();
    let has_proxy = feeding.contains("proxy");
    let has_otel = feeding.iter().any(|s| s.starts_with("otel"));
    let blind: Vec<String> = heartbeats
        .iter()
        .filter(|s| !feeding.contains(s.as_str()))
        .cloned()
        .collect();
    let status = if total_steps == 0 {
        if heartbeats.is_empty() {
            "none"
        } else {
            "red"
        }
    } else if has_proxy && has_otel {
        "green"
    } else {
        "amber"
    };
    let mut rows: Vec<serde_json::Value> = by_source
        .iter()
        .map(|(s, n, last)| {
            serde_json::json!({ "source": s, "steps": n, "last_day": last, "heartbeat": heartbeats.contains(s) })
        })
        .collect();
    for s in &blind {
        rows.push(
            serde_json::json!({ "source": s, "steps": 0, "last_day": "", "heartbeat": true }),
        );
    }
    serde_json::to_string(&serde_json::json!({
        "status": status,
        "has_proxy": has_proxy,
        "has_otel": has_otel,
        "blind_sources": blind,
        "sources": rows,
    }))
    .map_err(|e| e.to_string())
}

/// Clock-free burn-rate / projected period spend for the Live lens. Mirrors the CLI's
/// burnrate_for over core+store: reuse the period-to-date daily series and ask the core engine to
/// project "at your captured pace". Estimate, not a real-time forecast.
pub fn burnrate_json(db_path: &str) -> Result<String, String> {
    burnrate_json_for_range(db_path, None)
}

pub fn burnrate_json_for_range(
    db_path: &str,
    requested_range: Option<&str>,
) -> Result<String, String> {
    let cfg = tare_core::config::TareConfig::load(&config_path())?;
    let configured_period = cfg.budget.period.clone().unwrap_or_else(|| "month".into());
    let configured_cap = cfg
        .budget
        .period_max_spend_usd
        .and_then(tare_core::config::dollars_to_micros)
        .unwrap_or(0);
    let today = crate::today_local()?;
    let window = tare_core::burnrate::projection_window(
        requested_range.unwrap_or(&configured_period),
        &today,
    )?;
    let period = window.range.as_str();
    let start = window.start;
    let period_end = window.end;
    let days_in_period = window.days_in_period;
    let cap = if period == configured_period {
        configured_cap
    } else {
        0
    };
    // Use the full dense budget period rather than `trend_view`'s general-purpose 14-day default.
    // This preserves leading/trailing idle days and makes the returned observations exactly match
    // the history used by the projection.
    let trend =
        Store::open(db_path)?.trend_in_range(&start, &today, &pricing()?, TrendDimension::Total)?;
    let per_day: Vec<i64> = trend
        .series
        .first()
        .map(|s| s.per_day.clone())
        .unwrap_or_else(|| vec![0; trend.days.len()]);
    let br = tare_core::burnrate::project(&per_day, days_in_period, cap);
    let mut v = serde_json::to_value(&br).map_err(|e| e.to_string())?;
    if let Some(obj) = v.as_object_mut() {
        obj.insert("period".into(), serde_json::json!(period));
        obj.insert("period_start".into(), serde_json::json!(start));
        obj.insert("as_of".into(), serde_json::json!(today));
        obj.insert("period_end".into(), serde_json::json!(period_end));
    }
    serde_json::to_string(&v).map_err(|e| e.to_string())
}

/// Recorded provenance for a run: the runs row + distinct step dimensions + pricing
/// version/date. Makes the anonymous flamegraph self-describing. Errors if the run is unknown.
pub fn run_meta_json(db_path: &str, run_id: &str) -> Result<String, String> {
    let store = Store::open(db_path)?;
    let meta = store
        .run_meta(run_id)?
        .ok_or_else(|| format!("unknown run {run_id}"))?;
    let p = pricing()?;
    let mut v = serde_json::to_value(&meta).map_err(|e| e.to_string())?;
    if let Some(obj) = v.as_object_mut() {
        obj.insert("pricing_version".into(), serde_json::json!(p.version));
        obj.insert("effective_date".into(), serde_json::json!(p.effective_date));
    }
    serde_json::to_string(&v).map_err(|e| e.to_string())
}

/// Per-model estimate-vs-vendor reconciliation for a day. Estimate stays primary; the
/// vendor figure is a cross-check, never merged. Mirrors `tare_cli::reconcile_for` (the adapter
/// reimplements over core+store rather than depending on the CLI crate).
pub fn reconcile_json(db_path: &str, day: &str) -> Result<String, String> {
    use std::collections::BTreeMap;
    let store = Store::open(db_path)?;
    let p = pricing()?;
    let est = tare_core::rollup::rollup(
        &store.load_runs_on_date(day)?,
        &p,
        tare_core::rollup::RollupDim::Model,
    );
    let mut est_by_model: BTreeMap<String, (i64, u64)> = BTreeMap::new();
    for r in &est.rows {
        est_by_model.insert(r.label.clone(), (r.micros, r.tokens));
    }
    let vendor_by_model: BTreeMap<String, i64> = store.metered_by_model(day)?.into_iter().collect();

    let mut models: Vec<String> = est_by_model
        .keys()
        .chain(vendor_by_model.keys())
        .cloned()
        .collect();
    models.sort();
    models.dedup();

    let tolerance = |a: i64, b: i64| -> i64 { (a.max(b).max(0) / 20).max(1_000) };
    let mut rows = Vec::new();
    let mut est_total = 0i64;
    let mut vendor_total = 0i64;
    for m in &models {
        let (est_micros, tokens) = est_by_model.get(m).copied().unwrap_or((0, 0));
        let has_est = est_by_model.contains_key(m);
        let vendor_micros = vendor_by_model.get(m).copied().unwrap_or(0);
        let has_vendor = vendor_by_model.contains_key(m);
        est_total = est_total.saturating_add(est_micros);
        vendor_total = vendor_total.saturating_add(vendor_micros);
        let delta = est_micros.saturating_sub(vendor_micros);
        let cause = if has_est && est_micros == 0 && tokens > 0 {
            "unpriced"
        } else if has_vendor && !has_est {
            "coverage_gap"
        } else if has_est && !has_vendor {
            "estimate_only"
        } else if delta.saturating_abs() <= tolerance(est_micros, vendor_micros) {
            "ok"
        } else {
            "mismatch"
        };
        rows.push(serde_json::json!({
            "model": m,
            "estimate_micros": est_micros,
            "vendor_micros": vendor_micros,
            "delta_micros": delta,
            "tokens": tokens,
            "cause": cause,
        }));
    }
    serde_json::to_string(&serde_json::json!({
        "day": day,
        "pricing_version": est.pricing_version,
        "rows": rows,
        "estimate_total_micros": est_total,
        "vendor_total_micros": vendor_total,
        "delta_total_micros": est_total.saturating_sub(vendor_total),
        "has_vendor": vendor_total > 0,
    }))
    .map_err(|e| e.to_string())
}

/// Prefer a running serve's vendor cross-check over loopback HTTP; `None` if unreachable.
pub fn fetch_vendor_today(http_port: u16) -> Option<String> {
    let body = http_get_loopback(http_port, "/__tare/vendor_today")?;
    serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .filter(|v| v.get("cost_micros").is_some())
        .map(|_| body)
}

/// One run's export content (speedscope/otel/receipt) as a string for the GUI export buttons.
pub fn export_view(db_path: &str, run_id: &str, format: &str) -> Result<String, String> {
    let store = Store::open(db_path)?;
    let run = store
        .load_run(run_id)?
        .ok_or_else(|| format!("run `{run_id}` not found"))?;
    let v = env!("CARGO_PKG_VERSION");
    let pricing = pricing()?;
    match format {
        "speedscope" => {
            Ok(tare_core::speedscope::export(&build_flamegraph(&run, &pricing), v).to_string())
        }
        "otel" => Ok(tare_core::otel::export_otlp_json(&run, &pricing, v).to_string()),
        "receipt" => {
            let runs = [run];
            let report = attribute::build_report(&runs, &pricing);
            let confidence = confidence_over(&runs, &pricing, &today_local()?);
            let receipt = tare_core::receipt::attest(
                &runs,
                &report,
                &pricing,
                v,
                &format!("run:{run_id}"),
                true,
                confidence,
            );
            serde_json::to_string(&receipt).map_err(|e| e.to_string())
        }
        other => Err(format!("unknown export format {other:?}")),
    }
}

/// Estimate-confidence over a set of runs for the receipt stamp. Mirrors the CLI's
/// confidence_over: pricing age vs `today`, unpriced token share, and unknown capture coverage.
fn confidence_over(
    runs: &[tare_core::model::RunRecord],
    p: &PricingTable,
    today: &str,
) -> tare_core::confidence::EstimateConfidence {
    let report = attribute::build_report(runs, p);
    let total_tokens = tare_core::lenses::lenses(runs, p).total_tokens;
    let unpriced_tokens = report
        .unpriced
        .iter()
        .fold(0u64, |total, row| total.saturating_add(row.token_total));
    let unpriced_share = unpriced_tokens
        .saturating_mul(100)
        .checked_div(total_tokens)
        .unwrap_or(0) as i64;
    let age = tare_core::calendar::parse_date(&p.effective_date)
        .zip(tare_core::calendar::parse_date(today))
        .map(|(eff, now)| now - eff)
        .unwrap_or(0);
    // Coverage is honestly `unknown` out-of-band — mirrors tare-cli `confidence_over`.
    tare_core::confidence::confidence(
        age,
        unpriced_share,
        tare_core::confidence::CoverageStatus::Unknown,
        None,
    )
}

/// Pricing-table provenance (clock-free: version / effective date / note).
pub fn pricing_json() -> Result<String, String> {
    let p = pricing()?;
    serde_json::to_string(&serde_json::json!({
        "version": p.version,
        "effective_date": p.effective_date,
        "note": p.note,
        "models_by_provider": models_by_provider(&p),
        "models": deduped_models(&p),
    }))
    .map_err(|e| e.to_string())
}

/// The bundled per-model rate rows, deduped by (provider, model) — for the Models/Pricing
/// catalog screen. Reads bundled, never-network data.
pub fn deduped_models(p: &PricingTable) -> Vec<&tare_core::pricing::ModelRates> {
    let mut seen = std::collections::BTreeSet::new();
    p.models
        .iter()
        .filter(|m| seen.insert((m.provider.clone(), m.model_id.clone())))
        .collect()
}

/// `provider -> number of priced models` from the bundled table, so setup can say
/// "N models for <provider>; others show usage-only".
pub fn models_by_provider(p: &PricingTable) -> std::collections::BTreeMap<String, u32> {
    let mut counts: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    for m in &p.models {
        let count = counts.entry(m.provider.clone()).or_insert(0);
        *count = count.saturating_add(1);
    }
    counts
}

/// Attest a run into a receipt and verify it OFFLINE; returns `{receipt, verify}`.
pub fn receipt_json(db_path: &str, run_id: &str, max_private: bool) -> Result<String, String> {
    let store = Store::open(db_path)?;
    let run = store
        .load_run(run_id)?
        .ok_or_else(|| format!("run `{run_id}` not found"))?;
    let policy = if max_private {
        tare_core::PrivacyPolicy::max_private()
    } else {
        tare_core::PrivacyPolicy::default()
    };
    let runs = [run];
    let p = pricing()?;
    let report = attribute::build_report(&runs, &p).with_privacy(&policy);
    let with_flamegraph = !max_private;
    let confidence = confidence_over(&runs, &p, &today_local()?);
    let receipt = tare_core::receipt::attest(
        &runs,
        &report,
        &p,
        env!("CARGO_PKG_VERSION"),
        &format!("run:{run_id}"),
        with_flamegraph,
        confidence,
    );
    let json = serde_json::to_string(&receipt).map_err(|e| e.to_string())?;
    let verify = tare_core::receipt::verify(&json, &p)?;
    let receipt_val: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    serde_json::to_string(&serde_json::json!({ "receipt": receipt_val, "verify": verify }))
        .map_err(|e| e.to_string())
}

/// The current GUI-editable config (`tare.toml`) as JSON, or defaults if absent.
pub fn config_get_json_at(path: &str) -> Result<String, String> {
    let cfg = tare_core::config::TareConfig::load(path)?;
    serde_json::to_string(&cfg).map_err(|e| e.to_string())
}

/// Per-field config provenance: for each editable field, `env` (a TARE_* var overrides
/// it — the form would lie), `tare.toml` (set in the file), or `default`. `env_get` is injected so
/// this is a pure, testable function; the command wrapper passes `std::env::var`.
pub fn config_origins_at(
    path: &str,
    env_get: impl Fn(&str) -> Option<String>,
) -> Result<String, String> {
    let cfg = tare_core::config::TareConfig::load(path)?;
    // (web field path, env var, present-in-file?)
    let fields: [(&str, &str, bool); 12] = [
        (
            "budget.max_spend_usd",
            "TARE_MAX_SPEND_USD",
            cfg.budget.max_spend_usd.is_some(),
        ),
        (
            "budget.max_steps",
            "TARE_MAX_STEPS",
            cfg.budget.max_steps.is_some(),
        ),
        (
            "budget.max_repeats",
            "TARE_MAX_REPEATS",
            cfg.budget.max_repeats.is_some(),
        ),
        (
            "privacy.profile",
            "TARE_PRIVACY_PROFILE",
            cfg.privacy.profile.is_some(),
        ),
        (
            "privacy.salt",
            "TARE_PRIVACY_SALT",
            cfg.privacy.salt.is_some(),
        ),
        (
            "providers.anthropic_upstream",
            "TARE_ANTHROPIC_UPSTREAM",
            cfg.providers.anthropic_upstream.is_some(),
        ),
        (
            "providers.openai_upstream",
            "TARE_OPENAI_UPSTREAM",
            cfg.providers.openai_upstream.is_some(),
        ),
        (
            "providers.gemini_upstream",
            "TARE_GEMINI_UPSTREAM",
            cfg.providers.gemini_upstream.is_some(),
        ),
        (
            "providers.azure_openai_upstream",
            "TARE_AZURE_OPENAI_UPSTREAM",
            cfg.providers.azure_openai_upstream.is_some(),
        ),
        (
            "providers.bedrock_upstream",
            "TARE_BEDROCK_UPSTREAM",
            cfg.providers.bedrock_upstream.is_some(),
        ),
        ("proxy.db", "TARE_DB", cfg.proxy.db.is_some()),
        (
            "ui.tz_offset_minutes",
            "TARE_TZ_OFFSET_MINUTES",
            cfg.ui.tz_offset_minutes.is_some(),
        ),
    ];
    let mut map = serde_json::Map::new();
    for (field, env, in_file) in fields {
        let origin = if env_get(env).is_some_and(|v| !v.is_empty()) {
            "env"
        } else if in_file {
            "tare.toml"
        } else {
            "default"
        };
        map.insert(
            field.to_string(),
            serde_json::Value::String(origin.to_string()),
        );
    }
    serde_json::to_string(&map).map_err(|e| e.to_string())
}

pub fn config_origins_json(path: &str) -> Result<String, String> {
    config_origins_at(path, |k| std::env::var(k).ok())
}

/// Write a config JSON (from the Settings UI) back to `tare.toml`. Validates by parsing into the
/// typed `TareConfig` first, so a malformed payload can never land on disk.
pub fn config_save_json_at(path: &str, json: &str) -> Result<String, String> {
    ensure_ipc_body_size(json.as_bytes())?;
    // MERGE over what is already on disk instead of deserializing the payload straight into
    // TareConfig. Every field is `#[serde(default)]` and this is a whole-file
    // rewrite, so a payload that models only the sections the UI edits used to silently delete
    // `[[unit]]` and `[[lineage]]`. `merge_json` also validates, so a malformed payload still
    // cannot land on disk.
    // A present malformed file is user data, not an absent file. Refuse to replace it with defaults;
    // the user can fix it without losing sections the app could not parse.
    let on_disk = tare_core::config::TareConfig::load(path)?;
    let cfg = on_disk.merge_json(json)?;
    cfg.save(path)?;
    Ok("{\"ok\":true}".to_string())
}

fn changed_config_fields(
    before: &serde_json::Value,
    after: &serde_json::Value,
    prefix: &str,
    out: &mut Vec<String>,
) {
    if before == after {
        return;
    }
    match (before.as_object(), after.as_object()) {
        (Some(a), Some(b)) => {
            let keys: std::collections::BTreeSet<&str> =
                a.keys().chain(b.keys()).map(String::as_str).collect();
            for key in keys {
                let path = if prefix.is_empty() {
                    key.to_string()
                } else {
                    format!("{prefix}.{key}")
                };
                changed_config_fields(
                    a.get(key).unwrap_or(&serde_json::Value::Null),
                    b.get(key).unwrap_or(&serde_json::Value::Null),
                    &path,
                    out,
                );
            }
        }
        // Arrays are one declared config field. Never persist their user-entered contents as a
        // pseudo-path; `unit`/`alert`/`lineage` is sufficient event metadata.
        _ if !prefix.is_empty() => out.push(prefix.to_string()),
        _ => {}
    }
}

/// Settings-save writer that also records an authoritative local timeline event. The TOML write
/// happens first; only a successful, materially changed save gets an event. The event contains
/// sorted field names only — never either JSON/TOML value.
pub fn config_save_json_at_with_event(
    path: &str,
    db_path: &str,
    json: &str,
    occurred_at: &str,
) -> Result<String, String> {
    ensure_ipc_body_size(json.as_bytes())?;
    let before = tare_core::config::TareConfig::load(path)?;
    let after = before.merge_json(json)?;
    let before_value = serde_json::to_value(&before).map_err(|e| e.to_string())?;
    let after_value = serde_json::to_value(&after).map_err(|e| e.to_string())?;
    let mut changed_fields = Vec::new();
    changed_config_fields(&before_value, &after_value, "", &mut changed_fields);
    changed_fields.sort();
    changed_fields.dedup();

    // Establish/migrate the local event store before changing the config file. Cross-file atomicity
    // is impossible, but this prevents a bad/unopenable DB path from producing a successful TOML
    // write with no authoritative event.
    let event_store = if changed_fields.is_empty() {
        None
    } else {
        Some(Store::open(db_path)?)
    };
    after.save(path)?;
    if let Some(store) = event_store {
        store.record_config_change_event(occurred_at, "settings", &changed_fields)?;
    }
    Ok("{\"ok\":true}".to_string())
}

/// Acknowledge an anomaly: append its key to `[anomaly].acknowledged` in tare.toml.
/// Path-parameterized twin for tests; the command uses the default path.
pub fn acknowledge_anomaly_at(path: &str, key: &str) -> Result<(), String> {
    tare_core::config::TareConfig::acknowledge_anomaly(path, key)
}
pub fn acknowledge_anomaly(key: &str) -> Result<(), String> {
    acknowledge_anomaly_at(&config_path(), key)
}

pub fn config_get_json() -> Result<String, String> {
    config_get_json_at(&config_path())
}

/// Tray title string, e.g. `Tare · today $0.08` (2dp money for a glanceable tray).
pub fn tray_title(db_path: &str, date: &str) -> String {
    match today_spend_view(db_path, date) {
        Ok(t) => format!(
            "Tare · today {}",
            MicroUsd(t.total_micros).to_dollar_string_2dp()
        ),
        Err(_) => "Tare · today unavailable".to_string(),
    }
}

/// A glanceable tray status: the today-spend title plus two disabled readout rows
/// (budget + capture) and whether anything needs attention — which prepends a ⚠ glyph to the
/// title. Pure over its inputs, so the menu-bar extra's logic is unit-tested without Tauri.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayStatus {
    pub title: String,
    pub budget_row: String,
    pub capture_row: String,
    pub attention: bool,
}

/// Build the tray status from already-resolved inputs. `attention` fires when spend has crossed the
/// budget warn threshold (status != "ok" with a cap set) or capture is stale (receiver listening
/// but no live session).
pub fn tray_status_view(
    today_micros: i64,
    budget: &tare_core::budget::PeriodBudget,
    live_sessions: usize,
    otlp_listening: bool,
) -> TrayStatus {
    let attention_budget = budget.cap_micros > 0 && budget.status != "ok";
    let capture_stale = otlp_listening && live_sessions == 0;
    let attention = attention_budget || capture_stale;
    let title = format!(
        "{}Tare · today {}",
        if attention { "⚠ " } else { "" },
        MicroUsd(today_micros).to_dollar_string_2dp() // 2dp money in the tray —
    );
    let budget_row = if budget.cap_micros > 0 {
        format!(
            "Budget: {}% of {} {}",
            budget.pct,
            MicroUsd(budget.cap_micros).to_dollar_string_2dp(),
            budget.period
        )
    } else {
        "Budget: no cap set".to_string()
    };
    let capture_row = if !otlp_listening {
        "Capture: no receiver".to_string()
    } else if live_sessions > 0 {
        format!(
            "Capture: live ({} session{})",
            live_sessions,
            if live_sessions == 1 { "" } else { "s" }
        )
    } else {
        "Capture: stale".to_string()
    };
    TrayStatus {
        title,
        budget_row,
        capture_row,
        attention,
    }
}

/// Assemble the tray status from the store + config, reusing the same adapters the Connect/Live
/// screens use (budget, sessions, OTLP receiver). Clock-touching (session recency) lives here at
/// the edge, never in core.
pub fn tray_status(
    db_path: &str,
    date: &str,
    http_port: u16,
    otlp_port: u16,
) -> Result<TrayStatus, String> {
    let today_micros = today_spend_view(db_path, date)?.total_micros;
    let budget = serde_json::from_str::<tare_core::budget::PeriodBudget>(&budget_json(db_path)?)
        .map_err(|error| format!("decode budget status: {error}"))?;
    // Prefer the running server's in-memory views. The durable adapters remain an honest fallback
    // when no server is reachable.
    let live_receiver = fetch_otlp_status(http_port);
    let sessions_json = if live_receiver.is_some() {
        match fetch_sessions_live(http_port) {
            Some(sessions) => sessions,
            None => sessions_live_json(db_path)?,
        }
    } else {
        sessions_live_json(db_path)?
    };
    let sessions = serde_json::from_str::<Vec<serde_json::Value>>(&sessions_json)
        .map_err(|error| format!("decode live sessions: {error}"))?;
    let live_sessions = sessions
        .iter()
        .filter(|row| row.get("state").and_then(|state| state.as_str()) != Some("ended"))
        .count();
    let receiver_json = match live_receiver {
        Some(status) => status,
        None => otlp_status_json(db_path, otlp_port)?,
    };
    let otlp = serde_json::from_str::<serde_json::Value>(&receiver_json)
        .map_err(|error| format!("decode receiver status: {error}"))?;
    let otlp_listening = otlp
        .get("listening")
        .and_then(serde_json::Value::as_bool)
        .ok_or("receiver status is missing its listening flag")?;
    Ok(tray_status_view(
        today_micros,
        &budget,
        live_sessions,
        otlp_listening,
    ))
}

#[cfg(feature = "gui")]
pub mod gui;

#[cfg(test)]
mod tests {
    use super::*;
    use tare_core::ingest_step;
    use tare_core::model::Provider;

    #[test]
    fn settings_save_records_only_sorted_changed_field_names() {
        let root = std::env::temp_dir().join(format!(
            "tare-config-event-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let cfg = root.with_extension("toml").to_string_lossy().to_string();
        let db = root.with_extension("db").to_string_lossy().to_string();
        let _ = std::fs::remove_file(&cfg);
        let _ = std::fs::remove_file(&db);
        config_save_json_at_with_event(
            &cfg,
            &db,
            r#"{"budget":{},"privacy":{"profile":"max_private","salt":"never-store-this"},"providers":{"anthropic_upstream":"https://private.example"},"proxy":{},"capture":{"mode":"off"}}"#,
            "2026-07-11T06:30:00Z",
        )
        .unwrap();
        // Saving the identical typed config does not fabricate a second change event.
        let same = config_get_json_at(&cfg).unwrap();
        config_save_json_at_with_event(&cfg, &db, &same, "2026-07-11T06:31:00Z").unwrap();

        let store = Store::open(&db).unwrap();
        let events = store
            .config_change_events("2026-07-10", "2026-07-10", "America/Los_Angeles")
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].changed_fields,
            vec![
                "capture.mode".to_string(),
                "privacy.profile".to_string(),
                "privacy.salt".to_string(),
                "providers.anthropic_upstream".to_string(),
            ]
        );
        let raw = std::fs::read(&db).unwrap();
        let raw = String::from_utf8_lossy(&raw);
        assert!(!raw.contains("never-store-this"));
        assert!(!raw.contains("private.example"));
        let _ = std::fs::remove_file(&cfg);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn settings_save_never_replaces_a_malformed_existing_config() {
        let path = std::env::temp_dir().join(format!(
            "tare-invalid-config-{}-{}.toml",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let path_string = path.to_string_lossy();
        let malformed = "[budget\nmax_spend_usd = 5\n";
        std::fs::write(&path, malformed).unwrap();

        assert!(config_save_json_at(&path_string, r#"{"budget":{"max_spend_usd":1}}"#).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), malformed);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn pricing_rejects_malformed_config_and_configured_rate_files() {
        let root = std::env::temp_dir().join(format!(
            "tare-pricing-errors-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let config = root.with_extension("toml");
        let rates = root.with_extension("json");
        let _ = std::fs::remove_file(&config);
        let _ = std::fs::remove_file(&rates);

        std::fs::write(&config, "[proxy\npricing = 'missing.json'\n").unwrap();
        assert!(pricing_at(config.to_str().unwrap()).is_err());

        let quoted_rates = serde_json::to_string(rates.to_str().unwrap()).unwrap();
        std::fs::write(&config, format!("[proxy]\npricing = {quoted_rates}\n")).unwrap();
        std::fs::write(&rates, "{not valid pricing json").unwrap();
        assert!(pricing_at(config.to_str().unwrap()).is_err());

        std::fs::write(&rates, SHIPPED_PRICING).unwrap();
        assert!(!pricing_at(config.to_str().unwrap())
            .unwrap()
            .models
            .is_empty());
        // A cached table must not hide a later rate-file failure.
        std::fs::write(&rates, "[]").unwrap();
        assert!(pricing_at(config.to_str().unwrap()).is_err());

        let _ = std::fs::remove_file(config);
        let _ = std::fs::remove_file(rates);
    }

    #[test]
    fn settings_event_uses_the_merged_config_and_preserves_omitted_sections() {
        let root = std::env::temp_dir().join(format!(
            "tare-config-merge-event-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let config = root.with_extension("toml");
        let db = root.with_extension("db");
        let db_string = db.to_string_lossy();
        let _ = std::fs::remove_file(&config);
        let _ = std::fs::remove_file(&db);
        std::fs::write(
            &config,
            "[budget]\nwarn_pct = 80\n[proxy]\ndb = '/tmp/preserved-tare.db'\n",
        )
        .unwrap();

        config_save_json_at_with_event(
            config.to_str().unwrap(),
            &db_string,
            r#"{"budget":{"warn_pct":90}}"#,
            "2026-07-11T06:30:00Z",
        )
        .unwrap();

        let saved = tare_core::config::TareConfig::load(config.to_str().unwrap()).unwrap();
        assert_eq!(saved.budget.warn_pct, Some(90));
        assert_eq!(saved.proxy.db.as_deref(), Some("/tmp/preserved-tare.db"));
        let events = Store::open(&db_string)
            .unwrap()
            .config_change_events("2026-07-11", "2026-07-11", "UTC")
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].changed_fields, vec!["budget.warn_pct"]);

        let _ = std::fs::remove_file(config);
        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(format!("{db_string}-wal"));
        let _ = std::fs::remove_file(format!("{db_string}-shm"));
    }

    #[test]
    fn desktop_inputs_reject_invalid_dimensions_limits_and_oversized_json() {
        let db = std::env::temp_dir()
            .join(format!(
                "tare-tauri-validation-{}-{}.db",
                std::process::id(),
                std::thread::current().name().unwrap_or("test")
            ))
            .to_string_lossy()
            .into_owned();
        let _ = std::fs::remove_file(&db);
        drop(Store::open(&db).unwrap());

        assert!(trend_view(&db, None, None, "unknown").is_err());
        assert!(run_profile_view(&db, "missing", Some("unknown".into()), None).is_err());
        assert!(run_profile_view(&db, "missing", None, Some(0)).is_err());
        assert!(recent_steps_json(&db, 0).is_err());
        assert!(recent_steps_json(&db, 51).is_err());
        assert!(anomalies_json(&db, "total", 0, 50).is_err());
        assert!(anomalies_json(&db, "total", 7, -1).is_err());
        assert!(cost_regressions_json(&db, 0, 50).is_err());
        assert!(cost_regressions_json(&db, 7, -1).is_err());
        assert!(rollup_json(&db, "unknown", None, None).is_err());
        assert!(rollup_json(&db, "step", Some("model"), None).is_err());
        assert!(rollup_json(&db, "step", Some("model"), Some("")).is_err());

        let oversized = vec![b'x'; IPC_BODY_CAP + 1];
        assert!(ensure_ipc_body_size(&oversized).is_err());
        assert!(anomaly_why_json(&db, &oversized).is_err());
        assert!(save_run_note(&db, std::str::from_utf8(&oversized).unwrap()).is_err());
        let config = format!("{db}.toml");
        assert!(config_save_json_at(&config, std::str::from_utf8(&oversized).unwrap()).is_err());
        assert!(!std::path::Path::new(&config).exists());

        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(format!("{db}-wal"));
        let _ = std::fs::remove_file(format!("{db}-shm"));
    }

    #[test]
    fn notes_and_quality_require_known_runs_and_strict_fields() {
        let db = std::env::temp_dir()
            .join(format!(
                "tare-tauri-notes-{}-{}.db",
                std::process::id(),
                std::thread::current().name().unwrap_or("test")
            ))
            .to_string_lossy()
            .into_owned();
        let _ = std::fs::remove_file(&db);
        drop(Store::open(&db).unwrap());

        assert!(set_run_quality(&db, "missing", 80, "ui").is_err());
        assert!(save_run_note(
            &db,
            r#"{"run_id":"missing","tags":[],"note_text":"x","starred":false}"#
        )
        .is_err());
        assert!(set_run_quality(&db, "missing", 80, "import").is_err());
        assert!(set_run_quality(&db, "missing", 101, "ui").is_err());
        assert!(save_run_note(
            &db,
            r#"{"run_id":"missing","tags":["same","same"],"note_text":"x","starred":false}"#
        )
        .is_err());
        assert!(save_run_note(
            &db,
            r#"{"run_id":"missing","tags":[],"note_text":3,"starred":false}"#
        )
        .is_err());
        assert!(save_run_note(
            &db,
            r#"{"run_id":"missing","tags":[],"note_text":"x","starred":"yes"}"#
        )
        .is_err());

        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(format!("{db}-wal"));
        let _ = std::fs::remove_file(format!("{db}-shm"));
    }

    #[test]
    fn native_views_project_v2_investigations_and_encode_opaque_ids() {
        let db = std::env::temp_dir()
            .join(format!("tare-inv-menu-{}.db", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&db);
        let store = Store::open(&db).unwrap();
        let row = serde_json::json!({
            "id": "#/trends?by=model & owner=mé",
            "label": "Model investigation",
            "version": 2,
            "state": {
                "workspace": "investigate",
                "scope": {},
                "selection": null,
                "baseline": null,
                "match": {"kind": "aggregate_only"},
                "comparison": [],
                "pinned": null
            },
            "created_at": "2026-07-15T00:00:00Z",
            "updated_at": "2026-07-15T00:00:00Z"
        });
        store.upsert_investigation(&row).unwrap();
        drop(store);

        let views = saved_investigations_for_menu(&db);
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].label, "Model investigation");
        assert_eq!(
            saved_investigation_hash(&views[0]),
            "#/investigate?investigation=%23%2Ftrends%3Fby%3Dmodel%20%26%20owner%3Dm%C3%A9"
        );

        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(format!("{db}-wal"));
        let _ = std::fs::remove_file(format!("{db}-shm"));
    }

    #[test]
    fn transcript_json_reads_the_sibling_store_and_purge_clears_it() {
        // the `transcript` + `transcript_purge` Tauri commands delegate to these
        // helpers. transcript_json returns null when nothing captured, the {req,resp} object once a
        // step is stored in the SEPARATE sibling DB, and purge empties it.
        let db = std::env::temp_dir()
            .join(format!("tare-tx-{}.db", std::process::id()))
            .to_string_lossy()
            .to_string();
        let tpath = transcript_db_path(&db);
        let _ = std::fs::remove_file(&tpath);
        // No store yet → honest null, not an error.
        assert_eq!(transcript_json(&db, "run-x", 1).unwrap(), "null");
        assert_eq!(transcript_purge_all(&db).unwrap(), 0);
        // Seed one redacted step into the sibling store.
        tare_store::TranscriptStore::open(&tpath)
            .unwrap()
            .insert("run-x", 2, "{\"req\":1}", "{\"resp\":2}", true)
            .unwrap();
        let got = transcript_json(&db, "run-x", 2).unwrap();
        assert!(got.contains("\"req\""), "captured req present: {got}");
        assert!(got.contains("\"resp\""), "captured resp present: {got}");
        assert!(
            got.contains("\"truncated\":true"),
            "capture cap evidence present: {got}"
        );
        // A different step is still uncaptured → null.
        assert_eq!(transcript_json(&db, "run-x", 9).unwrap(), "null");
        // Purge removes the row; a subsequent read is null again.
        assert_eq!(transcript_purge_all(&db).unwrap(), 1);
        assert_eq!(transcript_json(&db, "run-x", 2).unwrap(), "null");
        let _ = std::fs::remove_file(&tpath);
    }

    #[test]
    fn tray_status_view_readouts_and_attention() {
        use tare_core::budget::period_status;
        // Under warn, receiver live with 3 sessions → calm; rows read out cleanly.
        let ok = tray_status_view(
            2_500_000,
            &period_status("month", 25_000_000, 50_000_000, 80),
            3,
            true,
        );
        assert_eq!(ok.title, "Tare · today $2.50"); // 2dp money in the tray —
        assert_eq!(ok.budget_row, "Budget: 50% of $50.00 month");
        assert_eq!(ok.capture_row, "Capture: live (3 sessions)");
        assert!(!ok.attention);

        // Past the warn threshold → attention glyph on the title.
        let warn = tray_status_view(
            45_000_000,
            &period_status("month", 45_000_000, 50_000_000, 80),
            1,
            true,
        );
        assert!(warn.attention);
        assert!(warn.title.starts_with("⚠ "));
        assert_eq!(warn.capture_row, "Capture: live (1 session)");

        // Receiver listening but no live session → stale capture is also attention-worthy.
        let stale = tray_status_view(1_000_000, &period_status("month", 0, 0, 80), 0, true);
        assert!(stale.attention);
        assert_eq!(stale.capture_row, "Capture: stale");
        assert_eq!(stale.budget_row, "Budget: no cap set");

        // No receiver → not "stale" (nothing to be stale about); no cap → calm.
        let off = tray_status_view(0, &period_status("month", 0, 0, 80), 0, false);
        assert!(!off.attention);
        assert_eq!(off.capture_row, "Capture: no receiver");
    }

    #[test]
    fn config_origins_resolves_env_over_toml_over_default() {
        // env-set field → "env"; file-set → "tare.toml"; neither → "default".
        let dir = std::env::temp_dir().join(format!("tare-origins-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tare.toml");
        std::fs::write(
            &path,
            "[budget]\nmax_spend_usd = 0.5\n[privacy]\nprofile = \"strict_counts\"\n",
        )
        .unwrap();
        // TARE_PRIVACY_PROFILE is "set" via the injected env, overriding the file value.
        let env = |k: &str| (k == "TARE_PRIVACY_PROFILE").then(|| "max_private".to_string());
        let json = config_origins_at(path.to_str().unwrap(), env).unwrap();
        let m: std::collections::BTreeMap<String, String> = serde_json::from_str(&json).unwrap();
        assert_eq!(m["budget.max_spend_usd"], "tare.toml"); // in file, no env
        assert_eq!(m["privacy.profile"], "env"); // env overrides the file
        assert_eq!(m["proxy.db"], "default"); // neither
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn http_body_extracts_only_on_200() {
        assert_eq!(
            super::http_body(
                "HTTP/1.0 200 OK\r\ncontent-type: application/json\r\n\r\n{\"events\":3}"
            ),
            Some("{\"events\":3}")
        );
        assert_eq!(super::http_body("HTTP/1.0 404 Not Found\r\n\r\nnope"), None);
        assert_eq!(super::http_body("garbage"), None);
    }

    #[test]
    fn otlp_status_counts_otel_sourced_events_only() {
        let tmp = std::env::temp_dir().join(format!("tare-otlp-status-{}.db", std::process::id()));
        let db = tmp.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");
        {
            let s = Store::open(&db).unwrap();
            let e = ingest_step("otel-run", 1, Provider::Anthropic, req, resp).unwrap();
            let p = ingest_step("proxy-run", 1, Provider::Anthropic, req, resp).unwrap();
            s.record_step_with_policy(&e, "2026-06-24", None, None, Some("otel-event"))
                .unwrap();
            s.record_step_with_policy(&p, "2026-06-24", None, None, Some("proxy"))
                .unwrap();
        }
        let v: serde_json::Value =
            serde_json::from_str(&otlp_status_json(&db, 4318).unwrap()).unwrap();
        assert_eq!(v["events"], 1); // only the otel-event step counts; the proxy step doesn't
        assert_eq!(v["port"], 4318);
        // DB-derived fallback reports listening=false honestly (no live receiver reached).
        assert_eq!(v["listening"], false);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn tray_and_views_over_store() {
        let store = Store::open_in_memory().unwrap();
        // (in-memory db can't be reopened by path; exercise the core view via a temp file)
        drop(store);

        let tmp = std::env::temp_dir().join(format!("tare-tauri-{}.db", std::process::id()));
        let db = tmp.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);

        let s = Store::open(&db).unwrap();
        let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");
        let step = ingest_step("r1", 1, Provider::Anthropic, req, resp).unwrap();
        s.record_step(&step, "2026-06-24").unwrap();
        drop(s);

        let title = tray_title(&db, "2026-06-24");
        assert!(title.starts_with("Tare · today $"));
        assert_ne!(title, "Tare · today $0.000000");

        let runs = list_run_ids(&db).unwrap();
        assert_eq!(runs, vec!["r1".to_string()]);

        let model = run_flamegraph_view(&db, "r1").unwrap();
        assert_eq!(model.run_id, "r1");

        // Profile view: flat self-sum equals the run total; top_n truncates.
        let prof = run_profile_view(&db, "r1", Some("flat".into()), None).unwrap();
        assert_eq!(prof.sort, tare_core::flamegraph::ProfileSort::Flat);
        let self_sum: i64 = prof.rows.iter().map(|r| r.self_micros).sum();
        assert_eq!(self_sum, prof.total_micros);
        let capped = run_profile_view(&db, "r1", None, Some(1)).unwrap();
        assert!(capped.rows.len() <= 1);
        assert_eq!(capped.sort, tare_core::flamegraph::ProfileSort::Cum);

        // Cost×quality frontier: plots the stored run; pure-cost until scored.
        let fr = run_frontier_view(&db).unwrap();
        assert!(!fr.points.is_empty());
        assert!(!fr.has_quality, "no quality attached yet");
        Store::open(&db)
            .unwrap()
            .set_run_quality("r1", 87, "ci", "2026-06-24")
            .unwrap();
        let scored = run_frontier_view(&db).unwrap();
        let point = scored
            .points
            .iter()
            .find(|point| point.run_id == "r1")
            .unwrap();
        assert_eq!(point.quality, Some(87));
        assert_eq!(point.quality_source.as_deref(), Some("ci"));

        // Trend view: default window resolves to the data, total spend is non-zero.
        let trend = trend_view(&db, None, None, "total").unwrap();
        assert!(
            !trend.days.is_empty(),
            "default window covers the recorded day"
        );
        assert_eq!(trend.series.len(), 1);
        assert!(trend.series[0].total_micros > 0);
        // by=provider names the provider; JSON form parses.
        let by_provider = trend_view(&db, None, None, "provider").unwrap();
        assert_eq!(by_provider.series[0].key, "anthropic");
        let _: serde_json::Value =
            serde_json::from_str(&trend_json(&db, None, None, "cause").unwrap()).unwrap();

        // Transport-parity view functions each produce well-formed JSON for the shell.
        let status = run_status_json(&db, "r1").unwrap();
        assert!(status.contains("\"steps\":1") && status.contains("\"run_id\":\"r1\""));
        assert!(!explain_view(&db, "r1").unwrap().is_empty());
        let _: serde_json::Value = serde_json::from_str(&advise_json(&db).unwrap()).unwrap();
        assert!(whatif_json(&db, false).unwrap().contains("recommendations"));
        let _: serde_json::Value =
            serde_json::from_str(&anomalies_json(&db, "total", 7, 50).unwrap()).unwrap();
        assert!(diff_json(&db, "r1", "r1")
            .unwrap()
            .contains("\"delta_micros\":0"));
        assert!(pricing_json().unwrap().contains("effective_date"));
        // Receipt round-trips through verify (recomputation).
        assert!(receipt_json(&db, "r1", false)
            .unwrap()
            .contains("recomputed_total_micros"));

        // Config round trip: save config JSON and read it back with the same shape.
        let cfgpath = std::env::temp_dir().join(format!("tare-cfg-{}.toml", std::process::id()));
        let cfgpath = cfgpath.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&cfgpath);
        assert!(config_get_json_at(&cfgpath).unwrap().contains("\"budget\""));
        config_save_json_at(
            &cfgpath,
            r#"{"budget":{"max_spend_usd":0.5},"privacy":{"profile":"max_private"},"providers":{},"proxy":{"port":8790}}"#,
        )
        .unwrap();
        let got = config_get_json_at(&cfgpath).unwrap();
        assert!(
            got.contains("\"max_spend_usd\":0.5")
                && got.contains("max_private")
                && got.contains("8790")
        );
        let _ = std::fs::remove_file(&cfgpath);

        // Rollup and per-run export produce well-formed content.
        let _: serde_json::Value =
            serde_json::from_str(&rollup_json(&db, "step", None, None).unwrap()).unwrap();
        // the filtered drill path parses too (filter_by+filter both present).
        let _: serde_json::Value = serde_json::from_str(
            &rollup_json(&db, "session", Some("template"), Some("template#0")).unwrap(),
        )
        .unwrap();
        assert!(export_view(&db, "r1", "otel")
            .unwrap()
            .contains("resourceSpans"));
        assert!(export_view(&db, "r1", "receipt")
            .unwrap()
            .contains("tare-receipt"));
        assert!(export_view(&db, "r1", "nope").is_err());

        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn savings_json_uses_live_action_store_totals() {
        // the desktop producer reads the same Store aggregation as HTTP. Applied
        // point exposure is live, dismissed rows are excluded, and all pre-v2 fields remain present.
        let tmp = std::env::temp_dir().join(format!(
            "tare-tauri-savings-lifecycle-{}.db",
            std::process::id()
        ));
        let db = tmp.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        let spec = "{\"timezone\":\"UTC\",\"entity\":\"run\",\"filters\":[],\
             \"pricing\":{\"mode\":\"effective_dated\"},\"metric\":\"spend_micros\",\
             \"normalization\":\"absolute\"}";
        let applied = format!(
            "{{\"opportunity_key\":\"cache:applied\",\"cohort\":{spec},\
             \"match\":{{\"kind\":\"aggregate_only\"}},\"metric\":\"spend_micros\",\
             \"normalization\":\"absolute\",\"expected_point_micros\":3210}}"
        );
        let dismissed = applied
            .replace("cache:applied", "cache:dismissed")
            .replace("3210", "9999");
        savings_accept_json(&db, applied.as_bytes()).unwrap();
        savings_dismiss_json(&db, dismissed.as_bytes()).unwrap();

        let ledger: tare_core::savings::SavingsLedgerV2 =
            serde_json::from_str(&savings_json(&db).unwrap()).unwrap();
        let direct = Store::open(&db)
            .unwrap()
            .savings_lifecycle_totals(&pricing().unwrap())
            .unwrap();
        assert_eq!(ledger.applied_micros, 3_210);
        assert_eq!(ledger.applied_micros, direct.applied_micros);
        assert_eq!(ledger.observed_micros, direct.observed_micros);
        assert_eq!(
            ledger.capped_potential_micros,
            ledger.total_recoverable_micros
        );
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn trend_view_empty_store_is_well_formed() {
        let tmp = std::env::temp_dir().join(format!("tare-tauri-empty-{}.db", std::process::id()));
        let db = tmp.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        // Open + migrate an empty store, then query the trend.
        drop(Store::open(&db).unwrap());
        let t = trend_view(&db, None, None, "total").unwrap();
        assert!(t.days.is_empty() && t.series.is_empty());
        assert!(t.estimated);
        let _ = std::fs::remove_file(&db);
    }
}
