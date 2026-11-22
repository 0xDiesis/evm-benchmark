use crate::types::{LatencyStats, PerMethodStats, TransactionType, TxOutcomeCounts, TxRecord};
use alloy_primitives::{Address, B256};
use dashmap::DashMap;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

/// Confirmed transaction entry with latency data.
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct ConfirmedEntry {
    latency_ms: u64,
    method: TransactionType,
    gas_limit: u64,
    gas_used: Option<u64>,
    wave: Option<u32>,
    /// Receipt status: `Some(true)` success, `Some(false)` revert, `None` not read.
    status: Option<bool>,
}

impl ConfirmedEntry {
    fn reverted(&self) -> bool {
        self.status == Some(false)
    }
}

/// Metadata captured when an accepted transaction enters confirmation tracking.
#[derive(Clone, Debug)]
pub struct SubmittedTxMeta {
    pub hash: B256,
    pub nonce: u64,
    pub sender: Address,
    pub gas_limit: u64,
    pub method: TransactionType,
    pub wave: Option<u32>,
    pub submit_time: Instant,
}

/// Per-wave latency breakdown.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct WaveEntry {
    pub wave: u32,
    pub count: u32,
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
    pub max: u64,
}

pub struct LatencyTracker {
    /// Pending transactions awaiting confirmation.
    pending: Arc<DashMap<B256, TxRecord>>,
    /// Pending hash for each sender and nonce, so block-body identity matching
    /// stays constant-time while many transactions are pending.
    pending_by_sender_nonce: Arc<DashMap<(Address, u64), B256>>,
    /// Pending records visited by linear scans. Confirmation lookups must not
    /// add to this; it lets tests prove they stay bounded.
    pending_scan_visits: Arc<AtomicU64>,
    /// Confirmed transactions moved out of pending immediately.
    confirmed: Arc<parking_lot::Mutex<Vec<ConfirmedEntry>>>,
    /// Included transactions that did not revert (successful or status
    /// unknown), for lock-free reads.
    confirmed_count: Arc<AtomicU32>,
}

impl Clone for LatencyTracker {
    fn clone(&self) -> Self {
        LatencyTracker {
            pending: Arc::clone(&self.pending),
            pending_by_sender_nonce: Arc::clone(&self.pending_by_sender_nonce),
            pending_scan_visits: Arc::clone(&self.pending_scan_visits),
            confirmed: Arc::clone(&self.confirmed),
            confirmed_count: Arc::clone(&self.confirmed_count),
        }
    }
}

