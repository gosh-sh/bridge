# Tool binaries — populate before running `run_multipath.sh`

`runbooks/run_multipath.sh` expects five acki-nacki binaries here so the
env vars Michael's `tests/mt/cli.py` reads are auto-bound. Nothing in
this directory is checked in — populate it once per machine.

## What goes here

| Binary            | Env var it becomes           | Notes |
|-------------------|------------------------------|-------|
| `tvm-cli`         | `CLI_NAME`, `TVM_CLI`        | v2-state-compatible build (matches the node commit under test). |
| `sold`            | `SOLD`                       | gosh Solidity compiler. |
| `tvm-debugger`    | `TVM_DEBUGGER`               | Same release as `tvm-cli`. |
| `zerostate-helper`| `ZEROSTATE_HELPER`           | From acki-nacki `cargo build --release`. |
| `node-helper`     | `NODE_HELPER`                | From acki-nacki `cargo build --release`. |

`DISABLE_MV=true` and `MESSAGE_ARCHIVE_OTEL_RUN_ID` are also exported
by the wrapper — no extra file needed.

## Where to get them

**Linux (n14):** grab the latest Linux release binaries from
- `tvmlabs/tvm-sdk` releases → `tvm-cli`, `sold`, `tvm-debugger` for
  x86_64-unknown-linux-gnu.
- `gosh-sh/acki-nacki` releases → `zerostate-helper`, `node-helper`
  for the same target. If a release doesn't publish them, build from
  a sibling `acki-nacki` checkout on the target machine:

  ```bash
  cd $ACKI_NACKI_DIR
  cargo build --release --bin zerostate-helper --bin node-helper
  cp target/release/{zerostate-helper,node-helper} /path/to/bridge/multithreading/tools/
  ```

**macOS (local dev):** if you already have a v2-tools shelf at
`/Volumes/x5/v2_tools/` etc., symlink instead of copying:

```bash
cd /Users/alinat/HALO2_TVM_EXPERIMENTS/bridge/multithreading/tools
ln -sf /Volumes/x5/v2_tools/tvm-cli               tvm-cli
ln -sf /Volumes/x5/v2_tools/sold                  sold
ln -sf /Volumes/x5/v2_tools/tvm-debugger          tvm-debugger
ln -sf /Volumes/x5/cargo-target/release/zerostate-helper  zerostate-helper
ln -sf /Volumes/x5/cargo-target/release/node-helper       node-helper
```

## After populating

```bash
cd bridge/multithreading/tools
chmod +x tvm-cli sold tvm-debugger zerostate-helper node-helper
./tvm-cli --version    # smoke test
```

## Overriding the location

If binaries live elsewhere on your box, set `TOOLS_DIR` before
invoking the wrapper:

```bash
TOOLS_DIR=/opt/acki-nacki/bin ./runbooks/run_multipath.sh
```

The wrapper validates every binary is present and executable and
prints exactly which one is missing before touching Docker.
