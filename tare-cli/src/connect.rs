//! `tare connect` / `tare disconnect`: additively and reversibly wire a coding agent's settings
//! file (Claude Code `settings*.json`) to export OpenTelemetry to Tare's local receiver.
//!
//! Safety contract for local AI configuration:
//!   - Touches ONLY the telemetry keys below; never `ANTHROPIC_BASE_URL`, `apiKeyHelper`, or any
//!     other existing key.
//!   - Refuses to overwrite an already-set `OTEL_EXPORTER_OTLP_ENDPOINT` (likely a corporate
//!     collector) unless `--force` — so it can't silently hijack the user's telemetry.
//!   - `disconnect` removes only keys whose values are Tare's own (loopback endpoint), so it never
//!     deletes config Tare didn't plausibly add.
//!
//! The JSON transforms are pure + unit-tested; file I/O is a thin wrapper.

use serde_json::{Map, Value};

/// Fixed telemetry keys Tare manages (key, Tare's canonical value).
const FIXED: &[(&str, &str)] = &[
    ("CLAUDE_CODE_ENABLE_TELEMETRY", "1"),
    ("OTEL_METRICS_EXPORTER", "otlp"),
    ("OTEL_LOGS_EXPORTER", "otlp"),
    ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/json"),
    // Cut Claude Code's log-export batch delay from the 5000ms default to ~1s so the Live view
    // reflects activity within a second. The per-request token detail rides the LOGS
    // signal (`claude_code.api_request`), so this alone captures short `-p` one-shots.
    ("OTEL_LOGS_EXPORT_INTERVAL", "1000"),
    // Also shorten the METRIC export interval from the 60s default: a one-shot
    // `claude -p …` that exits in <60s would otherwise never flush `claude_code.token.usage`, so the
    // aggregate metric lane silently captured nothing for short sessions. 10s keeps the metric cadence
    // modest (not the 1s logs cadence) while still flushing before a typical one-shot exits.
    ("OTEL_METRIC_EXPORT_INTERVAL", "10000"),
];
const ENDPOINT: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";

/// Per-signal endpoint overrides. A value here supersedes the general `ENDPOINT` for that signal, so
/// a pre-existing one would silently divert that signal AWAY from Tare even after we wire the
/// general endpoint — we must probe them and warn (the `ANTHROPIC_BASE_URL` lesson).
const PER_SIGNAL_ENDPOINTS: &[&str] = &[
    "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
    "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
];

/// True for an endpoint value that already points at a local Tare receiver (loopback).
fn is_local_endpoint(v: &str) -> bool {
    v.contains("127.0.0.1") || v.contains("localhost")
}

fn ensure_env(settings: &mut Value) -> &mut Map<String, Value> {
    if !settings.is_object() {
        *settings = Value::Object(Map::new());
    }
    let obj = settings.as_object_mut().unwrap();
    let env = obj
        .entry("env")
        .or_insert_with(|| Value::Object(Map::new()));
    if !env.is_object() {
        *env = Value::Object(Map::new());
    }
    env.as_object_mut().unwrap()
}

