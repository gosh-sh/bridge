#!/usr/bin/env python3
"""
Base test utilities for Acki Nacki bridge TVM contracts (pytest + tvm-debugger).

Adapted from dex/audit/tests-head/test_base.py (pruvendo-gosh-accumulator lineage).
"""

import subprocess
import json
import os
import shutil
import base64
import struct
import time
from pathlib import Path
from typing import Dict, Any, Optional, List, Tuple
from dataclasses import dataclass, field
from collections import deque


@dataclass
class JsonResult:
    """Result of a TVM execution with -j JSON output."""
    exit_code: int
    vm_success: bool
    gas_used: int
    response: Any
    messages: list = field(default_factory=list)
    raw_json: dict = field(default_factory=dict)
    process_exit_code: int = 0
    stderr: str = ""

    def first_internal_dest(self) -> Optional[str]:
        """Return the destination of the first *internal* outgoing message.

        In halo, several functions emit an external-out event (e.g.
        `address.makeAddrExtern(...)` for monitoring) BEFORE the real internal
        callback. External messages have no internal destination and would
        crash a naïve `messages[0]["destination"]` lookup with
        "Failed to decode message source: 0". This helper skips them and
        returns the first internal `dst` we can route to.
        """
        for m in self.messages:
            dst = m.get("destination") or m.get("dst") or m.get("dest")
            if not dst:
                continue
            s = str(dst)
            if s.startswith("0:") or s.startswith("-1:"):
                return s
        return None


@dataclass
class PipelineStep:
    """Record of a single message execution in the pipeline.

    The ``incoming_msg`` and ``outgoing_msgs`` fields are populated by
    :meth:`MessagePipeline.process` (Phase S.1+) so post-mortem analysers
    (e.g. ``SolvencyLedgerObserver``) can reconstruct ECC flows without
    re-executing the trace. Older test fixtures that constructed
    ``PipelineStep`` manually keep working — both fields default to
    None / empty list.
    """
    step: int
    destination: str
    contract_name: str
    exit_code: int
    vm_success: bool
    gas_used: int
    messages_out: int
    is_deploy: bool = False
    incoming_msg: Optional[dict] = None
    outgoing_msgs: list = field(default_factory=list)


