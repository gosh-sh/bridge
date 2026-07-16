# BUILD — AN bridge contract audit (pytest + tvm-debugger)

## Prerequisites

| Tool | Source | Local path |
|------|--------|------------|
| `sold` | `../TVM-Solidity-Compiler` | `.tools/sold` (symlink) |
| `tvm-debugger` | `../tvm-sdk` | `.tools/tvm-debugger` |
| `tvm-cli` | `../tvm-sdk` | `.tools/tvm-cli` (e2e only) |
| `python3` + `pytest` | system | `pip install pytest` |

One-time setup:

```bash
./scripts/setup_an_audit_tools.sh
./scripts/sync_an_contracts.sh   # acki-nacki origin/dev
```

Build TVM binaries if missing:

```bash
cd ../TVM-Solidity-Compiler && cargo build --release
cd ../tvm-sdk && cargo build --release
```

## Compile contracts

```bash
cd audit/spec/an-contracts && ./build.sh
```

Uses `sold --tvm-version gosh --base-path .` (see dex `audit/Makefile.inc`).

## Run tests

```bash
# toolchain smoke (no contracts required)
cd audit/spec/an && python3 -m pytest unit/test_toolchain_smoke.py -q

# full spec (after sync + build)
make audit-an-test
```

## tvm-debugger limits (read before writing tests)

From dex audit experience — **not full node emulation**:

- Runs **synchronous entry-tx** only; deferred self-call queues are incomplete
- `run-raw --input-file <tvc>` **mutates TVC in-place**
- Bounce body may be incomplete (mark xfail if testing bounce semantics)
- ECC balances may not persist across separate debugger invocations like on-chain
- Use `MessagePipeline` for multi-hop internal routing (see `test_base.py`)

Full AN background: `audit/knowledge/01-blockchain-overview.md` … `05-security-patterns.md`.

## Layout

```
.tools/                    # gitignored symlinks
audit/spec/an/             # pytest overlay (our tests)
audit/spec/an-contracts/   # working copy of acki-nacki Solidity + build/
audit/knowledge/           # symlinks → ../dex/knowledge/01-05
```

## ZK opcode scope

`ZKHALO2VERIFYWITHVK` — **smoke / trust-boundary only** in this audit pass; deep opcode audit is out of scope (covered in tvm-sdk + deposit E2E docs).
