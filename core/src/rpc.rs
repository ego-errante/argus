//! Minimal JSON-RPC over reqwest (Day 1-2). The full lifecycle is stream-based
//! (ADR 0004); this is only enough to fetch a recent blockhash for submission.

use anyhow::{anyhow, Result};
use solana_sdk::hash::Hash;
use std::str::FromStr;

/// Fetch a recent blockhash at `confirmed` commitment (freshest viable — the
/// README's question 2 reasoning: a finalized blockhash burns the validity window).
pub async fn get_latest_blockhash(http_url: &str) -> Result<Hash> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getLatestBlockhash",
        "params": [{ "commitment": "confirmed" }],
    });
    let resp: serde_json::Value =
        client.post(http_url).json(&body).send().await?.json().await?;
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
    let client = reqwest::Client::new();
    for _ in 0..tries {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getSignatureStatuses",
            "params": [[signature], { "searchTransactionHistory": false }],
        });
        let resp: serde_json::Value =
            client.post(http_url).json(&body).send().await?.json().await?;
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