/// Additively merge Tare's telemetry keys into a settings document. Returns the updated document
/// and a human-readable log of what changed (and what was kept).
pub fn otel_connect(mut settings: Value, endpoint: &str, force: bool) -> (Value, Vec<String>) {
    let mut msgs = Vec::new();
    {
        let env = ensure_env(&mut settings);
        for (k, v) in FIXED {
            let cur = env.get(*k).and_then(|x| x.as_str()).map(str::to_string);
            match cur.as_deref() {
                None => {
                    env.insert((*k).to_string(), Value::String((*v).to_string()));
                    msgs.push(format!("set {k}={v}"));
                }
                Some(c) if c == *v => msgs.push(format!("{k} already set")),
                Some(c) if force => {
                    env.insert((*k).to_string(), Value::String((*v).to_string()));
                    msgs.push(format!("overwrote {k} (was {c})"));
                }
                Some(c) => msgs.push(format!("kept existing {k}={c} (use --force to change)")),
            }
        }
        let cur = env
            .get(ENDPOINT)
            .and_then(|x| x.as_str())
            .map(str::to_string);
        match cur.as_deref() {
            None => {
                env.insert(ENDPOINT.to_string(), Value::String(endpoint.to_string()));
                msgs.push(format!("set {ENDPOINT}={endpoint}"));
            }
            Some(c) if c == endpoint => msgs.push(format!("{ENDPOINT} already points at Tare")),
            Some(c) if force => {
                env.insert(ENDPOINT.to_string(), Value::String(endpoint.to_string()));
                msgs.push(format!("overwrote {ENDPOINT} (was {c})"));
            }
            Some(c) => msgs.push(format!(
                "WARNING: {ENDPOINT} already set to {c} (a corporate collector?); NOT changing it \
                 — telemetry will keep going there, not to Tare. Re-run with --force to override."
            )),
        }
        // Probe the per-signal endpoint overrides: each supersedes the general
        // endpoint for its signal, so a pre-existing non-local one silently diverts that signal
        // away from Tare. Warn (skip) by default; on --force clear it so the general Tare endpoint
        // applies. We never SET these ourselves — the general endpoint is enough.
        for k in PER_SIGNAL_ENDPOINTS {
            let cur = env.get(*k).and_then(|x| x.as_str()).map(str::to_string);
            match cur.as_deref() {
                None => {}
                Some(c) if is_local_endpoint(c) => msgs.push(format!("{k} already local")),
                Some(c) if force => {
                    env.remove(*k);
                    msgs.push(format!(
                        "cleared {k} (was {c}) so this signal routes to Tare via {ENDPOINT}"
                    ));
                }
                Some(c) => msgs.push(format!(
                    "WARNING: {k}={c} OVERRIDES the general endpoint for this signal — it will NOT \
                     reach Tare. Re-run with --force to clear it."
                )),
            }
        }
    }
    (settings, msgs)
}

/// Remove Tare's telemetry keys (only when their values are Tare's own), reversing `otel_connect`.
pub fn otel_disconnect(mut settings: Value) -> (Value, Vec<String>) {
    let mut msgs = Vec::new();
    if let Some(obj) = settings.as_object_mut() {
        let mut drop_env = false;
        if let Some(env) = obj.get_mut("env").and_then(|e| e.as_object_mut()) {
            for (k, v) in FIXED {
                if env.get(*k).and_then(|x| x.as_str()) == Some(*v) {
                    env.remove(*k);
                    msgs.push(format!("removed {k}"));
                }
            }
            let cur = env
                .get(ENDPOINT)
                .and_then(|x| x.as_str())
                .map(str::to_string);
            match cur.as_deref() {
                Some(c) if c.contains("127.0.0.1") || c.contains("localhost") => {
                    env.remove(ENDPOINT);
                    msgs.push(format!("removed {ENDPOINT}"));
                }
                Some(c) => msgs.push(format!(
                    "kept {ENDPOINT}={c} (not a Tare loopback endpoint)"
                )),
                None => {}
            }
            drop_env = env.is_empty();
        }
        if drop_env {
            obj.remove("env");
            msgs.push("removed now-empty env block".into());
        }
    }
    if msgs.is_empty() {
        msgs.push("nothing to disconnect (no Tare telemetry keys found)".into());
    }
    (settings, msgs)
}

// ---- Claude Code lifecycle hooks: authoritative session liveness ----

/// Lifecycle events Tare registers a liveness hook on. The SAME intake command works for every one
/// — Claude Code passes the event JSON (with `hook_event_name`) on stdin — so registration is
/// uniform. Scope is deliberately the lifecycle set (start/turn-end/subagent/compaction/end); we do
/// NOT hook per-tool or prompt events (too chatty, and cost/turns come from the OTLP/JSONL lanes).
const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "Stop",
    "SubagentStop",
    "PreCompact",
    "PostCompact",
    "SessionEnd",
];

/// Stable substring identifying a Tare hook command, so `disconnect` removes ONLY ours and connect
/// can dedup its own prior registration (even if the port changed).
const HOOK_MARKER: &str = "/__tare/hook";

/// The command a Tare hook runs: POST the hook's stdin JSON to the loopback intake, time-bounded and
/// best-effort — it always ends `|| true` so a receiver hiccup can never block or fail the agent.
fn hook_command(endpoint: &str) -> String {
    format!(
        "curl -sS -m 2 -X POST -H 'Content-Type: application/json' --data-binary @- {endpoint}{HOOK_MARKER} >/dev/null 2>&1 || true"
    )
}

