//! Incremental JSONL transcript capture: the single ingest path shared by the manual
//! `tare backfill` (one sweep) and the live daemon (a sweep loop). It generalizes the old one-shot
//! backfill into a STATEFUL, repeatable sweep so opening the app / starting the daemon replays only
//! NEW transcript bytes instead of re-reading all of `~/.claude/projects` (the re-scan cost).
//!
//! State carried across sweeps (and persisted, so it survives a restart —):
//! - a `ScanCursor` (path -> mtime,size) so an unchanged file is skipped without opening it,
//! - a `TranscriptTailer` (path -> byte offset) so a grown file is read only from where we left off,
//! - the `backfill_seen` dedup ledger (dedup_key set) as the correctness backstop, so the same turn
//!   is never counted twice — even across the manual/live paths or a rotated file.
//!
//! Sessions already captured on a non-JSONL lane (OTLP/proxy/hook) are skipped whole (OTLP > JSONL at
//! session grain). Counts-only + read-only w.r.t. the transcripts; dates come from each turn's own
//! timestamp (never a wall clock).

use crate::{collect_jsonl, transcript_record_to_step};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use tare_core::transcript::{
    classify_transcript_path, ScanCursor, TranscriptKind, TranscriptTailer,
};
use tare_store::{ScanCursorRow, Store};

/// Owns everything needed to ingest Claude Code transcripts incrementally and repeatedly.
pub struct TranscriptCapture {
    store: Store,
    dirs: Vec<PathBuf>,
    /// dedup ledger (dedup_key set), loaded once and grown as turns are ingested.
    seen: BTreeSet<String>,
    /// per-session ordinal counter, seeded lazily from the store's current max.
    next_ordinal: BTreeMap<String, u32>,
    cursor: ScanCursor,
    tailer: TranscriptTailer,
    /// provenance label recorded on each step (e.g. "jsonl-backfill" vs "jsonl-live").
    provenance: &'static str,
}

/// A file's `(mtime_secs, size)` signature, or `None` if it can't be stat'd.
fn file_sig(p: &Path) -> Option<(i64, u64)> {
    let md = std::fs::metadata(p).ok()?;
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some((mtime, md.len()))
}

/// Read only the bytes a transcript file has grown by since `prev`: seek to the
/// stored line-boundary offset and read `[prev..EOF]`, returning `(start, tail_bytes)` for
/// [`TranscriptTailer::tail_chunk`]. This avoids re-slurping the whole (append-only, possibly
/// multi-MB) session file on every ~3s sweep. If the file shrank below `prev` (truncation/rotation),
/// read from the top (`start = 0`) so the tailer re-reads it. `None` on any IO error.
fn read_tail(path: &Path, prev: usize) -> Option<(usize, Vec<u8>)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len() as usize;
    let start = if len < prev { 0 } else { prev }; // shrink/rotation → re-read from the top
    if start > 0 {
        f.seek(SeekFrom::Start(start as u64)).ok()?;
    }
    // Bound each sweep's read. Without a ceiling, a runaway
    // multi-GB session, or a hostile file dropped into ~/.claude/projects, would be slurped whole and
    // OOM the always-on daemon (the OTLP receiver already caps bodies for exactly this reason). A
    // huge backlog now drains in TAIL_READ_CAP-sized chunks across sweeps: `tail_chunk` consumes only
    // through the last newline in the window and advances the offset, so the next sweep continues.
    let want = len.saturating_sub(start).min(TAIL_READ_CAP);
    let mut buf = Vec::with_capacity(want);
    f.take(TAIL_READ_CAP as u64).read_to_end(&mut buf).ok()?;
    Some((start, buf))
}

/// Max bytes `read_tail` pulls in a single sweep. Generous vs any real turn (JSONL lines are
/// newline-terminated), tiny vs a runaway/hostile file that would otherwise exhaust memory.
const TAIL_READ_CAP: usize = 64 * 1024 * 1024;

impl TranscriptCapture {
    /// Open a dedicated store connection and seed the cursor + tailer from the persisted rows.
    pub fn open(
        db_path: &str,
        dirs: Vec<PathBuf>,
        provenance: &'static str,
    ) -> Result<Self, String> {
        Self::with_store(Store::open(db_path)?, dirs, provenance)
    }

    /// Build from an already-open store (used by tests and callers that own a store).
    pub fn with_store(
        store: Store,
        dirs: Vec<PathBuf>,
        provenance: &'static str,
    ) -> Result<Self, String> {
        let seen = store.backfilled_keys()?;
        let rows = store.load_scan_cursor()?;
        let cursor = ScanCursor::seeded(rows.iter().map(|r| (r.path.clone(), r.mtime, r.size)));
        let tailer =
            TranscriptTailer::seeded(rows.iter().map(|r| (r.path.clone(), r.offset as usize)));
        Ok(Self {
            store,
            dirs,
            seen,
            next_ordinal: BTreeMap::new(),
            cursor,
            tailer,
            provenance,
        })
    }

