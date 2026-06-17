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
