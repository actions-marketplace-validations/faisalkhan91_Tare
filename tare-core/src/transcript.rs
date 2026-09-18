//! Claude Code JSONL transcript line parser for the backfill and reconciliation lane.
//! Claude Code appends one JSON object per line to `~/.claude/projects/<dashed-cwd>/<sessionId>.jsonl`;
//! `assistant` lines carry `message.usage` (the per-request token counts) plus identity fields. This
//! captures sessions that ran before Tare was installed / while it was down — retroactive
//! completeness the live OTel + hook lanes can't provide.
//!
//! COUNTS-ONLY (the invariant, defended in code): we read ONLY `message.usage` + identity
//! (`message.id`, `message.model`, `sessionId`, `uuid`/`parentUuid`, `requestId`, `timestamp`) and
//! NEVER the `content`/text of a message. Lines we can't or shouldn't account are skipped:
//! non-`assistant` lines, `model:"<synthetic>"`, `isApiErrorMessage`, and any line with zero usage.
//!
//! CLOCK-FREE: the `timestamp` is the transcript's own field, never a wall clock. The schema is an
//! UNDOCUMENTED Claude Code implementation detail (local observation), so this is exercised by
//! golden fixtures and kept tolerant of missing fields. On Bedrock installs `requestId` is absent on
//! real messages, so downstream dedup must fall back to `message.id` alone.

use crate::model::UsageTokens;
use serde::{Deserialize, Serialize};

/// One accountable assistant turn parsed from a transcript line (payload-free).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptRecord {
    /// `message.model` — the pricing key.
    pub model: String,
    pub usage: UsageTokens,
    /// `sessionId` (top-level) — groups turns into a session/run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// `message.id` — the primary cross-source dedup key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// `requestId` — secondary dedup key; ABSENT on Bedrock installs (dedup then falls back to id).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// `uuid`/`parentUuid` — the transcript's own turn-chain (fallback dedup + ordering).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,
    /// ISO-8601 `timestamp` from the line (clock-free: the transcript's own time, not a wall clock).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// Read a u64 token field (accepts a JSON number or a decimal string, tolerating producer quirks).
fn u64_field(v: &serde_json::Value, key: &str) -> u64 {
    match v.get(key) {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

/// Map an Anthropic `message.usage` object to Tare's [`UsageTokens`] — counts only. `cache_creation`
/// may arrive split (`{ephemeral_5m_input_tokens, ephemeral_1h_input_tokens}`) or as a single
/// `cache_creation_input_tokens` total; both are handled (a bare total is attributed to the 5m tier,
/// the common case, since the transcript doesn't split it).
fn usage_from(u: &serde_json::Value) -> UsageTokens {
    let (w5, w1) = match u.get("cache_creation") {
        Some(cc) if cc.is_object() => (
            u64_field(cc, "ephemeral_5m_input_tokens"),
            u64_field(cc, "ephemeral_1h_input_tokens"),
        ),
        _ => (u64_field(u, "cache_creation_input_tokens"), 0),
    };
    // Reasoning/thinking is a SUBSET of output (billed as output, tracked as a cause). Anthropic
    // reports it under `output_tokens_details.thinking_tokens` — mirror the wire path so the JSONL
    // and proxy/OTel lanes agree. Without this, the reasoning-share-of-output view always reads 0%.
    let reasoning = u
        .get("output_tokens_details")
        .map(|d| u64_field(d, "thinking_tokens"))
        .unwrap_or(0);
    UsageTokens {
        fresh_input: u64_field(u, "input_tokens"),
        output: u64_field(u, "output_tokens"),
        cache_read: u64_field(u, "cache_read_input_tokens"),
        cache_write_5m: w5,
        cache_write_1h: w1,
        reasoning,
        ..Default::default()
    }
}

fn opt_str(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(256).collect())
}

/// Parse one transcript line. Returns `None` for any line that should not be accounted: unparseable,
/// non-`assistant`, `<synthetic>`, an API-error message, or one whose usage is entirely zero.
pub fn parse_transcript_line(line: &str) -> Option<TranscriptRecord> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    // Error/synthetic markers are top-level in the current schema.
    if v.get("isApiErrorMessage").and_then(|x| x.as_bool()) == Some(true) {
        return None;
    }
    // Usage + model live under `message` on assistant lines; user/summary lines have no usage.
    let msg = v.get("message")?;
    let model = msg.get("model").and_then(|x| x.as_str()).unwrap_or("");
    if model.is_empty() || model == "<synthetic>" {
        return None;
    }
    let usage = usage_from(msg.get("usage")?);
    // Skip a zero-usage line (synthetic continuation / placeholder) — nothing to account.
    if usage.fresh_input == 0
        && usage.output == 0
        && usage.cache_read == 0
        && usage.cache_write_5m == 0
        && usage.cache_write_1h == 0
    {
        return None;
    }
    Some(TranscriptRecord {
        model: model.chars().take(64).collect(),
        usage,
        session_id: opt_str(&v, "sessionId"),
        message_id: opt_str(msg, "id"),
        request_id: opt_str(&v, "requestId"),
        uuid: opt_str(&v, "uuid"),
        parent_uuid: opt_str(&v, "parentUuid"),
        timestamp: opt_str(&v, "timestamp"),
    })
}

