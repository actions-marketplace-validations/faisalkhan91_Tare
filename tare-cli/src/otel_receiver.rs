//! Loopback OTLP/JSON receiver. Operational setup lives in `docs/MESH_USAGE.md`.
//!
//! A second listener that `tare serve` runs alongside the proxy/UI, accepting *out-of-band*
//! OpenTelemetry exports (Claude Code, Codex, SDKs) and recording them through the SAME step sink
//! as the proxy — so capture never sits in the model request path and coexists with whatever
//! `ANTHROPIC_BASE_URL`/proxy the user already has.
//!
//! `/v1/traces` reuses the byte-stable GenAI span ingest; `/v1/logs` maps Claude Code api_request
//! events to steps (+ user_prompt/tool_result liveness pulses); `/v1/metrics` maps Claude Code's
//! own cost/token counters into a separate metered series for a vendor cross-check, never cost
//! steps. The listener binds loopback only.

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Request, State},
    http::StatusCode,
    middleware::{from_fn, Next},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tare_core::model::StepRecord;
use tare_core::PrivacyPolicy;
use tare_proxy::Sink;

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn lock_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Liveness counters the Connect screen reads via `/__tare/otlp_status`, so the user can see
/// capture is working. Shared between the receiver (writer) and the proxy's read handler (reader).
#[derive(Default)]
pub struct OtlpStatus {
    events: AtomicU64,
    last_unix: AtomicU64,
    /// Whether the OTLP receiver actually bound its port. Defaults to false (fail-safe: report
    /// not-listening until proven bound) and is set true only on a successful bind, so the Connect
    /// screen can't falsely claim the receiver is listening after a bind failure.
    listening: AtomicBool,
}

impl OtlpStatus {
    fn record(&self, n: u64) {
        if n == 0 {
            return;
        }
        let _ = self
            .events
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(n))
            });
        self.last_unix.fetch_max(now_unix(), Ordering::Relaxed);
    }

    /// Mark whether the receiver's port bind succeeded — the truthful `listening` signal.
    pub fn set_listening(&self, live: bool) {
        self.listening.store(live, Ordering::Relaxed);
    }

    /// Whether the receiver actually bound its port (real, not assumed).
    pub fn is_listening(&self) -> bool {
        self.listening.load(Ordering::Relaxed)
    }

    /// `(total events captured, unix seconds of the last event or 0)`.
    pub fn snapshot(&self) -> (u64, u64) {
        (
            self.events.load(Ordering::Relaxed),
            self.last_unix.load(Ordering::Relaxed),
        )
    }
}

/// Liveness windows (seconds). Recency is the only signal either CLI gives us — neither emits a
/// session-end event or idle heartbeat, so "running" is a window rather than a fact.
const WORKING_WINDOW_S: u64 = 15;
const IDLE_WINDOW_S: u64 = 300; // 5 min; older than this is treated as ended

/// Classify a session's liveness purely from how long since its last event.
fn classify_state(age_s: u64) -> &'static str {
    if age_s <= WORKING_WINDOW_S {
        "working"
    } else if age_s <= IDLE_WINDOW_S {
        "idle"
    } else {
        "ended"
    }
}

/// One running/recent session as seen by the receiver (counts-only; no payload).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionLive {
    pub session: String,
    /// Capture source: `otel-span` | `otel-event`.
    pub source: String,
    /// `working` | `idle` | `ended`. Fused from hook lifecycle > live process >
    /// recency (`last_seen_age_s`), so an authoritative signal overrides the recency `ended` guess.
    pub state: String,
    pub last_seen_age_s: u64,
    pub events: u64,
    pub last_model: String,
    /// Lifetime estimated spend for this session (micro-USD). The receiver has no pricing, so it
    /// leaves 0; the read API / desktop enrich it from the store so the live list can show $ and
    /// float by cost ("rises to the top as it spends").
    #[serde(default)]
    pub micros: i64,
    /// Latest activity kind: `responded` after a cost step, or `user_prompt` /
    /// `tool_result` from a non-cost liveness pulse — lets the UI show "working" vs "waiting on
    /// user" beyond the recency-only `state`. Empty when only reseeded from the durable mirror.
    #[serde(default)]
    pub phase: String,
}

#[derive(Clone)]
struct Beat {
    last_unix: u64,
    events: u64,
    last_model: String,
    phase: String,
}

/// Per-`(source, session)` liveness table: updated on every captured step so the read
/// API can answer "which agent sessions are running now?" A fresh event REVIVES an idle/ended
/// bucket (handles Claude Code resume / Codex conversation reuse). In-memory by default; the
/// always-on daemon persists it. Counts-only: session ids + counts, never payload.
#[derive(Default)]
pub struct SessionActivity {
    beats: Mutex<BTreeMap<(String, String), Beat>>,
    /// Hook-health: per hook-event-name `(last_fired_unix, count)`. Lets the UI
    /// distinguish "hooks wired and firing" from "registered but silent" (a committed
    /// `disableAllHooks` / enterprise policy / never-connected) — otherwise a disabled hook reads as
    /// mysteriously-missing lifecycle data rather than a fixable warning.
    hook_health: Mutex<BTreeMap<String, (u64, u64)>>,
}

