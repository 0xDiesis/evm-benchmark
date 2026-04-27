use crate::types::{ExecutionMode, TestMode};
use clap::Parser;
use std::path::PathBuf;
use std::str::FromStr;
use url::Url;

/// Transaction submission method
///
/// Controls whether transactions are submitted via HTTP or WebSocket RPC.
/// HTTP provides reliable connection pooling while WebSocket may offer
/// lower latency for sustained submission rates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionMethod {
    /// Submit via HTTP RPC
    Http,
    /// Submit via WebSocket RPC
    WebSocket,
}

impl FromStr for SubmissionMethod {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "http" => Ok(SubmissionMethod::Http),
            "websocket" | "ws" => Ok(SubmissionMethod::WebSocket),
            other => Err(format!("Unknown submission method: {}", other)),
        }
    }
}

#[derive(Parser, Debug, Clone)]
#[command(name = "evm-benchmark")]
#[command(about = "EVM load testing harness — chain-agnostic, works with any EVM-compatible chain")]
pub struct Args {
    /// HTTP RPC endpoint(s) - comma-separated for multiple endpoints with round-robin
    /// Example: http://node1:8545,http://node2:8545,http://node3:8545
    #[arg(long, default_value = "http://localhost:8545")]
    pub rpc_endpoints: String,

    /// WebSocket RPC endpoint for block tracking
    #[arg(long, default_value = "ws://localhost:8546")]
    pub ws: Url,

    /// Prometheus metrics endpoint
    #[arg(long)]
    pub metrics: Option<Url>,

    /// Validator RPC endpoints for health monitoring - comma-separated list
    /// Example: http://validator1:8545,http://validator2:8545
    #[arg(long)]
    pub validators: Option<String>,

    /// Test mode: transfer or evm
    #[arg(long, default_value = "transfer")]
    pub test: String,

    /// Execution mode: burst, sustained, or ceiling
    #[arg(long, default_value = "burst")]
    pub execution: String,

    // Burst parameters
    /// Number of transactions
    #[arg(long, default_value = "10000")]
    pub txs: u32,

    /// Number of sender accounts. When BENCH_KEY is not set, generates this
    /// many deterministic keys from keccak256("bench-sender-{i}"). More senders
    /// avoids per-address tx limits on chains like Sonic (MaxTxsPerAddress=32).
    #[arg(long, default_value = "200")]
    pub senders: u32,

    /// Auto-fund sender accounts before benchmarking via a MultiSend contract.
    /// Deploys a tiny contract, then batch-funds all senders from the first key.
    /// Requires the first sender key (or BENCH_KEY) to be pre-funded on the chain.
    /// Funding is NOT counted toward TPS — the benchmark starts after all funds confirm.
    #[arg(long)]
    pub fund: bool,

    /// Number of waves in burst mode
    #[arg(long, default_value = "8")]
    pub waves: u32,

    /// Delay between waves in milliseconds (0 = no delay, maximum throughput)
    #[arg(long, default_value = "0")]
    pub wave_delay_ms: u64,

    // Sustained parameters
    /// Duration in seconds (sustained mode)
    #[arg(long, default_value = "60")]
    pub duration: u64,

    /// Target TPS (sustained/ceiling mode)
    #[arg(long, default_value = "100")]
    pub tps: u32,

    // Worker configuration
    /// Number of worker tasks
    #[arg(long, default_value = "8")]
    pub workers: u32,

    /// Batch size for RPC submission
    #[arg(long, default_value = "100")]
    pub batch_size: u32,

    /// Submission method: http or websocket
    #[arg(long, default_value = "http")]
    pub submission_method: String,

    /// Retry profile for transient submission failures: off, light, moderate, aggressive.
    #[arg(long, default_value = "light")]
    pub retry_profile: String,

    /// Number of additional block confirmations required before counting a tx as confirmed.
    /// Set >0 to enable finality-stress confirmation tracking.
    #[arg(long, default_value = "0")]
    pub finality_confirmations: u32,

    // Output
    /// JSON report output path
    #[arg(long, default_value = "report.json")]
    pub out: PathBuf,

    /// Suppress console output
    #[arg(long)]
    pub quiet: bool,

