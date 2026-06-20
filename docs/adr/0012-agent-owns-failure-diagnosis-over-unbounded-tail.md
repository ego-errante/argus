# AI Agent owns Failure Diagnosis over the unbounded program-error tail (supersedes ADR 0003; amends ADR 0010)

The Agent's owned decision is reframed from *picking a Remedy out of a four-class lookup* to **diagnosing an unbounded spread of real program failures from their raw surface and triaging each**. Concretely: stop handing the Agent `failure_class` (the classifier's own verdict); start handing it the raw failure surface — the structured `instruction_error` variant, the full program `logs`, and the failing program ID. The Agent returns a **Diagnosis** (free-text cause), a **Triage** (one of four recovery buckets), and the executable **Remedy**; `apply_remedy` executes the chosen Remedy unchanged. The four-class `classify_failure → default_remedy` map is **demoted** from the Agent's input to a *baseline comparison column* in the Lifecycle Log (and the agent-unreachable fallback).

## Why

**The trap, named.** Any decision specifiable cleanly enough to grade in a reproducible log is encodable as a classifier: legible ⟹ enumerable ⟹ lookup-replicable. ADR 0003 framed the owned decision as choosing a Remedy from a fixed Decision Space keyed on a four-class Failure Class. But handing the Agent the *classifier's output* (`failure_class`) plus a five-element Remedy set makes its decision a 4→5 mapping a `match` replicates — the exact "simple wrapper that calls functions sequentially without reasoning" the spec disqualifies. This is not hypothetical: ADR 0010's own Day 9-10 note records the live Agent *re-deriving the same remedies the local map hardcodes*. We were grading the Agent on a decision a lookup table makes optimally.

**The escape is the input, not the decision.** An LLM's only irreducible edge over a decision tree is on input that resists featurization: unbounded, unstructured. In this stack that input exists in exactly one cheaply-and-honestly-observable place — the **raw failure surface**. `simulateTransaction` executes real foreign programs against live mainnet state and returns each program's own error code and logs. Verified 2026-06-20, three foreign programs surfaced three distinct errors with self-describing logs, for zero SOL:

- **SPL-Memo** → `InstructionError:[0,"InvalidInstructionData"]` + log `Invalid UTF-8, from byte 0`
- **SPL-Token** → `InstructionError:[0,{"Custom":12}]` + log `Error: Invalid instruction`
- **Orca Whirlpool** → `InstructionError:[0,{"Custom":101}]` + log `AnchorError ... InstructionFallbackNotFound. Error Number: 101. Fallback functions are not supported.`

The four-class classifier collapses all three to `BundleFailure → abort` — blind, because the cause is *program-relative*: Token's `Custom(12)` and Anchor's `Custom(101)` share an integer namespace meaning different things, and the meaning lives in (program ID + log text), which a feature vector cannot hold. Reading that surface into a correct, distinct diagnosis is the genuinely-agent-shaped work; the four-class label is the lookup the Agent must sit *upstream* of, not consume.

**Why not Submission Timing (spec c).** The earlier pivot candidate. Its inputs are intrinsically bounded numerics — slots-until-window, blockhash budget, tip percentiles, a congestion proxy — so it is the same classifier under a prospective coat. Its one genuinely-unbounded surface is the transaction's own semantic intent (the *same* decode skill), but we cannot honestly populate it without varied real trade payloads — a DEX integration and a scope we reject (Argus is infrastructure, not a trading bot). Failure diagnosis is simply where the one irreducible skill — *decode unstructured Solana context into meaning* — is cheapest to demonstrate truthfully.

## Considered options

- **Keep ADR 0003's four-class → Remedy framing** — rejected: it *is* the disqualified lookup; the live Agent demonstrably re-derives the map (ADR 0010).
- **Pivot the owned decision to Submission Timing (spec c)** — rejected: bounded-numeric input is equally a classifier; its only unbounded surface (payload intent) is unfillable without becoming a trading system.
- **Replace the Remedy enum with a new `{retry-as-is, retry-modified, abort}` response space** — rejected: rips out the proven `apply_remedy` execution path for no gain. The action set is necessarily small and the existing Remedy enum already spans it. Keep Remedy as the execution bridge; add Diagnosis + Triage as the *graded reasoning*. The Agent's value is the read, not reinventing execution.
- **Add a genuine *transient* recovery anchor** (hot-account contention → retry-as-is → lands) — rejected as unobservable on our path. The reason source is preflight `simulateTransaction` — single-threaded, deterministic — structurally blind to live-scheduling transients (verified 2026-06-20). The only "transient" simulation surfaces is blockhash expiry, which is really retry-with-fresh-input.

## Consequences

