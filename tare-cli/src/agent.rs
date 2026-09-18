//! `tare agent`: the homelab side of the personal mesh. Polls a self-hosted
//! inference server's Prometheus `/metrics`, computes per-interval token deltas, and ships them to
//! the hub's OTLP receiver as `Local`-provider spans. Non-invasive (scrape only, never a proxy).
//!
//! The OTLP builder is pure + unit-tested; the scrape/forward loop is exercised by an e2e against a
//! stub. HTTP only for now (homelab LAN / tailnet); an HTTPS scrape would need a TLS feature.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use tare_core::prometheus::{counter_deltas, parse_token_counters, TokenCounters};

/// Build an OTLP/JSON trace export: one GenAI span per model carrying the token delta, tagged with
/// the `local` provider. `trace_id` must be unique per batch (see `unique_trace_id`) so batches
/// don't collide on the hub's per-run step ordinals.
pub fn build_otlp_spans(deltas: &BTreeMap<String, TokenCounters>, trace_id: &str) -> Value {
    let spans: Vec<Value> = deltas
        .iter()
        .map(|(model, d)| {
            json!({
                "traceId": trace_id,
                "attributes": [
                    {"key": "gen_ai.provider.name", "value": {"stringValue": "local"}},
                    {"key": "gen_ai.response.model", "value": {"stringValue": model}},
                    {"key": "gen_ai.usage.input_tokens", "value": {"intValue": d.prompt}},
                    {"key": "gen_ai.usage.output_tokens", "value": {"intValue": d.generation}}
                ]
            })
        })
        .collect();
    json!({ "resourceSpans": [{ "scopeSpans": [{ "spans": spans }] }] })
}

/// A trace id unique to each posted batch, prefixed with the host `identity`. Distinct batches
/// must NOT share a trace id: the span ingest numbers step ordinals from 1 per batch, so a shared
/// trace id collides on `UNIQUE(run_id, step_ordinal)` and later batches overwrite earlier ones.
fn unique_trace_id(identity: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{identity}-{nanos}-{seq}")
}

async fn get_text(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("scrape {url}: {e}"))?;
    resp.text()
        .await
        .map_err(|e| format!("scrape body {url}: {e}"))
}

/// Outcome of one delivery attempt (agent resilience). We split failures by whether a RETRY
/// could ever succeed: a network/connect error or a 5xx is `Retry` (keep the batch — the hub may just
/// be asleep, the whole point of the durable queue); a 4xx means the hub REJECTED this batch and always
/// will, so it's `Drop` — retrying it forever would clog the queue and never drain (a poison batch).
#[derive(Debug)]
enum PostResult {
    Delivered,
    Retry(String),
    Drop(String),
}

async fn post_raw(client: &reqwest::Client, hub: &str, body: &str) -> PostResult {
    let url = format!("{}/v1/traces", hub.trim_end_matches('/'));
    match client
        .post(&url)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
    {
        // Transport error (hub unreachable / asleep / DNS) — transient, keep the batch.
        Err(e) => PostResult::Retry(format!("post {url}: {e}")),
        Ok(resp) => {
            let s = resp.status();
            if s.is_success() {
                PostResult::Delivered
            } else if s.is_client_error() {
                // 4xx: the hub rejected THIS batch (malformed / unsupported) — it'll never accept it.
                PostResult::Drop(format!("post {url}: HTTP {s} — batch rejected, dropping"))
            } else {
                // 5xx / redirects / anything else: treat as transient and retry.
                PostResult::Retry(format!("post {url}: HTTP {s}"))
            }
        }
    }
}

// ---- durable retry queue (one OTLP/JSON batch per line) ----
// Lets the agent survive a sleeping/unreachable hub: failed batches persist and flush next cycle.

