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
    use crate::types::{
        CeilingResult, CeilingStep, ExecutionMode, LatencyStats, PerMethodStats, TestMode,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::{LazyLock, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};
    use url::Url;

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

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

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "evm-benchmark-{prefix}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn cleanup_report_artifacts(path: &Path) {
        let replay = build_replay_pack_path(path);
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(replay);
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }

    #[tokio::test]
    async fn test_write_report_produces_valid_json() {
        let config = make_test_config();
        let result = make_test_result();

        let dir = unique_temp_dir("report-basic");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bench_test.json");

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

        cleanup_report_artifacts(&path);
    }

    #[test]
    fn test_estimate_avg_gas_per_confirmed_uses_weighted_average() {
        let mut result = make_test_result();
        result.confirmed = 4;
        result.per_method = Some(BTreeMap::from([
            (
                "mint".to_string(),
                PerMethodStats {
                    count: 2,
                    confirmed: 2,
                    reverted: 0,
                    avg_gas: 50_000,
                    latency_p50: 10,
                    latency_p95: 20,
                },
            ),
            (
                "swap".to_string(),
                PerMethodStats {
                    count: 2,
                    confirmed: 2,
                    reverted: 0,
                    avg_gas: 100_000,
                    latency_p50: 10,
                    latency_p95: 20,
                },
            ),
        ]));

        assert_eq!(estimate_avg_gas_per_confirmed(&result), 75_000);
    }

    #[test]
    fn test_estimate_avg_gas_per_confirmed_falls_back_without_confirmed_samples() {
        let mut result = make_test_result();
        result.per_method = Some(BTreeMap::from([(
            "mint".to_string(),
            PerMethodStats {
                count: 3,
                confirmed: 0,
                reverted: 3,
                avg_gas: 80_000,
                latency_p50: 10,
                latency_p95: 20,
            },
        )]));

        assert_eq!(estimate_avg_gas_per_confirmed(&result), 21_000);
    }

    #[test]
    fn test_build_replay_pack_path_swaps_extension() {
        let output = PathBuf::from("/tmp/bench.report.json");
        assert_eq!(
            build_replay_pack_path(&output),
            PathBuf::from("/tmp/bench.report.replay.json")
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_write_report_includes_ceiling_cost_efficiency_and_replay_manifest() {
        let _guard = ENV_LOCK.lock().unwrap();
        let mut config = make_test_config();
        config.sender_keys = vec!["0xabc".to_string()];

        let mut result = make_test_result();
        result.confirmed = 4;
        result.per_method = Some(BTreeMap::from([
            (
                "mint".to_string(),
                PerMethodStats {
                    count: 2,
                    confirmed: 2,
                    reverted: 0,
                    avg_gas: 50_000,
                    latency_p50: 10,
                    latency_p95: 20,
                },
            ),
            (
                "swap".to_string(),
                PerMethodStats {
                    count: 2,
                    confirmed: 2,
                    reverted: 0,
                    avg_gas: 100_000,
                    latency_p50: 10,
                    latency_p95: 20,
                },
            ),
        ]));

        let ceiling = CeilingResult {
            steps: vec![CeilingStep {
                target_tps: 100,
                actual_tps: 95,
                pending_ratio: 0.01,
                error_rate: 0.0,
                duration_ms: 1_000,
                is_saturated: false,
            }],
            ceiling_tps: 95,
            burst_peak_tps: 110,
            confidence_score: 0.92,
            confidence_band_low: 90,
            confidence_band_high: 100,
            adaptive_step_enabled: true,
        };

        unsafe {
            std::env::set_var("BENCH_GAS_PRICE_WEI", "999999");
            std::env::set_var("BENCH_PREFLIGHT_STRICT", "1");
        }

        let dir = unique_temp_dir("report-rich");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("report.json");

        write_report(&config, &result, &path, Some(&ceiling), Some(3))
            .await
            .unwrap();

        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        let replay_contents = tokio::fs::read_to_string(build_replay_pack_path(&path))
            .await
            .unwrap();
        let replay: serde_json::Value = serde_json::from_str(&replay_contents).unwrap();

        assert_eq!(parsed["ceiling_analysis"]["sampled_steps"], 1);
        assert_eq!(parsed["cost_efficiency"]["estimated_total_gas"], 300_000);
        assert_eq!(
            parsed["cost_efficiency"]["estimated_total_fee_wei"],
            "900000"
        );
        assert_eq!(parsed["replay_pack"], "report.replay.json");
        assert_eq!(replay["env"]["BENCH_KEY"], "<redacted>");
        assert_eq!(replay["env"]["BENCH_PREFLIGHT_STRICT"], "1");
        assert_eq!(replay["env"]["BENCH_RETRY_PROFILE"], "light");
        assert_eq!(replay["env"]["BENCH_FINALITY_CONFIRMATIONS"], "0");

        unsafe {
            std::env::remove_var("BENCH_GAS_PRICE_WEI");
            std::env::remove_var("BENCH_PREFLIGHT_STRICT");
        }
        cleanup_report_artifacts(&path);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_write_report_uses_env_gas_price_when_argument_missing() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("BENCH_GAS_PRICE_WEI", "5");
            std::env::remove_var("BENCH_PREFLIGHT_STRICT");
        }

        let config = make_test_config();
        let result = make_test_result();
        let dir = unique_temp_dir("report-env-gas");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("report.json");

        write_report(&config, &result, &path, None, None)
            .await
            .unwrap();

        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();

        assert_eq!(parsed["cost_efficiency"]["estimated_total_gas"], 1_995_000);
        assert_eq!(
            parsed["cost_efficiency"]["estimated_total_fee_wei"],
            "9975000"
        );

        unsafe {
            std::env::remove_var("BENCH_GAS_PRICE_WEI");
        }
        cleanup_report_artifacts(&path);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_write_report_skips_cost_efficiency_for_zero_gas_price() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("BENCH_GAS_PRICE_WEI", "0");
            std::env::remove_var("BENCH_PREFLIGHT_STRICT");
        }

        let config = make_test_config();
        let result = make_test_result();
        let dir = unique_temp_dir("report-zero-gas");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("report.json");

        write_report(&config, &result, &path, None, None)
            .await
            .unwrap();

        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();

        assert!(parsed["cost_efficiency"].is_null());

        unsafe {
            std::env::remove_var("BENCH_GAS_PRICE_WEI");
        }
        cleanup_report_artifacts(&path);
    }

    #[tokio::test]
    async fn test_write_report_sets_confirmed_per_eth_to_zero_when_fee_is_zero() {
        let config = make_test_config();
        let mut result = make_test_result();
        result.confirmed = 0;
        let dir = unique_temp_dir("report-zero-fee");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("report.json");

        write_report(&config, &result, &path, None, Some(1))
            .await
            .unwrap();

        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();

        assert_eq!(parsed["cost_efficiency"]["estimated_total_fee_wei"], "0");
        assert_eq!(parsed["cost_efficiency"]["confirmed_per_eth"], 0.0);

        cleanup_report_artifacts(&path);
    }

    #[tokio::test]
    async fn test_write_report_propagates_file_write_errors() {
        let config = make_test_config();
        let result = make_test_result();
        let dir = unique_temp_dir("report-dir-output");
        std::fs::create_dir_all(&dir).unwrap();

        let err = write_report(&config, &result, &dir, None, None)
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("Is a directory") || err.to_string().contains("os error 21"),
            "unexpected error: {err:#}"
        );
        let _ = std::fs::remove_file(build_replay_pack_path(&dir));
        let _ = std::fs::remove_dir_all(dir);
    }
}
