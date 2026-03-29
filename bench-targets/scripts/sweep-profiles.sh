#!/bin/bash
# sweep-profiles.sh — Parameter sweep profile definitions.
# Sourced by sweep.sh. Each profile defines a parameter name, values, and description.

# Each profile: SWEEP_<name>_PARAM  = the E2E_ env var name
#               SWEEP_<name>_VALUES = space-separated values to try
#               SWEEP_<name>_DESC   = human description

# Block production interval
SWEEP_BLOCK_PERIOD_PARAM="E2E_BLOCK_PERIOD"
SWEEP_BLOCK_PERIOD_VALUES="50ms 100ms 150ms 200ms 500ms 1s"
SWEEP_BLOCK_PERIOD_DESC="Block production interval"

# DAG ordering window
SWEEP_ORDERING_WINDOW_PARAM="E2E_ORDERING_WINDOW"
SWEEP_ORDERING_WINDOW_VALUES="5ms 10ms 20ms 50ms 100ms"
SWEEP_ORDERING_WINDOW_DESC="DAG ordering window"

# Max transactions per block
SWEEP_MAX_BLOCK_TXS_PARAM="E2E_MAX_BLOCK_TX_COUNT"
SWEEP_MAX_BLOCK_TXS_VALUES="1000 2000 5000 10000 20000"
SWEEP_MAX_BLOCK_TXS_DESC="Max transactions per block"

# Parallel execution mode
SWEEP_PARALLEL_EXECUTION_PARAM="E2E_PARALLEL_EXECUTION"
SWEEP_PARALLEL_EXECUTION_VALUES="full sequential"
SWEEP_PARALLEL_EXECUTION_DESC="Execution parallelization mode"

# State commitment mode
SWEEP_COMMITMENT_MODE_PARAM="E2E_COMMITMENT_MODE"
SWEEP_COMMITMENT_MODE_VALUES="mpt verkle"
SWEEP_COMMITMENT_MODE_DESC="State commitment type"

# Max gas per proposal
SWEEP_PROPOSAL_GAS_PARAM="E2E_MAX_GAS_PER_PROPOSAL"
SWEEP_PROPOSAL_GAS_VALUES="15000000 30000000 60000000 120000000"
SWEEP_PROPOSAL_GAS_DESC="Max gas per proposal"

# Min consensus round delay
SWEEP_ROUND_DELAY_PARAM="E2E_MIN_ROUND_DELAY"
SWEEP_ROUND_DELAY_VALUES="5ms 10ms 20ms 50ms"
SWEEP_ROUND_DELAY_DESC="Min consensus round delay"

# Max proposal transaction count
SWEEP_MAX_PROPOSAL_TXS_PARAM="E2E_MAX_PROPOSAL_TX_COUNT"
SWEEP_MAX_PROPOSAL_TXS_VALUES="256 512 1024 2048 4096"
SWEEP_MAX_PROPOSAL_TXS_DESC="Max transactions per proposal"

# ── Registry of all profile names ────────────────────────────────────────
_SWEEP_PROFILES="block-period ordering-window max-block-txs parallel-execution commitment-mode proposal-gas round-delay max-proposal-txs"

# ── list_sweep_profiles ─────────────────────────────────────────────────
list_sweep_profiles() {
    printf "%-24s  %s\n" "PROFILE" "DESCRIPTION"
    printf "%-24s  %s\n" "-------" "-----------"
    for profile in $_SWEEP_PROFILES; do
        local key
        key="SWEEP_$(echo "$profile" | tr '-' '_' | tr '[:lower:]' '[:upper:]')_DESC"
        printf "%-24s  %s\n" "$profile" "${!key}"
    done
}

# ── load_sweep_profile <name> ───────────────────────────────────────────
# Sets SWEEP_PARAM, SWEEP_VALUES, SWEEP_DESC from the named profile.
load_sweep_profile() {
    local name="${1:?Usage: load_sweep_profile <profile-name>}"
    local upper
    upper="$(echo "$name" | tr '-' '_' | tr '[:lower:]' '[:upper:]')"

    local param_var="SWEEP_${upper}_PARAM"
    local values_var="SWEEP_${upper}_VALUES"
    local desc_var="SWEEP_${upper}_DESC"

    if [[ -z "${!param_var:-}" ]]; then
        echo "ERROR: Unknown sweep profile '${name}'" >&2
        echo "Available profiles:" >&2
        list_sweep_profiles >&2
        return 1
    fi

    SWEEP_PARAM="${!param_var}"
    SWEEP_VALUES="${!values_var}"
    SWEEP_DESC="${!desc_var}"
}
