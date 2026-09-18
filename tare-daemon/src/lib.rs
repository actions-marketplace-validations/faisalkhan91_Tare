//! Alert sinks for the always-on daemon. The alert DECISION is pure (`tare_core::alert`);
//! these are the thin side-effecting outputs. The local sinks (stderr / JSON-line / exit-code)
//! are always compiled and side-effect-contained; the off-box `WebhookSink` lives behind the
//! `webhooks` feature (egress, OUT of the offline gate) and routes through a loopback guard.

use std::io::Write;
use tare_core::alert::{Alert, AlertLevel};

/// A destination for fired alerts. `emit` is best-effort; a sink error never blocks others.
pub trait AlertSink {
    fn emit(&mut self, alert: &Alert) -> Result<(), String>;
}

/// One canonical JSON line for an alert (also the JSON-line sink's output / golden form).
pub fn alert_json_line(alert: &Alert) -> String {
    format!(
        "{}\n",
        serde_json::to_string(alert).unwrap_or_else(|_| "{}".into())
    )
}

/// Human-readable stderr sink.
pub struct StderrSink;
impl AlertSink for StderrSink {
    fn emit(&mut self, alert: &Alert) -> Result<(), String> {
        eprintln!(
            "tare alert [{}] {}: {}",
            alert.level.as_str(),
            alert.subject.label(),
            alert.message
        );
        Ok(())
    }
}

/// Newline-delimited JSON sink over any writer (file, pipe, in-memory buffer for tests).
pub struct JsonLineSink<W: Write> {
    pub writer: W,
}
impl<W: Write> JsonLineSink<W> {
    pub fn new(writer: W) -> Self {
        JsonLineSink { writer }
    }
}
impl<W: Write> AlertSink for JsonLineSink<W> {
    fn emit(&mut self, alert: &Alert) -> Result<(), String> {
        self.writer
            .write_all(alert_json_line(alert).as_bytes())
            .map_err(|e| format!("jsonline sink: {e}"))
    }
}

/// Tracks the worst alert seen so a CI/daemon wrapper can exit nonzero on Kill.
#[derive(Default)]
pub struct ExitCodeSink {
    worst: Option<AlertLevel>,
}
impl ExitCodeSink {
    /// 0 = nothing/warn-only is non-fatal? No: Warn is advisory (0), Kill is fatal (1).
    pub fn exit_code(&self) -> i32 {
        match self.worst {
            Some(AlertLevel::Kill) => 1,
            _ => 0,
        }
    }
}
impl AlertSink for ExitCodeSink {
    fn emit(&mut self, alert: &Alert) -> Result<(), String> {
        let rank = |l: AlertLevel| match l {
            AlertLevel::Warn => 1,
            AlertLevel::Kill => 2,
        };
        // Explicit match (not `Option::is_none_or`, which is Rust 1.82) so the workspace
        // honors its declared MSRV (rust-version = 1.80).
        let more_severe = match self.worst {
            None => true,
            Some(w) => rank(alert.level) > rank(w),
        };
        if more_severe {
            self.worst = Some(alert.level);
        }
        Ok(())
    }
}

/// Off-box webhook sink (egress). Behind the `webhooks` feature — never built in the offline
/// gate. Posts the alert as JSON (counts/causes only, no payload) and refuses any non-loopback
/// endpoint when `TARE_NETWORK_GUARD=loopback`.
#[cfg(feature = "webhooks")]
pub struct WebhookSink {
    pub endpoint: String,
}

