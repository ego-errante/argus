# Argus

A Solana smart-transaction stack — named for **Argus Panoptes**, the hundred-eyed watchman who never stops watching the network. Argus observes the network in real time (Yellowstone gRPC), submits Jito bundles intelligently, tracks each submission across commitment levels, and delegates one operational decision — failure recovery — to an AI agent that reasons rather than runs a script.

Built for the Superteam Nigeria **Advanced Infrastructure Challenge**.

> **Core = the eyes** (Rust: streaming, leader window, bundles, tips, lifecycle, failure classification). **Agent = the judgment** (TypeScript: a Claude-powered failure-recovery decision). They communicate over HTTP/JSON — the boundary is the architecture.

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
├── agent/       TypeScript — the AI failure-recovery decision (OpenRouter, default Sonnet 4.6)
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
> `message.reasoning` whatever the routed model; the structured `{remedy, rationale,
> confidence}` is a `submit_decision` tool call validated with zod.

## Status

Scaffold. The Core runs and initializes the lifecycle store; the Agent answers `POST /decide` through OpenRouter (default Sonnet 4.6) with a structured `{remedy, rationale, confidence}` plus a reasoning trace. The Solana integration (streaming, bundles, tips, lifecycle tracking) is stubbed with module boundaries in place — see **docs/PLAN.md** for the day-by-day build sequence, starting with the **Day 1-2 tracer bullet** (land one real Jito bundle on mainnet).
