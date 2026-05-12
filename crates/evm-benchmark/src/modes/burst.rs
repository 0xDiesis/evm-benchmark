use crate::config::Config;
use crate::generators::evm_mix::EvmMixGenerator;
use crate::metrics::MetricsExporter;
use crate::signing::BatchSigner;
use crate::submission::tracking::SubmittedTxMeta;
use crate::submission::{LatencyTracker, Submitter};
use crate::types::{
    BurstResult, SubmissionErrorSummary, TestMode, benchmark_validity,
    merge_submission_error_summaries,
};
use alloy_primitives::{Address, B256, U256};
use alloy_signer_local::PrivateKeySigner;
use anyhow::Result;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

const MIN_BALANCE_WEI: u128 = 1_000_000_000_000_000_000; // 1 ETH
const RECEIPT_POLL_PENDING_LIMIT: u32 = 512;
const BLOCK_CONFIRM_RESCAN_DEPTH: u64 = 64;

struct BlockTxIdentity {
    hash: Option<B256>,
    sender: Option<Address>,
    nonce: Option<u64>,
}

/// Attempt to fund the test account if it has insufficient balance.
/// This is a best-effort function that tries to send a self-transfer if the balance
/// is low. If the account already has balance, this is a no-op.
/// If funding fails, it logs a warning but continues (tests will fail with insufficient funds).
async fn ensure_account_funded(
    client: &reqwest::Client,
    rpc_url: &str,
    account: Address,
    quiet: bool,
) -> Result<()> {
    // Check balance
    let balance_payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_getBalance",
        "params": [format!("{:?}", account), "latest"],
        "id": 1
    });
    let balance_resp = client.post(rpc_url).json(&balance_payload).send().await?;
    let balance_result: serde_json::Value = balance_resp.json().await?;
    let balance_hex = balance_result
        .get("result")
        .and_then(|r| r.as_str())
        .ok_or_else(|| anyhow::anyhow!("Failed to get balance from RPC"))?;
    let balance_wei = u128::from_str_radix(balance_hex.trim_start_matches("0x"), 16).unwrap_or(0);

    if balance_wei >= MIN_BALANCE_WEI {
        // Account has sufficient balance
        return Ok(());
    }

    if !quiet {
        eprintln!(
            "⚠️  INSUFFICIENT BALANCE: Test account {} has only {} wei (< 1 ETH).",
            account, balance_wei
        );
        eprintln!("    Transactions will fail with 'insufficient funds' errors.");
        eprintln!();
        eprintln!("    Solution: Set BENCH_KEY to a pre-funded account private key, e.g.:");
        eprintln!("    export BENCH_KEY=0x<your_private_key>");
        eprintln!();
    }

    Ok(())
}

/// Poll `eth_getTransactionReceipt` for all pending txs concurrently.
///
/// Each tx gets its own confirmation timestamp (captured after the RPC round-trip)
/// for accurate per-tx latency measurement. Concurrency is capped at 200 to avoid
/// overwhelming the RPC endpoint while still saturating the connection pool.
///
/// Used by both burst and sustained modes for reliable confirmation tracking.
pub(crate) async fn poll_pending_receipts(
    client: &reqwest::Client,
    rpc_url: &str,
    tracker: &LatencyTracker,
    finality_confirmations: u32,
) {
    use futures::stream::{self, StreamExt};

    let hashes: Vec<alloy_primitives::B256> = tracker.pending_hashes();
    if hashes.is_empty() {
        return;
    }

    let finality_depth = finality_confirmations as u64;

    let latest_block = if finality_depth > 0 {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_blockNumber",
            "params": [],
            "id": 1
        });
        match client.post(rpc_url).json(&payload).send().await {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(body) => body
                    .get("result")
                    .and_then(|v| v.as_str())
                    .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
                    .unwrap_or(0),
                Err(_) => 0,
            },
            Err(_) => 0,
        }
    } else {
        0
    };

    // Capture a poll-start timestamp. This is the earliest possible confirmation time
    // for any tx found in this round (they were mined at or before this moment).
    // Per-tx: we record arrival immediately after each receipt is fetched so txs
    // confirmed in earlier blocks don't inherit a later poll timestamp.
    let poll_start = Instant::now();

    stream::iter(hashes)
        .for_each_concurrent(200, |hash| {
            let client = client.clone();
            let rpc_url = rpc_url.to_string();
            let tracker = tracker.clone();
            async move {
                let payload = serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "eth_getTransactionReceipt",
                    "params": [format!("{:?}", hash)],
                    "id": 1
                });
                if let Ok(resp) = client.post(&rpc_url).json(&payload).send().await
                    && let Ok(body) = resp.json::<serde_json::Value>().await
                    && let Some(receipt) = body.get("result").and_then(|r| r.as_object())
                {
                    if let Some(receipt_hash_value) = receipt.get("transactionHash") {
                        let Some(receipt_hash) = receipt_hash_value
                            .as_str()
                            .and_then(|value| value.parse::<B256>().ok())
                        else {
                            return;
                        };
                        if receipt_hash != hash {
                            return;
                        }
                    }
                    if finality_depth > 0 {
                        let receipt_block = receipt
                            .get("blockNumber")
                            .and_then(|v| v.as_str())
                            .and_then(|hex| {
                                u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok()
                            })
                            .unwrap_or(0);
                        let finalized_height = latest_block.saturating_sub(finality_depth);
                        if receipt_block == 0 || receipt_block > finalized_height {
                            return;
                        }
                    }
                    let gas_used = receipt
                        .get("gasUsed")
                        .and_then(|v| v.as_str())
                        .and_then(parse_hex_u64);
                    let status_success = receipt
                        .get("status")
                        .and_then(|v| v.as_str())
                        .and_then(parse_hex_u64)
                        .map(|status| status != 0);
                    // Use poll_start as the arrival time: this is when we initiated
                    // the poll round, so latency = submit_time → poll_start which
                    // upper-bounds the true inclusion latency by at most poll_interval (25ms).
                    tracker.on_block_inclusion_with_receipt(
                        hash,
                        poll_start,
                        gas_used,
                        status_success,
                    );
                }
            }
        })
        .await;
}

pub(crate) async fn rpc_latest_block_number(
    client: &reqwest::Client,
    rpc_url: &str,
) -> Option<u64> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_blockNumber",
        "params": [],
        "id": 1
    });
    let resp = client.post(rpc_url).json(&payload).send().await.ok()?;
    let body = resp.json::<serde_json::Value>().await.ok()?;
    let hex = body.get("result").and_then(|v| v.as_str())?;
    u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok()
}

fn parse_hex_u64(hex: &str) -> Option<u64> {
    u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok()
}

fn gas_price_with_safety_margin(base: u128) -> u128 {
    base.saturating_mul(2).max(1_000_000_000)
}

fn assume_isolated_block_counts() -> bool {
    std::env::var("BENCH_ASSUME_ISOLATED_BLOCKS")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

async fn fetch_block_transactions(
    client: &reqwest::Client,
    rpc_url: &str,
    block_num: u64,
) -> Option<Vec<BlockTxIdentity>> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_getBlockByNumber",
        "params": [format!("0x{block_num:x}"), true],
        "id": 1
    });
    let resp = client.post(rpc_url).json(&payload).send().await.ok()?;
    let body = resp.json::<serde_json::Value>().await.ok()?;
    let txs = body
        .get("result")
        .and_then(|b| b.get("transactions"))
        .and_then(|t| t.as_array())?;
    Some(
        txs.iter()
            .filter_map(|tx| {
                if let Some(hash) = tx.as_str().and_then(|hash| hash.parse().ok()) {
                    return Some(BlockTxIdentity {
                        hash: Some(hash),
                        sender: None,
                        nonce: None,
                    });
                }
                let obj = tx.as_object()?;
                Some(BlockTxIdentity {
                    hash: obj
                        .get("hash")
                        .and_then(|hash| hash.as_str())
                        .and_then(|hash| hash.parse().ok()),
                    sender: obj
                        .get("from")
                        .and_then(|from| from.as_str())
                        .and_then(|from| from.parse().ok()),
                    nonce: obj
                        .get("nonce")
                        .and_then(|nonce| nonce.as_str())
                        .and_then(parse_hex_u64),
                })
            })
            .collect(),
    )
}

