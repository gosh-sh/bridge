// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/LayerHashBridge.sol";
import "../src/LayerHashVerifier.sol";
import "../src/LayerHashGroth16VerifierGenerated.sol";

/// @title LayerHashE2ETest
/// @notice End-to-end tests using real Groth16 proofs generated from all 4
///         layer-hashes Halo2 circuit fixtures (via keygen → prove → gnark wrap).
contract LayerHashE2ETest is Test {
    Verifier public groth16Verifier;
    LayerHashVerifier public verifier;
    LayerHashBridge public bridge;

    /// @dev Helper: switch BK set commitment via the timelock (propose + warp + execute).
    function _timelockSetCommitment(uint256 newCommitment) internal {
        bridge.proposeBkSetCommitment(newCommitment);
        vm.warp(block.timestamp + bridge.COMMITMENT_TIMELOCK());
        bridge.executeBkSetCommitment();
    }

    // ─── Fixture 1: L2_H16_prevH0_S1 ────────────────────────────────────
    bytes constant PROOF_L2_H16 =
        hex"0035c2d9238de57ab0f251bf4a19a11c8948bce8254381a50068cefcfede693e1712444ef07afb717e4714bc0540e0598bae4c5e8d6ffab2f422d058df59066c00c2fad4ddaea8cb5353546c3ec2d340e022dd29395a583d801569f82d0f394008d5d84bc55c123180cf090684f3791b763c27c3675aa359cf166d2778c4a3bc28b4ca864fa44aee64d2ad663b83371e52db36ed79c846a766df739dda4ea3cd0608b5cbbc5521f2da6ddb95d39d4565a8b25707b8bebc8dd4c16812df9a10520b0fece8f6cd0fb12726dd60cd651f0a42ec1c8f27f94adb25aa5daddd6413562991941ba9268a6b65541ce2865e5657787d2968819f087006ad5e66ec7eb68f";
    uint256 constant BK_COMMIT_L2_H16 =
        4890018956593449954263527844362646346361583961851737482130404971273334394070;
    uint256 constant NUM_LAYERS_L2_H16 = 2;
    uint256 constant PREV_HASH_L2_H16 = 0; // prevH0 = zero

    function _layerHashes_L2_H16() internal pure returns (uint256[10] memory h) {
        h[0] = 5633846446675785182989510923841880967454911271170633354035817791943430569878;
        h[1] = 13518955109375986971128708807719942030061321662966224929073814870028855931539;
    }

    // ─── Fixture 2: L2_H32_prevH16_S1 ───────────────────────────────────
    bytes constant PROOF_L2_H32 =
        hex"17602e3d9b2f4eb4be8ec467dd4bb34e83afee90645c3628a31887dd45f0c0f7081c2d8c3aa9a1b1b64a9c3ea6349b6d6a0ac7d0585602f8ae7553882e3c59f825f2adf0408350a225c8f213f9df0fabb718d9d6232d3d5564b101eedc2eafc32474de942b8aee4c9afa90237182844cb3339278cb5a2767cd7e62b9f584022f28157295303a5da94df2c72bd0ac5f394b83e2e305c0c358569c6f407395394802e6072374a97337eee71f519947bb691301646a35ddabafa355eb862f707a7211bb6fccb779637697aa0d6408beb031e084c5b0c26abe31e69a4cec22879fc8202027573916f0c9572ea0da864af471f9a801fdc3478665d95154ceea2757f6";
    uint256 constant BK_COMMIT_L2_H32 =
        2082055167831776385016458206467312338642032618530874869326473644260933331674;
    uint256 constant NUM_LAYERS_L2_H32 = 2;
    uint256 constant PREV_HASH_L2_H32 =
        13518955109375986971128708807719942030061321662966224929073814870028855931539;

    function _layerHashes_L2_H32() internal pure returns (uint256[10] memory h) {
        h[0] = 1312328923348738243872003022834861960374527690484305820706684643889907745804;
        h[1] = 11804461175006845179186997411773723047791552327376866686905564838351998984473;
    }

    // ─── Fixture 3: L5_H12288_prevH1024_S11 ─────────────────────────────
    bytes constant PROOF_L5 =
        hex"180030d7bfb3faeb49711111aa2848096ef0cd7be2b034a294e34e5fb25d3d260d1f6754253a0ff93de77ad45f20c2d6bb68b7bc48ab19966d3d28d381167eef15bf99ebfd419adeb536c7d5e509aef59173ba72d9d3868168aec9f86169c8c618174074e8c405163060d21b17a6cd2b1fc8291d3dce9639e0daf3fae54202bf19b402125e03da443d916df21c51fee4e22325809786dc2d1cadb258141b3159143f53f723846a939369db959f58a7315f3be875face7701a023dd7e47cdd0e42bfe34ee928fa54ebdbb15c26a45e5eecba3fcef6b69f8fa8caae2f668ea92fa21189d88d6bb8991d59e27e02b583279d4b861be890755a67114882d1978c6ab";
    uint256 constant BK_COMMIT_L5 =
        17519872562455914146985955765984397348619488008718095210086697635098615010954;
    uint256 constant NUM_LAYERS_L5 = 5;
    uint256 constant PREV_HASH_L5 =
        13445760726362095236325035665495529400200772012935160526004632397196500728579;

    function _layerHashes_L5() internal pure returns (uint256[10] memory h) {
        h[0] = 1492922323469168146334901849713568820171632162458996538698332098599659685086;
        h[1] = 2926997989206114852342701294827434143345810556798176367869631172587212373990;
        h[2] = 9039608223832594836272018281612599083080458017524264905860787608845423736458;
        h[3] = 3566733270025860334048817640583790013488724570652861579923977463497834628464;
        h[4] = 5068027285261346328314792782318171893604681752302082677435947861791461903961;
    }

    // ─── Fixture 4: L6_H45056_prevH0_S11 ────────────────────────────────
    bytes constant PROOF_L6 =
        hex"13a28128710fbf4be5c96c072998f31c3f25372b8807b8f6b51c15244f135914133d61259d3ee616e63272ebb6080075764360a8758cef316aa62846f3a6fa072e4db122dfffac4dd0a45b3b69e25125dc600610cf74aa24d35ca63b227aca2a0eaeb2abd558315f82ae3b3572963a1624e7de2d37aaa94dfe3e44d1c3a3faba0564808845afc7c619fc1166c22106b913d43d35944061565766c61fe92795dd040c79af893b918b62cb01cf3404b96313e51169957e7827927ea0aabb2f17321141793e09c066a7d0de23563624382a15539aff01453cb085840f51eb66fe77143a24c083bbc432569a339403b871412ce98b2ee03ea75002e9f094d32b9f0d";
    uint256 constant BK_COMMIT_L6 =
        2801713364942349461606134384924516198521381347736415043384891515226566912957;
    uint256 constant NUM_LAYERS_L6 = 6;
    uint256 constant PREV_HASH_L6 = 0; // prevH0 = zero

    function _layerHashes_L6() internal pure returns (uint256[10] memory h) {
        h[0] = 12204812291234106848593951802291128510264324787519229674582895287417256616040;
        h[1] = 13457117494253021112276305539030826739528039954919828751535601909604675338231;
        h[2] = 5271404343949219958087631087379968099452869476639129281861476912352133899339;
        h[3] = 14189264921010794710825234398915242134667624346837104810431917694720365583861;
        h[4] = 11361335306241287775113940312874236157097765642735627686872556165707320628815;
        h[5] = 21351112421043795361707590484406142235055380872105669082486592334454493384585;
    }

    function setUp() public {
        groth16Verifier = new Verifier();
        verifier = new LayerHashVerifier(address(groth16Verifier));
        bridge = new LayerHashBridge(address(verifier), BK_COMMIT_L2_H16);
    }

    // =====================================================================
    //  Fixture 1: L2_H16_prevH0_S1 — first block, no previous state
    // =====================================================================

    function testE2E_L2_H16_verify() public view {
        bool valid = verifier.verifyLayerHashUpdate(
            PROOF_L2_H16,
            BK_COMMIT_L2_H16,
            NUM_LAYERS_L2_H16,
            _layerHashes_L2_H16(),
            PREV_HASH_L2_H16
        );
        assertTrue(valid, "L2_H16: real proof should verify");
    }

    function testE2E_L2_H16_bridgeUpdate() public {
        bridge.updateLayerHashes(
            PROOF_L2_H16, NUM_LAYERS_L2_H16, _layerHashes_L2_H16(), PREV_HASH_L2_H16
        );
        assertEq(bridge.updateCount(), 1);
        assertEq(bridge.currentNumLayers(), NUM_LAYERS_L2_H16);

        uint256[10] memory stored = bridge.getAllLayerHashes();
        uint256[10] memory expected = _layerHashes_L2_H16();
        for (uint256 i = 0; i < 10; i++) {
            assertEq(stored[i], expected[i]);
        }
    }

    // =====================================================================
    //  Fixture 2: L2_H32_prevH16_S1 — chained from fixture 1
    //  prev_max_level_layer_hash = fixture 1's top layer hash
    // =====================================================================

    function testE2E_L2_H32_verify() public view {
        bool valid = verifier.verifyLayerHashUpdate(
            PROOF_L2_H32,
            BK_COMMIT_L2_H32,
            NUM_LAYERS_L2_H32,
            _layerHashes_L2_H32(),
            PREV_HASH_L2_H32
        );
        assertTrue(valid, "L2_H32: real proof should verify");
    }

    function testE2E_L2_H32_sequentialBridgeUpdate() public {
        // Step 1: apply L2_H16 (prevH=0)
        bridge.updateLayerHashes(
            PROOF_L2_H16, NUM_LAYERS_L2_H16, _layerHashes_L2_H16(), PREV_HASH_L2_H16
        );
        assertEq(bridge.updateCount(), 1);

        // Step 2: update BK set for next fixture (different commitment)
        _timelockSetCommitment(BK_COMMIT_L2_H32);

        // Step 3: apply L2_H32 (prevH = fixture1's top layer hash)
        bridge.updateLayerHashes(
            PROOF_L2_H32, NUM_LAYERS_L2_H32, _layerHashes_L2_H32(), PREV_HASH_L2_H32
        );
        assertEq(bridge.updateCount(), 2);
        assertEq(bridge.currentNumLayers(), NUM_LAYERS_L2_H32);

        uint256[10] memory stored = bridge.getAllLayerHashes();
        uint256[10] memory expected = _layerHashes_L2_H32();
        for (uint256 i = 0; i < 10; i++) {
            assertEq(stored[i], expected[i]);
        }
    }

    // =====================================================================
    //  Fixture 3: L5_H12288_prevH1024_S11
    // =====================================================================

    function testE2E_L5_verify() public view {
        bool valid = verifier.verifyLayerHashUpdate(
            PROOF_L5, BK_COMMIT_L5, NUM_LAYERS_L5, _layerHashes_L5(), PREV_HASH_L5
        );
        assertTrue(valid, "L5: real proof should verify");
    }

    function testE2E_L5_bridgeUpdate() public {
        _timelockSetCommitment(BK_COMMIT_L5);
        bridge.updateLayerHashes(PROOF_L5, NUM_LAYERS_L5, _layerHashes_L5(), PREV_HASH_L5);
        assertEq(bridge.updateCount(), 1);
        assertEq(bridge.currentNumLayers(), NUM_LAYERS_L5);
    }

    // =====================================================================
    //  Fixture 4: L6_H45056_prevH0_S11
    // =====================================================================

    function testE2E_L6_verify() public view {
        bool valid = verifier.verifyLayerHashUpdate(
            PROOF_L6, BK_COMMIT_L6, NUM_LAYERS_L6, _layerHashes_L6(), PREV_HASH_L6
        );
        assertTrue(valid, "L6: real proof should verify");
    }

    function testE2E_L6_bridgeUpdate() public {
        _timelockSetCommitment(BK_COMMIT_L6);
        bridge.updateLayerHashes(PROOF_L6, NUM_LAYERS_L6, _layerHashes_L6(), PREV_HASH_L6);
        assertEq(bridge.updateCount(), 1);
        assertEq(bridge.currentNumLayers(), NUM_LAYERS_L6);
    }

    // =====================================================================
    //  Negative tests — wrong inputs for each fixture
    // =====================================================================

    function testE2E_L2_H16_wrongCommitment() public view {
        bool valid = verifier.verifyLayerHashUpdate(
            PROOF_L2_H16, 999, NUM_LAYERS_L2_H16, _layerHashes_L2_H16(), PREV_HASH_L2_H16
        );
        assertFalse(valid, "Wrong BK commitment should fail");
    }

    function testE2E_L5_wrongLayers() public view {
        bool valid = verifier.verifyLayerHashUpdate(
            PROOF_L5, BK_COMMIT_L5, 3, _layerHashes_L5(), PREV_HASH_L5
        );
        assertFalse(valid, "Wrong num_layers should fail");
    }

    function testE2E_L6_wrongLayerHash() public view {
        uint256[10] memory hashes = _layerHashes_L6();
        hashes[0] = 42;
        bool valid = verifier.verifyLayerHashUpdate(
            PROOF_L6, BK_COMMIT_L6, NUM_LAYERS_L6, hashes, PREV_HASH_L6
        );
        assertFalse(valid, "Wrong layer hash should fail");
    }

    function testE2E_L2_H32_wrongPrevHash() public view {
        bool valid = verifier.verifyLayerHashUpdate(
            PROOF_L2_H32, BK_COMMIT_L2_H32, NUM_LAYERS_L2_H32, _layerHashes_L2_H32(), 42
        );
        assertFalse(valid, "Wrong prev hash should fail");
    }

    function testE2E_L5_corruptedProof() public view {
        bytes memory bad = new bytes(256);
        for (uint256 i = 0; i < 256; i++) {
            bad[i] = PROOF_L5[i];
        }
        bad[10] = bytes1(uint8(bad[10]) ^ 0xFF);
        bool valid = verifier.verifyLayerHashUpdate(
            bad, BK_COMMIT_L5, NUM_LAYERS_L5, _layerHashes_L5(), PREV_HASH_L5
        );
        assertFalse(valid, "Corrupted proof should fail");
    }

    // =====================================================================
    //  Gas measurement
    // =====================================================================

    function testE2E_gasReport() public {
        _timelockSetCommitment(BK_COMMIT_L5);
        uint256 gasBefore = gasleft();
        bridge.updateLayerHashes(PROOF_L5, NUM_LAYERS_L5, _layerHashes_L5(), PREV_HASH_L5);
        uint256 gasUsed = gasBefore - gasleft();
        emit log_named_uint("Gas used for updateLayerHashes", gasUsed);
    }
}
