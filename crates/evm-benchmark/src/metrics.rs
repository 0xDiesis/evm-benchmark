//! Prometheus metrics export for benchmark statistics.
//!
//! This module provides a `MetricsExporter` that manages Prometheus metrics for the
//! benchmark harness, including counters, gauges, and histograms for transaction
//! submission, confirmation, and per-validator health metrics.

use crate::types::{HistogramDelta, ServerMetrics, ValidatorHealthSnapshot};
use anyhow::{Context, Result};
use prometheus::{Gauge, GaugeVec, Histogram, HistogramOpts, IntCounter, IntGauge, Registry};
use std::collections::HashMap;
use std::sync::Arc;
use url::Url;

/// Raw metrics map from Prometheus
pub type MetricsMap = HashMap<String, f64>;

/// Manages Prometheus metrics for benchmark execution.
///
/// This struct wraps Prometheus metric collectors and provides methods
/// to update metrics during benchmark execution.
#[derive(Clone)]
#[allow(dead_code)]
pub struct MetricsExporter {
    registry: Arc<Registry>,
    // Transaction counters
    transactions_submitted_total: IntCounter,
    transactions_confirmed_total: IntCounter,
    transactions_failed_total: IntCounter,
    // Current state gauges
    transactions_pending: IntGauge,
    tps_current: Gauge,
    memory_bytes: Gauge,
    // Latency histograms
    latency_submission_ms: Histogram,
    latency_confirmation_ms: Histogram,
    // Per-validator metrics
    validator_health: GaugeVec,
    validator_latency_p50_ms: GaugeVec,
    validator_latency_p95_ms: GaugeVec,
    validator_latency_p99_ms: GaugeVec,
    validator_error_rate: GaugeVec,
    validator_acceptance_rate: GaugeVec,
}

impl MetricsExporter {
    /// Creates a new `MetricsExporter` with a default Prometheus registry.
    pub fn new() -> Result<Self> {
        Self::with_registry(Registry::new())
    }

