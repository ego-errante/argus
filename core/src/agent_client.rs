//! HTTP boundary to the TS Agent (ADR 0001). The Core POSTs a FailureContext on
//! each Failure and receives a Decision { remedy, rationale, confidence } plus
//! the Reasoning Trace. This is the ONLY contract between Core and Agent.

use crate::model::{FailureClass, Remedy};
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Sent to the Agent's /decide endpoint. Field names/enums match agent/src/types.ts.
#[derive(Debug, Serialize)]
pub struct FailureContext<'a> {
    pub failure_class: FailureClass,
    pub attempt: u32,
    pub error_text: &'a str,
    pub tip_lamports: u64,
    pub tip_floor_p50: u64,
    pub tip_floor_p75: u64,
    pub blockhash_age_slots: Option<u64>,
    pub cu_limit: Option<u32>,
    pub cu_used: Option<u32>,
    pub current_slot: u64,
}

/// The Agent's structured decision + summarized thinking (the Reasoning Trace).
#[derive(Debug, Deserialize)]
pub struct Decision {
    pub remedy: Remedy,
    pub rationale: String,
    pub confidence: f64,
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
    pub fn new(url: impl Into<String>, timeout_secs: u64) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            url: url.into(),
        }
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
