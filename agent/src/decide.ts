import OpenAI from "openai";
import { RemedyDecisionSchema, type FailureContext, type RemedyDecision } from "./types.js";

// OpenRouter is OpenAI-compatible: one base URL, hundreds of models (ADR 0006).
// Reads OPENROUTER_API_KEY from the environment.
const client = new OpenAI({
  baseURL: "https://openrouter.ai/api/v1",
  apiKey: process.env.OPENROUTER_API_KEY,
});

// Model rotation lives in env, not code (ADR 0006). AGENT_MODEL is the primary;
// AGENT_MODEL_FALLBACKS is an optional comma-separated list OpenRouter tries in order.
const MODEL = process.env.AGENT_MODEL ?? "anthropic/claude-sonnet-4.6";
const FALLBACKS = (process.env.AGENT_MODEL_FALLBACKS ?? "")
  .split(",")
  .map((s) => s.trim())
  .filter(Boolean);
const REASONING_MAX_TOKENS = Number(process.env.AGENT_REASONING_MAX_TOKENS ?? 2048);

const SYSTEM = `You are the failure-recovery agent inside Argus, a Solana smart-transaction stack.
A Jito bundle Submission has failed. Given the failure context, diagnose the cause and choose
exactly ONE Remedy, then call submit_decision with it:

- refresh_blockhash : the blockhash expired or is too old to land; fetch a fresh one.
- bump_tip          : the bundle is losing the Jito auction (fee too low / not landing); raise the tip.
- raise_cu_limit    : the transaction exceeded its compute-unit limit.
- hold_and_resubmit : conditions are unfavorable right now; wait for a better leader window.
- abort             : the failure is non-recoverable (e.g. a deterministically failing instruction); stop.

Reason from the SPECIFIC numbers in the context (blockhash age vs the ~150-slot validity window,
tip vs the live tip-floor percentiles, cu_used vs cu_limit, the error text). Different failure
classes should generally lead to different remedies — do not apply a fixed sequence. State your
confidence honestly. This is a real operational decision, not a script. Always finish by calling
submit_decision exactly once.`;

// Structured-output channel. tool_choice stays "auto" (not forced): forcing a tool can
// suppress or conflict with reasoning on some providers, and the Reasoning Trace is a hard
// requirement. One tool + the explicit instruction above reliably yields both (ADR 0006).
const DECISION_TOOL: OpenAI.Chat.ChatCompletionTool = {
  type: "function",
  function: {
    name: "submit_decision",
    description: "Submit your chosen remedy for the failed bundle.",
    parameters: {
      type: "object",
      properties: {
        remedy: {
          type: "string",
          enum: ["refresh_blockhash", "bump_tip", "raise_cu_limit", "hold_and_resubmit", "abort"],
        },
        rationale: { type: "string", description: "Why this remedy, grounded in the context numbers." },
        confidence: { type: "number", description: "0.0-1.0." },
      },
      required: ["remedy", "rationale", "confidence"],
    },
  },
};

// OpenRouter-only request fields the base OpenAI types don't model.
type OpenRouterExtras = {
  reasoning?: { max_tokens?: number; effort?: "low" | "medium" | "high"; exclude?: boolean };
  models?: string[];
};

/**
 * The Agent's single owned decision (ADR 0003), via OpenRouter (ADR 0006).
 *
 * The `reasoning` blocks come back normalized on `message.reasoning` (whatever the
 * routed model), which is the visible-reasoning evidence; the `submit_decision` tool
 * call carries the typed {remedy, rationale, confidence}. Model + fallbacks are env-driven,
 * so the Run can rotate models without touching this file.
 */
export async function decide(
  ctx: FailureContext,
): Promise<RemedyDecision & { reasoning_trace?: string; model?: string }> {
  const body: OpenAI.Chat.ChatCompletionCreateParamsNonStreaming & OpenRouterExtras = {
    model: MODEL,
    ...(FALLBACKS.length ? { models: [MODEL, ...FALLBACKS] } : {}),
    max_tokens: 4096,
    reasoning: { max_tokens: REASONING_MAX_TOKENS },
    tools: [DECISION_TOOL],
    tool_choice: "auto",
    messages: [
      { role: "system", content: SYSTEM },
      { role: "user", content: JSON.stringify(ctx, null, 2) },
    ],
  };

  const completion = await client.chat.completions.create(body);
  const choice = completion.choices[0];
  const message = choice?.message as
    | ((typeof completion.choices)[number]["message"] & { reasoning?: string })
    | undefined;

  const reasoning_trace = message?.reasoning?.trim() || undefined;

  const call = message?.tool_calls?.find(
    (c) => c.type === "function" && c.function.name === "submit_decision",
  );
  if (!call || call.type !== "function") throw new Error("agent did not submit a decision");

  const decision = RemedyDecisionSchema.parse(JSON.parse(call.function.arguments));
  // OpenRouter reports the model that actually served the request (after any fallback).
  return { ...decision, reasoning_trace, model: completion.model };
}
