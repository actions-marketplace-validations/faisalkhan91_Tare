//! Opt-in redacted-transcript store. A SEPARATE SQLite file from the counts DB, so the
//! payload-free ledger is never contaminated by bodies: the counts DB keeps only counts/hashes, and
//! request/response text (already run through `tare_core::redact::scrub_body`) lives here, behind the
//! `max_inspect` privacy profile, and is one-click purgeable. Keyed by `(run_id, step_ordinal)`;
//! re-inserting a step REPLACES it (idempotent re-capture).

use rusqlite::{params, Connection, OptionalExtension};

const TRANSCRIPT_BODY_CAP: usize = 256 * 1024;
const TRANSCRIPT_ID_CAP: usize = 512;

/// One stored transcript row: the redacted request + response text for a step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptRow {
    pub step_ordinal: u32,
    pub req: String,
    pub resp: String,
    pub truncated: Option<bool>,
}

pub struct TranscriptStore {
    conn: Connection,
}

impl TranscriptStore {
    pub fn open(path: &str) -> Result<Self, String> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    format!("create transcript db dir {}: {error}", parent.display())
                })?;
            }
        }
        let conn = Connection::open(path).map_err(|e| format!("open transcript db: {e}"))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("open transcript db: {e}"))?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self, String> {
        // Same concurrency model as the counts store: the daemon writes transcripts while the CLI
        // opens this file to read (`transcript_json`) or purge. WAL keeps readers off the writer's
        // back, and a busy_timeout turns a locked-DB collision into a brief wait instead of a hard
        // `SQLITE_BUSY`. Best-effort; both are no-ops for the in-memory store.
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
        // secure_delete overwrites freed content with zeros so a purge doesn't leave redacted bodies
        // recoverable in the freelist. This opt-in body store's purge MUST
        // actually destroy the bytes, not just unlink rows.
        conn.pragma_update(None, "secure_delete", "ON")
            .map_err(|e| format!("enable transcript secure_delete: {e}"))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS transcript (
                run_id       TEXT NOT NULL,
                step_ordinal INTEGER NOT NULL,
                req          TEXT NOT NULL,
                resp         TEXT NOT NULL,
                truncated    INTEGER,
                PRIMARY KEY (run_id, step_ordinal)
             );",
        )
        .map_err(|e| format!("transcript schema: {e}"))?;
        // Compatibility migration for stores created before truncation evidence was persisted. Old
        // rows remain NULL because the information was discarded at capture and cannot be
        // reconstructed honestly; new captures always write the observed boolean.
        let has_truncated = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(transcript)")
                .map_err(|e| format!("transcript schema inspect: {e}"))?;
            let names = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .map_err(|e| format!("transcript schema inspect: {e}"))?;
            let mut found = false;
            for name in names {
                if name.map_err(|e| format!("transcript schema inspect: {e}"))? == "truncated" {
                    found = true;
                    break;
                }
            }
            found
        };
        if !has_truncated {
            conn.execute("ALTER TABLE transcript ADD COLUMN truncated INTEGER", [])
                .map_err(|e| format!("transcript schema migrate truncation: {e}"))?;
        }
        Ok(TranscriptStore { conn })
    }

    /// Store (or replace) the redacted bodies for a step. Capture callers scrub first so they can
    /// expose truncation evidence immediately; the store repeats the scrub as a final persistence
    /// guard so a future direct caller cannot accidentally land an obvious secret or oversized body.
    pub fn insert(
        &self,
        run_id: &str,
        step_ordinal: u32,
        redacted_req: &str,
        redacted_resp: &str,
        truncated: bool,
    ) -> Result<(), String> {
        if run_id.is_empty() || run_id.len() > TRANSCRIPT_ID_CAP {
            return Err(format!(
                "transcript run id must be 1-{TRANSCRIPT_ID_CAP} bytes"
            ));
        }
        let req = tare_core::redact::scrub_body(redacted_req, TRANSCRIPT_BODY_CAP);
        let resp = tare_core::redact::scrub_body(redacted_resp, TRANSCRIPT_BODY_CAP);
        let truncated = truncated || req.truncated || resp.truncated;
        self.conn
            .execute(
                "INSERT OR REPLACE INTO transcript (run_id, step_ordinal, req, resp, truncated)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![run_id, step_ordinal, req.redacted, resp.redacted, truncated],
            )
            .map(|_| ())
            .map_err(|e| format!("transcript insert: {e}"))
    }

    /// The redacted bodies for one step, if captured.
    pub fn get_by_step(
        &self,
        run_id: &str,
        step_ordinal: u32,
    ) -> Result<Option<(String, String, Option<bool>)>, String> {
        self.conn
            .query_row(
                "SELECT req, resp, truncated FROM transcript WHERE run_id = ?1 AND step_ordinal = ?2",
                params![run_id, step_ordinal],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<bool>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| format!("transcript get_by_step: {e}"))
    }

    /// All captured steps of a run, ordered by step ordinal.
    pub fn get_run(&self, run_id: &str) -> Result<Vec<TranscriptRow>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT step_ordinal, req, resp, truncated FROM transcript WHERE run_id = ?1 ORDER BY step_ordinal",
            )
            .map_err(|e| format!("transcript get_run: {e}"))?;
        let rows = stmt
            .query_map(params![run_id], |r| {
                Ok(TranscriptRow {
                    step_ordinal: r.get::<_, u32>(0)?,
                    req: r.get::<_, String>(1)?,
                    resp: r.get::<_, String>(2)?,
                    truncated: r.get::<_, Option<bool>>(3)?,
                })
            })
            .map_err(|e| format!("transcript get_run: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("transcript get_run: {e}"))
    }

    /// One-click purge: delete every captured transcript. Returns the number of rows removed.
    pub fn purge_all(&self) -> Result<usize, String> {
        let n = self
            .conn
            .execute("DELETE FROM transcript", [])
            .map_err(|e| format!("transcript purge: {e}"))?;
        // Destroy the bytes, don't just unlink rows. secure_delete (init) zeroes
        // freed pages; VACUUM rebuilds the file so no stale pages linger in the freelist; the WAL
        // checkpoint TRUNCATE clears the -wal sidecar. Without this the "purged" redacted bodies stay
        // recoverable via `strings tare.transcripts.db*` — a silent break of the purge promise.
        //
        // Propagate a failure here rather than swallowing it: VACUUM can fail (a lock held
        // past the busy-timeout, an open transaction, no free disk for the rebuild), leaving the
        // redacted bytes in freed pages / the -wal sidecar. Reporting a successful purge when the
        // destruction step failed silently breaks the privacy promise, so surface it to the caller.
        self.conn
            .execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(|e| format!("transcript purge secure-delete: {e}"))?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_and_purge_round_trip() {
        let ts = TranscriptStore::open_in_memory().unwrap();
        ts.insert("run-a", 1, "req-1 [REDACTED:key]", "resp-1", true)
            .unwrap();
        ts.insert("run-a", 2, "req-2", "resp-2", false).unwrap();
        ts.insert("run-b", 1, "b-req", "b-resp", false).unwrap();
        // get_by_step returns the stored (already-redacted) bodies.
        assert_eq!(
            ts.get_by_step("run-a", 1).unwrap(),
            Some((
                "req-1 [REDACTED:key]".to_string(),
                "resp-1".to_string(),
                Some(true)
            ))
        );
        assert_eq!(ts.get_by_step("run-a", 9).unwrap(), None); // absent step
                                                               // get_run is ordered by ordinal and scoped to the run.
        let run_a = ts.get_run("run-a").unwrap();
        assert_eq!(run_a.len(), 2);
        assert_eq!(run_a[0].step_ordinal, 1);
        assert_eq!(run_a[1].req, "req-2");
        assert_eq!(run_a[1].truncated, Some(false));
        // Purge clears everything (opt-out is one click).
        assert_eq!(ts.purge_all().unwrap(), 3);
        assert!(ts.get_run("run-a").unwrap().is_empty());
        assert_eq!(ts.get_by_step("run-b", 1).unwrap(), None);
    }

    #[test]
    fn re_inserting_a_step_replaces_it_idempotently() {
        let ts = TranscriptStore::open_in_memory().unwrap();
        ts.insert("r", 1, "first", "x", false).unwrap();
        ts.insert("r", 1, "second", "y", true).unwrap(); // same key → replace, not duplicate
        let rows = ts.get_run("r").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].req, "second");
        assert_eq!(rows[0].resp, "y");
        assert_eq!(rows[0].truncated, Some(true));
    }

    #[test]
    fn persistence_boundary_rescrubs_secrets_and_caps_bodies() {
        let ts = TranscriptStore::open_in_memory().unwrap();
        let secret = "sk-ant-api03-abcDEF1234567890xyz";
        let oversized = "ordinary prose ".repeat(TRANSCRIPT_BODY_CAP / 15 + 10);
        ts.insert("r", 1, secret, &oversized, false).unwrap();

        let (req, resp, truncated) = ts.get_by_step("r", 1).unwrap().unwrap();
        assert_eq!(req, "[REDACTED:key]");
        assert!(!req.contains(secret));
        assert_eq!(resp.len(), TRANSCRIPT_BODY_CAP);
        assert_eq!(truncated, Some(true));
        assert!(ts.insert("", 2, "req", "resp", false).is_err());
    }

    #[test]
    fn opens_legacy_schema_and_keeps_unknown_truncation_nullable() {
        let path = std::env::temp_dir().join(format!(
            "tare-transcript-legacy-{}-{}.db",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let path_string = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE transcript (
                    run_id TEXT NOT NULL,
                    step_ordinal INTEGER NOT NULL,
                    req TEXT NOT NULL,
                    resp TEXT NOT NULL,
                    PRIMARY KEY (run_id, step_ordinal)
                 );
                 INSERT INTO transcript VALUES ('legacy', 1, 'req', 'resp');",
            )
            .unwrap();
        }
        let ts = TranscriptStore::open(&path_string).unwrap();
        assert_eq!(
            ts.get_by_step("legacy", 1).unwrap(),
            Some(("req".to_string(), "resp".to_string(), None))
        );
        ts.insert("new", 2, "capped", "response", true).unwrap();
        assert_eq!(ts.get_by_step("new", 2).unwrap().unwrap().2, Some(true));
        drop(ts);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{path_string}-wal"));
        let _ = std::fs::remove_file(format!("{path_string}-shm"));
    }
}
