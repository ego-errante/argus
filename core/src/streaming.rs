//! Yellowstone gRPC streaming (ADR 0004).
//!
//! Two subscriptions, two questions:
//!   - slot subscription      -> Commitment Progression (Processed/Confirmed/Finalized)
//!   - transaction subscription (fee-payer filter, match Memo nonce) -> Inclusion / landing slot
//!
//! Backpressure via bounded channels; reconnection with logged lag/drop metrics
//! (the one hard required feature — PLAN.md Days 3-4 and 7-8). RPC polling alone
//! is NOT sufficient; getBundleStatuses is a cross-check only.
//!
//! This module starts with the slot subscription. Yellowstone reports a slot's
//! progress as a `SlotStatus`; only three of its variants are Solana commitment
//! levels (SlotProcessed/Confirmed/Finalized) — the rest (FirstShredReceived,
//! Completed, CreatedBank, Dead) are intra-slot signals we don't track for the
//! Commitment Progression deltas. `Commitment::from_slot_status` is that filter.

use std::collections::HashMap;

use anyhow::Result;
use futures::StreamExt;
use yellowstone_grpc_client::{ClientTlsConfig, GeyserGrpcBuilder, GeyserGrpcClient};
use yellowstone_grpc_proto::geyser::subscribe_update::UpdateOneof;
use yellowstone_grpc_proto::geyser::{
    CommitmentLevel, SlotStatus, SubscribeRequest, SubscribeRequestFilterSlots,
    SubscribeRequestFilterTransactions, SubscribeUpdateSlot, SubscribeUpdateTransaction,
};

/// A Solana commitment level (CONTEXT.md). The three stages a landed slot moves
/// through; the source of the Lifecycle Log's latency deltas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Commitment {
    Processed,
    Confirmed,
    Finalized,
}

impl Commitment {
    /// Map a Yellowstone `SlotStatus` to a commitment level. Returns `None` for
    /// the non-commitment statuses (FirstShredReceived/Completed/CreatedBank/Dead),
    /// which carry no Commitment Progression meaning for us.
    pub fn from_slot_status(status: SlotStatus) -> Option<Commitment> {
        match status {
            SlotStatus::SlotProcessed => Some(Commitment::Processed),
            SlotStatus::SlotConfirmed => Some(Commitment::Confirmed),
            SlotStatus::SlotFinalized => Some(Commitment::Finalized),
            _ => None,
        }
    }
}

/// One slot-commitment transition observed on the slot stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotUpdate {
    pub slot: u64,
    pub parent: Option<u64>,
    pub commitment: Commitment,
}

/// Convert a raw Yellowstone slot update into a `SlotUpdate`, or `None` if it's a
/// non-commitment status we don't track. `status()` is prost's getter that decodes
/// the wire `i32` into a `SlotStatus`.
pub fn slot_update_from_proto(s: &SubscribeUpdateSlot) -> Option<SlotUpdate> {
    let commitment = Commitment::from_slot_status(s.status())?;
    Some(SlotUpdate {
        slot: s.slot,
        parent: s.parent,
        commitment,
    })
}

/// The key Yellowstone echoes back in each update's `filters` for our slot sub.
pub const SLOTS_FILTER: &str = "argus_slots";

/// Build the `SubscribeRequest` for the slot subscription. `filter_by_commitment:
/// false` is deliberate — we want a slot update at EVERY commitment level so we can
/// time Processed -> Confirmed -> Finalized, not just one level.
pub fn build_slots_request() -> SubscribeRequest {
    let mut slots = HashMap::new();
    slots.insert(
        SLOTS_FILTER.to_string(),
        SubscribeRequestFilterSlots {
            filter_by_commitment: Some(false),
            ..Default::default()
        },
    );
    SubscribeRequest {
        slots,
        commitment: Some(CommitmentLevel::Processed as i32),
        ..Default::default()
    }
}

/// Connect to a Yellowstone gRPC endpoint and stream slot commitment updates,
/// invoking `on_update` for each Processed/Confirmed/Finalized transition. The
/// callback returns `false` to stop the stream (the read-only tracer's exit).
///
/// Read-only: we use `subscribe_once` (no request sink). Reconnection, ping/pong
/// keepalive, and bounded-channel backpressure are the Day 7-8 hardening; a short
/// observation window does not need them.
pub async fn subscribe_slots(
    grpc_url: &str,
    x_token: Option<&str>,
    mut on_update: impl FnMut(SlotUpdate) -> bool,
) -> Result<()> {
    let mut client = grpc_builder(grpc_url, x_token)?.connect().await?;

    let mut stream = client.subscribe_once(build_slots_request()).await?;
    while let Some(message) = stream.next().await {
        let update = message?;
        if let Some(UpdateOneof::Slot(slot)) = update.update_oneof {
            if let Some(su) = slot_update_from_proto(&slot) {
                if !on_update(su) {
                    break;
                }
            }
        }
        // Ping/Pong and other update kinds are ignored by the read-only tracer.
    }
    Ok(())
}

