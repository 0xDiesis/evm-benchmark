//! Setup and pre-signing for the judged mix (sustained mode).
//!
//! Before the measured load starts this funds the order-book traders, creates
//! the pinned spot markets (idempotently), gives general senders ERC-20
//! balances, seeds every book (an active ladder per side and the deep orders
//! the plan's cancels target) and records the seeded orders' ids from their
//! receipts. Setup transactions are sent in waves that respect per-account pool
//! slots, and every one must succeed. The measured pool is then signed in plan
//! order with each sender's nonces in sequence.

use crate::config::Config;
use crate::generators::evm_mix::EvmTxDescriptor;
use crate::generators::judged_mix::{
    self, ACTIVE_DEPTH_PER_SIDE, ACTIVE_LEVELS, Actors, Contracts, MAKER_LOTS, MARKETS, MID,
    RESERVE_LEVELS, RESERVE_OFFSET, SeededOrder, Skeleton,
};
use crate::generators::spot_book::{self, Side};
use crate::types::{SignedTxWithMetadata, TransactionType};
use alloy_consensus::{SignableTransaction, TxLegacy};
use alloy_eips::eip2718::Encodable2718;
use alloy_network::TxSignerSync;
use alloy_primitives::{Address, B256, Bytes, TxKind, U256, address};
use alloy_signer_local::PrivateKeySigner;
use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::str::FromStr;
use std::time::{Duration, Instant};

/// Development governance owner and TEST-USD / stDS holder in `diesis-dev`
/// genesis (private key 0x01, the first bench key).
pub const DEV_GOVERNANCE: Address = address!("7E5F4552091A69125d5DfCb7b8C2659029395Bdf");
/// Per-trader settlement deposits; orders lock at most a few thousand raw units.
const DEPOSIT_WDS: u64 = 1_000_000_000_000_000; // 0.001 DS
const DEPOSIT_STDS: u64 = 1_000_000_000_000_000;
const DEPOSIT_TEST_USD: u64 = 1_000_000_000; // 1,000 TEST-USD
/// ERC-20 balance minted to each general sender per token.
const ERC20_HOLDING: u128 = 1_000_000_000_000_000_000_000_000;
/// Setup gas: generous fixed limits; setup is not measured.
const SETUP_GAS: u64 = 500_000;
/// Setup transactions in flight per account (the e2e pool allows 256).
const SETUP_INFLIGHT_PER_SENDER: usize = 64;
const SETUP_RECEIPT_TIMEOUT: Duration = Duration::from_secs(180);

/// One transaction to sign: who sends it and what it does.
#[derive(Clone, Debug)]
pub struct Planned {
    pub desc: EvmTxDescriptor,
}

