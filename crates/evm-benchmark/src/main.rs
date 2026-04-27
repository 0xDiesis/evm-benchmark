mod analytics;
#[allow(dead_code)]
mod cache;
mod config;
mod errors;
mod funding;
#[allow(dead_code)]
mod generators;
mod metrics;
mod modes;
mod reporting;
mod setup;
mod signing;
mod submission;
mod types;
mod validators;

use alloy_primitives::Address;
use clap::Parser;
use std::future::Future;
use std::io::BufRead;
use std::path::Path;
use std::pin::Pin;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = anyhow::Result<T>> + Send + 'a>>;

type DownloadTargetsFn =
    dyn for<'a> Fn(&'a Path, Option<&'a str>) -> BoxFuture<'a, ()> + Send + Sync;
type FundSendersFn =
    dyn for<'a> Fn(&'a str, &'a str, &'a [Address], u64, bool) -> BoxFuture<'a, ()> + Send + Sync;
type DeployContractsFn = dyn for<'a> Fn(
        &'a str,
        &'a str,
        u64,
        u32,
        u32,
        u32,
        bool,
    ) -> BoxFuture<'a, generators::contract_deploy::EvmContracts>
    + Send
    + Sync;
type RunBurstFn =
    dyn for<'a> Fn(&'a config::Config) -> BoxFuture<'a, (types::BurstResult, u128)> + Send + Sync;
type RunSustainedFn = dyn for<'a> Fn(&'a config::Config) -> BoxFuture<'a, (types::SustainedResult, u128)>
    + Send
    + Sync;
type RunCeilingFn =
    dyn for<'a> Fn(&'a config::Config) -> BoxFuture<'a, types::CeilingResult> + Send + Sync;
type WriteReportFn = dyn for<'a> Fn(
        &'a config::Config,
        &'a types::BurstResult,
        &'a Path,
        Option<&'a types::CeilingResult>,
        Option<u128>,
    ) -> BoxFuture<'a, ()>
    + Send
    + Sync;

struct RuntimeDeps {
    download_targets: Box<DownloadTargetsFn>,
    fund_senders: Box<FundSendersFn>,
    deploy_contracts: Box<DeployContractsFn>,
    run_burst: Box<RunBurstFn>,
    run_sustained: Box<RunSustainedFn>,
    run_ceiling: Box<RunCeilingFn>,
    write_report: Box<WriteReportFn>,
}

impl RuntimeDeps {
    fn real() -> Self {
        Self {
            download_targets: Box::new(|dest, branch| {
                Box::pin(async move { runtime_download_targets(dest, branch).await })
            }),
            fund_senders: Box::new(|rpc_url, funder_key, addresses, chain_id, quiet| {
                Box::pin(async move {
                    funding::fund_senders(rpc_url, funder_key, addresses, chain_id, quiet).await
                })
            }),
            deploy_contracts: Box::new(
                |rpc_url, deployer_key, chain_id, token_count, pair_count, nft_count, quiet| {
                    Box::pin(async move {
                        generators::contract_deploy::deploy_contracts(
                            rpc_url,
                            deployer_key,
                            chain_id,
                            token_count,
                            pair_count,
                            nft_count,
                            quiet,
                        )
                        .await
                    })
                },
            ),
            run_burst: Box::new(|config| Box::pin(async move { modes::run_burst(config).await })),
            run_sustained: Box::new(|config| {
                Box::pin(async move { modes::run_sustained(config).await })
            }),
            run_ceiling: Box::new(|config| {
                Box::pin(async move { modes::run_ceiling(config).await })
            }),
            write_report: Box::new(|config, result, output, ceiling_meta, gas_price| {
                Box::pin(async move {
                    reporting::write_report(config, result, output, ceiling_meta, gas_price).await
                })
            }),
        }
    }
}

async fn runtime_download_targets(dest: &Path, branch: Option<&str>) -> anyhow::Result<()> {
    #[cfg(test)]
    if std::env::var_os("EVM_BENCH_TEST_SKIP_DOWNLOAD").is_some() {
        std::fs::create_dir_all(dest.join("scripts"))?;
        std::fs::create_dir_all(dest.join("chains"))?;
        return Ok(());
    }

    setup::download_targets(dest, branch).await
}

fn resolved_sender_message(sender_count: usize) -> String {
    format!(
        "Resolved {} sender key{}.",
        sender_count,
        if sender_count == 1 { "" } else { "s" }
    )
}

fn should_download_missing_targets(answer: &str) -> bool {
    let normalized = answer.trim().to_ascii_lowercase();
    normalized.is_empty() || normalized == "y" || normalized == "yes"
}

