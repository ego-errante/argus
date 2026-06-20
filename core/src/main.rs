#![allow(dead_code)]
//! Argus Core — the Rust transaction-infrastructure service.
//!
//! Terminology: ../../CONTEXT.md. Build sequence + decisions: ../../docs/PLAN.md
//! and ../../docs/adr/. Argus is the hundred-eyed watchman: this process is the
//! eyes (streaming + lifecycle); the TS Agent is the judgment (failure recovery).

mod agent_client;
mod config;
mod export; // Day 11: Lifecycle Log render (Markdown two-part + JSONL) from SQLite
mod model;
mod storage;

// Filled in across the PLAN.md milestones:
mod bundle; // Day 5-6: Jito bundle construction + submission
mod failure; // Day 7-8: fault injection + classification + local remedy policy
mod leader; // Day 5-6: getNextScheduledLeader + slot stream -> Leader Window
mod sender; // Day 1-2: Helius Sender submission (primary landing path, ADR 0007)
mod streaming; // Day 3-4 + 7-8: Yellowstone slot-sub + tx-sub, backpressure, reconnect
mod rpc; // Day 1-2: minimal JSON-RPC (getLatestBlockhash)
mod tip; // Day 5-6: Tip Floor percentile + tip-account rotation
mod wallet; // Day 1-2: load the signing keypair from secrets/

use anyhow::Result;
use solana_sdk::signer::Signer;
use tracing::{info, warn};

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis()
}

#[tokio::main]
async fn main() -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    // rustls 0.23 needs an explicit process-wide crypto provider (tonic pulls
    // rustls in without selecting one). Install ring before any TLS connection.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cfg = config::Config::from_env();
    info!(network = %cfg.network, "Argus core starting");

    let store = storage::Store::open(&cfg.db_path)?;
    store.init_schema()?;
    info!(db = %cfg.db_path, "lifecycle store ready");

    // Diagnostic mode (ARGUS_DIAG=1): one high-tip bundle + live inflight polling,
    // NO Sender backstop — a free probe of why unauthed bundles aren't landing.
    if std::env::var("ARGUS_DIAG").is_ok() {
        return bundle_diagnostic(&cfg).await;
    }

    // Stream tracer (ARGUS_STREAM=1): connect to Yellowstone and watch one slot
    // climb Processed -> Confirmed -> Finalized, logging the progression deltas.
    if std::env::var("ARGUS_STREAM").is_ok() {
        return slot_stream_probe(&cfg).await;
    }

    // Leader probe (ARGUS_LEADER=1): one NoAuth gRPC call to Jito's SearcherService
    // getNextScheduledLeader — proves the leader-window timing path live (ADR 0008).
    if std::env::var("ARGUS_LEADER").is_ok() {
        return leader_probe(&cfg).await;
    }

    // Deadline-response probe (ARGUS_PROBE=1): a feasibility sweep, NOT a graded path.
    // Send real bundles at the adaptive p75 tip while varying the blockhash AGE (the
    // deadline knob — (~150 - age) slots of validity remain), and measure landing rate +
    // slots-to-land per age. Answers: how tight must a deadline be before landing goes
    // marginal — the empirical input for whether the "race vs. abort" decision has teeth.
    if std::env::var("ARGUS_PROBE").is_ok() {
        return deadline_probe(&cfg).await;
    }

    // Lifecycle run (ARGUS_LIFECYCLE=1): submit ONE real bundle and track it through
    // both Yellowstone streams into SQLite (ADR 0004) — the Day 3-4 deliverable.
    if std::env::var("ARGUS_LIFECYCLE").is_ok() {
        return lifecycle_run(&cfg, &store).await;
    }

    // Standalone re-export (ARGUS_EXPORT=run-{ts}): regenerate a past Run's Lifecycle Log
    // from SQLite (the source of truth) WITHOUT re-submitting — free iteration on the
    // Markdown/JSONL formatting (ADR 0011). Checked before the keypair-bearing modes.
    if let Ok(run_prefix) = std::env::var("ARGUS_EXPORT") {
        let run_prefix = run_prefix.trim();
        // Guard this manual path's raw user input: an empty prefix yields the degenerate
        // LIKE `-%` (matches every Run → a mis-named `lifecycle-.md`), and `_`/`%` are SQL
        // LIKE wildcards that would silently broaden the scope across unrelated Runs. The
        // orchestrator's generated `run-{ts}` prefix is always safe; this only bites here.
        if run_prefix.is_empty() || run_prefix.contains(['%', '_']) {
            anyhow::bail!(
                "ARGUS_EXPORT must be a concrete Run prefix like 'run-1718…' (no blanks, no SQL-LIKE wildcards %/_): got {run_prefix:?}"
            );
        }
        let (jsonl, md) = export::write_lifecycle_log(&store, run_prefix, "logs")?;
        info!(run = %run_prefix, %jsonl, %md, "EXPORT: Lifecycle Log re-rendered from SQLite");
        return Ok(());
    }

    // The Run (ARGUS_RUN=1, ADR 0011): one orchestrated recording session — 3 injections +
    // N clean Payloads under one Run prefix — that produces the graded Lifecycle Log.
    if std::env::var("ARGUS_RUN").is_ok() {
        return run_orchestrator(&cfg, &store).await;
    }

    // ---- The scored path (ADR 0007): construct + land a REAL Jito bundle. ----
    // Jito-first. We detect the next Jito leader window, fan ONE bundle across all
    // eight regional Block Engines (each its own 1 req/s budget), and confirm the
    // landing on-chain. Helius Sender is only a keyless liveness backstop, below.
    let payer = wallet::load_keypair(&cfg.keypair_path)?;
    info!(payer = %payer.pubkey(), "loaded fee-payer keypair");

    // Dynamic Base Tip from the Jito Tip Floor (ADR 0005 — no hardcoded tip).
    let base_tip = tip::fetch_tip_lamports(&cfg.jito_tip_floor_url, cfg.jito_tip_percentile).await?;
    let run_id = now_millis();

    let regions = bundle::regional_endpoints();
    let auth = cfg.jito_auth_uuid.as_deref();
    let tip_accounts = bundle::published_tip_accounts();
    info!(
        jito = %cfg.jito_block_engine_url,
        regions = regions.len(),
        authed = auth.is_some(),
        base_tip,
        "submitting Jito bundles (primary scored path)"
    );

    // Cadence: one fan-out per attempt, ≥1 req/s/region honoured across retries.
    const SUBMIT_ATTEMPTS: u32 = 3;
    const RATE_LIMIT_GAP_MS: u64 = 1_200; // > 1s/region budget between retries
    const NEAR_LEADER_SLOTS: u64 = 2; // submit immediately within this window
    const SLOT_MS: u64 = 400; // ~Solana slot time
    const MAX_WINDOW_WAIT_MS: u64 = 2_500; // cap the pre-submit alignment wait

    let mut landed = false;
    for attempt in 1..=SUBMIT_ATTEMPTS {
        // Leader-window detection over gRPC (optimization signal — never blocks
        // submission). We pass the SAME regions we fan out to, so the next-leader
        // signal covers every region the bundle reaches. NoAuth, so `auth` is not
        // consumed here but stays in scope for the bundle fan-out below.
        match leader::next_scheduled_leader(&cfg.jito_searcher_grpc_url, &bundle::region_names()).await {
            Ok(nl) => {
                let gap = nl.slots_until_leader();
                info!(
                    attempt,
                    current_slot = nl.current_slot,
                    next_leader_slot = nl.next_leader_slot,
                    region = %nl.next_leader_region,
                    slots_until = gap,
                    "next Jito leader window"
                );
                if gap > NEAR_LEADER_SLOTS {
                    // saturating_mul: `gap` is network-controlled; cap can't undo an overflow.
                    let wait = gap.saturating_mul(SLOT_MS).min(MAX_WINDOW_WAIT_MS);
                    info!(attempt, wait_ms = wait, "aligning submission with leader window");
                    tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                }
            }
            Err(e) => {
                warn!(attempt, error = %e, "getNextScheduledLeader (gRPC) unavailable — submitting without leader timing")
            }
        }

        let recent_blockhash = rpc::get_latest_blockhash(&cfg.rpc_http_url).await?;
        let nonce = format!("argus-tracer-{run_id}-jito-{attempt}");
        let txs = bundle::build_bundle(&bundle::BundleParams {
            payer: &payer,
            recent_blockhash,
            nonce: &nonce,
            tip_account: tip_accounts[(attempt as usize - 1) % tip_accounts.len()],
            tip_lamports: base_tip, // dynamic (Tip Floor); floored at Jito's min in tip.rs
            self_transfer_lamports: 1,
            compute_unit_limit: None, // raw Jito bundle — no Sender mandates here
            priority_fee_microlamports: None,
        })?;
        let signature = txs[0].signatures[0].to_string();
        let explorer = format!("https://solscan.io/tx/{signature}");
        info!(attempt, %signature, %explorer, tip = base_tip, "submitting Jito bundle (all-region fan-out)");

        let results = bundle::submit_all_regions(&regions, &txs, auth).await;
        let accepted = results.iter().filter(|(_, r)| r.is_ok()).count();
        let bundle_id = results.iter().find_map(|(_, r)| r.as_ref().ok().cloned());
        info!(attempt, accepted, total = results.len(), ?bundle_id, "fan-out complete — confirming on-chain");

        // On-chain truth: the signature is identical across regions. Stream-based
        // confirmation (ADR 0004) lands in the Day 3-4 milestone; RPC is the interim.
        if let Some(slot) = rpc::await_signature(&cfg.rpc_http_url, &signature, 12, 2_500).await? {
            info!(attempt, %signature, slot, %explorer, "✅ LANDED (Jito bundle)");
            landed = true;
            break;
        }
        warn!(attempt, "bundle not landed this window — retrying with fresh blockhash");
        tokio::time::sleep(std::time::Duration::from_millis(RATE_LIMIT_GAP_MS)).await;
    }

    // ---- Backstop: Helius Sender (keyless liveness; NOT the scored path, ADR 0007). ----
    if !landed {
        warn!("Jito bundle did not land — falling back to the Helius Sender backstop");
        let recent_blockhash = rpc::get_latest_blockhash(&cfg.rpc_http_url).await?;
        let nonce = format!("argus-tracer-{run_id}-sender");
        // Sender mandates a CU limit + priority fee, and a tip ≥ its route minimum.
        let sender_tip = base_tip.max(sender::min_tip_lamports(
            cfg.helius_swqos_only,
            cfg.sender_dual_min_tip_lamports,
            cfg.sender_swqos_min_tip_lamports,
        ));
        let txs = bundle::build_bundle(&bundle::BundleParams {
            payer: &payer,
            recent_blockhash,
            nonce: &nonce,
            tip_account: sender::tip_account(0),
            tip_lamports: sender_tip,
            self_transfer_lamports: 1,
            compute_unit_limit: Some(cfg.sender_compute_unit_limit),
            priority_fee_microlamports: Some(cfg.sender_priority_fee_microlamports),
        })?;
        let signature = txs[0].signatures[0].to_string();
        let explorer = format!("https://solscan.io/tx/{signature}");
        info!(%signature, %explorer, sender_tip, swqos_only = cfg.helius_swqos_only, "submitting via Helius Sender backstop");
        match sender::submit(&cfg.helius_sender_url, &txs[0]).await {
            Ok(returned) => info!(returned_sig = %returned, "Sender accepted — confirming via RPC"),
            Err(e) => warn!(error = %e, "Sender backstop rejected"),
        }
        if let Some(slot) = rpc::await_signature(&cfg.rpc_http_url, &signature, 15, 2_000).await? {
            info!(%signature, slot, %explorer, "✅ LANDED (Helius Sender backstop)");
            landed = true;
        }
    }

    if !landed {
        warn!("did not land via Jito or the Sender backstop (no fee charged on non-landing) — re-run to try again");
    }
    Ok(())
}

