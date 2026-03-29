//! ASCII art report generation for terminal output.

use super::report_types::*;
use crate::types::AnalyticsReport;

/// Generate ASCII report with tables and styling
pub fn generate_ascii_report(report: &AnalyticsReport) -> String {
    let mut output = String::new();

    output.push_str("╔═══════════════════════════════════════════════════════════╗\n");
    output.push_str("║          Benchmark Analytics Report                       ║\n");
    output.push_str("╚═══════════════════════════════════════════════════════════╝\n\n");

    output.push_str(&format!(
        "Benchmark: {} [{}]\n",
        report.benchmark_name, report.execution_mode
    ));
    output.push_str(&format!(
        "Timestamp: {}\n\n",
        report.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
    ));

    // Metrics table
    output.push_str("┌─ PERFORMANCE METRICS ──────────────────────────────────────┐\n");
    output.push_str("│ Harness Metrics:                                           │\n");
    output.push_str(&format!(
        "│   • TPS Submitted:        {:<40} │\n",
        format!("{:.1} tx/s", report.snapshot.harness_metrics.tps_submitted)
    ));
    output.push_str(&format!(
        "│   • TPS Confirmed:        {:<40} │\n",
        format!("{:.1} tx/s", report.snapshot.harness_metrics.tps_confirmed)
    ));
    output.push_str(&format!(
        "│   • Latency (P50/P99):    {:<40} │\n",
        format!(
            "{}/{}ms",
            report.snapshot.harness_metrics.latency_p50,
            report.snapshot.harness_metrics.latency_p99
        )
    ));
    output.push_str(&format!(
        "│   • Memory Usage:         {:<40} │\n",
        format_bytes(report.snapshot.harness_metrics.memory_bytes)
    ));
    output.push_str("│                                                            │\n");
    output.push_str("│ Server Metrics:                                            │\n");
    output.push_str(&format!(
        "│   • Block Execution:      {:<40} │\n",
        format_ms(report.snapshot.server_metrics.block_execution_ms)
    ));
    output.push_str(&format!(
        "│   • State Root Time:      {:<40} │\n",
        format_ms(report.snapshot.server_metrics.state_root_ms)
    ));
    output.push_str(&format!(
        "│   • Memory Usage:         {:<40} │\n",
        format!("{}MB", report.snapshot.server_metrics.memory_usage_mb)
    ));
    output.push_str("└────────────────────────────────────────────────────────────┘\n\n");

    // Bottlenecks table
    if !report.bottlenecks.is_empty() {
        output.push_str("┌─ DETECTED BOTTLENECKS ─────────────────────────────────────┐\n");
        for (i, bn) in report.bottlenecks.iter().enumerate() {
            output.push_str(&format!(
                "│ [{}] {} (severity: {:.1}%)                       │\n",
                i + 1,
                bn.bottleneck_type,
                bn.severity * 100.0
            ));
            output.push_str(&format!(
                "│     {}                     │\n",
                truncate(&bn.details, 50)
            ));
        }
        output.push_str("└────────────────────────────────────────────────────────────┘\n\n");
    }

    // Recommendations table
    if !report.recommendations.is_empty() {
        output.push_str("┌─ TOP RECOMMENDATIONS ──────────────────────────────────────┐\n");
        for (i, rec) in report.recommendations.iter().take(5).enumerate() {
            output.push_str(&format!(
                "│ [{}] {} (ROI: {:.1}x)           │\n",
                i + 1,
                truncate(&rec.title, 40),
                rec.roi_score
            ));
            output.push_str(&format!(
                "│     Est. Improvement: {} | Effort: {}          │\n",
                format_pct(rec.estimated_tps_improvement_pct),
                rec.effort_level
            ));
        }
        output.push_str("└────────────────────────────────────────────────────────────┘\n\n");
    }

    output
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        format!("{}...", &s[..max_len - 3])
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use chrono::Utc;

    #[test]
    fn test_ascii_report_generation() {
        let report = AnalyticsReport {
            benchmark_name: "test".to_string(),
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

        let ascii = generate_ascii_report(&report);
        assert!(ascii.contains("Benchmark Analytics Report"));
        assert!(ascii.contains("test"));
        assert!(ascii.contains("TPS"));
    }
}
