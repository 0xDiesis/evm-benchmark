//! Deploy benchmark contracts (ERC-20, AMM pair, NFT) for EVM workload testing.
//!
//! Contracts are compiled from `contracts/src/benchmark/` and their bytecode is
//! embedded here. Deployment happens before the benchmark starts and is NOT
//! counted toward TPS.

use alloy_primitives::{Address, Bytes, U256};
use alloy_signer_local::PrivateKeySigner;
use anyhow::Result;
use std::str::FromStr;
use std::time::Duration;

/// Addresses of deployed benchmark contracts.
#[derive(Clone, Debug)]
pub struct EvmContracts {
    /// ERC-20 token contract addresses.
    pub tokens: Vec<Address>,
    /// AMM pair contract addresses (each pair references two tokens).
    pub pairs: Vec<Address>,
    /// NFT contract addresses.
    pub nfts: Vec<Address>,
}

// ── Embedded bytecode ──────────────────────────────────────────────────────
// From contracts/out/{BenchmarkToken,BenchmarkPair,BenchmarkNFT}.sol/*.json
// These are the creation bytecodes (include constructor logic).

const TOKEN_BYTECODE: &str = include_str!("../../bytecode/BenchmarkToken.hex");
const PAIR_BYTECODE: &str = include_str!("../../bytecode/BenchmarkPair.hex");
const NFT_BYTECODE: &str = include_str!("../../bytecode/BenchmarkNFT.hex");

/// Deploy a single contract and return its address.
async fn deploy_one(
    client: &reqwest::Client,
    rpc_url: &str,
    deployer: &PrivateKeySigner,
    chain_id: u64,
    gas_price: u128,
    nonce: u64,
    bytecode: &[u8],
) -> Result<(Address, u64)> {
    deploy_one_with_polling(
        client,
        rpc_url,
        deployer,
        chain_id,
        gas_price,
        nonce,
        bytecode,
        120,
        Duration::from_millis(500),
    )
    .await
}

async fn deploy_one_with_polling(
    client: &reqwest::Client,
    rpc_url: &str,
    deployer: &PrivateKeySigner,
    chain_id: u64,
    gas_price: u128,
    nonce: u64,
    bytecode: &[u8],
    max_receipt_polls: usize,
    poll_interval: Duration,
) -> Result<(Address, u64)> {
    use alloy_consensus::{SignableTransaction, TxLegacy};
    use alloy_eips::eip2718::Encodable2718;
    use alloy_network::TxSignerSync;
    use alloy_primitives::TxKind;

    let mut tx = TxLegacy {
        chain_id: Some(chain_id),
        nonce,
        gas_price,
        gas_limit: 3_000_000, // generous for contract creation
        to: TxKind::Create,
        value: U256::ZERO,
        input: Bytes::from(bytecode.to_vec()),
    };

    let sig = deployer.sign_transaction_sync(&mut tx)?;
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
        anyhow::bail!("Contract deploy tx failed: {}", err);
    }

    // Wait for receipt
    for _ in 0..max_receipt_polls {
        tokio::time::sleep(poll_interval).await;
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
            return Ok((addr, nonce + 1));
        }
    }
    anyhow::bail!(
        "Contract deploy receipt not found after 60s (tx: {:?})",
        tx_hash
    )
}

fn pair_bytecode_with_args(pair_bytecode: &[u8], token_a: Address, token_b: Address) -> Vec<u8> {
    let mut bytecode_with_args = pair_bytecode.to_vec();

    let mut token_a_word = [0u8; 32];
    token_a_word[12..].copy_from_slice(token_a.as_slice());
    bytecode_with_args.extend_from_slice(&token_a_word);

    let mut token_b_word = [0u8; 32];
    token_b_word[12..].copy_from_slice(token_b.as_slice());
    bytecode_with_args.extend_from_slice(&token_b_word);

    bytecode_with_args
}

