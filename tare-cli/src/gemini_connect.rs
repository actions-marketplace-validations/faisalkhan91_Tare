//! `tare gemini-connect` / `tare gemini-disconnect`: additively and reversibly wire the Gemini CLI
//! to export its OpenTelemetry to Tare's local receiver, by editing the `telemetry` block of
//! `~/.gemini/settings.json`.
//!
//! Gemini CLI ships telemetry OFF; when enabled it can target `local` (an OTLP endpoint) or `gcp`.
//! This opts it into `local` pointed at Tare. The confirmed settings schema (docs/reference/
//! configuration.md) is a top-level `telemetry` object: `enabled` (bool), `target` (`local`|`gcp`),
//! `otlpEndpoint` (string), `otlpProtocol` (`grpc`|`http`), `logPrompts` (bool). Tare's receiver is
//! OTLP/HTTP, so we set `otlpProtocol: "http"`.
//!
//! Safety contract (mirrors codex_connect.rs + the "never change local AI config" rule):
//!   - Touches ONLY the `telemetry` object; every other setting is preserved verbatim.
//!   - `logPrompts` is forced to `false` (never export raw prompt text) — only set when absent, so a
//!     user who deliberately enabled it isn't overridden silently... actually we force it OFF for
//!     privacy on connect and leave a pre-existing value only when it's already false.
//!   - Refuses to overwrite a telemetry endpoint that isn't a Tare loopback (likely a corporate
//!     collector) unless `--force`, so it can't hijack Gemini telemetry.
//!   - `disconnect` removes only the keys it set, and only when the endpoint is a loopback it
//!     plausibly added — it never deletes a foreign collector or unrelated settings.
//!
//! JSON has no format-preserving editor here (unlike codex's `toml_edit`), so the transforms are
//! `serde_json::Value → Value` and the file rewrite NORMALIZES formatting (2-space, sorted keys)
//! while preserving every setting's value. The transforms are pure + unit-tested; file I/O is thin.

use serde_json::{Map, Value};

/// True if `endpoint` points at a Tare loopback receiver (so disconnect knows it's ours to remove).
fn is_loopback(endpoint: &str) -> bool {
    endpoint.contains("127.0.0.1") || endpoint.contains("localhost") || endpoint.contains("[::1]")
}

/// The keys `connect` owns within the `telemetry` object (what `disconnect` removes).
const OWNED_KEYS: [&str; 5] = [
    "enabled",
    "target",
    "otlpEndpoint",
    "otlpProtocol",
    "logPrompts",
];

fn telemetry_endpoint(root: &Value) -> Option<String> {
    root.get("telemetry")?
        .get("otlpEndpoint")?
        .as_str()
        .map(str::to_string)
}

/// Additively wire Tare's OTLP/HTTP telemetry into a parsed `settings.json`. Returns the updated
/// value + a human-readable log. `force` overwrites a pre-existing non-loopback endpoint.
pub fn gemini_connect(mut root: Value, endpoint: &str, force: bool) -> (Value, Vec<String>) {
    let mut msgs = Vec::new();
    let existing = telemetry_endpoint(&root);
    let already = existing.as_deref() == Some(endpoint);
    // A foreign (non-loopback, not-already-Tare) endpoint is likely a corporate collector — keep it.
    let foreign = existing.as_deref().is_some_and(|e| !is_loopback(e)) && !already;
    if foreign && !force {
        msgs.push(format!(
            "WARNING: telemetry.otlpEndpoint already set ({}); NOT changing it — Gemini telemetry \
             will keep going there, not to Tare. Re-run with --force to override.",
            existing.unwrap_or_default()
        ));
        return (root, msgs);
    }

    if !root.is_object() {
        root = Value::Object(Map::new());
    }
    let obj = root.as_object_mut().expect("root is an object");
    let tel = obj
        .entry("telemetry")
        .or_insert_with(|| Value::Object(Map::new()));
    if !tel.is_object() {
        *tel = Value::Object(Map::new());
    }
    let t = tel.as_object_mut().expect("telemetry is an object");
    if already {
        msgs.push("telemetry.otlpEndpoint already points at Tare".into());
    } else {
        if foreign {
            msgs.push("overwrote existing telemetry endpoint (force)".into());
        }
        t.insert("enabled".into(), Value::Bool(true));
        t.insert("target".into(), Value::String("local".into()));
        t.insert("otlpEndpoint".into(), Value::String(endpoint.into()));
        t.insert("otlpProtocol".into(), Value::String("http".into()));
        msgs.push(format!(
            "set telemetry → local, otlpEndpoint={endpoint}, otlpProtocol=http"
        ));
    }
    // Never export raw prompts: force logPrompts off (respecting an already-false value).
    match t.get("logPrompts").and_then(Value::as_bool) {
        Some(false) => msgs.push("kept logPrompts=false".into()),
        _ => {
            t.insert("logPrompts".into(), Value::Bool(false));
            msgs.push("set logPrompts=false".into());
        }
    }
    (root, msgs)
}

