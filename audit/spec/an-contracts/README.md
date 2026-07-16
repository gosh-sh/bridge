# AN contract workspace (audit)

Working copy of Acki Nacki bridge Solidity for `sold` + `tvm-debugger` tests.

## Sync sources

AN contracts: sibling `acki-nacki`, branch **`dev`** on `origin` (gosh-sh):

```bash
./scripts/sync_an_contracts.sh
```

After sync, edit `contracts_manifest.json` if paths differ.

## Build

```bash
cd audit/spec/an-contracts
./build.sh
# or: SOLD=../../.tools/sold ./build.sh
```

Outputs: `build/{Contract}.tvc` + `.abi.json`.

## Tools

`tools/` → `../../../.tools/` (symlink to repo-root `.tools/{sold,tvm-debugger,tvm-cli}`).

Do not commit `.tvc` / `build/` artefacts.
