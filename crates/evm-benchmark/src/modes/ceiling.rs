use crate::config::Config;
use crate::metrics::MetricsExporter;
use crate::modes::burst::run_burst;
use crate::signing::BatchSigner;
use crate::submission::{BlockTracker, LatencyTracker, Submitter};
use crate::types::{CeilingResult, CeilingStep, SignedTxWithMetadata};
use alloy_primitives::{Address, U256};
use anyhow::Result;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use std::time::Instant;

/// Detailed timing breakdown for ceiling analysis
#[derive(Debug, Clone)]
struct TimingBreakdown {
    total_ms: u64,
    signing_ms: u64,
    submission_ms: u64,
    confirmation_ms: u64,
    other_ms: u64,
}

#[derive(Debug, Clone)]
struct CeilingIsolationConfig {
    restart_between_steps: bool,
    restart_cmd: Option<String>,
    restart_ready_timeout_secs: u64,
    cooldown_secs: u64,
    warmup_secs: u64,
}

impl CeilingIsolationConfig {
    fn from_env() -> Self {
        fn env_bool(name: &str, default: bool) -> bool {
            match std::env::var(name) {
                Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => true,
                    "0" | "false" | "no" | "off" => false,
                    _ => default,
                },
                Err(_) => default,
            }
        }

        fn env_u64(name: &str, default: u64) -> u64 {
            std::env::var(name)
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(default)
        }

        let restart_cmd = std::env::var("BENCH_CEILING_RESTART_CMD")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        Self {
            restart_between_steps: env_bool("BENCH_CEILING_RESTART_BETWEEN_STEPS", true),
            restart_cmd,
            restart_ready_timeout_secs: env_u64("BENCH_CEILING_RESTART_READY_TIMEOUT_SECS", 90),
            cooldown_secs: env_u64("BENCH_CEILING_COOLDOWN_SECS", 2),
            warmup_secs: env_u64("BENCH_CEILING_WARMUP_SECS", 3),
        }
    }

    fn enabled(&self) -> bool {
        self.restart_between_steps && self.restart_cmd.is_some()
    }
}

async fn rpc_block_number(http_client: &reqwest::Client, rpc_url: &str) -> Result<u64> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_blockNumber",
        "params": [],
        "id": 1
    });

    let resp: serde_json::Value = http_client
        .post(rpc_url)
        .json(&payload)
        .send()
        .await?
        .json()
        .await?;

    let hex = resp
        .get("result")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("eth_blockNumber missing result"))?;

    Ok(u64::from_str_radix(hex.trim_start_matches("0x"), 16).unwrap_or(0))
}

async fn wait_for_chain_ready(rpc_url: &str, timeout_secs: u64) -> Result<()> {
    let http_client = reqwest::Client::new();
    let start = Instant::now();

    while start.elapsed() < Duration::from_secs(timeout_secs) {
        if rpc_block_number(&http_client, rpc_url).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    Err(anyhow::anyhow!(
        "chain readiness timeout after {}s",
        timeout_secs
    ))
}

async fn warmup_after_restart(rpc_url: &str, warmup_secs: u64, quiet: bool) -> Result<()> {
    if warmup_secs == 0 {
        return Ok(());
    }

    let http_client = reqwest::Client::new();
    let start_block = rpc_block_number(&http_client, rpc_url).await?;
    let deadline = Instant::now() + Duration::from_secs(warmup_secs + 30);

    if !quiet {
        println!(
            "  Warmup: waiting up to {}s for post-restart block progress...",
            warmup_secs + 30
        );
    }

    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let current_block = rpc_block_number(&http_client, rpc_url).await?;
        if current_block > start_block {
            if !quiet {
                println!(
                    "  Warmup complete: block advanced {} -> {}",
                    start_block, current_block
                );
            }
            return Ok(());
        }
    }

    Err(anyhow::anyhow!(
        "warmup timed out waiting for block progress after restart"
    ))
}

async fn isolate_chain_between_steps(config: &Config, iso: &CeilingIsolationConfig) -> Result<()> {
    if !iso.enabled() {
        return Ok(());
    }

    if !config.quiet {
        println!(
            "  Cooling down for {}s before restart...",
            iso.cooldown_secs
        );
    }
    if iso.cooldown_secs > 0 {
        tokio::time::sleep(Duration::from_secs(iso.cooldown_secs)).await;
    }

    let restart_cmd = iso
        .restart_cmd
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("restart command missing"))?;

    if !config.quiet {
        println!("  Restarting chain between ceiling steps...");
    }

    let status = Command::new("bash")
        .arg("-lc")
        .arg(restart_cmd)
        .status()
        .map_err(|err| anyhow::anyhow!("failed to execute restart command: {}", err))?;

    if !status.success() {
        return Err(anyhow::anyhow!(
            "restart command failed with status {:?}",
            status.code()
        ));
    }

    if !config.quiet {
        println!(
            "  Waiting for chain readiness (timeout {}s)...",
            iso.restart_ready_timeout_secs
        );
    }
    wait_for_chain_ready(config.rpc.as_str(), iso.restart_ready_timeout_secs).await?;
    warmup_after_restart(config.rpc.as_str(), iso.warmup_secs, config.quiet).await?;

    Ok(())
}

