//! OpenTelemetry (OTLP/JSON) ingest + export — a pure sibling of `speedscope`. COUNTS ONLY:
//! like the rest of Tare, nothing here reads or emits payload text. Ingest reads GenAI
//! semantic-convention spans into degraded `StepRecord`s (token counts, but NO structural
//! component attribution — honestly marked); export emits one span per step with `gen_ai.*`
//! counts + `tare.*` cost. No clock (synthetic fixed timestamps), no network (any off-box
//! export endpoint must route through the proxy's `guard_allows`).

use crate::account::cost_usage;
use crate::attribute::build_report;
use crate::canon::fnv1a_64;
use crate::model::{Provider, RequestShape, RunRecord, StepRecord, UsageTokens};
use crate::pricing::PricingTable;
use crate::privacy::PrivacyPolicy;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// A GenAI span reduced to the counts Tare accounts from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtelStep {
    pub provider: Provider,
    pub model: String,
    pub usage: UsageTokens,
    pub finish_reason: Option<String>,
    pub trace_id: String,
    pub span_id: String,
    /// Parent span id (OTLP `parentSpanId`), when the export carries it — the sole evidence for a
    /// parent/concurrency relationship. `None` for a root/attribute-less span.
    pub parent_span_id: Option<String>,
    pub start_unix_nano: u128,
    /// Observed span latency in ms (end−start), computed at ingest from the export. 0 if the span
    /// carries no end time.
    pub duration_ms: u64,
    /// Correlation labels lifted from GenAI/Claude-Code attributes so the agent dimension lights
    /// up in rollup/segments without the inline proxy: agent/subagent -> parent, tool/skill ->
    /// component, operation -> step. The owning session/conversation groups runs into a task.
    pub agent: Option<String>,
    pub tool: Option<String>,
    pub operation: Option<String>,
    pub session: Option<String>,
    pub effort: Option<String>,
    pub mcp_server: Option<String>,
    /// User-provided workload key: lifted from `tare.workload_key` (or an
    /// allowed alias). Opaque grouping label, never payload.
    pub workload_key: Option<String>,
    /// For self-hosted providers, the specific backend (`ollama`/`vllm`/`llama.cpp`/…), preserved
    /// as the step's `vendor` so local runs keep per-backend identity instead of flattening to
    /// `local` — and so a user-supplied local pricing overlay can key rates per backend.
    pub vendor: Option<String>,
}

/// The specific self-hosted backend behind `Provider::Local`, normalized, or `None` for the generic
/// `local` tag / non-local providers. Kept as the step's vendor label.
fn local_backend_label(prov_str: &str) -> Option<String> {
    match prov_str {
        "vllm" => Some("vllm".to_string()),
        "ollama" => Some("ollama".to_string()),
        "llama.cpp" | "llamacpp" => Some("llama.cpp".to_string()),
        "tgi" => Some("tgi".to_string()),
        "lmstudio" => Some("lmstudio".to_string()),
        "localai" => Some("localai".to_string()),
        _ => None, // "local" (unspecified) or not a self-hosted backend
    }
}

fn provider_from_semconv(s: &str) -> Option<Provider> {
    match s {
        "anthropic" => Some(Provider::Anthropic),
        "openai" => Some(Provider::Openai),
        "azure.ai.openai" | "azure_openai" | "azure" => Some(Provider::AzureOpenai),
        // `bedrock_converse` is Tare's own internal as_str() tag — accept it so our own
        // export -> ingest round-trip works, alongside the spec's `aws.bedrock`.
        "aws.bedrock" | "bedrock" | "bedrock_converse" => Some(Provider::BedrockConverse),
        "gcp.gen_ai" | "gcp.vertex_ai" | "gemini" | "google" => Some(Provider::Gemini),
        // Self-hosted and open-source servers used by the homelab agent.
        "local" | "vllm" | "ollama" | "llama.cpp" | "llamacpp" | "tgi" | "lmstudio" | "localai" => {
            Some(Provider::Local)
        }
        _ => None,
    }
}

/// The OTel GenAI semconv well-known `gen_ai.provider.name` value for a provider — used on EXPORT
/// so Tare emits spec-compliant output a foreign collector understands (the internal `as_str()`
/// enum tags like `bedrock_converse` are NOT semconv values). `provider_from_semconv` still
/// accepts both the spec values and Tare's legacy tags on ingest.
fn provider_to_semconv(p: Provider) -> &'static str {
    match p {
        Provider::Anthropic => "anthropic",
        Provider::Openai => "openai",
        Provider::AzureOpenai => "azure.ai.openai",
        Provider::BedrockConverse => "aws.bedrock",
        Provider::Gemini => "gcp.gen_ai",
        Provider::Local => "local",
        // OTel's gen_ai.system has no "openai_compatible" value; emit the wire dialect it speaks.
        // (The specific vendor lives on the step's `vendor` label, not the semconv system tag.)
        Provider::OpenAiCompatible => "openai",
    }
}

/// Read `gen_ai.response.finish_reasons` per the semconv (a `string[]`): accept the spec's
/// `arrayValue` (take the first reason) AND a bare `stringValue` for tolerance / legacy producers.
fn finish_reasons_first(v: &Value) -> Option<String> {
    if let Some(s) = attr_str(v) {
        return Some(s.to_string());
    }
    v.get("arrayValue")?
        .get("values")?
        .as_array()?
        .iter()
        .find_map(attr_str)
        .map(str::to_string)
}

/// Read an OTLP attribute value as u64, accepting `intValue` as a JSON number OR a decimal
/// string (proto3-JSON encodes int64 as a string).
fn attr_u64(v: &Value) -> Option<u64> {
    let iv = v.get("intValue")?;
    if let Some(n) = iv.as_u64() {
        return Some(n);
    }
    iv.as_str().and_then(|s| s.parse().ok())
}

fn attr_str(v: &Value) -> Option<&str> {
    v.get("stringValue").and_then(|s| s.as_str())
}

fn int64_count(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// First present, non-empty string value among `keys` in an attribute map. Used to lift
/// agent/tool/operation/session correlation labels from whichever attribute name a producer uses.
fn first_attr(attrs: &BTreeMap<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| attrs.get(*k).and_then(attr_str))
        .filter(|s| !s.is_empty())
        // Cap the lifted label like the JSONL + git lanes: an oversized OTLP
        // attribute must not land uncapped in the counts DB + shareable exports.
        .map(|s| s.chars().take(256).collect())
}

// Attribute-name candidates per correlation label (OTel GenAI semconv + Claude Code vendor names).
const AGENT_KEYS: &[&str] = &[
    "gen_ai.agent.name",
    "agent.name",
    "subagent.name",
    "query_source",
];
const TOOL_KEYS: &[&str] = &[
    "gen_ai.tool.name",
    "tool_name",
    "tool.name",
    "mcp_tool.name",
    "skill.name",
];
const OP_KEYS: &[&str] = &["gen_ai.operation.name", "operation.name"];
const SESSION_KEYS: &[&str] = &[
    "gen_ai.conversation.id",
    "session.id",
    "conversation.id",
    "session_id",
];
const EFFORT_KEYS: &[&str] = &[
    "gen_ai.request.reasoning_effort",
    "reasoning_effort",
    "effort",
];
/// The MCP server that originated a request. `mcp_tool.name` already feeds TOOL_KEYS (the component
/// label), but the owning server was never its own dimension. Deliberately specific
/// names only — no generic `server.name` fallback, which would mis-bucket a collector/host as an
/// MCP server.
const MCP_SERVER_KEYS: &[&str] = &["mcp.server.name", "mcp_server.name", "gen_ai.mcp.server"];
/// User-provided workload key. Primary is the `tare.workload_key` extension;
/// a bare `workload_key` is accepted as a producer-friendly alias. No GenAI semantic-convention
/// name exists for this concept, so there is no upstream alias to fold in.
const WORKLOAD_KEY_KEYS: &[&str] = &["tare.workload_key", "workload_key"];
/// Claude Code stamps `terminal.type`/`query_source` on its metric points to distinguish the
/// user's main query from subagent/auxiliary spend — $/outcome ratios filter to `main`.
const QUERY_SOURCE_KEYS: &[&str] = &["query_source", "source", "session.type", "terminal.type"];

/// Flatten a span's `attributes: [{key, value}]` into a key -> value map.
fn attr_map(span: &Value) -> BTreeMap<String, Value> {
    let mut m = BTreeMap::new();
    if let Some(arr) = span.get("attributes").and_then(|a| a.as_array()) {
        for a in arr {
            if let (Some(k), Some(v)) = (a.get("key").and_then(|x| x.as_str()), a.get("value")) {
                m.insert(k.to_string(), v.clone());
            }
        }
    }
    m
}

/// One step read from a span under a capture convention — the shared shape every
/// convention normalizes to before the loop builds the `OtelStep`.
struct ConventionStep {
    provider: Provider,
    model: String,
    usage: UsageTokens,
    finish_reason: Option<String>,
    /// Raw provider token, for the local-backend vendor tag.
    prov_label: String,
}

/// A per-agent/dialect capture convention (ccusage-style). Each owns ONE instrumentation
/// dialect's token semantics — crucially the inclusive-vs-exclusive prompt split — feeding the shared
/// StepRecord path. `read` returns `Ok(Some)` when it claims the span, `Ok(None)` when the span isn't
/// its dialect (try the next), and `Err` when the span IS its dialect but malformed (e.g. an unknown
/// gen_ai provider — fail loudly, never silently misattribute). New agents/dialects (gemini_cli-native,
/// ccusage, file-tailers) add an impl + a slot in `read_step` without touching the ingest loop.
trait UsageConvention {
    fn read(&self, attrs: &BTreeMap<String, Value>) -> Result<Option<ConventionStep>, String>;
}

/// The OTel GenAI semconv. `input_tokens` EXCLUDES cache; unknown provider is a hard error.
struct GenAiConvention;
impl UsageConvention for GenAiConvention {
    fn read(&self, attrs: &BTreeMap<String, Value>) -> Result<Option<ConventionStep>, String> {
        // Claimed iff a provider key is present (new or deprecated name).
        let Some(prov_str) = attrs
            .get("gen_ai.provider.name")
            .and_then(attr_str)
            .or_else(|| attrs.get("gen_ai.system").and_then(attr_str))
        else {
            return Ok(None);
        };
        let provider = provider_from_semconv(prov_str)
            .ok_or_else(|| format!("otlp: unknown gen_ai provider {prov_str:?}"))?;
        let model = attrs
            .get("gen_ai.response.model")
            .and_then(attr_str)
            .or_else(|| attrs.get("gen_ai.request.model").and_then(attr_str))
            .unwrap_or("unknown")
            .to_string();
        let get = |k: &str| attrs.get(k).and_then(attr_u64).unwrap_or(0);
        // The GenAI semconv is at "Development" stability and the token attribute names already churned
        // once (prompt/completion -> input/output). Accept both so an older library still attributes.
        let get_any = |ks: &[&str]| {
            ks.iter()
                .find_map(|k| attrs.get(*k).and_then(attr_u64))
                .unwrap_or(0)
        };
        // Standard GenAI semconv has a single combined cache-creation count. Tare adds the
        // `tare.usage.cache_creation_1h.input_tokens` extension so a Tare->Tare round-trip preserves the
        // 1h tier (Anthropic 1h ≠ 5m rate); foreign OTel without it degrades to all-5m (attribute is 0).
        let cache_create = get("gen_ai.usage.cache_creation.input_tokens");
        let cache_1h = get("tare.usage.cache_creation_1h.input_tokens").min(cache_create);
        let output = get_any(&[
            "gen_ai.usage.output_tokens",
            "gen_ai.usage.completion_tokens",
        ]);
        let usage = UsageTokens {
            fresh_input: get_any(&["gen_ai.usage.input_tokens", "gen_ai.usage.prompt_tokens"]),
            cache_write_5m: cache_create.saturating_sub(cache_1h),
            cache_write_1h: cache_1h,
            cache_read: get("gen_ai.usage.cache_read.input_tokens"),
            output,
            reasoning: get("gen_ai.usage.reasoning.output_tokens").min(output),
            // Tare extensions preserve modality-specific counts across Tare -> OTLP -> Tare.
            // The generic GenAI token fields have no portable audio-detail axis.
            audio_input: get("tare.usage.audio_input_tokens"),
            audio_output: get("tare.usage.audio_output_tokens"),
        };
        let finish_reason = attrs
            .get("gen_ai.response.finish_reasons")
            .and_then(finish_reasons_first);
        Ok(Some(ConventionStep {
            provider,
            model,
            usage,
            finish_reason,
            prov_label: prov_str.to_string(),
        }))
    }
}

/// The OpenInference (Arize) convention (delegates to `openinference_step`, which owns the
/// inclusive→exclusive split). Unknown provider / non-LLM span → `Ok(None)` (skip, not an error).
struct OpenInferenceConvention;
impl UsageConvention for OpenInferenceConvention {
    fn read(&self, attrs: &BTreeMap<String, Value>) -> Result<Option<ConventionStep>, String> {
        Ok(
            openinference_step(attrs).map(|(provider, model, usage, finish_reason, prov_label)| {
                ConventionStep {
                    provider,
                    model,
                    usage,
                    finish_reason,
                    prov_label,
                }
            }),
        )
    }
}

/// Try each capture convention in order (gen_ai first — native + Tare's own export — then
/// OpenInference), returning the first that claims the span, or `None` if none does.
fn read_step(attrs: &BTreeMap<String, Value>) -> Result<Option<ConventionStep>, String> {
    let conventions: [&dyn UsageConvention; 2] = [&GenAiConvention, &OpenInferenceConvention];
    for c in conventions {
        if let Some(step) = c.read(attrs)? {
            return Ok(Some(step));
        }
    }
    Ok(None)
}

