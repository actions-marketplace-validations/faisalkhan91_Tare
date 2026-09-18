//! Loopback reverse proxy for Anthropic + OpenAI. Plain HTTP, no TLS interception.
//! Buffers the request once, injects OpenAI `include_usage` when absent, forwards,
//! and tees the (possibly streaming) response: each chunk goes downstream unchanged
//! while a clone is accumulated and parsed into a `StepRecord` on completion.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use bytes::Bytes;
use futures_util::StreamExt;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use tare_core::model::{Provider, StepRecord};

pub mod fake;

const BODY_CAP: usize = 16 * 1024 * 1024;
const LOCAL_API_BODY_CAP: usize = 256 * 1024;
const RUN_ID_CAP: usize = 512;

/// Callback the proxy invokes (once, on a strict budget escalation) with a fire-once alert.
/// Kept as a plain `Fn(Alert)` so the proxy depends only on `tare-core::alert` — the caller
/// (`tare serve`/`tare run`) wires it to a `tare-daemon` sink.
#[derive(Clone)]
pub struct AlertSink(pub Arc<dyn Fn(tare_core::alert::Alert) + Send + Sync>);

impl std::fmt::Debug for AlertSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AlertSink(..)")
    }
}

/// Run-level quality side-channel: when a request carries an `x-tare-quality` header,
/// the proxy hands `(run_id, score)` to this sink so the agent can self-report a quality scalar
/// inline (no separate `tare quality` call). A run annotation, NOT step data — it deliberately
/// does NOT ride the `StepRecord`, mirroring [`AlertSink`]. Tare NEVER computes quality; this only
/// forwards a user-supplied number.
#[derive(Clone)]
pub struct QualitySink(pub Arc<dyn Fn(String, i64) + Send + Sync>);

impl std::fmt::Debug for QualitySink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("QualitySink(..)")
    }
}

/// Read-only API handler for `GET /__tare/*` (path+query in, `(status, json_body)` out, or
/// `None` for 404). Served from the store by `tare serve` so the browser viewer can fetch
/// trend/report/runs from the same loopback origin without a forward. The proxy binds loopback,
/// so this surface is loopback-only by construction. Kept as a plain `Fn` so the proxy
/// stays decoupled from the store.
/// `path_and_query` -> `(status, content_type, body)`, or `None` for an unknown path (404).
/// The content type lets the same handler serve both the JSON read API and the static UI shell
/// (HTML/JS/CSS) from the reserved `/__tare` namespace.
pub type ReadFn = Arc<dyn Fn(&str) -> Option<(u16, String, String)> + Send + Sync>;

#[derive(Clone)]
pub struct ReadHandler(pub ReadFn);

impl std::fmt::Debug for ReadHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReadHandler(..)")
    }
}

/// Handler for `POST /__tare/*` (path+query, request body) -> `(status, content_type, body)`. The
/// ONLY mutation surface, and loopback-only by construction (the proxy binds loopback, #11). Used
/// for local-only run annotations; `None` (default) = the write API returns 404.
pub type WriteFn = Arc<dyn Fn(&str, &[u8]) -> (u16, String, String) + Send + Sync>;

#[derive(Clone)]
pub struct WriteHandler(pub WriteFn);

impl std::fmt::Debug for WriteHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WriteHandler(..)")
    }
}

/// Binary read handler for `GET /__tare/*`: `path_and_query` -> `(status, content_type,
/// bytes)`, or `None` to fall through to the text `read_handler`. Lets the reserved namespace serve
/// BINARY assets (self-hosted WOFF2 fonts) that the String-bodied `ReadHandler` can't carry. Checked
/// before `read_handler` on GET; loopback-only by construction like the others.
pub type BytesReadFn = Arc<dyn Fn(&str) -> Option<(u16, String, Vec<u8>)> + Send + Sync>;

#[derive(Clone)]
pub struct BytesReadHandler(pub BytesReadFn);

impl std::fmt::Debug for BytesReadHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BytesReadHandler(..)")
    }
}

/// Where to forward each provider's traffic.
#[derive(Clone, Debug)]
pub struct ProxyConfig {
    /// Per-provider upstream base URLs. A provider missing here falls back to its public
    /// default (`default_upstream`), so adding a provider needs no new struct field.
    pub upstreams: BTreeMap<Provider, String>,
    /// When set (by `tare run`), all captured steps are stamped with this run id,
    /// overriding the `x-tare-run` header. `tare serve` leaves this `None` and groups
    /// by the header instead.
    pub run_id_override: Option<String>,
    /// Optional per-run budget / runaway-loop kill-switch enforced before forwarding.
    pub budget: Option<tare_core::budget::Budget>,
    /// Pricing used to tally per-run spend for the budget (required for `max_micros`).
    pub pricing: Option<tare_core::PricingTable>,
    /// Max bytes the response tee will buffer for parsing. A response exceeding this is
    /// still forwarded downstream byte-for-byte, but is recorded as a zero-usage
    /// placeholder rather than growing the capture buffer without bound.
    pub resp_cap: usize,
    /// Privacy policy applied when constructing each step's redacted shape. Defaults to
    /// `strict_counts` (counts + weights + unsalted hashes; never any payload text).
    pub privacy: tare_core::PrivacyPolicy,
    /// Optional sink for fire-once budget alerts. `None` (default) = no alerting.
    pub alert_sink: Option<AlertSink>,
    /// Optional read-only API handler for `GET /__tare/*` (served from the store, never
    /// forwarded). `None` (default) = the read API returns 404.
    pub read_handler: Option<ReadHandler>,
    /// Optional BINARY read handler for `GET /__tare/*`: checked before `read_handler`,
    /// serves bytes (self-hosted WOFF2 fonts) that the String `read_handler` can't. `None` = skip.
    pub bytes_read_handler: Option<BytesReadHandler>,
    /// Optional write handler for `POST /__tare/*` (loopback-only mutation, e.g. run notes).
    /// `None` (default) = the write API returns 404.
    pub write_handler: Option<WriteHandler>,
    /// Run-scoped git attribution (config-gated): when `tare run` detects a git working
    /// tree AND `[capture] git_attribution` is on, these stamp every captured step's commit/author
    /// UNLESS the request already carried an `x-tare-commit`/`-author` header (which wins). `None`
    /// (default, and for `tare serve`) leaves git attribution to the headers alone.
    pub git_commit: Option<String>,
    pub git_author: Option<String>,
    /// Optional sink for a run's `x-tare-quality` self-report. `None` (default) = the
    /// header is ignored. Never computed — only a user-supplied scalar is forwarded.
    pub quality_sink: Option<QualitySink>,
}

