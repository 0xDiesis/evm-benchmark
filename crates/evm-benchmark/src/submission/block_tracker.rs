use crate::submission::LatencyTracker;
use anyhow::Result;
use std::sync::Arc;
use std::time::{Duration, Instant};
use url::Url;

/// Tracks block inclusion to measure transaction latency.
///
/// Uses raw WebSocket `eth_subscribe("newHeads")` for real-time block
/// notifications. Falls back to HTTP polling if WS fails.
///
/// `http_rpc_url` is the HTTP endpoint for fetching block bodies (may differ
/// from the WS endpoint — e.g. ws://localhost:8546 + http://localhost:8545).
#[allow(dead_code)]
pub struct BlockTracker {
    ws_url: Url,
    http_rpc_url: Url,
    tracker: Arc<LatencyTracker>,
    finality_confirmations: u32,
}

impl BlockTracker {
    #[allow(dead_code)]
    pub fn new(ws_url: Url, http_rpc_url: Url, tracker: Arc<LatencyTracker>) -> Self {
        Self::with_finality(ws_url, http_rpc_url, tracker, 0)
    }

    /// Create a block tracker with an explicit finality confirmation depth.
    #[allow(dead_code)]
    pub fn with_finality(
        ws_url: Url,
        http_rpc_url: Url,
        tracker: Arc<LatencyTracker>,
        finality_confirmations: u32,
    ) -> Self {
        BlockTracker {
            ws_url,
            http_rpc_url,
            tracker,
            finality_confirmations,
        }
    }

    /// Run block tracking for the specified timeout duration.
    pub async fn run(&self, timeout: Duration) -> Result<()> {
        self.run_with_ready_opt(timeout, None).await
    }

    /// Run block tracking, signalling `ready_tx` once tracking is active.
    ///
    /// Signals ready as soon as the WS subscription is established so callers
    /// can wait before submitting transactions and avoid missing early blocks.
    #[allow(dead_code)]
    pub async fn run_with_ready(
        &self,
        timeout: Duration,
        ready_tx: tokio::sync::oneshot::Sender<()>,
    ) -> Result<()> {
        self.run_with_ready_opt(timeout, Some(ready_tx)).await
    }

