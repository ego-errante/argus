# Argus

A Solana smart-transaction stack — named for **Argus Panoptes**, the hundred-eyed watchman who never stops watching the network. Argus observes the network in real time (Yellowstone gRPC), submits Jito bundles intelligently, tracks each submission across commitment levels, and delegates one operational decision — **failure diagnosis** — to an AI agent that reasons over the raw failure surface rather than running a script.

Built for the Superteam Nigeria **Advanced Infrastructure Challenge**.

> **Core = the eyes** (Rust: streaming, leader window, bundles, tips, lifecycle, baseline failure classification). **Agent = the judgment** (TypeScript: a Claude-powered failure-**diagnosis** decision — it reads the raw failure surface, the failing program + its structured error + its logs, and returns a cause + a Triage + the Remedy, NOT a lookup over a pre-assigned class — ADR 0012). They communicate over HTTP/JSON — the boundary is the architecture.

## Docs

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

# Run the two services locally:
make agent                  # TS agent on :8787  (needs OPENROUTER_API_KEY)
make core                   # Rust core (prints "scaffold ready" until the Day 1-2 tracer bullet lands)

# Or both in containers:
make up
```

> The agent reaches its model through **OpenRouter** (OpenAI-compatible API), so the model
> is env-configurable and rotatable mid-Run — `AGENT_MODEL` plus an optional
> `AGENT_MODEL_FALLBACKS` list (ADR 0006). The Reasoning Trace comes back normalized on
> `message.reasoning` whatever the routed model; the structured `{diagnosis, triage, remedy,
> rationale, confidence}` is a `submit_decision` tool call validated with zod.

## Status

**Live on mainnet.** The full stack runs end-to-end: Yellowstone slot/tx streaming, leader-window-timed Jito bundles with dynamic Tip-Floor tips, commitment-progression lifecycle tracking to SQLite, and the Agent's failure-diagnosis decision over OpenRouter. A graded recording Run (`ARGUS_RUN=1`, ADR 0011) has landed **12 Submissions / 3 Failures / 9 landed** on mainnet with full Reasoning Traces (`logs/lifecycle-*.{md,jsonl}`).

The Agent reasons over the **raw failure surface** — the failing program, its structured instruction error, and its logs — and returns an open-ended **Diagnosis** + a **Triage** + the **Remedy** (ADR 0012); the four-class taxonomy is retained as the *baseline* the Diagnosis is measured against, not the Agent's input. The foreign-program spread (Memo / Token / Whirlpool) shows the point: one identical malformed instruction draws three DISTINCT program-specific diagnoses where the baseline collapses all three to one blind `bundle_failure → abort`. See **docs/PLAN.md** for the day-by-day sequence and **docs/adr/** for the decisions.
