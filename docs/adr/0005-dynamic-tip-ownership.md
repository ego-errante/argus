# Dynamic Tips from the Tip Floor; Core sets the base, Agent adjusts on failure

Each Bundle's base Tip is derived by the Core from the live Jito Tip Floor (a percentile — ~50th–75th — scaled by Leader Window urgency), rotating across the published Tip Accounts. No Tip value is hardcoded. The Agent may raise the Tip as one Remedy on Fee-Too-Low / non-landing Failures, but base tipping stays deterministic in the Core.

## Why

The spec forbids hardcoded Tip values and rewards reasoning about "current network conditions." Splitting ownership keeps the Agent's single mandate intact (Failure Reasoning, per ADR 0003) while still letting it reason about cost-vs-landing where it matters — on failure. This boundary is recorded so a future reader doesn't "consolidate" all tip logic into the Agent and accidentally give it two decisions.

## Considered options

- **Agent owns all Tips (Tip Intelligence)** — rejected; would split the Agent's one decision.
- **Fixed percentile, no urgency scaling** — technically not-hardcoded, but a weaker "current network conditions" story. Rejected.
