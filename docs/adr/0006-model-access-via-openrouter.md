# Model access via OpenRouter, not a single-vendor SDK

The Agent reaches its model through **OpenRouter** (`https://openrouter.ai/api/v1`) using the OpenAI-compatible SDK, instead of binding to one vendor's SDK (`@anthropic-ai/sdk`). The active model is an env value (`AGENT_MODEL`, default `anthropic/claude-sonnet-4.6`) with an optional ordered fallback list (`AGENT_MODEL_FALLBACKS`), so the Run can rotate models without code changes.

## Why

The Agent owns exactly one decision (ADR 0003), and that decision is small and well-bounded — an ideal place to A/B different models cheaply. OpenRouter turns model choice into configuration: one API surface, hundreds of models, an ordered `models` fallback array for resilience if a provider is down mid-Run. It also normalizes two things we depend on across providers:

- **Reasoning Trace** arrives on `message.reasoning` regardless of which model produced it (requested via the `reasoning` parameter). The visible-reasoning evidence the spec demands no longer depends on one vendor's thinking-block shape.
- **Structured output** is a standard OpenAI-style `submit_decision` tool call, parsed and validated with the existing zod schema — identical across rotated models.

We keep `tool_choice: "auto"` (not forced) plus a single tool and an explicit "always call submit_decision" instruction. Forcing a tool can conflict with reasoning on some providers (native Anthropic rejects forced tool choice while thinking); auto + one tool + instruction is the portable combination that reliably yields both a Reasoning Trace and the structured decision.

## Considered options

- **Direct `@anthropic-ai/sdk`** (the prior scaffold) — one fewer hop and first-party feature access, but locks the Agent to one vendor, makes model rotation a code change, and couples the Reasoning Trace to Anthropic's thinking-block format. Rejected: the rotation flexibility is worth one proxy hop for a decision this small.
- **OpenRouter with forced `tool_choice`** — guarantees the tool call but risks the provider dropping or rejecting reasoning when a tool is forced. Rejected in favor of auto + instruction, because the Reasoning Trace is a hard requirement.
- **`response_format` JSON-schema instead of a tool** — clean, but JSON-schema structured-output support varies by model and would undercut the rotation goal. Rejected; tool calling is the more broadly supported path.

## Consequences

The Agent depends on `OPENROUTER_API_KEY` (not `ANTHROPIC_API_KEY`) and on OpenRouter's availability — a third party in the path. Model slugs are OpenRouter's namespaced form (`anthropic/claude-sonnet-4.6`, `openai/gpt-5.2`, …), not bare Anthropic IDs. Reasoning availability is per-model: rotating to a non-reasoning model yields an empty Reasoning Trace, so the Run's logged decisions should record which model produced each one. Cost and latency now include OpenRouter's routing; acceptable for one decision per Failure.

## Day 9-10: Agent activated (2026-06-18)

The seam went live; `Policy::Agent` now drives the remedy decision when selected. The "record which model produced each decision" intent above is now concrete machinery:

- **Selection is an explicit env flag, not inferred.** `ARGUS_POLICY=agent` swaps the decision source to the HTTP Agent (default `local` keeps the stand-in). `AGENT_URL` carries a config default, so its presence can't signal intent — the flag is the unambiguous, greppable record that a given Run used the Agent.
- **Model provenance is a first-class column.** The TS Agent already returned the post-fallback serving model; the Rust `Decision` now carries `model: Option<String>` and `decisions.model` persists it. Local paths self-identify: `model="local"` for the stand-in, `model="local-fallback"` for the degraded path below. The ADR's "confirm every scored decision carried a trace" check is now one query: a real `anthropic/…` slug with a non-empty `reasoning_trace`.
- **A traceless Agent decision is kept but warned, not dropped.** If `message.reasoning` comes back empty on the Agent path, the decision is still valid (the remedy is real) but the evidence is weak, so Core warns loudly *at decision time* — the gap surfaces live during the Run, not at Lifecycle-Log assembly. (The decision-time warning enforces the ADR's rule that no traceless decision silently enters the scored Log.)
- **Agent failure degrades loudly to Local, never kills the Run.** A bounded ~45s timeout (`ARGUS_AGENT_TIMEOUT_SECS`) plus any transport/decode error fall back to the local default remedy, recorded with `model="local-fallback"` and the cause in the rationale. A transient OpenRouter hiccup can't destroy an in-progress Lifecycle Log, and because a fallback row carries no trace, the provenance check excludes it from scored evidence automatically. No Rust-side retry — OpenRouter's `models` array does provider failover inside the one call. `confidence` is recorded as evidence only, never a Core control branch (uncertainty is the Agent's to express via `hold_and_resubmit`/`abort`).

Proven live (2026-06-18, `ARGUS_POLICY=agent`, Sonnet 4.6 via OpenRouter): the three deterministic injections drove **three distinct remedies** with full reasoning traces — `expired_blockhash`→RefreshBlockhash (0.98), `compute_exceeded`→RaiseCuLimit (0.98), `bundle_failure`→Abort (0.97); the two recoverable faults landed on attempt 2 (slots 427324624, 427324753), the bundle failure aborted with no landing. Every `decisions` row carried a real `anthropic/claude-4.6-sonnet-20260217` slug and a non-empty trace; the traceless-scored-decision count was 0.