/// Resolve a git label for a step: an explicit `x-tare-*` header wins; otherwise fall back to the
/// run-scoped default from `ProxyConfig`. Pure.
fn resolved_git_label(header: Option<String>, default: &Option<String>) -> Option<String> {
    header.or_else(|| default.clone())
}

/// The public upstream for a provider when not overridden in `ProxyConfig.upstreams`.
fn default_upstream(provider: Provider) -> &'static str {
    match provider {
        Provider::Anthropic => "https://api.anthropic.com",
        Provider::Openai => "https://api.openai.com",
        // Azure has no single public host (per-resource endpoint); require an explicit
        // `upstreams` override / `AZURE_OPENAI_ENDPOINT`. The placeholder fails the guard
        // loudly rather than silently forwarding somewhere wrong.
        Provider::AzureOpenai => "https://AZURE-RESOURCE-NOT-CONFIGURED.invalid",
        // Bedrock is region-specific; require an explicit upstreams override / gateway base.
        Provider::BedrockConverse => "https://BEDROCK-REGION-NOT-CONFIGURED.invalid",
        Provider::Gemini => "https://generativelanguage.googleapis.com",
        // Self-hosted models are captured out-of-band (homelab agent), never proxied; no public
        // upstream. The placeholder fails the guard loudly if ever reached.
        Provider::Local => "https://LOCAL-NOT-PROXIED.invalid",
        // OpenAI-compatible vendors have no single public host, so the base URL must be supplied via
        // `upstreams`. The placeholder fails the guard loudly rather than forwarding somewhere wrong.
        Provider::OpenAiCompatible => "https://OPENAI-COMPATIBLE-BASEURL-NOT-CONFIGURED.invalid",
    }
}

impl ProxyConfig {
    /// Upstream base URL for a provider: explicit override, else the public default.
    pub fn upstream_for(&self, provider: Provider) -> String {
        self.upstreams
            .get(&provider)
            .cloned()
            .unwrap_or_else(|| default_upstream(provider).to_string())
    }
}

impl Default for ProxyConfig {
    fn default() -> Self {
        ProxyConfig {
            upstreams: BTreeMap::new(),
            run_id_override: None,
            budget: None,
            pricing: None,
            resp_cap: BODY_CAP,
            privacy: tare_core::PrivacyPolicy::default(),
            alert_sink: None,
            quality_sink: None,
            read_handler: None,
            bytes_read_handler: None,
            write_handler: None,
            git_commit: None,
            git_author: None,
        }
    }
}

/// Sink for completed steps (write to store, collect in tests, etc.).
pub type Sink = Arc<dyn Fn(StepRecord) + Send + Sync>;

/// Sink for opt-in redacted transcripts: `(run_id, step_ordinal, redacted_req,
/// redacted_resp, truncated)`. Invoked ONLY under a `max_inspect` privacy policy, AFTER redaction, so the proxy
/// stays store-free (the CLI wires this to the separate `TranscriptStore`). Bodies are scrubbed here
/// via `tare_core::redact::scrub_body` before the sink ever sees them.
pub type TranscriptSink = Arc<dyn Fn(String, u32, String, String, bool) + Send + Sync>;

/// Byte cap for a captured transcript body (256 KiB) — matches the redaction/scrub cap intent.
const TRANSCRIPT_CAP: usize = 256 * 1024;

#[derive(Clone)]
struct AppState {
    config: ProxyConfig,
    client: reqwest::Client,
    sink: Sink,
    /// Opt-in redacted-transcript sink; `None` unless the caller wired one.
    transcript_sink: Option<TranscriptSink>,
    ordinals: Arc<Mutex<HashMap<String, u32>>>,
    tallies: Arc<Mutex<HashMap<String, tare_core::budget::RunTally>>>,
}

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "host",
    "content-length",
    // Stripped so the upstream returns an identity (uncompressed) body the tee can parse;
    // the SDK client still sees whatever the upstream sends. Without this, real SDKs send
    // `Accept-Encoding: gzip` and the captured bytes are unparseable -> usage lost.
    "accept-encoding",
];

fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP.contains(&name) || name.starts_with("proxy-")
}

fn is_internal_header(name: &str) -> bool {
    name.starts_with("x-tare-")
}

fn lock_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Gemini carries the model id in the URL path (`…/models/{model}:generateContent`), not the
/// body; extract it so it becomes the pricing key. Returns the segment between
/// `models/` and the next `:` or `/`.
fn gemini_model_from_path(path: &str) -> Option<String> {
    let rest = path.split("/models/").nth(1)?;
    let end = rest.find([':', '/', '?']).unwrap_or(rest.len());
    let model = &rest[..end];
    (!model.is_empty()).then(|| model.to_string())
}

fn decode_path_segment(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            decoded.push(bytes[i]);
            i += 1;
            continue;
        }
        let hex = bytes.get(i + 1..i + 3)?;
        let digit = |byte: u8| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        };
        decoded.push(
            digit(hex[0])?
                .saturating_mul(16)
                .saturating_add(digit(hex[1])?),
        );
        i += 3;
    }
    String::from_utf8(decoded).ok()
}

/// Bedrock Converse carries `modelId` in `/model/{modelId}/converse`, not in the request body.
/// Decode the path segment (AWS SDKs escape `:` and ARN `/` separators) and reduce a model/profile
/// ARN to its final resource id so core can normalize the pricing key.
fn bedrock_model_from_path(path: &str) -> Option<String> {
    let rest = path.split("/model/").nth(1)?;
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let decoded = decode_path_segment(&rest[..end])?;
    let model = decoded.rsplit('/').next().unwrap_or(&decoded).trim();
    (!model.is_empty()).then(|| model.to_string())
}

