use crate::errors::BenchError;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

/// Health metrics for a single validator
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatorHealth {
    /// Validator endpoint URL
    pub url: String,
    /// Last time health was checked (as Unix timestamp)
    #[serde(default)]
    pub last_check_timestamp: Option<u64>,
    /// Current block height
    pub block_height: Option<u64>,
    /// Whether validator is synced
    pub is_synced: bool,
    /// Availability: percentage of successful checks (0.0 - 100.0)
    pub availability_percent: f64,
    /// Latency percentiles in milliseconds
    pub latency_p50_ms: Option<u64>,
    pub latency_p95_ms: Option<u64>,
    pub latency_p99_ms: Option<u64>,
    /// Transaction acceptance rate (0.0 - 100.0)
    pub tx_acceptance_rate: f64,
    /// Error rate (0.0 - 100.0)
    pub error_rate: f64,
    /// Total checks performed
    pub total_checks: u64,
    /// Successful checks
    pub successful_checks: u64,
    /// Failed checks
    pub failed_checks: u64,
    /// Network connection status
    pub is_connected: bool,
}

impl ValidatorHealth {
    /// Create a new ValidatorHealth for a given URL
    pub fn new(url: String) -> Self {
        ValidatorHealth {
            url,
            last_check_timestamp: None,
            block_height: None,
            is_synced: false,
            availability_percent: 0.0,
            latency_p50_ms: None,
            latency_p95_ms: None,
            latency_p99_ms: None,
            tx_acceptance_rate: 0.0,
            error_rate: 0.0,
            total_checks: 0,
            successful_checks: 0,
            failed_checks: 0,
            is_connected: false,
        }
    }

    /// Update availability percentage based on successful/failed checks
    pub fn update_availability(&mut self) {
        if self.total_checks == 0 {
            self.availability_percent = 0.0;
        } else {
            self.availability_percent =
                (self.successful_checks as f64 / self.total_checks as f64) * 100.0;
        }
    }

    /// Calculate error rate from failed checks
    pub fn calculate_error_rate(&mut self) {
        if self.total_checks == 0 {
            self.error_rate = 0.0;
        } else {
            self.error_rate = (self.failed_checks as f64 / self.total_checks as f64) * 100.0;
        }
    }

    /// Record a successful check
    pub fn record_success(&mut self, block_height: u64, _latency_ms: u64) {
        self.successful_checks += 1;
        self.total_checks += 1;
        self.last_check_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());
        self.block_height = Some(block_height);
        self.is_connected = true;
        self.update_availability();
        self.calculate_error_rate();
    }

    /// Record a failed check
    pub fn record_failure(&mut self) {
        self.failed_checks += 1;
        self.total_checks += 1;
        self.last_check_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());
        self.is_connected = false;
        self.update_availability();
        self.calculate_error_rate();
    }
}

/// Health monitor for tracking multiple validator endpoints
pub struct HealthMonitor {
    /// Health metrics keyed by validator URL
    health_data: Arc<DashMap<String, ValidatorHealth>>,
    /// Latency samples for percentile calculation (keyed by URL)
    latency_samples: Arc<DashMap<String, Vec<u64>>>,
    /// Maximum number of latency samples to keep per validator
    max_latency_samples: usize,
    /// Poll interval for health checks
    poll_interval: Duration,
    /// Background task handle
    task_handle: Option<tokio::task::JoinHandle<()>>,
}

impl HealthMonitor {
    /// Create a new health monitor
    ///
    /// # Arguments
    /// * `validator_urls` - List of validator RPC endpoint URLs
    /// * `poll_interval_secs` - Interval for health checks (default 10 seconds)
    ///
    /// # Returns
    /// Result containing a new HealthMonitor instance
    pub fn new(validator_urls: Vec<String>, poll_interval_secs: u64) -> Result<Self, BenchError> {
        if validator_urls.is_empty() {
            return Err(BenchError::ConfigError(
                "At least one validator URL required".to_string(),
            ));
        }

        let health_data = Arc::new(DashMap::new());
        let latency_samples = Arc::new(DashMap::new());

        // Initialize health entries
        for url in validator_urls {
            health_data.insert(url.clone(), ValidatorHealth::new(url.clone()));
            latency_samples.insert(url, Vec::new());
        }

        Ok(HealthMonitor {
            health_data,
            latency_samples,
            max_latency_samples: 1000,
            poll_interval: Duration::from_secs(poll_interval_secs),
            task_handle: None,
        })
    }

