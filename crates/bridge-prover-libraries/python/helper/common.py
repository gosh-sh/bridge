import json
import os
import platform
import re
import shutil
import socket
import subprocess
import sys
import time

from dataclasses import asdict, is_dataclass
from dataclasses import dataclass
from datetime import datetime
from enum import Enum
from pathlib import Path
import urllib.error
import urllib.request
from urllib.parse import urlparse

COMPILER_DIR = "./contracts/compiler"
SOLD = os.getenv('SOLD', shutil.which("sold") or f"{COMPILER_DIR}/sold")


def _resolve_tvm_cli():
    """Pick a tvm-cli that actually runs here.

    `deploy_msig_and_mint.py` prepends python/bin to PATH, so a committed
    binary built for another OS/arch used to win over a working system
    install and die with "Exec format error". Explicit CLI_NAME always
    wins; otherwise take the first candidate that answers `version`.
    """
    import subprocess

    explicit = os.getenv('CLI_NAME')
    if explicit:
        return explicit

    # EVERY match on PATH, not just the first. `shutil.which` stops at the
    # first hit, so a broken binary in an early PATH entry — exactly what
    # the python/bin injection used to be — would be tried, rejected, and
    # then skipped straight past the working system install to
    # COMPILER_DIR. Walking PATH ourselves is what makes "the first
    # candidate that answers `version`" true rather than aspirational.
    seen = set()
    candidates = []
    for entry in os.environ.get("PATH", "").split(os.pathsep):
        if not entry:
            continue
        cand = os.path.join(entry, "tvm-cli")
        if cand not in seen and os.path.isfile(cand) and os.access(cand, os.X_OK):
            seen.add(cand)
            candidates.append(cand)
    fallback = f"{COMPILER_DIR}/tvm-cli"
    if fallback not in seen:
        candidates.append(fallback)

    for cand in candidates:
        try:
            subprocess.run([cand, "version"], check=True,
                           # `timeout` and `stdin`, both load-bearing. This
                           # runs an unknown executable found on PATH: one
                           # that reads stdin, or sits on a stale network
                           # mount, blocks here forever. `version` answers
                           # in milliseconds, so five seconds is generous
                           # and still bounded — and a candidate that
                           # cannot manage it is not one to drive a burn
                           # with.
                           timeout=5,
                           stdin=subprocess.DEVNULL,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            return cand
        except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired):
            continue
    return candidates[0] if candidates else "tvm-cli"


# Resolved on FIRST USE, not at import.
#
# This was `TVM_CLI = _resolve_tvm_cli()` at module level, so `import
# helper.common` executed every `tvm-cli` on PATH — before the timeout
# above, an import could hang forever, and it still spawns processes for
# anything that merely imports this module: a linter, a test collector, an
# editor's language server.
#
# Cached after the first call, so the walk happens once per process just
# as it did before.
_TVM_CLI = None


def tvm_cli() -> str:
    global _TVM_CLI
    if _TVM_CLI is None:
        _TVM_CLI = _resolve_tvm_cli()
        # STDERR, and once. This banner used to be `__check_cli__()` at
        # module level printing to stdout — so `import helper.common`
        # both spawned processes and wrote to stdout, and
        # `deploy_msig_and_mint.py`'s entire contract is that its stdout
        # holds nothing but two eval-able `export` lines.
        print(f"Using tvm-cli: {_TVM_CLI}", file=sys.stderr)
    return _TVM_CLI


def __getattr__(name):
    """Keep `common.TVM_CLI` and `from helper.common import TVM_CLI` working.

    Module-level `__getattr__` (PEP 562) is consulted only for attributes
    that are NOT found normally, which is exactly why the eager assignment
    had to go. It does not cover uses of the bare name INSIDE this module
    — those resolve as globals — so the call sites here use `tvm_cli()`.
    """
    if name == "TVM_CLI":
        return tvm_cli()
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
TVM_DEBUGGER = os.getenv('TVM_DEBUGGER', shutil.which("tvm-debugger") or f"{COMPILER_DIR}/tvm-debugger")

NETWORK = os.getenv('NETWORK', 'http://127.0.0.1:80')
WORK_DIR = None
GIVER_PATH = "tests/GiverV3"

WAS_ERROR = False
ECC_KEY = 2

