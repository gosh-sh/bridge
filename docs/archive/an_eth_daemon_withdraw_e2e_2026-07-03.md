> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/ETH-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

# AN→ETH daemon-driven withdrawal — live Sepolia E2E (2026-07-03)

This records the first **daemon-driven** AN→ETH payout on Sepolia and the new
`relayer daemon-withdraw` loop that makes both ETH legs long-running services.

> **Update 2026-07-04 — unified into one service.** The two ETH legs
> (`daemon-prover` = verifyBlock, `daemon-withdraw` = withdrawByProof) are now
> folded into a single `relayer daemon-bridge` subcommand and a single
> `bridge-relayer.service` on ursus (the standalone `bridge-withdraw.service`
> was retired). `daemon-bridge` interleaves the two legs sequentially in one
> loop on **one relayer EOA**, so there is never more than one in-flight
> transaction and the shared account's nonces can't race — the failure mode a
> two-service split on the same key invites. Each iteration: one prover tick
> (advance anchor) → one withdraw scan (pay ready events) → shared exponential
> backoff. The standalone `daemon-prover` / `daemon-withdraw` subcommands remain
> for manual / one-off use. Unit template: `scripts/ursus/bridge-relayer.service`.

## What shipped

`crates/bridge-relayer-daemon` gains a `daemon-withdraw` subcommand — the
withdraw-side twin of the existing `daemon-prover`:

- Polls `--proofs-dir` for `proof_event_*.json` Circuit-4 bundles
  (`discover_event_proofs` + `is_event_proof_file` filter out the
  `*.result.json` sidecars).
- Gates each proof on its partner `proof_event_NNN.result.json` ACK
  (`WithdrawalResultGate::is_accepted`); `--skip-verified-gate` bypasses.
- **Idempotent / restart-safe**: before spending gas it queries
  `isNullifierUsed(nullifier)` on-chain and skips anything already paid, so a
  cold restart or a re-scan of the same directory never double-pays and never
  reverts on `NullifierAlreadyUsed`. Within a run, paid proofs are also parked
  in an in-memory set.
- `--dry-run` simulates every pending withdrawal via `eth_call` (no tx).
- Exponential backoff (`--backoff-*`) on transient reverts (e.g. anchor not yet
  registered by the verifyBlock lane), SIGINT/SIGTERM-aware shutdown.

New helpers live in `withdrawal.rs`: `WithdrawalResultGate`,
`is_event_proof_file`, `result_path_for`, `discover_event_proofs` (all unit
tested — 48 crate tests green, fmt + clippy clean).

## `GenesisCursorBridge` test helper

`contracts/ethereum/script/DeployGenesisCursorBridge.s.sol` adds a thin subclass
of `AckiNackiBridge` whose constructor seeds `storedLastSeenBlockSeqNo` to a
**non-zero** genesis value. The base constructor always leaves it `0`, which
forces a fresh bridge to replay a chain segment from its *true* genesis (the
block whose Circuit-1A `last_seen` public input is 0). For a mid-chain replay we
only hold proofs for a later window, so we seed the cursor to the predecessor of
the first replayed block. `storedLastSeenBlockSeqNo` is `public` (non-private) on
the base, so the subclass initialises it directly — **no change to the audited
base contract, no blast radius** on the ~19 existing `VerifyBlockConfig`
constructors. Not for production (production genesis's at 0 and verifies the whole
chain).

### Why the cursor genesis is required

`verifyBlock` feeds `uint256(storedLastSeenBlockSeqNo)` as Circuit 1A's 4th
public input (`AckiNackiBridge.sol:702`). Circuit 1A cryptographically binds
`last_seen`, so a fresh bridge (genesis `last_seen = 0`) rejects any proof whose
baked `last_seen ≠ 0` with `AttestationProofRejected` (`0x87bf1c06`). The proof
window we replayed starts at block `1083392` (baked `last_seen = 1082880`), i.e.
above the true segment genesis. Genesis'ing the cursor to `1083904` lets a
**single** `verifyBlock(1084416)` land, because that block bakes
`last_seen = 1083904`.

