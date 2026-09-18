//! Core domain types shared across the workspace and exposed to the GUI as
//! serde-serializable view models. No I/O, no clock, no RNG.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Anthropic,
    Openai,
    /// Azure OpenAI: the OpenAI wire format on a per-resource endpoint + deployment path.
    #[serde(rename = "azure_openai")]
    AzureOpenai,
    /// AWS Bedrock Converse API (and InvokeModel aliases) — its own usage dialect.
    #[serde(rename = "bedrock_converse")]
    BedrockConverse,
    /// Google Gemini (Developer API + Vertex) — `usageMetadata` dialect.
    Gemini,
    /// A self-hosted / open-source model (Ollama, vLLM, llama.cpp, TGI, ...) on the user's own
    /// infra. Captured out-of-band by the homelab agent (never proxied), and unpriced by the
    /// bundled table (usage-first; an optional cost overlay is configured separately).
    Local,
    /// An OpenAI-compatible endpoint behind a free-text vendor label (OpenRouter, Groq, Together, …)
    /// — the market's universal adapter. One mode for the long tail instead of an enum
    /// variant per vendor; the specific vendor rides as `RequestShape.vendor` and composes the
    /// pricing key (see `pricing_key`), so Groq and Together price the same model name differently.
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::Openai => "openai",
            Provider::AzureOpenai => "azure_openai",
            Provider::BedrockConverse => "bedrock_converse",
            Provider::Gemini => "gemini",
            Provider::Local => "local",
            Provider::OpenAiCompatible => "openai_compatible",
        }
    }
    /// The provider dimension of the `(provider, model)` pricing key. For `OpenAiCompatible` the
    /// *vendor label* is that dimension (so `groq`/`together` price the same model differently);
    /// every other provider keys on `as_str()`.
    pub fn pricing_key<'a>(self, vendor: Option<&'a str>) -> std::borrow::Cow<'a, str> {
        match (self, vendor) {
            (Provider::OpenAiCompatible, Some(v)) if !v.is_empty() => {
                std::borrow::Cow::Owned(v.to_string())
            }
            _ => std::borrow::Cow::Borrowed(self.as_str()),
        }
    }
    /// Parse a wire/pricing provider tag (inverse of `as_str`).
    pub fn parse(s: &str) -> Option<Provider> {
        match s {
            "anthropic" => Some(Provider::Anthropic),
            "openai" => Some(Provider::Openai),
            "azure_openai" => Some(Provider::AzureOpenai),
            "bedrock_converse" => Some(Provider::BedrockConverse),
            "gemini" => Some(Provider::Gemini),
            "local" => Some(Provider::Local),
            "openai_compatible" => Some(Provider::OpenAiCompatible),
            _ => None,
        }
    }
}

/// TTL carried from the request's `cache_control.ttl`; selects the cache-write rate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheTtl {
    #[default]
    FiveMin,
    OneHour,
}

/// Cache class of a chunk of tokens — drives flamegraph color and accounting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheClass {
    Fresh,
    CacheWrite5m,
    CacheWrite1h,
    CacheRead,
    Output,
    Reasoning,
}

impl CacheClass {
    pub fn as_str(self) -> &'static str {
        match self {
            CacheClass::Fresh => "fresh",
            CacheClass::CacheWrite5m => "cache_write_5m",
            CacheClass::CacheWrite1h => "cache_write_1h",
            CacheClass::CacheRead => "cache_read",
            CacheClass::Output => "output",
            CacheClass::Reasoning => "reasoning",
        }
    }
    /// Stable hex color for SVG (no theme dependence).
    pub fn color(self) -> &'static str {
        match self {
            CacheClass::Fresh => "#4e79a7",
            CacheClass::CacheWrite5m => "#f28e2b",
            CacheClass::CacheWrite1h => "#e15759",
            CacheClass::CacheRead => "#59a14f",
            CacheClass::Output => "#b07aa1",
            CacheClass::Reasoning => "#9c755f",
        }
    }
}