/// OpenInference (Arize) LLM-span token convention → the shared 6-axis `UsageTokens`.
///
/// MONEY HAZARD this exists to avoid: OpenInference's `llm.token_count.prompt` is INCLUSIVE of ALL its
/// `prompt_details` subsets — `cache_read`, `cache_write` (creation), and `audio` — whereas gen_ai's
/// `input_tokens` EXCLUDES cache. Mapping `prompt` straight to `fresh_input` while ALSO counting
/// `cache_read`/`cache_write` separately would double-count those tokens — a fabricated over-estimate
/// (violates estimate-honesty #6). So the split is convention-aware:
/// `fresh_input = prompt − cache_read − cache_write − audio` and `output = completion − reasoning −
/// audio` (saturating, so a malformed span can't underflow). This keeps the input subsets summing back
/// to the original `prompt`: fresh_input + cache_read + cache_write + audio == prompt.
///
/// Returns `None` for spans without OpenInference token markers or an unknown provider → the caller
/// skips them (honest GAP), never a wrong capture. Tuple mirrors the gen_ai arm:
/// `(provider, model, usage, finish_reason, provider_label)`.
fn openinference_step(
    attrs: &BTreeMap<String, Value>,
) -> Option<(Provider, String, UsageTokens, Option<String>, String)> {
    let prompt = attrs.get("llm.token_count.prompt").and_then(attr_u64);
    let completion = attrs.get("llm.token_count.completion").and_then(attr_u64);
    // An OpenInference LLM span carries a provider AND at least one token count; require both so we
    // never mis-claim a non-LLM span.
    let prov_str = attrs
        .get("llm.provider")
        .and_then(attr_str)
        .or_else(|| attrs.get("llm.system").and_then(attr_str))?;
    if prompt.is_none() && completion.is_none() {
        return None;
    }
    let provider = provider_from_semconv(prov_str)?;
    let model = attrs
        .get("llm.model_name")
        .and_then(attr_str)
        .unwrap_or("unknown")
        .to_string();
    let g = |k: &str| attrs.get(k).and_then(attr_u64).unwrap_or(0);
    let prompt = prompt.unwrap_or(0);
    let completion = completion.unwrap_or(0);
    let cache_read = g("llm.token_count.prompt_details.cache_read");
    let cache_write = g("llm.token_count.prompt_details.cache_write");
    let audio_in = g("llm.token_count.prompt_details.audio");
    let reasoning = g("llm.token_count.completion_details.reasoning");
    let audio_out = g("llm.token_count.completion_details.audio");
    let prompt_details = u128::from(cache_read) + u128::from(cache_write) + u128::from(audio_in);
    if prompt_details > u128::from(prompt) || audio_out > completion {
        return None;
    }
    let text_output = completion - audio_out;
    let usage = UsageTokens {
        // Strip the cached + audio subsets out of the INCLUSIVE prompt total (see MONEY HAZARD above).
        // prompt is inclusive of cache_read + cache_write (creation) + audio — subtract ALL three so
        // fresh_input is the genuinely-uncached remainder and the creation tokens aren't double-counted
        // against cache_write_5m below.
        fresh_input: prompt
            .saturating_sub(cache_read)
            .saturating_sub(cache_write)
            .saturating_sub(audio_in),
        // OpenInference has no 5m/1h cache-tier split; treat any cache-write as 5m (the safe default).
        cache_write_5m: cache_write,
        cache_write_1h: 0,
        cache_read,
        // Output stays INCLUSIVE of reasoning (reasoning is billed as output and only broken out for
        // display — matching the gen_ai/OpenAI/Anthropic parsers, which `cost_from_usage` bills via the
        // whole `output`). Only the separately-priced audio subset is stripped out. Subtracting
        // reasoning here would UNDER-bill reasoning tokens.
        output: text_output,
        reasoning: reasoning.min(text_output),
        audio_input: audio_in,
        audio_output: audio_out,
    };
    let finish_reason = attrs
        .get("llm.finish_reason")
        .and_then(attr_str)
        .map(str::to_string);
    Some((provider, model, usage, finish_reason, prov_str.to_string()))
}

/// Parse OTLP/JSON trace export bytes into ordered GenAI `OtelStep`s. Non-GenAI spans are
/// skipped; an unknown provider is an error (never silently misattributed). Ordered by
/// `(start_unix_nano, span_id)` for determinism.
pub fn ingest_otlp_json(bytes: &[u8]) -> Result<Vec<OtelStep>, String> {
    let root: Value = serde_json::from_slice(bytes).map_err(|e| format!("otlp json: {e}"))?;
    let mut out = Vec::new();
    let resource_spans = root
        .get("resourceSpans")
        .and_then(|x| x.as_array())
        .ok_or("otlp: missing resourceSpans")?;
    for rs in resource_spans {
        let Some(scopes) = rs.get("scopeSpans").and_then(|x| x.as_array()) else {
            continue;
        };
        for sc in scopes {
            let Some(spans) = sc.get("spans").and_then(|x| x.as_array()) else {
                continue;
            };
            for span in spans {
                let attrs = attr_map(span);
                // Convention dispatch: each capture convention owns its dialect's token
                // semantics (crucially the inclusive-vs-exclusive prompt split) and maps a span to the
                // shared 6-axis usage. `read_step` tries them in order; a span no convention claims is
                // skipped (honest GAP, never a wrong capture). A convention that claims a span but finds
                // it malformed (unknown gen_ai provider) errors loudly via `?`.
                let Some(ConventionStep {
                    provider,
                    model,
                    usage,
                    finish_reason,
                    prov_label,
                }) = read_step(&attrs)?
                else {
                    continue;
                };
                let start_unix_nano = attrs
                    .get("gen_ai.tare.start_unix_nano")
                    .and_then(attr_u64)
                    .map(u128::from)
                    .or_else(|| {
                        span.get("startTimeUnixNano")
                            .and_then(|x| x.as_str())
                            .and_then(|s| s.parse().ok())
                    })
                    .unwrap_or(0);
                // Observed latency = span end−start, in ms. Pure arithmetic on counts handed in
                // from the export — no clock is read here. Tare's own end attribute wins;
                // else the OTLP `endTimeUnixNano`. Saturating sub guards a malformed/clock-skewed
                // export (end < start -> 0).
                let end_unix_nano = attrs
                    .get("gen_ai.tare.end_unix_nano")
                    .and_then(attr_u64)
                    .map(u128::from)
                    .or_else(|| {
                        span.get("endTimeUnixNano")
                            .and_then(|x| x.as_str())
                            .and_then(|s| s.parse().ok())
                    });
                let duration_ms = end_unix_nano
                    .map(|e| {
                        u64::try_from(e.saturating_sub(start_unix_nano) / 1_000_000)
                            .unwrap_or(u64::MAX)
                    })
                    .unwrap_or(0);
                let span_id = span
                    .get("spanId")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let trace_id = span
                    .get("traceId")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                // Parent span: the only evidence for a parent/concurrency relationship. Absent
                // or empty → None (a root span), never inferred.
                let parent_span_id = span
                    .get("parentSpanId")
                    .and_then(|x| x.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());
                out.push(OtelStep {
                    provider,
                    model,
                    usage,
                    finish_reason,
                    trace_id,
                    span_id,
                    parent_span_id,
                    start_unix_nano,
                    duration_ms,
                    agent: first_attr(&attrs, AGENT_KEYS),
                    tool: first_attr(&attrs, TOOL_KEYS),
                    operation: first_attr(&attrs, OP_KEYS),
                    session: first_attr(&attrs, SESSION_KEYS),
                    effort: first_attr(&attrs, EFFORT_KEYS),
                    mcp_server: first_attr(&attrs, MCP_SERVER_KEYS),
                    // Truncated to 64 UTF-8-safe chars so an oversized attribute can't smuggle
                    // payload, mirroring the proxy header treatment.
                    workload_key: first_attr(&attrs, WORKLOAD_KEY_KEYS)
                        .map(|s| s.chars().take(64).collect()),
                    vendor: local_backend_label(&prov_label),
                });
            }
        }
    }
    out.sort_by(|a, b| {
        a.start_unix_nano
            .cmp(&b.start_unix_nano)
            .then(a.span_id.cmp(&b.span_id))
    });
    Ok(out)
}

/// Convert ingested OTel spans into DEGRADED `StepRecord`s: real token counts, but empty
/// structural weights (no component attribution is available from a span) — so the
/// flamegraph is coarse but the dollars are exact. Grouped into runs by `traceId`. The
/// privacy policy is applied so the degraded shape is redacted at construction.
pub fn otel_steps_to_records(steps: &[OtelStep], policy: &PrivacyPolicy) -> Vec<StepRecord> {
    let mut per_trace: BTreeMap<String, u32> = BTreeMap::new();
    let mut out = Vec::new();
    for s in steps {
        let run_id = if s.trace_id.is_empty() {
            "otel".to_string()
        } else {
            s.trace_id.clone()
        };
        let ord = per_trace.entry(run_id.clone()).or_insert(0);
        *ord = ord.saturating_add(1);
        let shape = RequestShape {
            model: s.model.clone(),
            provider: s.provider,
            stream: false,
            ttl: crate::model::CacheTtl::FiveMin,
            has_cache_control: s.usage.cache_read > 0 || s.usage.cache_write() > 0,
            cached_component: None,
            // Degraded: no request body to hash or weigh; honestly empty.
            system_hash: None,
            weights: Vec::new(),
            // No request body was observed, so we cannot assert two steps are identical. Leave
            // the hash None (NOT hash(Null), which would make every degraded step collide and be
            // mis-attributed to a retry-loop). Retry detection is genuinely unavailable here.
            request_hash: None,
            // Agent correlation lifted from span attributes: operation becomes the step,
            // tool/skill -> component, agent/subagent -> parent. This is what lights up
            // rollup/segments on the out-of-band path without the inline proxy.
            step_label: s.operation.clone(),
            component_label: s.tool.clone(),
            parent_label: s.agent.clone(),
            attempt: None,
            session: s.session.clone(),
            effort: s.effort.clone(),
            mcp_server: s.mcp_server.clone(),
            workload_key: s.workload_key.clone(),
            // Self-hosted backend identity: ollama/vllm/llama.cpp/… preserved so local
            // runs don't flatten to "local" and a user's local pricing overlay can key per backend.
            // OpenAI-compatible vendor auto-capture requires separate binding work.
            vendor: s.vendor.clone(),
            // Git attribution isn't carried on OTLP spans; the proxy/run path stamps it.
            commit: None,
            author: None,
        };
        // Timing is suppressed under the latency privacy opt-out (a wall-clock start reveals *when*
        // you worked, like latency). Span identity (trace/span/parent) is opaque correlation, not
        // timing, so it's preserved regardless — it's what gates honest concurrency display.
        let record_timing = policy.should_record_latency();
        out.push(StepRecord {
            run_id,
            step_ordinal: *ord,
            provider: s.provider,
            model: s.model.clone(),
            usage: s.usage,
            shape,
            stop_reason: s.finish_reason.clone(),
            duration_ms: if record_timing { s.duration_ms } else { 0 },
            start_unix_nano: (record_timing && s.start_unix_nano > 0)
                .then_some(crate::model::UnixNanos(s.start_unix_nano)),
            trace_id: (!s.trace_id.is_empty()).then(|| s.trace_id.clone()),
            span_id: (!s.span_id.is_empty()).then(|| s.span_id.clone()),
            parent_span_id: s.parent_span_id.clone(),
        });
    }
    out
}

/// True for Claude Code OTel events that would carry raw prompt/response TEXT (`api_request_body`,
/// `api_response_body`, with or without a `claude_code.` prefix, plus any future `*_body` variant).
/// Tare's counts-only invariant is defended IN CODE, not just by the exporter default: these events
/// are dropped at ingest — never mapped to a step, never used for liveness, their text never read —
/// so even a misconfigured `OTEL_LOG_USER_PROMPTS`/`api_request_body` exporter can't leak payload
/// into the store.
pub fn is_payload_bearing_event(name: &str) -> bool {
    let n = name.strip_prefix("claude_code.").unwrap_or(name);
    n.ends_with("_body")
}

/// Parse OTLP/JSON **logs** export bytes and map Claude Code `api_request` events into degraded
/// `StepRecord`s — per-request token counts straight from Claude Code's own out-of-band telemetry
/// (no proxy). Non-`api_request` events (hooks, prompts, responses) are ignored. Runs are grouped
/// by `session.id`; `event.sequence` is the step ordinal. Provider is always Anthropic. Schema
/// verified against a real Claude Code v2.1.x OTLP/JSON logs export.
pub fn ingest_otlp_logs_json(
    bytes: &[u8],
    _policy: &PrivacyPolicy,
) -> Result<Vec<StepRecord>, String> {
    let root: Value = serde_json::from_slice(bytes).map_err(|e| format!("otlp logs json: {e}"))?;
    let resource_logs = root
        .get("resourceLogs")
        .and_then(|x| x.as_array())
        .ok_or("otlp logs: missing resourceLogs")?;
    let mut out = Vec::new();
    for rl in resource_logs {
        let Some(scopes) = rl.get("scopeLogs").and_then(|x| x.as_array()) else {
            continue;
        };
        for sc in scopes {
            let Some(records) = sc.get("logRecords").and_then(|x| x.as_array()) else {
                continue;
            };
            let mut codex_seq: u32 = 0; // fallback ordinal for Codex (no event.sequence)
            for lr in records {
                let attrs = attr_map(lr);
                match attrs.get("event.name").and_then(attr_str) {
                    // Privacy invariant defended IN CODE: a payload-bearing event
                    // (raw prompt/response body) is dropped at ingest even if a misconfigured
                    // exporter emits one — its text is never read, never stored, never a step.
                    Some(name) if is_payload_bearing_event(name) => {}
                    Some("api_request") => out.push(log_step_claude_code(&attrs)),
                    // Codex CLI reports token usage on codex.sse_event(response.completed).
                    Some("codex.sse_event")
                        if attrs.get("event.kind").and_then(attr_str)
                            == Some("response.completed") =>
                    {
                        out.push(log_step_codex(&attrs, &mut codex_seq));
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(out)
}

/// A non-cost liveness signal from a log event (Claude Code `user_prompt` / `tool_result`). Lets
/// the Live view distinguish "agent working" from "waiting on the user" WITHOUT minting a cost
/// `StepRecord` — so the cost path (`ingest_otlp_logs_json`) stays byte-stable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PulseKind {
    /// The user submitted a prompt — the agent is about to work.
    UserPrompt,
    /// A tool returned a result — the agent is mid-task.
    ToolResult,
}

impl PulseKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PulseKind::UserPrompt => "user_prompt",
            PulseKind::ToolResult => "tool_result",
        }
    }
}

/// One liveness pulse: which session, and what kind of activity (no tokens, no cost).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LivenessPulse {
    pub session: Option<String>,
    pub kind: PulseKind,
}

