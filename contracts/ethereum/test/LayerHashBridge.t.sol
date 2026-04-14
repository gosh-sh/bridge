// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/LayerHashBridge.sol";
import "../src/LayerHashVerifier.sol";
import "../src/ILayerHashVerifier.sol";
import "../src/LayerHashGroth16Verifier.sol";

/// @dev Mock Groth16 verifier that always succeeds (for unit testing).
contract MockGroth16Verifier is ILayerHashGroth16Verifier {
    bool public shouldRevert;

    function setRevert(bool _shouldRevert) external {
        shouldRevert = _shouldRevert;
    }

    function verifyProof(uint256[8] calldata, uint256[13] calldata) external view override {
        if (shouldRevert) {
            revert("invalid proof");
        }
    }
}

/// @dev Mock verifier that lets us control pass/fail for bridge testing.
contract MockLayerHashVerifier is ILayerHashVerifier {
    bool public nextResult = true;

    function setNextResult(bool _result) external {
        nextResult = _result;
    }

    function verifyLayerHashUpdate(
        bytes calldata,
        uint256,
        uint256,
        uint256[10] calldata,
        uint256
    ) external view override returns (bool) {
        return nextResult;
    }
}

contract LayerHashVerifierTest is Test {
    LayerHashVerifier public verifier;
    MockGroth16Verifier public groth16;

    function setUp() public {
        groth16 = new MockGroth16Verifier();
        verifier = new LayerHashVerifier(address(groth16));
    }

    function testConstructorRejectsZeroAddress() public {
        vm.expectRevert(LayerHashVerifier.InvalidVerifierAddress.selector);
        new LayerHashVerifier(address(0));
    }

    function testVerifyValidProof() public view {
        bytes memory proof = new bytes(256);
        uint256[10] memory layerHashes;
        layerHashes[0] = 42;

        bool result = verifier.verifyLayerHashUpdate(
            proof, 123, 1, layerHashes, 0
        );
        assertTrue(result);
    }

    function testVerifyInvalidProofLength() public view {
        bytes memory proof = new bytes(128);
        uint256[10] memory layerHashes;

        bool result = verifier.verifyLayerHashUpdate(
            proof, 123, 1, layerHashes, 0
        );
        assertFalse(result);
    }

    function testVerifyRevertingGroth16() public {
        groth16.setRevert(true);
        bytes memory proof = new bytes(256);
        uint256[10] memory layerHashes;

        bool result = verifier.verifyLayerHashUpdate(
            proof, 123, 1, layerHashes, 0
        );
        assertFalse(result);
    }
}

