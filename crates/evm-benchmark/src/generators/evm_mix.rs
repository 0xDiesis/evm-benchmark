use crate::generators::TxGenerator;
use crate::generators::contract_deploy::EvmContracts;
use crate::types::{SignedTxWithMetadata, TransactionType};
use alloy_consensus::{SignableTransaction, TxLegacy};
use alloy_eips::eip2718::Encodable2718;
use alloy_network::{TransactionBuilder, TxSignerSync};
use alloy_primitives::{Address, Bytes, TxKind, U256};
use alloy_rpc_types::TransactionRequest;
use alloy_signer_local::PrivateKeySigner;
use rand::distributions::WeightedIndex;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};
use std::time::Instant;

// ── Method selectors ────────────────────────────────────────────────────────
// First 4 bytes of keccak256 of each method signature, matching the benchmark
// Solidity contracts deployed in genesis.

/// `transfer(address,uint256)` — ERC-20 transfer
const SEL_ERC20_TRANSFER: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];

/// `mint(address,uint256)` — ERC-20 mint
const SEL_ERC20_MINT: [u8; 4] = [0x40, 0xc1, 0x0f, 0x19];

/// `approve(address,uint256)` — ERC-20 approve
const SEL_ERC20_APPROVE: [u8; 4] = [0x09, 0x5e, 0xa7, 0xb3];

/// `mint(address)` — NFT mint (single-arg overload used by BenchmarkNFT)
const SEL_NFT_MINT: [u8; 4] = [0x6a, 0x62, 0x78, 0x42];

/// `swap(uint256,bool)` — AMM swap
const SEL_SWAP: [u8; 4] = [0x89, 0xc0, 0xfb, 0x5d];

// ── Gas limits per tx type ──────────────────────────────────────────────────

/// Gas limit for ERC-20 transfer / mint / approve calls.
const GAS_ERC20_TRANSFER: u64 = 65_000;

/// Gas limit for ERC-20 mint calls.
const GAS_ERC20_MINT: u64 = 80_000;

/// Gas limit for ERC-20 approve calls.
const GAS_ERC20_APPROVE: u64 = 60_000;

/// Gas limit for AMM swap calls.
const GAS_SWAP: u64 = 120_000;

/// Gas limit for NFT mint calls.
const GAS_NFT_MINT: u64 = 150_000;

/// Gas limit for plain ETH transfers.
const GAS_ETH_TRANSFER: u64 = 21_000;

// ── ABI encoding helpers ────────────────────────────────────────────────────

/// Encode an ABI call with a 4-byte selector and ABI-encoded arguments.
///
/// Arguments are passed as pre-encoded 32-byte words (left-padded for addresses
/// and uint256). This avoids pulling in a full ABI encoder dependency.
fn encode_call(selector: &[u8; 4], args: &[U256]) -> Bytes {
    let mut data = Vec::with_capacity(4 + args.len() * 32);
    data.extend_from_slice(selector);
    for arg in args {
        data.extend_from_slice(&arg.to_be_bytes::<32>());
    }
    Bytes::from(data)
}

/// ABI-encode an address as a `U256` (left-padded to 32 bytes).
fn address_to_u256(addr: Address) -> U256 {
    U256::from_be_slice(addr.as_slice())
}

// ── EVM transaction descriptor ──────────────────────────────────────────────

/// Describes a single EVM transaction before signing.
///
/// Stores the target contract, calldata, gas limit and transaction type so that
/// a batch signer can turn it into a [`SignedTxWithMetadata`].
#[derive(Clone, Debug)]
pub struct EvmTxDescriptor {
    /// Target contract address (`TxKind::Call`).
    pub to: Address,
    /// ABI-encoded calldata (selector + arguments).
    pub input: Bytes,
    /// Gas limit for this tx type.
    pub gas_limit: u64,
    /// Sender address (must correspond to one of the funded accounts).
    pub sender: Address,
    /// Logical transaction type for per-method analytics.
    pub method: TransactionType,
}

// ── Configuration ───────────────────────────────────────────────────────────

