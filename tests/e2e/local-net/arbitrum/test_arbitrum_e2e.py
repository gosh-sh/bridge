"""Arbitrum Sepolia e2e: a fresh AckiNackiBridge on Arbitrum Sepolia, 2 USDC deposited through
it to the local Acki Nacki network with `ackinacki-bridge deposit`, and the same 2 USDC
withdrawn back with `ackinacki-bridge withdraw`.

The tests run in file order and pytest stops at the first failure (-x): each one is a step
of the scenario and asserts what that step must have produced, from the chains or from the
logs of the job that did it. A failure names the step, what it waited for and the tail of
the log; every log of the run stays on the prover host in run-logs/<run>/.
"""

import glob
import json
import os
import re

from conftest import ARB_CHAIN, ARB_ROLLUP, ARB_USDC, SEPOLIA_CHAIN, env, evm_address, kv, tail, wait_for

AMOUNT = 2_000_000          # base units: 2 USDC, there and back
GENESIS_SECS = 1800         # the network needs 1024 blocks before the bridge has a seed
ANCHOR_SECS = 5400          # one or two Arbitrum Sepolia assertion intervals, plus the parent block
COVERAGE_SECS = 5400        # daemon-live proving the bundle that holds the burn


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
    """The bridge's genesis anchors at the network's latest 1024-block boundary."""
    bk_set = {}
    for path in glob.glob(os.path.join(env("ACKI_CONFIG"), "block_keeper*_bls.keys.json")):
        idx = re.search(r"block_keeper(\d+)_bls", path).group(1)
        bk_set[idx] = json.load(open(path))[0]["public"]
    assert bk_set, "no block_keeperN_bls.keys.json in ACKI_CONFIG"
    run.job("genesis", stdin=json.dumps(dict(sorted(bk_set.items(), key=lambda kv_: int(kv_[0])))))
    run.finish("genesis", GENESIS_SECS + 60)
    g = kv(run.get("genesis.env"))
    assert int(g["GENESIS_SEED_SEQNO"]) % 1024 == 0 and int(g["BUNDLE_STRIDE"]) == 1024, g
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


# ── 3 · trust ───────────────────────────────────────────────────────────────

def test_30_trust_set(an, arb, ctx):
    """Acki Nacki trusts the new bridge for 421614, and the bridge holds the genesis anchor."""
    got = an("trust", ctx["bridge"])
    assert got.get("TRUSTED") == "true", f"isTrustedL1Bridge({ARB_CHAIN}, bridge) is false after setTrustedL1Bridge"
    g = ctx["genesis"]
    assert arb.call(ctx["bridge"], "storedBkSetCommitment()") == int(g["GENESIS_BK_SET_COMMITMENT"], 16)
    assert arb.call(ctx["bridge"], "storedPrevMaxLevelLayerHash()") == int(g["GENESIS_PREV_MAX_LEVEL_LAYER_HASH"], 16)
    assert arb.call(ctx["bridge"], "storedLastSeenBlockSeqNo()") == int(g["GENESIS_SEED_SEQNO"])


# ── 4 · relayers ────────────────────────────────────────────────────────────

def test_40_sender_keys_funded(run, arb):
    """The relayer and the withdrawal's burner have gas of their own, so no two senders share a nonce."""
    run.job("fund", stdin=env("ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY"))
    run.finish("fund", 600)
    f = kv(run.get("fund.env"))
    for role in ("RELAYER", "WITHDRAW_BURNER"):
        assert arb.eth(f[f"{role}_ADDRESS"]) > 0, f"{role.lower()} {f[f'{role}_ADDRESS']} has no ETH"


def test_41_relayers_started(run, owner_as_light_client):
    """daemon-live and arbitrum-anchors start and say so in their logs."""
    run.job("relayer", stdin=env("ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY"))
    # Any key signs proveBlock (it is permissionless); the owner's is at hand in the config.
    with open(os.path.join(env("ACKI_CONFIG"), "USDCBridge.keys.json")) as f:
        run.job("anchors", stdin=f.read().replace("\n", ""))
    wait_for("daemon-live reads the bridge's bundle stride", lambda: "bundle stride" in run.log("relayer"),
             600, log=lambda: run.log("relayer"))
    wait_for("daemon-live starts its loop", lambda: "daemon starting" in run.log("relayer"),
             900, log=lambda: run.log("relayer"))
    wait_for("arbitrum-anchors finds the verifier", lambda: "ArbitrumBlockVerifier of Arbitrum Sepolia" in run.log("anchors"),
             300, log=lambda: run.log("anchors"))
    wait_for("arbitrum-anchors starts", lambda: "arbitrum-anchors starting" in run.log("anchors"),
             300, log=lambda: run.log("anchors"))
    for job in ("relayer", "anchors"):
        assert run.wait(job, 1) is None, f"{job} exited\n" + tail(run.log(job))


def test_42_recipient_multisig(an, ctx):
    """An empty multisig on Acki Nacki receives the deposit."""
    got = an("msig")
    ctx.update(msig=got["MSIG"], msig_account=got["MSIG_ACCOUNT"], msig_keys=an.cat(got["MSIG_KEYS"]))
    assert int(an("ecc", ctx["msig"])["ECC3"]) == 0