/// One-shot probe (ARGUS_DIAG=1): submit a single HIGH-tip bundle and watch
/// `getInflightBundleStatuses` live across all regions to learn WHY unauthed
/// bundles aren't landing. No Sender backstop, so it costs $0 unless it lands.
///
/// Reading the result:
/// - stays `Invalid` throughout  -> never forwarded; the unauthed tier won't land
///   it regardless of tip -> we need an authed relay (SolInfra / Jito UUID).
/// - `Pending` then `Landed`     -> keyless path works; the low tip was the issue.
/// - `Pending` then `Failed`     -> forwarded but lost / leader skipped -> we need
///   real (gRPC) leader-window targeting.
async fn bundle_diagnostic(cfg: &config::Config) -> Result<()> {
    let payer = wallet::load_keypair(&cfg.keypair_path)?;
    info!(payer = %payer.pubkey(), "DIAG: loaded fee-payer keypair");

    // Tip floor for context, but submit with a deliberately HIGH tip to rule the
    // tip out as the variable (override via ARGUS_DIAG_TIP lamports; default 0.001 SOL).
    let base_tip = tip::fetch_tip_lamports(&cfg.jito_tip_floor_url, cfg.jito_tip_percentile).await?;
    let diag_floor: u64 = std::env::var("ARGUS_DIAG_TIP")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(1_000_000);
    let tip = base_tip.max(diag_floor);

    let regions = bundle::regional_endpoints();
    let auth = cfg.jito_auth_uuid.as_deref();
    let tip_accounts = bundle::published_tip_accounts();

    let recent_blockhash = rpc::get_latest_blockhash(&cfg.rpc_http_url).await?;
    let run_id = now_millis();
    let nonce = format!("argus-diag-{run_id}");
    let txs = bundle::build_bundle(&bundle::BundleParams {
        payer: &payer,
        recent_blockhash,
        nonce: &nonce,
        tip_account: tip_accounts[0],
        tip_lamports: tip,
        self_transfer_lamports: 1,
        compute_unit_limit: None,
        priority_fee_microlamports: None,
    })?;
    let signature = txs[0].signatures[0].to_string();
    let explorer = format!("https://solscan.io/tx/{signature}");
    info!(%signature, %explorer, base_tip, tip, authed = auth.is_some(), "DIAG: submitting ONE high-tip bundle (fan-out, no backstop)");

    let results = bundle::submit_all_regions(&regions, &txs, auth).await;
    for (region, r) in &results {
        match r {
            Ok(id) => info!(region = %region, bundle_id = %id, "DIAG: accepted"),
            Err(e) => warn!(region = %region, error = %e, "DIAG: rejected"),
        }
    }
    let bundle_id = match results.iter().find_map(|(_, r)| r.as_ref().ok().cloned()) {
        Some(id) => id,
        None => {
            warn!("DIAG: no region accepted the bundle — nothing to poll");
            return Ok(());
        }
    };
    info!(%bundle_id, "DIAG: polling getInflightBundleStatuses live (~40s)");

    // The bundle id is content-addressed (identical across regions), so we poll
    // every region for it and tally the statuses each tick. Stop early on a landing.
    let mut landed = false;
    for tick in 1..=16u32 {
        let mut tally: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for region in &regions {
            let status = bundle::inflight_status(region, &bundle_id)
                .await
                .unwrap_or_else(|_| "Err".to_string());
            *tally.entry(status).or_default() += 1;
        }
        info!(tick, ?tally, "DIAG: inflight status across regions");

        if let Some(slot) = rpc::await_signature(&cfg.rpc_http_url, &signature, 1, 0).await? {
            info!(tick, slot, %explorer, "DIAG: ✅ LANDED on-chain");
            landed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2_500)).await;
    }
    if !landed {
        warn!(%explorer, "DIAG: bundle did not land — Invalid throughout = not forwarded (need authed relay); Pending = forwarded but lost");
    }
    Ok(())
}

