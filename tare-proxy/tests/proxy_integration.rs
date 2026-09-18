//! End-to-end: real reqwest client -> Tare proxy -> fake provider (replaying
//! committed fixtures), then parse -> attribute -> cost -> store. Runs entirely on
//! loopback under the `TARE_NETWORK_GUARD=loopback` deny-all guard.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tare_core::model::{Provider, StepRecord};
use tare_core::{attribute, build_runs, PricingTable};
use tare_proxy::fake::FakeHandle;
use tare_proxy::{router, router_with_transcript, ProxyConfig, Sink, TranscriptSink, WriteHandler};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("fixtures")
}
fn read(rel: &str) -> Vec<u8> {
    fs::read(fixtures_dir().join(rel)).unwrap()
}
/// Point both providers at one upstream base (the fake), since tests drive a single fake.
fn both(base: &str) -> std::collections::BTreeMap<Provider, String> {
    let mut m = std::collections::BTreeMap::new();
    m.insert(Provider::Anthropic, base.to_string());
    m.insert(Provider::Openai, base.to_string());
    m.insert(Provider::AzureOpenai, base.to_string());
    m.insert(Provider::BedrockConverse, base.to_string());
    m.insert(Provider::Gemini, base.to_string());
    m.insert(Provider::OpenAiCompatible, base.to_string());
    m
}
fn pricing() -> PricingTable {
    PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml")).unwrap()
}