/// Structural component of a request that input tokens are attributed to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Component {
    System,
    Tools,
    ToolResult,
    UserMessage,
    AssistantMessage,
    Output,
    /// The cached input prefix as a single honest aggregate when we CAN'T separate it (out-of-band
    /// JSONL/OTel lanes see only counts, not the request body) — it is system prompt + tool
    /// definitions + conversation history combined. Never claim it's just the system
    /// prompt: the system prompt is a tiny fixed part, and no system-vs-tools split is recoverable
    /// out-of-band.
    CachedPrefix,
    /// Fallback bucket for tokens that can't be structurally attributed (e.g. a request with
    /// no recognizable components). Never silently folded into System.
    Other,
}

impl Component {
    pub fn as_str(self) -> &'static str {
        match self {
            Component::System => "system",
            Component::Tools => "tools",
            Component::ToolResult => "tool_result",
            Component::UserMessage => "user_message",
            Component::AssistantMessage => "assistant_message",
            Component::Output => "output",
            Component::CachedPrefix => "cached_prefix",
            Component::Other => "other",
        }
    }
    pub fn parse(s: &str) -> Option<Component> {
        Some(match s {
            "system" => Component::System,
            "tools" => Component::Tools,
            "tool_result" => Component::ToolResult,
            "user_message" => Component::UserMessage,
            "assistant_message" => Component::AssistantMessage,
            "output" => Component::Output,
            "cached_prefix" => Component::CachedPrefix,
            "other" => Component::Other,
            _ => return None,
        })
    }
    pub fn label(self) -> &'static str {
        match self {
            Component::System => "System prompt",
            Component::Tools => "Tool definitions",
            Component::ToolResult => "Tool results",
            Component::UserMessage => "User messages",
            Component::AssistantMessage => "Assistant history",
            Component::Output => "Output",
            Component::CachedPrefix => "Cached prefix (system + tools + history)",
            Component::Other => "Other",
        }
    }
}

/// Normalized, provider-reported token counts for one logical request/response.
/// Counts only — never re-tokenized.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageTokens {
    /// Uncached input remainder (Anthropic `input_tokens`; OpenAI prompt − cached).
    pub fresh_input: u64,
    /// 5-minute cache write (`cache_creation.ephemeral_5m_input_tokens`). OpenAI: 0.
    pub cache_write_5m: u64,
    /// 1-hour cache write (`cache_creation.ephemeral_1h_input_tokens`). OpenAI: 0.
    pub cache_write_1h: u64,
    /// `cache_read_input_tokens` (Anthropic) / `cached_tokens` (OpenAI).
    pub cache_read: u64,
    /// Output tokens (Anthropic final cumulative; OpenAI `completion_tokens`). Includes reasoning.
    pub output: u64,
    /// Reasoning/thinking tokens (subset of `output`, billed as output, tracked as a cause).
    pub reasoning: u64,
    /// Multimodal sub-classes providers token-meter separately (OpenAI
    /// `prompt_tokens_details.audio_tokens` / `completion_tokens_details.audio_tokens`). Priced at
    /// their own rate when set, else $0 — additive, so text-only captures and goldens are
    /// unchanged. (Image/video are typically billed per-item, not per-token, so are not axes here.)
    #[serde(default)]
    pub audio_input: u64,
    #[serde(default)]
    pub audio_output: u64,
}

impl UsageTokens {
    /// Total cache-write tokens across both TTL tiers. Saturating so a corrupt/huge count
    /// can never wrap (consistent with the money path).
    pub fn cache_write(&self) -> u64 {
        self.cache_write_5m.saturating_add(self.cache_write_1h)
    }
    pub fn total_prompt(&self) -> u64 {
        self.fresh_input
            .saturating_add(self.cache_write())
            .saturating_add(self.cache_read)
    }
    pub fn total(&self) -> u64 {
        self.total_prompt().saturating_add(self.output)
    }
}

/// A component's weight (UTF-8 byte length of its canonical text) parsed from a request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentWeight {
    pub component: Component,
    pub bytes: u64,
}