/// Live leader-window probe (ARGUS_LEADER=1): one NoAuth gRPC call to Jito's
/// `searcher.SearcherService/GetNextScheduledLeader`, logging the next Jito leader
/// slot/region and the slot gap. Proves the Day 5-6 timing path end-to-end (TLS +
/// gRPC + the regional host) the same way ARGUS_STREAM proves the Yellowstone path.
async fn leader_probe(cfg: &config::Config) -> Result<()> {
    info!(grpc = %cfg.jito_searcher_grpc_url, "LEADER: querying getNextScheduledLeader over gRPC (NoAuth)");
    match leader::next_scheduled_leader(&cfg.jito_searcher_grpc_url, &bundle::region_names()).await {
        Ok(nl) => info!(
            current_slot = nl.current_slot,
            next_leader_slot = nl.next_leader_slot,
            region = %nl.next_leader_region,
            identity = %nl.next_leader_identity,
            slots_until = nl.slots_until_leader(),
            "LEADER: ✅ next Jito leader"
        ),
        Err(e) => warn!(error = format!("{e:#}"), "LEADER: query failed (endpoint is best-effort)"),
    }
    Ok(())
}

/// Live slot-stream tracer (ARGUS_STREAM=1): connect to Yellowstone, latch the
/// first slot we see reach Processed, then follow THAT slot through Confirmed and
/// Finalized — logging the commitment-progression deltas (the README Q1 data).
/// Stops when the tracked slot finalizes, or after a safety cap of updates.
async fn slot_stream_probe(cfg: &config::Config) -> Result<()> {
    use streaming::Commitment::{Confirmed, Finalized, Processed};

    info!(grpc = %cfg.yellowstone_grpc_url, "STREAM: connecting to Yellowstone slot subscription");

    let mut tracked: Option<u64> = None;
    let mut t_processed: u128 = 0;
    let mut t_confirmed: u128 = 0;
    let mut seen = 0u32;
    const MAX_UPDATES: u32 = 400; // safety cap — the tracked slot finalizes well within this

    let outcome = streaming::subscribe_slots(
        &cfg.yellowstone_grpc_url,
        cfg.yellowstone_x_token.as_deref(),
        cfg.stream_channel_cap,
        cfg.stream_max_reconnects,
        |su| {
            seen += 1;

            // Latch the first Processed slot; its Confirmed/Finalized are still ahead.
            if tracked.is_none() && su.commitment == Processed {
                tracked = Some(su.slot);
                t_processed = now_millis();
                info!(slot = su.slot, "STREAM: tracking this slot's commitment progression");
                return true;
            }

            if Some(su.slot) == tracked {
                match su.commitment {
                    Processed => {}
                    Confirmed => {
                        t_confirmed = now_millis();
                        info!(
                            slot = su.slot,
                            processed_to_confirmed_ms = t_confirmed.saturating_sub(t_processed),
                            "STREAM: → Confirmed"
                        );
                    }
                    Finalized => {
                        let now = now_millis();
                        info!(
                            slot = su.slot,
                            confirmed_to_finalized_ms = now.saturating_sub(t_confirmed.max(t_processed)),
                            processed_to_finalized_ms = now.saturating_sub(t_processed),
                            saw_confirmed = (t_confirmed > 0),
                            "STREAM: → Finalized ✅ (commitment progression complete)"
                        );
                        return false; // tracked slot fully progressed — done
                    }
                }
            }

            seen < MAX_UPDATES
        },
    )
    .await?;

    info!(
        updates = seen,
        reconnects = outcome.metrics.reconnects,
        received = outcome.metrics.received,
        dropped = outcome.metrics.dropped,
        high_water = outcome.metrics.high_water,
        gave_up = outcome.gave_up,
        "STREAM: slot subscription closed"
    );
    if outcome.gave_up {
        warn!("STREAM: gave up after exhausting reconnects — endpoint unreachable?");
    }
    Ok(())
}

/// Delta in ms between two optional stage timestamps; -1 if either is missing.
fn stage_delta_ms(a: Option<u128>, b: Option<u128>) -> i64 {
    match (a, b) {
        (Some(a), Some(b)) => (b as i64) - (a as i64),
        _ => -1,
    }
}

