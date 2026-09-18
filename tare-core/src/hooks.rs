//! Claude Code hook-event model + session lifecycle state machine.
//!
//! Claude Code can be wired (additively, reversibly — see `tare connect`) to `curl` each hook's
//! stdin JSON to a loopback intake. Hooks are the ONLY *authoritative* signal for session liveness:
//! `SessionStart` (with a `source`: startup/resume/clear/compact), per-turn `Stop`, idle/permission
//! `Notification`, and best-effort `SessionEnd` (with a `reason`). This module parses those events
//! and folds them into a per-session lifecycle state.
//!
//! PRIVACY: hook payloads never carry cost/tokens and we never retain prompt text — only the opaque
//! `session_id`, the `hook_event_name`, a few enum-valued fields (`source`/`reason`), an optional
//! `prompt_id` join key, and the local paths Claude Code itself puts on every event. Cost/tokens
//! come from the OTLP/JSONL lanes, not here.
//!
//! CLOCK-FREE: a timestamp is read from the payload when present and is otherwise `None` — the core
//! never invents one. Async hooks have no cross-fire dedup, so we self-dedup turn events by
//! `(session_id, prompt_id)`.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A parsed Claude Code hook event (payload-free).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum HookEvent {
    /// Fires on every session; `source` ∈ startup|resume|clear|compact; `model` sometimes present.
    SessionStart {
        source: Option<String>,
        model: Option<String>,
    },
    /// A user turn began.
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    /// Idle or awaiting-permission notification (session alive but not producing).
    Notification,
    /// The assistant finished responding to a turn (graceful stop of generation).
    Stop,
    /// A subagent finished — does not end the parent turn.
    SubagentStop,
    PreCompact,
    PostCompact,
    /// Best-effort session end; `reason` ∈ clear|resume|logout|exit. Never fires on SIGKILL/OOM.
    SessionEnd {
        reason: Option<String>,
    },
}

impl HookEvent {
    /// The wire `hook_event_name` discriminant (used for dedup + display).
    pub fn name(&self) -> &'static str {
        match self {
            HookEvent::SessionStart { .. } => "SessionStart",
            HookEvent::UserPromptSubmit => "UserPromptSubmit",
            HookEvent::PreToolUse => "PreToolUse",
            HookEvent::PostToolUse => "PostToolUse",
            HookEvent::Notification => "Notification",
            HookEvent::Stop => "Stop",
            HookEvent::SubagentStop => "SubagentStop",
            HookEvent::PreCompact => "PreCompact",
            HookEvent::PostCompact => "PostCompact",
            HookEvent::SessionEnd { .. } => "SessionEnd",
        }
    }
}

/// One parsed hook record: the event plus its common fields (all payload-free).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRecord {
    pub session_id: String,
    pub event: HookEvent,
    /// Turn join key (`prompt_id`/`prompt.id`); the clock-free link to OTLP counts + the dedup key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
    /// Local working directory Claude Code stamps on every event (metadata, not payload).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Local transcript file path (metadata, not payload) — feeds the JSONL backfill lane.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    /// Unix seconds from the payload when present; `None` otherwise (the core never invents time).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts_unix: Option<i64>,
}

/// The lifecycle state derived from a session's hook stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionState {
    /// A `SessionStart` was seen but no turn activity yet.
    Started,
    /// A turn is in progress (prompt submitted / tools running).
    Working,
    /// Alive but idle or awaiting permission (a `Notification`).
    Waiting,
    /// The assistant finished a turn (a `Stop`); alive, ready for the next prompt.
    GracefulStopped,
    /// A `SessionEnd` was seen.
    Ended,
}

/// Read the trimmed string field `key` from a JSON object, capping length so a hostile/oversized
/// value can't smuggle bulk text (ids/paths/enums are short).
fn str_field(v: &serde_json::Value, key: &str, cap: usize) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .map(|s| s.chars().take(cap).collect::<String>())
        .filter(|s| !s.is_empty())
}