/// Remove Tare's telemetry wiring (only when the endpoint is a loopback we plausibly added),
/// reversing `gemini_connect`. Preserves foreign endpoints + all non-telemetry settings.
pub fn gemini_disconnect(mut root: Value) -> (Value, Vec<String>) {
    let mut msgs = Vec::new();
    let ours = telemetry_endpoint(&root).is_some_and(|e| is_loopback(&e));
    if !ours {
        if telemetry_endpoint(&root).is_some() {
            msgs.push("kept telemetry endpoint (not a Tare loopback)".into());
        } else {
            msgs.push("nothing to disconnect (no Tare telemetry found)".into());
        }
        return (root, msgs);
    }
    if let Some(t) = root.get_mut("telemetry").and_then(Value::as_object_mut) {
        for k in OWNED_KEYS {
            t.remove(k);
        }
        msgs.push("removed Tare telemetry keys".into());
        if t.is_empty() {
            root.as_object_mut().map(|o| o.remove("telemetry"));
            msgs.push("removed now-empty telemetry object".into());
        }
    }
    (root, msgs)
}

fn parse_settings(path: &str) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => {
            serde_json::from_str(&s).map_err(|e| format!("parse {path}: {e}"))
        }
        _ => Ok(Value::Object(Map::new())),
    }
}

fn write_settings(path: &str, root: &Value) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
        }
    }
    let body = serde_json::to_string_pretty(root).map_err(|e| e.to_string())?;
    std::fs::write(path, format!("{body}\n")).map_err(|e| format!("write {path}: {e}"))
}

/// Default Gemini settings path: `~/.gemini/settings.json`.
pub fn default_gemini_config() -> String {
    // Use %USERPROFILE% as a fallback so ~/.gemini resolves on Windows instead of relative to CWD.
    let home = crate::home_dir().unwrap_or_else(|| ".".into());
    format!("{home}/.gemini/settings.json")
}

pub fn gemini_connect_command(path: &str, endpoint: &str, force: bool) -> Result<(), String> {
    let (updated, msgs) = gemini_connect(parse_settings(path)?, endpoint, force);
    write_settings(path, &updated)?;
    println!("tare gemini-connect: {path}");
    for m in &msgs {
        println!("  {m}");
    }
    println!(
        "  (only the telemetry block changed; other settings preserved. JSON formatting normalized. \
         Restart Gemini CLI to apply; undo with `tare gemini-disconnect`.)"
    );
    Ok(())
}

