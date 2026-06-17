# Two-runtime split: Rust Core + TypeScript Agent over HTTP

The stack is split into a Rust **Core** (all network-facing transaction logic) and a separate TypeScript **Agent** (the AI failure-recovery decision), communicating over HTTP/JSON, in a single docker-compose monorepo (`/core`, `/agent`, `/docs`, `/logs`).

## Why

Rust earns its place specifically on the Yellowstone gRPC firehose: tokio gives idiomatic bounded-channel backpressure and reconnection, which is the one hard feature the spec names explicitly ("proper reconnection and backpressure handling"), and Rust signals the infrastructure depth this challenge rewards. TypeScript hosts the Agent because the LLM/agent ecosystem and prompt-iteration speed are best there. The process boundary is also the literal embodiment of the judged "clean separation between AI layer and core transaction stack."

## Considered options

- **All-TypeScript** — fastest path to a *complete* submission, but backpressure on a gRPC firehose is awkward on a single-threaded event loop, and the infra-depth optic is weaker. Rejected because the builder is Rust-comfortable and full-time, so completion risk is acceptable and the upside is real.
- **All-Rust** — strongest infra signal but poor LLM ergonomics (community-only SDKs, recompile-to-tune-prompts) and it dissolves the clean-separation optic into one process. Rejected.

## Consequences

Two toolchains to build and run. HTTP is the only contract between Core and Agent; the Agent must hold zero transaction logic, or the boundary blurs.