/// Normalize a gRPC endpoint to a full URI: managed gRPC (SolInfra/Yellowstone,
/// Jito searcher) hands out a bare host[:port], but tonic's URI parser needs a
/// scheme, so default to TLS (https) when none is supplied. An explicit scheme
/// (incl. `http://` for plaintext gRPC) is left untouched. Shared by both gRPC
/// clients (this builder and `leader::connect_searcher`) so the rule lives once.
pub(crate) fn normalize_grpc_endpoint(raw: &str) -> String {
    if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    }
}

/// Build a client builder for a Yellowstone endpoint.
fn grpc_builder(grpc_url: &str, x_token: Option<&str>) -> Result<GeyserGrpcBuilder> {
    let endpoint = normalize_grpc_endpoint(grpc_url);
    let mut builder = GeyserGrpcClient::build_from_shared(endpoint.clone())?;
    if let Some(token) = x_token {
        builder = builder.x_token(Some(token))?;
    }
    // TLS for https endpoints; plaintext gRPC (http://host:port) skips it.
    if endpoint.starts_with("https") {
        builder = builder.tls_config(ClientTlsConfig::new().with_native_roots())?;
    }
    Ok(builder)
}

/// Inclusion as seen on the transaction stream: our signature landed in a slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxLanding {
    pub signature: String,
    pub slot: u64,
}

/// Extract `(signature, slot)` from a Yellowstone transaction update, or `None` if
/// it carries no transaction payload. The signature is the bs58 of the wire bytes.
pub fn tx_landing_from_proto(u: &SubscribeUpdateTransaction) -> Option<TxLanding> {
    let info = u.transaction.as_ref()?;
    Some(TxLanding {
        signature: bs58::encode(&info.signature).into_string(),
        slot: u.slot,
    })
}

/// The key Yellowstone echoes back for our transaction filter.
pub const TX_FILTER: &str = "argus_tx";

/// Build the combined lifecycle `SubscribeRequest`: the slot filter (Commitment
/// Progression, all levels) AND a transaction filter narrowed to our exact
/// signature (Inclusion) — ADR 0004's two questions over one multiplexed stream.
pub fn build_lifecycle_request(signature: &str) -> SubscribeRequest {
    let mut transactions = HashMap::new();
    transactions.insert(
        TX_FILTER.to_string(),
        SubscribeRequestFilterTransactions {
            vote: Some(false),
            failed: Some(false),
            signature: Some(signature.to_string()),
            ..Default::default()
        },
    );
    SubscribeRequest {
        transactions,
        ..build_slots_request()
    }
}

/// A lifecycle event off the combined stream — either Inclusion (Landed) or one
/// Commitment Progression transition for some slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleEvent {
    Landed { signature: String, slot: u64 },
    Commitment { slot: u64, level: Commitment },
}

