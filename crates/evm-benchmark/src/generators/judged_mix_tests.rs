use super::*;
use std::collections::{HashMap, HashSet};

fn senders() -> Vec<Address> {
    (0..SENDERS)
        .map(|i| derived_address("test-sender", i))
        .collect()
}

fn contracts() -> Contracts {
    Contracts {
        tokens: (0..TOKEN_COUNT)
            .map(|i| derived_address("test-token", i))
            .collect(),
        nfts: (0..NFT_COUNT)
            .map(|i| derived_address("test-nft", i))
            .collect(),
    }
}

fn reserve_for(skeleton: &Skeleton, actors: &Actors) -> Vec<Vec<SeededOrder>> {
    let mut next_id = 1u64;
    skeleton
        .cancels_per_market()
        .iter()
        .map(|n| {
            (0..*n)
                .map(|i| {
                    next_id += 1;
                    SeededOrder {
                        id: B256::from(U256::from(next_id).to_be_bytes::<32>()),
                        owner: actors.makers[i % actors.makers.len()],
                    }
                })
                .collect()
        })
        .collect()
}

fn plan(
    count: usize,
) -> (
    Skeleton,
    Actors,
    Vec<Vec<SeededOrder>>,
    Vec<EvmTxDescriptor>,
) {
    let skeleton = Skeleton::new(count, SEED);
    let actors = Actors::from_senders(&senders()).expect("actors");
    let reserve = reserve_for(&skeleton, &actors);
    let descs = skeleton
        .bind(&actors, &contracts(), &reserve, SEED)
        .expect("bind");
    (skeleton, actors, reserve, descs)
}

#[test]
fn every_block_of_the_schedule_holds_the_exact_proportions() {
    let skeleton = Skeleton::new(10 * SCHEDULE_BLOCK, SEED);
    for block in skeleton.slots.chunks(SCHEDULE_BLOCK) {
        let mut counts: HashMap<Kind, usize> = HashMap::new();
        for slot in block {
            *counts.entry(slot.kind).or_default() += 1;
        }
        for (kind, expected) in KIND_COUNTS {
            assert_eq!(
                counts.get(&kind).copied().unwrap_or(0),
                expected,
                "{kind:?}"
            );
        }
    }
}

#[test]
fn proportions_over_a_fixed_seed_and_an_uneven_length() {
    // 25,037 transactions: 250 whole blocks plus a partial one.
    let (_, _, _, descs) = plan(25_037);
    let mut counts: HashMap<TransactionType, usize> = HashMap::new();
    for d in &descs {
        *counts.entry(d.method).or_default() += 1;
    }
    let whole = 25_000 / SCHEDULE_BLOCK;
    for (kind, per_block) in KIND_COUNTS {
        let n = counts.get(&kind.method()).copied().unwrap_or(0);
        assert!(
            n >= per_block * whole && n <= per_block * whole + 37,
            "{kind:?}: {n}"
        );
    }
    // Order book: 15% placements + 5% cancels + 5% takers = 25%.
    let order_book: usize = [
        TransactionType::SpotPlace,
        TransactionType::SpotCancel,
        TransactionType::SpotTake,
    ]
    .iter()
    .map(|t| counts[t])
    .sum();
    assert!((6_250..=6_250 + 37).contains(&order_book));
}

#[test]
fn the_plan_is_deterministic_and_seeded() {
    let a = Skeleton::new(5_000, SEED);
    let b = Skeleton::new(5_000, SEED);
    let c = Skeleton::new(5_000, SEED + 1);
    assert_eq!(a.slots, b.slots);
    assert_ne!(a.slots, c.slots);
    let (_, _, _, x) = plan(3_000);
    let (_, _, _, y) = plan(3_000);
    assert!(
        x.iter()
            .zip(&y)
            .all(|(p, q)| p.input == q.input && p.sender == q.sender && p.to == q.to)
    );
}

#[test]
fn market_concentration_follows_the_pinned_shares() {
    let skeleton = Skeleton::new(200_000, SEED);
    let mut per_market = [0usize; MARKETS.len()];
    let mut order_book = 0;
    for slot in skeleton.slots.iter().filter(|s| s.kind.is_order_book()) {
        per_market[slot.market] += 1;
        order_book += 1;
    }
    for (m, (_, _, share)) in MARKETS.iter().enumerate() {
        let observed = per_market[m] as f64 / order_book as f64 * 100.0;
        assert!(
            (observed - f64::from(*share)).abs() < 1.0,
            "market {m}: {observed:.2}%"
        );
    }
}