class MessagePipeline:
    """Cross-contract message router for local TVM testing.

    Maintains a registry of contract instances (address -> TVC path),
    a message queue, and processes messages sequentially, routing each
    outgoing message to its destination contract via run-raw.
    """

    def __init__(self, tb: "TestBase", block_seqno: Optional[int] = None,
                 block_timestamp: Optional[int] = None):
        self.tb = tb
        self.registry: Dict[str, Path] = {}
        self.abi_map: Dict[str, str] = {}
        self.queue: deque = deque()
        self.history: List[PipelineStep] = []
        self._deploy_counter = 0
        self._block_seqno = block_seqno
        self._block_timestamp = block_timestamp

    def register(self, address: str, tvc_path: Path, contract_name: str = ""):
        self.registry[address] = tvc_path
        if contract_name:
            self.abi_map[address] = contract_name

    def compute_address(self, state_init_b64: str) -> str:
        r = subprocess.run(
            [str(self.tb.tvm_debugger), "boc-hash"],
            input=state_init_b64, capture_output=True, text=True,
        )
        h = json.loads(r.stdout.strip().split("\n")[-1])["hash"]
        return f"0:{h}"

    def compute_address_from_tvc(self, tvc_path: Path) -> str:
        with open(tvc_path, "rb") as f:
            b64 = base64.b64encode(f.read()).decode()
        return self.compute_address(b64)

    def enqueue(self, messages: List[dict]):
        for m in messages:
            self.queue.append(m)

    def _deploy_contract(self, dest: str, state_init_b64: str) -> Path:
        self._deploy_counter += 1
        # PID-scoped to avoid pytest-xdist races where one worker's cleanup()
        # nukes another worker's pipeline-deploy tvc still in flight.
        tvc = self.tb.build_dir / (
            f"_pipeline_deploy_{os.getpid()}_{self._deploy_counter}.tvc"
        )
        with open(tvc, "wb") as f:
            f.write(base64.b64decode(state_init_b64))
        self.registry[dest] = tvc
        return tvc

    def _ensure_rejector_tvc(self) -> Path:
        """Compile a minimal contract that rejects any message (for bounce generation)."""
        path = self.tb.build_dir / "_Rejector.tvc"
        if not path.exists():
            src = self.tb.build_dir / "_Rejector.sol"
            src.write_text(
                'pragma ever-solidity >= 0.79.0;\n'
                'contract _Rejector { receive() external pure { revert(); } }\n'
            )
            subprocess.run(
                [str(self.tb.sold), str(src), "-o", str(self.tb.build_dir)],
                capture_output=True, text=True,
            )
        return path

    def _try_bounce_for_missing_dest(self, msg: dict, dest: str,
                                      verbose: bool) -> bool:
        """If msg has bounce:true, generate a bounce for non-existent destination.

        Runs the message against a rejector TVC (always fails), letting the
        debugger produce the bounce message.  Returns True if bounce was enqueued.
        """
        if not msg.get("bounce", False):
            return False

        boc = msg.get("boc", "")
        empty_tvc = self._ensure_rejector_tvc()
        jr = self._run_raw(empty_tvc, boc, dest)
        if not jr.vm_success and jr.messages:
            for m in jr.messages:
                self.queue.append(m)
            if verbose:
                print(f"  [pipeline] BOUNCE for missing {dest[:20]}... → "
                      f"{len(jr.messages)} bounce msg(s)")
            return True
        return False

    def _run_raw(self, tvc_path: Path, boc: str, address: str,
                  block_seqno: Optional[int] = None,
                  block_timestamp: Optional[int] = None) -> JsonResult:
        cmd = [
            str(self.tb.tvm_debugger), "run-raw",
            "--input-file", str(tvc_path),
            "--message-boc", boc,
            "--force-no-abi",
            "--address", address,
        ]
        seqno = block_seqno if block_seqno is not None else self._block_seqno
        if seqno is not None:
            cmd.extend(["--block-seqno", str(seqno)])
        ts = block_timestamp if block_timestamp is not None else self._block_timestamp
        if ts is not None:
            cmd.extend(["--block-timestamp", str(ts)])
        if self.tb.wasm_root_path:
            cmd.extend(["--wasm-root-path", self.tb.wasm_root_path])
        result = subprocess.run(cmd, capture_output=True, text=True)
        stdout = result.stdout.strip()
        if stdout:
            try:
                data = json.loads(stdout)
                if isinstance(data, str):
                    data = json.loads(data)
                return JsonResult(
                    exit_code=data.get("exit_code", -1),
                    vm_success=data.get("vm_success", False),
                    gas_used=data.get("gas_used", 0),
                    response=data.get("response", {}),
                    messages=data.get("messages", []),
                    raw_json=data,
                    process_exit_code=result.returncode,
                    stderr=result.stderr,
                )
            except json.JSONDecodeError:
                pass
        return JsonResult(
            exit_code=-1, vm_success=False, gas_used=0, response={},
            process_exit_code=result.returncode, stderr=result.stderr,
        )

    def peek_inflight(self) -> List[dict]:
        """Snapshot of pending queue messages (METHOD-GAP-07 / Sprint C)."""
        return list(self.queue)

    def reorder_inflight(self, permutation) -> None:
        """Reorder ``queue`` by index permutation (Sprint C.1).

        ``permutation[i]`` — old index of the message that becomes
        the new i-th FIFO slot (0 = next ``process()`` pop).
        """
        items = list(self.queue)
        n = len(items)
        perm = list(permutation)
        if len(perm) != n:
            raise ValueError(
                f"reorder_inflight: len(permutation)={len(perm)} "
                f"!= queue size {n}"
            )
        if sorted(perm) != list(range(n)):
            raise ValueError(
                f"reorder_inflight: {perm!r} is not a permutation of "
                f"0..{n - 1}"
            )
        self.queue.clear()
        self.queue.extend(items[i] for i in perm)

    def pop_inflight_at(self, index: int) -> dict:
        """Remove and return the message at ``index`` (0 = FIFO head)."""
        if index < 0 or index >= len(self.queue):
            raise IndexError(
                f"pop_inflight_at({index}): queue len={len(self.queue)}"
            )
        if index == 0:
            return self.queue.popleft()
        items = list(self.queue)
        msg = items.pop(index)
        self.queue.clear()
        self.queue.extend(items)
        return msg

    def _deliver_one_message(self, msg: dict, *, verbose: bool = True
                             ) -> Optional[PipelineStep]:
        """Deliver a single queued message; return ``PipelineStep`` or None."""
        dest = msg.get("destination", "")
        boc = msg.get("boc", "")
        si = msg.get("state_init")
        is_deploy = bool(si and si != "None" and si is not None)
        global_step = len(self.history)

        if dest not in self.registry:
            if is_deploy:
                self._deploy_contract(dest, si)
                if verbose:
                    print(f"  [pipeline #{global_step}] DEPLOY at {dest[:20]}...")
            else:
                bounced = self._try_bounce_for_missing_dest(msg, dest, verbose)
                if not bounced and verbose:
                    print(
                        f"  [pipeline #{global_step}] SKIP: no contract "
                        f"at {dest[:20]}..."
                    )
                return None

        tvc_path = self.registry[dest]
        cname = self.abi_map.get(dest, "unknown")
        jr = self._run_raw(tvc_path, boc, dest)

        record = PipelineStep(
            step=global_step, destination=dest, contract_name=cname,
            exit_code=jr.exit_code, vm_success=jr.vm_success,
            gas_used=jr.gas_used, messages_out=len(jr.messages),
            is_deploy=is_deploy,
            incoming_msg=msg,
            outgoing_msgs=list(jr.messages or []),
        )
        self.history.append(record)

        if verbose:
            status = "OK" if jr.vm_success else "FAIL"
            print(
                f"  [pipeline #{global_step}] {status} -> "
                f"{cname}@{dest[:16]}... exit={jr.exit_code} "
                f"gas={jr.gas_used} out_msgs={len(jr.messages)}"
            )

        if jr.messages:
            self.enqueue(jr.messages)
        return record

    def process_one_at(self, index: int = 0, *, verbose: bool = True
                       ) -> Optional[PipelineStep]:
        """Pop message at ``index`` and deliver exactly one hop."""
        if not self.queue:
            return None
        msg = self.pop_inflight_at(index)
        return self._deliver_one_message(msg, verbose=verbose)

    def process(self, max_steps: int = 100, verbose: bool = True) -> List[PipelineStep]:
        steps_this_call = 0
        while self.queue and steps_this_call < max_steps:
            msg = self.queue.popleft()
            self._deliver_one_message(msg, verbose=verbose)
            steps_this_call += 1

        return self.history

    def get_contract_state(self, address: str, contract_name: str,
                           method: str, params: Dict[str, Any] = None) -> JsonResult:
        if address not in self.registry:
            raise KeyError(f"No contract registered at {address}")
        tvc = self.registry[address]
        return self.tb.call(tvc, contract_name, method, params or {})

    def cleanup(self):
        # PID-scoped glob so a worker only removes its own deploy artifacts;
        # otherwise pytest-xdist workers race and one nukes the file another
        # one is about to read (FileNotFoundError on second use).
        pid = os.getpid()
        for p in self.tb.build_dir.glob(f"_pipeline_deploy_{pid}_*.tvc"):
            try:
                p.unlink()
            except FileNotFoundError:
                pass


