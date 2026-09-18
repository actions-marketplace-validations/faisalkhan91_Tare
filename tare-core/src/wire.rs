//! Wire parsers for Anthropic + OpenAI. Pinned to the committed fixture schemas.
//! Account ONLY from provider-reported usage; never re-tokenize.

use crate::model::{CacheTtl, Component, ComponentWeight, Provider, RequestShape, UsageTokens};
use crate::privacy::PrivacyPolicy;
use crate::sse;
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) const MAX_MODEL_ID_CHARS: usize = 256;

#[derive(Default)]
struct Weights(BTreeMap<Component, u64>);

impl Weights {
    fn add(&mut self, c: Component, n: u64) {
        if n == 0 {
            return;
        }
        let total = self.0.entry(c).or_insert(0);
        *total = total.saturating_add(n);
    }
    fn into_vec(self) -> Vec<ComponentWeight> {
        // BTreeMap<Component,_> iterates in Component's Ord order → deterministic.
        self.0
            .into_iter()
            .map(|(component, bytes)| ComponentWeight { component, bytes })
            .collect()
    }
}

fn byte_len(s: &str) -> u64 {
    u64::try_from(s.len()).unwrap_or(u64::MAX)
}

fn json_value_len(value: &Value) -> u64 {
    serde_json::to_vec(value)
        .map(|bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn ttl_is_1h(cc: &Value) -> bool {
    cc.get("ttl").and_then(|t| t.as_str()) == Some("1h")
}

fn content_text_len(opt: Option<&Value>) -> u64 {
    match opt {
        Some(Value::String(s)) => byte_len(s),
        Some(Value::Array(arr)) => arr
            .iter()
            .map(|b| {
                if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                    byte_len(t)
                } else {
                    json_value_len(b)
                }
            })
            .fold(0u64, u64::saturating_add),
        Some(other) => json_value_len(other),
        None => 0,
    }
}

fn u64_field(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
}

/// Normalize and bound an untrusted wire/path model id before it reaches stored shape metadata.
/// Bedrock's optional inference-profile region prefix is not part of the pricing identity.
pub(crate) fn normalize_model_id(provider: Provider, raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let normalized = if provider == Provider::BedrockConverse {
        bedrock_pricing_key(raw)
    } else {
        raw.to_string()
    };
    Some(normalized.chars().take(MAX_MODEL_ID_CHARS).collect())
}

// ---------- Anthropic ----------

pub fn anthropic_request_shape(
    bytes: &[u8],
    policy: &PrivacyPolicy,
) -> Result<RequestShape, String> {
    let v: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("anthropic request json: {e}"))?;
    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .and_then(|model| normalize_model_id(Provider::Anthropic, model))
        .unwrap_or_else(|| "unknown".to_string());
    let stream = v.get("stream").and_then(|b| b.as_bool()).unwrap_or(false);
    let mut weights = Weights::default();
    let mut has_cc = false;
    let mut ttl = CacheTtl::FiveMin;
    let mut cached_component: Option<Component> = None;
    let mut sys_text = String::new();

    if let Some(cc) = v.get("cache_control") {
        has_cc = true;
        cached_component.get_or_insert(Component::System);
        if ttl_is_1h(cc) {
            ttl = CacheTtl::OneHour;
        }
    }

    match v.get("system") {
        Some(Value::String(s)) => {
            weights.add(Component::System, byte_len(s));
            sys_text.push_str(s);
        }
        Some(Value::Array(arr)) => {
            for block in arr {
                if let Some(t) = block.get("text").and_then(|x| x.as_str()) {
                    weights.add(Component::System, byte_len(t));
                    sys_text.push_str(t);
                }
                if let Some(cc) = block.get("cache_control") {
                    has_cc = true;
                    cached_component.get_or_insert(Component::System);
                    if ttl_is_1h(cc) {
                        ttl = CacheTtl::OneHour;
                    }
                }
            }
        }
        _ => {}
    }

    if let Some(Value::Array(tools)) = v.get("tools") {
        for tool in tools {
            weights.add(Component::Tools, json_value_len(tool));
            if let Some(cc) = tool.get("cache_control") {
                has_cc = true;
                cached_component.get_or_insert(Component::Tools);
                if ttl_is_1h(cc) {
                    ttl = CacheTtl::OneHour;
                }
            }
        }
    }

    if let Some(Value::Array(msgs)) = v.get("messages") {
        for msg in msgs {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            let role_comp = if role == "assistant" {
                Component::AssistantMessage
            } else {
                Component::UserMessage
            };
            match msg.get("content") {
                Some(Value::String(s)) => weights.add(role_comp, byte_len(s)),
                Some(Value::Array(blocks)) => {
                    for b in blocks {
                        if let Some(cc) = b.get("cache_control") {
                            has_cc = true;
                            cached_component.get_or_insert(role_comp);
                            if ttl_is_1h(cc) {
                                ttl = CacheTtl::OneHour;
                            }
                        }
                        match b.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                            "text" => {
                                let t = b.get("text").and_then(|x| x.as_str()).unwrap_or("");
                                weights.add(role_comp, byte_len(t));
                            }
                            "tool_result" => {
                                weights
                                    .add(Component::ToolResult, content_text_len(b.get("content")));
                            }
                            "tool_use" => {
                                weights.add(Component::AssistantMessage, json_value_len(b));
                            }
                            _ => {
                                weights.add(role_comp, json_value_len(b));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    Ok(RequestShape {
        model,
        provider: Provider::Anthropic,
        stream,
        ttl,
        has_cache_control: has_cc,
        cached_component,
        system_hash: (!sys_text.is_empty())
            .then(|| policy.hash_content(sys_text.as_bytes()))
            .flatten(),
        weights: weights.into_vec(),
        request_hash: policy.hash_request(&v),
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
    })
}

/// Cache-write tokens split by TTL. The response's nested `cache_creation` object is
/// authoritative; the flat field + request TTL is only the fallback.
fn anthropic_cache_writes(u: &Value, ttl: CacheTtl) -> (u64, u64) {
    if let Some(cc) = u.get("cache_creation").and_then(|c| c.as_object()) {
        let m5 = cc
            .get("ephemeral_5m_input_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        let m1 = cc
            .get("ephemeral_1h_input_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        return (m5, m1);
    }
    let flat = u64_field(u, "cache_creation_input_tokens");
    match ttl {
        CacheTtl::OneHour => (0, flat),
        CacheTtl::FiveMin => (flat, 0),
    }
}

fn anthropic_thinking(u: &Value) -> u64 {
    u.get("output_tokens_details")
        .map(|d| u64_field(d, "thinking_tokens"))
        .unwrap_or(0)
}

pub fn anthropic_usage_nonstream(bytes: &[u8], ttl: CacheTtl) -> Result<UsageTokens, String> {
    let v: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("anthropic response json: {e}"))?;
    let u = v
        .get("usage")
        .filter(|usage| usage.is_object())
        .ok_or("anthropic response: missing or invalid usage")?;
    let (w5, w1) = anthropic_cache_writes(u, ttl);
    Ok(UsageTokens {
        fresh_input: u64_field(u, "input_tokens"),
        cache_write_5m: w5,
        cache_write_1h: w1,
        cache_read: u64_field(u, "cache_read_input_tokens"),
        output: u64_field(u, "output_tokens"),
        reasoning: anthropic_thinking(u),
        audio_input: 0,
        audio_output: 0,
    })
}

pub fn anthropic_usage_sse(bytes: &[u8], ttl: CacheTtl) -> Result<UsageTokens, String> {
    let mut usage = UsageTokens::default();
    let mut saw_start = false;
    for ev in sse::parse_events(bytes) {
        let Some(j) = ev.json() else { continue };
        match j.get("type").and_then(|t| t.as_str()) {
            Some("message_start") => {
                if let Some(u) = j
                    .get("message")
                    .and_then(|m| m.get("usage"))
                    .filter(|usage| usage.is_object())
                {
                    usage.fresh_input = u64_field(u, "input_tokens");
                    let (w5, w1) = anthropic_cache_writes(u, ttl);
                    usage.cache_write_5m = w5;
                    usage.cache_write_1h = w1;
                    usage.cache_read = u64_field(u, "cache_read_input_tokens");
                    saw_start = true;
                }
            }
            Some("message_delta") => {
                // Take the FINAL cumulative output_tokens; do not sum across events.
                if let Some(u) = j.get("usage").filter(|usage| usage.is_object()) {
                    if let Some(o) = u.get("output_tokens").and_then(|x| x.as_u64()) {
                        usage.output = o;
                    }
                    let th = anthropic_thinking(u);
                    if th > 0 {
                        usage.reasoning = th;
                    }
                }
            }
            _ => {}
        }
    }
    if !saw_start {
        return Err("anthropic sse: no message_start usage".into());
    }
    Ok(usage)
}

// ---------- OpenAI ----------

pub fn openai_request_shape(bytes: &[u8], policy: &PrivacyPolicy) -> Result<RequestShape, String> {
    let v: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("openai request json: {e}"))?;
    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .and_then(|model| normalize_model_id(Provider::Openai, model))
        .unwrap_or_else(|| "unknown".to_string());
    let stream = v.get("stream").and_then(|b| b.as_bool()).unwrap_or(false);
    let mut weights = Weights::default();
    let mut sys_text = String::new();

    if let Some(Value::Array(tools)) = v.get("tools") {
        for tool in tools {
            weights.add(Component::Tools, json_value_len(tool));
        }
    }

    if let Some(Value::Array(msgs)) = v.get("messages") {
        for msg in msgs {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            let comp = match role {
                // `developer` is the o1/o3/gpt-5-family successor to `system`; both are the
                // cacheable instruction prefix and must bucket to System (not UserMessage).
                "system" | "developer" => Component::System,
                "assistant" => Component::AssistantMessage,
                "tool" => Component::ToolResult,
                _ => Component::UserMessage,
            };
            weights.add(comp, content_text_len(msg.get("content")));
            if comp == Component::System {
                if let Some(s) = msg.get("content").and_then(|c| c.as_str()) {
                    sys_text.push_str(s);
                }
            }
        }
    }

    Ok(RequestShape {
        model,
        provider: Provider::Openai,
        stream,
        ttl: CacheTtl::FiveMin,
        has_cache_control: false,
        cached_component: None,
        system_hash: (!sys_text.is_empty())
            .then(|| policy.hash_content(sys_text.as_bytes()))
            .flatten(),
        weights: weights.into_vec(),
        request_hash: policy.hash_request(&v),
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
    })
}

pub fn openai_request_has_include_usage(v: &Value) -> bool {
    v.get("stream_options")
        .and_then(|o| o.get("include_usage"))
        .and_then(|b| b.as_bool())
        .unwrap_or(false)
}

/// Inject `stream_options.include_usage=true` if the request streams and omits it.
/// Returns the (possibly rewritten) body and whether an injection happened.
pub fn inject_openai_include_usage(bytes: &[u8]) -> (Vec<u8>, bool) {
    let Ok(mut v) = serde_json::from_slice::<Value>(bytes) else {
        return (bytes.to_vec(), false);
    };
    let streaming = v.get("stream").and_then(|b| b.as_bool()).unwrap_or(false);
    if !streaming || openai_request_has_include_usage(&v) {
        return (bytes.to_vec(), false);
    }
    let obj = match v.as_object_mut() {
        Some(o) => o,
        None => return (bytes.to_vec(), false),
    };
    let so = obj
        .entry("stream_options".to_string())
        .or_insert_with(|| Value::Object(Default::default()));
    // A present-but-non-object `stream_options` (null/string/number) can't take `include_usage` via
    // `as_object_mut`, which would leave us reporting injected=true while the streamed usage block is
    // silently lost. Overwrite it with a fresh object so the flag always lands.
    if !so.is_object() {
        *so = Value::Object(Default::default());
    }
    if let Some(so_obj) = so.as_object_mut() {
        so_obj.insert("include_usage".to_string(), Value::Bool(true));
    }
    (
        serde_json::to_vec(&v).unwrap_or_else(|_| bytes.to_vec()),
        true,
    )
}

fn openai_usage_from_obj(u: &Value) -> UsageTokens {
    let prompt = u64_field(u, "prompt_tokens");
    let cached = u
        .get("prompt_tokens_details")
        .map(|d| u64_field(d, "cached_tokens"))
        .unwrap_or(0);
    let reasoning = u
        .get("completion_tokens_details")
        .map(|d| u64_field(d, "reasoning_tokens"))
        .unwrap_or(0);
    // Multimodal audio sub-classes, when the provider meters them separately.
    let audio_input = u
        .get("prompt_tokens_details")
        .map(|d| u64_field(d, "audio_tokens"))
        .unwrap_or(0);
    let audio_output = u
        .get("completion_tokens_details")
        .map(|d| u64_field(d, "audio_tokens"))
        .unwrap_or(0);
    // Audio tokens are a SUBSET of prompt/completion totals — subtract them from the text axes so
    // they aren't billed twice (audio is priced at its own rate). For text-only usage both are 0.
    UsageTokens {
        fresh_input: prompt.saturating_sub(cached).saturating_sub(audio_input),
        cache_write_5m: 0,
        cache_write_1h: 0,
        cache_read: cached,
        output: u64_field(u, "completion_tokens").saturating_sub(audio_output),
        reasoning,
        audio_input,
        audio_output,
    }
}

pub fn openai_usage_nonstream(bytes: &[u8]) -> Result<UsageTokens, String> {
    let v: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("openai response json: {e}"))?;
    let u = v
        .get("usage")
        .filter(|usage| usage.is_object())
        .ok_or("openai response: missing or invalid usage")?;
    Ok(openai_usage_from_obj(u))
}

/// Streaming usage appears only in the final chunk (requires include_usage).
/// Returns None if no usage chunk is present (usage genuinely absent).
pub fn openai_usage_sse(bytes: &[u8]) -> Option<UsageTokens> {
    let mut found = None;
    for ev in sse::parse_events(bytes) {
        let Some(j) = ev.json() else { continue };
        if let Some(u) = j.get("usage").filter(|usage| usage.is_object()) {
            found = Some(openai_usage_from_obj(u));
        }
    }
    found
}

// ---------- Ollama native (/api/chat, /api/generate) ----------
// local OSS models via Ollama's NATIVE API (not its OpenAI-compat shim, which the OpenAI
// parser already handles). The final `done` object carries the counts at the TOP level:
// `prompt_eval_count` = input, `eval_count` = output. Ollama has no prompt caching, so the cache
// classes stay 0 (never fabricated). Counts are real provider-reported usage; cost overlays local
// rates (config LocalOverlay / cost_mode) elsewhere.

/// Map an Ollama `done` object (or a non-stream response) to a usage vector.
pub fn ollama_usage_from_obj(u: &Value) -> UsageTokens {
    UsageTokens {
        fresh_input: u64_field(u, "prompt_eval_count"),
        output: u64_field(u, "eval_count"),
        ..Default::default()
    }
}

pub fn ollama_usage_nonstream(bytes: &[u8]) -> Result<UsageTokens, String> {
    let v: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("ollama response json: {e}"))?;
    if v.get("prompt_eval_count").and_then(Value::as_u64).is_none()
        && v.get("eval_count").and_then(Value::as_u64).is_none()
    {
        return Err("ollama response: usage counts absent or invalid".into());
    }
    Ok(ollama_usage_from_obj(&v))
}

/// Ollama streams newline-delimited JSON; the counts arrive only in the final `"done":true` object.
/// Scan the NDJSON and take the last line carrying a count. `None` when usage is genuinely absent.
pub fn ollama_usage_stream(bytes: &[u8]) -> Option<UsageTokens> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut last = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if v.get("eval_count").and_then(Value::as_u64).is_some()
                || v.get("prompt_eval_count").and_then(Value::as_u64).is_some()
            {
                last = Some(ollama_usage_from_obj(&v));
            }
        }
    }
    last
}

/// Ollama's native completion reason (`done_reason`: "stop" / "length" / …), from the last NDJSON
/// line (or a single object) carrying it. `None` when absent, so the caller can fall back.
pub fn ollama_done_reason(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut last = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if let Some(r) = v.get("done_reason").and_then(|x| x.as_str()) {
                last = Some(r.to_string());
            }
        }
    }
    last
}

