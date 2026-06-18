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
use tracing::warn;

/// How far back to age the injected blockhash. Blockhashes are valid ~150 slots, so
/// 200 is reliably expired while still being a genuinely real, recent cluster hash.
pub const AGED_BLOCKHASH_SLOTS: u64 = 200;
/// CU limit for the Compute-Exceeded injection — far below any real payload's need.
pub const INJECT_CU_LIMIT: u32 = 1;
/// A self-transfer far larger than the low-balance keypair holds — a deterministic
/// "insufficient lamports" in simulation, for the Bundle-Failure injection.
pub const OVERSPEND_LAMPORTS: u64 = 1_000_000_000_000_000;

/// Remedy tuning (kept as module consts — operationally fixed, not env knobs).
const CU_MARGIN_NUMERATOR: u32 = 3; // 1.5x headroom over the observed CU need
const CU_MARGIN_DENOMINATOR: u32 = 2;
const RAISE_CU_FLOOR_MIN: u32 = 1_000; // never raise to a uselessly tiny limit
/// Solana per-tx compute cap — also the ceiling for the RaiseCuLimit max-CU re-sim.
pub const MAX_CU_LIMIT: u32 = 1_400_000;
const TIP_BUMP_NUMERATOR: u64 = 3; // 1.5x
const TIP_BUMP_DENOMINATOR: u64 = 2;

/// Provenance markers for the `Decision.model` field on the two local paths (ADR 0006).
/// Single source of truth — the ADR 0006 trace-provenance filter keys on these exact
/// strings, so a stray typo in a literal would silently break it. `model` stays free
/// text (it also holds real OpenRouter slugs), so these are consts, not an enum.
pub const MODEL_LOCAL: &str = "local";
pub const MODEL_LOCAL_FALLBACK: &str = "local-fallback";

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
/// Keys on the STRUCTURED `instruction_error` variant (parsed once from the runtime
/// error in `rpc::sim_result_from_response`), not on substrings of flattened JSON —
/// the lossy string match is what let the real `ComputationalBudgetExceeded` enum slip
/// past once (ADR 0010). `FeeTooLow` is intentionally absent: it is a probabilistic
/// landing outcome, not anything simulation reports. Any other non-empty `err` is an
/// Organic Failure (PLAN.md) -> `BundleFailure`.
pub fn classify_failure(sim: &SimResult) -> Option<FailureClass> {
    // Expired blockhash surfaces as a top-level message (string `err` or a JSON-RPC
    // error), NOT a structured InstructionError — match it on the err+logs text first.
    let text = format!(
        "{} {}",
        sim.err.clone().unwrap_or_default(),
        sim.logs.join(" ")
    )
    .to_lowercase();
    if text.contains("blockhashnotfound")
        || text.contains("blockhash not found")
        || text.contains("block height exceeded")
    {
        return Some(FailureClass::ExpiredBlockhash);
    }

    // A structured instruction error is the deterministic, runtime-sourced signal:
    // key Compute-Exceeded on the actual variant, and treat every OTHER instruction
    // error as a Bundle (program) Failure — no substring-guessing against log prose.
    if let Some(ie) = &sim.instruction_error {
        let v = ie.to_lowercase();
        if v.contains("computationalbudgetexceeded") || v.contains("computebudgetexceeded") {
            return Some(FailureClass::ComputeExceeded);
        }
        return Some(FailureClass::BundleFailure);
    }

    // No structured error — fall back to compute signals in the logs, then to the
    // organic catch-all for any remaining non-empty error.
    if text.contains("exceeded cus")
        || text.contains("exceeded compute")
        || text.contains("compute budget exceeded")
    {
        return Some(FailureClass::ComputeExceeded);
    }
    if sim.err.is_some() {
        return Some(FailureClass::BundleFailure); // organic catch-all
    }
    None // would succeed
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

/// The per-attempt knobs a Remedy can change between retries — the single carrier of
/// the bundle's tunable inputs (blockhash is always re-fetched fresh per attempt, so
/// it isn't carried here). Adding a future remedy knob means extending this struct,
/// not threading another loose argument through the submit path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryState {
    pub tip_lamports: u64,
    pub cu_limit: Option<u32>,
    pub priority_fee_microlamports: Option<u64>,
}

