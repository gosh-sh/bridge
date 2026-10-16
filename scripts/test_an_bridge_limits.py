#!/usr/bin/env python3
"""Execute deposit-limit regressions in TVM, without a node or proof generation.

Requires SOLD_0_80 and tvm-debugger. A temporary copy of the production source
replaces only Halo2 verification, adds a storage seed, and exposes the bounce
handler body as a test entry point. Reservation, source/anchor checks, voucher
address authentication, minting and upgrade guards execute the actual code.
The production voucher code is used unchanged. No test artefact is deployed.
"""

import json
import base64
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time
import unittest

REPO = Path(__file__).resolve().parents[1]
BRIDGE = "0:" + "1a" * 32
UNIT = 1_000_000
TRANSFER = 50 * UNIT
TVL = 10_000 * UNIT
DEBUGGER = os.environ.get("TVM_DEBUGGER", "tvm-debugger")


def command(args):
    result = subprocess.run(args, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(f"{args[0]} failed: {result.stdout}\n{result.stderr}")
    return result.stdout


class BridgeLimits(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="an-bridge-limits-")
        cls.root = Path(cls.temp.name)
        src = cls.root / "an"
        shutil.copytree(REPO / "contracts/an", src)
        bridge = src / "exchange/eccUSDCBridge.sol"
        source = bridge.read_text()
        verification = "gosh.zkhalo2VerifyWithVK(VK_BLOB, publicInputs, proof)"
        assert source.count(verification) == 1
        source = source.replace(verification, "testVerifyProof()")
        source = source.replace("onBounce(TvmSlice /*body*/) external {",
                                "function testBounce() public {")
        seed = """
    bool _testRejectProof;
    function testRejectProof() public { tvm.accept(); _testRejectProof = true; }
    function testVerifyProof() private view returns (bool) {
        require(!_testRejectProof, 490);
        return true;
    }
    function testSeed(TvmCell voucherCode, uint128 minted, uint128 burned) public {
        tvm.accept();
        _depositVoucherCode = voucherCode;
        _totalMintedBridgeByToken[USDC_ECC_ID] = minted;
        _totalBurnedBridgeByToken[USDC_ECC_ID] = burned;
        _outstandingBridgeUsdc = minted > burned ? minted - burned : 0;
        delete _outstandingByChain;
        _outstandingByChain[1] = _outstandingBridgeUsdc;
        _chainAccountingReady = true;
        _trustedL1Bridge[1][42] = true;
        _acceptedBlockHash[1][123] = true;
        _trustedL1Bridge[2][42] = true;
        _acceptedBlockHash[2][123] = true;
    }
    function testUpgradeV1() public {
        tvm.accept();
        mapping(address => bool) retiredSettledVouchers;
        TvmCell limitsState = abi.encode(_pendingDepositAmounts, _pendingDepositData,
            retiredSettledVouchers, _reservedDeposits, _legacyDepositCounts);
        TvmBuilder empty;
        TvmCell carried = abi.encode(LIMITS_STATE_TAG, empty.toCell(), limitsState);
        TvmCell migration = abi.encode(_ownerPubkey, _usdcWallet, _totalMinted,
            _mintNonce, _mintAccumulatorNonce, _totalMintedBridgeByToken,
            _totalBurnedBridgeByToken, _depositVoucherCode, carried);
        onCodeUpgrade(migration);
    }
    function testUpgradeBareTuple() public {
        tvm.accept();
        TvmCell empty;
        TvmCell migration = abi.encode(_ownerPubkey, _usdcWallet, _totalMinted,
            _mintNonce, _mintAccumulatorNonce, _totalMintedBridgeByToken,
            _totalBurnedBridgeByToken, _depositVoucherCode, empty);
        onCodeUpgrade(migration);
    }
    function testVoucher(uint256 depositId, uint256 chainId) public view returns (address voucher) {
        uint256 h = tvm.hash(abi.encode(depositId, uint256(42), uint256(0), chainId));
        voucher = address.makeAddrStd(0, tvm.hash(abi.encodeStateInit({
            contr: DepositVoucher, varInit: {_depositHash: h}, code: _depositVoucherCode
        })));
    }
"""
        end = source.rfind("}")
        bridge.write_text(source[:end] + seed + source[end:])
        sold = os.environ.get("SOLD_0_80")
        if not sold:
            raise RuntimeError("set SOLD_0_80 to sold 0.80.0")
        command([sold, "--tvm-version", "gosh", "--base-path", str(src),
                 str(bridge), "-o", str(cls.root)])
        cls.abi = cls.root / "eccUSDCBridge.abi.json"
        cls.bridge_code = json.loads(command([DEBUGGER, "state-decode", "-s",
                                             str(cls.root / "eccUSDCBridge.tvc")]))["code"]
        keys_source = (REPO / "crates/ackinacki-bridge/src/test_keys.rs").read_text()
        cls.keys = {name: re.search(rf'PAIR_{name.upper()}: &str = "([0-9a-f]+)"',
                                  keys_source)[1] for name in ("public", "secret")}
        cls.key_file = cls.root / "keys.json"
        cls.key_file.write_text(json.dumps(cls.keys))
        cls.voucher_code = json.loads(command([
            DEBUGGER, "state-decode", "-s",
            str(REPO / "contracts/an/0.81.0_compiled/exchange/DepositVoucher.tvc")
        ]))["code"]

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def setUp(self):
        self.tvc = self.root / "case.tvc"
        shutil.copyfile(self.root / "eccUSDCBridge.tvc", self.tvc)
        self.call("constructor", {"pubkey": "0x" + self.keys["public"],
                                  "usdcWallet": "0:" + "00" * 32})
        self.seed(TVL - TRANSFER)

    def call(self, function, args=None, *, source=None, signed=False, expect=0, ecc=None, shell=1_000_000_000, header=None):
        extra = []
        if source:
            extra = ["--internal", "--message-source", source,
                     "--message-value", str(shell)]
        if not source:
            extra += ["--abi-header", json.dumps(header if header is not None else {"expire": int(time.time()) + 240})]
        if signed:
            extra += ["--sign", str(self.key_file)]
        if ecc is not None:
            extra += ["--message-ecc", json.dumps({"3": str(ecc)})]
        result = json.loads(command([
            DEBUGGER, "run", "-i", str(self.tvc), "-a", str(self.abi),
            "--address", BRIDGE, "--json", "-m", function,
            "-p", json.dumps(args or {})
        ] + extra))
        self.assertEqual(result["exit_code"], expect, (function, result))
        return result

    def seed(self, minted, burned=0):
        self.call("testSeed", {"voucherCode": self.voucher_code,
                               "minted": str(minted), "burned": str(burned)})

    def finalize(self, deposit_id, amount=TRANSFER, expect=0, block=123, chain=1, header=None):
        pi = [deposit_id, 1, amount, 42, chain, 0, 0, 0, 43, 0, block, 0]
        return self.call("finalizeDeposit", {
            "proof": "", "publicInputs": b"".join(
                n.to_bytes(32, "little") for n in pi).hex()
        }, expect=expect, header=header)

    def limits(self, chain=1):
        return self.call("getDepositLimits", {"chainId": str(chain), "l1Bridge": "42"})["response"]

    def confirm(self, deposit_id, amount=TRANSFER, *, source=None, expect=0, chain=1):
        voucher = self.call("testVoucher", {"depositId": str(deposit_id), "chainId": str(chain)})["response"]["voucher"]
        self.call("confirmDeposit", {"depositId": str(deposit_id), "contractAddr": "42",
                  "dappId": "0", "chainId": str(chain), "amount": str(amount), "anAccount": "43"},
                  source=source or voucher, expect=expect)

    def legacy(self, count):
        self.call("setLegacyDepositCount", {"chainId": "1", "l1Bridge": "42",
                  "depositCount": str(count)}, signed=True)

    def test_limits_match_issue_98(self):
        limits = self.limits()
        self.assertEqual(int(limits["maxTransfer"]), TRANSFER)
        self.assertEqual(int(limits["maxTvl"]), TVL)

    def test_excess_historical_burns_do_not_credit_new_deposits(self):
        self.seed(0, TVL * 2)
        self.finalize(0)
        self.confirm(0)
        self.assertEqual(int(self.limits()["outstanding"]), TRANSFER)

    def test_current_excess_burn_is_refused_and_credits_no_future_deposit(self):
        # A burn above bridged inventory is refused outright since 1.10.0
        # (ERR_BURN_EXCEEDS_BRIDGED); an inventory-sized one leaves no credit.
        self.seed(1)
        self.call("initiateWithdrawal", {"dstChainId": "1", "recipient": "11" * 20},
                  source="0:" + "ab" * 32, ecc=TRANSFER, expect=240)
        self.seed(TRANSFER)
        self.call("initiateWithdrawal", {"dstChainId": "1", "recipient": "11" * 20},
                  source="0:" + "ab" * 32, ecc=TRANSFER)
        self.assertEqual(int(self.limits()["outstanding"]), 0)
        self.finalize(0)
        self.confirm(0)
        self.assertEqual(int(self.limits()["outstanding"]), TRANSFER)
        self.call("updateCode", {"newcode": self.bridge_code, "userCell": ""}, signed=True)
        self.assertEqual(int(self.limits()["outstanding"]), TRANSFER)

    def test_previous_tagged_upgrade_restores_pending_and_initializes_net(self):
        self.finalize(0)
        self.call("testUpgradeV1", source=BRIDGE)
        self.call("getDepositLimits", {"chainId": "1", "l1Bridge": "42"}, expect=239)
        self.call("initializeChainAccounting", {"chainIds": ["1"], "outstanding": [str(TVL - TRANSFER)]}, signed=True)
        self.assertEqual(int(self.limits()["reserved"]), TRANSFER)
        self.assertEqual(int(self.limits()["outstanding"]), TVL - TRANSFER)
        self.confirm(0)
        self.assertEqual(int(self.limits()["outstanding"]), TVL)

    def test_networks_have_independent_tvl_and_withdrawal_balances(self):
        self.finalize(0, chain=1)
        self.finalize(0, chain=2)
        self.finalize(1, 1, expect=234, chain=1)
        self.assertEqual(int(self.limits(1)["reserved"]), TRANSFER)
        self.assertEqual(int(self.limits(2)["reserved"]), TRANSFER)
        self.confirm(0, chain=1)
        self.confirm(0, chain=2)
        self.assertEqual(int(self.limits(1)["outstanding"]), TVL)
        self.assertEqual(int(self.limits(2)["outstanding"]), TRANSFER)
        self.call("initiateWithdrawal", {"dstChainId": "1", "recipient": "11" * 20},
                  source="0:" + "ab" * 32, ecc=TRANSFER)
        self.assertEqual(int(self.limits(1)["outstanding"]), TVL - TRANSFER)
        self.assertEqual(int(self.limits(2)["outstanding"]), TRANSFER)

    def test_bounce_and_upgrade_keep_network_reservations_separate(self):
        self.finalize(0, chain=1)
        self.finalize(0, chain=2)
        voucher = self.call("testVoucher", {"depositId": "0", "chainId": "2"})["response"]["voucher"]
        self.call("testBounce", source=voucher)
        self.assertEqual(int(self.limits(1)["reserved"]), TRANSFER)
        self.assertEqual(int(self.limits(2)["reserved"]), 0)
        self.call("updateCode", {"newcode": self.bridge_code, "userCell": ""}, signed=True)
        self.assertEqual(int(self.limits(1)["reserved"]), TRANSFER)
        self.assertEqual(int(self.limits(2)["reserved"]), 0)
        self.confirm(0, chain=1)
        self.assertEqual(int(self.limits(1)["outstanding"]), TVL)
        self.assertEqual(int(self.limits(2)["outstanding"]), 0)

    def test_historical_network_initialization_rejects_bad_or_repeated_snapshots(self):
        self.call("testUpgradeV1", source=BRIDGE)
        for chains, amounts in [(["1", "1"], [str(TVL), "0"]), (["1"], ["0"]), (["1"], [])]:
            self.call("initializeChainAccounting", {"chainIds": chains, "outstanding": amounts}, signed=True, expect=239)
        self.call("initializeChainAccounting", {"chainIds": ["1"], "outstanding": [str(TVL - TRANSFER)]}, expect=209)
        self.call("initializeChainAccounting", {"chainIds": ["1"], "outstanding": [str(TVL - TRANSFER)]}, signed=True)
        self.call("initializeChainAccounting", {"chainIds": ["1"], "outstanding": [str(TVL - TRANSFER)]}, signed=True, expect=239)

    def test_future_and_reordered_deposits_do_not_block_owner_or_other_deposits(self):
        self.seed(0)
        now = int(time.time() * 1000)
        expires = int(time.time()) + 240
        self.finalize(0, header={"time": now + 600_000, "expire": expires})
        self.finalize(0, expect=400, header={"time": now + 600_001, "expire": expires})
        self.call("setPaused", {"paused": True}, signed=True,
                  header={"time": now, "expire": expires})
        self.call("setPaused", {"paused": False}, signed=True,
                  header={"time": now - 1, "expire": expires})
        self.finalize(1, header={"time": now - 2, "expire": expires})
        self.assertEqual(int(self.limits()["reserved"]), 2 * TRANSFER)

    def test_owner_message_replay_is_rejected_even_after_upgrade(self):
        header = {"time": int(time.time() * 1000), "expire": int(time.time()) + 240}
        self.call("setPaused", {"paused": True}, signed=True, header=header)
        self.call("setPaused", {"paused": True}, signed=True, header=header, expect=400)
        self.call("updateCode", {"newcode": self.bridge_code, "userCell": ""}, signed=True)
        self.call("setPaused", {"paused": True}, signed=True, header=header, expect=400)

    def test_exact_deposit_message_replay_is_rejected_but_new_header_is_safe(self):
        self.seed(0)
        header = {"time": int(time.time() * 1000), "expire": int(time.time()) + 240}
        self.finalize(0, header=header)
        self.finalize(0, header=header, expect=400)
        header["time"] += 1
        self.finalize(0, header=header, expect=400)
        self.assertEqual(int(self.limits()["reserved"]), TRANSFER)

    def test_external_expiry_is_bounded_and_expired_messages_are_rejected(self):
        now = int(time.time())
        self.call("setPaused", {"paused": True}, signed=True,
                  header={"time": now * 1000, "expire": now - 1}, expect=402)
        self.call("setPaused", {"paused": True}, signed=True,
                  header={"time": now * 1000, "expire": now + 600}, expect=401)

    def test_unsigned_admin_call_cannot_commit_a_message_record(self):
        header = {"time": int(time.time() * 1000) + 600_000,
                  "expire": int(time.time()) + 240}
        self.call("setPaused", {"paused": True}, header=header, expect=209)
        self.call("setPaused", {"paused": True}, header=header, expect=209)
        self.call("setPaused", {"paused": True}, signed=True)

    def test_settled_proof_with_future_time_cannot_block_emergency_calls(self):
        self.seed(0)
        self.finalize(0)
        self.confirm(0)
        now = int(time.time() * 1000)
        replay = self.finalize(0, header={"time": now + 600_000,
                               "expire": int(time.time()) + 240}, expect=241)
        self.assertEqual(replay["messages"], [])
        self.call("setPaused", {"paused": True}, signed=True,
                  header={"time": now, "expire": int(time.time()) + 240})
        self.call("updateCode", {"newcode": self.bridge_code, "userCell": ""}, signed=True)

    def test_upgrade_from_implicit_timestamp_storage_uses_the_existing_migration_tuple(self):
        fixture = REPO / "scripts/fixtures/bridge_timestamp_rp_1_8_2.tvc.b64"
        self.tvc.write_bytes(base64.b64decode(fixture.read_text()))
        self.call("constructor", {"pubkey": "0x" + self.keys["public"],
                                  "usdcWallet": "0:" + "00" * 32})
        old_header = {"time": int(time.time() * 1000), "expire": int(time.time()) + 240}
        self.call("setPaused", {"paused": True}, signed=True, header=old_header)
        upgrade_header = {"time": int(time.time() * 1000) + 1, "expire": int(time.time()) + 240}
        upgrade_args = {"newcode": self.bridge_code, "userCell": ""}
        self.call("updateCode", upgrade_args, signed=True, header=upgrade_header)
        self.call("setPaused", {"paused": True}, signed=True, header=old_header, expect=403)
        self.call("updateCode", upgrade_args, signed=True, header=upgrade_header, expect=403)
        self.assertEqual(int(self.call("getOwnerPubkey")["response"]["value0"], 0),
                         int(self.keys["public"], 16))
        self.call("setPaused", {"paused": True}, signed=True)
        self.assertEqual(int(self.limits()["outstanding"]), 0)

    def test_paid_proof_replay_is_refused_before_accept_and_books_nothing(self):
        self.seed(0)
        self.finalize(0)
        self.confirm(0)
        replay = self.finalize(0, expect=241)
        self.assertEqual(replay["messages"], [], "a paid proof must not redeploy its voucher")
        self.assertFalse(replay["vm_success"])
        self.assertEqual(int(self.limits()["reserved"]), 0,
                         "a replay must not hold the network's capacity")
        self.finalize(1)
        self.assertEqual(int(self.limits()["reserved"]), TRANSFER)

    def test_recovered_deposit_cannot_be_proven_again(self):
        self.finalize(0)
        self.call("setPaused", {"paused": True}, signed=True)
        voucher = self.call("testVoucher", {"depositId": "0", "chainId": "1"})["response"]["voucher"]
        self.call("recoverDeposit", {"voucher": voucher}, signed=True)
        self.call("setPaused", {"paused": False}, signed=True)
        self.finalize(0, expect=241)

    def test_bare_tuple_upgrade_cannot_lock_the_owner_out(self):
        # The zerostate path upgrades with the nine-field tuple and an empty
        # userCell, so the legacy signed-message floor is read from a layout
        # that holds no timestamp there. Taken at face value that read lands
        # far in the future and refuses every signed call from then on, this
        # contract's own updateCode included.
        self.call("testUpgradeBareTuple")
        self.call("setPaused", {"paused": True}, signed=True)
        self.call("updateCode", {"newcode": self.bridge_code, "userCell": ""}, signed=True)

    def test_pending_callbacks_cannot_overbook_tvl(self):
        self.finalize(0)
        self.finalize(1, expect=234)
        self.assertEqual(int(self.limits()["reserved"]), TRANSFER)

    def test_pending_proof_is_rejected_before_accept_and_verification(self):
        self.finalize(0)
        self.call("testRejectProof")
        duplicate = self.finalize(0, expect=400)
        self.assertEqual(duplicate["messages"], [])
        self.assertFalse(duplicate["vm_success"])
        self.assertEqual(int(self.limits()["reserved"]), TRANSFER)
        self.finalize(1, expect=490)
        self.confirm(0)
        self.assertEqual(int(self.limits()["reserved"]), 0)

    def test_confirmation_moves_reservation_into_outstanding(self):
        self.finalize(0)
        self.confirm(0)
        limits = self.limits()
        self.assertEqual(int(limits["reserved"]), 0)
        self.assertEqual(int(limits["outstanding"]), TVL)
        self.finalize(1, 1, expect=234)

    def test_failed_deploy_releases_reservation_for_retry(self):
        first = self.finalize(0)
        self.call("testBounce", source=first["messages"][0]["destination"])
        self.assertEqual(int(self.limits()["reserved"]), 0)
        self.finalize(0)
        self.assertEqual(int(self.limits()["reserved"]), TRANSFER)

    def test_unrelated_bounce_does_not_release_reservation(self):
        self.finalize(0)
        self.call("testBounce", source="0:" + "ab" * 32)
        self.assertEqual(int(self.limits()["reserved"]), TRANSFER)

    def test_constructor_replay_fails_and_its_bounce_releases_reservation(self):
        deploy = self.finalize(0)["messages"][0]
        voucher = self.root / "deployed-voucher.tvc"
        voucher.write_bytes(base64.b64decode(deploy["state_init"]))
        abi = REPO / "contracts/an/0.81.0_compiled/exchange/DepositVoucher.abi.json"
        args = {"depositId": "0", "contractAddr": "42", "dappId": "0", "chainId": "1",
                "amount": str(TRANSFER), "anAccount": "43"}
        run = [DEBUGGER, "run", "-i", str(voucher), "-a", str(abi), "--json",
               "--address", deploy["destination"], "--internal", "--message-source", BRIDGE,
               "--message-value", "2000000000", "-m", "constructor", "-p", json.dumps(args)]
        self.assertEqual(json.loads(command(run))["exit_code"], 0)
        self.assertEqual(json.loads(command(run))["exit_code"], 51)
        decoded = json.loads(command(["tvm-cli", "-j", "decode", "msg", deploy["boc"], "--abi", str(abi)]))
        self.assertEqual(decoded["Header"]["bounce"], "true")
        self.call("testBounce", source=deploy["destination"])
        self.assertEqual(int(self.limits()["reserved"]), 0)

    def test_callback_authentication_and_reserved_amount(self):
        self.finalize(0)
        self.confirm(0, source="0:" + "ab" * 32, expect=207)
        self.confirm(0, TRANSFER - 1, expect=235)
        self.assertEqual(int(self.limits()["reserved"]), TRANSFER)

    def test_transfer_above_cap_is_rejected(self):
        self.finalize(0, TRANSFER + 1, expect=233)

    def test_legacy_deposit_completes_above_both_caps(self):
        self.legacy(1)
        self.finalize(0, 2 * TRANSFER)
        self.confirm(0, 2 * TRANSFER)
        self.assertEqual(int(self.limits()["outstanding"]), TVL + TRANSFER)
        self.finalize(1, 1, expect=234)
        self.finalize(1, TRANSFER + 1, expect=233)

    def test_legacy_deposit_still_requires_canonical_anchor(self):
        self.legacy(1)
        self.finalize(0, 2 * TRANSFER, block=124, expect=224)
        self.assertEqual(int(self.limits()["reserved"]), 0)

    def test_legacy_cutoff_cannot_be_extended(self):
        self.legacy(1)
        self.call("setLegacyDepositCount", {"chainId": "1", "l1Bridge": "42",
                  "depositCount": "2"}, signed=True, expect=237)

    def test_legacy_cutoff_can_be_lowered_and_disabled(self):
        self.legacy(2)
        self.legacy(1)
        self.finalize(1, TRANSFER + 1, expect=233)
        self.legacy(0)
        self.finalize(0, TRANSFER + 1, expect=233)
        self.call("setLegacyDepositCount", {"chainId": "1", "l1Bridge": "42",
                  "depositCount": "1"}, signed=True, expect=237)

    def test_upgrade_preserves_pending_reservations(self):
        self.finalize(0)
        self.call("updateCode", {"newcode": self.bridge_code, "userCell": ""}, signed=True)
        self.assertEqual(int(self.limits()["reserved"]), TRANSFER)
        self.confirm(0)
        self.assertEqual(int(self.limits()["outstanding"]), TVL)

    def test_pending_deposit_blocks_voucher_rotation_only(self):
        self.finalize(0)
        self.call("updateCode", {"newcode": self.bridge_code, "userCell": self.voucher_code},
                  signed=True, expect=236)

    def test_upgrade_preserves_a_lowered_legacy_cutoff(self):
        self.legacy(2)
        self.legacy(1)
        self.call("updateCode", {"newcode": self.bridge_code, "userCell": ""}, signed=True)
        self.assertEqual(int(self.limits()["legacyDepositCount"], 0), 1)
        self.seed(TVL - TRANSFER)
        self.call("setLegacyDepositCount", {"chainId": "1", "l1Bridge": "42",
                  "depositCount": "2"}, signed=True, expect=237)

    def test_failed_callback_can_be_recovered_without_late_double_mint(self):
        self.finalize(0)
        self.confirm(0, TRANSFER - 1, expect=235)
        voucher = self.call("testVoucher", {"depositId": "0", "chainId": "1"})["response"]["voucher"]
        self.call("recoverDeposit", {"voucher": voucher}, signed=True, expect=238)
        self.call("setPaused", {"paused": True}, signed=True)
        self.call("recoverDeposit", {"voucher": voucher}, expect=209)
        self.call("recoverDeposit", {"voucher": voucher}, signed=True)
        self.assertEqual(int(self.limits()["reserved"]), 0)
        self.confirm(0, expect=235)
        self.assertEqual(int(self.limits()["outstanding"]), TVL)
        self.call("updateCode", {"newcode": self.bridge_code, "userCell": ""}, signed=True)
        self.confirm(0, expect=235)
        self.assertEqual(int(self.limits()["outstanding"]), TVL)

    def test_unreserved_callback_is_rejected_without_minting(self):
        self.confirm(0, 2 * TRANSFER, expect=235)
        self.assertEqual(int(self.limits()["outstanding"]), TVL - TRANSFER)
        self.assertEqual(int(self.limits()["reserved"]), 0)

    def test_over_cap_withdrawal_commits_an_explicit_ecc_refund(self):
        sender = "0:" + "ab" * 32
        result = self.call("initiateWithdrawal", {"dstChainId": "1", "recipient": "11" * 20},
                           source=sender, ecc=TRANSFER + 1, expect=233)
        self.assertTrue(result["vm_success"], "refund actions must survive the rejection")
        self.assertEqual(len(result["messages"]), 1)
        self.assertEqual(result["messages"][0]["destination"], sender)
        decoded = json.loads(command(["tvm-cli", "-j", "decode", "msg", result["messages"][0]["boc"],
                                      "--abi", str(self.abi)]))
        self.assertEqual(int(decoded["Header"]["value"]["other"]["3"]), TRANSFER + 1)
        self.assertEqual(decoded["Header"]["bounce"], "false")
        self.assertEqual(int(decoded["Header"]["value"]["value"]), 100_000_000)

    def test_refund_shell_is_the_exact_minimum_of_incoming_and_cap(self):
        for incoming in (1, 50_000_000, 100_000_000, 100_000_001, 1_000_000_000):
            with self.subTest(incoming=incoming):
                result = self.call("initiateWithdrawal", {"dstChainId": "1", "recipient": "11" * 20},
                                   source="0:" + "ab" * 32, ecc=TRANSFER + 1, shell=incoming, expect=233)
                self.assertTrue(result["vm_success"])
                decoded = json.loads(command(["tvm-cli", "-j", "decode", "msg", result["messages"][0]["boc"],
                                              "--abi", str(self.abi)]))
                self.assertEqual(int(decoded["Header"]["value"]["other"]["3"]), TRANSFER + 1)
                self.assertEqual(int(decoded["Header"]["value"]["value"]), min(incoming, 100_000_000))


if __name__ == "__main__":
    unittest.main()