    /// Creates a new `MetricsExporter` with a custom Prometheus registry.
    pub fn with_registry(registry: Registry) -> Result<Self> {
        let registry = Arc::new(registry);

        // Create transaction counters
        let transactions_submitted_total = IntCounter::new(
            "evm_benchmark_transactions_submitted_total",
            "Total number of transactions submitted",
        )
        .context("Failed to create transactions_submitted_total counter")?;

        let transactions_confirmed_total = IntCounter::new(
            "evm_benchmark_transactions_confirmed_total",
            "Total number of transactions confirmed",
        )
        .context("Failed to create transactions_confirmed_total counter")?;

        let transactions_failed_total = IntCounter::new(
            "evm_benchmark_transactions_failed_total",
            "Total number of transactions failed",
        )
        .context("Failed to create transactions_failed_total counter")?;

        // Create state gauges
        let transactions_pending = IntGauge::new(
            "evm_benchmark_transactions_pending",
            "Number of pending transactions",
        )
        .context("Failed to create transactions_pending gauge")?;

        let tps_current = Gauge::new(
            "evm_benchmark_tps_current",
            "Current transactions per second",
        )
        .context("Failed to create tps_current gauge")?;

        let memory_bytes = Gauge::new("evm_benchmark_memory_bytes", "Memory usage in bytes")
            .context("Failed to create memory_bytes gauge")?;

        // Create latency histograms
        let latency_submission_ms = Histogram::with_opts(HistogramOpts::new(
            "evm_benchmark_latency_submission_ms",
            "Transaction submission latency in milliseconds",
        ))
        .context("Failed to create latency_submission_ms histogram")?;

        let latency_confirmation_ms = Histogram::with_opts(HistogramOpts::new(
            "evm_benchmark_latency_confirmation_ms",
            "Transaction confirmation latency in milliseconds",
        ))
        .context("Failed to create latency_confirmation_ms histogram")?;

        // Create per-validator metrics
        let validator_health = GaugeVec::new(
            prometheus::Opts::new(
                "evm_benchmark_validator_health",
                "Validator health status (0=down, 1=up)",
            ),
            &["url"],
        )
        .context("Failed to create validator_health gauge")?;

        let validator_latency_p50_ms = GaugeVec::new(
            prometheus::Opts::new(
                "evm_benchmark_validator_latency_p50_ms",
                "Validator P50 latency in milliseconds",
            ),
            &["url"],
        )
        .context("Failed to create validator_latency_p50_ms gauge")?;

        let validator_latency_p95_ms = GaugeVec::new(
            prometheus::Opts::new(
                "evm_benchmark_validator_latency_p95_ms",
                "Validator P95 latency in milliseconds",
            ),
            &["url"],
        )
        .context("Failed to create validator_latency_p95_ms gauge")?;

        let validator_latency_p99_ms = GaugeVec::new(
            prometheus::Opts::new(
                "evm_benchmark_validator_latency_p99_ms",
                "Validator P99 latency in milliseconds",
            ),
            &["url"],
        )
        .context("Failed to create validator_latency_p99_ms gauge")?;

        let validator_error_rate = GaugeVec::new(
            prometheus::Opts::new(
                "evm_benchmark_validator_error_rate",
                "Validator error rate (0-1)",
            ),
            &["url"],
        )
        .context("Failed to create validator_error_rate gauge")?;

        let validator_acceptance_rate = GaugeVec::new(
            prometheus::Opts::new(
                "evm_benchmark_validator_acceptance_rate",
                "Validator transaction acceptance rate (0-1)",
            ),
            &["url"],
        )
        .context("Failed to create validator_acceptance_rate gauge")?;

        // Register all metrics with the registry
        registry
            .register(Box::new(transactions_submitted_total.clone()))
            .context("Failed to register transactions_submitted_total")?;
        registry
            .register(Box::new(transactions_confirmed_total.clone()))
            .context("Failed to register transactions_confirmed_total")?;
        registry
            .register(Box::new(transactions_failed_total.clone()))
            .context("Failed to register transactions_failed_total")?;
        registry
            .register(Box::new(transactions_pending.clone()))
            .context("Failed to register transactions_pending")?;
        registry
            .register(Box::new(tps_current.clone()))
            .context("Failed to register tps_current")?;
        registry
            .register(Box::new(memory_bytes.clone()))
            .context("Failed to register memory_bytes")?;
        registry
            .register(Box::new(latency_submission_ms.clone()))
            .context("Failed to register latency_submission_ms")?;
        registry
            .register(Box::new(latency_confirmation_ms.clone()))
            .context("Failed to register latency_confirmation_ms")?;
        registry
            .register(Box::new(validator_health.clone()))
            .context("Failed to register validator_health")?;
        registry
            .register(Box::new(validator_latency_p50_ms.clone()))
            .context("Failed to register validator_latency_p50_ms")?;
        registry
            .register(Box::new(validator_latency_p95_ms.clone()))
            .context("Failed to register validator_latency_p95_ms")?;
        registry
            .register(Box::new(validator_latency_p99_ms.clone()))
            .context("Failed to register validator_latency_p99_ms")?;
        registry
            .register(Box::new(validator_error_rate.clone()))
            .context("Failed to register validator_error_rate")?;
        registry
            .register(Box::new(validator_acceptance_rate.clone()))
            .context("Failed to register validator_acceptance_rate")?;

        Ok(MetricsExporter {
            registry,
            transactions_submitted_total,
            transactions_confirmed_total,
            transactions_failed_total,
            transactions_pending,
            tps_current,
            memory_bytes,
            latency_submission_ms,
            latency_confirmation_ms,
            validator_health,
            validator_latency_p50_ms,
            validator_latency_p95_ms,
            validator_latency_p99_ms,
            validator_error_rate,
            validator_acceptance_rate,
        })
    }

    /// Increments the submitted transactions counter.
    pub fn inc_transactions_submitted(&self, count: u64) {
        self.transactions_submitted_total.inc_by(count);
    }

    /// Increments the confirmed transactions counter.
    pub fn inc_transactions_confirmed(&self, count: u64) {
        self.transactions_confirmed_total.inc_by(count);
    }

    /// Increments the failed transactions counter.
    pub fn inc_transactions_failed(&self, count: u64) {
        self.transactions_failed_total.inc_by(count);
    }

