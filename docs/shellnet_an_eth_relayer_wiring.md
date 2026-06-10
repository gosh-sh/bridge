# Shellnet AN→ETH relayer wiring — Sepolia `verifyBlock` + `withdrawByProof`

> **Status (2026-06-10).** First live `verifyBlock` landed on Sepolia:
> bridge `0xC0cdf8C0f67da725e36130A85Fc2aCEf269ce9E8`, tx
> `0xa81fa4f5318cfc09c6bf1c3a048c2383d7c834993aae69763d945559708356eb`,
> block `1083392`. Partner proofs still arrive as Halo2 (~8 KB); gnark-wrap
> via `scripts/wrap_partner_proof_groth16.py` before submit. `withdrawByProof`
> remains disabled on this deploy (Circuit 4 verifier not wired).

## 1. Pipeline overview

```
shellnet GraphQL
       │
       ▼
bridge-prover-daemon  ──► proofs/proof_<seqno>.json   (Halo2 1A + 2)
bridge-verifier-daemon ──► proofs/result_<seqno>.json (primary_verified && layer_verified)
       │
       │  gnark-wrappers/circuit-{1a,2} prove   ◄── operator step (not automated yet)
       ▼
bin/relayer verify-prover-proof / submit-verify-block / daemon-prover
       │
       ▼
Sepolia AckiNackiBridge.verifyBlock(...)
       │
       │  (after anchors in _knownAnchors)
       ▼
bin/relayer submit-withdraw  ◄── proof_event_*.json + circuit-4 gnark wrap
       │
       ▼
Sepolia AckiNackiBridge.withdrawByProof(...)
```

| Stage | Proof format | Where verified |
|-------|--------------|----------------|
| Partner daemon | Halo2 SHPLONK (~8 KB) | AN-side `bridge-verifier-daemon` |
| Ethereum `verifyBlock` | Groth16 256 B × 2 | `PrimaryVerifier` + `LayerHashesMovementVerifier` |
| Circuit 4 withdrawal | Groth16 256 B | `BridgeWithdrawalVerifier` (or mock) |

## 2. Prerequisites

### 2.1 Sepolia bridge redeploy (`WIRE_VERIFY_BLOCK=true`)

Current testnet bridge `0x99c37fb75326ae6953ebbbdcd261ec331df4ce82` was deployed with
verifiers **disabled** (`primaryVerifier = 0x0`, `storedLastSeenBlockSeqNo = 0`).
`verifyBlock` reverts with `VerifyBlockDisabled` until redeployed.

Redeploy with genesis anchors from the **first** shellnet proof bundle
(`proof_1083392.json` on ursus):

```bash
# Field elements are 32-byte LE hex in partner JSON → uint256 for forge envUint
export WIRE_VERIFY_BLOCK=true
export GENESIS_BK_SET_COMMITMENT=0x$(python3 -c "
import json; p=json.load(open('proof_1083392.json'))
print(p['bk_set_poseidon_hash_hex'])")
export GENESIS_PREV_MAX_LEVEL_LAYER_HASH=0x$(python3 -c "
import json; p=json.load(open('proof_1083392.json'))
print(p['prev_max_level_layer_hash_hex'])")

cd contracts/ethereum
forge script script/DeployRealBridge.s.sol:DeployRealBridge \
  --rpc-url "$RPC_URL" --broadcast --verify
```

Update `BRIDGE_ADDRESS` in `config/bridge-relayer.env` after redeploy.

For `withdrawByProof`, the deploy must also wire `BridgeWithdrawalVerifier`
(R15 aggregator pending — mock verifier acceptable for smoke tests).

### 2.2 Gnark-wrap partner Halo2 proofs

Partner `proof_<seqno>.json` fields:

- `primary_proof_hex` — Circuit 1A Halo2 export
- `layer_proof_hex` — Circuit 2 Halo2 export

`ProverProofsBlockSource` rejects non-256-byte proofs unless diagnostics flags are set.

Use the helper script (requires Go + cached `proving.key` under each wrapper dir):

```bash
# First block after genesis deploy: on-chain storedLastSeenBlockSeqNo is 0,
# but partner JSON may carry a non-zero last_seen_block_seqno — override PI[3]:
GNARK_LAST_SEEN_PI=0 python3 scripts/wrap_partner_proof_groth16.py proof_1083392.json
```

The script patches `primary_proof_hex` / `layer_proof_hex` in-place (`.bak` backup).
It already maps Circuit 2 PI `[0]` to `block_id_hex` (the single `verifyBlock`
`blockId`), not `layer_block_id_hex`.

Circuit 4 (`proof_event_000000.json`) needs the same treatment via
`gnark-wrappers/circuit-4/` before `submit-withdraw`.

## 3. Ursus deployment

Host: `ubuntu@ursus-tools.dev`, root `/home/ubuntu/bridge-e2e/`.

### 3.1 Build and install binary