pub fn gemini_disconnect_command(path: &str) -> Result<(), String> {
    if std::fs::metadata(path).is_err() {
        println!("tare gemini-disconnect: {path} not found; nothing to do");
        return Ok(());
    }
    let (updated, msgs) = gemini_disconnect(parse_settings(path)?);
    write_settings(path, &updated)?;
    println!("tare gemini-disconnect: {path}");
    for m in &msgs {
        println!("  {m}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EP: &str = "http://127.0.0.1:4318";

    fn v(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn connect_into_empty_sets_local_http_and_privacy() {
        let (out, _) = gemini_connect(Value::Object(Map::new()), EP, false);
        assert_eq!(out["telemetry"]["enabled"], Value::Bool(true));
        assert_eq!(out["telemetry"]["target"], "local");
        assert_eq!(out["telemetry"]["otlpEndpoint"], EP);
        assert_eq!(out["telemetry"]["otlpProtocol"], "http");
        assert_eq!(out["telemetry"]["logPrompts"], Value::Bool(false));
    }

    #[test]
    fn connect_preserves_unrelated_settings() {
        let (out, _) = gemini_connect(v(r#"{"theme":"dark","model":"gemini-2.5-pro"}"#), EP, false);
        assert_eq!(out["theme"], "dark");
        assert_eq!(out["model"], "gemini-2.5-pro");
        assert_eq!(out["telemetry"]["otlpEndpoint"], EP);
    }

    #[test]
    fn connect_refuses_foreign_endpoint_without_force() {
        let src = r#"{"telemetry":{"enabled":true,"otlpEndpoint":"https://corp:4318","otlpProtocol":"grpc"}}"#;
        let (out, msgs) = gemini_connect(v(src), EP, false);
        assert_eq!(out["telemetry"]["otlpEndpoint"], "https://corp:4318");
        assert!(msgs.iter().any(|m| m.contains("WARNING")));
        // --force overrides + flips to Tare's http endpoint.
        let (out2, _) = gemini_connect(v(src), EP, true);
        assert_eq!(out2["telemetry"]["otlpEndpoint"], EP);
        assert_eq!(out2["telemetry"]["otlpProtocol"], "http");
    }

    #[test]
    fn disconnect_reverses_connect_on_empty() {
        let (connected, _) = gemini_connect(Value::Object(Map::new()), EP, false);
        let (disconnected, _) = gemini_disconnect(connected);
        assert_eq!(disconnected, Value::Object(Map::new()), "back to empty");
    }

    #[test]
    fn disconnect_keeps_foreign_endpoint_and_other_settings() {
        let src =
            r#"{"theme":"dark","telemetry":{"enabled":true,"otlpEndpoint":"https://corp:4318"}}"#;
        let (out, _) = gemini_disconnect(v(src));
        assert_eq!(out["theme"], "dark");
        assert_eq!(
            out["telemetry"]["otlpEndpoint"], "https://corp:4318",
            "foreign kept"
        );
    }

    #[test]
    fn disconnect_keeps_non_owned_telemetry_keys() {
        // A user who also set `outfile` keeps it; only Tare's owned keys go.
        let (connected, _) =
            gemini_connect(v(r#"{"telemetry":{"outfile":"/tmp/t.log"}}"#), EP, false);
        let (out, _) = gemini_disconnect(connected);
        assert_eq!(out["telemetry"]["outfile"], "/tmp/t.log");
        assert!(
            out["telemetry"].get("otlpEndpoint").is_none(),
            "Tare keys removed"
        );
    }

    #[test]
    fn command_round_trip_through_a_temp_file_preserves_user_keys() {
        let dir = std::env::temp_dir().join(format!("tare-gemini-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.json");
        let p = path.to_string_lossy().to_string();
        std::fs::write(&path, r#"{"theme":"dark","model":"gemini-2.5-pro"}"#).unwrap();
        gemini_connect_command(&p, EP, false).unwrap();
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after["theme"], "dark");
        assert_eq!(after["telemetry"]["otlpEndpoint"], EP);
        gemini_disconnect_command(&p).unwrap();
        let restored: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(restored["theme"], "dark", "user key survived");
        assert_eq!(restored["model"], "gemini-2.5-pro");
        assert!(
            restored.get("telemetry").is_none(),
            "Tare telemetry removed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