/// Scan an OTLP/JSON logs export for liveness-only events (`user_prompt` / `tool_result`),
/// returning a pulse per match. Deliberately separate from `ingest_otlp_logs_json` so the cost
/// path's bytes never change; the receiver calls both over the same body. Cost events
/// (`api_request`, `codex.sse_event`) are ignored here — they already beat liveness as steps.
pub fn ingest_otlp_logs_pulses(bytes: &[u8]) -> Result<Vec<LivenessPulse>, String> {
    let root: Value = serde_json::from_slice(bytes).map_err(|e| format!("otlp logs json: {e}"))?;
    let resource_logs = root
        .get("resourceLogs")
        .and_then(|x| x.as_array())
        .ok_or("otlp logs: missing resourceLogs")?;
    let mut out = Vec::new();
    for rl in resource_logs {
        let Some(scopes) = rl.get("scopeLogs").and_then(|x| x.as_array()) else {
            continue;
        };
        for sc in scopes {
            let Some(records) = sc.get("logRecords").and_then(|x| x.as_array()) else {
                continue;
            };
            for lr in records {
                let attrs = attr_map(lr);
                let kind = match attrs.get("event.name").and_then(attr_str) {
                    // Defended drop: a payload-bearing event never even beats
                    // liveness — its text is not a signal we consume.
                    Some(name) if is_payload_bearing_event(name) => continue,
                    Some("user_prompt") => PulseKind::UserPrompt,
                    Some("tool_result") => PulseKind::ToolResult,
                    _ => continue,
                };
                out.push(LivenessPulse {
                    session: first_attr(&attrs, SESSION_KEYS),
                    kind,
                });
            }
        }
    }
    Ok(out)
}

/// One aggregate metric data point mapped to Tare's "metered series". These are
/// vendor-reported aggregates (Claude Code's own `claude_code.cost.usage` / `token.usage`
/// counters), stored separately from per-step rows and surfaced as a cross-check — NEVER folded
/// into the step-derived totals (that would double-count when logs are also enabled).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeteredPoint {
    /// UTC day (`YYYY-MM-DD`) from the data point's `timeUnixNano`.
    pub day: String,
    /// `cost` (micro-USD) | `token` (count) | outcome counters `lines_of_code` | `pull_request` |
    /// `commit` | `session` | `active_time` (seconds) | `edit_decision` (count).
    pub metric: String,
    /// Model attribute, or empty.
    pub model: String,
    /// The metric's discriminator: token `type`, lines/active_time `type`, edit `decision`; else "".
    pub kind: String,
    /// Owning session id, or empty.
    pub session: String,
    /// Delta value for this export (micro-USD for cost, tokens for token). Always > 0.
    pub value: i64,
    /// Reasoning effort (`low`|`medium`|`high`|`xhigh`|`max`), or empty.
    pub effort: String,
    /// Query source (`main`|`subagent`|…) — $/outcome ratios filter to `main` per the Claude Code
    /// monitoring docs; empty when the producer didn't stamp it.
    pub query_source: String,
}

/// Read an int64 data-point value (OTLP/JSON encodes int64 as a string, but tolerate a number).
fn dp_int(dp: &Value) -> i64 {
    if let Some(s) = dp.get("asInt").and_then(|v| v.as_str()) {
        return s.parse().unwrap_or(0);
    }
    if let Some(n) = dp.get("asInt").and_then(|v| v.as_i64()) {
        return n;
    }
    dp.get("asDouble").and_then(|v| v.as_f64()).unwrap_or(0.0) as i64
}

fn is_cumulative_sum(sum: &Value) -> bool {
    match sum.get("aggregationTemporality") {
        Some(v) if v.as_i64() == Some(2) => true,
        Some(v) => matches!(
            v.as_str(),
            Some("2" | "AGGREGATION_TEMPORALITY_CUMULATIVE" | "CUMULATIVE")
        ),
        None => false,
    }
}

/// Local-day string (`YYYY-MM-DD`) from an OTLP `timeUnixNano` (string or number), shifted by
/// `offset_minutes` so a metered point buckets on the SAME calendar day as the local-day estimate
/// it's reconciled against (`0` means UTC). Empty if absent or unparseable.
fn nanos_to_day(v: &Value, offset_minutes: i64) -> String {
    let nanos: u64 = v
        .as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| v.as_u64())
        .unwrap_or(0);
    if nanos == 0 {
        return String::new();
    }
    crate::calendar::civil_date_for((nanos / 1_000_000_000) as i64, offset_minutes)
}

/// Parse an OTLP/JSON metrics export into Tare metered points, keeping only Claude Code's
/// `cost.usage` / `token.usage` counters. Assumes **delta** temporality (Claude Code's default),
/// so each data point is an increment that the store simply sums — no cumulative-reset handling.
/// Outcome metrics (sessions, lines-of-code, etc.) share the metered lane but never enter step
/// cost totals. Pure + unit-tested. `offset_minutes`
/// shifts each point's day into the user's local calendar; pass `0` for UTC.
pub fn ingest_otlp_metrics_json(
    bytes: &[u8],
    offset_minutes: i64,
) -> Result<Vec<MeteredPoint>, String> {
    let root: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("otlp metrics json: {e}"))?;
    let resource_metrics = root
        .get("resourceMetrics")
        .and_then(|x| x.as_array())
        .ok_or("otlp metrics: missing resourceMetrics")?;
    let mut out = Vec::new();
    for rm in resource_metrics {
        for sm in rm
            .get("scopeMetrics")
            .and_then(|x| x.as_array())
            .into_iter()
            .flatten()
        {
            for m in sm
                .get("metrics")
                .and_then(|x| x.as_array())
                .into_iter()
                .flatten()
            {
                // `cost`/`token` feed cost lenses; the rest are OUTCOME counters that
                // power dollar-per-outcome — captured into the SAME metered lane, never folded into
                // cost-step totals. `kind` carries each metric's discriminator attribute.
                let (metric, kind_attr) = match m.get("name").and_then(|x| x.as_str()) {
                    Some("claude_code.cost.usage") => ("cost", None),
                    Some("claude_code.token.usage") => ("token", Some("type")),
                    Some("claude_code.lines_of_code.count") => ("lines_of_code", Some("type")),
                    Some("claude_code.pull_request.count") => ("pull_request", None),
                    Some("claude_code.commit.count") => ("commit", None),
                    Some("claude_code.session.count") => ("session", None),
                    Some("claude_code.active_time.total") => ("active_time", Some("type")),
                    Some("claude_code.code_edit_tool.decision") => {
                        ("edit_decision", Some("decision"))
                    }
                    // Gemini CLI reports token usage ONLY as this aggregate counter —
                    // no per-request usage in its metrics — so it lands in the metered CROSS-CHECK
                    // lane (never folded into cost), keyed by model + `type` (input|output|thought|
                    // cache|tool). Per-request Gemini cost, when the CLI emits `gen_ai.*` spans, is
                    // captured by the span path (`gcp.gen_ai` provider). The cumulative-Sum drop below
                    // applies uniformly, so Gemini's temporality (unknown to us) can't over-count.
                    Some("gemini_cli.token.usage") => ("token", Some("type")),
                    _ => continue,
                };
                // Counter points live under `sum.dataPoints`. We sum them as DELTAs — which is only
                // correct for delta temporality. Claude Code exports delta (1) or
                // leaves it unspecified (0); a corp collector reconfigured to CUMULATIVE (2) emits
                // running TOTALS, and naively summing those over-counts. Since this metered lane is a
                // cross-check only (never folded into cost-step totals), we DROP a cumulative Sum
                // rather than report an inflated cross-check — a missing check beats a wrong one.
                let Some(sum) = m.get("sum") else {
                    continue;
                };
                if is_cumulative_sum(sum) {
                    continue; // CUMULATIVE: cannot delta-diff statelessly → skip, don't over-count
                }
                let Some(points) = sum.get("dataPoints").and_then(|x| x.as_array()) else {
                    continue;
                };
                for dp in points {
                    let attrs = attr_map(dp);
                    let value = if metric == "cost" {
                        // cost.usage is USD (float) -> micro-USD.
                        let Some(usd) = dp
                            .get("asDouble")
                            .and_then(|v| v.as_f64())
                            .filter(|v| v.is_finite())
                        else {
                            continue;
                        };
                        let micros = usd * 1_000_000.0;
                        if !micros.is_finite() {
                            continue;
                        }
                        micros.round() as i64
                    } else {
                        // counts / seconds (active_time): integer delta.
                        dp_int(dp)
                    };
                    if value <= 0 {
                        continue; // skip empty/negative deltas
                    }
                    let day = dp
                        .get("timeUnixNano")
                        .map(|value| nanos_to_day(value, offset_minutes))
                        .unwrap_or_default();
                    if day.is_empty() {
                        // Without a timestamp the delta cannot be assigned to a reconciliation
                        // window. Persisting it under an empty pseudo-day makes it effectively
                        // invisible while still growing the store, so drop it explicitly.
                        continue;
                    }
                    out.push(MeteredPoint {
                        day,
                        metric: metric.to_string(),
                        model: attrs
                            .get("model")
                            .and_then(attr_str)
                            .unwrap_or("")
                            .to_string(),
                        kind: kind_attr
                            .and_then(|k| attrs.get(k))
                            .and_then(attr_str)
                            .unwrap_or("")
                            .to_string(),
                        session: first_attr(&attrs, SESSION_KEYS).unwrap_or_default(),
                        effort: first_attr(&attrs, EFFORT_KEYS).unwrap_or_default(),
                        query_source: first_attr(&attrs, QUERY_SOURCE_KEYS).unwrap_or_default(),
                        value,
                    });
                }
            }
        }
    }
    Ok(out)
}

/// Map a Claude Code `api_request` log event to a degraded `StepRecord` (counts only; no body).
fn log_step_claude_code(attrs: &BTreeMap<String, Value>) -> StepRecord {
    let get = |k: &str| attrs.get(k).and_then(attr_u64).unwrap_or(0);
    let model = attrs
        .get("model")
        .and_then(attr_str)
        .unwrap_or("unknown")
        .to_string();
    let run_id = attrs
        .get("session.id")
        .and_then(attr_str)
        .unwrap_or("otel")
        .to_string();
    let usage = UsageTokens {
        fresh_input: get("input_tokens"),
        // Claude Code reports a single cache-creation count (no 5m/1h split) -> all 5m.
        cache_write_5m: get("cache_creation_tokens"),
        cache_write_1h: 0,
        cache_read: get("cache_read_tokens"),
        output: get("output_tokens"),
        reasoning: 0,
        audio_input: 0,
        audio_output: 0,
    };
    let provider = Provider::Anthropic;
    let shape = degraded_shape(model.clone(), provider, &usage, attrs);
    StepRecord {
        run_id,
        step_ordinal: attrs
            .get("event.sequence")
            .and_then(attr_u64)
            .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
            .unwrap_or(0),
        provider,
        model,
        usage,
        shape,
        stop_reason: None,
        duration_ms: 0, // log-event captures carry no reliable per-step duration
        start_unix_nano: None,
        trace_id: None,
        span_id: None,
        parent_span_id: None,
    }
}

/// Map a Codex `codex.sse_event` (event.kind=response.completed) to a degraded `StepRecord`.
/// Codex uses the OpenAI Responses API: `cached_token_count` is a READ-only cache hit (no
/// cache-creation/TTL split), reasoning is a subset of output, and the token event carries no
/// session.id — so runs group by a conversation id when present, else trace/`codex`. Codex has
/// no monotonic `event.sequence`, so a batch-local counter supplies the step ordinal (grouping
/// is refined later — see the Codex epic open question).
fn log_step_codex(attrs: &BTreeMap<String, Value>, seq: &mut u32) -> StepRecord {
    let get = |k: &str| attrs.get(k).and_then(attr_u64).unwrap_or(0);
    let model = attrs
        .get("gen_ai.request.model")
        .and_then(attr_str)
        .or_else(|| attrs.get("model").and_then(attr_str))
        .unwrap_or("unknown")
        .to_string();
    let cached = get("cached_token_count");
    let input = get("input_token_count");
    let usage = UsageTokens {
        fresh_input: input.saturating_sub(cached),
        cache_write_5m: 0, // Codex cache is read-only: no creation count, no TTL split
        cache_write_1h: 0,
        cache_read: cached,
        output: get("output_token_count"),
        reasoning: get("reasoning_token_count").min(get("output_token_count")), // subset of output
        audio_input: 0,
        audio_output: 0,
    };
    let provider = Provider::Openai;
    let run_id = first_attr(attrs, SESSION_KEYS).unwrap_or_else(|| "codex".to_string());
    let ordinal = attrs
        .get("event.sequence")
        .and_then(attr_u64)
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
        .unwrap_or_else(|| {
            *seq = seq.saturating_add(1);
            *seq
        });
    let shape = degraded_shape(model.clone(), provider, &usage, attrs);
    StepRecord {
        run_id,
        step_ordinal: ordinal,
        provider,
        model,
        usage,
        shape,
        stop_reason: None,
        duration_ms: 0, // log-event captures carry no reliable per-step duration
        start_unix_nano: None,
        trace_id: None,
        span_id: None,
        parent_span_id: None,
    }
}