#[test]
fn roles_keep_takers_apart_from_makers_and_cancels_with_owners() {
    let (_, actors, reserve, descs) = plan(20_000);
    let general: HashSet<_> = actors.general.iter().collect();
    let makers: HashSet<_> = actors.makers.iter().collect();
    let takers: HashSet<_> = actors.takers.iter().collect();
    let mut cancel_ids = Vec::new();
    for d in &descs {
        match d.method {
            TransactionType::SpotTake => assert!(takers.contains(&d.sender)),
            TransactionType::SpotPlace => assert!(makers.contains(&d.sender)),
            TransactionType::SpotCancel => {
                assert!(makers.contains(&d.sender));
                cancel_ids.push((B256::from_slice(&d.input[4..36]), d.sender));
            }
            _ => assert!(general.contains(&d.sender)),
        }
    }
    // Every cancel targets a distinct seeded order and is signed by its owner.
    let seeded: HashMap<B256, Address> =
        reserve.iter().flatten().map(|o| (o.id, o.owner)).collect();
    let distinct: HashSet<_> = cancel_ids.iter().map(|(id, _)| *id).collect();
    assert_eq!(distinct.len(), cancel_ids.len());
    for (id, sender) in cancel_ids {
        assert_eq!(seeded[&id], sender);
    }
}

#[test]
fn senders_spread_evenly_within_each_role() {
    let (_, _, _, descs) = plan(40_000);
    let mut per_sender: HashMap<Address, usize> = HashMap::new();
    for d in descs.iter().filter(|d| {
        d.method == TransactionType::ETHTransfer || d.method == TransactionType::ERC20Transfer
    }) {
        *per_sender.entry(d.sender).or_default() += 1;
    }
    let (min, max) = (
        per_sender.values().min().copied().unwrap_or(0),
        per_sender.values().max().copied().unwrap_or(0),
    );
    assert_eq!(per_sender.len(), GENERAL_SENDERS);
    assert!(max - min <= max / 2, "min {min} max {max}");
}

#[test]
fn recipients_spread_over_the_pool_with_hot_accounts() {
    let (_, actors, _, descs) = plan(40_000);
    let mut hits: HashMap<Address, usize> = HashMap::new();
    for d in descs
        .iter()
        .filter(|d| d.method == TransactionType::ETHTransfer)
    {
        *hits.entry(d.to).or_default() += 1;
    }
    let pool: HashSet<_> = actors.recipients.iter().collect();
    assert!(hits.keys().all(|a| pool.contains(a)));
    assert!(hits.len() > 1_000, "{} distinct recipients", hits.len());
    let hottest = hits.values().max().copied().unwrap_or(0);
    assert!(
        hottest > 10 * (10_000 / RECIPIENT_POOL).max(1),
        "hottest {hottest}"
    );
}

#[test]
fn approvals_always_change_the_allowance() {
    let (_, _, _, descs) = plan(10_000);
    let mut last: HashMap<(Address, Address, Address), U256> = HashMap::new();
    let mut values = HashSet::new();
    for d in descs
        .iter()
        .filter(|d| d.method == TransactionType::ERC20Approve)
    {
        assert_eq!(
            &d.input[..4],
            &spot_book::selector("approve(address,uint256)")
        );
        let spender = Address::from_slice(&d.input[16..36]);
        let value = U256::from_be_slice(&d.input[36..68]);
        assert!(values.insert(value), "approval values are unique");
        if let Some(previous) = last.insert((d.sender, d.to, spender), value) {
            assert_ne!(previous, value);
            assert!(value > previous);
        }
        assert_ne!(value, U256::MAX);
    }
    assert!(!values.is_empty());
}

