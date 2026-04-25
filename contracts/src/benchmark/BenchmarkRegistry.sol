// SPDX-License-Identifier: MIT
pragma solidity 0.8.34;

/// @title BenchmarkRegistry
/// @notice Key-value registry for benchmarking. Models NFT metadata,
///         ENS-style name registration, and on-chain config stores.
///         Variable gas cost based on value size.
contract BenchmarkRegistry {
    error AlreadyRegistered();
    error NotOwner();

    struct Entry {
        address owner;
        uint256 value;
        uint256 timestamp;
    }

    mapping(uint256 => Entry) public entries;
    uint256 public entryCount;

    event Registered(address indexed owner, uint256 indexed key, uint256 value);
    event Updated(address indexed owner, uint256 indexed key, uint256 oldValue, uint256 newValue);

    /// @notice Register a new entry. ~55k gas (3 cold SSTOREs + event).
    function register(uint256 key, uint256 value) external {
        if (entries[key].owner != address(0)) revert AlreadyRegistered();
        entries[key] = Entry({owner: msg.sender, value: value, timestamp: block.timestamp});
        entryCount++;
        emit Registered(msg.sender, key, value);
    }

    /// @notice Update an existing entry. ~30k gas (2 warm SSTOREs + event).
    function update(uint256 key, uint256 newValue) external {
        Entry storage entry = entries[key];
        if (entry.owner != msg.sender) revert NotOwner();
        uint256 oldValue = entry.value;
        entry.value = newValue;
        entry.timestamp = block.timestamp;
        emit Updated(msg.sender, key, oldValue, newValue);
    }

    /// @notice Read an entry (view, no gas in calls).
    function lookup(uint256 key) external view returns (address owner, uint256 value, uint256 timestamp) {
        Entry storage entry = entries[key];
        return (entry.owner, entry.value, entry.timestamp);
    }
}