fn ceiling_summary_lines(ceiling: &types::CeilingResult) -> Vec<String> {
    let mut lines = vec![
        String::new(),
        "╔════════════════════════════════════════════════════╗".to_string(),
        "║          CEILING MODE - FINAL RESULTS            ║".to_string(),
        "╚════════════════════════════════════════════════════╝".to_string(),
        format!("Steps taken: {}", ceiling.steps.len()),
        format!("Ceiling TPS: {}", ceiling.ceiling_tps),
        format!("Peak TPS:    {}", ceiling.burst_peak_tps),
        format!(
            "Confidence:  {:.0}% (band {}-{} TPS)",
            ceiling.confidence_score * 100.0,
            ceiling.confidence_band_low,
            ceiling.confidence_band_high
        ),
        format!(
            "Adaptive search: {}",
            if ceiling.adaptive_step_enabled {
                "enabled"
            } else {
                "disabled"
            }
        ),
        String::new(),
        "Ramp-up Details:".to_string(),
    ];

    for (index, step) in ceiling.steps.iter().enumerate() {
        lines.push(format!(
            "  Step {}: {} TPS target -> {:.1} TPS actual | Pending: {:.1}% | Errors: {:.1}%{}",
            index + 1,
            step.target_tps,
            step.actual_tps,
            step.pending_ratio * 100.0,
            step.error_rate * 100.0,
            if step.is_saturated {
                " [SATURATED]"
            } else {
                ""
            }
        ));
    }

    lines
}

fn burst_result_from_ceiling(ceiling: &types::CeilingResult) -> types::BurstResult {
    types::BurstResult {
        submitted: ceiling.ceiling_tps,
        confirmed: ceiling.burst_peak_tps,
        pending: 0,
        sign_ms: 0,
        submit_ms: 0,
        confirm_ms: 0,
        submitted_tps: ceiling.burst_peak_tps as f32,
        confirmed_tps: ceiling.burst_peak_tps as f32,
        latency: types::LatencyStats {
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
    }
}

async fn rpc_request(
    client: &reqwest::Client,
    rpc_url: &url::Url,
    method: &str,
    params: serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
        "id": 1
    });

    let resp: serde_json::Value = client
        .post(rpc_url.clone())
        .json(&payload)
        .send()
        .await?
        .json()
        .await?;
    if let Some(err) = resp.get("error") {
        anyhow::bail!("RPC {} failed: {}", method, err);
    }
    Ok(resp)
}

async fn run_preflight(config: &config::Config, sender_keys: &[String]) -> anyhow::Result<()> {
    let strict = std::env::var("BENCH_PREFLIGHT_STRICT")
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(true);

    let client = reqwest::Client::new();

    let _ = rpc_request(
        &client,
        &config.rpc,
        "eth_blockNumber",
        serde_json::json!([]),
    )
    .await?;

    let chain_resp =
        rpc_request(&client, &config.rpc, "eth_chainId", serde_json::json!([])).await?;
    if let Some(chain_hex) = chain_resp.get("result").and_then(|value| value.as_str()) {
        let remote_chain = u64::from_str_radix(chain_hex.trim_start_matches("0x"), 16).unwrap_or(0);
        if remote_chain != 0 && remote_chain != config.chain_id {
            let msg = format!(
                "Preflight: configured chain_id={} but RPC reports chain_id={}",
                config.chain_id, remote_chain
            );
            if strict {
                anyhow::bail!(msg);
            } else {
                eprintln!("WARNING: {}", msg);
            }
        }
    }

    if !config.fund {
        let parsed = funding::parse_sender_keys(sender_keys)?;
        if let Some((_, _, first_addr)) = parsed.first() {
            let balance = rpc_request(
                &client,
                &config.rpc,
                "eth_getBalance",
                serde_json::json!([format!("{:?}", first_addr), "latest"]),
            )
            .await?;
            let balance_hex = balance
                .get("result")
                .and_then(|value| value.as_str())
                .unwrap_or("0x0");
            let balance_wei =
                u128::from_str_radix(balance_hex.trim_start_matches("0x"), 16).unwrap_or(0);
            let min_balance_wei = std::env::var("BENCH_PREFLIGHT_MIN_BALANCE_WEI")
                .ok()
                .and_then(|value| value.parse::<u128>().ok())
                .unwrap_or(1_000_000_000_000_000_000);
            if balance_wei < min_balance_wei {
                let msg = format!(
                    "Preflight: first sender {:?} balance {} wei is below minimum {} wei (set --fund or BENCH_KEY)",
                    first_addr, balance_wei, min_balance_wei
                );
                if strict {
                    anyhow::bail!(msg);
                } else {
                    eprintln!("WARNING: {}", msg);
                }
            }
        }
    }

    Ok(())
}

fn prepare_run<R: BufRead>(
    args: config::Args,
    stdin: &mut R,
    deps: &RuntimeDeps,
) -> anyhow::Result<Option<config::Config>> {
    if args.setup || args.update_targets {
        let dest = setup::default_targets_dir();
        let branch = args.targets_branch.clone();
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on((deps.download_targets)(&dest, Some(&branch)))?;
        if args.setup {
            return Ok(None);
        }
    }

    let targets_dir = setup::default_targets_dir();
    if !setup::targets_exist(&targets_dir) {
        eprintln!("Bench-targets not found at {}", targets_dir.display());
        eprint!("Download the latest bench-targets from GitHub? [Y/n] ");
        let mut input = String::new();
        stdin.read_line(&mut input)?;
        if should_download_missing_targets(&input) {
            let branch = args.targets_branch.clone();
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on((deps.download_targets)(&targets_dir, Some(&branch)))?;
        }
    }

    Ok(Some(args.into_config()?))
}

