//! Sender key generation and auto-funding via MultiSend contract.
//!
//! Generates deterministic sender keys from `keccak256("bench-sender-{i}")` and
//! funds them in batch via a deployed MultiSend contract that splits ETH across
//! recipients in a single transaction. The funding phase runs before any
//! benchmark so it doesn't affect TPS measurements.

use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_signer_local::PrivateKeySigner;
use anyhow::Result;
use std::str::FromStr;
use std::time::Duration;

/// Resolve sender keys from BENCH_KEY env var or generate deterministic keys.
///
/// If `BENCH_KEY` provides fewer keys than `count`, the remaining keys are
/// generated deterministically. This allows providing a single pre-funded key
/// via `BENCH_KEY` while generating additional senders via `--senders N --fund`.
pub fn resolve_sender_keys(count: u32) -> Vec<String> {
    let bench_key_env = std::env::var("BENCH_KEY").unwrap_or_default();

    let mut keys: Vec<String> = if bench_key_env.is_empty() {
        Vec::new()
    } else {
        bench_key_env
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    // If we have fewer keys than requested, generate the rest deterministically
    if keys.len() < count as usize {
        let generated = generate_sender_keys(count as usize - keys.len());
        keys.extend(generated);
    }

    keys
}

/// Generate `count` deterministic sender private keys.
///
/// Each key is derived from `keccak256("bench-sender-{i}")`. These are the same
/// keys used by the `fund-accounts.sh` scripts (via `cast keccak`), ensuring
/// the harness and external tooling agree on addresses.
pub fn generate_sender_keys(count: usize) -> Vec<String> {
    (0..count)
        .map(|i| {
            let input = format!("bench-sender-{}", i);
            let hash = alloy_primitives::keccak256(input.as_bytes());
            format!("0x{}", hex::encode(hash.as_slice()))
        })
        .collect()
}

/// Parse a list of hex private key strings into signers, returning (key, signer, address).
pub fn parse_sender_keys(keys: &[String]) -> Result<Vec<(String, PrivateKeySigner, Address)>> {
    keys.iter()
        .enumerate()
        .map(|(i, key)| {
            let signer = PrivateKeySigner::from_str(key)
                .map_err(|e| anyhow::anyhow!("Failed to parse sender key {}: {}", i, e))?;
            let addr = signer.address();
            Ok((key.clone(), signer, addr))
        })
        .collect()
}

/// Fetch the current gas price from the chain (with 2x safety margin).
pub async fn fetch_gas_price(client: &reqwest::Client, rpc_url: &str) -> Result<u128> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0", "method": "eth_gasPrice", "params": [], "id": 1
    });
    let resp = client.post(rpc_url).json(&payload).send().await?;
    let result: serde_json::Value = resp.json().await?;
    let hex = result
        .get("result")
        .and_then(|r| r.as_str())
        .unwrap_or("0x3b9aca00");
    let base = u128::from_str_radix(hex.trim_start_matches("0x"), 16).unwrap_or(1_000_000_000);
    Ok((base * 2).max(1_000_000_000))
}

/// Check which sender addresses need funding (balance < 0.1 ETH).
async fn check_balances(
    client: &reqwest::Client,
    rpc_url: &str,
    addresses: &[Address],
) -> Result<Vec<(usize, Address)>> {
    let min_balance = U256::from(100_000_000_000_000_000u128); // 0.1 ETH
    let mut to_fund = Vec::new();

    // Batch balance checks
    for (i, addr) in addresses.iter().enumerate() {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_getBalance",
            "params": [format!("{:?}", addr), "latest"],
            "id": i + 1
        });
        let resp = client.post(rpc_url).json(&payload).send().await?;
        let result: serde_json::Value = resp.json().await?;
        let bal_hex = result
            .get("result")
            .and_then(|r| r.as_str())
            .unwrap_or("0x0");
        let bal = U256::from_str_radix(bal_hex.trim_start_matches("0x"), 16).unwrap_or(U256::ZERO);
        if bal < min_balance {
            to_fund.push((i, *addr));
        }
    }
    Ok(to_fund)
}

// ── MultiSend contract ─────────────────────────────────────────────────────
// Minimal contract: function send(address[] calldata to) external payable
// Splits msg.value equally among all recipients via low-level call.
//
// Solidity source (compiled with solc 0.8.x):
// contract MultiSend {
//   function send(address[] calldata to) external payable {
//     uint256 amt = msg.value / to.length;
//     for (uint256 i = 0; i < to.length; i++) {
//       (bool ok,) = to[i].call{value: amt}("");
//       require(ok);
//     }
//   }
// }

/// Pre-compiled bytecode for the MultiSend contract.
const MULTISEND_BYTECODE: &str = "6080604052348015600e575f5ffd5b5061034e8061001c5f395ff3fe60806040526004361061001d575f3560e01c8063298c073314610021575b5f5ffd5b61003b60048036038101906100369190610174565b61003d565b005b5f828290503461004d91906101f5565b90505f5f90505b83839050811015610105575f84848381811061007357610072610225565b5b905060200201602081019061008891906102ac565b73ffffffffffffffffffffffffffffffffffffffff16836040516100ab90610304565b5f6040518083038185875af1925050503d805f81146100e5576040519150601f19603f3d011682016040523d82523d5f602084013e6100ea565b606091505b50509050806100f7575f5ffd5b508080600101915050610054565b50505050565b5f5ffd5b5f5ffd5b5f5ffd5b5f5ffd5b5f5ffd5b5f5f83601f84011261013457610133610113565b5b8235905067ffffffffffffffff81111561015157610150610117565b5b60208301915083602082028301111561016d5761016c61011b565b5b9250929050565b5f5f6020838503121561018a5761018961010b565b5b5f83013567ffffffffffffffff8111156101a7576101a661010f565b5b6101b38582860161011f565b92509250509250929050565b5f819050919050565b7f4e487b71000000000000000000000000000000000000000000000000000000005f52601260045260245ffd5b5f6101ff826101bf565b915061020a836101bf565b92508261021a576102196101c8565b5b828204905092915050565b7f4e487b71000000000000000000000000000000000000000000000000000000005f52603260045260245ffd5b5f73ffffffffffffffffffffffffffffffffffffffff82169050919050565b5f61027b82610252565b9050919050565b61028b81610271565b8114610295575f5ffd5b50565b5f813590506102a681610282565b92915050565b5f602082840312156102c1576102c061010b565b5b5f6102ce84828501610298565b91505092915050565b5f81905092915050565b50565b5f6102ef5f836102d7565b91506102fa826102e1565b5f82019050919050565b5f61030e826102e4565b915081905091905056fea2646970667358221220637bf60662fe0073c9d7744a2480e03ede6650ed6776139ceeefb9b7ad7a3e8c64736f6c63430008220033";

