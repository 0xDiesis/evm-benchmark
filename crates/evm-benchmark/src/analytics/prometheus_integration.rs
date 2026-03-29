use anyhow::Result;
use std::collections::HashMap;
use url::Url;

/// Raw metrics map from Prometheus (metric_name -> value)
pub type MetricsMap = HashMap<String, f64>;

/// Scrape Prometheus metrics endpoint and parse text format
pub async fn scrape_prometheus(url: &Url) -> Result<MetricsMap> {
    let client = reqwest::Client::new();
    let response = client.get(url.as_str()).send().await?;
    let text = response.text().await?;
    parse_prometheus_text(&text)
}

/// Parse Prometheus text format (lines like "metric_name{labels} value timestamp")
fn parse_prometheus_text(text: &str) -> Result<MetricsMap> {
    let mut metrics = HashMap::new();

    for line in text.lines() {
        // Skip comments and empty lines
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }

        // Parse "metric_name{...} value" or "metric_name{...} value timestamp" format
        // Extract metric name (everything before { or space)
        let metric_part = if let Some(idx) = line.find('{') {
            &line[..idx]
        } else if let Some(idx) = line.find(' ') {
            &line[..idx]
        } else {
            continue;
        };

        // Extract value from the metric line
        // Prometheus format: metric_name [labels] value [timestamp]
        // We want the numeric value which is always second-to-last or last token
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        // Determine which part is the value:
        // - If 2 parts: [metric_name, value]
        // - If 3+ parts: [metric_name, value, timestamp] or [metric_name{labels}, value, timestamp]
        let value_str = if parts.len() == 2 {
            // No timestamp case
            parts[1]
        } else if parts.len() >= 3 {
            // Has timestamp (or labels) - value is second-to-last
            parts[parts.len() - 2]
        } else {
            continue;
        };

        if let Ok(value) = value_str.parse::<f64>() {
            metrics.insert(metric_part.to_string(), value);
        }
    }

    Ok(metrics)
}

/// Calculate delta (absolute difference) between two metric snapshots.
///
/// # Arguments
/// * `before` - Metrics snapshot before benchmark
/// * `after` - Metrics snapshot after benchmark
/// * `metric` - Name of the metric to compare
///
/// # Returns
/// `Some(delta)` if metric exists in both snapshots, `None` otherwise.
pub fn calculate_metric_delta(
    before: &MetricsMap,
    after: &MetricsMap,
    metric: &str,
) -> Option<f64> {
    match (before.get(metric), after.get(metric)) {
        (Some(b), Some(a)) => Some(a - b),
        _ => None,
    }
}

