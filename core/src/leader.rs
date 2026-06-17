//! Leader Window detection (PLAN.md Day 5-6, promoted for ADR 0007's Jito-first path).
//!
//! Queries the Jito Block Engine `getNextScheduledLeader` for the next slot a
//! Jito-connected validator is scheduled to lead, so the Core can time Submission
//! into that window (the spec's "Detect the correct leader window for submission").
//!
//! This is an OPTIMIZATION signal, never a gate: a failed/empty response logs a
//! warning and the caller submits anyway. The authoritative current-slot signal in
//! the full lifecycle is the Yellowstone slot stream (Day 3-4); this RPC query is
//! enough to align submission with the next Jito leader.

use anyhow::{anyhow, Result};
use serde_json::Value;

/// The next slot a Jito-connected validator is scheduled to lead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextLeader {
    pub current_slot: u64,
    pub next_leader_slot: u64,
    pub next_leader_identity: String,
    pub next_leader_region: String,
}

impl NextLeader {
    /// Slots until the next Jito leader (0 if that slot is current or already past).
    pub fn slots_until_leader(&self) -> u64 {
        self.next_leader_slot.saturating_sub(self.current_slot)
    }
}

/// Candidate sub-paths for the method. Jito hosts `getNextScheduledLeader` at its
/// own path; we also try the `bundles` endpoint as a fallback so a routing change
/// (or our uncertainty about the exact path) degrades to "submit without timing"
/// rather than a hard failure.
const LEADER_PATHS: [&str; 2] = ["getNextScheduledLeader", "bundles"];

/// Parse a `getNextScheduledLeader` JSON-RPC response into a `NextLeader`.
/// Tolerates Jito's camelCase wire field names; identity/region are best-effort.
pub fn parse_next_leader(resp: &Value) -> Result<NextLeader> {
    let r = &resp["result"];
    if r.is_null() {
        return Err(anyhow!("getNextScheduledLeader: no result ({resp})"));
    }
    let current_slot = r["currentSlot"]
        .as_u64()
        .ok_or_else(|| anyhow!("getNextScheduledLeader: no currentSlot ({resp})"))?;
    let next_leader_slot = r["nextLeaderSlot"]
        .as_u64()
        .ok_or_else(|| anyhow!("getNextScheduledLeader: no nextLeaderSlot ({resp})"))?;
    Ok(NextLeader {
        current_slot,
        next_leader_slot,
        next_leader_identity: r["nextLeaderIdentity"].as_str().unwrap_or("").to_string(),
        next_leader_region: r["nextLeaderRegion"].as_str().unwrap_or("").to_string(),
    })
}

/// Query the Block Engine for the next scheduled Jito leader. `auth_uuid` is the
/// optional `x-jito-auth` UUID (read calls don't require it). Tries the dedicated
/// path, then the bundles path; returns the first usable result.
pub async fn next_scheduled_leader(
    block_engine_base: &str,
    auth_uuid: Option<&str>,
) -> Result<NextLeader> {
    let base = format!("{}/api/v1", block_engine_base.trim_end_matches('/'));
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getNextScheduledLeader",
        "params": [],
    });
    let client = reqwest::Client::new();
    let mut last_err = anyhow!("no leader endpoint tried");

    for path in LEADER_PATHS {
        let url = format!("{base}/{path}");
        let mut req = client.post(&url).json(&body);
        if let Some(uuid) = auth_uuid {
            req = req.header("x-jito-auth", uuid);
        }
        match req.send().await {
            Ok(http) => {
                let resp: Value = match http.json().await {
                    Ok(v) => v,
                    Err(e) => {
                        last_err = anyhow!("{url}: non-JSON response: {e}");
                        continue;
                    }
                };
                // Method-not-found at this path -> try the next candidate.
                if resp["error"]["code"].as_i64() == Some(-32601) {
                    last_err = anyhow!("{url}: method not found");
                    continue;
                }
                match parse_next_leader(&resp) {
                    Ok(nl) => return Ok(nl),
                    Err(e) => {
                        last_err = e;
                        continue;
                    }
                }
            }
            Err(e) => {
                last_err = anyhow!("{url}: {e}");
                continue;
            }
        }
    }
    Err(last_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_until_leader_counts_forward() {
        let nl = NextLeader {
            current_slot: 100,
            next_leader_slot: 104,
            next_leader_identity: "id".into(),
            next_leader_region: "ny".into(),
        };
        assert_eq!(nl.slots_until_leader(), 4);
    }

    #[test]
    fn slots_until_leader_saturates_when_already_passed() {
        let nl = NextLeader {
            current_slot: 110,
            next_leader_slot: 104,
            next_leader_identity: "id".into(),
            next_leader_region: "ny".into(),
        };
        assert_eq!(nl.slots_until_leader(), 0, "never report a negative window");
    }

    #[test]
    fn parses_jito_response() {
        let resp = serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {
                "currentSlot": 426400000u64,
                "nextLeaderSlot": 426400003u64,
                "nextLeaderIdentity": "J1to1eaderIdentityPubkey",
                "nextLeaderRegion": "frankfurt"
            }
        });
        let nl = parse_next_leader(&resp).expect("parse");
        assert_eq!(nl.current_slot, 426_400_000);
        assert_eq!(nl.next_leader_slot, 426_400_003);
        assert_eq!(nl.next_leader_region, "frankfurt");
        assert_eq!(nl.slots_until_leader(), 3);
    }

    #[test]
    fn errors_when_result_missing() {
        let resp = serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "error": { "code": -32601, "message": "Method not found" }
        });
        assert!(parse_next_leader(&resp).is_err(), "no result -> Err, not a panic");
    }
}
