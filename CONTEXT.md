# Argus

Named for Argus Panoptes, the hundred-eyed watchman of Greek myth — the system never stops watching the network. Argus is a Solana transaction-infrastructure system that observes the network in real time, submits Jito bundles intelligently, tracks each submission across commitment levels, and delegates one operational decision — failure recovery — to an AI agent. Built for the Superteam Nigeria "Advanced Infrastructure Challenge."

This glossary is the shared language for the codebase, the architecture document, and the README. The transaction domain is terminology-dense and several terms are overloaded (especially "confirmed"), so definitions here are deliberately opinionated.

## Language

### System & Components

**Argus**:
The whole system — Core plus Agent. The name of the deliverable, not any single process.
_Avoid_: "the app", "the bot", "Smart Transaction Stack" (the spec's phrase, not the name)

**Core**:
The Rust service that touches the network: streams from Yellowstone, builds and submits bundles, tracks the lifecycle, classifies failures, and executes remedies. All deterministic logic lives here.
_Avoid_: backend, server, engine

**Agent**:
The separate TypeScript service that owns the failure-recovery decision. It receives failure context over HTTP and returns a chosen Remedy with reasoning. It holds no transaction logic of its own.
_Avoid_: AI layer, LLM, model, brain

### Transaction Lifecycle

**Submission**:
One bundle sent to the Jito Block Engine. The unit counted by the "≥10 real bundle submissions" requirement. A retry is a *new* Submission, not a continuation of the old one.
_Avoid_: send, transaction (a Submission carries a transaction but is not one)

**Attempt**:
A Submission viewed as one try at landing a particular logical Payload. A Payload that fails and is retried produces multiple Attempts, each its own Submission with a fresh blockhash.
_Avoid_: retry (use "retry" only as a verb)

**Payload**:
The logical transaction-task a Run submits — the thing an Attempt is an attempt *at*. One Payload may take several Attempts (a Failure and its retry are Attempts at the same Payload); each Attempt is its own Submission. A Run is a sequence of Payloads (here: clean Payloads plus the injected ones), giving the hierarchy Run → Payload → Attempt/Submission.
_Avoid_: transaction (a Payload is realized as a transaction but is the logical unit), job, task

**Landed** / **Inclusion**:
The fact that a transaction was included in a produced block. This is binary and is detected via the transaction stream. Distinct from any commitment level.
_Avoid_: confirmed (do NOT use "confirmed" to mean landed), accepted

**Processed**:
The Solana commitment level: the block has been replayed by a node. One confirmation; may still be on a minority fork. The first lifecycle stage after Landing.
_Avoid_: seen, received

**Confirmed**:
The Solana commitment level: ≥2/3 of stake has voted on the slot (optimistic confirmation). Reserved strictly for this commitment level — never used to mean "landed".
_Avoid_: using "confirmed" loosely for landing or success

**Finalized**:
The Solana commitment level: the slot is rooted (≥31 confirmations deep) and irreversible.
_Avoid_: settled, final

**Commitment Progression**:
The advance of a landed transaction's slot through Processed → Confirmed → Finalized, tracked via the slot stream. The source of the latency deltas in the Lifecycle Log.
_Avoid_: status updates

**Lifecycle Log**:
The graded artifact: a table of ≥10 Submissions with slot numbers (as explorer links), Commitment Progression, timestamps, latency deltas, Tip amounts, and Failure Class. SQLite is the source of truth; JSONL and the Markdown table are exports.
_Avoid_: report, output, results

**Run**:
A single recording session that produces a Lifecycle Log of ≥10 Submissions including the required failures.
_Avoid_: session, batch

### Jito, Slots & Fees

**Bundle**:
An ordered, all-or-nothing group of transactions submitted to the Jito Block Engine for a specific leader slot. Carries the payload transaction(s) and the Tip.
_Avoid_: transaction group, batch

**Leader Window**:
The upcoming slot(s) led by a Jito-enabled validator, into which a Bundle can land. Detected via `getNextScheduledLeader` cross-referenced with the live slot stream.
_Avoid_: slot window, leader slot (when speaking of the targetable opportunity)

**Jito Leader**:
A validator running the Jito-Solana client that processes Bundles during its Leader Window. A Bundle can only land in a Jito Leader's slot.
_Avoid_: validator (too broad)

**Tip**:
The lamport payment attached to a Bundle that bids for inclusion in the Jito auction. Paid only if the Bundle lands. Computed dynamically from the Tip Floor — never hardcoded.
_Avoid_: fee, bribe, priority fee (a Tip is NOT a priority fee)

**Priority Fee**:
The compute-unit price (micro-lamports per CU) paid to the leader via a ComputeBudget instruction. Independent of the Tip; both may appear on the same transaction.
_Avoid_: tip, gas

**Tip Floor**:
The live distribution of recently-landed Tips (25th/50th/75th/95th/99th percentiles) published by Jito over WebSocket (REST fallback). The data source for the base Tip.
_Avoid_: tip price, going rate

**Tip Account**:
One of Jito's published accounts a Tip must be paid to. The Core rotates across them per Submission.
_Avoid_: tip wallet

### Failure & Recovery

**Failure Class**:
The classified cause of a non-landing Submission. Exactly four: Expired Blockhash, Fee Too Low, Compute Exceeded, Bundle Failure.
_Avoid_: error type, failure reason

**Fault Injection**:
Deliberately inducing a Failure Class to produce reproducible failures for the Lifecycle Log and to exercise the Agent — e.g. signing against a stale blockhash, setting a CU limit below need, or including a failing instruction.
_Avoid_: mocking, simulation (these failures are real on-chain, just deliberately caused)

**Organic Failure**:
A Failure that occurs naturally under live network conditions, not injected.
_Avoid_: real failure (injected failures are also real), random failure

**Blockhash Expiry**:
A Failure Class where the transaction's blockhash aged past its ~150-slot validity window before landing. The headline injected fault.
_Avoid_: timeout, stale transaction

**Remedy**:
The single action the Agent chooses in response to a Failure, drawn from a fixed decision space: refresh blockhash, bump Tip, raise CU limit, hold-and-resubmit, or abort. Executed by the Core.
_Avoid_: fix, retry strategy, action (use "Remedy" specifically for the Agent's chosen response)

### AI Agent

**Reasoning Trace**:
The Agent's extended-thinking output logged alongside each decision, together with the input context, chosen Remedy, and outcome. The primary evidence that the Agent reasons rather than runs a script.
_Avoid_: explanation, log (too generic), chain-of-thought

**Decision Space**:
The closed set of Remedies the Agent may choose from. Fixed and enumerated so the Agent's choice — and its variation across Failure Classes — is auditable.
_Avoid_: options, choices
