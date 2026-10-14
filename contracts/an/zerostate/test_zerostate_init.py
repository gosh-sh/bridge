#!/usr/bin/env python3
"""Tests for zerostate_init.py.

    python3 contracts/an/zerostate/test_zerostate_init.py
"""

import json
import os
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
import zerostate_init  # noqa: E402

OK = "TVM terminated with exit code 0"
CELL = "te6ccgEBAQEAAgAAAA=="

# Stands in for the placed encoder .tvc. `initialize()` must copy whatever
# `artefact(ENCODER_TVC)` returns before running the debugger on it (the
# debugger rewrites the persistent-data cell of any image it is given, and
# the placed file is a tracked artefact checked against a sha256 manifest),
# so the fake has to be a real file, not just a string.
ENCODER_TVC_CONTENT = b"pretend-encoder-bytes"


class FakeRun:
    def __init__(self):
        self.commands = []

    def __call__(self, cmd):
        self.commands.append(cmd)
        if "getBridgeData" in cmd:
            return f'{OK}\n{{"value0":"{CELL}"}}\n'
        return OK


def image_argument(cmd):
    """The path following `-i` in a debugger command line."""
    parts = cmd.split()
    return parts[parts.index("-i") + 1]


def params_argument(cmd):
    """The `-p '<json>'` payload of a debugger command line, parsed back."""
    start = cmd.index("-p '") + len("-p '")
    end = cmd.index("' -i", start)
    return json.loads(cmd[start:end])


def make_artefact(encoder_tvc_path):
    def artefact(src):
        if src == zerostate_init.ENCODER_TVC:
            return encoder_tvc_path
        return f"/placed/{src}"
    return artefact


def context(run, encoder_tvc_path):
    return {
        "version": zerostate_init.CONTEXT_VERSION,
        "tvm_debugger": "/tools/tvm-debugger",
        "zerostate_helper": "/tools/zerostate-helper",
        "zerostate_path": "./config/zerostate",
        "account_tvc": "/tmp/stub.tvc",
        "update_zero_abi": "./contracts/0.79.3_compiled/updatezerocontract/UpdateZeroContract.abi.json",
        "root_keys": "./config/BlockKeeperContractRoot.keys.json",
        "address": "0:" + "1a" * 32,
        "keys": "./config/USDCBridge.keys.json",
        "usdc_wallet": "0:" + "ab" * 32,
        "balance": 1_000_000_000_000,
        "dapp_id": "0x" + "00" * 32,
        "abi_header": "'{\"expire\":1}'",
        "run": run,
        "format_params": lambda params: "'" + json.dumps(params) + "'",
        "get_code": lambda path: f"code-of:{path}",
        "read_public_key": lambda path: "aa" * 32,
        "artefact": make_artefact(encoder_tvc_path),
    }