    async fn run_with_ready_opt(
        &self,
        timeout: Duration,
        ready_tx: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<()> {
        match self.run_ws_raw(timeout, ready_tx).await {
            Ok(_) => Ok(()),
            Err(e) => {
                eprintln!(
                    "[BlockTracker] WS failed ({}), falling back to HTTP polling",
                    e
                );
                self.run_http_polling(timeout, None).await
            }
        }
    }

    /// Subscribe to new block headers using raw WebSocket (tokio-tungstenite).
    ///
    /// On each new head, fetches the block body via HTTP to match tx hashes.
    async fn run_ws_raw(
        &self,
        timeout: Duration,
        ready_tx: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<()> {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

        let (mut ws_stream, _) = connect_async(self.ws_url.as_str())
            .await
            .map_err(|e| anyhow::anyhow!("WS connect failed: {}", e))?;

        // Subscribe to new block headers
        let sub_msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_subscribe",
            "params": ["newHeads"],
            "id": 1
        });
        ws_stream
            .send(Message::Text(sub_msg.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("WS send failed: {}", e))?;

        // Read the subscription confirmation
        let sub_resp = tokio::time::timeout(Duration::from_secs(5), ws_stream.next())
            .await
            .map_err(|_| anyhow::anyhow!("WS subscription confirmation timeout"))?
            .ok_or_else(|| anyhow::anyhow!("WS stream ended before subscription confirmed"))?
            .map_err(|e| anyhow::anyhow!("WS error: {}", e))?;

        // Verify we got a subscription ID
        if let Message::Text(text) = sub_resp {
            let resp: serde_json::Value = serde_json::from_str(&text.to_string())
                .map_err(|e| anyhow::anyhow!("WS subscription response parse error: {}", e))?;
            if resp.get("result").is_none() {
                return Err(anyhow::anyhow!("WS subscription failed: {:?}", resp));
            }
        }

        // Signal ready — subscription is live.
        if let Some(tx) = ready_tx {
            let _ = tx.send(());
        }

        let http_url: Url = self.http_rpc_url.clone();
        let http_client = reqwest::Client::builder()
            .pool_max_idle_per_host(16)
            .build()
            .unwrap_or_default();
        let deadline = Instant::now() + timeout;
        let tracker = self.tracker.clone();

        // Event loop: process incoming WS messages for new block headers.
        // Block body fetches are spawned as background tasks so slow HTTP fetches don't
        // block the WS receive loop and cause missed newHeads notifications.
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let wait = remaining.min(Duration::from_secs(5));

            match tokio::time::timeout(wait, ws_stream.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    let text_str = text.to_string();
                    // Parse the eth_subscription notification
                    if let Ok(msg) = serde_json::from_str::<serde_json::Value>(&text_str)
                        && let Some(block_num_hex) = msg
                            .get("params")
                            .and_then(|p| p.get("result"))
                            .and_then(|r| r.get("number"))
                            .and_then(|n| n.as_str())
                        && let Ok(block_num) =
                            u64::from_str_radix(block_num_hex.trim_start_matches("0x"), 16)
                    {
                        let arrival = Instant::now();
                        // Fetch synchronously — eth_getBlockReceipts is fast and
                        // keeping the WS loop in sync prevents stall detection issues.
                        Self::fetch_and_track_block(
                            &http_client,
                            &http_url,
                            block_num,
                            &tracker,
                            arrival,
                            self.finality_confirmations,
                        )
                        .await;
                    }
                }
                Ok(Some(Ok(Message::Ping(data)))) => {
                    // Respond to pings to keep connection alive
                    let _ = ws_stream.send(Message::Pong(data)).await;
                }
                Ok(Some(Ok(_))) => {} // ignore binary/pong/close frames
                Ok(Some(Err(e))) => {
                    return Err(anyhow::anyhow!("WS stream error: {}", e));
                }
                Ok(None) => {
                    return Err(anyhow::anyhow!("WS stream ended unexpectedly"));
                }
                Err(_) => {
                    // Timeout — check if deadline passed
                    if Instant::now() >= deadline {
                        break;
                    }
                    // Chain might be slow, keep waiting
                }
            }
        }

