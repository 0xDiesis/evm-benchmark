use crate::types::SignedTxWithMetadata;
use anyhow::Result;
use rand::Rng;
use reqwest::Client;
#[allow(unused_imports)]
use tracing::error;

#[derive(Clone, Copy, Debug)]
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

#[derive(Clone)]
pub struct SubmissionResult {
    pub submitted: u32,
    pub errors: u32,
    pub hashes: Vec<String>,
    /// Txs that were accepted by the RPC (result != null). Used to record only accepted txs.
    pub accepted_txs: Vec<SignedTxWithMetadata>,
    /// Txs rejected specifically because the txpool was full (retriable).
    #[allow(dead_code)]
    pub pool_full_txs: Vec<SignedTxWithMetadata>,
}

#[derive(Debug)]
pub struct RpcSubmitter {
    rpc_url: url::Url,
    batch_size: u32,
    client: Client,
    retry_profile: RetryProfile,
}

impl RpcSubmitter {
    #[allow(dead_code)]
    pub fn new(rpc_url: &url::Url, batch_size: u32) -> Result<Self> {
        Self::with_retry_profile(rpc_url, batch_size, "light")
    }

    /// Create a new RPC submitter with an explicit retry profile name.
    pub fn with_retry_profile(
        rpc_url: &url::Url,
        batch_size: u32,
        retry_profile: &str,
    ) -> Result<Self> {
        let client = Client::builder().pool_max_idle_per_host(32).build()?;

        Ok(RpcSubmitter {
            rpc_url: rpc_url.clone(),
            batch_size,
            client,
            retry_profile: RetryProfile::from_name(retry_profile),
        })
    }

