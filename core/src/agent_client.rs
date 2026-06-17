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
}

pub struct AgentClient {
    http: reqwest::Client,
    url: String,
}

impl AgentClient {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
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
