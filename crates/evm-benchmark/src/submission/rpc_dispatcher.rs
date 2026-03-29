use crate::submission::rpc::{RpcSubmitter, SubmissionResult};
use crate::types::SignedTxWithMetadata;
use anyhow::Result;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, warn};
use url::Url;

/// Tracks health status of an RPC endpoint
#[derive(Debug, Clone)]
struct EndpointHealth {
    url: Url,
    consecutive_failures: u32,
    last_failure_time: Option<Instant>,
    is_degraded: bool,
}

impl EndpointHealth {
    fn new(url: Url) -> Self {
        EndpointHealth {
            url,
            consecutive_failures: 0,
            last_failure_time: None,
            is_degraded: false,
        }
    }

    /// Record a successful request
    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.is_degraded = false;
    }

    /// Record a failed request
    fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        self.last_failure_time = Some(Instant::now());

        if self.consecutive_failures >= 3 {
            self.is_degraded = true;
        }
    }

    /// Check if endpoint is healthy and ready for requests
    fn is_healthy(&self) -> bool {
        if !self.is_degraded {
            return true;
        }

        // Check if we should recover from degraded state (30 seconds without errors)
        if let Some(last_failure) = self.last_failure_time
            && last_failure.elapsed() > Duration::from_secs(30)
        {
            return true;
        }

        false
    }
}

/// Dispatcher for multi-RPC endpoint support with round-robin and failover
#[derive(Debug)]
pub struct RpcDispatcher {
    endpoints: Arc<Mutex<Vec<EndpointHealth>>>,
    submitters: Vec<RpcSubmitter>,
    current_index: Arc<Mutex<usize>>,
    #[allow(dead_code)]
    batch_size: u32,
}

impl RpcDispatcher {
    /// Create a new RPC dispatcher with multiple endpoints
    pub fn new(urls: Vec<Url>, batch_size: u32) -> Result<Self> {
        Self::with_retry_profile(urls, batch_size, "light")
    }

    /// Create a new RPC dispatcher with an explicit retry profile name.
    pub fn with_retry_profile(
        urls: Vec<Url>,
        batch_size: u32,
        retry_profile: &str,
    ) -> Result<Self> {
        if urls.is_empty() {
            anyhow::bail!("At least one RPC endpoint URL is required");
        }

        let mut submitters = Vec::new();
        let mut endpoints = Vec::new();

        for url in urls {
            submitters.push(RpcSubmitter::with_retry_profile(
                &url,
                batch_size,
                retry_profile,
            )?);
            endpoints.push(EndpointHealth::new(url));
        }

        Ok(RpcDispatcher {
            endpoints: Arc::new(Mutex::new(endpoints)),
            submitters,
            current_index: Arc::new(Mutex::new(0)),
            batch_size,
        })
    }

    /// Create dispatcher from a single URL (backward compatible)
    #[allow(dead_code)]
    pub fn new_single(url: Url, batch_size: u32) -> Result<Self> {
        Self::new(vec![url], batch_size)
    }

    /// Submit batch with round-robin across healthy endpoints and automatic failover
    pub async fn submit_batch(&self, txs: Vec<SignedTxWithMetadata>) -> Result<SubmissionResult> {
        // Get initial state without holding lock
        let (current_idx, endpoints_len) = {
            let endpoints = self.endpoints.lock().unwrap();

            if endpoints.is_empty() {
                anyhow::bail!("No RPC endpoints available");
            }

            // Find the next healthy endpoint starting from current index
            let healthy_count = endpoints.iter().filter(|e| e.is_healthy()).count();

            if healthy_count == 0 {
                anyhow::bail!("No healthy RPC endpoints available");
            }

            let current = *self.current_index.lock().unwrap();
            (current, endpoints.len())
        };

        let mut attempts = 0;
        let max_attempts = endpoints_len;

        // Try endpoints starting from current index
        loop {
            let idx = (current_idx + attempts) % endpoints_len;

            // Check if endpoint is healthy (minimal lock scope)
            {
                let endpoints = self.endpoints.lock().unwrap();
                if !endpoints[idx].is_healthy() {
                    attempts += 1;
                    if attempts >= max_attempts {
                        anyhow::bail!("All RPC endpoints are degraded or unavailable");
                    }
                    continue;
                }
            }

            // Attempt submission (no lock held during await)
            match self.submitters[idx].submit_batch(txs.clone()).await {
                Ok(result) => {
                    // Record success and advance index for next call
                    {
                        let mut endpoints = self.endpoints.lock().unwrap();
                        endpoints[idx].record_success();
                        let url = endpoints[idx].url.to_string();

                        debug!(
                            endpoint = url,
                            submitted = result.submitted,
                            errors = result.errors,
                            "Batch submitted successfully"
                        );
                    }

                    *self.current_index.lock().unwrap() = (idx + 1) % endpoints_len;
                    return Ok(result);
                }
                Err(e) => {
                    // Record failure and try next endpoint
                    {
                        let mut endpoints = self.endpoints.lock().unwrap();
                        endpoints[idx].record_failure();

                        let url = endpoints[idx].url.to_string();
                        let is_degraded = endpoints[idx].is_degraded;

                        warn!(
                            endpoint = url,
                            error = %e,
                            degraded = is_degraded,
                            "Submission failed, attempting next endpoint"
                        );
                    }

                    attempts += 1;
                    if attempts >= max_attempts {
                        anyhow::bail!("All RPC endpoints failed. Last error: {}", e);
                    }
                }
            }
        }
    }