- **Agent input** gains `program_logs`, `instruction_error`, and `failing_program_id`, and loses `failure_class`. The program `logs` were *discarded* before the Agent saw them in the prior design (the `FailureContext` carried only the flattened `err`); threading them in is the single highest-value change.
- **Agent output** gains a free-text `diagnosis` and a four-way `Triage` — `RecoverableByRefresh | RecoverableByModification | Permanent | Funding`, the buckets a preflight simulation can actually observe. It still returns an executable `Remedy`; `apply_remedy` is untouched and the Core still owns all magnitudes (ADR 0005).
- **`classify_failure` and `default_remedy` are retained, not deleted** — they become the *baseline column* the Lifecycle Log shows beside the Agent's diagnosis, and the agent-unreachable fallback (ADR 0006 provenance markers unchanged). Grading shifts from ADR 0003's "do four classes drive four remedies" to "across an unbounded spread of real program failures, does the Agent produce correct *distinct* diagnoses where the baseline collapses to one blind abort."
- **Honesty boundary.** On the `Permanent` bucket the Agent and the baseline both abort — the Agent's added value there is the *reason*, not the action. Action-divergence (a recovering action where the baseline aborts) is limited to the recoverable buckets: blockhash + CU are in hand; a foreign-program parameter recovery (e.g. DEX slippage → retry-modified → lands) is a *paid stretch*, not required. The Lifecycle Log marks where the baseline was blind rather than overclaiming an action win.
- **Persistence**: `decisions` gains `diagnosis`, `triage`, `baseline_class`, `baseline_remedy` via the idempotent `ensure_column` guard (ADR 0010 review-fix) — additive, no migration framework.
- **Lifecycle Log** gains a *Failure Triage* section: agent diagnosis vs. four-class baseline, with a "baseline blind" marker where distinct causes collapsed to one action.
- **CONTEXT.md** is updated in coordination: the Agent owns *failure diagnosis*; Remedy is demoted to the executed action; Decision Space is reframed; Diagnosis and Triage are added; Failure Class is annotated as the baseline taxonomy.
- **PLAN.md** (Day 12-14 framing) and the **README** ("owns one decision") will be reframed at write-time.

## Note (2026-06-20): the DEX surface, worked

The clearest illustration of the thesis is a DEX, because a DEX's *entire* error space lands in the one bucket the four-class baseline is blindest in. This note works the argument; it does **not** commit to building it — a live DEX recovery remains the unbuilt "paid stretch" above (the execution mechanic it needs is scoped separately).

**The collapse, in code.** Almost every AMM failure surfaces as `InstructionError(Custom(N))`. `classify_failure` (failure.rs) routes *every* non-compute instruction error to one place:

```rust
if let Some(ie) = &sim.instruction_error {
    // ...compute-exceeded check...
    return Some(FailureClass::BundleFailure);   // every OTHER instruction error
}
```

→ `default_remedy(BundleFailure) = Abort`. So the baseline maps the whole DEX tail to **one bucket → one blind action.** It cannot separate "widen 0.3% and you land" from "this pool is dead, stop."

**Four real DEX failures, one classifier bucket, four right answers:**

| Real failure (Whirlpool-style) | Baseline | Agent reads program + code + log → |
|---|---|---|
| `AmountOutBelowMinimum` (slippage) | `bundle_failure → abort` | `RecoverableByModification` — widen min-out, retry |
| `TickArrayNotFound` / price left the range | `bundle_failure → abort` | `RecoverableByModification` — re-quote / new tick array |
| Insufficient input-token balance | `bundle_failure → abort` | `Funding` — top up, don't retry blind |
| `PoolPaused` / frozen / `OperationNotAllowed` | `bundle_failure → abort` | `Permanent` — abort, **and say why** |

The baseline is forced into one global policy across all four: abort-everything (loses every recoverable swap — missed fills, bad UX) **or** retry-everything (burns real tips + priority fees re-submitting into dead pools). Neither is correct; the Agent's Triage *is* the difference.

**Why "just add more classes" can't reach this.** The DEX error space is unbounded *and program-relative*. Each AMM (Orca, Raydium, Meteora, Phoenix, Lifinity…) defines its own custom-error enum with its own numbering: `Custom(6022)` is slippage in one program and something unrelated in another. The integer is only meaningful relative to (the program that threw it + that program's IDL/log text at that version). A static classifier would need a maintained per-program, per-version `code → meaning` table for every DEX it might ever touch — combinatorial, perpetually stale, broken by any new pool or program upgrade. The Agent instead reads the raw surface this ADR threads in — `failing_program_id` + `{"Custom":N}` + the self-describing log line (`AnchorError ... Error Code: AmountOutBelowMinimum`) — and infers meaning from program identity + log prose, no table. That generalizes to a DEX it has never seen. This is the same `program ID + log text` argument as the Memo/Token/Whirlpool spread above, sharpened: on a DEX the collapsed tail is not an edge case, it is the common case.

**Why it matters more here than elsewhere.** On a DEX, failure is routine (slippage on volatile pairs) and every retry spends real tip + priority fee. The Agent's read turns "bundle failed, aborted" into either *"auto-recovered — widened slippage, landed attempt 2"* or *"stopped — this pool is paused, here's the log line."* Both correct, neither reachable by a four-bucket classifier, and the choice between them is real money or a real swap saved. The DEX doesn't introduce a new kind of edge — it is the densest concentration of the edge this ADR already claims.
