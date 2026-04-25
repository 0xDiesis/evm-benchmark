// SPDX-License-Identifier: MIT
pragma solidity ^0.8.34;

import {Test} from "forge-std/Test.sol";
import {BenchmarkMixer} from "../../src/benchmark/BenchmarkMixer.sol";

contract BenchmarkMixerTest is Test {
    BenchmarkMixer mixer;
    address sender = address(0xBEEF);

    function setUp() public {
        mixer = new BenchmarkMixer();
    }

    // -------------------------------------------------------------------------
    // writeStore
    // -------------------------------------------------------------------------

    function test_writeStore() public {
        vm.prank(sender);
        mixer.writeStore(42, 100);

        assertEq(mixer.store(42), 100);
        assertEq(mixer.counter(), 1);
    }

    function test_writeStore_gas() public {
        // Cold write to new slot
        uint256 gasBefore = gasleft();
        mixer.writeStore(1, 999);
        uint256 gasUsed = gasBefore - gasleft();

        // Expect 30k-60k gas for cold SSTORE + counter increment + event
        assertGt(gasUsed, 30_000, "writeStore too cheap");
        assertLt(gasUsed, 60_000, "writeStore too expensive");
    }

    // -------------------------------------------------------------------------
    // computeHash
    // -------------------------------------------------------------------------

    function test_computeHash_deterministic() public {
        uint256 result1 = mixer.computeHash(123, 10);
        uint256 result2 = mixer.computeHash(123, 10);
        assertEq(result1, result2, "computeHash should be deterministic");
        assertTrue(result1 != 0, "result should be non-zero for non-trivial seed");
    }

    function test_computeHash_zero_iterations() public {
        // No keccak loop iterations: result must equal seed cast through bytes32.
        uint256 result = mixer.computeHash(42, 0);
        assertEq(result, 42);
    }

    function test_computeHash_different_seeds() public {
        uint256 result1 = mixer.computeHash(1, 10);
        uint256 result2 = mixer.computeHash(2, 10);
        assertTrue(result1 != result2, "different seeds should give different results");
    }

    function test_computeHash_gas() public {
        uint256 gasBefore = gasleft();
        mixer.computeHash(42, 10);
        uint256 gasUsed = gasBefore - gasleft();

        // Expect 5k-30k gas for 10 keccak iterations + event
        assertGt(gasUsed, 5_000, "computeHash too cheap");
        assertLt(gasUsed, 30_000, "computeHash too expensive");
    }

    // -------------------------------------------------------------------------
    // batchWrite
    // -------------------------------------------------------------------------

    function test_batchWrite() public {
        mixer.batchWrite(100, 10, 20, 30);

        assertEq(mixer.store(100), 10);
        assertEq(mixer.store(101), 20);
        assertEq(mixer.store(102), 30);
        assertEq(mixer.counter(), 3);
    }

    function test_batchWrite_gas() public {
        uint256 gasBefore = gasleft();
        mixer.batchWrite(200, 1, 2, 3);
        uint256 gasUsed = gasBefore - gasleft();

        // Expect 50k-120k gas for 3 cold SSTOREs + counter + event
        assertGt(gasUsed, 50_000, "batchWrite too cheap");
        assertLt(gasUsed, 120_000, "batchWrite too expensive");
    }

    // -------------------------------------------------------------------------
    // readStore
    // -------------------------------------------------------------------------

    function test_readStore_empty() public view {
        assertEq(mixer.readStore(999), 0);
    }

    function test_readStore_written() public {
        mixer.writeStore(7, 42);
        assertEq(mixer.readStore(7), 42);
    }

    // -------------------------------------------------------------------------
    // counter accumulation
    // -------------------------------------------------------------------------

    function test_counter_accumulates() public {
        mixer.writeStore(1, 1); // +1
        mixer.writeStore(2, 2); // +1
        mixer.batchWrite(3, 1, 2, 3); // +3
        assertEq(mixer.counter(), 5);
    }
}