/// Deadline-response probe (ARGUS_PROBE=1): a feasibility sweep, NOT a graded artifact.
/// For each blockhash AGE in a marginal-weighted sweep, send a real bundle at the adaptive
/// p75 tip and poll Jito for landing — measuring whether a bundle with only ~(150 - age)
/// slots of validity left can land in time. The land-rate curve across ages is the empirical
/// answer to "how tight a deadline before 'race vs. abort' actually bites." Cheap: a
/// non-landing bundle pays no tip (tips bill only on inclusion).
async fn deadline_probe(cfg: &config::Config) -> Result<()> {
    use solana_sdk::signer::Signer;

    let payer = wallet::load_keypair(&cfg.keypair_path)?;
    let tip = tip::fetch_tip_lamports(&cfg.jito_tip_floor_url, 75).await.unwrap_or(5_000);
    let regions = bundle::regional_endpoints();
    let auth = cfg.jito_auth_uuid.as_deref();
    let tip_accounts = bundle::published_tip_accounts();
    if tip_accounts.is_empty() {
        anyhow::bail!("PROBE: no published Jito tip accounts — cannot build a tippable bundle");
    }
    info!(payer = %payer.pubkey(), tip, regions = regions.len(),
        "PROBE: deadline-response sweep — adaptive p75 tip, varying blockhash age");

    // Blockhash age in slots (the deadline knob). Validity ~150 slots; landing was observed
    // at ~5 slots in the live Run, so the bite is near age ~143+ — sample densely there. The
    // effective age runs a few slots over the requested one (fetch+build+submit slippage),
    // which only sharpens the tail. The two cheap anchors (0, 110) confirm the un-pressured
    // baseline lands at all.
    let ages: [u64; 12] = [0, 110, 130, 130, 138, 138, 142, 142, 142, 145, 145, 145];

    // (age, budget_slots, landed, slots_to_land). `landed` is tracked SEPARATELY from the
    // slots-to-land delta: a bundle can land while the submit-slot read failed, leaving the delta
    // unknown — folding "unknown delta" into "didn't land" would undercount the land rate.
    let mut rows: Vec<(u64, i64, bool, Option<i64>)> = Vec::new();
    for (i, age) in ages.into_iter().enumerate() {
        // `None` if the slot read failed — then a landing's slots-to-land delta is left unknown
        // rather than computed against a fabricated 0 (which printed an absolute-slot garbage delta).
        let submit_slot = rpc::get_slot(&cfg.rpc_http_url).await.ok();
        let blockhash = if age == 0 {
            rpc::get_latest_blockhash(&cfg.rpc_http_url).await?
        } else {
            rpc::get_aged_blockhash(&cfg.rpc_http_url, age).await?
        };
        let budget = (150i64 - age as i64).max(0); // slots of validity left at submit (never negative)
        let nonce = format!("probe-{}-a{age}-{i}", now_millis());
        let params = bundle::BundleParams {
            payer: &payer,
            recent_blockhash: blockhash,
            nonce: &nonce,
            tip_account: tip_accounts[i % tip_accounts.len()],
            tip_lamports: tip,
            self_transfer_lamports: 1,
            compute_unit_limit: None,
            priority_fee_microlamports: None,
        };
        let txs = bundle::build_bundle(&params)?;
        let signature = txs[0].signatures[0].to_string();
        let send = bundle::submit_all_regions(&regions, &txs, auth).await;
        let landed_via = send.iter().find_map(|(reg, r)| r.as_ref().ok().map(|b| (reg.clone(), b.clone())));

        match landed_via {
            None => {
                warn!(age, %signature, "PROBE: no region accepted — counting as non-landing");
                rows.push((age, budget, false, None));
            }
            Some((region, bundle_id)) => {
                // Poll up to ~16s (40 × 400ms) — generous vs the observed ~2s landing.
                let landed = bundle::await_landed(&region, &bundle_id, 40, 400).await.unwrap_or(None);
                match landed {
                    Some(slot) => {
                        // Delta only when the submit-slot was actually read; else left unknown.
                        let stl = submit_slot.map(|ss| slot as i64 - ss as i64);
                        info!(age, budget, slots_to_land = ?stl, landed_slot = slot, %signature, "PROBE: ✅ landed in time");
                        rows.push((age, budget, true, stl));
                    }
                    None => {
                        info!(age, budget, %signature, "PROBE: ✗ did not land within the window (deadline missed)");
                        rows.push((age, budget, false, None));
                    }
                }
            }
        }
    }

    // Collapse to a land-rate curve by age — the decision input.
    info!("PROBE: ===== deadline-response curve =====");
    let mut ages_seen: Vec<u64> = rows.iter().map(|r| r.0).collect();
    ages_seen.sort_unstable();
    ages_seen.dedup();
    for age in ages_seen {
        let bucket: Vec<&(u64, i64, bool, Option<i64>)> = rows.iter().filter(|r| r.0 == age).collect();
        let n = bucket.len();
        let landed = bucket.iter().filter(|r| r.2).count();
        let budget = bucket.first().map(|r| r.1).unwrap_or(0);
        let stls: Vec<i64> = bucket.iter().filter_map(|r| r.3).collect();
        let avg = if stls.is_empty() { "—".to_string() } else { format!("{:.1}", stls.iter().sum::<i64>() as f64 / stls.len() as f64) };
        info!(age, budget_slots = budget, landed, of = n, avg_slots_to_land = %avg, "PROBE: age {age} → {landed}/{n} landed");
    }
    info!("PROBE: done — the marginal age is where land-rate falls from ~all to ~none");
    Ok(())
}

/// Live lifecycle run (ARGUS_LIFECYCLE=1): submit ONE real Jito bundle and track it
/// through the two-stream model (ADR 0004) — Inclusion (transaction stream) +
/// Commitment Progression (slot stream) — persisting submitted/landed/processed/
/// confirmed/finalized to SQLite, then logging the deltas (the Lifecycle Log data).
/// With ARGUS_INJECT set, the run becomes inject → classify → remedy → resubmit
/// (ADR 0010); see `injection_run`.
async fn lifecycle_run(cfg: &config::Config, store: &storage::Store) -> Result<()> {
    let payer = wallet::load_keypair(&cfg.keypair_path)?;
    info!(payer = %payer.pubkey(), "LIFECYCLE: loaded fee-payer keypair");

    let base_tip = tip::fetch_tip_lamports(&cfg.jito_tip_floor_url, cfg.jito_tip_percentile).await?;
    let run_id = format!("run-{}", now_millis());
    let regions = bundle::regional_endpoints();
    let auth = cfg.jito_auth_uuid.as_deref();
    let tip_accounts = bundle::published_tip_accounts();

    if let Some(injection) = cfg.injection {
        // Construct the decision Policy ONCE (only on the injection path — the clean
        // path never decides). ARGUS_POLICY=agent swaps in the HTTP Agent with no other
        // call-site change (the seam from ADR 0010); default stays the local stand-in.
        let policy = if cfg.use_agent {
            info!(agent_url = %cfg.agent_url, timeout_secs = cfg.agent_timeout_secs,
                "POLICY: AI Agent over HTTP (ARGUS_POLICY=agent)");
            failure::Policy::Agent(agent_client::AgentClient::new(
                &cfg.agent_url,
                cfg.agent_timeout_secs,
            )?)
        } else {
            info!("POLICY: local default policy (Agent stand-in) — set ARGUS_POLICY=agent for the AI Agent");
            failure::Policy::Local
        };
        return injection_run(
            cfg, store, &payer, &run_id, base_tip, &regions, auth, &tip_accounts, injection, &policy,
            false, // standalone ARGUS_INJECT stays sim-only (ADR 0010); only the Run sends the fault
        )
        .await;
    }

    // Clean run: one submission tracked to finalize.
    submit_and_track_one(
        &SubmitCtx { cfg, store, payer: &payer, run_id: &run_id, regions: &regions, auth },
        1,
        tip_accounts[0],
        &failure::RetryState { tip_lamports: base_tip, cu_limit: None, priority_fee_microlamports: None },
    )
    .await?;
    Ok(())
}

/// Warn below ~0.01 SOL before a Run — a full Run puts ~12 bundles on the wire (base fee
/// 5000 lamports per accepted tx; tips paid only on the ~9 that land), well under this, so
/// a payer below it risks running short.
const RUN_MIN_BALANCE_LAMPORTS: u64 = 10_000_000;

