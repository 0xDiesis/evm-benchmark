//! HTML report generation for interactive analysis.

use crate::types::AnalyticsReport;

/// Generate minimal HTML report (simplified version)
pub fn generate_html_report(report: &AnalyticsReport) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Benchmark Analytics Report: {}</title>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <style>
        * {{ margin: 0; padding: 0; box-sizing: border-box; }}
        body {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; background: #f5f5f5; color: #333; }}
        .container {{ max-width: 1200px; margin: 0 auto; padding: 20px; }}
        .header {{ background: linear-gradient(135deg, #667eea 0%, #764ba2 100%); color: white; padding: 40px; border-radius: 8px; margin-bottom: 30px; }}
        .header h1 {{ font-size: 2.5em; margin-bottom: 10px; }}
        .header p {{ font-size: 1.1em; opacity: 0.9; }}
        .metrics-grid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(250px, 1fr)); gap: 20px; margin-bottom: 30px; }}
        .metric-card {{ background: white; padding: 20px; border-radius: 8px; box-shadow: 0 2px 4px rgba(0,0,0,0.1); }}
        .metric-label {{ font-size: 0.9em; color: #666; margin-bottom: 8px; }}
        .metric-value {{ font-size: 2em; font-weight: bold; color: #667eea; }}
        .section {{ background: white; padding: 30px; border-radius: 8px; margin-bottom: 20px; box-shadow: 0 2px 4px rgba(0,0,0,0.1); }}
        .section h2 {{ font-size: 1.8em; margin-bottom: 20px; color: #333; border-bottom: 2px solid #667eea; padding-bottom: 10px; }}
        .bottleneck-item {{ border-left: 4px solid #e74c3c; padding: 15px; margin-bottom: 15px; background: #fef5f5; border-radius: 4px; }}
        .bottleneck-severity {{ display: inline-block; background: #e74c3c; color: white; padding: 4px 8px; border-radius: 3px; font-size: 0.9em; margin-bottom: 8px; }}
        .recommendation-item {{ border-left: 4px solid #27ae60; padding: 15px; margin-bottom: 15px; background: #f0f8f5; border-radius: 4px; }}
        .recommendation-roi {{ display: inline-block; background: #27ae60; color: white; padding: 4px 8px; border-radius: 3px; font-size: 0.9em; margin-bottom: 8px; }}
        footer {{ text-align: center; color: #666; margin-top: 40px; padding-top: 20px; border-top: 1px solid #ddd; }}
    </style>
</head>
<body>
    <div class="container">
        <div class="header">
            <h1>{}</h1>
            <p>Mode: <strong>{}</strong> | Generated: <strong>{}</strong></p>
        </div>

        <div class="metrics-grid">
            <div class="metric-card">
                <div class="metric-label">TPS Confirmed</div>
                <div class="metric-value">{:.0}</div>
            </div>
            <div class="metric-card">
                <div class="metric-label">Latency P99</div>
                <div class="metric-value">{}ms</div>
            </div>
            <div class="metric-card">
                <div class="metric-label">Confirmation Rate</div>
                <div class="metric-value">{:.1}%</div>
            </div>
            <div class="metric-card">
                <div class="metric-label">Server Memory</div>
                <div class="metric-value">{}MB</div>
            </div>
        </div>

        <div class="section">
            <h2>Bottlenecks ({})</h2>
            {}
        </div>

        <div class="section">
            <h2>Recommendations ({})</h2>
            {}
        </div>

        <footer>
            <p>Intelligent Benchmark Analytics Engine | Report Details</p>
        </footer>
    </div>
</body>
</html>"#,
        report.benchmark_name,
        report.benchmark_name,
        report.execution_mode,
        report.timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
        report.snapshot.harness_metrics.tps_confirmed,
        report.snapshot.harness_metrics.latency_p99,
        report.snapshot.harness_metrics.confirmation_rate * 100.0,
        report.snapshot.server_metrics.memory_usage_mb,
        report.bottlenecks.len(),
        report.bottlenecks.iter().map(|b| format!(
            "<div class='bottleneck-item'><div class='bottleneck-severity'>{:.0}% Severity</div><h3>{}</h3><p>{}</p></div>",
            b.severity * 100.0, b.bottleneck_type, b.details
        )).collect::<Vec<_>>().join(""),
        report.recommendations.len(),
        report.recommendations.iter().take(5).map(|r| format!(
            "<div class='recommendation-item'><div class='recommendation-roi'>ROI: {:.1}x</div><h3>{}</h3><p>{}</p></div>",
            r.roi_score, r.title, r.description
        )).collect::<Vec<_>>().join("")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use chrono::Utc;

    #[test]
    fn test_html_report_generation() {
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

        let html = generate_html_report(&report);
        assert!(html.contains("<!DOCTYPE html"));
        assert!(html.contains("test"));
    }
}
