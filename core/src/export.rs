//! Lifecycle Log export (Day 11, ADR 0011). Renders a Run's persisted Submissions +
//! Agent decisions (SQLite is the source of truth — ADR 0004) into the two graded
//! artifacts: a Markdown two-part table (a flat Submissions table with Solscan explorer
//! links + Commitment-Progression deltas, then a separate Agent-Decisions section) and a
//! lossless JSONL (one line per Submission, joined with its full Reasoning Trace).
//!
//! The render functions are PURE over `&[SubmissionRow]` / `&[DecisionRow]` so the
//! formatting is unit-testable without a DB or a live Run (the Q-grilled "re-export for
//! free from SQLite" property). `write_lifecycle_log` is the thin DB+filesystem shell.

use crate::storage::{DecisionRow, Store, SubmissionRow};
use anyhow::Result;

/// Solscan links — the explorer the README/Lifecycle Log already uses for tx pages.
fn tx_url(sig: &str) -> String {
    format!("https://solscan.io/tx/{sig}")
}
fn block_url(slot: i64) -> String {
    format!("https://solscan.io/block/{slot}")
}

/// The Payload label for a child run_id under a Run prefix: `run-{ts}-p{k}` -> `p{k}`
/// (ADR 0011). Falls back to the full run_id if it isn't prefixed as expected.
fn payload_label<'a>(run_id: &'a str, run_prefix: &str) -> &'a str {
    run_id
        .strip_prefix(run_prefix)
        .and_then(|rest| rest.strip_prefix('-'))
        .unwrap_or(run_id)
}

/// A short, readable signature label (the full sig lives in the link href + the JSONL).
/// Slices by `char`, not byte, so it can never panic on a non-ASCII value (base58
/// signatures are ASCII, but the helper stays total for any `&str`).
fn short_sig(sig: &str) -> String {
    let chars: Vec<char> = sig.chars().collect();
    if chars.len() <= 12 {
        sig.to_string()
    } else {
        let head: String = chars[..6].iter().collect();
        let tail: String = chars[chars.len() - 6..].iter().collect();
        format!("{head}…{tail}")
    }
}

/// Make a string safe to drop into a Markdown table cell: pipes would split the cell and
/// newlines would break the row, so escape/flatten them. Used for free-text columns.
fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace(['\n', '\r'], " ").trim().to_string()
}

/// First `max` chars of a (sanitized) string, with an ellipsis when truncated. For the
/// Reasoning-Trace / rationale excerpts in the Markdown (the full text is in the JSONL).
fn excerpt(s: &str, max: usize) -> String {
    let flat = cell(s);
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() <= max {
        flat
    } else {
        format!("{}…", chars[..max].iter().collect::<String>())
    }
}

/// ms between two optional stage timestamps (`b - a`), or `None` if either is missing.
fn delta_ms(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(b - a),
        _ => None,
    }
}

/// Human-format a millisecond delta: `<1s` as `{n}ms`, else `{s}s` to one decimal.
fn fmt_delta(d: Option<i64>) -> String {
    match d {
        None => "—".to_string(),
        Some(ms) if ms.abs() < 1_000 => format!("{ms}ms"),
        Some(ms) => format!("{:.1}s", ms as f64 / 1_000.0),
    }
}

/// The Slot cell: a Solscan block link when landed, an em-dash when not (a non-landing
/// faulted Submission, ADR 0011 — real on-the-wire, just never included).
fn slot_cell(landed_slot: Option<i64>) -> String {
    match landed_slot {
        Some(slot) => format!("[{slot}]({})", block_url(slot)),
        None => "—".to_string(),
    }
}

/// The Signature cell: always a Solscan tx link (every Submission records a signature).
fn sig_cell(sig: Option<&str>) -> String {
    match sig {
        Some(s) => format!("[{}]({})", short_sig(s), tx_url(s)),
        None => "—".to_string(),
    }
}

/// (sent, failures, landed) over a Run's Submissions — the end-of-Run assertion inputs
/// (ADR 0011: ≥10 sent, ≥2 failures). Every recorded Submission is now sent on the wire
/// (the faulted attempt-1 included); a Failure is one carrying a classified Failure Class.
pub fn run_counts(subs: &[SubmissionRow]) -> (usize, usize, usize) {
    let sent = subs.len();
    let failures = subs.iter().filter(|s| s.failure_class.is_some()).count();
    let landed = subs.iter().filter(|s| s.landed_slot.is_some()).count();
    (sent, failures, landed)
}

