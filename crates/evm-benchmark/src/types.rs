use alloy_primitives::{Address, B256};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

#[derive(Clone, Debug, Copy, PartialEq, Eq, Hash)]
pub enum TestMode {
    Transfer,
    Evm,
}

#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    Burst,
    Sustained,
    Ceiling,
}

#[derive(Clone, Debug, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum SignMode {
    Inline,
    Presigned,
}

#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum TransactionType {
    /// Simple ETH value transfer (21k gas).
    SimpleTransfer,
    /// ERC-20 `mint(address,uint256)`.
    ERC20Mint,
    /// ERC-20 `transfer(address,uint256)`.
    ERC20Transfer,
    /// ERC-20 `approve(address,uint256)`.
    ERC20Approve,
    /// AMM `swap(uint256,bool)`.
    Swap,
    /// NFT `mint(address)`.
    NFTMint,
    /// Plain ETH transfer within an EVM mix (distinct from [`SimpleTransfer`]).
    ETHTransfer,
}

// Per-transaction tracking
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct TxRecord {
    pub hash: B256,
    pub nonce: u64,
    pub sender: Address,
    pub gas_limit: u64,
    pub gas_used: Option<u64>,
    pub submit_time: Instant,
    pub block_time: Option<Instant>,
    pub method: TransactionType,
    pub revert_status: Option<bool>,
    /// Wave index for per-wave latency tracking in burst mode.
    pub wave: Option<u32>,
}

impl TxRecord {
    #[allow(dead_code)]
    pub fn latency_ms(&self) -> Option<u64> {
        self.block_time
            .map(|bt| (bt - self.submit_time).as_millis() as u64)
    }
}

// Signed transaction with metadata
#[derive(Clone)]
#[allow(dead_code)]
pub struct SignedTxWithMetadata {
    pub hash: B256,
    pub encoded: Vec<u8>,
    pub nonce: u64,
    pub gas_limit: u64,
    pub sender: Address,
    pub submit_time: Instant,
    pub method: TransactionType,
}

// Latency statistics
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LatencyStats {
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
    pub min: u64,
    pub max: u64,
    pub avg: u64,
}

// Per-method statistics for EVM benchmarks
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PerMethodStats {
    pub count: u32,
    pub confirmed: u32,
    pub reverted: u32,
    pub avg_gas: u64,
    pub latency_p50: u64,
    pub latency_p95: u64,
}

