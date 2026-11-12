# NACKL Bridge extension — requirements

| | |
|---|---|
| Status | **Normative, gosh-sh/bridge-audit#152.** What the bridge must do to carry NACKL between Acki Nacki and an EVM chain: the nine points of that issue and the two design decisions recorded on it (an external library for the EVM logic; on-chain checks plus the owner's procedure for the minter handover). Every requirement is covered by the test case named in [§12](#12-traceability). |
| Written against | `fd9a2ca8`, `eccBridge` 1.18.0 of gosh-sh/bridge#166 (bridge-audit#139 requirement 7), which the #152 branch is stacked on. A `file:line` points at that commit and marks behaviour that stays as it is; a requirement without one is new. Re-verify against the merge commit and bump this line when the #152 PR lands. |
| Genre | Design and correctness. The Ethereum contract reference is [`EVM-contracts-spec.md`](EVM-contracts-spec.md); the Acki Nacki contracts are described in [`contracts/an/README.md`](../contracts/an/README.md). |
| Paths | Relative to the repository root. `tvm-sdk` and `acki-nacki` paths are in those repositories, at the commits named where they are cited. |

## 1. Method

Requirements are written in **EARS** (Easy Approach to Requirements Syntax). Each has an ID and one
of these forms:

| Form | Template | Becomes |
|---|---|---|
| Ubiquitous | The ‹system› shall … | a test that the behaviour holds |
| Event-driven | When ‹trigger›, the ‹system› shall … | a test that fires the trigger |
| State-driven | While ‹state›, the ‹system› shall … | a test in that state |
| Unwanted behaviour | If ‹condition›, then the ‹system› shall … | a negative test that forces the condition |
| Optional feature | Where ‹feature›, the ‹system› shall … | a test on the configuration that has the feature |