/// Ollama native errors are a bare `{"error":"message"}` string (unlike OpenAI's nested object).
/// Returns the message when the body is such an error, else `None`.
pub fn ollama_string_error(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if let Some(Value::String(message)) = v.get("error") {
            return Some(message.clone());
        }
    }
    None
}

// ---------- Cumulative-counter agents (per-turn deltas) ----------
// some agents (e.g. Codex's rollout `total_token_usage`) report usage CUMULATIVELY
// across turns rather than per-request. The per-turn cost needs the increment. These pure helpers
// turn cumulative snapshots into deltas, reset-safe, so a new session / counter reset doesn't emit a
// giant negative-then-wrapping figure. Field-wise, saturating, never negative.

/// Per-field, reset-safe delta of two cumulative usage snapshots: each axis is `curr - prev` when
/// non-decreasing, else `curr` (a decrease means a reset, so the current value IS this turn's usage).
pub fn usage_delta(prev: &UsageTokens, curr: &UsageTokens) -> UsageTokens {
    let d = |p: u64, c: u64| if c >= p { c - p } else { c };
    UsageTokens {
        fresh_input: d(prev.fresh_input, curr.fresh_input),
        cache_write_5m: d(prev.cache_write_5m, curr.cache_write_5m),
        cache_write_1h: d(prev.cache_write_1h, curr.cache_write_1h),
        cache_read: d(prev.cache_read, curr.cache_read),
        output: d(prev.output, curr.output),
        reasoning: d(prev.reasoning, curr.reasoning),
        audio_input: d(prev.audio_input, curr.audio_input),
        audio_output: d(prev.audio_output, curr.audio_output),
    }
}

