"""Arbitrum Sepolia e2e: a fresh AckiNackiBridge on Arbitrum Sepolia, 2 USDC and 3 NACKL
deposited through it to the local Acki Nacki network, and 2 USDC and 5 NACKL withdrawn, all
paid by the relayers. `ackinacki-bridge deposit send` makes each deposit and stops once it is
confirmed; the deposit relayer finalizes it. The USDC withdrawal is a burn on Acki Nacki as the
wallet makes it, the NACKL one `ackinacki-bridge withdraw send`; daemon-live finds both, proves
them and pays them out with withdrawByProof. Then the CLI proves each transfer again —
`deposit proof --tx-hash`, `withdraw proof --burn-tx` — and finds it finalized or paid when it
sends: success, already_finalized and already_paid.

Nothing is accepted by hand. The one trusted input on the Acki Nacki side is the Sepolia
light client's committee, proven from the beacon chain and set once (section 5); then the
owner's anchor and rotation powers are switched off and asserted off. From there Sepolia
blocks are accepted only through the light client's proofs, and Arbitrum blocks only through
ArbitrumBlockVerifier from those Sepolia blocks.

The run is ordered for time. The bridge's genesis is the network's start (seq_no 0), so it is
deployed and the deposit sent as soon as the network is up; the light client is set up while
the deposit waits for its Arbitrum assertion (half an hour at least), and daemon-live proves
the network's 16384-block bundles from the start. The withdrawal does not wait for the deposit:
its 2 USDC are minted by the owner for 421614 and burned right away, so the burn lands in the
first bundle, and daemon-live pays it out of the deposit's 2 USDC. The NACKL burn locks its 5
NACKL on the Acki Nacki bridge before the NACKL deposit is finalized, which unlocks 3 of them.

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
import urllib.error
import urllib.request

from conftest import (ARB_AAVE_POOL, ARB_AUSDC, ARB_CHAIN, ARB_ROLLUP, ARB_USDC, REPO, SEPOLIA_CHAIN, env,
                      evm_address, keccak256, kv, tail, wait_for)

AMOUNT = 2_000_000          # base units: 2 USDC, there and back
NACKL = 1_000_000_000
NACKL_SUPPLY = 1000 * NACKL
NACKL_OUT = 5 * NACKL
NACKL_IN = 3 * NACKL
GENESIS_SECS = 600          # the genesis at seq_no 0 reads only the BK set from the chain
ANCHOR_SECS = 7200          # one or two Arbitrum Sepolia assertion intervals, plus the parent block
COVERAGE_SECS = 7800        # the next 16384-block boundary (~90 min) and its proof
LC_PROOF_SECS = 3600        # the step key's generation, or one light-client step proof
# The local network's GraphQL behind nginx0, on this runner's end of the tunnels fixture.
GRAPHQL = "http://127.0.0.1:18080/graphql"


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
    assert got.get("BRIDGE_ACTIVE") == "true", f"eccBridge is not Active: {got}"
    assert got.get("BRIDGE_VERSION") == "1.18.0", f"eccBridge is {got.get('BRIDGE_VERSION')}, not 1.18.0"


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


def graphql(query):
    """The local network's GraphQL answer to `query`, errors included."""
    req = urllib.request.Request(GRAPHQL, json.dumps({"query": query}).encode(),
                                 {"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as answer:
            return json.load(answer)
    except urllib.error.HTTPError as e:  # a query the schema refuses can come back as HTTP 400
        body = e.read()
        try:
            return json.loads(body)
        except ValueError:
            raise AssertionError(f"GraphQL answered HTTP {e.code}: {body[:500]!r}") from e


def test_20_genesis_from_the_network(run, ctx, tunnels):
    """The bridge's genesis is the network's own start, seq_no 0, at L2 anchoring as the live
    relayer runs: zero anchor, and the BK set at 0 read from the network's GraphQL (genesis.sh).
    Nothing waits for a key block, so the bridge is deployed and the deposit sent right away."""
    # compute_bridge_anchors and daemon-live take the BK set from blockchain.latestBkSet: a
    # gql-server without it fails them both, so say that here instead of in a job log.
    got = graphql("{ blockchain { latestBkSet { height } } }")
    errors = [e.get("message", "") for e in got.get("errors") or []]
    assert not errors, ("the local network's GraphQL refuses blockchain.latestBkSet, which "
                        f"compute_bridge_anchors and daemon-live need (gql-server >= 1.5.0): {errors}")
    print(f"  blockchain.latestBkSet: {((got.get('data') or {}).get('blockchain') or {}).get('latestBkSet')}")
    run.job("genesis")
    run.finish("genesis", GENESIS_SECS)
    g = kv(run.get("genesis.env"))
    assert int(g["GENESIS_SEED_SEQNO"]) == 0 and int(g["BUNDLE_STRIDE"]) == 16384, g
    assert int(g["GENESIS_PREV_MAX_LEVEL_LAYER_HASH"], 16) == 0 and int(g["GENESIS_LAST_BK_SET_UPDATE_SEQNO"]) == 0, g
    assert int(g["GENESIS_BK_SET_COMMITMENT"], 16) != 0, g
    ctx.update(genesis=g)


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
    nackl = evm["NACKL_ADDRESS"]
    assert arb.code(nackl) not in ("0x", ""), f"no code at the NACKL token {nackl}"
    assert arb.call(nackl, "owner()") == int(bridge, 16), "the bridge is not the NACKL token's minter"
    assert arb.call(nackl, "decimals()") == 9
    assert arb.words(bridge, "tokenInfo(uint32)", (1).to_bytes(32, "big"))[0] == int(nackl, 16), \
        "the bridge does not list the NACKL token under id 1"
    ctx.update(bridge=bridge, deploy_block=int(evm["BRIDGE_DEPLOY_BLOCK"]), nackl=nackl)


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
    got = an("trust", ctx["bridge"], ctx["nackl"])
    assert got.get("TOKEN_REGISTERED") == "true", "AN USDC token lane is not registered"
    assert got.get("NACKL_REGISTERED") == "true", "AN NACKL token lane is not registered"
    assert got.get("TRUSTED") == "true", f"isTrustedL1Bridge({ARB_CHAIN}, bridge) is false after setTrustedL1Bridge"
    assert int(got.get("EVM_BRIDGE", "0"), 16) == int(ctx["bridge"], 16), \
        f"getEvmBridge({ARB_CHAIN}) is {got.get('EVM_BRIDGE')} after setEvmBridge, want {ctx['bridge']}"
    g = ctx["genesis"]
    assert arb.call(ctx["bridge"], "storedBkSetCommitment()") == int(g["GENESIS_BK_SET_COMMITMENT"], 16)
    assert arb.call(ctx["bridge"], "storedPrevMaxLevelLayerHash()") == int(g["GENESIS_PREV_MAX_LEVEL_LAYER_HASH"], 16)
    assert arb.call(ctx["bridge"], "storedLastSeenBlockSeqNo()") == int(g["GENESIS_SEED_SEQNO"])


# ── 3 · deposit services, right away ────────────────────────────────────────


def owner_keys():
    with open(os.path.join(env("ACKI_CONFIG"), "USDCBridge.keys.json")) as f:
        return f.read().replace("\n", "")


def test_30_sender_keys_funded(run, arb):
    """The relayer has gas of its own for verifyBlock and withdrawByProof: no other sender
    shares its nonce."""
    run.job("fund", stdin=env("ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY"))
    run.finish("fund", 600)
    f = kv(run.get("fund.env"))
    assert arb.eth(f["RELAYER_ADDRESS"]) > 0, f"relayer {f['RELAYER_ADDRESS']} has no ETH"


def test_31_deposit_services_started(run):
    """daemon-live, eth-lc-relayer arbitrum-anchors and deposit-relayer daemon start from their
    images and say so in their logs. arbitrum-anchors has to run before the deposit: it takes
    the slot proofs when the deposit's batch lands, a minute or two after the deposit. The
    deposit relayer finalizes the deposits. daemon-live also pays out every burn that names the
    run's bridge."""
    run.job("relayer", stdin=env("ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY"))
    # The ARB_RPC secret, when set, follows the keys on the job's stdin: ci-entry logs every
    # command line, so a secret never travels as a job argument.
    anchors_rpc = os.environ.get("ANCHORS_ARB_RPC", "")
    run.job("anchors", stdin=owner_keys() + ("\n" + anchors_rpc if anchors_rpc else ""))
    run.job("deposit-relayer", stdin=owner_keys())
    # The texts relayer.rs logs at startup (`startup: bridge bundleStride matches`,
    # `daemon-live starting`); its later proving output pushes them out of a log tail fast.
    wait_for("daemon-live reads the bridge's bundle stride", lambda: "bundleStride matches" in run.log("relayer"),
             600, log=lambda: run.log("relayer", 200))
    wait_for("daemon-live starts its loop", lambda: "daemon-live starting" in run.log("relayer"),
             900, log=lambda: run.log("relayer", 200))
    wait_for("daemon-live starts its withdrawal claimer", lambda: "withdraw claimer starting" in run.log("relayer"),
             300, log=lambda: run.log("relayer", 200))
    # relayer.rs logs `startup: Cold — contract at genesis, bootstrapping policy=Explicit(0)`.
    assert "Explicit(0)" in run.log("relayer"), \
        "daemon-live did not start from the genesis seed\n" + tail(run.log("relayer", 200))
    wait_for("arbitrum-anchors finds the verifier", lambda: "ArbitrumBlockVerifier of Arbitrum Sepolia" in run.log("anchors"),
             300, log=lambda: run.log("anchors"))
    wait_for("arbitrum-anchors starts", lambda: "arbitrum-anchors starting" in run.log("anchors"),
             300, log=lambda: run.log("anchors"))
    wait_for("deposit-relayer starts its loop", lambda: "deposit daemon starting" in run.log("deposit-relayer"),
             600, log=lambda: run.log("deposit-relayer"))
    for job in ("relayer", "anchors", "deposit-relayer"):
        assert run.wait(job, 1) is None, f"{job} exited\n" + tail(run.log(job))


def test_32_recipient_multisig(an, ctx):
    """An empty multisig on Acki Nacki receives the deposit."""
    got = an("msig")
    ctx.update(msig=got["MSIG"], msig_account=got["MSIG_ACCOUNT"], msig_keys=an.cat(got["MSIG_KEYS"]))
    assert int(an("ecc", ctx["msig"])["ECC3"]) == 0


# ── 4 · deposits: ackinacki-bridge deposit send ─────────────────────────────


def steps(run, log):
    """The CLI's --json step events so far in `log`: {step: (state, detail)} of the latest of each."""
    out = {}
    for line in run.log(log).splitlines():
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if isinstance(e, dict) and e.get("event") == "step":
            out[e["step"]] = (e["state"], e.get("detail", ""))
    return out


def summary(run, log, key):
    """The CLI's --json summary in `log`: the last JSON object that carries `key`."""
    for line in reversed(run.log(log).splitlines()):
        if line.startswith("{") and f'"{key}"' in line:
            try:
                return json.loads(line)
            except ValueError:
                continue
    raise AssertionError(f"no summary with {key} in {log}\n" + tail(run.log(log)))


def step_done(run, job, step, what, timeout):
    def check():
        state = steps(run, job).get(step, ("", ""))
        assert state[0] != "failed", f"{job}: step {step} failed: {state[1]}\n" + tail(run.log(job))
        code = run.wait(job, 1)
        assert code in (None, 0), f"{job} exited {code} before {step}\n" + tail(run.log(job))
        return state if state[0] in ("done", "skipped") else None
    return wait_for(what, check, timeout, log=lambda: run.log(job))


def test_40_deposit_sent(run, ctx):
    """`deposit send` sends approve and deposit from ARBITRUM_SEPOLIA_E2E_ADDRESS."""
    run.job("cli-deposit", stdin=env("ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY"),
            TOKEN="usdc", FROM=ctx["address"], TO=ctx["msig"], AMOUNT="2")
    state = step_done(run, "cli-deposit", "deposit", "the deposit transaction is sent", 900)
    print(f"  deposit https://sepolia.arbiscan.io/tx/{state[1]}")
    ctx.update(deposit_tx=state[1])


def test_41_deposit_event_received(run, arb, ctx):
    """TC-21. The deposit is mined, confirmed, and its Deposit event is found; `deposit send` ends
    there, with nothing sent to Acki Nacki. The bridge is on the AAVE market of 421614, and the
    deposit put part of its 2 USDC into the pool."""
    state = step_done(run, "cli-deposit", "evm_confirm", "the Deposit event is received", 900)
    assert state[1].startswith("depositId"), state
    run.finish("cli-deposit", 300)
    got = summary(run, "cli-deposit", "deposit")
    assert got["deposit"]["tx_hash"] == ctx["deposit_tx"] and got.get("confirmation") is None, got
    bridge = ctx["bridge"]
    assert arb.call(bridge, "aavePool()") == int(ARB_AAVE_POOL, 16), "the bridge is not on the AAVE pool of 421614"
    assert arb.call(bridge, "aUSDC()") == int(ARB_AUSDC, 16), "the bridge does not hold the aUSDC of 421614"
    supplied = arb.call(bridge, "suppliedPrincipal()")
    assert supplied > 0, "the deposit supplied nothing to AAVE"
    ctx.update(supplied=supplied)


# ── 4b · withdrawals: burns on Acki Nacki, alongside the deposits ─────────


def test_42_withdrawal_multisig_funded(an, ctx):
    """A second multisig gets 2 USDC the owner mints with mintAndSendBridged for 421614, so the
    withdrawal runs alongside the deposit instead of after it: the bridge on Arbitrum Sepolia
    already holds the deposit's 2 USDC to pay it with."""
    got = an("msig-funded", AMOUNT)
    ctx.update(wmsig=got["MSIG"], wmsig_keys_path=got["MSIG_KEYS"])
    assert int(an("ecc", ctx["wmsig"])["ECC3"]) == AMOUNT, "the withdrawal multisig does not hold the 2 USDC"


def test_42b_nackl_withdrawal_multisig_funded(an, ctx):
    """A third multisig gets 5 NACKL from the giver."""
    got = an("msig-nackl", NACKL_OUT)
    ctx.update(nmsig=got["MSIG"], nmsig_keys=an.cat(got["MSIG_KEYS"]))
    assert int(an("ecc", ctx["nmsig"])["ECC1"]) >= NACKL_OUT


def the_claim(run, ctx, amount):
    """daemon-live's record of this run's burn of `amount` base units (its withdraw-claims.json,
    kept in /out), or None while it has not seen it. daemon-live must keep running."""
    code = run.wait("relayer", 1)
    assert code is None, f"daemon-live exited {code}\n" + tail(run.log("relayer", 200))
    try:
        claims = json.loads(run.get("withdraw-claims.json"))
    except ValueError:
        return None
    for claim in claims.get("withdrawals", []):
        if (claim.get("recipient") or "").lower() == ctx["address"].lower() and claim.get("amount") == str(amount):
            return claim
    return None


def claim_reaches(run, ctx, amount, what, done, timeout):
    """Waits until `done(claim)`; the failure shows the claim and the tail of daemon-live's log."""
    def show():
        return f"claim: {the_claim(run, ctx, amount)}\n" + run.log("relayer", 200)
    return wait_for(what, lambda: (lambda c: c if c and done(c) else None)(the_claim(run, ctx, amount)),
                    timeout, log=show)


def test_43_burn_on_acki_nacki(an, ctx):
    """The multisig burns the 2 USDC on Acki Nacki to ARBITRUM_SEPOLIA_E2E_ADDRESS, as the wallet
    does: sendTransaction to the bridge with initiateWithdrawal, and nothing after it."""
    got = an("burn", ctx["wmsig"], ctx["wmsig_keys_path"], AMOUNT, ctx["address"])
    assert int(got["ECC3_AFTER"]) == 0, "the multisig still holds the USDC it burned"
    ctx.update(burn_tx=got["BURN_TX"])


def test_43b_nackl_burn_by_withdraw_send(run, ctx):
    """`withdraw send --token nackl` burns the 5 NACKL to ARBITRUM_SEPOLIA_E2E_ADDRESS and stops once
    the bridge's WithdrawalInitiated is captured: nothing is proven or sent to Arbitrum Sepolia."""
    run.job("cli-withdraw-send", stdin=ctx["nmsig_keys"].replace("\n", ""), TOKEN="nackl",
            FROM=ctx["nmsig"], TO=ctx["address"], AMOUNT=str(NACKL_OUT // NACKL))
    run.finish("cli-withdraw-send", 900)
    got = summary(run, "cli-withdraw-send", "burn")
    assert got["burn"]["token_id"] == 1 and got["proof"] is None and got["submit"] is None, got
    ctx.update(nackl_burn_tx=got["burn"]["an_tx"])


def test_44_withdrawal_found_by_the_relayer(run, ctx):
    """The bridge emits WithdrawalInitiated and daemon-live finds both burns on its next polls:
    their recipient and amount are in its record, waiting for coverage."""
    usdc = claim_reaches(run, ctx, AMOUNT, "daemon-live finds the USDC burn",
                         lambda c: c.get("block_seq_no") is not None, 600)
    nackl = claim_reaches(run, ctx, NACKL_OUT, "daemon-live finds the NACKL burn",
                          lambda c: c.get("block_seq_no") is not None, 600)
    assert usdc["state"] == "pending" and nackl["state"] == "pending", (usdc, nackl)
    ctx.update(burn_seq_no=max(usdc["block_seq_no"], nackl["block_seq_no"]))


def test_45_nackl_locked_on_acki_nacki(an, ctx):
    """The NACKL burn locked the 5 NACKL on the Acki Nacki bridge, for 421614 and for this bridge."""
    got = an("nackl", ctx["bridge"])
    assert int(got["LOCKED"]) == NACKL_OUT, f"locked for 421614: {got}"
    assert int(got["LOCKED_TO"]) == NACKL_OUT, f"locked to the bridge: {got}"
    assert int(an("ecc", ctx["nmsig"])["ECC1"]) < NACKL_OUT, "the NACKL multisig still holds the 5 NACKL"


def test_46_nackl_deposit_sent(run, arb, ctx):
    """`deposit send --token nackl` deposits 3 of the NACKL the deployer holds to the recipient
    multisig and ends once it is confirmed."""
    at = arb.block()
    ctx["nackl_supply_before"] = arb.call(ctx["nackl"], "totalSupply()", block=at)
    ctx["nackl_minted_before"] = arb.words(ctx["bridge"], "nacklState()", block=at)[4]
    run.job("cli-deposit-nackl", stdin=env("ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY"),
            FROM=ctx["address"], TO=ctx["msig"], AMOUNT=str(NACKL_IN // NACKL))
    run.finish("cli-deposit-nackl", 900)
    got = summary(run, "cli-deposit-nackl", "deposit")
    assert got.get("confirmation") is None, got
    ctx.update(nackl_deposit_tx=got["deposit"]["tx_hash"])


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


# ── 6 · the deposits land ───────────────────────────────────────────────────


def test_60_light_client_accepts_spans(run):
    """arbitrum-anchors sends the light client the Sepolia blocks of the epoch an Arbitrum
    assertion landed in (submitAncestry), between the two checkpoints around it."""
    sent = "submitAncestry sent for the assertion's parent-chain block"
    wait_for("arbitrum-anchors gets a span accepted", lambda: sent in run.log("anchors"),
             ANCHOR_SECS, log=lambda: run.log("anchors"))
    assert "no more submitAncestry" not in run.log("anchors"), \
        "arbitrum-anchors gave a span up: its block will never be accepted\n" + tail(run.log("anchors"))


def test_61_deposit_block_anchored(run, an, arb, ctx):
    """The deposit's Arbitrum block reaches the anchor set through ArbitrumBlockVerifier, from an
    assertion in a Sepolia block the light client accepted."""
    block = arb.receipt(ctx["deposit_tx"])["blockHash"]
    wait_for("the deposit block is accepted on Acki Nacki",
             lambda: an("accepted", ARB_CHAIN, block).get("ACCEPTED") == "true", ANCHOR_SECS,
             log=lambda: run.log("anchors"))
    log = run.log("anchors")
    assert "proveBlock sent" in log and "the bridge accepts the deposit block" in log, \
        "arbitrum-anchors did not get the block accepted through proveBlock\n" + tail(log)


def test_62_deposits_finalized_by_the_relayer(run, an, ctx):
    """The deposit relayer builds each deposit's receipt proof and finalizes it on Acki Nacki, and
    the vouchers credit the recipient: 2 USDC and 3 NACKL."""
    def credited():
        got = an("ecc", ctx["msig"])
        return int(got["ECC3"]) == AMOUNT and int(got["ECC1"]) == NACKL_IN
    wait_for("the deposit relayer finalizes both deposits and they are credited", credited, 1800,
             log=lambda: run.log("deposit-relayer"))
    assert "daemon: finalized" in run.log("deposit-relayer"), tail(run.log("deposit-relayer"))


def test_63_deposit_proofs_find_them_finalized(run, ctx):
    """`deposit proof --tx-hash` of each deposit, once the relayer finalized it: the CLI builds the
    receipt proof, finds the deposit finalized before sending, sends nothing, confirms the credit
    and ends in success, finalize: already_finalized."""
    deposits = {"usdc": ctx["deposit_tx"], "nackl": ctx["nackl_deposit_tx"]}
    run.job("cli-deposit-proof", DEPOSITS=" ".join(f"{t}:{tx}" for t, tx in deposits.items()))
    run.finish("cli-deposit-proof", 1800)
    for token in deposits:
        log = f"cli-deposit-proof-{token}"
        got = steps(run, log)
        assert got.get("prove", ("",))[0] == "done", f"{token}: the receipt proof was not built: {got}"
        finalize = got.get("finalize", ("", ""))
        assert finalize[0] == "skipped" and "finalized" in finalize[1], f"{token}: finalize {finalize}"
        assert got.get("credit", ("",))[0] == "done", f"{token}: no credit confirmed: {got}"
        done = summary(run, log, "finalize")
        assert done["finalize"] == "already_finalized" and done["confirmation"], done


def test_65_deposit_delivered(an, ctx):
    """The 2 USDC are on the recipient multisig."""
    assert int(an("ecc", ctx["msig"])["ECC3"]) == AMOUNT, "the multisig does not hold the 2 USDC"


def test_66_nackl_deposit_delivered(an, arb, ctx):
    """The 3 NACKL are on the recipient multisig, burned on Arbitrum Sepolia and unlocked from the 5
    the withdrawal locked."""
    assert int(an("ecc", ctx["msig"])["ECC1"]) == NACKL_IN, "the multisig did not receive the 3 NACKL"
    at = arb.block()
    minted = arb.words(ctx["bridge"], "nacklState()", block=at)[4] - ctx["nackl_minted_before"]
    assert arb.call(ctx["nackl"], "totalSupply()", block=at) == \
        ctx["nackl_supply_before"] - NACKL_IN + minted, "the deposit did not burn the 3 NACKL"
    assert int(an("nackl", ctx["bridge"])["LOCKED"]) == NACKL_OUT - NACKL_IN


# ── 7 · the withdrawals land ────────────────────────────────────────────────


def test_70_burn_bundle_verified(arb, ctx):
    """daemon-live gets the bundle that covers both burns verified on Arbitrum Sepolia: the next
    16384 boundary past the later burn's block."""
    covering = (ctx["burn_seq_no"] // 16384 + 1) * 16384
    wait_for(f"verifyBlock reaches {covering}",
             lambda: arb.call(ctx["bridge"], "storedLastSeenBlockSeqNo()") >= covering, COVERAGE_SECS)


def test_71_withdrawal_paid_by_the_relayer(run, ctx):
    """daemon-live proves the USDC withdrawal (Circuit 4 and its SHPLONK aggregation) and pays it
    out with withdrawByProof: its record says paid, with the transaction."""
    claim = claim_reaches(run, ctx, AMOUNT, "daemon-live pays the USDC withdrawal out",
                          lambda c: c["state"] != "pending", 5400)
    assert claim["state"] == "paid", f"the burn ended {claim['state']}, not paid by daemon-live: {claim}"
    ctx.update(withdraw_tx=claim["tx_hash"])
    print(f"  withdrawByProof https://sepolia.arbiscan.io/tx/{claim['tx_hash']}")


def test_71b_nackl_withdrawal_paid_by_the_relayer(run, ctx):
    """daemon-live proves the NACKL withdrawal after the USDC one and pays it out: the bridge mints
    the 5 NACKL at once, within the daily limit and below the delay threshold."""
    claim = claim_reaches(run, ctx, NACKL_OUT, "daemon-live pays the NACKL withdrawal out",
                          lambda c: c["state"] != "pending", 1800)
    assert claim["state"] == "paid", f"the NACKL burn ended {claim['state']}, not paid at once: {claim}"
    print(f"  withdrawByProof https://sepolia.arbiscan.io/tx/{claim['tx_hash']}")


WITHDRAWAL_PAID = "0x" + keccak256(b"WithdrawalByProofExecuted(uint256,address,uint256,uint256,address)").hex()


def test_72_withdrawal_sent_from_the_relayer(run, arb, ctx):
    """The payout went to ARBITRUM_SEPOLIA_E2E_ADDRESS in a transaction the relayer's key sent
    and paid the gas for."""
    receipt = arb.receipt(ctx["withdraw_tx"])
    relayer = kv(run.get("fund.env"))["RELAYER_ADDRESS"].lower()
    assert receipt["from"].lower() == relayer, f"withdrawByProof came from {receipt['from']}, not the relayer {relayer}"
    paid = [l for l in receipt["logs"] if l["address"].lower() == ctx["bridge"].lower() and l["topics"][0] == WITHDRAWAL_PAID]
    assert paid, "no WithdrawalByProofExecuted in the payout"
    assert int(paid[0]["topics"][2], 16) == int(ctx["address"], 16), f"paid to {paid[0]['topics'][2]}"


WITHDRAWN_FROM_AAVE = "0x" + keccak256(b"WithdrawnFromAave(uint256,uint256)").hex()


def test_72a_withdrawal_pulled_from_aave(arb, ctx):
    """TC-22. The payout needed more than the USDC the bridge kept liquid: withdrawByProof pulled
    it back from the pool, and nothing the users are owed is booked as supplied any more."""
    bridge = ctx["bridge"].lower()
    logs = arb.receipt(ctx["withdraw_tx"])["logs"]
    pulls = [l for l in logs if l["address"].lower() == bridge and l["topics"][0] == WITHDRAWN_FROM_AAVE]
    assert pulls, "withdrawByProof did not pull from AAVE"
    assert arb.call(ctx["bridge"], "treasuryBalance()") == 0, "the bridge still owes USDC"
    assert arb.call(ctx["bridge"], "suppliedPrincipal()") == 0, \
        f"{ctx['supplied'] / 1e6} USDC were supplied; the payout left some booked"


def test_73_withdraw_proofs_find_them_paid(run, ctx):
    """`withdraw proof --burn-tx` of each burn — the wallet-style USDC burn and the NACKL burn of
    `withdraw send` — once daemon-live paid both: the CLI builds the Circuit 4 proof, finds the
    burn paid out when it sends and ends in success, submit.status already_paid."""
    burns = {"usdc": ctx["burn_tx"], "nackl": ctx["nackl_burn_tx"]}
    run.job("cli-withdraw", stdin=env("ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY"),
            BURNS=" ".join(f"{t}:{tx}" for t, tx in burns.items()))
    run.finish("cli-withdraw", 3600)
    for token, tx in burns.items():
        got = summary(run, f"cli-withdraw-{token}", "submit")
        assert got["burn"]["an_tx"] == tx.removeprefix("0x").lower(), got
        assert got["proof"] and got["proof"]["calldata_bytes"] > 0, f"{token}: no proof built: {got}"
        assert got["submit"]["status"] == "already_paid" and got["submit"]["eth_tx"] is None, got


def test_74_usdc_delivered(arb, an, ctx):
    """The 2 USDC are back on ARBITRUM_SEPOLIA_E2E_ADDRESS, the withdrawal multisig is empty, and
    the deposit's 2 USDC stay on the recipient multisig."""
    after = arb.usdc(ctx["address"])
    assert after == ctx["usdc_before"], \
        f"{ctx['address']} holds {after / 1e6} USDC, {ctx['usdc_before'] / 1e6} before the deposit"
    assert int(an("ecc", ctx["wmsig"])["ECC3"]) == 0, "the withdrawal multisig still holds USDC"
    assert int(an("ecc", ctx["msig"])["ECC3"]) == AMOUNT, "the deposit's 2 USDC left the recipient multisig"


def test_75_nackl_minted(arb, ctx):
    """The bridge minted the 5 NACKL it accepted, and nothing waits in its queue."""
    state = arb.words(ctx["bridge"], "nacklState()")
    accepted, minted, pending = state[3], state[4], state[5]
    assert (accepted, minted, pending) == (NACKL_OUT, NACKL_OUT, 0), f"nacklState: {state}"


def test_76_nackl_reconciles(arb, an, ctx):
    """NACKL supply on Arbitrum Sepolia and the lock on Acki Nacki agree: 5 out, 3 back."""
    assert arb.call(ctx["nackl"], "totalSupply()") == NACKL_SUPPLY - NACKL_IN + NACKL_OUT
    got = an("nackl", ctx["bridge"])
    assert int(got["LOCKED"]) == NACKL_OUT - NACKL_IN, got
    assert int(got["LOCKED_TO"]) == arb.words(ctx["bridge"], "nacklState()")[3], got
