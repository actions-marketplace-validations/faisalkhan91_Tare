# Running the personal observability mesh

This guide covers Tare's out-of-band capture service and optional self-hosted-model agent.
Everything is local-first. The JSONL, OpenTelemetry, and metrics lanes stay outside the model
request path; only the explicitly selected loopback proxy lane enters it.

## Install

```bash
cargo install --path tare-cli      # puts `tare` on PATH (~/.cargo/bin)
```

Desktop app (native window, same UI): `cargo run -p tare-tauri --features gui`.

## 1. Hub — capture and view

```bash
tare serve --db ~/.tare/tare.db
```

Starts: the web UI at `http://127.0.0.1:8788/__tare/`, the proxy at `:8788`, and the **out-of-band
OTLP receiver at `http://127.0.0.1:4318`** (`--otlp-port` to change). The desktop app reads the same
`~/.tare/tare.db`.

**Always-on (set-and-forget).** To keep the receiver + live-session tracking running across logins
without leaving a terminal open, install it as a macOS LaunchAgent (loopback-only, your machine only):

```bash
tare service install      # writes ~/Library/LaunchAgents/com.tare.serve.plist and loads it
tare service status
tare service uninstall     # removes exactly what install wrote
```

Live session state is persisted to the store, so it survives a restart. **Pulse → Active sessions**
then shows which agent sessions are running now, most-active first.

## 2. Frontier models (Claude Code and Codex)

`tare connect` additively wires Claude Code's settings to export OpenTelemetry to the receiver
(never touches `ANTHROPIC_BASE_URL` or `apiKeyHelper`; refuses to clobber an existing OTLP
endpoint):

```bash
tare connect            # writes .claude/settings.local.json in the cwd (project-local)
# ...restart Claude Code in that dir, use it normally...
tare disconnect         # removes exactly what it added
```

Or set the env yourself (the Capture sheet shows the block): `CLAUDE_CODE_ENABLE_TELEMETRY=1`,
`OTEL_METRICS_EXPORTER=otlp`, `OTEL_LOGS_EXPORTER=otlp`, `OTEL_EXPORTER_OTLP_PROTOCOL=http/json`,
`OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318`. Your Claude spend then appears in Pulse and
Investigate.

> **Inline / one-off (never touches `~/.claude`):** prefix a single invocation instead of writing a
> settings file —
> `CLAUDE_CODE_ENABLE_TELEMETRY=1 OTEL_LOGS_EXPORTER=otlp OTEL_METRICS_EXPORTER=otlp OTEL_EXPORTER_OTLP_PROTOCOL=http/json OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318 claude -p "…"`.

### Codex CLI (OpenAI Responses API)

Codex is **opt-in**: it defaults its log/trace exporters to `none` (metrics to `statsig`), so it
emits nothing until you wire its `[otel]` block. `tare codex-connect` does that additively to
`$CODEX_HOME/config.toml` (default `~/.codex/config.toml`) — format-preserving (keeps your comments,
ordering, and `model`/provider/auth keys), and it refuses to clobber a pre-existing exporter without
`--force`:

```bash
tare codex-connect          # adds [otel.exporter.otlp-http] -> :4318/v1/logs, log_user_prompt=false
# ...restart Codex, use it normally...
tare codex-disconnect       # byte-for-byte removes exactly what it added
```

The exact block it writes (Codex's exporter enum is externally-tagged, so it's a table — **not** a
bare `exporter = "otlp-http"` string):

```toml
[otel]
log_user_prompt = false                          # never export raw prompt text

[otel.exporter.otlp-http]                         # this is the LOG exporter (Codex emits cost as a log event)
endpoint = "http://127.0.0.1:4318/v1/logs"        # per-signal endpoint, used verbatim
protocol = "json"
```

