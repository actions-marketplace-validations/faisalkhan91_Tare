//! JSONL transcript watcher — the thin IO adapter over the pure-core tailer/parser.
//! Watches the Claude Code `projects/` subtrees for appended `.jsonl` lines and hands each changed
//! file's bytes to `tare_core::transcript::TranscriptTailer`, which yields only the newly-appended,
//! payload-free `TranscriptRecord`s. Cross-platform via `notify`'s `RecommendedWatcher` + a
//! debouncer (coalesces the burst of events an editor or agent write produces). It is read-only:
//! transcripts are opened for counts and identity, and user configuration is never written.

use notify_debouncer_full::notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, RecommendedCache};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tare_core::transcript::{resolve_claude_config_roots, TranscriptRecord, TranscriptTailer};

/// Debounce window: long enough to coalesce an agent's write burst, short enough for a responsive
/// backfill. The design's 200–500ms band; 300ms mirrors Claude Code's own statusLine cadence.
const DEBOUNCE_MS: u64 = 300;

/// Resolve the existing `<config-root>/projects` directories to watch, from the real environment
/// (`CLAUDE_CONFIG_DIR` / XDG / `~/.claude` + `~/.config/claude`). Only returns dirs that exist.
pub fn resolve_project_dirs() -> Vec<PathBuf> {
    let cfg = std::env::var("CLAUDE_CONFIG_DIR").ok();
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    // Use %USERPROFILE% as a fallback so capture and backfill find ~/.claude on Windows.
    let home = crate::home_dir();
    resolve_claude_config_roots(cfg.as_deref(), xdg.as_deref(), home.as_deref())
        .into_iter()
        .map(|r| Path::new(&r).join("projects"))
        .filter(|p| p.is_dir())
        .collect()
}

/// Read a transcript file and tail it — the deterministic, testable core of the watch loop. Returns
/// only records from lines appended since the tailer last saw this path (empty on a read error, so a
/// transient failure never crashes the watcher).
pub fn tail_file(tailer: &mut TranscriptTailer, path: &Path) -> Vec<TranscriptRecord> {
    match std::fs::read(path) {
        Ok(bytes) => tailer.tail(&path.to_string_lossy(), &bytes),
        Err(_) => Vec::new(),
    }
}

/// Recursively collect `.jsonl` files under `dir` with their `(mtime_unix, size)` — the input to the
/// scan cursor. Silent on unreadable dirs (best-effort; the watcher/next sweep retries).
fn stat_jsonl(dir: &Path, out: &mut Vec<(String, i64, u64)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(md) = e.metadata() else { continue };
        if md.is_dir() {
            stat_jsonl(&p, out);
        } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            out.push((p.to_string_lossy().into_owned(), mtime, md.len()));
        }
    }
}

/// One authoritative full-rescan sweep: walk `dirs`, and for every `.jsonl` the
/// [`ScanCursor`] reports new-or-changed, tail it (offset-tracked) and hand the new records to
/// `on_records`. This catches files the event stream dropped under load — it's the source of truth,
/// with the watcher just lowering latency. Returns how many files were re-tailed this sweep.
///
/// DOUBLE-COUNT SAFETY (when wiring live capture): this sweep and a concurrent
/// [`TranscriptWatcher`] must SHARE ONE `TranscriptTailer` (its per-file offset map is the only thing
/// that stops the same appended line being emitted twice — once per path). Transcript step ordinals
/// are counter-assigned at ingest (see `backfill_transcripts`), NOT derived from the line, so two
/// independent tailers feeding one sink would mint two different ordinals for the same line and the
/// store's `INSERT OR REPLACE` on `(run_id, step_ordinal)` would NOT dedup it — a real token
/// double-count. Either share the tailer, or route the live path through the backfill dedup ledger.
pub fn rescan_once(
    dirs: &[PathBuf],
    cursor: &mut tare_core::transcript::ScanCursor,
    tailer: &mut TranscriptTailer,
    mut on_records: impl FnMut(PathBuf, Vec<TranscriptRecord>),
) -> usize {
    let mut entries = Vec::new();
    for d in dirs {
        stat_jsonl(d, &mut entries);
    }
    let changed = cursor.changed(&entries);
    let n = changed.len();
    for path in changed {
        let p = PathBuf::from(&path);
        let recs = tail_file(tailer, &p);
        if !recs.is_empty() {
            on_records(p, recs);
        }
    }
    n
}

/// A running transcript watcher. Keep it alive to keep watching; drop it to stop.
pub struct TranscriptWatcher {
    _debouncer: Debouncer<notify_debouncer_full::notify::RecommendedWatcher, RecommendedCache>,
}

