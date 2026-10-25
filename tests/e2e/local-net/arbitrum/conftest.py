"""Fixtures of the Arbitrum Sepolia e2e (test_arbitrum_e2e.py).

Three places take part, and each has one helper here:

* the prover host, through the restricted CI key: `ProverHost` sends the dispatcher's verbs
  (tests/e2e/local-net/prover-host/ci-entry); jobs run there in containers and log to the
  run's external log volume;
* the local-network host: `LocalNet` runs python/arbitrum_e2e_an.py there for the owner calls
  and the Acki Nacki reads;
* Arbitrum Sepolia: `Arbitrum` reads it over JSON-RPC from this runner.

Every wait goes through `wait_for`, which fails with what it waited for, how long, and the
tail of the log it watched.
"""

import json
import os
import re
import subprocess
import sys
import threading
import time
import urllib.request
from datetime import datetime, timezone

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
LOCAL_NET = os.path.dirname(HERE)
REPO = os.path.abspath(os.path.join(LOCAL_NET, "..", "..", ".."))
sys.path.insert(0, os.path.join(REPO, "scripts", "l2-block-proof-bench"))
from keccak import keccak256  # noqa: E402

ARB_RPC = os.environ.get("ARB_RPC", "https://sepolia-rollup.arbitrum.io/rpc")
ARB_USDC = "0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d"
ARB_CHAIN, SEPOLIA_CHAIN = 421614, 11155111
ARB_ROLLUP = "0x042b2e6c5e99d4c521bd49beed5e99651d9b0cf4"


def env(name):
    value = os.environ.get(name, "")
    assert value, f"{name} is not set: it comes from the pipeline's secrets"
    return value


def kv(text):
    """KEY=VALUE lines of a command's answer."""
    return dict(line.split("=", 1) for line in text.splitlines() if re.match(r"^[A-Z0-9_]+=", line))


# ── the prover host ─────────────────────────────────────────────────────────

class ProverHost:
    def __init__(self):
        key = os.path.expanduser("~/.ssh/prover_ci")
        with open(os.open(key, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600), "w") as f:
            f.write(env("PROVER_HOST_SSH_KEY").replace("@", "\n"))
        self.opts = ["-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes", "-o", "StrictHostKeyChecking=accept-new",
                     "-o", "ServerAliveInterval=30", "-o", "LogLevel=ERROR", "-i", key,
                     "-p", env("PROVER_HOST_PORT")]
        self.addr = f"{env('PROVER_HOST_USER')}@{env('PROVER_HOST')}"

    def __call__(self, *argv, stdin="", timeout=600, check=True):
        p = subprocess.run(["ssh", *self.opts, self.addr, " ".join(str(a) for a in argv)],
                           input=stdin, capture_output=True, text=True, timeout=timeout)
        if check and p.returncode != 0:
            raise AssertionError(f"prover host `{argv[0]}` exited {p.returncode}: {p.stderr.strip()[-2000:]}")
        return p.stdout

    def tunnel(self, rev):
        return subprocess.Popen(["ssh", *self.opts, "-N", "-o", "ExitOnForwardFailure=yes", *rev, self.addr])


class Run:
    """One run on the prover host: arbitrum-e2e-<UTC timestamp>."""

    def __init__(self, ph, sha):
        self.ph, self.sha = ph, sha
        self.name = "arbitrum-e2e-" + datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")

    def job(self, name, stdin="", **args):
        return self.ph("job", self.name, self.sha, name, *[f"{k}={v}" for k, v in args.items()], stdin=stdin)

    def wait(self, name, secs):
        """The job's exit code, or None while it runs."""
        out = self.ph("wait", self.name, name, secs, check=False, timeout=secs + 120).strip().splitlines()
        code = out[-1] if out else ""
        return None if code in ("", "timeout") else int(code)

    def log(self, name, lines=5000):
        return self.ph("log", self.name, name, lines, check=False)

    def get(self, file):
        return self.ph("get", self.name, file, check=False)

    def put_log(self, name, text):
        self.ph("put-log", self.name, name, stdin=text, check=False)

    def finish(self, name, secs):
        """Waits for a job that is expected to end, and asserts it ended well."""
        code = self.wait(name, secs)
        assert code == 0, f"job {name} exited {code} (None: still running after {secs} s)\n" + tail(self.log(name, 60))