RFC1918 = re.compile(
    r"^(10\.)|"
    r"(192\.168\.)|"
    r"(172\.(1[6-9]|2[0-9]|3[0-1])\.)"
)


class SkippedReason(Enum):
    NoState = 0
    BadState = 1
    NoGas = 2
    Suspended = 3


def __check_cli__():
    """Print the resolved tvm-cli and its full version banner.

    No longer called at import. It used to be, at the bottom of this
    module, which meant importing the helpers ran two subprocesses — one
    of them through a shell, neither with a timeout — so anything that
    merely imported this file could hang, and its output landed on
    stdout. `tvm_cli()` now announces the chosen binary on stderr the
    first time it resolves; this remains for a caller that wants the
    version text too.
    """
    print(f"Checking cli specified in library: {tvm_cli()}", file=sys.stderr)
    execute_cmd(f"{tvm_cli()} version")


def setup():
    execute_cli_cmd(f"config clear")
    execute_cli_cmd(f"config --url {NETWORK}")


def set_config(options: dict):
    options_str = ""
    for key in options:
        options_str = f"{options_str} --{key} {options[key]}"
    execute_cli_cmd(f"config {options_str}")

def execute_cmd(command: str, work_dir=None, ignore_error=False, silent=False):
    global WAS_ERROR
    # `cwd=`, not `cd {work_dir} &&`. The directory came from
    # BRIDGE_WORK_DIR and was pasted into a shell command line, so an
    # ordinary path with a space broke every call and one containing
    # `;` or `$(…)` ran. `cwd=` hands the path to the kernel instead of
    # to a parser.
    #
    # `command` itself is still shell-interpreted — the callers build
    # `tvm-cli …` strings with interpolated paths, and moving them to
    # argument lists is a larger change than this. That is the remaining
    # exposure here, and it is now the only one.
    WAS_ERROR = False
    if not silent:
        print(f"[{work_dir or '.'}] {command}")
    try:
        output = subprocess.check_output(
            command, shell=True, cwd=work_dir, stderr=subprocess.STDOUT
        ).decode("utf-8")
        print(output)
    except subprocess.CalledProcessError as e:
        output = e.output.decode("utf-8")
        WAS_ERROR = True
        if not ignore_error:
            print(f"Command `{command}` execution failed: {output} {e.stderr}")
            exit(1)

    return output.strip()


def execute_cli_cmd(cmd: str, print_output=False) -> dict:
    cmd = f"{tvm_cli()} -j {cmd}"
    output = execute_cmd(cmd, WORK_DIR, True, False)

    if print_output:
        print(f"Execution result: {output}")
    try:
        res = json.loads(output)
    except json.JSONDecodeError:
        res = None
        print(f"Failed to decode output as json: '{output}'")
    return res

def execute_cmd_without_exit(command: str, work_dir=None, ignore_error=False, silent=False):
    global WAS_ERROR
    # See `execute_cmd`: `cwd=` rather than a shell `cd`, for the same
    # reason and with the same remaining caveat about `command`.
    WAS_ERROR = False
    if not silent:
        print(f"[{work_dir or '.'}] {command}")
    try:
        output = subprocess.check_output(
            command, shell=True, cwd=work_dir, stderr=subprocess.STDOUT
        ).decode("utf-8")
        print(output)
    except subprocess.CalledProcessError as e:
        output = e.output.decode("utf-8")
        WAS_ERROR = True
        #if not ignore_error:
        print(f"Command `{command}` execution failed: {output} {e.stderr}")

    return output.strip()

def execute_cli_cmd_without_exit(cmd: str) -> dict:
    cmd = f"{tvm_cli()} -j {cmd}"
    output = execute_cmd_without_exit(cmd, WORK_DIR, True, False)
    try:
        res = json.loads(output)
    except json.JSONDecodeError:
        res = None
        print(f"Failed to decode output as json: '{output}'")
    return res

def get_contract_path_stem(input_path: str) -> str:
    if input_path.endswith(".tvc"):
        input_path = input_path.split(".tvc")[0]
    if input_path.endswith(".sol"):
        input_path = input_path.split(".sol")[0]
    if input_path.endswith(".abi"):
        input_path = input_path.split(".abi")[0]
    return input_path


def get_code(tvc_path: str) -> str:
    subcmd = f"decode stateinit --tvc {tvc_path}"
    return execute_cli_cmd(subcmd, True)["code"]


