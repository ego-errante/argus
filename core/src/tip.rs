//! Dynamic Tip computation (ADR 0005, PLAN.md Day 5-6).
//!
//! Base Tip = a Jito Tip Floor percentile (~50th-75th, WS with REST fallback)
//! scaled by Leader Window urgency, rotated across the published Tip Accounts.
//! NO hardcoded tip values. The Agent's "bump_tip" Remedy adjusts on failure;
//! base tipping stays deterministic here.

use anyhow::{anyhow, Result};

/// Jito enforces a minimum tip; never submit below this.
const MIN_TIP_LAMPORTS: u64 = 1_000;
const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

/// Fetch a Base Tip (lamports) from the Jito Tip Floor REST endpoint (ADR 0005).
/// Uses the 75th landed-tips percentile — biased toward landing for the tracer
/// bullet; the full urgency-scaled curve arrives in the Day 5-6 milestone.
pub async fn fetch_tip_lamports(tip_floor_url: &str) -> Result<u64> {
    let client = reqwest::Client::new();
    let resp: serde_json::Value = client.get(tip_floor_url).send().await?.json().await?;

    // The endpoint returns a single-element array of SOL-denominated percentiles.
    let row = resp
        .get(0)
        .ok_or_else(|| anyhow!("empty tip floor response: {resp}"))?;
    let sol = row["landed_tips_75th_percentile"]
        .as_f64()
        .ok_or_else(|| anyhow!("no landed_tips_75th_percentile in tip floor: {resp}"))?;

    let lamports = (sol * LAMPORTS_PER_SOL) as u64;
    Ok(lamports.max(MIN_TIP_LAMPORTS))
}