def tail(text, n=40):
    lines = text.rstrip("\n").splitlines()
    return "\n".join(["--- last lines ---", *lines[-n:]])


def wait_for(what, check, timeout, interval=10, log=None):
    """Polls `check` until it returns a truthy value; fails with `what`, the wait and a log tail."""
    start = time.time()
    while True:
        value = check()
        if value:
            print(f"  ✔ {what} after {time.time() - start:.0f} s")
            return value
        if time.time() - start > timeout:
            raise AssertionError(f"{what}: not within {timeout:.0f} s\n" + (tail(log()) if log else ""))
        time.sleep(interval)


# ── the local-network host ──────────────────────────────────────────────────

class LocalNet:
    def __init__(self, sha):
        self.ssh = ["ssh", *os.environ.get("SSH_OPTS", "").split(), f"{env('E2E_CI_USER')}@{env('E2E_HOST')}"]
        self.sha = sha
        with open(os.path.join(LOCAL_NET, "an-on-host.sh")) as f:
            self.script = f.read()

    def __call__(self, *args, timeout=600):
        cmd = (f"BRIDGE_SHA='{self.sha}' ACKI_DIR='{env('ACKI_DIR')}' WORK_ROOT=$HOME/'{env('WORK_ROOT')}' "
               f"bash -s -- {' '.join(args)}")
        p = subprocess.run([*self.ssh, cmd], input=self.script, capture_output=True, text=True, timeout=timeout)
        if p.returncode != 0:
            raise AssertionError(f"local network `{' '.join(args)}` exited {p.returncode}:\n{tail(p.stderr)}")
        return kv(p.stdout)

    def cat(self, path):
        return subprocess.run([*self.ssh, f"cat '{path}'"], capture_output=True, text=True, check=True).stdout

    def renew_lease(self, run_id):
        with open(os.path.join(LOCAL_NET, "host-lease.sh")) as f:
            subprocess.run([*self.ssh, f"bash -s -- renew {run_id}"], input=f.read(), capture_output=True, text=True)

    def tunnel(self, fwd):
        return subprocess.Popen([*self.ssh[:-1], "-N", "-o", "ExitOnForwardFailure=yes", *fwd, self.ssh[-1]])


# ── Arbitrum Sepolia ────────────────────────────────────────────────────────

class Arbitrum:
    def rpc(self, method, params):
        body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
        req = urllib.request.Request(ARB_RPC, body, {"content-type": "application/json", "user-agent": "bridge-e2e"})
        for attempt in range(5):
            try:
                answer = json.load(urllib.request.urlopen(req, timeout=30))
                if "error" in answer:
                    raise AssertionError(f"{method}: {answer['error']}")
                return answer["result"]
            except OSError:
                time.sleep(2 * (attempt + 1))
        raise AssertionError(f"{method}: Arbitrum Sepolia RPC did not answer")

    def call(self, to, signature, args=b""):
        data = "0x" + keccak256(signature.encode())[:4].hex() + args.hex()
        return int(self.rpc("eth_call", [{"to": to, "data": data}, "latest"]), 16)

    def code(self, address):
        return self.rpc("eth_getCode", [address, "latest"])

    def eth(self, address):
        return int(self.rpc("eth_getBalance", [address, "latest"]), 16)

    def usdc(self, address):
        return self.call(ARB_USDC, "balanceOf(address)", bytes(12) + bytes.fromhex(address[2:]))


# ── secp256k1, to tie the deployer key to its address ───────────────────────

_P = 2**256 - 2**32 - 977
_G = (0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798,
      0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8)


