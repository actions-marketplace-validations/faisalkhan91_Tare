# Fixture provenance

**These fixtures are REAL provider wire bytes**, captured 2026-06-25 from a
LiteLLM-compatible proxy (`ANTHROPIC_BASE_URL`), serving native Anthropic
(`claude-opus-4-8`) and OpenAI (`gpt-5-mini`) formats. Re-capture with
`scripts/capture_fixtures.py` (reads the virtual key only from `$TARE_PROXY_KEY`; saves
request/response **bodies only** — no headers, no secret).

Axis coverage (all verified non-zero from the real captures unless noted):

| Case | Provider | Exercises |
|------|----------|-----------|
| `anthropic_nonstream` | Anthropic | fresh input + output |
| `anthropic_stream` | Anthropic | **cache write 5m** (`cache_creation.ephemeral_5m_input_tokens`) via SSE |
| `anthropic_cache_two_turn` | Anthropic | turn1 cache write → turn2 **cache read** (`cache_read_input_tokens`) |
| `anthropic_thinking` | Anthropic | **reasoning** (`output_tokens_details.thinking_tokens`) |
| `bloated_system_prompt` | Anthropic | large uncached system re-sent across 3 steps |
| `verbose_tool_output` | Anthropic | a tool_result dominating the prompt |
| `openai_nonstream` | OpenAI | fresh + output |
| `openai_stream_usage` | OpenAI | streaming usage + **reasoning_tokens** (final chunk) |
| `openai_stream_no_usage` | OpenAI | request omits `include_usage` (proves proxy injection); response carries usage |
| `retry_loop_3x` | OpenAI | identical request issued 3× (retry-loop cause) |

## Real-wire facts worth knowing

- Anthropic `usage` carries the **TTL split on the wire**: `cache_creation.{ephemeral_5m,ephemeral_1h}_input_tokens`,
  plus `output_tokens_details.thinking_tokens`, `service_tier`, and top-level `stop_details`.
- **`ttl:"1h"` is downgraded to 5m by this Bedrock backend** — so the captured fixtures only
  contain `ephemeral_5m` writes. The 1h tier is supported in code and covered by a parser
  **unit** test (a schema-valid hand-built usage object, not a fabricated wire fixture); no
  real `ephemeral_1h` sample exists here.
- OpenAI streaming chunks include real `obfuscation` / `service_tier` fields and a separate
  final `choices:[]` chunk carrying `usage` — parsers ignore unknown fields.
- `gpt-5-mini` is a reasoning model: `completion_tokens_details.reasoning_tokens > 0`.

## Synthetic-but-faithful fixtures (not provider wire bytes)

- `otel/genai_trace.otlp.json` — hand-authored OTLP/JSON GenAI trace. It is NOT captured
  provider wire output (there is none to capture for OTel); it is a schema-faithful OpenTelemetry
  document whose token COUNTS are lifted from the real `anthropic_nonstream` (44 in / 53 out) and
  `openai_nonstream` (17 in / 11 out) captures. Exercises both `gen_ai.provider.name` and the
  deprecated `gen_ai.system` fallback, and `intValue` as both string and number. The counts remain
  real even though the transport envelope is hand-authored.

## Schema-covered formats without real wire fixtures

- **Bedrock Converse (`bedrock_converse_nova`, `bedrock_converse_cache`, `bedrock_invoke_*`)**
  — not capturable in the current offline/loopback environment (no SigV4 path to a live
  Bedrock gateway). The `BedrockConverseParser` is implemented against the **documented**
  Converse `usage` schema (`inputTokens` = fresh/no-subtraction, `cacheReadInputTokens`,
  `cacheWriteInputTokens` split by `cacheDetails[].ttl`, top-level `stopReason`; reasoning is a
  documented Converse gap = 0) and covered by **schema-only unit tests** in `wire.rs`
  (`bedrock_converse_schema_maps_six_axes`, etc.) whose inline JSON verifies field mapping
  only — the numbers are not presented as real captures. Native AWS event-stream binary framing is
  out of scope because it reaches Tare only through a SigV4 connection the proxy cannot intercept;
  streaming is handled when a gateway re-emits it as SSE/JSON.

- **Gemini / Vertex (`gemini_nonstream`, `gemini_thinking`, `gemini_cache_two_turn`,
  `gemini_stream_sse`, `gemini_stream_json_array`)** — no Gemini route observed in the current
  environment. `GeminiParser` is implemented against the documented `usageMetadata` dialect
  (`promptTokenCount` INCLUDES cached → subtract `cachedContentTokenCount` for fresh;
  `candidatesTokenCount`+`thoughtsTokenCount` = output; `thoughtsTokenCount` = reasoning; no
  per-request cache-write tier) and BOTH stream framings (`?alt=sse` and the JSON-array body),
  covered by **schema-only unit tests** in `wire.rs` (mapping, JSON-array branch, `blockReason`
  pre-output refusal). Inline JSON verifies mapping only, not real captures. The proxy extracts the
  model ID from the URL path and passes it into the pricing key; focused proxy tests cover that path.