def generate_address(contract_path: str, key_path: str = None) -> str:
    contract_path = get_contract_path_stem(contract_path)
    if key_path is None:
        keys_option = f' --genkey {contract_path}.keys.json'
    else:
        if os.path.isabs(key_path):
            abs_key_path = key_path
        else:
            abs_key_path = f'{WORK_DIR}/{key_path}'
        if os.path.exists(abs_key_path):
            keys_option = f' --setkey {key_path}'
        else:
            keys_option = f' --genkey {key_path}'

    subcmd = f'genaddr --abi {contract_path}.abi.json {keys_option} --save {contract_path}.tvc'
    return execute_cli_cmd(subcmd)["raw_address"]

def generate_address_with_init_data(contract_path: str, init_data: str = None, key_path: str = None) -> str:
    contract_path = get_contract_path_stem(contract_path)
    if key_path is None:
        keys_option = f' --genkey {contract_path}.keys.json'
    else:
        if os.path.isabs(key_path):
            abs_key_path = key_path
        else:
            abs_key_path = f'{WORK_DIR}/{key_path}'
        if os.path.exists(abs_key_path):
            keys_option = f' --setkey {key_path}'
        else:
            keys_option = f' --genkey {key_path}'

    subcmd = f'genaddr --data {init_data} --abi {contract_path}.abi.json {keys_option} --save {contract_path}.tvc'
    return execute_cli_cmd(subcmd)["raw_address"]

def send_from_giver(address: str, value: int):
    with open(f'{GIVER_PATH}.address', 'r') as file:
        giver_address = file.read().rstrip()
    print(f"{giver_address=}")
    subcmd = f'call --abi {GIVER_PATH}.abi.json --keys \
{GIVER_PATH}.keys.json {giver_address} sendCurrencyWithFlag \'{{\"value\":{value},\"bounce\":false,\"dest\":\"{address}\",\"flag\":16,\"ecc\":{{\"2\": {value}}}}}\''
    execute_cli_cmd(subcmd)
    subcmd = f'call --abi {GIVER_PATH}.abi.json --keys \
{GIVER_PATH}.keys.json {giver_address} sendCurrencyWithFlag \'{{\"value\":{value},\"bounce\":false,\"dest\":\"{address}\",\"flag\":2,\"ecc\":{{\"2\": {value}}}}}\''
    execute_cli_cmd(subcmd)

def get_account(address: str, print_output=False) -> dict:
    subcmd = f"account {address}"
    return execute_cli_cmd(subcmd, print_output)

def get_balance(address: str) -> int:
    return get_account(address)["balance"]


def is_account_active(address: str) -> bool:
    account = get_account(address)
    if 'acc_type' not in account:
        return False
    return account["acc_type"] == "Active"

def wait_account_status(address: str, status: str) -> bool:
    start_time = time.time()
    timeout = 10  # seconds

    while time.time() - start_time < timeout:
        account = get_account(address)
        if 'acc_type' in account.keys() and account['acc_type'] == status:
            return True
        time.sleep(.5)

    return False


def wait_account_uninit(address: str):
    result = wait_account_status(address, "Uninit")
    assert result == True


# Sleep with progress bar
def sleep(interval: int):
    print(f"{datetime.now().strftime('%H:%M:%S')} Sleep {interval}s")

    try:
        from tqdm import tqdm
        for _i in tqdm(range(interval)):
            time.sleep(1)
    except ImportError as e:
        time.sleep(interval)



def wait_for_block_seq_no(block_seq_no: int, max_attempts: int = 20):
    while True:
        max_attempts -= 1
        assert max_attempts >= 0
        last_seq_no = get_latest_block_seq_no()
        break_flag = True
        if last_seq_no is not None:
            if last_seq_no < block_seq_no:
                break_flag = False
        else:
            break_flag = False
        if not break_flag:
            sleep(10)
            continue
        break

def wait_account_active(address: str):
    result = wait_account_status(address, "Active")
    assert result == True