/// One hook event's health: how long since it last fired and how many have arrived.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HookHealth {
    pub event: String,
    pub last_unix: u64,
    pub count: u64,
}

impl SessionActivity {
    /// Beat each session touched by a just-sinked batch tagged with `source`. Session identity is
    /// the step's `shape.session` (Claude Code session.id / Codex conversation id), falling back to
    /// the run id so proxy/SDK steps still appear.
    fn record(&self, source: &'static str, recs: &[StepRecord]) {
        if recs.is_empty() {
            return;
        }
        let now = now_unix();
        let mut g = lock_recover(&self.beats);
        for r in recs {
            let session = r.shape.session.clone().unwrap_or_else(|| r.run_id.clone());
            let b = g.entry((source.to_string(), session)).or_insert(Beat {
                last_unix: 0,
                events: 0,
                last_model: String::new(),
                phase: String::new(),
            });
            b.last_unix = now; // a fresh event revives an idle/ended bucket
            b.events = b.events.saturating_add(1);
            b.phase = "responded".to_string(); // a cost step = the model just produced output
            if !r.model.is_empty() {
                b.last_model = r.model.clone();
            }
        }
    }

    /// Beat each session named by a non-cost liveness pulse (Claude Code user_prompt / tool_result),
    /// refreshing its recency and `phase` WITHOUT counting a cost event or model. This is what lets
    /// the Live view show "agent working" (a tool just ran) vs "waiting on user", and keeps a
    /// long tool turn from decaying to `idle` between api_request events.
    fn record_pulses(&self, source: &'static str, pulses: &[tare_core::otel::LivenessPulse]) {
        if pulses.is_empty() {
            return;
        }
        let now = now_unix();
        let mut g = lock_recover(&self.beats);
        for p in pulses {
            let Some(session) = p.session.clone() else {
                continue; // un-attributable pulse (no session id) — skip
            };
            let b = g.entry((source.to_string(), session)).or_insert(Beat {
                last_unix: 0,
                events: 0,
                last_model: String::new(),
                phase: String::new(),
            });
            b.last_unix = now; // refresh/revive liveness
            b.phase = p.kind.as_str().to_string();
            // Intentionally NOT bumping `events`: that counts cost events only.
        }
    }

    /// Beat a session from an authoritative Claude Code hook. Hooks carry no cost, so
    /// this refreshes recency + sets `phase` from the lifecycle event WITHOUT counting a cost event
    /// or model — the same discipline as `record_pulses`, but sourced from real lifecycle rather
    /// than a log-derived guess. `SessionStart` `model` (when present) seeds `last_model`.
    fn record_hook(&self, source: &'static str, rec: &tare_core::hooks::HookRecord) {
        use tare_core::hooks::HookEvent;
        // Map the lifecycle event to the Live view's phase vocabulary.
        let phase = match &rec.event {
            HookEvent::SessionStart { .. } => "started",
            HookEvent::UserPromptSubmit
            | HookEvent::PreToolUse
            | HookEvent::PostToolUse
            | HookEvent::SubagentStop
            | HookEvent::PreCompact
            | HookEvent::PostCompact => "working",
            HookEvent::Notification => "waiting",
            HookEvent::Stop => "stopped",
            HookEvent::SessionEnd { .. } => "ended",
        };
        let now = now_unix();
        let mut g = lock_recover(&self.beats);
        let b = g
            .entry((source.to_string(), rec.session_id.clone()))
            .or_insert(Beat {
                last_unix: 0,
                events: 0,
                last_model: String::new(),
                phase: String::new(),
            });
        b.last_unix = now;
        b.phase = phase.to_string();
        if let HookEvent::SessionStart { model: Some(m), .. } = &rec.event {
            b.last_model = m.clone();
        }
        // Intentionally NOT bumping `events`: hooks are lifecycle, not cost.
        drop(g);
        // Record hook-health: per-event last-fired + count for the Connect surface.
        let mut h = lock_recover(&self.hook_health);
        let e = h.entry(rec.event.name().to_string()).or_insert((0, 0));
        e.0 = e.0.max(now);
        e.1 = e.1.saturating_add(1);
    }

    /// Hook-health rows, event-name-ordered: per hook event, when it last fired and
    /// how many have arrived. Empty when no hooks have ever been received (a visible signal that
    /// lifecycle hooks aren't wired/firing — the Live view is then recency+process only).
    pub fn hook_health(&self) -> Vec<HookHealth> {
        lock_recover(&self.hook_health)
            .iter()
            .map(|(event, (last_unix, count))| HookHealth {
                event: event.clone(),
                last_unix: *last_unix,
                count: *count,
            })
            .collect()
    }

    /// Reseed the in-memory table from durable rows on boot, so a `tare serve` /
    /// daemon restart doesn't blank the live view. `rows`: (source, session, last_unix, events,
    /// last_model). Existing in-memory beats win (a live event since boot is fresher).
    pub fn load(&self, rows: Vec<(String, String, u64, u64, String)>) {
        let mut g = lock_recover(&self.beats);
        for (source, session, last_unix, events, last_model) in rows {
            g.entry((source, session)).or_insert(Beat {
                last_unix,
                events,
                last_model,
                phase: String::new(), // phase is ephemeral; the durable mirror only persists counts
            });
        }
    }

