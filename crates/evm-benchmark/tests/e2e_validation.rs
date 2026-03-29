//! End-to-end validation tests for evm-benchmark features.
//!
//! These tests require the e2e Docker environment to be running (`make e2e-up-release`).
//! They are marked `#[ignore]` so they don't run in CI without explicit opt-in.
//!
//! Each test uses a different validator key to avoid nonce conflicts when tests run
//! in parallel. Keys 1-4 are pre-funded validators in genesis.
//!
//! Run with: `cargo test -p evm-benchmark --test e2e_validation -- --ignored`

use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

const RPC_URL: &str = "http://localhost:8545";
const WS_URL: &str = "ws://localhost:8546";

// Pre-funded validator keys (1-4). Each test uses a different key to avoid nonce races.
const KEY_1: &str = "0x0000000000000000000000000000000000000000000000000000000000000001";
const KEY_2: &str = "0x0000000000000000000000000000000000000000000000000000000000000002";
const KEY_3: &str = "0x0000000000000000000000000000000000000000000000000000000000000003";
const KEY_4: &str = "0x0000000000000000000000000000000000000000000000000000000000000004";

/// Helper: check if the e2e environment is reachable.
async fn e2e_is_up() -> bool {
    let client = reqwest::Client::new();
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_blockNumber",
        "params": [],
        "id": 1
    });
    matches!(client.post(RPC_URL).json(&payload).send().await, Ok(r) if r.status().is_success())
}

/// Helper: sign a simple transfer transaction with a specific key.
fn sign_test_tx(key: &str, nonce: u64) -> evm_benchmark::types::SignedTxWithMetadata {
    use alloy_consensus::{SignableTransaction, TxEip1559};
    use alloy_eips::eip2718::Encodable2718;
    use alloy_network::TxSignerSync;
    use alloy_primitives::{Address, TxKind, U256};
    use alloy_signer_local::PrivateKeySigner;

    let signer = PrivateKeySigner::from_str(key).unwrap();
    let account = signer.address();

    let mut tx = TxEip1559 {
        chain_id: 19803,
        nonce,
        gas_limit: 21_000,
        max_fee_per_gas: 2_000_000_000,
        max_priority_fee_per_gas: 100_000_000,
        to: TxKind::Call(Address::with_last_byte(0x42)),
        value: U256::from(1u32),
        input: alloy_primitives::Bytes::new(),
        access_list: Default::default(),
    };

    let sig = signer.sign_transaction_sync(&mut tx).unwrap();
    let signed = tx.into_signed(sig);
    let envelope = alloy_consensus::TxEnvelope::from(signed);
    let encoded = envelope.encoded_2718().to_vec();
    let hash = *envelope.hash();

    evm_benchmark::types::SignedTxWithMetadata {
        hash,
        encoded,
        nonce,
        gas_limit: 21_000,
        sender: account,
        submit_time: Instant::now(),
        method: evm_benchmark::types::TransactionType::SimpleTransfer,
    }
}

/// Helper: get the pending nonce for a given key.
async fn get_nonce(key: &str) -> u64 {
    use alloy_signer_local::PrivateKeySigner;
    let signer = PrivateKeySigner::from_str(key).unwrap();
    let account = format!("{:?}", signer.address());

    let client = reqwest::Client::new();
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_getTransactionCount",
        "params": [account, "pending"],
        "id": 1
    });
    let resp = client.post(RPC_URL).json(&payload).send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let hex = body["result"].as_str().unwrap();
    u64::from_str_radix(hex.trim_start_matches("0x"), 16).unwrap()
}

// ─── HTTP Submission Tests (KEY_1) ───────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn test_http_submitter_sends_transactions() {
    if !e2e_is_up().await {
        eprintln!("Skipping: e2e environment not available");
        return;
    }

    let rpc_url = url::Url::parse(RPC_URL).unwrap();
    let ws_url = url::Url::parse(WS_URL).unwrap();
    let submitter = evm_benchmark::submission::Submitter::new(
        vec![rpc_url],
        &ws_url,
        100,
        evm_benchmark::config::SubmissionMethod::Http,
    )
    .unwrap();

    let nonce = get_nonce(KEY_1).await;
    let txs: Vec<_> = (0..5).map(|i| sign_test_tx(KEY_1, nonce + i)).collect();

    let result = submitter.submit_batch(txs).await.unwrap();
    assert!(
        result.submitted > 0,
        "Expected at least 1 tx submitted, got {}",
        result.submitted
    );
    assert!(!result.hashes.is_empty(), "Expected tx hashes returned");
}

// ─── HTTP Multi-Endpoint Tests (KEY_2) ───────────────────────────────────────