/// One component in a step's prompt anatomy: structural byte-weight + whether it was the
/// cache-controlled component. Counts/labels only — no prompt text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AnatomyComponent {
    pub component: String,
    pub label: String,
    pub bytes: u64,
    pub cached: bool,
}

/// A privacy-safe "prompt anatomy" view of a step: the component byte-weights (largest first), a
/// flag on the cache-controlled component, and a SHORT stable system-prompt hash chip (so identical
/// system prompts across steps visibly share a chip). `system_hash` is `None` under `max_private`
/// (the UI shows "hash withheld"). Pure render data over already-stored `RequestShape` — no text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PromptAnatomy {
    pub components: Vec<AnatomyComponent>,
    pub total_bytes: u64,
    pub system_hash: Option<String>,
    /// Short stable fingerprint of the whole request shape. Withheld when the active privacy policy
    /// suppressed request hashing; never reversible to the request body.
    pub request_hash: Option<String>,
    /// Request/config facts already retained in `RequestShape`; these expose no payload text.
    pub stream: bool,
    pub cache_control: bool,
    /// Normalized request-intent cache TTL (`5m` or `1h`). Provider-reported usage remains the
    /// authoritative cache split for accounting.
    pub ttl: String,
    pub effort: Option<String>,
}

fn short_hash(hash: Option<u64>) -> Option<String> {
    hash.map(|value| format!("{value:016x}")[..8].to_string())
}

/// Build the prompt anatomy from a stored request shape. Deterministic: components are
/// sorted by byte-weight desc, then by component order for ties.
pub fn prompt_anatomy(shape: &RequestShape) -> PromptAnatomy {
    let mut components: Vec<AnatomyComponent> = shape
        .weights
        .iter()
        .map(|w| AnatomyComponent {
            component: w.component.as_str().to_string(),
            label: w.component.label().to_string(),
            bytes: w.bytes,
            cached: shape.cached_component == Some(w.component),
        })
        .collect();
    components.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.component.cmp(&b.component)));
    let total_bytes = components
        .iter()
        .fold(0u64, |sum, component| sum.saturating_add(component.bytes));
    PromptAnatomy {
        components,
        total_bytes,
        // Short, stable chip (8 hex chars) — identical system prompts share it. Withheld (None)
        // when the privacy profile suppressed the hash (max_private).
        system_hash: short_hash(shape.system_hash),
        request_hash: short_hash(shape.request_hash),
        stream: shape.stream,
        cache_control: shape.has_cache_control,
        ttl: if matches!(shape.ttl, CacheTtl::OneHour) {
            "1h".to_string()
        } else {
            "5m".to_string()
        },
        effort: shape.effort.clone(),
    }
}

