pub mod burst;
pub mod ceiling;
pub mod sustained;

use crate::config::Config;
use crate::generators::contract_deploy::EvmContracts;
use crate::generators::evm_mix::EvmMixConfig;
use crate::types::{ServerMetrics, ValidatorHealthSnapshot};
use crate::validators::HealthMonitor;

pub use burst::run_burst;
#[allow(unused_imports)]
pub use ceiling::run_ceiling;
pub use sustained::run_sustained;

const BENCH_CONFIRM_WAIT_SECS: &str = "BENCH_CONFIRM_WAIT_SECS";
const BENCH_SERVER_METRICS_AFTER_SETTLE_MS: &str = "BENCH_SERVER_METRICS_AFTER_SETTLE_MS";

pub(crate) fn confirmation_wait_duration(default_secs: u64) -> std::time::Duration {
    std::time::Duration::from_secs(confirmation_wait_secs_from(
        default_secs,
        std::env::var(BENCH_CONFIRM_WAIT_SECS).ok().as_deref(),
    ))
}

fn server_metrics_after_settle_duration() -> std::time::Duration {
    std::time::Duration::from_millis(server_metrics_after_settle_ms_from(
        std::env::var(BENCH_SERVER_METRICS_AFTER_SETTLE_MS)
            .ok()
            .as_deref(),
    ))
}

fn server_metrics_after_settle_ms_from(raw: Option<&str>) -> u64 {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(5_000)
}

pub(crate) fn evm_contracts(config: &Config) -> anyhow::Result<EvmContracts> {
    let contracts = EvmContracts {
        tokens: config.evm_tokens.clone(),
        pairs: config.evm_pairs.clone(),
        nfts: config.evm_nfts.clone(),
    };

    if contracts.tokens.is_empty() {
        anyhow::bail!(
            "EVM mode requires deployed contracts: token contract addresses missing. Use --fund to deploy or set BENCH_EVM_TOKENS."
        );
    }
    if contracts.nfts.is_empty() {
        anyhow::bail!(
            "EVM mode requires deployed contracts: NFT contract addresses missing. Use --fund to deploy or set BENCH_EVM_NFTS."
        );
    }

    Ok(contracts)
}

pub(crate) fn evm_mix_config(contracts: &EvmContracts) -> EvmMixConfig {
    EvmMixConfig {
        token_count: contracts.tokens.len() as u32,
        pair_count: contracts.pairs.len() as u32,
        nft_count: contracts.nfts.len() as u32,
        ..EvmMixConfig::default()
    }
}

pub(crate) async fn scrape_server_metrics_before(
    config: &Config,
) -> Option<crate::metrics::MetricsMap> {
    let metrics_url = config.metrics.as_ref()?;
    crate::metrics::scrape_prometheus(metrics_url).await.ok()
}

pub(crate) async fn scrape_server_metrics_after(
    config: &Config,
    before: Option<&crate::metrics::MetricsMap>,
) -> Option<ServerMetrics> {
    let before = before?;
    let metrics_url = config.metrics.as_ref()?;
    let settle = server_metrics_after_settle_duration();
    if !settle.is_zero() {
        tokio::time::sleep(settle).await;
    }
    let after = crate::metrics::scrape_prometheus(metrics_url).await.ok()?;
    crate::metrics::compute_server_metrics(before, &after)
}

pub(crate) fn start_health_monitor(config: &Config) -> anyhow::Result<Option<HealthMonitor>> {
    if config.validator_urls.is_empty() {
        return Ok(None);
    }
    let mut monitor = HealthMonitor::new(config.validator_urls.clone(), 10)
        .map_err(|e| anyhow::anyhow!("failed to start validator health monitor: {}", e))?;
    monitor
        .start()
        .map_err(|e| anyhow::anyhow!("failed to start validator health monitor: {}", e))?;
    Ok(Some(monitor))
}

pub(crate) fn validator_health_snapshot(
    monitor: Option<&HealthMonitor>,
) -> Option<Vec<ValidatorHealthSnapshot>> {
    let monitor = monitor?;
    monitor.update_latency_percentiles();
    Some(
        monitor
            .get_health_status()
            .into_iter()
            .map(|health| ValidatorHealthSnapshot {
                url: health.url,
                block_height: health.block_height,
                is_synced: health.is_synced,
                availability_percent: health.availability_percent,
                latency_p50_ms: health.latency_p50_ms,
                latency_p95_ms: health.latency_p95_ms,
                latency_p99_ms: health.latency_p99_ms,
                tx_acceptance_rate: health.tx_acceptance_rate,
                error_rate: health.error_rate,
                is_connected: health.is_connected,
            })
            .collect(),
    )
}

fn confirmation_wait_secs_from(default_secs: u64, raw: Option<&str>) -> u64 {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(default_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmation_wait_secs_uses_default_when_unset() {
        assert_eq!(confirmation_wait_secs_from(30, None), 30);
    }

    #[test]
    fn confirmation_wait_secs_uses_valid_override() {
        assert_eq!(confirmation_wait_secs_from(30, Some("90")), 90);
    }

    #[test]
    fn confirmation_wait_secs_ignores_zero_or_invalid_override() {
        assert_eq!(confirmation_wait_secs_from(30, Some("0")), 30);
        assert_eq!(confirmation_wait_secs_from(30, Some("garbage")), 30);
    }

    #[test]
    fn server_metrics_after_settle_ms_defaults_to_wrapper_settle() {
        assert_eq!(server_metrics_after_settle_ms_from(None), 5_000);
        assert_eq!(server_metrics_after_settle_ms_from(Some("garbage")), 5_000);
    }

    #[test]
    fn server_metrics_after_settle_ms_allows_zero_and_custom_override() {
        assert_eq!(server_metrics_after_settle_ms_from(Some("0")), 0);
        assert_eq!(server_metrics_after_settle_ms_from(Some("250")), 250);
    }
}