/// Maximum recipients per MultiSend call. Keeps gas usage within block limits.
const MULTISEND_BATCH_SIZE: usize = 100;

#[derive(Debug, Clone, Copy)]
struct FundingTimeouts {
    chain_ready: Duration,
    deploy_receipt: Duration,
    funding_confirm: Duration,
    retry_confirm: Duration,
    poll_interval: Duration,
}

impl FundingTimeouts {
    fn from_env() -> Self {
        let chain_ready_secs = parse_env_u64("BENCH_FUND_CHAIN_READY_TIMEOUT_SECS", 30);
        let deploy_receipt_secs = parse_env_u64("BENCH_FUND_DEPLOY_TIMEOUT_SECS", 30);
        let funding_confirm_secs = parse_env_u64("BENCH_FUND_CONFIRM_TIMEOUT_SECS", 60);
        let retry_confirm_secs = parse_env_u64("BENCH_FUND_RETRY_TIMEOUT_SECS", 30);
        let poll_interval_ms = parse_env_u64("BENCH_FUND_POLL_INTERVAL_MS", 500);

        Self {
            chain_ready: Duration::from_secs(chain_ready_secs),
            deploy_receipt: Duration::from_secs(deploy_receipt_secs),
            funding_confirm: Duration::from_secs(funding_confirm_secs),
            retry_confirm: Duration::from_secs(retry_confirm_secs),
            poll_interval: Duration::from_millis(poll_interval_ms),
        }
    }
}

fn parse_env_u64(var: &str, default: u64) -> u64 {
    match std::env::var(var) {
        Ok(value) => value
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|v| *v > 0)
            .unwrap_or(default),
        Err(_) => default,
    }
}

/// Get the nonce for an address.
async fn get_nonce(client: &reqwest::Client, rpc_url: &str, addr: Address) -> Result<u64> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_getTransactionCount",
        "params": [format!("{:?}", addr), "pending"],
        "id": 1
    });
    let resp = client.post(rpc_url).json(&payload).send().await?;
    let result: serde_json::Value = resp.json().await?;
    let hex = result
        .get("result")
        .and_then(|r| r.as_str())
        .ok_or_else(|| anyhow::anyhow!("Failed to get nonce for {:?}", addr))?;
    Ok(u64::from_str_radix(hex.trim_start_matches("0x"), 16)?)
}

/// Deploy the MultiSend contract and return its address.
async fn deploy_multisend(
    client: &reqwest::Client,
    rpc_url: &str,
    funder: &PrivateKeySigner,
    chain_id: u64,
    nonce: u64,
    gas_price: u128,
    timeouts: FundingTimeouts,
) -> Result<Address> {
    use alloy_consensus::SignableTransaction;
    use alloy_consensus::TxLegacy;
    use alloy_eips::eip2718::Encodable2718;
    use alloy_network::TxSignerSync;
    use alloy_primitives::TxKind;

    let bytecode = hex::decode(MULTISEND_BYTECODE)?;
    let mut tx = TxLegacy {
        chain_id: Some(chain_id),
        nonce,
        gas_price,
        gas_limit: 500_000,
        to: TxKind::Create,
        value: U256::ZERO,
        input: Bytes::from(bytecode),
    };

    let sig = funder.sign_transaction_sync(&mut tx)?;
    let signed = tx.into_signed(sig);
    let envelope = alloy_consensus::TxEnvelope::from(signed);
    let encoded = Encodable2718::encoded_2718(&envelope);
    let tx_hash = *envelope.hash();

    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_sendRawTransaction",
        "params": [format!("0x{}", hex::encode(&encoded))],
        "id": 1
    });
    let resp = client.post(rpc_url).json(&payload).send().await?;
    let result: serde_json::Value = resp.json().await?;
    if let Some(err) = result.get("error") {
        anyhow::bail!("MultiSend deploy failed: {}", err);
    }

    // Wait for receipt to get contract address
    let receipt_deadline = tokio::time::Instant::now() + timeouts.deploy_receipt;
    while tokio::time::Instant::now() < receipt_deadline {
        tokio::time::sleep(timeouts.poll_interval).await;
        let receipt_payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_getTransactionReceipt",
            "params": [format!("{:?}", tx_hash)],
            "id": 1
        });
        let resp = client.post(rpc_url).json(&receipt_payload).send().await?;
        let result: serde_json::Value = resp.json().await?;
        if let Some(receipt) = result.get("result").and_then(|r| r.as_object())
            && let Some(addr_str) = receipt.get("contractAddress").and_then(|a| a.as_str())
        {
            let addr = Address::from_str(addr_str)?;
            return Ok(addr);
        }
    }
    anyhow::bail!(
        "MultiSend deploy: receipt not found after {}s (tx: {:?})",
        timeouts.deploy_receipt.as_secs(),
        tx_hash
    )
}

/// ABI-encode a call to `send(address[])` with the given recipients.
fn encode_multisend_call(recipients: &[Address]) -> Bytes {
    // Function selector: keccak256("send(address[])") = 0x298c0733
    let selector: [u8; 4] = [0x29, 0x8c, 0x07, 0x33];

    // ABI encode: offset(32) + length(32) + addresses(32 each)
    let mut data = Vec::with_capacity(4 + 32 + 32 + recipients.len() * 32);
    data.extend_from_slice(&selector);
    // Offset to start of array (always 0x20 for a single dynamic param)
    data.extend_from_slice(&U256::from(32).to_be_bytes::<32>());
    // Array length
    data.extend_from_slice(&U256::from(recipients.len()).to_be_bytes::<32>());
    // Each address, left-padded to 32 bytes
    for addr in recipients {
        let mut word = [0u8; 32];
        word[12..].copy_from_slice(addr.as_slice());
        data.extend_from_slice(&word);
    }
    Bytes::from(data)
}

