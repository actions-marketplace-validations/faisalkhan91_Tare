//! Process-detection liveness: the 3rd, hook-independent liveness signal. Enumerating
//! the OS process table tells us an agent is *running* (and *where*, via cwd) BEFORE any hook fires
//! or any OTLP event arrives — and it survives a crash-missed `SessionEnd`. This module holds the
//! PURE classifier over an injected process snapshot; the actual `sysinfo` enumeration is a thin
//! adapter in the CLI (kept out of the pure, clock-free core).
//!
//! Matching (verified against the current Claude Code binary): the CLI ships as a Bun-compiled
//! standalone Mach-O whose basename is `claude` — NOT a `node` process — so the primary match is the
//! executable basename `claude`, with `node`/`bun` argv references kept as a legacy fallback.
//! Identity is `(pid, start_time)`, never pid alone (pids recycle) and never the process name alone
//! (Linux truncates it to 15 chars). No payload, no elevation: cwd + argv are same-UID readable.

use serde::{Deserialize, Serialize};

/// A process-table row, as the CLI's `sysinfo` adapter hands it to the classifier. Injectable so the
/// matching logic is unit-tested without touching the real OS.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcInfo {
    pub pid: i64,
    /// Process start time (Unix seconds); half of the stable identity, so a recycled pid is distinct.
    pub start_time: u64,
    /// OS process name. May be 15-char-truncated on Linux — never matched on alone.
    pub name: String,
    /// Basename of the executable path (the authoritative match key when available).
    #[serde(default)]
    pub exe_basename: Option<String>,
    /// Full argv.
    #[serde(default)]
    pub cmd: Vec<String>,
    /// Working directory (where the agent is running); same-UID readable, no elevation/TCC.
    #[serde(default)]
    pub cwd: Option<String>,
}

/// A detected running agent process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProcess {
    pub pid: i64,
    pub start_time: u64,
    /// Stable identity `pid:start_time` (survives pid recycling; the key for liveness fusion).
    pub key: String,
    /// How it matched: `claude` (the Bun standalone) | `legacy-node` | `legacy-bun`.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// `--resume <uuid>` from argv, cross-referencing a JSONL/hook session id when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_session: Option<String>,
}

