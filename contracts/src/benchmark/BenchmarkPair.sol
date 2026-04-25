// SPDX-License-Identifier: MIT
pragma solidity 0.8.34;

import {BenchmarkToken} from "./BenchmarkToken.sol";

/// @title BenchmarkPair
/// @notice Simplified constant-product AMM pair for benchmarking.
///         Models Uniswap-style reserve updates with token transfers.
///         No actual price invariant enforcement — optimized for gas
///         profiling, not economic correctness.
contract BenchmarkPair {
    error TransferFromFailed();
    error TransferFailed();
    error ZeroAmount();
    error NoLiquidity();
    error InsufficientOutput();

    BenchmarkToken public token0;
    BenchmarkToken public token1;
    uint256 public reserve0;
    uint256 public reserve1;
    uint256 public swapCount;

    event Swap(address indexed sender, uint256 amountIn, uint256 amountOut, bool zeroForOne);
    event LiquidityAdded(address indexed provider, uint256 amount0, uint256 amount1);

    constructor(address _token0, address _token1) {
        token0 = BenchmarkToken(_token0);
        token1 = BenchmarkToken(_token1);
    }

    /// @notice Add liquidity (benchmark helper — no LP tokens minted).
    function addLiquidity(uint256 amount0, uint256 amount1) external {
        if (!token0.transferFrom(msg.sender, address(this), amount0)) revert TransferFromFailed();
        if (!token1.transferFrom(msg.sender, address(this), amount1)) revert TransferFromFailed();
        reserve0 += amount0;
        reserve1 += amount1;
        emit LiquidityAdded(msg.sender, amount0, amount1);
    }

    /// @notice Swap token0 for token1 (or vice versa).
    /// @dev Uses a simplified 0.3% fee constant-product formula.
    ///      ~80-120k gas depending on token state (cold/warm).
    function swap(uint256 amountIn, bool zeroForOne) external {
        if (amountIn == 0) revert ZeroAmount();

        uint256 amountOut;
        if (zeroForOne) {
            if (reserve0 == 0 || reserve1 == 0) revert NoLiquidity();
            // amountOut = reserve1 * amountIn * 997 / (reserve0 * 1000 + amountIn * 997)
            uint256 amountInWithFee = amountIn * 997;
            amountOut = (reserve1 * amountInWithFee) / (reserve0 * 1000 + amountInWithFee);
            if (amountOut == 0) revert InsufficientOutput();

            if (!token0.transferFrom(msg.sender, address(this), amountIn)) revert TransferFromFailed();
            if (!token1.transfer(msg.sender, amountOut)) revert TransferFailed();
            reserve0 += amountIn;
            reserve1 -= amountOut;
        } else {
            if (reserve0 == 0 || reserve1 == 0) revert NoLiquidity();
            uint256 amountInWithFee = amountIn * 997;
            amountOut = (reserve0 * amountInWithFee) / (reserve1 * 1000 + amountInWithFee);
            if (amountOut == 0) revert InsufficientOutput();

            if (!token1.transferFrom(msg.sender, address(this), amountIn)) revert TransferFromFailed();
            if (!token0.transfer(msg.sender, amountOut)) revert TransferFailed();
            reserve1 += amountIn;
            reserve0 -= amountOut;
        }

        swapCount++;
        emit Swap(msg.sender, amountIn, amountOut, zeroForOne);
    }
}
