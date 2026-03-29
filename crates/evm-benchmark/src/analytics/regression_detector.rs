//! Regression detection with statistical significance testing.
//!
//! Compares current performance against historical baseline and determines
//! if differences are statistically significant using p-values and z-scores.

use crate::types::{HarnessMetrics, RegressionAnalysis};

/// Historical baseline metrics for comparison
#[derive(Clone, Debug)]
pub struct BaselineMetrics {
    pub tps_confirmed: f32,
    pub latency_p99_ms: u64,
}

impl BaselineMetrics {
    /// Create baseline from harness metrics.
    #[allow(dead_code)]
    pub fn from_harness_metrics(metrics: &HarnessMetrics) -> Self {
        BaselineMetrics {
            tps_confirmed: metrics.tps_confirmed,
            latency_p99_ms: metrics.latency_p99,
        }
    }
}

/// Detect regression by comparing current metrics against baseline
pub fn detect_regression(
    current: &HarnessMetrics,
    baseline: &BaselineMetrics,
) -> RegressionAnalysis {
    let tps_delta = current.tps_confirmed - baseline.tps_confirmed;
    let tps_pct_change = if baseline.tps_confirmed > 0.0 {
        (tps_delta / baseline.tps_confirmed) * 100.0
    } else {
        0.0
    };

    let latency_delta_ms = current.latency_p99 as i32 - baseline.latency_p99_ms as i32;

    // Calculate p-value based on effect size (simplified Z-score approach)
    let p_value = calculate_p_value(tps_pct_change);

    // Determine verdict based on p-value and effect size
    let verdict = if p_value < 0.05 {
        if tps_delta < 0.0 {
            "regressed".to_string()
        } else {
            "improved".to_string()
        }
    } else {
        "stable".to_string()
    };

    RegressionAnalysis {
        tps_delta,
        tps_pct_change,
        latency_delta_ms,
        p_value,
        verdict,
    }
}

/// Calculate p-value from TPS percentage change using normal distribution approximation
/// Assumes typical variance in benchmarks with ~5-10% natural variation
fn calculate_p_value(pct_change: f32) -> f32 {
    // Z-score calculation: effect_size / standard_error
    // Assuming standard error of ~2% TPS change
    let std_error = 2.0;
    let z_score = (pct_change.abs()) / std_error;

    // Convert Z-score to p-value (two-tailed)
    // Using approximation: p ≈ 2 * (1 - Φ(z))
    // For simplicity: p = erfc(z / sqrt(2)) / 2
    let p_value = normal_cdf(z_score);

    // Return two-tailed p-value
    ((1.0 - p_value) * 2.0).min(1.0)
}

