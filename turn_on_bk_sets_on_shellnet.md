# Changing `minBK` on shellnet (enable/disable Block Keeper rotation)

How to **enable BK rotation** on shellnet (`minBK = 4`) and how to **disable it** (`minBK = 5`).

---

## TL;DR

Shellnet runs with **5 active BKs**. The `minBlockKeepers` parameter of the `BlockKeeperContractRoot`
contract sets the minimum allowed number of active BKs. A BK can leave its epoch (and free its slot for
a new one) **only if at least `minBK` active BKs remain after it leaves**.

| `minBK` | `activeBK - 1 >= minBK` (with 5 BKs) | Behaviour | When |
|--------:|:------------------------------------:|:----------|:-----|
| **4**   | `5 - 1 = 4 >= 4` → **true**          | a BK can leave → its slot frees up → **rotation runs** | BK rotation is needed |
| **5**   | `5 - 1 = 4 >= 5` → **false**         | BKs are held in their epoch (`cantDelete`) → **no rotation** | no rotation needed |

It is changed with a single call to the **`setConfig`** method of `BlockKeeperContractRoot`
(address `0:7777…7777`, dapp 0), signed with the owner key.

---

## How it works

Contract: `contracts/bksystem/BlockKeeperContractRoot.sol` in the acki-nacki repository.

State field:

```solidity
uint128 _minBlockKeepers = 12;   // default in the source; set on shellnet via setConfig
```

