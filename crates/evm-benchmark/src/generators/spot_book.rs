//! Direct EVM calls to the Diesis exchange precompiles used by the judged mix.
//!
//! Every order-book transaction is a plain EVM call from the trader's account to
//! `DiesisSpotBook`; a crossing limit order matches inside that call. Addresses,
//! selectors, layouts and gas rules mirror the node (`crates/evm/src/precompiles/
//! exchange/` and `crates/exchange-wire/src/gas.rs`); the unit tests pin the
//! values a live `diesis-dev` fleet accepted.

use alloy_primitives::{Address, B256, Bytes, U256, address, keccak256};

/// `DiesisSpotBook` precompile.
pub const SPOT_BOOK: Address = address!("D1E515000000000000000000000000000000590D");
/// `DiesisMarkets` precompile (market creation is governance-only).
pub const MARKETS: Address = address!("D1E515000000000000000000000000000000B00C");
/// `DiesisSettlement` precompile (trader deposits and collateral).
pub const SETTLEMENT: Address = address!("D1E5150000000000000000000000000000005E71");
/// Wrapped DS: `deposit()` with value mints WDS 1:1.
pub const WDS: Address = address!("D1E51500000000000000000000000000000000D5");
/// Liquid-staked DS (ERC-20; development genesis allocates it to the governance key).
pub const STDS: Address = address!("D1E51500000000000000000000000000000AC1D5");
/// Development-only TEST-USD (ERC-20, fixed genesis supply, no mint).
pub const TEST_USD: Address = address!("D1E5150000000000000000000000000000000040");

/// Order side as the precompile encodes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Buy = 0,
    Sell = 1,
}