    /// The next ordinal for `session`, seeded from the store's current max on first use.
    ///
    /// Propagates a `max_step_ordinal` read failure rather than swallowing it: a
    /// transient lock/IO error previously fell through `.ok().flatten().unwrap_or(0)` to start=1,
    /// so a session that ALREADY had persisted turns would renumber from 1 and `INSERT OR REPLACE`
    /// would overwrite its earlier rows — silent history corruption. Surfacing the error lets the
    /// sweep skip this file and retry next tick instead.
    fn next_ordinal_for(&mut self, session: &str) -> Result<u32, String> {
        if let Some(e) = self.next_ordinal.get_mut(session) {
            let cur = *e;
            *e += 1;
            Ok(cur)
        } else {
            let start = self
                .store
                .max_step_ordinal(session)
                .map_err(|e| format!("max_step_ordinal({session}): {e}"))?
                .unwrap_or(0)
                + 1;
            self.next_ordinal.insert(session.to_string(), start + 1);
            Ok(start)
        }
    }

    /// Run one incremental sweep: stat every transcript, tail only the new/changed ones from their
    /// last offset, ingest fresh turns (deduped, ordinal-assigned, OTLP-session-skipped), then persist
    /// the advanced cursor. Returns how many steps were inserted this sweep.
    pub fn sweep(&mut self) -> Result<usize, String> {
        // Read the local-day offset once per sweep (env > tare.toml > 0), not per record.
        let offset = crate::tz_offset_minutes();
        let mut files: Vec<PathBuf> = Vec::new();
        for d in &self.dirs {
            collect_jsonl(d, &mut files);
        }
        // Stat every present file; the (mtime,size) gate is what lets us skip unchanged files.
        let mut entries: Vec<(String, i64, u64)> = Vec::new();
        let mut present: BTreeSet<String> = BTreeSet::new();
        let mut by_path: BTreeMap<String, PathBuf> = BTreeMap::new();
        for p in files {
            let key = p.to_string_lossy().into_owned();
            if let Some((mtime, size)) = file_sig(&p) {
                entries.push((key.clone(), mtime, size));
                present.insert(key.clone());
                by_path.insert(key, p);
            }
        }
        let changed = self.cursor.changed(&entries);
        // Reload each sweep: a session may have just gone live on OTLP, and JSONL must then yield.
        // KNOWN LIMITATION: this whole-session skip is forward-only — it stops FUTURE
        // sweeps of a now-foreign session, but does NOT retract JSONL turns already persisted before
        // the session first appeared on OTLP. So if a user runs the (opt-in) OTLP `connect` lane
        // ALONGSIDE the always-on JSONL lane for the SAME live session, that session's opening turns
        // can be double-counted (JSONL copy + OTLP copy). Per-request cross-lane dedup is the proper
        // fix, but it's only sound if Claude Code's OTLP spans and JSONL turns share request/message
        // ids (so the dedup keys match) — unverified without real dual-lane capture, and a delete-based
        // cleanup risks dropping early turns OTLP never received. Default config (JSONL-only) is
        // unaffected; documented until dual-lane behavior can be validated on real data.
        // The foreign-session skip set is read ONLY inside the `for key in &changed` loop below, so
        // computing it is pure waste when nothing changed. Gating it here removes a full 157K-row
        // `steps` scan from every idle ~3s sweep.
        let foreign = if changed.is_empty() {
            std::collections::BTreeSet::new()
        } else {
            self.store.sessions_with_foreign_steps()?
        };
        let mut inserted = 0usize;
        // Rollback bookkeeping: a file's in-memory progress (tailer offset, dedup
        // `seen` keys, `next_ordinal`, cursor signature) is advanced as we build its batch, but a
        // transient DB write failure (SQLITE_BUSY, disk full, WAL error) must NOT strand those turns.
        // On a per-file write error we undo that file's advancement so the NEXT sweep re-reads it,
        // record the error, and keep going; the error is surfaced after the cursor is persisted for
        // the files that DID succeed. Rolled-back files are excluded from the cursor write below.
        let mut first_err: Option<String> = None;
        let mut rolled_back: std::collections::HashSet<String> = std::collections::HashSet::new();
        for key in &changed {
            let Some(path) = by_path.get(key) else {
                continue;
            };
            let session = match classify_transcript_path(key) {
                Some(TranscriptKind::Session { session_id }) => session_id,
                Some(TranscriptKind::Subagent { parent_session, .. })
                | Some(TranscriptKind::WorkflowSubagent { parent_session, .. }) => parent_session,
                None => continue,
            };
            if foreign.contains(&session) {
                continue; // captured live already — never double-count
            }
            // Seek to our stored offset and read ONLY the appended tail, instead of
            // re-slurping the whole file every sweep. `tail_chunk` consumes complete lines from it.
            let prev = self.tailer.offset(key).unwrap_or(0);
            let Some((start, chunk)) = read_tail(path, prev) else {
                continue;
            };
            let recs = self.tailer.tail_chunk(key, start, &chunk);
            // Snapshot the per-file state we may have to undo on a write failure.
            let ord_before = self.next_ordinal.get(&session).copied();
            let mut staged_seen: Vec<String> = Vec::new();
            let mut file_err: Option<String> = None;
            let mut marks: Vec<(String, String)> = Vec::new();
            // Collect this file's new steps and commit them in ONE transaction below,
            // rather than a fsync-per-turn autocommit insert inside the loop.
            let mut batch = Vec::new();
            for rec in recs {
                let dkey = rec.dedup_key();
                if !self.seen.insert(dkey.clone()) {
                    continue; // already ingested (this run, a prior run, or the other lane)
                }
                staged_seen.push(dkey.clone());
                let Some(ts) = rec.timestamp.as_deref() else {
                    continue;
                };
                // Bucket by the user's local calendar day, matching today_local() and
                // every query. Previously this sliced the raw UTC date (`ts[0..10]`), so for a user
                // with a tz offset a near-midnight turn landed on the wrong day — backfilled and
                // live history diverged and `tare today` could miss a turn. Parse the turn's own ISO
                // instant and shift by the configured offset; the punchcard hour comes from the same
                // shifted instant (was UTC-only). A malformed timestamp falls back to the guarded
                // raw-slice path so a weird stamp degrades rather than skipping — never corrupts the axis.
                let (date, hour) = match tare_core::calendar::parse_iso8601_to_secs(ts) {
                    Some(secs) => {
                        let d = tare_core::calendar::civil_date_for(secs, offset);
                        let h = ((secs + offset * 60).rem_euclid(86_400) / 3_600) as u8;
                        (d, Some(h))
                    }
                    None => {
                        let d: String = ts.chars().take(10).collect();
                        if tare_core::calendar::parse_date(&d).is_none() {
                            continue;
                        }
                        let h = ts
                            .get(11..13)
                            .and_then(|h| h.parse::<u8>().ok())
                            .filter(|h| *h < 24);
                        (d, h)
                    }
                };
                let ord = match self.next_ordinal_for(&session) {
                    Ok(o) => o,
                    Err(e) => {
                        file_err = Some(e);
                        break;
                    }
                };
                let step = transcript_record_to_step(&rec, &session, ord);
                batch.push((step, date, hour));
                marks.push((dkey, session.clone()));
                inserted += 1;
            }
            // One transaction (one fsync) for the whole file's turns, not one per turn.
            // Steps and their dedup marks commit atomically: if this fails, nothing was
            // committed, so the in-memory rollback below fully restores the pre-sweep state and the
            // next sweep re-ingests cleanly — no committed-but-unmarked turns to double-count.
            if file_err.is_none() {
                if let Err(e) = self.store.record_transcript_steps(
                    &batch,
                    &marks,
                    Some(self.provenance),
                    Some("jsonl"),
                ) {
                    file_err = Some(e);
                }
            }
            if let Some(e) = file_err {
                // Undo this file's advancement so the next sweep re-reads exactly these bytes.
                self.tailer.set_offset(key, prev);
                for k in &staged_seen {
                    self.seen.remove(k);
                }
                match ord_before {
                    Some(v) => {
                        self.next_ordinal.insert(session.clone(), v);
                    }
                    None => {
                        self.next_ordinal.remove(&session);
                    }
                }
                self.cursor.invalidate(key); // re-list this file next sweep despite unchanged mtime/size
                rolled_back.insert(key.clone());
                inserted = inserted.saturating_sub(batch.len());
                if first_err.is_none() {
                    first_err = Some(e);
                }
                continue;
            }
        }
        // Persist the advanced cursor and drop rows for files now gone. Only write rows for files
        // that CHANGED this sweep — unchanged files' (mtime,size,offset) are already durable from a
        // prior sweep, so rewriting every row every ~3s is pure write amplification.
        let changed_set: std::collections::HashSet<&str> =
            changed.iter().map(|s| s.as_str()).collect();
        let rows: Vec<ScanCursorRow> = entries
            .iter()
            // Skip rolled-back files: persisting their (mtime,size) with an unadvanced
            // offset would make a RESTART treat them as fully seen and never retry the lost turns.
            .filter(|(p, _, _)| changed_set.contains(p.as_str()) && !rolled_back.contains(p))
            .map(|(p, mtime, size)| ScanCursorRow {
                path: p.clone(),
                mtime: *mtime,
                size: *size,
                offset: self.tailer.offset(p).unwrap_or(0) as u64,
            })
            .collect();
        self.store.save_scan_cursor(&rows)?;
        self.store.prune_scan_cursor(&present)?;
        // Surface a transient write failure to the caller (which logs + retries next tick), but only
        // after the files that DID persist have their cursor advanced.
        if let Some(e) = first_err {
            return Err(e);
        }
        Ok(inserted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A minimal accountable assistant turn; `n` makes the message id + timestamp unique so distinct
    // turns don't collide on the dedup key.
    fn assistant(n: u32) -> String {
        format!(
            "{{\"type\":\"assistant\",\"sessionId\":\"sess-1\",\"uuid\":\"u{n}\",\"requestId\":\"req-{n}\",\"timestamp\":\"2026-07-05T10:0{n}:00Z\",\"message\":{{\"id\":\"msg_{n}\",\"model\":\"claude-opus-4-8\",\"content\":[{{\"type\":\"text\",\"text\":\"hi\"}}],\"usage\":{{\"input_tokens\":1200,\"output_tokens\":300}}}}}}\n"
        )
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tare-cap-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn tmp_db(tag: &str) -> String {
        std::env::temp_dir()
            .join(format!("tare-capdb-{}-{}.db", tag, std::process::id()))
            .to_string_lossy()
            .into_owned()
    }

    fn clean_db(db: &str) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{db}{suffix}"));
        }
    }

    #[test]
    fn sweep_ingests_then_incremental_noop_then_picks_up_appends_and_survives_restart() {
        let dir = tmp_dir("flow");
        let db = tmp_db("flow");
        clean_db(&db);
        let file = dir.join("sess-1.jsonl");
        std::fs::write(&file, format!("{}{}", assistant(1), assistant(2))).unwrap();

        // First sweep: both turns ingested.
        let mut cap = TranscriptCapture::open(&db, vec![dir.clone()], "jsonl-live").unwrap();
        assert_eq!(cap.sweep().unwrap(), 2, "first sweep ingests both turns");
        // Second sweep, file unchanged: nothing re-read.
        assert_eq!(cap.sweep().unwrap(), 0, "unchanged file is skipped");

        // Append a third turn → only it is ingested on the next sweep.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&file)
                .unwrap();
            f.write_all(assistant(3).as_bytes()).unwrap();
        }
        assert_eq!(
            cap.sweep().unwrap(),
            1,
            "only the appended turn is ingested"
        );

        // Restart: a fresh capture over the same db+dir must NOT re-ingest (persisted cursor).
        let mut cap2 = TranscriptCapture::open(&db, vec![dir.clone()], "jsonl-live").unwrap();
        assert_eq!(
            cap2.sweep().unwrap(),
            0,
            "a restart resumes from the persisted cursor — no re-ingest"
        );

        // The store holds exactly 3 turns (ordinals 1..=3) for the one session.
        assert_eq!(
            Store::open(&db)
                .unwrap()
                .max_step_ordinal("sess-1")
                .unwrap(),
            Some(3),
            "3 turns recorded, ordinals counter-assigned without gaps or dupes"
        );

        clean_db(&db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_a_session_already_captured_on_the_otlp_lane() {
        let dir = tmp_dir("foreign");
        let db = tmp_db("foreign");
        clean_db(&db);
        std::fs::write(dir.join("sess-1.jsonl"), assistant(1)).unwrap();

        // Seed a NON-jsonl (otel) step for sess-1 so it counts as captured-live (OTLP > JSONL).
        {
            let store = Store::open(&db).unwrap();
            let rec = tare_core::transcript::parse_transcript(assistant(1).as_bytes()).remove(0);
            let step = transcript_record_to_step(&rec, "sess-1", 1);
            store
                .record_step_with_policy(&step, "2026-07-05", None, None, Some("otel"))
                .unwrap();
        }
        let mut cap = TranscriptCapture::open(&db, vec![dir.clone()], "jsonl-live").unwrap();
        assert_eq!(
            cap.sweep().unwrap(),
            0,
            "a session already on OTLP is not re-accounted from JSONL"
        );
        clean_db(&db);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
