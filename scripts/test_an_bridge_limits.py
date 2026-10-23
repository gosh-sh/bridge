#!/usr/bin/env python3
"""Run bridge limit regressions in TVM without a node or proof generation.

Set SOLD_0_80 to sold 0.80.0. Halo2 verification is stubbed in a temporary
source copy. Directed events are omitted because tvm-debugger tries to decode
them as function return values. Test hooks seed balances and derive voucher
addresses. The accounting, sender checks, refunds and upgrade code execute
unchanged. The production voucher is executed unchanged for its replay test.
"""

import base64
import json
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
SENDER = "0:" + "ab" * 32
TRANSFER = 50_000_000
TVL = 10_000_000_000
DEBUGGER = os.environ.get("TVM_DEBUGGER", "tvm-debugger")


def command(args):
    result = subprocess.run(args, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(f"{args[0]}: {result.stdout}\n{result.stderr}")
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
        source = re.sub(r"emit (?:DepositFinalized|WithdrawalInitiated|UsdcMinted)\{dest: addrExtern\}\(.*?\);",
                        "", source, flags=re.S)
        hooks = """
    bool _testRejectProof;
    function testRejectProof() public { tvm.accept(); _testRejectProof = true; }
    function testVerifyProof() private view returns (bool) { return !_testRejectProof; }
    function testSeed(TvmCell voucherCode, uint128 first, uint128 second) public {
        tvm.accept();
        _depositVoucherCode = voucherCode;
        _totalMintedBridgeByToken[USDC_ECC_ID] = first + second;
        _totalBurnedBridgeByToken[USDC_ECC_ID] = 0;
        _outstandingByChain[1] = first;
        _outstandingByChain[2] = second;
        _trustedL1Bridge[1][42] = true;
        _trustedL1Bridge[2][42] = true;
        _acceptedBlockHash[1][123] = true;
        _acceptedBlockHash[2][123] = true;
    }
    function testVoucher(uint256 depositId, uint256 chainId) public view returns (address voucher) {
        uint256 h = tvm.hash(abi.encode(depositId, uint256(42), uint256(0), chainId));
        voucher = address.makeAddrStd(0, tvm.hash(abi.encodeStateInit({
            contr: DepositVoucher, varInit: {_depositHash: h}, code: _depositVoucherCode
        })));
    }
"""
        end = source.rfind("}")
        bridge.write_text(source[:end] + hooks + source[end:])
        sold = os.environ["SOLD_0_80"]
        command([sold, "--tvm-version", "gosh", "--base-path", str(src),
                 str(bridge), "-o", str(cls.root)])
        cls.abi = cls.root / "eccUSDCBridge.abi.json"
        cls.bridge_code = json.loads(command([
            DEBUGGER, "state-decode", "-s", str(cls.root / "eccUSDCBridge.tvc")
        ]))["code"]
        keys = (REPO / "crates/ackinacki-bridge/src/test_keys.rs").read_text()
        cls.keys = {n: re.search(rf'PAIR_{n.upper()}: &str = "([0-9a-f]+)"', keys)[1]
                    for n in ("public", "secret")}
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

    def call(self, function, args=None, *, source=None, signed=False, expect=0,
             ecc=None, shell=1_000_000_000, header=None):
        extra = []
        if source:
            extra += ["--internal", "--message-source", source, "--message-value", str(shell)]
        else:
            extra += ["--abi-header", json.dumps(header or {"expire": int(time.time()) + 240})]
        if signed:
            extra += ["--sign", str(self.key_file)]
        if ecc is not None:
            extra += ["--message-ecc", json.dumps({"3": str(ecc)})]
        result = json.loads(command([
            DEBUGGER, "run", "-i", str(self.tvc), "-a", str(self.abi), "--address", BRIDGE,
            "--json", "--decode-out-messages", "-m", function, "-p", json.dumps(args or {})
        ] + extra))
        self.assertEqual(result["exit_code"], expect,
                         (function, result["exit_code"], result.get("vm_success")))
        return result

    def seed(self, first, second=0):
        self.call("testSeed", {"voucherCode": self.voucher_code,
                               "first": str(first), "second": str(second)})

    def limits(self, chain=1):
        return self.call("getDepositLimits", {"chainId": str(chain)})["response"]

    def finalize(self, deposit=0, amount=TRANSFER, chain=1, expect=0, header=None):
        pi = [deposit, 1, amount, 42, chain, 0, 0, 0, 43, 0, 123, 0]
        return self.call("finalizeDeposit", {"proof": "", "publicInputs": b"".join(
            n.to_bytes(32, "little") for n in pi).hex()}, expect=expect, header=header)

    def confirm(self, deposit=0, amount=TRANSFER, chain=1, source=None, expect=0):
        voucher = self.call("testVoucher", {"depositId": str(deposit), "chainId": str(chain)})[
            "response"]["voucher"]
        return self.call("confirmDeposit", {"depositId": str(deposit), "contractAddr": "42",
                         "dappId": "0", "chainId": str(chain), "amount": str(amount),
                         "anAccount": "43"}, source=source or voucher, expect=expect)

    def withdraw(self, amount, chain=1, shell=1_000_000_000):
        return self.call("initiateWithdrawal", {"dstChainId": str(chain), "recipient": "11" * 20},
                         source=SENDER, ecc=amount, shell=shell)

    def refund(self, result, amount, shell, reason):
        self.assertTrue(result["vm_success"])
        external = [m for m in result["messages"] if m["type"] == "external"]
        internal = [m for m in result["messages"] if m["type"] != "external"]
        self.assertEqual(len(external), 1, result["messages"])
        self.assertEqual(len(internal), 1, result["messages"])
        event = json.loads(command(["tvm-cli", "-j", "decode", "msg", external[0]["boc"],
                                    "--abi", str(self.abi)]))["BodyCall"]["WithdrawalRefused"]
        self.assertEqual(int(event["reason"]), reason)
        self.assertEqual(int(event["amount"]), min(amount, (1 << 128) - 1))
        self.assertEqual(event["sender"], SENDER)
        msg = internal[0]
        self.assertEqual(msg["destination"], SENDER)
        decoded = json.loads(command(["tvm-cli", "-j", "decode", "msg", msg["boc"],
                                      "--abi", str(self.abi)]))["Header"]
        self.assertEqual(int(decoded["value"]["other"]["3"]), amount)
        self.assertEqual(int(decoded["value"]["value"]), min(shell, 100_000_000))
        self.assertEqual(decoded["bounce"], "false")

    def test_issue_98_values(self):
        limits = self.limits()
        self.assertEqual(int(limits["maxTransfer"]), TRANSFER)
        self.assertEqual(int(limits["maxTvl"]), TVL)

    def test_limits_refuse_before_proof_verification(self):
        self.call("testRejectProof")
        self.finalize(amount=TRANSFER + 1, expect=234)
        self.seed(TVL)
        self.finalize(amount=1, expect=235)

    def test_confirmation_enforces_cap_and_sender(self):
        self.confirm(source=SENDER, expect=207)
        self.confirm(amount=TRANSFER + 1, expect=234)
        self.confirm()
        self.assertEqual(int(self.limits()["outstanding"]), TVL)
        self.confirm(deposit=1, amount=1, expect=235)
        self.assertEqual(int(self.limits()["outstanding"]), TVL)

    def test_each_network_has_its_own_balance(self):
        self.seed(TVL, TVL)
        self.withdraw(TRANSFER, chain=1)
        self.assertEqual(int(self.limits(1)["outstanding"]), TVL - TRANSFER)
        self.assertEqual(int(self.limits(2)["outstanding"]), TVL)
        self.finalize(chain=1)
        self.confirm(chain=1)
        self.assertEqual(int(self.limits(1)["outstanding"]), TVL)

    def test_another_networks_inventory_cannot_cover_a_burn(self):
        self.seed(0, TRANSFER)
        self.refund(self.withdraw(TRANSFER, chain=1), TRANSFER, 1_000_000_000, 236)
        self.assertEqual(int(self.limits(1)["outstanding"]), 0)
        self.assertEqual(int(self.limits(2)["outstanding"]), TRANSFER)

    def test_owner_recovery_credits_only_the_source_chain_and_enforces_limits(self):
        self.seed(0, TVL)
        args = {"recipient": SENDER, "value": str(TRANSFER), "nonce": "1", "chainId": "2"}
        self.call("mintAndSendBridged", args, signed=True, expect=235)
        args.update(chainId="1", value=str(TRANSFER + 1))
        self.call("mintAndSendBridged", args, signed=True, expect=234)
        args["value"] = str(TRANSFER)
        self.call("mintAndSendBridged", args, signed=True)
        self.assertEqual(int(self.limits(1)["outstanding"]), TRANSFER)
        self.assertEqual(int(self.limits(2)["outstanding"]), TVL)
        self.withdraw(TRANSFER, chain=1)
        self.assertEqual(int(self.limits(1)["outstanding"]), 0)
        args["nonce"] = "2"
        self.call("mintAndSendBridged", args, signed=True)
        self.assertEqual(int(self.limits(1)["outstanding"]), TRANSFER)

    def test_refund_preserves_large_ecc_amount_and_limits_shell(self):
        for amount in (TRANSFER + 1, (1 << 64) + 1, (1 << 128) + 1):
            for incoming in (1, 50_000_000, 100_000_000, 1_000_000_000):
                with self.subTest(amount=amount, incoming=incoming):
                    self.refund(self.withdraw(amount, shell=incoming), amount, incoming, 234)
                    self.assertEqual(int(self.limits()["outstanding"]), TVL - TRANSFER)

    def test_finalization_does_not_save_an_external_timestamp(self):
        self.seed(0)
        now = int(time.time() * 1000)
        self.finalize(header={"time": now + 600_000, "expire": int(time.time()) + 240})
        self.assertEqual(int(self.limits()["outstanding"]), 0)
        self.call("setPaused", {"paused": True}, signed=True,
                  header={"time": now, "expire": int(time.time()) + 240})

    def test_same_message_is_rejected_but_another_deposit_is_allowed(self):
        self.seed(0)
        header = {"time": int(time.time() * 1000), "expire": int(time.time()) + 240}
        self.finalize(header=header)
        self.finalize(header=header, expect=400)
        self.finalize(deposit=1, header=header)

    def test_replay_protection_survives_upgrade(self):
        header = {"time": int(time.time() * 1000), "expire": int(time.time()) + 240}
        self.call("setPaused", {"paused": True}, signed=True, header=header)
        self.call("updateCode", {"newcode": self.bridge_code, "userCell": ""}, signed=True)
        self.call("setPaused", {"paused": True}, signed=True, header=header, expect=400)

    def test_expiry_window_is_bounded(self):
        now = int(time.time())
        self.finalize(header={"expire": now + 600}, expect=401)
        self.finalize(header={"expire": now - 1}, expect=402)

    def test_upgrade_preserves_both_networks(self):
        self.seed(TRANSFER, 2 * TRANSFER)
        self.call("updateCode", {"newcode": self.bridge_code, "userCell": ""}, signed=True)
        self.assertEqual(int(self.limits(1)["outstanding"]), TRANSFER)
        self.assertEqual(int(self.limits(2)["outstanding"]), 2 * TRANSFER)

    def test_voucher_constructor_replay_cannot_send_a_second_confirmation(self):
        deploy = self.finalize()["messages"][0]
        self.assertEqual(int(self.limits()["outstanding"]), TVL - TRANSFER)
        voucher = self.root / "voucher-case.tvc"
        voucher.write_bytes(base64.b64decode(deploy["state_init"]))
        abi = REPO / "contracts/an/0.81.0_compiled/exchange/DepositVoucher.abi.json"
        params = {"depositId": "0", "contractAddr": "42", "dappId": "0", "chainId": "1",
                  "amount": str(TRANSFER), "anAccount": "43"}
        call = [DEBUGGER, "run", "-i", str(voucher), "-a", str(abi), "--json",
                "--address", deploy["destination"], "--internal", "--message-source", BRIDGE,
                "--message-value", "2000000000", "-m", "constructor", "-p", json.dumps(params)]
        first = json.loads(command(call))
        self.assertEqual(first["exit_code"], 0)
        self.assertEqual(len(first["messages"]), 1)
        self.confirm()
        second = json.loads(command(call))
        self.assertEqual(second["exit_code"], 51)
        self.assertEqual(second["messages"], [])
        self.assertEqual(int(self.limits()["outstanding"]), TVL)


if __name__ == "__main__":
    unittest.main()