This subtable is TOML-equivalent to the inline tagged-table form in
[Codex observability documentation](https://developers.openai.com/codex/config-advanced/#observability-and-telemetry).

**Headless / one-off (never writes the config file):** inject the same settings at runtime with
`-c` overrides, keeping `~/.codex` untouched for auth:

```bash
codex exec \
  -c 'otel.exporter.otlp-http.endpoint="http://127.0.0.1:4318/v1/logs"' \
  -c 'otel.exporter.otlp-http.protocol="json"' \
  -c 'otel.log_user_prompt=false' \
  "your prompt"
```

Codex reports token usage on `codex.sse_event(event.kind=response.completed)`: `input_token_count`,
`cached_token_count` (read-only cache — no creation/TTL split), `output_token_count`, and
`reasoning_token_count` (a subset of output). Tare maps `fresh_input = input − cached` and groups
runs by conversation id. Confirm capture in the **Capture** sheet (event count ticks up) and in
**Pulse → Active sessions** (the Codex conversation appears, source `otel-event`).

## 3. Self-hosted models (vLLM/llama.cpp on your homelab) — the agent

Run the agent beside the hub so it can post to Tare's loopback-only receiver, and scrape the
inference server's Prometheus `/metrics` endpoint over your LAN or tailnet:

```bash
tare agent \
  --scrape http://homelab:8000/metrics \
  --hub http://127.0.0.1:4318 \
  --identity homelab-1
```

If the homelab metrics endpoint listens only on its own loopback interface, forward it to the hub
first, then scrape the forwarded port:

```bash
ssh -N -L 18000:127.0.0.1:8000 homelab
tare agent --scrape http://127.0.0.1:18000/metrics --hub http://127.0.0.1:4318 --identity homelab-1
```

- Non-invasive (scrape only). Per-interval token deltas become `local`-provider spans on the hub.
- **Durable:** if the hub is unreachable (Mac asleep), batches persist to `--queue`
  (default `~/.tare/agent-queue.jsonl`) and flush when it comes back — no data lost.
- `--once` does a single baseline+delta cycle (testing/cron); default loops every `--interval` (15s).

## 4. Cost for local models (optional overlay)

Local models are **usage-first** (tokens/throughput; unpriced and excluded from priced totals) by
default. To attach a dollar value (cloud-equivalent or your amortized rate), price them via a
pricing file and point `serve` at it:

```bash
tare serve --db ~/.tare/tare.db --pricing local-rates.toml
```

```toml
# local-rates.toml
version = "local"
effective_date = "2026-06-01"
[[model]]
provider = "local"
model_id = "gemma"                # the model_name your server reports
input_micro_per_mtok = 200000     # $0.20 / 1M tokens
output_micro_per_mtok = 600000    # $0.60 / 1M
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
```

## What you see

- **Pulse:** current spend, capture health, anomalies, and active sessions.
- **Investigate:** totals by run, session, prompt template, step, or time; local scrape samples group
  by host identity. `source` tags each step `proxy` | `otel-event` | `otel-span`.
- **Capture:** receiver status, event flow, detected agent wiring, privacy, and next-step remedies.

## Notes / limits

- Degraded (out-of-band) capture has accurate token/cost totals but **no deep prompt-component
  attribution** (system-prompt vs tool-output) — that needs the request body (the opt-in proxy
  path). It correctly shows **no fabricated trim-cause** for OTel data.
- **Liveness is recency-derived.** Neither CLI emits a session-end event, so "working / idle /
  ended" is a window since the last event (≤15s / ≤300s / older), and an idle-but-open session can
  read as ended; a fresh event revives it. Treat session state as a hint, not a fact.
- **Date bucketing defaults to UTC.** Set `[ui].tz_offset_minutes` or
  `TARE_TZ_OFFSET_MINUTES` when capture-day boundaries should follow a fixed local offset.
  Analytical cohort views carry an explicit IANA timezone independently.
- Agent transport is **HTTP**. The hub endpoint stays on loopback; expose the scrape endpoint only
  on a trusted LAN or authenticated tailnet, or reach it through a tunnel. An HTTPS scrape endpoint
  requires TLS support outside Tare.
