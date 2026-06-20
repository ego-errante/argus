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
| Failure classify (4 classes) | Core: the deterministic **baseline** taxonomy — the comparison the Agent's Diagnosis is measured against (ADR 0012), no longer the Agent's input |
| Confirm via streams, not polling | ADR 0004; `getBundleStatuses` cross-check only |
| Auto-retry + blockhash refresh | Core executes Remedies chosen by Agent |
| Lifecycle Log ≥10, ≥2 failures | SQLite → JSONL + Markdown table w/ explorer links |
| AI agent owns one decision | Agent owns **Failure Diagnosis over the unbounded program-error tail** (ADR 0012, supersedes 0003): it reads the RAW failure surface (failing program + structured `instruction_error` + program logs), NOT the 4-class verdict, and returns an open-ended cause + a Triage + the Remedy — the genuinely-reasoned decision a `match` can't replicate (legible ⟹ enumerable ⟹ lookup-replicable is exactly what we moved *upstream* of). Model via OpenRouter (ADR 0006) |
| README 3 questions | Grounded in this Run's observed data — see below |
| Open source + setup | docker-compose / Makefile, top-level README |

## Build sequence (riskiest slice first)

| Days | Milestone | Done when |
|---|---|---|
| 1–2 (Jun 13–14) | **Tracer bullet** | SolInfra onboarded, keypair funded; Core submits ONE tx and **it lands on Solscan** ✅ (liveness proven via Helius Sender — slot 426438873). **TOP OPEN RISK — RESOLVED (Jun 17):** a real Jito **bundle** now lands. A Jito `x-jito-auth` UUID (2 req/s/region) was provisioned and wired via `JITO_AUTH_UUID`; with it the Block Engine forwards our bundles — `getInflightBundleStatuses` flipped from all-region `Invalid` to `Landed`. The **production scored path** landed on attempt 1 on the **dynamic floor tip (5000 lamports), no Sender backstop** — slot 427028288, bundle `d33b83c8…e454777` (ADR 0007). Slot streaming still to wire. |
| 3–4 | **Lifecycle tracking** ✅ | DONE (Jun 17): Inclusion (tx stream) + Commitment Progression (slot stream), multiplexed over one Yellowstone subscription, write submitted/landed/processed/confirmed/finalized + deltas to SQLite — proven live (slot 427131976, confirmed→finalized 11.85s, bundle `ffc29145…264047ae`). Pluggable instruction builder (`bundle::build_bundle_with_payload`; default = self-transfer + Memo nonce). 28 unit tests green. |
| 5–6 | **Dynamic tips + leader window** ✅ | DONE (Jun 17): real leader-window timing via **gRPC** `searcher.SearcherService/GetNextScheduledLeader` (NoAuth, minimal vendored proto — ADR 0008); `getNextScheduledLeader` is gRPC-only, the old HTTP path always degraded. Proven live in the scored path (slot 427150512 landed; `current_slot=427150504 region=frankfurt`). Tip Floor percentile now config-driven (`JITO_TIP_PERCENTILE`, default 75) + account rotation (ADR 0005). Focused hardcoded-values audit: tip percentile + Sender min-tips/compute-budget moved to config. 33 unit tests green. |
| 7–8 | **Failures + retry + backpressure** ✅ | DONE (Jun 18): (WS3) Streaming resilience — one generalized resilient driver (`streaming::resilient_subscribe`): spawned gRPC receive task → bounded `mpsc` channel → caller-task consumer; exponential-backoff reconnect + drop/lag `StreamMetrics`, the deferral from ADR 0004 closed (ADR 0009). (WS1+WS2) 3 deterministic injections (`ARGUS_INJECT`) classify via preflight `simulateTransaction` (the only reason-source for an all-or-nothing bundle) → local default-remedy policy behind a `Policy` seam (Agent stand-in) → Remedy executed; persisted `failure_class` + `decisions` rows (ADR 0010). Proven live: expired_blockhash→RefreshBlockhash and compute_exceeded→RaiseCuLimit both recovered+landed (slots 427242236, 427242…), bundle_failure→Abort (no landing, recorded). 63 unit tests green. **Hardened post-review (2026-06-18): all 15 `/code-review ultra` findings closed** — cumulative reconnect ceiling + `gave_up` outcome + post-stream RPC reconciliation (ADR 0009), structured-`instruction_error` classification + observed-CU-need remedy (ADR 0010); 65 tests green. |
| 9–10 | **Agent** ✅ | DONE (Jun 18): the `Policy::Agent` seam went live — `ARGUS_POLICY=agent` swaps the decision source to the TS service over OpenRouter (default Sonnet 4.6, env-rotatable) with **no call-site change** (`Policy::Local.decide` → `policy.decide`). Structured `{remedy, rationale, confidence}` via the `submit_decision` tool; Reasoning Traces (`message.reasoning`) + the serving `model` slug persisted per decision (`decisions.model` — the ADR 0006 provenance column). Agent failure degrades loudly to Local (`model="local-fallback"`, bounded `ARGUS_AGENT_TIMEOUT_SECS`); an empty trace warns live. **CUT-LINE MET:** proven live — the three injections drove **three distinct remedies** with full traces — `expired_blockhash`→RefreshBlockhash (0.98, landed 427324624), `compute_exceeded`→RaiseCuLimit (0.98, observed-CU 12684→19026, landed 427324753), `bundle_failure`→Abort (0.97, no landing, recorded); 0 traceless scored decisions. 68 unit tests green. |
| 11 | **Run** ✅ | DONE (Jun 19): single-session orchestrator (`ARGUS_RUN=1`, ADR 0011) drives 3 injections + `ARGUS_RUN_CLEAN_COUNT` (default 7) clean Payloads under one Run prefix `run-{ts}`, each Payload a child run_id `run-{ts}-p{k}` so the proven submit/track/persist path is reused verbatim (**Run-ID-prefix keying**, zero schema change). The faulted attempt-1 is now **sent on the wire** — a real, non-landing, free Submission (amends ADR 0010's sim-only stance), so 3+7 → **12 Submissions / 3 Failures**, clearing ≥10/≥2. Preflight **hard-fails on Agent `/health`** (no silent local-fallback, ADR 0006) + thin-balance warn; serial, tracked-to-finalized, best-effort continue; end-of-Run ≥10/≥2 assertion. Lifecycle Log auto-exported (and standalone re-export via `ARGUS_EXPORT=run-{ts}`): two-part Markdown (Submissions table — slot→Solscan block, sig→Solscan tx, `—` non-landed + Commitment deltas; Agent-Decisions section) + lossless JSONL (full Reasoning Trace), rendered **pure from SQLite**. 85 unit tests green (post `/code-review`: send scoped to the Run via `send_faulted`, `prove_non_landing` dropped, `ARGUS_EXPORT` wildcard guard, char-safe `short_sig`). **Live recording Run DONE** — `run-1781848699679`: **12 Submissions / 3 injected failures / 9 landed** on mainnet, every decision served by `anthropic/claude-4.6-sonnet-20260217` (refresh_blockhash 0.98 → slot 427446957; raise_cu_limit 0.99 → 427447045; abort 0.96, no landing); cost 0.000250 SOL. Lifecycle Log committed (`logs/lifecycle-1781848699679.{md,jsonl}`, `a6aaf06`). |
| +1 (Jun 20) | **Diagnosis over the unbounded tail (ADR 0012)** ✅ | DONE (Jun 20): pivoted the Agent's owned decision from the 4→5 classifier mapping (*legible ⟹ enumerable ⟹ lookup-replicable* — a `match` replicates it) to **Failure Diagnosis over the unbounded program-error tail**. The Agent now reads the RAW surface (failing program + structured `instruction_error` + program logs), NOT `failure_class`, and returns an open-ended cause + a 4-way Triage + the Remedy; the 4-class stays the **baseline** contrast (and agent-unreachable fallback). Foreign-program spread added (`ForeignFault` Memo/Token/Whirlpool) — one identical `[0xff;8]` instruction, three DISTINCT errors the baseline collapses to one blind `BundleFailure→abort`. Storage gains `diagnosis`/`triage`/`baseline_remedy` (idempotent `ensure_column`, old-DB safe); Lifecycle Log gains the agent-vs-baseline contrast section + ⚠ blind marker. **Proven zero-SOL (sim-only, real Agent):** 4 distinct program-specific diagnoses — Token `Custom(12)`=InvalidInstruction, Memo `InvalidInstructionData`=non-UTF-8, System `Custom(1)`=lamport/SOL unit bug, Whirlpool `Custom(101)`=Anchor InstructionFallbackNotFound. 91 unit tests green. **Full graded contrast Run pending** (real-SOL, user-triggered). |
| 12–13 | **Arch doc** | Notion + Excalidraw, every required section |
| 14 | **README** | 3 answers grounded in observed data; setup instructions |
| 15 | **Polish + buffer** | Logs reproduce; stretch only if ahead |
| 16 (Jun 28) | **Submit early** | — |

**Cut order under pressure (never cut the six required deliverables):** live dashboard → self-computed leader-schedule cross-check → 4th deterministic failure class (keep 3). _(SPL/program payloads — once the last droppable stretch — instead **shipped** as the ADR 0012 foreign-program spread, since the unbounded tail is where the Agent's owned decision stops being classifier-replicable.)_

## Failure-injection matrix

**Bounded faults — the four-class baseline handles these** (remedy variation):

| Failure Class | How induced | Determinism | Expected Agent Remedy |
|---|---|---|---|
| Expired Blockhash | Sign against a stale blockhash (headline fault) | Deterministic | refresh blockhash |
| Compute Exceeded | CU limit below instruction need | Deterministic | raise CU limit |
| Bundle Failure | Include a failing instruction | Deterministic | abort / rebuild |
| Fee Too Low | Tip below live floor under contention | Probabilistic (best-effort) | bump Tip |

**Unbounded tail — the foreign-program spread (ADR 0012)** (diagnosis variation): one identical
`[0xff;8]` / zero-account instruction to three real programs yields three DISTINCT errors the
four-class baseline collapses to one blind `BundleFailure → abort`. The Agent decodes each:

| Foreign fault | Raw error (proven on sim) | What the Agent diagnoses | Baseline |
|---|---|---|---|
| Memo (`foreign_memo`) | `InvalidInstructionData` | non-UTF-8 memo data ("Invalid UTF-8, from byte 0") | `bundle_failure → abort` ⚠ |
| Token (`foreign_token`) | `Custom(12)` | SPL-Token `InvalidInstruction` — malformed discriminant | `bundle_failure → abort` ⚠ |
| Whirlpool (`foreign_whirlpool`) | `Custom(101)` | Anchor `InstructionFallbackNotFound` — wrong/stale IDL | `bundle_failure → abort` ⚠ |

The bounded faults prove the Agent picks the right *remedy*; the unbounded tail proves the
irreducible skill — **different program errors → different DIAGNOSES** where a `match` over the
4-class verdict is blind. Same action on `permanent` failures (both abort), but the Agent supplies
the program-specific *reason* the catch-all can't (the ADR 0012 honesty boundary). Also capture any
Organic Failures.

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