> Historical note (superseded 2026-07-03, second run below): the first run's
> verifyBlock leg used the one-shot `submit-verify-block --block-seq-no 1084416`
> because `daemon-prover` computed `target = last_seen + 1` with an *exact*
> `proof_{target}.json` lookup and could not jump the 512-spaced proof stream.
> This has since been fixed — see "Fall-forward + both legs daemonized" below.

## Fall-forward + both legs daemonized (2026-07-03, second run)

`ProverProofsBlockSource::fetch` now **falls forward** to the smallest
`proof_<N>.json` with `N >= target` (`next_available_seq_no`), and the relayer
tick accepts a block whose seqno is `>= target` (submitting/recording the actual
seqno). AN emits proofs only for 512-spaced key blocks, and each key-block proof
bakes the *previous* key block as its Circuit-1A `last_seen` — which is exactly
the bridge's current `storedLastSeenBlockSeqNo` — so consecutive proofs chain
cleanly regardless of the numeric gap. `daemon-prover` can now advance a fresh
bridge across the whole stream on its own (unit test:
`prover_proofs_source_falls_forward_to_next_key_block`; 49 crate tests green).

Both ETH legs now run as **systemd services on ursus** alongside the deposit
relayer, so all three bridge directions are long-running:

| Service | Command | Role |
|---------|---------|------|
| `deposit-relayer.service` | `deposit-relayer daemon` | ETH→AN: listen → prove → `finalizeDeposit` |
| `bridge-relayer.service` | `relayer daemon-prover` | AN→ETH: fall-forward `verifyBlock` (anchor registration) |
| `bridge-withdraw.service` | `relayer daemon-withdraw` | AN→ETH: watch `proof_event_*` → `withdrawByProof` (payout) |

Unit templates: `scripts/ursus/{bridge-relayer,bridge-withdraw,deposit-relayer}.service`.
The `bridge-relayer.service` template drops the BK-rotation sentry
(`--an-node-url`) because shellnet exposes only GraphQL, not the
`/v2/bk_set{,_update}` REST the sentry polls; re-add it once a REST endpoint is
available.

### Second live run (all daemon-driven, Sepolia)

| Step | Value |
|------|-------|
| Fresh bridge | `GenesisCursorBridge` `0xd596fcFAe47AA0b5F2ed008F9FCCa0376982DC18` (genesis `lastSeen = 1083904`) |
| Treasury funded | 1 USDC (`deposit(1e6, 0, 0x…01)`, tx `0x80314cc9…`) |
| **`daemon-prover` verifyBlock** | fell forward `target 1083905 → proof 1084416`, tx `0x7653fbfc16ff4800a697842110fb1ef3c234c26ff3cda5262b937f111259dc90` (registers the event anchor); then auto-advanced `1084416 → 1084928 → …` |
| **`daemon-withdraw` payout** | tx `0xae9233cefb064de082ac1f3896e5fd5b618fe60936d65e8d3a511a8e6686fcac` (block 11196777, gas 82438), `paid=1 skipped=0` |
| USDC transfer | bridge → recipient `0x742d…f44e`, **1,000,000 (1 USDC)**; bridge treasury drained to `0` |
| Idempotency | subsequent scans log `paid=1 skipped=0` and the nullifier stays consumed |

### Full-loop rerun (both directions, 2026-07-03)

A clean rerun of both directions, entirely daemon-driven:

| Direction | Evidence |
|-----------|----------|
| **Deposit** id=8 | Sepolia `deposit` tx `0x2b3781efccade0afdff0a7b531227c3ee0df6c3397d9ba2cf5ea8fec9e8f88b8` (block 11196834) → `deposit-relayer.service` proved + `finalized on AN deposit_id=8 amount=1000000` (fresh incl. proving) |
| **Withdraw** on fresh bridge `0xe7E6D0Ea06ca5435b547288dcd74ABc0Ea8Fd2fE` | `daemon-prover` fall-forward verifyBlock tx `0x502b7648eaed23b365beb00e28b756690acc0d08825b33dd8e5364d2fd1c2ca8` → `daemon-withdraw` payout tx `0xb6e351c8086674ccf38f0b79946974f8aa3824d7a8d6b18b8e909c7e0cb18dcb` (block 11196846, gas 82438), treasury drained to `0` |