/// Build the judged pool for `total` transactions. Returns the signed pool in
/// plan order and the run record written next to the report.
pub async fn prepare(
    config: &Config,
    client: &reqwest::Client,
    sender_keys: &[String],
    gas_price: u128,
    total: usize,
) -> Result<(Vec<SignedTxWithMetadata>, Value)> {
    let started = Instant::now();
    let signers: Vec<PrivateKeySigner> = sender_keys
        .iter()
        .map(|k| PrivateKeySigner::from_str(k).context("parse sender key"))
        .collect::<Result<_>>()?;
    let addresses: Vec<Address> = signers.iter().map(|s| s.address()).collect();
    anyhow::ensure!(
        addresses.first() == Some(&DEV_GOVERNANCE),
        "the judged mix needs the development governance key 0x01 first in BENCH_KEY (it creates markets and holds TEST-USD/stDS)"
    );
    let actors = Actors::from_senders(&addresses)?;
    anyhow::ensure!(
        config.evm_tokens.len() >= judged_mix::TOKEN_COUNT
            && config.evm_nfts.len() >= judged_mix::NFT_COUNT,
        "the judged mix needs {} ERC-20 and {} NFT contracts (deploy with --fund), got {} and {}",
        judged_mix::TOKEN_COUNT,
        judged_mix::NFT_COUNT,
        config.evm_tokens.len(),
        config.evm_nfts.len()
    );
    let contracts = Contracts {
        tokens: config.evm_tokens[..judged_mix::TOKEN_COUNT].to_vec(),
        nfts: config.evm_nfts[..judged_mix::NFT_COUNT].to_vec(),
    };
    let skeleton = Skeleton::new(total, judged_mix::SEED);
    skeleton.check_capacity()?;
    let cancels = skeleton.cancels_per_market();

    let keyring: HashMap<Address, PrivateKeySigner> = addresses
        .iter()
        .copied()
        .zip(signers.iter().cloned())
        .collect();
    let mut sender = SetupSender::new(client.clone(), config, keyring.clone(), gas_price);

    let created = create_markets(&mut sender, client, config.rpc.as_str()).await?;
    sender
        .run(
            "fund traders and mint holdings",
            funding_batch(&actors, &contracts),
        )
        .await?;
    sender
        .run("trader deposits", deposit_batch(&actors))
        .await?;
    let (seeds, reserve_slots) = seed_batch(&actors, &cancels);
    let receipts = sender.run("seed books", seeds).await?;
    let reserve = collect_reserve(&reserve_slots, &receipts)?;

    let descs = skeleton.bind(&actors, &contracts, &reserve, judged_mix::SEED)?;
    let nonces = sender.pending_nonces(&addresses).await?;
    let pool = sign_in_plan_order(&descs, &keyring, &nonces, gas_price, config.chain_id)?;

    let mut planned: HashMap<String, usize> = HashMap::new();
    for d in &descs {
        *planned.entry(format!("{:?}", d.method)).or_default() += 1;
    }
    let record = json!({
        "mix": judged_mix::spec_summary(),
        "planned_transactions": total,
        "planned_by_type": planned,
        "declared_gas_per_1000": judged_mix::declared_gas_per(1_000),
        "markets_created": created,
        "seeded": {
            "cancel_targets_per_market": cancels,
            "active_per_side_per_market": ACTIVE_DEPTH_PER_SIDE,
            "peak_active_orders_plan_order": skeleton.peak_active_orders(),
            "min_active_depth_plan_order": skeleton.min_active_depth(),
        },
        "setup_transactions": sender.sent,
        "setup_seconds": started.elapsed().as_secs_f64(),
    });
    Ok((pool, record))
}

/// Create each pinned market unless it already exists (a reused chain).
async fn create_markets(
    sender: &mut SetupSender,
    client: &reqwest::Client,
    rpc: &str,
) -> Result<Vec<String>> {
    let mut batch = Vec::new();
    let mut created = Vec::new();
    for (base, quote, _) in MARKETS {
        let data = spot_book::create_spot_market(base, quote, 1, 1);
        let probe = rpc_call(
            client,
            rpc,
            "eth_call",
            json!([{"from": format!("{DEV_GOVERNANCE:?}"), "to": format!("{:?}", spot_book::MARKETS),
                    "data": format!("{data}"), "gas": format!("{SETUP_GAS:#x}")}, "latest"]),
        )
        .await;
        match probe {
            Ok(_) => {
                created.push(format!("{:?}", spot_book::market_id(base, quote)));
                batch.push(call(DEV_GOVERNANCE, spot_book::MARKETS, data, U256::ZERO));
            }
            Err(e) if e.to_string().contains("already exists") => {}
            Err(e) => return Err(e.context("createSpotMarket preflight")),
        }
    }
    sender.run("create markets", batch).await?;
    Ok(created)
}

fn call(sender: Address, to: Address, input: Bytes, value: U256) -> Planned {
    Planned {
        desc: EvmTxDescriptor {
            to,
            input,
            gas_limit: SETUP_GAS,
            value,
            sender,
            method: TransactionType::SimpleTransfer,
        },
    }
}

/// TEST-USD and stDS from the governance key to every order-book trader, and
/// each general sender's own mint of every token.
fn funding_batch(actors: &Actors, contracts: &Contracts) -> Vec<Planned> {
    let mut batch = Vec::new();
    for trader in actors.makers.iter().chain(&actors.takers) {
        for (token, amount) in [
            (spot_book::TEST_USD, DEPOSIT_TEST_USD),
            (spot_book::STDS, DEPOSIT_STDS),
        ] {
            batch.push(call(
                DEV_GOVERNANCE,
                token,
                spot_book::erc20_transfer(*trader, U256::from(amount)),
                U256::ZERO,
            ));
        }
    }
    for holder in &actors.general {
        for token in &contracts.tokens {
            let mut data = spot_book::selector("mint(address,uint256)").to_vec();
            data.extend_from_slice(&U256::from_be_slice(holder.as_slice()).to_be_bytes::<32>());
            data.extend_from_slice(&U256::from(ERC20_HOLDING).to_be_bytes::<32>());
            batch.push(call(*holder, *token, Bytes::from(data), U256::ZERO));
        }
    }
    batch
}

