pub mod analysis_engine;
pub mod bottleneck_detector;
pub mod metrics_collector;
#[allow(dead_code)]
pub mod prometheus_integration;
pub mod recommendations;
pub mod regression_detector;
#[allow(dead_code)]
pub mod reports;

pub use analysis_engine::run_analysis;
