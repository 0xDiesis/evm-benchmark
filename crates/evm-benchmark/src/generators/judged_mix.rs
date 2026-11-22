//! The judged mixed-EVM workload for Diesis speed runs.
//!
//! Proportions are by transaction, per block of [`SCHEDULE_BLOCK`] transactions,
//! so any window of whole blocks carries the exact mix:
//! 25% native transfers, 30% ERC-20 transfers, 10% ERC-20 approvals that change
//! an allowance, 5% ERC-20 mints, 5% NFT mints and 25% order book (15% resting
//! 1-lot limit placements, 5% cancels of live resting orders, 5% 2-lot takers
//! that each fill two distinct 1-lot makers; a taker counts once).
//!
//! Everything that shapes contention is pinned here: the seed, the sender
//! population and its roles, contract counts and their skew, the recipient
//! pool and its skew, approval spenders and values, market count and
//! concentration, book layout and depth, and maker/taker sizes. A plan is built
//! in two steps: [`Skeleton`] fixes every transaction's kind and market (and so
//! how many seeded orders each market's cancels need), then [`Skeleton::bind`]
//! turns it into descriptors once setup has seeded the books.

use crate::generators::evm_mix::EvmTxDescriptor;
use crate::generators::spot_book::{self, Side};
use crate::types::TransactionType;
use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use serde::Serialize;

/// Plan seed. Changing it changes every draw below.
pub const SEED: u64 = 0x4a55_4447_4544_4d58; // "JUDGEDMX"
/// Transactions per stratified block; each block holds the exact proportions.
pub const SCHEDULE_BLOCK: usize = 100;
/// Transactions of each kind per block, in [`Kind`] order.
pub const KIND_COUNTS: [(Kind, usize); 8] = [
    (Kind::NativeTransfer, 25),
    (Kind::Erc20Transfer, 30),
    (Kind::Erc20Approve, 10),
    (Kind::Erc20Mint, 5),
    (Kind::NftMint, 5),
    (Kind::SpotPlace, 15),
    (Kind::SpotCancel, 5),
    (Kind::SpotTake, 5),
];

/// Sender population. Judged runs use exactly this many senders.
pub const SENDERS: usize = 400;
/// Senders by role, proportional to their share of the mix: general senders
/// carry native, ERC-20 and NFT traffic; makers own every resting order
/// (placements and the cancels of their seeded orders); takers only take, so a
/// taker never meets its own maker.
pub const GENERAL_SENDERS: usize = 300;
pub const MAKER_SENDERS: usize = 80;
pub const TAKER_SENDERS: usize = 20;

/// ERC-20 contracts, chosen with Zipf exponent [`CONTRACT_ZIPF`].
pub const TOKEN_COUNT: usize = 8;
/// NFT contracts, chosen with Zipf exponent [`CONTRACT_ZIPF`].
pub const NFT_COUNT: usize = 4;
pub const CONTRACT_ZIPF: f64 = 1.0;
/// Recipients of transfers and mints: a fixed pool with Zipf exponent
/// [`RECIPIENT_ZIPF`] (a few hot accounts, a long tail of cold ones).
pub const RECIPIENT_POOL: usize = 4_096;
pub const RECIPIENT_ZIPF: f64 = 0.8;
/// Approval spenders, uniform. Each approval writes `APPROVAL_BASE + plan
/// index`, so every approval changes the stored allowance.
pub const SPENDER_POOL: usize = 64;
pub const APPROVAL_BASE: u64 = 1_000;

