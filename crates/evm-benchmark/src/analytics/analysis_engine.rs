//! Analysis engine orchestrator.
//!
//! Coordinates metrics collection, bottleneck detection, regression analysis,
//! recommendation generation, and multi-format report generation.

use crate::analytics::{
    bottleneck_detector::detect_bottlenecks,
    metrics_collector::{
        create_performance_snapshot, extract_harness_metrics, extract_server_metrics,
    },
    prometheus_integration::MetricsMap,
    recommendations::generate_recommendations,
    regression_detector::{BaselineMetrics, detect_regression},
    reports::{
        generate_ascii_report, generate_html_report, generate_json_report, generate_markdown_report,
    },
};
use crate::types::{AnalyticsReport, BurstResult, ReportPackage};
use anyhow::Result;
use chrono::Utc;
use url::Url;

/// Run complete analytics pipeline on benchmark results
///
/// Orchestrates all analysis modules to produce a comprehensive AnalyticsReport:
/// - Extracts metrics from benchmark harness and server (if available)
/// - Detects performance bottlenecks
/// - Analyzes regressions against baseline (if provided)
/// - Generates recommendations
/// - Produces reports in all 4 formats (JSON, ASCII, Markdown, HTML)
///
/// # Arguments
///
/// * `benchmark_name` - Human-readable benchmark identifier
/// * `execution_mode` - Execution mode (e.g., "burst", "sustained", "ceiling")
/// * `harness_result` - Benchmark results from harness execution
/// * `prometheus_url` - Optional Prometheus URL for server metrics collection
/// * `baseline` - Optional baseline metrics for regression detection
///
/// # Returns
///
/// Complete AnalyticsReport with all metrics, analysis, and reports
pub async fn run_analysis(
    benchmark_name: &str,
    execution_mode: &str,
    harness_result: &BurstResult,
    _prometheus_url: Option<&Url>,
    baseline: Option<&BaselineMetrics>,
) -> Result<AnalyticsReport> {
    // Extract harness metrics from benchmark result
    let harness_metrics = extract_harness_metrics(harness_result);

    // Scrape server metrics if Prometheus URL provided
    // For now, use empty metrics as default (would require actual Prometheus integration)
    let server_metrics = extract_server_metrics(&MetricsMap::default(), &MetricsMap::default());

    // Create performance snapshot
    let snapshot = create_performance_snapshot(harness_metrics.clone(), server_metrics);

    // Detect bottlenecks
    let bottlenecks = detect_bottlenecks(&snapshot);

    // Analyze for regressions if baseline provided
    let regression_analysis = baseline
        .as_ref()
        .map(|b| detect_regression(&harness_metrics, b));

    // Generate recommendations based on bottlenecks
    let recommendations = generate_recommendations(&bottlenecks);

    // Generate all report formats
    let json_report = {
        let report = AnalyticsReport {
            benchmark_name: benchmark_name.to_string(),
            execution_mode: execution_mode.to_string(),
            timestamp: Utc::now(),
            snapshot: snapshot.clone(),
            bottlenecks: bottlenecks.clone(),
            regression_analysis: regression_analysis.clone(),
            recommendations: recommendations.clone(),
            reports: ReportPackage {
                json: String::new(),
                html: String::new(),
                ascii: String::new(),
                markdown: String::new(),
            },
        };
        generate_json_report(&report).unwrap_or_default()
    };

    let ascii_report = {
        let report = AnalyticsReport {
            benchmark_name: benchmark_name.to_string(),
            execution_mode: execution_mode.to_string(),
            timestamp: Utc::now(),
            snapshot: snapshot.clone(),
            bottlenecks: bottlenecks.clone(),
            regression_analysis: regression_analysis.clone(),
            recommendations: recommendations.clone(),
            reports: ReportPackage {
                json: String::new(),
                html: String::new(),
                ascii: String::new(),
                markdown: String::new(),
            },
        };
        generate_ascii_report(&report)
    };

    let markdown_report = {
        let report = AnalyticsReport {
            benchmark_name: benchmark_name.to_string(),
            execution_mode: execution_mode.to_string(),
            timestamp: Utc::now(),
            snapshot: snapshot.clone(),
            bottlenecks: bottlenecks.clone(),
            regression_analysis: regression_analysis.clone(),
            recommendations: recommendations.clone(),
            reports: ReportPackage {
                json: String::new(),
                html: String::new(),
                ascii: String::new(),
                markdown: String::new(),
            },
        };
        generate_markdown_report(&report)
    };

    let html_report = {
        let report = AnalyticsReport {
            benchmark_name: benchmark_name.to_string(),
            execution_mode: execution_mode.to_string(),
            timestamp: Utc::now(),
            snapshot: snapshot.clone(),
            bottlenecks: bottlenecks.clone(),
            regression_analysis: regression_analysis.clone(),
            recommendations: recommendations.clone(),
            reports: ReportPackage {
                json: String::new(),
                html: String::new(),
                ascii: String::new(),
                markdown: String::new(),
            },
        };
        generate_html_report(&report)
    };

    // Create final report with all formats
    Ok(AnalyticsReport {
        benchmark_name: benchmark_name.to_string(),
        execution_mode: execution_mode.to_string(),
        timestamp: Utc::now(),
        snapshot,
        bottlenecks,
        regression_analysis,
        recommendations,
        reports: ReportPackage {
            json: json_report,
            html: html_report,
            ascii: ascii_report,
            markdown: markdown_report,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LatencyStats;

    #[tokio::test]
    async fn test_run_analysis_basic() {
        let result = BurstResult {
            submitted: 1000,
            confirmed: 980,
            pending: 10,
            sign_ms: 50,
            submit_ms: 100,
            confirm_ms: 200,
            submitted_tps: 100.0,
            confirmed_tps: 98.0,
            latency: LatencyStats {
                p50: 50,
                p95: 100,
                p99: 150,
                min: 10,
                max: 500,
                avg: 75,
            },
            server_metrics: None,
            per_method: None,
            validator_health: None,
            per_wave: None,
        };

        let analysis = run_analysis("test", "burst", &result, None, None).await;
        assert!(analysis.is_ok());

        let report = analysis.unwrap();
        assert_eq!(report.benchmark_name, "test");
        assert_eq!(report.execution_mode, "burst");
        assert!(!report.reports.json.is_empty());
        assert!(!report.reports.ascii.is_empty());
        assert!(!report.reports.markdown.is_empty());
        assert!(!report.reports.html.is_empty());
    }

    #[tokio::test]
    async fn test_run_analysis_with_regression() {
        let result = BurstResult {
            submitted: 1000,
            confirmed: 980,
            pending: 10,
            sign_ms: 50,
            submit_ms: 100,
            confirm_ms: 200,
            submitted_tps: 100.0,
            confirmed_tps: 98.0,
            latency: LatencyStats {
                p50: 50,
                p95: 100,
                p99: 150,
                min: 10,
                max: 500,
                avg: 75,
            },
            server_metrics: None,
            per_method: None,
            validator_health: None,
            per_wave: None,
        };

        let baseline = BaselineMetrics {
            tps_confirmed: 95.0,
            latency_p99_ms: 150,
        };

        let analysis = run_analysis("test", "burst", &result, None, Some(&baseline)).await;
        assert!(analysis.is_ok());

        let report = analysis.unwrap();
        assert!(report.regression_analysis.is_some());
    }

    #[tokio::test]
    async fn test_run_analysis_all_reports_generated() {
        let result = BurstResult {
            submitted: 500,
            confirmed: 490,
            pending: 5,
            sign_ms: 25,
            submit_ms: 50,
            confirm_ms: 100,
            submitted_tps: 50.0,
            confirmed_tps: 49.0,
            latency: LatencyStats {
                p50: 25,
                p95: 50,
                p99: 75,
                min: 5,
                max: 250,
                avg: 40,
            },
            server_metrics: None,
            per_method: None,
            validator_health: None,
            per_wave: None,
        };

        let analysis = run_analysis("multi-report-test", "sustained", &result, None, None).await;
        assert!(analysis.is_ok());

        let report = analysis.unwrap();
        // Verify all report formats are generated
        assert!(
            !report.reports.json.is_empty(),
            "JSON report should be generated"
        );
        assert!(
            !report.reports.ascii.is_empty(),
            "ASCII report should be generated"
        );
        assert!(
            !report.reports.markdown.is_empty(),
            "Markdown report should be generated"
        );
        assert!(
            !report.reports.html.is_empty(),
            "HTML report should be generated"
        );

        // Verify metadata
        assert_eq!(report.benchmark_name, "multi-report-test");
        assert_eq!(report.execution_mode, "sustained");
    }

    #[tokio::test]
    async fn test_run_analysis_zero_values() {
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

        // Should not panic on zero-value inputs
        let analysis = run_analysis("zero-test", "burst", &result, None, None).await;
        assert!(analysis.is_ok());
        let report = analysis.unwrap();
        // Zero p99 means no bottlenecks detected (early return in detect_bottlenecks)
        assert!(report.bottlenecks.is_empty());
        assert!(report.regression_analysis.is_none());
    }

    #[tokio::test]
    async fn test_run_analysis_regression_detected() {
        let result = BurstResult {
            submitted: 1000,
            confirmed: 700,
            pending: 50,
            sign_ms: 50,
            submit_ms: 100,
            confirm_ms: 200,
            submitted_tps: 100.0,
            confirmed_tps: 70.0,
            latency: LatencyStats {
                p50: 50,
                p95: 100,
                p99: 150,
                min: 10,
                max: 500,
                avg: 75,
            },
            server_metrics: None,
            per_method: None,
            validator_health: None,
            per_wave: None,
        };

        // Baseline is much better — should detect regression
        let baseline = BaselineMetrics {
            tps_confirmed: 200.0,
            latency_p99_ms: 50,
        };

        let analysis = run_analysis("regress-test", "burst", &result, None, Some(&baseline)).await;
        assert!(analysis.is_ok());
        let report = analysis.unwrap();
        assert!(report.regression_analysis.is_some());
        let regression = report.regression_analysis.unwrap();
        assert_eq!(regression.verdict, "regressed");
        assert!(regression.tps_delta < 0.0);
    }
}