    /// Classified live view at `now` (unix s), most-recently-active first so the caller can float
    /// active/costly sessions to the top. `now` is injected so the state machine is unit-testable.
    /// Liveness snapshot with authoritative state fusion. `alive_sessions` are the
    /// session ids a live agent PROCESS was detected for (from `procdetect` → `--resume` ids). Each
    /// row's `state` is fused from three signals — hook lifecycle > live process > recency — so the
    /// pure-recency `ended` guess is only used when neither stronger signal exists.
    pub fn snapshot_fused(
        &self,
        now: u64,
        alive_sessions: &std::collections::BTreeSet<String>,
    ) -> Vec<SessionLive> {
        let g = lock_recover(&self.beats);
        // Per-session authoritative hook phase (from the hook-liveness beats), keyed by session id.
        // Crash-safety: `SessionEnd` is NOT guaranteed (1.5s budget; never on
        // SIGKILL/OOM), so a crashed session's last hook is a non-end phase (e.g. `working`) with no
        // terminal event. We must NOT let that pin the session alive forever — so a non-end hook
        // phase only confers liveness while its beat is FRESH; once stale it's dropped and recency
        // (→ ended) reconciles it. A terminal `ended` phase is honored regardless of age.
        let mut hook_phase: BTreeMap<&str, &str> = BTreeMap::new();
        for ((source, session), b) in g.iter() {
            if source != "hook-liveness" || b.phase.is_empty() {
                continue;
            }
            let stale = now.saturating_sub(b.last_unix) > IDLE_WINDOW_S;
            if b.phase == "ended" || !stale {
                hook_phase.insert(session.as_str(), b.phase.as_str());
            }
        }
        let mut out: Vec<SessionLive> = g
            .iter()
            .map(|((source, session), b)| {
                let age = now.saturating_sub(b.last_unix);
                let recency = classify_state(age);
                let (state, _authority) = tare_core::hooks::fuse_state(
                    recency,
                    hook_phase.get(session.as_str()).copied(),
                    alive_sessions.contains(session),
                );
                SessionLive {
                    session: session.clone(),
                    source: source.clone(),
                    state: state.to_string(),
                    last_seen_age_s: age,
                    events: b.events,
                    last_model: b.last_model.clone(),
                    micros: 0, // enriched by the read API / desktop from the store
                    phase: b.phase.clone(),
                }
            })
            .collect();
        out.sort_by(|a, b| {
            a.last_seen_age_s
                .cmp(&b.last_seen_age_s)
                .then(a.session.cmp(&b.session))
        });
        out
    }
}

/// Sink for vendor-reported aggregate metric points (Claude Code cost/token counters).
pub type MeteredSink = Arc<dyn Fn(tare_core::otel::MeteredPoint) + Send + Sync>;

#[derive(Clone)]
struct RxState {
    /// Tags trace-derived steps "otel-span".
    span_sink: Sink,
    /// Tags Claude Code log-event steps "otel-event".
    event_sink: Sink,
    /// Receives vendor-reported aggregate metrics (stored separately, never as cost steps).
    metered_sink: MeteredSink,
    privacy: Arc<PrivacyPolicy>,
    status: Arc<OtlpStatus>,
    activity: Arc<SessionActivity>,
}

/// Parse OTLP/JSON trace-export bytes into degraded `StepRecord`s (reuses the byte-stable GenAI
/// span ingest in `tare_core::otel`). Pure + unit-tested.
pub fn ingest_traces_json(
    bytes: &[u8],
    privacy: &PrivacyPolicy,
) -> Result<Vec<StepRecord>, String> {
    let steps = tare_core::otel::ingest_otlp_json(bytes)?;
    Ok(tare_core::otel::otel_steps_to_records(&steps, privacy))
}

/// POST /v1/traces — record one step per GenAI span. Returns 400 on unparseable bodies (e.g. an
/// `http/protobuf` exporter); 200 once the spans are queued to the sink.
async fn post_traces(State(st): State<RxState>, body: Bytes) -> StatusCode {
    match ingest_traces_json(&body, &st.privacy) {
        Ok(recs) => sink_all(&st.span_sink, &st.status, &st.activity, "otel-span", recs),
        Err(_) => {
            // Parser errors can contain producer-controlled attribute values. Keep payload and
            // terminal-control bytes out of logs; the HTTP status is the actionable signal.
            eprintln!("tare otlp: rejected malformed or unsupported trace export");
            StatusCode::BAD_REQUEST
        }
    }
}

/// POST /v1/logs — map Claude Code `api_request` events (its primary out-of-band telemetry) into
/// per-request steps. `user_prompt` / `tool_result` events become non-cost liveness pulses
/// so the Live view tracks "working vs waiting" without inflating spend.
async fn post_logs(State(st): State<RxState>, body: Bytes) -> StatusCode {
    match tare_core::otel::ingest_otlp_logs_json(&body, &st.privacy) {
        Ok(recs) => {
            let status = sink_all(&st.event_sink, &st.status, &st.activity, "otel-event", recs);
            // Liveness pulses are parsed from the SAME body in a second pass — keeping the cost
            // path's bytes untouched — and beat the activity table without minting cost rows.
            if let Ok(pulses) = tare_core::otel::ingest_otlp_logs_pulses(&body) {
                st.activity.record_pulses("otel-event", &pulses);
            }
            status
        }
        Err(_) => {
            eprintln!("tare otlp: rejected malformed or unsupported log export");
            StatusCode::BAD_REQUEST
        }
    }
}