    /// Start background health monitoring task
    pub fn start(&mut self) -> Result<(), BenchError> {
        let health_data = Arc::clone(&self.health_data);
        let latency_samples = Arc::clone(&self.latency_samples);
        let poll_interval = self.poll_interval;

        let task = tokio::spawn(async move {
            loop {
                let urls: Vec<String> = health_data
                    .iter()
                    .map(|entry| entry.key().clone())
                    .collect();
                for url in urls {
                    // Simulate health check - in real implementation would call RPC
                    Self::perform_health_check(&url, &health_data, &latency_samples).await;
                }
                tokio::time::sleep(poll_interval).await;
            }
        });

        self.task_handle = Some(task);
        Ok(())
    }

    /// Perform a health check for a single validator
    async fn perform_health_check(
        url: &str,
        health_data: &Arc<DashMap<String, ValidatorHealth>>,
        latency_samples: &Arc<DashMap<String, Vec<u64>>>,
    ) {
        let _check_start = Instant::now();

        // Try to fetch block number via RPC
        match Self::fetch_block_number(url).await {
            Ok((block_height, latency_ms)) => {
                debug!(url, block_height, latency_ms, "Health check succeeded");

                if let Some(mut health) = health_data.get_mut(url) {
                    health.record_success(block_height, latency_ms);
                    health.is_synced = true;
                }

                // Track latency sample
                if let Some(mut samples) = latency_samples.get_mut(url) {
                    samples.push(latency_ms);
                    // Keep only the last N samples
                    if samples.len() > 1000 {
                        samples.remove(0);
                    }
                }
            }
            Err(e) => {
                warn!(url, error = %e, "Health check failed");
                if let Some(mut health) = health_data.get_mut(url) {
                    health.record_failure();
                }
            }
        }
    }