/// Turn a chronological sequence of CUMULATIVE usage snapshots into per-turn deltas (the first turn
/// is itself, vs a zero baseline). Deterministic; pure.
pub fn cumulative_usage_to_deltas(snapshots: &[UsageTokens]) -> Vec<UsageTokens> {
    let mut out = Vec::with_capacity(snapshots.len());
    let mut prev = UsageTokens::default();
    for s in snapshots {
        out.push(usage_delta(&prev, s));
        prev = *s;
    }
    out
}

// ---------- AWS Bedrock Converse ----------
//
// Converse usage dialect (camelCase): `inputTokens` is fresh (EXCLUDES cache — no subtraction),
// `outputTokens`, `cacheReadInputTokens`, `cacheWriteInputTokens` (split by `cacheDetails[].ttl`
// when present, else 5m). Reasoning tokens are not reported by Converse `usage` (documented gap,
// reported as 0 — never fabricated). Stop reason is the top-level `stopReason`.

/// Strip a leading inference-profile region prefix (`us.`, `eu.`, `apac.`) so the pricing key
/// matches the family id (e.g. `us.amazon.nova-lite-v1:0` -> `amazon.nova-lite-v1:0`).
fn bedrock_pricing_key(model_id: &str) -> String {
    for region in ["us.", "eu.", "apac.", "us-gov."] {
        if let Some(rest) = model_id.strip_prefix(region) {
            return rest.to_string();
        }
    }
    model_id.to_string()
}

pub fn bedrock_request_shape(bytes: &[u8], policy: &PrivacyPolicy) -> Result<RequestShape, String> {
    let v: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("bedrock request json: {e}"))?;
    // Converse carries modelId in the URL path, not the body; some gateways echo it in the
    // body. Use it if present (prefix-stripped), else "unknown" (response model may override).
    let model = v
        .get("modelId")
        .and_then(|m| m.as_str())
        .and_then(|model| normalize_model_id(Provider::BedrockConverse, model))
        .unwrap_or_else(|| "unknown".to_string());
    let mut weights = Weights::default();
    let mut sys_text = String::new();
    if let Some(Value::Array(sys)) = v.get("system") {
        for block in sys {
            if let Some(t) = block.get("text").and_then(|x| x.as_str()) {
                weights.add(Component::System, byte_len(t));
                sys_text.push_str(t);
            }
        }
    }
    if let Some(Value::Array(msgs)) = v.get("messages") {
        for msg in msgs {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            let comp = if role == "assistant" {
                Component::AssistantMessage
            } else {
                Component::UserMessage
            };
            if let Some(Value::Array(blocks)) = msg.get("content") {
                for b in blocks {
                    if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                        weights.add(comp, byte_len(t));
                    } else if let Some(tr) = b.get("toolResult") {
                        weights.add(Component::ToolResult, content_text_len(tr.get("content")));
                    } else {
                        weights.add(comp, json_value_len(b));
                    }
                }
            }
        }
    }
    if let Some(Value::Array(tools)) = v.get("toolConfig").and_then(|config| config.get("tools")) {
        for tool in tools {
            weights.add(Component::Tools, json_value_len(tool));
        }
    }
    Ok(RequestShape {
        model,
        provider: Provider::BedrockConverse,
        stream: false,
        ttl: CacheTtl::FiveMin,
        has_cache_control: false,
        cached_component: None,
        system_hash: (!sys_text.is_empty())
            .then(|| policy.hash_content(sys_text.as_bytes()))
            .flatten(),
        weights: weights.into_vec(),
        request_hash: policy.hash_request(&v),
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
    })
}

/// Map a Converse `usage` object to the six-axis `UsageTokens`.
fn bedrock_usage_from_obj(u: &Value) -> UsageTokens {
    let cache_write = u64_field(u, "cacheWriteInputTokens");
    // Split the cache write by TTL if `cacheDetails[]` is present, else all to 5m (fallback).
    let (mut w5, mut w1) = (cache_write, 0u64);
    if let Some(Value::Array(details)) = u.get("cacheDetails") {
        let (mut s5, mut s1) = (0u64, 0u64);
        let mut saw = false;
        for d in details {
            let toks = u64_field(d, "tokens");
            match d.get("ttl").and_then(|t| t.as_str()) {
                Some("1h") => {
                    s1 = s1.saturating_add(toks);
                    saw = true;
                }
                Some(_) => {
                    s5 = s5.saturating_add(toks);
                    saw = true;
                }
                None => {}
            }
        }
        if saw {
            w5 = s5;
            w1 = s1;
        }
    }
    UsageTokens {
        fresh_input: u64_field(u, "inputTokens"),
        cache_write_5m: w5,
        cache_write_1h: w1,
        cache_read: u64_field(u, "cacheReadInputTokens"),
        output: u64_field(u, "outputTokens"),
        reasoning: 0, // Converse usage does not report reasoning tokens (documented gap).
        audio_input: 0,
        audio_output: 0,
    }
}

pub fn bedrock_usage_nonstream(bytes: &[u8]) -> Result<UsageTokens, String> {
    let v: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("bedrock response json: {e}"))?;
    let u = v
        .get("usage")
        .filter(|usage| usage.is_object())
        .ok_or("bedrock response: missing or invalid usage")?;
    Ok(bedrock_usage_from_obj(u))
}

/// Converse-stream (as SSE via the gateway): usage rides the final `metadata` event.
pub fn bedrock_usage_sse(bytes: &[u8]) -> Option<UsageTokens> {
    let mut found = None;
    for ev in sse::parse_events(bytes) {
        let Some(j) = ev.json() else { continue };
        if let Some(u) = j.get("usage").filter(|usage| usage.is_object()) {
            found = Some(bedrock_usage_from_obj(u));
        }
    }
    found
}

fn bedrock_stop_reason(bytes: &[u8], is_sse: bool) -> Option<String> {
    if is_sse {
        let mut sr = None;
        for ev in sse::parse_events(bytes) {
            if let Some(j) = ev.json() {
                if let Some(s) = j.get("stopReason").and_then(|x| x.as_str()) {
                    sr = Some(s.to_string());
                }
            }
        }
        sr
    } else {
        json_of(bytes)?
            .get("stopReason")
            .and_then(|x| x.as_str())
            .map(str::to_string)
    }
}

fn bedrock_provider_error(bytes: &[u8], is_sse: bool) -> Option<String> {
    if is_sse {
        return sse_error_msg(bytes);
    }
    let v = json_of(bytes)?;
    // Bedrock error bodies carry `__type` (or a top-level `message` with no `usage`).
    if v.get("__type").is_some() || (v.get("message").is_some() && v.get("usage").is_none()) {
        return Some(
            v.get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("error")
                .to_string(),
        );
    }
    None
}

// ---------- Google Gemini (Developer API + Vertex) ----------
//
// `usageMetadata` dialect: `promptTokenCount` INCLUDES cached tokens (subtract
// `cachedContentTokenCount`, saturating, for fresh), `candidatesTokenCount` +
// `thoughtsTokenCount` = output (thoughts billed as output AND tracked as reasoning). No
// per-request cache-write tier (5m/1h = 0). Two stream framings: SSE (`?alt=sse`) and a
// JSON-array body (`streamGenerateContent` without `alt=sse`).

