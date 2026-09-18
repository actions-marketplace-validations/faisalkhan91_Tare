//! Per-user background-service support for `tare serve`, keeping the loopback OTLP receiver and
//! live-session tracking active while the desktop or browser UI is closed.
//!
//! Installation is opt-in and never system-wide:
//!   - **macOS**: a launchd **LaunchAgent** in `~/Library/LaunchAgents`, managed with the modern
//!     `launchctl bootstrap gui/<uid>`, `bootout`, and `kickstart -k` verbs.
//!   - **Linux**: a **systemd `--user` unit** (`Type=exec`, `Restart=always`, `WantedBy=default.target`)
//!     under `~/.config/systemd/user`, plus `loginctl enable-linger` so it survives logout.
//!   - **Windows**: a **Task Scheduler** per-user task with an at-logon trigger + restart-on-failure.
//!
//! The service runs entirely on the user's own machine, binds loopback only, and installs only Tare's
//! own agent — it never touches the user's `~/.claude` / `~/.codex` config. `uninstall` removes exactly
//! what `install` wrote. The unit/plist/XML GENERATORS below are pure (no I/O) so they are golden-tested
//! on any OS; only installation side effects are platform-gated.

use std::path::{Path, PathBuf};

/// launchd label for Tare's capture agent (also the plist filename stem). Also the systemd unit stem
/// and Windows task name share the "tare-serve"/"TareCaptureService" identity below.
pub const LABEL: &str = "com.tare.serve";
/// systemd `--user` unit filename.
pub const SYSTEMD_UNIT: &str = "tare-serve.service";
/// Windows Task Scheduler task name.
pub const WINDOWS_TASK: &str = "TareCaptureService";

// ---------------------------------------------------------------------------------------------------
// Pure generators — no I/O, golden-testable on any platform.
// ---------------------------------------------------------------------------------------------------

/// macOS LaunchAgent plist: run `<exe> serve --db <db> --port <port> --otlp-port <otlp>` at login and
/// keep it alive, logging to `<log>`.
pub fn launch_agent_plist(exe: &str, db: &str, port: u16, otlp_port: u16, log: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>serve</string>
    <string>--db</string><string>{db}</string>
    <string>--port</string><string>{port}</string>
    <string>--otlp-port</string><string>{otlp_port}</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Background</string>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#
    )
}

/// Linux systemd `--user` unit. `Type=exec` (ready once exec succeeds), `Restart=always` with a short
/// backoff, and `WantedBy=default.target` so `systemctl --user enable` wires it to login. Paths are
/// quoted so spaces in the exe/db path survive systemd's own tokenizer.
pub fn systemd_user_unit(exe: &str, db: &str, port: u16, otlp_port: u16) -> String {
    format!(
        r#"[Unit]
Description=Tare always-on capture service (loopback OTLP receiver + live-session tracking)
Documentation=https://github.com/tare
After=default.target

[Service]
Type=exec
ExecStart="{exe}" serve --db "{db}" --port {port} --otlp-port {otlp_port}
Restart=always
RestartSec=2

[Install]
WantedBy=default.target
"#
    )
}

/// Windows Task Scheduler task XML: an at-logon trigger (per-user) with restart-on-failure. `Command`
/// and the `--db` argument are quoted so paths with spaces (e.g. `C:\Users\Jane Doe\...`) are safe.
/// `ExecutionTimeLimit=PT0S` disables the kill-after-timeout so a long-running daemon is never reaped.
pub fn windows_task_xml(exe: &str, db: &str, port: u16, otlp_port: u16) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Tare always-on capture service (loopback OTLP receiver + live-session tracking)</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <Enabled>true</Enabled>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>999</Count>
    </RestartOnFailure>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>"{exe}"</Command>
      <Arguments>serve --db "{db}" --port {port} --otlp-port {otlp_port}</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

// ---------------------------------------------------------------------------------------------------
// Path helpers.
// ---------------------------------------------------------------------------------------------------

fn plist_path(dir: &Path) -> PathBuf {
    dir.join(format!("{LABEL}.plist"))
}

/// Default LaunchAgents dir (`~/Library/LaunchAgents`).
pub fn default_agent_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Library/LaunchAgents")
}

/// Pure resolver (testable without mutating process-global env): `$XDG_CONFIG_HOME/systemd/user`,
/// else `<home>/.config/systemd/user`.
fn systemd_unit_dir_from(xdg: Option<&str>, home: &str) -> PathBuf {
    match xdg {
        Some(x) if !x.is_empty() => PathBuf::from(x).join("systemd/user"),
        _ => PathBuf::from(home).join(".config/systemd/user"),
    }
}

/// systemd `--user` unit dir (`$XDG_CONFIG_HOME/systemd/user`, default `~/.config/systemd/user`).
pub fn systemd_unit_dir() -> PathBuf {
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    systemd_unit_dir_from(xdg.as_deref(), &home)
}

