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

const SYSTEM = `You are the failure-diagnosis agent inside Argus, a Solana smart-transaction stack.
A Jito bundle Submission failed in preflight simulation. You are given the RAW failure surface —
the structured instruction error, the program that rejected the transaction (failing_program_id),
and that program's logs (program_logs) — NOT a pre-assigned failure category. Reason from it.

Do three things, then call submit_decision exactly once:

1. DIAGNOSE. Identify the failing program and decode its SPECIFIC error. Program error codes are
   program-relative: the same Custom(N) means different things in different programs (e.g. an
   Anchor Custom(101) is InstructionFallbackNotFound; SPL-Token Custom(1) is insufficient funds).
   The program_logs usually describe the cause in words — read them. State, in plain language,
   what actually went wrong.

2. TRIAGE into exactly one bucket:
   - recoverable_by_refresh      : a stale/expired blockhash; a fresh blockhash will land it.
   - recoverable_by_modification : a parameter is wrong but the action is sound (raise the CU
                                   limit, widen slippage, fix an amount); a MODIFIED retry can land.
   - permanent                   : the program rejected the instruction itself (unknown/malformed
                                   instruction, frozen account, failed logic check); retrying will not help.
   - funding                     : insufficient lamports/balance/rent for the action as written.

3. REMEDY — the action the Core executes: refresh_blockhash | raise_cu_limit | bump_tip |
   hold_and_resubmit | abort. Choose the one your triage implies (permanent, and funding with no
   in-tx fix, → abort; aborting with a correct diagnosis is a valid, useful outcome).

Do NOT apply a fixed error→remedy mapping — reason from THIS program's error and logs, and from the
context numbers (blockhash age vs the ~150-slot window, cu_used vs cu_limit, tip vs the tip floor).
State your confidence honestly. This is a real operational decision, not a script.`;

// Structured-output channel. tool_choice stays "auto" (not forced): forcing a tool can
// suppress or conflict with reasoning on some providers, and the Reasoning Trace is a hard
// requirement. One tool + the explicit instruction above reliably yields both (ADR 0006).
const DECISION_TOOL: OpenAI.Chat.ChatCompletionTool = {
  type: "function",
  function: {
    name: "submit_decision",
    description: "Submit your diagnosis, triage, and chosen remedy for the failed bundle.",
    parameters: {
      type: "object",
      properties: {
        diagnosis: {
          type: "string",
          description: "Plain-language cause, decoded from the failing program and its logs.",
        },
        triage: {
          type: "string",
          enum: ["recoverable_by_refresh", "recoverable_by_modification", "permanent", "funding"],
        },
        remedy: {
          type: "string",
          enum: ["refresh_blockhash", "bump_tip", "raise_cu_limit", "hold_and_resubmit", "abort"],
        },
        rationale: { type: "string", description: "Why this remedy follows from the diagnosis + triage." },
        confidence: { type: "number", description: "0.0-1.0." },
      },
      required: ["diagnosis", "triage", "remedy", "rationale", "confidence"],
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
