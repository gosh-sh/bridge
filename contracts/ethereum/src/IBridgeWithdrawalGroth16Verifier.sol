// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IBridgeWithdrawalGroth16Verifier
/// @notice Interface for the gnark-generated Groth16 verifier of Circuit 4
///         (`bridge-event-prove-circuit`, single-final-root layout) — the
///         partner-shipped 10 public-input ABI on branch
///         `circuit4-single-final-root`. Lands as `BridgeWithdrawalGroth16VerifierGenerated.sol`
///         once the partner's gnark wrapper is regenerated against the
///         v3 circuit.
///
/// @dev Public-input layout (10 BN254 Fr elements), byte-for-byte
///      matches `bridge_event_prove_circuit::PUB_*` slot indices in the
///      partner repo:
///      ```
///      [0]  tokenId       (uint32 packed BE from event body[54..58))
///      [1]  amount        (uint128)
///      [2]  recipientHi   (Fr; top 10 bytes of 20-byte EVM address, BE)
///      [3]  recipientLo   (Fr; bottom 10 bytes of 20-byte EVM address, BE)
///      [4]  dstChainId    (uint256; must equal `block.chainid` on Ethereum)
///      [5]  senderAccFr   (Fr; AN-side sender 256-bit account id, std_addr$10)
///      [6]  dappFr        (Fr; bridge dApp identifier on AN side)
///      [7]  accFr         (Fr; bridge account identifier on AN side)
///      [8]  nullifier     (Fr; Poseidon(block_id_fr, tokenId, amount,
///                          recipientHi, recipientLo, senderAccFr))
///      [9]  finalRoot     (Fr; anchor root the proof binds to via the
///                          dense-chain extension; off-circuit-checked
///                          against the bridge's set of known anchors)
///      ```
///
///      Note — `senderDappFr` from the legacy 110-PI layout (Phase B v2)
///      is **NOT** present in v3: the TVM `MsgAddrStd` carries no dApp-id
///      field (it lives in `ShardAccount` state). Binding it would require
///      a separate `ShardAccount`-state Merkle proof and is out of scope
///      for v3 (see partner circuit doc comment, lines 27–38).
interface IBridgeWithdrawalGroth16Verifier {
    function verifyProof(uint256[8] calldata proof, uint256[10] calldata input) external view;
}