// ---------------------------------------------------------------------------------------------------
// macOS launchctl (modern verbs). argv builders are pure so the exact commands are testable.
// ---------------------------------------------------------------------------------------------------

/// The launchd GUI domain target for the current user, e.g. `gui/501`.
fn macos_domain(uid: &str) -> String {
    format!("gui/{uid}")
}
/// The launchd service target, e.g. `gui/501/com.tare.serve`.
fn macos_service_target(uid: &str) -> String {
    format!("gui/{uid}/{LABEL}")
}
/// Numeric UID via `id -u` (launchd domains are keyed by uid, not `$USER`). Falls back to "0".
fn current_uid() -> String {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "0".into())
}

// ---------------------------------------------------------------------------------------------------
// macOS install / uninstall / status. Signatures kept stable for the CLI caller.
// ---------------------------------------------------------------------------------------------------

/// Write the LaunchAgent plist into `dir`. When `load` is true, (re)bootstrap it into the user's GUI
/// domain with the modern verbs (skipped in tests so no resident process is started). Returns a human
/// summary.
pub fn install(
    dir: &Path,
    exe: &str,
    db: &str,
    port: u16,
    otlp_port: u16,
    log: &str,
    load: bool,
) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = plist_path(dir);
    std::fs::write(&path, launch_agent_plist(exe, db, port, otlp_port, log))
        .map_err(|e| format!("write plist: {e}"))?;
    if load {
        let uid = current_uid();
        let plist = path.to_string_lossy().to_string();
        // `bootout` first so a re-install reloads cleanly; ignore the error when nothing is loaded.
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &macos_service_target(&uid)])
            .status();
        std::process::Command::new("launchctl")
            .args(["bootstrap", &macos_domain(&uid), &plist])
            .status()
            .map_err(|e| format!("launchctl bootstrap: {e}"))?;
        // Force-(re)start immediately rather than waiting for the next login.
        let _ = std::process::Command::new("launchctl")
            .args(["kickstart", "-k", &macos_service_target(&uid)])
            .status();
    }
    Ok(format!(
        "installed {} (capture serve on :{port}, OTLP :{otlp_port}){}",
        path.display(),
        if load { " and bootstrapped" } else { "" }
    ))
}

/// Remove the plist (and `launchctl bootout` when `unload`). Idempotent.
pub fn uninstall(dir: &Path, unload: bool) -> Result<String, String> {
    let path = plist_path(dir);
    if unload && path.exists() {
        let uid = current_uid();
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &macos_service_target(&uid)])
            .status();
    }
    let existed = path.exists();
    if existed {
        std::fs::remove_file(&path).map_err(|e| format!("remove plist: {e}"))?;
    }
    Ok(if existed {
        format!("removed {}", path.display())
    } else {
        "no Tare service installed".to_string()
    })
}

/// Whether the plist is present in `dir`.
pub fn is_installed(dir: &Path) -> bool {
    plist_path(dir).exists()
}

// ---------------------------------------------------------------------------------------------------
// Linux systemd --user install / uninstall (written; validated on Linux later).
// ---------------------------------------------------------------------------------------------------

/// Write the systemd `--user` unit into `dir`. When `enable` is true, enable-linger + reload + enable
/// --now (skipped in tests). Returns a human summary.
pub fn install_systemd(
    dir: &Path,
    exe: &str,
    db: &str,
    port: u16,
    otlp_port: u16,
    enable: bool,
) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = dir.join(SYSTEMD_UNIT);
    std::fs::write(&path, systemd_user_unit(exe, db, port, otlp_port))
        .map_err(|e| format!("write unit: {e}"))?;
    if enable {
        // Linger keeps the user manager (and thus our service) alive across logout — REQUIRED for a
        // genuinely always-on daemon; without it the unit dies with the login session.
        if let Ok(user) = std::env::var("USER") {
            let _ = std::process::Command::new("loginctl")
                .args(["enable-linger", &user])
                .status();
        }
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        std::process::Command::new("systemctl")
            .args(["--user", "enable", "--now", SYSTEMD_UNIT])
            .status()
            .map_err(|e| format!("systemctl --user enable: {e}"))?;
    }
    Ok(format!(
        "installed {} (capture serve on :{port}, OTLP :{otlp_port}){}",
        path.display(),
        if enable {
            " and enabled (linger on)"
        } else {
            ""
        }
    ))
}

/// Disable + remove the systemd `--user` unit. Idempotent.
pub fn uninstall_systemd(dir: &Path, disable: bool) -> Result<String, String> {
    let path = dir.join(SYSTEMD_UNIT);
    if disable && path.exists() {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", SYSTEMD_UNIT])
            .status();
    }
    let existed = path.exists();
    if existed {
        std::fs::remove_file(&path).map_err(|e| format!("remove unit: {e}"))?;
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
    }
    Ok(if existed {
        format!("removed {}", path.display())
    } else {
        "no Tare service installed".to_string()
    })
}