/// True if a hook matcher-group contains a Tare command (identified by [`HOOK_MARKER`]).
fn group_is_tare(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hs| {
            hs.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| c.contains(HOOK_MARKER))
            })
        })
        .unwrap_or(false)
}

/// Additively register Tare's lifecycle hooks in a settings document. Idempotent (replaces a prior
/// Tare group per event, so re-running or a changed port never duplicates), and it touches ONLY its
/// own groups — any hooks the user or another tool added are preserved. `SessionEnd` is registered
/// synchronously (Claude Code gives it a 1.5s budget); the rest are `async` so they never add
/// latency to a turn. Respects `disableAllHooks` (registers, but warns it won't fire).
pub fn hooks_connect(mut settings: Value, endpoint: &str) -> (Value, Vec<String>) {
    let mut msgs = Vec::new();
    let cmd = hook_command(endpoint);
    if settings.get("disableAllHooks").and_then(|v| v.as_bool()) == Some(true) {
        msgs.push(
            "WARNING: `disableAllHooks` is true — Tare's hooks are registered but WON'T fire until \
             you clear it (live session state will fall back to the recency guess)."
                .into(),
        );
    }
    if !settings.is_object() {
        settings = Value::Object(Map::new());
    }
    let obj = settings.as_object_mut().unwrap();
    let hooks_v = obj
        .entry("hooks".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(hooks) = hooks_v.as_object_mut() else {
        msgs.push("WARNING: existing `hooks` is not an object; leaving it untouched.".into());
        return (settings, msgs);
    };
    for ev in HOOK_EVENTS {
        let arr_v = hooks
            .entry((*ev).to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        let Some(groups) = arr_v.as_array_mut() else {
            msgs.push(format!("WARNING: hooks.{ev} is not an array; skipping"));
            continue;
        };
        // Drop any prior Tare group first (idempotent), keeping every non-Tare hook intact.
        groups.retain(|g| !group_is_tare(g));
        let mut hook_def = Map::new();
        hook_def.insert("type".into(), Value::String("command".into()));
        hook_def.insert("command".into(), Value::String(cmd.clone()));
        if *ev != "SessionEnd" {
            hook_def.insert("async".into(), Value::Bool(true));
        }
        let mut group = Map::new();
        group.insert("hooks".into(), Value::Array(vec![Value::Object(hook_def)]));
        groups.push(Value::Object(group));
    }
    msgs.push(format!(
        "registered lifecycle hooks ({}) → {endpoint}{HOOK_MARKER}",
        HOOK_EVENTS.join(", ")
    ));
    (settings, msgs)
}

/// Remove ONLY Tare's lifecycle hooks, reversing [`hooks_connect`]. Non-Tare hooks, and any events
/// they use, are left exactly as they were; empty containers Tare emptied are cleaned up.
pub fn hooks_disconnect(mut settings: Value) -> (Value, Vec<String>) {
    let mut msgs = Vec::new();
    if let Some(obj) = settings.as_object_mut() {
        if let Some(hooks) = obj.get_mut("hooks").and_then(|h| h.as_object_mut()) {
            let events: Vec<String> = hooks.keys().cloned().collect();
            let mut removed = 0;
            for ev in &events {
                if let Some(groups) = hooks.get_mut(ev).and_then(|a| a.as_array_mut()) {
                    let before = groups.len();
                    groups.retain(|g| !group_is_tare(g));
                    removed += before - groups.len();
                    if groups.is_empty() {
                        hooks.remove(ev);
                    }
                }
            }
            if removed > 0 {
                msgs.push(format!("removed {removed} Tare lifecycle hook(s)"));
            }
            if hooks.is_empty() {
                obj.remove("hooks");
                msgs.push("removed now-empty hooks block".into());
            }
        }
    }
    (settings, msgs)
}

fn read_settings(path: &str) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => {
            serde_json::from_str(&s).map_err(|e| format!("parse {path}: {e}"))
        }
        _ => Ok(Value::Object(Map::new())),
    }
}

fn write_settings(path: &str, v: &Value) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
        }
    }
    let body = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    // Atomic write: stage to a temp sibling then rename, so an interrupted write can
    // never leave the user's settings.json half-written / corrupt (rename is atomic on one filesystem).
    let tmp = format!("{path}.tare-tmp");
    std::fs::write(&tmp, format!("{body}\n")).map_err(|e| format!("write {tmp}: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("finalize {path}: {e}")
    })
}