/// Parse a whole transcript (many lines), yielding every accountable assistant turn in file order.
pub fn parse_transcript(bytes: &[u8]) -> Vec<TranscriptRecord> {
    let text = String::from_utf8_lossy(bytes);
    text.lines().filter_map(parse_transcript_line).collect()
}

/// What a transcript file's PATH tells us about its role in the session tree. Claude
/// Code lays out (relative to a `projects/<dashed-cwd>/` dir): the main session as `<sessionId>.jsonl`;
/// plain subagents as `<sessionId>/subagents/agent-<id>.jsonl`; and workflow subagents as
/// `<sessionId>/subagents/workflows/wf_<id>/agent-<x>.jsonl` (each with an `agent-<x>.meta.json`
/// sidecar). Subagents carry the PARENT sessionId, so rolling their spend up is natural — but ONLY
/// if we recurse those subtrees; tailing the top-level `.jsonl` alone massively undercounts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranscriptKind {
    /// `<sessionId>.jsonl` — the main session transcript.
    Session { session_id: String },
    /// `<sessionId>/subagents/agent-<id>.jsonl` — a plain subagent under a parent session.
    Subagent {
        parent_session: String,
        agent_id: String,
    },
    /// `<sessionId>/subagents/workflows/wf_<id>/agent-<x>.jsonl` — a workflow's subagent.
    WorkflowSubagent {
        parent_session: String,
        workflow_id: String,
        agent_id: String,
    },
}

/// Strip the `agent-` prefix and `.jsonl` suffix from a subagent filename, yielding its agent id.
fn agent_id_from(filename: &str) -> String {
    filename
        .strip_suffix(".jsonl")
        .unwrap_or(filename)
        .strip_prefix("agent-")
        .unwrap_or(filename)
        .to_string()
}

/// Classify a `.jsonl` transcript path (any prefix is fine — we key off the `subagents`/`workflows`
/// markers and the component before them). Returns `None` for a non-`.jsonl` path or one whose shape
/// we don't recognize (e.g. a `workflows/wf_<id>.json` totals file — handled separately).
pub fn classify_transcript_path(path: &str) -> Option<TranscriptKind> {
    let comps: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    let file = *comps.last()?;
    if !file.ends_with(".jsonl") {
        return None;
    }
    match comps.iter().position(|&c| c == "subagents") {
        Some(i) if i > 0 => {
            let parent_session = comps[i - 1].to_string();
            if comps.get(i + 1) == Some(&"workflows") {
                // .../subagents/workflows/wf_<id>/agent-<x>.jsonl
                let workflow_id = comps.get(i + 2)?.to_string();
                Some(TranscriptKind::WorkflowSubagent {
                    parent_session,
                    workflow_id,
                    agent_id: agent_id_from(file),
                })
            } else {
                Some(TranscriptKind::Subagent {
                    parent_session,
                    agent_id: agent_id_from(file),
                })
            }
        }
        _ => Some(TranscriptKind::Session {
            session_id: file.strip_suffix(".jsonl").unwrap_or(file).to_string(),
        }),
    }
}

/// Payload-free identity from a subagent's `agent-<id>.meta.json` sidecar: the
/// `toolUseId` that attaches this subagent to its parent's `tool_use` block, the `agentType`
/// dimension, and `spawnDepth` for the tree. Never reads prompt/content.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spawn_depth: Option<u32>,
}