/// The Run (ARGUS_RUN=1, ADR 0011): a single-session orchestrator that drives the whole
/// graded recording — the 3 deterministic injections, then `ARGUS_RUN_CLEAN_COUNT` clean
/// Payloads — under ONE Run prefix (`run-{ts}`). Each Payload runs under a child run_id
/// `run-{ts}-p{k}`, so the proven submit/track/persist path is reused verbatim (only the
/// run_id varies — Run-ID-prefix keying). Serial, each Submission tracked to finalized;
/// best-effort (one Payload's organic non-landing is recorded and the Run continues, no
/// per-payload retry). Preflight hard-fails if the Agent `/health` is down (a scored Run
/// must not degrade to local-fallback, ADR 0006); at the end it asserts ≥10 sent / ≥2
/// failures and auto-exports the Lifecycle Log.
async fn run_orchestrator(cfg: &config::Config, store: &storage::Store) -> Result<()> {
    // A Run is a scored artifact — it MUST reason via the Agent, never the local stand-in.
    if !cfg.use_agent {
        anyhow::bail!(
            "ARGUS_RUN requires ARGUS_POLICY=agent — a scored Run must not run on the local stand-in (ADR 0006)"
        );
    }

    // Preflight: keypair is fatal; Agent /health is a hard gate; balance only warns.
    let payer = wallet::load_keypair(&cfg.keypair_path)?;
    info!(payer = %payer.pubkey(), "RUN: loaded fee-payer keypair");
    let agent = agent_client::AgentClient::new(&cfg.agent_url, cfg.agent_timeout_secs)?;
    agent.health().await.map_err(|e| {
        anyhow::anyhow!(
            "RUN preflight: Agent /health unreachable at {} ({e}) — refusing to start a scored Run (ADR 0011)",
            cfg.agent_url
        )
    })?;
    info!(agent_url = %cfg.agent_url, "RUN: Agent /health OK");
    let policy = failure::Policy::Agent(agent);

    match rpc::get_balance(&cfg.rpc_http_url, &payer.pubkey().to_string()).await {
        Ok(lamports) => {
            let balance_sol = lamports as f64 / 1_000_000_000.0;
            if lamports < RUN_MIN_BALANCE_LAMPORTS {
                warn!(payer = %payer.pubkey(), balance_sol, "RUN: thin fee-payer balance — a full Run may run short");
            } else {
                info!(payer = %payer.pubkey(), balance_sol, "RUN: fee-payer balance OK");
            }
        }
        Err(e) => warn!(error = %e, "RUN: balance check failed (continuing best-effort)"),
    }

    let base_tip = tip::fetch_tip_lamports(&cfg.jito_tip_floor_url, cfg.jito_tip_percentile).await?;
    let regions = bundle::regional_endpoints();
    let auth = cfg.jito_auth_uuid.as_deref();
    let tip_accounts = bundle::published_tip_accounts();

    // The Run prefix; each Payload runs under a child run_id `run-{ts}-p{k}` (ADR 0011).
    let run_prefix = format!("run-{}", now_millis());
    // The deterministic injections: the three bounded faults the four-class baseline handles,
    // then the foreign-program spread (ADR 0012) — three real programs that reject an identical
    // garbage instruction with DISTINCT errors the baseline collapses to one blind abort, but the
    // Agent diagnoses individually. All three abort (permanent/funding) → non-landing → zero SOL.
    let injections = [
        failure::Injection::ExpiredBlockhash,
        failure::Injection::ComputeExceeded,
        failure::Injection::BundleFailure,
        failure::Injection::ForeignProgram(failure::ForeignFault::Memo),
        failure::Injection::ForeignProgram(failure::ForeignFault::Token),
        failure::Injection::ForeignProgram(failure::ForeignFault::Whirlpool),
    ];
    let total = injections.len() + cfg.run_clean_count;
    info!(%run_prefix, injections = injections.len(), clean = cfg.run_clean_count, total,
        "RUN: starting (serial, each Submission tracked to finalized)");

    let mut k = 0usize;

    // The 3 injections first: each sends a faulted attempt-1, classifies, Agent-decides,
    // then applies the Remedy (attempt-2 for the recoverables; Abort for bundle_failure).
    for injection in injections {
        let run_id = format!("{run_prefix}-p{k}");
        info!(%run_id, ?injection, "RUN: injection Payload {}/{}", k + 1, total);
        if let Err(e) = injection_run(
            cfg, store, &payer, &run_id, base_tip, &regions, auth, &tip_accounts, injection, &policy,
            true, // the Run sends the faulted attempt-1 on the wire (ADR 0011)
        )
        .await
        {
            warn!(%run_id, ?injection, error = %format!("{e:#}"), "RUN: injection Payload errored — continuing best-effort");
        }
        k += 1;
    }

    // Then the clean Payloads — one Submission each, tracked to finalized.
    for _ in 0..cfg.run_clean_count {
        let run_id = format!("{run_prefix}-p{k}");
        info!(%run_id, "RUN: clean Payload {}/{}", k + 1, total);
        let seed = failure::RetryState { tip_lamports: base_tip, cu_limit: None, priority_fee_microlamports: None };
        let ctx = SubmitCtx { cfg, store, payer: &payer, run_id: &run_id, regions: &regions, auth };
        if let Err(e) = submit_and_track_one(&ctx, 1, tip_accounts[k % tip_accounts.len()], &seed).await {
            warn!(%run_id, error = %format!("{e:#}"), "RUN: clean Payload errored — continuing best-effort");
        }
        k += 1;
    }

    // End-of-Run: assert the graded bar BEFORE assembling the Lifecycle Log (ADR 0011), so
    // a deficient Run is caught loudly here, not at submission time.
    let subs = store.fetch_run_submissions(&run_prefix)?;
    let (sent, failures, landed) = export::run_counts(&subs);
    info!(%run_prefix, sent, failures, landed, "RUN: complete");
    if sent < 10 {
        warn!(sent, "RUN: ⚠ fewer than 10 real bundle Submissions — Lifecycle Log is short of the bar");
    }
    if failures < 2 {
        warn!(failures, "RUN: ⚠ fewer than 2 classified Failures — Lifecycle Log lacks the required failure evidence");
    }
    if sent >= 10 && failures >= 2 {
        info!(sent, failures, "RUN: ✅ meets the graded bar (≥10 Submissions, ≥2 Failures)");
    }

    // Auto-export the Lifecycle Log (JSONL + Markdown) for this Run.
    let (jsonl, md) = export::write_lifecycle_log(store, &run_prefix, "logs")?;
    info!(%run_prefix, %jsonl, %md, "RUN: Lifecycle Log exported");
    Ok(())
}

/// Shared context for one submission+track attempt (keeps the arg list sane).
#[derive(Clone, Copy)]
struct SubmitCtx<'a> {
    cfg: &'a config::Config,
    store: &'a storage::Store,
    payer: &'a solana_sdk::signature::Keypair,
    run_id: &'a str,
    regions: &'a [String],
    auth: Option<&'a str>,
}