    /// Chain ID for transaction signing. Defaults to 19803 (Diesis testnet).
    /// Override to benchmark other EVM chains (e.g. 1 for Ethereum mainnet, 250 for Sonic).
    #[arg(long, default_value = "19803")]
    pub chain_id: u64,

    /// Benchmark name written to the report JSON. Useful for labelling runs
    /// from different chains when comparing results.
    #[arg(long, default_value = "evm_bench_v1")]
    pub bench_name: String,

    /// Download the latest bench-targets (chain configs, Docker compose files,
    /// scripts) from GitHub and exit. Use this when running the binary standalone
    /// without cloning the repository.
    #[arg(long)]
    pub setup: bool,

    /// Re-download bench-targets from GitHub, replacing any existing local copy.
    #[arg(long)]
    pub update_targets: bool,

    /// Git branch to download bench-targets from (used with --setup or --update-targets).
    #[arg(long, default_value = "main")]
    pub targets_branch: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// RPC endpoint URLs for submission (may be multiple for round-robin)
    pub rpc_urls: Vec<Url>,
    /// Primary RPC URL (first in the list, for backward compatibility)
    pub rpc: Url,
    #[allow(dead_code)]
    pub ws: Url,
    #[allow(dead_code)]
    pub metrics: Option<Url>,
    /// Validator URLs for health monitoring
    #[allow(dead_code)]
    pub validator_urls: Vec<String>,
    #[allow(dead_code)]
    pub test_mode: TestMode,
    #[allow(dead_code)]
    pub execution_mode: ExecutionMode,
    pub tx_count: u32,
    pub sender_count: u32,
    pub wave_count: u32,
    pub wave_delay_ms: u64,
    #[allow(dead_code)]
    pub duration_secs: u64,
    #[allow(dead_code)]
    pub target_tps: u32,
    pub worker_count: u32,
    pub batch_size: u32,
    pub submission_method: SubmissionMethod,
    pub retry_profile: String,
    pub finality_confirmations: u32,
    pub output: PathBuf,
    pub quiet: bool,
    /// Chain ID used for transaction signing.
    pub chain_id: u64,
    /// Benchmark name written to the report JSON.
    pub bench_name: String,
    /// Auto-fund sender accounts before benchmarking.
    pub fund: bool,
    /// Resolved sender private keys (populated at runtime by main, after key resolution).
    pub sender_keys: Vec<String>,
    /// Deployed ERC-20 token contract addresses (populated at runtime if EVM mode + --fund).
    pub evm_tokens: Vec<alloy_primitives::Address>,
    /// Deployed AMM pair contract addresses (populated at runtime if EVM mode + --fund).
    pub evm_pairs: Vec<alloy_primitives::Address>,
    /// Deployed NFT contract addresses (populated at runtime if EVM mode + --fund).
    pub evm_nfts: Vec<alloy_primitives::Address>,
}