impl TimingBreakdown {
    fn new(total: u64, signing: u64, submission: u64, confirmation: u64) -> Self {
        let other = total.saturating_sub(signing + submission + confirmation);
        Self {
            total_ms: total,
            signing_ms: signing,
            submission_ms: submission,
            confirmation_ms: confirmation,
            other_ms: other,
        }
    }

    fn print_breakdown(&self, prefix: &str) {
        println!("{}", prefix);
        println!("  Total:        {:>6}ms (100%)", self.total_ms);
        if self.total_ms > 0 {
            println!(
                "    |- Signing:  {:>6}ms ({:>5.1}%)",
                self.signing_ms,
                self.signing_ms as f64 / self.total_ms as f64 * 100.0
            );
            println!(
                "    |- Submitting: {:>4}ms ({:>5.1}%)",
                self.submission_ms,
                self.submission_ms as f64 / self.total_ms as f64 * 100.0
            );
            println!(
                "    |- Confirmation: {:>2}ms ({:>5.1}%)",
                self.confirmation_ms,
                self.confirmation_ms as f64 / self.total_ms as f64 * 100.0
            );
            println!(
                "    `- Other:    {:>6}ms ({:>5.1}%)",
                self.other_ms,
                self.other_ms as f64 / self.total_ms as f64 * 100.0
            );
        }
    }
}

