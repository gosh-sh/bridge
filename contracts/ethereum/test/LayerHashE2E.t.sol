// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/LayerHashBridge.sol";
import "../src/LayerHashVerifier.sol";
import "../src/LayerHashGroth16VerifierGenerated.sol";

/// @title LayerHashE2ETest
/// @notice End-to-end test using a real Groth16 proof generated from a
///         layer-hashes Halo2 circuit fixture (L5_H12288_prevH1024_S11).
contract LayerHashE2ETest is Test {
    Verifier public groth16Verifier;
    LayerHashVerifier public verifier;
    LayerHashBridge public bridge;

    // Real proof from gnark-wrapper (fixture L5_H12288_prevH1024_S11)
    bytes constant PROOF = hex"26dbeb2de5dab628a2d2c7ac4961fa41c9ba3c1b653c1a5c7f0ade8f7ff513cf04acdb138fda93d08e10fb1378da247a136619d99f41b9103345eede965ae117219d64f5a04cbc927fa70fb1d390ce13269954c9d94278c84ebfeeee41d2d9f72e42bf8fa524b0f178fedafd376341fad2a66cd977b34368c6ec4ace2400ccda171e509164382e8f09148e998850af29250e4e1a20c41d8b5f5327bd3914a24521ace40c27336a2b7ac24407b1c095533e7a561e1b1fa0630fad1c7bb38964da0932c3006892d0e295d880becce68a892898642dda4954ab139fda08184954962f2bd06be75bc53c0c36e1951bec352f4279f5e65bd56f695f98676932f87916";

    // Public inputs from the circuit (13 BN254 Fr elements)
    uint256 constant BK_SET_COMMITMENT = 17519872562455914146985955765984397348619488008718095210086697635098615010954;
    uint256 constant NUM_LAYERS = 5;
    uint256 constant PREV_MAX_LEVEL = 13445760726362095236325035665495529400200772012935160526004632397196500728579;

    function _layerHashes() internal pure returns (uint256[10] memory h) {
        h[0] = 1492922323469168146334901849713568820171632162458996538698332098599659685086;
        h[1] = 2926997989206114852342701294827434143345810556798176367869631172587212373990;
        h[2] = 9039608223832594836272018281612599083080458017524264905860787608845423736458;
        h[3] = 3566733270025860334048817640583790013488724570652861579923977463497834628464;
        h[4] = 5068027285261346328314792782318171893604681752302082677435947861791461903961;
        h[5] = 0;
        h[6] = 0;
        h[7] = 0;
        h[8] = 0;
        h[9] = 0;
    }

    function setUp() public {
        groth16Verifier = new Verifier();
        verifier = new LayerHashVerifier(address(groth16Verifier));
        bridge = new LayerHashBridge(address(verifier), BK_SET_COMMITMENT);
    }

    function testE2ERealProofVerification() public view {
        bool valid = verifier.verifyLayerHashUpdate(
            PROOF,
            BK_SET_COMMITMENT,
            NUM_LAYERS,
            _layerHashes(),
            PREV_MAX_LEVEL
        );
        assertTrue(valid, "Real proof should verify");
    }

    function testE2ERealProofBridgeUpdate() public {
        bridge.updateLayerHashes(PROOF, NUM_LAYERS, _layerHashes(), PREV_MAX_LEVEL);

        assertEq(bridge.currentNumLayers(), NUM_LAYERS);
        assertEq(bridge.updateCount(), 1);

        uint256[10] memory stored = bridge.getAllLayerHashes();
        uint256[10] memory expected = _layerHashes();
        for (uint256 i = 0; i < 10; i++) {
            assertEq(stored[i], expected[i], "layer hash mismatch");
        }
    }

    function testE2EWrongBkSetCommitment() public view {
        bool valid = verifier.verifyLayerHashUpdate(
            PROOF,
            999,  // wrong commitment
            NUM_LAYERS,
            _layerHashes(),
            PREV_MAX_LEVEL
        );
        assertFalse(valid, "Wrong BK set commitment should fail");
    }

    function testE2EWrongNumLayers() public view {
        bool valid = verifier.verifyLayerHashUpdate(
            PROOF,
            BK_SET_COMMITMENT,
            3,  // wrong num_layers
            _layerHashes(),
            PREV_MAX_LEVEL
        );
        assertFalse(valid, "Wrong num_layers should fail");
    }

    function testE2EWrongLayerHash() public view {
        uint256[10] memory hashes = _layerHashes();
        hashes[0] = 42;  // corrupt first layer hash
        bool valid = verifier.verifyLayerHashUpdate(
            PROOF,
            BK_SET_COMMITMENT,
            NUM_LAYERS,
            hashes,
            PREV_MAX_LEVEL
        );
        assertFalse(valid, "Wrong layer hash should fail");
    }

    function testE2EWrongPrevHash() public view {
        bool valid = verifier.verifyLayerHashUpdate(
            PROOF,
            BK_SET_COMMITMENT,
            NUM_LAYERS,
            _layerHashes(),
            42  // wrong prev hash
        );
        assertFalse(valid, "Wrong prev hash should fail");
    }

    function testE2ECorruptedProof() public view {
        bytes memory badProof = new bytes(256);
        for (uint256 i = 0; i < 256; i++) {
            badProof[i] = PROOF[i];
        }
        badProof[10] = bytes1(uint8(badProof[10]) ^ 0xFF);

        bool valid = verifier.verifyLayerHashUpdate(
            badProof,
            BK_SET_COMMITMENT,
            NUM_LAYERS,
            _layerHashes(),
            PREV_MAX_LEVEL
        );
        assertFalse(valid, "Corrupted proof should fail");
    }
}