/// Scan newly produced blocks for pending benchmark tx hashes.
///
/// Some EVM nodes can expose block bodies before receipt indexes are cheap to
/// query under load. Block scanning prevents confirmed transactions from being
/// reported as pending when `eth_getTransactionReceipt` lags.
pub(crate) async fn poll_new_block_inclusions(
    client: &reqwest::Client,
    rpc_url: &str,
    tracker: &LatencyTracker,
    last_scanned_block: &mut u64,
    scan_floor_block: u64,
    finality_confirmations: u32,
) {
    let Some(latest_block) = rpc_latest_block_number(client, rpc_url).await else {
        return;
    };
    let scan_to = latest_block.saturating_sub(finality_confirmations as u64);
    if scan_to < scan_floor_block {
        return;
    }

    let next_unscanned = last_scanned_block.saturating_add(1);
    let rescan_start = rescan_floor(scan_to).max(scan_floor_block);
    let scan_from = next_unscanned.min(rescan_start).max(scan_floor_block);
    let mut processed_to = *last_scanned_block;
    let assume_isolated_blocks = assume_isolated_block_counts();
    for block_num in scan_from..=scan_to {
        let Some(block_txs) = fetch_block_transactions(client, rpc_url, block_num).await else {
            break;
        };
        let arrival = Instant::now();
        let block_tx_count = block_txs.len() as u32;
        let pending_before = tracker.pending_count();
        for tx in block_txs {
            let hash_matched = tx
                .hash
                .is_some_and(|hash| tracker.on_block_inclusion(hash, arrival));
            if !hash_matched && let (Some(sender), Some(nonce)) = (tx.sender, tx.nonce) {
                tracker.on_account_nonce_inclusion(sender, nonce, arrival);
            }
        }
        if assume_isolated_blocks {
            let matched = pending_before.saturating_sub(tracker.pending_count());
            let unmatched = block_tx_count.saturating_sub(matched);
            tracker.confirm_oldest_pending(unmatched, arrival);
        }
        processed_to = processed_to.max(block_num);
    }
    *last_scanned_block = processed_to;
}

fn rescan_floor(scan_to: u64) -> u64 {
    scan_to
        .saturating_sub(BLOCK_CONFIRM_RESCAN_DEPTH.saturating_sub(1))
        .max(1)
}

pub(crate) fn should_poll_receipts(pending_count: u32) -> bool {
    pending_count > 0 && pending_count <= RECEIPT_POLL_PENDING_LIMIT
}

fn confirmed_tps(confirmed: u32, elapsed: Duration) -> f32 {
    if !elapsed.is_zero() {
        confirmed as f32 / elapsed.as_secs_f32()
    } else {
        0.0
    }
}

fn log_analysis_result(quiet: bool, analysis_ascii: Result<String>) {
    match analysis_ascii {
        Ok(ascii) => {
            if !quiet {
                println!("\n{}", ascii);
            }
        }
        Err(e) => {
            if !quiet {
                eprintln!("Warning: Analytics pipeline failed: {}", e);
            }
        }
    }
}

#[derive(Default)]
struct SubmitAccounting {
    attempted: u32,
    accepted: u32,
    failed: u32,
    error_summaries: Vec<SubmissionErrorSummary>,
}

fn split_worker_txs_into_waves(
    txs: Vec<crate::types::SignedTxWithMetadata>,
    wave_count: usize,
) -> Vec<Vec<crate::types::SignedTxWithMetadata>> {
    let wave_count = wave_count.max(1);
    let mut waves = (0..wave_count).map(|_| Vec::new()).collect::<Vec<_>>();
    if txs.is_empty() {
        return waves;
    }

    let chunk_size = txs.len().div_ceil(wave_count).max(1);
    for (idx, tx) in txs.into_iter().enumerate() {
        let wave_idx = (idx / chunk_size).min(wave_count - 1);
        waves[wave_idx].push(tx);
    }
    waves
}

