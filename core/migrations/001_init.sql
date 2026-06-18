-- Argus lifecycle store — SQLite source of truth (ADR 0004).
-- One row per Submission; the Lifecycle Log (JSONL + Markdown table) exports from here.

CREATE TABLE IF NOT EXISTS submissions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id        TEXT    NOT NULL,
    attempt       INTEGER NOT NULL,
    nonce         TEXT    NOT NULL,            -- unique Memo nonce (join key)
    bundle_id     TEXT,
    signature     TEXT,
    tip_lamports  INTEGER NOT NULL,
    submitted_at  INTEGER NOT NULL,            -- epoch ms
    landed_slot   INTEGER,                     -- Inclusion (from tx-subscription)
    processed_at  INTEGER,                     -- Commitment Progression (from slot-subscription)
    confirmed_at  INTEGER,
    finalized_at  INTEGER,
    failure_class TEXT,                        -- expired_blockhash | fee_too_low | compute_exceeded | bundle_failure
    UNIQUE(run_id, attempt, nonce)
);

-- Agent decisions + Reasoning Trace (CONTEXT.md: Remedy / Reasoning Trace).
CREATE TABLE IF NOT EXISTS decisions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id          TEXT    NOT NULL,
    attempt         INTEGER NOT NULL,
    failure_class   TEXT    NOT NULL,
    remedy          TEXT    NOT NULL,          -- refresh_blockhash | bump_tip | raise_cu_limit | hold_and_resubmit | abort
    rationale       TEXT    NOT NULL,
    confidence      REAL,
    reasoning_trace TEXT,                      -- summarized thinking — the visible-reasoning evidence
    model           TEXT,                      -- serving model: real OpenRouter slug | local | local-fallback (ADR 0006 provenance)
    decided_at      INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_submissions_run ON submissions(run_id);
CREATE INDEX IF NOT EXISTS idx_decisions_run   ON decisions(run_id);
