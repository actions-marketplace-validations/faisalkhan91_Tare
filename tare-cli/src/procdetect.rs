//! `sysinfo` adapter for process-detection liveness. Thin, platform-touching layer:
//! it enumerates the OS process table, maps each row into a pure [`tare_core::process::ProcInfo`],
//! and defers all matching to `tare_core::process::detect_agents` (unit-tested there). `sysinfo`
//! covers macOS, Linux, and Windows. The Tauri macOS build cannot enable App Sandbox because it
//! blocks `proc_pidinfo`; Hardened Runtime remains compatible.

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tare_core::process::{detect_agents, AgentProcess, ProcInfo};

/// Snapshot the process table and return the detected agent processes. Best-effort: a refresh
/// failure or an unreadable field yields fewer rows, never a panic.
pub fn collect_agent_processes() -> Vec<AgentProcess> {
    let mut sys = System::new();
    // Explicit refresh kind: a bare refresh returns None for cwd/cmd/exe, so opt each in. Starting
    // from `nothing()` also means per-thread tasks aren't collected (we only need the process).
    let kind = ProcessRefreshKind::nothing()
        .with_cwd(UpdateKind::OnlyIfNotSet)
        .with_exe(UpdateKind::OnlyIfNotSet)
        .with_cmd(UpdateKind::Always);
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);

    let procs: Vec<ProcInfo> = sys
        .processes()
        .values()
        .map(|p| ProcInfo {
            pid: p.pid().as_u32() as i64,
            start_time: p.start_time(),
            name: p.name().to_string_lossy().into_owned(),
            exe_basename: p
                .exe()
                .and_then(|e| e.file_name())
                .map(|n| n.to_string_lossy().into_owned()),
            cmd: p
                .cmd()
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
            cwd: p.cwd().map(|c| c.to_string_lossy().into_owned()),
        })
        .collect();
    detect_agents(&procs)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Smoke test: enumeration runs without panicking on this machine and returns well-formed rows.
    // (We can't assert a Claude Code process is running in CI, so we only check invariants.)
    #[test]
    fn collect_runs_and_returns_wellformed_agents() {
        let agents = collect_agent_processes();
        for a in &agents {
            assert_eq!(a.key, format!("{}:{}", a.pid, a.start_time));
            assert!(matches!(
                a.kind.as_str(),
                "claude" | "legacy-node" | "legacy-bun"
            ));
        }
    }
}