/// Configuration for EVM mix generator.
///
/// Each `*_pct` field specifies the relative weight of that transaction type in
/// the generated mix. The weights are normalised internally so they do not need
/// to sum to 100.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvmMixConfig {
    /// Number of ERC-20 token contracts available.
    pub token_count: u32,
    /// Number of AMM pair contracts available.
    pub pair_count: u32,
    /// Number of NFT contracts available.
    pub nft_count: u32,
    /// Zipf exponent controlling sender / contract hotspot skew.
    pub zipf_parameter: f32,
    /// Relative weight of ERC-20 mint transactions.
    pub erc20_mint_pct: f32,
    /// Relative weight of ERC-20 transfer transactions.
    pub erc20_transfer_pct: f32,
    /// Relative weight of ERC-20 approve transactions.
    pub erc20_approve_pct: f32,
    /// Relative weight of AMM swap transactions.
    pub swap_pct: f32,
    /// Relative weight of NFT mint transactions.
    pub nft_mint_pct: f32,
    /// Relative weight of plain ETH transfer transactions.
    pub eth_transfer_pct: f32,
}

impl Default for EvmMixConfig {
    fn default() -> Self {
        // Default mix uses only operations that succeed without preconditions.
        // ERC-20 transfers and swaps require pre-existing token balances / liquidity
        // which would need a separate warmup phase. Set their weights to 0.
        EvmMixConfig {
            token_count: 5,
            pair_count: 3,
            nft_count: 2,
            zipf_parameter: 1.5,
            erc20_mint_pct: 30.0,
            erc20_transfer_pct: 0.0,
            erc20_approve_pct: 15.0,
            swap_pct: 0.0,
            nft_mint_pct: 30.0,
            eth_transfer_pct: 25.0,
        }
    }
}

// ── Generator ───────────────────────────────────────────────────────────────

/// Generator for EVM transaction mix with Zipf-distributed sender and contract selection.
///
/// Produces [`EvmTxDescriptor`]s (unsigned) or fully signed
/// [`SignedTxWithMetadata`] batches ready for RPC submission.
pub struct EvmMixGenerator {
    contracts: EvmContracts,
    #[allow(dead_code)]
    config: EvmMixConfig,
    senders: Vec<Address>,
    rng: rand::rngs::SmallRng,
    sender_dist: WeightedIndex<f32>,
    contract_dist: WeightedIndex<f32>,
    tx_type_dist: WeightedIndex<f32>,
    /// Chain ID used when signing transactions.
    chain_id: u64,
}

impl EvmMixGenerator {
    /// Create a new EVM mix generator.
    ///
    /// # Errors
    ///
    /// Returns an error if the weighted distributions cannot be constructed
    /// (e.g. all weights are zero or negative).
    pub fn new(
        contracts: EvmContracts,
        config: EvmMixConfig,
        senders: Vec<Address>,
        chain_id: u64,
    ) -> anyhow::Result<Self> {
        let rng = rand::rngs::SmallRng::from_entropy();

        // Create Zipf distributions for sender and contract selection
        let sender_weights = zipf_distribution(senders.len(), config.zipf_parameter);
        let contract_weights = zipf_distribution(
            (config.token_count + config.pair_count + config.nft_count) as usize,
            config.zipf_parameter,
        );

        // Create tx type distribution based on configured percentages
        let total_pct = config.erc20_mint_pct
            + config.erc20_transfer_pct
            + config.erc20_approve_pct
            + config.swap_pct
            + config.nft_mint_pct
            + config.eth_transfer_pct;

        let tx_type_weights = vec![
            config.erc20_mint_pct / total_pct,
            config.erc20_transfer_pct / total_pct,
            config.erc20_approve_pct / total_pct,
            config.swap_pct / total_pct,
            config.nft_mint_pct / total_pct,
            config.eth_transfer_pct / total_pct,
        ];

        let sender_dist = WeightedIndex::new(&sender_weights)?;
        let contract_dist = WeightedIndex::new(&contract_weights)?;
        let tx_type_dist = WeightedIndex::new(&tx_type_weights)?;

        Ok(EvmMixGenerator {
            contracts,
            config,
            senders,
            rng,
            sender_dist,
            contract_dist,
            tx_type_dist,
            chain_id,
        })
    }

    /// Generate a batch of unsigned [`EvmTxDescriptor`]s.
    ///
    /// Each descriptor contains the target address, calldata, gas limit and
    /// logical transaction type — everything needed for a batch signer to
    /// produce [`SignedTxWithMetadata`] entries.
    pub fn generate_batch(&mut self, count: usize) -> Vec<EvmTxDescriptor> {
        (0..count).map(|_| self.next_descriptor()).collect()
    }

