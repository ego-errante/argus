//! Dynamic Tip computation (ADR 0005, PLAN.md Day 5-6).
//!
//! Base Tip = a Jito Tip Floor percentile (config-driven via `JITO_TIP_PERCENTILE`),
//! rotated across the published Tip Accounts. NO hardcoded tip values. The Agent's
//! "bump_tip" Remedy adjusts on failure; base tipping stays deterministic here.

use anyhow::{anyhow, Result};

/// Jito enforces a minimum tip; never submit below this.
const MIN_TIP_LAMPORTS: u64 = 1_000;
/// Sanity ceiling: any computed tip above this (0.01 SOL) is treated as bad
/// upstream data, not a real floor — we refuse rather than submit it. Real Jito
/// tip-floor percentiles are sub-0.001 SOL, so this is generous headroom.
const MAX_TIP_LAMPORTS: u64 = 10_000_000;
const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

/// The percentiles Jito's Tip Floor publishes — the single source of truth for
/// both validation (config) and field-name construction (below). Anything else
/// has no `landed_tips_{p}th_percentile` field to read.
pub const SUPPORTED_TIP_PERCENTILES: [u8; 5] = [25, 50, 75, 95, 99];

/// Fetch a Base Tip (lamports) from the Jito Tip Floor REST endpoint (ADR 0005).
/// `percentile` selects the landed-tips percentile field (config-driven, no
/// hardcoded value — see `Config::jito_tip_percentile`).
pub async fn fetch_tip_lamports(tip_floor_url: &str, percentile: u8) -> Result<u64> {
    let client = reqwest::Client::new();
    let resp: serde_json::Value = client.get(tip_floor_url).send().await?.json().await?;
    tip_lamports_from_response(&resp, percentile)
}

/// Pure parse of the Tip Floor response: read `landed_tips_{percentile}th_percentile`
/// (SOL), convert to lamports, and clamp to Jito's [min, sane-max]. Network-free so
/// the percentile selection, conversion, and bounds are unit-testable. Rejects an
/// unsupported percentile, a non-finite/negative value, and an implausibly-high tip.
pub fn tip_lamports_from_response(resp: &serde_json::Value, percentile: u8) -> Result<u64> {
    if !SUPPORTED_TIP_PERCENTILES.contains(&percentile) {
        return Err(anyhow!(
            "unsupported tip percentile {percentile} (expected one of {SUPPORTED_TIP_PERCENTILES:?})"
        ));
    }
    // The endpoint returns a single-element array of SOL-denominated percentiles.
    let row = resp
        .get(0)
        .ok_or_else(|| anyhow!("empty tip floor response: {resp}"))?;
    let field = format!("landed_tips_{percentile}th_percentile");
    let sol = row[field.as_str()]
        .as_f64()
        .ok_or_else(|| anyhow!("no {field} in tip floor: {resp}"))?;

    if !sol.is_finite() || sol < 0.0 {
        return Err(anyhow!("tip floor {field} is not a valid amount: {sol}"));
    }
    // Round (not truncate) so we never under-tip by a representation error.
    let lamports = (sol * LAMPORTS_PER_SOL).round() as u64;
    if lamports > MAX_TIP_LAMPORTS {
        return Err(anyhow!(
            "tip floor {field} implausibly high ({lamports} lamports > {MAX_TIP_LAMPORTS}); refusing"
        ));
    }
    Ok(lamports.max(MIN_TIP_LAMPORTS))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floor_response() -> serde_json::Value {
        // Shape of the real Jito Tip Floor response (single-element array).
        serde_json::json!([{
            "landed_tips_25th_percentile": 0.000_000_5, // 500 lamports — BELOW the floor
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
    fn floors_below_minimum_up_to_jito_min() {
        // 25th = 0.0000005 SOL = 500 lamports, strictly below MIN -> clamped UP to MIN.
        // (The fixture is deliberately sub-minimum so the floor is actually exercised.)
        assert_eq!(
            tip_lamports_from_response(&floor_response(), 25).unwrap(),
            MIN_TIP_LAMPORTS
        );
    }

    #[test]
    fn rounds_rather_than_truncates() {
        // 0.0000019999 SOL = 1999.9 lamports -> rounds to 2000, not 1999.
        let resp = serde_json::json!([{ "landed_tips_50th_percentile": 0.000_001_999_9 }]);
        assert_eq!(tip_lamports_from_response(&resp, 50).unwrap(), 2000);
    }

    #[test]
    fn rejects_non_finite_or_negative() {
        let neg = serde_json::json!([{ "landed_tips_75th_percentile": -0.001 }]);
        assert!(tip_lamports_from_response(&neg, 75).is_err(), "negative -> Err, not floored");
        // serde_json cannot hold NaN/inf, but a non-numeric value must also Err.
        let nan = serde_json::json!([{ "landed_tips_75th_percentile": "oops" }]);
        assert!(tip_lamports_from_response(&nan, 75).is_err());
    }

    #[test]
    fn rejects_implausibly_high_tip() {
        // 1.0 SOL = 1e9 lamports, far above MAX_TIP_LAMPORTS -> refuse, don't submit.
        let huge = serde_json::json!([{ "landed_tips_95th_percentile": 1.0 }]);
        assert!(tip_lamports_from_response(&huge, 95).is_err());
    }

    #[test]
    fn rejects_unsupported_percentile() {
        assert!(tip_lamports_from_response(&floor_response(), 42).is_err());
    }
}
