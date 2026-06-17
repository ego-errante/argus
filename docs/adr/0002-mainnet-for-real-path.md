# Run the real path on mainnet, not devnet

The Lifecycle Log and all real bundle submissions run on **mainnet**, using SolInfra-sponsored credits (free premium RPC + Yellowstone gRPC) and a dedicated low-balance keypair. Devnet is retained only as a destructive/chaos-testing sandbox.

## Why

The spec says "Devnet or mainnet," but this is a constraint not visible in the code: **Jito's Block Engine only lands bundles on mainnet** (and testnet) — devnet has no block engine — and judges verify slot numbers on Solana explorers. Therefore the core requirement (real Jito bundles with explorer-verifiable slots) is *only satisfiable on mainnet*. Free sponsor infra removes the cost argument for devnet; the only mainnet exposure is tiny tips/fees, contained by the throwaway keypair.

## Consequences

A real funded keypair must be handled carefully (low balance, not committed). The Jito-specific path cannot be developed or validated on devnet, so it is built against mainnet from day one. Blockhash-expiry faults are injected on mainnet directly (sign against a stale blockhash) rather than requiring devnet.
