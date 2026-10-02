# bins_linux/ — acki-nacki tool binaries for Linux

Five native binaries the acki-nacki multithread test harness
(`tests/mt/cli.py`) needs, plus a sourceable `env.sh` that exports
them for manual runs. `runbooks/run_multipath.sh` picks this
directory up automatically on Linux (see the `uname -s` switch in
that script).

## Contents

| File | Env var it becomes | Where it originally comes from |
|---|---|---|
| `tvm-cli` | `CLI_NAME`, `TVM_CLI` | `acki-nacki/contracts/compiler/tvm-cli` (downloaded release) or a local `tvm-sdk cargo build --release` |
| `sold` | `SOLD` | `acki-nacki/contracts/compiler/sold` |
| `tvm-debugger` | `TVM_DEBUGGER` | `acki-nacki/contracts/compiler/tvm-debugger` |
| `zerostate-helper` | `ZEROSTATE_HELPER` | `acki-nacki/target/release/zerostate-helper` (`cargo build --release --bin zerostate-helper`) |
| `node-helper` | `NODE_HELPER` | `acki-nacki/target/release/node-helper` (same) |
| `env.sh` | — | sourceable convenience script for manual `cli.py` runs |

The five binaries are **not tracked in git** (see
`bridge/multithreading/.gitignore`); `env.sh` and this README are.

## Populate from a local acki-nacki checkout

```bash
cd /path/to/bridge/multithreading/bins_linux
AN=/path/to/acki-nacki

cp "$AN/contracts/compiler/tvm-cli" .
cp "$AN/contracts/compiler/sold" .
cp "$AN/contracts/compiler/tvm-debugger" .

# If the helpers aren't built yet:
( cd "$AN" && cargo build --release --bin zerostate-helper --bin node-helper )
cp "$AN/target/release/zerostate-helper" .
cp "$AN/target/release/node-helper" .

chmod +x tvm-cli sold tvm-debugger zerostate-helper node-helper
./tvm-cli --version   # smoke test
```

### Architecture / libc note

The pre-shipped binaries under `acki-nacki/contracts/compiler/` are
Linux `x86_64-unknown-linux-gnu` builds against a modern glibc
(≥ 2.31 in practice). If the target machine is aarch64 (e.g. AWS
Graviton, some CI runners) or musl-based (Alpine), the shipped
binaries won't run — rebuild from a matching `tvm-sdk` checkout.

`ldd tvm-cli` fails with "not a dynamic executable" or missing libs =
arch/libc mismatch. `file tvm-cli` prints the ELF class the binary
was built for.

## Use — manual `cli.py` invocation

```bash
cd bridge/multithreading
source bins_linux/env.sh          # exports 6 tool paths + DISABLE_MV
cd "$ACKI_NACKI_DIR"
python3 tests/mt/cli.py test-multithread-cross-thread ...
```

## Use — via the wrapper

`runbooks/run_multipath.sh` auto-detects Linux via `uname -s`, sets
`TOOLS_DIR=$MT_DIR/bins_linux` by default, and exports the same env
vars internally. No sourcing required.

To override (binaries elsewhere on the box):

```bash
TOOLS_DIR=/opt/an-tools ./runbooks/run_multipath.sh
```

## State-v2 note

The `tests/mt/cli.py test-multithread-cross-thread` harness against a
state-v2 acki-nacki node requires state-v2-compatible tools. A
`v3.0.6.an`-era `tvm-cli` against a state-v2 node fails silently at
zerostate generation. If the node was built from a state-v2 branch
(e.g. tvm-sdk `state_v2`, acki-nacki `feature/node-3953-...`),
`tvm-cli` / `sold` / `tvm-debugger` in this dir must come from a
state-v2 tvm-sdk build too — the contract-compiler downloads that
ship in `acki-nacki/contracts/compiler/` may need to be replaced with
locally built ones from a `tvm-sdk` checkout on the matching branch.
