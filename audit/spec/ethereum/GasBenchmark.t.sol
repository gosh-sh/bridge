// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title GasBenchmark
/// @notice Audit gas table for AckiNackiBridge and production SHPLONK verifiers.
/// @dev Run: `scripts/run_gas_benchmark.sh` or
///      `cd audit/spec/ethereum && forge test --match-contract GasBenchmark -vv`
/// Log format: GAS|<category>|<operation>|<variant>|<gas>

import "forge-std/Test.sol";
import { stdJson } from "forge-std/StdJson.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@src/IPrimaryVerifier.sol";
import "@src/IFallbackVerifier.sol";
import "@src/ILayerHashesMovementVerifier.sol";
import "@src/IBridgeWithdrawalVerifier.sol";
import "@script/ShplonkDeployLib.sol";

import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockPrimaryVerifier.sol";
import "@bridge-test/mocks/MockFallbackVerifier.sol";
import "@bridge-test/mocks/MockLayerHashesMovementVerifier.sol";
import "@bridge-test/mocks/MockBridgeWithdrawalVerifier.sol";
import "@bridge-test/mocks/MockAave.sol";
import "@bridge-test/mocks/MockERC20.sol";

contract GasBenchmark is Test {
    using stdJson for string;

    // Production artefacts (same paths as production E2E tests).
    string internal constant PRIMARY_BIN = "../../../contracts/ethereum/verifiers/PrimaryAggregatorVerifier.bin";
    string internal constant FALLBACK_BIN = "../../../contracts/ethereum/verifiers/FallbackAggregatorVerifier.bin";
    string internal constant LAYER_BIN = "../../../contracts/ethereum/verifiers/LayerHashesAggregatorVerifier.bin";
    string internal constant WITHDRAWAL_BIN =
        "../../../contracts/ethereum/verifiers/BridgeWithdrawalAggregatorVerifier.bin";
    string internal constant PRIMARY_CALLDATA =
        "../../../contracts/ethereum/verifiers/PrimaryAggregatorVerifier_calldata.bin";
    string internal constant FALLBACK_CALLDATA =
        "../../../contracts/ethereum/verifiers/FallbackAggregatorVerifier_calldata.bin";
    string internal constant LAYER_CALLDATA =
        "../../../contracts/ethereum/verifiers/LayerHashesAggregatorVerifier_calldata.bin";
    string internal constant WITHDRAWAL_CALLDATA =
        "../../../contracts/ethereum/verifiers/BridgeWithdrawalAggregatorVerifier_calldata.bin";
    string internal constant BOUND_SCENARIO =
        "../../../crates/bridge-prover-orchestrator/proofs/bound/bound_scenario.json";

    uint256 internal constant ACC = 12; // KZG limbs before C4 PI in calldata

    struct BoundScenario {
        uint256 blockId;
        uint256 bkSetPoseidon;
        uint64 blockSeqNo;
        uint8 numLayers;
        uint256 prevMaxLevelLayerHash;
        uint256[10] layerHashes;
    }

    BoundScenario internal scenario;

    function _logGas(string memory category, string memory op, string memory variant, uint256 gasUsed)
        internal
    {
        emit log(string.concat("GAS|", category, "|", op, "|", variant, "|", vm.toString(gasUsed)));
    }

    function _snap(string memory category, string memory op, string memory variant) internal {
        _logGas(category, op, variant, vm.snapshotGasLastCall(string.concat(category, ".", op, ".", variant)));
    }

    function _binPresent(string memory path) internal view returns (bool) {
        try vm.readFileBinary(path) returns (bytes memory b) {
            return b.length > 0;
        } catch {
            return false;
        }
    }

    function _productionReady() internal view returns (bool) {
        return _binPresent(PRIMARY_BIN) && _binPresent(FALLBACK_BIN) && _binPresent(LAYER_BIN)
            && _binPresent(PRIMARY_CALLDATA) && _binPresent(FALLBACK_CALLDATA)
            && _binPresent(LAYER_CALLDATA);
    }

    function _withdrawalReady() internal view returns (bool) {
        return _binPresent(WITHDRAWAL_BIN) && _binPresent(WITHDRAWAL_CALLDATA);
    }

    function _loadScenarioFromLayerCalldata() internal {
        bytes memory layer = vm.readFileBinary(LAYER_CALLDATA);
        scenario.blockId = _word(layer, 12);
        scenario.bkSetPoseidon = _word(layer, 13);
        scenario.numLayers = uint8(_word(layer, 14));
        for (uint256 i = 0; i < 10; i++) {
            scenario.layerHashes[i] = _word(layer, 15 + i);
        }
        scenario.prevMaxLevelLayerHash = _word(layer, 25);
        bytes memory primary = vm.readFileBinary(PRIMARY_CALLDATA);
        scenario.blockSeqNo = uint64(_word(primary, 14));
    }

    function _loadScenario() internal {
        if (_binPresent(BOUND_SCENARIO)) {
            string memory json = vm.readFile(BOUND_SCENARIO);
            scenario.blockId = vm.parseUint(json.readString(".block_id_decimal"));
            scenario.bkSetPoseidon = vm.parseUint(json.readString(".bk_set_poseidon_decimal"));
            scenario.blockSeqNo = uint64(json.readUint(".block_seq_no"));
            scenario.numLayers = uint8(json.readUint(".num_layers"));
            scenario.prevMaxLevelLayerHash =
                vm.parseUint(json.readString(".prev_max_level_layer_hash_decimal"));
            for (uint256 i = 0; i < 10; i++) {
                string memory key = string.concat(".layer_hash_decimals[", vm.toString(i), "]");
                scenario.layerHashes[i] = vm.parseUint(json.readString(key));
            }
        } else {
            _loadScenarioFromLayerCalldata();
        }
    }

    function _word(bytes memory b, uint256 wordIndex) internal pure returns (uint256 v) {
        uint256 off = wordIndex * 32;
        assembly {
            v := mload(add(add(b, 0x20), off))
        }
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

    // ─── User: deposit ───────────────────────────────────────────────────

    function test_gas_deposit() public {
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        MockERC20 usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        AckiNackiBridge bridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );

        address user = address(0xA11CE);
        uint256 amount = 10 * UsdcTestLib.UNIT;
        usdc.mint(user, amount);
        vm.startPrank(user);
        usdc.approve(address(bridge), amount);
        _snap("user", "erc20_approve", "10usdc");

        bridge.deposit(amount, 0, bytes32(uint256(uint160(user))));
        _snap("user", "deposit", "first_10usdc");

        usdc.mint(user, amount);
        usdc.approve(address(bridge), amount);
        bridge.deposit(amount, 0, bytes32(uint256(uint160(user))));
        _snap("user", "deposit", "warm_10usdc");

        uint256 maxAmt = bridge.MAX_DEPOSIT_AMOUNT();
        usdc.mint(user, maxAmt);
        usdc.approve(address(bridge), maxAmt);
        bridge.deposit(maxAmt, 0, bytes32(uint256(uint160(user))));
        _snap("user", "deposit", "max_100usdc");

        vm.stopPrank();
    }

    // ─── Relayer: verifyBlock (mock crypto — bridge overhead only) ───────

    function test_gas_verifyBlock_mock() public {
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        MockERC20 usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        MockPrimaryVerifier primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallback_ = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layer = new MockLayerHashesMovementVerifier();
        primary.setShouldAccept(true);
        fallback_.setShouldAccept(true);
        layer.setShouldAccept(true);

        uint256 bk = 0xBEEF;
        uint256 prev = 0xA10C;
        AckiNackiBridge bridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primary)),
                IFallbackVerifier(address(fallback_)),
                ILayerHashesMovementVerifier(address(layer)),
                bk,
                prev
            ),
            VerifyBlockConfigLib.disabledWithdraw()
        );

        uint256[10] memory lh;
        lh[0] = 0x111;
        lh[1] = 0x222;
        lh[2] = 0x333;
        uint8 n = 3;

        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"01",
            hex"02",
            0xC10C,
            bk,
            1,
            n,
            lh,
            prev
        );
        _snap("relayer", "verifyBlock", "mock_primary_first");

        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"01",
            hex"02",
            0xC10C2,
            bk,
            2,
            n,
            lh,
            lh[n - 1]
        );
        _snap("relayer", "verifyBlock", "mock_primary_warm");
    }

    // ─── Relayer: verifyBlock + SHPLONK (production) ───────────────────

    function test_gas_verifyBlock_production() public {
        if (!_productionReady()) {
            emit log("SKIP|production|verifyBlock|artefacts_missing");
            return;
        }
        _loadScenario();

        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        MockERC20 usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        ShplonkDeployLib.VerifyBlockVerifiers memory v =
            ShplonkDeployLib.deployVerifyBlockProduction(PRIMARY_BIN, FALLBACK_BIN, LAYER_BIN);

        AckiNackiBridge bridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                v.primary,
                v.fallback_,
                v.layerHashes,
                scenario.bkSetPoseidon,
                scenario.prevMaxLevelLayerHash
            ),
            VerifyBlockConfigLib.disabledWithdraw()
        );

        bytes memory proofPrimary = vm.readFileBinary(PRIMARY_CALLDATA);
        bytes memory proofLayer = vm.readFileBinary(LAYER_CALLDATA);
        bytes memory proofFallback = vm.readFileBinary(FALLBACK_CALLDATA);

        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            proofPrimary,
            proofLayer,
            scenario.blockId,
            scenario.bkSetPoseidon,
            scenario.blockSeqNo,
            scenario.numLayers,
            scenario.layerHashes,
            scenario.prevMaxLevelLayerHash
        );
        _snap("relayer", "verifyBlock", "production_primary");

        v.primary
            .verifyPrimaryAttestation(
                proofPrimary, scenario.blockId, scenario.bkSetPoseidon, scenario.blockSeqNo, 0
            );
        _snap("verifier", "primary_attestation", "isolated");

        v.layerHashes
            .verifyLayerHashesMovement(
                proofLayer,
                scenario.blockId,
                scenario.bkSetPoseidon,
                scenario.numLayers,
                scenario.layerHashes,
                scenario.prevMaxLevelLayerHash
            );
        _snap("verifier", "layer_hashes", "isolated");

        v.fallback_
            .verifyFallbackAttestation(
                proofFallback, scenario.blockId, scenario.bkSetPoseidon, scenario.blockSeqNo, 0
            );
        _snap("verifier", "fallback_attestation", "isolated");
    }

    // ─── Relayer: withdrawByProof (mock crypto) ────────────────────────

    function test_gas_withdrawByProof_mock() public {
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        MockERC20 usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        MockPrimaryVerifier primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallback_ = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layer = new MockLayerHashesMovementVerifier();
        MockBridgeWithdrawalVerifier withdrawal = new MockBridgeWithdrawalVerifier();
        primary.setShouldAccept(true);
        fallback_.setShouldAccept(true);
        layer.setShouldAccept(true);
        withdrawal.setShouldAccept(true);

        uint256 bk = 0xBEEF;
        uint256 prev = 0xA10C;
        uint256 dapp = 0xDEAD0001;
        uint256 acc = 0xACC00001;

        AckiNackiBridge bridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primary)),
                IFallbackVerifier(address(fallback_)),
                ILayerHashesMovementVerifier(address(layer)),
                bk,
                prev
            ),
            VerifyBlockConfigLib.withWithdraw(IBridgeWithdrawalVerifier(address(withdrawal)), dapp, acc)
        );

        UsdcTestLib.depositUsdc(vm, usdc, bridge, address(0xF00D), 50 * UsdcTestLib.UNIT);

        uint256[10] memory lh;
        lh[0] = 0xAAA;
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary, hex"01", hex"02", 1, bk, 1, 1, lh, prev
        );

        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = IBridgeWithdrawalVerifier
            .WithdrawalPublicInputs({
            tokenId: 0,
            amount: 5 * UsdcTestLib.UNIT,
            recipientHi: 0,
            recipientLo: uint256(uint160(address(0x1111))),
            dstChainId: block.chainid,
            senderAccFr: 1,
            dappFr: dapp,
            accFr: acc,
            nullifier: 0x1001,
            finalRoot: lh[0]
        });

        bridge.withdrawByProof(bytes("proof"), pub);
        _snap("relayer", "withdrawByProof", "mock_liquid_only");

        pub.nullifier = 0x1002;
        bridge.withdrawByProof(bytes("proof"), pub);
        _snap("relayer", "withdrawByProof", "mock_warm");
    }

    // ─── Relayer: withdrawByProof + AAVE pull (mock crypto) ────────────

    function test_gas_withdrawByProof_aave_pull_mock() public {
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        MockERC20 usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        MockAUSDC aUsdc = new MockAUSDC();
        MockAavePool pool = new MockAavePool(address(usdc), address(aUsdc));
        MockPrimaryVerifier primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallback_ = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layer = new MockLayerHashesMovementVerifier();
        MockBridgeWithdrawalVerifier withdrawal = new MockBridgeWithdrawalVerifier();
        primary.setShouldAccept(true);
        fallback_.setShouldAccept(true);
        layer.setShouldAccept(true);
        withdrawal.setShouldAccept(true);

        uint256 bk = 0xBEEF;
        uint256 prev = 0xA10C;
        uint256 dapp = 0xDEAD0001;
        uint256 acc = 0xACC00001;

        AckiNackiBridge bridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(pool),
            address(aUsdc),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primary)),
                IFallbackVerifier(address(fallback_)),
                ILayerHashesMovementVerifier(address(layer)),
                bk,
                prev
            ),
            VerifyBlockConfigLib.withWithdraw(IBridgeWithdrawalVerifier(address(withdrawal)), dapp, acc)
        );

        UsdcTestLib.depositUsdc(vm, usdc, bridge, address(0xF00D), 100 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);

        uint256[10] memory lh;
        lh[0] = 0xBBB;
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary, hex"01", hex"02", 2, bk, 1, 1, lh, prev
        );

        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = IBridgeWithdrawalVerifier
            .WithdrawalPublicInputs({
            tokenId: 0,
            amount: 50 * UsdcTestLib.UNIT,
            recipientHi: 0,
            recipientLo: uint256(uint160(address(0x2222))),
            dstChainId: block.chainid,
            senderAccFr: 1,
            dappFr: dapp,
            accFr: acc,
            nullifier: 0x1003,
            finalRoot: lh[0]
        });

        bridge.withdrawByProof(bytes("proof"), pub);
        _snap("relayer", "withdrawByProof", "mock_with_aave_pull");
    }

    // ─── Circuit 4 verifier isolated (production SHPLONK) ──────────────

    function test_gas_withdrawal_verifier_production() public {
        if (!_withdrawalReady()) {
            emit log("SKIP|production|withdrawal_verifier|artefacts_missing");
            return;
        }
        IBridgeWithdrawalVerifier verifier =
            ShplonkDeployLib.deployWithdrawalAdapter(WITHDRAWAL_BIN);
        bytes memory cd = vm.readFileBinary(WITHDRAWAL_CALLDATA);
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = _pubFromWithdrawalCalldata(cd);

        verifier.verifyWithdrawal(cd, pub);
        _snap("verifier", "withdrawal_c4", "isolated");
    }

    // ─── Owner / ops ───────────────────────────────────────────────────

    function test_gas_owner_ops() public {
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        MockERC20 usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        MockAUSDC aUsdc = new MockAUSDC();
        MockAavePool pool = new MockAavePool(address(usdc), address(aUsdc));

        AckiNackiBridge bridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(pool),
            address(aUsdc),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );

        UsdcTestLib.depositUsdc(vm, usdc, bridge, address(0xF00D), 100 * UsdcTestLib.UNIT);

        bridge.pause();
        _snap("owner", "pause", "default");

        bridge.unpause();
        _snap("owner", "unpause", "default");

        bridge.supplyToAave(type(uint256).max);
        _snap("owner", "supplyToAave", "max");

        bridge.withdrawFromAave(5 * UsdcTestLib.UNIT);
        _snap("owner", "withdrawFromAave", "5usdc");

        bridge.setYieldRecipient(address(0xBEEF));
        _snap("owner", "setYieldRecipient", "default");

        bridge.setLiquidReserveBps(500);
        _snap("owner", "setLiquidReserveBps", "5pct");

        bridge.setAaveEnabled(false);
        _snap("owner", "setAaveEnabled", "false");

        bridge.setAaveEnabled(true);
        _snap("owner", "setAaveEnabled", "true");

        // Accrue mock yield via aUSDC balance inflation
        aUsdc.mint(address(bridge), 2 * UsdcTestLib.UNIT);
        bridge.harvestYield(1 * UsdcTestLib.UNIT);
        _snap("owner", "harvestYield", "1usdc");
    }

    function test_gas_owner_emergencyWithdrawAll() public {
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        MockERC20 usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        MockAUSDC aUsdc = new MockAUSDC();
        MockAavePool pool = new MockAavePool(address(usdc), address(aUsdc));

        AckiNackiBridge bridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(pool),
            address(aUsdc),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );

        UsdcTestLib.depositUsdc(vm, usdc, bridge, address(0xF00D), 100 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);

        bridge.emergencyWithdrawAll();
        _snap("owner", "emergencyWithdrawAll", "default");
    }

    // ─── Views (off-chain / relayer pre-checks) ────────────────────────

    function test_gas_views() public {
        if (!_productionReady()) {
            emit log("SKIP|views|production|artefacts_missing");
            return;
        }
        _loadScenario();

        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        MockERC20 usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        ShplonkDeployLib.VerifyBlockVerifiers memory v =
            ShplonkDeployLib.deployVerifyBlockProduction(PRIMARY_BIN, FALLBACK_BIN, LAYER_BIN);

        AckiNackiBridge bridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                v.primary,
                v.fallback_,
                v.layerHashes,
                scenario.bkSetPoseidon,
                scenario.prevMaxLevelLayerHash
            ),
            VerifyBlockConfigLib.disabledWithdraw()
        );

        uint256 g0 = gasleft();
        bridge.expectedPrevAnchor(scenario.numLayers);
        _logGas("view", "expectedPrevAnchor", "cold", g0 - gasleft());

        uint256 g1 = gasleft();
        bridge.isNullifierUsed(0x1234);
        _logGas("view", "isNullifierUsed", "cold", g1 - gasleft());
    }
}
