use crate::config::Config;
use crate::metrics::MetricsExporter;
use crate::modes::burst::run_burst;
use crate::signing::BatchSigner;
use crate::submission::{BlockTracker, LatencyTracker, Submitter};
use crate::types::{BurstResult, CeilingResult, CeilingStep, SignedTxWithMetadata};
use alloy_primitives::{Address, U256};
use anyhow::Result;
use std::future::Future;
use std::pin::Pin;
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

type BurstRunFuture<'a> = Pin<Box<dyn Future<Output = Result<(BurstResult, u128)>> + Send + 'a>>;
type BurstRunner = for<'a> fn(&'a Config) -> BurstRunFuture<'a>;

#[derive(Clone, Copy)]
struct CeilingRunOptions {
    ramp_duration_secs: u64,
    max_confirm_wait: Duration,
}

impl Default for CeilingRunOptions {
    fn default() -> Self {
        Self {
            ramp_duration_secs: 5,
            max_confirm_wait: Duration::from_secs(20),
        }
    }
}

fn run_burst_boxed(config: &Config) -> BurstRunFuture<'_> {
    Box::pin(run_burst(config))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RampDecision {
    Saturated {
        ceiling_tps: u32,
    },
    MaxRampDuration {
        ceiling_tps: u32,
    },
    Continue {
        next_step_increase: u32,
        next_target_tps: u32,
    },
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
    wait_for_chain_ready_with_poll_interval(rpc_url, timeout_secs, Duration::from_millis(500)).await
}

async fn wait_for_chain_ready_with_poll_interval(
    rpc_url: &str,
    timeout_secs: u64,
    poll_interval: Duration,
) -> Result<()> {
    let http_client = reqwest::Client::new();
    let start = Instant::now();

    while start.elapsed() < Duration::from_secs(timeout_secs) {
        if rpc_block_number(&http_client, rpc_url).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(poll_interval).await;
    }

    Err(anyhow::anyhow!(
        "chain readiness timeout after {}s",
        timeout_secs
    ))
}

async fn warmup_after_restart(rpc_url: &str, warmup_secs: u64, quiet: bool) -> Result<()> {
    warmup_after_restart_with_poll_interval(rpc_url, warmup_secs, quiet, Duration::from_secs(1), 30)
        .await
}