pub fn gemini_request_shape(bytes: &[u8], policy: &PrivacyPolicy) -> Result<RequestShape, String> {
    let v: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("gemini request json: {e}"))?;
    // Gemini carries the model in the URL path (…/models/{model}:generateContent), not the
    // body; use a body `model` if the gateway echoes one, else "unknown".
    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .and_then(|model| normalize_model_id(Provider::Gemini, model))
        .unwrap_or_else(|| "unknown".to_string());
    let mut weights = Weights::default();
    let mut sys_text = String::new();
    let parts_text =
        |parts: Option<&Value>, w: &mut Weights, comp: Component, sink: Option<&mut String>| {
            let mut acc = String::new();
            if let Some(Value::Array(arr)) = parts {
                for p in arr {
                    if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                        w.add(comp, byte_len(t));
                        acc.push_str(t);
                    } else if let Some(fr) = p.get("functionResponse") {
                        w.add(Component::ToolResult, json_value_len(fr));
                    } else {
                        // Function calls, inline/file data, and future part types still consume
                        // input tokens. Attribute their structural bytes instead of dropping them.
                        w.add(comp, json_value_len(p));
                    }
                }
            }
            if let Some(s) = sink {
                s.push_str(&acc);
            }
        };
    if let Some(si) = v.get("systemInstruction") {
        parts_text(
            si.get("parts"),
            &mut weights,
            Component::System,
            Some(&mut sys_text),
        );
    }
    if let Some(Value::Array(contents)) = v.get("contents") {
        for c in contents {
            let role = c.get("role").and_then(|r| r.as_str()).unwrap_or("");
            let comp = if role == "model" {
                Component::AssistantMessage
            } else {
                Component::UserMessage
            };
            parts_text(c.get("parts"), &mut weights, comp, None);
        }
    }
    if let Some(Value::Array(tools)) = v.get("tools") {
        for t in tools {
            weights.add(Component::Tools, json_value_len(t));
        }
    }
    let has_cache = v
        .get("cachedContent")
        .and_then(Value::as_str)
        .is_some_and(|name| !name.trim().is_empty());
    Ok(RequestShape {
        model,
        provider: Provider::Gemini,
        stream: false,
        ttl: CacheTtl::FiveMin,
        has_cache_control: has_cache,
        cached_component: has_cache.then_some(Component::System),
        system_hash: (!sys_text.is_empty())
            .then(|| policy.hash_content(sys_text.as_bytes()))
            .flatten(),
        weights: weights.into_vec(),
        request_hash: policy.hash_request(&v),
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
    })
}

fn gemini_usage_from_obj(u: &Value) -> UsageTokens {
    let prompt = u64_field(u, "promptTokenCount");
    let cached = u64_field(u, "cachedContentTokenCount");
    let candidates = u64_field(u, "candidatesTokenCount");
    let thoughts = u64_field(u, "thoughtsTokenCount");
    UsageTokens {
        // promptTokenCount INCLUDES cached — subtract (saturating) for the fresh remainder.
        fresh_input: prompt.saturating_sub(cached),
        cache_write_5m: 0,
        cache_write_1h: 0,
        cache_read: cached,
        output: candidates.saturating_add(thoughts),
        reasoning: thoughts,
        audio_input: 0,
        audio_output: 0,
    }
}

/// Extract `usageMetadata` from a non-SSE body that may be a single object OR a JSON array
/// (`streamGenerateContent` without `alt=sse`): take the last array element carrying it.
fn gemini_usage_value(v: &Value) -> Option<&Value> {
    match v {
        Value::Array(arr) => arr.iter().rev().find_map(|e| e.get("usageMetadata")),
        _ => v.get("usageMetadata"),
    }
}

pub fn gemini_usage_nonstream(bytes: &[u8]) -> Result<UsageTokens, String> {
    let v: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("gemini response json: {e}"))?;
    let u = gemini_usage_value(&v)
        .filter(|usage| usage.is_object())
        .ok_or("gemini response: missing or invalid usageMetadata")?;
    Ok(gemini_usage_from_obj(u))
}

pub fn gemini_usage_sse(bytes: &[u8]) -> Option<UsageTokens> {
    let mut found = None;
    for ev in sse::parse_events(bytes) {
        if let Some(j) = ev.json() {
            if let Some(u) = j.get("usageMetadata").filter(|usage| usage.is_object()) {
                found = Some(gemini_usage_from_obj(u));
            }
        }
    }
    found
}

fn gemini_stop_reason(bytes: &[u8], is_sse: bool) -> Option<String> {
    // Prefer a prompt-level blockReason (pre-output refusal); else the last finishReason.
    let scan = |v: &Value| -> Option<String> {
        if let Some(br) = v
            .get("promptFeedback")
            .and_then(|p| p.get("blockReason"))
            .and_then(|x| x.as_str())
        {
            return Some(br.to_string());
        }
        v.get("candidates")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|c| c.get("finishReason"))
            .and_then(|x| x.as_str())
            .map(str::to_string)
    };
    if is_sse {
        let mut sr = None;
        for ev in sse::parse_events(bytes) {
            if let Some(j) = ev.json() {
                if let Some(s) = scan(&j) {
                    sr = Some(s);
                }
            }
        }
        sr
    } else {
        let v = json_of(bytes)?;
        match &v {
            Value::Array(arr) => arr.iter().rev().find_map(scan),
            other => scan(other),
        }
    }
}

fn gemini_provider_error(bytes: &[u8], is_sse: bool) -> Option<String> {
    if is_sse {
        return sse_error_msg(bytes);
    }
    let value = json_of(bytes)?;
    match value {
        Value::Array(values) => values.into_iter().find_map(|value| {
            value
                .get("error")
                .filter(|error| error.is_object() || error.is_string())
                .map(|_| err_msg_of(&value))
        }),
        value => value
            .get("error")
            .filter(|error| error.is_object() || error.is_string())
            .map(|_| err_msg_of(&value)),
    }
}

// ---------- Unified (WireParser seam) ----------

/// Last-resort content sniff when no `Content-Type` is available.
fn looks_like_sse(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(16)]);
    let h = head.trim_start();
    h.starts_with("event:") || h.starts_with("data:")
}

/// Decide streaming vs JSON, preferring the response `Content-Type` and sniffing only as a
/// fallback, so `text/event-stream` opening with `: ping` or a BOM is not misclassified.
fn is_sse_response(content_type: Option<&str>, bytes: &[u8]) -> bool {
    if let Some(ct) = content_type {
        let ct = ct.to_ascii_lowercase();
        if ct.contains("text/event-stream") {
            return true;
        }
        if ct.contains("application/json") {
            return false;
        }
    }
    looks_like_sse(bytes)
}

fn json_of(bytes: &[u8]) -> Option<Value> {
    serde_json::from_slice(bytes).ok()
}

fn err_msg_of(v: &Value) -> String {
    v.get("error")
        .and_then(|error| {
            error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
        })
        .or_else(|| v.get("message").and_then(Value::as_str))
        .or_else(|| {
            v.as_object()?.iter().find_map(|(key, value)| {
                let key = key.to_ascii_lowercase();
                (key.ends_with("exception") || key.ends_with("error"))
                    .then(|| value.get("message").and_then(Value::as_str))
                    .flatten()
            })
        })
        .unwrap_or("error")
        .to_string()
}

/// SSE error frame message. Some gateways preserve `event: error`; others emit only a JSON error
/// payload in a normal `data:` frame, so recognize both forms.
fn sse_error_msg(bytes: &[u8]) -> Option<String> {
    for ev in sse::parse_events(bytes) {
        let json = ev.json();
        let payload_is_error = json.as_ref().is_some_and(|value| {
            value.get("type").and_then(Value::as_str) == Some("error")
                || value
                    .get("error")
                    .is_some_and(|error| error.is_object() || error.is_string())
                || value.as_object().is_some_and(|object| {
                    object.keys().any(|key| {
                        let key = key.to_ascii_lowercase();
                        key.ends_with("exception") || key.ends_with("error")
                    })
                })
        });
        if ev.event.as_deref() == Some("error") || payload_is_error {
            return Some(
                json.map(|value| err_msg_of(&value))
                    .unwrap_or("error".into()),
            );
        }
    }
    None
}

fn anthropic_provider_error(bytes: &[u8], is_sse: bool) -> Option<String> {
    if is_sse {
        return sse_error_msg(bytes);
    }
    let v = json_of(bytes)?;
    (v.get("type").and_then(|t| t.as_str()) == Some("error")).then(|| err_msg_of(&v))
}

fn openai_provider_error(bytes: &[u8], is_sse: bool) -> Option<String> {
    if is_sse {
        return sse_error_msg(bytes);
    }
    let v = json_of(bytes)?;
    v.get("error")
        .filter(|error| error.is_object() || error.is_string())
        .map(|_| err_msg_of(&v))
}

/// Anthropic `stop_reason` (final `message_delta` when streaming).
fn anthropic_stop_reason(bytes: &[u8], is_sse: bool) -> Option<String> {
    if is_sse {
        let mut sr = None;
        for ev in sse::parse_events(bytes) {
            if let Some(j) = ev.json() {
                if j.get("type").and_then(|t| t.as_str()) == Some("message_delta") {
                    if let Some(s) = j
                        .get("delta")
                        .and_then(|d| d.get("stop_reason"))
                        .and_then(|x| x.as_str())
                    {
                        sr = Some(s.to_string());
                    }
                }
            }
        }
        sr
    } else {
        json_of(bytes)?
            .get("stop_reason")
            .and_then(|x| x.as_str())
            .map(str::to_string)
    }
}