def format_params(params: dict) -> str:
    if params is None:
        return ""

    def custom_serializer(obj, seen=None):
        if seen is None:
            seen = set()

        if isinstance(obj, (int, float, bool, str)):
            return obj
        if obj is None:
            return None
        if isinstance(obj, set):
            return list(obj)
        if isinstance(obj, list):
            return [custom_serializer(item, seen) for item in obj]
        if isinstance(obj, dict):
            return {key: custom_serializer(value, seen) for key, value in obj.items()}
        if is_dataclass(obj):
            return {key: custom_serializer(value, seen) for key, value in asdict(obj).items()}

        return obj

    formatted_params = json.dumps(custom_serializer(params), separators=(",", ":"), allow_nan=False).replace("'", "'\"'\"'")
    return f'\'{formatted_params}\''

def deploy_contract(
        contract_path: str,
        value: int = 100_000_000_000,
        key_path: str = None,
        constructor_params: dict = {},
        abi_path: str = None
) -> str:
    contract_path = get_contract_path_stem(contract_path)
    contract_address = generate_address(f"{contract_path}.tvc", key_path)
    send_from_giver(contract_address, value)

    max_attempts = 10
    while True:
        max_attempts -= 1
        assert max_attempts != 0
        account = get_account(contract_address)
        if 'acc_type' in account:
            break
        time.sleep(10)

    if key_path is None:
        key_path = f"{contract_path}.keys.json"
    if abi_path is None:
        abi_path = f"{contract_path}.abi.json"
    execute_cli_cmd(f"deployx --abi {abi_path} --keys {key_path} {contract_path}.tvc {format_params(constructor_params)}")

    check_attempts = 10
    while True:
        account = get_account(contract_address)
        if "acc_type" in account:
            if account["acc_type"] == "Active":
                return contract_address
        check_attempts -= 1
        if check_attempts == 0:
            raise NameError('Failed to deploy contract')
        time.sleep(10)


def call_contract(address: str, abi: str, keys: str|None, method: str, params=None, print_output=False) -> dict:
    if isinstance(params, dict) or params is None:
        params = format_params(params)

    arg_keys = f"--keys {keys}" if keys is not None else ""
    return execute_cli_cmd(
        f"callx --abi {abi} --addr {address} {arg_keys} -m {method} {params}",
        print_output
    )


def run_getter(address: str, abi: str, method: str, params: dict = None) -> dict:
    return execute_cli_cmd(
        f"runx --abi {abi} --addr {address} -m {method} {format_params(params)}"
    )


def tons(value) -> int:
    return int(value) * 1_000_000_000


def gen_keys(path: str):
    execute_cli_cmd(f"genphrase --dump {path}")


def read_public_key(path: str) -> str:
    with open(path, 'r') as file:
        key_pair = file.read().rstrip()
    pair = json.loads(key_pair)
    return pair["public"]

def read_bls_public_key(path: str) -> str:
    with open(path, 'r') as file:
        key_pair = file.read().rstrip()
    pair = json.loads(key_pair)[0]
    return pair["public"]


def generate_bls_key(path):
    bls_key = execute_cmd(f"./target/release/node-helper bls")
    key_pair = json.loads(bls_key)

    with open(path, 'w') as file:
        file.write(json.dumps(key_pair, indent=2))
    return key_pair["public"]


def get_latest_block_seq_no():
    data = execute_graphql_query("query{blockchain{blocks(last:1){edges{node{seq_no}}}}}")
    edges = data.get("data", {}).get("blockchain", {}).get("blocks", {}).get("edges", [])
    if edges:
        return edges[0]["node"]["seq_no"]
    return None


def get_accounts_by_code_hash(code_hash: str):
    cmd = f'''query-raw accounts "id" --order '[ {{ "path": "last_paid", "direction": "DESC" }} ]' --filter '{{"code_hash":{{"eq":"{code_hash}"}}}}' '''
    data = execute_cli_cmd(cmd)
    accounts = []
    for acc in data:
        accounts.append(acc['id'])
    return accounts


def get_error_extensions(result: dict) -> dict:
    return result.get('Error', {}).get('data', {}).get('node_error', {}).get('extensions', {})


def is_tvm_error(result: dict, err_code: int) -> bool:
    return get_error_extensions(result).get('details', {}).get('exit_code') == err_code


def is_compute_skipped(result: dict, exit_code: SkippedReason = None) -> bool:
    ext = get_error_extensions(result)
    return ext.get('code') == "COMPUTE_SKIPPED" and (exit_code is None or ext.get('details', {}).get('exit_code') == exit_code.value)


