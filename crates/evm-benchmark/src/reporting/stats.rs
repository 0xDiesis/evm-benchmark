//! Statistical computation functions for benchmark latency reporting.

use crate::types::LatencyStats;

/// Compute a percentile value from a **sorted** slice of latency samples.
///
/// `p` should be in the range `0.0..=1.0` (e.g. 0.95 for p95).
/// Returns `0` when the slice is empty.
pub fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 * p) as usize).min(sorted.len() - 1);
    sorted[idx]
}

/// Compute the arithmetic mean of a latency slice.
///
/// Returns `0` when the slice is empty.
pub fn mean(values: &[u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let sum: u64 = values.iter().sum();
    sum / values.len() as u64
}

/// Compute the population standard deviation of a latency slice.
///
/// Returns `0.0` when the slice is empty.
pub fn std_dev(values: &[u64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let avg = mean(values) as f64;
    let variance = values
        .iter()
        .map(|&v| (v as f64 - avg).powi(2))
        .sum::<f64>()
        / values.len() as f64;
    variance.sqrt()
}

/// Return the minimum value, or `0` for an empty slice.
pub fn min(values: &[u64]) -> u64 {
    values.iter().copied().min().unwrap_or(0)
}

/// Return the maximum value, or `0` for an empty slice.
pub fn max(values: &[u64]) -> u64 {
    values.iter().copied().max().unwrap_or(0)
}

/// Build a [`LatencyStats`] from an unsorted slice of latency samples (in ms).
///
/// The input slice is cloned and sorted internally. Returns zeroed stats when
/// the input is empty.
pub fn compute_latency_stats(latencies: &[u64]) -> LatencyStats {
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

    let mut sorted = latencies.to_vec();
    sorted.sort_unstable();

    LatencyStats {
        p50: percentile(&sorted, 0.50),
        p95: percentile(&sorted, 0.95),
        p99: percentile(&sorted, 0.99),
        min: sorted[0],
        max: sorted[sorted.len() - 1],
        avg: mean(&sorted),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_percentile_empty() {
        assert_eq!(percentile(&[], 0.50), 0);
    }

    #[test]
    fn test_percentile_single() {
        assert_eq!(percentile(&[42], 0.50), 42);
        assert_eq!(percentile(&[42], 0.99), 42);
    }

    #[test]
    fn test_percentile_known_values() {
        // 1..=100 sorted — idx = floor(len * p), 0-indexed
        let values: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&values, 0.50), 51);
        assert_eq!(percentile(&values, 0.95), 96);
        assert_eq!(percentile(&values, 0.99), 100);
    }

    #[test]
    fn test_mean_empty() {
        assert_eq!(mean(&[]), 0);
    }

    #[test]
    fn test_mean_values() {
        assert_eq!(mean(&[10, 20, 30]), 20);
        assert_eq!(mean(&[1, 2, 3, 4]), 2); // integer division: 10/4 = 2
    }

    #[test]
    fn test_std_dev_empty() {
        assert!((std_dev(&[]) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_std_dev_uniform() {
        // All same values => std dev = 0
        assert!((std_dev(&[5, 5, 5, 5]) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_std_dev_known() {
        // Population std dev of [2, 4, 4, 4, 5, 5, 7, 9] = 2.0
        let values = [2, 4, 4, 4, 5, 5, 7, 9];
        let sd = std_dev(&values);
        assert!((sd - 2.0).abs() < 0.01, "expected ~2.0, got {sd}");
    }

    #[test]
    fn test_min_max() {
        assert_eq!(min(&[]), 0);
        assert_eq!(max(&[]), 0);
        assert_eq!(min(&[3, 1, 4, 1, 5]), 1);
        assert_eq!(max(&[3, 1, 4, 1, 5]), 5);
    }

    #[test]
    fn test_compute_latency_stats_empty() {
        let stats = compute_latency_stats(&[]);
        assert_eq!(stats.p50, 0);
        assert_eq!(stats.avg, 0);
        assert_eq!(stats.min, 0);
        assert_eq!(stats.max, 0);
    }

    #[test]
    fn test_compute_latency_stats_unsorted_input() {
        let latencies = vec![100, 10, 50, 200, 150, 30, 80, 5, 90, 300];
        let stats = compute_latency_stats(&latencies);

        assert_eq!(stats.min, 5);
        assert_eq!(stats.max, 300);
        // Sorted: [5, 10, 30, 50, 80, 90, 100, 150, 200, 300]
        // p50: idx = (10 * 0.5) = 5 => 90
        assert_eq!(stats.p50, 90);
        // p95: idx = (10 * 0.95) = 9 => 300
        assert_eq!(stats.p95, 300);
        // p99: idx = min(9, 9) => 300
        assert_eq!(stats.p99, 300);
        // avg: 1015 / 10 = 101
        assert_eq!(stats.avg, 101);
    }

    #[test]
    fn test_compute_latency_stats_single() {
        let stats = compute_latency_stats(&[42]);
        assert_eq!(stats.min, 42);
        assert_eq!(stats.max, 42);
        assert_eq!(stats.p50, 42);
        assert_eq!(stats.avg, 42);
    }

    #[test]
    fn test_compute_latency_stats_large_range() {
        let latencies: Vec<u64> = (1..=1000).collect();
        let stats = compute_latency_stats(&latencies);
        assert_eq!(stats.min, 1);
        assert_eq!(stats.max, 1000);
        assert_eq!(stats.p50, 501);
        assert_eq!(stats.p95, 951);
        assert_eq!(stats.p99, 991);
        assert_eq!(stats.avg, 500);
    }
}
