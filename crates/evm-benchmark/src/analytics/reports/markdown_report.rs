//! Markdown report generation for documentation.

use super::report_types::*;
use crate::types::AnalyticsReport;

/// Generate Markdown report
pub fn generate_markdown_report(report: &AnalyticsReport) -> String {
    let mut md = String::new();

    md.push_str(&format!(
        "# Benchmark Analytics Report: {}\n\n",
        report.benchmark_name
    ));
    md.push_str(&format!("**Mode:** `{}`  \n", report.execution_mode));
    md.push_str(&format!(
        "**Generated:** {}\n\n",
        report.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
    ));

    // Metrics section
    md.push_str("## Performance Metrics\n\n");
    md.push_str("### Harness Metrics\n");
    md.push_str("| Metric | Value |\n");
    md.push_str("|--------|-------|\n");
    md.push_str(&format!(
        "| TPS Submitted | {:.1} tx/s |\n",
        report.snapshot.harness_metrics.tps_submitted
    ));
    md.push_str(&format!(
        "| TPS Confirmed | {:.1} tx/s |\n",
        report.snapshot.harness_metrics.tps_confirmed
    ));
    md.push_str(&format!(
        "| Latency P50 | {}ms |\n",
        report.snapshot.harness_metrics.latency_p50
    ));
    md.push_str(&format!(
        "| Latency P95 | {}ms |\n",
        report.snapshot.harness_metrics.latency_p95
    ));
    md.push_str(&format!(
        "| Latency P99 | {}ms |\n",
        report.snapshot.harness_metrics.latency_p99
    ));
    md.push_str(&format!(
        "| Confirmation Rate | {} |\n",
        format_pct(report.snapshot.harness_metrics.confirmation_rate * 100.0)
    ));
    md.push_str(&format!(
        "| Memory Usage | {} |\n\n",
        format_bytes(report.snapshot.harness_metrics.memory_bytes)
    ));

    md.push_str("### Server Metrics\n");
    md.push_str("| Metric | Value |\n");
    md.push_str("|--------|-------|\n");
    md.push_str(&format!(
        "| Block Execution | {} |\n",
        format_ms(report.snapshot.server_metrics.block_execution_ms)
    ));
    md.push_str(&format!(
        "| State Root Computation | {} |\n",
        format_ms(report.snapshot.server_metrics.state_root_ms)
    ));
    md.push_str(&format!(
        "| Publication Latency | {} |\n\n",
        format_ms(report.snapshot.server_metrics.publication_ms)
    ));

    // Bottlenecks
    if !report.bottlenecks.is_empty() {
        md.push_str("## Detected Bottlenecks\n\n");
        for (i, bn) in report.bottlenecks.iter().enumerate() {
            md.push_str(&format!("### {}. {}\n\n", i + 1, bn.bottleneck_type));
            md.push_str(&format!("- **Severity:** {:.1}%\n", bn.severity * 100.0));
            md.push_str(&format!(
                "- **Impact:** {:.1}% of total time\n",
                bn.pct_of_total
            ));
            md.push_str(&format!("- **Details:** {}\n\n", bn.details));
        }
    }

    // Recommendations
    if !report.recommendations.is_empty() {
        md.push_str("## Recommendations\n\n");
        for (i, rec) in report.recommendations.iter().enumerate() {
            md.push_str(&format!(
                "### {}. {} (Priority: {})\n\n",
                i + 1,
                rec.title,
                rec.priority
            ));
            md.push_str(&format!("{}\n\n", rec.description));
            md.push_str(&format!(
                "- **Est. TPS Improvement:** {}\n",
                format_pct(rec.estimated_tps_improvement_pct)
            ));
            md.push_str(&format!("- **Effort Level:** {}\n", rec.effort_level));
            md.push_str(&format!("- **ROI Score:** {:.2}\n\n", rec.roi_score));
            if !rec.implementation_hints.is_empty() {
                md.push_str("**Implementation Hints:**\n");
                for hint in &rec.implementation_hints {
                    md.push_str(&format!("- {}\n", hint));
                }
                md.push('\n');
            }
        }
    }

    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use chrono::Utc;

    #[test]
    fn test_markdown_report_generation() {
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
            recommendations: vec![
                Recommendation {
                    priority: "High".to_string(),
                    title: "Use Local RPC Endpoint".to_string(),
                    description: "Reduce RPC latency by colocating the endpoint.".to_string(),
                    estimated_tps_improvement_pct: 15.0,
                    effort_level: "Low".to_string(),
                    roi_score: 15.0,
                    implementation_hints: vec![
                        "Deploy RPC on localhost".to_string(),
                        "Enable connection reuse".to_string(),
                    ],
                },
                Recommendation {
                    priority: "Medium".to_string(),
                    title: "Trim validator polling".to_string(),
                    description: "Avoid unnecessary polling work.".to_string(),
                    estimated_tps_improvement_pct: 4.0,
                    effort_level: "Low".to_string(),
                    roi_score: 4.0,
                    implementation_hints: vec![],
                },
            ],
            reports: ReportPackage {
                json: String::new(),
                html: String::new(),
                ascii: String::new(),
                markdown: String::new(),
            },
        };

        let md = generate_markdown_report(&report);
        assert!(md.contains("# Benchmark Analytics Report"));
        assert!(md.contains("| TPS Submitted |"));
        assert!(md.contains("## Recommendations"));
        assert!(md.contains("**Implementation Hints:**"));
        assert!(md.contains("- Deploy RPC on localhost"));
    }
}