// ---------------------------------------------------------------------------------------------------
// Windows Task Scheduler install / uninstall (written; validated on Windows later).
// ---------------------------------------------------------------------------------------------------

/// Write the task XML into `dir` and, when `register`, `schtasks /create /xml` it (skipped in tests).
pub fn install_windows(
    dir: &Path,
    exe: &str,
    db: &str,
    port: u16,
    otlp_port: u16,
    register: bool,
) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = dir.join(format!("{WINDOWS_TASK}.xml"));
    std::fs::write(&path, windows_task_xml(exe, db, port, otlp_port))
        .map_err(|e| format!("write task xml: {e}"))?;
    if register {
        std::process::Command::new("schtasks")
            .args([
                "/create",
                "/tn",
                WINDOWS_TASK,
                "/xml",
                &path.to_string_lossy(),
                "/f",
            ])
            .status()
            .map_err(|e| format!("schtasks /create: {e}"))?;
    }
    Ok(format!(
        "installed {} (capture serve on :{port}, OTLP :{otlp_port}){}",
        path.display(),
        if register { " and registered" } else { "" }
    ))
}

/// Delete the scheduled task + remove the XML. Idempotent.
pub fn uninstall_windows(dir: &Path, unregister: bool) -> Result<String, String> {
    let path = dir.join(format!("{WINDOWS_TASK}.xml"));
    if unregister {
        let _ = std::process::Command::new("schtasks")
            .args(["/delete", "/tn", WINDOWS_TASK, "/f"])
            .status();
    }
    let existed = path.exists();
    if existed {
        std::fs::remove_file(&path).map_err(|e| format!("remove task xml: {e}"))?;
    }
    Ok(if existed {
        format!("removed {}", path.display())
    } else {
        "no Tare service installed".to_string()
    })
}

/// Windows task-XML dir (`%APPDATA%\Tare`, falling back to `%USERPROFILE%`/`~/.tare`).
pub fn windows_task_dir() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        if !appdata.is_empty() {
            return PathBuf::from(appdata).join("Tare");
        }
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".tare")
}

// ---------------------------------------------------------------------------------------------------
// OS-dispatched entry points — the CLI calls these; they pick the right platform mechanism + location.
// ---------------------------------------------------------------------------------------------------

/// Install the always-on service for the current OS: launchd LaunchAgent (macOS), systemd `--user`
/// unit (Linux), or Task Scheduler task (Windows). `activate` bootstraps/enables/registers it (vs just
/// writing the definition for inspection). `log` is used by the macOS plist only.
pub fn install_current(
    exe: &str,
    db: &str,
    port: u16,
    otlp_port: u16,
    log: &str,
    activate: bool,
) -> Result<String, String> {
    match std::env::consts::OS {
        "macos" => install(
            &default_agent_dir(),
            exe,
            db,
            port,
            otlp_port,
            log,
            activate,
        ),
        "linux" => install_systemd(&systemd_unit_dir(), exe, db, port, otlp_port, activate),
        "windows" => install_windows(&windows_task_dir(), exe, db, port, otlp_port, activate),
        other => Err(format!("service install: unsupported OS {other:?}")),
    }
}

/// Uninstall the current-OS service. `deactivate` also boots-out/disables/unregisters it.
pub fn uninstall_current(deactivate: bool) -> Result<String, String> {
    match std::env::consts::OS {
        "macos" => uninstall(&default_agent_dir(), deactivate),
        "linux" => uninstall_systemd(&systemd_unit_dir(), deactivate),
        "windows" => uninstall_windows(&windows_task_dir(), deactivate),
        other => Err(format!("service uninstall: unsupported OS {other:?}")),
    }
}