/// Build a clean bundle for one attempt, open the Yellowstone lifecycle streams,
/// submit after the subscription is live, and persist Inclusion + Commitment
/// Progression to SQLite. Returns whether it landed. Shared by the clean run and the
/// post-remedy retry (ADR 0010), so the proven stream-tracking path lives in one place.
async fn submit_and_track_one(
    ctx: &SubmitCtx<'_>,
    attempt: i64,
    tip_account: solana_sdk::pubkey::Pubkey,
    retry: &failure::RetryState,
) -> Result<bool> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use storage::Stage;
    use streaming::{Commitment, LifecycleEvent};

    let SubmitCtx { cfg, store, payer, run_id, regions, auth } = *ctx;
    let nonce = format!("argus-{run_id}-jito-{attempt}");

    let recent_blockhash = rpc::get_latest_blockhash(&cfg.rpc_http_url).await?;
    let txs = bundle::build_bundle(&bundle::BundleParams {
        payer,
        recent_blockhash,
        nonce: &nonce,
        tip_account,
        tip_lamports: retry.tip_lamports,
        self_transfer_lamports: 1,
        compute_unit_limit: retry.cu_limit,
        priority_fee_microlamports: retry.priority_fee_microlamports,
    })?;
    let signature = txs[0].signatures[0].to_string();
    let explorer = format!("https://solscan.io/tx/{signature}");

    let submitted_at = now_millis() as i64;
    store.record_submission(&storage::NewSubmission {
        run_id,
        attempt,
        nonce: &nonce,
        bundle_id: None,
        signature: &signature,
        tip_lamports: retry.tip_lamports,
        submitted_at,
    })?;
    info!(%run_id, attempt, %signature, %explorer, tip = retry.tip_lamports, authed = auth.is_some(), "LIFECYCLE: Submission recorded; opening Yellowstone streams");

    // Submit AFTER the subscription is live (on_subscribed) so Inclusion on the tx
    // stream is never missed. Stash the handle to read the bundle id afterward.
    let submit_handle: Arc<Mutex<Option<tokio::task::JoinHandle<Vec<(String, Result<String>)>>>>> =
        Arc::new(Mutex::new(None));
    let on_subscribed = {
        let submit_handle = Arc::clone(&submit_handle);
        let submit_txs = txs.clone();
        let submit_regions = regions.to_vec();
        let submit_auth = auth.map(String::from);
        move || {
            let h = tokio::spawn(async move {
                bundle::submit_all_regions(&submit_regions, &submit_txs, submit_auth.as_deref()).await
            });
            *submit_handle.lock().unwrap() = Some(h);
        }
    };

    // Per-slot observe-times [processed, confirmed, finalized], the landed slot
    // (from Inclusion), and which stages we've already persisted.
    let mut slot_times: HashMap<u64, [Option<u128>; 3]> = HashMap::new();
    let mut landed: Option<u64> = None;
    let mut written = [false; 3];
    let stages = [Stage::Processed, Stage::Confirmed, Stage::Finalized];

    let on_event = |ev: LifecycleEvent| -> bool {
        match ev {
            LifecycleEvent::Landed { signature: sig, slot } => {
                if landed.is_none() {
                    landed = Some(slot);
                    if let Err(e) = store.set_landed_slot(run_id, attempt, &nonce, slot) {
                        warn!(error = %e, "LIFECYCLE: set_landed_slot failed");
                    }
                    info!(slot, signature = %sig, "LIFECYCLE: ✅ Inclusion (landed)");
                    // Persist any stages already observed before we knew the slot was ours.
                    if let Some(times) = slot_times.get(&slot).copied() {
                        for (i, t) in times.iter().enumerate() {
                            if let (Some(t), false) = (t, written[i]) {
                                written[i] = true;
                                let _ = store.mark_stage(run_id, attempt, &nonce, stages[i], *t as i64);
                            }
                        }
                    }
                }
                true
            }
            LifecycleEvent::Commitment { slot, level } => {
                let idx = match level {
                    Commitment::Processed => 0,
                    Commitment::Confirmed => 1,
                    Commitment::Finalized => 2,
                };
                let now = now_millis();
                let entry = slot_times.entry(slot).or_insert([None; 3]);
                if entry[idx].is_none() {
                    entry[idx] = Some(now);
                }
                if landed == Some(slot) {
                    let t = slot_times[&slot][idx].unwrap_or(now);
                    if !written[idx] {
                        written[idx] = true;
                        if let Err(e) = store.mark_stage(run_id, attempt, &nonce, stages[idx], t as i64) {
                            warn!(error = %e, "LIFECYCLE: mark_stage failed");
                        }
                    }
                    if idx == 2 {
                        let ts = slot_times[&slot];
                        info!(
                            slot,
                            processed_to_confirmed_ms = stage_delta_ms(ts[0], ts[1]),
                            confirmed_to_finalized_ms = stage_delta_ms(ts[1], ts[2]),
                            processed_to_finalized_ms = stage_delta_ms(ts[0], ts[2]),
                            "LIFECYCLE: → Finalized ✅ (commitment progression complete)"
                        );
                        return false; // tracked slot fully progressed — done
                    }
                }
                true
            }
        }
    };

    let track = streaming::track_lifecycle(
        &cfg.yellowstone_grpc_url,
        cfg.yellowstone_x_token.as_deref(),
        &signature,
        cfg.stream_channel_cap,
        cfg.stream_max_reconnects,
        on_subscribed,
        on_event,
    );
    let mut gave_up = false;
    let mut timed_out = false;
    match tokio::time::timeout(std::time::Duration::from_secs(60), track).await {
        Ok(Ok(outcome)) => {
            gave_up = outcome.gave_up;
            info!(
                reconnects = outcome.metrics.reconnects,
                received = outcome.metrics.received,
                dropped = outcome.metrics.dropped,
                high_water = outcome.metrics.high_water,
                gave_up = outcome.gave_up,
                "LIFECYCLE: stream closed"
            );
        }
        Ok(Err(e)) => warn!(error = %e, "LIFECYCLE: stream error"),
        Err(_) => timed_out = true,
    }

    // If the producer gave up before the bundle was ever submitted (on_subscribed
    // never fired), nothing was broadcast — surface that, don't report a benign
    // non-landing against a recorded-but-never-sent Submission.
    let submit_fired = submit_handle.lock().unwrap().is_some();
    if gave_up && !submit_fired {
        anyhow::bail!(
            "LIFECYCLE: streaming never connected (producer gave up after exhausting reconnects) — bundle was NOT submitted"
        );
    }
    if gave_up {
        warn!("LIFECYCLE: stream gave up after exhausting reconnects — landing status uncertain, reconciling via RPC");
    }

    // Post-stream reconciliation (ADR 0009): a reconnect gap, a shed terminal frame, a
    // give-up, or the timeout can lose the Landed event even though the bundle landed
    // (Yellowstone replays no history). Cross-check via RPC before reporting non-landing.
    if landed.is_none() {
        match rpc::await_signature(&cfg.rpc_http_url, &signature, 3, 1000).await {
            Ok(Some(slot)) => {
                landed = Some(slot);
                if let Err(e) = store.set_landed_slot(run_id, attempt, &nonce, slot) {
                    warn!(error = %e, "LIFECYCLE: reconciliation set_landed_slot failed");
                }
                info!(slot, %signature, "LIFECYCLE: ✅ recovered landing via RPC reconciliation");
            }
            Ok(None) => {}
            Err(e) => warn!(error = %e, "LIFECYCLE: reconciliation RPC check failed"),
        }
    }
    if timed_out && landed.is_none() {
        warn!("LIFECYCLE: timed out before finalize and RPC reconciliation found no landing — bundle likely did not land");
    }

    // The submit finished early; record the bundle id for the Lifecycle Log.
    if let Some(h) = submit_handle.lock().unwrap().take() {
        match h.await {
            Ok(results) => {
                let accepted = results.iter().filter(|(_, r)| r.is_ok()).count();
                if let Some(bid) = results.iter().find_map(|(_, r)| r.as_ref().ok().cloned()) {
                    let _ = store.set_bundle_id(run_id, attempt, &nonce, &bid);
                    info!(accepted, total = results.len(), bundle_id = %bid, "LIFECYCLE: fan-out complete");
                } else {
                    warn!(accepted, "LIFECYCLE: no region returned a bundle id");
                }
            }
            Err(e) => warn!(error = %e, "LIFECYCLE: submit join error"),
        }
    }

    // The persisted row — SQLite source of truth for this Submission.
    if let Some(row) = store.fetch_submission(run_id, attempt, &nonce)? {
        info!(?row, "LIFECYCLE: persisted row");
    }
    Ok(landed.is_some())
}

