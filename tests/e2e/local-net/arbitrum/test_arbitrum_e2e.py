"""Arbitrum Sepolia e2e: a fresh AckiNackiBridge on Arbitrum Sepolia, 2 USDC deposited through
it to the local Acki Nacki network with `ackinacki-bridge deposit`, and the same 2 USDC
withdrawn back with `ackinacki-bridge withdraw`.

Nothing is accepted by hand. The one trusted input on the Acki Nacki side is the Sepolia
light client's committee, proven from the beacon chain and set once (section 5); then the
owner's anchor and rotation powers are switched off and asserted off. From there Sepolia
blocks are accepted only through the light client's proofs, and Arbitrum blocks only through
ArbitrumBlockVerifier from those Sepolia blocks.

The run is ordered for time. The bridge's genesis is the network's start (seq_no 0), so it is
deployed and the deposit sent as soon as the network is up; the light client is set up while
the deposit waits for its Arbitrum assertion (half an hour at least), and daemon-live proves
the network's 16384-block bundles from the start.

The tests run in file order and pytest stops at the first failure (-x): each one is a step
of the scenario and asserts what that step must have produced, from the chains or from the
logs of the job that did it. A failure names the step, what it waited for and the tail of
the log; every log of the run stays on the prover host in run-logs/<run>/.
"""

import hashlib
import json
import os
import re
import time
from datetime import datetime, timezone

from conftest import (ARB_CHAIN, ARB_ROLLUP, ARB_USDC, PERIOD_SLOTS, REPO, SEPOLIA_BEACON_GENESIS,
                      SEPOLIA_CHAIN, SLOT_SECS, env, evm_address, kv, tail, wait_for)

AMOUNT = 2_000_000          # base units: 2 USDC, there and back
GENESIS_SECS = 600          # the genesis at seq_no 0 reads nothing from the chain
ANCHOR_SECS = 7200          # one or two Arbitrum Sepolia assertion intervals, plus the parent block
COVERAGE_SECS = 7800        # the next 16384-block boundary (~90 min) and its proof; past the CLI's own 120 min
RUN_SECS = 4 * 3600         # how long a run may take, for the committee-period check
LC_PROOF_SECS = 3600        # the step key's generation, or one light-client step proof


# ── 0 · the run ─────────────────────────────────────────────────────────────


def test_00_clean_previous_deployment(ph):
    """Every earlier deployment is gone; earlier runs' logs stay."""
    first = ph("clean")
    print("  " + first.strip())
    again = ph("clean").strip()
    assert re.search(r"removed containers=0 networks=0 run-dirs=0", again), \
        f"a second clean still found something: {again}"


def test_01_deployer_key_matches_its_address(arb, ctx):
    """The key in ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY signs for ARBITRUM_SEPOLIA_E2E_ADDRESS, which
    holds gas and at least the 2 USDC the round trip moves."""
    address = env("ARBITRUM_SEPOLIA_E2E_ADDRESS").lower()
    assert evm_address(env("ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY")) == address, \
        "ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY is not the key of ARBITRUM_SEPOLIA_E2E_ADDRESS"
    eth, usdc = arb.eth(address), arb.usdc(address)
    print(f"  {address}: {eth / 1e18:.5f} ETH, {usdc / 1e6} USDC")
    assert eth >= 5 * 10**15, f"{address} has {eth / 1e18} ETH on Arbitrum Sepolia; a run needs ~0.005"
    assert usdc >= AMOUNT, f"{address} has {usdc / 1e6} USDC on Arbitrum Sepolia; the run moves 2"
    ctx.update(address=address, usdc_before=usdc)


