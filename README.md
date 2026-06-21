# Argus

A Solana smart-transaction stack — named for **Argus Panoptes**, the hundred-eyed watchman who never stops watching the network. Argus observes the network in real time (Yellowstone gRPC), submits Jito bundles intelligently, tracks each submission across commitment levels, and delegates one operational decision — **failure diagnosis** — to an AI agent that reasons over the raw failure surface rather than running a script.

Built for the Superteam Nigeria **Advanced Infrastructure Challenge**.

> **Core = the eyes** (Rust: streaming, leader window, bundles, tips, lifecycle, baseline failure classification). **Agent = the judgment** (TypeScript: a Claude-powered failure-**diagnosis** decision — it reads the raw failure surface, the failing program + its structured error + its logs, and returns a cause + a Triage + the Remedy, NOT a lookup over a pre-assigned class — ADR 0012). They communicate over HTTP/JSON — the boundary is the architecture.

## Evaluate Argus in 5 minutes

A judge's fast path — what to read, the one thing that proves the thesis, and how to reproduce it. The full system design lives in the **[hosted, interactive architecture document](https://argus-architecture.pages.dev/)** (the separately-judged deliverable) — step through the real run's reasoning traces and the explorable lifecycle log.

1. **The thesis (2 min).** Read **[docs/adr/0012](./docs/adr/0012-agent-owns-failure-diagnosis-over-unbounded-tail.md)**: the AI Agent does *not* pick a remedy from a fixed four-class lookup (a `match` could do that) — it reasons over the **raw failure surface** (the failing program + its structured `instruction_error` + its logs) and returns a free-text **Diagnosis** + a **Triage** + the **Remedy**. The four-class taxonomy is demoted to a *baseline contrast column*. Glossary first if needed: **[CONTEXT.md](./CONTEXT.md)**.

2. **The proof (2 min).** Open the graded mainnet Run **[logs/lifecycle-1781958744615.md](./logs/lifecycle-1781958744615.md)** and read the Agent-Decisions table: **four** payloads the baseline collapses to one identical blind `bundle_failure → abort ⚠` — System funding + Memo (`InvalidInstructionData`), SPL-Token (`Custom(12)`), Orca Whirlpool (`Custom(101)`) — drew **four DISTINCT** program-specific diagnoses at 0.97–0.98 confidence. The two recoverable injections (expired blockhash, CU-exceeded) were triaged and **landed on attempt 2**. Whole Run: **15 sent / 6 failed / 9 landed**, real mainnet, **0.000135 SOL** (faulted bundles never land → no tip).

3. **Reproduce it (1 min to kick off).** With `.env` filled and `make build` run once: start the Agent (`make agent`), then `ARGUS_RUN=1 ARGUS_POLICY=agent cargo run -p argus-core`. It health-gates the Agent, sends 6 injections + 7 clean Payloads on mainnet, tracks each to `finalized` via Yellowstone, and re-exports the Lifecycle Log.

One line: **Core (Rust) = the eyes; Agent (TS) = the judgment; the HTTP boundary is the point.** Decisions in **[docs/adr/](./docs/adr/)**.

## Docs

