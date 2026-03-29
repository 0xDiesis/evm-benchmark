//! Unified metrics collection from harness and Prometheus server metrics.

use crate::analytics::prometheus_integration::MetricsMap;
use crate::types::{BurstResult, HarnessMetrics, PerformanceSnapshot, UnifiedServerMetrics};
use chrono::Utc;

/// Extract harness metrics from a benchmark result
pub fn extract_harness_metrics(result: &BurstResult) -> HarnessMetrics {
    let confirmed = result.confirmed as f32;
    let submitted = result.submitted as f32;

    let tps_submitted = result.submitted_tps;
    let tps_confirmed = result.confirmed_tps;

    let confirmation_rate = if submitted > 0.0 {
        confirmed / submitted
    } else {
        0.0
    };

    let pending_ratio = if submitted > 0.0 {
        (result.pending as f32) / submitted
    } else {
        0.0
    };

    // Calculate error rate - we infer it from pending and confirmed vs submitted
    // error_rate = 1.0 - (confirmed + pending) / submitted
    let total_accounted = (result.confirmed + result.pending) as f32;
    let error_rate = if result.submitted > 0 {
        ((result.submitted as f32) - total_accounted) / (result.submitted as f32)
    } else {
        0.0
    };

    HarnessMetrics {
        tps_submitted,
        tps_confirmed,
        latency_p50: result.latency.p50,
        latency_p95: result.latency.p95,
        latency_p99: result.latency.p99,
        confirmation_rate,
        pending_ratio,
        error_rate,
        memory_bytes: 0, // Not tracked in BurstResult currently
    }
}

/// Extract server metrics from Prometheus snapshots
pub fn extract_server_metrics(before: &MetricsMap, after: &MetricsMap) -> UnifiedServerMetrics {
    let get_delta = |metric: &str| -> u64 {
        match (before.get(metric), after.get(metric)) {
            (Some(b), Some(a)) => ((a - b).max(0.0)) as u64,
            _ => 0,
        }
    };

    let get_delta_u32 = |metric: &str| -> u32 {
        match (before.get(metric), after.get(metric)) {
            (Some(b), Some(a)) => ((a - b).max(0.0)) as u32,
            _ => 0,
        }
    };

    UnifiedServerMetrics {
        block_execution_ms: get_delta("reth_diesis_pipeline_execution_ms"),
        state_root_ms: get_delta("reth_diesis_pipeline_state_root_ms"),
        parent_handoff_ms: get_delta("reth_diesis_pipeline_parent_handoff_wait_ms"),
        publication_ms: get_delta("reth_diesis_pipeline_publication_ms"),
        queue_wait_ms: get_delta("reth_diesis_pipeline_queue_wait_ms"),
        gas_per_block: get_delta("reth_node_gas_per_block"),
        transactions_per_block: get_delta_u32("reth_diesis_transactions_per_block"),
        memory_usage_mb: get_delta("reth_node_memory_usage_mb"),
    }
}

