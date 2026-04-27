//! Bottleneck detection and severity analysis.
//!
//! Analyzes performance snapshots to identify 6 types of bottlenecks:
//! - State Root Computation: Disproportionate state root calculation time
//! - Client Signing: High client-side signing overhead
//! - RPC Latency: High confirmation latency from RPC calls
//! - Confirmation Lag: Slow transaction confirmation
//! - Memory Pressure: High memory usage limiting throughput
//! - Network Congestion: Network-related latency

use crate::types::{BottleneckFinding, PerformanceSnapshot};
use std::collections::HashMap;

/// Analyze performance snapshot and detect bottlenecks
pub fn detect_bottlenecks(snapshot: &PerformanceSnapshot) -> Vec<BottleneckFinding> {
    let mut bottlenecks = Vec::new();

    // Total latency = p99 latency (worst-case)
    let total_latency_ms = snapshot.harness_metrics.latency_p99 as f32;
    if total_latency_ms <= 0.0 {
        return bottlenecks;
    }

    // Detect each bottleneck type
    if let Some(finding) = detect_state_root_bottleneck(snapshot, total_latency_ms) {
        bottlenecks.push(finding);
    }

    if let Some(finding) = detect_client_signing_bottleneck(snapshot, total_latency_ms) {
        bottlenecks.push(finding);
    }

    if let Some(finding) = detect_rpc_latency_bottleneck(snapshot, total_latency_ms) {
        bottlenecks.push(finding);
    }

    if let Some(finding) = detect_confirmation_lag_bottleneck(snapshot, total_latency_ms) {
        bottlenecks.push(finding);
    }

    if let Some(finding) = detect_memory_pressure_bottleneck(snapshot, total_latency_ms) {
        bottlenecks.push(finding);
    }

    if let Some(finding) = detect_network_congestion_bottleneck(snapshot, total_latency_ms) {
        bottlenecks.push(finding);
    }

    // Sort by severity (highest first)
    bottlenecks.sort_by(|a, b| {
        b.severity
            .partial_cmp(&a.severity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    bottlenecks
}

/// Detect State Root Computation bottleneck
fn detect_state_root_bottleneck(
    snapshot: &PerformanceSnapshot,
    total_latency_ms: f32,
) -> Option<BottleneckFinding> {
    let state_root_ms = snapshot.server_metrics.state_root_ms as f32;
    let block_exec_ms = snapshot.server_metrics.block_execution_ms as f32;

    // If state root > 40% of total execution time, it's a bottleneck
    let pct_of_exec = if block_exec_ms > 0.0 {
        (state_root_ms / block_exec_ms) * 100.0
    } else {
        0.0
    };

    if pct_of_exec > 40.0 {
        let severity = ((pct_of_exec - 40.0) / 60.0).min(1.0); // Scale 40-100% to 0-1
        let pct_of_total = (state_root_ms / total_latency_ms) * 100.0;

        let mut attribution = HashMap::new();
        attribution.insert("server_execution".to_string(), 80.0);
        attribution.insert("client_waiting".to_string(), 20.0);

        return Some(BottleneckFinding {
            bottleneck_type: "StateRootComputation".to_string(),
            severity,
            pct_of_total,
            details: format!(
                "State root computation takes {:.1}% of block execution time. Consider Verkle trees or incremental commitment schemes.",
                pct_of_exec
            ),
            stack_attribution: attribution,
        });
    }

    None
}

/// Detect Client Signing bottleneck
fn detect_client_signing_bottleneck(
    snapshot: &PerformanceSnapshot,
    total_latency_ms: f32,
) -> Option<BottleneckFinding> {
    let harness_latency_p99 = snapshot.harness_metrics.latency_p99 as f32;
    let server_latency = (snapshot.server_metrics.block_execution_ms
        + snapshot.server_metrics.state_root_ms
        + snapshot.server_metrics.publication_ms) as f32;

    // Client signing overhead = harness latency - server latency
    let client_overhead_ms = (harness_latency_p99 - server_latency).max(0.0);
    let pct_of_total = (client_overhead_ms / total_latency_ms) * 100.0;

    if pct_of_total > 25.0 {
        let severity = ((pct_of_total - 25.0) / 75.0).min(1.0);

        let mut attribution = HashMap::new();
        attribution.insert("client_signing".to_string(), 60.0);
        attribution.insert("serialization".to_string(), 25.0);
        attribution.insert("other".to_string(), 15.0);

        return Some(BottleneckFinding {
            bottleneck_type: "ClientSigning".to_string(),
            severity,
            pct_of_total,
            details: format!(
                "Client-side signing overhead is {:.1}% of total latency. Consider batch signing or hardware acceleration.",
                pct_of_total
            ),
            stack_attribution: attribution,
        });
    }

    None
}

/// Detect RPC Latency bottleneck
fn detect_rpc_latency_bottleneck(
    snapshot: &PerformanceSnapshot,
    total_latency_ms: f32,
) -> Option<BottleneckFinding> {
    let p95_latency = snapshot.harness_metrics.latency_p95 as f32;
    let p50_latency = snapshot.harness_metrics.latency_p50 as f32;

    // High variance between p50 and p95 indicates RPC latency issues
    let variance_ms = (p95_latency - p50_latency).max(0.0);
    let pct_of_total = (variance_ms / total_latency_ms) * 100.0;

    if pct_of_total > 20.0 {
        let severity = ((pct_of_total - 20.0) / 80.0).min(1.0);

        let mut attribution = HashMap::new();
        attribution.insert("rpc_server".to_string(), 50.0);
        attribution.insert("network".to_string(), 30.0);
        attribution.insert("serialization".to_string(), 20.0);

        return Some(BottleneckFinding {
            bottleneck_type: "RPCLatency".to_string(),
            severity,
            pct_of_total,
            details: format!(
                "RPC latency variance (p95-p50) is {:.1}ms ({:.1}% of total). Consider connection pooling, HTTP/2, or local RPC.",
                variance_ms, pct_of_total
            ),
            stack_attribution: attribution,
        });
    }

    None
}

/// Detect Confirmation Lag bottleneck
fn detect_confirmation_lag_bottleneck(
    snapshot: &PerformanceSnapshot,
    total_latency_ms: f32,
) -> Option<BottleneckFinding> {
    let parent_handoff = snapshot.server_metrics.parent_handoff_ms as f32;
    let queue_wait = snapshot.server_metrics.queue_wait_ms as f32;
    let _total_server = (snapshot.server_metrics.block_execution_ms
        + snapshot.server_metrics.state_root_ms
        + snapshot.server_metrics.publication_ms) as f32;

    let confirmation_delay = parent_handoff + queue_wait;
    let pct_of_total = (confirmation_delay / total_latency_ms) * 100.0;

    if pct_of_total > 15.0 {
        let severity = ((pct_of_total - 15.0) / 85.0).min(1.0);

        let mut attribution = HashMap::new();
        attribution.insert("queue_wait".to_string(), 60.0);
        attribution.insert("parent_handoff".to_string(), 40.0);

        return Some(BottleneckFinding {
            bottleneck_type: "ConfirmationLag".to_string(),
            severity,
            pct_of_total,
            details: format!(
                "Transaction confirmation is delayed by {:.1}ms in queue/handoff ({:.1}% of total). Improve block production rate or parallelize validation.",
                confirmation_delay, pct_of_total
            ),
            stack_attribution: attribution,
        });
    }

    None
}

/// Detect Memory Pressure bottleneck
fn detect_memory_pressure_bottleneck(
    snapshot: &PerformanceSnapshot,
    _total_latency_ms: f32,
) -> Option<BottleneckFinding> {
    let harness_memory_mb = snapshot.harness_metrics.memory_bytes / (1024 * 1024);
    let server_memory_mb = snapshot.server_metrics.memory_usage_mb;
    let total_memory_mb = harness_memory_mb + server_memory_mb;

    // If total > 2GB, memory pressure is likely
    let threshold_mb = 2048u64;

    if total_memory_mb > threshold_mb {
        let excess_mb = total_memory_mb - threshold_mb;
        let severity = ((excess_mb as f32) / 1024.0).min(1.0); // Scale: 2GB = 0, 3GB+ = 1

        let pct_of_total = 10.0; // Estimated impact

        let mut attribution = HashMap::new();
        attribution.insert("harness_state".to_string(), 40.0);
        attribution.insert("server_cache".to_string(), 35.0);
        attribution.insert("buffers".to_string(), 25.0);

        return Some(BottleneckFinding {
            bottleneck_type: "MemoryPressure".to_string(),
            severity,
            pct_of_total,
            details: format!(
                "Total memory usage is {:.0}MB (harness: {:.0}MB, server: {:.0}MB). High memory pressure can trigger GC pauses and reduce throughput.",
                total_memory_mb, harness_memory_mb, server_memory_mb
            ),
            stack_attribution: attribution,
        });
    }

    None
}

/// Detect Network Congestion bottleneck
fn detect_network_congestion_bottleneck(
    snapshot: &PerformanceSnapshot,
    total_latency_ms: f32,
) -> Option<BottleneckFinding> {
    let publication_ms = snapshot.server_metrics.publication_ms as f32;
    let block_exec_ms = snapshot.server_metrics.block_execution_ms as f32;

    // If publication > 30% of execution, network is likely bottleneck
    let pct_of_exec = if block_exec_ms > 0.0 {
        (publication_ms / block_exec_ms) * 100.0
    } else {
        0.0
    };

    if pct_of_exec > 30.0 {
        let severity = ((pct_of_exec - 30.0) / 70.0).min(1.0);
        let pct_of_total = (publication_ms / total_latency_ms) * 100.0;

        let mut attribution = HashMap::new();
        attribution.insert("block_propagation".to_string(), 50.0);
        attribution.insert("network_latency".to_string(), 35.0);
        attribution.insert("serialization".to_string(), 15.0);

        return Some(BottleneckFinding {
            bottleneck_type: "NetworkCongestion".to_string(),
            severity,
            pct_of_total,
            details: format!(
                "Block publication takes {:.1}% of execution time. Consider optimizing block format, using sparse trees, or tuning gossip parameters.",
                pct_of_exec
            ),
            stack_attribution: attribution,
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{HarnessMetrics, UnifiedServerMetrics};
    use chrono::Utc;

    fn make_snapshot(
        state_root_ms: u64,
        block_exec_ms: u64,
        latency_p99: u64,
        latency_p95: u64,
        latency_p50: u64,
        memory_bytes: u64,
        server_memory_mb: u64,
    ) -> PerformanceSnapshot {
        PerformanceSnapshot {
            timestamp: Utc::now(),
            harness_metrics: HarnessMetrics {
                tps_submitted: 3000.0,
                tps_confirmed: 2950.0,
                latency_p50,
                latency_p95,
                latency_p99,
                confirmation_rate: 0.98,
                pending_ratio: 0.01,
                error_rate: 0.01,
                memory_bytes,
            },
            server_metrics: UnifiedServerMetrics {
                block_execution_ms: block_exec_ms,
                state_root_ms,
                parent_handoff_ms: 10,
                publication_ms: 5,
                queue_wait_ms: 5,
                gas_per_block: 30_000_000,
                transactions_per_block: 150,
                memory_usage_mb: server_memory_mb,
            },
            correlation_confidence: 0.95,
        }
    }

    #[test]
    fn test_detect_state_root_bottleneck() {
        let snapshot = make_snapshot(
            60,         // state_root_ms: 60ms
            100,        // block_exec_ms: 100ms
            100,        // latency_p99
            80,         // latency_p95
            50,         // latency_p50
            50_000_000, // memory
            500,        // server_memory
        );

        let bottlenecks = detect_bottlenecks(&snapshot);
        assert!(
            bottlenecks
                .iter()
                .any(|b| b.bottleneck_type == "StateRootComputation")
        );
    }

    #[test]
    fn test_detect_client_signing_bottleneck() {
        let snapshot = make_snapshot(
            10,  // state_root_ms
            50,  // block_exec_ms
            150, // latency_p99: high latency (signing overhead)
            100, // latency_p95
            80,  // latency_p50
            50_000_000, 500,
        );

        let bottlenecks = detect_bottlenecks(&snapshot);
        assert!(
            bottlenecks
                .iter()
                .any(|b| b.bottleneck_type == "ClientSigning")
        );
    }

    #[test]
    fn test_detect_rpc_latency_bottleneck() {
        let snapshot = make_snapshot(
            10,  // state_root_ms
            50,  // block_exec_ms
            100, // latency_p99
            80,  // latency_p95: high variance
            30,  // latency_p50: even higher variance
            50_000_000, 500,
        );

        let bottlenecks = detect_bottlenecks(&snapshot);
        assert!(
            bottlenecks
                .iter()
                .any(|b| b.bottleneck_type == "RPCLatency")
        );
    }

    #[test]
    fn test_detect_memory_pressure_bottleneck() {
        let snapshot = make_snapshot(
            10,
            50,
            100,
            80,
            50,
            2_500_000_000, // 2.5GB harness memory
            500,           // 500MB server memory
        );

        let bottlenecks = detect_bottlenecks(&snapshot);
        assert!(
            bottlenecks
                .iter()
                .any(|b| b.bottleneck_type == "MemoryPressure")
        );
    }

    #[test]
    fn test_no_bottlenecks_healthy_system() {
        let snapshot = make_snapshot(
            5,          // Low state root time (5% of 100ms exec)
            100,        // Reasonable exec time
            130,        // Good p99 latency matching server time + minimal overhead
            128,        // Tight variance p95
            127,        // Tight distribution p50
            50_000_000, // Normal memory
            500,        // Normal server memory
        );

        let bottlenecks = detect_bottlenecks(&snapshot);
        assert!(bottlenecks.is_empty());
    }

    #[test]
    fn test_detect_confirmation_lag_bottleneck() {
        // Set large parent_handoff and queue_wait to trigger the bottleneck
        let mut snapshot = make_snapshot(
            10, 50, 100, // p99
            80,  // p95
            70,  // p50
            50_000_000, 500,
        );
        // Manually increase confirmation delays beyond the defaults
        snapshot.server_metrics.parent_handoff_ms = 20;
        snapshot.server_metrics.queue_wait_ms = 20;

        let bottlenecks = detect_bottlenecks(&snapshot);
        assert!(
            bottlenecks
                .iter()
                .any(|b| b.bottleneck_type == "ConfirmationLag")
        );
    }

    #[test]
    fn test_detect_network_congestion_bottleneck() {
        let mut snapshot = make_snapshot(10, 100, 150, 140, 130, 50_000_000, 500);
        // Set high publication time relative to execution
        snapshot.server_metrics.publication_ms = 40; // 40% of 100ms execution

        let bottlenecks = detect_bottlenecks(&snapshot);
        assert!(
            bottlenecks
                .iter()
                .any(|b| b.bottleneck_type == "NetworkCongestion")
        );
    }

    #[test]
    fn test_bottlenecks_sorted_by_severity() {
        let snapshot = make_snapshot(
            50, // Mild state root bottleneck
            100, 200, // High client signing overhead
            100, 50, 50_000_000, 500,
        );

        let bottlenecks = detect_bottlenecks(&snapshot);
        assert!(
            bottlenecks
                .windows(2)
                .all(|pair| pair[0].severity >= pair[1].severity)
        );
    }

    #[test]
    fn test_zero_latency_p99_returns_empty() {
        let snapshot = make_snapshot(0, 0, 0, 0, 0, 0, 0);
        let bottlenecks = detect_bottlenecks(&snapshot);
        assert!(
            bottlenecks.is_empty(),
            "Zero p99 latency should return no bottlenecks"
        );
    }

    #[test]
    fn test_no_state_root_bottleneck_when_low_ratio() {
        // State root is 10% of block execution (well below 40% threshold)
        let snapshot = make_snapshot(10, 100, 200, 150, 100, 50_000_000, 500);
        let bottlenecks = detect_bottlenecks(&snapshot);
        assert!(
            !bottlenecks
                .iter()
                .any(|b| b.bottleneck_type == "StateRootComputation"),
            "Should not detect state root bottleneck when ratio is low"
        );
    }

    #[test]
    fn test_no_network_congestion_when_publication_low() {
        // Publication is 5% of execution (well below 30% threshold)
        let mut snapshot = make_snapshot(10, 100, 200, 150, 100, 50_000_000, 500);
        snapshot.server_metrics.publication_ms = 5;
        let bottlenecks = detect_bottlenecks(&snapshot);
        assert!(
            !bottlenecks
                .iter()
                .any(|b| b.bottleneck_type == "NetworkCongestion"),
            "Should not detect network congestion when publication is low"
        );
    }

    #[test]
    fn test_no_memory_pressure_below_threshold() {
        // Total memory well below 2GB threshold
        let snapshot = make_snapshot(10, 100, 200, 150, 100, 100_000_000, 100);
        let bottlenecks = detect_bottlenecks(&snapshot);
        assert!(
            !bottlenecks
                .iter()
                .any(|b| b.bottleneck_type == "MemoryPressure"),
            "Should not detect memory pressure below 2GB"
        );
    }

    #[test]
    fn test_severity_clamped_to_one() {
        // Extreme state root bottleneck: 100% of execution
        let snapshot = make_snapshot(100, 100, 200, 150, 100, 50_000_000, 500);
        let bottlenecks = detect_bottlenecks(&snapshot);
        for b in &bottlenecks {
            assert!(
                b.severity <= 1.0,
                "Severity should be clamped to 1.0, got {}",
                b.severity
            );
        }
    }

    #[test]
    fn test_bottleneck_has_stack_attribution() {
        // Trigger state root bottleneck
        let snapshot = make_snapshot(80, 100, 200, 150, 100, 50_000_000, 500);
        let bottlenecks = detect_bottlenecks(&snapshot);
        let state_root = bottlenecks
            .iter()
            .find(|b| b.bottleneck_type == "StateRootComputation");
        assert!(state_root.is_some());
        let sr = state_root.unwrap();
        assert!(!sr.stack_attribution.is_empty());
        // Attribution percentages should sum to 100
        let total: f32 = sr.stack_attribution.values().sum();
        assert!((total - 100.0).abs() < 0.1);
    }

    #[test]
    fn test_zero_block_execution_no_panic() {
        // Zero block execution should not cause division by zero
        let snapshot = make_snapshot(0, 0, 100, 80, 50, 50_000_000, 500);
        let bottlenecks = detect_bottlenecks(&snapshot);
        // Should still work without panicking; may or may not find bottlenecks
        for b in &bottlenecks {
            assert!(b.pct_of_total.is_finite());
            assert!(b.severity.is_finite());
        }
    }
}
