#!/usr/bin/env python3
"""Run the light client's committee bookkeeping in TVM without a node.

Set SOLD_0_81_1 to sold 0.81.1 and TVM_DEBUGGER to a tvm-debugger. The contract is
compiled with test hooks: one bootstraps it without the bridge (`initialize` needs the
bridge as sender and a salted code), one runs the bookkeeping `submitRotate` does after
its proof verifies, one asks whether a step's signer is held, one runs the upgrade
migration. `submitUpdate` itself runs unchanged: its committee check comes before the
proof is verified, so a step signed by a committee the contract does not hold is refused
with `ERR_WRONG_COMMITTEE`, and one it holds passes the check and fails later, at the
proof.
"""

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
DEBUGGER = os.environ.get("TVM_DEBUGGER", "tvm-debugger")
ADDRESS = "0:" + "9c" * 32
BRIDGE = "0:" + "1a" * 32
SLOTS_PER_PERIOD = 8192
KEPT = 2
GLOAS = 11_296_768
ERR_BAD_PUBLIC_INPUTS = 240
ERR_WRONG_COMMITTEE = 242
ERR_STALE_PERIOD = 245
FIRST = 100


def command(args):
    result = subprocess.run(args, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(f"{args[0]}: {result.stdout}\n{result.stderr}")
    return result.stdout


def committee(period):
    """A made-up commitment for each period."""
    return 0xC0FFEE00 + period


def slot_in(period, offset=100):
    return period * SLOTS_PER_PERIOD + offset


def step_inputs(attested_slot, commitment, finalized_slot=None):
    """The 11 step public inputs, 32-byte little-endian each, of a step that would
    advance an empty head."""
    if finalized_slot is None:
        finalized_slot = attested_slot - 64
    words = [attested_slot, finalized_slot, 0x11, 0x22, 512, commitment,
             0x33, 0x44, 0x55, 0x66, GLOAS]
    return b"".join(w.to_bytes(32, "little") for w in words).hex()


def rotate_inputs(current, nxt, period):
    """The 17 rotate public inputs: 12 accumulator limbs, then the current and the next
    commitment, the period, the Gloas fork slot and an attested slot of the period before."""
    words = [0] * 12 + [current, nxt, period, GLOAS, slot_in(period - 1)]
    return b"".join(w.to_bytes(32, "little") for w in words).hex()


class LightClientCommittees(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="an-light-client-")
        cls.root = Path(cls.temp.name)
        src = cls.root / "an"
        shutil.copytree(REPO / "contracts/an", src)
        client = src / "exchange/EthBeaconLightClient.sol"
        source = client.read_text()
        hooks = """
    function testBootstrap(uint256 pubkey, uint256 committee, uint64 period) public {
        tvm.accept();
        _ownerPubkey = pubkey;
        _setOnlyCommittee(committee, period);
        _gloasForkSlot = 11296768;
        _gloasForkSlotSet = true;
    }
    function testSetHead(uint64 slot) public {
        tvm.accept();
        _finalizedSlot = slot;
    }
    function testRotate(uint256 next, uint64 period) public {
        tvm.accept();
        _installRotated(next, period);
    }
    function testSignedBy(uint64 attestedSlot, uint256 commitment) public view returns (bool held) {
        StepPI pi;
        pi.attestedSlot = attestedSlot;
        pi.committeeCommitment = commitment;
        held = _signedByHeldCommittee(pi);
    }
    function testUpgrade() public {
        tvm.accept();
        TvmCell userCell;
        TvmCell cell = abi.encode(_ownerPubkey, _l1ChainId, _finalizedSlot, _finalizedBeaconRoot,
            _finalizedExecutionBlockHash, _updatesApplied, _currentCommittee, _committeePeriod,
            _ownerRotationEnabled, _reAnchorsApplied, _provenEthSlot, _provenQueue,
            _provenQueueHead, _provenQueueTail, userCell);
        onCodeUpgrade(cell);
    }
"""
        end = source.rfind("}")
        client.write_text(source[:end] + hooks + source[end:])
        command([os.environ["SOLD_0_81_1"], "--tvm-version", "gosh", "--base-path", str(src),
                 str(client), "-o", str(cls.root)])
        cls.abi = cls.root / "EthBeaconLightClient.abi.json"
        keys = (REPO / "crates/ackinacki-bridge/src/test_keys.rs").read_text()
        cls.keys = {n: re.search(rf'PAIR_{n.upper()}: &str = "([0-9a-f]+)"', keys)[1]
                    for n in ("public", "secret")}
        cls.key_file = cls.root / "keys.json"
        cls.key_file.write_text(json.dumps(cls.keys))

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def setUp(self):
        self.tvc = self.root / "case.tvc"
        shutil.copyfile(self.root / "EthBeaconLightClient.tvc", self.tvc)
        self.call("constructor", source=BRIDGE)
        self.call("testBootstrap", {"pubkey": "0x" + self.keys["public"],
                                    "committee": str(committee(FIRST)), "period": FIRST})

    def call(self, function, args=None, *, source=None, signed=False, expect=0):
        if source:
            extra = ["--internal", "--message-source", source, "--message-value", "1000000000"]
        else:
            extra = ["--abi-header", json.dumps({"expire": int(time.time()) + 240})]
        if signed:
            extra += ["--sign", str(self.key_file)]
        result = json.loads(command([
            DEBUGGER, "run", "-i", str(self.tvc), "-a", str(self.abi), "--address", ADDRESS,
            "--json", "-m", function, "-p", json.dumps(args or {})
        ] + extra))
        if expect is not None:
            self.assertEqual(result["exit_code"], expect, (function, args, result["exit_code"]))
        return result

    def rotate_to(self, period):
        for p in range(self.state()["committeePeriod"] + 1, period + 1):
            self.call("testRotate", {"next": str(committee(p)), "period": p})

    def signed_by(self, attested_period, commitment, offset=100):
        response = self.call("testSignedBy", {"attestedSlot": slot_in(attested_period, offset),
                                              "commitment": str(commitment)})["response"]
        return response["held"] in (True, "true")

    def state(self):
        response = self.call("getCommitteeState")["response"]
        return {k: v if k == "ownerRotationEnabled" else int(v, 0)
                for k, v in response.items()}

    def held(self):
        committees = self.call("getCommittees")["response"]["committees"]
        return {int(p): int(c, 0) for p, c in committees.items()}

    def submit(self, attested_period, commitment, finalized_slot=None):
        return self.call("submitUpdate", {
            "proof": "00",
            "publicInputs": step_inputs(slot_in(attested_period), commitment, finalized_slot),
        }, expect=None)

    def test_a_replaced_committee_signs_for_its_period_for_one_more_rotation(self):
        for current in range(FIRST + 1, FIRST + KEPT):
            self.rotate_to(current)
            self.assertTrue(self.signed_by(FIRST, committee(FIRST)), current)
        self.rotate_to(FIRST + KEPT)
        self.assertFalse(self.signed_by(FIRST, committee(FIRST)))
        self.assertEqual(sorted(self.held()), list(range(FIRST + 1, FIRST + KEPT + 1)))

    def test_a_committee_signs_its_own_period_and_the_one_before(self):
        self.rotate_to(FIRST + 3)
        q = FIRST + 2
        self.assertTrue(self.signed_by(q, committee(q)))
        # The last header of a period is signed in the next one.
        self.assertTrue(self.signed_by(q - 1, committee(q), offset=SLOTS_PER_PERIOD - 1))
        self.assertFalse(self.signed_by(q + 1, committee(q)))
        self.assertFalse(self.signed_by(q - 2, committee(q)))
        self.assertFalse(self.signed_by(q - 1, committee(q + 1)))

    def test_the_state_getter_names_the_committee_before_the_one_in_force(self):
        self.rotate_to(FIRST + 2)
        state = self.state()
        self.assertEqual(state["committeePeriod"], FIRST + 2)
        self.assertEqual(state["currentCommittee"], committee(FIRST + 2))
        self.assertEqual(state["prevCommittee"], committee(FIRST + 1))
        self.assertEqual(self.held(), {p: committee(p) for p in range(FIRST + 1, FIRST + 3)})

    def test_an_owner_reset_drops_every_committee_but_its_own(self):
        self.rotate_to(FIRST + 4)
        self.call("setCommitteeCommitment",
                  {"committeeCommitment": "0xabc", "period": FIRST + 40}, signed=True)
        self.assertEqual(self.held(), {FIRST + 40: 0xABC})
        self.assertEqual(self.state()["prevCommittee"], 0)
        self.assertFalse(self.signed_by(FIRST + 3, committee(FIRST + 3)))

    def test_an_upgrade_keeps_only_the_committee_in_force(self):
        self.rotate_to(FIRST + 3)
        self.call("testUpgrade")
        self.assertEqual(self.held(), {FIRST + 3: committee(FIRST + 3)})
        self.assertEqual(self.state()["currentCommittee"], committee(FIRST + 3))

    def test_a_rotate_to_a_period_not_past_the_one_in_force_is_refused_before_the_proof(self):
        self.rotate_to(FIRST + 2)
        for period in (FIRST + 1, FIRST + 2):
            result = self.call("submitRotate", {
                "proof": "00",
                "publicInputs": rotate_inputs(committee(FIRST + 2), 0xD00D, period),
            }, expect=None)
            self.assertEqual(result["exit_code"], ERR_STALE_PERIOD, period)
        result = self.call("submitRotate", {
            "proof": "00",
            "publicInputs": rotate_inputs(committee(FIRST + 2), 0xD00D, FIRST + 3),
        }, expect=None)
        self.assertNotIn(result["exit_code"], (0, ERR_STALE_PERIOD), "a made-up proof")

    def test_a_step_signed_by_a_committee_not_held_is_refused_before_the_proof(self):
        self.rotate_to(FIRST + KEPT)
        result = self.submit(FIRST, committee(FIRST))
        self.assertEqual(result["exit_code"], ERR_WRONG_COMMITTEE)

    def test_a_step_signed_by_a_held_committee_gets_past_the_committee_check(self):
        self.rotate_to(FIRST + KEPT - 1)
        result = self.submit(FIRST, committee(FIRST))
        self.assertNotEqual(result["exit_code"], ERR_WRONG_COMMITTEE)
        self.assertNotEqual(result["exit_code"], 0, "a made-up proof must not verify")

    def test_a_step_finalizing_a_slot_after_its_attested_one_is_refused_before_the_proof(self):
        self.rotate_to(FIRST + KEPT - 1)
        for signer in (FIRST, FIRST + KEPT - 1):
            ahead = self.submit(signer, committee(signer), slot_in(FIRST + 40))
            self.assertEqual(ahead["exit_code"], ERR_BAD_PUBLIC_INPUTS, signer)
            same = self.submit(signer, committee(signer), slot_in(signer))
            self.assertNotIn(same["exit_code"], (0, ERR_BAD_PUBLIC_INPUTS), signer)

    def test_a_late_step_signed_by_a_dropped_committee_is_refused_before_the_proof(self):
        self.rotate_to(FIRST + KEPT)
        self.call("testSetHead", {"slot": slot_in(FIRST + KEPT, 500)})
        dropped = self.submit(FIRST, committee(FIRST))
        self.assertEqual(dropped["exit_code"], ERR_WRONG_COMMITTEE)
        held = self.submit(FIRST + 1, committee(FIRST + 1))
        self.assertNotIn(held["exit_code"], (0, ERR_WRONG_COMMITTEE))

    def test_a_rotate_past_several_periods_keeps_only_the_new_committee(self):
        self.call("testRotate", {"next": str(committee(FIRST + 5)), "period": FIRST + 5})
        self.assertEqual(self.held(), {FIRST + 5: committee(FIRST + 5)})
        self.assertFalse(self.signed_by(FIRST, committee(FIRST)))


if __name__ == "__main__":
    unittest.main()
