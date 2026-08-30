// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";
import "../script/ShplonkDeployLib.sol";

import "./helpers/VerifyBlockConfigLib.sol";
import "./helpers/UsdcTestLib.sol";
import "./helpers/Bn254FrLib.sol";
import "./mocks/MockPrimaryVerifier.sol";
import "./mocks/MockFallbackVerifier.sol";
import "./mocks/MockLayerHashesMovementVerifier.sol";
import "./mocks/MockERC20.sol";

/// @title AckiNackiBridgeEthFieldCongruenceTest
/// @notice Stage II PDF ETH-1 / ETH-2. Halo2 Yul reduces every instance
///         `mod f_q` (`BN254_R`); R15 adapters compare the *raw* word; the
///         bridge keys `_nullifiers` / layer windows by that raw word.
///         `x` and `x + k·R` are one field element and two mapping keys.
///
/// Invariant (WD-7, extended): a nullifier congruence class pays at most
/// once. Invariant (verifyBlock): a layer-hash congruence class occupies at
/// most one window slot, and the stored value is the canonical Fr.
contract AckiNackiBridgeEthFieldCongruenceTest is Test {
    uint256 internal constant BN254_R =
        0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001;

    uint256 internal constant ACC = 12;
    uint256 internal constant WD_NULLIFIER_WORD = ACC + 8;

    string internal constant WD_BIN = "verifiers/BridgeWithdrawalAggregatorVerifier.bin";
    string internal constant WD_CD = "verifiers/BridgeWithdrawalAggregatorVerifier_calldata.bin";

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV = 0xA10C;

    address internal funder = address(0xF00D);

    function _word(bytes memory b, uint256 wordIndex) internal pure returns (uint256 v) {
        uint256 off = wordIndex * 32;
        require(b.length >= off + 32, "short word");
        assembly {
            v := mload(add(add(b, 0x20), off))
        }
    }

    function _setWord(bytes memory b, uint256 wordIndex, uint256 v) internal pure {
        uint256 off = wordIndex * 32;
        require(b.length >= off + 32, "short word");
        assembly {
            mstore(add(add(b, 0x20), off), v)
        }
    }

    function _addR(bytes memory b, uint256 wordIndex) internal pure {
        _setWord(b, wordIndex, _word(b, wordIndex) + BN254_R);
    }

    function _binPresent(string memory path) internal view returns (bool) {
        try vm.readFileBinary(path) returns (bytes memory b) {
            return b.length > 0;
        } catch {
            return false;
        }
    }

    function _requireWithdrawalArtefacts() internal view {
        require(_binPresent(WD_BIN) && _binPresent(WD_CD), "missing Circuit 4 verifier artefacts");
    }

    function _pubFromWithdrawalCalldata(bytes memory cd)
        internal
        pure
        returns (IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub)
    {
        pub.tokenId = _word(cd, ACC + 0);
        pub.amount = _word(cd, ACC + 1);
        pub.recipientHi = _word(cd, ACC + 2);
        pub.recipientLo = _word(cd, ACC + 3);
        pub.dstChainId = _word(cd, ACC + 4);
        pub.senderAccFr = _word(cd, ACC + 5);
        pub.dappFr = _word(cd, ACC + 6);
        pub.accFr = _word(cd, ACC + 7);
        pub.nullifier = _word(cd, ACC + 8);
        pub.finalRoot = _word(cd, ACC + 9);
    }

    function _reconstruct(uint256 hi, uint256 lo) internal pure returns (address) {
        return address(uint160((hi << 80) | lo));
    }

    // ─────────────────────────────────────────────────────────────────────
    // ETH-1 — crypto fact: Yul still accepts nullifier + R
    // ─────────────────────────────────────────────────────────────────────

    function test_eth1_productionYul_acceptsNullifierPlusR() public {
        _requireWithdrawalArtefacts();
        IBridgeWithdrawalVerifier verifier = ShplonkDeployLib.deployWithdrawalAdapter(WD_BIN);
        bytes memory cd = vm.readFileBinary(WD_CD);
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = _pubFromWithdrawalCalldata(cd);

        assertTrue(verifier.verifyWithdrawal(cd, pub), "canonical Circuit 4 calldata must verify");

        uint256 n = pub.nullifier;
        _addR(cd, WD_NULLIFIER_WORD);
        pub.nullifier = n + BN254_R;

        assertTrue(
            verifier.verifyWithdrawal(cd, pub),
            "ETH-1: Yul reduces instances mod f_q, so N+R must still verify"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // ETH-1 — WD-7: congruence class pays at most once (production path)
    // ─────────────────────────────────────────────────────────────────────

    function test_eth1_withdrawByProof_nullifierPlusR_doesNotPayTwice() public {
        _requireWithdrawalArtefacts();
        bytes memory cd = vm.readFileBinary(WD_CD);
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = _pubFromWithdrawalCalldata(cd);

        vm.chainId(pub.dstChainId);

        IBridgeWithdrawalVerifier verifier = ShplonkDeployLib.deployWithdrawalAdapter(WD_BIN);
        AckiNackiBridge bridge = _deployWithdrawBridge(verifier, pub);

        UsdcTestLib.depositUsdc(
            vm, MockERC20(address(bridge.usdc())), bridge, funder, 3 * pub.amount
        );
        _seedAnchor(bridge, pub.finalRoot);

        address recipient = _reconstruct(pub.recipientHi, pub.recipientLo);
        uint256 treasuryBefore = bridge.treasuryBalance();

        assertTrue(bridge.withdrawByProof(cd, pub), "honest withdraw");
        assertTrue(bridge.isNullifierUsed(pub.nullifier));
        assertEq(MockERC20(address(bridge.usdc())).balanceOf(recipient), pub.amount);
        assertEq(bridge.treasuryBalance(), treasuryBefore - pub.amount);

        uint256 n = pub.nullifier;
        _addR(cd, WD_NULLIFIER_WORD);
        pub.nullifier = n + BN254_R;

        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.FieldElementOutOfRange.selector, pub.nullifier)
        );
        bridge.withdrawByProof(cd, pub);

        assertFalse(bridge.isNullifierUsed(n + BN254_R), "congruent key must stay unused");
        assertEq(
            MockERC20(address(bridge.usdc())).balanceOf(recipient), pub.amount, "no second payout"
        );
        assertEq(bridge.treasuryBalance(), treasuryBefore - pub.amount);
    }

    // ─────────────────────────────────────────────────────────────────────
    // ETH-1 — same invariant on a Yul-model mock (no .bin required)
    // ─────────────────────────────────────────────────────────────────────

    function test_eth1_yulModelMock_nullifierPlusR_doesNotPayTwice() public {
        YulModelWithdrawalVerifier mock = new YulModelWithdrawalVerifier();
        uint256 n = Bn254FrLib.toFr(uint256(keccak256("eth1-nul")));
        uint256 amount = 1 * UsdcTestLib.UNIT;
        uint256 finalRoot = Bn254FrLib.toFr(uint256(keccak256("eth1-anchor")));

        AckiNackiBridge bridge = _deployWithdrawBridge(mock, _mockPub(amount, n, finalRoot));
        UsdcTestLib.depositUsdc(vm, MockERC20(address(bridge.usdc())), bridge, funder, 3 * amount);
        _seedAnchor(bridge, finalRoot);

        bytes memory proofN = mock.packProof(n);
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = _mockPub(amount, n, finalRoot);
        assertTrue(bridge.withdrawByProof(proofN, pub));

        bytes memory proofNR = mock.packProof(n + BN254_R);
        pub.nullifier = n + BN254_R;
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.FieldElementOutOfRange.selector, pub.nullifier)
        );
        bridge.withdrawByProof(proofNR, pub);
    }

    // ─────────────────────────────────────────────────────────────────────
    // ETH-2 — unreduced last-layer hash / prevMax cannot enter the window.
    // Circuit 2 committed .bin/.calldata are desynced (ETH-6), so pairing
    // is not exercised here. The Yul source still does mod(calldataload, f_q)
    // on every instance; ETH-1 production pairing confirms that family
    // accepts x+R. The contract gate is what this test locks.
    // ─────────────────────────────────────────────────────────────────────

    function test_eth2_verifyBlock_layerHashPlusR_rejected() public {
        MockLayerHashesMovementVerifier lh = new MockLayerHashesMovementVerifier();
        lh.setShouldAccept(true);
        AckiNackiBridge bridge = _deployVerifyBlockBridge(lh, BK_SET, GENESIS_PREV);

        uint8 numLayers = 3;
        uint256[10] memory hashes;
        hashes[0] = 1;
        hashes[1] = 2;
        hashes[2] = 3;
        uint256 honest = hashes[2];
        hashes[2] = honest + BN254_R;

        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.FieldElementOutOfRange.selector, hashes[2])
        );
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"00",
            hex"00",
            0xC10C40001,
            BK_SET,
            1,
            numLayers,
            hashes,
            GENESIS_PREV
        );

        assertFalse(bridge.isKnownAnchor(honest), "honest H must not be displaced");
        assertFalse(bridge.isKnownAnchor(honest + BN254_R), "congruent H+R must not land");
        assertEq(bridge.expectedPrevAnchor(numLayers), GENESIS_PREV, "genesis head unchanged");
    }

    function test_eth2_verifyBlock_blockIdPlusR_rejected() public {
        MockLayerHashesMovementVerifier lh = new MockLayerHashesMovementVerifier();
        lh.setShouldAccept(true);
        AckiNackiBridge bridge = _deployVerifyBlockBridge(lh, BK_SET, GENESIS_PREV);

        uint256[10] memory hashes;
        hashes[0] = 1;
        hashes[1] = 2;
        hashes[2] = 3;
        uint256 blockId = 0xC10C40001;

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.FieldElementOutOfRange.selector, blockId + BN254_R
            )
        );
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"00",
            hex"00",
            blockId + BN254_R,
            BK_SET,
            1,
            3,
            hashes,
            GENESIS_PREV
        );
        assertEq(bridge.storedLastSeenBlockSeqNo(), 0, "cursor must not advance");
    }

    function test_eth2_verifyBlock_prevMaxPlusR_rejected() public {
        MockLayerHashesMovementVerifier lh = new MockLayerHashesMovementVerifier();
        lh.setShouldAccept(true);
        AckiNackiBridge bridge = _deployVerifyBlockBridge(lh, BK_SET, GENESIS_PREV);

        uint256[10] memory hashes;
        hashes[0] = 1;
        hashes[1] = 2;
        hashes[2] = 3;
        uint256 poisonedPrev = GENESIS_PREV + BN254_R;

        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.FieldElementOutOfRange.selector, poisonedPrev)
        );
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"00",
            hex"00",
            0xC10C40001,
            BK_SET,
            1,
            3,
            hashes,
            poisonedPrev
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Helpers
    // ─────────────────────────────────────────────────────────────────────

    function _mockPub(uint256 amount, uint256 nullifier, uint256 finalRoot)
        internal
        view
        returns (IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory)
    {
        address recipient = address(0x1111111111111111111111111111111111111111);
        uint256 a = uint256(uint160(recipient));
        return IBridgeWithdrawalVerifier.WithdrawalPublicInputs({
            tokenId: 0,
            amount: amount,
            recipientHi: a >> 80,
            recipientLo: a & ((1 << 80) - 1),
            dstChainId: block.chainid,
            senderAccFr: Bn254FrLib.toFr(uint256(keccak256("senderAcc"))),
            dappFr: 0xD499F4CEC0FFEE01,
            accFr: 0xAC0F4CEDEADBEEF1,
            nullifier: nullifier,
            finalRoot: finalRoot
        });
    }

    function _deployWithdrawBridge(
        IBridgeWithdrawalVerifier verifier,
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub
    ) internal returns (AckiNackiBridge bridge) {
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        MockERC20 usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        MockPrimaryVerifier primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallback_ = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layer = new MockLayerHashesMovementVerifier();
        primary.setShouldAccept(true);
        fallback_.setShouldAccept(true);
        layer.setShouldAccept(true);

        bridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primary)),
                IFallbackVerifier(address(fallback_)),
                ILayerHashesMovementVerifier(address(layer)),
                BK_SET,
                GENESIS_PREV
            ),
            VerifyBlockConfigLib.withWithdrawShellnet(
                    verifier, pub.dappFr, pub.accFr, 0, 0, pub.tokenId
                )
        );
    }

    function _deployVerifyBlockBridge(
        ILayerHashesMovementVerifier lh,
        uint256 genesisBkSet,
        uint256 genesisPrev
    ) internal returns (AckiNackiBridge bridge) {
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        MockERC20 usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        MockPrimaryVerifier primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallback_ = new MockFallbackVerifier();
        primary.setShouldAccept(true);
        fallback_.setShouldAccept(true);

        bridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primary)),
                IFallbackVerifier(address(fallback_)),
                lh,
                genesisBkSet,
                genesisPrev
            ),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    function _seedAnchor(AckiNackiBridge bridge, uint256 finalRoot) internal {
        uint256[10] memory layers;
        layers[0] = finalRoot;
        layers[1] = Bn254FrLib.toFr(uint256(keccak256("eth1-l2")));
        layers[2] = Bn254FrLib.toFr(uint256(keccak256("eth1-l3")));
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"00",
            hex"00",
            0xC10C40001,
            BK_SET,
            1,
            3,
            layers,
            bridge.expectedPrevAnchor(3)
        );
    }
}

/// @dev Models R15 adapter + Yul: raw instance must equal `pub.nullifier`,
///      SHPLONK pairing is treated as accepting any word (pairing sees `mod f_q`).
contract YulModelWithdrawalVerifier is IBridgeWithdrawalVerifier {
    uint256 internal constant ACC = 12;

    function packProof(uint256 nullifier) external pure returns (bytes memory proof) {
        proof = new bytes((ACC + 10) * 32);
        assembly {
            mstore(add(add(proof, 0x20), mul(add(ACC, 8), 32)), nullifier)
        }
    }

    function verifyWithdrawal(bytes calldata proof, WithdrawalPublicInputs calldata pub)
        external
        pure
        override
        returns (bool)
    {
        if (proof.length < (ACC + 10) * 32) return false;
        uint256 inst = uint256(bytes32(proof[(ACC + 8) * 32:(ACC + 9) * 32]));
        return inst == pub.nullifier;
    }
}
