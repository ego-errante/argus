//! Environment-driven configuration. Network swap (mainnet/devnet) lives here,
//! per ADR 0002. Missing required vars are warned, not fatal, so the scaffold
//! runs as a smoke test before SolInfra onboarding.

#[derive(Debug, Clone)]
pub struct Config {
    pub network: String, // "mainnet" | "devnet"
    pub rpc_http_url: String,
    pub rpc_ws_url: String,
    pub yellowstone_grpc_url: String,
    pub yellowstone_x_token: Option<String>,
    pub jito_block_engine_url: String,
    /// Jito SearcherService gRPC endpoint (NoAuth) for `getNextScheduledLeader`
    /// (ADR 0008). Must be a REGIONAL block-engine host — the bare mainnet host
    /// does not reliably serve the searcher gRPC. e.g. frankfurt.mainnet.*.
    pub jito_searcher_grpc_url: String,
    pub jito_tip_floor_url: String,
    /// Jito Tip Floor percentile used for the Base Tip (one of 25/50/75/95/99).
    /// Selects the `landed_tips_{p}th_percentile` field (ADR 0005 — no hardcoded tip).
    pub jito_tip_percentile: u8,
    /// Optional Jito `x-jito-auth` UUID — grants higher rate limits / forwarding
    /// headroom on the Block Engine. Unset = unauthenticated public tier.
    pub jito_auth_uuid: Option<String>,
    /// Helius Sender endpoint — primary submission path (ADR 0007). Keyless;
    /// dual-routes through staked validators + Jito. `?swqos_only=true` for the
    /// cheaper staked-only lane.
    pub helius_sender_url: String,
    /// Use Sender's staked-only lane (cheaper tip) instead of dual-routing.
    pub helius_swqos_only: bool,
    /// Sender per-route minimum tips + mandated compute budget (ADR 0007 backstop).
    /// Configurable; default to Sender's documented values.
    pub sender_dual_min_tip_lamports: u64,
    pub sender_swqos_min_tip_lamports: u64,
    pub sender_compute_unit_limit: u32,
    pub sender_priority_fee_microlamports: u64,
    pub keypair_path: String,
    pub agent_url: String,
    pub db_path: String,
}

fn get(key: &str, missing: &mut Vec<String>) -> String {
    match std::env::var(key) {
        Ok(v) => v,
        Err(_) => {
            missing.push(key.to_string());
            String::new()
        }
    }
}

/// Parse a numeric env var, falling back to `default` when unset or unparseable.
fn env_num<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(default)
}

/// The Jito Tip Floor only publishes 25/50/75/95/99th percentiles; anything else
/// has no field to read. Default 75 (landing-biased); warn + fall back on a bad value.
const SUPPORTED_TIP_PERCENTILES: [u8; 5] = [25, 50, 75, 95, 99];
fn parse_tip_percentile(raw: Option<String>) -> u8 {
    match raw.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => 75,
        Some(s) => match s.parse::<u8>() {
            Ok(p) if SUPPORTED_TIP_PERCENTILES.contains(&p) => p,
            _ => {
                tracing::warn!(
                    value = s,
                    "JITO_TIP_PERCENTILE must be one of 25/50/75/95/99 — using 75"
                );
                75
            }
        },
    }
}

impl Config {
    pub fn from_env() -> Self {
        let _ = dotenvy::dotenv();
        let mut missing = Vec::new();

        let cfg = Config {
            network: std::env::var("ARGUS_NETWORK").unwrap_or_else(|_| "mainnet".into()),
            rpc_http_url: get("RPC_HTTP_URL", &mut missing),
            rpc_ws_url: get("RPC_WS_URL", &mut missing),
            yellowstone_grpc_url: get("YELLOWSTONE_GRPC_URL", &mut missing),
            yellowstone_x_token: std::env::var("YELLOWSTONE_X_TOKEN").ok(),
            jito_block_engine_url: std::env::var("JITO_BLOCK_ENGINE_URL")
                .unwrap_or_else(|_| "https://mainnet.block-engine.jito.wtf".into()),
            jito_searcher_grpc_url: std::env::var("JITO_SEARCHER_GRPC_URL")
                .unwrap_or_else(|_| "https://frankfurt.mainnet.block-engine.jito.wtf".into()),
            jito_tip_floor_url: std::env::var("JITO_TIP_FLOOR_URL")
                .unwrap_or_else(|_| "https://bundles.jito.wtf/api/v1/bundles/tip_floor".into()),
            jito_tip_percentile: parse_tip_percentile(std::env::var("JITO_TIP_PERCENTILE").ok()),
            jito_auth_uuid: std::env::var("JITO_AUTH_UUID")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            helius_sender_url: std::env::var("HELIUS_SENDER_URL")
                .unwrap_or_else(|_| "https://sender.helius-rpc.com/fast".into()),
            helius_swqos_only: std::env::var("HELIUS_SWQOS_ONLY")
                .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
                .unwrap_or(false),
            sender_dual_min_tip_lamports: env_num(
                "SENDER_DUAL_MIN_TIP_LAMPORTS",
                crate::sender::DUAL_MIN_TIP_LAMPORTS,
            ),
            sender_swqos_min_tip_lamports: env_num(
                "SENDER_SWQOS_MIN_TIP_LAMPORTS",
                crate::sender::SWQOS_MIN_TIP_LAMPORTS,
            ),
            sender_compute_unit_limit: env_num("SENDER_COMPUTE_UNIT_LIMIT", 20_000u32),
            sender_priority_fee_microlamports: env_num(
                "SENDER_PRIORITY_FEE_MICROLAMPORTS",
                100_000u64,
            ),
            keypair_path: std::env::var("KEYPAIR_PATH")
                .unwrap_or_else(|_| "./secrets/keypair.json".into()),
            agent_url: std::env::var("AGENT_URL")
                .unwrap_or_else(|_| "http://localhost:8787/decide".into()),
            db_path: std::env::var("ARGUS_DB_PATH").unwrap_or_else(|_| "logs/argus.db".into()),
        };

        if !missing.is_empty() {
            tracing::warn!(
                ?missing,
                "missing env vars — copy .env.example to .env and fill in before the tracer bullet"
            );
        }
        cfg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tip_percentile_accepts_supported_values() {
        for p in SUPPORTED_TIP_PERCENTILES {
            assert_eq!(parse_tip_percentile(Some(p.to_string())), p);
        }
    }

    #[test]
    fn tip_percentile_defaults_and_rejects_unsupported() {
        assert_eq!(parse_tip_percentile(None), 75, "unset -> default 75");
        assert_eq!(parse_tip_percentile(Some("  ".into())), 75, "blank -> default 75");
        assert_eq!(parse_tip_percentile(Some("60".into())), 75, "unsupported -> default 75");
        assert_eq!(parse_tip_percentile(Some("nonsense".into())), 75, "unparseable -> default 75");
    }
}