- **[Architecture document](https://argus-architecture.pages.dev/)** — the hosted, **interactive** system design (separately-judged): the big-picture *why* and *shape*, plus an explorable lifecycle log with the agent's real reasoning traces.
- **[CONTEXT.md](./CONTEXT.md)** — the glossary (shared language; read this first).
- **[docs/PLAN.md](./docs/PLAN.md)** — build sequence, requirement map, failure-injection matrix, README-answer prep.
- **[docs/adr/](./docs/adr/)** — the architectural decisions and why.

## Layout

```
argus/
├── core/        Rust — network-facing transaction logic (Yellowstone, Jito, lifecycle)
│   ├── src/     config, model, storage, agent_client, streaming, leader, bundle, tip
│   └── migrations/001_init.sql   SQLite schema (Lifecycle Log source of truth)
├── agent/       TypeScript — the AI failure-diagnosis decision (OpenRouter, default Sonnet 4.6)
│   └── src/     index (HTTP), decide (OpenRouter), types (zod schemas)
├── docs/        PLAN.md + ADRs
└── logs/        SQLite + JSONL + generated Markdown Lifecycle Log
```

## Quick start

```bash
cp .env.example .env        # then fill in SolInfra endpoints + OPENROUTER_API_KEY (see docs/PLAN.md)
make build                  # run ONCE first: builds the Core + installs & typechecks the Agent

# 1) Start the Agent first — a scored Run health-checks it and refuses to start if it's down:
make agent                  # TS agent on :8787  (needs OPENROUTER_API_KEY)

# 2) Drive the Core (reads .env; an env flag selects the mode):
make core                                                 # health/liveness check, no submission
ARGUS_LIFECYCLE=1 cargo run -p argus-core                 # submit ONE real bundle, track it to finalized
ARGUS_RUN=1 ARGUS_POLICY=agent cargo run -p argus-core    # the graded Run (ADR 0011): 6 injections + 7 clean, Agent diagnoses each failure

# Or both in containers:
make up
```

> The agent reaches its model through **OpenRouter** (OpenAI-compatible API), so the model
> is env-configurable and rotatable mid-Run — `AGENT_MODEL` plus an optional
> `AGENT_MODEL_FALLBACKS` list (ADR 0006). The Reasoning Trace comes back normalized on
> `message.reasoning` whatever the routed model; the structured `{diagnosis, triage, remedy,
> rationale, confidence}` is a `submit_decision` tool call validated with zod.

## Status

**Live on mainnet.** The full stack runs end-to-end: Yellowstone slot/tx streaming, leader-window-timed Jito bundles with dynamic Tip-Floor tips, commitment-progression lifecycle tracking to SQLite, and the Agent's failure-diagnosis decision over OpenRouter. A graded recording Run (`ARGUS_RUN=1`, ADR 0011) landed **15 Submissions / 6 Failures / 9 landed** on mainnet with full Reasoning Traces, for **0.000135 SOL** total (the 6 faulted bundles never landed → no tip charged) — see `logs/lifecycle-1781958744615.{md,jsonl}`.

The Agent reasons over the **raw failure surface** — the failing program, its structured instruction error, and its logs — and returns an open-ended **Diagnosis** + a **Triage** + the **Remedy** (ADR 0012); the four-class taxonomy is retained as the *baseline* the Diagnosis is measured against, not the Agent's input. The foreign-program spread shows the point: in that Run, **four** payloads the baseline collapses to one identical blind `bundle_failure → abort` — a System-Program funding shortfall plus three malformed foreign calls (**Memo → `InvalidInstructionData`**, **SPL-Token → `Custom(12)`**, **Orca Whirlpool → `Custom(101)` `InstructionFallbackNotFound`**) — drew **four DISTINCT** program-specific diagnoses, each at 0.97–0.98 confidence. The two recoverable injections (expired blockhash, CU-exceeded) were triaged and **landed on attempt 2**. See **docs/PLAN.md** for the day-by-day sequence and **docs/adr/** for the decisions.

## Three questions, answered from the graded Run

The numbers below are from `run-1781958744615` (`logs/lifecycle-1781958744615.{md,jsonl}`), 9 landed Submissions on mainnet.

**1. What does the `processed → confirmed` time delta represent?**
It's the cluster's **vote-aggregation latency**, not inclusion speed. `processed` means a leader has produced the block and we've observed it; `confirmed` (optimistic confirmation) means **≥⅔ of stake has voted** on that block. The delta therefore measures **consensus health / how fast votes propagate and aggregate** — independent of how quickly the transaction itself was included. In this Run it was **87–272 ms (median 123 ms)** across the 9 landed Submissions: sub-quarter-second, i.e. healthy consensus. For contrast, the *next* hop — `confirmed → finalized` — took **~11.8–13.3 s (median ~12.2 s)**, because finalization waits for the block to be rooted (~31 confirmed blocks). Two adjacent deltas, two orders of magnitude apart, measuring different things.

**2. Why not use a `finalized` blockhash for a time-sensitive transaction?**
A recent blockhash is valid for only **~150 slots (~60–90 s)**. A blockhash fetched at `finalized` commitment is already **~31 slots old** the instant you receive it — you've burned ~20% of the validity window before you've even built, signed, and submitted. For a latency-sensitive bundle that's wasted runway and raises the risk of expiry mid-flight; fetch the **freshest viable** blockhash (`processed`/`confirmed`) instead. The Run demonstrates the failure mode directly: payload **p0** was injected with a blockhash aged **200 slots** (past the ~150 window) → the runtime rejected it at preflight with **`BlockhashNotFound`**, before any instruction executed. The Agent diagnosed exactly that (confidence 0.99), triaged it `recoverable_by_refresh`, and recovery required a **fresh blockhash** — attempt 2 then landed.

**3. What happens if the Jito leader skips their slot?**
A Jito bundle is **slot-specific** (it targets a Jito-validator leader slot) and **atomic / all-or-nothing**. If that leader skips or fails to build the slot, the bundle is simply **not included** — it is **not** auto-forwarded to the next leader, and because **Jito tips are paid only on inclusion, no tip is charged** for a bundle that never lands. The remedy is to **resubmit to the next Jito leader window**, with a fresh blockhash if the original is near expiry. This Run is the receipt: all **6** faulted bundles were sent "non-landing, free" (zero tip across them), the whole 9-landed Run cost **0.000135 SOL**, and the two *recoverable* faults were resubmitted and **landed on the following attempt**.