/// Markets (base, quote) and their share of order-book traffic in percent.
/// Pricing version 0 allows one market per ordered pair of settlement tokens.
pub const MARKETS: [(Address, Address, u32); 3] = [
    (spot_book::WDS, spot_book::TEST_USD, 40),
    (spot_book::STDS, spot_book::TEST_USD, 35),
    (spot_book::STDS, spot_book::WDS, 25),
];
/// Book layout in raw price ticks (pricing version 0): resting asks rest in
/// `MID+1..=MID+ACTIVE_LEVELS`, bids in `MID-ACTIVE_LEVELS..=MID-1`. Takers
/// cross the whole active side (buy limit `MID+ACTIVE_LEVELS`, sell limit
/// `MID-ACTIVE_LEVELS`). Cancel targets rest deep, beyond every taker limit,
/// in `MID+RESERVE_OFFSET..MID+RESERVE_OFFSET+RESERVE_LEVELS` and the mirror
/// below, so no taker can fill an order a cancel needs.
pub const MID: u64 = 1_000;
pub const ACTIVE_LEVELS: u64 = 10;
pub const RESERVE_OFFSET: u64 = 51;
pub const RESERVE_LEVELS: u64 = 50;
/// Active orders seeded per side per market before the window.
pub const ACTIVE_DEPTH_PER_SIDE: usize = 64;
/// Maker and taker sizes: a 2-lot taker against 1-lot makers fills exactly two
/// distinct makers while the side has two live orders.
pub const MAKER_LOTS: u64 = 1;
pub const TAKER_LOTS: u64 = 2;
/// Node cap on active orders per market (and per user per market).
pub const MAX_ACTIVE_ORDERS_PER_MARKET: usize = 32_768;

/// Declared gas per call type: fixed, never tuned to the block.
/// Native transfer: exact intrinsic.
pub const GAS_NATIVE_TRANSFER: u64 = 21_000;
/// `BenchmarkToken.transfer` to a cold recipient whose balance goes zero to
/// nonzero (20k set + cold access), sender update, event, calldata.
pub const GAS_ERC20_TRANSFER: u64 = 65_000;
/// `BenchmarkToken.approve`: allowance slot may go zero to nonzero.
pub const GAS_ERC20_APPROVE: u64 = 60_000;
/// `BenchmarkToken.mint`: recipient balance zero to nonzero plus totalSupply.
pub const GAS_ERC20_MINT: u64 = 80_000;
/// `BenchmarkNFT.mint`: new owner slot, balance slot, totalSupply, event.
pub const GAS_NFT_MINT: u64 = 150_000;

/// One transaction kind of the judged mix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Kind {
    NativeTransfer,
    Erc20Transfer,
    Erc20Approve,
    Erc20Mint,
    NftMint,
    SpotPlace,
    SpotCancel,
    SpotTake,
}

impl Kind {
    /// Reporting type of this kind.
    pub fn method(self) -> TransactionType {
        match self {
            Kind::NativeTransfer => TransactionType::ETHTransfer,
            Kind::Erc20Transfer => TransactionType::ERC20Transfer,
            Kind::Erc20Approve => TransactionType::ERC20Approve,
            Kind::Erc20Mint => TransactionType::ERC20Mint,
            Kind::NftMint => TransactionType::NFTMint,
            Kind::SpotPlace => TransactionType::SpotPlace,
            Kind::SpotCancel => TransactionType::SpotCancel,
            Kind::SpotTake => TransactionType::SpotTake,
        }
    }

    fn is_order_book(self) -> bool {
        matches!(self, Kind::SpotPlace | Kind::SpotCancel | Kind::SpotTake)
    }
}

/// SplitMix64: a tiny, fully specified generator, so a plan is identical on
/// every platform and dependency version.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (`n > 0`).
    pub fn below(&mut self, n: u64) -> u64 {
        // Rejection sampling keeps the draw exactly uniform.
        let zone = u64::MAX - u64::MAX % n;
        loop {
            let x = self.next_u64();
            if x < zone {
                return x % n;
            }
        }
    }

    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Cumulative weights for a Zipf or explicit distribution.
#[derive(Clone, Debug)]
pub struct Cdf(Vec<f64>);

impl Cdf {
    pub fn zipf(n: usize, s: f64) -> Self {
        Self::weights((1..=n).map(|k| 1.0 / (k as f64).powf(s)))
    }

    pub fn weights(weights: impl IntoIterator<Item = f64>) -> Self {
        let mut total = 0.0;
        let mut cdf: Vec<f64> = weights
            .into_iter()
            .map(|w| {
                total += w;
                total
            })
            .collect();
        for c in &mut cdf {
            *c /= total;
        }
        Cdf(cdf)
    }

    pub fn sample(&self, rng: &mut Rng) -> usize {
        let u = rng.unit();
        self.0
            .iter()
            .position(|c| u < *c)
            .unwrap_or(self.0.len() - 1)
    }
}