fn read_queue(path: &str) -> Vec<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| {
            s.lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn write_queue(path: &str, lines: &[String]) -> std::io::Result<()> {
    if lines.is_empty() {
        let _ = std::fs::remove_file(path);
        return Ok(());
    }
    if let Some(p) = std::path::Path::new(path).parent() {
        if !p.as_os_str().is_empty() {
            std::fs::create_dir_all(p)?;
        }
    }
    // Atomic write: a crash mid-write must never truncate the live queue (durability is the whole
    // point). Write a sibling temp file, then rename over the target (atomic on POSIX).
    let tmp = format!("{path}.tmp");
    std::fs::write(&tmp, format!("{}\n", lines.join("\n")))?;
    std::fs::rename(&tmp, path)
}

fn enqueue(path: &str, body: &str) {
    let mut lines = read_queue(path);
    // JSON from serde_json::to_string is single-line; guard anyway so one batch == one line.
    lines.push(body.replace('\n', " "));
    if let Err(e) = write_queue(path, &lines) {
        eprintln!("tare agent: failed to persist queue {path}: {e}");
    }
}

/// Try to deliver every queued batch. A transient failure (hub asleep) KEEPS the batch — durable
/// across restarts until the hub accepts it. A 4xx REJECTION drops the batch, so one poison batch can't
/// clog the queue forever (agent resilience).
async fn flush_queue(client: &reqwest::Client, hub: &str, path: &str) {
    let lines = read_queue(path);
    if lines.is_empty() {
        return;
    }
    let mut still = Vec::new();
    let mut dropped = 0usize;
    for l in lines {
        match post_raw(client, hub, &l).await {
            PostResult::Delivered => {}
            PostResult::Retry(_) => still.push(l),
            PostResult::Drop(reason) => {
                dropped += 1;
                eprintln!("tare agent: {reason}");
            }
        }
    }
    let _ = write_queue(path, &still);
    if dropped > 0 {
        eprintln!("tare agent: dropped {dropped} rejected batch(es) — won't retry a 4xx");
    }
    if still.is_empty() {
        eprintln!("tare agent: flushed all queued batches");
    } else {
        eprintln!(
            "tare agent: {} batch(es) still queued (hub unreachable)",
            still.len()
        );
    }
}

/// Run the agent. Establishes a baseline scrape first (no forward, so an agent restart never
/// re-counts a server's cumulative history), then forwards per-interval deltas. `once` does a
/// single baseline+delta cycle (for testing / cron-style use).
pub fn agent_command(
    scrape_url: &str,
    hub_url: &str,
    interval_secs: u64,
    identity: &str,
    queue_path: &str,
    once: bool,
) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(async move {
        let client = reqwest::Client::new();
        eprintln!(
            "tare agent: scraping {scrape_url} every {interval_secs}s -> {hub_url} (identity={identity}, queue={queue_path})"
        );
        let mut prev = parse_token_counters(&get_text(&client, scrape_url).await?);
        loop {
            if !once {
                tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
            }
            // Retry anything that didn't deliver before (e.g. hub was asleep).
            flush_queue(&client, hub_url, queue_path).await;
            // A scrape failure (server restarting / unreachable) must not kill the agent.
            let cur = match get_text(&client, scrape_url).await {
                Ok(t) => parse_token_counters(&t),
                Err(e) => {
                    eprintln!("tare agent: scrape failed ({e}); retrying next cycle");
                    if once {
                        break;
                    }
                    continue;
                }
            };
            let deltas = counter_deltas(&prev, &cur);
            if !deltas.is_empty() {
                let body = serde_json::to_string(&build_otlp_spans(&deltas, &unique_trace_id(identity)))
                    .map_err(|e| e.to_string())?;
                match post_raw(&client, hub_url, &body).await {
                    PostResult::Delivered => {
                        let total: u64 = deltas.values().map(|d| d.prompt + d.generation).sum();
                        eprintln!("tare agent: forwarded {} model(s), {total} tokens", deltas.len());
                    }
                    PostResult::Retry(e) => {
                        eprintln!(
                            "tare agent: hub unreachable ({e}); queued {} model(s) for retry",
                            deltas.len()
                        );
                        enqueue(queue_path, &body);
                    }
                    PostResult::Drop(reason) => {
                        // Hub rejected this batch (4xx) — don't queue a poison batch that never drains.
                        eprintln!("tare agent: {reason}");
                    }
                }
            }
            prev = cur;
            if once {
                break;
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_one_local_span_per_model_with_token_deltas() {
        let mut deltas = BTreeMap::new();
        deltas.insert(
            "gemma".to_string(),
            TokenCounters {
                prompt: 200,
                generation: 11,
            },
        );
        let v = build_otlp_spans(&deltas, "homelab-1");
        let span = &v["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert_eq!(span["traceId"], "homelab-1");
        let attrs = span["attributes"].as_array().unwrap();
        let find = |k: &str| {
            attrs
                .iter()
                .find(|a| a["key"] == k)
                .map(|a| a["value"].clone())
                .unwrap()
        };
        assert_eq!(find("gen_ai.provider.name")["stringValue"], "local");
        assert_eq!(find("gen_ai.response.model")["stringValue"], "gemma");
        assert_eq!(find("gen_ai.usage.input_tokens")["intValue"], 200);
        assert_eq!(find("gen_ai.usage.output_tokens")["intValue"], 11);
    }

    #[test]
    fn queue_persists_batches_and_clears_when_empty() {
        let tmp = std::env::temp_dir().join(format!("tare-q-{}.jsonl", std::process::id()));
        let p = tmp.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&p);
        assert!(read_queue(&p).is_empty());
        enqueue(&p, r#"{"a":1}"#);
        enqueue(&p, r#"{"b":2}"#);
        let lines = read_queue(&p);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], r#"{"a":1}"#);
        // Writing an empty set removes the file (nothing pending).
        write_queue(&p, &[]).unwrap();
        assert!(read_queue(&p).is_empty());
        assert!(std::fs::metadata(&p).is_err());
    }

    #[test]
    fn round_trips_through_the_hub_ingest() {
        // The spans the agent builds must parse back to a Local-provider StepRecord on the hub.
        let mut deltas = BTreeMap::new();
        deltas.insert(
            "gemma".to_string(),
            TokenCounters {
                prompt: 50,
                generation: 7,
            },
        );
        let bytes = serde_json::to_vec(&build_otlp_spans(&deltas, "h1")).unwrap();
        let steps = tare_core::otel::ingest_otlp_json(&bytes).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].provider, tare_core::model::Provider::Local);
        assert_eq!(steps[0].model, "gemma");
        assert_eq!(steps[0].usage.fresh_input, 50);
        assert_eq!(steps[0].usage.output, 7);
    }

    #[tokio::test]
    async fn flush_queue_holds_on_failure_and_clears_on_success() {
        // the durability claim — a batch stays queued across a hub outage and is
        // delivered once the hub is reachable, so no local-model usage is lost.
        let tmp = std::env::temp_dir().join(format!("tare-flush-{}.jsonl", std::process::id()));
        let p = tmp.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&p);
        enqueue(&p, r#"{"a":1}"#);
        enqueue(&p, r#"{"b":2}"#);
        let client = reqwest::Client::new();

        // Hub asleep (nothing listening) → delivery fails, both batches stay queued.
        flush_queue(&client, "http://127.0.0.1:1", &p).await;
        assert_eq!(read_queue(&p).len(), 2, "a failed flush keeps every batch");

        // Hub awake (any path → 200) → the queue drains completely.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().fallback(axum::routing::any(|| async { "ok" }));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        flush_queue(&client, &format!("http://{addr}"), &p).await;
        assert!(
            read_queue(&p).is_empty(),
            "a successful flush clears the queue"
        );
        assert!(
            std::fs::metadata(&p).is_err(),
            "empty queue removes the file"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn flush_drops_a_batch_the_hub_rejects_with_4xx() {
        // agent resilience: a hub 4xx means the batch will NEVER be accepted, so DROP it —
        // don't clog the durable queue forever (contrast the asleep/5xx case, which retains for retry).
        let tmp = std::env::temp_dir().join(format!("tare-poison-{}.jsonl", std::process::id()));
        let p = tmp.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&p);
        enqueue(&p, r#"{"bad":true}"#);
        let client = reqwest::Client::new();
        // Every request → 400 (the hub rejects this batch shape).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().fallback(axum::routing::any(|| async {
            axum::http::StatusCode::BAD_REQUEST
        }));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        flush_queue(&client, &format!("http://{addr}"), &p).await;
        assert!(
            read_queue(&p).is_empty(),
            "a 4xx-rejected batch is dropped, not retained forever"
        );
        let _ = std::fs::remove_file(&p);
    }
}
