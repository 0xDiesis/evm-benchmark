// SPDX-License-Identifier: MIT
pragma solidity ^0.8.34;

import {Test} from "forge-std/Test.sol";
import {BenchmarkToken} from "../../src/benchmark/BenchmarkToken.sol";
import {BenchmarkPair} from "../../src/benchmark/BenchmarkPair.sol";

contract BenchmarkPairTest is Test {
    BenchmarkToken token0;
    BenchmarkToken token1;
    BenchmarkPair pair;
    address lp = address(0x1234);
    address trader = address(0xBEEF);

    function setUp() public {
        token0 = new BenchmarkToken();
        token1 = new BenchmarkToken();
        pair = new BenchmarkPair(address(token0), address(token1));

        // Fund LP and add liquidity
        token0.mint(lp, 10_000e18);
        token1.mint(lp, 10_000e18);
        vm.startPrank(lp);
        token0.approve(address(pair), type(uint256).max);
        token1.approve(address(pair), type(uint256).max);
        pair.addLiquidity(10_000e18, 10_000e18);
        vm.stopPrank();

        // Fund trader
        token0.mint(trader, 1000e18);
        token1.mint(trader, 1000e18);
        vm.startPrank(trader);
        token0.approve(address(pair), type(uint256).max);
        token1.approve(address(pair), type(uint256).max);
        vm.stopPrank();
    }

    function test_addLiquidity() public view {
        assertEq(pair.reserve0(), 10_000e18);
        assertEq(pair.reserve1(), 10_000e18);
    }

    function test_swap_zeroForOne() public {
        vm.prank(trader);
        pair.swap(100e18, true);

        assertGt(token1.balanceOf(trader), 1000e18, "trader should receive token1");
        assertEq(token0.balanceOf(trader), 900e18);
        assertEq(pair.swapCount(), 1);
    }

    function test_swap_oneForZero() public {
        vm.prank(trader);
        pair.swap(100e18, false);

        assertGt(token0.balanceOf(trader), 1000e18, "trader should receive token0");
        assertEq(token1.balanceOf(trader), 900e18);
    }

    function test_swap_gas() public {
        vm.prank(trader);
        uint256 gasBefore = gasleft();
        pair.swap(100e18, true);
        uint256 gasUsed = gasBefore - gasleft();
        // Swap touches: 2 transferFrom, 1 transfer, reserve updates, event
        assertGt(gasUsed, 60_000);
        assertLt(gasUsed, 200_000);
    }

    function test_multiple_swaps() public {
        vm.startPrank(trader);
        pair.swap(10e18, true);
        pair.swap(10e18, false);
        pair.swap(10e18, true);
        vm.stopPrank();
        assertEq(pair.swapCount(), 3);
    }

    function test_swap_reverts_zero_amount() public {
        vm.prank(trader);
        vm.expectRevert(BenchmarkPair.ZeroAmount.selector);
        pair.swap(0, true);
    }

    function test_swap_reverts_no_liquidity_zeroForOne() public {
        BenchmarkToken t0 = new BenchmarkToken();
        BenchmarkToken t1 = new BenchmarkToken();
        BenchmarkPair empty = new BenchmarkPair(address(t0), address(t1));

        t0.mint(trader, 100e18);
        vm.startPrank(trader);
        t0.approve(address(empty), type(uint256).max);
        vm.expectRevert(BenchmarkPair.NoLiquidity.selector);
        empty.swap(1e18, true);
        vm.stopPrank();
    }

    function test_swap_reverts_no_liquidity_oneForZero() public {
        BenchmarkToken t0 = new BenchmarkToken();
        BenchmarkToken t1 = new BenchmarkToken();
        BenchmarkPair empty = new BenchmarkPair(address(t0), address(t1));

        t1.mint(trader, 100e18);
        vm.startPrank(trader);
        t1.approve(address(empty), type(uint256).max);
        vm.expectRevert(BenchmarkPair.NoLiquidity.selector);
        empty.swap(1e18, false);
        vm.stopPrank();
    }

    function test_swap_reverts_insufficient_output_zeroForOne() public {
        // Tiny amountIn against huge reserves rounds amountOut to zero.
        vm.prank(trader);
        vm.expectRevert(BenchmarkPair.InsufficientOutput.selector);
        pair.swap(1, true);
    }

    function test_swap_reverts_insufficient_output_oneForZero() public {
        vm.prank(trader);
        vm.expectRevert(BenchmarkPair.InsufficientOutput.selector);
        pair.swap(1, false);
    }
}