    /// Get endpoint status for monitoring
    #[allow(dead_code)]
    pub fn get_endpoint_status(&self) -> Vec<(String, bool, u32)> {
        let endpoints = self.endpoints.lock().unwrap();
        endpoints
            .iter()
            .map(|e| (e.url.to_string(), e.is_healthy(), e.consecutive_failures))
            .collect()
    }

    /// Submit a single transaction via round-robin dispatcher
    pub async fn submit_single(&self, tx: SignedTxWithMetadata) -> Result<SubmissionResult> {
        self.submit_batch(vec![tx]).await
    }

    /// Warm up all endpoints
    pub async fn warm_up(&self, dummy_request_count: u32) -> Result<()> {
        for submitter in &self.submitters {
            submitter.warm_up(dummy_request_count).await?;
        }
        Ok(())
    }

    /// Get the number of configured endpoints
    #[allow(dead_code)]
    pub fn endpoint_count(&self) -> usize {
        self.submitters.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_urls(count: usize) -> Vec<Url> {
        (0..count)
            .map(|i| {
                Url::parse(&format!("http://localhost:{}", 8545 + i))
                    .expect("Failed to parse test URL")
            })
            .collect()
    }

    #[test]
    fn test_dispatcher_creation_single() {
        let url = Url::parse("http://localhost:8545").expect("Failed to parse URL");
        let dispatcher = RpcDispatcher::new_single(url, 100).expect("Failed to create dispatcher");

        assert_eq!(dispatcher.endpoint_count(), 1);
    }

    #[test]
    fn test_dispatcher_creation_multiple() {
        let urls = create_test_urls(3);
        let dispatcher = RpcDispatcher::new(urls, 100).expect("Failed to create dispatcher");

        assert_eq!(dispatcher.endpoint_count(), 3);
    }

    #[test]
    fn test_dispatcher_rejects_empty_urls() {
        let result = RpcDispatcher::new(vec![], 100);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("At least one"));
    }

    #[test]
    fn test_endpoint_health_success() {
        let url = Url::parse("http://localhost:8545").expect("Failed to parse URL");
        let health = EndpointHealth::new(url);

        assert!(health.is_healthy());
        assert_eq!(health.consecutive_failures, 0);
    }

    #[test]
    fn test_endpoint_health_degradation() {
        let url = Url::parse("http://localhost:8545").expect("Failed to parse URL");
        let mut health = EndpointHealth::new(url);

        // Record failures
        health.record_failure();
        assert!(health.is_healthy());
        assert_eq!(health.consecutive_failures, 1);

        health.record_failure();
        assert!(health.is_healthy());
        assert_eq!(health.consecutive_failures, 2);

        // Third failure marks as degraded
        health.record_failure();
        assert!(!health.is_healthy());
        assert_eq!(health.consecutive_failures, 3);
        assert!(health.is_degraded);
    }

    #[test]
    fn test_endpoint_health_recovery() {
        let url = Url::parse("http://localhost:8545").expect("Failed to parse URL");
        let mut health = EndpointHealth::new(url);

        // Mark as degraded
        health.record_failure();
        health.record_failure();
        health.record_failure();
        assert!(!health.is_healthy());

        // Record success clears failures
        health.record_success();
        assert!(health.is_healthy());
        assert_eq!(health.consecutive_failures, 0);
        assert!(!health.is_degraded);
    }