    /// Sets the number of pending transactions.
    pub fn set_pending_transactions(&self, count: i64) {
        self.transactions_pending.set(count);
    }

    /// Sets the current TPS (transactions per second).
    pub fn set_current_tps(&self, tps: f64) {
        self.tps_current.set(tps);
    }

    /// Sets the current memory usage in bytes.
    #[allow(dead_code)]
    pub fn set_memory_bytes(&self, bytes: f64) {
        self.memory_bytes.set(bytes);
    }

    /// Records a submission latency measurement in milliseconds.
    #[allow(dead_code)]
    pub fn observe_submission_latency_ms(&self, ms: f64) {
        self.latency_submission_ms.observe(ms);
    }

    /// Records a confirmation latency measurement in milliseconds.
    #[allow(dead_code)]
    pub fn observe_confirmation_latency_ms(&self, ms: f64) {
        self.latency_confirmation_ms.observe(ms);
    }

    /// Updates validator health metrics from a snapshot.
    #[allow(dead_code)]
    pub fn update_validator_health(&self, snapshot: &ValidatorHealthSnapshot) {
        let url = &snapshot.url;
        let health = if snapshot.is_connected { 1.0 } else { 0.0 };
        self.validator_health.with_label_values(&[url]).set(health);

        if let Some(p50) = snapshot.latency_p50_ms {
            self.validator_latency_p50_ms
                .with_label_values(&[url])
                .set(p50 as f64);
        }

        if let Some(p95) = snapshot.latency_p95_ms {
            self.validator_latency_p95_ms
                .with_label_values(&[url])
                .set(p95 as f64);
        }

        if let Some(p99) = snapshot.latency_p99_ms {
            self.validator_latency_p99_ms
                .with_label_values(&[url])
                .set(p99 as f64);
        }

        self.validator_error_rate
            .with_label_values(&[url])
            .set(snapshot.error_rate);

        self.validator_acceptance_rate
            .with_label_values(&[url])
            .set(snapshot.tx_acceptance_rate);
    }

    /// Returns the metrics in Prometheus text format.
    #[allow(dead_code)]
    pub fn gather(&self) -> Result<Vec<u8>> {
        use prometheus::Encoder;
        let encoder = prometheus::TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut buffer = Vec::new();
        encoder
            .encode(&metric_families, &mut buffer)
            .context("Failed to encode metrics")?;
        Ok(buffer)
    }

    /// Exports metrics as Prometheus-formatted text.
    #[allow(dead_code)]
    pub fn export_text(&self) -> Result<String> {
        use prometheus::Encoder;
        let encoder = prometheus::TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut buffer = Vec::new();
        encoder
            .encode(&metric_families, &mut buffer)
            .context("Failed to encode metrics")?;
        String::from_utf8(buffer).context("Metrics contain invalid UTF-8")
    }
}

impl Default for MetricsExporter {
    fn default() -> Self {
        Self::new().expect("Failed to create default MetricsExporter")
    }
}

/// Scrape a Prometheus metrics endpoint and return parsed metric values.
///
/// Performs an HTTP GET against `url`, parses the Prometheus text exposition
/// format, and returns a map of metric names to their numeric values. Comment
/// lines (starting with `#`) and empty lines are skipped. For lines with
/// labels (`metric{labels} value`), only the base metric name is used as the
/// key (everything before the `{`).
#[allow(dead_code)]
pub async fn scrape_prometheus(url: &Url) -> Result<MetricsMap> {
    let client = reqwest::Client::new();
    let response = client
        .get(url.as_str())
        .send()
        .await
        .context("failed to GET Prometheus metrics endpoint")?;
    let text = response
        .text()
        .await
        .context("failed to read Prometheus response body")?;
    parse_prometheus_text(&text)
}

