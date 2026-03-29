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

use clap::Parser;

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
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(true);

    let client = reqwest::Client::new();

    // 1) RPC reachability.
    let _ = rpc_request(
        &client,
        &config.rpc,
        "eth_blockNumber",
        serde_json::json!([]),
    )
    .await?;

    // 2) Chain ID sanity.
    let chain_resp =
        rpc_request(&client, &config.rpc, "eth_chainId", serde_json::json!([])).await?;
    if let Some(chain_hex) = chain_resp.get("result").and_then(|v| v.as_str()) {
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

    // 3) No-fund runs should have a funded first signer.
    if !config.fund {
        let parsed = funding::parse_sender_keys(sender_keys)?;
        if let Some((_, _, first_addr)) = parsed.first() {
            let bal = rpc_request(
                &client,
                &config.rpc,
                "eth_getBalance",
                serde_json::json!([format!("{:?}", first_addr), "latest"]),
            )
            .await?;
            let bal_hex = bal.get("result").and_then(|v| v.as_str()).unwrap_or("0x0");
            let wei = u128::from_str_radix(bal_hex.trim_start_matches("0x"), 16).unwrap_or(0);
            let min_wei: u128 = std::env::var("BENCH_PREFLIGHT_MIN_BALANCE_WEI")
                .ok()
                .and_then(|v| v.parse::<u128>().ok())
                .unwrap_or(1_000_000_000_000_000_000);
            if wei < min_wei {
                let msg = format!(
                    "Preflight: first sender {:?} balance {} wei is below minimum {} wei (set --fund or BENCH_KEY)",
                    first_addr, wei, min_wei
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

fn main() -> anyhow::Result<()> {
    let args = config::Args::parse();

    // Handle --setup and --update-targets before anything else.
    if args.setup || args.update_targets {
        let dest = setup::default_targets_dir();
        let branch = args.targets_branch.clone();
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(setup::download_targets(&dest, Some(&branch)))?;
        if args.setup {
            return Ok(());
        }
    }

    // Auto-detect missing bench-targets and offer to download.
    let targets_dir = setup::default_targets_dir();
    if !setup::targets_exist(&targets_dir) {
        eprintln!("Bench-targets not found at {}", targets_dir.display());
        eprint!("Download the latest bench-targets from GitHub? [Y/n] ");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let answer = input.trim().to_lowercase();
        if answer.is_empty() || answer == "y" || answer == "yes" {
            let branch = args.targets_branch.clone();
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(setup::download_targets(&targets_dir, Some(&branch)))?;
        }
    }

    let config = args.into_config()?;

    // Build and enter the tokio runtime.
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async_main(config))
}

async fn async_main(mut config: config::Config) -> anyhow::Result<()> {
    // ── Resolve sender keys ────────────────────────────────────────────
    let sender_keys = funding::resolve_sender_keys(config.sender_count);
    if !config.quiet {
        println!(
            "Resolved {} sender key{}.",
            sender_keys.len(),
            if sender_keys.len() == 1 { "" } else { "s" }
        );
    }

    // ── Preflight guardrails ────────────────────────────────────────────
    run_preflight(&config, &sender_keys).await?;

    // ── Auto-fund senders (if --fund) ──────────────────────────────────
    // This phase deploys a MultiSend contract and batch-funds all senders.
    // It runs BEFORE the benchmark and is NOT counted toward TPS.
    if config.fund {
        let parsed = funding::parse_sender_keys(&sender_keys)?;
        let addresses: Vec<_> = parsed.iter().map(|(_, _, addr)| *addr).collect();
        let funder_key = &sender_keys[0];
        funding::fund_senders(
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

    // ��─ Deploy benchmark contracts (if EVM mode + --fund) ────────────────
    // Deploy ERC-20, AMM pair, and NFT contracts for realistic EVM workloads.
    // Deployment is NOT counted toward TPS.
    if config.test_mode == types::TestMode::Evm && config.fund {
        let deployer_key = &sender_keys[0];
        let contracts = generators::contract_deploy::deploy_contracts(
            config.rpc.as_str(),
            deployer_key,
            config.chain_id,
            5, // 5 tokens
            3, // 3 AMM pairs
            2, // 2 NFTs
            config.quiet,
        )
        .await?;

        // Store deployed contract addresses in config for the EVM generator.
        config.evm_tokens = contracts.tokens;
        config.evm_pairs = contracts.pairs;
        config.evm_nfts = contracts.nfts;

        if !config.quiet {
            println!("Contracts deployed. Starting benchmark...\n");
        }
    }

    // ── Store resolved keys in config for modes to use ───────────────────
    config.sender_keys = sender_keys;

    if !config.quiet {
        println!("Starting benchmark...");
    }

    let mut ceiling_meta: Option<types::CeilingResult> = None;
    let mut effective_gas_price: Option<u128> = None;

    let result = match config.execution_mode {
        types::ExecutionMode::Burst => {
            let (burst, gas_price) = modes::run_burst(&config).await?;
            effective_gas_price = Some(gas_price);
            burst
        }
        types::ExecutionMode::Sustained => {
            let (sustained, gas_price) = modes::run_sustained(&config).await?;
            effective_gas_price = Some(gas_price);
            sustained.to_burst_result()
        }
        types::ExecutionMode::Ceiling => {
            let ceiling = modes::run_ceiling(&config).await?;
            ceiling_meta = Some(ceiling.clone());

            // Convert ceiling result to burst-like result for reporting
            if !config.quiet {
                println!();
                println!("╔════════════════════════════════════════════════════╗");
                println!("║          CEILING MODE - FINAL RESULTS            ║");
                println!("╚════════════════════════════════════════════════════╝");
                println!("Steps taken: {}", ceiling.steps.len());
                println!("Ceiling TPS: {}", ceiling.ceiling_tps);
                println!("Peak TPS:    {}", ceiling.burst_peak_tps);
                println!(
                    "Confidence:  {:.0}% (band {}-{} TPS)",
                    ceiling.confidence_score * 100.0,
                    ceiling.confidence_band_low,
                    ceiling.confidence_band_high
                );
                println!(
                    "Adaptive search: {}",
                    if ceiling.adaptive_step_enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                );
                println!();
                println!("Ramp-up Details:");
                for (i, step) in ceiling.steps.iter().enumerate() {
                    println!(
                        "  Step {}: {} TPS target -> {:.1} TPS actual | Pending: {:.1}% | Errors: {:.1}%{}",
                        i + 1,
                        step.target_tps,
                        step.actual_tps,
                        step.pending_ratio * 100.0,
                        step.error_rate * 100.0,
                        if step.is_saturated {
                            " [SATURATED]"
                        } else {
                            ""
                        }
                    );
                }
            }

            // Create a synthetic BurstResult from ceiling metrics
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
    };

    // Write report
    reporting::write_report(
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
