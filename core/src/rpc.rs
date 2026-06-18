//! Minimal JSON-RPC over reqwest (Day 1-2). The full lifecycle is stream-based
//! (ADR 0004); this is only enough to fetch a recent blockhash for submission.

use anyhow::{anyhow, Result};
use solana_sdk::hash::Hash;
use solana_sdk::transaction::Transaction;
use std::str::FromStr;
use std::sync::OnceLock;

/// One pooled reqwest client shared by every JSON-RPC call, so connections + TLS
/// sessions are reused (injection runs fire ~6 calls back-to-back at the same host).
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// Issue one JSON-RPC call and return the parsed response `Value`. The single place
/// the reqwest transport lives — callers build only `params` and parse the response.
async fn rpc_call(http_url: &str, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let resp = http_client().post(http_url).json(&body).send().await?.json().await?;
    Ok(resp)
}

/// Fetch a recent blockhash at `confirmed` commitment (freshest viable — the
/// README's question 2 reasoning: a finalized blockhash burns the validity window).
pub async fn get_latest_blockhash(http_url: &str) -> Result<Hash> {
    let resp = rpc_call(http_url, "getLatestBlockhash", serde_json::json!([{ "commitment": "confirmed" }])).await?;
    let bh = resp["result"]["value"]["blockhash"]
        .as_str()
        .ok_or_else(|| anyhow!("no blockhash in getLatestBlockhash response: {resp}"))?;
    Hash::from_str(bh).map_err(|e| anyhow!("invalid blockhash {bh}: {e}"))
}

