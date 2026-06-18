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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use futures::StreamExt;
use tokio::sync::mpsc;
use tracing::warn;
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

// ---- Streaming resilience (Day 7-8, ADR 0009) -------------------------------
//
// A bounded channel sits between the gRPC receive task and the consumer: a slow
// consumer sheds surplus updates (counted, not silently buffered toward OOM), and
// a dropped/errored stream is reconnected with exponential backoff. The two public
// entry points (`subscribe_slots`/`track_lifecycle`) are thin wrappers over one
// generalized `resilient_subscribe` driver, so the resilience lives in one place
// rather than as a band-aid in each consumer.

/// Default bounded-channel capacity between the receive task and the consumer.
pub const DEFAULT_CHANNEL_CAP: usize = 1024;
/// Default reconnect ceiling before the driver gives up (soft signal, no panic).
pub const DEFAULT_MAX_RECONNECTS: u32 = 10;

const RECONNECT_BASE_MS: u64 = 500;
const RECONNECT_MAX_MS: u64 = 30_000;
/// Cap the backoff exponent so `1 << shift` can never overflow; 2^16 * base is
/// already far past the cap, so `.min` clamps it well before this bound bites.
const MAX_BACKOFF_SHIFT: u32 = 16;

/// Reconnect backoff for the Nth consecutive failure (1-based): `BASE * 2^(n-1)`,
/// capped at MAX. Pure — the schedule is unit-tested; the reconnect loop is not.
pub fn backoff_delay(attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(MAX_BACKOFF_SHIFT);
    let ms = RECONNECT_BASE_MS
        .saturating_mul(1u64 << shift)
        .min(RECONNECT_MAX_MS);
    Duration::from_millis(ms)
}

/// Lag/drop accounting for a resilient subscription (logged at run end). `received`
/// counts every tracked event decoded off the stream; `dropped` the subset shed
/// when the bounded channel was full; `high_water` the deepest the channel got.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StreamMetrics {
    pub reconnects: u64,
    pub received: u64,
    pub dropped: u64,
    pub high_water: usize,
}

impl StreamMetrics {
    /// Account one event handed toward the consumer. `delivered=false` means the
    /// bounded channel was full and the event was shed; `depth` is the channel
    /// occupancy at send time (feeds the high-water mark).
    pub fn record_send(&mut self, delivered: bool, depth: usize) {
        self.received += 1;
        if !delivered {
            self.dropped += 1;
        }
        if depth > self.high_water {
            self.high_water = depth;
        }
    }

    pub fn record_reconnect(&mut self) {
        self.reconnects += 1;
    }
}

/// The gRPC receive task: owns the stream, maps each update to an event `E`, and
/// `try_send`s it into the bounded channel (shedding + counting on a full channel).
/// On a dropped/errored stream it reconnects with `backoff_delay`, giving up after
/// `max_reconnects` (soft signal — never panics). Fires `on_subscribed` once, after
/// the FIRST successful subscribe (the caller submits then, so Inclusion isn't
/// missed); reconnects do not re-fire it. Returning / dropping `tx` ends the consumer.
async fn run_producer<E>(
    grpc_url: String,
    x_token: Option<String>,
    request: SubscribeRequest,
    map_update: impl Fn(UpdateOneof) -> Option<E>,
    max_reconnects: u32,
    tx: mpsc::Sender<E>,
    metrics: Arc<Mutex<StreamMetrics>>,
    mut on_subscribed: Option<Box<dyn FnOnce() + Send>>,
) {
    let mut backoff_attempt = 0u32;
    loop {
        let connect = async {
            let mut client = grpc_builder(&grpc_url, x_token.as_deref())?.connect().await?;
            let stream = client.subscribe_once(request.clone()).await?;
            anyhow::Ok(stream)
        }
        .await;

        let mut stream = match connect {
            Ok(s) => s,
            Err(e) => {
                backoff_attempt += 1;
                metrics.lock().unwrap().record_reconnect();
                if backoff_attempt > max_reconnects {
                    warn!(error = %e, max_reconnects, "stream connect failed — reconnect ceiling hit, giving up");
                    return;
                }
                warn!(attempt = backoff_attempt, error = %e, "stream connect failed — backing off");
                tokio::time::sleep(backoff_delay(backoff_attempt)).await;
                continue;
            }
        };

        // Stream is live: the server-side subscription is active the instant
        // subscribe_once returns, so submitting now can't miss Inclusion.
        if let Some(cb) = on_subscribed.take() {
            cb();
        }

        let mut got_data = false;
        while let Some(message) = stream.next().await {
            let update = match message {
                Ok(u) => u,
                Err(e) => {
                    warn!(error = %e, "stream error — reconnecting");
                    break;
                }
            };
            // Any frame (incl. ping) proves the connection healthy → fresh backoff.
            if !got_data {
                got_data = true;
                backoff_attempt = 0;
            }
            let Some(ev) = update.update_oneof.and_then(&map_update) else {
                continue; // ping/pong and untracked update kinds
            };
            match tx.try_send(ev) {
                Ok(()) => {
                    let depth = tx.max_capacity() - tx.capacity();
                    metrics.lock().unwrap().record_send(true, depth);
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    let dropped = {
                        let mut m = metrics.lock().unwrap();
                        m.record_send(false, tx.max_capacity());
                        m.dropped
                    };
                    if dropped == 1 || dropped % 256 == 0 {
                        warn!(dropped, "stream consumer lagging — bounded channel full, shedding updates");
                    }
                }
                Err(mpsc::error::TrySendError::Closed(_)) => return, // consumer done
            }
        }

        if tx.is_closed() {
            return;
        }
        // Stream ended or errored while the consumer is still listening — reconnect.
        backoff_attempt += 1;
        metrics.lock().unwrap().record_reconnect();
        if backoff_attempt > max_reconnects {
            warn!(max_reconnects, "stream reconnect ceiling hit — giving up (soft signal)");
            return;
        }
        tokio::time::sleep(backoff_delay(backoff_attempt)).await;
    }
}