The withdrawal replays the same partner-generated `proof_event_000000.json` on a
fresh bridge (unused nullifier); the deposit is a genuinely new event proven
fresh. Both ETH-side legs ran with zero manual submission.

## True round-trip (address-bound withdrawal) — BLOCKED on shellnet USDC mint (2026-07-03)

Goal: originate a *fresh* AN withdrawal bound to our own ETH address so the payout
returns to the depositor (`0x6Ab0EeDEc35c5FAb406642eDc20bAB9f1B1dCCEa`), instead of
replaying the canned proof (recipient `0x742d35Cc…f44e`).

**Our side is ready:**
- The partner orchestrator's ETH recipient is now env-overridable (patched on ursus):
  `RECIPIENT_HEX = os.environ.get("RECIPIENT_HEX", "742d35cc…f44e").lower().removeprefix("0x")`
  in `acki-nacki-to-eth-bridge-halo2-prover/python/generate_withdrawals_with_live_event_proving_shellnet.py`
  (default unchanged; `.orig` kept beside it). Set `RECIPIENT_HEX=6ab0…dccea` to bind our EOA.
- `bridge-prover-daemon` + `bridge-verifier-daemon` bootstrap + prove cleanly against
  current shellnet (seed 699904, key block 700416, bk_set_size=5).
- Relayer `daemon-prover` (fall-forward) + `daemon-withdraw` are proven to register the
  anchor and pay out automatically.

**Blocker (partner-side):** shellnet was reset (head ~699k, *below* our 1.08M-era proofs).
The orchestrator's `USDCBridge.mintAndSend` (owner-signed with the bundled
`python/contracts/USDCBridge.shellnet.keys.json`) no longer credits `ecc[3]` USDC — the
freshly deployed multisig receives only `ecc[2]` (gas). The `USDCBridge` (`1a1a…::1a1a…`)
is Active (code_hash `af0a4825…`), so the most likely cause is a **stale bridge-owner key**
after the reset. Without `ecc[3]` there is no `initiateWithdrawal` → no fresh proof.

**Resume runbook (once partner confirms the shellnet mint works):**

1. On ursus, in `acki-nacki-to-eth-bridge-halo2-prover/`, fresh-launch the daemons vs
   shellnet (bypass the broken `cargo build` git-auth step — the binaries are prebuilt):
   `rm -f proofs/*.json state/*.json logs/*.log && BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql RUST_LOG=info ./target/release/bridge-verifier-daemon &` then `… bridge-prover-daemon &`.
2. Run the orchestrator bound to our EOA:
   `BRIDGE_GQL_ENDPOINT=… RECIPIENT_HEX=6ab0eedec35c5fab406642edc20bab9f1b1dccea python3 python/generate_withdrawals_with_live_event_proving_shellnet.py`
   → writes `proofs/proof_event_000000.json` bound to our address + new-chain bundle proofs.
3. Read the seed bundle proof `proof_<seedKB>.json`: byte-reverse `bk_set_poseidon_hash_hex`
   → `GENESIS_BK_SET_COMMITMENT`, `prev_max_level_layer_hash_hex` → `GENESIS_PREV_MAX_LEVEL_LAYER_HASH`,
   and set `GENESIS_LAST_SEEN_BLOCK_SEQ_NO` = the proof's baked `last_seen_block_seqno`.
4. Deploy a fresh `GenesisCursorBridge` with those genesis values (reuse the same 4 verifiers),
   fund 1 USDC treasury, repoint `config/bridge-relayer.env` `BRIDGE_ADDRESS`, reset
   `state/bridge-relayer-state.json`, restart `bridge-relayer.service` + `bridge-withdraw.service`.
