# Implementation Plan

Guides implementation of Argus (the Smart Transaction Stack) for the Superteam Nigeria "Advanced Infrastructure Challenge." Terminology is defined in [CONTEXT.md](../CONTEXT.md); architectural decisions in [docs/adr/](./adr/). This file is the *how* and *when*.

## Constraints

- **Prize:** 5,000 USDG (1st 2,500 / 2nd 1,500 / 3rd 1,000). ~27 submissions competing for 3 slots — build to **win**, not just complete.
- **Deadline:** 2026-06-29 11:00 UTC. **Target submit: 2026-06-28** (don't trust the cutoff to your timezone).
- **Eligibility:** Nigeria-only — verify before investing.
- **Solo, full-time, Rust-comfortable.**

## Architecture at a glance

Rust **Core** + TypeScript **Agent** over HTTP/JSON, monorepo + docker-compose (ADR 0001). Mainnet via SolInfra credits + a dedicated low-balance keypair; devnet is a chaos sandbox only (ADR 0002).

```
/core    Rust: streaming, leader window, bundles, tips, lifecycle, failure classify, remedy exec
/agent   TS:   HTTP service, OpenRouter (model-rotatable, default Sonnet 4.6), Decision Space, Reasoning Trace
/docs    arch doc source notes, ADRs, this plan
/logs    SQLite (source of truth) + JSONL export + generated Markdown Lifecycle Log
```

## Requirement → deliverable map

| Spec requirement | Where it's satisfied |
|---|---|
| Architecture doc (hosted off-repo) | Notion + Excalidraw; all required sections |
| Live slot/leader monitoring | Core: Yellowstone slot-sub + `getNextScheduledLeader` |
| Jito bundles + dynamic tips, no hardcoded | Real Jito `sendBundle` to the Block Engine (multi-region fan-out + leader-window timing) — the **scored** path (ADR 0007); Tip Floor percentile + account rotation, no hardcoded values (ADR 0005); Helius Sender kept as a keyless reliability backstop, **not** the scored path |
| Lifecycle tracking + deltas | Core: tx-sub (Inclusion) + slot-sub (Commitment Progression) (ADR 0004) |
| Failure classify (4 classes) | Core: Failure Class detection |
| Confirm via streams, not polling | ADR 0004; `getBundleStatuses` cross-check only |
| Auto-retry + blockhash refresh | Core executes Remedies chosen by Agent |
| Lifecycle Log ≥10, ≥2 failures | SQLite → JSONL + Markdown table w/ explorer links |
| AI agent owns one decision | Agent: Failure Reasoning, labeled Autonomous Retry (ADR 0003); model via OpenRouter (ADR 0006) |
| README 3 questions | Grounded in this Run's observed data — see below |
| Open source + setup | docker-compose / Makefile, top-level README |

## Build sequence (riskiest slice first)

| Days | Milestone | Done when |
|---|---|---|
| 1–2 (Jun 13–14) | **Tracer bullet** | SolInfra onboarded, keypair funded; Core submits ONE tx and **it lands on Solscan** ✅ (liveness proven via Helius Sender — slot 426438873). **TOP OPEN RISK — RESOLVED (Jun 17):** a real Jito **bundle** now lands. A Jito `x-jito-auth` UUID (2 req/s/region) was provisioned and wired via `JITO_AUTH_UUID`; with it the Block Engine forwards our bundles — `getInflightBundleStatuses` flipped from all-region `Invalid` to `Landed`. The **production scored path** landed on attempt 1 on the **dynamic floor tip (5000 lamports), no Sender backstop** — slot 427028288, bundle `d33b83c8…e454777` (ADR 0007). Slot streaming still to wire. |
| 3–4 | **Lifecycle tracking** ✅ | DONE (Jun 17): Inclusion (tx stream) + Commitment Progression (slot stream), multiplexed over one Yellowstone subscription, write submitted/landed/processed/confirmed/finalized + deltas to SQLite — proven live (slot 427131976, confirmed→finalized 11.85s, bundle `ffc29145…264047ae`). Pluggable instruction builder (`bundle::build_bundle_with_payload`; default = self-transfer + Memo nonce). 28 unit tests green. |
| 5–6 | **Dynamic tips + leader window** | Tip Floor percentile + account rotation; `getNextScheduledLeader` timing; **all hardcoded values deleted** |
| 7–8 | **Failures + retry + backpressure** | 3 deterministic injections classify correctly; bounded-channel backpressure + reconnection with logged lag/drop metrics; Remedy execution hooks |
| 9–10 | **Agent** | TS service over OpenRouter (default Sonnet 4.6, env-rotatable), structured `{remedy, rationale, confidence}` via `submit_decision` tool, Core→Agent HTTP, multi-Remedy, Reasoning Traces (`message.reasoning`) logged with the serving model. **CUT-LINE: Agent reasons over real Failures by end of Day 10.** |
| 11 | **Run** | ≥10 Submissions incl. injected failures; Lifecycle Log table w/ explorer links generated |
| 12–13 | **Arch doc** | Notion + Excalidraw, every required section |
| 14 | **README** | 3 answers grounded in observed data; setup instructions |
| 15 | **Polish + buffer** | Logs reproduce; stretch only if ahead |
| 16 (Jun 28) | **Submit early** | — |

**Cut order under pressure (never cut the six required deliverables):** live dashboard → self-computed leader-schedule cross-check → 4th deterministic failure class (keep 3) → SPL/program payloads.

## Failure-injection matrix

| Failure Class | How induced | Determinism | Expected Agent Remedy |
|---|---|---|---|
| Expired Blockhash | Sign against a stale blockhash (headline fault) | Deterministic | refresh blockhash |
| Compute Exceeded | CU limit below instruction need | Deterministic | raise CU limit |
| Bundle Failure | Include a failing instruction | Deterministic | abort / rebuild |
| Fee Too Low | Tip below live floor under contention | Probabilistic (best-effort) | bump Tip |

Also capture any Organic Failures. The point is *variation*: different Failure Classes → different Remedies = the proof the Agent reasons (ADR 0003).

> **Model-rotation constraint (ADR 0006).** The Reasoning Trace is the visible-reasoning evidence the judges score, and it comes back on `message.reasoning` only for reasoning-capable models. Rotating to a non-reasoning model yields an *empty* trace and silently weakens that evidence. So: keep the scored Run (Day 11) on reasoning-capable models only, and rely on the `model` field logged with each decision to confirm every scored decision actually carried a trace. Rotate freely for cost/latency experiments, but never let a traceless model produce a decision that ends up in the submitted Lifecycle Log.

## README answers — must be backed by this Run's numbers

1. **processed→confirmed delta = ?** Consensus/vote-propagation latency (≥2/3 stake voting), i.e. *consensus health*, not inclusion speed. Quote observed deltas + conditions.
2. **Why not `finalized` blockhash for time-sensitive tx?** Blockhash valid ~150 slots (~60–90s); a finalized blockhash is already ~31+ slots old, burning the validity window → expiry risk. Use freshest viable. Back with injected expiry data.
3. **Jito leader skips their slot?** Bundle is slot-specific + all-or-nothing → not included, **not** auto-forwarded; **no Tip charged** (tips pay only on landing); must resubmit to the next Leader Window with a fresh blockhash. Back with leader-window/retry logs.

## Open items before Day 1

- [ ] Confirm Nigeria eligibility.
- [ ] Claim SolInfra credits (RPC + Yellowstone gRPC); confirm mainnet endpoints + auth.
- [ ] Create + fund dedicated mainnet keypair (~0.05 SOL); ensure it's gitignored.
- [ ] Decide default payload instruction (self-transfer + Memo is the default; builder is pluggable).
- [ ] Get an OpenRouter API key (`OPENROUTER_API_KEY`); confirm the default model slug + any rotation fallbacks (ADR 0006).