/// Best-effort provider from the request path, or `None` when no known pattern matches, so the
/// caller can flag a guess rather than silently defaulting. Explicit selection via the
/// `x-tare-provider` header is always preferred over this.
fn provider_for_path_opt(path: &str) -> Option<Provider> {
    let p = path.to_ascii_lowercase();
    // Gemini Developer API + Vertex: …/models/{model}:generateContent[?alt=sse].
    if p.contains(":generatecontent") || p.contains(":streamgeneratecontent") {
        Some(Provider::Gemini)
    } else if p.contains("/converse") || p.contains("/invoke") || p.contains("/model/") {
        // Bedrock: /model/{modelId}/converse | /converse-stream | /invoke | invoke-with-…
        Some(Provider::BedrockConverse)
    } else if p.contains("/openai/deployments/") {
        // Azure OpenAI: /openai/deployments/{deployment}/chat/completions?api-version=…
        // Checked before the generic /chat/completions substring.
        Some(Provider::AzureOpenai)
    } else if p.contains("/chat/completions") || p.contains("/completions") {
        Some(Provider::Openai)
    } else if p.contains("/messages") || p.contains("/v1/complete") {
        // Anthropic Messages / legacy Complete.
        Some(Provider::Anthropic)
    } else {
        None
    }
}

/// Path-sniffed provider, defaulting to Anthropic for an unrecognized path (legacy behavior). The
/// handler uses `provider_for_path_opt` directly so it can mark the default as a guess; this
/// total wrapper exists only to pin that legacy default in tests.
#[cfg(test)]
fn provider_for_path(path: &str) -> Provider {
    provider_for_path_opt(path).unwrap_or(Provider::Anthropic)
}

/// One-time-per-path diagnostic when the proxy has to GUESS the provider (no `x-tare-provider`
/// header and no recognized path). Bounds noise to one line per distinct unrecognized path.
fn warn_provider_guess(path: &str) {
    use std::sync::OnceLock;
    static SEEN: OnceLock<Mutex<std::collections::BTreeSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(std::collections::BTreeSet::new()));
    if lock_recover(seen).insert(path.to_string()) {
        eprintln!(
            "tare: provider inferred (defaulted to anthropic) for unrecognized path {path:?} — \
             set the x-tare-provider header (connect preset) to attribute it correctly"
        );
    }
}

/// Loopback-only network guard. When `TARE_NETWORK_GUARD=loopback`, refuse any
/// upstream that is not loopback — so the test suite fails loudly on a non-loopback
/// connect attempt.
fn guard_allows(upstream: &str) -> bool {
    if std::env::var("TARE_NETWORK_GUARD").as_deref() != Ok("loopback") {
        return true;
    }
    let Some(host) = reqwest::Url::parse(upstream)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
    else {
        return false;
    };
    let host = host.trim_matches(['[', ']']);
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn increment_ordinal(map: &mut HashMap<String, u32>, run_id: &str) -> Option<u32> {
    let n = map.entry(run_id.to_string()).or_insert(0);
    let next = n.checked_add(1)?;
    *n = next;
    Some(next)
}

fn next_ordinal(state: &AppState, run_id: &str) -> Option<u32> {
    increment_ordinal(&mut lock_recover(&state.ordinals), run_id)
}

/// Returns a pre-send budget reservation on Drop, so it is released on EVERY exit path —
/// normal completion, early return (upstream send failure), stream error, and client
/// disconnect (the response stream dropped mid-flight). Without this, a reservation made
/// before forwarding leaks on the error/disconnect paths and eventually false-trips the spend
/// cap on a long-lived `tare serve`.
struct ReservationGuard {
    tallies: Arc<Mutex<HashMap<String, tare_core::budget::RunTally>>>,
    run_id: String,
    micros: i64,
}

impl Drop for ReservationGuard {
    fn drop(&mut self) {
        if self.micros != 0 {
            lock_recover(&self.tallies)
                .entry(self.run_id.clone())
                .or_default()
                .release(self.micros);
        }
    }
}

/// Conservative pre-send spend estimate for the budget reservation. Approximates input
/// tokens as `bytes / 4` (NO re-tokenization) priced at the fresh-input rate, so concurrent
/// in-flight requests reserve against `max_micros`. This bounds the *input* contribution to the
/// cap overshoot; OUTPUT cost isn't known pre-send and is unreserved, so a burst can still
/// overshoot by the in-flight requests' output cost (a known limitation, not a hard one-step bound).
fn estimate_input_micros(
    provider: Provider,
    vendor: Option<&str>,
    parsed: &serde_json::Value,
    body_len: usize,
    pricing: &tare_core::PricingTable,
) -> i64 {
    let Some(model) = parsed.get("model").and_then(|m| m.as_str()) else {
        return 0;
    };
    let Some(rates) = pricing.lookup(provider, vendor, model) else {
        return 0;
    };
    let approx_tokens = (body_len / 4) as u64;
    tare_core::money::MicroUsd::for_tokens(
        approx_tokens,
        rates.micro_per_mtok(tare_core::model::CacheClass::Fresh),
    )
    .micros()
}

fn error_response(code: u16, msg: &str) -> Response {
    Response::builder()
        .status(code)
        .header("content-type", "text/plain")
        .body(Body::from(msg.to_string()))
        .unwrap()
}

/// Synthetic 429 returned (without forwarding) when the budget kill-switch fires.
fn error_json_429(reason: &str) -> Response {
    let body = serde_json::json!({
        "type": "error",
        "error": { "type": "tare_budget_exceeded", "message": reason }
    })
    .to_string();
    Response::builder()
        .status(429)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap()
}

fn local_handler_response(status: u16, content_type: &str, body: impl Into<Body>) -> Response {
    let code = axum::http::StatusCode::from_u16(status)
        .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    let Ok(content_type) = content_type.parse::<axum::http::HeaderValue>() else {
        return error_response(500, "tare: local handler returned an invalid content type");
    };
    Response::builder()
        .status(code)
        .header(axum::http::header::CONTENT_TYPE, content_type)
        .body(body.into())
        .unwrap_or_else(|_| error_response(500, "tare: failed to build local response"))
}

enum CapturedRequestBody {
    Buffered(Bytes),
    Streaming(reqwest::Body),
}

/// Buffer a request through `cap`. If an undeclared/chunked body crosses the cap, reconstruct a
/// stream from the already-read prefix, the crossing chunk, and the untouched tail so fail-open
/// forwarding remains byte-for-byte rather than rejecting the request after partial consumption.
async fn capture_request_body(body: Body, cap: usize) -> Result<CapturedRequestBody, axum::Error> {
    let mut stream = body.into_data_stream();
    let mut buffered = Vec::new();
    while let Some(item) = stream.next().await {
        let chunk = item?;
        if chunk.len() <= cap.saturating_sub(buffered.len()) {
            buffered.extend_from_slice(&chunk);
            continue;
        }

        let prefix = Bytes::from(buffered);
        let reconstructed = async_stream::stream! {
            if !prefix.is_empty() {
                yield Ok::<Bytes, axum::Error>(prefix);
            }
            yield Ok::<Bytes, axum::Error>(chunk);
            while let Some(item) = stream.next().await {
                yield item;
            }
        };
        return Ok(CapturedRequestBody::Streaming(reqwest::Body::wrap_stream(
            reconstructed,
        )));
    }
    Ok(CapturedRequestBody::Buffered(Bytes::from(buffered)))
}

async fn forward_unattributed(
    state: &AppState,
    method: &axum::http::Method,
    path_and_query: &str,
    provider: Provider,
    headers: reqwest::header::HeaderMap,
    body: reqwest::Body,
    run_id: &str,
) -> Response {
    let base = state.config.upstream_for(provider);
    if !guard_allows(&base) {
        return error_response(502, "tare: network guard blocked non-loopback upstream");
    }
    let url = format!("{}{}", base.trim_end_matches('/'), path_and_query);
    let method =
        reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::POST);
    eprintln!(
        "tare: run {run_id} step forwarded unattributed (request body exceeds {BODY_CAP}-byte cap)"
    );
    let upstream = match state
        .client
        .request(method, &url)
        .headers(headers)
        .body(body)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => return error_response(502, &format!("tare: upstream error: {error}")),
    };
    let mut builder = Response::builder().status(upstream.status().as_u16());
    for (name, value) in upstream.headers() {
        let name_lower = name.as_str().to_ascii_lowercase();
        if !is_hop_by_hop(&name_lower) && !is_internal_header(&name_lower) {
            builder = builder.header(name.as_str(), value.as_bytes());
        }
    }
    builder
        .body(Body::from_stream(upstream.bytes_stream()))
        .unwrap_or_else(|_| error_response(502, "tare: failed to build response"))
}

