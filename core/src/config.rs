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
    pub jito_tip_floor_url: String,
    /// Optional Jito `x-jito-auth` UUID — grants higher rate limits / forwarding
    /// headroom on the Block Engine. Unset = unauthenticated public tier.
    pub jito_auth_uuid: Option<String>,
    /// Helius Sender endpoint — primary submission path (ADR 0007). Keyless;
    /// dual-routes through staked validators + Jito. `?swqos_only=true` for the
    /// cheaper staked-only lane.
    pub helius_sender_url: String,
    /// Use Sender's staked-only lane (cheaper tip) instead of dual-routing.
    pub helius_swqos_only: bool,
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
            jito_tip_floor_url: std::env::var("JITO_TIP_FLOOR_URL")
                .unwrap_or_else(|_| "https://bundles.jito.wtf/api/v1/bundles/tip_floor".into()),
            jito_auth_uuid: std::env::var("JITO_AUTH_UUID")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            helius_sender_url: std::env::var("HELIUS_SENDER_URL")
                .unwrap_or_else(|_| "https://sender.helius-rpc.com/fast".into()),
            helius_swqos_only: std::env::var("HELIUS_SWQOS_ONLY")
                .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
                .unwrap_or(false),
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