/// Structural shape of a request, used for deterministic attribution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestShape {
    pub model: String,
    pub provider: Provider,
    pub stream: bool,
    /// Request-intent TTL hint. Only a FALLBACK for billing — the authoritative split now
    /// comes from the response `cache_creation.ephemeral_5m/1h_input_tokens` (see wire.rs).
    pub ttl: CacheTtl,
    /// True if the request marks a stable prefix for caching (`cache_control` present).
    pub has_cache_control: bool,
    /// The component that carried `cache_control` (cache write/read are attributed here).
    #[serde(default)]
    pub cached_component: Option<Component>,
    /// Stable hash of the system-prompt text (content-based bloat grouping; not byte length).
    #[serde(default)]
    pub system_hash: Option<u64>,
    pub weights: Vec<ComponentWeight>,
    /// Canonical hash of the request for retry-loop detection. `None` under the `max_private`
    /// privacy profile; retry detection then degrades to unavailable.
    #[serde(default)]
    pub request_hash: Option<u64>,
    /// Optional framework-adapter correlation labels (opaque SHORT strings — never payload;
    /// the proxy truncates them). Additive: omitted from `shape_json` when absent, so existing
    /// rows and every golden are byte-identical. `attempt` is the honest retry-vs-best-of-N fix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// Owning session/conversation id — promotes the agent TASK above individual runs, so spend
    /// across many api_request steps rolls up into one "what did this task cost?" receipt. Opaque
    /// correlation id (like `run_id`), never payload; omitted from `shape_json` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// User-provided opaque workload key: the caller's own stable grouping
    /// label for "the same job across runs" (proxy header `x-tare-workload-key` / OTel attribute
    /// `tare.workload_key`). Normalized + truncated to 64 UTF-8-safe chars at the capture edge;
    /// NEVER derived from payload contents. Optional — omitted from `shape_json` when absent, so
    /// existing rows and every golden stay byte-identical. Preferred cohort-compare match key when
    /// both cohorts carry it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_key: Option<String>,
    /// Reasoning effort level the request ran at (low/medium/high/xhigh/max) — a large, newly-
    /// tunable cost knob for coding agents. A short enum-like label, never payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// MCP server that originated this request, lifted from OTel span/log attributes
    /// (mcp.server.name / mcp_server.name / gen_ai.mcp.server). Answers "which MCP server is
    /// expensive?". A short opaque server id, never payload; omitted from `shape_json` when absent.
    /// Only the OTel path populates it — the proxy can't observe MCP identity, so proxy-captured
    /// steps fall into the unlabeled bucket under the McpServer dimension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_server: Option<String>,
    /// Free-text vendor/base-URL label for an `OpenAiCompatible` provider (groq, together,
    /// openrouter, …) — the pricing dimension paired with `model` (see `Provider::pricing_key`),
    /// and the honest display label so "OpenAI-compatible" calls aren't all mislabeled "openai".
    /// Opaque short label, never payload; omitted from `shape_json` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    /// Git commit SHA the working tree was on when this run executed. Short SHA;
    /// counts-only (never a diff/message). Powers `rollup --by commit` — git-blame-for-cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Git commit author (`%an`) — powers `rollup --by author`. Opaque short label, never payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

/// Optional framework-adapter correlation labels carried on `x-tare-*` request headers. All
/// fields are opaque short strings (the proxy truncates them), never payload text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StepMeta {
    pub step_label: Option<String>,
    pub component_label: Option<String>,
    pub parent_label: Option<String>,
    pub attempt: Option<u32>,
    /// User-provided opaque workload key (`x-tare-workload-key`). Truncated to 64 UTF-8-safe
    /// chars at the proxy, like the other opaque labels; never payload.
    pub workload_key: Option<String>,
    /// OpenAI-compatible vendor label set explicitly by the connect preset (proxy `x-tare-vendor`
    /// header) so the step prices on vendor+model instead of bucketing as bare "openai_compatible".
    /// Opaque short label, never payload.
    pub vendor: Option<String>,
    /// Git commit SHA / author of the working tree when this run executed. Capture is config-gated;
    /// values are stamped onto the shape so spend rolls up by commit/author.
    pub commit: Option<String>,
    pub author: Option<String>,
}

impl StepMeta {
    pub fn is_empty(&self) -> bool {
        self.step_label.is_none()
            && self.component_label.is_none()
            && self.parent_label.is_none()
            && self.attempt.is_none()
            && self.vendor.is_none()
            && self.workload_key.is_none()
    }
}

/// Unix nanoseconds since the epoch, carried as `u128` in Rust but ALWAYS serialized as a decimal
/// JSON *string*: JSON numbers lose precision past 2^53 and a signed-64 SQLite
/// `INTEGER` can't hold nanoseconds safely, so both the wire form and the TEXT column use the decimal
/// string — parsed to `u128` only here at the Rust boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnixNanos(pub u128);

impl UnixNanos {
    pub fn get(self) -> u128 {
        self.0
    }
    /// The decimal-string form persisted to the TEXT column / emitted on the wire.
    pub fn to_decimal_string(self) -> String {
        self.0.to_string()
    }
    /// Parse the decimal-string form read back from the TEXT column.
    pub fn parse(s: &str) -> Option<UnixNanos> {
        s.parse::<u128>().ok().map(UnixNanos)
    }
}