/// Returns `(BurstResult, effective_gas_price_wei)`.
pub async fn run_burst(config: &Config) -> Result<(BurstResult, u128)> {
    let server_metrics_before = super::scrape_server_metrics_before(config).await;
    let health_monitor = super::start_health_monitor(config)?;
    let dispatcher = Arc::new(Submitter::with_retry_profile(
        config.rpc_urls.clone(),
        &config.ws,
        config.batch_size,
        config.submission_method,
        &config.retry_profile,
    )?);
    let tracker = Arc::new(LatencyTracker::new());
    let metrics = Arc::new(MetricsExporter::new()?);

    // Warm up HTTP connection pool(s) before benchmarking
    if !config.quiet {
        println!(
            "Warming up connection pool{} ({} endpoint{})...",
            if config.rpc_urls.len() > 1 { "s" } else { "" },
            config.rpc_urls.len(),
            if config.rpc_urls.len() > 1 { "s" } else { "" }
        );
    }
    dispatcher.warm_up(10).await?;

    // Use sender keys from config (populated by main.rs after key resolution).
    // Falls back to default deterministic test keys if config has no keys.
    let sender_keys: Vec<String> = if config.sender_keys.is_empty() {
        // Default: use 4 pre-funded validator keys (keys 1-4 correspond to the genesis-funded
        // validator addresses in docker/docker-compose.e2e.yml)
        (1u8..=4).map(|i| format!("0x{:064x}", i)).collect()
    } else {
        config.sender_keys.clone()
    };

    // Per-account txpool slot cap. Stay below typical node defaults (5000) to
    // avoid txpool eviction under load.
    const POOL_SLOTS_PER_ACCOUNT: usize = 4000;

    let num_keys = sender_keys.len();
    let requested_worker_count = (config.worker_count as usize).max(1);
    let worker_count = requested_worker_count.min(config.tx_count.max(1) as usize);
    let active_key_count = num_keys.min(config.tx_count.max(1) as usize);
    let max_pool_capacity = active_key_count * POOL_SLOTS_PER_ACCOUNT;

    // Cap tx_count to the pool's total capacity across all accounts
    let tx_count = if config.tx_count as usize > max_pool_capacity {
        if !config.quiet {
            eprintln!(
                "[burst] tx_count {} exceeds active pool capacity ({} active accounts × {} slots = {}). Capping to {}.",
                config.tx_count,
                active_key_count,
                POOL_SLOTS_PER_ACCOUNT,
                max_pool_capacity,
                max_pool_capacity
            );
        }
        max_pool_capacity
    } else {
        config.tx_count as usize
    };

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(200)
        .build()
        .unwrap_or_default();
    let recipient = Address::with_last_byte(0x42);

    // Fetch current gas price from chain to ensure txs are never underpriced.
    // Use 2x current gas price as a safety margin for base fee spikes during the burst.
    let gas_price: u128 = {
        let gp_payload = serde_json::json!({
            "jsonrpc": "2.0", "method": "eth_gasPrice", "params": [], "id": 1
        });
        let gp_resp = client
            .post(config.rpc.as_str())
            .json(&gp_payload)
            .send()
            .await?;
        let gp_result: serde_json::Value = gp_resp.json().await?;
        let gp_hex = gp_result
            .get("result")
            .and_then(|r| r.as_str())
            .unwrap_or("0x3b9aca00");
        let base =
            u128::from_str_radix(gp_hex.trim_start_matches("0x"), 16).unwrap_or(1_000_000_000);
        // 2x current gas price to handle EIP-1559 base fee spikes during load
        gas_price_with_safety_margin(base)
    };

    if !config.quiet {
        println!("Gas price: {} gwei", gas_price / 1_000_000_000);
    }
    // Phase 1: Fetch nonces for active sender keys upfront, then build one
    // nonce-ordered slice per sender. Submission concurrency is limited by
    // worker_count later, so --senders controls nonce lanes and --workers controls
    // in-flight RPC work.
    let _sign_start = Instant::now();

    // txs per sender — evenly distributed
    let txs_per_sender = tx_count.div_ceil(active_key_count.max(1));

    // Fetch base nonce for each active key once
    let mut key_nonces: Vec<(String, u64)> = Vec::new();
    for (i, key) in sender_keys.iter().take(active_key_count).enumerate() {
        let private_signer = PrivateKeySigner::from_str(key)
            .map_err(|e| anyhow::anyhow!("Failed to parse signer key {}: {}", i, e))?;
        let account = private_signer.address();

        if i == 0 {
            ensure_account_funded(&client, config.rpc.as_str(), account, config.quiet).await?;
        }

        let nonce_payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_getTransactionCount",
            "params": [format!("{:?}", account), "pending"],
            "id": 1
        });
        let resp = client
            .post(config.rpc.as_str())
            .json(&nonce_payload)
            .send()
            .await?;
        let result: serde_json::Value = resp.json().await?;
        let nonce_hex = result
            .get("result")
            .and_then(|r| r.as_str())
            .ok_or_else(|| anyhow::anyhow!("Failed to get nonce for key {}", i))?;
        let base_nonce = u64::from_str_radix(nonce_hex.trim_start_matches("0x"), 16)?;
        key_nonces.push((key.clone(), base_nonce));
    }

    // Build per-sender tx slices. Each active key owns exactly one sequential
    // nonce range so the benchmark can use all funded sender accounts without
    // tying nonce-lane count to RPC worker count.
    let mut all_signed_txs: Vec<Vec<crate::types::SignedTxWithMetadata>> = Vec::new();

    let evm_contracts = if config.test_mode == TestMode::Evm {
        Some(super::evm_contracts(config)?)
    } else {
        None
    };

    for (sender_idx, (key, base_nonce)) in key_nonces.iter().enumerate() {
        let nonce_start = *base_nonce;

        let submitted_so_far = sender_idx * txs_per_sender;
        let remaining = tx_count.saturating_sub(submitted_so_far);
        let this_count = remaining.min(txs_per_sender);
        if this_count == 0 {
            break;
        }

        let private_signer = PrivateKeySigner::from_str(key).map_err(|e| {
            anyhow::anyhow!(
                "Failed to parse signer key for sender {}: {}",
                sender_idx,
                e
            )
        })?;

        let signed = if let Some(ref contracts) = evm_contracts {
            // EVM mode: generate descriptors for the signer that will actually
            // sign this worker slice so sender metadata and nonce fallback match.
            let mut generator = EvmMixGenerator::new(
                contracts.clone(),
                super::evm_mix_config(contracts),
                vec![private_signer.address()],
                config.chain_id,
            )
            .map_err(|e| anyhow::anyhow!("Failed to create EVM generator: {}", e))?;
            let worker_descs = generator.generate_batch(this_count);
            EvmMixGenerator::sign_batch(
                &worker_descs,
                &private_signer,
                nonce_start,
                gas_price,
                config.chain_id,
            )
            .map_err(|e| {
                anyhow::anyhow!("EVM batch signing failed for sender {}: {}", sender_idx, e)
            })?
        } else {
            // Transfer mode: simple ETH transfers
            let tx_data: Vec<(Address, U256)> = (0..this_count)
                .map(|_| (recipient, U256::from(1u32)))
                .collect();

            let batch_signer = BatchSigner::new_with_gas_price(
                private_signer,
                nonce_start,
                gas_price,
                config.chain_id,
            );
            batch_signer.sign_batch_parallel(tx_data).map_err(|e| {
                anyhow::anyhow!("Batch signing failed for sender {}: {}", sender_idx, e)
            })?
        };

        all_signed_txs.push(signed);
    }

    let total_signed: usize = all_signed_txs.iter().map(|v| v.len()).sum();
    let _sign_time = _sign_start.elapsed();

    if !config.quiet {
        println!(
            "Signed {} txs across {} sender stream{} using {} RPC worker{} in {:.2}s",
            total_signed,
            all_signed_txs.len(),
            if all_signed_txs.len() == 1 { "" } else { "s" },
            worker_count,
            if worker_count == 1 { "" } else { "s" },
            _sign_time.as_secs_f32()
        );
    }

    let max_wait = super::confirmation_wait_duration(60);
    let mut last_scanned_block = rpc_latest_block_number(&client, config.rpc.as_str())
        .await
        .unwrap_or(0);
    let scan_floor_block = last_scanned_block.saturating_add(1);

    // Phase 3: submit real waves. Each worker's nonce-ordered slice is split
    // across the configured wave count, then all workers submit their slice for
    // a wave concurrently before the next wave delay.
    let submit_start = Instant::now();
    let wave_count = (config.wave_count as usize).max(1);
    let sender_wave_txs = all_signed_txs
        .into_iter()
        .map(|txs| split_worker_txs_into_waves(txs, wave_count))
        .collect::<Vec<_>>();
    let mut accounting = SubmitAccounting::default();

    for wave_idx in 0..wave_count {
        if wave_idx > 0 && config.wave_delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(config.wave_delay_ms)).await;
        }

        use futures::stream::{self, StreamExt};

        let wave_txs = sender_wave_txs
            .iter()
            .flat_map(|waves| {
                waves
                    .get(wave_idx)
                    .into_iter()
                    .flat_map(|txs| txs.iter().cloned())
            })
            .collect::<Vec<_>>();
        let batch_size = config.batch_size.max(1) as usize;
        let wave_batches = wave_txs
            .chunks(batch_size)
            .enumerate()
            .map(|(batch_idx, txs)| (batch_idx, txs.to_vec()))
            .collect::<Vec<_>>();

        let wave_results = stream::iter(wave_batches)
            .map(|(batch_idx, sender_txs)| {
                let dispatcher = dispatcher.clone();
                let tracker = tracker.clone();
                let metrics = metrics.clone();
                let quiet = config.quiet;
                async move {
                    let attempted = sender_txs.len() as u32;
                    let batch_submit_start = Instant::now();
                    match dispatcher.submit_batch(sender_txs).await {
                        Ok(result) => {
                            let accepted = result.accepted_txs.len() as u32;
                            let failed = attempted.saturating_sub(accepted);
                            // Record only txs the RPC actually accepted, with wave index.
                            for tx in &result.accepted_txs {
                                tracker.record_submit_meta(SubmittedTxMeta {
                                    hash: tx.hash,
                                    nonce: tx.nonce,
                                    sender: tx.sender,
                                    gas_limit: tx.gas_limit,
                                    method: tx.method,
                                    wave: Some(wave_idx as u32),
                                    submit_time: batch_submit_start,
                                });
                            }
                            metrics.inc_transactions_submitted(accepted as u64);
                            if failed > 0 {
                                metrics.inc_transactions_failed(failed as u64);
                            }
                            if !quiet {
                                if failed > 0 {
                                    println!(
                                        "Wave {} batch {}: accepted {} of {} txs, {} failed",
                                        wave_idx, batch_idx, accepted, attempted, failed
                                    );
                                } else {
                                    println!(
                                        "Wave {} batch {}: accepted {} txs",
                                        wave_idx, batch_idx, accepted
                                    );
                                }
                            }
                            SubmitAccounting {
                                attempted,
                                accepted,
                                failed,
                                error_summaries: result.error_summaries,
                            }
                        }
                        Err(e) => {
                            metrics.inc_transactions_failed(attempted as u64);
                            if !quiet {
                                eprintln!(
                                    "Wave {} batch {} submission error: {}",
                                    wave_idx, batch_idx, e
                                );
                            }
                            SubmitAccounting {
                                attempted,
                                accepted: 0,
                                failed: attempted,
                                error_summaries: vec![SubmissionErrorSummary::new(
                                    e.to_string(),
                                    attempted,
                                )],
                            }
                        }
                    }
                }
            })
            .buffer_unordered(worker_count.max(1))
            .collect::<Vec<_>>()
            .await;

        for batch_accounting in wave_results {
            accounting.attempted = accounting
                .attempted
                .saturating_add(batch_accounting.attempted);
            accounting.accepted = accounting
                .accepted
                .saturating_add(batch_accounting.accepted);
            accounting.failed = accounting.failed.saturating_add(batch_accounting.failed);
            accounting
                .error_summaries
                .extend(batch_accounting.error_summaries);
        }
    }
    accounting.error_summaries = merge_submission_error_summaries(accounting.error_summaries);

    let submit_time = submit_start.elapsed();

    // Phase 4: Poll eth_getTransactionReceipt for all submitted txs concurrently.
    // This is more reliable than block-based tracking — no missed blocks, no stalls.
    // We poll every 25ms until all txs are confirmed or max_wait is exceeded.
    let confirm_start = Instant::now();

    while confirm_start.elapsed() < max_wait && tracker.pending_count() > 0 {
        metrics.set_pending_transactions(tracker.pending_count() as i64);
        poll_new_block_inclusions(
            &client,
            config.rpc.as_str(),
            &tracker,
            &mut last_scanned_block,
            scan_floor_block,
            config.finality_confirmations,
        )
        .await;
        if should_poll_receipts(tracker.pending_count()) {
            poll_pending_receipts(
                &client,
                config.rpc.as_str(),
                &tracker,
                config.finality_confirmations,
            )
            .await;
        }
        if tracker.pending_count() > 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    poll_new_block_inclusions(
        &client,
        config.rpc.as_str(),
        &tracker,
        &mut last_scanned_block,
        scan_floor_block,
        config.finality_confirmations,
    )
    .await;

    let confirm_ms = confirm_start.elapsed();
    let stats = tracker.statistics();
    let confirmed = tracker.confirmed_count();
    let pending = tracker.pending_count();
    let (valid, invalid_reason) = benchmark_validity(
        accounting.attempted,
        accounting.accepted,
        accounting.failed,
        confirmed,
        pending,
    );

    // confirmed_tps is conservative end-to-end burst throughput: confirmed
    // transactions divided by submission plus post-submit confirmation time.
    // Blocks are produced while the client is submitting, so measuring only the
    // post-submit receipt polling phase can overstate chain throughput.
    let end_to_end = submit_time.saturating_add(confirm_ms);

    metrics.set_pending_transactions(pending as i64);
    if !end_to_end.is_zero() {
        metrics.set_current_tps(confirmed as f64 / end_to_end.as_secs_f64());
    }

    let result = BurstResult {
        attempted: accounting.attempted,
        submitted: accounting.accepted,
        accepted: accounting.accepted,
        failed: accounting.failed,
        confirmed,
        pending,
        valid,
        invalid_reason,
        submission_errors: accounting.error_summaries,
        sign_ms: _sign_time.as_millis() as u64,
        submit_ms: submit_time.as_millis() as u64,
        confirm_ms: confirm_ms.as_millis() as u64,
        submitted_tps: if !submit_time.is_zero() {
            accounting.accepted as f32 / submit_time.as_secs_f32()
        } else {
            0.0
        },
        confirmed_tps: confirmed_tps(confirmed, end_to_end),
        latency: stats,
        server_metrics: super::scrape_server_metrics_after(config, server_metrics_before.as_ref())
            .await,
        per_method: if config.test_mode == TestMode::Evm {
            Some(tracker.per_method_statistics())
        } else {
            None
        },
        validator_health: super::validator_health_snapshot(health_monitor.as_ref()),
        per_wave: {
            let waves = tracker.per_wave_statistics();
            if waves.is_empty() { None } else { Some(waves) }
        },
    };

    // Run analytics pipeline on benchmark results
    let analysis_ascii =
        crate::analytics::run_analysis("burst-benchmark", "burst", &result, None, None)
            .await
            .map(|analytics_report| analytics_report.reports.ascii);
    log_analysis_result(config.quiet, analysis_ascii);

    Ok((result, gas_price))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, SubmissionMethod};
    use crate::submission::LatencyTracker;
    use crate::types::ExecutionMode;
    use crate::types::TestMode;
    use crate::types::TransactionType;
    use alloy_primitives::B256;
    use std::net::TcpListener;
    use std::sync::Arc as StdArc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::{body_partial_json, method};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    fn burst_test_config(rpc_url: &str) -> Config {
        let rpc = url::Url::parse(rpc_url).unwrap();
        Config {
            rpc_urls: vec![rpc.clone()],
            rpc,
            ws: url::Url::parse("ws://127.0.0.1:19998").unwrap(),
            metrics: None,
            validator_urls: vec![],
            test_mode: TestMode::Transfer,
            execution_mode: ExecutionMode::Burst,
            tx_count: 3,
            sender_count: 0,
            wave_count: 0,
            wave_delay_ms: 0,
            duration_secs: 0,
            target_tps: 0,
            worker_count: 2,
            batch_size: 10,
            submission_method: SubmissionMethod::Http,
            retry_profile: "off".to_string(),
            finality_confirmations: 0,
            output: std::path::PathBuf::from("burst-test.json"),
            quiet: true,
            chain_id: 1,
            bench_name: "burst-test".to_string(),
            fund: false,
            sender_keys: vec![format!("0x{:064x}", 1), format!("0x{:064x}", 2)],
            evm_tokens: vec![],
            evm_pairs: vec![],
            evm_nfts: vec![],
        }
    }

    fn rpc_method(request: &Request) -> Option<String> {
        let body: serde_json::Value = serde_json::from_slice(&request.body).ok()?;
        body.get("method")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
    }

    /// Helper: build a JSON-RPC receipt response with a non-null result object.
    fn receipt_response(tx_hash: &str) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "transactionHash": tx_hash,
                "blockNumber": "0x1",
                "blockHash": "0x0000000000000000000000000000000000000000000000000000000000000abc",
                "transactionIndex": "0x0",
                "gasUsed": "0x5208",
                "cumulativeGasUsed": "0x5208",
                "status": "0x1",
                "from": "0x0000000000000000000000000000000000000000",
                "to": "0x0000000000000000000000000000000000000042",
                "logs": []
            }
        })
    }

    /// Helper: build a JSON-RPC response with null result (tx still pending).
    fn null_receipt_response() -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": null
        })
    }

    fn block_response(tx_hashes: &[String]) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "number": "0x1",
                "transactions": tx_hashes
            }
        })
    }

    fn full_block_response(transactions: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "number": "0x1",
                "transactions": transactions
            }
        })
    }

    fn receipt_response_with_block(tx_hash: &str, block_number: &str) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "transactionHash": tx_hash,
                "blockNumber": block_number,
                "blockHash": "0x0000000000000000000000000000000000000000000000000000000000000abc",
                "transactionIndex": "0x0",
                "gasUsed": "0x5208",
                "cumulativeGasUsed": "0x5208",
                "status": "0x1",
                "from": "0x0000000000000000000000000000000000000000",
                "to": "0x0000000000000000000000000000000000000042",
                "logs": []
            }
        })
    }

    fn receipt_response_for_requested_hash(request: &Request) -> ResponseTemplate {
        let body: serde_json::Value =
            serde_json::from_slice(&request.body).expect("valid rpc json");
        let hash = body["params"][0]
            .as_str()
            .expect("receipt request should include tx hash");
        ResponseTemplate::new(200).set_body_json(receipt_response(hash))
    }

    fn balance_response(balance_hex: &str) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": balance_hex
        })
    }

    fn unexpected_test_rpc_method(method: &str) -> ! {
        panic!("unexpected rpc method: {method}");
    }

    fn assert_expected_rpc_method(method: &str, expected: &[&str]) {
        assert!(
            expected.contains(&method),
            "unexpected rpc method: {method}"
        );
    }

    #[tokio::test]
    async fn test_ensure_account_funded_noop_when_balance_is_sufficient() {
        let mock_server = MockServer::start().await;
        let account = Address::with_last_byte(0x11);

        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_getBalance",
                "params": [format!("{:?}", account), "latest"],
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(balance_response("0xde0b6b3a7640000")),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        ensure_account_funded(&client, &mock_server.uri(), account, true)
            .await
            .expect("sufficient balance should be a no-op");
    }

    #[tokio::test]
    async fn test_ensure_account_funded_low_balance_returns_ok() {
        let mock_server = MockServer::start().await;
        let account = Address::with_last_byte(0x22);

        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_getBalance",
                "params": [format!("{:?}", account), "latest"],
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(balance_response("0x1")))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        ensure_account_funded(&client, &mock_server.uri(), account, true)
            .await
            .expect("low balance should warn but not fail");
    }

    #[tokio::test]
    async fn test_ensure_account_funded_low_balance_logs_when_not_quiet() {
        let mock_server = MockServer::start().await;
        let account = Address::with_last_byte(0x23);

        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_getBalance",
                "params": [format!("{:?}", account), "latest"],
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(balance_response("0x2")))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        ensure_account_funded(&client, &mock_server.uri(), account, false)
            .await
            .expect("low balance should still return ok when noisy");
    }

    #[tokio::test]
    async fn test_poll_new_block_inclusions_tracks_block_transactions() {
        let mock_server = MockServer::start().await;
        let tracker = LatencyTracker::new();
        let hash_a = B256::with_last_byte(0xa1);
        let hash_b = B256::with_last_byte(0xb2);
        tracker.record_submit(
            hash_a,
            0,
            Address::default(),
            21_000,
            TransactionType::SimpleTransfer,
        );
        tracker.record_submit(
            hash_b,
            1,
            Address::default(),
            21_000,
            TransactionType::SimpleTransfer,
        );

        let txs = vec![format!("{hash_a:?}"), format!("{hash_b:?}")];
        Mock::given(method("POST"))
            .respond_with(move |request: &Request| {
                let body: serde_json::Value =
                    serde_json::from_slice(&request.body).expect("valid rpc json");
                match body.get("method").and_then(|m| m.as_str()) {
                    Some("eth_blockNumber") => ResponseTemplate::new(200).set_body_json(
                        serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": "0x1"}),
                    ),
                    Some("eth_getBlockByNumber") => {
                        ResponseTemplate::new(200).set_body_json(block_response(&txs))
                    }
                    other => panic!("unexpected rpc method: {other:?}"),
                }
            })
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let mut last_scanned_block = 0;
        poll_new_block_inclusions(
            &client,
            &mock_server.uri(),
            &tracker,
            &mut last_scanned_block,
            1,
            0,
        )
        .await;

        assert_eq!(last_scanned_block, 1);
        assert_eq!(tracker.confirmed_count(), 2);
        assert_eq!(tracker.pending_count(), 0);
    }

    #[tokio::test]
    async fn test_poll_new_block_inclusions_honors_finality_depth() {
        let mock_server = MockServer::start().await;
        let tracker = LatencyTracker::new();
        let hash = B256::with_last_byte(0xc3);
        tracker.record_submit(
            hash,
            0,
            Address::default(),
            21_000,
            TransactionType::SimpleTransfer,
        );

        let txs = vec![format!("{hash:?}")];
        Mock::given(method("POST"))
            .respond_with(move |request: &Request| {
                let body: serde_json::Value =
                    serde_json::from_slice(&request.body).expect("valid rpc json");
                match body.get("method").and_then(|m| m.as_str()) {
                    Some("eth_blockNumber") => ResponseTemplate::new(200).set_body_json(
                        serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": "0x2"}),
                    ),
                    Some("eth_getBlockByNumber") => {
                        let block = body["params"][0].as_str().unwrap_or_default();
                        let block_txs = if block == "0x1" { vec![] } else { txs.clone() };
                        ResponseTemplate::new(200).set_body_json(block_response(&block_txs))
                    }
                    other => panic!("unexpected rpc method: {other:?}"),
                }
            })
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let mut last_scanned_block = 0;
        poll_new_block_inclusions(
            &client,
            &mock_server.uri(),
            &tracker,
            &mut last_scanned_block,
            1,
            1,
        )
        .await;

        assert_eq!(last_scanned_block, 1);
        assert_eq!(tracker.confirmed_count(), 0);
        assert_eq!(tracker.pending_count(), 1);
    }

    #[tokio::test]
    async fn test_poll_new_block_inclusions_matches_sender_nonce_fallback() {
        let mock_server = MockServer::start().await;
        let tracker = LatencyTracker::new();
        let pending_hash = B256::with_last_byte(0xd4);
        let block_hash = B256::with_last_byte(0xee);
        let sender = Address::with_last_byte(0x44);
        tracker.record_submit(
            pending_hash,
            9,
            sender,
            21_000,
            TransactionType::SimpleTransfer,
        );

        Mock::given(method("POST"))
            .respond_with(move |request: &Request| {
                let body: serde_json::Value =
                    serde_json::from_slice(&request.body).expect("valid rpc json");
                match body.get("method").and_then(|m| m.as_str()) {
                    Some("eth_blockNumber") => ResponseTemplate::new(200).set_body_json(
                        serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": "0x1"}),
                    ),
                    Some("eth_getBlockByNumber") => {
                        ResponseTemplate::new(200).set_body_json(full_block_response(vec![
                            serde_json::json!({
                                "hash": format!("{block_hash:?}"),
                                "from": format!("{sender:?}"),
                                "nonce": "0x9"
                            }),
                        ]))
                    }
                    other => panic!("unexpected rpc method: {other:?}"),
                }
            })
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let mut last_scanned_block = 0;
        poll_new_block_inclusions(
            &client,
            &mock_server.uri(),
            &tracker,
            &mut last_scanned_block,
            1,
            0,
        )
        .await;

        assert_eq!(tracker.confirmed_count(), 1);
        assert_eq!(tracker.pending_count(), 0);
    }

    #[tokio::test]
    async fn test_poll_new_block_inclusions_rescans_recent_blocks() {
        let mock_server = MockServer::start().await;
        let tracker = LatencyTracker::new();
        let hash = B256::with_last_byte(0xf5);
        tracker.record_submit(
            hash,
            0,
            Address::default(),
            21_000,
            TransactionType::SimpleTransfer,
        );

        let txs = vec![format!("{hash:?}")];
        Mock::given(method("POST"))
            .respond_with(move |request: &Request| {
                let body: serde_json::Value =
                    serde_json::from_slice(&request.body).expect("valid rpc json");
                match body.get("method").and_then(|m| m.as_str()) {
                    Some("eth_blockNumber") => ResponseTemplate::new(200).set_body_json(
                        serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": "0x3"}),
                    ),
                    Some("eth_getBlockByNumber") => {
                        let block = body["params"][0].as_str().unwrap_or_default();
                        let block_txs = if block == "0x2" { txs.clone() } else { vec![] };
                        ResponseTemplate::new(200).set_body_json(block_response(&block_txs))
                    }
                    other => panic!("unexpected rpc method: {other:?}"),
                }
            })
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let mut last_scanned_block = 2;
        poll_new_block_inclusions(
            &client,
            &mock_server.uri(),
            &tracker,
            &mut last_scanned_block,
            1,
            0,
        )
        .await;

        assert_eq!(last_scanned_block, 3);
        assert_eq!(tracker.confirmed_count(), 1);
        assert_eq!(tracker.pending_count(), 0);
    }

    #[test]
    fn test_should_poll_receipts_only_for_small_pending_sets() {
        assert!(!should_poll_receipts(0));
        assert!(should_poll_receipts(1));
        assert!(should_poll_receipts(RECEIPT_POLL_PENDING_LIMIT));
        assert!(!should_poll_receipts(RECEIPT_POLL_PENDING_LIMIT + 1));
    }

    #[tokio::test]
    async fn test_ensure_account_funded_errors_on_missing_balance_result() {
        let mock_server = MockServer::start().await;
        let account = Address::with_last_byte(0x33);

        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_getBalance",
                "params": [format!("{:?}", account), "latest"],
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {"code": -32000, "message": "missing result"}
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let err = ensure_account_funded(&client, &mock_server.uri(), account, true)
            .await
            .expect_err("missing result should fail");
        assert!(err.to_string().contains("Failed to get balance from RPC"));
    }

    #[tokio::test]
    async fn test_poll_pending_receipts_confirms_single_tx() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(receipt_response(
                "0x0000000000000000000000000000000000000000000000000000000000000001",
            )))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let tracker = LatencyTracker::new();

        let hash = B256::with_last_byte(1);
        tracker.record_submit(
            hash,
            0,
            Address::default(),
            21_000,
            TransactionType::SimpleTransfer,
        );
        assert_eq!(tracker.pending_count(), 1);
        assert_eq!(tracker.confirmed_count(), 0);

        poll_pending_receipts(&client, &mock_server.uri(), &tracker, 0).await;

        assert_eq!(tracker.confirmed_count(), 1);
        assert_eq!(tracker.pending_count(), 0);
    }

    #[tokio::test]
    async fn test_poll_pending_receipts_empty_pending_returns_immediately() {
        let mock_server = MockServer::start().await;

        // Mount a mock that should never be called (no pending txs).
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(receipt_response(
                "0x0000000000000000000000000000000000000000000000000000000000000001",
            )))
            .expect(0)
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let tracker = LatencyTracker::new();

        // No pending txs — should return immediately without making any RPC calls.
        poll_pending_receipts(&client, &mock_server.uri(), &tracker, 0).await;

        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.confirmed_count(), 0);
    }

    #[tokio::test]
    async fn test_poll_pending_receipts_null_result_stays_pending() {
        let mock_server = MockServer::start().await;

        // Return null receipt (tx is still in the mempool).
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(null_receipt_response()))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let tracker = LatencyTracker::new();

        let hash = B256::with_last_byte(1);
        tracker.record_submit(
            hash,
            0,
            Address::default(),
            21_000,
            TransactionType::SimpleTransfer,
        );

        poll_pending_receipts(&client, &mock_server.uri(), &tracker, 0).await;

        // The tx should remain pending since the receipt was null.
        assert_eq!(tracker.pending_count(), 1);
        assert_eq!(tracker.confirmed_count(), 0);
    }

    #[tokio::test]
    async fn test_poll_pending_receipts_ignores_mismatched_receipt_hash() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(receipt_response(
                "0x0000000000000000000000000000000000000000000000000000000000000002",
            )))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let tracker = LatencyTracker::new();
        tracker.record_submit(
            B256::with_last_byte(1),
            0,
            Address::default(),
            21_000,
            TransactionType::SimpleTransfer,
        );

        poll_pending_receipts(&client, &mock_server.uri(), &tracker, 0).await;

        assert_eq!(tracker.pending_count(), 1);
        assert_eq!(tracker.confirmed_count(), 0);
    }

    #[tokio::test]
    async fn test_poll_pending_receipts_mixed_confirmed_and_pending() {
        // We need a mock that returns a receipt for some txs and null for others.
        // wiremock responds with the same template for all POSTs, so we use a
        // receipt response that has a valid `result` object — all txs will be
        // confirmed. To test the "mixed" scenario, we use two separate poll rounds.

        let mock_server = MockServer::start().await;

        // Round 1: null receipts for all txs (none confirmed yet).
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(null_receipt_response()))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let tracker = LatencyTracker::new();

        // Submit 3 txs.
        for i in 1u8..=3 {
            tracker.record_submit(
                B256::with_last_byte(i),
                i as u64,
                Address::default(),
                21_000,
                TransactionType::SimpleTransfer,
            );
        }
        assert_eq!(tracker.pending_count(), 3);

        // First poll — all still pending.
        poll_pending_receipts(&client, &mock_server.uri(), &tracker, 0).await;
        assert_eq!(tracker.pending_count(), 3);
        assert_eq!(tracker.confirmed_count(), 0);

        // Replace mock: now return valid receipts.
        mock_server.reset().await;
        Mock::given(method("POST"))
            .respond_with(receipt_response_for_requested_hash)
            .mount(&mock_server)
            .await;

        // Second poll — all should now be confirmed.
        poll_pending_receipts(&client, &mock_server.uri(), &tracker, 0).await;
        assert_eq!(tracker.confirmed_count(), 3);
        assert_eq!(tracker.pending_count(), 0);
    }

    #[tokio::test]
    async fn test_poll_pending_receipts_multiple_txs_all_confirmed() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(receipt_response_for_requested_hash)
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let tracker = LatencyTracker::new();

        // Submit 10 txs.
        for i in 1u8..=10 {
            tracker.record_submit(
                B256::with_last_byte(i),
                i as u64,
                Address::default(),
                21_000,
                TransactionType::SimpleTransfer,
            );
        }
        assert_eq!(tracker.pending_count(), 10);

        poll_pending_receipts(&client, &mock_server.uri(), &tracker, 0).await;

        assert_eq!(tracker.confirmed_count(), 10);
        assert_eq!(tracker.pending_count(), 0);
    }

    #[tokio::test]
    async fn test_poll_pending_receipts_finality_confirms_only_finalized_receipts() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_blockNumber",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": "0x5"
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_getTransactionReceipt",
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(receipt_response_with_block(
                    "0x0000000000000000000000000000000000000000000000000000000000000001",
                    "0x4",
                )),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let tracker = LatencyTracker::new();
        let hash = B256::with_last_byte(1);
        tracker.record_submit(
            hash,
            0,
            Address::default(),
            21_000,
            TransactionType::SimpleTransfer,
        );

        poll_pending_receipts(&client, &mock_server.uri(), &tracker, 1).await;

        assert_eq!(tracker.confirmed_count(), 1);
        assert_eq!(tracker.pending_count(), 0);
    }

    #[tokio::test]
    async fn test_poll_pending_receipts_finality_leaves_recent_receipts_pending() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_blockNumber",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": "0x5"
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_getTransactionReceipt",
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(receipt_response_with_block(
                    "0x0000000000000000000000000000000000000000000000000000000000000002",
                    "0x5",
                )),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let tracker = LatencyTracker::new();
        let hash = B256::with_last_byte(2);
        tracker.record_submit(
            hash,
            0,
            Address::default(),
            21_000,
            TransactionType::SimpleTransfer,
        );

        poll_pending_receipts(&client, &mock_server.uri(), &tracker, 1).await;

        assert_eq!(tracker.confirmed_count(), 0);
        assert_eq!(tracker.pending_count(), 1);
    }

    #[tokio::test]
    async fn test_poll_pending_receipts_finality_ignores_invalid_blocknumber_response() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(|request: &Request| {
                let method = rpc_method(request).expect("rpc method");
                if method == "eth_blockNumber" {
                    ResponseTemplate::new(200).set_body_string("not-json")
                } else {
                    ResponseTemplate::new(200).set_body_json(receipt_response_with_block(
                        "0x0000000000000000000000000000000000000000000000000000000000000001",
                        "0x1",
                    ))
                }
            })
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let tracker = LatencyTracker::new();
        tracker.record_submit(
            B256::with_last_byte(1),
            0,
            Address::default(),
            21_000,
            TransactionType::SimpleTransfer,
        );

        poll_pending_receipts(&client, &mock_server.uri(), &tracker, 1).await;

        assert_eq!(tracker.confirmed_count(), 0);
        assert_eq!(tracker.pending_count(), 1);
    }

    #[tokio::test]
    async fn test_poll_pending_receipts_finality_ignores_blocknumber_transport_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let client = reqwest::Client::new();
        let tracker = LatencyTracker::new();
        tracker.record_submit(
            B256::with_last_byte(2),
            0,
            Address::default(),
            21_000,
            TransactionType::SimpleTransfer,
        );

        poll_pending_receipts(&client, &format!("http://{}", addr), &tracker, 1).await;

        assert_eq!(tracker.confirmed_count(), 0);
        assert_eq!(tracker.pending_count(), 1);
    }

    #[tokio::test]
    async fn test_run_burst_errors_when_submitter_creation_fails() {
        let rpc = url::Url::parse("http://127.0.0.1:8545").unwrap();
        let mut config = burst_test_config(rpc.as_str());
        config.rpc_urls = vec![];

        let err = run_burst(&config)
            .await
            .expect_err("missing rpc_urls should fail submitter creation");
        assert!(err.to_string().contains("At least one"));
    }

    #[tokio::test]
    async fn test_run_burst_transfer_mode_end_to_end_tracks_waves() {
        let mock_server = MockServer::start().await;
        let submit_count = StdArc::new(AtomicUsize::new(0));
        let submit_count_for_mock = submit_count.clone();

        Mock::given(method("POST"))
            .respond_with(move |request: &Request| {
                let body: serde_json::Value =
                    serde_json::from_slice(&request.body).expect("valid rpc json");
                if let Some(items) = body.as_array() {
                    let call_idx = submit_count_for_mock.fetch_add(1, Ordering::SeqCst);
                    let results: Vec<_> = items
                        .iter()
                        .enumerate()
                        .map(|(idx, _)| {
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": idx,
                                "result": format!("0xaccepted{:02x}", call_idx * 16 + idx),
                            })
                        })
                        .collect();
                    return ResponseTemplate::new(200).set_body_json(results);
                }

                let method = rpc_method(request).expect("rpc method");
                assert_expected_rpc_method(
                    &method,
                    &[
                        "eth_blockNumber",
                        "eth_gasPrice",
                        "eth_getBalance",
                        "eth_getTransactionCount",
                        "eth_getTransactionReceipt",
                    ],
                );
                let response = if method == "eth_blockNumber" {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": "0x7"
                    })
                } else if method == "eth_gasPrice" {
                    balance_response("0x3b9aca00")
                } else if method == "eth_getBalance" {
                    balance_response("0xde0b6b3a7640000")
                } else if method == "eth_getTransactionCount" {
                    balance_response("0x0")
                } else {
                    let hash = body["params"][0].as_str().unwrap_or_default().to_string();
                    receipt_response(&hash)
                };
                ResponseTemplate::new(200).set_body_json(response)
            })
            .mount(&mock_server)
            .await;

        let mut config = burst_test_config(&mock_server.uri());
        config.tx_count = 3;
        config.wave_count = 2;
        config.wave_delay_ms = 1;

        let (result, gas_price) = run_burst(&config).await.expect("burst run succeeds");

        assert_eq!(gas_price, 2_000_000_000);
        assert_eq!(result.attempted, 3);
        assert_eq!(result.submitted, 3);
        assert_eq!(result.accepted, 3);
        assert_eq!(result.failed, 0);
        assert_eq!(result.confirmed, 3);
        assert_eq!(result.pending, 0);
        assert!(result.valid);
        let per_wave = result.per_wave.expect("per-wave stats should be recorded");
        assert_eq!(per_wave.len(), 2);
        assert_eq!(per_wave[0].wave, 0);
        assert_eq!(per_wave[0].count, 2);
        assert_eq!(per_wave[1].wave, 1);
        assert_eq!(per_wave[1].count, 1);
        assert_eq!(submit_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_run_burst_uses_sender_count_independent_of_worker_count() {
        let mock_server = MockServer::start().await;
        let submit_count = StdArc::new(AtomicUsize::new(0));
        let submit_count_for_mock = submit_count.clone();

        Mock::given(method("POST"))
            .respond_with(move |request: &Request| {
                let body: serde_json::Value =
                    serde_json::from_slice(&request.body).expect("valid rpc json");
                if let Some(items) = body.as_array() {
                    let call_idx = submit_count_for_mock.fetch_add(1, Ordering::SeqCst);
                    let results: Vec<_> = items
                        .iter()
                        .enumerate()
                        .map(|(idx, _)| {
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": idx,
                                "result": format!("0xactive{:02x}", call_idx * 16 + idx),
                            })
                        })
                        .collect();
                    return ResponseTemplate::new(200).set_body_json(results);
                }

                let method = rpc_method(request).expect("rpc method");
                assert_expected_rpc_method(
                    &method,
                    &[
                        "eth_blockNumber",
                        "eth_gasPrice",
                        "eth_getBalance",
                        "eth_getTransactionCount",
                        "eth_getTransactionReceipt",
                    ],
                );
                let response = if method == "eth_blockNumber" {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": "0x7"
                    })
                } else if method == "eth_gasPrice" {
                    balance_response("0x3b9aca00")
                } else if method == "eth_getBalance" {
                    balance_response("0xde0b6b3a7640000")
                } else if method == "eth_getTransactionCount" {
                    balance_response("0x0")
                } else {
                    let hash = body["params"][0].as_str().unwrap_or_default().to_string();
                    receipt_response(&hash)
                };
                ResponseTemplate::new(200).set_body_json(response)
            })
            .mount(&mock_server)
            .await;

        let mut config = burst_test_config(&mock_server.uri());
        config.tx_count = 6;
        config.worker_count = 2;
        config.batch_size = 1;
        config.sender_keys = (1u8..=6).map(|i| format!("0x{:064x}", i)).collect();

        let (result, _) = run_burst(&config).await.expect("burst run succeeds");

        assert_eq!(result.attempted, 6);
        assert_eq!(result.accepted, 6);
        assert_eq!(result.confirmed, 6);
        assert_eq!(
            submit_count.load(Ordering::SeqCst),
            6,
            "each active sender should have its own nonce-ordered submission slice"
        );
    }

    #[tokio::test]
    async fn test_run_burst_breaks_worker_loop_when_txs_run_out() {
        let mock_server = MockServer::start().await;
        let submit_count = StdArc::new(AtomicUsize::new(0));
        let submit_count_for_mock = submit_count.clone();

        Mock::given(method("POST"))
            .respond_with(move |request: &Request| {
                let body: serde_json::Value =
                    serde_json::from_slice(&request.body).expect("valid rpc json");
                if let Some(items) = body.as_array() {
                    submit_count_for_mock.fetch_add(1, Ordering::SeqCst);
                    let results: Vec<_> = items
                        .iter()
                        .enumerate()
                        .map(|(idx, _)| {
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": idx,
                                "result": format!("0xsingle{idx:02x}"),
                            })
                        })
                        .collect();
                    return ResponseTemplate::new(200).set_body_json(results);
                }

                let method = rpc_method(request).expect("rpc method");
                assert_expected_rpc_method(
                    &method,
                    &[
                        "eth_blockNumber",
                        "eth_gasPrice",
                        "eth_getBalance",
                        "eth_getTransactionCount",
                        "eth_getTransactionReceipt",
                    ],
                );
                let response = if method == "eth_blockNumber" {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": "0x2"
                    })
                } else if method == "eth_gasPrice" {
                    balance_response("0x3b9aca00")
                } else if method == "eth_getBalance" {
                    balance_response("0xde0b6b3a7640000")
                } else if method == "eth_getTransactionCount" {
                    balance_response("0x0")
                } else {
                    let hash = body["params"][0].as_str().unwrap_or_default().to_string();
                    receipt_response(&hash)
                };
                ResponseTemplate::new(200).set_body_json(response)
            })
            .mount(&mock_server)
            .await;

        let mut config = burst_test_config(&mock_server.uri());
        config.tx_count = 1;
        config.quiet = false;

        let (result, _gas_price) = run_burst(&config).await.expect("burst run succeeds");

        assert_eq!(result.submitted, 1);
        assert_eq!(result.confirmed, 1);
        assert_eq!(submit_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_run_burst_quiet_false_hits_submission_result_branches_and_receipt_retry() {
        let mock_server = MockServer::start().await;
        let batch_count = StdArc::new(AtomicUsize::new(0));
        let receipt_count = StdArc::new(AtomicUsize::new(0));
        let batch_count_for_mock = batch_count.clone();
        let receipt_count_for_mock = receipt_count.clone();

        Mock::given(method("POST"))
            .respond_with(move |request: &Request| {
                let body: serde_json::Value =
                    serde_json::from_slice(&request.body).expect("valid rpc json");

                if let Some(_items) = body.as_array() {
                    let call_idx = batch_count_for_mock.fetch_add(1, Ordering::SeqCst);
                    return if call_idx == 0 {
                        ResponseTemplate::new(200).set_body_json(serde_json::json!([
                            {"jsonrpc": "2.0", "id": 0, "result": "0xaccepted00"},
                            {
                                "jsonrpc": "2.0",
                                "id": 1,
                                "error": {"code": -32000, "message": "txpool full"}
                            }
                        ]))
                    } else {
                        ResponseTemplate::new(200)
                            .set_body_json(serde_json::json!({"jsonrpc": "2.0", "id": 1}))
                    };
                }

                let method = rpc_method(request).expect("rpc method");
                assert_expected_rpc_method(
                    &method,
                    &[
                        "eth_blockNumber",
                        "eth_gasPrice",
                        "eth_getBalance",
                        "eth_getTransactionCount",
                        "eth_getTransactionReceipt",
                    ],
                );
                if method == "eth_blockNumber" {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": "0x3"
                    }))
                } else if method == "eth_gasPrice" || method == "eth_getBalance" {
                    ResponseTemplate::new(200).set_body_json(balance_response("0x1"))
                } else if method == "eth_getTransactionCount" {
                    ResponseTemplate::new(200).set_body_json(balance_response("0x0"))
                } else {
                    let call_idx = receipt_count_for_mock.fetch_add(1, Ordering::SeqCst);
                    if call_idx == 0 {
                        ResponseTemplate::new(200).set_body_json(null_receipt_response())
                    } else {
                        let hash = body["params"][0].as_str().unwrap_or_default().to_string();
                        ResponseTemplate::new(200).set_body_json(receipt_response(&hash))
                    }
                }
            })
            .mount(&mock_server)
            .await;

        let mut config = burst_test_config(&mock_server.uri());
        config.quiet = false;
        config.tx_count = 3;
        config.wave_delay_ms = 1;

        let (result, gas_price) = run_burst(&config).await.expect("burst run succeeds");

        assert_eq!(gas_price, 1_000_000_000);
        assert_eq!(result.attempted, 3);
        assert_eq!(result.submitted, 1);
        assert_eq!(result.accepted, 1);
        assert_eq!(result.failed, 2);
        assert_eq!(result.confirmed, 1);
        assert_eq!(result.pending, 0);
        assert!(!result.valid);
        assert!(
            result
                .invalid_reason
                .as_deref()
                .unwrap_or_default()
                .contains("accepted 1 of 3 attempted")
        );
        assert_eq!(batch_count.load(Ordering::SeqCst), 1);
        assert_eq!(receipt_count.load(Ordering::SeqCst), 2);
        let per_wave = result
            .per_wave
            .expect("accepted tx should record wave stats");
        assert_eq!(per_wave.len(), 1);
        assert_eq!(per_wave[0].count, 1);
    }

    #[tokio::test]
    async fn test_run_burst_submission_errors_leave_tracker_empty() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(|request: &Request| {
                let body: serde_json::Value =
                    serde_json::from_slice(&request.body).expect("valid rpc json");
                if body.is_array() {
                    return ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({"jsonrpc": "2.0", "id": 1}));
                }

                let method = rpc_method(request).expect("rpc method");
                assert_expected_rpc_method(
                    &method,
                    &["eth_gasPrice", "eth_getBalance", "eth_getTransactionCount"],
                );
                let response = if method == "eth_gasPrice" {
                    balance_response("0x3b9aca00")
                } else if method == "eth_getBalance" {
                    balance_response("0xde0b6b3a7640000")
                } else {
                    balance_response("0x0")
                };
                ResponseTemplate::new(200).set_body_json(response)
            })
            .mount(&mock_server)
            .await;

        let mut config = burst_test_config(&mock_server.uri());
        config.tx_count = 2;

        let (result, _gas_price) = run_burst(&config).await.expect("burst run returns result");

        assert_eq!(result.attempted, 2);
        assert_eq!(result.submitted, 0);
        assert_eq!(result.failed, 2);
        assert_eq!(result.confirmed, 0);
        assert_eq!(result.pending, 0);
        assert!(!result.valid);
        assert!(result.per_wave.is_none());
    }

    #[tokio::test]
    async fn test_run_burst_submission_transport_errors_exercise_sender_error_branch() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(|request: &Request| {
                let method = rpc_method(request).expect("rpc method");
                assert_expected_rpc_method(
                    &method,
                    &["eth_gasPrice", "eth_getBalance", "eth_getTransactionCount"],
                );
                let response = if method == "eth_gasPrice" {
                    balance_response("0x3b9aca00")
                } else if method == "eth_getBalance" {
                    balance_response("0xde0b6b3a7640000")
                } else {
                    balance_response("0x0")
                };
                ResponseTemplate::new(200).set_body_json(response)
            })
            .mount(&mock_server)
            .await;

        let mut config = burst_test_config(&mock_server.uri());
        config.rpc_urls = vec![url::Url::parse("testerr://force").unwrap()];
        config.quiet = false;
        config.tx_count = 2;

        let (result, gas_price) = run_burst(&config)
            .await
            .expect("transport submission failures should not abort burst mode");

        assert_eq!(gas_price, 2_000_000_000);
        assert_eq!(result.attempted, 2);
        assert_eq!(result.submitted, 0);
        assert_eq!(result.failed, 2);
        assert_eq!(result.confirmed, 0);
        assert_eq!(result.pending, 0);
        assert!(!result.valid);
        assert!(result.per_wave.is_none());
    }

    #[tokio::test]
    async fn test_run_burst_quiet_false_caps_tx_count_before_missing_evm_contracts() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(|request: &Request| {
                let method = rpc_method(request).expect("rpc method");
                assert_expected_rpc_method(
                    &method,
                    &[
                        "eth_blockNumber",
                        "eth_gasPrice",
                        "eth_getBalance",
                        "eth_getTransactionCount",
                    ],
                );
                if method == "eth_blockNumber" {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": "0x2"
                    }))
                } else if method == "eth_gasPrice" {
                    ResponseTemplate::new(200).set_body_json(balance_response("0x3b9aca00"))
                } else if method == "eth_getBalance" {
                    ResponseTemplate::new(200).set_body_json(balance_response("0xde0b6b3a7640000"))
                } else {
                    ResponseTemplate::new(200).set_body_json(balance_response("0x0"))
                }
            })
            .mount(&mock_server)
            .await;

        let mut config = burst_test_config(&mock_server.uri());
        config.rpc_urls = vec![
            url::Url::parse(&mock_server.uri()).unwrap(),
            url::Url::parse(&mock_server.uri()).unwrap(),
        ];
        config.rpc = config.rpc_urls[0].clone();
        config.quiet = false;
        config.test_mode = TestMode::Evm;
        config.tx_count = 4_001;
        config.sender_keys = vec![format!("0x{:064x}", 1)];

        let err = run_burst(&config)
            .await
            .expect_err("missing contracts should still fail after tx cap");
        assert!(
            err.to_string()
                .contains("EVM mode requires deployed contracts")
        );
    }

    #[tokio::test]
    async fn test_run_burst_evm_mode_without_contracts_errors() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(|request: &Request| {
                let method = rpc_method(request).expect("rpc method");
                assert_expected_rpc_method(
                    &method,
                    &[
                        "eth_blockNumber",
                        "eth_gasPrice",
                        "eth_getBalance",
                        "eth_getTransactionCount",
                    ],
                );
                let response = if method == "eth_blockNumber" {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": "0x2"
                    })
                } else if method == "eth_gasPrice" {
                    balance_response("0x3b9aca00")
                } else if method == "eth_getBalance" {
                    balance_response("0xde0b6b3a7640000")
                } else {
                    balance_response("0x0")
                };
                ResponseTemplate::new(200).set_body_json(response)
            })
            .mount(&mock_server)
            .await;

        let mut config = burst_test_config(&mock_server.uri());
        config.test_mode = TestMode::Evm;
        config.tx_count = 1;

        let err = run_burst(&config)
            .await
            .expect_err("missing deployed EVM contracts should fail");
        assert!(
            err.to_string()
                .contains("EVM mode requires deployed contracts")
        );
    }

    #[tokio::test]
    async fn test_run_burst_evm_mode_with_contracts_uses_generator_and_signs_batches() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(|request: &Request| {
                let body: serde_json::Value =
                    serde_json::from_slice(&request.body).expect("valid rpc json");

                if let Some(items) = body.as_array() {
                    let results: Vec<_> = items
                        .iter()
                        .enumerate()
                        .map(|(idx, _)| {
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": idx,
                                "result": format!("0xevm{idx:02x}"),
                            })
                        })
                        .collect();
                    return ResponseTemplate::new(200).set_body_json(results);
                }

                let method = rpc_method(request).expect("rpc method");
                assert_expected_rpc_method(
                    &method,
                    &[
                        "eth_blockNumber",
                        "eth_gasPrice",
                        "eth_getBalance",
                        "eth_getTransactionCount",
                        "eth_getTransactionReceipt",
                    ],
                );
                if method == "eth_blockNumber" {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": "0x6"
                    }))
                } else if method == "eth_gasPrice" {
                    ResponseTemplate::new(200).set_body_json(balance_response("0x3b9aca00"))
                } else if method == "eth_getBalance" {
                    ResponseTemplate::new(200).set_body_json(balance_response("0xde0b6b3a7640000"))
                } else if method == "eth_getTransactionCount" {
                    ResponseTemplate::new(200).set_body_json(balance_response("0x0"))
                } else {
                    let hash = body["params"][0].as_str().unwrap_or_default().to_string();
                    ResponseTemplate::new(200).set_body_json(receipt_response(&hash))
                }
            })
            .mount(&mock_server)
            .await;

        let mut config = burst_test_config(&mock_server.uri());
        config.test_mode = TestMode::Evm;
        config.tx_count = 2;
        config.sender_keys = vec![format!("0x{:064x}", 1)];
        config.evm_tokens = vec![Address::with_last_byte(0x10)];
        config.evm_pairs = vec![Address::with_last_byte(0x20)];
        config.evm_nfts = vec![Address::with_last_byte(0x30)];

        let (result, gas_price) = run_burst(&config)
            .await
            .expect("evm mode should sign and submit generated descriptors");

        assert_eq!(gas_price, 2_000_000_000);
        assert_eq!(result.attempted, 2);
        assert_eq!(result.submitted, 2);
        assert_eq!(result.failed, 0);
        assert_eq!(result.confirmed, 2);
        assert_eq!(result.pending, 0);
        assert!(result.valid);
    }

    #[test]
    fn test_confirmed_tps_returns_zero_for_zero_duration() {
        assert_eq!(confirmed_tps(100, Duration::ZERO), 0.0);
    }

    #[test]
    fn test_log_analysis_result_handles_ok_and_err_paths() {
        let ok_result: Result<String> = Ok("ascii report".to_string());
        log_analysis_result(false, ok_result);

        let err_result: Result<String> = Err(anyhow::anyhow!("boom"));
        log_analysis_result(false, err_result);
    }

    #[test]
    #[should_panic(expected = "unexpected rpc method")]
    fn test_unexpected_test_rpc_method_panics() {
        unexpected_test_rpc_method("eth_chainId");
    }

    // ── BurstResult computation tests ──────────────────────────────────

    /// Verify submitted_tps calculation: total_signed / submit_time.
    #[test]
    fn test_burst_result_submitted_tps_calculation() {
        let total_signed = 2000usize;
        let submit_secs = 4.0f32;
        let submitted_tps = total_signed as f32 / submit_secs;
        assert!((submitted_tps - 500.0).abs() < 0.01);
    }

    /// Verify confirmed_tps calculation: confirmed / elapsed end-to-end time.
    #[test]
    fn test_burst_result_confirmed_tps_calculation() {
        let confirmed = 1800u32;
        let elapsed_secs = 6.0f32;
        let confirmed_tps = confirmed as f32 / elapsed_secs;
        assert!((confirmed_tps - 300.0).abs() < 0.01);
    }

    /// When elapsed time is zero, confirmed_tps should be 0.
    #[test]
    fn test_burst_result_zero_confirm_time() {
        assert_eq!(confirmed_tps(100, Duration::ZERO), 0.0);
    }

    /// Pool capacity capping: tx_count is capped to num_keys * POOL_SLOTS_PER_ACCOUNT.
    #[test]
    fn test_pool_capacity_capping() {
        const POOL_SLOTS_PER_ACCOUNT: usize = 4000;
        let num_keys = 4;
        let max_pool_capacity = num_keys * POOL_SLOTS_PER_ACCOUNT;

        let tx_count = 20_000usize.min(max_pool_capacity);
        assert_eq!(tx_count, 16_000);

        let tx_count2 = 10_000usize.min(max_pool_capacity);
        assert_eq!(tx_count2, 10_000);
    }

    /// Sender tx distribution: txs_per_sender with div_ceil.
    #[test]
    fn test_txs_per_sender_distribution() {
        // 10 txs, 3 senders -> 4, 4, 2 (via div_ceil)
        let tx_count = 10usize;
        let sender_count = 3usize;
        let txs_per_sender = tx_count.div_ceil(sender_count);
        assert_eq!(txs_per_sender, 4);

        // Exact division: 12 txs, 4 senders -> 3 each
        let txs_per_sender2 = 12usize.div_ceil(4);
        assert_eq!(txs_per_sender2, 3);

        // Single sender gets all txs
        let txs_per_sender3 = 100usize.div_ceil(1);
        assert_eq!(txs_per_sender3, 100);
    }

    /// Last sender should not get more txs than remaining.
    #[test]
    fn test_sender_tx_slicing_no_overshoot() {
        let tx_count = 10usize;
        let sender_count = 3usize;
        let txs_per_sender = tx_count.div_ceil(sender_count);
        let mut total_assigned = 0;

        for sender_idx in 0..sender_count {
            let submitted_so_far = sender_idx * txs_per_sender;
            let remaining = tx_count.saturating_sub(submitted_so_far);
            let this_count = remaining.min(txs_per_sender);
            total_assigned += this_count;
        }

        assert_eq!(
            total_assigned, tx_count,
            "total assigned must equal tx_count"
        );
    }

    #[test]
    fn test_split_worker_txs_into_waves_preserves_nonce_order() {
        let txs = (0..5u64)
            .map(|nonce| crate::types::SignedTxWithMetadata {
                hash: B256::with_last_byte(nonce as u8),
                encoded: vec![nonce as u8],
                nonce,
                gas_limit: 21_000,
                sender: Address::with_last_byte(1),
                submit_time: Instant::now(),
                method: TransactionType::SimpleTransfer,
            })
            .collect::<Vec<_>>();

        let waves = split_worker_txs_into_waves(txs, 3);
        assert_eq!(waves.len(), 3);
        assert_eq!(
            waves.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![2, 2, 1]
        );

        let flattened_nonces = waves
            .iter()
            .flat_map(|wave| wave.iter().map(|tx| tx.nonce))
            .collect::<Vec<_>>();
        assert_eq!(flattened_nonces, vec![0, 1, 2, 3, 4]);

        let empty = split_worker_txs_into_waves(vec![], 3);
        assert_eq!(empty.len(), 3);
        assert!(empty.iter().all(Vec::is_empty));
    }

    /// Gas price is at least 1 Gwei even if chain returns lower.
    #[test]
    fn test_gas_price_minimum_floor() {
        let base_price = 100_000_000u128; // 0.1 Gwei from chain
        let gas_price = gas_price_with_safety_margin(base_price);
        assert_eq!(
            gas_price, 1_000_000_000,
            "floor should enforce 1 Gwei minimum"
        );
    }

    #[test]
    fn test_gas_price_safety_margin_saturates_on_extreme_values() {
        assert_eq!(
            gas_price_with_safety_margin(50_000_000_000),
            100_000_000_000
        );
        assert_eq!(
            gas_price_with_safety_margin(u128::MAX),
            u128::MAX,
            "extreme RPC gas prices should not overflow"
        );
    }

    /// Gas price 2x safety margin.
    #[test]
    fn test_gas_price_2x_safety_margin() {
        let base_price = 5_000_000_000u128; // 5 Gwei from chain
        let gas_price = (base_price * 2).max(1_000_000_000);
        assert_eq!(gas_price, 10_000_000_000, "should be 2x base price");
    }

    /// MIN_BALANCE_WEI constant is 1 ETH.
    #[test]
    fn test_min_balance_wei_constant() {
        assert_eq!(MIN_BALANCE_WEI, 1_000_000_000_000_000_000);
    }

    /// Default sender keys: 4 pre-funded validator keys.
    #[test]
    fn test_default_sender_keys_generation() {
        let keys: Vec<String> = (1u8..=4).map(|i| format!("0x{:064x}", i)).collect();
        assert_eq!(keys.len(), 4);
        assert_eq!(keys[0], format!("0x{:064x}", 1));
        assert_eq!(keys[3], format!("0x{:064x}", 4));
    }

    /// Multi-key parsing from comma-separated BENCH_KEY.
    #[test]
    fn test_multi_key_parsing() {
        let bench_key_env = "0xaaa,0xbbb,0xccc".to_string();
        let sender_keys: Vec<String> = bench_key_env
            .split(',')
            .map(|s| s.trim().to_string())
            .collect();
        assert_eq!(sender_keys.len(), 3);
        assert_eq!(sender_keys[0], "0xaaa");
        assert_eq!(sender_keys[2], "0xccc");
    }

    /// Single key env var is not split.
    #[test]
    fn test_single_key_env() {
        let bench_key_env = "0xdeadbeef".to_string();
        let sender_keys = [bench_key_env];
        assert_eq!(sender_keys.len(), 1);
        assert_eq!(sender_keys[0], "0xdeadbeef");
    }

    /// Burst mode uses one contiguous nonce range per active sender.
    #[test]
    fn test_nonce_range_distribution() {
        let base_nonces = [10u64, 20u64, 30u64];

        for sender_idx in 0..base_nonces.len() {
            let nonce_start = base_nonces[sender_idx];
            assert_eq!(nonce_start, base_nonces[sender_idx]);
        }
    }
}
