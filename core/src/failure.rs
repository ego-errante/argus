//! Failure injection, classification, and the local remedy policy (Day 7-8, ADR 0010).
//!
//! Three deterministic faults are injectable (`ARGUS_INJECT`); each is classified
//! from a preflight `simulateTransaction` (the only deterministic reason-source for
//! an all-or-nothing Jito bundle — see ADR 0010). A local default policy stands in
//! for the AI Agent (Day 9-10) behind the `Policy` seam, mapping each Failure Class
//! to a Remedy; `apply_remedy` is the one place remedy SEMANTICS live (the Agent
//! picks WHICH remedy; the Core executes it).

use crate::agent_client::{AgentClient, Decision, FailureContext};
use crate::model::{FailureClass, Remedy};
use crate::rpc::SimResult;
use anyhow::Result;
use solana_sdk::instruction::Instruction;
use solana_sdk::pubkey::Pubkey;

/// How far back to age the injected blockhash. Blockhashes are valid ~150 slots, so
/// 200 is reliably expired while still being a genuinely real, recent cluster hash.
pub const AGED_BLOCKHASH_SLOTS: u64 = 200;
/// CU limit for the Compute-Exceeded injection — far below any real payload's need.
pub const INJECT_CU_LIMIT: u32 = 1;
/// A self-transfer far larger than the low-balance keypair holds — a deterministic
/// "insufficient lamports" in simulation, for the Bundle-Failure injection.
pub const OVERSPEND_LAMPORTS: u64 = 1_000_000_000_000_000;

/// Remedy tuning (kept as module consts — operationally fixed, not env knobs).
const RAISE_CU_FLOOR: u32 = 200_000; // comfortably above our payload's need
const MAX_CU_LIMIT: u32 = 1_400_000; // Solana per-tx compute cap
const TIP_BUMP_NUMERATOR: u64 = 3; // 1.5x
const TIP_BUMP_DENOMINATOR: u64 = 2;

/// A deterministic fault to inject. Mirrors the three deterministic `FailureClass`
/// causes; `FeeTooLow` is probabilistic (landing-contention) and not injectable here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Injection {
    ExpiredBlockhash,
    ComputeExceeded,
    BundleFailure,
}

/// Parse `ARGUS_INJECT` into an optional `Injection`. snake_case to match the
/// `FailureClass` wire strings; blank/unknown -> `None` (no injection).
pub fn parse_injection(raw: Option<&str>) -> Option<Injection> {
    match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("expired_blockhash") => Some(Injection::ExpiredBlockhash),
        Some("compute_exceeded") => Some(Injection::ComputeExceeded),
        Some("bundle_failure") => Some(Injection::BundleFailure),
        _ => None,
    }
}

/// Map a preflight `SimResult` to a `FailureClass` — the central testable unit.
/// Case-insensitive substring match on the combined `err` + `logs`, MOST-SPECIFIC
/// FIRST (order is load-bearing: a compute error's `err` also contains
/// "InstructionError", so compute must be tested before the generic bundle catch).
/// `FeeTooLow` is intentionally absent: it is a probabilistic landing outcome, not
/// anything simulation reports. Any other non-empty `err` is an Organic Failure
/// (PLAN.md) -> `BundleFailure`.
pub fn classify_failure(sim: &SimResult) -> Option<FailureClass> {
    let hay = format!(
        "{} {}",
        sim.err.clone().unwrap_or_default(),
        sim.logs.join(" ")
    )
    .to_lowercase();

    if hay.contains("blockhashnotfound")
        || hay.contains("blockhash not found")
        || hay.contains("block height exceeded")
    {
        Some(FailureClass::ExpiredBlockhash)
    } else if hay.contains("computationalbudgetexceeded") // the real runtime enum name
        || hay.contains("computebudgetexceeded")
        || hay.contains("exceeded cus")
        || hay.contains("exceeded compute")
        || hay.contains("compute budget exceeded")
    {
        Some(FailureClass::ComputeExceeded)
    } else if hay.contains("insufficient")
        || hay.contains("custom program error")
        || hay.contains("instructionerror")
        || hay.contains("\"custom\"")
    {
        Some(FailureClass::BundleFailure)
    } else if sim.err.is_some() {
        Some(FailureClass::BundleFailure) // organic catch-all
    } else {
        None // would succeed
    }
}

/// The local default policy that stands in for the Agent until Day 9-10 — each
/// Failure Class to its canonical Remedy (the PLAN.md failure matrix).
pub fn default_remedy(class: FailureClass) -> Remedy {
    match class {
        FailureClass::ExpiredBlockhash => Remedy::RefreshBlockhash,
        FailureClass::ComputeExceeded => Remedy::RaiseCuLimit,
        FailureClass::BundleFailure => Remedy::Abort,
        FailureClass::FeeTooLow => Remedy::BumpTip,
    }
}