```bash
# From dev machine — sync crate, build on ursus (GLIBC-safe)
rsync -avz --exclude target \
  crates/bridge-relayer-daemon/ \
  ubuntu@ursus-tools.dev:/home/ubuntu/bridge-e2e/acki-nacki-bridge/crates/bridge-relayer-daemon/

ssh ubuntu@ursus-tools.dev '
  source ~/.cargo/env
  cd /home/ubuntu/bridge-e2e/acki-nacki-bridge/crates/bridge-relayer-daemon
  cargo build --release --locked
  install -m 755 target/release/relayer /home/ubuntu/bridge-e2e/bin/relayer
'
```

### 3.2 Config + systemd

```bash
scp scripts/ursus/bridge-relayer.env.example \
  ubuntu@ursus-tools.dev:/home/ubuntu/bridge-e2e/config/bridge-relayer.env
# Edit: set RELAYER_PRIVATE_KEY (Sepolia gas EOA)

ssh ubuntu@ursus-tools.dev '
  sudo cp /home/ubuntu/bridge-e2e/acki-nacki-bridge/scripts/ursus/bridge-relayer.service \
    /etc/systemd/system/bridge-relayer.service
  mkdir -p /home/ubuntu/bridge-e2e/state
  sudo systemctl daemon-reload
'
```

Keep the service **stopped** until bridge redeploy + gnark-wrapped proofs are ready:

```bash
sudo systemctl enable bridge-relayer.service   # optional
sudo systemctl start bridge-relayer.service    # after unblock checklist §4
```

## 4. CLI smoke tests (manual)

All commands use env from `config/bridge-relayer.env`:

```bash
set -a; source /home/ubuntu/bridge-e2e/config/bridge-relayer.env; set +a
PROOFS=/home/ubuntu/bridge-e2e/acki-nacki-to-eth-bridge-halo2-prover/proofs
RELAYER=/home/ubuntu/bridge-e2e/bin/relayer
```

| Step | Command | Expected today |
|------|---------|----------------|
| 1. Load proof | `$RELAYER verify-prover-proof --proofs-dir $PROOFS --block-seq-no 1083392 --rpc-url $RPC_URL --bridge-address $BRIDGE_ADDRESS --no-simulate` | **FAIL** `bk_set mismatch` (chain genesis ≠ shellnet) until redeploy |
| 2. After redeploy | same without `--no-simulate` | **FAIL** `proof is 8192 bytes` until gnark wrap |
| 3. After wrap | `$RELAYER submit-verify-block --proofs-dir $PROOFS --block-seq-no 1083392 --rpc-url $RPC_URL --bridge-address $BRIDGE_ADDRESS --private-key $RELAYER_PRIVATE_KEY --dry-run` | eth_call PASS |
| 4. Live submit | drop `--dry-run` | tx mined, `storedLastSeenBlockSeqNo` advances |
| 5. Withdrawal | `$RELAYER submit-withdraw --proof-event $PROOFS/proof_event_000000.json ... --dry-run` | PASS after C4 wrap + anchor in `_knownAnchors` |

One-shot submit (no daemon):

```bash
$RELAYER submit-verify-block \
  --proofs-dir "$PROOFS" --block-seq-no 1083392 \
  --rpc-url "$RPC_URL" --bridge-address "$BRIDGE_ADDRESS" \
  --private-key "$RELAYER_PRIVATE_KEY"
```

Long-running loop (waits for `result_*.json` gate + next seqno):

```bash
$RELAYER daemon-prover \
  --state "$RELAYER_STATE" --proofs-dir "$PROOFS" \
  --rpc-url "$RPC_URL" --bridge-address "$BRIDGE_ADDRESS" \
  --private-key "$RELAYER_PRIVATE_KEY" \
  --an-node-url "$AN_NODE_URL"
```

Flags:

- `--skip-verified-gate` — load `proof_*.json` even when `result_*.json` is missing/failed
- Partner proofs with Halo2 bytes only fail at submit with a clear byte-length error

## 5. Unblock checklist

- [x] Redeploy Sepolia bridge with `WIRE_VERIFY_BLOCK=true` + genesis from `proof_1083392.json`
- [x] Gnark-wrap `proof_1083392.json` (`GNARK_LAST_SEEN_PI=0` for genesis submit)
- [x] `verify-prover-proof` eth_call PASS
- [x] `submit-verify-block` mined first `verifyBlock` (seq `1083392`)
- [ ] Wire `BridgeWithdrawalVerifier` on redeploy for payout path
- [ ] Gnark-wrap `proof_event_000000.json` → `submit-withdraw`
- [ ] Enable `bridge-relayer.service` for steady-state sync (subsequent blocks need
      `GNARK_LAST_SEEN_PI=<on-chain storedLastSeen>` when wrapping)

## 6. Related docs

- EVM→AN deposit (orthogonal): `docs/shellnet_usdcbridge_deposit_vk_redeploy.md`
- Partner shellnet E2E: `acki-nacki-to-eth-bridge-halo2-prover/SHELLNET_e2e_test.md`
- Deploy script: `contracts/ethereum/script/DeployRealBridge.s.sol`
- Relayer crate: `crates/bridge-relayer-daemon/`

*Prepared from ursus shellnet session 2026-06-09. Host:
`ubuntu@ursus-tools.dev:/home/ubuntu/bridge-e2e/`.*