/// OpenAI `finish_reason` (last seen across choices/chunks).
fn openai_stop_reason(bytes: &[u8], is_sse: bool) -> Option<String> {
    if is_sse {
        let mut fr = None;
        for ev in sse::parse_events(bytes) {
            if let Some(j) = ev.json() {
                if let Some(arr) = j.get("choices").and_then(|c| c.as_array()) {
                    for c in arr {
                        if let Some(f) = c.get("finish_reason").and_then(|x| x.as_str()) {
                            fr = Some(f.to_string());
                        }
                    }
                }
            }
        }
        fr
    } else {
        json_of(bytes)?
            .get("choices")?
            .as_array()?
            .first()?
            .get("finish_reason")
            .and_then(|x| x.as_str())
            .map(str::to_string)
    }
}

/// A provider's wire dialect. One impl per provider; `parse_step` dispatches through
/// `parser_for`, so adding a provider is one impl + one `parser_for` arm (compiler-enforced
/// completeness) rather than edits scattered across the parse path.
pub trait WireParser: Sync {
    fn request_shape(&self, bytes: &[u8], policy: &PrivacyPolicy) -> Result<RequestShape, String>;
    fn usage_nonstream(&self, bytes: &[u8], ttl: CacheTtl) -> Result<UsageTokens, String>;
    fn usage_sse(&self, bytes: &[u8], ttl: CacheTtl) -> Result<UsageTokens, String>;
    fn stop_reason(&self, bytes: &[u8], is_sse: bool) -> Option<String>;
    /// Returns the error message if `bytes` is a provider error body (so the caller records
    /// a zero-usage step rather than treating it as a malformed response).
    fn provider_error(&self, bytes: &[u8], is_sse: bool) -> Option<String>;
}

pub struct AnthropicParser;
pub struct OpenaiParser;

pub struct BedrockConverseParser;
pub struct GeminiParser;
/// Self-hosted models via Ollama. Handles Ollama's NATIVE `/api/chat` + `/api/generate`
/// dialect (`prompt_eval_count` / `eval_count` at the top level of the final object, single-JSON or
/// NDJSON) AND transparently falls back to the OpenAI dialect for Ollama's OpenAI-compat shim.
pub struct OllamaParser;

static ANTHROPIC_PARSER: AnthropicParser = AnthropicParser;
static OPENAI_PARSER: OpenaiParser = OpenaiParser;
static BEDROCK_PARSER: BedrockConverseParser = BedrockConverseParser;
static GEMINI_PARSER: GeminiParser = GeminiParser;
static OLLAMA_PARSER: OllamaParser = OllamaParser;

/// Dispatch to the parser for a provider. The match is the single place a new provider must
/// be wired — the compiler flags any unhandled `Provider` variant here.
pub fn parser_for(provider: Provider) -> &'static dyn WireParser {
    match provider {
        Provider::Anthropic => &ANTHROPIC_PARSER,
        // Azure OpenAI and the OpenAI-compatible long tail (OpenRouter/Groq/Together/…) all speak
        // the byte-identical OpenAI dialect — reuse its parser.
        Provider::Openai | Provider::AzureOpenai | Provider::OpenAiCompatible => &OPENAI_PARSER,
        Provider::BedrockConverse => &BEDROCK_PARSER,
        Provider::Gemini => &GEMINI_PARSER,
        // Self-hosted models: usually captured out-of-band (the homelab agent builds
        // StepRecords from scraped metrics), but Ollama can also be proxied directly — its NATIVE
        // `/api/chat` + `/api/generate` responses (`prompt_eval_count`/`eval_count`) don't carry an
        // OpenAI `usage` block, so the OpenAI parser would silently record zero tokens. `OllamaParser`
        // reads the native counts and falls back to the OpenAI dialect for Ollama's compat shim.
        Provider::Local => &OLLAMA_PARSER,
    }
}

impl WireParser for GeminiParser {
    fn request_shape(&self, bytes: &[u8], policy: &PrivacyPolicy) -> Result<RequestShape, String> {
        gemini_request_shape(bytes, policy)
    }
    fn usage_nonstream(&self, bytes: &[u8], _ttl: CacheTtl) -> Result<UsageTokens, String> {
        gemini_usage_nonstream(bytes)
    }
    fn usage_sse(&self, bytes: &[u8], _ttl: CacheTtl) -> Result<UsageTokens, String> {
        gemini_usage_sse(bytes).ok_or_else(|| "gemini stream: usageMetadata absent".to_string())
    }
    fn stop_reason(&self, bytes: &[u8], is_sse: bool) -> Option<String> {
        gemini_stop_reason(bytes, is_sse)
    }
    fn provider_error(&self, bytes: &[u8], is_sse: bool) -> Option<String> {
        gemini_provider_error(bytes, is_sse)
    }
}

impl WireParser for BedrockConverseParser {
    fn request_shape(&self, bytes: &[u8], policy: &PrivacyPolicy) -> Result<RequestShape, String> {
        bedrock_request_shape(bytes, policy)
    }
    fn usage_nonstream(&self, bytes: &[u8], _ttl: CacheTtl) -> Result<UsageTokens, String> {
        bedrock_usage_nonstream(bytes)
    }
    fn usage_sse(&self, bytes: &[u8], _ttl: CacheTtl) -> Result<UsageTokens, String> {
        bedrock_usage_sse(bytes)
            .ok_or_else(|| "bedrock converse-stream: usage metadata absent".to_string())
    }
    fn stop_reason(&self, bytes: &[u8], is_sse: bool) -> Option<String> {
        bedrock_stop_reason(bytes, is_sse)
    }
    fn provider_error(&self, bytes: &[u8], is_sse: bool) -> Option<String> {
        bedrock_provider_error(bytes, is_sse)
    }
}

/// The model name reported in the RESPONSE (top-level `model`, or the first SSE event's
/// `model` / `message.model`). Used to resolve a real pricing key when the request carried a
/// deployment alias (Azure) or any provider-side rename; falls back to the request model.
fn response_model(provider: Provider, bytes: &[u8], is_sse: bool) -> Option<String> {
    let from_value = |v: &Value| -> Option<String> {
        v.get("model")
            .and_then(|m| m.as_str())
            .or_else(|| {
                v.get("message")
                    .and_then(|m| m.get("model"))
                    .and_then(|m| m.as_str())
            })
            .and_then(|model| normalize_model_id(provider, model))
    };
    if is_sse {
        for ev in sse::parse_events(bytes) {
            if let Some(j) = ev.json() {
                if let Some(m) = from_value(&j) {
                    return Some(m);
                }
            }
        }
        None
    } else {
        from_value(&json_of(bytes)?)
    }
}

impl WireParser for AnthropicParser {
    fn request_shape(&self, bytes: &[u8], policy: &PrivacyPolicy) -> Result<RequestShape, String> {
        anthropic_request_shape(bytes, policy)
    }
    fn usage_nonstream(&self, bytes: &[u8], ttl: CacheTtl) -> Result<UsageTokens, String> {
        anthropic_usage_nonstream(bytes, ttl)
    }
    fn usage_sse(&self, bytes: &[u8], ttl: CacheTtl) -> Result<UsageTokens, String> {
        anthropic_usage_sse(bytes, ttl)
    }
    fn stop_reason(&self, bytes: &[u8], is_sse: bool) -> Option<String> {
        anthropic_stop_reason(bytes, is_sse)
    }
    fn provider_error(&self, bytes: &[u8], is_sse: bool) -> Option<String> {
        anthropic_provider_error(bytes, is_sse)
    }
}

impl WireParser for OpenaiParser {
    fn request_shape(&self, bytes: &[u8], policy: &PrivacyPolicy) -> Result<RequestShape, String> {
        openai_request_shape(bytes, policy)
    }
    fn usage_nonstream(&self, bytes: &[u8], _ttl: CacheTtl) -> Result<UsageTokens, String> {
        openai_usage_nonstream(bytes)
    }
    fn usage_sse(&self, bytes: &[u8], _ttl: CacheTtl) -> Result<UsageTokens, String> {
        openai_usage_sse(bytes)
            .ok_or_else(|| "openai sse: usage absent (include_usage not set upstream)".to_string())
    }
    fn stop_reason(&self, bytes: &[u8], is_sse: bool) -> Option<String> {
        openai_stop_reason(bytes, is_sse)
    }
    fn provider_error(&self, bytes: &[u8], is_sse: bool) -> Option<String> {
        openai_provider_error(bytes, is_sse)
    }
}