    /// Fetch current block height from validator RPC
    async fn fetch_block_number(url: &str) -> Result<(u64, u64), BenchError> {
        let check_start = Instant::now();

        let client = reqwest::Client::new();
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_blockNumber",
            "params": [],
            "id": 1
        });

        let response = client
            .post(url)
            .json(&payload)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| BenchError::RpcError(format!("Connection failed: {}", e)))?;

        let result: serde_json::Value = response
            .json()
            .await
            .map_err(|e| BenchError::RpcError(format!("Failed to parse response: {}", e)))?;

        let block_hex = result
            .get("result")
            .and_then(|r| r.as_str())
            .ok_or_else(|| BenchError::RpcError("No result in response".to_string()))?;

        let block_height = u64::from_str_radix(block_hex.trim_start_matches("0x"), 16)
            .map_err(|e| BenchError::RpcError(format!("Invalid block number: {}", e)))?;

        let latency_ms = check_start.elapsed().as_millis() as u64;

        Ok((block_height, latency_ms))
    }

    /// Get current health status for all validators
    pub fn get_health_status(&self) -> Vec<ValidatorHealth> {
        self.health_data
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Get health status for a specific validator
    pub fn get_validator_health(&self, url: &str) -> Option<ValidatorHealth> {
        self.health_data.get(url).map(|entry| entry.value().clone())
    }

    /// Update latency percentiles for all validators
    pub fn update_latency_percentiles(&self) {
        for mut health_entry in self.health_data.iter_mut() {
            let url = health_entry.key().clone();
            if let Some(samples) = self.latency_samples.get(&url)
                && !samples.is_empty()
            {
                health_entry.latency_p50_ms = Some(Self::calculate_percentile(&samples, 50));
                health_entry.latency_p95_ms = Some(Self::calculate_percentile(&samples, 95));
                health_entry.latency_p99_ms = Some(Self::calculate_percentile(&samples, 99));
            }
        }
    }

    /// Calculate percentile from sorted samples
    fn calculate_percentile(samples: &[u64], percentile: u64) -> u64 {
        if samples.is_empty() {
            return 0;
        }

        let mut sorted = samples.to_vec();
        sorted.sort_unstable();

        let index = ((percentile as f64 / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
        sorted[index.min(sorted.len() - 1)]
    }

    /// Record a transaction submission success
    pub fn record_tx_accepted(&self, url: &str) {
        if let Some(mut health) = self.health_data.get_mut(url) {
            // Update acceptance rate based on recent submissions
            health.tx_acceptance_rate = 100.0; // Simplified - would track submissions
        }
    }

    /// Record a transaction submission failure
    pub fn record_tx_rejected(&self, url: &str) {
        if let Some(mut health) = self.health_data.get_mut(url) {
            health.tx_acceptance_rate = 0.0; // Simplified - would track submissions
        }
    }

    /// Clear all health data
    pub fn clear(&self) {
        self.health_data.clear();
        self.latency_samples.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn test_validator_health_initialization() {
        let health = ValidatorHealth::new("http://localhost:8545".to_string());
        assert_eq!(health.url, "http://localhost:8545");
        assert_eq!(health.total_checks, 0);
        assert_eq!(health.successful_checks, 0);
        assert_eq!(health.failed_checks, 0);
        assert_eq!(health.availability_percent, 0.0);
        assert!(!health.is_connected);
    }

    #[test]
    fn test_validator_health_record_success() {
        let mut health = ValidatorHealth::new("http://localhost:8545".to_string());
        health.record_success(1000, 50);

        assert_eq!(health.total_checks, 1);
        assert_eq!(health.successful_checks, 1);
        assert_eq!(health.failed_checks, 0);
        assert_eq!(health.block_height, Some(1000));
        assert_eq!(health.availability_percent, 100.0);
        assert!(health.is_connected);
    }

    #[test]
    fn test_validator_health_record_failure() {
        let mut health = ValidatorHealth::new("http://localhost:8545".to_string());
        health.record_success(1000, 50);
        health.record_failure();

        assert_eq!(health.total_checks, 2);
        assert_eq!(health.successful_checks, 1);
        assert_eq!(health.failed_checks, 1);
        assert_eq!(health.availability_percent, 50.0);
        assert!(!health.is_connected);
    }

    #[test]
    fn test_validator_health_availability_calculation() {
        let mut health = ValidatorHealth::new("http://localhost:8545".to_string());

        for _ in 0..9 {
            health.record_success(1000, 50);
        }
        health.record_failure();

        assert_eq!(health.total_checks, 10);
        assert_eq!(health.successful_checks, 9);
        assert_eq!(health.failed_checks, 1);
        assert_eq!(health.availability_percent, 90.0);
    }

    #[test]
    fn test_validator_health_error_rate() {
        let mut health = ValidatorHealth::new("http://localhost:8545".to_string());

        for _ in 0..7 {
            health.record_success(1000, 50);
        }
        for _ in 0..3 {
            health.record_failure();
        }

        assert_eq!(health.total_checks, 10);
        assert_eq!(health.error_rate, 30.0);
    }

    #[test]
    fn test_health_monitor_initialization() {
        let monitor = HealthMonitor::new(vec!["http://localhost:8545".to_string()], 10);

        assert!(monitor.is_ok());
        let monitor = monitor.unwrap();
        assert_eq!(monitor.health_data.len(), 1);
    }

    #[test]
    fn test_health_monitor_empty_urls_error() {
        let monitor = HealthMonitor::new(vec![], 10);
        assert!(monitor.is_err());
    }

    #[test]
    fn test_health_monitor_get_health_status() {
        let monitor = HealthMonitor::new(vec!["http://localhost:8545".to_string()], 10).unwrap();

        let statuses = monitor.get_health_status();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].url, "http://localhost:8545");
    }

    #[test]
    fn test_health_monitor_get_specific_validator() {
        let monitor = HealthMonitor::new(vec!["http://localhost:8545".to_string()], 10).unwrap();

        let health = monitor.get_validator_health("http://localhost:8545");
        assert!(health.is_some());
        assert_eq!(health.unwrap().url, "http://localhost:8545");
    }

    #[test]
    fn test_latency_percentile_calculation() {
        let samples = vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100];

        let p50 = HealthMonitor::calculate_percentile(&samples, 50);
        let p95 = HealthMonitor::calculate_percentile(&samples, 95);
        let p99 = HealthMonitor::calculate_percentile(&samples, 99);

        assert!((40..=60).contains(&p50), "p50 should be around 50");
        assert!((85..=100).contains(&p95), "p95 should be around 95");
        assert_eq!(p99, 100, "p99 should be 100");
    }

    #[test]
    fn test_latency_percentile_single_sample() {
        let samples = vec![42];

        let p50 = HealthMonitor::calculate_percentile(&samples, 50);
        let p95 = HealthMonitor::calculate_percentile(&samples, 95);
        let p99 = HealthMonitor::calculate_percentile(&samples, 99);

        assert_eq!(p50, 42);
        assert_eq!(p95, 42);
        assert_eq!(p99, 42);
    }

    #[test]
    fn test_latency_percentile_empty_samples() {
        let samples: Vec<u64> = vec![];
        let p50 = HealthMonitor::calculate_percentile(&samples, 50);
        assert_eq!(p50, 0);
    }

    #[test]
    fn test_health_monitor_clear() {
        let monitor = HealthMonitor::new(
            vec![
                "http://localhost:8545".to_string(),
                "http://localhost:8546".to_string(),
            ],
            10,
        )
        .unwrap();

        assert_eq!(monitor.health_data.len(), 2);
        monitor.clear();
        assert_eq!(monitor.health_data.len(), 0);
    }

    #[tokio::test]
    async fn test_health_monitor_multiple_validators() {
        let urls = vec![
            "http://localhost:8545".to_string(),
            "http://localhost:8546".to_string(),
            "http://localhost:8547".to_string(),
        ];

        let monitor = HealthMonitor::new(urls.clone(), 10).unwrap();
        assert_eq!(monitor.health_data.len(), 3);

        let statuses = monitor.get_health_status();
        assert_eq!(statuses.len(), 3);

        let status_urls: Vec<String> = statuses.iter().map(|s| s.url.clone()).collect();
        for url in &urls {
            assert!(
                status_urls.contains(url),
                "Expected URL {} in statuses",
                url
            );
        }
    }

    #[test]
    fn test_health_monitor_update_latency_percentiles() {
        let monitor = HealthMonitor::new(vec!["http://localhost:8545".to_string()], 10).unwrap();

        let url = "http://localhost:8545";
        {
            let mut samples = monitor.latency_samples.get_mut(url).unwrap();
            for i in 1..=100 {
                samples.push(i);
            }
        }

        monitor.update_latency_percentiles();

        let health = monitor.get_validator_health(url).unwrap();
        assert!(health.latency_p50_ms.is_some());
        assert!(health.latency_p95_ms.is_some());
        assert!(health.latency_p99_ms.is_some());
    }

    #[test]
    fn test_validator_health_new_defaults() {
        let health = ValidatorHealth::new("http://example:8545".to_string());
        assert_eq!(health.url, "http://example:8545");
        assert!(health.last_check_timestamp.is_none());
        assert!(health.block_height.is_none());
        assert!(!health.is_synced);
        assert_eq!(health.availability_percent, 0.0);
        assert!(health.latency_p50_ms.is_none());
        assert!(health.latency_p95_ms.is_none());
        assert!(health.latency_p99_ms.is_none());
        assert_eq!(health.tx_acceptance_rate, 0.0);
        assert_eq!(health.error_rate, 0.0);
        assert_eq!(health.total_checks, 0);
        assert_eq!(health.successful_checks, 0);
        assert_eq!(health.failed_checks, 0);
        assert!(!health.is_connected);
    }

    #[test]
    fn test_record_tx_accepted_updates_acceptance_rate() {
        let url = "http://localhost:8545".to_string();
        let monitor = HealthMonitor::new(vec![url.clone()], 10).unwrap();

        monitor.record_tx_accepted(&url);

        let health = monitor.get_validator_health(&url).unwrap();
        assert_eq!(health.tx_acceptance_rate, 100.0);
    }

    #[test]
    fn test_record_tx_rejected_updates_acceptance_rate_to_zero() {
        let url = "http://localhost:8545".to_string();
        let monitor = HealthMonitor::new(vec![url.clone()], 10).unwrap();

        // First accept, then reject
        monitor.record_tx_accepted(&url);
        monitor.record_tx_rejected(&url);

        let health = monitor.get_validator_health(&url).unwrap();
        assert_eq!(health.tx_acceptance_rate, 0.0);
    }

    #[test]
    fn test_get_validator_health_nonexistent_returns_none() {
        let monitor = HealthMonitor::new(vec!["http://localhost:8545".to_string()], 10).unwrap();
        let result = monitor.get_validator_health("http://nonexistent:9999");
        assert!(result.is_none());
    }

    #[test]
    fn test_monitor_multiple_urls_all_initialized() {
        let urls = vec![
            "http://a:8545".to_string(),
            "http://b:8545".to_string(),
            "http://c:8545".to_string(),
        ];
        let monitor = HealthMonitor::new(urls.clone(), 5).unwrap();

        for url in &urls {
            let health = monitor.get_validator_health(url);
            assert!(health.is_some(), "expected health for {url}");
            let h = health.unwrap();
            assert_eq!(h.url, *url);
            assert_eq!(h.total_checks, 0);
        }
        assert_eq!(monitor.health_data.len(), 3);
    }

    #[test]
    fn test_calculate_percentile_two_samples() {
        let samples = vec![10, 90];
        let p50 = HealthMonitor::calculate_percentile(&samples, 50);
        // With 2 items sorted [10, 90], index = round(0.5 * 1) = 1 -> 90
        assert_eq!(p50, 90);

        let p0 = HealthMonitor::calculate_percentile(&samples, 0);
        assert_eq!(p0, 10);

        let p100 = HealthMonitor::calculate_percentile(&samples, 100);
        assert_eq!(p100, 90);
    }

    #[test]
    fn test_poll_interval_stored_correctly() {
        let monitor = HealthMonitor::new(vec!["http://localhost:8545".to_string()], 30).unwrap();
        assert_eq!(monitor.poll_interval, Duration::from_secs(30));
    }

    #[tokio::test]
    async fn test_health_monitor_start_sets_task_handle() {
        let mut monitor =
            HealthMonitor::new(vec!["http://localhost:8545".to_string()], 60).unwrap();
        assert!(monitor.task_handle.is_none());

        monitor.start().unwrap();
        assert!(monitor.task_handle.is_some());

        // Abort the background task so it doesn't leak
        monitor.task_handle.unwrap().abort();
    }

    #[tokio::test]
    async fn test_health_monitor_start_runs_background_checks() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("\"method\":\"eth_blockNumber\""))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"jsonrpc":"2.0","id":1,"result":"0x2b"})),
            )
            .mount(&mock_server)
            .await;

        let mut monitor = HealthMonitor::new(vec![mock_server.uri()], 1).unwrap();
        monitor.poll_interval = Duration::from_millis(5);
        monitor.start().unwrap();

        let mut observed_success = false;
        for _ in 0..40 {
            let health = monitor.get_health_status().pop().unwrap();
            if health.successful_checks > 0 && health.is_connected {
                observed_success = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        monitor.task_handle.take().unwrap().abort();
        assert!(observed_success);
    }

    #[tokio::test]
    async fn test_fetch_block_number_success_and_errors() {
        let success_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("\"method\":\"eth_blockNumber\""))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"jsonrpc":"2.0","id":1,"result":"0x2a"})),
            )
            .mount(&success_server)
            .await;

        let (block, latency_ms) = HealthMonitor::fetch_block_number(&success_server.uri())
            .await
            .expect("fetch should succeed");
        assert_eq!(block, 42);
        assert!(latency_ms <= 5_000);

        let missing_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"jsonrpc":"2.0","id":1})),
            )
            .mount(&missing_server)
            .await;
        assert!(
            HealthMonitor::fetch_block_number(&missing_server.uri())
                .await
                .unwrap_err()
                .to_string()
                .contains("No result")
        );

        let invalid_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"jsonrpc":"2.0","id":1,"result":"0xzz"})),
            )
            .mount(&invalid_server)
            .await;
        assert!(
            HealthMonitor::fetch_block_number(&invalid_server.uri())
                .await
                .unwrap_err()
                .to_string()
                .contains("Invalid block number")
        );
    }

    #[tokio::test]
    async fn test_perform_health_check_updates_success_and_failure_paths() {
        let success_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("\"method\":\"eth_blockNumber\""))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"jsonrpc":"2.0","id":1,"result":"0x10"})),
            )
            .mount(&success_server)
            .await;

        let url = success_server.uri();
        let monitor = HealthMonitor::new(vec![url.clone()], 1).unwrap();
        HealthMonitor::perform_health_check(&url, &monitor.health_data, &monitor.latency_samples)
            .await;

        let health = monitor.get_validator_health(&url).unwrap();
        assert_eq!(health.block_height, Some(16));
        assert!(health.is_connected);
        assert_eq!(health.successful_checks, 1);
        assert!(
            monitor
                .latency_samples
                .get(&url)
                .map(|samples| !samples.is_empty())
                .unwrap_or(false)
        );

        let failure_url = "http://127.0.0.1:9".to_string();
        let failure_monitor = HealthMonitor::new(vec![failure_url.clone()], 1).unwrap();
        HealthMonitor::perform_health_check(
            &failure_url,
            &failure_monitor.health_data,
            &failure_monitor.latency_samples,
        )
        .await;

        let failed = failure_monitor.get_validator_health(&failure_url).unwrap();
        assert_eq!(failed.failed_checks, 1);
        assert!(!failed.is_connected);
    }

    #[tokio::test]
    async fn test_perform_health_check_trims_latency_samples_over_limit() {
        let success_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("\"method\":\"eth_blockNumber\""))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"jsonrpc":"2.0","id":1,"result":"0x11"})),
            )
            .mount(&success_server)
            .await;

        let url = success_server.uri();
        let monitor = HealthMonitor::new(vec![url.clone()], 1).unwrap();
        {
            let mut samples = monitor.latency_samples.get_mut(&url).unwrap();
            samples.extend(1..=1000);
        }

        HealthMonitor::perform_health_check(&url, &monitor.health_data, &monitor.latency_samples)
            .await;

        let samples = monitor.latency_samples.get(&url).unwrap();
        assert_eq!(samples.len(), 1000);
        assert_eq!(samples[0], 2);
    }

    #[test]
    fn test_multiple_record_success_updates_block_height() {
        let mut health = ValidatorHealth::new("http://localhost:8545".to_string());

        health.record_success(100, 10);
        assert_eq!(health.block_height, Some(100));
        assert_eq!(health.successful_checks, 1);

        health.record_success(200, 20);
        assert_eq!(health.block_height, Some(200));
        assert_eq!(health.successful_checks, 2);

        health.record_success(300, 30);
        assert_eq!(health.block_height, Some(300));
        assert_eq!(health.successful_checks, 3);
        assert_eq!(health.total_checks, 3);
        assert_eq!(health.availability_percent, 100.0);
        assert_eq!(health.error_rate, 0.0);
        assert!(health.is_connected);
        assert!(health.last_check_timestamp.is_some());
    }

    #[test]
    fn test_interleaved_success_failure_accuracy() {
        let mut health = ValidatorHealth::new("http://localhost:8545".to_string());

        // Pattern: S, F, S, S, F, S, S, S, F, S => 7 success, 3 failure
        health.record_success(1, 10);
        health.record_failure();
        health.record_success(2, 10);
        health.record_success(3, 10);
        health.record_failure();
        health.record_success(4, 10);
        health.record_success(5, 10);
        health.record_success(6, 10);
        health.record_failure();
        health.record_success(7, 10);

        assert_eq!(health.total_checks, 10);
        assert_eq!(health.successful_checks, 7);
        assert_eq!(health.failed_checks, 3);
        assert_eq!(health.availability_percent, 70.0);
        assert_eq!(health.error_rate, 30.0);
        // Last operation was success
        assert!(health.is_connected);
        assert_eq!(health.block_height, Some(7));
    }

    #[test]
    fn test_all_failures_metrics() {
        let mut health = ValidatorHealth::new("http://localhost:8545".to_string());
        for _ in 0..5 {
            health.record_failure();
        }
        assert_eq!(health.total_checks, 5);
        assert_eq!(health.successful_checks, 0);
        assert_eq!(health.failed_checks, 5);
        assert_eq!(health.availability_percent, 0.0);
        assert_eq!(health.error_rate, 100.0);
        assert!(!health.is_connected);
    }

    #[test]
    fn test_update_availability_zero_checks() {
        let mut health = ValidatorHealth::new("http://localhost:8545".to_string());
        health.update_availability();
        assert_eq!(health.availability_percent, 0.0);
    }

    #[test]
    fn test_calculate_error_rate_zero_checks() {
        let mut health = ValidatorHealth::new("http://localhost:8545".to_string());
        health.calculate_error_rate();
        assert_eq!(health.error_rate, 0.0);
    }

    #[test]
    fn test_update_latency_percentiles_empty_samples() {
        let monitor = HealthMonitor::new(vec!["http://localhost:8545".to_string()], 10).unwrap();
        // samples are empty by default, update should be a no-op
        monitor.update_latency_percentiles();
        let health = monitor
            .get_validator_health("http://localhost:8545")
            .unwrap();
        assert!(health.latency_p50_ms.is_none());
        assert!(health.latency_p95_ms.is_none());
        assert!(health.latency_p99_ms.is_none());
    }

    #[test]
    fn test_record_tx_for_nonexistent_url() {
        let monitor = HealthMonitor::new(vec!["http://localhost:8545".to_string()], 10).unwrap();
        // These should not panic even for unknown URLs
        monitor.record_tx_accepted("http://nonexistent:9999");
        monitor.record_tx_rejected("http://nonexistent:9999");
    }
}
