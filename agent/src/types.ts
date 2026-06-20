import { z } from "zod";

// CONTEXT.md: Failure Class. Strings match the Rust Core enums (snake_case).
export const FailureClass = z.enum([
  "expired_blockhash",
  "fee_too_low",
  "compute_exceeded",
  "bundle_failure",
]);
export type FailureClass = z.infer<typeof FailureClass>;

// CONTEXT.md: the Agent's Decision Space (Remedy) — the executable action.
export const Remedy = z.enum([
  "refresh_blockhash",
  "bump_tip",
  "raise_cu_limit",
  "hold_and_resubmit",
  "abort",
]);
export type Remedy = z.infer<typeof Remedy>;

// CONTEXT.md: Triage — the Agent's recovery-bucket sort, the axis it reasons on (ADR 0012).
export const Triage = z.enum([
  "recoverable_by_refresh",
  "recoverable_by_modification",
  "permanent",
  "funding",
]);
export type Triage = z.infer<typeof Triage>;

// Sent by the Rust Core on each Failure (mirrors core/src/agent_client.rs). The Agent reasons
// over the RAW failure surface — NOT a pre-classified `failure_class` (ADR 0012): handing it
// the classifier's verdict is the lookup we removed. `instruction_error` is the structured
// variant (e.g. `{"Custom":101}`), `failing_program_id` the rejecting program, `program_logs`
// that program's own logs.
export const FailureContextSchema = z.object({
  attempt: z.number().int(),
  error_text: z.string(),
  instruction_error: z.string().nullable().optional(),
  failing_program_id: z.string().nullable().optional(),
  program_logs: z.array(z.string()),
  tip_lamports: z.number().int(),
  // Honestly optional: a `null` means the Core couldn't fetch it (not a fabricated
  // 0/base). Matches the Rust FailureContext's Option<u64> for these fields.
  tip_floor_p50: z.number().int().nullable().optional(),
  tip_floor_p75: z.number().int().nullable().optional(),
  blockhash_age_slots: z.number().int().nullable().optional(),
  cu_limit: z.number().int().nullable().optional(),
  cu_used: z.number().int().nullable().optional(),
  current_slot: z.number().int().nullable().optional(),
});
export type FailureContext = z.infer<typeof FailureContextSchema>;

// The Agent's structured decision (parsed from the submit_decision tool call). The diagnosis
// + triage are the reasoned output (ADR 0012); remedy is the executable action they imply.
export const RemedyDecisionSchema = z.object({
  diagnosis: z.string(),
  triage: Triage,
  remedy: Remedy,
  rationale: z.string(),
  confidence: z.number(),
});
export type RemedyDecision = z.infer<typeof RemedyDecisionSchema>;