async fn spawn(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

struct Call {
    run: &'static str,
    path: &'static str,
    req: &'static str,
    resp: &'static str,
    sse: bool,
}

fn plan() -> Vec<Call> {
    use Call as C;
    vec![
        C {
            run: "r-anon",
            path: "/v1/messages",
            req: "anthropic_nonstream/request.json",
            resp: "anthropic_nonstream/response.json",
            sse: false,
        },
        C {
            run: "r-astream",
            path: "/v1/messages",
            req: "anthropic_stream/request.json",
            resp: "anthropic_stream/response.sse",
            sse: true,
        },
        C {
            run: "r-cache",
            path: "/v1/messages",
            req: "anthropic_cache_two_turn/turn1.request.json",
            resp: "anthropic_cache_two_turn/turn1.response.sse",
            sse: true,
        },
        C {
            run: "r-cache",
            path: "/v1/messages",
            req: "anthropic_cache_two_turn/turn2.request.json",
            resp: "anthropic_cache_two_turn/turn2.response.sse",
            sse: true,
        },
        C {
            run: "r-oai",
            path: "/v1/chat/completions",
            req: "openai_nonstream/request.json",
            resp: "openai_nonstream/response.json",
            sse: false,
        },
        C {
            run: "r-oaistream",
            path: "/v1/chat/completions",
            req: "openai_stream_usage/request.json",
            resp: "openai_stream_usage/response.sse",
            sse: true,
        },
        C {
            run: "r-oainous",
            path: "/v1/chat/completions",
            req: "openai_stream_no_usage/request.json",
            resp: "openai_stream_no_usage/response.sse",
            sse: true,
        },
        C {
            run: "r-retry",
            path: "/v1/chat/completions",
            req: "retry_loop_3x/attempt1.request.json",
            resp: "retry_loop_3x/attempt1.response.json",
            sse: false,
        },
        C {
            run: "r-retry",
            path: "/v1/chat/completions",
            req: "retry_loop_3x/attempt2.request.json",
            resp: "retry_loop_3x/attempt2.response.json",
            sse: false,
        },
        C {
            run: "r-retry",
            path: "/v1/chat/completions",
            req: "retry_loop_3x/attempt3.request.json",
            resp: "retry_loop_3x/attempt3.response.json",
            sse: false,
        },
        C {
            run: "r-bloat",
            path: "/v1/messages",
            req: "bloated_system_prompt/step1.request.json",
            resp: "bloated_system_prompt/step1.response.json",
            sse: false,
        },
        C {
            run: "r-bloat",
            path: "/v1/messages",
            req: "bloated_system_prompt/step2.request.json",
            resp: "bloated_system_prompt/step2.response.json",
            sse: false,
        },
        C {
            run: "r-bloat",
            path: "/v1/messages",
            req: "bloated_system_prompt/step3.request.json",
            resp: "bloated_system_prompt/step3.response.json",
            sse: false,
        },
        C {
            run: "r-verbose",
            path: "/v1/messages",
            req: "verbose_tool_output/request.json",
            resp: "verbose_tool_output/response.json",
            sse: false,
        },
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_pipeline_drives_all_fixtures_into_store() {
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");

    let fake = FakeHandle::new();
    let fake_base = spawn(fake.router()).await;

    let captured: Arc<Mutex<Vec<StepRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_store = captured.clone();
    let sink: Sink = Arc::new(move |step| sink_store.lock().unwrap().push(step));

    let config = ProxyConfig {
        upstreams: both(&fake_base),
        run_id_override: None,
        budget: None,
        pricing: None,
        ..ProxyConfig::default()
    };
    let proxy_base = spawn(router(config, sink)).await;

    let client = reqwest::Client::new();
    let plan = plan();

    for call in &plan {
        // Enqueue the matching fixture response immediately before the request (FIFO).
        let body = read(call.resp);
        if call.sse {
            fake.enqueue_sse(body);
        } else {
            fake.enqueue_json(body);
        }
        let resp = client
            .post(format!("{proxy_base}{}", call.path))
            .header("x-tare-run", call.run)
            .header("content-type", "application/json")
            .header("anthropic-version", "2023-06-01")
            .body(read(call.req))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200, "call {} status", call.req);
        // Read the full body to drive the tee to completion (which records the step).
        let _ = resp.bytes().await.unwrap();
    }

    // Wait until all steps are captured.
    let want = plan.len();
    let deadline = std::time::Duration::from_secs(10);
    let start = std::time::Instant::now();
    loop {
        if captured.lock().unwrap().len() >= want {
            break;
        }
        if start.elapsed() > deadline {
            panic!(
                "only captured {} of {want} steps",
                captured.lock().unwrap().len()
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let steps = captured.lock().unwrap().clone();
    assert_eq!(steps.len(), want);

    // --- Every accounting axis is non-zero through the live pipeline ---
    let agg = steps.iter().fold((0u64, 0u64, 0u64, 0u64, 0u64), |a, s| {
        (
            a.0 + s.usage.fresh_input,
            a.1 + s.usage.cache_write(),
            a.2 + s.usage.cache_read,
            a.3 + s.usage.output,
            a.4 + s.usage.reasoning,
        )
    });
    assert!(
        agg.0 > 0 && agg.1 > 0 && agg.2 > 0 && agg.3 > 0 && agg.4 > 0,
        "axes: {agg:?}"
    );

    // --- Anthropic streaming usage merge verified through the tee ---
    let astream = steps.iter().find(|s| s.run_id == "r-astream").unwrap();
    assert_eq!(astream.usage.cache_write_5m, 3204);
    assert_eq!(astream.usage.output, 64);
    assert_eq!(astream.provider, Provider::Anthropic);

    // --- OpenAI reasoning captured from the final streamed usage chunk ---
    let oaistream = steps.iter().find(|s| s.run_id == "r-oaistream").unwrap();
    assert_eq!(oaistream.usage.reasoning, 384);

    // --- include_usage injection: the request that omitted it was rewritten ---
    let received = fake.received();
    let no_usage_fwd = received
        .iter()
        .find(|r| String::from_utf8_lossy(&r.body).contains("one-line status"))
        .expect("no_usage request reached fake");
    let fwd: serde_json::Value = serde_json::from_slice(&no_usage_fwd.body).unwrap();
    assert_eq!(
        fwd["stream_options"]["include_usage"],
        serde_json::json!(true),
        "proxy must inject include_usage"
    );

    // --- Correlation: steps grouped into runs by x-tare-run ---
    let runs = build_runs(steps.clone());
    let retry_run = runs.iter().find(|r| r.run_id == "r-retry").unwrap();
    assert_eq!(retry_run.steps.len(), 3);
    assert_eq!(retry_run.steps[0].step_ordinal, 1);
    assert_eq!(retry_run.steps[2].step_ordinal, 3);

    // --- Persist to store and confirm aggregations survive the round-trip ---
    let store = tare_store::Store::open_in_memory().unwrap();
    for s in &steps {
        store.record_step(s, "2026-06-24").unwrap();
    }
    let pricing = pricing();
    let from_store = store.load_runs().unwrap();
    let report_store = attribute::build_report(&from_store, &pricing);
    let report_direct = attribute::build_report(&runs, &pricing);
    assert_eq!(report_store.total_micros, report_direct.total_micros);
    assert!(report_store.rows.iter().any(|r| r.cause == "retry-loop"));
    assert!(report_store
        .rows
        .iter()
        .any(|r| r.cause == "bloated-system-prompt"));
    assert!(report_store
        .rows
        .iter()
        .any(|r| r.cause == "verbose-tool-output"));
    assert!(report_store
        .rows
        .iter()
        .any(|r| r.cause == "cache-read-vs-write"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn network_guard_blocks_non_loopback() {
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");

    let captured: Arc<Mutex<Vec<StepRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_store = captured.clone();
    let sink: Sink = Arc::new(move |step| sink_store.lock().unwrap().push(step));

    // Non-loopback upstream must be refused by the guard.
    let config = ProxyConfig {
        upstreams: both("http://example.com:9"),
        run_id_override: None,
        budget: None,
        pricing: None,
        ..ProxyConfig::default()
    };
    let proxy_base = spawn(router(config, sink)).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{proxy_base}/v1/messages"))
        .header("x-tare-run", "guarded")
        .body(read("anthropic_nonstream/request.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 502, "non-loopback must be blocked");
    assert!(captured.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bytes_read_handler_serves_binary_assets_and_falls_through() {
    // GET /__tare/*woff2 is served as raw bytes with font/woff2 via the bytes read handler
    // (checked before the text read handler); a path it doesn't claim falls through.
    let sink: Sink = Arc::new(|_step| {});
    let payload: Vec<u8> = b"wOF2\x00\x01\x02\x03fake-font-payload".to_vec();
    let served = payload.clone();
    let config = ProxyConfig {
        bytes_read_handler: Some(tare_proxy::BytesReadHandler(Arc::new(move |pq: &str| {
            let path = pq.split('?').next().unwrap_or(pq);
            (path == "/__tare/assets/fonts/files/x.woff2")
                .then(|| (200u16, "font/woff2".to_string(), served.clone()))
        }))),
        ..ProxyConfig::default()
    };
    let base = spawn(router(config, sink)).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/__tare/assets/fonts/files/x.woff2"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.headers().get("content-type").unwrap(), "font/woff2");
    let body = resp.bytes().await.unwrap();
    assert_eq!(
        body.as_ref(),
        payload.as_slice(),
        "exact WOFF2 bytes served, not corrupted through a String"
    );

    // A path the bytes handler returns None for falls through (404 — no text read handler set here).
    let miss = client
        .get(format!("{base}/__tare/other"))
        .send()
        .await
        .unwrap();
    assert_eq!(miss.status().as_u16(), 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gemini_path_model_becomes_the_pricing_key() {
    // The model ID lives in the URL path; the captured step must carry it rather than "unknown",
    // so it prices instead of silently dropping to $0.
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");
    let fake = FakeHandle::new();
    let fake_base = spawn(fake.router()).await;
    let captured: Arc<Mutex<Vec<StepRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_store = captured.clone();
    let sink: Sink = Arc::new(move |s| sink_store.lock().unwrap().push(s));
    let proxy_base = spawn(router(
        ProxyConfig {
            upstreams: both(&fake_base),
            ..ProxyConfig::default()
        },
        sink,
    ))
    .await;

    fake.enqueue_json(
        br#"{"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5}}"#
            .to_vec(),
    );
    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "{proxy_base}/v1beta/models/gemini-2.5-flash:generateContent"
        ))
        .header("x-tare-run", "gem")
        .header("content-type", "application/json")
        .body(br#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let _ = resp.bytes().await.unwrap();
    for _ in 0..50 {
        if !captured.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let steps = captured.lock().unwrap().clone();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].provider, Provider::Gemini);
    assert_eq!(
        steps[0].model, "gemini-2.5-flash",
        "path model is the pricing key"
    );
    assert!(steps[0].usage.total() > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn correlation_headers_are_captured_on_the_step() {
    // x-tare-* adapter headers land as opaque labels on the StepRecord; the
    // flamegraph step node names itself with the label.
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");
    let fake = FakeHandle::new();
    let fake_base = spawn(fake.router()).await;
    let captured: Arc<Mutex<Vec<StepRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_store = captured.clone();
    let sink: Sink = Arc::new(move |s| sink_store.lock().unwrap().push(s));
    let proxy_base = spawn(router(
        ProxyConfig {
            upstreams: both(&fake_base),
            ..ProxyConfig::default()
        },
        sink,
    ))
    .await;

    fake.enqueue_json(read("anthropic_nonstream/response.json"));
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{proxy_base}/v1/messages"))
        .header("x-tare-run", "labelled")
        .header("x-tare-step", "plan_step")
        .header("x-tare-component", "planner")
        .header("x-tare-attempt", "2")
        .header("content-type", "application/json")
        .body(read("anthropic_nonstream/request.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let _ = resp.bytes().await.unwrap();

    for _ in 0..50 {
        if !captured.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let steps = captured.lock().unwrap().clone();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].shape.step_label.as_deref(), Some("plan_step"));
    assert_eq!(steps[0].shape.component_label.as_deref(), Some("planner"));
    assert_eq!(steps[0].shape.attempt, Some(2));
    // The flamegraph step node now carries the label (additive, only when present).
    let run = tare_core::model::RunRecord {
        run_id: "labelled".into(),
        steps: steps.clone(),
    };
    let model = tare_core::flamegraph::build_flamegraph(&run, &pricing());
    assert!(model.root.children[0].name.contains("plan_step"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn azure_deployment_path_routes_to_openai_parser() {
    // An Azure `/openai/deployments/{dep}/chat/completions` request is captured as
    // AzureOpenai, parsed by the OpenAI dialect, and priced via the response model.
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");
    let fake = FakeHandle::new();
    let fake_base = spawn(fake.router()).await;
    let captured: Arc<Mutex<Vec<StepRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_store = captured.clone();
    let sink: Sink = Arc::new(move |s| sink_store.lock().unwrap().push(s));
    let proxy_base = spawn(router(
        ProxyConfig {
            upstreams: both(&fake_base),
            run_id_override: None,
            budget: None,
            pricing: None,
            ..ProxyConfig::default()
        },
        sink,
    ))
    .await;

    // OpenAI-format response carrying the real model name (deployment alias in the request).
    fake.enqueue_json(read("openai_nonstream/response.json"));
    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "{proxy_base}/openai/deployments/my-deploy/chat/completions?api-version=2024-02-01"
        ))
        .header("x-tare-run", "azure")
        .header("content-type", "application/json")
        .body(read("openai_nonstream/request.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let _ = resp.bytes().await.unwrap();

    for _ in 0..50 {
        if !captured.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let steps = captured.lock().unwrap().clone();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].provider, Provider::AzureOpenai);
    // OpenAI parser produced usage; response model resolved the pricing key.
    assert!(steps[0].usage.total() > 0);
    assert_eq!(steps[0].model, "gpt-5-mini");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn strips_accept_encoding_so_upstream_returns_identity() {
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");
    let fake = FakeHandle::new();
    let fake_base = spawn(fake.router()).await;
    let sink: Sink = Arc::new(|_| {});
    let proxy_base = spawn(router(
        ProxyConfig {
            upstreams: both(&fake_base),
            run_id_override: None,
            budget: None,
            pricing: None,
            ..ProxyConfig::default()
        },
        sink,
    ))
    .await;

    fake.enqueue_json(read("anthropic_nonstream/response.json"));
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{proxy_base}/v1/messages"))
        .header("x-tare-run", "ae")
        .header("accept-encoding", "gzip, br")
        .header("content-type", "application/json")
        .body(read("anthropic_nonstream/request.json"))
        .send()
        .await
        .unwrap();
    let _ = resp.bytes().await.unwrap();

    let recv = fake.received();
    let r = recv
        .iter()
        .find(|r| r.path.contains("/v1/messages"))
        .expect("request reached fake");
    assert!(
        !r.headers.iter().any(|(k, _)| k == "accept-encoding"),
        "proxy must strip accept-encoding; fake saw: {:?}",
        r.headers.iter().map(|(k, _)| k).collect::<Vec<_>>()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unparseable_response_records_zero_usage_step_not_silent_loss() {
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");
    let fake = FakeHandle::new();
    let fake_base = spawn(fake.router()).await;

    let captured: Arc<Mutex<Vec<StepRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_store = captured.clone();
    let sink: Sink = Arc::new(move |s| sink_store.lock().unwrap().push(s));
    let proxy_base = spawn(router(
        ProxyConfig {
            upstreams: both(&fake_base),
            run_id_override: None,
            budget: None,
            pricing: None,
            ..ProxyConfig::default()
        },
        sink,
    ))
    .await;

    // An Anthropic error body has no `usage` — the response parse fails.
    fake.enqueue_json(
        br#"{"type":"error","error":{"type":"overloaded_error","message":"x"}}"#.to_vec(),
    );
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{proxy_base}/v1/messages"))
        .header("x-tare-run", "errrun")
        .header("content-type", "application/json")
        .body(read("anthropic_nonstream/request.json"))
        .send()
        .await
        .unwrap();
    let _ = resp.bytes().await.unwrap();

    // The ordinal is accounted for: a zero-usage placeholder step is recorded, not dropped.
    for _ in 0..50 {
        if !captured.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let steps = captured.lock().unwrap().clone();
    assert_eq!(
        steps.len(),
        1,
        "parse failure must still record a step (no silent loss)"
    );
    assert_eq!(
        steps[0].usage.total(),
        0,
        "placeholder step carries zero usage"
    );
    assert_eq!(steps[0].step_ordinal, 1);
    // A detected provider error body is tagged "error" (distinct from "parse_error").
    assert_eq!(steps[0].stop_reason.as_deref(), Some("error"));
    assert!(steps[0].is_error());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn response_over_capture_cap_forwards_fully_but_records_placeholder() {
    // A response larger than `resp_cap` is forwarded downstream byte-for-byte, but the
    // capture buffer stops growing and the step is recorded as a zero-usage placeholder.
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");
    let fake = FakeHandle::new();
    let fake_base = spawn(fake.router()).await;

    let captured: Arc<Mutex<Vec<StepRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_store = captured.clone();
    let sink: Sink = Arc::new(move |s| sink_store.lock().unwrap().push(s));
    type CapturedTranscript = (String, u32, String, String, bool);
    let transcripts: Arc<Mutex<Vec<CapturedTranscript>>> = Arc::new(Mutex::new(Vec::new()));
    let transcript_store = transcripts.clone();
    let transcript_sink: TranscriptSink = Arc::new(move |run, ordinal, req, resp, truncated| {
        transcript_store
            .lock()
            .unwrap()
            .push((run, ordinal, req, resp, truncated));
    });
    let proxy_base = spawn(router_with_transcript(
        ProxyConfig {
            upstreams: both(&fake_base),
            run_id_override: None,
            budget: None,
            pricing: None,
            resp_cap: 64, // tiny cap so the real fixture exceeds it
            privacy: tare_core::PrivacyPolicy::max_inspect(),
            ..ProxyConfig::default()
        },
        sink,
        Some(transcript_sink),
    ))
    .await;

    let full = read("anthropic_nonstream/response.json");
    assert!(full.len() > 64, "fixture must exceed the test cap");
    fake.enqueue_json(full.clone());
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{proxy_base}/v1/messages"))
        .header("x-tare-run", "bigresp")
        .header("content-type", "application/json")
        .body(read("anthropic_nonstream/request.json"))
        .send()
        .await
        .unwrap();
    let body = resp.bytes().await.unwrap();
    // Downstream client receives the COMPLETE response despite the capture cap.
    assert_eq!(body.len(), full.len(), "client must get the full body");

    for _ in 0..50 {
        if !captured.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let steps = captured.lock().unwrap().clone();
    assert_eq!(steps.len(), 1, "capped response still records its ordinal");
    assert_eq!(steps[0].usage.total(), 0, "placeholder carries zero usage");
    assert_eq!(steps[0].stop_reason.as_deref(), Some("parse_error"));
    let transcripts = transcripts.lock().unwrap();
    assert_eq!(transcripts.len(), 1);
    assert_eq!(transcripts[0].0, "bigresp");
    assert_eq!(transcripts[0].1, 1);
    assert_eq!(transcripts[0].3.as_bytes(), &full[..64]);
    assert!(
        transcripts[0].4,
        "the bounded prefix must be marked incomplete"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upstream_stream_error_keeps_the_redacted_response_prefix() {
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");
    let fake = FakeHandle::new();
    let fake_base = spawn(fake.router()).await;
    let steps: Arc<Mutex<Vec<StepRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let step_store = steps.clone();
    let sink: Sink = Arc::new(move |step| step_store.lock().unwrap().push(step));
    type CapturedTranscript = (String, u32, String, String, bool);
    let transcripts: Arc<Mutex<Vec<CapturedTranscript>>> = Arc::new(Mutex::new(Vec::new()));
    let transcript_store = transcripts.clone();
    let transcript_sink: TranscriptSink = Arc::new(move |run, ordinal, req, resp, truncated| {
        transcript_store
            .lock()
            .unwrap()
            .push((run, ordinal, req, resp, truncated));
    });
    let proxy_base = spawn(router_with_transcript(
        ProxyConfig {
            upstreams: both(&fake_base),
            privacy: tare_core::PrivacyPolicy::max_inspect(),
            ..ProxyConfig::default()
        },
        sink,
        Some(transcript_sink),
    ))
    .await;

    let prefix = br#"{"partial":"safe prefix"#.to_vec();
    fake.enqueue_stream_error("application/json", prefix.clone());
    let response = reqwest::Client::new()
        .post(format!("{proxy_base}/v1/messages"))
        .header("x-tare-run", "broken-stream")
        .header("content-type", "application/json")
        .body(read("anthropic_nonstream/request.json"))
        .send()
        .await
        .unwrap();
    assert!(
        response.bytes().await.is_err(),
        "the downstream stream also fails"
    );

    for _ in 0..50 {
        if !steps.lock().unwrap().is_empty() && !transcripts.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let steps = steps.lock().unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].stop_reason.as_deref(), Some("stream_error"));
    let transcripts = transcripts.lock().unwrap();
    assert_eq!(transcripts.len(), 1);
    assert_eq!(transcripts[0].0, "broken-stream");
    assert_eq!(transcripts[0].3.as_bytes(), prefix.as_slice());
    assert!(
        transcripts[0].4,
        "a stream error must mark the prefix incomplete"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loopback_read_api_serves_and_does_not_forward() {
    // Browser data path: GET /__tare/* is served by the read handler and never forwarded;
    // unknown read paths are 404; non-read paths still forward upstream.
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");
    let fake = FakeHandle::new();
    let fake_base = spawn(fake.router()).await;
    let sink: Sink = Arc::new(|_| {});
    let config = ProxyConfig {
        upstreams: both(&fake_base),
        read_handler: Some(tare_proxy::ReadHandler(Arc::new(|pq: &str| {
            if pq.starts_with("/__tare/trend") {
                Some((
                    200,
                    "application/json".to_string(),
                    format!("{{\"served\":{:?}}}", pq),
                ))
            } else {
                None
            }
        }))),
        ..ProxyConfig::default()
    };
    let proxy_base = spawn(router(config, sink)).await;
    let client = reqwest::Client::new();

    // Served from the store handler, not forwarded (the fake has nothing enqueued).
    let resp = client
        .get(format!("{proxy_base}/__tare/trend?by=provider"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.headers()["content-type"], "application/json");
    let body = resp.text().await.unwrap();
    assert!(body.contains("/__tare/trend?by=provider"));

    // Unknown read path -> 404 (handler returned None).
    let nf = client
        .get(format!("{proxy_base}/__tare/nope"))
        .send()
        .await
        .unwrap();
    assert_eq!(nf.status().as_u16(), 404);

    // The bare /__tare path is also intercepted and never forwarded upstream.
    let bare = client
        .get(format!("{proxy_base}/__tare"))
        .send()
        .await
        .unwrap();
    assert_eq!(bare.status().as_u16(), 404);

    // A POST to /__tare with no write handler configured is 404 (write API not enabled), never
    // forwarded upstream (routes POST to the write branch).
    let bad = client
        .post(format!("{proxy_base}/__tare/trend"))
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status().as_u16(), 404);

    // A normal provider request still forwards + records.
    fake.enqueue_json(read("anthropic_nonstream/response.json"));
    let fwd = client
        .post(format!("{proxy_base}/v1/messages"))
        .header("x-tare-run", "r")
        .header("content-type", "application/json")
        .body(read("anthropic_nonstream/request.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(fwd.status().as_u16(), 200);
}

/// the /__tare API is loopback-only. A forged off-box Host (DNS-rebinding) or an off-box
/// Origin (cross-site) is rejected with 403 before any read/write — a malicious local web page can't
/// reach the control+read API. A normal same-origin request (loopback Host, no forged Origin) is fine.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tare_api_rejects_offbox_host_and_origin() {
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");
    let fake = FakeHandle::new();
    let fake_base = spawn(fake.router()).await;
    let sink: Sink = Arc::new(|_| {});
    let config = ProxyConfig {
        upstreams: both(&fake_base),
        read_handler: Some(tare_proxy::ReadHandler(Arc::new(|_pq: &str| {
            Some((200, "application/json".to_string(), "{}".to_string()))
        }))),
        ..ProxyConfig::default()
    };
    let proxy_base = spawn(router(config, sink)).await;
    let client = reqwest::Client::new();

    // Same-origin (reqwest sets a loopback Host from the URL, no Origin) → served.
    let ok = client
        .get(format!("{proxy_base}/__tare/trend"))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status().as_u16(), 200, "same-origin loopback is served");

    // Rebound attacker domain (forged Host) → 403.
    let rebind = client
        .get(format!("{proxy_base}/__tare/trend"))
        .header(reqwest::header::HOST, "evil.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(rebind.status().as_u16(), 403, "off-box Host is rejected");

    // Cross-site page (loopback Host but off-box Origin) → 403.
    let cross = client
        .get(format!("{proxy_base}/__tare/trend"))
        .header(reqwest::header::ORIGIN, "https://evil.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(cross.status().as_u16(), 403, "off-box Origin is rejected");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn budget_decisions_fire_once_into_the_alert_sink() {
    // The proxy routes budget Decision transitions through alert::evaluate into the
    // configured sink — fire-once, even when the run keeps tripping the cap.
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");
    let fake = FakeHandle::new();
    let fake_base = spawn(fake.router()).await;
    let sink: Sink = Arc::new(|_| {});
    let alerts: Arc<Mutex<Vec<tare_core::alert::Alert>>> = Arc::new(Mutex::new(Vec::new()));
    let cap = alerts.clone();
    let config = ProxyConfig {
        upstreams: both(&fake_base),
        budget: Some(tare_core::budget::Budget {
            max_identical_repeats: Some(2),
            ..Default::default()
        }),
        alert_sink: Some(tare_proxy::AlertSink(Arc::new(move |a| {
            cap.lock().unwrap().push(a)
        }))),
        ..ProxyConfig::default()
    };
    let proxy_base = spawn(router(config, sink)).await;

    let client = reqwest::Client::new();
    let body = read("anthropic_nonstream/request.json");
    fake.enqueue_json(read("anthropic_nonstream/response.json"));
    fake.enqueue_json(read("anthropic_nonstream/response.json"));
    // 4 identical requests: the 3rd and 4th both trip the repeat cap (Kill).
    for _ in 0..4 {
        let resp = client
            .post(format!("{proxy_base}/v1/messages"))
            .header("x-tare-run", "loop")
            .header("content-type", "application/json")
            .body(body.clone())
            .send()
            .await
            .unwrap();
        let _ = resp.bytes().await.unwrap();
    }
    let fired = alerts.lock().unwrap().clone();
    // Exactly ONE Kill alert despite two killed requests (fire-once on the Allow->Kill crossing).
    assert_eq!(fired.len(), 1, "alert must fire once, not per killed step");
    assert_eq!(fired[0].level, tare_core::alert::AlertLevel::Kill);
    assert_eq!(
        fired[0].subject,
        tare_core::alert::AlertSubject::run("loop")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn budget_kills_runaway_identical_requests() {
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");
    let fake = FakeHandle::new();
    let fake_base = spawn(fake.router()).await;
    let sink: Sink = Arc::new(|_| {});
    let config = ProxyConfig {
        upstreams: both(&fake_base),
        run_id_override: None,
        budget: Some(tare_core::budget::Budget {
            max_identical_repeats: Some(2),
            ..Default::default()
        }),
        pricing: None,
        ..ProxyConfig::default()
    };
    let proxy_base = spawn(router(config, sink)).await;

    let client = reqwest::Client::new();
    let body = read("anthropic_nonstream/request.json");
    // Only the first two identical requests should be forwarded.
    fake.enqueue_json(read("anthropic_nonstream/response.json"));
    fake.enqueue_json(read("anthropic_nonstream/response.json"));

    let mut statuses = Vec::new();
    for _ in 0..3 {
        let resp = client
            .post(format!("{proxy_base}/v1/messages"))
            .header("x-tare-run", "loop")
            .header("content-type", "application/json")
            .body(body.clone())
            .send()
            .await
            .unwrap();
        statuses.push(resp.status().as_u16());
        let _ = resp.bytes().await.unwrap();
    }
    assert_eq!(
        statuses,
        vec![200, 200, 429],
        "3rd identical request must be killed"
    );
    assert_eq!(
        fake.received().len(),
        2,
        "killed request must not be forwarded"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn self_governance_headers_carry_integers_and_a_coded_decision() {
    // A forwarded response is stamped with opaque integer x-tare-run-* headers and a coded
    // decision so the agent can read its own running spend mid-run.
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");
    let fake = FakeHandle::new();
    let fake_base = spawn(fake.router()).await;
    let sink: Sink = Arc::new(|_| {});
    let config = ProxyConfig {
        upstreams: both(&fake_base),
        budget: Some(tare_core::budget::Budget {
            max_micros: Some(1_000_000_000),
            ..Default::default()
        }),
        pricing: Some(pricing()),
        ..ProxyConfig::default()
    };
    let proxy_base = spawn(router(config, sink)).await;

    fake.enqueue_json(read("anthropic_nonstream/response.json"));
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{proxy_base}/v1/messages"))
        .header("x-tare-run", "gov")
        .header("content-type", "application/json")
        .body(read("anthropic_nonstream/request.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let h = resp.headers();
    // Budget is echoed verbatim; spend/steps parse as integers; decision is a coded label.
    assert_eq!(h.get("x-tare-run-budget").unwrap(), "1000000000");
    assert_eq!(h.get("x-tare-run-steps").unwrap(), "1");
    assert!(h
        .get("x-tare-run-micros")
        .unwrap()
        .to_str()
        .unwrap()
        .parse::<i64>()
        .is_ok());
    assert_eq!(h.get("x-tare-run-decision").unwrap(), "allow");
    let _ = resp.bytes().await.unwrap();
}

/// an explicit `x-tare-provider` header is trusted over path-sniffing, and `x-tare-vendor`
/// flows onto the captured step's shape so an OpenAI-compatible call prices on vendor+model.
#[tokio::test]
async fn explicit_provider_and_vendor_headers_override_path_sniffing() {
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");
    let fake = FakeHandle::new();
    let fake_base = spawn(fake.router()).await;
    let captured: Arc<Mutex<Vec<StepRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_store = captured.clone();
    let sink: Sink = Arc::new(move |s| sink_store.lock().unwrap().push(s));
    let config = ProxyConfig {
        upstreams: both(&fake_base),
        pricing: Some(pricing()),
        ..ProxyConfig::default()
    };
    let proxy_base = spawn(router(config, sink)).await;

    fake.enqueue_json(read("openai_nonstream/response.json"));
    let client = reqwest::Client::new();
    // The path (/v1/chat/completions) would sniff as plain OpenAI, but the explicit header wins.
    let resp = client
        .post(format!("{proxy_base}/v1/chat/completions"))
        .header("x-tare-run", "oai-compat")
        .header("x-tare-provider", "openai_compatible")
        .header("x-tare-vendor", "Groq") // mixed-case -> lower-cased to a stable pricing key
        .header("content-type", "application/json")
        .body(read("openai_nonstream/request.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let _ = resp.bytes().await.unwrap();

    let steps = captured.lock().unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].provider, Provider::OpenAiCompatible);
    assert_eq!(steps[0].shape.vendor.as_deref(), Some("groq"));
}

#[tokio::test]
async fn malformed_control_identifiers_are_rejected_before_forwarding() {
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");
    let fake = FakeHandle::new();
    let fake_base = spawn(fake.router()).await;
    let proxy_base = spawn(router(
        ProxyConfig {
            upstreams: both(&fake_base),
            ..ProxyConfig::default()
        },
        Arc::new(|_| {}),
    ))
    .await;
    let client = reqwest::Client::new();
    let body = read("anthropic_nonstream/request.json");

    let bad_provider = client
        .post(format!("{proxy_base}/v1/messages"))
        .header("x-tare-provider", "not-a-provider")
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(bad_provider.status().as_u16(), 400);

    let long_run = client
        .post(format!("{proxy_base}/v1/messages"))
        .header("x-tare-run", "r".repeat(513))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(long_run.status().as_u16(), 400);
    assert!(
        fake.received().is_empty(),
        "invalid local control values must never reach the provider"
    );
}

/// POST /__tare/* routes to the write handler (loopback-only mutation), and an oversized
/// body is rejected before the handler runs (memory-safety cap).
#[tokio::test]
async fn write_api_routes_post_to_the_write_handler() {
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");
    type Seen = Arc<Mutex<Vec<(String, Vec<u8>)>>>; // (path, body) recorded per POST
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let write_handler = Some(WriteHandler(Arc::new(move |pq: &str, body: &[u8]| {
        seen2.lock().unwrap().push((pq.to_string(), body.to_vec()));
        (
            200u16,
            "application/json".to_string(),
            "{\"ok\":true}".to_string(),
        )
    })));
    let config = ProxyConfig {
        write_handler,
        ..ProxyConfig::default()
    };
    let sink: Sink = Arc::new(|_| {});
    let proxy_base = spawn(router(config, sink)).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{proxy_base}/__tare/notes"))
        .body("{\"run_id\":\"r1\"}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    {
        let got = seen.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "/__tare/notes");
        assert_eq!(got[0].1, b"{\"run_id\":\"r1\"}");
    }

    // Bodies above the old 64 KiB cap are valid: analysis endpoints share a 256 KiB limit.
    let medium_body = "x".repeat(128 * 1024);
    let medium = client
        .post(format!("{proxy_base}/__tare/analysis"))
        .body(medium_body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(medium.status().as_u16(), 200);
    assert_eq!(seen.lock().unwrap()[1].1.len(), medium_body.len());

    // Over 256 KiB -> 413 before the handler is invoked.
    let too_big = client
        .post(format!("{proxy_base}/__tare/notes"))
        .body("x".repeat(256 * 1024 + 1))
        .send()
        .await
        .unwrap();
    assert_eq!(too_big.status().as_u16(), 413);
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn invalid_local_handler_content_type_returns_500_without_panicking() {
    let config = ProxyConfig {
        read_handler: Some(tare_proxy::ReadHandler(Arc::new(|_| {
            Some((200, "text/plain\ninvalid".to_string(), "body".to_string()))
        }))),
        ..ProxyConfig::default()
    };
    let proxy_base = spawn(router(config, Arc::new(|_| {}))).await;
    let response = reqwest::get(format!("{proxy_base}/__tare/example"))
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 500);
    assert_eq!(
        response.text().await.unwrap(),
        "tare: local handler returned an invalid content type"
    );
}

/// an `x-tare-quality: <int>` request header forwards `(run_id, score)` to the quality
/// sink; a missing or unparseable header reports nothing. Runs entirely on loopback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quality_header_forwards_run_score_to_the_sink() {
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");
    let fake = FakeHandle::new();
    let fake_base = spawn(fake.router()).await;

    let captured: Arc<Mutex<Vec<(String, i64)>>> = Arc::new(Mutex::new(Vec::new()));
    let qstore = captured.clone();
    let config = ProxyConfig {
        upstreams: both(&fake_base),
        run_id_override: None,
        budget: None,
        pricing: None,
        quality_sink: Some(tare_proxy::QualitySink(Arc::new(move |run, score| {
            qstore.lock().unwrap().push((run, score));
        }))),
        ..ProxyConfig::default()
    };
    let sink: Sink = Arc::new(|_step| {}); // steps are irrelevant to this test
    let proxy_base = spawn(router(config, sink)).await;

    let client = reqwest::Client::new();
    let req = read("bloated_system_prompt/step1.request.json");
    let resp_body = read("bloated_system_prompt/step1.response.json");
    let send = |run: &'static str, quality: Option<&'static str>, body: Vec<u8>| {
        let client = client.clone();
        let base = proxy_base.clone();
        async move {
            let mut rb = client
                .post(format!("{base}/v1/messages"))
                .header("x-tare-run", run)
                .header("content-type", "application/json")
                .header("anthropic-version", "2023-06-01");
            if let Some(q) = quality {
                rb = rb.header("x-tare-quality", q);
            }
            let r = rb.body(body).send().await.unwrap();
            let _ = r.bytes().await.unwrap();
        }
    };

    // Valid header is reported; missing, malformed, and out-of-range values are ignored.
    fake.enqueue_json(resp_body.clone());
    send("run-q", Some("87"), req.clone()).await;
    fake.enqueue_json(resp_body.clone());
    send("run-noq", None, req.clone()).await;
    fake.enqueue_json(resp_body.clone());
    send("run-bad", Some("not-a-number"), req.clone()).await;
    fake.enqueue_json(resp_body.clone());
    send("run-low", Some("-1"), req.clone()).await;
    fake.enqueue_json(resp_body);
    send("run-high", Some("101"), req).await;

    // The sink fires synchronously in the handler (before the response returns), so it's settled.
    let got = captured.lock().unwrap().clone();
    assert_eq!(
        got,
        vec![("run-q".to_string(), 87)],
        "only the valid x-tare-quality header reports a run score"
    );
}

/// a chained upstream mounted at a BASE PATH (a corporate gateway at `/llm-proxy`) — assert
/// the proxy concatenates base + request path correctly and forwards the caller's auth headers
/// verbatim (so the gateway still authenticates), a regression guard for the base-path/auth path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chained_upstream_preserves_base_path_and_forwards_auth() {
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");
    let fake = FakeHandle::new();
    let fake_base = spawn(fake.router()).await;
    let sink: Sink = Arc::new(|_step| {});
    let mut upstreams = both(&fake_base);
    upstreams.insert(Provider::Anthropic, format!("{fake_base}/llm-proxy")); // gateway base path
    let config = ProxyConfig {
        upstreams,
        ..ProxyConfig::default()
    };
    let proxy_base = spawn(router(config, sink)).await;

    fake.enqueue_json(read("anthropic_nonstream/response.json"));
    let resp = reqwest::Client::new()
        .post(format!("{proxy_base}/v1/messages"))
        .header("x-tare-run", "r-basepath")
        .header("x-tare-provider", "anthropic")
        .header("x-tare-step", "gateway-call")
        .header("x-tare-quality", "92")
        .header("content-type", "application/json")
        .header("anthropic-version", "2023-06-01")
        .header("authorization", "Bearer sk-user-secret")
        .header("x-api-key", "user-key-123")
        .body(read("anthropic_nonstream/request.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let _ = resp.bytes().await.unwrap();

    let received = fake.received();
    let last = received.last().expect("request reached the fake upstream");
    // Base path preserved: gateway base + the request path (not a naked /v1/messages).
    assert_eq!(last.path, "/llm-proxy/v1/messages");
    let hdr = |name: &str| {
        last.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    // Caller auth headers reach the upstream verbatim (the gateway still authenticates).
    assert_eq!(hdr("authorization"), Some("Bearer sk-user-secret"));
    assert_eq!(hdr("x-api-key"), Some("user-key-123"));
    assert!(
        last.headers
            .iter()
            .all(|(name, _)| !name.starts_with("x-tare-")),
        "proxy control and attribution headers must stay local"
    );
}

/// SSE integrity through a base-path chained upstream — the streamed bytes pass through
/// downstream unchanged AND the tee still parses usage into a captured step.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_passes_through_a_chained_base_path_upstream() {
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");
    let fake = FakeHandle::new();
    let fake_base = spawn(fake.router()).await;
    let captured: Arc<Mutex<Vec<StepRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_store = captured.clone();
    let sink: Sink = Arc::new(move |s| sink_store.lock().unwrap().push(s));
    let mut upstreams = both(&fake_base);
    upstreams.insert(Provider::Anthropic, format!("{fake_base}/llm-proxy"));
    let config = ProxyConfig {
        upstreams,
        ..ProxyConfig::default()
    };
    let proxy_base = spawn(router(config, sink)).await;

    let sse = read("anthropic_stream/response.sse");
    fake.enqueue_sse(sse.clone());
    let resp = reqwest::Client::new()
        .post(format!("{proxy_base}/v1/messages"))
        .header("x-tare-run", "r-basepath-sse")
        .header("content-type", "application/json")
        .header("anthropic-version", "2023-06-01")
        .body(read("anthropic_stream/request.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let downstream = resp.bytes().await.unwrap();
    // Byte-for-byte passthrough downstream (the tee must not corrupt the stream).
    assert_eq!(
        downstream.as_ref(),
        sse.as_slice(),
        "SSE bytes pass through unchanged"
    );
    assert_eq!(
        fake.received().last().unwrap().path,
        "/llm-proxy/v1/messages"
    );

    // The tee still parsed usage into a step (poll — recorded when the stream completes).
    let deadline = std::time::Duration::from_secs(10);
    let start = std::time::Instant::now();
    while captured.lock().unwrap().is_empty() {
        if start.elapsed() > deadline {
            panic!("no step captured from the chained SSE stream");
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let steps = captured.lock().unwrap().clone();
    assert_eq!(steps.len(), 1);
    assert_eq!(
        steps[0].usage.output, 64,
        "usage still tee'd through the base-path upstream"
    );
}

/// (fail-open): a request exceeding the 16 MiB attribution buffer must still reach the
/// upstream whether its size is declared or discovered while reading a chunked stream. It is
/// forwarded unbuffered and left unattributed, never rejected because capture ran out of room.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_declared_and_chunked_requests_are_forwarded_unattributed() {
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");
    let fake = FakeHandle::new();
    let fake_base = spawn(fake.router()).await;

    let captured: Arc<Mutex<Vec<StepRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_store = captured.clone();
    let sink: Sink = Arc::new(move |s| sink_store.lock().unwrap().push(s));
    let proxy_base = spawn(router(
        ProxyConfig {
            upstreams: both(&fake_base),
            ..ProxyConfig::default()
        },
        sink,
    ))
    .await;

    // One byte over the 16 MiB attribution cap: reqwest sets Content-Length to the body's exact
    // length, so the proxy sees declared_len > BODY_CAP and takes the fail-open (unbuffered) branch.
    let huge = vec![b'x'; 16 * 1024 * 1024 + 1];
    fake.enqueue_json(read("anthropic_nonstream/response.json"));
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{proxy_base}/v1/messages"))
        .header("x-tare-run", "huge")
        .header("content-type", "application/json")
        .body(huge)
        .send()
        .await
        .unwrap();
    // Fail-open: the oversized request is NOT rejected — the upstream's 200 reaches the client.
    assert_eq!(
        resp.status().as_u16(),
        200,
        "an oversized request must fail open (forwarded), never be blocked by a capture limit"
    );
    let _ = resp.bytes().await.unwrap();

    // It reached the upstream (forwarded, not dropped)...
    assert_eq!(
        fake.received().len(),
        1,
        "the oversized request must be forwarded upstream"
    );

    // Repeat without Content-Length. The proxy only discovers the overflow after consuming the
    // first chunks, so it must reconstruct the stream from that prefix plus the untouched tail.
    fake.enqueue_json(read("anthropic_nonstream/response.json"));
    let chunked_body = reqwest::Body::wrap_stream(futures_util::stream::iter(vec![
        Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from(vec![b'a'; 8 * 1024 * 1024])),
        Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from(vec![b'b'; 8 * 1024 * 1024 + 1])),
    ]));
    let chunked = client
        .post(format!("{proxy_base}/v1/messages"))
        .header("x-tare-run", "huge-chunked")
        .header("content-type", "application/json")
        .body(chunked_body)
        .send()
        .await
        .unwrap();
    assert_eq!(chunked.status().as_u16(), 200);
    let _ = chunked.bytes().await.unwrap();
    let received = fake.received();
    assert_eq!(received.len(), 2);
    assert_eq!(received[0].body.len(), 16 * 1024 * 1024 + 1);
    assert_eq!(received[1].body.len(), 16 * 1024 * 1024 + 1);
    assert_eq!(received[1].body.first(), Some(&b'a'));
    assert_eq!(received[1].body.last(), Some(&b'b'));

    // ...but is left UNATTRIBUTED: the unbuffered branch skips the tee, so no step is ever recorded
    // (the request body was never buffered for parsing). A short settle proves nothing lands async.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(
        captured.lock().unwrap().is_empty(),
        "an unbuffered oversized request is forwarded but deliberately not attributed"
    );
}

/// the proxy captures REDACTED transcripts ONLY under a `max_inspect` privacy policy, and
/// scrubs secrets before the sink (→ the separate store) sees them. Any other profile captures none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_inspect_captures_scrubbed_transcripts_others_dont() {
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");

    // (run_id, step_ordinal, redacted_req, redacted_resp, either_body_truncated).
    type Captured = Vec<(String, u32, String, String, bool)>;
    async fn run(privacy: tare_core::PrivacyPolicy, expect: usize) -> Captured {
        let fake = FakeHandle::new();
        let fake_base = spawn(fake.router()).await;
        let captured: Arc<Mutex<Captured>> = Arc::new(Mutex::new(Vec::new()));
        let store = captured.clone();
        let tsink: TranscriptSink = Arc::new(move |run, ord, req, resp, truncated| {
            store.lock().unwrap().push((run, ord, req, resp, truncated))
        });
        let sink: Sink = Arc::new(|_| {});
        let config = ProxyConfig {
            upstreams: both(&fake_base),
            privacy,
            ..ProxyConfig::default()
        };
        let proxy = spawn(router_with_transcript(config, sink, Some(tsink))).await;

        fake.enqueue_json(read("anthropic_nonstream/response.json"));
        // Valid request larger than the transcript cap but below the proxy's forwarding cap. Ordinary
        // short words avoid the high-entropy scrub rule collapsing the padding before truncation.
        let body = format!(
            "{{\"model\":\"claude-3-5-sonnet\",\"max_tokens\":16,\"messages\":[{{\"role\":\"user\",\"content\":\"key sk-ant-api03-ABCDEFGHIJKLMNOP1234567890 ok {}\"}}]}}",
            "word ".repeat(60_000)
        );
        let resp = reqwest::Client::new()
            .post(format!("{proxy}/v1/messages"))
            .header("x-tare-run", "r-tx")
            .header("content-type", "application/json")
            .header("anthropic-version", "2023-06-01")
            .body(body.into_bytes())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let _ = resp.bytes().await.unwrap();
        // Poll for the expected number of captures (the tee emits as the forwarded stream completes).
        let start = std::time::Instant::now();
        loop {
            let n = captured.lock().unwrap().len();
            if n >= expect || start.elapsed() > std::time::Duration::from_secs(5) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // A short settle so a wrongful capture on the no-capture path would show up.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let out = captured.lock().unwrap().clone();
        out
    }

    // Default strict_counts profile: NO transcript captured (payload-free, unchanged).
    assert!(run(tare_core::PrivacyPolicy::strict_counts(), 0)
        .await
        .is_empty());

    // max_inspect: exactly one transcript, with the secret scrubbed to a placeholder.
    let tx = run(tare_core::PrivacyPolicy::max_inspect(), 1).await;
    assert_eq!(tx.len(), 1);
    let (run_id, ord, req, _resp, truncated) = &tx[0];
    assert_eq!(run_id, "r-tx");
    assert_eq!(*ord, 1);
    assert!(
        !req.contains("sk-ant-api03-ABCDEFGHIJKLMNOP1234567890"),
        "the request secret must be scrubbed, not stored raw"
    );
    assert!(req.contains("[REDACTED:key]"), "masked placeholder present");
    assert!(
        *truncated,
        "the cap result must survive the transcript sink"
    );
    assert!(req.len() <= 256 * 1024, "stored request is capped");
}