/// Back up `path` to a timestamped sibling before we mutate it, so a bad merge is always
/// recoverable via [`revert_command`]. Returns the backup path, or `None` when there's no existing file
/// to back up (a fresh connect). Best-effort timestamp — a clock error just yields a `0` suffix.
fn backup_settings(path: &str) -> Result<Option<String>, String> {
    if std::fs::metadata(path).is_err() {
        return Ok(None); // nothing to back up (first connect writes a new file)
    }
    // Never shadow the pristine original: a second `tare connect` runs against an already-connected
    // file, so a fresh backup would capture the connected state. Keep the existing backup.
    if let Some(existing) = pristine_backup(path) {
        return Ok(Some(existing));
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = format!("{path}.tare-bak-{secs}");
    std::fs::copy(path, &backup).map_err(|e| format!("backup {path}: {e}"))?;
    Ok(Some(backup))
}

/// The pristine pre-Tare backup: the oldest `<file>.tare-bak-<unixsecs>` sibling. The
/// first `tare connect` captures the true original; picking the oldest means a later connect — or a
/// stray connected-state backup — can never make `tare revert` restore a Tare-connected file.
fn pristine_backup(path: &str) -> Option<String> {
    let p = std::path::Path::new(path);
    let dir = p
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map(std::path::Path::to_path_buf)?;
    let fname = p.file_name()?.to_str()?;
    let prefix = format!("{fname}.tare-bak-");
    let mut best: Option<(u64, String)> = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(ts) = name
            .strip_prefix(&prefix)
            .and_then(|s| s.parse::<u64>().ok())
        {
            if best.as_ref().is_none_or(|(b, _)| ts < *b) {
                best = Some((ts, entry.path().to_string_lossy().into_owned()));
            }
        }
    }
    best.map(|(_, f)| f)
}

/// `tare connect`: merge the telemetry keys into `path`, then print what changed.
pub fn connect_command(path: &str, endpoint: &str, force: bool) -> Result<(), String> {
    let (updated, mut msgs) = otel_connect(read_settings(path)?, endpoint, force);
    // Also register authoritative lifecycle hooks — additive + reversible.
    let (updated, hook_msgs) = hooks_connect(updated, endpoint);
    msgs.extend(hook_msgs);
    // Back up the untouched file BEFORE mutating it, so the exact original is always
    // recoverable with `tare revert` — reversibility via disconnect isn't the same as the byte-for-byte
    // original (disconnect only removes Tare's keys; a backup restores comments/order/everything).
    let backup = backup_settings(path)?;
    write_settings(path, &updated)?;
    println!("tare connect: {path}");
    for m in &msgs {
        println!("  {m}");
    }
    if let Some(bak) = &backup {
        println!("  backed up original → {bak} (restore with `tare revert`)");
    }
    println!("  (additive: ANTHROPIC_BASE_URL / apiKeyHelper / non-Tare hooks untouched. Restart your agent to apply; undo with `tare disconnect`, or restore the exact original with `tare revert`.)");
    Ok(())
}

/// `tare revert`: restore `path` from the most recent Tare backup (one-click revert) — the
/// byte-for-byte original as it was before the last `tare connect`. Idempotent-ish: with no backup it
/// reports so and changes nothing.
pub fn revert_command(path: &str) -> Result<(), String> {
    match pristine_backup(path) {
        Some(bak) => {
            std::fs::copy(&bak, path).map_err(|e| format!("restore {path} from {bak}: {e}"))?;
            println!("tare revert: restored {path} from {bak}");
            println!("  (restart your agent to apply. The backup file is left in place.)");
            Ok(())
        }
        None => {
            println!("tare revert: no Tare backup found next to {path}; nothing to restore");
            Ok(())
        }
    }
}