/// Deterministic address `keccak256(label-i)[12..]`.
pub fn derived_address(label: &str, i: usize) -> Address {
    Address::from_slice(&keccak256(format!("{label}-{i}").as_bytes())[12..])
}

/// One planned transaction before order ids are known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub kind: Kind,
    /// Market index for order-book kinds.
    pub market: usize,
    /// Order side for placements and takers.
    pub side: Side,
}

/// A resting order seeded before the window, with its owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeededOrder {
    pub id: B256,
    pub owner: Address,
}

/// Every transaction's kind and market, fixed by the seed.
#[derive(Clone, Debug)]
pub struct Skeleton {
    pub slots: Vec<Slot>,
}

impl Skeleton {
    /// Plan `count` transactions: each block of [`SCHEDULE_BLOCK`] is a seeded
    /// shuffle of the exact per-block counts; order-book transactions draw a
    /// market by [`MARKETS`] concentration.
    pub fn new(count: usize, seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let market_cdf = Cdf::weights(MARKETS.iter().map(|m| f64::from(m.2)));
        let block: Vec<Kind> = KIND_COUNTS
            .iter()
            .flat_map(|(kind, n)| std::iter::repeat_n(*kind, *n))
            .collect();
        debug_assert_eq!(block.len(), SCHEDULE_BLOCK);
        let mut slots = Vec::with_capacity(count);
        while slots.len() < count {
            let mut shuffled = block.clone();
            for i in (1..shuffled.len()).rev() {
                let j = rng.below(i as u64 + 1) as usize;
                shuffled.swap(i, j);
            }
            for kind in shuffled {
                if slots.len() == count {
                    break;
                }
                let market = if kind.is_order_book() {
                    market_cdf.sample(&mut rng)
                } else {
                    0
                };
                let side = if matches!(kind, Kind::SpotPlace | Kind::SpotTake) && rng.below(2) == 1
                {
                    Side::Sell
                } else {
                    Side::Buy
                };
                slots.push(Slot { kind, market, side });
            }
        }
        Skeleton { slots }
    }

    /// Cancels planned in each market: the seeded orders each market needs.
    pub fn cancels_per_market(&self) -> [usize; MARKETS.len()] {
        let mut counts = [0; MARKETS.len()];
        for slot in &self.slots {
            if slot.kind == Kind::SpotCancel {
                counts[slot.market] += 1;
            }
        }
        counts
    }

    /// Fail before any setup when a market's seeded book would exceed the node's
    /// per-market cap. The book's total stays near its seeded size during a run:
    /// placements add 15%, takers remove 10% and cancels 5%.
    pub fn check_capacity(&self) -> anyhow::Result<()> {
        for (market, peak) in self.peak_active_orders().into_iter().enumerate() {
            if peak > MAX_ACTIVE_ORDERS_PER_MARKET {
                anyhow::bail!(
                    "judged mix: market {market} would reach {peak} active orders in plan order (seeded cancel \
                     targets {}, active {}), above the node's {MAX_ACTIVE_ORDERS_PER_MARKET} per market; \
                     shorten the run or lower the rate",
                    self.cancels_per_market()[market],
                    2 * ACTIVE_DEPTH_PER_SIDE
                );
            }
        }
        Ok(())
    }

    /// Smallest active depth per market and side (`[bids, asks]`) when the plan
    /// executes in order. Takers fill two distinct makers only while it stays
    /// at two or more.
    pub fn min_active_depth(&self) -> [[i64; 2]; MARKETS.len()] {
        let start = ACTIVE_DEPTH_PER_SIDE as i64;
        let mut depth = [[start; 2]; MARKETS.len()];
        let mut low = depth;
        for slot in &self.slots {
            let m = slot.market;
            match (slot.kind, slot.side) {
                (Kind::SpotPlace, side) => depth[m][side as usize] += 1,
                // A buy taker consumes asks; a sell taker consumes bids.
                (Kind::SpotTake, Side::Buy) => depth[m][1] -= TAKER_LOTS as i64,
                (Kind::SpotTake, Side::Sell) => depth[m][0] -= TAKER_LOTS as i64,
                _ => continue,
            }
            for side in 0..2 {
                low[m][side] = low[m][side].min(depth[m][side]);
            }
        }
        low
    }

