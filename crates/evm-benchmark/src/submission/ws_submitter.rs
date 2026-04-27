use crate::submission::rpc::SubmissionResult;
use crate::types::SignedTxWithMetadata;
use alloy_network::AnyNetwork;
use alloy_provider::Provider;
use anyhow::Result;
use rand::Rng;
use tracing::error;
use tracing::warn;

#[derive(Clone, Copy)]
struct RetryProfile {
    max_attempts: u32,
    base_backoff_ms: u64,
    jitter_ms: u64,
}

fn is_transient_submission_error(error: &str) -> bool {
    error.contains("timeout")
        || error.contains("connection")
        || error.contains("temporarily unavailable")
        || error.contains("closed")
        || error.contains("busy")
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
                    #[cfg(not(test))]
                    eprintln!("WS warm-up request failed: {}", e);
                    #[cfg(test)]
                    let _ = e;
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
            let batch_result = Self::submit_chunk(&provider, chunk, self.retry_profile).await?;
            submitted += batch_result.submitted;
            errors += batch_result.errors;
            hashes.extend(batch_result.hashes);
            accepted_txs.extend(batch_result.accepted_txs);
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
                        let error_str = e.to_string();
                        if attempt < retry.max_attempts && is_transient_submission_error(&error_str)
                        {
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

                        error!(tx_idx = idx, error = %e, "TX submission failed");
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
    use crate::types::TransactionType;
    use alloy_primitives::B256;
    use futures::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;
    use tokio_tungstenite::{accept_async, tungstenite::protocol::Message};

    fn make_test_tx(nonce: u64) -> SignedTxWithMetadata {
        SignedTxWithMetadata {
            hash: B256::with_last_byte(nonce as u8),
            encoded: vec![0x02, nonce as u8],
            nonce,
            gas_limit: 21_000,
            sender: alloy_primitives::Address::default(),
            submit_time: Instant::now(),
            method: TransactionType::SimpleTransfer,
        }
    }

    async fn start_ws_rpc_server(
        handler: Arc<dyn Fn(Value) -> Value + Send + Sync>,
    ) -> (url::Url, Arc<Mutex<Vec<Value>>>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind ws test server");
        let addr = listener
            .local_addr()
            .expect("failed to read ws test server address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let requests_for_task = Arc::clone(&requests);

        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("failed to accept ws client");
            let mut socket = accept_async(stream)
                .await
                .expect("failed to upgrade ws client");

            while let Some(message) = socket.next().await {
                let message = match message {
                    Ok(message) => message,
                    Err(_) => break,
                };
                match message {
                    Message::Text(text) => {
                        let text = text.to_string();
                        let request: Value =
                            serde_json::from_str(&text).expect("ws request should be json");
                        requests_for_task
                            .lock()
                            .expect("request log mutex poisoned")
                            .push(request.clone());
                        let response = handler(request);
                        socket
                            .send(Message::Text(response.to_string()))
                            .await
                            .expect("failed to send ws response");
                    }
                    Message::Ping(data) => {
                        socket
                            .send(Message::Pong(data))
                            .await
                            .expect("failed to send pong");
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        });

        let url = url::Url::parse(&format!("ws://{addr}")).expect("failed to parse ws test url");
        (url, requests, task)
    }

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

    #[test]
    fn test_retry_profile_named_variants() {
        let off = RetryProfile::from_name("off");
        assert_eq!(off.max_attempts, 1);
        assert_eq!(off.base_backoff_ms, 0);
        assert_eq!(off.jitter_ms, 0);

        let moderate = RetryProfile::from_name("moderate");
        assert_eq!(moderate.max_attempts, 4);
        assert_eq!(moderate.base_backoff_ms, 20);
        assert_eq!(moderate.jitter_ms, 20);

        let aggressive = RetryProfile::from_name("aggressive");
        assert_eq!(aggressive.max_attempts, 5);
        assert_eq!(aggressive.base_backoff_ms, 30);
        assert_eq!(aggressive.jitter_ms, 40);

        let defaulted = RetryProfile::from_name("unknown-profile");
        assert_eq!(defaulted.max_attempts, 3);
        assert_eq!(defaulted.base_backoff_ms, 10);
        assert_eq!(defaulted.jitter_ms, 10);
    }

    #[test]
    fn test_retry_profile_delay_for_attempt_edges() {
        let off = RetryProfile::from_name("off");
        assert_eq!(
            off.delay_for_attempt(1),
            std::time::Duration::from_millis(0)
        );
        assert_eq!(
            off.delay_for_attempt(3),
            std::time::Duration::from_millis(0)
        );

        let defaulted = RetryProfile::from_name("light");
        assert_eq!(
            defaulted.delay_for_attempt(1),
            std::time::Duration::from_millis(0)
        );

        let second_attempt = defaulted.delay_for_attempt(2);
        assert!(second_attempt.as_millis() >= 10);
        assert!(second_attempt.as_millis() <= 20);

        let third_attempt = defaulted.delay_for_attempt(3);
        assert!(third_attempt.as_millis() >= 20);
        assert!(third_attempt.as_millis() <= 30);
    }

    #[test]
    fn test_retry_profile_delay_without_jitter_is_exact() {
        let profile = RetryProfile {
            max_attempts: 2,
            base_backoff_ms: 12,
            jitter_ms: 0,
        };

        assert_eq!(
            profile.delay_for_attempt(2),
            std::time::Duration::from_millis(12)
        );
        assert_eq!(
            profile.delay_for_attempt(4),
            std::time::Duration::from_millis(48)
        );
    }

    #[tokio::test]
    async fn test_ws_submitter_warm_up_success() {
        let block_number_requests = Arc::new(AtomicUsize::new(0));
        let block_number_requests_for_handler = Arc::clone(&block_number_requests);
        let handler = Arc::new(move |request: Value| {
            let id = request.get("id").cloned().unwrap_or_else(|| json!(1));
            assert_eq!(
                request.get("method").and_then(|m| m.as_str()),
                Some("eth_blockNumber")
            );
            block_number_requests_for_handler.fetch_add(1, Ordering::Relaxed);
            json!({"jsonrpc": "2.0", "id": id, "result": "0x10"})
        });
        let (url, _requests, server_task) = start_ws_rpc_server(handler).await;

        let submitter = WsSubmitter::new(&url, 100).expect("failed to create submitter");
        submitter
            .warm_up(3)
            .await
            .expect("warm_up should succeed against mock ws rpc");
        assert_eq!(block_number_requests.load(Ordering::Relaxed), 3);

        drop(submitter);
        server_task
            .await
            .expect("ws server task should finish cleanly");
    }

    #[tokio::test]
    async fn test_ws_submitter_warm_up_ignores_rpc_errors() {
        let block_number_requests = Arc::new(AtomicUsize::new(0));
        let block_number_requests_for_handler = Arc::clone(&block_number_requests);
        let handler = Arc::new(move |request: Value| {
            let id = request.get("id").cloned().unwrap_or_else(|| json!(1));
            assert_eq!(
                request.get("method").and_then(|m| m.as_str()),
                Some("eth_blockNumber")
            );
            block_number_requests_for_handler.fetch_add(1, Ordering::Relaxed);
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32000, "message": "temporarily unavailable"}
            })
        });
        let (url, _requests, server_task) = start_ws_rpc_server(handler).await;

        let submitter = WsSubmitter::new(&url, 100).expect("failed to create submitter");
        submitter
            .warm_up(2)
            .await
            .expect("warm_up should ignore individual block number errors");
        assert_eq!(block_number_requests.load(Ordering::Relaxed), 2);

        drop(submitter);
        server_task
            .await
            .expect("ws server task should finish cleanly");
    }

    #[tokio::test]
    async fn test_ws_submitter_submit_batch_success_across_chunks() {
        let hash_counter = Arc::new(AtomicUsize::new(0));
        let hash_counter_for_handler = Arc::clone(&hash_counter);
        let handler = Arc::new(move |request: Value| {
            let id = request.get("id").cloned().unwrap_or_else(|| json!(1));
            assert_eq!(
                request.get("method").and_then(|m| m.as_str()),
                Some("eth_sendRawTransaction")
            );
            let idx = hash_counter_for_handler.fetch_add(1, Ordering::Relaxed) + 1;
            let tx_hash = format!("0x{:064x}", idx);
            json!({"jsonrpc": "2.0", "id": id, "result": tx_hash})
        });
        let (url, requests, server_task) = start_ws_rpc_server(handler).await;

        let submitter = WsSubmitter::new(&url, 2).expect("failed to create submitter");
        let txs = vec![make_test_tx(0), make_test_tx(1), make_test_tx(2)];
        let result = submitter
            .submit_batch(txs.clone())
            .await
            .expect("submit_batch should succeed");

        assert_eq!(result.submitted, 3);
        assert_eq!(result.errors, 0);
        assert_eq!(result.hashes.len(), 3);
        assert_eq!(result.accepted_txs.len(), 3);
        assert_eq!(
            result
                .accepted_txs
                .iter()
                .map(|tx| tx.nonce)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );

        let send_count = requests
            .lock()
            .expect("request log mutex poisoned")
            .iter()
            .filter(|request| {
                request
                    .get("method")
                    .and_then(|m| m.as_str())
                    .is_some_and(|method| method == "eth_sendRawTransaction")
            })
            .count();
        assert_eq!(send_count, 3);

        drop(submitter);
        server_task
            .await
            .expect("ws server task should finish cleanly");
    }

    #[tokio::test]
    async fn test_ws_submitter_submit_single_delegates_to_batch() {
        let handler = Arc::new(move |request: Value| {
            let id = request.get("id").cloned().unwrap_or_else(|| json!(1));
            assert_eq!(
                request.get("method").and_then(|m| m.as_str()),
                Some("eth_sendRawTransaction")
            );
            json!({"jsonrpc": "2.0", "id": id, "result": format!("0x{:064x}", 7)})
        });
        let (url, requests, server_task) = start_ws_rpc_server(handler).await;

        let submitter = WsSubmitter::new(&url, 5).expect("failed to create submitter");
        let result = submitter
            .submit_single(make_test_tx(7))
            .await
            .expect("submit_single should succeed");

        assert_eq!(result.submitted, 1);
        assert_eq!(result.errors, 0);
        assert_eq!(result.hashes, vec![format!("0x{:064x}", 7)]);
        assert_eq!(result.accepted_txs.len(), 1);
        assert_eq!(
            requests
                .lock()
                .expect("request log mutex poisoned")
                .iter()
                .filter(|request| {
                    request
                        .get("method")
                        .and_then(|m| m.as_str())
                        .is_some_and(|method| method == "eth_sendRawTransaction")
                })
                .count(),
            1
        );

        drop(submitter);
        server_task
            .await
            .expect("ws server task should finish cleanly");
    }

    #[tokio::test]
    async fn test_ws_submitter_retries_transient_errors() {
        let attempts = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
        let attempts_for_handler = Arc::clone(&attempts);
        let handler = Arc::new(move |request: Value| {
            let id = request.get("id").cloned().unwrap_or_else(|| json!(1));
            assert_eq!(
                request.get("method").and_then(|m| m.as_str()),
                Some("eth_sendRawTransaction")
            );
            let raw_tx = request["params"][0]
                .as_str()
                .expect("raw transaction should be encoded as hex")
                .to_string();
            let mut attempts = attempts_for_handler
                .lock()
                .expect("attempt map mutex poisoned");
            let attempt = attempts.entry(raw_tx).or_insert(0);
            *attempt += 1;

            if *attempt == 1 {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32000, "message": "connection busy"}
                })
            } else {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": format!("0x{:064x}", 1)
                })
            }
        });
        let (url, _requests, server_task) = start_ws_rpc_server(handler).await;

        let submitter =
            WsSubmitter::with_retry_profile(&url, 1, "light").expect("failed to create submitter");
        let result = submitter
            .submit_batch(vec![make_test_tx(9)])
            .await
            .expect("submit_batch should succeed after a transient retry");

        assert_eq!(result.submitted, 1);
        assert_eq!(result.errors, 0);
        assert_eq!(result.hashes.len(), 1);
        assert_eq!(
            attempts
                .lock()
                .expect("attempt map mutex poisoned")
                .values()
                .copied()
                .next(),
            Some(2)
        );

        drop(submitter);
        server_task
            .await
            .expect("ws server task should finish cleanly");
    }

    #[tokio::test]
    async fn test_ws_submitter_does_not_retry_non_transient_errors() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_handler = Arc::clone(&attempts);
        let handler = Arc::new(move |request: Value| {
            let id = request.get("id").cloned().unwrap_or_else(|| json!(1));
            assert_eq!(
                request.get("method").and_then(|m| m.as_str()),
                Some("eth_sendRawTransaction")
            );
            attempts_for_handler.fetch_add(1, Ordering::Relaxed);
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32000, "message": "nonce too low"}
            })
        });
        let (url, _requests, server_task) = start_ws_rpc_server(handler).await;

        let submitter = WsSubmitter::with_retry_profile(&url, 1, "aggressive")
            .expect("failed to create submitter");
        let result = submitter
            .submit_batch(vec![make_test_tx(11)])
            .await
            .expect("submit_batch should return a result even on rpc rejection");

        assert_eq!(result.submitted, 0);
        assert_eq!(result.errors, 1);
        assert!(result.hashes.is_empty());
        assert!(result.accepted_txs.is_empty());
        assert_eq!(attempts.load(Ordering::Relaxed), 1);

        drop(submitter);
        server_task
            .await
            .expect("ws server task should finish cleanly");
    }

    #[tokio::test]
    async fn test_ws_rpc_server_handles_ping_binary_and_close_frames() {
        let handler =
            Arc::new(|_request: Value| json!({"jsonrpc": "2.0", "id": 1, "result": "0x1"}));
        let (url, requests, server_task) = start_ws_rpc_server(handler).await;

        let (mut client, _) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .expect("manual ws client should connect");
        client
            .send(Message::Ping(vec![9, 9]))
            .await
            .expect("manual ws client should send ping");
        let next_message = client
            .next()
            .await
            .expect("pong frame should arrive")
            .expect("pong frame should be readable");
        assert!(matches!(next_message, Message::Pong(_)));
        assert_eq!(next_message.into_data().to_vec(), vec![9, 9]);

        client
            .send(Message::Binary(vec![1, 2, 3]))
            .await
            .expect("manual ws client should send binary");
        client
            .send(Message::Close(None))
            .await
            .expect("manual ws client should send close");

        drop(client);
        server_task
            .await
            .expect("ws server task should finish cleanly");
        assert!(
            requests
                .lock()
                .expect("request log mutex poisoned")
                .is_empty()
        );
    }
}
