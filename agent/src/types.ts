import { z } from "zod";

// CONTEXT.md: Failure Class. Strings match the Rust Core enums (snake_case).
export const FailureClass = z.enum([
  "expired_blockhash",
  "fee_too_low",
  "compute_exceeded",
  "bundle_failure",
]);
export type FailureClass = z.infer<typeof FailureClass>;

// CONTEXT.md: the Agent's Decision Space (Remedy).
export const Remedy = z.enum([
  "refresh_blockhash",
  "bump_tip",
  "raise_cu_limit",
  "hold_and_resubmit",
  "abort",
]);
export type Remedy = z.infer<typeof Remedy>;

// Sent by the Rust Core on each Failure (mirrors core/src/agent_client.rs).
export const FailureContextSchema = z.object({
  failure_class: FailureClass,
  attempt: z.number().int(),
  error_text: z.string(),
  tip_lamports: z.number().int(),
  tip_floor_p50: z.number().int(),
  tip_floor_p75: z.number().int(),
  blockhash_age_slots: z.number().int().nullable().optional(),
  cu_limit: z.number().int().nullable().optional(),
  cu_used: z.number().int().nullable().optional(),
  current_slot: z.number().int(),
});
export type FailureContext = z.infer<typeof FailureContextSchema>;

// The Agent's structured decision (parsed from the submit_decision tool call).
export const RemedyDecisionSchema = z.object({
  remedy: Remedy,
  rationale: z.string(),
  confidence: z.number(),
});
export type RemedyDecision = z.infer<typeof RemedyDecisionSchema>;
