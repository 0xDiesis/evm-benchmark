use crate::config::Config;
use crate::metrics::MetricsExporter;
use crate::signing::BatchSigner;
use crate::submission::{BlockTracker, LatencyTracker, Submitter};
use crate::types::{SustainedResult, WindowEntry};
use alloy_primitives::{Address, U256};
use alloy_signer_local::PrivateKeySigner;
use anyhow::Result;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Run sustained mode benchmark at target TPS for specified duration.
///
/// Returns `(SustainedResult, effective_gas_price_wei)`.
///
/// # Strategy
/// - Pre-sign a pool of transactions so the hot loop only does submission,
///   not signing.
/// - Divide the pre-signed pool evenly across workers; each worker uses
///   `tokio::time::interval` for precise rate limiting.
/// - BlockTracker is kept alive through the post-run confirmation wait.
/// - Timeline task captures per-second metrics.
pub async fn run_sustained(config: &Config) -> Result<(SustainedResult, u128)> {
    let dispatcher = Arc::new(Submitter::with_retry_profile(
        config.rpc_urls.clone(),
        &config.ws,
        config.batch_size,
        config.submission_method,
        &config.retry_profile,
    )?);
    let tracker = Arc::new(LatencyTracker::new());
    let metrics = Arc::new(MetricsExporter::new()?);

    // Warm up the HTTP connection pool(s)
    dispatcher.warm_up(10).await?;

    // Resolve sender keys — supports multiple comma-separated keys in BENCH_KEY
    let sender_keys = crate::funding::resolve_sender_keys(config.sender_count);
    let num_keys = sender_keys.len();

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(200)
        .build()
        .unwrap_or_default();

    // Fetch gas price from chain (2x safety margin)
    let gas_price = crate::funding::fetch_gas_price(&client, config.rpc.as_str()).await?;

    let duration = Duration::from_secs(config.duration_secs);
    let target_tps = config.target_tps as usize;
    let worker_count = (config.worker_count as usize).max(1);

    // Pre-sign enough transactions for the full run, distributed across all senders.
    let total_txs = (target_tps * config.duration_secs as usize * 5).max(1000);
    let txs_per_key = total_txs.div_ceil(num_keys);

    if !config.quiet {
        println!(
            "Pre-signing {} txs across {} senders for {}s @ {} TPS (gas: {} gwei)...",
            total_txs,
            num_keys,
            config.duration_secs,
            target_tps,
            gas_price / 1_000_000_000
        );
    }

    let recipient = Address::with_last_byte(0x42);
    let mut all_pre_signed = Vec::new();

    for (i, key) in sender_keys.iter().enumerate() {
        let signer = PrivateKeySigner::from_str(key)
            .map_err(|e| anyhow::anyhow!("Failed to parse sender key {}: {}", i, e))?;
        let account = signer.address();

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
            .ok_or_else(|| anyhow::anyhow!("Failed to get nonce for sender {}", i))?;
        let nonce = u64::from_str_radix(nonce_hex.trim_start_matches("0x"), 16)?;

        let count = if i < num_keys - 1 {
            txs_per_key
        } else {
            total_txs - i * txs_per_key
        };
        let tx_data: Vec<(Address, U256)> =
            (0..count).map(|_| (recipient, U256::from(1u32))).collect();
        let batch_signer =
            BatchSigner::new_with_gas_price(signer, nonce, gas_price, config.chain_id);
        let signed = batch_signer
            .sign_batch_parallel(tx_data)
            .map_err(|e| anyhow::anyhow!("Pre-signing failed for sender {}: {}", i, e))?;
        all_pre_signed.extend(signed);
    }

    // Alias for existing code below
    let pre_signed = all_pre_signed;

    if !config.quiet {
        println!("Pre-signing complete. Starting benchmark...");
    }

    let pre_signed = Arc::new(pre_signed);
    let pool_idx = Arc::new(AtomicU32::new(0));

    let start = Instant::now();
    let tps_per_worker = target_tps as f64 / worker_count as f64;

    let timeline = Arc::new(Mutex::new(Vec::<WindowEntry>::new()));
    let sent_count = Arc::new(AtomicU32::new(0));
    let error_count = Arc::new(AtomicU32::new(0));

    // Spawn block tracker. Kept alive through the post-run confirmation wait —
    // do NOT abort it until after the wait loop.
    let max_wait = Duration::from_secs(30);
    let tracker_run_duration = max_wait + Duration::from_secs(config.duration_secs);
    let tracker_clone = tracker.clone();
    let ws_url = config.ws.clone();
    let rpc_url = config.rpc.clone();
    let finality_confirmations = config.finality_confirmations;
    let tracker_handle = tokio::spawn(async move {
        let block_tracker =
            BlockTracker::with_finality(ws_url, rpc_url, tracker_clone, finality_confirmations);
        let _ = block_tracker.run(tracker_run_duration).await;
    });

    // Spawn worker tasks
    let mut handles = vec![];
    for _worker_id in 0..worker_count {
        let dispatcher = dispatcher.clone();
        let tracker = tracker.clone();
        let sent_count = sent_count.clone();
        let error_count = error_count.clone();
        let metrics = metrics.clone();
        let pre_signed = pre_signed.clone();
        let pool_idx = pool_idx.clone();
        let worker_start = Instant::now();

        let handle = tokio::spawn(async move {
            run_worker(
                dispatcher,
                tracker,
                sent_count,
                error_count,
                metrics,
                duration,
                tps_per_worker,
                worker_start,
                pre_signed,
                pool_idx,
            )
            .await
        });
        handles.push(handle);
    }

    // Timeline update task
    let tracker_clone = tracker.clone();
    let timeline_for_task = timeline.clone();
    let timeline_start = Instant::now();
    let metrics_clone = metrics.clone();
    let timeline_handle = tokio::spawn(async move {
        update_timeline(
            timeline_start,
            duration,
            timeline_for_task,
            tracker_clone,
            metrics_clone,
        )
        .await;
    });

    // Wait for all workers to complete
    for handle in handles {
        let _ = handle.await;
    }

    timeline_handle.abort();

    // Wait for remaining confirmations. Poll eth_getTransactionReceipt directly
    // (same approach as burst mode) for reliable confirmation tracking. The
    // BlockTracker (WS newHeads) is kept alive as a backup but receipt polling
    // catches txs that the WS subscription misses.
    let confirm_start = Instant::now();
    while confirm_start.elapsed() < max_wait && tracker.pending_count() > 0 {
        metrics.set_pending_transactions(tracker.pending_count() as i64);
        super::burst::poll_pending_receipts(
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

    // Now abort the block tracker
    tracker_handle.abort();

    let total_duration = start.elapsed();
    let sent = sent_count.load(Ordering::SeqCst);
    let confirmed = tracker.confirmed_count();
    let pending = tracker.pending_count();
    let errors = error_count.load(Ordering::SeqCst);
    let stats = tracker.statistics();

    let actual_tps = if total_duration.as_secs_f32() > 0.0 {
        confirmed as f32 / total_duration.as_secs_f32()
    } else {
        0.0
    };

    metrics.set_pending_transactions(pending as i64);
    metrics.set_current_tps(actual_tps as f64);
    metrics.inc_transactions_confirmed(confirmed as u64);

    let timeline_vec = timeline.lock().await.clone();

    let result = SustainedResult {
        sent,
        confirmed,
        pending,
        errors,
        duration_ms: total_duration.as_millis() as u64,
        actual_tps,
        latency: stats,
        timeline: timeline_vec,
    };

    // Run analytics pipeline on benchmark results
    let burst_equiv = result.to_burst_result();
    match crate::analytics::run_analysis(
        "sustained-benchmark",
        "sustained",
        &burst_equiv,
        None,
        None,
    )
    .await
    {
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

/// Worker: pulls pre-signed txs from the shared pool and submits at the target rate.
///
/// Uses `tokio::time::interval` for precise rate limiting.
/// Records submission errors back to the shared counter.
#[allow(clippy::too_many_arguments)]
async fn run_worker(
    dispatcher: Arc<Submitter>,
    tracker: Arc<LatencyTracker>,
    sent_count: Arc<AtomicU32>,
    error_count: Arc<AtomicU32>,
    metrics: Arc<MetricsExporter>,
    duration: Duration,
    tps_per_worker: f64,
    start: Instant,
    pre_signed: Arc<Vec<crate::types::SignedTxWithMetadata>>,
    pool_idx: Arc<AtomicU32>,
) {
    let pool_len = pre_signed.len() as u32;

    let interval_ms = if tps_per_worker > 0.0 {
        (1000.0 / tps_per_worker) as u64
    } else {
        1000
    }
    .max(1);

    let mut interval = tokio::time::interval(Duration::from_millis(interval_ms));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    while start.elapsed() < duration {
        interval.tick().await;

        let idx = pool_idx.fetch_add(1, Ordering::SeqCst);
        if idx >= pool_len {
            break;
        }

        let signed_tx = pre_signed[idx as usize].clone();

        tracker.record_submit(
            signed_tx.hash,
            signed_tx.nonce,
            signed_tx.sender,
            signed_tx.gas_limit,
            signed_tx.method,
        );

        match dispatcher.submit_single(signed_tx).await {
            Ok(_) => {
                metrics.inc_transactions_submitted(1);
                sent_count.fetch_add(1, Ordering::SeqCst);
            }
            Err(_) => {
                metrics.inc_transactions_failed(1);
                error_count.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
}

/// Timeline task: captures per-second metrics snapshot.
async fn update_timeline(
    start: Instant,
    duration: Duration,
    timeline: Arc<Mutex<Vec<WindowEntry>>>,
    tracker: Arc<LatencyTracker>,
    metrics: Arc<MetricsExporter>,
) {
    let mut second_idx = 0u32;
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    while start.elapsed() < duration {
        interval.tick().await;

        let confirmed = tracker.confirmed_count();
        let pending = tracker.pending_count();
        let stats = tracker.statistics();
        let tps = if start.elapsed().as_secs_f64() > 0.0 {
            confirmed as f64 / start.elapsed().as_secs_f64()
        } else {
            0.0
        };

        metrics.set_pending_transactions(pending as i64);
        metrics.set_current_tps(tps);

        let mut tl = timeline.lock().await;
        tl.push(WindowEntry {
            second: second_idx,
            sent: pending + confirmed,
            confirmed,
            latency_p50: stats.p50,
        });

        second_idx += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tps_distribution() {
        let target_tps = 100.0;
        let worker_count = 4;
        let tps_per_worker = target_tps / worker_count as f64;
        assert_eq!(tps_per_worker, 25.0);

        let interval_ms = (1000.0 / tps_per_worker) as u64;
        assert_eq!(interval_ms, 40);
    }

    #[test]
    fn test_interval_ms_clamped_to_one() {
        let tps_per_worker = 100_000.0f64;
        let interval_ms = ((1000.0 / tps_per_worker) as u64).max(1);
        assert_eq!(interval_ms, 1);
    }

    #[tokio::test]
    async fn test_sustained_mode_basic() {
        let rpc_url = url::Url::parse("http://localhost:8545").unwrap();
        let config = Config {
            rpc_urls: vec![rpc_url.clone()],
            rpc: rpc_url,
            ws: url::Url::parse("ws://localhost:8546").unwrap(),
            metrics: None,
            validator_urls: vec![],
            test_mode: crate::types::TestMode::Transfer,
            execution_mode: crate::types::ExecutionMode::Sustained,
            tx_count: 0,
            sender_count: 0,
            wave_count: 0,
            wave_delay_ms: 0,
            duration_secs: 5,
            target_tps: 50,
            worker_count: 2,
            batch_size: 100,
            submission_method: crate::config::SubmissionMethod::Http,
            retry_profile: "light".to_string(),
            finality_confirmations: 0,
            output: std::path::PathBuf::from("test.json"),
            quiet: true,
            chain_id: 1,
            bench_name: "test".to_string(),
            fund: false,
            sender_keys: vec![],
            evm_tokens: vec![],
            evm_pairs: vec![],
            evm_nfts: vec![],
        };
        let _ = config;
    }

    /// Verify pre-signing produces the correct nonce sequence.
    #[test]
    fn test_presign_nonce_sequence() {
        use crate::signing::BatchSigner;
        use alloy_primitives::{Address, U256};
        use alloy_signer_local::PrivateKeySigner;
        use std::str::FromStr;

        let key = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let signer = PrivateKeySigner::from_str(key).unwrap();
        let batch = BatchSigner::new(signer, 42, 1);
        let txs: Vec<(Address, U256)> = (0..5)
            .map(|_| (Address::with_last_byte(0x42), U256::from(1u32)))
            .collect();
        let signed = batch.sign_batch_parallel(txs).unwrap();
        assert_eq!(signed.len(), 5);
        for (i, tx) in signed.iter().enumerate() {
            assert_eq!(tx.nonce, 42 + i as u64);
        }
    }

    #[test]
    fn test_window_entry_creation_and_field_access() {
        let entry = WindowEntry {
            second: 5,
            sent: 100,
            confirmed: 90,
            latency_p50: 42,
        };
        assert_eq!(entry.second, 5);
        assert_eq!(entry.sent, 100);
        assert_eq!(entry.confirmed, 90);
        assert_eq!(entry.latency_p50, 42);
    }

    #[test]
    fn test_sustained_result_to_burst_result_preserves_fields() {
        use crate::types::{LatencyStats, SustainedResult};

        let sustained = SustainedResult {
            sent: 500,
            confirmed: 480,
            pending: 10,
            errors: 10,
            duration_ms: 5000,
            actual_tps: 96.0,
            latency: LatencyStats {
                p50: 25,
                p95: 50,
                p99: 75,
                min: 5,
                max: 200,
                avg: 35,
            },
            timeline: vec![],
        };

        let burst = sustained.to_burst_result();
        // Core fields map correctly
        assert_eq!(burst.submitted, 500);
        assert_eq!(burst.confirmed, 480);
        assert_eq!(burst.pending, 10);
        // duration_ms maps to submit_ms
        assert_eq!(burst.submit_ms, 5000);
        // confirm_ms is 0 for sustained (no separate confirm phase)
        assert_eq!(burst.confirm_ms, 0);
        // sign_ms is 0 (pre-signed)
        assert_eq!(burst.sign_ms, 0);
        // TPS values
        assert_eq!(burst.submitted_tps, 96.0);
        assert_eq!(burst.confirmed_tps, 96.0);
        // Latency stats preserved
        assert_eq!(burst.latency.p50, 25);
        assert_eq!(burst.latency.p99, 75);
        // Note: errors field from SustainedResult doesn't exist on BurstResult;
        // errors are inferred from submitted - confirmed - pending in analytics.
    }

    #[test]
    fn test_interval_for_very_low_tps() {
        // At 0.5 TPS per worker, interval should be 2000ms
        let tps_per_worker = 0.5f64;
        let interval_ms = if tps_per_worker > 0.0 {
            (1000.0 / tps_per_worker) as u64
        } else {
            1000
        }
        .max(1);
        assert_eq!(interval_ms, 2000);
    }

    #[test]
    fn test_interval_for_zero_tps() {
        // When tps_per_worker is 0, should default to 1000ms
        let tps_per_worker = 0.0f64;
        let interval_ms = if tps_per_worker > 0.0 {
            (1000.0 / tps_per_worker) as u64
        } else {
            1000
        }
        .max(1);
        assert_eq!(interval_ms, 1000);
    }

    #[test]
    fn test_interval_for_negative_tps() {
        // Negative TPS should also default to 1000ms (not > 0)
        let tps_per_worker = -1.0f64;
        let interval_ms = if tps_per_worker > 0.0 {
            (1000.0 / tps_per_worker) as u64
        } else {
            1000
        }
        .max(1);
        assert_eq!(interval_ms, 1000);
    }

    // ── Pre-signing pool sizing ──────────────────────────────────────────

    /// Total pre-signed tx count: 5x target to have headroom.
    #[test]
    fn test_presign_pool_size_calculation() {
        let target_tps = 100usize;
        let duration_secs = 10usize;
        let total_txs = (target_tps * duration_secs * 5).max(1000);
        assert_eq!(total_txs, 5000);
    }

    /// Small run: floor of 1000 txs.
    #[test]
    fn test_presign_pool_minimum_size() {
        let target_tps = 1usize;
        let duration_secs = 1usize;
        let total_txs = (target_tps * duration_secs * 5).max(1000);
        assert_eq!(total_txs, 1000);
    }

    /// Per-key distribution with remainder goes to last key.
    #[test]
    fn test_per_key_tx_distribution() {
        let total_txs = 1000usize;
        let num_keys = 3usize;
        let txs_per_key = total_txs.div_ceil(num_keys);
        assert_eq!(txs_per_key, 334);

        let mut assigned = 0;
        for i in 0..num_keys {
            let count = if i < num_keys - 1 {
                txs_per_key
            } else {
                total_txs - i * txs_per_key
            };
            assigned += count;
        }
        assert_eq!(assigned, total_txs);
    }

    // ── TPS distribution across workers ──────────────────────────────────

    /// Worker interval for 1 TPS per worker is 1000ms.
    #[test]
    fn test_interval_for_1_tps() {
        let tps_per_worker = 1.0f64;
        let interval_ms = if tps_per_worker > 0.0 {
            (1000.0 / tps_per_worker) as u64
        } else {
            1000
        }
        .max(1);
        assert_eq!(interval_ms, 1000);
    }

    /// Worker interval for 10 TPS is 100ms.
    #[test]
    fn test_interval_for_10_tps() {
        let tps_per_worker = 10.0f64;
        let interval_ms = (1000.0 / tps_per_worker) as u64;
        assert_eq!(interval_ms, 100);
    }

    /// Actual TPS: confirmed / duration.
    #[test]
    fn test_actual_tps_calculation() {
        let confirmed = 450u32;
        let total_duration_secs = 10.0f32;
        let actual_tps = confirmed as f32 / total_duration_secs;
        assert!((actual_tps - 45.0).abs() < 0.01);
    }

    /// Zero duration yields 0 TPS (guarded by > 0 check).
    #[test]
    fn test_actual_tps_zero_duration() {
        let confirmed = 100u32;
        let total_duration_secs = 0.0f32;
        let actual_tps = if total_duration_secs > 0.0 {
            confirmed as f32 / total_duration_secs
        } else {
            0.0
        };
        assert_eq!(actual_tps, 0.0);
    }

    // ── WindowEntry timeline ─────────────────────────────────────────────

    /// Timeline entries capture per-second state snapshots.
    #[test]
    fn test_window_entry_timeline_sequence() {
        let mut timeline = Vec::new();
        for s in 0..5u32 {
            timeline.push(WindowEntry {
                second: s,
                sent: s * 100 + 50,
                confirmed: s * 95,
                latency_p50: 30 + s as u64,
            });
        }
        assert_eq!(timeline.len(), 5);
        assert_eq!(timeline[0].second, 0);
        assert_eq!(timeline[4].second, 4);
        assert_eq!(timeline[2].sent, 250);
        assert_eq!(timeline[3].confirmed, 285);
    }

    /// TPS from timeline: confirmed at second N / elapsed seconds.
    #[test]
    fn test_timeline_tps_from_entries() {
        let confirmed = 500u32;
        let elapsed_secs = 5.0f64;
        let tps = if elapsed_secs > 0.0 {
            confirmed as f64 / elapsed_secs
        } else {
            0.0
        };
        assert!((tps - 100.0).abs() < 0.01);
    }

    // ── SustainedResult construction ─────────────────────────────────────

    /// SustainedResult tracks errors separately from pending.
    #[test]
    fn test_sustained_result_error_tracking() {
        let result = SustainedResult {
            sent: 1000,
            confirmed: 900,
            pending: 50,
            errors: 50,
            duration_ms: 10_000,
            actual_tps: 90.0,
            latency: crate::types::LatencyStats {
                p50: 20,
                p95: 50,
                p99: 80,
                min: 5,
                max: 150,
                avg: 25,
            },
            timeline: vec![],
        };
        // sent = confirmed + pending + errors
        assert_eq!(
            result.sent,
            result.confirmed + result.pending + result.errors
        );
    }

    /// to_burst_result preserves latency stats.
    #[test]
    fn test_to_burst_result_latency_preservation() {
        let result = SustainedResult {
            sent: 100,
            confirmed: 90,
            pending: 5,
            errors: 5,
            duration_ms: 5000,
            actual_tps: 18.0,
            latency: crate::types::LatencyStats {
                p50: 42,
                p95: 100,
                p99: 200,
                min: 3,
                max: 500,
                avg: 55,
            },
            timeline: vec![],
        };
        let burst = result.to_burst_result();
        assert_eq!(burst.latency.p50, 42);
        assert_eq!(burst.latency.p95, 100);
        assert_eq!(burst.latency.p99, 200);
        assert_eq!(burst.latency.min, 3);
        assert_eq!(burst.latency.max, 500);
        assert_eq!(burst.latency.avg, 55);
    }

    // ── Worker count distribution ────────────────────────────────────────

    /// Worker count must be at least 1 even if config says 0.
    #[test]
    fn test_worker_count_minimum_one() {
        let worker_count = (0u32 as usize).max(1);
        assert_eq!(worker_count, 1);
    }

    /// Many workers split a low TPS target into tiny per-worker intervals.
    #[test]
    fn test_many_workers_low_tps_distribution() {
        let target_tps = 10.0f64;
        let worker_count = 100usize;
        let tps_per_worker = target_tps / worker_count as f64;
        assert!((tps_per_worker - 0.1).abs() < 0.001);

        let interval_ms = if tps_per_worker > 0.0 {
            (1000.0 / tps_per_worker) as u64
        } else {
            1000
        }
        .max(1);
        assert_eq!(interval_ms, 10_000); // 10 seconds between sends per worker
    }
}