def test_02_no_sepolia_committee_change_during_the_run():
    """The Sepolia sync committee stays the same for the whole run. The light client follows a
    new committee only through a rotate proof, which does not run live yet, or by the owner's
    hand, which this test does not allow: a run across the change would stall."""
    slot = int((time.time() - SEPOLIA_BEACON_GENESIS) // SLOT_SECS)
    left = (PERIOD_SLOTS - slot % PERIOD_SLOTS) * SLOT_SECS
    change = datetime.fromtimestamp(time.time() + left, timezone.utc)
    print(f"  Sepolia period {slot // PERIOD_SLOTS}, next committee at {change:%F %H:%M} UTC")
    assert left > RUN_SECS, (f"the Sepolia sync committee changes in {left // 60} min, at {change:%H:%M} UTC; "
                             f"start the run after that")


# ── 1 · the network ─────────────────────────────────────────────────────────


def test_10_local_network_bridge_active(an):
    """The zerostate bridge is up at the version this commit ships."""
    got = an("bridge")
    assert got.get("BRIDGE_ACTIVE") == "true", f"eccUSDCBridge is not Active: {got}"
    assert got.get("BRIDGE_VERSION") == "1.14.0", f"eccUSDCBridge is {got.get('BRIDGE_VERSION')}, not 1.14.0"


def test_11_build_on_prover_host(ph, sha):
    """The commit's binaries, built once per commit on the prover host."""
    print("  " + ph("build", sha, timeout=3600).strip().splitlines()[-1])


def test_12_prover_host_reaches_the_network(ph, run, tunnels):
    """Through the tunnels and the proxies, the jobs see GraphQL and every node."""
    print("  " + ph("net-up", run.name, run.sha).strip())
    run.job("probe")
    run.finish("probe", 120)
    got = kv(run.get("probe.env"))
    assert got.get("GRAPHQL_OK") == "true", "GraphQL behind nginx0 does not answer\n" + tail(run.log("probe"))
    closed = [n for n in range(5) if got.get(f"NODE{n}_8600") != "open"]
    assert not closed, f"node REST ports closed: {closed}"


# ── 2 · contracts ───────────────────────────────────────────────────────────


def test_20_genesis_from_the_network(run, ctx):
    """The bridge's genesis is the network's own start, seq_no 0, at L2 anchoring as the live
    relayer runs: zero anchor, and the zerostate's keepers as the BK set (genesis.sh). Nothing
    waits for a key block, so the bridge is deployed and the deposit sent right away."""
    # The keepers the zerostate generator put in the zerostate. Not config/block_keeper*_bls
    # files: acki-nacki ships nine of them, and a five-keeper network regenerates only 0..4.
    with open(os.path.join(env("ACKI_CONFIG"), "keys_config.json")) as f:
        nodes = json.load(f)["bk_nodes"]
    bk_set = {i: nodes[i]["bls_pubkey"] for i in sorted(nodes, key=int)}
    assert bk_set, "keys_config.json in ACKI_CONFIG records no bk_nodes"
    print(f"  {len(bk_set)} block keepers in the zerostate")
    run.job("genesis", stdin=json.dumps(bk_set))
    run.finish("genesis", GENESIS_SECS)
    g = kv(run.get("genesis.env"))
    assert int(g["GENESIS_SEED_SEQNO"]) == 0 and int(g["BUNDLE_STRIDE"]) == 16384, g
    assert int(g["GENESIS_PREV_MAX_LEVEL_LAYER_HASH"], 16) == 0 and int(g["GENESIS_LAST_BK_SET_UPDATE_SEQNO"]) == 0, g
    assert int(g["GENESIS_BK_SET_COMMITMENT"], 16) != 0, g
    ctx.update(genesis=g, bk_signers=len(bk_set))


def test_21_evm_contracts_deployed(run, arb, ctx):
    """A new AckiNackiBridge and its oracle and four verifiers have code on Arbitrum Sepolia,
    and the bridge holds the configuration this run gave it."""
    run.job("deploy", stdin=env("ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY"))
    run.finish("deploy", 1200)
    evm = kv(run.get("evm.env"))
    bridge = evm["BRIDGE_ADDRESS"]
    print(f"  AckiNackiBridge https://sepolia.arbiscan.io/address/{bridge}")
    contracts = {"AckiNackiBridge": bridge, **{k[5:]: v for k, v in evm.items() if k.startswith("ADDR_")}}
    assert len(contracts) >= 6, f"expected the bridge, the oracle and four verifiers: {contracts}"
    empty = [name for name, a in contracts.items() if arb.code(a) in ("0x", "")]
    assert not empty, f"no code at {empty}"
    g = ctx["genesis"]
    assert arb.call(bridge, "usdc()") == int(ARB_USDC, 16), "the bridge does not take Circle's USDC"
    assert arb.call(bridge, "bundleStride()") == int(g["BUNDLE_STRIDE"])
    assert arb.call(bridge, "bridgeWithdrawalAccFr()") == int("1a" * 32, 16)
    ctx.update(bridge=bridge, deploy_block=int(evm["BRIDGE_DEPLOY_BLOCK"]))


def test_22_l2_verifier_deployed(an, ctx):
    """The bridge deploys ArbitrumBlockVerifier for Arbitrum Sepolia from the zerostate's code."""
    got = an("verifier", ARB_CHAIN, SEPOLIA_CHAIN, ARB_ROLLUP)
    assert got.get("VERIFIER", "").startswith("0:"), f"getL2Verifier({ARB_CHAIN}) records no verifier: {got}"
    assert int(got["VERIFIER_PARENT"]) == SEPOLIA_CHAIN, got
    assert (int(got["VERIFIER_L2"]), int(got["VERIFIER_L1"])) == (ARB_CHAIN, SEPOLIA_CHAIN), got
    assert int(got["VERIFIER_ROLLUP"], 16) == int(ARB_ROLLUP, 16), got
    ctx["verifier"] = got["VERIFIER"]


def test_23_trust_set(an, arb, ctx):
    """Acki Nacki trusts the new bridge for 421614, and the bridge holds the genesis anchor."""
    got = an("trust", ctx["bridge"])
    assert got.get("TRUSTED") == "true", f"isTrustedL1Bridge({ARB_CHAIN}, bridge) is false after setTrustedL1Bridge"
    g = ctx["genesis"]
    assert arb.call(ctx["bridge"], "storedBkSetCommitment()") == int(g["GENESIS_BK_SET_COMMITMENT"], 16)
    assert arb.call(ctx["bridge"], "storedPrevMaxLevelLayerHash()") == int(g["GENESIS_PREV_MAX_LEVEL_LAYER_HASH"], 16)
    assert arb.call(ctx["bridge"], "storedLastSeenBlockSeqNo()") == int(g["GENESIS_SEED_SEQNO"])


# ── 3 · deposit services, right away ────────────────────────────────────────


def owner_keys():
    with open(os.path.join(env("ACKI_CONFIG"), "USDCBridge.keys.json")) as f:
        return f.read().replace("\n", "")


def test_30_sender_keys_funded(run, arb):
    """The relayer and the withdrawal's burner have gas of their own, so no two senders share a nonce."""
    run.job("fund", stdin=env("ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY"))
    run.finish("fund", 600)
    f = kv(run.get("fund.env"))
    for role in ("RELAYER", "WITHDRAW_BURNER"):
        assert arb.eth(f[f"{role}_ADDRESS"]) > 0, f"{role.lower()} {f[f'{role}_ADDRESS']} has no ETH"


def test_31_deposit_services_started(run):
    """daemon-live and arbitrum-anchors start and say so in their logs. arbitrum-anchors has to
    run before the deposit: it takes the slot proofs when the deposit's batch lands, a minute or
    two after the deposit."""
    run.job("relayer", stdin=env("ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY"))
    run.job("anchors", stdin=owner_keys())
    # The texts relayer.rs logs at startup (`startup: bridge bundleStride matches`,
    # `daemon-live starting`); its later proving output pushes them out of a log tail fast.
    wait_for("daemon-live reads the bridge's bundle stride", lambda: "bundleStride matches" in run.log("relayer"),
             600, log=lambda: run.log("relayer", 200))
    wait_for("daemon-live starts its loop", lambda: "daemon-live starting" in run.log("relayer"),
             900, log=lambda: run.log("relayer", 200))
    # relayer.rs logs `startup: Cold — contract at genesis, bootstrapping policy=Explicit(0)`.
    assert "Explicit(0)" in run.log("relayer"), \
        "daemon-live did not start from the genesis seed\n" + tail(run.log("relayer", 200))
    wait_for("arbitrum-anchors finds the verifier", lambda: "ArbitrumBlockVerifier of Arbitrum Sepolia" in run.log("anchors"),
             300, log=lambda: run.log("anchors"))
    wait_for("arbitrum-anchors starts", lambda: "arbitrum-anchors starting" in run.log("anchors"),
             300, log=lambda: run.log("anchors"))
    for job in ("relayer", "anchors"):
        assert run.wait(job, 1) is None, f"{job} exited\n" + tail(run.log(job))


def test_32_recipient_multisig(an, ctx):
    """An empty multisig on Acki Nacki receives the deposit."""
    got = an("msig")
    ctx.update(msig=got["MSIG"], msig_account=got["MSIG_ACCOUNT"], msig_keys=an.cat(got["MSIG_KEYS"]))
    assert int(an("ecc", ctx["msig"])["ECC3"]) == 0


# ── 4 · deposit: ackinacki-bridge deposit ───────────────────────────────────


def steps(run, job):
    """The CLI's --json step events so far: {step: (state, detail)} of the latest of each."""
    out = {}
    for line in run.log(job).splitlines():
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if isinstance(e, dict) and e.get("event") == "step":
            out[e["step"]] = (e["state"], e.get("detail", ""))
    return out


def step_done(run, step, what, timeout):
    def check():
        state = steps(run, "cli-deposit").get(step, ("", ""))
        assert state[0] != "failed", f"deposit step {step} failed: {state[1]}\n" + tail(run.log("cli-deposit"))
        code = run.wait("cli-deposit", 1)
        assert code in (None, 0), f"ackinacki-bridge deposit exited {code} before {step}\n" + tail(run.log("cli-deposit"))
        return state if state[0] in ("done", "skipped") else None
    return wait_for(what, check, timeout, log=lambda: run.log("cli-deposit"))


def test_40_deposit_sent(run, ctx):
    """The CLI sends approve and deposit from ARBITRUM_SEPOLIA_E2E_ADDRESS."""
    run.job("cli-deposit", stdin=env("ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY"),
            FROM=ctx["address"], TO=ctx["msig"], AMOUNT="2")
    state = step_done(run, "deposit", "the deposit transaction is sent", 900)
    print(f"  deposit https://sepolia.arbiscan.io/tx/{state[1]}")


def test_41_deposit_event_received(run, ctx):
    """The deposit is mined, confirmed, and its Deposit event is found."""
    state = step_done(run, "evm_confirm", "the Deposit event is received", 900)
    assert state[1].startswith("depositId"), state


# ── 5 · the light client, while the deposit waits for its assertion ────────


def contract_vk_blob_sha256():
    """sha256 of the VK_BLOB the light client contract verifies step proofs with."""
    path = os.path.join(REPO, "contracts", "an", "exchange", "EthBeaconLightClient.sol")
    m = re.search(r'bytes constant VK_BLOB\s*=((?:\s*hex"[0-9a-fA-F]*")+)\s*;', open(path).read())
    assert m, f"no `bytes constant VK_BLOB` literal in {path}"
    return hashlib.sha256(bytes.fromhex("".join(re.findall(r'hex"([0-9a-fA-F]*)"', m.group(1))))).hexdigest()


def test_50_light_client_key_generated(run):
    """The light client's step proving key is generated once, before anything is deployed, and its
    VK is the one the light client contract verifies with. Every proof of the run reads it."""
    run.job("lc-keygen")
    run.finish("lc-keygen", LC_PROOF_SECS)
    got = kv(run.get("lc-keygen.env"))
    print(f"  proving key {int(got['LC_PK_BYTES']) / 2**30:.1f} GiB, VK blob sha256 {got['LC_VK_BLOB_SHA256']}")
    assert got["LC_VK_BLOB_SHA256"] == contract_vk_blob_sha256(), \
        "the generated step VK is not EthBeaconLightClient's VK_BLOB: its proofs would not verify"


def test_51_light_client_proof_started(run):
    """eth-lc-relayer prove-one starts proving the latest Sepolia finality update with that key:
    its committee is the light client's one trusted input."""
    run.job("lc-prove")
    time.sleep(30)
    assert run.wait("lc-prove", 1) in (None, 0), "lc-prove exited\n" + tail(run.log("lc-prove"))


def test_52_light_client_deployed(an, ctx):
    """The bridge deploys EthBeaconLightClient for Sepolia, owned by the bridge owner's key, with
    no committee yet."""
    got = an("lightclient")
    assert "::" in got.get("LC", ""), f"getLightClient({SEPOLIA_CHAIN}) names no light client: {got}"
    assert int(got["LC_L1"]) == SEPOLIA_CHAIN, got
    assert int(got["LC_OWNER"], 16) == int(got["OWNER_PUBKEY"], 16), got
    assert an("lc", got["LC"]).get("LC_COMMITTEE") == "0x" + "0" * 64, "the light client has a committee already"
    ctx["lc"] = got["LC"]


def test_53_light_client_committee_set_once(run, an, ctx):
    """The committee lc-prove proved becomes the light client's, once, with the Gloas fork slot."""
    run.finish("lc-prove", LC_PROOF_SECS)
    proved = kv(run.get("lc.env"))
    assert int(proved["LC_COMMITMENT"], 16) != 0, proved
    run.job("lc-bootstrap", stdin=owner_keys(), LC=ctx["lc"])
    run.finish("lc-bootstrap", 900)
    got = an("lc", ctx["lc"])
    assert int(got["LC_COMMITTEE"], 16) == int(proved["LC_COMMITMENT"], 16), (got, proved)
    assert int(got["LC_PERIOD"]) == int(proved["LC_PERIOD"]), (got, proved)
    assert got["LC_GLOAS_SET"] == "true" and int(got["LC_GLOAS_SLOT"]) == 11296768, got
    print(f"  committee of period {got['LC_PERIOD']}: {got['LC_COMMITTEE']}")
    ctx["lc_period"] = int(got["LC_PERIOD"])


def test_54_owner_cannot_accept_blocks_or_change_the_committee(an, ctx):
    """From here on no block is accepted by hand: the owner's anchors are off for Sepolia (the
    flip in lc-bootstrap) and for Arbitrum Sepolia, and the light client's owner rotation is off."""
    assert an("disable-owner-anchors", ARB_CHAIN).get("OWNER_ANCHORS") == "false", \
        f"isOwnerAnchorsEnabled({ARB_CHAIN}) is still true after disableOwnerAnchorsOn"
    assert an("owner-anchors", SEPOLIA_CHAIN).get("OWNER_ANCHORS") == "false", \
        f"isOwnerAnchorsEnabled({SEPOLIA_CHAIN}) is still true after flip-owner"
    assert an("lc", ctx["lc"]).get("LC_OWNER_ROTATION") == "false", "the light client's owner rotation is on"


def test_55_light_client_relayer_started(run, an, ctx):
    """eth-lc-relayer starts, and the light client accepts its first Sepolia checkpoint."""
    # Any key signs submitUpdate, submitAncestry and proveBlock (they are permissionless); the
    # owner's is at hand in the config, and after test_54 it can no longer accept anything.
    run.job("lc-relayer", stdin=owner_keys(), LC=ctx["lc"])
    wait_for("eth-lc-relayer gets its first checkpoint accepted",
             lambda: "submitUpdate accepted" in run.log("lc-relayer"), LC_PROOF_SECS, log=lambda: run.log("lc-relayer"))
    got = an("lc", ctx["lc"])
    assert int(got["LC_UPDATES"]) >= 1 and int(got["LC_PERIOD"]) == ctx["lc_period"], got
    for job in ("lc-relayer", "relayer", "anchors"):
        assert run.wait(job, 1) is None, f"{job} exited\n" + tail(run.log(job))


# ── 6 · the deposit lands ───────────────────────────────────────────────────


def test_60_light_client_accepts_spans(run):
    """The light client accepts the Sepolia blocks between two checkpoints (submitAncestry): an
    Arbitrum assertion's Sepolia block is one of those."""
    wait_for("eth-lc-relayer gets a span accepted", lambda: "submitAncestry accepted" in run.log("lc-relayer"),
             2 * LC_PROOF_SECS, log=lambda: run.log("lc-relayer"))
    assert "span given up" not in run.log("lc-relayer"), \
        "eth-lc-relayer gave a span up: its blocks will never be accepted\n" + tail(run.log("lc-relayer"))


def test_61_deposit_block_anchored(run, an):
    """The deposit's Arbitrum block reaches the anchor set through ArbitrumBlockVerifier, from an
    assertion in a Sepolia block the light client accepted."""
    state = step_done(run, "anchor", "the deposit block is accepted on Acki Nacki", ANCHOR_SECS)
    assert an("accepted", ARB_CHAIN, state[1]).get("ACCEPTED") == "true", f"isAcceptedBlockHash({ARB_CHAIN}, {state[1]})"
    log = run.log("anchors")
    assert "proveBlock sent" in log and "the bridge accepts the deposit block" in log, \
        "arbitrum-anchors did not get the block accepted through proveBlock\n" + tail(log)


def test_62_deposit_proof_generated(run):
    """The deposit prover builds the proof."""
    step_done(run, "prove", "the deposit proof is generated", 3600)


def test_63_deposit_finalized_on_acki_nacki(run):
    """finalizeDeposit is sent to Acki Nacki and accepted."""
    step_done(run, "finalize", "finalizeDeposit is confirmed", 900)


def test_64_deposit_minted(run):
    """The voucher confirms the deposit: USDC is minted on Acki Nacki."""
    state = step_done(run, "credit", "the deposit is minted (confirmDeposit)", 900)
    assert state[1].startswith("confirmDeposit"), state


def test_65_deposit_delivered(run, an, ctx):
    """The CLI ends well, and the 2 USDC are on the multisig."""
    run.finish("cli-deposit", 600)
    summary = [json.loads(line) for line in run.log("cli-deposit").splitlines()
               if line.startswith("{") and '"confirmation"' in line]
    assert summary and summary[-1]["confirmation"].get("delivery_tx"), \
        "no delivery transaction in the CLI's summary\n" + tail(run.log("cli-deposit"))
    assert int(an("ecc", ctx["msig"])["ECC3"]) == AMOUNT, "the multisig does not hold the 2 USDC"


# ── 7 · withdrawal: ackinacki-bridge withdraw ───────────────────────────────


def log_line(run, pattern, what, timeout):
    def check():
        text = run.log("cli-withdraw")
        code = run.wait("cli-withdraw", 1)
        m = re.search(pattern, text)
        assert m or code in (None, 0), f"ackinacki-bridge withdraw exited {code} before: {what}\n" + tail(text)
        return m
    return wait_for(what, check, timeout, log=lambda: run.log("cli-withdraw"))


def test_70_burn_on_acki_nacki(run, ctx):
    """The CLI burns the 2 USDC on Acki Nacki."""
    run.job("cli-withdraw", stdin=env("ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY") + "\n" + ctx["msig_keys"].replace("\n", ""),
            FROM=ctx["msig"], TO=ctx["address"], AMOUNT="2")
    log_line(run, r"burn broadcast an_tx=", "the burn is sent", 900)


def test_71_withdrawal_event_captured(run):
    """The bridge emits WithdrawalInitiated and the CLI captures it."""
    log_line(run, r"captured WithdrawalInitiated event", "WithdrawalInitiated is captured", 900)


def test_72_burn_bundle_verified(run):
    """daemon-live gets the bundle that holds the burn verified on Arbitrum Sepolia."""
    log_line(run, r"stage 5/6", "verifyBlock covers the burn's bundle", COVERAGE_SECS)


def test_73_withdrawal_proof_generated(run):
    """Circuit 4 and its SHPLONK aggregation prove the withdrawal."""
    log_line(run, r"capture \+ prove complete", "the withdrawal proof is generated", 3600)


def test_74_withdrawal_paid(run):
    """withdrawByProof is sent and confirmed on Arbitrum Sepolia."""
    m = log_line(run, r"withdrawByProof paid out tx=(0x[0-9a-fA-F]+)", "withdrawByProof is confirmed", 1800)
    print(f"  withdrawByProof https://sepolia.arbiscan.io/tx/{m.group(1)}")
    run.finish("cli-withdraw", 300)


def test_75_usdc_delivered(arb, an, ctx):
    """The 2 USDC are back on ARBITRUM_SEPOLIA_E2E_ADDRESS, and the multisig is empty."""
    after = arb.usdc(ctx["address"])
    assert after == ctx["usdc_before"], \
        f"{ctx['address']} holds {after / 1e6} USDC, {ctx['usdc_before'] / 1e6} before the deposit"
    assert int(an("ecc", ctx["msig"])["ECC3"]) == 0, "the multisig still holds USDC"
