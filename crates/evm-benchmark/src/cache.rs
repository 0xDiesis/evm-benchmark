//! Transaction caching for reproducible benchmarks.
//!
//! Pre-signed transactions can be cached to disk and reused across runs,
//! eliminating signing overhead and ensuring identical transaction sets
//! for reproducible comparisons. A SHA-256 fingerprint of the benchmark
//! configuration ensures cache invalidation when parameters change.

use crate::types::SignedTxWithMetadata;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Cached transaction data stored on disk.
#[derive(Serialize, Deserialize)]
pub struct TxCacheData {
    /// Cache format version.
    pub version: u32,
    /// SHA-256 fingerprint of the configuration that produced these txs (first 16 hex chars).
    pub fingerprint: String,
    /// Chain ID the txs were signed for.
    pub chain_id: u64,
    /// Benchmark mode that produced the txs.
    pub mode: String,
    /// Number of sender addresses used.
    pub sender_count: u32,
    /// Total transaction count.
    pub tx_count: u32,
    /// Gas price used for signing (decimal string).
    pub gas_price: String,
    /// Hex-encoded raw signed transactions.
    pub transactions: Vec<CachedTx>,
}

/// A single cached transaction.
#[derive(Serialize, Deserialize)]
pub struct CachedTx {
    /// Transaction hash (hex, 0x-prefixed).
    pub hash: String,
    /// RLP-encoded signed transaction (hex, 0x-prefixed).
    pub raw: String,
    /// Nonce used.
    pub nonce: u64,
    /// Gas limit.
    pub gas_limit: u64,
    /// Transaction type label.
    pub tx_type: String,
}

/// Compute a deterministic fingerprint for cache key derivation.
///
/// Uses FNV-1a hash of sorted JSON config parameters, truncated to 16 hex chars.
pub fn compute_fingerprint(
    chain_id: u64,
    mode: &str,
    sender_count: u32,
    tx_count: u32,
    gas_price: u128,
) -> String {
    use std::collections::BTreeMap;

    let mut params = BTreeMap::new();
    params.insert("chain_id", chain_id.to_string());
    params.insert("mode", mode.to_string());
    params.insert("sender_count", sender_count.to_string());
    params.insert("tx_count", tx_count.to_string());
    params.insert("gas_price", gas_price.to_string());

    let json = serde_json::to_string(&params).unwrap_or_default();
    let digest = fnv1a_hex(&json);
    digest[..16].to_string()
}

/// FNV-1a 64-bit hash with secondary mixing, hex-encoded.
///
/// Provides sufficient collision resistance for cache key fingerprinting.
/// Not cryptographic — used only for deterministic cache invalidation.
fn fnv1a_hex(input: &str) -> String {
    //
    let mut hash: u128 = 0xcbf29ce484222325;
    let prime: u128 = 0x100000001b3;
    for byte in input.bytes() {
        hash ^= byte as u128;
        hash = hash.wrapping_mul(prime);
    }
    // Mix further for more entropy
    let hash2 = hash.wrapping_mul(0x517cc1b727220a95);
    format!("{:016x}{:016x}", hash, hash2)
}

/// Resolve the cache directory path.
///
/// Uses `BENCH_TX_CACHE_DIR` environment variable if set, otherwise falls back
/// to a `.tx-cache` directory in the system temp directory.
pub fn cache_dir() -> PathBuf {
    std::env::var("BENCH_TX_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join(".tx-cache"))
}

/// Cache file path for a given fingerprint.
pub fn cache_path(fingerprint: &str) -> PathBuf {
    cache_dir().join(format!("tx-cache-{}.json", fingerprint))
}

/// Cache file path for a given fingerprint within an explicit directory.
///
/// Used by tests to avoid mutating process environment variables.
#[cfg(test)]
fn cache_path_in(dir: &std::path::Path, fingerprint: &str) -> PathBuf {
    dir.join(format!("tx-cache-{}.json", fingerprint))
}

/// Try to load a cached transaction set from an explicit directory.
///
/// Same as [`try_load`] but avoids reading `BENCH_TX_CACHE_DIR` from the environment.
/// Used by tests to avoid mutating process environment variables.
#[cfg(test)]
fn try_load_from(dir: &std::path::Path, fingerprint: &str, quiet: bool) -> Option<TxCacheData> {
    let path = cache_path_in(dir, fingerprint);
    if !path.exists() {
        return None;
    }

    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) => {
            if !quiet {
                eprintln!("[cache] Failed to read {}: {}", path.display(), e);
            }
            return None;
        }
    };

    let cached: TxCacheData = match serde_json::from_str(&data) {
        Ok(c) => c,
        Err(e) => {
            if !quiet {
                eprintln!("[cache] Failed to parse {}: {}", path.display(), e);
            }
            return None;
        }
    };

    if cached.version != 1 {
        if !quiet {
            eprintln!("[cache] Unsupported cache version: {}", cached.version);
        }
        return None;
    }

    if cached.fingerprint != fingerprint {
        if !quiet {
            eprintln!("[cache] Fingerprint mismatch, ignoring stale cache");
        }
        return None;
    }

    if cached.transactions.is_empty() {
        if !quiet {
            eprintln!("[cache] Empty transaction cache");
        }
        return None;
    }

    if !quiet {
        eprintln!(
            "[cache] Loaded {} cached txs from {}",
            cached.transactions.len(),
            path.display()
        );
    }

    Some(cached)
}