/// The trailing path component of a `/`-separated path (or the whole string if there's no `/`).
fn basename(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

/// Extract the `--resume <uuid>` argument (accepts `--resume X` and `--resume=X`).
fn resume_arg(cmd: &[String]) -> Option<String> {
    for (i, a) in cmd.iter().enumerate() {
        if let Some(v) = a.strip_prefix("--resume=") {
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
        if a == "--resume" {
            if let Some(next) = cmd.get(i + 1) {
                if !next.is_empty() && !next.starts_with('-') {
                    return Some(next.clone());
                }
            }
        }
    }
    None
}

/// Classify one process, returning its agent `kind` if it is a Claude Code run. Primary match is the
/// executable basename `claude`; the legacy fallback is a `node`/`bun` interpreter whose argv points
/// at a `claude` entrypoint (older installs). Conservative — a bare `claude` substring never matches.
fn classify_kind(p: &ProcInfo) -> Option<&'static str> {
    let exe = p.exe_basename.as_deref().unwrap_or(&p.name);
    if exe == "claude" {
        return Some("claude");
    }
    // Legacy: a node/bun interpreter launching a `claude` entrypoint.
    let interp = basename(exe);
    let is_node = interp == "node" || interp.starts_with("node");
    let is_bun = interp == "bun";
    if is_node || is_bun {
        let launches_claude = p.cmd.iter().any(
            |a| matches!(basename(a), "claude" | "claude.js" | "cli.js" if a.contains("claude")),
        );
        if launches_claude {
            return Some(if is_bun { "legacy-bun" } else { "legacy-node" });
        }
    }
    None
}

/// Detect running agent processes from a process-table snapshot. Deterministic: input order is
/// preserved, identity is `(pid, start_time)`. No dedup (a process table lists each pid once).
pub fn detect_agents(procs: &[ProcInfo]) -> Vec<AgentProcess> {
    procs
        .iter()
        .filter_map(|p| {
            let kind = classify_kind(p)?;
            Some(AgentProcess {
                pid: p.pid,
                start_time: p.start_time,
                key: format!("{}:{}", p.pid, p.start_time),
                kind: kind.to_string(),
                cwd: p.cwd.clone(),
                resume_session: resume_arg(&p.cmd),
            })
        })
        .collect()
}

/// Local-process liveness confirmation gate. The live-sessions view derives "running now"
/// from event recency; when a captured session ALSO carries an OS pid we can optionally confirm it
/// with a read-only `kill -0`-style probe. But a pid is only meaningful on the host that minted it —
/// a homelab/remote session's pid says nothing about a local process (and could collide with an
/// unrelated one). Return the pid to confirm ONLY when it's safe to treat as LOCAL:
/// - a positive pid is present, AND
/// - the capture host is either unknown (the local-first default — the receiver runs on the user's
///   own machine and most agents don't emit `host.name`) OR explicitly equals `local_host`.
///
/// An explicitly-different host disqualifies it (`None`), so a remote pid is never probed against the
/// local process table. Pure; no clock, no syscalls (the aliveness probe itself lives at the edge).
pub fn confirmable_pid(
    pid: Option<i64>,
    captured_host: Option<&str>,
    local_host: &str,
) -> Option<i64> {
    let pid = pid?;
    if pid <= 0 {
        return None;
    }
    match captured_host {
        Some(h) if h != local_host => None, // captured on another machine → not our pid
        _ => Some(pid),                     // same host, or host unknown (local-first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: i64, name: &str, exe: Option<&str>, cmd: &[&str], cwd: Option<&str>) -> ProcInfo {
        ProcInfo {
            pid,
            start_time: 1_700_000_000,
            name: name.into(),
            exe_basename: exe.map(String::from),
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            cwd: cwd.map(String::from),
        }
    }

    #[test]
    fn confirmable_pid_confirms_local_and_unknown_refuses_remote() {
        // Same host → confirm.
        assert_eq!(
            confirmable_pid(Some(4242), Some("mac-studio"), "mac-studio"),
            Some(4242)
        );
        // Unknown host (no host.name attr — the common case) → confirm, local-first.
        assert_eq!(confirmable_pid(Some(4242), None, "mac-studio"), Some(4242));
        // Explicitly different host → refuse (a homelab session's pid isn't our process).
        assert_eq!(
            confirmable_pid(Some(4242), Some("gpu-box"), "mac-studio"),
            None
        );
        // Absent / non-positive pid → nothing to confirm.
        assert_eq!(confirmable_pid(None, None, "mac-studio"), None);
        assert_eq!(confirmable_pid(Some(0), None, "mac-studio"), None);
        assert_eq!(
            confirmable_pid(Some(-1), Some("mac-studio"), "mac-studio"),
            None
        );
    }

    #[test]
    fn matches_the_bun_standalone_claude_by_exe_basename() {
        let procs = vec![
            proc(
                101,
                "claude",
                Some("claude"),
                &["claude", "--resume", "sess-abc"],
                Some("/home/u/proj"),
            ),
            proc(
                202,
                "bash",
                Some("bash"),
                &["bash", "-lc", "echo claude"],
                None,
            ), // not a match
        ];
        let found = detect_agents(&procs);
        assert_eq!(found.len(), 1, "only the real claude process matches");
        let a = &found[0];
        assert_eq!(a.kind, "claude");
        assert_eq!(
            a.key, "101:1700000000",
            "identity is pid:start_time, not pid alone"
        );
        assert_eq!(a.cwd.as_deref(), Some("/home/u/proj"));
        assert_eq!(a.resume_session.as_deref(), Some("sess-abc"));
    }

    #[test]
    fn legacy_node_and_bun_entrypoints_match_as_legacy() {
        let node = proc(
            301,
            "node",
            Some("node"),
            &["node", "/usr/lib/claude/claude.js"],
            None,
        );
        let bun = proc(
            302,
            "bun",
            Some("bun"),
            &["bun", "/opt/claude/cli.js"],
            None,
        );
        // A node process NOT launching claude must not match.
        let other = proc(303, "node", Some("node"), &["node", "server.js"], None);
        let found = detect_agents(&[node, bun, other]);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].kind, "legacy-node");
        assert_eq!(found[1].kind, "legacy-bun");
    }

    #[test]
    fn resume_accepts_equals_form_and_ignores_a_flag_value() {
        let p = proc(
            1,
            "claude",
            Some("claude"),
            &["claude", "--resume=uuid-9"],
            None,
        );
        assert_eq!(
            detect_agents(&[p])[0].resume_session.as_deref(),
            Some("uuid-9")
        );
        // A dangling --resume followed by another flag yields no session.
        let p2 = proc(
            2,
            "claude",
            Some("claude"),
            &["claude", "--resume", "--verbose"],
            None,
        );
        assert_eq!(detect_agents(&[p2])[0].resume_session, None);
    }

    #[test]
    fn falls_back_to_name_when_exe_is_unavailable() {
        // sysinfo may not resolve exe under some sandboxes → match on name as a fallback.
        let p = proc(5, "claude", None, &["claude"], None);
        assert_eq!(detect_agents(&[p]).len(), 1);
    }

    #[test]
    fn empty_table_finds_nothing() {
        assert!(detect_agents(&[]).is_empty());
    }
}