        Ok(())
    }

    async fn fetch_and_track_block(
        http_client: &reqwest::Client,
        http_url: &Url,
        block_num: u64,
        tracker: &Arc<LatencyTracker>,
        arrival: Instant,
        finality_confirmations: u32,
    ) {
        // Finality-stress mode: defer confirmations to receipt polling logic
        // so we can apply required confirmation depth checks consistently.
        let finality_depth = finality_confirmations as u64;
        if finality_depth > 0 {
            return;
        }

        // Use eth_getBlockReceipts for efficiency: one call returns all tx hashes
        // without needing to fetch the full block body or individual receipts.
        let receipts_payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_getBlockReceipts",
            "params": [format!("0x{:x}", block_num)],
            "id": 1
        });

        if let Ok(resp) = http_client
            .post(http_url.clone())
            .json(&receipts_payload)
            .send()
            .await
            && let Ok(body) = resp.json::<serde_json::Value>().await
            && let Some(receipts) = body.get("result").and_then(|r| r.as_array())
        {
            for receipt in receipts {
                if let Some(hash_str) = receipt.get("transactionHash").and_then(|h| h.as_str())
                    && let Ok(hash) = hash_str.parse()
                {
                    tracker.on_block_inclusion(hash, arrival);
                }
            }
            return; // eth_getBlockReceipts succeeded
        }

        // Fallback: fetch block body with tx hashes if eth_getBlockReceipts fails
        let block_payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_getBlockByNumber",
            "params": [format!("0x{:x}", block_num), false],
            "id": 1
        });

        if let Ok(resp) = http_client
            .post(http_url.clone())
            .json(&block_payload)
            .send()
            .await
            && let Ok(body) = resp.json::<serde_json::Value>().await
            && let Some(txs) = body
                .get("result")
                .and_then(|b| b.get("transactions"))
                .and_then(|t| t.as_array())
        {
            for tx in txs {
                if let Some(hash_str) = tx.as_str()
                    && let Ok(hash) = hash_str.parse()
                {
                    tracker.on_block_inclusion(hash, arrival);
                }
            }
        }
    }

    /// Run HTTP polling as the primary tracker (no WS), with 25ms poll interval.
    ///
    /// Used as the primary tracker in burst mode where low latency and reliability
    /// matter more than connection overhead.
    #[allow(dead_code)]
    pub async fn run_http_only(&self, timeout: Duration) -> Result<()> {
        self.run_http_polling_with_interval(timeout, None, Duration::from_millis(25))
            .await
    }

    /// HTTP polling fallback: poll eth_blockNumber every 100ms.
    async fn run_http_polling(
        &self,
        timeout: Duration,
        ready_tx: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<()> {
        self.run_http_polling_with_interval(timeout, ready_tx, Duration::from_millis(100))
            .await
    }

    async fn run_http_polling_with_interval(
        &self,
        timeout: Duration,
        ready_tx: Option<tokio::sync::oneshot::Sender<()>>,
        poll_interval: Duration,
    ) -> Result<()> {
        let client = reqwest::Client::new();
        let http_url = self.http_rpc_url.clone();
        let deadline = Instant::now() + timeout;

        // Start from current block to avoid reprocessing history
        let current_block = self.fetch_block_number(&client).await.unwrap_or(0);
        let mut last_processed = current_block.saturating_sub(1);

        // Signal ready
        if let Some(tx) = ready_tx {
            let _ = tx.send(());
        }

        while Instant::now() < deadline {
            if let Some(current) = self.fetch_block_number(&client).await
                && current > last_processed
            {
                // Process all new blocks concurrently using eth_getBlockReceipts.
                // Await all fetches before returning so pending_count() is accurate.
                let new_blocks: Vec<u64> = (last_processed + 1..=current).collect();
                let finality = self.finality_confirmations;
                let futs: Vec<_> = new_blocks
                    .into_iter()
                    .map(|block_num| {
                        let c = client.clone();
                        let u = http_url.clone();
                        let t = self.tracker.clone();
                        let arrival = Instant::now();
                        async move {
                            Self::fetch_and_track_block(&c, &u, block_num, &t, arrival, finality)
                                .await;
                        }
                    })
                    .collect();
                futures::future::join_all(futs).await;
                last_processed = current;
            }
            tokio::time::sleep(poll_interval).await;
        }

        Ok(())
    }

    async fn fetch_block_number(&self, client: &reqwest::Client) -> Option<u64> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_blockNumber",
            "params": [],
            "id": 1
        });
        let resp = client
            .post(self.http_rpc_url.clone())
            .json(&payload)
            .send()
            .await
            .ok()?;
        let result: serde_json::Value = resp.json().await.ok()?;
        let hex = result.get("result").and_then(|r| r.as_str())?;
        u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok()
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
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;
    use tokio_tungstenite::{accept_async, tungstenite::protocol::Message};
    use wiremock::matchers::{body_partial_json, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn record_pending_hash(tracker: &Arc<LatencyTracker>, hash: B256) {
        tracker.record_submit(
            hash,
            0,
            alloy_primitives::Address::default(),
            21_000,
            TransactionType::SimpleTransfer,
        );
    }

    async fn start_heads_ws_server(
        subscription_response: Message,
        follow_up_messages: Vec<Message>,
        hold_open: Duration,
    ) -> (Url, JoinHandle<()>) {
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

            let first_message = socket
                .next()
                .await
                .expect("subscription request should be sent")
                .expect("subscription request should be readable");
            let text = first_message
                .into_text()
                .expect("subscription request should be a text frame");
            let request: Value = serde_json::from_str(&text.to_string())
                .expect("subscription request should be valid json");
            assert_eq!(
                request.get("method").and_then(|m| m.as_str()),
                Some("eth_subscribe")
            );

            socket
                .send(subscription_response)
                .await
                .expect("failed to send subscription response");

            for message in follow_up_messages {
                socket
                    .send(message)
                    .await
                    .expect("failed to send follow-up ws message");
            }

            tokio::time::sleep(hold_open).await;
        });

        let url = Url::parse(&format!("ws://{addr}")).expect("failed to parse ws test url");
        (url, task)
    }

    #[test]
    fn test_block_tracker_creation() {
        let ws_url = Url::parse("ws://localhost:8546").unwrap();
        let http_url = Url::parse("http://localhost:8545").unwrap();
        let tracker = Arc::new(LatencyTracker::new());
        let block_tracker = BlockTracker::new(ws_url, http_url, tracker);
        assert!(block_tracker.ws_url.to_string().contains("localhost"));
    }

    #[test]
    fn test_block_tracker_new_preserves_urls() {
        let ws_url = Url::parse("ws://10.0.0.1:8546").unwrap();
        let http_url = Url::parse("http://10.0.0.1:8545").unwrap();
        let tracker = Arc::new(LatencyTracker::new());
        let bt = BlockTracker::new(ws_url.clone(), http_url.clone(), tracker);

        assert_eq!(bt.ws_url, ws_url);
        assert_eq!(bt.http_rpc_url, http_url);
    }

    #[test]
    fn test_block_tracker_different_url_combinations() {
        let cases = vec![
            ("ws://localhost:8546", "http://localhost:8545"),
            ("ws://127.0.0.1:9546", "http://127.0.0.1:9545"),
            (
                "wss://node.example.com:443/ws",
                "https://node.example.com:443/rpc",
            ),
        ];

        for (ws_str, http_str) in cases {
            let ws_url = Url::parse(ws_str).unwrap();
            let http_url = Url::parse(http_str).unwrap();
            let tracker = Arc::new(LatencyTracker::new());
            let bt = BlockTracker::new(ws_url.clone(), http_url.clone(), tracker);

            assert_eq!(bt.ws_url, ws_url);
            assert_eq!(bt.http_rpc_url, http_url);
        }
    }

    #[test]
    fn test_block_tracker_with_finality_preserves_depth() {
        let ws_url = Url::parse("ws://localhost:8546").unwrap();
        let http_url = Url::parse("http://localhost:8545").unwrap();
        let tracker = Arc::new(LatencyTracker::new());
        let bt = BlockTracker::with_finality(ws_url, http_url, tracker, 3);

        assert_eq!(bt.finality_confirmations, 3);
    }

    #[tokio::test]
    async fn test_fetch_and_track_block_uses_receipts_response() {
        let mock_server = MockServer::start().await;
        let hash = B256::with_last_byte(1);

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "method": "eth_getBlockReceipts",
                "params": ["0x1"]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": [{"transactionHash": format!("{hash:#x}")}]
            })))
            .mount(&mock_server)
            .await;

        let tracker = Arc::new(LatencyTracker::new());
        record_pending_hash(&tracker, hash);

        let http_client = reqwest::Client::new();
        let http_url = Url::parse(&mock_server.uri()).unwrap();
        BlockTracker::fetch_and_track_block(
            &http_client,
            &http_url,
            1,
            &tracker,
            Instant::now(),
            0,
        )
        .await;

        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.confirmed_count(), 1);
    }

    #[tokio::test]
    async fn test_fetch_and_track_block_falls_back_to_block_body() {
        let mock_server = MockServer::start().await;
        let hash = B256::with_last_byte(2);

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "method": "eth_getBlockReceipts",
                "params": ["0x2"]
            })))
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "method": "eth_getBlockByNumber",
                "params": ["0x2", false]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"transactions": [format!("{hash:#x}")]}
            })))
            .mount(&mock_server)
            .await;

        let tracker = Arc::new(LatencyTracker::new());
        record_pending_hash(&tracker, hash);

        let http_client = reqwest::Client::new();
        let http_url = Url::parse(&mock_server.uri()).unwrap();
        BlockTracker::fetch_and_track_block(
            &http_client,
            &http_url,
            2,
            &tracker,
            Instant::now(),
            0,
        )
        .await;

        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.confirmed_count(), 1);
    }

    #[tokio::test]
    async fn test_fetch_and_track_block_ignores_invalid_block_body_entries() {
        let mock_server = MockServer::start().await;
        let hash = B256::with_last_byte(9);

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "method": "eth_getBlockReceipts",
                "params": ["0x9"]
            })))
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "method": "eth_getBlockByNumber",
                "params": ["0x9", false]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"transactions": ["not-a-hash", 7, null]}
            })))
            .mount(&mock_server)
            .await;

        let tracker = Arc::new(LatencyTracker::new());
        record_pending_hash(&tracker, hash);

        BlockTracker::fetch_and_track_block(
            &reqwest::Client::new(),
            &Url::parse(&mock_server.uri()).unwrap(),
            9,
            &tracker,
            Instant::now(),
            0,
        )
        .await;

        assert_eq!(tracker.pending_count(), 1);
        assert_eq!(tracker.confirmed_count(), 0);
    }

    #[tokio::test]
    async fn test_fetch_and_track_block_skips_processing_when_finality_enabled() {
        let tracker = Arc::new(LatencyTracker::new());
        let hash = B256::with_last_byte(3);
        record_pending_hash(&tracker, hash);

        BlockTracker::fetch_and_track_block(
            &reqwest::Client::new(),
            &Url::parse("http://127.0.0.1:9").unwrap(),
            3,
            &tracker,
            Instant::now(),
            2,
        )
        .await;

        assert_eq!(tracker.pending_count(), 1);
        assert_eq!(tracker.confirmed_count(), 0);
    }

    #[tokio::test]
    async fn test_fetch_block_number_returns_none_for_invalid_payload() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "method": "eth_blockNumber"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": "not-hex"
            })))
            .mount(&mock_server)
            .await;

        let tracker = Arc::new(LatencyTracker::new());
        let bt = BlockTracker::new(
            Url::parse("ws://localhost:8546").unwrap(),
            Url::parse(&mock_server.uri()).unwrap(),
            tracker,
        );

        let block_number = bt.fetch_block_number(&reqwest::Client::new()).await;
        assert!(block_number.is_none());
    }

    #[tokio::test]
    async fn test_run_ws_raw_signals_ready_and_tracks_receipts() {
        let mock_server = MockServer::start().await;
        let hash = B256::with_last_byte(4);

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "method": "eth_getBlockReceipts",
                "params": ["0x1"]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": [{"transactionHash": format!("{hash:#x}")}]
            })))
            .mount(&mock_server)
            .await;

        let new_head = json!({
            "jsonrpc": "2.0",
            "method": "eth_subscription",
            "params": {
                "subscription": "0xsub",
                "result": {"number": "0x1"}
            }
        });
        let (ws_url, server_task) = start_heads_ws_server(
            Message::Text(json!({"jsonrpc": "2.0", "id": 1, "result": "0xsub"}).to_string()),
            vec![
                Message::Ping(vec![1, 2, 3]),
                Message::Text(new_head.to_string()),
            ],
            Duration::from_millis(250),
        )
        .await;

        let tracker = Arc::new(LatencyTracker::new());
        record_pending_hash(&tracker, hash);
        let bt = BlockTracker::new(
            ws_url,
            Url::parse(&mock_server.uri()).unwrap(),
            tracker.clone(),
        );
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

        bt.run_ws_raw(Duration::from_millis(125), Some(ready_tx))
            .await
            .expect("run_ws_raw should succeed");

        ready_rx.await.expect("ready signal should be delivered");
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.confirmed_count(), 1);

        server_task
            .await
            .expect("ws server task should finish cleanly");
    }

    #[tokio::test]
    async fn test_run_ws_raw_rejects_subscription_without_result() {
        let (ws_url, server_task) = start_heads_ws_server(
            Message::Text(
                json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32000, "message": "denied"}})
                    .to_string(),
            ),
            Vec::new(),
            Duration::from_millis(50),
        )
        .await;

        let tracker = Arc::new(LatencyTracker::new());
        let bt = BlockTracker::new(
            ws_url,
            Url::parse("http://127.0.0.1:8545").unwrap(),
            tracker,
        );

        let err = bt
            .run_ws_raw(Duration::from_millis(50), None)
            .await
            .expect_err("subscription without result should fail");
        assert!(err.to_string().contains("WS subscription failed"));

        server_task
            .await
            .expect("ws server task should finish cleanly");
    }

    #[tokio::test]
    async fn test_run_with_ready_delegates_and_signals_ready() {
        let (ws_url, server_task) = start_heads_ws_server(
            Message::Text(json!({"jsonrpc": "2.0", "id": 1, "result": "0xsub"}).to_string()),
            Vec::new(),
            Duration::from_millis(125),
        )
        .await;

        let tracker = Arc::new(LatencyTracker::new());
        let bt = BlockTracker::new(
            ws_url,
            Url::parse("http://127.0.0.1:8545").unwrap(),
            tracker,
        );
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

        bt.run_with_ready(Duration::from_millis(50), ready_tx)
            .await
            .expect("run_with_ready should succeed");
        ready_rx.await.expect("run_with_ready should signal ready");

        server_task
            .await
            .expect("ws server task should finish cleanly");
    }

    #[tokio::test]
    async fn test_run_ws_raw_accepts_non_text_subscription_confirmation() {
        let (ws_url, server_task) = start_heads_ws_server(
            Message::Ping(vec![7]),
            Vec::new(),
            Duration::from_millis(125),
        )
        .await;

        let tracker = Arc::new(LatencyTracker::new());
        let bt = BlockTracker::new(
            ws_url,
            Url::parse("http://127.0.0.1:8545").unwrap(),
            tracker,
        );

        let result = bt.run_ws_raw(Duration::from_millis(50), None).await;
        match result {
            Ok(()) => {}
            Err(err) => assert!(
                err.to_string().contains("WS stream error"),
                "non-text subscription confirmation should be ignored before shutdown: {err:?}"
            ),
        }

        server_task
            .await
            .expect("ws server task should finish cleanly");
    }

    #[tokio::test]
    async fn test_run_ws_raw_errors_when_stream_ends_after_close_frame() {
        let (ws_url, server_task) = start_heads_ws_server(
            Message::Text(json!({"jsonrpc": "2.0", "id": 1, "result": "0xsub"}).to_string()),
            vec![Message::Close(None)],
            Duration::from_millis(25),
        )
        .await;

        let tracker = Arc::new(LatencyTracker::new());
        let bt = BlockTracker::new(
            ws_url,
            Url::parse("http://127.0.0.1:8545").unwrap(),
            tracker,
        );

        let err = bt
            .run_ws_raw(Duration::from_millis(200), None)
            .await
            .expect_err("connection end after close frame should be reported");
        assert!(err.to_string().contains("WS stream ended unexpectedly"));

        server_task
            .await
            .expect("ws server task should finish cleanly");
    }

    #[tokio::test]
    async fn test_run_ws_raw_reports_stream_errors() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind raw ws test server");
        let addr = listener
            .local_addr()
            .expect("failed to read raw ws test server address");

        let server_task = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            let (mut stream, _) = listener.accept().await.expect("failed to accept ws client");

            let mut request_bytes = Vec::new();
            let mut buf = [0u8; 1024];
            while !request_bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream
                    .read(&mut buf)
                    .await
                    .expect("handshake should be readable");
                assert!(read > 0, "client closed before handshake completed");
                request_bytes.extend_from_slice(&buf[..read]);
            }

            let request =
                String::from_utf8(request_bytes).expect("handshake request should be utf8");
            let websocket_key = request
                .lines()
                .find_map(|line| line.strip_prefix("Sec-WebSocket-Key: "))
                .expect("client handshake must include websocket key")
                .trim()
                .to_string();
            let accept_key = {
                use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
                derive_accept_key(websocket_key.as_bytes())
            };
            let response = format!(
                "HTTP/1.1 101 Switching Protocols\r\n\
Upgrade: websocket\r\n\
Connection: Upgrade\r\n\
Sec-WebSocket-Accept: {accept_key}\r\n\r\n"
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("handshake response should be writable");

            let valid_subscription = r#"{"jsonrpc":"2.0","id":1,"result":"0xsub"}"#;
            let valid_frame = {
                let mut frame = vec![0x81, valid_subscription.len() as u8];
                frame.extend_from_slice(valid_subscription.as_bytes());
                frame
            };
            stream
                .write_all(&valid_frame)
                .await
                .expect("subscription frame should be writable");

            stream
                .write_all(&[0x81, 0x80])
                .await
                .expect("invalid text frame should be writable");
        });

        let tracker = Arc::new(LatencyTracker::new());
        let bt = BlockTracker::new(
            Url::parse(&format!("ws://{addr}")).unwrap(),
            Url::parse("http://127.0.0.1:8545").unwrap(),
            tracker,
        );

        let err = bt
            .run_ws_raw(Duration::from_millis(200), None)
            .await
            .expect_err("invalid websocket frame should surface as a stream error");
        assert!(err.to_string().contains("WS stream error"));

        server_task
            .await
            .expect("raw ws server task should finish cleanly");
    }

    #[tokio::test]
    async fn test_run_ws_raw_ignores_invalid_new_head_messages() {
        let (ws_url, server_task) = start_heads_ws_server(
            Message::Text(json!({"jsonrpc": "2.0", "id": 1, "result": "0xsub"}).to_string()),
            vec![Message::Text(
                json!({
                    "jsonrpc": "2.0",
                    "method": "eth_subscription",
                    "params": {"subscription": "0xsub", "result": {"number": "not-hex"}}
                })
                .to_string(),
            )],
            Duration::from_millis(125),
        )
        .await;

        let tracker = Arc::new(LatencyTracker::new());
        let bt = BlockTracker::new(
            ws_url,
            Url::parse("http://127.0.0.1:8545").unwrap(),
            tracker,
        );

        bt.run_ws_raw(Duration::from_millis(50), None)
            .await
            .expect("invalid newHeads payload should be ignored");

        server_task
            .await
            .expect("ws server task should finish cleanly");
    }

    #[tokio::test]
    async fn test_run_ws_raw_times_out_cleanly_at_deadline() {
        let (ws_url, server_task) = start_heads_ws_server(
            Message::Text(json!({"jsonrpc": "2.0", "id": 1, "result": "0xsub"}).to_string()),
            Vec::new(),
            Duration::from_millis(150),
        )
        .await;

        let tracker = Arc::new(LatencyTracker::new());
        let bt = BlockTracker::new(
            ws_url,
            Url::parse("http://127.0.0.1:8545").unwrap(),
            tracker,
        );

        bt.run_ws_raw(Duration::from_millis(40), None)
            .await
            .expect("timeout at deadline should return ok");

        server_task
            .await
            .expect("ws server task should finish cleanly");
    }

    #[tokio::test]
    async fn test_fetch_and_track_block_ignores_missing_block_transactions() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "method": "eth_getBlockReceipts",
                "params": ["0xa"]
            })))
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "method": "eth_getBlockByNumber",
                "params": ["0xa", false]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {}
            })))
            .mount(&mock_server)
            .await;

        let tracker = Arc::new(LatencyTracker::new());
        BlockTracker::fetch_and_track_block(
            &reqwest::Client::new(),
            &Url::parse(&mock_server.uri()).unwrap(),
            10,
            &tracker,
            Instant::now(),
            0,
        )
        .await;

        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.confirmed_count(), 0);
    }

    #[tokio::test]
    async fn test_run_http_only_processes_new_blocks() {
        let mock_server = MockServer::start().await;
        let hash = B256::with_last_byte(6);

        Mock::given(method("POST"))
            .and(body_partial_json(json!({ "method": "eth_blockNumber" })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"jsonrpc": "2.0", "id": 1, "result": "0x1"})),
            )
            .expect(1..)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "method": "eth_getBlockReceipts",
                "params": ["0x1"]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": [{"transactionHash": format!("{hash:#x}")}]
            })))
            .mount(&mock_server)
            .await;

        let tracker = Arc::new(LatencyTracker::new());
        record_pending_hash(&tracker, hash);
        let bt = BlockTracker::new(
            Url::parse("ws://localhost:8546").unwrap(),
            Url::parse(&mock_server.uri()).unwrap(),
            tracker.clone(),
        );

        bt.run_http_only(Duration::from_millis(40))
            .await
            .expect("run_http_only should succeed");

        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.confirmed_count(), 1);
    }

    #[tokio::test]
    async fn test_run_http_polling_with_interval_signals_ready() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({ "method": "eth_blockNumber" })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"jsonrpc": "2.0", "id": 1, "result": "0x0"})),
            )
            .mount(&mock_server)
            .await;

        let tracker = Arc::new(LatencyTracker::new());
        let bt = BlockTracker::new(
            Url::parse("ws://localhost:8546").unwrap(),
            Url::parse(&mock_server.uri()).unwrap(),
            tracker,
        );
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

        bt.run_http_polling_with_interval(
            Duration::from_millis(20),
            Some(ready_tx),
            Duration::from_millis(5),
        )
        .await
        .expect("http polling should succeed");
        ready_rx
            .await
            .expect("http polling should signal readiness immediately");
    }

    #[tokio::test]
    async fn test_run_falls_back_to_http_polling_after_ws_failure() {
        let mock_server = MockServer::start().await;
        let hash = B256::with_last_byte(5);

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "method": "eth_blockNumber"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": "0x1"
            })))
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "method": "eth_getBlockReceipts",
                "params": ["0x1"]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": [{"transactionHash": format!("{hash:#x}")}]
            })))
            .mount(&mock_server)
            .await;

        let unused_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to reserve unused tcp port");
        let unused_addr = unused_listener
            .local_addr()
            .expect("failed to read unused tcp addr");
        drop(unused_listener);

        let tracker = Arc::new(LatencyTracker::new());
        record_pending_hash(&tracker, hash);
        let bt = BlockTracker::new(
            Url::parse(&format!("ws://{unused_addr}")).unwrap(),
            Url::parse(&mock_server.uri()).unwrap(),
            tracker.clone(),
        );

        bt.run(Duration::from_millis(150))
            .await
            .expect("http fallback should succeed");

        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.confirmed_count(), 1);
    }
}
#[cfg(test)]
mod hash_format_tests {
    #[test]
    fn test_b256_format() {
        use alloy_primitives::B256;
        let _h: B256 = "0xe43f1d06a00ebadfe0d1c88e1c963e3d4c5f4f682cf3e2843f5d62c33bf31136"
            .parse()
            .unwrap();
    }
}
