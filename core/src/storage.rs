//! SQLite source of truth for the Lifecycle Log (ADR 0004 / PLAN.md).
//! JSONL + the Markdown table are exported from this (Day 11).

use crate::model::{FailureClass, Remedy};
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::Mutex;
use tracing::warn;

/// Serde snake_case token for a domain enum (matches the column comments + the TS
/// Agent's zod enums). The enums always serialize to a JSON string, so this is total.
fn enum_token<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Surface an UPDATE that matched no row. For the UNCONDITIONAL setters (no `IS NULL`
/// guard) 0 rows means the (run_id, attempt, nonce) key drifted from the submission —
/// the write silently did nothing, which would otherwise look like success.
fn warn_no_rows(rows: usize, op: &str, run_id: &str, attempt: i64, nonce: &str) {
    if rows == 0 {
        warn!(op, run_id, attempt, nonce, "UPDATE matched no submission row — key drift, write dropped");
    }
}

/// Add `column` to `table` if it isn't already present (idempotent). Reads
/// `PRAGMA table_info` and `ALTER`s only when the column is absent. `table`/`column`/
/// `decl` are compile-time constants at every call site — no SQL-injection surface
/// (PRAGMA/ALTER can't bind identifiers as `?` params, hence the interpolation).
fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let present = stmt
        .query_map([], |r| r.get::<_, String>(1))? // column 1 = name
        .collect::<rusqlite::Result<Vec<String>>>()?
        .iter()
        .any(|name| name == column);
    if !present {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"), [])?;
    }
    Ok(())
}

pub struct Store {
    conn: Mutex<Connection>,
}

/// A Submission to persist at submit time (lifecycle stages filled in later).
/// The unit counted by the "≥10 real bundle submissions" requirement.
#[derive(Debug, Clone)]
pub struct NewSubmission<'a> {
    pub run_id: &'a str,
    pub attempt: i64,
    pub nonce: &'a str,
    pub bundle_id: Option<&'a str>,
    pub signature: &'a str,
    pub tip_lamports: u64,
    pub submitted_at: i64, // epoch ms
}

/// A Commitment Progression stage (CONTEXT.md). Maps to the timestamp column stamped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Processed,
    Confirmed,
    Finalized,
}

impl Stage {
    fn column(self) -> &'static str {
        match self {
            Stage::Processed => "processed_at",
            Stage::Confirmed => "confirmed_at",
            Stage::Finalized => "finalized_at",
        }
    }
}

/// A persisted Submission row (for assertions + Day 11 export with deltas).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionRow {
    pub run_id: String,
    pub attempt: i64,
    pub nonce: String,
    pub bundle_id: Option<String>,
    pub signature: Option<String>,
    pub tip_lamports: i64,
    pub submitted_at: i64,
    pub landed_slot: Option<i64>,
    pub processed_at: Option<i64>,
    pub confirmed_at: Option<i64>,
    pub finalized_at: Option<i64>,
    pub failure_class: Option<String>,
}

impl Store {
    pub fn open(path: &str) -> Result<Self> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            // ":memory:" and bare filenames have an empty parent — nothing to create.
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let conn = Connection::open(path)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn init_schema(&self) -> Result<()> {
        let sql = include_str!("../migrations/001_init.sql");
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(sql)?;
        // Idempotent column guard for DBs created before a column existed: `CREATE TABLE
        // IF NOT EXISTS` skips an existing table, so a newly-added column (here
        // `decisions.model`, the ADR 0006 provenance field) would be missing on an old
        // argus.db and every 9-column INSERT would fail `no such column: model`. This adds
        // ONLY the missing column — a one-column guard, not a versioned migration framework
        // (the Q4 grilling decision). Fresh DBs already have it from the CREATE above.
        ensure_column(&conn, "decisions", "model", "TEXT")?;
        Ok(())
    }

