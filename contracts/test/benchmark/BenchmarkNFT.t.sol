// SPDX-License-Identifier: MIT
pragma solidity ^0.8.34;

import {Test} from "forge-std/Test.sol";
import {BenchmarkNFT} from "../../src/benchmark/BenchmarkNFT.sol";

contract BenchmarkNFTTest is Test {
    BenchmarkNFT nft;
    address alice = address(0xA11CE);
    address bob = address(0xB0B);
    address carol = address(0xCAA01);

    function setUp() public {
        nft = new BenchmarkNFT();
    }

    function test_metadata() public view {
        assertEq(nft.name(), "BenchNFT");
        assertEq(nft.symbol(), "BNFT");
    }

    function test_mint() public {
        uint256 id0 = nft.mint(alice);
        uint256 id1 = nft.mint(alice);

        assertEq(id0, 0);
        assertEq(id1, 1);
        assertEq(nft.totalSupply(), 2);
        assertEq(nft.ownerOf(0), alice);
        assertEq(nft.ownerOf(1), alice);
        assertEq(nft.balanceOf(alice), 2);
    }

    function test_transferFrom_byOwner() public {
        nft.mint(alice);

        vm.prank(alice);
        nft.transferFrom(alice, bob, 0);

        assertEq(nft.ownerOf(0), bob);
        assertEq(nft.balanceOf(alice), 0);
        assertEq(nft.balanceOf(bob), 1);
    }

    function test_transferFrom_bySingleApproved_clearsApproval() public {
        nft.mint(alice);

        vm.prank(alice);
        nft.approve(bob, 0);
        assertEq(nft.getApproved(0), bob);

        vm.prank(bob);
        nft.transferFrom(alice, carol, 0);

        assertEq(nft.ownerOf(0), carol);
        assertEq(nft.getApproved(0), address(0), "approval should be cleared");
    }

    function test_transferFrom_byOperator() public {
        nft.mint(alice);

        vm.prank(alice);
        nft.setApprovalForAll(bob, true);

        vm.prank(bob);
        nft.transferFrom(alice, carol, 0);

        assertEq(nft.ownerOf(0), carol);
    }

    function test_transferFrom_reverts_notOwner() public {
        nft.mint(alice);

        vm.prank(alice);
        vm.expectRevert(BenchmarkNFT.NotOwner.selector);
        nft.transferFrom(bob, carol, 0);
    }

    function test_transferFrom_reverts_notAuthorized() public {
        nft.mint(alice);

        vm.prank(bob);
        vm.expectRevert(BenchmarkNFT.NotAuthorized.selector);
        nft.transferFrom(alice, carol, 0);
    }

    function test_approve_byOwner() public {
        nft.mint(alice);

        vm.prank(alice);
        nft.approve(bob, 0);

        assertEq(nft.getApproved(0), bob);
    }

    function test_approve_byOperator() public {
        nft.mint(alice);

        vm.prank(alice);
        nft.setApprovalForAll(bob, true);

        vm.prank(bob);
        nft.approve(carol, 0);

        assertEq(nft.getApproved(0), carol);
    }

    function test_approve_reverts_notAuthorized() public {
        nft.mint(alice);

        vm.prank(bob);
        vm.expectRevert(BenchmarkNFT.NotAuthorized.selector);
        nft.approve(carol, 0);
    }

    function test_setApprovalForAll() public {
        vm.prank(alice);
        nft.setApprovalForAll(bob, true);
        assertTrue(nft.isApprovedForAll(alice, bob));

        vm.prank(alice);
        nft.setApprovalForAll(bob, false);
        assertFalse(nft.isApprovedForAll(alice, bob));
    }

    function test_mint_gas() public {
        uint256 gasBefore = gasleft();
        nft.mint(alice);
        uint256 gasUsed = gasBefore - gasleft();
        // 3 cold SSTOREs + event
        assertGt(gasUsed, 40_000);
        assertLt(gasUsed, 120_000);
    }
}
