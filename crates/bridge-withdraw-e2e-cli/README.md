# bridge-withdraw-e2e-cli

End-user CLI for withdrawing USDC from an Acki Nacki multisig to an EVM
recipient via the bridge. This is the operator-facing counterpart to the
relayer daemon: the daemon owns the continuous bundle-proving stream
(`verifyBlock`); this CLI owns per-withdrawal composition
(multisig burn → capture → Circuit-4 SHPLONK proof → `withdrawByProof`).

## What it does

Given the four user inputs (`--from`, `--from-keys`, `--to`, `--to-chain`,
`--amount`), the tool runs the six-stage pipeline:

1. **Preflight** — read-only checks: key file 0600, `--from` is an active
   single-custodian multisig whose owner matches `--from-keys`, USDCBridge
   resolves, multisig ECC[3] ≥ amount.
2. **Idempotency reserve** — SHA-256 dedup key on
   `(from, to, to_chain, amount)`; refuse a duplicate in-flight unless
   `--allow-retry` is passed.
3. **Burn** — compose the multisig `sendTransaction` payload calling
   `USDCBridge.initiateWithdrawal(dstChainId, recipient)` (via `tvm-cli`),
   broadcast it, record the AN tx hash. Bounce defaults to `true` so USDC
   returns to the multisig on any bridge revert.
4. **Capture** — wait for the corresponding `WithdrawalInitiated` ExtOut
   event using [`bridge_relayer_daemon::withdraw_e2e::capture_next_withdrawal_event`].
5. **Resurrect + wait for coverage** — read the deployed `AckiNackiBridge`
   contract at `--bridge-address` via `EthBridgeClient::read_full_state`
   and poll until `storedLastSeenBlockSeqNo` has advanced past the
   covering L1 (`W·P = 1024`) or L2 (`W² = 16 384`) bundle boundary for
   the burn's block seq_no. Once the covering bundle has landed
   on-chain (fed by a relayer running on some other host),
   `BridgeState::from_contract` builds a byte-for-byte mirror of the
   contract state — no local `prover_state.json` needed.
6. **Prove** — export the private witness, enrich against the resurrected
   `BridgeState` (single-shot, no retry), produce a Circuit-4 SHPLONK
   proof via the in-process aggregator + Circuit-4 prover (reuses
   [`bridge_relayer_daemon::withdraw_e2e::run_once_with_state`]).
7. **Submit** — always call `dry_run_withdraw` first; unless `--dry-run`
   is set, submit `withdrawByProof` and wait for the receipt.

Every stage transition is persisted to a per-withdrawal state file so a
mid-flight crash leaves a resumable trace (v2: `--resume`).

## Prerequisites

- Some bridge relayer daemon (`daemon-live`) is running against the same
  `AckiNackiBridge` deploy — anywhere, not necessarily on this host. Its
  `verifyBlock` submissions are what advance the on-chain state the CLI
  polls in stage 5. No shared filesystem or daemon-produced JSON is
  needed: everything the CLI needs lives in contract storage.
- `tvm-cli` is on `PATH` (used to encode the initiateWithdrawal body and
  fire the multisig `sendTransaction`).
- USDCBridge is deployed and unpaused; treasury is seeded on the EVM
  side (separate scripts under `scripts/`).
- The `--from` multisig is deployed, single-custodian, and holds ≥
  amount USDC in ECC[3].

## Environment / flags

Every environment variable has a corresponding `--flag` override. The
env-var form is intended for daemonized/scripted use; the flag form for
one-off runs.

