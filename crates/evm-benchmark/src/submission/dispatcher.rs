use crate::config::SubmissionMethod;
use crate::submission::rpc::SubmissionResult;
use crate::submission::rpc_dispatcher::RpcDispatcher;
use crate::submission::ws_submitter::WsSubmitter;
use crate::types::SignedTxWithMetadata;
use anyhow::Result;

/// Submission dispatcher that routes to HTTP (with round-robin failover) or WebSocket.
///
/// HTTP mode wraps [`RpcDispatcher`] which provides multi-endpoint round-robin
/// and automatic failover. WebSocket mode uses a single [`WsSubmitter`].
pub enum Submitter {
    /// HTTP RPC with round-robin failover across endpoints
    Http(RpcDispatcher),
    /// WebSocket RPC submitter (single endpoint)
    WebSocket(WsSubmitter),
}

impl Submitter {
    /// Create a new submitter based on the submission method.
    ///
    /// For HTTP, creates an [`RpcDispatcher`] with round-robin across all `rpc_urls`.
    /// For WebSocket, creates a [`WsSubmitter`] targeting `ws_url`.
    #[allow(dead_code)]
    pub fn new(
        rpc_urls: Vec<url::Url>,
        ws_url: &url::Url,
        batch_size: u32,
        method: SubmissionMethod,
    ) -> Result<Self> {
        Self::with_retry_profile(rpc_urls, ws_url, batch_size, method, "light")
    }

    /// Create a new submitter with an explicit retry profile name.
    pub fn with_retry_profile(
        rpc_urls: Vec<url::Url>,
        ws_url: &url::Url,
        batch_size: u32,
        method: SubmissionMethod,
        retry_profile: &str,
    ) -> Result<Self> {
        match method {
            SubmissionMethod::Http => {
                let dispatcher =
                    RpcDispatcher::with_retry_profile(rpc_urls, batch_size, retry_profile)?;
                Ok(Submitter::Http(dispatcher))
            }
            SubmissionMethod::WebSocket => {
                let submitter = WsSubmitter::with_retry_profile(ws_url, batch_size, retry_profile)?;
                Ok(Submitter::WebSocket(submitter))
            }
        }
    }

    /// Warm up connections before benchmarking.
    pub async fn warm_up(&self, dummy_request_count: u32) -> Result<()> {
        match self {
            Submitter::Http(dispatcher) => dispatcher.warm_up(dummy_request_count).await,
            Submitter::WebSocket(submitter) => submitter.warm_up(dummy_request_count).await,
        }
    }

    /// Submit a batch of transactions.
    pub async fn submit_batch(&self, txs: Vec<SignedTxWithMetadata>) -> Result<SubmissionResult> {
        match self {
            Submitter::Http(dispatcher) => dispatcher.submit_batch(txs).await,
            Submitter::WebSocket(submitter) => submitter.submit_batch(txs).await,
        }
    }

