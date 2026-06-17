//! Domain types — the shared language from ../../CONTEXT.md, in code.
//! serde rename_all = snake_case keeps the wire strings identical to the TS
//! Agent's zod enums (agent/src/types.ts).

use serde::{Deserialize, Serialize};

/// Solana commitment levels. Distinct from "Landed" (CONTEXT.md).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Commitment {
    Processed,
    Confirmed,
    Finalized,
}

/// The four classified causes of a non-landing Submission.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    ExpiredBlockhash,
    FeeTooLow,
    ComputeExceeded,
    BundleFailure,
}

/// The Agent's Decision Space — exactly one is chosen per Failure (ADR 0003).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Remedy {
    RefreshBlockhash,
    BumpTip,
    RaiseCuLimit,
    HoldAndResubmit,
    Abort,
}

/// One Submission's lifecycle record (one row of the Lifecycle Log).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmissionRecord {
    pub run_id: String,
    pub attempt: u32,
    pub nonce: String, // unique Memo nonce — the join key across the streams
    pub bundle_id: Option<String>,
    pub signature: Option<String>,
    pub tip_lamports: u64,
    pub submitted_at: i64, // epoch ms
    pub landed_slot: Option<u64>,
    pub processed_at: Option<i64>,
    pub confirmed_at: Option<i64>,
    pub finalized_at: Option<i64>,
    pub failure_class: Option<FailureClass>,
}
