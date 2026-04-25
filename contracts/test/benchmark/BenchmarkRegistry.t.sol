// SPDX-License-Identifier: MIT
pragma solidity ^0.8.34;

import {Test} from "forge-std/Test.sol";
import {BenchmarkRegistry} from "../../src/benchmark/BenchmarkRegistry.sol";

contract BenchmarkRegistryTest is Test {
    BenchmarkRegistry registry;
    address alice = address(0xA11CE);
    address bob = address(0xB0B);

    function setUp() public {
        registry = new BenchmarkRegistry();
    }

    function test_register() public {
        vm.prank(alice);
        registry.register(1, 42);

        (address owner, uint256 value, uint256 ts) = registry.lookup(1);
        assertEq(owner, alice);
        assertEq(value, 42);
        assertGt(ts, 0);
        assertEq(registry.entryCount(), 1);
    }

    function test_register_gas() public {
        vm.prank(alice);
        uint256 gasBefore = gasleft();
        registry.register(1, 100);
        uint256 gasUsed = gasBefore - gasleft();
        assertGt(gasUsed, 40_000);
        assertLt(gasUsed, 120_000);
    }

    function test_update() public {
        vm.prank(alice);
        registry.register(1, 42);

        vm.prank(alice);
        registry.update(1, 99);

        (, uint256 value,) = registry.lookup(1);
        assertEq(value, 99);
    }

    function test_update_gas() public {
        vm.prank(alice);
        registry.register(1, 42);

        vm.prank(alice);
        uint256 gasBefore = gasleft();
        registry.update(1, 99);
        uint256 gasUsed = gasBefore - gasleft();
        assertGt(gasUsed, 2_000);
        assertLt(gasUsed, 30_000);
    }

    function test_register_reverts_duplicate() public {
        vm.prank(alice);
        registry.register(1, 42);

        vm.prank(bob);
        vm.expectRevert(BenchmarkRegistry.AlreadyRegistered.selector);
        registry.register(1, 99);
    }

    function test_update_reverts_not_owner() public {
        vm.prank(alice);
        registry.register(1, 42);

        vm.prank(bob);
        vm.expectRevert(BenchmarkRegistry.NotOwner.selector);
        registry.update(1, 99);
    }
}