impl Args {
    pub fn into_config(self) -> anyhow::Result<Config> {
        let test_mode = match self.test.to_lowercase().as_str() {
            "transfer" => TestMode::Transfer,
            "evm" => TestMode::Evm,
            other => anyhow::bail!("Unknown test mode: {}", other),
        };

        let execution_mode = match self.execution.to_lowercase().as_str() {
            "burst" => ExecutionMode::Burst,
            "sustained" => ExecutionMode::Sustained,
            "ceiling" => ExecutionMode::Ceiling,
            other => anyhow::bail!("Unknown execution mode: {}", other),
        };

        // Parse comma-separated RPC endpoints
        let rpc_urls: Result<Vec<Url>, _> = self
            .rpc_endpoints
            .split(',')
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(|trimmed| {
                Url::parse(trimmed)
                    .map_err(|e| anyhow::anyhow!("Invalid RPC URL '{}': {}", trimmed, e))
            })
            .collect();
        let rpc_urls = rpc_urls?;

        if rpc_urls.is_empty() {
            anyhow::bail!("At least one RPC endpoint URL is required");
        }

        let primary_rpc = rpc_urls[0].clone();

        // Parse validator URLs if provided
        let validator_urls = self
            .validators
            .map(|validators_str| {
                validators_str
                    .split(',')
                    .map(|url| url.trim().to_string())
                    .filter(|url| !url.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let submission_method =
            SubmissionMethod::from_str(&self.submission_method).map_err(|e| anyhow::anyhow!(e))?;

        Ok(Config {
            rpc_urls,
            rpc: primary_rpc,
            ws: self.ws,
            metrics: self.metrics,
            validator_urls,
            test_mode,
            execution_mode,
            tx_count: self.txs,
            sender_count: self.senders,
            wave_count: self.waves,
            wave_delay_ms: self.wave_delay_ms,
            duration_secs: self.duration,
            target_tps: self.tps,
            worker_count: self.workers,
            batch_size: self.batch_size,
            submission_method,
            retry_profile: self.retry_profile,
            finality_confirmations: self.finality_confirmations,
            output: self.out,
            quiet: self.quiet,
            chain_id: self.chain_id,
            bench_name: self.bench_name,
            fund: self.fund,
            sender_keys: vec![],
            evm_tokens: vec![],
            evm_pairs: vec![],
            evm_nfts: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_submission_method_http_parsing() {
        let method = SubmissionMethod::from_str("http").expect("failed to parse http");
        assert_eq!(method, SubmissionMethod::Http);
    }

    #[test]
    fn test_submission_method_websocket_parsing() {
        let method = SubmissionMethod::from_str("websocket").expect("failed to parse websocket");
        assert_eq!(method, SubmissionMethod::WebSocket);
    }

    #[test]
    fn test_submission_method_ws_parsing() {
        let method = SubmissionMethod::from_str("ws").expect("failed to parse ws");
        assert_eq!(method, SubmissionMethod::WebSocket);
    }

    #[test]
    fn test_submission_method_case_insensitive() {
        let method = SubmissionMethod::from_str("HTTP").expect("failed to parse HTTP");
        assert_eq!(method, SubmissionMethod::Http);

        let method = SubmissionMethod::from_str("WebSocket").expect("failed to parse WebSocket");
        assert_eq!(method, SubmissionMethod::WebSocket);
    }

    #[test]
    fn test_submission_method_invalid() {
        let result = SubmissionMethod::from_str("invalid");
        assert!(result.is_err());
    }

    /// Helper to build an `Args` with sensible defaults for testing.
    fn make_args(rpc_endpoints: &str, submission_method: &str) -> Args {
        Args {
            rpc_endpoints: rpc_endpoints.to_string(),
            ws: url::Url::parse("ws://localhost:8546").unwrap(),
            metrics: None,
            validators: None,
            test: "transfer".to_string(),
            execution: "burst".to_string(),
            txs: 100,
            senders: 10,
            waves: 4,
            wave_delay_ms: 0,
            duration: 60,
            tps: 100,
            workers: 8,
            batch_size: 50,
            submission_method: submission_method.to_string(),
            retry_profile: "light".to_string(),
            finality_confirmations: 0,
            out: std::path::PathBuf::from("test-report.json"),
            quiet: true,
            chain_id: 19803,
            bench_name: "evm_bench_v1".to_string(),
            fund: false,
            setup: false,
            update_targets: false,
            targets_branch: "main".to_string(),
        }
    }

    #[test]
    fn test_args_into_config_http_method() {
        let args = make_args("http://localhost:8545", "http");
        let config = args.into_config().expect("into_config failed");
        assert_eq!(config.submission_method, SubmissionMethod::Http);
    }

    #[test]
    fn test_args_into_config_ws_method() {
        let args = make_args("http://localhost:8545", "ws");
        let config = args.into_config().expect("into_config failed");
        assert_eq!(config.submission_method, SubmissionMethod::WebSocket);
    }

    #[test]
    fn test_args_into_config_multi_endpoint() {
        let args = make_args("http://a:8545,http://b:8545", "http");
        let config = args.into_config().expect("into_config failed");
        assert_eq!(config.rpc_urls.len(), 2);
        assert_eq!(config.rpc_urls[0].host_str(), Some("a"));
        assert_eq!(config.rpc_urls[1].host_str(), Some("b"));
        // Primary RPC should be the first endpoint
        assert_eq!(config.rpc.host_str(), Some("a"));
    }

    #[test]
    fn test_args_into_config_ignores_blank_endpoints() {
        let args = make_args(" http://a:8545 , , http://b:8545 , ", "http");
        let config = args.into_config().expect("into_config failed");
        assert_eq!(config.rpc_urls.len(), 2);
        assert_eq!(config.rpc_urls[0].host_str(), Some("a"));
        assert_eq!(config.rpc_urls[1].host_str(), Some("b"));
    }

    #[test]
    fn test_args_into_config_requires_at_least_one_endpoint() {
        let args = make_args(" ,  , ", "http");
        let result = args.into_config();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("At least one RPC endpoint URL is required")
        );
    }

    #[test]
    fn test_invalid_test_mode_errors() {
        let mut args = make_args("http://localhost:8545", "http");
        args.test = "unknown".into();
        let result = args.into_config();
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Unknown test mode"), "got: {msg}");
    }

    #[test]
    fn test_invalid_execution_mode_errors() {
        let mut args = make_args("http://localhost:8545", "http");
        args.execution = "unknown".into();
        let result = args.into_config();
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Unknown execution mode"), "got: {msg}");
    }

    #[test]
    fn test_into_config_evm_test_mode() {
        let mut args = make_args("http://localhost:8545", "http");
        args.test = "evm".into();
        let config = args.into_config().unwrap();
        assert_eq!(config.test_mode, TestMode::Evm);
    }

    #[test]
    fn test_into_config_sustained_execution_mode() {
        let mut args = make_args("http://localhost:8545", "http");
        args.execution = "sustained".into();
        let config = args.into_config().unwrap();
        assert_eq!(config.execution_mode, ExecutionMode::Sustained);
    }

    #[test]
    fn test_into_config_ceiling_execution_mode() {
        let mut args = make_args("http://localhost:8545", "http");
        args.execution = "ceiling".into();
        let config = args.into_config().unwrap();
        assert_eq!(config.execution_mode, ExecutionMode::Ceiling);
    }

    #[test]
    fn test_into_config_with_validators() {
        let mut args = make_args("http://localhost:8545", "http");
        args.validators = Some("http://v1:8545,http://v2:8545".into());
        let config = args.into_config().unwrap();
        assert_eq!(config.validator_urls.len(), 2);
        assert_eq!(config.validator_urls[0], "http://v1:8545");
        assert_eq!(config.validator_urls[1], "http://v2:8545");
    }

    #[test]
    fn test_into_config_with_empty_validators() {
        let mut args = make_args("http://localhost:8545", "http");
        args.validators = Some("".into());
        let config = args.into_config().unwrap();
        assert!(config.validator_urls.is_empty());
    }

    #[test]
    fn test_into_config_preserves_all_fields() {
        let mut args = make_args("http://localhost:8545", "http");
        args.chain_id = 1;
        args.bench_name = "my_bench".into();
        args.fund = true;
        args.duration = 120;
        args.tps = 500;
        args.txs = 2000;
        args.senders = 50;
        args.waves = 16;
        args.wave_delay_ms = 50;
        args.workers = 16;
        args.batch_size = 200;
        args.quiet = false;

        let config = args.into_config().unwrap();
        assert_eq!(config.chain_id, 1);
        assert_eq!(config.bench_name, "my_bench");
        assert!(config.fund);
        assert_eq!(config.duration_secs, 120);
        assert_eq!(config.target_tps, 500);
        assert_eq!(config.tx_count, 2000);
        assert_eq!(config.sender_count, 50);
        assert_eq!(config.wave_count, 16);
        assert_eq!(config.wave_delay_ms, 50);
        assert_eq!(config.worker_count, 16);
        assert_eq!(config.batch_size, 200);
        assert!(!config.quiet);
    }

    #[test]
    fn test_multi_endpoint_with_spaces() {
        let args = make_args("http://a:8545 , http://b:8545 , http://c:8545", "http");
        let config = args.into_config().unwrap();
        assert_eq!(config.rpc_urls.len(), 3);
        assert_eq!(config.rpc_urls[0].host_str(), Some("a"));
        assert_eq!(config.rpc_urls[1].host_str(), Some("b"));
        assert_eq!(config.rpc_urls[2].host_str(), Some("c"));
    }

    #[test]
    fn test_invalid_rpc_url_errors() {
        let args = make_args("not-a-url", "http");
        let result = args.into_config();
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Invalid RPC URL"), "got: {msg}");
    }
}