contract LayerHashBridgeTest is Test {
    LayerHashBridge public bridge;
    MockLayerHashVerifier public verifier;

    uint256 constant BK_COMMITMENT = 0xdeadbeef;

    event LayerHashesUpdated(
        uint256 indexed updateIndex,
        uint256 numLayers,
        uint256 prevHash,
        uint256 timestamp
    );

    event BkSetCommitmentUpdated(uint256 oldCommitment, uint256 newCommitment);

    function setUp() public {
        verifier = new MockLayerHashVerifier();
        bridge = new LayerHashBridge(address(verifier), BK_COMMITMENT);
    }

    function testInitialState() public view {
        assertEq(bridge.currentBkSetCommitment(), BK_COMMITMENT);
        assertEq(bridge.currentNumLayers(), 0);
        assertEq(bridge.updateCount(), 0);
        assertEq(bridge.owner(), address(this));
    }

    function testFirstUpdate() public {
        bytes memory proof = new bytes(256);
        uint256[10] memory hashes;
        hashes[0] = 111;
        hashes[1] = 222;

        vm.expectEmit(true, false, false, true);
        emit LayerHashesUpdated(1, 2, 0, block.timestamp);

        bridge.updateLayerHashes(proof, 2, hashes, 0);

        assertEq(bridge.currentNumLayers(), 2);
        assertEq(bridge.getLayerHash(0), 111);
        assertEq(bridge.getLayerHash(1), 222);
        assertEq(bridge.updateCount(), 1);
    }

    function testSequentialUpdates() public {
        bytes memory proof = new bytes(256);

        uint256[10] memory hashes1;
        hashes1[0] = 100;
        hashes1[1] = 200;
        bridge.updateLayerHashes(proof, 2, hashes1, 0);

        // Second update: prevHash must match hashes1[1] (top-level of 2 layers)
        uint256[10] memory hashes2;
        hashes2[0] = 300;
        hashes2[1] = 400;
        hashes2[2] = 500;
        bridge.updateLayerHashes(proof, 3, hashes2, 200);

        assertEq(bridge.currentNumLayers(), 3);
        assertEq(bridge.getLayerHash(0), 300);
        assertEq(bridge.getLayerHash(2), 500);
        assertEq(bridge.updateCount(), 2);
    }

    function testRevertOnPrevHashMismatch() public {
        bytes memory proof = new bytes(256);

        uint256[10] memory hashes1;
        hashes1[0] = 100;
        bridge.updateLayerHashes(proof, 1, hashes1, 0);

        uint256[10] memory hashes2;
        vm.expectRevert(
            abi.encodeWithSelector(LayerHashBridge.PrevHashMismatch.selector, 100, 999)
        );
        bridge.updateLayerHashes(proof, 1, hashes2, 999);
    }

    function testRevertOnInvalidProof() public {
        verifier.setNextResult(false);
        bytes memory proof = new bytes(256);
        uint256[10] memory hashes;
        hashes[0] = 1;

        vm.expectRevert(LayerHashBridge.InvalidProof.selector);
        bridge.updateLayerHashes(proof, 1, hashes, 0);
    }

    function testRevertOnZeroNumLayers() public {
        bytes memory proof = new bytes(256);
        uint256[10] memory hashes;

        vm.expectRevert(LayerHashBridge.InvalidNumLayers.selector);
        bridge.updateLayerHashes(proof, 0, hashes, 0);
    }

    function testRevertOnTooManyLayers() public {
        bytes memory proof = new bytes(256);
        uint256[10] memory hashes;

        vm.expectRevert(LayerHashBridge.InvalidNumLayers.selector);
        bridge.updateLayerHashes(proof, 11, hashes, 0);
    }

    function testSetBkSetCommitment() public {
        vm.expectEmit(false, false, false, true);
        emit BkSetCommitmentUpdated(BK_COMMITMENT, 0xcafe);

        bridge.setBkSetCommitment(0xcafe);
        assertEq(bridge.currentBkSetCommitment(), 0xcafe);
    }

    function testSetBkSetCommitmentOnlyOwner() public {
        vm.prank(address(0xbad));
        vm.expectRevert(LayerHashBridge.Unauthorized.selector);
        bridge.setBkSetCommitment(0xcafe);
    }

    function testSetVerifier() public {
        MockLayerHashVerifier newVerifier = new MockLayerHashVerifier();
        bridge.setVerifier(address(newVerifier));
        assertEq(address(bridge.verifier()), address(newVerifier));
    }

    function testSetVerifierRejectsZero() public {
        vm.expectRevert(LayerHashBridge.InvalidVerifier.selector);
        bridge.setVerifier(address(0));
    }

    function testTransferOwnership() public {
        bridge.transferOwnership(address(0x42));
        assertEq(bridge.owner(), address(0x42));

        vm.expectRevert(LayerHashBridge.Unauthorized.selector);
        bridge.setBkSetCommitment(0);
    }

    function testGetAllLayerHashes() public {
        bytes memory proof = new bytes(256);
        uint256[10] memory hashes;
        for (uint256 i = 0; i < 10; i++) {
            hashes[i] = (i + 1) * 100;
        }
        bridge.updateLayerHashes(proof, 10, hashes, 0);

        uint256[10] memory stored = bridge.getAllLayerHashes();
        for (uint256 i = 0; i < 10; i++) {
            assertEq(stored[i], (i + 1) * 100);
        }
    }
}