class InitializeTest(unittest.TestCase):
    def setUp(self):
        fd, self.encoder_tvc = tempfile.mkstemp(suffix=".tvc")
        with os.fdopen(fd, "wb") as f:
            f.write(ENCODER_TVC_CONTENT)
        self.addCleanup(self._remove_if_present, self.encoder_tvc)

    @staticmethod
    def _remove_if_present(path):
        if os.path.exists(path):
            os.remove(path)

    def context(self, run):
        return context(run, self.encoder_tvc)

    def run_initialize(self):
        run = FakeRun()
        zerostate_init.initialize(self.context(run))
        return run.commands

    def test_commands_run_in_order(self):
        commands = self.run_initialize()
        self.assertEqual(len(commands), 5)
        self.assertIn("-m getBridgeData", commands[0])
        self.assertIn("-m updateCode", commands[1])
        self.assertIn("-m setTrustedL1Bridge", commands[2])
        self.assertIn("-m setLightClientCode", commands[3])
        self.assertIn("add contract", commands[4])

    def test_encoder_runs_with_the_placed_abi_and_decodes_output(self):
        encoder = self.run_initialize()[0]
        self.assertIn("/placed/contracts/an/zerostate/BridgeZerostateData.abi.json", encoder)
        self.assertIn("--decode-out-messages", encoder)

    def test_encoder_runs_on_a_copy_of_the_placed_tvc_not_the_original(self):
        # `tvm-debugger run -i` rewrites the persistent-data cell of
        # whatever image it is given, and the placed encoder .tvc is a
        # tracked file checked against a sha256 manifest, so the debugger
        # must run on a throwaway copy of it.
        seen = {}

        class RecordingRun(FakeRun):
            def __call__(self, cmd):
                output = super().__call__(cmd)
                if "getBridgeData" in cmd:
                    image = image_argument(cmd)
                    seen["image"] = image
                    seen["existed_during_run"] = os.path.exists(image)
                    with open(image, "rb") as f:
                        seen["content"] = f.read()
                return output

        run = RecordingRun()
        zerostate_init.initialize(self.context(run))

        self.assertNotEqual(seen["image"], self.encoder_tvc)
        self.assertTrue(seen["existed_during_run"])
        self.assertEqual(seen["content"], ENCODER_TVC_CONTENT)
        # Nothing left behind: neither the copy nor its containing temp dir.
        self.assertFalse(os.path.exists(seen["image"]))
        self.assertFalse(os.path.isdir(os.path.dirname(seen["image"])))

    def test_abi_header_is_carried_on_every_debugger_call(self):
        run = FakeRun()
        ctx = self.context(run)
        header = ctx["abi_header"]
        zerostate_init.initialize(ctx)
        # getBridgeData, updateCode, setTrustedL1Bridge, setLightClientCode:
        # every call that goes through `_debugger` must carry the caller's
        # expire header; `add contract` (commands[4]) does not go through
        # `_debugger` and carries none.
        for cmd in run.commands[:4]:
            self.assertIn(f"--abi-header {header}", cmd)

    def test_encoder_params_match_getBridgeData(self):
        run = FakeRun()
        ctx = self.context(run)
        zerostate_init.initialize(ctx)
        params = params_argument(run.commands[0])
        self.assertEqual(params["pubkey"], "0x" + ctx["read_public_key"](ctx["keys"]))
        self.assertEqual(params["usdcWallet"], ctx["usdc_wallet"])
        self.assertEqual(params["totalMinted"], "0")
        self.assertEqual(params["mintNonce"], "0")
        self.assertEqual(params["mintAccumulatorNonce"], "0")
        self.assertEqual(params["mintedKeys"], [])
        self.assertEqual(params["mintedValues"], [])
        self.assertEqual(params["burnedKeys"], [])
        self.assertEqual(params["burnedValues"], [])
        # Specifically the voucher's code, not the bridge's and not the
        # light client's — a swap here silently ships a bridge that cannot
        # deploy vouchers.
        self.assertEqual(
            params["depositVoucherCode"],
            ctx["get_code"](ctx["artefact"](zerostate_init.VOUCHER_TVC)),
        )

    def test_update_code_carries_the_encoded_cell_and_the_bridge_code(self):
        update = self.run_initialize()[1]
        self.assertIn(CELL, update)
        self.assertIn("code-of:/placed/contracts/an/0.80.0_compiled/exchange/eccUSDCBridge.tvc", update)
        self.assertIn("--sign ./config/BlockKeeperContractRoot.keys.json", update)

    def test_owner_calls_are_signed_with_the_bridge_key(self):
        commands = self.run_initialize()
        for cmd in commands[2:4]:
            self.assertIn("--sign ./config/USDCBridge.keys.json", cmd)
            self.assertIn("/placed/contracts/an/0.80.0_compiled/exchange/eccUSDCBridge.abi.json", cmd)

    def test_light_client_code_comes_from_the_placed_artefact(self):
        self.assertIn(
            "code-of:/placed/contracts/an/0.81.0_compiled/exchange/EthBeaconLightClient.tvc",
            self.run_initialize()[3],
        )

    def test_account_is_added_with_balance_and_dapp_id(self):
        add = self.run_initialize()[4]
        self.assertIn("/tmp/stub.tvc 1000000000000 0:" + "1a" * 32, add)
        self.assertIn("0x" + "00" * 32, add)

    def test_l1_parameters_default_to_sepolia(self):
        params = params_argument(self.run_initialize()[2])
        self.assertEqual(params["chainId"], "11155111")
        self.assertEqual(params["l1Bridge"], "0xCdFd6Cef70F68d0849310cD970F8ef8F8E4b4fdb")

    def test_l1_parameters_come_from_the_environment(self):
        os.environ["BRIDGE_ZS_L1_CHAIN_ID"] = "1"
        os.environ["BRIDGE_ZS_L1_BRIDGE"] = "0xfeed"
        try:
            reloaded = __import__("importlib").reload(zerostate_init)
            run = FakeRun()
            reloaded.initialize(self.context(run))
            self.assertIn("0xfeed", run.commands[2])
        finally:
            del os.environ["BRIDGE_ZS_L1_CHAIN_ID"], os.environ["BRIDGE_ZS_L1_BRIDGE"]
            __import__("importlib").reload(zerostate_init)

    def test_unknown_context_version_is_refused(self):
        ctx = self.context(FakeRun())
        ctx["version"] = zerostate_init.CONTEXT_VERSION + 1
        with self.assertRaises(SystemExit) as err:
            zerostate_init.initialize(ctx)
        self.assertIn(str(zerostate_init.CONTEXT_VERSION), str(err.exception))

    def test_missing_context_key_is_named(self):
        ctx = self.context(FakeRun())
        del ctx["usdc_wallet"]
        with self.assertRaises(SystemExit) as err:
            zerostate_init.initialize(ctx)
        self.assertIn("usdc_wallet", str(err.exception))

    def test_a_failed_debugger_call_stops_the_run(self):
        class Failing(FakeRun):
            def __call__(self, cmd):
                output = super().__call__(cmd)
                if "setTrustedL1Bridge" in cmd:
                    return "TVM terminated with exit code 42"
                return output

        run = Failing()
        with self.assertRaises(SystemExit):
            zerostate_init.initialize(self.context(run))
        self.assertEqual(len(run.commands), 3)


if __name__ == "__main__":
    unittest.main()
