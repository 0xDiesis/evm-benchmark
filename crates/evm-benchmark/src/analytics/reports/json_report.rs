//! JSON report generation for CI/CD integration.

use crate::types::AnalyticsReport;
use serde_json::json;

/// Generate JSON report
pub fn generate_json_report(
    report: &AnalyticsReport,
) -> Result<String, Box<dyn std::error::Error>> {
    let json = json!({
        "timestamp": report.timestamp.to_rfc3339(),
        "benchmark": {
            "name": report.benchmark_name,
            "mode": report.execution_mode,
        },
        "metrics": {
            "harness": {
                "tps_submitted": report.snapshot.harness_metrics.tps_submitted,
                "tps_confirmed": report.snapshot.harness_metrics.tps_confirmed,
                "latency_p50_ms": report.snapshot.harness_metrics.latency_p50,
                "latency_p95_ms": report.snapshot.harness_metrics.latency_p95,
                "latency_p99_ms": report.snapshot.harness_metrics.latency_p99,
                "confirmation_rate": report.snapshot.harness_metrics.confirmation_rate,
                "memory_mb": report.snapshot.harness_metrics.memory_bytes / (1024 * 1024),
            },
            "server": {
                "block_execution_ms": report.snapshot.server_metrics.block_execution_ms,
                "state_root_ms": report.snapshot.server_metrics.state_root_ms,
                "parent_handoff_ms": report.snapshot.server_metrics.parent_handoff_ms,
                "publication_ms": report.snapshot.server_metrics.publication_ms,
                "memory_mb": report.snapshot.server_metrics.memory_usage_mb,
            }
        },
        "bottlenecks": report.bottlenecks.iter().map(|b| json!({
            "type": b.bottleneck_type,
            "severity": b.severity,
            "pct_of_total": b.pct_of_total,
            "details": b.details,
        })).collect::<Vec<_>>(),
        "recommendations": report.recommendations.iter().map(|r| json!({
            "priority": r.priority,
            "title": r.title,
            "description": r.description,
            "estimated_tps_improvement_pct": r.estimated_tps_improvement_pct,
            "effort": r.effort_level,
            "roi_score": r.roi_score,
        })).collect::<Vec<_>>(),
        "regression": report.regression_analysis.as_ref().map(|r| json!({
            "tps_delta": r.tps_delta,
            "tps_pct_change": r.tps_pct_change,
            "latency_delta_ms": r.latency_delta_ms,
            "p_value": r.p_value,
            "verdict": r.verdict,
        })),
    });

    Ok(serde_json::to_string_pretty(&json)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AnalyticsReport, HarnessMetrics, PerformanceSnapshot, ReportPackage, UnifiedServerMetrics,
    };
    use chrono::Utc;

    #[test]
    fn test_json_report_serialization() {
        let report = AnalyticsReport {
            benchmark_name: "test-bench".to_string(),
            execution_mode: "burst".to_string(),
            timestamp: Utc::now(),
            snapshot: PerformanceSnapshot {
                timestamp: Utc::now(),
                harness_metrics: HarnessMetrics {
                    tps_submitted: 3000.0,
                    tps_confirmed: 2950.0,
                    latency_p50: 50,
                    latency_p95: 100,
                    latency_p99: 150,
                    confirmation_rate: 0.98,
                    pending_ratio: 0.01,
                    error_rate: 0.01,
                    memory_bytes: 50_000_000,
                },
                server_metrics: UnifiedServerMetrics {
                    block_execution_ms: 100,
                    state_root_ms: 40,
                    parent_handoff_ms: 10,
                    publication_ms: 5,
                    queue_wait_ms: 5,
                    gas_per_block: 30_000_000,
                    transactions_per_block: 150,
                    memory_usage_mb: 500,
                },
                correlation_confidence: 0.95,
            },
            bottlenecks: vec![],
            regression_analysis: None,
            recommendations: vec![],
            reports: ReportPackage {
                json: String::new(),
                html: String::new(),
                ascii: String::new(),
                markdown: String::new(),
            },
        };

        let json = generate_json_report(&report);
        assert!(json.is_ok());
        let json_str = json.unwrap();
        assert!(json_str.contains("test-bench"));
        assert!(json_str.contains("burst"));
    }
}
