//! HTTP boundary to the TS Agent (ADR 0001). The Core POSTs the raw FailureContext on
//! each Failure and receives a Decision { diagnosis, triage, remedy, rationale, confidence }
//! plus the Reasoning Trace and serving-model slug (ADR 0012). This is the ONLY contract
//! between Core and Agent.

use crate::model::{FailureClass, Remedy, Triage};
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Sent to the Agent's /decide endpoint. Field names/enums match agent/src/types.ts.
#[derive(Debug, Serialize)]
pub struct FailureContext<'a> {
    /// The Core's deterministic baseline class. Used Rust-side by the Local policy, the
    /// agent-unreachable fallback, and the Lifecycle Log's baseline column — but `serde(skip)`,
    /// so it is NEVER sent to the Agent: handing it the classifier's verdict is the lookup
    /// ADR 0012 removes. The Agent reasons from the raw surface below instead.
    #[serde(skip)]
    pub failure_class: FailureClass,
    pub attempt: u32,
    pub error_text: &'a str,
    /// The raw failure surface the Agent reasons over (ADR 0012): the structured instruction
    /// error variant (e.g. `{"Custom":101}`), the program that rejected the transaction, and the
    /// full program logs. This unstructured, program-specific input is the Agent's irreducible
    /// edge over the four-class baseline. Honestly optional where extraction can fail.
    pub instruction_error: Option<&'a str>,
    pub failing_program_id: Option<&'a str>,
    pub program_logs: &'a [String],
    pub tip_lamports: u64,
    // Context the Agent reasons over, honestly optional: `None` means "couldn't fetch"
    // (serialized as JSON `null`), never a fabricated 0/base — so the Agent can tell a
    // real floor/slot from a missing one. Matches the already-optional fields below.
    pub tip_floor_p50: Option<u64>,
    pub tip_floor_p75: Option<u64>,
    pub blockhash_age_slots: Option<u64>,
    pub cu_limit: Option<u32>,
    pub cu_used: Option<u32>,
    pub current_slot: Option<u64>,
}

/// The Agent's structured decision + summarized thinking (the Reasoning Trace).
#[derive(Debug, Deserialize)]
pub struct Decision {
    pub remedy: Remedy,
    pub rationale: String,
    pub confidence: f64,
    /// The Agent's plain-language read of the cause (ADR 0012) — the genuinely-reasoned output,
    /// inferred from the raw surface rather than a pre-assigned class. `None` on the local paths.
    #[serde(default)]
    pub diagnosis: Option<String>,
    /// The Agent's recovery-bucket sort (ADR 0012). `None` on the local paths.
    #[serde(default)]
    pub triage: Option<Triage>,
    #[serde(default)]
    pub reasoning_trace: Option<String>,
    /// The model that actually served the decision (post-fallback OpenRouter slug),
    /// or a `local`/`local-fallback` marker for the local policy paths. Logged per
    /// decision so the Run can confirm every scored decision carried a trace (ADR 0006).
    #[serde(default)]
    pub model: Option<String>,
}

pub struct AgentClient {
    http: reqwest::Client,
    url: String,
}

impl AgentClient {
    /// `timeout_secs` bounds the decide round-trip. A reasoning completion over
    /// OpenRouter is genuinely slow (tens of seconds), so the default is generous
    /// (~45s); a truly dead Agent trips the timeout, which the caller turns into a
    /// loud, recorded fallback to the local policy rather than an indefinite hang.
    pub fn new(url: impl Into<String>, timeout_secs: u64) -> Result<Self> {
        // Propagate a builder failure rather than swallowing it into a no-timeout
        // client — a silently-unbounded client would defeat ARGUS_AGENT_TIMEOUT_SECS
        // and let a dead Agent hang the Run instead of degrading to the local fallback.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .build()?;
        Ok(Self {
            http,
            url: url.into(),
        })
    }

    /// Liveness probe for the Run's preflight (ADR 0011). A scored Run must NOT silently
    /// degrade to `local-fallback` (ADR 0006), so the orchestrator refuses to start if the
    /// Agent's `/health` doesn't answer 2xx. Derives the health URL from the decide URL
    /// (same host, sibling path) so there's one configured endpoint.
    pub async fn health(&self) -> Result<()> {
        let health_url = match self.url.rsplit_once('/') {
            Some((base, _)) => format!("{base}/health"),
            None => format!("{}/health", self.url),
        };
        self.http
            .get(&health_url)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn decide(&self, ctx: &FailureContext<'_>) -> Result<Decision> {
        let resp = self
            .http
            .post(&self.url)
            .json(ctx)
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json::<Decision>().await?)
    }
}
