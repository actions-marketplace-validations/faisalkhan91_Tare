//! `tare codex-connect` / `tare codex-disconnect`: additively and reversibly wire the OpenAI Codex
//! CLI to export its OpenTelemetry **logs** to Tare's local receiver, by editing
//! `$CODEX_HOME/config.toml` (default `~/.codex/config.toml`).
//!
//! Codex routes model traffic through whatever gateway the user already has, so the only way Tare
//! sees Codex spend is its out-of-band OTel side-channel. Codex defaults the log/trace exporters to
//! `none` (metrics to `statsig`), so it emits nothing until opted in — this is that opt-in.
//!
//! Safety contract (mirrors `connect.rs` + the "never change local AI config" rule):
//!   - Touches ONLY the `[otel]` table: `log_user_prompt` (forced OFF) and the log `exporter`
//!     subtable. Never touches model/provider/auth keys or anything else in config.toml.
//!   - Format-preserving (`toml_edit`): comments, ordering, and unrelated tables survive a
//!     round-trip untouched.
//!   - Refuses to overwrite an already-configured log `exporter` (likely a corporate collector)
//!     unless `--force`, so it can't silently hijack Codex telemetry.
//!   - `disconnect` removes only the exporter it recognizes as Tare's own (a loopback endpoint),
//!     so it never deletes an exporter Tare didn't add.
//!
//! Codex's exporter enum is externally-tagged (`OtelExporterKind::OtlpHttp { endpoint, protocol,
//! .. }` with `rename_all = "kebab-case"`), so the wire shape is `[otel.exporter.otlp-http]` with
//! `endpoint` + `protocol`, NOT a bare `exporter = "otlp-http"` string. The TOML transforms are
//! pure + unit-tested; file I/O is a thin wrapper.

use toml_edit::{table, value, DocumentMut, Item, Table};

/// The exporter kind Tare configures (the kebab-case external tag of `OtelExporterKind::OtlpHttp`).
const KIND: &str = "otlp-http";

/// True if `endpoint` points at a Tare loopback receiver (so disconnect knows it's ours to remove).
fn is_loopback(endpoint: &str) -> bool {
    endpoint.contains("127.0.0.1") || endpoint.contains("localhost") || endpoint.contains("[::1]")
}

/// Read the current log exporter's endpoint, if `[otel.exporter.otlp-http].endpoint` is set.
fn current_endpoint(doc: &DocumentMut) -> Option<String> {
    doc.get("otel")?
        .get("exporter")?
        .get(KIND)?
        .get("endpoint")?
        .as_str()
        .map(str::to_string)
}

/// Does the `[otel]` table already declare *some* log exporter? (Any `exporter` key — even a kind
/// other than otlp-http, e.g. a corporate `otlp-grpc` — counts, so we never clobber it silently.)
fn has_any_exporter(doc: &DocumentMut) -> bool {
    doc.get("otel")
        .and_then(|o| o.get("exporter"))
        .is_some_and(|e| !e.is_none())
}

/// Ensure `doc["otel"]` is a table and return it (created as an implicit table if absent).
fn ensure_otel(doc: &mut DocumentMut) -> &mut Table {
    if !doc.get("otel").map(Item::is_table).unwrap_or(false) {
        doc["otel"] = table();
    }
    doc["otel"].as_table_mut().expect("otel is a table")
}

/// Additively wire Tare's log exporter into a parsed `config.toml`. Returns the updated document and
/// a human-readable log of what changed (and what was kept). `force` overwrites a pre-existing
/// exporter (a corporate collector); without it, a foreign exporter is preserved with a warning.
pub fn codex_connect(
    mut doc: DocumentMut,
    endpoint: &str,
    force: bool,
) -> (DocumentMut, Vec<String>) {
    let mut msgs = Vec::new();

    // Read current state before mutating (borrow checker; also keeps the decisions in one place).
    let existing = current_endpoint(&doc);
    let already = existing.as_deref() == Some(endpoint);
    // A foreign exporter (anything that isn't already Tare's loopback) is likely a corporate
    // collector — don't hijack it.
    let foreign = has_any_exporter(&doc) && !already;
    if foreign && !force {
        let cur = existing.unwrap_or_else(|| "<non-otlp-http exporter>".into());
        msgs.push(format!(
            "WARNING: [otel].exporter already set ({cur}); NOT changing it — Codex telemetry will \
             keep going there, not to Tare. Re-run with --force to override."
        ));
        return (doc, msgs);
    }

    // Never export raw prompt text. Set it only if absent (respect a user who already chose).
    {
        let otel = ensure_otel(&mut doc);
        if otel.get("log_user_prompt").is_none() {
            otel["log_user_prompt"] = value(false);
            msgs.push("set log_user_prompt=false".into());
        } else {
            msgs.push("kept existing log_user_prompt".into());
        }
    }

    if already {
        msgs.push("[otel.exporter.otlp-http] already points at Tare".into());
        return (doc, msgs);
    }
    if foreign {
        msgs.push("overwrote existing [otel].exporter (force)".into());
    }

    // Write the externally-tagged variant: [otel.exporter.otlp-http] { endpoint, protocol }.
    let mut http = Table::new();
    http["endpoint"] = value(endpoint);
    http["protocol"] = value("json");
    let mut exporter = Table::new();
    exporter.set_implicit(true);
    exporter.insert(KIND, Item::Table(http));
    doc["otel"]["exporter"] = Item::Table(exporter);
    msgs.push(format!(
        "set [otel.exporter.otlp-http] endpoint={endpoint} protocol=json"
    ));

    (doc, msgs)
}

