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
/// `percentile` selects the landed-tips percentile field (config-driven, no
/// hardcoded value — see `Config::jito_tip_percentile`).
pub async fn fetch_tip_lamports(tip_floor_url: &str, percentile: u8) -> Result<u64> {
    let client = reqwest::Client::new();
    let resp: serde_json::Value = client.get(tip_floor_url).send().await?.json().await?;
    tip_lamports_from_response(&resp, percentile)
}

/// Pure parse of the Tip Floor response: read `landed_tips_{percentile}th_percentile`
/// (SOL), convert to lamports, and floor at Jito's minimum. Network-free so the
/// percentile selection + conversion are unit-testable.
pub fn tip_lamports_from_response(resp: &serde_json::Value, percentile: u8) -> Result<u64> {
    // The endpoint returns a single-element array of SOL-denominated percentiles.
    let row = resp
        .get(0)
        .ok_or_else(|| anyhow!("empty tip floor response: {resp}"))?;
    let field = format!("landed_tips_{percentile}th_percentile");
    let sol = row[field.as_str()]
        .as_f64()
        .ok_or_else(|| anyhow!("no {field} in tip floor: {resp}"))?;

    let lamports = (sol * LAMPORTS_PER_SOL) as u64;
    Ok(lamports.max(MIN_TIP_LAMPORTS))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floor_response() -> serde_json::Value {
        // Shape of the real Jito Tip Floor response (single-element array).
        serde_json::json!([{
            "landed_tips_25th_percentile": 0.000_001,
            "landed_tips_50th_percentile": 0.000_010,
            "landed_tips_75th_percentile": 0.000_100,
            "landed_tips_95th_percentile": 0.001_000,
            "landed_tips_99th_percentile": 0.010_000,
        }])
    }

    #[test]
    fn reads_the_configured_percentile_field() {
        let resp = floor_response();
        // 75th = 0.0001 SOL = 100_000 lamports; 50th = 0.00001 SOL = 10_000 lamports.
        assert_eq!(tip_lamports_from_response(&resp, 75).unwrap(), 100_000);
        assert_eq!(tip_lamports_from_response(&resp, 50).unwrap(), 10_000);
        assert_eq!(tip_lamports_from_response(&resp, 99).unwrap(), 10_000_000);
    }

    #[test]
    fn floors_at_jito_minimum() {
        // 25th = 0.000001 SOL = 1_000 lamports exactly — at the floor, not below.
        assert_eq!(tip_lamports_from_response(&floor_response(), 25).unwrap(), MIN_TIP_LAMPORTS);
    }

    #[test]
    fn errors_when_percentile_field_absent() {
        // A percentile with no published field -> Err, not a panic.
        assert!(tip_lamports_from_response(&floor_response(), 42).is_err());
    }
}
