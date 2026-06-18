//! Jito bundle construction + submission (PLAN.md Day 1-2 tracer bullet, then 5-6).
//!
//! Pluggable instruction builder — default payload is a 1-lamport self-transfer
//! plus a Memo carrying a unique Run/Attempt nonce (the join key for Inclusion
//! detection), plus the Tip transfer to a rotated Tip Account. All-or-nothing.

use anyhow::{anyhow, Result};
use jito_sdk_rust::JitoJsonRpcSDK;
use solana_sdk::hash::Hash;
use solana_sdk::instruction::Instruction;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signer::keypair::Keypair;
use solana_sdk::signer::Signer;
use solana_sdk::transaction::Transaction;
use std::str::FromStr;

/// SPL Memo program — carries the Run/Attempt nonce (the Inclusion join key).
const MEMO_PROGRAM_ID: &str = "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr";

/// Compute Budget program — sets the CU limit + priority fee (price). Helius
/// Sender requires both a CU limit and a priority fee on every submission.
const COMPUTE_BUDGET_PROGRAM_ID: &str = "ComputeBudget111111111111111111111111111111";

/// `SetComputeUnitLimit` instruction (discriminator 2, then u32 units).
fn set_compute_unit_limit(units: u32) -> Instruction {
    let prog = Pubkey::from_str(COMPUTE_BUDGET_PROGRAM_ID).expect("valid compute budget id");
    let mut data = vec![0x02u8];
    data.extend_from_slice(&units.to_le_bytes());
    Instruction::new_with_bytes(prog, &data, vec![])
}

/// `SetComputeUnitPrice` instruction (discriminator 3, then u64 micro-lamports).
fn set_compute_unit_price(micro_lamports: u64) -> Instruction {
    let prog = Pubkey::from_str(COMPUTE_BUDGET_PROGRAM_ID).expect("valid compute budget id");
    let mut data = vec![0x03u8];
    data.extend_from_slice(&micro_lamports.to_le_bytes());
    Instruction::new_with_bytes(prog, &data, vec![])
}

/// Inputs to build the bundle's transaction(s). The default payload is a
/// 1-lamport self-transfer + a Memo carrying `nonce` + the Tip transfer.
pub struct BundleParams<'a> {
    pub payer: &'a Keypair,
    pub recent_blockhash: Hash,
    /// Run/Attempt nonce, carried in a Memo — the Inclusion join key.
    pub nonce: &'a str,
    /// A rotated Jito Tip Account (ADR 0005).
    pub tip_account: Pubkey,
    /// Tip in lamports (dynamic — set by the Core, never hardcoded; ADR 0005).
    pub tip_lamports: u64,
    /// Default-payload self-transfer amount in lamports (1 for the tracer bullet).
    pub self_transfer_lamports: u64,
    /// Optional Compute Budget unit limit — prepended when set. Required by the
    /// Helius Sender path (ADR 0007); left `None` for raw Jito bundles.
    pub compute_unit_limit: Option<u32>,
    /// Optional priority fee in micro-lamports per CU — prepended when set.
    pub priority_fee_microlamports: Option<u64>,
}

/// The DEFAULT payload (the pluggable unit): a Memo carrying the Run/Attempt nonce
/// (the Inclusion join key) + a self-transfer of `self_transfer_lamports`. Callers
/// can supply their own instructions via `build_bundle_with_payload` instead.
pub fn default_payload(payer: &Pubkey, nonce: &str, self_transfer_lamports: u64) -> Vec<Instruction> {
    let memo_id = Pubkey::from_str(MEMO_PROGRAM_ID).expect("valid memo program id");
    vec![
        Instruction::new_with_bytes(memo_id, nonce.as_bytes(), vec![]),
        solana_system_interface::instruction::transfer(payer, payer, self_transfer_lamports),
    ]
}

/// Build the bundle with the DEFAULT payload (Memo nonce + self-transfer). One
/// all-or-nothing transaction; the returned `Vec` is the Jito bundle (≤5 txs).
pub fn build_bundle(params: &BundleParams) -> Result<Vec<Transaction>> {
    let payload = default_payload(&params.payer.pubkey(), params.nonce, params.self_transfer_lamports);
    build_bundle_with_payload(params, payload)
}

