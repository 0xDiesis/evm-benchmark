use crate::types::{SignedTxWithMetadata, TransactionType};
use alloy_consensus::{SignableTransaction, TxLegacy};
use alloy_eips::eip2718::Encodable2718;
use alloy_network::TxSignerSync;
use alloy_primitives::{Address, TxKind, U256};
use alloy_signer_local::PrivateKeySigner;
use anyhow::Result;
use rayon::prelude::*;
use std::time::Instant;

const GAS_LIMIT: u64 = 21_000;

/// Batch signer for parallel signing of multiple transactions across CPU cores.
///
/// Uses legacy (type 0) transactions with an explicit gas price. Legacy txs are not
/// subject to EIP-1559 base fee caps so they stay valid even when the base fee spikes.
pub struct BatchSigner {
    signer: PrivateKeySigner,
    account: Address,
    nonce_base: u64,
    gas_price: u128,
    chain_id: u64,
}

impl BatchSigner {
    /// Create a new batch signer with default gas price (1 gwei).
    ///
    /// Prefer [`new_with_gas_price`](Self::new_with_gas_price) for chains with
    /// higher minimum base fees (e.g. Sonic's 50 gwei).
    #[allow(dead_code)] // Used in benchmarks and tests via the lib crate
    pub fn new(signer: PrivateKeySigner, nonce_base: u64, chain_id: u64) -> Self {
        Self::new_with_gas_price(signer, nonce_base, 1_000_000_000, chain_id) // default 1 gwei
    }

    pub fn new_with_gas_price(
        signer: PrivateKeySigner,
        nonce_base: u64,
        gas_price: u128,
        chain_id: u64,
    ) -> Self {
        let account = signer.address();
        BatchSigner {
            signer,
            account,
            nonce_base,
            gas_price,
            chain_id,
        }
    }

