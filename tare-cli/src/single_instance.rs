//! Single-instance capture lock.
//!
//! Two capture daemons tailing the SAME store concurrently double-count: each assigns transcript
//! step ordinals from its own counter (seeded from the store's max), so the store's
//! `INSERT OR REPLACE (run_id, step_ordinal)` sees two DIFFERENT ordinals for the same turn and
//! keeps both rows instead of collapsing them (see `transcript_watch.rs` safety notes). This guard
//! makes the second capture daemon on a given `--db` refuse to start rather than silently
//! inflate spend.
//!
//! It is a best-effort advisory lock, NOT a security boundary — tare is loopback, single-user,
//! local-first. It is acquired ONLY by the capture daemon (`serve_command`), never by the read
//! paths (CLI reports, the browser read handler), so those keep opening the store freely.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// An acquired capture lock. Held for the daemon's lifetime; the lock file is removed on drop.
pub struct CaptureLock {
    path: PathBuf,
}

impl CaptureLock {
    /// Acquire the capture lock for `db_path`. Returns an error naming the holder if another LIVE
    /// process already holds it. A stale lock left behind by a crashed daemon (its recorded PID is
    /// no longer running) is reclaimed automatically.
    pub fn acquire(db_path: &str) -> Result<Self, String> {
        let path = PathBuf::from(format!("{db_path}.capture.lock"));
        let me = std::process::id();
        // Bounded so a pathological reclaim/create race can't spin forever; `create_new` is atomic,
        // so at most one racer wins each round and a loser then reads the winner's live PID.
        for _ in 0..5 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    // Best-effort PID record; even if the write fails the lock file exists and holds.
                    let _ = write!(f, "{me}");
                    return Ok(CaptureLock { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let holder = fs::read_to_string(&path)
                        .ok()
                        .and_then(|s| s.trim().parse::<u32>().ok());
                    match holder {
                        // A LIVE holder (any PID, including our own — a double-acquire is a bug we
                        // should surface, not paper over) means real contention: refuse.
                        Some(pid) if pid_alive(pid) => {
                            return Err(format!(
                                "another tare capture daemon (pid {pid}) is already writing to {db_path} — \
                                 stop it first, or point --db at a different path (concurrent capture would \
                                 double-count spend)"
                            ));
                        }
                        // Stale lock: the recorded PID is dead (crashed daemon) or unreadable. Reclaim + retry.
                        _ => {
                            let _ = fs::remove_file(&path);
                            continue;
                        }
                    }
                }
                Err(e) => return Err(format!("capture lock {}: {e}", path.display())),
            }
        }
        Err(format!(
            "could not acquire the capture lock at {}.capture.lock (contended)",
            db_path
        ))
    }
}

impl Drop for CaptureLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Is a process with this PID currently running? Cross-platform via sysinfo (same crate the live
/// view already uses). A recycled PID could read as alive — a false-positive refusal, which is the
/// safe direction for a local single-user tool.
fn pid_alive(pid: u32) -> bool {
    let p = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[p]),
        true,
        ProcessRefreshKind::nothing(),
    );
    sys.process(p).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db(tag: &str) -> String {
        std::env::temp_dir()
            .join(format!("tare-lock-{}-{}.db", std::process::id(), tag))
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn acquire_then_second_live_holder_is_refused() {
        let db = tmp_db("live");
        let _ = fs::remove_file(format!("{db}.capture.lock"));
        let lock = CaptureLock::acquire(&db).expect("first acquire");
        // A second acquire while THIS process still holds it (our own live PID) must be refused.
        let err = match CaptureLock::acquire(&db) {
            Ok(_) => panic!("second acquire should have been refused"),
            Err(e) => e,
        };
        assert!(err.contains("already writing"), "got: {err}");
        drop(lock);
        // Once released, the lock file is gone and a fresh acquire succeeds.
        assert!(!std::path::Path::new(&format!("{db}.capture.lock")).exists());
        let again = CaptureLock::acquire(&db).expect("re-acquire after release");
        drop(again);
    }

    #[test]
    fn a_stale_lock_from_a_dead_pid_is_reclaimed() {
        let db = tmp_db("stale");
        let lockfile = format!("{db}.capture.lock");
        // A PID far above any real one is not running → treated as stale and reclaimed.
        fs::write(&lockfile, "4000000000").unwrap();
        let lock = CaptureLock::acquire(&db).expect("stale lock should be reclaimed");
        drop(lock);
        let _ = fs::remove_file(&lockfile);
    }
}
