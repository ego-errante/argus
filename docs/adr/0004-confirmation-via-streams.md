# Confirmation via two Yellowstone subscriptions; bundle-status RPC is cross-check only

Landing and Commitment Progression are tracked with two distinct Yellowstone streams: a **transaction subscription** (filtered by fee-payer account) to detect Inclusion and capture the landing slot, and a **slot subscription** to follow that slot through Processed → Confirmed → Finalized. Jito's `getBundleStatuses` RPC is used only as a secondary sanity cross-check, never as the source of truth.

## Why

The spec states "RPC polling alone is not sufficient." Beyond compliance, the two-subscription model is architecturally correct: Inclusion and Commitment are different questions answered by different streams (people routinely conflate them). It also yields the first-hand processed→confirmed delta data the README's network-health question requires.

## Consequences

Inclusion detection depends on matching the payload's unique Memo nonce / fee-payer in the transaction stream. If a provider supports signature-level transaction filters, that is preferred; otherwise the fee-payer filter plus in-stream nonce match is the fallback.

## Realization (2026-06-17)

The two questions are answered by two **filters multiplexed over one Yellowstone subscription**, not two separate connections: `streaming::build_lifecycle_request` carries a slot filter (`filter_by_commitment: false`, so we see every commitment level) AND a transaction filter narrowed to our **exact signature** — the preferred signature-level filter, so Inclusion needs no in-stream nonce scan. One stream, one consumer loop (`track_lifecycle`) dispatching `Slot` vs `Transaction` updates; the conceptual separation (Inclusion vs Commitment) is preserved while keeping a single connection and a clean in-process join (landed slot → its commitment timestamps). The subscription is opened **before** the bundle is submitted so Inclusion is never missed. Stage times are stamped first-observation-wins into SQLite (`processed_at/confirmed_at/finalized_at`); the Lifecycle Log's deltas are derived from those timestamps. Bundle-status RPC remains a cross-check only. Reconnection / bounded-channel backpressure are deferred to Days 7–8.

## Realization (2026-06-18)

The deferred reconnection / bounded-channel backpressure landed in Day 7-8 as a generalized resilient driver (`streaming::resilient_subscribe`) that both `subscribe_slots` and `track_lifecycle` delegate to — see ADR 0009. `track_lifecycle` keeps the same external shape (the `on_subscribed`-then-submit ordering is preserved; `on_subscribed` fires once after the first successful subscribe), now over a reconnecting, backpressured stream that reports `StreamMetrics`.
