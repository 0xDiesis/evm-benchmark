use crate::submission::rpc::SubmissionResult;
use crate::types::SignedTxWithMetadata;
use alloy_network::AnyNetwork;
use alloy_provider::Provider;
use anyhow::Result;
use rand::Rng;
use tracing::{error, warn};

#[derive(Clone, Copy)]
struct RetryProfile {
    max_attempts: u32,
    base_backoff_ms: u64,
    jitter_ms: u64,
}

impl RetryProfile {
    fn from_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "off" => Self {
                max_attempts: 1,
                base_backoff_ms: 0,
                jitter_ms: 0,
            },
            "moderate" => Self {
                max_attempts: 4,
                base_backoff_ms: 20,
                jitter_ms: 20,
            },
            "aggressive" => Self {
                max_attempts: 5,
                base_backoff_ms: 30,
                jitter_ms: 40,
            },
            _ => Self {
                max_attempts: 3,
                base_backoff_ms: 10,
                jitter_ms: 10,
            },
        }
    }

    fn delay_for_attempt(&self, attempt: u32) -> std::time::Duration {
        if attempt <= 1 || self.base_backoff_ms == 0 {
            return std::time::Duration::from_millis(0);
        }
        let exp = self
            .base_backoff_ms
            .saturating_mul(2u64.saturating_pow(attempt - 2));
        let jitter = if self.jitter_ms > 0 {
            rand::thread_rng().gen_range(0..=self.jitter_ms)
        } else {
            0
        };
        std::time::Duration::from_millis(exp.saturating_add(jitter))
    }
}

/// WebSocket-based transaction submitter.
///
/// Submits transactions via WebSocket connections to reduce latency
/// and improve throughput compared to HTTP.
pub struct WsSubmitter {
    ws_url: url::Url,
    batch_size: u32,
    retry_profile: RetryProfile,
}

impl WsSubmitter {
    /// Create a new WebSocket submitter.
    #[allow(dead_code)]
    pub fn new(ws_url: &url::Url, batch_size: u32) -> Result<Self> {
        Self::with_retry_profile(ws_url, batch_size, "light")
    }

    /// Create a new WebSocket submitter with an explicit retry profile name.
    pub fn with_retry_profile(
        ws_url: &url::Url,
        batch_size: u32,
        retry_profile: &str,
    ) -> Result<Self> {
        Ok(WsSubmitter {
            ws_url: ws_url.clone(),
            batch_size,
            retry_profile: RetryProfile::from_name(retry_profile),
        })
    }

    /// Warm up the WebSocket connection before benchmarking.
    pub async fn warm_up(&self, dummy_request_count: u32) -> Result<()> {
        let provider =
            alloy_provider::RootProvider::<AnyNetwork>::connect(self.ws_url.as_str()).await?;

        for _ in 0..dummy_request_count {
            match provider.get_block_number().await {
                Ok(_) => {}
                Err(e) => {
                    if !cfg!(test) {
                        eprintln!("WS warm-up request failed: {}", e);
                    }
                }
            }
        }

        Ok(())
    }

    /// Submit a batch of transactions via WebSocket.
    ///
    /// Opens a single WebSocket connection and submits all transactions through it,
    /// chunked by `batch_size`. Each transaction uses exponential backoff retry logic
    /// (3 attempts, 2^n milliseconds backoff) for transient errors. If the shared
    /// connection fails mid-submission, remaining transactions in the chunk are counted
    /// as errors and processing continues with the next chunk.
    pub async fn submit_batch(&self, txs: Vec<SignedTxWithMetadata>) -> Result<SubmissionResult> {
        let provider =
            alloy_provider::RootProvider::<AnyNetwork>::connect(self.ws_url.as_str()).await?;

        let mut submitted = 0u32;
        let mut errors = 0u32;
        let mut hashes = vec![];
        let mut accepted_txs = vec![];

        for chunk in txs.chunks(self.batch_size as usize) {
            match Self::submit_chunk(&provider, chunk, self.retry_profile).await {
                Ok(batch_result) => {
                    submitted += batch_result.submitted;
                    errors += batch_result.errors;
                    hashes.extend(batch_result.hashes);
                    accepted_txs.extend(batch_result.accepted_txs);
                }
                Err(e) => {
                    if !cfg!(test) {
                        eprintln!("WS batch error: {}", e);
                    }
                    errors += chunk.len() as u32;
                }
            }
        }

        Ok(SubmissionResult {
            submitted,
            errors,
            hashes,
            accepted_txs,
            pool_full_txs: vec![],
        })
    }

