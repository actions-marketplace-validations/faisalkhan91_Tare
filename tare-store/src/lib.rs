//! Local SQLite persistence for Tare. Bundled SQLite (no system lib, no network).
//! Holds runs, steps (with their request shape), and persisted attribution rows.
//! Migrations run from empty. All accounting/attribution lives in `tare-core`; the
//! store only persists and reloads its inputs/outputs.

use rusqlite::{params, Connection, OptionalExtension};
use tare_core::anomaly::{AnomalyWhy, AnomalyWhyRequest};
use tare_core::attribute;
use tare_core::cohort::{
    AllocationMethod, AnalysisProvenance, AnalysisResponse, CohortCompareRequest,
    CohortCompareResult, CohortDimension, CohortEntity, CohortEntitySummary, CohortError,
    CohortFacetRequest, CohortFacetResult, CohortFilter, CohortMetric, CohortResolveResult,
    CohortSearchRequest, CohortSearchResult, CohortSpec, CohortTimelineRequest,
    CohortTimelineResult, ComponentFidelity, EntityRef, FacetRow, MatchRule, MeteredOutcome,
    Normalization, OutcomeDenominator, PricingEdition, PricingMode, SearchField, StepRef,
    TimelineConfigEvent, TimelineGroup, TimelinePoint, TimelineSeries, TimelineUnit, ValueClass,
    SEARCH_RESULT_CAP,
};
use tare_core::model::{
    CacheTtl, Provider, RequestShape, RunRecord, StepRecord, TodaySpend, UnixNanos, UsageTokens,
};
use tare_core::savings::{
    SavingsAction, SavingsActionIdentity, SavingsActionRequest, SavingsLifecycleTotals,
    SavingsVerifyRequest, SavingsVerifyResult,
};
use tare_core::trend::{self, DatedRun, TrendDimension, TrendReport};
use tare_core::{build_runs, PricingTable};

/// Opt-in redacted-transcript store — a SEPARATE sqlite file, never the counts DB.
pub mod transcript_store;
pub use transcript_store::{TranscriptRow, TranscriptStore};

// ---- shared run cache --------------------------------------------------------------
// Both `tare serve` and the Tauri desktop fire 6-13 whole-runs reads per screen, each of which
// re-opened SQLite and re-ran load_runs() — a full scan + reconstruction of the `steps` table. This
// lives in tare-store (not tare-cli) so the DESKTOP can use it too without tare-tauri depending on
// tare-cli (a forbidden dep). It caches only RAW, unpriced RunRecords — repricing/attribution stay
// downstream — so there is no stale-COST risk; the only freshness concern is "did new runs/steps land".
// Signature = (db file mtime/len, MAX(rowid) of `runs`, MAX(rowid) of `steps`). Those two tables are
// append/replace-only — INSERT OR IGNORE / INSERT OR REPLACE, verified no UPDATE/DELETE — so a landing
// or replaced step strictly raises `steps`' rightmost rowid (a replace is delete+insert → new rowid)
// and a new run raises `runs`'. Reading those two O(1) btree tips lets us IGNORE the ~3s WAL churn from
// writes to OTHER tables (scan_cursor/backfill_seen/session_activity/vendor_session_cost) that used to
// bump the -wal sidecar and dump this ~37MB raw-run cache on every idle sweep. The db
// file stat is kept as a conservative backstop (a checkpoint, a wholesale file swap, or a probe that
// couldn't read still invalidates). Read BEFORE the load, so a racing write causes at most one extra
// reload, never a stale hit. A cost tool must never show stale numbers; this stays biased to re-read.
type RunsSig = ((u128, u64), i64, i64); // (db file mtime/len, runs MAX(rowid), steps MAX(rowid))

fn file_stat(path: &str) -> (u128, u64) {
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

fn runs_sig(db: &str) -> RunsSig {
    let fs = file_stat(db);
    if !std::path::Path::new(db).exists() {
        return (fs, 0, 0);
    }
    // A bare read connection — no migrate, no pragmas — with a busy timeout so a concurrent capture
    // commit is waited out rather than misread as empty. MAX(rowid) is an O(1) rightmost-btree read;
    // a missing table (pre-migrate) or any error → 0, which the db-file-stat backstop still guards.
    let (mut runs_max, mut steps_max) = (0i64, 0i64);
    if let Ok(conn) = Connection::open(db) {
        let _ = conn.busy_timeout(std::time::Duration::from_secs(2));
        runs_max = conn
            .query_row("SELECT COALESCE(MAX(rowid), 0) FROM runs", [], |r| r.get(0))
            .unwrap_or(0);
        steps_max = conn
            .query_row("SELECT COALESCE(MAX(rowid), 0) FROM steps", [], |r| {
                r.get(0)
            })
            .unwrap_or(0);
    }
    (fs, runs_max, steps_max)
}

#[allow(clippy::type_complexity)]
fn runs_cache() -> &'static std::sync::Mutex<
    std::collections::HashMap<String, (RunsSig, std::sync::Arc<Vec<RunRecord>>)>,
> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::HashMap<String, (RunsSig, std::sync::Arc<Vec<RunRecord>>)>,
        >,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

// Per-db load lock for single-flight. Async Tauri commands and the multi-threaded serve read API
// mean a cold-cache screen load fires 6-13 DISTINCT whole-runs
// endpoints that ALL miss at once — without coordination each runs its own full steps-table scan.
// This map hands each db its own load mutex so concurrent misses on the same db collapse to ONE
// scan (the others wait, then hit the fresh entry), while different dbs never cross-block.
#[allow(clippy::type_complexity)]
fn load_locks() -> &'static std::sync::Mutex<
    std::collections::HashMap<String, std::sync::Arc<std::sync::Mutex<()>>>,
> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<std::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    LOCKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn cache_hit(db: &str, sig: RunsSig) -> Option<std::sync::Arc<Vec<RunRecord>>> {
    let map = runs_cache().lock().ok()?;
    let (cached_sig, runs) = map.get(db)?;
    (*cached_sig == sig).then(|| runs.clone())
}

/// Load every run for `db`, reusing a cached copy while the db (and its WAL) are byte-for-byte
/// unchanged since the last load. Returns an `Arc` so a hit is a refcount bump, not a clone; the
/// slice derefs transparently, so callers use it exactly like an owned `Vec<RunRecord>`. Concurrent
/// misses on the same db are single-flighted so they don't stampede `load_runs`.
pub fn cached_runs(db: &str) -> Result<std::sync::Arc<Vec<RunRecord>>, String> {
    let sig = runs_sig(db); // BEFORE the load — never label the runs newer than they actually are
    if let Some(runs) = cache_hit(db, sig) {
        return Ok(runs);
    }

    // Miss: take this db's load lock so only ONE thread scans; hold the map lock only for the O(1)
    // get-or-insert of the per-db lock, never across the load.
    let lock = {
        let mut locks = load_locks().lock().map_err(|e| e.to_string())?;
        if locks.len() > 8 {
            locks.clear();
        }
        locks
            .entry(db.to_string())
            .or_insert_with(|| std::sync::Arc::new(std::sync::Mutex::new(())))
            .clone()
    };
    let _guard = lock.lock().map_err(|e| e.to_string())?;

    // Double-check under the load lock: a peer that raced us here may have just populated the cache
    // for the current file state. Re-read the signature fresh so we honor any write since our first probe.
    let sig = runs_sig(db);
    if let Some(runs) = cache_hit(db, sig) {
        return Ok(runs);
    }

    let runs = std::sync::Arc::new(Store::open(db)?.load_runs()?);
    if let Ok(mut map) = runs_cache().lock() {
        if map.len() > 8 {
            map.clear(); // bound memory if the path ever varies (tests / alternate dbs)
        }
        map.insert(db.to_string(), (sig, runs.clone()));
    }
    Ok(runs)
}

pub struct Store {
    conn: Connection,
}

/// Recorded provenance for a single run. Counts/labels only — no payload. The
/// pricing version/date is layered on by the CLI/desktop adapter (it owns the pricing table).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RunMeta {
    pub run_id: String,
    pub created_date: String,
    pub privacy_policy_id: Option<String>,
    pub profile: Option<String>,
    pub steps: u32,
    pub models: Vec<String>,
    pub providers: Vec<String>,
    pub sources: Vec<String>,
    pub stop_reasons: Vec<String>,
}

/// A user-authored annotation on a run: free tags, a note, and a star. The user's own
/// words — never provider payload — stored locally and purgeable. `tags` is a JSON string array.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RunNote {
    pub run_id: String,
    pub tags: Vec<String>,
    pub note_text: String,
    pub starred: bool,
    pub updated_at: String,
}

/// A user-supplied per-run quality scalar — the y-axis of the cost×quality frontier.
/// Counts-only: Tare stores a number the user brings in, it NEVER computes or grades it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunQuality {
    pub run_id: String,
    /// An integer score the user assigns meaning to (e.g. pass/fail → 100/0, or a 0..100 rubric).
    pub score: i64,
    /// Provenance only: `cli` | `header` | `ci`. Never affects the value.
    pub source: String,
    pub updated_at: String,
}

/// A user's acceptance of a savings opportunity: the moment they acted, plus the
/// recoverable estimate snapshotted then, so realized savings can be proven against it later.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Acceptance {
    /// Stable opportunity identity: `kind:label` (e.g. `loop:search`).
    pub opportunity_key: String,
    pub accepted_date: String,
    /// The opportunity's recoverable estimate (micro-USD) at accept time.
    pub recoverable_micros: i64,
}

/// Length caps enforced at the store (the single choke point) so an oversized note/tag set is
/// rejected loudly rather than silently truncated.
const NOTE_TEXT_CAP: usize = 8192;
const TAGS_JSON_CAP: usize = 1024;
const STORED_ID_CAP: usize = 512;
const SHAPE_JSON_CAP: usize = 64 * 1024;
const ANALYSIS_BODY_CAP: usize = 256 * 1024;
const SEARCH_QUERY_CAP: usize = 1024;

fn validate_date_range(from: &str, to: &str, context: &str) -> Result<(), String> {
    let from_day = tare_core::calendar::parse_date(from)
        .ok_or_else(|| format!("{context} start must be a valid YYYY-MM-DD date"))?;
    let to_day = tare_core::calendar::parse_date(to)
        .ok_or_else(|| format!("{context} end must be a valid YYYY-MM-DD date"))?;
    if from_day > to_day {
        return Err(format!("{context} start must not be after its end"));
    }
    Ok(())
}

fn validate_step_for_storage(
    step: &StepRecord,
    date: &str,
    hour: Option<u8>,
    policy_id: Option<&str>,
    profile: Option<&str>,
    source: Option<&str>,
    shape_json: &str,
) -> Result<(), String> {
    if step.run_id.is_empty() || step.run_id.len() > STORED_ID_CAP {
        return Err(format!(
            "run id must be 1-{STORED_ID_CAP} bytes before persistence"
        ));
    }
    if step.model.is_empty() || step.model.len() > STORED_ID_CAP {
        return Err(format!(
            "model id must be 1-{STORED_ID_CAP} bytes before persistence"
        ));
    }
    if tare_core::calendar::parse_date(date).is_none() {
        return Err(format!(
            "capture date must be a valid YYYY-MM-DD value, got {date:?}"
        ));
    }
    if hour.is_some_and(|value| value > 23) {
        return Err("capture hour must be between 0 and 23".into());
    }
    if step.shape.model != step.model || step.shape.provider != step.provider {
        return Err("step pricing identity disagrees with its request shape".into());
    }
    if step.usage.reasoning > step.usage.output {
        return Err("reasoning tokens must be a subset of output tokens".into());
    }
    let sqlite_counts = [
        ("fresh_input", step.usage.fresh_input),
        ("cache_write_5m", step.usage.cache_write_5m),
        ("cache_write_1h", step.usage.cache_write_1h),
        ("cache_read", step.usage.cache_read),
        ("output", step.usage.output),
        ("reasoning", step.usage.reasoning),
        ("audio_input", step.usage.audio_input),
        ("audio_output", step.usage.audio_output),
        ("duration_ms", step.duration_ms),
    ];
    if let Some((name, value)) = sqlite_counts
        .into_iter()
        .find(|(_, value)| *value > i64::MAX as u64)
    {
        return Err(format!(
            "{name} value {value} exceeds SQLite's signed 64-bit integer range"
        ));
    }
    if shape_json.len() > SHAPE_JSON_CAP {
        return Err(format!(
            "request shape exceeds the {SHAPE_JSON_CAP}-byte persistence cap"
        ));
    }
    for (name, value) in [
        ("policy id", policy_id),
        ("privacy profile", profile),
        ("capture source", source),
        ("stop reason", step.stop_reason.as_deref()),
        ("trace id", step.trace_id.as_deref()),
        ("span id", step.span_id.as_deref()),
        ("parent span id", step.parent_span_id.as_deref()),
    ] {
        if value.is_some_and(|value| value.len() > STORED_ID_CAP) {
            return Err(format!(
                "{name} exceeds the {STORED_ID_CAP}-byte persistence cap"
            ));
        }
    }
    Ok(())
}

/// Ordered schema migrations. `MIGRATIONS[i]` upgrades from version `i` to `i+1`. Append a
/// new entry to bump the schema; the loop in `migrate()` ships both from-empty and
/// from-any-prior-version upgrade paths automatically. Never edit a released entry.
const MIGRATIONS: &[&str] = &[
    // v0 -> v1: base schema.
    "CREATE TABLE runs (
        run_id TEXT PRIMARY KEY,
        created_date TEXT NOT NULL
     );
     CREATE TABLE steps (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id TEXT NOT NULL,
        step_ordinal INTEGER NOT NULL,
        provider TEXT NOT NULL,
        model TEXT NOT NULL,
        fresh_input INTEGER NOT NULL,
        cache_write_5m INTEGER NOT NULL,
        cache_write_1h INTEGER NOT NULL,
        cache_read INTEGER NOT NULL,
        output INTEGER NOT NULL,
        reasoning INTEGER NOT NULL,
        shape_json TEXT NOT NULL,
        stop_reason TEXT,
        UNIQUE(run_id, step_ordinal)
     );
     CREATE TABLE attributions (
        scope TEXT NOT NULL,
        cause TEXT NOT NULL,
        detail TEXT NOT NULL,
        tokens INTEGER NOT NULL,
        micros INTEGER NOT NULL,
        projected_saved_micros INTEGER NOT NULL,
        PRIMARY KEY(scope, cause)
     );",
    // v1 -> v2: performance indexes (additive, idempotent).
    "CREATE INDEX IF NOT EXISTS idx_steps_run ON steps(run_id, step_ordinal);
     CREATE INDEX IF NOT EXISTS idx_runs_date ON runs(created_date);",
    // v2 -> v3: stamp each run with the privacy policy that produced it. Nullable, so
    // pre-v3 rows simply carry NULL. Runs once on the version transition (ALTER is safe here).
    "ALTER TABLE runs ADD COLUMN privacy_policy_id TEXT;
     ALTER TABLE runs ADD COLUMN profile TEXT;",
    // v3 -> v4: index for trend range/aggregation queries (perf only, additive).
    "CREATE INDEX IF NOT EXISTS idx_steps_provider_model ON steps(provider, model);",
    // v4 -> v5: operator-facing fired-alert set for the daemon's fire-once anomaly delivery
    // Keyed by AlertSubject::dedup_key() — for an anomaly that is (date, series_key,
    // kind), not run_id. Pure daemon state: never rendered or included in a golden.
    "CREATE TABLE IF NOT EXISTS fired_alerts (
        alert_key TEXT PRIMARY KEY,
        fired_date TEXT NOT NULL
     );",
    // v5 -> v6: capture-method tag per step (proxy | otel-span | otel-event). Nullable, so
    // pre-v6 rows carry NULL. Counts only: a fixed enum-like label, never payload text.
    "ALTER TABLE steps ADD COLUMN source TEXT;",
    // v6 -> v7: multimodal audio sub-class token axes. Default 0, so pre-v7 rows (and any text-
    // only capture) price audio at $0 and reconcile exactly with prior totals.
    "ALTER TABLE steps ADD COLUMN audio_input INTEGER NOT NULL DEFAULT 0;\n     ALTER TABLE steps ADD COLUMN audio_output INTEGER NOT NULL DEFAULT 0;",
    // v7 -> v8: durable mirror of the live per-session activity table so running-
    // session liveness survives a `tare serve` / daemon restart and the desktop can read it from
    // the DB. Counts-only: capture source + opaque session id + recency + event count + last model.
    "CREATE TABLE session_activity (
        source TEXT NOT NULL,
        session TEXT NOT NULL,
        last_unix INTEGER NOT NULL,
        events INTEGER NOT NULL,
        last_model TEXT,
        PRIMARY KEY (source, session)
     );",
    // v8 -> v9: vendor-reported aggregate metric series. Claude Code's own
    // claude_code.cost.usage / token.usage delta counters, summed per (day, metric, model, kind,
    // session). Stored SEPARATELY from per-step rows and surfaced only as a cross-check — never
    // folded into step-derived totals (that would double-count when logs are enabled). Counts/USD
    // only; `value` is micro-USD for cost rows and a token count for token rows.
    "CREATE TABLE metered_series (
        day TEXT NOT NULL,
        metric TEXT NOT NULL,
        model TEXT NOT NULL,
        kind TEXT NOT NULL,
        session TEXT NOT NULL,
        value INTEGER NOT NULL,
        PRIMARY KEY (day, metric, model, kind, session)
     );",
    // v9 -> v10: enrich the metered lane with `effort` (reasoning effort) + `query_source`
    // (main | subagent | …) so $/outcome can be computed on query_source='main' only (the
    // Claude-Code-prescribed attribution) and spend sliced by effort. Both join the PRIMARY KEY so
    // points differing only in effort/source don't collapse — which needs a table rebuild (SQLite
    // can't extend a PK in place). Existing rows backfill to ''.
    "ALTER TABLE metered_series RENAME TO metered_series_v9;
     CREATE TABLE metered_series (
        day TEXT NOT NULL,
        metric TEXT NOT NULL,
        model TEXT NOT NULL,
        kind TEXT NOT NULL,
        session TEXT NOT NULL,
        effort TEXT NOT NULL DEFAULT '',
        query_source TEXT NOT NULL DEFAULT '',
        value INTEGER NOT NULL,
        PRIMARY KEY (day, metric, model, kind, session, effort, query_source)
     );
     INSERT INTO metered_series (day, metric, model, kind, session, effort, query_source, value)
        SELECT day, metric, model, kind, session, '', '', value FROM metered_series_v9;
     DROP TABLE metered_series_v9;",
    // v10 -> v11: observed per-step latency in ms, measured at the capture edge. Additive
    // column; existing rows backfill to 0 (= unmeasured), matching the StepRecord serde default.
    "ALTER TABLE steps ADD COLUMN duration_ms INTEGER NOT NULL DEFAULT 0;",
    // v11 -> v12: user-authored run notes. The user's OWN words about their own runs —
    // NOT provider payload, so it's exempt from the #8 shape guard; purgeable and length-capped by
    // the writer. `tags` is a JSON array of short validated labels. Local-only (loopback write API).
    "CREATE TABLE run_notes (
        run_id     TEXT PRIMARY KEY,
        tags       TEXT NOT NULL DEFAULT '[]',
        note_text  TEXT NOT NULL DEFAULT '',
        starred    INTEGER NOT NULL DEFAULT 0,
        updated_at TEXT NOT NULL
     );",
    // v12 -> v13: user-supplied per-run QUALITY scalar — the y-axis of the cost×quality
    // frontier. Counts-only: an integer score the user brings in (a pass/fail mapped to 0/100, a CI
    // metric, an `x-tare-quality` header). HARD LINE — Tare never computes it: no scorer, no judge,
    // no payload read. `source` records provenance only (cli | header | ci). Local-only, purgeable.
    "CREATE TABLE run_quality (
        run_id     TEXT PRIMARY KEY,
        score      INTEGER NOT NULL,
        source     TEXT NOT NULL DEFAULT 'cli',
        updated_at TEXT NOT NULL
     );",
    // v13 -> v14: savings-realization lifecycle. When the user ACTS on a savings
    // opportunity they mark it accepted; we snapshot the date + the recoverable estimate at that
    // moment. Later, comparing total spend in the equal-length windows before vs after the accept
    // date PROVES whether the advice paid off — open → accepted → realized. Local-only, purgeable.
    "CREATE TABLE savings_acceptance (
        opportunity_key   TEXT PRIMARY KEY,
        accepted_date     TEXT NOT NULL,
        recoverable_micros INTEGER NOT NULL
     );",
    // v14 -> v15: JSONL-backfill dedup ledger. Each transcript request Tare has
    // already backfilled is remembered by its cross-source dedup key, so a re-scan / restart never
    // double-counts. Counts-only (a key + the session it belonged to); purgeable.
    "CREATE TABLE backfill_seen (
        dedup_key TEXT PRIMARY KEY,
        run_id    TEXT NOT NULL
     );",
    // v15 -> v16: statusline vendor-cost forward store. Claude Code's statusLine reports
    // its cumulative session cost (total_cost_usd) on EVERY invocation; we UPSERT the LATEST per
    // session (never append/sum, so repeated invocations can't double-count) so `tare report` can
    // cross-check Tare's estimate against the vendor figure. The vendor number is a LABELLED
    // cross-check, NEVER merged into the ledger/cost. Counts-only (a session id + integer micro-USD),
    // purgeable.
    "CREATE TABLE vendor_session_cost (
        session_id         TEXT PRIMARY KEY,
        vendor_cost_micros INTEGER NOT NULL,
        updated_at         TEXT NOT NULL
     );",
    // v16 -> v17: persisted JSONL scan cursor. The live tailer's ScanCursor
    // (path -> mtime,size) and TranscriptTailer (path -> byte offset) are in-memory only, so every
    // daemon/app restart would re-stat and RE-READ every transcript from the top — a full re-scan of
    // ~/.claude/projects on each open (the re-scan cost). Persist one row per file so catch-up on
    // start is INCREMENTAL: unchanged files (same mtime+size) are skipped entirely, and a grown file
    // resumes tailing from its stored byte offset. Counts-only (a path + three integers), purgeable;
    // the dedup ledger (backfill_seen) remains the correctness backstop, this is the perf/anti-storm
    // layer.
    "CREATE TABLE scan_cursor (
        path   TEXT PRIMARY KEY,
        mtime  INTEGER NOT NULL,
        size   INTEGER NOT NULL,
        offset INTEGER NOT NULL
     );",
    // v17 -> v18: hour-of-day for the day×hour punchcard. The store was day-granular
    // (created_date only); this adds a nullable local hour 0-23 derived at ingest from the source
    // turn's own timestamp (never a wall clock — the store stays clock-free). Nullable so existing
    // rows and lanes that don't carry a per-turn timestamp (e.g. some OTLP paths) are an honest GAP
    // (excluded from the punchcard) rather than bucketed at a fake hour. Local-only, purgeable.
    "ALTER TABLE runs ADD COLUMN created_hour INTEGER;",
    // v18 -> v19: index the steps.source column so sessions_with_foreign_steps —
    // run every JSONL sweep — plus source_counts / coverage_by_source SEEK instead of full-scanning
    // the wide steps table. Additive; no data change.
    "CREATE INDEX IF NOT EXISTS idx_steps_source ON steps(source, run_id);",
    // v19 -> v20: normalized step_dimensions index for interactive cohort facets.
    // Provider/Model/Source/date stay first-class columns; the allow-listed shape
    // dimensions are materialized here. SCHEMA ONLY — the backfill from existing shape_json runs as
    // a transactional post-migration Rust step in open() so it uses the SAME materializer as the
    // write path (guaranteeing "old DB migration and new writes produce identical dimensions").
    "CREATE TABLE step_dimensions (
        run_id TEXT NOT NULL,
        step_ordinal INTEGER NOT NULL,
        dimension TEXT NOT NULL,
        value TEXT NOT NULL,
        PRIMARY KEY (run_id, step_ordinal, dimension, value)
     );
     CREATE INDEX step_dimensions_lookup ON step_dimensions (dimension, value, run_id, step_ordinal);
     CREATE INDEX step_dimensions_step ON step_dimensions (run_id, step_ordinal);",
    // v20 -> v21: nullable OTLP timing + span relationships. All additive and
    // nullable, so pre-v21 rows (and every proxy/JSONL step that never had a real timestamp or span)
    // load as NULL -> None. `start_unix_nano` is a decimal STRING in a TEXT column: SQLite's signed
    // 64-bit INTEGER cannot safely hold Unix nanoseconds, so we never store it as an integer. The
    // display end time is derived from start + duration_ms, so no redundant end column is persisted.
    "ALTER TABLE steps ADD COLUMN start_unix_nano TEXT;
     ALTER TABLE steps ADD COLUMN trace_id TEXT;
     ALTER TABLE steps ADD COLUMN span_id TEXT;
     ALTER TABLE steps ADD COLUMN parent_span_id TEXT;",
    // v21 -> v22: durable saved investigations. The SOLE source of truth for
    // the browser and desktop (the v1 localStorage / saved_views.json bridge has been removed).
    // The full DTO (state minus transient focus, columns, pane
    // widths) lives in `state_json`; id/label/version/timestamps are also columns for listing +
    // ordering without parsing every blob. Active focus/highlight is NEVER written here.
    "CREATE TABLE saved_investigations (
        id TEXT PRIMARY KEY,
        label TEXT NOT NULL,
        version INTEGER NOT NULL,
        state_json TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
     );",
    // v22 -> v23: savings action lifecycle. A NEW v2 action table keyed by
    // (opportunity_key, cohort_hash) — the exact intervention the user applied/dismissed, with the
    // full cohort/baseline/match/metric snapshot needed to verify it later. Distinct from the
    // legacy three-column `savings_acceptance`, which stays readable and is migrated in Rust into
    // aggregate-only `applied` rows (never faking a historical cohort snapshot).
    "CREATE TABLE savings_actions (
        opportunity_key TEXT NOT NULL,
        cohort_hash TEXT NOT NULL,
        status TEXT NOT NULL,
        acted_at TEXT NOT NULL,
        cohort_json TEXT NOT NULL,
        baseline_json TEXT,
        match_json TEXT,
        metric TEXT NOT NULL,
        normalization TEXT NOT NULL,
        outcome_json TEXT,
        expected_low_micros INTEGER,
        expected_point_micros INTEGER,
        expected_high_micros INTEGER,
        quality_guardrail INTEGER,
        PRIMARY KEY (opportunity_key, cohort_hash)
     );",
    // v23 -> v24: authoritative local Settings-save history for timeline markers.
    // Metadata only: the UTC instant, fixed source label, and a sorted JSON array of changed FIELD
    // NAMES. Configuration values, secrets, salts, URLs, and payload text are never stored here.
    "CREATE TABLE config_change_events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        occurred_at TEXT NOT NULL,
        source TEXT NOT NULL,
        changed_fields_json TEXT NOT NULL
     );
     CREATE INDEX config_change_events_time ON config_change_events (occurred_at, id);",
];

/// One persisted `scan_cursor` row: a transcript file's last-seen `(mtime, size)`
/// signature plus the byte `offset` tailed so far. Seeds the in-memory `ScanCursor` +
/// `TranscriptTailer` on daemon/app start so catch-up skips unchanged files and resumes a grown file
/// from where it left off — no full re-read of `~/.claude/projects` on every open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanCursorRow {
    pub path: String,
    pub mtime: i64,
    pub size: u64,
    pub offset: u64,
}

/// Saved-investigation size caps: one serialized investigation ≤ 256 KiB, the
/// whole list ≤ 1 MiB — so a hostile or runaway client can't bloat the DB.
const INVESTIGATION_MAX_BYTES: usize = 256 * 1024;
const INVESTIGATIONS_TOTAL_MAX_BYTES: usize = 1024 * 1024;

/// Top-level JSON keys a persisted `shape_json` is allowed to carry. Anything else is rejected
/// on write so a future field that smuggles payload text can never silently land in the DB
/// (the `deny.new_text_columns` store guard).
const SHAPE_KEY_ALLOWLIST: &[&str] = &[
    "model",
    "provider",
    "stream",
    "ttl",
    "has_cache_control",
    "cached_component",
    "system_hash",
    "weights",
    "request_hash",
    // Adapter correlation labels (opaque short strings, truncated at the proxy — not payload).
    "step_label",
    "component_label",
    "parent_label",
    "attempt",
    // Owning session/conversation id (opaque correlation id, like run_id — never payload text).
    "session",
    // Reasoning effort level (low/medium/high/xhigh/max) — a short label, never payload.
    "effort",
    // MCP server id that originated the request (opaque short label, OTel-only) — never payload.
    "mcp_server",
    // OpenAI-compatible vendor label (groq/together/openrouter/…) — opaque short label, the pricing
    // dimension for Provider::OpenAiCompatible, never payload.
    "vendor",
    // Git attribution: commit SHA + author of the working tree (opaque short labels, never a diff
    // or message) — the git-blame-for-cost rollup dimension.
    "commit",
    "author",
    // User-provided workload key: opaque grouping label, truncated to 64
    // UTF-8-safe chars at the capture edge — never derived from payload.
    "workload_key",
];

/// Reject a serialized shape that carries any key outside the allowlist. The allowlist is the
/// set of `RequestShape` fields, all of which are counts/weights/hashes/enums — never text.
fn check_shape_allowlist(shape_json: &str) -> Result<(), String> {
    let v: serde_json::Value =
        serde_json::from_str(shape_json).map_err(|e| format!("shape json: {e}"))?;
    let obj = v
        .as_object()
        .ok_or("shape json must be a JSON object".to_string())?;
    for key in obj.keys() {
        if !SHAPE_KEY_ALLOWLIST.contains(&key.as_str()) {
            return Err(format!(
                "shape carries non-allowlisted key {key:?}; refusing to persist (privacy guard)"
            ));
        }
    }
    Ok(())
}

/// Materialize a step's allow-listed shape dimensions as `(dimension, value)` rows for the
/// `step_dimensions` index. This is the SINGLE source of truth used by
/// BOTH the write path and the backfill, so an old DB's backfilled rows are byte-identical to what a
/// fresh write would produce. Provider/Model/Source/run-date stay on their first-class columns and
/// are NOT duplicated here; CacheClass is derived by the attribution engine, not stored per step.
///
/// Alias note for `RollupDim`: `parent_label` is stored once under `"parent"` and `component_label`
/// once under `"component"`; the resolve engine maps the agent-vocabulary
/// `agent -> parent` and `tool -> component`. `workload_key` is the
/// user-provided grouping label, materialized here so it facets, searches, and matches like any dim.
fn step_dimension_rows(shape: &RequestShape) -> Vec<(&'static str, String)> {
    fn push(out: &mut Vec<(&'static str, String)>, dim: &'static str, v: &Option<String>) {
        if let Some(s) = v {
            if !s.is_empty() {
                out.push((dim, s.clone()));
            }
        }
    }
    let mut out: Vec<(&'static str, String)> = Vec::new();
    push(&mut out, "session", &shape.session);
    push(&mut out, "parent", &shape.parent_label);
    push(&mut out, "component", &shape.component_label);
    push(&mut out, "step", &shape.step_label);
    push(&mut out, "effort", &shape.effort);
    push(&mut out, "mcp_server", &shape.mcp_server);
    push(&mut out, "vendor", &shape.vendor);
    push(&mut out, "commit", &shape.commit);
    push(&mut out, "author", &shape.author);
    push(&mut out, "workload_key", &shape.workload_key);
    if let Some(h) = shape.system_hash {
        out.push(("template", format!("{h:016x}")));
    }
    out.push((
        "ttl",
        match shape.ttl {
            CacheTtl::FiveMin => "5m",
            CacheTtl::OneHour => "1h",
        }
        .to_string(),
    ));
    out
}

/// Refresh the `step_dimensions` rows for one step: delete any prior rows for `(run_id, ordinal)`
/// then insert the current materialization. Idempotent (safe to re-run when a step is re-recorded
/// via INSERT OR REPLACE). Runs on the caller's connection so it composes inside a transaction.
fn write_step_dimensions(
    conn: &Connection,
    run_id: &str,
    step_ordinal: u32,
    shape: &RequestShape,
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM step_dimensions WHERE run_id = ?1 AND step_ordinal = ?2",
        params![run_id, step_ordinal],
    )?;
    let mut stmt =
        conn.prepare_cached("INSERT OR IGNORE INTO step_dimensions VALUES (?1, ?2, ?3, ?4)")?;
    for (dim, val) in step_dimension_rows(shape) {
        stmt.execute(params![run_id, step_ordinal, dim, val])?;
    }
    Ok(())
}

/// Clamp a persisted token count to non-negative, logging a diagnostic on corruption
/// (rather than silently wrapping a negative i64 into a huge u64).
fn nonneg(v: i64, field: &str) -> u64 {
    if v < 0 {
        eprintln!("tare-store: clamped negative {field} ({v}) to 0 on load");
        0
    } else {
        v as u64
    }
}

/// Parse a persisted provider tag. Unknown values are an error rather than silently
/// defaulting to Anthropic, so a corrupt or forward-version row cannot be misattributed.
fn provider_from_str(s: &str) -> Result<Provider, String> {
    // Delegate to the canonical parser so a new Provider variant (e.g. openai_compatible)
    // round-trips through the store without a second list to keep in sync.
    Provider::parse(s).ok_or_else(|| format!("unknown provider in stored row: {s:?}"))
}

/// Serialize a snake_case unit enum (e.g. `CohortMetric`, `Normalization`) to its BARE wire token
/// for a TEXT column — `"spend_micros"`, not the quoted JSON `"\"spend_micros\""`.
fn wire_enum<T: serde::Serialize>(v: &T) -> Result<String, String> {
    match serde_json::to_value(v).map_err(|e| e.to_string())? {
        serde_json::Value::String(s) => Ok(s),
        other => Err(format!("expected a string enum token, got {other}")),
    }
}

/// Inverse of [`wire_enum`]: parse a bare wire token back into a unit enum.
fn parse_wire_enum<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, String> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).map_err(|e| e.to_string())
}

/// The empty AGGREGATE cohort: whole-store, no filters — the honest scope for a legacy
/// acceptance that has no captured cohort snapshot. Its `cohort_hash` is stable, so migrating the
/// same legacy row twice is idempotent.
fn aggregate_cohort() -> tare_core::cohort::CohortSpec {
    tare_core::cohort::CohortSpec {
        from: None,
        to: None,
        timezone: "UTC".to_string(),
        entity: tare_core::cohort::CohortEntity::Run,
        filters: Vec::new(),
        pricing: tare_core::cohort::PricingMode::EffectiveDated,
        metric: tare_core::cohort::CohortMetric::SpendMicros,
        normalization: tare_core::cohort::Normalization::Absolute,
        outcome_denominator: None,
    }
}

impl Store {
    pub fn open(path: &str) -> Result<Self, String> {
        // SQLite won't create intermediate directories, so a path like `~/.tare/tare.db` (the
        // desktop default) fails on first launch unless we create the parent first.
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("create db dir {}: {e}", parent.display()))?;
            }
        }
        let conn = Connection::open(path).map_err(|e| format!("open db: {e}"))?;
        // WAL lets the single writer thread commit without blocking concurrent readers
        // (CLI report/flamegraph) on the same on-disk DB. Best-effort: ignored for the
        // in-memory store and harmless if the platform falls back to the default journal.
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        // Wait-and-retry on a locked DB instead of erroring immediately (default timeout is 0).
        // Multiple tare processes touch one file — the daemon writes while CLI commands open and
        // read (and `tare backfill` / session beats / migrate-on-open write) — so write-write
        // contention and checkpoint collisions are expected. A busy_timeout turns a hard
        // `SQLITE_BUSY` ("database is locked") into a brief wait. Pure lock-wait behavior; no
        // durability tradeoff (unlike synchronous=NORMAL). Set BEFORE migrate() so the migration
        // write is covered too.
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
        // Read-oriented tuning. The hot read path is the wide `steps ⋈ runs` scan +
        // ORDER BY in load_runs, plus every cache-miss reconstruction; these three pure-perf pragmas
        // help it and cost nothing in durability (unlike synchronous=NORMAL, deliberately NOT set).
        // They reset per-connection, so they must be applied on every open.
        //   mmap_size  : read pages straight from a memory map instead of read()/copy (256 MB cap).
        //   cache_size : negative = KiB, so -64000 ≈ 64 MB page cache (default is ~2 MB) — keeps the
        //                steps b-tree hot across the burst of read endpoints one screen fires.
        //   temp_store : build the ORDER BY's transient b-trees in RAM, not a temp file.
        let _ = conn.pragma_update(None, "mmap_size", 268_435_456i64);
        let _ = conn.pragma_update(None, "cache_size", -64_000i64);
        let _ = conn.pragma_update(None, "temp_store", "MEMORY");
        let mut s = Store { conn };
        let from = s.migrate()?;
        s.backfill_step_dimensions_if_migrated(from)?;
        s.migrate_legacy_acceptances_if_migrated(from)?;
        Ok(s)
    }

    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("open db: {e}"))?;
        // Moot for a per-connection in-memory DB, but set for parity with `open` so both
        // constructors configure the connection identically.
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
        let mut s = Store { conn };
        let from = s.migrate()?;
        s.backfill_step_dimensions_if_migrated(from)?;
        s.migrate_legacy_acceptances_if_migrated(from)?;
        Ok(s)
    }

    /// Versioned, idempotent migration. `MIGRATIONS[i]` upgrades schema version `i -> i+1`;
    /// the loop applies every pending step (from empty OR from any prior version), each in its
    /// own transaction that also records the new version.
    /// Returns the schema version the DB was at BEFORE migrating, so the caller can run one-time
    /// post-migration backfills (e.g. step_dimensions) only when a relevant migration just applied.
    fn migrate(&mut self) -> Result<i64, String> {
        self.conn
            .execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);")
            .map_err(|e| format!("migrate: {e}"))?;
        let mut current: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |r| r.get(0),
            )
            .map_err(|e| format!("migrate read: {e}"))?;
        // Forward-version guard: a DB stamped NEWER than this build knows was
        // written by a newer tare (the user downgraded). The migration loop would silently no-op and
        // we'd then read/write against an unknown schema — corrupting data or erroring cryptically.
        // Refuse to open it with an actionable message instead.
        if current < 0 {
            return Err(format!(
                "database schema version is invalid ({current}); restore an uncorrupted database"
            ));
        }
        if current as usize > MIGRATIONS.len() {
            return Err(format!(
                "database schema v{current} is newer than this build supports (max v{}); \
                 it was written by a newer tare. Upgrade tare, or point --db at a different path.",
                MIGRATIONS.len()
            ));
        }
        let from_version = current;
        while (current as usize) < MIGRATIONS.len() {
            let next = current + 1;
            let batch = format!(
                "BEGIN;\n{}\nINSERT INTO schema_version(version) VALUES ({next});\nCOMMIT;",
                MIGRATIONS[current as usize]
            );
            self.conn
                .execute_batch(&batch)
                .map_err(|e| format!("migrate v{next}: {e}"))?;
            current = next;
        }
        Ok(from_version)
    }

    /// One-time post-migration backfill of `step_dimensions` from existing `shape_json`.
    /// Runs ONLY when a migration was just applied during this open
    /// (`from < MIGRATIONS.len()`), so a normal reopen never rescans the steps table. Uses the same
    /// [`step_dimension_rows`] materializer as the write path, so backfilled rows are byte-identical
    /// to what a fresh write produces (`INSERT OR IGNORE`, transactional). Fresh/empty DBs are a
    /// no-op.
    fn backfill_step_dimensions_if_migrated(&self, from: i64) -> Result<(), String> {
        if (from as usize) >= MIGRATIONS.len() {
            return Ok(());
        }
        let rows: Vec<(String, u32, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT run_id, step_ordinal, shape_json FROM steps")
                .map_err(|e| format!("backfill prepare: {e}"))?;
            let mapped = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, u32>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .map_err(|e| format!("backfill query: {e}"))?;
            mapped
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("backfill row: {e}"))?
        };
        if rows.is_empty() {
            return Ok(());
        }
        self.conn
            .execute_batch("BEGIN")
            .map_err(|e| format!("backfill begin: {e}"))?;
        let res = (|| {
            for (run_id, ordinal, shape_json) in &rows {
                // A legacy/degenerate row whose shape_json isn't a current RequestShape (e.g. an old
                // hand-written `{}`) must not brick open(): skip it — it simply gets no dimensions
                // (the write path never emits an unparseable shape). Honest: no fabricated dims.
                let Ok(shape) = serde_json::from_str::<RequestShape>(shape_json) else {
                    continue;
                };
                write_step_dimensions(&self.conn, run_id, *ordinal, &shape)
                    .map_err(|e| format!("backfill insert: {e}"))?;
            }
            Ok::<(), String>(())
        })();
        match res {
            Ok(()) => self
                .conn
                .execute_batch("COMMIT")
                .map_err(|e| format!("backfill commit: {e}")),
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Persist a step, stamping its run with `date` (YYYY-MM-DD) on first sight.
    pub fn record_step(&self, step: &StepRecord, date: &str) -> Result<(), String> {
        self.record_step_with_policy(step, date, None, None, None)
    }

    /// As [`record_step`], stamping the run with the privacy policy that produced it.
    /// The shape is allowlist-checked BEFORE any insert, so a rejected step persists nothing.
    pub fn record_step_with_policy(
        &self,
        step: &StepRecord,
        date: &str,
        policy_id: Option<&str>,
        profile: Option<&str>,
        source: Option<&str>,
    ) -> Result<(), String> {
        self.record_step_with_time(step, date, None, policy_id, profile, source)
    }

    /// As [`record_step_with_policy`], also stamping the run's local hour-of-day (0-23) for the
    /// day×hour punchcard. `hour` is derived by the caller from the SOURCE turn's own
    /// timestamp (keeping the store clock-free); `None` leaves it an honest GAP (the run is excluded
    /// from the punchcard rather than bucketed at a fake hour). Set only on first sight of the run.
    pub fn record_step_with_time(
        &self,
        step: &StepRecord,
        date: &str,
        hour: Option<u8>,
        policy_id: Option<&str>,
        profile: Option<&str>,
        source: Option<&str>,
    ) -> Result<(), String> {
        let shape_json =
            serde_json::to_string(&step.shape).map_err(|e| format!("shape json: {e}"))?;
        check_shape_allowlist(&shape_json)?;
        validate_step_for_storage(step, date, hour, policy_id, profile, source, &shape_json)?;
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("insert step begin: {e}"))?;
        tx
            .execute(
                "INSERT OR IGNORE INTO runs(run_id, created_date, created_hour, privacy_policy_id, profile) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![step.run_id, date, hour, policy_id, profile],
            )
            .map_err(|e| format!("insert run: {e}"))?;
        tx
            .execute(
                "INSERT OR REPLACE INTO steps
                 (run_id, step_ordinal, provider, model, fresh_input, cache_write_5m, cache_write_1h, cache_read, output, reasoning, shape_json, stop_reason, source, audio_input, audio_output, duration_ms, start_unix_nano, trace_id, span_id, parent_span_id)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
                params![
                    step.run_id,
                    step.step_ordinal,
                    step.provider.as_str(),
                    step.model,
                    step.usage.fresh_input,
                    step.usage.cache_write_5m,
                    step.usage.cache_write_1h,
                    step.usage.cache_read,
                    step.usage.output,
                    step.usage.reasoning,
                    shape_json,
                    step.stop_reason,
                    source,
                    step.usage.audio_input,
                    step.usage.audio_output,
                    step.duration_ms,
                    // Unix nanoseconds as a decimal STRING, never a signed-64 INTEGER.
                    step.start_unix_nano.map(|n| n.to_decimal_string()),
                    step.trace_id,
                    step.span_id,
                    step.parent_span_id,
                ],
            )
            .map_err(|e| format!("insert step: {e}"))?;
        // Maintain the step_dimensions index alongside the step.
        write_step_dimensions(&tx, &step.run_id, step.step_ordinal, &step.shape)
            .map_err(|e| format!("insert step dimensions: {e}"))?;
        tx.commit().map_err(|e| format!("insert step commit: {e}"))
    }

    /// Batch-insert a run of transcript steps in ONE transaction with cached statements
    /// The per-step [`record_step_with_time`] path commits in autocommit, so under
    /// WAL+`synchronous=FULL` a backfill fsync'd once *per turn* — turning a 150K-step import into
    /// 150K disk flushes. This wraps the whole file's steps in a single `BEGIN/COMMIT` (one flush)
    /// and reuses two `prepare_cached` statements across the batch. Semantics are identical to
    /// calling `record_step_with_time` per row with `policy_id = None`: `date`/`hour` stamp the run
    /// on first sight, `source`/`profile` are the shared provenance. Each shape is serialized and
    /// allowlist-checked BEFORE the transaction opens, so a rejected shape commits nothing.
    pub fn record_transcript_steps(
        &self,
        rows: &[(StepRecord, String, Option<u8>)],
        // Dedup-ledger entries (dedup_key, run_id) commit in the same transaction as the steps.
        // This makes the step insert and dedup mark atomic, so a failed
        // ledger write can't leave committed steps un-deduped — which the transcript-capture sweep
        // would otherwise re-ingest with fresh ordinals on the next pass, permanently double-counting.
        marks: &[(String, String)],
        profile: Option<&str>,
        source: Option<&str>,
    ) -> Result<(), String> {
        if rows.is_empty() && marks.is_empty() {
            return Ok(());
        }
        // Serialize + allowlist-check up front (outside the txn): a bad shape aborts before any write.
        let mut prepared: Vec<(&StepRecord, &str, Option<u8>, String)> =
            Vec::with_capacity(rows.len());
        for (step, date, hour) in rows {
            let shape_json =
                serde_json::to_string(&step.shape).map_err(|e| format!("shape json: {e}"))?;
            check_shape_allowlist(&shape_json)?;
            validate_step_for_storage(step, date, *hour, None, profile, source, &shape_json)?;
            prepared.push((step, date.as_str(), *hour, shape_json));
        }
        for (key, run_id) in marks {
            if key.is_empty()
                || key.len() > 1024
                || run_id.is_empty()
                || run_id.len() > STORED_ID_CAP
            {
                return Err("dedup keys and run ids must be non-empty and bounded".into());
            }
        }
        self.conn
            .execute_batch("BEGIN")
            .map_err(|e| format!("record_transcript_steps begin: {e}"))?;
        let res = (|| {
            let mut run_stmt = self
                .conn
                .prepare_cached(
                    "INSERT OR IGNORE INTO runs(run_id, created_date, created_hour, privacy_policy_id, profile) VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .map_err(|e| format!("prepare run: {e}"))?;
            let mut step_stmt = self
                .conn
                .prepare_cached(
                    "INSERT OR REPLACE INTO steps
                     (run_id, step_ordinal, provider, model, fresh_input, cache_write_5m, cache_write_1h, cache_read, output, reasoning, shape_json, stop_reason, source, audio_input, audio_output, duration_ms, start_unix_nano, trace_id, span_id, parent_span_id)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
                )
                .map_err(|e| format!("prepare step: {e}"))?;
            for (step, date, hour, shape_json) in &prepared {
                run_stmt
                    .execute(params![
                        step.run_id,
                        date,
                        hour,
                        Option::<&str>::None,
                        profile
                    ])
                    .map_err(|e| format!("insert run: {e}"))?;
                step_stmt
                    .execute(params![
                        step.run_id,
                        step.step_ordinal,
                        step.provider.as_str(),
                        step.model,
                        step.usage.fresh_input,
                        step.usage.cache_write_5m,
                        step.usage.cache_write_1h,
                        step.usage.cache_read,
                        step.usage.output,
                        step.usage.reasoning,
                        shape_json,
                        step.stop_reason,
                        source,
                        step.usage.audio_input,
                        step.usage.audio_output,
                        step.duration_ms,
                        // Unix nanoseconds as a decimal STRING, never a signed-64 INTEGER.
                        step.start_unix_nano.map(|n| n.to_decimal_string()),
                        step.trace_id,
                        step.span_id,
                        step.parent_span_id,
                    ])
                    .map_err(|e| format!("insert step: {e}"))?;
                // Maintain step_dimensions in the SAME transaction.
                write_step_dimensions(&self.conn, &step.run_id, step.step_ordinal, &step.shape)
                    .map_err(|e| format!("insert step dimensions: {e}"))?;
            }
            // Dedup marks share the transaction: commit or roll back atomically with
            // the steps so the capture sweep's rollback is sound (no committed-but-unmarked turns).
            for (key, run_id) in marks {
                self.conn
                    .execute(
                        "INSERT OR IGNORE INTO backfill_seen (dedup_key, run_id) VALUES (?1, ?2)",
                        params![key, run_id],
                    )
                    .map_err(|e| format!("insert dedup mark: {e}"))?;
            }
            Ok::<(), String>(())
        })();
        match res {
            Ok(()) => self
                .conn
                .execute_batch("COMMIT")
                .map_err(|e| format!("record_transcript_steps commit: {e}")),
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Beat a session's durable activity row: set recency + last model and increment
    /// the event count, upserting on (source, session). Mirrors the receiver's in-memory table so
    /// liveness survives a restart. Counts-only.
    pub fn record_session_beat(
        &self,
        source: &str,
        session: &str,
        last_unix: i64,
        last_model: &str,
    ) -> Result<(), String> {
        if source.is_empty()
            || source.len() > 64
            || session.is_empty()
            || session.len() > STORED_ID_CAP
            || last_model.len() > STORED_ID_CAP
            || last_unix < 0
        {
            return Err("session activity fields are empty, oversized, or invalid".into());
        }
        self.conn
            .execute(
                "INSERT INTO session_activity (source, session, last_unix, events, last_model)
                 VALUES (?1, ?2, ?3, 1, ?4)
                 ON CONFLICT(source, session) DO UPDATE SET
                   last_unix = MAX(last_unix, ?3),
                   events = CASE WHEN events < 9223372036854775807 THEN events + 1 ELSE events END,
                   last_model = CASE WHEN ?3 >= last_unix THEN ?4 ELSE last_model END",
                params![source, session, last_unix, last_model],
            )
            .map_err(|e| format!("session beat: {e}"))?;
        Ok(())
    }

    /// Record one vendor-reported metric delta, summing into the `(day, metric, model,
    /// kind, session, effort, query_source)` series. Delta temporality means each point
    /// is an increment, so we add.
    pub fn record_metered(&self, p: &tare_core::otel::MeteredPoint) -> Result<(), String> {
        const METRICS: &[&str] = &[
            "cost",
            "token",
            "lines_of_code",
            "pull_request",
            "commit",
            "session",
            "active_time",
            "edit_decision",
        ];
        if tare_core::calendar::parse_date(&p.day).is_none()
            || !METRICS.contains(&p.metric.as_str())
            || p.value <= 0
        {
            return Err("metered point has an invalid day, metric, or non-positive delta".into());
        }
        for (name, value) in [
            ("model", p.model.as_str()),
            ("kind", p.kind.as_str()),
            ("session", p.session.as_str()),
            ("effort", p.effort.as_str()),
            ("query source", p.query_source.as_str()),
        ] {
            if value.len() > STORED_ID_CAP {
                return Err(format!(
                    "metered {name} exceeds the {STORED_ID_CAP}-byte persistence cap"
                ));
            }
        }
        self.conn
            .execute(
                "INSERT INTO metered_series (day, metric, model, kind, session, effort, query_source, value)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(day, metric, model, kind, session, effort, query_source) DO UPDATE SET
                   value = CASE
                     WHEN value > 9223372036854775807 - ?8 THEN 9223372036854775807
                     ELSE value + ?8
                   END",
                params![p.day, p.metric, p.model, p.kind, p.session, p.effort, p.query_source, p.value],
            )
            .map_err(|e| format!("record_metered: {e}"))?;
        Ok(())
    }

    /// Outcome counts over a `[from, to]` day window (inclusive), assembled from the metered series
    /// for cost-effectiveness: cost + PRs/commits/lines-added/active-seconds/sessions/
    /// edit accept+reject. A pure SUM per (metric, kind); empty window -> all zeros.
    ///
    /// Attribution: explicit `query_source='subagent'` rows are excluded so nested
    /// subagent spend doesn't inflate the numerator against a main-query outcome — the intent
    /// behind stamping query_source. We exclude only the explicit `subagent` marker (not "keep
    /// `main` only"): unstamped rows (`''` — older captures, non-Claude-Code providers that don't
    /// emit the attribute) are kept, so the ratio degrades gracefully instead of vanishing. The
    /// `edit_decision` metric carries no query_source at all (its `source` attr is the edit source),
    /// so its rows are never `subagent` and are unaffected by the filter.
    pub fn metered_outcomes(
        &self,
        from: &str,
        to: &str,
    ) -> Result<tare_core::effectiveness::OutcomeCounts, String> {
        validate_date_range(from, to, "metered outcome window")?;
        // One conditional-SUM pass over the window, not 8 separate full-window scans.
        let sql = "SELECT \
            COALESCE(SUM(CASE WHEN metric='cost' THEN value END),0), \
            COALESCE(SUM(CASE WHEN metric='pull_request' THEN value END),0), \
            COALESCE(SUM(CASE WHEN metric='commit' THEN value END),0), \
            COALESCE(SUM(CASE WHEN metric='lines_of_code' AND kind='added' THEN value END),0), \
            COALESCE(SUM(CASE WHEN metric='active_time' THEN value END),0), \
            COALESCE(SUM(CASE WHEN metric='session' THEN value END),0), \
            COALESCE(SUM(CASE WHEN metric='edit_decision' AND kind='accept' THEN value END),0), \
            COALESCE(SUM(CASE WHEN metric='edit_decision' AND kind='reject' THEN value END),0) \
            FROM metered_series WHERE day BETWEEN ?1 AND ?2 AND query_source != 'subagent'";
        let r = self
            .conn
            .query_row(sql, params![from, to], |row| {
                Ok([
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                ])
            })
            .map_err(|e| format!("metered_outcomes: {e}"))?;
        Ok(tare_core::effectiveness::OutcomeCounts {
            cost_micros: r[0].max(0),
            pull_requests: r[1].max(0) as u64,
            commits: r[2].max(0) as u64,
            lines_added: r[3].max(0) as u64,
            active_seconds: r[4].max(0) as u64,
            sessions: r[5].max(0) as u64,
            edits_accepted: r[6].max(0) as u64,
            edits_rejected: r[7].max(0) as u64,
        })
    }

    /// Persist one successful local Settings save as counts-only metadata.
    /// Callers pass changed FIELD PATHS only; the guard rejects anything outside the conservative
    /// identifier alphabet so a value/URL cannot accidentally enter this table.
    pub fn record_config_change_event(
        &self,
        occurred_at: &str,
        source: &str,
        changed_fields: &[String],
    ) -> Result<(), String> {
        if tare_core::calendar::parse_iso8601_to_secs(occurred_at).is_none() {
            return Err("config event occurred_at must be RFC3339/ISO-8601".to_string());
        }
        if source != "settings" {
            return Err("config event source must be settings".to_string());
        }
        let mut fields = changed_fields.to_vec();
        fields.sort();
        fields.dedup();
        if fields.is_empty() {
            return Ok(()); // an unchanged Save is not a configuration change
        }
        if fields.len() > 256
            || fields.iter().any(|field| {
                field.is_empty()
                    || field.len() > 128
                    || !field
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.'))
            })
        {
            return Err("config changed fields must be 1-256 dotted field names".to_string());
        }
        let json = serde_json::to_string(&fields).map_err(|e| e.to_string())?;
        self.conn
            .execute(
                "INSERT INTO config_change_events (occurred_at, source, changed_fields_json)
                 VALUES (?1, ?2, ?3)",
                params![occurred_at, source, json],
            )
            .map_err(|e| format!("record config change event: {e}"))?;
        Ok(())
    }

    /// Read persisted Settings events whose timezone-local calendar day is inside `[from, to]`.
    /// The conversion uses the bundled IANA database, so markers behave the same on every OS.
    pub fn config_change_events(
        &self,
        from: &str,
        to: &str,
        timezone: &str,
    ) -> Result<Vec<TimelineConfigEvent>, String> {
        validate_date_range(from, to, "config event window")?;
        if !tare_core::tz::is_valid_zone(timezone) {
            return Err("invalid config event timezone".to_string());
        }
        let mut stmt = self
            .conn
            .prepare(
                "SELECT occurred_at, source, changed_fields_json
                 FROM config_change_events ORDER BY occurred_at, id",
            )
            .map_err(|e| format!("config events prepare: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| format!("config events query: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            let (occurred_at, source, fields_json) =
                row.map_err(|e| format!("config events row: {e}"))?;
            let Some(secs) = tare_core::calendar::parse_iso8601_to_secs(&occurred_at) else {
                continue; // guarded on write; retain forward resilience for hand-edited databases
            };
            let Ok(day) = tare_core::tz::zone_local_date(secs as i128 * 1_000_000_000, timezone)
            else {
                continue;
            };
            if day.as_str() < from || day.as_str() > to {
                continue;
            }
            let mut changed_fields: Vec<String> = serde_json::from_str(&fields_json)
                .map_err(|e| format!("config event fields: {e}"))?;
            changed_fields.sort();
            changed_fields.dedup();
            out.push(TimelineConfigEvent {
                occurred_at,
                day,
                source,
                changed_fields,
            });
        }
        Ok(out)
    }

    /// Per-day cost + outcome counters (commits, accepted edits) over `[from, to]`, for the
    /// cost-effectiveness regression detector. One row per day that has ANY metered
    /// value; days with none are omitted (the detector only ratios days with a positive outcome).
    /// Excludes explicit `query_source='subagent'` rows for the same attribution reason as
    /// [`Store::metered_outcomes`] — unstamped rows are kept.
    pub fn outcomes_by_day(
        &self,
        from: &str,
        to: &str,
    ) -> Result<Vec<tare_core::cost_regression::DayOutcome>, String> {
        validate_date_range(from, to, "daily outcome window")?;
        let mut stmt = self
            .conn
            .prepare(
                "SELECT day,
                    COALESCE(SUM(CASE WHEN metric='cost' THEN value END),0),
                    COALESCE(SUM(CASE WHEN metric='commit' THEN value END),0),
                    COALESCE(SUM(CASE WHEN metric='edit_decision' AND kind='accept' THEN value END),0)
                 FROM metered_series WHERE day BETWEEN ?1 AND ?2 AND query_source != 'subagent'
                 GROUP BY day ORDER BY day",
            )
            .map_err(|e| format!("outcomes_by_day prepare: {e}"))?;
        let rows = stmt
            .query_map(params![from, to], |r| {
                Ok(tare_core::cost_regression::DayOutcome {
                    day: r.get::<_, String>(0)?,
                    cost_micros: r.get::<_, i64>(1)?,
                    commits: r.get::<_, i64>(2)?.max(0) as u64,
                    edits_accepted: r.get::<_, i64>(3)?.max(0) as u64,
                })
            })
            .map_err(|e| format!("outcomes_by_day query: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("outcomes_by_day row: {e}"))
    }

    /// Vendor-reported totals for a day: `(cost_micros, tokens)` from the metered series. Used as a
    /// cross-check against Tare's own step-derived estimate — never added to it.
    pub fn metered_totals(&self, day: &str) -> Result<(i64, u64), String> {
        validate_date_range(day, day, "metered totals day")?;
        let cost: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(value), 0) FROM metered_series WHERE day = ?1 AND metric = 'cost'",
                params![day],
                |r| r.get(0),
            )
            .map_err(|e| format!("metered_totals cost: {e}"))?;
        let tokens: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(value), 0) FROM metered_series WHERE day = ?1 AND metric = 'token'",
                params![day],
                |r| r.get(0),
            )
            .map_err(|e| format!("metered_totals tokens: {e}"))?;
        Ok((cost, tokens.max(0) as u64))
    }

    /// Vendor-reported cost per model for a day (micro-USD), from the metered series — for
    /// estimate-vs-vendor reconciliation. Summed over kind/session; empty model labels
    /// are coalesced to "(unknown)". This is a cross-check lane, never merged into step totals.
    pub fn metered_by_model(&self, day: &str) -> Result<Vec<(String, i64)>, String> {
        validate_date_range(day, day, "metered model day")?;
        let mut stmt = self
            .conn
            .prepare(
                "SELECT CASE WHEN model = '' THEN '(unknown)' ELSE model END AS m,
                        COALESCE(SUM(value), 0)
                 FROM metered_series
                 WHERE day = ?1 AND metric = 'cost'
                 GROUP BY m
                 ORDER BY 2 DESC, m ASC",
            )
            .map_err(|e| format!("metered_by_model prepare: {e}"))?;
        let rows = stmt
            .query_map(params![day], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(|e| format!("metered_by_model query: {e}"))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| format!("metered_by_model row: {e}"))?);
        }
        Ok(out)
    }

    /// Load the durable session-activity rows `(source, session, last_unix, events, last_model)`.
    /// Used to reseed the receiver's in-memory table on boot and for the desktop's DB read path.
    #[allow(clippy::type_complexity)]
    pub fn load_session_activity(&self) -> Result<Vec<(String, String, u64, u64, String)>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT source, session, last_unix, events, COALESCE(last_model, '')
                 FROM session_activity ORDER BY source, session",
            )
            .map_err(|e| format!("load_session_activity prepare: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?.max(0) as u64,
                    r.get::<_, i64>(3)?.max(0) as u64,
                    r.get::<_, String>(4)?,
                ))
            })
            .map_err(|e| format!("load_session_activity query: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("load_session_activity row: {e}"))
    }

    // ---- user-authored run notes: local-only annotations ----

    /// Upsert the note for a run. `tags_json` is a canonical JSON string array (validated upstream).
    /// Length-capped HERE (the single choke point): oversize is a hard error, never truncation.
    pub fn upsert_run_note(
        &self,
        run_id: &str,
        tags_json: &str,
        note_text: &str,
        starred: bool,
        updated_at: &str,
    ) -> Result<(), String> {
        if run_id.is_empty() || run_id.len() > STORED_ID_CAP {
            return Err(format!("run id must be 1-{STORED_ID_CAP} bytes"));
        }
        if note_text.len() > NOTE_TEXT_CAP {
            return Err(format!("note text exceeds {NOTE_TEXT_CAP}-byte cap"));
        }
        if tags_json.len() > TAGS_JSON_CAP {
            return Err(format!("tags exceed {TAGS_JSON_CAP}-byte cap"));
        }
        let tags: Vec<String> = serde_json::from_str(tags_json)
            .map_err(|e| format!("tags must be a JSON array: {e}"))?;
        let mut unique = std::collections::BTreeSet::new();
        if tags.len() > 24
            || tags.iter().any(|tag| {
                tag.is_empty()
                    || tag.len() > 32
                    || !tag
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                    || !unique.insert(tag.as_str())
            })
        {
            return Err("tags must be at most 24 unique 1-32 byte [A-Za-z0-9_-] values".into());
        }
        if updated_at.len() > 64 {
            return Err("note timestamp exceeds 64 bytes".into());
        }
        self.conn
            .execute(
                "INSERT OR REPLACE INTO run_notes (run_id, tags, note_text, starred, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![run_id, tags_json, note_text, starred as i64, updated_at],
            )
            .map(|_| ())
            .map_err(|e| format!("upsert_run_note: {e}"))
    }

    /// Record the vendor's reported cumulative session cost, UPSERTing the LATEST value
    /// per session — Claude Code's statusLine reports a cumulative total on every invocation, so
    /// INSERT-OR-REPLACE (never append) keeps exactly one row per session and can't double-count.
    /// This is a labelled VENDOR cross-check, never merged into the estimate ledger. `updated_at` is
    /// supplied by the clock-owning caller (the core stays clock-free).
    pub fn upsert_vendor_session_cost(
        &self,
        session_id: &str,
        vendor_cost_micros: i64,
        updated_at: &str,
    ) -> Result<(), String> {
        if session_id.is_empty()
            || session_id.len() > STORED_ID_CAP
            || vendor_cost_micros < 0
            || updated_at.len() > 64
        {
            return Err("vendor session cost has an invalid id, amount, or timestamp".into());
        }
        self.conn
            .execute(
                "INSERT OR REPLACE INTO vendor_session_cost (session_id, vendor_cost_micros, updated_at)
                 VALUES (?1, ?2, ?3)",
                params![session_id, vendor_cost_micros, updated_at],
            )
            .map(|_| ())
            .map_err(|e| format!("upsert_vendor_session_cost: {e}"))
    }

    /// All recorded vendor session costs, `(session_id, vendor_cost_micros)`, sorted by
    /// session id for determinism. For the report's estimate-vs-vendor cross-check panel.
    pub fn vendor_session_costs(&self) -> Result<Vec<(String, i64)>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT session_id, vendor_cost_micros FROM vendor_session_cost ORDER BY session_id")
            .map_err(|e| format!("vendor_session_costs: {e}"))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| format!("vendor_session_costs: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("vendor_session_costs: {e}"))
    }

    /// The note for a run, or `None` if never annotated.
    pub fn load_run_note(&self, run_id: &str) -> Result<Option<RunNote>, String> {
        self.conn
            .query_row(
                "SELECT run_id, tags, note_text, starred, updated_at FROM run_notes WHERE run_id = ?1",
                params![run_id],
                |r| {
                    let tags_json: String = r.get(1)?;
                    Ok(RunNote {
                        run_id: r.get(0)?,
                        tags: serde_json::from_str(&tags_json).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                1,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                        note_text: r.get(2)?,
                        starred: r.get::<_, i64>(3)? != 0,
                        updated_at: r.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(|e| format!("load_run_note: {e}"))
    }

    /// Run ids tagged with `tag` (exact membership in the JSON array — matched in Rust, not SQL, so
    /// the tag string can never be an injection or substring-collision vector).
    pub fn notes_by_tag(&self, tag: &str) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT run_id, tags FROM run_notes ORDER BY run_id")
            .map_err(|e| format!("notes_by_tag prepare: {e}"))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| format!("notes_by_tag query: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            let (run_id, tags_json) = row.map_err(|e| format!("notes_by_tag row: {e}"))?;
            let tags: Vec<String> = serde_json::from_str(&tags_json)
                .map_err(|e| format!("notes_by_tag stored tags: {e}"))?;
            if tags.iter().any(|t| t == tag) {
                out.push(run_id);
            }
        }
        Ok(out)
    }

    /// Run ids the user has starred.
    pub fn list_starred(&self) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT run_id FROM run_notes WHERE starred = 1 ORDER BY run_id")
            .map_err(|e| format!("list_starred prepare: {e}"))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| format!("list_starred query: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("list_starred row: {e}"))
    }

    /// Purge a run's note entirely (the delete path).
    pub fn delete_run_note(&self, run_id: &str) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM run_notes WHERE run_id = ?1", params![run_id])
            .map(|_| ())
            .map_err(|e| format!("delete_run_note: {e}"))
    }

    // ---- user-supplied quality scalar: counts-only, never computed ----

    /// Attach (or replace) a run's quality score. `source` is provenance only
    /// (`cli` | `header` | `ci` | `ui`).
    pub fn set_run_quality(
        &self,
        run_id: &str,
        score: i64,
        source: &str,
        updated_at: &str,
    ) -> Result<(), String> {
        if run_id.is_empty()
            || run_id.len() > STORED_ID_CAP
            || !(0..=100).contains(&score)
            || !matches!(source, "cli" | "header" | "ci" | "ui")
            || updated_at.len() > 64
        {
            return Err("quality requires a bounded run id, score 0-100, and known source".into());
        }
        self.conn
            .execute(
                "INSERT OR REPLACE INTO run_quality (run_id, score, source, updated_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![run_id, score, source, updated_at],
            )
            .map(|_| ())
            .map_err(|e| format!("set_run_quality: {e}"))
    }

    /// A single run's quality scalar, or `None` if the user never attached one.
    pub fn run_quality(&self, run_id: &str) -> Result<Option<RunQuality>, String> {
        self.conn
            .query_row(
                "SELECT run_id, score, source, updated_at FROM run_quality WHERE run_id = ?1",
                params![run_id],
                |r| {
                    Ok(RunQuality {
                        run_id: r.get(0)?,
                        score: r.get(1)?,
                        source: r.get(2)?,
                        updated_at: r.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(|e| format!("run_quality: {e}"))
    }

    /// Every run that carries a quality scalar (the frontier y-axis source), run_id-ordered.
    pub fn all_run_quality(&self) -> Result<Vec<RunQuality>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT run_id, score, source, updated_at FROM run_quality ORDER BY run_id")
            .map_err(|e| format!("all_run_quality prepare: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(RunQuality {
                    run_id: r.get(0)?,
                    score: r.get(1)?,
                    source: r.get(2)?,
                    updated_at: r.get(3)?,
                })
            })
            .map_err(|e| format!("all_run_quality query: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("all_run_quality row: {e}"))
    }

    /// Purge a run's quality scalar (the delete path).
    pub fn delete_run_quality(&self, run_id: &str) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM run_quality WHERE run_id = ?1", params![run_id])
            .map(|_| ())
            .map_err(|e| format!("delete_run_quality: {e}"))
    }

    // ---- savings-realization lifecycle: mark accepted, prove realized ----

    /// Mark a savings opportunity accepted on `date`, snapshotting its recoverable estimate.
    pub fn accept_savings(
        &self,
        opportunity_key: &str,
        accepted_date: &str,
        recoverable_micros: i64,
    ) -> Result<(), String> {
        if opportunity_key.is_empty()
            || opportunity_key.len() > STORED_ID_CAP
            || tare_core::calendar::parse_date(accepted_date).is_none()
            || recoverable_micros < 0
        {
            return Err("savings acceptance has an invalid key, date, or negative estimate".into());
        }
        self.conn
            .execute(
                "INSERT OR REPLACE INTO savings_acceptance (opportunity_key, accepted_date, recoverable_micros)
                 VALUES (?1, ?2, ?3)",
                params![opportunity_key, accepted_date, recoverable_micros],
            )
            .map(|_| ())
            .map_err(|e| format!("accept_savings: {e}"))
    }

    /// Every accepted opportunity, key-ordered (deterministic).
    pub fn savings_acceptances(&self) -> Result<Vec<Acceptance>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT opportunity_key, accepted_date, recoverable_micros
                 FROM savings_acceptance ORDER BY opportunity_key",
            )
            .map_err(|e| format!("savings_acceptances prepare: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Acceptance {
                    opportunity_key: r.get(0)?,
                    accepted_date: r.get(1)?,
                    recoverable_micros: r.get(2)?,
                })
            })
            .map_err(|e| format!("savings_acceptances query: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("savings_acceptances row: {e}"))
    }

    /// Un-accept an opportunity (the delete path).
    pub fn unaccept_savings(&self, opportunity_key: &str) -> Result<(), String> {
        self.conn
            .execute(
                "DELETE FROM savings_acceptance WHERE opportunity_key = ?1",
                params![opportunity_key],
            )
            .map(|_| ())
            .map_err(|e| format!("unaccept_savings: {e}"))
    }

    // ---- Savings action lifecycle v2 ----

    /// Persist an apply (`status="applied"`) or dismiss (`status="dismissed"`) action, keyed on
    /// (opportunity_key, canonical cohort_hash). `acted_at` is an RFC3339 UTC instant stamped by the
    /// clock-owning caller (the store is clock-free). Returns a `(status, message)` error: **409** on
    /// a cohort-hash COLLISION (same hash, different canonical spec — we refuse to mutate the wrong
    /// row) or an INCOMPATIBLE in-place transition (applied↔dismissed must unaccept first); 500 on a
    /// store failure. Re-applying the SAME status is idempotent (refreshes the snapshot).
    pub fn put_savings_action(
        &self,
        req: &SavingsActionRequest,
        status: &str,
        acted_at: &str,
    ) -> Result<(), (u16, String)> {
        let invalid = |message: String| (400u16, message);
        if !matches!(status, "applied" | "dismissed") {
            return Err(invalid(format!("invalid savings action status {status:?}")));
        }
        if req.opportunity_key.is_empty() || req.opportunity_key.len() > STORED_ID_CAP {
            return Err(invalid(
                "opportunity_key must be non-empty and bounded".into(),
            ));
        }
        if tare_core::calendar::parse_iso8601_to_secs(acted_at).is_none() {
            return Err(invalid(
                "acted_at must be an RFC3339/ISO-8601 instant".into(),
            ));
        }
        req.cohort
            .validate()
            .map_err(|error| invalid(format!("invalid action cohort: {error}")))?;
        if req.metric != req.cohort.metric
            || req.normalization != req.cohort.normalization
            || req.outcome_denominator != req.cohort.outcome_denominator
        {
            return Err(invalid(
                "action metric, normalization, and outcome denominator must match its cohort"
                    .into(),
            ));
        }
        if let Some(baseline) = &req.baseline {
            baseline
                .cohort
                .validate()
                .map_err(|error| invalid(format!("invalid baseline cohort: {error}")))?;
            if baseline.kind.is_empty() || baseline.kind.len() > 64 || baseline.label.len() > 256 {
                return Err(invalid("baseline kind or label is invalid".into()));
            }
        }
        match &req.match_rule {
            MatchRule::WorkloadKey { key } if key.is_empty() || key.len() > 64 => {
                return Err(invalid("workload match key must be 1-64 bytes".into()));
            }
            MatchRule::TemplateLineage { hash }
                if hash.len() != 16 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
            {
                return Err(invalid(
                    "template lineage must be a 16-digit hexadecimal hash".into(),
                ));
            }
            _ => {}
        }
        let estimates = [
            req.expected_low_micros,
            req.expected_point_micros,
            req.expected_high_micros,
        ];
        if estimates.into_iter().flatten().any(|value| value < 0)
            || req
                .expected_low_micros
                .zip(req.expected_point_micros)
                .is_some_and(|(low, point)| low > point)
            || req
                .expected_point_micros
                .zip(req.expected_high_micros)
                .is_some_and(|(point, high)| point > high)
            || req
                .expected_low_micros
                .zip(req.expected_high_micros)
                .is_some_and(|(low, high)| low > high)
            || req
                .quality_guardrail
                .is_some_and(|score| !(0..=100).contains(&score))
        {
            return Err(invalid(
                "expected savings must be non-negative and ordered; quality must be 0-100".into(),
            ));
        }
        let hash = req.cohort.cohort_hash();
        let cohort_json = req.cohort.canonical_json();
        if let Some((existing_status, existing_json)) = self
            .conn
            .query_row(
                "SELECT status, cohort_json FROM savings_actions WHERE opportunity_key = ?1 AND cohort_hash = ?2",
                params![req.opportunity_key, hash],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|e| (500u16, e.to_string()))?
        {
            if existing_json != cohort_json {
                return Err((
                    409u16,
                    "cohort_hash collision: the stored cohort spec differs from the request — refusing to mutate the wrong action".to_string(),
                ));
            }
            if existing_status != status {
                return Err((
                    409u16,
                    format!("incompatible transition {existing_status} -> {status}: unaccept the action first"),
                ));
            }
        }
        let metric = wire_enum(&req.metric).map_err(|e| (500u16, e))?;
        let normalization = wire_enum(&req.normalization).map_err(|e| (500u16, e))?;
        let baseline_json = match &req.baseline {
            Some(b) => Some(serde_json::to_string(b).map_err(|e| (500u16, e.to_string()))?),
            None => None,
        };
        let match_json =
            serde_json::to_string(&req.match_rule).map_err(|e| (500u16, e.to_string()))?;
        let outcome_json = match &req.outcome_denominator {
            Some(o) => Some(serde_json::to_string(o).map_err(|e| (500u16, e.to_string()))?),
            None => None,
        };
        self.conn
            .execute(
                "INSERT OR REPLACE INTO savings_actions
                 (opportunity_key, cohort_hash, status, acted_at, cohort_json, baseline_json, match_json, metric, normalization, outcome_json, expected_low_micros, expected_point_micros, expected_high_micros, quality_guardrail)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                params![
                    req.opportunity_key, hash, status, acted_at, cohort_json, baseline_json,
                    match_json, metric, normalization, outcome_json,
                    req.expected_low_micros, req.expected_point_micros, req.expected_high_micros,
                    req.quality_guardrail,
                ],
            )
            .map_err(|e| (500u16, format!("put_savings_action: {e}")))?;
        Ok(())
    }

    /// Remove a persisted action by identity (unaccept). **404** when no matching row exists.
    pub fn delete_savings_action(&self, id: &SavingsActionIdentity) -> Result<(), (u16, String)> {
        let n = self
            .conn
            .execute(
                "DELETE FROM savings_actions WHERE opportunity_key = ?1 AND cohort_hash = ?2",
                params![id.opportunity_key, id.cohort_hash],
            )
            .map_err(|e| (500u16, format!("delete_savings_action: {e}")))?;
        if n == 0 {
            return Err((404u16, "no matching savings action to unaccept".to_string()));
        }
        Ok(())
    }

    /// Every persisted savings action, key-ordered (deterministic), each with its computed
    /// compatibility warnings (aggregate-only / legacy rows carry the confounding warning).
    pub fn list_savings_actions(&self) -> Result<Vec<SavingsAction>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT opportunity_key, cohort_hash, status, acted_at, cohort_json, baseline_json, match_json, metric, normalization, outcome_json, expected_low_micros, expected_point_micros, expected_high_micros, quality_guardrail
                 FROM savings_actions ORDER BY opportunity_key, cohort_hash",
            )
            .map_err(|e| format!("list_savings_actions prepare: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, Option<String>>(9)?,
                    r.get::<_, Option<i64>>(10)?,
                    r.get::<_, Option<i64>>(11)?,
                    r.get::<_, Option<i64>>(12)?,
                    r.get::<_, Option<i64>>(13)?,
                ))
            })
            .map_err(|e| format!("list_savings_actions query: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            let (
                opportunity_key,
                cohort_hash,
                status,
                acted_at,
                cohort_json,
                baseline_json,
                match_json,
                metric_s,
                normalization_s,
                outcome_json,
                expected_low_micros,
                expected_point_micros,
                expected_high_micros,
                quality_guardrail,
            ) = row.map_err(|e| format!("list_savings_actions row: {e}"))?;
            let cohort = serde_json::from_str(&cohort_json)
                .map_err(|e| format!("action cohort parse: {e}"))?;
            let match_rule: tare_core::cohort::MatchRule = match match_json {
                Some(j) => {
                    serde_json::from_str(&j).map_err(|e| format!("action match parse: {e}"))?
                }
                None => tare_core::cohort::MatchRule::AggregateOnly,
            };
            let baseline = match baseline_json {
                Some(j) => {
                    Some(serde_json::from_str(&j).map_err(|e| format!("baseline parse: {e}"))?)
                }
                None => None,
            };
            let outcome_denominator = match outcome_json {
                Some(j) => {
                    Some(serde_json::from_str(&j).map_err(|e| format!("outcome parse: {e}"))?)
                }
                None => None,
            };
            let compatibility_warnings = SavingsAction::warnings_for(&match_rule);
            out.push(SavingsAction {
                opportunity_key,
                cohort_hash,
                status,
                acted_at,
                cohort,
                baseline,
                match_rule,
                metric: parse_wire_enum(&metric_s).map_err(|e| format!("action metric: {e}"))?,
                normalization: parse_wire_enum(&normalization_s)
                    .map_err(|e| format!("action normalization: {e}"))?,
                outcome_denominator,
                expected_low_micros,
                expected_point_micros,
                expected_high_micros,
                quality_guardrail,
                compatibility_warnings,
            });
        }
        Ok(out)
    }

    /// One stored action by identity (opportunity_key + cohort_hash), or `None`.
    pub fn get_savings_action(
        &self,
        opportunity_key: &str,
        cohort_hash: &str,
    ) -> Result<Option<SavingsAction>, String> {
        Ok(self
            .list_savings_actions()?
            .into_iter()
            .find(|a| a.opportunity_key == opportunity_key && a.cohort_hash == cohort_hash))
    }

    /// Aggregate the live lifecycle rows for `SavingsLedgerV2`. Applied is expected-point
    /// exposure from `applied` actions only. Observed is the sum of positive, complete action-local
    /// verification results for those same actions. Both sums intentionally retain overlap between
    /// action cohorts; callers must display them separately from capped potential.
    pub fn savings_lifecycle_totals(
        &self,
        pricing: &PricingTable,
    ) -> Result<SavingsLifecycleTotals, String> {
        let mut totals = SavingsLifecycleTotals::default();
        for action in self
            .list_savings_actions()?
            .into_iter()
            .filter(|action| action.status == "applied")
        {
            totals.applied_micros = totals
                .applied_micros
                .saturating_add(action.expected_point_micros.unwrap_or(0));
            let verification = self
                .verify_savings_action(
                    &SavingsVerifyRequest {
                        opportunity_key: action.opportunity_key.clone(),
                        cohort_hash: action.cohort_hash.clone(),
                        as_of_date: None,
                        window_days: None,
                    },
                    pricing,
                )
                .map_err(|(status, message)| {
                    format!(
                        "lifecycle verification for {} / {} failed ({status}): {message}",
                        action.opportunity_key, action.cohort_hash
                    )
                })?;
            if verification.complete
                && verification.status == "observed_reduction"
                && verification.observed_reduction_micros > 0
            {
                totals.observed_micros = totals
                    .observed_micros
                    .saturating_add(verification.observed_reduction_micros);
            }
        }
        Ok(totals)
    }

    /// Cohort-scoped OBSERVED-REDUCTION verification. Deliberately NOT the
    /// global `realization()`: re-resolves the STORED cohort/baseline/match over EQUAL
    /// before/after calendar-date windows around `acted_at`, EXCLUDING the intervention day. With a
    /// baseline it reports the difference-in-differences adjusted reduction; without one, the
    /// unadjusted selection reduction (warned). Stays `verifying` until a complete after window is
    /// available. 404 when no matching action exists.
    ///
    /// The intervention date is the LOCAL calendar date of `acted_at` in the stored cohort timezone
    /// — so the excluded intervention day and the before/after windows are in
    /// the same local frame the cohort itself resolves in. Falls back to the raw `acted_at` date
    /// prefix only if the instant/zone can't be resolved.
    pub fn verify_savings_action(
        &self,
        req: &SavingsVerifyRequest,
        pricing: &PricingTable,
    ) -> Result<SavingsVerifyResult, (u16, String)> {
        if req.opportunity_key.is_empty()
            || req.opportunity_key.len() > STORED_ID_CAP
            || req.cohort_hash.is_empty()
            || req.cohort_hash.len() > 128
            || req
                .as_of_date
                .as_deref()
                .is_some_and(|date| tare_core::calendar::parse_date(date).is_none())
            || req
                .window_days
                .is_some_and(|days| days == 0 || days > 3_660)
        {
            return Err((
                400u16,
                "invalid savings verification identity, date, or window (1-3660 days)".into(),
            ));
        }
        let action = self
            .get_savings_action(&req.opportunity_key, &req.cohort_hash)
            .map_err(|e| (500u16, e))?
            .ok_or((404u16, "no matching savings action to verify".to_string()))?;
        let window_days = req.window_days.unwrap_or(7).max(1) as i64;
        // Intervention day in the cohort's timezone: bucket acted_at's instant by the stored zone,
        // falling back to the raw date prefix if it can't be resolved.
        let acted_days = tare_core::calendar::parse_iso8601_to_secs(&action.acted_at)
            .and_then(|secs| {
                tare_core::tz::zone_local_date(
                    secs as i128 * 1_000_000_000,
                    &action.cohort.timezone,
                )
                .ok()
            })
            .as_deref()
            .and_then(tare_core::calendar::parse_date)
            .or_else(|| {
                action
                    .acted_at
                    .get(0..10)
                    .and_then(tare_core::calendar::parse_date)
            })
            .ok_or((
                500u16,
                format!("unparseable acted_at: {:?}", action.acted_at),
            ))?;
        // Equal windows around the intervention, the intervention day itself EXCLUDED.
        let fmt = tare_core::calendar::format_date;
        let before_from = fmt(acted_days - window_days);
        let before_to = fmt(acted_days - 1);
        let after_from = fmt(acted_days + 1);
        let after_to = fmt(acted_days + window_days);
        // Completeness: the after window is complete once data extends through its end. Default the
        // "now" date to the latest captured day so this is clock-free.
        let as_of = match &req.as_of_date {
            Some(date) => date.clone(),
            None => self
                .run_date_bounds()
                .map_err(|e| (500u16, e))?
                .map(|(_, max)| max)
                .unwrap_or_else(|| before_to.clone()),
        };
        let complete = as_of.as_str() >= after_to.as_str();

        // Resolve every required window to the SAME entity representation used for both spend and
        // match counts. The old implementation totaled the full cohort here and only applied the
        // match rule to a separate count pass, so an unmatched expensive run could manufacture an
        // apparent reduction.
        let sel_before_entities =
            self.resolve_window_entities(&action.cohort, &before_from, &before_to, pricing)?;
        let sel_after_entities =
            self.resolve_window_entities(&action.cohort, &after_from, &after_to, pricing)?;
        let (base_before_entities, base_after_entities) = match &action.baseline {
            Some(b) => (
                Some(self.resolve_window_entities(&b.cohort, &before_from, &before_to, pricing)?),
                Some(self.resolve_window_entities(&b.cohort, &after_from, &after_to, pricing)?),
            ),
            None => (None, None),
        };

        // A workload/template unit is eligible only when that exact stored identity is present in
        // BOTH selection windows and, when a baseline exists, BOTH baseline windows. This is the
        // like-for-like intersection required by the stored match rule. A disappearing workload is
        // excluded from
        // every side instead of being misreported as a reduction to zero.
        let matched_unit_is_common = if matches!(&action.match_rule, MatchRule::AggregateOnly) {
            false
        } else {
            let mut windows: Vec<&[MatchedEntity]> =
                vec![&sel_before_entities, &sel_after_entities];
            if let Some(entities) = &base_before_entities {
                windows.push(entities);
            }
            if let Some(entities) = &base_after_entities {
                windows.push(entities);
            }
            windows.into_iter().all(|entities| {
                entities
                    .iter()
                    .any(|entity| matched_entity_matches_rule(entity, &action.match_rule))
            })
        };

        let sel_before = verification_window_totals(
            &sel_before_entities,
            &action.match_rule,
            matched_unit_is_common,
        );
        let sel_after = verification_window_totals(
            &sel_after_entities,
            &action.match_rule,
            matched_unit_is_common,
        );
        let base_before = base_before_entities.as_ref().map(|entities| {
            verification_window_totals(entities, &action.match_rule, matched_unit_is_common)
        });
        let base_after = base_after_entities.as_ref().map(|entities| {
            verification_window_totals(entities, &action.match_rule, matched_unit_is_common)
        });

        let mut compatibility_warnings = SavingsAction::warnings_for(&action.match_rule);
        if !matches!(&action.match_rule, MatchRule::AggregateOnly) && !matched_unit_is_common {
            let identity = match &action.match_rule {
                MatchRule::WorkloadKey { key } => format!("workload key {key:?}"),
                MatchRule::TemplateLineage { hash } => format!("template lineage {hash}"),
                MatchRule::AggregateOnly => unreachable!(),
            };
            compatibility_warnings.push(format!(
                "{identity} is not present in every required verification window; all non-common units were excluded"
            ));
        }
        let observed_reduction_micros = match (&base_before, &base_after) {
            (Some(bb), Some(ba)) => sel_before
                .micros
                .saturating_sub(sel_after.micros)
                .saturating_add(ba.micros.saturating_sub(bb.micros)),
            _ => {
                compatibility_warnings.push(
                    "no baseline: unadjusted selection reduction may reflect an overall trend, not the intervention".to_string(),
                );
                sel_before.micros.saturating_sub(sel_after.micros)
            }
        };
        // Status: verifying until complete; then observed_reduction iff a positive reduction.
        let status = if !complete {
            "verifying"
        } else if observed_reduction_micros > 0 {
            "observed_reduction"
        } else {
            "not_observed"
        };
        Ok(SavingsVerifyResult {
            status: status.to_string(),
            complete,
            selection_before_micros: sel_before.micros,
            selection_after_micros: sel_after.micros,
            baseline_before_micros: base_before.as_ref().map(|window| window.micros),
            baseline_after_micros: base_after.as_ref().map(|window| window.micros),
            observed_reduction_micros,
            matched_before: sel_before.matched,
            matched_after: sel_after.matched,
            unmatched_before: sel_before.unmatched,
            unmatched_after: sel_after.unmatched,
            baseline_matched_before: base_before.as_ref().map(|window| window.matched),
            baseline_matched_after: base_after.as_ref().map(|window| window.matched),
            baseline_unmatched_before: base_before.as_ref().map(|window| window.unmatched),
            baseline_unmatched_after: base_after.as_ref().map(|window| window.unmatched),
            compatibility_warnings,
        })
    }

    /// Re-resolve a stored cohort restricted to `[from, to]` into the shared matched-entity form.
    /// The window overrides any from/to on the stored spec — the window IS the temporal scope.
    fn resolve_window_entities(
        &self,
        cohort: &tare_core::cohort::CohortSpec,
        from: &str,
        to: &str,
        pricing: &PricingTable,
    ) -> Result<Vec<MatchedEntity>, (u16, String)> {
        let mut spec = cohort.clone();
        spec.from = Some(from.to_string());
        spec.to = Some(to.to_string());
        self.resolve_matched(&spec, pricing)
            .map_err(|e| (500u16, e))
    }

    /// One-time migration of legacy `savings_acceptance` rows into aggregate-only `applied` actions.
    /// Runs when crossing into v23. HONEST: a legacy acceptance has NO historical cohort
    /// snapshot, so it becomes an `aggregate_only` action over an empty aggregate cohort (which
    /// carries the confounding warning on read) — never a faked scope. Idempotent (INSERT OR IGNORE
    /// keyed on the aggregate cohort_hash), so it can't clobber a genuine v2 action.
    fn migrate_legacy_acceptances_if_migrated(&self, from: i64) -> Result<(), String> {
        if from >= 23 {
            return Ok(()); // already on v23+ — nothing to migrate
        }
        let legacy = self.savings_acceptances()?;
        if legacy.is_empty() {
            return Ok(());
        }
        let agg = aggregate_cohort();
        let hash = agg.cohort_hash();
        let cohort_json = agg.canonical_json();
        let match_json = serde_json::to_string(&tare_core::cohort::MatchRule::AggregateOnly)
            .map_err(|e| e.to_string())?;
        for a in legacy {
            // A date-only legacy stamp → the day's UTC midnight instant (honest, deterministic).
            let acted_at = format!("{}T00:00:00Z", a.accepted_date);
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO savings_actions
                     (opportunity_key, cohort_hash, status, acted_at, cohort_json, match_json, metric, normalization, expected_point_micros)
                     VALUES (?1,?2,'applied',?3,?4,?5,'spend_micros','absolute',?6)",
                    params![a.opportunity_key, hash, acted_at, cohort_json, match_json, a.recoverable_micros],
                )
                .map_err(|e| format!("migrate legacy acceptance: {e}"))?;
        }
        Ok(())
    }

    // ---- JSONL backfill dedup ledger ----

    /// Every cross-source dedup key the JSONL lane has already backfilled — the `seen` set that
    /// makes re-scans idempotent.
    pub fn backfilled_keys(&self) -> Result<std::collections::BTreeSet<String>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT dedup_key FROM backfill_seen")
            .map_err(|e| format!("backfilled_keys prepare: {e}"))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| format!("backfilled_keys query: {e}"))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| format!("backfilled_keys row: {e}"))
    }

    /// Record dedup keys as backfilled (idempotent via INSERT OR IGNORE on the PK). One transaction
    /// for the whole batch — not a fsync per row: with synchronous=FULL, a per-row
    /// autocommit fsyncs each insert, so a large backfill paid one disk flush per turn.
    pub fn mark_backfilled(&self, entries: &[(String, String)]) -> Result<(), String> {
        if entries.is_empty() {
            return Ok(());
        }
        if entries.iter().any(|(key, run_id)| {
            key.is_empty() || key.len() > 1024 || run_id.is_empty() || run_id.len() > STORED_ID_CAP
        }) {
            return Err("dedup keys and run ids must be non-empty and bounded".into());
        }
        self.conn
            .execute_batch("BEGIN")
            .map_err(|e| format!("mark_backfilled begin: {e}"))?;
        for (key, run_id) in entries {
            if let Err(e) = self.conn.execute(
                "INSERT OR IGNORE INTO backfill_seen (dedup_key, run_id) VALUES (?1, ?2)",
                params![key, run_id],
            ) {
                let _ = self.conn.execute_batch("ROLLBACK");
                return Err(format!("mark_backfilled: {e}"));
            }
        }
        self.conn
            .execute_batch("COMMIT")
            .map_err(|e| format!("mark_backfilled commit: {e}"))
    }

    // ---- persisted JSONL scan cursor ----

    /// Load every persisted scan-cursor row, to seed the in-memory `ScanCursor` + `TranscriptTailer`
    /// on start so catch-up is incremental (unchanged files skipped, grown files resumed from offset).
    pub fn load_scan_cursor(&self) -> Result<Vec<ScanCursorRow>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, mtime, size, offset FROM scan_cursor")
            .map_err(|e| format!("load_scan_cursor prepare: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ScanCursorRow {
                    path: r.get::<_, String>(0)?,
                    mtime: r.get::<_, i64>(1)?,
                    // sqlite stores these as i64; clamp a stray negative to 0 (sizes/offsets are >= 0).
                    size: r.get::<_, i64>(2)?.max(0) as u64,
                    offset: r.get::<_, i64>(3)?.max(0) as u64,
                })
            })
            .map_err(|e| format!("load_scan_cursor query: {e}"))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| format!("load_scan_cursor row: {e}"))
    }

    /// Upsert scan-cursor rows (one per tailed transcript file). Idempotent replace on the `path` PK,
    /// wrapped in a single transaction so a batch after each rescan sweep commits atomically.
    pub fn save_scan_cursor(&self, rows: &[ScanCursorRow]) -> Result<(), String> {
        if rows.is_empty() {
            return Ok(());
        }
        let prepared: Vec<(&ScanCursorRow, i64, i64)> = rows
            .iter()
            .map(|row| {
                if row.path.is_empty() || row.path.len() > 16 * 1024 || row.offset > row.size {
                    return Err("scan cursor path or offset is invalid".to_string());
                }
                let size = i64::try_from(row.size)
                    .map_err(|_| format!("scan cursor size is too large for {}", row.path))?;
                let offset = i64::try_from(row.offset)
                    .map_err(|_| format!("scan cursor offset is too large for {}", row.path))?;
                Ok((row, size, offset))
            })
            .collect::<Result<_, String>>()?;
        self.conn
            .execute_batch("BEGIN")
            .map_err(|e| format!("save_scan_cursor begin: {e}"))?;
        let res = (|| {
            for (row, size, offset) in &prepared {
                self.conn
                    .execute(
                        "INSERT OR REPLACE INTO scan_cursor (path, mtime, size, offset)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![row.path, row.mtime, size, offset],
                    )
                    .map_err(|e| format!("save_scan_cursor upsert: {e}"))?;
            }
            Ok::<(), String>(())
        })();
        match res {
            Ok(()) => self
                .conn
                .execute_batch("COMMIT")
                .map_err(|e| format!("save_scan_cursor commit: {e}")),
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Drop cursor rows for paths no longer present on disk (deleted transcripts), keeping the table
    /// bounded. `keep` is the current on-disk listing; rows outside it are removed. Returns how many
    /// rows were pruned.
    pub fn prune_scan_cursor(
        &self,
        keep: &std::collections::BTreeSet<String>,
    ) -> Result<usize, String> {
        let existing: Vec<String> = self
            .load_scan_cursor()?
            .into_iter()
            .map(|r| r.path)
            .collect();
        let mut pruned = 0usize;
        for path in existing {
            if !keep.contains(&path) {
                self.conn
                    .execute("DELETE FROM scan_cursor WHERE path = ?1", params![path])
                    .map_err(|e| format!("prune_scan_cursor: {e}"))?;
                pruned += 1;
            }
        }
        Ok(pruned)
    }

    /// Run ids (== Claude Code session ids) that already carry at least one step from a NON-JSONL
    /// source (proxy = NULL, otel, hook). Those sessions were captured live, so the JSONL lane must
    /// NOT re-account them (enforces OTLP > JSONL at the session grain, no double-count).
    pub fn sessions_with_foreign_steps(
        &self,
    ) -> Result<std::collections::BTreeSet<String>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT run_id FROM steps WHERE source IS NULL OR source <> 'jsonl'")
            .map_err(|e| format!("sessions_with_foreign_steps prepare: {e}"))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| format!("sessions_with_foreign_steps query: {e}"))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| format!("sessions_with_foreign_steps row: {e}"))
    }

    /// The highest `step_ordinal` stored for a run, or `None` if it has no steps — so a backfill can
    /// append without colliding with existing ordinals.
    pub fn max_step_ordinal(&self, run_id: &str) -> Result<Option<u32>, String> {
        self.conn
            .query_row(
                "SELECT MAX(step_ordinal) FROM steps WHERE run_id = ?1",
                params![run_id],
                |r| r.get::<_, Option<u32>>(0),
            )
            .map_err(|e| format!("max_step_ordinal: {e}"))
    }

    /// Count steps by capture source (`proxy` | `otel-span` | `otel-event` | NULL -> "unknown").
    /// Counts-only; powers a future "by source" breakdown (frontier vs self-hosted).
    pub fn source_counts(&self) -> Result<std::collections::BTreeMap<String, u64>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT COALESCE(source, 'unknown'), COUNT(*) FROM steps GROUP BY 1")
            .map_err(|e| format!("source_counts prepare: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
            })
            .map_err(|e| format!("source_counts query: {e}"))?;
        let mut out = std::collections::BTreeMap::new();
        for r in rows {
            let (k, v) = r.map_err(|e| format!("source_counts row: {e}"))?;
            out.insert(k, v);
        }
        Ok(out)
    }

    /// Per-capture-source cost-step coverage: `(source, step_count, last_seen_day)`
    /// where `last_seen_day` is the latest `created_date` of a run that source contributed a step
    /// to. Joins steps→runs for the day; NULL source coalesces to "unknown". Clock-free (uses the
    /// stored created_date, never a wall clock). The caller cross-checks this against
    /// `load_session_activity` heartbeat sources to flag "heartbeats but no cost steps" (blind
    /// spend / degraded capture).
    pub fn coverage_by_source(&self) -> Result<Vec<(String, u64, String)>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT COALESCE(s.source, 'unknown') AS src, COUNT(*), COALESCE(MAX(r.created_date), '')
                 FROM steps s JOIN runs r ON r.run_id = s.run_id
                 GROUP BY src ORDER BY 2 DESC, src ASC",
            )
            .map_err(|e| format!("coverage_by_source prepare: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| format!("coverage_by_source query: {e}"))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| format!("coverage_by_source row: {e}"))?);
        }
        Ok(out)
    }

    fn rows_to_runs(
        &self,
        sql: &str,
        args: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<RunRecord>, String> {
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|e| format!("prepare: {e}"))?;
        let iter = stmt
            .query_map(args, |row| {
                let run_id: String = row.get(0)?;
                let step_ordinal: u32 = row.get(1)?;
                let provider: String = row.get(2)?;
                let model: String = row.get(3)?;
                let usage = UsageTokens {
                    fresh_input: nonneg(row.get::<_, i64>(4)?, "fresh_input"),
                    cache_write_5m: nonneg(row.get::<_, i64>(5)?, "cache_write_5m"),
                    cache_write_1h: nonneg(row.get::<_, i64>(6)?, "cache_write_1h"),
                    cache_read: nonneg(row.get::<_, i64>(7)?, "cache_read"),
                    output: nonneg(row.get::<_, i64>(8)?, "output"),
                    reasoning: nonneg(row.get::<_, i64>(9)?, "reasoning"),
                    audio_input: nonneg(row.get::<_, i64>(12)?, "audio_input"),
                    audio_output: nonneg(row.get::<_, i64>(13)?, "audio_output"),
                };
                let shape_json: String = row.get(10)?;
                let stop_reason: Option<String> = row.get(11)?;
                let duration_ms = nonneg(row.get::<_, i64>(14)?, "duration_ms");
                // Nullable timing/span: start_unix_nano is a decimal TEXT string;
                // pre-v21 / proxy / JSONL rows are NULL -> None (step-order-only, no timeline).
                let start_unix_nano: Option<String> = row.get(15)?;
                let trace_id: Option<String> = row.get(16)?;
                let span_id: Option<String> = row.get(17)?;
                let parent_span_id: Option<String> = row.get(18)?;
                Ok((
                    run_id,
                    step_ordinal,
                    provider,
                    model,
                    usage,
                    shape_json,
                    stop_reason,
                    duration_ms,
                    start_unix_nano,
                    trace_id,
                    span_id,
                    parent_span_id,
                ))
            })
            .map_err(|e| format!("query: {e}"))?;

        let mut steps: Vec<StepRecord> = Vec::new();
        for r in iter {
            let (
                run_id,
                step_ordinal,
                provider,
                model,
                usage,
                shape_json,
                stop_reason,
                duration_ms,
                start_unix_nano,
                trace_id,
                span_id,
                parent_span_id,
            ) = r.map_err(|e| format!("row: {e}"))?;
            let shape: RequestShape =
                serde_json::from_str(&shape_json).map_err(|e| format!("shape parse: {e}"))?;
            let provider = provider_from_str(&provider)?;
            if shape.model != model || shape.provider != provider {
                return Err(format!(
                    "stored step {run_id}/{step_ordinal} pricing identity disagrees with shape"
                ));
            }
            steps.push(StepRecord {
                run_id,
                step_ordinal,
                provider,
                model,
                usage,
                shape,
                stop_reason,
                duration_ms,
                // A malformed decimal string (should never happen — we wrote it) degrades to None
                // rather than failing the whole load.
                start_unix_nano: start_unix_nano.as_deref().and_then(UnixNanos::parse),
                trace_id,
                span_id,
                parent_span_id,
            });
        }
        Ok(build_runs(steps))
    }

    /// Reconstruct all runs in insertion order.
    pub fn load_runs(&self) -> Result<Vec<RunRecord>, String> {
        self.rows_to_runs(
            "SELECT s.run_id, s.step_ordinal, s.provider, s.model, s.fresh_input, s.cache_write_5m, s.cache_write_1h,
                    s.cache_read, s.output, s.reasoning, s.shape_json, s.stop_reason, s.audio_input, s.audio_output, s.duration_ms, s.start_unix_nano, s.trace_id, s.span_id, s.parent_span_id
             FROM steps s JOIN runs r ON r.run_id = s.run_id
             ORDER BY r.rowid, s.step_ordinal",
            &[],
        )
    }

    /// Reconstruct only the runs in `ids`, in the SAME insertion order (`r.rowid`) as `load_runs`, so
    /// a candidate subset produces entity rows in byte-identical order to a full scan. Stages `ids`
    /// in a per-connection temp table and JOINs, so ANY candidate-set size loads in one query with no
    /// bound-parameter limit and without ever loading non-candidate runs. This is the core of the
    /// indexed resolve speedup. Safe on the single `!Sync` connection (resolve is single-threaded).
    fn load_runs_in(
        &self,
        ids: &std::collections::BTreeSet<String>,
    ) -> Result<Vec<RunRecord>, String> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.conn
            .execute_batch(
                "CREATE TEMP TABLE IF NOT EXISTS _cohort_candidates (run_id TEXT PRIMARY KEY);
                 DELETE FROM _cohort_candidates;",
            )
            .map_err(|e| e.to_string())?;
        {
            let mut ins = self
                .conn
                .prepare_cached("INSERT OR IGNORE INTO _cohort_candidates (run_id) VALUES (?1)")
                .map_err(|e| e.to_string())?;
            for id in ids {
                ins.execute([id]).map_err(|e| e.to_string())?;
            }
        }
        let runs = self.rows_to_runs(
            "SELECT s.run_id, s.step_ordinal, s.provider, s.model, s.fresh_input, s.cache_write_5m, s.cache_write_1h,
                    s.cache_read, s.output, s.reasoning, s.shape_json, s.stop_reason, s.audio_input, s.audio_output, s.duration_ms, s.start_unix_nano, s.trace_id, s.span_id, s.parent_span_id
             FROM steps s JOIN runs r ON r.run_id = s.run_id
             JOIN _cohort_candidates c ON c.run_id = s.run_id
             ORDER BY r.rowid, s.step_ordinal",
            &[],
        )?;
        self.conn
            .execute("DELETE FROM _cohort_candidates", [])
            .map_err(|e| e.to_string())?;
        Ok(runs)
    }

    /// Map of `run_id` → `created_date` (`YYYY-MM-DD`) for every stored run, so a report
    /// can reprice each run as-of its capture day against a multi-edition pricing table.
    pub fn run_days(&self) -> Result<std::collections::BTreeMap<String, String>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT run_id, created_date FROM runs")
            .map_err(|e| format!("run_days prepare: {e}"))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| format!("run_days query: {e}"))?;
        let mut m = std::collections::BTreeMap::new();
        for row in rows {
            let (id, day) = row.map_err(|e| format!("run_days row: {e}"))?;
            m.insert(id, day);
        }
        Ok(m)
    }

    /// Per-run `(run_id, created_date, created_hour)` for the day×hour punchcard. Runs
    /// with no stored hour yield `None` (the caller drops them — an honest GAP, never a fake bucket).
    /// The caller joins these with each run's priced cost to build the (weekday, hour, micros) grid.
    pub fn run_day_hours(&self) -> Result<Vec<(String, String, Option<u8>)>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT run_id, created_date, created_hour FROM runs")
            .map_err(|e| format!("run_day_hours prepare: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?
                        .and_then(|hour| u8::try_from(hour).ok())
                        .filter(|hour| *hour <= 23),
                ))
            })
            .map_err(|e| format!("run_day_hours query: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("run_day_hours row: {e}"))?);
        }
        Ok(out)
    }

    /// The `n` most recently persisted run ids, newest first (by insertion order). Light query for
    /// the desktop tray/menu recent-runs group — no step reconstruction.
    pub fn recent_run_ids(&self, n: usize) -> Result<Vec<String>, String> {
        let limit = i64::try_from(n).map_err(|_| "recent run limit exceeds i64".to_string())?;
        let mut stmt = self
            .conn
            .prepare("SELECT run_id FROM runs ORDER BY rowid DESC LIMIT ?1")
            .map_err(|e| format!("recent_run_ids prepare: {e}"))?;
        let rows = stmt
            .query_map(params![limit], |r| r.get::<_, String>(0))
            .map_err(|e| format!("recent_run_ids query: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("recent_run_ids row: {e}"))
    }

    /// All run ids, newest first — a light `runs`-table-only query (no step reconstruction), for the
    /// `/__tare/runs` list route (it only needs ids, not every step of every run).
    pub fn all_run_ids(&self) -> Result<Vec<String>, String> {
        // Ascending rowid (newest LAST) — byte-order-compatible with the prior `/__tare/runs`
        // (which mapped load_runs, ASC) and with the desktop list, so web consumers that take
        // `.slice(-N)` for "recent" still get the newest runs (regression fix).
        let mut stmt = self
            .conn
            .prepare("SELECT run_id FROM runs ORDER BY rowid")
            .map_err(|e| format!("all_run_ids prepare: {e}"))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| format!("all_run_ids query: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("all_run_ids row: {e}"))
    }

    /// Reconstruct a single run, filtering in SQL (no load-all-then-find).
    pub fn load_run(&self, run_id: &str) -> Result<Option<RunRecord>, String> {
        let runs = self.rows_to_runs(
            "SELECT s.run_id, s.step_ordinal, s.provider, s.model, s.fresh_input, s.cache_write_5m, s.cache_write_1h,
                    s.cache_read, s.output, s.reasoning, s.shape_json, s.stop_reason, s.audio_input, s.audio_output, s.duration_ms, s.start_unix_nano, s.trace_id, s.span_id, s.parent_span_id
             FROM steps s JOIN runs r ON r.run_id = s.run_id
             WHERE s.run_id = ?1
             ORDER BY s.step_ordinal",
            &[&run_id],
        )?;
        Ok(runs.into_iter().next())
    }

    /// Recorded provenance for one run: the `runs` row (created_date, privacy policy +
    /// profile) plus distinct dimensions read off its steps (models / providers / capture sources /
    /// stop_reasons) and the step count. Makes the otherwise-anonymous flamegraph self-describing.
    /// Returns `None` if the run id is unknown. Counts/labels only — never payload.
    pub fn run_meta(&self, run_id: &str) -> Result<Option<RunMeta>, String> {
        let row = self
            .conn
            .query_row(
                "SELECT created_date, privacy_policy_id, profile FROM runs WHERE run_id = ?1",
                params![run_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| format!("run_meta runs row: {e}"))?;
        let Some((created_date, privacy_policy_id, profile)) = row else {
            return Ok(None);
        };

        let mut stmt = self
            .conn
            .prepare("SELECT model, provider, source, stop_reason FROM steps WHERE run_id = ?1")
            .map_err(|e| format!("run_meta steps prepare: {e}"))?;
        let rows = stmt
            .query_map(params![run_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(|e| format!("run_meta steps query: {e}"))?;

        let mut steps = 0u32;
        let (mut models, mut providers, mut sources, mut stop_reasons) = (
            std::collections::BTreeSet::new(),
            std::collections::BTreeSet::new(),
            std::collections::BTreeSet::new(),
            std::collections::BTreeSet::new(),
        );
        for r in rows {
            let (model, provider, source, stop) = r.map_err(|e| format!("run_meta row: {e}"))?;
            steps = steps.saturating_add(1);
            if !model.is_empty() {
                models.insert(model);
            }
            providers.insert(provider);
            sources.insert(source.unwrap_or_else(|| "unknown".to_string()));
            if let Some(s) = stop {
                stop_reasons.insert(s);
            }
        }
        Ok(Some(RunMeta {
            run_id: run_id.to_string(),
            created_date,
            privacy_policy_id,
            profile,
            steps,
            models: models.into_iter().collect(),
            providers: providers.into_iter().collect(),
            sources: sources.into_iter().collect(),
            stop_reasons: stop_reasons.into_iter().collect(),
        }))
    }

    /// The most-recently-captured steps across all runs, newest first. Ordered by insertion id so
    /// it's a true "as they landed" feed. Counts-only.
    pub fn recent_steps(&self, limit: u32) -> Result<Vec<StepRecord>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT s.run_id, s.step_ordinal, s.provider, s.model, s.fresh_input, s.cache_write_5m,
                        s.cache_write_1h, s.cache_read, s.output, s.reasoning, s.shape_json, s.stop_reason,
                        s.audio_input, s.audio_output, s.duration_ms, s.start_unix_nano,
                        s.trace_id, s.span_id, s.parent_span_id
                 FROM steps s ORDER BY s.id DESC LIMIT ?1",
            )
            .map_err(|e| format!("prepare recent_steps: {e}"))?;
        let rows = stmt
            .query_map([limit], |row| {
                let usage = UsageTokens {
                    fresh_input: nonneg(row.get::<_, i64>(4)?, "fresh_input"),
                    cache_write_5m: nonneg(row.get::<_, i64>(5)?, "cache_write_5m"),
                    cache_write_1h: nonneg(row.get::<_, i64>(6)?, "cache_write_1h"),
                    cache_read: nonneg(row.get::<_, i64>(7)?, "cache_read"),
                    output: nonneg(row.get::<_, i64>(8)?, "output"),
                    reasoning: nonneg(row.get::<_, i64>(9)?, "reasoning"),
                    audio_input: nonneg(row.get::<_, i64>(12)?, "audio_input"),
                    audio_output: nonneg(row.get::<_, i64>(13)?, "audio_output"),
                };
                let run_id: String = row.get(0)?;
                let step_ordinal: u32 = row.get(1)?;
                let provider: String = row.get(2)?;
                let model: String = row.get(3)?;
                let shape_json: String = row.get(10)?;
                let stop_reason: Option<String> = row.get(11)?;
                let duration_ms = nonneg(row.get::<_, i64>(14)?, "duration_ms");
                let start_unix_nano: Option<String> = row.get(15)?;
                let trace_id: Option<String> = row.get(16)?;
                let span_id: Option<String> = row.get(17)?;
                let parent_span_id: Option<String> = row.get(18)?;
                Ok((
                    run_id,
                    step_ordinal,
                    provider,
                    model,
                    usage,
                    shape_json,
                    stop_reason,
                    duration_ms,
                    start_unix_nano,
                    trace_id,
                    span_id,
                    parent_span_id,
                ))
            })
            .map_err(|e| format!("query recent_steps: {e}"))?;
        let mut out = Vec::new();
        for r in rows {
            let (
                run_id,
                step_ordinal,
                provider,
                model,
                usage,
                shape_json,
                stop_reason,
                duration_ms,
                start_unix_nano,
                trace_id,
                span_id,
                parent_span_id,
            ) = r.map_err(|e| format!("row: {e}"))?;
            let shape: RequestShape =
                serde_json::from_str(&shape_json).map_err(|e| format!("shape parse: {e}"))?;
            let provider = provider_from_str(&provider)?;
            if shape.model != model || shape.provider != provider {
                return Err(format!(
                    "stored recent step {run_id}/{step_ordinal} pricing identity disagrees with shape"
                ));
            }
            out.push(StepRecord {
                run_id,
                step_ordinal,
                provider,
                model,
                usage,
                shape,
                stop_reason,
                duration_ms,
                start_unix_nano: start_unix_nano.as_deref().and_then(UnixNanos::parse),
                trace_id,
                span_id,
                parent_span_id,
            });
        }
        Ok(out)
    }

    /// Reconstruct runs created on a given date (YYYY-MM-DD) — used for `report --today`.
    pub fn load_runs_on_date(&self, date: &str) -> Result<Vec<RunRecord>, String> {
        validate_date_range(date, date, "run date")?;
        self.rows_to_runs(
            "SELECT s.run_id, s.step_ordinal, s.provider, s.model, s.fresh_input, s.cache_write_5m, s.cache_write_1h,
                    s.cache_read, s.output, s.reasoning, s.shape_json, s.stop_reason, s.audio_input, s.audio_output, s.duration_ms, s.start_unix_nano, s.trace_id, s.span_id, s.parent_span_id
             FROM steps s JOIN runs r ON r.run_id = s.run_id
             WHERE r.created_date = ?1
             ORDER BY r.rowid, s.step_ordinal",
            &[&date],
        )
    }

    /// Atomically claim an alert key for fire-once delivery: inserts `key` into the
    /// `fired_alerts` set and returns `true` iff this call was the one to insert it (i.e. it had
    /// not fired before). A second tick with the same `(date, series_key, kind)` key returns
    /// `false`, so the daemon delivers each anomaly exactly once.
    pub fn claim_alert(&self, key: &str, fired_date: &str) -> Result<bool, String> {
        let changed = self
            .conn
            .execute(
                "INSERT OR IGNORE INTO fired_alerts (alert_key, fired_date) VALUES (?1, ?2)",
                rusqlite::params![key, fired_date],
            )
            .map_err(|e| format!("claim alert: {e}"))?;
        Ok(changed == 1)
    }

    /// Whether an alert key has already fired (read-only companion to `claim_alert`).
    pub fn alert_already_fired(&self, key: &str) -> Result<bool, String> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM fired_alerts WHERE alert_key = ?1",
                [key],
                |r| r.get(0),
            )
            .map_err(|e| format!("fired lookup: {e}"))?;
        Ok(n > 0)
    }

    /// Min/max `created_date` across all runs (for defaulting a trend window to the data).
    pub fn run_date_bounds(&self) -> Result<Option<(String, String)>, String> {
        let row: (Option<String>, Option<String>) = self
            .conn
            .query_row(
                "SELECT MIN(created_date), MAX(created_date) FROM runs",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| format!("date bounds: {e}"))?;
        Ok(match row {
            (Some(a), Some(b)) => Some((a, b)),
            _ => None,
        })
    }

    /// Reconstruct runs recorded in `[from, to]` (inclusive), each tagged with its date.
    pub fn load_dated_runs_in_range(&self, from: &str, to: &str) -> Result<Vec<DatedRun>, String> {
        validate_date_range(from, to, "run window")?;
        // Both reads below (dates, then steps) must see ONE consistent snapshot: under WAL a
        // concurrent writer (the daemon) committing a run between the two autocommit reads would
        // otherwise land in the steps result but not `date_of`, and the filter_map would silently
        // DROP that run's spend from the trend (review 2026-07-03). A deferred read-transaction
        // pins a single snapshot across both, making the drop unreachable.
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("begin read snapshot: {e}"))?;
        // Map run_id -> created_date for the window (small; one row per run).
        let mut date_of: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT run_id, created_date FROM runs WHERE created_date BETWEEN ?1 AND ?2",
                )
                .map_err(|e| format!("prepare dates: {e}"))?;
            let rows = stmt
                .query_map([from, to], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(|e| format!("query dates: {e}"))?;
            for r in rows {
                let (rid, d) = r.map_err(|e| format!("date row: {e}"))?;
                date_of.insert(rid, d);
            }
        }
        let runs = self.rows_to_runs(
            "SELECT s.run_id, s.step_ordinal, s.provider, s.model, s.fresh_input, s.cache_write_5m, s.cache_write_1h,
                    s.cache_read, s.output, s.reasoning, s.shape_json, s.stop_reason, s.audio_input, s.audio_output, s.duration_ms, s.start_unix_nano, s.trace_id, s.span_id, s.parent_span_id
             FROM steps s JOIN runs r ON r.run_id = s.run_id
             WHERE r.created_date BETWEEN ?1 AND ?2
             ORDER BY r.rowid, s.step_ordinal",
            &[&from, &to],
        )?;
        // Read-only snapshot done; commit ends it (both reads saw the same snapshot, so every run in
        // `runs` is present in `date_of` — the filter_map can't drop a concurrently-written run).
        tx.commit().map_err(|e| format!("end read snapshot: {e}"))?;
        Ok(runs
            .into_iter()
            .filter_map(|run| {
                date_of.get(&run.run_id).map(|date| DatedRun {
                    date: date.clone(),
                    run,
                })
            })
            .collect())
    }

    /// Build a trend over `[from, to]` broken down by `dim`, pricing at view time.
    pub fn trend_in_range(
        &self,
        from: &str,
        to: &str,
        pricing: &PricingTable,
        dim: TrendDimension,
    ) -> Result<TrendReport, String> {
        let dated = self.load_dated_runs_in_range(from, to)?;
        Ok(trend::trend(&dated, from, to, pricing, dim))
    }

    /// Resolve `[from, to]` for a trend/anomaly window: explicit dates win; otherwise default to the
    /// last 14 days ending at the NEWEST captured day, clamped to the data's bounds. `None` = empty
    /// store. The single source of truth for the window rule (the CLI delegates here).
    pub fn resolve_trend_window(
        &self,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Option<(String, String)>, String> {
        if let Some(from) = from {
            validate_date_range(from, from, "trend start")?;
        }
        if let Some(to) = to {
            validate_date_range(to, to, "trend end")?;
        }
        if let (Some(from), Some(to)) = (from, to) {
            validate_date_range(from, to, "trend window")?;
        }
        let Some((min_d, max_d)) = self.run_date_bounds()? else {
            return Ok(None);
        };
        let to = to.map(str::to_string).unwrap_or_else(|| max_d.clone());
        let from = from.map(str::to_string).unwrap_or_else(|| {
            tare_core::calendar::parse_date(&to)
                .map(|d| tare_core::calendar::format_date(d - 13))
                .unwrap_or_else(|| min_d.clone())
        });
        let from = if from < min_d { min_d.clone() } else { from };
        let to = if to > max_d { max_d.clone() } else { to };
        let from = if from > to { to.clone() } else { from };
        Ok(Some((from, to)))
    }

    /// Scoped anomaly explanation. The SHARED orchestrator both transports call:
    /// resolve the window, resolve `req.scope` to its run set BEFORE detection, restrict the dated
    /// runs the trend is built from, then run the pure `tare_core::anomaly::decompose_spikes`. `cause`
    /// and an empty store both yield an empty result (honest, not a guess).
    pub fn anomaly_why(
        &self,
        req: &AnomalyWhyRequest,
        pricing: &PricingTable,
    ) -> Result<Vec<AnomalyWhy>, String> {
        let dim = req.dimension.to_trend();
        if matches!(dim, TrendDimension::ByCause) {
            return Ok(vec![]); // cause isn't a step field → nothing to decompose
        }
        let window = req.window.unwrap_or(7);
        let threshold = req.threshold.unwrap_or(50);
        let Some((from, to)) = self.resolve_trend_window(req.from.as_deref(), req.to.as_deref())?
        else {
            return Ok(vec![]);
        };
        // Resolve the scope to its run set BEFORE detection, then restrict the dated runs — so the
        // whole detect→decompose pipeline sees only the scoped subset (never a post-filter).
        let allow: Option<std::collections::HashSet<String>> = match &req.scope {
            Some(spec) => Some(
                self.resolve_cohort(spec, pricing)?
                    .run_ids
                    .into_iter()
                    .collect(),
            ),
            None => None,
        };
        let mut dated = self.load_dated_runs_in_range(&from, &to)?;
        if let Some(allow) = &allow {
            dated.retain(|dr| allow.contains(&dr.run.run_id));
        }
        let report = trend::trend(&dated, &from, &to, pricing, dim);
        Ok(tare_core::anomaly::decompose_spikes(
            &report, &dated, dim, window, threshold, pricing,
        ))
    }

    /// Hierarchical node-level flame diff between an EXPLICIT run pair.
    /// The SHARED orchestrator both transports call: load each run, build its flamegraph, and merge
    /// via the existing pure `tare_core::flame_diff::flame_diff` (no reimplementation). `normalize`
    /// selects share-mode (structural) diffing. A missing run is a clear error, never a silent empty
    /// tree. Distinct from the row-level report diff behind `/__tare/diff`.
    pub fn flame_diff(
        &self,
        run_a: &str,
        run_b: &str,
        normalize: bool,
        pricing: &PricingTable,
    ) -> Result<tare_core::flame_diff::FlameDiffModel, String> {
        let a = self
            .load_run(run_a)?
            .ok_or_else(|| format!("run `{run_a}` not found"))?;
        let b = self
            .load_run(run_b)?
            .ok_or_else(|| format!("run `{run_b}` not found"))?;
        let fa = tare_core::flamegraph::build_flamegraph(&a, pricing);
        let fb = tare_core::flamegraph::build_flamegraph(&b, pricing);
        Ok(tare_core::flame_diff::flame_diff(&fa, &fb, normalize))
    }

    /// Offline counterfactual cost experiment over a cohort. The SHARED
    /// orchestrator both transports call: resolve the cohort to its run set, optionally gate those
    /// runs by their INGESTED quality scalar, then run the existing pure grid engine
    /// (`tare_core::experiment::run_experiment`) — repricing stored usage vectors, NEVER re-executing
    /// or reading payload. Complements the read-only captured `frontier`; this executes the grid.
    pub fn experiment(
        &self,
        req: &tare_core::experiment::ExperimentRequest,
        pricing: &PricingTable,
    ) -> Result<tare_core::experiment::ExperimentResult, String> {
        let resolved = self.resolve_cohort(&req.cohort, pricing)?;
        let want: std::collections::HashSet<String> = resolved.run_ids.into_iter().collect();
        let mut runs: Vec<RunRecord> = self
            .load_runs()?
            .into_iter()
            .filter(|r| want.contains(&r.run_id))
            .collect();
        // Quality gate (honest): keep only runs whose ingested quality scalar admits the constraint.
        // A run without a score can't be asserted to meet the bar, so it's excluded when gated.
        if let Some(qc) = &req.quality_constraint {
            let scores: std::collections::HashMap<String, i64> = self
                .all_run_quality()?
                .into_iter()
                .map(|q| (q.run_id, q.score))
                .collect();
            runs.retain(|r| scores.get(&r.run_id).is_some_and(|s| qc.admits(*s)));
        }
        if let Some(constraint) = &req.quality_constraint {
            constraint.validate()?;
        }
        tare_core::experiment::run_experiment(&runs, pricing, &req.experiment)
    }

    // ---- Saved investigations ----
    //
    // The durable, cross-transport store for named investigations. The full DTO (state-minus-focus,
    // columns, pane widths, loss marker) is persisted opaquely in `state_json`; only id/label/version/
    // timestamps are also columns (for listing + ordering). The store never inspects `state`, so it
    // can't accidentally persist transient focus — that omission is the client's contract, and
    // the store stores exactly the payload it's given.

    /// List every saved investigation as its full DTO JSON, most-recently-updated first (ties by id
    /// for determinism).
    pub fn list_investigations(&self) -> Result<Vec<serde_json::Value>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT state_json FROM saved_investigations ORDER BY updated_at DESC, id ASC")
            .map_err(|e| format!("prepare list_investigations: {e}"))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| format!("query investigations: {e}"))?;
        let mut out = Vec::new();
        for r in rows {
            let s = r.map_err(|e| format!("investigation row: {e}"))?;
            out.push(serde_json::from_str(&s).map_err(|e| format!("investigation parse: {e}"))?);
        }
        Ok(out)
    }

    /// Upsert one investigation from its full DTO JSON. Validates the required fields, enforces
    /// the 256 KiB per-item and 1 MiB total caps, and stores the whole DTO in `state_json`. Keyed on
    /// `id`, so re-saving the same investigation replaces it.
    pub fn upsert_investigation(&self, dto: &serde_json::Value) -> Result<(), String> {
        const KEYS: &[&str] = &[
            "id",
            "label",
            "version",
            "state",
            "columns",
            "pane_widths",
            "created_at",
            "updated_at",
        ];
        let object = dto
            .as_object()
            .ok_or("investigation must be a JSON object")?;
        let json = serde_json::to_string(dto).map_err(|e| e.to_string())?;
        if json.len() > INVESTIGATION_MAX_BYTES {
            return Err(format!(
                "investigation exceeds {} KiB",
                INVESTIGATION_MAX_BYTES / 1024
            ));
        }
        if let Some(key) = object.keys().find(|key| !KEYS.contains(&key.as_str())) {
            return Err(format!("investigation has unknown top-level field {key:?}"));
        }
        let field = |k: &str| dto.get(k).and_then(|v| v.as_str());
        let id = field("id")
            .filter(|s| !s.is_empty() && s.len() <= 128)
            .ok_or("investigation missing id")?;
        let label = field("label")
            .filter(|label| !label.trim().is_empty() && label.len() <= 256)
            .ok_or("investigation missing or invalid label")?;
        let version = dto
            .get("version")
            .and_then(|v| v.as_i64())
            .ok_or("investigation missing version")?;
        if version != 2 {
            return Err(format!("unsupported investigation version {version}"));
        }
        let state = dto
            .get("state")
            .and_then(serde_json::Value::as_object)
            .ok_or("investigation missing state object")?;
        if state.contains_key("focus") {
            return Err("investigation state must not persist transient focus".into());
        }
        let created_at = field("created_at")
            .filter(|value| tare_core::calendar::parse_iso8601_to_secs(value).is_some())
            .ok_or("investigation missing or invalid created_at")?;
        let updated_at = field("updated_at")
            .filter(|value| tare_core::calendar::parse_iso8601_to_secs(value).is_some())
            .ok_or("investigation missing or invalid updated_at")?;
        if let Some(columns) = dto.get("columns") {
            let columns = columns
                .as_object()
                .ok_or("investigation columns must be an object")?;
            if columns.len() > 64
                || columns.iter().any(|(name, values)| {
                    name.len() > 64
                        || values.as_array().is_none_or(|values| {
                            values.len() > 128
                                || values.iter().any(|value| {
                                    value.as_str().is_none_or(|value| value.len() > 128)
                                })
                        })
                })
            {
                return Err("investigation columns are oversized or malformed".into());
            }
        }
        if let Some(widths) = dto.get("pane_widths") {
            let widths = widths
                .as_object()
                .ok_or("investigation pane_widths must be an object")?;
            if widths.len() > 32
                || widths.iter().any(|(name, width)| {
                    name.len() > 64
                        || width.as_f64().is_none_or(|width| {
                            !width.is_finite() || !(0.0..=10_000.0).contains(&width)
                        })
                })
            {
                return Err("investigation pane widths are oversized or invalid".into());
            }
        }
        // Total-cap guard: current total, minus this id's existing size (if updating), plus the new
        // size — so an in-place update is measured correctly, not double-counted.
        let projected: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(state_json)), 0)
                   - COALESCE((SELECT LENGTH(state_json) FROM saved_investigations WHERE id = ?1), 0)
                 FROM saved_investigations",
                params![id],
                |r| r.get(0),
            )
            .map_err(|e| format!("investigations total: {e}"))?;
        let projected = usize::try_from(projected.max(0)).unwrap_or(usize::MAX);
        if projected.saturating_add(json.len()) > INVESTIGATIONS_TOTAL_MAX_BYTES {
            return Err(format!(
                "saved investigations exceed {} MiB total",
                INVESTIGATIONS_TOTAL_MAX_BYTES / (1024 * 1024)
            ));
        }
        self.conn
            .execute(
                "INSERT OR REPLACE INTO saved_investigations(id, label, version, state_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, label, version, json, created_at, updated_at],
            )
            .map_err(|e| format!("upsert investigation: {e}"))?;
        Ok(())
    }

    /// Delete a saved investigation by id (idempotent).
    pub fn delete_investigation(&self, id: &str) -> Result<(), String> {
        if id.is_empty() || id.len() > 128 {
            return Err("investigation id must be 1-128 bytes".into());
        }
        self.conn
            .execute(
                "DELETE FROM saved_investigations WHERE id = ?1",
                params![id],
            )
            .map_err(|e| format!("delete investigation: {e}"))?;
        Ok(())
    }

    /// Today's spend aggregation for the tray readout.
    pub fn today_spend(&self, date: &str, pricing: &PricingTable) -> Result<TodaySpend, String> {
        let runs = self.load_runs_on_date(date)?;
        Ok(attribute::today_spend(&runs, pricing))
    }
}

// ---- Cohort resolve engine ----

/// The value a step presents for a given dimension. Provider/Model read step
/// columns; Source reads the steps.source column (passed in); the rest read the shape, with the
/// agent-vocabulary aliases resolved here (`agent -> parent_label`, `tool -> component_label`).
/// `RunDate` is run-scoped and `CacheClass` is a per-class narrowing — both handled separately, so
/// they return `None` here. `WorkloadKey` is the user-provided grouping label.
fn resolve_dim_value(
    step: &StepRecord,
    source: Option<&str>,
    dim: CohortDimension,
) -> Option<String> {
    let s = &step.shape;
    match dim {
        CohortDimension::Provider => Some(step.provider.as_str().to_string()),
        CohortDimension::Model => Some(step.model.clone()),
        CohortDimension::Source => source.map(|x| x.to_string()),
        CohortDimension::Session => s.session.clone(),
        CohortDimension::Parent | CohortDimension::Agent => s.parent_label.clone(),
        CohortDimension::Component | CohortDimension::Tool => s.component_label.clone(),
        CohortDimension::StepLabel => s.step_label.clone(),
        CohortDimension::Effort => s.effort.clone(),
        CohortDimension::McpServer => s.mcp_server.clone(),
        CohortDimension::Commit => s.commit.clone(),
        CohortDimension::Author => s.author.clone(),
        CohortDimension::Template => s.system_hash.map(|h| format!("{h:016x}")),
        CohortDimension::Ttl => Some(
            match s.ttl {
                CacheTtl::FiveMin => "5m",
                CacheTtl::OneHour => "1h",
            }
            .to_string(),
        ),
        CohortDimension::StopReason => step.stop_reason.clone(),
        CohortDimension::WorkloadKey => s.workload_key.clone(),
        CohortDimension::RunDate => None, // run-scoped, handled in the run loop
        CohortDimension::CacheClass => None, // per-class narrowing, handled specially
    }
}

/// Whether a step carries any tokens in the named cache class. CacheClass narrows attribution
/// buckets inside matched steps — here at step-presence granularity; per-bucket micros narrowing is
/// a documented refinement).
fn step_has_cache_class(step: &StepRecord, class: &str) -> bool {
    let u = &step.usage;
    match class {
        "fresh" => u.fresh_input > 0,
        "cache_write_5m" => u.cache_write_5m > 0,
        "cache_write_1h" => u.cache_write_1h > 0,
        "cache_read" => u.cache_read > 0,
        "output" => u.output > 0,
        "reasoning" => u.reasoning > 0,
        _ => false,
    }
}

/// Step-scoped filters narrow steps: dimension Eq/In (except RunDate, which is run-scoped) and
/// StepRefs.
fn filter_is_step_scoped(f: &CohortFilter) -> bool {
    match f {
        CohortFilter::Eq { dimension, .. } | CohortFilter::In { dimension, .. } => {
            *dimension != CohortDimension::RunDate
        }
        CohortFilter::StepRefs { .. } => true,
        _ => false,
    }
}

/// The bucket date for a run in IANA zone `tz`. When any step carries an
/// authoritative instant (`start_unix_nano`), bucket by the zone-local date of the EARLIEST such
/// instant (the run's start), honoring DST. Otherwise fall back to the immutable `created_date` —
/// a legacy row whose true instant is unknown — and report `used_legacy = true` so the response
/// provenance can add the `legacy_date_bucket` assumption. Never re-buckets an instant-less row.
fn run_bucket_date(
    run: &RunRecord,
    created_date: Option<&str>,
    tz: &str,
) -> (Option<String>, bool) {
    let instant = run
        .steps
        .iter()
        .filter_map(|s| s.start_unix_nano)
        .map(|n| n.0)
        .min();
    match instant {
        // Defensive: tz is pre-validated and instants are real, but on any conversion error keep the
        // immutable capture date rather than dropping the run.
        Some(n) => match tare_core::tz::zone_local_date(n as i128, tz) {
            Ok(d) => (Some(d), false),
            Err(_) => (created_date.map(String::from), true),
        },
        None => (created_date.map(String::from), true),
    }
}

/// The `step_dimensions.dimension` value that indexes a step-scoped Eq/In filter, or `None` when the
/// dimension is not materialized in that index. Mirrors `resolve_dim_value`'s aliasing exactly
/// (Agent->parent, Tool->component) so an index lookup selects the same steps the scan would. The
/// `None` dimensions are resolved elsewhere: Provider/Model/Source are `steps` columns (see
/// `candidate_steps_for_filter`); CacheClass/StopReason are derived/unindexed (no narrowing); RunDate
/// is run-scoped.
fn index_dim_name(dim: CohortDimension) -> Option<&'static str> {
    Some(match dim {
        CohortDimension::Session => "session",
        CohortDimension::Parent | CohortDimension::Agent => "parent",
        CohortDimension::Component | CohortDimension::Tool => "component",
        CohortDimension::StepLabel => "step",
        CohortDimension::Effort => "effort",
        CohortDimension::McpServer => "mcp_server",
        CohortDimension::Commit => "commit",
        CohortDimension::Author => "author",
        CohortDimension::Template => "template",
        CohortDimension::Ttl => "ttl",
        CohortDimension::WorkloadKey => "workload_key",
        _ => return None,
    })
}

/// The `steps` column that indexes a step-scoped Eq/In filter directly (Provider/Model/Source are
/// stored on `steps`, not `step_dimensions`), or `None`. Column names are literals (no injection).
fn steps_column_for(dim: CohortDimension) -> Option<&'static str> {
    match dim {
        CohortDimension::Provider => Some("provider"),
        CohortDimension::Model => Some("model"),
        CohortDimension::Source => Some("source"),
        _ => None,
    }
}

/// Run-scoped filters include/exclude owning runs: RunIds, Tag, QualityRange, and RunDate Eq/In.
fn filter_is_run_scoped(f: &CohortFilter) -> bool {
    match f {
        CohortFilter::RunIds { .. }
        | CohortFilter::Tag { .. }
        | CohortFilter::QualityRange { .. } => true,
        CohortFilter::Eq { dimension, .. } | CohortFilter::In { dimension, .. } => {
            *dimension == CohortDimension::RunDate
        }
        _ => false,
    }
}

fn step_passes(
    step: &StepRecord,
    source: Option<&str>,
    run_id: &str,
    ordinal: u32,
    f: &CohortFilter,
) -> bool {
    match f {
        CohortFilter::Eq { dimension, value } => {
            if *dimension == CohortDimension::CacheClass {
                step_has_cache_class(step, value)
            } else {
                resolve_dim_value(step, source, *dimension).as_deref() == Some(value.as_str())
            }
        }
        CohortFilter::In { dimension, values } => {
            if *dimension == CohortDimension::CacheClass {
                values.iter().any(|v| step_has_cache_class(step, v))
            } else {
                resolve_dim_value(step, source, *dimension).is_some_and(|v| values.contains(&v))
            }
        }
        CohortFilter::StepRefs { refs } => refs
            .iter()
            .any(|r| r.run_id == run_id && r.step_ordinal == ordinal),
        _ => true, // run-scoped / threshold filters are not step-scoped
    }
}

fn run_passes_run_scoped(
    run_id: &str,
    date: Option<&str>,
    quality: Option<i64>,
    tags: &[String],
    f: &CohortFilter,
) -> bool {
    match f {
        CohortFilter::RunIds { ids } => ids.iter().any(|i| i == run_id),
        CohortFilter::Tag { value } => tags.iter().any(|t| t == value),
        CohortFilter::QualityRange { min, max } => {
            quality.is_some_and(|q| min.is_none_or(|m| q >= m) && max.is_none_or(|x| q <= x))
        }
        CohortFilter::Eq {
            dimension: CohortDimension::RunDate,
            value,
        } => date == Some(value.as_str()),
        CohortFilter::In {
            dimension: CohortDimension::RunDate,
            values,
        } => date.is_some_and(|d| values.iter().any(|v| v == d)),
        _ => true,
    }
}

fn threshold_ok(micros: i64, f: &CohortFilter) -> bool {
    match f {
        CohortFilter::GteMicros { value } => micros >= *value,
        CohortFilter::LteMicros { value } => micros <= *value,
        _ => true,
    }
}

/// One matched step, carrying enough to price it and read ANY dimension value later (facets read a
/// dimension that need not be a filter). Holds a cloned `StepRecord` + its `source` column.
struct MatchedStep {
    step: StepRecord,
    source: Option<String>,
    micros: i64,
}

/// A resolved entity, represented by its matched steps — the grouping IS the entity boundary.
/// At Run grain one entity per matched run (`steps` = its matched steps); at Step
/// grain one entity per matched step (`steps` = [it]). Facets count entities and read step values
/// off `steps`. `run_id` and `whole_micros` (the entity's FULL spend: whole-run at Run grain, the
/// step's own micros at Step grain) let `resolve_cohort` map to `CohortResolveResult` without
/// re-deriving them.
struct MatchedEntity {
    run_id: String,
    whole_micros: i64,
    steps: Vec<MatchedStep>,
    /// The run's bucket date (perf): the same value `resolve_matched_over_runs`
    /// already computes per run via `run_bucket_date`. Lets `timeline_cohort` bucket ONE whole-window
    /// resolve by day instead of re-resolving per day. `None` for a run with no assignable bucket
    /// (already excluded from any bounded date window by the filter above, so it never populates a
    /// day bucket either — consistent with the prior per-day-resolve behavior).
    bucket_date: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
struct TimelineRawValue {
    additive: f64,
    attributed_tokens: u64,
    cache_read: u64,
    cache_input: u64,
    support: u32,
}

/// Spend and entity-count denominators from one verification window. `micros` and `matched` are
/// deliberately derived from the same filtered entity iterator.
struct VerificationWindowTotals {
    micros: i64,
    matched: u64,
    unmatched: u64,
}

/// Whether a resolved, scoped entity carries the stored matching identity. Reading only the
/// entity's already-matched steps keeps step-scoped cohort filters intact; the previous whole-run
/// reload could match on a step outside the saved scope.
fn matched_entity_matches_rule(entity: &MatchedEntity, rule: &MatchRule) -> bool {
    match rule {
        MatchRule::AggregateOnly => false,
        MatchRule::WorkloadKey { key } => entity
            .steps
            .iter()
            .any(|s| s.step.shape.workload_key.as_deref() == Some(key.as_str())),
        MatchRule::TemplateLineage { hash } => entity.steps.iter().any(|s| {
            s.step
                .shape
                .system_hash
                .map(|value| format!("{value:016x}"))
                .as_deref()
                == Some(hash.as_str())
        }),
    }
}

/// Calculate spend and counts over exactly the same matched set. Aggregate-only intentionally keeps
/// its legacy full-cohort arithmetic and zero matched count, with the caller's confounding warning;
/// it is warning-only compatibility behavior, not a fabricated entity match.
fn verification_window_totals(
    entities: &[MatchedEntity],
    rule: &MatchRule,
    matched_unit_is_common: bool,
) -> VerificationWindowTotals {
    if matches!(rule, MatchRule::AggregateOnly) {
        return VerificationWindowTotals {
            micros: entities
                .iter()
                .flat_map(|entity| &entity.steps)
                .map(|step| step.micros)
                .fold(0i64, i64::saturating_add),
            matched: 0,
            unmatched: entities.len() as u64,
        };
    }

    let matched_entities: Vec<&MatchedEntity> = if matched_unit_is_common {
        entities
            .iter()
            .filter(|entity| matched_entity_matches_rule(entity, rule))
            .collect()
    } else {
        Vec::new()
    };
    VerificationWindowTotals {
        micros: matched_entities
            .iter()
            .flat_map(|entity| &entity.steps)
            .map(|step| step.micros)
            .fold(0i64, i64::saturating_add),
        matched: matched_entities.len() as u64,
        unmatched: (entities.len() as u64).saturating_sub(matched_entities.len() as u64),
    }
}

/// One cohort's per-value facet tallies over its matched entities.
struct FacetAgg {
    /// value -> SUPPORT (# entities carrying it; an entity counts once even if multi-valued).
    support: std::collections::BTreeMap<String, i64>,
    /// value -> SPEND (Σ step micros whose value == this; step-level, so additive across values).
    micros: std::collections::BTreeMap<String, i64>,
    total_entities: i64,
    /// Entities with NO value for the dimension — reported separately, never folded into a value.
    missing: i64,
    /// Σ matched step micros (denominator for spend share; steps with no value still count here).
    total_micros: i64,
}

impl FacetAgg {
    fn support_share_pct(&self, value: &str) -> f64 {
        let non_missing = self.total_entities - self.missing;
        if non_missing <= 0 {
            0.0
        } else {
            *self.support.get(value).unwrap_or(&0) as f64 / non_missing as f64 * 100.0
        }
    }
    fn spend_share_pct(&self, value: &str) -> f64 {
        if self.total_micros <= 0 {
            0.0
        } else {
            *self.micros.get(value).unwrap_or(&0) as f64 / self.total_micros as f64 * 100.0
        }
    }
    fn missing_pct(&self) -> f64 {
        if self.total_entities <= 0 {
            0.0
        } else {
            self.missing as f64 / self.total_entities as f64 * 100.0
        }
    }
}

/// Aggregate matched entities into a `DayAgg` for the compare decomposition: `steps` is the ENTITY
/// count (volume is the entity-count change), while `tokens`/`micros` sum over the
/// matched steps.
fn dayagg_of(entities: &[MatchedEntity]) -> tare_core::anomaly::DayAgg {
    let mut tokens = 0u64;
    let mut micros = 0i64;
    for e in entities {
        for s in &e.steps {
            tokens = tokens.saturating_add(s.step.usage.total());
            micros = micros.saturating_add(s.micros);
        }
    }
    tare_core::anomaly::DayAgg {
        steps: entities.len() as u64,
        tokens,
        micros,
    }
}

/// Compatibility warnings for a comparison: pricing-mode, attribution-fidelity,
/// match-rule coverage, and user-supplied quality provenance. `aggregate_only` ALWAYS warns
/// (confounding); partial workload/template matches report matched/total counts for both sides.
fn compare_warnings(
    selection: &CohortSpec,
    baseline: &CohortSpec,
    match_rule: &MatchRule,
    sel_ents: &[MatchedEntity],
    base_ents: &[MatchedEntity],
    quality: &[RunQuality],
) -> Vec<String> {
    let mut w = Vec::new();
    if selection.pricing != baseline.pricing {
        w.push("pricing mode differs between cohorts — cost deltas mix pricing bases.".to_string());
    }
    // Unpriced usage in only one cohort = a fidelity/coverage mismatch worth flagging.
    let has_unpriced = |ents: &[MatchedEntity]| {
        ents.iter().any(|e| {
            e.steps
                .iter()
                .any(|s| s.micros == 0 && s.step.usage.total() > 0)
        })
    };
    if has_unpriced(sel_ents) != has_unpriced(base_ents) {
        w.push("attribution fidelity differs — one cohort contains unpriced usage.".to_string());
    }
    match match_rule {
        MatchRule::AggregateOnly => w.push(
            "aggregate-only match — differences may be confounded; observed association, not causation."
                .to_string(),
        ),
        MatchRule::WorkloadKey { key } => {
            // Workload keys are captured. Report partial coverage on BOTH
            // sides: a complete selection must not hide an incompatible baseline.
            let matched = |entities: &[MatchedEntity]| {
                entities
                    .iter()
                    .filter(|e| {
                        e.steps.iter().any(|s| {
                            s.step.shape.workload_key.as_deref() == Some(key.as_str())
                        })
                    })
                    .count()
            };
            let selection_matched = matched(sel_ents);
            if selection_matched < sel_ents.len() {
                w.push(format!(
                    "workload-key match on {key:?}: {selection_matched} of {} selection entities carry the key.",
                    sel_ents.len()
                ));
            }
            let baseline_matched = matched(base_ents);
            if baseline_matched < base_ents.len() {
                w.push(format!(
                    "workload-key match on {key:?}: {baseline_matched} of {} baseline entities carry the key.",
                    base_ents.len()
                ));
            }
        }
        MatchRule::TemplateLineage { hash } => {
            let matched = |entities: &[MatchedEntity]| {
                entities
                    .iter()
                    .filter(|e| {
                        e.steps.iter().any(|s| {
                            s.step
                                .shape
                                .system_hash
                                .map(|h| format!("{h:016x}"))
                                .as_deref()
                                == Some(hash.as_str())
                        })
                    })
                    .count()
            };
            let selection_matched = matched(sel_ents);
            if selection_matched < sel_ents.len() {
                w.push(format!(
                    "template-lineage match on {hash}: {selection_matched} of {} selection entities carry the lineage hash.",
                    sel_ents.len()
                ));
            }
            let baseline_matched = matched(base_ents);
            if baseline_matched < base_ents.len() {
                w.push(format!(
                    "template-lineage match on {hash}: {baseline_matched} of {} baseline entities carry the lineage hash.",
                    base_ents.len()
                ));
            }
        }
    }

    // Quality is user-supplied counts-only evidence. Compare only whether each unique resolved run
    // carries a value and the recorded source labels; never compare, infer, or grade the scores.
    let quality_by: std::collections::HashMap<&str, &str> = quality
        .iter()
        .map(|q| (q.run_id.as_str(), q.source.as_str()))
        .collect();
    let quality_profile = |entities: &[MatchedEntity]| {
        let runs: std::collections::BTreeSet<&str> =
            entities.iter().map(|e| e.run_id.as_str()).collect();
        let covered = runs
            .iter()
            .filter(|run_id| quality_by.contains_key(**run_id))
            .count();
        let sources: std::collections::BTreeSet<&str> = runs
            .iter()
            .filter_map(|run_id| quality_by.get(*run_id).copied())
            .collect();
        (covered, runs.len(), sources)
    };
    let (selection_quality, selection_runs, selection_sources) = quality_profile(sel_ents);
    let (baseline_quality, baseline_runs, baseline_sources) = quality_profile(base_ents);
    let coverage_differs = selection_quality * baseline_runs != baseline_quality * selection_runs;
    if coverage_differs || selection_sources != baseline_sources {
        let source_list = |sources: &std::collections::BTreeSet<&str>| {
            if sources.is_empty() {
                "none".to_string()
            } else {
                sources.iter().copied().collect::<Vec<_>>().join(", ")
            }
        };
        w.push(format!(
            "quality provenance differs — selection has user-supplied quality for {selection_quality} of {selection_runs} runs (sources: {}); baseline has {baseline_quality} of {baseline_runs} runs (sources: {}). Scores were not inferred or graded.",
            source_list(&selection_sources),
            source_list(&baseline_sources)
        ));
    }
    w
}

/// Does a matched entity match `query` on any requested search field? IDs/hashes
/// match exactly or by PREFIX; labels/tags/notes/model/config use case-insensitive SUBSTRING. Reads
/// ONLY opaque IDs, allow-listed shape labels, hashes, user tags/notes, model, and config knobs —
/// never prompt/response payload (there is none in the counts store; the field set has no payload
/// option). `qlow` is the pre-lowercased query for substring fields.
fn search_entity_matches(
    entity: &MatchedEntity,
    run_id: &str,
    note: Option<&RunNote>,
    query: &str,
    qlow: &str,
    fields: &[SearchField],
) -> bool {
    for field in fields {
        match field {
            SearchField::Id => {
                if run_id == query || run_id.starts_with(query) {
                    return true;
                }
                for s in &entity.steps {
                    let ord = s.step.step_ordinal.to_string();
                    if ord == query || ord.starts_with(query) {
                        return true;
                    }
                }
            }
            SearchField::Hash => {
                for s in &entity.steps {
                    if let Some(h) = s.step.shape.system_hash {
                        let hex = format!("{h:016x}");
                        if hex == query || hex.starts_with(query) {
                            return true;
                        }
                    }
                }
            }
            SearchField::Label => {
                for s in &entity.steps {
                    let sh = &s.step.shape;
                    for v in [
                        &sh.session,
                        &sh.parent_label,
                        &sh.component_label,
                        &sh.step_label,
                        &sh.mcp_server,
                        &sh.vendor,
                        &sh.commit,
                        &sh.author,
                        &sh.workload_key,
                    ]
                    .into_iter()
                    .flatten()
                    {
                        if v.to_lowercase().contains(qlow) {
                            return true;
                        }
                    }
                }
            }
            SearchField::Tag => {
                if let Some(n) = note {
                    if n.tags.iter().any(|t| t.to_lowercase().contains(qlow)) {
                        return true;
                    }
                }
            }
            SearchField::Note => {
                if let Some(n) = note {
                    if n.note_text.to_lowercase().contains(qlow) {
                        return true;
                    }
                }
            }
            SearchField::Model => {
                for s in &entity.steps {
                    if s.step.model.to_lowercase().contains(qlow) {
                        return true;
                    }
                }
            }
            SearchField::Config => {
                for s in &entity.steps {
                    if let Some(ef) = &s.step.shape.effort {
                        if ef.to_lowercase().contains(qlow) {
                            return true;
                        }
                    }
                    let ttl = match s.step.shape.ttl {
                        CacheTtl::FiveMin => "5m",
                        CacheTtl::OneHour => "1h",
                    };
                    if ttl.contains(qlow) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Tally support (entity-level, deduped/multi-valued) and spend (step-level) per value for `dim`.
fn facet_aggregate(entities: &[MatchedEntity], dim: CohortDimension) -> FacetAgg {
    let mut agg = FacetAgg {
        support: std::collections::BTreeMap::new(),
        micros: std::collections::BTreeMap::new(),
        total_entities: 0,
        missing: 0,
        total_micros: 0,
    };
    for e in entities {
        agg.total_entities = agg.total_entities.saturating_add(1);
        let vals = Store::entity_values(e, dim);
        if vals.is_empty() {
            agg.missing = agg.missing.saturating_add(1);
        } else {
            for v in &vals {
                let support = agg.support.entry(v.clone()).or_insert(0);
                *support = support.saturating_add(1);
            }
        }
        for ms in &e.steps {
            agg.total_micros = agg.total_micros.saturating_add(ms.micros);
            if let Some(v) = resolve_dim_value(&ms.step, ms.source.as_deref(), dim) {
                let micros = agg.micros.entry(v).or_insert(0);
                *micros = micros.saturating_add(ms.micros);
            }
        }
    }
    agg
}

fn timeline_unit(metric: CohortMetric, normalization: Normalization) -> TimelineUnit {
    match (metric, normalization) {
        (CohortMetric::SpendMicros, Normalization::Absolute) => TimelineUnit::EstimatedMicroUsd,
        (CohortMetric::Tokens, Normalization::Absolute) => TimelineUnit::Tokens,
        (CohortMetric::CacheHitRate, Normalization::Absolute)
        | (_, Normalization::ShareOfSelection) => TimelineUnit::Percent,
        (CohortMetric::SpendMicros, Normalization::PerRun) => TimelineUnit::EstimatedMicroUsdPerRun,
        (CohortMetric::Tokens, Normalization::PerRun) => TimelineUnit::TokensPerRun,
        (CohortMetric::SpendMicros, Normalization::PerOutcome) => {
            TimelineUnit::EstimatedMicroUsdPerOutcome
        }
        (CohortMetric::Tokens, Normalization::PerOutcome) => TimelineUnit::TokensPerOutcome,
        // CohortSpec::validate rejects every cache-rate normalization except Absolute.
        (CohortMetric::CacheHitRate, _) => TimelineUnit::Percent,
    }
}

fn timeline_add_step(raw: &mut TimelineRawValue, step: &MatchedStep, metric: CohortMetric) {
    match metric {
        CohortMetric::SpendMicros => raw.additive += step.micros as f64,
        CohortMetric::Tokens => raw.additive += step.step.usage.total() as f64,
        CohortMetric::CacheHitRate => {
            let u = &step.step.usage;
            raw.cache_read = raw.cache_read.saturating_add(u.cache_read);
            raw.cache_input = raw.cache_input.saturating_add(
                u.fresh_input
                    .saturating_add(u.cache_write_5m)
                    .saturating_add(u.cache_write_1h)
                    .saturating_add(u.cache_read),
            );
        }
    }
}

fn timeline_scoped_runs(entities: &[MatchedEntity]) -> Vec<RunRecord> {
    let mut by_run: std::collections::BTreeMap<String, Vec<StepRecord>> =
        std::collections::BTreeMap::new();
    for entity in entities {
        let steps = by_run.entry(entity.run_id.clone()).or_default();
        for matched in &entity.steps {
            if !steps
                .iter()
                .any(|step| step.step_ordinal == matched.step.step_ordinal)
            {
                steps.push(matched.step.clone());
            }
        }
    }
    by_run
        .into_iter()
        .map(|(run_id, mut steps)| {
            steps.sort_by_key(|step| step.step_ordinal);
            RunRecord { run_id, steps }
        })
        .collect()
}

fn timeline_cause_rows(
    entities: &[MatchedEntity],
    pricing: &PricingTable,
    mode: &PricingMode,
    day: &str,
) -> std::collections::BTreeMap<String, TimelineRawValue> {
    let report_for = |runs: &[RunRecord]| {
        let dates: std::collections::BTreeMap<String, String> = match mode {
            PricingMode::EffectiveDated => runs
                .iter()
                .map(|run| (run.run_id.clone(), day.to_string()))
                .collect(),
            PricingMode::AsOf { date } => runs
                .iter()
                .map(|run| (run.run_id.clone(), date.clone()))
                .collect(),
            PricingMode::Latest => std::collections::BTreeMap::new(),
        };
        attribute::build_report_dated(runs, pricing, &dates)
    };
    let runs = timeline_scoped_runs(entities);
    let report = report_for(&runs);
    let mut out = std::collections::BTreeMap::<String, TimelineRawValue>::new();
    for row in report.rows {
        let entry = out.entry(row.cause).or_default();
        entry.additive = row.micros as f64;
        entry.attributed_tokens = row.tokens;
    }
    // Cause support is selected RUN count. Cause detection (especially retry-loop) needs the
    // selected steps in their run context, so step-grain entities are recombined by run first.
    for run in &runs {
        for row in report_for(std::slice::from_ref(run)).rows {
            let support = &mut out.entry(row.cause).or_default().support;
            *support = support.saturating_add(1);
        }
    }
    out
}

/// Map resolved entities (from `resolve_matched`/`resolve_matched_over_runs`) into the
/// `CohortResolveResult` wire DTO: one `entity_rows` summary per entity in entity order,
/// distinct sorted `run_ids`, sorted `step_refs` at Step grain, and scoped totals. Byte-identical to
/// the former inline construction in `resolve_cohort` — `matched_micros = Σ step micros`,
/// `whole_entity_micros = entity.whole_micros`. The resolve_* + drift-guard tests are the gate.
fn matched_to_result(spec: &CohortSpec, ents: &[MatchedEntity]) -> CohortResolveResult {
    let mut entity_rows: Vec<CohortEntitySummary> = Vec::with_capacity(ents.len());
    let mut run_ids_set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut step_refs: Vec<StepRef> = Vec::new();
    let mut total_micros = 0i64;
    let mut step_count = 0u32;

    for e in ents {
        let matched_micros = e
            .steps
            .iter()
            .map(|s| s.micros)
            .fold(0i64, i64::saturating_add);
        let step_ordinal = match spec.entity {
            CohortEntity::Step => e.steps.first().map(|s| s.step.step_ordinal),
            CohortEntity::Run => None,
        };
        entity_rows.push(CohortEntitySummary {
            entity: EntityRef {
                run_id: e.run_id.clone(),
                step_ordinal,
            },
            matched_micros,
            whole_entity_micros: e.whole_micros,
            matched_step_count: u32::try_from(e.steps.len()).unwrap_or(u32::MAX),
        });
        run_ids_set.insert(e.run_id.clone());
        if matches!(spec.entity, CohortEntity::Step) {
            if let Some(s) = e.steps.first() {
                step_refs.push(StepRef {
                    run_id: e.run_id.clone(),
                    step_ordinal: s.step.step_ordinal,
                });
            }
        }
        total_micros = total_micros.saturating_add(matched_micros);
        step_count = step_count.saturating_add(u32::try_from(e.steps.len()).unwrap_or(u32::MAX));
    }

    step_refs.sort_by(|a, b| {
        a.run_id
            .cmp(&b.run_id)
            .then(a.step_ordinal.cmp(&b.step_ordinal))
    });
    let run_ids: Vec<String> = run_ids_set.into_iter().collect();
    CohortResolveResult {
        cohort_id: None,
        run_count: u32::try_from(run_ids.len()).unwrap_or(u32::MAX),
        step_count,
        total_micros,
        run_ids,
        step_refs: matches!(spec.entity, CohortEntity::Step).then_some(step_refs),
        entity_rows,
    }
}

impl Store {
    /// Shared resolution core: apply run-scoped + step-scoped filters + the
    /// date window + entity thresholds, returning grain-aware matched entities. `resolve_cohort`
    /// shapes these into a `CohortResolveResult`; `facet_cohort` reads dimension values off the
    /// matched steps. This full-corpus path favors correctness; `resolve_cohort` applies indexed
    /// candidate narrowing before calling the shared predicate engine.
    fn resolve_matched(
        &self,
        spec: &CohortSpec,
        pricing: &PricingTable,
    ) -> Result<Vec<MatchedEntity>, String> {
        spec.validate().map_err(|e| e.to_string())?;
        let runs = self.load_runs()?;
        self.resolve_matched_over_runs(spec, pricing, &runs)
    }

    /// The shared resolution core over an explicit run set: group each run's matching
    /// steps into entities, applying the date window, run-scoped predicates, per-step predicates, and
    /// entity-grain thresholds. Callers pass either the full corpus (`resolve_matched`) or the indexed
    /// candidate subset (`resolve_cohort`); the entity set is identical for a superset input
    /// because every predicate is applied here. Spec is assumed pre-validated.
    fn resolve_matched_over_runs(
        &self,
        spec: &CohortSpec,
        pricing: &PricingTable,
        runs: &[RunRecord],
    ) -> Result<Vec<MatchedEntity>, String> {
        let dates = self.run_days()?;
        let quality_by: std::collections::HashMap<String, i64> = self
            .all_run_quality()?
            .into_iter()
            .map(|q| (q.run_id, q.score))
            .collect();
        // Per-step source (steps.source is not carried on StepRecord).
        let source_by: std::collections::HashMap<(String, u32), String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT run_id, step_ordinal, source FROM steps WHERE source IS NOT NULL")
                .map_err(|e| format!("resolve source prepare: {e}"))?;
            let mapped = stmt
                .query_map([], |r| {
                    Ok((
                        (r.get::<_, String>(0)?, r.get::<_, u32>(1)?),
                        r.get::<_, String>(2)?,
                    ))
                })
                .map_err(|e| format!("resolve source query: {e}"))?;
            mapped
                .collect::<Result<_, _>>()
                .map_err(|e| format!("resolve source row: {e}"))?
        };
        let has_tag_filters = spec
            .filters
            .iter()
            .any(|f| matches!(f, CohortFilter::Tag { .. }));

        let step_scoped: Vec<&CohortFilter> = spec
            .filters
            .iter()
            .filter(|f| filter_is_step_scoped(f))
            .collect();
        let run_scoped: Vec<&CohortFilter> = spec
            .filters
            .iter()
            .filter(|f| filter_is_run_scoped(f))
            .collect();
        let thresholds: Vec<&CohortFilter> = spec
            .filters
            .iter()
            .filter(|f| {
                matches!(
                    f,
                    CohortFilter::GteMicros { .. } | CohortFilter::LteMicros { .. }
                )
            })
            .collect();

        let mut out: Vec<MatchedEntity> = Vec::new();

        for run in runs {
            // Bucket by the zone-local date of the run's earliest instant when
            // any step carries `start_unix_nano`; otherwise retain the immutable `created_date`
            // (legacy_date_bucket). The bucket drives the date window, RunDate filter, and
            // effective-dated pricing so a stamped run and its window/timeline always agree.
            let created = dates.get(&run.run_id).map(String::as_str);
            let (bucket, _legacy) = run_bucket_date(run, created, &spec.timezone);
            let date = bucket.as_deref();
            if let Some(f) = &spec.from {
                if date.is_none_or(|d| d < f.as_str()) {
                    continue;
                }
            }
            if let Some(t) = &spec.to {
                if date.is_none_or(|d| d > t.as_str()) {
                    continue;
                }
            }
            let tags: Vec<String> = if has_tag_filters {
                self.load_run_note(&run.run_id)?
                    .map(|n| n.tags)
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let quality = quality_by.get(&run.run_id).copied();
            if !run_scoped
                .iter()
                .all(|f| run_passes_run_scoped(&run.run_id, date, quality, &tags, f))
            {
                continue;
            }

            let on = match &spec.pricing {
                PricingMode::EffectiveDated => date.map(|d| d.to_string()),
                PricingMode::AsOf { date } => Some(date.clone()),
                PricingMode::Latest => None,
            };
            let on_ref = on.as_deref();
            let whole_run: i64 = run
                .steps
                .iter()
                .map(|s| attribute::step_micros(s, pricing, on_ref))
                .fold(0i64, i64::saturating_add);

            let matched: Vec<MatchedStep> = run
                .steps
                .iter()
                .filter(|step| {
                    let src = source_by
                        .get(&(run.run_id.clone(), step.step_ordinal))
                        .map(String::as_str);
                    step_scoped
                        .iter()
                        .all(|f| step_passes(step, src, &run.run_id, step.step_ordinal, f))
                })
                .map(|step| MatchedStep {
                    step: step.clone(),
                    source: source_by
                        .get(&(run.run_id.clone(), step.step_ordinal))
                        .cloned(),
                    micros: attribute::step_micros(step, pricing, on_ref),
                })
                .collect();

            match spec.entity {
                CohortEntity::Run => {
                    // Threshold applies to the RUN entity's own (whole) total.
                    if !thresholds.iter().all(|f| threshold_ok(whole_run, f)) {
                        continue;
                    }
                    // A run with step-scoped filters but no matching steps contributes nothing.
                    if matched.is_empty() && !step_scoped.is_empty() {
                        continue;
                    }
                    out.push(MatchedEntity {
                        run_id: run.run_id.clone(),
                        whole_micros: whole_run,
                        steps: matched,
                        bucket_date: bucket.clone(),
                    });
                }
                CohortEntity::Step => {
                    for ms in matched {
                        if !thresholds.iter().all(|f| threshold_ok(ms.micros, f)) {
                            continue;
                        }
                        // At Step grain the entity's whole spend IS the step's own micros.
                        let whole = ms.micros;
                        out.push(MatchedEntity {
                            run_id: run.run_id.clone(),
                            whole_micros: whole,
                            steps: vec![ms],
                            bucket_date: bucket.clone(),
                        });
                    }
                }
            }
        }
        Ok(out)
    }

    /// The faceted value(s) an entity carries for `dim`, deduped. At Run grain a run can span
    /// several models/sessions, so the dimension can be multi-valued. Empty means the entity is
    /// MISSING this dimension.
    fn entity_values(entity: &MatchedEntity, dim: CohortDimension) -> Vec<String> {
        let mut vals: Vec<String> = entity
            .steps
            .iter()
            .filter_map(|ms| resolve_dim_value(&ms.step, ms.source.as_deref(), dim))
            .collect();
        vals.sort();
        vals.dedup();
        vals
    }

    /// Dense daily timeline over the exact resolved cohort. Every metric and
    /// grouping consumes `resolve_matched`; no legacy/global trend value can enter the numerator.
    /// External metered outcomes are used only for an unfiltered total cohort, the one case where
    /// their day-level scope is exact. Otherwise the point is null with an explicit reason.
    pub fn timeline_cohort(
        &self,
        req: &CohortTimelineRequest,
        pricing: &PricingTable,
        units: &[tare_core::workunit::WorkUnit],
    ) -> Result<CohortTimelineResult, String> {
        req.cohort.validate().map_err(|e| e.to_string())?;
        let (Some(from), Some(to)) = (&req.cohort.from, &req.cohort.to) else {
            return Err("timeline cohort requires bounded from and to dates".to_string());
        };
        let days = tare_core::calendar::days_between(from, to);
        if days.is_empty() {
            return Err("timeline cohort date range is invalid or empty".to_string());
        }
        if days.len() > 366 {
            return Err("timeline cohort range exceeds 366 days".to_string());
        }

        // Resolve the WHOLE window exactly once (perf): a per-day re-resolve
        // here used to repeat the full-corpus load + source scan up to 366x. Every entity already
        // carries its run's `bucket_date`, so the per-day split below is a plain in-memory grouping
        // of this one resolve, not a second (or 366th) database pass. Byte-identical to the old
        // per-day resolve: the date-window filter inside `resolve_matched_over_runs` is the same
        // filter that populated `bucket_date`, so a day's bucket here is exactly the entity set a
        // `from=to=day` resolve would have returned.
        let all_entities = self.resolve_matched(&req.cohort, pricing)?;
        let run_count = u32::try_from(
            all_entities
                .iter()
                .map(|entity| entity.run_id.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
        )
        .unwrap_or(u32::MAX);
        let mut by_day: std::collections::HashMap<String, Vec<MatchedEntity>> =
            std::collections::HashMap::new();
        for entity in all_entities {
            if let Some(d) = entity.bucket_date.clone() {
                by_day.entry(d).or_default().push(entity);
            }
        }
        let no_entities: Vec<MatchedEntity> = Vec::new();

        let unit = timeline_unit(req.cohort.metric, req.cohort.normalization);
        let mut daily = Vec::<std::collections::BTreeMap<String, TimelineRawValue>>::new();
        let mut daily_totals = Vec::<f64>::new();
        let mut daily_outcomes = Vec::<Result<f64, String>>::new();
        let mut keys = std::collections::BTreeSet::<String>::new();

        for day in &days {
            let entities: &[MatchedEntity] =
                by_day.get(day).map(Vec::as_slice).unwrap_or(&no_entities);
            let mut rows = std::collections::BTreeMap::<String, TimelineRawValue>::new();
            let mut total = TimelineRawValue {
                support: u32::try_from(entities.len()).unwrap_or(u32::MAX),
                ..Default::default()
            };
            for entity in entities {
                for step in &entity.steps {
                    timeline_add_step(&mut total, step, req.cohort.metric);
                }
            }
            let selection_total = match req.cohort.metric {
                CohortMetric::SpendMicros | CohortMetric::Tokens => total.additive,
                CohortMetric::CacheHitRate => total.cache_input as f64,
            };

            match req.group {
                TimelineGroup::Total => {
                    rows.insert("total".to_string(), total);
                }
                TimelineGroup::Provider | TimelineGroup::Model => {
                    let dimension = if req.group == TimelineGroup::Provider {
                        CohortDimension::Provider
                    } else {
                        CohortDimension::Model
                    };
                    for entity in entities {
                        for value in Self::entity_values(entity, dimension) {
                            let support = &mut rows.entry(value).or_default().support;
                            *support = support.saturating_add(1);
                        }
                        for step in &entity.steps {
                            if let Some(value) =
                                resolve_dim_value(&step.step, step.source.as_deref(), dimension)
                            {
                                timeline_add_step(
                                    rows.entry(value).or_default(),
                                    step,
                                    req.cohort.metric,
                                );
                            }
                        }
                    }
                }
                TimelineGroup::Cause if req.cohort.metric == CohortMetric::CacheHitRate => {
                    // A cause row has attributed tokens/spend, not a cache-read numerator paired to
                    // the same cause. Keep one dense unavailable series instead of inventing it.
                    rows.insert("cause".to_string(), TimelineRawValue::default());
                }
                TimelineGroup::Cause => {
                    let mut causes =
                        timeline_cause_rows(entities, pricing, &req.cohort.pricing, day);
                    if req.cohort.metric == CohortMetric::Tokens {
                        for raw in causes.values_mut() {
                            raw.additive = raw.attributed_tokens as f64;
                        }
                    }
                    rows = causes;
                }
            }
            keys.extend(rows.keys().cloned());
            daily_outcomes.push(self.timeline_outcome_denominator(
                &req.cohort,
                req.group,
                entities,
                day,
                pricing,
                units,
            ));
            daily_totals.push(selection_total);
            daily.push(rows);
        }

        let mut series = Vec::new();
        for key in keys {
            let mut points = Vec::with_capacity(days.len());
            for (index, day) in days.iter().enumerate() {
                let raw = daily[index].get(&key).copied().unwrap_or_default();
                let (value, denominator, unavailable_reason) = if req.group == TimelineGroup::Cause
                    && req.cohort.metric == CohortMetric::CacheHitRate
                {
                    (
                        None,
                        None,
                        Some(
                            "cache hit rate by cause is unavailable: attribution rows do not carry an authoritative cache numerator and denominator"
                                .to_string(),
                        ),
                    )
                } else {
                    match req.cohort.normalization {
                        Normalization::Absolute if req.cohort.metric == CohortMetric::CacheHitRate => {
                            if raw.cache_input > 0 {
                                (
                                    Some(raw.cache_read as f64 / raw.cache_input as f64 * 100.0),
                                    Some(raw.cache_input as f64),
                                    None,
                                )
                            } else {
                                (
                                    None,
                                    None,
                                    Some("cache input denominator is unavailable for this day and series".to_string()),
                                )
                            }
                        }
                        Normalization::Absolute => (Some(raw.additive), None, None),
                        Normalization::PerRun => {
                            if raw.support > 0 {
                                (
                                    Some(raw.additive / raw.support as f64),
                                    Some(raw.support as f64),
                                    None,
                                )
                            } else {
                                (
                                    None,
                                    None,
                                    Some("per-run denominator is unavailable: no selected entities carry this series".to_string()),
                                )
                            }
                        }
                        Normalization::ShareOfSelection => {
                            let denom = daily_totals[index];
                            if denom > 0.0 {
                                (Some(raw.additive / denom * 100.0), Some(denom), None)
                            } else {
                                (
                                    None,
                                    None,
                                    Some("share denominator is unavailable: the selected daily total is zero".to_string()),
                                )
                            }
                        }
                        Normalization::PerOutcome => match &daily_outcomes[index] {
                            Ok(denom) if *denom > 0.0 => {
                                (Some(raw.additive / *denom), Some(*denom), None)
                            }
                            Ok(_) => (
                                None,
                                None,
                                Some("outcome denominator is unavailable: the selected daily count is zero".to_string()),
                            ),
                            Err(reason) => (None, None, Some(reason.clone())),
                        },
                    }
                };
                points.push(TimelinePoint {
                    day: day.clone(),
                    value,
                    support_count: raw.support,
                    denominator,
                    unavailable_reason,
                });
            }
            series.push(TimelineSeries { key, points });
        }
        series.sort_by(|a, b| {
            let total = |row: &TimelineSeries| {
                row.points
                    .iter()
                    .filter_map(|point| point.value)
                    .sum::<f64>()
            };
            total(b)
                .partial_cmp(&total(a))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.key.cmp(&b.key))
        });
        let config_events = self.config_change_events(from, to, &req.cohort.timezone)?;
        Ok(CohortTimelineResult {
            days,
            run_count,
            unit,
            series,
            config_events,
        })
    }

    fn timeline_outcome_denominator(
        &self,
        spec: &CohortSpec,
        group: TimelineGroup,
        entities: &[MatchedEntity],
        day: &str,
        pricing: &PricingTable,
        units: &[tare_core::workunit::WorkUnit],
    ) -> Result<f64, String> {
        if spec.normalization != Normalization::PerOutcome {
            return Ok(0.0);
        }
        let Some(outcome) = &spec.outcome_denominator else {
            return Err("outcome denominator is missing".to_string());
        };
        let run_ids: std::collections::BTreeSet<String> = entities
            .iter()
            .map(|entity| entity.run_id.clone())
            .collect();
        match outcome {
            OutcomeDenominator::WorkUnit { name } => {
                if !units.iter().any(|unit| unit.name == *name) {
                    return Err(format!(
                        "work-unit denominator is unavailable: no configured unit named {name:?}"
                    ));
                }
                let runs = self.load_runs_in(&run_ids)?;
                let report = tare_core::workunit::unit_report(&runs, pricing, units);
                Ok(report
                    .rows
                    .iter()
                    .find(|row| row.name == *name)
                    .map(|row| row.runs as f64)
                    .unwrap_or(0.0))
            }
            OutcomeDenominator::Metered {
                kind: MeteredOutcome::SuccessfulRuns,
            } => {
                let runs = self.load_runs_in(&run_ids)?;
                Ok(tare_core::session::run_outcome_split(&runs, pricing).successful_runs as f64)
            }
            OutcomeDenominator::Metered { kind } => {
                if !spec.filters.is_empty() {
                    return Err(
                        "metered outcome denominator is unavailable for a filtered cohort; global totals are never substituted"
                            .to_string(),
                    );
                }
                if group != TimelineGroup::Total {
                    return Err(
                        "metered outcome denominator is unavailable for grouped series; a global total is never substituted"
                            .to_string(),
                    );
                }
                let counts = self.metered_outcomes(day, day)?;
                Ok(match kind {
                    MeteredOutcome::PullRequests => counts.pull_requests as f64,
                    MeteredOutcome::Commits => counts.commits as f64,
                    MeteredOutcome::LinesAddedPer1k => counts.lines_added as f64 / 1_000.0,
                    MeteredOutcome::ActiveHours => counts.active_seconds as f64 / 3_600.0,
                    MeteredOutcome::Sessions => counts.sessions as f64,
                    MeteredOutcome::SuccessfulRuns => unreachable!(),
                })
            }
        }
    }

    /// Selection-vs-baseline facet over one dimension. SUPPORT is an
    /// entity count (multi-valued at Run grain, so support shares need not sum to 100%); SPEND
    /// share is step-level micros / cohort micros; MISSING is reported separately (never an
    /// `unlabeled` value). `lift_ratio` is omitted when baseline support share is 0 (use
    /// `delta_support_share_points`). Rows are ranked by selection support desc (down-ranking
    /// high-cardinality unique IDs), ties broken by value for determinism.
    pub fn facet_cohort(
        &self,
        selection: &CohortSpec,
        baseline: &CohortSpec,
        dimension: CohortDimension,
        pricing: &PricingTable,
    ) -> Result<CohortFacetResult, String> {
        let sel = facet_aggregate(&self.resolve_matched(selection, pricing)?, dimension);
        let base = facet_aggregate(&self.resolve_matched(baseline, pricing)?, dimension);

        let values: std::collections::BTreeSet<String> = sel
            .support
            .keys()
            .chain(base.support.keys())
            .cloned()
            .collect();
        let mut rows: Vec<FacetRow> = values
            .into_iter()
            .map(|v| {
                let sel_share = sel.support_share_pct(&v);
                let base_share = base.support_share_pct(&v);
                FacetRow {
                    selection_support: *sel.support.get(&v).unwrap_or(&0),
                    baseline_support: *base.support.get(&v).unwrap_or(&0),
                    selection_micros: *sel.micros.get(&v).unwrap_or(&0),
                    baseline_micros: *base.micros.get(&v).unwrap_or(&0),
                    selection_support_share_pct: sel_share,
                    baseline_support_share_pct: base_share,
                    selection_spend_share_pct: sel.spend_share_pct(&v),
                    baseline_spend_share_pct: base.spend_share_pct(&v),
                    delta_support_share_points: sel_share - base_share,
                    // Undefined when the baseline never carries the value: use delta points instead.
                    lift_ratio: (base_share > 0.0).then(|| sel_share / base_share),
                    selection_missing_pct: sel.missing_pct(),
                    baseline_missing_pct: base.missing_pct(),
                    value: v,
                }
            })
            .collect();
        rows.sort_by(|a, b| {
            b.selection_support
                .cmp(&a.selection_support)
                .then(a.value.cmp(&b.value))
        });
        Ok(CohortFacetResult { dimension, rows })
    }

    /// Compare a selection cohort against a baseline. Returns both resolved
    /// summaries, compatibility warnings, and a volume/size/efficiency decomposition that reuses the
    /// deterministic anomaly arithmetic (`decompose_change`), so the three deltas sum EXACTLY to
    /// `total_delta_micros`.
    pub fn compare_cohort(
        &self,
        selection: &CohortSpec,
        baseline: &CohortSpec,
        match_rule: &MatchRule,
        pricing: &PricingTable,
    ) -> Result<CohortCompareResult, String> {
        // One resolve per cohort (perf): previously resolved each cohort
        // twice — once via resolve_cohort (candidate-narrowed, for the DTO) and once via
        // resolve_matched (full-scan, for the aggregates) — 4 resolves for 2 cohorts. Both the DTO
        // and the aggregates are derived from the same entity set below.
        let sel_ents = self.resolve_cohort_entities(selection, pricing)?;
        let base_ents = self.resolve_cohort_entities(baseline, pricing)?;
        let selection_res = matched_to_result(selection, &sel_ents);
        let baseline_res = matched_to_result(baseline, &base_ents);

        let sel_agg = dayagg_of(&sel_ents);
        let base_agg = dayagg_of(&base_ents);
        let quality = self.all_run_quality()?;
        let why = tare_core::anomaly::decompose_change("cohort", sel_agg, base_agg);
        let total_delta_pct = (base_agg.micros != 0)
            .then(|| why.total_delta_micros as f64 / base_agg.micros as f64 * 100.0);

        Ok(CohortCompareResult {
            selection: selection_res,
            baseline: baseline_res,
            total_delta_micros: why.total_delta_micros,
            total_delta_pct,
            volume_delta_micros: why.volume_micros,
            size_delta_micros: why.size_micros,
            efficiency_delta_micros: why.efficiency_micros,
            compatibility_warnings: compare_warnings(
                selection, baseline, match_rule, &sel_ents, &base_ents, &quality,
            ),
        })
    }

    /// Cross-run search WITHIN a resolved cohort. Matches opaque IDs/hashes
    /// (exact or prefix) and allow-listed labels/tags/notes/model/config (case-insensitive
    /// substring) — never payload text. Capped at 200 results (or a smaller caller `limit`), with a
    /// `truncated` flag. An empty query matches nothing.
    pub fn search_cohort(
        &self,
        cohort: &CohortSpec,
        query: &str,
        fields: Option<&[SearchField]>,
        limit: Option<u32>,
        pricing: &PricingTable,
    ) -> Result<CohortSearchResult, String> {
        if query.is_empty() {
            return Ok(CohortSearchResult {
                entities: Vec::new(),
                truncated: false,
            });
        }
        let fields: &[SearchField] = fields.unwrap_or(&SearchField::ALL);
        let cap = limit
            .map(|l| l as usize)
            .unwrap_or(SEARCH_RESULT_CAP)
            .min(SEARCH_RESULT_CAP);
        let need_note = fields
            .iter()
            .any(|f| matches!(f, SearchField::Tag | SearchField::Note));
        let ents = self.resolve_matched(cohort, pricing)?;
        let qlow = query.to_lowercase();
        let step_grain = matches!(cohort.entity, CohortEntity::Step);

        let mut matched: Vec<EntityRef> = Vec::new();
        for e in &ents {
            let Some(first) = e.steps.first() else {
                continue;
            };
            let run_id = first.step.run_id.clone();
            let note = if need_note {
                self.load_run_note(&run_id)?
            } else {
                None
            };
            if search_entity_matches(e, &run_id, note.as_ref(), query, &qlow, fields) {
                matched.push(EntityRef {
                    run_id,
                    step_ordinal: step_grain.then_some(first.step.step_ordinal),
                });
            }
        }
        let truncated = matched.len() > cap;
        matched.truncate(cap);
        Ok(CohortSearchResult {
            entities: matched,
            truncated,
        })
    }
}

impl Store {
    /// Compute the narrowed candidate run set for `resolve_cohort`.
    /// - `Ok(None)`: no step-scoped filter is index-narrowable (e.g. only CacheClass, or no
    ///   step-scoped filter at all) — scan the full corpus.
    /// - `Ok(Some(run_ids))`: >=1 narrowable step-scoped filter — the distinct run_ids of steps
    ///   satisfying ALL narrowable filters (per-step AND = set intersection). A SUPERSET of
    ///   contributing runs, because non-narrowable step-scoped filters (CacheClass/StopReason) and
    ///   run-scoped/threshold predicates are still applied by `resolve_matched_over_runs`, so the
    ///   resolved result is byte-identical to a full scan.
    fn indexed_candidate_run_ids(
        &self,
        spec: &CohortSpec,
    ) -> Result<Option<std::collections::BTreeSet<String>>, String> {
        let mut acc: Option<std::collections::HashSet<(String, i64)>> = None;
        let mut any_narrowable = false;
        for f in &spec.filters {
            if !filter_is_step_scoped(f) {
                continue;
            }
            let Some(set) = self.candidate_steps_for_filter(f)? else {
                continue; // step-scoped but not index-narrowable; the scan still applies it
            };
            any_narrowable = true;
            acc = Some(match acc {
                None => set,
                Some(prev) => prev.intersection(&set).cloned().collect(),
            });
        }
        if !any_narrowable {
            return Ok(None);
        }
        Ok(Some(
            acc.unwrap_or_default()
                .into_iter()
                .map(|(r, _)| r)
                .collect(),
        ))
    }

    /// The (run_id, step_ordinal) set matching one narrowable step-scoped filter, or `None` when the
    /// filter is not index-narrowable. `Some(empty)` correctly means "excludes every step".
    fn candidate_steps_for_filter(
        &self,
        f: &CohortFilter,
    ) -> Result<Option<std::collections::HashSet<(String, i64)>>, String> {
        match f {
            CohortFilter::StepRefs { refs } => Ok(Some(
                refs.iter()
                    .map(|r| (r.run_id.clone(), r.step_ordinal as i64))
                    .collect(),
            )),
            CohortFilter::Eq { dimension, value } => {
                self.candidate_steps_for_values(*dimension, std::slice::from_ref(value))
            }
            CohortFilter::In { dimension, values } => {
                self.candidate_steps_for_values(*dimension, values)
            }
            _ => Ok(None),
        }
    }

    /// Query the candidate (run_id, step_ordinal) set for an Eq/In on `dim` over `values`, via the
    /// `step_dimensions` index or a `steps` column. `None` = dimension not index-narrowable.
    fn candidate_steps_for_values(
        &self,
        dim: CohortDimension,
        values: &[String],
    ) -> Result<Option<std::collections::HashSet<(String, i64)>>, String> {
        if values.is_empty() {
            return Ok(Some(std::collections::HashSet::new()));
        }
        let placeholders = std::iter::repeat_n("?", values.len())
            .collect::<Vec<_>>()
            .join(",");
        let mut params: Vec<String> = Vec::with_capacity(values.len() + 1);
        let sql = if let Some(idx_dim) = index_dim_name(dim) {
            params.push(idx_dim.to_string());
            format!(
                "SELECT run_id, step_ordinal FROM step_dimensions WHERE dimension = ? AND value IN ({placeholders})"
            )
        } else if let Some(col) = steps_column_for(dim) {
            format!("SELECT run_id, step_ordinal FROM steps WHERE {col} IN ({placeholders})")
        } else {
            return Ok(None);
        };
        params.extend(values.iter().cloned());
        let mut stmt = self.conn.prepare_cached(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| e.to_string())?;
        let mut out = std::collections::HashSet::new();
        for r in rows {
            out.insert(r.map_err(|e| e.to_string())?);
        }
        Ok(Some(out))
    }

    /// Shared candidate-narrowed resolution (perf): narrows to the indexed
    /// candidate run set first — a superset of contributing runs, since narrowing only
    /// uses indexable filters and every run-scoped/threshold predicate is still applied inside
    /// `resolve_matched_over_runs` — so the result is byte-identical to a full-corpus resolve (the
    /// `resolve_cohort_agrees_with_shared_resolve_matched` drift guard is the gate). `resolve_cohort`
    /// and `compare_cohort` both call this so each cohort is resolved once, not once per caller.
    fn resolve_cohort_entities(
        &self,
        spec: &CohortSpec,
        pricing: &PricingTable,
    ) -> Result<Vec<MatchedEntity>, String> {
        spec.validate().map_err(|e| e.to_string())?;
        let candidate = self.indexed_candidate_run_ids(spec)?;
        let runs: Vec<RunRecord> = match candidate {
            Some(ref ids) => self.load_runs_in(ids)?,
            None => self.load_runs()?,
        };
        self.resolve_matched_over_runs(spec, pricing, &runs)
    }

    pub fn resolve_cohort(
        &self,
        spec: &CohortSpec,
        pricing: &PricingTable,
    ) -> Result<CohortResolveResult, String> {
        let ents = self.resolve_cohort_entities(spec, pricing)?;
        Ok(matched_to_result(spec, &ents))
    }

    // ---- Analysis response layer: the SHARED envelope assembly ------------------
    //
    // These methods parse a request DTO, run the matching engine method, wrap the payload in the
    // AnalysisResponse{data,provenance} envelope, and serialize to JSON. BOTH transports
    // — tare-cli's HTTP `/__tare/cohort/*` routes and tare-tauri's `cohort_*` commands — call these,
    // so desktop and browser return byte-identical data with no parallel
    // re-implementation to drift. The store is clock-free: `refreshed_at` is stamped by the caller
    // (RFC3339, via `tare_core::calendar::rfc3339_utc`). Errors carry an HTTP status: validation → 400,
    // list/limit overflow → 413, engine/store failure → 500.

    /// Honest provenance for a cohort response. `coverage_status` is `unknown`
    /// because out-of-band capture has no defensible denominator; `component_fidelity` is the
    /// `coarse` floor; spend is `derived` from `provider_counts` × the pricing edition.
    /// `priced_token_share_pct` is computed over exactly the cohort's runs and present only when
    /// tokens are nonzero; `capture_sources`/pricing edition come from real state.
    fn cohort_provenance(
        &self,
        scope: &CohortSpec,
        pricing: &PricingTable,
        run_ids: &[String],
        refreshed_at: &str,
    ) -> Result<AnalysisProvenance, String> {
        let capture_sources = self.source_counts()?.into_keys().collect();

        // The cohort's runs, loaded once for both the priced-token share and the bucketing provenance.
        let run_ids: std::collections::BTreeSet<String> = run_ids.iter().cloned().collect();
        let cohort_runs = self.load_runs_in(&run_ids)?;

        // Priced token share over exactly the cohort's runs: 100 - unpriced_share, only when
        // tokens are nonzero — mirrors the confidence computation so the two never disagree.
        let priced_token_share_pct = (|| {
            let total_tokens = tare_core::lenses::lenses(&cohort_runs, pricing).total_tokens;
            if total_tokens == 0 {
                return None;
            }
            let report = tare_core::attribute::build_report(&cohort_runs, pricing);
            let unpriced_tokens = report
                .unpriced
                .iter()
                .map(|u| u.token_total)
                .fold(0u64, u64::saturating_add);
            let unpriced_share = unpriced_tokens
                .saturating_mul(100)
                .checked_div(total_tokens)
                .unwrap_or(0) as i64;
            Some(100 - unpriced_share)
        })();

        // The `legacy_date_bucket` assumption is honest — present ONLY when at
        // least one cohort run was bucketed by its immutable capture date (no step carried an
        // authoritative `start_unix_nano`). A fully instant-stamped cohort re-buckets by the requested
        // zone and carries no such caveat.
        let mut assumptions: Vec<String> = Vec::new();
        let any_legacy = cohort_runs
            .iter()
            .any(|r| !r.steps.iter().any(|s| s.start_unix_nano.is_some()));
        if any_legacy {
            assumptions.push(
                "legacy_date_bucket: some entities bucketed by immutable capture date (no start_unix_nano)"
                    .to_string(),
            );
        }

        Ok(AnalysisProvenance {
            refreshed_at: refreshed_at.to_string(),
            scope: scope.clone(),
            capture_sources,
            coverage_status: tare_core::confidence::CoverageStatus::Unknown,
            priced_token_share_pct,
            component_fidelity: ComponentFidelity::Coarse,
            pricing_edition: PricingEdition {
                version: pricing.version.clone(),
                effective_date: pricing.effective_date.clone(),
                mode: PricingEdition::mode_of(&scope.pricing).to_string(),
            },
            allocation_method: AllocationMethod::ProviderCounts,
            value_class: ValueClass::Derived,
            assumptions,
        })
    }

    /// Resolve a `CohortSpec` (JSON body) into the entity set + totals, enveloped.
    pub fn cohort_resolve_response(
        &self,
        body: &[u8],
        pricing: &PricingTable,
        refreshed_at: &str,
    ) -> Result<String, (u16, String)> {
        let spec: CohortSpec = parse_cohort_body(body, "CohortSpec")?;
        validate_cohort_spec(&spec)?;
        let data = self.resolve_cohort(&spec, pricing).map_err(engine_err)?;
        let prov = self
            .cohort_provenance(&spec, pricing, &data.run_ids, refreshed_at)
            .map_err(engine_err)?;
        cohort_response_json(data, prov)
    }

    /// Selection-vs-baseline facet profile for one dimension, enveloped (scoped to the selection).
    pub fn cohort_facets_response(
        &self,
        body: &[u8],
        pricing: &PricingTable,
        refreshed_at: &str,
    ) -> Result<String, (u16, String)> {
        let req: CohortFacetRequest = parse_cohort_body(body, "CohortFacetRequest")?;
        validate_cohort_spec(&req.selection)?;
        validate_cohort_spec(&req.baseline)?;
        let data = self
            .facet_cohort(&req.selection, &req.baseline, req.dimension, pricing)
            .map_err(engine_err)?;
        let sel = self
            .resolve_cohort(&req.selection, pricing)
            .map_err(engine_err)?;
        let prov = self
            .cohort_provenance(&req.selection, pricing, &sel.run_ids, refreshed_at)
            .map_err(engine_err)?;
        cohort_response_json(data, prov)
    }

    /// Decompose selection-vs-baseline spend into volume/size/efficiency deltas, enveloped.
    pub fn cohort_compare_response(
        &self,
        body: &[u8],
        pricing: &PricingTable,
        refreshed_at: &str,
    ) -> Result<String, (u16, String)> {
        let req: CohortCompareRequest = parse_cohort_body(body, "CohortCompareRequest")?;
        validate_cohort_spec(&req.selection)?;
        validate_cohort_spec(&req.baseline)?;
        validate_match_rule(&req.match_rule)?;
        let data = self
            .compare_cohort(&req.selection, &req.baseline, &req.match_rule, pricing)
            .map_err(engine_err)?;
        let prov = self
            .cohort_provenance(
                &req.selection,
                pricing,
                &data.selection.run_ids,
                refreshed_at,
            )
            .map_err(engine_err)?;
        cohort_response_json(data, prov)
    }

    /// Allow-listed cross-run search within a resolved cohort, enveloped.
    pub fn cohort_search_response(
        &self,
        body: &[u8],
        pricing: &PricingTable,
        refreshed_at: &str,
    ) -> Result<String, (u16, String)> {
        let req: CohortSearchRequest = parse_cohort_body(body, "CohortSearchRequest")?;
        validate_cohort_spec(&req.cohort)?;
        if req.query.len() > SEARCH_QUERY_CAP {
            return Err((
                413,
                format!("search query exceeds {SEARCH_QUERY_CAP} bytes"),
            ));
        }
        if req
            .fields
            .as_ref()
            .is_some_and(|fields| fields.len() > SearchField::ALL.len())
        {
            return Err((413, "search field list exceeds the supported set".into()));
        }
        let data = self
            .search_cohort(
                &req.cohort,
                &req.query,
                req.fields.as_deref(),
                req.limit,
                pricing,
            )
            .map_err(engine_err)?;
        let scope = self
            .resolve_cohort(&req.cohort, pricing)
            .map_err(engine_err)?;
        let prov = self
            .cohort_provenance(&req.cohort, pricing, &scope.run_ids, refreshed_at)
            .map_err(engine_err)?;
        cohort_response_json(data, prov)
    }

    /// Dense scoped daily metrics + persisted local configuration markers, enveloped.
    pub fn cohort_timeline_response(
        &self,
        body: &[u8],
        pricing: &PricingTable,
        refreshed_at: &str,
        units: &[tare_core::workunit::WorkUnit],
    ) -> Result<String, (u16, String)> {
        let req: CohortTimelineRequest = parse_cohort_body(body, "CohortTimelineRequest")?;
        validate_cohort_spec(&req.cohort)?;
        let (Some(from), Some(to)) = (&req.cohort.from, &req.cohort.to) else {
            return Err((
                400,
                "timeline cohort requires bounded from and to dates".to_string(),
            ));
        };
        let days = tare_core::calendar::days_between(from, to);
        if days.is_empty() || days.len() > 366 {
            return Err((
                400,
                "timeline cohort date range must contain 1-366 valid inclusive days".to_string(),
            ));
        }
        let data = self
            .timeline_cohort(&req, pricing, units)
            .map_err(engine_err)?;
        let scope = self
            .resolve_cohort(&req.cohort, pricing)
            .map_err(engine_err)?;
        let mut prov = self
            .cohort_provenance(&req.cohort, pricing, &scope.run_ids, refreshed_at)
            .map_err(engine_err)?;
        if req.cohort.metric == CohortMetric::Tokens && req.group != TimelineGroup::Cause {
            // Provider/model/total token counts are captured usage, not a priced derivation. Cause
            // token rows remain derived classifications from the attribution engine.
            prov.value_class = ValueClass::Observed;
        }
        cohort_response_json(data, prov)
    }
}

/// Parse a cohort request body, mapping a deserialize failure to a 400 with the DTO name.
fn parse_cohort_body<T: serde::de::DeserializeOwned>(
    body: &[u8],
    what: &str,
) -> Result<T, (u16, String)> {
    if body.len() > ANALYSIS_BODY_CAP {
        return Err((413, "request body exceeds 256 KiB".into()));
    }
    serde_json::from_slice(body).map_err(|e| (400u16, format!("bad {what}: {e}")))
}

fn validate_match_rule(rule: &MatchRule) -> Result<(), (u16, String)> {
    match rule {
        MatchRule::WorkloadKey { key } if key.is_empty() || key.len() > 64 => {
            Err((400, "workload match key must be 1-64 bytes".into()))
        }
        MatchRule::TemplateLineage { hash }
            if hash.len() != 16 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            Err((
                400,
                "template lineage must be a 16-digit hexadecimal hash".into(),
            ))
        }
        _ => Ok(()),
    }
}

/// Validate a spec, classifying the error: list/limit overflows → 413, other violations → 400.
fn validate_cohort_spec(spec: &CohortSpec) -> Result<(), (u16, String)> {
    spec.validate()
        .map_err(|e| (cohort_error_status(&e), e.to_string()))
}

/// Map a [`CohortError`] to an HTTP status.
fn cohort_error_status(e: &CohortError) -> u16 {
    match e {
        CohortError::TooManyFilters(_)
        | CohortError::TooManyInValues(_)
        | CohortError::TooManyRunIds(_)
        | CohortError::TooManyStepRefs(_) => 413,
        _ => 400,
    }
}

/// An engine/store failure is a 500 (the request itself was well-formed + valid).
fn engine_err(e: String) -> (u16, String) {
    (500u16, e)
}

/// Serialize an [`AnalysisResponse`] to JSON, mapping a serializer failure to 500.
fn cohort_response_json<T: serde::Serialize>(
    data: T,
    provenance: AnalysisProvenance,
) -> Result<String, (u16, String)> {
    serde_json::to_string(&AnalysisResponse { data, provenance })
        .map_err(|e| (500u16, format!("serialize analysis response: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tare_core::ingest_step;

    fn fixture_pricing() -> PricingTable {
        PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml")).unwrap()
    }

    #[test]
    fn record_transcript_steps_commits_dedup_marks_atomically() {
        // The dedup mark is written in the same call and transaction as the step, so the
        // capture sweep's rollback is sound and a committed step is never left un-deduped (which
        // would re-ingest with fresh ordinals and double-count on the next sweep).
        use tare_core::ingest_step;
        let store = Store::open_in_memory().unwrap();
        let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");
        let step = ingest_step("run-a", 1, Provider::Anthropic, req, resp).unwrap();
        let marks = vec![("id:m1|run-a".to_string(), "run-a".to_string())];
        store
            .record_transcript_steps(
                &[(step, "2026-06-24".to_string(), Some(9u8))],
                &marks,
                None,
                Some("jsonl"),
            )
            .unwrap();
        // Both the step and its dedup key landed from the one call.
        assert_eq!(store.max_step_ordinal("run-a").unwrap(), Some(1));
        assert!(store.backfilled_keys().unwrap().contains("id:m1|run-a"));
    }

    #[test]
    fn step_writes_validate_inputs_and_roll_back_dimension_failures() {
        let store = Store::open_in_memory().unwrap();
        let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");
        let step = ingest_step("run-a", 1, Provider::Anthropic, req, resp).unwrap();

        assert!(store.record_step(&step, "2026-02-30").is_err());
        assert!(store
            .record_step_with_time(&step, "2026-06-24", Some(24), None, None, None)
            .is_err());

        let mut inconsistent = step.clone();
        inconsistent.shape.model = "different-model".into();
        assert!(store.record_step(&inconsistent, "2026-06-24").is_err());

        let mut bad_reasoning = step.clone();
        bad_reasoning.usage.reasoning = bad_reasoning.usage.output.saturating_add(1);
        assert!(store.record_step(&bad_reasoning, "2026-06-24").is_err());

        let mut oversized = step.clone();
        oversized.usage.fresh_input = i64::MAX as u64 + 1;
        assert!(store.record_step(&oversized, "2026-06-24").is_err());

        let counts = |store: &Store| {
            store
                .conn
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM runs), (SELECT COUNT(*) FROM steps)",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .unwrap()
        };
        assert_eq!(
            counts(&store),
            (0, 0),
            "validation failures persist nothing"
        );

        store
            .conn
            .execute_batch(
                "CREATE TRIGGER reject_step_dimensions
                 BEFORE INSERT ON step_dimensions
                 BEGIN SELECT RAISE(ABORT, 'forced dimension failure'); END;",
            )
            .unwrap();
        assert!(store.record_step(&step, "2026-06-24").is_err());
        assert_eq!(
            counts(&store),
            (0, 0),
            "the run and step roll back with the index"
        );
    }

    #[test]
    fn migrates_from_empty_and_round_trips() {
        let store = Store::open_in_memory().unwrap();

        let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");
        let step = ingest_step("run-a", 1, Provider::Anthropic, req, resp).unwrap();
        store.record_step(&step, "2026-06-24").unwrap();

        let runs = store.load_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, "run-a");
        // Round-trip equality: rebuilt StepRecord matches the original byte-for-byte.
        assert_eq!(runs[0].steps[0], step);
        // From-empty reaches the current schema version.
        let v: i64 = store
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
    }

    // ---- step_dimensions index ----

    fn all_dims(store: &Store) -> Vec<(String, i64, String, String)> {
        let mut stmt = store
            .conn
            .prepare(
                "SELECT run_id, step_ordinal, dimension, value FROM step_dimensions
                 ORDER BY run_id, step_ordinal, dimension, value",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    fn orphan_count(store: &Store) -> i64 {
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM step_dimensions sd
                 LEFT JOIN steps s ON sd.run_id = s.run_id AND sd.step_ordinal = s.step_ordinal
                 WHERE s.run_id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    #[test]
    fn migration_creates_step_dimensions_table_and_indexes() {
        let store = Store::open_in_memory().unwrap();
        let idx: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index'
                 AND name IN ('step_dimensions_lookup','step_dimensions_step')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx, 2, "both covering indexes exist");
        let n: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM step_dimensions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn step_dimension_rows_maps_shape_fields() {
        let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");
        let mut step = ingest_step("r", 1, Provider::Anthropic, req, resp).unwrap();
        step.shape.session = Some("sess-1".into());
        step.shape.parent_label = Some("agent-x".into());
        step.shape.component_label = Some("tool-y".into());
        step.shape.effort = Some("high".into());
        step.shape.workload_key = Some("nightly-eval".into());
        step.shape.system_hash = Some(0x0123_4567_89ab_cdef);
        step.shape.ttl = CacheTtl::OneHour;
        let rows = step_dimension_rows(&step.shape);
        let has = |d: &str, v: &str| rows.iter().any(|(dd, vv)| *dd == d && vv == v);
        assert!(has("session", "sess-1"));
        assert!(has("parent", "agent-x")); // agent vocabulary resolves to parent storage
        assert!(has("component", "tool-y")); // tool resolves to component storage
        assert!(has("effort", "high"));
        assert!(has("workload_key", "nightly-eval")); // User-provided grouping label.
        assert!(has("template", "0123456789abcdef")); // 16-hex, matches CohortDimension::Template
        assert!(has("ttl", "1h"));
        // Provider/model are first-class columns — never duplicated into step_dimensions.
        assert!(!rows.iter().any(|(d, _)| *d == "provider" || *d == "model"));
    }

    #[test]
    fn step_dimensions_materialize_on_write_and_stay_orphan_free() {
        let store = Store::open_in_memory().unwrap();
        let bench = tare_core::calibrated_bench::build();
        for sr in &bench.runs {
            for step in &sr.run.steps {
                store
                    .record_step_with_time(
                        step,
                        &sr.date,
                        sr.hour,
                        None,
                        Some(sr.profile),
                        Some(sr.source),
                    )
                    .unwrap();
            }
        }
        let dims = all_dims(&store);
        assert!(!dims.is_empty());
        assert_eq!(
            orphan_count(&store),
            0,
            "no step_dimensions row lacks a parent step"
        );
        // The fixture exercises the allow-listed dimensions.
        let present: std::collections::BTreeSet<&str> = dims.iter().map(|d| d.2.as_str()).collect();
        for expect in [
            "session",
            "effort",
            "ttl",
            "template",
            "commit",
            "author",
            "mcp_server",
            "vendor",
        ] {
            assert!(present.contains(expect), "dimension {expect} materialized");
        }
    }

    #[test]
    fn backfill_reproduces_identical_dimensions_as_writes() {
        // Acceptance: "old DB migration and new writes produce identical dimensions."
        let store = Store::open_in_memory().unwrap();
        let bench = tare_core::calibrated_bench::build();
        for sr in &bench.runs {
            for step in &sr.run.steps {
                store
                    .record_step_with_time(
                        step,
                        &sr.date,
                        sr.hour,
                        None,
                        Some(sr.profile),
                        Some(sr.source),
                    )
                    .unwrap();
            }
        }
        let from_writes = all_dims(&store);
        // Simulate an un-backfilled old DB, then run the post-migration backfill (from < len forces it).
        store
            .conn
            .execute("DELETE FROM step_dimensions", [])
            .unwrap();
        assert!(all_dims(&store).is_empty());
        store.backfill_step_dimensions_if_migrated(0).unwrap();
        assert_eq!(
            from_writes,
            all_dims(&store),
            "backfill == write-path materialization"
        );
        assert_eq!(orphan_count(&store), 0);
    }

    #[test]
    fn re_recording_a_step_refreshes_its_dimensions() {
        let store = Store::open_in_memory().unwrap();
        let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");
        let mut step = ingest_step("r", 1, Provider::Anthropic, req, resp).unwrap();
        step.shape.session = Some("old".into());
        store.record_step(&step, "2026-06-24").unwrap();
        step.shape.session = Some("new".into());
        store.record_step(&step, "2026-06-24").unwrap(); // INSERT OR REPLACE + dims refresh
        let sessions: Vec<String> = {
            let mut s = store
                .conn
                .prepare("SELECT value FROM step_dimensions WHERE dimension='session'")
                .unwrap();
            s.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(
            sessions,
            vec!["new".to_string()],
            "stale dimension removed on re-record"
        );
    }

    // ---- cohort resolve engine ----

    fn cohort_base(entity: CohortEntity) -> CohortSpec {
        CohortSpec {
            from: None,
            to: None,
            timezone: "UTC".into(),
            entity,
            filters: vec![],
            pricing: PricingMode::Latest,
            metric: tare_core::cohort::CohortMetric::SpendMicros,
            normalization: tare_core::cohort::Normalization::Absolute,
            outcome_denominator: None,
        }
    }

    fn priced_step(run_id: &str, ordinal: u32, model: &str, session: &str) -> StepRecord {
        let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");
        let mut s = ingest_step(run_id, ordinal, Provider::Anthropic, req, resp).unwrap();
        s.model = model.to_string();
        s.shape.model = model.to_string();
        s.shape.session = Some(session.to_string());
        s
    }

    /// Two runs: A = opus + haiku, B = opus. All priced via the fixture table.
    fn seed_cohort_store() -> (Store, StepRecord, StepRecord, StepRecord) {
        let store = Store::open_in_memory().unwrap();
        let a1 = priced_step("A", 1, "claude-opus-4-8", "alpha");
        let a2 = priced_step("A", 2, "claude-haiku-4-5", "beta");
        let b1 = priced_step("B", 1, "claude-opus-4-8", "alpha");
        for s in [&a1, &a2, &b1] {
            store.record_step(s, "2026-06-24").unwrap();
        }
        (store, a1, a2, b1)
    }

    #[test]
    fn scoped_timeline_supports_daily_metrics_without_global_fallbacks() {
        use tare_core::cohort::{
            CohortTimelineRequest, MeteredOutcome, Normalization, OutcomeDenominator,
            TimelineGroup, TimelineUnit,
        };
        use tare_core::model::UsageTokens;

        let store = Store::open_in_memory().unwrap();
        let mut keep = priced_step("keep", 1, "claude-opus-4-8", "selected");
        keep.usage = UsageTokens {
            fresh_input: 100,
            cache_read: 300,
            output: 50,
            ..Default::default()
        };
        let mut noise = priced_step("noise", 1, "claude-haiku-4-5", "other");
        noise.usage = UsageTokens {
            fresh_input: 10_000,
            cache_read: 20_000,
            ..Default::default()
        };
        store.record_step(&keep, "2026-07-10").unwrap();
        store.record_step(&noise, "2026-07-10").unwrap();
        let pricing = fixture_pricing();
        let mut cohort = cohort_base(CohortEntity::Run);
        cohort.from = Some("2026-07-10".into());
        cohort.to = Some("2026-07-11".into());
        cohort.metric = tare_core::cohort::CohortMetric::Tokens;
        cohort.filters = vec![CohortFilter::Eq {
            dimension: CohortDimension::Model,
            value: "claude-opus-4-8".into(),
        }];

        let tokens = store
            .timeline_cohort(
                &CohortTimelineRequest {
                    cohort: cohort.clone(),
                    group: TimelineGroup::Total,
                },
                &pricing,
                &[],
            )
            .unwrap();
        assert_eq!(tokens.days, vec!["2026-07-10", "2026-07-11"]);
        assert_eq!(tokens.unit, TimelineUnit::Tokens);
        assert_eq!(tokens.run_count, 1);
        assert_eq!(tokens.series[0].points[0].value, Some(450.0));
        assert_eq!(tokens.series[0].points[1].value, Some(0.0));

        cohort.metric = tare_core::cohort::CohortMetric::CacheHitRate;
        let cache = store
            .timeline_cohort(
                &CohortTimelineRequest {
                    cohort: cohort.clone(),
                    group: TimelineGroup::Total,
                },
                &pricing,
                &[],
            )
            .unwrap();
        assert_eq!(cache.unit, TimelineUnit::Percent);
        assert_eq!(cache.series[0].points[0].value, Some(75.0));
        assert!(cache.series[0].points[1].value.is_none());
        assert!(cache.series[0].points[1]
            .unavailable_reason
            .as_deref()
            .unwrap()
            .contains("cache input denominator"));

        // A filtered cohort cannot borrow the whole-day metered PR count. It remains unavailable
        // with a reason even though a global counter exists for the same day.
        store
            .record_metered(&tare_core::otel::MeteredPoint {
                day: "2026-07-10".into(),
                metric: "pull_request".into(),
                model: String::new(),
                kind: String::new(),
                session: String::new(),
                effort: String::new(),
                query_source: String::new(),
                value: 7,
            })
            .unwrap();
        cohort.metric = tare_core::cohort::CohortMetric::Tokens;
        cohort.normalization = Normalization::PerOutcome;
        cohort.outcome_denominator = Some(OutcomeDenominator::Metered {
            kind: MeteredOutcome::PullRequests,
        });
        let filtered_ratio = store
            .timeline_cohort(
                &CohortTimelineRequest {
                    cohort,
                    group: TimelineGroup::Total,
                },
                &pricing,
                &[],
            )
            .unwrap();
        assert!(filtered_ratio.series[0].points[0].value.is_none());
        assert_eq!(filtered_ratio.series[0].points[0].denominator, None);
        assert!(filtered_ratio.series[0].points[0]
            .unavailable_reason
            .as_deref()
            .unwrap()
            .contains("filtered cohort"));
    }

    #[test]
    fn timeline_config_events_are_persisted_metadata_and_timezone_bucketed() {
        let store = Store::open_in_memory().unwrap();
        store
            .record_config_change_event(
                "2026-07-11T06:30:00Z",
                "settings",
                &["privacy.profile".into(), "capture.mode".into()],
            )
            .unwrap();
        let rows = store
            .config_change_events("2026-07-10", "2026-07-10", "America/Los_Angeles")
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].day, "2026-07-10");
        assert_eq!(
            rows[0].changed_fields,
            vec!["capture.mode".to_string(), "privacy.profile".to_string()]
        );
        let raw: String = store
            .conn
            .query_row(
                "SELECT changed_fields_json FROM config_change_events",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!raw.contains("strict_counts"));
        assert!(!raw.contains("http"));
    }

    #[test]
    fn scoped_timeline_covers_grouping_and_real_selected_run_denominators() {
        use tare_core::cohort::{
            CohortTimelineRequest, MeteredOutcome, Normalization, OutcomeDenominator,
            TimelineGroup, TimelineUnit,
        };
        use tare_core::workunit::{UnitMatch, WorkUnit};

        let (store, _a1, _a2, _b1) = seed_cohort_store();
        let pricing = fixture_pricing();
        let mut cohort = cohort_base(CohortEntity::Run);
        cohort.from = Some("2026-06-24".into());
        cohort.to = Some("2026-06-24".into());
        cohort.metric = CohortMetric::Tokens;
        cohort.normalization = Normalization::ShareOfSelection;
        let shares = store
            .timeline_cohort(
                &CohortTimelineRequest {
                    cohort: cohort.clone(),
                    group: TimelineGroup::Model,
                },
                &pricing,
                &[],
            )
            .unwrap();
        assert_eq!(shares.unit, TimelineUnit::Percent);
        let share_sum: f64 = shares
            .series
            .iter()
            .map(|series| series.points[0].value.unwrap())
            .sum();
        assert!((share_sum - 100.0).abs() < 0.000_001);

        cohort.normalization = Normalization::Absolute;
        let causes = store
            .timeline_cohort(
                &CohortTimelineRequest {
                    cohort: cohort.clone(),
                    group: TimelineGroup::Cause,
                },
                &pricing,
                &[],
            )
            .unwrap();
        assert!(
            !causes.series.is_empty(),
            "fixture attribution exposes at least one cause"
        );
        assert!(causes
            .series
            .iter()
            .all(|series| series.points[0].support_count <= 2));

        cohort.metric = CohortMetric::SpendMicros;
        cohort.normalization = Normalization::PerRun;
        let per_run = store
            .timeline_cohort(
                &CohortTimelineRequest {
                    cohort: cohort.clone(),
                    group: TimelineGroup::Total,
                },
                &pricing,
                &[],
            )
            .unwrap();
        assert_eq!(per_run.series[0].points[0].denominator, Some(2.0));
        assert_eq!(
            per_run.series[0].points[0].value,
            Some(
                store
                    .resolve_cohort(&cohort, &pricing)
                    .unwrap()
                    .total_micros as f64
                    / 2.0
            )
        );

        cohort.normalization = Normalization::PerOutcome;
        cohort.outcome_denominator = Some(OutcomeDenominator::Metered {
            kind: MeteredOutcome::SuccessfulRuns,
        });
        let successful = store
            .timeline_cohort(
                &CohortTimelineRequest {
                    cohort: cohort.clone(),
                    group: TimelineGroup::Total,
                },
                &pricing,
                &[],
            )
            .unwrap();
        assert_eq!(successful.series[0].points[0].denominator, Some(2.0));

        cohort.outcome_denominator = Some(OutcomeDenominator::WorkUnit {
            name: "selected-work".into(),
        });
        let units = [WorkUnit {
            name: "selected-work".into(),
            match_: UnitMatch {
                sessions: vec!["alpha".into()],
                ..Default::default()
            },
        }];
        let work_unit = store
            .timeline_cohort(
                &CohortTimelineRequest {
                    cohort,
                    group: TimelineGroup::Total,
                },
                &pricing,
                &units,
            )
            .unwrap();
        assert_eq!(work_unit.series[0].points[0].denominator, Some(2.0));
    }

    #[test]
    fn resolve_single_equality_matches_rollup() {
        let (store, a1, _a2, b1) = seed_cohort_store();
        let pricing = fixture_pricing();
        let mut spec = cohort_base(CohortEntity::Run);
        spec.filters = vec![CohortFilter::Eq {
            dimension: CohortDimension::Model,
            value: "claude-opus-4-8".into(),
        }];
        let r = store.resolve_cohort(&spec, &pricing).unwrap();
        assert_eq!(r.run_ids, vec!["A".to_string(), "B".to_string()]);
        // Scoped total == the opus steps' spend, computed independently the same way rollup would.
        let opus = attribute::step_micros(&a1, &pricing, None)
            + attribute::step_micros(&b1, &pricing, None);
        assert!(opus > 0);
        assert_eq!(r.total_micros, opus);
        assert_eq!(r.step_count, 2);
    }

    #[test]
    fn indexed_resolve_is_byte_identical_to_full_scan() {
        // resolve_cohort narrows to a candidate run set via step_dimensions / steps
        // columns, then prices only those runs. The serialized CohortResolveResult MUST be identical
        // to the full scan — the shared core over the WHOLE corpus (resolve_matched_over_runs +
        // matched_to_result). Exercise: step_dimensions dim (session), steps column (model),
        // In, per-step intersection, StepRefs, narrowable+non-narrowable (CacheClass) mix, and empty-In
        // — over Run and Step grain.
        let (store, ..) = seed_cohort_store();
        let pricing = fixture_pricing();
        let all = store.load_runs().unwrap();

        let mut specs: Vec<CohortSpec> = Vec::new();
        for entity in [CohortEntity::Run, CohortEntity::Step] {
            let mut s = cohort_base(entity);
            s.filters = vec![CohortFilter::Eq {
                dimension: CohortDimension::Session,
                value: "alpha".into(),
            }];
            specs.push(s);

            let mut s = cohort_base(entity);
            s.filters = vec![CohortFilter::Eq {
                dimension: CohortDimension::Model,
                value: "claude-opus-4-8".into(),
            }];
            specs.push(s);

            let mut s = cohort_base(entity);
            s.filters = vec![CohortFilter::In {
                dimension: CohortDimension::Session,
                values: vec!["alpha".into(), "beta".into()],
            }];
            specs.push(s);

            let mut s = cohort_base(entity);
            s.filters = vec![
                CohortFilter::Eq {
                    dimension: CohortDimension::Session,
                    value: "alpha".into(),
                },
                CohortFilter::Eq {
                    dimension: CohortDimension::Model,
                    value: "claude-opus-4-8".into(),
                },
            ];
            specs.push(s);

            let mut s = cohort_base(entity);
            s.filters = vec![CohortFilter::StepRefs {
                refs: vec![StepRef {
                    run_id: "A".into(),
                    step_ordinal: 1,
                }],
            }];
            specs.push(s);

            // Narrowable (model) AND non-narrowable (CacheClass): index narrows the candidate set,
            // the scan still applies CacheClass — byte-identical either way.
            let mut s = cohort_base(entity);
            s.filters = vec![
                CohortFilter::Eq {
                    dimension: CohortDimension::Model,
                    value: "claude-opus-4-8".into(),
                },
                CohortFilter::Eq {
                    dimension: CohortDimension::CacheClass,
                    value: "warm".into(),
                },
            ];
            specs.push(s);

            // Empty In excludes every step -> empty candidate set -> empty result.
            let mut s = cohort_base(entity);
            s.filters = vec![CohortFilter::In {
                dimension: CohortDimension::Session,
                values: vec![],
            }];
            specs.push(s);
        }

        for spec in &specs {
            let indexed = store.resolve_cohort(spec, &pricing).unwrap();
            let scan = matched_to_result(
                spec,
                &store
                    .resolve_matched_over_runs(spec, &pricing, &all)
                    .unwrap(),
            );
            assert_eq!(
                serde_json::to_string(&indexed).unwrap(),
                serde_json::to_string(&scan).unwrap(),
                "indexed != full scan for filters {:?}",
                spec.filters
            );
        }
    }

    #[test]
    fn load_runs_in_preserves_full_scan_order() {
        // The candidate subset loader must preserve the full-scan run order (rowid) so entity_rows
        // ordering stays byte-identical.
        let (store, ..) = seed_cohort_store();
        let ids: std::collections::BTreeSet<String> =
            ["A".to_string(), "B".to_string()].into_iter().collect();
        let subset: Vec<String> = store
            .load_runs_in(&ids)
            .unwrap()
            .into_iter()
            .map(|r| r.run_id)
            .collect();
        let filtered: Vec<String> = store
            .load_runs()
            .unwrap()
            .into_iter()
            .filter(|r| ids.contains(&r.run_id))
            .map(|r| r.run_id)
            .collect();
        assert_eq!(subset, filtered);
        assert!(store
            .load_runs_in(&std::collections::BTreeSet::new())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn savings_verification_excludes_the_intervention_local_date() {
        // the intervention day excluded from the before/after windows is
        // acted_at's date IN THE STORED COHORT TIMEZONE, not its UTC date. acted_at 2026-06-15T02:00Z
        // is 2026-06-14 in America/Los_Angeles (PDT -7), so with window_days=1 the AFTER window is
        // [06-15] and a run on 06-15 lands IN it — whereas UTC-date derivation would have excluded
        // 06-15 as the intervention day (after window [06-16], empty).
        use serde_json::json;
        use tare_core::model::UsageTokens;
        let store = Store::open_in_memory().unwrap();
        let mk = |run: &str, day: &str, fresh: u64| {
            let mut s = priced_step(run, 1, "claude-opus-4-8", "s");
            s.usage = UsageTokens {
                fresh_input: fresh,
                ..Default::default()
            };
            store.record_step(&s, day).unwrap();
        };
        mk("before", "2026-06-13", 10_000_000);
        mk("dayof", "2026-06-15", 7_000_000);
        let pricing = fixture_pricing();
        let sel = json!({"timezone":"America/Los_Angeles","entity":"run","filters":[{"op":"eq","dimension":"model","value":"claude-opus-4-8"}],"pricing":{"mode":"effective_dated"},"metric":"spend_micros","normalization":"absolute"});
        let req: SavingsActionRequest = serde_json::from_value(json!({
            "opportunity_key":"k","cohort":sel,"match":{"kind":"aggregate_only"},
            "metric":"spend_micros","normalization":"absolute"
        }))
        .unwrap();
        store
            .put_savings_action(&req, "applied", "2026-06-15T02:00:00Z")
            .unwrap();
        let hash = req.cohort.cohort_hash();
        let vr: SavingsVerifyRequest = serde_json::from_value(json!({
            "opportunity_key":"k","cohort_hash":hash,"window_days":1,"as_of_date":"2026-06-30"
        }))
        .unwrap();
        let r = store.verify_savings_action(&vr, &pricing).unwrap();
        // Local intervention day = 06-14 → after window [06-15] includes the 06-15 run (>0). Under UTC
        // derivation the after window would be [06-16] and this would be 0.
        assert!(
            r.selection_after_micros > 0,
            "06-15 run must fall in the after window under local (LA) intervention-date bucketing"
        );
        assert!(r.selection_before_micros > 0); // 06-13 run in the before window
    }

    #[test]
    fn timestamped_runs_rebucket_by_zone_legacy_rows_keep_capture_date() {
        // a run with start_unix_nano buckets by its zone-local (DST-aware) date
        // and RE-buckets when the requested tz changes; a legacy run (no instant) keeps its immutable
        // created_date regardless of tz. Provenance carries `legacy_date_bucket` ONLY when a legacy
        // run is in the cohort.
        let store = Store::open_in_memory().unwrap();
        let pricing = fixture_pricing();
        let secs = tare_core::calendar::parse_iso8601_to_secs("2026-07-01T02:00:00Z").unwrap();
        // 02:00Z Jul 1 -> America/Los_Angeles (PDT -7) = Jun 30 19:00 -> 2026-06-30; UTC -> 2026-07-01.
        let mut ts = priced_step("ts", 1, "claude-opus-4-8", "alpha");
        ts.start_unix_nano = Some(UnixNanos(secs as u128 * 1_000_000_000));
        // created_date deliberately far off, to prove the INSTANT wins over the capture date.
        store.record_step(&ts, "2026-01-01").unwrap();
        let legacy = priced_step("legacy", 1, "claude-opus-4-8", "alpha"); // no instant
        store.record_step(&legacy, "2026-06-30").unwrap();

        let resolve_ids = |tz: &str, day: &str| {
            let mut spec = cohort_base(CohortEntity::Run);
            spec.timezone = tz.into();
            spec.from = Some(day.into());
            spec.to = Some(day.into());
            let mut ids = store.resolve_cohort(&spec, &pricing).unwrap().run_ids;
            ids.sort();
            ids
        };
        // LA: ts re-buckets to 2026-06-30 (instant, not its 2026-01-01 capture date); legacy is 06-30.
        assert_eq!(
            resolve_ids("America/Los_Angeles", "2026-06-30"),
            vec!["legacy".to_string(), "ts".to_string()]
        );
        // UTC: the SAME instant buckets to 2026-07-01, so ts leaves the 06-30 window; legacy stays.
        assert_eq!(resolve_ids("UTC", "2026-06-30"), vec!["legacy".to_string()]);
        assert_eq!(resolve_ids("UTC", "2026-07-01"), vec!["ts".to_string()]);

        // Provenance: ts-only cohort (fully stamped) carries NO legacy_date_bucket; a cohort with the
        // legacy run does.
        let prov = |tz: &str, day: &str| {
            let mut spec = cohort_base(CohortEntity::Run);
            spec.timezone = tz.into();
            spec.from = Some(day.into());
            spec.to = Some(day.into());
            let ids = store.resolve_cohort(&spec, &pricing).unwrap().run_ids;
            store
                .cohort_provenance(&spec, &pricing, &ids, "2026-07-14T00:00:00Z")
                .unwrap()
        };
        let is_legacy = |p: &AnalysisProvenance| {
            p.assumptions
                .iter()
                .any(|a| a.contains("legacy_date_bucket"))
        };
        assert!(!is_legacy(&prov("UTC", "2026-07-01"))); // only ts (stamped)
        assert!(is_legacy(&prov("UTC", "2026-06-30"))); // only the legacy run
    }

    #[test]
    fn otlp_timing_persists_as_text_and_round_trips() {
        // Timing/span fields round-trip through the store, start_unix_nano is
        // stored as a decimal TEXT string (never an unsafe signed-64 INTEGER), and legacy/proxy rows
        // with NULL columns load as None (step-order-only).
        let store = Store::open_in_memory().unwrap();
        let big: u128 = 1_700_000_000_123_456_789; // > i64::MAX-ish nanos; must survive as text
        let mut timed = priced_step("A", 1, "claude-opus-4-8", "alpha");
        timed.start_unix_nano = Some(UnixNanos(big));
        timed.trace_id = Some("trace42".into());
        timed.span_id = Some("child01".into());
        timed.parent_span_id = Some("root99".into());
        timed.duration_ms = 1500;
        let legacy = priced_step("B", 1, "claude-opus-4-8", "alpha"); // no timing (proxy/legacy)
        store.record_step(&timed, "2026-06-24").unwrap();
        store.record_step(&legacy, "2026-06-24").unwrap();

        // Column is TEXT and holds the exact decimal string — never rounded through an integer.
        let (kind, val): (String, String) = store
            .conn
            .query_row(
                "SELECT typeof(start_unix_nano), start_unix_nano FROM steps WHERE run_id='A'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(kind, "text");
        assert_eq!(val, big.to_string());

        // Round-trip through the normal load path.
        let runs = store.load_runs().unwrap();
        let a = runs.iter().find(|r| r.run_id == "A").unwrap();
        let s = &a.steps[0];
        assert_eq!(s.start_unix_nano, Some(UnixNanos(big)));
        assert_eq!(s.trace_id.as_deref(), Some("trace42"));
        assert_eq!(s.span_id.as_deref(), Some("child01"));
        assert_eq!(s.parent_span_id.as_deref(), Some("root99"));
        assert_eq!(s.end_unix_nano(), Some(UnixNanos(big + 1_500_000_000)));

        // Legacy/proxy row: all timing fields load as None (UI shows step order, not a timeline).
        let b = runs.iter().find(|r| r.run_id == "B").unwrap();
        assert_eq!(b.steps[0].start_unix_nano, None);
        assert_eq!(b.steps[0].parent_span_id, None);
    }

    #[test]
    fn savings_verification_adjusted_formula_windows_and_states() {
        // Equal before/after windows exclude the intervention day; the
        // adjusted reduction is difference-in-differences; incomplete after window stays `verifying`;
        // aggregate-only + no-baseline warn; a missing action is 404.
        use serde_json::json;
        use tare_core::model::UsageTokens;
        let store = Store::open_in_memory().unwrap();
        let mk = |run: &str, model: &str, fresh: u64, wk: &str| {
            let mut s = priced_step(run, 1, model, "s");
            s.usage = UsageTokens {
                fresh_input: fresh,
                ..Default::default()
            };
            s.shape.workload_key = Some(wk.into());
            s
        };
        // Selection = opus, tagged "nightly". Before (06-13) big, after (06-17) small → a reduction.
        store
            .record_step(
                &mk("op-b", "claude-opus-4-8", 10_000_000, "nightly"),
                "2026-06-13",
            )
            .unwrap();
        store
            .record_step(
                &mk("op-a", "claude-opus-4-8", 1_000_000, "nightly"),
                "2026-06-17",
            )
            .unwrap();
        // Baseline = haiku, unchanged across the windows (control), carrying the SAME workload
        // identity so all four required windows share a like-for-like unit.
        store
            .record_step(
                &mk("hk-b", "claude-haiku-4-5", 5_000_000, "nightly"),
                "2026-06-13",
            )
            .unwrap();
        store
            .record_step(
                &mk("hk-a", "claude-haiku-4-5", 5_000_000, "nightly"),
                "2026-06-17",
            )
            .unwrap();
        // A step ON the intervention day must be EXCLUDED from both windows.
        store
            .record_step(
                &mk("op-x", "claude-opus-4-8", 99_000_000, "nightly"),
                "2026-06-15",
            )
            .unwrap();
        let pricing = fixture_pricing();

        let sel = json!({"timezone":"UTC","entity":"run","filters":[{"op":"eq","dimension":"model","value":"claude-opus-4-8"}],
            "pricing":{"mode":"effective_dated"},"metric":"spend_micros","normalization":"absolute"});
        let base = json!({"timezone":"UTC","entity":"run","filters":[{"op":"eq","dimension":"model","value":"claude-haiku-4-5"}],
            "pricing":{"mode":"effective_dated"},"metric":"spend_micros","normalization":"absolute"});
        let req: SavingsActionRequest = serde_json::from_value(json!({
            "opportunity_key":"rightsizing:opus","cohort":sel,
            "baseline":{"kind":"explicit_cohort","label":"haiku control","cohort":base},
            "match":{"kind":"workload_key","key":"nightly"},
            "metric":"spend_micros","normalization":"absolute"
        }))
        .unwrap();
        store
            .put_savings_action(&req, "applied", "2026-06-15T00:00:00Z")
            .unwrap();
        let hash = req.cohort.cohort_hash();

        // Complete verification (as_of well past the after window), window 3.
        let vr: SavingsVerifyRequest = serde_json::from_value(json!({
            "opportunity_key":"rightsizing:opus","cohort_hash":hash,"window_days":3,"as_of_date":"2026-06-30"
        })).unwrap();
        let r = store.verify_savings_action(&vr, &pricing).unwrap();
        assert!(r.complete);
        // The intervention day's 99M-token step is NOT in either window.
        assert!(r.selection_before_micros < 99_000_000_000_000);
        assert!(
            r.selection_before_micros > r.selection_after_micros,
            "opus spend fell"
        );
        let bb = r.baseline_before_micros.unwrap();
        let ba = r.baseline_after_micros.unwrap();
        // ADJUSTED formula = difference-in-differences (validated against the returned components).
        assert_eq!(
            r.observed_reduction_micros,
            (r.selection_before_micros - r.selection_after_micros) + (ba - bb)
        );
        assert_eq!(r.status, "observed_reduction"); // complete + positive
        assert_eq!(r.matched_before, 1); // op-b carries "nightly"
        assert_eq!(r.matched_after, 1); // op-a carries "nightly"
        assert_eq!(r.unmatched_before, 0);
        assert_eq!(r.baseline_matched_before, Some(1));
        assert_eq!(r.baseline_matched_after, Some(1));
        assert_eq!(r.baseline_unmatched_before, Some(0));
        assert_eq!(r.baseline_unmatched_after, Some(0));
        let wire = serde_json::to_value(&r).unwrap();
        assert_eq!(wire["baseline_matched_before"], 1);
        assert_eq!(wire["baseline_unmatched_after"], 0);

        // Incomplete after window → verifying (as_of before the after window closes).
        let vr_inc: SavingsVerifyRequest = serde_json::from_value(json!({
            "opportunity_key":"rightsizing:opus","cohort_hash":hash,"window_days":3,"as_of_date":"2026-06-16"
        })).unwrap();
        let r_inc = store.verify_savings_action(&vr_inc, &pricing).unwrap();
        assert!(!r_inc.complete);
        assert_eq!(r_inc.status, "verifying");

        // Aggregate-only + no baseline → both warnings; unadjusted reduction; matched=0.
        let agg: SavingsActionRequest = serde_json::from_value(json!({
            "opportunity_key":"loop:x","cohort":sel,
            "match":{"kind":"aggregate_only"},"metric":"spend_micros","normalization":"absolute"
        }))
        .unwrap();
        store
            .put_savings_action(&agg, "applied", "2026-06-15T00:00:00Z")
            .unwrap();
        let vr_agg: SavingsVerifyRequest = serde_json::from_value(json!({
            "opportunity_key":"loop:x","cohort_hash":agg.cohort.cohort_hash(),"window_days":3,"as_of_date":"2026-06-30"
        })).unwrap();
        let r_agg = store.verify_savings_action(&vr_agg, &pricing).unwrap();
        assert!(r_agg.baseline_before_micros.is_none());
        assert_eq!(
            r_agg.observed_reduction_micros,
            r_agg.selection_before_micros - r_agg.selection_after_micros
        );
        assert!(r_agg.matched_before == 0 && r_agg.matched_after == 0); // aggregate matches nothing
        assert!(r_agg
            .compatibility_warnings
            .iter()
            .any(|w| w.contains("aggregate-only")));
        assert!(r_agg
            .compatibility_warnings
            .iter()
            .any(|w| w.contains("no baseline")));

        // Unknown action → 404.
        let vr_missing: SavingsVerifyRequest = serde_json::from_value(json!({
            "opportunity_key":"nope","cohort_hash":"0"
        }))
        .unwrap();
        let (code, _) = store
            .verify_savings_action(&vr_missing, &pricing)
            .unwrap_err();
        assert_eq!(code, 404);
    }

    #[test]
    fn savings_lifecycle_totals_include_only_applied_and_positive_complete_results() {
        // Applied is the (overlap-retaining) sum of stored point estimates
        // on applied actions. Observed likewise retains overlap, but only for complete, positive
        // action-local verification results; dismissed, incomplete, and non-positive rows stay out.
        use serde_json::json;
        use tare_core::model::UsageTokens;
        let store = Store::open_in_memory().unwrap();
        let mk = |run: &str, model: &str, fresh: u64| {
            let mut step = priced_step(run, 1, model, "s");
            step.usage = UsageTokens {
                fresh_input: fresh,
                ..Default::default()
            };
            step
        };
        store
            .record_step(
                &mk("opus-before", "claude-opus-4-8", 10_000_000),
                "2026-06-13",
            )
            .unwrap();
        store
            .record_step(
                &mk("opus-after", "claude-opus-4-8", 1_000_000),
                "2026-06-17",
            )
            .unwrap();
        // Extends the captured range through the default seven-day after window. It also gives the
        // haiku action a complete negative result, proving non-positive observations are excluded.
        store
            .record_step(
                &mk("haiku-after", "claude-haiku-4-5", 1_000_000),
                "2026-06-22",
            )
            .unwrap();

        let opus = json!({"timezone":"UTC","entity":"run","filters":[{"op":"eq","dimension":"model","value":"claude-opus-4-8"}],
            "pricing":{"mode":"effective_dated"},"metric":"spend_micros","normalization":"absolute"});
        let mut positive: SavingsActionRequest = serde_json::from_value(json!({
            "opportunity_key":"cache:positive","cohort":opus,
            "match":{"kind":"aggregate_only"},"metric":"spend_micros",
            "normalization":"absolute","expected_point_micros":4000
        }))
        .unwrap();
        store
            .put_savings_action(&positive, "applied", "2026-06-15T00:00:00Z")
            .unwrap();

        // Same cohort and intervention under a distinct opportunity: the lifecycle totals retain
        // this overlap rather than pretending the action cohorts are disjoint.
        positive.opportunity_key = "cache:overlap".into();
        positive.expected_point_micros = Some(5_000);
        store
            .put_savings_action(&positive, "applied", "2026-06-15T00:00:00Z")
            .unwrap();

        let mut dismissed = positive.clone();
        dismissed.opportunity_key = "cache:dismissed".into();
        dismissed.expected_point_micros = Some(9_999);
        store
            .put_savings_action(&dismissed, "dismissed", "2026-06-15T00:00:00Z")
            .unwrap();

        let haiku = json!({"timezone":"UTC","entity":"run","filters":[{"op":"eq","dimension":"model","value":"claude-haiku-4-5"}],
            "pricing":{"mode":"effective_dated"},"metric":"spend_micros","normalization":"absolute"});
        let not_observed: SavingsActionRequest = serde_json::from_value(json!({
            "opportunity_key":"cache:not-observed","cohort":haiku,
            "match":{"kind":"aggregate_only"},"metric":"spend_micros",
            "normalization":"absolute","expected_point_micros":3000
        }))
        .unwrap();
        store
            .put_savings_action(&not_observed, "applied", "2026-06-15T00:00:00Z")
            .unwrap();

        let mut incomplete = positive.clone();
        incomplete.opportunity_key = "cache:incomplete".into();
        incomplete.expected_point_micros = Some(2_000);
        store
            .put_savings_action(&incomplete, "applied", "2026-06-22T00:00:00Z")
            .unwrap();

        let pricing = fixture_pricing();
        let direct = store
            .verify_savings_action(
                &SavingsVerifyRequest {
                    opportunity_key: "cache:positive".into(),
                    cohort_hash: positive.cohort.cohort_hash(),
                    as_of_date: None,
                    window_days: None,
                },
                &pricing,
            )
            .unwrap();
        assert!(direct.complete && direct.observed_reduction_micros > 0);
        assert_eq!(direct.status, "observed_reduction");

        let totals = store.savings_lifecycle_totals(&pricing).unwrap();
        assert_eq!(totals.applied_micros, 14_000); // 4k + 5k + 3k + 2k; dismissed excluded
        assert_eq!(
            totals.observed_micros,
            direct.observed_reduction_micros.saturating_mul(2),
            "both overlapping positive actions count; dismissed/non-positive/incomplete do not"
        );
    }

    #[test]
    fn savings_verification_excludes_unmatched_expensive_runs_from_spend() {
        // counts and spend MUST come from the same workload-key intersection. An
        // expensive unmatched before-only run cannot create a false observed reduction.
        use serde_json::json;
        use tare_core::model::UsageTokens;
        let store = Store::open_in_memory().unwrap();
        let mk = |run: &str, fresh: u64, workload: &str| {
            let mut step = priced_step(run, 1, "claude-opus-4-8", "s");
            step.usage = UsageTokens {
                fresh_input: fresh,
                ..Default::default()
            };
            step.shape.workload_key = Some(workload.into());
            step
        };

        // The like-for-like nightly unit is unchanged across the windows.
        store
            .record_step(&mk("matched-before", 1_000_000, "nightly"), "2026-06-13")
            .unwrap();
        store
            .record_step(&mk("matched-after", 1_000_000, "nightly"), "2026-06-17")
            .unwrap();
        // This before-only unit is 100x more expensive. Full-cohort arithmetic would falsely call
        // its disappearance an intervention reduction even though it is not the matched workload.
        store
            .record_step(
                &mk("unmatched-expensive", 100_000_000, "adhoc"),
                "2026-06-13",
            )
            .unwrap();

        let cohort = json!({"timezone":"UTC","entity":"run","filters":[{"op":"eq","dimension":"model","value":"claude-opus-4-8"}],
            "pricing":{"mode":"effective_dated"},"metric":"spend_micros","normalization":"absolute"});
        let action: SavingsActionRequest = serde_json::from_value(json!({
            "opportunity_key":"cache:nightly","cohort":cohort,
            "match":{"kind":"workload_key","key":"nightly"},
            "metric":"spend_micros","normalization":"absolute"
        }))
        .unwrap();
        store
            .put_savings_action(&action, "applied", "2026-06-15T00:00:00Z")
            .unwrap();
        let request: SavingsVerifyRequest = serde_json::from_value(json!({
            "opportunity_key":"cache:nightly","cohort_hash":action.cohort.cohort_hash(),
            "window_days":3,"as_of_date":"2026-06-30"
        }))
        .unwrap();
        let result = store
            .verify_savings_action(&request, &fixture_pricing())
            .unwrap();

        assert_eq!(
            result.selection_before_micros, result.selection_after_micros,
            "only the unchanged nightly workload belongs in verification spend"
        );
        assert_eq!(result.observed_reduction_micros, 0);
        assert_eq!(result.status, "not_observed");
        assert_eq!((result.matched_before, result.matched_after), (1, 1));
        assert_eq!((result.unmatched_before, result.unmatched_after), (1, 0));
        assert_eq!(result.baseline_matched_before, None);
        assert!(serde_json::to_value(&result).unwrap()["baseline_matched_before"].is_null());
    }

    #[test]
    fn savings_verification_requires_template_lineage_in_baseline_windows() {
        // when a baseline participates, the template-lineage intersection spans all
        // four windows. A lineage missing from baseline-after is excluded from EVERY denominator.
        use serde_json::json;
        use tare_core::model::UsageTokens;
        let store = Store::open_in_memory().unwrap();
        let common = 0xabcdu64;
        let other = 0xdef0u64;
        let mk = |run: &str, model: &str, lineage: u64| {
            let mut step = priced_step(run, 1, model, "s");
            step.usage = UsageTokens {
                fresh_input: 1_000_000,
                ..Default::default()
            };
            step.shape.system_hash = Some(lineage);
            step
        };
        store
            .record_step(&mk("sel-before", "claude-opus-4-8", common), "2026-06-13")
            .unwrap();
        store
            .record_step(&mk("sel-after", "claude-opus-4-8", common), "2026-06-17")
            .unwrap();
        store
            .record_step(&mk("base-before", "claude-haiku-4-5", common), "2026-06-13")
            .unwrap();
        store
            .record_step(&mk("base-after", "claude-haiku-4-5", other), "2026-06-17")
            .unwrap();

        let selection = json!({"timezone":"UTC","entity":"run","filters":[{"op":"eq","dimension":"model","value":"claude-opus-4-8"}],
            "pricing":{"mode":"effective_dated"},"metric":"spend_micros","normalization":"absolute"});
        let baseline = json!({"timezone":"UTC","entity":"run","filters":[{"op":"eq","dimension":"model","value":"claude-haiku-4-5"}],
            "pricing":{"mode":"effective_dated"},"metric":"spend_micros","normalization":"absolute"});
        let action: SavingsActionRequest = serde_json::from_value(json!({
            "opportunity_key":"template:common","cohort":selection,
            "baseline":{"kind":"explicit_cohort","label":"control","cohort":baseline},
            "match":{"kind":"template_lineage","hash":format!("{common:016x}")},
            "metric":"spend_micros","normalization":"absolute"
        }))
        .unwrap();
        store
            .put_savings_action(&action, "applied", "2026-06-15T00:00:00Z")
            .unwrap();
        let request: SavingsVerifyRequest = serde_json::from_value(json!({
            "opportunity_key":"template:common","cohort_hash":action.cohort.cohort_hash(),
            "window_days":3,"as_of_date":"2026-06-30"
        }))
        .unwrap();
        let result = store
            .verify_savings_action(&request, &fixture_pricing())
            .unwrap();

        assert_eq!(result.selection_before_micros, 0);
        assert_eq!(result.selection_after_micros, 0);
        assert_eq!(result.baseline_before_micros, Some(0));
        assert_eq!(result.baseline_after_micros, Some(0));
        assert_eq!((result.matched_before, result.matched_after), (0, 0));
        assert_eq!((result.unmatched_before, result.unmatched_after), (1, 1));
        assert_eq!(result.baseline_matched_before, Some(0));
        assert_eq!(result.baseline_matched_after, Some(0));
        assert_eq!(result.baseline_unmatched_before, Some(1));
        assert_eq!(result.baseline_unmatched_after, Some(1));
        assert!(result
            .compatibility_warnings
            .iter()
            .any(|warning| warning.contains("not present in every required verification window")));
    }

    #[test]
    fn savings_actions_lifecycle_collision_and_legacy_migration() {
        // Apply/dismiss/unaccept per (opportunity, cohort); 409 on collision or
        // incompatible transition; 404 on unaccept-missing; legacy acceptances migrate to aggregate-
        // only applied actions carrying the confounding warning.
        use serde_json::json;
        let store = Store::open_in_memory().unwrap();
        let req: SavingsActionRequest = serde_json::from_value(json!({
            "opportunity_key": "cache:opus",
            "cohort": {"timezone":"UTC","entity":"run","filters":[{"op":"eq","dimension":"model","value":"opus"}],
                       "pricing":{"mode":"effective_dated"},"metric":"spend_micros","normalization":"absolute"},
            "match": {"kind":"workload_key","key":"nightly"},
            "metric":"spend_micros","normalization":"absolute",
            "expected_point_micros": 4210
        }))
        .unwrap();

        let (code, _) = store
            .put_savings_action(&req, "unknown", "2026-07-10T00:00:00Z")
            .unwrap_err();
        assert_eq!(code, 400);
        let (code, _) = store
            .put_savings_action(&req, "applied", "not-a-timestamp")
            .unwrap_err();
        assert_eq!(code, 400);
        let mut unordered = req.clone();
        unordered.expected_low_micros = Some(500);
        unordered.expected_high_micros = Some(100);
        let (code, _) = store
            .put_savings_action(&unordered, "applied", "2026-07-10T00:00:00Z")
            .unwrap_err();
        assert_eq!(code, 400);

        let invalid_verification = SavingsVerifyRequest {
            opportunity_key: req.opportunity_key.clone(),
            cohort_hash: req.cohort.cohort_hash(),
            as_of_date: Some("2026-02-30".into()),
            window_days: Some(0),
        };
        let (code, _) = store
            .verify_savings_action(&invalid_verification, &fixture_pricing())
            .unwrap_err();
        assert_eq!(code, 400);

        // Apply → one row, no warning (workload_key match), round-trips the snapshot.
        store
            .put_savings_action(&req, "applied", "2026-07-10T00:00:00Z")
            .unwrap();
        let acts = store.list_savings_actions().unwrap();
        assert_eq!(acts.len(), 1);
        assert_eq!(acts[0].status, "applied");
        assert_eq!(acts[0].opportunity_key, "cache:opus");
        assert_eq!(acts[0].expected_point_micros, Some(4210));
        assert!(acts[0].compatibility_warnings.is_empty()); // workload_key ≠ aggregate-only
        let hash = req.cohort.cohort_hash();
        assert_eq!(acts[0].cohort_hash, hash);

        // Re-apply (same status) is idempotent.
        store
            .put_savings_action(&req, "applied", "2026-07-11T00:00:00Z")
            .unwrap();
        assert_eq!(store.list_savings_actions().unwrap().len(), 1);

        // Incompatible in-place transition (applied → dismissed) is a 409.
        let (code, _) = store
            .put_savings_action(&req, "dismissed", "2026-07-12T00:00:00Z")
            .unwrap_err();
        assert_eq!(code, 409);

        // Simulated cohort_hash COLLISION: a stored row under this hash but a DIFFERENT canonical
        // spec → put refuses with 409 rather than mutating the wrong row.
        store
            .conn
            .execute(
                "UPDATE savings_actions SET cohort_json = '{\"different\":true}' WHERE opportunity_key = 'cache:opus'",
                [],
            )
            .unwrap();
        let (code, msg) = store
            .put_savings_action(&req, "applied", "2026-07-13T00:00:00Z")
            .unwrap_err();
        assert_eq!(code, 409);
        assert!(msg.contains("collision"), "got {msg}");

        // Unaccept a missing action → 404; unaccept the real one → gone.
        let (code, _) = store
            .delete_savings_action(&SavingsActionIdentity {
                opportunity_key: "nope".into(),
                cohort_hash: "0".into(),
            })
            .unwrap_err();
        assert_eq!(code, 404);
        store
            .delete_savings_action(&SavingsActionIdentity {
                opportunity_key: "cache:opus".into(),
                cohort_hash: hash.clone(),
            })
            .unwrap();
        assert!(store.list_savings_actions().unwrap().is_empty());

        // Legacy migration: a savings_acceptance row → an aggregate-only applied action with the
        // confounding warning (no faked cohort snapshot).
        store
            .accept_savings("loop:search", "2026-06-20", 500_000)
            .unwrap();
        store.migrate_legacy_acceptances_if_migrated(0).unwrap();
        let migrated = store.list_savings_actions().unwrap();
        let legacy = migrated
            .iter()
            .find(|a| a.opportunity_key == "loop:search")
            .expect("migrated legacy acceptance");
        assert_eq!(legacy.status, "applied");
        assert_eq!(legacy.acted_at, "2026-06-20T00:00:00Z");
        assert!(matches!(
            legacy.match_rule,
            tare_core::cohort::MatchRule::AggregateOnly
        ));
        assert!(!legacy.compatibility_warnings.is_empty()); // explicit aggregate-only warning
                                                            // Idempotent: re-running the migration doesn't duplicate.
        store.migrate_legacy_acceptances_if_migrated(0).unwrap();
        assert_eq!(
            store
                .list_savings_actions()
                .unwrap()
                .iter()
                .filter(|a| a.opportunity_key == "loop:search")
                .count(),
            1
        );
    }

    #[test]
    fn saved_investigations_round_trip_and_enforce_caps() {
        // Upsert/list/delete round-trip; per-item 256 KiB and total 1 MiB caps;
        // the full DTO is preserved verbatim (so the client's focus-omission is what it stores).
        use serde_json::json;
        let store = Store::open_in_memory().unwrap();
        let inv = |id: &str, updated: &str| {
            json!({
                "id": id, "label": format!("view {id}"), "version": 2,
                "state": {"workspace": "investigate", "scope": {}, "match": {"kind": "aggregate_only"}},
                "pane_widths": {"canvas": 320},
                "created_at": "2026-07-01T00:00:00Z", "updated_at": updated,
            })
        };
        store
            .upsert_investigation(&inv("a", "2026-07-01T00:00:00Z"))
            .unwrap();
        store
            .upsert_investigation(&inv("b", "2026-07-02T00:00:00Z"))
            .unwrap();
        // Newest-updated first; the full DTO round-trips (including optional pane_widths + state).
        let list = store.list_investigations().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["id"], "b"); // b updated later → first
        assert_eq!(list[0]["pane_widths"]["canvas"], 320);
        assert_eq!(list[0]["state"]["workspace"], "investigate");
        assert!(
            list[0].get("focus").is_none(),
            "no transient focus is ever stored"
        );

        // Upsert (same id) replaces, not duplicates.
        store
            .upsert_investigation(&inv("a", "2026-07-03T00:00:00Z"))
            .unwrap();
        let list = store.list_investigations().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["id"], "a"); // a now newest

        // Missing required field → error, not a silent write.
        assert!(store.upsert_investigation(&json!({"label": "x"})).is_err());
        let mut unknown = inv("unknown", "2026-07-03T00:00:00Z");
        unknown["unexpected"] = json!(true);
        assert!(store.upsert_investigation(&unknown).is_err());
        let mut focused = inv("focused", "2026-07-03T00:00:00Z");
        focused["state"]["focus"] = json!("transient-row");
        assert!(store.upsert_investigation(&focused).is_err());
        let mut old_version = inv("old", "2026-07-03T00:00:00Z");
        old_version["version"] = json!(1);
        assert!(store.upsert_investigation(&old_version).is_err());

        // Per-item 256 KiB cap.
        let mut huge = inv("big", "2026-07-04T00:00:00Z");
        huge["state"]["padding"] = json!("x".repeat(300 * 1024));
        let e = store.upsert_investigation(&huge).unwrap_err();
        assert!(e.contains("256 KiB"), "got {e}");

        // Total 1 MiB cap: several ~200 KiB items eventually exceed the total.
        let mut hit_total_cap = false;
        for i in 0..8 {
            let mut big = inv(&format!("t{i}"), "2026-07-05T00:00:00Z");
            big["state"]["padding"] = json!("y".repeat(200 * 1024));
            if let Err(e) = store.upsert_investigation(&big) {
                assert!(e.contains("MiB total"), "got {e}");
                hit_total_cap = true;
                break;
            }
        }
        assert!(hit_total_cap, "the 1 MiB total cap must eventually reject");

        // Delete is idempotent.
        store.delete_investigation("a").unwrap();
        store.delete_investigation("a").unwrap();
        assert!(store
            .list_investigations()
            .unwrap()
            .iter()
            .all(|v| v["id"] != "a"));
    }

    #[test]
    fn experiment_grid_reprices_cohort_and_honors_quality_gate() {
        // The offline grid reprices the cohort's captured usage (no re-run),
        // is deterministic, and the quality gate filters the RUN SET by ingested score before gridding.
        use tare_core::experiment::{Axis, CostExperiment, ExperimentRequest, QualityConstraint};
        use tare_core::model::UsageTokens;
        let store = Store::open_in_memory().unwrap();
        let mut big = priced_step("big", 1, "claude-opus-4-8", "s");
        big.usage = UsageTokens {
            fresh_input: 10_000_000,
            ..Default::default()
        };
        let mut small = priced_step("small", 1, "claude-opus-4-8", "s");
        small.usage = UsageTokens {
            fresh_input: 1_000,
            ..Default::default()
        };
        store.record_step(&big, "2026-06-24").unwrap();
        store.record_step(&small, "2026-06-24").unwrap();
        store
            .set_run_quality("big", 40, "ci", "2026-06-24")
            .unwrap();
        store
            .set_run_quality("small", 95, "ci", "2026-06-24")
            .unwrap();
        let pricing = fixture_pricing();

        let exp = CostExperiment {
            axes: vec![Axis::Model(vec![
                Axis::AS_CAPTURED.into(),
                "claude-haiku-4-5".into(),
            ])],
        };
        let req = ExperimentRequest {
            cohort: cohort_base(CohortEntity::Run),
            experiment: exp,
            quality_constraint: None,
        };
        let r = store.experiment(&req, &pricing).unwrap();
        assert_eq!(r.cells.len(), 2, "as-captured + haiku"); // deterministic grid
        assert!(r.baseline_micros > 0);
        assert!(r.best_micros <= r.baseline_micros); // cheapest cell never exceeds baseline
                                                     // Determinism: same inputs → byte-identical result.
        assert_eq!(store.experiment(&req, &pricing).unwrap(), r);

        // Quality gate min=90 excludes "big" (scored 40) → a strictly smaller baseline run set.
        let gated = ExperimentRequest {
            quality_constraint: Some(QualityConstraint {
                min: Some(90),
                max: None,
            }),
            ..req.clone()
        };
        let rg = store.experiment(&gated, &pricing).unwrap();
        assert!(
            rg.baseline_micros < r.baseline_micros,
            "gating out the big low-quality run shrinks the baseline ({} vs {})",
            rg.baseline_micros,
            r.baseline_micros
        );
    }

    #[test]
    fn anomaly_why_resolves_scope_before_detection() {
        // The scope is resolved to its run set BEFORE detection. A huge spike
        // that lives ENTIRELY in an out-of-scope run must vanish when the scope excludes it — proving
        // pre-detection filtering, not a post-filter of already-detected anomaly rows.
        use tare_core::model::UsageTokens;
        let store = Store::open_in_memory().unwrap();
        let mk = |run: &str, fresh: u64, wk: &str| {
            let mut s = priced_step(run, 1, "claude-opus-4-8", "s");
            s.usage = UsageTokens {
                fresh_input: fresh,
                ..Default::default()
            };
            s.shape.workload_key = Some(wk.into());
            s
        };
        // Five flat, cheap baseline days — all in the "keep" workload.
        for d in [
            "2026-06-20",
            "2026-06-21",
            "2026-06-22",
            "2026-06-23",
            "2026-06-24",
        ] {
            store
                .record_step(&mk(&format!("keep-{d}"), 1_000, "keep"), d)
                .unwrap();
        }
        // Spike day: a tiny "keep" step + a HUGE "noise" step in a different workload.
        store
            .record_step(&mk("keep-spike", 1_000, "keep"), "2026-06-25")
            .unwrap();
        store
            .record_step(&mk("noise-spike", 80_000_000, "noise"), "2026-06-25")
            .unwrap();
        let pricing = fixture_pricing();

        let base = tare_core::anomaly::AnomalyWhyRequest {
            from: None,
            to: None,
            dimension: tare_core::anomaly::AnomalyDimension::Total,
            window: Some(5),
            threshold: Some(100),
            scope: None,
        };
        // Whole store: the huge noise step drives a detectable Total spike.
        let unscoped = store.anomaly_why(&base, &pricing).unwrap();
        assert!(
            !unscoped.is_empty(),
            "the noise spike is detected over the whole store"
        );

        // Scope to the "keep" workload: the noise run is excluded BEFORE detection, so no spike.
        let mut scope = cohort_base(CohortEntity::Run);
        scope.filters = vec![CohortFilter::Eq {
            dimension: CohortDimension::WorkloadKey,
            value: "keep".into(),
        }];
        let scoped_req = tare_core::anomaly::AnomalyWhyRequest {
            scope: Some(scope),
            ..base.clone()
        };
        let scoped = store.anomaly_why(&scoped_req, &pricing).unwrap();
        assert!(
            scoped.is_empty(),
            "scoping out the noise run removes the spike (resolved before detection), got {scoped:?}"
        );

        // `cause` dimension is not a step attribute → always empty (honest, not a guess).
        let cause_req = tare_core::anomaly::AnomalyWhyRequest {
            dimension: tare_core::anomaly::AnomalyDimension::Cause,
            ..base.clone()
        };
        assert!(store.anomaly_why(&cause_req, &pricing).unwrap().is_empty());
    }

    #[test]
    fn workload_key_dimension_and_match_round_trip() {
        // A workload_key materialized on steps is filterable as a dimension,
        // facetable, searchable, and usable as the compare match rule — end-to-end via the store.
        let store = Store::open_in_memory().unwrap();
        let mut a = priced_step("A", 1, "claude-opus-4-8", "alpha");
        a.shape.workload_key = Some("nightly-eval".into());
        let mut b = priced_step("B", 1, "claude-opus-4-8", "alpha");
        b.shape.workload_key = Some("nightly-eval".into());
        let c = priced_step("C", 1, "claude-opus-4-8", "alpha"); // no workload_key
        for s in [&a, &b, &c] {
            store.record_step(s, "2026-06-24").unwrap();
        }
        let pricing = fixture_pricing();

        // Filter by the workload_key dimension → only the two tagged runs.
        let mut spec = cohort_base(CohortEntity::Run);
        spec.filters = vec![CohortFilter::Eq {
            dimension: CohortDimension::WorkloadKey,
            value: "nightly-eval".into(),
        }];
        let r = store.resolve_cohort(&spec, &pricing).unwrap();
        assert_eq!(r.run_ids, vec!["A".to_string(), "B".to_string()]);

        // Facet over WorkloadKey exposes the value (the untagged run reports as missing, not folded).
        let base = cohort_base(CohortEntity::Run);
        let facet = store
            .facet_cohort(&base, &base, CohortDimension::WorkloadKey, &pricing)
            .unwrap();
        assert!(facet.rows.iter().any(|row| row.value == "nightly-eval"));

        // Search finds the key via the Label field.
        let hits = store
            .search_cohort(&base, "nightly", None, None, &pricing)
            .unwrap();
        assert_eq!(hits.entities.len(), 2);

        // Compare with a WorkloadKey match rule: the full cohort is 3 runs but only 2 carry the key,
        // so the warning reports the partial pairing (comparison uses the key only when available).
        let cmp = store
            .compare_cohort(
                &base,
                &base,
                &MatchRule::WorkloadKey {
                    key: "nightly-eval".into(),
                },
                &pricing,
            )
            .unwrap();
        assert!(cmp
            .compatibility_warnings
            .iter()
            .any(|w| w.contains("2 of 3")));
    }

    #[test]
    fn resolve_run_ids_and_step_refs_round_trip() {
        let (store, _a1, _a2, _b1) = seed_cohort_store();
        let pricing = fixture_pricing();
        // RunIds round-trips at Run grain.
        let mut s = cohort_base(CohortEntity::Run);
        s.filters = vec![CohortFilter::RunIds {
            ids: vec!["B".into()],
        }];
        assert_eq!(
            store.resolve_cohort(&s, &pricing).unwrap().run_ids,
            vec!["B".to_string()]
        );
        // StepRefs round-trips at Step grain.
        let mut s2 = cohort_base(CohortEntity::Step);
        s2.filters = vec![CohortFilter::StepRefs {
            refs: vec![StepRef {
                run_id: "A".into(),
                step_ordinal: 2,
            }],
        }];
        let r2 = store.resolve_cohort(&s2, &pricing).unwrap();
        assert_eq!(
            r2.step_refs,
            Some(vec![StepRef {
                run_id: "A".into(),
                step_ordinal: 2
            }])
        );
        assert_eq!(r2.step_count, 1);
        assert_eq!(r2.run_ids, vec!["A".to_string()]);
    }

    #[test]
    fn resolve_totals_use_scoped_contributions() {
        let (store, a1, a2, _b1) = seed_cohort_store();
        let pricing = fixture_pricing();
        let mut spec = cohort_base(CohortEntity::Run);
        spec.filters = vec![CohortFilter::Eq {
            dimension: CohortDimension::Model,
            value: "claude-opus-4-8".into(),
        }];
        let r = store.resolve_cohort(&spec, &pricing).unwrap();
        let a_row = r
            .entity_rows
            .iter()
            .find(|e| e.entity.run_id == "A")
            .unwrap();
        let a_whole = attribute::step_micros(&a1, &pricing, None)
            + attribute::step_micros(&a2, &pricing, None);
        assert_eq!(
            a_row.whole_entity_micros, a_whole,
            "whole run = opus + haiku"
        );
        assert_eq!(
            a_row.matched_micros,
            attribute::step_micros(&a1, &pricing, None),
            "scoped = opus only"
        );
        assert!(
            a_row.matched_micros < a_row.whole_entity_micros,
            "haiku step excluded from scoped total"
        );
        assert_eq!(a_row.matched_step_count, 1);
    }

    #[test]
    fn resolve_deterministic_error_rules() {
        let (store, _a1, _a2, _b1) = seed_cohort_store();
        let pricing = fixture_pricing();
        let mut bad_tz = cohort_base(CohortEntity::Run);
        bad_tz.timezone = "".into();
        assert!(
            store.resolve_cohort(&bad_tz, &pricing).is_err(),
            "empty timezone rejected"
        );
        let mut too_many = cohort_base(CohortEntity::Run);
        too_many.filters = (0..33)
            .map(|_| CohortFilter::Tag { value: "t".into() })
            .collect();
        assert!(
            store.resolve_cohort(&too_many, &pricing).is_err(),
            "over-many filters rejected"
        );
    }

    // ---- facet engine ----

    #[test]
    fn facet_multi_valued_shares_lift_and_zero_baseline() {
        // Selection = all runs (A: opus/alpha + haiku/beta, B: opus/alpha). Baseline = run B only.
        let (store, _a1, _a2, _b1) = seed_cohort_store();
        let pricing = fixture_pricing();
        let sel = cohort_base(CohortEntity::Run);
        let mut base = cohort_base(CohortEntity::Run);
        base.filters = vec![CohortFilter::RunIds {
            ids: vec!["B".into()],
        }];
        let r = store
            .facet_cohort(&sel, &base, CohortDimension::Session, &pricing)
            .unwrap();
        let alpha = r.rows.iter().find(|x| x.value == "alpha").unwrap();
        let beta = r.rows.iter().find(|x| x.value == "beta").unwrap();
        // Support = entity (run) count. alpha in A+B; beta in A only.
        assert_eq!((alpha.selection_support, beta.selection_support), (2, 1));
        // Shares over non-missing entities (2). Multi-valued: they need NOT sum to 100% (150% here).
        assert_eq!(alpha.selection_support_share_pct, 100.0);
        assert_eq!(beta.selection_support_share_pct, 50.0);
        assert!(alpha.selection_support_share_pct + beta.selection_support_share_pct > 100.0);
        // Baseline (run B) carries only alpha.
        assert_eq!(beta.baseline_support, 0);
        assert_eq!(beta.baseline_support_share_pct, 0.0);
        // Zero baseline support share -> lift omitted; use delta points instead.
        assert_eq!(beta.lift_ratio, None);
        assert_eq!(beta.delta_support_share_points, 50.0);
        assert_eq!(alpha.lift_ratio, Some(1.0)); // 100% / 100%
        assert_eq!(alpha.selection_missing_pct, 0.0);
        // Sorted by selection support desc (alpha before beta).
        assert!(r.rows.first().unwrap().value == "alpha");
    }

    #[test]
    fn facet_missing_is_separate_from_values() {
        // Run C carries a commit; run D does not. Faceting Commit: D is MISSING, never a value.
        let store = Store::open_in_memory().unwrap();
        let mut c = priced_step("C", 1, "claude-opus-4-8", "s");
        c.shape.commit = Some("abc".into());
        let d = priced_step("D", 1, "claude-opus-4-8", "s"); // no commit
        store.record_step(&c, "2026-06-24").unwrap();
        store.record_step(&d, "2026-06-24").unwrap();
        let pricing = fixture_pricing();
        let spec = cohort_base(CohortEntity::Run);
        let r = store
            .facet_cohort(&spec, &spec, CohortDimension::Commit, &pricing)
            .unwrap();
        assert_eq!(
            r.rows.len(),
            1,
            "only the real commit value, D is not a value"
        );
        let abc = &r.rows[0];
        assert_eq!(abc.value, "abc");
        assert_eq!(abc.selection_support, 1);
        assert_eq!(abc.selection_support_share_pct, 100.0); // 1 of 1 non-missing entity
        assert_eq!(abc.selection_missing_pct, 50.0); // 1 missing (D) of 2 total entities
    }

    #[test]
    fn resolve_cohort_agrees_with_shared_resolve_matched() {
        // Drift guard while resolve_cohort keeps its own loop (folding into resolve_matched is a
        // tracked cleanup): both must agree on scoped total + entity count for the same spec.
        let (store, _a1, _a2, _b1) = seed_cohort_store();
        let pricing = fixture_pricing();
        let mut spec = cohort_base(CohortEntity::Run);
        spec.filters = vec![CohortFilter::Eq {
            dimension: CohortDimension::Model,
            value: "claude-opus-4-8".into(),
        }];
        let rc = store.resolve_cohort(&spec, &pricing).unwrap();
        let rm = store.resolve_matched(&spec, &pricing).unwrap();
        let rm_total: i64 = rm
            .iter()
            .flat_map(|e| e.steps.iter().map(|s| s.micros))
            .sum();
        assert_eq!(rc.total_micros, rm_total);
        assert_eq!(rc.run_count as usize, rm.len()); // Run grain: one entity per matched run
    }

    // ---- cohort compare + decomposition ----

    #[test]
    fn compare_decomposition_closes_exactly_and_matches_anomaly_arithmetic() {
        let (store, _a1, _a2, _b1) = seed_cohort_store();
        let pricing = fixture_pricing();
        let sel = cohort_base(CohortEntity::Run); // A + B
        let mut base = cohort_base(CohortEntity::Run);
        base.filters = vec![CohortFilter::RunIds {
            ids: vec!["B".into()],
        }];
        let r = store
            .compare_cohort(&sel, &base, &MatchRule::AggregateOnly, &pricing)
            .unwrap();
        // The three levers sum EXACTLY to the total delta (integer decomposition closes).
        assert_eq!(
            r.volume_delta_micros + r.size_delta_micros + r.efficiency_delta_micros,
            r.total_delta_micros
        );
        // …and equal the shared anomaly arithmetic on the same aggregates (reuse, not re-derivation).
        let sel_ents = store.resolve_matched(&sel, &pricing).unwrap();
        let base_ents = store.resolve_matched(&base, &pricing).unwrap();
        let why = tare_core::anomaly::decompose_change(
            "cohort",
            dayagg_of(&sel_ents),
            dayagg_of(&base_ents),
        );
        assert_eq!(r.total_delta_micros, why.total_delta_micros);
        assert_eq!(r.volume_delta_micros, why.volume_micros);
        assert_eq!(r.size_delta_micros, why.size_micros);
        assert_eq!(r.efficiency_delta_micros, why.efficiency_micros);
        // Selection (A+B) outspends the single-run baseline.
        assert!(r.total_delta_micros > 0);
        assert_eq!(r.selection.run_count, 2);
        assert_eq!(r.baseline.run_count, 1);
    }

    #[test]
    fn compare_warns_on_aggregate_and_pricing_mismatch() {
        let (store, _a1, _a2, _b1) = seed_cohort_store();
        let pricing = fixture_pricing();
        let sel = cohort_base(CohortEntity::Run); // pricing = Latest
        let mut base = cohort_base(CohortEntity::Run);
        base.filters = vec![CohortFilter::RunIds {
            ids: vec!["B".into()],
        }];
        // aggregate_only always confounds.
        let r = store
            .compare_cohort(&sel, &base, &MatchRule::AggregateOnly, &pricing)
            .unwrap();
        assert!(r
            .compatibility_warnings
            .iter()
            .any(|w| w.contains("aggregate-only")));
        // Different pricing modes warn.
        let mut base_eff = base.clone();
        base_eff.pricing = PricingMode::EffectiveDated;
        let r2 = store
            .compare_cohort(&sel, &base_eff, &MatchRule::AggregateOnly, &pricing)
            .unwrap();
        assert!(r2
            .compatibility_warnings
            .iter()
            .any(|w| w.contains("pricing mode differs")));
    }

    #[test]
    fn compare_warns_on_fidelity_mismatch() {
        // Selection has an UNPRICED (local) step; baseline is all priced → fidelity warning.
        let store = Store::open_in_memory().unwrap();
        let mut u = priced_step("U", 1, "llama-local", "s");
        u.provider = Provider::Local;
        u.shape.provider = Provider::Local;
        let p = priced_step("P", 1, "claude-opus-4-8", "s");
        store.record_step(&u, "2026-06-24").unwrap();
        store.record_step(&p, "2026-06-24").unwrap();
        let pricing = fixture_pricing();
        let sel = cohort_base(CohortEntity::Run); // U (unpriced) + P
        let mut base = cohort_base(CohortEntity::Run);
        base.filters = vec![CohortFilter::RunIds {
            ids: vec!["P".into()],
        }]; // priced only
        let r = store
            .compare_cohort(&sel, &base, &MatchRule::AggregateOnly, &pricing)
            .unwrap();
        assert!(r
            .compatibility_warnings
            .iter()
            .any(|w| w.contains("fidelity")));
    }

    #[test]
    fn compare_warns_when_quality_provenance_or_match_coverage_differs() {
        let store = Store::open_in_memory().unwrap();
        let mut a = priced_step("A", 1, "claude-opus-4-8", "s");
        a.shape.workload_key = Some("nightly".into());
        let b = priced_step("B", 1, "claude-opus-4-8", "s");
        store.record_step(&a, "2026-06-24").unwrap();
        store.record_step(&b, "2026-06-24").unwrap();
        store.set_run_quality("A", 90, "ci", "2026-06-24").unwrap();
        let pricing = fixture_pricing();
        let mut selection = cohort_base(CohortEntity::Run);
        selection.filters = vec![CohortFilter::RunIds {
            ids: vec!["A".into()],
        }];
        let mut baseline = cohort_base(CohortEntity::Run);
        baseline.filters = vec![CohortFilter::RunIds {
            ids: vec!["B".into()],
        }];
        let result = store
            .compare_cohort(
                &selection,
                &baseline,
                &MatchRule::WorkloadKey {
                    key: "nightly".into(),
                },
                &pricing,
            )
            .unwrap();
        assert!(result
            .compatibility_warnings
            .iter()
            .any(|warning| warning.contains("baseline entities carry the key")));
        assert!(result
            .compatibility_warnings
            .iter()
            .any(|warning| warning.contains("quality provenance differs")));

        // Equal quality coverage still warns when the recorded source provenance differs. Scores
        // themselves are deliberately irrelevant to compatibility.
        store.set_run_quality("B", 12, "cli", "2026-06-24").unwrap();
        let source_result = store
            .compare_cohort(
                &selection,
                &baseline,
                &MatchRule::WorkloadKey {
                    key: "nightly".into(),
                },
                &pricing,
            )
            .unwrap();
        assert!(source_result
            .compatibility_warnings
            .iter()
            .any(|warning| warning.contains("quality provenance differs")));

        store.set_run_quality("B", 1, "ci", "2026-06-24").unwrap();
        let same_provenance = store
            .compare_cohort(
                &selection,
                &baseline,
                &MatchRule::WorkloadKey {
                    key: "nightly".into(),
                },
                &pricing,
            )
            .unwrap();
        assert!(!same_provenance
            .compatibility_warnings
            .iter()
            .any(|warning| warning.contains("quality provenance differs")));
    }

    // ---- cross-run cohort search ----

    #[test]
    fn search_matches_id_prefix_label_and_model_scoped_to_cohort() {
        let (store, _a1, _a2, _b1) = seed_cohort_store();
        let pricing = fixture_pricing();
        let all = cohort_base(CohortEntity::Run);
        // Label substring: "beta" only tags run A (its haiku step's session).
        let r = store
            .search_cohort(&all, "beta", Some(&[SearchField::Label]), None, &pricing)
            .unwrap();
        assert_eq!(
            r.entities
                .iter()
                .map(|e| e.run_id.clone())
                .collect::<Vec<_>>(),
            vec!["A".to_string()]
        );
        assert!(!r.truncated);
        // Id prefix.
        let r2 = store
            .search_cohort(&all, "A", Some(&[SearchField::Id]), None, &pricing)
            .unwrap();
        assert_eq!(r2.entities.len(), 1);
        assert_eq!(r2.entities[0].run_id, "A");
        // Model substring: only A has haiku.
        let r3 = store
            .search_cohort(&all, "haiku", Some(&[SearchField::Model]), None, &pricing)
            .unwrap();
        assert_eq!(
            r3.entities
                .iter()
                .map(|e| e.run_id.clone())
                .collect::<Vec<_>>(),
            vec!["A".to_string()]
        );
        // Scoped: within only run B, A's "beta" session is not searchable.
        let mut only_b = cohort_base(CohortEntity::Run);
        only_b.filters = vec![CohortFilter::RunIds {
            ids: vec!["B".into()],
        }];
        let r4 = store
            .search_cohort(&only_b, "beta", Some(&[SearchField::Label]), None, &pricing)
            .unwrap();
        assert!(r4.entities.is_empty());
    }

    #[test]
    fn search_caps_results_and_flags_truncation() {
        let (store, _a1, _a2, _b1) = seed_cohort_store();
        let pricing = fixture_pricing();
        let all = cohort_base(CohortEntity::Run);
        // "opus" matches both A and B; a limit of 1 truncates.
        let r = store
            .search_cohort(&all, "opus", Some(&[SearchField::Model]), Some(1), &pricing)
            .unwrap();
        assert_eq!(r.entities.len(), 1);
        assert!(r.truncated);
        let r2 = store
            .search_cohort(&all, "opus", Some(&[SearchField::Model]), None, &pricing)
            .unwrap();
        assert_eq!(r2.entities.len(), 2);
        assert!(!r2.truncated);
        assert_eq!(SEARCH_RESULT_CAP, 200);
    }

    #[test]
    fn search_is_field_scoped_and_never_payload() {
        let (store, _a1, _a2, _b1) = seed_cohort_store();
        let pricing = fixture_pricing();
        let all = cohort_base(CohortEntity::Run);
        // "opus" IS in the model, but searching only Note/Tag (none set) finds nothing — field
        // scoping. The searchable field set is closed (no payload option).
        let r = store
            .search_cohort(
                &all,
                "opus",
                Some(&[SearchField::Note, SearchField::Tag]),
                None,
                &pricing,
            )
            .unwrap();
        assert!(r.entities.is_empty());
        assert_eq!(SearchField::ALL.len(), 7);
        // Empty query matches nothing.
        assert!(store
            .search_cohort(&all, "", None, None, &pricing)
            .unwrap()
            .entities
            .is_empty());
    }

    #[test]
    fn search_runs_on_the_calibrated_bench_fixture() {
        let store = Store::open_in_memory().unwrap();
        let bench = tare_core::calibrated_bench::build();
        for sr in &bench.runs {
            for s in &sr.run.steps {
                store
                    .record_step_with_time(
                        s,
                        &sr.date,
                        sr.hour,
                        None,
                        Some(sr.profile),
                        Some(sr.source),
                    )
                    .unwrap();
            }
        }
        let pricing = fixture_pricing();
        let all = cohort_base(CohortEntity::Run);
        let r = store
            .search_cohort(
                &all,
                "sess-refactor",
                Some(&[SearchField::Label]),
                None,
                &pricing,
            )
            .unwrap();
        assert!(!r.entities.is_empty(), "finds the session across runs");
        assert!(r.entities.len() <= SEARCH_RESULT_CAP);
    }

    #[test]
    fn runs_sig_tracks_run_step_writes_but_ignores_unrelated_table_churn() {
        // the raw-run cache signature must change when a run/step lands, but NOT when
        // only an unrelated table (scan_cursor) is written — that idle-sweep WAL churn used to dump
        // the ~37MB cache every ~3s. Uses a file-backed db because runs_sig probes the path itself.
        use tare_core::ingest_step;
        let path = std::env::temp_dir().join(format!("tare-runssig-{}.db", std::process::id()));
        let p = path.to_str().unwrap();
        for sfx in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{p}{sfx}"));
        }
        let store = Store::open(p).unwrap();
        let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");

        let sig_empty = runs_sig(p);
        let s1 = ingest_step("run-a", 1, Provider::Anthropic, req, resp).unwrap();
        store.record_step(&s1, "2026-06-24").unwrap();
        let sig_after_step = runs_sig(p);
        assert_ne!(
            sig_empty, sig_after_step,
            "a new run/step must invalidate the cache"
        );

        // Unrelated-table write (scan_cursor): under WAL the main db file is untouched and runs/steps
        // MAX(rowid) is unchanged, so the signature must NOT move — the core of the H3 fix.
        store
            .save_scan_cursor(&[ScanCursorRow {
                path: "x.jsonl".into(),
                mtime: 1,
                size: 2,
                offset: 0,
            }])
            .unwrap();
        assert_eq!(
            sig_after_step,
            runs_sig(p),
            "an unrelated-table write must not invalidate the run cache"
        );

        // Another step still moves the signature (steps MAX(rowid) advances even before a checkpoint).
        let s2 = ingest_step("run-a", 2, Provider::Anthropic, req, resp).unwrap();
        store.record_step(&s2, "2026-06-24").unwrap();
        assert_ne!(
            sig_after_step,
            runs_sig(p),
            "a further step must invalidate the cache again"
        );

        drop(store);
        for sfx in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{p}{sfx}"));
        }
    }

    #[test]
    fn run_day_hours_persists_the_hour_and_leaves_hourless_lanes_a_gap() {
        // the JSONL lane stamps the hour from the turn timestamp; lanes without one
        // (or pre-migration rows) stay NULL — an honest GAP the punchcard excludes.
        let store = Store::open_in_memory().unwrap();
        let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");
        let a = ingest_step("run-a", 1, Provider::Anthropic, req, resp).unwrap();
        let b = ingest_step("run-b", 1, Provider::Anthropic, req, resp).unwrap();
        store
            .record_step_with_time(&a, "2026-07-06", Some(9), None, None, Some("jsonl"))
            .unwrap();
        store
            .record_step_with_time(&b, "2026-07-06", None, None, None, Some("otel"))
            .unwrap();
        let dh = store.run_day_hours().unwrap();
        let hour_of = |id: &str| dh.iter().find(|(r, _, _)| r == id).unwrap().2;
        assert_eq!(hour_of("run-a"), Some(9));
        assert_eq!(hour_of("run-b"), None);
    }

    #[test]
    fn read_paths_reconcile_over_a_multi_week_seed() {
        // an O(n) correctness/regression guard over a multi-week dataset — substitutes for
        // the flaky wall-clock <100ms budget, catching range/trend read regressions headlessly.
        let store = Store::open_in_memory().unwrap();
        let pricing = fixture_pricing();
        let seeded = tare_core::demo::seed_weeks("2026-06-01", 2, 3); // 2×7×3 = 42 dated runs
        for (date, run) in &seeded {
            for step in &run.steps {
                store.record_step(step, date).unwrap();
            }
        }
        // Full window returns every run (boundaries inclusive).
        let all = store
            .load_dated_runs_in_range("2026-06-01", "2026-06-14")
            .unwrap();
        assert_eq!(all.len(), seeded.len());
        // A window before the data is empty — honest, not a panic.
        assert!(store
            .load_dated_runs_in_range("2026-05-01", "2026-05-31")
            .unwrap()
            .is_empty());
        // A seeded day has positive spend; the trend spans all 14 days.
        assert!(
            store
                .today_spend("2026-06-05", &pricing)
                .unwrap()
                .total_micros
                > 0
        );
        let tr = store
            .trend_in_range("2026-06-01", "2026-06-14", &pricing, TrendDimension::Total)
            .unwrap();
        assert_eq!(tr.days.len(), 14);
        // Per-day totals reconcile to a direct whole-window recompute (no double-count across the range).
        let per_day_sum: i64 = tr.series.iter().flat_map(|s| s.per_day.iter()).sum();
        let runs: Vec<_> = all.iter().map(|d| d.run.clone()).collect();
        let window_total = tare_core::attribute::build_report(&runs, &pricing).total_micros;
        assert_eq!(per_day_sum, window_total);
        assert!(window_total > 0);
    }

    #[test]
    fn run_meta_reports_provenance_and_distinct_dims() {
        let store = Store::open_in_memory().unwrap();
        let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");
        let s1 = ingest_step("run-m", 1, Provider::Anthropic, req, resp).unwrap();
        let s2 = ingest_step("run-m", 2, Provider::Anthropic, req, resp).unwrap();
        store
            .record_step_with_policy(
                &s1,
                "2026-06-24",
                Some("policy-x"),
                Some("strict_counts"),
                Some("proxy"),
            )
            .unwrap();
        store
            .record_step_with_policy(
                &s2,
                "2026-06-24",
                Some("policy-x"),
                Some("strict_counts"),
                Some("otel-event"),
            )
            .unwrap();

        let m = store.run_meta("run-m").unwrap().expect("run exists");
        assert_eq!(m.created_date, "2026-06-24");
        assert_eq!(m.profile.as_deref(), Some("strict_counts"));
        assert_eq!(m.privacy_policy_id.as_deref(), Some("policy-x"));
        assert_eq!(m.steps, 2);
        assert_eq!(m.providers, vec!["anthropic".to_string()]);
        // Distinct capture sources, sorted.
        assert_eq!(
            m.sources,
            vec!["otel-event".to_string(), "proxy".to_string()]
        );
        assert!(!m.models.is_empty());
        // Unknown run id -> None.
        assert!(store.run_meta("nope").unwrap().is_none());
    }

    #[test]
    fn coverage_by_source_counts_steps_and_last_day_per_source() {
        let store = Store::open_in_memory().unwrap();
        let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");
        let s1 = ingest_step("run-p", 1, Provider::Anthropic, req, resp).unwrap();
        let s2 = ingest_step("run-p", 2, Provider::Anthropic, req, resp).unwrap();
        let s3 = ingest_step("run-o", 1, Provider::Anthropic, req, resp).unwrap();
        store
            .record_step_with_policy(&s1, "2026-06-24", None, None, Some("proxy"))
            .unwrap();
        store
            .record_step_with_policy(&s2, "2026-06-25", None, None, Some("proxy"))
            .unwrap();
        store
            .record_step_with_policy(&s3, "2026-06-26", None, None, Some("otel-event"))
            .unwrap();
        let cov = store.coverage_by_source().unwrap();
        let proxy = cov.iter().find(|(s, _, _)| s == "proxy").unwrap();
        assert_eq!(proxy.1, 2, "two proxy steps");
        // last_day is the owning run's created_date (set on first insert), so both proxy steps in
        // run-p report run-p's creation day.
        assert_eq!(proxy.2, "2026-06-24");
        let otel = cov.iter().find(|(s, _, _)| s == "otel-event").unwrap();
        assert_eq!((otel.1, otel.2.as_str()), (1, "2026-06-26"));
    }

    #[test]
    fn persists_and_round_trips_an_openai_compatible_vendor_label() {
        // an OpenAI-compatible step carries a vendor label; it must survive write+read.
        // This guards the SHAPE_KEY_ALLOWLIST "vendor" entry — without it record_step rejects it.
        let store = Store::open_in_memory().unwrap();
        let req = include_bytes!("../../fixtures/openai_nonstream/request.json");
        let resp = include_bytes!("../../fixtures/openai_nonstream/response.json");
        let mut s1 = ingest_step("run-oai", 1, Provider::OpenAiCompatible, req, resp).unwrap();
        s1.shape.vendor = Some("groq".into());
        store.record_step(&s1, "2026-06-24").unwrap();
        let runs = store.load_runs().unwrap();
        assert_eq!(runs[0].steps[0].shape.vendor.as_deref(), Some("groq"));
        assert_eq!(runs[0].steps[0].provider, Provider::OpenAiCompatible);
    }

    #[test]
    fn backfill_ledger_dedups_and_flags_foreign_sessions() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.backfilled_keys().unwrap().is_empty());
        store
            .mark_backfilled(&[
                ("id:m1|r1".into(), "sess-a".into()),
                ("id:m2|r2".into(), "sess-a".into()),
            ])
            .unwrap();
        // Idempotent: re-marking the same key doesn't duplicate.
        store
            .mark_backfilled(&[("id:m1|r1".into(), "sess-a".into())])
            .unwrap();
        let keys = store.backfilled_keys().unwrap();
        assert_eq!(keys.len(), 2);
        assert!(keys.contains("id:m1|r1"));
    }

    #[test]
    fn savings_acceptance_round_trips_and_purges() {
        // accept snapshots date + estimate; re-accept replaces; delete purges.
        let store = Store::open_in_memory().unwrap();
        store
            .accept_savings("loop:search", "2026-06-20", 500_000)
            .unwrap();
        store
            .accept_savings("cache:m", "2026-06-21", 1_200_000)
            .unwrap();
        let all = store.savings_acceptances().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].opportunity_key, "cache:m", "key-ordered");
        assert_eq!(all[1].recoverable_micros, 500_000);
        // Re-accept replaces the snapshot, not duplicates.
        store
            .accept_savings("loop:search", "2026-06-25", 400_000)
            .unwrap();
        let again = store.savings_acceptances().unwrap();
        assert_eq!(again.len(), 2);
        let loop_acc = again
            .iter()
            .find(|a| a.opportunity_key == "loop:search")
            .unwrap();
        assert_eq!(loop_acc.accepted_date, "2026-06-25");
        assert_eq!(loop_acc.recoverable_micros, 400_000);
        store.unaccept_savings("loop:search").unwrap();
        assert_eq!(store.savings_acceptances().unwrap().len(), 1);
    }

    #[test]
    fn run_quality_upsert_load_all_and_delete() {
        // a user-supplied quality scalar round-trips, replaces on re-set, and purges.
        let store = Store::open_in_memory().unwrap();
        store.set_run_quality("r1", 92, "ci", "100").unwrap();
        store.set_run_quality("r2", 40, "cli", "101").unwrap();
        assert!(store.set_run_quality("bad-low", -1, "cli", "101").is_err());
        assert!(store.set_run_quality("bad-high", 101, "ci", "101").is_err());
        assert!(store
            .set_run_quality("bad-source", 50, "import", "101")
            .is_err());
        let q = store.run_quality("r1").unwrap().unwrap();
        assert_eq!(q.score, 92);
        assert_eq!(q.source, "ci");
        assert!(store.run_quality("missing").unwrap().is_none());
        // Re-set replaces (INSERT OR REPLACE), not duplicates.
        store.set_run_quality("r1", 95, "cli", "102").unwrap();
        assert_eq!(store.run_quality("r1").unwrap().unwrap().score, 95);
        let all = store.all_run_quality().unwrap();
        assert_eq!(all.len(), 2, "one row per run");
        assert_eq!(all[0].run_id, "r1", "run_id-ordered");
        store.delete_run_quality("r1").unwrap();
        assert!(store.run_quality("r1").unwrap().is_none());
        assert_eq!(store.all_run_quality().unwrap().len(), 1);
    }

    #[test]
    fn run_notes_upsert_load_filter_and_delete() {
        // notes round-trip; tag/star filters; length caps; delete/purge.
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_run_note(
                "r1",
                "[\"baseline\",\"known-bug\"]",
                "reviewed",
                true,
                "100",
            )
            .unwrap();
        store
            .upsert_run_note("r2", "[\"known-bug\"]", "", false, "101")
            .unwrap();
        let n = store.load_run_note("r1").unwrap().unwrap();
        assert_eq!(n.tags, vec!["baseline", "known-bug"]);
        assert!(n.starred);
        assert_eq!(n.note_text, "reviewed");
        assert!(store.load_run_note("missing").unwrap().is_none());
        // Tag membership is exact (both runs carry known-bug; only r1 is baseline).
        assert_eq!(store.notes_by_tag("known-bug").unwrap().len(), 2);
        assert_eq!(store.notes_by_tag("baseline").unwrap(), vec!["r1"]);
        assert_eq!(store.list_starred().unwrap(), vec!["r1"]);
        // Length caps are enforced (hard error, not truncation).
        assert!(store
            .upsert_run_note("r3", "[]", &"x".repeat(9000), false, "102")
            .is_err());
        assert!(store
            .upsert_run_note("r3", "not-json", "", false, "102")
            .is_err());
        assert!(store
            .upsert_run_note("r3", "[\"dup\",\"dup\"]", "", false, "102")
            .is_err());
        assert!(store
            .upsert_run_note("r3", "[\"bad tag\"]", "", false, "102")
            .is_err());
        // Upsert overwrites; delete purges.
        store
            .upsert_run_note("r1", "[]", "changed", false, "103")
            .unwrap();
        assert!(!store.load_run_note("r1").unwrap().unwrap().starred);
        store.delete_run_note("r1").unwrap();
        assert!(store.load_run_note("r1").unwrap().is_none());
    }

    #[test]
    fn persists_and_round_trips_per_step_latency() {
        // a step's observed duration_ms must survive write + read (the v10->v11 column).
        let store = Store::open_in_memory().unwrap();
        let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");
        let mut s1 = ingest_step("run-lat", 1, Provider::Anthropic, req, resp).unwrap();
        s1.duration_ms = 1234;
        s1.start_unix_nano = Some(tare_core::model::UnixNanos(1_234_567_890));
        s1.trace_id = Some("trace".into());
        s1.span_id = Some("span".into());
        s1.parent_span_id = Some("parent".into());
        store.record_step(&s1, "2026-06-24").unwrap();
        let runs = store.load_runs().unwrap();
        assert_eq!(runs[0].steps[0].duration_ms, 1234);
        // recent_steps reads through its own SELECT/mapping — guard it too.
        let tail = store.recent_steps(1).unwrap();
        assert_eq!(tail[0].duration_ms, 1234);
        assert_eq!(tail[0].start_unix_nano, s1.start_unix_nano);
        assert_eq!(tail[0].trace_id, s1.trace_id);
        assert_eq!(tail[0].span_id, s1.span_id);
        assert_eq!(tail[0].parent_span_id, s1.parent_span_id);

        store
            .conn
            .execute("UPDATE steps SET model = 'corrupt'", [])
            .unwrap();
        assert!(store.recent_steps(1).is_err());
    }

    #[test]
    fn persists_and_round_trips_an_mcp_server_shape_label() {
        // an OTel-captured step carrying mcp_server must survive write+read. This guards
        // the SHAPE_KEY_ALLOWLIST entry — without it, record_step rejects the shape on write.
        let store = Store::open_in_memory().unwrap();
        let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");
        let mut s1 = ingest_step("run-mcp", 1, Provider::Anthropic, req, resp).unwrap();
        s1.shape.mcp_server = Some("github-mcp".into());
        store.record_step(&s1, "2026-06-24").unwrap();
        let runs = store.load_runs().unwrap();
        let loaded = &runs[0].steps[0];
        assert_eq!(loaded.shape.mcp_server.as_deref(), Some("github-mcp"));
    }

    #[test]
    fn recent_steps_are_newest_first_across_runs() {
        let store = Store::open_in_memory().unwrap();
        let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");
        // Three steps inserted in order; the tail must return them newest-first by insertion id,
        // flat across runs (not regrouped per run like load_runs).
        let s1 = ingest_step("run-a", 1, Provider::Anthropic, req, resp).unwrap();
        let s2 = ingest_step("run-a", 2, Provider::Anthropic, req, resp).unwrap();
        let s3 = ingest_step("run-b", 1, Provider::Anthropic, req, resp).unwrap();
        store.record_step(&s1, "2026-06-24").unwrap();
        store.record_step(&s2, "2026-06-24").unwrap();
        store.record_step(&s3, "2026-06-24").unwrap();

        let tail = store.recent_steps(2).unwrap();
        assert_eq!(tail.len(), 2, "limit is honored");
        assert_eq!(
            (tail[0].run_id.as_str(), tail[0].step_ordinal),
            ("run-b", 1)
        );
        assert_eq!(
            (tail[1].run_id.as_str(), tail[1].step_ordinal),
            ("run-a", 2)
        );
        // Round-trips the full StepRecord (usage + shape), same as load_run.
        assert_eq!(tail[1], s2);
    }

    #[test]
    fn records_and_counts_capture_source() {
        let store = Store::open_in_memory().unwrap();
        let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");
        let s1 = ingest_step("run-otel", 1, Provider::Anthropic, req, resp).unwrap();
        let s2 = ingest_step("run-proxy", 1, Provider::Anthropic, req, resp).unwrap();
        store
            .record_step_with_policy(&s1, "2026-06-24", None, None, Some("otel-event"))
            .unwrap();
        store
            .record_step_with_policy(&s2, "2026-06-24", None, None, Some("proxy"))
            .unwrap();
        let counts = store.source_counts().unwrap();
        assert_eq!(counts.get("otel-event"), Some(&1));
        assert_eq!(counts.get("proxy"), Some(&1));
        // `record_step` (no source) lands as NULL -> "unknown".
        let s3 = ingest_step("run-x", 1, Provider::Anthropic, req, resp).unwrap();
        store.record_step(&s3, "2026-06-24").unwrap();
        assert_eq!(store.source_counts().unwrap().get("unknown"), Some(&1));
    }

    #[test]
    fn session_activity_beats_upsert_and_load() {
        let path = std::env::temp_dir().join(format!("tare-act-{}.db", std::process::id()));
        let db = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        let store = Store::open(&db).unwrap();
        store
            .record_session_beat("otel-event", "sess-1", 1000, "claude-opus-4-8")
            .unwrap();
        store
            .record_session_beat("otel-event", "sess-1", 1005, "claude-opus-4-8")
            .unwrap();
        store
            .record_session_beat("otel-span", "sess-2", 1002, "gpt-5")
            .unwrap();
        let rows = store.load_session_activity().unwrap();
        assert_eq!(rows.len(), 2);
        let s1 = rows.iter().find(|r| r.1 == "sess-1").unwrap();
        assert_eq!(s1.2, 1005, "last_unix updated to the latest beat");
        assert_eq!(s1.3, 2, "events incremented across beats (upsert)");
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn metered_series_sums_deltas_and_totals_by_day() {
        let path = std::env::temp_dir().join(format!("tare-met-{}.db", std::process::id()));
        let db = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        let store = Store::open(&db).unwrap();
        let p = |metric: &str, kind: &str, value: i64| tare_core::otel::MeteredPoint {
            day: "2026-06-29".into(),
            metric: metric.into(),
            model: "claude-opus-4-8".into(),
            kind: kind.into(),
            session: "sess-1".into(),
            effort: String::new(),
            query_source: String::new(),
            value,
        };
        // Two cost deltas on the same series sum; tokens tracked separately by day.
        store.record_metered(&p("cost", "", 150_000)).unwrap();
        store.record_metered(&p("cost", "", 100_000)).unwrap();
        store.record_metered(&p("token", "input", 1200)).unwrap();
        store.record_metered(&p("token", "output", 300)).unwrap();
        let (cost, tokens) = store.metered_totals("2026-06-29").unwrap();
        assert_eq!(cost, 250_000, "delta cost summed -> $0.25");
        assert_eq!(tokens, 1500, "input + output tokens summed");
        // A different day is isolated.
        assert_eq!(store.metered_totals("2026-06-28").unwrap(), (0, 0));

        // metered_by_model: cost rows group by model; tokens excluded. A second model's
        // cost lands as its own row.
        store
            .record_metered(&tare_core::otel::MeteredPoint {
                day: "2026-06-29".into(),
                metric: "cost".into(),
                model: "claude-haiku-4-5".into(),
                kind: "".into(),
                session: "sess-2".into(),
                effort: String::new(),
                query_source: String::new(),
                value: 40_000,
            })
            .unwrap();
        let by_model = store.metered_by_model("2026-06-29").unwrap();
        assert_eq!(by_model.len(), 2, "two models with cost");
        assert_eq!(
            by_model[0],
            ("claude-opus-4-8".into(), 250_000),
            "biggest first"
        );
        assert_eq!(by_model[1], ("claude-haiku-4-5".into(), 40_000));
        assert!(store.metered_by_model("2026-06-28").unwrap().is_empty());

        // points differing ONLY in effort/query_source are distinct rows (not collapsed),
        // so spend stays sliceable; the day total still sums both.
        let mk = |effort: &str, qs: &str, value: i64| tare_core::otel::MeteredPoint {
            day: "2026-06-29".into(),
            metric: "cost".into(),
            model: "claude-opus-4-8".into(),
            kind: "".into(),
            session: "sess-9".into(),
            effort: effort.into(),
            query_source: qs.into(),
            value,
        };
        store.record_metered(&mk("high", "main", 7_000)).unwrap();
        store.record_metered(&mk("low", "main", 3_000)).unwrap();
        store.record_metered(&mk("high", "main", 1_000)).unwrap(); // same key as #1 -> sums
                                                                   // opus day total was 250_000; +11_000 across the three new cost points.
        assert_eq!(store.metered_by_model("2026-06-29").unwrap()[0].1, 261_000);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn metered_outcomes_aggregates_counters_over_a_window() {
        let path = std::env::temp_dir().join(format!("tare-out-{}.db", std::process::id()));
        let db = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        let store = Store::open(&db).unwrap();
        let p = |metric: &str, kind: &str, day: &str, value: i64| tare_core::otel::MeteredPoint {
            day: day.into(),
            metric: metric.into(),
            model: "m".into(),
            kind: kind.into(),
            session: "s".into(),
            effort: String::new(),
            query_source: String::new(),
            value,
        };
        store
            .record_metered(&p("cost", "", "2026-06-29", 12_000_000))
            .unwrap();
        store
            .record_metered(&p("pull_request", "", "2026-06-29", 3))
            .unwrap();
        store
            .record_metered(&p("lines_of_code", "added", "2026-06-29", 2000))
            .unwrap();
        store
            .record_metered(&p("lines_of_code", "removed", "2026-06-29", 500))
            .unwrap();
        store
            .record_metered(&p("edit_decision", "accept", "2026-06-29", 9))
            .unwrap();
        store
            .record_metered(&p("edit_decision", "reject", "2026-06-29", 1))
            .unwrap();
        store
            .record_metered(&p("cost", "", "2026-07-01", 99_000_000))
            .unwrap(); // out of window
                       // A nested subagent's spend must NOT inflate the numerator. Distinct row via
                       // the query_source PK column; the unstamped ('') cost above is still counted.
        store
            .record_metered(&tare_core::otel::MeteredPoint {
                day: "2026-06-29".into(),
                metric: "cost".into(),
                model: "m".into(),
                kind: String::new(),
                session: "s".into(),
                effort: String::new(),
                query_source: "subagent".into(),
                value: 5_000_000,
            })
            .unwrap();

        let o = store.metered_outcomes("2026-06-01", "2026-06-30").unwrap();
        assert_eq!(
            o.cost_micros, 12_000_000,
            "subagent spend excluded; unstamped ('') cost kept"
        );
        assert_eq!(o.pull_requests, 3);
        assert_eq!(o.lines_added, 2000, "only the 'added' kind");
        assert_eq!(o.edits_accepted, 9);
        assert_eq!(o.edits_rejected, 1);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn recent_run_ids_returns_newest_first_capped() {
        let store = Store::open_in_memory().unwrap();
        let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");
        for run in ["run-a", "run-b", "run-c"] {
            let step = ingest_step(run, 1, Provider::Anthropic, req, resp).unwrap();
            store.record_step(&step, "2026-06-24").unwrap();
        }
        // Newest persisted first, capped at n.
        assert_eq!(store.recent_run_ids(2).unwrap(), vec!["run-c", "run-b"]);
        assert_eq!(
            store.recent_run_ids(10).unwrap(),
            vec!["run-c", "run-b", "run-a"]
        );
        assert!(store.recent_run_ids(0).unwrap().is_empty());
    }

    #[test]
    fn outcomes_by_day_groups_cost_and_outcomes_per_day() {
        let path = std::env::temp_dir().join(format!("tare-obd-{}.db", std::process::id()));
        let db = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        let store = Store::open(&db).unwrap();
        let p = |metric: &str, kind: &str, day: &str, value: i64| tare_core::otel::MeteredPoint {
            day: day.into(),
            metric: metric.into(),
            model: "m".into(),
            kind: kind.into(),
            session: "s".into(),
            effort: String::new(),
            query_source: String::new(),
            value,
        };
        store
            .record_metered(&p("cost", "", "2026-06-28", 2_000_000))
            .unwrap();
        store
            .record_metered(&p("commit", "", "2026-06-28", 2))
            .unwrap();
        store
            .record_metered(&p("cost", "", "2026-06-29", 6_000_000))
            .unwrap();
        store
            .record_metered(&p("commit", "", "2026-06-29", 1))
            .unwrap();
        store
            .record_metered(&p("edit_decision", "accept", "2026-06-29", 4))
            .unwrap();
        store
            .record_metered(&p("edit_decision", "reject", "2026-06-29", 1))
            .unwrap();
        // A day outside the window is excluded.
        store
            .record_metered(&p("cost", "", "2026-07-05", 9_000_000))
            .unwrap();
        // Subagent spend on an in-window day is excluded — must not inflate 06-28.
        store
            .record_metered(&tare_core::otel::MeteredPoint {
                day: "2026-06-28".into(),
                metric: "cost".into(),
                model: "m".into(),
                kind: String::new(),
                session: "s".into(),
                effort: String::new(),
                query_source: "subagent".into(),
                value: 8_000_000,
            })
            .unwrap();

        let days = store.outcomes_by_day("2026-06-01", "2026-06-30").unwrap();
        assert_eq!(days.len(), 2, "two in-window days");
        assert_eq!(days[0].day, "2026-06-28");
        assert_eq!(days[0].cost_micros, 2_000_000);
        assert_eq!(days[0].commits, 2);
        assert_eq!(days[0].edits_accepted, 0);
        assert_eq!(days[1].day, "2026-06-29");
        assert_eq!(days[1].cost_micros, 6_000_000);
        assert_eq!(days[1].commits, 1);
        assert_eq!(
            days[1].edits_accepted, 4,
            "only the 'accept' kind, not 'reject'"
        );
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn migrates_v7_db_to_v8_adds_session_activity() {
        let path = std::env::temp_dir().join(format!("tare-mig8-{}.db", std::process::id()));
        let db = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch("CREATE TABLE schema_version (version INTEGER NOT NULL);")
                .unwrap();
            for m in &MIGRATIONS[..7] {
                conn.execute_batch(m).unwrap();
            }
            conn.execute_batch("INSERT INTO schema_version(version) VALUES (7);")
                .unwrap();
        }
        let store = Store::open(&db).unwrap();
        let v: i64 = store
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
        // The new table exists and is usable on an upgraded DB.
        store
            .record_session_beat("otel-event", "s", 42, "m")
            .unwrap();
        assert_eq!(store.load_session_activity().unwrap().len(), 1);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn migrates_v6_db_to_v7_adds_audio_columns_preserving_rows() {
        let path = std::env::temp_dir().join(format!("tare-mig7-{}.db", std::process::id()));
        let db = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        {
            // Hand-build a schema_version=6 DB (through v6: steps has `source`, no audio columns).
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch("CREATE TABLE schema_version (version INTEGER NOT NULL);")
                .unwrap();
            for m in &MIGRATIONS[..6] {
                conn.execute_batch(m).unwrap();
            }
            conn.execute_batch(
                "INSERT INTO runs(run_id, created_date) VALUES ('r1', '2026-06-24');
                 INSERT INTO steps(run_id, step_ordinal, provider, model, fresh_input,
                     cache_write_5m, cache_write_1h, cache_read, output, reasoning, shape_json)
                 VALUES ('r1', 1, 'anthropic', 'claude-opus-4-8', 10,0,0,0,5,0,
                     '{\"model\":\"claude-opus-4-8\",\"provider\":\"anthropic\",\"stream\":false,\"ttl\":\"five_min\",\"has_cache_control\":false,\"weights\":[],\"request_hash\":0}');
                 INSERT INTO schema_version(version) VALUES (6);",
            )
            .unwrap();
        }
        let store = Store::open(&db).unwrap();
        let v: i64 = store
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
        // The pre-existing v6 row survives; its (new) audio axes default to 0.
        let runs = store.load_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].steps[0].usage.audio_input, 0);
        assert_eq!(runs[0].steps[0].usage.audio_output, 0);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn migrates_v1_db_to_v2_preserving_rows() {
        let path = std::env::temp_dir().join(format!("tare-mig-{}.db", std::process::id()));
        let db = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        // Hand-build a schema_version=1 DB (base schema, one row, NO indexes).
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch("CREATE TABLE schema_version (version INTEGER NOT NULL);")
                .unwrap();
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            conn.execute_batch("INSERT INTO schema_version(version) VALUES (1);")
                .unwrap();
            conn.execute(
                "INSERT INTO runs(run_id, created_date) VALUES ('r','2026-06-24')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO steps(run_id, step_ordinal, provider, model, fresh_input, cache_write_5m, cache_write_1h, cache_read, output, reasoning, shape_json, stop_reason)
                 VALUES ('r',1,'anthropic','m',10,0,0,0,5,0,
                   '{\"model\":\"m\",\"provider\":\"anthropic\",\"stream\":false,\"ttl\":\"five_min\",\"has_cache_control\":false,\"weights\":[],\"request_hash\":0}', NULL)",
                [],
            )
            .unwrap();
        }
        // Opening upgrades v1 -> latest idempotently (v2 indexes + v3 privacy columns).
        let store = Store::open(&db).unwrap();
        let v: i64 = store
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
        let has_idx: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_steps_run'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_idx, 1, "v2 index must exist after upgrade");
        // v3 privacy columns exist and default to NULL for pre-existing rows.
        let null_policy: Option<String> = store
            .conn
            .query_row(
                "SELECT privacy_policy_id FROM runs WHERE run_id='r'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(null_policy, None);
        // Pre-existing row preserved.
        let runs = store.load_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].steps[0].usage.output, 5);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn refuses_to_open_a_db_from_a_newer_tare() {
        let path = std::env::temp_dir().join(format!("tare-fwd-{}.db", std::process::id()));
        let db = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        // Hand-stamp a schema_version one PAST what this build knows (as if written by a newer tare).
        let future = (MIGRATIONS.len() + 1) as i64;
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch("CREATE TABLE schema_version (version INTEGER NOT NULL);")
                .unwrap();
            conn.execute("INSERT INTO schema_version(version) VALUES (?1)", [future])
                .unwrap();
        }
        let err = match Store::open(&db) {
            Ok(_) => panic!("expected a forward-version refusal, but open succeeded"),
            Err(e) => e,
        };
        assert!(
            err.contains("newer than this build supports"),
            "expected a forward-version refusal, got: {err}"
        );
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn open_creates_missing_parent_dirs() {
        // The desktop default (~/.tare/tare.db) lands in a dir that may not exist yet.
        let base = std::env::temp_dir().join(format!("tare-nested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let db = base.join("a").join("b").join("tare.db");
        let s = Store::open(&db.to_string_lossy()).unwrap();
        // Usable immediately after creation.
        assert!(s.run_date_bounds().unwrap().is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn open_sets_wal_and_a_busy_timeout_for_concurrent_access() {
        // The store's concurrency model (daemon writes + CLI reads, multiple processes on one file)
        // relies on WAL + a non-zero busy_timeout so a locked DB waits-and-retries instead of erroring
        // with SQLITE_BUSY. Lock the config so a future refactor can't silently drop it.
        let path = std::env::temp_dir().join(format!("tare-pragma-{}.db", std::process::id()));
        let db = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        let s = Store::open(&db).unwrap();
        let journal: String = s
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            journal.to_ascii_lowercase(),
            "wal",
            "on-disk store uses WAL"
        );
        let timeout_ms: i64 = s
            .conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            timeout_ms, 5000,
            "a locked DB waits rather than erroring immediately"
        );
        drop(s);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{db}{suffix}"));
        }
    }

    #[test]
    fn scan_cursor_round_trips_upserts_and_prunes() {
        // the persisted incremental-catch-up cursor. Rows round-trip, upsert on the
        // path PK (offset advances, no duplicate), and prune drops files no longer on disk.
        let s = Store::open_in_memory().unwrap();
        assert!(s.load_scan_cursor().unwrap().is_empty());
        let rows = vec![
            ScanCursorRow {
                path: "a.jsonl".into(),
                mtime: 111,
                size: 20,
                offset: 18,
            },
            ScanCursorRow {
                path: "b.jsonl".into(),
                mtime: 222,
                size: 40,
                offset: 40,
            },
        ];
        s.save_scan_cursor(&rows).unwrap();
        let mut loaded = s.load_scan_cursor().unwrap();
        loaded.sort_by(|x, y| x.path.cmp(&y.path));
        assert_eq!(loaded, rows, "rows round-trip verbatim");
        // Upsert: same path, advanced size/offset → replace in place, still 2 rows total.
        s.save_scan_cursor(&[ScanCursorRow {
            path: "a.jsonl".into(),
            mtime: 111,
            size: 25,
            offset: 25,
        }])
        .unwrap();
        let all = s.load_scan_cursor().unwrap();
        assert_eq!(all.len(), 2, "upsert on the path PK, never a duplicate");
        let a = all.iter().find(|r| r.path == "a.jsonl").unwrap();
        assert_eq!((a.size, a.offset), (25, 25), "offset advanced in place");
        // Prune: keep only a.jsonl → b.jsonl (deleted on disk) is dropped.
        let keep: std::collections::BTreeSet<String> =
            ["a.jsonl".to_string()].into_iter().collect();
        assert_eq!(s.prune_scan_cursor(&keep).unwrap(), 1);
        let after = s.load_scan_cursor().unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].path, "a.jsonl");

        assert!(s
            .save_scan_cursor(&[ScanCursorRow {
                path: "past-end.jsonl".into(),
                mtime: 1,
                size: 5,
                offset: 6,
            }])
            .is_err());
        assert!(s
            .save_scan_cursor(&[ScanCursorRow {
                path: "overflow.jsonl".into(),
                mtime: 1,
                size: u64::MAX,
                offset: 0,
            }])
            .is_err());
        assert_eq!(s.load_scan_cursor().unwrap(), after);
    }

    #[test]
    fn fired_alert_set_claims_each_key_once() {
        // The daemon uses fire-once dedup: a second claim of the
        // same (date, series_key, kind) key returns false, so an anomaly fires exactly once.
        let path = std::env::temp_dir().join(format!("tare-fired-{}.db", std::process::id()));
        let db = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        let store = Store::open(&db).unwrap();
        let key = "anomaly:2026-06-24:anthropic/claude-opus-4-8:Spike";
        assert!(!store.alert_already_fired(key).unwrap());
        assert!(
            store.claim_alert(key, "2026-06-24").unwrap(),
            "first claim wins"
        );
        assert!(store.alert_already_fired(key).unwrap());
        assert!(
            !store.claim_alert(key, "2026-06-24").unwrap(),
            "second claim of the same key must not fire"
        );
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn migrates_v4_db_to_v5_adds_fired_alerts() {
        // The v5 fired-alerts table is added to an existing v4 DB without touching prior rows.
        let path = std::env::temp_dir().join(format!("tare-mig5-{}.db", std::process::id()));
        let db = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        {
            // Hand-build a schema_version=4 DB (all migrations through v4, NO fired_alerts).
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch("CREATE TABLE schema_version (version INTEGER NOT NULL);")
                .unwrap();
            for m in &MIGRATIONS[..4] {
                conn.execute_batch(m).unwrap();
            }
            conn.execute_batch("INSERT INTO schema_version(version) VALUES (4);")
                .unwrap();
        }
        let store = Store::open(&db).unwrap();
        let v: i64 = store
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
        // The new table is usable after the upgrade.
        assert!(store.claim_alert("k", "2026-06-24").unwrap());
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn migrates_v5_db_to_v6_adds_source_column_preserving_rows() {
        // The v6 `source` column is added to an existing v5 DB; a pre-existing step survives and
        // reads as 'unknown' (NULL -> COALESCE), and new steps can carry a source tag.
        let path = std::env::temp_dir().join(format!("tare-mig6-{}.db", std::process::id()));
        let db = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        {
            // Hand-build a schema_version=5 DB (all migrations through v5, steps has NO source).
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch("CREATE TABLE schema_version (version INTEGER NOT NULL);")
                .unwrap();
            for m in &MIGRATIONS[..5] {
                conn.execute_batch(m).unwrap();
            }
            conn.execute_batch(
                "INSERT INTO runs(run_id, created_date) VALUES ('r1', '2026-06-24');
                 INSERT INTO steps(run_id, step_ordinal, provider, model, fresh_input,
                     cache_write_5m, cache_write_1h, cache_read, output, reasoning, shape_json)
                 VALUES ('r1', 1, 'anthropic', 'claude-opus-4-8', 10,0,0,0,5,0, '{}');
                 INSERT INTO schema_version(version) VALUES (5);",
            )
            .unwrap();
        }
        let store = Store::open(&db).unwrap();
        let v: i64 = store
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
        // The pre-existing v5 row survives and its (new) source reads as 'unknown'.
        let counts = store.source_counts().unwrap();
        assert_eq!(counts.get("unknown"), Some(&1));
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn no_payload_text_in_db() {
        // Plant unique sentinel strings in the request payload, ingest and record under the default
        // policy, then read the entire SQLite file and assert that none of them appear. The unique
        // tokens cannot collide with structural field names or enum values.
        const SYS: &str = "SENTINEL_SYS_a1b2c3d4e5f6";
        const USER: &str = "SENTINEL_USER_9f8e7d6c5b4a";
        const TOOL: &str = "SENTINEL_TOOL_0a1b2c3d4e5f";
        let req = format!(
            r#"{{"model":"claude-opus-4-8","system":"{SYS}","messages":[
                {{"role":"user","content":"{USER}"}},
                {{"role":"user","content":[{{"type":"tool_result","content":"{TOOL}"}}]}}
            ]}}"#
        );
        let resp = br#"{"usage":{"input_tokens":42,"output_tokens":7},"stop_reason":"end_turn"}"#;

        let path = std::env::temp_dir().join(format!("tare-sentinel-{}.db", std::process::id()));
        let db = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        {
            let store = Store::open(&db).unwrap();
            let step =
                ingest_step("sentinel", 1, Provider::Anthropic, req.as_bytes(), resp).unwrap();
            // Sanity: the step DID capture usage (so we know text actually flowed through).
            assert_eq!(step.usage.fresh_input, 42);
            store.record_step(&step, "2026-06-24").unwrap();
        }
        let bytes = std::fs::read(&db).unwrap();
        for needle in [SYS, USER, TOOL] {
            let n = needle.as_bytes();
            let leaked = bytes.windows(n.len()).any(|w| w == n);
            assert!(
                !leaked,
                "payload sentinel {needle:?} leaked into the DB file"
            );
        }
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn new_text_column_is_rejected() {
        // A shape carrying a key outside the allowlist (e.g. a smuggled prompt excerpt) is
        // refused at write time, and nothing is inserted.
        assert!(check_shape_allowlist(
            r#"{"model":"m","provider":"anthropic","stream":false,"ttl":"five_min","has_cache_control":false,"weights":[],"request_hash":0}"#
        )
        .is_ok());
        let err = check_shape_allowlist(
            r#"{"model":"m","provider":"anthropic","prompt_excerpt":"secret user text"}"#,
        );
        assert!(err.is_err(), "extra key must be rejected");
        assert!(err.unwrap_err().contains("prompt_excerpt"));
    }

    #[test]
    fn unknown_provider_row_errors_known_rows_load() {
        // Known providers parse; an unknown tag is an error rather than silently becoming Anthropic.
        assert_eq!(provider_from_str("anthropic"), Ok(Provider::Anthropic));
        assert_eq!(provider_from_str("openai"), Ok(Provider::Openai));
        assert_eq!(provider_from_str("gemini"), Ok(Provider::Gemini));
        assert!(provider_from_str("martian").is_err());

        let store = Store::open_in_memory().unwrap();
        store
            .conn
            .execute(
                "INSERT INTO runs(run_id, created_date) VALUES ('r','2026-06-24')",
                [],
            )
            .unwrap();
        store.conn.execute(
            "INSERT INTO steps(run_id, step_ordinal, provider, model, fresh_input, cache_write_5m, cache_write_1h, cache_read, output, reasoning, shape_json, stop_reason)
             VALUES ('r',1,'martian','m',1,0,0,0,1,0,'{\"model\":\"m\",\"provider\":\"anthropic\",\"stream\":false,\"ttl\":\"five_min\",\"has_cache_control\":false,\"weights\":[],\"request_hash\":0}',NULL)",
            [],
        ).unwrap();
        assert!(
            store.load_runs().is_err(),
            "corrupt provider must surface as error"
        );
    }

    #[test]
    fn load_run_filters_to_one_run() {
        let store = Store::open_in_memory().unwrap();
        let req = include_bytes!("../../fixtures/anthropic_nonstream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_nonstream/response.json");
        for (rid, ord) in [("a", 1u32), ("a", 2), ("b", 1)] {
            let s = ingest_step(rid, ord, Provider::Anthropic, req, resp).unwrap();
            store.record_step(&s, "2026-06-24").unwrap();
        }
        let a = store.load_run("a").unwrap().expect("run a");
        assert_eq!(a.run_id, "a");
        assert_eq!(a.steps.len(), 2);
        assert!(store.load_run("missing").unwrap().is_none());
        // Identical to the old load-all-then-filter result.
        let via_all = store
            .load_runs()
            .unwrap()
            .into_iter()
            .find(|r| r.run_id == "a")
            .unwrap();
        assert_eq!(a, via_all);
    }

    #[test]
    fn dated_range_queries_are_inclusive_and_ordered() {
        let store = Store::open_in_memory().unwrap();
        let req = include_bytes!("../../fixtures/openai_nonstream/request.json");
        let resp = include_bytes!("../../fixtures/openai_nonstream/response.json");
        for (rid, date) in [
            ("a", "2026-06-20"),
            ("b", "2026-06-22"),
            ("c", "2026-06-25"),
        ] {
            let s = ingest_step(rid, 1, Provider::Openai, req, resp).unwrap();
            store.record_step(&s, date).unwrap();
        }
        assert_eq!(
            store.run_date_bounds().unwrap(),
            Some(("2026-06-20".into(), "2026-06-25".into()))
        );
        // Inclusive window [20, 22] picks a and b, not c.
        let dated = store
            .load_dated_runs_in_range("2026-06-20", "2026-06-22")
            .unwrap();
        let mut ids: Vec<_> = dated.iter().map(|d| d.run.run_id.clone()).collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b"]);
        // Empty window.
        assert!(store
            .load_dated_runs_in_range("2026-07-01", "2026-07-05")
            .unwrap()
            .is_empty());
        // Trend over the window has a dense axis and non-zero total.
        let pricing = fixture_pricing();
        let t = store
            .trend_in_range("2026-06-20", "2026-06-22", &pricing, TrendDimension::Total)
            .unwrap();
        assert_eq!(t.days.len(), 3);
        assert!(t.series[0].total_micros > 0);

        assert!(store
            .load_dated_runs_in_range("2026-02-30", "2026-06-22")
            .is_err());
        assert!(store
            .load_dated_runs_in_range("2026-06-22", "2026-06-20")
            .is_err());
        assert!(store.load_runs_on_date("not-a-date").is_err());
        assert!(store
            .resolve_trend_window(Some("2026-06-25"), Some("2026-06-20"))
            .is_err());
        assert!(store.metered_outcomes("2026-06-22", "2026-06-20").is_err());
        assert!(store.outcomes_by_day("2026-06-22", "2026-06-20").is_err());
        assert!(store.metered_totals("2026-02-30").is_err());
        assert!(store.metered_by_model("2026-02-30").is_err());
    }

    #[test]
    fn today_filter_excludes_older_runs() {
        let store = Store::open_in_memory().unwrap();
        let pricing = fixture_pricing();

        let r1 = include_bytes!("../../fixtures/openai_nonstream/request.json");
        let s1 = include_bytes!("../../fixtures/openai_nonstream/response.json");
        let step_today = ingest_step("today-run", 1, Provider::Openai, r1, s1).unwrap();
        store.record_step(&step_today, "2026-06-24").unwrap();

        let r2 = include_bytes!("../../fixtures/anthropic_nonstream/request.json");
        let s2 = include_bytes!("../../fixtures/anthropic_nonstream/response.json");
        let step_old = ingest_step("old-run", 1, Provider::Anthropic, r2, s2).unwrap();
        store.record_step(&step_old, "2026-06-01").unwrap();

        // --today filters by date.
        let today = store.today_spend("2026-06-24", &pricing).unwrap();
        assert_eq!(today.run_count, 1);
        let only_today = store.load_runs_on_date("2026-06-24").unwrap();
        let expected = attribute::today_spend(&only_today, &pricing);
        assert_eq!(today, expected);
    }

    #[test]
    fn vendor_session_cost_upserts_latest_never_sums() {
        // statusLine reports a CUMULATIVE total each call; the store keeps the latest per
        // session, never accumulating (so 100 invocations of a $2 session read $2, not $200).
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_vendor_session_cost("sess-a", 2_000_000, "2026-06-24")
            .unwrap();
        store
            .upsert_vendor_session_cost("sess-a", 2_340_000, "2026-06-24")
            .unwrap(); // later, higher
        store
            .upsert_vendor_session_cost("sess-b", 500_000, "2026-06-24")
            .unwrap();
        let costs = store.vendor_session_costs().unwrap();
        assert_eq!(
            costs,
            vec![
                ("sess-a".to_string(), 2_340_000), // latest wins, not 2M+2.34M
                ("sess-b".to_string(), 500_000),
            ]
        );
    }
}