impl serde::Serialize for UnixNanos {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for UnixNanos {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = UnixNanos;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a decimal string of unix nanoseconds")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<UnixNanos, E> {
                v.parse::<u128>()
                    .map(UnixNanos)
                    .map_err(|_| E::custom(format!("invalid unix nanos: {v:?}")))
            }
            // Lenient number acceptance so a producer that emitted a JSON integer still reads back.
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<UnixNanos, E> {
                Ok(UnixNanos(v as u128))
            }
            fn visit_u128<E: serde::de::Error>(self, v: u128) -> Result<UnixNanos, E> {
                Ok(UnixNanos(v))
            }
        }
        d.deserialize_any(V)
    }
}

/// One captured request/response pair (a "step") within a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRecord {
    pub run_id: String,
    pub step_ordinal: u32,
    pub provider: Provider,
    pub model: String,
    pub usage: UsageTokens,
    pub shape: RequestShape,
    /// Response stop reason (Anthropic `stop_reason` / OpenAI `finish_reason`), or
    /// `Some("provider_error")` when the response was a provider error body. `None` when
    /// not present/parseable.
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// Observed local round-trip latency in milliseconds, measured at the capture EDGE (the proxy's
    /// monotonic `Instant` delta, or an OTel span's end−start). This is wall-clock-free in core: a
    /// number handed in from outside, never computed here. It is observed LOCAL latency (network +
    /// provider compute as seen from this machine), NOT a provider SLA. `0` = unmeasured or the
    /// `suppress_latency` privacy opt-out. Additive: omitted from JSON when 0 so goldens stay
    /// byte-identical.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub duration_ms: u64,
    /// Wall-clock start of the step (OTLP `startTimeUnixNano`), or `None` for proxy/legacy capture
    /// with no real timestamp — the UI then shows STEP ORDER, not a timeline.
    /// Serialized as a decimal string and persisted to a TEXT column (never unsafe signed-64).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_unix_nano: Option<UnixNanos>,
    /// OTLP trace identity, when captured out-of-band (proxy capture leaves it `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// OTLP span identity of this step, when captured out-of-band.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
    /// Parent span, when captured. This is the ONLY evidence for a parent/concurrency relationship —
    /// concurrency is shown solely when this (or overlap) is present, never inferred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

impl StepRecord {
    /// Display end instant = start + observed `duration_ms`: computed from the real start plus
    /// the observed duration rather than persisting a redundant, precision-unsafe end integer. `None`
    /// for step-order-only capture — a timeline is never fabricated without a real start timestamp.
    pub fn end_unix_nano(&self) -> Option<UnixNanos> {
        self.start_unix_nano.map(|start| {
            let duration_nanos = u128::from(self.duration_ms).saturating_mul(1_000_000);
            UnixNanos(start.0.saturating_add(duration_nanos))
        })
    }

    /// The response was a provider error / proxy parse failure (no usable result).
    pub fn is_error(&self) -> bool {
        matches!(
            self.stop_reason.as_deref(),
            Some("provider_error") | Some("error") | Some("parse_error")
        )
    }
    /// The model declined for safety reasons (`stop_reason: "refusal"`).
    pub fn is_refusal(&self) -> bool {
        self.stop_reason.as_deref() == Some("refusal")
    }
    /// No tokens were reported at all (e.g. a placeholder step). NOT a failure on its own —
    /// a legitimate empty completion is not a retry trigger.
    pub fn is_empty(&self) -> bool {
        self.usage.total() == 0
    }
    /// A re-issue after this step is a genuine retry only if it errored or was refused.
    pub fn is_retry_worthy_failure(&self) -> bool {
        self.is_error() || self.is_refusal()
    }
}

/// A run is an ordered sequence of steps sharing a correlation id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    pub steps: Vec<StepRecord>,
}