    /// Largest active-order count per market when the plan executes in order:
    /// the seeded book, then +1 per placement, -2 per taker, -1 per cancel.
    pub fn peak_active_orders(&self) -> [usize; MARKETS.len()] {
        let cancels = self.cancels_per_market();
        let mut live: Vec<i64> = cancels
            .iter()
            .map(|c| (*c + 2 * ACTIVE_DEPTH_PER_SIDE) as i64)
            .collect();
        let mut peak: [usize; MARKETS.len()] = std::array::from_fn(|m| live[m] as usize);
        for slot in &self.slots {
            let delta = match slot.kind {
                Kind::SpotPlace => 1,
                Kind::SpotTake => -(TAKER_LOTS as i64),
                Kind::SpotCancel => -1,
                _ => continue,
            };
            live[slot.market] += delta;
            peak[slot.market] = peak[slot.market].max(live[slot.market].max(0) as usize);
        }
        peak
    }

    /// Descriptors for every slot. `reserve[m]` lists market `m`'s seeded
    /// cancel targets, consumed in order.
    pub fn bind(
        &self,
        actors: &Actors,
        contracts: &Contracts,
        reserve: &[Vec<SeededOrder>],
        seed: u64,
    ) -> anyhow::Result<Vec<EvmTxDescriptor>> {
        anyhow::ensure!(
            reserve.len() == MARKETS.len(),
            "judged mix: reserve for {} markets, expected {}",
            reserve.len(),
            MARKETS.len()
        );
        for (market, need) in self.cancels_per_market().into_iter().enumerate() {
            anyhow::ensure!(
                reserve[market].len() >= need,
                "judged mix: market {market} has {} seeded cancel targets, the plan cancels {need}",
                reserve[market].len()
            );
        }
        let mut rng = Rng::new(seed ^ 0x6269_6e64); // independent of the skeleton draws
        let token_cdf = Cdf::zipf(contracts.tokens.len(), CONTRACT_ZIPF);
        let nft_cdf = Cdf::zipf(contracts.nfts.len(), CONTRACT_ZIPF);
        let recipient_cdf = Cdf::zipf(RECIPIENT_POOL, RECIPIENT_ZIPF);
        let markets: Vec<B256> = MARKETS
            .iter()
            .map(|(b, q, _)| spot_book::market_id(*b, *q))
            .collect();
        let mut cursors = [0usize; 3];
        let mut next_cancel = vec![0usize; MARKETS.len()];
        let mut out = Vec::with_capacity(self.slots.len());
        for (index, slot) in self.slots.iter().enumerate() {
            let mut role = |r: usize, pool: &[Address]| {
                let sender = pool[cursors[r] % pool.len()];
                cursors[r] += 1;
                sender
            };
            let recipient = |rng: &mut Rng| actors.recipients[recipient_cdf.sample(rng)];
            let desc = match slot.kind {
                Kind::NativeTransfer => EvmTxDescriptor {
                    sender: role(0, &actors.general),
                    to: recipient(&mut rng),
                    input: Bytes::new(),
                    gas_limit: GAS_NATIVE_TRANSFER,
                    value: U256::from(1 + rng.below(1_000)),
                    method: slot.kind.method(),
                },
                Kind::Erc20Transfer => EvmTxDescriptor {
                    sender: role(0, &actors.general),
                    to: contracts.tokens[token_cdf.sample(&mut rng)],
                    input: erc20_call(
                        "transfer(address,uint256)",
                        recipient(&mut rng),
                        U256::from(1 + rng.below(1_000)),
                    ),
                    gas_limit: GAS_ERC20_TRANSFER,
                    value: U256::ZERO,
                    method: slot.kind.method(),
                },
                Kind::Erc20Approve => EvmTxDescriptor {
                    sender: role(0, &actors.general),
                    to: contracts.tokens[token_cdf.sample(&mut rng)],
                    input: erc20_call(
                        "approve(address,uint256)",
                        actors.spenders[rng.below(actors.spenders.len() as u64) as usize],
                        U256::from(APPROVAL_BASE + index as u64),
                    ),
                    gas_limit: GAS_ERC20_APPROVE,
                    value: U256::ZERO,
                    method: slot.kind.method(),
                },
                Kind::Erc20Mint => EvmTxDescriptor {
                    sender: role(0, &actors.general),
                    to: contracts.tokens[token_cdf.sample(&mut rng)],
                    input: erc20_call(
                        "mint(address,uint256)",
                        recipient(&mut rng),
                        U256::from(1 + rng.below(1_000_000)),
                    ),
                    gas_limit: GAS_ERC20_MINT,
                    value: U256::ZERO,
                    method: slot.kind.method(),
                },
                Kind::NftMint => {
                    let to = recipient(&mut rng);
                    let mut input = spot_book::selector("mint(address)").to_vec();
                    input
                        .extend_from_slice(&U256::from_be_slice(to.as_slice()).to_be_bytes::<32>());
                    EvmTxDescriptor {
                        sender: role(0, &actors.general),
                        to: contracts.nfts[nft_cdf.sample(&mut rng)],
                        input: Bytes::from(input),
                        gas_limit: GAS_NFT_MINT,
                        value: U256::ZERO,
                        method: slot.kind.method(),
                    }
                }
                Kind::SpotPlace => {
                    let side = slot.side;
                    let level = 1 + rng.below(ACTIVE_LEVELS);
                    let price = match side {
                        Side::Buy => MID - level,
                        Side::Sell => MID + level,
                    };
                    EvmTxDescriptor {
                        sender: role(1, &actors.makers),
                        to: spot_book::SPOT_BOOK,
                        input: spot_book::limit_order(
                            markets[slot.market],
                            side,
                            price,
                            MAKER_LOTS,
                        ),
                        gas_limit: spot_book::GAS_SPOT_PLACE,
                        value: U256::ZERO,
                        method: slot.kind.method(),
                    }
                }
                Kind::SpotTake => {
                    let limit = match slot.side {
                        Side::Buy => MID + ACTIVE_LEVELS,
                        Side::Sell => MID - ACTIVE_LEVELS,
                    };
                    EvmTxDescriptor {
                        sender: role(2, &actors.takers),
                        to: spot_book::SPOT_BOOK,
                        input: spot_book::limit_order(
                            markets[slot.market],
                            slot.side,
                            limit,
                            TAKER_LOTS,
                        ),
                        gas_limit: spot_book::GAS_SPOT_TAKE,
                        value: U256::ZERO,
                        method: slot.kind.method(),
                    }
                }
                Kind::SpotCancel => {
                    let order = reserve[slot.market][next_cancel[slot.market]];
                    next_cancel[slot.market] += 1;
                    EvmTxDescriptor {
                        sender: order.owner,
                        to: spot_book::SPOT_BOOK,
                        input: spot_book::cancel_order(order.id),
                        gas_limit: spot_book::GAS_SPOT_CANCEL,
                        value: U256::ZERO,
                        method: slot.kind.method(),
                    }
                }
            };
            out.push(desc);
        }
        Ok(out)
    }
}