/// The over-budget payload for the Bundle-Failure injection: the default payload
/// plus a self-transfer of more lamports than the payer holds (deterministic
/// "insufficient lamports" in simulation). Pure — tested via `build_bundle_with_payload`.
pub fn failing_payload(payer: &Pubkey, nonce: &str, self_transfer_lamports: u64) -> Vec<Instruction> {
    let mut payload = crate::bundle::default_payload(payer, nonce, self_transfer_lamports);
    payload.push(solana_system_interface::instruction::transfer(
        payer,
        payer,
        OVERSPEND_LAMPORTS,
    ));
    payload
}

/// The per-attempt knobs a Remedy can change between retries (blockhash is always
/// re-fetched fresh per attempt, so it isn't carried here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryState {
    pub tip_lamports: u64,
    pub cu_limit: Option<u32>,
}

/// Apply a Remedy to the retry state, returning the next attempt's state and whether
/// to continue (`false` = stop). The one place remedy semantics live.
pub fn apply_remedy(remedy: Remedy, state: RetryState, tip_floor_p75: u64) -> (RetryState, bool) {
    match remedy {
        // Blockhash is re-fetched fresh each attempt; "hold" is the leader-window
        // wait. Neither changes the carried state — just retry.
        Remedy::RefreshBlockhash | Remedy::HoldAndResubmit => (state, true),
        Remedy::RaiseCuLimit => {
            let raised = state
                .cu_limit
                .unwrap_or(0)
                .saturating_mul(2)
                .max(RAISE_CU_FLOOR)
                .min(MAX_CU_LIMIT);
            (
                RetryState {
                    cu_limit: Some(raised),
                    ..state
                },
                true,
            )
        }
        Remedy::BumpTip => {
            let bumped = state
                .tip_lamports
                .saturating_mul(TIP_BUMP_NUMERATOR)
                / TIP_BUMP_DENOMINATOR;
            (
                RetryState {
                    tip_lamports: bumped.max(tip_floor_p75),
                    ..state
                },
                true,
            )
        }
        Remedy::Abort => (state, false),
    }
}

/// The decision seam: a local default policy now, the HTTP Agent (Day 9-10) later,
/// behind one `decide` call. A plain enum (no `async-trait`) keeps the zero-new-crates
/// posture; Day 9-10 swaps `Policy::Agent` in with no call-site change.
pub enum Policy {
    Local,
    Agent(AgentClient),
}

impl Policy {
    pub async fn decide(&self, ctx: &FailureContext<'_>) -> Result<Decision> {
        match self {
            Policy::Local => Ok(local_decision(ctx.failure_class)),
            Policy::Agent(client) => client.decide(ctx).await,
        }
    }
}

