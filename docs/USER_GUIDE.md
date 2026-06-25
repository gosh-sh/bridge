# Acki Nacki Bridge — Operator Guide

Practical, copy-pasteable runbook for deploying and **verifying** the Ethereum-side
bridge contracts on a block explorer (Etherscan / Sepolia Etherscan).

> Scope: this guide focuses on **"Verify & Publish source code"** on Etherscan for
> `AckiNackiBridge.sol` and its companion contracts. For the ZK / proof pipeline see
> `docs/four_circuit_architecture.md`; for end-to-end shellnet runs see
> `docs/shellnet_e2e_acceptance_runbook.md`.

---

## 1. Why verify on Etherscan

Source verification publishes the exact Solidity that produced the deployed bytecode,
so anyone can read the contract on Etherscan, confirm it matches this repo, and use the
**Read / Write Contract** tabs. It is required before we ask auditors or the customer to
trust a deployed address.

There are **two** kinds of contracts we deploy, and they verify differently:

| Contract | Source | Etherscan verifiable as source? |
|----------|--------|----------------------------------|
| `AckiNackiBridge`, `MockBlockHeaderOracle`, mocks | Solidity (this repo) | ✅ Yes — normal flow below |
| `PrimaryAggregatorVerifier`, `FallbackAggregatorVerifier`, `LayerHashesAggregatorVerifier`, `BridgeWithdrawalAggregatorVerifier` | **Raw Yul bytecode** deployed from `verifiers/*.bin` via `ShplonkDeployLib` (CREATE from a `.bin`, no Solidity AST) | ⚠️ Not as Solidity — see [§6](#6-the-shplonk-yul-verifiers-raw-bytecode) |

---

## 2. Prerequisites

```bash
# Foundry (use a recent stable; nightly only if your target chain is brand-new)
foundryup

# Etherscan API key — Etherscan migrated to the unified V2 API:
#   ONE key works across all supported chains (mainnet, Sepolia, Base, ...).
#   Create at https://etherscan.io/myapikey
export ETHERSCAN_API_KEY=XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX
```

You also need:

- The **deployed contract address** (e.g. Sepolia bridge `0x58a1c8d22a79a91db6e7448a7d64d59ad4dc043d`).
- The **chain id** (Sepolia = `11155111`, mainnet = `1`).
- The **constructor arguments** that were used at deploy time (see [§5](#5-getting-the-constructor-arguments)).

> ⚠️ **Compiler settings must match the deployment byte-for-byte.** This repo pins them
> in `contracts/ethereum/foundry.toml`:
>
> | Setting | Value |
> |---------|-------|
> | `solc_version` | `0.8.19` |
> | `optimizer` | `true` |
> | `optimizer_runs` | `1` |
> | `via_ir` | `true` |
> | `evm_version` | default (`paris`) — `shanghai` only under the `fork` profile |
>
> When you run `forge verify-contract` **from inside `contracts/ethereum/`**, Foundry reads
> `foundry.toml` and submits these automatically. Always run it from that directory.

---

## 3. Method A (recommended): verify at deploy time

The least error-prone path is to verify in the same command that deploys. Add `--verify`
to the deploy script run; Foundry deploys, captures the exact constructor args, and submits
verification for every contract the script created:

```bash
cd contracts/ethereum

forge script script/DeployShellnetE2EBridge.s.sol:DeployShellnetE2EBridge \
  --rpc-url "$SEPOLIA_RPC_URL" \
  --private-key "$DEPLOYER_PK" \
  --broadcast \
  --verify \
  --etherscan-api-key "$ETHERSCAN_API_KEY" \
  -vvvv
```

If a contract was deployed from raw bytecode (the SHPLONK verifiers), the script-level
`--verify` will skip / fail on those — that's expected, see [§6](#6-the-shplonk-yul-verifiers-raw-bytecode).

---

## 4. Method B: verify an already-deployed contract

Use this when the contract is already on-chain (the common case for us).

```bash
cd contracts/ethereum

forge verify-contract \
  --chain 11155111 \
  --watch \
  --etherscan-api-key "$ETHERSCAN_API_KEY" \
  --constructor-args "$CONSTRUCTOR_ARGS_HEX" \
  0x58a1c8d22a79a91db6e7448a7d64d59ad4dc043d \
  src/AckiNackiBridge.sol:AckiNackiBridge
```

- `--watch` blocks until Etherscan returns `Pass - Verified` (or a failure reason).
- `--chain` accepts the name (`sepolia`) or the EIP-155 id (`11155111`).
- You do **not** normally pass `--compiler-version` / `--optimizer-runs` / `--via-ir`:
  Foundry derives them from `foundry.toml`. Pass them explicitly only if verifying from
  outside the project root.

---

## 5. Getting the constructor arguments

`AckiNackiBridge` has a struct-heavy constructor:

```solidity
constructor(
    address _blockHeaderOracle,
    address _usdc,
    address _aavePool,
    address _aUSDC,
    VerifyBlockConfig memory _vb,   // (primary, fallback, layerHashes, genesisBkSetCommitment, genesisPrevMaxLevelLayerHash)
    BridgeWithdrawConfig memory _bw // (bridgeWithdrawalVerifier, dappFr, accFr, altDstChainId, altDstHostChainId, altTokenId)
)
```

Pick **one** of these to obtain the ABI-encoded `$CONSTRUCTOR_ARGS_HEX`:

### 5a. Let Foundry guess from on-chain creation code (easiest)

```bash
forge verify-contract --chain 11155111 --watch \
  --etherscan-api-key "$ETHERSCAN_API_KEY" \
  --guess-constructor-args \
  0x58a1c8d22a79a91db6e7448a7d64d59ad4dc043d \
  src/AckiNackiBridge.sol:AckiNackiBridge
```

### 5b. Read them from the broadcast artifact (exact, reproducible)

A `--broadcast` deploy writes the full transaction, including the encoded args:

```bash
# run-latest.json holds the creation tx for the run on this chain id
jq -r '.transactions[] | select(.contractName=="AckiNackiBridge") | .arguments' \
  broadcast/DeployShellnetE2EBridge.s.sol/11155111/run-latest.json
```

The 4-byte-stripped `data` field (creation bytecode) ends with the encoded constructor args;
or take the decoded `arguments` array and re-encode (next option).

### 5c. ABI-encode by hand with `cast`

Structs are encoded as tuples. Example skeleton:

```bash
CONSTRUCTOR_ARGS_HEX=$(cast abi-encode \
  "constructor(address,address,address,address,(address,address,address,uint256,uint256),(address,uint256,uint256,uint256,uint256,uint256))" \
  "$ORACLE" "$USDC" "$AAVE_POOL" "$AUSDC" \
  "($PRIMARY,$FALLBACK,$LAYERHASHES,$GENESIS_BK_SET_COMMITMENT,$GENESIS_PREV_MAX_LEVEL_LAYER_HASH)" \
  "($WITHDRAW_VERIFIER,$DAPP_FR,$ACC_FR,$ALT_DST_CHAIN_ID,$ALT_DST_HOST_CHAIN_ID,$ALT_TOKEN_ID)")
```

Disabled sub-systems use zero addresses / zeros (e.g. AAVE off → `_aavePool=_aUSDC=0`,
withdraw off → the `_bw` tuple is `(0x0,0,0,0,0,0)`).

---

## 6. The SHPLONK Yul verifiers (raw bytecode)

`PrimaryAggregatorVerifier` / `FallbackAggregatorVerifier` / `LayerHashesAggregatorVerifier`
(and the future `BridgeWithdrawalAggregatorVerifier`) are **snark-verifier-generated Yul**,
deployed from `contracts/ethereum/verifiers/*.bin` via `ShplonkDeployLib` — there is no
Solidity source to submit. Standard "Solidity (Single/Standard-JSON)" verification will not
match.

Options, in order of preference:

1. **Leave them unverified** but publish provenance: the `.bin` + its keccak, the
   snark-verifier rev, and the regen command live in `contracts/ethereum/verifiers/README.md`.
   Anyone can reproduce the bytecode and compare to on-chain `extcodehash`.
2. **Verify as Yul** via Etherscan's *Standard-JSON-Input* upload with `"language": "Yul"`
   if/when the generated `.yul` is checked in. (We currently ship only the compiled `.bin`.)
3. **Sourcify** sometimes accepts a metadata-based match; try
   `--verifier sourcify` if Etherscan rejects.

Document whichever you choose on the deployment record so reviewers aren't surprised by a
"not verified" badge on those addresses.

---

## 7. Etherscan V2 API gotchas

Etherscan's V2 API is now **mandatory**. Two failure modes show up:

- **`ETHERSCAN_API_KEY must be set ...`** even though it is set → usually means Foundry
  doesn't recognise the `--chain` value. Pass a known chain id/name, or for an unmapped
  explorer add an explicit V2 endpoint:

  ```bash
  forge verify-contract ... \
    --verifier etherscan \
    --verifier-url "https://api.etherscan.io/v2/api?chainid=11155111" \
    --etherscan-api-key "$ETHERSCAN_API_KEY"
  ```

- **Brand-new chains** Foundry doesn't know yet: use a recent `foundryup -i nightly`, since
  chain support lands in nightly before stable.

Supported chain ids: <https://api.etherscan.io/v2/chainlist>.

---

## 8. Worked example — Sepolia E2E bridge

```bash
cd contracts/ethereum
export ETHERSCAN_API_KEY=...   # your V2 key

forge verify-contract \
  --chain 11155111 \
  --watch \
  --guess-constructor-args \
  --etherscan-api-key "$ETHERSCAN_API_KEY" \
  0x58a1c8d22a79a91db6e7448a7d64d59ad4dc043d \
  src/AckiNackiBridge.sol:AckiNackiBridge
```

Expected tail:

```
Submitting verification for [src/AckiNackiBridge.sol:AckiNackiBridge] 0x58a1...043d.
Submitted contract for verification:
        Response: `OK`
        GUID: `xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx`
        URL: https://sepolia.etherscan.io/address/0x58a1...043d
Contract successfully verified
```

Known deployed addresses (non-secret, from `AGENTS.md`):

| Purpose | Network | Address |
|---------|---------|---------|
| Deposits bridge | Sepolia | `0x99c37fb75326ae6953ebbbdcd261ec331df4ce82` |
| AN→ETH E2E bridge (withdraw) | Sepolia | `0x58a1c8d22a79a91db6e7448a7d64d59ad4dc043d` |

---

## 9. Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| `Bytecode does NOT match` | compiler settings differ | run from `contracts/ethereum/`; confirm solc `0.8.19`, `optimizer_runs=1`, `via_ir=true` |
| `Unable to verify` / metadata mismatch | via-IR metadata hash | use `--show-standard-json-input` and submit the JSON manually in the Etherscan UI |
| `ETHERSCAN_API_KEY must be set` (but it is) | unrecognised `--chain` | pass numeric id + `--verifier-url https://api.etherscan.io/v2/api?chainid=<id>` |
| Constructor args rejected | wrong/missing encoding | use `--guess-constructor-args`, or pull from `broadcast/.../run-latest.json` |
| SHPLONK verifier won't verify | raw Yul bytecode, no Solidity source | see [§6](#6-the-shplonk-yul-verifiers-raw-bytecode) — publish provenance instead |

---

## 10. After verification

1. Confirm the green ✓ badge on the address page and that **Read/Write Contract** tabs render.
2. Record the verified address + explorer URL in the deployment notes / `AGENTS.md`.
3. For the raw-bytecode verifiers, link the `verifiers/README.md` provenance entry.