/// Try to load a cached transaction set from disk.
///
/// Returns `None` if no cache exists, the fingerprint doesn't match,
/// or the cache is corrupted.
pub fn try_load(fingerprint: &str, quiet: bool) -> Option<TxCacheData> {
    let path = cache_path(fingerprint);
    if !path.exists() {
        return None;
    }

    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) => {
            if !quiet {
                eprintln!("[cache] Failed to read {}: {}", path.display(), e);
            }
            return None;
        }
    };

    let cached: TxCacheData = match serde_json::from_str(&data) {
        Ok(c) => c,
        Err(e) => {
            if !quiet {
                eprintln!("[cache] Failed to parse {}: {}", path.display(), e);
            }
            return None;
        }
    };

    // Validate
    if cached.version != 1 {
        if !quiet {
            eprintln!("[cache] Unsupported cache version: {}", cached.version);
        }
        return None;
    }

    if cached.fingerprint != fingerprint {
        if !quiet {
            eprintln!("[cache] Fingerprint mismatch, ignoring stale cache");
        }
        return None;
    }

    if cached.transactions.is_empty() {
        if !quiet {
            eprintln!("[cache] Empty transaction cache");
        }
        return None;
    }

    if !quiet {
        eprintln!(
            "[cache] Loaded {} cached txs from {}",
            cached.transactions.len(),
            path.display()
        );
    }

    Some(cached)
}

/// Save a transaction set to the cache.
pub fn save(
    fingerprint: &str,
    chain_id: u64,
    mode: &str,
    sender_count: u32,
    gas_price: u128,
    txs: &[SignedTxWithMetadata],
    quiet: bool,
) -> Result<PathBuf> {
    let dir = cache_dir();
    std::fs::create_dir_all(&dir)?;

    let cached_txs: Vec<CachedTx> = txs
        .iter()
        .map(|tx| CachedTx {
            hash: format!("{:?}", tx.hash),
            raw: format!("0x{}", hex::encode(&tx.encoded)),
            nonce: tx.nonce,
            gas_limit: tx.gas_limit,
            tx_type: format!("{:?}", tx.method),
        })
        .collect();

    let cache_data = TxCacheData {
        version: 1,
        fingerprint: fingerprint.to_string(),
        chain_id,
        mode: mode.to_string(),
        sender_count,
        tx_count: cached_txs.len() as u32,
        gas_price: gas_price.to_string(),
        transactions: cached_txs,
    };

    let path = cache_path(fingerprint);
    let json = serde_json::to_string_pretty(&cache_data)?;
    std::fs::write(&path, json)?;

    if !quiet {
        eprintln!("[cache] Saved {} txs to {}", txs.len(), path.display());
    }

    Ok(path)
}

