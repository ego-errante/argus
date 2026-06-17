//! Leader Window detection (PLAN.md Day 5-6, ADR 0008) over gRPC.
//!
//! Queries Jito's `searcher.SearcherService/GetNextScheduledLeader` for the next
//! slot a Jito-connected validator is scheduled to lead, so the Core can time
//! Submission into that window (the spec's "Detect the correct leader window for
//! submission"). This method is gRPC-ONLY — it is not on Jito's HTTP JSON-RPC API
//! (verified against jito-labs/mev-protos) — and is served NoAuth for read calls.
//!
//! This is an OPTIMIZATION signal, never a gate: a failed/empty response logs a
//! warning and the caller submits anyway. The authoritative current-slot signal in
//! the full lifecycle is the Yellowstone slot stream (Day 3-4).

use anyhow::Result;
use std::time::Duration;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

/// Bound the connect so a flaky/throttled searcher endpoint fails fast and the
/// caller proceeds to submit without timing, rather than stalling the window.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Generated client + messages for the minimal `searcher.proto` (see build.rs).
pub mod searcher_proto {
    tonic::include_proto!("searcher");
}
use searcher_proto::{
    searcher_service_client::SearcherServiceClient, NextScheduledLeaderRequest,
    NextScheduledLeaderResponse,
};

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

/// Pure mapping: the gRPC response -> our domain struct. No network, so it is
/// unit-testable by constructing the proto message directly (cf. the
/// `*_from_proto` helpers in `streaming.rs`).
pub fn next_leader_from_proto(r: &NextScheduledLeaderResponse) -> NextLeader {
    NextLeader {
        current_slot: r.current_slot,
        next_leader_slot: r.next_leader_slot,
        next_leader_identity: r.next_leader_identity.clone(),
        next_leader_region: r.next_leader_region.clone(),
    }
}

/// Normalize a gRPC endpoint to a full URI, defaulting to `https://` when no
/// scheme is present (mirrors `streaming::grpc_builder`'s scheme handling).
fn searcher_endpoint_uri(raw: &str) -> String {
    if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    }
}

/// Connect a `SearcherServiceClient` over TLS (https) or plaintext (http). Reuses
/// the process-wide ring `CryptoProvider` installed in `main.rs`; native roots come
/// from the OS trust store (the Dockerfile installs `ca-certificates`).
async fn connect_searcher(grpc_endpoint: &str) -> Result<SearcherServiceClient<Channel>> {
    let mut endpoint =
        Endpoint::from_shared(searcher_endpoint_uri(grpc_endpoint))?.connect_timeout(CONNECT_TIMEOUT);
    if endpoint.uri().scheme_str() == Some("https") {
        endpoint = endpoint.tls_config(ClientTlsConfig::new().with_native_roots())?;
    }
    let channel = endpoint.connect().await?;
    Ok(SearcherServiceClient::new(channel))
}

/// Query Jito's SearcherService over gRPC (NoAuth) for the next scheduled Jito
/// leader. `regions` may be empty (defaults to the connected region). Optimization
/// signal only — the caller treats `Err` as "submit without leader timing".
pub async fn next_scheduled_leader(grpc_endpoint: &str, regions: &[String]) -> Result<NextLeader> {
    let mut client = connect_searcher(grpc_endpoint).await?;
    let resp = client
        .get_next_scheduled_leader(NextScheduledLeaderRequest {
            regions: regions.to_vec(),
        })
        .await?;
    Ok(next_leader_from_proto(resp.get_ref()))
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
    fn maps_next_leader_from_proto() {
        let resp = NextScheduledLeaderResponse {
            current_slot: 426_400_000,
            next_leader_slot: 426_400_003,
            next_leader_identity: "J1to1eaderIdentityPubkey".into(),
            next_leader_region: "frankfurt".into(),
        };
        let nl = next_leader_from_proto(&resp);
        assert_eq!(nl.current_slot, 426_400_000);
        assert_eq!(nl.next_leader_slot, 426_400_003);
        assert_eq!(nl.next_leader_identity, "J1to1eaderIdentityPubkey");
        assert_eq!(nl.next_leader_region, "frankfurt");
        assert_eq!(nl.slots_until_leader(), 3);
    }

    #[test]
    fn endpoint_uri_defaults_scheme_to_https() {
        assert_eq!(
            searcher_endpoint_uri("frankfurt.mainnet.block-engine.jito.wtf"),
            "https://frankfurt.mainnet.block-engine.jito.wtf"
        );
        // An explicit scheme is left untouched (plaintext gRPC stays http).
        assert_eq!(searcher_endpoint_uri("http://localhost:1003"), "http://localhost:1003");
        assert_eq!(
            searcher_endpoint_uri("https://frankfurt.mainnet.block-engine.jito.wtf"),
            "https://frankfurt.mainnet.block-engine.jito.wtf"
        );
    }
}
