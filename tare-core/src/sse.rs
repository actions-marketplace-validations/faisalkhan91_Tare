//! SSE parsing. A full-buffer parser for committed fixtures, plus an incremental
//! accumulator the proxy tee feeds chunk-by-chunk (events may split mid-chunk).

use serde_json::Value;

/// The data payload string of one SSE event (multiple `data:` lines joined by `\n`),
/// plus the optional `event:` name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

impl SseEvent {
    pub fn json(&self) -> Option<Value> {
        if self.data.trim() == "[DONE]" {
            return None;
        }
        serde_json::from_str(&self.data).ok()
    }
}

/// Parse a complete SSE byte buffer into events.
pub fn parse_events(bytes: &[u8]) -> Vec<SseEvent> {
    let mut acc = SseAccumulator::new();
    let mut out = acc.push_bytes(bytes);
    out.extend(acc.flush());
    out
}

/// Incremental SSE accumulator. Feed arbitrary byte chunks; get back any
/// fully-framed events. Carries a partial trailing chunk across calls.
#[derive(Default)]
pub struct SseAccumulator {
    /// Raw bytes buffered until a full event is framed. UTF-8 is decoded only at event
    /// boundaries, so a multibyte character split across chunks is never corrupted.
    buf: Vec<u8>,
}

/// Locate two consecutive SSE line endings. The event-stream grammar accepts LF, CRLF, and bare
/// CR; callers need both the end of the event payload and the end of the blank-line separator.
fn find_sep(buf: &[u8]) -> Option<(usize, usize)> {
    let line_ending_len = |at: usize| match buf.get(at) {
        Some(b'\n') => Some(1),
        Some(b'\r') if buf.get(at + 1) == Some(&b'\n') => Some(2),
        Some(b'\r') => Some(1),
        _ => None,
    };

    for first in 0..buf.len() {
        let Some(first_len) = line_ending_len(first) else {
            continue;
        };
        let second = first + first_len;
        let Some(second_len) = line_ending_len(second) else {
            continue;
        };
        return Some((first, second + second_len));
    }
    None
}

impl SseAccumulator {
    pub fn new() -> Self {
        SseAccumulator { buf: Vec::new() }
    }

    /// Feed a chunk of raw bytes; returns any events completed by this chunk. Bytes are
    /// buffered and only decoded as UTF-8 once a full event is framed, so a multibyte
    /// character split across two chunks is never corrupted.
    pub fn push_bytes(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut events = Vec::new();
        // Events are separated by a blank line. Keep the trailing partial, including a CRLF split
        // across chunks, until the second line ending arrives.
        while let Some((event_end, separator_end)) = find_sep(&self.buf) {
            let raw: Vec<u8> = self.buf.drain(..separator_end).collect();
            let text = String::from_utf8_lossy(&raw[..event_end]);
            if let Some(ev) = parse_one(&text) {
                events.push(ev);
            }
        }
        events
    }

    pub fn push_str(&mut self, chunk: &str) -> Vec<SseEvent> {
        self.push_bytes(chunk.as_bytes())
    }

    /// Emit any final event held without a trailing blank line.
    pub fn flush(&mut self) -> Vec<SseEvent> {
        let raw = std::mem::take(&mut self.buf);
        let text = String::from_utf8_lossy(&raw);
        if !text.trim().is_empty() {
            if let Some(ev) = parse_one(&text) {
                return vec![ev];
            }
        }
        Vec::new()
    }
}

fn parse_one(raw: &str) -> Option<SseEvent> {
    let mut event = None;
    let mut data_lines: Vec<&str> = Vec::new();
    // `str::lines` handles LF and CRLF but not a bare CR, which is also a valid SSE line ending.
    for line in raw.split(['\r', '\n']) {
        // Do NOT trim the line: per the SSE spec only the field-name colon and a single
        // leading space are removed; trailing whitespace inside `data:` is significant.
        // The split above removes the line terminator without touching field contents.
        if let Some(rest) = line.strip_prefix("event:") {
            event = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
        // Comment lines (":") and other fields are ignored.
    }
    if data_lines.is_empty() && event.is_none() {
        return None;
    }
    Some(SseEvent {
        event,
        data: data_lines.join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_events_across_chunks() {
        let mut acc = SseAccumulator::new();
        // Split mid-event to exercise the carry-across-reads path.
        let mut got = acc.push_str("event: message_start\ndata: {\"type\":\"message_st");
        assert!(got.is_empty());
        got = acc.push_str("art\"}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].event.as_deref(), Some("message_start"));
        assert_eq!(got[0].json().unwrap()["type"], "message_start");
        assert_eq!(got[1].json().unwrap()["type"], "message_stop");
    }

    #[test]
    fn done_sentinel_has_no_json() {
        let evs = parse_events(b"data: [DONE]\n\n");
        assert_eq!(evs.len(), 1);
        assert!(evs[0].json().is_none());
    }

    #[test]
    fn multibyte_char_split_across_chunks_is_not_corrupted() {
        let mut acc = SseAccumulator::new();
        // "—" (em dash) is 3 UTF-8 bytes (E2 80 94); split the chunk INSIDE it.
        let full = "data: {\"t\":\"a—b\"}\n\n";
        let bytes = full.as_bytes();
        let mid = full.find('—').unwrap() + 1; // split mid-character
        let got = acc.push_bytes(&bytes[..mid]);
        assert!(got.is_empty());
        let got = acc.push_bytes(&bytes[mid..]);
        assert_eq!(got.len(), 1);
        // The em dash survives intact (no U+FFFD replacement chars).
        assert_eq!(got[0].json().unwrap()["t"], "a—b");
    }

    #[test]
    fn trailing_whitespace_in_data_is_preserved() {
        // Only one leading space after `data:` is stripped; trailing space is significant.
        let evs = parse_events(b"data:  x  \n\n");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data, " x  ");
    }

    #[test]
    fn accepts_crlf_and_emits_before_stream_end() {
        let mut acc = SseAccumulator::new();
        let got = acc.push_bytes(
            b"event: message\r\ndata: {\"n\":1}\r\n\r\nevent: pending\r\ndata: {\"n\":2}",
        );
        assert_eq!(got.len(), 1, "a CRLF-framed event must not wait for flush");
        assert_eq!(got[0].event.as_deref(), Some("message"));
        assert_eq!(got[0].json().unwrap()["n"], 1);
    }

    #[test]
    fn crlf_separator_may_split_at_any_chunk_boundary() {
        let stream = b"data: {\"n\":1}\r\n\r\ndata: {\"n\":2}\r\n\r\n";
        for split in 0..=stream.len() {
            let mut acc = SseAccumulator::new();
            let mut got = acc.push_bytes(&stream[..split]);
            got.extend(acc.push_bytes(&stream[split..]));
            got.extend(acc.flush());
            assert_eq!(got.len(), 2, "split at byte {split}");
            assert_eq!(got[0].json().unwrap()["n"], 1, "split at byte {split}");
            assert_eq!(got[1].json().unwrap()["n"], 2, "split at byte {split}");
        }
    }

    #[test]
    fn accepts_bare_cr_line_endings() {
        let got = parse_events(b"event: message\rdata: {\"ok\":true}\r\r");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].event.as_deref(), Some("message"));
        assert_eq!(got[0].json().unwrap()["ok"], true);
    }
}