/// Read an opaque identifier without truncating it. Truncation can collapse two distinct session
/// or prompt ids into one dedup key, so an overlong identifier invalidates the record instead.
fn id_field(v: &serde_json::Value, key: &str, cap: usize) -> Result<Option<String>, String> {
    id_value(v.get(key), key, cap)
}

fn id_value(
    value: Option<&serde_json::Value>,
    field: &str,
    cap: usize,
) -> Result<Option<String>, String> {
    let Some(value) = value.and_then(|x| x.as_str()) else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > cap {
        return Err(format!("hook: {field} exceeds {cap} characters"));
    }
    Ok(Some(value.to_string()))
}

/// Parse one hook payload (as JSON) into a [`HookRecord`]. Errors on a missing/blank
/// `session_id` or an unknown `hook_event_name`; retains no prompt text.
pub fn parse_hook(v: &serde_json::Value) -> Result<HookRecord, String> {
    let session_id = id_field(v, "session_id", 128)?.ok_or("hook: missing session_id")?;
    let name = v
        .get("hook_event_name")
        .and_then(|x| x.as_str())
        .ok_or("hook: missing hook_event_name")?;
    let event = match name {
        "SessionStart" => HookEvent::SessionStart {
            source: str_field(v, "source", 32),
            model: str_field(v, "model", 64),
        },
        "UserPromptSubmit" => HookEvent::UserPromptSubmit,
        "PreToolUse" => HookEvent::PreToolUse,
        "PostToolUse" => HookEvent::PostToolUse,
        "Notification" => HookEvent::Notification,
        "Stop" => HookEvent::Stop,
        "SubagentStop" => HookEvent::SubagentStop,
        "PreCompact" => HookEvent::PreCompact,
        "PostCompact" => HookEvent::PostCompact,
        "SessionEnd" => HookEvent::SessionEnd {
            reason: str_field(v, "reason", 32),
        },
        other if other.chars().count() <= 64 => {
            return Err(format!("hook: unknown hook_event_name {other:?}"));
        }
        _ => return Err("hook: hook_event_name exceeds 64 characters".into()),
    };
    // prompt_id may arrive as `prompt_id` (hooks) or `prompt.id`/`prompt_uuid` — accept the common
    // spellings; it's the clock-free join key to OTLP `prompt.id`.
    let prompt_id = if let Some(id) = id_field(v, "prompt_id", 128)? {
        Some(id)
    } else if let Some(id) = id_field(v, "prompt_uuid", 128)? {
        Some(id)
    } else {
        id_value(
            v.get("prompt").and_then(|prompt| prompt.get("id")),
            "prompt.id",
            128,
        )?
    };
    let ts_unix = v
        .get("timestamp")
        .or_else(|| v.get("ts"))
        .and_then(|x| x.as_i64())
        .filter(|timestamp| *timestamp >= 0);
    Ok(HookRecord {
        session_id,
        event,
        prompt_id,
        cwd: str_field(v, "cwd", 512),
        transcript_path: str_field(v, "transcript_path", 512),
        ts_unix,
    })
}

/// The lifecycle transition: given the current state and the next event, return the new state.
/// Events that don't change lifecycle (subagent stop, compaction) leave the state as-is.
pub fn advance(state: SessionState, event: &HookEvent) -> SessionState {
    if state == SessionState::Ended && !matches!(event, HookEvent::SessionStart { .. }) {
        return SessionState::Ended;
    }
    match event {
        HookEvent::SessionStart { .. } => SessionState::Started,
        HookEvent::UserPromptSubmit | HookEvent::PreToolUse | HookEvent::PostToolUse => {
            SessionState::Working
        }
        HookEvent::Notification => SessionState::Waiting,
        HookEvent::Stop => SessionState::GracefulStopped,
        HookEvent::SessionEnd { .. } => SessionState::Ended,
        // Subagent completion + in-place compaction don't move the top-level lifecycle.
        HookEvent::SubagentStop | HookEvent::PreCompact | HookEvent::PostCompact => state,
    }
}

