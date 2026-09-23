> **⚠️ ARCHIVED 2026-09-23 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/EVM-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

# Ursus shellnet E2E operator host (as of 2026-06-11)

Moved out of `AGENTS.md` on 2026-09-23.

Shellnet E2E operator box. SSH: `ssh ubuntu@ursus-tools.dev` (passwordless from dev machine). Root layout: `/home/ubuntu/bridge-e2e/`. Live manifest: `DEPLOYED_VERSION.json` on the host.

| Path | Role |
|------|------|
| `acki-nacki-bridge/` | rsync'd from local (no git on host); **tip `15ea747`** (2026-06-11) |
| `acki-nacki-to-eth-bridge-halo2-prover/` | partner AN→ETH prover + `proofs/` |
| `bin/deposit-relayer` | EVM→AN daemon (rebuilt 2026-06-11) |
| `bin/relayer` | AN→ETH `verifyBlock` / `withdrawByProof` CLI |
| `config/deposit-relayer.env` | Sepolia + shellnet GraphQL + `AN_SENDER` msig |
| `config/bridge-relayer.env` | Sepolia E2E bridge `0x58a1…043d` (AN→ETH) |
| `tvm-sdk/` | `v3.0.0.an` tag build for `tvm-cli` (prover python tests) |

**systemd**

| Unit | State | Purpose |
|------|-------|---------|
| `deposit-relayer.service` | **active** | EVM→AN loop → `finalizeDeposit` on shellnet |
| `bridge-relayer.service` | **active** | unified AN→ETH `relayer daemon-bridge` — verifyBlock + withdrawByProof in one process/one EOA (replaced the old `bridge-withdraw.service` split 2026-07-04) |

**Redeploy recipe** (from dev machine — build on ursus for GLIBC safety):

```bash
# sync source
rsync -avz --delete --exclude target --exclude .git \
  acki-nacki-bridge/ ubuntu@ursus-tools.dev:/home/ubuntu/bridge-e2e/acki-nacki-bridge/

# rebuild + install
ssh ubuntu@ursus-tools.dev '
  source ~/.cargo/env
  cd /home/ubuntu/bridge-e2e/acki-nacki-bridge/crates/deposit-relayer-daemon
  cargo build --release --locked && install -m 755 target/release/deposit-relayer /home/ubuntu/bridge-e2e/bin/
  cd ../bridge-prover-libraries
  cargo build --release --locked -p bridge-relayer-daemon --bin relayer && install -m 755 target/release/relayer /home/ubuntu/bridge-e2e/bin/
  sudo systemctl restart deposit-relayer.service
'
```

Unit + env templates: `scripts/ursus/deposit-relayer.{service,env.example}`, `scripts/ursus/bridge-relayer.{service,env.example}`. Set `BRIDGE_DEPLOY_BLOCK` in deposit-relayer env (not genesis). AN→ETH wiring: `docs/shellnet_an_eth_relayer_wiring.md`.

**GitHub (bridge-EVM, the predecessor repository — private):** active docs/integration PR PR #5 in `gosh-sh/bridge-EVM` (private) (`pruvendo/shellnet-e2e-landing` → `main`).

**Key config (deposit, non-secret)**

- Sepolia bridge (deposits): `0x99c37fb75326ae6953ebbbdcd261ec331df4ce82`
- Sepolia E2E bridge (withdraw): `0x58a1c8d22a79a91db6e7448a7d64d59ad4dc043d`
- Shellnet GraphQL: `https://shellnet.ackinacki.org/graphql`
- `AN_SENDER`: `20c2db9c…::20c2db9c…` (relayer msig, deployed 2026-06)
- RPC: Alchemy Sepolia (switched from publicnode 2026-06-11 — was hitting HTTP 429)