def _add(a, b):
    if a is None:
        return b
    if b is None:
        return a
    if a[0] == b[0] and (a[1] + b[1]) % _P == 0:
        return None
    lam = (3 * a[0] * a[0] * pow(2 * a[1], -1, _P) if a == b else (b[1] - a[1]) * pow(b[0] - a[0], -1, _P)) % _P
    x = (lam * lam - a[0] - b[0]) % _P
    return x, (lam * (a[0] - x) - a[1]) % _P


def evm_address(private_key_hex):
    k, r, pt = int(private_key_hex, 16), None, _G
    while k:
        if k & 1:
            r = _add(r, pt)
        pt, k = _add(pt, pt), k >> 1
    return "0x" + keccak256(r[0].to_bytes(32, "big") + r[1].to_bytes(32, "big"))[-20:].hex()


# ── session fixtures ────────────────────────────────────────────────────────

class Context(dict):
    """What one test hands the next: addresses, hashes, balances."""


@pytest.fixture(scope="session")
def ctx():
    return Context()


@pytest.fixture(scope="session")
def sha():
    return env("CI_COMMIT_SHA")


@pytest.fixture(scope="session")
def ph():
    return ProverHost()


@pytest.fixture(scope="session")
def an(sha):
    return LocalNet(sha)


@pytest.fixture(scope="session")
def arb():
    return Arbitrum()


@pytest.fixture(scope="session")
def run(ph, an, sha):
    r = Run(ph, sha)
    print(f"\nrun {r.name}: logs on the prover host in run-logs/{r.name}/")
    lease = f"bridge-{os.environ.get('CI_PIPELINE_NUMBER', 'manual')}"
    stop = threading.Event()

    def heartbeat():  # the local network's host lease outlives the test only while renewed
        while not stop.wait(300):
            an.renew_lease(lease)
    threading.Thread(target=heartbeat, daemon=True).start()
    yield r
    stop.set()
    for job in ("relayer", "anchors", "cli-deposit", "cli-withdraw"):
        text = r.log(job, 80)
        if text:
            print(f"\n----- {job} (last 80 lines)\n{text}")
    print(ph("down", r.name, check=False))


@pytest.fixture(scope="session")
def tunnels(an, ph):
    """nginx0 and node0..4 of the local network, through this runner, on the prover host's
    loopback (127.0.0.1:28080 and :28600-28604), where the dispatcher's proxies pick them up."""
    fwd, rev = ["-L", "127.0.0.1:18080:127.0.0.1:80"], ["-R", "127.0.0.1:28080:127.0.0.1:18080"]
    for n in range(5):
        fwd += ["-L", f"127.0.0.1:1860{n}:node{n}:8600"]
        rev += ["-R", f"127.0.0.1:2860{n}:127.0.0.1:1860{n}"]
    procs = [an.tunnel(fwd), ph.tunnel(rev)]
    time.sleep(5)
    for p in procs:
        assert p.poll() is None, "an ssh tunnel did not come up (a port held by an earlier run?)"
    yield procs
    for p in procs:
        p.terminate()


@pytest.fixture(scope="session")
def owner_as_light_client(an, run):
    """Stands in for the Sepolia light client: whenever arbitrum-anchors holds a proof whose
    Sepolia block the bridge does not accept yet, the owner accepts that block."""
    anchored, stop = [], threading.Event()

    def loop():
        while not stop.wait(20):
            try:
                state = json.loads(run.get("anchors-state.json") or "{}")
            except ValueError:
                continue
            for ready in state.get("ready", []):
                h = ready["parent_hash"]
                if h in anchored:
                    continue
                answer = an("anchor", SEPOLIA_CHAIN, h)
                anchored.append(h)
                line = f"{datetime.now(timezone.utc):%FT%TZ} owner accepted Sepolia block {h}: {answer}\n"
                print("  " + line.strip())
                run.put_log("owner-anchors", line)
    t = threading.Thread(target=loop, daemon=True)
    t.start()
    yield anchored
    stop.set()