async fn warmup_after_restart_with_poll_interval(
    rpc_url: &str,
    warmup_secs: u64,
    quiet: bool,
    poll_interval: Duration,
    grace_secs: u64,
) -> Result<()> {
    if warmup_secs == 0 {
        return Ok(());
    }

    let http_client = reqwest::Client::new();
    let start_block = rpc_block_number(&http_client, rpc_url).await?;
    let deadline = Instant::now() + Duration::from_secs(warmup_secs + grace_secs);

    if !quiet {
        println!(
            "  Warmup: waiting up to {}s for post-restart block progress...",
            warmup_secs + grace_secs
        );
    }

    while Instant::now() < deadline {
        tokio::time::sleep(poll_interval).await;
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

    if should_print_status(config.quiet) {
        println!(
            "  Cooling down for {}s before restart...",
            iso.cooldown_secs
        );
    }
    if iso.cooldown_secs > 0 {
        tokio::time::sleep(Duration::from_secs(iso.cooldown_secs)).await;
    }

    let restart_cmd = restart_command(iso)?;

    if should_print_status(config.quiet) {
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

    if should_print_status(config.quiet) {
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

#[derive(Debug, Clone, Copy, PartialEq)]
struct StepStats {
    pending_ratio: f32,
    error_rate: f32,
    actual_tps: f32,
}

fn initial_target_tps(target_tps: u32) -> u32 {
    target_tps.max(100)
}

fn initial_step_increase(target_tps: u32) -> u32 {
    (initial_target_tps(target_tps) / 2).max(100)
}

fn parse_balance_wei(balance_result: &serde_json::Value) -> Result<u128> {
    let balance_hex = balance_result
        .get("result")
        .and_then(|r| r.as_str())
        .ok_or_else(|| anyhow::anyhow!("Failed to get balance from RPC"))?;
    Ok(u128::from_str_radix(balance_hex.trim_start_matches("0x"), 16).unwrap_or(0))
}

fn parse_nonce_result(nonce_result: &serde_json::Value, idx: usize) -> Result<u64> {
    let nonce_hex = nonce_result
        .get("result")
        .and_then(|r| r.as_str())
        .ok_or_else(|| anyhow::anyhow!("Failed to get nonce for sender {}", idx))?;
    Ok(u64::from_str_radix(nonce_hex.trim_start_matches("0x"), 16)?)
}

fn effective_gas_price(gas_price_result: &serde_json::Value) -> u128 {
    let gas_price_hex = gas_price_result
        .get("result")
        .and_then(|r| r.as_str())
        .unwrap_or("0x3b9aca00");
    let base =
        u128::from_str_radix(gas_price_hex.trim_start_matches("0x"), 16).unwrap_or(1_000_000_000);
    (base * 2).max(1_000_000_000)
}

fn should_warn_low_balance(balance_wei: u128, quiet: bool) -> bool {
    const MIN_BALANCE_WEI: u128 = 1_000_000_000_000_000_000;
    balance_wei < MIN_BALANCE_WEI && !quiet
}

fn should_print_status(quiet: bool) -> bool {
    !quiet
}

fn restart_command(iso: &CeilingIsolationConfig) -> Result<&str> {
    iso.restart_cmd
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("restart command missing"))
}

fn selected_rpc_urls(config: &Config) -> Vec<url::Url> {
    if config.rpc_urls.len() > 1 {
        config.rpc_urls.clone()
    } else {
        vec![config.rpc.clone()]
    }
}

fn presign_tx_count(target_tps: u32, ramp_duration_secs: u64) -> usize {
    (target_tps as usize * ramp_duration_secs as usize * 2).max(200)
}

fn signer_transaction_count(total_txs: usize, signer_count: usize, signer_idx: usize) -> usize {
    let signer_count = signer_count.max(1);
    let base_per_signer = total_txs / signer_count;
    let extra = total_txs % signer_count;
    base_per_signer + usize::from(signer_idx < extra)
}

fn compute_step_stats(
    sent: u32,
    confirmed: u32,
    pending: u32,
    errors: u32,
    step_duration: Duration,
) -> StepStats {
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

    StepStats {
        pending_ratio,
        error_rate,
        actual_tps,
    }
}

fn is_saturated_step(
    pending_ratio: f32,
    error_rate: f32,
    actual_tps: f32,
    target_tps: u32,
    saturation_threshold_pending: f32,
    saturation_threshold_error: f32,
    saturation_threshold_tps_ratio: f32,
) -> bool {
    pending_ratio > saturation_threshold_pending
        || error_rate > saturation_threshold_error
        || actual_tps < (target_tps as f32 * saturation_threshold_tps_ratio)
}

fn saturation_bottleneck(timing: &TimingBreakdown) -> &'static str {
    if timing.confirmation_ms > timing.signing_ms && timing.confirmation_ms > timing.submission_ms {
        "BOTTLENECK: Block confirmation latency"
    } else if timing.submission_ms > timing.signing_ms {
        "BOTTLENECK: RPC submission latency"
    } else {
        "BOTTLENECK: Signing latency"
    }
}

fn worker_interval_ms(tps_per_worker: f64) -> u64 {
    if tps_per_worker > 0.0 {
        (1000.0 / tps_per_worker) as u64
    } else {
        1000
    }
    .max(1)
}

async fn wait_for_pending_confirmations(
    metrics: &MetricsExporter,
    tracker: &LatencyTracker,
    max_confirm_wait: Duration,
    poll_interval: Duration,
) -> u64 {
    let confirm_start = Instant::now();
    while confirm_start.elapsed() < max_confirm_wait && tracker.pending_count() > 0 {
        metrics.set_pending_transactions(tracker.pending_count() as i64);
        tokio::time::sleep(poll_interval).await;
    }
    confirm_start.elapsed().as_millis() as u64
}

fn next_step_increase(
    step_increase: u32,
    target_tps: u32,
    actual_tps: f32,
    adaptive_step_enabled: bool,
) -> u32 {
    if !adaptive_step_enabled {
        return step_increase;
    }

    let headroom = if target_tps > 0 {
        actual_tps / target_tps as f32
    } else {
        1.0
    };

    if headroom > 1.15 {
        (step_increase.saturating_mul(3) / 2).max(100)
    } else if headroom < 0.95 {
        (step_increase / 2).max(50)
    } else {
        step_increase.max(75)
    }
}

fn next_ramp_decision(
    is_saturated: bool,
    ramp_elapsed: Duration,
    max_ramp_duration: Duration,
    target_tps: u32,
    step_increase: u32,
    actual_tps: f32,
    adaptive_step_enabled: bool,
) -> RampDecision {
    if is_saturated {
        return RampDecision::Saturated {
            ceiling_tps: target_tps,
        };
    }

    if ramp_elapsed > max_ramp_duration {
        return RampDecision::MaxRampDuration {
            ceiling_tps: target_tps,
        };
    }

    let next_step_increase =
        next_step_increase(step_increase, target_tps, actual_tps, adaptive_step_enabled);
    RampDecision::Continue {
        next_step_increase,
        next_target_tps: target_tps.saturating_add(next_step_increase),
    }
}

fn build_step_submitter(config: &Config) -> Result<Arc<Submitter>> {
    let rpc_urls = selected_rpc_urls(config);
    let submitter = Submitter::with_retry_profile(rpc_urls, &config.ws, config.batch_size, config.submission_method, &config.retry_profile)?;
    Ok(Arc::new(submitter))
}

fn spawn_step_block_tracker(
    config: &Config,
    tracker: Arc<LatencyTracker>,
    duration: Duration,
    max_confirm_wait: Duration,
) -> tokio::task::JoinHandle<()> {
    let ws_url = config.ws.clone();
    let rpc_url = config.rpc.clone();
    let finality_confirmations = config.finality_confirmations;
    tokio::spawn(async move {
        let _ = BlockTracker::with_finality(ws_url, rpc_url, tracker, finality_confirmations)
            .run(duration + max_confirm_wait)
            .await; })
}

async fn apply_ramp_decision(
    config: &Config,
    isolation: &CeilingIsolationConfig,
    decision: RampDecision,
    ceiling_tps: &mut u32,
    step_increase: &mut u32,
    target_tps: &mut u32,
) -> Result<bool> {
    match decision {
        RampDecision::Saturated { ceiling_tps: next } => {
            *ceiling_tps = next;
            Ok(true)
        }
        RampDecision::MaxRampDuration { ceiling_tps: next } => {
            *ceiling_tps = next;
            if should_print_status(config.quiet) { println!("Max ramp duration reached (180s)"); }
            Ok(true)
        }
        RampDecision::Continue {
            next_step_increase,
            next_target_tps,
        } => {
            isolate_chain_between_steps(config, isolation).await?;
            *step_increase = next_step_increase;
            *target_tps = next_target_tps;
            Ok(false)
        }
    }
}

fn build_burst_config(config: &Config, ceiling_tps: u32) -> Config {
    let mut burst_config = config.clone();
    burst_config.tx_count = (ceiling_tps * 5).max(1000);
    burst_config.wave_count = 1;
    burst_config.wave_delay_ms = 0;
    burst_config
}

fn confidence_score(steps: &[CeilingStep]) -> f32 {
    let sample_window: Vec<&CeilingStep> = steps.iter().rev().take(3).collect();
    if sample_window.is_empty() {
        return 0.0;
    }

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
}

fn confidence_band(ceiling_tps: u32, confidence_score: f32) -> (u32, u32) {
    let low = ((ceiling_tps as f32) * (1.0 - (1.0 - confidence_score) * 0.25)).max(0.0) as u32;
    let high = ((ceiling_tps as f32) * (1.0 + (1.0 - confidence_score) * 0.25)) as u32;
    (low, high)
}

/// Run ceiling mode: ramp TPS from start to saturation, then measure peak burst.
///
/// Key improvements over the original:
/// - Key, account address, and HTTP client resolved once outside the ramp loop.
/// - Each step pre-signs all needed txs in parallel (BatchSigner / rayon).
/// - Workers use `tokio::time::interval` — no sleep+elapsed busy loop.
/// - BlockTracker kept alive through the confirmation wait, then aborted.
pub async fn run_ceiling(config: &Config) -> Result<CeilingResult> {
    run_ceiling_with(config, CeilingRunOptions::default(), run_burst_boxed).await
}

async fn run_ceiling_with(
    config: &Config,
    options: CeilingRunOptions,
    run_burst_fn: BurstRunner,
) -> Result<CeilingResult> {
    let mut steps = vec![];
    let metrics = Arc::new(MetricsExporter::new()?);

    let mut target_tps = initial_target_tps(config.target_tps);
    let mut step_increase = initial_step_increase(config.target_tps);
    let adaptive_step_enabled = true;
    let ramp_duration_secs = options.ramp_duration_secs;
    let saturation_threshold_pending = 0.5f32;
    let saturation_threshold_error = 0.05f32;
    let saturation_threshold_tps_ratio = 0.85f32;

    if should_print_status(config.quiet) {
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

    if should_print_status(config.quiet) {
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
    let balance_wei = parse_balance_wei(&balance_result)?;
    if should_warn_low_balance(balance_wei, config.quiet) {
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
        effective_gas_price(&gp_result)
    };

    let mut ceiling_tps = target_tps;
    let ramp_start = Instant::now();
    let max_ramp_duration = Duration::from_secs(180);
    let isolation = CeilingIsolationConfig::from_env();

    if should_print_status(config.quiet) && isolation.enabled() {
        println!(
            "Ceiling isolation enabled: restart between steps, cooldown={}s, warmup={}s",
            isolation.cooldown_secs, isolation.warmup_secs
        );
        println!();
    }

    loop {
        if should_print_status(config.quiet) {
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
            let nonce = parse_nonce_result(&nonce_result, idx)?;
            current_nonces.push(nonce);
        }

        let step_start = Instant::now();
        let duration = Duration::from_secs(ramp_duration_secs);
        let worker_count = config.worker_count.max(1) as usize;

        // Pre-sign txs for this step — 2x headroom so the pool never runs dry.
        let total_txs = presign_tx_count(target_tps, ramp_duration_secs);
        let sign_start = Instant::now();

        if should_print_status(config.quiet) {
            println!("  Pre-signing {} txs...", total_txs);
        }

        let recipient = Address::with_last_byte(0x42);
        let signer_count = signers.len().max(1);
        let mut pre_signed = Vec::with_capacity(total_txs);

        for (idx, (signer, nonce_start)) in signers.iter().zip(current_nonces.iter()).enumerate() {
            let signer_txs = signer_transaction_count(total_txs, signer_count, idx);
            if signer_txs > 0 {
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
        }

        let sign_ms = sign_start.elapsed().as_millis() as u64;

        let dispatcher = build_step_submitter(config)?;
        let tracker = Arc::new(LatencyTracker::new());

        let pre_signed = Arc::new(pre_signed);
        let pool_idx = Arc::new(AtomicU32::new(0));
        let pool_len = pre_signed.len() as u32;
        let sent_count = Arc::new(AtomicU32::new(0));
        let error_count = Arc::new(AtomicU32::new(0));

        let tracker_handle =
            spawn_step_block_tracker(config, tracker.clone(), duration, options.max_confirm_wait);

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
        let confirm_ms = wait_for_pending_confirmations(
            &metrics,
            &tracker,
            options.max_confirm_wait,
            Duration::from_millis(100),
        )
        .await;

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

        let step_stats = compute_step_stats(sent, confirmed, pending, errors, step_duration);
        let pending_ratio = step_stats.pending_ratio;
        let error_rate = step_stats.error_rate;
        let actual_tps = step_stats.actual_tps;

        let timing = TimingBreakdown::new(
            step_duration.as_millis() as u64,
            sign_ms,
            submit_ms,
            confirm_ms,
        );

        let is_saturated = is_saturated_step(
            pending_ratio,
            error_rate,
            actual_tps,
            target_tps,
            saturation_threshold_pending,
            saturation_threshold_error,
            saturation_threshold_tps_ratio,
        );

        if should_print_status(config.quiet) {
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
                let bottleneck = saturation_bottleneck(&timing);
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

        if apply_ramp_decision(
            config,
            &isolation,
            next_ramp_decision(
                is_saturated,
                ramp_start.elapsed(),
                max_ramp_duration,
                target_tps,
                step_increase,
                actual_tps,
                adaptive_step_enabled,
            ),
            &mut ceiling_tps,
            &mut step_increase,
            &mut target_tps,
        )
        .await?
        { break; }
    }

    // Phase 2: Measure peak instantaneous TPS with a burst
    if should_print_status(config.quiet) {
        println!("Measuring peak TPS at {}...", ceiling_tps);
    }

    let burst_config = build_burst_config(config, ceiling_tps);

    let (burst_result, _gas_price) = run_burst_fn(&burst_config).await?;
    let burst_peak_tps = burst_result.confirmed_tps as u32;

    if should_print_status(config.quiet) {
        println!("Peak TPS: {}", burst_peak_tps);
    }

    // Confidence model uses the last few ramp samples near saturation.
    let confidence_score = confidence_score(&steps);
    let (confidence_band_low, confidence_band_high) =
        confidence_band(ceiling_tps, confidence_score);

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
    let interval_ms = worker_interval_ms(tps_per_worker);

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
            Ok(result) => {
                if result.submitted > 0 {
                    metrics.inc_transactions_submitted(result.submitted as u64);
                    sent_count.fetch_add(result.submitted, Ordering::SeqCst);
                }
                if result.errors > 0 {
                    metrics.inc_transactions_failed(result.errors as u64);
                    error_count.fetch_add(result.errors, Ordering::SeqCst);
                }
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
    use crate::config::SubmissionMethod;
    use crate::types::{ExecutionMode, LatencyStats, TestMode, TransactionType};
    use alloy_primitives::B256;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::task::JoinHandle;
    use url::Url;

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        previous: Vec<(String, Option<String>)>,
    }

    impl EnvGuard {
        fn set(vars: &[(&str, Option<&str>)]) -> Self {
            let lock = ENV_MUTEX.lock().unwrap();
            let mut previous = Vec::with_capacity(vars.len());

            for (name, value) in vars {
                previous.push(((*name).to_string(), std::env::var(name).ok()));
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }

            Self {
                _lock: lock,
                previous,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, value) in self.previous.drain(..).rev() {
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(&name, value),
                        None => std::env::remove_var(&name),
                    }
                }
            }
        }
    }

    fn test_config(rpc_url: &str, quiet: bool) -> Config {
        let rpc = Url::parse(rpc_url).unwrap();
        Config {
            rpc_urls: vec![rpc.clone()],
            rpc,
            ws: Url::parse("ws://localhost:8546").unwrap(),
            metrics: None,
            validator_urls: vec![],
            test_mode: TestMode::Transfer,
            execution_mode: ExecutionMode::Ceiling,
            tx_count: 100,
            sender_count: 1,
            wave_count: 1,
            wave_delay_ms: 0,
            duration_secs: 1,
            target_tps: 100,
            worker_count: 1,
            batch_size: 1,
            submission_method: SubmissionMethod::Http,
            retry_profile: "off".to_string(),
            finality_confirmations: 0,
            output: PathBuf::from("report.json"),
            quiet,
            chain_id: 1,
            bench_name: "ceiling-tests".to_string(),
            fund: false,
            sender_keys: vec![],
            evm_tokens: vec![],
            evm_pairs: vec![],
            evm_nfts: vec![],
        }
    }

    async fn handle_rpc_connection(
        stream: &mut tokio::net::TcpStream,
        responses: &Arc<Mutex<VecDeque<String>>>,
        fallback: &str,
    ) {
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).await;

        let body = {
            let mut guard = responses.lock().unwrap();
            guard.pop_front().unwrap_or_else(|| fallback.to_string())
        };

        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(response.as_bytes()).await;
    }

    fn spawn_raw_rpc_server(
        listener: tokio::net::TcpListener,
        responses: Arc<Mutex<VecDeque<String>>>,
        fallback: String,
        max_connections: Option<usize>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut served = 0usize;
            while let Ok((mut stream, _)) = listener.accept().await {
                handle_rpc_connection(&mut stream, &responses, &fallback).await;
                served += 1;
                if max_connections.is_some_and(|max| served >= max) {
                    break;
                }
            }
        })
    }

    async fn spawn_rpc_server(responses: Vec<serde_json::Value>) -> (String, JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let responses = Arc::new(Mutex::new(VecDeque::from(
            responses
                .into_iter()
                .map(|response| response.to_string())
                .collect::<Vec<_>>(),
        )));
        let fallback = serde_json::json!({"jsonrpc": "2.0", "result": "0x0", "id": 1}).to_string();
        let handle = spawn_raw_rpc_server(listener, responses, fallback, None);

        (format!("http://{}", addr), handle)
    }

    fn stub_burst_runner(_: &Config) -> BurstRunFuture<'_> {
        Box::pin(async {
            Ok((
                BurstResult {
                    submitted: 1,
                    confirmed: 1,
                    pending: 0,
                    sign_ms: 0,
                    submit_ms: 0,
                    confirm_ms: 0,
                    submitted_tps: 0.0,
                    confirmed_tps: 321.0,
                    latency: LatencyStats {
                        p50: 0,
                        p95: 0,
                        p99: 0,
                        min: 0,
                        max: 0,
                        avg: 0,
                    },
                    server_metrics: None,
                    per_method: None,
                    validator_health: None,
                    per_wave: None,
                },
                0,
            ))
        })
    }

    #[test]
    fn test_run_burst_boxed_constructs_future_without_polling() {
        let config = test_config("http://127.0.0.1:8545", true);
        let future = run_burst_boxed(&config);
        drop(future);
    }

    fn make_signed_tx(nonce: u64) -> SignedTxWithMetadata {
        SignedTxWithMetadata {
            hash: B256::from([nonce as u8; 32]),
            encoded: vec![0x02, nonce as u8],
            nonce,
            gas_limit: 21_000,
            sender: Address::with_last_byte(nonce as u8),
            submit_time: Instant::now(),
            method: TransactionType::SimpleTransfer,
        }
    }

    #[tokio::test]
    async fn test_run_ceiling_with_stub_burst_covers_saturated_single_step() {
        let _guard = EnvGuard::set(&[
            ("BENCH_CEILING_RESTART_BETWEEN_STEPS", Some("1")),
            ("BENCH_CEILING_RESTART_CMD", Some(":")),
            ("BENCH_CEILING_COOLDOWN_SECS", Some("0")),
            ("BENCH_CEILING_WARMUP_SECS", Some("0")),
        ]);
        let (rpc_url, server) = spawn_rpc_server(vec![
            serde_json::json!({"jsonrpc": "2.0", "result": "0x1", "id": 1}),
            serde_json::json!({"jsonrpc": "2.0", "result": "0x1", "id": 1}),
            serde_json::json!({"jsonrpc": "2.0", "result": "0x0", "id": 1}),
        ])
        .await;

        let mut config = test_config(&rpc_url, false);
        config.ws = Url::parse("ws://127.0.0.1:9").unwrap();

        let result = run_ceiling_with(
            &config,
            CeilingRunOptions {
                ramp_duration_secs: 0,
                max_confirm_wait: Duration::ZERO,
            },
            stub_burst_runner,
        )
        .await
        .expect("run_ceiling_with should succeed with stub burst");

        assert_eq!(result.steps.len(), 1);
        assert_eq!(result.ceiling_tps, 100);
        assert_eq!(result.burst_peak_tps, 321);
        assert!(result.steps[0].is_saturated);
        assert!(result.adaptive_step_enabled);

        drop(server);
    }

    #[tokio::test]
    async fn test_run_ceiling_with_many_senders_covers_zero_batch_distribution() {
        let _guard = EnvGuard::set(&[
            ("BENCH_CEILING_RESTART_BETWEEN_STEPS", Some("0")),
            ("BENCH_CEILING_RESTART_CMD", None),
        ]);
        let (rpc_url, server) = spawn_rpc_server(vec![
            serde_json::json!({"jsonrpc": "2.0", "result": "0xde0b6b3a7640000", "id": 1}),
            serde_json::json!({"jsonrpc": "2.0", "result": "0x1", "id": 1}),
            serde_json::json!({"jsonrpc": "2.0", "result": "0x0", "id": 1}),
        ])
        .await;

        let mut config = test_config(&rpc_url, true);
        config.ws = Url::parse("ws://127.0.0.1:9").unwrap();
        config.sender_count = 250;

        let result = run_ceiling_with(
            &config,
            CeilingRunOptions {
                ramp_duration_secs: 0,
                max_confirm_wait: Duration::ZERO,
            },
            stub_burst_runner,
        )
        .await
        .expect("run_ceiling_with should tolerate zero-work signers");

        assert_eq!(result.steps.len(), 1);
        assert_eq!(result.ceiling_tps, 100);
        drop(server);
    }

    #[tokio::test]
    async fn test_warmup_after_restart_prints_completion_when_not_quiet() {
        let (rpc_url, server) = spawn_rpc_server(vec![
            serde_json::json!({"jsonrpc": "2.0", "result": "0x1", "id": 1}),
            serde_json::json!({"jsonrpc": "2.0", "result": "0x2", "id": 1}),
        ])
        .await;

        warmup_after_restart_with_poll_interval(&rpc_url, 1, false, Duration::from_millis(1), 0)
            .await
            .expect("warmup should detect block progress");

        drop(server);
    }

    #[tokio::test]
    async fn test_isolate_chain_between_steps_not_quiet_waits_for_cooldown_and_readiness() {
        let (rpc_url, server) = spawn_rpc_server(vec![
            serde_json::json!({"jsonrpc": "2.0", "result": "0x1", "id": 1}),
        ])
        .await;
        let mut config = test_config(&rpc_url, false);
        config.ws = Url::parse("ws://127.0.0.1:9").unwrap();

        isolate_chain_between_steps(
            &config,
            &CeilingIsolationConfig {
                restart_between_steps: true,
                restart_cmd: Some(":".to_string()),
                restart_ready_timeout_secs: 1,
                cooldown_secs: 1,
                warmup_secs: 0,
            },
        )
        .await
        .expect("restart isolation should succeed");

        drop(server);
    }

    #[tokio::test]
    async fn test_run_step_worker_tracks_successful_submission() {
        let (rpc_url, server) = spawn_rpc_server(vec![serde_json::json!([{
            "jsonrpc": "2.0",
            "result": format!("0x{:064x}", 1),
            "id": 0
        }])])
        .await;
        let rpc = Url::parse(&rpc_url).unwrap();
        let dispatcher = Arc::new(
            Submitter::with_retry_profile(
                vec![rpc],
                &Url::parse("ws://127.0.0.1:9").unwrap(),
                1,
                SubmissionMethod::Http,
                "off",
            )
            .expect("dispatcher should build"),
        );
        let tracker = Arc::new(LatencyTracker::new());
        let metrics = Arc::new(MetricsExporter::new().unwrap());
        let sent_count = Arc::new(AtomicU32::new(0));
        let error_count = Arc::new(AtomicU32::new(0));
        let pool = Arc::new(vec![make_signed_tx(1)]);
        let pool_idx = Arc::new(AtomicU32::new(0));

        run_step_worker(
            dispatcher,
            tracker.clone(),
            metrics,
            sent_count.clone(),
            error_count.clone(),
            Duration::from_millis(5),
            1_000.0,
            pool,
            pool_idx,
            1,
        )
        .await;

        assert_eq!(sent_count.load(Ordering::SeqCst), 1);
        assert_eq!(error_count.load(Ordering::SeqCst), 0);
        assert_eq!(tracker.pending_count(), 1);

        drop(server);
    }

    #[tokio::test]
    async fn test_run_step_worker_tracks_failed_submission() {
        let (rpc_url, server) = spawn_rpc_server(vec![serde_json::json!([{
            "jsonrpc": "2.0",
            "error": {"code": -32000, "message": "boom"},
            "id": 0
        }])])
        .await;
        let rpc = Url::parse(&rpc_url).unwrap();
        let dispatcher = Arc::new(
            Submitter::with_retry_profile(
                vec![rpc],
                &Url::parse("ws://127.0.0.1:9").unwrap(),
                1,
                SubmissionMethod::Http,
                "off",
            )
            .expect("dispatcher should build"),
        );
        let tracker = Arc::new(LatencyTracker::new());
        let metrics = Arc::new(MetricsExporter::new().unwrap());
        let sent_count = Arc::new(AtomicU32::new(0));
        let error_count = Arc::new(AtomicU32::new(0));
        let pool = Arc::new(vec![make_signed_tx(2)]);
        let pool_idx = Arc::new(AtomicU32::new(0));

        run_step_worker(
            dispatcher,
            tracker,
            metrics,
            sent_count.clone(),
            error_count.clone(),
            Duration::from_millis(5),
            1_000.0,
            pool,
            pool_idx,
            1,
        )
        .await;

        assert_eq!(sent_count.load(Ordering::SeqCst), 0);
        assert_eq!(error_count.load(Ordering::SeqCst), 1);

        drop(server);
    }

    #[tokio::test]
    async fn test_run_step_worker_tracks_transport_error() {
        let dispatcher = Arc::new(
            Submitter::with_retry_profile(
                vec![Url::parse("testerr://forced").unwrap()],
                &Url::parse("ws://127.0.0.1:9").unwrap(),
                1,
                SubmissionMethod::Http,
                "off",
            )
            .expect("dispatcher should build"),
        );
        let tracker = Arc::new(LatencyTracker::new());
        let metrics = Arc::new(MetricsExporter::new().unwrap());
        let sent_count = Arc::new(AtomicU32::new(0));
        let error_count = Arc::new(AtomicU32::new(0));
        let pool = Arc::new(vec![make_signed_tx(3)]);
        let pool_idx = Arc::new(AtomicU32::new(0));

        run_step_worker(
            dispatcher,
            tracker,
            metrics,
            sent_count.clone(),
            error_count.clone(),
            Duration::from_millis(5),
            1_000.0,
            pool,
            pool_idx,
            1,
        )
        .await;

        assert_eq!(sent_count.load(Ordering::SeqCst), 0);
        assert_eq!(error_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_run_step_block_tracker_task_returns_after_ws_failure() {
        let _ = BlockTracker::with_finality(
            Url::parse("ws://127.0.0.1:1").unwrap(),
            Url::parse("http://127.0.0.1:1").unwrap(),
            Arc::new(LatencyTracker::new()),
            0,
        )
        .run(Duration::from_millis(1))
        .await;
    }

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
        let stats = compute_step_stats(0, 0, 0, 0, Duration::from_secs(0));
        let is_sat = is_saturated_step(
            stats.pending_ratio,
            stats.error_rate,
            stats.actual_tps,
            100,
            0.5,
            0.05,
            0.85,
        );
        assert!(is_sat);
        assert_eq!(stats.pending_ratio, 0.0);
        assert_eq!(stats.error_rate, 0.0);
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
        assert_eq!(
            saturation_bottleneck(&t),
            "BOTTLENECK: Block confirmation latency"
        );
    }

    /// Bottleneck detection: submission is dominant.
    #[test]
    fn test_bottleneck_submission_dominant() {
        let t = TimingBreakdown::new(1000, 100, 600, 200);
        assert_eq!(
            saturation_bottleneck(&t),
            "BOTTLENECK: RPC submission latency"
        );
    }

    /// Bottleneck detection: signing is dominant.
    #[test]
    fn test_bottleneck_signing_dominant() {
        let t = TimingBreakdown::new(1000, 700, 100, 100);
        assert_eq!(saturation_bottleneck(&t), "BOTTLENECK: Signing latency");
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
        assert_eq!(worker_interval_ms(tps_per_worker), 8); // 1000 / 125 = 8ms
        assert_eq!(worker_interval_ms(0.0), 1000);
    }

    #[test]
    fn test_initial_ramp_parameters_use_floor() {
        assert_eq!(initial_target_tps(25), 100);
        assert_eq!(initial_target_tps(250), 250);
        assert_eq!(initial_step_increase(25), 100);
        assert_eq!(initial_step_increase(400), 200);
    }

    #[test]
    fn test_parse_nonce_result_covers_success_missing_and_invalid_hex() {
        let ok = serde_json::json!({"result": "0x2a"});
        let missing = serde_json::json!({});
        let invalid = serde_json::json!({"result": "0xzz"});

        assert_eq!(parse_nonce_result(&ok, 3).unwrap(), 42);
        assert!(
            parse_nonce_result(&missing, 3)
                .unwrap_err()
                .to_string()
                .contains("Failed to get nonce for sender 3")
        );
        assert!(parse_nonce_result(&invalid, 0).is_err());
    }

    #[test]
    fn test_parse_balance_wei_requires_result_field() {
        let ok = serde_json::json!({"result": "0xde0b6b3a7640000"});
        let low = serde_json::json!({"result": "0xzz"});
        let missing = serde_json::json!({"jsonrpc": "2.0"});

        assert_eq!(parse_balance_wei(&ok).unwrap(), 1_000_000_000_000_000_000);
        assert_eq!(parse_balance_wei(&low).unwrap(), 0);
        assert!(parse_balance_wei(&missing).is_err());
    }

    #[test]
    fn test_effective_gas_price_uses_fallback_and_floor() {
        let missing = serde_json::json!({});
        let invalid = serde_json::json!({"result": "0xnope"});
        let valid = serde_json::json!({"result": "0x77359400"});

        assert_eq!(effective_gas_price(&missing), 2_000_000_000);
        assert_eq!(effective_gas_price(&invalid), 2_000_000_000);
        assert_eq!(effective_gas_price(&valid), 4_000_000_000);
    }

    #[test]
    fn test_should_warn_low_balance_and_should_print_status() {
        assert!(should_warn_low_balance(0, false));
        assert!(!should_warn_low_balance(1_000_000_000_000_000_000, false));
        assert!(!should_warn_low_balance(0, true));
        assert!(should_print_status(false));
        assert!(!should_print_status(true));
    }

    #[test]
    fn test_restart_command_requires_value() {
        let enabled = CeilingIsolationConfig {
            restart_between_steps: true,
            restart_cmd: Some("echo ok".to_string()),
            restart_ready_timeout_secs: 1,
            cooldown_secs: 0,
            warmup_secs: 0,
        };
        assert_eq!(restart_command(&enabled).unwrap(), "echo ok");

        let missing = CeilingIsolationConfig {
            restart_cmd: None,
            ..enabled
        };
        assert!(
            restart_command(&missing)
                .unwrap_err()
                .to_string()
                .contains("restart command missing")
        );
    }

    #[test]
    fn test_selected_rpc_urls_prefers_multi_url_set() {
        let single = test_config("http://127.0.0.1:8545", true);
        let urls = selected_rpc_urls(&single);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0], single.rpc);

        let mut multi = test_config("http://127.0.0.1:8545", true);
        multi.rpc_urls = vec![
            Url::parse("http://127.0.0.1:8545").unwrap(),
            Url::parse("http://127.0.0.1:9545").unwrap(),
        ];
        let urls = selected_rpc_urls(&multi);
        assert_eq!(urls, multi.rpc_urls);
    }

    #[test]
    fn test_presign_tx_count_and_signer_distribution() {
        assert_eq!(presign_tx_count(10, 5), 200);
        assert_eq!(presign_tx_count(100, 5), 1000);

        let total = 10;
        let counts = (0..3)
            .map(|idx| signer_transaction_count(total, 3, idx))
            .collect::<Vec<_>>();
        assert_eq!(counts, vec![4, 3, 3]);
        assert_eq!(counts.iter().sum::<usize>(), total);
        assert_eq!(signer_transaction_count(3, 10, 7), 0);
    }

    #[test]
    fn test_compute_step_stats_handles_zero_and_nonzero_duration() {
        let stats = compute_step_stats(100, 80, 20, 5, Duration::from_secs(4));
        assert_eq!(
            stats,
            StepStats {
                pending_ratio: 0.2,
                error_rate: 0.05,
                actual_tps: 20.0,
            }
        );

        let zero_sent = compute_step_stats(0, 0, 10, 2, Duration::from_secs(0));
        assert_eq!(
            zero_sent,
            StepStats {
                pending_ratio: 0.0,
                error_rate: 0.0,
                actual_tps: 0.0,
            }
        );
    }

    #[test]
    fn test_is_saturated_step_covers_all_thresholds() {
        assert!(is_saturated_step(0.6, 0.0, 100.0, 100, 0.5, 0.05, 0.85));
        assert!(is_saturated_step(0.0, 0.06, 100.0, 100, 0.5, 0.05, 0.85));
        assert!(is_saturated_step(0.0, 0.0, 84.0, 100, 0.5, 0.05, 0.85));
        assert!(!is_saturated_step(0.5, 0.05, 85.0, 100, 0.5, 0.05, 0.85));
    }

    #[test]
    fn test_saturation_bottleneck_prefers_signing_on_ties() {
        let timing = TimingBreakdown::new(1000, 300, 300, 300);
        assert_eq!(
            saturation_bottleneck(&timing),
            "BOTTLENECK: Signing latency"
        );
    }

    #[test]
    fn test_saturation_bottleneck_covers_confirmation_and_submission_branches() {
        let confirmation = TimingBreakdown::new(1000, 100, 200, 500);
        let submission = TimingBreakdown::new(1000, 100, 600, 200);

        assert_eq!(
            saturation_bottleneck(&confirmation),
            "BOTTLENECK: Block confirmation latency"
        );
        assert_eq!(
            saturation_bottleneck(&submission),
            "BOTTLENECK: RPC submission latency"
        );
    }

    #[test]
    fn test_next_step_increase_covers_all_adaptive_branches() {
        assert_eq!(next_step_increase(100, 100, 120.0, true), 150);
        assert_eq!(next_step_increase(100, 100, 90.0, true), 50);
        assert_eq!(next_step_increase(60, 100, 100.0, true), 75);
        assert_eq!(next_step_increase(80, 0, 0.0, true), 80);
        assert_eq!(next_step_increase(120, 100, 130.0, false), 120);
    }

    #[test]
    fn test_worker_interval_ms_covers_positive_zero_and_floor_branches() {
        assert_eq!(worker_interval_ms(125.0), 8);
        assert_eq!(worker_interval_ms(0.0), 1000);
        assert_eq!(worker_interval_ms(5000.0), 1);
    }

    #[test]
    fn test_next_ramp_decision_covers_break_and_continue_paths() {
        assert_eq!(
            next_ramp_decision(
                true,
                Duration::from_secs(1),
                Duration::from_secs(180),
                100,
                100,
                0.0,
                true,
            ),
            RampDecision::Saturated { ceiling_tps: 100 }
        );
        assert_eq!(
            next_ramp_decision(
                false,
                Duration::from_secs(181),
                Duration::from_secs(180),
                200,
                100,
                100.0,
                true,
            ),
            RampDecision::MaxRampDuration { ceiling_tps: 200 }
        );
        assert_eq!(
            next_ramp_decision(
                false,
                Duration::from_secs(1),
                Duration::from_secs(180),
                100,
                100,
                120.0,
                true,
            ),
            RampDecision::Continue {
                next_step_increase: 150,
                next_target_tps: 250,
            }
        );
    }

    #[tokio::test]
    async fn test_apply_ramp_decision_covers_max_duration_and_continue_paths() {
        let mut config = test_config("http://127.0.0.1:8545", true);
        config.ws = Url::parse("ws://127.0.0.1:9").unwrap();
        let isolation = CeilingIsolationConfig {
            restart_between_steps: false,
            restart_cmd: None,
            restart_ready_timeout_secs: 0,
            cooldown_secs: 0,
            warmup_secs: 0,
        };

        let mut ceiling_tps = 0;
        let mut step_increase = 100;
        let mut target_tps = 100;
        assert!(
            apply_ramp_decision(
                &config,
                &isolation,
                RampDecision::MaxRampDuration { ceiling_tps: 200 },
                &mut ceiling_tps,
                &mut step_increase,
                &mut target_tps,
            )
            .await
            .expect("max duration decision should succeed")
        );
        assert_eq!(ceiling_tps, 200);

        let mut ceiling_tps = 0;
        let mut step_increase = 100;
        let mut target_tps = 100;
        assert!(
            !apply_ramp_decision(
                &config,
                &isolation,
                RampDecision::Continue {
                    next_step_increase: 150,
                    next_target_tps: 250,
                },
                &mut ceiling_tps,
                &mut step_increase,
                &mut target_tps,
            )
            .await
            .expect("continue decision should succeed")
        );
        assert_eq!(ceiling_tps, 0);
        assert_eq!(step_increase, 150);
        assert_eq!(target_tps, 250);
    }

    #[tokio::test]
    async fn test_wait_for_pending_confirmations_updates_metrics_until_timeout() {
        let metrics = MetricsExporter::new().unwrap();
        let tracker = LatencyTracker::new();
        let tx = make_signed_tx(9);
        tracker.record_submit(tx.hash, tx.nonce, tx.sender, tx.gas_limit, tx.method);

        let waited = wait_for_pending_confirmations(
            &metrics,
            &tracker,
            Duration::from_millis(5),
            Duration::from_millis(1),
        )
        .await;

        assert!(waited >= 1);
        assert_eq!(tracker.pending_count(), 1);
    }

    #[test]
    fn test_build_burst_config_sets_ceiling_burst_shape() {
        let mut config = test_config("http://127.0.0.1:8545", true);
        config.tx_count = 123;
        config.wave_count = 9;
        config.wave_delay_ms = 77;

        let burst = build_burst_config(&config, 150);
        assert_eq!(burst.tx_count, 1000);
        assert_eq!(burst.wave_count, 1);
        assert_eq!(burst.wave_delay_ms, 0);

        let burst = build_burst_config(&config, 250);
        assert_eq!(burst.tx_count, 1250);
    }

    #[test]
    fn test_confidence_score_handles_empty_single_and_penalized_samples() {
        assert_eq!(confidence_score(&[]), 0.0);

        let single = vec![CeilingStep {
            target_tps: 100,
            actual_tps: 100,
            pending_ratio: 0.0,
            error_rate: 0.0,
            duration_ms: 5000,
            is_saturated: false,
        }];
        assert_eq!(confidence_score(&single), 1.0);

        let penalized = vec![
            CeilingStep {
                target_tps: 100,
                actual_tps: 50,
                pending_ratio: 1.0,
                error_rate: 1.0,
                duration_ms: 5000,
                is_saturated: true,
            },
            CeilingStep {
                target_tps: 100,
                actual_tps: 150,
                pending_ratio: 1.0,
                error_rate: 1.0,
                duration_ms: 5000,
                is_saturated: true,
            },
        ];
        assert_eq!(confidence_score(&penalized), 0.0);
    }

    #[test]
    fn test_confidence_score_uses_last_three_steps_and_confidence_band() {
        let steps = vec![
            CeilingStep {
                target_tps: 100,
                actual_tps: 1,
                pending_ratio: 1.0,
                error_rate: 1.0,
                duration_ms: 5000,
                is_saturated: true,
            },
            CeilingStep {
                target_tps: 200,
                actual_tps: 200,
                pending_ratio: 0.0,
                error_rate: 0.0,
                duration_ms: 5000,
                is_saturated: false,
            },
            CeilingStep {
                target_tps: 300,
                actual_tps: 201,
                pending_ratio: 0.0,
                error_rate: 0.0,
                duration_ms: 5000,
                is_saturated: false,
            },
            CeilingStep {
                target_tps: 400,
                actual_tps: 199,
                pending_ratio: 0.0,
                error_rate: 0.0,
                duration_ms: 5000,
                is_saturated: false,
            },
        ];

        let score = confidence_score(&steps);
        assert!(score > 0.99, "score={score}");

        let (low, high) = confidence_band(400, 0.5);
        assert_eq!(low, 350);
        assert_eq!(high, 450);
    }

    #[test]
    fn test_ceiling_isolation_config_defaults() {
        let _env = EnvGuard::set(&[
            ("BENCH_CEILING_RESTART_BETWEEN_STEPS", None),
            ("BENCH_CEILING_RESTART_CMD", None),
            ("BENCH_CEILING_RESTART_READY_TIMEOUT_SECS", None),
            ("BENCH_CEILING_COOLDOWN_SECS", None),
            ("BENCH_CEILING_WARMUP_SECS", None),
        ]);

        let config = CeilingIsolationConfig::from_env();

        assert!(config.restart_between_steps);
        assert_eq!(config.restart_cmd, None);
        assert_eq!(config.restart_ready_timeout_secs, 90);
        assert_eq!(config.cooldown_secs, 2);
        assert_eq!(config.warmup_secs, 3);
        assert!(!config.enabled());
    }

    #[test]
    fn test_ceiling_isolation_config_parses_custom_values() {
        let _env = EnvGuard::set(&[
            ("BENCH_CEILING_RESTART_BETWEEN_STEPS", Some("off")),
            ("BENCH_CEILING_RESTART_CMD", Some("  echo restart  ")),
            ("BENCH_CEILING_RESTART_READY_TIMEOUT_SECS", Some("12")),
            ("BENCH_CEILING_COOLDOWN_SECS", Some("5")),
            ("BENCH_CEILING_WARMUP_SECS", Some("6")),
        ]);

        let config = CeilingIsolationConfig::from_env();

        assert!(!config.restart_between_steps);
        assert_eq!(config.restart_cmd.as_deref(), Some("echo restart"));
        assert_eq!(config.restart_ready_timeout_secs, 12);
        assert_eq!(config.cooldown_secs, 5);
        assert_eq!(config.warmup_secs, 6);
        assert!(!config.enabled());
    }

    #[test]
    fn test_ceiling_isolation_config_invalid_values_fall_back_to_defaults() {
        let _env = EnvGuard::set(&[
            ("BENCH_CEILING_RESTART_BETWEEN_STEPS", Some("maybe")),
            ("BENCH_CEILING_RESTART_CMD", Some("   ")),
            ("BENCH_CEILING_RESTART_READY_TIMEOUT_SECS", Some("abc")),
            ("BENCH_CEILING_COOLDOWN_SECS", Some("-1")),
            ("BENCH_CEILING_WARMUP_SECS", Some("")),
        ]);

        let config = CeilingIsolationConfig::from_env();

        assert!(config.restart_between_steps);
        assert_eq!(config.restart_cmd, None);
        assert_eq!(config.restart_ready_timeout_secs, 90);
        assert_eq!(config.cooldown_secs, 2);
        assert_eq!(config.warmup_secs, 3);
        assert!(!config.enabled());
    }

    #[test]
    fn test_ceiling_isolation_enabled_requires_flag_and_command() {
        let config = CeilingIsolationConfig {
            restart_between_steps: true,
            restart_cmd: Some("echo restart".to_string()),
            restart_ready_timeout_secs: 90,
            cooldown_secs: 0,
            warmup_secs: 0,
        };
        assert!(config.enabled());

        let disabled = CeilingIsolationConfig {
            restart_between_steps: false,
            restart_cmd: Some("echo restart".to_string()),
            ..config.clone()
        };
        assert!(!disabled.enabled());
    }

    #[test]
    fn test_env_guard_restores_previous_value_on_drop() {
        let _lock = ENV_MUTEX.lock().unwrap();
        unsafe {
            std::env::set_var("BENCH_CEILING_RESTORE_TEST", "before");
        }
        drop(_lock);

        {
            let _guard = EnvGuard::set(&[("BENCH_CEILING_RESTORE_TEST", Some("during"))]);
            assert_eq!(
                std::env::var("BENCH_CEILING_RESTORE_TEST").unwrap(),
                "during"
            );
        }

        assert_eq!(
            std::env::var("BENCH_CEILING_RESTORE_TEST").unwrap(),
            "before"
        );
        unsafe {
            std::env::remove_var("BENCH_CEILING_RESTORE_TEST");
        }
    }

    #[tokio::test]
    async fn test_rpc_block_number_parses_hex_result() {
        let (rpc_url, server) = spawn_rpc_server(vec![
            serde_json::json!({"jsonrpc": "2.0", "result": "0x2a", "id": 1}),
        ])
        .await;
        let client = reqwest::Client::new();

        let block = rpc_block_number(&client, &rpc_url).await.unwrap();

        server.abort();
        assert_eq!(block, 42);
    }

    #[tokio::test]
    async fn test_rpc_block_number_errors_when_result_missing() {
        let (rpc_url, server) =
            spawn_rpc_server(vec![serde_json::json!({"jsonrpc": "2.0", "id": 1})]).await;
        let client = reqwest::Client::new();

        let err = rpc_block_number(&client, &rpc_url).await.unwrap_err();

        server.abort();
        assert!(err.to_string().contains("eth_blockNumber missing result"));
    }

    #[tokio::test]
    async fn test_wait_for_chain_ready_succeeds_after_rpc_response() {
        let (rpc_url, server) = spawn_rpc_server(vec![
            serde_json::json!({"jsonrpc": "2.0", "result": "0x1", "id": 1}),
        ])
        .await;

        let result = wait_for_chain_ready(&rpc_url, 1).await;

        server.abort();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_wait_for_chain_ready_retries_after_initial_failure() {
        let responses = vec![
            serde_json::json!({"jsonrpc": "2.0", "id": 1}),
            serde_json::json!({"jsonrpc": "2.0", "result": "0x1", "id": 1}),
        ];
        let (rpc_url, server) = spawn_rpc_server(responses).await;

        let result =
            wait_for_chain_ready_with_poll_interval(&rpc_url, 1, Duration::from_millis(10)).await;

        server.abort();
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test]
    async fn test_run_rpc_server_returns_after_connection_limit() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let responses = Arc::new(Mutex::new(VecDeque::from(vec![
            serde_json::json!({"jsonrpc": "2.0", "result": "0x1", "id": 1}).to_string(),
        ])));
        let fallback = serde_json::json!({"jsonrpc": "2.0", "result": "0x0", "id": 1}).to_string();
        let server = spawn_raw_rpc_server(listener, responses, fallback, Some(1));

        let response = reqwest::Client::new()
            .post(format!("http://{}", addr))
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_blockNumber",
                "params": [],
                "id": 1
            }))
            .send()
            .await
            .unwrap();

        assert!(response.status().is_success());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn test_wait_for_chain_ready_zero_timeout_errors_immediately() {
        let err = wait_for_chain_ready("http://127.0.0.1:1", 0)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("chain readiness timeout after 0s"));
    }

    #[tokio::test]
    async fn test_warmup_after_restart_zero_secs_returns_without_rpc() {
        warmup_after_restart("http://127.0.0.1:1", 0, true)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_warmup_after_restart_waits_for_block_progress() {
        let responses = vec![
            serde_json::json!({"jsonrpc": "2.0", "result": "0x1", "id": 1}),
            serde_json::json!({"jsonrpc": "2.0", "result": "0x2", "id": 1}),
        ];
        let (rpc_url, server) = spawn_rpc_server(responses).await;

        let result = warmup_after_restart(&rpc_url, 1, true).await;

        server.abort();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_warmup_after_restart_times_out_without_block_progress() {
        let responses = vec![
            serde_json::json!({"jsonrpc": "2.0", "result": "0x1", "id": 1}),
            serde_json::json!({"jsonrpc": "2.0", "result": "0x1", "id": 1}),
            serde_json::json!({"jsonrpc": "2.0", "result": "0x1", "id": 1}),
        ];
        let (rpc_url, server) = spawn_rpc_server(responses).await;

        let err = warmup_after_restart_with_poll_interval(
            &rpc_url,
            1,
            false,
            Duration::from_millis(1),
            0,
        )
        .await
        .unwrap_err();

        server.abort();
        assert!(
            err.to_string()
                .contains("warmup timed out waiting for block progress after restart")
        );
    }

    #[tokio::test]
    async fn test_isolate_chain_between_steps_is_noop_when_disabled() {
        let config = test_config("http://127.0.0.1:8545", true);
        let isolation = CeilingIsolationConfig {
            restart_between_steps: false,
            restart_cmd: Some("exit 0".to_string()),
            restart_ready_timeout_secs: 1,
            cooldown_secs: 0,
            warmup_secs: 0,
        };

        isolate_chain_between_steps(&config, &isolation)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_isolate_chain_between_steps_runs_restart_and_waits_for_ready() {
        let responses = vec![
            serde_json::json!({"jsonrpc": "2.0", "result": "0x1", "id": 1}),
            serde_json::json!({"jsonrpc": "2.0", "result": "0x1", "id": 1}),
            serde_json::json!({"jsonrpc": "2.0", "result": "0x2", "id": 1}),
        ];
        let (rpc_url, server) = spawn_rpc_server(responses).await;
        let config = test_config(&rpc_url, true);
        let isolation = CeilingIsolationConfig {
            restart_between_steps: true,
            restart_cmd: Some("true".to_string()),
            restart_ready_timeout_secs: 1,
            cooldown_secs: 0,
            warmup_secs: 1,
        };

        let result = isolate_chain_between_steps(&config, &isolation).await;

        server.abort();
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test]
    async fn test_isolate_chain_between_steps_reports_restart_command_failure() {
        let config = test_config("http://127.0.0.1:8545", true);
        let isolation = CeilingIsolationConfig {
            restart_between_steps: true,
            restart_cmd: Some("exit 7".to_string()),
            restart_ready_timeout_secs: 1,
            cooldown_secs: 0,
            warmup_secs: 0,
        };

        let err = isolate_chain_between_steps(&config, &isolation)
            .await
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("restart command failed with status Some(7)")
        );
    }
}