/// Each trader wraps DS and deposits WDS, stDS and TEST-USD into settlement.
fn deposit_batch(actors: &Actors) -> Vec<Planned> {
    let mut batch = Vec::new();
    for trader in actors.makers.iter().chain(&actors.takers) {
        batch.push(call(
            *trader,
            spot_book::WDS,
            spot_book::wrap_deposit(),
            U256::from(DEPOSIT_WDS),
        ));
        for (token, amount) in [
            (spot_book::WDS, DEPOSIT_WDS),
            (spot_book::STDS, DEPOSIT_STDS),
            (spot_book::TEST_USD, DEPOSIT_TEST_USD),
        ] {
            batch.push(call(
                *trader,
                spot_book::SETTLEMENT,
                spot_book::settlement_deposit(token, U256::from(amount)),
                U256::ZERO,
            ));
        }
    }
    batch
}

/// The seeded books: per market an active ladder of [`ACTIVE_DEPTH_PER_SIDE`]
/// orders per side over the active levels, then the deep cancel targets,
/// alternating sides over the reserve levels. Owners rotate over the makers.
/// Returns the batch and, for each reserve order, `(market, batch index)`.
pub fn seed_batch(
    actors: &Actors,
    cancels: &[usize; MARKETS.len()],
) -> (Vec<Planned>, Vec<(usize, usize)>) {
    let mut batch = Vec::new();
    let mut reserve = Vec::new();
    let mut owner = 0usize;
    let mut next_owner = || {
        let maker = actors.makers[owner % actors.makers.len()];
        owner += 1;
        maker
    };
    for (m, (base, quote, _)) in MARKETS.iter().enumerate() {
        let market = spot_book::market_id(*base, *quote);
        for i in 0..ACTIVE_DEPTH_PER_SIDE as u64 {
            let level = 1 + i % ACTIVE_LEVELS;
            for (side, price) in [(Side::Buy, MID - level), (Side::Sell, MID + level)] {
                let order = spot_book::limit_order(market, side, price, MAKER_LOTS);
                batch.push(seed_call(next_owner(), order));
            }
        }
        for i in 0..cancels[m] as u64 {
            let level = RESERVE_OFFSET + (i / 2) % RESERVE_LEVELS;
            let (side, price) = if i % 2 == 0 {
                (Side::Buy, MID - level)
            } else {
                (Side::Sell, MID + level)
            };
            reserve.push((m, batch.len()));
            let order = spot_book::limit_order(market, side, price, MAKER_LOTS);
            batch.push(seed_call(next_owner(), order));
        }
    }
    (batch, reserve)
}

fn seed_call(owner: Address, order: Bytes) -> Planned {
    Planned {
        desc: EvmTxDescriptor {
            to: spot_book::SPOT_BOOK,
            input: order,
            gas_limit: spot_book::GAS_SPOT_PLACE,
            value: U256::ZERO,
            sender: owner,
            method: TransactionType::SpotPlace,
        },
    }
}

/// Seeded cancel targets per market, from the seed receipts' `OrderPlaced` logs.
pub fn collect_reserve(
    slots: &[(usize, usize)],
    receipts: &[SetupReceipt],
) -> Result<Vec<Vec<SeededOrder>>> {
    let mut reserve = vec![Vec::new(); MARKETS.len()];
    for (market, index) in slots {
        let receipt = &receipts[*index];
        let id = receipt
            .order_id
            .ok_or_else(|| anyhow::anyhow!("seeded order {index} has no OrderPlaced log"))?;
        reserve[*market].push(SeededOrder {
            id,
            owner: receipt.sender,
        });
    }
    Ok(reserve)
}

