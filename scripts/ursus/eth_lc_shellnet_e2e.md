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
- `EthBeaconLightClient` deployed. Set `AN_USDC_BRIDGE` (do **not** pass
  `--no-flip-owner` until you want the owner path left on).
- Binary: `cargo build --release --features live-submit` in the relayer crate.

## Steps

1. `eth-lc-relayer beacon-watch --beacon-url $BEACON_URL` — slots parse.
2. `eth-lc-relayer prove-one --beacon-url $BEACON_URL --prover-dir … --srs-path … --out-dir ./bundle`
   on **n14** (~k=19, tens of GB). `COMMITTEE_JSON_PATH` is filled by the
   relayer from `light_client/updates`. Synthetic OsRng committee is a bug.
3. `eth-lc-relayer submit-one --bundle-dir ./bundle …` — `submitUpdate` ACCEPTED,
   `getHead` matches the proven execution hash.
4. Independent Ethereum node: that hash is canonical and ≥ 64 confirmations.
5. Sepolia (or local) `deposit` whose receipt is **in that checkpoint block**
   → `deposit-relayer prove-one` → `finalizeDeposit` ACCEPTED.
6. For a deposit in a non-checkpoint block of the same epoch:
   `eth-lc-relayer submit-ancestry --eth-rpc-url $ETH_RPC_URL --checkpoint-hash 0x…`
   then `finalizeDeposit`. `ancestry-one` is the read-only check.
7. Period boundary: `eth-lc-relayer daemon` (rotate **on** by default) **or**
   `submit-rotate` from `rotate_tree_n8` `EMIT_VKBLOB=1` (~40 GB n14).
8. Owner flip is issued by the daemon after step 3 (or `eth-lc-relayer flip-owner`).
   Confirm `getLightClient() != 0` and owner flags are off. See
   `scripts/ursus/flip_deposit_to_light_client.md`.

Negative: a privately mined `Deposit` whose `blockHash` is not on the parent
chain of a proven checkpoint must still revert `finalizeDeposit`.