impl RunRecord {
    pub fn new(run_id: impl Into<String>) -> Self {
        RunRecord {
            run_id: run_id.into(),
            steps: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_anatomy_sorts_by_weight_marks_cached_and_shortens_hash() {
        let shape = RequestShape {
            model: "m".into(),
            provider: Provider::Anthropic,
            stream: false,
            ttl: CacheTtl::FiveMin,
            has_cache_control: true,
            cached_component: Some(Component::System),
            system_hash: Some(0xa1b2_c3d4_e5f6_0718),
            weights: vec![
                ComponentWeight {
                    component: Component::Tools,
                    bytes: 300,
                },
                ComponentWeight {
                    component: Component::System,
                    bytes: 620,
                },
                ComponentWeight {
                    component: Component::UserMessage,
                    bytes: 80,
                },
            ],
            request_hash: Some(0x1020_3040_5060_7080),
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
        };
        let a = prompt_anatomy(&shape);
        // Sorted by bytes desc: system (620) first, then tools, then user.
        assert_eq!(a.components[0].component, "system");
        assert!(a.components[0].cached); // the cache-controlled component
        assert!(!a.components[1].cached);
        assert_eq!(a.total_bytes, 1000);
        // Short, stable 8-hex chip (not the full 16).
        assert_eq!(a.system_hash.as_deref(), Some("a1b2c3d4"));
        assert_eq!(a.request_hash.as_deref(), Some("10203040"));
        assert!(!a.stream);
        assert!(a.cache_control);
        assert_eq!(a.ttl, "5m");
        assert_eq!(a.effort, None);
        // Withheld when the profile suppressed the hash (max_private).
        let mut redacted = shape.clone();
        redacted.system_hash = None;
        redacted.request_hash = None;
        assert_eq!(prompt_anatomy(&redacted).system_hash, None);
        assert_eq!(prompt_anatomy(&redacted).request_hash, None);
    }

    fn step(stop: Option<&str>, output: u64) -> StepRecord {
        StepRecord {
            run_id: "r".into(),
            step_ordinal: 1,
            provider: Provider::Anthropic,
            model: "m".into(),
            usage: UsageTokens {
                output,
                ..Default::default()
            },
            shape: RequestShape {
                model: "m".into(),
                provider: Provider::Anthropic,
                stream: false,
                ttl: CacheTtl::FiveMin,
                has_cache_control: false,
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
            stop_reason: stop.map(String::from),
            duration_ms: 0,
            start_unix_nano: None,
            trace_id: None,
            span_id: None,
            parent_span_id: None,
        }
    }

    #[test]
    fn retry_worthy_only_on_error_or_refusal() {
        // A successful but empty completion is NOT a retry trigger.
        assert!(!step(Some("end_turn"), 0).is_retry_worthy_failure());
        assert!(step(Some("end_turn"), 0).is_empty());
        // Errors and refusals are.
        assert!(step(Some("error"), 0).is_retry_worthy_failure());
        assert!(step(Some("parse_error"), 0).is_retry_worthy_failure());
        assert!(step(Some("provider_error"), 0).is_retry_worthy_failure());
        assert!(step(Some("refusal"), 5).is_retry_worthy_failure());
        // A normal answer is neither.
        assert!(!step(Some("end_turn"), 42).is_retry_worthy_failure());
    }

    #[test]
    fn derived_view_values_saturate_at_numeric_boundaries() {
        let mut shape = step(None, 0).shape;
        shape.weights = vec![
            ComponentWeight {
                component: Component::System,
                bytes: u64::MAX,
            },
            ComponentWeight {
                component: Component::Tools,
                bytes: 1,
            },
        ];
        assert_eq!(prompt_anatomy(&shape).total_bytes, u64::MAX);

        let mut timed = step(None, 0);
        timed.start_unix_nano = Some(UnixNanos(u128::MAX - 10));
        timed.duration_ms = u64::MAX;
        assert_eq!(timed.end_unix_nano(), Some(UnixNanos(u128::MAX)));
    }
}

// ---- View models (GUI / CLI boundary) ----

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodaySpend {
    pub run_count: u32,
    pub total_micros: i64,
    pub pricing_version: String,
    pub effective_date: String,
}
