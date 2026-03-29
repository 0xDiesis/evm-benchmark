//! Multi-validator health monitoring
//!
//! Provides health tracking and metrics for multiple validator endpoints.

#[allow(dead_code)]
pub mod health_monitor;

#[allow(unused_imports)]
pub use health_monitor::{HealthMonitor, ValidatorHealth};