/// Deploy benchmark contracts for EVM testing.
///
/// Deploys `token_count` ERC-20 tokens, `pair_count` AMM pairs (each referencing
/// two tokens), and `nft_count` NFT contracts. All deployed from the same
/// deployer key.
pub async fn deploy_contracts(
    rpc_url: &str,
    deployer_key: &str,
    chain_id: u64,
    token_count: u32,
    pair_count: u32,
    nft_count: u32,
    quiet: bool,
) -> Result<EvmContracts> {
    if pair_count > 0 && token_count == 0 {
        anyhow::bail!("pair deployment requires at least one token")
    }

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(10)
        .build()?;

    let deployer = PrivateKeySigner::from_str(deployer_key)
        .map_err(|e| anyhow::anyhow!("Failed to parse deployer key: {}", e))?;

    let gas_price = crate::funding::fetch_gas_price(&client, rpc_url).await?;

    // Wait for block production
    if !quiet {
        println!("  Deploying benchmark contracts...");
    }

    // Get deployer nonce
    let nonce_payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_getTransactionCount",
        "params": [format!("{:?}", deployer.address()), "pending"],
        "id": 1
    });
    let resp = client.post(rpc_url).json(&nonce_payload).send().await?;
    let result: serde_json::Value = resp.json().await?;
    let nonce_hex = result
        .get("result")
        .and_then(|r| r.as_str())
        .ok_or_else(|| anyhow::anyhow!("Failed to get deployer nonce"))?;
    let mut nonce = u64::from_str_radix(nonce_hex.trim_start_matches("0x"), 16)?;

    let token_bc = hex::decode(TOKEN_BYTECODE.trim())?;
    let pair_bc_raw = hex::decode(PAIR_BYTECODE.trim())?;
    let nft_bc = hex::decode(NFT_BYTECODE.trim())?;

    // Deploy tokens
    let mut tokens = Vec::new();
    for i in 0..token_count {
        let (addr, next_nonce) = deploy_one(
            &client, rpc_url, &deployer, chain_id, gas_price, nonce, &token_bc,
        )
        .await?;
        if !quiet {
            println!("    Token {}: {:?}", i, addr);
        }
        tokens.push(addr);
        nonce = next_nonce;
    }

    // Deploy pairs (each references token[i] and token[(i+1) % len])
    let mut pairs = Vec::new();
    for i in 0..pair_count {
        let t0 = tokens[i as usize % tokens.len()];
        let t1 = tokens[(i as usize + 1) % tokens.len()];
        let bytecode_with_args = pair_bytecode_with_args(&pair_bc_raw, t0, t1);

        let (addr, next_nonce) = deploy_one(
            &client,
            rpc_url,
            &deployer,
            chain_id,
            gas_price,
            nonce,
            &bytecode_with_args,
        )
        .await?;
        if !quiet {
            println!("    Pair {}: {:?} (tokens: {:?}, {:?})", i, addr, t0, t1);
        }
        pairs.push(addr);
        nonce = next_nonce;
    }

    // Deploy NFTs
    let mut nfts = Vec::new();
    for i in 0..nft_count {
        let (addr, next_nonce) = deploy_one(
            &client, rpc_url, &deployer, chain_id, gas_price, nonce, &nft_bc,
        )
        .await?;
        if !quiet {
            println!("    NFT {}: {:?}", i, addr);
        }
        nfts.push(addr);
        nonce = next_nonce;
    }

    if !quiet {
        println!(
            "  Deployed {} tokens, {} pairs, {} NFTs.",
            tokens.len(),
            pairs.len(),
            nfts.len()
        );
    }

    Ok(EvmContracts {
        tokens,
        pairs,
        nfts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_key() -> &'static str {
        "0x0123456789012345678901234567890123456789012345678901234567890123"
    }

    fn test_signer() -> PrivateKeySigner {
        PrivateKeySigner::from_str(test_key()).unwrap()
    }

    #[test]
    fn test_evm_contracts_struct() {
        let contracts = EvmContracts {
            tokens: vec![Address::with_last_byte(1)],
            pairs: vec![Address::with_last_byte(2)],
            nfts: vec![Address::with_last_byte(3)],
        };
        assert_eq!(contracts.tokens.len(), 1);
        assert_eq!(contracts.pairs.len(), 1);
        assert_eq!(contracts.nfts.len(), 1);
    }

    #[test]
    fn test_evm_contracts_empty_vecs() {
        let contracts = EvmContracts {
            tokens: vec![],
            pairs: vec![],
            nfts: vec![],
        };
        assert!(contracts.tokens.is_empty());
        assert!(contracts.pairs.is_empty());
        assert!(contracts.nfts.is_empty());
    }

    #[test]
    fn test_evm_contracts_clone() {
        let contracts = EvmContracts {
            tokens: vec![Address::with_last_byte(1), Address::with_last_byte(2)],
            pairs: vec![Address::with_last_byte(3)],
            nfts: vec![Address::with_last_byte(4)],
        };
        let cloned = contracts.clone();
        assert_eq!(cloned.tokens, contracts.tokens);
        assert_eq!(cloned.pairs, contracts.pairs);
        assert_eq!(cloned.nfts, contracts.nfts);
    }

    #[test]
    fn test_pair_bytecode_with_args_appends_two_abi_words() {
        let bytecode = vec![0xde, 0xad];
        let token_a = Address::with_last_byte(0xaa);
        let token_b = Address::with_last_byte(0xbb);
        let mut expected_a = [0u8; 32];
        expected_a[31] = 0xaa;
        let mut expected_b = [0u8; 32];
        expected_b[31] = 0xbb;

        let encoded = pair_bytecode_with_args(&bytecode, token_a, token_b);

        assert_eq!(encoded.len(), bytecode.len() + 64);
        assert_eq!(&encoded[..2], &[0xde, 0xad]);
        assert_eq!(&encoded[2..34], expected_a.as_slice());
        assert_eq!(&encoded[34..66], expected_b.as_slice());
    }

    #[tokio::test]
    async fn test_deploy_one_with_polling_returns_contract_address() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({"method": "eth_sendRawTransaction"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "result": "0x1234",
                "id": 1
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({"method": "eth_getTransactionReceipt"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "result": {
                    "contractAddress": "0x00000000000000000000000000000000000000aa"
                },
                "id": 1
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let (address, next_nonce) = deploy_one_with_polling(
            &client,
            &server.uri(),
            &test_signer(),
            1,
            1_000_000_000,
            7,
            &[0xde, 0xad],
            1,
            Duration::ZERO,
        )
        .await
        .unwrap();

        assert_eq!(address, Address::with_last_byte(0xaa));
        assert_eq!(next_nonce, 8);
    }

    #[tokio::test]
    async fn test_deploy_one_with_polling_surfaces_rpc_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({"method": "eth_sendRawTransaction"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "error": { "code": -32000, "message": "boom" },
                "id": 1
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let err = deploy_one_with_polling(
            &client,
            &server.uri(),
            &test_signer(),
            1,
            1_000_000_000,
            0,
            &[0xde, 0xad],
            1,
            Duration::ZERO,
        )
        .await
        .unwrap_err();

        assert!(
            err.to_string().contains("Contract deploy tx failed"),
            "unexpected error: {err:#}"
        );
    }

    #[tokio::test]
    async fn test_deploy_one_with_polling_times_out_without_receipt() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({"method": "eth_sendRawTransaction"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "result": "0x1234",
                "id": 1
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({"method": "eth_getTransactionReceipt"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "result": null,
                "id": 1
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let err = deploy_one_with_polling(
            &client,
            &server.uri(),
            &test_signer(),
            1,
            1_000_000_000,
            0,
            &[0xde, 0xad],
            2,
            Duration::ZERO,
        )
        .await
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("Contract deploy receipt not found"),
            "unexpected error: {err:#}"
        );
    }

    #[tokio::test]
    async fn test_deploy_one_with_polling_rejects_invalid_receipt_address() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({"method": "eth_sendRawTransaction"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "result": "0x1234",
                "id": 1
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({"method": "eth_getTransactionReceipt"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "result": {
                    "contractAddress": "not-an-address"
                },
                "id": 1
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let err = deploy_one_with_polling(
            &client,
            &server.uri(),
            &test_signer(),
            1,
            1_000_000_000,
            0,
            &[0xde, 0xad],
            1,
            Duration::ZERO,
        )
        .await
        .unwrap_err();

        assert!(
            err.to_string().contains("invalid"),
            "unexpected error: {err:#}"
        );
    }

    #[tokio::test]
    async fn test_deploy_contracts_rejects_pairs_without_tokens() {
        let err = deploy_contracts("http://localhost:8545", test_key(), 1, 0, 1, 0, true)
            .await
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("pair deployment requires at least one token"),
            "unexpected error: {err:#}"
        );
    }

    #[tokio::test]
    async fn test_deploy_contracts_rejects_invalid_deployer_key() {
        let err = deploy_contracts("http://localhost:8545", "invalid-key", 1, 1, 0, 0, true)
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("Failed to parse deployer key"),
            "unexpected error: {err:#}"
        );
    }

    #[tokio::test]
    async fn test_deploy_contracts_errors_when_nonce_is_missing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method": "eth_gasPrice"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "result": "0x1",
                "id": 1
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({"method": "eth_getTransactionCount"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1
            })))
            .mount(&server)
            .await;

        let err = deploy_contracts(&server.uri(), test_key(), 1, 1, 0, 0, true)
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("Failed to get deployer nonce"),
            "unexpected error: {err:#}"
        );
    }

    #[tokio::test]
    async fn test_deploy_contracts_deploys_token_pair_and_nft() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method": "eth_gasPrice"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "result": "0x1",
                "id": 1
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({"method": "eth_getTransactionCount"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "result": "0x0",
                "id": 1
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({"method": "eth_sendRawTransaction"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "result": "0x1234",
                "id": 1
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({"method": "eth_getTransactionReceipt"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "result": {
                    "contractAddress": "0x00000000000000000000000000000000000000aa"
                },
                "id": 1
            })))
            .mount(&server)
            .await;

        let contracts = deploy_contracts(&server.uri(), test_key(), 1, 1, 1, 1, false)
            .await
            .unwrap();

        assert_eq!(contracts.tokens, vec![Address::with_last_byte(0xaa)]);
        assert_eq!(contracts.pairs, vec![Address::with_last_byte(0xaa)]);
        assert_eq!(contracts.nfts, vec![Address::with_last_byte(0xaa)]);
    }
}