#[cfg(feature = "webhooks")]
impl WebhookSink {
    fn endpoint_url(&self) -> Result<reqwest::Url, String> {
        let url = reqwest::Url::parse(&self.endpoint)
            .map_err(|error| format!("invalid webhook endpoint {:?}: {error}", self.endpoint))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || url.cannot_be_a_base()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(format!(
                "invalid webhook endpoint {:?}: expected an http(s) URL without credentials or a fragment",
                self.endpoint
            ));
        }
        Ok(url)
    }

    fn guard_allows(url: &reqwest::Url) -> Result<bool, String> {
        let armed = match std::env::var("TARE_NETWORK_GUARD") {
            Ok(value) => value == "loopback",
            Err(std::env::VarError::NotPresent) => false,
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err("TARE_NETWORK_GUARD is not valid Unicode".into())
            }
        };
        if !armed {
            return Ok(true);
        }
        let host = url.host_str().unwrap_or_default();
        let address_literal = host
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .unwrap_or(host);
        let loopback = host.eq_ignore_ascii_case("localhost")
            || address_literal
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback());
        Ok(loopback)
    }
}

#[cfg(feature = "webhooks")]
impl AlertSink for WebhookSink {
    fn emit(&mut self, alert: &Alert) -> Result<(), String> {
        let endpoint = self.endpoint_url()?;
        if !Self::guard_allows(&endpoint)? {
            return Err(format!(
                "network guard: refusing non-loopback webhook {}",
                self.endpoint
            ));
        }
        // Counts/causes only — `Alert` carries no payload by construction.
        let body = serde_json::to_string(alert).map_err(|e| e.to_string())?;
        post_webhook(endpoint, body)
    }
}