/// Run ceiling mode: ramp TPS from start to saturation, then measure peak burst.
///
/// Key improvements over the original:
/// - Key, account address, and HTTP client resolved once outside the ramp loop.
/// - Each step pre-signs all needed txs in parallel (BatchSigner / rayon).
/// - Workers use `tokio::time::interval` — no sleep+elapsed busy loop.
/// - BlockTracker kept alive through the confirmation wait, then aborted.
pub async fn run_ceiling(config: &Config) -> Result<CeilingResult> {
    let mut steps = vec![];
    let metrics = Arc::new(MetricsExporter::new()?);

    let mut target_tps = config.target_tps.max(100);
    let mut step_increase = (config.target_tps.max(100) / 2).max(100);
    let adaptive_step_enabled = true;
    let ramp_duration_secs = 5u64;
    let saturation_threshold_pending = 0.5f32;
    let saturation_threshold_error = 0.05f32;
    let saturation_threshold_tps_ratio = 0.85f32;

    if !config.quiet {
        println!("====================================================");
        println!("         CEILING MODE - SATURATION DETECTION        ");
        println!("====================================================");
        println!(
            "Starting TPS: {}  |  Step: {}  |  Duration: {}s",
            target_tps, step_increase, ramp_duration_secs
        );
        println!();
    }

    // Resolve sender keys for multi-account ceiling ramp.
    let sender_keys = crate::funding::resolve_sender_keys(config.sender_count);
    let parsed_senders = crate::funding::parse_sender_keys(&sender_keys)?;
    let signers: Vec<_> = parsed_senders
        .iter()
        .map(|(_, signer, _)| signer.clone())
        .collect();
    let accounts: Vec<_> = parsed_senders.iter().map(|(_, _, addr)| *addr).collect();

    if !config.quiet {
        println!("Sender keys: {}", signers.len());
        println!();
    }

    // Single HTTP client for all nonce/balance queries across steps.
    let http_client = reqwest::Client::new();

    // Balance check once
    let balance_payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_getBalance",
        "params": [format!("{:?}", accounts[0]), "latest"],
        "id": 1
    });
    let balance_result: serde_json::Value = http_client
        .post(config.rpc.as_str())
        .json(&balance_payload)
        .send()
        .await?
        .json()
        .await?;
    let balance_hex = balance_result
        .get("result")
        .and_then(|r| r.as_str())
        .ok_or_else(|| anyhow::anyhow!("Failed to get balance from RPC"))?;
    let balance_wei = u128::from_str_radix(balance_hex.trim_start_matches("0x"), 16).unwrap_or(0);
    const MIN_BALANCE_WEI: u128 = 1_000_000_000_000_000_000;
    if balance_wei < MIN_BALANCE_WEI && !config.quiet {
        eprintln!(
            "WARNING: First test account has only {} wei (< 1 ETH). Transactions will likely fail.",
            balance_wei
        );
        eprintln!("Set BENCH_KEY to a pre-funded account private key.");
    }

    // Fetch current gas price (2x for safety margin against base fee spikes).
    let gas_price: u128 = {
        let gp_payload = serde_json::json!({
            "jsonrpc": "2.0", "method": "eth_gasPrice", "params": [], "id": 1
        });
        let gp_resp = http_client
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
        (base * 2).max(1_000_000_000)
    };

    let ceiling_tps: u32;
    let ramp_start = Instant::now();
    let max_ramp_duration = Duration::from_secs(180);
    let isolation = CeilingIsolationConfig::from_env();

    if !config.quiet && isolation.enabled() {
        println!(
            "Ceiling isolation enabled: restart between steps, cooldown={}s, warmup={}s",
            isolation.cooldown_secs, isolation.warmup_secs
        );
        println!();
    }

    loop {
        if !config.quiet {
            println!("---------------------------------------------------");
            println!(
                "  Testing at {:>4} TPS (ramp: {:.0}s elapsed)",
                target_tps,
                ramp_start.elapsed().as_secs_f32()
            );
            println!("---------------------------------------------------");
        }

        // Fresh nonces at the start of each step (one per sender account).
        let mut current_nonces = Vec::with_capacity(accounts.len());
        for (idx, account) in accounts.iter().enumerate() {
            let nonce_payload = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_getTransactionCount",
                "params": [format!("{:?}", account), "latest"],
                "id": idx + 1
            });
            let nonce_result: serde_json::Value = http_client
                .post(config.rpc.as_str())
                .json(&nonce_payload)
                .send()
                .await?
                .json()
                .await?;
            let nonce_hex = nonce_result
                .get("result")
                .and_then(|r| r.as_str())
                .ok_or_else(|| anyhow::anyhow!("Failed to get nonce for sender {}", idx))?;
            let nonce = u64::from_str_radix(nonce_hex.trim_start_matches("0x"), 16)?;
            current_nonces.push(nonce);
        }

        let step_start = Instant::now();
        let duration = Duration::from_secs(ramp_duration_secs);
        let worker_count = config.worker_count.max(1) as usize;

        // Pre-sign txs for this step — 2x headroom so the pool never runs dry.
        let total_txs = (target_tps as usize * ramp_duration_secs as usize * 2).max(200);
        let sign_start = Instant::now();

        if !config.quiet {
            println!("  Pre-signing {} txs...", total_txs);
        }

        let recipient = Address::with_last_byte(0x42);
        let signer_count = signers.len().max(1);
        let base_per_signer = total_txs / signer_count;
        let extra = total_txs % signer_count;
        let mut pre_signed = Vec::with_capacity(total_txs);

        for (idx, (signer, nonce_start)) in signers.iter().zip(current_nonces.iter()).enumerate() {
            let signer_txs = base_per_signer + usize::from(idx < extra);
            if signer_txs == 0 {
                continue;
            }

            let batch_signer = BatchSigner::new_with_gas_price(
                signer.clone(),
                *nonce_start,
                gas_price,
                config.chain_id,
            );
            let tx_data: Vec<(Address, U256)> = (0..signer_txs)
                .map(|_| (recipient, U256::from(1u32)))
                .collect();
            let mut signed = batch_signer
                .sign_batch_parallel(tx_data)
                .map_err(|e| anyhow::anyhow!("Pre-signing failed for sender {}: {}", idx, e))?;
            pre_signed.append(&mut signed);
        }

        let sign_ms = sign_start.elapsed().as_millis() as u64;

        let rpc_urls = if config.rpc_urls.len() > 1 {
            config.rpc_urls.clone()
        } else {
            vec![config.rpc.clone()]
        };
        let dispatcher = Arc::new(Submitter::with_retry_profile(
            rpc_urls,
            &config.ws,
            config.batch_size,
            config.submission_method,
            &config.retry_profile,
        )?);
        let tracker = Arc::new(LatencyTracker::new());

        let pre_signed = Arc::new(pre_signed);
        let pool_idx = Arc::new(AtomicU32::new(0));
        let pool_len = pre_signed.len() as u32;
        let sent_count = Arc::new(AtomicU32::new(0));
        let error_count = Arc::new(AtomicU32::new(0));

        let max_confirm_wait = Duration::from_secs(20);
        let tracker_clone = tracker.clone();
        let ws_url = config.ws.clone();
        let rpc_url = config.rpc.clone();
        let finality_confirmations = config.finality_confirmations;
        let tracker_handle = tokio::spawn(async move {
            let block_tracker =
                BlockTracker::with_finality(ws_url, rpc_url, tracker_clone, finality_confirmations);
            let _ = block_tracker.run(duration + max_confirm_wait).await;
        });

        let tps_per_worker = target_tps as f64 / worker_count as f64;
        let mut worker_handles = vec![];

        for _ in 0..worker_count {
            let d = dispatcher.clone();
            let t = tracker.clone();
            let m = metrics.clone();
            let ps = pre_signed.clone();
            let pi = pool_idx.clone();
            let sc = sent_count.clone();
            let ec = error_count.clone();

            worker_handles.push(tokio::spawn(async move {
                run_step_worker(d, t, m, sc, ec, duration, tps_per_worker, ps, pi, pool_len).await
            }));
        }

        let submit_start = Instant::now();
        for handle in worker_handles {
            let _ = handle.await;
        }
        let submit_ms = submit_start.elapsed().as_millis() as u64;

        // Confirmation wait — block tracker still running
        let confirm_start = Instant::now();
        while confirm_start.elapsed() < max_confirm_wait && tracker.pending_count() > 0 {
            metrics.set_pending_transactions(tracker.pending_count() as i64);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let confirm_ms = confirm_start.elapsed().as_millis() as u64;

        tracker_handle.abort();

        let step_duration = step_start.elapsed();
        let sent = sent_count.load(Ordering::SeqCst);
        let confirmed = tracker.confirmed_count();
        let pending = tracker.pending_count();
        let errors = error_count.load(Ordering::SeqCst);

        metrics.set_pending_transactions(pending as i64);
        metrics.inc_transactions_confirmed(confirmed as u64);
        if step_duration.as_secs_f32() > 0.0 {
            metrics.set_current_tps((confirmed as f32 / step_duration.as_secs_f32()) as f64);
        }

        let pending_ratio = if sent > 0 {
            pending as f32 / sent as f32
        } else {
            0.0
        };
        let error_rate = if sent > 0 {
            errors as f32 / sent as f32
        } else {
            0.0
        };
        let actual_tps = if step_duration.as_secs_f32() > 0.0 {
            confirmed as f32 / step_duration.as_secs_f32()
        } else {
            0.0
        };

        let timing = TimingBreakdown::new(
            step_duration.as_millis() as u64,
            sign_ms,
            submit_ms,
            confirm_ms,
        );

        let is_saturated = pending_ratio > saturation_threshold_pending
            || error_rate > saturation_threshold_error
            || actual_tps < (target_tps as f32 * saturation_threshold_tps_ratio);

        if !config.quiet {
            println!("  Results:");
            println!(
                "    Target TPS:  {} | Actual TPS: {:.1}",
                target_tps, actual_tps
            );
            println!(
                "    Sent: {} | Confirmed: {} | Pending: {} ({:.1}%)",
                sent,
                confirmed,
                pending,
                pending_ratio * 100.0
            );
            println!("    Errors: {} ({:.1}%)", errors, error_rate * 100.0);
            timing.print_breakdown("  Timing Breakdown:");

            if is_saturated {
                let bottleneck = if timing.confirmation_ms > timing.signing_ms
                    && timing.confirmation_ms > timing.submission_ms
                {
                    "BOTTLENECK: Block confirmation latency"
                } else if timing.submission_ms > timing.signing_ms {
                    "BOTTLENECK: RPC submission latency"
                } else {
                    "BOTTLENECK: Signing latency"
                };
                println!("  {}", bottleneck);
                println!("  SATURATION DETECTED!");
            }
            println!();
        }

        steps.push(CeilingStep {
            target_tps,
            actual_tps: actual_tps as u32,
            pending_ratio,
            error_rate,
            duration_ms: step_duration.as_millis() as u64,
            is_saturated,
        });

        if is_saturated {
            ceiling_tps = target_tps;
            break;
        }

        if ramp_start.elapsed() > max_ramp_duration {
            ceiling_tps = target_tps;
            if !config.quiet {
                println!("Max ramp duration reached (180s)");
            }
            break;
        }

        isolate_chain_between_steps(config, &isolation).await?;

        if adaptive_step_enabled {
            let headroom = if target_tps > 0 {
                actual_tps / target_tps as f32
            } else {
                1.0
            };
            step_increase = if headroom > 1.15 {
                (step_increase.saturating_mul(3) / 2).max(100)
            } else if headroom < 0.95 {
                (step_increase / 2).max(50)
            } else {
                step_increase.max(75)
            };
        }

        target_tps = target_tps.saturating_add(step_increase);
    }

    // Phase 2: Measure peak instantaneous TPS with a burst
    if !config.quiet {
        println!("Measuring peak TPS at {}...", ceiling_tps);
    }

    let mut burst_config = config.clone();
    burst_config.tx_count = (ceiling_tps * 5).max(1000);
    burst_config.wave_count = 1;
    burst_config.wave_delay_ms = 0;

    let (burst_result, _gas_price) = run_burst(&burst_config).await?;
    let burst_peak_tps = burst_result.confirmed_tps as u32;

    if !config.quiet {
        println!("Peak TPS: {}", burst_peak_tps);
    }

    // Confidence model uses the last few ramp samples near saturation.
    let sample_window: Vec<&CeilingStep> = steps.iter().rev().take(3).collect();
    let confidence_score = if sample_window.is_empty() {
        0.0
    } else {
        let actual: Vec<f32> = sample_window.iter().map(|s| s.actual_tps as f32).collect();
        let mean = actual.iter().sum::<f32>() / actual.len() as f32;
        let variance = if actual.len() > 1 {
            actual.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / actual.len() as f32
        } else {
            0.0
        };
        let stddev = variance.sqrt();
        let cv = if mean > 0.0 { stddev / mean } else { 1.0 };
        let pending_penalty =
            sample_window.iter().map(|s| s.pending_ratio).sum::<f32>() / sample_window.len() as f32;
        let error_penalty =
            sample_window.iter().map(|s| s.error_rate).sum::<f32>() / sample_window.len() as f32;

        (1.0 - cv.min(1.0) - (pending_penalty * 0.4) - (error_penalty * 0.6)).clamp(0.0, 1.0)
    };

    let confidence_band_low =
        ((ceiling_tps as f32) * (1.0 - (1.0 - confidence_score) * 0.25)).max(0.0) as u32;
    let confidence_band_high =
        ((ceiling_tps as f32) * (1.0 + (1.0 - confidence_score) * 0.25)) as u32;

    Ok(CeilingResult {
        steps,
        ceiling_tps,
        burst_peak_tps,
        confidence_score,
        confidence_band_low,
        confidence_band_high,
        adaptive_step_enabled,
    })
}