    /// Generate a single unsigned [`EvmTxDescriptor`].
    pub fn next_descriptor(&mut self) -> EvmTxDescriptor {
        let sender_idx = self.rng.sample(&self.sender_dist);
        let sender = self.senders[sender_idx];
        let contract_idx = self.rng.sample(&self.contract_dist);
        let tx_type_idx = self.rng.sample(&self.tx_type_dist);

        match tx_type_idx {
            0 => self.build_erc20_mint(sender),
            1 => self.build_erc20_transfer(sender, contract_idx),
            2 => self.build_erc20_approve(sender, contract_idx),
            3 => self.build_swap(sender, contract_idx),
            4 => self.build_nft_mint(sender),
            _ => Self::build_eth_transfer(sender),
        }
    }

    /// Returns the chain ID this generator was configured for.
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// Sign a batch of [`EvmTxDescriptor`]s in parallel using rayon.
    ///
    /// Each transaction is assigned a sequential nonce starting from
    /// `nonce_base`. Returns [`SignedTxWithMetadata`] ready for RPC submission.
    ///
    /// # Errors
    ///
    /// Returns an error if any individual signing operation fails.
    pub fn sign_batch(
        descriptors: &[EvmTxDescriptor],
        signer: &PrivateKeySigner,
        nonce_base: u64,
        gas_price: u128,
        chain_id: u64,
    ) -> anyhow::Result<Vec<SignedTxWithMetadata>> {
        use rayon::prelude::*;

        descriptors
            .par_iter()
            .enumerate()
            .map(|(i, desc)| {
                let nonce = nonce_base + i as u64;

                let mut tx = TxLegacy {
                    chain_id: Some(chain_id),
                    nonce,
                    gas_price,
                    gas_limit: desc.gas_limit,
                    to: TxKind::Call(desc.to),
                    value: U256::ZERO,
                    input: desc.input.clone(),
                };

                let sig = signer
                    .sign_transaction_sync(&mut tx)
                    .map_err(|e| anyhow::anyhow!("EVM tx signing failed at index {i}: {e}"))?;

                let signed = tx.into_signed(sig);
                let envelope = alloy_consensus::TxEnvelope::from(signed);
                let encoded = envelope.encoded_2718().to_vec();
                let hash = *envelope.hash();

                Ok(SignedTxWithMetadata {
                    hash,
                    encoded,
                    nonce,
                    gas_limit: desc.gas_limit,
                    sender: desc.sender,
                    submit_time: Instant::now(),
                    method: desc.method,
                })
            })
            .collect()
    }

    // ── Private builders ────────────────────────────────────────────────

    /// Build an ERC-20 `mint(address,uint256)` descriptor.
    fn build_erc20_mint(&self, sender: Address) -> EvmTxDescriptor {
        let token_addr = self.contracts.tokens[0];
        let mint_amount = U256::from(1_000_000u64); // 1M base units
        let calldata = encode_call(&SEL_ERC20_MINT, &[address_to_u256(sender), mint_amount]);

        EvmTxDescriptor {
            to: token_addr,
            input: calldata,
            gas_limit: GAS_ERC20_MINT,
            sender,
            method: TransactionType::ERC20Mint,
        }
    }

    /// Build an ERC-20 `transfer(address,uint256)` descriptor.
    fn build_erc20_transfer(&self, sender: Address, contract_idx: usize) -> EvmTxDescriptor {
        let token_addr = self.contracts.tokens[contract_idx % self.contracts.tokens.len()];
        let recipient = Address::with_last_byte((contract_idx as u8) ^ 0xFF);
        let amount = U256::from(100u64);
        let calldata = encode_call(&SEL_ERC20_TRANSFER, &[address_to_u256(recipient), amount]);

        EvmTxDescriptor {
            to: token_addr,
            input: calldata,
            gas_limit: GAS_ERC20_TRANSFER,
            sender,
            method: TransactionType::ERC20Transfer,
        }
    }