// Server metrics from Prometheus
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistogramDelta {
    pub start: f64,
    pub end: f64,
    pub count: u64,
    pub sum: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServerMetrics {
    pub execution_ms: Option<HistogramDelta>,
    pub state_root_ms: Option<HistogramDelta>,
    pub parent_handoff_ms: Option<HistogramDelta>,
    pub publication_ms: Option<HistogramDelta>,
    pub queue_wait_ms: Option<HistogramDelta>,
}

// Burst mode results
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BurstResult {
    pub submitted: u32,
    pub confirmed: u32,
    pub pending: u32,
    pub sign_ms: u64,
    pub submit_ms: u64,
    pub confirm_ms: u64,
    pub submitted_tps: f32,
    pub confirmed_tps: f32,
    pub latency: LatencyStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_metrics: Option<ServerMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_method: Option<BTreeMap<String, PerMethodStats>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validator_health: Option<Vec<ValidatorHealthSnapshot>>,
    /// Per-wave latency breakdown (burst mode only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_wave: Option<Vec<crate::submission::tracking::WaveEntry>>,
}

/// Validator health snapshot for reporting
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatorHealthSnapshot {
    pub url: String,
    pub block_height: Option<u64>,
    pub is_synced: bool,
    pub availability_percent: f64,
    pub latency_p50_ms: Option<u64>,
    pub latency_p95_ms: Option<u64>,
    pub latency_p99_ms: Option<u64>,
    pub tx_acceptance_rate: f64,
    pub error_rate: f64,
    pub is_connected: bool,
}

// Sustained mode results with per-second timeline
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowEntry {
    pub second: u32,
    pub sent: u32,
    pub confirmed: u32,
    pub latency_p50: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SustainedResult {
    pub sent: u32,
    pub confirmed: u32,
    pub pending: u32,
    pub errors: u32,
    pub duration_ms: u64,
    pub actual_tps: f32,
    pub latency: LatencyStats,
    pub timeline: Vec<WindowEntry>,
}

impl SustainedResult {
    /// Convert to a [`BurstResult`] for the analytics pipeline.
    ///
    /// Sustained mode interleaves submission and confirmation, so there is no
    /// distinct confirm phase. `submit_ms` carries the total wall-clock duration
    /// and `confirm_ms` is set to zero to avoid double-counting.
    pub fn to_burst_result(&self) -> BurstResult {
        BurstResult {
            submitted: self.sent,
            confirmed: self.confirmed,
            pending: self.pending,
            sign_ms: 0,
            submit_ms: self.duration_ms,
            confirm_ms: 0,
            submitted_tps: self.actual_tps,
            confirmed_tps: self.actual_tps,
            latency: self.latency.clone(),
            server_metrics: None,
            per_method: None,
            validator_health: None,
            per_wave: None,
        }
    }
}

// Ceiling mode results with ramp-up steps
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CeilingStep {
    pub target_tps: u32,
    pub actual_tps: u32,
    pub pending_ratio: f32,
    pub error_rate: f32,
    pub duration_ms: u64,
    pub is_saturated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CeilingResult {
    pub steps: Vec<CeilingStep>,
    pub ceiling_tps: u32,
    pub burst_peak_tps: u32,
    pub confidence_score: f32,
    pub confidence_band_low: u32,
    pub confidence_band_high: u32,
    pub adaptive_step_enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CeilingAnalysis {
    pub confidence_score: f32,
    pub confidence_band_low: u32,
    pub confidence_band_high: u32,
    pub adaptive_step_enabled: bool,
    pub sampled_steps: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CostEfficiencyReport {
    pub estimated_total_gas: u64,
    pub estimated_total_fee_wei: String,
    pub estimated_total_fee_eth: f64,
    pub confirmed_per_eth: f64,
}

// Full benchmark report
#[derive(Serialize, Deserialize)]
pub struct BenchmarkReport {
    pub benchmark: String,
    pub captured_at: String,
    pub chain_id: u64,
    pub config: ConfigSnapshot,
    pub results: BurstResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ceiling_analysis: Option<CeilingAnalysis>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_efficiency: Option<CostEfficiencyReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_pack: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfigSnapshot {
    pub test_mode: String,
    pub execution_mode: String,
    pub tx_count: u32,
    pub sender_count: u32,
    pub wave_count: u32,
    pub wave_delay_ms: u64,
    pub worker_count: u32,
}

// Analytics types for unified metrics collection
/// Benchmark harness metrics extracted from execution results.
///
/// Contains submission TPS, confirmation TPS, latencies at various percentiles,
/// confirmation rate, and memory footprint metrics.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HarnessMetrics {
    pub tps_submitted: f32,
    pub tps_confirmed: f32,
    pub latency_p50: u64,
    pub latency_p95: u64,
    pub latency_p99: u64,
    pub confirmation_rate: f32, // confirmed / submitted
    pub pending_ratio: f32,     // pending / submitted
    pub error_rate: f32,
    pub memory_bytes: u64,
}

/// Server-side metrics extracted from Prometheus.
///
/// Contains timing metrics for different pipeline stages (execution, state root computation,
/// publication), and block-level metrics (gas, transactions, memory).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UnifiedServerMetrics {
    pub block_execution_ms: u64,
    pub state_root_ms: u64,
    pub parent_handoff_ms: u64,
    pub publication_ms: u64,
    pub queue_wait_ms: u64,
    pub gas_per_block: u64,
    pub transactions_per_block: u32,
    pub memory_usage_mb: u64,
}

/// Combined snapshot of harness and server metrics at a point in time.
///
/// Represents a correlated view of client-side and server-side performance,
/// timestamped for timeline analysis.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PerformanceSnapshot {
    pub timestamp: DateTime<Utc>,
    pub harness_metrics: HarnessMetrics,
    pub server_metrics: UnifiedServerMetrics,
    pub correlation_confidence: f32, // 0.0-1.0, how aligned are the timestamps?
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BottleneckFinding {
    pub bottleneck_type: String, // "StateRootComputation", "ClientSigning", etc
    pub severity: f32,           // 0.0-1.0
    pub pct_of_total: f32,       // % of total time
    pub details: String,
    pub stack_attribution: HashMap<String, f32>, // "harness" -> 0%, "execution" -> 45%, etc
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegressionAnalysis {
    pub tps_delta: f32,
    pub tps_pct_change: f32,
    pub latency_delta_ms: i32,
    pub p_value: f32,    // Statistical significance
    pub verdict: String, // "regressed", "stable", "improved"
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Recommendation {
    pub priority: String, // "Critical", "High", "Medium", "Low"
    pub title: String,
    pub description: String,
    pub estimated_tps_improvement_pct: f32,
    pub effort_level: String, // "Low", "Medium", "High"
    pub roi_score: f32,       // impact / effort
    pub implementation_hints: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReportPackage {
    pub json: String,     // Serialized JSON report
    pub html: String,     // Serialized HTML report
    pub ascii: String,    // ASCII art report
    pub markdown: String, // Markdown report
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnalyticsReport {
    pub benchmark_name: String,
    pub execution_mode: String,
    pub timestamp: DateTime<Utc>,
    pub snapshot: PerformanceSnapshot,
    pub bottlenecks: Vec<BottleneckFinding>,
    pub regression_analysis: Option<RegressionAnalysis>,
    pub recommendations: Vec<Recommendation>,
    pub reports: ReportPackage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sustained_result_to_burst_result() {
        let sustained = SustainedResult {
            sent: 500,
            confirmed: 480,
            pending: 20,
            errors: 3,
            duration_ms: 10_000,
            actual_tps: 48.0,
            latency: LatencyStats {
                p50: 100,
                p95: 250,
                p99: 400,
                min: 10,
                max: 500,
                avg: 120,
            },
            timeline: vec![],
        };

        let burst = sustained.to_burst_result();

        assert_eq!(burst.submitted, 500);
        assert_eq!(burst.confirmed, 480);
        assert_eq!(burst.pending, 20);
        assert_eq!(burst.submit_ms, 10_000);
        assert_eq!(burst.confirm_ms, 0);
        assert_eq!(burst.submitted_tps, 48.0);
        assert_eq!(burst.confirmed_tps, 48.0);
        assert_eq!(burst.sign_ms, 0);
        assert!(burst.server_metrics.is_none());
        assert!(burst.per_method.is_none());
        assert!(burst.validator_health.is_none());

        // Latency fields should be cloned from the source
        assert_eq!(burst.latency.p50, 100);
        assert_eq!(burst.latency.p95, 250);
        assert_eq!(burst.latency.p99, 400);
        assert_eq!(burst.latency.min, 10);
        assert_eq!(burst.latency.max, 500);
        assert_eq!(burst.latency.avg, 120);
    }

    #[test]
    fn test_tx_record_latency_ms_with_block_time() {
        let submit = Instant::now();
        let block = submit + std::time::Duration::from_millis(150);
        let rec = TxRecord {
            hash: B256::ZERO,
            nonce: 0,
            sender: Address::ZERO,
            gas_limit: 21_000,
            gas_used: Some(21_000),
            submit_time: submit,
            block_time: Some(block),
            method: TransactionType::SimpleTransfer,
            revert_status: None,
            wave: None,
        };
        let lat = rec.latency_ms().unwrap();
        assert!(lat >= 150, "expected >=150, got {lat}");
    }

    #[test]
    fn test_tx_record_latency_ms_without_block_time() {
        let rec = TxRecord {
            hash: B256::ZERO,
            nonce: 1,
            sender: Address::ZERO,
            gas_limit: 21_000,
            gas_used: None,
            submit_time: Instant::now(),
            block_time: None,
            method: TransactionType::SimpleTransfer,
            revert_status: None,
            wave: None,
        };
        assert!(rec.latency_ms().is_none());
    }

    #[test]
    fn test_latency_stats_serde_roundtrip() {
        let stats = LatencyStats {
            p50: 10,
            p95: 20,
            p99: 30,
            min: 1,
            max: 50,
            avg: 15,
        };
        let json = serde_json::to_string(&stats).unwrap();
        let deser: LatencyStats = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.p50, 10);
        assert_eq!(deser.max, 50);
    }

    #[test]
    fn test_config_snapshot_serde_roundtrip() {
        let snap = ConfigSnapshot {
            test_mode: "transfer".into(),
            execution_mode: "burst".into(),
            tx_count: 100,
            sender_count: 10,
            wave_count: 4,
            wave_delay_ms: 0,
            worker_count: 8,
        };
        let json = serde_json::to_string(&snap).unwrap();
        let deser: ConfigSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.tx_count, 100);
        assert_eq!(deser.test_mode, "transfer");
    }

    fn make_latency_stats() -> LatencyStats {
        LatencyStats {
            p50: 50,
            p95: 95,
            p99: 99,
            min: 5,
            max: 200,
            avg: 60,
        }
    }

    #[test]
    fn test_burst_result_serde_roundtrip() {
        let br = BurstResult {
            submitted: 1000,
            confirmed: 950,
            pending: 50,
            sign_ms: 10,
            submit_ms: 200,
            confirm_ms: 500,
            submitted_tps: 100.0,
            confirmed_tps: 95.0,
            latency: make_latency_stats(),
            server_metrics: None,
            per_method: None,
            validator_health: None,
            per_wave: None,
        };
        let json = serde_json::to_string(&br).unwrap();
        let deser: BurstResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.submitted, 1000);
        assert_eq!(deser.confirmed_tps, 95.0);
    }

    #[test]
    fn test_benchmark_report_serde_roundtrip() {
        let report = BenchmarkReport {
            benchmark: "test_bench".into(),
            captured_at: "2026-01-01T00:00:00Z".into(),
            chain_id: 19803,
            config: ConfigSnapshot {
                test_mode: "transfer".into(),
                execution_mode: "burst".into(),
                tx_count: 500,
                sender_count: 10,
                wave_count: 4,
                wave_delay_ms: 0,
                worker_count: 8,
            },
            results: BurstResult {
                submitted: 500,
                confirmed: 490,
                pending: 10,
                sign_ms: 5,
                submit_ms: 100,
                confirm_ms: 300,
                submitted_tps: 50.0,
                confirmed_tps: 49.0,
                latency: make_latency_stats(),
                server_metrics: None,
                per_method: None,
                validator_health: None,
                per_wave: None,
            },
            ceiling_analysis: None,
            cost_efficiency: None,
            replay_pack: None,
        };
        let json = serde_json::to_string(&report).unwrap();
        let deser: BenchmarkReport = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.benchmark, "test_bench");
        assert_eq!(deser.chain_id, 19803);
    }

    #[test]
    fn test_per_method_stats_serde() {
        let pms = PerMethodStats {
            count: 100,
            confirmed: 98,
            reverted: 2,
            avg_gas: 50_000,
            latency_p50: 40,
            latency_p95: 120,
        };
        let json = serde_json::to_string(&pms).unwrap();
        let deser: PerMethodStats = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.count, 100);
        assert_eq!(deser.reverted, 2);
    }

    #[test]
    fn test_server_metrics_serde() {
        let sm = ServerMetrics {
            execution_ms: Some(HistogramDelta {
                start: 0.0,
                end: 100.0,
                count: 10,
                sum: 500.0,
            }),
            state_root_ms: None,
            parent_handoff_ms: None,
            publication_ms: None,
            queue_wait_ms: None,
        };
        let json = serde_json::to_string(&sm).unwrap();
        let deser: ServerMetrics = serde_json::from_str(&json).unwrap();
        assert!(deser.execution_ms.is_some());
        assert!(deser.state_root_ms.is_none());
    }

    #[test]
    fn test_histogram_delta_serde() {
        let hd = HistogramDelta {
            start: 1.0,
            end: 2.0,
            count: 5,
            sum: 7.5,
        };
        let json = serde_json::to_string(&hd).unwrap();
        let deser: HistogramDelta = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.count, 5);
        assert!((deser.sum - 7.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_validator_health_snapshot_serde() {
        let snap = ValidatorHealthSnapshot {
            url: "http://localhost:8545".into(),
            block_height: Some(42),
            is_synced: true,
            availability_percent: 99.5,
            latency_p50_ms: Some(10),
            latency_p95_ms: Some(30),
            latency_p99_ms: Some(50),
            tx_acceptance_rate: 0.98,
            error_rate: 0.02,
            is_connected: true,
        };
        let json = serde_json::to_string(&snap).unwrap();
        let deser: ValidatorHealthSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.block_height, Some(42));
        assert!(deser.is_synced);
    }

    #[test]
    fn test_window_entry_serde() {
        let we = WindowEntry {
            second: 5,
            sent: 100,
            confirmed: 95,
            latency_p50: 30,
        };
        let json = serde_json::to_string(&we).unwrap();
        let deser: WindowEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.second, 5);
        assert_eq!(deser.confirmed, 95);
    }

    #[test]
    fn test_sustained_result_serde() {
        let sr = SustainedResult {
            sent: 1000,
            confirmed: 990,
            pending: 10,
            errors: 1,
            duration_ms: 60_000,
            actual_tps: 16.5,
            latency: make_latency_stats(),
            timeline: vec![WindowEntry {
                second: 0,
                sent: 50,
                confirmed: 48,
                latency_p50: 25,
            }],
        };
        let json = serde_json::to_string(&sr).unwrap();
        let deser: SustainedResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.sent, 1000);
        assert_eq!(deser.timeline.len(), 1);
    }

    #[test]
    fn test_ceiling_step_serde() {
        let step = CeilingStep {
            target_tps: 200,
            actual_tps: 180,
            pending_ratio: 0.1,
            error_rate: 0.02,
            duration_ms: 10_000,
            is_saturated: false,
        };
        let json = serde_json::to_string(&step).unwrap();
        let deser: CeilingStep = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.target_tps, 200);
        assert!(!deser.is_saturated);
    }

    #[test]
    fn test_ceiling_result_serde() {
        let cr = CeilingResult {
            steps: vec![CeilingStep {
                target_tps: 100,
                actual_tps: 100,
                pending_ratio: 0.0,
                error_rate: 0.0,
                duration_ms: 5000,
                is_saturated: false,
            }],
            ceiling_tps: 500,
            burst_peak_tps: 600,
            confidence_score: 0.8,
            confidence_band_low: 450,
            confidence_band_high: 550,
            adaptive_step_enabled: true,
        };
        let json = serde_json::to_string(&cr).unwrap();
        let deser: CeilingResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.ceiling_tps, 500);
        assert_eq!(deser.steps.len(), 1);
    }

    fn make_harness_metrics() -> HarnessMetrics {
        HarnessMetrics {
            tps_submitted: 100.0,
            tps_confirmed: 95.0,
            latency_p50: 50,
            latency_p95: 95,
            latency_p99: 99,
            confirmation_rate: 0.95,
            pending_ratio: 0.05,
            error_rate: 0.01,
            memory_bytes: 1024 * 1024,
        }
    }

    fn make_unified_server_metrics() -> UnifiedServerMetrics {
        UnifiedServerMetrics {
            block_execution_ms: 100,
            state_root_ms: 50,
            parent_handoff_ms: 10,
            publication_ms: 20,
            queue_wait_ms: 5,
            gas_per_block: 15_000_000,
            transactions_per_block: 200,
            memory_usage_mb: 512,
        }
    }

    #[test]
    fn test_harness_metrics_creation_and_serde() {
        let hm = make_harness_metrics();
        let json = serde_json::to_string(&hm).unwrap();
        let deser: HarnessMetrics = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.tps_submitted, 100.0);
        assert_eq!(deser.memory_bytes, 1024 * 1024);
    }

    #[test]
    fn test_unified_server_metrics_creation_and_serde() {
        let usm = make_unified_server_metrics();
        let json = serde_json::to_string(&usm).unwrap();
        let deser: UnifiedServerMetrics = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.block_execution_ms, 100);
        assert_eq!(deser.transactions_per_block, 200);
    }

    #[test]
    fn test_performance_snapshot_creation_and_serde() {
        let ps = PerformanceSnapshot {
            timestamp: Utc::now(),
            harness_metrics: make_harness_metrics(),
            server_metrics: make_unified_server_metrics(),
            correlation_confidence: 0.85,
        };
        let json = serde_json::to_string(&ps).unwrap();
        let deser: PerformanceSnapshot = serde_json::from_str(&json).unwrap();
        assert!((deser.correlation_confidence - 0.85).abs() < f32::EPSILON);
    }

    #[test]
    fn test_bottleneck_finding_serde() {
        let mut stack = HashMap::new();
        stack.insert("execution".to_string(), 0.45);
        stack.insert("harness".to_string(), 0.0);
        let bf = BottleneckFinding {
            bottleneck_type: "StateRootComputation".into(),
            severity: 0.8,
            pct_of_total: 0.45,
            details: "State root is dominant".into(),
            stack_attribution: stack,
        };
        let json = serde_json::to_string(&bf).unwrap();
        let deser: BottleneckFinding = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.bottleneck_type, "StateRootComputation");
        assert_eq!(deser.stack_attribution.len(), 2);
    }

    #[test]
    fn test_regression_analysis_serde() {
        let ra = RegressionAnalysis {
            tps_delta: -50.0,
            tps_pct_change: -5.0,
            latency_delta_ms: 10,
            p_value: 0.03,
            verdict: "regressed".into(),
        };
        let json = serde_json::to_string(&ra).unwrap();
        let deser: RegressionAnalysis = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.verdict, "regressed");
        assert_eq!(deser.latency_delta_ms, 10);
    }

    #[test]
    fn test_recommendation_serde() {
        let rec = Recommendation {
            priority: "High".into(),
            title: "Optimize state root".into(),
            description: "Consider parallel hashing".into(),
            estimated_tps_improvement_pct: 15.0,
            effort_level: "Medium".into(),
            roi_score: 2.5,
            implementation_hints: vec!["Use rayon".into(), "Profile first".into()],
        };
        let json = serde_json::to_string(&rec).unwrap();
        let deser: Recommendation = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.priority, "High");
        assert_eq!(deser.implementation_hints.len(), 2);
    }

    #[test]
    fn test_report_package_serde() {
        let rp = ReportPackage {
            json: r#"{"key":"value"}"#.into(),
            html: "<html></html>".into(),
            ascii: "== report ==".into(),
            markdown: "# Report".into(),
        };
        let json = serde_json::to_string(&rp).unwrap();
        let deser: ReportPackage = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.markdown, "# Report");
    }

    #[test]
    fn test_analytics_report_creation_and_serde() {
        let report = AnalyticsReport {
            benchmark_name: "bench_v1".into(),
            execution_mode: "burst".into(),
            timestamp: Utc::now(),
            snapshot: PerformanceSnapshot {
                timestamp: Utc::now(),
                harness_metrics: make_harness_metrics(),
                server_metrics: make_unified_server_metrics(),
                correlation_confidence: 0.9,
            },
            bottlenecks: vec![],
            regression_analysis: None,
            recommendations: vec![],
            reports: ReportPackage {
                json: "{}".into(),
                html: "".into(),
                ascii: "".into(),
                markdown: "".into(),
            },
        };
        let json = serde_json::to_string(&report).unwrap();
        let deser: AnalyticsReport = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.benchmark_name, "bench_v1");
        assert!(deser.regression_analysis.is_none());
    }

    #[test]
    fn test_transaction_type_all_variants_exist() {
        // Ensure all variants are constructible and distinct.
        let variants = [
            TransactionType::SimpleTransfer,
            TransactionType::ERC20Mint,
            TransactionType::ERC20Transfer,
            TransactionType::ERC20Approve,
            TransactionType::Swap,
            TransactionType::NFTMint,
            TransactionType::ETHTransfer,
        ];
        // All variants should be distinct (7 unique values).
        let mut set = std::collections::HashSet::new();
        for v in &variants {
            set.insert(std::mem::discriminant(v));
        }
        assert_eq!(set.len(), 7);
    }
}