/// Parse a subagent `.meta.json` — identity fields only. Tolerant of missing keys / bad JSON.
pub fn parse_subagent_meta(bytes: &[u8]) -> SubagentMeta {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return SubagentMeta::default();
    };
    SubagentMeta {
        tool_use_id: opt_str(&v, "toolUseId"),
        agent_type: opt_str(&v, "agentType"),
        spawn_depth: v
            .get("spawnDepth")
            .and_then(|x| x.as_u64())
            .map(|d| d as u32),
    }
}

/// Cross-source dedup identity of an accountable request. The SAME request can be
/// seen on two lanes — an OTLP `api_request` event AND its JSONL `message.usage` line — and must be
/// counted once. Precedence mirrors the verified schema: `message.id + request_id` (both lanes carry
/// these); FALLBACK `message.id + uuid` when `request_id` is absent (Bedrock installs omit it);
/// last-resort `session + timestamp` when there's no `message.id`. A stable, allocation-cheap key.
pub fn dedup_key(
    message_id: Option<&str>,
    request_id: Option<&str>,
    uuid: Option<&str>,
    session_id: Option<&str>,
    timestamp: Option<&str>,
) -> String {
    // `message.id` ALONE is the dedup key when present (the invariant noted above): it is
    // globally unique per API response, so it collapses BOTH the OTLP lane (same id) AND replayed
    // JSONL copies of a turn. Claude Code rewrites each assistant turn several times per session —
    // IDENTICAL usage but a DIFFERENT per-line `uuid` each time — and Bedrock installs omit
    // `request_id`; the old `id + (request_id|uuid)` key gave every replay a distinct key and
    // over-counted backfill ~2.2x when checked against independent message-id ground truth.
    // `request_id`/`uuid` stay in the signature for callers + the no-id fallback.
    let _ = (request_id, uuid);
    match message_id {
        Some(m) if !m.is_empty() => format!("id:{m}"),
        _ => format!(
            "st:{}|{}",
            session_id.unwrap_or(""),
            timestamp.unwrap_or("")
        ),
    }
}

impl TranscriptRecord {
    /// This record's cross-source dedup key (see [`dedup_key`]).
    pub fn dedup_key(&self) -> String {
        dedup_key(
            self.message_id.as_deref(),
            self.request_id.as_deref(),
            self.uuid.as_deref(),
            self.session_id.as_deref(),
            self.timestamp.as_deref(),
        )
    }
}

/// Scan cursor for the periodic full-rescan. `notify` drops events under load
/// (Windows silently, Linux `IN_Q_OVERFLOW`, macOS `MustScanSubDirs`), so a periodic walk of the
/// whole `projects` tree — not the event stream — is the AUTHORITATIVE source. To avoid re-reading
/// every file each sweep, the cursor remembers each path's `(mtime, size)` and reports only the new
/// or changed files to re-tail. Pure: the caller supplies the `stat`-ed listing; the byte-offset
/// [`TranscriptTailer`] then ensures a re-tailed file still emits only its appended lines.
#[derive(Default)]
pub struct ScanCursor {
    seen: std::collections::BTreeMap<String, (i64, u64)>,
}

impl ScanCursor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed the cursor from persisted `(path, mtime, size)` signatures so the first
    /// sweep after a restart reports only files that changed since we last persisted — unchanged
    /// files are skipped, not re-read. Pair with `TranscriptTailer::seeded` (offsets) to resume.
    pub fn seeded(entries: impl IntoIterator<Item = (String, i64, u64)>) -> Self {
        Self {
            seen: entries
                .into_iter()
                .map(|(p, mtime, size)| (p, (mtime, size)))
                .collect(),
        }
    }

    /// Given the current listing `(path, mtime_unix, size)`, return the paths new-or-changed since
    /// the last scan (to re-tail), and advance the cursor to exactly this listing (so a since-deleted
    /// file drops out and a later recreate re-reports). Order-preserving over the input.
    pub fn changed(&mut self, entries: &[(String, i64, u64)]) -> Vec<String> {
        let mut out = Vec::new();
        let mut next = std::collections::BTreeMap::new();
        for (path, mtime, size) in entries {
            let sig = (*mtime, *size);
            if self.seen.get(path) != Some(&sig) {
                out.push(path.clone());
            }
            next.insert(path.clone(), sig);
        }
        self.seen = next;
        out
    }

    /// Forget a path's signature so the next `changed()` re-reports it. Used to roll
    /// back a file whose DB write failed this sweep: dropping its signature makes the next sweep
    /// re-list it even though its (mtime,size) is unchanged, so the un-persisted turns get retried.
    pub fn invalidate(&mut self, path: &str) {
        self.seen.remove(path);
    }
}