    /// Sign a batch of transactions in parallel using rayon.
    /// Returns a Vec of SignedTxWithMetadata signed with sequential nonces starting from nonce_base.
    /// Uses legacy (type 0) transactions for maximum base-fee tolerance.
    pub fn sign_batch_parallel(
        &self,
        txs: Vec<(Address, U256)>, // (recipient, value)
    ) -> Result<Vec<SignedTxWithMetadata>> {
        let gas_price = self.gas_price;
        let chain_id = self.chain_id;
        txs.into_par_iter()
            .enumerate()
            .map(|(i, (recipient, value))| {
                let nonce = self.nonce_base + i as u64;

                // Legacy (type 0) transaction
                let mut tx = TxLegacy {
                    chain_id: Some(chain_id),
                    nonce,
                    gas_price,
                    gas_limit: GAS_LIMIT,
                    to: TxKind::Call(recipient),
                    value,
                    input: alloy_primitives::Bytes::new(),
                };

                let sig = self
                    .signer
                    .sign_transaction_sync(&mut tx)
                    .map_err(|e| anyhow::anyhow!("Signing failed at index {}: {}", i, e))?;

                let signed = tx.into_signed(sig);
                let envelope = alloy_consensus::TxEnvelope::from(signed);
                let encoded = envelope.encoded_2718().to_vec();
                let hash = *envelope.hash();

                Ok(SignedTxWithMetadata {
                    hash,
                    encoded,
                    nonce,
                    gas_limit: GAS_LIMIT,
                    sender: self.account,
                    submit_time: Instant::now(),
                    method: TransactionType::SimpleTransfer,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_batch_signer_creation() {
        let key = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let signer = PrivateKeySigner::from_str(key).expect("failed to parse signer");
        let batch_signer = BatchSigner::new(signer, 0, 1);
        assert_eq!(batch_signer.nonce_base, 0);
    }

    #[test]
    fn test_sign_batch_parallel() {
        let key = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let signer = PrivateKeySigner::from_str(key).expect("failed to parse signer");
        let batch_signer = BatchSigner::new(signer, 0, 1);

        let txs = vec![
            (Address::with_last_byte(0x42), U256::from(1u32)),
            (Address::with_last_byte(0x43), U256::from(2u32)),
            (Address::with_last_byte(0x44), U256::from(3u32)),
        ];

        let signed = batch_signer
            .sign_batch_parallel(txs)
            .expect("failed to sign batch");
        assert_eq!(signed.len(), 3);
        assert_eq!(signed[0].nonce, 0);
        assert_eq!(signed[1].nonce, 1);
        assert_eq!(signed[2].nonce, 2);
    }

    #[test]
    fn test_new_with_gas_price_fields() {
        let key = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let signer = PrivateKeySigner::from_str(key).expect("failed to parse signer");
        let expected_addr = signer.address();
        let batch_signer = BatchSigner::new_with_gas_price(signer, 42, 5_000_000_000, 19803);
        assert_eq!(batch_signer.nonce_base, 42);
        assert_eq!(batch_signer.gas_price, 5_000_000_000);
        assert_eq!(batch_signer.chain_id, 19803);
        assert_eq!(batch_signer.account, expected_addr);
    }

    #[test]
    fn test_sign_batch_parallel_empty() {
        let key = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let signer = PrivateKeySigner::from_str(key).expect("failed to parse signer");
        let batch_signer = BatchSigner::new(signer, 0, 1);

        let signed = batch_signer.sign_batch_parallel(vec![]).unwrap();
        assert!(signed.is_empty());
    }

    #[test]
    fn test_sign_batch_parallel_single_tx() {
        let key = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let signer = PrivateKeySigner::from_str(key).expect("failed to parse signer");
        let batch_signer = BatchSigner::new(signer, 10, 1);

        let txs = vec![(Address::with_last_byte(0x99), U256::from(100u32))];
        let signed = batch_signer.sign_batch_parallel(txs).unwrap();
        assert_eq!(signed.len(), 1);
        assert_eq!(signed[0].nonce, 10);
        assert_eq!(signed[0].gas_limit, GAS_LIMIT);
        assert!(!signed[0].encoded.is_empty());
        assert!(!signed[0].hash.is_zero());
    }

    #[test]
    fn test_sign_batch_parallel_unique_hashes() {
        let key = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let signer = PrivateKeySigner::from_str(key).expect("failed to parse signer");
        let batch_signer = BatchSigner::new(signer, 0, 1);

        let txs = vec![
            (Address::with_last_byte(0x01), U256::from(1u32)),
            (Address::with_last_byte(0x02), U256::from(2u32)),
            (Address::with_last_byte(0x03), U256::from(3u32)),
            (Address::with_last_byte(0x04), U256::from(4u32)),
        ];

        let signed = batch_signer.sign_batch_parallel(txs).unwrap();
        assert_eq!(signed.len(), 4);

        // All hashes must be unique
        for i in 0..signed.len() {
            for j in (i + 1)..signed.len() {
                assert_ne!(
                    signed[i].hash, signed[j].hash,
                    "tx {} and tx {} should have different hashes",
                    i, j
                );
            }
        }
    }

    #[test]
    fn test_sign_batch_parallel_preserves_sender() {
        let key = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let signer = PrivateKeySigner::from_str(key).expect("failed to parse signer");
        let expected_addr = signer.address();
        let batch_signer = BatchSigner::new(signer, 0, 1);

        let txs = vec![
            (Address::with_last_byte(0x10), U256::from(1u32)),
            (Address::with_last_byte(0x20), U256::from(2u32)),
            (Address::with_last_byte(0x30), U256::from(3u32)),
        ];

        let signed = batch_signer.sign_batch_parallel(txs).unwrap();
        for (i, stx) in signed.iter().enumerate() {
            assert_eq!(
                stx.sender, expected_addr,
                "tx {} sender should match signer address",
                i
            );
        }
    }

    #[test]
    fn test_sign_batch_parallel_custom_gas_and_chain() {
        let key = "0x0000000000000000000000000000000000000000000000000000000000000002";
        let signer = PrivateKeySigner::from_str(key).expect("failed to parse signer");
        let batch_signer = BatchSigner::new_with_gas_price(signer, 100, 50_000_000_000, 42);

        let txs = vec![(Address::with_last_byte(0xaa), U256::from(999u32))];

        let signed = batch_signer.sign_batch_parallel(txs).unwrap();
        assert_eq!(signed.len(), 1);
        assert_eq!(signed[0].nonce, 100);
        // The encoded tx should be non-empty and different from chain_id=1 txs
        assert!(!signed[0].encoded.is_empty());
        assert_eq!(signed[0].method, TransactionType::SimpleTransfer);
    }
}