fn erc20_call(signature: &str, address: Address, amount: U256) -> Bytes {
    let mut data = spot_book::selector(signature).to_vec();
    data.extend_from_slice(&U256::from_be_slice(address.as_slice()).to_be_bytes::<32>());
    data.extend_from_slice(&amount.to_be_bytes::<32>());
    Bytes::from(data)
}

/// Senders by role plus the fixed recipient and spender pools.
#[derive(Clone, Debug)]
pub struct Actors {
    pub general: Vec<Address>,
    pub makers: Vec<Address>,
    pub takers: Vec<Address>,
    pub recipients: Vec<Address>,
    pub spenders: Vec<Address>,
}

impl Actors {
    /// Split exactly [`SENDERS`] senders into roles, in order: general first
    /// (including the funding keys), then makers, then takers.
    pub fn from_senders(senders: &[Address]) -> anyhow::Result<Self> {
        anyhow::ensure!(
            senders.len() == SENDERS,
            "the judged mix pins {SENDERS} senders, got {}; pass --senders {SENDERS}",
            senders.len()
        );
        Ok(Actors {
            general: senders[..GENERAL_SENDERS].to_vec(),
            makers: senders[GENERAL_SENDERS..GENERAL_SENDERS + MAKER_SENDERS].to_vec(),
            takers: senders[GENERAL_SENDERS + MAKER_SENDERS..].to_vec(),
            recipients: (0..RECIPIENT_POOL)
                .map(|i| derived_address("judged-recipient", i))
                .collect(),
            spenders: (0..SPENDER_POOL)
                .map(|i| derived_address("judged-spender", i))
                .collect(),
        })
    }
}

