//! Multi-format report generation module for benchmark analytics.
//!
//! Provides generators for JSON, ASCII, Markdown, and HTML reports to serve different audiences:
//! - JSON: Machine-readable, CI/CD integration
//! - ASCII: Terminal-friendly tables
//! - Markdown: Documentation format
//! - HTML: Interactive visual analysis

pub mod ascii_report;
pub mod html_report;
pub mod json_report;
pub mod markdown_report;
pub mod report_types;

pub use ascii_report::generate_ascii_report;
pub use html_report::generate_html_report;
pub use json_report::generate_json_report;
pub use markdown_report::generate_markdown_report;