/// Render the Markdown Lifecycle Log: a header with the Run totals, the flat Submissions
/// table, then the Agent-Decisions section. Pure over the fetched rows.
pub fn render_markdown(run_prefix: &str, subs: &[SubmissionRow], decs: &[DecisionRow]) -> String {
    let (sent, failures, landed) = run_counts(subs);
    let mut out = String::new();

    out.push_str(&format!("# Lifecycle Log — {run_prefix}\n\n"));
    out.push_str(&format!(
        "- **Submissions (real bundles sent):** {sent}\n\
         - **Failures (injected, classified, non-landing):** {failures}\n\
         - **Landed:** {landed}\n\n"
    ));

    out.push_str("## Submissions\n\n");
    out.push_str(
        "| # | Payload | Attempt | Slot | Signature | Tip (lamports) | P→C | C→F | Failure |\n\
         |---|---------|---------|------|-----------|----------------|-----|-----|---------|\n",
    );
    for (i, s) in subs.iter().enumerate() {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            i + 1,
            payload_label(&s.run_id, run_prefix),
            s.attempt,
            slot_cell(s.landed_slot),
            sig_cell(s.signature.as_deref()),
            s.tip_lamports,
            fmt_delta(delta_ms(s.processed_at, s.confirmed_at)),
            fmt_delta(delta_ms(s.confirmed_at, s.finalized_at)),
            s.failure_class.as_deref().map(cell).unwrap_or_else(|| "—".to_string()),
        ));
    }

    out.push_str("\n## Agent Decisions\n\n");
    if decs.is_empty() {
        out.push_str("_No Agent decisions recorded for this Run._\n");
        return out;
    }
    out.push_str(
        "| Payload | Failure | Remedy | Confidence | Model | Rationale | Reasoning (excerpt) |\n\
         |---------|---------|--------|------------|-------|-----------|---------------------|\n",
    );
    for d in decs {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            payload_label(&d.run_id, run_prefix),
            cell(&d.failure_class),
            cell(&d.remedy),
            d.confidence.map(|c| format!("{c:.2}")).unwrap_or_else(|| "—".to_string()),
            d.model.as_deref().map(cell).unwrap_or_else(|| "—".to_string()),
            excerpt(&d.rationale, 100),
            d.reasoning_trace.as_deref().map(|t| excerpt(t, 200)).unwrap_or_else(|| "—".to_string()),
        ));
    }
    out
}

/// Render the lossless JSONL: one object per Submission, joined to its decision (by
/// run_id+attempt) with the FULL Reasoning Trace. Explorer links + Commitment deltas are
/// precomputed so the JSONL stands alone. Pure over the fetched rows.
pub fn render_jsonl(run_prefix: &str, subs: &[SubmissionRow], decs: &[DecisionRow]) -> String {
    let mut out = String::new();
    for s in subs {
        let decision = decs
            .iter()
            .find(|d| d.run_id == s.run_id && d.attempt == s.attempt)
            .map(|d| {
                serde_json::json!({
                    "failure_class": d.failure_class,
                    "remedy": d.remedy,
                    "rationale": d.rationale,
                    "confidence": d.confidence,
                    "model": d.model,
                    "reasoning_trace": d.reasoning_trace, // FULL trace (Markdown gets an excerpt)
                    "decided_at": d.decided_at,
                })
            });
        let obj = serde_json::json!({
            "run": run_prefix,
            "run_id": s.run_id,
            "payload": payload_label(&s.run_id, run_prefix),
            "attempt": s.attempt,
            "nonce": s.nonce,
            "bundle_id": s.bundle_id,
            "signature": s.signature,
            "explorer_tx": s.signature.as_deref().map(tx_url),
            "tip_lamports": s.tip_lamports,
            "submitted_at": s.submitted_at,
            "landed": s.landed_slot.is_some(),
            "landed_slot": s.landed_slot,
            "explorer_block": s.landed_slot.map(block_url),
            "processed_at": s.processed_at,
            "confirmed_at": s.confirmed_at,
            "finalized_at": s.finalized_at,
            "processed_to_confirmed_ms": delta_ms(s.processed_at, s.confirmed_at),
            "confirmed_to_finalized_ms": delta_ms(s.confirmed_at, s.finalized_at),
            "failure_class": s.failure_class,
            "decision": decision,
        });
        out.push_str(&serde_json::to_string(&obj).unwrap_or_default());
        out.push('\n');
    }
    out
}