impl WireParser for OllamaParser {
    fn request_shape(&self, bytes: &[u8], policy: &PrivacyPolicy) -> Result<RequestShape, String> {
        // Native `/api/chat` + `/api/generate` carry a top-level `model` (+ messages/prompt), enough
        // for the OpenAI shape builder to extract the model and structural weights; the compat shim is
        // the OpenAI dialect outright. One builder serves both.
        openai_request_shape(bytes, policy)
    }
    fn usage_nonstream(&self, bytes: &[u8], _ttl: CacheTtl) -> Result<UsageTokens, String> {
        // Prefer native counts — the final object's `prompt_eval_count`/`eval_count`, whether the body
        // is a single JSON object or NDJSON — then fall back to the OpenAI `usage` block (compat shim).
        if let Some(u) = ollama_usage_stream(bytes) {
            return Ok(u);
        }
        openai_usage_nonstream(bytes)
    }
    fn usage_sse(&self, bytes: &[u8], _ttl: CacheTtl) -> Result<UsageTokens, String> {
        // Ollama's native surface is NDJSON, not SSE; an SSE body here is the compat shim. Try native
        // NDJSON first (harmless), then the OpenAI SSE reader.
        if let Some(u) = ollama_usage_stream(bytes) {
            return Ok(u);
        }
        openai_usage_sse(bytes).ok_or_else(|| "ollama/openai sse: usage absent".to_string())
    }
    fn stop_reason(&self, bytes: &[u8], is_sse: bool) -> Option<String> {
        // Native `done_reason` ("stop"/"length") first; else the OpenAI `finish_reason`.
        ollama_done_reason(bytes).or_else(|| openai_stop_reason(bytes, is_sse))
    }
    fn provider_error(&self, bytes: &[u8], is_sse: bool) -> Option<String> {
        // Native errors are a bare `{"error":"…"}` string; else the OpenAI nested error object.
        ollama_string_error(bytes).or_else(|| openai_provider_error(bytes, is_sse))
    }
}

/// Parse a captured step into its request shape, provider-reported usage, and stop reason.
/// A provider error body is returned as `Err` (the caller records a zero-usage step).
pub fn parse_step(
    provider: Provider,
    request_bytes: &[u8],
    response_bytes: &[u8],
) -> Result<(RequestShape, UsageTokens, Option<String>), String> {
    parse_step_with_content_type(
        provider,
        request_bytes,
        response_bytes,
        None,
        &PrivacyPolicy::default(),
    )
}