/// Observe the TRUE compute-unit need of the clean attempt-2 transaction by simulating
/// it at the per-tx MAX CU ceiling — so the sim SUCCEEDS and `unitsConsumed` is the real
/// requirement, not a capped-out figure. Feeds the RaiseCuLimit remedy so the raised
/// limit is derived from observation, not a payload-tuned constant (ADR 0010 hardening).
async fn observe_cu_need(
    cfg: &config::Config,
    payer: &solana_sdk::signature::Keypair,
    run_id: &str,
    tip_lamports: u64,
    tip_account: solana_sdk::pubkey::Pubkey,
) -> Result<u32> {
    let recent_blockhash = rpc::get_latest_blockhash(&cfg.rpc_http_url).await?;
    let nonce = format!("argus-{run_id}-jito-2");
    let txs = bundle::build_bundle(&bundle::BundleParams {
        payer,
        recent_blockhash,
        nonce: &nonce,
        tip_account,
        tip_lamports,
        self_transfer_lamports: 1,
        compute_unit_limit: Some(failure::MAX_CU_LIMIT),
        priority_fee_microlamports: None,
    })?;
    let sim = rpc::simulate_transaction(&cfg.rpc_http_url, &txs[0]).await?;
    sim.units_consumed
        .ok_or_else(|| anyhow::anyhow!("max-CU re-sim returned no unitsConsumed: err={:?}", sim.err))
}

