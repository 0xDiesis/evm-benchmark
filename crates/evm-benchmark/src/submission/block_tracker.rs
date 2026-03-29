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
}

#[cfg(test)]
mod ws_integration_tests {
    use super::*;

    #[tokio::test]
    #[ignore] // only run when e2e is up
    async fn test_raw_ws_receives_blocks() {
        let ws_url = Url::parse("ws://localhost:8546").unwrap();
        let http_url = Url::parse("http://localhost:8545").unwrap();
        let tracker = Arc::new(LatencyTracker::new());
        let bt = BlockTracker::new(ws_url, http_url, tracker.clone());
        let result = bt.run(Duration::from_secs(5)).await;
        assert!(result.is_ok(), "BlockTracker failed: {:?}", result);
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
