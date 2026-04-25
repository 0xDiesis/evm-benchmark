// SPDX-License-Identifier: MIT
pragma solidity 0.8.34;

/// @title BenchmarkNFT
/// @notice Minimal ERC-721 for e2e benchmarking. Open mint with sequential
///         token IDs. Different storage pattern from ERC-20: each mint
///         creates a new slot (tokenId -> owner), transfers update existing
///         slots. No metadata, no enumeration.
contract BenchmarkNFT {
    error NotOwner();
    error NotAuthorized();

    string public name = "BenchNFT";
    string public symbol = "BNFT";

    uint256 public totalSupply;
    mapping(uint256 => address) public ownerOf;
    mapping(address => uint256) public balanceOf;
    mapping(uint256 => address) public getApproved;
    mapping(address => mapping(address => bool)) public isApprovedForAll;

    event Transfer(address indexed from, address indexed to, uint256 indexed tokenId);
    event Approval(address indexed owner, address indexed approved, uint256 indexed tokenId);
    event ApprovalForAll(address indexed owner, address indexed operator, bool approved);

    /// @notice Mint the next token to `to`. Open for benchmarking convenience.
    /// @return tokenId The newly minted token ID.
    function mint(address to) external returns (uint256 tokenId) {
        tokenId = totalSupply++;
        ownerOf[tokenId] = to;
        balanceOf[to]++;
        emit Transfer(address(0), to, tokenId);
    }

    /// @notice Transfer token from `from` to `to`.
    function transferFrom(address from, address to, uint256 tokenId) external {
        if (ownerOf[tokenId] != from) revert NotOwner();
        if (msg.sender != from && getApproved[tokenId] != msg.sender && !isApprovedForAll[from][msg.sender]) {
            revert NotAuthorized();
        }
        delete getApproved[tokenId];
        balanceOf[from]--;
        balanceOf[to]++;
        ownerOf[tokenId] = to;
        emit Transfer(from, to, tokenId);
    }

    /// @notice Approve `spender` for a single token.
    function approve(address spender, uint256 tokenId) external {
        address owner = ownerOf[tokenId];
        if (msg.sender != owner && !isApprovedForAll[owner][msg.sender]) revert NotAuthorized();
        getApproved[tokenId] = spender;
        emit Approval(owner, spender, tokenId);
    }

    /// @notice Set operator approval for all tokens.
    function setApprovalForAll(address operator, bool approved) external {
        isApprovedForAll[msg.sender][operator] = approved;
        emit ApprovalForAll(msg.sender, operator, approved);
    }
}