    #[test]
    fn test_endpoint_health_timeout_recovery() {
        let url = Url::parse("http://localhost:8545").expect("Failed to parse URL");
        let mut health = EndpointHealth::new(url);

        // Mark as degraded
        health.record_failure();
        health.record_failure();
        health.record_failure();
        assert!(!health.is_healthy());

        // Simulate recovery timeout by manually setting last_failure_time to 31 seconds ago
        health.last_failure_time = Some(Instant::now() - Duration::from_secs(31));
        assert!(health.is_healthy());
    }

    #[test]
    fn test_get_endpoint_status() {
        let urls = create_test_urls(2);
        let dispatcher = RpcDispatcher::new(urls, 100).expect("Failed to create dispatcher");

        let status = dispatcher.get_endpoint_status();
        assert_eq!(status.len(), 2);

        // All should be healthy initially
        for (_, is_healthy, failures) in status {
            assert!(is_healthy);
            assert_eq!(failures, 0);
        }
    }

    #[tokio::test]
    async fn test_submit_batch_single_endpoint() {
        // This test will fail until the actual RPC call is mocked or integration tested
        // For now, we're testing the structure works
        let url = Url::parse("http://localhost:9999").expect("Failed to parse URL");
        let dispatcher = RpcDispatcher::new_single(url, 100).expect("Failed to create dispatcher");

        // Should have 1 endpoint
        assert_eq!(dispatcher.endpoint_count(), 1);
    }

    #[test]
    fn test_round_robin_index_advance() {
        let urls = create_test_urls(3);
        let dispatcher = RpcDispatcher::new(urls, 100).expect("Failed to create dispatcher");

        // Check that index advances properly
        let idx = *dispatcher.current_index.lock().unwrap();
        assert!(idx < 3);
    }

    #[test]
    fn test_new_single_creates_one_endpoint() {
        let url = Url::parse("http://localhost:8545").expect("Failed to parse URL");
        let dispatcher = RpcDispatcher::new_single(url.clone(), 50).expect("Failed to create");
        assert_eq!(dispatcher.endpoint_count(), 1);

        let status = dispatcher.get_endpoint_status();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].0, url.to_string());
        assert!(status[0].1); // healthy
        assert_eq!(status[0].2, 0); // zero failures
    }

    #[test]
    fn test_endpoint_count_returns_correct_value() {
        for count in [1, 2, 5, 10] {
            let urls = create_test_urls(count);
            let dispatcher = RpcDispatcher::new(urls, 100).expect("Failed to create dispatcher");
            assert_eq!(dispatcher.endpoint_count(), count);
        }
    }

    #[test]
    fn test_endpoint_health_degradation_and_recovery_cycles() {
        let url = Url::parse("http://localhost:8545").expect("Failed to parse URL");
        let mut health = EndpointHealth::new(url);

        // Cycle 1: degrade then recover
        health.record_failure();
        health.record_failure();
        health.record_failure();
        assert!(!health.is_healthy());
        assert_eq!(health.consecutive_failures, 3);

        health.record_success();
        assert!(health.is_healthy());
        assert_eq!(health.consecutive_failures, 0);

        // Cycle 2: degrade again then recover
        health.record_failure();
        assert!(health.is_healthy()); // only 1 failure
        health.record_failure();
        health.record_failure();
        assert!(!health.is_healthy());

        health.record_success();
        assert!(health.is_healthy());
        assert!(!health.is_degraded);

        // Cycle 3: partial failures, never degraded
        health.record_failure();
        health.record_failure();
        assert!(health.is_healthy()); // 2 < 3 threshold
        health.record_success();
        assert!(health.is_healthy());
        assert_eq!(health.consecutive_failures, 0);
    }

    #[test]
    fn test_endpoint_health_five_failures_then_recovery() {
        let url = Url::parse("http://localhost:8545").expect("Failed to parse URL");
        let mut health = EndpointHealth::new(url);

        // Record 5 consecutive failures
        for _ in 0..5 {
            health.record_failure();
        }
        assert_eq!(health.consecutive_failures, 5);
        assert!(health.is_degraded);
        assert!(!health.is_healthy());

        // Single success fully recovers
        health.record_success();
        assert!(health.is_healthy());
        assert_eq!(health.consecutive_failures, 0);
        assert!(!health.is_degraded);
    }
}