/// Faulted lifecycle run (ARGUS_INJECT, ADR 0010 + 0011): build an attempt-1 bundle
/// carrying one deterministic fault; for the Run (`send_faulted`) SEND it on the wire (a
/// real, non-landing, free Submission — ADR 0011), the standalone `ARGUS_INJECT` path
/// passes `false` and stays sim-only (its ADR 0010 behavior). Classify it via preflight
/// simulation (the only reason source for an all-or-nothing bundle), let the Policy (Agent
/// over HTTP, or the local stand-in) choose a Remedy, persist the Failure Class + decision,
/// then execute the Remedy — Abort stops; otherwise attempt 2 resubmits clean and is
/// tracked to finalize.
#[allow(clippy::too_many_arguments)]
async fn injection_run(
    cfg: &config::Config,
    store: &storage::Store,
    payer: &solana_sdk::signature::Keypair,
    run_id: &str,
    base_tip: u64,
    regions: &[String],
    auth: Option<&str>,
    tip_accounts: &[solana_sdk::pubkey::Pubkey],
    injection: failure::Injection,
    policy: &failure::Policy,
    send_faulted: bool,
) -> Result<()> {
    use failure::{Injection, RetryState};
    use solana_sdk::signer::Signer;

    let attempt: i64 = 1;
    let nonce = format!("argus-{run_id}-jito-{attempt}");

    // 1) Build the faulted attempt-1 bundle (the injection).
    let (recent_blockhash, cu_limit, payload_override) = match injection {
        Injection::ExpiredBlockhash => (
            rpc::get_aged_blockhash(&cfg.rpc_http_url, failure::AGED_BLOCKHASH_SLOTS).await?,
            None,
            None,
        ),
        Injection::ComputeExceeded => (
            rpc::get_latest_blockhash(&cfg.rpc_http_url).await?,
            Some(failure::INJECT_CU_LIMIT),
            None,
        ),
        Injection::BundleFailure => (
            rpc::get_latest_blockhash(&cfg.rpc_http_url).await?,
            None,
            Some(failure::failing_payload(&payer.pubkey(), &nonce, 1)),
        ),
        Injection::ForeignProgram(fault) => (
            rpc::get_latest_blockhash(&cfg.rpc_http_url).await?,
            None,
            Some(failure::foreign_fault_payload(fault, &payer.pubkey(), &nonce)),
        ),
    };
    let params = bundle::BundleParams {
        payer,
        recent_blockhash,
        nonce: &nonce,
        tip_account: tip_accounts[0],
        tip_lamports: base_tip,
        self_transfer_lamports: 1,
        compute_unit_limit: cu_limit,
        priority_fee_microlamports: None,
    };
    let txs = match payload_override {
        Some(payload) => bundle::build_bundle_with_payload(&params, payload)?,
        None => bundle::build_bundle(&params)?,
    };
    let signature = txs[0].signatures[0].to_string();
    let explorer = format!("https://solscan.io/tx/{signature}");

    store.record_submission(&storage::NewSubmission {
        run_id,
        attempt,
        nonce: &nonce,
        bundle_id: None,
        signature: &signature,
        tip_lamports: base_tip,
        submitted_at: now_millis() as i64,
    })?;
    info!(?injection, %signature, %explorer, "INJECT: built faulted bundle");

    // 1b) For the Run (`send_faulted`), SEND the faulted attempt-1 on the wire (ADR 0011,
    // amending ADR 0010's sim-only stance). A doomed bundle never lands, so it pays no tip —
    // it is a REAL, non-landing Submission that counts toward the Run's "≥10 real bundle
    // submissions" and is genuine ≥2-failure evidence. Send-and-record, NOT track-to-finalize:
    // it emits no Landed event, so a finalize stream would only burn the ADR 0009 ceiling. The
    // standalone `ARGUS_INJECT` path passes `false` and stays sim-only (its ADR 0010 behavior).
    // Non-landing is authoritative from the absent `landed_slot` — no inflight poll needed. The
    // preflight simulation below stays the *classification* source either way (an all-or-nothing
    // bundle leaves no on-chain meta — ADR 0010's core reason still holds).
    if send_faulted {
        let send_results = bundle::submit_all_regions(regions, &txs, auth).await;
        let accepted = send_results.iter().filter(|(_, r)| r.is_ok()).count();
        if let Some(bid) = send_results.iter().find_map(|(_, r)| r.as_ref().ok().cloned()) {
            let _ = store.set_bundle_id(run_id, attempt, &nonce, &bid);
            info!(accepted, total = send_results.len(), bundle_id = %bid, %explorer, "INJECT: faulted bundle sent (non-landing, free) — recorded");
        } else {
            warn!(accepted, "INJECT: no region accepted the faulted bundle — still recorded as a non-landing Submission");
        }
    }

    // 2) Classify via preflight simulation (the deterministic reason source).
    let sim = rpc::simulate_transaction(&cfg.rpc_http_url, &txs[0]).await?;
    let class = failure::classify_failure(&sim).unwrap_or(model::FailureClass::BundleFailure);
    store.set_failure_class(run_id, attempt, &nonce, class)?;
    let err_text = sim.err.clone().unwrap_or_default();
    warn!(?injection, ?class, err = %err_text, instruction_error = ?sim.instruction_error, units_consumed = ?sim.units_consumed, "INJECT: classified Failure");

    // 3) Decide a Remedy via the selected Policy (the AI Agent over HTTP when
    // ARGUS_POLICY=agent, else the local stand-in). These three fetches populate the
    // FailureContext the Agent reasons over (the Local policy reads only failure_class);
    // run them concurrently so they're off the serial path.
    let (tip_floor_p50, tip_floor_p75, current_slot) = tokio::join!(
        tip::fetch_tip_lamports(&cfg.jito_tip_floor_url, 50),
        tip::fetch_tip_lamports(&cfg.jito_tip_floor_url, 75),
        rpc::get_slot(&cfg.rpc_http_url),
    );
    // The Agent gets honest Options (a fetch failure is `None`, not a fabricated 0/base it
    // can't distinguish from a real value). `apply_remedy`'s BumpTip math still needs a
    // concrete floor, so keep a separate `bump_floor` that falls back to base_tip locally.
    let (tip_floor_p50, tip_floor_p75, current_slot) =
        (tip_floor_p50.ok(), tip_floor_p75.ok(), current_slot.ok());
    let bump_floor = tip_floor_p75.unwrap_or(base_tip);
    // The raw failure surface the Agent reasons over instead of the four-class verdict
    // (ADR 0012): the failing program (parsed from the logs), the structured error variant,
    // and the full program logs. `failure_class` is still set on the context but is
    // `serde(skip)` — kept for the Local policy / baseline column, never sent to the Agent.
    let failing_pid = failure::failing_program_id(&sim.logs);
    let ctx = agent_client::FailureContext {
        failure_class: class,
        attempt: attempt as u32,
        error_text: &err_text,
        instruction_error: sim.instruction_error.as_deref(),
        failing_program_id: failing_pid.as_deref(),
        program_logs: &sim.logs,
        tip_lamports: base_tip,
        tip_floor_p50,
        tip_floor_p75,
        blockhash_age_slots: matches!(injection, Injection::ExpiredBlockhash)
            .then_some(failure::AGED_BLOCKHASH_SLOTS),
        cu_limit,
        cu_used: sim.units_consumed,
        current_slot,
    };
    let decision = policy.decide(&ctx).await;
    // ADR 0006: a scored decision must carry a Reasoning Trace. On the Agent path, warn
    // loudly if it came back empty (provider hiccup / non-reasoning fallback model) so the
    // gap is caught live during the Run, not at Lifecycle-Log assembly. The decision is
    // still valid and kept — only the visible-reasoning evidence is weak.
    if cfg.use_agent {
        if failure::is_blank(decision.reasoning_trace.as_deref()) {
            warn!(model = ?decision.model, ?class, remedy = ?decision.remedy,
                "INJECT: agent decision recorded with EMPTY reasoning trace — weak scored evidence (ADR 0006)");
        }
        // A blank model slug breaks the ADR 0006 provenance filter (it can't be tied to a
        // reasoning-capable model), so warn at decision time too — same evidence-gap class.
        if failure::is_blank(decision.model.as_deref()) {
            warn!(?class, remedy = ?decision.remedy,
                "INJECT: agent decision recorded with EMPTY model slug — provenance gap (ADR 0006)");
        }
    }
    // The baseline contrast (ADR 0012): what the four-class default policy WOULD do for this
    // class, persisted next to the Agent's chosen remedy + diagnosis + triage. Computed here,
    // where `default_remedy` is in scope, so `storage` stays a policy-free sink.
    let baseline_remedy = failure::default_remedy(class);
    store.record_decision(
        run_id,
        attempt,
        class,
        decision.remedy,
        baseline_remedy,
        decision.diagnosis.as_deref(),
        decision.triage,
        &decision.rationale,
        Some(decision.confidence),
        decision.reasoning_trace.as_deref(),
        decision.model.as_deref(),
        now_millis() as i64,
    )?;
    info!(?class, remedy = ?decision.remedy, triage = ?decision.triage, model = ?decision.model, confidence = decision.confidence, failing_program = ?failing_pid, diagnosis = ?decision.diagnosis, rationale = %decision.rationale, "INJECT: Diagnosis + Remedy chosen (baseline class shown for contrast)");

    // 4) Execute the Remedy hook: next-attempt state, or stop on Abort. For a compute
    // remedy, observe the TRUE CU need from a max-CU re-simulation of the clean attempt-2
    // tx (the failed attempt-1 sim's units_consumed ≈ the injected cap, so it can't be
    // read directly) — apply_remedy then DERIVES the limit instead of using a magic floor.
    let cu_used = if decision.remedy == model::Remedy::RaiseCuLimit {
        match observe_cu_need(cfg, payer, run_id, base_tip, tip_accounts[1 % tip_accounts.len()]).await {
            Ok(n) => {
                info!(observed_cu_need = n, "INJECT: max-CU re-sim observed the true compute need");
                Some(n)
            }
            Err(e) => {
                warn!(error = %e, "INJECT: max-CU re-sim failed — falling back to doubling the prior limit");
                None
            }
        }
    } else {
        None
    };
    // Seed attempt-2 CLEAN: the injected attempt-1 `cu_limit` (Some(1) for ComputeExceeded)
    // is attempt-1-only — carrying it forward would make any non-RaiseCuLimit remedy rebuild
    // with cu_limit=1 and re-fail. RaiseCuLimit derives its real limit from `cu_used` (the
    // max-CU re-sim), so the happy path is unchanged; the Agent still sees the failing limit
    // via FailureContext.cu_limit above. Only the retry seed is reset.
    let state = RetryState { tip_lamports: base_tip, cu_limit: None, priority_fee_microlamports: None };
    let (next, cont) = failure::apply_remedy(decision.remedy, state, bump_floor, cu_used);
    if !cont {
        warn!(?class, remedy = ?decision.remedy, "INJECT: Remedy = Abort — not retrying (Failure recorded, no landing)");
        if let Some(row) = store.fetch_submission(run_id, attempt, &nonce)? {
            info!(?row, "INJECT: persisted faulted row");
        }
        return Ok(());
    }

    // 5) Attempt 2: clean submission with the Remedy applied, tracked to finalize.
    info!(next_tip = next.tip_lamports, next_cu = ?next.cu_limit, "INJECT: applying Remedy and resubmitting (attempt 2)");
    let landed = submit_and_track_one(
        &SubmitCtx { cfg, store, payer, run_id, regions, auth },
        2,
        tip_accounts[1 % tip_accounts.len()],
        &next,
    )
    .await?;
    if landed {
        info!(?class, "INJECT: ✅ recovered — attempt 2 landed after the Remedy");
    } else {
        warn!(?class, "INJECT: attempt 2 did not land");
    }
    Ok(())
}