/// Sign descriptors in plan order, each sender's transactions taking its
/// nonces in sequence from `nonces`.
pub fn sign_in_plan_order(
    descs: &[EvmTxDescriptor],
    keyring: &HashMap<Address, PrivateKeySigner>,
    nonces: &HashMap<Address, u64>,
    gas_price: u128,
    chain_id: u64,
) -> Result<Vec<SignedTxWithMetadata>> {
    use rayon::prelude::*;
    let mut next = nonces.clone();
    let assigned: Vec<u64> = descs
        .iter()
        .map(|d| {
            let n = next.entry(d.sender).or_insert(0);
            let nonce = *n;
            *n += 1;
            nonce
        })
        .collect();
    descs
        .par_iter()
        .zip(assigned.par_iter())
        .map(|(desc, nonce)| {
            let signer = keyring
                .get(&desc.sender)
                .ok_or_else(|| anyhow::anyhow!("no key for sender {:?}", desc.sender))?;
            let mut tx = TxLegacy {
                chain_id: Some(chain_id),
                nonce: *nonce,
                gas_price,
                gas_limit: desc.gas_limit,
                to: TxKind::Call(desc.to),
                value: desc.value,
                input: desc.input.clone(),
            };
            let sig = signer.sign_transaction_sync(&mut tx)?;
            let envelope = alloy_consensus::TxEnvelope::from(tx.into_signed(sig));
            Ok(SignedTxWithMetadata {
                hash: *envelope.hash(),
                encoded: envelope.encoded_2718().to_vec(),
                nonce: *nonce,
                gas_limit: desc.gas_limit,
                sender: desc.sender,
                submit_time: Instant::now(),
                method: desc.method,
            })
        })
        .collect()
}

/// Receipt facts setup needs.
#[derive(Clone, Debug)]
pub struct SetupReceipt {
    pub sender: Address,
    pub order_id: Option<B256>,
}

/// Sends setup batches in waves and requires every receipt to succeed.
struct SetupSender {
    client: reqwest::Client,
    rpc: String,
    chain_id: u64,
    gas_price: u128,
    keyring: HashMap<Address, PrivateKeySigner>,
    sent: usize,
    quiet: bool,
}

impl SetupSender {
    fn new(
        client: reqwest::Client,
        config: &Config,
        keyring: HashMap<Address, PrivateKeySigner>,
        gas_price: u128,
    ) -> Self {
        SetupSender {
            client,
            rpc: config.rpc.to_string(),
            chain_id: config.chain_id,
            gas_price,
            keyring,
            sent: 0,
            quiet: config.quiet,
        }
    }

    async fn pending_nonces(&self, senders: &[Address]) -> Result<HashMap<Address, u64>> {
        let (client, rpc) = (&self.client, self.rpc.as_str());
        let results: Vec<Result<(Address, u64)>> = stream::iter(senders.iter().copied())
            .map(|a| async move {
                let v = rpc_call(
                    client,
                    rpc,
                    "eth_getTransactionCount",
                    json!([format!("{a:?}"), "pending"]),
                )
                .await?;
                Ok((a, hex_u64(&v)?))
            })
            .buffer_unordered(64)
            .collect()
            .await;
        results.into_iter().collect()
    }

