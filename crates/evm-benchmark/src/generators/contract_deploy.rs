//! Deploy benchmark contracts (ERC-20, AMM pair, NFT) for EVM workload testing.
//!
//! Contracts are compiled from `contracts/src/benchmark/` and their bytecode is
//! embedded here. Deployment happens before the benchmark starts and is NOT
//! counted toward TPS.

use alloy_primitives::{Address, Bytes, U256};
use alloy_signer_local::PrivateKeySigner;
use anyhow::Result;
use std::str::FromStr;

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
    for _ in 0..120 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
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

        // ABI-encode constructor args: (address, address)
        let mut bytecode_with_args = pair_bc_raw.clone();
        let mut t0_word = [0u8; 32];
        t0_word[12..].copy_from_slice(t0.as_slice());
        bytecode_with_args.extend_from_slice(&t0_word);
        let mut t1_word = [0u8; 32];
        t1_word[12..].copy_from_slice(t1.as_slice());
        bytecode_with_args.extend_from_slice(&t1_word);

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
}