/// Remove Tare's log exporter (only when its endpoint is a loopback we plausibly added), reversing
/// `codex_connect`. Drops `log_user_prompt=false` and an emptied `[otel]` table too.
pub fn codex_disconnect(mut doc: DocumentMut) -> (DocumentMut, Vec<String>) {
    let mut msgs = Vec::new();
    let ours = current_endpoint(&doc).is_some_and(|e| is_loopback(&e));

    if let Some(otel) = doc.get_mut("otel").and_then(Item::as_table_mut) {
        match current_endpoint_in(otel) {
            Some(e) if is_loopback(&e) => {
                // Remove the otlp-http subtable; remove the exporter table if it becomes empty.
                if let Some(exporter) = otel.get_mut("exporter").and_then(Item::as_table_mut) {
                    exporter.remove(KIND);
                    msgs.push("removed [otel.exporter.otlp-http]".into());
                    if exporter.is_empty() {
                        otel.remove("exporter");
                    }
                }
            }
            Some(e) => msgs.push(format!(
                "kept [otel].exporter endpoint={e} (not a Tare loopback)"
            )),
            None => {}
        }
        // Only drop log_user_prompt if it's the default-false we set, and only when we removed our
        // exporter (don't strip a user's explicit choice while a foreign exporter remains).
        if ours && otel.get("log_user_prompt").and_then(Item::as_bool) == Some(false) {
            otel.remove("log_user_prompt");
            msgs.push("removed log_user_prompt".into());
        }
        if otel.is_empty() {
            doc.remove("otel");
            msgs.push("removed now-empty [otel] table".into());
        }
    }
    if msgs.is_empty() {
        msgs.push("nothing to disconnect (no Tare otel exporter found)".into());
    }
    (doc, msgs)
}

/// Endpoint of the otlp-http exporter as read directly from an `[otel]` table.
fn current_endpoint_in(otel: &Table) -> Option<String> {
    otel.get("exporter")?
        .get(KIND)?
        .get("endpoint")?
        .as_str()
        .map(str::to_string)
}

fn parse_doc(path: &str) -> Result<DocumentMut, String> {
    match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => s.parse().map_err(|e| format!("parse {path}: {e}")),
        _ => Ok(DocumentMut::new()),
    }
}

fn write_doc(path: &str, doc: &DocumentMut) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
        }
    }
    std::fs::write(path, doc.to_string()).map_err(|e| format!("write {path}: {e}"))
}

/// Default Codex config path: `$CODEX_HOME/config.toml`, else `~/.codex/config.toml`.
pub fn default_codex_config() -> String {
    if let Ok(home) = std::env::var("CODEX_HOME") {
        return format!("{home}/config.toml");
    }
    // Use %USERPROFILE% as a fallback so ~/.codex resolves on Windows instead of relative to CWD.
    let home = crate::home_dir().unwrap_or_else(|| ".".into());
    format!("{home}/.codex/config.toml")
}

/// `tare codex-connect`: merge the otel log exporter into `path`, then print what changed.
pub fn codex_connect_command(path: &str, endpoint: &str, force: bool) -> Result<(), String> {
    let (updated, msgs) = codex_connect(parse_doc(path)?, endpoint, force);
    write_doc(path, &updated)?;
    println!("tare codex-connect: {path}");
    for m in &msgs {
        println!("  {m}");
    }
    println!(
        "  (additive: model/provider/auth keys untouched. Restart Codex to apply; undo with \
         `tare codex-disconnect`.)"
    );
    Ok(())
}