    /// Sign and send `batch`, at most [`SETUP_INFLIGHT_PER_SENDER`] per account
    /// per wave, and return one receipt per transaction in batch order.
    async fn run(&mut self, label: &str, batch: Vec<Planned>) -> Result<Vec<SetupReceipt>> {
        if batch.is_empty() {
            return Ok(Vec::new());
        }
        let started = Instant::now();
        let senders: Vec<Address> = {
            let mut s: Vec<Address> = batch.iter().map(|p| p.desc.sender).collect();
            s.sort();
            s.dedup();
            s
        };
        let nonces = self.pending_nonces(&senders).await?;
        let descs: Vec<EvmTxDescriptor> = batch.iter().map(|p| p.desc.clone()).collect();
        let signed = sign_in_plan_order(
            &descs,
            &self.keyring,
            &nonces,
            self.gas_price,
            self.chain_id,
        )?;
        // Waves: the k-th transaction of each sender goes in wave k / inflight.
        let mut seen: HashMap<Address, usize> = HashMap::new();
        let mut waves: Vec<Vec<usize>> = Vec::new();
        for (i, tx) in signed.iter().enumerate() {
            let k = seen.entry(tx.sender).or_insert(0);
            let wave = *k / SETUP_INFLIGHT_PER_SENDER;
            *k += 1;
            if waves.len() <= wave {
                waves.resize_with(wave + 1, Vec::new);
            }
            waves[wave].push(i);
        }
        let mut receipts: Vec<Option<SetupReceipt>> = vec![None; signed.len()];
        let (client, rpc) = (&self.client, self.rpc.as_str());
        for wave in waves {
            let sends: Vec<Result<()>> = stream::iter(wave.iter().copied())
                .map(|i| {
                    let raw = format!("0x{}", hex::encode(&signed[i].encoded));
                    async move {
                        rpc_call(client, rpc, "eth_sendRawTransaction", json!([raw]))
                            .await
                            .map(|_| ())
                            .with_context(|| format!("{label}: send {i}"))
                    }
                })
                .buffer_unordered(64)
                .collect()
                .await;
            sends.into_iter().collect::<Result<Vec<()>>>()?;
            let deadline = Instant::now() + SETUP_RECEIPT_TIMEOUT;
            let mut waiting = wave;
            while !waiting.is_empty() {
                anyhow::ensure!(
                    Instant::now() < deadline,
                    "{label}: {} receipts missing after {}s",
                    waiting.len(),
                    SETUP_RECEIPT_TIMEOUT.as_secs()
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
                let polled: Vec<(usize, Result<Option<Value>>)> =
                    stream::iter(waiting.iter().copied())
                        .map(|i| {
                            let hash = format!("{:?}", signed[i].hash);
                            async move {
                                let r = rpc_call(
                                    client,
                                    rpc,
                                    "eth_getTransactionReceipt",
                                    json!([hash]),
                                )
                                .await
                                .map(|v| (!v.is_null()).then_some(v));
                                (i, r)
                            }
                        })
                        .buffer_unordered(128)
                        .collect()
                        .await;
                let mut still = Vec::new();
                for (i, result) in polled {
                    match result? {
                        None => still.push(i),
                        Some(receipt) => {
                            let status = receipt.get("status").and_then(Value::as_str);
                            anyhow::ensure!(
                                status == Some("0x1"),
                                "{label}: setup transaction {i} ({:?} from {:?}) reverted",
                                descs[i].to,
                                descs[i].sender
                            );
                            receipts[i] = Some(SetupReceipt {
                                sender: descs[i].sender,
                                order_id: order_id_from_receipt(&receipt),
                            });
                        }
                    }
                }
                waiting = still;
            }
        }
        self.sent += signed.len();
        if !self.quiet {
            println!(
                "judged setup: {label}: {} transactions in {:.1}s",
                signed.len(),
                started.elapsed().as_secs_f64()
            );
        }
        Ok(receipts
            .into_iter()
            .map(|r| r.expect("every receipt read"))
            .collect())
    }
}

/// The `OrderPlaced` order id in a receipt, if any.
pub fn order_id_from_receipt(receipt: &Value) -> Option<B256> {
    receipt.get("logs")?.as_array()?.iter().find_map(|log| {
        let address = Address::from_str(log.get("address")?.as_str()?).ok()?;
        let topics: Vec<B256> = log
            .get("topics")?
            .as_array()?
            .iter()
            .filter_map(|t| B256::from_str(t.as_str()?).ok())
            .collect();
        spot_book::order_placed_id(address, &topics)
    })
}

async fn rpc_call(
    client: &reqwest::Client,
    url: &str,
    method: &str,
    params: Value,
) -> Result<Value> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let response: Value = client.post(url).json(&body).send().await?.json().await?;
    if let Some(error) = response.get("error") {
        anyhow::bail!("{method}: {error}");
    }
    Ok(response.get("result").cloned().unwrap_or(Value::Null))
}