/// Build the bundle with a CALLER-SUPPLIED payload (the pluggable instruction
/// builder). Compute-budget instructions (if set) are prepended and the Tip
/// transfer is appended around `payload`; the single tx is then signed.
pub fn build_bundle_with_payload(
    params: &BundleParams,
    payload: Vec<Instruction>,
) -> Result<Vec<Transaction>> {
    let payer_pk = params.payer.pubkey();

    let mut instructions: Vec<Instruction> = Vec::new();
    // Compute Budget instructions first (convention) — Sender requires both.
    if let Some(units) = params.compute_unit_limit {
        instructions.push(set_compute_unit_limit(units));
    }
    if let Some(price) = params.priority_fee_microlamports {
        instructions.push(set_compute_unit_price(price));
    }
    // Pluggable payload (default: Memo nonce + self-transfer).
    instructions.extend(payload);
    // Tip transfer to a rotated Jito Tip Account (ADR 0005). Pays only on landing.
    instructions.push(solana_system_interface::instruction::transfer(
        &payer_pk,
        &params.tip_account,
        params.tip_lamports,
    ));

    let tx = Transaction::new_signed_with_payer(
        &instructions,
        Some(&payer_pk),
        &[params.payer],
        params.recent_blockhash,
    );
    Ok(vec![tx])
}

/// Encode the bundle's transactions to the base64 wire form Jito's `sendBundle`
/// consumes (legacy bincode serialization, then base64). One string per tx.
pub fn encode_for_jito(txs: &[Transaction]) -> Result<Vec<String>> {
    use base64::Engine;
    txs.iter()
        .map(|tx| {
            let bytes = bincode::serialize(tx)?;
            Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
        })
        .collect()
}

/// The Jito JSON-RPC base appends `/bundles`, so it needs the `/api/v1` suffix.
fn jito_sdk(jito_base_url: &str) -> JitoJsonRpcSDK {
    JitoJsonRpcSDK::new(&format!("{}/api/v1", jito_base_url.trim_end_matches('/')), None)
}

/// The published Jito mainnet Tip Accounts (stable protocol constants; ADR 0005).
/// We use these directly rather than calling getTipAccounts — every Block Engine
/// call counts against Jito's 1 req/s/IP/region budget, so we spend that budget
/// only on the submit that matters.
const TIP_ACCOUNTS: [&str; 8] = [
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
    "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
];

/// The eight regional Jito mainnet Block Engines. Each region is a SEPARATE
/// rate-limit bucket (1 req/s/IP/region) AND forwards to whichever Jito-Solana
/// leader its region sees — so fanning one bundle across all eight both spreads
/// the budget and maximises the odds some region has a leader to forward to.
const JITO_REGIONS: [&str; 8] = [
    "https://amsterdam.mainnet.block-engine.jito.wtf",
    "https://dublin.mainnet.block-engine.jito.wtf",
    "https://frankfurt.mainnet.block-engine.jito.wtf",
    "https://london.mainnet.block-engine.jito.wtf",
    "https://ny.mainnet.block-engine.jito.wtf",
    "https://slc.mainnet.block-engine.jito.wtf",
    "https://singapore.mainnet.block-engine.jito.wtf",
    "https://tokyo.mainnet.block-engine.jito.wtf",
];

/// The eight regional Block Engine base URLs (fan-out targets for submission).
pub fn regional_endpoints() -> Vec<String> {
    JITO_REGIONS.iter().map(|s| s.to_string()).collect()
}

