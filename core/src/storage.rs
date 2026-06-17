//! SQLite source of truth for the Lifecycle Log (ADR 0004 / PLAN.md).
//! JSONL + the Markdown table are exported from this (Day 11).

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::Mutex;

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
        self.conn.lock().unwrap().execute_batch(sql)?;
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
        self.conn.lock().unwrap().execute(
            "UPDATE submissions SET bundle_id = ?4 WHERE run_id = ?1 AND attempt = ?2 AND nonce = ?3",
            params![run_id, attempt, nonce, bundle_id],
        )?;
        Ok(())
    }

    /// Record Inclusion: the landing slot detected on the transaction stream.
    pub fn set_landed_slot(&self, run_id: &str, attempt: i64, nonce: &str, slot: u64) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE submissions SET landed_slot = ?4 WHERE run_id = ?1 AND attempt = ?2 AND nonce = ?3",
            params![run_id, attempt, nonce, slot as i64],
        )?;
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

    // TODO (Day 9-10): record_decision (Remedy + Reasoning Trace).
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
}
