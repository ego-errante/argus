//! Helius Sender submission — keyless reliability BACKSTOP, not the scored path (ADR 0007).
//!
//! The scored deliverable is real Jito bundles (see `bundle.rs`); Sender exists only
//! as a liveness fallback when bundles don't land. It submits a *single transaction*
//! (not a Jito bundle) dual-routed through staked validator connections AND the Jito
//! auction, via `sendTransaction` with `skipPreflight: true`, and mandates both a
//! priority fee and a tip to one of its tip accounts.

use anyhow::{anyhow, Result};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::transaction::Transaction;
use std::str::FromStr;

/// Sender's published mainnet tip accounts. Rotated locally (a tip is mandatory;
/// any one is valid). Picking among them reduces write-lock contention.
const TIP_ACCOUNTS: [&str; 10] = [
    "4ACfpUFoaSD9bfPdeu6DBt89gB6ENTeHBXCAi87NhDEE",
    "D2L6yPZ2FmmmTKPgzaMKdhu6EWZcTpLy1Vhx8uvZe7NZ",
    "9bnz4RShgq1hAnLnZbP8kbgBg1kEmcJBYQq3gQbmnSta",
    "5VY91ws6B2hMmBFRsXkoAAdsPHBJwRfBht4DXox3xkwn",
    "2nyhqdwKcJZR2vcqCyrYsaPVdAnFoJjiksCXJ7hfEYgD",
    "2q5pghRs6arqVjRvT5gfgWfWcHWmw1ZuCzphgd5KfWGJ",
    "wyvPkWjVZz1M8fHQnMMCDTQDbkManefNNhweYk5WkcF",
    "3KCKozbAaF75qEU33jtzozcJ29yJuaLJTy2jFdzUY8bT",
    "4vieeGHPYPG2MmyPRcYjdiDmmhN3ww7hsFNap8pVN3Ey",
    "4TQLFNWK8AovT1gFvda5jfw2oJeRMKEmw7aH6MGBJ3or",
];

/// Sender's documented minimum tips (lamports) — the DEFAULTS the config falls
/// back to (`SENDER_DUAL_MIN_TIP_LAMPORTS` / `SENDER_SWQOS_MIN_TIP_LAMPORTS`).
pub const DUAL_MIN_TIP_LAMPORTS: u64 = 200_000; // 0.0002 SOL (staked + Jito)
pub const SWQOS_MIN_TIP_LAMPORTS: u64 = 5_000; //   0.000005 SOL (staked only)

/// Sender mandates a compute budget on its single-tx path; these are the DEFAULTS
/// the config falls back to (`SENDER_COMPUTE_UNIT_LIMIT` / `SENDER_PRIORITY_FEE_MICROLAMPORTS`).
pub const COMPUTE_UNIT_LIMIT: u32 = 20_000;
pub const PRIORITY_FEE_MICROLAMPORTS: u64 = 100_000;

/// A Sender tip account, rotated by `i`.
pub fn tip_account(i: usize) -> Pubkey {
    Pubkey::from_str(TIP_ACCOUNTS[i % TIP_ACCOUNTS.len()]).expect("valid sender tip account")
}

/// The minimum tip for the selected route, given the configured per-route minimums.
pub fn min_tip_lamports(swqos_only: bool, dual_min: u64, swqos_min: u64) -> u64 {
    if swqos_only {
        swqos_min
    } else {
        dual_min
    }
}

/// Submit one fully-signed transaction to Helius Sender. Returns the signature.
/// `skipPreflight` is mandatory; `maxRetries: 0` lets Sender own the retry path.
pub async fn submit(sender_url: &str, tx: &Transaction) -> Result<String> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bincode::serialize(tx)?);
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "sendTransaction",
        "params": [b64, { "encoding": "base64", "skipPreflight": true, "maxRetries": 0 }],
    });
    let resp: serde_json::Value = reqwest::Client::new()
        .post(sender_url)
        .json(&body)
        .send()
        .await?
        .json()
        .await?;
    resp["result"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| anyhow!("Sender sendTransaction returned no signature: {resp}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tip_accounts_parse_and_rotate() {
        // All ten parse, and rotation wraps.
        for i in 0..TIP_ACCOUNTS.len() {
            let _ = tip_account(i);
        }
        assert_eq!(tip_account(0), tip_account(TIP_ACCOUNTS.len()));
    }

    #[test]
    fn min_tip_matches_route() {
        // Dual route -> dual min; staked-only route -> the (lower) swqos min.
        assert_eq!(min_tip_lamports(false, 200_000, 5_000), 200_000);
        assert_eq!(min_tip_lamports(true, 200_000, 5_000), 5_000);
    }
}