/// Shared degraded `RequestShape` for log-event steps: no body to hash/weigh (request_hash=None
/// so distinct requests aren't collapsed into a bogus retry-loop), with agent correlation labels
/// lifted from whatever the producer stamped.
fn degraded_shape(
    model: String,
    provider: Provider,
    usage: &UsageTokens,
    attrs: &BTreeMap<String, Value>,
) -> RequestShape {
    RequestShape {
        model,
        provider,
        stream: false,
        ttl: crate::model::CacheTtl::FiveMin,
        has_cache_control: usage.cache_read > 0 || usage.cache_write() > 0,
        cached_component: None,
        system_hash: None,
        weights: Vec::new(),
        request_hash: None,
        step_label: first_attr(attrs, OP_KEYS),
        component_label: first_attr(attrs, TOOL_KEYS),
        parent_label: first_attr(attrs, AGENT_KEYS),
        attempt: None,
        session: first_attr(attrs, SESSION_KEYS),
        effort: first_attr(attrs, EFFORT_KEYS),
        mcp_server: first_attr(attrs, MCP_SERVER_KEYS),
        // Workload key truncated to 64 UTF-8-safe chars; never payload.
        workload_key: first_attr(attrs, WORKLOAD_KEY_KEYS).map(|s| s.chars().take(64).collect()),
        // OpenAI-compatible vendor auto-capture requires separate binding work.
        vendor: None,
        commit: None,
        author: None,
    }
}

// ---- Export ----

fn int_attr(key: &str, n: i64) -> Value {
    // OTLP proto3-JSON encodes int64 as a decimal STRING.
    json!({"key": key, "value": {"intValue": n.to_string()}})
}
fn str_attr(key: &str, s: &str) -> Value {
    json!({"key": key, "value": {"stringValue": s}})
}
/// A single-element OTLP `arrayValue` of strings (for spec fields typed `string[]`, e.g.
/// `gen_ai.response.finish_reasons`).
fn str_array_attr(key: &str, s: &str) -> Value {
    json!({"key": key, "value": {"arrayValue": {"values": [{"stringValue": s}]}}})
}
fn bool_attr(key: &str, b: bool) -> Value {
    json!({"key": key, "value": {"boolValue": b}})
}

/// The fully-specified GenAI cost attribute set for one priced-or-unpriced step: integer
/// micro-USD (PRESENT only when the model is priced — never a fabricated $0, #6) plus the
/// pricing provenance (`pricing_version`/`effective_date`) and an explicit `unpriced` flag.
/// Fixed key order keeps the export byte-identical.
fn cost_attrs(micros: Option<i64>, pricing: &PricingTable) -> Vec<Value> {
    let mut a = vec![
        bool_attr("tare.cost.unpriced", micros.is_none()),
        str_attr("tare.cost.pricing_version", &pricing.version),
        str_attr("tare.cost.effective_date", &pricing.effective_date),
        str_attr("tare.cost.currency", "USD"),
        bool_attr("tare.estimated", true),
    ];
    // Only emit a number when we actually have a price — an unpriced model carries NO
    // `tare.cost.micro_usd`, so downstream can't mistake a gap for "$0".
    if let Some(m) = micros {
        a.push(int_attr("tare.cost.micro_usd", m));
    }
    a
}

/// 32-hex-char trace id derived from the run id (FNV, no clock/RNG).
fn trace_id_of(run_id: &str) -> String {
    let a = fnv1a_64(run_id.as_bytes());
    let b = fnv1a_64(format!("{run_id}:trace").as_bytes());
    format!("{a:016x}{b:016x}")
}
/// 16-hex-char span id derived from run id + ordinal.
fn span_id_of(run_id: &str, ordinal: u32) -> String {
    format!(
        "{:016x}",
        fnv1a_64(format!("{run_id}:{ordinal}").as_bytes())
    )
}

/// Export a run to OTLP/JSON trace structure: one span per step, `gen_ai.*` counts +
/// `tare.*` cost (micro-USD as a decimal-string `intValue`). COUNTS ONLY — no payload.
/// Synthetic fixed timestamps (clock-free): step N spans `[N*1e9, N*1e9+1e9)` ns.
pub fn export_otlp_json(run: &RunRecord, pricing: &PricingTable, tare_version: &str) -> Value {
    let trace_id = trace_id_of(&run.run_id);
    let report = build_report(std::slice::from_ref(run), pricing);
    // Map cause -> dominant? We attach the run-level top cause as a hint per span is overkill;
    // attach the per-run top cause to each span as `tare.top_cause` (counts/labels only).
    let top_cause = report
        .rows
        .first()
        .map(|r| r.cause.clone())
        .unwrap_or_default();

    let mut spans = Vec::new();
    for step in &run.steps {
        // None when the model is unpriced; cost_attrs marks the gap instead of fabricating $0.
        let micros = pricing
            .lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
            .map(|r| cost_usage(&step.usage, r, &step.shape).total.micros());
        let start = (step.step_ordinal as u128) * 1_000_000_000u128;
        let end = start + 1_000_000_000u128;
        let mut attrs = vec![
            str_attr("gen_ai.provider.name", provider_to_semconv(step.provider)),
            str_attr("gen_ai.request.model", &step.model),
            str_attr("gen_ai.response.model", &step.model),
            int_attr(
                "gen_ai.usage.input_tokens",
                int64_count(step.usage.fresh_input),
            ),
            int_attr("gen_ai.usage.output_tokens", int64_count(step.usage.output)),
            int_attr(
                "gen_ai.usage.cache_creation.input_tokens",
                int64_count(step.usage.cache_write()),
            ),
            // Tare extension: the 1h portion of the combined cache-creation count, so a
            // Tare->Tare round-trip preserves the tier split (foreign consumers ignore it).
            int_attr(
                "tare.usage.cache_creation_1h.input_tokens",
                int64_count(step.usage.cache_write_1h),
            ),
            int_attr(
                "gen_ai.usage.cache_read.input_tokens",
                int64_count(step.usage.cache_read),
            ),
            int_attr(
                "gen_ai.usage.reasoning.output_tokens",
                int64_count(step.usage.reasoning),
            ),
            int_attr(
                "tare.usage.audio_input_tokens",
                int64_count(step.usage.audio_input),
            ),
            int_attr(
                "tare.usage.audio_output_tokens",
                int64_count(step.usage.audio_output),
            ),
            str_attr("tare.run_id", &run.run_id),
            str_attr("tare.top_cause", &top_cause),
        ];
        // The fully-specified, provenance-stamped cost attribute set (#6: unpriced is flagged,
        // not rendered as $0). Fixed key order keeps the export byte-identical.
        attrs.extend(cost_attrs(micros, pricing));
        if let Some(fr) = &step.stop_reason {
            // semconv types this as string[] — emit a single-element array, not a scalar.
            attrs.push(str_array_attr("gen_ai.response.finish_reasons", fr));
        }
        spans.push(json!({
            "traceId": trace_id,
            "spanId": span_id_of(&run.run_id, step.step_ordinal),
            "name": format!("gen_ai.{}", step.provider.as_str()),
            "kind": 3, // CLIENT
            "startTimeUnixNano": start.to_string(),
            "endTimeUnixNano": end.to_string(),
            "attributes": attrs,
        }));
    }

    json!({
        "resourceSpans": [{
            "resource": {
                "attributes": [
                    str_attr("service.name", "tare"),
                    str_attr("tare.version", tare_version),
                    str_attr("tare.pricing_version", &pricing.version),
                    str_attr("tare.effective_date", &pricing.effective_date),
                ]
            },
            "scopeSpans": [{
                "scope": {"name": "tare", "version": tare_version},
                "spans": spans,
            }]
        }]
    })
}

/// Structural validation of an OTLP/JSON trace document (shape only; values not interpreted).
pub fn validate_otlp_structure(v: &Value) -> Result<(), String> {
    let rs = v
        .get("resourceSpans")
        .and_then(|x| x.as_array())
        .ok_or("missing resourceSpans[]")?;
    for r in rs {
        let scopes = r
            .get("scopeSpans")
            .and_then(|x| x.as_array())
            .ok_or("missing scopeSpans[]")?;
        for sc in scopes {
            let spans = sc
                .get("spans")
                .and_then(|x| x.as_array())
                .ok_or("missing spans[]")?;
            for span in spans {
                if span.get("traceId").and_then(|x| x.as_str()).is_none() {
                    return Err("span missing traceId".into());
                }
                let attrs = span
                    .get("attributes")
                    .and_then(|x| x.as_array())
                    .ok_or("span missing attributes[]")?;
                for a in attrs {
                    let _: &Map<String, Value> =
                        a.as_object().ok_or("attribute is not an object")?;
                    a.get("key")
                        .and_then(|x| x.as_str())
                        .ok_or("attr missing key")?;
                    a.get("value").ok_or("attr missing value")?;
                }
            }
        }
    }
    Ok(())
}

// ---- Metrics export (sibling of the traces exporter) ----

/// A monotonic cumulative `Sum` metric with one int data point per (provider, model) bucket.
/// `aggregationTemporality: 2` = CUMULATIVE; `isMonotonic: true` — both hardcoded (not derived)
/// to preserve byte identity. Timestamps are fixed synthetically, so this remains clock-free.
fn sum_metric(name: &str, unit: &str, points: &[(String, String, i64)]) -> Value {
    let data_points: Vec<Value> = points
        .iter()
        .map(|(provider, model, val)| {
            json!({
                "attributes": [str_attr("gen_ai.provider.name", provider), str_attr("gen_ai.request.model", model)],
                "startTimeUnixNano": "0",
                "timeUnixNano": "1000000000",
                "asInt": val.to_string(),
            })
        })
        .collect();
    json!({
        "name": name,
        "unit": unit,
        "sum": { "dataPoints": data_points, "aggregationTemporality": 2, "isMonotonic": true }
    })
}

/// Export a run as OTLP/JSON **metrics**: the six `gen_ai.usage.*` token counts + `tare.cost.
/// micro_usd`, each a cumulative monotonic Sum with a data point per (provider, model). Teams
/// dashboard or alert on metrics, not spans. Counts only, with no payload; deterministic.
pub fn export_otlp_metrics_json(
    run: &RunRecord,
    pricing: &PricingTable,
    tare_version: &str,
) -> Value {
    // Aggregate per (provider, model): seven token axes + micro-USD cost.
    let mut buckets: BTreeMap<(String, String), [i64; 8]> = BTreeMap::new();
    // A bucket is "priced" only if its model resolves to a rates row — an unpriced model emits
    // NO cost data point (a gap, never a fabricated $0, #6). Priced-ness is constant per bucket.
    let mut priced: BTreeMap<(String, String), bool> = BTreeMap::new();
    for step in &run.steps {
        let micros = pricing
            .lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
            .map(|r| cost_usage(&step.usage, r, &step.shape).total.micros());
        let key = (step.provider.as_str().to_string(), step.model.clone());
        priced
            .entry(key.clone())
            .and_modify(|p| *p &= micros.is_some())
            .or_insert(micros.is_some());
        let e = buckets.entry(key).or_default();
        e[0] = e[0].saturating_add(int64_count(step.usage.fresh_input));
        e[1] = e[1].saturating_add(int64_count(step.usage.output));
        e[2] = e[2].saturating_add(int64_count(step.usage.cache_write()));
        e[3] = e[3].saturating_add(int64_count(step.usage.cache_read));
        e[4] = e[4].saturating_add(int64_count(step.usage.reasoning));
        e[5] = e[5].saturating_add(int64_count(step.usage.audio_input));
        e[6] = e[6].saturating_add(int64_count(step.usage.audio_output));
        e[7] = e[7].saturating_add(micros.unwrap_or(0));
    }
    let col = |idx: usize| -> Vec<(String, String, i64)> {
        buckets
            .iter()
            .map(|((p, m), v)| (p.clone(), m.clone(), v[idx]))
            .collect()
    };
    // Cost column: priced buckets only — unpriced (provider, model) pairs are omitted entirely.
    let cost_col: Vec<(String, String, i64)> = buckets
        .iter()
        .filter(|(k, _)| priced.get(*k).copied().unwrap_or(false))
        .map(|((p, m), v)| (p.clone(), m.clone(), v[7]))
        .collect();
    json!({
        "resourceMetrics": [{
            "resource": { "attributes": [
                str_attr("service.name", "tare"),
                str_attr("tare.version", tare_version),
                str_attr("tare.pricing_version", &pricing.version),
                str_attr("tare.effective_date", &pricing.effective_date),
            ]},
            "scopeMetrics": [{
                "scope": {"name": "tare", "version": tare_version},
                "metrics": [
                    sum_metric("gen_ai.usage.input_tokens", "{token}", &col(0)),
                    sum_metric("gen_ai.usage.output_tokens", "{token}", &col(1)),
                    sum_metric("gen_ai.usage.cache_creation.input_tokens", "{token}", &col(2)),
                    sum_metric("gen_ai.usage.cache_read.input_tokens", "{token}", &col(3)),
                    sum_metric("gen_ai.usage.reasoning.output_tokens", "{token}", &col(4)),
                    sum_metric("tare.usage.audio_input_tokens", "{token}", &col(5)),
                    sum_metric("tare.usage.audio_output_tokens", "{token}", &col(6)),
                    sum_metric("tare.cost.micro_usd", "uUSD", &cost_col),
                ]
            }]
        }]
    })
}