    /// Build an ERC-20 `approve(address,uint256)` descriptor.
    fn build_erc20_approve(&self, sender: Address, contract_idx: usize) -> EvmTxDescriptor {
        let token_addr = self.contracts.tokens[contract_idx % self.contracts.tokens.len()];
        let spender = if !self.contracts.pairs.is_empty() {
            self.contracts.pairs[contract_idx % self.contracts.pairs.len()]
        } else {
            Address::with_last_byte(0xAA)
        };
        let max_approval = U256::MAX;
        let calldata = encode_call(
            &SEL_ERC20_APPROVE,
            &[address_to_u256(spender), max_approval],
        );

        EvmTxDescriptor {
            to: token_addr,
            input: calldata,
            gas_limit: GAS_ERC20_APPROVE,
            sender,
            method: TransactionType::ERC20Approve,
        }
    }

    /// Build an AMM `swap(uint256,bool)` descriptor.
    fn build_swap(&self, sender: Address, contract_idx: usize) -> EvmTxDescriptor {
        let pair_addr = self.contracts.pairs[contract_idx % self.contracts.pairs.len()];
        let swap_amount = U256::from(1_000u64);
        let zero_for_one = U256::from(1u64); // true
        let calldata = encode_call(&SEL_SWAP, &[swap_amount, zero_for_one]);

        EvmTxDescriptor {
            to: pair_addr,
            input: calldata,
            gas_limit: GAS_SWAP,
            sender,
            method: TransactionType::Swap,
        }
    }

    /// Build an NFT `mint(address)` descriptor.
    fn build_nft_mint(&self, sender: Address) -> EvmTxDescriptor {
        let nft_addr = self.contracts.nfts[0];
        let calldata = encode_call(&SEL_NFT_MINT, &[address_to_u256(sender)]);

        EvmTxDescriptor {
            to: nft_addr,
            input: calldata,
            gas_limit: GAS_NFT_MINT,
            sender,
            method: TransactionType::NFTMint,
        }
    }

    /// Build a plain ETH transfer descriptor.
    fn build_eth_transfer(sender: Address) -> EvmTxDescriptor {
        let recipient = Address::with_last_byte(42);

        EvmTxDescriptor {
            to: recipient,
            input: Bytes::new(),
            gas_limit: GAS_ETH_TRANSFER,
            sender,
            method: TransactionType::ETHTransfer,
        }
    }
}

impl TxGenerator for EvmMixGenerator {
    fn next(&mut self) -> TransactionRequest {
        let desc = self.next_descriptor();

        let mut req = TransactionRequest::default()
            .with_from(desc.sender)
            .with_to(desc.to)
            .with_gas_limit(desc.gas_limit)
            .with_value(U256::ZERO);

        if !desc.input.is_empty() {
            req = req.with_input(desc.input);
        }

        req
    }
}