/// Contracts the general senders call.
#[derive(Clone, Debug)]
pub struct Contracts {
    pub tokens: Vec<Address>,
    pub nfts: Vec<Address>,
}

/// Pinned parameters, recorded with every judged run.
pub fn spec_summary() -> serde_json::Value {
    serde_json::json!({
        "name": "judged",
        "seed": format!("{SEED:#x}"),
        "schedule_block": SCHEDULE_BLOCK,
        "kind_counts_per_block": KIND_COUNTS.iter().map(|(k, n)| (format!("{k:?}"), n)).collect::<std::collections::BTreeMap<_, _>>(),
        "senders": {"total": SENDERS, "general": GENERAL_SENDERS, "makers": MAKER_SENDERS, "takers": TAKER_SENDERS,
                    "selection": "round-robin within role"},
        "tokens": {"count": TOKEN_COUNT, "zipf": CONTRACT_ZIPF},
        "nfts": {"count": NFT_COUNT, "zipf": CONTRACT_ZIPF},
        "recipients": {"pool": RECIPIENT_POOL, "zipf": RECIPIENT_ZIPF},
        "approvals": {"spenders": SPENDER_POOL, "value": "1000 + plan index (always changes the allowance)"},
        "markets": MARKETS.iter().map(|(b, q, share)| serde_json::json!({
            "base": format!("{b:?}"), "quote": format!("{q:?}"), "share_pct": share,
            "id": format!("{:?}", spot_book::market_id(*b, *q))})).collect::<Vec<_>>(),
        "book": {"mid": MID, "active_levels": ACTIVE_LEVELS, "active_depth_per_side": ACTIVE_DEPTH_PER_SIDE,
                 "reserve_offset": RESERVE_OFFSET, "reserve_levels": RESERVE_LEVELS,
                 "maker_lots": MAKER_LOTS, "taker_lots": TAKER_LOTS, "taker_fills": 2},
        "gas_limits": {"ETHTransfer": GAS_NATIVE_TRANSFER, "ERC20Transfer": GAS_ERC20_TRANSFER,
                       "ERC20Approve": GAS_ERC20_APPROVE, "ERC20Mint": GAS_ERC20_MINT, "NFTMint": GAS_NFT_MINT,
                       "SpotPlace": spot_book::GAS_SPOT_PLACE, "SpotTake": spot_book::GAS_SPOT_TAKE,
                       "SpotCancel": spot_book::GAS_SPOT_CANCEL},
    })
}

/// Declared gas of `n` transactions at the judged proportions.
pub fn declared_gas_per(n: u64) -> u64 {
    let per_block: u64 = KIND_COUNTS
        .iter()
        .map(|(kind, count)| {
            let gas = match kind {
                Kind::NativeTransfer => GAS_NATIVE_TRANSFER,
                Kind::Erc20Transfer => GAS_ERC20_TRANSFER,
                Kind::Erc20Approve => GAS_ERC20_APPROVE,
                Kind::Erc20Mint => GAS_ERC20_MINT,
                Kind::NftMint => GAS_NFT_MINT,
                Kind::SpotPlace => spot_book::GAS_SPOT_PLACE,
                Kind::SpotCancel => spot_book::GAS_SPOT_CANCEL,
                Kind::SpotTake => spot_book::GAS_SPOT_TAKE,
            };
            gas * *count as u64
        })
        .sum();
    per_block * n / SCHEDULE_BLOCK as u64
}

#[cfg(test)]
#[path = "judged_mix_tests.rs"]
mod tests;