def _load_contract_manifest(project_root: Path) -> List[Tuple[str, Optional[Path]]]:
    """Return [(contract_name, source_dir_or_none), ...] from contracts_manifest.json."""
    manifest = project_root / "contracts_manifest.json"
    if not manifest.exists():
        return []
    data = json.loads(manifest.read_text())
    entries: List[Tuple[str, Optional[Path]]] = []
    for item in data.get("contracts", []):
        if isinstance(item, str):
            entries.append((item, None))
        else:
            name = item["name"]
            sub = item.get("source", "")
            src_dir = project_root / sub if sub else None
            entries.append((name, src_dir))
    return entries


# Legacy name kept for dex-ported tests; bridge uses contracts_manifest.json.
DEX_CONTRACTS: List[str] = []


def _normalize_int(val):
    """Convert hex string or decimal string to int for comparison."""
    s = str(val)
    try:
        if s.startswith("0x") or s.startswith("0X"):
            return int(s, 16)
        return int(s)
    except (ValueError, TypeError):
        return None


def _values_equal(actual, expected) -> bool:
    """Compare two values, normalizing hex/decimal integers."""
    if str(actual) == str(expected):
        return True
    a_int = _normalize_int(actual)
    e_int = _normalize_int(expected)
    if a_int is not None and e_int is not None:
        return a_int == e_int
    return False