#[tokio::test]
#[ignore]
async fn test_http_multi_endpoint_round_robin() {
    if !e2e_is_up().await {
        eprintln!("Skipping: e2e environment not available");
        return;
    }

    let urls: Vec<url::Url> = vec![
        url::Url::parse("http://localhost:8545").unwrap(),
        url::Url::parse("http://localhost:8555").unwrap(),
        url::Url::parse("http://localhost:8565").unwrap(),
    ];
    let ws_url = url::Url::parse(WS_URL).unwrap();
    let submitter = evm_benchmark::submission::Submitter::new(
        urls,
        &ws_url,
        100,
        evm_benchmark::config::SubmissionMethod::Http,
    )
    .unwrap();

    submitter.warm_up(3).await.unwrap();

    let nonce = get_nonce(KEY_2).await;
    let txs: Vec<_> = (0..3).map(|i| sign_test_tx(KEY_2, nonce + i)).collect();
    let result = submitter.submit_batch(txs).await.unwrap();
    assert!(result.submitted > 0, "Round-robin submission failed");
}

// ─── WebSocket Submission Tests (KEY_3) ──────────────────────────────────────

#[tokio::test]
#[ignore]
async fn test_ws_submitter_sends_transactions() {
    if !e2e_is_up().await {
        eprintln!("Skipping: e2e environment not available");
        return;
    }

    let rpc_url = url::Url::parse(RPC_URL).unwrap();
    let ws_url = url::Url::parse(WS_URL).unwrap();
    let submitter = evm_benchmark::submission::Submitter::new(
        vec![rpc_url],
        &ws_url,
        100,
        evm_benchmark::config::SubmissionMethod::WebSocket,
    )
    .unwrap();

    let nonce = get_nonce(KEY_3).await;
    let txs: Vec<_> = (0..5).map(|i| sign_test_tx(KEY_3, nonce + i)).collect();

    let result = submitter.submit_batch(txs).await.unwrap();
    assert!(
        result.submitted > 0,
        "WS submission should succeed, got 0 submitted"
    );
}

#[tokio::test]
#[ignore]
async fn test_ws_submitter_warm_up() {
    if !e2e_is_up().await {
        eprintln!("Skipping: e2e environment not available");
        return;
    }

    let rpc_url = url::Url::parse(RPC_URL).unwrap();
    let ws_url = url::Url::parse(WS_URL).unwrap();
    let submitter = evm_benchmark::submission::Submitter::new(
        vec![rpc_url],
        &ws_url,
        100,
        evm_benchmark::config::SubmissionMethod::WebSocket,
    )
    .unwrap();

    submitter.warm_up(3).await.unwrap();
}

// ─── BlockTracker Tests ──────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn test_block_tracker_receives_blocks() {
    if !e2e_is_up().await {
        eprintln!("Skipping: e2e environment not available");
        return;
    }

    let ws_url = url::Url::parse(WS_URL).unwrap();
    let rpc_url = url::Url::parse(RPC_URL).unwrap();
    let tracker = Arc::new(evm_benchmark::submission::LatencyTracker::new());

    let block_tracker =
        evm_benchmark::submission::BlockTracker::new(ws_url, rpc_url, tracker.clone());

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        block_tracker.run(Duration::from_secs(3)),
    )
    .await;

    assert!(
        result.is_ok(),
        "BlockTracker should complete within timeout"
    );
}

// ─── Receipt Polling Tests (KEY_4) ───────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn test_receipt_polling_finds_confirmed_txs() {
    if !e2e_is_up().await {
        eprintln!("Skipping: e2e environment not available");
        return;
    }

    let rpc_url = url::Url::parse(RPC_URL).unwrap();
    let ws_url = url::Url::parse(WS_URL).unwrap();
    let tracker = Arc::new(evm_benchmark::submission::LatencyTracker::new());

    let submitter = evm_benchmark::submission::Submitter::new(
        vec![rpc_url],
        &ws_url,
        100,
        evm_benchmark::config::SubmissionMethod::Http,
    )
    .unwrap();

    let nonce = get_nonce(KEY_4).await;
    let txs: Vec<_> = (0..5).map(|i| sign_test_tx(KEY_4, nonce + i)).collect();

    let result = submitter.submit_batch(txs).await.unwrap();
    assert!(result.submitted > 0, "Should submit txs");

    for tx in &result.accepted_txs {
        tracker.record_submit(tx.hash, tx.nonce, tx.sender, tx.gas_limit, tx.method);
    }

    // Wait for chain to include them
    tokio::time::sleep(Duration::from_secs(2)).await;

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(16)
        .build()
        .unwrap();

    assert!(tracker.pending_count() > 0, "Should have pending txs");

    for _ in 0..10 {
        if tracker.pending_count() == 0 {
            break;
        }
        let hashes = tracker.pending_hashes();
        for hash in hashes {
            let payload = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_getTransactionReceipt",
                "params": [format!("{:?}", hash)],
                "id": 1
            });
            if let Ok(resp) = client.post(RPC_URL).json(&payload).send().await
                && let Ok(body) = resp.json::<serde_json::Value>().await
                && body.get("result").and_then(|r| r.as_object()).is_some()
            {
                tracker.on_block_inclusion(hash, Instant::now());
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let confirmed = tracker.confirmed_count();
    assert!(
        confirmed > 0,
        "Receipt polling should find confirmed txs, got 0 out of {} submitted",
        result.submitted
    );
}

