# P1 — Local acki-nacki devnet

Bare-minimum steps to bring up a fresh local devnet before starting the mt test.
Mirrors `/Users/alinat/HALO2_TVM_EXPERIMENTS/run_acki_nacki_node.md`.

## Preconditions

- Docker Desktop VM: **≥ 13 GiB RAM, ≥ 20 GB disk free**.
  Aerospike hits stop-writes below 12 GiB.
- No stale docker containers/volumes from previous runs — `docker ps -a` empty of node
  containers, `docker volume ls` free of `bm-archive`/aerospike volumes.

## Steps

```bash
cd /Users/alinat/HALO2_TVM_EXPERIMENTS/acki-nacki

cargo clean
cargo update

# Only needed the first time or after any zerostate-affecting change (contracts, config, …).
make generate_zerostate

make run
```

## Verify

```bash
# GQL up on nginx0 → 8700
curl -s http://localhost:8700/graphql -H 'Content-Type: application/json' \
  -d '{"query":"{ blockchain { blocks(last:1) { edges { node { seq_no thread_id } } } } }"}' \
  | jq .

# Should see a growing seq_no over successive calls.
```

## Common gotchas

- **`bk_set` mismatch** — for shellnet, `BRIDGE_BK_SET_CONFIG` must be set. For local
  devnet the default `bk_set.local.json` works and no override is needed.
- **Zerostate keys drift** — `contracts/USDCBridge.keys.json` in acki-nacki must match
  what the bridge python helper expects. See
  `feedback/bridge_python_orchestrator_usdc_keys.md` in `MEMORY.md`.

## Teardown

```bash
cd /Users/alinat/HALO2_TVM_EXPERIMENTS/acki-nacki
make stop     # or: docker compose -f docker/… down -v
```