Properties that hold across every operation are in [§11](#11-properties); each becomes a Foundry
fuzz or invariant test, or a TVM test that checks the property after every step of a sequence.

A requirement that cannot be checked by a test says so and says what checks it instead.

## 2. Glossary

| Term | Meaning |
|---|---|
| **NACKL** | The Acki Nacki native token, ECC currency id 1 (`ECC_NACKL_KEY`, `tvm-sdk` `tvm_vm/src/executor/token.rs:41`), 9 decimals (`acki-nacki` `helpers/zerostate-helper/src/generate/contracts.rs:802`, commit `a99209ef3`). |
| **ERC20 NACKL** | The ERC-20 that represents NACKL on an EVM chain: `contracts/ethereum/src/NacklToken.sol`, OpenZeppelin `ERC20` + `ERC20Burnable` + `Ownable`. Its owner is the only account that can mint it. |
| **AN bridge** | `eccBridge`, `contracts/an/exchange/eccBridge.sol`. |
| **EVM bridge** | `AckiNackiBridge`, `contracts/ethereum/src/AckiNackiBridge.sol`. |
| **Lock** | NACKL that a withdrawal leaves on the AN bridge instead of burning it. |
| **Locked amount** (`locked[c]`) | The NACKL the AN bridge holds on behalf of EVM chain `c`: increased by every withdrawal to `c`, decreased by every deposit from `c`. It lives in `BridgeTokenConfig.outstanding` (`contracts/an/exchange/modifiers/modifiers.sol:11`) of the NACKL entry for `c`, so `updateCode` carries it without a format change (`eccBridge.sol:1116-1124`). |
| **Unlock** | The AN bridge sending locked NACKL to the recipient of a proven deposit. |
| **Payout** | The EVM bridge minting ERC20 NACKL to the recipient of a proven withdrawal. |
| **Queued payout** | A payout `withdrawByProof` has accepted — the proof verified, the nullifier spent — but not minted yet, because of the daily limit or the delay. It is keyed by the withdrawal's nullifier. |
| **Daily limit** (`D`) | The most ERC20 NACKL the EVM bridge mints within any 24 hours. Set by the owner. |
| **Delay threshold** (`T`) | A payout above it is queued for 24 hours before it can be minted. Set by the owner. |
| **Window** | The last 24 hours, counted in whole hours: a mint at time `t` counts against `D` until the start of the 25th hour after the hour `t` falls in, so for at least 24 and at most 25 hours. |
| **Capacity** | `D` less what the window has minted. A payout fits when its amount is at most the capacity. |
| **Minter handover** | Moving the ownership of ERC20 NACKL from an EVM bridge to its successor, the bridge whose `predecessor()` names it. |
| **Abandoned payout** | A queued payout the owner names in the minter handover. It is removed with an event and is never minted. Only for a payout the owner holds to be forged; see HO-3. |
| **Owner key** | The ed25519 key in the AN bridge's `_ownerPubkey`. It authorises `updateCode` (`eccBridge.sol:1116`), `setBridgeToken` and the recovery calls. |
| **Special account** | An Acki Nacki account that the node lets mint and burn ECC: the config account and the accounts listed in `special_contracts` (`tvm-sdk` `tvm_executor/src/blockchain_config.rs:345-351`). |

## 3. The supply rule

NACKL exists on Acki Nacki. On an EVM chain `c` the bridge only represents NACKL that is locked on
Acki Nacki for `c`:

> **supply of ERC20 NACKL on `c` ≤ `locked[c]`**

Withdrawals raise both sides by the same amount (lock, then mint after the proof; a queued payout is
locked and not yet minted). Deposits lower both (burn, then unlock after the proof). A holder who
burns ERC20 NACKL directly lowers only the left side. The rule breaks only if a forged proof mints on
the EVM chain or unlocks on Acki Nacki. The AN bridge cannot unlock more than `locked[c]` (AN-6),
which bounds a forged deposit proof; the EVM bridge has no such bound, so a forged withdrawal proof
is bounded by the daily limit and the delay instead (§7).

## 4. Acki Nacki bridge (AN)

| ID | Form | Requirement |
|---|---|---|
| AN-1 | Ubiquitous | `setBridgeToken` shall accept NACKL (id 1) as well as USDC (id 3) and refuse every other id with `ERR_TOKEN_CONFIG` (242). Today it accepts id 3 only (`eccBridge.sol:721`). For NACKL the USDC ceilings `MAX_TRANSFER_AMOUNT` and `MAX_BRIDGE_TVL` do not apply; `maxTransfer` stays within `uint64` (`:722`) and is also the per-withdrawal cap, `maxWithdrawal`, which never decreases (`:735-736`). |
| AN-2 | Event-driven | When `initiateWithdrawal` receives NACKL for a chain whose NACKL entry is registered and whose EVM bridge is set, within `maxWithdrawal`, the AN bridge shall keep the NACKL, add the amount to `locked[dstChainId]` and emit `WithdrawalInitiated` with the same fields as today (`:466-467`). |
| AN-3 | Unwanted | If a NACKL withdrawal is refused — the entry is not registered (241), no EVM bridge is set (239), the amount is over `maxWithdrawal` (234) — then the AN bridge shall return the NACKL with `WithdrawalRefused`, as it does for USDC (`:420-437`). |
| AN-4 | Ubiquitous | A NACKL withdrawal shall not be bounded by bridged minted less bridged burned or by the chain's outstanding amount: those bound a burn of what the bridge minted (`:441-452`), and a lock mints nothing. |
| AN-5 | Event-driven | When a NACKL deposit is confirmed (`confirmTokenDeposit`, `:1035`), the AN bridge shall send `amount` of the NACKL it holds to the recipient and subtract it from `locked[chainId]`. |
| AN-6 | Unwanted | If a NACKL deposit's amount is above `locked[chainId]`, then `finalizeDeposit` shall refuse it before `tvm.accept()` with `ERR_CHAIN_BALANCE_EXCEEDED` (236), and `confirmTokenDeposit` and the recovery call of AN-8 shall refuse it with the same code. |
| AN-7 | Ubiquitous | For NACKL, `finalizeDeposit` and `confirmTokenDeposit` shall keep every other check they make for USDC: deposits enabled, `maxTransfer` on a new deposit, `maxWithdrawal` on confirmation, the registered token (`:743-750`). The TVL cap (`:751-752`) does not apply to NACKL: the bound is `locked[chainId]`. |
| AN-8 | Event-driven | When the owner pays an aborted NACKL deposit with `mintAndSendTokenBridged` (`:353`), the AN bridge shall unlock — send held NACKL and subtract it from `locked[chainId]` — instead of minting. |
| AN-9 | Ubiquitous | The AN bridge shall never mint or burn NACKL: no code path calls `gosh.mintecc` or `gosh.burnecc` with id 1. `getTotalBridged(1)` shall stay `(0, 0)`. |
| AN-10 | Ubiquitous | Locked NACKL shall leave the AN bridge only by an unlock (AN-5, AN-8). NACKL sent to the bridge outside `initiateWithdrawal` is not locked for any chain and no call sends it out. |
| AN-11 | Ubiquitous | `updateCode` shall carry `locked[c]` for every chain unchanged (`:1116-1124`, `:1157-1195`). |
| AN-12 | Ubiquitous | `getTokenDepositConfig` and `getTokenWithdrawalConfig` (`:769-787`) shall report `locked[c]` as `outstanding` for the NACKL entry. |
| AN-13 | Ubiquitous | `setOutstandingByChain` (`:822`) shall keep writing the USDC counter only. |
| AN-14 | Ubiquitous | The USDC paths shall not change: `mintecc` on deposit, `burnecc` on withdrawal, the accounting of `_totalMintedBridgeByToken[3]`, `_totalBurnedBridgeByToken[3]` and `_outstandingByChain`. |
| AN-15 | Ubiquitous | The AN bridge's version shall become `1.19.0` (`:57`). The CLI's `NEWEST_KNOWN_BRIDGE_VERSION` shall follow it, and the CLI shall refuse to move NACKL through a bridge older than `1.19.0`. |

### The owner key (KEY)

These are findings, not requirements a test of this repository can check. Each one names what does
check it.

| ID | Statement |
|---|---|
| KEY-1 | The node lets the AN bridge mint NACKL. `MINTECC` takes any currency id (`tvm-sdk` `tvm_vm/src/executor/token.rs:160-169`), and the action phase applies `MintToken` for any special account without looking at the id (`tvm-sdk` `tvm_executor/src/transaction_executor.rs:949-962`, the same code at `:918` in `6edd3279`, the revision the node pins). The AN bridge mints USDC, so it is a special account. Code installed by `updateCode` can therefore mint NACKL, and send away every locked NACKL. AN-9 and AN-10 hold for the code in this repository, not for code the owner key installs. |
| KEY-2 | The owner key is generated with the zerostate: `acki-nacki` `contracts/scripts/generate_zerostate.py:1284-1290` runs `tvm-cli getkeypair` into `config/USDCBridge.keys.json` when `GENERATE_KEYS` is set and reads that file otherwise. Who holds it for a given network is not in any code; the team that generated the zerostate has to name the holder. |
| KEY-3 | A guarantee that holds against the owner key needs the node: refuse `MintToken` and `BurnToken` with id 1 from the AN bridge's address. That is a change to `tvm-sdk`/`acki-nacki`, out of scope here. |

## 5. ERC20 NACKL (TOK)

| ID | Form | Requirement |
|---|---|---|
| TOK-1 | Ubiquitous | ERC20 NACKL shall be OpenZeppelin `ERC20`, `ERC20Burnable` and `Ownable` (`@openzeppelin/contracts` 5.7.0, pinned in `contracts/ethereum/package.json`), with `decimals()` = 9. |
| TOK-2 | Ubiquitous | Only the owner shall mint (`mint(to, amount)`, `onlyOwner`). Anyone may burn their own tokens or tokens they are approved for (`burn`, `burnFrom`). |
| TOK-3 | Ubiquitous | The owner is set in the constructor, together with an initial holder and an initial supply. |
| TOK-4 | Ubiquitous | The ERC20 NACKL that a bridge pays out on a public network shall be deployed with a zero initial supply and the bridge as its owner, so that no other account can ever mint it. `script/DeployNacklToken.s.sol` refuses a non-zero supply unless `NACKL_TEST_TOKEN=1`. |
| TOK-5 | Where the token is a test token, it shall be deployed with an initial supply to a test holder (`NACKL_TEST_TOKEN=1`, `NACKL_INITIAL_HOLDER`, `NACKL_INITIAL_SUPPLY`). |

## 6. EVM bridge: listing, deposit and payout (EVM)

| ID | Form | Requirement |
|---|---|---|
| EVM-1 | Event-driven | When the owner calls `listNackl(token, maxTransfer, dailyLimit, delayThreshold)`, the EVM bridge shall list `token` under id 1 (`NACKL_TOKEN_ID`) with deposits open, as `_listToken` does (`AckiNackiBridge.sol:1110`), and set the daily limit and the delay threshold. |
| EVM-2 | Unwanted | If NACKL is already listed, or `token.decimals()` is not 9, or the limits break LIM-5, then `listNackl` shall revert. No other token can be listed after the constructor (`:1107-1109`). |
| EVM-3 | Event-driven | When `deposit` is called for ERC20 NACKL, the EVM bridge shall make the checks it makes for every token (`:1000-1007`), burn exactly `amount` from the caller with `burnFrom` — the caller approves the bridge, as for any token — and emit `Deposit` with the same fields as today (`:1017`). It shall hold no NACKL and book no liabilities. |
| EVM-4 | Event-driven | When `withdrawByProof` is called for id 1, the EVM bridge shall make every check it makes today up to and including the proof (`:1851-1916`), spend the nullifier, and then mint or queue the payout (LIM, DLY). It shall not check liabilities (`:1919-1922`): ERC20 NACKL is minted, not held. |
| EVM-5 | Unwanted | If `fundToken` or `migrateToken` is called for id 1, then it shall revert: there is no NACKL custody to move. The minter moves with HO-1. |
| EVM-6 | Unwanted | If `setTokenLimits` would set NACKL's `maxTransfer` above the daily limit, then it shall revert with `NacklLimitsInvalid`: `maxWithdrawal` follows `maxTransfer` up (`:1039`) and LIM-5 keeps it within the daily limit. |
| EVM-7 | Ubiquitous | The USDC paths shall not change. |
| EVM-8 | Ubiquitous | The NACKL logic shall live in the external library `NacklMint` (`contracts/ethereum/src/NacklMint.sol`), linked into the bridge at deployment; the bridge's runtime code shall stay within EIP-170. |

## 7. Rate limiting the mint (LIM, DLY)

Modelled on the Wormhole Governor (a 24-hour sliding window per chain, delayed large transfers) and
on the Chainlink CCIP rate limits (owner-set capacity per token). There is no ceiling on the supply
of ERC20 NACKL: it trades freely on the EVM chain.

| ID | Form | Requirement |
|---|---|---|
| LIM-1 | Ubiquitous | The EVM bridge shall mint at most the daily limit of ERC20 NACKL within any 24 hours. |
| LIM-2 | Event-driven | When a proven payout is at most the delay threshold and fits the capacity, `withdrawByProof` shall mint it in the same transaction and emit `WithdrawalByProofExecuted` (`:1953`). |
| LIM-3 | State-driven | While a proven payout at most the delay threshold does not fit the capacity, the EVM bridge shall queue it, due at once, and emit `NacklPayoutQueued(nullifier, recipient, amount, readyAt)`. |
| LIM-4 | Event-driven | When the owner calls `setNacklMintLimits(dailyLimit, delayThreshold)`, the EVM bridge shall use the new values from the next mint on and emit `NacklMintLimitsSet`. What the window has already minted keeps counting. |
| LIM-5 | Unwanted | If a call would set the daily limit to zero, or below NACKL's `maxWithdrawal`, then it shall revert with `NacklLimitsInvalid`. Every payout `withdrawByProof` accepts must be able to fit some day. |
| LIM-6 | Ubiquitous | `nacklState()` shall return the daily limit, the delay threshold, the capacity at the current block, the number of queued payouts and the successor ERC20 NACKL was handed over to (zero before the handover). |
| DLY-1 | Event-driven | When a proven payout is above the delay threshold, the EVM bridge shall queue it, due 24 hours after the `withdrawByProof` that accepted it, and emit `NacklPayoutQueued`. |
| DLY-2 | Event-driven | When anyone calls `executeNacklPayout(nullifier)` for a queued payout that is due and fits the capacity, the EVM bridge shall mint it to the recipient recorded in the queue, remove it and emit `WithdrawalByProofExecuted` with the caller as relayer. |
| DLY-3 | Unwanted | If the payout is not queued, not due, or does not fit the capacity, then `executeNacklPayout` shall revert (`NacklPayoutNotQueued`, `NacklPayoutNotDue`, `NacklMintLimitReached`) and change nothing. |
| DLY-4 | State-driven | While the EVM bridge is paused, `executeNacklPayout` shall revert. Pausing is how a forged payout is stopped during its delay (bridge-audit#104). |
| DLY-5 | Ubiquitous | A queued payout shall not be cancelled and its recipient and amount shall not change: the lock on Acki Nacki has already happened. The only way it leaves the queue without a mint is HO-3. |
| DLY-6 | Ubiquitous | `nacklPayout(nullifier)` shall return the recipient, the amount and the due time of a queued payout. |

## 8. Minter handover (HO)

| ID | Form | Requirement |
|---|---|---|
| HO-1 | Event-driven | When the owner calls `migrateNacklMinter(successor, abandoned)` on a paused bridge whose successor names it as `predecessor` (`_requirePausedForSuccessor`, `:2248`), lists the same ERC20 NACKL under id 1 and has a `maxWithdrawal` for it no lower than this bridge's, the EVM bridge shall remove every payout in `abandoned` from the queue, transfer the ownership of ERC20 NACKL to `successor` and emit `NacklMinterMigrated(successor)`. |
| HO-2 | Unwanted | If, after removing `abandoned`, a payout is still queued, then `migrateNacklMinter` shall revert with `NacklQueueNotEmpty`. If an entry of `abandoned` is not queued, it shall revert with `NacklPayoutNotQueued`. |
| HO-3 | Event-driven | When `migrateNacklMinter` removes an abandoned payout, the EVM bridge shall emit `NacklPayoutAbandoned(nullifier, recipient, amount)`. This is for a payout the owner holds to be forged, found during its delay with the bridge paused; a genuine payout is executed before the handover (HO-6). |
| HO-4 | State-driven | While ERC20 NACKL has been handed over, the old bridge shall not mint it: `withdrawByProof` for id 1 and `executeNacklPayout` revert with `NacklMinterHandedOver`, and the token refuses a mint from any account but its owner. |
| HO-5 | Unwanted | If the token's owner is not `successor` after the transfer, then `migrateNacklMinter` shall revert. |
| HO-6 | Ubiquitous | *Owner procedure, not checked on-chain.* A withdrawal proof pays only through the EVM bridge it names (`:1866-1868`, bridge-audit#140), so the handover waits until the old bridge has paid every withdrawal bound to it. In order: (1) deploy the successor with `predecessor` set and `listNackl` it; (2) on Acki Nacki, point `setEvmBridge` for the chain at the successor, so new withdrawals name it; (3) let the relayer pay every `WithdrawalInitiated` that names the old bridge and execute every queued payout as it falls due; (4) pause the old bridge; (5) `migrateNacklMinter(successor, [])`. A non-empty `abandoned` list is for the incident case of HO-3 only. |

## 9. Cryptography and deployment (CRY, ENV)

| ID | Form | Requirement |
|---|---|---|
| CRY-1 | Ubiquitous | The circuits and their verification keys shall not change. `WithdrawalInitiated` and `Deposit` keep the layout the circuits and the receipt proof bind (`eccBridge.sol:64`, `AckiNackiBridge.sol:399-407`); NACKL differs only in the `tokenId` and `token` fields they already carry. |
| ENV-1 | Ubiquitous | One ERC20 NACKL shall be deployed on Arbitrum Sepolia (421614) with `script/DeployNacklToken.s.sol`, owner = the bridge, zero supply, and its address recorded in `CHANGELOG.md`. |
| ENV-2 | Ubiquitous | Every deploy script that deploys `AckiNackiBridge` shall link `NacklMint` (forge links it on `forge script --broadcast`). |

## 10. End-to-end and off-chain (E2E, OFF)

| ID | Form | Requirement |
|---|---|---|
| E2E-1 | Ubiquitous | Wherever the local-net e2e withdraws USDC, it shall also withdraw NACKL. |
| E2E-2 | Ubiquitous | The e2e shall deposit NACKL back after the NACKL withdrawal, of no more than that withdrawal locked, and check the balances on both sides: the AN recipient's NACKL, `locked[421614]`, the ERC20 NACKL supply and balances. |
| E2E-3 | Where the run is an e2e, the ERC20 NACKL shall be a test token with an initial supply (TOK-5). |
| OFF-1 | Ubiquitous | The end-user CLI shall withdraw and deposit NACKL as it does USDC, selected by the token. |
| OFF-2 | Event-driven | When a NACKL payout is queued, the AN→ETH relayer shall call `executeNacklPayout` for it once it is due, retrying while it does not fit the capacity. |
| OFF-3 | Ubiquitous | The deposit relayer shall finalize NACKL deposits as it does USDC deposits: the `Deposit` log is the same. |

## 11. Properties

| ID | Property |
|---|---|
| P-1 | For every chain `c`: the AN bridge's NACKL balance ≥ Σ `locked[c]`. Holds after every AN operation. |
| P-2 | ERC20 NACKL supply minted by the EVM bridge ≤ the sum of the amounts of the payouts it accepted with a valid proof; an accepted payout is minted at most once. |
| P-3 | Over any 24-hour interval, ERC20 NACKL minted by the EVM bridge ≤ the daily limit in force at the last mint of that interval. |
| P-4 | A payout above the delay threshold is minted no earlier than 24 hours after the `withdrawByProof` that accepted it. |
| P-5 | The EVM bridge's ERC20 NACKL balance never changes because of `deposit` or `withdrawByProof`. |

## 12. Traceability

| Requirement | Test |
|---|---|
| AN-1 … AN-14 | `scripts/test_an_bridge_limits.py`, the `test_nackl_*` cases, on `tvm-debugger` against the built `eccBridge.tvc`. |
| AN-15 | `crates/ackinacki-bridge/src/deposit/an_preflight.rs` unit tests. |
| TOK-1 … TOK-5 | `contracts/ethereum/test/NacklToken.t.sol`. |
| EVM-1 … EVM-8, LIM-*, DLY-*, HO-1 … HO-5 | `contracts/ethereum/test/NacklBridge.t.sol`, each test naming its requirement IDs in its NatSpec. |
| P-2 … P-5 | `contracts/ethereum/test/NacklBridgeInvariant.t.sol`. |
| P-1 | `test_nackl_balance_covers_locked_after_every_step` in `scripts/test_an_bridge_limits.py`. |
| CRY-1 | `.woodpecker/an-contracts.yaml` (event ABI checks) and `contracts/ethereum/verifiers/` left unchanged by this branch. |
| E2E-*, OFF-* | `tests/e2e/local-net/arbitrum/test_arbitrum_e2e.py`; the relayer and CLI unit tests. |
| HO-6, KEY-* | Procedure and findings; not checked by a test. |