5. `daemon-prover` fall-forwards → `verifyBlock` registers the event's anchor; `daemon-withdraw`
   → `withdrawByProof` pays **our** EOA. Verify the USDC `Transfer` `to = 0x6Ab0…CCEa`.

Partner ask: confirm the current shellnet `USDCBridge` owner keypair / mint procedure so
`mintAndSend` credits `ecc[3]` again.

## The run (all on Sepolia, relayer EOA `0x6Ab0…CCEa`)

| Step | Value |
|------|-------|
| Fresh bridge | `GenesisCursorBridge` `0x9256cC867022bFe2F10E266Ad36525220cc030dE` |
| Genesis `bkSetCommitment` | `0x2dfb560b…f2d27e2c` (byte-reversed `proof_1084416.bk_set_poseidon`) |
| Genesis `prevMaxLevelLayerHash` | `0x08900cef…d52b00b4` (byte-reversed `proof_1084416.prev_max_level`) |
| Genesis `lastSeen` | `1083904` |
| Reused verifiers | Primary `0x35EDd105…`, Fallback `0x10F9EE39…`, LayerHashes `0xC5aFb756…`, Withdrawal `0x2bd5C390…` |
| Withdraw identity | `dappFr = 0`, `accFr = 0x1a1a…1a`, `altDstChainId = 1`, `altDstHostChainId = 11155111`, `altTokenId = 3` |
| Treasury funded | 1 USDC (`deposit(1e6, 0, 0x…01)`) |
| `verifyBlock(1084416)` | tx `0xb487b8f62392d4d2852f43fa9c71d4b69435db957c9c97e88f1507e404913a40` → registers anchor `2da97bd0…512618` |
| **`daemon-withdraw` payout** | tx `0xba937a9c9c53cf800d63f15b0ed6410bc7832941937f4f82b86b9fb79601b371` (block 11195094, gas 82438) |
| USDC transfer | bridge → recipient `0x742d35Cc6634C0532925a3b844Bc454e4438f44e`, **1,000,000 (1 USDC)** |
| Nullifier | `29420b1c…39b49871` consumed; treasury drained to `0` |
| Restart | daemon logs `nullifier already used on-chain; skipping` (skipped=1, paid=0) — idempotent |

## Reproduce

```bash
# 1. Deploy fresh bridge (reused verifiers + genesis cursor)
cd contracts/ethereum
PRIVATE_KEY=… \
PRIMARY_VERIFIER=0x35edd1059141f76ccecbee74a6b5fd277871168d \
FALLBACK_VERIFIER=0x10f9ee39effc1ba0f5262cb90aab7ef637d53a66 \
LAYER_HASHES_VERIFIER=0xc5afb7567e2d460db74e3c9f3fe1da21cf89804b \
WITHDRAWAL_VERIFIER=0x2bd5c390110b07c173388b628181a571ed8405c3 \
GENESIS_BK_SET_COMMITMENT=0x2dfb560bb0e92d1401c2a45c3a64c47ad742a0f404b3c75684d84b53f2d27e2c \
GENESIS_PREV_MAX_LEVEL_LAYER_HASH=0x08900cefb54252a98824c3390d9c7430a03fcb48a856ded7b755355fd52b00b4 \
GENESIS_LAST_SEEN_BLOCK_SEQ_NO=1083904 \
WITHDRAW_DAPP_FR=0 \
WITHDRAW_ACC_FR=0x1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a \
forge script script/DeployGenesisCursorBridge.s.sol:DeployGenesisCursorBridge --rpc-url $RPC --broadcast

# 2. Fund treasury (approve + deposit 1 USDC), then register the anchor
relayer submit-verify-block --proofs-dir <prover proofs/> --block-seq-no 1084416 \
  --rpc-url $RPC --bridge-address <fresh bridge> --private-key … --skip-verified-gate

# 3. Auto-pay: watch proof_event_*.json → withdrawByProof (idempotent)
relayer daemon-withdraw --proofs-dir <prover proofs/> \
  --rpc-url $RPC --bridge-address <fresh bridge> --private-key … --poll-secs 20
#   (add --dry-run first to eth_call-simulate without spending gas)
```