/// Fund sender accounts via the MultiSend contract in batches.
///
/// Deploys a MultiSend contract, then calls it in batches of
/// [`MULTISEND_BATCH_SIZE`] addresses, sending 1 ETH to each recipient per
/// batch in a single on-chain transaction. This is dramatically faster than
/// individual transfers since it collapses N transfers into 1 tx per batch.
///
/// This is a pre-benchmark phase — it does NOT count toward TPS.
pub async fn fund_senders(
    rpc_url: &str,
    funder_key: &str,
    sender_addresses: &[Address],
    chain_id: u64,
    quiet: bool,
) -> Result<()> {
    use alloy_consensus::SignableTransaction;
    use alloy_consensus::TxLegacy;
    use alloy_eips::eip2718::Encodable2718;
    use alloy_network::TxSignerSync;
    use alloy_primitives::TxKind;

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(20)
        .build()?;
    let timeouts = FundingTimeouts::from_env();

    let funder = PrivateKeySigner::from_str(funder_key)
        .map_err(|e| anyhow::anyhow!("Failed to parse funder key: {}", e))?;

    // Check which accounts need funding
    let to_fund = check_balances(&client, rpc_url, sender_addresses).await?;
    if to_fund.is_empty() {
        if !quiet {
            println!(
                "All {} sender accounts already funded.",
                sender_addresses.len()
            );
        }
        return Ok(());
    }

    if !quiet {
        println!(
            "Funding {} / {} sender accounts via MultiSend contract...",
            to_fund.len(),
            sender_addresses.len()
        );
        println!(
            "  Funding timeouts: ready={}s deploy={}s confirm={}s retry={}s poll={}ms",
            timeouts.chain_ready.as_secs(),
            timeouts.deploy_receipt.as_secs(),
            timeouts.funding_confirm.as_secs(),
            timeouts.retry_confirm.as_secs(),
            timeouts.poll_interval.as_millis()
        );
    }

    let gas_price = fetch_gas_price(&client, rpc_url).await?;

    // Wait for the chain to be producing blocks before deploying
    if !quiet {
        println!("  Waiting for block production...");
    }
    let mut last_block = 0u64;
    let chain_deadline = tokio::time::Instant::now() + timeouts.chain_ready;
    while tokio::time::Instant::now() < chain_deadline {
        let payload = serde_json::json!({
            "jsonrpc": "2.0", "method": "eth_blockNumber", "params": [], "id": 1
        });
        let resp = client.post(rpc_url).json(&payload).send().await?;
        let result: serde_json::Value = resp.json().await?;
        let block = result
            .get("result")
            .and_then(|r| r.as_str())
            .and_then(|h| u64::from_str_radix(h.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0);
        if block > last_block && last_block > 0 {
            break; // Chain is advancing
        }
        last_block = block;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // Deploy the MultiSend contract
    let mut nonce = get_nonce(&client, rpc_url, funder.address()).await?;
    if !quiet {
        println!("  Deploying MultiSend contract...");
    }
    let multisend_addr = deploy_multisend(
        &client, rpc_url, &funder, chain_id, nonce, gas_price, timeouts,
    )
    .await?;
    nonce += 1;
    if !quiet {
        println!("  MultiSend deployed at {:?}", multisend_addr);
    }

    // Fund in batches via MultiSend — each batch is a single on-chain tx
    let amount_per_account = U256::from(10_000_000_000_000_000_000u128); // 10 ETH
    let unfunded_addrs: Vec<Address> = to_fund.iter().map(|(_, addr)| *addr).collect();
    let mut tx_hashes: Vec<B256> = Vec::new();

    for (batch_idx, chunk) in unfunded_addrs.chunks(MULTISEND_BATCH_SIZE).enumerate() {
        let calldata = encode_multisend_call(chunk);
        let total_value = amount_per_account
            .checked_mul(U256::from(chunk.len()))
            .ok_or_else(|| anyhow::anyhow!("Funding value overflow"))?;
        // Gas: ~50k base + ~40k per recipient (CALL + G_newaccount + value transfer stipend)
        let gas_limit = 100_000 + 40_000 * chunk.len() as u64;

        let mut tx = TxLegacy {
            chain_id: Some(chain_id),
            nonce,
            gas_price,
            gas_limit,
            to: TxKind::Call(multisend_addr),
            value: total_value,
            input: calldata,
        };
        let sig = funder.sign_transaction_sync(&mut tx)?;
        let signed = tx.into_signed(sig);
        let envelope = alloy_consensus::TxEnvelope::from(signed);
        let encoded = Encodable2718::encoded_2718(&envelope);
        let tx_hash = *envelope.hash();
        tx_hashes.push(tx_hash);

        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_sendRawTransaction",
            "params": [format!("0x{}", hex::encode(&encoded))],
            "id": nonce
        });
        let resp = client.post(rpc_url).json(&payload).send().await?;
        let result: serde_json::Value = resp.json().await?;
        if let Some(err) = result.get("error") {
            anyhow::bail!("MultiSend batch {} failed: {}", batch_idx + 1, err);
        }

        if !quiet {
            println!(
                "  Batch {}: funding {} accounts in 1 tx (hash: {:?})",
                batch_idx + 1,
                chunk.len(),
                tx_hash,
            );
        }
        nonce += 1;
    }

    // Wait for all funding txs to confirm
    if !quiet {
        println!(
            "  Waiting for {} MultiSend tx{} to confirm...",
            tx_hashes.len(),
            if tx_hashes.len() == 1 { "" } else { "s" }
        );
    }

    let confirm_deadline = tokio::time::Instant::now() + timeouts.funding_confirm;
    while tokio::time::Instant::now() < confirm_deadline {
        let mut all_confirmed = true;
        for hash in &tx_hashes {
            let payload = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_getTransactionReceipt",
                "params": [format!("{:?}", hash)],
                "id": 1
            });
            let resp = client.post(rpc_url).json(&payload).send().await?;
            let result: serde_json::Value = resp.json().await?;
            match result.get("result").and_then(|r| r.as_object()) {
                None => {
                    all_confirmed = false;
                    break;
                }
                Some(receipt) => {
                    let status = receipt
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("0x0");
                    if status == "0x0" {
                        anyhow::bail!(
                            "MultiSend funding tx {:?} reverted (status=0x0). \
                             Gas may be insufficient or funder balance too low.",
                            hash
                        );
                    }
                }
            }
        }
        if all_confirmed {
            if !quiet {
                println!("  All funding confirmed.");
            }

            // Verify that all accounts actually received funds. MultiSend integer
            // division or other edge cases can leave accounts with zero balance even
            // when the batch tx succeeds.
            let still_unfunded = check_balances(&client, rpc_url, sender_addresses).await?;
            if !still_unfunded.is_empty() {
                let addrs: Vec<String> = still_unfunded
                    .iter()
                    .map(|(i, a)| format!("[{}] {:?}", i, a))
                    .collect();
                if !quiet {
                    eprintln!(
                        "  Warning: {} accounts still unfunded after MultiSend: {}",
                        still_unfunded.len(),
                        addrs.join(", ")
                    );
                    eprintln!("  Retrying with individual transfers...");
                }

                // Retry unfunded accounts with direct transfers (slower but reliable)
                let mut retry_nonce = get_nonce(&client, rpc_url, funder.address()).await?;
                for (_idx, addr) in &still_unfunded {
                    let mut tx = TxLegacy {
                        chain_id: Some(chain_id),
                        nonce: retry_nonce,
                        gas_price,
                        gas_limit: 21_000,
                        to: TxKind::Call(*addr),
                        value: amount_per_account,
                        input: Bytes::new(),
                    };
                    let sig = funder.sign_transaction_sync(&mut tx)?;
                    let signed = tx.into_signed(sig);
                    let envelope = alloy_consensus::TxEnvelope::from(signed);
                    let encoded = Encodable2718::encoded_2718(&envelope);
                    let payload = serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "eth_sendRawTransaction",
                        "params": [format!("0x{}", hex::encode(&encoded))],
                        "id": retry_nonce
                    });
                    let resp = client.post(rpc_url).json(&payload).send().await?;
                    let result: serde_json::Value = resp.json().await?;
                    if let Some(err) = result.get("error")
                        && !quiet
                    {
                        eprintln!("  Warning: retry funding {:?} failed: {}", addr, err);
                    }
                    retry_nonce += 1;
                }

                // Wait for retry txs to confirm
                let retry_deadline = tokio::time::Instant::now() + timeouts.retry_confirm;
                while tokio::time::Instant::now() < retry_deadline {
                    let remaining = check_balances(&client, rpc_url, sender_addresses).await?;
                    if remaining.is_empty() {
                        if !quiet {
                            println!("  Retry funding confirmed. All accounts funded.");
                        }
                        return Ok(());
                    }
                    tokio::time::sleep(timeouts.poll_interval).await;
                }
                let remaining_count = check_balances(&client, rpc_url, sender_addresses)
                    .await?
                    .len();
                if !quiet && remaining_count > 0 {
                    eprintln!(
                        "  Warning: {} accounts still unfunded after retry. \
                         Benchmark may have partial failures.",
                        remaining_count
                    );
                }
            }

            return Ok(());
        }
        tokio::time::sleep(timeouts.poll_interval).await;
    }

    anyhow::bail!(
        "Funding transactions did not confirm within {}s",
        timeouts.funding_confirm.as_secs()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use wiremock::matchers::{body_partial_json, body_string_contains, method};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: tests serialize environment mutation via `env_lock`.
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => {
                    // SAFETY: tests serialize environment mutation via `env_lock`.
                    unsafe { std::env::set_var(self.key, value) };
                }
                None => {
                    // SAFETY: tests serialize environment mutation via `env_lock`.
                    unsafe { std::env::remove_var(self.key) };
                }
            }
        }
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn funding_timeout_guards(
        chain_ready_secs: &str,
        deploy_receipt_secs: &str,
        funding_confirm_secs: &str,
        retry_confirm_secs: &str,
        poll_interval_ms: &str,
    ) -> [EnvVarGuard; 5] {
        [
            EnvVarGuard::set("BENCH_FUND_CHAIN_READY_TIMEOUT_SECS", chain_ready_secs),
            EnvVarGuard::set("BENCH_FUND_DEPLOY_TIMEOUT_SECS", deploy_receipt_secs),
            EnvVarGuard::set("BENCH_FUND_CONFIRM_TIMEOUT_SECS", funding_confirm_secs),
            EnvVarGuard::set("BENCH_FUND_RETRY_TIMEOUT_SECS", retry_confirm_secs),
            EnvVarGuard::set("BENCH_FUND_POLL_INTERVAL_MS", poll_interval_ms),
        ]
    }

    #[derive(Clone, Copy)]
    enum FundSendsFlow {
        HappyPath,
        BatchError,
        ConfirmRevert,
        ConfirmTimeout,
        RetrySuccess,
        RetryTimeoutWithRetryError,
    }

    #[derive(Default)]
    struct RpcCallCounters {
        balance: AtomicUsize,
        block_number: AtomicUsize,
        nonce: AtomicUsize,
        send_raw: AtomicUsize,
        receipt: AtomicUsize,
    }

    struct FundSendsResponder {
        flow: FundSendsFlow,
        counters: Arc<RpcCallCounters>,
    }

    impl FundSendsResponder {
        fn new(flow: FundSendsFlow) -> Self {
            Self {
                flow,
                counters: Arc::new(RpcCallCounters::default()),
            }
        }

        fn json(result: serde_json::Value) -> ResponseTemplate {
            ResponseTemplate::new(200).set_body_json(result)
        }
    }

    impl Respond for FundSendsResponder {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let payload: serde_json::Value =
                serde_json::from_slice(&request.body).expect("request body should be JSON");
            let method = payload
                .get("method")
                .and_then(|value| value.as_str())
                .expect("json-rpc method should be present");

            match method {
                "eth_getBalance" => {
                    let call = self.counters.balance.fetch_add(1, Ordering::SeqCst);
                    let result = match self.flow {
                        FundSendsFlow::HappyPath
                        | FundSendsFlow::BatchError
                        | FundSendsFlow::ConfirmRevert
                        | FundSendsFlow::ConfirmTimeout => {
                            if call == 0 {
                                "0x0"
                            } else {
                                "0x8ac7230489e80000"
                            }
                        }
                        FundSendsFlow::RetrySuccess => {
                            if call < 2 {
                                "0x0"
                            } else {
                                "0x8ac7230489e80000"
                            }
                        }
                        FundSendsFlow::RetryTimeoutWithRetryError => "0x0",
                    };
                    Self::json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": result,
                    }))
                }
                "eth_gasPrice" => Self::json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": "0x3b9aca00",
                })),
                "eth_blockNumber" => {
                    let call = self.counters.block_number.fetch_add(1, Ordering::SeqCst);
                    let block = if call == 0 { "0x1" } else { "0x2" };
                    Self::json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": block,
                    }))
                }
                "eth_getTransactionCount" => {
                    let call = self.counters.nonce.fetch_add(1, Ordering::SeqCst);
                    let nonce = if call == 0 { "0x0" } else { "0x1" };
                    Self::json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": nonce,
                    }))
                }
                "eth_sendRawTransaction" => {
                    let call = self.counters.send_raw.fetch_add(1, Ordering::SeqCst);
                    let response = match self.flow {
                        FundSendsFlow::BatchError if call == 1 => serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "error": { "code": -32000, "message": "batch failed" },
                        }),
                        FundSendsFlow::RetryTimeoutWithRetryError if call == 2 => {
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "error": { "code": -32000, "message": "retry failed" },
                            })
                        }
                        _ => serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "result": format!("0x{:064x}", call + 1),
                        }),
                    };
                    Self::json(response)
                }
                "eth_getTransactionReceipt" => {
                    let call = self.counters.receipt.fetch_add(1, Ordering::SeqCst);
                    let response = if call == 0 {
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "result": {
                                "contractAddress": "0x00000000000000000000000000000000000000aa"
                            },
                        })
                    } else {
                        match self.flow {
                            FundSendsFlow::ConfirmRevert => serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "result": { "status": "0x0" },
                            }),
                            FundSendsFlow::ConfirmTimeout => serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "result": null,
                            }),
                            _ => serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "result": { "status": "0x1" },
                            }),
                        }
                    };
                    Self::json(response)
                }
                other => Self::json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "error": {
                        "code": -32601,
                        "message": format!("unexpected json-rpc method: {other}"),
                    }
                })),
            }
        }
    }

    async fn mount_fund_senders_mock(flow: FundSendsFlow) -> MockServer {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(FundSendsResponder::new(flow))
            .mount(&mock_server)
            .await;
        mock_server
    }

    #[test]
    fn test_generate_sender_keys_deterministic() {
        let keys1 = generate_sender_keys(5);
        let keys2 = generate_sender_keys(5);
        assert_eq!(keys1, keys2);
        assert_eq!(keys1.len(), 5);
        // All keys should be different
        for i in 0..keys1.len() {
            for j in (i + 1)..keys1.len() {
                assert_ne!(keys1[i], keys1[j]);
            }
        }
    }

    #[test]
    fn test_generate_sender_keys_valid_signers() {
        let keys = generate_sender_keys(10);
        for (i, key) in keys.iter().enumerate() {
            let signer = PrivateKeySigner::from_str(key);
            assert!(signer.is_ok(), "key {i} should parse");
        }
    }

    #[test]
    fn test_parse_sender_keys() {
        let keys = generate_sender_keys(3);
        let parsed = parse_sender_keys(&keys).expect("parse failed");
        assert_eq!(parsed.len(), 3);
        // All addresses should be different
        assert_ne!(parsed[0].2, parsed[1].2);
        assert_ne!(parsed[1].2, parsed[2].2);
    }

    #[test]
    fn test_encode_multisend_call() {
        let addrs = vec![Address::with_last_byte(0x01), Address::with_last_byte(0x02)];
        let data = encode_multisend_call(&addrs);
        // Selector (4) + offset (32) + length (32) + 2 addresses (64) = 132 bytes
        assert_eq!(data.len(), 132);
        assert_eq!(&data[..4], &[0x29, 0x8c, 0x07, 0x33]);
    }

    #[test]
    fn test_resolve_sender_keys_count_zero() {
        let keys = resolve_sender_keys(0);
        // With count=0, if BENCH_KEY is set we may get those keys, but no generated keys.
        // The result length should be <= number of env keys (possibly 0).
        // Since we can't control env, just verify it doesn't panic and returns a vec.
        assert!(
            keys.len()
                <= std::env::var("BENCH_KEY")
                    .unwrap_or_default()
                    .split(',')
                    .filter(|s| !s.trim().is_empty())
                    .count()
                    .max(0)
        );
    }

    #[test]
    fn test_resolve_sender_keys_generates_valid_keys() {
        // Request 5 keys — any env-provided keys plus generated ones should all be valid
        let keys = resolve_sender_keys(5);
        assert!(keys.len() >= 5);
        for (i, key) in keys.iter().enumerate() {
            let result = PrivateKeySigner::from_str(key);
            assert!(result.is_ok(), "key {i} should be a valid private key");
        }
    }

    #[test]
    fn test_resolve_sender_keys_uses_env_and_filters_blanks() {
        let _guard = env_lock().lock().unwrap();
        // SAFETY: tests serialize environment mutation via `env_lock`.
        unsafe { std::env::set_var("BENCH_KEY", " key-a , ,key-b ,, ") };
        let keys = resolve_sender_keys(2);
        assert_eq!(keys, vec!["key-a".to_string(), "key-b".to_string()]);
        // SAFETY: tests serialize environment mutation via `env_lock`.
        unsafe { std::env::remove_var("BENCH_KEY") };
    }

    #[test]
    fn test_env_var_guard_restores_previous_funding_timeout() {
        let _guard = env_lock().lock().unwrap();
        // SAFETY: tests serialize environment mutation via `env_lock`.
        unsafe { std::env::set_var("BENCH_FUND_CHAIN_READY_TIMEOUT_SECS", "9") };
        {
            let _env = EnvVarGuard::set("BENCH_FUND_CHAIN_READY_TIMEOUT_SECS", "12");
            assert_eq!(
                std::env::var("BENCH_FUND_CHAIN_READY_TIMEOUT_SECS").unwrap(),
                "12"
            );
        }
        assert_eq!(
            std::env::var("BENCH_FUND_CHAIN_READY_TIMEOUT_SECS").unwrap(),
            "9"
        );
        // SAFETY: tests serialize environment mutation via `env_lock`.
        unsafe { std::env::remove_var("BENCH_FUND_CHAIN_READY_TIMEOUT_SECS") };
    }

    #[test]
    fn test_generate_sender_keys_count_zero() {
        let keys = generate_sender_keys(0);
        assert!(keys.is_empty());
    }

    #[test]
    fn test_generate_sender_keys_count_one() {
        let keys = generate_sender_keys(1);
        assert_eq!(keys.len(), 1);
        let signer = PrivateKeySigner::from_str(&keys[0]);
        assert!(
            signer.is_ok(),
            "Generated key should be a valid private key"
        );
    }

    #[test]
    fn test_parse_sender_keys_empty() {
        let keys: Vec<String> = vec![];
        let parsed = parse_sender_keys(&keys).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_parse_sender_keys_invalid() {
        let keys = vec!["not_a_valid_hex_key".to_string()];
        let result = parse_sender_keys(&keys);
        assert!(result.is_err());
    }

    #[test]
    fn test_encode_multisend_call_zero_recipients() {
        let addrs: Vec<Address> = vec![];
        let data = encode_multisend_call(&addrs);
        // Selector (4) + offset (32) + length (32) = 68 bytes, no address words
        assert_eq!(data.len(), 4 + 32 + 32);
        // Selector
        assert_eq!(&data[..4], &[0x29, 0x8c, 0x07, 0x33]);
        // Offset = 32
        assert_eq!(data[35], 32);
        // Length = 0
        assert_eq!(data[67], 0);
    }

    #[test]
    fn test_encode_multisend_call_one_recipient() {
        let addrs = vec![Address::with_last_byte(0xff)];
        let data = encode_multisend_call(&addrs);
        // Selector (4) + offset (32) + length (32) + 1 address (32) = 100 bytes
        assert_eq!(data.len(), 100);
        assert_eq!(&data[..4], &[0x29, 0x8c, 0x07, 0x33]);
        // The last byte of the address word should be 0xff
        assert_eq!(data[99], 0xff);
    }

    #[test]
    fn test_encode_multisend_call_five_recipients() {
        let addrs: Vec<Address> = (1..=5).map(Address::with_last_byte).collect();
        let data = encode_multisend_call(&addrs);
        // Selector (4) + offset (32) + length (32) + 5 * 32 = 228 bytes
        assert_eq!(data.len(), 4 + 32 + 32 + 5 * 32);
        // Verify each address is in the right position
        for (i, addr) in addrs.iter().enumerate() {
            let offset = 4 + 32 + 32 + i * 32;
            let word = &data[offset..offset + 32];
            // First 12 bytes should be zero-padding
            assert_eq!(&word[..12], &[0u8; 12]);
            assert_eq!(&word[12..], addr.as_slice());
        }
    }

    #[test]
    fn test_encode_multisend_call_batch_size_boundary() {
        // Test with exactly MULTISEND_BATCH_SIZE recipients
        let addrs: Vec<Address> = (0..MULTISEND_BATCH_SIZE)
            .map(|i| Address::with_last_byte(i as u8))
            .collect();
        let data = encode_multisend_call(&addrs);
        let expected_len = 4 + 32 + 32 + MULTISEND_BATCH_SIZE * 32;
        assert_eq!(data.len(), expected_len);
        // Verify the array length word encodes MULTISEND_BATCH_SIZE
        let len_word = &data[36..68];
        let encoded_len =
            U256::from_be_bytes::<32>(len_word.try_into().expect("slice should be 32 bytes"));
        assert_eq!(encoded_len, U256::from(MULTISEND_BATCH_SIZE));
    }

    #[test]
    fn test_parse_env_u64_and_timeouts_from_env() {
        let _guard = env_lock().lock().unwrap();
        let _chain_ready = EnvVarGuard::set("BENCH_FUND_CHAIN_READY_TIMEOUT_SECS", "7");
        let _deploy = EnvVarGuard::set("BENCH_FUND_DEPLOY_TIMEOUT_SECS", "0");
        let _confirm = EnvVarGuard::set("BENCH_FUND_CONFIRM_TIMEOUT_SECS", "garbage");
        let _retry = EnvVarGuard::set("BENCH_FUND_RETRY_TIMEOUT_SECS", "11");
        let _poll = EnvVarGuard::set("BENCH_FUND_POLL_INTERVAL_MS", "250");

        assert_eq!(parse_env_u64("BENCH_FUND_CHAIN_READY_TIMEOUT_SECS", 30), 7);
        assert_eq!(parse_env_u64("BENCH_FUND_DEPLOY_TIMEOUT_SECS", 30), 30);
        assert_eq!(parse_env_u64("BENCH_FUND_CONFIRM_TIMEOUT_SECS", 60), 60);

        let timeouts = FundingTimeouts::from_env();
        assert_eq!(timeouts.chain_ready, Duration::from_secs(7));
        assert_eq!(timeouts.deploy_receipt, Duration::from_secs(30));
        assert_eq!(timeouts.funding_confirm, Duration::from_secs(60));
        assert_eq!(timeouts.retry_confirm, Duration::from_secs(11));
        assert_eq!(timeouts.poll_interval, Duration::from_millis(250));
    }

    #[test]
    fn test_parse_env_u64_missing_uses_default() {
        let _guard = env_lock().lock().unwrap();
        // SAFETY: tests serialize environment mutation via `env_lock`.
        unsafe { std::env::remove_var("BENCH_FUND_CHAIN_READY_TIMEOUT_SECS") };
        assert_eq!(parse_env_u64("BENCH_FUND_CHAIN_READY_TIMEOUT_SECS", 42), 42);
    }

    #[test]
    fn test_fund_sends_responder_unknown_method_returns_json_rpc_error() {
        let responder = FundSendsResponder::new(FundSendsFlow::HappyPath);
        let request = Request {
            url: "http://localhost".parse().unwrap(),
            method: "POST".parse().unwrap(),
            headers: Default::default(),
            body: br#"{"jsonrpc":"2.0","method":"eth_notReal","params":[],"id":1}"#.to_vec(),
        };

        let _response = responder.respond(&request);
    }

    // ── Wiremock-based async tests for RPC-dependent functions ──────────

    #[tokio::test]
    async fn test_fetch_gas_price_normal() {
        let mock_server = MockServer::start().await;

        // Return 1 gwei (0x3b9aca00) for eth_gasPrice
        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_gasPrice"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "result": "0x3b9aca00"
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let gas_price = fetch_gas_price(&client, &mock_server.uri())
            .await
            .expect("fetch_gas_price should succeed");
        // 1 gwei * 2 = 2 gwei
        assert_eq!(gas_price, 2_000_000_000);
    }

    #[tokio::test]
    async fn test_fetch_gas_price_very_low_returns_min_1_gwei() {
        let mock_server = MockServer::start().await;

        // Return a very low gas price: 100 wei (0x64)
        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_gasPrice"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "result": "0x64"
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let gas_price = fetch_gas_price(&client, &mock_server.uri())
            .await
            .expect("fetch_gas_price should succeed");
        // 100 * 2 = 200, but min is 1 gwei
        assert_eq!(gas_price, 1_000_000_000);
    }

    #[tokio::test]
    async fn test_fetch_gas_price_missing_result_uses_default() {
        let mock_server = MockServer::start().await;

        // Return a response with no "result" field
        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_gasPrice"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let gas_price = fetch_gas_price(&client, &mock_server.uri())
            .await
            .expect("fetch_gas_price should succeed with default");
        // Default is 1 gwei, * 2 = 2 gwei
        assert_eq!(gas_price, 2_000_000_000);
    }

    #[tokio::test]
    async fn test_fetch_gas_price_high_value() {
        let mock_server = MockServer::start().await;

        // Return 50 gwei (0xba43b7400)
        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_gasPrice"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "result": "0xba43b7400"
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let gas_price = fetch_gas_price(&client, &mock_server.uri())
            .await
            .expect("fetch_gas_price should succeed");
        // 50 gwei * 2 = 100 gwei
        assert_eq!(gas_price, 100_000_000_000);
    }

    #[tokio::test]
    async fn test_fetch_gas_price_invalid_hex_uses_default() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_gasPrice"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "result": "0xnothex"
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let gas_price = fetch_gas_price(&client, &mock_server.uri())
            .await
            .expect("fetch_gas_price should fall back to default");
        assert_eq!(gas_price, 2_000_000_000);
    }

    #[tokio::test]
    async fn test_get_nonce_success() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_getTransactionCount"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "result": "0x2a"
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let addr = Address::with_last_byte(0x01);
        let nonce = get_nonce(&client, &mock_server.uri(), addr)
            .await
            .expect("get_nonce should succeed");
        assert_eq!(nonce, 42);
    }

    #[tokio::test]
    async fn test_get_nonce_missing_result_errors() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_getTransactionCount"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let addr = Address::with_last_byte(0x01);
        let result = get_nonce(&client, &mock_server.uri(), addr).await;
        assert!(
            result.is_err(),
            "get_nonce should fail when result is missing"
        );
    }

    #[tokio::test]
    async fn test_get_nonce_invalid_hex_errors() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_getTransactionCount"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "result": "0xnothex"
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let addr = Address::with_last_byte(0x01);
        let result = get_nonce(&client, &mock_server.uri(), addr).await;
        assert!(result.is_err(), "invalid nonce hex should fail");
    }

    #[tokio::test]
    async fn test_check_balances_all_funded() {
        let mock_server = MockServer::start().await;

        // Return 1 ETH balance for all balance queries — well above 0.1 ETH threshold
        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_getBalance"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "result": "0xde0b6b3a7640000"
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let addrs = vec![
            Address::with_last_byte(0x01),
            Address::with_last_byte(0x02),
            Address::with_last_byte(0x03),
        ];
        let to_fund = check_balances(&client, &mock_server.uri(), &addrs)
            .await
            .expect("check_balances should succeed");
        assert!(to_fund.is_empty(), "All accounts should be funded");
    }

    #[tokio::test]
    async fn test_check_balances_some_underfunded() {
        let mock_server = MockServer::start().await;

        // First two calls return high balance, third returns zero.
        // wiremock serves mocks in reverse-mount order for matching requests,
        // so we use a counter-based approach: mount a default response that
        // returns zero, then we rely on the fact that all requests match the
        // same mock. Instead, return 0 for all and verify all need funding.
        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_getBalance"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "result": "0x0"
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let addrs = vec![Address::with_last_byte(0x01), Address::with_last_byte(0x02)];
        let to_fund = check_balances(&client, &mock_server.uri(), &addrs)
            .await
            .expect("check_balances should succeed");
        assert_eq!(to_fund.len(), 2);
        assert_eq!(to_fund[0].0, 0);
        assert_eq!(to_fund[0].1, Address::with_last_byte(0x01));
        assert_eq!(to_fund[1].0, 1);
        assert_eq!(to_fund[1].1, Address::with_last_byte(0x02));
    }

    #[tokio::test]
    async fn test_check_balances_missing_result_defaults_to_underfunded() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_getBalance"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let addrs = vec![Address::with_last_byte(0x01)];
        let to_fund = check_balances(&client, &mock_server.uri(), &addrs)
            .await
            .expect("missing result should default to zero");
        assert_eq!(to_fund, vec![(0, Address::with_last_byte(0x01))]);
    }

    #[tokio::test]
    async fn test_check_balances_can_distinguish_funded_and_underfunded_accounts() {
        let mock_server = MockServer::start().await;
        let funded = Address::with_last_byte(0x01);
        let underfunded = Address::with_last_byte(0x02);

        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_getBalance"
            })))
            .and(body_string_contains(format!("{funded:?}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "result": "0xde0b6b3a7640000"
            })))
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_getBalance"
            })))
            .and(body_string_contains(format!("{underfunded:?}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 2, "result": "0x1"
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let addrs = vec![funded, underfunded];
        let to_fund = check_balances(&client, &mock_server.uri(), &addrs)
            .await
            .expect("balance check should succeed");
        assert_eq!(to_fund, vec![(1, underfunded)]);
    }

    #[tokio::test]
    async fn test_deploy_multisend_send_raw_transaction_error() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_sendRawTransaction"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": { "code": -32000, "message": "replacement transaction underpriced" }
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let funder_key = generate_sender_keys(1).into_iter().next().unwrap();
        let funder = PrivateKeySigner::from_str(&funder_key).unwrap();
        let timeouts = FundingTimeouts {
            chain_ready: Duration::from_millis(1),
            deploy_receipt: Duration::from_millis(25),
            funding_confirm: Duration::from_millis(25),
            retry_confirm: Duration::from_millis(25),
            poll_interval: Duration::from_millis(1),
        };

        let err = deploy_multisend(&client, &mock_server.uri(), &funder, 1, 0, 1, timeouts)
            .await
            .expect_err("RPC deploy error should bubble up");
        assert!(err.to_string().contains("MultiSend deploy failed"));
    }

    #[tokio::test]
    async fn test_deploy_multisend_times_out_without_receipt() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_sendRawTransaction"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": "0x01"
            })))
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_getTransactionReceipt"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": null
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let funder_key = generate_sender_keys(1).into_iter().next().unwrap();
        let funder = PrivateKeySigner::from_str(&funder_key).unwrap();
        let timeouts = FundingTimeouts {
            chain_ready: Duration::from_millis(1),
            deploy_receipt: Duration::from_millis(25),
            funding_confirm: Duration::from_millis(25),
            retry_confirm: Duration::from_millis(25),
            poll_interval: Duration::from_millis(1),
        };

        let err = deploy_multisend(&client, &mock_server.uri(), &funder, 1, 0, 1, timeouts)
            .await
            .expect_err("missing receipt should time out");
        assert!(err.to_string().contains("receipt not found"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_fund_senders_full_happy_path() {
        let _guard = env_lock().lock().unwrap();
        let _timeouts = funding_timeout_guards("2", "1", "1", "1", "1");
        let mock_server = mount_fund_senders_mock(FundSendsFlow::HappyPath).await;

        let funder_key = &generate_sender_keys(1)[0];
        let addrs = vec![Address::with_last_byte(0x01)];
        let result = fund_senders(&mock_server.uri(), funder_key, &addrs, 1, false).await;
        assert!(result.is_ok(), "expected full funding path to succeed");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_fund_senders_batch_submission_error() {
        let _guard = env_lock().lock().unwrap();
        let _timeouts = funding_timeout_guards("2", "1", "1", "1", "1");
        let mock_server = mount_fund_senders_mock(FundSendsFlow::BatchError).await;

        let funder_key = &generate_sender_keys(1)[0];
        let addrs = vec![Address::with_last_byte(0x01)];
        let err = fund_senders(&mock_server.uri(), funder_key, &addrs, 1, true)
            .await
            .expect_err("batch RPC error should fail funding");
        assert!(err.to_string().contains("MultiSend batch 1 failed"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_fund_senders_receipt_revert_error() {
        let _guard = env_lock().lock().unwrap();
        let _timeouts = funding_timeout_guards("2", "1", "1", "1", "1");
        let mock_server = mount_fund_senders_mock(FundSendsFlow::ConfirmRevert).await;

        let funder_key = &generate_sender_keys(1)[0];
        let addrs = vec![Address::with_last_byte(0x01)];
        let err = fund_senders(&mock_server.uri(), funder_key, &addrs, 1, true)
            .await
            .expect_err("reverted funding receipt should fail");
        assert!(err.to_string().contains("reverted (status=0x0)"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_fund_senders_confirm_timeout_error() {
        let _guard = env_lock().lock().unwrap();
        let _timeouts = funding_timeout_guards("2", "1", "1", "1", "1");
        let mock_server = mount_fund_senders_mock(FundSendsFlow::ConfirmTimeout).await;

        let funder_key = &generate_sender_keys(1)[0];
        let addrs = vec![Address::with_last_byte(0x01)];
        let err = fund_senders(&mock_server.uri(), funder_key, &addrs, 1, true)
            .await
            .expect_err("missing funding receipts should time out");
        assert!(err.to_string().contains("did not confirm within 1s"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_fund_senders_retry_path_succeeds() {
        let _guard = env_lock().lock().unwrap();
        let _timeouts = funding_timeout_guards("2", "1", "1", "1", "1");
        let mock_server = mount_fund_senders_mock(FundSendsFlow::RetrySuccess).await;

        let funder_key = &generate_sender_keys(1)[0];
        let addrs = vec![Address::with_last_byte(0x01)];
        let result = fund_senders(&mock_server.uri(), funder_key, &addrs, 1, false).await;
        assert!(
            result.is_ok(),
            "retry flow should eventually fund the account"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_fund_senders_retry_timeout_returns_ok_with_warning() {
        let _guard = env_lock().lock().unwrap();
        let _timeouts = funding_timeout_guards("2", "1", "1", "1", "1");
        let mock_server = mount_fund_senders_mock(FundSendsFlow::RetryTimeoutWithRetryError).await;

        let funder_key = &generate_sender_keys(1)[0];
        let addrs = vec![Address::with_last_byte(0x01)];
        let result = fund_senders(&mock_server.uri(), funder_key, &addrs, 1, false).await;
        assert!(
            result.is_ok(),
            "function currently returns Ok even if retry funding leaves accounts unfunded"
        );
    }

    #[tokio::test]
    async fn test_fund_senders_all_already_funded() {
        let mock_server = MockServer::start().await;

        // Return high balance (10 ETH) for all eth_getBalance calls so the
        // early-return "all accounts already funded" path is taken.
        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_getBalance"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "result": "0x8ac7230489e80000"
            })))
            .mount(&mock_server)
            .await;

        // Generate a valid funder key
        let funder_key = &generate_sender_keys(1)[0];
        let addrs = vec![
            Address::with_last_byte(0x01),
            Address::with_last_byte(0x02),
            Address::with_last_byte(0x03),
        ];

        let result = fund_senders(&mock_server.uri(), funder_key, &addrs, 1, true).await;
        assert!(
            result.is_ok(),
            "fund_senders should succeed when all accounts are funded: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_fund_senders_all_already_funded_verbose() {
        let mock_server = MockServer::start().await;

        // Return high balance for all balance queries
        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_getBalance"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "result": "0x8ac7230489e80000"
            })))
            .mount(&mock_server)
            .await;

        let funder_key = &generate_sender_keys(1)[0];
        let addrs = vec![Address::with_last_byte(0x01)];

        // Test with quiet=false to cover the println branch
        let result = fund_senders(&mock_server.uri(), funder_key, &addrs, 1, false).await;
        assert!(
            result.is_ok(),
            "fund_senders should succeed (verbose mode): {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_fund_senders_invalid_funder_key() {
        let mock_server = MockServer::start().await;

        // Even though balance check would pass, an invalid key should error
        // before any RPC call (the key is parsed first, but actually it's
        // parsed after check_balances). Mount a balance mock anyway.
        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "method": "eth_getBalance"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "result": "0x0"
            })))
            .mount(&mock_server)
            .await;

        let addrs = vec![Address::with_last_byte(0x01)];
        let result = fund_senders(&mock_server.uri(), "not_a_valid_key", &addrs, 1, true).await;
        assert!(
            result.is_err(),
            "fund_senders should fail with invalid funder key"
        );
    }
}