fn emit_transcript(
    transcript_sink: Option<&TranscriptSink>,
    privacy: &tare_core::PrivacyPolicy,
    run_id: &str,
    ordinal: u32,
    request: &[u8],
    response: &[u8],
    incomplete: bool,
) {
    if !privacy.captures_transcript() {
        return;
    }
    let Some(sink) = transcript_sink else {
        return;
    };
    let request = tare_core::redact::scrub_body(&String::from_utf8_lossy(request), TRANSCRIPT_CAP);
    let response =
        tare_core::redact::scrub_body(&String::from_utf8_lossy(response), TRANSCRIPT_CAP);
    sink(
        run_id.to_string(),
        ordinal,
        request.redacted,
        response.redacted,
        incomplete || request.truncated || response.truncated,
    );
}

/// Is `hostport` (a `Host` header value or an `Origin`'s authority) a loopback name? Strips an
/// optional `:port`, handling the `[::1]:port` IPv6 bracket form.
fn host_is_loopback(hostport: &str) -> bool {
    let host = if let Some(rest) = hostport.strip_prefix('[') {
        rest.split(']').next().unwrap_or("") // [::1] / [::1]:port
    } else {
        hostport
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(hostport)
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// Anti-DNS-rebinding / anti-cross-site guard for the loopback `/__tare` control+read API.
/// A browser cannot forge `Host` or `Origin`, so requiring both to name a loopback host blocks a
/// malicious web page — or a rebound attacker domain (whose `Host` is the attacker's name, not
/// 127.0.0.1) — from reaching the API and POSTing/purging local state. A non-browser client that omits
/// `Origin` is allowed through on a loopback `Host`.
pub fn loopback_request_ok(headers: &axum::http::HeaderMap) -> bool {
    // Host must be present and loopback: a rebinding attack carries `Host: attacker-domain`.
    let host_ok = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(host_is_loopback)
        .unwrap_or(false);
    if !host_ok {
        return false;
    }
    // A cross-site fetch always sends Origin; if present it must also be loopback.
    if let Some(origin) = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    {
        let authority = origin.split("://").nth(1).unwrap_or(origin);
        let host = authority.split('/').next().unwrap_or(authority);
        if !host_is_loopback(host) {
            return false;
        }
    }
    true
}

async fn handler(State(state): State<AppState>, req: Request) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    let path_and_query = uri
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| path.clone());

    // Loopback read API: the entire `/__tare` namespace is served from the store via the read
    // handler and is NEVER forwarded upstream (the bare `/__tare` with no trailing slash is
    // matched too, so it cannot slip through to a provider.
    if path == "/__tare" || path.starts_with("/__tare/") {
        // Anti-DNS-rebinding / anti-cross-site: reject any request whose Host/Origin points off-box
        // before it can read or mutate local state. Applies to the whole namespace.
        if !loopback_request_ok(req.headers()) {
            return error_response(
                403,
                "tare: loopback-only API (off-box Host/Origin rejected)",
            );
        }
        if method.as_str() == "GET" {
            // Binary assets first: if the bytes handler claims this path (e.g. a WOFF2
            // font), serve its bytes; otherwise fall through to the text read handler below.
            if let Some(bh) = &state.config.bytes_read_handler {
                if let Some((status, content_type, bytes)) = (bh.0)(&path_and_query) {
                    return local_handler_response(status, &content_type, bytes);
                }
            }
            if let Some(h) = &state.config.read_handler {
                return match (h.0)(&path_and_query) {
                    Some((status, content_type, body)) => {
                        local_handler_response(status, &content_type, body)
                    }
                    None => error_response(404, "tare: not found"),
                };
            }
            return error_response(404, "tare: read API not enabled");
        }
        // Loopback-only WRITE surface: POST /__tare/* mutates local state (run notes).
        // Bounded body read so a POST can't exhaust memory; handler-supplied status is clamped.
        if method.as_str() == "POST" {
            if let Some(h) = state.config.write_handler.clone() {
                let bytes = match axum::body::to_bytes(req.into_body(), LOCAL_API_BODY_CAP).await {
                    Ok(b) => b,
                    Err(_) => return error_response(413, "tare: request body too large"),
                };
                let (status, content_type, body) = (h.0)(&path_and_query, &bytes);
                return local_handler_response(status, &content_type, body);
            }
            return error_response(404, "tare: write API not enabled");
        }
        return error_response(405, "tare: read/write API is GET/POST-only");
    }

    // Provider selection: trust the connect preset's explicit `x-tare-provider` header
    // over path-sniffing — every gateway routes by explicit selection, and silent path
    // misclassification corrupts attribution/cost with no signal as breadth grows. Fall back to
    // `provider_for_path_opt` (a recognized path, else a warned Anthropic guess) only when no
    // explicit tag is present.
    let provider = match req.headers().get("x-tare-provider") {
        Some(value) => {
            let Ok(value) = value.to_str() else {
                return error_response(400, "tare: invalid x-tare-provider header");
            };
            let Some(provider) = Provider::parse(value.trim()) else {
                return error_response(400, "tare: unknown x-tare-provider value");
            };
            provider
        }
        None => match provider_for_path_opt(&path) {
            Some(p) => p, // inferred from a recognized path (reliable)
            None => {
                // No explicit tag and no recognized path: defaulting to Anthropic is a GUESS that
                // can mislabel/misprice. Warn once so the user knows to set x-tare-provider — the
                // honest "provider inferred" marker.
                warn_provider_guess(&path);
                Provider::Anthropic
            }
        },
    };
    let run_id = match state.config.run_id_override.as_deref() {
        Some(run_id) => run_id.to_string(),
        None => match req.headers().get("x-tare-run") {
            Some(value) => match value.to_str() {
                Ok(value) => value.to_string(),
                Err(_) => return error_response(400, "tare: invalid x-tare-run header"),
            },
            None => "default".to_string(),
        },
    };
    if run_id.trim().is_empty() || run_id.len() > RUN_ID_CAP || run_id.chars().any(char::is_control)
    {
        return error_response(
            400,
            "tare: run id must be 1-512 bytes with no control characters",
        );
    }

    // Optional framework-adapter correlation labels (opaque, truncated to 64 chars so a
    // header can never smuggle payload). Additive — absent headers leave the shape unchanged.
    // Extracted into owned Strings here so no borrow of `req` (whose Body is !Sync) is held
    // across a later await.
    let meta = {
        let h = req.headers();
        let label = |name: &str| -> Option<String> {
            h.get(name)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.chars().take(64).collect())
        };
        tare_core::model::StepMeta {
            step_label: label("x-tare-step"),
            component_label: label("x-tare-component"),
            parent_label: label("x-tare-parent"),
            attempt: label("x-tare-attempt").and_then(|s| s.parse().ok()),
            // User-provided workload key: opaque grouping label, truncated to
            // 64 UTF-8-safe chars by `label` above so a header can never smuggle payload.
            workload_key: label("x-tare-workload-key"),
            // The OpenAI-compatible vendor (groq/together/…) the preset selected, so the step
            // prices on vendor+model. Lower-cased to a stable pricing key.
            vendor: label("x-tare-vendor").map(|s| s.to_ascii_lowercase()),
            // Git attribution: the x-tare-commit/-author header wins; otherwise the
            // run-scoped config default (set by `tare run` when config-gated on) applies.
            commit: resolved_git_label(label("x-tare-commit"), &state.config.git_commit),
            author: resolved_git_label(label("x-tare-author"), &state.config.git_author),
        }
    };

    // Quality self-report: an `x-tare-quality: <int>` header annotates the RUN, so an
    // agent can score its own output inline. Forwarded to the sink (which persists it); a run
    // annotation, not step data, so it never touches the StepRecord. Ignored if unparseable/absent.
    if let Some(sink) = state.config.quality_sink.as_ref() {
        if let Some(score) = req
            .headers()
            .get("x-tare-quality")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<i64>().ok())
            .filter(|score| (0..=100).contains(score))
        {
            (sink.0)(run_id.clone(), score);
        }
    }

    // Build forwarded request headers (strip hop-by-hop).
    let mut fwd_headers = reqwest::header::HeaderMap::new();
    for (name, value) in req.headers() {
        let lname = name.as_str().to_ascii_lowercase();
        if is_hop_by_hop(&lname) || is_internal_header(&lname) {
            continue;
        }
        if let (Ok(hn), Ok(hv)) = (
            reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()),
            reqwest::header::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            fwd_headers.insert(hn, hv);
        }
    }

    // Fail-open for oversized requests (#fail-open): a request whose declared body exceeds the
    // attribution buffer must still reach the provider — a capture limitation must never break the
    // user's request. Forward it UNBUFFERED (streamed) with no capture; it simply isn't attributed.
    let declared_len = req
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok());
    let request_body = if declared_len.is_some_and(|n| n > BODY_CAP) {
        CapturedRequestBody::Streaming(reqwest::Body::wrap_stream(
            req.into_body().into_data_stream(),
        ))
    } else {
        match capture_request_body(req.into_body(), BODY_CAP).await {
            Ok(body) => body,
            Err(_) => return error_response(400, "tare: request body could not be read"),
        }
    };
    let orig_bytes = match request_body {
        CapturedRequestBody::Buffered(bytes) => bytes,
        CapturedRequestBody::Streaming(body) => {
            return forward_unattributed(
                &state,
                &method,
                &path_and_query,
                provider,
                fwd_headers,
                body,
                &run_id,
            )
            .await;
        }
    };

    // Parse a clone for attribution against the ORIGINAL bytes (client intent).
    // For OpenAI, inject include_usage into the FORWARDED body when absent.
    let forward_bytes: Bytes = match provider {
        // OpenAI-compatible vendors speak the same dialect, so they need the same include_usage
        // injection to report usage on streamed responses.
        Provider::Openai | Provider::AzureOpenai | Provider::OpenAiCompatible => {
            let (b, _injected) = tare_core::wire::inject_openai_include_usage(&orig_bytes);
            Bytes::from(b)
        }
        // Anthropic forwards verbatim (usage is always reported); Bedrock Converse and Gemini
        // report usage unconditionally too, so no body rewrite is needed.
        Provider::Anthropic | Provider::BedrockConverse | Provider::Gemini | Provider::Local => {
            orig_bytes.clone()
        }
    };

    let base = state.config.upstream_for(provider);
    if !guard_allows(&base) {
        return error_response(502, "tare: network guard blocked non-loopback upstream");
    }
    let url = format!("{}{}", base.trim_end_matches('/'), path_and_query);

    // Budget / runaway-loop kill-switch — consulted BEFORE forwarding. On Allow/Warn we
    // RESERVE a conservative spend estimate so simultaneous in-flight requests see each
    // other's pending cost and the cap cannot be overshot by concurrent steps. The
    // reservation is released and replaced with the actual cost when the response completes.
    // Parse the body once and reuse the Value for both the request hash and the input estimate
    // (avoids a second full deserialize of a large prompt body per request).
    let parsed_body = serde_json::from_slice::<serde_json::Value>(&orig_bytes).ok();
    let incoming_hash = parsed_body.as_ref().map(tare_core::canon::request_hash);
    let mut reserved_estimate = 0i64;
    let mut kill_reason: Option<String> = None;
    let mut pending_alert: Option<tare_core::alert::Alert> = None;
    // Self-governance snapshot: integer spend, budget, steps, and a coded decision, captured
    // under the tally lock and stamped onto the forwarded response as opaque `x-tare-run-*`
    // headers so the agent can read its own running spend mid-run and self-correct before the
    // kill 429. Integers and a coded label only; never free text. Loopback only.
    let mut run_status_headers: Option<(i64, i64, u32, &'static str)> = None;
    if let (Some(budget), Some(hash)) = (state.config.budget.as_ref(), incoming_hash) {
        if budget.is_set() {
            let estimate = state
                .config
                .pricing
                .as_ref()
                .map(|p| match parsed_body.as_ref() {
                    Some(v) => estimate_input_micros(
                        provider,
                        meta.vendor.as_deref(),
                        v,
                        orig_bytes.len(),
                        p,
                    ),
                    None => 0,
                })
                .unwrap_or(0);
            let mut tallies = lock_recover(&state.tallies);
            let tally = tallies.entry(run_id.clone()).or_default();
            let decision = tare_core::budget::evaluate(budget, tally, hash);
            // Fire-once alert on a STRICT escalation (computed under the lock so it's race-free;
            // emitted below, AFTER the lock is dropped, so a slow sink can't serialize requests).
            let prev = tally
                .last_decision
                .clone()
                .unwrap_or(tare_core::budget::Decision::Allow);
            pending_alert = tare_core::alert::evaluate(&prev, &decision, &run_id);
            tally.last_decision = Some(decision.clone());
            match &decision {
                tare_core::budget::Decision::Kill(reason) => {
                    kill_reason = Some(reason.clone());
                }
                tare_core::budget::Decision::Warn(_) | tare_core::budget::Decision::Allow => {
                    tally.note_forwarded(hash);
                    tally.reserve(estimate);
                    reserved_estimate = estimate;
                }
            }
            // Snapshot for the response headers (only the coded decision crosses the wire).
            run_status_headers = Some((
                tally.effective_micros(),
                budget.max_micros.unwrap_or(0),
                tally.steps,
                decision.code(),
            ));
        }
    }
    // Emit the alert outside the tallies lock.
    if let (Some(alert), Some(sink)) = (pending_alert, state.config.alert_sink.as_ref()) {
        (sink.0)(alert);
    }
    if let Some(reason) = kill_reason {
        eprintln!("tare: run {run_id} killed: {reason}");
        return error_json_429(&reason);
    }

    // Hold the reservation in an RAII guard from here on, so it is released no matter how this
    // request exits (send failure below, stream error, client disconnect, or normal completion).
    let reservation = ReservationGuard {
        tallies: state.tallies.clone(),
        run_id: run_id.clone(),
        micros: reserved_estimate,
    };

    // Correlate: assign the ordinal at INGRESS (before the upstream send), so concurrent
    // requests are ordered by arrival rather than by which response header returns first.
    // Construct the reservation guard first so an exhausted ordinal also releases pending spend.
    let Some(ordinal) = next_ordinal(&state, &run_id) else {
        return error_response(429, "tare: run has reached the maximum step count");
    };

    let rmethod =
        reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::POST);

    // Observed local latency: a monotonic Instant bracketing the upstream round trip,
    // measured here at the edge — not in core. Copy, so it moves into the tee generator freely.
    let t_start = std::time::Instant::now();
    let upstream = state
        .client
        .request(rmethod, &url)
        .headers(fwd_headers)
        .body(forward_bytes)
        .send()
        .await;

    let upstream = match upstream {
        Ok(r) => r,
        Err(e) => return error_response(502, &format!("tare: upstream error: {e}")),
    };

    let status = upstream.status().as_u16();
    // Capture the upstream Content-Type so the parser decides SSE-vs-JSON from the header
    // rather than a content sniff.
    let content_type = upstream
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let mut builder = Response::builder().status(status);
    for (name, value) in upstream.headers() {
        let lname = name.as_str().to_ascii_lowercase();
        if is_hop_by_hop(&lname) || is_internal_header(&lname) {
            continue;
        }
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    // Self-governance headers contain opaque integers and a coded decision and stay on loopback.
    if let Some((micros, budget_micros, steps, code)) = run_status_headers {
        builder = builder
            .header("x-tare-run-micros", micros.to_string())
            .header("x-tare-run-budget", budget_micros.to_string())
            .header("x-tare-run-steps", steps.to_string())
            .header("x-tare-run-decision", code);
    }

    let sink = state.sink.clone();
    let transcript_sink = state.transcript_sink.clone();
    let req_bytes = orig_bytes.clone();
    let pricing = state.config.pricing.clone();
    let tallies = state.tallies.clone();
    let resp_cap = state.config.resp_cap;
    let privacy = state.config.privacy.clone();
    // Gemini and Bedrock model IDs live in the URL path; carry them as pricing-key hints.
    let model_hint = match provider {
        Provider::Gemini => gemini_model_from_path(&path),
        Provider::BedrockConverse => bedrock_model_from_path(&path),
        _ => None,
    };

    // Tee: forward each chunk unchanged, accumulate a bounded clone, then parse and record at
    // end. The forwarded stream is never altered or truncated — only the capture buffer is
    // capped, so a pathological/huge response can't grow proxy memory without bound.
    let mut stream = upstream.bytes_stream();
    let tee = async_stream::stream! {
        // Own the reservation: it is released when this generator is dropped — on completion,
        // on the stream-error return below, or when the client disconnects mid-stream.
        let _reservation = reservation;
        let mut buf: Vec<u8> = Vec::new();
        let mut truncated = false;
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    if !truncated {
                        let remaining = resp_cap.saturating_sub(buf.len());
                        if chunk.len() > remaining {
                            buf.extend_from_slice(&chunk[..remaining]);
                            truncated = true;
                        } else {
                            buf.extend_from_slice(&chunk);
                        }
                    }
                    yield Ok::<Bytes, std::io::Error>(chunk);
                }
                Err(e) => {
                    // Upstream stream error: record a zero-usage placeholder so the assigned
                    // ordinal is never silently lost, then end the stream (the reservation is
                    // released by `_reservation`'s Drop).
                    eprintln!(
                        "tare: run {run_id} step {ordinal}: stream error; recording zero-usage placeholder"
                    );
                    emit_transcript(
                        transcript_sink.as_ref(),
                        &privacy,
                        &run_id,
                        ordinal,
                        &req_bytes,
                        &buf,
                        true,
                    );
                    if let Some(step) = tare_core::ingest_request_only(
                        run_id.clone(),
                        ordinal,
                        provider,
                        &req_bytes,
                        "stream_error",
                        &privacy,
                    ) {
                        (sink)(step);
                    }
                    yield Err(std::io::Error::other(e.to_string()));
                    return;
                }
            }
        }
        // A capped response can't be parsed for usage — record a zero-usage placeholder so the
        // assigned ordinal is never silently lost.
        if truncated {
            eprintln!(
                "tare: run {run_id} step {ordinal}: response exceeded {resp_cap}B capture cap; recording zero-usage placeholder"
            );
            emit_transcript(
                transcript_sink.as_ref(),
                &privacy,
                &run_id,
                ordinal,
                &req_bytes,
                &buf,
                true,
            );
            if let Some(step) = tare_core::ingest_request_only(
                run_id,
                ordinal,
                provider,
                &req_bytes,
                "parse_error",
                &privacy,
            ) {
                (sink)(step);
            }
            return;
        }
        // Parse the captured response and record the step. On parse failure (compressed/error/
        // partial body) still record a zero-usage step so the ordinal is never lost.
        // Latency = the monotonic elapsed since just before the upstream send (clamped to u64 ms).
        let duration_ms = t_start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        emit_transcript(
            transcript_sink.as_ref(),
            &privacy,
            &run_id,
            ordinal,
            &req_bytes,
            &buf,
            false,
        );
        match tare_core::ingest_step_ct(run_id.clone(), ordinal, provider, &req_bytes, &buf, content_type.as_deref(), &privacy, &meta, model_hint.as_deref(), duration_ms) {
            Ok(step) => {
                // Tally estimated spend for the budget guard (best-effort).
                if let Some(p) = pricing.as_ref() {
                    if let Some(rates) = p.lookup(step.provider, step.shape.vendor.as_deref(), &step.model) {
                        let micros = tare_core::account::cost_usage(&step.usage, rates, &step.shape)
                            .total
                            .micros();
                        lock_recover(&tallies)
                            .entry(run_id.clone())
                            .or_default()
                            .add_cost(micros);
                    }
                }
                (sink)(step);
            }
            Err(e) => {
                // Distinguish a detected provider error body from an unparseable response.
                // NOTE: never interpolate `e` — for a provider error it carries the upstream
                // error.message may contain payload text; log only the structural tag.
                let tag = if e.contains("provider error") { "error" } else { "parse_error" };
                eprintln!(
                    "tare: run {run_id} step {ordinal}: {tag}; recording zero-usage placeholder"
                );
                if let Some(step) = tare_core::ingest_request_only(
                    run_id, ordinal, provider, &req_bytes, tag, &privacy,
                ) {
                    (sink)(step);
                }
            }
        }
    };

    match builder.body(Body::from_stream(tee)) {
        Ok(resp) => resp,
        Err(_) => error_response(500, "tare: failed to build response"),
    }
}