fn sink_all(
    sink: &Sink,
    status: &OtlpStatus,
    activity: &SessionActivity,
    source: &'static str,
    recs: Vec<StepRecord>,
) -> StatusCode {
    let n = u64::try_from(recs.len()).unwrap_or(u64::MAX);
    activity.record(source, &recs); // liveness beats before the steps move into the sink
    for r in recs {
        sink(r);
    }
    status.record(n);
    StatusCode::OK
}

/// POST /v1/metrics — map Claude Code's vendor `cost.usage` / `token.usage` counters into metered
/// series. These are aggregates stored SEPARATELY (never cost steps), so they don't
/// double-count the per-request logs. Unparseable bodies still 200 (an exporter shouldn't see
/// errors for a signal we treat as best-effort); only the metered points we recognize are kept.
async fn post_metrics(State(st): State<RxState>, body: Bytes) -> StatusCode {
    // Bucket metered points on the user's local day so they reconcile against the local-day
    // estimate rather than a UTC day that drifts near midnight for non-UTC users.
    if let Ok(points) = tare_core::otel::ingest_otlp_metrics_json(&body, crate::tz_offset_minutes())
    {
        for p in points {
            (st.metered_sink)(p);
        }
    }
    StatusCode::OK
}

/// Max hook body we'll parse: hook payloads are tiny; anything larger is rejected
/// rather than buffered. Guards against a wedged/oversized stdin blob hanging the intake.
const HOOK_BODY_CAP: usize = 64 * 1024;

/// Body cap for the OTLP ingest routes (`/v1/{traces,metrics,logs}`). Axum's default is 2 MiB; a
/// busy agent's export batch can exceed that, and a non-retryable 413 would make the exporter drop
/// it (silent telemetry loss). Matches the proxy's 16 MiB `BODY_CAP` so both ingest paths agree —
/// still bounded, so a pathological body can't grow intake memory without limit.
const OTLP_BODY_CAP: usize = 16 * 1024 * 1024;

/// Loopback intake for Claude Code command hooks. Accepts the hook's stdin JSON,
/// parses it into a payload-free lifecycle event, and beats the in-memory liveness table — never
/// persisting payload, never blocking (the hook is async and must not hang the session). Always
/// returns fast: 200 on a recorded event, 400 on oversize/garbage (the command hook ignores status
/// and `exit 0`s regardless).
async fn post_hook(State(st): State<RxState>, body: Bytes) -> StatusCode {
    if body.len() > HOOK_BODY_CAP {
        return StatusCode::BAD_REQUEST;
    }
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    match tare_core::hooks::parse_hook(&v) {
        Ok(rec) => {
            st.activity.record_hook("hook-liveness", &rec);
            StatusCode::OK
        }
        Err(_) => StatusCode::BAD_REQUEST,
    }
}

/// Reject any request that isn't loopback-origin: real OTLP exporters + the
/// Claude Code hook send a loopback Host and no Origin; a browser page's cross-site fetch carries a
/// non-loopback Origin (and a rebound domain a non-loopback Host). Reuses the proxy's exact guard.
async fn loopback_guard(req: Request, next: Next) -> Response {
    if tare_proxy::loopback_request_ok(req.headers()) {
        next.run(req).await
    } else {
        (StatusCode::FORBIDDEN, "loopback origin required").into_response()
    }
}

/// Build the OTLP receiver router over per-source step sinks + shared liveness counters.
pub fn router(
    span_sink: Sink,
    event_sink: Sink,
    metered_sink: MeteredSink,
    privacy: PrivacyPolicy,
    status: Arc<OtlpStatus>,
    activity: Arc<SessionActivity>,
) -> Router {
    let st = RxState {
        span_sink,
        event_sink,
        metered_sink,
        privacy: Arc::new(privacy),
        status,
        activity,
    };
    Router::new()
        .route("/v1/traces", post(post_traces))
        .route("/v1/metrics", post(post_metrics))
        .route("/v1/logs", post(post_logs))
        .route(
            "/__tare/hook",
            post(post_hook).layer(DefaultBodyLimit::max(HOOK_BODY_CAP)),
        )
        // Anti-CSRF / anti-DNS-rebinding guard: the OTLP + hook ingest is the
        // ONLY durable-write network surface and had no origin check, so any web page the user
        // browsed could POST fabricated resourceLogs/traces/hooks to 127.0.0.1 and inject bogus
        // cost/session data. Mirror the proxy's loopback guard: real exporters/hooks (Host
        // 127.0.0.1:PORT, no Origin) pass; a cross-site fetch or a rebound domain is rejected 403.
        .layer(from_fn(loopback_guard))
        // Raise the body cap above axum's silent 2 MiB default: a busy agent's OTLP export batch can
        // exceed 2 MiB, and a 413 is non-retryable in the OTLP spec, so the extractor rejecting it
        // would make the exporter DROP the batch — silent telemetry loss on the core capture path.
        // Match the proxy's 16 MiB ceiling so both ingest paths agree; still bounded (the Bytes
        // extractor won't buffer past this), so no memory-DoS. `post_hook` keeps its tighter 64 KiB
        // check — this is only the looser outer bound.
        .layer(DefaultBodyLimit::max(OTLP_BODY_CAP))
        .with_state(st)
}