/// Structural validation of an OTLP/JSON metrics document (shape only).
pub fn validate_otlp_metrics_structure(v: &Value) -> Result<(), String> {
    let rm = v
        .get("resourceMetrics")
        .and_then(|x| x.as_array())
        .ok_or("missing resourceMetrics[]")?;
    for r in rm {
        let scopes = r
            .get("scopeMetrics")
            .and_then(|x| x.as_array())
            .ok_or("missing scopeMetrics[]")?;
        for sc in scopes {
            let metrics = sc
                .get("metrics")
                .and_then(|x| x.as_array())
                .ok_or("missing metrics[]")?;
            for m in metrics {
                m.get("name")
                    .and_then(|x| x.as_str())
                    .ok_or("metric missing name")?;
                m.get("sum")
                    .and_then(|s| s.get("dataPoints"))
                    .and_then(|x| x.as_array())
                    .ok_or("metric missing sum.dataPoints[]")?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pricing() -> PricingTable {
        PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml")).unwrap()
    }

    #[test]
    fn intvalue_number_and_string_both_parse() {
        assert_eq!(attr_u64(&json!({"intValue": 42})), Some(42));
        assert_eq!(attr_u64(&json!({"intValue": "42"})), Some(42));
    }

    // Synthetic OTLP/JSON logs export matching a real Claude Code v2.1.x `api_request` event
    // (real user/session hashes intentionally NOT used). Includes a non-api_request event that
    // must be ignored.
    const CC_LOGS: &str = r#"{"resourceLogs":[{"scopeLogs":[{"logRecords":[
      {"attributes":[
        {"key":"event.name","value":{"stringValue":"user_prompt"}},
        {"key":"session.id","value":{"stringValue":"sess-x"}}
      ]},
      {"attributes":[
        {"key":"event.name","value":{"stringValue":"api_request"}},
        {"key":"session.id","value":{"stringValue":"sess-x"}},
        {"key":"model","value":{"stringValue":"claude-opus-4-8"}},
        {"key":"input_tokens","value":{"intValue":2135}},
        {"key":"output_tokens","value":{"intValue":4}},
        {"key":"cache_read_tokens","value":{"intValue":29177}},
        {"key":"cache_creation_tokens","value":{"intValue":0}},
        {"key":"event.sequence","value":{"intValue":12}}
      ]}
    ]}]}]}"#;

    #[test]
    fn tolerates_legacy_prompt_completion_token_attr_names() {
        // An older instrumentation library that still emits prompt_tokens/completion_tokens.
        let legacy = r#"{"resourceSpans":[{"scopeSpans":[{"spans":[
          {"traceId":"t","attributes":[
            {"key":"gen_ai.provider.name","value":{"stringValue":"openai"}},
            {"key":"gen_ai.response.model","value":{"stringValue":"gpt-5"}},
            {"key":"gen_ai.usage.prompt_tokens","value":{"intValue":700}},
            {"key":"gen_ai.usage.completion_tokens","value":{"intValue":120}}
          ]}
        ]}]}]}"#;
        let steps = ingest_otlp_json(legacy.as_bytes()).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].usage.fresh_input, 700);
        assert_eq!(steps[0].usage.output, 120);
    }

    #[test]
    fn openinference_span_splits_inclusive_prompt_without_double_counting() {
        // OpenInference (Arize) LLM span: llm.token_count.prompt is INCLUSIVE of ALL prompt_details
        // subsets — cache_read + cache_write(creation) + audio (iolq #7). prompt=1000
        // (contains 800 cache_read + 100 cache_write + 40 audio); completion=200 (50 reasoning + 10 audio).
        let oi = r#"{"resourceSpans":[{"scopeSpans":[{"spans":[
          {"traceId":"t","attributes":[
            {"key":"llm.provider","value":{"stringValue":"anthropic"}},
            {"key":"llm.model_name","value":{"stringValue":"claude-opus-4-8"}},
            {"key":"llm.token_count.prompt","value":{"intValue":1000}},
            {"key":"llm.token_count.prompt_details.cache_read","value":{"intValue":800}},
            {"key":"llm.token_count.prompt_details.cache_write","value":{"intValue":100}},
            {"key":"llm.token_count.prompt_details.audio","value":{"intValue":40}},
            {"key":"llm.token_count.completion","value":{"intValue":200}},
            {"key":"llm.token_count.completion_details.reasoning","value":{"intValue":50}},
            {"key":"llm.token_count.completion_details.audio","value":{"intValue":10}}
          ]}
        ]}]}]}"#;
        let steps = ingest_otlp_json(oi.as_bytes()).unwrap();
        assert_eq!(steps.len(), 1);
        let u = &steps[0].usage;
        assert_eq!(steps[0].provider, Provider::Anthropic);
        assert_eq!(steps[0].model, "claude-opus-4-8");
        // Input: fresh = 1000 − 800(read) − 100(write) − 40(audio) = 60. Crucially cache_write is
        // subtracted from fresh AND surfaced as cache_write_5m — counted ONCE, not double (iolq #7).
        assert_eq!(u.fresh_input, 60);
        assert_eq!(u.cache_read, 800);
        assert_eq!(u.cache_write_5m, 100);
        assert_eq!(u.audio_input, 40);
        assert_eq!(u.output, 190);
        assert_eq!(u.reasoning, 50);
        assert_eq!(u.audio_output, 10);
        // Input subsets sum back to the ORIGINAL inclusive prompt — no double-count, no under-count.
        assert_eq!(
            u.fresh_input + u.cache_read + u.cache_write_5m + u.audio_input,
            1000
        );
        assert_eq!(u.output + u.audio_output, 200);
    }

    #[test]
    fn malformed_openinference_subsets_are_dropped_instead_of_overcounted() {
        let oi = r#"{"resourceSpans":[{"scopeSpans":[{"spans":[
          {"traceId":"t","attributes":[
            {"key":"llm.provider","value":{"stringValue":"anthropic"}},
            {"key":"llm.token_count.prompt","value":{"intValue":10}},
            {"key":"llm.token_count.prompt_details.cache_read","value":{"intValue":8}},
            {"key":"llm.token_count.prompt_details.cache_write","value":{"intValue":8}}
          ]}
        ]}]}]}"#;
        assert!(ingest_otlp_json(oi.as_bytes()).unwrap().is_empty());
    }

    #[test]
    fn convention_dispatch_prefers_gen_ai_when_both_dialects_are_present() {
        // A span carrying BOTH gen_ai and OpenInference token attrs → gen_ai wins (tried first), so its
        // cache-EXCLUSIVE semantics apply (fresh_input read straight from gen_ai.usage.input_tokens),
        // not the OpenInference inclusive split. Locks the read_step ordering.
        let both = r#"{"resourceSpans":[{"scopeSpans":[{"spans":[
          {"traceId":"t","attributes":[
            {"key":"gen_ai.provider.name","value":{"stringValue":"anthropic"}},
            {"key":"gen_ai.response.model","value":{"stringValue":"claude-opus-4-8"}},
            {"key":"gen_ai.usage.input_tokens","value":{"intValue":200}},
            {"key":"gen_ai.usage.cache_read.input_tokens","value":{"intValue":800}},
            {"key":"gen_ai.usage.output_tokens","value":{"intValue":150}},
            {"key":"llm.provider","value":{"stringValue":"openai"}},
            {"key":"llm.token_count.prompt","value":{"intValue":1000}},
            {"key":"llm.token_count.completion","value":{"intValue":999}}
          ]}
        ]}]}]}"#;
        let steps = ingest_otlp_json(both.as_bytes()).unwrap();
        assert_eq!(steps.len(), 1);
        // gen_ai path: fresh=200 (not 1000−800 from OpenInference), provider anthropic (not openai).
        assert_eq!(steps[0].provider, Provider::Anthropic);
        assert_eq!(steps[0].usage.fresh_input, 200);
        assert_eq!(steps[0].usage.cache_read, 800);
        assert_eq!(steps[0].usage.output, 150);
    }

    #[test]
    fn non_llm_and_unknown_provider_spans_are_skipped_not_miscaptured() {
        // A span with token counts but no provider, and one with an unknown provider, are honest GAPs.
        let spans = r#"{"resourceSpans":[{"scopeSpans":[{"spans":[
          {"traceId":"t","attributes":[
            {"key":"llm.token_count.prompt","value":{"intValue":10}}
          ]},
          {"traceId":"t","attributes":[
            {"key":"llm.provider","value":{"stringValue":"acme-llm"}},
            {"key":"llm.token_count.prompt","value":{"intValue":10}}
          ]}
        ]}]}]}"#;
        assert!(ingest_otlp_json(spans.as_bytes()).unwrap().is_empty());
    }

    #[test]
    fn payload_bearing_events_are_dropped_and_never_read() {
        // even if an exporter is misconfigured to emit request/response BODIES with
        // raw text, Tare must drop them at ingest — no step, no liveness pulse, no text retained.
        let body = r#"{"resourceLogs":[{"scopeLogs":[{"logRecords":[
            {"attributes":[
                {"key":"event.name","value":{"stringValue":"claude_code.api_request_body"}},
                {"key":"session.id","value":{"stringValue":"sess-leak"}},
                {"key":"body","value":{"stringValue":"SECRET PROMPT TEXT do not store"}}
            ]},
            {"attributes":[
                {"key":"event.name","value":{"stringValue":"api_response_body"}},
                {"key":"content","value":{"stringValue":"SECRET RESPONSE do not store"}}
            ]}
        ]}]}]}"#;
        let steps = ingest_otlp_logs_json(body.as_bytes(), &PrivacyPolicy::default()).unwrap();
        assert!(steps.is_empty(), "body events never become steps");
        let pulses = ingest_otlp_logs_pulses(body.as_bytes()).unwrap();
        assert!(pulses.is_empty(), "body events never beat liveness");
        // The classifier itself is unambiguous.
        assert!(is_payload_bearing_event("claude_code.api_request_body"));
        assert!(is_payload_bearing_event("api_response_body"));
        assert!(!is_payload_bearing_event("api_request"));
        assert!(!is_payload_bearing_event("user_prompt"));
    }

    #[test]
    fn ingest_logs_maps_claude_code_api_request_events_only() {
        let recs = ingest_otlp_logs_json(CC_LOGS.as_bytes(), &PrivacyPolicy::default()).unwrap();
        assert_eq!(recs.len(), 1, "only the api_request event becomes a step");
        let r = &recs[0];
        assert_eq!(r.run_id, "sess-x");
        assert_eq!(r.model, "claude-opus-4-8");
        assert_eq!(r.provider, Provider::Anthropic);
        assert_eq!(r.step_ordinal, 12);
        assert_eq!(r.usage.fresh_input, 2135);
        assert_eq!(r.usage.output, 4);
        assert_eq!(r.usage.cache_read, 29177);
        assert_eq!(r.usage.cache_write_5m, 0);
        // Regression: no request body was observed, so request_hash MUST be None — otherwise every
        // degraded step collides on hash(Null) and gets mis-attributed to a bogus retry-loop.
        assert!(r.shape.request_hash.is_none());
    }

    #[test]
    fn metrics_export_maps_claude_code_cost_and_token_counters() {
        // timeUnixNano 1_782_000_000 s -> a UTC day; cost USD -> micro-USD; token type preserved.
        let nanos = 1_782_000_000_000_000_000u64; // seconds * 1e9
        let metrics = format!(
            r#"{{"resourceMetrics":[{{"scopeMetrics":[{{"metrics":[
              {{"name":"claude_code.cost.usage","sum":{{"dataPoints":[
                {{"asDouble":0.25,"timeUnixNano":"{nanos}","attributes":[
                  {{"key":"model","value":{{"stringValue":"claude-opus-4-8"}}}},
                  {{"key":"session.id","value":{{"stringValue":"sess-1"}}}}]}}]}}}},
              {{"name":"claude_code.token.usage","sum":{{"dataPoints":[
                {{"asInt":"1200","timeUnixNano":"{nanos}","attributes":[
                  {{"key":"type","value":{{"stringValue":"input"}}}},
                  {{"key":"model","value":{{"stringValue":"claude-opus-4-8"}}}}]}}]}}}},
              {{"name":"claude_code.session.count","sum":{{"dataPoints":[
                {{"asInt":"1","timeUnixNano":"{nanos}","attributes":[]}}]}}}}
            ]}}]}}]}}"#
        );
        let pts = ingest_otlp_metrics_json(metrics.as_bytes(), 0).unwrap();
        // cost + token + the session OUTCOME counter (now captures outcome metrics too).
        assert_eq!(pts.len(), 3);

        let cost = pts.iter().find(|p| p.metric == "cost").unwrap();
        assert_eq!(cost.value, 250_000, "$0.25 -> 250000 micro-USD");
        assert_eq!(cost.model, "claude-opus-4-8");
        assert_eq!(cost.session, "sess-1");
        assert_eq!(cost.kind, "", "cost has no token-type kind");
        assert!(!cost.day.is_empty() && cost.day.starts_with("20"));

        let tok = pts.iter().find(|p| p.metric == "token").unwrap();
        assert_eq!(tok.value, 1200);
        assert_eq!(tok.kind, "input");

        let sess = pts.iter().find(|p| p.metric == "session").unwrap();
        assert_eq!(sess.value, 1);
    }

    #[test]
    fn metrics_without_a_timestamp_are_dropped() {
        let metrics = br#"{"resourceMetrics":[{"scopeMetrics":[{"metrics":[
          {"name":"claude_code.cost.usage","sum":{"dataPoints":[{"asDouble":0.25}]}}
        ]}]}]}"#;
        assert!(ingest_otlp_metrics_json(metrics, 0).unwrap().is_empty());
    }

    #[test]
    fn metrics_ingest_captures_gemini_cli_token_usage_cross_check() {
        // Gemini CLI reports tokens only via the `gemini_cli.token.usage` counter,
        // keyed by model + `type` (input|output|thought|cache|tool). It lands in the metered
        // cross-check lane (metric "token"), never folded into cost.
        let nanos = 1_782_000_000_000_000_000u64;
        let metrics = format!(
            r#"{{"resourceMetrics":[{{"scopeMetrics":[{{"metrics":[
              {{"name":"gemini_cli.token.usage","sum":{{"aggregationTemporality":1,"dataPoints":[
                {{"asInt":"900","timeUnixNano":"{nanos}","attributes":[
                  {{"key":"type","value":{{"stringValue":"input"}}}},
                  {{"key":"model","value":{{"stringValue":"gemini-2.5-pro"}}}}]}},
                {{"asInt":"140","timeUnixNano":"{nanos}","attributes":[
                  {{"key":"type","value":{{"stringValue":"thought"}}}},
                  {{"key":"model","value":{{"stringValue":"gemini-2.5-pro"}}}}]}}]}}}}
            ]}}]}}]}}"#
        );
        let pts = ingest_otlp_metrics_json(metrics.as_bytes(), 0).unwrap();
        assert_eq!(pts.len(), 2);
        let input = pts.iter().find(|p| p.kind == "input").unwrap();
        assert_eq!(input.metric, "token");
        assert_eq!(input.model, "gemini-2.5-pro");
        assert_eq!(input.value, 900);
        let thought = pts.iter().find(|p| p.kind == "thought").unwrap();
        assert_eq!(thought.value, 140);
    }

    #[test]
    fn metrics_drops_cumulative_gemini_token_usage() {
        // The cumulative-drop safety applies to Gemini too: unknown temporality can't over-count.
        let nanos = 1_782_000_000_000_000_000u64;
        let metrics = format!(
            r#"{{"resourceMetrics":[{{"scopeMetrics":[{{"metrics":[
              {{"name":"gemini_cli.token.usage","sum":{{"aggregationTemporality":2,"dataPoints":[
                {{"asInt":"5000","timeUnixNano":"{nanos}","attributes":[
                  {{"key":"type","value":{{"stringValue":"input"}}}},
                  {{"key":"model","value":{{"stringValue":"gemini-2.5-pro"}}}}]}}]}}}}
            ]}}]}}]}}"#
        );
        assert!(
            ingest_otlp_metrics_json(metrics.as_bytes(), 0)
                .unwrap()
                .is_empty(),
            "a CUMULATIVE Gemini counter is dropped, not summed as a delta"
        );
    }

    #[test]
    fn metrics_drops_cumulative_temporality_to_avoid_overcounting() {
        // a Sum reconfigured to CUMULATIVE (2) carries running totals; summing them
        // as deltas over-counts. Since this lane is cross-check-only, drop it rather than inflate.
        let nanos = 1_782_000_000_000_000_000u64;
        let cumulative = format!(
            r#"{{"resourceMetrics":[{{"scopeMetrics":[{{"metrics":[
              {{"name":"claude_code.cost.usage","sum":{{"aggregationTemporality":2,"dataPoints":[
                {{"asDouble":0.25,"timeUnixNano":"{nanos}","attributes":[
                  {{"key":"model","value":{{"stringValue":"claude-opus-4-8"}}}}]}}]}}}}
            ]}}]}}]}}"#
        );
        assert!(
            ingest_otlp_metrics_json(cumulative.as_bytes(), 0)
                .unwrap()
                .is_empty(),
            "cumulative Sum is dropped, never summed as a delta"
        );
        let named = cumulative.replace(
            "\"aggregationTemporality\":2",
            "\"aggregationTemporality\":\"AGGREGATION_TEMPORALITY_CUMULATIVE\"",
        );
        assert!(
            ingest_otlp_metrics_json(named.as_bytes(), 0)
                .unwrap()
                .is_empty(),
            "proto-JSON enum names must be recognized too"
        );
        // Explicit DELTA (1) and absent temporality (Claude Code's default) are both kept.
        for temp in ["\"aggregationTemporality\":1,", ""] {
            let delta = format!(
                r#"{{"resourceMetrics":[{{"scopeMetrics":[{{"metrics":[
                  {{"name":"claude_code.cost.usage","sum":{{{temp}"dataPoints":[
                    {{"asDouble":0.25,"timeUnixNano":"{nanos}","attributes":[
                      {{"key":"model","value":{{"stringValue":"claude-opus-4-8"}}}}]}}]}}}}
                ]}}]}}]}}"#
            );
            assert_eq!(
                ingest_otlp_metrics_json(delta.as_bytes(), 0).unwrap().len(),
                1,
                "delta/unspecified temporality is summable and kept"
            );
        }
    }

    #[test]
    fn metrics_ingest_captures_effort_and_query_source() {
        // a cost point stamped with reasoning effort + query source carries both, so
        // spend can later be sliced by effort and $/outcome computed on query_source='main' only.
        let nanos = 1_782_000_000_000_000_000u64;
        let metrics = format!(
            r#"{{"resourceMetrics":[{{"scopeMetrics":[{{"metrics":[
              {{"name":"claude_code.cost.usage","sum":{{"dataPoints":[
                {{"asDouble":0.5,"timeUnixNano":"{nanos}","attributes":[
                  {{"key":"model","value":{{"stringValue":"claude-opus-4-8"}}}},
                  {{"key":"reasoning_effort","value":{{"stringValue":"high"}}}},
                  {{"key":"query_source","value":{{"stringValue":"main"}}}}]}}]}}}}
            ]}}]}}]}}"#
        );
        let pts = ingest_otlp_metrics_json(metrics.as_bytes(), 0).unwrap();
        let cost = pts.iter().find(|p| p.metric == "cost").unwrap();
        assert_eq!(cost.effort, "high");
        assert_eq!(cost.query_source, "main");
        // Absent attributes default to empty (older captures).
        let bare = format!(
            r#"{{"resourceMetrics":[{{"scopeMetrics":[{{"metrics":[
              {{"name":"claude_code.cost.usage","sum":{{"dataPoints":[
                {{"asDouble":0.1,"timeUnixNano":"{nanos}","attributes":[]}}]}}}}
            ]}}]}}]}}"#
        );
        let p2 = ingest_otlp_metrics_json(bare.as_bytes(), 0).unwrap();
        assert_eq!(p2[0].effort, "");
        assert_eq!(p2[0].query_source, "");
    }

    #[test]
    fn metrics_export_captures_outcome_counters_with_discriminators() {
        // lines_of_code(type), commit, pull_request, active_time(type), edit decision.
        let n = 1_782_000_000_000_000_000u64;
        let metrics = format!(
            r#"{{"resourceMetrics":[{{"scopeMetrics":[{{"metrics":[
              {{"name":"claude_code.lines_of_code.count","sum":{{"dataPoints":[
                {{"asInt":"40","timeUnixNano":"{n}","attributes":[{{"key":"type","value":{{"stringValue":"added"}}}}]}}]}}}},
              {{"name":"claude_code.commit.count","sum":{{"dataPoints":[
                {{"asInt":"2","timeUnixNano":"{n}","attributes":[]}}]}}}},
              {{"name":"claude_code.pull_request.count","sum":{{"dataPoints":[
                {{"asInt":"1","timeUnixNano":"{n}","attributes":[]}}]}}}},
              {{"name":"claude_code.active_time.total","sum":{{"dataPoints":[
                {{"asDouble":123.7,"timeUnixNano":"{n}","attributes":[{{"key":"type","value":{{"stringValue":"cli"}}}}]}}]}}}},
              {{"name":"claude_code.code_edit_tool.decision","sum":{{"dataPoints":[
                {{"asInt":"3","timeUnixNano":"{n}","attributes":[{{"key":"decision","value":{{"stringValue":"accept"}}}}]}}]}}}}
            ]}}]}}]}}"#
        );
        let pts = ingest_otlp_metrics_json(metrics.as_bytes(), 0).unwrap();
        let find = |m: &str| pts.iter().find(|p| p.metric == m).unwrap();
        assert_eq!(
            (
                find("lines_of_code").value,
                find("lines_of_code").kind.as_str()
            ),
            (40, "added")
        );
        assert_eq!(find("commit").value, 2);
        assert_eq!(find("pull_request").value, 1);
        assert_eq!(
            (find("active_time").value, find("active_time").kind.as_str()),
            (123, "cli")
        );
        assert_eq!(
            (
                find("edit_decision").value,
                find("edit_decision").kind.as_str()
            ),
            (3, "accept")
        );
    }

    #[test]
    fn liveness_pulses_capture_prompt_and_tool_events_without_cost_rows() {
        // A mixed batch: one api_request (cost), one user_prompt + one tool_result (liveness only).
        let logs = r#"{"resourceLogs":[{"scopeLogs":[{"logRecords":[
          {"attributes":[
            {"key":"event.name","value":{"stringValue":"api_request"}},
            {"key":"session.id","value":{"stringValue":"sess-1"}},
            {"key":"model","value":{"stringValue":"claude-opus-4-8"}},
            {"key":"input_tokens","value":{"intValue":100}}]},
          {"attributes":[
            {"key":"event.name","value":{"stringValue":"user_prompt"}},
            {"key":"session.id","value":{"stringValue":"sess-1"}}]},
          {"attributes":[
            {"key":"event.name","value":{"stringValue":"tool_result"}},
            {"key":"session.id","value":{"stringValue":"sess-1"}}]}
        ]}]}]}"#;
        // The cost path sees ONLY the api_request (byte-stable; pulses never become steps).
        let recs = ingest_otlp_logs_json(logs.as_bytes(), &PrivacyPolicy::default()).unwrap();
        assert_eq!(recs.len(), 1, "only api_request mints a cost row");
        // The pulse path sees the two liveness events (and ignores the api_request).
        let pulses = ingest_otlp_logs_pulses(logs.as_bytes()).unwrap();
        assert_eq!(pulses.len(), 2);
        assert_eq!(pulses[0].kind, PulseKind::UserPrompt);
        assert_eq!(pulses[0].session.as_deref(), Some("sess-1"));
        assert_eq!(pulses[1].kind, PulseKind::ToolResult);
        assert_eq!(pulses[1].kind.as_str(), "tool_result");
    }

    #[test]
    fn degraded_steps_are_not_collapsed_into_a_retry_loop() {
        // Two distinct api_request events must NOT be flagged as identical retries.
        let logs = r#"{"resourceLogs":[{"scopeLogs":[{"logRecords":[
          {"attributes":[
            {"key":"event.name","value":{"stringValue":"api_request"}},
            {"key":"session.id","value":{"stringValue":"s"}},
            {"key":"model","value":{"stringValue":"claude-opus-4-8"}},
            {"key":"input_tokens","value":{"intValue":100}},
            {"key":"event.sequence","value":{"intValue":1}}]},
          {"attributes":[
            {"key":"event.name","value":{"stringValue":"api_request"}},
            {"key":"session.id","value":{"stringValue":"s"}},
            {"key":"model","value":{"stringValue":"claude-opus-4-8"}},
            {"key":"input_tokens","value":{"intValue":200}},
            {"key":"event.sequence","value":{"intValue":2}}]}
        ]}]}]}"#;
        let recs = ingest_otlp_logs_json(logs.as_bytes(), &PrivacyPolicy::default()).unwrap();
        assert_eq!(recs.len(), 2);
        assert!(recs.iter().all(|r| r.shape.request_hash.is_none()));
    }

    #[test]
    fn self_hosted_backend_identity_is_preserved_as_vendor() {
        // A self-hosted span keeps its backend label instead of flattening to "local".
        let doc = json!({
            "resourceSpans": [{
                "scopeSpans": [{
                    "spans": [
                        {"spanId": "aaaa", "startTimeUnixNano": "1000000000",
                         "attributes": [
                            {"key": "gen_ai.provider.name", "value": {"stringValue": "ollama"}},
                            {"key": "gen_ai.request.model", "value": {"stringValue": "llama3"}},
                            {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "10"}},
                            {"key": "gen_ai.usage.output_tokens", "value": {"intValue": 5}}
                         ]}
                    ]
                }]
            }]
        });
        let steps = ingest_otlp_json(doc.to_string().as_bytes()).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].provider, Provider::Local);
        assert_eq!(steps[0].vendor.as_deref(), Some("ollama"));
    }

    #[test]
    fn generic_local_provider_has_no_backend_vendor() {
        let doc = json!({
            "resourceSpans": [{
                "scopeSpans": [{
                    "spans": [
                        {"spanId": "aaaa", "startTimeUnixNano": "1000000000",
                         "attributes": [
                            {"key": "gen_ai.provider.name", "value": {"stringValue": "local"}},
                            {"key": "gen_ai.request.model", "value": {"stringValue": "mystery"}},
                            {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "10"}},
                            {"key": "gen_ai.usage.output_tokens", "value": {"intValue": 5}}
                         ]}
                    ]
                }]
            }]
        });
        let steps = ingest_otlp_json(doc.to_string().as_bytes()).unwrap();
        assert_eq!(steps[0].provider, Provider::Local);
        assert_eq!(steps[0].vendor, None);
    }

    #[test]
    fn ingest_orders_genai_spans_and_skips_others() {
        let doc = json!({
            "resourceSpans": [{
                "scopeSpans": [{
                    "spans": [
                        {"spanId": "bbbb", "startTimeUnixNano": "2000000000",
                         "attributes": [
                            {"key": "gen_ai.system", "value": {"stringValue": "openai"}},
                            {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "10"}},
                            {"key": "gen_ai.usage.output_tokens", "value": {"intValue": 5}}
                         ]},
                        {"spanId": "zzzz", "attributes": [
                            {"key": "http.method", "value": {"stringValue": "GET"}}
                         ]},
                        {"spanId": "aaaa", "startTimeUnixNano": "1000000000",
                         "attributes": [
                            {"key": "gen_ai.provider.name", "value": {"stringValue": "anthropic"}},
                            {"key": "gen_ai.response.model", "value": {"stringValue": "claude-opus-4-8"}},
                            {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "100"}}
                         ]}
                    ]
                }]
            }]
        });
        let steps = ingest_otlp_json(doc.to_string().as_bytes()).unwrap();
        assert_eq!(steps.len(), 2); // GenAI only
                                    // Ordered by start time: anthropic (1e9) before openai (2e9).
        assert_eq!(steps[0].provider, Provider::Anthropic);
        assert_eq!(steps[0].model, "claude-opus-4-8");
        assert_eq!(steps[1].provider, Provider::Openai);
        assert_eq!(steps[1].usage.output, 5);
    }

    #[test]
    fn unknown_provider_errors() {
        let doc = json!({"resourceSpans": [{"scopeSpans": [{"spans": [
            {"spanId": "x", "attributes": [
                {"key": "gen_ai.provider.name", "value": {"stringValue": "skynet"}}
            ]}
        ]}]}]});
        assert!(ingest_otlp_json(doc.to_string().as_bytes()).is_err());
    }

    #[test]
    fn lifts_mcp_server_attribute_and_threads_it_into_the_shape() {
        // Primary key on one span, the `gen_ai.mcp.server` fallback on another, and a third with
        // no MCP attribute at all (-> None, the unlabeled bucket).
        let doc = json!({"resourceSpans": [{"scopeSpans": [{"spans": [
            {"spanId": "a", "startTimeUnixNano": "1000000000", "traceId": "t1", "attributes": [
                {"key": "gen_ai.provider.name", "value": {"stringValue": "anthropic"}},
                {"key": "gen_ai.response.model", "value": {"stringValue": "claude-opus-4-8"}},
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "1000"}},
                {"key": "mcp.server.name", "value": {"stringValue": "github-mcp"}}
            ]},
            {"spanId": "b", "startTimeUnixNano": "1000000001", "traceId": "t1", "attributes": [
                {"key": "gen_ai.provider.name", "value": {"stringValue": "anthropic"}},
                {"key": "gen_ai.response.model", "value": {"stringValue": "claude-opus-4-8"}},
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "1000"}},
                {"key": "gen_ai.mcp.server", "value": {"stringValue": "fs-mcp"}}
            ]},
            {"spanId": "c", "startTimeUnixNano": "1000000002", "traceId": "t1", "attributes": [
                {"key": "gen_ai.provider.name", "value": {"stringValue": "anthropic"}},
                {"key": "gen_ai.response.model", "value": {"stringValue": "claude-opus-4-8"}},
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "1000"}}
            ]},
            {"spanId": "d", "startTimeUnixNano": "1000000003", "traceId": "t1", "attributes": [
                {"key": "gen_ai.provider.name", "value": {"stringValue": "anthropic"}},
                {"key": "gen_ai.response.model", "value": {"stringValue": "claude-opus-4-8"}},
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "1000"}},
                {"key": "mcp_server.name", "value": {"stringValue": "snake-mcp"}}
            ]}
        ]}]}]});
        let steps = ingest_otlp_json(doc.to_string().as_bytes()).unwrap();
        assert_eq!(steps[0].mcp_server.as_deref(), Some("github-mcp"));
        assert_eq!(steps[1].mcp_server.as_deref(), Some("fs-mcp")); // gen_ai.mcp.server fallback
        assert_eq!(steps[2].mcp_server, None);
        assert_eq!(steps[3].mcp_server.as_deref(), Some("snake-mcp")); // mcp_server.name alt key
                                                                       // The label survives the degraded shape conversion the rollup reads from.
        let recs = otel_steps_to_records(&steps, &PrivacyPolicy::default());
        assert_eq!(recs[0].shape.mcp_server.as_deref(), Some("github-mcp"));
        assert_eq!(recs[2].shape.mcp_server, None);
    }

    #[test]
    fn computes_duration_from_span_end_minus_start_and_opts_out() {
        // A span with start + end 250ms later; another with no end (-> 0).
        let doc = json!({"resourceSpans": [{"scopeSpans": [{"spans": [
            {"spanId": "a", "startTimeUnixNano": "1000000000", "endTimeUnixNano": "1250000000", "traceId": "t1", "attributes": [
                {"key": "gen_ai.provider.name", "value": {"stringValue": "anthropic"}},
                {"key": "gen_ai.response.model", "value": {"stringValue": "claude-opus-4-8"}},
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "1000"}}
            ]},
            {"spanId": "b", "startTimeUnixNano": "2000000000", "traceId": "t1", "attributes": [
                {"key": "gen_ai.provider.name", "value": {"stringValue": "anthropic"}},
                {"key": "gen_ai.response.model", "value": {"stringValue": "claude-opus-4-8"}},
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "1000"}}
            ]}
        ]}]}]});
        let steps = ingest_otlp_json(doc.to_string().as_bytes()).unwrap();
        assert_eq!(steps[0].duration_ms, 250); // (1.25e9 - 1.0e9) / 1e6
        assert_eq!(steps[1].duration_ms, 0); // no end time
                                             // It survives the conversion the timeline reads…
        let recs = otel_steps_to_records(&steps, &PrivacyPolicy::default());
        assert_eq!(recs[0].duration_ms, 250);
        // …and the privacy opt-out (max_private) zeroes it.
        let private = otel_steps_to_records(&steps, &PrivacyPolicy::max_private());
        assert_eq!(private[0].duration_ms, 0);
    }

    #[test]
    fn degraded_records_have_empty_weights_but_cost_nonzero() {
        let doc = json!({"resourceSpans": [{"scopeSpans": [{"spans": [
            {"spanId": "a", "startTimeUnixNano": "1000000000", "traceId": "t1", "attributes": [
                {"key": "gen_ai.provider.name", "value": {"stringValue": "openai"}},
                {"key": "gen_ai.response.model", "value": {"stringValue": "gpt-5-mini"}},
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "1000"}},
                {"key": "gen_ai.usage.output_tokens", "value": {"intValue": "200"}}
            ]}
        ]}]}]});
        let steps = ingest_otlp_json(doc.to_string().as_bytes()).unwrap();
        let recs = otel_steps_to_records(&steps, &PrivacyPolicy::default());
        assert_eq!(recs.len(), 1);
        assert!(recs[0].shape.weights.is_empty(), "degraded: no weights");
        assert_eq!(recs[0].run_id, "t1");
        let run = RunRecord {
            run_id: "t1".into(),
            steps: recs,
        };
        let micros = crate::attribute::total_micros(&[run], &pricing());
        assert!(micros > 0, "exact dollars even when degraded");
    }

    #[test]
    fn metrics_export_validates_and_carries_counts_and_cost() {
        let run = RunRecord {
            run_id: "t".into(),
            steps: vec![StepRecord {
                run_id: "t".into(),
                step_ordinal: 1,
                provider: Provider::Anthropic,
                model: "claude-opus-4-8".into(),
                usage: UsageTokens {
                    fresh_input: 100,
                    cache_write_5m: 0,
                    cache_write_1h: 0,
                    cache_read: 0,
                    output: 20,
                    reasoning: 0,
                    audio_input: 7,
                    audio_output: 8,
                },
                shape: crate::wire::anthropic_request_shape(
                    br#"{"model":"claude-opus-4-8","messages":[]}"#,
                    &PrivacyPolicy::default(),
                )
                .unwrap(),
                stop_reason: Some("end_turn".into()),
                duration_ms: 0,
                start_unix_nano: None,
                trace_id: None,
                span_id: None,
                parent_span_id: None,
            }],
        };
        let m = export_otlp_metrics_json(&run, &pricing(), "test");
        validate_otlp_metrics_structure(&m).unwrap();
        let s = m.to_string();
        // Cumulative monotonic sums, gen_ai counts + tare cost, no payload text.
        assert!(s.contains("\"aggregationTemporality\":2") && s.contains("\"isMonotonic\":true"));
        assert!(s.contains("gen_ai.usage.input_tokens") && s.contains("tare.cost.micro_usd"));
        assert!(s.contains("tare.usage.audio_input_tokens"));
        assert!(s.contains("tare.usage.audio_output_tokens"));
        assert!(s.contains("\"asInt\":\"100\"")); // 100 input tokens
        assert!(!s.contains("content") && !s.contains("message"));
    }

    #[test]
    fn cost_attribute_set_is_integer_provenanced_and_unpriced_is_flagged_not_zeroed() {
        // A4: a priced step carries an integer micro-USD cost + pricing provenance + unpriced=false;
        // an unpriced model carries NO micro_usd number (a flagged gap, never a fabricated $0, #6).
        let shape = crate::wire::anthropic_request_shape(
            br#"{"model":"m","messages":[]}"#,
            &PrivacyPolicy::default(),
        )
        .unwrap();
        let mk = |model: &str| RunRecord {
            run_id: "t".into(),
            steps: vec![StepRecord {
                run_id: "t".into(),
                step_ordinal: 1,
                provider: Provider::Anthropic,
                model: model.into(),
                usage: UsageTokens {
                    fresh_input: 100,
                    cache_write_5m: 0,
                    cache_write_1h: 0,
                    cache_read: 0,
                    output: 20,
                    reasoning: 0,
                    audio_input: 0,
                    audio_output: 0,
                },
                shape: shape.clone(),
                stop_reason: Some("end_turn".into()),
                duration_ms: 0,
                start_unix_nano: None,
                trace_id: None,
                span_id: None,
                parent_span_id: None,
            }],
        };

        // Priced model: integer micro_usd present, unpriced=false, provenance stamped.
        let priced = export_otlp_json(&mk("claude-opus-4-8"), &pricing(), "test");
        let ps = priced.to_string();
        assert!(ps.contains(r#"{"key":"tare.cost.micro_usd","value":{"intValue":"#));
        assert!(ps.contains(r#"{"key":"tare.cost.unpriced","value":{"boolValue":false}}"#));
        assert!(ps.contains(r#"{"key":"tare.cost.pricing_version""#));
        assert!(ps.contains(r#"{"key":"tare.cost.effective_date""#));
        assert!(ps.contains(r#"{"key":"tare.estimated","value":{"boolValue":true}}"#));
        // Byte-stable across runs.
        assert_eq!(
            ps,
            export_otlp_json(&mk("claude-opus-4-8"), &pricing(), "test").to_string()
        );

        // Unpriced model: flagged, with no fabricated cost number anywhere in the span.
        let un = export_otlp_json(&mk("ghost-model-unpriced"), &pricing(), "test");
        let us = un.to_string();
        assert!(us.contains(r#"{"key":"tare.cost.unpriced","value":{"boolValue":true}}"#));
        assert!(
            !us.contains("tare.cost.micro_usd"),
            "unpriced step must carry no micro-USD number, got:\n{us}"
        );
    }

    #[test]
    fn metrics_omits_cost_for_unpriced_models() {
        // A4: an unpriced (provider, model) bucket contributes counts but NO cost data point —
        // never a fabricated $0 sum.
        let shape = crate::wire::anthropic_request_shape(
            br#"{"model":"m","messages":[]}"#,
            &PrivacyPolicy::default(),
        )
        .unwrap();
        let run = RunRecord {
            run_id: "t".into(),
            steps: vec![StepRecord {
                run_id: "t".into(),
                step_ordinal: 1,
                provider: Provider::Anthropic,
                model: "ghost-model-unpriced".into(),
                usage: UsageTokens {
                    fresh_input: 100,
                    cache_write_5m: 0,
                    cache_write_1h: 0,
                    cache_read: 0,
                    output: 20,
                    reasoning: 0,
                    audio_input: 0,
                    audio_output: 0,
                },
                shape,
                stop_reason: Some("end_turn".into()),
                duration_ms: 0,
                start_unix_nano: None,
                trace_id: None,
                span_id: None,
                parent_span_id: None,
            }],
        };
        let m = export_otlp_metrics_json(&run, &pricing(), "test");
        validate_otlp_metrics_structure(&m).unwrap();
        let s = m.to_string();
        // Token counts still present; the cost Sum carries no data points for the unpriced bucket.
        assert!(s.contains("gen_ai.usage.input_tokens"));
        assert!(
            !s.contains(r#""name":"tare.cost.micro_usd","unit":"uUSD","sum":{"dataPoints":[{"#),
            "unpriced bucket must not emit a cost data point, got:\n{s}"
        );
    }

    #[test]
    fn export_ingest_preserves_1h_cache_tier_and_audio() {
        // Tare->Tare round-trip must preserve the priced token sub-classes.
        let run = RunRecord {
            run_id: "t".into(),
            steps: vec![StepRecord {
                run_id: "t".into(),
                step_ordinal: 1,
                provider: Provider::Anthropic,
                model: "claude-opus-4-8".into(),
                usage: UsageTokens {
                    fresh_input: 10,
                    cache_write_5m: 7,
                    cache_write_1h: 3,
                    cache_read: 0,
                    output: 5,
                    reasoning: 0,
                    audio_input: 11,
                    audio_output: 13,
                },
                shape: crate::wire::anthropic_request_shape(
                    br#"{"model":"claude-opus-4-8","messages":[]}"#,
                    &PrivacyPolicy::default(),
                )
                .unwrap(),
                stop_reason: Some("end_turn".into()),
                duration_ms: 0,
                start_unix_nano: None,
                trace_id: None,
                span_id: None,
                parent_span_id: None,
            }],
        };
        let exported = export_otlp_json(&run, &pricing(), "test");
        let back = ingest_otlp_json(exported.to_string().as_bytes()).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].usage.cache_write_5m, 7);
        assert_eq!(
            back[0].usage.cache_write_1h, 3,
            "1h tier must survive round-trip"
        );
        assert_eq!(back[0].usage.cache_write(), 10);
        assert_eq!(back[0].usage.audio_input, 11);
        assert_eq!(back[0].usage.audio_output, 13);

        let mut huge = run;
        huge.steps[0].usage.fresh_input = u64::MAX;
        let trace = export_otlp_json(&huge, &pricing(), "test").to_string();
        let metrics = export_otlp_metrics_json(&huge, &pricing(), "test").to_string();
        let saturated = format!(r#""intValue":"{}""#, i64::MAX);
        assert!(trace.contains(&saturated));
        assert!(metrics.contains(&format!(r#""asInt":"{}""#, i64::MAX)));
        assert!(!trace.contains(r#""intValue":"-1""#));
    }

    #[test]
    fn export_validates_and_round_trips_lossless_subset() {
        let doc = json!({"resourceSpans": [{"scopeSpans": [{"spans": [
            {"spanId": "a", "startTimeUnixNano": "1000000000", "traceId": "t1", "attributes": [
                {"key": "gen_ai.provider.name", "value": {"stringValue": "anthropic"}},
                {"key": "gen_ai.response.model", "value": {"stringValue": "claude-opus-4-8"}},
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "100"}},
                {"key": "gen_ai.usage.output_tokens", "value": {"intValue": "20"}},
                {"key": "gen_ai.usage.cache_read.input_tokens", "value": {"intValue": "40"}}
            ]}
        ]}]}]});
        let steps = ingest_otlp_json(doc.to_string().as_bytes()).unwrap();
        let recs = otel_steps_to_records(&steps, &PrivacyPolicy::default());
        let run = RunRecord {
            run_id: "t1".into(),
            steps: recs,
        };
        let exported = export_otlp_json(&run, &pricing(), "test");
        validate_otlp_structure(&exported).unwrap();
        // Round-trip: ingest(export(run)) recovers the lossless usage subset.
        let reingested = ingest_otlp_json(exported.to_string().as_bytes()).unwrap();
        assert_eq!(reingested.len(), 1);
        assert_eq!(reingested[0].usage.fresh_input, 100);
        assert_eq!(reingested[0].usage.output, 20);
        assert_eq!(reingested[0].usage.cache_read, 40);
        // No payload strings: every attribute value is an allowed key/count/label.
        let s = exported.to_string();
        assert!(!s.contains("content") && !s.contains("message"));
    }

    #[test]
    fn bedrock_provider_survives_export_then_reingest() {
        // C0 regression: export must emit the semconv value `aws.bedrock` (not the internal
        // `bedrock_converse` tag), so Tare's own export -> ingest round-trip recovers Bedrock.
        let doc = json!({"resourceSpans": [{"scopeSpans": [{"spans": [
            {"spanId": "a", "startTimeUnixNano": "1000000000", "traceId": "t1", "attributes": [
                {"key": "gen_ai.provider.name", "value": {"stringValue": "aws.bedrock"}},
                {"key": "gen_ai.response.model", "value": {"stringValue": "anthropic.claude-opus-4-8"}},
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "10"}},
                {"key": "gen_ai.usage.output_tokens", "value": {"intValue": "5"}}
            ]}
        ]}]}]});
        let steps = ingest_otlp_json(doc.to_string().as_bytes()).unwrap();
        assert_eq!(steps[0].provider, Provider::BedrockConverse);
        let exported = export_otlp_json(
            &RunRecord {
                run_id: "t1".into(),
                steps: otel_steps_to_records(&steps, &PrivacyPolicy::default()),
            },
            &pricing(),
            "test",
        );
        // Export carries the spec value, and re-ingest (which previously failed on the internal
        // tag) recovers Bedrock.
        assert!(exported.to_string().contains("aws.bedrock"));
        let reingested = ingest_otlp_json(exported.to_string().as_bytes()).unwrap();
        assert_eq!(reingested[0].provider, Provider::BedrockConverse);
    }

    #[test]
    fn finish_reasons_array_is_read_and_round_trips() {
        // C3: semconv types finish_reasons as string[]. Ingest must read the array form, and
        // export must emit an array (so the round-trip recovers it).
        let doc = json!({"resourceSpans": [{"scopeSpans": [{"spans": [
            {"spanId": "a", "startTimeUnixNano": "1000000000", "traceId": "t1", "attributes": [
                {"key": "gen_ai.provider.name", "value": {"stringValue": "anthropic"}},
                {"key": "gen_ai.response.model", "value": {"stringValue": "claude-opus-4-8"}},
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "1"}},
                {"key": "gen_ai.response.finish_reasons", "value": {"arrayValue": {"values": [{"stringValue": "end_turn"}]}}}
            ]}
        ]}]}]});
        let steps = ingest_otlp_json(doc.to_string().as_bytes()).unwrap();
        assert_eq!(steps[0].finish_reason.as_deref(), Some("end_turn"));
        let exported = export_otlp_json(
            &RunRecord {
                run_id: "t1".into(),
                steps: otel_steps_to_records(&steps, &PrivacyPolicy::default()),
            },
            &pricing(),
            "test",
        );
        assert!(exported.to_string().contains("arrayValue"));
        assert_eq!(
            ingest_otlp_json(exported.to_string().as_bytes()).unwrap()[0]
                .finish_reason
                .as_deref(),
            Some("end_turn")
        );
    }

    #[test]
    fn lifts_agent_tool_session_labels_from_span_and_log_attrs() {
        // Out-of-band paths must populate parent (agent) and component dimensions
        // (tool) / step (operation) labels so rollup/segments light up without the inline proxy.
        let span = json!({"resourceSpans": [{"scopeSpans": [{"spans": [
            {"spanId": "a", "startTimeUnixNano": "1000000000", "traceId": "t1", "attributes": [
                {"key": "gen_ai.provider.name", "value": {"stringValue": "anthropic"}},
                {"key": "gen_ai.response.model", "value": {"stringValue": "claude-opus-4-8"}},
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "10"}},
                {"key": "gen_ai.agent.name", "value": {"stringValue": "code-reviewer"}},
                {"key": "gen_ai.tool.name", "value": {"stringValue": "Bash"}},
                {"key": "gen_ai.operation.name", "value": {"stringValue": "chat"}},
                {"key": "session.id", "value": {"stringValue": "sess-42"}},
                {"key": "gen_ai.request.reasoning_effort", "value": {"stringValue": "high"}}
            ]}
        ]}]}]});
        let steps = ingest_otlp_json(span.to_string().as_bytes()).unwrap();
        assert_eq!(steps[0].agent.as_deref(), Some("code-reviewer"));
        assert_eq!(steps[0].tool.as_deref(), Some("Bash"));
        assert_eq!(steps[0].session.as_deref(), Some("sess-42"));
        assert_eq!(steps[0].effort.as_deref(), Some("high"));
        let recs = otel_steps_to_records(&steps, &PrivacyPolicy::default());
        assert_eq!(recs[0].shape.parent_label.as_deref(), Some("code-reviewer"));
        assert_eq!(recs[0].shape.component_label.as_deref(), Some("Bash"));
        assert_eq!(recs[0].shape.step_label.as_deref(), Some("chat"));
        assert_eq!(recs[0].shape.effort.as_deref(), Some("high"));

        // Claude Code api_request log events carry the same correlation via vendor attr names.
        let logs = json!({"resourceLogs": [{"scopeLogs": [{"logRecords": [
            {"attributes": [
                {"key": "event.name", "value": {"stringValue": "api_request"}},
                {"key": "session.id", "value": {"stringValue": "sess-9"}},
                {"key": "model", "value": {"stringValue": "claude-opus-4-8"}},
                {"key": "input_tokens", "value": {"intValue": "5"}},
                {"key": "tool_name", "value": {"stringValue": "Read"}}
            ]}
        ]}]}]});
        let lrecs =
            ingest_otlp_logs_json(logs.to_string().as_bytes(), &PrivacyPolicy::default()).unwrap();
        assert_eq!(lrecs[0].shape.component_label.as_deref(), Some("Read"));
    }

    #[test]
    fn lifts_and_truncates_workload_key_from_otel_attr() {
        // `tare.workload_key` (or the bare `workload_key` alias) is lifted onto
        // the shape, truncated to 64 UTF-8-safe chars, and never derived from payload.
        let long = "k".repeat(200);
        let span = json!({"resourceSpans": [{"scopeSpans": [{"spans": [
            {"spanId": "a", "startTimeUnixNano": "1000000000", "traceId": "t1", "attributes": [
                {"key": "gen_ai.provider.name", "value": {"stringValue": "anthropic"}},
                {"key": "gen_ai.response.model", "value": {"stringValue": "claude-opus-4-8"}},
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "10"}},
                {"key": "tare.workload_key", "value": {"stringValue": long}}
            ]}
        ]}]}]});
        let steps = ingest_otlp_json(span.to_string().as_bytes()).unwrap();
        let recs = otel_steps_to_records(&steps, &PrivacyPolicy::default());
        let wk = recs[0].shape.workload_key.as_deref().unwrap();
        assert_eq!(wk.chars().count(), 64, "truncated to 64 chars");
        assert!(wk.chars().all(|c| c == 'k'));

        // Absent key stays optional AND is omitted from serialized shape (byte-stable goldens).
        let bare = json!({"resourceSpans": [{"scopeSpans": [{"spans": [
            {"spanId": "b", "startTimeUnixNano": "1", "traceId": "t2", "attributes": [
                {"key": "gen_ai.provider.name", "value": {"stringValue": "anthropic"}},
                {"key": "gen_ai.response.model", "value": {"stringValue": "claude-opus-4-8"}},
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "10"}}
            ]}
        ]}]}]});
        let s2 = ingest_otlp_json(bare.to_string().as_bytes()).unwrap();
        let r2 = otel_steps_to_records(&s2, &PrivacyPolicy::default());
        assert_eq!(r2[0].shape.workload_key, None);
        let json = serde_json::to_string(&r2[0].shape).unwrap();
        assert!(
            !json.contains("workload_key"),
            "absent key omitted from shape_json"
        );
    }

    #[test]
    fn lifts_otlp_timing_and_span_relationships() {
        // startTimeUnixNano/traceId/spanId/parentSpanId flow onto the record,
        // and the display end is derived from start + observed duration (end−start).
        let start: u128 = 1_700_000_000_000_000_000;
        let end: u128 = start + 2_500_000_000; // +2.5s
        let doc = json!({"resourceSpans": [{"scopeSpans": [{"spans": [
            {"spanId": "child01", "parentSpanId": "root99", "traceId": "trace42",
             "startTimeUnixNano": start.to_string(), "endTimeUnixNano": end.to_string(),
             "attributes": [
                {"key": "gen_ai.provider.name", "value": {"stringValue": "anthropic"}},
                {"key": "gen_ai.response.model", "value": {"stringValue": "claude-opus-4-8"}},
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "10"}}
            ]}
        ]}]}]});
        let steps = ingest_otlp_json(doc.to_string().as_bytes()).unwrap();
        assert_eq!(steps[0].parent_span_id.as_deref(), Some("root99"));
        let recs = otel_steps_to_records(&steps, &PrivacyPolicy::default());
        let r = &recs[0];
        assert_eq!(r.start_unix_nano.map(|n| n.get()), Some(start));
        assert_eq!(r.trace_id.as_deref(), Some("trace42"));
        assert_eq!(r.span_id.as_deref(), Some("child01"));
        assert_eq!(r.parent_span_id.as_deref(), Some("root99"));
        assert_eq!(r.duration_ms, 2500);
        // Display end = start + duration, never a persisted redundant integer.
        assert_eq!(r.end_unix_nano().map(|n| n.get()), Some(end));

        // Timing is suppressed under the latency privacy opt-out; span ids (opaque) survive.
        let masked = otel_steps_to_records(&steps, &PrivacyPolicy::max_private());
        assert_eq!(masked[0].start_unix_nano, None); // no wall-clock under the opt-out
        assert_eq!(masked[0].duration_ms, 0);
        assert_eq!(masked[0].span_id.as_deref(), Some("child01")); // identity is not timing

        let extreme = json!({"resourceSpans": [{"scopeSpans": [{"spans": [
            {"spanId": "wide", "traceId": "trace", "startTimeUnixNano": "0",
             "endTimeUnixNano": u128::MAX.to_string(), "attributes": [
                {"key": "gen_ai.provider.name", "value": {"stringValue": "anthropic"}},
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "1"}}
             ]}
        ]}]}]});
        let extreme_steps = ingest_otlp_json(extreme.to_string().as_bytes()).unwrap();
        assert_eq!(extreme_steps[0].duration_ms, u64::MAX);
    }

    #[test]
    fn unix_nanos_serializes_as_decimal_string() {
        // UnixNanos is ALWAYS a decimal JSON string (JSON numbers lose precision past 2^53).
        let n = crate::model::UnixNanos(1_700_000_000_123_456_789);
        let js = serde_json::to_string(&n).unwrap();
        assert_eq!(js, "\"1700000000123456789\"");
        let back: crate::model::UnixNanos = serde_json::from_str(&js).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn codex_sse_event_response_completed_maps_to_a_step() {
        // Codex CLI cost event: input includes the cached read; cache is read-only (no creation);
        // reasoning is a subset of output. fresh = input - cached.
        let logs = json!({"resourceLogs":[{"scopeLogs":[{"logRecords":[
            {"attributes":[
                {"key":"event.name","value":{"stringValue":"codex.sse_event"}},
                {"key":"event.kind","value":{"stringValue":"response.completed"}},
                {"key":"gen_ai.request.model","value":{"stringValue":"gpt-5.5"}},
                {"key":"input_token_count","value":{"intValue":1000}},
                {"key":"cached_token_count","value":{"intValue":300}},
                {"key":"output_token_count","value":{"intValue":200}},
                {"key":"reasoning_token_count","value":{"intValue":120}},
                {"key":"conversation.id","value":{"stringValue":"conv-7"}}
            ]},
            {"attributes":[
                {"key":"event.name","value":{"stringValue":"codex.api_request"}},
                {"key":"http.status","value":{"stringValue":"200"}}
            ]}
        ]}]}]});
        let recs =
            ingest_otlp_logs_json(logs.to_string().as_bytes(), &PrivacyPolicy::default()).unwrap();
        assert_eq!(recs.len(), 1, "only response.completed yields a cost row");
        let r = &recs[0];
        assert_eq!(r.provider, Provider::Openai);
        assert_eq!(r.model, "gpt-5.5");
        assert_eq!(r.usage.fresh_input, 700); // 1000 - 300 cached
        assert_eq!(r.usage.cache_read, 300);
        assert_eq!(r.usage.cache_write_5m, 0); // read-only cache
        assert_eq!(r.usage.output, 200);
        assert_eq!(r.usage.reasoning, 120);
        assert_eq!(r.run_id, "conv-7"); // grouped by conversation id
        assert!(r.shape.request_hash.is_none()); // degraded, no bogus retry-loop
    }

    #[test]
    fn malformed_attr_values_degrade_to_zero_not_panic() {
        // C16: a negative or non-numeric token count degrades to 0 (never a panic or a wrap), and
        // structurally-empty resource/scope spans are skipped silently.
        let doc = json!({"resourceSpans": [
            {"scopeSpans": [{"spans": [
                {"spanId": "a", "startTimeUnixNano": "1000000000", "traceId": "t1", "attributes": [
                    {"key": "gen_ai.provider.name", "value": {"stringValue": "anthropic"}},
                    {"key": "gen_ai.response.model", "value": {"stringValue": "claude-opus-4-8"}},
                    {"key": "gen_ai.usage.input_tokens", "value": {"intValue": -5}},
                    {"key": "gen_ai.usage.output_tokens", "value": {"stringValue": "not-a-number"}}
                ]}
            ]}]},
            {},                          // resourceSpans entry with no scopeSpans -> skipped
            {"scopeSpans": [{}]}          // scopeSpans entry with no spans -> skipped
        ]});
        let steps = ingest_otlp_json(doc.to_string().as_bytes()).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].usage.fresh_input, 0);
        assert_eq!(steps[0].usage.output, 0);
    }
}