/// Open the combined lifecycle subscription for `signature`, invoke `on_subscribed`
/// once the stream is live (the caller submits the bundle THEN, so Inclusion isn't
/// missed), then dispatch each `LifecycleEvent` to `on_event`. The callback returns
/// `false` to stop (the caller stops once the landed slot finalizes).
pub async fn track_lifecycle(
    grpc_url: &str,
    x_token: Option<&str>,
    signature: &str,
    on_subscribed: impl FnOnce(),
    mut on_event: impl FnMut(LifecycleEvent) -> bool,
) -> Result<()> {
    let mut client = grpc_builder(grpc_url, x_token)?.connect().await?;
    let mut stream = client
        .subscribe_once(build_lifecycle_request(signature))
        .await?;
    on_subscribed();
    while let Some(message) = stream.next().await {
        let update = message?;
        match update.update_oneof {
            Some(UpdateOneof::Slot(slot)) => {
                if let Some(su) = slot_update_from_proto(&slot) {
                    if !on_event(LifecycleEvent::Commitment {
                        slot: su.slot,
                        level: su.commitment,
                    }) {
                        break;
                    }
                }
            }
            Some(UpdateOneof::Transaction(tx)) => {
                if let Some(l) = tx_landing_from_proto(&tx) {
                    if !on_event(LifecycleEvent::Landed {
                        signature: l.signature,
                        slot: l.slot,
                    }) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use yellowstone_grpc_proto::geyser::SubscribeUpdateTransactionInfo;

    #[test]
    fn normalize_grpc_endpoint_defaults_scheme_to_https() {
        // Bare host (SolInfra/Jito searcher style) -> https; explicit scheme kept.
        assert_eq!(
            normalize_grpc_endpoint("frankfurt.mainnet.block-engine.jito.wtf"),
            "https://frankfurt.mainnet.block-engine.jito.wtf"
        );
        assert_eq!(normalize_grpc_endpoint("http://localhost:1003"), "http://localhost:1003");
        assert_eq!(
            normalize_grpc_endpoint("https://fra.grpc.solinfra.dev:443"),
            "https://fra.grpc.solinfra.dev:443"
        );
    }

    #[test]
    fn maps_the_three_commitment_statuses() {
        assert_eq!(
            Commitment::from_slot_status(SlotStatus::SlotProcessed),
            Some(Commitment::Processed)
        );
        assert_eq!(
            Commitment::from_slot_status(SlotStatus::SlotConfirmed),
            Some(Commitment::Confirmed)
        );
        assert_eq!(
            Commitment::from_slot_status(SlotStatus::SlotFinalized),
            Some(Commitment::Finalized)
        );
    }

    #[test]
    fn ignores_non_commitment_statuses() {
        for s in [
            SlotStatus::SlotFirstShredReceived,
            SlotStatus::SlotCompleted,
            SlotStatus::SlotCreatedBank,
            SlotStatus::SlotDead,
        ] {
            assert_eq!(
                Commitment::from_slot_status(s),
                None,
                "{s:?} is not a commitment level"
            );
        }
    }

    #[test]
    fn builds_slot_update_from_proto_confirmed() {
        let raw = SubscribeUpdateSlot {
            slot: 427_028_288,
            parent: Some(427_028_287),
            status: SlotStatus::SlotConfirmed as i32,
            ..Default::default()
        };
        let up = slot_update_from_proto(&raw).expect("confirmed maps to a SlotUpdate");
        assert_eq!(
            up,
            SlotUpdate {
                slot: 427_028_288,
                parent: Some(427_028_287),
                commitment: Commitment::Confirmed,
            }
        );
    }

    #[test]
    fn drops_non_commitment_proto_update() {
        let raw = SubscribeUpdateSlot {
            slot: 1,
            parent: None,
            status: SlotStatus::SlotFirstShredReceived as i32,
            ..Default::default()
        };
        assert!(slot_update_from_proto(&raw).is_none());
    }

    #[test]
    fn request_subscribes_to_one_slot_filter_all_commitments() {
        let req = build_slots_request();
        assert_eq!(req.slots.len(), 1, "exactly one slots filter");
        let f = req.slots.get(SLOTS_FILTER).expect("our filter key present");
        assert_eq!(
            f.filter_by_commitment,
            Some(false),
            "must receive all commitment levels for progression"
        );
    }

    #[test]
    fn lifecycle_request_carries_slot_and_signature_filters() {
        let req = build_lifecycle_request("sigABC");
        assert_eq!(req.slots.len(), 1, "slot filter (Commitment Progression)");
        assert_eq!(req.transactions.len(), 1, "transaction filter (Inclusion)");
        let tx = req.transactions.get(TX_FILTER).expect("tx filter present");
        assert_eq!(tx.signature.as_deref(), Some("sigABC"), "filtered to our exact signature");
        assert_eq!(tx.vote, Some(false));
        assert_eq!(tx.failed, Some(false));
    }

    #[test]
    fn tx_landing_extracts_signature_and_slot() {
        let sig_bytes = vec![7u8; 64];
        let expected = bs58::encode(&sig_bytes).into_string();
        let update = SubscribeUpdateTransaction {
            slot: 427_000_111,
            transaction: Some(SubscribeUpdateTransactionInfo {
                signature: sig_bytes,
                ..Default::default()
            }),
            ..Default::default()
        };
        let l = tx_landing_from_proto(&update).expect("update has a transaction");
        assert_eq!(l.slot, 427_000_111);
        assert_eq!(l.signature, expected);
    }

    #[test]
    fn tx_landing_none_without_transaction() {
        let update = SubscribeUpdateTransaction {
            slot: 1,
            transaction: None,
            ..Default::default()
        };
        assert!(tx_landing_from_proto(&update).is_none());
    }
}