def is_ok(result) -> bool:
    return 'Error' not in result.keys()


def is_error(result) -> bool:
    return not is_ok(result)


@dataclass
class Message:
    source: str = ""
    destination: str = ""
    value: int = 0
    state_init: bool = False
    boc: str = ""
    body_hex: str = ""
    serialized_data: str = ""


def get_messages_from_linker_test_log(output: str):
    messages = []
    cur_message = Message()
    first_message = True

    start_message_lookup = 'Action(SendMsg)'
    source_lookup = "   source      "
    dest_lookup = "  destination "
    value_lookup = "   value       "
    init_lookup = "init  : "
    boc_lookup = "boc_base64:"
    body_lookup = "body_base64: "
    serialized_data = "serialized_data: "

    for line in output.split('\n'):
        if start_message_lookup in line:
            if first_message:
                first_message = False
            else:
                messages.append(cur_message)
                cur_message = Message()
        if source_lookup in line:
            cur_message.source = line.split(":")[-1].strip()
        if dest_lookup in line:
            cur_message.destination = line.split(":")[-1].strip()
        if value_lookup in line:
            cur_message.value = line.split(":")[-1].strip()
        if init_lookup in line:
            cur_message.state_init = line.split(":")[-1].strip() != "None"
        if boc_lookup in line:
            cur_message.boc = line.split(":")[-1].strip()
        if body_lookup in line:
            cur_message.body_hex = line.split(":")[-1].strip()
        if serialized_data in line:
            cur_message.serialized_data = line.split(":")[-1].strip()
    messages.append(cur_message)
    return messages


def test_wrapper(func):
    def wrapper(*args, **kwargs):
        print(f"running: {func.__name__}...")
        try:
            result = func(*args, **kwargs)
            print(f"{func.__name__}: ok\n")
            return result
        except Exception as e:
            print(f"error: {e}")
            print(f"{func.__name__}: fail\n")
            sys.exit(1)

    return wrapper


def num2addr(value: int) -> str:
    return f"0:{value:064x}"


def hex2dec(value: str) -> int:
    return int(value, 16) if value.startswith("0x") else int(value)


def get_map_bitmask_thread(filename: str) -> dict[str, str]:
    pattern = re.compile(
        r"Bitmask\s*\{\s*mask_bits:\s*([0-9a-fA-F]+).*?ThreadIdentifier<([0-9a-fA-F]+)>",
        re.DOTALL,
    )

    result = {}

    with open(filename, "r", encoding="utf-8") as f:
        content = f.read()

        matches = pattern.findall(content)
        for mask_bits, thread_id in matches:
            result[mask_bits] = thread_id

    return result

def remove_files(path: str):
    p = Path(path)

    if not p.exists():
        print(f"Path does not exist: {p}")
        return
    if os.getenv("CMD_PREFIX") is not None:
        assert p != Path("/")
        execute_cmd(f"sudo rm -rf {p} | true")
        print(f"Removed path with sudo: {p}")
        return
    if p.is_file() or p.is_symlink():
        p.unlink()
        print(f"Removed file: {p}")
    elif p.is_dir():
        shutil.rmtree(p)
        print(f"Removed directory (recursively): {p}")
    else:
        print(f"Unknown file type: {p}")


def get_default_gateway_ip():
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect(("1.1.1.1", 80))  # Cloudflare DNS
        ip = s.getsockname()[0]
        s.close()
        return ip
    except Exception as e:
        return f"Error: {e}"


def get_lan_ipv4() -> str:
    system = platform.system()

    if system == "Windows":
        return _lan_ipv4_windows()
    elif system == "Darwin":
        return _lan_ipv4_macos()
    elif system == "Linux":
        return _lan_ipv4_linux()
    else:
        raise RuntimeError(f"Unsupported OS: {system}")


def _lan_ipv4_windows() -> str:
    out = subprocess.check_output(
        ["ipconfig"], text=True, encoding="utf-8", errors="ignore"
    )

    for line in out.splitlines():
        if "IPv4 Address" in line or "IPv4-адрес" in line:
            ip = line.split(":")[-1].strip()
            if RFC1918.match(ip):
                return ip

    raise RuntimeError("LAN IPv4 not found on Windows")