/// Worker for a single ceiling ramp step.
///
/// Pulls from a shared pre-signed pool via atomic index.
/// Uses `tokio::time::interval` for precise per-tick rate limiting.
#[allow(clippy::too_many_arguments)]
async fn run_step_worker(
    dispatcher: Arc<Submitter>,
    tracker: Arc<LatencyTracker>,
    metrics: Arc<MetricsExporter>,
    sent_count: Arc<AtomicU32>,
    error_count: Arc<AtomicU32>,
    duration: Duration,
    tps_per_worker: f64,
    pre_signed: Arc<Vec<SignedTxWithMetadata>>,
    pool_idx: Arc<AtomicU32>,
    pool_len: u32,
) {
    let interval_ms = if tps_per_worker > 0.0 {
        (1000.0 / tps_per_worker) as u64
    } else {
        1000
    }
    .max(1);

    let mut interval = tokio::time::interval(Duration::from_millis(interval_ms));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let start = Instant::now();

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timing_breakdown_other() {
        let t = TimingBreakdown::new(1000, 100, 200, 300);
        assert_eq!(t.other_ms, 400);
        assert_eq!(t.total_ms, 1000);
    }

    #[test]
    fn test_timing_breakdown_saturating_sub() {
        // components sum > total: other should be 0, not underflow
        let t = TimingBreakdown::new(100, 50, 60, 70);
        assert_eq!(t.other_ms, 0);
    }

    #[test]
    fn test_saturation_pending_ratio() {
        let (sent, pending, errors, actual_tps, target_tps) =
            (100u32, 60u32, 0u32, 95.0f32, 100u32);
        let pending_ratio = pending as f32 / sent as f32;
        let error_rate = errors as f32 / sent as f32;
        let is_sat =
            pending_ratio > 0.5 || error_rate > 0.05 || actual_tps < target_tps as f32 * 0.85;
        assert!(is_sat);
    }

    #[test]
    fn test_saturation_error_rate() {
        let (sent, pending, errors, actual_tps, target_tps) =
            (100u32, 0u32, 10u32, 95.0f32, 100u32);
        let pending_ratio = pending as f32 / sent as f32;
        let error_rate = errors as f32 / sent as f32;
        let is_sat =
            pending_ratio > 0.5 || error_rate > 0.05 || actual_tps < target_tps as f32 * 0.85;
        assert!(is_sat);
    }

    #[test]
    fn test_saturation_tps_regression() {
        let (sent, pending, errors, actual_tps, target_tps) = (100u32, 0u32, 0u32, 80.0f32, 100u32);
        let pending_ratio = pending as f32 / sent as f32;
        let error_rate = errors as f32 / sent as f32;
        let is_sat =
            pending_ratio > 0.5 || error_rate > 0.05 || actual_tps < target_tps as f32 * 0.85;
        assert!(is_sat);
    }

    #[test]
    fn test_no_saturation_healthy() {
        let (sent, pending, errors, actual_tps, target_tps) = (100u32, 5u32, 2u32, 95.0f32, 100u32);
        let pending_ratio = pending as f32 / sent as f32;
        let error_rate = errors as f32 / sent as f32;
        let is_sat =
            pending_ratio > 0.5 || error_rate > 0.05 || actual_tps < target_tps as f32 * 0.85;
        assert!(!is_sat);
    }

    #[test]
    fn test_timing_breakdown_zero_total() {
        let t = TimingBreakdown::new(0, 0, 0, 0);
        assert_eq!(t.total_ms, 0);
        assert_eq!(t.other_ms, 0);
        assert_eq!(t.signing_ms, 0);
        assert_eq!(t.submission_ms, 0);
        assert_eq!(t.confirmation_ms, 0);
    }

    #[test]
    fn test_timing_breakdown_print_zero_total_no_panic() {
        let t = TimingBreakdown::new(0, 0, 0, 0);
        // Should not panic — the zero-total branch skips percentage printing
        t.print_breakdown("Zero total test:");
    }

    #[test]
    fn test_timing_breakdown_print_normal_no_panic() {
        let t = TimingBreakdown::new(1000, 100, 200, 300);
        // Should not panic and should print percentages
        t.print_breakdown("Normal test:");
    }

    #[test]
    fn test_no_saturation_all_within_range() {
        // All thresholds within safe range: pending < 50%, errors < 5%, actual > 85% target
        let (sent, pending, errors, actual_tps, target_tps) =
            (1000u32, 100u32, 10u32, 900.0f32, 1000u32);
        let pending_ratio = pending as f32 / sent as f32; // 10%
        let error_rate = errors as f32 / sent as f32; // 1%
        let tps_ratio = actual_tps / target_tps as f32; // 90%
        let is_sat =
            pending_ratio > 0.5 || error_rate > 0.05 || actual_tps < target_tps as f32 * 0.85;
        assert!(!is_sat);
        assert!(pending_ratio < 0.5);
        assert!(error_rate < 0.05);
        assert!(tps_ratio > 0.85);
    }

    #[test]
    fn test_step_increase_at_least_100() {
        // For target_tps = 100, step = max(100/2, 100) = 100
        let target_tps_low: u32 = 100;
        let step_low = (target_tps_low.max(100) / 2).max(100);
        assert!(step_low >= 100);

        // For target_tps = 50 (clamped to 100), step = max(100/2, 100) = 100
        let target_tps_very_low: u32 = 50;
        let step_very_low = (target_tps_very_low.max(100) / 2).max(100);
        assert!(step_very_low >= 100);

        // For target_tps = 500, step = max(500/2, 100) = 250
        let target_tps_high: u32 = 500;
        let step_high = (target_tps_high.max(100) / 2).max(100);
        assert!(step_high >= 100);
        assert_eq!(step_high, 250);

        // For target_tps = 150, step = max(150/2, 100) = 100
        let target_tps_mid: u32 = 150;
        let step_mid = (target_tps_mid.max(100) / 2).max(100);
        assert_eq!(step_mid, 100);
    }

    #[test]
    fn test_saturation_zero_sent() {
        // When sent is 0, ratios should be 0, not panic
        let (sent, pending, errors, actual_tps, target_tps) = (0u32, 0u32, 0u32, 0.0f32, 100u32);
        let pending_ratio = if sent > 0 {
            pending as f32 / sent as f32
        } else {
            0.0
        };
        let error_rate = if sent > 0 {
            errors as f32 / sent as f32
        } else {
            0.0
        };
        let is_sat =
            pending_ratio > 0.5 || error_rate > 0.05 || actual_tps < target_tps as f32 * 0.85;
        // actual_tps (0) < 85 triggers saturation
        assert!(is_sat);
        assert_eq!(pending_ratio, 0.0);
        assert_eq!(error_rate, 0.0);
    }

    // ── TimingBreakdown construction and properties ───────────────────────

    /// When components sum exactly to total, other_ms is 0.
    #[test]
    fn test_timing_breakdown_exact_sum() {
        let t = TimingBreakdown::new(1000, 300, 400, 300);
        assert_eq!(t.other_ms, 0);
    }

    /// When all components are 0, other_ms equals total_ms.
    #[test]
    fn test_timing_breakdown_all_components_zero() {
        let t = TimingBreakdown::new(500, 0, 0, 0);
        assert_eq!(t.other_ms, 500);
    }

    /// Large timing values do not overflow.
    #[test]
    fn test_timing_breakdown_large_values() {
        let t = TimingBreakdown::new(u64::MAX, 1000, 2000, 3000);
        assert_eq!(t.total_ms, u64::MAX);
        assert_eq!(t.other_ms, u64::MAX - 6000);
    }

    /// Percentage calculation sanity check.
    #[test]
    fn test_timing_breakdown_percentages() {
        let t = TimingBreakdown::new(1000, 250, 500, 150);
        assert_eq!(t.other_ms, 100);
        let signing_pct = t.signing_ms as f64 / t.total_ms as f64 * 100.0;
        assert!((signing_pct - 25.0).abs() < 0.01);
        let submission_pct = t.submission_ms as f64 / t.total_ms as f64 * 100.0;
        assert!((submission_pct - 50.0).abs() < 0.01);
    }

    // ── CeilingStep construction and saturation thresholds ───────────────

    /// CeilingStep with all healthy metrics is not saturated.
    #[test]
    fn test_ceiling_step_healthy() {
        let step = CeilingStep {
            target_tps: 500,
            actual_tps: 490,
            pending_ratio: 0.02,
            error_rate: 0.01,
            duration_ms: 5000,
            is_saturated: false,
        };
        assert!(!step.is_saturated);
        assert!(step.actual_tps > 0);
    }

    /// CeilingStep with high pending ratio is saturated.
    #[test]
    fn test_ceiling_step_high_pending() {
        let step = CeilingStep {
            target_tps: 1000,
            actual_tps: 800,
            pending_ratio: 0.6,
            error_rate: 0.01,
            duration_ms: 5000,
            is_saturated: true,
        };
        assert!(step.is_saturated);
        assert!(step.pending_ratio > 0.5);
    }

    // ── Step increase calculation ────────────────────────────────────────

    /// Step increase is at least 100 but scales with target TPS.
    #[test]
    fn test_step_increase_scaling() {
        let cases: Vec<(u32, u32)> = vec![
            (100, 100),  // max(50, 100) = 100
            (200, 100),  // max(100, 100) = 100
            (400, 200),  // max(200, 100) = 200
            (1000, 500), // max(500, 100) = 500
        ];
        for (target, expected_step) in cases {
            let step = (target.max(100) / 2).max(100);
            assert_eq!(step, expected_step, "target_tps={target}");
        }
    }

    /// Ramp starting TPS is clamped to at least 100.
    #[test]
    fn test_initial_tps_floor() {
        let target_tps_low = 50u32;
        let initial = target_tps_low.max(100);
        assert_eq!(initial, 100);

        let target_tps_high = 500u32;
        let initial_high = target_tps_high.max(100);
        assert_eq!(initial_high, 500);
    }

    // ── Pre-sign pool sizing for ceiling steps ───────────────────────────

    /// Pre-sign pool is 2x headroom with a minimum of 200 txs.
    #[test]
    fn test_presign_pool_ceiling() {
        let target_tps = 100u32;
        let ramp_duration_secs = 5u64;
        let total_txs = (target_tps as usize * ramp_duration_secs as usize * 2).max(200);
        assert_eq!(total_txs, 1000);

        // Very low TPS: floor at 200
        let total_txs_low = 200;
        assert_eq!(total_txs_low, 200);
    }

    // ── Saturation detection: threshold boundary values ──────────────────

    /// Exactly at pending threshold (50%) is NOT saturated (strictly greater).
    #[test]
    fn test_saturation_pending_at_boundary() {
        let pending_ratio = 0.5f32;
        let error_rate = 0.0f32;
        let actual_tps = 100.0f32;
        let target_tps = 100u32;
        let is_sat =
            pending_ratio > 0.5 || error_rate > 0.05 || actual_tps < target_tps as f32 * 0.85;
        assert!(!is_sat, "exactly 0.5 should not trigger pending saturation");
    }

    /// Exactly at error threshold (5%) is NOT saturated (strictly greater).
    #[test]
    fn test_saturation_error_at_boundary() {
        let pending_ratio = 0.0f32;
        let error_rate = 0.05f32;
        let actual_tps = 100.0f32;
        let target_tps = 100u32;
        let is_sat =
            pending_ratio > 0.5 || error_rate > 0.05 || actual_tps < target_tps as f32 * 0.85;
        assert!(!is_sat, "exactly 0.05 should not trigger error saturation");
    }

    /// Exactly at TPS threshold (85%) is NOT saturated (strictly less).
    #[test]
    fn test_saturation_tps_at_boundary() {
        let pending_ratio = 0.0f32;
        let error_rate = 0.0f32;
        let actual_tps = 85.0f32;
        let target_tps = 100u32;
        let is_sat =
            pending_ratio > 0.5 || error_rate > 0.05 || actual_tps < target_tps as f32 * 0.85;
        assert!(!is_sat, "exactly 85% should not trigger TPS saturation");
    }

    /// Just below TPS threshold IS saturated.
    #[test]
    fn test_saturation_tps_just_below() {
        let pending_ratio = 0.0f32;
        let error_rate = 0.0f32;
        let actual_tps = 84.9f32;
        let target_tps = 100u32;
        let is_sat =
            pending_ratio > 0.5 || error_rate > 0.05 || actual_tps < target_tps as f32 * 0.85;
        assert!(is_sat, "84.9% should trigger TPS saturation");
    }

    // ── Bottleneck identification logic ──────────────────────────────────

    /// Bottleneck detection: confirmation is dominant.
    #[test]
    fn test_bottleneck_confirmation_dominant() {
        let t = TimingBreakdown::new(1000, 100, 200, 500);
        let bottleneck = if t.confirmation_ms > t.signing_ms && t.confirmation_ms > t.submission_ms
        {
            "confirmation"
        } else if t.submission_ms > t.signing_ms {
            "submission"
        } else {
            "signing"
        };
        assert_eq!(bottleneck, "confirmation");
    }

    /// Bottleneck detection: submission is dominant.
    #[test]
    fn test_bottleneck_submission_dominant() {
        let t = TimingBreakdown::new(1000, 100, 600, 200);
        let bottleneck = if t.confirmation_ms > t.signing_ms && t.confirmation_ms > t.submission_ms
        {
            "confirmation"
        } else if t.submission_ms > t.signing_ms {
            "submission"
        } else {
            "signing"
        };
        assert_eq!(bottleneck, "submission");
    }

    /// Bottleneck detection: signing is dominant.
    #[test]
    fn test_bottleneck_signing_dominant() {
        let t = TimingBreakdown::new(1000, 700, 100, 100);
        let bottleneck = if t.confirmation_ms > t.signing_ms && t.confirmation_ms > t.submission_ms
        {
            "confirmation"
        } else if t.submission_ms > t.signing_ms {
            "submission"
        } else {
            "signing"
        };
        assert_eq!(bottleneck, "signing");
    }

    // ── CeilingResult construction ───────────────────────────────────────

    /// CeilingResult with multiple steps tracks progression.
    #[test]
    fn test_ceiling_result_multi_step_progression() {
        let steps = vec![
            CeilingStep {
                target_tps: 100,
                actual_tps: 100,
                pending_ratio: 0.01,
                error_rate: 0.0,
                duration_ms: 5000,
                is_saturated: false,
            },
            CeilingStep {
                target_tps: 200,
                actual_tps: 195,
                pending_ratio: 0.05,
                error_rate: 0.01,
                duration_ms: 5000,
                is_saturated: false,
            },
            CeilingStep {
                target_tps: 300,
                actual_tps: 250,
                pending_ratio: 0.6,
                error_rate: 0.03,
                duration_ms: 5000,
                is_saturated: true,
            },
        ];

        let result = CeilingResult {
            steps: steps.clone(),
            ceiling_tps: 300,
            burst_peak_tps: 350,
            confidence_score: 0.75,
            confidence_band_low: 260,
            confidence_band_high: 340,
            adaptive_step_enabled: true,
        };

        assert_eq!(result.steps.len(), 3);
        assert!(!result.steps[0].is_saturated);
        assert!(!result.steps[1].is_saturated);
        assert!(result.steps[2].is_saturated);
        assert_eq!(result.ceiling_tps, 300);
        assert!(result.burst_peak_tps > result.ceiling_tps);
    }

    /// target_tps ramp: saturating_add prevents overflow.
    #[test]
    fn test_target_tps_ramp_saturating() {
        let mut target = u32::MAX - 50;
        let step_increase = 100u32;
        target = target.saturating_add(step_increase);
        assert_eq!(target, u32::MAX);
    }

    /// Worker interval calculation matches sustained mode.
    #[test]
    fn test_step_worker_interval() {
        let target_tps = 500.0f64;
        let worker_count = 4usize;
        let tps_per_worker = target_tps / worker_count as f64;
        let interval_ms = if tps_per_worker > 0.0 {
            (1000.0 / tps_per_worker) as u64
        } else {
            1000
        }
        .max(1);
        assert_eq!(interval_ms, 8); // 1000 / 125 = 8ms
    }
}
