/// Contract deployment helpers for benchmark contracts.
pub mod contract_deploy;
/// EVM mixed-workload transaction generator.
pub mod evm_mix;
/// The pinned judged mixed-EVM workload for Diesis speed runs.
pub mod judged_mix;
/// Simple ETH transfer generator.
pub mod simple_transfer;
/// Direct EVM calls to the Diesis exchange precompiles.
pub mod spot_book;

#[allow(unused_imports)]
pub use contract_deploy::{EvmContracts, deploy_contracts};
#[allow(unused_imports)]
pub use evm_mix::{EvmMixConfig, EvmMixGenerator, EvmTxDescriptor};
#[allow(unused_imports)]
pub use simple_transfer::SimpleTransferGenerator;

use alloy_rpc_types::TransactionRequest;

/// Trait for stateful transaction generators.
pub trait TxGenerator: Send + Sync {
    /// Produce the next unsigned [`TransactionRequest`].
    fn next(&mut self) -> TransactionRequest;
}