The rotation gate is in `decreaseActiveBlockKeeper(...)` (called when a BK's epoch has ended and the BK wants to leave):

```solidity
if (_numberOfActiveBlockKeepers - 1 >= _minBlockKeepers) {
    _numberOfActiveBlockKeepers -= 1;
    // ... BlockKeeperEpoch(msg.sender).canDelete(...)  → the BK is removed, its slot is free → a new one joins
} else {
    BlockKeeperEpoch(msg.sender).cantDelete(...);       // → the BK stays, no rotation
}
```

As a result, the network **does not let** the number of active BKs drop below `minBK`. Therefore:

- with `minBK = 5` and exactly 5 BKs, none of them can leave (that would drop to 4) → all are pinned, no rotation;
- with `minBK = 4`, one BK can leave (4 ≥ 4 remain) → its slot frees up and a candidate joins → a rotation cycle.

> ⚠️ **For a real rotation with `minBK = 4`, candidates must be queued** (BK wallets that have
> sent a stake request). Without candidates the old BK leaves with no replacement, and the network
> simply drops to 4 active BKs.

---

## ⚠️ The main pitfall: `setConfig` overwrites the WHOLE config

Signature (from the compiled ABI `contracts/0.79.3_compiled/bksystem/BlockKeeperContractRoot.abi.json`):

```
setConfig(
    uint64  epochDuration,
    uint128 minBlockKeepers,
    bool    isNeedNumberOfActiveBlockKeepers,
    uint128 needNumberOfActiveBlockKeepers,
    uint8   walletTouch,
    uint128 nlinit
)
```

The method **does not patch one field — it overwrites all six in a single call**. In addition, derived
values are recomputed from `epochDuration`:

```solidity
_epochCliff = epochDuration / 10;   // CONFIG_CLIFF_DENOMINATOR
_waitStep   = epochDuration / 20;   // CONFIG_WAIT_DENOMINATOR
```

So to change **only** `minBlockKeepers`, pass the **current** values of the other five fields.
Otherwise you will accidentally reset the epoch length, `walletTouch` or `nlinit`.

### Current values to preserve

| Field | Value on shellnet | How to find it |
|:------|:------------------|:---------------|
| `epochDuration` | **`10800`** (measured on shellnet 14-07-2026, see Step 1) | getter **`getConfig`** → `epochDuration`. It does not equal the Python default `660`! |
| `isNeedNumberOfActiveBlockKeepers` | `false` | fixed when the zerostate was generated |
| `needNumberOfActiveBlockKeepers` | `0` | fixed when the zerostate was generated |
| `walletTouch` | `200` | **no getter** — from the deploy env (`WALLET_TOUCH`, default 200) |
| `nlinit` | `5000` | **no getter** — from the deploy env (`NLINIT`, default 5000) |

> ⚠️ There are **no** getters for `minBlockKeepers`, `walletTouch`, `nlinit` or `needNumber…` — their
> values cannot be read from the contract. `epochDuration` is read via `getConfig`; take the others from
> the shellnet deploy configuration. **If shellnet was deployed with a non-standard env, check them
> before the call.**

Source of the defaults: `contracts/scripts/generate_zerostate.py` in the acki-nacki repository
(`EPOCH_LENGTH_AFTER_ZEROSTATE=660`, `MIN_BLOCKKEEPERS=5`, `WALLET_TOUCH=200`, `NLINIT=5000`,
`isNeed=false`, `needNumber=0`).

> ⚠️ **Important:** the Python default `EPOCH_LENGTH_AFTER_ZEROSTATE=660` is NOT what runs on
> shellnet. The 14-07-2026 shellnet was actually deployed with `epochDuration=10800`. Always run
> Step 1 `getConfig` before `setConfig` and pass the live value, otherwise you shrink the epoch
> 16-fold in one go.

> `epochDuration` is counted in **blocks**, not seconds: a BK epoch ends once
> `block.seqno >= seqNoStart + epochDuration` (`BlockKeeperEpochContract.sol`). At shellnet's current
> ~3 blocks/s, `10800` is about **1 hour** — matching the observed cadence on 2026-10-13: a burst of 5
> rotations every ~10 850 blocks.

---

## Keys and permissions

`setConfig` is protected by the `onlyOwner` modifier:

```solidity
modifier onlyOwner { require(msg.pubkey() == tvm.pubkey(), ERR_NOT_OWNER); _; }
```

→ sign with the **`BlockKeeperContractRoot` owner key** — the keypair the shellnet zerostate was
deployed with (`config/BlockKeeperContractRoot.keys.json` of that deploy).

> Before the call, confirm with SeHor05 which keys file is the BK-root owner on the current shellnet.

---

## Step by step

### Self-service environment (all local — no need to involve Sergey)

```bash
# tvm-cli v3.1.0 (a local build). IMPORTANT: /usr/bin/tvm-cli 2.24.x is incompatible
# with the new node — do not use it.
CLI=/path/to/tvm-cli

# Public shellnet GraphQL endpoint
ENDPOINT="https://shellnet.ackinacki.org/graphql"

# BlockKeeperContractRoot (dapp 0) — a fixed address, the same on every deploy
ROOT=0:7777777777777777777777777777777777777777777777777777777777777777

# ABI from a local acki-nacki clone
ABI=/path/to/acki-nacki/contracts/0.79.3_compiled/bksystem/BlockKeeperContractRoot.abi.json

# Shellnet BK-root owner key (confirmed for the 14-07-2026 deploy)
#   pubkey: 44577cbc0d95eaa5fdca4defdbef3a8bf4a8acd6ae58f9fe368d7952bb01ea41
KEYS=/path/to/shellnet_config_14_07_2026/BlockKeeperContractRoot.keys.json

# Put the endpoint into the CLI config (so --url is not needed on every call)
$CLI config --url "$ENDPOINT"

# Check that the CLI and the ABI are in place
$CLI version | head -1    # → "tvm-cli 3.1.0"
test -r "$ABI"   && echo "ABI ok"
test -r "$KEYS"  && echo "KEYS ok"
```

> ⚠️ Set the three paths for your machine. The files (`ABI`, `KEYS`, `CLI`) must not change during
> the experiment — do not rebuild `acki-nacki` and do not touch `shellnet_config_14_07_2026/`
> between Steps 1–5. tvm-cli 3.x expects `--addr` as `dapp_id::account_id`: if it rejects
> `$ROOT`, use `0000000000000000000000000000000000000000000000000000000000000000::7777…7777`.

### Step 1. Read the current `epochDuration` and record the "before" state

```bash
$CLI -j runx --abi "$ABI" --addr "$ROOT" -m getConfig
# shellnet 14-07-2026:
# → {"epochDuration":"10800","epochCliff":"1080","waitStep":"540",...}

$CLI -j runx --abi "$ABI" --addr "$ROOT" -m getDetails
# → {"minStake":"...","numberOfActiveBlockKeepers":"5",...}
```

- Take `epochDuration` from the first output (on the 14-07-2026 shellnet it is `10800` blocks, about
  1 hour at ~3 blocks/s; still `10800` on 2026-10-14). If you get a different value, use **yours** in every `setConfig` call below;
  `_epochCliff/_waitStep` are recomputed automatically as `epochDuration/10` and `epochDuration/20`.
- Write down `numberOfActiveBlockKeepers` from the second output — it is the baseline for Step 3.
  For the 14-07-2026 shellnet the expected value is `5`.

> `walletTouch` and `nlinit` **have no getter**. The 14-07-2026 shellnet was deployed with the standard
> env → defaults `walletTouch=200`, `nlinit=5000` (see `generate_zerostate.py`). If you suspect a
> non-standard deploy, extract them from `shellnet_config_14_07_2026/zerostate` or confirm with an admin
> **before** Step 2. With wrong values `setConfig` overwrites them with 200/5000.

### Step 1.5. Dry run (nothing is sent)

To make sure the JSON payload parses correctly and the ABI matches, run **the same body** through
`runx` (a local simulator, no signature and no broadcast):

```bash
$CLI -j runx --abi "$ABI" --addr "$ROOT" -m setConfig \
  '{"epochDuration":10800,"minBlockKeepers":4,"isNeedNumberOfActiveBlockKeepers":false,"needNumberOfActiveBlockKeepers":0,"walletTouch":200,"nlinit":5000}'
```

- Exit code 0 and an empty `{}` in the output → the payload is valid.
- `Wrong data format` / `Field * is missing` → fix the JSON, do not send `callx`.
- `Not found` on a field → **wrong ABI**; check the path.

### Step 2a. ENABLE rotation → `minBK = 4`

```bash
$CLI callx --abi "$ABI" --addr "$ROOT" --keys "$KEYS" -m setConfig \
  '{"epochDuration":10800,"minBlockKeepers":4,"isNeedNumberOfActiveBlockKeepers":false,"needNumberOfActiveBlockKeepers":0,"walletTouch":200,"nlinit":5000}'
```

### Step 2b. DISABLE rotation → `minBK = 5`

```bash
$CLI callx --abi "$ABI" --addr "$ROOT" --keys "$KEYS" -m setConfig \
  '{"epochDuration":10800,"minBlockKeepers":5,"isNeedNumberOfActiveBlockKeepers":false,"needNumberOfActiveBlockKeepers":0,"walletTouch":200,"nlinit":5000}'
```

> Use **your** `epochDuration` (from Step 1), `walletTouch` and `nlinit` if the deploy was non-standard.

### Step 3. Verify

1. **The transaction went through** — no errors in the `callx` output, exit code 0.
2. **Number of active BKs** (there is no getter for `minBK` itself):

   ```bash
   $CLI -j runx --abi "$ABI" --addr "$ROOT" -m getDetails
   # → {"minStake":"...","numberOfActiveBlockKeepers":"5"}
   ```

3. **In practice** — watch the network for an epoch (`epochDuration` blocks; at ~3 blocks/s that is **~1 hour**):
   - with `minBK = 4`, when an epoch ends one of the BKs moves to the cooler and a candidate takes its
     place (the number of active BKs roughly stays the same, the membership changes);
   - with `minBK = 5`, the set of active BKs does not change; all of them serve out their epoch or are
     re-elected in place.

4. **BK set via GraphQL** — what the relayer sees:

   ```bash
   # the latest BK-set update that reached the chain
   curl -s "$ENDPOINT" -H 'content-type: application/json' -d '{
     "query":"{ blockchain { bkSetUpdates(last: 1) { edges { node { height block_id } } } } }"
   }' | jq
   ```

   - with `minBK = 4` this height should keep growing as rotations happen;
   - with `minBK = 5` it should freeze at its last value.

### Step 5. Rollback (DISABLE again) — in case of trouble

The same command as Step 2b, but keep it ready in a separate terminal **before** Step 2a:

```bash
$CLI callx --abi "$ABI" --addr "$ROOT" --keys "$KEYS" -m setConfig \
  '{"epochDuration":10800,"minBlockKeepers":5,"isNeedNumberOfActiveBlockKeepers":false,"needNumberOfActiveBlockKeepers":0,"walletTouch":200,"nlinit":5000}'
```

It takes effect in the contract immediately, but rotation physically stops only when the next BK tries
to leave and gets `cantDelete` — i.e. not before the next epoch ends.

> ⚠️ A rollback **does not restore** the previous BK membership: if the membership changed during the
> experiment, it stays as it is at the moment of the rollback. That is fine — the current BK set is a
> working one and has already joined the network; it only affects later cycles.

---

## Coordination with the relayer (bridge daemon)

Running BK-set rotation on shellnet is exactly the load the
[relayer runbook](crates/bridge-relayer-daemon/docs/live_relayer_bridge_runbook.md) was prepared for.
Two safe scenarios:

### Scenario A: the daemon is already running (steady state at `minBK=5`)

1. Make sure the daemon is healthy (runbook §4, "Healthy operation").
2. Run Step 2a (enable `minBK=4`).
3. Watch for `applyBkSetUpdate` transactions in the daemon log, starting with the next BK-set update
   in GraphQL (Step 3, item 4).
4. If something goes wrong, run Step 5 (rollback) **WITHOUT stopping the daemon**. The daemon finishes
   the update already in flight and then continues with `verifyBlock` only.

### Scenario B: the daemon is not running yet

1. Run Step 2a (enable `minBK=4`) in advance, so the first BK-set update reaches the chain **before**
   the cold start.
2. Wait for the next BK-set update height (Step 3, item 4).
3. Start the daemon per Case 1 of the runbook (§3) — it picks the update up as usual.
4. Roll back (Step 5) when ready, with the daemon already live.

### What NOT to do

- **DO NOT** toggle `minBK` 4↔5 more often than once per epoch — the relayer may not have finished
  processing the previous `applyBkSetUpdate`.
- **DO NOT** edit `bk_set.shellnet.json`: it is the BK set from before rotations began, and every
  BK-set rebuild for a new bridge replays the changes on top of it (runbook §3.2). The daemon starts
  from the set built for its seed and afterwards follows `prover_bk_set.json` and GraphQL
  `bkSetUpdates`.

---

## Troubleshooting

| Symptom | Cause | What to do |
|:--------|:------|:-----------|
| `callx` → `exit_code = 100` (`ERR_NOT_OWNER`) | Wrong KEYS | Compare the `pubkey` in `BlockKeeperContractRoot.keys.json` with the one used at deploy time. For the 14-07-2026 shellnet it is `44577cbc…ea41`. |
| `callx` → `Wrong data format` | The JSON payload does not match the ABI | Run Step 1.5 (dry run). Check that all 6 fields are passed, with number/bool types as in the ABI. |
| `callx` → `Account not found` or `No account` | Wrong ENDPOINT / the node does not respond | `curl -s "$ENDPOINT" -d '{"query":"{info{version}}"}' \| jq` must return a version. |
| `getConfig` returns the old `epochCliff/waitStep` after `setConfig` | The transaction is not finalized yet | Wait ~5–10 s and retry. |
| After `setConfig minBK=4` the BK membership does not change after one epoch or two | No stake candidates are queued | Check that someone has sent a stake request. Without candidates rotation is physically impossible (BKs just stay in the coolers → fewer than 5 active). |
| No `applyBkSetUpdate` appears in the daemon log | The GraphQL BK-set update height does not grow | Repeat Step 3, item 4. If it is frozen, check on the node that `BlockKeeperContractRoot` really received `setConfig` (`getConfig` → `_epochCliff = epochDuration/10`). |

---

## Notes

- `minBK` **does not affect stake economics**: with `isNeedNumberOfActiveBlockKeepers = false`,
  `minStake` is computed from `numberOfActiveBlockKeepersAtBlockStart`, not from `minBK`. The parameter
  works **only** as the exit/rotation gate.
- `setConfig` can be called any number of times — toggling 4 ↔ 5 back and forth is safe (as long as
  the other 5 fields are passed correctly every time).
- The threshold is relative to the **current number of active BKs**. The gate formula is
  `activeBK - 1 >= minBK`. For a network of a different size (not 5 BKs), pick `minBK` for the desired
  behaviour using the same formula.
- A change takes effect from the next epoch-end cycle, not immediately.