/// Combine harness and server metrics into a unified PerformanceSnapshot
pub fn create_performance_snapshot(
    harness_metrics: HarnessMetrics,
    server_metrics: UnifiedServerMetrics,
) -> PerformanceSnapshot {
    PerformanceSnapshot {
        timestamp: Utc::now(),
        harness_metrics,
        server_metrics,
        correlation_confidence: 0.95, // Will be refined by analysis engine
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LatencyStats;
    use std::collections::HashMap;

    #[test]
    fn test_extract_harness_metrics() {
        let result = BurstResult {
            submitted: 1000,
            confirmed: 980,
            pending: 10,
            sign_ms: 50,
            submit_ms: 100,
            confirm_ms: 200,
            submitted_tps: 10000.0,
            confirmed_tps: 9800.0,
            latency: LatencyStats {
                p50: 50,
                p95: 150,
                p99: 200,
                min: 10,
                max: 300,
                avg: 100,
            },
            server_metrics: None,
            per_method: None,
            validator_health: None,
            per_wave: None,
        };

        let metrics = extract_harness_metrics(&result);
        assert_eq!(metrics.latency_p50, 50);
        assert_eq!(metrics.latency_p95, 150);
        assert_eq!(metrics.latency_p99, 200);
        assert!(metrics.tps_submitted > 0.0);
        assert!(metrics.tps_confirmed > 0.0);
        assert!(metrics.confirmation_rate > 0.0);
        assert!(metrics.confirmation_rate <= 1.0);
    }

    #[test]
    fn test_extract_server_metrics() {
        let mut before = HashMap::new();
        before.insert("reth_diesis_pipeline_execution_ms".to_string(), 1000.0);
        before.insert("reth_diesis_pipeline_state_root_ms".to_string(), 500.0);

        let mut after = HashMap::new();
        after.insert("reth_diesis_pipeline_execution_ms".to_string(), 1500.0);
        after.insert("reth_diesis_pipeline_state_root_ms".to_string(), 700.0);

        let metrics = extract_server_metrics(&before, &after);
        assert_eq!(metrics.block_execution_ms, 500);
        assert_eq!(metrics.state_root_ms, 200);
    }

    #[test]
    fn test_create_performance_snapshot() {
        let harness = HarnessMetrics {
            tps_submitted: 3000.0,
            tps_confirmed: 2950.0,
            latency_p50: 10,
            latency_p95: 50,
            latency_p99: 100,
            confirmation_rate: 0.98,
            pending_ratio: 0.01,
            error_rate: 0.01,
            memory_bytes: 50_000_000,
        };

        let server = UnifiedServerMetrics {
            block_execution_ms: 100,
            state_root_ms: 45,
            parent_handoff_ms: 10,
            publication_ms: 5,
            queue_wait_ms: 40,
            gas_per_block: 30_000_000,
            transactions_per_block: 150,
            memory_usage_mb: 500,
        };

        let snapshot = create_performance_snapshot(harness, server);
        assert_eq!(snapshot.correlation_confidence, 0.95);
        assert!(snapshot.timestamp <= Utc::now());
    }

    #[test]
    fn test_extract_harness_metrics_zero_submitted() {
        let result = BurstResult {
            submitted: 0,
            confirmed: 0,
            pending: 0,
            sign_ms: 0,
            submit_ms: 0,
            confirm_ms: 0,
            submitted_tps: 0.0,
            confirmed_tps: 0.0,
            latency: LatencyStats {
                p50: 0,
                p95: 0,
                p99: 0,
                min: 0,
                max: 0,
                avg: 0,
            },
            server_metrics: None,
            per_method: None,
            validator_health: None,
            per_wave: None,
        };

        let metrics = extract_harness_metrics(&result);
        assert_eq!(metrics.tps_submitted, 0.0);
        assert_eq!(metrics.tps_confirmed, 0.0);
        assert_eq!(metrics.confirmation_rate, 0.0);
        assert_eq!(metrics.error_rate, 0.0);
    }

    #[test]
    fn test_extract_harness_metrics_confirmation_rate_correct() {
        let result = BurstResult {
            submitted: 1000,
            confirmed: 900,
            pending: 50,
            sign_ms: 50,
            submit_ms: 100,
            confirm_ms: 200,
            submitted_tps: 10000.0,
            confirmed_tps: 9000.0,
            latency: LatencyStats {
                p50: 50,
                p95: 150,
                p99: 200,
                min: 10,
                max: 300,
                avg: 100,
            },
            server_metrics: None,
            per_method: None,
            validator_health: None,
            per_wave: None,
        };

        let metrics = extract_harness_metrics(&result);
        // confirmation_rate should be confirmed / submitted = 900 / 1000 = 0.9
        assert!((metrics.confirmation_rate - 0.9).abs() < 0.001);
        // error_rate should be failed / submitted = (submitted - confirmed - pending) / submitted = 50 / 1000 = 0.05
        assert!((metrics.error_rate - 0.05).abs() < 0.001);
    }

    #[test]
    fn test_extract_server_metrics_missing_metrics() {
        let before = HashMap::new();
        let after = HashMap::new();

        let metrics = extract_server_metrics(&before, &after);
        // All metrics should default to 0 when missing
        assert_eq!(metrics.block_execution_ms, 0);
        assert_eq!(metrics.state_root_ms, 0);
        assert_eq!(metrics.parent_handoff_ms, 0);
    }

    #[test]
    fn test_extract_server_metrics_negative_delta_clamped() {
        let mut before = HashMap::new();
        before.insert("reth_diesis_pipeline_execution_ms".to_string(), 1000.0);

        let mut after = HashMap::new();
        // Metric went down (shouldn't happen, but we clamp it)
        after.insert("reth_diesis_pipeline_execution_ms".to_string(), 900.0);

        let metrics = extract_server_metrics(&before, &after);
        // Negative delta should be clamped to 0
        assert_eq!(metrics.block_execution_ms, 0);
    }

    #[test]
    fn test_create_performance_snapshot_preserves_metrics() {
        let harness = HarnessMetrics {
            tps_submitted: 5000.0,
            tps_confirmed: 4900.0,
            latency_p50: 20,
            latency_p95: 60,
            latency_p99: 120,
            confirmation_rate: 0.98,
            pending_ratio: 0.01,
            error_rate: 0.01,
            memory_bytes: 100_000_000,
        };

        let server = UnifiedServerMetrics {
            block_execution_ms: 200,
            state_root_ms: 80,
            parent_handoff_ms: 15,
            publication_ms: 10,
            queue_wait_ms: 5,
            gas_per_block: 60_000_000,
            transactions_per_block: 300,
            memory_usage_mb: 1024,
        };

        let snapshot = create_performance_snapshot(harness.clone(), server.clone());
        assert_eq!(snapshot.harness_metrics.tps_submitted, 5000.0);
        assert_eq!(snapshot.harness_metrics.latency_p99, 120);
        assert_eq!(snapshot.server_metrics.block_execution_ms, 200);
        assert_eq!(snapshot.server_metrics.state_root_ms, 80);
        assert_eq!(snapshot.server_metrics.transactions_per_block, 300);
    }

    #[test]
    fn test_extract_harness_metrics_error_rate_all_confirmed() {
        let result = BurstResult {
            submitted: 1000,
            confirmed: 1000,
            pending: 0,
            sign_ms: 50,
            submit_ms: 100,
            confirm_ms: 200,
            submitted_tps: 10000.0,
            confirmed_tps: 10000.0,
            latency: LatencyStats {
                p50: 10,
                p95: 20,
                p99: 30,
                min: 5,
                max: 50,
                avg: 15,
            },
            server_metrics: None,
            per_method: None,
            validator_health: None,
            per_wave: None,
        };

        let metrics = extract_harness_metrics(&result);
        assert_eq!(metrics.confirmation_rate, 1.0);
        assert_eq!(metrics.error_rate, 0.0);
        assert_eq!(metrics.pending_ratio, 0.0);
    }

    #[test]
    fn test_extract_server_metrics_partial_before_after() {
        // Only 'before' has the metric, 'after' doesn't
        let mut before = HashMap::new();
        before.insert("reth_diesis_pipeline_execution_ms".to_string(), 100.0);

        let after = HashMap::new();

        let metrics = extract_server_metrics(&before, &after);
        // Missing in 'after' => defaults to 0
        assert_eq!(metrics.block_execution_ms, 0);
    }

    #[test]
    fn test_extract_server_metrics_all_fields() {
        let mut before = HashMap::new();
        before.insert("reth_diesis_pipeline_execution_ms".to_string(), 100.0);
        before.insert("reth_diesis_pipeline_state_root_ms".to_string(), 50.0);
        before.insert(
            "reth_diesis_pipeline_parent_handoff_wait_ms".to_string(),
            10.0,
        );
        before.insert("reth_diesis_pipeline_publication_ms".to_string(), 5.0);
        before.insert("reth_diesis_pipeline_queue_wait_ms".to_string(), 3.0);
        before.insert("reth_node_gas_per_block".to_string(), 1000.0);
        before.insert("reth_diesis_transactions_per_block".to_string(), 50.0);
        before.insert("reth_node_memory_usage_mb".to_string(), 500.0);

        let mut after = HashMap::new();
        after.insert("reth_diesis_pipeline_execution_ms".to_string(), 200.0);
        after.insert("reth_diesis_pipeline_state_root_ms".to_string(), 120.0);
        after.insert(
            "reth_diesis_pipeline_parent_handoff_wait_ms".to_string(),
            25.0,
        );
        after.insert("reth_diesis_pipeline_publication_ms".to_string(), 12.0);
        after.insert("reth_diesis_pipeline_queue_wait_ms".to_string(), 8.0);
        after.insert("reth_node_gas_per_block".to_string(), 2000.0);
        after.insert("reth_diesis_transactions_per_block".to_string(), 150.0);
        after.insert("reth_node_memory_usage_mb".to_string(), 600.0);

        let metrics = extract_server_metrics(&before, &after);
        assert_eq!(metrics.block_execution_ms, 100);
        assert_eq!(metrics.state_root_ms, 70);
        assert_eq!(metrics.parent_handoff_ms, 15);
        assert_eq!(metrics.publication_ms, 7);
        assert_eq!(metrics.queue_wait_ms, 5);
        assert_eq!(metrics.gas_per_block, 1000);
        assert_eq!(metrics.transactions_per_block, 100);
        assert_eq!(metrics.memory_usage_mb, 100);
    }
}