#[test]
fn order_book_calls_rest_take_and_cancel_in_their_zones() {
    let (_, _, _, descs) = plan(20_000);
    let markets: HashSet<B256> = MARKETS
        .iter()
        .map(|(b, q, _)| spot_book::market_id(*b, *q))
        .collect();
    for d in descs.iter().filter(|d| d.to == spot_book::SPOT_BOOK) {
        assert_eq!(d.value, U256::ZERO);
        match d.method {
            TransactionType::SpotPlace | TransactionType::SpotTake => {
                assert_eq!(d.input.len(), 164);
                assert!(markets.contains(&B256::from_slice(&d.input[4..36])));
                let side = d.input[4 + 63];
                let price = U256::from_be_slice(&d.input[68..100]).to::<u64>();
                let lots = U256::from_be_slice(&d.input[100..132]).to::<u64>();
                if d.method == TransactionType::SpotPlace {
                    assert_eq!(lots, MAKER_LOTS);
                    assert_eq!(d.gas_limit, spot_book::GAS_SPOT_PLACE);
                    if side == 0 {
                        assert!((MID - ACTIVE_LEVELS..MID).contains(&price));
                    } else {
                        assert!((MID + 1..=MID + ACTIVE_LEVELS).contains(&price));
                    }
                } else {
                    assert_eq!(lots, TAKER_LOTS);
                    assert_eq!(d.gas_limit, spot_book::GAS_SPOT_TAKE);
                    let limit = if side == 0 {
                        MID + ACTIVE_LEVELS
                    } else {
                        MID - ACTIVE_LEVELS
                    };
                    assert_eq!(price, limit);
                    // The taker crosses every active level but never the cancel reserve.
                    assert!(limit < MID + RESERVE_OFFSET && limit > MID - RESERVE_OFFSET);
                }
                let exact =
                    spot_book::intrinsic_gas(&d.input) + spot_book::limit_order_precharge(lots);
                assert!(exact <= d.gas_limit, "declared gas covers the precharge");
            }
            TransactionType::SpotCancel => {
                assert_eq!(d.input.len(), 36);
                assert_eq!(d.gas_limit, spot_book::GAS_SPOT_CANCEL);
                assert!(spot_book::intrinsic_gas(&d.input) + 3_000 <= d.gas_limit);
            }
            other => panic!("unexpected {other:?} to the spot book"),
        }
    }
}

#[test]
fn gas_limits_are_fixed_per_type() {
    let (_, _, _, descs) = plan(5_000);
    let mut limits: HashMap<TransactionType, HashSet<u64>> = HashMap::new();
    for d in &descs {
        limits.entry(d.method).or_default().insert(d.gas_limit);
    }
    assert!(limits.values().all(|set| set.len() == 1));
    assert_eq!(limits.len(), KIND_COUNTS.len());
}

#[test]
fn judged_scale_fits_the_node_book_caps_and_keeps_two_makers_live() {
    // 2,000 TPS for 660 s with the 1.1 presign headroom.
    let skeleton = Skeleton::new(2_000 * 660 * 11 / 10, SEED);
    skeleton.check_capacity().expect("fits per-market cap");
    for side_depths in skeleton.min_active_depth() {
        for depth in side_depths {
            assert!(depth >= 2, "active depth fell to {depth}");
        }
    }
    let peak = skeleton.peak_active_orders();
    assert!(
        peak.iter().all(|p| *p <= MAX_ACTIVE_ORDERS_PER_MARKET),
        "{peak:?}"
    );
}

#[test]
fn capacity_check_refuses_a_plan_beyond_the_cap() {
    let skeleton = Skeleton::new(2_000 * 1_200 * 11 / 10, SEED);
    let err = skeleton.check_capacity().expect_err("too long a run");
    assert!(err.to_string().contains("per market"), "{err}");
}

#[test]
fn bind_refuses_a_short_reserve_and_the_wrong_sender_count() {
    let skeleton = Skeleton::new(2_000, SEED);
    let actors = Actors::from_senders(&senders()).expect("actors");
    let empty = vec![Vec::new(); MARKETS.len()];
    assert!(skeleton.bind(&actors, &contracts(), &empty, SEED).is_err());
    assert!(Actors::from_senders(&senders()[..10]).is_err());
}

#[test]
fn declared_gas_per_thousand_transactions() {
    // 25% x 21,000 + 30% x 65,000 + 10% x 60,000 + 5% x 80,000 + 5% x 150,000
    // + 15% x 115,960 + 5% x 24,288 + 5% x 151,408, per 1,000.
    assert_eq!(declared_gas_per(1_000), 68_428_800);
}
