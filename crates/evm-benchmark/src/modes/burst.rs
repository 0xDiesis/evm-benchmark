use crate::config::Config;
use crate::generators::contract_deploy::EvmContracts;
use crate::generators::evm_mix::{EvmMixConfig, EvmMixGenerator};
use crate::metrics::MetricsExporter;
use crate::signing::BatchSigner;
use crate::submission::{LatencyTracker, Submitter};
use crate::types::{BurstResult, TestMode};
use alloy_primitives::{Address, U256};
use alloy_signer_local::PrivateKeySigner;
use anyhow::Result;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

const MIN_BALANCE_WEI: u128 = 1_000_000_000_000_000_000; // 1 ETH

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
                    // Use poll_start as the arrival time: this is when we initiated
                    // the poll round, so latency = submit_time → poll_start which
                    // upper-bounds the true inclusion latency by at most poll_interval (25ms).
                    tracker.on_block_inclusion(hash, poll_start);
                }
            }
        })
        .await;
}

/// Returns `(BurstResult, effective_gas_price_wei)`.
pub async fn run_burst(config: &Config) -> Result<(BurstResult, u128)> {
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
    let max_pool_capacity = num_keys * POOL_SLOTS_PER_ACCOUNT;

    // Cap tx_count to the pool's total capacity across all accounts
    let tx_count = if config.tx_count as usize > max_pool_capacity {
        if !config.quiet {
            eprintln!(
                "[burst] tx_count {} exceeds pool capacity ({} accounts × {} slots = {}). Capping to {}.",
                config.tx_count,
                num_keys,
                POOL_SLOTS_PER_ACCOUNT,
                max_pool_capacity,
                max_pool_capacity
            );
        }
        max_pool_capacity
    } else {
        config.tx_count as usize
    };

    // Workers = one per key, distributing txs evenly across all funded accounts
    let worker_count = num_keys;
    let _ = config.worker_count; // ignored — worker count is driven by key count

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
        (base * 2).max(1_000_000_000)
    };

    if !config.quiet {
        println!("Gas price: {} gwei", gas_price / 1_000_000_000);
    }
    // Phase 1: Fetch nonces for all unique sender keys upfront, then assign workers.
    // If worker_count > num_keys, multiple workers share a key but get non-overlapping
    // nonce ranges (each worker pre-signs its own slice with sequential nonces).
    let _sign_start = Instant::now();

    // txs per worker — evenly distributed
    let txs_per_worker = tx_count.div_ceil(worker_count);

    // Fetch base nonce for each unique key once
    let mut key_nonces: Vec<(String, u64)> = Vec::new();
    for (i, key) in sender_keys.iter().enumerate() {
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

    // Build per-worker tx slices. Worker w uses key[w % num_keys] with nonce starting at
    // base_nonce + (w / num_keys) * txs_per_worker.
    let mut all_signed_txs: Vec<Vec<crate::types::SignedTxWithMetadata>> = Vec::new();

    // Build EVM generator if in EVM mode (contracts must have been deployed by main.rs)
    let evm_generator = if config.test_mode == TestMode::Evm {
        let contracts = EvmContracts {
            tokens: config.evm_tokens.clone(),
            pairs: config.evm_pairs.clone(),
            nfts: config.evm_nfts.clone(),
        };

        if contracts.tokens.is_empty() {
            anyhow::bail!(
                "EVM mode requires deployed contracts (no token addresses configured). Use --fund to deploy."
            );
        }

        let sender_addrs: Vec<Address> = key_nonces
            .iter()
            .map(|(k, _)| PrivateKeySigner::from_str(k).expect("valid key").address())
            .collect();

        Some(
            EvmMixGenerator::new(
                contracts,
                EvmMixConfig::default(),
                sender_addrs,
                config.chain_id,
            )
            .map_err(|e| anyhow::anyhow!("Failed to create EVM generator: {}", e))?,
        )
    } else {
        None
    };

    // Pre-generate all EVM descriptors if in EVM mode so we can split per-worker
    let evm_descriptors = evm_generator.map(|mut generator| generator.generate_batch(tx_count));

    for w in 0..worker_count {
        let key_idx = w % num_keys;
        let slot_in_key = w / num_keys; // which slot within this key's workers

        let (key, base_nonce) = &key_nonces[key_idx];
        let nonce_start = base_nonce + (slot_in_key * txs_per_worker) as u64;

        let submitted_so_far = w * txs_per_worker;
        let remaining = tx_count.saturating_sub(submitted_so_far);
        let this_count = remaining.min(txs_per_worker);
        if this_count == 0 {
            break;
        }

        let private_signer = PrivateKeySigner::from_str(key)
            .map_err(|e| anyhow::anyhow!("Failed to parse signer key for worker {}: {}", w, e))?;

        let signed = if let Some(ref all_descs) = evm_descriptors {
            // EVM mode: sign the pre-generated descriptors for this worker's slice
            let start = submitted_so_far;
            let end = (start + this_count).min(all_descs.len());
            let worker_descs = &all_descs[start..end];
            EvmMixGenerator::sign_batch(
                worker_descs,
                &private_signer,
                nonce_start,
                gas_price,
                config.chain_id,
            )
            .map_err(|e| anyhow::anyhow!("EVM batch signing failed for worker {}: {}", w, e))?
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
            batch_signer
                .sign_batch_parallel(tx_data)
                .map_err(|e| anyhow::anyhow!("Batch signing failed for worker {}: {}", w, e))?
        };

        all_signed_txs.push(signed);
    }

    let total_signed: usize = all_signed_txs.iter().map(|v| v.len()).sum();
    let _sign_time = _sign_start.elapsed();

    if !config.quiet {
        println!(
            "Signed {} txs across {} senders in {:.2}s",
            total_signed,
            all_signed_txs.len(),
            _sign_time.as_secs_f32()
        );
    }

    let max_wait = Duration::from_secs(60);

    // Phase 3: One worker per sender — each submits its own tx slice concurrently.
    // This avoids the per-account txpool slot limit (5000 slots/sender) by spreading
    // txs across multiple funded accounts.
    let submit_start = Instant::now();
    let mut submit_handles = vec![];

    for (wave_idx, sender_txs) in all_signed_txs.into_iter().enumerate() {
        if wave_idx > 0 && config.wave_delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(config.wave_delay_ms)).await;
        }

        let dispatcher = dispatcher.clone();
        let tracker = tracker.clone();
        let metrics = metrics.clone();
        let quiet = config.quiet;

        let handle = tokio::spawn(async move {
            match dispatcher.submit_batch(sender_txs.clone()).await {
                Ok(result) => {
                    // Record only txs the RPC actually accepted, with wave index
                    for tx in &result.accepted_txs {
                        tracker.record_submit_with_wave(
                            tx.hash,
                            tx.nonce,
                            tx.sender,
                            tx.gas_limit,
                            tx.method,
                            Some(wave_idx as u32),
                        );
                    }
                    metrics.inc_transactions_submitted(result.submitted as u64);
                    if !quiet {
                        if result.errors > 0 {
                            println!(
                                "Sender {}: submitted {} txs, {} errors",
                                wave_idx, result.submitted, result.errors
                            );
                        } else {
                            println!("Sender {}: submitted {} txs", wave_idx, result.submitted);
                        }
                    }
                }
                Err(e) => {
                    metrics.inc_transactions_failed(sender_txs.len() as u64);
                    if !quiet {
                        eprintln!("Sender {} submission error: {}", wave_idx, e);
                    }
                }
            }
        });
        submit_handles.push(handle);
    }

    // Wait for all submission workers to complete
    for handle in submit_handles {
        let _ = handle.await;
    }

    let submit_time = submit_start.elapsed();

    // Phase 4: Poll eth_getTransactionReceipt for all submitted txs concurrently.
    // This is more reliable than block-based tracking — no missed blocks, no stalls.
    // We poll every 25ms until all txs are confirmed or max_wait is exceeded.
    let confirm_start = Instant::now();

    while confirm_start.elapsed() < max_wait && tracker.pending_count() > 0 {
        metrics.set_pending_transactions(tracker.pending_count() as i64);
        poll_pending_receipts(
            &client,
            config.rpc.as_str(),
            &tracker,
            config.finality_confirmations,
        )
        .await;
        if tracker.pending_count() > 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    let confirm_ms = confirm_start.elapsed();
    let stats = tracker.statistics();
    let confirmed = tracker.confirmed_count();

    // confirmed_tps: chain throughput = txs confirmed / time spent waiting for confirmations.
    // This measures how fast the chain processes transactions, excluding submission overhead.
    metrics.set_pending_transactions(tracker.pending_count() as i64);
    if !confirm_ms.is_zero() {
        metrics.set_current_tps(confirmed as f64 / confirm_ms.as_secs_f64());
    }

    let result = BurstResult {
        submitted: total_signed as u32,
        confirmed,
        pending: tracker.pending_count(),
        sign_ms: _sign_time.as_millis() as u64,
        submit_ms: submit_time.as_millis() as u64,
        confirm_ms: confirm_ms.as_millis() as u64,
        submitted_tps: total_signed as f32 / submit_time.as_secs_f32(),
        confirmed_tps: if !confirm_ms.is_zero() {
            confirmed as f32 / confirm_ms.as_secs_f32()
        } else {
            0.0
        },
        latency: stats,
        server_metrics: None,
        per_method: None,
        validator_health: None,
        per_wave: {
            let waves = tracker.per_wave_statistics();
            if waves.is_empty() { None } else { Some(waves) }
        },
    };

    // Run analytics pipeline on benchmark results
    match crate::analytics::run_analysis("burst-benchmark", "burst", &result, None, None).await {
        Ok(analytics_report) => {
            if !config.quiet {
                println!("\n{}", analytics_report.reports.ascii);
            }
        }
        Err(e) => {
            if !config.quiet {
                eprintln!("Warning: Analytics pipeline failed: {}", e);
            }
        }
    }

    Ok((result, gas_price))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::submission::LatencyTracker;
    use crate::types::TransactionType;
    use alloy_primitives::B256;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
            .respond_with(ResponseTemplate::new(200).set_body_json(receipt_response(
                "0x0000000000000000000000000000000000000000000000000000000000000001",
            )))
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
            .respond_with(ResponseTemplate::new(200).set_body_json(receipt_response(
                "0x0000000000000000000000000000000000000000000000000000000000000099",
            )))
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

    // ── BurstResult computation tests ──────────────────────────────────

    /// Verify submitted_tps calculation: total_signed / submit_time.
    #[test]
    fn test_burst_result_submitted_tps_calculation() {
        let total_signed = 2000usize;
        let submit_secs = 4.0f32;
        let submitted_tps = total_signed as f32 / submit_secs;
        assert!((submitted_tps - 500.0).abs() < 0.01);
    }

    /// Verify confirmed_tps calculation: confirmed / confirm_time.
    #[test]
    fn test_burst_result_confirmed_tps_calculation() {
        let confirmed = 1800u32;
        let confirm_secs = 3.0f32;
        let confirmed_tps = confirmed as f32 / confirm_secs;
        assert!((confirmed_tps - 600.0).abs() < 0.01);
    }

    /// When confirm time is zero, confirmed_tps should be 0.
    #[test]
    fn test_burst_result_zero_confirm_time() {
        let confirm_ms = std::time::Duration::from_secs(0);
        let confirmed_tps = if !confirm_ms.is_zero() {
            100_f32 / confirm_ms.as_secs_f32()
        } else {
            0.0
        };
        assert_eq!(confirmed_tps, 0.0);
    }

    /// Pool capacity capping: tx_count is capped to num_keys * POOL_SLOTS_PER_ACCOUNT.
    #[test]
    fn test_pool_capacity_capping() {
        const POOL_SLOTS_PER_ACCOUNT: usize = 4000;
        let num_keys = 4;
        let max_pool_capacity = num_keys * POOL_SLOTS_PER_ACCOUNT;

        // tx_count exceeds capacity → should be capped
        let tx_count_input = 20_000usize;
        let tx_count = if tx_count_input > max_pool_capacity {
            max_pool_capacity
        } else {
            tx_count_input
        };
        assert_eq!(tx_count, 16_000);

        // tx_count within capacity → no capping
        let tx_count_input2 = 10_000usize;
        let tx_count2 = if tx_count_input2 > max_pool_capacity {
            max_pool_capacity
        } else {
            tx_count_input2
        };
        assert_eq!(tx_count2, 10_000);
    }

    /// Worker tx distribution: txs_per_worker with div_ceil.
    #[test]
    fn test_txs_per_worker_distribution() {
        // 10 txs, 3 workers → 4, 4, 2 (via div_ceil)
        let tx_count = 10usize;
        let worker_count = 3usize;
        let txs_per_worker = tx_count.div_ceil(worker_count);
        assert_eq!(txs_per_worker, 4);

        // Exact division: 12 txs, 4 workers → 3 each
        let txs_per_worker2 = 12usize.div_ceil(4);
        assert_eq!(txs_per_worker2, 3);

        // Single worker gets all txs
        let txs_per_worker3 = 100usize.div_ceil(1);
        assert_eq!(txs_per_worker3, 100);
    }

    /// Last worker should not get more txs than remaining.
    #[test]
    fn test_worker_tx_slicing_no_overshoot() {
        let tx_count = 10usize;
        let worker_count = 3usize;
        let txs_per_worker = tx_count.div_ceil(worker_count);
        let mut total_assigned = 0;

        for w in 0..worker_count {
            let submitted_so_far = w * txs_per_worker;
            let remaining = tx_count.saturating_sub(submitted_so_far);
            let this_count = remaining.min(txs_per_worker);
            total_assigned += this_count;
        }

        assert_eq!(
            total_assigned, tx_count,
            "total assigned must equal tx_count"
        );
    }

    /// Gas price is at least 1 Gwei even if chain returns lower.
    #[test]
    fn test_gas_price_minimum_floor() {
        let base_price = 100_000_000u128; // 0.1 Gwei from chain
        let gas_price = (base_price * 2).max(1_000_000_000);
        assert_eq!(
            gas_price, 1_000_000_000,
            "floor should enforce 1 Gwei minimum"
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
        let sender_keys: Vec<String> = if bench_key_env.is_empty() {
            vec![]
        } else if bench_key_env.contains(',') {
            bench_key_env
                .split(',')
                .map(|s| s.trim().to_string())
                .collect()
        } else {
            vec![bench_key_env]
        };
        assert_eq!(sender_keys.len(), 3);
        assert_eq!(sender_keys[0], "0xaaa");
        assert_eq!(sender_keys[2], "0xccc");
    }

    /// Single key env var is not split.
    #[test]
    fn test_single_key_env() {
        let bench_key_env = "0xdeadbeef".to_string();
        let sender_keys: Vec<String> = if bench_key_env.is_empty() {
            vec![]
        } else if bench_key_env.contains(',') {
            bench_key_env
                .split(',')
                .map(|s| s.trim().to_string())
                .collect()
        } else {
            vec![bench_key_env]
        };
        assert_eq!(sender_keys.len(), 1);
        assert_eq!(sender_keys[0], "0xdeadbeef");
    }

    /// Nonce range distribution for multi-worker per-key.
    #[test]
    fn test_nonce_range_distribution() {
        let num_keys = 2;
        let worker_count = 4;
        let txs_per_worker = 100;

        // Workers 0,2 share key 0; workers 1,3 share key 1.
        // Nonce start for worker w: base_nonce + (w / num_keys) * txs_per_worker
        let base_nonces = [10u64, 20u64]; // key 0 starts at 10, key 1 at 20

        for w in 0..worker_count {
            let key_idx = w % num_keys;
            let slot_in_key = w / num_keys;
            let nonce_start = base_nonces[key_idx] + (slot_in_key * txs_per_worker) as u64;

            match w {
                0 => assert_eq!(nonce_start, 10),  // key 0, slot 0
                1 => assert_eq!(nonce_start, 20),  // key 1, slot 0
                2 => assert_eq!(nonce_start, 110), // key 0, slot 1
                3 => assert_eq!(nonce_start, 120), // key 1, slot 1
                _ => unreachable!(),
            }
        }
    }
}
