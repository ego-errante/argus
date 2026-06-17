# Leader-window timing via a minimal gRPC SearcherService client (NoAuth)

The Core detects the next Jito leader window by calling `searcher.SearcherService/GetNextScheduledLeader` over **gRPC**, NoAuth, against a regional Block Engine host (default `frankfurt.mainnet.block-engine.jito.wtf`). The client is generated at build time from a **self-contained minimal `searcher.proto`** (only the read-only RPCs we use), not from a Jito SDK crate. Leader timing is a soft optimization signal, never a submission gate (per `leader.rs` / ADR 0007).

## Why

`getNextScheduledLeader` is **not** on Jito's HTTP JSON-RPC API — verified against `jito-labs/mev-protos` (`json_rpc/http.md` exposes only `sendBundle`/`getBundleStatuses`/`getTipAccounts`). It exists only on the gRPC SearcherService. The earlier HTTP attempt therefore always 404'd and silently degraded to "submit without timing." gRPC is the only transport that returns real data, and the read-only searcher methods are served NoAuth (no whitelist / auth-challenge flow — confirmed by the Jito Go SDK's `NewNoAuth` and proven live: a probe returned a live `current_slot`/region with no auth header).

## Considered options

- **Add `jito-protos` / `jito-searcher-client`** — rejected. They drag in a conflicting `solana-sdk`, threatening the hard-won single-`solana-sdk` / single-`tonic 0.14.6` coherence with yellowstone.
- **Vendor a minimal `searcher.proto` + `tonic-prost-build`** — adopted. Wire-compatible (package/service/method names + field tags match Jito's); uses only primitive fields; **zero new crates** (`tonic`/`prost`/`tonic-prost-build`/`protoc-bin-vendored` already resolve in the tree via yellowstone). `protoc` is the vendored binary, so the docker build needs no system `protoc`.
- **Self-compute the schedule via Solana RPC `getSlotLeaders`** — kept as a documented fallback only. It yields the leader identity per slot but not Jito-enabled/region info, a weaker non-Jito-specific signal. Reachable today on the SolInfra relay (`fra.rpc.solinfra.dev`), which supports `getSlotLeaders`/`getLeaderSchedule`.

## Consequences

The NoAuth searcher endpoint is **best-effort**: rapid fresh connections are intermittently refused ("transport error"). The soft-signal design absorbs this — observed live when the scored path landed a bundle despite a leader-call transport error. A 2s `connect_timeout` bounds a flaky connect so it fails fast instead of stalling the submission window (we observed a ~7s hang before adding it). Across the scored path's three retry attempts (each a fresh leader call), at least one usually succeeds. `getNextScheduledLeader` is region-aware; we default to Frankfurt and pass empty regions (the connected region).
