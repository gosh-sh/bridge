# Audit vendor checkouts (local only)

Gitignored shallow clones for cross-repo audit context. **Not** git submodules — nothing here is committed.

Setup:

```bash
./scripts/setup_audit_vendors.sh
```

| Path | Role | ZK focus |
|------|------|----------|
| `acki-nacki/` | AN contracts source (`origin/dev`) | contracts only |
| `acki-nacki-to-eth-bridge-halo2-prover/` | AN→ETH prover daemon (optional) | pull when needed |
| `acki-nacki-to-eth-bridge-halo2-circuits/` | partner circuit sources (optional) | out of audit scope |
| `tvm-sdk/` | opcode reference (optional) | smoke only |
| `TVM-Solidity-Compiler/` | `sold` source (optional) | — |

If `../acki-nacki` already exists, the script symlinks it instead of re-cloning.

Contract sync for pytest still uses `scripts/sync_an_contracts.sh` → `audit/spec/an-contracts/`.

Full non-E2E plan: `audit/reports/non-e2e-verification-cycle.md`.