impl Default for LatencyTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyTracker {
    pub fn new() -> Self {
        LatencyTracker {
            pending: Arc::new(DashMap::new()),
            pending_by_sender_nonce: Arc::new(DashMap::new()),
            pending_scan_visits: Arc::new(AtomicU64::new(0)),
            confirmed: Arc::new(parking_lot::Mutex::new(Vec::new())),
            confirmed_count: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Record a transaction submission with an optional wave index.
    #[allow(dead_code)]
    pub fn record_submit(
        &self,
        hash: B256,
        nonce: u64,
        sender: alloy_primitives::Address,
        gas_limit: u64,
        method: TransactionType,
    ) {
        self.record_submit_with_wave(hash, nonce, sender, gas_limit, method, None);
    }

    pub fn record_submit_at(
        &self,
        hash: B256,
        nonce: u64,
        sender: alloy_primitives::Address,
        gas_limit: u64,
        method: TransactionType,
        submit_time: Instant,
    ) {
        self.record_submit_meta(SubmittedTxMeta {
            hash,
            nonce,
            sender,
            gas_limit,
            method,
            wave: None,
            submit_time,
        });
    }

    /// Record a transaction submission with a wave index for per-wave tracking.
    #[allow(dead_code)]
    pub fn record_submit_with_wave(
        &self,
        hash: B256,
        nonce: u64,
        sender: alloy_primitives::Address,
        gas_limit: u64,
        method: TransactionType,
        wave: Option<u32>,
    ) {
        self.record_submit_meta(SubmittedTxMeta {
            hash,
            nonce,
            sender,
            gas_limit,
            method,
            wave,
            submit_time: Instant::now(),
        });
    }

    /// Record an accepted transaction submission.
    pub fn record_submit_meta(&self, meta: SubmittedTxMeta) {
        let record = TxRecord {
            hash: meta.hash,
            nonce: meta.nonce,
            sender: meta.sender,
            gas_limit: meta.gas_limit,
            gas_used: None,
            submit_time: meta.submit_time,
            block_time: None,
            method: meta.method,
            revert_status: None,
            wave: meta.wave,
        };
        self.pending_by_sender_nonce
            .insert((meta.sender, meta.nonce), meta.hash);
        self.pending.insert(meta.hash, record);
    }

    /// Mark a transaction as confirmed. Removes it from pending and moves
    /// latency data to the confirmed list.
    pub fn on_block_inclusion(&self, tx_hash: B256, block_time: Instant) -> bool {
        self.on_block_inclusion_with_receipt(tx_hash, block_time, None, None)
    }

    /// Mark a transaction as confirmed and attach receipt-derived execution data
    /// when available.
    pub fn on_block_inclusion_with_receipt(
        &self,
        tx_hash: B256,
        block_time: Instant,
        gas_used: Option<u64>,
        status_success: Option<bool>,
    ) -> bool {
        if let Some((_, record)) = self.pending.remove(&tx_hash) {
            self.pending_by_sender_nonce
                .remove_if(&(record.sender, record.nonce), |_, hash| *hash == tx_hash);
            let latency_ms = block_time
                .saturating_duration_since(record.submit_time)
                .as_millis() as u64;
            let entry = ConfirmedEntry {
                latency_ms,
                method: record.method,
                gas_limit: record.gas_limit,
                gas_used: gas_used.or(record.gas_used),
                wave: record.wave,
                status: status_success,
            };
            if !entry.reverted() {
                self.confirmed_count.fetch_add(1, Ordering::Relaxed);
            }
            self.confirmed.lock().push(entry);
            true
        } else {
            false
        }
    }

    /// Mark a transaction as confirmed by sender and nonce.
    ///
    /// This is a fallback for RPCs that expose block bodies before individual
    /// receipt/hash lookup is reliable under load.
    pub fn on_account_nonce_inclusion(
        &self,
        sender: Address,
        nonce: u64,
        block_time: Instant,
    ) -> bool {
        let pending_hash = self
            .pending_by_sender_nonce
            .get(&(sender, nonce))
            .map(|entry| *entry.value());
        if let Some(hash) = pending_hash {
            self.on_block_inclusion(hash, block_time)
        } else {
            false
        }
    }

    /// Confirm the oldest pending transactions when running against an
    /// explicitly isolated benchmark chain and block inclusion is known but RPC
    /// transaction identities lag.
    pub fn confirm_oldest_pending(&self, count: u32, block_time: Instant) -> u32 {
        if count == 0 {
            return 0;
        }
        let mut pending: Vec<_> = self
            .pending
            .iter()
            .map(|entry| (*entry.key(), entry.submit_time))
            .collect();
        self.pending_scan_visits
            .fetch_add(pending.len() as u64, Ordering::Relaxed);
        pending.sort_by_key(|(_, submit_time)| *submit_time);

        let mut confirmed = 0;
        for (hash, _) in pending.into_iter().take(count as usize) {
            if self.on_block_inclusion(hash, block_time) {
                confirmed += 1;
            }
        }
        confirmed
    }

    /// Pending records visited by linear scans since the tracker was created.
    pub fn pending_scan_visits(&self) -> u64 {
        self.pending_scan_visits.load(Ordering::Relaxed)
    }

    /// Number of transactions still awaiting confirmation.
    pub fn pending_count(&self) -> u32 {
        self.pending.len() as u32
    }

    /// Included transactions that did not revert: successful receipts plus
    /// inclusions whose receipt status was not read. Reverts are excluded.
    pub fn confirmed_count(&self) -> u32 {
        self.confirmed_count.load(Ordering::Relaxed)
    }

    /// Included transactions split by receipt outcome.
    pub fn outcome_counts(&self) -> TxOutcomeCounts {
        let mut counts = TxOutcomeCounts::default();
        for entry in self.confirmed.lock().iter() {
            match entry.status {
                Some(true) => counts.successful += 1,
                Some(false) => counts.reverted += 1,
                None => counts.status_unknown += 1,
            }
        }
        counts
    }

    /// Return hashes of all transactions that have not yet been confirmed.
    pub fn pending_hashes(&self) -> Vec<B256> {
        self.pending.iter().map(|r| *r.key()).collect()
    }

    /// Compute latency statistics across all confirmed transactions.
    pub fn statistics(&self) -> LatencyStats {
        let confirmed = self.confirmed.lock();
        let mut latencies: Vec<u64> = confirmed
            .iter()
            .filter(|e| !e.reverted())
            .map(|e| e.latency_ms)
            .collect();

        if latencies.is_empty() {
            return LatencyStats {
                p50: 0,
                p95: 0,
                p99: 0,
                min: 0,
                max: 0,
                avg: 0,
            };
        }

        latencies.sort_unstable();
        let sum: u64 = latencies.iter().sum();

        LatencyStats {
            p50: percentile(&latencies, 0.50),
            p95: percentile(&latencies, 0.95),
            p99: percentile(&latencies, 0.99),
            min: *latencies.first().unwrap_or(&0),
            max: *latencies.last().unwrap_or(&0),
            avg: sum / latencies.len() as u64,
        }
    }

    /// Compute per-wave latency breakdown.
    pub fn per_wave_statistics(&self) -> Vec<WaveEntry> {
        let confirmed = self.confirmed.lock();

        // Collect latencies grouped by wave
        let mut wave_map: BTreeMap<u32, Vec<u64>> = BTreeMap::new();
        for entry in confirmed.iter() {
            if let Some(wave) = entry.wave {
                wave_map.entry(wave).or_default().push(entry.latency_ms);
            }
        }

        wave_map
            .into_iter()
            .map(|(wave, mut latencies)| {
                latencies.sort_unstable();
                WaveEntry {
                    wave,
                    count: latencies.len() as u32,
                    p50: percentile(&latencies, 0.50),
                    p95: percentile(&latencies, 0.95),
                    p99: percentile(&latencies, 0.99),
                    max: *latencies.last().unwrap_or(&0),
                }
            })
            .collect()
    }

    /// Compute per-method statistics for EVM benchmarks.
    ///
    /// `confirmed` excludes reverts; gas used averages come from receipts of
    /// the matching outcome only (never the declared limit); latencies cover
    /// confirmed transactions.
    #[allow(dead_code)]
    pub fn per_method_statistics(&self) -> BTreeMap<String, PerMethodStats> {
        #[derive(Default)]
        struct Acc {
            stats: PerMethodStats,
            latencies: Vec<u64>,
            gas_ok: (u64, u64),
            gas_reverted: (u64, u64),
            gas_limit: (u64, u64),
        }
        let mut by_method: BTreeMap<String, Acc> = BTreeMap::new();
        for entry in self.confirmed.lock().iter() {
            let acc = by_method.entry(format!("{:?}", entry.method)).or_default();
            acc.stats.count += 1;
            acc.gas_limit.0 += entry.gas_limit;
            acc.gas_limit.1 += 1;
            match entry.status {
                Some(true) => {
                    acc.stats.successful += 1;
                    if let Some(gas) = entry.gas_used {
                        acc.gas_ok.0 += gas;
                        acc.gas_ok.1 += 1;
                    }
                }
                Some(false) => {
                    acc.stats.reverted += 1;
                    if let Some(gas) = entry.gas_used {
                        acc.gas_reverted.0 += gas;
                        acc.gas_reverted.1 += 1;
                    }
                }
                None => acc.stats.status_unknown += 1,
            }
            if !entry.reverted() {
                acc.stats.confirmed += 1;
                acc.latencies.push(entry.latency_ms);
            }
        }
        for entry in self.pending.iter() {
            let acc = by_method.entry(format!("{:?}", entry.method)).or_default();
            acc.stats.count += 1;
            acc.stats.pending += 1;
            acc.gas_limit.0 += entry.gas_limit;
            acc.gas_limit.1 += 1;
        }

        let mean = |(sum, n): (u64, u64)| sum.checked_div(n).unwrap_or(0);
        by_method
            .into_iter()
            .map(|(method, mut acc)| {
                acc.latencies.sort_unstable();
                acc.stats.avg_gas = mean(acc.gas_ok);
                acc.stats.avg_gas_reverted = mean(acc.gas_reverted);
                acc.stats.avg_gas_limit = mean(acc.gas_limit);
                acc.stats.latency_p50 = percentile(&acc.latencies, 0.50);
                acc.stats.latency_p95 = percentile(&acc.latencies, 0.95);
                acc.stats.latency_p99 = percentile(&acc.latencies, 0.99);
                (method, acc.stats)
            })
            .collect()
    }
}

fn percentile(sorted: &[u64], p: f32) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f32 * p) as usize).min(sorted.len() - 1);
    sorted[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_latency_stats() {
        let tracker = LatencyTracker::new();
        let hash = B256::default();

        tracker.record_submit(
            hash,
            0,
            alloy_primitives::Address::default(),
            21_000,
            TransactionType::SimpleTransfer,
        );

        std::thread::sleep(std::time::Duration::from_millis(100));
        tracker.on_block_inclusion(hash, Instant::now());

        assert_eq!(tracker.pending_count(), 0, "Should be removed from pending");
        assert_eq!(tracker.confirmed_count(), 1, "Should be confirmed");

        let stats = tracker.statistics();
        assert!(stats.p50 >= 100 && stats.p50 < 200);
    }

    #[test]
    fn test_pending_removal_on_confirmation() {
        let tracker = LatencyTracker::new();
        let h1 = B256::with_last_byte(1);
        let h2 = B256::with_last_byte(2);
        let h3 = B256::with_last_byte(3);

        for h in [h1, h2, h3] {
            tracker.record_submit(
                h,
                0,
                alloy_primitives::Address::default(),
                21_000,
                TransactionType::SimpleTransfer,
            );
        }

        assert_eq!(tracker.pending_count(), 3);
        assert_eq!(tracker.confirmed_count(), 0);

        tracker.on_block_inclusion(h1, Instant::now());
        assert_eq!(tracker.pending_count(), 2);
        assert_eq!(tracker.confirmed_count(), 1);

        tracker.on_block_inclusion(h2, Instant::now());
        assert_eq!(tracker.pending_count(), 1);
        assert_eq!(tracker.confirmed_count(), 2);

        // Double-confirm should return false
        assert!(!tracker.on_block_inclusion(h1, Instant::now()));
    }

    #[test]
    fn test_account_nonce_confirmation_fallback() {
        let tracker = LatencyTracker::new();
        let hash = B256::with_last_byte(0x42);
        let sender = Address::with_last_byte(0x11);

        tracker.record_submit(hash, 7, sender, 21_000, TransactionType::SimpleTransfer);

        assert!(tracker.on_account_nonce_inclusion(sender, 7, Instant::now()));
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.confirmed_count(), 1);
        assert!(!tracker.on_account_nonce_inclusion(sender, 7, Instant::now()));
    }

    #[test]
    fn account_nonce_lookups_do_not_scan_pending_transactions() {
        let tracker = LatencyTracker::new();
        let pending = 20_000u64;
        let sender = Address::with_last_byte(0x22);
        for nonce in 0..pending {
            let mut bytes = [0u8; 32];
            bytes[24..].copy_from_slice(&nonce.to_be_bytes());
            tracker.record_submit(
                B256::from(bytes),
                nonce,
                sender,
                21_000,
                TransactionType::SimpleTransfer,
            );
        }

        // A rescanned block repeats identities that are already confirmed or
        // belong to other clients. None of them may walk the pending map.
        let block_time = Instant::now();
        let other_sender = Address::with_last_byte(0x33);
        for nonce in 0..10_000u64 {
            assert!(!tracker.on_account_nonce_inclusion(other_sender, nonce, block_time));
        }
        for nonce in 0..pending {
            assert!(tracker.on_account_nonce_inclusion(sender, nonce, block_time));
            assert!(!tracker.on_account_nonce_inclusion(sender, nonce, block_time));
        }

        assert_eq!(tracker.pending_scan_visits(), 0);
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.confirmed_count(), pending as u32);
        assert!(tracker.pending_by_sender_nonce.is_empty());
    }

    #[test]
    fn hash_confirmation_releases_sender_nonce_identity() {
        let tracker = LatencyTracker::new();
        let hash = B256::with_last_byte(0x51);
        let sender = Address::with_last_byte(0x52);
        tracker.record_submit(hash, 3, sender, 21_000, TransactionType::SimpleTransfer);

        assert!(tracker.on_block_inclusion(hash, Instant::now()));
        assert!(tracker.pending_by_sender_nonce.is_empty());
        assert!(!tracker.on_account_nonce_inclusion(sender, 3, Instant::now()));
        assert_eq!(tracker.confirmed_count(), 1);
    }

    #[test]
    fn replaced_identity_keeps_the_latest_pending_hash() {
        let tracker = LatencyTracker::new();
        let sender = Address::with_last_byte(0x61);
        let original = B256::with_last_byte(0x62);
        let replacement = B256::with_last_byte(0x63);
        for hash in [original, replacement] {
            tracker.record_submit(hash, 9, sender, 21_000, TransactionType::SimpleTransfer);
        }

        // Confirming the superseded hash must not drop the replacement's index.
        assert!(tracker.on_block_inclusion(original, Instant::now()));
        assert!(tracker.on_account_nonce_inclusion(sender, 9, Instant::now()));
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.confirmed_count(), 2);
    }

    #[test]
    fn test_confirm_oldest_pending_caps_to_pending_count() {
        let tracker = LatencyTracker::new();
        for nonce in 0..3 {
            tracker.record_submit(
                B256::with_last_byte(nonce as u8),
                nonce,
                Address::default(),
                21_000,
                TransactionType::SimpleTransfer,
            );
        }

        assert_eq!(tracker.confirm_oldest_pending(10, Instant::now()), 3);
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.confirmed_count(), 3);
    }

    #[test]
    fn test_per_wave_statistics() {
        let tracker = LatencyTracker::new();

        for i in 0..6u8 {
            let hash = B256::with_last_byte(i);
            let wave = Some(i as u32 / 3); // wave 0: [0,1,2], wave 1: [3,4,5]
            tracker.record_submit_with_wave(
                hash,
                i as u64,
                alloy_primitives::Address::default(),
                21_000,
                TransactionType::SimpleTransfer,
                wave,
            );
        }

        // Confirm all
        std::thread::sleep(std::time::Duration::from_millis(10));
        for i in 0..6u8 {
            tracker.on_block_inclusion(B256::with_last_byte(i), Instant::now());
        }

        let waves = tracker.per_wave_statistics();
        assert_eq!(waves.len(), 2);
        assert_eq!(waves[0].wave, 0);
        assert_eq!(waves[0].count, 3);
        assert_eq!(waves[1].wave, 1);
        assert_eq!(waves[1].count, 3);
    }

    #[test]
    fn test_per_method_statistics_mixed_methods() {
        let tracker = LatencyTracker::new();

        // Submit 3 SimpleTransfer
        for i in 0..3u8 {
            let hash = B256::with_last_byte(i);
            tracker.record_submit(
                hash,
                i as u64,
                alloy_primitives::Address::default(),
                21_000,
                TransactionType::SimpleTransfer,
            );
        }
        // Submit 2 ERC20Mint
        for i in 3..5u8 {
            let hash = B256::with_last_byte(i);
            tracker.record_submit(
                hash,
                i as u64,
                alloy_primitives::Address::default(),
                60_000,
                TransactionType::ERC20Mint,
            );
        }

        // Confirm all
        std::thread::sleep(std::time::Duration::from_millis(5));
        for i in 0..5u8 {
            tracker.on_block_inclusion(B256::with_last_byte(i), Instant::now());
        }

        let stats = tracker.per_method_statistics();
        assert_eq!(stats.len(), 2, "Should have 2 methods");

        let transfer_stats = stats.get("SimpleTransfer").expect("missing SimpleTransfer");
        assert_eq!(transfer_stats.confirmed, 3);

        let mint_stats = stats.get("ERC20Mint").expect("missing ERC20Mint");
        assert_eq!(mint_stats.confirmed, 2);
    }

    #[test]
    fn test_per_method_statistics_single_method() {
        let tracker = LatencyTracker::new();

        for i in 0..4u8 {
            let hash = B256::with_last_byte(i);
            tracker.record_submit(
                hash,
                i as u64,
                alloy_primitives::Address::default(),
                21_000,
                TransactionType::SimpleTransfer,
            );
        }

        // Confirm all
        std::thread::sleep(std::time::Duration::from_millis(5));
        for i in 0..4u8 {
            tracker.on_block_inclusion(B256::with_last_byte(i), Instant::now());
        }

        let stats = tracker.per_method_statistics();
        assert_eq!(stats.len(), 1, "Should have 1 method");

        let transfer_stats = stats.get("SimpleTransfer").expect("missing SimpleTransfer");
        assert_eq!(transfer_stats.count, 4);
        assert_eq!(transfer_stats.confirmed, 4);
    }

    #[test]
    fn test_per_method_statistics_counts_pending_by_own_method() {
        let tracker = LatencyTracker::new();

        tracker.record_submit(
            B256::with_last_byte(1),
            1,
            alloy_primitives::Address::default(),
            21_000,
            TransactionType::SimpleTransfer,
        );
        tracker.record_submit(
            B256::with_last_byte(2),
            2,
            alloy_primitives::Address::default(),
            80_000,
            TransactionType::ERC20Mint,
        );

        std::thread::sleep(std::time::Duration::from_millis(5));
        tracker.on_block_inclusion(B256::with_last_byte(1), Instant::now());

        let stats = tracker.per_method_statistics();
        let transfer_stats = stats.get("SimpleTransfer").expect("missing SimpleTransfer");
        let mint_stats = stats.get("ERC20Mint").expect("missing ERC20Mint");

        assert_eq!(transfer_stats.count, 1);
        assert_eq!(transfer_stats.confirmed, 1);
        assert_eq!(mint_stats.count, 1);
        assert_eq!(mint_stats.confirmed, 0);
    }

    #[test]
    fn test_statistics_empty_tracker() {
        let tracker = LatencyTracker::new();
        let stats = tracker.statistics();
        assert_eq!(stats.p50, 0);
        assert_eq!(stats.p95, 0);
        assert_eq!(stats.p99, 0);
        assert_eq!(stats.min, 0);
        assert_eq!(stats.max, 0);
        assert_eq!(stats.avg, 0);
    }

    #[test]
    fn test_pending_hashes_returns_correct_hashes() {
        let tracker = LatencyTracker::new();
        let h1 = B256::with_last_byte(10);
        let h2 = B256::with_last_byte(20);
        let h3 = B256::with_last_byte(30);

        for h in [h1, h2, h3] {
            tracker.record_submit(
                h,
                0,
                alloy_primitives::Address::default(),
                21_000,
                TransactionType::SimpleTransfer,
            );
        }

        let mut pending = tracker.pending_hashes();
        pending.sort();
        let mut expected = vec![h1, h2, h3];
        expected.sort();
        assert_eq!(pending, expected);
    }

    #[test]
    fn test_pending_hashes_empty_after_all_confirmed() {
        let tracker = LatencyTracker::new();
        let h1 = B256::with_last_byte(1);
        let h2 = B256::with_last_byte(2);

        for h in [h1, h2] {
            tracker.record_submit(
                h,
                0,
                alloy_primitives::Address::default(),
                21_000,
                TransactionType::SimpleTransfer,
            );
        }

        tracker.on_block_inclusion(h1, Instant::now());
        tracker.on_block_inclusion(h2, Instant::now());

        assert!(tracker.pending_hashes().is_empty());
    }

    #[test]
    fn test_latency_tracker_default() {
        let tracker = LatencyTracker::default();
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.confirmed_count(), 0);
    }

    #[test]
    fn test_record_submit_without_wave_pending_count() {
        let tracker = LatencyTracker::new();

        tracker.record_submit(
            B256::with_last_byte(1),
            0,
            alloy_primitives::Address::default(),
            21_000,
            TransactionType::SimpleTransfer,
        );
        tracker.record_submit(
            B256::with_last_byte(2),
            1,
            alloy_primitives::Address::default(),
            21_000,
            TransactionType::SimpleTransfer,
        );

        assert_eq!(tracker.pending_count(), 2);
        assert_eq!(tracker.confirmed_count(), 0);
    }

    #[test]
    fn test_on_block_inclusion_unknown_hash() {
        let tracker = LatencyTracker::new();
        let unknown = B256::with_last_byte(99);
        assert!(!tracker.on_block_inclusion(unknown, Instant::now()));
    }

    #[test]
    fn test_multiple_concurrent_confirms() {
        let tracker = LatencyTracker::new();

        for i in 0..10u8 {
            tracker.record_submit(
                B256::with_last_byte(i),
                i as u64,
                alloy_primitives::Address::default(),
                21_000,
                TransactionType::SimpleTransfer,
            );
        }

        assert_eq!(tracker.pending_count(), 10);

        // Confirm all
        std::thread::sleep(std::time::Duration::from_millis(5));
        for i in 0..10u8 {
            let result = tracker.on_block_inclusion(B256::with_last_byte(i), Instant::now());
            assert!(result, "Hash {} should confirm successfully", i);
        }

        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.confirmed_count(), 10);

        let stats = tracker.statistics();
        assert!(stats.min > 0, "Latency should be non-zero");
        assert!(stats.max >= stats.min);
    }

    #[test]
    fn test_confirmation_before_submit_time_saturates_latency() {
        let tracker = LatencyTracker::new();
        let hash = B256::with_last_byte(0xaa);
        let submit_time = Instant::now() + std::time::Duration::from_millis(100);

        tracker.record_submit_at(
            hash,
            0,
            alloy_primitives::Address::default(),
            21_000,
            TransactionType::SimpleTransfer,
            submit_time,
        );

        assert!(tracker.on_block_inclusion(hash, Instant::now()));
        let stats = tracker.statistics();
        assert_eq!(stats.p50, 0);
        assert_eq!(stats.avg, 0);
    }

    #[test]
    fn test_per_method_statistics_ignores_gas_without_receipt_status() {
        let tracker = LatencyTracker::new();

        for (idx, gas_used) in [(1u8, 40_000u64), (2u8, 80_000u64)] {
            let hash = B256::with_last_byte(idx);
            tracker.record_submit(
                hash,
                idx as u64,
                alloy_primitives::Address::default(),
                90_000,
                TransactionType::ERC20Mint,
            );
            tracker.pending.get_mut(&hash).unwrap().gas_used = Some(gas_used);
        }

        std::thread::sleep(std::time::Duration::from_millis(5));
        for idx in [1u8, 2u8] {
            tracker.on_block_inclusion(B256::with_last_byte(idx), Instant::now());
        }

        let stats = tracker.per_method_statistics();
        let mint_stats = stats.get("ERC20Mint").expect("missing ERC20Mint");
        assert_eq!(mint_stats.confirmed, 2);
        assert_eq!(mint_stats.status_unknown, 2);
        assert_eq!(
            mint_stats.avg_gas, 0,
            "gas from inclusions without a receipt status is not a success average"
        );
        assert_eq!(mint_stats.avg_gas_limit, 90_000);
    }

    #[test]
    fn test_per_method_statistics_uses_receipt_gas_and_status() {
        let tracker = LatencyTracker::new();
        let ok_hash = B256::with_last_byte(1);
        let reverted_hash = B256::with_last_byte(2);

        for (idx, hash) in [ok_hash, reverted_hash].into_iter().enumerate() {
            tracker.record_submit(
                hash,
                idx as u64,
                alloy_primitives::Address::default(),
                90_000,
                TransactionType::ERC20Mint,
            );
        }

        std::thread::sleep(std::time::Duration::from_millis(5));
        tracker.on_block_inclusion_with_receipt(ok_hash, Instant::now(), Some(30_000), Some(true));
        tracker.on_block_inclusion_with_receipt(
            reverted_hash,
            Instant::now(),
            Some(50_000),
            Some(false),
        );

        let stats = tracker.per_method_statistics();
        let mint_stats = stats.get("ERC20Mint").expect("missing ERC20Mint");
        assert_eq!(mint_stats.count, 2);
        assert_eq!(
            mint_stats.confirmed, 1,
            "a reverted receipt is not a confirmation"
        );
        assert_eq!(mint_stats.successful, 1);
        assert_eq!(mint_stats.reverted, 1);
        assert_eq!(mint_stats.status_unknown, 0);
        assert_eq!(
            mint_stats.avg_gas, 30_000,
            "gas used comes from successful receipts only"
        );
        assert_eq!(mint_stats.avg_gas_reverted, 50_000);
    }

    #[test]
    fn reverted_receipts_are_counted_apart_from_confirmations() {
        let tracker = LatencyTracker::new();
        let ok = B256::with_last_byte(1);
        let reverted = B256::with_last_byte(2);
        let no_status = B256::with_last_byte(3);
        let backlog = B256::with_last_byte(4);
        for (nonce, hash) in [ok, reverted, no_status, backlog].into_iter().enumerate() {
            tracker.record_submit(
                hash,
                nonce as u64,
                Address::default(),
                90_000,
                TransactionType::ERC20Transfer,
            );
        }

        let now = Instant::now();
        tracker.on_block_inclusion_with_receipt(ok, now, Some(30_000), Some(true));
        tracker.on_block_inclusion_with_receipt(reverted, now, Some(25_000), Some(false));
        // A block-body inclusion carries no receipt status.
        tracker.on_block_inclusion(no_status, now);

        let outcomes = tracker.outcome_counts();
        assert_eq!(outcomes.successful, 1);
        assert_eq!(outcomes.reverted, 1);
        assert_eq!(outcomes.status_unknown, 1);
        assert_eq!(
            tracker.confirmed_count(),
            2,
            "confirmed = successful + status unknown, never reverted"
        );
        assert_eq!(tracker.pending_count(), 1, "backlog stays pending");
    }

    #[test]
    fn latency_statistics_exclude_reverted_transactions() {
        let tracker = LatencyTracker::new();
        let submit = Instant::now();
        let ok = B256::with_last_byte(1);
        let reverted = B256::with_last_byte(2);
        for (nonce, hash) in [ok, reverted].into_iter().enumerate() {
            tracker.record_submit_at(
                hash,
                nonce as u64,
                Address::default(),
                21_000,
                TransactionType::ETHTransfer,
                submit,
            );
        }
        tracker.on_block_inclusion_with_receipt(
            ok,
            submit + std::time::Duration::from_millis(100),
            Some(21_000),
            Some(true),
        );
        tracker.on_block_inclusion_with_receipt(
            reverted,
            submit + std::time::Duration::from_millis(900),
            Some(21_000),
            Some(false),
        );
        let stats = tracker.statistics();
        assert_eq!(stats.max, 100);
    }

    #[test]
    fn test_percentile_empty_slice_returns_zero() {
        assert_eq!(percentile(&[], 0.95), 0);
    }
}