/// Incremental tailer: tracks a per-file byte offset so a re-read of a growing
/// transcript parses ONLY the newly-appended lines, not the whole file each time. Pure — the
/// `notify` watcher (a thin IO adapter) hands it each changed file's current bytes. A trailing
/// partial line (no final newline yet) is held until it completes; a shrunk file (rotation / clear /
/// `--no-session-persistence` recreate) resets the offset and re-reads from the top.
#[derive(Default)]
pub struct TranscriptTailer {
    /// path key -> bytes consumed so far (always at a line boundary).
    offsets: std::collections::BTreeMap<String, usize>,
}

impl TranscriptTailer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed byte offsets from persisted scan-cursor rows so a re-tail after a restart
    /// resumes past the bytes already ingested, not from the top. Pair with `ScanCursor::seeded`.
    pub fn seeded(offsets: impl IntoIterator<Item = (String, usize)>) -> Self {
        Self {
            offsets: offsets.into_iter().collect(),
        }
    }

    /// The byte offset tailed so far for `key` (a line boundary), or `None` if never tailed. Used to
    /// persist the cursor after a sweep.
    pub fn offset(&self, key: &str) -> Option<usize> {
        self.offsets.get(key).copied()
    }

    /// Force the stored offset for `key` back to `offset`. Used to rewind a file to its
    /// pre-sweep boundary when its DB write failed, so the next sweep re-tails the same bytes.
    pub fn set_offset(&mut self, key: &str, offset: usize) {
        self.offsets.insert(key.to_string(), offset);
    }

    /// Tail `bytes` (the file's full current content) for `key`, returning records from complete
    /// lines appended since the last call. Advances the stored offset past the last complete line.
    pub fn tail(&mut self, key: &str, bytes: &[u8]) -> Vec<TranscriptRecord> {
        let mut prev = self.offsets.get(key).copied().unwrap_or(0);
        if bytes.len() < prev {
            prev = 0; // file shrank → truncation/rotation; re-read from the top
        }
        // Delegate to the chunk form so the line-boundary logic has a single source of truth. The
        // caller here holds the whole file, so the chunk begins at `prev` and runs to EOF.
        self.tail_chunk(key, prev, &bytes[prev..])
    }

    /// Tail a chunk that begins at absolute byte offset `chunk_start` and runs to EOF, returning
    /// records from complete lines within it and advancing the stored offset. This is the seek-based
    /// entry point: a caller that has seeked to the stored line-boundary offset and
    /// read ONLY the appended tail passes `(offset, tail_bytes)` instead of re-reading the whole
    /// file every sweep. `chunk_start` MUST be a line boundary — the stored offset always is, and a
    /// caller re-reading from the top after a shrink passes `0`. A trailing partial line waits.
    pub fn tail_chunk(
        &mut self,
        key: &str,
        chunk_start: usize,
        chunk: &[u8],
    ) -> Vec<TranscriptRecord> {
        // Only consume through the last newline; a trailing partial line waits for its completion.
        let complete_end = match chunk.iter().rposition(|&b| b == b'\n') {
            Some(i) => i + 1,
            None => {
                self.offsets.insert(key.to_string(), chunk_start);
                return Vec::new(); // no complete line yet in this chunk
            }
        };
        self.offsets
            .insert(key.to_string(), chunk_start + complete_end);
        String::from_utf8_lossy(&chunk[..complete_end])
            .lines()
            .filter_map(parse_transcript_line)
            .collect()
    }

    /// Forget a path (e.g. it was deleted) so a later recreate re-reads from the top.
    pub fn forget(&mut self, key: &str) {
        self.offsets.remove(key);
    }
}