| Flag                    | Env var                     | Purpose |
|-------------------------|-----------------------------|---------|
| `--gql-endpoint`        | `BRIDGE_GQL_ENDPOINT`       | AN GraphQL for account queries + event capture |
| `--usdc-bridge-account` | `USDC_BRIDGE_ACCOUNT_ID`    | On-chain USDCBridge acc id; default is shellnet canonical |
| `--anchor-layer`        | —                           | `auto` (default), `1`, or `2` — must match the deploy's anchoring mode |
| `--i-know-the-wait`     | —                           | Acknowledge L2's ~91 min chain-time budget when `--anchor-layer 2` |
| `--rpc-url`             | `RPC_URL`                   | EVM JSON-RPC — used both for polling coverage and submitting `withdrawByProof` |
| `--bridge-address`      | `BRIDGE_ADDRESS`            | Deployed `AckiNackiBridge` — the sole source of prover state |
| `--eth-private-key`     | `RELAYER_PRIVATE_KEY`       | Signer for `withdrawByProof` (distinct from `--from-keys`) |
| `--aggregator-dir`      | `BRIDGE_AGGREGATOR_DIR`     | Circuit-4 aggregator artifacts |
| `--verifiers-dir`       | `BRIDGE_VERIFIERS_DIR`      | Precomputed inner verifier keys |
| `--params-dir`          | `BRIDGE_PARAMS_DIR`         | KZG SRS params |
| `--pk-cache-dir`        | `BRIDGE_PK_CACHE_DIR`       | Warm-start pk cache (optional) |
| `--state-dir`           | `BRIDGE_WITHDRAW_STATE_DIR` | Per-withdrawal idempotency state dir |

## Usage

```
bridge-withdraw-e2e-cli withdraw \
  --from    <dapp_id>::<account_id> \
  --from-keys /path/to/owner.keys.json \
  --to      0xRecipient \
  --to-chain 11155111 \
  --amount   1.000000
```

Add `--json` for machine-readable output on stdout (human logs still go
to stderr).

Add `--dry-run` for a full end-to-end preview: everything up to and
including `dry_run_withdraw` runs, but neither the AN burn nor the EVM
submit is broadcast.

Add `--yes` to skip the confirmation prompt (or `--non-interactive` to
refuse if a prompt would be needed).

## Exit codes

Distinguishing "nothing broadcast" from "broadcast, unknown outcome" is
the whole point of the exit-code discipline — scripts that pattern-match
on a single non-zero would blind an operator to the difference that
matters for money.

| Code | Meaning |
|------|---------|
| 0    | Success (or `--dry-run` returned OK) |
| 2    | Preflight refused; nothing broadcast |
| 3    | Duplicate in-flight refused; nothing broadcast |
| 10   | AN burn broadcast, final outcome unknown — reconcile via GQL |
| 11   | Burn confirmed, `WithdrawalInitiated` capture timed out |
| 12   | Capture succeeded, Circuit-4 proof failed |
| 13   | Proof succeeded, `withdrawByProof` reverted / dry-run reverted |

## Safety

- `--from-keys` file contents are never logged, printed, or persisted
  anywhere the CLI writes. The owner public key derived from the file
  IS written (to state files, printed on preflight) — it's an
  on-chain-observable identifier, not a secret.
- ETH signer key (`--eth-private-key`) is the same discipline.
- Idempotency state files under `--state-dir` contain only chain-observable
  identifiers (AN tx hash, WithdrawalInitiated msg id, block seq no, ETH
  tx hash).

## Layout

Physical crate at `crates/bridge-withdraw-e2e-cli/`. Symlinked into the
`an-bridge-prover` sub-workspace at
`crates/an-bridge-prover/bridge-withdraw-e2e-cli/` so it can depend on
the halo2-heavy prover crates while remaining excluded from the root
workspace. Build via:

```
cd crates/an-bridge-prover
cargo build -p bridge-withdraw-e2e-cli
```

Vendored ABIs live under `abi/`:
- `USDCBridge.abi.json` — for encoding the `initiateWithdrawal` body cell
- `UpdateCustodianMultisigWallet.abi.json` — for the outer `sendTransaction`

## Not this CLI's job

- Multisig deploy → separate script (Python-based; mirrors the vendored
  driver).
- USDC treasury seeding on the EVM side → separate script (`cast` / faucet).
- Continuous bundle proving → some daemon owns that, running anywhere;
  the CLI only waits for its output to land on-chain (stage 5).
- Full `--resume` semantics → v2 (v1 has refuse-duplicate + blunt
  `--allow-retry` override).
