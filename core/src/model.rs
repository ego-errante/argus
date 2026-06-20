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

/// The catch-all class's wire token. `BundleFailure` is the bucket the four-class baseline
/// drops every error it can't name into — the one the Agent's Diagnosis exists to disambiguate
/// (ADR 0012), so the Lifecycle Log flags it with ⚠. Exported as a const (vs. a bare literal in
/// export.rs) so the marker is tied to the enum: the test below fails if the serde token drifts.
pub const CATCH_ALL_CLASS_TOKEN: &str = "bundle_failure";

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

/// The Agent's recovery-relevant sort of a diagnosed Failure (ADR 0012). Derived from the
/// Diagnosis, NOT the four-class FailureClass baseline — it is the axis the Agent reasons on,
/// covering the buckets a preflight simulation can actually observe.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Triage {
    RecoverableByRefresh,
    RecoverableByModification,
    Permanent,
    Funding,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catch_all_token_matches_the_bundle_failure_wire_string() {
        // The ⚠ blind-marker in export.rs keys on CATCH_ALL_CLASS_TOKEN; it must stay equal to
        // BundleFailure's serde wire string, or a rename would silently stop the marker firing
        // (the ADR 0012 altitude fix — tie the literal to the enum).
        let wire = serde_json::to_value(FailureClass::BundleFailure).unwrap();
        assert_eq!(wire, serde_json::json!(CATCH_ALL_CLASS_TOKEN));
    }
}
