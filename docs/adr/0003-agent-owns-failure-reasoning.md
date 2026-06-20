# AI Agent owns Failure Reasoning, submitted as the "Autonomous Retry with Fault Injection" superset

> **Superseded by ADR 0012 (2026-06-20).** The owned decision is reframed from *picking a Remedy out of a four-class lookup* to *diagnosing an unbounded spread of real program failures from their raw surface*. The reason: handing the Agent the classifier's verdict (`failure_class`) plus a five-element Remedy set is a 4→5 mapping a `match` replicates — the very "disguised heuristic" this ADR set out to avoid. The escape is a different *input* (the raw failure surface), not a different decision. This ADR's framing — Agent owns one failure-recovery decision, Core owns magnitudes (ADR 0005), variation is the evidence — still holds; only the decision's altitude changes.

The Agent owns exactly one operational decision: **Failure Reasoning** — given a classified Failure, diagnose the cause and choose one Remedy from a fixed Decision Space (refresh blockhash, bump Tip, raise CU limit, hold-and-resubmit, abort). The submission is *labeled* "Autonomous Retry with Fault Injection" because Failure Reasoning plus an injected blockhash-expiry fault is a strict superset of that option.

## Why

The spec demands "meaningful decisions, visible reasoning, NOT simple sequential automation." The literal option-4 flow (detect → refresh → recalc → resubmit) is itself the prescribed *sequence* that reads as automation. Failure Reasoning has no prescribed sequence — diagnosing among multiple causes and choosing among multiple Remedies is inherently LLM-shaped and hardest to dismiss as a disguised heuristic. Different Failure Classes must demonstrably produce different Remedies; the logged Reasoning Trace per decision is the evidence.

## Considered options

- **Literal Autonomous Retry (fixed flow)** — highest risk of reading as sequential automation. Rejected as the *implementation*, kept only as the *label*.
- **Tip Intelligence as the agent's mandate** — rejected; tip-bumping is instead one Remedy *within* Failure Reasoning. Giving the Agent a standalone tip mandate would split its "one decision."

## Consequences

The Agent must be able to choose "wrong" and to vary by Failure Class, or the reasoning claim is hollow. Base Tip computation stays in the Core (see ADR 0005); the Agent only adjusts Tip as a Remedy.