/// Fetch a Run's rows from SQLite and write both Lifecycle-Log artifacts to `dir`,
/// returning `(jsonl_path, md_path)`. The DB+filesystem shell around the pure renderers —
/// shared by the orchestrator's auto-export and the standalone `ARGUS_EXPORT` re-export
/// (ADR 0011), so the same Run re-renders identically without re-submitting.
pub fn write_lifecycle_log(store: &Store, run_prefix: &str, dir: &str) -> Result<(String, String)> {
    let subs = store.fetch_run_submissions(run_prefix)?;
    let decs = store.fetch_run_decisions(run_prefix)?;
    std::fs::create_dir_all(dir)?;
    // The Run prefix is `run-{ts}`; name the artifacts by the bare ts.
    let ts = run_prefix.strip_prefix("run-").unwrap_or(run_prefix);
    let jsonl_path = format!("{dir}/lifecycle-{ts}.jsonl");
    let md_path = format!("{dir}/lifecycle-{ts}.md");
    std::fs::write(&jsonl_path, render_jsonl(run_prefix, &subs, &decs))?;
    std::fs::write(&md_path, render_markdown(run_prefix, &subs, &decs))?;
    Ok((jsonl_path, md_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(run_id: &str, attempt: i64, landed_slot: Option<i64>, failure_class: Option<&str>) -> SubmissionRow {
        SubmissionRow {
            run_id: run_id.to_string(),
            attempt,
            nonce: format!("argus-{run_id}-jito-{attempt}"),
            bundle_id: Some("bundle-xyz".to_string()),
            signature: Some("5f3aQ1cWbVeryLongSignatureValue9b2cZ".to_string()),
            tip_lamports: 1000,
            submitted_at: 1_000,
            landed_slot,
            processed_at: landed_slot.map(|_| 10_000),
            confirmed_at: landed_slot.map(|_| 10_432),
            finalized_at: landed_slot.map(|_| 22_800),
            failure_class: failure_class.map(String::from),
        }
    }

    fn dec(run_id: &str, attempt: i64, trace: Option<&str>) -> DecisionRow {
        DecisionRow {
            run_id: run_id.to_string(),
            attempt,
            failure_class: "expired_blockhash".to_string(),
            remedy: "refresh_blockhash".to_string(),
            rationale: "Blockhash aged past its validity window; refreshing is the canonical recovery.".to_string(),
            confidence: Some(0.98),
            reasoning_trace: trace.map(String::from),
            model: Some("anthropic/claude-4.6-sonnet-20260217".to_string()),
            decided_at: 9_000,
        }
    }

    #[test]
    fn short_sig_is_char_safe_and_truncates() {
        assert_eq!(short_sig("short"), "short", "≤12 chars pass through unchanged");
        // A 14-char multibyte string would panic under byte slicing at index 6 / len-6.
        let s = short_sig("✓✓✓✓✓✓✓✓✓✓✓✓✓✓");
        assert!(s.contains('…') && !s.is_empty(), "multibyte truncates without panic");
        // A normal base58-shaped signature: head…tail.
        assert_eq!(short_sig("ABCDEFGHIJKLMNOPQRSTUV"), "ABCDEF…QRSTUV");
    }

    #[test]
    fn payload_label_strips_the_run_prefix() {
        assert_eq!(payload_label("run-100-p3", "run-100"), "p3");
        assert_eq!(payload_label("run-100-p0", "run-100"), "p0");
        // Not prefixed as expected -> the whole id (defensive, never panics).
        assert_eq!(payload_label("ad-hoc-run", "run-100"), "ad-hoc-run");
    }

    #[test]
    fn markdown_landed_row_links_slot_to_block_and_sig_to_tx() {
        let subs = [sub("run-100-p0", 2, Some(427_368_462), None)];
        let md = render_markdown("run-100", &subs, &[]);
        assert!(md.contains("https://solscan.io/block/427368462"), "slot -> Solscan block link");
        assert!(md.contains("https://solscan.io/tx/5f3aQ1cWbVeryLongSignatureValue9b2cZ"), "sig -> Solscan tx link");
        assert!(md.contains("| p0 |"), "Payload label column");
    }

    #[test]
    fn markdown_non_landed_failure_row_shows_em_dash_slot_and_class() {
        // A sent-but-non-landing faulted Submission (ADR 0011): no slot, but a real sig + class.
        let subs = [sub("run-100-p0", 1, None, Some("expired_blockhash"))];
        let md = render_markdown("run-100", &subs, &[]);
        let row = md.lines().find(|l| l.starts_with("| 1 |")).expect("submission row");
        assert!(row.contains("| — |"), "non-landed slot is an em-dash, not a fabricated block");
        assert!(row.contains("expired_blockhash"), "the Failure Class still shows");
        assert!(row.contains("https://solscan.io/tx/"), "a non-landing Submission was still sent — it has a tx link");
    }

    #[test]
    fn markdown_renders_commitment_deltas_and_dashes_when_absent() {
        let landed = render_markdown("run-100", &[sub("run-100-p0", 2, Some(1), None)], &[]);
        // processed 10_000 -> confirmed 10_432 = 432ms; confirmed -> finalized 22_800 = 12.4s.
        assert!(landed.contains("432ms"), "sub-second delta in ms");
        assert!(landed.contains("12.4s"), "multi-second delta to one decimal");
        // A non-landed row has no stage times -> both deltas are em-dashes.
        let unlanded = render_markdown("run-100", &[sub("run-100-p1", 1, None, Some("bundle_failure"))], &[]);
        let row = unlanded.lines().find(|l| l.starts_with("| 1 |")).unwrap();
        assert_eq!(row.matches("—").count(), 3, "slot + P→C + C→F are all em-dashes");
    }

    #[test]
    fn markdown_decisions_section_carries_remedy_model_and_trace_excerpt() {
        let decs = [dec("run-100-p0", 1, Some("I weighed BumpTip but the error is a stale blockhash, so RefreshBlockhash is correct."))];
        let md = render_markdown("run-100", &[], &decs);
        assert!(md.contains("## Agent Decisions"));
        assert!(md.contains("refresh_blockhash"), "the chosen Remedy");
        assert!(md.contains("0.98"), "confidence to 2dp");
        assert!(md.contains("anthropic/claude-4.6-sonnet-20260217"), "model provenance (ADR 0006)");
        assert!(md.contains("I weighed BumpTip"), "a Reasoning-Trace excerpt is shown");
    }

    #[test]
    fn markdown_excerpt_truncates_long_traces_with_an_ellipsis() {
        let long = "x".repeat(500);
        let decs = [dec("run-100-p0", 1, Some(&long))];
        let md = render_markdown("run-100", &[], &decs);
        assert!(md.contains('…'), "a long trace is truncated with an ellipsis");
        assert!(!md.contains(&"x".repeat(300)), "the full 500-char trace is NOT inlined (excerpt only)");
    }

    #[test]
    fn cell_escapes_pipes_and_flattens_newlines() {
        // A bare pipe would open a new column; a newline would break the row.
        assert_eq!(cell("a | b"), "a \\| b", "a pipe is backslash-escaped");
        assert_eq!(cell("line one\nline two"), "line one line two", "newlines flatten to spaces");
        assert_eq!(cell("  trimmed  "), "trimmed");
    }

    #[test]
    fn markdown_row_stays_single_line_with_a_multiline_trace() {
        let decs = [dec("run-100-p0", 1, Some("first line\nsecond line | tail"))];
        let md = render_markdown("run-100", &[], &decs);
        // The decision must render as exactly ONE table row — the newline didn't break it.
        let rows: Vec<&str> = md.lines().filter(|l| l.contains("refresh_blockhash")).collect();
        assert_eq!(rows.len(), 1, "a multi-line trace stays on one row");
        assert!(rows[0].contains("first line second line"), "newline flattened to a space");
        assert!(rows[0].contains("\\|"), "the in-trace pipe is escaped, not a column break");
    }

    #[test]
    fn jsonl_emits_one_line_per_submission_with_links_and_deltas() {
        let subs = [
            sub("run-100-p0", 1, None, Some("expired_blockhash")),
            sub("run-100-p0", 2, Some(427_368_462), None),
        ];
        let jsonl = render_jsonl("run-100", &subs, &[]);
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), 2, "one JSON object per Submission");

        let landed: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(landed["landed"], true);
        assert_eq!(landed["landed_slot"], 427_368_462);
        assert_eq!(landed["explorer_block"], "https://solscan.io/block/427368462");
        assert_eq!(landed["processed_to_confirmed_ms"], 432);
        assert_eq!(landed["confirmed_to_finalized_ms"], 12_368);
        assert_eq!(landed["payload"], "p0");

        let failed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(failed["landed"], false);
        assert_eq!(failed["landed_slot"], serde_json::Value::Null, "non-landed slot is null");
        assert_eq!(failed["failure_class"], "expired_blockhash");
        assert!(failed["explorer_tx"].as_str().unwrap().starts_with("https://solscan.io/tx/"));
    }

    #[test]
    fn jsonl_joins_the_full_reasoning_trace_to_its_faulted_submission() {
        let full_trace = "A".repeat(900); // the JSONL keeps the FULL trace, unlike the Markdown excerpt
        let subs = [sub("run-100-p0", 1, None, Some("expired_blockhash"))];
        let decs = [dec("run-100-p0", 1, Some(&full_trace))];
        let jsonl = render_jsonl("run-100", &subs, &decs);
        let line: serde_json::Value = serde_json::from_str(jsonl.lines().next().unwrap()).unwrap();
        assert_eq!(line["decision"]["remedy"], "refresh_blockhash");
        assert_eq!(line["decision"]["model"], "anthropic/claude-4.6-sonnet-20260217");
        assert_eq!(line["decision"]["reasoning_trace"].as_str().unwrap().len(), 900, "the FULL trace is preserved in JSONL");
    }

    #[test]
    fn jsonl_submission_without_a_decision_has_null_decision() {
        let subs = [sub("run-100-p5", 1, Some(1), None)]; // a clean Payload — no Failure, no decision
        let jsonl = render_jsonl("run-100", &subs, &[]);
        let line: serde_json::Value = serde_json::from_str(jsonl.lines().next().unwrap()).unwrap();
        assert_eq!(line["decision"], serde_json::Value::Null);
    }

    #[test]
    fn write_lifecycle_log_renders_files_from_a_store() {
        // End-to-end shell: seed a Run in SQLite, write both artifacts, read them back.
        use crate::model::{FailureClass, Remedy};
        use crate::storage::{NewSubmission, Store};
        let store = Store::open(":memory:").unwrap();
        store.init_schema().unwrap();
        // p0: a sent-but-faulted Submission with an Agent decision (the failure evidence).
        store.record_submission(&NewSubmission {
            run_id: "run-7-p0", attempt: 1, nonce: "n1", bundle_id: None,
            signature: "sigFAIL", tip_lamports: 1000, submitted_at: 1,
        }).unwrap();
        store.set_failure_class("run-7-p0", 1, "n1", FailureClass::ExpiredBlockhash).unwrap();
        store.record_decision("run-7-p0", 1, FailureClass::ExpiredBlockhash, Remedy::RefreshBlockhash,
            "rationale", Some(0.91), Some("full trace text"), Some("anthropic/claude-4.6-sonnet-20260217"), 5).unwrap();
        // p1: a clean, landed Submission.
        store.record_submission(&NewSubmission {
            run_id: "run-7-p1", attempt: 1, nonce: "n2", bundle_id: None,
            signature: "sigOK", tip_lamports: 1000, submitted_at: 2,
        }).unwrap();
        store.set_landed_slot("run-7-p1", 1, "n2", 999).unwrap();

        let dir = std::env::temp_dir().join(format!("argus-export-{}", std::process::id()));
        let dir = dir.to_str().unwrap().to_string();
        let (jsonl, md) = write_lifecycle_log(&store, "run-7", &dir).unwrap();

        assert!(md.ends_with("lifecycle-7.md"), "artifact named by the bare ts");
        assert!(jsonl.ends_with("lifecycle-7.jsonl"));
        let md_body = std::fs::read_to_string(&md).unwrap();
        let jsonl_body = std::fs::read_to_string(&jsonl).unwrap();
        assert!(md_body.contains("https://solscan.io/block/999"), "landed slot links to Solscan block");
        assert!(md_body.contains("expired_blockhash"), "the Failure Class is in the table");
        assert!(md_body.contains("anthropic/claude-4.6-sonnet-20260217"), "the decision section carries the model");
        assert_eq!(jsonl_body.lines().count(), 2, "one JSONL line per Submission");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_counts_tallies_sent_failures_and_landed() {
        // The canonical Run shape (ADR 0011): 3 faulted (sent, non-landing) + 2 recoveries + 7 clean.
        let mut subs = vec![
            sub("run-100-p0", 1, None, Some("expired_blockhash")),
            sub("run-100-p0", 2, Some(1), None),
            sub("run-100-p1", 1, None, Some("compute_exceeded")),
            sub("run-100-p1", 2, Some(2), None),
            sub("run-100-p2", 1, None, Some("bundle_failure")), // Abort — no attempt-2
        ];
        for k in 3..10 {
            subs.push(sub(&format!("run-100-p{k}"), 1, Some(100 + k), None));
        }
        let (sent, failures, landed) = run_counts(&subs);
        assert_eq!(sent, 12, "≥10 real bundle Submissions");
        assert_eq!(failures, 3, "≥2 injected, classified, non-landing Failures");
        assert_eq!(landed, 9, "2 recoveries + 7 clean landed");
    }
}