// ─── Full Pipeline Tests (uses built-in multi-key, no nonce conflict) ────────

#[tokio::test]
#[ignore]
async fn test_burst_mode_end_to_end() {
    if !e2e_is_up().await {
        eprintln!("Skipping: e2e environment not available");
        return;
    }

    let config = evm_benchmark::Config {
        rpc_urls: vec![url::Url::parse(RPC_URL).unwrap()],
        rpc: url::Url::parse(RPC_URL).unwrap(),
        ws: url::Url::parse(WS_URL).unwrap(),
        metrics: None,
        validator_urls: vec![],
        test_mode: evm_benchmark::types::TestMode::Transfer,
        execution_mode: evm_benchmark::types::ExecutionMode::Burst,
        tx_count: 100,
        sender_count: 1,
        wave_count: 1,
        wave_delay_ms: 0,
        duration_secs: 10,
        target_tps: 100,
        worker_count: 1,
        batch_size: 100,
        submission_method: evm_benchmark::config::SubmissionMethod::Http,
        retry_profile: "light".to_string(),
        finality_confirmations: 0,
        output: std::path::PathBuf::from("/tmp/bench-test-burst.json"),
        quiet: true,
        chain_id: 19803,
        bench_name: "evm_bench_v1".to_string(),
        fund: false,
        sender_keys: vec![],
        evm_tokens: vec![],
        evm_pairs: vec![],
        evm_nfts: vec![],
    };

    let result = evm_benchmark::run_burst(&config).await;
    assert!(result.is_ok(), "Burst mode failed: {:?}", result.err());

    let (burst, _gas_price) = result.unwrap();
    assert!(burst.submitted > 0, "Should submit transactions");
    assert!(burst.confirmed > 0, "Should confirm transactions");
    assert!(burst.confirmed_tps > 0.0, "Should measure TPS > 0");

    eprintln!(
        "Burst e2e: {} submitted, {} confirmed, {:.0} TPS, p50={}ms p99={}ms",
        burst.submitted, burst.confirmed, burst.confirmed_tps, burst.latency.p50, burst.latency.p99
    );
}

#[tokio::test]
#[ignore]
async fn test_burst_mode_ws_submission() {
    if !e2e_is_up().await {
        eprintln!("Skipping: e2e environment not available");
        return;
    }

    fn set_test_env_var<K: AsRef<std::ffi::OsStr>, V: AsRef<std::ffi::OsStr>>(key: K, value: V) {
        // SAFETY: This ignored integration test serializes env mutation for the duration of the
        // benchmark run before restoring the previous process environment.
        unsafe { std::env::set_var(key, value) }
    }

    fn remove_test_env_var<K: AsRef<std::ffi::OsStr>>(key: K) {
        // SAFETY: This ignored integration test serializes env mutation for the duration of the
        // benchmark run before restoring the previous process environment.
        unsafe { std::env::remove_var(key) }
    }

    static ENV_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

    let _guard = ENV_LOCK.lock().await;

    // Use a single specific key to avoid conflict with the default multi-key burst above
    set_test_env_var("BENCH_KEY", KEY_3);

    let config = evm_benchmark::Config {
        rpc_urls: vec![url::Url::parse(RPC_URL).unwrap()],
        rpc: url::Url::parse(RPC_URL).unwrap(),
        ws: url::Url::parse(WS_URL).unwrap(),
        metrics: None,
        validator_urls: vec![],
        test_mode: evm_benchmark::types::TestMode::Transfer,
        execution_mode: evm_benchmark::types::ExecutionMode::Burst,
        tx_count: 50,
        sender_count: 1,
        wave_count: 1,
        wave_delay_ms: 0,
        duration_secs: 10,
        target_tps: 100,
        worker_count: 1,
        batch_size: 50,
        submission_method: evm_benchmark::config::SubmissionMethod::WebSocket,
        retry_profile: "light".to_string(),
        finality_confirmations: 0,
        output: std::path::PathBuf::from("/tmp/bench-test-burst-ws.json"),
        quiet: true,
        chain_id: 19803,
        bench_name: "evm_bench_v1".to_string(),
        fund: false,
        sender_keys: vec![],
        evm_tokens: vec![],
        evm_pairs: vec![],
        evm_nfts: vec![],
    };

    let result = evm_benchmark::run_burst(&config).await;

    // Restore env
    remove_test_env_var("BENCH_KEY");

    assert!(
        result.is_ok(),
        "Burst mode with WS submission failed: {:?}",
        result.err()
    );

    let (burst, _gas_price) = result.unwrap();
    assert!(burst.submitted > 0, "WS burst should submit transactions");
    assert!(burst.confirmed > 0, "WS burst should confirm transactions");

    eprintln!(
        "WS Burst e2e: {} submitted, {} confirmed, {:.0} TPS",
        burst.submitted, burst.confirmed, burst.confirmed_tps
    );
}