/// Four-byte selector of a Solidity signature.
pub fn selector(signature: &str) -> [u8; 4] {
    let hash = keccak256(signature.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
}

fn call(signature: &str, words: &[U256]) -> Bytes {
    let mut data = Vec::with_capacity(4 + 32 * words.len());
    data.extend_from_slice(&selector(signature));
    for word in words {
        data.extend_from_slice(&word.to_be_bytes::<32>());
    }
    Bytes::from(data)
}

fn word(address: Address) -> U256 {
    U256::from_be_slice(address.as_slice())
}

/// Spot market id for pricing version 0: `keccak256(base ++ quote ++ 0x00)`.
pub fn market_id(base: Address, quote: Address) -> B256 {
    let mut buf = [0u8; 41];
    buf[..20].copy_from_slice(base.as_slice());
    buf[20..40].copy_from_slice(quote.as_slice());
    keccak256(buf)
}

/// `submitLimitOrder(bytes32 marketId, uint8 side, uint256 priceTicks, uint256 amountLots, uint8 flags)`.
pub fn limit_order(market: B256, side: Side, price_ticks: u64, lots: u64) -> Bytes {
    call(
        "submitLimitOrder(bytes32,uint8,uint256,uint256,uint8)",
        &[
            U256::from_be_bytes(market.0),
            U256::from(side as u8),
            U256::from(price_ticks),
            U256::from(lots),
            U256::ZERO,
        ],
    )
}

/// `cancelOrder(bytes32 orderId)`.
pub fn cancel_order(order_id: B256) -> Bytes {
    call("cancelOrder(bytes32)", &[U256::from_be_bytes(order_id.0)])
}

/// `createSpotMarket(address base, address quote, uint64 tickSize, uint64 lotSize, uint256 bond)`.
pub fn create_spot_market(base: Address, quote: Address, tick: u64, lot: u64) -> Bytes {
    call(
        "createSpotMarket(address,address,uint64,uint64,uint256)",
        &[
            word(base),
            word(quote),
            U256::from(tick),
            U256::from(lot),
            U256::ZERO,
        ],
    )
}

/// Settlement `deposit(address token, uint256 amount)`.
pub fn settlement_deposit(token: Address, amount: U256) -> Bytes {
    call("deposit(address,uint256)", &[word(token), amount])
}

/// WDS `deposit()` (send the native amount as value).
pub fn wrap_deposit() -> Bytes {
    call("deposit()", &[])
}

/// ERC-20 `transfer(address to, uint256 amount)`.
pub fn erc20_transfer(to: Address, amount: U256) -> Bytes {
    call("transfer(address,uint256)", &[word(to), amount])
}

/// `OrderPlaced(bytes32 orderId, bytes32 marketId, address owner, uint8, uint256, uint256, uint8)`.
pub fn order_placed_topic() -> B256 {
    keccak256("OrderPlaced(bytes32,bytes32,address,uint8,uint256,uint256,uint8)")
}

// ── Gas ────────────────────────────────────────────────────────────────────
// Node rules (`exchange-wire/src/gas.rs`): a limit order must carry its
// worst-case matching precharge for the requested lots before any mutation and
// is charged actual work; `cancelOrder` charges a flat base.

const GAS_LIMIT_ORDER: u64 = 8_000;
const GAS_CANCEL_ORDER: u64 = 3_000;
const GAS_PER_FILL: u64 = 1_500;
const GAS_PER_CHANGED_LEVEL: u64 = 500;
const GAS_PER_RADIX_NODE: u64 = 50;
const GAS_PER_MATCH_STORAGE_OP: u64 = 200;
const GAS_PER_MATCH_LOG_BYTE: u64 = 2;
const MAX_ACTIVE_ORDERS_PER_MARKET: u64 = 32_768;
const TERMINAL_REMAINDER_CANCEL_STORAGE_OPS: u64 = 40;
/// Four topics: `ExchangeL3MutationV2` and `ExchangeTradeFillV2` carry 4 topics + 5 words.
const V2_LOG_BYTES: u64 = 4 * 32 + 5 * 32;
/// `OrderFillFeeV1`: 4 topics + 3 words.
const FILL_FEE_LOG_BYTES: u64 = 4 * 32 + 3 * 32;
const ORDER_PLACED_LOG_BYTES: u64 = 256;
const ORDER_CANCELLED_LOG_BYTES: u64 = 256;

/// Node `order_precharge(GAS_LIMIT_ORDER, lots)`: the gas a limit order must
/// have available before the precompile mutates anything.
pub const fn limit_order_precharge(lots: u64) -> u64 {
    let fills = if lots < MAX_ACTIVE_ORDERS_PER_MARKET {
        lots
    } else {
        MAX_ACTIVE_ORDERS_PER_MARKET
    };
    let levels_created = 1;
    let levels_removed = fills + 1;
    let searches = fills + 1;
    let changed_levels = levels_created + levels_removed;
    let radix_nodes = (fills * 2 + 3) * 64;
    let storage_ops =
        changed_levels * 71 + fills * 40 + searches * 6 + TERMINAL_REMAINDER_CANCEL_STORAGE_OPS;
    let log_bytes = fills * (1_088 + 2 * FILL_FEE_LOG_BYTES + V2_LOG_BYTES)
        + ORDER_PLACED_LOG_BYTES
        + V2_LOG_BYTES
        + ORDER_CANCELLED_LOG_BYTES
        + V2_LOG_BYTES;
    GAS_LIMIT_ORDER
        + fills * GAS_PER_FILL
        + changed_levels * GAS_PER_CHANGED_LEVEL
        + radix_nodes * GAS_PER_RADIX_NODE
        + storage_ops * GAS_PER_MATCH_STORAGE_OP
        + log_bytes * GAS_PER_MATCH_LOG_BYTE
}

/// Intrinsic gas of a call: 21,000 plus 4 per zero and 16 per nonzero calldata byte.
pub fn intrinsic_gas(data: &[u8]) -> u64 {
    21_000
        + data
            .iter()
            .map(|b| if *b == 0 { 4 } else { 16 })
            .sum::<u64>()
}

/// Intrinsic gas for calldata of `len` bytes with at most `nonzero` nonzero bytes.
const fn intrinsic_worst_case(len: u64, nonzero: u64) -> u64 {
    21_000 + nonzero * 16 + (len - nonzero) * 4
}

/// Worst case nonzero bytes in a judged `submitLimitOrder`: selector (4), a
/// hashed market id (32), side (1), a price below 65,536 (2) and lots below
/// 256 (1); flags are zero.
const LIMIT_ORDER_NONZERO_BYTES: u64 = 4 + 32 + 1 + 2 + 1;
/// Worst case nonzero bytes in `cancelOrder`: selector (4) and a u64 order id (8).
const CANCEL_NONZERO_BYTES: u64 = 4 + 8;

/// Declared gas for a resting (1-lot) limit order: worst-case calldata intrinsic
/// plus the protocol precharge.
pub const GAS_SPOT_PLACE: u64 =
    intrinsic_worst_case(164, LIMIT_ORDER_NONZERO_BYTES) + limit_order_precharge(1);
/// Declared gas for a 2-lot taker that fills two 1-lot makers.
pub const GAS_SPOT_TAKE: u64 =
    intrinsic_worst_case(164, LIMIT_ORDER_NONZERO_BYTES) + limit_order_precharge(2);
/// Declared gas for `cancelOrder`: worst-case calldata intrinsic plus the flat charge.
pub const GAS_SPOT_CANCEL: u64 = intrinsic_worst_case(36, CANCEL_NONZERO_BYTES) + GAS_CANCEL_ORDER;

/// Order id from an `OrderPlaced` log's topics, if the log is one.
pub fn order_placed_id(log_address: Address, topics: &[B256]) -> Option<B256> {
    (log_address == SPOT_BOOK && topics.len() == 4 && topics[0] == order_placed_topic())
        .then(|| topics[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{b256, hex};

    #[test]
    fn selectors_match_the_node_signatures() {
        assert_eq!(
            hex::encode(selector(
                "submitLimitOrder(bytes32,uint8,uint256,uint256,uint8)"
            )),
            hex::encode(&limit_order(B256::ZERO, Side::Buy, 1, 1)[..4])
        );
        assert_eq!(hex::encode(selector("deposit()")), "d0e30db0");
        assert_eq!(
            hex::encode(selector("transfer(address,uint256)")),
            "a9059cbb"
        );
    }

    #[test]
    fn market_id_matches_the_fleet_market() {
        // createSpotMarket(WDS, TEST_USD) on a diesis-dev fleet created this id.
        assert_eq!(
            market_id(WDS, TEST_USD),
            b256!("99461d208dd762e90b36ebf0d1e63da834b187350d558074e5187f2f3a43506b")
        );
    }

    #[test]
    fn limit_order_calldata_layout() {
        let market = market_id(WDS, TEST_USD);
        let data = limit_order(market, Side::Sell, 1_001, 2);
        assert_eq!(data.len(), 4 + 5 * 32);
        assert_eq!(&data[4..36], market.as_slice());
        assert_eq!(data[4 + 63], 1, "side word");
        assert_eq!(
            U256::from_be_slice(&data[4 + 64..4 + 96]),
            U256::from(1_001)
        );
        assert_eq!(U256::from_be_slice(&data[4 + 96..4 + 128]), U256::from(2));
        assert!(data[4 + 128..].iter().all(|b| *b == 0), "flags zero");
    }

    #[test]
    fn cancel_calldata_layout() {
        let id = B256::from(U256::from(4u64).to_be_bytes::<32>());
        let data = cancel_order(id);
        assert_eq!(data.len(), 36);
        assert_eq!(&data[..4], &selector("cancelOrder(bytes32)"));
        assert_eq!(&data[4..], id.as_slice());
    }

    #[test]
    fn precharge_matches_the_node_formula() {
        assert_eq!(limit_order_precharge(1), 93_824);
        assert_eq!(limit_order_precharge(2), 129_272);
        // Node test anchor: stop_activation_precharge(223) = 5,000 + limit precharge.
        assert_eq!(5_000 + limit_order_precharge(223), 7_968_280);
        assert_eq!(5_000 + limit_order_precharge(224), 8_003_728);
    }

    #[test]
    fn declared_gas_is_worst_case_intrinsic_plus_precharge() {
        assert_eq!(GAS_SPOT_PLACE, 22_136 + 93_824);
        assert_eq!(GAS_SPOT_TAKE, 22_136 + 129_272);
        assert_eq!(GAS_SPOT_CANCEL, 21_288 + 3_000);
        // A live fleet accepted these exact amounts; one gas less ran out.
        let market = market_id(WDS, TEST_USD);
        let place = limit_order(market, Side::Sell, 1_001, 1);
        assert_eq!(
            intrinsic_gas(&place) + limit_order_precharge(1),
            GAS_SPOT_PLACE
        );
        let take = limit_order(market, Side::Sell, 65_535, 2);
        assert!(intrinsic_gas(&take) + limit_order_precharge(2) <= GAS_SPOT_TAKE);
        let cancel = cancel_order(B256::from(U256::from(u64::MAX).to_be_bytes::<32>()));
        assert_eq!(intrinsic_gas(&cancel) + 3_000, GAS_SPOT_CANCEL);
    }

    #[test]
    fn order_placed_log_yields_the_order_id() {
        let id = B256::with_last_byte(9);
        let topics = [order_placed_topic(), id, B256::ZERO, B256::ZERO];
        assert_eq!(order_placed_id(SPOT_BOOK, &topics), Some(id));
        assert_eq!(order_placed_id(MARKETS, &topics), None);
        assert_eq!(order_placed_id(SPOT_BOOK, &topics[..3]), None);
    }
}
