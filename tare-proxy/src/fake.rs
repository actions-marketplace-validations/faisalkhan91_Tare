//! In-process fake provider for tests. Replays committed fixtures byte-for-byte.
//! SSE bodies are emitted in small fixed-size byte windows (splitting events
//! mid-chunk) to exercise the proxy's tee carry-across-reads path. No network.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use bytes::Bytes;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct FakeResponse {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
    pub sse: bool,
    pub fail_after_body: bool,
}

#[derive(Clone, Debug)]
pub struct ReceivedRequest {
    pub path: String,
    /// Lower-cased header names + values as forwarded by the proxy.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Shared handle: enqueue responses, inspect what the proxy forwarded.
#[derive(Clone, Default)]
pub struct FakeHandle {
    queue: Arc<Mutex<VecDeque<FakeResponse>>>,
    received: Arc<Mutex<Vec<ReceivedRequest>>>,
}

impl FakeHandle {
    pub fn new() -> Self {
        FakeHandle::default()
    }

    pub fn enqueue_json(&self, body: Vec<u8>) {
        self.queue.lock().unwrap().push_back(FakeResponse {
            status: 200,
            content_type: "application/json".to_string(),
            body,
            sse: false,
            fail_after_body: false,
        });
    }

    pub fn enqueue_sse(&self, body: Vec<u8>) {
        self.queue.lock().unwrap().push_back(FakeResponse {
            status: 200,
            content_type: "text/event-stream".to_string(),
            body,
            sse: true,
            fail_after_body: false,
        });
    }

    /// Queue a response that emits `body` and then fails its stream. This exercises incomplete
    /// upstream-body handling without relying on a real broken network connection.
    pub fn enqueue_stream_error(&self, content_type: &str, body: Vec<u8>) {
        self.queue.lock().unwrap().push_back(FakeResponse {
            status: 200,
            content_type: content_type.to_string(),
            body,
            sse: true,
            fail_after_body: true,
        });
    }

    pub fn received(&self) -> Vec<ReceivedRequest> {
        self.received.lock().unwrap().clone()
    }

    pub fn router(&self) -> Router {
        Router::new()
            .fallback(any(handler))
            .with_state(self.clone())
    }
}

async fn handler(State(state): State<FakeHandle>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_ascii_lowercase(),
                String::from_utf8_lossy(v.as_bytes()).to_string(),
            )
        })
        .collect();
    let body = req.into_body();
    let bytes = axum::body::to_bytes(body, 32 * 1024 * 1024)
        .await
        .unwrap_or_default();
    state.received.lock().unwrap().push(ReceivedRequest {
        path,
        headers,
        body: bytes.to_vec(),
    });

    let resp = state.queue.lock().unwrap().pop_front();
    let Some(resp) = resp else {
        return Response::builder()
            .status(500)
            .body(Body::from("fake: no queued response"))
            .unwrap();
    };

    if resp.sse {
        // Emit in 5-byte windows to split SSE events mid-chunk.
        let data = resp.body.clone();
        let fail_after_body = resp.fail_after_body;
        let stream = async_stream::stream! {
            let mut i = 0usize;
            while i < data.len() {
                let end = (i + 5).min(data.len());
                let chunk = Bytes::copy_from_slice(&data[i..end]);
                i = end;
                yield Ok::<Bytes, std::io::Error>(chunk);
                tokio::task::yield_now().await;
            }
            if fail_after_body {
                yield Err(std::io::Error::other("fake: injected stream failure"));
            }
        };
        Response::builder()
            .status(resp.status)
            .header("content-type", resp.content_type)
            .body(Body::from_stream(stream))
            .unwrap()
    } else {
        Response::builder()
            .status(resp.status)
            .header("content-type", resp.content_type)
            .body(Body::from(resp.body))
            .unwrap()
    }
}