fn hex_u64(v: &Value) -> Result<u64> {
    let s = v
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("expected hex string, got {v}"))?;
    Ok(u64::from_str_radix(s.trim_start_matches("0x"), 16)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generators::judged_mix::{SENDERS, derived_address};

    fn actors() -> Actors {
        let senders: Vec<Address> = (0..SENDERS).map(|i| derived_address("s", i)).collect();
        Actors::from_senders(&senders).expect("actors")
    }

    #[test]
    fn seeded_books_rest_active_ladders_and_deep_cancel_targets() {
        let actors = actors();
        let cancels = [7, 5, 3];
        let (batch, reserve) = seed_batch(&actors, &cancels);
        assert_eq!(batch.len(), 3 * 2 * ACTIVE_DEPTH_PER_SIDE + 15);
        assert_eq!(reserve.len(), 15);
        let makers: std::collections::HashSet<_> = actors.makers.iter().collect();
        for (i, p) in batch.iter().enumerate() {
            assert!(makers.contains(&p.desc.sender));
            assert_eq!(p.desc.to, spot_book::SPOT_BOOK);
            let side = p.desc.input[4 + 63];
            let price = U256::from_be_slice(&p.desc.input[68..100]).to::<u64>();
            let deep = reserve.iter().any(|(_, idx)| *idx == i);
            let distance = price.abs_diff(MID);
            if deep {
                assert!((RESERVE_OFFSET..RESERVE_OFFSET + RESERVE_LEVELS).contains(&distance));
            } else {
                assert!((1..=ACTIVE_LEVELS).contains(&distance));
            }
            // Bids below the mid, asks above: the seeded book never crosses.
            assert_eq!(side == 0, price < MID);
        }
        let per_market: Vec<usize> = (0..3)
            .map(|m| reserve.iter().filter(|(market, _)| *market == m).count())
            .collect();
        assert_eq!(per_market, vec![7, 5, 3]);
    }

    #[test]
    fn reserve_ids_come_from_order_placed_logs() {
        let receipt = json!({"status": "0x1", "logs": [
            {"address": format!("{:?}", spot_book::SPOT_BOOK),
             "topics": [format!("{:?}", spot_book::order_placed_topic()),
                        format!("{:?}", B256::with_last_byte(42)),
                        format!("{:?}", B256::ZERO), format!("{:?}", B256::ZERO)]}]});
        assert_eq!(
            order_id_from_receipt(&receipt),
            Some(B256::with_last_byte(42))
        );
        let owner = Address::with_last_byte(5);
        let receipts = vec![SetupReceipt {
            sender: owner,
            order_id: order_id_from_receipt(&receipt),
        }];
        let reserve = collect_reserve(&[(1, 0)], &receipts).expect("reserve");
        assert_eq!(
            reserve[1],
            vec![SeededOrder {
                id: B256::with_last_byte(42),
                owner
            }]
        );
        let missing = vec![SetupReceipt {
            sender: owner,
            order_id: None,
        }];
        assert!(collect_reserve(&[(0, 0)], &missing).is_err());
    }

    #[test]
    fn signing_keeps_plan_order_and_per_sender_nonce_sequence() {
        let a = PrivateKeySigner::from_str(&format!("0x{:064x}", 11)).expect("key");
        let b = PrivateKeySigner::from_str(&format!("0x{:064x}", 12)).expect("key");
        let keyring: HashMap<Address, PrivateKeySigner> =
            [(a.address(), a.clone()), (b.address(), b.clone())]
                .into_iter()
                .collect();
        let nonces: HashMap<Address, u64> =
            [(a.address(), 5), (b.address(), 0)].into_iter().collect();
        let descs: Vec<EvmTxDescriptor> = [
            a.address(),
            b.address(),
            a.address(),
            a.address(),
            b.address(),
        ]
        .into_iter()
        .map(|sender| call(sender, Address::with_last_byte(1), Bytes::new(), U256::ZERO).desc)
        .collect();
        let signed = sign_in_plan_order(&descs, &keyring, &nonces, 1, 19803).expect("sign");
        let seq: Vec<(Address, u64)> = signed.iter().map(|t| (t.sender, t.nonce)).collect();
        assert_eq!(
            seq,
            vec![
                (a.address(), 5),
                (b.address(), 0),
                (a.address(), 6),
                (a.address(), 7),
                (b.address(), 1)
            ]
        );
    }

    #[test]
    fn setup_batches_fund_and_deposit_every_order_book_trader() {
        let actors = actors();
        let contracts = Contracts {
            tokens: (0..judged_mix::TOKEN_COUNT)
                .map(|i| derived_address("t", i))
                .collect(),
            nfts: vec![],
        };
        let funding = funding_batch(&actors, &contracts);
        let traders = actors.makers.len() + actors.takers.len();
        assert_eq!(
            funding.len(),
            traders * 2 + actors.general.len() * judged_mix::TOKEN_COUNT
        );
        assert!(
            funding[..traders * 2]
                .iter()
                .all(|p| p.desc.sender == DEV_GOVERNANCE)
        );
        let deposits = deposit_batch(&actors);
        assert_eq!(deposits.len(), traders * 4);
        assert_eq!(deposits[0].desc.value, U256::from(DEPOSIT_WDS));
    }
}