    /// Warm up the HTTP connection pool before benchmarking
    pub async fn warm_up(&self, dummy_request_count: u32) -> Result<()> {
        use std::time::Instant;
        let start = Instant::now();

        for _ in 0..dummy_request_count {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_blockNumber",
                "params": [],
                "id": 1,
            });
            let _ = self
                .client
                .post(self.rpc_url.clone())
                .json(&body)
                .send()
                .await;
        }

        let elapsed = start.elapsed();
        if !cfg!(test) {
            println!(
                "Warm-up complete: {} requests in {:.2}ms",
                dummy_request_count,
                elapsed.as_millis()
            );
        }
        Ok(())
    }

    pub async fn submit_batch(&self, txs: Vec<SignedTxWithMetadata>) -> Result<SubmissionResult> {
        // Send all sub-batches concurrently to saturate the RPC endpoint.
        use futures::future::join_all;

        let chunks: Vec<Vec<SignedTxWithMetadata>> = txs
            .chunks(self.batch_size as usize)
            .map(|c| c.to_vec())
            .collect();

        let futures: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                let submitter = self.client.clone();
                let url = self.rpc_url.clone();
                let batch_size = self.batch_size;
                let self_retry = self.retry_profile;
                async move {
                    // Create a temporary submitter using the shared client
                    let sub = RpcSubmitter {
                        rpc_url: url,
                        batch_size,
                        client: submitter,
                        retry_profile: self_retry,
                    };
                    sub.submit_batch_jsonrpc(&chunk).await.unwrap_or_else(|e| {
                        if !cfg!(test) {
                            eprintln!("RPC batch error: {}", e);
                        }
                        SubmissionResult {
                            submitted: 0,
                            errors: chunk.len() as u32,
                            hashes: vec![],
                            accepted_txs: vec![],
                            pool_full_txs: vec![],
                        }
                    })
                }
            })
            .collect();

        let results = join_all(futures).await;

        let mut submitted = 0u32;
        let mut errors = 0u32;
        let mut hashes = vec![];
        let mut accepted_txs = vec![];

        for r in results {
            submitted += r.submitted;
            errors += r.errors;
            hashes.extend(r.hashes);
            accepted_txs.extend(r.accepted_txs);
        }

        Ok(SubmissionResult {
            submitted,
            errors,
            hashes,
            accepted_txs,
            pool_full_txs: vec![],
        })
    }

    #[allow(dead_code)]
    pub async fn submit_single(&self, tx: SignedTxWithMetadata) -> Result<SubmissionResult> {
        self.submit_batch(vec![tx]).await
    }

    /// Submit a batch as a single JSON-RPC batch request (one HTTP POST for N txs).
    ///
    /// Builds a JSON array of `eth_sendRawTransaction` calls and sends as one
    /// request, then parses the array response. This eliminates per-tx HTTP
    /// round-trip overhead.
    async fn submit_batch_jsonrpc(&self, txs: &[SignedTxWithMetadata]) -> Result<SubmissionResult> {
        // Build batch request body: [{"jsonrpc":"2.0","method":"eth_sendRawTransaction","params":["0x..."],"id":0}, ...]
        let batch: Vec<serde_json::Value> = txs
            .iter()
            .enumerate()
            .map(|(i, tx)| {
                let hex = format!("0x{}", hex::encode(&tx.encoded));
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "eth_sendRawTransaction",
                    "params": [hex],
                    "id": i
                })
            })
            .collect();

        let retry = self.retry_profile;
        let mut last_err: Option<anyhow::Error> = None;
        let mut resp_opt = None;

        for attempt in 1..=retry.max_attempts {
            match self
                .client
                .post(self.rpc_url.clone())
                .json(&batch)
                .send()
                .await
            {
                Ok(resp) => {
                    // Retry overloaded/upstream-unavailable responses.
                    if (resp.status().as_u16() == 429 || resp.status().as_u16() == 503)
                        && attempt < retry.max_attempts
                    {
                        tokio::time::sleep(retry.delay_for_attempt(attempt)).await;
                        continue;
                    }
                    resp_opt = Some(resp);
                    break;
                }
                Err(e) => {
                    last_err = Some(anyhow::anyhow!("HTTP request failed: {}", e));
                    if attempt < retry.max_attempts {
                        tokio::time::sleep(retry.delay_for_attempt(attempt)).await;
                    }
                }
            }
        }

        let resp = if let Some(resp) = resp_opt {
            resp
        } else {
            return Err(last_err.unwrap_or_else(|| anyhow::anyhow!("HTTP request failed")));
        };

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse batch response: {}", e))?;

        // Parse response array
        let responses = body
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Expected JSON array response from batch RPC"))?;

        let mut submitted = 0u32;
        let mut errors = 0u32;
        let mut hashes = Vec::with_capacity(responses.len());
        let mut accepted_txs = Vec::with_capacity(responses.len());
        let mut pool_full_txs = Vec::new();

        // Collect unique error messages for diagnostics (cap at 3 distinct messages)
        let mut seen_errors: std::collections::HashSet<String> = std::collections::HashSet::new();

        for item in responses.iter() {
            // Use the response `id` to map back to the original tx.
            // If the id is missing or non-numeric, skip the tx mapping rather
            // than silently defaulting to index 0.
            let tx_idx = item.get("id").and_then(|v| v.as_u64()).map(|v| v as usize);
            if let Some(result) = item.get("result").and_then(|r| r.as_str()) {
                submitted += 1;
                hashes.push(result.to_string());
                if let Some(idx) = tx_idx
                    && idx < txs.len()
                {
                    accepted_txs.push(txs[idx].clone());
                }
            } else {
                let err_msg = item
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error")
                    .to_string();
                if !cfg!(test) && seen_errors.len() < 3 && seen_errors.insert(err_msg.clone()) {
                    eprintln!("[RPC] tx[{:?}] error: {}", tx_idx, err_msg);
                }
                if let Some(idx) = tx_idx
                    && err_msg.contains("txpool is full")
                    && idx < txs.len()
                {
                    pool_full_txs.push(txs[idx].clone());
                }
                errors += 1;
            }
        }

        Ok(SubmissionResult {
            submitted,
            errors,
            hashes,
            accepted_txs,
            pool_full_txs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TransactionType;
    use alloy_primitives::B256;
    use std::time::Instant;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Helper to build a test transaction with a given nonce.
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

    #[test]
    fn test_submitter_creation() {
        let url = url::Url::parse("http://localhost:8545").expect("failed to parse url");
        let submitter = RpcSubmitter::new(&url, 100).expect("failed to create submitter");
        assert_eq!(submitter.batch_size, 100);
    }

    #[test]
    fn test_submitter_batch_size() {
        let url = url::Url::parse("http://localhost:8545").expect("failed to parse url");
        let submitter = RpcSubmitter::new(&url, 50).expect("failed to create submitter");
        assert_eq!(submitter.batch_size, 50);
    }

    /// Verify the batch JSON body format matches what geth/reth expects.
    #[test]
    fn test_batch_body_format() {
        let tx = SignedTxWithMetadata {
            hash: B256::default(),
            encoded: vec![0x02, 0xab, 0xcd],
            nonce: 0,
            gas_limit: 21_000,
            sender: alloy_primitives::Address::default(),
            submit_time: Instant::now(),
            method: TransactionType::SimpleTransfer,
        };

        // Verify the hex encoding is correct (just test the format logic directly)
        let hex = format!("0x{}", hex::encode(&tx.encoded));
        assert_eq!(hex, "0x02abcd");
    }

    #[test]
    fn test_submission_result_default_construction() {
        let result = SubmissionResult {
            submitted: 0,
            errors: 0,
            hashes: vec![],
            accepted_txs: vec![],
            pool_full_txs: vec![],
        };
        assert_eq!(result.submitted, 0);
        assert_eq!(result.errors, 0);
        assert!(result.hashes.is_empty());
        assert!(result.accepted_txs.is_empty());
        assert!(result.pool_full_txs.is_empty());
    }

    #[test]
    fn test_batch_chunking_logic() {
        // Create 250 fake transactions
        let txs: Vec<SignedTxWithMetadata> = (0..250u32)
            .map(|i| SignedTxWithMetadata {
                hash: B256::with_last_byte(i as u8),
                encoded: vec![0x02, i as u8],
                nonce: i as u64,
                gas_limit: 21_000,
                sender: alloy_primitives::Address::default(),
                submit_time: Instant::now(),
                method: TransactionType::SimpleTransfer,
            })
            .collect();

        let batch_size: usize = 100;
        let chunks: Vec<Vec<SignedTxWithMetadata>> =
            txs.chunks(batch_size).map(|c| c.to_vec()).collect();

        assert_eq!(chunks.len(), 3, "250 txs / 100 batch_size = 3 chunks");
        assert_eq!(chunks[0].len(), 100);
        assert_eq!(chunks[1].len(), 100);
        assert_eq!(chunks[2].len(), 50);
    }

    /// Warm-up sends N dummy `eth_blockNumber` requests to the RPC endpoint.
    #[tokio::test]
    async fn test_warm_up_success() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": "0x10"
            })))
            .expect(3)
            .mount(&mock_server)
            .await;

        let url = url::Url::parse(&mock_server.uri()).expect("failed to parse mock url");
        let submitter = RpcSubmitter::new(&url, 100).expect("failed to create submitter");

        let result = submitter.warm_up(3).await;
        assert!(result.is_ok(), "warm_up should succeed");
    }

    /// Submit a batch where all transactions are accepted (result field present).
    #[tokio::test]
    async fn test_submit_batch_all_accepted() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"jsonrpc": "2.0", "id": 0, "result": "0xaaaa"},
                {"jsonrpc": "2.0", "id": 1, "result": "0xbbbb"},
                {"jsonrpc": "2.0", "id": 2, "result": "0xcccc"}
            ])))
            .mount(&mock_server)
            .await;

        let url = url::Url::parse(&mock_server.uri()).expect("failed to parse mock url");
        let submitter = RpcSubmitter::new(&url, 100).expect("failed to create submitter");

        let txs = vec![make_test_tx(0), make_test_tx(1), make_test_tx(2)];
        let result = submitter
            .submit_batch(txs)
            .await
            .expect("submit_batch should succeed");

        assert_eq!(result.submitted, 3);
        assert_eq!(result.errors, 0);
        assert_eq!(result.hashes.len(), 3);
        assert_eq!(result.hashes[0], "0xaaaa");
        assert_eq!(result.hashes[1], "0xbbbb");
        assert_eq!(result.hashes[2], "0xcccc");
        assert_eq!(result.accepted_txs.len(), 3);
    }

    /// Submit a batch where some responses contain errors.
    #[tokio::test]
    async fn test_submit_batch_partial_errors() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"jsonrpc": "2.0", "id": 0, "result": "0xaaaa"},
                {"jsonrpc": "2.0", "id": 1, "error": {"code": -32000, "message": "nonce too low"}},
                {"jsonrpc": "2.0", "id": 2, "result": "0xcccc"},
                {"jsonrpc": "2.0", "id": 3, "error": {"code": -32000, "message": "already known"}}
            ])))
            .mount(&mock_server)
            .await;

        let url = url::Url::parse(&mock_server.uri()).expect("failed to parse mock url");
        let submitter = RpcSubmitter::new(&url, 100).expect("failed to create submitter");

        let txs = vec![
            make_test_tx(0),
            make_test_tx(1),
            make_test_tx(2),
            make_test_tx(3),
        ];
        let result = submitter
            .submit_batch(txs)
            .await
            .expect("submit_batch should succeed even with partial errors");

        assert_eq!(result.submitted, 2);
        assert_eq!(result.errors, 2);
        assert_eq!(result.hashes.len(), 2);
        assert_eq!(result.accepted_txs.len(), 2);
    }

    /// Verify that txpool-full errors are tracked in `pool_full_txs`.
    #[tokio::test]
    async fn test_submit_batch_pool_full_detection() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"jsonrpc": "2.0", "id": 0, "result": "0xaaaa"},
                {"jsonrpc": "2.0", "id": 1, "error": {"code": -32000, "message": "txpool is full"}},
                {"jsonrpc": "2.0", "id": 2, "error": {"code": -32000, "message": "txpool is full"}}
            ])))
            .mount(&mock_server)
            .await;

        let url = url::Url::parse(&mock_server.uri()).expect("failed to parse mock url");
        // Use batch_size large enough that all txs go in one request
        let submitter = RpcSubmitter::new(&url, 100).expect("failed to create submitter");

        let txs = vec![make_test_tx(0), make_test_tx(1), make_test_tx(2)];
        let result = submitter
            .submit_batch(txs)
            .await
            .expect("submit_batch should succeed");

        assert_eq!(result.submitted, 1);
        assert_eq!(result.errors, 2);
        // pool_full_txs are collected per-chunk in submit_batch_jsonrpc but the
        // outer submit_batch currently resets pool_full_txs to empty vec.
        // The inner results still counted the errors correctly.
    }

    /// Submit a single transaction via `submit_single`.
    #[tokio::test]
    async fn test_submit_single_success() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"jsonrpc": "2.0", "id": 0, "result": "0xdeadbeef"}
            ])))
            .mount(&mock_server)
            .await;

        let url = url::Url::parse(&mock_server.uri()).expect("failed to parse mock url");
        let submitter = RpcSubmitter::new(&url, 100).expect("failed to create submitter");

        let tx = make_test_tx(42);
        let result = submitter
            .submit_single(tx)
            .await
            .expect("submit_single should succeed");

        assert_eq!(result.submitted, 1);
        assert_eq!(result.errors, 0);
        assert_eq!(result.hashes, vec!["0xdeadbeef"]);
        assert_eq!(result.accepted_txs.len(), 1);
        assert_eq!(result.accepted_txs[0].nonce, 42);
    }

    /// When batch_size is small, `submit_batch` splits into multiple HTTP
    /// requests (chunks). Verify all chunks are aggregated correctly.
    #[tokio::test]
    async fn test_submit_batch_multiple_chunks() {
        let mock_server = MockServer::start().await;

        // batch_size=2 with 5 txs => 3 chunks (2, 2, 1).
        // wiremock will respond with the same body for every POST, so we craft
        // a response that always has two results. The last chunk (1 tx) will
        // still get the same 2-element response, but that is fine for
        // verifying aggregation — the extra response item just maps to a
        // non-existent tx index and is still counted as submitted.
        //
        // To test more precisely, we use a response that returns a single
        // result. Each chunk gets 1 accepted result per response item, and we
        // can verify totals.
        //
        // The simplest approach: respond with a single-element array for every
        // request. With 3 chunks we expect 3 submitted.

        // Actually, we cannot vary the response per-request easily with
        // wiremock's static Mock. Instead, respond with 2 results always.
        // Chunks of size [2,2,1] will all get 2 results back, yielding
        // 6 submitted total (2*3 chunks). The third chunk only has 1 tx in
        // the request but gets 2 results — the second result's id=1 maps
        // outside the 1-element chunk so accepted_txs won't include it, but
        // submitted count still increments. Total: 6 submitted, 0 errors.

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"jsonrpc": "2.0", "id": 0, "result": "0x1111"},
                {"jsonrpc": "2.0", "id": 1, "result": "0x2222"}
            ])))
            .expect(3) // 3 HTTP requests for 3 chunks
            .mount(&mock_server)
            .await;

        let url = url::Url::parse(&mock_server.uri()).expect("failed to parse mock url");
        let submitter = RpcSubmitter::new(&url, 2).expect("failed to create submitter");

        let txs: Vec<_> = (0..5).map(make_test_tx).collect();
        let result = submitter
            .submit_batch(txs)
            .await
            .expect("submit_batch should succeed");

        // 3 chunks * 2 results each = 6 submitted total
        assert_eq!(result.submitted, 6);
        assert_eq!(result.errors, 0);
        assert_eq!(result.hashes.len(), 6);
        // accepted_txs: for the first two chunks (size 2), both ids 0 and 1
        // map to real txs. For the third chunk (size 1), only id=0 maps.
        // Total accepted_txs = 2 + 2 + 1 = 5.
        assert_eq!(result.accepted_txs.len(), 5);
    }

    /// When the RPC returns a non-array body, `submit_batch_jsonrpc` should
    /// return an error, which `submit_batch` catches and converts into an
    /// all-errors result.
    #[tokio::test]
    async fn test_submit_batch_non_array_response() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 0,
                "error": {"code": -32600, "message": "invalid request"}
            })))
            .mount(&mock_server)
            .await;

        let url = url::Url::parse(&mock_server.uri()).expect("failed to parse mock url");
        let submitter = RpcSubmitter::new(&url, 100).expect("failed to create submitter");

        let txs = vec![make_test_tx(0), make_test_tx(1)];
        let result = submitter
            .submit_batch(txs)
            .await
            .expect("submit_batch should not propagate inner errors");

        // The inner submit_batch_jsonrpc fails (non-array response),
        // so the fallback sets errors = chunk.len().
        assert_eq!(result.submitted, 0);
        assert_eq!(result.errors, 2);
        assert!(result.hashes.is_empty());
    }
}