/// Calculate percentage change between two metric snapshots.
///
/// # Arguments
/// * `before` - Metrics snapshot before benchmark
/// * `after` - Metrics snapshot after benchmark
/// * `metric` - Name of the metric to compare
///
/// # Returns
/// `Some(pct_change)` if metric exists in both snapshots and baseline is non-zero, `None` otherwise.
/// Result is in percentage (e.g., 25.0 for 25% increase).
pub fn calculate_metric_pct_change(
    before: &MetricsMap,
    after: &MetricsMap,
    metric: &str,
) -> Option<f32> {
    match (before.get(metric), after.get(metric)) {
        (Some(b), Some(a)) if *b != 0.0 => Some(((*a - *b) / *b) as f32 * 100.0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_prometheus_text_with_timestamp() {
        let text = r#"# HELP evm_benchmark_transactions_submitted_total Total transactions submitted
# TYPE evm_benchmark_transactions_submitted_total counter
evm_benchmark_transactions_submitted_total 1000 1711526400000
evm_benchmark_tps_current 3102.5 1711526400000
"#;
        let metrics =
            parse_prometheus_text(text).expect("Failed to parse Prometheus text with timestamps");
        assert_eq!(
            metrics.get("evm_benchmark_transactions_submitted_total"),
            Some(&1000.0)
        );
        assert_eq!(metrics.get("evm_benchmark_tps_current"), Some(&3102.5));
    }

    #[test]
    fn test_parse_prometheus_text_without_timestamp() {
        let text = r#"# HELP evm_benchmark_transactions_submitted_total Total transactions submitted
evm_benchmark_transactions_submitted_total 500
evm_benchmark_tps_current 2500.0
"#;
        let metrics = parse_prometheus_text(text)
            .expect("Failed to parse Prometheus text without timestamps");
        assert_eq!(
            metrics.get("evm_benchmark_transactions_submitted_total"),
            Some(&500.0)
        );
        assert_eq!(metrics.get("evm_benchmark_tps_current"), Some(&2500.0));
    }

    #[test]
    fn test_parse_prometheus_text_with_labels() {
        let text = r#"node_cpu_seconds_total{cpu="0",mode="user"} 42.5 1711526400000
node_memory_bytes{instance="localhost:9090"} 1024000
"#;
        let metrics =
            parse_prometheus_text(text).expect("Failed to parse Prometheus text with labels");
        assert_eq!(metrics.get("node_cpu_seconds_total"), Some(&42.5));
        assert_eq!(metrics.get("node_memory_bytes"), Some(&1024000.0));
    }

    #[test]
    fn test_parse_prometheus_empty_input() {
        let text = "";
        let metrics = parse_prometheus_text(text).expect("Failed to parse empty Prometheus text");
        assert!(metrics.is_empty());
    }

    #[test]
    fn test_parse_prometheus_only_comments() {
        let text = "# HELP metric description\n# TYPE metric counter\n";
        let metrics = parse_prometheus_text(text)
            .expect("Failed to parse Prometheus text with only comments");
        assert!(metrics.is_empty());
    }

    #[test]
    fn test_parse_prometheus_invalid_values() {
        let text = r#"metric_valid 42.0
metric_invalid not_a_number
metric_valid2 7.89
"#;
        let metrics = parse_prometheus_text(text)
            .expect("Failed to parse Prometheus text with invalid values");
        assert_eq!(metrics.get("metric_valid"), Some(&42.0));
        assert_eq!(metrics.get("metric_invalid"), None); // Should be skipped
        assert_eq!(metrics.get("metric_valid2"), Some(&7.89));
    }

    #[test]
    fn test_calculate_metric_delta() {
        let mut before = HashMap::new();
        before.insert("test_metric".to_string(), 100.0);

        let mut after = HashMap::new();
        after.insert("test_metric".to_string(), 145.0);

        assert_eq!(
            calculate_metric_delta(&before, &after, "test_metric"),
            Some(45.0)
        );
    }

    #[test]
    fn test_calculate_metric_delta_missing_metric() {
        let before = HashMap::new();
        let after = HashMap::new();

        assert_eq!(calculate_metric_delta(&before, &after, "test_metric"), None);
    }

    #[test]
    fn test_calculate_metric_delta_negative() {
        let mut before = HashMap::new();
        before.insert("test_metric".to_string(), 100.0);

        let mut after = HashMap::new();
        after.insert("test_metric".to_string(), 75.0);

        assert_eq!(
            calculate_metric_delta(&before, &after, "test_metric"),
            Some(-25.0)
        );
    }

    #[test]
    fn test_calculate_metric_pct_change() {
        let mut before = HashMap::new();
        before.insert("test_metric".to_string(), 100.0);

        let mut after = HashMap::new();
        after.insert("test_metric".to_string(), 150.0);

        let pct = calculate_metric_pct_change(&before, &after, "test_metric");
        assert!(pct.is_some());
        assert!((pct.unwrap() - 50.0).abs() < 0.1); // 50% increase
    }

    #[test]
    fn test_calculate_metric_pct_change_zero_baseline() {
        let mut before = HashMap::new();
        before.insert("test_metric".to_string(), 0.0);

        let mut after = HashMap::new();
        after.insert("test_metric".to_string(), 100.0);

        // Should return None when baseline is zero (avoid division by zero)
        assert_eq!(
            calculate_metric_pct_change(&before, &after, "test_metric"),
            None
        );
    }

    #[test]
    fn test_calculate_metric_pct_change_missing_metric() {
        let before = HashMap::new();
        let after = HashMap::new();

        assert_eq!(
            calculate_metric_pct_change(&before, &after, "test_metric"),
            None
        );
    }

    #[test]
    fn test_parse_prometheus_multiline_mixed() {
        let text = r#"# HELP go_goroutines Number of goroutines
# TYPE go_goroutines gauge
go_goroutines 42
# HELP process_cpu_seconds_total
# TYPE process_cpu_seconds_total counter
process_cpu_seconds_total 12345.67

reth_diesis_pipeline_execution_ms 250.5
"#;
        let metrics = parse_prometheus_text(text).unwrap();
        assert_eq!(metrics.get("go_goroutines"), Some(&42.0));
        assert_eq!(metrics.get("process_cpu_seconds_total"), Some(&12345.67));
        assert_eq!(
            metrics.get("reth_diesis_pipeline_execution_ms"),
            Some(&250.5)
        );
    }

    #[test]
    fn test_parse_prometheus_whitespace_only_lines() {
        let text = "   \n\n  \nmetric_a 10\n   \nmetric_b 20\n";
        let metrics = parse_prometheus_text(text).unwrap();
        assert_eq!(metrics.get("metric_a"), Some(&10.0));
        assert_eq!(metrics.get("metric_b"), Some(&20.0));
    }

    #[test]
    fn test_calculate_metric_delta_same_value() {
        let mut before = HashMap::new();
        before.insert("m".to_string(), 42.0);
        let mut after = HashMap::new();
        after.insert("m".to_string(), 42.0);

        assert_eq!(calculate_metric_delta(&before, &after, "m"), Some(0.0));
    }

    #[test]
    fn test_calculate_metric_pct_change_decrease() {
        let mut before = HashMap::new();
        before.insert("m".to_string(), 200.0);
        let mut after = HashMap::new();
        after.insert("m".to_string(), 150.0);

        let pct = calculate_metric_pct_change(&before, &after, "m");
        assert!(pct.is_some());
        assert!((pct.unwrap() - (-25.0)).abs() < 0.1); // 25% decrease
    }

    #[test]
    fn test_calculate_metric_pct_change_only_in_before() {
        let mut before = HashMap::new();
        before.insert("m".to_string(), 100.0);
        let after = HashMap::new();

        assert_eq!(calculate_metric_pct_change(&before, &after, "m"), None);
    }

    #[test]
    fn test_calculate_metric_pct_change_only_in_after() {
        let before = HashMap::new();
        let mut after = HashMap::new();
        after.insert("m".to_string(), 100.0);

        assert_eq!(calculate_metric_pct_change(&before, &after, "m"), None);
    }

    #[test]
    fn test_parse_prometheus_scientific_notation() {
        let text = "metric_sci 1.23e4\n";
        let metrics = parse_prometheus_text(text).unwrap();
        assert_eq!(metrics.get("metric_sci"), Some(&12300.0));
    }

    #[test]
    fn test_parse_prometheus_negative_value() {
        let text = "metric_neg -42.5\n";
        let metrics = parse_prometheus_text(text).unwrap();
        assert_eq!(metrics.get("metric_neg"), Some(&-42.5));
    }

    #[test]
    fn test_parse_prometheus_line_with_no_space_or_brace() {
        // A line that has no space and no brace should be skipped
        let text = "metricwithoutvalue\nmetric_ok 10\n";
        let metrics = parse_prometheus_text(text).unwrap();
        assert!(!metrics.contains_key("metricwithoutvalue"));
        assert_eq!(metrics.get("metric_ok"), Some(&10.0));
    }

    #[test]
    fn test_parse_prometheus_labels_with_timestamp() {
        // Line with labels AND a timestamp: 3+ parts after splitting on whitespace
        let text = "http_requests_total{method=\"GET\",code=\"200\"} 1027 1395066363000\n";
        let metrics = parse_prometheus_text(text).unwrap();
        assert_eq!(metrics.get("http_requests_total"), Some(&1027.0));
    }

    #[test]
    fn test_parse_prometheus_labels_without_timestamp() {
        // Line with labels but NO timestamp: the label part counts as one whitespace token
        // "metric{label=\"value\"} 42" -> 2 parts
        let text = "http_requests_total{method=\"GET\"} 42\n";
        let metrics = parse_prometheus_text(text).unwrap();
        assert_eq!(metrics.get("http_requests_total"), Some(&42.0));
    }

    #[test]
    fn test_parse_prometheus_inf_value() {
        let text = "metric_inf +Inf\n";
        let metrics = parse_prometheus_text(text).unwrap();
        let val = metrics.get("metric_inf");
        assert!(val.is_some());
        assert!(val.unwrap().is_infinite());
    }

    #[test]
    fn test_parse_prometheus_nan_value() {
        let text = "metric_nan NaN\n";
        let metrics = parse_prometheus_text(text).unwrap();
        let val = metrics.get("metric_nan");
        assert!(val.is_some());
        assert!(val.unwrap().is_nan());
    }

    #[test]
    fn test_calculate_metric_delta_only_in_before() {
        let mut before = HashMap::new();
        before.insert("m".to_string(), 42.0);
        let after = HashMap::new();
        assert_eq!(calculate_metric_delta(&before, &after, "m"), None);
    }

    #[test]
    fn test_calculate_metric_delta_only_in_after() {
        let before = HashMap::new();
        let mut after = HashMap::new();
        after.insert("m".to_string(), 42.0);
        assert_eq!(calculate_metric_delta(&before, &after, "m"), None);
    }

    #[test]
    fn test_calculate_metric_pct_change_no_change() {
        let mut before = HashMap::new();
        before.insert("m".to_string(), 100.0);
        let mut after = HashMap::new();
        after.insert("m".to_string(), 100.0);
        let pct = calculate_metric_pct_change(&before, &after, "m");
        assert!(pct.is_some());
        assert!((pct.unwrap()).abs() < 0.001);
    }

    #[test]
    fn test_parse_prometheus_single_whitespace_token_line() {
        // A line with exactly one whitespace-separated token (no value at all)
        let text = "   singletoken   \n";
        let metrics = parse_prometheus_text(text).unwrap();
        assert!(metrics.is_empty());
    }
}