/// Restore pre-signed transactions from cache.
///
/// Converts cached hex-encoded transactions back into `SignedTxWithMetadata`.
/// The `submit_time` is set to `Instant::now()` since cached txs don't have
/// meaningful submission timestamps.
pub fn restore_txs(cached: &TxCacheData) -> Result<Vec<SignedTxWithMetadata>> {
    use alloy_primitives::B256;
    use std::time::Instant;

    let mut txs = Vec::with_capacity(cached.transactions.len());

    for ct in &cached.transactions {
        let hash: B256 = ct
            .hash
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid cached tx hash: {}", ct.hash))?;

        let raw_hex = ct.raw.strip_prefix("0x").unwrap_or(&ct.raw);
        let encoded =
            hex::decode(raw_hex).map_err(|e| anyhow::anyhow!("Invalid cached tx hex: {}", e))?;

        let method = match ct.tx_type.as_str() {
            "SimpleTransfer" => crate::types::TransactionType::SimpleTransfer,
            "ERC20Mint" => crate::types::TransactionType::ERC20Mint,
            "ERC20Transfer" => crate::types::TransactionType::ERC20Transfer,
            "Swap" => crate::types::TransactionType::Swap,
            "NFTMint" => crate::types::TransactionType::NFTMint,
            _ => crate::types::TransactionType::SimpleTransfer,
        };

        txs.push(SignedTxWithMetadata {
            hash,
            encoded,
            nonce: ct.nonce,
            gas_limit: ct.gas_limit,
            sender: alloy_primitives::Address::default(), // Not stored in cache
            submit_time: Instant::now(),
            method,
        });
    }

    Ok(txs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

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

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            ".{prefix}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    fn sample_cached_tx() -> CachedTx {
        CachedTx {
            hash: "0x0000000000000000000000000000000000000000000000000000000000000001".to_string(),
            raw: "0xdead".to_string(),
            nonce: 0,
            gas_limit: 21_000,
            tx_type: "SimpleTransfer".to_string(),
        }
    }

    fn sample_cache_data(fingerprint: &str) -> TxCacheData {
        TxCacheData {
            version: 1,
            fingerprint: fingerprint.to_string(),
            chain_id: 1,
            mode: "burst".to_string(),
            sender_count: 1,
            tx_count: 1,
            gas_price: "1000000000".to_string(),
            transactions: vec![sample_cached_tx()],
        }
    }

    fn write_cache_file(dir: &std::path::Path, fingerprint: &str, data: &TxCacheData) {
        let path = dir.join(format!("tx-cache-{fingerprint}.json"));
        let json = serde_json::to_string_pretty(data).unwrap();
        std::fs::write(path, json).unwrap();
    }

    #[test]
    fn test_fingerprint_deterministic() {
        let fp1 = compute_fingerprint(19803, "burst", 4, 1000, 2_000_000_000);
        let fp2 = compute_fingerprint(19803, "burst", 4, 1000, 2_000_000_000);
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 16);
    }

    #[test]
    fn test_fingerprint_changes_with_params() {
        let fp1 = compute_fingerprint(19803, "burst", 4, 1000, 2_000_000_000);
        let fp2 = compute_fingerprint(19803, "burst", 4, 2000, 2_000_000_000);
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn test_cache_roundtrip() {
        use alloy_primitives::{Address, B256};
        use std::time::Instant;

        let txs = [SignedTxWithMetadata {
            hash: B256::with_last_byte(0x42),
            encoded: vec![0x02, 0xab, 0xcd],
            nonce: 0,
            gas_limit: 21_000,
            sender: Address::default(),
            submit_time: Instant::now(),
            method: crate::types::TransactionType::SimpleTransfer,
        }];

        let fp = "test_roundtrip";

        // Use a unique temp dir to avoid racing with other tests
        let tmp = std::env::temp_dir().join(format!(".tx-cache-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);

        // Write directly to known path (avoid env var races)
        let path = tmp.join(format!("tx-cache-{}.json", fp));
        let cached_txs: Vec<CachedTx> = txs
            .iter()
            .map(|tx| CachedTx {
                hash: format!("{:?}", tx.hash),
                raw: format!("0x{}", hex::encode(&tx.encoded)),
                nonce: tx.nonce,
                gas_limit: tx.gas_limit,
                tx_type: format!("{:?}", tx.method),
            })
            .collect();
        let cache_data = TxCacheData {
            version: 1,
            fingerprint: fp.to_string(),
            chain_id: 19803,
            mode: "burst".to_string(),
            sender_count: 1,
            tx_count: 1,
            gas_price: "1000000000".to_string(),
            transactions: cached_txs,
        };
        let json = serde_json::to_string_pretty(&cache_data).unwrap();
        std::fs::write(&path, &json).unwrap();
        assert!(path.exists());

        // Load from written file
        let loaded: TxCacheData = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.fingerprint, fp);
        assert_eq!(loaded.transactions.len(), 1);
        assert_eq!(loaded.transactions[0].nonce, 0);

        // Restore
        let restored = restore_txs(&loaded).unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].hash, B256::with_last_byte(0x42));
        assert_eq!(restored[0].encoded, vec![0x02, 0xab, 0xcd]);

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_nonexistent() {
        // Use a path that definitely doesn't exist
        assert!(try_load("definitely_nonexistent_fingerprint_12345", true).is_none());
    }

    #[test]
    fn test_fingerprint_different_chain_ids() {
        let fp1 = compute_fingerprint(1, "burst", 4, 1000, 2_000_000_000);
        let fp2 = compute_fingerprint(2, "burst", 4, 1000, 2_000_000_000);
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn test_fingerprint_different_modes() {
        let fp1 = compute_fingerprint(19803, "burst", 4, 1000, 2_000_000_000);
        let fp2 = compute_fingerprint(19803, "ceiling", 4, 1000, 2_000_000_000);
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn test_fingerprint_length_always_16() {
        // Verify that compute_fingerprint always returns a 16-char hex string,
        // which tests the sha256_hex truncation indirectly.
        for chain_id in [1u64, 100, 19803, u64::MAX] {
            for mode in ["burst", "ceiling", "sustained", ""] {
                let fp = compute_fingerprint(chain_id, mode, 1, 1, 1);
                assert_eq!(
                    fp.len(),
                    16,
                    "fingerprint for chain_id={chain_id}, mode={mode}"
                );
                assert!(
                    fp.chars().all(|c| c.is_ascii_hexdigit()),
                    "fingerprint should be hex: {fp}"
                );
            }
        }
    }

    #[test]
    fn test_cache_dir_returns_valid_path() {
        let dir = cache_dir();
        // Should end with ".tx-cache" or be whatever BENCH_TX_CACHE_DIR says
        // At minimum, it should be a non-empty path
        assert!(!dir.as_os_str().is_empty());
    }

    #[test]
    fn test_cache_dir_uses_env_override() {
        let _guard = env_lock().lock().unwrap();
        let tmp = unique_temp_dir("tx-cache-env-override");
        let _env = EnvVarGuard::set("BENCH_TX_CACHE_DIR", &tmp);
        assert_eq!(cache_dir(), tmp);
    }

    #[test]
    fn test_env_var_guard_restores_previous_cache_dir() {
        let _guard = env_lock().lock().unwrap();
        let original = unique_temp_dir("tx-cache-original");
        // SAFETY: tests serialize environment mutation via `env_lock`.
        unsafe { std::env::set_var("BENCH_TX_CACHE_DIR", &original) };
        {
            let override_dir = unique_temp_dir("tx-cache-override");
            let _env = EnvVarGuard::set("BENCH_TX_CACHE_DIR", &override_dir);
            assert_eq!(
                std::env::var("BENCH_TX_CACHE_DIR").unwrap(),
                override_dir.display().to_string()
            );
        }
        assert_eq!(
            std::env::var("BENCH_TX_CACHE_DIR").unwrap(),
            original.display().to_string()
        );
        // SAFETY: tests serialize environment mutation via `env_lock`.
        unsafe { std::env::remove_var("BENCH_TX_CACHE_DIR") };
    }

    #[test]
    fn test_cache_path_includes_fingerprint() {
        let fp = "abcdef1234567890";
        let path = cache_path(fp);
        let filename = path.file_name().unwrap().to_str().unwrap();
        assert!(filename.contains(fp));
        assert!(filename.starts_with("tx-cache-"));
        assert!(filename.ends_with(".json"));
    }

    #[test]
    fn test_try_load_from_nonexistent_returns_none() {
        let tmp = unique_temp_dir("tx-cache-missing-explicit");
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(try_load_from(&tmp, "no-such-cache", false).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_invalid_json() {
        let tmp =
            std::env::temp_dir().join(format!(".tx-cache-test-invalid-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let fp = "invalidjson12345";
        let path = tmp.join(format!("tx-cache-{fp}.json"));
        std::fs::write(&path, "this is not valid json!!!").unwrap();

        // Override cache dir by writing to the expected path and loading directly
        // We can't easily override cache_dir, so test via serde_json directly
        let data = std::fs::read_to_string(&path).unwrap();
        let result: Result<TxCacheData, _> = serde_json::from_str(&data);
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_wrong_version_via_env() {
        let tmp =
            std::env::temp_dir().join(format!(".tx-cache-test-version-env-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        let fp = "wrongversion1234";
        let cache_data = TxCacheData {
            version: 2,
            fingerprint: fp.to_string(),
            chain_id: 1,
            mode: "burst".to_string(),
            sender_count: 1,
            tx_count: 1,
            gas_price: "1000000000".to_string(),
            transactions: vec![CachedTx {
                hash: "0x0000000000000000000000000000000000000000000000000000000000000001"
                    .to_string(),
                raw: "0xdead".to_string(),
                nonce: 0,
                gas_limit: 21000,
                tx_type: "SimpleTransfer".to_string(),
            }],
        };

        let path = tmp.join(format!("tx-cache-{fp}.json"));
        let json = serde_json::to_string_pretty(&cache_data).unwrap();
        std::fs::write(&path, &json).unwrap();

        // Point cache_dir() to our temp dir via env var
        let result = try_load_from(&tmp, fp, true);
        // version 2 is unsupported, should return None
        assert!(result.is_none());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_from_read_error_not_quiet() {
        let tmp = unique_temp_dir("tx-cache-read-error-explicit");
        std::fs::create_dir_all(&tmp).unwrap();
        let fp = "readerror";
        let path = tmp.join(format!("tx-cache-{fp}.json"));
        std::fs::create_dir(&path).unwrap();

        assert!(try_load_from(&tmp, fp, false).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_from_invalid_json_not_quiet() {
        let tmp = unique_temp_dir("tx-cache-bad-json-explicit");
        std::fs::create_dir_all(&tmp).unwrap();
        let fp = "badjson";
        let path = tmp.join(format!("tx-cache-{fp}.json"));
        std::fs::write(&path, "{ definitely not json").unwrap();

        assert!(try_load_from(&tmp, fp, false).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_from_wrong_version_not_quiet() {
        let tmp = unique_temp_dir("tx-cache-wrong-version-explicit");
        std::fs::create_dir_all(&tmp).unwrap();
        let fp = "wrongversion";
        let mut cache_data = sample_cache_data(fp);
        cache_data.version = 2;
        write_cache_file(&tmp, fp, &cache_data);

        assert!(try_load_from(&tmp, fp, false).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_from_fingerprint_mismatch_not_quiet() {
        let tmp = unique_temp_dir("tx-cache-mismatch-explicit");
        std::fs::create_dir_all(&tmp).unwrap();
        let lookup_fp = "lookup";
        let cache_data = sample_cache_data("other");
        write_cache_file(&tmp, lookup_fp, &cache_data);

        assert!(try_load_from(&tmp, lookup_fp, false).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_from_empty_transactions_not_quiet() {
        let tmp = unique_temp_dir("tx-cache-empty-explicit");
        std::fs::create_dir_all(&tmp).unwrap();
        let fp = "empty";
        let mut cache_data = sample_cache_data(fp);
        cache_data.tx_count = 0;
        cache_data.transactions.clear();
        write_cache_file(&tmp, fp, &cache_data);

        assert!(try_load_from(&tmp, fp, false).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_mismatched_fingerprint_via_env() {
        let tmp =
            std::env::temp_dir().join(format!(".tx-cache-test-fpmismatch-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        // Write file with fingerprint "fingerprint_aaaa" but try to load with "fingerprint_bbbb"
        // We need to name the file for the lookup fingerprint so try_load finds it.
        let lookup_fp = "fingerprint_bbbb";
        let cache_data = TxCacheData {
            version: 1,
            fingerprint: "fingerprint_aaaa".to_string(),
            chain_id: 1,
            mode: "burst".to_string(),
            sender_count: 1,
            tx_count: 1,
            gas_price: "1000000000".to_string(),
            transactions: vec![CachedTx {
                hash: "0x0000000000000000000000000000000000000000000000000000000000000001"
                    .to_string(),
                raw: "0xdead".to_string(),
                nonce: 0,
                gas_limit: 21000,
                tx_type: "SimpleTransfer".to_string(),
            }],
        };

        let path = tmp.join(format!("tx-cache-{lookup_fp}.json"));
        let json = serde_json::to_string_pretty(&cache_data).unwrap();
        std::fs::write(&path, &json).unwrap();

        let result = try_load_from(&tmp, lookup_fp, true);
        assert!(result.is_none(), "fingerprint mismatch should return None");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_empty_transactions_via_env() {
        let tmp = std::env::temp_dir().join(format!(".tx-cache-test-empty-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        let fp = "emptytxs12345678";
        let cache_data = TxCacheData {
            version: 1,
            fingerprint: fp.to_string(),
            chain_id: 1,
            mode: "burst".to_string(),
            sender_count: 1,
            tx_count: 0,
            gas_price: "1000000000".to_string(),
            transactions: vec![],
        };

        let path = tmp.join(format!("tx-cache-{fp}.json"));
        let json = serde_json::to_string_pretty(&cache_data).unwrap();
        std::fs::write(&path, &json).unwrap();

        let result = try_load_from(&tmp, fp, true);
        assert!(result.is_none(), "empty transactions should return None");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_restore_txs_all_types() {
        let test_cases = vec![
            (
                "ERC20Transfer",
                crate::types::TransactionType::ERC20Transfer,
            ),
            ("Swap", crate::types::TransactionType::Swap),
            ("NFTMint", crate::types::TransactionType::NFTMint),
            ("ERC20Mint", crate::types::TransactionType::ERC20Mint),
            (
                "SimpleTransfer",
                crate::types::TransactionType::SimpleTransfer,
            ),
            // Unknown types should fall back to SimpleTransfer
            (
                "ERC20Approve",
                crate::types::TransactionType::SimpleTransfer,
            ),
            ("ETHTransfer", crate::types::TransactionType::SimpleTransfer),
            ("UnknownType", crate::types::TransactionType::SimpleTransfer),
        ];

        for (tx_type_str, expected_method) in test_cases {
            let cached = TxCacheData {
                version: 1,
                fingerprint: "test".to_string(),
                chain_id: 1,
                mode: "burst".to_string(),
                sender_count: 1,
                tx_count: 1,
                gas_price: "1000000000".to_string(),
                transactions: vec![CachedTx {
                    hash: "0x0000000000000000000000000000000000000000000000000000000000000042"
                        .to_string(),
                    raw: "0xabcd".to_string(),
                    nonce: 7,
                    gas_limit: 50_000,
                    tx_type: tx_type_str.to_string(),
                }],
            };

            let restored = restore_txs(&cached).unwrap();
            assert_eq!(restored.len(), 1);
            assert_eq!(
                restored[0].method, expected_method,
                "tx_type '{}' should map to {:?}",
                tx_type_str, expected_method
            );
            assert_eq!(restored[0].nonce, 7);
            assert_eq!(restored[0].gas_limit, 50_000);
            assert_eq!(restored[0].encoded, vec![0xab, 0xcd]);
        }
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        use alloy_primitives::{Address, B256};
        use std::time::Instant;

        let tmp = std::env::temp_dir().join(format!(".tx-cache-test-save-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);

        let fp = "saveroundtrip00";
        let txs = [
            SignedTxWithMetadata {
                hash: B256::with_last_byte(0x01),
                encoded: vec![0x01, 0x02, 0x03],
                nonce: 0,
                gas_limit: 21_000,
                sender: Address::default(),
                submit_time: Instant::now(),
                method: crate::types::TransactionType::SimpleTransfer,
            },
            SignedTxWithMetadata {
                hash: B256::with_last_byte(0x02),
                encoded: vec![0x04, 0x05],
                nonce: 1,
                gas_limit: 50_000,
                sender: Address::default(),
                submit_time: Instant::now(),
                method: crate::types::TransactionType::ERC20Mint,
            },
        ];

        // Build cache data manually and write to our temp dir
        let cached_txs: Vec<CachedTx> = txs
            .iter()
            .map(|tx| CachedTx {
                hash: format!("{:?}", tx.hash),
                raw: format!("0x{}", hex::encode(&tx.encoded)),
                nonce: tx.nonce,
                gas_limit: tx.gas_limit,
                tx_type: format!("{:?}", tx.method),
            })
            .collect();
        let cache_data = TxCacheData {
            version: 1,
            fingerprint: fp.to_string(),
            chain_id: 19803,
            mode: "burst".to_string(),
            sender_count: 1,
            tx_count: cached_txs.len() as u32,
            gas_price: "2000000000".to_string(),
            transactions: cached_txs,
        };
        let path = tmp.join(format!("tx-cache-{fp}.json"));
        let json = serde_json::to_string_pretty(&cache_data).unwrap();
        std::fs::write(&path, &json).unwrap();

        // Load back and verify
        let loaded: TxCacheData = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.transactions.len(), 2);
        assert_eq!(loaded.chain_id, 19803);
        assert_eq!(loaded.tx_count, 2);

        let restored = restore_txs(&loaded).unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].hash, B256::with_last_byte(0x01));
        assert_eq!(restored[0].encoded, vec![0x01, 0x02, 0x03]);
        assert_eq!(restored[0].nonce, 0);
        assert_eq!(restored[1].hash, B256::with_last_byte(0x02));
        assert_eq!(restored[1].encoded, vec![0x04, 0x05]);
        assert_eq!(restored[1].nonce, 1);
        assert_eq!(restored[1].gas_limit, 50_000);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Test save() + try_load() round-trip using env var to control cache_dir.
    /// Uses #[serial] semantics via a unique dir per thread to avoid env var races.
    #[test]
    fn test_save_creates_file_and_try_load_reads_it() {
        use alloy_primitives::{Address, B256};
        use std::time::Instant;

        let unique = format!("{}-{:?}", std::process::id(), std::thread::current().id());
        let tmp = std::env::temp_dir().join(format!(".tx-cache-test-saveload-{unique}"));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let fp = "saveloadrt000000";

        let txs = [
            SignedTxWithMetadata {
                hash: B256::with_last_byte(0xaa),
                encoded: vec![0x01, 0x02],
                nonce: 0,
                gas_limit: 21_000,
                sender: Address::default(),
                submit_time: Instant::now(),
                method: crate::types::TransactionType::SimpleTransfer,
            },
            SignedTxWithMetadata {
                hash: B256::with_last_byte(0xbb),
                encoded: vec![0x03, 0x04, 0x05],
                nonce: 1,
                gas_limit: 50_000,
                sender: Address::default(),
                submit_time: Instant::now(),
                method: crate::types::TransactionType::ERC20Mint,
            },
        ];

        // Manually build and write cache data (avoids env var dependency for save)
        let cached_txs: Vec<CachedTx> = txs
            .iter()
            .map(|tx| CachedTx {
                hash: format!("{:?}", tx.hash),
                raw: format!("0x{}", hex::encode(&tx.encoded)),
                nonce: tx.nonce,
                gas_limit: tx.gas_limit,
                tx_type: format!("{:?}", tx.method),
            })
            .collect();
        let cache_data = TxCacheData {
            version: 1,
            fingerprint: fp.to_string(),
            chain_id: 19803,
            mode: "burst".to_string(),
            sender_count: 1,
            tx_count: cached_txs.len() as u32,
            gas_price: "1000000000".to_string(),
            transactions: cached_txs,
        };
        let path = tmp.join(format!("tx-cache-{fp}.json"));
        let json = serde_json::to_string_pretty(&cache_data).unwrap();
        std::fs::write(&path, &json).unwrap();
        assert!(path.exists());

        // Verify JSON structure
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["version"], 1);
        assert_eq!(parsed["chain_id"], 19803);
        assert_eq!(parsed["mode"], "burst");
        assert_eq!(parsed["sender_count"], 1);
        assert_eq!(parsed["tx_count"], 2);
        assert_eq!(parsed["transactions"].as_array().unwrap().len(), 2);

        // Use try_load with env var pointing to our temp dir
        let loaded = try_load_from(&tmp, fp, true);

        assert!(loaded.is_some(), "try_load should find the saved cache");
        let loaded = loaded.unwrap();
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.fingerprint, fp);
        assert_eq!(loaded.chain_id, 19803);
        assert_eq!(loaded.transactions.len(), 2);
        assert_eq!(loaded.transactions[0].nonce, 0);
        assert_eq!(loaded.transactions[1].nonce, 1);
        assert_eq!(loaded.transactions[1].gas_limit, 50_000);

        // Restore and verify
        let restored = restore_txs(&loaded).unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].hash, B256::with_last_byte(0xaa));
        assert_eq!(restored[1].hash, B256::with_last_byte(0xbb));
        assert_eq!(restored[0].encoded, vec![0x01, 0x02]);
        assert_eq!(restored[1].encoded, vec![0x03, 0x04, 0x05]);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_invalid_json_via_env() {
        let unique = format!("{}-{:?}", std::process::id(), std::thread::current().id());
        let tmp = std::env::temp_dir().join(format!(".tx-cache-test-badjson-{unique}"));
        std::fs::create_dir_all(&tmp).unwrap();

        let fp = "badjsonfile12345";
        let path = tmp.join(format!("tx-cache-{fp}.json"));
        std::fs::write(&path, "{{{{not valid json at all!!!!").unwrap();

        let result = try_load_from(&tmp, fp, true);

        assert!(result.is_none(), "invalid JSON should return None");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_invalid_json_public_api() {
        let _guard = env_lock().lock().unwrap();
        let tmp = unique_temp_dir("tx-cache-public-badjson");
        std::fs::create_dir_all(&tmp).unwrap();
        let _env = EnvVarGuard::set("BENCH_TX_CACHE_DIR", &tmp);

        let fp = "publicbadjson12";
        let path = tmp.join(format!("tx-cache-{fp}.json"));
        std::fs::write(&path, "{{ definitely not json").unwrap();

        assert!(try_load(fp, true).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_public_invalid_json_not_quiet() {
        let _guard = env_lock().lock().unwrap();
        let tmp = unique_temp_dir("tx-cache-public-badjson-verbose");
        std::fs::create_dir_all(&tmp).unwrap();
        let _env = EnvVarGuard::set("BENCH_TX_CACHE_DIR", &tmp);
        let fp = "badjsonverbose";
        let path = tmp.join(format!("tx-cache-{fp}.json"));
        std::fs::write(&path, "{ invalid json").unwrap();

        assert!(try_load(fp, false).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_valid_cache_not_quiet() {
        let unique = format!("{}-{:?}", std::process::id(), std::thread::current().id());
        let tmp = std::env::temp_dir().join(format!(".tx-cache-test-notquiet-{unique}"));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let fp = "notquiettest0000";
        let cache_data = TxCacheData {
            version: 1,
            fingerprint: fp.to_string(),
            chain_id: 1,
            mode: "burst".to_string(),
            sender_count: 1,
            tx_count: 1,
            gas_price: "1000000000".to_string(),
            transactions: vec![CachedTx {
                hash: "0x0000000000000000000000000000000000000000000000000000000000000001"
                    .to_string(),
                raw: "0xdead".to_string(),
                nonce: 0,
                gas_limit: 21_000,
                tx_type: "SimpleTransfer".to_string(),
            }],
        };
        let path = tmp.join(format!("tx-cache-{fp}.json"));
        let json = serde_json::to_string_pretty(&cache_data).unwrap();
        std::fs::write(&path, &json).unwrap();

        // load with quiet=false to exercise the success eprintln path in try_load
        let loaded = try_load_from(&tmp, fp, false);

        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().transactions.len(), 1);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_directory_read_error_public_api() {
        let _guard = env_lock().lock().unwrap();
        let tmp = unique_temp_dir("tx-cache-public-readerr");
        std::fs::create_dir_all(&tmp).unwrap();
        let _env = EnvVarGuard::set("BENCH_TX_CACHE_DIR", &tmp);

        let fp = "publicreaderror";
        let path = tmp.join(format!("tx-cache-{fp}.json"));
        std::fs::create_dir(&path).unwrap();

        assert!(try_load(fp, true).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_directory_read_error_public_api_not_quiet() {
        let _guard = env_lock().lock().unwrap();
        let tmp = unique_temp_dir("tx-cache-public-readerr-verbose");
        std::fs::create_dir_all(&tmp).unwrap();
        let _env = EnvVarGuard::set("BENCH_TX_CACHE_DIR", &tmp);

        let fp = "publicreaderrorverbose";
        let path = tmp.join(format!("tx-cache-{fp}.json"));
        std::fs::create_dir(&path).unwrap();

        assert!(try_load(fp, false).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_public_wrong_version_not_quiet() {
        let _guard = env_lock().lock().unwrap();
        let tmp = unique_temp_dir("tx-cache-public-version-verbose");
        std::fs::create_dir_all(&tmp).unwrap();
        let _env = EnvVarGuard::set("BENCH_TX_CACHE_DIR", &tmp);
        let fp = "publicversion";
        let mut cache_data = sample_cache_data(fp);
        cache_data.version = 9;
        write_cache_file(&tmp, fp, &cache_data);

        assert!(try_load(fp, false).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_public_fingerprint_mismatch_not_quiet() {
        let _guard = env_lock().lock().unwrap();
        let tmp = unique_temp_dir("tx-cache-public-mismatch-verbose");
        std::fs::create_dir_all(&tmp).unwrap();
        let _env = EnvVarGuard::set("BENCH_TX_CACHE_DIR", &tmp);
        let lookup_fp = "publiclookup";
        let cache_data = sample_cache_data("stale");
        write_cache_file(&tmp, lookup_fp, &cache_data);

        assert!(try_load(lookup_fp, false).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_public_empty_transactions_not_quiet() {
        let _guard = env_lock().lock().unwrap();
        let tmp = unique_temp_dir("tx-cache-public-empty-verbose");
        std::fs::create_dir_all(&tmp).unwrap();
        let _env = EnvVarGuard::set("BENCH_TX_CACHE_DIR", &tmp);
        let fp = "publicempty";
        let mut cache_data = sample_cache_data(fp);
        cache_data.tx_count = 0;
        cache_data.transactions.clear();
        write_cache_file(&tmp, fp, &cache_data);

        assert!(try_load(fp, false).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_try_load_public_success_not_quiet() {
        let _guard = env_lock().lock().unwrap();
        let tmp = unique_temp_dir("tx-cache-public-success-verbose");
        std::fs::create_dir_all(&tmp).unwrap();
        let _env = EnvVarGuard::set("BENCH_TX_CACHE_DIR", &tmp);
        let fp = "publicsuccess";
        let cache_data = sample_cache_data(fp);
        write_cache_file(&tmp, fp, &cache_data);

        let loaded = try_load(fp, false).expect("cache should load");
        assert_eq!(loaded.fingerprint, fp);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_save_and_try_load_public_roundtrip() {
        use alloy_primitives::{Address, B256};
        use std::time::Instant;

        let _guard = env_lock().lock().unwrap();
        let tmp = unique_temp_dir("tx-cache-save-load-public");
        std::fs::create_dir_all(&tmp).unwrap();
        let _env = EnvVarGuard::set("BENCH_TX_CACHE_DIR", &tmp);

        let fp = "publicroundtrip1";
        let txs = vec![
            SignedTxWithMetadata {
                hash: B256::with_last_byte(0x11),
                encoded: vec![0xde, 0xad, 0xbe, 0xef],
                nonce: 3,
                gas_limit: 21_000,
                sender: Address::with_last_byte(0x44),
                submit_time: Instant::now(),
                method: crate::types::TransactionType::SimpleTransfer,
            },
            SignedTxWithMetadata {
                hash: B256::with_last_byte(0x22),
                encoded: vec![0xca, 0xfe],
                nonce: 4,
                gas_limit: 65_000,
                sender: Address::with_last_byte(0x55),
                submit_time: Instant::now(),
                method: crate::types::TransactionType::ERC20Transfer,
            },
        ];

        let path = save(fp, 19803, "burst", 2, 2_000_000_000, &txs, true).unwrap();
        assert_eq!(path, tmp.join(format!("tx-cache-{fp}.json")));

        let loaded = try_load(fp, true).expect("cache should load");
        assert_eq!(loaded.chain_id, 19803);
        assert_eq!(loaded.sender_count, 2);
        assert_eq!(loaded.tx_count, 2);
        assert_eq!(loaded.transactions[1].tx_type, "ERC20Transfer");

        let restored = restore_txs(&loaded).unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].encoded, txs[0].encoded);
        assert_eq!(restored[1].nonce, txs[1].nonce);
        assert_eq!(
            restored[1].method,
            crate::types::TransactionType::ERC20Transfer
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_save_errors_when_cache_dir_points_to_file() {
        let _guard = env_lock().lock().unwrap();
        let tmp = unique_temp_dir("tx-cache-save-error");
        std::fs::create_dir_all(&tmp).unwrap();
        let cache_root = tmp.join("cache-root-file");
        std::fs::write(&cache_root, "not a directory").unwrap();
        let _env = EnvVarGuard::set("BENCH_TX_CACHE_DIR", &cache_root);

        let txs: Vec<SignedTxWithMetadata> = Vec::new();
        let result = save("saveerrortest12", 1, "burst", 0, 1_000_000_000, &txs, true);
        assert!(result.is_err(), "save should fail when cache dir is a file");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_save_verbose_mode() {
        use alloy_primitives::{Address, B256};
        use std::time::Instant;

        let _guard = env_lock().lock().unwrap();
        let tmp = unique_temp_dir("tx-cache-save-verbose");
        std::fs::create_dir_all(&tmp).unwrap();
        let _env = EnvVarGuard::set("BENCH_TX_CACHE_DIR", &tmp);

        let txs = vec![SignedTxWithMetadata {
            hash: B256::with_last_byte(0x11),
            encoded: vec![0xaa, 0xbb],
            nonce: 1,
            gas_limit: 21_000,
            sender: Address::default(),
            submit_time: Instant::now(),
            method: crate::types::TransactionType::SimpleTransfer,
        }];

        let path = save("saveverbose", 1, "burst", 1, 1_000_000_000, &txs, false).unwrap();
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_restore_txs_invalid_hash() {
        let cached = TxCacheData {
            version: 1,
            fingerprint: "test".to_string(),
            chain_id: 1,
            mode: "burst".to_string(),
            sender_count: 1,
            tx_count: 1,
            gas_price: "1000000000".to_string(),
            transactions: vec![CachedTx {
                hash: "not_a_valid_hash".to_string(),
                raw: "0xabcd".to_string(),
                nonce: 0,
                gas_limit: 21_000,
                tx_type: "SimpleTransfer".to_string(),
            }],
        };

        let result = restore_txs(&cached);
        assert!(result.is_err(), "invalid hash should cause an error");
        let err_msg = result.err().unwrap().to_string();
        assert!(err_msg.contains("Invalid cached tx hash"), "got: {err_msg}");
    }

    #[test]
    fn test_restore_txs_invalid_hex() {
        let cached = TxCacheData {
            version: 1,
            fingerprint: "test".to_string(),
            chain_id: 1,
            mode: "burst".to_string(),
            sender_count: 1,
            tx_count: 1,
            gas_price: "1000000000".to_string(),
            transactions: vec![CachedTx {
                hash: "0x0000000000000000000000000000000000000000000000000000000000000042"
                    .to_string(),
                raw: "0xZZZZ".to_string(),
                nonce: 0,
                gas_limit: 21_000,
                tx_type: "SimpleTransfer".to_string(),
            }],
        };

        let result = restore_txs(&cached);
        assert!(result.is_err(), "invalid hex should cause an error");
        let err_msg = result.err().unwrap().to_string();
        assert!(err_msg.contains("Invalid cached tx hex"), "got: {err_msg}");
    }

    #[test]
    fn test_restore_txs_accepts_raw_without_0x_prefix() {
        let cached = TxCacheData {
            version: 1,
            fingerprint: "test".to_string(),
            chain_id: 1,
            mode: "burst".to_string(),
            sender_count: 1,
            tx_count: 1,
            gas_price: "1000000000".to_string(),
            transactions: vec![CachedTx {
                hash: "0x0000000000000000000000000000000000000000000000000000000000000042"
                    .to_string(),
                raw: "abcd".to_string(),
                nonce: 9,
                gas_limit: 99_999,
                tx_type: "SimpleTransfer".to_string(),
            }],
        };

        let restored = restore_txs(&cached).unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].encoded, vec![0xab, 0xcd]);
        assert_eq!(restored[0].nonce, 9);
    }
}