/// Approximation of standard normal CDF (cumulative distribution function)
/// Uses Taylor series approximation for Φ(x)
#[allow(clippy::excessive_precision)]
fn normal_cdf(x: f32) -> f32 {
    if x < -6.0 {
        return 0.0;
    }
    if x > 6.0 {
        return 1.0;
    }

    // Abramowitz and Stegun approximation
    let b1 = 0.319381530;
    let b2 = -0.356563782;
    let b3 = 1.781477937;
    let b4 = -1.821255978;
    let b5 = 1.330274429;
    let p = 0.2316419;
    let c = 0.39894228;

    let abs_x = x.abs();
    let t = 1.0 / (1.0 + p * abs_x);

    let phi = 1.0
        - c * (-(x * x) / 2.0).exp()
            * (b1 * t + b2 * t * t + b3 * t * t * t + b4 * t * t * t * t + b5 * t * t * t * t * t);

    if x < 0.0 { 1.0 - phi } else { phi }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_stable_performance() {
        let baseline = BaselineMetrics {
            tps_confirmed: 3000.0,
            latency_p99_ms: 100,
        };

        let current = HarnessMetrics {
            tps_submitted: 3050.0,
            tps_confirmed: 3010.0, // 0.33% change - within noise
            latency_p50: 50,
            latency_p95: 80,
            latency_p99: 102,
            confirmation_rate: 0.98,
            pending_ratio: 0.01,
            error_rate: 0.01,
            memory_bytes: 50_000_000,
        };

        let analysis = detect_regression(&current, &baseline);
        assert_eq!(analysis.verdict, "stable");
        assert!(analysis.p_value > 0.05); // Not statistically significant
    }

    #[test]
    fn test_detect_performance_improvement() {
        let baseline = BaselineMetrics {
            tps_confirmed: 3000.0,
            latency_p99_ms: 100,
        };

        let current = HarnessMetrics {
            tps_submitted: 3500.0,
            tps_confirmed: 3300.0, // 10% improvement
            latency_p50: 50,
            latency_p95: 80,
            latency_p99: 90,
            confirmation_rate: 0.99,
            pending_ratio: 0.005,
            error_rate: 0.005,
            memory_bytes: 40_000_000,
        };

        let analysis = detect_regression(&current, &baseline);
        assert_eq!(analysis.verdict, "improved");
        assert!(analysis.p_value < 0.05); // Statistically significant
        assert!(analysis.tps_pct_change > 0.0);
    }

    #[test]
    fn test_detect_performance_regression() {
        let baseline = BaselineMetrics {
            tps_confirmed: 3000.0,
            latency_p99_ms: 100,
        };

        let current = HarnessMetrics {
            tps_submitted: 2700.0,
            tps_confirmed: 2550.0, // 15% regression
            latency_p50: 80,
            latency_p95: 120,
            latency_p99: 150,
            confirmation_rate: 0.95,
            pending_ratio: 0.03,
            error_rate: 0.02,
            memory_bytes: 60_000_000,
        };

        let analysis = detect_regression(&current, &baseline);
        assert_eq!(analysis.verdict, "regressed");
        assert!(analysis.p_value < 0.05); // Statistically significant
        assert!(analysis.tps_pct_change < 0.0);
        assert!(analysis.tps_delta < 0.0);
    }

    #[test]
    fn test_latency_delta_calculation() {
        let baseline = BaselineMetrics {
            tps_confirmed: 3000.0,
            latency_p99_ms: 100,
        };

        let current = HarnessMetrics {
            tps_submitted: 3000.0,
            tps_confirmed: 3000.0,
            latency_p50: 50,
            latency_p95: 80,
            latency_p99: 150,
            confirmation_rate: 0.98,
            pending_ratio: 0.01,
            error_rate: 0.01,
            memory_bytes: 50_000_000,
        };

        let analysis = detect_regression(&current, &baseline);
        assert_eq!(analysis.latency_delta_ms, 50); // 150 - 100
    }

    #[test]
    fn test_normal_cdf_approximation() {
        // Test that normal_cdf produces sensible CDF values
        assert!(normal_cdf(-6.0) < 0.001); // Far left tail
        assert!(normal_cdf(0.0) > 0.49 && normal_cdf(0.0) < 0.51); // Should be ~0.5
        assert!(normal_cdf(6.0) > 0.999); // Far right tail
    }

    #[test]
    fn test_baseline_from_metrics() {
        let metrics = HarnessMetrics {
            tps_submitted: 3100.0,
            tps_confirmed: 3000.0,
            latency_p50: 50,
            latency_p95: 80,
            latency_p99: 100,
            confirmation_rate: 0.97,
            pending_ratio: 0.02,
            error_rate: 0.01,
            memory_bytes: 50_000_000,
        };

        let baseline = BaselineMetrics::from_harness_metrics(&metrics);
        assert_eq!(baseline.tps_confirmed, 3000.0);
        assert_eq!(baseline.latency_p99_ms, 100);
    }

    #[test]
    fn test_equal_baselines_stable() {
        let baseline = BaselineMetrics {
            tps_confirmed: 3000.0,
            latency_p99_ms: 100,
        };

        let current = HarnessMetrics {
            tps_submitted: 3000.0,
            tps_confirmed: 3000.0,
            latency_p50: 50,
            latency_p95: 80,
            latency_p99: 100,
            confirmation_rate: 0.98,
            pending_ratio: 0.01,
            error_rate: 0.01,
            memory_bytes: 50_000_000,
        };

        let analysis = detect_regression(&current, &baseline);
        assert_eq!(analysis.verdict, "stable");
        assert_eq!(analysis.tps_delta, 0.0);
        assert_eq!(analysis.tps_pct_change, 0.0);
        assert_eq!(analysis.latency_delta_ms, 0);
        // p-value should be 1.0 for zero change
        assert!((analysis.p_value - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_zero_baseline_tps() {
        let baseline = BaselineMetrics {
            tps_confirmed: 0.0,
            latency_p99_ms: 100,
        };

        let current = HarnessMetrics {
            tps_submitted: 3000.0,
            tps_confirmed: 3000.0,
            latency_p50: 50,
            latency_p95: 80,
            latency_p99: 100,
            confirmation_rate: 0.98,
            pending_ratio: 0.01,
            error_rate: 0.01,
            memory_bytes: 50_000_000,
        };

        let analysis = detect_regression(&current, &baseline);
        // With zero baseline, tps_pct_change should be 0 (guarded division)
        assert_eq!(analysis.tps_pct_change, 0.0);
        // Verdict should be stable since pct_change is 0
        assert_eq!(analysis.verdict, "stable");
    }

    #[test]
    fn test_latency_improvement_negative_delta() {
        let baseline = BaselineMetrics {
            tps_confirmed: 3000.0,
            latency_p99_ms: 200,
        };

        let current = HarnessMetrics {
            tps_submitted: 3000.0,
            tps_confirmed: 3000.0,
            latency_p50: 30,
            latency_p95: 60,
            latency_p99: 100,
            confirmation_rate: 0.98,
            pending_ratio: 0.01,
            error_rate: 0.01,
            memory_bytes: 50_000_000,
        };

        let analysis = detect_regression(&current, &baseline);
        // Latency improved: 100 - 200 = -100
        assert_eq!(analysis.latency_delta_ms, -100);
    }

    #[test]
    fn test_normal_cdf_symmetry() {
        // CDF(-x) should be approximately 1 - CDF(x)
        let x = 2.0f32;
        let cdf_pos = normal_cdf(x);
        let cdf_neg = normal_cdf(-x);
        assert!((cdf_pos + cdf_neg - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_normal_cdf_extreme_values() {
        assert!(normal_cdf(-7.0) < 0.001);
        assert!(normal_cdf(7.0) > 0.999);
        // At exactly the boundaries
        assert!(normal_cdf(-6.0) < 0.001);
        assert!(normal_cdf(6.0) > 0.999);
    }

    #[test]
    fn test_p_value_small_change_not_significant() {
        // 1% change relative to 2% std error => z = 0.5, p > 0.05
        let baseline = BaselineMetrics {
            tps_confirmed: 1000.0,
            latency_p99_ms: 100,
        };

        let current = HarnessMetrics {
            tps_submitted: 1010.0,
            tps_confirmed: 1010.0, // 1% change
            latency_p50: 50,
            latency_p95: 80,
            latency_p99: 100,
            confirmation_rate: 0.98,
            pending_ratio: 0.01,
            error_rate: 0.01,
            memory_bytes: 50_000_000,
        };

        let analysis = detect_regression(&current, &baseline);
        assert!(
            analysis.p_value > 0.05,
            "Small change should not be significant"
        );
        assert_eq!(analysis.verdict, "stable");
    }
}