/// As [`parse_step`], but prefers the response `Content-Type` over a content sniff to decide
/// streaming vs JSON and applies an explicit privacy policy. The proxy threads the
/// upstream header and the resolved policy here.
pub fn parse_step_with_content_type(
    provider: Provider,
    request_bytes: &[u8],
    response_bytes: &[u8],
    content_type: Option<&str>,
    policy: &PrivacyPolicy,
) -> Result<(RequestShape, UsageTokens, Option<String>), String> {
    let parser = parser_for(provider);
    let is_sse = is_sse_response(content_type, response_bytes);
    if let Some(msg) = parser.provider_error(response_bytes, is_sse) {
        return Err(format!("{} provider error: {msg}", provider.as_str()));
    }
    let stop = parser.stop_reason(response_bytes, is_sse);
    let mut shape = parser.request_shape(request_bytes, policy)?;
    // Several providers intentionally share the OpenAI wire parser. The parser describes the
    // payload dialect, but the caller owns the actual capture/pricing identity (Azure or a named
    // OpenAI-compatible endpoint), so do not leak the parser's `Openai` default into storage.
    shape.provider = provider;
    // Prefer the response-reported model for the pricing key (Azure sends a deployment alias
    // in the request); fall back to the request model when the response omits it. The response
    // model must go through the SAME normalization the request path applied, or a region-prefixed
    // Bedrock modelId (us./eu./apac.) echoed by a gateway silently misses pricing (honest GAP, not
    // a fabricated $0) — mirror bedrock_request_shape's bedrock_pricing_key.
    if let Some(model) = response_model(provider, response_bytes, is_sse) {
        shape.model = model;
    }
    let usage = if is_sse {
        parser.usage_sse(response_bytes, shape.ttl)?
    } else {
        parser.usage_nonstream(response_bytes, shape.ttl)?
    };
    Ok((shape, usage, stop))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cumulative_usage_deltas_are_reset_safe() {
        // cumulative snapshots → per-turn increments; a decrease (reset) takes the
        // current value as the delta rather than underflowing.
        let snap = |i: u64, o: u64| UsageTokens {
            fresh_input: i,
            output: o,
            ..Default::default()
        };
        let deltas = cumulative_usage_to_deltas(&[snap(10, 5), snap(25, 20), snap(5, 2)]);
        assert_eq!(deltas.len(), 3);
        assert_eq!((deltas[0].fresh_input, deltas[0].output), (10, 5)); // vs zero baseline
        assert_eq!((deltas[1].fresh_input, deltas[1].output), (15, 15)); // increment
        assert_eq!((deltas[2].fresh_input, deltas[2].output), (5, 2)); // reset → current value
    }

    #[test]
    fn ollama_native_usage_maps_top_level_counts() {
        // Ollama's done object carries counts at the top level (not under `usage`).
        let done = br#"{"model":"llama3.1","done":true,"prompt_eval_count":26,"eval_count":298}"#;
        let u = ollama_usage_nonstream(done).unwrap();
        assert_eq!(u.fresh_input, 26);
        assert_eq!(u.output, 298);
        // Ollama has no caching — those classes stay zero, never fabricated.
        assert_eq!(u.cache_read, 0);
        assert_eq!(u.cache_write_5m, 0);
        assert_eq!(u.total(), 324);
    }

    #[test]
    fn ollama_stream_takes_the_final_done_object() {
        // NDJSON: intermediate chunks have no counts; the final done object does.
        let ndjson = concat!(
            "{\"model\":\"llama3.1\",\"done\":false,\"response\":\"hi\"}\n",
            "{\"model\":\"llama3.1\",\"done\":false,\"response\":\" there\"}\n",
            "{\"model\":\"llama3.1\",\"done\":true,\"prompt_eval_count\":40,\"eval_count\":120}\n"
        );
        let u = ollama_usage_stream(ndjson.as_bytes()).unwrap();
        assert_eq!(u.fresh_input, 40);
        assert_eq!(u.output, 120);
        // No counts anywhere → None (an honest gap, not a fabricated zero-cost row).
        assert!(ollama_usage_stream(b"{\"done\":false}\n").is_none());
    }

    #[test]
    fn local_provider_parses_native_ollama_end_to_end() {
        // a native Ollama /api/chat round-trip through Provider::Local yields a step
        // with correct input/output tokens + the model — NOT zero (which the OpenAI parser would give,
        // since the native body has no `usage` block).
        let req = br#"{"model":"llama3.1","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = br#"{"model":"llama3.1","done":true,"done_reason":"stop","prompt_eval_count":26,"eval_count":298}"#;
        let (shape, usage, stop) = parse_step(Provider::Local, req, resp).unwrap();
        assert_eq!(shape.model, "llama3.1");
        assert_eq!(usage.fresh_input, 26);
        assert_eq!(usage.output, 298);
        assert_eq!(usage.cache_read, 0); // Ollama has no caching — never fabricated
        assert_eq!(stop.as_deref(), Some("stop")); // native done_reason
    }

    #[test]
    fn local_provider_falls_back_to_openai_compat_shim() {
        // Ollama's OpenAI-compat endpoint returns the OpenAI dialect (usage block); the same
        // Provider::Local parser must still capture those tokens (no native counts present).
        let req = br#"{"model":"llama3.1","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = br#"{"model":"llama3.1","choices":[{"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":22}}"#;
        let (shape, usage, stop) = parse_step(Provider::Local, req, resp).unwrap();
        assert_eq!(shape.model, "llama3.1");
        assert_eq!(usage.fresh_input, 11);
        assert_eq!(usage.output, 22);
        assert_eq!(stop.as_deref(), Some("stop"));
    }

    #[test]
    fn ollama_done_reason_and_string_error_helpers() {
        // done_reason is read from the final NDJSON line; a bare {"error":"…"} string is surfaced.
        let ndjson = concat!(
            "{\"model\":\"m\",\"done\":false}\n",
            "{\"model\":\"m\",\"done\":true,\"done_reason\":\"length\"}\n"
        );
        assert_eq!(
            ollama_done_reason(ndjson.as_bytes()).as_deref(),
            Some("length")
        );
        assert_eq!(ollama_done_reason(b"{\"done\":false}").as_deref(), None);
        assert_eq!(
            ollama_string_error(br#"{"error":"model 'x' not found"}"#).as_deref(),
            Some("model 'x' not found")
        );
        // The OpenAI nested error object is NOT a native string error (handled by the OpenAI reader).
        assert_eq!(
            ollama_string_error(br#"{"error":{"message":"nested"}}"#),
            None
        );
        assert_eq!(
            ollama_string_error(b"{\"done\":false}\n{\"error\":\"stream failed\"}\n").as_deref(),
            Some("stream failed")
        );
    }

    #[test]
    fn malformed_usage_objects_are_rejected_instead_of_becoming_zero_usage() {
        assert!(anthropic_usage_nonstream(br#"{"usage":null}"#, CacheTtl::FiveMin).is_err());
        assert!(openai_usage_nonstream(br#"{"usage":"invalid"}"#).is_err());
        assert!(bedrock_usage_nonstream(br#"{"usage":[]}"#).is_err());
        assert!(gemini_usage_nonstream(br#"{"usageMetadata":null}"#).is_err());
        assert!(ollama_usage_nonstream(br#"{"done":true,"eval_count":-1}"#).is_err());
    }

    #[test]
    fn data_only_sse_error_payloads_are_detected() {
        let openai = b"data: {\"error\":{\"message\":\"rate limited\"}}\n\n";
        assert_eq!(
            openai_provider_error(openai, true).as_deref(),
            Some("rate limited")
        );
        let anthropic = b"data: {\"type\":\"error\",\"error\":{\"message\":\"overloaded\"}}\n\n";
        assert_eq!(
            anthropic_provider_error(anthropic, true).as_deref(),
            Some("overloaded")
        );
    }

    #[test]
    fn openai_audio_tokens_split_out_of_text_axes() {
        // Audio tokens are a subset of prompt/completion — pulled onto their own axis so they
        // aren't double-billed at the text rate.
        let u: Value = serde_json::from_str(
            r#"{"prompt_tokens":1000,"completion_tokens":500,
                "prompt_tokens_details":{"cached_tokens":200,"audio_tokens":300},
                "completion_tokens_details":{"reasoning_tokens":0,"audio_tokens":100}}"#,
        )
        .unwrap();
        let usage = openai_usage_from_obj(&u);
        assert_eq!(usage.cache_read, 200);
        assert_eq!(usage.audio_input, 300);
        assert_eq!(usage.audio_output, 100);
        // fresh = prompt - cached - audio_input = 1000 - 200 - 300 = 500.
        assert_eq!(usage.fresh_input, 500);
        // output = completion - audio_output = 500 - 100 = 400.
        assert_eq!(usage.output, 400);
    }

    #[test]
    fn gemini_usage_metadata_maps_six_axes() {
        // SCHEMA-ONLY (Gemini capture BLOCKED offline — see PROVENANCE). Verifies field
        // mapping: promptTokenCount INCLUDES cached (subtract for fresh); thoughts -> reasoning
        // and output. Numbers verify mapping only, not real costs.
        let req = br#"{"model":"gemini-2.5-flash","systemInstruction":{"parts":[{"text":"sys"}]},
            "contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
        let resp = br#"{"candidates":[{"finishReason":"STOP"}],
            "usageMetadata":{"promptTokenCount":100,"cachedContentTokenCount":30,
            "candidatesTokenCount":20,"thoughtsTokenCount":8,"totalTokenCount":128}}"#;
        let (shape, u, stop) = parse_step(Provider::Gemini, req, resp).unwrap();
        assert_eq!(shape.model, "gemini-2.5-flash");
        assert_eq!(u.fresh_input, 70); // 100 - 30 cached
        assert_eq!(u.cache_read, 30);
        assert_eq!(u.output, 28); // candidates 20 + thoughts 8
        assert_eq!(u.reasoning, 8);
        assert_eq!(u.cache_write_5m, 0);
        assert_eq!(u.cache_write_1h, 0);
        assert_eq!(stop.as_deref(), Some("STOP"));
    }

    #[test]
    fn gemini_shape_counts_non_text_parts_and_ignores_null_cache_name() {
        let request = br#"{
          "model":"gemini-test","cachedContent":null,
          "contents":[{"role":"model","parts":[{"functionCall":{"name":"lookup","args":{"q":"x"}}}]}]
        }"#;
        let shape = gemini_request_shape(request, &PrivacyPolicy::default()).unwrap();
        assert!(!shape.has_cache_control);
        assert!(shape
            .weights
            .iter()
            .any(|weight| weight.component == Component::AssistantMessage && weight.bytes > 0));
    }

    #[test]
    fn gemini_output_addition_saturates() {
        let usage = gemini_usage_from_obj(&serde_json::json!({
            "candidatesTokenCount": u64::MAX,
            "thoughtsTokenCount": u64::MAX
        }));
        assert_eq!(usage.output, u64::MAX);
        assert_eq!(usage.reasoning, u64::MAX);
    }

    #[test]
    fn gemini_json_array_stream_framing() {
        // streamGenerateContent without alt=sse returns a JSON ARRAY; usage rides the last
        // element. is_sse=false, so this exercises the non-SSE array branch.
        let resp = br#"[
            {"candidates":[{"content":{"parts":[{"text":"a"}]}}]},
            {"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":3}}
        ]"#;
        let u = gemini_usage_nonstream(resp).unwrap();
        assert_eq!(u.fresh_input, 5);
        assert_eq!(u.output, 3);
        assert_eq!(gemini_stop_reason(resp, false).as_deref(), Some("STOP"));
    }

    #[test]
    fn gemini_block_reason_is_a_refusal_with_zero_usage() {
        // A pre-output safety block: promptFeedback.blockReason, no usageMetadata.
        let resp = br#"{"promptFeedback":{"blockReason":"SAFETY"}}"#;
        assert_eq!(gemini_stop_reason(resp, false).as_deref(), Some("SAFETY"));
        assert!(gemini_usage_nonstream(resp).is_err()); // no usage -> caller records placeholder
    }

    #[test]
    fn bedrock_converse_schema_maps_six_axes() {
        // Schema-only test: no real capture is available (see fixtures/PROVENANCE.md). Inline JSON
        // follows the documented Converse shape; the numbers verify field mapping, not real costs.
        let req = br#"{
            "modelId":"us.amazon.nova-lite-v1:0",
            "system":[{"text":"sys"}],
            "toolConfig":{"tools":[{"toolSpec":{"name":"lookup","inputSchema":{"json":{"type":"object"}}}}]},
            "messages":[{"role":"user","content":[
                {"text":"hello"},
                {"toolResult":{"content":[{"text":"tool output"}]}}
            ]}]
        }"#;
        let resp = br#"{
            "stopReason":"end_turn",
            "usage":{
                "inputTokens":100,"outputTokens":20,"totalTokens":175,
                "cacheReadInputTokens":40,"cacheWriteInputTokens":15,
                "cacheDetails":[{"ttl":"1h","tokens":10},{"ttl":"5m","tokens":5}]
            }
        }"#;
        let (shape, usage, stop) = parse_step(Provider::BedrockConverse, req, resp).unwrap();
        // Region prefix stripped for the pricing key.
        assert_eq!(shape.model, "amazon.nova-lite-v1:0");
        assert!(shape
            .weights
            .iter()
            .any(|weight| weight.component == Component::Tools && weight.bytes > 0));
        // Six-axis mapping: inputTokens is fresh (no cache subtraction).
        assert_eq!(usage.fresh_input, 100);
        assert_eq!(usage.cache_read, 40);
        assert_eq!(usage.output, 20);
        assert_eq!(usage.reasoning, 0); // documented Converse gap
                                        // cacheDetails split the write across tiers (the first REAL 1h-write coverage).
        assert_eq!(usage.cache_write_1h, 10);
        assert_eq!(usage.cache_write_5m, 5);
        assert_eq!(stop.as_deref(), Some("end_turn"));
    }

    #[test]
    fn bedrock_cache_write_defaults_to_5m_without_details() {
        let resp = br#"{"stopReason":"end_turn","usage":{"inputTokens":5,"outputTokens":1,"cacheWriteInputTokens":12}}"#;
        let u = bedrock_usage_nonstream(resp).unwrap();
        assert_eq!(u.cache_write_5m, 12);
        assert_eq!(u.cache_write_1h, 0);
    }

    #[test]
    fn bedrock_error_body_detected() {
        let err = br#"{"__type":"ThrottlingException","message":"Rate exceeded"}"#;
        assert_eq!(
            bedrock_provider_error(err, false).as_deref(),
            Some("Rate exceeded")
        );
        // A normal response with usage is NOT an error.
        let ok = br#"{"usage":{"inputTokens":1,"outputTokens":1},"stopReason":"end_turn"}"#;
        assert!(bedrock_provider_error(ok, false).is_none());
    }

    #[test]
    fn model_ids_are_trimmed_normalized_and_bounded() {
        assert_eq!(
            normalize_model_id(Provider::BedrockConverse, "  us.amazon.nova-lite-v1:0  ")
                .as_deref(),
            Some("amazon.nova-lite-v1:0")
        );
        assert!(normalize_model_id(Provider::Openai, "  ").is_none());
        let long = "x".repeat(MAX_MODEL_ID_CHARS + 10);
        assert_eq!(
            normalize_model_id(Provider::Openai, &long)
                .unwrap()
                .chars()
                .count(),
            MAX_MODEL_ID_CHARS
        );
    }

    #[test]
    fn response_model_overrides_request_deployment_alias() {
        // Azure-style: the request carries a deployment alias; the response carries the real
        // model. parse_step must use the response model (the pricing key). Parsed via the
        // OpenAI dialect since Azure reuses it.
        let req = br#"{"model":"my-deployment-alias","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = br#"{"model":"gpt-5","choices":[{"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#;
        let (shape, _usage, _stop) = parse_step(Provider::AzureOpenai, req, resp).unwrap();
        assert_eq!(
            shape.model, "gpt-5",
            "pricing key comes from the response model"
        );
        assert_eq!(
            shape.provider,
            Provider::AzureOpenai,
            "shared wire parsing must preserve the caller's provider identity"
        );
    }

    #[test]
    fn bedrock_response_model_is_region_normalized_for_pricing() {
        // A gateway echoes a region-prefixed modelId in the RESPONSE body. The response model wins
        // (like Azure's alias override), but for Bedrock it must be run through the same region-prefix
        // strip the request path applies — else the pricing key misses and the step degrades to an
        // honest GAP instead of matching.
        let req = br#"{"modelId":"us.amazon.nova-lite-v1:0","messages":[{"role":"user","content":[{"text":"hi"}]}]}"#;
        let resp = br#"{"model":"apac.amazon.nova-lite-v1:0","stopReason":"end_turn","usage":{"inputTokens":10,"outputTokens":2}}"#;
        let (shape, _usage, _stop) = parse_step(Provider::BedrockConverse, req, resp).unwrap();
        assert_eq!(
            shape.model, "amazon.nova-lite-v1:0",
            "the echoed region-prefixed response model must be normalized for the pricing key"
        );
    }

    #[test]
    fn inject_include_usage_overwrites_nonobject_stream_options() {
        // stream_options present but null: the old code returned injected=true WITHOUT inserting
        // include_usage, silently losing the streamed usage block. It must overwrite the non-object
        // so the flag always lands.
        let (out, injected) = inject_openai_include_usage(
            br#"{"model":"gpt-5","stream":true,"stream_options":null}"#,
        );
        assert!(injected);
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            v.get("stream_options")
                .and_then(|s| s.get("include_usage"))
                .and_then(|b| b.as_bool()),
            Some(true),
            "include_usage must be inserted even when stream_options was a non-object"
        );
    }

    #[test]
    fn azure_reuses_openai_parser() {
        // The Azure parser IS the OpenAI parser (same dialect).
        let req = include_bytes!("../../fixtures/openai_nonstream/request.json");
        let resp = include_bytes!("../../fixtures/openai_nonstream/response.json");
        let (s_oai, u_oai, st_oai) = parse_step(Provider::Openai, req, resp).unwrap();
        let (s_az, u_az, st_az) = parse_step(Provider::AzureOpenai, req, resp).unwrap();
        assert_eq!(u_oai, u_az);
        assert_eq!(st_oai, st_az);
        assert_eq!(s_oai.provider, Provider::Openai);
        assert_eq!(s_az.provider, Provider::AzureOpenai);
    }

    #[test]
    fn anthropic_stream_merges_usage() {
        // Real capture: a 5m cache write via SSE.
        let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");
        let (shape, usage, _) = parse_step(Provider::Anthropic, req, resp).unwrap();
        assert!(shape.stream);
        assert!(shape.has_cache_control);
        assert_eq!(shape.ttl, CacheTtl::FiveMin);
        assert_eq!(usage.cache_write_5m, 3204);
        assert_eq!(usage.cache_write(), 3204);
        assert_eq!(usage.cache_read, 0);
        assert_eq!(usage.output, 64); // final cumulative, not summed
    }

    #[test]
    fn anthropic_two_turn_cache_write_then_read() {
        let t1r = include_bytes!("../../fixtures/anthropic_cache_two_turn/turn1.request.json");
        let t1s = include_bytes!("../../fixtures/anthropic_cache_two_turn/turn1.response.sse");
        let (s1, u1, _) = parse_step(Provider::Anthropic, t1r, t1s).unwrap();
        // The request asked for ttl=1h, but the Bedrock backend reported a 5m write —
        // the WIRE (nested cache_creation) is authoritative over the request hint.
        assert_eq!(s1.ttl, CacheTtl::OneHour);
        assert_eq!(u1.cache_write_5m, 3215);
        assert_eq!(u1.cache_write_1h, 0);
        assert_eq!(u1.cache_read, 0);

        let t2r = include_bytes!("../../fixtures/anthropic_cache_two_turn/turn2.request.json");
        let t2s = include_bytes!("../../fixtures/anthropic_cache_two_turn/turn2.response.sse");
        let (_s2, u2, _) = parse_step(Provider::Anthropic, t2r, t2s).unwrap();
        assert_eq!(u2.cache_write(), 0);
        assert_eq!(u2.cache_read, 3215);
    }

    #[test]
    fn nested_cache_split_is_authoritative_with_1h_fallback() {
        use serde_json::json;
        // Nested object present and carries BOTH tiers -> authoritative.
        let r = json!({"usage": {"input_tokens": 5, "cache_creation_input_tokens": 300,
            "cache_creation": {"ephemeral_5m_input_tokens": 100, "ephemeral_1h_input_tokens": 200},
            "cache_read_input_tokens": 0, "output_tokens": 7}})
        .to_string();
        let u = anthropic_usage_nonstream(r.as_bytes(), CacheTtl::FiveMin).unwrap();
        assert_eq!(u.cache_write_5m, 100);
        assert_eq!(u.cache_write_1h, 200);
        // No nested object -> flat total assigned by the request-intent TTL (fallback).
        let r2 = json!({"usage": {"input_tokens": 5, "cache_creation_input_tokens": 300,
            "cache_read_input_tokens": 0, "output_tokens": 7}})
        .to_string();
        let u1h = anthropic_usage_nonstream(r2.as_bytes(), CacheTtl::OneHour).unwrap();
        assert_eq!(u1h.cache_write_1h, 300);
        assert_eq!(u1h.cache_write_5m, 0);
    }

    #[test]
    fn anthropic_thinking_tokens_captured() {
        let req = include_bytes!("../../fixtures/anthropic_thinking/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_thinking/response.json");
        let (_s, u, _) = parse_step(Provider::Anthropic, req, resp).unwrap();
        assert!(
            u.reasoning > 0,
            "Anthropic thinking_tokens should be captured"
        );
    }

    #[test]
    fn openai_nonstream_splits_cached() {
        let req = include_bytes!("../../fixtures/openai_nonstream/request.json");
        let resp = include_bytes!("../../fixtures/openai_nonstream/response.json");
        let (_s, u, _) = parse_step(Provider::Openai, req, resp).unwrap();
        assert_eq!(u.fresh_input, 17); // prompt 17 - cached 0
        assert_eq!(u.cache_write(), 0);
        assert_eq!(u.output, 11);
    }

    #[test]
    fn openai_stream_usage_and_reasoning() {
        let req = include_bytes!("../../fixtures/openai_stream_usage/request.json");
        let resp = include_bytes!("../../fixtures/openai_stream_usage/response.sse");
        let (_s, u, _) = parse_step(Provider::Openai, req, resp).unwrap();
        assert_eq!(u.fresh_input, 24);
        assert_eq!(u.output, 674);
        assert_eq!(u.reasoning, 384);
    }

    #[test]
    fn captures_stop_reason_and_detects_provider_errors() {
        // Real fixture: a bloated step that hit the max_tokens cap.
        let req = include_bytes!("../../fixtures/bloated_system_prompt/step1.request.json");
        let resp = include_bytes!("../../fixtures/bloated_system_prompt/step1.response.json");
        let (_s, _u, stop) = parse_step(Provider::Anthropic, req, resp).unwrap();
        assert!(stop.is_some(), "stop_reason should be captured");

        // Anthropic error body -> provider error (Err), never a fabricated usage.
        let aerr = br#"{"type":"error","error":{"type":"overloaded_error","message":"slow down"}}"#;
        assert!(parse_step(Provider::Anthropic, req, aerr).is_err());

        // OpenAI error body likewise.
        let oreq = include_bytes!("../../fixtures/openai_nonstream/request.json");
        let oerr = br#"{"error":{"message":"bad request","type":"invalid_request_error"}}"#;
        assert!(parse_step(Provider::Openai, oreq, oerr).is_err());
    }

    #[test]
    fn injects_include_usage_when_missing() {
        let req = include_bytes!("../../fixtures/openai_stream_no_usage/request.json");
        let (new_body, injected) = inject_openai_include_usage(req);
        assert!(injected);
        let v: Value = serde_json::from_slice(&new_body).unwrap();
        assert!(openai_request_has_include_usage(&v));
    }

    #[test]
    fn does_not_inject_when_present() {
        let req = include_bytes!("../../fixtures/openai_stream_usage/request.json");
        let (_b, injected) = inject_openai_include_usage(req);
        assert!(!injected);
    }
}