/// Poll a signature's commitment via RPC. `Ok(Some(slot))` once confirmed/finalized,
/// `Ok(None)` on timeout. Used to confirm a Jito landing WITHOUT hammering the
/// rate-limited Block Engine (the full lifecycle uses streams — ADR 0004).
pub async fn await_signature(
    http_url: &str,
    signature: &str,
    tries: u32,
    delay_ms: u64,
) -> Result<Option<u64>> {
    for _ in 0..tries {
        let resp = rpc_call(
            http_url,
            "getSignatureStatuses",
            serde_json::json!([[signature], { "searchTransactionHistory": false }]),
        )
        .await?;
        let status = &resp["result"]["value"][0];
        if !status.is_null() {
            let commitment = status["confirmationStatus"].as_str().unwrap_or("");
            if matches!(commitment, "confirmed" | "finalized") {
                return Ok(status["slot"].as_u64());
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
    }
    Ok(None)
}

/// The outcome of a preflight `simulateTransaction` — the deterministic source of a
/// failure REASON (Day 7-8, ADR 0010). A Jito bundle is all-or-nothing, so a failing
/// tx never lands and leaves NO on-chain meta to read; preflight simulation is the
/// only pre-submit reason source. `err == None` means the tx would succeed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SimResult {
    pub err: Option<String>,
    /// The InstructionError VARIANT parsed once from a structured tx error — e.g.
    /// `ComputationalBudgetExceeded` from `{"InstructionError":[idx, variant]}`. Lets
    /// classification key on the runtime enum instead of substrings of flattened JSON.
    pub instruction_error: Option<String>,
    pub logs: Vec<String>,
    pub units_consumed: Option<u32>,
}

/// Extract the variant of a structured `{"InstructionError":[idx, variant]}` tx error.
/// `variant` is a bare string (`"ComputationalBudgetExceeded"`) or an object
/// (`{"Custom":1}`) — return the string verbatim, the object stringified, else `None`.
fn instruction_error_variant(err: &serde_json::Value) -> Option<String> {
    match err.get("InstructionError")?.get(1)? {
        serde_json::Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// Pure parse of a `simulateTransaction` JSON-RPC response into a `SimResult`.
/// A JSON-RPC-level `error` (vs a tx-level `err`) is surfaced as the reason text.
pub fn sim_result_from_response(resp: &serde_json::Value) -> Result<SimResult> {
    if let Some(e) = resp.get("error").filter(|e| !e.is_null()) {
        return Ok(SimResult {
            err: Some(e.to_string()),
            instruction_error: None,
            logs: Vec::new(),
            units_consumed: None,
        });
    }
    let value = &resp["result"]["value"];
    if value.is_null() {
        return Err(anyhow!("no value in simulateTransaction response: {resp}"));
    }
    let err = match &value["err"] {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    };
    let instruction_error = instruction_error_variant(&value["err"]);
    let logs = value["logs"]
        .as_array()
        .map(|a| a.iter().filter_map(|l| l.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let units_consumed = value["unitsConsumed"].as_u64().map(|u| u as u32);
    Ok(SimResult {
        err,
        instruction_error,
        logs,
        units_consumed,
    })
}

/// Preflight a signed tx via `simulateTransaction`. `replaceRecentBlockhash:false` is
/// load-bearing: the default `true` swaps in a fresh blockhash, which would MASK an
/// expired-blockhash injection. `sigVerify:false` so a tx signed over an aged
/// blockhash still simulates (we want the blockhash error, not a sig error).
pub async fn simulate_transaction(http_url: &str, tx: &Transaction) -> Result<SimResult> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bincode::serialize(tx)?);
    let resp = rpc_call(
        http_url,
        "simulateTransaction",
        serde_json::json!([b64, {
            "encoding": "base64",
            "sigVerify": false,
            "replaceRecentBlockhash": false,
            "commitment": "confirmed"
        }]),
    )
    .await?;
    sim_result_from_response(&resp)
}

/// Current slot at `confirmed` — used to age a blockhash and stamp FailureContext.
pub async fn get_slot(http_url: &str) -> Result<u64> {
    let resp = rpc_call(http_url, "getSlot", serde_json::json!([{ "commitment": "confirmed" }])).await?;
    resp["result"]
        .as_u64()
        .ok_or_else(|| anyhow!("no slot in getSlot response: {resp}"))
}

/// Pure: extract the blockhash from a `getBlock` response, or `None` if the slot was
/// skipped (`result: null`) — the caller then walks back to the previous slot.
pub fn blockhash_from_block_response(resp: &serde_json::Value) -> Option<Hash> {
    resp["result"]["blockhash"]
        .as_str()
        .and_then(|s| Hash::from_str(s).ok())
}

/// Fetch a genuinely real blockhash from ~`slots_back` slots ago — old enough that
/// the cluster reports it expired (BlockhashNotFound) — for the headline
/// expired-blockhash injection (ADR 0010). Walks back over skipped slots.
pub async fn get_aged_blockhash(http_url: &str, slots_back: u64) -> Result<Hash> {
    let current = get_slot(http_url).await?;
    let target = current.saturating_sub(slots_back);
    for slot in (target.saturating_sub(40)..=target).rev() {
        let resp = rpc_call(
            http_url,
            "getBlock",
            serde_json::json!([slot, {
                "encoding": "json",
                "transactionDetails": "none",
                "rewards": false,
                "maxSupportedTransactionVersion": 0
            }]),
        )
        .await?;
        if let Some(h) = blockhash_from_block_response(&resp) {
            return Ok(h);
        }
    }
    Err(anyhow!("no block with a blockhash found near slot {target}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sim_parses_blockhash_not_found() {
        let resp = serde_json::json!({
            "result": { "value": { "err": "BlockhashNotFound", "logs": [], "unitsConsumed": 0 } }
        });
        let sim = sim_result_from_response(&resp).unwrap();
        assert_eq!(sim.err.as_deref(), Some("BlockhashNotFound"));
        assert_eq!(sim.units_consumed, Some(0));
    }

    #[test]
    fn sim_parses_compute_exceeded_with_logs() {
        // The REAL runtime enum is `ComputationalBudgetExceeded` (ADR 0010 live
        // correction), NOT the docs' `ComputeBudgetExceeded`.
        let resp = serde_json::json!({
            "result": { "value": {
                "err": { "InstructionError": [1, "ComputationalBudgetExceeded"] },
                "logs": ["Program ... failed: exceeded CUs meter at BPF instruction"],
                "unitsConsumed": 1
            } }
        });
        let sim = sim_result_from_response(&resp).unwrap();
        assert!(sim.err.unwrap().contains("ComputationalBudgetExceeded"));
        assert_eq!(
            sim.instruction_error.as_deref(),
            Some("ComputationalBudgetExceeded"),
            "the structured variant is parsed out for the classifier"
        );
        assert_eq!(sim.logs.len(), 1);
        assert_eq!(sim.units_consumed, Some(1));
    }

    #[test]
    fn sim_extracts_object_instruction_error_variant() {
        // An object variant (`{"Custom": 1}`) — stringified, still surfaced structurally.
        let resp = serde_json::json!({
            "result": { "value": {
                "err": { "InstructionError": [0, { "Custom": 1 }] },
                "logs": [], "unitsConsumed": 200
            } }
        });
        let sim = sim_result_from_response(&resp).unwrap();
        assert_eq!(sim.instruction_error.as_deref(), Some("{\"Custom\":1}"));
        assert_eq!(sim.units_consumed, Some(200));
    }

    #[test]
    fn sim_parses_success_as_no_err() {
        let resp = serde_json::json!({
            "result": { "value": { "err": serde_json::Value::Null, "logs": ["Program ... success"], "unitsConsumed": 450 } }
        });
        let sim = sim_result_from_response(&resp).unwrap();
        assert!(sim.err.is_none(), "a clean simulation has no err");
        assert_eq!(sim.units_consumed, Some(450));
    }

    #[test]
    fn sim_surfaces_jsonrpc_error() {
        let resp = serde_json::json!({ "error": { "code": -32002, "message": "blockhash not found" } });
        let sim = sim_result_from_response(&resp).unwrap();
        assert!(sim.err.unwrap().to_lowercase().contains("blockhash not found"));
    }

    #[test]
    fn block_response_extracts_blockhash_else_none() {
        let real = "11111111111111111111111111111111"; // valid base58 Hash shape
        let resp = serde_json::json!({ "result": { "blockhash": real } });
        assert_eq!(
            blockhash_from_block_response(&resp),
            Some(Hash::from_str(real).unwrap())
        );
        // A skipped slot returns `result: null` -> None (caller walks back).
        let skipped = serde_json::json!({ "result": serde_json::Value::Null });
        assert!(blockhash_from_block_response(&skipped).is_none());
    }
}