    /// Submit a single transaction.
    pub async fn submit_single(&self, tx: SignedTxWithMetadata) -> Result<SubmissionResult> {
        match self {
            Submitter::Http(dispatcher) => dispatcher.submit_single(tx).await,
            Submitter::WebSocket(submitter) => submitter.submit_single(tx).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TransactionType;
    use alloy_primitives::B256;
    use futures::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use std::sync::Arc;
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
    ) -> (url::Url, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind ws test server");
        let addr = listener
            .local_addr()
            .expect("failed to read ws test server address");

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

                let text = message
                    .into_text()
                    .expect("ws request should be sent as a text frame");
                let request: Value = serde_json::from_str(&text.to_string())
                    .expect("ws request should be valid json");
                let response = handler(request);
                socket
                    .send(Message::Text(response.to_string().into()))
                    .await
                    .expect("failed to send ws response");
            }
        });

        let url = url::Url::parse(&format!("ws://{addr}")).expect("failed to parse ws test url");
        (url, task)
    }

    #[test]
    fn test_dispatcher_http_creation() {
        let rpc_url = url::Url::parse("http://localhost:8545").expect("failed to parse url");
        let ws_url = url::Url::parse("ws://localhost:8546").expect("failed to parse url");
        let submitter = Submitter::new(vec![rpc_url], &ws_url, 100, SubmissionMethod::Http);
        assert!(submitter.is_ok());
    }

    #[test]
    fn test_dispatcher_http_multi_endpoint() {
        let rpc_urls = vec![
            url::Url::parse("http://localhost:8545").unwrap(),
            url::Url::parse("http://localhost:8555").unwrap(),
            url::Url::parse("http://localhost:8565").unwrap(),
        ];
        let ws_url = url::Url::parse("ws://localhost:8546").expect("failed to parse url");
        let submitter = Submitter::new(rpc_urls, &ws_url, 100, SubmissionMethod::Http);
        assert!(submitter.is_ok());
    }

    #[test]
    fn test_dispatcher_ws_creation() {
        let rpc_url = url::Url::parse("http://localhost:8545").expect("failed to parse url");
        let ws_url = url::Url::parse("ws://localhost:8546").expect("failed to parse url");
        let submitter = Submitter::new(vec![rpc_url], &ws_url, 100, SubmissionMethod::WebSocket);
        assert!(submitter.is_ok());
    }

    #[test]
    fn test_dispatcher_variant_is_http() {
        let rpc_url = url::Url::parse("http://localhost:8545").unwrap();
        let ws_url = url::Url::parse("ws://localhost:8546").unwrap();
        let submitter = Submitter::new(vec![rpc_url], &ws_url, 50, SubmissionMethod::Http).unwrap();
        assert!(matches!(submitter, Submitter::Http(_)));
    }

    #[test]
    fn test_dispatcher_variant_is_ws() {
        let rpc_url = url::Url::parse("http://localhost:8545").unwrap();
        let ws_url = url::Url::parse("ws://localhost:8546").unwrap();
        let submitter =
            Submitter::new(vec![rpc_url], &ws_url, 50, SubmissionMethod::WebSocket).unwrap();
        assert!(matches!(submitter, Submitter::WebSocket(_)));
    }

    /// Verify that warm_up on the HTTP variant delegates to RpcDispatcher.
    /// This will fail to connect (no server running) but proves the dispatch path works.
    #[tokio::test]
    async fn test_http_warm_up_dispatches() {
        let rpc_url = url::Url::parse("http://127.0.0.1:19999").unwrap();
        let ws_url = url::Url::parse("ws://127.0.0.1:19998").unwrap();
        let submitter = Submitter::new(vec![rpc_url], &ws_url, 10, SubmissionMethod::Http).unwrap();
        // warm_up with 0 requests should succeed immediately (no actual HTTP calls)
        let result = submitter.warm_up(0).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_submitter_new_empty_rpc_urls_http_fails() {
        let ws_url = url::Url::parse("ws://localhost:8546").unwrap();
        let result = Submitter::new(vec![], &ws_url, 100, SubmissionMethod::Http);
        let err = result.err().expect("should be an error");
        assert!(
            err.to_string().contains("At least one"),
            "Should fail with missing endpoint error"
        );
    }

    #[test]
    fn test_submitter_structure_http() {
        let rpc_url = url::Url::parse("http://localhost:8545").unwrap();
        let ws_url = url::Url::parse("ws://localhost:8546").unwrap();
        let submitter =
            Submitter::new(vec![rpc_url], &ws_url, 200, SubmissionMethod::Http).unwrap();

        assert!(matches!(submitter, Submitter::Http(_)));
        if let Submitter::Http(dispatcher) = &submitter {
            assert_eq!(dispatcher.endpoint_count(), 1);
        }
    }

    #[tokio::test]
    async fn test_ws_warm_up_dispatches() {
        let handler = Arc::new(|request: Value| {
            let id = request.get("id").cloned().unwrap_or_else(|| json!(1));
            assert_eq!(
                request.get("method").and_then(|m| m.as_str()),
                Some("eth_blockNumber")
            );
            json!({"jsonrpc": "2.0", "id": id, "result": "0x10"})
        });
        let (ws_url, server_task) = start_ws_rpc_server(handler).await;
        let rpc_url = url::Url::parse("http://127.0.0.1:19999").unwrap();
        let submitter =
            Submitter::new(vec![rpc_url], &ws_url, 10, SubmissionMethod::WebSocket).unwrap();

        let result = submitter.warm_up(2).await;
        assert!(result.is_ok());

        server_task
            .await
            .expect("ws server task should finish cleanly");
    }

    #[tokio::test]
    async fn test_ws_submit_batch_dispatches() {
        let handler = Arc::new(|request: Value| {
            let id = request.get("id").cloned().unwrap_or_else(|| json!(1));
            assert_eq!(
                request.get("method").and_then(|m| m.as_str()),
                Some("eth_sendRawTransaction")
            );
            json!({"jsonrpc": "2.0", "id": id, "result": format!("0x{:064x}", 1)})
        });
        let (ws_url, server_task) = start_ws_rpc_server(handler).await;
        let rpc_url = url::Url::parse("http://127.0.0.1:19999").unwrap();
        let submitter =
            Submitter::new(vec![rpc_url], &ws_url, 10, SubmissionMethod::WebSocket).unwrap();

        let result = submitter
            .submit_batch(vec![make_test_tx(1)])
            .await
            .expect("ws submit_batch should delegate");
        assert_eq!(result.submitted, 1);
        assert_eq!(result.errors, 0);

        server_task
            .await
            .expect("ws server task should finish cleanly");
    }

    #[tokio::test]
    async fn test_ws_submit_single_dispatches() {
        let handler = Arc::new(|request: Value| {
            let id = request.get("id").cloned().unwrap_or_else(|| json!(1));
            assert_eq!(
                request.get("method").and_then(|m| m.as_str()),
                Some("eth_sendRawTransaction")
            );
            json!({"jsonrpc": "2.0", "id": id, "result": format!("0x{:064x}", 7)})
        });
        let (ws_url, server_task) = start_ws_rpc_server(handler).await;
        let rpc_url = url::Url::parse("http://127.0.0.1:19999").unwrap();
        let submitter =
            Submitter::new(vec![rpc_url], &ws_url, 10, SubmissionMethod::WebSocket).unwrap();

        let result = submitter
            .submit_single(make_test_tx(7))
            .await
            .expect("ws submit_single should delegate");
        assert_eq!(result.submitted, 1);
        assert_eq!(result.errors, 0);

        server_task
            .await
            .expect("ws server task should finish cleanly");
    }

    #[tokio::test]
    async fn test_http_submit_batch_with_mock() {
        use alloy_primitives::{Address, B256};
        use std::time::Instant;
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        // Set up mock to return a successful batch response
        let response_body = serde_json::json!([
            {"jsonrpc": "2.0", "id": 0, "result": "0xaaaa"},
            {"jsonrpc": "2.0", "id": 1, "result": "0xbbbb"}
        ]);

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
            .mount(&mock_server)
            .await;

        let rpc_url = url::Url::parse(&mock_server.uri()).unwrap();
        let ws_url = url::Url::parse("ws://127.0.0.1:19998").unwrap();
        let submitter =
            Submitter::new(vec![rpc_url], &ws_url, 100, SubmissionMethod::Http).unwrap();

        let txs = vec![
            SignedTxWithMetadata {
                hash: B256::with_last_byte(0x01),
                encoded: vec![0x01, 0x02],
                nonce: 0,
                gas_limit: 21_000,
                sender: Address::default(),
                submit_time: Instant::now(),
                method: crate::types::TransactionType::SimpleTransfer,
            },
            SignedTxWithMetadata {
                hash: B256::with_last_byte(0x02),
                encoded: vec![0x03, 0x04],
                nonce: 1,
                gas_limit: 21_000,
                sender: Address::default(),
                submit_time: Instant::now(),
                method: crate::types::TransactionType::SimpleTransfer,
            },
        ];

        let result = submitter.submit_batch(txs).await.unwrap();
        assert_eq!(result.submitted, 2);
        assert_eq!(result.errors, 0);
        assert_eq!(result.hashes.len(), 2);
        assert_eq!(result.accepted_txs.len(), 2);
    }

    #[tokio::test]
    async fn test_http_submit_single_with_mock() {
        use alloy_primitives::{Address, B256};
        use std::time::Instant;
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        let response_body = serde_json::json!([
            {"jsonrpc": "2.0", "id": 0, "result": "0xdeadbeef"}
        ]);

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
            .mount(&mock_server)
            .await;

        let rpc_url = url::Url::parse(&mock_server.uri()).unwrap();
        let ws_url = url::Url::parse("ws://127.0.0.1:19998").unwrap();
        let submitter =
            Submitter::new(vec![rpc_url], &ws_url, 100, SubmissionMethod::Http).unwrap();

        let tx = SignedTxWithMetadata {
            hash: B256::with_last_byte(0x01),
            encoded: vec![0xab, 0xcd],
            nonce: 0,
            gas_limit: 21_000,
            sender: Address::default(),
            submit_time: Instant::now(),
            method: crate::types::TransactionType::SimpleTransfer,
        };

        let result = submitter.submit_single(tx).await.unwrap();
        assert_eq!(result.submitted, 1);
        assert_eq!(result.errors, 0);
        assert_eq!(result.hashes, vec!["0xdeadbeef"]);
    }

    #[tokio::test]
    async fn test_http_submit_batch_with_errors() {
        use alloy_primitives::{Address, B256};
        use std::time::Instant;
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        // Mix of success and error responses
        let response_body = serde_json::json!([
            {"jsonrpc": "2.0", "id": 0, "result": "0xaaaa"},
            {"jsonrpc": "2.0", "id": 1, "error": {"code": -32000, "message": "nonce too low"}}
        ]);

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
            .mount(&mock_server)
            .await;

        let rpc_url = url::Url::parse(&mock_server.uri()).unwrap();
        let ws_url = url::Url::parse("ws://127.0.0.1:19998").unwrap();
        let submitter =
            Submitter::new(vec![rpc_url], &ws_url, 100, SubmissionMethod::Http).unwrap();

        let txs = vec![
            SignedTxWithMetadata {
                hash: B256::with_last_byte(0x01),
                encoded: vec![0x01],
                nonce: 0,
                gas_limit: 21_000,
                sender: Address::default(),
                submit_time: Instant::now(),
                method: crate::types::TransactionType::SimpleTransfer,
            },
            SignedTxWithMetadata {
                hash: B256::with_last_byte(0x02),
                encoded: vec![0x02],
                nonce: 1,
                gas_limit: 21_000,
                sender: Address::default(),
                submit_time: Instant::now(),
                method: crate::types::TransactionType::SimpleTransfer,
            },
        ];

        let result = submitter.submit_batch(txs).await.unwrap();
        assert_eq!(result.submitted, 1);
        assert_eq!(result.errors, 1);
        assert_eq!(result.hashes.len(), 1);
    }

    #[tokio::test]
    async fn test_http_warm_up_with_mock() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        let response_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": "0x1"
        });

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
            .expect(2)
            .mount(&mock_server)
            .await;

        let rpc_url = url::Url::parse(&mock_server.uri()).unwrap();
        let ws_url = url::Url::parse("ws://127.0.0.1:19998").unwrap();
        let submitter =
            Submitter::new(vec![rpc_url], &ws_url, 100, SubmissionMethod::Http).unwrap();

        let result = submitter.warm_up(2).await;
        assert!(result.is_ok());
    }
}
