//! Tare CLI library. Holds the testable logic behind `tare run/report/serve/export/
//! render/demo`. The thin `main.rs` only parses argv and calls these.

use std::io::{Read, Write};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tare_core::attribute::{self, Report};
use tare_core::flamegraph::{build_flamegraph, FlamegraphModel};
use tare_core::model::{Provider, RunRecord, StepRecord};
use tare_core::money::MicroUsd;
use tare_core::{speedscope, svg, PricingTable};
use tare_proxy::{serve_with_shutdown_transcript, ProxyConfig, Sink};
use tare_store::{Store, TranscriptStore};

pub mod agent;
pub mod aider_connect;
pub mod codex_connect;
pub mod connect;
pub mod gemini_connect;
mod otel_receiver;
pub mod procdetect;
pub mod service;
pub mod single_instance;
pub mod transcript_capture;
pub mod transcript_watch;
mod ui_assets;

pub const SHIPPED_PRICING: &str = include_str!("../../pricing/pricing.json");
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Resolve the user's home directory cross-platform. Prefer `$HOME` (normally set
/// on unix/macOS), falling back to `%USERPROFILE%` on Windows where `$HOME` is normally unset.
/// Returns `None` only when neither is set to a non-empty value. Without this, default Claude Code /
/// Codex / Gemini config discovery returned nothing on Windows, silently disabling capture, backfill,
/// `detect`/`doctor`, and agent wiring even with the agent installed at `%USERPROFILE%\.claude`.
pub fn home_dir() -> Option<String> {
    std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .filter(|s| !s.trim().is_empty())
}

/// Load pricing from a file (`.toml`/`.json`) or the embedded shipped table.
pub fn load_pricing(path: Option<&str>) -> Result<PricingTable, String> {
    load_pricing_with_config(path, &load_config())
}

/// Load pricing without re-reading configuration. Capture commands use this after their strict
/// config load so a concurrent edit or a lenient fallback cannot silently drop price overrides.
fn load_pricing_with_config(
    path: Option<&str>,
    cfg: &tare_core::config::TareConfig,
) -> Result<PricingTable, String> {
    let table = match path {
        Some(p) => {
            let s = std::fs::read_to_string(p).map_err(|e| format!("read pricing {p}: {e}"))?;
            if p.ends_with(".toml") {
                PricingTable::from_toml_str(&s)?
            } else {
                PricingTable::from_json_str(&s)?
            }
        }
        None => PricingTable::from_json_str(SHIPPED_PRICING)?,
    };
    Ok(table.with_config(cfg))
}

/// UTC date `YYYY-MM-DD`. The store stamps runs with this; core stays clock-free.
pub fn today_utc() -> String {
    civil_date_for(now_unix_secs(), 0)
}

/// "Today" in the user's local calendar day: UTC shifted by the configured offset
/// (`TARE_TZ_OFFSET_MINUTES` env > `[ui] tz_offset_minutes` > 0 = UTC). Used to STAMP captured
/// runs and to query "today" and trends so the stamp and query always agree.
pub fn today_local() -> String {
    civil_date_for(now_unix_secs(), tz_offset_minutes())
}

/// Configured local-day offset in minutes (environment over tare.toml over 0). Clamped to ±24 hours:
/// no real timezone exceeds that, and an absurd value could otherwise overflow the day math.
pub fn tz_offset_minutes() -> i64 {
    std::env::var("TARE_TZ_OFFSET_MINUTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .or_else(|| load_config().ui.tz_offset_minutes)
        .unwrap_or(0)
        .clamp(-1440, 1440)
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Pure: the `YYYY-MM-DD` calendar day for a unix-seconds instant shifted by `offset_minutes`.
/// Delegates to the shared core rule so stamping and querying cannot drift.
fn civil_date_for(secs: i64, offset_minutes: i64) -> String {
    tare_core::calendar::civil_date_for(secs, offset_minutes)
}

/// Channel item drained by the single SQLite writer thread: either a captured step (with its
/// capture `source`: proxy | otel-span | otel-event) or a vendor-reported metered point.
/// One enum over one channel keeps a single writer owning the connection (SQLite is single-writer).
enum WriteMsg {
    // Boxed: a StepRecord is much larger than a MeteredPoint, so box it to keep the channel item
    // small (clippy::large_enum_variant).
    Step(Box<StepRecord>, &'static str),
    Metered(tare_core::otel::MeteredPoint),
    /// A run's self-reported `x-tare-quality` scalar, routed through the single writer.
    Quality {
        run_id: String,
        score: i64,
    },
    /// A redacted request/response pair for the SEPARATE transcript store, captured only
    /// under the `max_inspect` profile. Bodies are already scrubbed at the proxy edge; the writer
    /// just persists them. Routed through the same single writer so the transcript DB has one owner.
    Transcript {
        run_id: String,
        step_ordinal: u32,
        request: String,
        response: String,
        truncated: bool,
    },
}

type StepWriter = (
    std::sync::mpsc::Sender<WriteMsg>,
    std::thread::JoinHandle<Result<(), String>>,
);

/// Spawn the single writer thread that owns the counts [`Store`]. Optionally the writer also owns a
/// [`TranscriptStore`] opened from `transcript_db`: the transcript store lives in a separate SQLite
/// file and is only wired when the effective profile is `max_inspect`;
/// `WriteMsg::Transcript` payloads are already redacted at the proxy edge, so the writer just
/// persists them. Keeping the transcript connection on the same single writer thread means both DBs
/// have exactly one owner (no cross-thread sharing of a non-`Sync` rusqlite `Connection`).
fn spawn_step_writer_with_transcript(
    store: Store,
    policy: tare_core::PrivacyPolicy,
    transcript_db: Option<String>,
) -> Result<StepWriter, String> {
    let (tx, rx) = std::sync::mpsc::channel::<WriteMsg>();
    let policy_id = policy.policy_id();
    let profile = policy.effective_profile().as_str().to_string();
    // Open before spawning so max-inspect never starts while its requested transcript store is
    // unavailable. The connection moves to, and remains owned by, the writer thread.
    let transcript = transcript_db
        .map(|path| {
            TranscriptStore::open(&path).map_err(|e| format!("open transcript store {path}: {e}"))
        })
        .transpose()?;
    let writer = std::thread::Builder::new()
        .name("tare-step-writer".into())
        .spawn(move || {
            let mut first_error = None;
            let mut record_error = |message: String| {
                eprintln!("tare: {message}");
                if first_error.is_none() {
                    first_error = Some(message);
                }
            };
            for msg in rx {
                match msg {
                    WriteMsg::Step(step, source) => {
                        // Derive the calendar day PER STEP, at write time. The date
                        // was captured ONCE at startup, so an always-on `tare serve` stamped every future
                        // day's steps with the launch date — `report --today` read zero and trend/heatmap
                        // piled a whole run of days onto one bucket. Live steps are written as they arrive,
                        // so today_local() here is the event's day.
                        let date = today_local();
                        match store.record_step_with_policy(
                            &step,
                            &date,
                            Some(&policy_id),
                            Some(&profile),
                            Some(source),
                        ) {
                            Ok(()) => {
                                // Only mark a session active after its step is durable. A rejected write
                                // must not leave behind activity for a run that does not exist.
                                let session = step
                                    .shape
                                    .session
                                    .clone()
                                    .unwrap_or_else(|| step.run_id.clone());
                                if let Err(e) = store.record_session_beat(
                                    source,
                                    &session,
                                    now_unix_secs(),
                                    &step.model,
                                ) {
                                    record_error(format!("session-activity beat failed: {e}"));
                                }
                            }
                            Err(e) => record_error(format!(
                                "failed to persist run {} step {}: {e}",
                                step.run_id, step.step_ordinal
                            )),
                        }
                    }
                    WriteMsg::Metered(point) => {
                        // Vendor-reported aggregate: stored separately as a cross-check.
                        if let Err(e) = store.record_metered(&point) {
                            record_error(format!("failed to persist metered point: {e}"));
                        }
                    }
                    WriteMsg::Quality { run_id, score } => {
                        // Run self-report via x-tare-quality. Idempotent (INSERT OR REPLACE).
                        if let Err(e) = store.set_run_quality(
                            &run_id,
                            score,
                            "header",
                            &now_unix_secs().to_string(),
                        ) {
                            record_error(format!("failed to persist run quality: {e}"));
                        }
                    }
                    WriteMsg::Transcript {
                        run_id,
                        step_ordinal,
                        request,
                        response,
                        truncated,
                    } => {
                        // Redacted transcript capture, max_inspect only. Bodies are already
                        // scrubbed at the proxy edge; persist to the separate store if one is open.
                        if let Some(ts) = &transcript {
                            if let Err(e) =
                                ts.insert(&run_id, step_ordinal, &request, &response, truncated)
                            {
                                record_error(format!("failed to persist transcript: {e}"));
                            }
                        }
                    }
                }
            }
            first_error.map_or(Ok(()), Err)
        })
        .map_err(|e| format!("spawn step-writer thread: {e}"))?;
    Ok((tx, writer))
}

fn join_step_writer(writer: std::thread::JoinHandle<Result<(), String>>) -> Result<(), String> {
    writer
        .join()
        .map_err(|_| "step-writer thread panicked".to_string())?
}

/// A `QualitySink` that routes a run's `x-tare-quality` self-report through the single writer
/// thread, so the store has one owner.
fn quality_sink(tx: &std::sync::mpsc::Sender<WriteMsg>) -> tare_proxy::QualitySink {
    let tx = tx.clone();
    tare_proxy::QualitySink(Arc::new(move |run_id, score| {
        if tx.send(WriteMsg::Quality { run_id, score }).is_err() {
            eprintln!("tare: step-writer thread is gone; dropping a quality report");
        }
    }))
}

/// A `Sink` that tags every captured step with its `source` before queueing it to the writer.
fn tagged_sink(tx: &std::sync::mpsc::Sender<WriteMsg>, source: &'static str) -> Sink {
    let tx = tx.clone();
    Arc::new(move |step| {
        if tx.send(WriteMsg::Step(Box::new(step), source)).is_err() {
            eprintln!("tare: step-writer thread is gone; dropping a captured step");
        }
    })
}

/// A sink for vendor-reported metered points, queued to the same writer thread.
fn metered_sink(
    tx: &std::sync::mpsc::Sender<WriteMsg>,
) -> Arc<dyn Fn(tare_core::otel::MeteredPoint) + Send + Sync> {
    let tx = tx.clone();
    Arc::new(move |point| {
        if tx.send(WriteMsg::Metered(point)).is_err() {
            eprintln!("tare: step-writer thread is gone; dropping a metered point");
        }
    })
}

/// A `TranscriptSink` that queues an already-redacted request/response pair to the same
/// single writer thread, which owns the separate transcript store. Only wired under `max_inspect`.
fn transcript_sink(tx: &std::sync::mpsc::Sender<WriteMsg>) -> tare_proxy::TranscriptSink {
    let tx = tx.clone();
    Arc::new(move |run_id, step_ordinal, request, response, truncated| {
        if tx
            .send(WriteMsg::Transcript {
                run_id,
                step_ordinal,
                request,
                response,
                truncated,
            })
            .is_err()
        {
            eprintln!("tare: step-writer thread is gone; dropping a transcript capture");
        }
    })
}

/// Child environment for `tare run`: loopback base URLs, dummy non-empty keys, run id.
pub fn prepare_child_env(base_url: &str, run_id: &str) -> Vec<(String, String)> {
    let v1 = format!("{}/v1", base_url.trim_end_matches('/'));
    vec![
        ("ANTHROPIC_BASE_URL".into(), base_url.to_string()),
        ("OPENAI_BASE_URL".into(), v1.clone()),
        ("OPENAI_API_BASE".into(), v1),
        ("ANTHROPIC_API_KEY".into(), "tare-dummy-key".into()),
        ("OPENAI_API_KEY".into(), "tare-dummy-key".into()),
        // Azure SDKs read AZURE_OPENAI_ENDPOINT; point it at the loopback proxy too.
        ("AZURE_OPENAI_ENDPOINT".into(), base_url.to_string()),
        ("AZURE_OPENAI_API_KEY".into(), "tare-dummy-key".into()),
        // Gemini SDKs read these base-URL overrides.
        ("GEMINI_BASE_URL".into(), base_url.to_string()),
        ("GOOGLE_GEMINI_BASE_URL".into(), base_url.to_string()),
        ("GEMINI_API_KEY".into(), "tare-dummy-key".into()),
        ("TARE_RUN_ID".into(), run_id.to_string()),
    ]
}

/// Environment entries `tare run` must explicitly set on its child. Real inherited provider keys
/// are omitted from this list, which lets `Command` preserve them while still injecting dummy keys
/// for SDKs that require a non-empty credential in unauthenticated/fake-upstream workflows.
fn child_env_overrides(
    base_url: &str,
    run_id: &str,
    has_inherited_value: impl Fn(&str) -> bool,
) -> Vec<(String, String)> {
    prepare_child_env(base_url, run_id)
        .into_iter()
        .filter(|(name, value)| value != "tare-dummy-key" || !has_inherited_value(name))
        .collect()
}

/// The config file path: `TARE_CONFIG` env, else CWD-relative `tare.toml`. Keeping the
/// CWD default preserves terminal/CLI and test behavior; the desktop app sets `TARE_CONFIG` to its
/// stable root (`$HOME/.tare/tare.toml`) on the `tare serve` it spawns, so a Finder-launched app —
/// whose CWD is `/` — still reads the user's real config instead of a nonexistent `/tare.toml`.
pub fn config_path() -> String {
    config_path_from(std::env::var("TARE_CONFIG").ok())
}

/// Pure resolver (testable without mutating process env): the `TARE_CONFIG` value if set, else the
/// CWD-relative default.
fn config_path_from(env: Option<String>) -> String {
    env.unwrap_or_else(|| "tare.toml".to_string())
}

/// Load the config (see [`config_path`]), or the default if absent. A malformed file is logged and
/// treated as default for read-only commands. Capture commands use [`load_config_strict`].
pub fn load_config() -> tare_core::config::TareConfig {
    tare_core::config::TareConfig::load(&config_path()).unwrap_or_else(|e| {
        eprintln!("tare: ignoring tare.toml ({e})");
        tare_core::config::TareConfig::default()
    })
}

/// Like [`load_config`] but fails on a present-but-unparseable tare.toml instead of silently
/// dropping to defaults. The budget-enforcing paths (`tare run` and `tare serve`) use
/// this: a typo anywhere in the file must not silently disable the configured spend cap /
/// runaway-loop kill-switch and let the proxy run wide open. Read-only commands keep using the
/// lenient `load_config` so a bad edit can't block reporting.
pub fn load_config_strict() -> Result<tare_core::config::TareConfig, String> {
    tare_core::config::TareConfig::load(&config_path()).map_err(|e| {
        format!("tare.toml is invalid ({e}) — fix or remove it; refusing to start without your configured budget/kill-switch")
    })
}

/// Build a per-run budget / kill-switch. Precedence: env var > `[budget]` in `tare.toml` >
/// unset. `TARE_MAX_SPEND_USD`, `TARE_MAX_STEPS`, `TARE_MAX_REPEATS` each override their file key.
fn resolve_budget(
    cfg: &tare_core::config::TareConfig,
) -> Result<Option<tare_core::budget::Budget>, String> {
    resolve_budget_with(cfg, optional_env)
}

fn resolve_budget_with(
    cfg: &tare_core::config::TareConfig,
    mut env: impl FnMut(&str) -> Result<Option<String>, String>,
) -> Result<Option<tare_core::budget::Budget>, String> {
    let base = cfg.budget.to_budget();
    let max_micros = match env("TARE_MAX_SPEND_USD")? {
        Some(raw) => {
            let usd = raw.parse::<f64>().map_err(|_| {
                format!("TARE_MAX_SPEND_USD must be a positive dollar amount, got {raw:?}")
            })?;
            if usd <= 0.0 {
                return Err(format!(
                    "TARE_MAX_SPEND_USD must be greater than zero, got {raw:?}"
                ));
            }
            Some(tare_core::config::dollars_to_micros(usd).ok_or_else(|| {
                format!("TARE_MAX_SPEND_USD is not a finite representable amount: {raw:?}")
            })?)
        }
        None => base.max_micros,
    };
    let mut env_u32 = |name: &str, fallback: Option<u32>| -> Result<Option<u32>, String> {
        match env(name)? {
            Some(raw) => raw.parse::<u32>().map(Some).map_err(|_| {
                format!(
                    "{name} must be an integer from 0 to {}, got {raw:?}",
                    u32::MAX
                )
            }),
            None => Ok(fallback),
        }
    };
    let max_steps = env_u32("TARE_MAX_STEPS", base.max_steps)?;
    let max_identical_repeats = env_u32("TARE_MAX_REPEATS", base.max_identical_repeats)?;
    let b = tare_core::budget::Budget {
        max_micros,
        soft_micros: base.soft_micros,
        max_steps,
        max_identical_repeats,
    };
    Ok(b.is_set().then_some(b))
}

/// A stderr alert sink for budget warnings/kills (fire-once), wired into the proxy by
/// `tare run`/`tare serve`. Each fired alert is printed via `tare-daemon`'s `StderrSink`.
fn stderr_alert_sink() -> tare_proxy::AlertSink {
    use tare_daemon::AlertSink as _;
    tare_proxy::AlertSink(Arc::new(|alert| {
        let _ = tare_daemon::StderrSink.emit(&alert);
    }))
}

fn optional_env(name: &str) -> Result<Option<String>, String> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!("{name} is not valid Unicode")),
    }
}

/// Resolve the active privacy policy: `TARE_PRIVACY_PROFILE`/`TARE_PRIVACY_SALT` env over a
/// `[privacy]` table in `./tare.toml`, over the built-in `strict_counts` default.
pub fn resolve_privacy() -> Result<tare_core::PrivacyPolicy, String> {
    let env_profile = optional_env("TARE_PRIVACY_PROFILE")?;
    let env_salt = optional_env("TARE_PRIVACY_SALT")?;
    let path = config_path();
    let file = match std::fs::read_to_string(&path) {
        Ok(file) => Some(file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("read privacy config {path}: {error}")),
    };
    resolve_privacy_from(env_profile.as_deref(), env_salt.as_deref(), file.as_deref())
}

fn resolve_privacy_from(
    env_profile: Option<&str>,
    env_salt: Option<&str>,
    file: Option<&str>,
) -> Result<tare_core::PrivacyPolicy, String> {
    if let Some(contents) = file {
        // Privacy is a safety boundary: a misspelled field must not silently select a less-private
        // default. Validate against the unified schema before resolving env precedence.
        tare_core::config::TareConfig::from_toml_str(contents)
            .map_err(|error| format!("invalid privacy configuration: {error}"))?;
    }
    tare_core::PrivacyPolicy::resolve(None, env_profile, env_salt, file)
        .map_err(|e| format!("invalid privacy configuration: {e}"))
}

/// Per-provider upstream overrides. Precedence: env var > `[providers]` in `tare.toml`. A
/// provider left unset everywhere is omitted so `ProxyConfig::upstream_for` falls back to its
/// public default.
fn resolve_upstreams(
    cfg: &tare_core::config::TareConfig,
) -> Result<std::collections::BTreeMap<Provider, String>, String> {
    resolve_upstreams_with(cfg, optional_env)
}

fn resolve_upstreams_with(
    cfg: &tare_core::config::TareConfig,
    mut env: impl FnMut(&str) -> Result<Option<String>, String>,
) -> Result<std::collections::BTreeMap<Provider, String>, String> {
    let mut m = std::collections::BTreeMap::new();
    let mut insert = |provider: Provider, raw: String| -> Result<(), String> {
        let endpoint = raw.trim();
        let url = reqwest::Url::parse(endpoint)
            .map_err(|e| format!("invalid {} upstream {raw:?}: {e}", provider.as_str()))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || url.cannot_be_a_base()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(format!(
                "invalid {} upstream {raw:?}: expected an http(s) base URL without credentials, query, or fragment",
                provider.as_str()
            ));
        }
        m.insert(provider, endpoint.trim_end_matches('/').to_string());
        Ok(())
    };
    if let Some(a) =
        env("TARE_ANTHROPIC_UPSTREAM")?.or_else(|| cfg.providers.anthropic_upstream.clone())
    {
        insert(Provider::Anthropic, a)?;
    }
    if let Some(o) = env("TARE_OPENAI_UPSTREAM")?.or_else(|| cfg.providers.openai_upstream.clone())
    {
        insert(Provider::Openai, o)?;
    }
    if let Some(g) = env("TARE_GEMINI_UPSTREAM")?.or_else(|| cfg.providers.gemini_upstream.clone())
    {
        insert(Provider::Gemini, g)?;
    }
    // Azure OpenAI and Bedrock are routable providers whose built-in upstream is a deliberately
    // failing `.invalid` placeholder, so they REQUIRE an explicit override to be usable. Azure's
    // natural source is the standard AZURE_OPENAI_ENDPOINT env var.
    let azure_env = match env("TARE_AZURE_OPENAI_UPSTREAM")? {
        some @ Some(_) => some,
        None => env("AZURE_OPENAI_ENDPOINT")?,
    };
    if let Some(az) = azure_env.or_else(|| cfg.providers.azure_openai_upstream.clone()) {
        insert(Provider::AzureOpenai, az)?;
    }
    if let Some(br) =
        env("TARE_BEDROCK_UPSTREAM")?.or_else(|| cfg.providers.bedrock_upstream.clone())
    {
        insert(Provider::BedrockConverse, br)?;
    }
    Ok(m)
}

/// `tare run -- <cmd>`: spin a dedicated loopback proxy, inject env, run the child,
/// capturing every step into the store under `run_id`. Returns the child exit code.
pub fn run_command(child_argv: &[String], db_path: &str, run_id: &str) -> Result<i32, String> {
    if child_argv.is_empty() {
        return Err("tare run: missing command after --".into());
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;

    rt.block_on(async move {
        let cfg = load_config_strict()?;
        let privacy = resolve_privacy()?;
        let budget = resolve_budget(&cfg)?;
        let upstreams = resolve_upstreams(&cfg)?;
        let pricing = if budget
            .as_ref()
            .is_some_and(|b| b.max_micros.is_some() || b.soft_micros.is_some())
        {
            Some(
                load_pricing_with_config(cfg.proxy.pricing.as_deref(), &cfg).map_err(|e| {
                    format!("cannot enforce the configured spend budget without pricing: {e}")
                })?,
            )
        } else {
            None
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| format!("bind proxy: {e}"))?;
        let addr = listener.local_addr().map_err(|e| format!("addr: {e}"))?;
        let base_url = format!("http://{addr}");
        let store = Store::open(db_path)?;
        let transcript_db = privacy
            .captures_transcript()
            .then(|| transcript_db_path(db_path));
        let (tx, writer) =
            spawn_step_writer_with_transcript(store, privacy.clone(), transcript_db)?;
        let sink = tagged_sink(&tx, "proxy");
        let qsink = quality_sink(&tx);
        let tsink = privacy.captures_transcript().then(|| transcript_sink(&tx));
        drop(tx);
        // Git attribution: only when opted in via `[privacy] git_attribution`. Detect
        // the working-tree commit/author once at run start and stamp every captured step.
        let (git_commit, git_author) = if cfg.privacy.git_attribution {
            match std::env::current_dir()
                .ok()
                .and_then(|cwd| detect_git_attribution(&cwd))
            {
                Some(g) => (Some(g.short_sha), g.author),
                None => (None, None),
            }
        } else {
            (None, None)
        };
        let config = ProxyConfig {
            upstreams,
            run_id_override: Some(run_id.to_string()),
            budget,
            pricing,
            privacy,
            alert_sink: Some(stderr_alert_sink()),
            git_commit,
            git_author,
            quality_sink: Some(qsink),
            ..ProxyConfig::default()
        };
        // Graceful shutdown so a request the child fired just before exiting finishes capturing
        // (its step reaches the sink) instead of being dropped with an aborted connection task.
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve_with_shutdown_transcript(
            listener,
            config,
            sink,
            tsink,
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        let env = child_env_overrides(&base_url, run_id, |name| {
            std::env::var_os(name).is_some_and(|value| !value.is_empty())
        });
        let argv: Vec<String> = child_argv.to_vec();
        let status_result = tokio::task::spawn_blocking(move || {
            let mut cmd = std::process::Command::new(&argv[0]);
            cmd.args(&argv[1..]);
            for (name, value) in env {
                cmd.env(name, value);
            }
            cmd.status()
        })
        .await
        .map_err(|e| format!("join child: {e}"))
        .and_then(|result| result.map_err(|e| format!("spawn child `{}`: {e}", child_argv[0])));

        // Signal graceful shutdown, then await the server: it stops accepting and DRAINS in-flight
        // connection tasks (so their captured steps reach the sink) before returning, which drops
        // the serve future and its Sink sender, closing the channel so the writer flushes and exits.
        let _ = shutdown_tx.send(());
        let server_result = match server.await {
            Ok(result) => result.map_err(|e| format!("proxy server failed: {e}")),
            Err(error) => Err(format!("proxy task failed: {error}")),
        };
        let writer_result = join_step_writer(writer);
        let status = status_result?;
        server_result?;
        writer_result?;
        Ok(status.code().unwrap_or(1))
    })
}

/// Adaptive capture-poll backoff: how many 100ms ticks to sleep before the next
/// transcript sweep, given the count of consecutive idle sweeps (a sweep that captured no new steps).
/// 3s while active (streak 0), doubling to a 30s cap so an idle daemon stops walking the whole
/// `projects/` tree every 3s; a landing step resets the streak to 0 → back to 3s.
fn capture_backoff_ticks(idle_streak: u32) -> u32 {
    const BASE_TICKS: u32 = 30; // 3.0s at 100ms/tick
    const MAX_TICKS: u32 = 300; // 30s cap
    BASE_TICKS
        .saturating_mul(1u32 << idle_streak.min(4))
        .min(MAX_TICKS)
}

#[cfg(test)]
mod capture_backoff_tests {
    use super::capture_backoff_ticks;
    #[test]
    fn grows_from_3s_to_a_30s_cap_and_resets_with_activity() {
        assert_eq!(capture_backoff_ticks(0), 30, "active → 3s");
        assert_eq!(capture_backoff_ticks(1), 60, "6s");
        assert_eq!(capture_backoff_ticks(2), 120, "12s");
        assert_eq!(capture_backoff_ticks(3), 240, "24s");
        assert_eq!(capture_backoff_ticks(4), 300, "48s clamped to the 30s cap");
        assert_eq!(capture_backoff_ticks(50), 300, "stays capped, no overflow");
    }
}

#[cfg(test)]
mod analysis_pricing_tests {
    use super::{analysis_pricing, load_pricing};

    /// `PricingTable::default()` carries NO model rows, so anything that
    /// falls back to it prices every model at $0 — a total, silent understatement rather than a
    /// visible failure. `tare serve` used to reach it via `.unwrap_or_default()` on the user's
    /// `--pricing` path, and every analysis POST endpoint used it as its error fallback.
    #[test]
    fn the_empty_pricing_table_prices_nothing_so_it_is_never_a_fallback() {
        assert!(
            tare_core::pricing::PricingTable::default()
                .models
                .is_empty(),
            "if this ever gains rows, re-read the fallback logic in analysis_pricing/serve_command"
        );
        // An explicitly requested table that cannot be loaded is an ERROR, so serve refuses to start
        // instead of quietly serving $0 for the whole session.
        assert!(load_pricing(Some("/nonexistent/tare-pricing.json")).is_err());
        // ...and the analysis endpoints always have real rates to price with.
        assert!(
            !analysis_pricing().unwrap().models.is_empty(),
            "analysis endpoints must never price from an empty table"
        );
    }

    /// GET and POST inside one `tare serve` must price from the same table. Both paths ultimately
    /// resolve through `load_pricing`, so the bundled default they agree on must be identical.
    #[test]
    fn analysis_pricing_matches_the_bundled_table_when_nothing_is_configured() {
        let bundled = load_pricing(None).unwrap();
        let analysis = analysis_pricing().unwrap();
        // Only meaningful when the developer's own tare.toml configures no `[proxy].pricing`; when it
        // does, analysis_pricing intentionally follows it (that is the divergence fix).
        if super::load_config().proxy.pricing.is_none() {
            assert_eq!(analysis.version, bundled.version);
            assert_eq!(analysis.models.len(), bundled.models.len());
        }
    }
}

/// `tare serve`: long-lived proxy grouping by the `x-tare-run` header, until SIGINT.
pub fn serve_command(
    db_path: &str,
    port: u16,
    otlp_port: u16,
    pricing_path: Option<&str>,
) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(async move {
        let cfg = load_config_strict()?;
        let privacy = resolve_privacy()?;
        let budget = resolve_budget(&cfg)?;
        let upstreams = resolve_upstreams(&cfg)?;
        let effective_pricing_path = pricing_path
            .map(str::to_string)
            .or_else(|| cfg.proxy.pricing.clone());
        // The dashboard always needs pricing. Load it once from the strict config and share that
        // exact table with the proxy's spend guard so the two paths cannot diverge.
        let read_pricing = load_pricing_with_config(effective_pricing_path.as_deref(), &cfg)
            .map_err(|e| {
                format!(
                    "{e}\n  (refusing to serve: an unloadable pricing table would make estimates incomplete)"
                )
            })?;
        let pricing = budget
            .as_ref()
            .is_some_and(|b| b.max_micros.is_some() || b.soft_micros.is_some())
            .then(|| read_pricing.clone());

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| format!("bind {port}: {e}"))?;
        let addr = listener.local_addr().map_err(|e| format!("addr: {e}"))?;
        let store = Store::open(db_path)?;
        let transcript_db = privacy
            .captures_transcript()
            .then(|| transcript_db_path(db_path));
        // Single-instance capture lock: a second capture daemon tailing the
        // same store would assign transcript step ordinals from an independent counter and
        // double-count turns (INSERT OR REPLACE can't dedup differing ordinals). Refuse to start
        // instead. Held to the end of this block; the lock file is removed on drop. `tare run`'s
        // proxy lane uses unique run_ids and never takes this lock, so wrapping a command while a
        // dashboard `tare serve` is up still works.
        let _capture_lock = crate::single_instance::CaptureLock::acquire(db_path)?;
        let (tx, writer) =
            spawn_step_writer_with_transcript(store, privacy.clone(), transcript_db)?;
        eprintln!("tare serve: listening on http://{addr} (Ctrl-C to stop)");
        // Loopback read API for the browser viewer: GET /__tare/{runs,report,flamegraph,trend}
        // served from the store (counts only). `--pricing` lets the user supply a table that prices
        // self-hosted (provider=local) models, i.e. a cloud-equivalent overlay; else shipped rates.
        let read_db = db_path.to_string();
        // Share the resolved path with the analysis POST endpoints so GET and POST cannot price the
        // same cohort from two different tables.
        let _ = SERVE_PRICING.set(read_pricing.clone());
        // Liveness counters shared with the OTLP receiver, surfaced at /__tare/otlp_status so the
        // Connect screen can show "receiver listening, N events, last seen Xs ago".
        let otlp_status = Arc::new(otel_receiver::OtlpStatus::default());
        let status_rd = otlp_status.clone();
        // Per-session liveness table: which agent sessions are running now.
        // Reseed from the durable mirror so a restart doesn't blank the live view.
        let activity = Arc::new(otel_receiver::SessionActivity::default());
        if let Ok(store) = Store::open(db_path) {
            if let Ok(rows) = store.load_session_activity() {
                activity.load(rows);
            }
        }
        let activity_rd = activity.clone();
        let read_handler = Some(tare_proxy::ReadHandler(Arc::new(move |pq: &str| {
            if pq.split('?').next() == Some("/__tare/sessions_live") {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                // Fuse in process-detection liveness: a running `claude` resumed with
                // `--resume <id>` marks that session authoritatively alive, overriding the recency
                // `ended` guess. Enumerated on-demand per poll (UI cadence; cheap enough).
                let alive: std::collections::BTreeSet<String> =
                    procdetect::collect_agent_processes()
                        .into_iter()
                        .filter_map(|a| a.resume_session)
                        .collect();
                let mut live = activity_rd.snapshot_fused(now, &alive);
                // Enrich each live session with its lifetime cost from the store, so the list can
                // show $ and float by spend ("rises to the top as it spends"). This endpoint is
                // polled on the Live cadence, so it reads through the shared run cache
                // — a cache hit unless new steps landed since the last poll; the receiver itself
                // stays pricing-free.
                if let Ok(runs) = cached_runs(&read_db) {
                    let rep = tare_core::session::sessions(&runs, &read_pricing);
                    let costs: std::collections::HashMap<&str, i64> =
                        rep.rows.iter().map(|r| (r.session.as_str(), r.micros)).collect();
                    for s in live.iter_mut() {
                        s.micros = costs.get(s.session.as_str()).copied().unwrap_or(0);
                    }
                }
                return Some((
                    200u16,
                    "application/json".to_string(),
                    serde_json::to_string(&live).unwrap_or_else(|_| "[]".to_string()),
                ));
            }
            if pq.split('?').next() == Some("/__tare/otlp_status") {
                let (events, last) = status_rd.snapshot();
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let age = if last == 0 {
                    serde_json::Value::Null
                } else {
                    serde_json::json!(now.saturating_sub(last))
                };
                // Hook-health: per-event last-fired age + count, plus a total, so
                // the Connect UI can warn when lifecycle hooks are registered-but-silent.
                let hh = activity_rd.hook_health();
                let hooks_seen: u64 = hh.iter().map(|h| h.count).sum();
                let hooks: Vec<serde_json::Value> = hh
                    .iter()
                    .map(|h| {
                        serde_json::json!({
                            "event": h.event,
                            "count": h.count,
                            "age_seconds": now.saturating_sub(h.last_unix),
                        })
                    })
                    .collect();
                return Some((
                    200u16,
                    "application/json".to_string(),
                    serde_json::json!({
                        // Real bind outcome, not an assumption: false when the receiver
                        // failed to bind its port, so the Connect screen never falsely claims it's up.
                        "listening": status_rd.is_listening(),
                        "port": otlp_port,
                        "events": events,
                        "last_event_unix": last,
                        "age_seconds": age,
                        "hooks_seen": hooks_seen,
                        "hooks": hooks,
                    })
                    .to_string(),
                ));
            }
            read_api(&read_db, &read_pricing, pq)
        })));
        // Loopback-only write handler for run notes. Fresh Store::open per request —
        // low-frequency single-user mutation; SQLite serializes writers.
        let write_db = db_path.to_string();
        let write_handler = Some(tare_proxy::WriteHandler(Arc::new(move |pq: &str, body: &[u8]| {
            write_api(&write_db, pq, body)
        })));
        // Per-source sinks over the one writer: the proxy tags "proxy"; the OTLP receiver tags
        // "otel-span" (traces) and "otel-event" (Claude Code logs). Dropping `tx` leaves the
        // writer alive until all sinks drop (on shutdown).
        let proxy_sink = tagged_sink(&tx, "proxy");
        let otlp_span_sink = tagged_sink(&tx, "otel-span");
        let otlp_event_sink = tagged_sink(&tx, "otel-event");
        let otlp_metered_sink = metered_sink(&tx);
        let qsink = quality_sink(&tx);
        let tsink = privacy.captures_transcript().then(|| transcript_sink(&tx));
        drop(tx);
        let otlp_privacy = privacy.clone();
        let config = ProxyConfig {
            upstreams,
            run_id_override: None,
            budget,
            pricing,
            privacy,
            alert_sink: Some(stderr_alert_sink()),
            read_handler,
            // Binary UI assets: serve the self-hosted WOFF2 fonts as bytes (the text
            // read_handler can't). Checked before read_handler on GET; non-binary paths return None and
            // fall through to the text/JSON read API.
            bytes_read_handler: Some(tare_proxy::BytesReadHandler(Arc::new(|pq: &str| {
                let path = pq.split('?').next().unwrap_or(pq);
                let rel = path.strip_prefix("/__tare/")?;
                ui_assets::ui_binary_asset(rel)
                    .map(|(ct, bytes)| (200u16, ct.to_string(), bytes.to_vec()))
            }))),
            write_handler,
            quality_sink: Some(qsink),
            ..ProxyConfig::default()
        };
        // Live JSONL transcript capture: a background sweep loop owning its OWN store
        // connection (safe under WAL + busy_timeout). On start it catches up from the persisted scan
        // cursor — so every session Claude Code wrote while tare was down is replayed, no `connect`,
        // no per-session anything — then incrementally ingests new turns as the files grow. Two
        // distinct guards, not one: the `backfill_seen` ledger dedups the JSONL lane
        // against ITSELF per request; cross-lane overlap with the proxy/OTLP lanes is prevented by a
        // WHOLE-SESSION skip (sessions already captured elsewhere are ignored wholesale — per-request
        // cross-lane dedup is not wired, since those lanes don't write dedup keys).
        let capture_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // Honor [capture].mode = off: don't run the JSONL sweep at all. app_only and
        // always_on both capture (the difference is WHERE the daemon is launched from — app child vs
        // OS login item — which `tare capture sync` handles, not this thread). Also honor the
        // independent [capture].jsonl toggle: a user can keep the daemon/OTLP up but
        // switch the JSONL session-log lane off without turning capture off entirely.
        let capture_cfg = cfg.capture;
        let capture_off = capture_cfg.mode == tare_core::config::CaptureMode::Off;
        let jsonl_off = !capture_cfg.jsonl;
        let capture_handle = if capture_off || jsonl_off {
            eprintln!(
                "tare serve: JSONL session capture disabled ({})",
                if capture_off { "capture mode is off" } else { "[capture].jsonl = false" }
            );
            None
        } else {
            let stop = capture_stop.clone();
            let cap_db = db_path.to_string();
            std::thread::Builder::new()
                .name("tare-transcript-capture".into())
                .spawn(move || {
                    let dirs = crate::transcript_watch::resolve_project_dirs();
                    if dirs.is_empty() {
                        eprintln!("tare serve: no Claude Code projects dir found; JSONL capture idle");
                        return;
                    }
                    let mut cap = match crate::transcript_capture::TranscriptCapture::open(
                        &cap_db,
                        dirs,
                        "jsonl-live",
                    ) {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("tare serve: JSONL capture disabled: {e}");
                            return;
                        }
                    };
                    eprintln!("tare serve: JSONL session capture active (catching up…)");
                    let mut idle_streak: u32 = 0;
                    loop {
                        match cap.sweep() {
                            Ok(n) if n > 0 => {
                                idle_streak = 0;
                                eprintln!("tare serve: captured {n} new transcript step(s)")
                            }
                            Ok(_) => idle_streak = idle_streak.saturating_add(1),
                            Err(e) => eprintln!("tare serve: transcript sweep error: {e}"),
                        }
                        // Adaptive backoff: poll ~every 3s while steps are landing,
                        // stretching toward a ~30s cap after consecutive idle sweeps so an idle daemon
                        // stops walking the whole projects tree every 3s (frequent FS syscalls load
                        // this host's security stack). A landing step snaps back to 3s. Still wake
                        // every 100ms so Ctrl-C shutdown stays responsive.
                        for _ in 0..capture_backoff_ticks(idle_streak) {
                            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                                return;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                    }
                })
                .map(Some)
                .map_err(|e| format!("spawn JSONL capture thread: {e}"))?
        };
        // The out-of-band OTLP receiver uses a separate loopback port. It captures Claude
        // Code / Codex / SDK telemetry without sitting in the model request path; if its port is
        // taken, the proxy/UI keep running.
        let serve_result = match tokio::net::TcpListener::bind(("127.0.0.1", otlp_port)).await {
            Ok(otlp_listener) => {
                eprintln!(
                    "tare serve: OTLP receiver on http://127.0.0.1:{otlp_port} (POST /v1/traces|metrics|logs)"
                );
                // The bind succeeded → truthfully report the receiver as listening. The
                // read handler's `status_rd` clone shares this atomic; the Err branch leaves it false.
                otlp_status.set_listening(true);
                // Ctrl-C triggers the proxy's GRACEFUL shutdown (drain in-flight captures), which
                // returns from serve and completes the select.
                tokio::select! {
                    r = serve_with_shutdown_transcript(listener, config, proxy_sink, tsink, async { let _ = tokio::signal::ctrl_c().await; }) => r.map_err(|e| format!("serve: {e}")),
                    r = otel_receiver::serve_otlp(otlp_listener, otlp_span_sink, otlp_event_sink, otlp_metered_sink, otlp_privacy, otlp_status, activity) => {
                        r.map_err(|e| format!("otlp serve: {e}"))
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "tare serve: OTLP receiver disabled (bind :{otlp_port} failed: {e}); proxy/UI still running"
                );
                serve_with_shutdown_transcript(listener, config, proxy_sink, tsink, async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await
                .map_err(|e| format!("serve: {e}"))
            }
        };
        // Stop the JSONL capture loop and let its final sweep finish before we exit.
        capture_stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let capture_result = capture_handle.map_or(Ok(()), |handle| {
            handle
                .join()
                .map_err(|_| "JSONL capture thread panicked".to_string())
        });
        // The serve future has returned (graceful drain complete) and dropped the Sink sender;
        // drain the writer before exit.
        let writer_result = join_step_writer(writer);
        serve_result?;
        capture_result?;
        writer_result?;
        eprintln!("tare serve: stopped");
        Ok(())
    })
}

// ---- Reporting / rendering over the store ----

// The shared run cache now lives in tare-store so the desktop shell can use it too
// (tare-tauri must not depend on tare-cli). Re-exported here so the ~25 serve/CLI call sites keep
// their unqualified `cached_runs(db_path)?`.
pub use tare_store::cached_runs;

pub fn report_for(
    db_path: &str,
    today_only: bool,
    pricing: &PricingTable,
) -> Result<Report, String> {
    // Whole-runs, single-edition is the common hot path: serve from the shared run cache with no
    // store open at all. The today window stays a targeted date query; multi-edition still needs the
    // per-run capture days.
    let runs = if today_only {
        Arc::new(Store::open(db_path)?.load_runs_on_date(&today_local())?)
    } else {
        cached_runs(db_path)?
    };
    // Reprice each run as-of its capture day: honest per-run AsOf on a multi-edition
    // table. For a single-edition table (the common case) the dated path is only a SEMANTIC no-op —
    // row_on still linearly scans+sorts all rows per lookup — so pass an EMPTY day-map there to take
    // the O(log n) index path instead. Under `reprice = latest` the table is already
    // collapsed to one edition at load, so it's single-edition here too.
    let days = if pricing.is_multi_edition() {
        Store::open(db_path)?.run_days()?
    } else {
        std::collections::BTreeMap::new()
    };
    // Stamp the active privacy profile so the report states how its inputs were redacted.
    let policy = resolve_privacy()?;
    Ok(attribute::build_report_dated(&runs, pricing, &days).with_privacy(&policy))
}

pub fn render_report_text(report: &Report) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Tare report — estimated (pricing {}, effective {})\n",
        report.pricing_version, report.effective_date
    ));
    out.push_str(&format!(
        "Total estimated spend: {}\n\n",
        MicroUsd(report.total_micros).to_dollar_string()
    ));
    // Honest coarse-attribution note: out-of-band OTel/log captures lack prompt-
    // component detail, so component-level causes under-attribute.
    if report.attribution_confidence.as_deref() == Some("coarse") {
        out.push_str(
            "⚠ Attribution is COARSE — most spend was captured out-of-band (OTel/log) without \
             prompt-component detail; component causes are incomplete. Route through the inline \
             proxy for full attribution.\n\n",
        );
    }
    out.push_str(&format!(
        "{:<24} {:>9} {:>15} {:>15}\n",
        "CAUSE", "TOKENS", "DOLLARS(est)", "SAVE(est)"
    ));
    for row in &report.rows {
        out.push_str(&format!(
            "{:<24} {:>9} {:>15} {:>15}\n",
            row.cause,
            row.tokens,
            MicroUsd(row.micros).to_dollar_string(),
            MicroUsd(row.projected_saved_micros).to_dollar_string()
        ));
        out.push_str(&format!("    └ {}\n", row.detail));
    }
    if report.rows.is_empty() {
        out.push_str("(no spend recorded yet — run `tare run -- <cmd>`)\n");
    }
    if !report.unpriced.is_empty() {
        let (steps, tokens) = report.unpriced.iter().fold((0u32, 0u64), |(s, t), u| {
            (
                s.saturating_add(u.step_count),
                t.saturating_add(u.token_total),
            )
        });
        out.push_str(&format!(
            "\n⚠ {steps} step(s) ({tokens} tokens) on {} model(s) with NO bundled price — cost NOT included above:\n",
            report.unpriced.len()
        ));
        for u in &report.unpriced {
            out.push_str(&format!(
                "    {} / {} — {} tokens, {} step(s)\n",
                u.provider, u.model, u.token_total, u.step_count
            ));
        }
    }
    out
}

// ---- shareable redacted report bundle ----

const FULL_BUNDLE_CONTENTS: &str = "counts/weights/hashes + cause labels + model names + optional flamegraph; never request/response payload";
const PRIVATE_BUNDLE_CONTENTS: &str = "aggregate counts + integer micro-USD + fixed diagnostic text + redaction markers; no run, provider, model, adapter, or payload identity";

#[derive(serde::Serialize, serde::Deserialize)]
struct BundleManifest {
    tare_version: String,
    pricing_version: String,
    effective_date: String,
    privacy_policy_id: String,
    profile: String,
    scope: String,
    /// What the bundle is attested to contain — claimed EXACTLY (not a blanket "nothing
    /// sensitive"): counts/weights/hashes + fixed cause labels, never request/response payload.
    contains: String,
    labels_included: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ReportBundle {
    manifest: BundleManifest,
    report: Report,
    #[serde(skip_serializing_if = "Option::is_none")]
    flamegraph_svg: Option<String>,
    /// The savings ledger (top opportunities + spend-bounded capped potential + Savings Index).
    /// Omitted under max_private because its labels/fix-text name models and sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    savings: Option<tare_core::savings::SavingsLedger>,
    /// CostExperiment proof: the offline reprice-over-axes grid + cheapest equal-quality
    /// config, auto-populated from the cheaper priced candidates. Omitted under max_private (its cell
    /// labels name models) and when there's nothing cheaper to propose.
    #[serde(skip_serializing_if = "Option::is_none")]
    experiment: Option<tare_core::experiment::ExperimentResult>,
    /// Estimate-confidence (pricing age, unpriced share, coverage) — counts-only, so it's always
    /// included (even under max_private) to self-document the data quality.
    confidence: tare_core::confidence::EstimateConfidence,
}

/// Top-level keys a bundle is allowed to carry — anything else fails the scan (closed set).
const BUNDLE_KEYS: &[&str] = &[
    "manifest",
    "report",
    "flamegraph_svg",
    "savings",
    "experiment",
    "confidence",
];
const MANIFEST_KEYS: &[&str] = &[
    "tare_version",
    "pricing_version",
    "effective_date",
    "privacy_policy_id",
    "profile",
    "scope",
    "contains",
    "labels_included",
];

/// Fail-closed structural scan over the FINAL serialized bundle bytes: only allowlisted keys
/// may appear, the report must round-trip, and a `max_private` bundle must carry no flamegraph
/// (its node names embed model + adapter labels). Mirrors the on-disk sentinel guard, extended
/// to the outbound artifact.
fn scan_bundle(json: &str, max_private: bool) -> Result<(), String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("bundle json: {e}"))?;
    let obj = v.as_object().ok_or("bundle must be a JSON object")?;
    for k in obj.keys() {
        if !BUNDLE_KEYS.contains(&k.as_str()) {
            return Err(format!("bundle carries non-allowlisted key {k:?}"));
        }
    }
    let manifest = obj
        .get("manifest")
        .and_then(|m| m.as_object())
        .ok_or("bundle missing manifest")?;
    for k in manifest.keys() {
        if !MANIFEST_KEYS.contains(&k.as_str()) {
            return Err(format!("manifest carries non-allowlisted key {k:?}"));
        }
    }
    // Compare the complete nested shape with its typed serialization. Serde otherwise ignores
    // unknown fields by default, so merely parsing `Report` would let a nested payload-looking key
    // survive this final outbound check.
    let bundle: ReportBundle =
        serde_json::from_value(v.clone()).map_err(|e| format!("bundle shape: {e}"))?;
    let typed = serde_json::to_value(&bundle).map_err(|e| format!("bundle serialization: {e}"))?;
    if typed != v {
        return Err(
            "bundle JSON does not match the closed typed shape (unknown or noncanonical field)"
                .into(),
        );
    }
    if bundle.manifest.pricing_version != bundle.report.pricing_version
        || bundle.manifest.effective_date != bundle.report.effective_date
        || Some(bundle.manifest.privacy_policy_id.as_str())
            != bundle.report.privacy_policy_id.as_deref()
        || Some(bundle.manifest.profile.as_str()) != bundle.report.profile.as_deref()
    {
        return Err("bundle manifest disagrees with the embedded report metadata".into());
    }
    let expected_contents = if max_private {
        PRIVATE_BUNDLE_CONTENTS
    } else {
        FULL_BUNDLE_CONTENTS
    };
    if bundle.manifest.contains != expected_contents {
        return Err("bundle manifest contains-description does not match its profile".into());
    }
    if bundle.manifest.labels_included == max_private {
        return Err("bundle labels_included does not match its profile".into());
    }
    if max_private && obj.contains_key("flamegraph_svg") {
        return Err(
            "max_private bundle must not include a flamegraph (labels in node names)".into(),
        );
    }
    if max_private && obj.contains_key("savings") {
        return Err(
            "max_private bundle must not include the savings ledger (labels/fix-text name models + sessions)".into(),
        );
    }
    if max_private && obj.contains_key("experiment") {
        return Err(
            "max_private bundle must not include the cost experiment (cell labels name models)"
                .into(),
        );
    }
    if max_private {
        let private_policy = tare_core::PrivacyPolicy::max_private();
        if bundle.manifest.profile != "max_private"
            || bundle.manifest.privacy_policy_id != private_policy.policy_id()
        {
            return Err("max_private bundle carries the wrong privacy metadata".into());
        }
        if !matches!(bundle.manifest.scope.as_str(), "single-run" | "all-runs") {
            return Err("max_private bundle scope must not disclose a run identifier".into());
        }
        if bundle.report.unpriced.len() > 1
            || bundle
                .report
                .unpriced
                .iter()
                .any(|model| model.provider != "withheld" || model.model != "withheld")
        {
            return Err("max_private bundle discloses an unpriced provider or model".into());
        }
        for row in &bundle.report.rows {
            let expected = private_row_detail(&row.cause)
                .ok_or_else(|| format!("max_private bundle has unknown cause {:?}", row.cause))?;
            if row.detail != expected {
                return Err(format!(
                    "max_private bundle has noncanonical detail for cause {:?}",
                    row.cause
                ));
            }
        }
    }
    Ok(())
}

fn private_row_detail(cause: &str) -> Option<&'static str> {
    match cause {
        "retry-loop" => Some("Repeated-request cost; request content and identifiers withheld."),
        "bloated-system-prompt" => {
            Some("Repeated uncached system-prefix cost; content and identifiers withheld.")
        }
        "verbose-tool-output" => {
            Some("Large tool-result input cost; content and identifiers withheld.")
        }
        "cache-read-vs-write" => Some("Unread cache-write premium; identifiers withheld."),
        "coarse-attribution" => {
            Some("Spend lacks prompt-component detail; source and model identifiers withheld.")
        }
        _ => None,
    }
}

/// Remove identity-bearing strings from the report while retaining every numeric fact. Unpriced
/// entries are collapsed because even their count can otherwise be joined back to named models.
fn redact_private_bundle_report(mut report: Report) -> Result<Report, String> {
    for row in &mut report.rows {
        row.detail = private_row_detail(&row.cause)
            .ok_or_else(|| format!("cannot safely export unknown report cause {:?}", row.cause))?
            .to_string();
    }
    if !report.unpriced.is_empty() {
        let (token_total, step_count) =
            report
                .unpriced
                .iter()
                .fold((0u64, 0u32), |(tokens, steps), model| {
                    (
                        tokens.saturating_add(model.token_total),
                        steps.saturating_add(model.step_count),
                    )
                });
        report.unpriced = vec![tare_core::attribute::UnpricedModel {
            provider: "withheld".into(),
            model: "withheld".into(),
            token_total,
            step_count,
        }];
    }
    Ok(report)
}

/// Build a shareable bundle: a report (+ flamegraph SVG unless max_private) + a manifest
/// attesting exactly what it contains, then a fail-closed payload scan over the serialized
/// bytes. `--profile max_private` yields a counts-only artifact without a flamegraph.
pub fn report_bundle(
    db_path: &str,
    run_id: Option<&str>,
    max_private: bool,
    pricing: &PricingTable,
) -> Result<String, String> {
    let policy = if max_private {
        tare_core::PrivacyPolicy::max_private()
    } else {
        resolve_privacy()?
    };
    // The savings ledger surfaced in the bundle — only when labels are included
    // (max_private omits it: opportunity labels + fix-text name models/sessions).
    let ledger_for = |runs: &[tare_core::model::RunRecord]| {
        if max_private {
            None
        } else {
            Some(tare_core::savings::savings(runs, pricing))
        }
    };
    // CostExperiment proof: auto-propose a Model axis over the cheaper priced candidates
    // (from the what-if recommender) + the as-captured baseline, and reprice the grid offline. Omitted
    // under max_private (cell labels name models) and when nothing cheaper exists.
    let experiment_for = |runs: &[tare_core::model::RunRecord]| -> Result<
        Option<tare_core::experiment::ExperimentResult>,
        String,
    > {
        if max_private {
            return Ok(None);
        }
        use tare_core::experiment::{run_experiment, Axis, CostExperiment};
        let rec = tare_core::whatif::recommend(runs, pricing, false);
        let mut cands: Vec<String> = rec
            .recommendations
            .iter()
            .filter(|r| r.delta_micros < 0)
            .map(|r| format!("{}/{}", r.to_provider, r.to_model))
            .collect();
        cands.truncate(4); // keep the bundle bounded
        if cands.is_empty() {
            return Ok(None);
        }
        let mut models = vec![Axis::AS_CAPTURED.to_string()];
        models.extend(cands);
        run_experiment(
            runs,
            pricing,
            &CostExperiment {
                axes: vec![Axis::Model(models)],
            },
        )
        .map(Some)
    };
    let (mut report, flamegraph_svg, scope, savings, experiment, confidence) = match run_id {
        Some(r) => {
            let run = load_run(db_path, r)?;
            let runs = std::slice::from_ref(&run);
            let report = attribute::build_report(runs, pricing).with_privacy(&policy);
            let svg = if max_private {
                None
            } else {
                Some(svg::render_svg(&build_flamegraph(&run, pricing)))
            };
            let confidence = confidence_over(runs, pricing, &today_local());
            (
                report,
                svg,
                if max_private {
                    "single-run".to_string()
                } else {
                    format!("run:{r}")
                },
                ledger_for(runs),
                experiment_for(runs)?,
                confidence,
            )
        }
        None => {
            let runs = cached_runs(db_path)?;
            (
                report_for(db_path, false, pricing)?.with_privacy(&policy),
                None,
                "all-runs".to_string(),
                ledger_for(&runs),
                experiment_for(&runs)?,
                confidence_over(&runs, pricing, &today_local()),
            )
        }
    };
    if max_private {
        report = redact_private_bundle_report(report)?;
    }
    let manifest = BundleManifest {
        tare_version: VERSION.to_string(),
        pricing_version: report.pricing_version.clone(),
        effective_date: report.effective_date.clone(),
        privacy_policy_id: report.privacy_policy_id.clone().unwrap_or_default(),
        profile: report.profile.clone().unwrap_or_default(),
        scope,
        contains: if max_private {
            PRIVATE_BUNDLE_CONTENTS.into()
        } else {
            FULL_BUNDLE_CONTENTS.into()
        },
        labels_included: !max_private,
    };
    let bundle = ReportBundle {
        manifest,
        report,
        flamegraph_svg,
        savings,
        experiment,
        confidence,
    };
    let json = serde_json::to_string_pretty(&bundle).map_err(|e| e.to_string())?;
    scan_bundle(&json, max_private)?; // fail-closed before the caller writes it anywhere
    Ok(json)
}

/// One run's export content as a string, for the GUI's per-run export buttons. Formats reachable
/// from the core (speedscope/otel) plus the recomputable receipt. Estimated; payload-free.
pub fn export_content(
    db_path: &str,
    run_id: &str,
    format: &str,
    pricing: &PricingTable,
) -> Result<String, String> {
    let run = load_run(db_path, run_id)?;
    match format {
        "speedscope" => {
            Ok(speedscope::export(&build_flamegraph(&run, pricing), VERSION).to_string())
        }
        "otel" => Ok(tare_core::otel::export_otlp_json(&run, pricing, VERSION).to_string()),
        "receipt" => attest_receipt(db_path, Some(run_id), false, pricing),
        other => Err(format!("unknown export format {other:?}")),
    }
}

/// Node-level flame-diff between an explicit run pair. `normalize` selects share-mode.
/// Delegates to the shared `Store::flame_diff` so HTTP and Tauri return identical models.
pub fn flame_diff_for(
    db_path: &str,
    pricing: &PricingTable,
    run_a: &str,
    run_b: &str,
    normalize: bool,
) -> Result<tare_core::flame_diff::FlameDiffModel, String> {
    Store::open(db_path)?.flame_diff(run_a, run_b, normalize, pricing)
}

// ---- recomputable cost receipt (attest / verify) ----

/// Build a `tare-receipt v1` for a run (or all runs) — a portable, offline-recomputable cost
/// receipt. `max_private` removes correlation metadata, hashes, weights, timing, and the
/// flamegraph. Provider/model/vendor pricing identities remain because offline recomputation cannot
/// reproduce the cost without them.
pub fn attest_receipt(
    db_path: &str,
    run_id: Option<&str>,
    max_private: bool,
    pricing: &PricingTable,
) -> Result<String, String> {
    let policy = if max_private {
        tare_core::PrivacyPolicy::max_private()
    } else {
        resolve_privacy()?
    };
    let (runs, scope) = match run_id {
        Some(r) => (vec![load_run(db_path, r)?], format!("run:{r}")),
        None => (Store::open(db_path)?.load_runs()?, "all-runs".to_string()),
    };
    let report = attribute::build_report(&runs, pricing).with_privacy(&policy);
    let with_flamegraph = !max_private && runs.len() == 1;
    // Honest data-quality metadata stamped into the receipt.
    let confidence = confidence_over(&runs, pricing, &today_local());
    let receipt = tare_core::receipt::attest(
        &runs,
        &report,
        pricing,
        VERSION,
        &scope,
        with_flamegraph,
        confidence,
    );
    serde_json::to_string_pretty(&receipt).map_err(|e| e.to_string())
}

/// Verify a receipt file OFFLINE against the shipped/loaded pricing. Returns a human summary.
pub fn verify_receipt(path: &str, pricing: &PricingTable) -> Result<String, String> {
    let json = std::fs::read_to_string(path).map_err(|e| format!("read receipt {path}: {e}"))?;
    let v = tare_core::receipt::verify(&json, pricing)?;
    // when the loaded table differs from the receipt's, say we reproduced the figure at
    // contemporaneous (as-of) rates — the Trust/provenance line, so the number isn't mistaken for
    // today's rates.
    let pricing_line = match &v.reconciled_as_of {
        None => v.pricing_version.clone(),
        Some(date) => format!(
            "{} (reproduced as-of {date} from table {})",
            v.pricing_version, pricing.version
        ),
    };
    Ok(format!(
        "receipt OK (recomputation, not a cryptographic seal)\n  scope: {}\n  pricing: {}\n  recomputed total: {}\n  causes: {}\n  flamegraph re-rendered+matched: {}\n  digest: {:#018x}\n",
        v.scope,
        pricing_line,
        MicroUsd(v.recomputed_total_micros).to_dollar_string(),
        v.rows,
        v.flamegraph_checked,
        v.digest,
    ))
}

// ---- Daemon anomaly delivery ----

/// One daemon tick: build the daily Total trend from the store, detect spend anomalies, and
/// CLAIM each new `(date, series_key, kind)` in the persistent fired-set so it's delivered
/// exactly once. Returns only the anomalies newly fired this tick (already deduped). Pure of
/// any sink wiring and clock-free — `today` would only matter for stamping, and the detector
/// reads dates from the series. The caller emits the returned alerts through its sinks.
pub fn anomaly_delivery_tick(
    db_path: &str,
    pricing: &PricingTable,
    window: usize,
    threshold_pct: i64,
) -> Result<Vec<tare_core::alert::Alert>, String> {
    use tare_core::alert::{Alert, AlertLevel, AlertSubject};
    let store = Store::open(db_path)?;
    let Some(report) = trend_for(
        db_path,
        None,
        None,
        tare_core::trend::TrendDimension::Total,
        pricing,
    )?
    else {
        return Ok(Vec::new());
    };
    let mut fired = Vec::new();
    let cfg = load_config_strict()?;
    // Vantage-style noise floors keep low-value spikes from ever alerting; the persistent
    // fired-set below still owns "deliver once", so the config dedupe-window is a no-op here.
    let detected = tare_core::anomaly::filter_acknowledged(
        tare_core::anomaly::detect_filtered(
            &report,
            window,
            threshold_pct,
            cfg.anomaly.noise_filters(),
        ),
        &cfg.anomaly.acknowledged,
    );
    // Detection -> resolution: run the savings ledger over the same captured
    // data, so an alert says "spend jumped — here's the likely waste and the fix", not a dead end.
    // The top opportunities are the biggest recoverable spend; attach the top 1-2 as remediation.
    let remediation = {
        let led = savings_for(db_path, pricing)?;
        let tops: Vec<String> = led
            .opportunities
            .iter()
            .take(2)
            .map(|o| {
                format!(
                    "{} ({} avoidable) — {}",
                    o.label,
                    MicroUsd(o.recoverable_micros).to_dollar_string(),
                    o.fix_text
                )
            })
            .collect();
        if tops.is_empty() {
            String::new()
        } else {
            format!(" Likely waste → {}", tops.join("; "))
        }
    };

    for a in detected {
        // Severity gate: minor spikes don't alarm (they'd re-fire forever as noise).
        if a.materiality == "minor" {
            continue;
        }
        let subject = AlertSubject::Anomaly {
            date: a.date.clone(),
            series_key: a.series_key.clone(),
            kind: format!("{:?}", a.kind),
        };
        // Atomic claim: only the first tick to see this key gets to fire it.
        if store.claim_alert(&subject.dedup_key(), &a.date)? {
            fired.push(Alert {
                level: AlertLevel::Warn,
                subject,
                message: format!(
                    "daily spend {} vs trailing baseline {} ({} impact).{}",
                    MicroUsd(a.value_micros).to_dollar_string(),
                    MicroUsd(a.baseline_micros).to_dollar_string(),
                    a.materiality,
                    remediation,
                ),
            });
        }
    }
    Ok(fired)
}

/// One complete background-monitor tick: built-in spend anomalies, periodic-budget notices, and
/// every configured `[[alert]]` rule. Each observation is atomically claimed in the store before it
/// is returned, so a fast loop or a second daemon cannot deliver it twice.
pub fn monitor_delivery_tick(
    db_path: &str,
    pricing: &PricingTable,
    today: &str,
    anomaly_window: usize,
    anomaly_threshold_pct: i64,
) -> Result<Vec<tare_core::alert::Alert>, String> {
    let mut fired = anomaly_delivery_tick(db_path, pricing, anomaly_window, anomaly_threshold_pct)?;
    let cfg = load_config_strict()?;
    let budget = period_budget_for(db_path, pricing)?;
    let captured_events = Store::open(db_path)?
        .source_counts()?
        .values()
        .fold(0u64, |total, count| total.saturating_add(*count));

    let needs_today = cfg.alert.iter().any(|rule| rule.metric == "today_spend");
    let today_micros = if needs_today {
        today_spend_for(db_path, today, pricing)?.total_micros
    } else {
        0
    };
    let needs_run_rate = cfg.alert.iter().any(|rule| rule.metric == "run_rate");
    let run_rate_micros_per_day = if needs_run_rate {
        Some(
            serde_json::from_str::<tare_core::burnrate::BurnRate>(&burnrate_for(db_path, pricing)?)
                .map_err(|error| format!("decode run-rate monitor input: {error}"))?
                .run_rate_micros_per_day,
        )
    } else {
        None
    };

    let anomaly_rules: Vec<&tare_core::config::AlertRule> = cfg
        .alert
        .iter()
        .filter(|rule| rule.metric == "anomaly_kind")
        .collect();
    let anomalies = if anomaly_rules.iter().any(|rule| rule.window_days.is_none()) {
        anomalies_for(
            db_path,
            tare_core::trend::TrendDimension::Total,
            anomaly_window,
            anomaly_threshold_pct,
            pricing,
        )?
    } else {
        Vec::new()
    };
    let mut anomalies_by_window = std::collections::BTreeMap::new();
    for window in anomaly_rules
        .iter()
        .filter_map(|rule| rule.window_days)
        .collect::<std::collections::BTreeSet<_>>()
    {
        anomalies_by_window.insert(
            window,
            anomalies_for(
                db_path,
                tare_core::trend::TrendDimension::Total,
                window as usize,
                anomaly_threshold_pct,
                pricing,
            )?,
        );
    }

    let snapshot = tare_core::alert::MonitorSnapshot {
        day: today,
        today_micros,
        budget: &budget,
        run_rate_micros_per_day,
        anomalies: &anomalies,
        anomalies_by_window: &anomalies_by_window,
        captured_events,
    };
    let store = Store::open(db_path)?;
    for alert in tare_core::alert::evaluate_monitor_alerts(&cfg.alert, &snapshot) {
        if store.claim_alert(&alert.subject.dedup_key(), today)? {
            fired.push(alert);
        }
    }
    Ok(fired)
}

// ---- spend anomaly detection ----

/// Detect spend anomalies over a window and dimension. Builds the trend, then runs the pure
/// detector. `by` selects the trend dimension (total/provider/model/cause).
pub fn anomalies_for(
    db_path: &str,
    by: tare_core::trend::TrendDimension,
    window: usize,
    threshold_pct: i64,
    pricing: &PricingTable,
) -> Result<Vec<tare_core::anomaly::Anomaly>, String> {
    let Some(trend) = trend_for(db_path, None, None, by, pricing)? else {
        return Ok(Vec::new());
    };
    let cfg = load_config_strict()?;
    // Vantage-style noise floors + dedupe, then hide acknowledged false positives;
    // materiality rides on each returned anomaly. Both default to permissive/empty.
    Ok(tare_core::anomaly::filter_acknowledged(
        tare_core::anomaly::detect_filtered(
            &trend,
            window,
            threshold_pct,
            cfg.anomaly.noise_filters(),
        ),
        &cfg.anomaly.acknowledged,
    ))
}

// ---- cost-regression bisection ----

/// Find the first day the daily spend (total, or a chosen cause series) crossed `threshold_pct`
/// above the trailing-`window`-day median (K7b). Builds the dense series from the trend engine.
/// Commits in chronological (oldest-first) order, as 12-character short SHAs matching the stamped
/// `commit` label. Runs `git rev-list --reverse --abbrev-commit --abbrev=12 HEAD` at
/// the edge; empty if not a repo.
pub fn git_commit_order(cwd: &std::path::Path) -> Result<Vec<String>, String> {
    let output = std::process::Command::new("git")
        .args([
            "rev-list",
            "--reverse",
            "--abbrev-commit",
            "--abbrev=12",
            "HEAD",
        ])
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("run git rev-list in {}: {e}", cwd.display()))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "git rev-list failed in {} ({}): {}",
            cwd.display(),
            output.status,
            detail.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect())
}

/// Align a `rollup --by commit` report to `ordered_commits`, keeping ONLY commits that have captured
/// spend (in the given order) → `(commit labels, cost series)`. Pure — feeds `bisect`.
pub fn commit_cost_series(
    rollup: &tare_core::rollup::RollupReport,
    ordered_commits: &[String],
) -> (Vec<String>, Vec<i64>) {
    let costs: std::collections::BTreeMap<&str, i64> = rollup
        .rows
        .iter()
        .map(|r| (r.label.as_str(), r.micros))
        .collect();
    let mut labels = Vec::new();
    let mut values = Vec::new();
    for c in ordered_commits {
        if let Some(&m) = costs.get(c.as_str()) {
            labels.push(c.clone());
            values.push(m);
        }
    }
    (labels, values)
}

/// Bisect a cost regression across git history: order captured commits by git
/// chronology, join with their `rollup --by commit` cost, and find the first commit whose spend
/// crossed the threshold above the trailing median. The returned `Regression.date` holds the commit.
pub fn bisect_git_for(
    db_path: &str,
    cwd: &std::path::Path,
    window: usize,
    threshold_pct: i64,
    pricing: &PricingTable,
) -> Result<Option<tare_core::bisect::Regression>, String> {
    let runs = cached_runs(db_path)?;
    let rep = tare_core::rollup::rollup(&runs, pricing, tare_core::rollup::RollupDim::Commit);
    let commits = git_commit_order(cwd)?;
    let (labels, values) = commit_cost_series(&rep, &commits);
    Ok(tare_core::bisect::bisect(
        &labels,
        &values,
        window,
        threshold_pct,
    ))
}

pub fn bisect_for(
    db_path: &str,
    cause: Option<&str>,
    window: usize,
    threshold_pct: i64,
    pricing: &PricingTable,
) -> Result<Option<tare_core::bisect::Regression>, String> {
    let dim = if cause.is_some() {
        tare_core::trend::TrendDimension::ByCause
    } else {
        tare_core::trend::TrendDimension::Total
    };
    let Some(trend) = trend_for(db_path, None, None, dim, pricing)? else {
        return Ok(None);
    };
    let values: Vec<i64> = match cause {
        Some(c) => match trend.series.iter().find(|s| s.key == c) {
            Some(s) => s.per_day.clone(),
            None => return Ok(None), // that cause never appeared
        },
        None => trend
            .series
            .first()
            .map(|s| s.per_day.clone())
            .unwrap_or_default(),
    };
    Ok(tare_core::bisect::bisect(
        &trend.days,
        &values,
        window,
        threshold_pct,
    ))
}

// ---- prompt-cache advisor ----

pub fn advise_for(
    db_path: &str,
    pricing: &PricingTable,
) -> Result<Vec<tare_core::advise::CacheAdvice>, String> {
    let runs = cached_runs(db_path)?;
    Ok(tare_core::advise::advise(&runs, pricing))
}

/// Build the `tare share` single-file redacted HTML report: the Savings Ledger plus that run's
/// inline flamegraph SVG when `run` is given. Fully offline and self-contained.
pub fn share_html_for(
    db_path: &str,
    pricing: &PricingTable,
    run: Option<&str>,
    title: &str,
    generated_label: &str,
    experiment_to: &[String],
) -> Result<String, String> {
    let ledger = savings_for(db_path, pricing)?;
    let svg = match run {
        Some(r) => Some(svg::render_svg(&build_flamegraph(
            &load_run(db_path, r)?,
            pricing,
        ))),
        None => None,
    };
    // Optional CostExperiment: a Model axis over the given candidates + the baseline.
    let experiment = if experiment_to.is_empty() {
        None
    } else {
        use tare_core::experiment::{run_experiment, Axis, CostExperiment};
        let runs = cached_runs(db_path)?;
        let mut models = vec![Axis::AS_CAPTURED.to_string()];
        models.extend(experiment_to.iter().cloned());
        let exp = CostExperiment {
            axes: vec![Axis::Model(models)],
        };
        Some(run_experiment(&runs, pricing, &exp)?)
    };
    Ok(tare_core::share::render_share_html(
        title,
        &ledger,
        generated_label,
        svg.as_deref(),
        experiment.as_ref(),
    ))
}

/// Today's estimated spend. The `date` boundary is supplied by the caller (clock-free
/// core); the CLI passes the local calendar day.
pub fn today_spend_for(
    db_path: &str,
    date: &str,
    pricing: &PricingTable,
) -> Result<tare_core::model::TodaySpend, String> {
    Store::open(db_path)?.today_spend(date, pricing)
}

/// Render today's spend. `oneline` is a compact string for a tmux/shell prompt or a menubar/
/// statusline widget; the default is a short human block. 2-decimal dollars (never 6-dp) for a
/// glanceable widget.
pub fn render_today_text(t: &tare_core::model::TodaySpend, date: &str, oneline: bool) -> String {
    let money = MicroUsd(t.total_micros).to_dollar_string_2dp();
    if oneline {
        // e.g. "tare ⟡ $1.23 today · 5 runs" — one line, no trailing newline (prompt-friendly).
        format!("tare ⟡ {money} today · {} run(s)", t.run_count)
    } else {
        format!(
            "{date}: {money} estimated across {} run(s) [pricing {}]\n",
            t.run_count, t.pricing_version
        )
    }
}

/// Parsed subset of Claude Code's statusLine stdin JSON. Tolerant: every field is
/// optional because Claude Code omits or nulls fields early in a session (`context_window` before the
/// first API call, `effort`/`rate_limits` by plan). We read only what the status line needs.
#[derive(Debug, Default, PartialEq)]
pub struct StatusLineInput {
    pub session_id: Option<String>,
    pub model_label: Option<String>,
    /// Claude Code's own reported cumulative session cost (`cost.total_cost_usd`) — the VENDOR figure.
    pub vendor_cost_usd: Option<f64>,
    pub ctx_used_pct: Option<f64>,
    pub exceeds_200k: bool,
}

/// Parse the statusLine JSON Claude Code passes on stdin. NEVER fails — an unparseable or partial
/// payload yields defaults so the status line still renders (Claude Code invokes this constantly and a
/// hard error would blank the user's status line). Schema per code.claude.com/docs/en/statusline.
pub fn parse_statusline_input(json: &str) -> StatusLineInput {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let model_label = v.get("model").and_then(|m| {
        m.get("display_name")
            .and_then(|s| s.as_str())
            .or_else(|| m.get("id").and_then(|s| s.as_str()))
            .map(String::from)
    });
    StatusLineInput {
        session_id: v
            .get("session_id")
            .and_then(|s| s.as_str())
            .map(String::from),
        model_label,
        vendor_cost_usd: v
            .get("cost")
            .and_then(|c| c.get("total_cost_usd"))
            .and_then(|x| x.as_f64()),
        ctx_used_pct: v
            .get("context_window")
            .and_then(|c| c.get("used_percentage"))
            .and_then(|x| x.as_f64()),
        exceeds_200k: v
            .get("exceeds_200k_tokens")
            .and_then(|b| b.as_bool())
            .unwrap_or(false),
    }
}

/// Render the one-line status. Tare's own session estimate is the PRIMARY figure;
/// Claude Code's reported `total_cost_usd` is a labelled VENDOR cross-check, never merged into the
/// ledger (estimate-honesty). A `Δ` flags a material (>15%) divergence between the two — the whole
/// point of the cross-check. Plain text (no ANSI) so it composes into any statusLine wrapper.
pub fn render_statusline(tare_micros: Option<i64>, inp: &StatusLineInput) -> String {
    let mut parts: Vec<String> = Vec::new();
    match tare_micros {
        Some(m) => parts.push(format!("tare {} est", MicroUsd(m).to_dollar_string_2dp())),
        // Session not captured locally — show the vendor figure alone rather than a fabricated $0.
        None => parts.push("tare —".to_string()),
    }
    if let Some(vendor_micros) = inp
        .vendor_cost_usd
        .and_then(tare_core::config::dollars_to_micros)
    {
        let mut seg = format!("CC {}", MicroUsd(vendor_micros).to_dollar_string_2dp());
        if let Some(tm) = tare_micros {
            // Only flag divergence when the vendor figure is non-trivial (> $0.01), to avoid noise.
            if vendor_micros > 10_000
                && (i128::from(tm) - i128::from(vendor_micros)).abs() * 100
                    > i128::from(vendor_micros) * 15
            {
                seg.push_str(" Δ");
            }
        }
        parts.push(seg);
    }
    if let Some(m) = &inp.model_label {
        parts.push(m.clone());
    }
    if let Some(p) = inp.ctx_used_pct {
        let warn = if inp.exceeds_200k { " ⚠" } else { "" };
        parts.push(format!("ctx {}%{warn}", p.round() as i64));
    } else if inp.exceeds_200k {
        parts.push("ctx ⚠".to_string());
    }
    parts.join(" · ")
}

/// Build the status line for a stdin payload: parse it, look up Tare's estimate for the reported
/// session (by `session_id`), and render. Best-effort — any lookup failure yields a `tare —` primary
/// rather than an error, so the status line never breaks Claude Code.
pub fn statusline_for(db_path: &str, pricing: &PricingTable, stdin_json: &str) -> String {
    let inp = parse_statusline_input(stdin_json);
    // Forward the vendor's cumulative session cost into the cross-check store, UPSERTing
    // the latest per session so repeated statusLine calls can't double-count. Best-effort: the status
    // line renders constantly and must never fail on a store hiccup. A labelled cross-check, never
    // merged into the estimate.
    if let (Some(sid), Some(micros)) = (
        inp.session_id.as_deref(),
        inp.vendor_cost_usd
            .and_then(tare_core::config::dollars_to_micros),
    ) {
        if let Ok(store) = Store::open(db_path) {
            let _ = store.upsert_vendor_session_cost(sid, micros, &today_local());
        }
    }
    // Cost ONLY this session's run, not the entire history. The statusLine fires
    // ~once per assistant turn in a fresh, short-lived process, so the run cache is always cold —
    // re-costing all 157K steps just to keep one session's total was the single most frequent waste.
    // For a Claude Code session every step is written with `run_id == session id`, so `load_run(sid)`
    // returns exactly this session's steps; `sessions()` over that one run yields its bucket. If a
    // step carried a different session label than its run id, the row wouldn't match `sid` and we'd
    // render no estimate — never a wrong number.
    let tare_micros = inp.session_id.as_deref().and_then(|sid| {
        let store = Store::open(db_path).ok()?;
        let run = store.load_run(sid).ok()??;
        tare_core::session::sessions(&[run], pricing)
            .rows
            .into_iter()
            .find(|r| r.session == sid)
            .map(|r| r.micros)
    });
    render_statusline(tare_micros, &inp)
}

/// Calendar heatmap of daily spend intensity. Builds a dense `(date, micros)` series
/// from the stored dated runs over the full captured span, optionally trimmed to the last `window`
/// days. Offline; the day×hour punchcard half is deferred (no hour capture).
pub fn heatmap_for(
    db_path: &str,
    pricing: &PricingTable,
    window: Option<usize>,
) -> Result<tare_core::heatmap::HeatmapModel, String> {
    let store = Store::open(db_path)?;
    let Some((from, to)) = store.run_date_bounds()? else {
        return Ok(tare_core::heatmap::calendar_heatmap(&[]));
    };
    let dated: Vec<(tare_core::model::RunRecord, String)> = store
        .load_dated_runs_in_range(&from, &to)?
        .into_iter()
        .map(|d| (d.run, d.date))
        .collect();
    Ok(tare_core::heatmap::heatmap_from_runs(
        &dated, pricing, window,
    ))
}

/// Day×hour spend punchcard: buckets each captured run's priced cost into its
/// `(weekday, hour)` cell. The hour is the one stamped at ingest from the turn's OWN timestamp (UTC;
/// localization is a follow-on). Runs with no stored hour — pre-migration rows, or lanes that don't
/// carry a per-turn timestamp — are an honest GAP (excluded, never bucketed at a fake hour). Offline.
pub fn punchcard_for(
    db_path: &str,
    pricing: &PricingTable,
) -> Result<tare_core::punchcard::PunchcardModel, String> {
    let store = Store::open(db_path)?;
    let day_hours: std::collections::HashMap<String, (String, Option<u8>)> = store
        .run_day_hours()?
        .into_iter()
        .map(|(id, date, hour)| (id, (date, hour)))
        .collect();
    Ok(tare_core::punchcard::punchcard_from_runs(
        &cached_runs(db_path)?,
        &day_hours,
        pricing,
    ))
}

/// Cache-economics anti-pattern hunter: per-template cache health + volatile-prefix
/// sessions, each with its $ impact. Fully offline.
pub fn cache_health_for(
    db_path: &str,
    pricing: &PricingTable,
) -> Result<tare_core::cache_health::CacheHealthReport, String> {
    let runs = cached_runs(db_path)?;
    Ok(tare_core::cache_health::cache_health(&runs, pricing))
}

/// Render the cache-health report. Losses are MEASURED premiums paid; saveable is PROJECTED.
pub fn render_cache_health_text(r: &tare_core::cache_health::CacheHealthReport) -> String {
    let mut out = String::new();
    out.push_str(
        "Cache health — ESTIMATE; the specific prompt-caching anti-patterns + their cost\n\n",
    );
    out.push_str(&format!(
        "  Lost to single-use writes: {}   ·   Saveable by caching repeats: {}   ({} templates examined)\n\n",
        MicroUsd(r.total_lost_micros).to_dollar_string_2dp(),
        MicroUsd(r.total_saveable_micros).to_dollar_string_2dp(),
        r.templates_examined
    ));
    if r.flagged.is_empty() {
        out.push_str("  No cache anti-patterns found.\n");
    } else {
        for t in &r.flagged {
            let ratio = if t.read_ratio_pct < 0 {
                "n/a".to_string()
            } else {
                format!("{}%", t.read_ratio_pct)
            };
            let kind = match t.impact_kind.as_str() {
                "lost" => " lost",
                "saveable" => " saveable",
                _ => "",
            };
            let money = if t.impact_micros != 0 {
                format!(
                    "  {}{}",
                    MicroUsd(t.impact_micros).to_dollar_string_2dp(),
                    kind
                )
            } else {
                String::new()
            };
            out.push_str(&format!(
                "  [{}] {} {} — {} send(s), cache-read {}{}{}\n",
                t.antipattern,
                t.model,
                t.template,
                t.sends,
                ratio,
                money,
                if t.volatile_session {
                    "  (volatile prefix)"
                } else {
                    ""
                }
            ));
        }
    }
    if !r.volatile_sessions.is_empty() {
        out.push_str(
            "\n  Volatile-prefix sessions — cache_control sits on content that changes every send,\n  so each \"cached\" write is a cold write that's never reused. Move the breakpoint to a stable block:\n",
        );
        for v in &r.volatile_sessions {
            out.push_str(&format!(
                "    session {}: {} cached sends, {} distinct prefixes → {} in writes never reused\n",
                v.session,
                v.cached_sends,
                v.distinct_prefixes,
                MicroUsd(v.lost_micros).to_dollar_string_2dp()
            ));
        }
    }
    out
}

/// Non-floor advisories: batch-eligibility, reasoning-effort, compression band,
/// and cache scorecard — "things to look at" whose at-risk figure is deliberately NOT a recoverable
/// dollar (see `tare_core::advisory`). Kept separate from `savings` so the headline stays honest.
pub fn advisories_for(
    db_path: &str,
    pricing: &PricingTable,
) -> Result<Vec<tare_core::advisory::Advisory>, String> {
    let runs = cached_runs(db_path)?;
    Ok(tare_core::advisory::advisories(&runs, pricing))
}

/// Render the advisory list. At-risk dollars are labelled as upper bounds, never savings.
pub fn render_advisories_text(adv: &[tare_core::advisory::Advisory]) -> String {
    let mut out = String::new();
    out.push_str(
        "Tare advisories — ESTIMATE; at-risk/upper-bound figures, NOT counted as recoverable\n\n",
    );
    if adv.is_empty() {
        out.push_str("No advisories — no batch/reasoning/compression/cache patterns to flag.\n");
        return out;
    }
    for a in adv {
        let risk = if a.at_risk_micros > 0 {
            format!(
                " (~{} at risk)",
                MicroUsd(a.at_risk_micros).to_dollar_string()
            )
        } else {
            String::new()
        };
        out.push_str(&format!(
            "[{}] {}{}\n  {}\n",
            a.kind, a.headline, risk, a.detail
        ));
    }
    out
}

/// Budget goals & streaks: build a dense daily series (spend + input-token classes)
/// from the stored dated runs and evaluate it against the user's goals. The date range is the full
/// captured span (clock-free — bounds come from the store), optionally trimmed to the last `window`
/// days. Fully offline.
pub fn streaks_for(
    db_path: &str,
    pricing: &PricingTable,
    goals: &tare_core::streaks::Goals,
    window: Option<usize>,
) -> Result<tare_core::streaks::StreakReport, String> {
    use std::collections::BTreeMap;
    use tare_core::streaks::{evaluate_streaks, DayStats};
    goals.validate()?;
    if window == Some(0) {
        return Err("streak window must be at least 1".into());
    }
    let store = Store::open(db_path)?;
    let Some((from, to)) = store.run_date_bounds()? else {
        return Ok(evaluate_streaks(&[], goals));
    };
    let dated = store.load_dated_runs_in_range(&from, &to)?;
    // Bucket runs by date: cost (via build_report) + input-token classes.
    let mut micros: BTreeMap<String, i64> = BTreeMap::new();
    let mut read: BTreeMap<String, u64> = BTreeMap::new();
    let mut input: BTreeMap<String, u64> = BTreeMap::new();
    let mut unpriced: BTreeMap<String, bool> = BTreeMap::new();
    for d in &dated {
        let report = tare_core::attribute::build_report(std::slice::from_ref(&d.run), pricing);
        let day_micros = micros.entry(d.date.clone()).or_insert(0);
        *day_micros = day_micros.saturating_add(report.total_micros.max(0));
        // A day with any unpriced-model spend has an under-counted `micros` (#6, review).
        if !report.unpriced.is_empty() {
            unpriced.insert(d.date.clone(), true);
        }
        for step in &d.run.steps {
            let u = &step.usage;
            let day_read = read.entry(d.date.clone()).or_insert(0);
            *day_read = day_read.saturating_add(u.cache_read);
            let day_input = input.entry(d.date.clone()).or_insert(0);
            *day_input = day_input.saturating_add(u.total_prompt());
        }
    }
    // Dense calendar series (idle days present, with zeros).
    let mut series: Vec<DayStats> = tare_core::calendar::days_between(&from, &to)
        .into_iter()
        .map(|date| DayStats {
            micros: micros.get(&date).copied().unwrap_or(0),
            cache_read_tokens: read.get(&date).copied().unwrap_or(0),
            input_total_tokens: input.get(&date).copied().unwrap_or(0),
            has_unpriced: unpriced.get(&date).copied().unwrap_or(false),
            date,
        })
        .collect();
    if let Some(n) = window {
        if series.len() > n {
            series.drain(0..series.len() - n);
        }
    }
    Ok(evaluate_streaks(&series, goals))
}

/// Render the streak report. Counts-only — a met day is a plain ✓, a missed day a ·, no nag.
pub fn render_streaks_text(r: &tare_core::streaks::StreakReport) -> String {
    let mut out = String::new();
    if !r.goals.any_set() {
        return "No budget goals set. Try --max-daily-usd <D> and/or --min-cache-read-pct <N>.\n"
            .to_string();
    }
    out.push_str("Budget streaks — ESTIMATE; your goals, your thresholds (counts only)\n\n");
    if let Some(m) = r.goals.max_daily_micros {
        out.push_str(&format!(
            "  goal: stay at or under {}/day\n",
            MicroUsd(m).to_dollar_string_2dp()
        ));
    }
    if let Some(p) = r.goals.min_cache_read_pct {
        out.push_str(&format!("  goal: cache-read ratio ≥ {p}%\n"));
    }
    out.push_str(&format!(
        "\n  current streak: {} day(s)   |   longest: {} day(s)\n\n",
        r.current_streak, r.longest_streak
    ));
    for d in &r.days {
        let mark = if d.budget_unknown {
            "?"
        } else if d.met_all {
            "✓"
        } else {
            "·"
        };
        let note = if d.budget_unknown {
            "  (unpriced spend — budget unknown)"
        } else {
            ""
        };
        out.push_str(&format!(
            "  {} {}   {:>10}   cache {:>3}%{}\n",
            mark,
            d.date,
            MicroUsd(d.micros).to_dollar_string_2dp(),
            d.cache_read_pct,
            note
        ));
    }
    out
}

/// Provider-invoice reconciliation: parse the user-supplied CSV and compare its
/// per-model totals against Tare's per-model estimate (from the by-model rollup). Fully offline.
pub fn invoice_reconcile_for(
    db_path: &str,
    pricing: &PricingTable,
    csv_text: &str,
) -> Result<tare_core::reconcile::ReconReport, String> {
    use tare_core::rollup::{rollup, RollupDim};
    let invoice = tare_core::reconcile::parse_invoice_csv(csv_text)?;
    let runs = cached_runs(db_path)?;
    let by_model: std::collections::BTreeMap<String, i64> =
        rollup(&runs, pricing, RollupDim::Model)
            .rows
            .into_iter()
            .map(|r| (r.label, r.micros))
            .collect();
    Ok(tare_core::reconcile::reconcile(&invoice, &by_model))
}

/// Render the reconciliation. Deltas are `estimate − invoice`; a coverage gap is called out.
pub fn render_invoice_reconcile_text(r: &tare_core::reconcile::ReconReport) -> String {
    let mut out = String::new();
    out.push_str(
        "Invoice reconciliation — ESTIMATE vs your provider bill (offline; nothing sent)\n\n",
    );
    out.push_str(&format!(
        "{:<28} {:>14} {:>14} {:>14} {:>8}\n",
        "model", "invoice", "estimate", "delta", "delta%"
    ));
    for row in &r.rows {
        let flag = match (row.on_invoice, row.on_estimate) {
            (true, false) => "  (not captured)",
            (false, true) => "  (not billed)",
            _ => "",
        };
        out.push_str(&format!(
            "{:<28} {:>14} {:>14} {:>14} {:>7}%{}\n",
            row.model,
            MicroUsd(row.invoice_micros).to_dollar_string(),
            MicroUsd(row.estimated_micros).to_dollar_string(),
            MicroUsd(row.delta_micros).to_dollar_string(),
            row.delta_bps / 100,
            flag,
        ));
    }
    out.push_str(&format!(
        "\n{:<28} {:>14} {:>14} {:>14}\n",
        "TOTAL",
        MicroUsd(r.invoice_total_micros).to_dollar_string(),
        MicroUsd(r.estimated_total_micros).to_dollar_string(),
        MicroUsd(r.delta_micros).to_dollar_string(),
    ));
    if !r.uncovered_invoice_models.is_empty() {
        out.push_str(&format!(
            "\nCoverage gap — billed but not captured by Tare: {}\n",
            r.uncovered_invoice_models.join(", ")
        ));
    }
    if !r.unbilled_estimate_models.is_empty() {
        out.push_str(&format!(
            "Estimated but not on this invoice (other period, or self-hosted overlay): {}\n",
            r.unbilled_estimate_models.join(", ")
        ));
    }
    out
}

/// Realized cache-savings ledger: money already kept because cache reads were billed
/// at the read rate instead of the base input rate they'd have cost as fresh input.
pub fn cache_ledger_for(
    db_path: &str,
    pricing: &PricingTable,
) -> Result<tare_core::cache_ledger::CacheLedger, String> {
    let runs = cached_runs(db_path)?;
    Ok(tare_core::cache_ledger::cache_ledger(&runs, pricing))
}

/// Reasoning/thinking-token breakout: output spend split into answer vs reasoning.
pub fn reasoning_for(
    db_path: &str,
    pricing: &PricingTable,
) -> Result<tare_core::reasoning::ReasoningBreakout, String> {
    let runs = cached_runs(db_path)?;
    Ok(tare_core::reasoning::reasoning_breakout(&runs, pricing))
}

// ---- `tare doctor`: setup self-check ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorStatus {
    Ok,
    Warn,
    Bad,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCheck {
    pub status: DoctorStatus,
    pub name: String,
    pub message: String,
    /// The exact fix to print when not Ok.
    pub fix: Option<String>,
}

impl DoctorCheck {
    fn ok(name: &str, message: String) -> Self {
        Self {
            status: DoctorStatus::Ok,
            name: name.into(),
            message,
            fix: None,
        }
    }
    fn warn(name: &str, message: String, fix: &str) -> Self {
        Self {
            status: DoctorStatus::Warn,
            name: name.into(),
            message,
            fix: Some(fix.into()),
        }
    }
    fn bad(name: &str, message: String, fix: &str) -> Self {
        Self {
            status: DoctorStatus::Bad,
            name: name.into(),
            message,
            fix: Some(fix.into()),
        }
    }
}

/// True if something is accepting connections on `127.0.0.1:port` (a quick liveness probe).
fn port_listening(port: u16) -> bool {
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

/// Minimal blocking HTTP/1.0 GET over loopback (no async runtime, no extra deps). Returns the
/// response body, or `None` if unreachable / timed out. Used to read `/__tare/otlp_status`.
fn http_get_local(port: u16, path: &str) -> Option<String> {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(800))).ok()?;
    s.set_write_timeout(Some(Duration::from_millis(800))).ok()?;
    let req = format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).ok()?;
    let mut buf = String::new();
    s.take(64 * 1024).read_to_string(&mut buf).ok()?;
    let (head, body) = buf.split_once("\r\n\r\n")?;
    let status = head
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse::<u16>()
        .ok()?;
    (200..300).contains(&status).then(|| body.to_string())
}

/// Pure classification of the capture state — the heart of `tare doctor` (unit-tested).
pub fn capture_verdict(receiver_listening: bool, events: Option<u64>) -> DoctorCheck {
    if !receiver_listening {
        return DoctorCheck::bad(
            "capture",
            "OTLP receiver not reachable".into(),
            "start it with `tare up` (or `tare serve`)",
        );
    }
    match events {
        Some(n) if n > 0 => {
            DoctorCheck::ok("capture", format!("receiver live, {n} event(s) captured"))
        }
        _ => DoctorCheck::warn(
            "capture",
            "receiver listening but 0 events captured".into(),
            "restart your agent, or wire it with `tare detect` / `tare connect`",
        ),
    }
}

/// Does a settings/config file plausibly point telemetry at OUR loopback receiver?
fn file_points_at(path: &str, otlp_port: u16) -> bool {
    std::fs::read_to_string(path)
        .map(|s| {
            (s.contains("OTEL_EXPORTER_OTLP_ENDPOINT") || s.contains("otlp-http"))
                && (s.contains(&format!("127.0.0.1:{otlp_port}"))
                    || s.contains(&format!("localhost:{otlp_port}")))
        })
        .unwrap_or(false)
}

/// Which agents are wired to this receiver (Claude Code global/project, Codex).
fn agent_wiring_check(otlp_port: u16) -> DoctorCheck {
    let home = home_dir().unwrap_or_default();
    let mut wired = Vec::new();
    if !home.is_empty() && file_points_at(&format!("{home}/.claude/settings.json"), otlp_port) {
        wired.push("Claude Code (global)");
    }
    if file_points_at(".claude/settings.local.json", otlp_port) {
        wired.push("Claude Code (project)");
    }
    if file_points_at(&codex_connect::default_codex_config(), otlp_port) {
        wired.push("Codex");
    }
    if wired.is_empty() {
        DoctorCheck::warn(
            "agents",
            "no agent is wired to this receiver".into(),
            "run `tare detect` (auto-wire) or `tare connect --global`",
        )
    } else {
        DoctorCheck::ok("agents", format!("wired: {}", wired.join(", ")))
    }
}

// ---- `tare up`: the one-command front door ----

/// Platform argv to open a URL in the default browser (pure, testable).
pub fn open_url_argv(url: &str) -> Vec<String> {
    if cfg!(target_os = "macos") {
        vec!["open".into(), url.into()]
    } else if cfg!(target_os = "windows") {
        vec![
            "cmd".into(),
            "/C".into(),
            "start".into(),
            String::new(),
            url.into(),
        ]
    } else {
        vec!["xdg-open".into(), url.into()]
    }
}

/// `tare up`: start the proxy + OTLP receiver + UI and (unless `open` is false) pop the browser at
/// the UI once the listener is up. Blocks on the server until Ctrl-C, exactly like `tare serve` —
/// it just collapses "start everything and show me" into one verb.
pub fn up_command(
    db_path: &str,
    port: u16,
    otlp_port: u16,
    pricing: Option<&str>,
    open: bool,
) -> Result<(), String> {
    let url = format!("http://127.0.0.1:{port}/__tare/");
    println!("tare up — opening {url}  (Ctrl-C to stop)");
    if open {
        let argv = open_url_argv(&url);
        // Open shortly after the listener binds (serve_command blocks below).
        std::thread::Builder::new()
            .name("tare-browser-open".into())
            .spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(900));
                if let Some((bin, args)) = argv.split_first() {
                    if let Err(error) = std::process::Command::new(bin).args(args).spawn() {
                        eprintln!("tare: could not open the dashboard: {error}");
                    }
                }
            })
            .map_err(|e| format!("spawn dashboard opener: {e}"))?;
    }
    serve_command(db_path, port, otlp_port, pricing)
}

// ---- `tare detect`: find installed agents + one-click additive wiring ----

/// `tare detect`: probe known config locations (read-only) for installed agents, report whether
/// each is wired to THIS receiver, and — with `wire` — additively connect the unwired ones using
/// the same reversible transforms as `tare connect`. Honors "never change local AI config":
/// additive and undoable via `tare disconnect` / `tare codex-disconnect`.
pub fn detect_command(otlp_port: u16, wire: bool) -> Result<(), String> {
    let home = home_dir().unwrap_or_default();
    let endpoint = format!("http://127.0.0.1:{otlp_port}");
    println!("tare detect — installed agents\n");

    // Claude Code: ~/.claude present -> installed; we wire the GLOBAL settings.json.
    if !home.is_empty() && std::path::Path::new(&format!("{home}/.claude")).exists() {
        let path = format!("{home}/.claude/settings.json");
        let wired = file_points_at(&path, otlp_port);
        println!("  {} Claude Code  {}", if wired { "✓" } else { "○" }, path);
        if wire && !wired {
            connect::connect_command(&path, &endpoint, false)?;
        }
    } else {
        println!("  – Claude Code  not found (~/.claude absent)");
    }

    // Codex: $CODEX_HOME/config.toml (default ~/.codex/config.toml); installed if its dir exists.
    let codex_path = codex_connect::default_codex_config();
    let codex_dir_present = std::path::Path::new(&codex_path)
        .parent()
        .map(|p| p.exists())
        .unwrap_or(false);
    if codex_dir_present {
        let wired = file_points_at(&codex_path, otlp_port);
        println!(
            "  {} Codex        {}",
            if wired { "✓" } else { "○" },
            codex_path
        );
        if wire && !wired {
            codex_connect::codex_connect_command(
                &codex_path,
                &format!("{endpoint}/v1/logs"),
                false,
            )?;
        }
    } else {
        println!("  – Codex        not found");
    }

    if wire {
        println!("\nWired (additive + reversible). Restart your agent, then run `tare doctor`.");
    } else {
        println!("\n○ = installed but not wired. Run `tare detect --wire` to connect them.");
    }
    Ok(())
}

/// Assemble the doctor checks. `today` is injected (YYYY-MM-DD) so the pricing-freshness check is
/// deterministic in tests. Returns the checks and a process exit code (1 if any check is Bad).
pub fn doctor_run(
    db_path: &str,
    http_port: u16,
    otlp_port: u16,
    today: &str,
) -> (Vec<DoctorCheck>, i32) {
    let mut checks = Vec::new();

    // Store readable.
    match Store::open(db_path).and_then(|s| s.load_runs()) {
        Ok(runs) => checks.push(DoctorCheck::ok(
            "store",
            format!("{db_path} readable ({} runs)", runs.len()),
        )),
        Err(e) => checks.push(DoctorCheck::bad(
            "store",
            format!("cannot open {db_path}: {e}"),
            "check the path and permissions, or pass --db",
        )),
    }

    // Pricing freshness (estimate quality).
    match load_pricing(None) {
        Ok(p) => {
            let age = tare_core::calendar::parse_date(&p.effective_date)
                .zip(tare_core::calendar::parse_date(today))
                .map(|(eff, now)| now - eff);
            match age {
                Some(d) if d > 90 => checks.push(DoctorCheck::warn(
                    "pricing",
                    format!("pricing table {} is {} days old", p.version, d),
                    "estimates may drift; update pricing.json or pass --pricing",
                )),
                _ => checks.push(DoctorCheck::ok(
                    "pricing",
                    format!("table {} ({})", p.version, p.effective_date),
                )),
            }
        }
        Err(e) => checks.push(DoctorCheck::bad(
            "pricing",
            format!("cannot load pricing: {e}"),
            "pass a valid --pricing file",
        )),
    }

    // Capture: receiver up + events flowing.
    let listening = port_listening(otlp_port);
    let events = http_get_local(http_port, "/__tare/otlp_status")
        .and_then(|b| serde_json::from_str::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("events").and_then(|e| e.as_u64()));
    checks.push(capture_verdict(listening, events));

    // Agent wiring.
    checks.push(agent_wiring_check(otlp_port));

    let code = if checks.iter().any(|c| c.status == DoctorStatus::Bad) {
        1
    } else {
        0
    };
    (checks, code)
}

/// Run the checks and print a green/amber/red checklist; return the process exit code.
pub fn doctor_command(db_path: &str, http_port: u16, otlp_port: u16) -> i32 {
    let (checks, code) = doctor_run(db_path, http_port, otlp_port, &today_local());
    println!("tare doctor — setup self-check\n");
    for c in &checks {
        let glyph = match c.status {
            DoctorStatus::Ok => "✓",
            DoctorStatus::Warn => "⚠",
            DoctorStatus::Bad => "✗",
        };
        println!("  {glyph} {:<9} {}", c.name, c.message);
        if let Some(fix) = &c.fix {
            println!("      → {fix}");
        }
    }
    println!(
        "\n{}",
        if code == 0 {
            "All good — capture is live."
        } else {
            "Some checks failed (see → fixes above)."
        }
    );
    code
}

/// Cost-effectiveness over a day window (default: all time) — dollars per outcome,
/// plus cost-per-successful-run from the run-outcome split.
pub fn effectiveness_for(
    db_path: &str,
    from: &str,
    to: &str,
    pricing: &PricingTable,
) -> Result<tare_core::effectiveness::Effectiveness, String> {
    let store = Store::open(db_path)?;
    let mut eff = tare_core::effectiveness::effectiveness(&store.metered_outcomes(from, to)?);
    let split = tare_core::session::run_outcome_split(&cached_runs(db_path)?, pricing);
    eff.cost_per_successful_run_micros = split.cost_per_successful_micros;
    eff.run_success_rate_pct = split.success_rate_pct;
    Ok(eff)
}

/// Cost-EFFECTIVENESS regressions: days whose $/outcome ($/commit, else $/accepted-edit)
/// jumped above its own trailing-window median — the outcome-aware sibling of `anomalies_for`,
/// which alarms on raw spend. Default window: all time; the pure detector supplies the baseline.
pub fn cost_regressions_for(
    db_path: &str,
    from: &str,
    to: &str,
    window: usize,
    threshold_pct: i64,
) -> Result<Vec<tare_core::cost_regression::CostRegression>, String> {
    let store = Store::open(db_path)?;
    let days = store.outcomes_by_day(from, to)?;
    Ok(tare_core::cost_regression::detect(
        &days,
        window,
        threshold_pct,
    ))
}

/// Config↔outcome correlation (the "Explain" parallel-coordinates panel): re-project all
/// stored runs onto config-knob + outcome axes. Reprices per-run as-of capture day so costs match
/// the report byte-for-byte. Pure over stored rows — zero new capture.
pub fn correlate_for(
    db_path: &str,
    pricing: &PricingTable,
) -> Result<tare_core::correlation::CorrelationReport, String> {
    let store = Store::open(db_path)?;
    let runs = cached_runs(db_path)?;
    Ok(tare_core::correlation::correlate_runs_dated(
        &runs,
        pricing,
        &store.run_days()?,
    ))
}

/// Path of the SEPARATE transcript sqlite, a sibling of the counts DB (`tare.db` →
/// `tare.transcripts.db`). Kept apart so the payload-free counts DB never holds bodies.
pub fn transcript_db_path(db_path: &str) -> String {
    let base = db_path.strip_suffix(".db").unwrap_or(db_path);
    format!("{base}.transcripts.db")
}

/// Read one step's redacted transcript: `(req, resp)` if captured, else `None`. Opens the
/// sibling transcript store read-only-ish; returns `None` when the store doesn't exist yet.
pub fn transcript_for(
    db_path: &str,
    run_id: &str,
    step_ordinal: u32,
) -> Result<Option<(String, String, Option<bool>)>, String> {
    let tpath = transcript_db_path(db_path);
    match std::fs::metadata(&tpath) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("inspect transcript store {tpath}: {error}")),
    }
    tare_store::TranscriptStore::open(&tpath)?.get_by_step(run_id, step_ordinal)
}

/// Purge ALL captured transcripts (one-click opt-out). Returns rows removed (0 if none).
pub fn transcript_purge(db_path: &str) -> Result<usize, String> {
    let tpath = transcript_db_path(db_path);
    match std::fs::metadata(&tpath) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("inspect transcript store {tpath}: {error}")),
    }
    tare_store::TranscriptStore::open(&tpath)?.purge_all()
}

/// Prompt/config lineage: project one named lineage's versions onto cost-per-run, so a
/// user can see whether a rewrite held quality at lower cost. Pure over stored rows; zero new capture.
pub fn lineage_for(
    db_path: &str,
    pricing: &PricingTable,
    name: &str,
) -> Result<tare_core::lineage::LineageReport, String> {
    let cfg = load_config_strict()?;
    let lineage = cfg
        .lineage
        .iter()
        .find(|l| l.name == name)
        .ok_or_else(|| format!("no lineage named {name:?} in tare.toml ([[lineage]])"))?;
    let runs = cached_runs(db_path)?;
    Ok(tare_core::lineage::lineage_report(&runs, pricing, lineage))
}

/// All configured lineages projected onto cost-per-run — the list view / web dropdown.
/// Loads runs once and reuses them across every lineage.
pub fn lineages_all(
    db_path: &str,
    pricing: &PricingTable,
) -> Result<Vec<tare_core::lineage::LineageReport>, String> {
    let cfg = load_config_strict()?;
    let runs = cached_runs(db_path)?;
    Ok(cfg
        .lineage
        .iter()
        .map(|l| tare_core::lineage::lineage_report(&runs, pricing, l))
        .collect())
}

/// Render a lineage report as a text table: one row per version, cost-per-run the focus.
pub fn render_lineage_text(rep: &tare_core::lineage::LineageReport) -> String {
    let mut out = format!(
        "Lineage {:?} — cost per run by version — ESTIMATE (deterministic, on-device)\n\n",
        rep.name
    );
    out.push_str("  version        runs    $/run       total\n");
    for r in &rep.rows {
        out.push_str(&format!(
            "  {:<12}  {:>5}  {:>9}  {:>10}\n",
            r.label,
            r.runs,
            MicroUsd(r.micros_per_run).to_dollar_string(),
            MicroUsd(r.cost_micros).to_dollar_string(),
        ));
    }
    if rep.rows.is_empty() {
        out.push_str("  (no versions declared)\n");
    }
    out
}

/// Cost-per-unit-of-work: bucket captured runs into the configured `[[unit]]` rules and
/// price each — dev-meaningful denominators (cost per task / PR / feature). Pure; zero new capture.
pub fn units_for(
    db_path: &str,
    pricing: &PricingTable,
) -> Result<tare_core::workunit::UnitReport, String> {
    let cfg = load_config_strict()?;
    let runs = cached_runs(db_path)?;
    Ok(tare_core::workunit::unit_report(&runs, pricing, &cfg.unit))
}

/// Render a unit-of-work report as a text table: one row per unit + the unbucketed rest.
pub fn render_units_text(rep: &tare_core::workunit::UnitReport) -> String {
    let mut out = String::from("Cost per unit of work — ESTIMATE (deterministic, on-device)\n\n");
    out.push_str("  unit              runs    $/run       total\n");
    for r in &rep.rows {
        out.push_str(&format!(
            "  {:<16}  {:>5}  {:>9}  {:>10}\n",
            r.name,
            r.runs,
            MicroUsd(r.micros_per_run).to_dollar_string(),
            MicroUsd(r.cost_micros).to_dollar_string(),
        ));
    }
    out.push_str(&format!(
        "\n  total: {}  ({} run{} unbucketed)\n",
        MicroUsd(rep.total_micros).to_dollar_string(),
        rep.unbucketed_runs,
        if rep.unbucketed_runs == 1 { "" } else { "s" },
    ));
    out
}

// ---- Calibrated Bench cohort facade --------------------------------------------
//
// Thin HTTP-side glue over the SHARED `Store::cohort_*_response` envelope builders (in tare-store),
// which BOTH these HTTP routes and the tare-tauri `cohort_*` commands call — so the two transports
// return byte-identical data for the same inputs (the equivalence acceptance) with no parallel
// re-implementation to drift. The builders classify errors to an HTTP status (validation → 400,
// list/limit overflow → 413, store/analysis failure → 500); the CLI maps that onto the response.

/// RFC3339 UTC instant for `refreshed_at`. Built from unix seconds (the CLI is the clock-owning edge;
/// tare-core stays clock-free) via the shared civil-date rule so it can't drift from query bucketing.
fn now_rfc3339_utc() -> String {
    tare_core::calendar::rfc3339_utc(now_unix_secs())
}

// The `cohort_*_json` fns are thin transport glue: open the store and delegate to the SHARED
// `Store::cohort_*_response` envelope builders (parse → engine → honest provenance → JSON). The store
// is clock-free, so the CLI stamps `refreshed_at` here. `Store::open` failure is a 500.

/// POST `/__tare/cohort/resolve`: resolve a [`CohortSpec`] to its entity set + totals.
pub fn cohort_resolve_json(
    db: &str,
    body: &[u8],
    pricing: &PricingTable,
) -> Result<String, (u16, String)> {
    let store = Store::open(db).map_err(|e| (500u16, e))?;
    store.cohort_resolve_response(body, pricing, &now_rfc3339_utc())
}

/// POST `/__tare/cohort/facets`: selection-vs-baseline facet profile for one dimension.
pub fn cohort_facets_json(
    db: &str,
    body: &[u8],
    pricing: &PricingTable,
) -> Result<String, (u16, String)> {
    let store = Store::open(db).map_err(|e| (500u16, e))?;
    store.cohort_facets_response(body, pricing, &now_rfc3339_utc())
}

/// POST `/__tare/cohort/compare`: decompose selection-vs-baseline spend into volume/size/efficiency.
pub fn cohort_compare_json(
    db: &str,
    body: &[u8],
    pricing: &PricingTable,
) -> Result<String, (u16, String)> {
    let store = Store::open(db).map_err(|e| (500u16, e))?;
    store.cohort_compare_response(body, pricing, &now_rfc3339_utc())
}

/// POST `/__tare/cohort/search`: allow-listed cross-run search within a resolved cohort.
pub fn cohort_search_json(
    db: &str,
    body: &[u8],
    pricing: &PricingTable,
) -> Result<String, (u16, String)> {
    let store = Store::open(db).map_err(|e| (500u16, e))?;
    store.cohort_search_response(body, pricing, &now_rfc3339_utc())
}

/// POST `/__tare/cohort/timeline`: dense daily metrics over the exact cohort plus persisted local
/// config-change annotations. Configured work units are supplied only as denominator rules.
pub fn cohort_timeline_json(
    db: &str,
    body: &[u8],
    pricing: &PricingTable,
) -> Result<String, (u16, String)> {
    let store = Store::open(db).map_err(|e| (500u16, e))?;
    let cfg = load_config_strict().map_err(|e| (500u16, e))?;
    store.cohort_timeline_response(body, pricing, &now_rfc3339_utc(), &cfg.unit)
}

/// The exact pricing table `tare serve` loaded at startup. Analysis POST endpoints take no pricing
/// argument, so retaining the assembled table keeps them byte-for-byte aligned with GET endpoints
/// even if the config or source file changes while the server is running.
static SERVE_PRICING: std::sync::OnceLock<PricingTable> = std::sync::OnceLock::new();

/// The pricing table for the analysis POST endpoints.
///
/// Before this, every POST route called `load_pricing(None)` — the bundled table — while the GET
/// routes were priced from `--pricing` / `[proxy].pricing`. So `tare serve --pricing custom.json`
/// answered `/__tare/report` from custom.json and `/__tare/cohort/resolve` from the bundled rates:
/// the same cohort reported two different dollar figures depending on which endpoint you asked,
/// which breaks invariant 1 ("never a wrong number") inside a single process.
///
/// Never returns an empty or fallback table: callers surface an error rather than understating cost.
pub fn analysis_pricing() -> Result<PricingTable, String> {
    if let Some(table) = SERVE_PRICING.get() {
        return Ok(table.clone());
    }
    let cfg = load_config_strict()?;
    load_pricing_with_config(cfg.proxy.pricing.as_deref(), &cfg)
}

// Transport-agnostic cohort entry points: load the active pricing table
// and dispatch to the matching `cohort_*_json` fn. BOTH the HTTP `write_api` routes AND the Tauri
// commands call these, so the two transports return byte-identical `data` for the same request —
// the equivalence acceptance is satisfied by construction, not by parallel re-implementation. `db`
// is the already-resolved database path; the `(status, message)` error is mapped per transport.

/// `/__tare/cohort/resolve` (HTTP) == `cohort_resolve` (Tauri).
pub fn cohort_resolve_api(db: &str, body: &[u8]) -> Result<String, (u16, String)> {
    let pricing = analysis_pricing().map_err(|e| (500u16, e))?;
    cohort_resolve_json(db, body, &pricing)
}

/// `/__tare/cohort/facets` (HTTP) == `cohort_facets` (Tauri).
pub fn cohort_facets_api(db: &str, body: &[u8]) -> Result<String, (u16, String)> {
    let pricing = analysis_pricing().map_err(|e| (500u16, e))?;
    cohort_facets_json(db, body, &pricing)
}

/// `/__tare/cohort/compare` (HTTP) == `cohort_compare` (Tauri).
pub fn cohort_compare_api(db: &str, body: &[u8]) -> Result<String, (u16, String)> {
    let pricing = analysis_pricing().map_err(|e| (500u16, e))?;
    cohort_compare_json(db, body, &pricing)
}

/// `/__tare/cohort/search` (HTTP) == `cohort_search` (Tauri).
pub fn cohort_search_api(db: &str, body: &[u8]) -> Result<String, (u16, String)> {
    let pricing = analysis_pricing().map_err(|e| (500u16, e))?;
    cohort_search_json(db, body, &pricing)
}

/// `/__tare/cohort/timeline` (HTTP) == `cohort_timeline` (Tauri).
pub fn cohort_timeline_api(db: &str, body: &[u8]) -> Result<String, (u16, String)> {
    let pricing = analysis_pricing().map_err(|e| (500u16, e))?;
    cohort_timeline_json(db, body, &pricing)
}

/// Estimate-Confidence: fuse pricing freshness + unpriced-token share + coverage into
/// one label. `today` is injected so the freshness factor is deterministic. Coverage is reported as
/// `unknown`: out-of-band capture has no defensible denominator for
/// expected total spend, so we never fabricate a completeness percentage. A future blind-spot meter
/// may supply `partial`/`full` with a real denominator.
pub fn confidence_for(
    db_path: &str,
    pricing: &PricingTable,
    today: &str,
) -> Result<tare_core::confidence::EstimateConfidence, String> {
    Ok(confidence_over(&cached_runs(db_path)?, pricing, today))
}

/// Estimate-confidence over a specific set of runs (clock-free given `today`). Shared by the
/// all-runs `confidence_for` and the receipt's per-scope confidence stamp.
pub fn confidence_over(
    runs: &[tare_core::model::RunRecord],
    pricing: &PricingTable,
    today: &str,
) -> tare_core::confidence::EstimateConfidence {
    let report = tare_core::attribute::build_report(runs, pricing);
    let total_tokens = tare_core::lenses::lenses(runs, pricing).total_tokens;
    let unpriced_tokens = report
        .unpriced
        .iter()
        .fold(0u64, |total, model| total.saturating_add(model.token_total));
    let unpriced_share = unpriced_tokens
        .saturating_mul(100)
        .checked_div(total_tokens)
        .unwrap_or(0) as i64;
    let age = tare_core::calendar::parse_date(&pricing.effective_date)
        .zip(tare_core::calendar::parse_date(today))
        .map(|(eff, now)| now.saturating_sub(eff).max(0))
        // Invalid dates must not masquerade as fresh pricing. Valid loaded tables and the CLI clock
        // never reach this branch, but public callers still get a conservative result.
        .unwrap_or(i64::MAX);
    // Coverage is honestly `unknown` — out-of-band capture has no denominator.
    tare_core::confidence::confidence(
        age,
        unpriced_share,
        tare_core::confidence::CoverageStatus::Unknown,
        None,
    )
}

/// The unified Savings Ledger over the whole store.
pub fn savings_for(
    db_path: &str,
    pricing: &PricingTable,
) -> Result<tare_core::savings::SavingsLedger, String> {
    let runs = cached_runs(db_path)?;
    Ok(tare_core::savings::savings(&runs, pricing))
}

/// The additive v2 savings ledger: v1 fields + OpportunityV2 rows (stable key,
/// capped evidence, resolvable cohort snapshot, assumptions, quality risk) plus lifecycle totals.
pub fn savings_v2_for(
    db_path: &str,
    pricing: &PricingTable,
) -> Result<tare_core::savings::SavingsLedgerV2, String> {
    let runs = cached_runs(db_path)?;
    let lifecycle = Store::open(db_path)?.savings_lifecycle_totals(pricing)?;
    Ok(tare_core::savings::savings_v2_with_lifecycle(
        &runs, pricing, lifecycle,
    ))
}

/// Render the unified Savings Ledger as a ranked plain-text lens: the capped-potential
/// recoverable-$ (spend-bounded, NOT a deduped floor), the Savings Index, then each opportunity.
pub fn render_savings_text(led: &tare_core::savings::SavingsLedger) -> String {
    let mut out = format!(
        "Tare savings ledger — ESTIMATE (pricing {})\n\n",
        led.pricing_version
    );
    out.push_str(&format!(
        "Capped potential (categories may overlap): {}  of {} spent  ·  Savings Index {}/100\n\n",
        MicroUsd(led.total_recoverable_micros).to_dollar_string(),
        MicroUsd(led.total_spend_micros).to_dollar_string(),
        led.savings_index,
    ));
    if led.opportunities.is_empty() {
        out.push_str("No recoverable spend found — nothing to act on.\n");
        return out;
    }
    for o in &led.opportunities {
        out.push_str(&format!(
            "  {:>10}  [{:^11}] {:<14} {} ({})\n",
            MicroUsd(o.recoverable_micros).to_dollar_string(),
            o.confidence,
            o.kind,
            o.label,
            o.effort,
        ));
    }
    out.push_str(
        "\nEach figure is a per-category upper bound; capped potential is bounded by spend.\n",
    );
    out
}

/// Render the Savings Ledger with an `✓ accepted` marker on rows the user already acted on. The
/// plain `savings` view previously gave no way to tell accepted opportunities apart
/// from open ones — the ledger and its acceptance state were disjoint. `accepted` holds the stored
/// `kind:label` keys (from `savings_acceptances`).
pub fn render_savings_text_marked(
    led: &tare_core::savings::SavingsLedger,
    accepted: &std::collections::HashSet<String>,
) -> String {
    let mut out = format!(
        "Tare savings ledger — ESTIMATE (pricing {})\n\n",
        led.pricing_version
    );
    out.push_str(&format!(
        "Capped potential (categories may overlap): {}  of {} spent  ·  Savings Index {}/100\n\n",
        MicroUsd(led.total_recoverable_micros).to_dollar_string(),
        MicroUsd(led.total_spend_micros).to_dollar_string(),
        led.savings_index,
    ));
    if led.opportunities.is_empty() {
        out.push_str("No recoverable spend found — nothing to act on.\n");
        return out;
    }
    for o in &led.opportunities {
        let mark = if accepted.contains(&format!("{}:{}", o.kind, o.label)) {
            " ✓ accepted"
        } else {
            ""
        };
        out.push_str(&format!(
            "  {:>10}  [{:^11}] {:<14} {} ({}){}\n",
            MicroUsd(o.recoverable_micros).to_dollar_string(),
            o.confidence,
            o.kind,
            o.label,
            o.effort,
            mark,
        ));
    }
    out.push_str(
        "\nEach figure is a per-category upper bound; capped potential is bounded by spend.\n\
         Rows marked ✓ were accepted — run `tare realized` to prove the savings.\n",
    );
    out
}

/// The set of accepted opportunity keys (`kind:label`) for marking the savings view.
pub fn accepted_savings_keys(db_path: &str) -> Result<std::collections::HashSet<String>, String> {
    Ok(Store::open(db_path)?
        .savings_acceptances()?
        .into_iter()
        .map(|a| a.opportunity_key)
        .collect())
}

pub fn action_plan_for(
    db_path: &str,
    pricing: &PricingTable,
) -> Result<tare_core::action_plan::ActionPlan, String> {
    let runs = cached_runs(db_path)?;
    Ok(tare_core::action_plan::action_plan(&runs, pricing))
}

/// Render the unified Action Plan: one ranked worklist merging recoverable
/// opportunities and at-risk advisories, every row dollar-quantified. Capped potential and at-risk
/// exposure are shown as separate totals; at-risk exposure is never added to potential.
pub fn render_action_plan_text(plan: &tare_core::action_plan::ActionPlan) -> String {
    let mut out = format!(
        "Tare action plan — ESTIMATE (pricing {})\n\n",
        plan.pricing_version
    );
    out.push_str(&format!(
        "Capped potential (categories may overlap): {}  ·  At-risk (upper bound, not a saving): {}  ·  of {} spent  ·  Savings Index {}/100\n\n",
        MicroUsd(plan.recoverable_floor_micros).to_dollar_string(),
        MicroUsd(plan.at_risk_total_micros).to_dollar_string(),
        MicroUsd(plan.total_spend_micros).to_dollar_string(),
        plan.savings_index,
    ));
    if plan.items.is_empty() {
        out.push_str(
            "Nothing to act on — no dollar-quantified opportunities or exposures found.\n",
        );
        return out;
    }
    for i in &plan.items {
        out.push_str(&format!(
            "  {:>10}  [{:^11}] {:<12} {:<14} {} ({})\n",
            MicroUsd(i.dollars_micros).to_dollar_string(),
            i.confidence,
            i.basis,
            i.kind,
            i.label,
            i.effort,
        ));
    }
    out.push_str(
        "\nrecoverable = dollars a fix would save (floor bounded by spend); at-risk = dollars exposed\n\
         to a pattern (an upper bound, never summed into recoverable). No row without a dollar figure.\n",
    );
    out
}

/// Mark a savings opportunity accepted: look it up in the current ledger by
/// `kind:label`, snapshot its recoverable estimate, and record `today` as the accept date.
pub fn accept_savings_for(
    db_path: &str,
    key: &str,
    today: &str,
    pricing: &PricingTable,
) -> Result<i64, String> {
    let led = savings_for(db_path, pricing)?;
    let opp = led
        .opportunities
        .iter()
        .find(|o| format!("{}:{}", o.kind, o.label) == key)
        .ok_or_else(|| format!("no open opportunity with key {key:?} — see `tare savings`"))?;
    let recoverable = opp.recoverable_micros;
    Store::open(db_path)?.accept_savings(key, today, recoverable)?;
    Ok(recoverable)
}

/// Un-accept a previously accepted opportunity.
pub fn unaccept_savings_for(db_path: &str, key: &str) -> Result<(), String> {
    Store::open(db_path)?.unaccept_savings(key)
}

/// Build the savings-realization ledger over accepted opportunities.
pub fn realized_for(
    db_path: &str,
    today: &str,
    window_days: i64,
    pricing: &PricingTable,
) -> Result<tare_core::realization::RealizationLedger, String> {
    let today_days = tare_core::calendar::parse_date(today)
        .ok_or_else(|| format!("invalid realization date {today:?}; expected YYYY-MM-DD"))?;
    if window_days < 1 {
        return Err("realization window must be at least 1 day".into());
    }
    let store = Store::open(db_path)?;
    let accepted: Vec<tare_core::realization::AcceptedOpportunity> = store
        .savings_acceptances()?
        .into_iter()
        .map(|a| tare_core::realization::AcceptedOpportunity {
            opportunity_key: a.opportunity_key,
            accepted_date: a.accepted_date,
            recoverable_micros: a.recoverable_micros,
        })
        .collect();
    // Span both windows: earliest accept − W .. today.
    let from = accepted
        .iter()
        .filter_map(|a| tare_core::calendar::parse_date(&a.accepted_date))
        .min()
        .map(|day| tare_core::calendar::format_date(day.saturating_sub(window_days)))
        .unwrap_or_else(|| tare_core::calendar::format_date(today_days));
    let dated = store.load_dated_runs_in_range(&from, today)?;
    Ok(tare_core::realization::realization(
        &accepted,
        &dated,
        pricing,
        today,
        window_days,
    ))
}

/// Human-facing render of the realization ledger.
pub fn render_realization_text(led: &tare_core::realization::RealizationLedger) -> String {
    let mut out = format!(
        "Tare savings realization — ESTIMATE, {}-day windows (pricing {})\n\n",
        led.window_days, led.pricing_version
    );
    if led.rows.is_empty() {
        out.push_str("No accepted opportunities yet — accept one with `tare savings --accept \"<kind>:<label>\"`.\n");
        return out;
    }
    out.push_str(&format!(
        "Total realized (completed windows): {}\n\n",
        MicroUsd(led.total_realized_micros).to_dollar_string()
    ));
    for r in &led.rows {
        out.push_str(&format!(
            "  {:<10} {:<22} accepted {}  before {} → after {}  = {}\n",
            r.state,
            r.opportunity_key,
            r.accepted_date,
            MicroUsd(r.before_micros).to_dollar_string(),
            MicroUsd(r.after_micros).to_dollar_string(),
            MicroUsd(r.realized_micros).to_dollar_string(),
        ));
    }
    out.push_str("\nRealized = max(0, spend before − spend after) over equal windows; `pending` until the after-window elapses.\n");
    out
}

/// Detect the git attribution (SHA/branch/author/dirty) of the working tree at `cwd`.
/// Runs the four short git commands at the edge and hands their stdout to the pure core parser;
/// returns `None` if `cwd` isn't a git repo (or git is absent). Counts-only — no diffs/messages.
pub fn detect_git_attribution(cwd: &std::path::Path) -> Option<tare_core::git::GitAttribution> {
    let run = |args: &[&str]| -> String {
        std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    tare_core::git::parse_git_attribution(
        &run(&["rev-parse", "HEAD"]),
        &run(&["symbolic-ref", "--quiet", "--short", "HEAD"]),
        &run(&["log", "-1", "--format=%an"]),
        &run(&["status", "--porcelain"]),
    )
}

/// Convert a LiteLLM `model_prices_and_context_window.json` file into a DATED Tare pricing edition
/// written to `out_path`. Pure conversion (`PricingTable::from_litellm_json`) + a
/// file write; the clock-owning caller supplies `effective_date`. Returns the model count.
pub fn pricing_refresh_from_litellm_file(
    from_path: &str,
    effective_date: &str,
    out_path: &str,
) -> Result<usize, String> {
    let src = std::fs::read_to_string(from_path).map_err(|e| format!("read {from_path}: {e}"))?;
    let table = tare_core::PricingTable::from_litellm_json(&src, effective_date)?;
    let n = table.models.len();
    let json = serde_json::to_string_pretty(&table).map_err(|e| e.to_string())?;
    std::fs::write(out_path, json).map_err(|e| format!("write {out_path}: {e}"))?;
    Ok(n)
}

/// Convert a downloaded models.dev `api.json` into a dated Tare edition — the mirror of
/// the LiteLLM path, feeding the already-tested `from_modelsdev_json` converter. (Auto-fetching the URL
/// is the TLS-gated follow-on; this makes the second source usable today from a downloaded file.)
pub fn pricing_refresh_from_modelsdev_file(
    from_path: &str,
    effective_date: &str,
    out_path: &str,
) -> Result<usize, String> {
    let src = std::fs::read_to_string(from_path).map_err(|e| format!("read {from_path}: {e}"))?;
    let table = tare_core::PricingTable::from_modelsdev_json(&src, effective_date)?;
    let n = table.models.len();
    let json = serde_json::to_string_pretty(&table).map_err(|e| e.to_string())?;
    std::fs::write(out_path, json).map_err(|e| format!("write {out_path}: {e}"))?;
    Ok(n)
}

/// The canonical live price-map URL for a source. LiteLLM's raw map + models.dev's
/// api.json — the two the tested converters accept.
pub fn pricing_source_url(source: &str) -> Result<&'static str, String> {
    match source {
        "litellm" => Ok("https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json"),
        "modelsdev" | "models.dev" => Ok("https://models.dev/api.json"),
        other => Err(format!(
            "pricing refresh --fetch: unknown source {other:?} (expected litellm|modelsdev)"
        )),
    }
}

/// Bounded HTTPS GET: short timeout + hard size cap, so a hung/huge endpoint can't
/// stall or balloon the always-on agent. TLS-gated behind the `pricing-fetch` feature (out of the
/// offline gate). Returns the body as a string.
#[cfg(feature = "pricing-fetch")]
pub fn pricing_fetch_url(url: &str, timeout_secs: u64, max_bytes: usize) -> Result<String, String> {
    use std::io::Read;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .user_agent(concat!("tare-pricing-refresh/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let resp = client
        .get(url)
        .send()
        .map_err(|e| format!("GET {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET {url}: HTTP {}", resp.status()));
    }
    if let Some(length) = resp.content_length() {
        let cap = u64::try_from(max_bytes)
            .map_err(|_| format!("GET {url}: response cap is not representable"))?;
        if length > cap {
            return Err(format!("GET {url}: response exceeds {max_bytes}-byte cap"));
        }
    }
    // Read at most cap+1 bytes so we can detect (and reject) an over-cap body without buffering it all.
    let read_cap = u64::try_from(max_bytes)
        .ok()
        .and_then(|cap| cap.checked_add(1))
        .ok_or_else(|| format!("GET {url}: response cap is too large"))?;
    let mut buf = Vec::new();
    resp.take(read_cap)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read {url}: {e}"))?;
    if buf.len() > max_bytes {
        return Err(format!("GET {url}: response exceeds {max_bytes}-byte cap"));
    }
    String::from_utf8(buf).map_err(|e| format!("GET {url}: non-UTF-8 body: {e}"))
}

/// Fetch a live price map and write it as a dated Tare edition: the auto-fetch mirror
/// of `pricing_refresh_from_file`, feeding the same tested converters. 15 s timeout, 8 MiB cap.
#[cfg(feature = "pricing-fetch")]
pub fn pricing_refresh_from_url(
    source: &str,
    effective_date: &str,
    out_path: &str,
) -> Result<usize, String> {
    let url = pricing_source_url(source)?;
    let body = pricing_fetch_url(url, 15, 8 * 1024 * 1024)?;
    let table = match source {
        "litellm" => tare_core::PricingTable::from_litellm_json(&body, effective_date)?,
        _ => tare_core::PricingTable::from_modelsdev_json(&body, effective_date)?,
    };
    let n = table.models.len();
    let json = serde_json::to_string_pretty(&table).map_err(|e| e.to_string())?;
    std::fs::write(out_path, json).map_err(|e| format!("write {out_path}: {e}"))?;
    Ok(n)
}

/// Fetch BOTH live price maps and write a merged dated edition: LiteLLM is the primary
/// (first-party rates + Anthropic long-context tiers win), models.dev backfills models LiteLLM lacks.
/// Bounded fetches (15 s / 8 MiB each), exact merge, no tokenizer swap.
#[cfg(feature = "pricing-fetch")]
pub fn pricing_refresh_merged(effective_date: &str, out_path: &str) -> Result<usize, String> {
    let litellm = pricing_fetch_url(pricing_source_url("litellm")?, 15, 8 * 1024 * 1024)?;
    let modelsdev = pricing_fetch_url(pricing_source_url("modelsdev")?, 15, 8 * 1024 * 1024)?;
    let primary = tare_core::PricingTable::from_litellm_json(&litellm, effective_date)?;
    let fallback = tare_core::PricingTable::from_modelsdev_json(&modelsdev, effective_date)?;
    let merged = primary.merged_with(&fallback);
    let n = merged.models.len();
    let json = serde_json::to_string_pretty(&merged).map_err(|e| e.to_string())?;
    std::fs::write(out_path, json).map_err(|e| format!("write {out_path}: {e}"))?;
    Ok(n)
}

/// Dispatch a local-file pricing refresh by source: `litellm` (default) or `modelsdev`.
pub fn pricing_refresh_from_file(
    from_path: &str,
    effective_date: &str,
    out_path: &str,
    source: &str,
) -> Result<usize, String> {
    match source {
        "litellm" => pricing_refresh_from_litellm_file(from_path, effective_date, out_path),
        "modelsdev" | "models.dev" => {
            pricing_refresh_from_modelsdev_file(from_path, effective_date, out_path)
        }
        other => Err(format!(
            "pricing refresh: unknown --source {other:?} (expected litellm|modelsdev)"
        )),
    }
}

/// Backfill Claude Code JSONL transcripts into the store. Walks each `projects/` dir and, for every
/// `.jsonl`, classifies its
/// path to the owning session (subagent/workflow records roll up to the PARENT session), SKIPS any
/// session already captured live (has non-`jsonl` steps → OTLP > JSONL, no double-count), dedups the
/// rest against the backfill ledger (idempotent across re-runs), and persists the fresh turns as
/// `source = jsonl` steps dated from the transcript. Returns how many steps were inserted. Read-only
/// w.r.t. the transcripts; counts-only.
pub fn backfill_transcripts(db_path: &str, dirs: &[std::path::PathBuf]) -> Result<usize, String> {
    // One shared ingest path with the live daemon: a single incremental sweep via
    // TranscriptCapture. The persisted scan cursor makes a re-run skip unchanged files; the dedup
    // ledger stays the correctness backstop. `jsonl-backfill` marks these as the manual-import lane.
    let mut cap = crate::transcript_capture::TranscriptCapture::open(
        db_path,
        dirs.to_vec(),
        "jsonl-backfill",
    )?;
    cap.sweep()
}

/// Map a payload-free [`tare_core::transcript::TranscriptRecord`] to a degraded `StepRecord` tagged
/// to `session` (its run id) with ordinal `ord`. Claude Code = Anthropic pricing key; shape carries
/// only the session label (counts-only backfill, no structural attribution).
pub(crate) fn transcript_record_to_step(
    rec: &tare_core::transcript::TranscriptRecord,
    session: &str,
    ord: u32,
) -> tare_core::model::StepRecord {
    use tare_core::model::{Provider, RequestShape, StepRecord};
    StepRecord {
        run_id: session.to_string(),
        step_ordinal: ord,
        provider: Provider::Anthropic,
        model: rec.model.clone(),
        usage: rec.usage,
        shape: RequestShape {
            model: rec.model.clone(),
            provider: Provider::Anthropic,
            stream: false,
            ttl: tare_core::model::CacheTtl::FiveMin,
            has_cache_control: rec.usage.cache_read > 0
                || rec.usage.cache_write_5m > 0
                || rec.usage.cache_write_1h > 0,
            cached_component: None,
            system_hash: None,
            weights: vec![],
            request_hash: None,
            step_label: None,
            component_label: None,
            parent_label: None,
            attempt: None,
            session: Some(session.to_string()),
            workload_key: None,
            effort: None,
            mcp_server: None,
            vendor: None,
            commit: None,
            author: None,
        },
        stop_reason: None,
        duration_ms: 0,
        start_unix_nano: None,
        trace_id: None,
        span_id: None,
        parent_span_id: None,
    }
}

/// Recursively collect `.jsonl` files under `dir` (best-effort).
pub(crate) fn collect_jsonl(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        // Use the ENTRY's own file type, which (unlike `Path::is_dir`) does NOT follow symlinks.
        // A `--dir` can be pointed anywhere, and real project trees are full of symlinks (e.g.
        // node_modules); a symlinked directory pointing back at an ancestor would send this
        // recursive walk into an infinite loop (stack overflow), and any symlinked directory could
        // escape the intended tree into an unbounded read sweep — the very re-scan cost backfill
        // must avoid. So we recurse ONLY into real directories and read only regular jsonl files.
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.is_dir() {
            collect_jsonl(&p, out);
        } else if ft.is_file() && p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
            out.push(p);
        }
    }
}

/// Weekly digest: recompose trend + anomalies + savings over the two weeks ending
/// `today` into one local report. Loads only the runs in-window; zero new capture.
pub fn digest_for(
    db_path: &str,
    today: &str,
    pricing: &tare_core::PricingTable,
) -> Result<tare_core::digest::Digest, String> {
    let store = Store::open(db_path)?;
    // The digest needs the prior week too (for the WoW delta): [today-13, today].
    let today_days = tare_core::calendar::parse_date(today)
        .ok_or_else(|| format!("invalid digest date {today:?}; expected YYYY-MM-DD"))?;
    let from = tare_core::calendar::format_date(today_days.saturating_sub(13));
    let dated = store.load_dated_runs_in_range(&from, today)?;
    Ok(tare_core::digest::digest(&dated, pricing, today))
}

/// Weekly pricing-refresh cadence: at most once per ISO week, auto-fetch the live
/// price map and write a fresh dated edition to `<out_dir>/pricing-<monday>.json`. Fire-once via the
/// same `claim_alert` seam as the digest. SILENT FALLBACK: any fetch/parse failure leaves the current
/// editions untouched (returns `Ok(None)`), so a bad network day never disrupts the always-on agent.
/// TLS-gated behind `pricing-fetch` (out of the offline gate); a no-op build without it.
#[cfg(feature = "pricing-fetch")]
pub fn maybe_refresh_pricing_weekly(
    db_path: &str,
    today: &str,
    source: &str,
    out_dir: &str,
) -> Result<Option<String>, String> {
    let Some(week) = tare_core::calendar::week_start(today) else {
        return Ok(None);
    };
    // One bounded attempt per week (claim upfront so a failure doesn't hammer the network each tick).
    if !Store::open(db_path)?.claim_alert(&format!("weekly-pricing-refresh:{week}"), &week)? {
        return Ok(None);
    }
    let out = std::path::Path::new(out_dir)
        .join(format!("pricing-{week}.json"))
        .to_string_lossy()
        .to_string();
    // Silent fallback: on any error keep the current table; never surface a fatal from the daemon.
    match pricing_refresh_from_url(source, &week, &out) {
        Ok(_) => Ok(Some(out)),
        Err(_) => Ok(None),
    }
}

/// Weekly-digest cadence: fire the digest at most once per ISO week. Claims
/// `weekly-digest:<monday>` in the persistent fired-set (the same fire-once seam the anomaly daemon
/// uses), and on the first claim of a week renders the digest and writes it to
/// `<out_dir>/tare-digest-<monday>.txt`, returning that path. Returns `Ok(None)` when this week's
/// digest already fired (or `today` is unparseable). The daemon raises a local notice on the path.
pub fn maybe_write_weekly_digest(
    db_path: &str,
    pricing: &PricingTable,
    today: &str,
    out_dir: &str,
) -> Result<Option<String>, String> {
    let Some(week) = tare_core::calendar::week_start(today) else {
        return Ok(None);
    };
    // Atomic fire-once claim for this ISO week; a second tick in the same week is a no-op.
    if !Store::open(db_path)?.claim_alert(&format!("weekly-digest:{week}"), &week)? {
        return Ok(None);
    }
    let digest = digest_for(db_path, today, pricing)?;
    let text = tare_core::digest::render_digest_text(&digest);
    let path = std::path::Path::new(out_dir).join(format!("tare-digest-{week}.txt"));
    std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(Some(path.to_string_lossy().to_string()))
}

/// Build the OS-native desktop-notification command (program + args) for `title`/`body`.
/// DEPENDENCY-FREE — shells out to the platform's built-in notifier so it works in the offline build
/// (no notification crate to cache): macOS `osascript`, Linux `notify-send`. Returns `None` where we
/// have no built-in target; Windows notifications require a platform-specific module.
/// Values are passed as `run` arguments rather than interpolated into AppleScript source.
pub fn notify_command(os: &str, title: &str, body: &str) -> Option<(String, Vec<String>)> {
    match os {
        "macos" => Some((
            "osascript".to_string(),
            vec![
                "-e".to_string(),
                "on run argv".to_string(),
                "-e".to_string(),
                "display notification (item 2 of argv) with title (item 1 of argv)".to_string(),
                "-e".to_string(),
                "end run".to_string(),
                "--".to_string(),
                title.to_string(),
                body.to_string(),
            ],
        )),
        "linux" => Some((
            "notify-send".to_string(),
            vec![title.to_string(), body.to_string()],
        )),
        _ => None,
    }
}

/// Fire a best-effort local desktop notification for the current OS. NEVER blocks or
/// fails the caller: a missing notifier / spawn error is swallowed — the digest file is the source of
/// truth, the popup is a nicety. Fully local; nothing leaves the box.
pub fn fire_desktop_notification(title: &str, body: &str) {
    if let Some((prog, args)) = notify_command(std::env::consts::OS, title, body) {
        let _ = std::process::Command::new(prog)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

/// Pre-flight cost band: reprice a stored run's shape, optionally onto a swap target.
pub fn estimate_for(
    db_path: &str,
    run_id: &str,
    target: Option<&str>,
    pricing: &tare_core::PricingTable,
) -> Result<tare_core::estimate::Estimate, String> {
    let run = load_run(db_path, run_id)?;
    tare_core::estimate::estimate_like(&run, pricing, target)
}

/// Human-facing render of a pre-flight estimate band.
pub fn render_estimate_text(e: &tare_core::estimate::Estimate) -> String {
    let mut out = format!(
        "Pre-flight estimate — like run `{}` on {} (ESTIMATE; pricing {})\n",
        e.run_id, e.model, e.pricing_version
    );
    if e.low_micros == e.high_micros {
        out.push_str(&format!(
            "  ~{}\n",
            MicroUsd(e.low_micros).to_dollar_string()
        ));
    } else {
        out.push_str(&format!(
            "  {} – {}  (point {})\n",
            MicroUsd(e.low_micros).to_dollar_string(),
            MicroUsd(e.high_micros).to_dollar_string(),
            MicroUsd(e.point_micros).to_dollar_string(),
        ));
    }
    out.push_str("  Floor = captured counts as-is; high adds ~804-tok/step tool-use overhead");
    if e.cross_tokenizer {
        out.push_str(" + ~30% cross-tokenizer inflation (a different model tokenizes differently)");
    }
    out.push_str(".\n");
    out
}

/// Build the cost×quality frontier across all stored runs, joining each run's total
/// cost with its ingested quality scalar (6xj.2). Pure read; no re-execution, no payload.
pub fn frontier_for(
    db_path: &str,
    pricing: &tare_core::PricingTable,
) -> Result<tare_core::experiment::Frontier, String> {
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
            &runs, pricing, &quality, &sources,
        ),
    )
}

// ---- quality scalar: attach/read a user-supplied number, never computed ----

/// Attach (or replace) a run's quality scalar. `run_id` must name a stored run so a typo can't
/// silently orphan a score. `source` is provenance only.
pub fn set_quality_for(
    db_path: &str,
    run_id: &str,
    score: i64,
    source: &str,
) -> Result<(), String> {
    let store = Store::open(db_path)?;
    if store.load_run(run_id)?.is_none() {
        return Err(format!(
            "run `{run_id}` not found — capture it before scoring it"
        ));
    }
    let updated = now_unix_secs().to_string();
    store.set_run_quality(run_id, score, source, &updated)
}

pub fn quality_for(db_path: &str, run_id: &str) -> Result<Option<tare_store::RunQuality>, String> {
    Store::open(db_path)?.run_quality(run_id)
}

pub fn all_quality_for(db_path: &str) -> Result<Vec<tare_store::RunQuality>, String> {
    Store::open(db_path)?.all_run_quality()
}

pub fn clear_quality_for(db_path: &str, run_id: &str) -> Result<(), String> {
    Store::open(db_path)?.delete_run_quality(run_id)
}

pub fn render_advise_text(advice: &[tare_core::advise::CacheAdvice]) -> String {
    let mut out = String::new();
    out.push_str("Tare cache advisor — ESTIMATE, retrospective over the captured window\n\n");
    if advice.is_empty() {
        out.push_str("No repeated uncached system prefix found — nothing to cache.\n");
        return out;
    }
    for a in advice {
        let (save, tier) = match a.recommend.as_str() {
            "1h" => (a.save_1h_micros, "1h"),
            "5m" => (a.save_5m_micros, "5m"),
            _ => (0, "none"),
        };
        out.push_str(&format!(
            "{}/{}: {}-token system prefix sent {}× uncached ({}). \
             Cache it ({tier}) to have saved {} (break-even at {} read(s)).\n",
            a.provider,
            a.model,
            a.system_tokens,
            a.sends,
            MicroUsd(a.uncached_micros).to_dollar_string(),
            MicroUsd(save).to_dollar_string(),
            a.breakeven_reads
                .map(|b| b.to_string())
                .unwrap_or_else(|| "n/a".into()),
        ));
    }
    out
}

// ---- what-if model/tier swap ----

pub fn whatif_for(
    db_path: &str,
    swaps: &[tare_core::whatif::Swap],
    pricing: &PricingTable,
) -> Result<tare_core::whatif::WhatIfReport, String> {
    let runs = cached_runs(db_path)?;
    tare_core::whatif::whatif(&runs, pricing, swaps)
}

/// Reprice all captured runs under a conditional routing policy. Approximate
/// (cross-tokenizer); an unpriced target hard-errors.
pub fn route_whatif_for(
    db_path: &str,
    policy: &tare_core::whatif::RoutingPolicy,
    pricing: &PricingTable,
) -> Result<tare_core::whatif::WhatIfReport, String> {
    let runs = cached_runs(db_path)?;
    tare_core::whatif::route_whatif(&runs, pricing, policy)
}

/// Rank cheaper priced models for the captured runs (A5). Nothing is labeled exact.
pub fn whatif_recommend_for(
    db_path: &str,
    cross_provider: bool,
    pricing: &PricingTable,
) -> Result<tare_core::whatif::WhatIfRecommendations, String> {
    let runs = cached_runs(db_path)?;
    Ok(tare_core::whatif::recommend(&runs, pricing, cross_provider))
}

pub fn render_whatif_recommend_text(r: &tare_core::whatif::WhatIfRecommendations) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Tare what-if recommend — ESTIMATE (reprices captured counts; all rows APPROXIMATE) — baseline {}\n\n",
        MicroUsd(r.baseline_micros).to_dollar_string()
    ));
    out.push_str(&format!(
        "{:<34} {:>14} {:>14}\n",
        "MODEL", "TOTAL(est)", "Δ vs now"
    ));
    for c in &r.recommendations {
        let flag = if c.approximate_cross_provider {
            " *cross-provider"
        } else {
            ""
        };
        out.push_str(&format!(
            "{:<34} {:>14} {:>14}{}\n",
            format!("{}/{}", c.to_provider, c.to_model),
            MicroUsd(c.total_after_micros).to_dollar_string(),
            signed_dollars(c.delta_micros),
            flag,
        ));
    }
    if r.recommendations.is_empty() {
        out.push_str("(no priced candidate models)\n");
    }
    out
}

pub fn render_whatif_text(r: &tare_core::whatif::WhatIfReport) -> String {
    let mut out = String::new();
    out.push_str("Tare what-if — ESTIMATE (reprices captured token counts; not a re-run)\n");
    for s in &r.swaps {
        let mut caveats = Vec::new();
        if s.approximate_tokenizer {
            caveats.push("approx. tokenizer");
        }
        if s.approximate_cross_provider {
            caveats.push("cross-provider: cache semantics differ");
        }
        let note = if caveats.is_empty() {
            String::new()
        } else {
            format!("  [{}]", caveats.join("; "))
        };
        out.push_str(&format!(
            "  {} -> {}/{}{}\n",
            s.from, s.to_provider, s.to_model, note
        ));
    }
    out.push_str(&format!(
        "\n  before {} -> after {}  (Δ {})\n",
        MicroUsd(r.diff.total_before).to_dollar_string(),
        MicroUsd(r.diff.total_after).to_dollar_string(),
        signed_dollars(r.diff.delta_micros),
    ));
    out
}

// ---- pricing freshness + provenance ----

/// Human-readable pricing status: version, effective date, age in days vs `today` (passed in
/// so this stays clock-free), the provenance note, and the unpriced models seen in the store
/// using the same unpriced-model aggregation. `today` is `YYYY-MM-DD`.
pub fn pricing_status(
    db_path: &str,
    pricing: &PricingTable,
    today: &str,
) -> Result<String, String> {
    let mut out = String::new();
    out.push_str(&format!(
        "pricing version : {}\neffective date  : {}\n",
        pricing.version, pricing.effective_date
    ));
    if let (Some(eff), Some(now)) = (
        tare_core::calendar::parse_date(&pricing.effective_date),
        tare_core::calendar::parse_date(today),
    ) {
        let age = now - eff;
        out.push_str(&format!("age             : {age} day(s) as of {today}\n"));
        if age > 90 {
            out.push_str("  ⚠ pricing table is over 90 days old — rates may be stale.\n");
        }
        if age < 0 {
            out.push_str("  (effective date is in the future relative to your data)\n");
        }
    }
    if let Some(note) = &pricing.note {
        out.push_str(&format!("note            : {note}\n"));
    }
    // Unpriced models seen in the store provide the live correctness signal.
    match report_for(db_path, false, pricing)? {
        rep if !rep.unpriced.is_empty() => {
            out.push_str("unpriced models seen (NOT in any total):\n");
            for u in &rep.unpriced {
                out.push_str(&format!(
                    "  {} / {} — {} tokens, {} step(s)\n",
                    u.provider, u.model, u.token_total, u.step_count
                ));
            }
        }
        _ => out.push_str("unpriced models : none — every captured model is priced.\n"),
    }
    Ok(out)
}

// ---- explain (deterministic narrative for one run) ----

/// Plain-language narrative of a single run's spend. Builds the report over just that run.
pub fn explain_for(db_path: &str, run_id: &str, pricing: &PricingTable) -> Result<String, String> {
    let runs = [load_run(db_path, run_id)?];
    let report = attribute::build_report(&runs, pricing);
    let ledger = tare_core::savings::savings(&runs, pricing);
    Ok(tare_core::explain::explain(&report, &ledger))
}

// ---- rollup (spend by correlation label) ----

pub fn rollup_for(
    db_path: &str,
    dim: tare_core::rollup::RollupDim,
    pricing: &PricingTable,
) -> Result<tare_core::rollup::RollupReport, String> {
    rollup_filtered_for(db_path, dim, None, pricing)
}

/// As [`rollup_for`], but restricted to the steps matching a parent `(filter_dim, label)` — the
/// progressive-drill backend: "bucket by `dim` WHERE `filter_dim` = label". `None` is
/// identical to `rollup_for`.
pub fn rollup_filtered_for(
    db_path: &str,
    dim: tare_core::rollup::RollupDim,
    filter: Option<(tare_core::rollup::RollupDim, String)>,
    pricing: &PricingTable,
) -> Result<tare_core::rollup::RollupReport, String> {
    let runs = cached_runs(db_path)?;
    let filter_ref = filter.as_ref().map(|(d, l)| (*d, l.as_str()));
    Ok(tare_core::rollup::rollup_filtered(
        &runs, pricing, dim, filter_ref,
    ))
}

/// Estimate-vs-vendor reconciliation for one day. Joins Tare's per-model step-derived
/// estimate against Claude Code's own metered cost per model, and auto-attributes each residual to
/// a detectable cause. The estimate stays primary; the vendor figure is a cross-check, NEVER merged
/// into Tare's totals. Returns a JSON string.
///
/// Per-model `cause`:
/// - `unpriced`  — we captured tokens for this model but have no price → $0 estimate, full delta is
///   blind cost.
/// - `coverage_gap` — vendor reported cost for a model we captured no steps for (capture missed it).
/// - `estimate_only` — we estimated cost the vendor didn't report (e.g. a non-Claude-Code provider).
/// - `mismatch` — both sides present but the delta exceeds tolerance (pricing-version / rate drift).
/// - `ok` — both present and within tolerance.
pub fn reconcile_for(db_path: &str, pricing: &PricingTable, day: &str) -> Result<String, String> {
    use std::collections::BTreeMap;
    let store = Store::open(db_path)?;
    let runs = store.load_runs_on_date(day)?;
    let est = tare_core::rollup::rollup(&runs, pricing, tare_core::rollup::RollupDim::Model);
    let vendor = store.metered_by_model(day)?;

    // Index estimate rows by model label.
    let mut est_by_model: BTreeMap<String, (i64, u64)> = BTreeMap::new();
    for r in &est.rows {
        est_by_model.insert(r.label.clone(), (r.micros, r.tokens));
    }
    let vendor_by_model: BTreeMap<String, i64> = vendor.iter().cloned().collect();

    // Union of model labels from both sides.
    let mut models: Vec<String> = est_by_model
        .keys()
        .chain(vendor_by_model.keys())
        .cloned()
        .collect();
    models.sort();
    models.dedup();

    // Tolerance: 5% of the larger side, floored at 1000 micro-USD ($0.001), so tiny rounding
    // differences don't read as a mismatch.
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
        } else if i128::from(delta).abs() <= i128::from(tolerance(est_micros, vendor_micros)) {
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
        // The cross-check is only meaningful when the vendor actually reported something.
        "has_vendor": vendor_total > 0,
    }))
    .map_err(|e| e.to_string())
}

pub fn render_rollup_text(rep: &tare_core::rollup::RollupReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Tare rollup by {} — estimated (pricing {}) — total {}\n\n",
        rep.dimension,
        rep.pricing_version,
        MicroUsd(rep.total_micros).to_dollar_string()
    ));
    out.push_str(&format!(
        "{:<28} {:>6} {:>7} {:>12} {:>14}\n",
        "LABEL", "RUNS", "STEPS", "TOKENS", "TOTAL(est)"
    ));
    for r in &rep.rows {
        out.push_str(&format!(
            "{:<28} {:>6} {:>7} {:>12} {:>14}\n",
            r.label,
            r.runs,
            r.steps,
            r.tokens,
            MicroUsd(r.micros).to_dollar_string()
        ));
    }
    if rep.rows.is_empty() {
        out.push_str("(no spend recorded yet — run `tare run -- <cmd>`)\n");
    }
    out
}

// ---- sessions (spend grouped by owning session/task) ----

pub fn sessions_for(
    db_path: &str,
    pricing: &PricingTable,
) -> Result<tare_core::session::SessionReport, String> {
    Ok(tare_core::session::sessions(
        &cached_runs(db_path)?,
        pricing,
    ))
}

/// JSON for the per-session cost autopsy: the exact per-class decomposition + the
/// actionability-gated waste opportunities + the fallback-ladder headline for ONE session, for the
/// run-detail drill. `median` (the user's median session cost — the client already has it from the
/// sessions list) powers the vs-median reference; pass `None` to omit it. Out-of-band, no proxy.
pub fn session_autopsy_json(
    db_path: &str,
    run_id: &str,
    pricing: &PricingTable,
    median: Option<i64>,
) -> Result<String, String> {
    let run = load_run(db_path, run_id)?;
    let autopsy = tare_core::autopsy::session_autopsy(&run, pricing, median);
    serde_json::to_string(&autopsy).map_err(|e| format!("session_autopsy json: {e}"))
}

pub fn render_sessions_text(rep: &tare_core::session::SessionReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Tare sessions — estimated (pricing {}) — total {}\n\n",
        rep.pricing_version,
        MicroUsd(rep.total_micros).to_dollar_string()
    ));
    out.push_str(&format!(
        "{:<32} {:>5} {:>6} {:>6} {:>7} {:>14}\n",
        "SESSION", "RUNS", "STEPS", "TOOLS", "AGENTS", "TOTAL(est)"
    ));
    for r in &rep.rows {
        out.push_str(&format!(
            "{:<32} {:>5} {:>6} {:>6} {:>7} {:>14}\n",
            r.session,
            r.runs,
            r.steps,
            r.tools,
            r.agents,
            MicroUsd(r.micros).to_dollar_string()
        ));
    }
    if rep.rows.is_empty() {
        out.push_str("(no spend recorded yet)\n");
    }
    out
}

pub fn loop_waste_for(
    db_path: &str,
    pricing: &PricingTable,
) -> Result<tare_core::session::LoopWasteReport, String> {
    Ok(tare_core::session::loop_waste(
        &cached_runs(db_path)?,
        pricing,
    ))
}

pub fn render_loops_text(rep: &tare_core::session::LoopWasteReport) -> String {
    let mut out = format!(
        "Tare loop waste — estimated (pricing {}) — {} redundant re-issue(s), total {}\n\n",
        rep.pricing_version,
        rep.total_redundant_steps,
        MicroUsd(rep.total_micros).to_dollar_string()
    );
    out.push_str(&format!(
        "{:<28} {:>9} {:>10} {:>14}\n",
        "TOOL/AGENT", "REDUNDANT", "MAX-REPEAT", "WASTE(est)"
    ));
    for r in &rep.rows {
        out.push_str(&format!(
            "{:<28} {:>9} {:>10} {:>14}\n",
            r.label,
            r.redundant_steps,
            r.max_repeat,
            MicroUsd(r.micros).to_dollar_string()
        ));
    }
    if rep.rows.is_empty() {
        out.push_str("(no retry loops detected)\n");
    }
    out
}

pub fn failure_waste_for(
    db_path: &str,
    pricing: &PricingTable,
) -> Result<tare_core::session::FailureWasteReport, String> {
    Ok(tare_core::session::failure_waste(
        &cached_runs(db_path)?,
        pricing,
    ))
}

pub fn lenses_for(
    db_path: &str,
    pricing: &PricingTable,
) -> Result<tare_core::lenses::Lenses, String> {
    Ok(tare_core::lenses::lenses(&cached_runs(db_path)?, pricing))
}

pub fn sandwich_for(
    db_path: &str,
    pricing: &PricingTable,
    component: &str,
) -> Result<tare_core::lenses::Sandwich, String> {
    Ok(tare_core::lenses::component_sandwich(
        &cached_runs(db_path)?,
        pricing,
        component,
    ))
}

/// Local-day start (`YYYY-MM-DD`) of the current calendar `period`: "week" = most recent Sunday,
/// "month" = the 1st. Uses the configured tz offset so it matches `today_local()`.
fn period_start(period: &str) -> String {
    let offset = tz_offset_minutes();
    let days = now_unix_secs()
        .saturating_add(offset.saturating_mul(60))
        .div_euclid(86_400);
    match period {
        "week" => {
            // 1970-01-01 was a Thursday → weekday = (days + 4) % 7, 0 = Sunday.
            let weekday = (days + 4).rem_euclid(7);
            let (y, m, d) = tare_core::calendar::civil_from_days(days - weekday);
            format!("{y:04}-{m:02}-{d:02}")
        }
        // month (default): first of the current local month.
        _ => {
            let (y, m, _d) = tare_core::calendar::civil_from_days(days);
            format!("{y:04}-{m:02}-01")
        }
    }
}

/// Capture-coverage / blind-spot report: which sources are feeding cost steps vs only
/// emitting heartbeats (degraded capture / blind spend). Pure store read, clock-free. Status:
/// green = proxy AND OTel both feeding cost steps; amber = one channel; red = heartbeats but no
/// cost steps at all; none = nothing captured yet.
pub fn coverage_for(db_path: &str) -> Result<String, String> {
    let store = Store::open(db_path)?;
    let by_source = store.coverage_by_source()?;
    let heartbeats: std::collections::BTreeSet<String> = store
        .load_session_activity()?
        .into_iter()
        .map(|(source, _, _, _, _)| source)
        .collect();

    let total_steps = by_source
        .iter()
        .fold(0u64, |total, (_, count, _)| total.saturating_add(*count));
    let feeding: std::collections::BTreeSet<&str> = by_source
        .iter()
        .filter(|(_, n, _)| *n > 0)
        .map(|(s, _, _)| s.as_str())
        .collect();
    let has_proxy = feeding.contains("proxy");
    let has_otel = feeding.iter().any(|s| s.starts_with("otel"));

    // Sources with heartbeats but no captured cost steps = blind spend.
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

    // Union of cost-step sources + heartbeat-only sources (steps 0) so the strip shows blind ones.
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

fn periodic_budget_config(
    cfg: &tare_core::config::TareConfig,
) -> Result<(String, i64, i64), String> {
    let period = cfg.budget.period.clone().unwrap_or_else(|| "month".into());
    if !matches!(period.as_str(), "week" | "month") {
        return Err(format!(
            "budget.period must be \"week\" or \"month\", got {period:?}"
        ));
    }
    let cap_micros = match cfg.budget.period_max_spend_usd {
        Some(value) => tare_core::config::dollars_to_micros(value).ok_or_else(|| {
            "budget.period_max_spend_usd must be a finite non-negative amount".to_string()
        })?,
        None => 0,
    };
    let warn_pct = cfg.budget.warn_pct.unwrap_or(80);
    if !(1..=100).contains(&warn_pct) {
        return Err(format!(
            "budget.warn_pct must be from 1 to 100, got {warn_pct}"
        ));
    }
    Ok((period, cap_micros, warn_pct))
}

/// Clock-free burn-rate / projected-overrun for the current budget period (core
/// burnrate). Reuses the same period-to-date daily series as `period_budget_for`, then asks the
/// core engine to project "at your captured pace". Framed as an estimate, never a real-time clock
/// projection.
pub fn burnrate_for(db_path: &str, pricing: &PricingTable) -> Result<String, String> {
    burnrate_for_range(db_path, pricing, None)
}

/// Range-aware counterpart used by the interactive Pulse chart. With no explicit range this keeps
/// the historical configured-budget behavior for CLI alerts and older clients.
pub fn burnrate_for_range(
    db_path: &str,
    pricing: &PricingTable,
    requested_range: Option<&str>,
) -> Result<String, String> {
    let cfg = load_config_strict()?;
    let (configured_period, configured_cap_micros, _) = periodic_budget_config(&cfg)?;
    let period = requested_range.unwrap_or(&configured_period);
    let today = today_local();
    let window = tare_core::burnrate::projection_window(period, &today)?;
    let period = window.range.as_str();
    let start = window.start;
    let period_end = window.end;
    let days_in_period = window.days_in_period;
    // A weekly cap has no honest meaning on a YTD graph (and vice versa). Only draw and classify
    // against it when the selected chart window is the configured budget period.
    let cap_micros = if period == configured_period {
        configured_cap_micros
    } else {
        0
    };
    // Burn rate is period-to-date, not the generic trend endpoint's default 14-day window. Build
    // the dense calendar range directly so idle days at either edge remain part of the activity
    // frequency and the UI receives the actual history used by the projection.
    let trend = Store::open(db_path)?.trend_in_range(
        &start,
        &today,
        pricing,
        tare_core::trend::TrendDimension::Total,
    )?;
    let per_day: Vec<i64> = trend
        .series
        .first()
        .map(|s| s.per_day.clone())
        .unwrap_or_else(|| vec![0; trend.days.len()]);
    let br = tare_core::burnrate::project(&per_day, days_in_period, cap_micros);
    let mut v = serde_json::to_value(&br).map_err(|e| e.to_string())?;
    if let Some(obj) = v.as_object_mut() {
        obj.insert("period".into(), serde_json::json!(period));
        obj.insert("period_start".into(), serde_json::json!(start));
        obj.insert("as_of".into(), serde_json::json!(today));
        obj.insert("period_end".into(), serde_json::json!(period_end));
    }
    serde_json::to_string(&v).map_err(|e| e.to_string())
}

/// Periodic spend-budget status: sum daily spend on-or-after the period start and classify it
/// against the configured cap + warn threshold. Backward-looking (no forecasting).
pub fn period_budget_for(
    db_path: &str,
    pricing: &PricingTable,
) -> Result<tare_core::budget::PeriodBudget, String> {
    let cfg = load_config_strict()?;
    let (period, cap_micros, warn_pct) = periodic_budget_config(&cfg)?;
    let start = period_start(&period);
    // Sum the Total-dimension daily trend on-or-after the period start.
    let spent = match trend_for(
        db_path,
        None,
        None,
        tare_core::trend::TrendDimension::Total,
        pricing,
    )? {
        Some(t) => t
            .series
            .first()
            .map(|s| {
                t.days
                    .iter()
                    .zip(s.per_day.iter())
                    .filter(|(d, _)| d.as_str() >= start.as_str())
                    .map(|(_, v)| *v)
                    .fold(0i64, i64::saturating_add)
            })
            .unwrap_or(0),
        None => 0,
    };
    Ok(tare_core::budget::period_status(
        &period, spent, cap_micros, warn_pct,
    ))
}

pub fn render_failures_text(rep: &tare_core::session::FailureWasteReport) -> String {
    let mut out = format!(
        "Tare failure waste — estimated (pricing {}) — {} failed step(s), {} ({}% of spend)\n\n",
        rep.pricing_version,
        rep.total_failed_steps,
        MicroUsd(rep.total_micros).to_dollar_string(),
        rep.pct_of_spend
    );
    out.push_str(&format!(
        "{:<28} {:>7} {:>14}\n",
        "TOOL/AGENT", "FAILED", "WASTE(est)"
    ));
    for r in &rep.rows {
        out.push_str(&format!(
            "{:<28} {:>7} {:>14}\n",
            r.label,
            r.failed_steps,
            MicroUsd(r.micros).to_dollar_string()
        ));
    }
    if rep.rows.is_empty() {
        out.push_str("(no failed/refused steps)\n");
    }
    out
}

// ---- loopback read API (GET /__tare/*) served by `tare serve` ----

/// Percent/`+`-decode a query value (so a `run` id with spaces or `/` round-trips with the
/// browser client's `encodeURIComponent`/`URLSearchParams`).
fn pct_decode(s: &str) -> Result<String, String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            // Decode `%HH` only when both following bytes are ASCII hex. Operating on the BYTES
            // (not a `&s[..]` slice) avoids panicking when `%` precedes a multi-byte UTF-8 char.
            b'%' => {
                let Some(hex) = b.get(i + 1..i + 3) else {
                    return Err("truncated percent escape in query".into());
                };
                if !hex.iter().all(u8::is_ascii_hexdigit) {
                    return Err("invalid percent escape in query".into());
                }
                let hi = (hex[0] as char).to_digit(16).unwrap_or(0);
                let lo = (hex[1] as char).to_digit(16).unwrap_or(0);
                out.push((hi * 16 + lo) as u8);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| "query value is not valid UTF-8".into())
}

/// Minimal `k=v&k=v` query parser. Values are percent/`+`-decoded; keys are our own fixed names.
fn parse_query(q: &str) -> Result<std::collections::HashMap<String, String>, String> {
    let mut parsed = std::collections::HashMap::new();
    for pair in q.split('&').filter(|pair| !pair.is_empty()) {
        // A bare flag (`?today`) maps to an empty value rather than being dropped.
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key.is_empty() {
            return Err("query parameter name must not be empty".into());
        }
        if parsed.insert(key.to_string(), pct_decode(value)?).is_some() {
            return Err(format!("duplicate query parameter {key:?}"));
        }
    }
    Ok(parsed)
}

/// Serve a read-only `/__tare/*` request from the store. Returns `(status, json_body)`, or
/// `None` for an unknown path (proxy answers 404). COUNTS ONLY — same redaction as everywhere.
/// The loopback-only WRITE API: `POST /__tare/notes` upserts a run's annotation,
/// `POST /__tare/notes/delete` purges it. Validates strictly BEFORE touching the store (run_id +
/// tag charset bounded); the store enforces the length caps as a second backstop. Returns
/// `(status, content_type, body)`.
pub fn write_api(db: &str, path_and_query: &str, body: &[u8]) -> (u16, String, String) {
    const JSON: &str = "application/json";
    let (path, _q) = path_and_query
        .split_once('?')
        .unwrap_or((path_and_query, ""));
    let bad = |m: &str| {
        (
            400u16,
            JSON.to_string(),
            format!("{{\"error\":{}}}", json_str(m)),
        )
    };
    let okj = || (200u16, JSON.to_string(), "{\"ok\":true}".to_string());
    let errj = |e: String| {
        (
            500u16,
            JSON.to_string(),
            format!("{{\"error\":{}}}", json_str(&e)),
        )
    };

    // Calibrated Bench cohort analysis endpoints: POST-only, wrapped in
    // the AnalysisResponse{data,provenance} envelope. Dispatched BEFORE the generic body parse + the
    // run_id check since cohort bodies are typed DTOs, not run-keyed writes. Body is capped at
    // 256 KiB before any parse; the shared cohort_*_json fns classify errors to a status.
    if let Some(sub) = path.strip_prefix("/__tare/cohort/") {
        const MAX_COHORT_BODY: usize = 256 * 1024;
        if body.len() > MAX_COHORT_BODY {
            return (
                413u16,
                JSON.to_string(),
                format!("{{\"error\":{}}}", json_str("request body exceeds 256 KiB")),
            );
        }
        let res = match sub {
            "resolve" => cohort_resolve_api(db, body),
            "facets" => cohort_facets_api(db, body),
            "compare" => cohort_compare_api(db, body),
            "search" => cohort_search_api(db, body),
            "timeline" => cohort_timeline_api(db, body),
            _ => Err((404u16, format!("unknown cohort endpoint: {sub}"))),
        };
        return match res {
            Ok(json) => (200u16, JSON.to_string(), json),
            Err((code, msg)) => (
                code,
                JSON.to_string(),
                format!("{{\"error\":{}}}", json_str(&msg)),
            ),
        };
    }

    // Scoped anomaly explanation: POST because the optional `scope` is a full
    // CohortSpec. Resolves scope BEFORE detection (in `anomaly_why_scoped`) and returns the bare
    // deterministic volume/size/efficiency decomposition array — same shape the core produces, so a
    // fixture's headline/components match core exactly.
    if path == "/__tare/anomaly_why" {
        if body.len() > 256 * 1024 {
            return (
                413u16,
                JSON.to_string(),
                format!("{{\"error\":{}}}", json_str("request body exceeds 256 KiB")),
            );
        }
        let req: tare_core::anomaly::AnomalyWhyRequest = match serde_json::from_slice(body) {
            Ok(r) => r,
            Err(e) => return bad(&format!("bad AnomalyWhyRequest: {e}")),
        };
        let pricing = match analysis_pricing() {
            Ok(pricing) => pricing,
            Err(error) => return errj(error),
        };
        return match anomaly_why_scoped(db, &pricing, &req)
            .and_then(|w| serde_json::to_string(&w).map_err(|e| e.to_string()))
        {
            Ok(json) => (200u16, JSON.to_string(), json),
            Err(e) => errj(e),
        };
    }

    // Offline counterfactual cost experiment over a cohort: POST because the
    // body carries a full CohortSpec + axis grid. Executes the grid by repricing stored usage — no
    // re-execution, no payload read. Complements the read-only GET /__tare/frontier (unchanged).
    if path == "/__tare/experiment" {
        if body.len() > 256 * 1024 {
            return (
                413u16,
                JSON.to_string(),
                format!("{{\"error\":{}}}", json_str("request body exceeds 256 KiB")),
            );
        }
        let req: tare_core::experiment::ExperimentRequest = match serde_json::from_slice(body) {
            Ok(r) => r,
            Err(e) => return bad(&format!("bad ExperimentRequest: {e}")),
        };
        let pricing = match analysis_pricing() {
            Ok(pricing) => pricing,
            Err(error) => return errj(error),
        };
        return match Store::open(db)
            .and_then(|s| s.experiment(&req, &pricing))
            .and_then(|r| serde_json::to_string(&r).map_err(|e| e.to_string()))
        {
            Ok(json) => (200u16, JSON.to_string(), json),
            Err(e) => errj(e),
        };
    }

    // Durable saved investigations: the cross-transport source of truth. Upsert
    // the full DTO (state-minus-focus + columns + pane widths + loss marker); the store enforces the
    // 256 KiB per-item / 1 MiB total caps. Distinct path suffix for delete. No run_id involved.
    // Both paths upsert; `/investigations/upsert` remains an explicit alias for compatibility.
    if path == "/__tare/investigations" || path == "/__tare/investigations/upsert" {
        if body.len() > 256 * 1024 {
            return (
                413u16,
                JSON.to_string(),
                format!(
                    "{{\"error\":{}}}",
                    json_str("investigation exceeds 256 KiB")
                ),
            );
        }
        let dto: serde_json::Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(e) => return bad(&format!("bad SavedInvestigation: {e}")),
        };
        let store = match Store::open(db) {
            Ok(s) => s,
            Err(e) => return errj(e),
        };
        return match store.upsert_investigation(&dto) {
            Ok(()) => okj(),
            Err(e) if e.contains("exceed") => (
                413u16,
                JSON.to_string(),
                format!("{{\"error\":{}}}", json_str(&e)),
            ),
            Err(e) => bad(&e),
        };
    }
    if path == "/__tare/investigations/delete" {
        let id = match serde_json::from_slice::<serde_json::Value>(body) {
            Ok(v) => v
                .get("id")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string(),
            Err(e) => return bad(&format!("bad json: {e}")),
        };
        if id.is_empty() {
            return bad("id required");
        }
        return match Store::open(db).and_then(|s| s.delete_investigation(&id)) {
            Ok(()) => okj(),
            Err(e) => errj(e),
        };
    }

    // Savings action lifecycle: apply / dismiss persist a SavingsActionRequest
    // keyed on (opportunity_key, canonical cohort_hash); unaccept removes the exact row by identity.
    // Status errors: 400 bad contract, 404 unknown action (unaccept), 409 hash collision or
    // incompatible in-place transition. `acted_at` is stamped here (the store is clock-free).
    if path == "/__tare/savings/accept" || path == "/__tare/savings/dismiss" {
        let req: tare_core::savings::SavingsActionRequest = match serde_json::from_slice(body) {
            Ok(r) => r,
            Err(e) => return bad(&format!("bad SavingsActionRequest: {e}")),
        };
        let status = if path.ends_with("dismiss") {
            "dismissed"
        } else {
            "applied"
        };
        let store = match Store::open(db) {
            Ok(s) => s,
            Err(e) => return errj(e),
        };
        return match store.put_savings_action(&req, status, &now_rfc3339_utc()) {
            Ok(()) => okj(),
            Err((code, msg)) => (
                code,
                JSON.to_string(),
                format!("{{\"error\":{}}}", json_str(&msg)),
            ),
        };
    }
    if path == "/__tare/savings/unaccept" {
        let id: tare_core::savings::SavingsActionIdentity = match serde_json::from_slice(body) {
            Ok(i) => i,
            Err(e) => return bad(&format!("bad SavingsActionIdentity: {e}")),
        };
        let store = match Store::open(db) {
            Ok(s) => s,
            Err(e) => return errj(e),
        };
        return match store.delete_savings_action(&id) {
            Ok(()) => okj(),
            Err((code, msg)) => (
                code,
                JSON.to_string(),
                format!("{{\"error\":{}}}", json_str(&msg)),
            ),
        };
    }
    // Cohort-scoped observed-reduction verification: POST because the body is a
    // SavingsVerifyRequest (identity + optional as_of/window). Read-only, but POST per the route
    // contract. 404 unknown action, 500 store failure.
    if path == "/__tare/savings/verify" {
        let req: tare_core::savings::SavingsVerifyRequest = match serde_json::from_slice(body) {
            Ok(r) => r,
            Err(e) => return bad(&format!("bad SavingsVerifyRequest: {e}")),
        };
        let pricing = match analysis_pricing() {
            Ok(pricing) => pricing,
            Err(error) => return errj(error),
        };
        let store = match Store::open(db) {
            Ok(s) => s,
            Err(e) => return errj(e),
        };
        return match store
            .verify_savings_action(&req, &pricing)
            .and_then(|r| serde_json::to_string(&r).map_err(|e| (500u16, e.to_string())))
        {
            Ok(json) => (200u16, JSON.to_string(), json),
            Err((code, msg)) => (
                code,
                JSON.to_string(),
                format!("{{\"error\":{}}}", json_str(&msg)),
            ),
        };
    }

    let v: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return bad(&format!("bad json: {e}")),
    };
    if !v.is_object() {
        return bad("request body must be a JSON object");
    }

    // Acknowledge an anomaly: appends its key to [anomaly].acknowledged in tare.toml
    // so detect() hides it. Keyed on `key`, not `run_id`, so it branches before the note checks.
    if path == "/__tare/acknowledge" {
        let key = v.get("key").and_then(|x| x.as_str()).unwrap_or("");
        return match tare_core::config::TareConfig::acknowledge_anomaly(&config_path(), key) {
            Ok(()) => okj(),
            Err(e) => bad(&e),
        };
    }

    // Seed the bundled sample run (onboarding activation): so a first-run user who hasn't
    // captured anything yet can still land on a populated Overview. Idempotent — the demo run has a
    // fixed id, so re-seeding overwrites rather than duplicating. No run_id in the body.
    if path == "/__tare/demo" {
        return match demo_seed_store(db) {
            Ok(_) => okj(),
            Err(e) => bad(&e),
        };
    }
    // one-click purge of ALL captured transcripts (opt-out). No run_id in the body.
    if path == "/__tare/transcript_purge" {
        return match transcript_purge(db) {
            Ok(_) => okj(),
            Err(e) => bad(&e),
        };
    }

    let run_id = v.get("run_id").and_then(|x| x.as_str()).unwrap_or("");
    if run_id.is_empty() || run_id.len() > 128 || run_id.chars().any(char::is_control) {
        return bad("run_id required (1-128 bytes, no control characters)");
    }
    match path {
        "/__tare/notes" => {
            let note_text = match v.get("note_text") {
                Some(value) => match value.as_str() {
                    Some(text) => text,
                    None => return bad("note_text must be a string"),
                },
                None => "",
            };
            let starred = match v.get("starred") {
                Some(value) => match value.as_bool() {
                    Some(starred) => starred,
                    None => return bad("starred must be a boolean"),
                },
                None => false,
            };
            let mut tags: Vec<String> = Vec::new();
            let mut unique_tags = std::collections::BTreeSet::new();
            if let Some(arr) = v.get("tags").and_then(|x| x.as_array()) {
                for t in arr {
                    let Some(s) = t.as_str() else {
                        return bad("tags must be strings");
                    };
                    // Strict charset keeps tags renderable and the JSON-array match unambiguous.
                    if s.is_empty()
                        || s.len() > 32
                        || !s
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                        || !unique_tags.insert(s)
                    {
                        return bad("each tag must be a unique 1-32 byte [A-Za-z0-9_-] value");
                    }
                    tags.push(s.to_string());
                }
            } else if v.get("tags").is_some() {
                return bad("tags must be an array of strings");
            }
            if tags.len() > 24 {
                return bad("too many tags (max 24)");
            }
            let tags_json = match serde_json::to_string(&tags) {
                Ok(json) => json,
                Err(error) => return errj(format!("serialize tags: {error}")),
            };
            let updated = now_unix_secs().to_string();
            let store = match Store::open(db) {
                Ok(store) => store,
                Err(error) => return errj(error),
            };
            match store.load_run(run_id) {
                Ok(Some(_)) => {}
                Ok(None) => return bad("cannot annotate an unknown run"),
                Err(error) => return errj(error),
            }
            match store.upsert_run_note(run_id, &tags_json, note_text, starred, &updated) {
                Ok(()) => okj(),
                Err(e) => errj(e),
            }
        }
        "/__tare/notes/delete" => match Store::open(db).and_then(|s| s.delete_run_note(run_id)) {
            Ok(()) => okj(),
            Err(e) => errj(e),
        },
        // Attach a user-supplied quality scalar. Tare STORES the number, never
        // computes it — the value must arrive as a bare integer in the body; a request that asks
        // Tare to run/read anything to derive it has no representation here and is simply absent.
        "/__tare/quality" => {
            let Some(score) = v.get("score").and_then(|x| x.as_i64()) else {
                return bad("score required (integer)");
            };
            let source = match v.get("source") {
                None => "cli",
                Some(value) => match value.as_str() {
                    Some(source @ ("cli" | "header" | "ci" | "ui")) => source,
                    _ => return bad("source must be one of: cli | header | ci | ui"),
                },
            };
            let updated = now_unix_secs().to_string();
            let store = match Store::open(db) {
                Ok(store) => store,
                Err(error) => return errj(error),
            };
            match store.load_run(run_id) {
                Ok(Some(_)) => {}
                Ok(None) => return bad("cannot score an unknown run"),
                Err(error) => return errj(error),
            }
            match store.set_run_quality(run_id, score, source, &updated) {
                Ok(()) => okj(),
                Err(e) => errj(e),
            }
        }
        "/__tare/quality/delete" => {
            match Store::open(db).and_then(|s| s.delete_run_quality(run_id)) {
                Ok(()) => okj(),
                Err(e) => errj(e),
            }
        }
        _ => (
            404,
            JSON.to_string(),
            "{\"error\":\"unknown write path\"}".to_string(),
        ),
    }
}

pub fn read_api(
    db: &str,
    pricing: &PricingTable,
    path_and_query: &str,
) -> Option<(u16, String, String)> {
    let (path, query) = path_and_query
        .split_once('?')
        .unwrap_or((path_and_query, ""));
    const JSON: &str = "application/json";
    let q = match parse_query(query) {
        Ok(query) => query,
        Err(error) => {
            return Some((
                400,
                JSON.to_string(),
                format!("{{\"error\":{}}}", json_str(&error)),
            ))
        }
    };
    let ok = |s: String| Some((200u16, JSON.to_string(), s));
    let err = |e: String| {
        Some((
            500u16,
            JSON.to_string(),
            format!("{{\"error\":{}}}", json_str(&e)),
        ))
    };
    let not_found = |e: String| {
        Some((
            404u16,
            JSON.to_string(),
            format!("{{\"error\":{}}}", json_str(&e)),
        ))
    };
    let bad = |e: String| {
        Some((
            400u16,
            JSON.to_string(),
            format!("{{\"error\":{}}}", json_str(&e)),
        ))
    };
    let to_json = |r: Result<String, String>| match r {
        Ok(s) => ok(s),
        Err(e) => err(e),
    };
    match path {
        "/__tare/runs" => match Store::open(db).and_then(|s| s.all_run_ids()) {
            // Run-ids-only query — no per-step reconstruction just to return the id list.
            Ok(ids) => to_json(serde_json::to_string(&ids).map_err(|e| e.to_string())),
            Err(e) => err(e),
        },
        // Per-session cost autopsy: exact class decomposition + waste opportunities +
        // fallback-ladder headline for one session. `id`=run/session id; optional `median` (the user's
        // median session cost, which the client already has) powers the vs-median reference.
        "/__tare/session_autopsy" => {
            let run_id = q
                .get("id")
                .or_else(|| q.get("run"))
                .cloned()
                .unwrap_or_default();
            if run_id.is_empty() {
                return bad("session_autopsy requires `id` or `run`".into());
            }
            let median = match q.get("median") {
                Some(raw) => match raw.parse::<i64>() {
                    Ok(value) => Some(value),
                    Err(_) => return bad(format!("invalid median {raw:?}")),
                },
                None => None,
            };
            to_json(session_autopsy_json(db, &run_id, pricing, median))
        }
        // User-authored run notes: read side. Writes go through the POST write handler.
        "/__tare/run_note" => {
            let run_id = q.get("run_id").cloned().unwrap_or_default();
            if run_id.is_empty() {
                return bad("run_note requires `run_id`".into());
            }
            to_json(
                Store::open(db)
                    .and_then(|s| s.load_run_note(&run_id))
                    .and_then(|n| serde_json::to_string(&n).map_err(|e| e.to_string())),
            )
        }
        "/__tare/notes_by_tag" => {
            let tag = q.get("tag").cloned().unwrap_or_default();
            if tag.is_empty()
                || tag.len() > 32
                || !tag
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                return bad("tag must be 1-32 bytes of [A-Za-z0-9_-]".into());
            }
            to_json(
                Store::open(db)
                    .and_then(|s| s.notes_by_tag(&tag))
                    .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
            )
        }
        "/__tare/starred_runs" => to_json(
            Store::open(db)
                .and_then(|s| s.list_starred())
                .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        ),
        "/__tare/report" => to_json(
            report_for(db, q.contains_key("today"), pricing)
                .and_then(|rep| serde_json::to_string(&rep).map_err(|e| e.to_string())),
        ),
        "/__tare/punchcard" => to_json(
            punchcard_for(db, pricing)
                .and_then(|m| serde_json::to_string(&m).map_err(|e| e.to_string())),
        ),
        "/__tare/heatmap" => {
            let window = match q.get("window") {
                Some(raw) => match raw.parse::<usize>() {
                    Ok(value) if value > 0 => Some(value),
                    _ => return bad(format!("invalid heatmap window {raw:?}; expected 1+")),
                },
                None => None,
            };
            to_json(
                heatmap_for(db, pricing, window)
                    .and_then(|m| serde_json::to_string(&m).map_err(|e| e.to_string())),
            )
        }
        "/__tare/today" => to_json(report_for(db, true, pricing).and_then(|rep| {
            serde_json::to_string(&serde_json::json!({
                "total_micros": rep.total_micros,
                "pricing_version": rep.pricing_version,
                "effective_date": rep.effective_date,
            }))
            .map_err(|e| e.to_string())
        })),
        "/__tare/vendor_today" => {
            // Claude Code's OWN reported spend/tokens today (from its OTLP metrics), as an honest
            // cross-check against Tare's step-derived estimate — never added to it. `available` is
            // false when no vendor metrics were captured (so the UI can hide the cross-check).
            let day = q.get("day").cloned().unwrap_or_else(today_local);
            if tare_core::calendar::parse_date(&day).is_none() {
                return bad(format!("invalid day {day:?}; expected YYYY-MM-DD"));
            }
            to_json(
                Store::open(db)
                    .and_then(|s| s.metered_totals(&day))
                    .and_then(|(cost_micros, tokens)| {
                        serde_json::to_string(&serde_json::json!({
                            "cost_micros": cost_micros,
                            "tokens": tokens,
                            "day": day,
                            "available": cost_micros > 0 || tokens > 0,
                        }))
                        .map_err(|e| e.to_string())
                    }),
            )
        }
        "/__tare/burnrate" => {
            // Clock-free run-rate + projected period spend. Estimate, not a forecast.
            let range = q.get("range").map(String::as_str);
            if let Some(value) = range {
                if let Err(error) = tare_core::burnrate::ProjectionRange::parse(value) {
                    return bad(error);
                }
            }
            to_json(burnrate_for_range(db, pricing, range))
        }
        "/__tare/coverage" => {
            // Capture-coverage / blind-spot meter: is DATA flowing from each channel?
            to_json(coverage_for(db))
        }
        "/__tare/reconcile" => {
            // Per-model estimate-vs-vendor reconciliation for a day. Estimate stays
            // primary; vendor is a cross-check, never merged.
            let day = q.get("day").cloned().unwrap_or_else(today_local);
            if tare_core::calendar::parse_date(&day).is_none() {
                return bad(format!("invalid day {day:?}; expected YYYY-MM-DD"));
            }
            to_json(reconcile_for(db, pricing, &day))
        }
        "/__tare/run_meta" => {
            // Recorded provenance for a run: created date, privacy policy/profile,
            // distinct models/providers/capture-sources/stop-reasons, + pricing version/date.
            let Some(run_id) = q.get("run") else {
                return bad("run_meta requires `run`".into());
            };
            to_json(
                Store::open(db)
                    .and_then(|s| s.run_meta(run_id))
                    .and_then(|meta| {
                        let meta = meta.ok_or_else(|| format!("unknown run {run_id}"))?;
                        let mut v = serde_json::to_value(&meta).map_err(|e| e.to_string())?;
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert(
                                "pricing_version".into(),
                                serde_json::json!(pricing.version),
                            );
                            obj.insert(
                                "effective_date".into(),
                                serde_json::json!(pricing.effective_date),
                            );
                        }
                        serde_json::to_string(&v).map_err(|e| e.to_string())
                    }),
            )
        }
        "/__tare/run_status" => {
            let Some(run_id) = q.get("run") else {
                return bad("run_status requires `run`".into());
            };
            match Store::open(db).and_then(|store| store.load_run(run_id)) {
                Ok(Some(run)) => {
                    let rep = attribute::build_report(std::slice::from_ref(&run), pricing);
                    let tokens = run
                        .steps
                        .iter()
                        .fold(0u64, |total, step| total.saturating_add(step.usage.total()));
                    let last_model = run
                        .steps
                        .last()
                        .map(|s| s.model.clone())
                        .unwrap_or_default();
                    // Lifecycle flags for the runs-table state pill. `unpriced` = at
                    // least one token-bearing step has no price (blind cost); `errored` = the run's
                    // terminal step is a retry-worthy failure.
                    let unpriced = run.steps.iter().any(|s| {
                        s.usage.total() > 0
                            && pricing
                                .lookup(s.provider, s.shape.vendor.as_deref(), &s.model)
                                .is_none()
                    });
                    let errored = run
                        .steps
                        .last()
                        .map(|s| s.is_retry_worthy_failure())
                        .unwrap_or(false);
                    let body = serde_json::to_string(&serde_json::json!({
                        "run_id": run_id,
                        "micros": rep.total_micros,
                        "steps": run.steps.len(),
                        "top_cause": rep.rows.first().map(|r| r.cause.clone()),
                        "tokens": tokens,
                        "last_model": last_model,
                        "unpriced": unpriced,
                        "errored": errored,
                    }))
                    .map_err(|e| e.to_string());
                    to_json(body)
                }
                Ok(None) => not_found(format!("run `{run_id}` not found in {db}")),
                Err(error) => err(error),
            }
        }
        "/__tare/run_statuses" => {
            // Batch: every run's status in ONE pass over the cached run set, so the
            // Runs list makes a single request instead of firing a /__tare/run_status round-trip per
            // run (an N+1 that also re-opened the DB N times). Same per-run object shape as above.
            to_json((|| -> Result<String, String> {
                let runs = cached_runs(db)?;
                let out: Vec<serde_json::Value> = runs
                    .iter()
                    .map(|run| {
                        let rep = attribute::build_report(std::slice::from_ref(run), pricing);
                        let unpriced = run.steps.iter().any(|s| {
                            s.usage.total() > 0
                                && pricing
                                    .lookup(s.provider, s.shape.vendor.as_deref(), &s.model)
                                    .is_none()
                        });
                        let errored = run
                            .steps
                            .last()
                            .map(|s| s.is_retry_worthy_failure())
                            .unwrap_or(false);
                        serde_json::json!({
                            "run_id": run.run_id,
                            "micros": rep.total_micros,
                            "steps": run.steps.len(),
                            "top_cause": rep.rows.first().map(|r| r.cause.clone()),
                            "tokens": run.steps.iter().fold(0u64, |total, step| total.saturating_add(step.usage.total())),
                            "last_model": run.steps.last().map(|s| s.model.clone()).unwrap_or_default(),
                            "unpriced": unpriced,
                            "errored": errored,
                        })
                    })
                    .collect();
                serde_json::to_string(&out).map_err(|e| e.to_string())
            })())
        }
        "/__tare/run_steps" => {
            // Ordered per-step timeline (counts + cost) for the run's Inspect tab.
            let Some(run_id) = q.get("run") else {
                return bad("run_steps requires `run`".into());
            };
            to_json(load_run(db, run_id).and_then(|run| {
                let steps: Vec<serde_json::Value> = run
                    .steps
                    .iter()
                    .map(|s| {
                        let micros = pricing
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
                            // Timing/span fields: decimal strings, null when absent.
                            // (step-order-only). end derived from start + duration, never persisted.
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
            }))
        }
        "/__tare/recent_steps" => {
            // Live activity tail: the most recent steps across all runs, newest first.
            let n = match q.get("n") {
                Some(raw) => match raw.parse::<u32>() {
                    Ok(value) if (1..=50).contains(&value) => value,
                    _ => return bad(format!("invalid recent_steps n {raw:?}; expected 1-50")),
                },
                None => 12,
            };
            to_json(
                Store::open(db)
                    .and_then(|s| s.recent_steps(n))
                    .and_then(|steps| {
                        let out: Vec<serde_json::Value> = steps
                            .iter()
                            .map(|s| {
                                let micros = pricing
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
                    }),
            )
        }
        "/__tare/flamegraph" => {
            let Some(run_id) = q.get("run") else {
                return bad("flamegraph requires `run`".into());
            };
            to_json(load_run(db, run_id).and_then(|run| {
                serde_json::to_string(&build_flamegraph(&run, pricing)).map_err(|e| e.to_string())
            }))
        }
        "/__tare/quality" => {
            // User-supplied quality scalars. ?run=ID → one run (204-style empty when
            // absent, rendered as `null`); no run → the whole set (frontier y-axis source).
            let store = match Store::open(db) {
                Ok(s) => s,
                Err(e) => return Some((500, "text/plain".into(), e)),
            };
            match q.get("run") {
                Some(run_id) => to_json(
                    store
                        .run_quality(run_id)
                        .and_then(|opt| serde_json::to_string(&opt).map_err(|e| e.to_string())),
                ),
                None => to_json(
                    store
                        .all_run_quality()
                        .and_then(|all| serde_json::to_string(&all).map_err(|e| e.to_string())),
                ),
            }
        }
        "/__tare/frontier" => to_json(
            frontier_for(db, pricing)
                .and_then(|f| serde_json::to_string(&f).map_err(|e| e.to_string())),
        ),
        "/__tare/profile" => {
            // Flat/cum profile table over a run's flamegraph. ?sort=flat|cum
            // (default cum), ?top=N truncates.
            let Some(run_id) = q.get("run") else {
                return bad("profile requires `run`".into());
            };
            let sort = match q.get("sort").map(String::as_str) {
                Some("flat") => tare_core::flamegraph::ProfileSort::Flat,
                Some("cum") | None => tare_core::flamegraph::ProfileSort::Cum,
                Some(other) => return bad(format!("invalid profile sort {other:?}")),
            };
            let top_n = match q.get("top") {
                Some(raw) => match raw.parse::<usize>() {
                    Ok(value) if value > 0 => Some(value),
                    _ => return bad(format!("invalid profile top {raw:?}; expected 1+")),
                },
                None => None,
            };
            to_json(load_run(db, run_id).and_then(|run| {
                let model = build_flamegraph(&run, pricing);
                let table = tare_core::flamegraph::profile_table(&model, sort, top_n);
                serde_json::to_string(&table).map_err(|e| e.to_string())
            }))
        }
        "/__tare/advise" => to_json(
            advise_for(db, pricing)
                .and_then(|a| serde_json::to_string(&a).map_err(|e| e.to_string())),
        ),
        // Additive SavingsLedgerV2: a superset of the v1 ledger — old
        // consumers keep reading the retained fields, new ones get OpportunityV2 rows + the explicit
        // capped-potential / applied / observed totals.
        "/__tare/savings" => to_json(
            savings_v2_for(db, pricing)
                .and_then(|s| serde_json::to_string(&s).map_err(|e| e.to_string())),
        ),
        // Persisted savings actions: the applied/dismissed lifecycle rows, each
        // with its compatibility warnings (aggregate-only / legacy rows flagged). Read side of the
        // apply/dismiss/unaccept writes; verification status layers on top.
        "/__tare/savings/actions" => to_json(
            Store::open(db)
                .and_then(|s| s.list_savings_actions())
                .and_then(|a| serde_json::to_string(&a).map_err(|e| e.to_string())),
        ),
        "/__tare/action_plan" => to_json(
            action_plan_for(db, pricing)
                .and_then(|p| serde_json::to_string(&p).map_err(|e| e.to_string())),
        ),
        "/__tare/cache_ledger" => to_json(
            cache_ledger_for(db, pricing)
                .and_then(|l| serde_json::to_string(&l).map_err(|e| e.to_string())),
        ),
        "/__tare/reasoning" => to_json(
            reasoning_for(db, pricing)
                .and_then(|r| serde_json::to_string(&r).map_err(|e| e.to_string())),
        ),
        "/__tare/confidence" => to_json(
            confidence_for(db, pricing, &today_local())
                .and_then(|c| serde_json::to_string(&c).map_err(|e| e.to_string())),
        ),
        "/__tare/effectiveness" => to_json(
            // Default window: all time (wide bounds). ?from/&to narrow it.
            effectiveness_for(
                db,
                q.get("from").map(String::as_str).unwrap_or("0001-01-01"),
                q.get("to").map(String::as_str).unwrap_or("9999-12-31"),
                pricing,
            )
            .and_then(|e| serde_json::to_string(&e).map_err(|e| e.to_string())),
        ),
        "/__tare/whatif" => {
            // Recommend mode only on the read API (named swaps are a CLI/MCP affordance).
            let cross = q.contains_key("cross_provider");
            to_json(
                whatif_recommend_for(db, cross, pricing)
                    .and_then(|r| serde_json::to_string(&r).map_err(|e| e.to_string())),
            )
        }
        "/__tare/anomalies" => {
            let by = match q.get("by") {
                Some(raw) => match tare_core::trend::TrendDimension::parse(raw) {
                    Some(dimension) => dimension,
                    None => return bad(format!("invalid anomaly dimension {raw:?}")),
                },
                None => tare_core::trend::TrendDimension::Total,
            };
            // Query wins, then [anomaly] config, then the 7/50 default — matching the desktop
            // command so serve and Tauri detect the SAME anomalies.
            let acfg = match load_config_strict() {
                Ok(cfg) => cfg.anomaly,
                Err(error) => return err(error),
            };
            let window = match q.get("window") {
                Some(raw) => match raw.parse::<usize>() {
                    Ok(value) if value > 0 => value,
                    _ => return bad(format!("invalid anomaly window {raw:?}; expected 1+")),
                },
                None => acfg.window.unwrap_or(7),
            };
            let threshold = match q.get("threshold") {
                Some(raw) => match raw.parse::<i64>() {
                    Ok(value) if value >= 0 => value,
                    _ => {
                        return bad(format!(
                            "invalid anomaly threshold {raw:?}; expected a non-negative integer"
                        ))
                    }
                },
                None => acfg.threshold.unwrap_or(50),
            };
            if window == 0 || threshold < 0 {
                return err("invalid anomaly window/threshold in tare.toml".into());
            }
            to_json(
                anomalies_for(db, by, window, threshold, pricing)
                    .and_then(|a| serde_json::to_string(&a).map_err(|e| e.to_string())),
            )
        }
        "/__tare/cost_regressions" => {
            // Outcome-aware unit-cost regressions. Query wins, then [anomaly] config, then
            // the default — matching the desktop command.
            let acfg = match load_config_strict() {
                Ok(cfg) => cfg.anomaly,
                Err(error) => return err(error),
            };
            let window = match q.get("window") {
                Some(raw) => match raw.parse::<usize>() {
                    Ok(value) if value > 0 => value,
                    _ => return bad(format!("invalid regression window {raw:?}; expected 1+")),
                },
                None => acfg.window.unwrap_or(7),
            };
            let threshold = match q.get("threshold") {
                Some(raw) => match raw.parse::<i64>() {
                    Ok(value) if value >= 0 => value,
                    _ => {
                        return bad(format!(
                            "invalid regression threshold {raw:?}; expected a non-negative integer"
                        ))
                    }
                },
                None => acfg.threshold.unwrap_or(50),
            };
            if window == 0 || threshold < 0 {
                return err("invalid anomaly window/threshold in tare.toml".into());
            }
            to_json(
                cost_regressions_for(
                    db,
                    q.get("from").map(String::as_str).unwrap_or("0001-01-01"),
                    q.get("to").map(String::as_str).unwrap_or("9999-12-31"),
                    window,
                    threshold,
                )
                .and_then(|r| serde_json::to_string(&r).map_err(|e| e.to_string())),
            )
        }
        "/__tare/explain" => {
            let Some(run_id) = q.get("run") else {
                return bad("explain requires `run`".into());
            };
            // Wrap the plain-text narrative in a JSON string so the read API stays JSON.
            to_json(explain_for(db, run_id, pricing).and_then(|s| {
                serde_json::to_string(&serde_json::json!({"explain": s})).map_err(|e| e.to_string())
            }))
        }
        "/__tare/rollup" => {
            let dim = match q.get("by") {
                Some(raw) => match tare_core::rollup::RollupDim::parse(raw) {
                    Some(dimension) => dimension,
                    None => return bad(format!("invalid rollup dimension {raw:?}")),
                },
                None => tare_core::rollup::RollupDim::Step,
            };
            // Optional progressive-drill filter: `&filter_by=<dim>&filter=<label>`
            // restricts to the steps under that parent bucket. Both must be present + valid.
            let filter = match (q.get("filter_by"), q.get("filter")) {
                (Some(raw), Some(label)) => match tare_core::rollup::RollupDim::parse(raw) {
                    Some(dimension) if !label.is_empty() => Some((dimension, label.clone())),
                    Some(_) => return bad("rollup filter must not be empty".into()),
                    None => return bad(format!("invalid rollup filter dimension {raw:?}")),
                },
                (None, None) => None,
                _ => return bad("rollup filter_by and filter must be supplied together".into()),
            };
            to_json(
                rollup_filtered_for(db, dim, filter, pricing)
                    .and_then(|r| serde_json::to_string(&r).map_err(|e| e.to_string())),
            )
        }
        "/__tare/sessions" => to_json(
            sessions_for(db, pricing)
                .and_then(|r| serde_json::to_string(&r).map_err(|e| e.to_string())),
        ),
        "/__tare/correlate" => to_json(
            correlate_for(db, pricing)
                .and_then(|r| serde_json::to_string(&r).map_err(|e| e.to_string())),
        ),
        // `?name=<lineage>` → one lineage's version rows; no name → every lineage.
        "/__tare/lineage" => to_json(match q.get("name") {
            Some(name) => lineage_for(db, pricing, name)
                .and_then(|r| serde_json::to_string(&r).map_err(|e| e.to_string())),
            None => lineages_all(db, pricing)
                .and_then(|r| serde_json::to_string(&r).map_err(|e| e.to_string())),
        }),
        // captured runs bucketed into the configured units of work, cost per unit.
        "/__tare/units" => to_json(
            units_for(db, pricing)
                .and_then(|r| serde_json::to_string(&r).map_err(|e| e.to_string())),
        ),
        // one step's REDACTED transcript (only present under max_inspect). `null` when not
        // captured. Requires `run` + `step` query params.
        "/__tare/transcript" => match (
            q.get("run"),
            q.get("step").and_then(|s| s.parse::<u32>().ok()),
        ) {
            (Some(run), Some(step)) => to_json(transcript_for(db, run, step).and_then(|opt| {
                let body = opt.map(|(req, resp, truncated)| {
                    serde_json::json!({ "req": req, "resp": resp, "truncated": truncated })
                });
                serde_json::to_string(&body).map_err(|e| e.to_string())
            })),
            _ => bad("transcript: valid `run` and `step` query params are required".to_string()),
        },
        "/__tare/loops" => to_json(
            loop_waste_for(db, pricing)
                .and_then(|r| serde_json::to_string(&r).map_err(|e| e.to_string())),
        ),
        "/__tare/failures" => to_json(
            failure_waste_for(db, pricing)
                .and_then(|r| serde_json::to_string(&r).map_err(|e| e.to_string())),
        ),
        "/__tare/lenses" => to_json(
            lenses_for(db, pricing)
                .and_then(|r| serde_json::to_string(&r).map_err(|e| e.to_string())),
        ),
        "/__tare/budget" => to_json(
            period_budget_for(db, pricing)
                .and_then(|r| serde_json::to_string(&r).map_err(|e| e.to_string())),
        ),
        "/__tare/sandwich" => {
            let component = q.get("component").map(|s| s.as_str()).unwrap_or("system");
            to_json(
                sandwich_for(db, pricing, component)
                    .and_then(|r| serde_json::to_string(&r).map_err(|e| e.to_string())),
            )
        }
        "/__tare/diff" => {
            let (Some(a), Some(b)) = (q.get("a"), q.get("b")) else {
                return bad("diff requires `a` and `b`".into());
            };
            to_json(
                diff_runs(db, a, b, pricing)
                    .and_then(|d| serde_json::to_string(&d).map_err(|e| e.to_string())),
            )
        }
        // Hierarchical node-level flame diff over an EXPLICIT run pair — a
        // distinct contract from the row-level report `/__tare/diff` above, which is unchanged. Both
        // `a` and `b` are required, so an accidental whole-store diff is impossible.
        // `normalize`|`normalized`=true|1 selects share-mode
        // structural diffing. Both spellings are accepted: the documented route uses `normalize`,
        // while the shipped client sends `normalized`.
        "/__tare/flame_diff" => {
            let (Some(a), Some(b)) = (q.get("a"), q.get("b")) else {
                return bad("flame_diff requires `a` and `b`".into());
            };
            let normalized = match q.get("normalized").or_else(|| q.get("normalize")) {
                Some(value) if matches!(value.as_str(), "true" | "1") => true,
                Some(value) if matches!(value.as_str(), "false" | "0") => false,
                Some(value) => return bad(format!("invalid normalize value {value:?}")),
                None => false,
            };
            to_json(
                flame_diff_for(db, pricing, a, b, normalized)
                    .and_then(|d| serde_json::to_string(&d).map_err(|e| e.to_string())),
            )
        }
        // Saved investigations list: the cross-transport source of truth,
        // newest-updated first. Browser + desktop both read this; localStorage is only a fallback.
        "/__tare/investigations" => to_json(
            Store::open(db)
                .and_then(|s| s.list_investigations())
                .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        ),
        "/__tare/config" => to_json(
            load_config_strict()
                .and_then(|cfg| serde_json::to_string(&cfg).map_err(|e| e.to_string())),
        ),
        "/__tare/export" => {
            let Some(run) = q.get("run") else {
                return bad("export requires `run`".into());
            };
            let fmt = q.get("format").map(|s| s.as_str()).unwrap_or("speedscope");
            to_json(export_content(db, run, fmt, pricing))
        }
        "/__tare/pricing" => to_json(
            // Clock-free: version/effective-date/note only (the CLI `tare pricing` adds age).
            // `models_by_provider` lets setup say "N models for <provider>".
            {
                let mut counts: std::collections::BTreeMap<String, u32> =
                    std::collections::BTreeMap::new();
                let mut seen = std::collections::BTreeSet::new();
                let mut models = Vec::new();
                for m in &pricing.models {
                    let count = counts.entry(m.provider.clone()).or_insert(0);
                    *count = count.saturating_add(1);
                    // Deduped per-model rate rows for the Models/Pricing catalog.
                    if seen.insert((m.provider.clone(), m.model_id.clone())) {
                        models.push(m);
                    }
                }
                serde_json::to_string(&serde_json::json!({
                    "version": pricing.version,
                    "effective_date": pricing.effective_date,
                    "note": pricing.note,
                    "models_by_provider": counts,
                    "models": models,
                }))
                .map_err(|e| e.to_string())
            },
        ),
        "/__tare/receipt" => {
            let Some(run) = q.get("run") else {
                return bad("receipt requires `run`".into());
            };
            let max_private = match q.get("profile").map(String::as_str) {
                Some("max_private") => true,
                Some("strict_counts") | None => false,
                Some(other) => return bad(format!("invalid receipt profile {other:?}")),
            };
            to_json(
                attest_receipt(db, Some(run), max_private, pricing).and_then(|json| {
                    let receipt: serde_json::Value =
                        serde_json::from_str(&json).map_err(|e| e.to_string())?;
                    // Verify in-process so the UI shows the recomputation result (not a crypto seal).
                    let verify =
                        tare_core::receipt::verify(&json, pricing).map_err(|e| e.to_string())?;
                    serde_json::to_string(
                        &serde_json::json!({ "receipt": receipt, "verify": verify }),
                    )
                    .map_err(|e| e.to_string())
                }),
            )
        }
        "/__tare/trend" => {
            let dim = match q.get("by") {
                Some(raw) => match tare_core::trend::TrendDimension::parse(raw) {
                    Some(dimension) => dimension,
                    None => return bad(format!("invalid trend dimension {raw:?}")),
                },
                None => tare_core::trend::TrendDimension::Total,
            };
            match trend_for(
                db,
                q.get("from").map(String::as_str),
                q.get("to").map(String::as_str),
                dim,
                pricing,
            ) {
                Ok(Some(rep)) => to_json(serde_json::to_string(&rep).map_err(|e| e.to_string())),
                Ok(None) => {
                    // Empty store: a well-formed empty report.
                    let empty = tare_core::trend::TrendReport {
                        dimension: dim.as_str().to_string(),
                        from: String::new(),
                        to: String::new(),
                        days: Vec::new(),
                        series: Vec::new(),
                        pricing_version: pricing.version.clone(),
                        estimated: true,
                    };
                    to_json(serde_json::to_string(&empty).map_err(|e| e.to_string()))
                }
                Err(e) => err(e),
            }
        }
        // Anything else in the reserved namespace is a UI asset request: serve the embedded,
        // self-contained dashboard shell (`/__tare/` -> index.html, plus its JS/CSS modules).
        other => {
            let rel = other.strip_prefix("/__tare/").or_else(|| {
                if other == "/__tare" {
                    Some("")
                } else {
                    None
                }
            })?;
            ui_assets::ui_asset(rel)
                .map(|(ctype, body)| (200u16, ctype.to_string(), body.to_string()))
        }
    }
}

/// JSON-encode a string (for hand-built error bodies).
fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

// ---- trend / history over a calendar window ----

/// Resolve `[from, to]` for a trend: explicit flags win; otherwise default to the last 14 days
/// ending today, clamped to the data's actual date bounds. Returns `None` if the DB is empty.
pub fn resolve_trend_window(
    store: &Store,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<Option<(String, String)>, String> {
    store.resolve_trend_window(from, to)
}

pub fn trend_for(
    db_path: &str,
    from: Option<&str>,
    to: Option<&str>,
    dim: tare_core::trend::TrendDimension,
    pricing: &PricingTable,
) -> Result<Option<tare_core::trend::TrendReport>, String> {
    let store = Store::open(db_path)?;
    let Some((from, to)) = resolve_trend_window(&store, from, to)? else {
        return Ok(None);
    };
    Ok(Some(store.trend_in_range(&from, &to, pricing, dim)?))
}

/// Whole-store anomaly explanation: thin wrapper over the shared `Store::anomaly_why` orchestrator
/// with no scope. `dim` is a `TrendDimension` for the existing CLI `why` command's callers.
pub fn anomaly_why_for(
    db_path: &str,
    pricing: &PricingTable,
    from: Option<&str>,
    to: Option<&str>,
    dim: tare_core::trend::TrendDimension,
    window: usize,
    threshold: i64,
) -> Result<Vec<tare_core::anomaly::AnomalyWhy>, String> {
    use tare_core::anomaly::AnomalyDimension;
    use tare_core::trend::TrendDimension;
    let dimension = match dim {
        TrendDimension::Total => AnomalyDimension::Total,
        TrendDimension::ByProvider => AnomalyDimension::Provider,
        TrendDimension::ByModel => AnomalyDimension::Model,
        TrendDimension::ByCause => AnomalyDimension::Cause,
    };
    let req = tare_core::anomaly::AnomalyWhyRequest {
        from: from.map(str::to_string),
        to: to.map(str::to_string),
        dimension,
        window: Some(window),
        threshold: Some(threshold),
        scope: None,
    };
    Store::open(db_path)?.anomaly_why(&req, pricing)
}

/// Scoped anomaly explanation: delegates to the shared `Store::anomaly_why`,
/// which resolves `req.scope` to a run set BEFORE detection (never a post-filter of anomaly rows).
pub fn anomaly_why_scoped(
    db_path: &str,
    pricing: &PricingTable,
    req: &tare_core::anomaly::AnomalyWhyRequest,
) -> Result<Vec<tare_core::anomaly::AnomalyWhy>, String> {
    Store::open(db_path)?.anomaly_why(req, pricing)
}

/// Render the "what changed and why" decompositions.
pub fn render_anomaly_why_text(whys: &[tare_core::anomaly::AnomalyWhy]) -> String {
    if whys.is_empty() {
        return "no decomposable spend anomalies in this window\n".to_string();
    }
    let mut out = String::from("What changed and why — ESTIMATE (deterministic, on-device)\n\n");
    for w in whys {
        out.push_str(&format!("  {}\n", w.headline));
        // Single-cause attribution: name the cost-class that drove the majority, when
        // one did — else stay silent (a diffuse change has no honest single cause).
        if let Some(c) = &w.dominant_cause {
            out.push_str(&format!(
                "    ↳ mostly {} ({}% of the change)\n",
                c.label, c.share_pct
            ));
        }
        out.push_str(&format!("    → {}\n\n", w.bisect_hint));
    }
    out
}

pub fn render_trend_text(report: &tare_core::trend::TrendReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Tare trend — estimated (pricing {}) — {} to {} by {}\n\n",
        report.pricing_version, report.from, report.to, report.dimension
    ));
    out.push_str(&format!("{:<28} {:>15}\n", "SERIES", "TOTAL(est)"));
    for s in &report.series {
        out.push_str(&format!(
            "{:<28} {:>15}\n",
            s.key,
            MicroUsd(s.total_micros).to_dollar_string()
        ));
    }
    if report.series.is_empty() {
        out.push_str("(no spend recorded yet — run `tare run -- <cmd>`)\n");
    }
    out
}

// ---- diff (compare two `tare report --json` files) ----

pub fn load_report(path: &str) -> Result<tare_core::attribute::Report, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    serde_json::from_str(&s).map_err(|e| format!("parse report {path}: {e}"))
}

pub fn diff_files(before: &str, after: &str) -> Result<tare_core::diff::ReportDiff, String> {
    Ok(tare_core::diff::diff_reports(
        &load_report(before)?,
        &load_report(after)?,
    ))
}

/// Build the report for a single stored run.
fn report_of_run(db_path: &str, run_id: &str, pricing: &PricingTable) -> Result<Report, String> {
    Ok(attribute::build_report(
        &[load_run(db_path, run_id)?],
        pricing,
    ))
}

/// Build the report for a stored date window `[from, to]` (inclusive).
fn report_of_window(
    db_path: &str,
    from: &str,
    to: &str,
    pricing: &PricingTable,
) -> Result<Report, String> {
    let runs = Store::open(db_path)?.load_dated_runs_in_range(from, to)?;
    let runs: Vec<RunRecord> = runs.into_iter().map(|d| d.run).collect();
    Ok(attribute::build_report(&runs, pricing))
}

/// Diff two stored runs (K7a) — feeds `diff_reports` verbatim (zero core change).
pub fn diff_runs(
    db_path: &str,
    before: &str,
    after: &str,
    pricing: &PricingTable,
) -> Result<tare_core::diff::ReportDiff, String> {
    Ok(tare_core::diff::diff_reports(
        &report_of_run(db_path, before, pricing)?,
        &report_of_run(db_path, after, pricing)?,
    ))
}

/// Diff two stored date windows (K7a).
pub fn diff_windows(
    db_path: &str,
    from1: &str,
    to1: &str,
    from2: &str,
    to2: &str,
    pricing: &PricingTable,
) -> Result<tare_core::diff::ReportDiff, String> {
    Ok(tare_core::diff::diff_reports(
        &report_of_window(db_path, from1, to1, pricing)?,
        &report_of_window(db_path, from2, to2, pricing)?,
    ))
}

fn signed_dollars(micros: i64) -> String {
    let s = MicroUsd(micros.saturating_abs()).to_dollar_string();
    if micros < 0 {
        format!("-{s}")
    } else {
        format!("+{s}")
    }
}

pub fn render_diff_text(d: &tare_core::diff::ReportDiff) -> String {
    let pct = if d.total_before > 0 {
        tare_core::money::percent_of(d.delta_micros, d.total_before)
    } else {
        0
    };
    let mut out = String::new();
    out.push_str(&format!(
        "Tare diff — estimated (pricing {})\n",
        d.pricing_version
    ));
    out.push_str(&format!(
        "Total: {} -> {}  (Δ {}, {pct:+}%)\n\n",
        MicroUsd(d.total_before).to_dollar_string(),
        MicroUsd(d.total_after).to_dollar_string(),
        signed_dollars(d.delta_micros),
    ));
    out.push_str(&format!(
        "{:<24} {:>15} {:>15} {:>15}\n",
        "CAUSE", "BEFORE", "AFTER", "Δ"
    ));
    for row in &d.rows {
        out.push_str(&format!(
            "{:<24} {:>15} {:>15} {:>15}\n",
            row.cause,
            MicroUsd(row.micros_before).to_dollar_string(),
            MicroUsd(row.micros_after).to_dollar_string(),
            signed_dollars(row.delta_micros),
        ));
    }
    out
}

// ---- cost-gate (CI) ----

/// Returns (passed, summary). Fails when spend exceeds `max_spend_micros`, or — with a
/// baseline and `fail_on_regression` — when total spend regressed vs the baseline report.
/// Optional cost-&-shape regression assertions for `tare gate` (A3). All default to off so the
/// existing spend/regression gate is unchanged when none are set.
#[derive(Clone, Debug, Default)]
pub struct ShapeGateOpts {
    pub max_system_prompt_tokens: Option<u64>,
    pub max_tool_def_tokens: Option<u64>,
    /// Required cache-read share of cache traffic, in WHOLE PERCENT (integer at the boundary).
    pub require_cache_read_ratio_pct: Option<u64>,
    /// Fail if any run re-issued an identical request (request-hash based). Fails CLOSED when
    /// the hash is unavailable (e.g. `max_private`).
    pub no_retry_loops: bool,
    /// Fail if any component's tokens grew more than this percent vs the `--baseline-run`.
    pub max_component_growth_pct: Option<u64>,
}

impl ShapeGateOpts {
    pub fn any_set(&self) -> bool {
        self.max_system_prompt_tokens.is_some()
            || self.max_tool_def_tokens.is_some()
            || self.require_cache_read_ratio_pct.is_some()
            || self.no_retry_loops
            || self.max_component_growth_pct.is_some()
    }
}

/// GitHub Actions workflow that runs the Tare cost gate on every PR + posts the Infracost-style
/// comment to the step summary. Static template; `fetch-depth: 0` so `--baseline-ref`
/// can resolve the base branch's merge-base.
pub fn render_github_workflow() -> String {
    r#"name: Tare cost gate
on: [pull_request]
jobs:
  cost:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0  # full history so --baseline-ref can find the merge-base
      - name: Tare cost gate
        run: |
          tare gate --github-comment --baseline-ref "origin/${{ github.base_ref }}" --fail-on-regression \
            | tee -a "$GITHUB_STEP_SUMMARY"
"#
    .to_string()
}

/// GitLab CI job equivalent (runs on merge requests). Static template.
pub fn render_gitlab_ci() -> String {
    r#"tare-cost-gate:
  stage: test
  script:
    - tare gate --github-comment --baseline-ref "origin/$CI_MERGE_REQUEST_TARGET_BRANCH_NAME" --fail-on-regression
  rules:
    - if: $CI_PIPELINE_SOURCE == "merge_request_event"
"#
    .to_string()
}

/// A git `pre-push` hook that blocks a push introducing a cost regression vs the upstream base
/// (bypass with `git push --no-verify`). Static template.
pub fn render_prepush_hook() -> String {
    r#"#!/bin/sh
# Tare cost gate (pre-push) — blocks a push that regresses estimated AI spend vs the upstream base.
# Bypass once with:  git push --no-verify
if command -v tare >/dev/null 2>&1; then
  if ! tare gate --baseline-ref "origin/HEAD" --fail-on-regression; then
    echo "tare: cost regression detected — push blocked (use --no-verify to override)" >&2
    exit 1
  fi
fi
exit 0
"#
    .to_string()
}

/// Resolve a git ref to the short SHA of its merge-base with HEAD: the commit the PR
/// branched from, which is the honest cost baseline. Runs `git merge-base <ref> HEAD` at the edge;
/// returns the 12-character short SHA matching the stamped `commit` label, or `None` if the ref is
/// unknown / not a repo.
pub fn detect_merge_base(cwd: &std::path::Path, git_ref: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["merge-base", git_ref, "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let sha: String = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(sha.chars().take(12).collect())
    } else {
        None
    }
}

/// The stored cost (micro-USD) attributed to a given commit — the `rollup --by commit` bucket for
/// `commit`. `None` if no captured runs carry that commit label. Pure over the store.
pub fn baseline_from_commit(
    db_path: &str,
    pricing: &PricingTable,
    commit: &str,
) -> Result<Option<i64>, String> {
    let runs = cached_runs(db_path)?;
    let rep = tare_core::rollup::rollup(&runs, pricing, tare_core::rollup::RollupDim::Commit);
    Ok(rep
        .rows
        .iter()
        .find(|r| r.label == commit)
        .map(|r| r.micros))
}

/// Total estimated spend across all runs, and (optionally) a baseline run's total — the two numbers
/// the PR-comment renderer needs. Reuses the lenses total; no re-computation drift.
pub fn gate_totals(
    db_path: &str,
    pricing: &PricingTable,
    baseline_run: Option<&str>,
) -> Result<(i64, Option<i64>), String> {
    let total = tare_core::lenses::lenses(&cached_runs(db_path)?, pricing).total_micros;
    let baseline = match baseline_run {
        Some(r) => Some(tare_core::lenses::lenses(&[load_run(db_path, r)?], pricing).total_micros),
        None => None,
    };
    Ok((total, baseline))
}

/// Render an Infracost-style PR comment (GitHub-flavored markdown) for a cost gate:
/// pass/fail header + a This-PR / baseline / Δ table when a baseline is given. Pure + deterministic
/// so it's snapshot-testable and the CI step just pipes it to the PR. `$/%` deltas are integer-safe.
pub fn render_gate_pr_comment(
    passed: bool,
    total_micros: i64,
    baseline_micros: Option<i64>,
    pricing_version: &str,
) -> String {
    let mark = if passed {
        "✅ **Tare cost gate: passed**"
    } else {
        "❌ **Tare cost gate: FAILED**"
    };
    let mut out = format!("{mark}\n\n");
    match baseline_micros {
        Some(base) => {
            let delta = total_micros.saturating_sub(base);
            let pct = if base > 0 {
                tare_core::money::percent_of(delta, base)
            } else {
                0
            };
            let arrow = if delta > 0 {
                "🔺"
            } else if delta < 0 {
                "🔻"
            } else {
                "▪️"
            };
            out.push_str("| | Estimated cost |\n|---|---:|\n");
            out.push_str(&format!(
                "| This PR | {} |\n",
                MicroUsd(total_micros).to_dollar_string()
            ));
            out.push_str(&format!(
                "| Baseline | {} |\n",
                MicroUsd(base).to_dollar_string()
            ));
            out.push_str(&format!(
                "| Change | {arrow} {} ({}{}%) |\n",
                MicroUsd(delta.saturating_abs()).to_dollar_string(),
                if delta >= 0 { "+" } else { "-" },
                pct.saturating_abs()
            ));
        }
        None => {
            out.push_str(&format!(
                "Estimated cost: **{}**\n",
                MicroUsd(total_micros).to_dollar_string()
            ));
        }
    }
    out.push_str(&format!(
        "\n<sub>Estimate — recomputed offline from captured token counts (pricing {pricing_version}). Not a bill.</sub>\n"
    ));
    out
}

#[allow(clippy::too_many_arguments)] // CI gate knobs; grouping shape opts into a struct already.
pub fn gate(
    db_path: &str,
    max_spend_micros: Option<i64>,
    pricing: &PricingTable,
    baseline: Option<&str>,
    fail_on_regression: bool,
    max_unpriced_tokens: Option<u64>,
    baseline_run: Option<&str>,
    shape: &ShapeGateOpts,
) -> Result<(bool, String), String> {
    gate_with_baseline_total(
        db_path,
        max_spend_micros,
        pricing,
        baseline,
        fail_on_regression,
        max_unpriced_tokens,
        baseline_run,
        shape,
        None,
    )
}

/// Run the cost gate with an optional already-resolved baseline total. This is used for a git
/// baseline, whose stored commit rollup is a number rather than a saved report or run id.
#[allow(clippy::too_many_arguments)]
pub fn gate_with_baseline_total(
    db_path: &str,
    max_spend_micros: Option<i64>,
    pricing: &PricingTable,
    baseline: Option<&str>,
    fail_on_regression: bool,
    max_unpriced_tokens: Option<u64>,
    baseline_run: Option<&str>,
    shape: &ShapeGateOpts,
    resolved_baseline_micros: Option<i64>,
) -> Result<(bool, String), String> {
    let report = report_for(db_path, false, pricing)?;
    // Fail CLOSED on an empty store: a gate run against a DB with NO captured spend
    // (wrong/missing --db path, un-synced CI checkout) would otherwise pass every assertion at $0 —
    // turning a spend ceiling into a silent no-op. If any assertion was requested but there is
    // nothing to assert against, that is a setup error, not a green build.
    let asserts_requested = max_spend_micros.is_some()
        || max_unpriced_tokens.is_some()
        || shape.max_system_prompt_tokens.is_some()
        || shape.max_tool_def_tokens.is_some()
        || shape.require_cache_read_ratio_pct.is_some()
        || shape.max_component_growth_pct.is_some()
        || shape.no_retry_loops
        || fail_on_regression;
    let store_empty = report.rows.is_empty()
        && report.total_micros == 0
        && report.unpriced.iter().all(|u| u.token_total == 0);
    if asserts_requested && store_empty {
        return Ok((
            false,
            format!(
                "Tare gate — FAIL: no captured spend at {db_path} (empty/missing store). A gate \
                 with assertions must not pass against an empty store — check the --db path / that \
                 capture ran.\n"
            ),
        ));
    }
    let mut passed = true;
    let mut out = String::new();
    out.push_str(&format!(
        "Tare gate — total {} (estimated, pricing {})\n",
        MicroUsd(report.total_micros).to_dollar_string(),
        report.pricing_version
    ));
    // Unpriced tokens are EXCLUDED from total_micros, so a model rename can otherwise sneak
    // spend past a max-spend cap. Surface it; optionally fail the gate on it.
    let unpriced_tokens = report
        .unpriced
        .iter()
        .fold(0u64, |total, model| total.saturating_add(model.token_total));
    if unpriced_tokens > 0 {
        out.push_str(&format!(
            "  unpriced : {unpriced_tokens} tokens on {} model(s) (NOT in total)\n",
            report.unpriced.len()
        ));
    }
    if let Some(limit) = max_unpriced_tokens {
        let ok = unpriced_tokens <= limit;
        passed &= ok;
        out.push_str(&format!(
            "  max-unpriced-tokens {limit} : {}\n",
            if ok { "PASS" } else { "FAIL" }
        ));
    }
    if let Some(cap) = max_spend_micros {
        let ok = report.total_micros <= cap;
        passed &= ok;
        out.push_str(&format!(
            "  max-spend {} : {}\n",
            MicroUsd(cap).to_dollar_string(),
            if ok { "PASS" } else { "FAIL" }
        ));
    }
    // A baseline can be a resolved git commit total, a stored run, or a saved report file.
    let baseline_total = match (resolved_baseline_micros, baseline_run, baseline) {
        (Some(total), _, _) => Some(total),
        (None, Some(run), _) => Some(report_of_run(db_path, run, pricing)?.total_micros),
        (None, None, Some(path)) => Some(load_report(path)?.total_micros),
        (None, None, None) => None,
    };
    if let Some(before) = baseline_total {
        let delta = report.total_micros.saturating_sub(before);
        let regressed = delta > 0;
        out.push_str(&format!(
            "  vs baseline {} -> {} (Δ {})\n",
            MicroUsd(before).to_dollar_string(),
            MicroUsd(report.total_micros).to_dollar_string(),
            signed_dollars(delta),
        ));
        if fail_on_regression {
            passed &= !regressed;
            out.push_str(&format!(
                "  regression check : {}\n",
                if regressed { "FAIL" } else { "PASS" }
            ));
            // Localize the regression: bisect the daily Total series (over all stored runs) for
            // the first crossing, and name the driving cause — so a red gate says WHICH day +
            // WHY, not just "spend up" (A1). Best-effort: a flat/short series simply adds nothing.
            if regressed {
                if let Ok(Some(reg)) = bisect_for(db_path, None, 7, 50, pricing) {
                    let driver = report
                        .rows
                        .iter()
                        .max_by_key(|r| r.micros)
                        .map(|r| r.cause.as_str())
                        .unwrap_or("unknown");
                    out.push_str(&format!(
                        "  regression entered {} (+{}% vs trailing median {}), driven by {}\n",
                        reg.date,
                        reg.pct_over,
                        MicroUsd(reg.baseline_micros).to_dollar_string(),
                        driver,
                    ));
                }
            }
        }
    } else if fail_on_regression {
        passed = false;
        out.push_str(
            "  regression check : FAIL (requires --baseline, --baseline-run, or --baseline-ref)\n",
        );
    }
    // --- cost-&-shape regression assertions (A3) ---
    if shape.any_set() {
        use tare_core::shape_gate::{component_growth_violations, shape_stats};
        let runs = cached_runs(db_path)?;
        let stats = shape_stats(&runs, pricing);
        if let Some(cap) = shape.max_system_prompt_tokens {
            let v = stats.system_tokens();
            let ok = v <= cap;
            passed &= ok;
            out.push_str(&format!(
                "  max-system-prompt-tokens {cap} : {v} tok {}\n",
                if ok { "PASS" } else { "FAIL" }
            ));
        }
        if let Some(cap) = shape.max_tool_def_tokens {
            let v = stats.tool_def_tokens();
            let ok = v <= cap;
            passed &= ok;
            out.push_str(&format!(
                "  max-tool-def-tokens {cap} : {v} tok {}\n",
                if ok { "PASS" } else { "FAIL" }
            ));
        }
        if let Some(pct) = shape.require_cache_read_ratio_pct {
            // Whole percent at the boundary -> parts-per-1000 for the integer cross-multiply.
            let ok = stats.meets_cache_read_ratio(pct.saturating_mul(10));
            passed &= ok;
            out.push_str(&format!(
                "  require-cache-read-ratio {pct}% : read {} / write {} {}\n",
                stats.cache_read_tokens,
                stats.cache_write_tokens,
                if ok { "PASS" } else { "FAIL" }
            ));
        }
        if shape.no_retry_loops {
            // Fail CLOSED when the request hash is unavailable (e.g. max_private) — never a
            // silent pass.
            let (ok, note) = if !stats.retry_available {
                (false, "unavailable under this privacy profile")
            } else if stats.has_retry_loop {
                (false, "retry loop detected")
            } else {
                (true, "none")
            };
            passed &= ok;
            out.push_str(&format!(
                "  no-retry-loops : {note} {}\n",
                if ok { "PASS" } else { "FAIL" }
            ));
        }
        if let Some(pct) = shape.max_component_growth_pct {
            // Needs per-component baseline token vectors -> a stored run, not a report file.
            match baseline_run {
                None => {
                    passed = false;
                    out.push_str("  max-component-growth-pct : FAIL (requires --baseline-run)\n");
                }
                Some(run) => {
                    let base_stats = shape_stats(&[load_run(db_path, run)?], pricing);
                    let viol = component_growth_violations(&base_stats, &stats, pct);
                    let ok = viol.is_empty();
                    passed &= ok;
                    if ok {
                        out.push_str(&format!("  max-component-growth-pct {pct}% : PASS\n"));
                    } else {
                        for v in &viol {
                            out.push_str(&format!(
                                "  max-component-growth-pct {pct}% : FAIL {} {}→{} tok\n",
                                v.component.as_str(),
                                v.baseline_tokens,
                                v.current_tokens,
                            ));
                        }
                    }
                }
            }
        }
    }
    out.push_str(if passed { "GATE PASS\n" } else { "GATE FAIL\n" });
    Ok((passed, out))
}

fn load_run(db_path: &str, run_id: &str) -> Result<RunRecord, String> {
    Store::open(db_path)?
        .load_run(run_id)?
        .ok_or_else(|| format!("run `{run_id}` not found in {db_path}"))
}

pub fn export_speedscope(
    db_path: &str,
    run_id: &str,
    out: &str,
    pricing: &PricingTable,
) -> Result<(), String> {
    let run = load_run(db_path, run_id)?;
    let model = build_flamegraph(&run, pricing);
    let json = serde_json::to_string_pretty(&speedscope::export(&model, VERSION))
        .map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(out, json).map_err(|e| format!("write {out}: {e}"))
}

/// Export a run as OpenTelemetry OTLP/JSON (counts + cost only — never payload).
pub fn export_otel_for(
    db_path: &str,
    run_id: &str,
    out: &str,
    pricing: &PricingTable,
) -> Result<(), String> {
    let run = load_run(db_path, run_id)?;
    let doc = tare_core::otel::export_otlp_json(&run, pricing, VERSION);
    let json = serde_json::to_string_pretty(&doc).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(out, json).map_err(|e| format!("write {out}: {e}"))
}

/// Export a run as OpenTelemetry OTLP/JSON **metrics** (Sum/cost data points; counts only).
pub fn export_otel_metrics_for(
    db_path: &str,
    run_id: &str,
    out: &str,
    pricing: &PricingTable,
) -> Result<(), String> {
    let run = load_run(db_path, run_id)?;
    let doc = tare_core::otel::export_otlp_metrics_json(&run, pricing, VERSION);
    let json = serde_json::to_string_pretty(&doc).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(out, json).map_err(|e| format!("write {out}: {e}"))
}

/// Import OTLP/JSON GenAI spans into the store as degraded steps (counts, no component
/// attribution). Honors the active privacy policy. Returns the number of steps imported.
pub fn import_otlp(db_path: &str, path: &str) -> Result<usize, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
    let steps = tare_core::otel::ingest_otlp_json(&bytes)?;
    let policy = resolve_privacy()?;
    let recs = tare_core::otel::otel_steps_to_records(&steps, &policy);
    let store = Store::open(db_path)?;
    let date = today_local();
    let pid = policy.policy_id();
    let prof = policy.effective_profile().as_str().to_string();
    for r in &recs {
        store.record_step_with_policy(r, &date, Some(&pid), Some(&prof), Some("otel-span"))?;
    }
    Ok(recs.len())
}

pub fn render_svg_for(
    db_path: &str,
    run_id: &str,
    out: &str,
    pricing: &PricingTable,
) -> Result<(), String> {
    let run = load_run(db_path, run_id)?;
    let model = build_flamegraph(&run, pricing);
    std::fs::write(out, svg::render_svg(&model)).map_err(|e| format!("write {out}: {e}"))
}

// ---- Offline demo (embedded fixtures) for the README hero ----

fn demo_run() -> Result<RunRecord, String> {
    // The sample run now lives in core (tare-core::demo) so the CLI, desktop, and preview all seed
    // the identical run without duplicating the fixtures.
    tare_core::demo::demo_run()
}

pub fn demo_model(pricing: &PricingTable) -> Result<FlamegraphModel, String> {
    Ok(build_flamegraph(&demo_run()?, pricing))
}

pub fn demo_write_svg(out: &str, pricing: &PricingTable) -> Result<(), String> {
    let model = demo_model(pricing)?;
    std::fs::write(out, svg::render_svg(&model)).map_err(|e| format!("write {out}: {e}"))
}

pub fn demo_write_speedscope(out: &str, pricing: &PricingTable) -> Result<(), String> {
    let model = demo_model(pricing)?;
    let json = serde_json::to_string_pretty(&speedscope::export(&model, VERSION))
        .map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(out, json).map_err(|e| format!("write {out}: {e}"))
}

/// Seed the bundled demo run into a store so a first-run user sees a populated, real flamegraph +
/// trim-list in seconds instead of a blank Overview. The run id is `demo` and the source
/// is tagged `demo`, so the UI can flag it as sample data and the user can tell it from their own
/// captured spend. Idempotent (INSERT OR REPLACE on the run's steps). Returns the run id.
pub fn demo_seed_store(db_path: &str) -> Result<String, String> {
    let run = demo_run()?;
    let store = Store::open(db_path)?;
    // A fixed sample date keeps the seed deterministic and obviously not "today".
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

/// Hidden self-test child used by the `tare run` integration test. Reads the injected
/// env and issues `n` minimal Anthropic POSTs to the proxy over plain loopback HTTP,
/// with zero external deps (no SDK, no reqwest). Loopback only.
pub fn emit_selftest() -> Result<(), String> {
    let base = std::env::var("ANTHROPIC_BASE_URL").map_err(|_| "ANTHROPIC_BASE_URL unset")?;
    let n = match std::env::var("TARE_EMIT_N") {
        Ok(raw) => match raw.parse::<usize>() {
            Ok(value) if (1..=1_000).contains(&value) => value,
            _ => return Err(format!("TARE_EMIT_N must be from 1 to 1000, got {raw:?}")),
        },
        Err(std::env::VarError::NotPresent) => 2,
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err("TARE_EMIT_N is not valid Unicode".into())
        }
    };
    let body = br#"{"model":"claude-opus-4-8","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}"#;
    for _ in 0..n {
        post_loopback(&base, "/v1/messages", body)?;
    }
    Ok(())
}

fn post_loopback(base: &str, path: &str, body: &[u8]) -> Result<(), String> {
    let host_port = base
        .strip_prefix("http://")
        .ok_or("emit: base must be http://")?
        .trim_end_matches('/');
    let mut stream =
        std::net::TcpStream::connect(host_port).map_err(|e| format!("connect {host_port}: {e}"))?;
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host_port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    stream
        .write_all(body)
        .map_err(|e| format!("write body: {e}"))?;
    let mut response = Vec::new();
    // Read to EOF (Connection: close makes the server close the socket).
    stream
        .take(1024 * 1024)
        .read_to_end(&mut response)
        .map_err(|e| format!("read: {e}"))?;
    let head = response
        .split(|byte| *byte == b'\n')
        .next()
        .ok_or("empty HTTP response")?;
    let head = std::str::from_utf8(head).map_err(|e| format!("HTTP status is not UTF-8: {e}"))?;
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| format!("invalid HTTP status line {head:?}"))?;
    if !(200..300).contains(&status) {
        return Err(format!("emit request failed with HTTP {status}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tare_core::cohort::AnalysisResponse;

    #[test]
    fn statusline_parses_the_claude_code_schema() {
        let json = r#"{
            "session_id": "sess-abc",
            "model": {"id": "claude-opus-4-8", "display_name": "Opus"},
            "cost": {"total_cost_usd": 0.0234, "total_duration_ms": 45000},
            "context_window": {"used_percentage": 8, "context_window_size": 200000},
            "exceeds_200k_tokens": false
        }"#;
        let inp = parse_statusline_input(json);
        assert_eq!(inp.session_id.as_deref(), Some("sess-abc"));
        assert_eq!(inp.model_label.as_deref(), Some("Opus"));
        assert_eq!(inp.vendor_cost_usd, Some(0.0234));
        assert_eq!(inp.ctx_used_pct, Some(8.0));
        assert!(!inp.exceeds_200k);
    }

    #[test]
    fn statusline_parse_is_tolerant_of_partial_null_and_garbage() {
        // Unparseable → all defaults, never panics.
        assert_eq!(
            parse_statusline_input("not json"),
            StatusLineInput::default()
        );
        // display_name absent → falls back to model.id; context_window null → no ctx%.
        let inp = parse_statusline_input(
            r#"{"model":{"id":"gpt-x"},"context_window":null,"exceeds_200k_tokens":true}"#,
        );
        assert_eq!(inp.model_label.as_deref(), Some("gpt-x"));
        assert_eq!(inp.ctx_used_pct, None);
        assert!(inp.exceeds_200k);
        assert!(inp.vendor_cost_usd.is_none());
    }

    #[test]
    fn statusline_render_pairs_tare_estimate_with_vendor_cross_check() {
        let inp = StatusLineInput {
            session_id: Some("s".into()),
            model_label: Some("Opus".into()),
            vendor_cost_usd: Some(0.02),
            ctx_used_pct: Some(8.0),
            exceeds_200k: false,
        };
        let line = render_statusline(Some(20_000), &inp); // $0.02 tare vs $0.02 vendor — agree.
        assert!(line.contains("tare $0.02 est"));
        assert!(line.contains("CC $0.02"));
        assert!(line.contains("Opus"));
        assert!(line.contains("ctx 8%"));
        assert!(!line.contains('Δ'), "no divergence marker when they agree");
    }

    #[test]
    fn statusline_render_flags_material_divergence_and_missing_tare() {
        let inp = StatusLineInput {
            vendor_cost_usd: Some(1.00), // $1.00 vendor
            ..Default::default()
        };
        // Tare $0.50 vs vendor $1.00 → >15% divergence → Δ.
        assert!(render_statusline(Some(500_000), &inp).contains('Δ'));
        // No captured session → primary shows an em dash, vendor still shown, no Δ.
        let none = render_statusline(None, &inp);
        assert!(none.contains("tare —"));
        assert!(none.contains("CC $1.00"));
        assert!(!none.contains('Δ'));
    }

    #[test]
    fn statusline_over_200k_warns_even_without_a_percentage() {
        let inp = StatusLineInput {
            exceeds_200k: true,
            ..Default::default()
        };
        assert!(render_statusline(None, &inp).contains("ctx ⚠"));
    }

    #[test]
    fn doctor_capture_verdict_classifies_the_three_states() {
        // Receiver down -> bad, with a fix pointing at `tare up`.
        let down = capture_verdict(false, None);
        assert_eq!(down.status, DoctorStatus::Bad);
        assert!(down.fix.unwrap().contains("tare up"));
        // Up but silent -> warn (the demoralizing "is it working?" case).
        assert_eq!(capture_verdict(true, Some(0)).status, DoctorStatus::Warn);
        assert_eq!(capture_verdict(true, None).status, DoctorStatus::Warn);
        // Up + events -> ok.
        assert_eq!(capture_verdict(true, Some(7)).status, DoctorStatus::Ok);
    }

    #[test]
    fn open_url_argv_is_platform_appropriate() {
        let argv = open_url_argv("http://127.0.0.1:8788/__tare/");
        assert!(argv.last().unwrap().contains("__tare"));
        let opener = &argv[0];
        assert!(
            opener == "open" || opener == "xdg-open" || opener == "cmd",
            "a real browser opener: {opener}"
        );
    }

    #[test]
    fn file_points_at_detects_our_loopback_wiring() {
        let dir = std::env::temp_dir().join(format!("tare-detect-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("settings.json");
        let ps = p.to_string_lossy().to_string();
        // A Claude Code settings file wired to our receiver on :4318.
        std::fs::write(
            &p,
            r#"{"env":{"OTEL_EXPORTER_OTLP_ENDPOINT":"http://127.0.0.1:4318"}}"#,
        )
        .unwrap();
        assert!(file_points_at(&ps, 4318));
        assert!(!file_points_at(&ps, 4319), "different port -> not ours");
        // A foreign collector is not ours.
        std::fs::write(
            &p,
            r#"{"env":{"OTEL_EXPORTER_OTLP_ENDPOINT":"https://corp:4318"}}"#,
        )
        .unwrap();
        assert!(!file_points_at(&ps, 4318));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn doctor_run_reports_store_and_exits_clean_when_no_bad() {
        let db = std::env::temp_dir().join(format!("tare-doctor-{}.db", std::process::id()));
        let dbs = db.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        // Open once so the schema exists and the store is readable.
        let _ = Store::open(&dbs).unwrap();
        // Use an unbound port so the receiver check is deterministically "down" (a Bad) -> code 1,
        // but the STORE check must be Ok regardless.
        let (checks, _code) = doctor_run(&dbs, 1, 59_999, "2026-06-29");
        let store = checks.iter().find(|c| c.name == "store").unwrap();
        assert_eq!(
            store.status,
            DoctorStatus::Ok,
            "freshly opened store is readable"
        );
        assert!(checks.iter().any(|c| c.name == "pricing"));
        assert!(checks.iter().any(|c| c.name == "capture"));
        let _ = std::fs::remove_file(&db);
    }

    // the shared run cache serves repeats without a rescan, but a write MUST bust it —
    // a cost tool can never show stale numbers. Proves both halves: hit (same Arc, no reload) then
    // invalidation on the next call after a step lands.
    #[test]
    fn cached_runs_hits_then_invalidates_on_write() {
        use tare_core::model::{CacheTtl, Provider, RequestShape, StepRecord, UsageTokens};
        let db = std::env::temp_dir().join(format!("tare-runcache-{}.db", std::process::id()));
        let dbs = db.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(format!("{dbs}-wal"));
        Store::open(&dbs).unwrap(); // create the schema (empty)

        let r1 = cached_runs(&dbs).unwrap();
        assert_eq!(r1.len(), 0, "empty db -> no runs");
        let r2 = cached_runs(&dbs).unwrap();
        assert!(
            Arc::ptr_eq(&r1, &r2),
            "no write between reads -> cache hit (same Arc, no rescan)"
        );

        let step = StepRecord {
            run_id: "run-1".to_string(),
            step_ordinal: 0,
            provider: Provider::Anthropic,
            model: "claude-opus-4-8".to_string(),
            usage: UsageTokens {
                fresh_input: 100,
                output: 50,
                ..Default::default()
            },
            shape: RequestShape {
                model: "claude-opus-4-8".to_string(),
                provider: Provider::Anthropic,
                stream: false,
                ttl: CacheTtl::FiveMin,
                has_cache_control: false,
                cached_component: None,
                system_hash: None,
                weights: vec![],
                request_hash: None,
                step_label: None,
                component_label: None,
                parent_label: None,
                attempt: None,
                session: Some("run-1".to_string()),
                workload_key: None,
                effort: None,
                mcp_server: None,
                vendor: None,
                commit: None,
                author: None,
            },
            stop_reason: None,
            duration_ms: 0,
            start_unix_nano: None,
            trace_id: None,
            span_id: None,
            parent_span_id: None,
        };
        Store::open(&dbs)
            .unwrap()
            .record_step(&step, "2026-07-06")
            .unwrap();

        let r3 = cached_runs(&dbs).unwrap();
        assert!(
            !Arc::ptr_eq(&r2, &r3),
            "a write must invalidate the cache (fresh Arc)"
        );
        assert_eq!(r3.len(), 1, "reload reflects the newly written run");
        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(format!("{dbs}-wal"));
    }

    // both agents' real telemetry shapes flow receiver-parser -> writer -> store ->
    // sessions, and beat the durable live-activity table. Hermetic (loopback only, no network /
    // corp creds), so it runs in the offline gate as a capture regression guard. Real CLI smoke
    // checks (`claude -p` and `codex exec`) remain manual.
    #[test]
    fn dual_agent_telemetry_is_captured_through_the_store() {
        let cc = br#"{"resourceLogs":[{"scopeLogs":[{"logRecords":[
          {"attributes":[
            {"key":"event.name","value":{"stringValue":"api_request"}},
            {"key":"session.id","value":{"stringValue":"cc-sess"}},
            {"key":"model","value":{"stringValue":"claude-opus-4-8"}},
            {"key":"input_tokens","value":{"intValue":1200}},
            {"key":"output_tokens","value":{"intValue":80}},
            {"key":"event.sequence","value":{"intValue":1}}]}
        ]}]}]}"#;
        let codex = br#"{"resourceLogs":[{"scopeLogs":[{"logRecords":[
          {"attributes":[
            {"key":"event.name","value":{"stringValue":"codex.sse_event"}},
            {"key":"event.kind","value":{"stringValue":"response.completed"}},
            {"key":"gen_ai.request.model","value":{"stringValue":"gpt-5.5"}},
            {"key":"conversation.id","value":{"stringValue":"cx-conv"}},
            {"key":"input_token_count","value":{"intValue":900}},
            {"key":"cached_token_count","value":{"intValue":100}},
            {"key":"output_token_count","value":{"intValue":50}}]}
        ]}]}]}"#;
        let policy = tare_core::PrivacyPolicy::default();
        let cc_recs = tare_core::otel::ingest_otlp_logs_json(cc, &policy).unwrap();
        let cx_recs = tare_core::otel::ingest_otlp_logs_json(codex, &policy).unwrap();
        assert_eq!(cc_recs.len(), 1);
        assert_eq!(cx_recs.len(), 1);

        let db = std::env::temp_dir().join(format!("tare-dual-agent-{}.db", std::process::id()));
        let dbs = db.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&dbs);
        {
            let store = Store::open(&dbs).unwrap();
            let (tx, writer) = spawn_step_writer_with_transcript(store, policy, None).unwrap();
            for r in cc_recs.into_iter().chain(cx_recs) {
                tx.send(WriteMsg::Step(Box::new(r), "otel-event")).unwrap();
            }
            drop(tx); // last sender -> writer drains and exits
            join_step_writer(writer).unwrap();
        }

        // Both sessions landed, with the right providers, and the durable live table beat both.
        let store = Store::open(&dbs).unwrap();
        let runs = store.load_runs().unwrap();
        let providers: std::collections::BTreeSet<_> = runs
            .iter()
            .flat_map(|r| r.steps.iter().map(|s| s.provider))
            .collect();
        assert!(
            providers.contains(&Provider::Anthropic),
            "Claude Code captured"
        );
        assert!(providers.contains(&Provider::Openai), "Codex captured");
        let acts = store.load_session_activity().unwrap();
        assert_eq!(acts.len(), 2, "both sessions beat the durable live table");
        let _ = std::fs::remove_file(&dbs);
    }

    #[test]
    fn transcript_sink_routes_through_writer_to_separate_store() {
        // under max_inspect the writer owns a TranscriptStore in a SEPARATE sqlite file,
        // and transcript_sink() queues already-redacted bodies to it. Verify the round-trip and that
        // the transcript lands in tare.transcripts.db, NOT the counts db.
        let db =
            std::env::temp_dir().join(format!("tare-transcript-wire-{}.db", std::process::id()));
        let dbs = db.to_string_lossy().to_string();
        let tpath = transcript_db_path(&dbs);
        let _ = std::fs::remove_file(&dbs);
        let _ = std::fs::remove_file(&tpath);
        {
            let store = Store::open(&dbs).unwrap();
            let policy = tare_core::PrivacyPolicy::max_inspect();
            let (tx, writer) =
                spawn_step_writer_with_transcript(store, policy, Some(tpath.clone())).unwrap();
            let tsink = transcript_sink(&tx);
            // Bodies arrive already scrubbed from the proxy edge (simulated here).
            tsink(
                "run-xyz".into(),
                3,
                "{\"model\":\"m\",\"messages\":[…]}".into(),
                "{\"usage\":{\"output_tokens\":42}}".into(),
                true,
            );
            drop(tsink);
            drop(tx); // last sender -> writer drains and exits
            join_step_writer(writer).unwrap();
        }
        // The transcript is retrievable from the separate store, keyed by (run, step).
        let ts = TranscriptStore::open(&tpath).unwrap();
        let got = ts.get_by_step("run-xyz", 3).unwrap();
        assert_eq!(
            got,
            Some((
                "{\"model\":\"m\",\"messages\":[…]}".into(),
                "{\"usage\":{\"output_tokens\":42}}".into(),
                Some(true)
            ))
        );
        // The counts db has no transcript table — isolation holds.
        let counts = Store::open(&dbs).unwrap();
        assert!(counts.load_runs().unwrap().is_empty());
        let _ = std::fs::remove_file(&dbs);
        let _ = std::fs::remove_file(&tpath);
    }

    #[test]
    fn civil_date_for_buckets_into_the_offset_local_day() {
        // 2026-06-01 23:30 UTC. With a -8h (US Pacific) offset it's still 2026-06-01 locally...
        let utc_2330 = 1_780_355_400; // 2026-06-01T23:30:00Z
        assert_eq!(civil_date_for(utc_2330, 0), "2026-06-01");
        assert_eq!(civil_date_for(utc_2330, -8 * 60), "2026-06-01");
        // ...but at 2026-06-02 02:00 UTC, Pacific is still 2026-06-01 (the midnight bug).
        let utc_0200 = utc_2330 + 2 * 3600 + 30 * 60; // 2026-06-02T02:00:00Z
        assert_eq!(civil_date_for(utc_0200, 0), "2026-06-02");
        assert_eq!(civil_date_for(utc_0200, -8 * 60), "2026-06-01");
        // East-of-UTC (+9h Tokyo) rolls forward.
        assert_eq!(civil_date_for(utc_2330, 9 * 60), "2026-06-02");
    }

    #[test]
    fn budget_resolves_from_config_with_env_override_and_rejects_bad_values() {
        use tare_core::config::TareConfig;
        let cfg = TareConfig::from_toml_str("[budget]\nmax_spend_usd = 0.50\n").unwrap();
        assert_eq!(
            resolve_budget_with(&cfg, |_| Ok(None))
                .unwrap()
                .unwrap()
                .max_micros,
            Some(500_000)
        );
        assert_eq!(
            resolve_budget_with(&cfg, |name| {
                Ok((name == "TARE_MAX_SPEND_USD").then(|| "2".to_string()))
            })
            .unwrap()
            .unwrap()
            .max_micros,
            Some(2_000_000)
        );
        assert!(resolve_budget_with(&TareConfig::default(), |_| Ok(None))
            .unwrap()
            .is_none());

        for invalid in ["", "nope", "0", "-1", "NaN", "inf", "1e100"] {
            assert!(resolve_budget_with(&TareConfig::default(), |name| {
                Ok((name == "TARE_MAX_SPEND_USD").then(|| invalid.to_string()))
            })
            .is_err());
        }
        for (variable, invalid) in [
            ("TARE_MAX_STEPS", "-1"),
            ("TARE_MAX_STEPS", "1.5"),
            ("TARE_MAX_REPEATS", "many"),
        ] {
            assert!(resolve_budget_with(&TareConfig::default(), |name| {
                Ok((name == variable).then(|| invalid.to_string()))
            })
            .is_err());
        }
        assert!(resolve_budget_with(&TareConfig::default(), |name| {
            if name == "TARE_MAX_SPEND_USD" {
                Err("TARE_MAX_SPEND_USD is not valid Unicode".to_string())
            } else {
                Ok(None)
            }
        })
        .is_err());
    }

    #[test]
    fn privacy_and_upstream_configuration_fail_closed() {
        use tare_core::config::TareConfig;

        assert!(resolve_privacy_from(Some("unknown"), None, None).is_err());
        assert!(resolve_privacy_from(None, None, Some("[privacy]\nprofile = 7\n")).is_err());
        assert!(
            resolve_privacy_from(None, None, Some("[privacy]\nprofil = 'max_private'\n")).is_err()
        );
        assert_eq!(
            resolve_privacy_from(
                Some("max_private"),
                None,
                Some("[privacy]\nprofile = 'strict_counts'\n")
            )
            .unwrap()
            .effective_profile(),
            tare_core::privacy::Profile::MaxPrivate
        );

        for invalid in [
            "not a URL",
            "ftp://example.com",
            "https://user:secret@example.com",
            "https://example.com?token=secret",
            "https://example.com/#fragment",
        ] {
            let mut cfg = TareConfig::default();
            cfg.providers.anthropic_upstream = Some(invalid.to_string());
            assert!(
                resolve_upstreams_with(&cfg, |_| Ok(None)).is_err(),
                "{invalid}"
            );
        }

        let mut cfg = TareConfig::default();
        cfg.providers.anthropic_upstream = Some("https://config.example/".to_string());
        let upstreams = resolve_upstreams_with(&cfg, |name| {
            Ok((name == "TARE_ANTHROPIC_UPSTREAM").then(|| "http://127.0.0.1:9999/".to_string()))
        })
        .unwrap();
        assert_eq!(
            upstreams.get(&Provider::Anthropic).map(String::as_str),
            Some("http://127.0.0.1:9999")
        );
    }

    #[test]
    fn civil_dates() {
        assert_eq!(tare_core::calendar::civil_from_days(0), (1970, 1, 1));
        // 2026-06-24 is 20628 days after epoch.
        assert_eq!(tare_core::calendar::civil_from_days(20628), (2026, 6, 24));
        // Round-trips a leap day.
        assert_eq!(tare_core::calendar::civil_from_days(19782), (2024, 2, 29));
    }

    #[test]
    fn child_env_is_loopback_with_dummy_keys() {
        let env = prepare_child_env("http://127.0.0.1:54321", "run-xyz");
        let map: std::collections::HashMap<_, _> = env.into_iter().collect();
        assert_eq!(map["ANTHROPIC_BASE_URL"], "http://127.0.0.1:54321");
        assert_eq!(map["OPENAI_BASE_URL"], "http://127.0.0.1:54321/v1");
        assert_eq!(map["OPENAI_API_BASE"], "http://127.0.0.1:54321/v1");
        assert!(!map["ANTHROPIC_API_KEY"].is_empty());
        assert!(!map["OPENAI_API_KEY"].is_empty());
        assert_eq!(map["TARE_RUN_ID"], "run-xyz");

        let inherited = child_env_overrides("http://127.0.0.1:54321", "run-xyz", |name| {
            matches!(name, "ANTHROPIC_API_KEY" | "GEMINI_API_KEY")
        });
        let inherited: std::collections::HashMap<_, _> = inherited.into_iter().collect();
        assert!(
            !inherited.contains_key("ANTHROPIC_API_KEY"),
            "a real Anthropic key remains inherited"
        );
        assert!(
            !inherited.contains_key("GEMINI_API_KEY"),
            "a real Gemini key remains inherited"
        );
        assert_eq!(inherited["OPENAI_API_KEY"], "tare-dummy-key");
        assert_eq!(inherited["OPENAI_BASE_URL"], "http://127.0.0.1:54321/v1");
    }

    #[test]
    fn report_bundle_attests_and_scans_payload_free() {
        let tmp = std::env::temp_dir().join(format!("tare-bundle-{}.db", std::process::id()));
        let db = tmp.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        {
            let store = Store::open(&db).unwrap();
            let step = tare_core::ingest_step(
                "r1",
                1,
                tare_core::model::Provider::Anthropic,
                include_bytes!("../../fixtures/bloated_system_prompt/step1.request.json"),
                include_bytes!("../../fixtures/bloated_system_prompt/step1.response.json"),
            )
            .unwrap();
            store.record_step(&step, "2026-06-25").unwrap();
            let unpriced = tare_core::ingest_step(
                "r1",
                2,
                tare_core::model::Provider::Anthropic,
                br#"{"model":"private-future-model","messages":[]}"#,
                br#"{"usage":{"input_tokens":100,"output_tokens":5},"stop_reason":"end_turn"}"#,
            )
            .unwrap();
            store.record_step(&unpriced, "2026-06-25").unwrap();
        }
        let p = load_pricing(None).unwrap();

        // A run bundle includes a flamegraph, manifest, savings ledger, and generated experiment.
        let full = report_bundle(&db, Some("r1"), false, &p).unwrap();
        assert!(full.contains("\"flamegraph_svg\""));
        assert!(full.contains("\"manifest\"") && full.contains("\"profile\""));
        assert!(full.contains("\"savings\"") && full.contains("\"savings_index\""));
        // The cost experiment is populated from cheaper priced candidates.
        assert!(full.contains("\"experiment\"") && full.contains("\"best_saving_micros\""));
        let _: serde_json::Value = serde_json::from_str(&full).unwrap();
        assert!(
            full.contains("private-future-model"),
            "the ordinary bundle intentionally names unpriced models"
        );

        // Estimate-confidence is counts-only, so it's present in BOTH profiles. Coverage
        // is an honest STATUS (unknown out-of-band) — no fabricated numeric share.
        assert!(full.contains("\"confidence\"") && full.contains("\"coverage_status\""));
        // Bundle JSON is pretty-printed (spaced). Coverage is the honest status, no fabricated %.
        assert!(full.contains("\"coverage_status\": \"unknown\""));
        assert!(!full.contains("\"coverage_share_pct\"")); // omitted with no denominator

        // max_private bundle is counts-only: NO flamegraph and NO savings ledger (both carry labels).
        let private = report_bundle(&db, Some("r1"), true, &p).unwrap();
        assert!(!private.contains("\"flamegraph_svg\""));
        assert!(!private.contains("\"savings\""));
        assert!(!private.contains("\"experiment\"")); // cell labels name models -> omitted (6xj.7)
        assert!(private.contains("\"confidence\"")); // counts-only -> still included
        assert!(private.contains("max_private"));
        assert!(private.contains("\"scope\": \"single-run\""));
        assert!(private.contains("\"provider\": \"withheld\""));
        assert!(!private.contains("private-future-model"));
        assert!(!private.contains("\"scope\": \"run:r1\""));
        // The scanner rejects a max_private bundle that smuggles a savings ledger or experiment.
        assert!(scan_bundle(r#"{"manifest":{},"report":{},"savings":{}}"#, true).is_err());
        assert!(scan_bundle(r#"{"manifest":{},"report":{},"experiment":{}}"#, true).is_err());

        // The fail-closed scanner rejects a bundle with an unexpected top-level key.
        assert!(scan_bundle(r#"{"manifest":{},"report":{},"leak":"x"}"#, false).is_err());

        // Nested fields are rejected too; a typed parse alone would silently discard these.
        let mut hostile: serde_json::Value = serde_json::from_str(&private).unwrap();
        hostile["report"]["payload"] = serde_json::json!("must not survive");
        let error = scan_bundle(&hostile.to_string(), true).unwrap_err();
        assert!(error.contains("closed typed shape"), "got: {error}");

        // Profile claims, privacy flags, and redaction markers must agree with the bytes.
        let mut inconsistent: serde_json::Value = serde_json::from_str(&private).unwrap();
        inconsistent["manifest"]["profile"] = serde_json::json!("strict_counts");
        assert!(scan_bundle(&inconsistent.to_string(), true)
            .unwrap_err()
            .contains("disagrees"));
        let mut identity_leak: serde_json::Value = serde_json::from_str(&private).unwrap();
        identity_leak["report"]["unpriced"][0]["model"] = serde_json::json!("private-future-model");
        assert!(scan_bundle(&identity_leak.to_string(), true)
            .unwrap_err()
            .contains("discloses"));
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn pricing_status_reports_version_age_and_unpriced() {
        let tmp = std::env::temp_dir().join(format!("tare-pricing-{}.db", std::process::id()));
        let db = tmp.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        {
            let store = Store::open(&db).unwrap();
            let req = br#"{"model":"claude-future-99","messages":[]}"#;
            let resp =
                br#"{"usage":{"input_tokens":100,"output_tokens":5},"stop_reason":"end_turn"}"#;
            let step =
                tare_core::ingest_step("r", 1, tare_core::model::Provider::Anthropic, req, resp)
                    .unwrap();
            store.record_step(&step, "2026-06-25").unwrap();
        }
        let p = load_pricing(None).unwrap();
        let s = pricing_status(&db, &p, "2026-12-01").unwrap();
        assert!(s.contains("pricing version :"));
        assert!(s.contains("age             :"));
        assert!(s.contains("over 90 days old")); // 2026-06-01 effective vs 2026-12-01
        assert!(s.contains("claude-future-99")); // unpriced surfaced
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn query_values_are_percent_decoded() {
        let q = parse_query("by=provider&run=run%20a%2Fb&from=2026-06-20").unwrap();
        assert_eq!(q["by"], "provider");
        assert_eq!(q["run"], "run a/b"); // %20 -> space, %2F -> '/'
        assert_eq!(q["from"], "2026-06-20");
        assert_eq!(parse_query("x=a+b").unwrap()["x"], "a b"); // '+' -> space
                                                               // A bare flag (no '=') is kept with an empty value.
        assert!(parse_query("today&by=cause").unwrap().contains_key("today"));
        // Malformed escapes, invalid UTF-8, and ambiguous duplicate keys fail explicitly.
        assert!(parse_query("x=100%é").is_err());
        assert!(parse_query("x=50%").is_err());
        assert!(parse_query("x=%zz").is_err());
        assert!(parse_query("x=%FF").is_err());
        assert!(parse_query("x=one&x=two").is_err());
    }

    #[test]
    fn punchcard_for_buckets_hour_stamped_runs_and_excludes_hourless() {
        use tare_core::ingest_step;
        use tare_core::model::Provider;
        let tmp = std::env::temp_dir().join(format!("tare-punch-{}.db", std::process::id()));
        let db = tmp.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        {
            let store = Store::open(&db).unwrap();
            let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
            let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");
            let a = ingest_step("run-a", 1, Provider::Anthropic, req, resp).unwrap();
            let b = ingest_step("run-b", 1, Provider::Anthropic, req, resp).unwrap();
            // A: hour stamped (JSONL lane). B: no hour (hourless lane) → must be excluded.
            store
                .record_step_with_time(&a, "2026-07-06", Some(9), None, None, Some("jsonl"))
                .unwrap();
            store
                .record_step_with_time(&b, "2026-07-06", None, None, None, Some("otel"))
                .unwrap();
        }
        let pricing = load_pricing(None).unwrap();
        let m = punchcard_for(&db, &pricing).unwrap();
        assert_eq!(m.cells.len(), 7 * 24, "always the full rectangle");
        let at_9: i64 = m
            .cells
            .iter()
            .filter(|c| c.hour == 9)
            .map(|c| c.micros)
            .sum();
        let elsewhere: i64 = m
            .cells
            .iter()
            .filter(|c| c.hour != 9)
            .map(|c| c.micros)
            .sum();
        assert!(at_9 > 0, "the hour-9 run is priced and bucketed");
        assert_eq!(
            elsewhere, 0,
            "the hourless run is excluded — no other bucket populated"
        );
        assert_eq!(m.total_micros, at_9);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn read_api_serves_trend_runs_and_unknown_404() {
        use tare_core::ingest_step;
        use tare_core::model::Provider;
        let tmp = std::env::temp_dir().join(format!("tare-readapi-{}.db", std::process::id()));
        let db = tmp.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&db);
        {
            let store = Store::open(&db).unwrap();
            let req = include_bytes!("../../fixtures/openai_nonstream/request.json");
            let resp = include_bytes!("../../fixtures/openai_nonstream/response.json");
            // A run id needing URL-encoding, to exercise the decode path end-to-end.
            let s = ingest_step("run a/b", 1, Provider::Openai, req, resp).unwrap();
            store.record_step(&s, "2026-06-24").unwrap();
        }
        let pricing = load_pricing(None).unwrap();

        let (status, ct, body) = read_api(&db, &pricing, "/__tare/runs").unwrap();
        assert_eq!(status, 200);
        assert_eq!(ct, "application/json");
        assert!(body.contains("run a/b"));

        let (_s, _c, body) = read_api(&db, &pricing, "/__tare/trend?by=provider").unwrap();
        assert!(body.contains("openai"));

        // Rollup over the store (this step is unlabeled -> the unlabeled bucket).
        let (_s, _c, body) = read_api(&db, &pricing, "/__tare/rollup?by=step").unwrap();
        assert!(body.contains("unlabeled"));

        // Progressive-drill filter: a filter that matches nothing yields an empty
        // rollup, proving the filter param is parsed + applied through the read-API.
        let (status, _c, body) = read_api(
            &db,
            &pricing,
            "/__tare/rollup?by=step&filter_by=step&filter=__none__",
        )
        .unwrap();
        assert_eq!(status, 200);
        let rep: tare_core::rollup::RollupReport = serde_json::from_str(&body).unwrap();
        assert!(
            rep.rows.is_empty(),
            "no step matches the bogus filter → empty rollup"
        );
        assert_eq!(rep.total_micros, 0);

        // Encoded run id resolves to the real run (decode asymmetry regression guard).
        let (_s, _c, body) = read_api(&db, &pricing, "/__tare/flamegraph?run=run%20a%2Fb").unwrap();
        assert!(body.contains("run a/b"));

        // Today and per-run status.
        let (_s, _c, body) = read_api(&db, &pricing, "/__tare/today").unwrap();
        assert!(body.contains("total_micros"));
        let (_s, _c, body) = read_api(&db, &pricing, "/__tare/run_status?run=run%20a%2Fb").unwrap();
        assert!(body.contains("\"steps\":1") && body.contains("run a/b"));
        let (status, ct, body) = read_api(&db, &pricing, "/__tare/run_status?run=missing").unwrap();
        assert_eq!(status, 404, "a deleted/stale run is not a server failure");
        assert_eq!(ct, "application/json");
        assert!(body.contains("run `missing` not found"));

        // Batch status: one array covering every run, same per-run shape as the single.
        let (status, _c, body) = read_api(&db, &pricing, "/__tare/run_statuses").unwrap();
        assert_eq!(status, 200);
        let rows: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
        assert_eq!(rows.len(), 1, "one run in the store → one status row");
        assert_eq!(rows[0]["run_id"], "run a/b");
        assert_eq!(rows[0]["steps"], 1);

        // The embedded UI shell is served from the reserved namespace with the right MIME.
        let (status, ct, body) = read_api(&db, &pricing, "/__tare/").unwrap();
        assert_eq!(status, 200);
        assert!(ct.starts_with("text/html"));
        assert!(body.contains("<div id=\"app\">"));
        let (_s, ct, body) = read_api(&db, &pricing, "/__tare/main.js").unwrap();
        assert!(ct.starts_with("text/javascript") && body.contains("mountApp"));
        let (_s, ct, _b) = read_api(&db, &pricing, "/__tare/ui/tokens.css").unwrap();
        assert!(ct.starts_with("text/css"));

        // Diff, pricing, and receipt. Diffing a run against itself is a zero delta.
        let (_s, _c, body) =
            read_api(&db, &pricing, "/__tare/diff?a=run%20a%2Fb&b=run%20a%2Fb").unwrap();
        assert!(body.contains("\"delta_micros\":0"));
        // Node-level flame diff: a distinct endpoint from /diff above. Self vs
        // self is a zero-delta tree; the model echoes the run pair + the normalized flag.
        let (fs, _c, body) = read_api(
            &db,
            &pricing,
            "/__tare/flame_diff?a=run%20a%2Fb&b=run%20a%2Fb&normalized=true",
        )
        .unwrap();
        assert_eq!(fs, 200);
        let fd: tare_core::flame_diff::FlameDiffModel = serde_json::from_str(&body).unwrap();
        assert_eq!(fd.run_a, "run a/b");
        assert_eq!(fd.run_b, "run a/b");
        assert!(fd.normalized);
        assert_eq!(fd.root.delta_micros, 0); // identical runs → no node-level change
                                             // Only explicit pairs: a missing operand is a client error, never a whole-store diff.
        let (status, _, _) = read_api(&db, &pricing, "/__tare/flame_diff?a=run%20a%2Fb").unwrap();
        assert_eq!(status, 400);
        // A missing run is a clear error (not a silent empty tree).
        assert!(read_api(&db, &pricing, "/__tare/flame_diff?a=run%20a%2Fb&b=nope").is_some());
        let (_s, _c, body) = read_api(&db, &pricing, "/__tare/pricing").unwrap();
        assert!(body.contains("version") && body.contains("effective_date"));
        // Config read: the four sections are present.
        let (_s, _c, body) = read_api(&db, &pricing, "/__tare/config").unwrap();
        assert!(body.contains("budget") && body.contains("privacy") && body.contains("proxy"));
        // Per-run export: speedscope and receipt content for a run.
        let (_s, _c, body) = read_api(
            &db,
            &pricing,
            "/__tare/export?run=run%20a%2Fb&format=speedscope",
        )
        .unwrap();
        assert!(body.contains("\"$schema\"") || body.contains("speedscope"));
        let (_s, _c, body) = read_api(
            &db,
            &pricing,
            "/__tare/export?run=run%20a%2Fb&format=receipt",
        )
        .unwrap();
        assert!(body.contains("tare-receipt"));
        let (_s, _c, body) = read_api(&db, &pricing, "/__tare/receipt?run=run%20a%2Fb").unwrap();
        // The verify result is embedded (recomputation receipt), with the scope echoed.
        assert!(body.contains("\"verify\"") && body.contains("recomputed_total_micros"));

        // Unknown asset / path -> None (proxy answers 404).
        assert!(read_api(&db, &pricing, "/__tare/nope").is_none());

        // vendor_today: empty by default (available=false); reflects recorded metered points.
        let (_s, _c, body) =
            read_api(&db, &pricing, "/__tare/vendor_today?day=2026-06-29").unwrap();
        assert!(body.contains("\"available\":false") && body.contains("\"cost_micros\":0"));
        {
            let s = Store::open(&db).unwrap();
            s.record_metered(&tare_core::otel::MeteredPoint {
                day: "2026-06-29".into(),
                metric: "cost".into(),
                model: "claude-opus-4-8".into(),
                kind: String::new(),
                session: "x".into(),
                effort: String::new(),
                query_source: String::new(),
                value: 250_000,
            })
            .unwrap();
        }
        let (_s, _c, body) =
            read_api(&db, &pricing, "/__tare/vendor_today?day=2026-06-29").unwrap();
        assert!(body.contains("\"cost_micros\":250000") && body.contains("\"available\":true"));

        // reconcile: the recorded vendor point joins per-model against the estimate.
        // No opus steps were captured on this day, so the vendor-only model is a coverage gap;
        // the estimate stays separate from the vendor figure (never merged).
        let (_s, _c, body) = read_api(&db, &pricing, "/__tare/reconcile?day=2026-06-29").unwrap();
        assert!(body.contains("claude-opus-4-8"));
        assert!(body.contains("\"vendor_micros\":250000"));
        assert!(body.contains("\"vendor_total_micros\":250000"));
        assert!(body.contains("\"has_vendor\":true"));
        assert!(body.contains("\"cause\":\"coverage_gap\""));
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn local_read_api_rejects_missing_and_invalid_parameters() {
        let db = std::env::temp_dir()
            .join(format!("tare-readapi-invalid-{}.db", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let _ = std::fs::remove_file(&db);
        let pricing = load_pricing(None).unwrap();
        let invalid = [
            "/__tare/report?today=1&today=2",
            "/__tare/report?today=%ZZ",
            "/__tare/report?=value",
            "/__tare/session_autopsy",
            "/__tare/session_autopsy?id=run&median=nope",
            "/__tare/run_note",
            "/__tare/notes_by_tag?tag=bad%20tag",
            "/__tare/heatmap?window=0",
            "/__tare/burnrate?range=quarter",
            "/__tare/vendor_today?day=2026-02-31",
            "/__tare/reconcile?day=not-a-date",
            "/__tare/run_meta",
            "/__tare/run_status",
            "/__tare/run_steps",
            "/__tare/recent_steps?n=51",
            "/__tare/flamegraph",
            "/__tare/profile?run=x&sort=unknown",
            "/__tare/profile?run=x&top=0",
            "/__tare/anomalies?by=unknown",
            "/__tare/anomalies?window=0",
            "/__tare/cost_regressions?threshold=-1",
            "/__tare/explain",
            "/__tare/rollup?by=unknown",
            "/__tare/rollup?filter_by=model",
            "/__tare/transcript?run=x&step=nope",
            "/__tare/diff?a=x",
            "/__tare/flame_diff?a=x&b=y&normalize=maybe",
            "/__tare/export",
            "/__tare/receipt?run=x&profile=unknown",
            "/__tare/trend?by=unknown",
        ];
        for request in invalid {
            let response = read_api(&db, &pricing, request)
                .unwrap_or_else(|| panic!("{request} unexpectedly fell through as not found"));
            assert_eq!(response.0, 400, "request={request}, body={}", response.2);
            assert_eq!(response.1, "application/json");
            assert!(response.2.contains("error"));
        }

        let invalid_writes: [(&str, &[u8]); 6] = [
            ("/__tare/notes", b"[]"),
            ("/__tare/notes", br#"{"run_id":""}"#),
            (
                "/__tare/notes",
                br#"{"run_id":"missing","tags":"not-an-array"}"#,
            ),
            ("/__tare/notes", br#"{"run_id":"missing","starred":"yes"}"#),
            ("/__tare/quality", br#"{"run_id":"missing","score":"high"}"#),
            (
                "/__tare/quality",
                br#"{"run_id":"missing","score":80,"source":"import"}"#,
            ),
        ];
        for (path, body) in invalid_writes {
            let response = write_api(&db, path, body);
            assert_eq!(response.0, 400, "path={path}, body={}", response.2);
        }
        assert!(!std::path::Path::new(&db).exists());
    }

    #[test]
    fn write_api_demo_seeds_the_sample_run() {
        let db = std::env::temp_dir()
            .join(format!("tare-demoseed-{}.db", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&db);
        // Empty store → seed via the loopback write API.
        let (status, _ct, _body) = write_api(&db, "/__tare/demo", b"{}");
        assert_eq!(status, 200);
        let store = Store::open(&db).unwrap();
        let runs = store.load_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, tare_core::demo::DEMO_RUN_ID);
        // Idempotent: seeding again doesn't duplicate the run.
        assert_eq!(write_api(&db, "/__tare/demo", b"{}").0, 200);
        assert_eq!(Store::open(&db).unwrap().load_runs().unwrap().len(), 1);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn read_api_serves_action_plan() {
        let db = std::env::temp_dir()
            .join(format!("tare-ap-{}.db", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&db);
        let pricing = load_pricing(None).unwrap();
        demo_seed_store(&db).unwrap();
        let (status, ct, body) = read_api(&db, &pricing, "/__tare/action_plan").unwrap();
        assert_eq!(status, 200);
        assert!(ct.contains("application/json"));
        let plan: tare_core::action_plan::ActionPlan = serde_json::from_str(&body).unwrap();
        // The demo run has spend, so there is at least one ranked, dollar-quantified item.
        assert!(plan.total_spend_micros > 0);
        assert!(!plan.items.is_empty());
        assert!(plan.items.iter().all(|i| i.dollars_micros > 0));
        // Recoverable floor never absorbs the at-risk exposure.
        assert!(plan.recoverable_floor_micros <= plan.total_spend_micros);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn read_api_serves_cache_ledger() {
        let db = std::env::temp_dir()
            .join(format!("tare-cl-{}.db", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&db);
        let pricing = load_pricing(None).unwrap();
        demo_seed_store(&db).unwrap(); // a real run set
        let (status, ct, body) = read_api(&db, &pricing, "/__tare/cache_ledger").unwrap();
        assert_eq!(status, 200);
        assert!(ct.contains("application/json"));
        // Valid ledger shape, non-negative realized savings (the demo run may or may not cache).
        let led: tare_core::cache_ledger::CacheLedger = serde_json::from_str(&body).unwrap();
        assert!(led.saved_micros >= 0);
        assert!(body.contains("\"cache_read_tokens\""));
        let _ = std::fs::remove_file(&db);
    }

    // ---- Calibrated Bench cohort HTTP transport ----

    /// Minimal valid CohortSpec wire form (snake_case, all fields explicit — no filters, UTC).
    fn cohort_spec_json() -> &'static str {
        "{\"from\":null,\"to\":null,\"timezone\":\"UTC\",\"entity\":\"run\",\"filters\":[],\
         \"pricing\":{\"mode\":\"effective_dated\"},\"metric\":\"spend_micros\",\
         \"normalization\":\"absolute\",\"outcome_denominator\":null}"
    }

    fn cohort_test_db(tag: &str) -> String {
        let db = std::env::temp_dir()
            .join(format!("tare-cohort-{tag}-{}.db", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&db);
        demo_seed_store(&db).unwrap();
        db
    }

    /// Assert the provenance envelope is honest and does not over-claim.
    fn assert_honest_provenance(prov: &tare_core::cohort::AnalysisProvenance) {
        use tare_core::cohort::{AllocationMethod, ComponentFidelity, ValueClass};
        use tare_core::confidence::CoverageStatus;
        assert_eq!(prov.coverage_status, CoverageStatus::Unknown); // no denominator out-of-band
        assert_eq!(prov.component_fidelity, ComponentFidelity::Coarse); // never over-claim detail
        assert_eq!(prov.allocation_method, AllocationMethod::ProviderCounts);
        assert_eq!(prov.value_class, ValueClass::Derived);
        assert_eq!(prov.pricing_edition.mode, "effective");
        assert!(prov.refreshed_at.ends_with('Z') && prov.refreshed_at.contains('T'));
        assert!(prov
            .assumptions
            .iter()
            .any(|a| a.starts_with("legacy_date_bucket")));
    }

    #[test]
    fn write_api_cohort_resolve_returns_envelope() {
        let db = cohort_test_db("resolve");
        let (status, ct, body) =
            write_api(&db, "/__tare/cohort/resolve", cohort_spec_json().as_bytes());
        assert_eq!(status, 200, "body={body}");
        assert!(ct.contains("application/json"));
        let resp: AnalysisResponse<tare_core::cohort::CohortResolveResult> =
            serde_json::from_str(&body).unwrap();
        assert!(resp.data.run_count >= 1); // the demo run resolves into the unfiltered cohort
        assert_eq!(resp.data.run_ids.len(), resp.data.run_count as usize);
        assert_honest_provenance(&resp.provenance);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn write_api_cohort_facets_returns_envelope() {
        let db = cohort_test_db("facets");
        let spec = cohort_spec_json();
        let body_req =
            format!("{{\"selection\":{spec},\"baseline\":{spec},\"dimension\":\"model\"}}");
        let (status, _ct, body) = write_api(&db, "/__tare/cohort/facets", body_req.as_bytes());
        assert_eq!(status, 200, "body={body}");
        let resp: AnalysisResponse<tare_core::cohort::CohortFacetResult> =
            serde_json::from_str(&body).unwrap();
        assert_eq!(
            resp.data.dimension,
            tare_core::cohort::CohortDimension::Model
        );
        assert_honest_provenance(&resp.provenance);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn write_api_cohort_compare_returns_envelope() {
        let db = cohort_test_db("compare");
        let spec = cohort_spec_json();
        let body_req = format!(
            "{{\"selection\":{spec},\"baseline\":{spec},\"match\":{{\"kind\":\"aggregate_only\"}}}}"
        );
        let (status, _ct, body) = write_api(&db, "/__tare/cohort/compare", body_req.as_bytes());
        assert_eq!(status, 200, "body={body}");
        let resp: AnalysisResponse<tare_core::cohort::CohortCompareResult> =
            serde_json::from_str(&body).unwrap();
        // Selection == baseline → zero total delta; the three deltas still sum exactly to it.
        assert_eq!(resp.data.total_delta_micros, 0);
        assert_eq!(
            resp.data.volume_delta_micros
                + resp.data.size_delta_micros
                + resp.data.efficiency_delta_micros,
            resp.data.total_delta_micros
        );
        // aggregate_only always carries a confounding warning.
        assert!(!resp.data.compatibility_warnings.is_empty());
        assert_honest_provenance(&resp.provenance);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn write_api_cohort_search_returns_envelope() {
        let db = cohort_test_db("search");
        let spec = cohort_spec_json();
        let body_req =
            format!("{{\"cohort\":{spec},\"query\":\"a\",\"fields\":null,\"limit\":null}}");
        let (status, _ct, body) = write_api(&db, "/__tare/cohort/search", body_req.as_bytes());
        assert_eq!(status, 200, "body={body}");
        let resp: AnalysisResponse<tare_core::cohort::CohortSearchResult> =
            serde_json::from_str(&body).unwrap();
        assert!(!resp.data.truncated); // one demo run can't exceed the 200 cap
        assert_honest_provenance(&resp.provenance);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn write_api_cohort_timeline_returns_dense_scoped_envelope() {
        let db = cohort_test_db("timeline");
        let spec = cohort_spec_json().replace(
            "\"from\":null,\"to\":null",
            "\"from\":\"2026-06-01\",\"to\":\"2026-06-02\"",
        );
        let body_req = format!("{{\"cohort\":{spec},\"group\":\"total\"}}");
        let (status, _ct, body) = write_api(&db, "/__tare/cohort/timeline", body_req.as_bytes());
        assert_eq!(status, 200, "body={body}");
        let resp: AnalysisResponse<tare_core::cohort::CohortTimelineResult> =
            serde_json::from_str(&body).unwrap();
        assert_eq!(resp.data.days, vec!["2026-06-01", "2026-06-02"]);
        assert_eq!(resp.data.series[0].points.len(), 2);
        assert_eq!(
            resp.data.unit,
            tare_core::cohort::TimelineUnit::EstimatedMicroUsd
        );
        assert_honest_provenance(&resp.provenance);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn write_api_cohort_rejects_oversized_body() {
        let db = cohort_test_db("big");
        let big = vec![b' '; 256 * 1024 + 1]; // checked before any parse
        let (status, _ct, body) = write_api(&db, "/__tare/cohort/resolve", &big);
        assert_eq!(status, 413);
        assert!(body.contains("256 KiB"));
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn write_api_cohort_rejects_invalid_spec() {
        let db = cohort_test_db("badspec");
        // Empty timezone → CohortError::EmptyTimezone → 400 (contract violation, not a store error).
        let bad_spec = cohort_spec_json().replace("\"timezone\":\"UTC\"", "\"timezone\":\"\"");
        let (status, _ct, body) = write_api(&db, "/__tare/cohort/resolve", bad_spec.as_bytes());
        assert_eq!(status, 400, "body={body}");
        assert!(body.contains("error"));
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn write_api_cohort_unknown_endpoint_is_404() {
        let db = cohort_test_db("unknown");
        let (status, _ct, _body) =
            write_api(&db, "/__tare/cohort/bogus", cohort_spec_json().as_bytes());
        assert_eq!(status, 404);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn write_api_cohort_malformed_json_is_400() {
        let db = cohort_test_db("malformed");
        let (status, _ct, _body) = write_api(&db, "/__tare/cohort/resolve", b"{not json");
        assert_eq!(status, 400);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn write_api_anomaly_why_returns_array_and_validates() {
        // The scoped anomaly-why endpoint returns the bare decomposition array
        // (same shape core produces) and rejects a malformed body — HTTP parity + error path.
        let db = cohort_test_db("anomwhy");
        let (status, ct, body) = write_api(&db, "/__tare/anomaly_why", br#"{"dimension":"total"}"#);
        assert_eq!(status, 200, "body={body}");
        assert!(ct.contains("application/json"));
        // Parses as the exact core type — the API doesn't reshape the decomposition.
        let _rows: Vec<tare_core::anomaly::AnomalyWhy> = serde_json::from_str(&body).unwrap();
        // A malformed request body is a 400, not a 500.
        let (bad, _ct, _b) = write_api(&db, "/__tare/anomaly_why", b"{not json");
        assert_eq!(bad, 400);
        // Body-size guard.
        let big = vec![b' '; 256 * 1024 + 1];
        let (toobig, _ct, _b) = write_api(&db, "/__tare/anomaly_why", &big);
        assert_eq!(toobig, 413);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn write_api_experiment_runs_grid_and_validates() {
        // POST /__tare/experiment returns a deterministic ExperimentResult
        // (grid + Pareto) over the cohort, and rejects a malformed / oversized body.
        let db = cohort_test_db("experiment");
        let spec = "{\"timezone\":\"UTC\",\"entity\":\"run\",\"filters\":[],\
             \"pricing\":{\"mode\":\"effective_dated\"},\"metric\":\"spend_micros\",\
             \"normalization\":\"absolute\"}";
        let body = format!(
            "{{\"cohort\":{spec},\"experiment\":{{\"axes\":[{{\"kind\":\"cache_strategy\",\
             \"values\":[\"*as-captured*\",\"decache\"]}}]}}}}"
        );
        let (status, ct, body_s) = write_api(&db, "/__tare/experiment", body.as_bytes());
        assert_eq!(status, 200, "body={body_s}");
        assert!(ct.contains("application/json"));
        let result: tare_core::experiment::ExperimentResult =
            serde_json::from_str(&body_s).unwrap();
        assert_eq!(result.cells.len(), 2); // as-captured + decache
        assert!(result.baseline_micros >= 0);
        // Malformed body → 400 (not 500).
        let (bad, _ct, _b) = write_api(&db, "/__tare/experiment", b"{not json");
        assert_eq!(bad, 400);
        let big = vec![b' '; 256 * 1024 + 1];
        let (toobig, _ct, _b) = write_api(&db, "/__tare/experiment", &big);
        assert_eq!(toobig, 413);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn savings_actions_lifecycle_over_http() {
        // Accept → actions list → 409 incompatible transition → unaccept →
        // 404 unaccept-missing, all over the HTTP transport with the proper status envelope. The
        // v2 ledger also reads live applied exposure while excluding a dismissed action.
        let db = cohort_test_db("svgact");
        let spec = "{\"timezone\":\"UTC\",\"entity\":\"run\",\"filters\":[],\
             \"pricing\":{\"mode\":\"effective_dated\"},\"metric\":\"spend_micros\",\
             \"normalization\":\"absolute\"}";
        let req = format!(
            "{{\"opportunity_key\":\"loop:search\",\"cohort\":{spec},\
             \"match\":{{\"kind\":\"aggregate_only\"}},\"metric\":\"spend_micros\",\
             \"normalization\":\"absolute\",\"expected_point_micros\":3210}}"
        );
        // Accept.
        let (s, _c, b) = write_api(&db, "/__tare/savings/accept", req.as_bytes());
        assert_eq!(s, 200, "body={b}");
        // The action lists with the aggregate-only compatibility warning.
        let (ls, _c, body) =
            read_api(&db, &load_pricing(None).unwrap(), "/__tare/savings/actions").unwrap();
        assert_eq!(ls, 200);
        let acts: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
        assert_eq!(acts.len(), 1);
        assert_eq!(acts[0]["status"], "applied");
        assert!(!acts[0]["compatibility_warnings"]
            .as_array()
            .unwrap()
            .is_empty());
        let hash = acts[0]["cohort_hash"].as_str().unwrap().to_string();
        // A separate dismissed action is persisted but contributes neither applied nor observed.
        let dismissed = req
            .replace("loop:search", "loop:dismissed")
            .replace("3210", "9999");
        assert_eq!(
            write_api(&db, "/__tare/savings/dismiss", dismissed.as_bytes()).0,
            200
        );
        let (_ss, _sc, savings_body) =
            read_api(&db, &load_pricing(None).unwrap(), "/__tare/savings").unwrap();
        let ledger: tare_core::savings::SavingsLedgerV2 =
            serde_json::from_str(&savings_body).unwrap();
        assert_eq!(ledger.applied_micros, 3_210);
        // Incompatible in-place transition (applied → dismissed) → 409.
        let (conflict, _c, _b) = write_api(&db, "/__tare/savings/dismiss", req.as_bytes());
        assert_eq!(conflict, 409);
        // Bad contract → 400.
        let (bad, _c, _b) = write_api(&db, "/__tare/savings/accept", b"{not json");
        assert_eq!(bad, 400);
        // Unaccept the real action → 200, then it's gone.
        let unaccept =
            format!("{{\"opportunity_key\":\"loop:search\",\"cohort_hash\":\"{hash}\"}}");
        let (us, _c, _b) = write_api(&db, "/__tare/savings/unaccept", unaccept.as_bytes());
        assert_eq!(us, 200);
        // Unaccept a missing action → 404.
        let (missing, _c, _b) = write_api(
            &db,
            "/__tare/savings/unaccept",
            b"{\"opportunity_key\":\"nope\",\"cohort_hash\":\"0\"}",
        );
        assert_eq!(missing, 404);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn savings_verify_over_http() {
        // Verify a stored action over HTTP → a SavingsVerifyResult; unknown
        // action → 404; bad body → 400.
        let db = cohort_test_db("svgver");
        let spec = "{\"timezone\":\"UTC\",\"entity\":\"run\",\"filters\":[],\
             \"pricing\":{\"mode\":\"effective_dated\"},\"metric\":\"spend_micros\",\
             \"normalization\":\"absolute\"}";
        // Apply an aggregate-only action, then verify it.
        let req = format!(
            "{{\"opportunity_key\":\"loop:search\",\"cohort\":{spec},\
             \"match\":{{\"kind\":\"aggregate_only\"}},\"metric\":\"spend_micros\",\
             \"normalization\":\"absolute\"}}"
        );
        assert_eq!(
            write_api(&db, "/__tare/savings/accept", req.as_bytes()).0,
            200
        );
        // Recover the cohort_hash from the actions list.
        let (_s, _c, body) =
            read_api(&db, &load_pricing(None).unwrap(), "/__tare/savings/actions").unwrap();
        let acts: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
        let hash = acts[0]["cohort_hash"].as_str().unwrap().to_string();
        let vbody = format!(
            "{{\"opportunity_key\":\"loop:search\",\"cohort_hash\":\"{hash}\",\"window_days\":7}}"
        );
        let (vs, ct, vbody_out) = write_api(&db, "/__tare/savings/verify", vbody.as_bytes());
        assert_eq!(vs, 200, "body={vbody_out}");
        assert!(ct.contains("application/json"));
        let vr: tare_core::savings::SavingsVerifyResult = serde_json::from_str(&vbody_out).unwrap();
        // Aggregate-only carries the confounding warning; never the global realization number.
        assert!(vr
            .compatibility_warnings
            .iter()
            .any(|w| w.contains("aggregate-only")));
        // Unknown action → 404.
        let (nf, _c, _b) = write_api(
            &db,
            "/__tare/savings/verify",
            b"{\"opportunity_key\":\"nope\",\"cohort_hash\":\"0\"}",
        );
        assert_eq!(nf, 404);
        // Bad body → 400.
        let (bad, _c, _b) = write_api(&db, "/__tare/savings/verify", b"{not json");
        assert_eq!(bad, 400);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn compatibility_aliases_work() {
        // Documented parameter and route spellings work as aliases.
        let db = cohort_test_db("compat");
        let pricing = load_pricing(None).unwrap();
        // `/investigations/upsert` upserts identically to `/investigations`.
        let dto = "{\"id\":\"v1\",\"label\":\"L\",\"version\":2,\"state\":{\"workspace\":\"investigate\"},\
             \"created_at\":\"2026-07-01T00:00:00Z\",\"updated_at\":\"2026-07-01T00:00:00Z\"}";
        assert_eq!(
            write_api(&db, "/__tare/investigations/upsert", dto.as_bytes()).0,
            200
        );
        let (_s, _c, body) = read_api(&db, &pricing, "/__tare/investigations").unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<serde_json::Value>>(&body)
                .unwrap()
                .len(),
            1
        );
        // flame_diff accepts the documented `normalize` spelling (as well as `normalized`).
        let (fs, _c, body) = read_api(
            &db,
            &pricing,
            "/__tare/flame_diff?a=demo&b=demo&normalize=true",
        )
        .unwrap();
        assert_eq!(fs, 200);
        let fd: tare_core::flame_diff::FlameDiffModel = serde_json::from_str(&body).unwrap();
        assert!(fd.normalized);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn investigations_round_trip_over_http() {
        // Upsert (POST) → list (GET) → delete (POST) over the HTTP transport,
        // plus the validation + size-cap error paths. The stored DTO round-trips verbatim.
        let db = cohort_test_db("invg");
        let dto = "{\"id\":\"v1\",\"label\":\"Trends\",\"version\":2,\
             \"state\":{\"workspace\":\"investigate\"},\"pane_widths\":{\"canvas\":320},\
             \"created_at\":\"2026-07-01T00:00:00Z\",\"updated_at\":\"2026-07-01T00:00:00Z\"}";
        let (s, _c, _b) = write_api(&db, "/__tare/investigations", dto.as_bytes());
        assert_eq!(s, 200);
        // List reflects it (read transport).
        let (ls, ct, body) =
            read_api(&db, &load_pricing(None).unwrap(), "/__tare/investigations").unwrap();
        assert_eq!(ls, 200);
        assert!(ct.contains("application/json"));
        let list: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["label"], "Trends");
        assert_eq!(list[0]["pane_widths"]["canvas"], 320);
        // Missing id → 400.
        let (bad, _c, _b) = write_api(&db, "/__tare/investigations", b"{\"label\":\"x\"}");
        assert_eq!(bad, 400);
        // Oversized body → 413.
        let big = vec![b' '; 256 * 1024 + 1];
        let (toobig, _c, _b) = write_api(&db, "/__tare/investigations", &big);
        assert_eq!(toobig, 413);
        // Delete, then the list is empty.
        let (ds, _c, _b) = write_api(&db, "/__tare/investigations/delete", b"{\"id\":\"v1\"}");
        assert_eq!(ds, 200);
        let (_s, _c, body) =
            read_api(&db, &load_pricing(None).unwrap(), "/__tare/investigations").unwrap();
        let list: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
        assert!(list.is_empty());
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn quality_scalar_round_trips_and_rejects_non_integers() {
        let db = std::env::temp_dir()
            .join(format!("tare-q-{}.db", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&db);
        demo_seed_store(&db).unwrap();
        let run = tare_core::demo::DEMO_RUN_ID;

        // Set via the write API, read back via the read API.
        let body = format!(r#"{{"run_id":"{run}","score":88,"source":"ci"}}"#);
        let (st, _ct, _b) = write_api(&db, "/__tare/quality", body.as_bytes());
        assert_eq!(st, 200);
        let (st, _ct, got) = read_api(
            &db,
            &load_pricing(None).unwrap(),
            &format!("/__tare/quality?run={run}"),
        )
        .unwrap();
        assert_eq!(st, 200);
        let q: tare_store::RunQuality = serde_json::from_str(&got).unwrap();
        assert_eq!(q.score, 88);
        assert_eq!(q.source, "ci");

        // Hard line: Tare stores a number, never a thing to run/read. A non-integer is refused.
        let (st, _ct, _b) = write_api(
            &db,
            "/__tare/quality",
            br#"{"run_id":"demo","score":"good"}"#,
        );
        assert_ne!(st, 200, "a non-integer score is rejected, not coerced");
        // Bad source is refused too.
        let (st, _ct, _b) = write_api(
            &db,
            "/__tare/quality",
            br#"{"run_id":"demo","score":50,"source":"llm-judge"}"#,
        );
        assert_ne!(st, 200, "only cli|header|ci provenance is accepted");

        // The whole set feeds the frontier y-axis.
        let (st, _ct, all) =
            read_api(&db, &load_pricing(None).unwrap(), "/__tare/quality").unwrap();
        assert_eq!(st, 200);
        let rows: Vec<tare_store::RunQuality> = serde_json::from_str(&all).unwrap();
        assert_eq!(rows.len(), 1);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn savings_accept_then_realized_round_trips() {
        let db = std::env::temp_dir()
            .join(format!("tare-real-{}.db", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&db);
        let pricing = load_pricing(None).unwrap();
        demo_seed_store(&db).unwrap();
        // Pick the top opportunity from the demo ledger and accept it.
        let led = savings_for(&db, &pricing).unwrap();
        let opp = led
            .opportunities
            .first()
            .expect("demo run yields at least one opportunity");
        let key = format!("{}:{}", opp.kind, opp.label);
        let snap = accept_savings_for(&db, &key, "2026-06-14", &pricing).unwrap();
        assert_eq!(
            snap, opp.recoverable_micros,
            "acceptance snapshots the estimate"
        );
        // Realized ledger has the accepted row; window complete since today >> accept+window.
        let rl = realized_for(&db, "2026-06-30", 7, &pricing).unwrap();
        let row = rl.rows.iter().find(|r| r.opportunity_key == key).unwrap();
        assert_eq!(row.accepted_date, "2026-06-14");
        assert!(row.complete);
        assert!(["realized", "not-realized"].contains(&row.state.as_str()));
        // Un-accept clears it.
        unaccept_savings_for(&db, &key).unwrap();
        assert!(realized_for(&db, "2026-06-30", 7, &pricing)
            .unwrap()
            .rows
            .is_empty());
        // Accepting an unknown key errors rather than silently recording.
        assert!(accept_savings_for(&db, "loop:does-not-exist", "2026-06-14", &pricing).is_err());
        assert!(render_realization_text(&rl).contains("realization"));
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn ci_scaffold_templates_are_wellformed() {
        let gh = render_github_workflow();
        assert!(gh.contains("on: [pull_request]"));
        assert!(gh.contains("tare gate --github-comment"));
        assert!(
            gh.contains("fetch-depth: 0"),
            "needs full history for --baseline-ref"
        );
        let gl = render_gitlab_ci();
        assert!(gl.contains("merge_request_event"));
        assert!(gl.contains("tare gate"));
        let hook = render_prepush_hook();
        assert!(hook.starts_with("#!/bin/sh"), "executable shell hook");
        assert!(hook.contains("--fail-on-regression"));
        assert!(hook.contains("--no-verify"), "documents the bypass");
    }

    #[test]
    fn commit_cost_series_aligns_git_order_with_costs() {
        // Build a rollup-by-commit report from two commit-labeled runs, then align to git order.
        let mk = |run: &str, commit: &str, out: u64| {
            let mut s = transcript_record_to_step(
                &tare_core::transcript::TranscriptRecord {
                    model: "claude-opus-4-8".into(),
                    usage: tare_core::model::UsageTokens {
                        output: out,
                        ..Default::default()
                    },
                    session_id: Some(run.into()),
                    message_id: Some(run.into()),
                    request_id: None,
                    uuid: None,
                    parent_uuid: None,
                    timestamp: Some("2026-06-20T00:00:00Z".into()),
                },
                run,
                1,
            );
            s.shape.commit = Some(commit.into());
            tare_core::model::RunRecord {
                run_id: run.into(),
                steps: vec![s],
            }
        };
        let runs = vec![mk("r1", "aaa", 1000), mk("r2", "ccc", 5000)];
        let pricing = load_pricing(None).unwrap();
        let rep = tare_core::rollup::rollup(&runs, &pricing, tare_core::rollup::RollupDim::Commit);
        // Git order lists aaa, bbb (no runs — dropped), ccc.
        let order = vec!["aaa".to_string(), "bbb".to_string(), "ccc".to_string()];
        let (labels, values) = commit_cost_series(&rep, &order);
        assert_eq!(
            labels,
            vec!["aaa".to_string(), "ccc".to_string()],
            "only captured commits, in git order"
        );
        assert_eq!(values.len(), 2);
        assert!(
            values[1] > values[0],
            "ccc (5k output) costs more than aaa (1k)"
        );
        // git_commit_order returns real short SHAs in this repo (chronological).
        let order = git_commit_order(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert!(
            order.len() >= 2
                && order
                    .iter()
                    .all(|c| c.chars().all(|x| x.is_ascii_hexdigit()))
        );
    }

    #[test]
    fn baseline_ref_resolves_commit_cost() {
        // detect_merge_base: HEAD's merge-base with itself is HEAD → a 12-char hex short SHA.
        let cwd = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mb = detect_merge_base(cwd, "HEAD").expect("merge-base HEAD HEAD");
        assert_eq!(mb.len(), 12);
        assert!(mb.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(detect_merge_base(cwd, "no-such-ref-xyz").is_none());

        // baseline_from_commit: look up a commit's rolled-up cost from stored, git-labeled steps.
        let db = std::env::temp_dir()
            .join(format!("tare-bref-{}.db", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&db);
        let pricing = load_pricing(None).unwrap();
        {
            let store = Store::open(&db).unwrap();
            let mut s = transcript_record_to_step(
                &tare_core::transcript::TranscriptRecord {
                    model: "claude-opus-4-8".into(),
                    usage: tare_core::model::UsageTokens {
                        output: 1000,
                        ..Default::default()
                    },
                    session_id: Some("r".into()),
                    message_id: Some("m".into()),
                    request_id: None,
                    uuid: None,
                    parent_uuid: None,
                    timestamp: Some("2026-06-20T00:00:00Z".into()),
                },
                "r",
                1,
            );
            s.shape.commit = Some("abc123def456".into());
            store
                .record_step_with_policy(&s, "2026-06-20", None, None, Some("jsonl"))
                .unwrap();
        }
        let got = baseline_from_commit(&db, &pricing, "abc123def456").unwrap();
        assert!(
            got.is_some() && got.unwrap() > 0,
            "commit has a stored cost"
        );
        assert_eq!(
            baseline_from_commit(&db, &pricing, "deadbeef").unwrap(),
            None
        );
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn gate_pr_comment_renders_pass_fail_and_baseline_delta() {
        // With a baseline: shows This PR / Baseline / Δ ($ + %) and a pass marker.
        let c = render_gate_pr_comment(true, 6_000_000, Some(4_000_000), "fixture-2026.06");
        assert!(c.contains("passed"));
        assert!(c.contains("This PR"));
        assert!(c.contains("🔺")); // cost went up
        assert!(c.contains("+50%"), "6 vs 4 → +50%");
        assert!(c.contains("Not a bill"));
        // A drop shows the down arrow + negative pct.
        let down = render_gate_pr_comment(true, 3_000_000, Some(4_000_000), "v");
        assert!(down.contains("🔻") && down.contains("-25%"));
        // Failure marker + no-baseline path shows a bare total.
        let f = render_gate_pr_comment(false, 9_000_000, None, "v");
        assert!(f.contains("FAILED"));
        assert!(f.contains("Estimated cost:"));
        assert!(!f.contains("Baseline"));
    }

    #[test]
    fn detect_git_attribution_reads_this_repo() {
        // The crate builds inside the tare git repo, so detection returns a real, hex SHA.
        let g = detect_git_attribution(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
            .expect("running inside a git repo");
        assert_eq!(g.sha.len(), 40, "full SHA");
        assert!(g.sha.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(g.short_sha.len(), 12);
        assert!(g.author.is_some());
        // A non-repo path yields nothing (never a fabricated attribution).
        assert!(detect_git_attribution(std::path::Path::new("/")).is_none());
    }

    #[test]
    fn pricing_refresh_writes_a_dated_edition_from_litellm() {
        let dir = std::env::temp_dir().join(format!("tare-pr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let from = dir.join("litellm.json");
        let out = dir.join("pricing-2026-07-02.json");
        std::fs::write(
            &from,
            r#"{"claude-opus-4-8":{"litellm_provider":"anthropic","input_cost_per_token":0.000005,"output_cost_per_token":0.000025}}"#,
        )
        .unwrap();
        let n = pricing_refresh_from_litellm_file(
            &from.to_string_lossy(),
            "2026-07-02",
            &out.to_string_lossy(),
        )
        .unwrap();
        assert_eq!(n, 1);
        // The written edition re-loads as a valid dated table with the converted rate.
        let table = tare_core::PricingTable::from_json_str(&std::fs::read_to_string(&out).unwrap())
            .unwrap();
        assert_eq!(table.effective_date, "2026-07-02");
        assert_eq!(
            table
                .find_by_model("claude-opus-4-8")
                .unwrap()
                .input_micro_per_mtok,
            5_000_000
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pricing_source_url_maps_known_sources_and_rejects_others() {
        // the fetch source→URL map is pure + always compiled (the fetch itself is
        // TLS-feature-gated). An unknown source is an honest error, never a silent wrong download.
        assert!(pricing_source_url("litellm")
            .unwrap()
            .contains("model_prices_and_context_window.json"));
        assert!(pricing_source_url("modelsdev")
            .unwrap()
            .contains("models.dev/api.json"));
        assert_eq!(
            pricing_source_url("models.dev"),
            pricing_source_url("modelsdev")
        );
        assert!(pricing_source_url("openrouter").is_err());
    }

    #[test]
    fn pricing_refresh_writes_a_dated_edition_from_modelsdev() {
        // the --source modelsdev path unlocks the tested from_modelsdev_json converter
        // from the CLI (import a downloaded models.dev api.json → dated edition).
        let dir = std::env::temp_dir().join(format!("tare-prmd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let from = dir.join("modelsdev.json");
        let out = dir.join("pricing-2026-07-02.json");
        std::fs::write(
            &from,
            r#"{"requesty":{"models":{"anthropic/claude-opus-4-8":{"cost":{"input":5,"output":25,"cache_read":0.5}}}}}"#,
        )
        .unwrap();
        // Dispatch via the source-keyed entry point (what the CLI calls).
        let n = pricing_refresh_from_file(
            &from.to_string_lossy(),
            "2026-07-02",
            &out.to_string_lossy(),
            "modelsdev",
        )
        .unwrap();
        assert_eq!(n, 1);
        let table = tare_core::PricingTable::from_json_str(&std::fs::read_to_string(&out).unwrap())
            .unwrap();
        assert_eq!(table.effective_date, "2026-07-02");
        assert_eq!(
            table
                .find_by_model("claude-opus-4-8")
                .unwrap()
                .input_micro_per_mtok,
            5_000_000 // 5 $/Mtok, per-Mtok in models.dev
        );
        // An unknown source errors rather than silently importing wrong (never fabricate pricing).
        assert!(pricing_refresh_from_file(
            &from.to_string_lossy(),
            "2026-07-02",
            &out.to_string_lossy(),
            "bogus"
        )
        .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_path_prefers_tare_config_env_over_the_cwd_default() {
        // the app sets TARE_CONFIG to its stable root; CLI/tests keep the CWD default.
        assert_eq!(
            config_path_from(Some("/Users/x/.tare/tare.toml".to_string())),
            "/Users/x/.tare/tare.toml"
        );
        assert_eq!(config_path_from(None), "tare.toml");
    }

    #[cfg(unix)]
    #[test]
    fn collect_jsonl_does_not_follow_symlinked_dirs_into_a_cycle() {
        // A `--dir` can be pointed anywhere, and real project trees are full of symlinks. A symlinked
        // directory pointing back at an ancestor would send a symlink-following walk into an infinite
        // loop (stack overflow). Assert the walk TERMINATES and finds each real transcript exactly
        // once, ignoring the symlink. (Pre-fix, using Path::is_dir, this looped forever.)
        let base = std::env::temp_dir().join(format!("tare-symcycle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let real = base.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(base.join("top.jsonl"), "{}\n").unwrap();
        std::fs::write(real.join("deep.jsonl"), "{}\n").unwrap();
        // A symlink inside `real` pointing back at `base` — a cycle a following walk would loop on.
        std::os::unix::fs::symlink(&base, real.join("loop")).unwrap();

        let mut out = Vec::new();
        collect_jsonl(&base, &mut out); // must terminate — no infinite recursion
        let names: std::collections::BTreeSet<String> = out
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            out.len(),
            2,
            "each real jsonl found exactly once, the symlink cycle ignored"
        );
        assert!(names.contains("top.jsonl") && names.contains("deep.jsonl"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn backfill_is_idempotent_and_skips_live_sessions() {
        let base = std::env::temp_dir().join(format!("tare-bf-{}", std::process::id()));
        let db = base.join("t.db").to_string_lossy().to_string();
        let proj = base.join("projects/-U-proj");
        let sub = proj.join("backS/subagents"); // rolls up to parent session backS
        std::fs::create_dir_all(&sub).unwrap();
        let _ = std::fs::remove_file(&db);
        let line = |id: &str, sess: &str| {
            format!(
                r#"{{"type":"assistant","sessionId":"{sess}","requestId":"r-{id}","timestamp":"2026-06-20T09:00:00Z","message":{{"id":"{id}","model":"claude-opus-4-8","usage":{{"input_tokens":100,"output_tokens":50}}}}}}"#
            )
        };
        // Session "backS": main + a subagent (rolls up to backS). Session "liveS": will be captured live.
        std::fs::write(
            proj.join("backS.jsonl"),
            format!("{}\n", line("m1", "backS")),
        )
        .unwrap();
        std::fs::write(
            sub.join("agent-x.jsonl"),
            format!("{}\n", line("m2", "sub-of-live")),
        )
        .unwrap();
        std::fs::write(
            proj.join("liveS.jsonl"),
            format!("{}\n", line("m3", "liveS")),
        )
        .unwrap();

        // Mark "liveS" as already captured live (a non-jsonl step exists).
        {
            let store = Store::open(&db).unwrap();
            let mut s = transcript_record_to_step(
                &tare_core::transcript::TranscriptRecord {
                    model: "claude-opus-4-8".into(),
                    usage: tare_core::model::UsageTokens {
                        output: 5,
                        ..Default::default()
                    },
                    session_id: Some("liveS".into()),
                    message_id: Some("live-1".into()),
                    request_id: None,
                    uuid: None,
                    parent_uuid: None,
                    timestamp: Some("2026-06-20T09:00:00Z".into()),
                },
                "liveS",
                1,
            );
            s.run_id = "liveS".into();
            store
                .record_step_with_policy(&s, "2026-06-20", None, None, Some("otel-event"))
                .unwrap();
        }

        let dirs = vec![base.join("projects")];
        let n = backfill_transcripts(&db, &dirs).unwrap();
        assert_eq!(
            n, 2,
            "backS main + its subagent; liveS skipped (captured live)"
        );
        // Idempotent: a second backfill inserts nothing.
        assert_eq!(backfill_transcripts(&db, &dirs).unwrap(), 0);
        // liveS was NOT backfilled (no jsonl step for it).
        let store = Store::open(&db).unwrap();
        let foreign = store.sessions_with_foreign_steps().unwrap();
        assert!(foreign.contains("liveS"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn savings_lens_renders_the_ranked_ledger() {
        let db = std::env::temp_dir()
            .join(format!("tare-sav-{}.db", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&db);
        let pricing = load_pricing(None).unwrap();
        demo_seed_store(&db).unwrap();
        let led = savings_for(&db, &pricing).unwrap();
        let txt = render_savings_text(&led);
        assert!(txt.contains("Savings ledger") || txt.contains("savings ledger"));
        assert!(txt.contains("Savings Index"));
        // The index is bounded to 0..100 and capped potential never exceeds spend.
        assert!((0..=100).contains(&led.savings_index));
        assert!(led.total_recoverable_micros <= led.total_spend_micros);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn digest_recomposes_the_week_from_stored_runs() {
        let db = std::env::temp_dir()
            .join(format!("tare-dig-{}.db", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&db);
        let pricing = load_pricing(None).unwrap();
        demo_seed_store(&db).unwrap();
        // The demo run is seeded on a fixed DEMO_DATE — anchor the digest there so it's in-window.
        let today = tare_core::demo::DEMO_DATE.to_string();
        let d = digest_for(&db, &today, &pricing).unwrap();
        assert_eq!(d.week_end, today);
        assert!(
            d.this_week_micros > 0,
            "the demo run lands in this week's window"
        );
        assert!(d.estimated);
        // Render is a plain-text local report.
        let txt = render_digest_text_via(&d);
        assert!(txt.contains("weekly digest"));
        assert!(txt.contains("Unclaimed savings"));
        let _ = std::fs::remove_file(&db);
    }

    // Thin shim so the test doesn't depend on tare_core being in the cli lib's namespace.
    fn render_digest_text_via(d: &tare_core::digest::Digest) -> String {
        tare_core::digest::render_digest_text(d)
    }

    #[test]
    fn estimate_like_bands_a_stored_run() {
        let db = std::env::temp_dir()
            .join(format!("tare-est-{}.db", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&db);
        let pricing = load_pricing(None).unwrap();
        demo_seed_store(&db).unwrap();
        let run = tare_core::demo::DEMO_RUN_ID;
        // Same-model estimate: floor = captured cost, high adds tool-use overhead (>= floor).
        let e = estimate_for(&db, run, None, &pricing).unwrap();
        assert!(!e.cross_tokenizer);
        assert_eq!(e.low_micros, e.point_micros);
        assert!(e.high_micros >= e.low_micros);
        assert!(e.point_micros > 0);
        // Unpriced target errors rather than rendering $0.
        assert!(estimate_for(&db, run, Some("no-such-model"), &pricing).is_err());
        // Render mentions the honest caveat.
        let txt = render_estimate_text(&e);
        assert!(txt.contains("tool-use overhead"));
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn read_api_serves_cost_quality_frontier() {
        let db = std::env::temp_dir()
            .join(format!("tare-fr-{}.db", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&db);
        let pricing = load_pricing(None).unwrap();
        demo_seed_store(&db).unwrap();
        // No quality yet → pure-cost frontier (cheapest run is the sole frontier point).
        let (st, ct, body) = read_api(&db, &pricing, "/__tare/frontier").unwrap();
        assert_eq!(st, 200);
        assert!(ct.contains("application/json"));
        let f: tare_core::experiment::Frontier = serde_json::from_str(&body).unwrap();
        assert!(!f.has_quality);
        assert!(!f.points.is_empty());
        assert!(
            f.points.iter().filter(|p| p.on_frontier).count() >= 1,
            "at least the cheapest run is on the frontier"
        );
        // Attach a quality score → has_quality flips true.
        write_api(
            &db,
            "/__tare/quality",
            format!(
                r#"{{"run_id":"{}","score":80}}"#,
                tare_core::demo::DEMO_RUN_ID
            )
            .as_bytes(),
        );
        let (_st, _ct, body) = read_api(&db, &pricing, "/__tare/frontier").unwrap();
        let f: tare_core::experiment::Frontier = serde_json::from_str(&body).unwrap();
        assert!(f.has_quality, "an ingested score turns on the quality axis");
        let scored = f
            .points
            .iter()
            .find(|point| point.run_id == tare_core::demo::DEMO_RUN_ID)
            .unwrap();
        assert_eq!(scored.quality, Some(80));
        assert_eq!(scored.quality_source.as_deref(), Some("cli"));
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn read_api_serves_profile_table() {
        let db = std::env::temp_dir()
            .join(format!("tare-prof-{}.db", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&db);
        let pricing = load_pricing(None).unwrap();
        demo_seed_store(&db).unwrap();
        let run = tare_core::demo::DEMO_RUN_ID; // "demo" — no URL-encoding needed
                                                // Cum sort by default; top=3 truncates.
        let (status, ct, body) =
            read_api(&db, &pricing, &format!("/__tare/profile?run={run}&top=3")).unwrap();
        assert_eq!(status, 200);
        assert!(ct.contains("application/json"));
        let table: tare_core::flamegraph::ProfileTable = serde_json::from_str(&body).unwrap();
        assert!(table.rows.len() <= 3, "top=3 truncates");
        assert_eq!(table.sort, tare_core::flamegraph::ProfileSort::Cum);
        // pprof invariant: self across ALL rows (untruncated) sums to the run total.
        let (_s, _c, full) = read_api(
            &db,
            &pricing,
            &format!("/__tare/profile?run={run}&sort=flat"),
        )
        .unwrap();
        let full: tare_core::flamegraph::ProfileTable = serde_json::from_str(&full).unwrap();
        let self_sum: i64 = full.rows.iter().map(|r| r.self_micros).sum();
        assert_eq!(self_sum, full.total_micros);
        assert_eq!(full.sort, tare_core::flamegraph::ProfileSort::Flat);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn read_api_serves_cost_regressions() {
        let db = std::env::temp_dir()
            .join(format!("tare-crr-{}.db", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&db);
        let pricing = load_pricing(None).unwrap();
        {
            let s = Store::open(&db).unwrap();
            let point = |metric: &str, day: &str, value: i64| {
                s.record_metered(&tare_core::otel::MeteredPoint {
                    day: day.into(),
                    metric: metric.into(),
                    model: "m".into(),
                    kind: String::new(),
                    session: "x".into(),
                    effort: String::new(),
                    query_source: String::new(),
                    value,
                })
                .unwrap();
            };
            // Four steady $1/commit days, then a day at $3/commit — a regression.
            for d in ["2026-06-20", "2026-06-21", "2026-06-22", "2026-06-23"] {
                point("cost", d, 1_000_000);
                point("commit", d, 1);
            }
            point("cost", "2026-06-24", 3_000_000);
            point("commit", "2026-06-24", 1);
        }
        let (status, ct, body) = read_api(
            &db,
            &pricing,
            "/__tare/cost_regressions?window=4&threshold=50",
        )
        .unwrap();
        assert_eq!(status, 200);
        assert!(ct.contains("application/json"));
        assert!(body.contains("\"day\":\"2026-06-24\""));
        assert!(body.contains("\"outcome\":\"commit\""));
        assert!(body.contains("\"over_pct\":200"));
        // The steady days are not flagged.
        assert!(!body.contains("2026-06-21"));
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn read_api_serves_correlation() {
        // the Explain panel re-projects stored runs onto config↔outcome axes.
        let db = std::env::temp_dir()
            .join(format!("tare-corr-{}.db", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&db);
        let pricing = load_pricing(None).unwrap();
        demo_seed_store(&db).unwrap();
        let (status, ct, body) = read_api(&db, &pricing, "/__tare/correlate").unwrap();
        assert_eq!(status, 200);
        assert!(ct.contains("application/json"));
        let report: tare_core::correlation::CorrelationReport =
            serde_json::from_str(&body).unwrap();
        // Estimate-honesty: the panel is always flagged estimated, never billed.
        assert!(report.estimated);
        assert!(!report.rows.is_empty(), "demo run projects to a row");
        // The demo run projects with its model + a cost that reconciles with the report.
        let demo = report
            .rows
            .iter()
            .find(|r| r.run_id == tare_core::demo::DEMO_RUN_ID)
            .expect("demo run present");
        assert!(!demo.model.is_empty());
        assert!(demo.cost_micros > 0);
        assert!(demo.tokens > 0);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn read_api_serves_lineage() {
        // with no [[lineage]] configured (tests run with no ambient tare.toml), the list
        // endpoint returns an empty array and a named lookup fails cleanly — the plumbing works and
        // the pure projection is covered by tare_core::lineage's own unit test.
        let db = std::env::temp_dir()
            .join(format!("tare-lin-{}.db", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&db);
        let pricing = load_pricing(None).unwrap();
        demo_seed_store(&db).unwrap();
        // No name → every configured lineage (none) → [].
        let (status, ct, body) = read_api(&db, &pricing, "/__tare/lineage").unwrap();
        assert_eq!(status, 200);
        assert!(ct.contains("application/json"));
        let reps: Vec<tare_core::lineage::LineageReport> = serde_json::from_str(&body).unwrap();
        assert!(reps.is_empty(), "no lineages configured → empty list");
        // A named lookup with no such lineage is an honest error, not a fabricated empty report.
        let (status, _, _) = read_api(&db, &pricing, "/__tare/lineage?name=nope").unwrap();
        assert_eq!(status, 500);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn read_api_serves_units() {
        // with no [[unit]] configured, every demo run falls to the unbucketed row and the
        // total reconciles — the plumbing works; bucketing logic is covered by tare_core::workunit.
        let db = std::env::temp_dir()
            .join(format!("tare-unit-{}.db", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&db);
        let pricing = load_pricing(None).unwrap();
        demo_seed_store(&db).unwrap();
        let (status, ct, body) = read_api(&db, &pricing, "/__tare/units").unwrap();
        assert_eq!(status, 200);
        assert!(ct.contains("application/json"));
        let rep: tare_core::workunit::UnitReport = serde_json::from_str(&body).unwrap();
        assert!(rep.estimated);
        // No units declared → one unbucketed row holding every run, total reconciles.
        assert_eq!(rep.rows.len(), 1);
        assert_eq!(rep.rows[0].name, "unbucketed");
        assert!(rep.unbucketed_runs >= 1);
        assert_eq!(rep.rows[0].cost_micros, rep.total_micros);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn weekly_digest_fires_once_per_week() {
        // the daemon writes the digest to a file at most once per ISO week.
        let pid = std::process::id();
        let db = std::env::temp_dir()
            .join(format!("tare-wd-{pid}.db"))
            .to_string_lossy()
            .to_string();
        let out = std::env::temp_dir().join(format!("tare-wd-out-{pid}"));
        let _ = std::fs::remove_file(&db);
        std::fs::create_dir_all(&out).unwrap();
        let out_dir = out.to_string_lossy().to_string();
        let pricing = load_pricing(None).unwrap();
        demo_seed_store(&db).unwrap();
        // Anchor "today" in the demo week so the digest window includes the seeded run.
        let today = tare_core::demo::DEMO_DATE;
        let first = maybe_write_weekly_digest(&db, &pricing, today, &out_dir).unwrap();
        let path = first.expect("first tick of the week writes a digest");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("weekly digest"),
            "rendered digest written: {text}"
        );
        // A second tick in the SAME week is a no-op (fire-once dedup via claim_alert).
        assert!(
            maybe_write_weekly_digest(&db, &pricing, today, &out_dir)
                .unwrap()
                .is_none(),
            "same week does not re-fire"
        );
        // A later week fires again.
        assert!(
            maybe_write_weekly_digest(&db, &pricing, "2026-06-10", &out_dir)
                .unwrap()
                .is_some(),
            "a new ISO week fires a fresh digest"
        );
        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn notify_command_builds_the_native_notifier_per_os() {
        // dependency-free desktop notification via each OS's built-in tool.
        let (prog, args) = notify_command("macos", "Tare", "digest ready").unwrap();
        assert_eq!(prog, "osascript");
        assert_eq!(args[0], "-e");
        assert_eq!(args[1], "on run argv");
        assert_eq!(args[7], "Tare");
        assert_eq!(args[8], "digest ready");
        assert_eq!(
            notify_command("linux", "Tare", "digest ready").unwrap(),
            (
                "notify-send".to_string(),
                vec!["Tare".to_string(), "digest ready".to_string()]
            )
        );
        // No built-in target yet on Windows/other → None (best-effort; validated later).
        assert_eq!(notify_command("windows", "Tare", "x"), None);
        // Untrusted text is carried as argv, never interpolated into executable AppleScript.
        let (_, a) = notify_command("macos", "Ti\"tle", "bo\\\"dy\nnext").unwrap();
        assert_eq!(a[7], "Ti\"tle");
        assert_eq!(a[8], "bo\\\"dy\nnext");
        assert!(!a[3].contains("Ti\"tle"));
    }

    #[test]
    fn transcript_read_and_purge_round_trip() {
        // the read/purge API over the SEPARATE transcript store. Seed it as the proxy tee
        // would (post-scrub), then read + purge via the HTTP surface.
        let pid = std::process::id();
        let db = std::env::temp_dir()
            .join(format!("tare-tx-{pid}.db"))
            .to_string_lossy()
            .to_string();
        let tpath = transcript_db_path(&db);
        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(&tpath);
        let pricing = load_pricing(None).unwrap();
        {
            let ts = tare_store::TranscriptStore::open(&tpath).unwrap();
            ts.insert("run-x", 2, "req [REDACTED:key]", "resp body", true)
                .unwrap();
        }
        // Read the captured step.
        let (status, ct, body) =
            read_api(&db, &pricing, "/__tare/transcript?run=run-x&step=2").unwrap();
        assert_eq!(status, 200);
        assert!(ct.contains("application/json"));
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["req"], "req [REDACTED:key]");
        assert_eq!(v["resp"], "resp body");
        assert_eq!(v["truncated"], true);
        // A step with no capture → JSON null (present-but-empty, not an error).
        let (_s, _c, miss) =
            read_api(&db, &pricing, "/__tare/transcript?run=run-x&step=9").unwrap();
        assert_eq!(miss, "null");
        // Missing params are a client error.
        let (bad, _, _) = read_api(&db, &pricing, "/__tare/transcript?run=run-x").unwrap();
        assert_eq!(bad, 400);
        // Purge via the write-API clears everything; the read then returns null.
        let (ps, _, _) = write_api(&db, "/__tare/transcript_purge", b"{}");
        assert_eq!(ps, 200);
        let (_s, _c, gone) =
            read_api(&db, &pricing, "/__tare/transcript?run=run-x&step=2").unwrap();
        assert_eq!(gone, "null");
        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(&tpath);
    }

    #[test]
    fn anomaly_why_attributes_the_dominant_cost_class() {
        // a spike driven overwhelmingly by cache-write should be attributed to it.
        use tare_core::model::{CacheTtl, Provider, RequestShape, StepRecord, UsageTokens};
        let db = std::env::temp_dir()
            .join(format!("tare-why-{}.db", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&db);
        let pricing = load_pricing(None).unwrap();

        let step = |cache_write: u64, fresh: u64| StepRecord {
            run_id: "r".into(),
            step_ordinal: 1,
            provider: Provider::Anthropic,
            model: "claude-opus-4-8".into(),
            usage: UsageTokens {
                fresh_input: fresh,
                cache_write_5m: cache_write,
                ..Default::default()
            },
            shape: RequestShape {
                model: "claude-opus-4-8".into(),
                provider: Provider::Anthropic,
                stream: false,
                ttl: CacheTtl::FiveMin,
                has_cache_control: cache_write > 0,
                cached_component: None,
                system_hash: None,
                weights: vec![],
                request_hash: Some(0),
                step_label: None,
                component_label: None,
                parent_label: None,
                attempt: None,
                session: None,
                workload_key: None,
                effort: None,
                mcp_server: None,
                vendor: None,
                commit: None,
                author: None,
            },
            stop_reason: None,
            duration_ms: 0,
            start_unix_nano: None,
            trace_id: None,
            span_id: None,
            parent_span_id: None,
        };
        {
            let store = Store::open(&db).unwrap();
            // Flat, cheap baseline across five days (a little fresh input, no cache-write).
            for d in [
                "2026-06-20",
                "2026-06-21",
                "2026-06-22",
                "2026-06-23",
                "2026-06-24",
            ] {
                let s = step(0, 100_000);
                store
                    .record_step(
                        &{
                            let mut s = s;
                            s.run_id = format!("run-{d}");
                            s
                        },
                        d,
                    )
                    .unwrap();
            }
            // Spike day: a huge cache-write, tiny everything else.
            let mut sp = step(80_000_000, 100_000);
            sp.run_id = "run-spike".into();
            store.record_step(&sp, "2026-06-25").unwrap();
        }

        let whys = anomaly_why_for(
            &db,
            &pricing,
            None,
            None,
            tare_core::trend::TrendDimension::Total,
            5,
            100,
        )
        .unwrap();
        let spike = whys
            .iter()
            .find(|w| w.dominant_cause.is_some())
            .expect("a spike with an attributed cause");
        let cause = spike.dominant_cause.as_ref().unwrap();
        assert_eq!(cause.label, "cache-write");
        assert!(
            cause.share_pct > 50,
            "cache-write drove the majority of the delta"
        );
        // The human render surfaces it.
        let text = render_anomaly_why_text(&whys);
        assert!(
            text.contains("mostly cache-write"),
            "render names the dominant cause: {text}"
        );
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn demo_svg_renders() {
        let pricing = load_pricing(None).unwrap();
        let model = demo_model(&pricing).unwrap();
        assert_eq!(model.run_id, "demo");
        let s = svg::render_svg(&model);
        assert!(s.starts_with("<svg"));
        assert!(s.contains("System prompt"));
    }

    #[test]
    fn demo_seed_populates_a_store_with_the_demo_run() {
        // `tare demo --db` seeds a loadable "demo" run so the dashboard isn't blank.
        let dir = std::env::temp_dir().join(format!("tare-demo-seed-{}", std::process::id()));
        let db = dir.join("demo.db");
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = db.to_str().unwrap();
        let run_id = demo_seed_store(db_path).unwrap();
        assert_eq!(run_id, "demo");
        let runs = Store::open(db_path).unwrap().load_runs().unwrap();
        let demo = runs
            .iter()
            .find(|r| r.run_id == "demo")
            .expect("demo run present");
        assert_eq!(demo.steps.len(), 3);
        // Idempotent: re-seeding doesn't duplicate steps.
        demo_seed_store(db_path).unwrap();
        let again = Store::open(db_path).unwrap().load_runs().unwrap();
        assert_eq!(
            again
                .iter()
                .find(|r| r.run_id == "demo")
                .unwrap()
                .steps
                .len(),
            3
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