fn run_with_args_and_input<R: BufRead>(
    args: config::Args,
    stdin: &mut R,
    deps: &RuntimeDeps,
) -> anyhow::Result<()> {
    let Some(config) = prepare_run(args, stdin, deps)? else {
        return Ok(());
    };

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async_main_with(config, deps))
}

fn main() -> anyhow::Result<()> {
    let args = config::Args::parse();
    let stdin = std::io::stdin();
    let mut locked = stdin.lock();
    let deps = RuntimeDeps::real();
    run_with_args_and_input(args, &mut locked, &deps)
}

#[allow(dead_code)]
async fn async_main(config: config::Config) -> anyhow::Result<()> {
    let deps = RuntimeDeps::real();
    async_main_with(config, &deps).await
}

async fn async_main_with(mut config: config::Config, deps: &RuntimeDeps) -> anyhow::Result<()> {
    let sender_keys = funding::resolve_sender_keys(config.sender_count);
    if !config.quiet {
        println!("{}", resolved_sender_message(sender_keys.len()));
    }

    run_preflight(&config, &sender_keys).await?;

    if config.fund {
        let parsed = funding::parse_sender_keys(&sender_keys)?;
        let addresses: Vec<_> = parsed.iter().map(|(_, _, addr)| *addr).collect();
        let funder_key = &sender_keys[0];
        (deps.fund_senders)(
            config.rpc.as_str(),
            funder_key,
            &addresses,
            config.chain_id,
            config.quiet,
        )
        .await?;
        if !config.quiet {
            println!("Funding complete. Starting benchmark...\n");
        }
    }

    if config.test_mode == types::TestMode::Evm && config.fund {
        let deployer_key = &sender_keys[0];
        let contracts = (deps.deploy_contracts)(
            config.rpc.as_str(),
            deployer_key,
            config.chain_id,
            5,
            3,
            2,
            config.quiet,
        )
        .await?;

        config.evm_tokens = contracts.tokens;
        config.evm_pairs = contracts.pairs;
        config.evm_nfts = contracts.nfts;

        if !config.quiet {
            println!("Contracts deployed. Starting benchmark...\n");
        }
    }

    config.sender_keys = sender_keys;

    if !config.quiet {
        println!("Starting benchmark...");
    }

    let (result, ceiling_meta, effective_gas_price) = match config.execution_mode {
        types::ExecutionMode::Burst => {
            let (burst, gas_price) = (deps.run_burst)(&config).await?;
            (burst, None, Some(gas_price))
        }
        types::ExecutionMode::Sustained => {
            let (sustained, gas_price) = (deps.run_sustained)(&config).await?;
            (sustained.to_burst_result(), None, Some(gas_price))
        }
        types::ExecutionMode::Ceiling => {
            let ceiling = (deps.run_ceiling)(&config).await?;
            if !config.quiet {
                for line in ceiling_summary_lines(&ceiling) {
                    println!("{}", line);
                }
            }
            (burst_result_from_ceiling(&ceiling), Some(ceiling), None)
        }
    };

    (deps.write_report)(
        &config,
        &result,
        &config.output,
        ceiling_meta.as_ref(),
        effective_gas_price,
    )
    .await?;

    if !config.quiet {
        println!("Report written to {}", config.output.display());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn test_lock() -> &'static Mutex<()> {
        TEST_LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    struct CurrentDirGuard {
        previous: PathBuf,
    }

    impl CurrentDirGuard {
        fn set(path: &Path) -> Self {
            let previous = std::env::current_dir().expect("failed to read current dir");
            std::env::set_current_dir(path).expect("failed to switch current dir");
            Self { previous }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous);
        }
    }

    fn temp_dir(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{}-{}", prefix, unique));
        std::fs::create_dir_all(&path).expect("failed to create temp dir");
        path
    }

    fn sample_args(rpc_endpoints: &str) -> config::Args {
        config::Args {
            rpc_endpoints: rpc_endpoints.to_string(),
            ws: url::Url::parse("ws://localhost:8546").expect("invalid ws url"),
            metrics: None,
            validators: None,
            test: "transfer".to_string(),
            execution: "burst".to_string(),
            txs: 1,
            senders: 1,
            fund: false,
            waves: 1,
            wave_delay_ms: 0,
            duration: 1,
            tps: 1,
            workers: 1,
            batch_size: 1,
            submission_method: "http".to_string(),
            retry_profile: "light".to_string(),
            finality_confirmations: 0,
            out: PathBuf::from("test-report.json"),
            quiet: true,
            chain_id: 19803,
            bench_name: "evm_bench_v1".to_string(),
            setup: false,
            update_targets: false,
            targets_branch: "main".to_string(),
        }
    }

    fn sample_config(rpc_endpoints: &str) -> config::Config {
        sample_args(rpc_endpoints)
            .into_config()
            .expect("failed to build config")
    }

    fn sample_latency() -> types::LatencyStats {
        types::LatencyStats {
            p50: 1,
            p95: 2,
            p99: 3,
            min: 1,
            max: 4,
            avg: 2,
        }
    }

    fn sample_burst_result() -> types::BurstResult {
        types::BurstResult {
            submitted: 10,
            confirmed: 9,
            pending: 1,
            sign_ms: 1,
            submit_ms: 2,
            confirm_ms: 3,
            submitted_tps: 4.0,
            confirmed_tps: 3.0,
            latency: sample_latency(),
            server_metrics: None,
            per_method: None,
            validator_health: None,
            per_wave: None,
        }
    }

    fn sample_sustained_result() -> types::SustainedResult {
        types::SustainedResult {
            sent: 20,
            confirmed: 18,
            pending: 2,
            errors: 0,
            duration_ms: 1_000,
            actual_tps: 18.0,
            latency: sample_latency(),
            timeline: vec![],
        }
    }

    fn sample_ceiling_result() -> types::CeilingResult {
        types::CeilingResult {
            steps: vec![
                types::CeilingStep {
                    target_tps: 100,
                    actual_tps: 95,
                    pending_ratio: 0.1,
                    error_rate: 0.0,
                    duration_ms: 1_000,
                    is_saturated: false,
                },
                types::CeilingStep {
                    target_tps: 200,
                    actual_tps: 150,
                    pending_ratio: 0.35,
                    error_rate: 0.05,
                    duration_ms: 1_000,
                    is_saturated: true,
                },
            ],
            ceiling_tps: 200,
            burst_peak_tps: 150,
            confidence_score: 0.85,
            confidence_band_low: 140,
            confidence_band_high: 160,
            adaptive_step_enabled: true,
        }
    }

    fn default_test_deps() -> RuntimeDeps {
        RuntimeDeps {
            download_targets: Box::new(|_, _| Box::pin(async { Ok(()) })),
            fund_senders: Box::new(|_, _, _, _, _| Box::pin(async { Ok(()) })),
            deploy_contracts: Box::new(|_, _, _, _, _, _, _| {
                Box::pin(async {
                    Ok(generators::contract_deploy::EvmContracts {
                        tokens: vec![Address::with_last_byte(1)],
                        pairs: vec![Address::with_last_byte(2)],
                        nfts: vec![Address::with_last_byte(3)],
                    })
                })
            }),
            run_burst: Box::new(|_| Box::pin(async { Ok((sample_burst_result(), 123)) })),
            run_sustained: Box::new(|_| Box::pin(async { Ok((sample_sustained_result(), 456)) })),
            run_ceiling: Box::new(|_| Box::pin(async { Ok(sample_ceiling_result()) })),
            write_report: Box::new(|_, _, _, _, _| Box::pin(async { Ok(()) })),
        }
    }

    async fn mount_preflight_mocks(
        server: &MockServer,
        chain_hex: &str,
        balance_hex: Option<&str>,
    ) {
        Mock::given(method("POST"))
            .and(body_string_contains("\"method\":\"eth_blockNumber\""))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"jsonrpc":"2.0","id":1,"result":"0x1"})),
            )
            .mount(server)
            .await;

        Mock::given(method("POST"))
            .and(body_string_contains("\"method\":\"eth_chainId\""))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"jsonrpc":"2.0","id":1,"result":chain_hex})),
            )
            .mount(server)
            .await;

        if let Some(balance_hex) = balance_hex {
            Mock::given(method("POST"))
                .and(body_string_contains("\"method\":\"eth_getBalance\""))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"jsonrpc":"2.0","id":1,"result":balance_hex}),
                ))
                .mount(server)
                .await;
        }
    }

    #[test]
    fn test_resolved_sender_message_pluralization() {
        assert_eq!(resolved_sender_message(1), "Resolved 1 sender key.");
        assert_eq!(resolved_sender_message(2), "Resolved 2 sender keys.");
    }

    #[test]
    fn test_should_download_missing_targets_accepts_expected_answers() {
        assert!(should_download_missing_targets(""));
        assert!(should_download_missing_targets("y"));
        assert!(should_download_missing_targets(" YES "));
        assert!(!should_download_missing_targets("n"));
    }

    #[test]
    fn test_ceiling_summary_lines_and_burst_conversion() {
        let ceiling = sample_ceiling_result();
        let lines = ceiling_summary_lines(&ceiling);
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Adaptive search: enabled"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Step 2: 200 TPS target -> 150"))
        );
        assert!(lines.iter().any(|line| line.contains("[SATURATED]")));

        let burst = burst_result_from_ceiling(&ceiling);
        assert_eq!(burst.submitted, 200);
        assert_eq!(burst.confirmed, 150);
        assert_eq!(burst.confirmed_tps, 150.0);
    }

    #[test]
    fn test_ceiling_summary_lines_handles_disabled_adaptive_search() {
        let mut ceiling = sample_ceiling_result();
        ceiling.adaptive_step_enabled = false;

        let lines = ceiling_summary_lines(&ceiling);
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Adaptive search: disabled"))
        );
    }

    #[tokio::test]
    async fn test_rpc_request_success_and_error() {
        let success_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"jsonrpc":"2.0","id":1,"result":"0x1"})),
            )
            .mount(&success_server)
            .await;

        let client = reqwest::Client::new();
        let value = rpc_request(
            &client,
            &url::Url::parse(&success_server.uri()).expect("invalid mock url"),
            "eth_blockNumber",
            serde_json::json!([]),
        )
        .await
        .expect("rpc request should succeed");
        assert_eq!(value["result"], "0x1");

        let error_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"boom"}}),
            ))
            .mount(&error_server)
            .await;

        let err = rpc_request(
            &client,
            &url::Url::parse(&error_server.uri()).expect("invalid mock url"),
            "eth_chainId",
            serde_json::json!([]),
        )
        .await
        .expect_err("rpc request should fail");
        assert!(err.to_string().contains("RPC eth_chainId failed"));
    }

    #[tokio::test]
    async fn test_run_preflight_strict_and_non_strict_paths() {
        let _guard = test_lock().lock().expect("lock poisoned");
        let strict_guard = EnvVarGuard::set("BENCH_PREFLIGHT_STRICT", "true");
        let _ = &strict_guard;

        let mismatch_server = MockServer::start().await;
        mount_preflight_mocks(&mismatch_server, "0x1", Some("0xde0b6b3a7640000")).await;

        let mismatch_config = sample_config(&mismatch_server.uri());
        let sender_keys = funding::resolve_sender_keys(1);
        let mismatch_err = run_preflight(&mismatch_config, &sender_keys)
            .await
            .expect_err("strict preflight should fail on chain mismatch");
        assert!(
            mismatch_err
                .to_string()
                .contains("configured chain_id=19803")
        );

        let non_strict_guard = EnvVarGuard::set("BENCH_PREFLIGHT_STRICT", "false");
        let min_balance_guard = EnvVarGuard::set("BENCH_PREFLIGHT_MIN_BALANCE_WEI", "100");
        let _ = (&non_strict_guard, &min_balance_guard);

        let low_balance_server = MockServer::start().await;
        mount_preflight_mocks(&low_balance_server, "0x4d5b", Some("0x1")).await;

        let low_balance_config = sample_config(&low_balance_server.uri());
        run_preflight(&low_balance_config, &sender_keys)
            .await
            .expect("non-strict preflight should warn and continue");
    }

    #[tokio::test]
    async fn test_run_preflight_non_strict_chain_mismatch_warns_and_continues() {
        let _guard = test_lock().lock().expect("lock poisoned");
        let strict_guard = EnvVarGuard::set("BENCH_PREFLIGHT_STRICT", "false");
        let min_balance_guard = EnvVarGuard::set("BENCH_PREFLIGHT_MIN_BALANCE_WEI", "1");
        let _ = (&strict_guard, &min_balance_guard);

        let server = MockServer::start().await;
        mount_preflight_mocks(&server, "0x1", Some("0xde0b6b3a7640000")).await;

        let config = sample_config(&server.uri());
        let sender_keys = funding::resolve_sender_keys(1);
        run_preflight(&config, &sender_keys)
            .await
            .expect("non-strict preflight should continue on chain mismatch");
    }

    #[tokio::test]
    async fn test_run_preflight_strict_low_balance_errors() {
        let _guard = test_lock().lock().expect("lock poisoned");
        let strict_guard = EnvVarGuard::set("BENCH_PREFLIGHT_STRICT", "true");
        let min_balance_guard = EnvVarGuard::set("BENCH_PREFLIGHT_MIN_BALANCE_WEI", "100");
        let _ = (&strict_guard, &min_balance_guard);

        let server = MockServer::start().await;
        mount_preflight_mocks(&server, "0x4d5b", Some("0x1")).await;

        let config = sample_config(&server.uri());
        let sender_keys = funding::resolve_sender_keys(1);
        let err = run_preflight(&config, &sender_keys)
            .await
            .expect_err("strict preflight should fail on low balance");
        assert!(err.to_string().contains("below minimum 100 wei"));
    }

    #[tokio::test]
    async fn test_run_preflight_ignores_missing_chain_id_result_and_empty_senders() {
        let _guard = test_lock().lock().expect("lock poisoned");
        let strict_guard = EnvVarGuard::set("BENCH_PREFLIGHT_STRICT", "true");
        let _ = &strict_guard;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("\"method\":\"eth_blockNumber\""))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"jsonrpc":"2.0","id":1,"result":"0x1"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("\"method\":\"eth_chainId\""))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"jsonrpc":"2.0","id":1,"result":null})),
            )
            .mount(&server)
            .await;

        let config = sample_config(&server.uri());
        run_preflight(&config, &[])
            .await
            .expect("preflight should succeed without sender balance checks");
    }

    #[tokio::test]
    async fn test_runtime_deps_real_exercises_download_and_runtime_wrappers() {
        let _guard = test_lock().lock().expect("lock poisoned");
        let bench_key_guard = EnvVarGuard::set("BENCH_KEY", "");
        let download_guard = EnvVarGuard::set("EVM_BENCH_TEST_SKIP_DOWNLOAD", "1");
        let _ = (&bench_key_guard, &download_guard);

        let deps = RuntimeDeps::real();
        let temp = temp_dir("evm-bench-real-deps");
        let targets_dir = temp.join("bench-targets");
        (deps.download_targets)(&targets_dir, Some("ignored-for-tests"))
            .await
            .expect("test seam should create bench-targets");
        assert!(targets_dir.join("scripts").is_dir());
        assert!(targets_dir.join("chains").is_dir());

        let fund_err = (deps.fund_senders)(
            "http://127.0.0.1:9",
            "not-a-private-key",
            &[Address::ZERO],
            19803,
            true,
        )
        .await
        .expect_err("invalid funder key should fail before network work");
        assert!(fund_err.to_string().contains("Failed to parse funder key"));

        let deploy_err =
            (deps.deploy_contracts)("http://127.0.0.1:9", "0x01", 19803, 0, 1, 0, true)
                .await
                .expect_err("pair deployment without tokens should fail");
        assert!(
            deploy_err
                .to_string()
                .contains("pair deployment requires at least one token")
        );

        let mut config = sample_config("http://127.0.0.1:9");
        config.ws = url::Url::parse("ws://127.0.0.1:9").expect("invalid ws url");
        config.quiet = true;

        let burst_err = tokio::time::timeout(Duration::from_secs(5), (deps.run_burst)(&config))
            .await
            .expect("run_burst should fail quickly")
            .expect_err("run_burst should fail against an unavailable RPC");
        let burst_message = burst_err.to_string();
        assert!(
            ["Connection refused", "error sending request"]
                .iter()
                .any(|needle| burst_message.contains(needle)),
            "unexpected burst error: {burst_message}"
        );

        let sustained_err =
            tokio::time::timeout(Duration::from_secs(5), (deps.run_sustained)(&config))
                .await
                .expect("run_sustained should fail quickly")
                .expect_err("run_sustained should fail against an unavailable RPC");
        let sustained_message = sustained_err.to_string();
        assert!(
            ["Connection refused", "error sending request"]
                .iter()
                .any(|needle| sustained_message.contains(needle)),
            "unexpected sustained error: {sustained_message}"
        );

        config.execution_mode = types::ExecutionMode::Ceiling;
        let ceiling_err = tokio::time::timeout(Duration::from_secs(5), (deps.run_ceiling)(&config))
            .await
            .expect("run_ceiling should fail quickly")
            .expect_err("run_ceiling should fail against an unavailable RPC");
        let ceiling_message = ceiling_err.to_string();
        assert!(
            ["Connection refused", "error sending request"]
                .iter()
                .any(|needle| ceiling_message.contains(needle)),
            "unexpected ceiling error: {ceiling_message}"
        );

        let report_path = temp.join("real-deps-report.json");
        (deps.write_report)(
            &config,
            &sample_burst_result(),
            &report_path,
            None,
            Some(123),
        )
        .await
        .expect("real write_report should succeed");
        assert!(report_path.is_file());
    }

    #[tokio::test]
    async fn test_runtime_download_targets_covers_test_and_real_paths() {
        let _guard = test_lock().lock().expect("lock poisoned");
        let temp = temp_dir("evm-bench-runtime-download");

        let skip_guard = EnvVarGuard::set("EVM_BENCH_TEST_SKIP_DOWNLOAD", "1");
        let skipped_dir = temp.join("skipped");
        runtime_download_targets(&skipped_dir, Some("ignored"))
            .await
            .expect("test seam should create local target directories");
        assert!(skipped_dir.join("scripts").is_dir());
        assert!(skipped_dir.join("chains").is_dir());
        drop(skip_guard);

        let real_dir = temp.join("real");
        match runtime_download_targets(&real_dir, Some("main")).await {
            Ok(()) => {
                assert!(real_dir.join("scripts").is_dir());
                assert!(real_dir.join("chains").is_dir());
            }
            Err(err) => {
                let message = err.to_string();
                assert!(
                    message.contains("failed to download archive")
                        || message.contains("error sending request")
                        || message.contains("dns error")
                        || message.contains("ConnectError"),
                    "unexpected runtime download error: {message}"
                );
            }
        }
    }

    #[test]
    fn test_prepare_run_handles_setup_and_missing_targets_prompt() {
        let _guard = test_lock().lock().expect("lock poisoned");
        let temp = temp_dir("evm-bench-prepare");
        let _cwd = CurrentDirGuard::set(&temp);

        let downloads = Arc::new(Mutex::new(Vec::new()));
        let download_calls = downloads.clone();
        let deps = RuntimeDeps {
            download_targets: Box::new(move |dest, branch| {
                let download_calls = download_calls.clone();
                let dest = dest.to_path_buf();
                let branch = branch.map(str::to_owned);
                Box::pin(async move {
                    download_calls
                        .lock()
                        .expect("lock poisoned")
                        .push((dest, branch));
                    Ok(())
                })
            }),
            ..default_test_deps()
        };

        let mut setup_args = sample_args("http://localhost:8545");
        setup_args.setup = true;
        let mut empty_input = std::io::Cursor::new(Vec::<u8>::new());
        let setup_result =
            prepare_run(setup_args, &mut empty_input, &deps).expect("setup path should succeed");
        assert!(setup_result.is_none());
        assert_eq!(downloads.lock().expect("lock poisoned").len(), 1);

        let mut yes_input = std::io::Cursor::new(b"yes\n".to_vec());
        let prompt_result =
            prepare_run(sample_args("http://localhost:8545"), &mut yes_input, &deps)
                .expect("prompt path should succeed");
        assert!(prompt_result.is_some());
        assert_eq!(downloads.lock().expect("lock poisoned").len(), 2);
    }

    #[test]
    fn test_prepare_run_update_targets_and_declined_prompt() {
        let _guard = test_lock().lock().expect("lock poisoned");
        let temp = temp_dir("evm-bench-update-targets");
        let _cwd = CurrentDirGuard::set(&temp);

        let downloads = Arc::new(Mutex::new(Vec::new()));
        let download_calls = downloads.clone();
        let deps = RuntimeDeps {
            download_targets: Box::new(move |dest, branch| {
                let download_calls = download_calls.clone();
                let dest = dest.to_path_buf();
                let branch = branch.map(str::to_owned);
                Box::pin(async move {
                    download_calls
                        .lock()
                        .expect("lock poisoned")
                        .push((dest, branch));
                    Ok(())
                })
            }),
            ..default_test_deps()
        };

        let mut args = sample_args("http://localhost:8545");
        args.update_targets = true;
        let mut decline_input = std::io::Cursor::new(b"n\n".to_vec());
        let result = prepare_run(args, &mut decline_input, &deps)
            .expect("update-targets path should still build config");
        assert!(result.is_some());
        assert_eq!(downloads.lock().expect("lock poisoned").len(), 1);
    }

    #[test]
    fn test_run_with_args_and_input_returns_early_for_setup() {
        let _guard = test_lock().lock().expect("lock poisoned");
        let temp = temp_dir("evm-bench-run-setup");
        let _cwd = CurrentDirGuard::set(&temp);

        let downloads = Arc::new(Mutex::new(0usize));
        let download_calls = downloads.clone();
        let deps = RuntimeDeps {
            download_targets: Box::new(move |_, _| {
                let download_calls = download_calls.clone();
                Box::pin(async move {
                    *download_calls.lock().expect("lock poisoned") += 1;
                    Ok(())
                })
            }),
            write_report: Box::new(|_, _, _, _, _| {
                Box::pin(async { anyhow::bail!("write_report should not be called") })
            }),
            ..default_test_deps()
        };

        let mut args = sample_args("http://localhost:8545");
        args.setup = true;
        let mut no_input = std::io::Cursor::new(Vec::<u8>::new());
        run_with_args_and_input(args, &mut no_input, &deps)
            .expect("setup run should return before executing benchmark");
        assert_eq!(*downloads.lock().expect("lock poisoned"), 1);

        let runtime = tokio::runtime::Runtime::new().expect("failed to build runtime");
        let err = runtime
            .block_on((deps.write_report)(
                &sample_config("http://localhost:8545"),
                &sample_burst_result(),
                Path::new("unused.json"),
                None,
                None,
            ))
            .expect_err("guard write_report closure should still error when called directly");
        assert!(
            err.to_string()
                .contains("write_report should not be called")
        );
    }

    #[test]
    fn test_run_with_args_and_input_and_async_main_branches() {
        let _guard = test_lock().lock().expect("lock poisoned");
        let temp = temp_dir("evm-bench-run");
        let _cwd = CurrentDirGuard::set(&temp);
        std::fs::create_dir_all(temp.join("bench-targets/scripts"))
            .expect("failed to create scripts dir");
        std::fs::create_dir_all(temp.join("bench-targets/chains"))
            .expect("failed to create chains dir");

        let runtime = tokio::runtime::Runtime::new().expect("failed to build runtime");

        let server = runtime.block_on(MockServer::start());
        runtime.block_on(mount_preflight_mocks(
            &server,
            "0x4d5b",
            Some("0xde0b6b3a7640000"),
        ));

        let wrote_paths = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
        let burst_writes = wrote_paths.clone();
        let deps = RuntimeDeps {
            write_report: Box::new(move |_, _, output, _, gas_price| {
                let burst_writes = burst_writes.clone();
                let output = output.to_path_buf();
                Box::pin(async move {
                    assert_eq!(gas_price, Some(123));
                    burst_writes.lock().expect("lock poisoned").push(output);
                    Ok(())
                })
            }),
            ..default_test_deps()
        };

        let mut args = sample_args(&server.uri());
        args.out = temp.join("burst-report.json");
        let mut no_prompt = std::io::Cursor::new(Vec::<u8>::new());
        run_with_args_and_input(args, &mut no_prompt, &deps).expect("burst run should succeed");
        assert_eq!(wrote_paths.lock().expect("lock poisoned").len(), 1);

        let sustained_writes = Arc::new(Mutex::new(Vec::<Option<u128>>::new()));
        let sustained_records = sustained_writes.clone();
        let sustained_deps = RuntimeDeps {
            write_report: Box::new(move |_, result, _, ceiling_meta, gas_price| {
                let sustained_records = sustained_records.clone();
                let confirmed = result.confirmed;
                Box::pin(async move {
                    assert_eq!(confirmed, 18);
                    assert!(ceiling_meta.is_none());
                    sustained_records
                        .lock()
                        .expect("lock poisoned")
                        .push(gas_price);
                    Ok(())
                })
            }),
            ..default_test_deps()
        };

        let mut sustained_config = sample_config(&server.uri());
        sustained_config.execution_mode = types::ExecutionMode::Sustained;
        runtime
            .block_on(async_main_with(sustained_config, &sustained_deps))
            .expect("sustained branch should succeed");
        assert_eq!(
            sustained_writes.lock().expect("lock poisoned").as_slice(),
            &[Some(456)]
        );

        let ceiling_server = runtime.block_on(MockServer::start());
        runtime.block_on(mount_preflight_mocks(&ceiling_server, "0x4d5b", None));

        let fund_calls = Arc::new(Mutex::new(0usize));
        let deploy_calls = Arc::new(Mutex::new(0usize));
        let write_calls = Arc::new(Mutex::new(0usize));
        let fund_calls_clone = fund_calls.clone();
        let deploy_calls_clone = deploy_calls.clone();
        let write_calls_clone = write_calls.clone();
        let ceiling_deps = RuntimeDeps {
            fund_senders: Box::new(move |_, _, _, _, _| {
                let fund_calls_clone = fund_calls_clone.clone();
                Box::pin(async move {
                    *fund_calls_clone.lock().expect("lock poisoned") += 1;
                    Ok(())
                })
            }),
            deploy_contracts: Box::new(move |_, _, _, _, _, _, _| {
                let deploy_calls_clone = deploy_calls_clone.clone();
                Box::pin(async move {
                    *deploy_calls_clone.lock().expect("lock poisoned") += 1;
                    Ok(generators::contract_deploy::EvmContracts {
                        tokens: vec![Address::with_last_byte(1)],
                        pairs: vec![Address::with_last_byte(2)],
                        nfts: vec![Address::with_last_byte(3)],
                    })
                })
            }),
            write_report: Box::new(move |_, result, _, ceiling_meta, gas_price| {
                let write_calls_clone = write_calls_clone.clone();
                let submitted = result.submitted;
                Box::pin(async move {
                    *write_calls_clone.lock().expect("lock poisoned") += 1;
                    assert_eq!(submitted, 200);
                    assert!(ceiling_meta.is_some());
                    assert!(gas_price.is_none());
                    Ok(())
                })
            }),
            ..default_test_deps()
        };

        let mut ceiling_config = sample_config(&ceiling_server.uri());
        ceiling_config.execution_mode = types::ExecutionMode::Ceiling;
        ceiling_config.test_mode = types::TestMode::Evm;
        ceiling_config.fund = true;
        ceiling_config.quiet = false;
        runtime
            .block_on(async_main_with(ceiling_config, &ceiling_deps))
            .expect("ceiling branch should succeed");

        assert_eq!(*fund_calls.lock().expect("lock poisoned"), 1);
        assert_eq!(*deploy_calls.lock().expect("lock poisoned"), 1);
        assert_eq!(*write_calls.lock().expect("lock poisoned"), 1);
    }

    #[test]
    fn test_async_main_with_ceiling_quiet_skips_summary_print_loop() {
        let _guard = test_lock().lock().expect("lock poisoned");
        let runtime = tokio::runtime::Runtime::new().expect("failed to build runtime");

        let server = runtime.block_on(MockServer::start());
        runtime.block_on(mount_preflight_mocks(
            &server,
            "0x4d5b",
            Some("0xde0b6b3a7640000"),
        ));

        let write_calls = Arc::new(Mutex::new(0usize));
        let write_calls_clone = write_calls.clone();
        let deps = RuntimeDeps {
            write_report: Box::new(move |_, result, _, ceiling_meta, gas_price| {
                let write_calls_clone = write_calls_clone.clone();
                let confirmed = result.confirmed;
                Box::pin(async move {
                    *write_calls_clone.lock().expect("lock poisoned") += 1;
                    assert_eq!(confirmed, 150);
                    assert!(ceiling_meta.is_some());
                    assert!(gas_price.is_none());
                    Ok(())
                })
            }),
            ..default_test_deps()
        };

        let mut config = sample_config(&server.uri());
        config.execution_mode = types::ExecutionMode::Ceiling;
        config.quiet = true;
        runtime
            .block_on(async_main_with(config, &deps))
            .expect("quiet ceiling branch should still write a report");
        assert_eq!(*write_calls.lock().expect("lock poisoned"), 1);
    }

    #[tokio::test]
    async fn test_async_main_surfaces_preflight_errors() {
        let _guard = test_lock().lock().expect("lock poisoned");
        let server = MockServer::start().await;
        mount_preflight_mocks(&server, "0x1", Some("0xde0b6b3a7640000")).await;

        let err = async_main(sample_config(&server.uri()))
            .await
            .expect_err("async_main should stop on preflight failure");
        assert!(err.to_string().contains("configured chain_id=19803"));
    }

    #[tokio::test]
    async fn test_default_test_deps_deploy_contracts_returns_sample_addresses() {
        let deps = default_test_deps();
        let contracts =
            (deps.deploy_contracts)("http://localhost:8545", "0x01", 19803, 5, 3, 2, true)
                .await
                .expect("default deploy_contracts should return sample contracts");

        assert_eq!(contracts.tokens, vec![Address::with_last_byte(1)]);
        assert_eq!(contracts.pairs, vec![Address::with_last_byte(2)]);
        assert_eq!(contracts.nfts, vec![Address::with_last_byte(3)]);
    }
}