#[cfg(feature = "webhooks")]
fn post_webhook(endpoint: reqwest::Url, body: String) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(10))
        // A loopback endpoint must not redirect around the network guard.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("build webhook client: {error}"))?;
    let response = client
        .post(endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::USER_AGENT, "tare-daemon")
        .body(body)
        .send()
        .map_err(|error| format!("send webhook: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("webhook returned HTTP {}", response.status()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tare_core::alert::Alert;

    use tare_core::alert::AlertSubject;

    fn warn() -> Alert {
        Alert {
            level: AlertLevel::Warn,
            subject: AlertSubject::run("r"),
            message: "approaching spend limit".into(),
        }
    }
    fn kill() -> Alert {
        Alert {
            level: AlertLevel::Kill,
            subject: AlertSubject::run("r"),
            message: "spend limit reached".into(),
        }
    }
    fn anomaly() -> Alert {
        Alert {
            level: AlertLevel::Warn,
            subject: AlertSubject::Anomaly {
                date: "2026-06-24".into(),
                series_key: "anthropic/claude-opus-4-8".into(),
                kind: "Spike".into(),
            },
            message: "daily spend 3x trailing median".into(),
        }
    }

    #[cfg(feature = "webhooks")]
    #[test]
    fn webhook_sink_guard_blocks_non_loopback_when_armed() {
        // The only network-egress sink must refuse off-box endpoints under the loopback guard,
        // and allow loopback without relying on brittle string slicing.
        std::env::set_var("TARE_NETWORK_GUARD", "loopback");
        let mut evil = WebhookSink {
            endpoint: "https://evil.example.com/hook".into(),
        };
        assert!(evil.emit(&kill()).is_err());
        let mut local = WebhookSink {
            endpoint: "http://127.0.0.1:9999/hook".into(),
        };
        assert!(WebhookSink::guard_allows(&local.endpoint_url().unwrap()).unwrap());
        local.endpoint = "http://[::1]/hook".into();
        assert!(WebhookSink::guard_allows(&local.endpoint_url().unwrap()).unwrap());
        // Link-local metadata endpoint is not loopback -> blocked.
        let mut meta = WebhookSink {
            endpoint: "http://169.254.169.254/latest".into(),
        };
        assert!(meta.emit(&warn()).is_err());
        std::env::remove_var("TARE_NETWORK_GUARD");
    }

    #[cfg(feature = "webhooks")]
    fn one_shot_http_server(status: &str) -> (String, std::thread::JoinHandle<Vec<u8>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_string();
        let thread = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let expected_len = loop {
                let mut chunk = [0u8; 2048];
                let count = stream.read(&mut chunk).unwrap();
                assert!(count > 0, "client closed before completing request headers");
                request.extend_from_slice(&chunk[..count]);
                if let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_len = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    break header_end + 4 + content_len;
                }
                assert!(
                    request.len() <= 64 * 1024,
                    "unexpectedly large webhook request"
                );
            };
            while request.len() < expected_len {
                let mut chunk = [0u8; 2048];
                let count = stream.read(&mut chunk).unwrap();
                assert!(count > 0, "client closed before completing request body");
                request.extend_from_slice(&chunk[..count]);
            }
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            request
        });
        (format!("http://{address}/hook?token=local"), thread)
    }

    #[cfg(feature = "webhooks")]
    #[test]
    fn webhook_sink_posts_json_and_propagates_http_failures() {
        let (endpoint, server) = one_shot_http_server("204 No Content");
        let mut sink = WebhookSink { endpoint };
        sink.emit(&warn()).unwrap();
        let request = String::from_utf8(server.join().unwrap()).unwrap();
        assert!(request.starts_with("POST /hook?token=local HTTP/1.1\r\n"));
        assert!(request
            .to_ascii_lowercase()
            .contains("content-type: application/json"));
        assert!(request.contains("\"message\":\"approaching spend limit\""));

        let (endpoint, server) = one_shot_http_server("500 Internal Server Error");
        let mut sink = WebhookSink { endpoint };
        let error = sink.emit(&warn()).unwrap_err();
        server.join().unwrap();
        assert!(error.contains("HTTP 500"), "{error}");
    }

    #[test]
    fn json_line_sink_golden() {
        let mut buf = Vec::new();
        {
            let mut sink = JsonLineSink::new(&mut buf);
            sink.emit(&warn()).unwrap();
            sink.emit(&kill()).unwrap();
            sink.emit(&anomaly()).unwrap();
        }
        let out = String::from_utf8(buf).unwrap();
        assert_eq!(
            out,
            "{\"level\":\"warn\",\"subject\":\"run\",\"run_id\":\"r\",\"message\":\"approaching spend limit\"}\n\
             {\"level\":\"kill\",\"subject\":\"run\",\"run_id\":\"r\",\"message\":\"spend limit reached\"}\n\
             {\"level\":\"warn\",\"subject\":\"anomaly\",\"date\":\"2026-06-24\",\"series_key\":\"anthropic/claude-opus-4-8\",\"kind\":\"Spike\",\"message\":\"daily spend 3x trailing median\"}\n"
        );
    }

    #[test]
    fn budget_breach_drives_alerts_into_sink() {
        // End-to-end (pure): a run accrues spend past the cap; the budget decisions feed
        // alert::evaluate (fire-once) into an ExitCodeSink, which ends up fatal.
        use tare_core::alert::evaluate;
        use tare_core::budget::{Budget, Decision, RunTally};
        let budget = Budget {
            max_micros: Some(1000),
            ..Default::default()
        };
        let mut tally = RunTally::default();
        let mut prev = Decision::Allow;
        let mut sink = ExitCodeSink::default();
        let mut fired = Vec::new();
        // Three steps: 0 -> 800 (warn) -> 1100 (kill).
        for cost in [0i64, 800, 300] {
            tally.add_cost(cost);
            let curr = tare_core::budget::evaluate(&budget, &tally, 1);
            if let Some(a) = evaluate(&prev, &curr, "run-x") {
                sink.emit(&a).unwrap();
                fired.push(a.level);
            }
            prev = curr;
        }
        assert_eq!(fired, vec![AlertLevel::Warn, AlertLevel::Kill]);
        assert_eq!(sink.exit_code(), 1, "breach is fatal");
    }

    #[test]
    fn exit_code_sink_kill_is_nonzero() {
        let mut sink = ExitCodeSink::default();
        assert_eq!(sink.exit_code(), 0);
        sink.emit(&warn()).unwrap();
        assert_eq!(sink.exit_code(), 0, "warn is advisory");
        sink.emit(&kill()).unwrap();
        assert_eq!(sink.exit_code(), 1, "kill is fatal");
    }
}