    /// Insert a Submission at submit time. Lifecycle stages are stamped later by
    /// the streams. Keyed by (run_id, attempt, nonce) — the schema's UNIQUE tuple.
    pub fn record_submission(&self, s: &NewSubmission) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO submissions (run_id, attempt, nonce, bundle_id, signature, tip_lamports, submitted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                s.run_id,
                s.attempt,
                s.nonce,
                s.bundle_id,
                s.signature,
                s.tip_lamports as i64,
                s.submitted_at
            ],
        )?;
        Ok(())
    }

    /// Set the bundle id once the regional fan-out returns it.
    pub fn set_bundle_id(&self, run_id: &str, attempt: i64, nonce: &str, bundle_id: &str) -> Result<()> {
        let rows = self.conn.lock().unwrap().execute(
            "UPDATE submissions SET bundle_id = ?4 WHERE run_id = ?1 AND attempt = ?2 AND nonce = ?3",
            params![run_id, attempt, nonce, bundle_id],
        )?;
        warn_no_rows(rows, "set_bundle_id", run_id, attempt, nonce);
        Ok(())
    }

    /// Record Inclusion: the landing slot detected on the transaction stream.
    pub fn set_landed_slot(&self, run_id: &str, attempt: i64, nonce: &str, slot: u64) -> Result<()> {
        let rows = self.conn.lock().unwrap().execute(
            "UPDATE submissions SET landed_slot = ?4 WHERE run_id = ?1 AND attempt = ?2 AND nonce = ?3",
            params![run_id, attempt, nonce, slot as i64],
        )?;
        warn_no_rows(rows, "set_landed_slot", run_id, attempt, nonce);
        Ok(())
    }

    /// Stamp a Commitment Progression stage time (epoch ms), FIRST observation wins
    /// (the `IS NULL` guard) — Yellowstone may redeliver the same commitment.
    pub fn mark_stage(
        &self,
        run_id: &str,
        attempt: i64,
        nonce: &str,
        stage: Stage,
        at_ms: i64,
    ) -> Result<()> {
        let col = stage.column();
        let sql = format!(
            "UPDATE submissions SET {col} = ?4 \
             WHERE run_id = ?1 AND attempt = ?2 AND nonce = ?3 AND {col} IS NULL"
        );
        self.conn
            .lock()
            .unwrap()
            .execute(&sql, params![run_id, attempt, nonce, at_ms])?;
        Ok(())
    }

    pub fn fetch_submission(
        &self,
        run_id: &str,
        attempt: i64,
        nonce: &str,
    ) -> Result<Option<SubmissionRow>> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT run_id, attempt, nonce, bundle_id, signature, tip_lamports, submitted_at, \
                 landed_slot, processed_at, confirmed_at, finalized_at, failure_class \
                 FROM submissions WHERE run_id = ?1 AND attempt = ?2 AND nonce = ?3",
                params![run_id, attempt, nonce],
                |r| {
                    Ok(SubmissionRow {
                        run_id: r.get(0)?,
                        attempt: r.get(1)?,
                        nonce: r.get(2)?,
                        bundle_id: r.get(3)?,
                        signature: r.get(4)?,
                        tip_lamports: r.get(5)?,
                        submitted_at: r.get(6)?,
                        landed_slot: r.get(7)?,
                        processed_at: r.get(8)?,
                        confirmed_at: r.get(9)?,
                        finalized_at: r.get(10)?,
                        failure_class: r.get(11)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Persist the classified Failure Class for a Submission (the Lifecycle Log
    /// column). Stores the serde snake_case token (`expired_blockhash`, ...).
    pub fn set_failure_class(
        &self,
        run_id: &str,
        attempt: i64,
        nonce: &str,
        class: FailureClass,
    ) -> Result<()> {
        let rows = self.conn.lock().unwrap().execute(
            "UPDATE submissions SET failure_class = ?4 WHERE run_id = ?1 AND attempt = ?2 AND nonce = ?3",
            params![run_id, attempt, nonce, enum_token(class)],
        )?;
        warn_no_rows(rows, "set_failure_class", run_id, attempt, nonce);
        Ok(())
    }

    /// Record a Remedy decision + its Reasoning Trace into the `decisions` table.
    /// Day 7-8 writes the local default policy's decision here; Day 9-10 swaps the
    /// source to the Agent without changing this sink.
    #[allow(clippy::too_many_arguments)]
    pub fn record_decision(
        &self,
        run_id: &str,
        attempt: i64,
        class: FailureClass,
        remedy: Remedy,
        rationale: &str,
        confidence: Option<f64>,
        reasoning_trace: Option<&str>,
        model: Option<&str>,
        decided_at: i64,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO decisions (run_id, attempt, failure_class, remedy, rationale, confidence, reasoning_trace, model, decided_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                run_id,
                attempt,
                enum_token(class),
                enum_token(remedy),
                rationale,
                confidence,
                reasoning_trace,
                model,
                decided_at
            ],
        )?;
        Ok(())
    }

    // TODO (Day 11): export_jsonl / export_markdown_table (with explorer links + deltas).
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let s = Store::open(":memory:").unwrap();
        s.init_schema().unwrap();
        s
    }

    fn sample() -> NewSubmission<'static> {
        NewSubmission {
            run_id: "run-1",
            attempt: 1,
            nonce: "argus-1-jito-1",
            bundle_id: None,
            signature: "sig1",
            tip_lamports: 5000,
            submitted_at: 1_000,
        }
    }

    #[test]
    fn records_and_fetches_a_submission() {
        let s = store();
        s.record_submission(&sample()).unwrap();
        let row = s.fetch_submission("run-1", 1, "argus-1-jito-1").unwrap().expect("row exists");
        assert_eq!(row.signature.as_deref(), Some("sig1"));
        assert_eq!(row.tip_lamports, 5000);
        assert_eq!(row.submitted_at, 1_000);
        assert!(row.landed_slot.is_none(), "no commitment data yet");
    }

    #[test]
    fn records_inclusion_then_commitment_progression() {
        let s = store();
        s.record_submission(&sample()).unwrap();
        s.set_landed_slot("run-1", 1, "argus-1-jito-1", 427).unwrap();
        s.mark_stage("run-1", 1, "argus-1-jito-1", Stage::Processed, 1_100).unwrap();
        s.mark_stage("run-1", 1, "argus-1-jito-1", Stage::Confirmed, 1_150).unwrap();
        s.mark_stage("run-1", 1, "argus-1-jito-1", Stage::Finalized, 13_000).unwrap();

        let row = s.fetch_submission("run-1", 1, "argus-1-jito-1").unwrap().unwrap();
        assert_eq!(row.landed_slot, Some(427));
        assert_eq!(row.processed_at, Some(1_100));
        assert_eq!(row.confirmed_at, Some(1_150));
        assert_eq!(row.finalized_at, Some(13_000));
        // Deltas the Lifecycle Log reports are derived from these timestamps:
        assert_eq!(row.confirmed_at.unwrap() - row.processed_at.unwrap(), 50);
    }

    #[test]
    fn mark_stage_first_observation_wins() {
        let s = store();
        s.record_submission(&sample()).unwrap();
        s.mark_stage("run-1", 1, "argus-1-jito-1", Stage::Processed, 1_100).unwrap();
        // A redelivered Processed must not overwrite the first stamp.
        s.mark_stage("run-1", 1, "argus-1-jito-1", Stage::Processed, 9_999).unwrap();
        let row = s.fetch_submission("run-1", 1, "argus-1-jito-1").unwrap().unwrap();
        assert_eq!(row.processed_at, Some(1_100));
    }

    #[test]
    fn set_bundle_id_updates_the_row() {
        let s = store();
        s.record_submission(&sample()).unwrap();
        s.set_bundle_id("run-1", 1, "argus-1-jito-1", "bundle-abc").unwrap();
        let row = s.fetch_submission("run-1", 1, "argus-1-jito-1").unwrap().unwrap();
        assert_eq!(row.bundle_id.as_deref(), Some("bundle-abc"));
    }

    #[test]
    fn set_failure_class_persists_the_snake_case_token() {
        let s = store();
        s.record_submission(&sample()).unwrap();
        s.set_failure_class("run-1", 1, "argus-1-jito-1", FailureClass::ComputeExceeded)
            .unwrap();
        let row = s.fetch_submission("run-1", 1, "argus-1-jito-1").unwrap().unwrap();
        assert_eq!(row.failure_class.as_deref(), Some("compute_exceeded"));
    }

    #[test]
    fn ensure_column_upgrades_an_old_db_and_is_idempotent() {
        // Simulate a pre-`model` argus.db: a decisions table created before the column
        // existed (8 columns, no `model`) — exactly what CREATE TABLE IF NOT EXISTS skips.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE decisions (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id          TEXT    NOT NULL,
                attempt         INTEGER NOT NULL,
                failure_class   TEXT    NOT NULL,
                remedy          TEXT    NOT NULL,
                rationale       TEXT    NOT NULL,
                confidence      REAL,
                reasoning_trace TEXT,
                decided_at      INTEGER NOT NULL
            );",
        )
        .unwrap();

        // The guard adds the missing column...
        ensure_column(&conn, "decisions", "model", "TEXT").unwrap();
        // ...so the full 9-column INSERT (the one record_decision uses) now succeeds.
        conn.execute(
            "INSERT INTO decisions (run_id, attempt, failure_class, remedy, rationale, confidence, reasoning_trace, model, decided_at)
             VALUES ('run-old', 1, 'expired_blockhash', 'refresh_blockhash', 'x', 1.0, NULL, 'local', 7)",
            [],
        )
        .unwrap();
        // Running it again is a no-op, not an error (idempotent — safe on every startup).
        ensure_column(&conn, "decisions", "model", "TEXT").unwrap();

        let model: Option<String> = conn
            .query_row("SELECT model FROM decisions WHERE run_id = 'run-old'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(model.as_deref(), Some("local"), "the upgraded column round-trips");
    }

    #[test]
    fn record_decision_round_trips() {
        let s = store();
        s.record_decision(
            "run-1",
            1,
            FailureClass::ExpiredBlockhash,
            Remedy::RefreshBlockhash,
            "local default policy",
            Some(1.0),
            None,
            Some("anthropic/claude-sonnet-4.6"),
            42,
        )
        .unwrap();
        // The test lives in the storage module, so it can read the private conn.
        let conn = s.conn.lock().unwrap();
        let (class, remedy, model, decided): (String, String, Option<String>, i64) = conn
            .query_row(
                "SELECT failure_class, remedy, model, decided_at FROM decisions WHERE run_id = ?1 AND attempt = ?2",
                params!["run-1", 1],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(class, "expired_blockhash");
        assert_eq!(remedy, "refresh_blockhash");
        assert_eq!(model.as_deref(), Some("anthropic/claude-sonnet-4.6"), "the serving model persists (ADR 0006)");
        assert_eq!(decided, 42);
    }
}