class TestBase:
    """Base class for bridge AN TVM contract testing."""
    __test__ = False

    def __init__(self, project_root: Optional[Path] = None,
                 tests_dir: Optional[Path] = None,
                 wasm_root_path: Optional[str] = None):
        env_root = os.environ.get("AN_PROJECT_ROOT") or os.environ.get("DEX_PROJECT_ROOT")
        if project_root is None and env_root:
            project_root = Path(env_root).resolve()

        tests_dir_auto: Optional[Path] = None
        if project_root is None:
            current = Path(__file__).parent
            while current != current.parent:
                if (current / "tools").exists():
                    project_root = current
                    break
                if (current / "contracts" / "tools").exists():
                    project_root = current / "contracts"
                    if (current / "tests").exists():
                        tests_dir_auto = current / "tests"
                    break
                current = current.parent
            else:
                raise RuntimeError(
                    "Could not find project root (tools/ or contracts/tools/)"
                )

        self.project_root = project_root
        self.tools_dir = project_root / "tools"
        self.build_dir = project_root / "build"
        self.contracts_dir = project_root
        self._contract_manifest = _load_contract_manifest(project_root)

        if tests_dir is not None:
            self.tests_dir = tests_dir
        elif tests_dir_auto is not None:
            self.tests_dir = tests_dir_auto
        else:
            self.tests_dir = project_root / "tests"

        self.build_dir.mkdir(parents=True, exist_ok=True)

        self.sold = self.tools_dir / "sold"
        self.tvm_debugger = self.tools_dir / "tvm-debugger"

        if wasm_root_path is not None:
            self.wasm_root_path = wasm_root_path
        else:
            candidates = [
                project_root / "wasm_binaries",
                project_root.parent / "build-artifacts" / "wasm_binaries",
            ]
            self.wasm_root_path = next(
                (str(d) for d in candidates if d.is_dir()), None
            )

        self.keys_dir = self.tests_dir / "keys"
        self.admin_keys = str(self.keys_dir / "test_admin.keys.json")
        self.admin_pubkey = self._load_pubkey(self.admin_keys)

        if not self.sold.exists():
            raise RuntimeError(f"TVM compiler not found: {self.sold}")
        if not self.tvm_debugger.exists():
            raise RuntimeError(f"TVM debugger not found: {self.tvm_debugger}")

    @staticmethod
    def _load_pubkey(keys_path: str) -> str:
        try:
            with open(keys_path) as f:
                keys = json.load(f)
            return keys["public"]
        except (FileNotFoundError, KeyError):
            return ""

    def compile_contract(self, contract_name: str, source_dir: Optional[Path] = None) -> bool:
        if source_dir is None:
            source_dir = self.contracts_dir
        contract_path = source_dir / f"{contract_name}.sol"
        if not contract_path.exists():
            print(f"Contract not found: {contract_path}")
            return False

        cmd = [
            str(self.sold),
            str(contract_path),
            "--output-dir", str(self.build_dir),
            "--tvm-version", "gosh",
            "--base-path", str(self.project_root),
        ]
        result = subprocess.run(cmd, capture_output=True, text=True)
        if result.returncode != 0:
            print(f"Compilation failed: {contract_name}")
            print(result.stderr)
            return False
        return True

    def compile_all(self) -> bool:
        manifest = self._contract_manifest
        if not manifest:
            # Toolchain-only mode — no contracts synced yet.
            return True
        all_exist = all(
            (self.build_dir / f"{name}.tvc").exists() and
            (self.build_dir / f"{name}.abi.json").exists()
            for name, _ in manifest
        )
        if all_exist:
            return True
        ok = True
        for name, src_dir in manifest:
            if not self.compile_contract(name, source_dir=src_dir or self.contracts_dir):
                ok = False
        return ok

    def compute_address_of(self, tvc_path: Path) -> str:
        with open(tvc_path, "rb") as f:
            b64 = base64.b64encode(f.read()).decode()
        r = subprocess.run(
            [str(self.tvm_debugger), "boc-hash"],
            input=b64, capture_output=True, text=True,
        )
        h = json.loads(r.stdout.strip().split("\n")[-1])["hash"]
        return f"0:{h}"

    def create_instance(self, contract_name: str, instance_name: str) -> Path:
        original_tvc = self.build_dir / f"{contract_name}.tvc"
        # PID-scope instance file so parallel workers don't clobber each
        # other's per-test deployments.
        instance_tvc = self.build_dir / (
            f"{contract_name}.{instance_name}_{os.getpid()}.tvc"
        )
        if not original_tvc.exists():
            raise FileNotFoundError(f"Original TVC not found: {original_tvc}")
        shutil.copy(original_tvc, instance_tvc)
        return instance_tvc

    def cleanup_instance(self, contract_name: str, instance_name: str):
        instance_tvc = self.build_dir / (
            f"{contract_name}.{instance_name}_{os.getpid()}.tvc"
        )
        if instance_tvc.exists():
            instance_tvc.unlink()

    def call(
        self,
        tvc_path: Path,
        contract_name: str,
        method: str,
        params: Dict[str, Any],
        sign_keys: str = None,
        address: str = "0:0000000000000000000000000000000000000000000000000000000000000000",
        block_seqno: Optional[int] = None,
        block_timestamp: Optional[int] = None,
        expire: Optional[int] = None,
        time_ms: Optional[int] = None,
    ) -> JsonResult:
        """Call a contract method as external message.

        `time_ms`: opcional explicit AbiHeader.time (uint64 milliseconds).
        Useful for replay-protection PoC where two consecutive calls must
        produce the SAME message bytes (same messageHash). If omitted —
        tvm-debugger uses the wall-clock time, so each call is unique.

        Notes on `expire` (HEAD `4ee91f3c` adds replay-protection in
        ReplayProtection mix-in for PrivateNote and similar). Contracts mark
        user-facing methods with `saveMsg` modifier; their afterSignatureCheck
        hook validates `body.expireAt`:
            require(expireAt > block.timestamp, ERR_MESSAGE_EXPIRED=402);
            require(expireAt < block.timestamp + 5 minutes, ERR_MESSAGE_WITH_HUGE_EXPIREAT=401).
        We auto-set expire = (block_timestamp or now) + 60s so legacy tests
        without explicit expire still pass replay-protection. Tests that need
        to exercise expired/too-far cases pass explicit `expire`.
        """
        abi_path = self.build_dir / f"{contract_name}.abi.json"
        cmd = [
            str(self.tvm_debugger), "run",
            "--input-file", str(tvc_path),
            "--abi-file", str(abi_path),
            "--function-name", method,
            "--call-parameters", json.dumps(params),
            "--address", address,
            "-j",
        ]
        if sign_keys:
            cmd.extend(["--sign", sign_keys])
        if block_seqno is not None:
            cmd.extend(["--block-seqno", str(block_seqno)])
        if block_timestamp is not None:
            cmd.extend(["--block-timestamp", str(block_timestamp)])
        if expire is None:
            base_ts = block_timestamp if block_timestamp is not None else int(time.time())
            expire = base_ts + 60
        abi_header = {"expire": int(expire)}
        if time_ms is not None:
            abi_header["time"] = int(time_ms)
        cmd.extend(["--abi-header", json.dumps(abi_header)])
        if self.wasm_root_path:
            cmd.extend(["--wasm-root-path", self.wasm_root_path])
        return self._execute_json(cmd, method, params)

    def call_internal(
        self,
        tvc_path: Path,
        contract_name: str,
        method: str,
        params: Dict[str, Any],
        sender: str,
        value: int = 1000000000,
        ecc: Optional[Dict[int, int]] = None,
        address: str = "0:0000000000000000000000000000000000000000000000000000000000000000",
        block_seqno: Optional[int] = None,
        block_timestamp: Optional[int] = None,
    ) -> JsonResult:
        """Call a contract method as internal message."""
        abi_path = self.build_dir / f"{contract_name}.abi.json"
        cmd = [
            str(self.tvm_debugger), "run",
            "--input-file", str(tvc_path),
            "--abi-file", str(abi_path),
            "--function-name", method,
            "--call-parameters", json.dumps(params),
            "--internal",
            "--message-source", sender,
            "--message-value", str(value),
            "--address", address,
            "-j",
        ]
        if ecc:
            ecc_json = json.dumps({str(k): str(v) for k, v in ecc.items()})
            cmd.extend(["--message-ecc", ecc_json])
        if block_seqno is not None:
            cmd.extend(["--block-seqno", str(block_seqno)])
        if block_timestamp is not None:
            cmd.extend(["--block-timestamp", str(block_timestamp)])
        if self.wasm_root_path:
            cmd.extend(["--wasm-root-path", self.wasm_root_path])
        return self._execute_json(cmd, method, params, internal=True, sender=sender)

    def _execute_json(
        self, cmd: list, method: str, params: Dict[str, Any],
        internal: bool = False, sender: str = "",
    ) -> JsonResult:
        msg_type = f"internal from {sender[:20]}..." if internal else "external"
        print(f"  [{msg_type}] {method}({json.dumps(params, ensure_ascii=False)[:80]})")

        result = subprocess.run(cmd, capture_output=True, text=True)
        stdout = result.stdout.strip()
        if stdout:
            data = None
            try:
                data = json.loads(stdout)
            except json.JSONDecodeError:
                for line in reversed(stdout.split("\n")):
                    line = line.strip()
                    if line.startswith("{"):
                        try:
                            data = json.loads(line)
                            break
                        except json.JSONDecodeError:
                            continue
            if data is not None:
                jr = JsonResult(
                    exit_code=data.get("exit_code", -1),
                    vm_success=data.get("vm_success", False),
                    gas_used=data.get("gas_used", 0),
                    response=data.get("response", {}),
                    messages=data.get("messages", []),
                    raw_json=data,
                    process_exit_code=result.returncode,
                    stderr=result.stderr,
                )
                status = "OK" if jr.vm_success else "FAIL"
                print(f"    {status} exit_code={jr.exit_code} gas={jr.gas_used}")
                if jr.response and jr.response != "{}":
                    resp_str = json.dumps(jr.response, ensure_ascii=False)
                    if len(resp_str) > 120:
                        resp_str = resp_str[:120] + "..."
                    print(f"    response={resp_str}")
                return jr

        print(f"    FAIL: could not parse JSON output")
        if result.stderr:
            print(f"    stderr: {result.stderr[:200]}")
        return JsonResult(
            exit_code=-1, vm_success=False, gas_used=0, response={},
            messages=[], raw_json={},
            process_exit_code=result.returncode, stderr=result.stderr,
        )

    def _ensure_test_abi(self, contract_name: str) -> Path:
        """Create a test ABI with __receive_trigger (funcId=0) for calling receive()."""
        test_abi = self.build_dir / f"{contract_name}.test.abi.json"
        if test_abi.exists():
            return test_abi
        abi_path = self.build_dir / f"{contract_name}.abi.json"
        with open(abi_path) as f:
            abi = json.load(f)
        abi["functions"].append({
            "name": "__receive_trigger",
            "id": "0x00000000",
            "inputs": [],
            "outputs": [],
        })
        with open(test_abi, "w") as f:
            json.dump(abi, f, indent=2)
        return test_abi

    def call_receive(
        self,
        tvc_path: Path,
        contract_name: str,
        sender: str,
        ecc: Dict[int, int],
        value: int = 1000000000,
        address: str = "0:0000000000000000000000000000000000000000000000000000000000000000",
    ) -> JsonResult:
        """Send an internal message triggering receive() with ECC currencies.

        Uses a test ABI trick: funcId=0 routes to receive() in TVM bytecode.
        """
        test_abi = self._ensure_test_abi(contract_name)
        ecc_json = json.dumps({str(k): str(v) for k, v in ecc.items()})
        cmd = [
            str(self.tvm_debugger), "run",
            "--input-file", str(tvc_path),
            "--abi-file", str(test_abi),
            "--function-name", "__receive_trigger",
            "--call-parameters", "{}",
            "--internal",
            "--message-source", sender,
            "--message-value", str(value),
            "--message-ecc", ecc_json,
            "--address", address,
            "-j",
        ]
        if self.wasm_root_path:
            cmd.extend(["--wasm-root-path", self.wasm_root_path])
        return self._execute_json(
            cmd, f"receive(ecc={ecc_json})", {},
            internal=True, sender=sender,
        )

    def extract_code_boc(self, contract_name: str) -> str:
        """Extract code BOC (base64) from a compiled TVC file."""
        tvc_path = self.build_dir / f"{contract_name}.tvc"
        result = subprocess.run(
            [str(self.tvm_debugger), "state-decode", "--state-init", str(tvc_path)],
            capture_output=True, text=True,
        )
        if result.returncode != 0:
            raise RuntimeError(f"state-decode failed for {contract_name}: {result.stderr}")
        state = json.loads(result.stdout)
        return state["code"]

    @staticmethod
    def _boc_root_depth(boc_bytes: bytes) -> int:
        """Return ``TvmCell.depth()`` for the root cell of a standard TVM BOC."""
        if struct.unpack(">I", boc_bytes[:4])[0] != 0xB5EE9C72:
            raise ValueError("unsupported BOC magic (expected 0xb5ee9c72)")
        flags = boc_bytes[4]
        ref_size = flags & 7
        offset_size = boc_bytes[5]
        pos = 6
        cells_count = int.from_bytes(boc_bytes[pos:pos + ref_size], "big")
        pos += ref_size * 3 + offset_size + ref_size

        raw_cells: List[Tuple[int, List[int]]] = []
        for _ in range(cells_count):
            d1 = boc_bytes[pos]
            pos += 1
            d2 = boc_bytes[pos]
            pos += 1
            refs_count = d1 & 7
            if d1 & 0x80:
                data_len = d2 // 2
            else:
                data_len = d2 // 2 + (1 if d2 & 1 else 0)
            pos += data_len
            refs = [
                int.from_bytes(boc_bytes[pos + i * ref_size:pos + (i + 1) * ref_size], "big")
                for i in range(refs_count)
            ]
            pos += refs_count * ref_size
            raw_cells.append((refs_count, refs))

        depths = [0] * cells_count
        for i in range(cells_count - 1, -1, -1):
            refs_count, refs = raw_cells[i]
            depths[i] = 0 if refs_count == 0 else 1 + max(depths[r] for r in refs)

        root_idx = int.from_bytes(
            boc_bytes[6 + ref_size * 3 + offset_size:6 + ref_size * 3 + offset_size + ref_size],
            "big",
        )
        return depths[root_idx]

    def tvm_hash_boc(self, code_boc_b64: str) -> str:
        """``tvm.hash(cell)`` for a base64-encoded code BOC → ``0x`` hex."""
        result = subprocess.run(
            [str(self.tvm_debugger), "boc-hash"],
            input=code_boc_b64,
            capture_output=True,
            text=True,
            check=True,
        )
        line = result.stdout.strip().split("\n")[-1]
        hash_hex = json.loads(line)["hash"]
        return "0x" + hash_hex

    def tvm_depth_boc(self, code_boc_b64: str) -> int:
        """``TvmCell.depth()`` for a base64-encoded code BOC."""
        return self._boc_root_depth(base64.b64decode(code_boc_b64))

    def orderbook_pn_code_patches(self, pn_code_boc: str) -> Dict[str, str]:
        """OrderBook identity fields (HEAD b0f9bc0+: hash/depth, not full code).

        OB stores ``_privateNoteCodeHash`` + ``_privateNoteCodeDepth`` and
        resolves PN senders via ``DexLib.computePrivateNoteAddressFromHash``.
        """
        if not hasattr(self, "_ob_pn_code_meta_cache"):
            self._ob_pn_code_meta_cache: Dict[str, Tuple[str, int]] = {}
        if pn_code_boc not in self._ob_pn_code_meta_cache:
            self._ob_pn_code_meta_cache[pn_code_boc] = (
                self.tvm_hash_boc(pn_code_boc),
                self.tvm_depth_boc(pn_code_boc),
            )
        code_hash, code_depth = self._ob_pn_code_meta_cache[pn_code_boc]
        return {
            "_privateNoteCodeHash": code_hash,
            "_privateNoteCodeDepth": str(code_depth),
        }

    def patch_state(
        self,
        tvc_path: Path,
        contract_name: str,
        patches: Dict[str, Any],
    ) -> None:
        """Patch specific fields in a contract's persistent state."""
        abi_path = self.build_dir / f"{contract_name}.abi.json"
        with open(abi_path) as f:
            abi = json.load(f)
        fields = abi["fields"]

        state = json.loads(subprocess.run(
            [str(self.tvm_debugger), "state-decode", "-s", str(tvc_path)],
            capture_output=True, text=True, check=True,
        ).stdout)

        # PID-scope fields.json — pytest-xdist workers race on a
        # contract-shared name and overwrite each other's schema while
        # the next boc-decode/encode is still reading.
        fields_file = str(
            self.build_dir / f"{contract_name}.fields_{os.getpid()}.json"
        )
        with open(fields_file, "w") as f:
            json.dump(fields, f)

        decoded = json.loads(subprocess.run(
            [str(self.tvm_debugger), "boc-decode", "-b", state["data"], "-p", fields_file],
            capture_output=True, text=True, check=True,
        ).stdout)["data"]

        for k, v in patches.items():
            if k not in decoded:
                raise KeyError(f"Field '{k}' not found in state. "
                               f"Available: {list(decoded.keys())}")
            if isinstance(v, (dict, list)):
                decoded[k] = v
            elif isinstance(v, str):
                try:
                    parsed = json.loads(v)
                    if isinstance(parsed, (dict, list)):
                        decoded[k] = parsed
                    else:
                        decoded[k] = str(v)
                except (json.JSONDecodeError, ValueError):
                    decoded[k] = str(v)
            else:
                decoded[k] = str(v)

        # PID-scope patched_data.json — pytest-xdist workers race on a
        # contract-shared name and one worker's encode picks up another's data.
        patched_file = str(self.build_dir / f"{contract_name}.patched_data_{os.getpid()}.json")
        with open(patched_file, "w") as f:
            json.dump(decoded, f)

        new_data = json.loads(subprocess.run(
            [str(self.tvm_debugger), "boc-encode", "-d", patched_file, "-p", fields_file],
            capture_output=True, text=True, check=True,
        ).stdout)["boc"]

        new_state = json.loads(subprocess.run(
            [str(self.tvm_debugger), "state-encode", "-c", state["code"], "-d", new_data],
            capture_output=True, text=True, check=True,
        ).stdout)

        with open(str(tvc_path), "wb") as f:
            f.write(base64.b64decode(new_state["state_init"]))

        applied = ", ".join(f"{k}={v}" for k, v in patches.items())
        print(f"  [patch] {contract_name}: {applied}")

    def encode_cell(self, param_types: List[Dict], values: Dict[str, Any]) -> str:
        """Encode values into a TvmCell BOC (base64) using ABI encoding.

        PID-scope params/data files: pytest-xdist workers race on
        _encode_params.json / _encode_data.json otherwise — one worker writes
        while another reads, producing CalledProcessError on shared TVC paths.
        """
        pid = os.getpid()
        params_file = str(self.build_dir / f"_encode_params_{pid}.json")
        data_file = str(self.build_dir / f"_encode_data_{pid}.json")
        with open(params_file, "w") as f:
            json.dump(param_types, f)
        with open(data_file, "w") as f:
            json.dump(values, f)
        result = subprocess.run(
            [str(self.tvm_debugger), "boc-encode", "-d", data_file, "-p", params_file],
            capture_output=True, text=True,
        )
        if result.returncode != 0:
            raise RuntimeError(f"boc-encode failed: {result.stderr}")
        return json.loads(result.stdout)["boc"]

    # ================================================================
    # Assertions
    # ================================================================

    def assert_success(self, result: JsonResult, message: str = ""):
        if not result.vm_success:
            raise AssertionError(
                f"VM execution failed: {message}\n"
                f"exit_code={result.exit_code}, response={result.response}\n"
                f"stderr={result.stderr}"
            )

    @staticmethod
    def is_reverted(result: JsonResult) -> bool:
        """True iff the call ended in a revert (compute exception, possibly
        post-`tvm.accept()`). Use this in tests instead of `not vm_success`
        when the contract method has the `accept` modifier (or calls
        `tvm.accept()` early), because in that case `vm_success` is True even
        when the body reverts via `require(...)`.
        """
        return (not result.vm_success) or result.exit_code != 0

    def assert_response(self, result: JsonResult, key: str, expected: Any, message: str = ""):
        self.assert_success(result, message)
        resp = result.response
        if isinstance(resp, str):
            try:
                resp = json.loads(resp)
                result.response = resp
            except (json.JSONDecodeError, TypeError):
                pass
        if not isinstance(resp, dict):
            raise AssertionError(f"Response is not a dict: {resp}\n{message}")
        actual = result.response.get(key)
        if not _values_equal(actual, expected):
            raise AssertionError(
                f"Response mismatch for '{key}': {message}\n"
                f"Expected: {expected!r}\nActual: {actual!r}\n"
                f"Full response: {result.response}"
            )
        print(f"    CHECK {key} == {expected!r}")

    def assert_failure(self, result: JsonResult, expected_exit_code: Optional[int] = None,
                       message: str = ""):
        # NOTE: A method that calls `tvm.accept()` (or uses the `accept`
        # modifier) can revert AFTER acceptance. tvm-debugger then reports
        # `vm_success=True` (action phase accepted) but `exit_code` still
        # reflects the compute-phase revert. Treat any non-zero exit_code as
        # failure for the purpose of this helper, regardless of vm_success.
        is_failure = (not result.vm_success) or result.exit_code != 0
        if not is_failure:
            raise AssertionError(
                f"Expected VM failure but succeeded: {message}\n"
                f"exit_code={result.exit_code}, response={result.response}"
            )
        if expected_exit_code is not None and result.exit_code != expected_exit_code:
            raise AssertionError(
                f"Wrong exit code: {message}\n"
                f"Expected: {expected_exit_code}\nActual: {result.exit_code}"
            )
        ec_msg = f" (exit_code={result.exit_code})" if expected_exit_code else ""
        print(f"    CHECK: VM failed as expected{ec_msg}")

    def assert_messages_contain(self, result: JsonResult, dest: str = None, message: str = ""):
        self.assert_success(result, message)
        if not result.messages:
            raise AssertionError(f"No outgoing messages: {message}")
        if dest:
            dests = [m.get("destination", "") for m in result.messages]
            if dest not in dests:
                raise AssertionError(
                    f"Message to {dest} not found: {message}\n"
                    f"Actual destinations: {dests}"
                )
        print(f"    CHECK: {len(result.messages)} outgoing message(s)")
