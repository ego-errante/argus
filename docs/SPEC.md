# Challenge Spec — Advanced Infrastructure Challenge: Build a Smart Transaction Stack

> **Verbatim reference copy** of the official Superteam Earn listing, captured for offline
> reference. Source: <https://superteam.fun/earn/listing/advanced-infrastructure-challenge-build-a-smart-transaction-stack>
> Captured 2026-06-21 by rendering the (JavaScript-hosted) listing in a headless browser.
> Site navigation, "related listings", comment threads, and footer chrome are omitted; all
> challenge content below is reproduced as published.

## Listing metadata

| Field | Value |
|---|---|
| Sponsor | Superteam Nigeria |
| Type | Bounty |
| Status | Submissions Open |
| Region | **Nigeria only** (regional listing) |
| Total prize pool | 5,000 USDG |
| 1st / 2nd / 3rd | 2,500 / 1,500 / 1,000 USDG |
| Skills needed | Backend, Blockchain, Content |
| Submissions (at capture) | 45 |
| Time remaining (at capture) | 8d 5h 41m → submission deadline ≈ **2026-06-29** |
| Winner announcement | July 13, 2026 (as scheduled by the sponsor) |

---

## Overview

On Solana, sending a transaction is only one small part of the story.

There are different lifecycles in the network before a transaction lands; leader scheduling, TPU ingestion, block production, shred propagation, and multiple commitment stages. Production systems need to understand this entire flow, react to failures correctly, and make smart decisions under changing network conditions.

This bounty focuses on building a real transaction infrastructure stack powered by:

- Jito bundles
- Live Yellowstone/Geyser streaming
- Transaction lifecycle tracking
- AI-assisted decision making

You will build a smart transaction stack that observes the network in real time, submits transactions intelligently, tracks outcomes across commitment levels, and uses an AI agent to make one meaningful operational decision autonomously.

This is an infrastructure-heavy challenge designed for developers who want to go deeper into how Solana transactions actually move through the network.

---

## Requirements

### 1. Architecture Design Document

Your submission must include a public architecture document hosted separately from your GitHub repository.

Accepted formats:

- Figma
- Notion
- Google Docs
- Any public URL

Your document should explain:

- The system architecture
- Key components
- Data flow between services
- Infrastructure decisions
- Failure handling strategy
- AI agent responsibilities

Diagrams are strongly encouraged.

This document will be judged separately, so clarity and depth matter.

### 2. The Transaction Stack

Build a working smart transaction stack that can:

- Monitor live slot and leader data using:
  - Yellowstone gRPC
  - Or any compatible Geyser stream provider
- Detect the correct leader window for submission
- Construct and submit Jito bundles
- Calculate bundle tips dynamically using:
  - Real recent tip account data
  - Current network conditions
  - **No hardcoded tip values**
- Track transaction lifecycle stages:
  - Submitted
  - Processed
  - Confirmed
  - Finalized
- Capture:
  - Timestamps
  - Slot numbers
  - Latency deltas between stages
- Detect and classify failures:
  - Expired blockhash
  - Fee too low
  - Compute exceeded
  - Bundle failure
- Confirm landing using stream subscriptions
  - **RPC polling alone is not sufficient**
- Handle retries automatically
  - Including blockhash refresh on expiry

### 3. Lifecycle Log

Your submission must include a lifecycle log from at least:

- **10 real bundle submissions**
- Including at least **2 failure cases**

Each log entry should contain:

- Slot numbers
- Commitment progression
- Timestamps
- Tip amounts
- Failure classification (if applicable)

Judges will cross-reference slot numbers using Solana explorers to verify that your stack ran on real infrastructure.

### 4. AI Agent Demonstration

You must build an AI agent that owns one real operational decision inside your stack.

Choose one of the following:

**Failure Reasoning**

The agent:

- Observes failed transactions
- Reasons about why they failed
- Decides what should change before retrying

Retry decisions must come from the agent itself, not hardcoded logic.

**Tip Intelligence**

The agent:

- Analyzes recent tip account data
- Monitors current slot conditions
- Decides how much to tip for each bundle

The reasoning process should balance:

- Cost
- Landing probability

**Submission Timing**

The agent:

- Watches slot streams and leader schedules
- Decides when to submit
- Holds transactions when conditions are unfavorable

**Autonomous Retry with Fault Injection**

Your stack must:

- Simulate at least one blockhash expiry failure

The agent must:

- Detect the failure
- Reason about the cause
- Refresh the blockhash
- Recalculate the tip
- Resubmit autonomously

No hardcoded retry flow allowed.

The AI agent must make real decisions.

A simple wrapper that calls functions sequentially without reasoning will not qualify.

### 5. README Questions

We want submissions that show real operational understanding, not just working code.

Your README should explain the observations, tradeoffs, and lessons from your running system. Strong submissions will clearly show that the builder understands how Solana transactions behave under real network conditions.

Your README must answer these three questions:

**Question 1**

What does the delta between `processed_at` and `confirmed_at` tell you about network health at the time of submission?

**Question 2**

Why should you never use finalized commitment when fetching a blockhash for a time-sensitive transaction?

**Question 3**

What happens to your bundle if the Jito leader skips their slot?

These questions have specific correct answers. Responses based on real observations from your running infrastructure will score highest.

### 6. General Requirements

Your submission must include:

- Open-source code
- Clear setup instructions
- Working prototype on:
  - Devnet
  - Or mainnet

You may build:

- Solo
- Or with a team

---

## Technical Expectations

Your project should demonstrate:

- Correct slot streaming implementation
- Proper reconnection and backpressure handling
- Real Jito bundle construction
- Dynamic tip logic from live data
- Proper use of commitment levels
- Clean separation between:
  - AI layer
  - Core transaction stack

Failure handling is required.

Happy-path-only submissions will not score well.

---

## Judging Criteria

**Does It Work?**

- Functional stack
- Real lifecycle logs
- Successful and failed submissions demonstrated

**Depth of Integration**

- Proper use of Jito and streaming infrastructure
- Correct commitment handling
- No hardcoded shortcuts
- Strong understanding of Solana transaction flow

**AI Demonstration**

- Agent makes meaningful decisions
- Reasoning is visible
- Not simple sequential automation

**Explanation**

- Quality of architecture document
- README depth
- Evidence of real operational understanding
- Observations from actual system behavior

---

## Prizes

A total prize pool of $5,000 USDG will be distributed among the top submissions:

- 1st Place: $2,500
- 2nd Place: $1,500
- 3rd Place: $1,000

---

## Infrastructure Support

SolInfra is powering builders in this bounty with up to $20,000 in infrastructure credits, including free access to:

- High-performance RPC nodes
- Yellowstone gRPC access
- Premium Solana infrastructure tooling
- Technical support throughout the bounty period

This support is intended to help teams build, test, and scale without running into infrastructure bottlenecks during development.

---

## Resources

- Jito TypeScript SDK — <https://github.com/jito-labs/jito-ts>
- Jito Rust JSON-RPC SDK — <https://github.com/jito-labs/jito-rust-rpc>
- Jito Documentation — <https://docs.jito.wtf>
- Yellowstone gRPC — <https://github.com/rpcpool/yellowstone-grpc>
- Triton Yellowstone Documentation — <https://docs.triton.one/project-yellowstone/dragons-mouth-grpc-subscriptions>
- Solana JSON RPC API — <https://solana.com/docs/rpc>
- Solana Documentation — <https://solana.com/docs>