/// Resolve the candidate Claude Code config roots, in priority order, from injected
/// env values (kept pure — the CLI passes real `std::env` lookups). `CLAUDE_CONFIG_DIR` wins and is
/// comma-split (Claude Code supports multiple); otherwise both `~/.claude` and the XDG location
/// (`$XDG_CONFIG_HOME/claude`, default `~/.config/claude`) are candidates. Each returned root is a
/// directory to look for a `projects/` subtree under. Order-preserving + de-duplicated.
pub fn resolve_claude_config_roots(
    config_dir_env: Option<&str>,
    xdg_config_home: Option<&str>,
    home: Option<&str>,
) -> Vec<String> {
    let mut roots: Vec<String> = Vec::new();
    let mut push = |r: String| {
        if !r.is_empty() && !roots.contains(&r) {
            roots.push(r);
        }
    };
    if let Some(cfg) = config_dir_env.filter(|s| !s.trim().is_empty()) {
        for part in cfg.split(',') {
            let p = part.trim();
            if !p.is_empty() {
                push(p.to_string());
            }
        }
        return roots; // an explicit override is authoritative — don't also guess defaults
    }
    if let Some(h) = home.filter(|s| !s.is_empty()) {
        push(format!("{h}/.claude"));
    }
    match xdg_config_home.filter(|s| !s.is_empty()) {
        Some(x) => push(format!("{x}/claude")),
        None => {
            if let Some(h) = home.filter(|s| !s.is_empty()) {
                push(format!("{h}/.config/claude"));
            }
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    // A realistic Claude Code v2.1.x assistant line (identity + split cache creation). Prompt text
    // is deliberately present in the fixture to prove we DON'T read it.
    const ASSISTANT: &str = r#"{"type":"assistant","sessionId":"sess-1","uuid":"u2","parentUuid":"u1","requestId":"req-9","timestamp":"2026-06-27T10:00:00Z","message":{"id":"msg_abc","model":"claude-opus-4-8","content":[{"type":"text","text":"SECRET REPLY do not store"}],"usage":{"input_tokens":1200,"output_tokens":300,"cache_read_input_tokens":5000,"cache_creation":{"ephemeral_5m_input_tokens":800,"ephemeral_1h_input_tokens":100}}}}"#;

    #[test]
    fn parses_usage_and_identity_only() {
        let r = parse_transcript_line(ASSISTANT).unwrap();
        assert_eq!(r.model, "claude-opus-4-8");
        assert_eq!(r.usage.fresh_input, 1200);
        assert_eq!(r.usage.output, 300);
        assert_eq!(r.usage.cache_read, 5000);
        assert_eq!(r.usage.cache_write_5m, 800);
        assert_eq!(r.usage.cache_write_1h, 100);
        assert_eq!(r.session_id.as_deref(), Some("sess-1"));
        assert_eq!(r.message_id.as_deref(), Some("msg_abc"));
        assert_eq!(r.request_id.as_deref(), Some("req-9"));
        assert_eq!(r.parent_uuid.as_deref(), Some("u1"));
        // The record has no field that could carry payload text.
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("SECRET"), "payload text is never retained");
    }

    #[test]
    fn handles_a_bare_cache_creation_total() {
        let line = r#"{"type":"assistant","sessionId":"s","message":{"id":"m","model":"claude-sonnet-4-6","usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":400}}}"#;
        let r = parse_transcript_line(line).unwrap();
        assert_eq!(r.usage.cache_write_5m, 400, "bare total → 5m tier");
        assert_eq!(r.usage.cache_write_1h, 0);
    }

    #[test]
    fn captures_thinking_tokens_as_reasoning() {
        // Real Bedrock/Anthropic shape: reasoning lives under output_tokens_details.thinking_tokens
        // and is a SUBSET of output. Regression guard for the JSONL lane reading reasoning=0.
        let line = r#"{"type":"assistant","sessionId":"s","message":{"id":"m","model":"claude-opus-4-8","usage":{"input_tokens":100,"output_tokens":300,"output_tokens_details":{"thinking_tokens":90}}}}"#;
        let r = parse_transcript_line(line).unwrap();
        assert_eq!(r.usage.output, 300);
        assert_eq!(r.usage.reasoning, 90, "thinking_tokens → reasoning");
        assert!(
            r.usage.reasoning <= r.usage.output,
            "reasoning is a subset of output"
        );
    }

    #[test]
    fn reasoning_defaults_to_zero_without_details() {
        let line = r#"{"type":"assistant","sessionId":"s","message":{"id":"m","model":"claude-opus-4-8","usage":{"input_tokens":10,"output_tokens":5}}}"#;
        let r = parse_transcript_line(line).unwrap();
        assert_eq!(r.usage.reasoning, 0);
    }

    #[test]
    fn skips_synthetic_error_user_and_zero_usage_lines() {
        // <synthetic> model.
        assert!(parse_transcript_line(
            r#"{"type":"assistant","message":{"id":"m","model":"<synthetic>","usage":{"output_tokens":0}}}"#
        )
        .is_none());
        // API error message.
        assert!(parse_transcript_line(
            r#"{"type":"assistant","isApiErrorMessage":true,"message":{"id":"m","model":"claude-opus-4-8","usage":{"output_tokens":5}}}"#
        )
        .is_none());
        // A user line (no message.usage).
        assert!(parse_transcript_line(
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#
        )
        .is_none());
        // Zero usage everywhere.
        assert!(parse_transcript_line(
            r#"{"type":"assistant","message":{"id":"m","model":"claude-opus-4-8","usage":{"input_tokens":0,"output_tokens":0}}}"#
        )
        .is_none());
        // Not JSON.
        assert!(parse_transcript_line("not json at all").is_none());
    }

    #[test]
    fn invalidate_forces_a_file_to_re_report_next_scan() {
        // Rolling back a failed write drops the file's signature so the next sweep
        // re-lists it even though (mtime,size) is unchanged.
        let mut c = ScanCursor::new();
        assert_eq!(c.changed(&[("a".into(), 100, 10)]), vec!["a".to_string()]);
        assert!(c.changed(&[("a".into(), 100, 10)]).is_empty()); // unchanged → skipped
        c.invalidate("a");
        assert_eq!(
            c.changed(&[("a".into(), 100, 10)]),
            vec!["a".to_string()],
            "invalidated file must re-report despite identical mtime/size"
        );
    }

    #[test]
    fn tailer_set_offset_rewinds_to_re_tail() {
        // Rewinding the offset makes the next read re-tail the same bytes.
        let mut t = TranscriptTailer::seeded(Vec::<(String, usize)>::new());
        let _ = t.tail_chunk("k", 0, b"line1\nline2\n");
        assert_eq!(t.offset("k"), Some(12));
        t.set_offset("k", 0);
        assert_eq!(t.offset("k"), Some(0));
    }

    #[test]
    fn bedrock_missing_request_id_still_parses() {
        // On Bedrock installs requestId is absent — the record still parses (dedup falls back to id).
        let line = r#"{"type":"assistant","sessionId":"s","message":{"id":"m","model":"claude-opus-4-8","usage":{"input_tokens":10,"output_tokens":5}}}"#;
        let r = parse_transcript_line(line).unwrap();
        assert_eq!(r.request_id, None);
        assert_eq!(r.message_id.as_deref(), Some("m"));
    }

    #[test]
    fn tailer_returns_only_newly_appended_complete_lines() {
        let mut t = TranscriptTailer::new();
        let l1 = format!("{ASSISTANT}\n");
        // First tail: one record.
        assert_eq!(t.tail("f", l1.as_bytes()).len(), 1);
        // Re-tail identical content: nothing new.
        assert_eq!(t.tail("f", l1.as_bytes()).len(), 0);
        // Append a second complete line + a trailing PARTIAL line (no newline): only the complete
        // one is returned; the partial is held.
        let l2 = format!(
            "{l1}{}\n{}",
            r#"{"type":"assistant","sessionId":"s","message":{"id":"m2","model":"claude-haiku-4-5","usage":{"output_tokens":9}}}"#,
            r#"{"type":"assistant","message":{"id":"partial""#,
        );
        let recs = t.tail("f", l2.as_bytes());
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].message_id.as_deref(), Some("m2"));
        // Once the partial line completes, it's returned on the next tail.
        let l3 = format!(
            "{}{}\n",
            &l2[..l2.rfind('\n').unwrap() + 1],
            r#"{"type":"assistant","sessionId":"s","message":{"id":"m3","model":"claude-haiku-4-5","usage":{"output_tokens":3}}}"#,
        );
        let recs = t.tail("f", l3.as_bytes());
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].message_id.as_deref(), Some("m3"));
    }

    #[test]
    fn scan_cursor_reports_only_new_or_changed_files() {
        let mut c = ScanCursor::new();
        // First scan: everything is new.
        let s1 = c.changed(&[("a".into(), 100, 10), ("b".into(), 100, 20)]);
        assert_eq!(s1, vec!["a".to_string(), "b".to_string()]);
        // Second scan, nothing changed → empty (no needless re-reads).
        assert!(c
            .changed(&[("a".into(), 100, 10), ("b".into(), 100, 20)])
            .is_empty());
        // b grew (size changed), c is new; a unchanged → only b, c.
        let s3 = c.changed(&[
            ("a".into(), 100, 10),
            ("b".into(), 100, 25),
            ("c".into(), 200, 5),
        ]);
        assert_eq!(s3, vec!["b".to_string(), "c".to_string()]);
        // A changed mtime alone (size same) still re-reports.
        let s4 = c.changed(&[
            ("a".into(), 101, 10),
            ("b".into(), 100, 25),
            ("c".into(), 200, 5),
        ]);
        assert_eq!(s4, vec!["a".to_string()]);
        // A deleted file drops from the cursor; recreating it later re-reports.
        c.changed(&[("a".into(), 101, 10)]); // b, c gone
        assert_eq!(
            c.changed(&[("a".into(), 101, 10), ("b".into(), 100, 25)]),
            vec!["b".to_string()]
        );
    }

    #[test]
    fn tailer_resets_on_truncation() {
        let mut t = TranscriptTailer::new();
        let full = format!("{ASSISTANT}\n{ASSISTANT}\n");
        assert_eq!(t.tail("f", full.as_bytes()).len(), 2);
        // File shrank (cleared/rotated) → re-read from the top.
        let one = format!("{ASSISTANT}\n");
        assert_eq!(
            t.tail("f", one.as_bytes()).len(),
            1,
            "truncation re-reads from 0"
        );
    }

    #[test]
    fn seeded_cursor_and_tailer_resume_incrementally_after_a_restart() {
        // seeding from persisted rows means a restart skips unchanged files and
        // resumes a grown file past what was already ingested — the anti-re-scan / anti-storm win.
        let two = format!("{ASSISTANT}\n{ASSISTANT}\n");
        let sig = (two.len() as i64 * 7, two.len() as u64); // (mtime, size) as if persisted
                                                            // A cursor seeded with the file's last-seen signature reports NO change for the same stat.
        let mut cur = ScanCursor::seeded([("f.jsonl".to_string(), sig.0, sig.1)]);
        assert!(
            cur.changed(&[("f.jsonl".to_string(), sig.0, sig.1)])
                .is_empty(),
            "unchanged file (same mtime+size) is skipped, not re-read"
        );
        // A tailer seeded at the end of what we already ingested yields nothing until the file grows.
        let mut t = TranscriptTailer::seeded([("f.jsonl".to_string(), two.len())]);
        assert_eq!(t.tail("f.jsonl", two.as_bytes()).len(), 0, "already tailed");
        assert_eq!(t.offset("f.jsonl"), Some(two.len()));
        // The file grows by one appended record → the cursor reports it changed, tailer emits only it.
        let three = format!("{two}{ASSISTANT}\n");
        assert_eq!(
            cur.changed(&[("f.jsonl".to_string(), sig.0 + 1, three.len() as u64)]),
            vec!["f.jsonl".to_string()],
            "a grown file is reported changed"
        );
        assert_eq!(
            t.tail("f.jsonl", three.as_bytes()).len(),
            1,
            "only the newly appended record is emitted"
        );
        assert_eq!(t.offset("f.jsonl"), Some(three.len()));
    }

    #[test]
    fn classifies_session_subagent_and_workflow_paths() {
        // Main session (real shape: projects/<dashed-cwd>/<sessionId>.jsonl).
        assert_eq!(
            classify_transcript_path("/Users/u/.claude/projects/-U-proj/a38a13e5.jsonl"),
            Some(TranscriptKind::Session {
                session_id: "a38a13e5".into()
            })
        );
        // Plain subagent.
        assert_eq!(
            classify_transcript_path("-U-proj/81b23a63/subagents/agent-a64f.jsonl"),
            Some(TranscriptKind::Subagent {
                parent_session: "81b23a63".into(),
                agent_id: "a64f".into(),
            })
        );
        // Workflow subagent (real shape observed under subagents/workflows/wf_<id>/).
        assert_eq!(
            classify_transcript_path(
                "-U-proj/a38a13e5/subagents/workflows/wf_aef46b20-057/agent-a67e.jsonl"
            ),
            Some(TranscriptKind::WorkflowSubagent {
                parent_session: "a38a13e5".into(),
                workflow_id: "wf_aef46b20-057".into(),
                agent_id: "a67e".into(),
            })
        );
        // A workflow totals file (.json, not .jsonl) is not a transcript here.
        assert_eq!(
            classify_transcript_path("-U-proj/a38a13e5/workflows/wf_aef46b20-057.json"),
            None
        );
    }

    #[test]
    fn parses_subagent_meta_identity_only() {
        let m = parse_subagent_meta(
            br#"{"toolUseId":"toolu_bdrk_123","agentType":"explore","spawnDepth":2,"prompt":"SECRET"}"#,
        );
        assert_eq!(m.tool_use_id.as_deref(), Some("toolu_bdrk_123"));
        assert_eq!(m.agent_type.as_deref(), Some("explore"));
        assert_eq!(m.spawn_depth, Some(2));
        assert_eq!(parse_subagent_meta(b"not json"), SubagentMeta::default());
    }

    #[test]
    fn dedup_key_is_message_id_alone_when_present() {
        // message.id ALONE — never split by request_id or uuid.
        assert_eq!(
            dedup_key(Some("m"), Some("r"), Some("u"), Some("s"), Some("t")),
            "id:m"
        );
        // No message.id → session + timestamp last resort.
        assert_eq!(dedup_key(None, None, None, Some("s"), Some("t")), "st:s|t");
        // THE BUG THIS FIXES: Claude Code replays the same assistant turn with a DIFFERENT per-line
        // uuid (and Bedrock omits request_id). All copies must collapse to ONE key, or backfill
        // over-counts ~2.2x. Same id + differing uuid/request_id/session/ts → identical key.
        assert_eq!(
            dedup_key(Some("m"), None, Some("uA"), Some("s"), Some("t1")),
            dedup_key(Some("m"), None, Some("uB"), Some("s"), Some("t2"))
        );
        // Cross-lane: same message.id from OTLP + JSONL collides to one key regardless of the rest.
        assert_eq!(
            dedup_key(Some("m"), Some("r"), None, None, None),
            dedup_key(Some("m"), Some("r"), Some("uX"), Some("sX"), Some("tX"))
        );
    }

    #[test]
    fn resolves_config_roots_from_env() {
        // Explicit override wins and is comma-split; defaults are NOT also added.
        assert_eq!(
            resolve_claude_config_roots(Some("/a,/b"), Some("/xdg"), Some("/home/u")),
            vec!["/a".to_string(), "/b".to_string()]
        );
        // Default: ~/.claude + $XDG/claude.
        assert_eq!(
            resolve_claude_config_roots(None, Some("/home/u/.config"), Some("/home/u")),
            vec![
                "/home/u/.claude".to_string(),
                "/home/u/.config/claude".to_string()
            ]
        );
        // Default without XDG falls back to ~/.config/claude.
        assert_eq!(
            resolve_claude_config_roots(None, None, Some("/home/u")),
            vec![
                "/home/u/.claude".to_string(),
                "/home/u/.config/claude".to_string()
            ]
        );
    }

    #[test]
    fn parse_transcript_yields_only_accountable_turns() {
        let doc = format!(
            "{ASSISTANT}\n{}\n{}\n\n{}",
            r#"{"type":"user","message":{"content":"hello"}}"#,
            r#"{"type":"assistant","message":{"id":"m2","model":"<synthetic>","usage":{"output_tokens":0}}}"#,
            r#"{"type":"assistant","sessionId":"sess-1","message":{"id":"m3","model":"claude-haiku-4-5","usage":{"output_tokens":42}}}"#,
        );
        let recs = parse_transcript(doc.as_bytes());
        assert_eq!(
            recs.len(),
            2,
            "the assistant turn + the haiku turn; user/synthetic/blank skipped"
        );
        assert_eq!(recs[0].message_id.as_deref(), Some("msg_abc"));
        assert_eq!(recs[1].message_id.as_deref(), Some("m3"));
    }
}