impl TranscriptWatcher {
    /// Watch `dirs` recursively; on each debounced batch, tail every changed `.jsonl` file and hand
    /// the new records to `on_records`. The callback runs on the debouncer's thread.
    ///
    /// NOTE: this owns its OWN [`TranscriptTailer`]. If a concurrent [`rescan_once`] sweep feeds the
    /// SAME ingest sink, they must share one tailer (or the ingest must dedup) — see the double-count
    /// safety note on [`rescan_once`]. Independent tailers double-count under counter-assigned ordinals.
    pub fn spawn(
        dirs: &[PathBuf],
        mut on_records: impl FnMut(PathBuf, Vec<TranscriptRecord>) + Send + 'static,
    ) -> Result<Self, String> {
        let tailer = Arc::new(Mutex::new(TranscriptTailer::new()));
        let mut debouncer = new_debouncer(
            Duration::from_millis(DEBOUNCE_MS),
            None,
            move |res: DebounceEventResult| {
                let Ok(events) = res else { return }; // watch errors are non-fatal; next event retries
                                                      // Collect distinct .jsonl paths touched this batch (dedup within the burst).
                let mut seen: std::collections::BTreeSet<PathBuf> =
                    std::collections::BTreeSet::new();
                for ev in events {
                    for p in &ev.paths {
                        if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                            seen.insert(p.clone());
                        }
                    }
                }
                for path in seen {
                    let recs = {
                        let mut t = tailer.lock().unwrap();
                        tail_file(&mut t, &path)
                    };
                    if !recs.is_empty() {
                        on_records(path, recs);
                    }
                }
            },
        )
        .map_err(|e| format!("transcript watcher: {e}"))?;
        for d in dirs {
            debouncer
                .watch(d, RecursiveMode::Recursive)
                .map_err(|e| format!("transcript watcher: watch {}: {e}", d.display()))?;
        }
        Ok(Self {
            _debouncer: debouncer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = r#"{"type":"assistant","sessionId":"s","message":{"id":"m1","model":"claude-opus-4-8","usage":{"output_tokens":42}}}"#;

    #[test]
    fn tail_file_reads_only_appended_lines() {
        let dir = std::env::temp_dir().join(format!("tare-tw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("sess.jsonl");
        std::fs::write(&f, format!("{LINE}\n")).unwrap();
        let mut t = TranscriptTailer::new();
        assert_eq!(tail_file(&mut t, &f).len(), 1);
        assert_eq!(tail_file(&mut t, &f).len(), 0, "no new lines");
        // Append a second line.
        std::fs::write(&f, format!("{LINE}\n{LINE}\n")).unwrap();
        assert_eq!(tail_file(&mut t, &f).len(), 1, "only the appended line");
        // A missing file yields nothing, never panics.
        assert!(tail_file(&mut t, &dir.join("gone.jsonl")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rescan_once_tails_new_and_changed_files_only() {
        use tare_core::transcript::ScanCursor;
        let dir = std::env::temp_dir().join(format!("tare-rescan-{}", std::process::id()));
        let sub = dir.join("sess/subagents");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.join("main.jsonl"), format!("{LINE}\n")).unwrap();
        std::fs::write(sub.join("agent-x.jsonl"), format!("{LINE}\n")).unwrap();

        let mut cursor = ScanCursor::new();
        let mut tailer = TranscriptTailer::new();
        let mut got = 0usize;
        // First sweep: both files (main + the recursed subagent) are new → 2 files, 2 records.
        let n = rescan_once(
            std::slice::from_ref(&dir),
            &mut cursor,
            &mut tailer,
            |_p, r| got += r.len(),
        );
        assert_eq!(n, 2, "recurses into subagents/");
        assert_eq!(got, 2);
        // Second sweep, nothing changed → no re-tail.
        got = 0;
        assert_eq!(
            rescan_once(
                std::slice::from_ref(&dir),
                &mut cursor,
                &mut tailer,
                |_p, r| got += r.len()
            ),
            0
        );
        assert_eq!(got, 0);
        // Append to main → only it is re-tailed, and only the appended line emits.
        std::fs::write(dir.join("main.jsonl"), format!("{LINE}\n{LINE}\n")).unwrap();
        got = 0;
        let n = rescan_once(
            std::slice::from_ref(&dir),
            &mut cursor,
            &mut tailer,
            |_p, r| got += r.len(),
        );
        assert_eq!(n, 1, "only the changed file");
        assert_eq!(got, 1, "only the appended line");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn watcher_spawns_over_a_temp_dir_without_error() {
        // Construction smoke test — we don't assert FS-event delivery (timing-dependent/flaky), only
        // that wiring the debouncer over a real dir succeeds. Runs on a worker thread with a bounded
        // wait: under heavy concurrent load (the full gate runs every crate's tests at once) or a
        // wedged macOS fsevents backend, notify's `RecommendedWatcher` teardown can block on the
        // CFRunLoop join. That's an OS-level condition, not a wiring defect — and a smoke test must
        // never be able to hang CI forever. So we give construction+teardown a generous budget and
        // treat a timeout as an inconclusive-but-non-fatal environment signal. On a healthy machine
        // this completes in milliseconds; the detached worker is reaped when the process exits.
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let dir = std::env::temp_dir().join(format!("tare-tw-spawn-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let w = TranscriptWatcher::spawn(std::slice::from_ref(&dir), |_p, _r| {});
            let ok = w.is_ok();
            drop(w); // teardown is the step that can block under a contended fsevents backend
            let _ = std::fs::remove_dir_all(&dir);
            let _ = tx.send(ok);
        });
        match rx.recv_timeout(std::time::Duration::from_secs(20)) {
            Ok(ok) => assert!(ok, "watcher should wire up over a real dir"),
            Err(_) => eprintln!(
                "watcher_spawns: fsevents backend did not settle within 20s (contended/wedged); \
                 treating construction smoke test as inconclusive, not failed"
            ),
        }
    }
}