/// Apply a Remedy to the retry state, returning the next attempt's state and whether
/// to continue (`false` = stop). The one place remedy semantics live. `cu_used` is the
/// TRUE compute need observed from a max-CU re-simulation (the caller measures it), so
/// `RaiseCuLimit` derives the new limit from observation rather than a payload-tuned
/// constant — falling back to doubling when no observation is available.
pub fn apply_remedy(
    remedy: Remedy,
    state: RetryState,
    tip_floor_p75: u64,
    cu_used: Option<u32>,
) -> (RetryState, bool) {
    match remedy {
        // Blockhash is re-fetched fresh each attempt; "hold" is the leader-window
        // wait. Neither changes the carried state — just retry.
        Remedy::RefreshBlockhash | Remedy::HoldAndResubmit => (state, true),
        Remedy::RaiseCuLimit => {
            // Observed need × margin (preferred), else double the prior limit; floor at
            // a small minimum, cap at the per-tx max. No payload-tuned magic constant.
            let from_observed = cu_used
                .map(|n| n.saturating_mul(CU_MARGIN_NUMERATOR) / CU_MARGIN_DENOMINATOR)
                .unwrap_or(0);
            let from_double = state.cu_limit.unwrap_or(0).saturating_mul(2);
            let raised = from_observed
                .max(from_double)
                .clamp(RAISE_CU_FLOOR_MIN, MAX_CU_LIMIT);
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
    /// Decide a Remedy. The Agent arm NEVER propagates a transport error: on any
    /// failure (unreachable, timeout, 5xx, bad body) it warns loudly and returns the
    /// local fallback decision, marked `local-fallback`, so a transient Agent hiccup
    /// can't kill an in-progress Run yet the provenance never lies (ADR 0006/0008).
    pub async fn decide(&self, ctx: &FailureContext<'_>) -> Result<Decision> {
        match self {
            Policy::Local => Ok(local_decision(ctx.failure_class)),
            Policy::Agent(client) => match client.decide(ctx).await {
                Ok(d) => Ok(d),
                Err(e) => {
                    warn!(error = %e, "agent decide failed — falling back to local default policy (recorded)");
                    Ok(fallback_decision(ctx.failure_class, &e.to_string()))
                }
            },
        }
    }
}

/// Shared shape of the two local (non-Agent) Decisions: the class's default remedy,
/// full confidence, NO Reasoning Trace (so the ADR 0006 provenance check excludes them
/// from scored evidence), and a `model` provenance marker. The caller supplies the
/// marker + rationale — the only things that differ between the stand-in and the fallback.
fn local_like(class: FailureClass, model: &str, rationale: String) -> Decision {
    Decision {
        remedy: default_remedy(class),
        rationale,
        confidence: 1.0,
        reasoning_trace: None,
        model: Some(model.to_string()),
    }
}

/// The local policy's decision (pure — the testable core of `Policy::Local`).
fn local_decision(class: FailureClass) -> Decision {
    let remedy = default_remedy(class);
    local_like(
        class,
        MODEL_LOCAL,
        format!("local default policy: {class:?} -> {remedy:?} (Agent stand-in, ADR 0003)"),
    )
}

/// The decision used when the Agent path errors (Q3): the local default remedy, marked
/// `local-fallback` with the cause in the rationale. A fallback row carries no Reasoning
/// Trace, so the ADR 0006 trace-provenance check naturally excludes it from scored evidence.
fn fallback_decision(class: FailureClass, err: &str) -> Decision {
    let remedy = default_remedy(class);
    local_like(
        class,
        MODEL_LOCAL_FALLBACK,
        format!("agent unreachable ({err}); local fallback: {class:?} -> {remedy:?}"),
    )
}

/// True when an optional string field is absent or blank (only whitespace). On the Agent
/// path it flags the ADR 0006 evidence gaps to warn on — an empty Reasoning Trace or an
/// empty `model` slug (the decision is kept, but that provenance is weak).
pub fn is_blank(s: Option<&str>) -> bool {
    s.map(str::trim).is_none_or(str::is_empty)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Build a SimResult the way production does — route the fixture through the real
    // `sim_result_from_response`, so a structured `err` string (e.g. an InstructionError)
    // populates `instruction_error` exactly as a live response would. `err` is parsed as
    // JSON when it is JSON, else treated as a plain string error.
    fn sim(err: Option<&str>, logs: &[&str]) -> SimResult {
        let err_json = match err {
            None => serde_json::Value::Null,
            Some(e) => serde_json::from_str(e).unwrap_or_else(|_| serde_json::Value::String(e.to_string())),
        };
        let resp = serde_json::json!({
            "result": { "value": { "err": err_json, "logs": logs, "unitsConsumed": 0 } }
        });
        crate::rpc::sim_result_from_response(&resp).unwrap()
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

    fn retry(tip_lamports: u64, cu_limit: Option<u32>) -> RetryState {
        RetryState { tip_lamports, cu_limit, priority_fee_microlamports: None }
    }

    #[test]
    fn raise_cu_derives_from_observed_need_else_doubles_and_caps() {
        // With an observed true need (from the max-CU re-sim), raise to need × 1.5.
        let (s0, cont) = apply_remedy(Remedy::RaiseCuLimit, retry(5000, Some(1)), 0, Some(40_000));
        assert!(cont);
        assert_eq!(s0.cu_limit, Some(60_000), "observed 40k need -> 1.5x headroom");
        // No observation + the pathological injected limit (1) -> the small safety floor.
        let (s1, _) = apply_remedy(Remedy::RaiseCuLimit, retry(5000, Some(1)), 0, None);
        assert_eq!(s1.cu_limit, Some(1_000), "no observation, tiny prior -> safety floor");
        // No observation, above the floor -> doubles the prior limit.
        let (s2, _) = apply_remedy(Remedy::RaiseCuLimit, retry(5000, Some(150_000)), 0, None);
        assert_eq!(s2.cu_limit, Some(300_000), "no observation -> doubles the prior limit");
        // Either source past the per-tx cap clamps.
        let (s3, _) = apply_remedy(Remedy::RaiseCuLimit, retry(5000, Some(1_000_000)), 0, Some(2_000_000));
        assert_eq!(s3.cu_limit, Some(1_400_000), "clamped at the Solana per-tx cap");
    }

    #[test]
    fn bump_tip_floors_at_p75_else_scales() {
        // Below p75 -> lifted to p75.
        let (s1, cont) = apply_remedy(Remedy::BumpTip, retry(1_000, None), 8_000, None);
        assert!(cont);
        assert_eq!(s1.tip_lamports, 8_000, "a tip below p75 is lifted to p75");
        // Above p75 -> scaled by 1.5x.
        let (s2, _) = apply_remedy(Remedy::BumpTip, retry(10_000, None), 1_000, None);
        assert_eq!(s2.tip_lamports, 15_000, "1.5x bump when already above p75");
    }

    #[test]
    fn abort_stops_refresh_and_hold_are_noops() {
        let base = retry(5_000, Some(20_000));
        let (s, cont) = apply_remedy(Remedy::Abort, base, 0, None);
        assert!(!cont, "Abort stops the retry loop");
        assert_eq!(s, base, "Abort doesn't mutate state");
        assert_eq!(apply_remedy(Remedy::RefreshBlockhash, base, 0, None), (base, true));
        assert_eq!(apply_remedy(Remedy::HoldAndResubmit, base, 0, None), (base, true));
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
            assert_eq!(d.model.as_deref(), Some(MODEL_LOCAL), "local policy is marked 'local'");
        }
    }

    #[test]
    fn fallback_decision_uses_default_remedy_and_is_marked() {
        // The Agent-unreachable fallback must still pick the right remedy, but be marked
        // 'local-fallback' with no trace so it's excluded from scored evidence (ADR 0006).
        let d = fallback_decision(FailureClass::ComputeExceeded, "connection refused");
        assert_eq!(d.remedy, default_remedy(FailureClass::ComputeExceeded));
        assert_eq!(d.model.as_deref(), Some(MODEL_LOCAL_FALLBACK));
        assert!(d.reasoning_trace.is_none(), "a fallback carries no Reasoning Trace");
        assert!(d.rationale.contains("connection refused"), "the cause is recorded in the rationale");
    }

    #[test]
    fn is_blank_treats_absent_and_whitespace_as_blank() {
        assert!(is_blank(None), "absent -> blank");
        assert!(is_blank(Some("")), "empty string -> blank");
        assert!(is_blank(Some("   \n\t")), "whitespace -> blank");
        assert!(!is_blank(Some("I chose refresh because ...")), "real reasoning -> not blank");
        assert!(!is_blank(Some("anthropic/claude-sonnet-4.6")), "a real model slug -> not blank");
    }
}
