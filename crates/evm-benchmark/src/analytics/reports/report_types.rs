//! Shared report data structures and utilities.

use crate::types::{AnalyticsReport, BottleneckFinding, Recommendation};

/// Report builder interface for all formats
pub trait ReportBuilder {
    fn add_header(&mut self, title: &str, mode: &str) -> &mut Self;
    fn add_metrics_summary(&mut self, report: &AnalyticsReport) -> &mut Self;
    fn add_bottlenecks(&mut self, bottlenecks: &[BottleneckFinding]) -> &mut Self;
    fn add_recommendations(&mut self, recommendations: &[Recommendation]) -> &mut Self;
    fn build(&self) -> String;
}

/// Format large numbers with commas for readability
pub fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.insert(0, ',');
        }
        result.insert(0, c);
    }
    result
}

/// Format percentage with 1 decimal place
pub fn format_pct(pct: f32) -> String {
    format!("{:.1}%", pct)
}

/// Format milliseconds with units
pub fn format_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else {
        format!("{:.2}s", ms as f32 / 1000.0)
    }
}

/// Format memory in bytes with units
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f32 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1}MB", bytes as f32 / (1024.0 * 1024.0))
    } else {
        format!("{:.2}GB", bytes as f32 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// Severity color for terminal output (ANSI codes)
pub fn severity_color(severity: f32) -> &'static str {
    match severity {
        s if s >= 0.8 => "\x1b[91m", // Bright red
        s if s >= 0.6 => "\x1b[33m", // Yellow
        s if s >= 0.4 => "\x1b[36m", // Cyan
        _ => "\x1b[32m",             // Green
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_number() {
        assert_eq!(format_number(1000), "1,000");
        assert_eq!(format_number(1000000), "1,000,000");
        assert_eq!(format_number(42), "42");
    }

    #[test]
    fn test_format_pct() {
        assert_eq!(format_pct(50.5), "50.5%");
        assert_eq!(format_pct(99.99), "100.0%");
    }

    #[test]
    fn test_format_ms() {
        assert_eq!(format_ms(100), "100ms");
        assert_eq!(format_ms(5000), "5.00s");
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(512), "512B");
        assert_eq!(format_bytes(1024), "1.0KB");
        assert_eq!(format_bytes(1048576), "1.0MB");
    }

    #[test]
    fn test_format_bytes_gigabytes() {
        // 1 GB
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00GB");
        // 2.5 GB
        assert_eq!(
            format_bytes((2.5 * 1024.0 * 1024.0 * 1024.0) as u64),
            "2.50GB"
        );
    }

    #[test]
    fn test_format_bytes_zero() {
        assert_eq!(format_bytes(0), "0B");
    }

    #[test]
    fn test_format_bytes_boundary_values() {
        assert_eq!(format_bytes(1023), "1023B");
        assert_eq!(format_bytes(1024 * 1024 - 1), "1024.0KB");
        assert_eq!(format_bytes(1024 * 1024 * 1024 - 1), "1024.0MB");
    }

    #[test]
    fn test_format_number_zero() {
        assert_eq!(format_number(0), "0");
    }

    #[test]
    fn test_format_number_single_digit() {
        assert_eq!(format_number(5), "5");
    }

    #[test]
    fn test_format_number_large() {
        assert_eq!(format_number(1234567890), "1,234,567,890");
    }

    #[test]
    fn test_format_pct_zero() {
        assert_eq!(format_pct(0.0), "0.0%");
    }

    #[test]
    fn test_format_pct_hundred() {
        assert_eq!(format_pct(100.0), "100.0%");
    }

    #[test]
    fn test_format_ms_boundary() {
        assert_eq!(format_ms(999), "999ms");
        assert_eq!(format_ms(1000), "1.00s");
        assert_eq!(format_ms(1500), "1.50s");
    }

    #[test]
    fn test_format_ms_zero() {
        assert_eq!(format_ms(0), "0ms");
    }

    #[test]
    fn test_severity_color_high() {
        let color = severity_color(0.9);
        assert_eq!(color, "\x1b[91m"); // Bright red
    }

    #[test]
    fn test_severity_color_medium_high() {
        let color = severity_color(0.7);
        assert_eq!(color, "\x1b[33m"); // Yellow
    }

    #[test]
    fn test_severity_color_medium() {
        let color = severity_color(0.5);
        assert_eq!(color, "\x1b[36m"); // Cyan
    }

    #[test]
    fn test_severity_color_low() {
        let color = severity_color(0.2);
        assert_eq!(color, "\x1b[32m"); // Green
    }

    #[test]
    fn test_severity_color_boundary_values() {
        assert_eq!(severity_color(0.8), "\x1b[91m"); // Exactly 0.8 -> red
        assert_eq!(severity_color(0.6), "\x1b[33m"); // Exactly 0.6 -> yellow
        assert_eq!(severity_color(0.4), "\x1b[36m"); // Exactly 0.4 -> cyan
        assert_eq!(severity_color(0.39), "\x1b[32m"); // Just below 0.4 -> green
    }
}