/// The lowercase region id for each fan-out endpoint (e.g. "frankfurt"), DERIVED
/// from `JITO_REGIONS` so the two can't drift. Passed to `getNextScheduledLeader`
/// so the leader-window signal covers exactly the regions we submit to (not just
/// the searcher's single connected region).
pub fn region_names() -> Vec<String> {
    JITO_REGIONS
        .iter()
        .map(|url| {
            url.trim_start_matches("https://")
                .split('.')
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The published Tip Accounts as Pubkeys — no network call (ADR 0005 constants).
pub fn published_tip_accounts() -> Vec<Pubkey> {
    TIP_ACCOUNTS
        .iter()
        .map(|s| Pubkey::from_str(s).expect("valid published tip account"))
        .collect()
}

/// The published Jito Tip Accounts (fetched once, rotated locally; ADR 0005).
/// Falls back to the published constants if the engine is rate-limited.
pub async fn tip_accounts(jito_base_url: &str) -> Result<Vec<Pubkey>> {
    let parse = |strs: &[&str]| -> Result<Vec<Pubkey>> {
        strs.iter()
            .map(|s| Pubkey::from_str(s))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| anyhow!("bad tip account: {e}"))
    };

    match jito_sdk(jito_base_url).get_tip_accounts().await {
        Ok(resp) => {
            if let Some(arr) = resp["result"].as_array() {
                let strs: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
                if !strs.is_empty() {
                    return parse(&strs);
                }
            }
            tracing::warn!("getTipAccounts unusable ({resp}); using published constants");
            parse(&TIP_ACCOUNTS)
        }
        Err(e) => {
            tracing::warn!("getTipAccounts failed ({e}); using published constants");
            parse(&TIP_ACCOUNTS)
        }
    }
}

/// Submit pre-encoded transactions to one Block Engine. Returns the bundle id.
///
/// We build the fully-wrapped params form `[[tx,...], {"encoding":"base64"}]`
/// OURSELVES rather than passing a bare `[tx,...]`. The jito-sdk-rust v0.3.2
/// `send_bundle` has a length heuristic: a 2-element array is forwarded verbatim,
/// so a bare 2-tx bundle would be misread as `[txA, txB]` — txB taken as the
/// encoding-options object, silently invalidating every 2-tx bundle. Passing the
/// wrapped form (itself a 2-element array) hits that pass-through arm on purpose,
/// with the correct shape, for any bundle size 1..=5.
async fn submit_encoded(
    jito_base_url: &str,
    encoded: &[String],
    auth_uuid: Option<&str>,
) -> Result<String> {
    let params = serde_json::json!([encoded, { "encoding": "base64" }]);
    let resp = jito_sdk(jito_base_url)
        .send_bundle(Some(params), auth_uuid)
        .await?;
    resp["result"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| anyhow!("no bundle id in sendBundle response: {resp}"))
}

/// Submit one bundle to a single Block Engine (ADR 0002). Returns the bundle id.
/// `auth_uuid` is the optional Jito `x-jito-auth` UUID for rate-limit headroom.
pub async fn submit_one(
    jito_base_url: &str,
    txs: &[Transaction],
    auth_uuid: Option<&str>,
) -> Result<String> {
    let encoded = encode_for_jito(txs)?;
    submit_encoded(jito_base_url, &encoded, auth_uuid).await
}

/// Fan one bundle out to many regional Block Engines concurrently — one submit
/// per region (each region its own 1 req/s budget). Returns `(region, result)`
/// per endpoint; a region's `Err` (rate limit, network) never aborts the others.
pub async fn submit_all_regions(
    regions: &[String],
    txs: &[Transaction],
    auth_uuid: Option<&str>,
) -> Vec<(String, Result<String>)> {
    let encoded = match encode_for_jito(txs) {
        Ok(e) => e,
        Err(e) => {
            let msg = e.to_string();
            return regions
                .iter()
                .map(|r| (r.clone(), Err(anyhow!("encode failed: {msg}"))))
                .collect();
        }
    };

    let mut handles = Vec::with_capacity(regions.len());
    for region in regions {
        let region_owned = region.clone();
        let encoded_owned = encoded.clone();
        let auth_owned = auth_uuid.map(String::from);
        let handle = tokio::spawn(async move {
            submit_encoded(&region_owned, &encoded_owned, auth_owned.as_deref()).await
        });
        handles.push((region.clone(), handle));
    }

    let mut out = Vec::with_capacity(handles.len());
    for (region, handle) in handles {
        let res = match handle.await {
            Ok(r) => r,
            Err(e) => Err(anyhow!("submit task panicked: {e}")),
        };
        out.push((region, res));
    }
    out
}

/// One-shot read of a bundle's inflight status string (Pending/Landed/Failed/Invalid).
pub async fn inflight_status(jito_base_url: &str, bundle_id: &str) -> Result<String> {
    let resp = jito_sdk(jito_base_url)
        .get_in_flight_bundle_statuses(vec![bundle_id.to_string()])
        .await?;
    Ok(resp["result"]["value"][0]["status"]
        .as_str()
        .unwrap_or("Unknown")
        .to_string())
}

/// Poll Jito for the bundle's landing. `Ok(Some(slot))` once Landed, `Ok(None)`
/// on timeout, `Err` if Jito reports it Failed/Invalid.
pub async fn await_landed(
    jito_base_url: &str,
    bundle_id: &str,
    tries: u32,
    delay_ms: u64,
) -> Result<Option<u64>> {
    let sdk = jito_sdk(jito_base_url);
    for _ in 0..tries {
        let resp = sdk
            .get_in_flight_bundle_statuses(vec![bundle_id.to_string()])
            .await?;
        if let Some(v) = resp["result"]["value"].as_array().and_then(|a| a.first()) {
            match v["status"].as_str().unwrap_or("") {
                "Landed" => return Ok(Some(v["landed_slot"].as_u64().unwrap_or(0))),
                "Failed" => return Ok(None), // definitively not landed this window
                // "Invalid" (status API hasn't registered it yet) / "Pending" -> keep polling
                _ => {}
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::message::Message;

    #[test]
    fn region_names_derive_from_endpoints() {
        let names = region_names();
        assert_eq!(names.len(), JITO_REGIONS.len());
        assert_eq!(names[0], "amsterdam");
        assert!(names.contains(&"frankfurt".to_string()));
        // No scheme/host leakage in the derived ids.
        assert!(names.iter().all(|n| !n.is_empty() && !n.contains('.') && !n.contains('/')));
    }

    fn fixture(payer: &Keypair) -> BundleParams<'_> {
        BundleParams {
            payer,
            recent_blockhash: Hash::default(),
            nonce: "run-0001:attempt-01:0001",
            tip_account: Pubkey::new_unique(),
            tip_lamports: 10_000,
            self_transfer_lamports: 1,
            compute_unit_limit: None,
            priority_fee_microlamports: None,
        }
    }

    /// True if `msg` contains a System transfer of `lamports` from `from` to `to`.
    fn has_system_transfer(msg: &Message, from: Pubkey, to: Pubkey, lamports: u64) -> bool {
        use std::str::FromStr;
        let sys_id = Pubkey::from_str("11111111111111111111111111111111").unwrap();
        msg.instructions.iter().any(|ci| {
            let prog = msg.account_keys[ci.program_id_index as usize];
            let accts: Vec<Pubkey> =
                ci.accounts.iter().map(|i| msg.account_keys[*i as usize]).collect();
            prog == sys_id
                && accts == [from, to]
                && ci.data.len() == 12
                && ci.data[..4] == [2u8, 0, 0, 0] // SystemInstruction::Transfer variant
                && u64::from_le_bytes(ci.data[4..12].try_into().unwrap()) == lamports
        })
    }

    #[test]
    fn builds_exactly_one_transaction() {
        let payer = Keypair::new();
        let bundle = build_bundle(&fixture(&payer)).expect("build");
        assert_eq!(bundle.len(), 1, "default payload is a single all-or-nothing tx");
    }

    #[test]
    fn carries_memo_with_nonce() {
        use std::str::FromStr;
        let payer = Keypair::new();
        let p = fixture(&payer);
        let bundle = build_bundle(&p).expect("build");
        let msg = &bundle[0].message;
        let memo_id = Pubkey::from_str(MEMO_PROGRAM_ID).unwrap();
        let found = msg.instructions.iter().any(|ci| {
            msg.account_keys[ci.program_id_index as usize] == memo_id
                && ci.data == p.nonce.as_bytes()
        });
        assert!(found, "a Memo instruction must carry the nonce as its data");
    }

    #[test]
    fn carries_self_transfer_of_n_lamports() {
        let payer = Keypair::new();
        let p = fixture(&payer);
        let bundle = build_bundle(&p).expect("build");
        let payer_pk = payer.pubkey();
        assert!(
            has_system_transfer(&bundle[0].message, payer_pk, payer_pk, p.self_transfer_lamports),
            "must contain a payer->payer system transfer of self_transfer_lamports"
        );
    }

    #[test]
    fn carries_tip_transfer_to_tip_account() {
        let payer = Keypair::new();
        let p = fixture(&payer);
        let bundle = build_bundle(&p).expect("build");
        assert!(
            has_system_transfer(&bundle[0].message, payer.pubkey(), p.tip_account, p.tip_lamports),
            "must contain a payer->tip_account transfer of tip_lamports"
        );
    }

    #[test]
    fn is_signed_by_payer_and_verifies() {
        let payer = Keypair::new();
        let bundle = build_bundle(&fixture(&payer)).expect("build");
        let tx = &bundle[0];
        assert_eq!(tx.message.account_keys[0], payer.pubkey(), "payer is the fee payer");
        assert_eq!(tx.signatures.len(), 1, "exactly one signature (the payer)");
        tx.verify().expect("signature must verify against the message");
    }

    #[test]
    fn prepends_compute_budget_when_set() {
        let payer = Keypair::new();
        let mut p = fixture(&payer);
        p.compute_unit_limit = Some(20_000);
        p.priority_fee_microlamports = Some(100_000);
        let bundle = build_bundle(&p).expect("build");
        let msg = &bundle[0].message;
        let cb = Pubkey::from_str(COMPUTE_BUDGET_PROGRAM_ID).unwrap();
        // The compute-budget instructions: CU limit (disc 2) then price (disc 3).
        let cb_data: Vec<&[u8]> = msg
            .instructions
            .iter()
            .filter(|ci| msg.account_keys[ci.program_id_index as usize] == cb)
            .map(|ci| ci.data.as_slice())
            .collect();
        assert_eq!(cb_data.len(), 2, "both compute-budget instructions present");
        assert_eq!(cb_data[0][0], 0x02, "first is SetComputeUnitLimit");
        assert_eq!(cb_data[1][0], 0x03, "second is SetComputeUnitPrice");
    }

    #[test]
    fn omits_compute_budget_when_unset() {
        let payer = Keypair::new();
        let bundle = build_bundle(&fixture(&payer)).expect("build");
        let msg = &bundle[0].message;
        let cb = Pubkey::from_str(COMPUTE_BUDGET_PROGRAM_ID).unwrap();
        assert!(
            !msg.instructions
                .iter()
                .any(|ci| msg.account_keys[ci.program_id_index as usize] == cb),
            "no compute-budget instructions for a raw Jito bundle"
        );
    }

    #[test]
    fn encodes_to_jito_wire_form_roundtrip() {
        use base64::Engine;
        let payer = Keypair::new();
        let bundle = build_bundle(&fixture(&payer)).expect("build");

        let encoded = encode_for_jito(&bundle).expect("encode");
        assert_eq!(encoded.len(), bundle.len(), "one wire string per tx");

        let raw = base64::engine::general_purpose::STANDARD
            .decode(&encoded[0])
            .expect("valid base64");
        let decoded: Transaction = bincode::deserialize(&raw).expect("valid bincode tx");
        assert_eq!(&decoded, &bundle[0], "wire form must round-trip to the same tx");
        decoded.verify().expect("decoded tx still verifies");
    }

    #[test]
    fn build_with_custom_payload_replaces_default() {
        let payer = Keypair::new();
        let p = fixture(&payer);
        let memo_id = Pubkey::from_str(MEMO_PROGRAM_ID).unwrap();
        let custom = vec![Instruction::new_with_bytes(memo_id, b"custom-payload", vec![])];

        let bundle = build_bundle_with_payload(&p, custom).expect("build");
        let msg = &bundle[0].message;

        // The custom instruction is present...
        let has_custom = msg.instructions.iter().any(|ci| {
            msg.account_keys[ci.program_id_index as usize] == memo_id && ci.data == b"custom-payload"
        });
        assert!(has_custom, "custom payload instruction must be included");

        // ...the default nonce-memo is NOT (the payload was replaced)...
        let has_default_nonce = msg.instructions.iter().any(|ci| {
            msg.account_keys[ci.program_id_index as usize] == memo_id && ci.data == p.nonce.as_bytes()
        });
        assert!(!has_default_nonce, "custom payload replaces the default nonce memo");

        // ...and the Tip transfer is still appended around the custom payload.
        assert!(
            has_system_transfer(msg, payer.pubkey(), p.tip_account, p.tip_lamports),
            "tip transfer must still wrap a custom payload"
        );
    }
}