/// A folded per-session lifecycle snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub state: SessionState,
    /// `source` from the last `SessionStart` (startup/resume/clear/compact).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// `model` from `SessionStart` when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `reason` from the last `SessionEnd` when ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<String>,
    /// Distinct user turns seen (deduped by `prompt_id`).
    pub turns: u32,
    /// Latest payload timestamp seen for this session (Unix seconds), if any were present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ts_unix: Option<i64>,
}

/// Fold a stream of hook records into per-session lifecycle snapshots. Records are processed in
/// order; turn events are self-deduped by `(session_id, prompt_id)` (async hooks re-fire without
/// cross-dedup). Snapshots are returned sorted by `session_id` (deterministic).
pub fn fold_sessions(records: &[HookRecord]) -> Vec<SessionSnapshot> {
    let mut snaps: BTreeMap<String, SessionSnapshot> = BTreeMap::new();
    // Dedup key for turn-bearing events: (session_id, event_name, prompt_id). Only applied when a
    // prompt_id is present — that's the join key async re-fires share.
    let mut seen: BTreeSet<(String, &'static str, String)> = BTreeSet::new();

    for r in records {
        if let Some(pid) = &r.prompt_id {
            let key = (r.session_id.clone(), r.event.name(), pid.clone());
            if !seen.insert(key) {
                continue; // duplicate async re-fire of the same turn event — skip
            }
        }
        let snap = snaps
            .entry(r.session_id.clone())
            .or_insert_with(|| SessionSnapshot {
                session_id: r.session_id.clone(),
                state: SessionState::Started,
                source: None,
                model: None,
                end_reason: None,
                turns: 0,
                last_ts_unix: None,
            });
        if snap.state == SessionState::Ended && !matches!(&r.event, HookEvent::SessionStart { .. })
        {
            continue; // Ignore late async delivery after the authoritative end event.
        }
        snap.state = advance(snap.state, &r.event);
        match &r.event {
            HookEvent::SessionStart { source, model } => {
                snap.source = source.clone();
                snap.model = model.clone();
                snap.end_reason = None;
            }
            HookEvent::UserPromptSubmit => snap.turns = snap.turns.saturating_add(1),
            HookEvent::SessionEnd { reason } => snap.end_reason = reason.clone(),
            _ => {}
        }
        if let Some(ts) = r.ts_unix {
            snap.last_ts_unix = Some(snap.last_ts_unix.map_or(ts, |cur| cur.max(ts)));
        }
    }
    snaps.into_values().collect()
}

/// Fuse the three liveness signals into an authoritative Live state, retiring the
/// pure-recency `ended` guess wherever a stronger signal exists. Inputs: `recency` (the
/// recency-window verdict — `working`/`idle`/`ended`), the session's latest hook `phase` (from the
/// receiver: `started`/`working`/`waiting`/`stopped`/`ended`, or `None` if no hook was seen), and
/// whether a live agent process was detected for it. Returns `(state, authority)` where authority is
/// `hook` | `process` | `recency` so the UI can show WHY.
///
/// Precedence: an explicit `SessionEnd` hook ends the session even if events were recent (authority
/// hook); otherwise a detected live process or a non-end hook means it is NOT ended (alive, refined
/// to working/idle); with neither signal we fall back to the recency verdict — including its guess.
pub fn fuse_state(
    recency: &str,
    hook_phase: Option<&str>,
    process_alive: bool,
) -> (&'static str, &'static str) {
    // Static equivalents so we return &'static str regardless of the borrowed `recency` input.
    let recency_state = match recency {
        "working" => "working",
        "idle" => "idle",
        _ => "ended",
    };
    match hook_phase {
        Some("ended") => ("ended", "hook"),
        _ if process_alive => {
            // A running process means alive; keep "working" if events are also fresh, else "idle".
            (
                if recency_state == "working" {
                    "working"
                } else {
                    "idle"
                },
                "process",
            )
        }
        Some("working") | Some("started") => ("working", "hook"),
        Some("waiting") | Some("stopped") => ("idle", "hook"),
        _ => (recency_state, "recency"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fuse_session_end_hook_ends_even_when_recent() {
        // Recency says "working" (fresh events) but a SessionEnd hook fired → authoritatively ended.
        assert_eq!(
            fuse_state("working", Some("ended"), false),
            ("ended", "hook")
        );
    }

    #[test]
    fn fuse_live_process_overrides_recency_ended_guess() {
        // Recency would call it ended (>5min since last event), but the process is still running.
        assert_eq!(fuse_state("ended", None, true), ("idle", "process"));
        // A live process with fresh events stays "working".
        assert_eq!(fuse_state("working", None, true), ("working", "process"));
    }

    #[test]
    fn fuse_non_end_hook_keeps_session_alive() {
        assert_eq!(
            fuse_state("ended", Some("waiting"), false),
            ("idle", "hook")
        );
        assert_eq!(
            fuse_state("ended", Some("working"), false),
            ("working", "hook")
        );
    }

    #[test]
    fn fuse_falls_back_to_recency_without_hook_or_process() {
        assert_eq!(fuse_state("ended", None, false), ("ended", "recency"));
        assert_eq!(fuse_state("working", None, false), ("working", "recency"));
        assert_eq!(fuse_state("idle", None, false), ("idle", "recency"));
    }

    #[test]
    fn fuse_session_end_wins_over_a_live_process() {
        // Explicit end is the strongest signal (the process detection may be a stale snapshot).
        assert_eq!(
            fuse_state("working", Some("ended"), true),
            ("ended", "hook")
        );
    }

    #[test]
    fn parses_common_and_event_specific_fields() {
        let v = json!({
            "hook_event_name": "SessionStart",
            "session_id": "sess-1",
            "source": "resume",
            "model": "claude-opus-4-8",
            "cwd": "/home/u/proj",
            "transcript_path": "/home/u/.claude/projects/x/sess-1.jsonl",
            "timestamp": 1_700_000_000
        });
        let r = parse_hook(&v).unwrap();
        assert_eq!(r.session_id, "sess-1");
        assert_eq!(
            r.event,
            HookEvent::SessionStart {
                source: Some("resume".into()),
                model: Some("claude-opus-4-8".into())
            }
        );
        assert_eq!(r.ts_unix, Some(1_700_000_000));
        assert_eq!(r.cwd.as_deref(), Some("/home/u/proj"));
    }

    #[test]
    fn rejects_missing_session_id_and_unknown_event() {
        assert!(parse_hook(&json!({"hook_event_name": "Stop"})).is_err());
        assert!(parse_hook(&json!({"session_id": "   ", "hook_event_name": "Stop"})).is_err());
        assert!(parse_hook(&json!({"session_id": "s", "hook_event_name": "Frobnicate"})).is_err());
        assert!(parse_hook(&json!({
            "session_id": "x".repeat(129), "hook_event_name": "Stop"
        }))
        .is_err());
        assert!(parse_hook(&json!({
            "session_id": "s", "hook_event_name": "x".repeat(65)
        }))
        .is_err());
    }

    #[test]
    fn trims_ids_consistently_without_truncating_them() {
        let record = parse_hook(&json!({
            "session_id": "  session  ",
            "hook_event_name": "UserPromptSubmit",
            "prompt": {"id": "  prompt  "},
            "cwd": "  /tmp/project  ",
            "timestamp": -1
        }))
        .unwrap();
        assert_eq!(record.session_id, "session");
        assert_eq!(record.prompt_id.as_deref(), Some("prompt"));
        assert_eq!(record.cwd.as_deref(), Some("/tmp/project"));
        assert_eq!(record.ts_unix, None, "negative hook times are invalid");

        assert!(parse_hook(&json!({
            "session_id": "s",
            "hook_event_name": "UserPromptSubmit",
            "prompt": {"id": "x".repeat(129)}
        }))
        .is_err());
    }

    #[test]
    fn lifecycle_walks_started_working_stopped_ended() {
        let mk =
            |name: &str| parse_hook(&json!({"session_id": "s", "hook_event_name": name})).unwrap();
        let prompt = parse_hook(
            &json!({"session_id": "s", "hook_event_name": "UserPromptSubmit", "prompt_id": "p1"}),
        )
        .unwrap();
        let records = vec![mk("SessionStart"), prompt, mk("Stop"), mk("SessionEnd")];
        let snaps = fold_sessions(&records);
        assert_eq!(snaps.len(), 1);
        let s = &snaps[0];
        assert_eq!(s.state, SessionState::Ended, "SessionEnd is terminal");
        assert_eq!(s.turns, 1);
    }

    #[test]
    fn end_is_terminal_until_a_new_session_start() {
        let event = |name: &str, extra: serde_json::Value| {
            let mut value = json!({"session_id": "s", "hook_event_name": name});
            value.as_object_mut().unwrap().extend(
                extra
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
            parse_hook(&value).unwrap()
        };
        let ended = fold_sessions(&[
            event("SessionStart", json!({"source": "startup", "model": "m"})),
            event("SessionEnd", json!({"reason": "exit", "timestamp": 10})),
            event(
                "UserPromptSubmit",
                json!({"prompt_id": "late", "timestamp": 11}),
            ),
        ]);
        assert_eq!(ended[0].state, SessionState::Ended);
        assert_eq!(ended[0].turns, 0);
        assert_eq!(ended[0].last_ts_unix, Some(10));

        let restarted = fold_sessions(&[
            event("SessionStart", json!({"source": "startup", "model": "old"})),
            event("SessionEnd", json!({"reason": "exit"})),
            event("SessionStart", json!({"source": "resume"})),
        ]);
        assert_eq!(restarted[0].state, SessionState::Started);
        assert_eq!(restarted[0].source.as_deref(), Some("resume"));
        assert_eq!(restarted[0].model, None);
        assert_eq!(restarted[0].end_reason, None);
        assert_eq!(
            advance(SessionState::Ended, &HookEvent::Notification),
            SessionState::Ended
        );
    }

    #[test]
    fn notification_and_stop_distinguish_waiting_from_graceful_stop() {
        let notif =
            parse_hook(&json!({"session_id": "s", "hook_event_name": "Notification"})).unwrap();
        assert_eq!(fold_sessions(&[notif])[0].state, SessionState::Waiting);
        let stop = parse_hook(&json!({"session_id": "s", "hook_event_name": "Stop"})).unwrap();
        assert_eq!(
            fold_sessions(&[stop])[0].state,
            SessionState::GracefulStopped
        );
    }

    #[test]
    fn self_dedups_turn_events_by_session_and_prompt_id() {
        // The same UserPromptSubmit re-fires (async, no cross-dedup) — count the turn once.
        let p = || {
            parse_hook(&json!({
                "session_id": "s", "hook_event_name": "UserPromptSubmit", "prompt_id": "p1"
            }))
            .unwrap()
        };
        let snaps = fold_sessions(&[p(), p(), p()]);
        assert_eq!(snaps[0].turns, 1, "three re-fires of prompt p1 = one turn");
    }

    #[test]
    fn subagent_stop_and_compaction_do_not_move_lifecycle() {
        let start =
            parse_hook(&json!({"session_id": "s", "hook_event_name": "SessionStart"})).unwrap();
        let prompt = parse_hook(
            &json!({"session_id": "s", "hook_event_name": "UserPromptSubmit", "prompt_id": "p1"}),
        )
        .unwrap();
        let sub =
            parse_hook(&json!({"session_id": "s", "hook_event_name": "SubagentStop"})).unwrap();
        let compact =
            parse_hook(&json!({"session_id": "s", "hook_event_name": "PostCompact"})).unwrap();
        // Working (from the prompt) survives a subagent stop + a compaction.
        let snaps = fold_sessions(&[start, prompt, sub, compact]);
        assert_eq!(snaps[0].state, SessionState::Working);
    }
}