/// The local policy's decision (pure — the testable core of `Policy::Local`).
fn local_decision(class: FailureClass) -> Decision {
    let remedy = default_remedy(class);
    Decision {
        remedy,
        rationale: format!("local default policy: {class:?} -> {remedy:?} (Agent stand-in, ADR 0003)"),
        confidence: 1.0,
        reasoning_trace: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sim(err: Option<&str>, logs: &[&str]) -> SimResult {
        SimResult {
            err: err.map(String::from),
            logs: logs.iter().map(|s| s.to_string()).collect(),
            units_consumed: None,
        }
    }

    #[test]
    fn parse_injection_maps_each_class_else_none() {
        assert_eq!(parse_injection(Some("expired_blockhash")), Some(Injection::ExpiredBlockhash));
        assert_eq!(parse_injection(Some(" Compute_Exceeded ")), Some(Injection::ComputeExceeded));
        assert_eq!(parse_injection(Some("bundle_failure")), Some(Injection::BundleFailure));
        assert_eq!(parse_injection(None), None);
        assert_eq!(parse_injection(Some("")), None);
        assert_eq!(parse_injection(Some("nonsense")), None);
    }

    #[test]
    fn classifies_expired_blockhash() {
        assert_eq!(classify_failure(&sim(Some("BlockhashNotFound"), &[])), Some(FailureClass::ExpiredBlockhash));
        assert_eq!(
            classify_failure(&sim(None, &["Error: block height exceeded"])),
            Some(FailureClass::ExpiredBlockhash)
        );
    }

    #[test]
    fn classifies_compute_exceeded() {
        // The REAL runtime err string observed live is `ComputationalBudgetExceeded`
        // (not the docs' `ComputeBudgetExceeded`); the err also carries
        // "InstructionError", so compute must outrank the bundle-failure catch.
        let s = sim(
            Some("{\"InstructionError\":[0,\"ComputationalBudgetExceeded\"]}"),
            &["Program failed: exceeded CUs meter at BPF instruction"],
        );
        assert_eq!(classify_failure(&s), Some(FailureClass::ComputeExceeded));
    }

    #[test]
    fn compute_outranks_the_instruction_error_catch() {
        // err contains BOTH "ComputationalBudgetExceeded" and "InstructionError";
        // it must classify as ComputeExceeded, not the generic BundleFailure.
        let s = sim(Some("{\"InstructionError\":[0,\"ComputationalBudgetExceeded\"]}"), &[]);
        assert_eq!(classify_failure(&s), Some(FailureClass::ComputeExceeded));
    }

    #[test]
    fn classifies_bundle_failure_insufficient_and_custom() {
        assert_eq!(
            classify_failure(&sim(Some("{\"InstructionError\":[2,{\"Custom\":1}]}"), &["Transfer: insufficient lamports 1, need 1000000000000000"])),
            Some(FailureClass::BundleFailure)
        );
        assert_eq!(
            classify_failure(&sim(Some("{\"InstructionError\":[0,{\"Custom\":6001}]}"), &[])),
            Some(FailureClass::BundleFailure)
        );
    }

    #[test]
    fn classifies_unknown_err_as_organic_bundle_failure() {
        assert_eq!(classify_failure(&sim(Some("AccountInUse"), &[])), Some(FailureClass::BundleFailure));
    }

    #[test]
    fn classifies_success_as_none() {
        assert_eq!(classify_failure(&sim(None, &["Program ... success"])), None);
    }

    #[test]
    fn precedence_blockhash_outranks_compute() {
        // A sim carrying BOTH signals classifies as ExpiredBlockhash (checked first).
        let s = sim(Some("BlockhashNotFound"), &["also: exceeded CUs meter"]);
        assert_eq!(classify_failure(&s), Some(FailureClass::ExpiredBlockhash));
    }

    #[test]
    fn default_remedy_maps_each_class() {
        assert_eq!(default_remedy(FailureClass::ExpiredBlockhash), Remedy::RefreshBlockhash);
        assert_eq!(default_remedy(FailureClass::ComputeExceeded), Remedy::RaiseCuLimit);
        assert_eq!(default_remedy(FailureClass::BundleFailure), Remedy::Abort);
        assert_eq!(default_remedy(FailureClass::FeeTooLow), Remedy::BumpTip);
    }

    #[test]
    fn raise_cu_lifts_to_floor_doubles_and_caps() {
        // From the pathological injected limit (1) -> jumps to the working floor.
        let (s1, cont) = apply_remedy(Remedy::RaiseCuLimit, RetryState { tip_lamports: 5000, cu_limit: Some(1) }, 0);
        assert!(cont);
        assert_eq!(s1.cu_limit, Some(200_000), "tiny limit lifts to the working floor");
        // Above the floor -> doubles.
        let (s2, _) = apply_remedy(Remedy::RaiseCuLimit, RetryState { tip_lamports: 5000, cu_limit: Some(150_000) }, 0);
        assert_eq!(s2.cu_limit, Some(300_000), "doubles when already above the floor");
        // Doubling past the per-tx cap clamps.
        let (s3, _) = apply_remedy(Remedy::RaiseCuLimit, RetryState { tip_lamports: 5000, cu_limit: Some(1_000_000) }, 0);
        assert_eq!(s3.cu_limit, Some(1_400_000), "clamped at the Solana per-tx cap");
    }

    #[test]
    fn bump_tip_floors_at_p75_else_scales() {
        // Below p75 -> lifted to p75.
        let (s1, cont) = apply_remedy(Remedy::BumpTip, RetryState { tip_lamports: 1_000, cu_limit: None }, 8_000);
        assert!(cont);
        assert_eq!(s1.tip_lamports, 8_000, "a tip below p75 is lifted to p75");
        // Above p75 -> scaled by 1.5x.
        let (s2, _) = apply_remedy(Remedy::BumpTip, RetryState { tip_lamports: 10_000, cu_limit: None }, 1_000);
        assert_eq!(s2.tip_lamports, 15_000, "1.5x bump when already above p75");
    }

    #[test]
    fn abort_stops_refresh_and_hold_are_noops() {
        let base = RetryState { tip_lamports: 5_000, cu_limit: Some(20_000) };
        let (s, cont) = apply_remedy(Remedy::Abort, base, 0);
        assert!(!cont, "Abort stops the retry loop");
        assert_eq!(s, base, "Abort doesn't mutate state");
        assert_eq!(apply_remedy(Remedy::RefreshBlockhash, base, 0), (base, true));
        assert_eq!(apply_remedy(Remedy::HoldAndResubmit, base, 0), (base, true));
    }

    #[test]
    fn local_decision_follows_default_remedy() {
        for class in [
            FailureClass::ExpiredBlockhash,
            FailureClass::ComputeExceeded,
            FailureClass::BundleFailure,
            FailureClass::FeeTooLow,
        ] {
            let d = local_decision(class);
            assert_eq!(d.remedy, default_remedy(class));
            assert_eq!(d.confidence, 1.0);
            assert!(d.reasoning_trace.is_none(), "the local stand-in has no Reasoning Trace");
        }
    }
}
