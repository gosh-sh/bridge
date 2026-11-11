#!/usr/bin/env python3
"""Run bridge limit and deposit-receipt regressions in TVM without a node.

Set SOLD_0_80_1 to sold 0.80.1 and SOLD_0_81 to sold 0.81.0, and TVM_DEBUGGER to a
tvm-debugger that runs KECCAK256 (tvm-sdk fd76a8a9 or later). Every deposit is a real
receipt proof: `finalize` builds a block whose receipt trie holds the deposit
(`scripts/deposit-receipt/vectors.py`) and a test hook anchors its hash. Directed events are
omitted because tvm-debugger tries to decode them as function return values; `WithdrawalRefused` is replaced by a hook that
records its reason, so a refusal's cause is still asserted. Test hooks seed
balances and derive voucher addresses. The bridge registers USDC alone; a test hook lifts that check
for one case so the per-token accounting stays covered, and `test_only_usdc_can_be_registered` runs
with it in place. The accounting, sender checks, refunds and upgrade code execute unchanged. The
production voucher is executed unchanged for its replay test.
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
import sys
import unittest

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO / "scripts/deposit-receipt"))
import vectors  # noqa: E402

BRIDGE = "0:" + "1a" * 32
SENDER = "0:" + "ab" * 32
TRANSFER = 50_000_000
TVL = 10_000_000_000
USDC_TOKEN = 68
SECOND_TOKEN = 85
THIRD_TOKEN = 102
EVM_BRIDGE = 0x3FB082A905A895151224F6D93BD074E816338638
EMITTER = {1: 42, 2: 44}
DEBUGGER = os.environ.get("TVM_DEBUGGER", "tvm-debugger")
ACCEPT_REACHED = 999
GAS_CREDIT = 500_000
TRANSFER_LOG = ((USDC_TOKEN).to_bytes(20, "big"), [bytes.fromhex(
    "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"), b"\0" * 12 + b"\xab" * 20,
    b"\0" * 31 + b"\x2a"], TRANSFER.to_bytes(32, "big"))
SUPPLY_LOG = (b"\x77" * 20, [b"\x55" * 32, b"\0" * 31 + b"\x2a"], b"\0" * 64)


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
        bridge = src / "exchange/eccBridge.sol"
        source = bridge.read_text()
        usdc_only = "require(tokenId == USDC_ECC_ID, ERR_TOKEN_CONFIG);"
        assert source.count(usdc_only) == 1
        source = source.replace(usdc_only, "require(tokenId == USDC_ECC_ID || "
                                "(_testAllowOtherTokens && tokenId > USDC_ECC_ID), ERR_TOKEN_CONFIG);")
        accept = "tvm.accept();\n        _saveMsg();\n        tvm.commit();"
        assert source.count(accept) == 1
        source = source.replace(accept, f"require(!_testStopAtAccept, {ACCEPT_REACHED});\n        " + accept)
        source = re.sub(r"emit WithdrawalRefused\{dest: addrExtern\}\(.*?\);",
                        "_testLastRefusal = reason;", source, flags=re.S)
        source = source.replace(
            "bytes recipient, uint32 tokenId, uint16 reason) private pure {",
            "bytes recipient, uint32 tokenId, uint16 reason) private {")
        source = re.sub(r"emit (?:DepositFinalized|WithdrawalInitiated|UsdcMinted)\{dest: addrExtern\}\(.*?\);",
                        "", source, flags=re.S)
        hooks = """
    function testLegacyUpgrade() public {
        tvm.accept();
        TvmCell userCell;
        TvmCell data = abi.encode(_ownerPubkey, _usdcWallet, _totalMinted, _mintNonce,
            _mintAccumulatorNonce, _totalMintedBridgeByToken, _totalBurnedBridgeByToken,
            _depositVoucherCode, userCell, _outstandingByChain, _messages);
        onCodeUpgrade(data);
    }
    uint16 _testLastRefusal;
    function testLastRefusal() public view returns (uint16 reason) {
        reason = _testLastRefusal;
    }
    bool _testAllowOtherTokens;
    function testAllowOtherTokens() public { tvm.accept(); _testAllowOtherTokens = true; }
    bool _testStopAtAccept;
    function testStopAtAccept() public { tvm.accept(); _testStopAtAccept = true; }
    function testAnchor(uint256 chainId, uint256 blockHash) public {
        tvm.accept();
        _acceptedBlockHash[chainId][blockHash] = true;
    }
    function testReceiptLog(bytes header, uint16 txIndex, bytes[] receiptProof, uint16 logIndex)
        public pure returns (uint256 emitter, uint256 topic0)
    {
        (, uint256 root) = EthTrie.headerRoots(header, gosh.keccak256(header));
        (uint256 key, uint nibbles) = EthTrie.receiptKey(txIndex);
        (TvmSlice r, uint len) = EthTrie.prove(root, key, nibbles, receiptProof);
        r = EthTrie.receiptLog(r, len, logIndex);
        uint8 kind;
        uint n;
        uint hdr;
        (kind, n, hdr, r) = EthTrie._hdr(r);
        (kind, n, hdr, r) = EthTrie._hdr(r);
        (emitter, r) = EthTrie._uint(r, 20);
        (kind, n, hdr, r) = EthTrie._hdr(r);
        (topic0, r) = EthTrie._hash(r);
    }
    function testSeed(TvmCell voucherCode, uint128 first, uint128 second) public {
        tvm.accept();
        _depositVoucherCode = voucherCode;
        _totalMintedBridgeByToken[USDC_ECC_ID] = first + second;
        _totalBurnedBridgeByToken[USDC_ECC_ID] = 0;
        _outstandingByChain[1] = first;
        _outstandingByChain[2] = second;
    }
    function testVoucher(uint256 depositId, uint256 contractAddr, uint256 chainId) public view returns (address voucher) {
        uint256 h = tvm.hash(abi.encode(depositId, contractAddr, uint256(0), chainId));
        voucher = address.makeAddrStd(0, tvm.hash(abi.encodeStateInit({
            contr: DepositVoucher, varInit: {_depositHash: h}, code: _depositVoucherCode
        })));
    }
