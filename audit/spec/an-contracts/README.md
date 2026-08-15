# AN contract workspace (audit)

Working copy of Acki Nacki bridge Solidity for `sold` + `tvm-debugger` tests.

## Sync sources

AN contracts: sibling `acki-nacki`, branch **`contracts/bridge`** on `origin` (gosh-sh).
Upstream exchange: **`eccUSDCBridge.sol`** (v1.3.x, 12 PI, `_trustedL1Bridge` SET) + `DepositVoucher.sol`.

```bash
./scripts/sync_an_contracts.sh   # preserves audit VK_BLOB 724687a4… by default
```

After sync, edit `contracts_manifest.json` if paths differ (sync script auto-picks `eccUSDCBridge` vs `USDCBridge`).

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