    /// Submit a single transaction via WebSocket.
    #[allow(dead_code)]
    pub async fn submit_single(&self, tx: SignedTxWithMetadata) -> Result<SubmissionResult> {
        self.submit_batch(vec![tx]).await
    }

    /// Internal: submit a chunk of transactions over an existing WebSocket provider.
    ///
    /// Each transaction is retried up to 3 times with exponential backoff for
    /// transient errors (timeout, connection, closed).
    async fn submit_chunk(
        provider: &alloy_provider::RootProvider<AnyNetwork>,
        txs: &[SignedTxWithMetadata],
        retry: RetryProfile,
    ) -> Result<SubmissionResult> {
        let mut hashes = vec![];
        let mut errors = 0;
        let mut accepted_txs = vec![];

        for (idx, tx) in txs.iter().enumerate() {
            let mut attempt = 0;

            loop {
                match provider.send_raw_transaction(&tx.encoded).await {
                    Ok(pending_tx) => {
                        let returned_hash = pending_tx.tx_hash();
                        hashes.push(format!("{}", returned_hash));
                        accepted_txs.push(tx.clone());
                        break;
                    }
                    Err(e) => {
                        attempt += 1;

                        if attempt < retry.max_attempts {
                            let error_str = e.to_string();
                            let is_transient = error_str.contains("timeout")
                                || error_str.contains("connection")
                                || error_str.contains("temporarily unavailable")
                                || error_str.contains("closed")
                                || error_str.contains("busy");

                            if is_transient {
                                let backoff = retry.delay_for_attempt(attempt);
                                warn!(
                                    tx_idx = idx,
                                    attempt,
                                    backoff_ms = backoff.as_millis() as u64,
                                    "Transient submission error, retrying..."
                                );
                                tokio::time::sleep(backoff).await;
                                continue;
                            }
                        }

                        if !cfg!(test) {
                            error!(tx_idx = idx, error = %e, "TX submission failed");
                        }
                        errors += 1;
                        break;
                    }
                }
            }
        }

        Ok(SubmissionResult {
            submitted: (txs.len() as u32) - errors,
            errors,
            hashes,
            accepted_txs,
            pool_full_txs: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ws_submitter_creation() {
        let url = url::Url::parse("ws://localhost:8546").expect("failed to parse url");
        let submitter = WsSubmitter::new(&url, 100).expect("failed to create submitter");
        assert_eq!(submitter.ws_url.as_str(), "ws://localhost:8546/");
        assert_eq!(submitter.batch_size, 100);
    }

    #[test]
    fn test_ws_submitter_batch_size() {
        let url = url::Url::parse("ws://localhost:8546").expect("failed to parse url");
        let submitter = WsSubmitter::new(&url, 50).expect("failed to create submitter");
        assert_eq!(submitter.batch_size, 50);
    }

    #[tokio::test]
    async fn test_ws_submitter_warm_up_no_server() {
        let url = url::Url::parse("ws://localhost:9999").expect("failed to parse url");
        let submitter = WsSubmitter::new(&url, 100).expect("failed to create submitter");
        let result = submitter.warm_up(3).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_ws_submitter_different_batch_sizes() {
        let url = url::Url::parse("ws://localhost:8546").expect("failed to parse url");

        for batch_size in [1, 10, 50, 100, 500, 1000] {
            let submitter = WsSubmitter::new(&url, batch_size).expect("failed to create submitter");
            assert_eq!(submitter.batch_size, batch_size);
        }
    }

    #[test]
    fn test_ws_submitter_url_preserved() {
        let urls = [
            "ws://localhost:8546",
            "ws://10.0.0.1:9000",
            "wss://example.com:443/ws",
        ];

        for url_str in urls {
            let url = url::Url::parse(url_str).expect("failed to parse url");
            let submitter = WsSubmitter::new(&url, 100).expect("failed to create submitter");
            assert_eq!(submitter.ws_url, url);
        }
    }
}
