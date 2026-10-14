# Shellnet E2E — beacon light client (PR #36)

Operator procedure on n14 + shellnet. This is the M6 E2E for this PR, not a
follow-up ticket.

tvm-sdk#284 co-deploys with this contract (every node on the opcode decider).

## Preconditions

- Docker / live AN cluster GraphQL on n14 (`http://localhost/graphql`) **or**
  shellnet GraphQL.
- Hermez SRS at `STEP_SRS_PATH` (opcode is Hermez-keyed).
- `eth-light-client-prover` + `crates/eth-light-client-relayer` synced to this
  branch.
- `EthBeaconLightClient` deployed from the bridge (`deployLightClient`). Set
  `AN_USDC_BRIDGE` and `AN_USDC_ABI_PATH` only if you mean to flip (step 9).
- Binary: `cargo build --release --features live-submit` in the relayer crate.

## Steps

1. `eth-lc-relayer beacon-watch --beacon-url $BEACON_URL` — slots parse.
2. `eth-lc-relayer set-gloas-fork-slot --slot 11296768` (Sepolia; mainnet
   `18446744073709551615` while Gloas is unscheduled). Required once after
   `deployLightClient`; `submitUpdate` and `submitRotate` revert 256 until
   it runs. The daemon does not set it.
3. `eth-lc-relayer prove-one --beacon-url $BEACON_URL --prover-dir … --srs-path … --out-dir ./bundle`
   on **n14** (~k=19, tens of GB). `COMMITTEE_JSON_PATH` is filled by the
   relayer from `light_client/updates`. Synthetic OsRng committee is a bug.
4. `eth-lc-relayer submit-one --bundle-dir ./bundle …` — `submitUpdate` ACCEPTED,
   `getHead` matches the proven execution hash.
5. Independent Ethereum node: that hash is canonical and ≥ 64 confirmations.
6. Sepolia (or local) `deposit` whose receipt is **in that checkpoint block**
   → `deposit-relayer prove-one` → `finalizeDeposit` ACCEPTED.
7. For a deposit in a non-checkpoint block of the same epoch:
   `eth-lc-relayer submit-ancestry --eth-rpc-url $ETH_RPC_URL
   --checkpoint-hash 0x<newer> --older-anchor-hash 0x<older>` then
   `finalizeDeposit`. `ancestry-one` only reports whether the block sits on the
   parent chain of a checkpoint; coverage also needs both ends of the span
   proven and the span sent (`docs/eth-light-client.md` §3.4).
8. Period boundary: prove `rotate_tree_n8` `EMIT_VKBLOB=1` on n14 against
   the live hand-off, then `eth-lc-relayer submit-rotate --state
   ./eth-lc-relayer-state.json`. The daemon does not rotate a live
   period — `rotate_tree_n8` is a committed fixture, and a mismatch
   returns `RotateRequired` without proving. The next tick reads
   `getCommitteeState` and continues; `--state` is for a restart.
9. Owner flip, only on purpose: `eth-lc-relayer daemon --flip-owner` issues it
   after step 4, or run `eth-lc-relayer flip-owner`. After it, deposits outside
   checkpoint blocks and from L2s have no anchor writer; read
   `scripts/ursus/flip_deposit_to_light_client.md` first. Confirm
   `getLightClient() != 0` and owner flags are off.

Negative: a privately mined `Deposit` whose `blockHash` is not on the parent
chain of a proven checkpoint must still revert `finalizeDeposit`.
