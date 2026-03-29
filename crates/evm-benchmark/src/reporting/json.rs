use crate::config::Config;
use crate::types::{
    BenchmarkReport, BurstResult, CeilingAnalysis, CeilingResult, ConfigSnapshot,
    CostEfficiencyReport,
};
use anyhow::Result;
use chrono::Utc;
use std::path::{Path, PathBuf};

fn estimate_avg_gas_per_confirmed(result: &BurstResult) -> u64 {
    if let Some(per_method) = &result.per_method {
        let mut total_gas: u128 = 0;
        let mut total_confirmed: u128 = 0;
        for stats in per_method.values() {
            total_gas =
                total_gas.saturating_add((stats.avg_gas as u128) * (stats.confirmed as u128));
            total_confirmed = total_confirmed.saturating_add(stats.confirmed as u128);
        }
        if total_confirmed > 0 {
            return (total_gas / total_confirmed) as u64;
        }
    }
    21_000
}

fn build_replay_pack_path(output_path: &Path) -> PathBuf {
    let stem = output_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("report");
    let replay_name = format!("{}.replay.json", stem);
    output_path.with_file_name(replay_name)
}

pub async fn write_report(
    config: &Config,
    result: &BurstResult,
    output_path: &Path,
    ceiling_result: Option<&CeilingResult>,
    effective_gas_price_wei: Option<u128>,
) -> Result<()> {
    let snapshot = ConfigSnapshot {
        test_mode: format!("{:?}", config.test_mode).to_lowercase(),
        execution_mode: format!("{:?}", config.execution_mode).to_lowercase(),
        tx_count: config.tx_count,
        sender_count: config.sender_count,
        wave_count: config.wave_count,
        wave_delay_ms: config.wave_delay_ms,
        worker_count: config.worker_count,
    };

    let ceiling_analysis = ceiling_result.map(|c| CeilingAnalysis {
        confidence_score: c.confidence_score,
        confidence_band_low: c.confidence_band_low,
        confidence_band_high: c.confidence_band_high,
        adaptive_step_enabled: c.adaptive_step_enabled,
        sampled_steps: c.steps.len(),
    });

    let cost_efficiency = effective_gas_price_wei
        .or_else(|| {
            std::env::var("BENCH_GAS_PRICE_WEI")
                .ok()
                .and_then(|v| v.parse::<u128>().ok())
        })
        .filter(|v| *v > 0)
        .map(|gas_price_wei| {
            let avg_gas = estimate_avg_gas_per_confirmed(result) as u128;
            let estimated_total_gas = (result.confirmed as u128).saturating_mul(avg_gas);
            let total_fee_wei = estimated_total_gas.saturating_mul(gas_price_wei);
            let fee_eth = total_fee_wei as f64 / 1e18_f64;
            let confirmed_per_eth = if fee_eth > 0.0 {
                result.confirmed as f64 / fee_eth
            } else {
                0.0
            };

            CostEfficiencyReport {
                estimated_total_gas: estimated_total_gas.min(u64::MAX as u128) as u64,
                estimated_total_fee_wei: total_fee_wei.to_string(),
                estimated_total_fee_eth: fee_eth,
                confirmed_per_eth,
            }
        });

    let replay_path = build_replay_pack_path(output_path);
    let replay_manifest = serde_json::json!({
        "version": 1,
        "captured_at": Utc::now().to_rfc3339(),
        "benchmark": config.bench_name,
        "chain_id": config.chain_id,
        "config": snapshot,
        "rpc_endpoints": config.rpc_urls.iter().map(|u| u.to_string()).collect::<Vec<_>>(),
        "ws": config.ws.to_string(),
        "env": {
            "BENCH_KEY": if config.sender_keys.is_empty() { None } else { Some("<redacted>") },
            "BENCH_RETRY_PROFILE": Some(&config.retry_profile),
            "BENCH_FINALITY_CONFIRMATIONS": Some(config.finality_confirmations.to_string()),
            "BENCH_PREFLIGHT_STRICT": std::env::var("BENCH_PREFLIGHT_STRICT").ok(),
        }
    });
    tokio::fs::write(
        &replay_path,
        serde_json::to_string_pretty(&replay_manifest)?,
    )
    .await?;

    let report = BenchmarkReport {
        benchmark: config.bench_name.clone(),
        captured_at: Utc::now().to_rfc3339(),
        chain_id: config.chain_id,
        config: snapshot,
        results: result.clone(),
        ceiling_analysis,
        cost_efficiency,
        replay_pack: replay_path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string()),
    };

    let json = serde_json::to_string_pretty(&report)?;
    tokio::fs::write(output_path, json).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, SubmissionMethod};
    use crate::types::{ExecutionMode, LatencyStats, TestMode};
    use std::path::PathBuf;
    use url::Url;

    fn make_test_config() -> Config {
        Config {
            rpc_urls: vec![Url::parse("http://localhost:8545").unwrap()],
            rpc: Url::parse("http://localhost:8545").unwrap(),
            ws: Url::parse("ws://localhost:8546").unwrap(),
            metrics: None,
            validator_urls: vec![],
            test_mode: TestMode::Transfer,
            execution_mode: ExecutionMode::Burst,
            tx_count: 100,
            sender_count: 4,
            wave_count: 2,
            wave_delay_ms: 0,
            duration_secs: 60,
            target_tps: 100,
            worker_count: 8,
            batch_size: 50,
            submission_method: SubmissionMethod::Http,
            retry_profile: "light".to_string(),
            finality_confirmations: 0,
            output: PathBuf::from("test.json"),
            quiet: true,
            chain_id: 19803,
            bench_name: "test_bench".to_string(),
            fund: false,
            sender_keys: vec![],
            evm_tokens: vec![],
            evm_pairs: vec![],
            evm_nfts: vec![],
        }
    }

    fn make_test_result() -> BurstResult {
        BurstResult {
            submitted: 100,
            confirmed: 95,
            pending: 5,
            sign_ms: 10,
            submit_ms: 500,
            confirm_ms: 1000,
            submitted_tps: 200.0,
            confirmed_tps: 190.0,
            latency: LatencyStats {
                p50: 50,
                p95: 150,
                p99: 300,
                min: 5,
                max: 500,
                avg: 80,
            },
            server_metrics: None,
            per_method: None,
            validator_health: None,
            per_wave: None,
        }
    }

    #[tokio::test]
    async fn test_write_report_produces_valid_json() {
        let config = make_test_config();
        let result = make_test_result();

        let dir = std::env::temp_dir();
        let path = dir.join(format!("bench_test_{}.json", std::process::id()));

        write_report(&config, &result, &path, None, None)
            .await
            .unwrap();

        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();

        assert_eq!(parsed["benchmark"], "test_bench");
        assert_eq!(parsed["chain_id"], 19803);
        assert_eq!(parsed["config"]["tx_count"], 100);
        assert_eq!(parsed["config"]["sender_count"], 4);
        assert_eq!(parsed["config"]["wave_count"], 2);
        assert_eq!(parsed["config"]["worker_count"], 8);
        assert_eq!(parsed["results"]["submitted"], 100);
        assert_eq!(parsed["results"]["confirmed"], 95);
        assert!(parsed["captured_at"].as_str().is_some());

        // Cleanup
        let _ = tokio::fs::remove_file(&path).await;
        let replay = path.with_file_name(
            "bench_test_".to_string() + &std::process::id().to_string() + ".replay.json",
        );
        let _ = tokio::fs::remove_file(replay).await;
    }
}