/// Build the proxy router with a step sink.
pub fn router(config: ProxyConfig, sink: Sink) -> Router {
    router_with_transcript(config, sink, None)
}

/// As [`router`], plus an optional redacted-transcript sink. The transcript sink is only
/// invoked when the config's privacy policy is `max_inspect`; otherwise it's dead weight. Bodies are
/// scrubbed via `tare_core::redact::scrub_body` before the sink sees them.
pub fn router_with_transcript(
    config: ProxyConfig,
    sink: Sink,
    transcript_sink: Option<TranscriptSink>,
) -> Router {
    let state = AppState {
        config,
        client: reqwest::Client::builder().build().expect("reqwest client"),
        sink,
        transcript_sink,
        ordinals: Arc::new(Mutex::new(HashMap::new())),
        tallies: Arc::new(Mutex::new(HashMap::new())),
    };
    Router::new().fallback(any(handler)).with_state(state)
}

/// Serve until `shutdown` resolves, then GRACEFULLY drain in-flight connections before returning.
/// This matters for capture integrity: each connection task holds a Sink sender, and a request
/// whose response is still being tee'd into the sink must finish before the writer is joined —
/// otherwise that step is lost. A plain serve + task `abort()` drops those tasks mid-flight.
///
/// Pass `transcript_sink: None` for the common case; supply one only when the config's privacy
/// policy is `max_inspect` and redacted transcripts should be captured.
pub async fn serve_with_shutdown_transcript(
    listener: tokio::net::TcpListener,
    config: ProxyConfig,
    sink: Sink,
    transcript_sink: Option<TranscriptSink>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let app = router_with_transcript(config, sink, transcript_sink);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_guard_rejects_offbox_host_and_origin() {
        // the /__tare API accepts only loopback Host + (if present) loopback Origin, so a
        // malicious web page or a rebound attacker domain can't reach it. Browsers can't forge these.
        use axum::http::{header, HeaderMap, HeaderValue};
        let mk = |host: Option<&str>, origin: Option<&str>| {
            let mut h = HeaderMap::new();
            if let Some(v) = host {
                h.insert(header::HOST, HeaderValue::from_str(v).unwrap());
            }
            if let Some(v) = origin {
                h.insert(header::ORIGIN, HeaderValue::from_str(v).unwrap());
            }
            h
        };
        // Same-origin loopback (with or without Origin, incl. IPv6) is allowed.
        assert!(loopback_request_ok(&mk(Some("127.0.0.1:8788"), None)));
        assert!(loopback_request_ok(&mk(
            Some("localhost:8788"),
            Some("http://localhost:8788")
        )));
        assert!(loopback_request_ok(&mk(Some("[::1]:8788"), None)));
        // A rebound attacker domain (Host is the attacker's name, not 127.0.0.1) is rejected.
        assert!(!loopback_request_ok(&mk(Some("evil.example.com"), None)));
        // A cross-site page (loopback Host but off-box Origin) is rejected.
        assert!(!loopback_request_ok(&mk(
            Some("127.0.0.1:8788"),
            Some("https://evil.example.com")
        )));
        // No Host at all → rejected (can't rebind, but fail closed on the control API).
        assert!(!loopback_request_ok(&mk(None, None)));
    }

    #[test]
    fn git_label_header_wins_over_run_default() {
        // an explicit x-tare-* header takes precedence; else the run-scoped default.
        let default = Some("abc123".to_string());
        assert_eq!(
            resolved_git_label(Some("hdr-sha".into()), &default),
            Some("hdr-sha".to_string()),
            "header wins"
        );
        assert_eq!(
            resolved_git_label(None, &default),
            Some("abc123".to_string()),
            "falls back to the run default"
        );
        assert_eq!(resolved_git_label(None, &None), None, "neither → unlabeled");
    }

    #[test]
    fn provider_routing_precedence() {
        // Recognized paths classify; the wrapper defaults unmatched to Anthropic (legacy).
        assert_eq!(provider_for_path("/v1/messages"), Provider::Anthropic);
        assert_eq!(provider_for_path("/v1/chat/completions"), Provider::Openai);
        assert_eq!(
            provider_for_path("/openai/deployments/d/chat/completions?api-version=x"),
            Provider::AzureOpenai
        );
        assert_eq!(
            provider_for_path("/model/x/converse"),
            Provider::BedrockConverse
        );
        assert_eq!(
            provider_for_path("/v1beta/models/gemini-2.5-flash:generateContent"),
            Provider::Gemini
        );
    }

    #[test]
    fn unrecognized_path_is_an_explicit_none_not_a_silent_default() {
        // a recognized path returns Some(...), but a path matching no pattern returns
        // None so the handler can flag the Anthropic default as a guess (not silently mislabel).
        assert_eq!(
            provider_for_path_opt("/v1/messages"),
            Some(Provider::Anthropic)
        );
        assert_eq!(
            provider_for_path_opt("/v1/chat/completions"),
            Some(Provider::Openai)
        );
        assert_eq!(provider_for_path_opt("/some/unknown/gateway/route"), None);
        assert_eq!(provider_for_path_opt("/"), None);
    }

    #[test]
    fn gemini_model_extracted_from_path() {
        assert_eq!(
            gemini_model_from_path("/v1beta/models/gemini-2.5-flash:generateContent"),
            Some("gemini-2.5-flash".to_string())
        );
        assert_eq!(
            gemini_model_from_path("/v1/models/gemini-2.5-pro:streamGenerateContent?alt=sse"),
            Some("gemini-2.5-pro".to_string())
        );
        assert_eq!(gemini_model_from_path("/v1/chat/completions"), None);
    }

    #[test]
    fn bedrock_model_extracted_and_decoded_from_path() {
        assert_eq!(
            bedrock_model_from_path("/model/us.anthropic.claude-sonnet-4-20250514-v1%3A0/converse"),
            Some("us.anthropic.claude-sonnet-4-20250514-v1:0".to_string())
        );
        assert_eq!(
            bedrock_model_from_path(
                "/model/arn%3Aaws%3Abedrock%3Aus-west-2%3A123%3Ainference-profile%2Fus.anthropic.claude-sonnet-4-v1%3A0/converse-stream"
            ),
            Some("us.anthropic.claude-sonnet-4-v1:0".to_string())
        );
        assert_eq!(bedrock_model_from_path("/v1/messages"), None);
    }

    #[test]
    fn loopback_guard_blocks_non_loopback_when_armed() {
        std::env::set_var("TARE_NETWORK_GUARD", "loopback");
        assert!(guard_allows("http://127.0.0.1:8080/v1"));
        assert!(guard_allows("http://127.42.0.9:8080/v1"));
        assert!(guard_allows("http://localhost/x"));
        assert!(guard_allows("http://[::1]:8080/v1"));
        assert!(!guard_allows("https://api.anthropic.com"));
        assert!(!guard_allows("http://169.254.169.254/latest")); // link-local metadata
        std::env::remove_var("TARE_NETWORK_GUARD");
    }

    #[test]
    fn ordinal_increment_reports_exhaustion_without_wrapping() {
        let mut ordinals = HashMap::new();
        ordinals.insert("last".to_string(), u32::MAX - 1);
        assert_eq!(increment_ordinal(&mut ordinals, "last"), Some(u32::MAX));
        assert_eq!(increment_ordinal(&mut ordinals, "last"), None);
        assert_eq!(ordinals["last"], u32::MAX);
    }

    #[test]
    fn compatible_budget_estimate_uses_the_vendor_pricing_dimension() {
        let pricing = tare_core::PricingTable::from_toml_str(include_str!(
            "../../pricing/pricing.fixture.toml"
        ))
        .unwrap();
        let request = serde_json::json!({ "model": "llama-3.1-70b" });
        assert_eq!(
            estimate_input_micros(
                Provider::OpenAiCompatible,
                Some("groq"),
                &request,
                4_000_000,
                &pricing,
            ),
            590_000
        );
        assert_eq!(
            estimate_input_micros(
                Provider::OpenAiCompatible,
                None,
                &request,
                4_000_000,
                &pricing,
            ),
            0
        );
    }

    #[test]
    fn reservation_guard_releases_on_drop_every_path() {
        // The leak fix: a reservation is returned when the guard drops, regardless of how the
        // request exits (this models the stream-error / client-disconnect paths that previously
        // bypassed the explicit release).
        let tallies: Arc<Mutex<HashMap<String, tare_core::budget::RunTally>>> =
            Arc::new(Mutex::new(HashMap::new()));
        {
            let mut g = tallies.lock().unwrap();
            let t = g.entry("r".into()).or_default();
            t.reserve(500);
            assert_eq!(t.reserved, 500);
        }
        {
            let _guard = ReservationGuard {
                tallies: tallies.clone(),
                run_id: "r".into(),
                micros: 500,
            };
            // ... request exits here (error/disconnect/normal) -> guard drops
        }
        assert_eq!(
            tallies.lock().unwrap().get("r").unwrap().reserved,
            0,
            "reservation must be released on drop"
        );
    }

    #[test]
    fn reservation_guard_zero_is_noop() {
        // No-budget path reserves 0 -> drop must not touch the tally map needlessly.
        let tallies: Arc<Mutex<HashMap<String, tare_core::budget::RunTally>>> =
            Arc::new(Mutex::new(HashMap::new()));
        {
            let _g = ReservationGuard {
                tallies: tallies.clone(),
                run_id: "r".into(),
                micros: 0,
            };
        }
        assert!(tallies.lock().unwrap().is_empty());
    }
}