def _lan_ipv4_macos() -> str:
    out = subprocess.check_output(["ifconfig"], text=True)

    for line in out.splitlines():
        line = line.strip()
        if not line.startswith("inet "):
            continue
        if "broadcast" not in line:
            continue
        if "-->" in line:
            continue

        ip = line.split()[1]
        if RFC1918.match(ip):
            return ip

    raise RuntimeError("LAN IPv4 not found on macOS")


def _lan_ipv4_linux() -> str:
    out = subprocess.check_output(
        ["ip", "-4", "-o", "addr", "show"], text=True
    )

    for line in out.splitlines():
        parts = line.split()
        ip = parts[3].split("/")[0]
        if RFC1918.match(ip):
            return ip

    raise RuntimeError("LAN IPv4 not found on Linux")

def _get_tvm_cli_endpoint() -> str:
    data = execute_cli_cmd('config --list')
    endpoints = data.get("endpoints", [])
    if endpoints:
        return endpoints[0]
    return data["url"]


def _normalize_http_endpoint(endpoint: str) -> str:
    parsed = urlparse(endpoint)
    if parsed.scheme:
        return endpoint
    return f"http://{endpoint}"


_TRANSIENT_HTTP_STATUSES = {502, 503, 504}


def execute_graphql_query(
    query: str,
    endpoint=None,
    *,
    retries: int = 5,
    backoff: float = 0.5,
    timeout: float = 10.0,
) -> dict:
    if endpoint is None:
        endpoint = _get_tvm_cli_endpoint()
    endpoint = _normalize_http_endpoint(endpoint)

    url = f"{endpoint.rstrip('/')}/graphql"
    body = json.dumps({"query": query}).encode("utf-8")

    for attempt in range(retries):
        request = urllib.request.Request(
            url,
            data=body,
            headers={"Content-Type": "application/json"},
        )
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                return json.loads(response.read().decode("utf-8"))
        except urllib.error.HTTPError as e:
            if e.code in _TRANSIENT_HTTP_STATUSES and attempt < retries - 1:
                time.sleep(backoff * (2 ** attempt))
                continue
            raise
        except urllib.error.URLError:
            if attempt < retries - 1:
                time.sleep(backoff * (2 ** attempt))
                continue
            raise

    raise RuntimeError("execute_graphql_query: retry loop exhausted without result")


ZERO_DAPP = "0" * 64
# All DEX contracts (RootPN/PrivateNote/PMP/OrderBook and RootOracle/Oracle/
# OracleEventList) are deployed in dapp_id 0 — matches the node's hardcoded
# ext_out tracking (DEFAULT_TRACKED_ROOT_PN_ROUTING) required by the voucher
# ZK-proof pipeline. The tvm-cli v3 "<dapp>::<acc>" wrapping below still applies.
DEX_DAPP_ID = ZERO_DAPP


def acc_id_of(addr: str) -> str:
    """Extract the bare 64-hex account_id from any address form:
        "0:<acc>", "<acc>", or "<dapp>::<acc>".
    """
    if "::" in addr:
        acc = addr.split("::", 1)[1]
    elif ":" in addr:
        wc, _, acc = addr.partition(":")
        if wc != "0":
            raise ValueError(f"non-zero workchain not supported: {wc!r}")
    else:
        acc = addr

    if len(acc) != 64 or not all(c in "0123456789abcdefABCDEF" for c in acc):
        raise ValueError(f"account_id must be 64 hex chars, got: {acc!r}")
    return acc


def to_dapp_address(addr: str, dapp: str = ZERO_DAPP) -> str:
    """Convert any address form to the tvm-cli v3 "<dapp_id>::<account_id>"
    CLI-target form, applying the given dapp_id.

    Accepts "0:<acc>", "<acc>", or "<dapp>::<acc>" — the account_id is
    re-extracted, so it is safe to call on an already-wrapped address (the
    dapp_id is replaced by `dapp`). Used for `--addr`/`account`/`runx`/`callx`
    targets; ABI payload address fields must keep the legacy "0:<acc>" form.
    """
    return f"{dapp}::{acc_id_of(addr)}"


def to_legacy_address(addr: str) -> str:
    """ABI payload address fields ('dest', 'recipient', 'to', …) must stay in
    the legacy '0:<account_id>' form even under tvm-cli v3. Accepts any form."""
    return "0:" + acc_id_of(addr)