/// `tare disconnect`: remove Tare's telemetry keys from `path`.
pub fn disconnect_command(path: &str) -> Result<(), String> {
    if std::fs::metadata(path).is_err() {
        println!("tare disconnect: {path} not found; nothing to do");
        return Ok(());
    }
    let (updated, mut msgs) = otel_disconnect(read_settings(path)?);
    // Also remove Tare's lifecycle hooks — leaves any non-Tare hooks intact.
    let (updated, hook_msgs) = hooks_disconnect(updated);
    msgs.extend(hook_msgs);
    write_settings(path, &updated)?;
    println!("tare disconnect: {path}");
    for m in &msgs {
        println!("  {m}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const EP: &str = "http://127.0.0.1:4318";

    // ---- lifecycle hook registration: temp/in-memory only ----

    #[test]
    fn hooks_connect_registers_all_events_additively() {
        let (out, _) = hooks_connect(json!({}), EP);
        let hooks = out["hooks"].as_object().unwrap();
        for ev in HOOK_EVENTS {
            let groups = hooks[*ev].as_array().unwrap();
            let cmd = groups[0]["hooks"][0]["command"].as_str().unwrap();
            assert!(
                cmd.contains("/__tare/hook"),
                "{ev} hook posts to the intake"
            );
            assert!(
                cmd.ends_with("|| true"),
                "{ev} hook is best-effort, never blocks"
            );
            // SessionEnd is synchronous with a 1.5-second budget; the rest are async.
            let is_async = groups[0]["hooks"][0].get("async").and_then(|a| a.as_bool());
            if *ev == "SessionEnd" {
                assert_eq!(is_async, None, "SessionEnd stays synchronous");
            } else {
                assert_eq!(is_async, Some(true), "{ev} is async");
            }
        }
    }

    #[test]
    fn hooks_connect_preserves_a_users_own_hook() {
        let existing = json!({
            "hooks": {
                "Stop": [ { "hooks": [ { "type": "command", "command": "echo mine" } ] } ]
            }
        });
        let (out, _) = hooks_connect(existing, EP);
        let stop = out["hooks"]["Stop"].as_array().unwrap();
        // The user's hook survives; Tare's is added alongside.
        assert!(stop.iter().any(|g| g["hooks"][0]["command"] == "echo mine"));
        assert!(stop.iter().any(group_is_tare));
        assert_eq!(stop.len(), 2);
    }

    #[test]
    fn hooks_connect_is_idempotent_even_on_a_changed_port() {
        let (once, _) = hooks_connect(json!({}), EP);
        let (twice, _) = hooks_connect(once.clone(), EP);
        assert_eq!(
            once, twice,
            "re-connecting the same endpoint doesn't duplicate"
        );
        // A changed port replaces Tare's group rather than stacking a stale one.
        let (moved, _) = hooks_connect(twice, "http://127.0.0.1:9999");
        let start = moved["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(start.len(), 1, "only one Tare SessionStart group");
        assert!(start[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("9999"));
    }

    #[test]
    fn hooks_disconnect_removes_only_tare_and_cleans_empty_containers() {
        let (connected, _) = hooks_connect(
            json!({ "hooks": { "Stop": [ { "hooks": [ { "type": "command", "command": "echo mine" } ] } ] } }),
            EP,
        );
        let (out, msgs) = hooks_disconnect(connected);
        assert!(msgs.iter().any(|m| m.contains("removed")));
        // The user's Stop hook remains; Tare-only events are gone entirely.
        let stop = out["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0]["hooks"][0]["command"], "echo mine");
        assert!(
            out["hooks"].get("SessionStart").is_none(),
            "Tare-only event removed"
        );
    }

    #[test]
    fn hooks_disconnect_drops_the_whole_block_when_only_tare_was_present() {
        let (connected, _) = hooks_connect(json!({}), EP);
        let (out, _) = hooks_disconnect(connected);
        assert!(out.get("hooks").is_none(), "empty hooks block removed");
    }

    #[test]
    fn hooks_connect_warns_but_still_registers_under_disable_all_hooks() {
        let (out, msgs) = hooks_connect(json!({ "disableAllHooks": true }), EP);
        assert!(msgs.iter().any(|m| m.contains("disableAllHooks")));
        assert!(out["hooks"]["SessionStart"].as_array().unwrap().len() == 1);
        assert_eq!(
            out["disableAllHooks"], true,
            "we never flip the user's flag"
        );
    }

    #[test]
    fn command_round_trip_registers_and_reverses_hooks_on_a_temp_file() {
        let dir = std::env::temp_dir().join(format!("tare-hooks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let p = path.to_string_lossy().to_string();
        // Seed a user hook so we can prove we don't clobber it.
        std::fs::write(
            &path,
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo mine"}]}]}}"#,
        )
        .unwrap();
        connect_command(&p, EP, false).unwrap();
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            after["hooks"]["SessionStart"].as_array().unwrap()[0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("/__tare/hook")
        );
        disconnect_command(&p).unwrap();
        let restored: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // Tare hooks gone; the user's Stop hook intact.
        assert!(restored["hooks"].get("SessionStart").is_none());
        assert_eq!(
            restored["hooks"]["Stop"][0]["hooks"][0]["command"],
            "echo mine"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn connect_into_empty_adds_all_keys() {
        let (out, _) = otel_connect(json!({}), EP, false);
        let env = &out["env"];
        assert_eq!(env["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
        assert_eq!(env["OTEL_EXPORTER_OTLP_ENDPOINT"], EP);
        assert_eq!(env["OTEL_METRICS_EXPORTER"], "otlp");
        // the log-export interval is tightened for a responsive Live view.
        assert_eq!(env["OTEL_LOGS_EXPORT_INTERVAL"], "1000");
    }

    #[test]
    fn connect_warns_on_a_diverting_per_signal_endpoint_but_does_not_clobber() {
        // A pre-existing per-signal LOGS endpoint would divert logs away from Tare.
        let existing = json!({
            "env": { "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT": "https://corp-collector:4318" }
        });
        let (out, msgs) = otel_connect(existing, EP, false);
        // Without --force we DO NOT touch it, but we warn loudly.
        assert_eq!(
            out["env"]["OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"],
            "https://corp-collector:4318"
        );
        assert!(msgs
            .iter()
            .any(|m| m.contains("OVERRIDES") && m.contains("LOGS_ENDPOINT")));
    }

    #[test]
    fn connect_force_clears_a_diverting_per_signal_endpoint() {
        let existing = json!({
            "env": { "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT": "https://corp:4318" }
        });
        let (out, msgs) = otel_connect(existing, EP, true);
        assert!(out["env"]
            .get("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT")
            .is_none());
        assert!(msgs
            .iter()
            .any(|m| m.contains("cleared") && m.contains("METRICS_ENDPOINT")));
    }

    #[test]
    fn connect_leaves_an_already_local_per_signal_endpoint() {
        let existing = json!({
            "env": { "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT": "http://127.0.0.1:4318" }
        });
        let (out, msgs) = otel_connect(existing, EP, false);
        assert_eq!(
            out["env"]["OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"],
            "http://127.0.0.1:4318"
        );
        assert!(msgs.iter().any(|m| m.contains("already local")));
    }

    #[test]
    fn connect_preserves_existing_unrelated_keys() {
        let existing = json!({
            "apiKeyHelper": "~/.local/bin/keys",
            "env": { "ANTHROPIC_BASE_URL": "https://corp/llm-proxy", "API_TIMEOUT_MS": "600000" }
        });
        let (out, _) = otel_connect(existing, EP, false);
        assert_eq!(out["apiKeyHelper"], "~/.local/bin/keys");
        assert_eq!(out["env"]["ANTHROPIC_BASE_URL"], "https://corp/llm-proxy");
        assert_eq!(out["env"]["API_TIMEOUT_MS"], "600000");
        assert_eq!(out["env"]["OTEL_EXPORTER_OTLP_ENDPOINT"], EP);
    }

    #[test]
    fn connect_refuses_to_clobber_existing_endpoint_without_force() {
        let existing =
            json!({ "env": { "OTEL_EXPORTER_OTLP_ENDPOINT": "https://corp-collector:4317" } });
        let (out, msgs) = otel_connect(existing, EP, false);
        assert_eq!(
            out["env"]["OTEL_EXPORTER_OTLP_ENDPOINT"],
            "https://corp-collector:4317"
        );
        assert!(msgs
            .iter()
            .any(|m| m.contains("WARNING") && m.contains("corporate collector")));
        // ...but --force overrides.
        let existing2 =
            json!({ "env": { "OTEL_EXPORTER_OTLP_ENDPOINT": "https://corp-collector:4317" } });
        let (out2, _) = otel_connect(existing2, EP, true);
        assert_eq!(out2["env"]["OTEL_EXPORTER_OTLP_ENDPOINT"], EP);
    }

    #[test]
    fn disconnect_is_an_exact_inverse_of_connect_on_empty() {
        let (connected, _) = otel_connect(json!({}), EP, false);
        let (disconnected, _) = otel_disconnect(connected);
        // env block removed entirely (it held only Tare keys) -> back to {}.
        assert_eq!(disconnected, json!({}));
    }

    #[test]
    fn disconnect_keeps_unrelated_keys_and_a_corporate_endpoint() {
        let connected = json!({
            "apiKeyHelper": "~/.local/bin/keys",
            "env": {
                "ANTHROPIC_BASE_URL": "https://corp/llm-proxy",
                "OTEL_EXPORTER_OTLP_ENDPOINT": "https://corp-collector:4317",
                "CLAUDE_CODE_ENABLE_TELEMETRY": "1"
            }
        });
        let (out, _) = otel_disconnect(connected);
        assert_eq!(out["apiKeyHelper"], "~/.local/bin/keys");
        assert_eq!(out["env"]["ANTHROPIC_BASE_URL"], "https://corp/llm-proxy");
        // corporate (non-loopback) endpoint kept; Tare's CC flag removed.
        assert_eq!(
            out["env"]["OTEL_EXPORTER_OTLP_ENDPOINT"],
            "https://corp-collector:4317"
        );
        assert!(out["env"].get("CLAUDE_CODE_ENABLE_TELEMETRY").is_none());
    }

    #[test]
    fn command_round_trip_through_a_temp_file() {
        let dir = std::env::temp_dir().join(format!("tare-connect-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.local.json");
        let p = path.to_string_lossy().to_string();
        // seed a pre-existing config we must not disturb
        std::fs::write(&path, r#"{"env":{"ANTHROPIC_BASE_URL":"https://corp/x"}}"#).unwrap();
        connect_command(&p, EP, false).unwrap();
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after["env"]["ANTHROPIC_BASE_URL"], "https://corp/x");
        assert_eq!(after["env"]["OTEL_EXPORTER_OTLP_ENDPOINT"], EP);
        disconnect_command(&p).unwrap();
        let restored: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(restored["env"]["ANTHROPIC_BASE_URL"], "https://corp/x");
        assert!(restored["env"].get("OTEL_EXPORTER_OTLP_ENDPOINT").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn connect_backs_up_the_original_and_revert_restores_it_byte_for_byte() {
        // write safety — exercised entirely against a temp file, never a real ~/.claude.
        let dir = std::env::temp_dir().join(format!("tare-revert-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.local.json");
        let p = path.to_string_lossy().to_string();
        let original = "{\n  \"env\": { \"ANTHROPIC_BASE_URL\": \"https://corp/x\" },\n  \"customKey\": 42\n}\n";
        std::fs::write(&path, original).unwrap();

        connect_command(&p, EP, false).unwrap();
        // A timestamped backup holds the byte-for-byte original.
        let bak = pristine_backup(&p).expect("connect should leave a backup");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), original);
        // The live file merged the OTLP keys while preserving unrelated keys.
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after["env"]["OTEL_EXPORTER_OTLP_ENDPOINT"], EP);
        assert_eq!(after["customKey"], 42);
        // The atomic write left no temp file behind.
        assert!(!std::path::Path::new(&format!("{p}.tare-tmp")).exists());

        // One-click revert restores the exact original bytes.
        revert_command(&p).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn double_connect_then_revert_still_restores_the_pristine_original() {
        // A second connect must not shadow the pristine backup with connected state
        // (even in the same wall-clock second, which used to overwrite the same-named backup file).
        let dir = std::env::temp_dir().join(format!("tare-revert-dbl-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.local.json");
        let p = path.to_string_lossy().to_string();
        let original = "{\n  \"env\": { \"ANTHROPIC_BASE_URL\": \"https://corp/x\" },\n  \"customKey\": 42\n}\n";
        std::fs::write(&path, original).unwrap();

        connect_command(&p, EP, false).unwrap();
        connect_command(&p, EP, false).unwrap(); // re-connect against the already-connected file
                                                 // Exactly one backup exists and it still holds the byte-for-byte original.
        let bak = pristine_backup(&p).expect("a backup should exist");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), original);
        revert_command(&p).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn revert_with_no_backup_is_a_no_op() {
        let dir = std::env::temp_dir().join(format!("tare-revert-none-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.local.json");
        let p = path.to_string_lossy().to_string();
        std::fs::write(&path, "{}\n").unwrap();
        // No backup exists → revert changes nothing and doesn't error.
        revert_command(&p).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
