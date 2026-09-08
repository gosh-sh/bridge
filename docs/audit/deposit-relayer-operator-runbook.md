# Deposit Relayer — Operator Runbook

**Audience:** bridge operators running `deposit-relayer daemon` on Ursus / shellnet.  
**Scope:** EVM→AN deposit finalization liveness (F10 QC-OFF-02, QC-OFF-04).

---

## Normal operation

1. Configure env from `scripts/ursus/deposit-relayer.env.example`:
   - `BRIDGE_DEPLOY_BLOCK` — Sepolia bridge deploy block (never scan from genesis).
   - `AN_DAPP_ID` — non-zero hex U256 bound into proofs (must match deployed USDCBridge).
   - `AN_GRAPHQL_URL` — **HTTPS** in production (e.g. `https://shellnet.ackinacki.org/graphql`).
   - `AN_KEYS_PATH` — tvm-cli signer JSON; treat as **hot wallet** (see Key custody below).

2. Start the daemon (systemd unit `deposit-relayer.service` on Ursus):
   ```bash
   deposit-relayer daemon \
     --rpc-url "$SEPOLIA_RPC_URL" \
     --bridge-address "$BRIDGE_ADDRESS" \
     --from-block 0 \
     --deposit-prover-dir /path/to/deposit-prover \
     --an-graphql-url "$AN_GRAPHQL_URL" \
     --an-keys-path "$AN_KEYS_PATH" \
     --an-bridge-abi-path "$AN_BRIDGE_ABI_PATH" \
     --an-token-bridge "$AN_TOKEN_BRIDGE" \
     --an-sender "$AN_SENDER" \
     --state ./deposit-relayer-state.json \
     --skip-after-attempts 64
   ```

3. Monitor logs for `finalized on AN`, `already finalized`, or `deposit parked`.

4. Inspect cursor / parked ids:
   ```bash
   jq '{last_processed_deposit_id, parked_deposit_ids, scanned_through_block, attempts_since_progress}' deposit-relayer-state.json
   ```

5. **ETH-16 metrics (no daemon change):** scrape `state.json` with
   `scripts/deposit_relayer_textfile_metrics.sh` into a node_exporter textfile
   directory. Alert on `bridge_deposit_relayer_parked_deposits > 0`, on
   `bridge_deposit_relayer_attempts_since_progress` climbing toward
   `SKIP_AFTER_ATTEMPTS`, and on a stale `bridge_deposit_relayer_state_mtime_seconds`
   (daemon hung or dead). Treat a `deposit parked` log as a page either way.

---

## Manual recovery — `finalize-one`

When the daemon parks a deposit (`parked_deposit_ids` in `state.json`) or proving succeeded but submit failed:

1. **Prove** (if no bundle on disk):
   ```bash
   deposit-relayer prove-one \
     --rpc-url "$SEPOLIA_RPC_URL" \
     --bridge-address "$BRIDGE_ADDRESS" \
     --deposit-id <ID> \
     --deposit-prover-dir /path/to/deposit-prover \
     --out-dir ./out/deposit-<ID>
   ```

2. **Submit** the bundle:
   ```bash
   deposit-relayer finalize-one \
     --bundle-dir ./out/deposit-<ID> \
     --an-graphql-url "$AN_GRAPHQL_URL" \
     --an-keys-path "$AN_KEYS_PATH" \
     --an-bridge-abi-path "$AN_BRIDGE_ABI_PATH" \
     --an-token-bridge "$AN_TOKEN_BRIDGE" \
     --an-sender "$AN_SENDER"
   ```

3. Confirm on shellnet GraphQL: voucher deployed, `confirmDeposit` minted, recipient credited.

4. Restart daemon — nullifier pre-check skips finalized ids; cursor catches up automatically.

**Fast path** when tx hash is known:
```bash
deposit-relayer prove-one ... --deposit-id <ID> \
  --tx-hash 0x... --log-index <receipt-local-index> --out-dir ./out/deposit-<ID>
```

---

## Head-of-line blocking — `--skip-after-attempts`

**Policy (QC-OFF-01):** the official daemon is **strictly sequential**. `next_target = last_processed + 1`; it does not finalize a higher L1 `depositId` while a lower id is still open. That is HOL blocking, not lost USDC on L1. Authors keep this invariant and reject out-of-order finalize (see [qc-off-01-hol-policy.md](qc-off-01-hol-policy.md); GitHub issue [#34](https://github.com/gosh-sh/bridge/issues/34) is not taken).

CLI default is skip disabled (`--skip-after-attempts 0`) so tests stay strictly sequential. **Production systemd** (`scripts/ursus/deposit-relayer.service`) sets `SKIP_AFTER_ATTEMPTS=64` so one stuck id does not block the queue forever: after N consecutive failures the daemon parks the id in `state.json` → `parked_deposit_ids` and advances the cursor. L1 ids stay dense (`0, 1, 2, …`); a hole on AN is from park/skip, not from the chain skipping an id.

```bash
--skip-after-attempts 64   # park after 64 consecutive failures, advance cursor
```

**Run `finalize-one` for each parked id** before assuming the bridge is caught up. That recovery is manual by design.

Competing relayers: if another operator finalizes first, exit code **51** maps to `AlreadyFinalized` — cursor advances without error. Permissionless submitters may still mint any proven id; the daemon itself does not reorder.

---

## Backup relayer SLA (recommended)

| Scenario | Action | Target |
|----------|--------|--------|
| Daemon down | Secondary operator starts standby daemon with **same** `AN_SENDER` keys (single-writer lock prevents dual submit) | Resume within 15 min |
| Parked deposit | Manual `finalize-one` | Within 1 h of park log |
| Proof gen failure | Check RPC, prover dir, SRS; retry tick | Self-heal on transient RPC |
| AN reject (exit 220) | Wrong VK/dappId — stop daemon, fix config, re-prove | Before retry |

Run **at most one live daemon** per `(state file, AN_SENDER)` — `StateLock` enforces exclusive access.

---

## Security — GraphQL TLS & key custody (QC-OFF-04)

### GraphQL endpoint

- **Production:** HTTPS only. Daemon rejects `http://` URLs unless loopback (`127.0.0.1` / `localhost`) or `--allow-insecure-graphql`.
- Pin endpoints in env; do not pass user-controlled URLs.
- Shellnet canonical: `https://shellnet.ackinacki.org/graphql`.

### Signer keys (`AN_KEYS_PATH`)

- File mode `0600`; owned by the service user.
- **Hot wallet** — fund with gas only; rotate if host compromised.
- Do not commit keys; use Ursus `/home/ubuntu/bridge-e2e/config/` layout.
- Separate backup relayer = separate msig + keys (competing finalize is safe; double-mint is not).

---

## Known upstream limitations

- **QC-PROV-04:** axiom-eth receipt padding malleability — accepted for testnet; no relayer-side fix.
- **`is_finalized` pre-check:** returns false until AN read API for `usedDepositIds` ships (QC-OFF-05); submit path remains safe via on-chain nullifier.
