# P1 — Local acki-nacki devnet

Bare-minimum steps to bring up a fresh local devnet before starting the mt test.

## Preconditions

- **`acki-nacki` checkout** sibling to this `bridge` repo, on branch
  `feature/node-3953-add-test-slow-block-builder-with-300ms-per-block-build-on`
  (Michael's cyclic-hold branch — this is what `tests/mt/cli.py
  test-multithread-cross-thread` needs). Clone:

  ```bash
  cd ~/work   # or wherever bridge/ lives
  git clone https://github.com/gosh-sh/acki-nacki.git
  cd acki-nacki
  git checkout feature/node-3953-add-test-slow-block-builder-with-300ms-per-block-build-on
  ```

  Default assumed layout: `../../acki-nacki` relative to
  `bridge/multithreading/`. Set `ACKI_NACKI_DIR` to point elsewhere.
- Docker Desktop VM: **≥ 13 GiB RAM, ≥ 20 GB disk free**.
  Aerospike hits stop-writes below 12 GiB.
- No stale docker containers/volumes from previous runs — `docker ps -a` empty of node
  containers, `docker volume ls` free of `bm-archive`/aerospike volumes.
- Docker images built (or pullable) for the compose project. On a fresh
  box `make run` will build them on the first invocation; this is slow
  (tens of minutes). If your `docker images` is empty, budget for that.

## Steps

```bash
cd "${ACKI_NACKI_DIR:-../../acki-nacki}"   # assumes CWD is bridge/multithreading/

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
  what the bridge python helper expects. Symptom: `USDCBridge.mintAndSend`
  fails with TVM exit code 209 on a fresh local devnet. Diagnose by
  diffing the acki-nacki `config/USDCBridge.keys.json` against the copy
  the bridge helper actually loads (env var `USDC_BRIDGE_KEY_PATH`, or
  `research/vendored/contracts/USDCBridge.keys.json` if the bridge tree
  ships a vendored copy); overlay whichever is stale.

## Teardown

```bash
cd "${ACKI_NACKI_DIR:-../../acki-nacki}"   # assumes CWD is bridge/multithreading/
make stop     # or: docker compose -f docker/… down -v
```