/// `tare codex-disconnect`: remove Tare's otel log exporter from `path`.
pub fn codex_disconnect_command(path: &str) -> Result<(), String> {
    if std::fs::metadata(path).is_err() {
        println!("tare codex-disconnect: {path} not found; nothing to do");
        return Ok(());
    }
    let (updated, msgs) = codex_disconnect(parse_doc(path)?);
    write_doc(path, &updated)?;
    println!("tare codex-disconnect: {path}");
    for m in &msgs {
        println!("  {m}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EP: &str = "http://127.0.0.1:4318/v1/logs";

    fn doc(s: &str) -> DocumentMut {
        s.parse().unwrap()
    }

    #[test]
    fn connect_into_empty_writes_the_otlp_http_exporter() {
        let (out, _) = codex_connect(DocumentMut::new(), EP, false);
        assert_eq!(out["otel"]["exporter"][KIND]["endpoint"].as_str(), Some(EP));
        assert_eq!(
            out["otel"]["exporter"][KIND]["protocol"].as_str(),
            Some("json")
        );
        assert_eq!(out["otel"]["log_user_prompt"].as_bool(), Some(false));
    }

    #[test]
    fn connect_preserves_unrelated_tables_and_comments() {
        let src = "# my codex config\nmodel = \"gpt-5\"\n\n[tui]\ntheme = \"dark\"\n";
        let (out, _) = codex_connect(doc(src), EP, false);
        let rendered = out.to_string();
        assert!(rendered.contains("# my codex config"), "comment kept");
        assert!(rendered.contains("model = \"gpt-5\""), "model key kept");
        assert!(rendered.contains("theme = \"dark\""), "[tui] kept");
        assert!(rendered.contains("otlp-http"), "exporter added");
    }

    #[test]
    fn connect_refuses_to_clobber_a_foreign_exporter_without_force() {
        let src = "[otel.exporter.otlp-http]\nendpoint = \"https://corp-collector:4318/v1/logs\"\nprotocol = \"json\"\n";
        let (out, msgs) = codex_connect(doc(src), EP, false);
        assert_eq!(
            out["otel"]["exporter"][KIND]["endpoint"].as_str(),
            Some("https://corp-collector:4318/v1/logs"),
            "corporate endpoint preserved"
        );
        assert!(msgs.iter().any(|m| m.contains("WARNING")));
        // ...but --force overrides it with Tare's loopback endpoint.
        let (out2, _) = codex_connect(doc(src), EP, true);
        assert_eq!(
            out2["otel"]["exporter"][KIND]["endpoint"].as_str(),
            Some(EP)
        );
    }

    #[test]
    fn connect_is_idempotent_when_already_pointing_at_tare() {
        let (once, _) = codex_connect(DocumentMut::new(), EP, false);
        let (twice, msgs) = codex_connect(once, EP, false);
        assert!(msgs.iter().any(|m| m.contains("already points at Tare")));
        assert_eq!(
            twice["otel"]["exporter"][KIND]["endpoint"].as_str(),
            Some(EP)
        );
    }

    #[test]
    fn disconnect_is_an_exact_inverse_of_connect_on_empty() {
        let (connected, _) = codex_connect(DocumentMut::new(), EP, false);
        let (disconnected, _) = codex_disconnect(connected);
        assert!(
            disconnected.get("otel").is_none(),
            "[otel] gone -> back to empty"
        );
        assert_eq!(disconnected.to_string(), "");
    }

    #[test]
    fn disconnect_keeps_a_corporate_exporter_and_unrelated_keys() {
        let src = "model = \"gpt-5\"\n\n[otel.exporter.otlp-http]\nendpoint = \"https://corp-collector:4318/v1/logs\"\nprotocol = \"json\"\n";
        let (out, _) = codex_disconnect(doc(src));
        assert_eq!(out["model"].as_str(), Some("gpt-5"), "model kept");
        assert_eq!(
            out["otel"]["exporter"][KIND]["endpoint"].as_str(),
            Some("https://corp-collector:4318/v1/logs"),
            "corporate (non-loopback) exporter kept"
        );
    }

    #[test]
    fn connect_then_disconnect_is_byte_for_byte_reversible() {
        // the strongest safety guarantee — wiring Codex and then unwiring it leaves the
        // user's config.toml EXACTLY as it was, byte for byte (comments, blank lines, ordering).
        for original in [
            "model = \"gpt-5\"\n\n[tui]\ntheme = \"dark\"\n",
            "# leading comment\nmodel = \"gpt-5\"\n",
            "[mcp_servers.fs]\ncommand = \"npx\"\nargs = [\"-y\", \"server\"]\n",
            "model = \"gpt-5\"\n\n[otel]\nenvironment = \"prod\"\n", // pre-existing [otel], no exporter
        ] {
            let (connected, _) = codex_connect(original.parse().unwrap(), EP, false);
            let (restored, _) = codex_disconnect(connected);
            assert_eq!(
                restored.to_string(),
                original,
                "round-trip must restore the original bytes exactly"
            );
        }
    }

    #[test]
    fn command_round_trip_through_a_temp_file_preserves_user_keys() {
        let dir = std::env::temp_dir().join(format!("tare-codex-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.toml");
        let p = path.to_string_lossy().to_string();
        std::fs::write(&path, "model = \"gpt-5\"\n[tui]\ntheme = \"dark\"\n").unwrap();
        codex_connect_command(&p, EP, false).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("model = \"gpt-5\""));
        assert!(after.contains("otlp-http"));
        codex_disconnect_command(&p).unwrap();
        let restored = std::fs::read_to_string(&path).unwrap();
        assert!(
            restored.contains("model = \"gpt-5\""),
            "user key survived round-trip"
        );
        assert!(restored.contains("theme = \"dark\""));
        assert!(!restored.contains("otlp-http"), "Tare exporter removed");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