# ── 5 · deposit: ackinacki-bridge deposit ───────────────────────────────────

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


def test_50_deposit_sent(run, ctx):
    """The CLI sends approve and deposit from ARBITRUM_SEPOLIA_E2E_ADDRESS."""
    run.job("cli-deposit", stdin=env("ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY"),
            FROM=ctx["address"], TO=ctx["msig"], AMOUNT="2")
    state = step_done(run, "deposit", "the deposit transaction is sent", 900)
    print(f"  deposit https://sepolia.arbiscan.io/tx/{state[1]}")


def test_51_deposit_event_received(run, ctx):
    """The deposit is mined, confirmed, and its Deposit event is found."""
    state = step_done(run, "evm_confirm", "the Deposit event is received", 900)
    assert state[1].startswith("depositId"), state


def test_52_deposit_block_anchored(run, an, owner_as_light_client):
    """The deposit's Arbitrum block reaches the anchor set through ArbitrumBlockVerifier."""
    state = step_done(run, "anchor", "the deposit block is accepted on Acki Nacki", ANCHOR_SECS)
    assert owner_as_light_client, "no Sepolia block was owner-anchored, so the verifier proved nothing"
    assert an("accepted", ARB_CHAIN, state[1]).get("ACCEPTED") == "true", f"isAcceptedBlockHash({ARB_CHAIN}, {state[1]})"


def test_53_deposit_proof_generated(run):
    """The deposit prover builds the proof."""
    step_done(run, "prove", "the deposit proof is generated", 3600)


def test_54_deposit_finalized_on_acki_nacki(run):
    """finalizeDeposit is sent to Acki Nacki and accepted."""
    step_done(run, "finalize", "finalizeDeposit is confirmed", 900)


def test_55_deposit_minted(run):
    """The voucher confirms the deposit: USDC is minted on Acki Nacki."""
    state = step_done(run, "credit", "the deposit is minted (confirmDeposit)", 900)
    assert state[1].startswith("confirmDeposit"), state


def test_56_deposit_delivered(run, an, ctx):
    """The CLI ends well, and the 2 USDC are on the multisig."""
    run.finish("cli-deposit", 600)
    summary = [json.loads(line) for line in run.log("cli-deposit").splitlines()
               if line.startswith("{") and '"confirmation"' in line]
    assert summary and summary[-1]["confirmation"].get("delivery_tx"), \
        "no delivery transaction in the CLI's summary\n" + tail(run.log("cli-deposit"))
    assert int(an("ecc", ctx["msig"])["ECC3"]) == AMOUNT, "the multisig does not hold the 2 USDC"


# ── 6 · withdrawal: ackinacki-bridge withdraw ───────────────────────────────

def log_line(run, pattern, what, timeout):
    def check():
        text = run.log("cli-withdraw")
        code = run.wait("cli-withdraw", 1)
        m = re.search(pattern, text)
        assert m or code in (None, 0), f"ackinacki-bridge withdraw exited {code} before: {what}\n" + tail(text)
        return m
    return wait_for(what, check, timeout, log=lambda: run.log("cli-withdraw"))


def test_60_burn_on_acki_nacki(run, ctx):
    """The CLI burns the 2 USDC on Acki Nacki."""
    run.job("cli-withdraw", stdin=env("ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY") + "\n" + ctx["msig_keys"].replace("\n", ""),
            FROM=ctx["msig"], TO=ctx["address"], AMOUNT="2")
    log_line(run, r"burn broadcast an_tx=", "the burn is sent", 900)


def test_61_withdrawal_event_captured(run):
    """The bridge emits WithdrawalInitiated and the CLI captures it."""
    log_line(run, r"captured WithdrawalInitiated event", "WithdrawalInitiated is captured", 900)


def test_62_burn_bundle_verified(run):
    """daemon-live gets the bundle that holds the burn verified on Arbitrum Sepolia."""
    log_line(run, r"stage 5/6", "verifyBlock covers the burn's bundle", COVERAGE_SECS)


def test_63_withdrawal_proof_generated(run):
    """Circuit 4 and its SHPLONK aggregation prove the withdrawal."""
    log_line(run, r"capture \+ prove complete", "the withdrawal proof is generated", 3600)


def test_64_withdrawal_paid(run):
    """withdrawByProof is sent and confirmed on Arbitrum Sepolia."""
    m = log_line(run, r"withdrawByProof paid out tx=(0x[0-9a-fA-F]+)", "withdrawByProof is confirmed", 1800)
    print(f"  withdrawByProof https://sepolia.arbiscan.io/tx/{m.group(1)}")
    run.finish("cli-withdraw", 300)


def test_65_usdc_delivered(arb, an, ctx):
    """The 2 USDC are back on ARBITRUM_SEPOLIA_E2E_ADDRESS, and the multisig is empty."""
    after = arb.usdc(ctx["address"])
    assert after == ctx["usdc_before"], \
        f"{ctx['address']} holds {after / 1e6} USDC, {ctx['usdc_before'] / 1e6} before the deposit"
    assert int(an("ecc", ctx["msig"])["ECC3"]) == 0, "the multisig still holds USDC"
