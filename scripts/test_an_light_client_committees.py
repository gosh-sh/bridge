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
KEPT = 3
GLOAS = 11_296_768
ERR_BAD_PUBLIC_INPUTS = 240
ERR_WRONG_COMMITTEE = 242
ERR_STALE_PERIOD = 245
ERR_COMMITTEE_TOO_OLD = 259
ERR_UNKNOWN_L1 = 260
GRACE_SECONDS = 7200
PERIOD_SECONDS = 12 * SLOTS_PER_PERIOD
GENESIS = {1: 1606824023, 11155111: 1655733600, 560048: 1742213400}
SEPOLIA = 11155111
PERIOD_NOW = 1500
NOW = GENESIS[SEPOLIA] + PERIOD_NOW * PERIOD_SECONDS + 4000
FIRST = PERIOD_NOW


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
        clock = "uint64(block.timestamp)"
        assert source.count(clock) == 1
        source = source.replace(clock, "_testClock")
        hooks = """
    uint64 _testClock;
    function testSetClock(uint64 time) public {
        tvm.accept();
        _testClock = time;
    }
    function testBootstrap(uint256 pubkey, uint256 committee, uint64 period) public {
        tvm.accept();
        _ownerPubkey = pubkey;
        _l1ChainId = 11155111;
        _setOnlyCommittee(committee, period);
        _gloasForkSlot = 11296768;
        _gloasForkSlotSet = true;
    }
    function testSetChain(uint256 chainId) public {
        tvm.accept();
        _l1ChainId = chainId;
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
        held = _heldSignerPeriod(pi).hasValue();
    }
    function testHash(bytes header) public pure returns (uint256 hash) {
        hash = gosh.keccak256(header);
    }
    function testCacheHash(uint256 hash, uint64 slot) public {
        tvm.accept();
        _provenEthSlot[_piForm(hash)] = slot;
    }
    function testUpgrade(bool keepRecovery) public {
        tvm.accept();
        TvmCell userCell;
        if (keepRecovery) { userCell = _encodeMigrationExtension(userCell); }
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
        self.set_clock(NOW)
        self.bootstrap(FIRST)

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

    def set_clock(self, time_):
        self.call("testSetClock", {"time": time_})

    def bootstrap(self, period):
        self.call("testBootstrap", {"pubkey": "0x" + self.keys["public"],
                                    "committee": str(committee(period)), "period": period})

    def submit(self, attested_period, commitment, finalized_slot=None):
        return self.call("submitUpdate", {
            "proof": "00",
            "publicInputs": step_inputs(slot_in(attested_period), commitment, finalized_slot),
        }, expect=None)

    def ready_to_verify(self, result):
        self.assertEqual(result["exit_code"], 220, "invalid proof must reach the verifier")

    def rotation(self, signer, target=None):
        return self.call("submitRotate", {
            "proof": "00", "publicInputs": rotate_inputs(
                committee(signer), committee(signer + 1), target or signer + 1)
        }, expect=None)

    def hash(self, header):
        value = self.call("testHash", {"header": header.hex()})["response"]["hash"]
        return int(value, 0).to_bytes(32, "big")

    def headers(self, n=5):
        out = []
        parent = bytes(32)
        for i in range(n):
            header = bytes([0xe2, 0xa0]) + parent + bytes([i])
            out.append(header)
            parent = self.hash(header)
        return out

    def checkpoint(self, headers, slot=None, period=None, signed=True, expect=0):
        period = PERIOD_NOW if period is None else period
        slot = slot_in(period, 200) if slot is None else slot
        return self.call("reAnchorCheckpoint", {
            "committeeCommitment": str(committee(period)), "period": period,
            "finalizedSlot": slot, "finalizedBeaconRoot": "0x1234",
            "executionBlockHash": "0x" + self.hash(headers[-1]).hex(),
        }, signed=signed, expect=expect)

    def ancestry(self, headers, expect=0):
        return self.call("submitAncestry", {"headerRlps": [h.hex() for h in headers]},
                         expect=expect)

    def trust(self):
        return self.call("getTrustState")["response"]

    def proven(self, header):
        h = self.hash(header)
        packed = h[:16][::-1] + h[16:][::-1]
        return self.call("isProvenExecutionBlockHash", {
            "blockHash": "0x" + packed.hex()})["response"]["value0"] in (True, "true")

    def test_current_signer_reaches_the_verifier(self):
        self.ready_to_verify(self.submit(FIRST, committee(FIRST)))

    def test_previous_signer_has_exactly_two_hours_on_all_supported_networks(self):
        for chain, genesis in GENESIS.items():
            self.bootstrap(FIRST)
            self.call("testSetChain", {"chainId": chain})
            edge = genesis + (FIRST + 1) * PERIOD_SECONDS + GRACE_SECONDS
            self.set_clock(edge - 1)
            self.ready_to_verify(self.submit(FIRST, committee(FIRST)))
            self.ready_to_verify(self.rotation(FIRST))
            self.set_clock(edge)
            self.assertEqual(self.submit(FIRST, committee(FIRST))["exit_code"], ERR_COMMITTEE_TOO_OLD)
            self.assertEqual(self.rotation(FIRST)["exit_code"], ERR_COMMITTEE_TOO_OLD)

    def test_late_installation_does_not_renew_the_deadline(self):
        self.set_clock(GENESIS[SEPOLIA] + (FIRST + 1) * PERIOD_SECONDS + GRACE_SECONDS)
        self.bootstrap(FIRST)
        self.assertEqual(self.submit(FIRST, committee(FIRST))["exit_code"], ERR_COMMITTEE_TOO_OLD)

    def test_no_step_or_rotation_after_three_days_without_recovery(self):
        self.set_clock(NOW + 3 * 86400)
        self.assertEqual(self.submit(FIRST, committee(FIRST))["exit_code"], ERR_COMMITTEE_TOO_OLD)
        self.assertEqual(self.rotation(FIRST)["exit_code"], ERR_COMMITTEE_TOO_OLD)

    def test_next_committee_cannot_sign_early(self):
        self.rotate_to(FIRST + 1)
        result = self.call("submitUpdate", {"proof": "00", "publicInputs": step_inputs(
            slot_in(FIRST, SLOTS_PER_PERIOD - 1), committee(FIRST + 1))}, expect=None)
        self.assertEqual(result["exit_code"], 261)
        self.assertEqual(self.rotation(FIRST + 1)["exit_code"], 261)

    def test_future_header_is_refused(self):
        self.assertEqual(self.submit(FIRST + 1, committee(FIRST))["exit_code"], 261)

    def test_previous_signer_survives_preparation_of_next_committee(self):
        self.bootstrap(FIRST - 1)
        self.rotate_to(FIRST + 1)
        self.ready_to_verify(self.submit(FIRST - 1, committee(FIRST - 1)))
        self.set_clock(GENESIS[SEPOLIA] + FIRST * PERIOD_SECONDS + GRACE_SECONDS)
        self.assertEqual(self.submit(FIRST - 1, committee(FIRST - 1))["exit_code"], ERR_COMMITTEE_TOO_OLD)

    def test_finalized_after_attested_is_refused(self):
        result = self.submit(FIRST, committee(FIRST), slot_in(FIRST, 101))
        self.assertEqual(result["exit_code"], ERR_BAD_PUBLIC_INPUTS)

    def test_held_signer_cannot_register_a_root_behind_the_head(self):
        self.call("testSetHead", {"slot": slot_in(FIRST, 150)})
        self.assertEqual(self.submit(FIRST, committee(FIRST))["exit_code"], 244)

    def test_unknown_signer_is_refused(self):
        self.assertEqual(self.submit(FIRST, committee(FIRST) + 10)["exit_code"], ERR_WRONG_COMMITTEE)

    def test_rotation_must_be_consecutive(self):
        self.assertEqual(self.rotation(FIRST, FIRST + 2)["exit_code"], ERR_BAD_PUBLIC_INPUTS)

    def test_unknown_network_rejects_proofs_and_checkpoint(self):
        self.call("testSetChain", {"chainId": 31337})
        self.assertEqual(self.submit(FIRST, committee(FIRST))["exit_code"], ERR_UNKNOWN_L1)
        self.assertEqual(self.rotation(FIRST)["exit_code"], ERR_UNKNOWN_L1)
        self.checkpoint(self.headers(), expect=ERR_UNKNOWN_L1)

    def test_before_genesis_rejects_instead_of_disabling_time_checks(self):
        self.set_clock(GENESIS[SEPOLIA] - 1)
        self.assertEqual(self.submit(FIRST, committee(FIRST))["exit_code"], 261)

    def test_only_owner_can_set_a_checkpoint(self):
        result = self.checkpoint(self.headers(), signed=False, expect=None)
        self.assertNotEqual(result["exit_code"], 0)
        self.assertFalse(self.trust()["checkpointReady"] in (True, "true"))

    def test_checkpoint_restores_trust_and_opens_real_ancestry_after_outage(self):
        headers = self.headers()
        self.set_clock(NOW + 3 * 86400)
        self.ancestry(headers, expect=ERR_COMMITTEE_TOO_OLD)
        period = (NOW + 3 * 86400 - GENESIS[SEPOLIA]) // PERIOD_SECONDS
        self.checkpoint(headers, slot=period * SLOTS_PER_PERIOD + 100, period=period, expect=ERR_COMMITTEE_TOO_OLD)
        now_slot = (NOW + 3 * 86400 - GENESIS[SEPOLIA]) // 12
        self.checkpoint(headers, slot=now_slot - 64, period=period)
        self.assertTrue(self.proven(headers[-1]))
        self.ancestry(headers)
        for header in headers:
            self.assertTrue(self.proven(header))
        result = self.call("submitUpdate", {"proof": "00", "publicInputs": step_inputs(
            now_slot - 1, committee(period), now_slot - 32)}, expect=None)
        self.ready_to_verify(result)

    def test_expired_client_requires_complete_checkpoint_not_a_committee_only_reset(self):
        headers = self.headers()
        self.checkpoint(headers)
        self.set_clock(NOW + 3 * 86400)
        now_slot = (NOW + 3 * 86400 - GENESIS[SEPOLIA]) // 12
        period = now_slot // SLOTS_PER_PERIOD
        args = {"committeeCommitment": str(committee(period)), "period": period}
        self.call("setCommitteeCommitment", args, signed=True, expect=262)
        self.call("disableOwnerRotation", signed=True)
        self.call("reAnchorCommittee", args, signed=True, expect=262)
        expired = self.call("submitUpdate", {"proof": "00", "publicInputs": step_inputs(
            slot_in(FIRST, 300), committee(FIRST), slot_in(FIRST, 236))}, expect=None)
        self.assertEqual(expired["exit_code"], ERR_COMMITTEE_TOO_OLD)
        self.assertEqual(self.rotation(FIRST)["exit_code"], ERR_COMMITTEE_TOO_OLD)
        self.checkpoint(headers, slot=now_slot - 64, period=period)
        self.ancestry(headers)
        result = self.call("submitUpdate", {"proof": "00", "publicInputs": step_inputs(
            now_slot - 1, committee(period), now_slot - 32)}, expect=None)
        self.ready_to_verify(result)

    def test_fabricated_middle_header_rolls_back_every_admission(self):
        headers = self.headers()
        self.checkpoint(headers)
        bad = list(headers)
        bad[1] = bad[1][:-1] + b"\x7f"
        self.ancestry(bad, expect=250)
        self.assertFalse(self.proven(headers[-2]))
        self.assertFalse(self.proven(bad[1]))
        self.ancestry(headers)
        self.assertTrue(self.proven(headers[0]))

    def test_untrusted_endpoint_is_refused(self):
        self.ancestry(self.headers(), expect=249)

    def test_cached_hash_without_checkpoint_provenance_is_refused(self):
        headers = self.headers()
        self.call("testCacheHash", {"hash": "0x" + self.hash(headers[-1]).hex(), "slot": slot_in(FIRST)})
        self.ancestry(headers, expect=262)

    def test_backfill_can_continue_from_a_verified_intermediate_header(self):
        headers = self.headers(8)
        self.checkpoint(headers)
        self.ancestry(headers[4:])
        self.ancestry(headers[:5])
        self.assertTrue(self.proven(headers[0]))

    def test_committee_only_updates_reject_future_or_empty_committee(self):
        args = {"committeeCommitment": str(committee(FIRST + 1)), "period": FIRST + 1}
        self.call("setCommitteeCommitment", args, signed=True, expect=ERR_STALE_PERIOD)
        self.call("setCommitteeCommitment", {"committeeCommitment": "0", "period": FIRST},
                  signed=True, expect=243)
        self.call("disableOwnerRotation", signed=True)
        self.call("reAnchorCommittee", args, signed=True, expect=ERR_STALE_PERIOD)

    def test_committee_reset_closes_old_ancestry_endpoints(self):
        headers = self.headers()
        self.checkpoint(headers)
        self.call("setCommitteeCommitment", {"committeeCommitment": str(committee(FIRST)),
                                             "period": FIRST}, signed=True)
        self.ancestry(headers, expect=262)
        self.checkpoint(headers)
        self.ancestry(headers)

    def test_recovery_history_keeps_distinct_checkpoint_hashes(self):
        first = self.headers()
        second = first[:-1] + [first[-1][:-1] + b"\x7f"]
        self.checkpoint(first)
        self.checkpoint(second, slot=slot_in(FIRST, 201))
        self.call("testUpgrade", {"keepRecovery": True})
        self.set_clock(NOW)
        generation = 0
        for headers in (first, second):
            receipt = self.call("nextRecoveryHash", {"afterGeneration": generation})["response"]
            self.assertGreater(int(receipt["generation"]), generation)
            generation = int(receipt["generation"])
            self.assertEqual(int(receipt["executionBlockHash"], 0),
                             int.from_bytes(self.hash(headers[-1]), "big"))
        end = self.call("nextRecoveryHash", {"afterGeneration": generation})["response"]
        self.assertEqual(int(end["generation"]), 0)

    def test_checkpoint_reset_does_not_keep_unrelated_old_endpoints(self):
        old = self.headers()
        self.checkpoint(old)
        other = old[:-1] + [old[-1][:-1] + b"\x7f"]
        self.checkpoint(other, slot=slot_in(FIRST, 201))
        self.ancestry(old, expect=262)
        self.ancestry(other)

    def test_checkpoint_requires_current_committee_fresh_slot_and_nonzero_hashes(self):
        headers = self.headers()
        self.checkpoint(headers, period=FIRST - 1, expect=ERR_STALE_PERIOD)
        self.checkpoint(headers, slot=slot_in(FIRST, 400), expect=261)
        self.checkpoint(headers, slot=slot_in(FIRST - 1), expect=ERR_COMMITTEE_TOO_OLD)

    def test_upgrade_closes_cached_endpoints_until_a_new_checkpoint(self):
        headers = self.headers()
        self.checkpoint(headers)
        self.call("testUpgrade", {"keepRecovery": False})
        self.set_clock(NOW)
        self.assertTrue(self.proven(headers[-1]))
        self.ancestry(headers, expect=262)
        self.checkpoint(headers)
        self.ancestry(headers)

    def test_checkpoint_never_moves_the_head_backwards(self):
        headers = self.headers()
        self.checkpoint(headers)
        self.checkpoint(headers, slot=slot_in(FIRST, 199), expect=244)


if __name__ == "__main__":
    unittest.main()