/// Generalized resilient subscription: spawn the gRPC receive task (reconnect +
/// bounded-channel backpressure) and drain its events on THIS task into `on_event`
/// — so the callback may borrow non-`Send` local state, as the lifecycle consumer
/// does. Returns `StreamMetrics` once the consumer stops (callback returns `false`)
/// or the producer gives up. `E` is the consumer's decoded event type.
async fn resilient_subscribe<E>(
    grpc_url: &str,
    x_token: Option<&str>,
    request: SubscribeRequest,
    map_update: impl Fn(UpdateOneof) -> Option<E> + Send + 'static,
    on_subscribed: Option<Box<dyn FnOnce() + Send>>,
    channel_cap: usize,
    max_reconnects: u32,
    mut on_event: impl FnMut(E) -> bool,
) -> Result<StreamMetrics>
where
    E: Send + 'static,
{
    let (tx, mut rx) = mpsc::channel::<E>(channel_cap);
    let metrics = Arc::new(Mutex::new(StreamMetrics::default()));
    let producer = tokio::spawn(run_producer(
        grpc_url.to_string(),
        x_token.map(String::from),
        request,
        map_update,
        max_reconnects,
        tx,
        Arc::clone(&metrics),
        on_subscribed,
    ));

    while let Some(ev) = rx.recv().await {
        if !on_event(ev) {
            break;
        }
    }

    // The producer may be parked in `stream.next()`, so abort rather than join —
    // the metrics it accumulated live in the shared Arc, not its return value.
    producer.abort();
    let _ = producer.await;
    let m = *metrics.lock().unwrap();
    Ok(m)
}

/// Connect to a Yellowstone gRPC endpoint and stream slot commitment updates,
/// invoking `on_update` for each Processed/Confirmed/Finalized transition. The
/// callback returns `false` to stop. Resilient: reconnects with backoff and sheds
/// surplus updates on a full bounded channel (ADR 0009); returns the StreamMetrics.
pub async fn subscribe_slots(
    grpc_url: &str,
    x_token: Option<&str>,
    channel_cap: usize,
    max_reconnects: u32,
    on_update: impl FnMut(SlotUpdate) -> bool,
) -> Result<StreamMetrics> {
    resilient_subscribe(
        grpc_url,
        x_token,
        build_slots_request(),
        |u| match u {
            UpdateOneof::Slot(s) => slot_update_from_proto(&s),
            _ => None,
        },
        None,
        channel_cap,
        max_reconnects,
        on_update,
    )
    .await
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
    channel_cap: usize,
    max_reconnects: u32,
    on_subscribed: impl FnOnce() + Send + 'static,
    on_event: impl FnMut(LifecycleEvent) -> bool,
) -> Result<StreamMetrics> {
    resilient_subscribe(
        grpc_url,
        x_token,
        build_lifecycle_request(signature),
        |u| match u {
            UpdateOneof::Slot(s) => slot_update_from_proto(&s).map(|su| LifecycleEvent::Commitment {
                slot: su.slot,
                level: su.commitment,
            }),
            UpdateOneof::Transaction(tx) => tx_landing_from_proto(&tx).map(|l| LifecycleEvent::Landed {
                signature: l.signature,
                slot: l.slot,
            }),
            _ => None,
        },
        Some(Box::new(on_subscribed)),
        channel_cap,
        max_reconnects,
        on_event,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use yellowstone_grpc_proto::geyser::SubscribeUpdateTransactionInfo;

    #[test]
    fn backoff_starts_at_base_and_doubles() {
        assert_eq!(backoff_delay(1), Duration::from_millis(500));
        assert_eq!(backoff_delay(2), Duration::from_millis(1_000));
        assert_eq!(backoff_delay(3), Duration::from_millis(2_000));
        assert_eq!(backoff_delay(4), Duration::from_millis(4_000));
    }

    #[test]
    fn backoff_is_monotonic_and_caps_at_max() {
        let max = Duration::from_millis(30_000);
        let mut prev = Duration::ZERO;
        for n in 1..=40 {
            let d = backoff_delay(n);
            assert!(d >= prev, "backoff must be non-decreasing at attempt {n}");
            assert!(d <= max, "backoff must never exceed the cap at attempt {n}");
            prev = d;
        }
        assert_eq!(backoff_delay(40), max, "deep retries sit at the cap");
    }

    #[test]
    fn metrics_count_drops_and_track_high_water() {
        let mut m = StreamMetrics::default();
        for depth in [1usize, 5, 2, 9, 3] {
            m.record_send(true, depth);
        }
        m.record_send(false, 9); // one shed event on a full channel
        assert_eq!(m.received, 6, "every tracked event is counted");
        assert_eq!(m.dropped, 1, "only the shed event is a drop");
        assert_eq!(m.high_water, 9, "deepest observed occupancy");
    }

    #[test]
    fn metrics_count_reconnects() {
        let mut m = StreamMetrics::default();
        for _ in 0..4 {
            m.record_reconnect();
        }
        assert_eq!(m.reconnects, 4);
    }

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