/// Parse Prometheus text exposition format into a `MetricsMap`.
///
/// Each non-comment, non-empty line is expected to be one of:
/// - `metric_name value`
/// - `metric_name value timestamp`
/// - `metric_name{labels} value`
/// - `metric_name{labels} value timestamp`
fn parse_prometheus_text(text: &str) -> Result<MetricsMap> {
    let mut metrics = HashMap::new();

    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }

        // Extract the metric name (everything before `{` or first space).
        let metric_name = if let Some(idx) = line.find('{') {
            &line[..idx]
        } else if let Some(idx) = line.find(' ') {
            &line[..idx]
        } else {
            continue;
        };

        // Split on whitespace to find the value token.
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }

        // Value is always the second-to-last token when a timestamp is present,
        // or the last token when it is not. With labels collapsed into one token
        // by split_whitespace the layout is:
        //   2 parts -> [name_or_name{labels}, value]
        //   3+ parts -> [..., value, timestamp]
        let value_str = if parts.len() == 2 {
            parts[1]
        } else {
            parts[parts.len() - 2]
        };

        if let Ok(value) = value_str.parse::<f64>() {
            metrics.insert(metric_name.to_string(), value);
        }
    }

    Ok(metrics)
}

/// Compute server metrics deltas between before/after Prometheus snapshots.
///
/// For each histogram-style server metric, computes a [`HistogramDelta`] using
/// the `_sum` and `_count` suffixed keys from the Prometheus scrape. Returns
/// `None` only when none of the expected metrics are present in either snapshot.
#[allow(dead_code)]
pub fn compute_server_metrics(before: &MetricsMap, after: &MetricsMap) -> Option<ServerMetrics> {
    /// Build a [`HistogramDelta`] from before/after snapshots for the given
    /// base metric name (e.g. `reth_diesis_pipeline_execution_ms`).
    ///
    /// Looks for `{base}_sum` and `{base}_count` keys. If the `_sum` key is
    /// present in both snapshots, a delta is produced; otherwise `None`.
    fn histogram_delta(
        before: &MetricsMap,
        after: &MetricsMap,
        base: &str,
    ) -> Option<HistogramDelta> {
        let sum_key = format!("{base}_sum");
        let count_key = format!("{base}_count");

        let b_sum = before.get(&sum_key).or_else(|| before.get(base))?;
        let a_sum = after.get(&sum_key).or_else(|| after.get(base))?;

        let b_count = before.get(&count_key).copied().unwrap_or(0.0);
        let a_count = after.get(&count_key).copied().unwrap_or(0.0);

        Some(HistogramDelta {
            start: *b_sum,
            end: *a_sum,
            count: ((a_count - b_count).max(0.0)) as u64,
            sum: (a_sum - b_sum).max(0.0),
        })
    }

    let execution_ms = histogram_delta(before, after, "reth_diesis_pipeline_execution_ms");
    let state_root_ms = histogram_delta(before, after, "reth_diesis_pipeline_state_root_ms");
    let parent_handoff_ms =
        histogram_delta(before, after, "reth_diesis_pipeline_parent_handoff_wait_ms");
    let publication_ms = histogram_delta(before, after, "reth_diesis_pipeline_publication_ms");
    let queue_wait_ms = histogram_delta(before, after, "reth_diesis_pipeline_queue_wait_ms");

    // Return None only when every field is None (no server metrics available).
    if execution_ms.is_none()
        && state_root_ms.is_none()
        && parent_handoff_ms.is_none()
        && publication_ms.is_none()
        && queue_wait_ms.is_none()
    {
        return None;
    }

    Some(ServerMetrics {
        execution_ms,
        state_root_ms,
        parent_handoff_ms,
        publication_ms,
        queue_wait_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::HistogramDelta;

    #[test]
    fn test_metrics_exporter_creation() {
        let exporter = MetricsExporter::new();
        assert!(exporter.is_ok());
    }

    #[test]
    fn test_metrics_exporter_default() {
        let exporter = MetricsExporter::default();
        assert_eq!(exporter.transactions_submitted_total.get(), 0);
        assert_eq!(exporter.transactions_confirmed_total.get(), 0);
        assert_eq!(exporter.transactions_failed_total.get(), 0);
    }

    #[test]
    fn test_increment_submitted() {
        let exporter = MetricsExporter::new().expect("Failed to create exporter");
        exporter.inc_transactions_submitted(5);
        assert_eq!(exporter.transactions_submitted_total.get(), 5);
        exporter.inc_transactions_submitted(3);
        assert_eq!(exporter.transactions_submitted_total.get(), 8);
    }

    #[test]
    fn test_increment_confirmed() {
        let exporter = MetricsExporter::new().expect("Failed to create exporter");
        exporter.inc_transactions_confirmed(10);
        assert_eq!(exporter.transactions_confirmed_total.get(), 10);
    }

    #[test]
    fn test_increment_failed() {
        let exporter = MetricsExporter::new().expect("Failed to create exporter");
        exporter.inc_transactions_failed(2);
        assert_eq!(exporter.transactions_failed_total.get(), 2);
    }

    #[test]
    fn test_set_pending_transactions() {
        let exporter = MetricsExporter::new().expect("Failed to create exporter");
        exporter.set_pending_transactions(42);
        assert_eq!(exporter.transactions_pending.get(), 42);
        exporter.set_pending_transactions(50);
        assert_eq!(exporter.transactions_pending.get(), 50);
    }

    #[test]
    fn test_set_current_tps() {
        let exporter = MetricsExporter::new().expect("Failed to create exporter");
        exporter.set_current_tps(1234.5);
        assert!((exporter.tps_current.get() - 1234.5).abs() < 0.01);
    }

    #[test]
    fn test_set_memory_bytes() {
        let exporter = MetricsExporter::new().expect("Failed to create exporter");
        exporter.set_memory_bytes(1024.0 * 1024.0); // 1 MB
        assert!(
            (exporter.memory_bytes.get() - (1024.0 * 1024.0)).abs() < 1.0,
            "Memory should be approximately 1 MB"
        );
    }

    #[test]
    fn test_observe_latencies() {
        let exporter = MetricsExporter::new().expect("Failed to create exporter");
        exporter.observe_submission_latency_ms(100.0);
        exporter.observe_submission_latency_ms(150.0);
        exporter.observe_confirmation_latency_ms(200.0);
        exporter.observe_confirmation_latency_ms(250.0);

        // Metrics are recorded, but prometheus histograms don't expose individual values
        // We just verify the method calls don't panic
    }

    #[test]
    fn test_export_text_format() {
        let exporter = MetricsExporter::new().expect("Failed to create exporter");
        exporter.inc_transactions_submitted(5);
        exporter.inc_transactions_confirmed(3);
        exporter.set_pending_transactions(2);
        exporter.set_current_tps(100.0);

        let text = exporter.export_text().expect("Failed to export metrics");
        assert!(!text.is_empty());
        assert!(text.contains("evm_benchmark_transactions_submitted_total"));
        assert!(text.contains("evm_benchmark_transactions_confirmed_total"));
        assert!(text.contains("evm_benchmark_transactions_pending"));
        assert!(text.contains("evm_benchmark_tps_current"));
    }

    #[test]
    fn test_validator_health_update() {
        let exporter = MetricsExporter::new().expect("Failed to create exporter");

        let snapshot = ValidatorHealthSnapshot {
            url: "http://validator1:8545".to_string(),
            block_height: Some(1000),
            is_synced: true,
            availability_percent: 99.9,
            latency_p50_ms: Some(50),
            latency_p95_ms: Some(100),
            latency_p99_ms: Some(150),
            tx_acceptance_rate: 0.98,
            error_rate: 0.02,
            is_connected: true,
        };

        exporter.update_validator_health(&snapshot);

        // Verify the text export contains validator metrics
        let text = exporter.export_text().expect("Failed to export metrics");
        assert!(text.contains("evm_benchmark_validator_health"));
        assert!(text.contains("http://validator1:8545"));
    }

    #[test]
    fn test_metrics_map_creation() {
        let metrics: MetricsMap = HashMap::new();
        assert_eq!(metrics.len(), 0);
    }

    #[test]
    fn test_histogram_delta_creation() {
        let delta = HistogramDelta {
            start: 100.0,
            end: 200.0,
            count: 50,
            sum: 7500.0,
        };
        assert_eq!(delta.count, 50);
        assert!((delta.end - delta.start) == 100.0);
    }

    #[test]
    fn test_multiple_validators() {
        let exporter = MetricsExporter::new().expect("Failed to create exporter");

        let validator1 = ValidatorHealthSnapshot {
            url: "http://validator1:8545".to_string(),
            block_height: Some(1000),
            is_synced: true,
            availability_percent: 99.9,
            latency_p50_ms: Some(50),
            latency_p95_ms: Some(100),
            latency_p99_ms: Some(150),
            tx_acceptance_rate: 0.98,
            error_rate: 0.02,
            is_connected: true,
        };

        let validator2 = ValidatorHealthSnapshot {
            url: "http://validator2:8545".to_string(),
            block_height: Some(1000),
            is_synced: true,
            availability_percent: 99.5,
            latency_p50_ms: Some(60),
            latency_p95_ms: Some(110),
            latency_p99_ms: Some(160),
            tx_acceptance_rate: 0.95,
            error_rate: 0.05,
            is_connected: true,
        };

        exporter.update_validator_health(&validator1);
        exporter.update_validator_health(&validator2);

        let text = exporter.export_text().expect("Failed to export metrics");
        assert!(text.contains("http://validator1:8545"));
        assert!(text.contains("http://validator2:8545"));
    }

    #[test]
    fn test_with_registry_works() {
        let registry = prometheus::Registry::new();
        let exporter = MetricsExporter::with_registry(registry);
        assert!(exporter.is_ok());
        let exporter = exporter.unwrap();
        assert_eq!(exporter.transactions_submitted_total.get(), 0);
    }

    #[test]
    fn test_gather_returns_non_empty_bytes() {
        let exporter = MetricsExporter::new().expect("Failed to create exporter");
        exporter.inc_transactions_submitted(1);
        let bytes = exporter.gather().expect("Failed to gather metrics");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn test_parse_prometheus_basic() {
        let text = "# HELP metric help\n# TYPE metric gauge\nmy_metric 42.5\n";
        let map = parse_prometheus_text(text).expect("parse should succeed");
        assert_eq!(map.get("my_metric"), Some(&42.5));
    }

    #[test]
    fn test_parse_prometheus_with_labels() {
        let text = "http_requests{method=\"GET\"} 100\n";
        let map = parse_prometheus_text(text).expect("parse should succeed");
        assert_eq!(map.get("http_requests"), Some(&100.0));
    }

    #[test]
    fn test_parse_prometheus_with_timestamp() {
        let text = "my_counter 1234 1711526400000\n";
        let map = parse_prometheus_text(text).expect("parse should succeed");
        assert_eq!(map.get("my_counter"), Some(&1234.0));
    }

    #[test]
    fn test_parse_prometheus_skips_comments_and_empty() {
        let text = "# comment\n\n   \nvalid_metric 7\n";
        let map = parse_prometheus_text(text).expect("parse should succeed");
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("valid_metric"), Some(&7.0));
    }

    #[test]
    fn test_compute_server_metrics_returns_none_when_empty() {
        let before: MetricsMap = HashMap::new();
        let after: MetricsMap = HashMap::new();
        let result = compute_server_metrics(&before, &after);
        assert!(result.is_none());
    }

    #[test]
    fn test_compute_server_metrics_with_sum_count_keys() {
        let mut before: MetricsMap = HashMap::new();
        before.insert("reth_diesis_pipeline_execution_ms_sum".into(), 100.0);
        before.insert("reth_diesis_pipeline_execution_ms_count".into(), 10.0);
        before.insert("reth_diesis_pipeline_state_root_ms_sum".into(), 50.0);
        before.insert("reth_diesis_pipeline_state_root_ms_count".into(), 5.0);

        let mut after: MetricsMap = HashMap::new();
        after.insert("reth_diesis_pipeline_execution_ms_sum".into(), 300.0);
        after.insert("reth_diesis_pipeline_execution_ms_count".into(), 30.0);
        after.insert("reth_diesis_pipeline_state_root_ms_sum".into(), 120.0);
        after.insert("reth_diesis_pipeline_state_root_ms_count".into(), 12.0);

        let sm = compute_server_metrics(&before, &after).expect("should produce ServerMetrics");
        let exec = sm.execution_ms.expect("execution_ms should be Some");
        assert_eq!(exec.start, 100.0);
        assert_eq!(exec.end, 300.0);
        assert_eq!(exec.count, 20);
        assert!((exec.sum - 200.0).abs() < f64::EPSILON);

        let sr = sm.state_root_ms.expect("state_root_ms should be Some");
        assert_eq!(sr.count, 7);
        assert!((sr.sum - 70.0).abs() < f64::EPSILON);

        // Fields not present in snapshots should be None.
        assert!(sm.parent_handoff_ms.is_none());
        assert!(sm.publication_ms.is_none());
        assert!(sm.queue_wait_ms.is_none());
    }

    #[test]
    fn test_compute_server_metrics_with_plain_keys() {
        // When the server exposes plain gauge-style keys (no _sum/_count suffix),
        // compute_server_metrics falls back to using the base key directly.
        let mut before: MetricsMap = HashMap::new();
        before.insert("reth_diesis_pipeline_execution_ms".into(), 100.0);

        let mut after: MetricsMap = HashMap::new();
        after.insert("reth_diesis_pipeline_execution_ms".into(), 250.0);

        let sm = compute_server_metrics(&before, &after).expect("should produce ServerMetrics");
        let exec = sm.execution_ms.expect("execution_ms should be Some");
        assert!((exec.sum - 150.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compute_server_metrics_clamps_negative_delta() {
        let mut before: MetricsMap = HashMap::new();
        before.insert("reth_diesis_pipeline_queue_wait_ms_sum".into(), 200.0);
        before.insert("reth_diesis_pipeline_queue_wait_ms_count".into(), 20.0);

        let mut after: MetricsMap = HashMap::new();
        after.insert("reth_diesis_pipeline_queue_wait_ms_sum".into(), 100.0);
        after.insert("reth_diesis_pipeline_queue_wait_ms_count".into(), 10.0);

        let sm = compute_server_metrics(&before, &after).expect("should produce ServerMetrics");
        let qw = sm.queue_wait_ms.expect("queue_wait_ms should be Some");
        // Negative deltas are clamped to 0.
        assert_eq!(qw.sum, 0.0);
        assert_eq!(qw.count, 0);
    }

    #[test]
    fn test_export_text_after_validator_health_with_none_latencies() {
        let exporter = MetricsExporter::new().expect("Failed to create exporter");

        let snapshot = ValidatorHealthSnapshot {
            url: "http://partial:8545".to_string(),
            block_height: None,
            is_synced: false,
            availability_percent: 50.0,
            latency_p50_ms: None,
            latency_p95_ms: None,
            latency_p99_ms: None,
            tx_acceptance_rate: 0.5,
            error_rate: 0.5,
            is_connected: true,
        };

        exporter.update_validator_health(&snapshot);

        let text = exporter.export_text().expect("Failed to export metrics");
        assert!(text.contains("http://partial:8545"));
        // Health gauge should still be set (connected = 1.0)
        assert!(text.contains("evm_benchmark_validator_health"));
    }

    #[test]
    fn test_multiple_increments_of_all_counters() {
        let exporter = MetricsExporter::new().expect("Failed to create exporter");

        exporter.inc_transactions_submitted(10);
        exporter.inc_transactions_submitted(20);
        assert_eq!(exporter.transactions_submitted_total.get(), 30);

        exporter.inc_transactions_confirmed(5);
        exporter.inc_transactions_confirmed(15);
        assert_eq!(exporter.transactions_confirmed_total.get(), 20);

        exporter.inc_transactions_failed(1);
        exporter.inc_transactions_failed(2);
        exporter.inc_transactions_failed(3);
        assert_eq!(exporter.transactions_failed_total.get(), 6);

        exporter.set_pending_transactions(100);
        assert_eq!(exporter.transactions_pending.get(), 100);
        exporter.set_pending_transactions(50);
        assert_eq!(exporter.transactions_pending.get(), 50);

        exporter.set_current_tps(999.9);
        assert!((exporter.tps_current.get() - 999.9).abs() < 0.01);

        exporter.set_memory_bytes(2048.0);
        assert!((exporter.memory_bytes.get() - 2048.0).abs() < 0.01);
    }
}