/// Serve the OTLP receiver on a loopback listener until the process exits.
#[allow(clippy::too_many_arguments)]
pub async fn serve_otlp(
    listener: tokio::net::TcpListener,
    span_sink: Sink,
    event_sink: Sink,
    metered_sink: MeteredSink,
    privacy: PrivacyPolicy,
    status: Arc<OtlpStatus>,
    activity: Arc<SessionActivity>,
) -> Result<(), std::io::Error> {
    axum::serve(
        listener,
        router(
            span_sink,
            event_sink,
            metered_sink,
            privacy,
            status,
            activity,
        ),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const SAMPLE: &str = r#"{"resourceSpans":[{"scopeSpans":[{"spans":[
      {"traceId":"abc","attributes":[
        {"key":"gen_ai.provider.name","value":{"stringValue":"anthropic"}},
        {"key":"gen_ai.response.model","value":{"stringValue":"claude-opus-4-8"}},
        {"key":"gen_ai.usage.input_tokens","value":{"intValue":1200}},
        {"key":"gen_ai.usage.output_tokens","value":{"intValue":340}}
      ]}
    ]}]}]}"#;

    #[test]
    fn ingests_otlp_json_traces_to_steprecords() {
        let recs = ingest_traces_json(SAMPLE.as_bytes(), &PrivacyPolicy::default()).unwrap();
        assert_eq!(recs.len(), 1);
        let r = &recs[0];
        assert_eq!(r.model, "claude-opus-4-8");
        assert_eq!(r.usage.fresh_input, 1200);
        assert_eq!(r.usage.output, 340);
        assert_eq!(r.run_id, "abc");
    }

    #[test]
    fn status_records_event_counts() {
        let s = OtlpStatus::default();
        assert_eq!(s.snapshot(), (0, 0));
        s.record(3);
        assert_eq!(s.snapshot().0, 3);
        assert!(s.snapshot().1 > 0, "last_unix set on a recorded event");
        s.record(0); // no-op
        assert_eq!(s.snapshot().0, 3);
    }

    #[test]
    fn status_and_activity_counts_saturate_instead_of_wrapping() {
        let status = OtlpStatus::default();
        status.events.store(u64::MAX - 1, Ordering::Relaxed);
        status.record(3);
        assert_eq!(status.snapshot().0, u64::MAX);

        let activity = SessionActivity::default();
        activity.load(vec![(
            "otel-event".to_string(),
            "session".to_string(),
            now_unix(),
            u64::MAX,
            "model".to_string(),
        )]);
        let mut recs = ingest_traces_json(SAMPLE.as_bytes(), &PrivacyPolicy::default()).unwrap();
        recs[0].shape.session = Some("session".to_string());
        activity.record("otel-event", &recs);
        assert_eq!(
            activity.snapshot_fused(now_unix(), &std::collections::BTreeSet::new())[0].events,
            u64::MAX
        );
    }

    #[test]
    fn listening_defaults_false_and_reflects_the_bind_outcome() {
        // fail-safe — never claim the receiver is listening until a bind actually
        // succeeds, so a bind failure can't be reported as a live receiver.
        let s = OtlpStatus::default();
        assert!(!s.is_listening(), "defaults to not-listening (fail-safe)");
        s.set_listening(true);
        assert!(s.is_listening());
        s.set_listening(false);
        assert!(!s.is_listening());
    }

    #[test]
    fn invalid_json_is_an_error_not_a_panic() {
        assert!(ingest_traces_json(b"not otlp json", &PrivacyPolicy::default()).is_err());
    }

    #[test]
    fn each_span_reaches_the_sink_once() {
        let captured: Arc<Mutex<Vec<StepRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let c2 = captured.clone();
        let sink: Sink = Arc::new(move |s| c2.lock().unwrap().push(s));
        for r in ingest_traces_json(SAMPLE.as_bytes(), &PrivacyPolicy::default()).unwrap() {
            (sink)(r);
        }
        assert_eq!(captured.lock().unwrap().len(), 1);
    }

    const CC_LOGS: &str = r#"{"resourceLogs":[{"scopeLogs":[{"logRecords":[
      {"attributes":[
        {"key":"event.name","value":{"stringValue":"api_request"}},
        {"key":"session.id","value":{"stringValue":"sess-x"}},
        {"key":"model","value":{"stringValue":"claude-opus-4-8"}},
        {"key":"input_tokens","value":{"intValue":2135}},
        {"key":"output_tokens","value":{"intValue":4}},
        {"key":"event.sequence","value":{"intValue":7}}]}
    ]}]}]}"#;

    // C2: exercise the LIVE HTTP layer (`tare serve`'s capture path), not just the pure fns —
    // bind the router on a loopback port, POST real OTLP/JSON, and assert routing + status.
    #[tokio::test]
    async fn http_layer_routes_traces_and_logs_and_status() {
        let spans: Arc<Mutex<Vec<StepRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let events: Arc<Mutex<Vec<StepRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let (s1, e1) = (spans.clone(), events.clone());
        let span_sink: Sink = Arc::new(move |s| s1.lock().unwrap().push(s));
        let event_sink: Sink = Arc::new(move |s| e1.lock().unwrap().push(s));
        let metered: Arc<Mutex<Vec<tare_core::otel::MeteredPoint>>> =
            Arc::new(Mutex::new(Vec::new()));
        let m1 = metered.clone();
        let metered_sink: MeteredSink = Arc::new(move |p| m1.lock().unwrap().push(p));
        let status = Arc::new(OtlpStatus::default());
        let activity = Arc::new(SessionActivity::default());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let st = status.clone();
        let act = activity.clone();
        tokio::spawn(async move {
            let _ = serve_otlp(
                listener,
                span_sink,
                event_sink,
                metered_sink,
                PrivacyPolicy::default(),
                st,
                act,
            )
            .await;
        });

        let base = format!("http://{addr}");
        let http = reqwest::Client::new();

        // Traces -> 200, one span-sourced step.
        let r = http
            .post(format!("{base}/v1/traces"))
            .body(SAMPLE)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);

        // Claude Code logs -> 200, one event-sourced step.
        let r = http
            .post(format!("{base}/v1/logs"))
            .body(CC_LOGS)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);

        // Metrics are accepted (so exporters don't error).
        // Metrics: a vendor cost counter -> 200 and reaches the metered sink (not the step sinks).
        let cc_metrics = r#"{"resourceMetrics":[{"scopeMetrics":[{"metrics":[
          {"name":"claude_code.cost.usage","sum":{"dataPoints":[
            {"asDouble":0.5,"timeUnixNano":"1782000000000000000","attributes":[
              {"key":"model","value":{"stringValue":"claude-opus-4-8"}}]}]}}]}]}]}"#;
        assert_eq!(
            http.post(format!("{base}/v1/metrics"))
                .body(cc_metrics)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        // An empty/garbage metrics body still 200s (best-effort signal), recording nothing.
        assert_eq!(
            http.post(format!("{base}/v1/metrics"))
                .body("{}")
                .send()
                .await
                .unwrap()
                .status(),
            200
        );

        // Malformed trace export -> 400, never a panic.
        assert_eq!(
            http.post(format!("{base}/v1/traces"))
                .body("not otlp")
                .send()
                .await
                .unwrap()
                .status(),
            400
        );

        assert_eq!(
            spans.lock().unwrap().len(),
            1,
            "trace span reached span_sink"
        );
        assert_eq!(
            events.lock().unwrap().len(),
            1,
            "api_request reached event_sink"
        );
        assert_eq!(status.snapshot().0, 2, "both successful exports counted");
        // The activity table saw the two sessions (clock-injected snapshot, freshly active).
        let live = activity.snapshot_fused(now_unix(), &std::collections::BTreeSet::new());
        assert_eq!(live.len(), 2, "trace + log sessions both tracked");
        assert!(live.iter().all(|s| s.state == "working"));
        // The cost metric reached the metered sink — and NOT the step sinks (no double-count).
        let m = metered.lock().unwrap();
        assert_eq!(m.len(), 1, "one vendor cost point recorded");
        assert_eq!(m[0].metric, "cost");
        assert_eq!(m[0].value, 500_000, "$0.50 -> micro-USD");
    }

    #[tokio::test]
    async fn otlp_ingest_accepts_batches_larger_than_the_2mib_axum_default() {
        // A busy agent's OTLP export batch can exceed axum's silent 2 MiB default body limit; a 413 is
        // non-retryable in the OTLP spec, so the exporter would DROP the batch (silent telemetry loss).
        // The receiver raises the cap to OTLP_BODY_CAP. Prove a >2 MiB body REACHES the handler: a
        // 3 MiB non-OTLP body must come back 400 (handler ran, parse failed), NOT 413 (extractor
        // rejected it before the handler). Pre-fix this returned 413.
        let span_sink: Sink = Arc::new(|_| {});
        let event_sink: Sink = Arc::new(|_| {});
        let metered_sink: MeteredSink = Arc::new(|_| {});
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = serve_otlp(
                listener,
                span_sink,
                event_sink,
                metered_sink,
                PrivacyPolicy::default(),
                Arc::new(OtlpStatus::default()),
                Arc::new(SessionActivity::default()),
            )
            .await;
        });
        // 3 MiB — over the 2 MiB axum default, under the 16 MiB OTLP_BODY_CAP.
        let big = vec![b'x'; 3 * 1024 * 1024];
        let status = reqwest::Client::new()
            .post(format!("http://{addr}/v1/traces"))
            .body(big)
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(
            status, 400,
            "a >2 MiB body must reach the handler (400 parse error), not be rejected as 413"
        );
    }

    // a liveness pulse refreshes recency + phase WITHOUT counting a cost event.
    #[test]
    fn pulses_beat_liveness_and_set_phase_without_cost_events() {
        use tare_core::otel::{LivenessPulse, PulseKind};
        let act = SessionActivity::default();
        // A cost step first: events=1, phase "responded".
        let step = |sess: &str| {
            let s = tare_core::otel::ingest_otlp_json(SAMPLE.as_bytes()).unwrap();
            let mut recs = tare_core::otel::otel_steps_to_records(&s, &PrivacyPolicy::default());
            recs[0].shape.session = Some(sess.to_string());
            recs
        };
        act.record("otel-event", &step("sess-1"));
        let live = act.snapshot_fused(now_unix(), &std::collections::BTreeSet::new());
        assert_eq!(live[0].events, 1);
        assert_eq!(live[0].phase, "responded");

        // A tool_result pulse: refreshes phase to working, but does NOT bump the cost-event count.
        act.record_pulses(
            "otel-event",
            &[LivenessPulse {
                session: Some("sess-1".to_string()),
                kind: PulseKind::ToolResult,
            }],
        );
        let live = act.snapshot_fused(now_unix(), &std::collections::BTreeSet::new());
        assert_eq!(live[0].events, 1, "pulses never count as cost events");
        assert_eq!(live[0].phase, "tool_result");
        assert_eq!(live[0].state, "working");

        // A pulse for a brand-new session creates a working, zero-cost bucket (agent working, no
        // spend yet) — and an un-attributable pulse (no session) is dropped.
        act.record_pulses(
            "otel-event",
            &[
                LivenessPulse {
                    session: Some("sess-2".to_string()),
                    kind: PulseKind::UserPrompt,
                },
                LivenessPulse {
                    session: None,
                    kind: PulseKind::UserPrompt,
                },
            ],
        );
        let live = act.snapshot_fused(now_unix(), &std::collections::BTreeSet::new());
        assert_eq!(
            live.len(),
            2,
            "sess-2 added; the session-less pulse dropped"
        );
        let s2 = live.iter().find(|s| s.session == "sess-2").unwrap();
        assert_eq!(s2.events, 0);
        assert_eq!(s2.phase, "user_prompt");
    }

    // an authoritative hook beats liveness + sets phase, without minting cost events.
    #[test]
    fn hooks_beat_liveness_and_set_phase_from_lifecycle() {
        use serde_json::json;
        let act = SessionActivity::default();
        let hook = |name: &str, extra: serde_json::Value| {
            let mut v = json!({"session_id": "sess-h", "hook_event_name": name});
            if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
                for (k, val) in e {
                    o.insert(k.clone(), val.clone());
                }
            }
            tare_core::hooks::parse_hook(&v).unwrap()
        };
        // SessionStart with a model → started, last_model seeded, zero cost events.
        act.record_hook(
            "hook-liveness",
            &hook("SessionStart", json!({"model": "claude-opus-4-8"})),
        );
        let live = act.snapshot_fused(now_unix(), &std::collections::BTreeSet::new());
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].phase, "started");
        assert_eq!(live[0].events, 0, "hooks are lifecycle, not cost");
        assert_eq!(live[0].last_model, "claude-opus-4-8");
        // A Stop moves phase to stopped (still zero cost events).
        act.record_hook("hook-liveness", &hook("Stop", json!({})));
        let live = act.snapshot_fused(now_unix(), &std::collections::BTreeSet::new());
        assert_eq!(live[0].phase, "stopped");
        assert_eq!(live[0].events, 0);
    }

    // fusion retires the recency `ended` guess when a stronger signal exists.
    #[test]
    fn fused_snapshot_prefers_hooks_and_process_over_stale_recency() {
        use serde_json::json;
        let act = SessionActivity::default();
        // An OTLP event for session "s" (its recency will be forced stale below).
        let step = |sess: &str| {
            let s = tare_core::otel::ingest_otlp_json(SAMPLE.as_bytes()).unwrap();
            let mut recs = tare_core::otel::otel_steps_to_records(&s, &PrivacyPolicy::default());
            recs[0].shape.session = Some(sess.to_string());
            recs
        };
        act.record("otel-event", &step("s"));
        // Force staleness: snapshot far in the future → pure recency would say "ended".
        let far = now_unix() + IDLE_WINDOW_S + 100;
        let empty = std::collections::BTreeSet::new();
        assert_eq!(
            act.snapshot_fused(far, &empty)[0].state,
            "ended",
            "no hook, no process → recency guess stands"
        );
        // A live process for "s" overrides the ended guess.
        let alive: std::collections::BTreeSet<String> = ["s".to_string()].into_iter().collect();
        assert_eq!(act.snapshot_fused(far, &alive)[0].state, "idle");
        // A FRESH working hook makes it authoritatively working (a stale one reconciles to ended —
        // see the crash-safety test).
        act.record_hook(
            "hook-liveness",
            &tare_core::hooks::parse_hook(
                &json!({"session_id": "s", "hook_event_name": "UserPromptSubmit", "prompt_id": "p"}),
            )
            .unwrap(),
        );
        let live = act.snapshot_fused(now_unix(), &empty);
        assert!(
            live.iter()
                .any(|r| r.session == "s" && r.state == "working"),
            "a fresh working hook keeps the session working"
        );
        // A SessionEnd hook ends it authoritatively even if a process still lingers.
        act.record_hook(
            "hook-liveness",
            &tare_core::hooks::parse_hook(
                &json!({"session_id": "s", "hook_event_name": "SessionEnd"}),
            )
            .unwrap(),
        );
        let live = act.snapshot_fused(now_unix(), &alive);
        assert!(live.iter().all(|r| r.session != "s" || r.state == "ended"));
    }

    // hooks track per-event health (last-fired + count) for the Connect surface.
    #[test]
    fn hook_health_tracks_per_event_counts() {
        use serde_json::json;
        let act = SessionActivity::default();
        assert!(act.hook_health().is_empty(), "no hooks yet → silent");
        let hook = |name: &str| {
            tare_core::hooks::parse_hook(&json!({"session_id": "s", "hook_event_name": name}))
                .unwrap()
        };
        act.record_hook("hook-liveness", &hook("SessionStart"));
        act.record_hook("hook-liveness", &hook("Stop"));
        act.record_hook("hook-liveness", &hook("Stop"));
        let hh = act.hook_health();
        let stop = hh.iter().find(|h| h.event == "Stop").unwrap();
        assert_eq!(stop.count, 2);
        assert!(hh.iter().any(|h| h.event == "SessionStart" && h.count == 1));
        assert!(stop.last_unix > 0);
    }

    // a crash-missed SessionEnd must not pin a session "working" forever — a stale
    // non-end hook reconciles to recency (ended); a stale SessionEnd stays terminal.
    #[test]
    fn crash_missed_session_end_reconciles_via_hook_staleness() {
        use serde_json::json;
        let act = SessionActivity::default();
        let working = tare_core::hooks::parse_hook(
            &json!({"session_id": "s", "hook_event_name": "UserPromptSubmit", "prompt_id": "p"}),
        )
        .unwrap();
        act.record_hook("hook-liveness", &working);
        // Fresh: the working hook keeps it alive.
        let empty = std::collections::BTreeSet::new();
        assert_eq!(act.snapshot_fused(now_unix(), &empty)[0].state, "working");
        // Far future (session crashed, no SessionEnd ever arrived): the stale working hook is
        // dropped, recency reconciles it to ended — no eternal ghost.
        let far = now_unix() + IDLE_WINDOW_S + 100;
        assert_eq!(
            act.snapshot_fused(far, &empty)[0].state,
            "ended",
            "stale non-end hook no longer pins the session alive"
        );
        // But a live process still legitimately overrides (it really IS running).
        let alive: std::collections::BTreeSet<String> = ["s".to_string()].into_iter().collect();
        assert_eq!(act.snapshot_fused(far, &alive)[0].state, "idle");
    }

    // the loopback /__tare/hook intake accepts a hook, caps the body, never hangs.
    #[tokio::test]
    async fn http_layer_accepts_hook_intake_and_caps_body() {
        let sink: Sink = Arc::new(|_s| {});
        let metered_sink: MeteredSink = Arc::new(|_p| {});
        let status = Arc::new(OtlpStatus::default());
        let activity = Arc::new(SessionActivity::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (act, st) = (activity.clone(), status.clone());
        tokio::spawn(async move {
            let _ = serve_otlp(
                listener,
                sink.clone(),
                sink,
                metered_sink,
                PrivacyPolicy::default(),
                st,
                act,
            )
            .await;
        });
        let base = format!("http://{addr}");
        let http = reqwest::Client::new();

        // A real SessionStart hook → 200, and the session shows up live.
        let r = http
            .post(format!("{base}/__tare/hook"))
            .body(r#"{"session_id":"s-http","hook_event_name":"SessionStart","source":"startup"}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert!(activity
            .snapshot_fused(now_unix(), &std::collections::BTreeSet::new())
            .iter()
            .any(|s| s.session == "s-http" && s.phase == "started"));

        // Garbage → 400, never a panic.
        assert_eq!(
            http.post(format!("{base}/__tare/hook"))
                .body("not json")
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        // Oversize (> 64 KiB) is rejected by the route-specific extractor before buffering it all.
        let big = "x".repeat(70 * 1024);
        assert_eq!(
            http.post(format!("{base}/__tare/hook"))
                .body(big)
                .send()
                .await
                .unwrap()
                .status(),
            413
        );
    }

    // liveness state machine is pure recency, clock-injected, and revives on a new event.
    #[test]
    fn liveness_state_machine_classifies_and_revives() {
        assert_eq!(classify_state(0), "working");
        assert_eq!(classify_state(WORKING_WINDOW_S), "working");
        assert_eq!(classify_state(WORKING_WINDOW_S + 1), "idle");
        assert_eq!(classify_state(IDLE_WINDOW_S), "idle");
        assert_eq!(classify_state(IDLE_WINDOW_S + 1), "ended");

        let act = SessionActivity::default();
        let step = |sess: &str| {
            let s = tare_core::otel::ingest_otlp_json(SAMPLE.as_bytes()).unwrap();
            let mut recs = tare_core::otel::otel_steps_to_records(&s, &PrivacyPolicy::default());
            recs[0].shape.session = Some(sess.to_string());
            recs
        };
        act.record("otel-event", &step("sess-1"));
        // A beat is at now_unix(); a snapshot taken FAR in the future reads it as ended...
        let base = now_unix();
        let live = act.snapshot_fused(
            base + IDLE_WINDOW_S + 10,
            &std::collections::BTreeSet::new(),
        );
        assert_eq!(live[0].state, "ended");
        // ...then a fresh event revives it to working (latest snapshot at the new now).
        act.record("otel-event", &step("sess-1"));
        let live2 = act.snapshot_fused(now_unix(), &std::collections::BTreeSet::new());
        assert_eq!(live2.len(), 1, "same session, not duplicated");
        assert_eq!(live2[0].state, "working");
        assert_eq!(live2[0].events, 2);
    }
}
