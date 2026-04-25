// SPDX-License-Identifier: MIT
pragma solidity ^0.8.34;

import {Test} from "forge-std/Test.sol";
import {BenchmarkToken} from "../../src/benchmark/BenchmarkToken.sol";

contract BenchmarkTokenTest is Test {
    BenchmarkToken token;
    address alice = address(0xA11CE);
    address bob = address(0xB0B);

    function setUp() public {
        token = new BenchmarkToken();
    }

    function test_mint() public {
        token.mint(alice, 1000e18);
        assertEq(token.balanceOf(alice), 1000e18);
        assertEq(token.totalSupply(), 1000e18);
    }

    function test_transfer() public {
        token.mint(alice, 1000e18);
        vm.prank(alice);
        assertTrue(token.transfer(bob, 100e18));
        assertEq(token.balanceOf(alice), 900e18);
        assertEq(token.balanceOf(bob), 100e18);
    }

    function test_transfer_gas() public {
        token.mint(alice, 1000e18);
        vm.prank(alice);
        uint256 gasBefore = gasleft();
        assertTrue(token.transfer(bob, 100e18));
        uint256 gasUsed = gasBefore - gasleft();
        assertGt(gasUsed, 20_000);
        assertLt(gasUsed, 60_000);
    }

    function test_approve_and_transferFrom() public {
        token.mint(alice, 1000e18);
        vm.prank(alice);
        token.approve(bob, 500e18);

        vm.prank(bob);
        assertTrue(token.transferFrom(alice, bob, 200e18));
        assertEq(token.balanceOf(alice), 800e18);
        assertEq(token.balanceOf(bob), 200e18);
        assertEq(token.allowance(alice, bob), 300e18);
    }

    function test_transfer_reverts_insufficient() public {
        token.mint(alice, 100e18);
        vm.prank(alice);
        vm.expectRevert(BenchmarkToken.InsufficientBalance.selector);
        bool success = token.transfer(bob, 200e18);
        assertFalse(success);
    }

    function test_transferFrom_reverts_insufficient_balance() public {
        token.mint(alice, 100e18);
        vm.prank(alice);
        token.approve(bob, type(uint256).max);

        vm.prank(bob);
        vm.expectRevert(BenchmarkToken.InsufficientBalance.selector);
        token.transferFrom(alice, bob, 200e18);
    }

    function test_transferFrom_reverts_insufficient_allowance() public {
        token.mint(alice, 1000e18);
        vm.prank(alice);
        token.approve(bob, 50e18);

        vm.prank(bob);
        vm.expectRevert(BenchmarkToken.InsufficientAllowance.selector);
        token.transferFrom(alice, bob, 100e18);
    }
}
