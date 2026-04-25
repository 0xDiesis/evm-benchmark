// SPDX-License-Identifier: MIT
pragma solidity 0.8.34;

/// @title BenchmarkMixer
/// @notice E2E benchmark contract exercising storage, computation, and events.
/// @dev Used by the Rust evm-benchmark to measure EVM throughput under
///      realistic mixed workloads. Not intended for production deployment.
contract BenchmarkMixer {
    mapping(uint256 => uint256) public store;
    uint256 public counter;

    event Stored(address indexed sender, uint256 key, uint256 value);
    event Computed(address indexed sender, uint256 result);

    /// @notice Write a single key-value pair, increment counter, emit event.
    /// @dev ~45k gas (1 cold SSTORE + 1 warm SSTORE + LOG2)
    function writeStore(uint256 key, uint256 value) external {
        store[key] = value;
        counter++;
        emit Stored(msg.sender, key, value);
    }

    /// @notice Hash a seed iteratively. Pure computation benchmark.
    /// @dev ~28k gas at 10 iterations (keccak256 loop + LOG2)
    function computeHash(uint256 seed, uint256 iterations) external returns (uint256 result) {
        bytes32 h = bytes32(seed);
        for (uint256 i; i < iterations;) {
            h = keccak256(abi.encodePacked(h));
            unchecked {
                ++i;
            }
        }
        result = uint256(h);
        emit Computed(msg.sender, result);
    }

    /// @notice Write three key-value pairs in one call. Storage-heavy benchmark.
    /// @dev ~65k gas (3 cold SSTOREs + 1 warm SSTORE + LOG2)
    function batchWrite(uint256 base, uint256 a, uint256 b, uint256 c) external {
        store[base] = a;
        store[base + 1] = b;
        store[base + 2] = c;
        counter += 3;
        emit Stored(msg.sender, base, a);
    }

    /// @notice Read a value from the store. View-only, no state change.
    function readStore(uint256 key) external view returns (uint256 value) {
        value = store[key];
    }
}