/// Generate a Zipf distribution with parameter `s` over `n` items.
pub fn zipf_distribution(n: usize, s: f32) -> Vec<f32> {
    let raw: Vec<f32> = (1..=n).map(|k| 1.0 / (k as f32).powf(s)).collect();
    let sum: f32 = raw.iter().sum();
    raw.iter().map(|w| w / sum).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zipf_distribution() {
        let dist = zipf_distribution(10, 1.5);
        assert_eq!(dist.len(), 10);

        // Sum should be approximately 1.0
        let sum: f32 = dist.iter().sum();
        assert!((sum - 1.0).abs() < 0.001);

        // First element should be largest
        assert!(dist[0] > dist[1]);
        assert!(dist[1] > dist[9]);
    }

    #[test]
    fn test_evm_mix_generator_produces_transactions() {
        let contracts = EvmContracts {
            tokens: vec![Address::with_last_byte(1)],
            pairs: vec![Address::with_last_byte(2)],
            nfts: vec![Address::with_last_byte(3)],
        };

        let senders = vec![Address::with_last_byte(10), Address::with_last_byte(11)];

        let config = EvmMixConfig::default();
        let mut generator =
            EvmMixGenerator::new(contracts, config, senders, 1).expect("generator init");

        // Generate a few transactions via the TxGenerator trait
        let tx1 = generator.next();
        assert!(tx1.from.is_some());

        let tx2 = generator.next();
        assert!(tx2.from.is_some());
    }

    #[test]
    fn test_evm_mix_generate_batch() {
        let contracts = EvmContracts {
            tokens: vec![Address::with_last_byte(1), Address::with_last_byte(2)],
            pairs: vec![Address::with_last_byte(10)],
            nfts: vec![Address::with_last_byte(20)],
        };
        let senders = vec![
            Address::with_last_byte(0xA0),
            Address::with_last_byte(0xA1),
            Address::with_last_byte(0xA2),
        ];

        let config = EvmMixConfig::default();
        let mut generator =
            EvmMixGenerator::new(contracts, config, senders.clone(), 1).expect("generator init");

        let batch = generator.generate_batch(50);
        assert_eq!(batch.len(), 50);

        // Every descriptor should have a sender from our list
        for desc in &batch {
            assert!(
                senders.contains(&desc.sender),
                "unexpected sender {:?}",
                desc.sender
            );
            assert!(desc.gas_limit > 0);
        }

        // At least some should have non-empty calldata (contract calls)
        let with_calldata = batch.iter().filter(|d| !d.input.is_empty()).count();
        assert!(
            with_calldata > 0,
            "expected some transactions with calldata"
        );
    }

    // ── ABI encoding tests ──────────────────────────────────────────────

    #[test]
    fn test_encode_erc20_transfer_calldata() {
        let recipient = Address::with_last_byte(0x42);
        let amount = U256::from(1000u64);
        let data = encode_call(&SEL_ERC20_TRANSFER, &[address_to_u256(recipient), amount]);

        // 4-byte selector + 2 * 32-byte args = 68 bytes
        assert_eq!(data.len(), 68);
        assert_eq!(&data[..4], &SEL_ERC20_TRANSFER);

        // Verify address is right-aligned in the 32-byte word
        assert_eq!(data[4 + 11], 0x00); // padding
        assert_eq!(data[4 + 31], 0x42); // last byte of address

        // Verify amount
        assert_eq!(data[4 + 32 + 31], 0xE8); // 1000 = 0x3E8 -> last byte
        assert_eq!(data[4 + 32 + 30], 0x03); // 0x03
    }

    #[test]
    fn test_encode_erc20_mint_calldata() {
        let to = Address::with_last_byte(0xAB);
        let amount = U256::from(1_000_000u64);
        let data = encode_call(&SEL_ERC20_MINT, &[address_to_u256(to), amount]);

        assert_eq!(data.len(), 68);
        assert_eq!(&data[..4], &SEL_ERC20_MINT);
        // Address last byte
        assert_eq!(data[4 + 31], 0xAB);
    }

    #[test]
    fn test_encode_nft_mint_calldata() {
        let recipient = Address::with_last_byte(0x99);
        let data = encode_call(&SEL_NFT_MINT, &[address_to_u256(recipient)]);

        // 4-byte selector + 1 * 32-byte arg = 36 bytes
        assert_eq!(data.len(), 36);
        assert_eq!(&data[..4], &SEL_NFT_MINT);
        assert_eq!(data[4 + 31], 0x99);
    }

    #[test]
    fn test_encode_swap_calldata() {
        let amount = U256::from(5000u64);
        let zero_for_one = U256::from(1u64); // true
        let data = encode_call(&SEL_SWAP, &[amount, zero_for_one]);

        assert_eq!(data.len(), 68);
        assert_eq!(&data[..4], &SEL_SWAP);
    }

    #[test]
    fn test_encode_approve_calldata() {
        let spender = Address::with_last_byte(0xBB);
        let max = U256::MAX;
        let data = encode_call(&SEL_ERC20_APPROVE, &[address_to_u256(spender), max]);

        assert_eq!(data.len(), 68);
        assert_eq!(&data[..4], &SEL_ERC20_APPROVE);
        // All 32 bytes of amount should be 0xFF for U256::MAX
        assert!(data[4 + 32..4 + 64].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn test_eth_transfer_has_empty_calldata() {
        let desc = EvmMixGenerator::build_eth_transfer(Address::with_last_byte(1));
        assert!(desc.input.is_empty());
        assert_eq!(desc.gas_limit, GAS_ETH_TRANSFER);
        assert_eq!(desc.method, TransactionType::ETHTransfer);
    }

    #[test]
    fn test_descriptor_transaction_types() {
        let contracts = EvmContracts {
            tokens: vec![Address::with_last_byte(1)],
            pairs: vec![Address::with_last_byte(2)],
            nfts: vec![Address::with_last_byte(3)],
        };
        let sender = Address::with_last_byte(0xAA);

        let generator =
            EvmMixGenerator::new(contracts.clone(), EvmMixConfig::default(), vec![sender], 1)
                .expect("generator init");

        // Test each builder directly
        let mint = generator.build_erc20_mint(sender);
        assert_eq!(mint.method, TransactionType::ERC20Mint);
        assert_eq!(mint.gas_limit, GAS_ERC20_MINT);
        assert_eq!(&mint.input[..4], &SEL_ERC20_MINT);

        let transfer = generator.build_erc20_transfer(sender, 0);
        assert_eq!(transfer.method, TransactionType::ERC20Transfer);
        assert_eq!(transfer.gas_limit, GAS_ERC20_TRANSFER);
        assert_eq!(&transfer.input[..4], &SEL_ERC20_TRANSFER);

        let approve = generator.build_erc20_approve(sender, 0);
        assert_eq!(approve.method, TransactionType::ERC20Approve);
        assert_eq!(approve.gas_limit, GAS_ERC20_APPROVE);
        assert_eq!(&approve.input[..4], &SEL_ERC20_APPROVE);

        let swap = generator.build_swap(sender, 0);
        assert_eq!(swap.method, TransactionType::Swap);
        assert_eq!(swap.gas_limit, GAS_SWAP);
        assert_eq!(&swap.input[..4], &SEL_SWAP);

        let nft = generator.build_nft_mint(sender);
        assert_eq!(nft.method, TransactionType::NFTMint);
        assert_eq!(nft.gas_limit, GAS_NFT_MINT);
        assert_eq!(&nft.input[..4], &SEL_NFT_MINT);
    }

    #[test]
    fn test_next_descriptor_can_select_erc20_transfer() {
        let contracts = EvmContracts {
            tokens: vec![Address::with_last_byte(1)],
            pairs: vec![Address::with_last_byte(2)],
            nfts: vec![Address::with_last_byte(3)],
        };
        let config = EvmMixConfig {
            erc20_mint_pct: 0.0,
            erc20_transfer_pct: 100.0,
            erc20_approve_pct: 0.0,
            swap_pct: 0.0,
            nft_mint_pct: 0.0,
            eth_transfer_pct: 0.0,
            ..EvmMixConfig::default()
        };
        let mut generator =
            EvmMixGenerator::new(contracts, config, vec![Address::with_last_byte(0xAA)], 1)
                .expect("generator init");

        let descriptor = generator.next_descriptor();
        assert_eq!(descriptor.method, TransactionType::ERC20Transfer);
        assert_eq!(descriptor.gas_limit, GAS_ERC20_TRANSFER);
        assert_eq!(&descriptor.input[..4], &SEL_ERC20_TRANSFER);
    }

    #[test]
    fn test_next_descriptor_can_select_swap() {
        let contracts = EvmContracts {
            tokens: vec![Address::with_last_byte(1)],
            pairs: vec![Address::with_last_byte(2)],
            nfts: vec![Address::with_last_byte(3)],
        };
        let config = EvmMixConfig {
            erc20_mint_pct: 0.0,
            erc20_transfer_pct: 0.0,
            erc20_approve_pct: 0.0,
            swap_pct: 100.0,
            nft_mint_pct: 0.0,
            eth_transfer_pct: 0.0,
            ..EvmMixConfig::default()
        };
        let mut generator =
            EvmMixGenerator::new(contracts, config, vec![Address::with_last_byte(0xAA)], 1)
                .expect("generator init");

        let descriptor = generator.next_descriptor();
        assert_eq!(descriptor.method, TransactionType::Swap);
        assert_eq!(descriptor.gas_limit, GAS_SWAP);
        assert_eq!(&descriptor.input[..4], &SEL_SWAP);
    }

    #[test]
    fn test_sign_batch_produces_valid_signed_txs() {
        let contracts = EvmContracts {
            tokens: vec![Address::with_last_byte(1)],
            pairs: vec![Address::with_last_byte(2)],
            nfts: vec![Address::with_last_byte(3)],
        };

        let key = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let signer: PrivateKeySigner = key.parse().expect("valid key");
        let sender = signer.address();
        let senders = vec![sender];

        let config = EvmMixConfig::default();
        let mut generator =
            EvmMixGenerator::new(contracts, config, senders, 1).expect("generator init");

        let descs = generator.generate_batch(10);
        let signed = EvmMixGenerator::sign_batch(&descs, &signer, 0, 1_000_000_000, 1)
            .expect("signing should succeed");

        assert_eq!(signed.len(), 10);

        // Nonces should be sequential
        for (i, tx) in signed.iter().enumerate() {
            assert_eq!(tx.nonce, i as u64);
            assert!(!tx.encoded.is_empty());
            assert_eq!(tx.sender, sender);
        }

        // At least some should be contract call types (not SimpleTransfer)
        let contract_calls = signed
            .iter()
            .filter(|t| t.method != TransactionType::SimpleTransfer)
            .count();
        assert!(
            contract_calls > 0,
            "expected contract call transaction types"
        );
    }

    #[test]
    fn test_address_to_u256_roundtrip() {
        let addr = Address::with_last_byte(0x42);
        let u = address_to_u256(addr);
        // The U256 should have the address in its lower 20 bytes
        let bytes = u.to_be_bytes::<32>();
        // First 12 bytes should be zero (padding)
        assert!(bytes[..12].iter().all(|&b| b == 0));
        // Last 20 bytes should match the address
        assert_eq!(&bytes[12..], addr.as_slice());
    }

    #[test]
    fn test_evm_mix_config_default_weights() {
        let config = EvmMixConfig::default();
        assert_eq!(config.erc20_mint_pct, 30.0);
        assert_eq!(config.erc20_transfer_pct, 0.0);
        assert_eq!(config.erc20_approve_pct, 15.0);
        assert_eq!(config.swap_pct, 0.0);
        assert_eq!(config.nft_mint_pct, 30.0);
        assert_eq!(config.eth_transfer_pct, 25.0);
    }

    #[test]
    fn test_zipf_distribution_single_item() {
        let dist = zipf_distribution(1, 1.5);
        assert_eq!(dist.len(), 1);
        assert!((dist[0] - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_zipf_distribution_uniform_when_s_is_zero() {
        let dist = zipf_distribution(5, 0.0);
        assert_eq!(dist.len(), 5);
        // With s=0 every item has weight 1/k^0 = 1, so uniform
        let expected = 1.0 / 5.0;
        for w in &dist {
            assert!(
                (w - expected).abs() < 0.001,
                "expected uniform ~{expected}, got {w}"
            );
        }
    }

    #[test]
    fn test_evm_mix_generator_chain_id() {
        let contracts = EvmContracts {
            tokens: vec![Address::with_last_byte(1)],
            pairs: vec![Address::with_last_byte(2)],
            nfts: vec![Address::with_last_byte(3)],
        };
        let senders = vec![Address::with_last_byte(10)];
        let generator = EvmMixGenerator::new(contracts, EvmMixConfig::default(), senders, 42)
            .expect("generator init");
        assert_eq!(generator.chain_id(), 42);
    }

    #[test]
    fn test_evm_mix_generator_all_zero_weights_fails() {
        let contracts = EvmContracts {
            tokens: vec![Address::with_last_byte(1)],
            pairs: vec![Address::with_last_byte(2)],
            nfts: vec![Address::with_last_byte(3)],
        };
        let senders = vec![Address::with_last_byte(10)];
        let config = EvmMixConfig {
            erc20_mint_pct: 0.0,
            erc20_transfer_pct: 0.0,
            erc20_approve_pct: 0.0,
            swap_pct: 0.0,
            nft_mint_pct: 0.0,
            eth_transfer_pct: 0.0,
            ..EvmMixConfig::default()
        };
        let result = EvmMixGenerator::new(contracts, config, senders, 1);
        assert!(result.is_err(), "all-zero weights should fail");
    }

    #[test]
    fn test_build_erc20_approve_empty_pairs_uses_fallback() {
        let contracts = EvmContracts {
            tokens: vec![Address::with_last_byte(1)],
            pairs: vec![],
            nfts: vec![Address::with_last_byte(3)],
        };
        let senders = vec![Address::with_last_byte(10)];
        let generator = EvmMixGenerator::new(contracts, EvmMixConfig::default(), senders, 1)
            .expect("generator init");

        let sender = Address::with_last_byte(10);
        let desc = generator.build_erc20_approve(sender, 0);

        // With empty pairs, the spender should fall back to 0xAA
        assert_eq!(desc.method, TransactionType::ERC20Approve);
        // The calldata encodes the spender as the first arg after the selector
        let spender_byte = desc.input[4 + 31]; // last byte of first 32-byte word
        assert_eq!(spender_byte, 0xAA, "expected fallback address 0xAA");
    }
}