/// Whether the current-OS service definition is present.
pub fn is_installed_current() -> bool {
    match std::env::consts::OS {
        "macos" => is_installed(&default_agent_dir()),
        "linux" => systemd_unit_dir().join(SYSTEMD_UNIT).exists(),
        "windows" => windows_task_dir()
            .join(format!("{WINDOWS_TASK}.xml"))
            .exists(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_runs_serve_with_ports_and_keepalive() {
        let p = launch_agent_plist(
            "/usr/local/bin/tare",
            "/home/u/.tare/tare.db",
            8788,
            4318,
            "/tmp/t.log",
        );
        assert!(p.contains("<string>com.tare.serve</string>"));
        assert!(p.contains("<string>serve</string>"));
        assert!(p.contains("<string>--otlp-port</string><string>4318</string>"));
        assert!(p.contains("<string>--port</string><string>8788</string>"));
        assert!(p.contains("<key>RunAtLoad</key><true/>"));
        assert!(p.contains("<key>KeepAlive</key><true/>"));
        assert!(p.contains("/usr/local/bin/tare"));
    }

    #[test]
    fn install_writes_and_uninstall_removes_without_launchctl() {
        let dir = std::env::temp_dir().join(format!("tare-svc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!is_installed(&dir));
        // load=false / unload=false so the test never touches the real launchd.
        let msg = install(&dir, "/bin/tare", "/db", 8788, 4318, "/tmp/l.log", false).unwrap();
        assert!(msg.contains("installed"));
        assert!(is_installed(&dir));
        // Re-install is idempotent (overwrites).
        install(&dir, "/bin/tare", "/db", 9999, 4319, "/tmp/l.log", false).unwrap();
        assert!(plist_path(&dir).exists());
        let u = uninstall(&dir, false).unwrap();
        assert!(u.contains("removed"));
        assert!(!is_installed(&dir));
        // Uninstalling again is a no-op, not an error.
        assert!(uninstall(&dir, false).unwrap().contains("no Tare service"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn macos_launchctl_targets_use_modern_gui_domain() {
        // The migration off deprecated load/unload: verbs must address the gui/<uid> domain + service.
        assert_eq!(macos_domain("501"), "gui/501");
        assert_eq!(macos_service_target("501"), "gui/501/com.tare.serve");
    }

    #[test]
    fn systemd_unit_has_restart_type_and_wantedby() {
        let u = systemd_user_unit("/opt/tare/tare", "/home/u/.tare/tare.db", 8788, 4318);
        assert!(u.contains("Type=exec"));
        assert!(u.contains("Restart=always"));
        assert!(u.contains("WantedBy=default.target"));
        // ExecStart quotes the exe + db path (spaces safe) and carries the ports.
        assert!(u.contains(r#"ExecStart="/opt/tare/tare" serve --db "/home/u/.tare/tare.db" --port 8788 --otlp-port 4318"#));
    }

    #[test]
    fn systemd_unit_dir_honors_xdg() {
        // Pure resolver — no process-global env mutation (which would race parallel tests).
        assert_eq!(
            systemd_unit_dir_from(Some("/tmp/xdgtest"), "/home/u"),
            PathBuf::from("/tmp/xdgtest/systemd/user")
        );
        assert_eq!(
            systemd_unit_dir_from(None, "/home/u"),
            PathBuf::from("/home/u/.config/systemd/user")
        );
        assert_eq!(
            systemd_unit_dir_from(Some(""), "/home/u"),
            PathBuf::from("/home/u/.config/systemd/user")
        );
    }

    #[test]
    fn windows_task_is_at_logon_with_restart_and_quoted_paths() {
        let x = windows_task_xml(
            r"C:\Program Files\Tare\tare.exe",
            r"C:\Users\Jane Doe\tare.db",
            8788,
            4318,
        );
        assert!(x.contains("<LogonTrigger>"));
        assert!(x.contains("<RestartOnFailure>"));
        // Quoted command + db path so spaces (Program Files, Jane Doe) survive.
        assert!(x.contains(r#"<Command>"C:\Program Files\Tare\tare.exe"</Command>"#));
        assert!(x.contains(r#"--db "C:\Users\Jane Doe\tare.db""#));
        // No execution time limit → a long-running daemon is never reaped.
        assert!(x.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
    }

    #[test]
    fn systemd_install_writes_and_uninstall_removes_without_systemctl() {
        let dir = std::env::temp_dir().join(format!("tare-sysd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // enable=false so the test never touches the real systemd/loginctl.
        let msg = install_systemd(&dir, "/bin/tare", "/db", 8788, 4318, false).unwrap();
        assert!(msg.contains("installed"));
        assert!(dir.join(SYSTEMD_UNIT).exists());
        let u = uninstall_systemd(&dir, false).unwrap();
        assert!(u.contains("removed"));
        assert!(!dir.join(SYSTEMD_UNIT).exists());
        assert!(uninstall_systemd(&dir, false)
            .unwrap()
            .contains("no Tare service"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn windows_install_writes_and_uninstall_removes_without_schtasks() {
        let dir = std::env::temp_dir().join(format!("tare-win-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // register=false so the test never shells out to schtasks.
        let msg =
            install_windows(&dir, r"C:\t\tare.exe", r"C:\t\tare.db", 8788, 4318, false).unwrap();
        assert!(msg.contains("installed"));
        assert!(dir.join(format!("{WINDOWS_TASK}.xml")).exists());
        let u = uninstall_windows(&dir, false).unwrap();
        assert!(u.contains("removed"));
        assert!(!dir.join(format!("{WINDOWS_TASK}.xml")).exists());
        assert!(uninstall_windows(&dir, false)
            .unwrap()
            .contains("no Tare service"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