"""
        end = source.rfind("}")
        bridge.write_text(source[:end] + hooks + source[end:])
        sold = os.environ["SOLD_0_80_1"]
        command([sold, "--tvm-version", "gosh", "--base-path", str(src),
                 str(bridge), "-o", str(cls.root)])
        cls.abi = cls.root / "eccBridge.abi.json"
        cls.bridge_code = json.loads(command([
            DEBUGGER, "state-decode", "-s", str(cls.root / "eccBridge.tvc")
        ]))["code"]
        keys = (REPO / "crates/ackinacki-bridge/src/test_keys.rs").read_text()
        cls.keys = {n: re.search(rf'PAIR_{n.upper()}: &str = "([0-9a-f]+)"', keys)[1]
                    for n in ("public", "secret")}
        cls.key_file = cls.root / "keys.json"
        cls.key_file.write_text(json.dumps(cls.keys))
        voucher_dir = cls.root / "voucher"
        voucher_dir.mkdir()
        command([os.environ.get("SOLD_0_81", "/usr/bin/sold"), "--tvm-version", "gosh",
                 "--base-path", str(REPO / "contracts/an"),
                 str(REPO / "contracts/an/exchange/DepositVoucher.sol"), "-o", str(voucher_dir)])
        cls.voucher_abi = voucher_dir / "DepositVoucher.abi.json"
        cls.voucher_code = json.loads(command([
            DEBUGGER, "state-decode", "-s", str(voucher_dir / "DepositVoucher.tvc")
        ]))["code"]

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def setUp(self):
        self.tvc = self.root / "case.tvc"
        shutil.copyfile(self.root / "eccBridge.tvc", self.tvc)
        self.call("constructor", {"pubkey": "0x" + self.keys["public"],
                                  "usdcWallet": "0:" + "00" * 32})
        self.seed(TVL - TRANSFER)
        self.bind(1)
        self.bind(2)
        for chain in (1, 2):
            self.register(USDC_TOKEN, 3, TRANSFER, TVL, chain=chain)
            self.trust(chain, EMITTER[chain])

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
            extra += ["--message-ecc", json.dumps(ecc if isinstance(ecc, dict) else {"3": str(ecc)})]
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

    def bind(self, chain, bridge=EVM_BRIDGE):
        self.call("setEvmBridge", {"dstChainId": str(chain), "evmBridge": str(bridge)},
                  signed=True)

    def trust(self, chain, l1_bridge, allowed=True, expect=0):
        return self.call("setTrustedL1Bridge", {"chainId": str(chain), "l1Bridge": str(l1_bridge),
                                                "allowed": allowed}, signed=True, expect=expect)

    def trusted(self, chain, l1_bridge):
        return self.call("isTrustedL1Bridge", {"chainId": str(chain), "l1Bridge": str(l1_bridge)})[
            "response"]["value0"]

    def trusted_chain(self, l1_bridge):
        chain = self.call("getTrustedL1BridgeChain", {"l1Bridge": str(l1_bridge)})["response"]["value0"]
        return None if chain is None else int(chain, 0)

    def evm_bridge(self, chain):
        return int(self.call("getEvmBridge", {"dstChainId": str(chain)})["response"]["value0"], 0)

    def limits(self, chain=1):
        return self.call("getDepositLimits", {"chainId": str(chain)})["response"]

    def finalize(self, deposit=0, amount=TRANSFER, chain=1, expect=0, header=None, token=USDC_TOKEN,
                 logs=None, receipt=None, anchor=True, log_index=1, **block):
        """Finalize a deposit from a block of its own: the deposit transaction's receipt carries
        the USDC transfer, the `Deposit` log, and an AAVE supply after it."""
        if receipt is None:
            if logs is None:
                logs = [TRANSFER_LOG, vectors.deposit_log(EMITTER[chain], deposit, amount, 43, token),
                        SUPPLY_LOG]
            receipt = vectors.receipt(logs)
        v = vectors.synthetic(receipt, **block)
        if anchor:
            self.anchor(v, chain)
        return self.submit(v, chain, log_index, expect=expect, header=header)

    def anchor(self, v, chain=1):
        self.call("testAnchor", {"chainId": str(chain), "blockHash": v["blockHash"]})

    def submit(self, v, chain=1, log_index=1, expect=0, header=None):
        return self.call("finalizeDeposit", {
            "chainId": str(chain), "header": v["header"], "txIndex": v["txIndex"],
            "receiptProof": v["receiptProof"], "logIndex": log_index}, expect=expect, header=header)

    def voucher(self, deposit, chain=1):
        return self.call("testVoucher", {"depositId": str(deposit), "contractAddr": str(EMITTER[chain]),
                                         "chainId": str(chain)})["response"]["voucher"]

    def deployed(self, result, deposit, chain=1):
        self.assertEqual([m["destination"] for m in result["messages"]], [self.voucher(deposit, chain)])

    def confirm(self, deposit=0, amount=TRANSFER, chain=1, source=None, expect=0):
        return self.call("confirmDeposit", {"depositId": str(deposit), "contractAddr": str(EMITTER[chain]),
                         "dappId": "0", "chainId": str(chain), "amount": str(amount),
                         "anAccount": "43"}, source=source or self.voucher(deposit, chain), expect=expect)

    def withdraw(self, amount, chain=1, shell=1_000_000_000):
        return self.call("initiateWithdrawal", {"dstChainId": str(chain), "recipient": "11" * 20},
                         source=SENDER, ecc=amount, shell=shell)

    def refund(self, result, amount, shell, reason):
        self.assertTrue(result["vm_success"])
        self.assertEqual(len(result["messages"]), 1)
        self.assertEqual(int(self.call("testLastRefusal")["response"]["reason"]), reason)
        msg = result["messages"][0]
        self.assertEqual(msg["destination"], SENDER)
        decoded = json.loads(command(["tvm-cli", "-j", "decode", "msg", msg["boc"],
                                      "--abi", str(self.abi)]))["Header"]
        self.assertEqual(int(decoded["value"]["other"]["3"]), amount)
        self.assertEqual(int(decoded["value"]["value"]), min(shell, 100_000_000))
        self.assertEqual(decoded["bounce"], "false")

    def register(self, token, token_id, max_transfer, max_tvl, *, chain=1, enabled=True, expect=0):
        if token_id != 3:
            self.call("testAllowOtherTokens")
        return self.set_bridge_token(token, token_id, max_transfer, max_tvl, chain, enabled, expect)

    def set_bridge_token(self, token, token_id, max_transfer, max_tvl, chain=1, enabled=True, expect=0):
        return self.call("setBridgeToken", {
            "chainId": str(chain), "evmToken": str(token), "tokenId": token_id,
            "maxTransfer": str(max_transfer), "maxTvl": str(max_tvl),
            "depositsEnabled": enabled}, signed=True, expect=expect)

    def token_limits(self, token_id=4, chain=1):
        return self.call("getTokenWithdrawalConfig", {
            "chainId": str(chain), "tokenId": token_id})["response"]

    def confirm_token(self, token=SECOND_TOKEN, amount=10**18, deposit=100, chain=1, expect=0):
        return self.call("confirmTokenDeposit", {
            "depositId": str(deposit), "contractAddr": str(EMITTER[chain]), "dappId": "0", "chainId": str(chain),
            "amount": str(amount), "anAccount": "43", "evmToken": str(token)},
            source=self.voucher(deposit, chain), expect=expect)

    def withdraw_token(self, amount, token_id=4, chain=1):
        return self.call("initiateWithdrawal", {"dstChainId": str(chain), "recipient": "11" * 20},
                         source=SENDER, ecc={str(token_id): str(amount)})

    def test_only_usdc_can_be_registered(self):
        for token, token_id in ((SECOND_TOKEN, 4), (THIRD_TOKEN, 5), (SECOND_TOKEN, 2), (SECOND_TOKEN, 0)):
            self.set_bridge_token(token, token_id, 100, 1000, expect=242)
            self.assertFalse(self.token_limits(token_id)["listed"])
        self.finalize(token=SECOND_TOKEN, expect=241)
        self.confirm_token(expect=241)
        self.assertTrue(self.token_limits(3)["listed"])

    def test_unregistered_proven_token_cannot_mint_usdc(self):
        before = self.call("getTotalBridged", {"tokenId": 3})["response"]
        self.finalize(token=SECOND_TOKEN, expect=241)
        self.confirm_token(expect=241)
        self.assertEqual(self.call("getTotalBridged", {"tokenId": 3})["response"], before)

    def test_second_token_has_independent_mint_burn_and_chain_accounting(self):
        self.register(SECOND_TOKEN, 4, 10**18, 10**19)
        before_usdc = self.limits()["outstanding"]
        self.finalize(deposit=100, token=SECOND_TOKEN, amount=10**18)
        result = self.confirm_token()
        self.assertTrue(result["vm_success"])
        self.assertEqual(int(self.token_limits()["outstanding"]), 10**18)
        self.assertEqual(self.limits()["outstanding"], before_usdc)
        self.assertEqual(int(self.call("getTotalBridged", {"tokenId": 4})["response"]["minted"]), 10**18)
        self.withdraw_token(10**18)
        self.assertEqual(int(self.token_limits()["outstanding"]), 0)
        self.assertEqual(self.limits()["outstanding"], before_usdc)
        self.assertEqual(int(self.call("getTotalBridged", {"tokenId": 4})["response"]["burned"]), 10**18)

    def test_third_token_needs_only_registration(self):
        self.register(SECOND_TOKEN, 4, 10**18, 10**19)
        self.register(THIRD_TOKEN, 5, 123, 1230)
        self.finalize(deposit=101, token=THIRD_TOKEN, amount=123)
        self.confirm_token(token=THIRD_TOKEN, amount=123, deposit=101)
        self.assertEqual(int(self.token_limits(5)["outstanding"]), 123)
        self.assertEqual(int(self.token_limits(4)["outstanding"]), 0)
        self.withdraw_token(123, token_id=5)
        self.assertEqual(int(self.token_limits(5)["outstanding"]), 0)

    def test_several_tokens_round_trip_with_separate_books(self):
        usdc_before = int(self.limits()["outstanding"])
        self.register(SECOND_TOKEN, 4, 10**18, 10**19)
        self.register(THIRD_TOKEN, 5, 500, 5000)
        self.finalize(deposit=200, token=SECOND_TOKEN, amount=10**18)
        self.finalize(deposit=201, token=THIRD_TOKEN, amount=500)
        self.finalize(deposit=202, amount=TRANSFER)
        self.confirm_token(token=THIRD_TOKEN, amount=500, deposit=201)
        self.confirm_token(token=SECOND_TOKEN, amount=10**18, deposit=200)
        self.confirm(deposit=202)
        self.assertEqual(int(self.token_limits(4)["outstanding"]), 10**18)
        self.assertEqual(int(self.token_limits(5)["outstanding"]), 500)
        self.assertEqual(int(self.limits()["outstanding"]), usdc_before + TRANSFER)
        for token_id, minted in ((4, 10**18), (5, 500)):
            self.assertEqual(int(self.call("getTotalBridged", {"tokenId": token_id})["response"]["minted"]), minted)
        burn = self.withdraw_token(200, token_id=5)
        self.assertTrue(burn["vm_success"])
        self.assertEqual(int(self.token_limits(5)["outstanding"]), 300)
        self.assertEqual(int(self.token_limits(4)["outstanding"]), 10**18)
        self.assertEqual(int(self.limits()["outstanding"]), usdc_before + TRANSFER)
        self.withdraw_token(10**18, token_id=4)
        self.assertEqual(int(self.token_limits(4)["outstanding"]), 0)
        self.assertEqual(int(self.token_limits(5)["outstanding"]), 300)
        self.withdraw(TRANSFER)
        self.assertEqual(int(self.limits()["outstanding"]), usdc_before)
        self.assertEqual(int(self.call("getTotalBridged", {"tokenId": 5})["response"]["burned"]), 200)
        self.assertEqual(int(self.call("getTotalBridged", {"tokenId": 4})["response"]["burned"]), 10**18)

    def test_several_tokens_have_their_own_caps(self):
        self.register(SECOND_TOKEN, 4, 10**18, 10**19)
        self.register(THIRD_TOKEN, 5, 500, 5000)
        self.finalize(deposit=210, token=SECOND_TOKEN, amount=10**18)
        self.finalize(deposit=211, token=THIRD_TOKEN, amount=501, expect=234)
        self.register(THIRD_TOKEN, 5, 500, 5000, enabled=False)
        self.finalize(deposit=212, token=THIRD_TOKEN, amount=500, expect=243)
        self.finalize(deposit=213, token=SECOND_TOKEN, amount=10**18)

    def test_several_tokens_stay_separate_per_source_chain(self):
        self.register(SECOND_TOKEN, 4, 10**18, 10**19, chain=1)
        self.register(SECOND_TOKEN, 4, 10**18, 10**19, chain=2)
        self.register(THIRD_TOKEN, 5, 500, 5000, chain=2)
        self.finalize(deposit=220, token=SECOND_TOKEN, amount=10**18, chain=2)
        self.confirm_token(token=SECOND_TOKEN, amount=10**18, deposit=220, chain=2)
        self.finalize(deposit=221, token=THIRD_TOKEN, amount=500, chain=2)
        self.confirm_token(token=THIRD_TOKEN, amount=500, deposit=221, chain=2)
        self.assertEqual(int(self.token_limits(4, chain=1)["outstanding"]), 0)
        self.assertEqual(int(self.token_limits(4, chain=2)["outstanding"]), 10**18)
        self.assertEqual(int(self.token_limits(5, chain=2)["outstanding"]), 500)
        self.finalize(deposit=222, token=THIRD_TOKEN, amount=500, chain=1, expect=241)

    def test_token_identity_cannot_be_reassigned(self):
        self.register(SECOND_TOKEN, 4, 100, 1000)
        self.register(THIRD_TOKEN, 4, 100, 1000, expect=242)
        self.register(SECOND_TOKEN, 5, 100, 1000, expect=242)
        self.assertEqual(int(self.token_limits(4)["evmToken"], 0), SECOND_TOKEN)
        self.assertFalse(self.token_limits(5)["listed"])

    def test_lowering_or_closing_deposits_does_not_strand_a_prior_payout(self):
        self.register(SECOND_TOKEN, 4, 10**18, 10**19)
        self.confirm_token()
        self.register(SECOND_TOKEN, 4, 1, 10**19, enabled=False)
        config = self.token_limits()
        self.assertEqual(int(config["outstanding"]), 10**18)
        self.assertEqual(int(config["maxWithdrawal"]), 10**18)
        self.finalize(token=SECOND_TOKEN, amount=1, expect=243)
        self.withdraw_token(10**18)
        self.assertEqual(int(self.token_limits()["outstanding"]), 0)

    def test_second_token_cannot_spend_another_chain_inventory(self):
        for chain in (1, 2): self.register(SECOND_TOKEN, 4, 10**18, 10**19, chain=chain)
        self.confirm_token(chain=1)
        result = self.withdraw_token(10**18, chain=2)
        self.assertEqual(int(self.call("testLastRefusal")["response"]["reason"]), 236)
        self.assertEqual(int(self.token_limits(chain=1)["outstanding"]), 10**18)
        self.assertEqual(int(self.token_limits(chain=2)["outstanding"]), 0)
        self.withdraw_token(10**18, chain=1)
        self.assertEqual(int(self.token_limits(chain=1)["outstanding"]), 0)

    def test_registry_survives_upgrade_with_per_token_principal(self):
        self.register(SECOND_TOKEN, 4, 10**18, 10**19)
        self.confirm_token()
        self.call("updateCode", {"newcode": self.bridge_code, "userCell": ""}, signed=True)
        config = self.token_limits()
        self.assertTrue(config["listed"])
        self.assertEqual(int(config["evmToken"], 0), SECOND_TOKEN)
        self.assertEqual(int(config["outstanding"]), 10**18)
        self.bind(1)
        self.withdraw_token(10**18)
        self.assertEqual(int(self.token_limits()["outstanding"]), 0)

    def test_legacy_upgrade_prefix_keeps_usdc_balances_and_rejects_unknown_assets(self):
        before = self.limits()["outstanding"]
        self.register(SECOND_TOKEN, 4, 100, 1000)
        self.call("testLegacyUpgrade")
        self.assertEqual(self.limits()["outstanding"], before)
        self.assertFalse(self.token_limits()["listed"])
        self.bind(1)
        self.withdraw(TRANSFER)
        self.assertEqual(int(self.limits()["outstanding"]), int(before) - TRANSFER)

    def test_registering_legacy_usdc_with_a_lower_deposit_cap_preserves_payouts(self):
        self.call("setOutstandingByChain", {"chainId": "3", "amount": str(TRANSFER)}, signed=True)
        self.register(USDC_TOKEN, 3, 10_000_000, TVL, chain=3)
        config = self.token_limits(token_id=3, chain=3)
        self.assertEqual(int(config["maxTransfer"]), 10_000_000)
        self.assertEqual(int(config["maxWithdrawal"]), TRANSFER)
        self.bind(3)
        result = self.withdraw(TRANSFER, chain=3)
        self.assertEqual(result["messages"], [])
        self.assertEqual(int(self.limits(chain=3)["outstanding"]), 0)

    def test_tvl_reduction_cannot_invalidate_an_accepted_deposit(self):
        amount = 10**18
        self.register(SECOND_TOKEN, 4, amount, amount * 10)
        self.confirm_token(deposit=100)
        self.finalize(deposit=101, amount=amount, token=SECOND_TOKEN)
        self.register(SECOND_TOKEN, 4, amount, amount, expect=242)
        self.confirm_token(deposit=101)
        self.assertEqual(int(self.token_limits()["outstanding"]), amount * 2)

    def test_usdc_registration_cannot_raise_the_protocol_caps(self):
        self.register(USDC_TOKEN, 3, TRANSFER + 1, TVL, expect=242)
        self.register(USDC_TOKEN, 3, TRANSFER, TVL + 1, expect=242)

    def test_issue_98_values(self):
        limits = self.limits()
        self.assertEqual(int(limits["maxTransfer"]), TRANSFER)
        self.assertEqual(int(limits["maxTvl"]), TVL)

    def test_finalize_refuses_before_accept_within_the_gas_credit(self):
        self.call("testStopAtAccept")
        result = self.finalize(anchor=False, expect=224)
        self.assertLess(result["gas_used"], GAS_CREDIT)
        result = self.finalize(expect=ACCEPT_REACHED)
        self.assertLess(result["gas_used"], GAS_CREDIT)
        # Every refusal of a proven receipt comes before accept, so the bridge pays for none.
        foreign = [vectors.deposit_log(43, 1, 1, 43, USDC_TOKEN)]
        for kwargs, code in (({"logs": [TRANSFER_LOG], "log_index": 0}, 244),
                             ({"logs": foreign, "log_index": 0}, 222),
                             ({"token": SECOND_TOKEN}, 241),
                             ({"amount": TRANSFER + 1}, 234),
                             ({"receipt": vectors.receipt([TRANSFER_LOG], status=0), "log_index": 0}, 308)):
            result = self.finalize(expect=code, **kwargs)
            self.assertLess(result["gas_used"], GAS_CREDIT, code)
        # A receipt with 60 transfers before its deposit is still checked within the credit.
        logs = [TRANSFER_LOG] * 60 + [vectors.deposit_log(42, 7, 1, 43, USDC_TOKEN)]
        result = self.finalize(logs=logs, log_index=60, expect=ACCEPT_REACHED)
        self.assertLess(result["gas_used"], GAS_CREDIT)
        self.call("setPaused", {"paused": True}, signed=True)
        result = self.finalize(expect=231)
        self.assertLess(result["gas_used"], GAS_CREDIT)

    def test_limits_refuse_a_proven_deposit(self):
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

    def test_a_chain_without_an_evm_bridge_refuses_before_any_limit(self):
        self.assertEqual(self.evm_bridge(1), EVM_BRIDGE)
        self.bind(1, 0)
        self.assertEqual(self.evm_bridge(1), 0)
        self.assertEqual(self.evm_bridge(2), EVM_BRIDGE)
        self.refund(self.withdraw(TRANSFER + 1, chain=1), TRANSFER + 1, 1_000_000_000, 239)
        self.refund(self.withdraw(1_000_000, chain=1), 1_000_000, 1_000_000_000, 239)
        self.refund(self.withdraw(1_000_000, chain=7), 1_000_000, 1_000_000_000, 239)
        self.assertEqual(int(self.limits(1)["outstanding"]), TVL - TRANSFER)
        self.bind(1)
        self.assertTrue(self.withdraw(1_000_000, chain=1)["vm_success"])
        self.assertEqual(int(self.limits(1)["outstanding"]), TVL - TRANSFER - 1_000_000)

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

    def test_a_chain_balance_alone_does_not_pay_out_an_empty_inventory(self):
        self.seed(0)
        self.call("setOutstandingByChain", {"chainId": "1", "amount": str(TRANSFER)},
                  signed=True)
        self.assertEqual(int(self.limits(1)["outstanding"]), TRANSFER)
        self.refund(self.withdraw(1_000_000, chain=1), 1_000_000, 1_000_000_000, 233)

    def test_the_owner_restores_a_chain_balance_an_upgrade_left_empty(self):
        self.seed(TRANSFER)
        self.call("setOutstandingByChain", {"chainId": "1", "amount": "0"}, signed=True)
        self.refund(self.withdraw(1_000_000, chain=1), 1_000_000, 1_000_000_000, 236)
        self.call("setOutstandingByChain", {"chainId": "1", "amount": str(TRANSFER)},
                  signed=True)
        result = self.withdraw(1_000_000, chain=1)
        self.assertTrue(result["vm_success"])
        self.assertEqual(int(self.limits(1)["outstanding"]), TRANSFER - 1_000_000)
        bridged = self.call("getTotalBridged", {"tokenId": 3})["response"]
        self.assertEqual(int(bridged["burned"]), 1_000_000)

    def test_upgrade_preserves_both_networks(self):
        self.seed(TRANSFER, 2 * TRANSFER)
        self.call("updateCode", {"newcode": self.bridge_code, "userCell": ""}, signed=True)
        self.assertEqual(int(self.limits(1)["outstanding"]), TRANSFER)
        self.assertEqual(int(self.limits(2)["outstanding"]), 2 * TRANSFER)

    def test_production_voucher_forwards_the_second_token_and_round_trips_its_currency(self):
        amount = 10**18
        self.register(SECOND_TOKEN, 4, amount, amount * 10)
        usdc_before = self.limits()["outstanding"]
        deploy = self.finalize(token=SECOND_TOKEN, amount=amount)["messages"][0]
        voucher = self.root / "second-token-voucher.tvc"
        voucher.write_bytes(base64.b64decode(deploy["state_init"]))
        params = {"depositId": "0", "contractAddr": "42", "dappId": "0", "chainId": "1",
                  "amount": str(amount), "anAccount": "43", "evmToken": str(SECOND_TOKEN)}
        result = json.loads(command([
            DEBUGGER, "run", "-i", str(voucher), "-a", str(self.voucher_abi), "--json",
            "--address", deploy["destination"], "--internal", "--message-source", BRIDGE,
            "--message-value", "2000000000", "-m", "constructor", "-p", json.dumps(params)]))
        self.assertEqual(result["exit_code"], 0)
        self.assertEqual(len(result["messages"]), 1)
        callback = json.loads(command(["tvm-cli", "-j", "decode", "msg",
                                       result["messages"][0]["boc"], "--abi", str(self.abi)]))
        body = callback["BodyCall"]["confirmTokenDeposit"]
        self.assertEqual(int(body["evmToken"], 0), SECOND_TOKEN)
        self.assertEqual(callback["Header"]["destination"], BRIDGE)
        self.call("confirmTokenDeposit", body, source=callback["Header"]["source"])
        self.assertEqual(int(self.token_limits()["outstanding"]), amount)
        self.assertEqual(self.limits()["outstanding"], usdc_before)
        self.withdraw_token(amount)
        self.assertEqual(int(self.token_limits()["outstanding"]), 0)
        self.assertEqual(self.limits()["outstanding"], usdc_before)

    def test_voucher_constructor_replay_cannot_send_a_second_confirmation(self):
        deploy = self.finalize()["messages"][0]
        self.assertEqual(int(self.limits()["outstanding"]), TVL - TRANSFER)
        voucher = self.root / "voucher-case.tvc"
        voucher.write_bytes(base64.b64decode(deploy["state_init"]))
        abi = self.voucher_abi
        params = {"depositId": "0", "contractAddr": "42", "dappId": "0", "chainId": "1",
                  "amount": str(TRANSFER), "anAccount": "43", "evmToken": str(USDC_TOKEN)}
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


    # ------------------------------------------------------------ deposit receipt

    def test_a_proven_deposit_deploys_its_voucher(self):
        self.deployed(self.finalize(deposit=7), 7)

    def test_legacy_typed_and_arbitrum_receipts_all_prove(self):
        for i, tx_type in enumerate((0, 1, 2, 3, 4, 0x64, 0x68, 0x6a, 0x78)):
            with self.subTest(tx_type=tx_type):
                log = vectors.deposit_log(42, 10 + i, TRANSFER // 10, 43, USDC_TOKEN)
                receipt = vectors.receipt([TRANSFER_LOG, log], tx_type=tx_type)
                self.deployed(self.finalize(receipt=receipt), 10 + i)

    def test_the_deposit_log_is_the_one_at_log_index(self):
        logs = [vectors.deposit_log(42, 30, 1, 43, USDC_TOKEN), TRANSFER_LOG,
                vectors.deposit_log(42, 31, 2, 43, USDC_TOKEN)]
        self.deployed(self.finalize(logs=logs, log_index=0), 30)
        self.deployed(self.finalize(logs=logs, log_index=2), 31)
        self.finalize(logs=logs, log_index=1, expect=244)
        self.finalize(logs=logs, log_index=3, expect=307)
        self.finalize(logs=[], log_index=0, expect=307)

    def test_a_deposit_from_an_untrusted_emitter_is_refused(self):
        for emitter in (43, 0):
            with self.subTest(emitter=emitter):
                self.trust(1, 0)
                log = vectors.deposit_log(emitter, 40, 1, 43, USDC_TOKEN)
                self.finalize(logs=[log], log_index=0, expect=222)
        self.trust(2, 43)
        self.finalize(logs=[vectors.deposit_log(43, 40, 1, 43, USDC_TOKEN)], log_index=0, expect=222)

    def test_an_l1_bridge_is_trusted_under_one_chain_only(self):
        self.trust(2, 42, expect=228)
        self.assertFalse(self.trusted(2, 42))
        self.trust(1, 42)
        self.trust(2, 42, allowed=False)
        self.assertTrue(self.trusted(1, 42))
        self.assertEqual(self.trusted_chain(42), 1)
        self.trust(1, 42, allowed=False)
        self.assertFalse(self.trusted(1, 42))
        self.assertIsNone(self.trusted_chain(42))
        self.trust(2, 42)
        self.assertEqual(self.trusted_chain(42), 2)
        self.trust(1, 42, expect=228)
        self.assertFalse(self.trusted(1, 42))

    def test_a_deposit_made_on_one_chain_does_not_mint_under_another(self):
        v = vectors.synthetic(vectors.receipt([vectors.deposit_log(42, 49, 1, 43, USDC_TOKEN)]))
        self.anchor(v, chain=2)
        self.trust(2, 42, expect=228)
        self.submit(v, chain=2, log_index=0, expect=222)
        self.anchor(v, chain=1)
        self.deployed(self.submit(v, chain=1, log_index=0), 49)

    def test_an_upgrade_drops_the_trusted_set_and_the_chain_it_was_bound_to(self):
        self.call("updateCode", {"newcode": self.bridge_code, "userCell": ""}, signed=True)
        self.assertFalse(self.trusted(1, 42))
        self.assertIsNone(self.trusted_chain(42))
        self.trust(2, 42)
        self.assertEqual(self.trusted_chain(42), 2)

    def test_another_event_of_the_bridge_is_refused(self):
        withdrawal = vectors.keccak256(b"WithdrawalPaid(uint256,address,uint256,bytes32,uint256,address)")
        for topic0 in (withdrawal, vectors.LEGACY_DEPOSIT_TOPIC0, b"\0" * 32):
            with self.subTest(topic0=topic0.hex()):
                log = vectors.deposit_log(42, 41, 1, 43, USDC_TOKEN, topic0=topic0)
                self.finalize(logs=[log], log_index=0, expect=244)

    def test_a_deposit_log_of_another_shape_is_refused(self):
        legacy = vectors.deposit_log(42, 42, 1, 43, USDC_TOKEN, drop_words=1,
                                     topic0=vectors.LEGACY_DEPOSIT_TOPIC0)
        for log in (vectors.deposit_log(42, 42, 1, 43, USDC_TOKEN, drop_words=1),
                    vectors.deposit_log(42, 42, 1, 43, USDC_TOKEN, extra_words=1),
                    vectors.deposit_log(42, 42, 1, 43, USDC_TOKEN, extra_topics=1),
                    vectors.deposit_log(42, 42, 1, 43, USDC_TOKEN, drop_words=4), legacy):
            with self.subTest(log=log[2].hex()[:16]):
                self.finalize(logs=[log], log_index=0, expect=244)

    def test_a_deposit_log_inside_another_logs_data_is_refused(self):
        inner = vectors.rlp(list(vectors.deposit_log(42, 43, 1, 43, USDC_TOKEN)))
        outer = (b"\x77" * 20, [vectors.keccak256(b"Forward(bytes)")], inner)
        self.finalize(logs=[outer], log_index=0, expect=244)
        self.finalize(logs=[outer], log_index=1, expect=307)
        carrier = (vectors.deposit_log(42, 0, 0, 0, 0)[0], [vectors.DEPOSIT_TOPIC0], inner)
        self.finalize(logs=[carrier], log_index=0, expect=244)

    def test_a_block_that_is_not_accepted_is_refused(self):
        self.finalize(anchor=False, expect=224)
        v = vectors.synthetic(vectors.receipt([vectors.deposit_log(EMITTER[2], 44, 1, 43, USDC_TOKEN)]))
        self.anchor(v, chain=2)
        self.submit(v, chain=1, log_index=0, expect=224)
        self.deployed(self.submit(v, chain=2, log_index=0), 44, chain=2)

    def test_a_repeated_deposit_id_reaches_the_same_voucher(self):
        self.deployed(self.finalize(deposit=45, amount=1), 45)
        self.deployed(self.finalize(deposit=45, amount=2, number=9_000_001), 45)

    def test_a_tampered_block_receipt_or_proof_is_refused(self):
        v = vectors.synthetic(vectors.receipt([TRANSFER_LOG, vectors.deposit_log(42, 46, 1, 43, USDC_TOKEN)]))
        self.anchor(v)

        def flip(h, at):
            b = bytearray.fromhex(h)
            b[at] ^= 1
            return b.hex()

        leaf = v["receiptProof"][-1]
        for case, change, code in (
                ("header", {"header": flip(v["header"], 100)}, 224),
                ("receipt", {"receiptProof": v["receiptProof"][:-1] + [flip(leaf, len(leaf) // 2 - 40)]}, 302),
                ("root node", {"receiptProof": [flip(v["receiptProof"][0], 50)] + v["receiptProof"][1:]}, 302),
                ("node dropped", {"receiptProof": v["receiptProof"][:-1]}, 303),
                ("node added", {"receiptProof": v["receiptProof"] + [leaf]}, 303),
                ("no proof", {"receiptProof": []}, 303),
                ("another transaction", {"txIndex": v["txIndex"] + 1}, 302),
                ("past the last transaction", {"txIndex": 0x7f}, 305)):
            with self.subTest(case=case):
                self.submit(dict(v, **change), expect=code)
        self.deployed(self.submit(v), 46)

    def test_a_failed_transaction_is_refused(self):
        receipt = vectors.receipt([vectors.deposit_log(42, 47, 1, 43, USDC_TOKEN)], status=0)
        self.finalize(receipt=receipt, log_index=0, expect=308)

    def test_deposit_fields_are_checked_after_the_proof(self):
        self.finalize(logs=[vectors.deposit_log(42, 48, 0, 43, USDC_TOKEN)], log_index=0, expect=204)
        self.finalize(logs=[vectors.deposit_log(42, 48, 1 << 64, 43, USDC_TOKEN)], log_index=0, expect=214)
        self.finalize(logs=[vectors.deposit_log(42, 48, 1, 0, USDC_TOKEN)], log_index=0, expect=230)
        self.finalize(logs=[vectors.deposit_log(42, 48, 1, 43, SECOND_TOKEN)], log_index=0, expect=241)

    def test_live_arbitrum_sepolia_receipts_prove(self):
        for name in ("arbitrum_sepolia_326615634.json", "arbitrum_sepolia_326657966.json"):
            with self.subTest(vector=name):
                live = json.loads((REPO / "scripts/deposit-receipt/fixtures" / name).read_text())
                args = dict(live["finalizeDeposit"])
                chain = args.pop("chainId")
                log = live["logs"][0]
                got = self.call("testReceiptLog", args)["response"]
                self.assertEqual(int(got["emitter"], 0), int(log["address"], 16))
                self.assertEqual(int(got["topic0"], 0), int(log["topics"][0], 16))
                # These are the deployed bridge's five-field Deposit: proven, then refused.
                self.call("testAnchor", {"chainId": str(chain), "blockHash": live["blockHash"]})
                self.trust(chain, int(log["address"], 16))
                self.submit({**args}, chain=chain, log_index=args["logIndex"], expect=244)

if __name__ == "__main__":
    unittest.main()
