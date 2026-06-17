# Jito bundles are the scored submission path; Helius Sender is a reliability backstop

Argus's scored deliverable — the Lifecycle Log of **≥10 submissions (incl. ≥2 failures)** — is produced by constructing and submitting **real Jito bundles** via `sendBundle` to the Jito Block Engine, with multi-region fan-out and leader-window targeting. **Helius Sender** (`sender.helius-rpc.com/fast`) is retained as a keyless reliability backstop and a development-liveness path, but it submits a *plain transaction* (routed partly through Jito's auction), **not** a Jito bundle, so it cannot satisfy the bounty's Jito-bundle requirement on its own.

## Why

This **reverses an earlier draft of this ADR**, which treated "routes through Jito's auction" as satisfying the spec. Re-reading the live listing makes clear the spec demands *real Jito bundles*, not merely Jito-routed transactions:

- *"Construct and submit **Jito bundles**"* / *"**Real Jito bundle construction**"*
- Lifecycle Log = *"**10 real bundle submissions**"* with bundle **tip amounts** and a *"**Bundle failure**"* failure class
- README **Q3** is about *"your **bundle**"* when *"the Jito leader skips their slot"*
- Judging weights *"**Proper use of Jito** … No hardcoded shortcuts,"* and judges *"**cross-reference slot numbers using Solana explorers**."*

A Helius Sender submission returns a transaction **signature**, not a **bundle id**; it can't be tracked via `getBundleStatuses` / `getInflightBundleStatuses`, and on an explorer it is indistinguishable from an ordinary tip-carrying transaction. Scoring the ≥10 log on Sender would forfeit the largest judged dimension (Depth of Integration).

The original blocker remains real but is the *wrong product*, not the *wrong approach*: the unauthenticated public Block Engine **accepted** bundles (6–7/8 regions returned ids) yet did **not forward** them during congestion (`getInflightBundleStatuses` → `Invalid`; `-32097` "globally rate limited"). Construction is provably correct (unit tests + on-chain simulation). The fix is **headroom + timing**, not a different submission product:

- **Leader-window targeting** (a spec requirement anyway — *"Detect the correct leader window for submission"*) concentrates submissions into the window where forwarding actually lands.
- **An authed / credited relay**: SolInfra's bundle endpoint — the bounty's *official infra sponsor* (*"Premium Solana infrastructure tooling,"* up to $20k credits) — is the intended route; a Jito `x-jito-auth` UUID (already wired via `JITO_AUTH_UUID`) is the free alternative.

We only need **~10 landings over the Run**, not sustained throughput — patience + leader timing + one authed path is sufficient.

## Consequences

- The scored Lifecycle Log is produced by `bundle::submit_all_regions` (real `sendBundle`), tip = dynamic Tip Floor (ADR 0005), tip account = a published **Jito** tip account (not a Sender tip account), confirmation cross-checked via bundle status **+** stream subscription (ADR 0004; spec: *"RPC polling alone is not sufficient"*).
- Helius Sender (`core/src/sender.rs`) stays as a keyless backstop / liveness probe and is explicitly **not** the scored path. The Core's primary/fallback ordering must be **flipped to Jito-first** for the scored Run — the current tracer runs Sender-first only to prove end-to-end liveness.
- The two required failure cases are cheap: a non-forwarded / rejected bundle (organic *Bundle failure*) and an injected expired-blockhash bundle.
- ~~**Landing real bundles is now the top open risk**~~ — **RESOLVED (2026-06-17)** by the Jito `x-jito-auth` UUID (see Resolution below). SolInfra `sendBundle` drops off the critical path — a redundant second relay if ever needed.

### Empirical finding (2026-06-15) — keyless public Jito ruled out for landing

Two live mainnet runs settled it. The unauthenticated public Block Engine **accepts** our bundles (6–7/8 regions return the content-addressed bundle id) but `getInflightBundleStatuses` reports **`Invalid` on all 8 regions** — i.e. the bundle never enters the auction system. A controlled probe (`ARGUS_DIAG`) ruled the tip out as the variable: a **0.001 SOL tip (≈8× the live floor)** stayed `Invalid` across all regions for ~3 minutes; London additionally returned `-32097` *"Network congested. Endpoint is globally rate limited."* Separately, `getNextScheduledLeader` (leader-window timing) is **HTTP 404** — it's a gRPC-only Searcher method, so there is no JSON-RPC HTTP path to time the window. **Conclusion: the free tier won't forward our bundles regardless of tip or timing; an authed/credited relay is required to land real bundles.** The architecture is done and correct — only the *endpoint* is missing (auth is wired via `JITO_AUTH_UUID`; a SolInfra `sendBundle` is just a different base URL + key).

### Resolution (2026-06-17) — authed Jito UUID lands real bundles

A Jito `x-jito-auth` JSON-RPC UUID (rate limit **2 req/s per region**) was provisioned via the Jito Discord and wired through `JITO_AUTH_UUID` (already read by `config.rs`; both the scored path and the diagnostic pass it to `send_bundle`). The reversal is total and on-chain:

- **Diagnostic** (`ARGUS_DIAG`, 0.001 SOL override tip): `getInflightBundleStatuses` went from all-region `Invalid` to `{Landed: 6, Pending: 2}`; landed **slot 427028078**, bundle `f1d2e06a…995cd0df`. Six regions returned `-32602 "already processed"` — the tx was on-chain before they could ack it.
- **Production scored path** (Jito-first, no backstop, **dynamic floor tip 5000 lamports**): landed on attempt 1, **slot 427028288**, bundle `d33b83c8…e454777` — proving the no-hardcoded-tip path lands a trackable bundle on its own.

The empirical finding above stands as the *why* (the unauthed tier never forwards); the missing piece was exactly the predicted authed relay. **The top open risk is closed** — bundles forward, land, and are trackable by bundle id for the Lifecycle Log. Tip is confirmed irrelevant to forwarding (5000 lamports authed lands; 0.001 SOL unauthed did not).

## Considered options

- **Helius Sender as the scored path** — rejected: lands transactions but produces no Jito bundle; fails *"Real Jito bundle construction,"* bundle-status tracking, *"Bundle failure"* classification, and on-chain verification. Kept only as a backstop.
- **Public Jito Block Engine, multi-region fan-out (free)** — **ruled out empirically as the scored path** (see finding above): accepts but never forwards on the unauthed tier, independent of tip. Still useful for demonstrating *Bundle failure* (an `Invalid`/non-forwarded bundle is a legitimate logged failure case).
- **SolInfra `sendBundle` (sponsor infra / credits)** — preferred once enabled; returned *"Method not found"* on our RPC-scoped key, so it needs the correct key scope / access (request in flight). Best alignment with *"Proper use of Jito infrastructure."*
- **Jito `x-jito-auth` UUID (direct)** — **ADOPTED (2026-06-17).** Provisioned at 2 req/s/region, wired via `JITO_AUTH_UUID`; lands real bundles on the dynamic floor tip (see Resolution). This is the scored relay.
- **Paid relay (QuickNode Lil'JIT ~$138/mo)** — reliable fallback if free paths can't land the ≥10 before the deadline; reserved.
