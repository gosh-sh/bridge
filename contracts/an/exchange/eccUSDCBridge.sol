pragma gosh-solidity >=0.80;
pragma AbiHeader expire;
pragma AbiHeader pubkey;

import "./modifiers/modifiers.sol";
import "./modifiers/replayprotection.sol";
import "./DepositVoucher.sol";
import "./EthBeaconLightClient.sol";
import "./ArbitrumBlockVerifierDeploy.sol";
import "../token/interface/ISubscriber.sol";

interface IShellAccumulator {
    function buyShellFor(address buyer) external;
}

/// @title eccUSDCBridge
/// @notice The name covers three distinct flows that share storage / owner key
///         but are otherwise independent:
///
///         1. TIP-3 USDC → ECC[3] stripe-mint gateway
///            `onTransferReceived` (ISubscriber callback) — fires when the
///            bridge's TIP-3 USDC TokenWallet receives a transfer. Mints
///            equivalent ECC[3] USDC and forwards to the original depositor.
///            One-way: TIP-3 → ECC. Counter: `_totalMinted`.
///
///         2. Owner-mint admin path
///            `mintAndSend` / `mintAndSendAccumulator` / `mintAndSendBridged`
///            — owner-key (pubkey) mints ECC[3] USDC and dispatches to
///            recipient / Accumulator buyShellFor. Nonce-protected
///            (`_mintNonce`, `_mintAccumulatorNonce`) against off-chain
///            replay of signed mint requests. `mintAndSend` and
///            `mintAndSendAccumulator` increment `_totalMinted` only.
///            `mintAndSendBridged` matches `confirmDeposit`: it increments
///            `_totalMintedBridgeByToken[USDC_ECC_ID]` and is the recovery
///            mint after a voucher-consumed `confirmDeposit` abort (CLI
///            exit 37). Do not use it for ordinary owner mint.
///
///         3. Cross-chain bridge — USDC-only (tokenId == USDC_ECC_ID)
///            `initiateWithdrawal` (burn ECC + emit `WithdrawalInitiated`) and
///            `finalizeDeposit` / `confirmDeposit` (verify proof, deploy
///            deterministic `DepositVoucher` for anti-replay, mint ECC).
///            The destination chain of a withdrawal is opaque; the SOURCE of a
///            deposit is proof-bound (chainId + emitting contract) and gated by
///            the owner-managed `_trustedL1Bridge` allowlist.
///            Counters: per-tokenId `_totalMintedBridgeByToken` /
///            `_totalBurnedBridgeByToken`. `initiateWithdrawal` refuses a
///            burn that would take burned above minted for that token
///            (`ERR_BURN_EXCEEDS_BRIDGED`). Owner-mint (`mintAndSend`,
///            `mintAndSendAccumulator`) and TIP-3 mint do not increment
///            the bridged counters; `mintAndSendBridged` does.
///
///         Deployed at fixed address in zerostate.
contract eccUSDCBridge is eccUSDCBridgeModifiers, BridgeReplayProtection, ISubscriber {
    string constant version = "1.14.0";

    uint128 constant MAX_TRANSFER_AMOUNT = 50_000_000;
    uint128 constant MAX_BRIDGE_TVL = 10_000_000_000;

    event UsdcMigrated(address from, uint128 value);
    event UsdcMinted(address recipient, uint128 value);
    event WithdrawalInitiated(
        uint256 dstChainId,
        bytes recipient,
        uint128 amount,
        uint32 tokenId,
        address sender
    );
    event WithdrawalRefused(
        uint256 dstChainId,
        bytes recipient,
        uint128 amount,
        uint32 tokenId,
        address sender,
        uint16 reason
    );
    event DepositFinalized(
        uint256 depositId,
        uint256 contractAddr,
        uint256 dappId,
        uint256 chainId,
        uint128 amount,
        uint256 anAccount
    );

    /// @notice Deposit fields read out of the proven public-inputs blob.
    struct DepositPI {
        uint256 depositId;     // fr[0] — anti-replay anchor (per source chain/contract/dapp)
        uint128 amount;        // fr[2]
        uint256 contractAddr;  // fr[3] — L1 bridge contract that emitted the event
        uint256 chainId;       // fr[4] — source chain id (typed-tx field 0, proof-bound)
        uint256 dappId;        // pinned to 0 — see _parsePublicInputs
        uint256 anAccount;     // fr[7]<<128 | fr[8] — AN recipient (256-bit, proof-bound)
    }

    uint256 _ownerPubkey;

    // TokenWallet address for TIP-3 USDC bridge (one-way: TIP-3 -> ECC[3])
    address _usdcWallet;

    // Total ECC[3] USDC minted by the stripe (TIP-3) bridge
    uint128 _totalMinted;

    // Nonces for double-spend protection
    uint64 _mintNonce;
    uint64 _mintAccumulatorNonce;

    // Cross-chain bridge accounting per tokenId (independent of stripe
    // `_totalMinted`). `initiateWithdrawal` keeps burned <= minted for that
    // token. ECC[3] is fungible, so this is an inventory cap, not a
    // provenance fence: owner/TIP-3 ECC can still consume deposit headroom.
    mapping(uint32 => uint128) _totalMintedBridgeByToken;
    mapping(uint32 => uint128) _totalBurnedBridgeByToken;
    mapping(uint256 => uint128) _outstandingByChain;

    // Code of DepositVoucher contract — deployed per inbound deposit for replay protection
    TvmCell _depositVoucherCode;

    // Source-chain allowlist: L1 chainId -> SET of bridge contracts on that
    // chain whose deposit events this bridge accepts. A deposit passes if its
    // proof-bound (chainId, contractAddr) hits any present entry — so an L1
    // bridge rotation can keep the old and the new address trusted at once for
    // a migration window, then drop the old one. An absent entry (false) means
    // that address is not accepted. Owner-managed via `setTrustedL1Bridge`;
    // deliberately NOT carried through `onCodeUpgrade` — after a code upgrade
    // the bridge accepts no deposits until the owner re-seeds it (fail-closed).
    mapping(uint256 => mapping(uint256 => bool)) _trustedL1Bridge;

    // Canonicality anchor: source chain id -> L1 block hash -> admitted. The
    // deposit proof binds its event to a block, but says nothing about that
    // block sitting on the canonical chain — a privately mined block carrying a
    // fabricated deposit event proves just as well. This mapping is that
    // assertion, made from outside the proof.
    //
    // Fail-closed: an unset entry rejects the deposit. Like `_trustedL1Bridge`
    // this is NOT threaded through the `onCodeUpgrade` migration cell, so after
    // a code upgrade the anchors start empty and have to be re-established.
    mapping(uint256 => mapping(uint256 => bool)) _acceptedBlockHash;

    // Code of the beacon light client (`EthBeaconLightClient`), the only
    // non-owner writer of `_acceptedBlockHash`. The bridge is the sole account
    // that may deploy it, and its address follows from this code, so there is
    // no address to set and none to spoof. Empty = no light client yet; like
    // `_trustedL1Bridge` it is not carried through `onCodeUpgrade`.
    TvmCell _lightClientCode;

    // Light client per source-chain id, derived from `_lightClientCode`
    // salted with that id, never set directly. One code, one client per
    // chain, each at its own address.
    mapping(uint256 => address) _lightClients;

    // Chain id of the light client deployed last, written by every
    // `deployLightClient`. 0 until the first one. Only the no-argument
    // `disableOwnerAnchors` and `getAnchorConfig` read it; every other path
    // takes the chain id from its caller. Not carried through
    // `onCodeUpgrade`.
    uint256 _lightClientL1ChainId;

    // Owner writer of `_acceptedBlockHash`, keyed by source-chain id.
    // Missing = owner may still write. The bit is set per chain, for any
    // chain that has a deployed light client. Like `_trustedL1Bridge` it is
    // NOT carried through `onCodeUpgrade`.
    mapping(uint256 => bool) _ownerAnchorsDisabled;

    // Code every Arbitrum block verifier (`ArbitrumBlockVerifier`) is deployed
    // from, the second non-owner writer of `_acceptedBlockHash`. Like the
    // light client, a verifier's address follows from this code salted with
    // its chain id, so there is no address to set. Not carried through
    // `onCodeUpgrade`.
    TvmCell _l2VerifierCode;

    // Verifier per Arbitrum chain id, and the chain whose accepted blocks it
    // reads the assertions from. Set only by `deployL2Verifier`. Not carried
    // through `onCodeUpgrade`.
    mapping(uint256 => address) _l2Verifiers;
    mapping(uint256 => uint256) _l2VerifierParent;

    // Owner-operated stop of the cross-chain lane: `finalizeDeposit` and
    // `initiateWithdrawal` refuse while it is set. Like `_trustedL1Bridge` and
    // `_acceptedBlockHash` it is NOT carried through `onCodeUpgrade`, so an upgraded bridge starts
    // unpaused — with an empty allowlist and empty anchors, which hold deposits
    // anyway until the owner re-seeds them.
    bool _paused;

    // ZK verifying key (VkBlob) for the ETH-deposit circuit (12 public
    // inputs, chainId at instance index 4): type 1 and 2 enclosing
    // transactions, 2048 B calldata, 2048 B per receipt log, keccak 128.
    // Keyed on the Hermez Perpetual Powers of Tau SRS (s_g2 = 928fafb3…).
    // Canonical fixture, regenerated with deposit-prover's
    // `export_deposit_proof_set`:
    //   deposit-prover/fixtures/deposit_10proofs/deposit_vk_blob.bin
    // Magic "VKBLOB\x00\x00" + version 2, shape "Rlc". 13071 bytes;
    // sha256 = 0180ee5af41ba3f408d38bf0c10c005c6618ad379d38da97a7ccc07ab1c3efa1.
    // To rotate: regenerate the fixture above, then rewrite the constant
    // below and the sha256 above with
    //   scripts/embed_deposit_vk_blob.py contracts/an/exchange/eccUSDCBridge.sol
    // — never by hand; CI runs the same script with --check. The fixtures
    // under tests/exchange/fixtures follow the new blob.
    bytes constant VK_BLOB =
        hex"564b424c4f4200000200010000000000ed0000007b22726c63223a7b2262617365223a7b226b223a31382c226e756d5f6164766963655f7065725f70"
        hex"68617365223a5b34362c34345d2c226e756d5f6669786564223a312c226e756d5f6c6f6f6b75705f6164766963655f7065725f7068617365223a5b31"
        hex"2c312c305d2c226c6f6f6b75705f62697473223a382c226e756d5f696e7374616e63655f636f6c756d6e73223a317d2c226e756d5f726c635f636f6c"
        hex"756d6e73223a357d2c226b656363616b223a7b22636f6d705f6c6f616465725f706172616d73223a7b226d61785f686569676874223a302c22736861"
        hex"72645f63617073223a5b3132385d7d7d7d0a3200000212000000006300000058c338696a199ecb26464c9bdedd6f46e6fa0c9974d91b9fcaa627c329"
        hex"dff21a4e7b0e1f40caf730f9cb23b5583598194673b2bc3c7f181c20266ff8b4d3cf262ddd9439fbc7e89d57c1cfca2502cb77422607cff07811a479"
        hex"72f3d9f6184c0c328477f31576fb9e36bf4efd175e1d5dc90f4e341f3d77ed161f612613f0c01f3e95f55881ae181828bca4d5c448b310809dff7892"
        hex"cad5cd580bd89380500e3084f208a3f9e148900d13303b0463f219ab0b1c8edc9cc1ea7efd1ce13892a40d8175537a1dc6dfc62cbc14ec73a57991d0"
        hex"51b3e8e7008002f8469672c51d7e1ef9c6dd3592c2f442d338f77c0fcf2fbf435f14d811e778d32418d496eb6e4c18978617446daacc32dacd89051d"
        hex"450be6b1381aad97b4ec7bf6429f8ede02890abd7775ef94a1f1803d91720760f39e7a1c0741fa59af0da8056dcc75d5f26a171750e695bf399381ff"
        hex"d9bb1c217da936a24274ef66e395a0d5b14bf3bae0e21aace9ea20dc7ba3bd83428ee1f94bac52776f74963306ca57aa9f35d626e9591d08ef76f71e"
        hex"290f9851d342d6544362c9e81a8e54e6149c7457f10c6110ddb92a43c104c7a08e2348e52461a4258f00051787959c12983cdddbfe4a9d8f60be060b"
        hex"c1448b3d0f3c2b065c87654b4ccfc98c6299b90acb86a7e3f5fd8a4d8cd0177e3b12e7fb3577994373cb2094d402392d70c15ad7914442534f89c7f1"
        hex"07b42bf221d15bfc5a929538b9c31ef70f963e11d91f02e7516f12200100fe84e59f09aed6301a4893380933666b000d3152d3cb8048528c5fcb6fc7"
        hex"06028ba6e5db1608516aca3730512a7c3d4648e8053bb6572fa59ec9aa967b608a140893df9e29ae794cf7c9cdc4d3cb0a1fad061ac92d64eabb9cd2"
        hex"a0f3809d7ff322569a3e0ffbf8d7e5d8721c6df176f43efc5d26cd43273feefb1cbb1f1ce371a04592bd2fea0a4c081d9e8f2b0c8d217f4347cc7276"
        hex"b481071a742312ca0d8739f38a3b05c11cacaa94df4212629e8987d6865174e9d79f779451748385e83b1546d7310fef3d5573d007e0eb89726c81fc"
        hex"10c7f9598c91d458545fc7b4fcfccb3e83bf2a983c910153fb861095216c8581742d360343a4369294de9bfe9b3dfefbe05821072e19499fed491d7d"
        hex"84261ffac9a40273033a83d14278cd2be543f40f6e8c07d8f5b967aa3bbd63c9e6ae4febe4578e1046afcd65504bf0ca42130b98ef232a83133ed0c1"
        hex"75ca67259ccb421df93afbb58cdc466f42e9e56b25f36ce04b412e295f12577637aa9d3a53937158e418017e2ad203a6460bc232aeba282569391517"
        hex"dbad7f9bf85bb054404cacf673ac20397f2a10ed835bbd5ae0e357673b61088909c4b254b65ceac56af18efd8cb8c30d75c17f8a75c7d3630357ee85"
        hex"c8702e571c3ea144b62b4c5b1402ca17ce4a3f8dc44c71a144e38048ea688d928ce81db72d8c3b081cfb67e378c062d5a2696a767659527ac7cf5509"
        hex"704d69c9ebec0c67c6f5a7e88d6d7de62423d780373c263abd2b24a73e0eb61c966abb12690b14c11cacaa94df4212629e8987d6865174e9d79f7794"
        hex"51748385e83b1546d7310fef3d5573d007e0eb89726c81fc10c7f9598c91d458545fc7b4fcfccb3e83bf2a4e37a3c904eda3fdc7213ebc7105913dab"
        hex"883ab6befeb1a5577d164e84faf514bfa0424f0e6f2c4c5c7d06ac66a646d3357f2f15ca797db8f43c6adb717b8f0120012ff0a1ac465bf2a834b0c7"
        hex"cdbbb3152c29fdc667a151ed424c68a4c13a292e93fc1e3c33b9996833062cbb45c1f81eb1cc20d6ebe85ec84d19468e9cec0ee94c74cb679a406a0d"
        hex"6b7ffd8a6b12e1c1c265f75debbe5b3b3a71ce15bc24203ddf72dd48e5fa119c668ea2558c5b48b729160d0ee555ed65039d8dcf8b1c04f3dee361b3"
        hex"cbdb539ce33ab9223735c179f618ca15e886067e0952c8a6de000ab20fa6245e389d7cf6fa10a2f9e844c5a359584fca0d2a4ad1b1aff36b84471a46"
        hex"be61a6dae2483c4f287435cedd727124e08b3ccee661bc5cfc59796c045c049f43522af36ccef4ec44cb9a571eb67ebefb5dafb3604898d7c68bd0d5"
        hex"167e09aa5d7d9a724bf0fb63dc58194a98284edb2054e436afc98734131060fe8973183c8d1b940d8f21f09a56dc46d269eff268ac0cd0fa5d5a4f0e"
        hex"b163811a38af09bdc863c4777aaee2f00fa833f991d1b708708e367e0eea027a2e1de9e89ab72a1b7ef6e8c44e5593b6af236121a9bf6950394b0b2c"
        hex"164efae65fab6abafec803a064cec5a9fc6d614d596451d1f0862c57703086e09dbe64be845dc2e46a9d2902e03ec04a4da517142fe46dd555bd09ff"
        hex"e00b58b1fc2e592110bfbca09c8702db211af84f1c1b0b7f656acf5efa74c6c1843b5b2299b64237ce5df5c7732018878adcc2559200fa3980cb850e"
        hex"bf098a924c2329ce0294cf308d1a8206ac86035bebf03c607c672aefc0046ad4960ae03aa9404825078d69992bff80a2965407e128a1125085bbef40"
        hex"ad2b016eb90da6e61e27d0b8a81b279417882225b62f0bc11cacaa94df4212629e8987d6865174e9d79f779451748385e83b1546d7310fef3d5573d0"
        hex"07e0eb89726c81fc10c7f9598c91d458545fc7b4fcfccb3e83bf2a35ed719e5d93d7632108acb529d33fc30d88b81bfb13adc8af285d0eda98141c1f"
        hex"8ec22ecb061b5711ffb432edb044f6fd4b1a6b19ca8b12fc854f5d9f52fd1202939a05a55def0cce9ed23aebd0b4402cda6623fddd2cec1b139cf992"
        hex"258a0347437f338e5c3f6856d6e4efe89c28ae0fcc8347e804b92bb7febd884db4c103e670de1b657d2d9b116d268f1ecbf30b1f8f618c254ed636ae"
        hex"3cf0e2b1f8112e30090d98a7f77d46ae3779a63466ecdf93bb6bd52ebf9399929dab96007d113025b2f953f0470acc32e12416aa461718fd67c5ca0d"
        hex"c8a00c39b55568e0723204d2f790561a29223636098b63fcc3faf4b43aa06ef912b564a50ba9a69d172a10983c910153fb861095216c8581742d3603"
        hex"43a4369294de9bfe9b3dfefbe05821072e19499fed491d7d84261ffac9a40273033a83d14278cd2be543f40f6e8c071538c8add48eb1cdc67dcac6d8"
        hex"1242dba1a9cf18ceaae4ead4be578c04ddd213e75f58e246197b2a79ffab97cc66377b0f7b3ce9ea542f370f98e3f38746471bc11cacaa94df421262"
        hex"9e8987d6865174e9d79f779451748385e83b1546d7310fef3d5573d007e0eb89726c81fc10c7f9598c91d458545fc7b4fcfccb3e83bf2a002c8c8f79"
        hex"d15a1e2056edc050aedcee879c2ff4565ad8c0118098f0b0a38109ca8e60028cd95b29698e01e8fc88e2e353914f61d04306063a22e5bd03b5352298"
        hex"3c910153fb861095216c8581742d360343a4369294de9bfe9b3dfefbe05821072e19499fed491d7d84261ffac9a40273033a83d14278cd2be543f40f"
        hex"6e8c0718987414fbe33d47e760028b0c0b44ef8f15c2f49a33ddea56b40bcd0880eb1a498c9ce231c1b65ccdefa28b22a4dcf95ef39e56d204db67fa"
        hex"fbf5f8adc71508c11cacaa94df4212629e8987d6865174e9d79f779451748385e83b1546d7310fef3d5573d007e0eb89726c81fc10c7f9598c91d458"
        hex"545fc7b4fcfccb3e83bf2a07e10019c8abc76945c3a4be7227cd2b7140358b47672e5dabfc7973b4bee2171db8b073c88807a5848099286ce7415775"
        hex"295961807e4151f252b24867ba5405d17ac433d5a6beb90cde611cd18287ee9db02f965c52f6dbee1f5b73981ec41d4d6b3cf78465383be9af46e1db"
        hex"c0ff3e5ed068e367417ddd112a0a1ab4b3882b983c910153fb861095216c8581742d360343a4369294de9bfe9b3dfefbe05821072e19499fed491d7d"
        hex"84261ffac9a40273033a83d14278cd2be543f40f6e8c07a433ec8ac09c843370d1140711ec9ba7b4e9b515d8dac736eb5f126b44288b18bc1efcc16f"
        hex"d22710a34fdef259a8de07567f1a922c6c19610a50980676d445069bdf1daeafef8c9ac952f6a740c8630b801223d72ae0aef52e43a690658d0815fe"
        hex"d07af1aa350816d7a65d09844185a5ed5c0d680f09df3a7a1b668352c6a91acef59ee5c1720ad064df57257d9d9063316f9156cf239d6e993c2eeb5d"
        hex"66b91f8eaa063b1dcb8263572e565a392762af7e10c1a75f60adcaab233e81d4577b02051aed3a59139e595f0fee4df313cb9e32a57d8cd3be419442"
        hex"c219636bbede02b7584519a3f203f288fecab4cd3589be5de2b8ce1b739668b15e0a39267337188a16a452596d540bcc309b374d6a1c879da69d3093"
        hex"0a763aaa7cd640741495170cd8977572f2eb7c9c4ab0ed27e774ee6599ee29d4bfb2c90cb63926aeab7e134070fade585255c8c481ae77e7f1d3cf95"
        hex"fa8b84842d2a1dd7e74ab627f620240a867967f5c37d58de35fe349547563c2e62422bdf337e9886d9fc3c20d9612b5be025f10381ca2c870ccc3813"
        hex"8d34b3458a4dd39bf2bdebb89a5ba6943639307a14f2386df5e65efaeea8d550bb3abdd45acc4ed5f2a396bc776f9b6c2dd107983c910153fb861095"
        hex"216c8581742d360343a4369294de9bfe9b3dfefbe05821072e19499fed491d7d84261ffac9a40273033a83d14278cd2be543f40f6e8c07983c910153"
        hex"fb861095216c8581742d360343a4369294de9bfe9b3dfefbe05821072e19499fed491d7d84261ffac9a40273033a83d14278cd2be543f40f6e8c0701"
        hex"38047bef994ac89b33d2d22877e299b4b953e117a595c963f014988a6df023137f3d82569ee78160eda14f861989cccb81b5bd09499e45547d589ba2"
        hex"15a8149b509443b73413c96d2b7a5d9d807bbd86ba854e80aec28a16584f431d803c0affec54111349a452f948aceb8299662094eec949d07d9447c0"
        hex"a0f8d92ff4dd00983c910153fb861095216c8581742d360343a4369294de9bfe9b3dfefbe05821072e19499fed491d7d84261ffac9a40273033a83d1"
        hex"4278cd2be543f40f6e8c07fb19015d036c23ac7ae5fbb8cd34b6e2ffabad433554b8659142e0c39509e91b1bb558765acd065d5fb38a9cf0608aabb2"
        hex"993508eb3d64a2544c808198080f159f3ba06493cc326974c38bd59fd377a4b59a656b47bacbb119edafcd3dc5a51c69397d0d67916779553e90df7e"
        hex"6a4f8ec34fcd6d3ccd2cd6b8186d0af5e830149ebeb63a47961ae3066df689bfe36be7d6e518350fe8282ed894983d2269400cb6334b415e2c3e1431"
        hex"a8e6f5dc991a13c8e683f547504f1fb2e796545015a32e6757b5e01caea92d4272d6de2743084c0ca1dd58c970aab90b5d40040eb3510eeccd0a8bd2"
        hex"58f0b4434515d819060cc516ed4ad48e30898ff81ac38b0daf55112afc615329dfa96fff86891aff23d0347efa221913483ecf7af0b20e38d2230048"
        hex"7978f8bc453e4bab72f0598bc8b272e3771eaa9fc329490df42b373018642e034a072e89336a9f5b1d1b4af60f12a9ccf23d50589a442f65c129180d"
        hex"7f7a19ef241bfd3e1409dd269513836ff92fb393fbc75d9a8c5488fc06b3b0478a2c15c11cacaa94df4212629e8987d6865174e9d79f779451748385"
        hex"e83b1546d7310fef3d5573d007e0eb89726c81fc10c7f9598c91d458545fc7b4fcfccb3e83bf2ae04a76e8dd84e68dcef1e8f113cbec5b4888130767"
        hex"9ffdfe50ab25e4be192d1e8a24b543e7953ec1b6e9dce05beba40edc98b11d629e91d5ca989d71abcbf620983c910153fb861095216c8581742d3603"
        hex"43a4369294de9bfe9b3dfefbe05821072e19499fed491d7d84261ffac9a40273033a83d14278cd2be543f40f6e8c07b5752a04fc390877cc3af482c0"
        hex"12f1e4d83baeceaac2271b0daf8e7ab315d417885312328bf031f448f7ad033d18df04b4bff1144afe9e853ef195cbf7fdb20cd1d843544d0c47e87f"
        hex"d7fe80d92ef9f6ceb70b1195236de49c468f7648057b2d1c275974ee5a0c19e0a078767a32c225f25c4315abce5fe8ffadd26cd36a870c983c910153"
        hex"fb861095216c8581742d360343a4369294de9bfe9b3dfefbe05821072e19499fed491d7d84261ffac9a40273033a83d14278cd2be543f40f6e8c0700"
        hex"3d40d6fb02ee7be6a05348ccc95d39db3d35a0489b98e841d5053414230f07d7c16bb624aeeaba3620d36cc00a6d73a08bb01f520a30da267ccafef1"
        hex"0b5726c11cacaa94df4212629e8987d6865174e9d79f779451748385e83b1546d7310fef3d5573d007e0eb89726c81fc10c7f9598c91d458545fc7b4"
        hex"fcfccb3e83bf2a7f1eda1b17e1e20ff2320dff64055140ee8874286deea55bafc8f508479189091e63bba5722f8546ae6c6fdbd30afee41739fa4296"
        hex"d0585cb1c610300fabb901971be5088a900a7838d5011db297222f50af2f3a77c96b44f26dce99cc4d3829c5a0f2e94e9c47ffd7c0be6e2580e9e242"
        hex"240fc8246a7b24eacc1d61d110d82945a9bc5fdc643928408c42e651abf5512a7eb7a5b559708b3a20f5fc5de3e42039dd20bca3afe2924d11c36d60"
        hex"5020409f0f42a1e8b49db4bddc70bc6d86fc03983c910153fb861095216c8581742d360343a4369294de9bfe9b3dfefbe05821072e19499fed491d7d"
        hex"84261ffac9a40273033a83d14278cd2be543f40f6e8c070cf2a246f42b2736603bc4c98fe44609b526fad1531ffbab8b3aca5e1a0c592b0727d2ae8e"
        hex"b37826ad9ce3a5055f450590a6ce9ac35fe91ab96eb946408366074d5d7e9b2262033373ecfd7e6be430ec499cb93359fef8597e51d3a5a4ad0929ba"
        hex"db6382abfb42471162d18417d038b7b4355a7a528db9d7711fd1c66802d326983c910153fb861095216c8581742d360343a4369294de9bfe9b3dfefb"
        hex"e05821072e19499fed491d7d84261ffac9a40273033a83d14278cd2be543f40f6e8c07670e4b983d3df112301ea1f763ab3f8495e57b8e01f74ca65d"
        hex"c7838e248c970ac466810bf43bfa1801d485737f91b5d75dcda9fe934b422e710d5470b76068219f7b0a3dd8eee518db3b5850bd260eb2680ef8f92f"
        hex"1ac0702dbe6af88b23c41f96d69d05200269d4d098def854586b33af66abe0db14761098cd2c3bad2b0b23983c910153fb861095216c8581742d3603"
        hex"43a4369294de9bfe9b3dfefbe05821072e19499fed491d7d84261ffac9a40273033a83d14278cd2be543f40f6e8c07dc0e06b1bc6e5d34030232e984"
        hex"bb125bb1b10db0beb33f97caa5b11452bfb61387b0ef2e1bd65d4b0a18c03a832ae2799a55f6b1830569bae3a4e4859b48bb2f983c910153fb861095"
        hex"216c8581742d360343a4369294de9bfe9b3dfefbe05821072e19499fed491d7d84261ffac9a40273033a83d14278cd2be543f40f6e8c07885a7b73e3"
        hex"3074cfd8b08b2acc3c5c76602e3b070cdb193ad60ee36fced06224f0114f6e13aa9ad1af81fc4cca4e9d4cb2dc18bffd52e3af2ceb0d6c2b3cde13ce"
        hex"aca0a9709ae8e7442b72dd2b98f62dffac4859019dd69ebabe92088bbe8414260f5eea9c6c305e88aac8e36ae011b2ae6fe23dc20c16ac2ce65d8b38"
        hex"8efd1a5c0b5d4b96634d8c620065c3ee69b88420a428777923273f79128a67cfbfe917aff0a38e9cad5334faafda013a035d977e82df9836c7dc6bdd"
        hex"ec1b767441a50c02549e6a6c2bbb48821d06b0492955306d21bf6f7f46e83f671fb987cdda51133bf92bbffb7cd6b508febaba84d2d837d5c5d592f4"
        hex"e4de19cf352beaaa604a20968cead5881fb4bbedbb7b2b9792662a770c9b88e548315a986747edd2f9e51164cec81e15f87deac342a05d045bd4666a"
        hex"805f718604b8832af2517fa8f5970304cf4d9c0c46a426d668e379d1fc7652a5327695321b0d2f931e5c68ddf9b724d8322879e8d514a35358ca52e6"
        hex"60ff8040abb7f121fb777d01c06c8a09165700adfe83c76d14b2e8854952ac52cf9dd4aa450ea9b9d657f383f51054eef9790e23aae48d8e5efc4f95"
        hex"8dae1f9ca7809cc844d93890cc1b830157daf80214311d983c910153fb861095216c8581742d360343a4369294de9bfe9b3dfefbe05821072e19499f"
        hex"ed491d7d84261ffac9a40273033a83d14278cd2be543f40f6e8c0783abe0055144df7d8756d7453de61d8ac8ac89118f0b65684dcf7adb970f8925a8"
        hex"8b13047c959483a42bf42773c145af3b2560ad3a59fafb06b2436a299d8005ee2b537d356a9a7fde0a98c3898f839c18f596249ae78e4711a78069e8"
        hex"61ae027c21fbbb7cf089485974df19440fc85eeed0ce89c85ff4d1de5a3a2e8bab1914bd465b5daa8814d9dac1d849034b4963492ed0959e06811c1d"
        hex"77530d35e30e1b3d92719ecfed99a60e0409a768270a27a77936b19249ff52c9ad47b72855900dea38d68353dc4f0bbc88d27496312e1381962af702"
        hex"f079bb0468f1f628c5e72227b45e9baafbdad7e47552f1cb2f3fbc5f7a65c112311710940794f8e48c4a23da3f0c3b24c1d6ac27d4113944e7365beb"
        hex"de52dedbdeef4a05e826bc1886ff19d145d38314eace47377b514427185de0f1595909c06c9511e92db0bae350751f19c9bffae66ab757783261a72b"
        hex"ee496613ce11bd33acde9a00fc649ca9e8120e54a4c280b68062953be8357771215a1ee257286bf60f23cc963ee40feeb4552a2f97c7f77bccfb41b1"
        hex"1377ca4ecc290f0d318c4ec7926530168add94603f0b0caef62ffbb14bac7876f835cc98a5afe7310125dc961bb3c5412c7d00ef19191fdb5812f977"
        hex"f08666c3a71f02bd19460c1c7c3ba4e3b086b2bae3f8667861e621ad78dec8f77c8d5d90de8af113f652766e170af9c064abc9884c94a749e3ce2e59"
        hex"22d00a7c29dac6456000e7f9e7bd3a170e8869a951cce817f8543a7e289714c64c52befff5c7c3a536cecd7919508c0f972e9805f0b149289ccf3ad8"
        hex"bb6114d31e29efd3a7e2a0a5d4280fa122df22cc658f0be9f8377bd2710a020a08b329d7482da8182e039f960fb00432c97839b5aa4ac146655cfce1"
        hex"28eb06b783871de66b196f555eef7404fae9bee37681e50b5d4561310e0af56a55421c30f0961ce209af5d81cdb4e66e9f2b0e35e3d83cb27faf3c59"
        hex"dd56342fd53e1261a7b10f0abe06b740f731b579fccc903632faf7f8931ff2f582cca23533172e3a124607eb8dc9da549f8eac94550d8c7bb3d96111"
        hex"8b9d2cf51101bf9a4a8532ec87ee27d4623d45171d711e90c3c2511e0c035e3fb9c58cef960e6e509dcc18d3f06c25a061407752ca6df979dd80fd1d"
        hex"46fc9617a9f3c965440ecf1e254f81ad66702a281f1bb7e64b22ac8f50e38784ff19c32a10e05ea8a58ffa297415cb3c9b720d12c1fc75c1f55aefc8"
        hex"836c93c22ea32eff0e902d7ebc52bfa30ece20b4260816d029cb5d84d0aaa3fc735bcd743ee2af3e1aba03feceeed928c1c71ac14ec02fea416c9591"
        hex"026099029b9a942736fd83d108358b00a718193ca83b767a4058271d2a69a1601c197de486dca412aa4f935f697bf09e4e52a4205efa591034c20cf9"
        hex"be6a5fa397c85103f66377376f5ed488303d7649cefe49b68f6d9d6914e922e2a0ea13f8acd7c3a02df4ccfc89d7ea260ed0e706eee819f7166d7b85"
        hex"c4602e479808271e10ffb881b49801cc41c85b2a05b073ee8352166c65cf46d31efe1138c2710da7c5e670b29275566500638029bbf7a1097cda6572"
        hex"d2131551d6f61fc06da01a1150988cd000454f40c18dffeb06ec8e424bb3c9e32d02b7a4d58629076d3b73308444214bd6c0f04d52586d15a438607c"
        hex"c4457d6fbd886fcc3cbb2c6d12d90c5fde8fc55c277abd3ec5c921d65508990ec4a820939cfcdd028c40246cfbf5d04d5c4116259993b92dfa492c35"
        hex"9b105f25b8f64359fba41941eb541e976b4a33decca5ea60351f0fa5e91bc76036feaf7466c39ee76076541426402746d234868875a13f37ca016e8f"
        hex"a5e0e3a25e7743b8f90eb07cb57cd3842547271bda27e0dc5e6099216ecd59eb8bffc52352c5477784f441575aa08f4cc32426aa4a1d8417bf0bc281"
        hex"0037627262c95d2f857391d1645b40565ae814c941312c6f78b7d83bb399427c9dff99ca12d29c5facd2f2f037230aba667a96a315551ab2fce365e6"
        hex"f60c870cc9c9ffcc2a7d3a102c4ac5be0831de3a51ca886bd7dd28c9d9ecbe883dcd1a619dbfde90d2148061e7c22188805e62a7ab3f8defbff41f38"
        hex"7fd2d9c27198ae96414e9f01f01f447a71c02a67468cf38f30fbbb8865f41195f3920992dc316088c9bfadfae5929847f6bb44720defc700f62bfd06"
        hex"a8511d8baa797c320bff8ec705aef3219a7b34778c636cdfd41618f111379cb64d4c0e783b6e8964173259bc652b6c6ab30d53094eb03ab6c01f7185"
        hex"252f1e5311740e704a4ef42b3b16cdfbc770a3dd4b01ef5613b949a38b9cfeb66e92b3e921f921f74dab4dbcc29d2b0c7f6936ed63bc3100db000121"
        hex"b9bc406f24c419e96456016b6d5fcf9b5ac2e06186a4d52d0d9ab369c6b0b3951d705a9151bd6899120e03b9a39061bd4974de977170a48ad527d434"
        hex"8dcd437b72b4a8cafda2a370b34e138e8ff207cf1e9303b8eeec4956dba4d83baec46413153c8e327ff8a57b87671df175754f0701c3eecc41d8b6aa"
        hex"fbcff6e821cd48ba7cf8acfd93575799efe12bab3c04775649dc35f0d32c474cdc34c8c128e77366fa5a1b75f4053c7620de20a90b51148bade2930f"
        hex"0d34c53d8e4c3078d73296889bedd2a4cb024792e3001ee6573a01133a25d73b1801c418af8578301e01d95f9e37c50b8b6389d29444040b247e2c18"
        hex"c6ec282c091ad56b785bd83591a0b24a4fa54d67a86519ceb45e1b744c2758dc0ce38becac57f5fe8f90f35567943422981610be94c1db716c110b45"
        hex"761ff2b85d211f0bda7637ddace02b8937ce81e19878f1248c4ac831679702a793ef730da60105b65309b25c8ca3954498f35ec000773376f0031d85"
        hex"4d581204e3c980fd1d3371d31117be9aa4049ca2f92466501fc6f876d7a25f7ab47817f0531ecd93a4e3ee23a9847203d90915193a413b8efcf7d22f"
        hex"40b98ce86834299b0dd7f6b17cb6df8d857c881d0424d3c1a296d5dae56282b3c4acc24b9ada12516815d035df565741c6b68efe6606bfc4481e5723"
        hex"2a72e321bba908d97f900305d8529a4bce468309d4c5d3a3a4bf030d89aa210821c3b21e8dac27a8869e1881c9ff1e8a2ee6c0a22f29c646c47dc9e3"
        hex"c308bd013ec641d480006edc62e612ae51a912604fb5da7634b8263585f8d9dec324e9e4ae7de66ee3add49957b7224fb58a4a337c5c3979e7fce8a7"
        hex"ff4f531473cdd73d0fe218a3f625748b234f14524048e0742d1c572bf32717b7ea90d6e8271d6fb8d5b888ce29285c0134d31857fe0640c4bd484928"
        hex"3683ba92b9ae925ffbfa260de35e36827bb626ab50bd213039cf3180a93fbea29dc7d359780d9e8eae0f97c69c1872b7bc504da90c2e267b75eba699"
        hex"83d747d68f7a16e2fc0785442357aae1b30d724cf0a272e92dae12cb353c4ca6a0d3efe4ad3cb38b87554248e3c16b3a342bfb7813ad70fa2ffc2c1b"
        hex"1ff6eb16ddcc96106085b8910876b5681b56201208a7821385e5ee7ff90c038bbcbd975ee9c59e20717e0babbd3d30a9dc3e03b315a1f7007641723d"
        hex"3ad30e268464121074d0bedd60681c71d17eae52e9d2e4604e9722930934d83b3aba23021cc265388ff5dfd260beb387d27bdb7708b1721308060da9"
        hex"121fa508192923e8022efe73258146b543ffac5f2fa58864658d822a289121ff33a309ee949e1d9550fe5110488c5a915291e5d1e601cbd95b830493"
        hex"90cb3bd765522d62895b18c38500fe5d775215a17c9287d7c15f60bd52a0ae93dfaadbbf66c966813e500e4d8b5fb8523daa1697eb0242cb2ec8bf09"
        hex"6ee1b39674207f0e3079010298dc1826b1010c1193c7e364b446e8a4f5434be3ea60dfe977a37d98a7b52c9d4e4f2d54a0c7d78ed818facf9826dde3"
        hex"bd55e099b7d76498f9395bd6d38caeb5d1a22f4dfb40a88931aef27bbf7953cd92e1f3bd99077b9cfb0103d5faf253f0aa7a1a7e9883d763dfc56445"
        hex"fb5dc081ef6458c60900677597f4dc89a1bd483061022bd28af613222e98a97a3f39473531389cc04f52f4090b249da8ed72e994110714b1356e47c9"
        hex"c4bddba8ca91ccb1a45dec735fd43a38d3e0fe647b7fca59ed2924fff6e53d78fda433c4d41ddc29099804e7b41a8f06e3edee51e8dc28c02d7d19e4"
        hex"7ff8d16203567c2c45b055f10834c475da504cc7c765005e44edf13a616a2390e7c2ac5e12586247b172371264ca8c6f9eb32ff8501a7580f4dda61e"
        hex"3a7c2f62b0206bbafee5242d4da0f1e9159a430494081bc91f246f73d0c1fcc83a6e22659a37af05ee1155afe0faa2e93f01cbdac66178356908a338"
        hex"2c1cb84450c329387ccabd4b438bd5f14d47fb552f5161af7ce44ccac8370c32914ebb500e961d6a034aa3ba0bb407ca4141813c560ba450b7677bea"
        hex"c937567a4e0bc38864f127b8a625002a7545f9d52a5e4c3f793faeddf3d7bbef60e25e38fcc9986aa6c701a01f93edf508914a41df954482110521e8"
        hex"f9ce26a048ad53b1798202a41ddf1c55116fc79d38814b6a130657ac956febd1c1314513b1dfec1296fafe833b75049726f717a6c8a85cea8ffb9b9b"
        hex"c0609d2207d167f8a7256aa0e5e6f1509c8115c790c745f609da4e2f530a91b2dd7313588d01e6ed049261d9822e4b3a993e2c2ead8df6db5a9e5cc7"
        hex"e2299ccdb28e98f67e2f0c0cb3c14802f49f484c7a76077881f00b8d8242ac305426db16e6ddc007fa2f354bd4245f90064e7c1fe9e627dde36b2feb"
        hex"ba8e3b26fcc98cb7da744a58bf4812f97147ef2fd91326db33211e290ae71096724237558a869a554e85481aeae4e4e761668a92cfd3c3d7b1d604d8"
        hex"eed396409cea570ce176fbd4375d58072ef64cc56e13b46bf4f26a90a6f92724d9fb9b9cc3b440d6f1a145f8cc75f152caf45bc31788c2110dc46ff8"
        hex"4d4c1769aa78d729cba3c4dba49bffbc23914fc10165b2af51a30132371e15051b120e9055c36e5dc14dc076dc58a84de7f66e7d9aeb1663dddb92db"
        hex"3f80c3ce1bdb0722f7def545ad07c1097d7131f5d52ca54e8363877d2acbf12998e5ac1314241d896775bb506426a0f1ca42a7ac370f658daf51fb61"
        hex"c6d024170a488fde8e2a30dd07f27b61acf881ee1754315448da55089e5d4626a5e29ae5c6af57504cf11da09733da8d159c2108a39c108364ddf6f3"
        hex"3e78bb7396a5c8c409d5cebfa7d00d2918172fe471773aea7ceae20f7c8c2caa27e0c50f2e935ffbfbff350bd7ce04ae4dd4f35e60ae6ee9c2f994e3"
        hex"eaa601f3f9f588fc0666da73157b8bbc488d29435da78ece60b9cb75e5b580578d089ab56dad740a02582faf40afff2c87162b64b87406dc182cee61"
        hex"d330071a3c33a71ffc4acf6b55aaf3cecd40fc922ace1633fbcb5fbd70e94f5ce63ec4d1ae4ab80233d14fb118970cb9b0404fa594a60141498ec1ca"
        hex"8ce5118f520d50a691f5fa302b4d765c39febcb0e511c5e384f02dced05c7c3196414b285851c00f6783df72eb2e9729fd935536e70c76aa51522af0"
        hex"d45002b51af308e68a16ed27a32bd97b780a3ac79220d1d0b56be97b1b031a55e1708f472ee01844c9ccc5d22192f2fafa057216eff5b461b5185076"
        hex"39182e16d55a8c65b37e0d907d308243e4ae7b9a88c6c9c9a25a6f24129300e821f224e04463b08c6fc3cbe67ff26d8cce4ec4dd0cb364b70a1a5acb"
        hex"a3ba48907d69089fee28e54374831a792af431a472a4ed56553226656162ca549dd4fdd201300a7c6acd1e6b32bfe94895430fa5d6d4cd6c5c676ca3"
        hex"1a31bd4892f5eb804b6604b78bbe827f788a52a731e0b914cc2c2fd526ef7455147fed59a4d3c0f618a62d1c8f62fb565481184a380e72df4782daab"
        hex"3d6fdc4f94eb851b1277786570e70a355e891c55dbaf51f3f7d9d6160b5143b221783dcad0482924aaed094e0a2d0e15ca80f09d5a7b458f6930c977"
        hex"b27df9e3fd80ebdebcf279e1f6cc61758fc701c7711747db22ceba93fe6da2fa2ba0d7a983c70d0c5a34f2893b30d7caa11f00c7fac5d09b8d0841a9"
        hex"4555c88570730d0a0188ea4a371c8547f6fd18c701312fa029a76c5ff03968d8bd3861dbea1869adbcd4d29787c03d544bca77e9924c1193f91ddac6"
        hex"60628e38ee26e1d2f1a1a5ab2b76782cc2140ee1f3ec781a86ed2e4681ba576f7eaee23e0a2d19a29e7d84438359db80456a1b05eaa7c85c725e29fa"
        hex"2ff7b2fe07f951150f5b9b1c054234dd1e1e6f4d2b1de5b67514742ff14b17c58014ff9e63a1d8bc4d7a03efa5d9d4abc8de2813b2f3be43f938f5cb"
        hex"5d1f13ca61f890076a89584f9c7c88e6d041043e03ff34fd85096edb482f0282a0731a164886ddc038296bc3ac6571a67fdcac76ac4dec08fbd114bd"
        hex"7ec7f082c4f01dc271cda8d41d73045c359819111b7d97e7dcca2398e0ccb2243a7d4479e6f20cfe168f56e34f96669f6a5978eca87677b60ec29091"
        hex"712710ba344125b704481ff1efd878d0cc719b8847bf33d3e97f122268a8b566bc0534cb3a40eb308c320d021231a712c0a290f9f3202a73d96e3b03"
        hex"e44f7163b25c53cdb73339ece8b314d2f3fd9e5747e0b953525d0f4f2f1ae027fa79ce6b222d65d855a0c9b54c4722e2bcc266bc0bc462f97ca96b35"
        hex"5868d8d6c10188a0c38c6f106cc5a61994ff20cda432439ae8bc1896ead7b0c7a1c8ee3d368aa4379ca63da385727e51d07d02800974bbd4fbbee879"
        hex"f36fd82fdf73b6326f195b878cc07c38844cc0a520912c5576bf3f1b4de0fd7cb6d4781796178cf6f94b11e25ec1846ce85e8b7e6c3701241f308cf3"
        hex"c66dd708f7ecdb7d9f423afb05386535632e4b0709bfc80f16790012ccf8a919b19368d3df091bd8ef0caff32015d090f67170c50d743a7785281d2e"
        hex"bf7a526ca90cd160d8fac19e7a0e8eed48ac6941853f872c1eed0679b5b9091f0e63f080989bd859cbe0d5b5a4db49f582ff55efdf2c70aec5e5f8a2"
        hex"577112e062f71609c7cdc02ac1de3ef007d17186880a8f87992e5b4e0fedb1f01b9a1eadc02e76e036464b702b6ffdbba4788cfc832b392cf49bef81"
        hex"9ca59dfbc7781aec22453b18025587552300c579ee29ac99021925ea244e67c993b8304d15e810be56a81c796cc751a75cd0df66e5f14a6966cc127f"
        hex"493e2325473ea30146110c9dcdad7e086d8aed085cac93663f73a9cd319e8ec8a9b9aa4c9ac8302b3ab520a42c06d94cc90ebdfa9c45c3e27d51baa3"
        hex"f8ae5f0400cd63d030cdc2773ad71ccf6bac6649af32dff142313d75c2f4deebb2c6f90ccbe54f824819019a34f4054fd610b991badffba85c65e489"
        hex"664d1e9bd0711908a7a4a7a23e20bf65622a2baa48030038cd2b4e894fea23aa971a037bcdb179d343fc6676b13492e8028a082fc34c17b98cc27045"
        hex"947539913d27b308eb00888425b25a2290b0fa647e6b0d242282f81a596d6ca14baff850829d5ff13e0278c06b46dbf152b6b97d37601a6d78e00b6f"
        hex"ecc8456b3f4eece1786b24b2ea75c2dbd255de4b50bc6737d189153bbb2d6e413eaecf145bdb60ddd61dea928a4bd8b2b9eca55dd9ac85a82f25093b"
        hex"6100f163de6f1e60db8fe48ca8ffc692fb9c8042da34672b07f51e8d35b11a27636436091d202736cb1ad0340b6e162e459fedba0ab49e86ea095ffd"
        hex"f08b2f2fe49b8ed97c6499b16148e40dfd4c27c9c32abea34a0b10d5fb8e4e2923701fe599ff0bdbc49dc9a3fe7c8b6a9f4ed8a0389d373b662336ea"
        hex"0ed353750446241f0a45951bb8a52e1e45ace95b49f367396cb33ab6d9082862a8aa16d391fc170e99b83450596187dd90056412c7f1296ac6affc27"
        hex"acbb5cbecbb8ae34496e161b20305f18272aa0c26f104ce2d1236fcc19d4a95bee54b754401c26e964ca14798552753e0b094ab5ed32417784f02125"
        hex"77ba36493734b1ea6e2b174eee7c2abf604a687621da9e686b7d4e1ce84ac118fb666f7f97e3cadf57a9b0fe8b91293e62f95681dabc83bf58dbf2b8"
        hex"96f9f65eed11c1adf752ffb287b820b0651a2011040e87a922e0a0e43af1839aa3a5e24c3c9c72dafda2afb6916bbc86a0740cbf3814489ee1b03451"
        hex"ec91fafa4b9aabe0a77b7556f35f05660655fb796613144691c5f9870bfe378a1eae0c692c23749b485654ab4aa579dd05a3545937eb28eca64e7042"
        hex"ddd84c79de276d53a08c72bb1313b779941c803c2969d373b06016609ade3f7f1d290fbe30ae20ce00ec5e331806938aa04d4727d80e556583da052e"
        hex"ac65e46494eedea68e382d137ca5a7f9bf5091b0c98b5a4aa4872c66cdce265d86418a0e5dd9e3a275edf470da1a091e72e35b39f988e0066bafdc6e"
        hex"e6d91264ce944978c5455c7de02330c6f8687979ab3ac41d4a23b04c57bc90d801f02b7c905a8c4abe39240d6a94acea2216f44247e39f9e0d9d4af7"
        hex"3a5d11c80362111c2e6ad08d05c2a433429916b1a1cd9f510846295542211f0ea9b88ad08f471b81c391bd59dc221d54b6f5864320cab2d08d729a54"
        hex"892674f40a3584e23f7e18981e5fdaacf9621f093b00e589d7721af5f927bb96a7143df939818a7131bd27b3a1d0dc33fabe7dbb99fece1b3c586374"
        hex"100d15328c3d129214b9c48f050e0ff03dc45cf1c05a998513dd15a470e8f77aad92595af9ab457cebbf500e2ccf1c084804a0014265730e84056a5c"
        hex"fce1958d1035bf15225bff39f6dde45b87e401edfa273111dc50e4761c85c92342c60bfa05f7b4345c5aa51fcec7ce48b2681e495eb5efef1b582c38"
        hex"8ba3fe28ee9ba5929206de0f12b7067674b38eada37c07255dad55c997148280d04b8975bb75fa9eda7657e5c7435649cf1bcc911e362b367e0dac97"
        hex"d84eceb02b47e2601a60a6d68a08862f003fb3ce589bef87acba0204fa2fc43509b3486dba281d9595692f0645e92857219db2991b3d481f0da01553"
        hex"4b2a53ba784bb34c29311b827a8188c8334380d2abf3a1c3bcb39ff621a90ed8f53a069d3ad23ced2e46b4df9b1f08063cbd2a9094b3008d14106d2e"
        hex"0da820c68f7c36e9ee4d5e2db0e064c51ecb9a70a9c201835907c14bbf6e7d960b6325ac131cf65b8449ca44f71934cf8b72df7fa403ee2565032d36"
        hex"fc2e5a323ea0274b682fd30d3cd5242398fec5efaa3485aa80772cac5e3f8ea388596145e932021dbb988e00221d3f6869b53ce3165e6c022f89ccdd"
        hex"b62ceb5d5eca6eff670b13984f26834cd7c115a01c1e38de8eb4dd523d1d434f7ef1f079c8eea06eeaa52f02131fd98ced959705bcd57e8a788648d5"
        hex"78b7f7884e0818b8109a5aad6b6511ffaee0c9ab1f4015758710dbaa380c0be46b1e06dd2f9b4d31cdbbe4ec037f065461d06173cbeb0a7e1d5a9ff7"
        hex"5e8d155d69d3cd26314060a0f0a8f8b517f90c758ce1ea61c8d877913d4475a27adc25bd206cfa6107786bda9b71c3a0c81c27e3ff58bb08268cbaec"
        hex"b8fb98e48f41744a059785b0068fc2aa41cbe3a553690edb0fe34eec06e875bddc319f962b743a77361b940343f7eb552e64356bc8652d036464628c"
        hex"0b940aa594e1f7c65fe9d8dfbd8229ef50601a7d5840b9f00f0f2b81867cc2ba295c607e48d86f84ba289122cc18bcc9d4758664d78cb3c1416e018e"
        hex"4a2bbf39cd75262d286dfc295a63629105ec51297fdc40e70473280c31df1c21f4cd8178df9f24a09653effa15d6b9380ae30f72fac78bae7a2aa154"
        hex"5b042a38d2583972c4c44b799a858a616c7a5cd86dd696ad80892f23aab1cc881ee012719ca5c118067ea6024211c712b71940d55405455554cd59da"
        hex"788266bd6d07203c38236c6d33756533877bfb63ea8a869ce8b6984bf07cd29518c4bcaaa7b90c3293b5812812b0ed3a9b6120c7b6ada7d995e2dfe6"
        hex"f78ea175e2c8d3c135de25f341bb1741e4e97d4ddaf09971b5238505aa978deb7eb1d90e2999233df2fa1d5677fba2e2394b694b38d15ac87be9e639"
        hex"b27a6785405d3732f8e754a1ac7317ea06ecc0929e49f9d80958d8b62a7bf831a554fafd3dd4c3f80f4095bf87660f9afd9a68106902a0a98819413c"
        hex"bc73758e1b9a91a1100aa8a632c268270030095ce7d646ae90a55a21ededa23b27481fbc64fc3fe34524f73acf93c35838701f";

    /// @notice Contract constructor.
    /// @dev `_depositVoucherCode` is intentionally NOT a constructor arg:
    ///       in the only deploy path that matters (zerostate premine stub +
    ///       `updateCode` upgrade) the voucher code arrives via the
    ///       `onCodeUpgrade` payload. There is no standalone setter (B2 fix),
    ///       so the only way to populate / rotate `_depositVoucherCode` is a
    ///       full `updateCode` upgrade of eccUSDCBridge.
    /// @param pubkey — owner public key for admin operations
    /// @param usdcWallet — address of the Exchange's TIP-3 USDC TokenWallet (subscriber target)
    constructor(
        uint256 pubkey,
        address usdcWallet
    ) accept {
        require(pubkey != 0, ERR_ZERO_PUBKEY);
        _ownerPubkey = pubkey;
        _usdcWallet = usdcWallet;
    }

    /// @notice Ensures contract balance stays above MIN_BALANCE by minting vmshell if needed.
    function ensureBalance() private pure {
        if (address(this).balance >= MIN_BALANCE) { return; }
        gosh.mintshellq(MIN_BALANCE);
    }

    // ========================================================
    // TIP-3 USDC -> ECC[3] bridge (ISubscriber callback)
    // ========================================================

    /// @notice ISubscriber callback invoked by the bridge's TIP-3 USDC TokenWallet
    ///         when it receives a TIP-3 transfer. Mints equivalent ECC[3] USDC and sends
    ///         it to the original depositor. Only callable by _usdcWallet.
    /// @param from — address of the original depositor (wallet owner who sent TIP-3 USDC)
    /// @param value — amount of TIP-3 USDC received (in micro-USDC, 6 decimals)
    function onTransferReceived(
        address from,
        address /*to*/,
        uint128 value,
        uint128 /*balance*/
    ) external override {
        require(msg.sender == _usdcWallet, ERR_INVALID_SENDER);
        tvm.accept();
        ensureBalance();

        // TIP-3 USDC deposited -> mint ECC[3] and send to depositor
        require(value <= uint128(type(uint64).max), ERR_OVERFLOW);
        gosh.mintecc(uint64(value), USDC_ECC_ID);
        _totalMinted += value;

        mapping(uint32 => varuint32) ecc;
        ecc[USDC_ECC_ID] = varuint32(value);
        from.transfer({value: 1 vmshell, bounce: false, flag: 1, currencies: ecc});

        address addrExtern = address.makeAddrExtern(UsdcMigratedEmit, bitCntAddress);
        emit UsdcMigrated{dest: addrExtern}(from, value);
    }

    // ========================================================
    // Mint ECC[3] USDC and send to recipient (owner only)
    // ========================================================

    /// @notice Mints ECC[3] USDC and sends it to the specified recipient address.
    ///         Only callable by the owner (by public key). Increments
    ///         `_totalMinted` only — not bridged inventory. After CLI exit 37
    ///         (voucher spent, `confirmDeposit` aborted) use
    ///         `mintAndSendBridged` instead, or the USDC already in the
    ///         Ethereum treasury never becomes withdrawable headroom.
    /// @param recipient — address to receive the minted ECC[3] USDC
    /// @param value — amount of ECC[3] USDC to mint and send (in micro-USDC)
    function mintAndSend(address recipient, uint128 value, uint64 nonce) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        ensureBalance();
        require(nonce == _mintNonce + 1, ERR_INVALID_NONCE);
        require(value > 0, ERR_ZERO_AMOUNT);
        require(value <= uint128(type(uint64).max), ERR_OVERFLOW);
        _mintNonce = nonce;

        gosh.mintecc(uint64(value), USDC_ECC_ID);
        _totalMinted += value;

        mapping(uint32 => varuint32) ecc;
        ecc[USDC_ECC_ID] = varuint32(value);
        recipient.transfer({value: 1 vmshell, bounce: false, flag: 1, currencies: ecc});

        address addrExtern = address.makeAddrExtern(UsdcMintedEmit, bitCntAddress);
        emit UsdcMinted{dest: addrExtern}(recipient, value);
    }

    // ========================================================
    // Owner recovery mint that credits bridged inventory
    // ========================================================

    /// @notice Owner recovery mint matching `confirmDeposit` accounting:
    ///         `gosh.mintecc` plus `_totalMintedBridgeByToken[USDC_ECC_ID]`.
    ///         Call this after CLI exit 37: the deposit voucher is spent so
    ///         `finalizeDeposit` cannot mint again, Ethereum already holds
    ///         the USDC, and ordinary `mintAndSend` would leave
    ///         `getTotalBridged` short — last withdrawers then hit 233.
    ///         Shares `_mintNonce` with `mintAndSend`. The call is not bound
    ///         to a deposit id: paying twice, or typing the wrong `value`,
    ///         still credits bridged inventory the Ethereum treasury does
    ///         not hold, and last withdrawers then lose the difference.
    ///         Match the aborted deposit's recipient and amount, once.
    ///         Do not use it for ordinary owner mint.
    /// @param recipient — address to receive the minted ECC[3] USDC
    /// @param value — amount of ECC[3] USDC to mint and send (in micro-USDC)
    /// @param nonce — next `_mintNonce` (shared with `mintAndSend`)
    /// @param chainId — EIP-155 source chain whose aborted deposit is being paid
    function mintAndSendBridged(address recipient, uint128 value, uint64 nonce, uint256 chainId) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        ensureBalance();
        require(nonce == _mintNonce + 1, ERR_INVALID_NONCE);
        require(value > 0, ERR_ZERO_AMOUNT);
        require(value <= uint128(type(uint64).max), ERR_OVERFLOW);
        require(value <= MAX_TRANSFER_AMOUNT, ERR_TRANSFER_TOO_LARGE);
        require(_outstandingByChain[chainId] + value <= MAX_BRIDGE_TVL, ERR_TVL_CAP);
        _mintNonce = nonce;

        gosh.mintecc(uint64(value), USDC_ECC_ID);
        _totalMintedBridgeByToken[USDC_ECC_ID] += value;
        _outstandingByChain[chainId] += value;

        mapping(uint32 => varuint32) ecc;
        ecc[USDC_ECC_ID] = varuint32(value);
        recipient.transfer({value: 1 vmshell, bounce: false, flag: 1, currencies: ecc});

        address addrExtern = address.makeAddrExtern(UsdcMintedEmit, bitCntAddress);
        emit UsdcMinted{dest: addrExtern}(recipient, value);
    }

    // ========================================================
    // Mint USDC and send to Accumulator for a buyer
    // ========================================================

    /// @notice Mints ECC[3] USDC and sends it to the Accumulator's buyShellFor,
    ///         which will process the purchase and send ECC[2] Shell to the buyer.
    /// @param buyer — address to receive Shell from the Accumulator
    /// @param value — amount of ECC[3] USDC to mint (in micro-USDC)
    function mintAndSendAccumulator(address buyer, uint128 value, uint64 nonce) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        ensureBalance();
        require(nonce == _mintAccumulatorNonce + 1, ERR_INVALID_NONCE);
        require(value > 0, ERR_ZERO_AMOUNT);
        require(value % USDC_DECIMALS_FACTOR == 0, ERR_NOT_WHOLE_USDC);
        require(value <= uint128(type(uint64).max), ERR_OVERFLOW);
        _mintAccumulatorNonce = nonce;

        gosh.mintecc(uint64(value), USDC_ECC_ID);
        _totalMinted += value;

        mapping(uint32 => varuint32) ecc;
        ecc[USDC_ECC_ID] = varuint32(value);
        IShellAccumulator(ACCUMULATOR_ADDRESS).buyShellFor{value: 1 vmshell, bounce: false, flag: 1, currencies: ecc}(buyer);

        address addrExtern = address.makeAddrExtern(UsdcMintedEmit, bitCntAddress);
        emit UsdcMinted{dest: addrExtern}(buyer, value);
    }

    // ========================================================
    // Cross-chain bridge — outbound (AN -> any chain): burn ECC, emit proof-source event
    // ========================================================

    /// @notice Burns the ECC currency attached to this message and emits an event
    ///         carrying the data needed to mint the equivalent on the destination
    ///         chain. Exactly one ECC currency must be attached; its id and amount
    ///         are taken from `msg.currencies`. The destination chain is opaque
    ///         to this contract — `dstChainId` is just passed through to the event.
    /// @param dstChainId — opaque destination chain identifier (passed through to event)
    /// @param recipient  — destination-chain recipient bytes (≤64 bytes)
    function initiateWithdrawal(uint256 dstChainId, bytes recipient) public {
        require(!_paused, ERR_PAUSED);
        require(recipient.length > 0, ERR_RECIPIENT_EMPTY);
        require(recipient.length <= 64, ERR_RECIPIENT_TOO_LONG);
        require(!_isZeroRecipient(recipient), ERR_ZERO_RECIPIENT);

        mapping(uint32 => varuint32) currencies = msg.currencies;
        uint32[] keys = currencies.keys();
        require(keys.length >= 1, ERR_NO_ECC);
        require(keys.length == 1, ERR_MULTIPLE_ECC);

        uint32 tokenId = keys[0];
        require(tokenId == USDC_ECC_ID, ERR_UNSUPPORTED_TOKEN);
        if (currencies[tokenId] > varuint32(MAX_TRANSFER_AMOUNT)) {
            _refundWithdrawal(currencies, dstChainId, recipient, tokenId,
                              ERR_TRANSFER_TOO_LARGE);
            return;
        }
        uint128 amount = uint128(currencies[tokenId]);
        require(amount > 0, ERR_ZERO_AMOUNT);
        require(amount <= uint128(type(uint64).max), ERR_OVERFLOW);
        uint128 minted = _totalMintedBridgeByToken[tokenId];
        uint128 burned = _totalBurnedBridgeByToken[tokenId];
        if (burned > minted || amount > minted - burned) {
            _refundWithdrawal(currencies, dstChainId, recipient, tokenId,
                              ERR_BURN_EXCEEDS_BRIDGED);
            return;
        }
        if (amount > _outstandingByChain[dstChainId]) {
            _refundWithdrawal(currencies, dstChainId, recipient, tokenId,
                              ERR_CHAIN_BALANCE_EXCEEDED);
            return;
        }

        tvm.accept();
        ensureBalance();

        gosh.burnecc(uint64(amount), tokenId);
        _totalBurnedBridgeByToken[tokenId] += amount;
        _outstandingByChain[dstChainId] -= amount;

        address addrExtern = address.makeAddrExtern(WithdrawalInitiatedEmit, bitCntAddress);
        emit WithdrawalInitiated{dest: addrExtern}(dstChainId, recipient, amount, tokenId, msg.sender);
    }

    /// @dev Returns the ECC and says why on `WithdrawalRefused`, so a refusal
    ///      leaves a record a relayer or a UI can read: `reason` is the error
    ///      code the call would have thrown if a refusal threw —
    ///      `ERR_TRANSFER_TOO_LARGE` (234) over the per-transfer cap,
    ///      `ERR_BURN_EXCEEDS_BRIDGED` (233) when the burn would take bridged
    ///      burned above bridged minted, `ERR_CHAIN_BALANCE_EXCEEDED` (236)
    ///      when the destination chain accounts for less than this amount.
    ///      An amount too large for `uint128` is reported saturated; it is
    ///      already far above the cap that refused it.
    function _refundWithdrawal(mapping(uint32 => varuint32) currencies, uint256 dstChainId,
                               bytes recipient, uint32 tokenId, uint16 reason) private pure {
        tvm.accept();
        ensureBalance();
        varuint32 sent = currencies[tokenId];
        uint128 shown = sent > varuint32(type(uint128).max)
            ? type(uint128).max : uint128(sent);
        address addrExtern = address.makeAddrExtern(WithdrawalRefusedEmit, bitCntAddress);
        emit WithdrawalRefused{dest: addrExtern}(dstChainId, recipient, shown, tokenId,
                                                msg.sender, reason);
        msg.sender.transfer({value: math.min(msg.value, 0.1 vmshell),
                             bounce: false, flag: 1, currencies: currencies});
    }

    /// @dev True when every byte of `recipient` is zero. The destination chain
    ///      is opaque here, so this cannot compare against one chain's zero
    ///      address: a 20-byte EVM zero and a 32-byte one both name no one.
    function _isZeroRecipient(bytes recipient) private pure returns (bool) {
        TvmSlice s = recipient.toSlice();
        uint i;
        for (i = 0; i < recipient.length; i++) {
            if (s.bits() < 8) {
                s = s.loadRef().toSlice();
            }
            if (s.loadUint(8) != 0) {
                return false;
            }
        }
        return true;
    }

    // ========================================================
    // Cross-chain bridge — inbound (any chain -> AN): verify proof, deploy DepositVoucher, mint ECC
    // ========================================================

    /// @notice Owner-managed source-chain allowlist entry: add or remove ONE L1
    ///         bridge contract from the trusted SET of `chainId`. `l1Bridge` is
    ///         the L1 address left-padded to uint256, exactly as the circuit
    ///         exposes it in the public inputs (fr[3]). `allowed=true` trusts it,
    ///         `false` revokes it; several addresses may be trusted on the same
    ///         chain at once (rotation window). Applies to `finalizeDeposit`
    ///         only — the outbound path and the TIP-3/owner-mint flows are
    ///         unaffected.
    function setTrustedL1Bridge(uint256 chainId, uint256 l1Bridge, bool allowed) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        ensureBalance();
        if (allowed) {
            _trustedL1Bridge[chainId][l1Bridge] = true;
        } else {
            delete _trustedL1Bridge[chainId][l1Bridge];
        }
    }

    /// @notice Returns the trusted L1 bridge SET for `chainId` (address -> true).
    function getTrustedL1Bridges(uint256 chainId) external view returns (mapping(uint256 => bool)) {
        return _trustedL1Bridge[chainId];
    }

    /// @notice True if `l1Bridge` is in the trusted set of `chainId`.
    function isTrustedL1Bridge(uint256 chainId, uint256 l1Bridge) external view returns (bool) {
        return _trustedL1Bridge[chainId][l1Bridge];
    }

    /// @notice Admits (or retracts) a source-chain block hash as canonical.
    ///         Whoever holds the owner key MUST:
    ///           * verify the block against an independent node, not against
    ///             the relayer that produced the proof, and
    ///           * wait for enough confirmations, since a reorged-out block
    ///             loses the funds exactly like a dishonest one.
    ///
    ///         Accepts a hash rather than a header so that swapping this writer
    ///         for the light client later needs no change to `finalizeDeposit`.
    /// @param chainId   — EIP-155 chain id of the source L1/L2.
    /// @param blockHash — the block's 256-bit hash, as the circuit binds it into
    ///                    public inputs #9/#10 (`hi << 128 | lo`).
    /// @param accepted  — true to admit, false to retract (e.g. on discovering
    ///                    the block was reorged out before any deposit landed).
    function setAcceptedBlockHash(uint256 chainId, uint256 blockHash, bool accepted)
        public onlyOwnerPubkey(_ownerPubkey) accept saveMsg
    {
        require(!_ownerAnchorsDisabled[chainId], ERR_OWNER_ANCHORS_DISABLED);
        ensureBalance();
        if (accepted) {
            _acceptedBlockHash[chainId][blockHash] = true;
        } else {
            delete _acceptedBlockHash[chainId][blockHash];
        }
    }

    /// @notice Installs the code every light client is deployed from. It
    ///         changes the address the next `deployLightClient` derives;
    ///         clients already recorded in `_lightClients` keep their
    ///         addresses and stay recognized as writers. This remains owner-
    ///         callable after the anchor flip; a subsequent `deployLightClient`
    ///         can replace the recorded writer for a flipped chain.
    function setLightClientCode(TvmCell code) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        ensureBalance();
        _lightClientCode = code;
    }

    /// @notice Deploys the beacon light client. The bridge is the only account
    ///         allowed to do so: the contract's constructor refuses any other
    ///         sender, so nothing else can occupy that address. Always writes
    ///         `l1ChainId` into `_lightClientL1ChainId`, even if the
    ///         constructor message bounces because the account already exists.
    function deployLightClient(
        uint256 pubkey,
        uint256 l1ChainId,
        uint256 bootstrapCommittee,
        uint64  bootstrapPeriod
    ) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        require(_lightClientCode.depth() != 0, ERR_LIGHT_CLIENT_UNSET);
        require(l1ChainId != 0, ERR_NOT_LIGHT_CLIENT_CHAIN);
        ensureBalance();
        TvmCell stateInit = abi.encodeStateInit({
            contr: EthBeaconLightClient,
            varInit: {},
            code: abi.setCodeSalt(_lightClientCode, abi.encode(l1ChainId))
        });
        address lightClient = address.makeAddrStd(0, tvm.hash(stateInit));
        new EthBeaconLightClient{
            stateInit: stateInit,
            value: 10 vmshell,
            flag: 1
        }();
        EthBeaconLightClient(lightClient).initialize{
            value: 2 vmshell,
            flag: 1
        }(pubkey, l1ChainId, bootstrapCommittee, bootstrapPeriod);
        _lightClients[l1ChainId] = lightClient;
        _lightClientL1ChainId = l1ChainId;
    }

    /// @notice Address the light client for `l1ChainId` is deployed at
    ///         (0 until `deployLightClient` ran for that chain).
    function getLightClient(uint256 l1ChainId) external view returns (address) {
        return _lightClients[l1ChainId];
    }

    /// @notice Disables direct owner anchor writes for the chain whose light
    ///         client was deployed last. Other chains keep their own setting;
    ///         use `disableOwnerAnchorsOn` to select one explicitly.
    /// @dev There is no direct re-enable function in this code, but the owner
    ///      retains `updateCode`, which resets these flags, and can replace
    ///      the recorded client through `setLightClientCode` followed by
    ///      `deployLightClient`. This call does not revoke owner trust or
    ///      upgrade authority. Requires a recorded client for the chain;
    ///      it does not check that the account is Active. Idempotent.
    function disableOwnerAnchors() public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        _disableOwnerAnchors(_lightClientL1ChainId);
    }

    /// @notice Same as `disableOwnerAnchors()`, for an explicit chain id.
    ///         Any chain with a light client in `_lightClients` is accepted;
    ///         a chain without one reverts `ERR_LIGHT_CLIENT_UNSET` (226),
    ///         and chain id 0 reverts `ERR_NOT_LIGHT_CLIENT_CHAIN` (227).
    ///         Sold does not allow a public overload of
    ///         `disableOwnerAnchors`.
    function disableOwnerAnchorsOn(uint256 chainId) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        _disableOwnerAnchors(chainId);
    }

    function _disableOwnerAnchors(uint256 chainId) private {
        require(chainId != 0, ERR_NOT_LIGHT_CLIENT_CHAIN);
        // `exists`, not `!= address(0)`: a missing key reads as addr_none,
        // which is not address(0), so that comparison passed for any chain.
        require(_lightClients.exists(chainId) || _l2Verifiers.exists(chainId),
                ERR_LIGHT_CLIENT_UNSET);
        ensureBalance();
        _ownerAnchorsDisabled[chainId] = true;
    }

    /// @notice Stops and resumes the cross-chain lane: while paused,
    ///         `finalizeDeposit` and `initiateWithdrawal` throw
    ///         `ERR_PAUSED` (231) before doing any work. Reversible, and
    ///         deliberately narrow — the TIP-3 inbound callback, the owner
    ///         mints, `confirmDeposit` and every anchor and allowlist call stay
    ///         available, so a deposit already proven can still be paid out and
    ///         the owner can keep the bridge's configuration current while it
    ///         is stopped.
    function setPaused(bool paused) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        ensureBalance();
        _paused = paused;
    }

    /// @notice States how much of one chain's principal this bridge still
    ///         accounts for, which is the ceiling `initiateWithdrawal` pays out
    ///         to that chain and the room left under `MAX_BRIDGE_TVL` for a
    ///         credit to it. `confirmDeposit` raises the counter, a withdrawal
    ///         spends it, and `onCodeUpgrade` carries it between builds that
    ///         both have it. What leaves it empty is a migration from a build
    ///         that had none, while the principal stays locked on the source
    ///         chain. This is how the owner states what is locked there.
    ///
    ///         The figure to write is the principal L1 holds less the deposits
    ///         not yet credited here and the burns L1 has not yet paid out —
    ///         not L1's balance, which still carries both. The counter is also
    ///         the deposit room, since `finalizeDeposit` and `confirmDeposit`
    ///         bound `outstanding + amount` by `MAX_BRIDGE_TVL`, so writing
    ///         more than that figure buys withdrawal room by refusing deposits
    ///         with `ERR_TVL_CAP`, and a deposit caught between finalization
    ///         and confirmation aborts with its voucher already consumed.
    ///         Writing less is the safe direction: it only refuses
    ///         withdrawals, which return the ECC.
    ///
    ///         It moves this counter alone. A withdrawal is still bounded by
    ///         bridged minted less bridged burned, so where that inventory is
    ///         empty too — a fresh deployment — this pays nothing out on its
    ///         own: the withdrawal is refused with `ERR_BURN_EXCEEDS_BRIDGED`
    ///         instead. It recovers a deployment that carried its inventory
    ///         across an upgrade but not its per-chain counter. It is no new
    ///         authority: `mintAndSend` has no ceiling at all, and a call that
    ///         writes a counter without minting is weaker than one that mints.
    ///         See `contracts/an/README.md`, *Restoring a chain's outstanding
    ///         amount*.
    /// @param chainId — EIP-155 chain id of the destination L1/L2.
    /// @param amount  — principal outstanding to that chain, in token units,
    ///                  at most `MAX_BRIDGE_TVL`.
    function setOutstandingByChain(uint256 chainId, uint128 amount)
        public onlyOwnerPubkey(_ownerPubkey) accept saveMsg
    {
        require(amount <= MAX_BRIDGE_TVL, ERR_TVL_CAP);
        ensureBalance();
        _outstandingByChain[chainId] = amount;
    }

    /// @notice Admits a block hash the light client proved final on the source
    ///         chain. Authorized solely by being the light client this bridge
    ///         — no human asserts canonicality, the proof does.
    function acceptBlockHashFromLightClient(uint256 chainId, uint256 blockHash) public {
        require(_lightClients[chainId] != address(0) && msg.sender == _lightClients[chainId],
                ERR_INVALID_SENDER);
        tvm.accept();
        ensureBalance();
        _acceptedBlockHash[chainId][blockHash] = true;
    }

    /// @notice Drops a hash the light client has aged out of its window.
    function forgetBlockHashFromLightClient(uint256 chainId, uint256 blockHash) public {
        require(_lightClients[chainId] != address(0) && msg.sender == _lightClients[chainId],
                ERR_INVALID_SENDER);
        tvm.accept();
        ensureBalance();
        delete _acceptedBlockHash[chainId][blockHash];
    }

    /// @notice Installs the code every Arbitrum block verifier is deployed
    ///         from. Verifiers already recorded keep their addresses.
    function setL2VerifierCode(TvmCell code) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        ensureBalance();
        _l2VerifierCode = code;
    }

    /// @notice Deploys the block verifier of Arbitrum chain `l2ChainId`. It
    ///         proves that chain's blocks from the assertions `rollup` posts on
    ///         `parentChainId`, whose blocks must reach `_acceptedBlockHash`
    ///         through that chain's light client. Re-running it for a chain
    ///         replaces the recorded verifier.
    /// @param l2ChainId     — EIP-155 id of the Arbitrum chain (42161, 421614).
    /// @param parentChainId — EIP-155 id of the chain its rollup lives on (1, 11155111).
    /// @param rollup        — the rollup contract that emits `AssertionCreated`.
    function deployL2Verifier(uint256 l2ChainId, uint256 parentChainId, uint160 rollup)
        public onlyOwnerPubkey(_ownerPubkey) accept saveMsg
    {
        require(_l2VerifierCode.depth() != 0, ERR_L2_VERIFIER_UNSET);
        require(l2ChainId != 0 && parentChainId != 0 && rollup != 0, ERR_NOT_LIGHT_CLIENT_CHAIN);
        ensureBalance();
        TvmCell stateInit = abi.encodeStateInit({
            contr: ArbitrumBlockVerifierDeploy,
            varInit: {},
            code: abi.setCodeSalt(_l2VerifierCode, abi.encode(l2ChainId))
        });
        address verifier = address.makeAddrStd(0, tvm.hash(stateInit));
        new ArbitrumBlockVerifierDeploy{
            stateInit: stateInit,
            value: 10 vmshell,
            flag: 1
        }();
        ArbitrumBlockVerifierDeploy(verifier).initialize{
            value: 2 vmshell,
            flag: 1
        }(l2ChainId, parentChainId, rollup);
        _l2Verifiers[l2ChainId] = verifier;
        _l2VerifierParent[l2ChainId] = parentChainId;
    }

    /// @notice The verifier of `l2ChainId` and its parent chain (0, 0 until
    ///         `deployL2Verifier` ran for it).
    function getL2Verifier(uint256 l2ChainId) external view returns (address verifier, uint256 parentChainId) {
        return (_l2Verifiers[l2ChainId], _l2VerifierParent[l2ChainId]);
    }

    /// @notice Admits an Arbitrum block hash the chain's verifier proved from
    ///         an assertion in parent-chain block `parentBlockHash`. Only that
    ///         verifier may call it, and only once the parent-chain block is
    ///         accepted here, so the trust reaches back to the light client.
    function acceptBlockHashFromL2Verifier(uint256 l2ChainId, uint256 parentBlockHash, uint256 blockHash) public {
        require(_l2Verifiers.exists(l2ChainId) && msg.sender == _l2Verifiers[l2ChainId],
                ERR_INVALID_SENDER);
        require(_acceptedBlockHash[_l2VerifierParent[l2ChainId]][parentBlockHash], ERR_UNKNOWN_BLOCK);
        tvm.accept();
        ensureBalance();
        _acceptedBlockHash[l2ChainId][blockHash] = true;
    }

    /// @notice True if `blockHash` is admitted as canonical for `chainId`.
    function isAcceptedBlockHash(uint256 chainId, uint256 blockHash) external view returns (bool) {
        return _acceptedBlockHash[chainId][blockHash];
    }

    /// @notice Anchor-path configuration of the chain whose light client was
    ///         deployed last, and whether the owner may still admit anchors
    ///         there. With clients on several chains this names only that one:
    ///         read `getLightClient(chainId)` and
    ///         `isOwnerAnchorsEnabled(chainId)` for a specific chain.
    function getAnchorConfig() external view returns (address lightClient, bool ownerAnchorsEnabled) {
        return (_lightClients[_lightClientL1ChainId],
                _lightClientL1ChainId == 0 || !_ownerAnchorsDisabled[_lightClientL1ChainId]);
    }

    /// @notice Whether the owner may still admit anchors for `chainId`.
    ///         False after `disableOwnerAnchors` / `disableOwnerAnchorsOn`
    ///         for that chain; every chain is independent.
    function isOwnerAnchorsEnabled(uint256 chainId) external view returns (bool) {
        return !_ownerAnchorsDisabled[chainId];
    }

    /// @notice EIP-155 chain id the deployed light client follows (0 until
    ///         `deployLightClient`).
    function getLightClientL1ChainId() external view returns (uint256) {
        return _lightClientL1ChainId;
    }

    /// @notice Whether the cross-chain lane is stopped by the owner.
    function isPaused() external view returns (bool) {
        return _paused;
    }

    /// @notice Finalizes an L1 deposit proven by the final ETH-deposit halo2
    ///         circuit (receipt-proof of the L1 deposit event). The relayer
    ///         passes the proof and its public-inputs blob verbatim; we verify
    ///         against `VK_BLOB` and read every deposit field straight out of the
    ///         PROVEN instances — amount, recipient and source identity are all
    ///         proof-bound, nothing is caller-set. A deterministic
    ///         `DepositVoucher` (keyed on the proof-bound deposit identity) gives
    ///         replay protection; it calls back `confirmDeposit` to mint + pay.
    /// @param proof         — SHPLONK proof bytes (no header), fed verbatim as the
    ///                         `proof_cell` operand of TVM opcode
    ///                         ZKHALO2VERIFYWITHVK (0xC7 0x4A).
    /// @param publicInputs  — the circuit instance column: 12 × 32-byte LE Fr
    ///                         (deposit_id, sender, amount, contract, chain_id,
    ///                         dapp_hi, dapp_lo, an_account_hi, an_account_lo,
    ///                         + 2 block-hash halves + promise commit). Verified
    ///                         verbatim; business fields read at fixed offsets —
    ///                         see `_parsePublicInputs`.
    function finalizeDeposit(bytes proof, bytes publicInputs) public {
        // Cheap parse + sanity BEFORE accept (within the pre-accept gas budget).
        require(!_paused, ERR_PAUSED);
        DepositPI f = _parsePublicInputs(publicInputs);
        require(f.amount > 0, ERR_ZERO_AMOUNT);
        require(f.amount <= MAX_TRANSFER_AMOUNT, ERR_TRANSFER_TOO_LARGE);
        require(_outstandingByChain[f.chainId] + f.amount <= MAX_BRIDGE_TVL, ERR_TVL_CAP);
        // The proof binds (chainId, contractAddr) to the L1 event; the allowlist
        // pins which (chain, bridge contract) pairs this side trusts. The deposit
        // passes if its proven address is in the chain's trusted set — an absent
        // entry is false, so unknown chains/addresses reject. The != 0 guard
        // keeps a stray `_trustedL1Bridge[chainId][0]=true` from ever admitting a
        // zero contract.
        require(f.contractAddr != 0 && _trustedL1Bridge[f.chainId][f.contractAddr],
                ERR_UNSUPPORTED_SRC_CHAIN);
        require(f.anAccount != 0, ERR_ZERO_RECIPIENT);

        // accept() must precede the halo2 verify: ZKHALO2VERIFYWITHVK is a
        // multi-second WASM extern that vastly exceeds the external-message
        // pre-accept gas limit. Permissionless submission — the proof itself is
        // the authorization; a garbage proof only wastes the bridge's own gas.
        tvm.accept();
        require(
            gosh.zkhalo2VerifyWithVK(VK_BLOB, publicInputs, proof),
            ERR_INVALID_ZKPROOF
        );

        // Canonical-chain gate: the proof binds the event to a block whose
        // header hashes to instances #9/#10, but says nothing about that block
        // being on the canonical chain. The hash must therefore appear in the
        // anchor set, which is asserted from outside the proof (see
        // `_acceptedBlockHash`). Parsed here rather than in the pre-accept path
        // so the external-message gas budget stays untouched.
        require(
            _acceptedBlockHash[f.chainId][_parseBlockHash(publicInputs)],
            ERR_UNKNOWN_BLOCK
        );

        _saveMsg();
        tvm.commit();
        ensureBalance();

        // Anti-replay anchor = proof-bound (deposit_id, source contract, source
        // chain); the dapp component is pinned to 0 (see _parsePublicInputs).
        // chainId is IN the key: two different L1s may legitimately emit the
        // same (deposit_id, contract) pair. amount/recipient are NOT in the
        // key — they are fixed by the proof, so a replay can never re-route or
        // re-mint: same key ⇒ same voucher ⇒ no-op.
        uint256 depositHash = tvm.hash(abi.encode(f.depositId, f.contractAddr, f.dappId, f.chainId));

        TvmCell stateInit = abi.encodeStateInit({
            contr: DepositVoucher,
            varInit: { _depositHash: depositHash },
            code: _depositVoucherCode
        });

        new DepositVoucher{
            stateInit: stateInit,
            value: 2 vmshell,
            flag: 1
        }(f.depositId, f.contractAddr, f.dappId, f.chainId, f.amount, f.anAccount);
    }

    /// @notice Internal callback from a freshly deployed `DepositVoucher`. Mints
    ///         USDC ECC and sends it to the proof-bound AN recipient. The caller
    ///         must be the deterministic voucher address derived from the
    ///         deposit identity — replay attempts hit the existing voucher
    ///         account whose constructor was already consumed. If this call
    ///         aborts after that constructor has run, the voucher is spent
    ///         and the owner pays the recipient with `mintAndSendBridged`.
    function confirmDeposit(
        uint256 depositId,
        uint256 contractAddr,
        uint256 dappId,
        uint256 chainId,
        uint128 amount,
        uint256 anAccount
    ) public {
        uint256 depositHash = tvm.hash(abi.encode(depositId, contractAddr, dappId, chainId));
        TvmCell stateInit = abi.encodeStateInit({
            contr: DepositVoucher,
            varInit: { _depositHash: depositHash },
            code: _depositVoucherCode
        });
        require(msg.sender == address.makeAddrStd(0, tvm.hash(stateInit)), ERR_INVALID_SENDER);
        require(anAccount != 0, ERR_ZERO_RECIPIENT);
        require(amount > 0, ERR_ZERO_AMOUNT);
        require(amount <= MAX_TRANSFER_AMOUNT, ERR_TRANSFER_TOO_LARGE);
        require(_outstandingByChain[chainId] + amount <= MAX_BRIDGE_TVL, ERR_TVL_CAP);

        tvm.accept();
        ensureBalance();

        gosh.mintecc(uint64(amount), USDC_ECC_ID);
        _totalMintedBridgeByToken[USDC_ECC_ID] += amount;
        _outstandingByChain[chainId] += amount;

        mapping(uint32 => varuint32) ecc;
        ecc[USDC_ECC_ID] = varuint32(amount);
        address.makeAddrStd(0, anAccount).transfer({
            value: 1 vmshell,
            bounce: false,
            flag: 1,
            currencies: ecc
        });

        address addrExtern = address.makeAddrExtern(DepositFinalizedEmit, bitCntAddress);
        emit DepositFinalized{dest: addrExtern}(
            depositId, contractAddr, dappId, chainId, amount, anAccount
        );
    }

    // DepositVoucher code rotation is intentionally not exposed as a
    // standalone setter. The only way to change `_depositVoucherCode` is via
    // a full `updateCode` upgrade of eccUSDCBridge (the new code+layout pass
    // through `onCodeUpgrade`). This removes the "owner can swap voucher
    // logic in one tx and free-mint" backdoor flagged in PR2112 review (B2).

    // ========================================================
    // Admin
    // ========================================================

    /// @notice Replaces the owner public key. Only callable by the current owner.
    /// @param pubkey — new owner public key (uint256)
    function setPubkey(uint256 pubkey) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        require(pubkey != 0, ERR_ZERO_PUBKEY);
        ensureBalance();
        _ownerPubkey = pubkey;
    }

    /// @notice Sends a plain transfer to the given address from the bridge.
    ///         Used to trigger Transaction contracts deployed by the bridge's USDC wallet
    ///         (e.g. SET_SUBSCRIBER_TYPE). Only callable by the owner.
    /// @param txAddr — address of the Transaction contract to trigger
    function triggerTransaction(address txAddr) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        ensureBalance();
        txAddr.transfer({value: 1 vmshell, bounce: true, flag: 1});
    }

    // ========================================================
    // On-chain code upgrade (owner only)
    // ========================================================

    /// @notice Upgrades the contract code on-chain. Only callable by the owner.
    ///         Remains available after the anchor flip. This migration clears
    ///         the owner-anchor flags and recorded clients; operators must
    ///         restore configuration and repeat each intended chain's flip.
    /// @param newcode — new contract code TvmCell
    /// @param userCell — reserved passthrough for future upgrade payloads.
    ///        Currently unused (the new code receives a cell built purely
    ///        from snapshot of current storage). Future upgrades can read
    ///        this slot once `onCodeUpgrade` is extended; today it lets the
    ///        ABI stay stable.
    function updateCode(TvmCell newcode, TvmCell userCell) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        ensureBalance();
        TvmCell migrationCell = abi.encode(
            _ownerPubkey, _usdcWallet, _totalMinted, _mintNonce, _mintAccumulatorNonce,
            _totalMintedBridgeByToken, _totalBurnedBridgeByToken, _depositVoucherCode,
            userCell, _outstandingByChain, _messages
        );
        tvm.commit();
        tvm.setcode(newcode);
        tvm.setCurrentCode(newcode);
        onCodeUpgrade(migrationCell);
    }

    /// @notice Initializes state after code upgrade. Resets all storage and re-initializes
    ///         from the provided cell. Called by UpdateZeroContract (zerostate) and updateCode().
    /// @param cell — ABI-encoded tuple:
    ///                 (uint256 pubkey,
    ///                  address usdcWallet,
    ///                  uint128 totalMinted,
    ///                  uint64  mintNonce,
    ///                  uint64  mintAccumulatorNonce,
    ///                  mapping(uint32 => uint128) totalMintedBridgeByToken,
    ///                  mapping(uint32 => uint128) totalBurnedBridgeByToken,
    ///                  TvmCell depositVoucherCode,
    ///                  TvmCell userCell,
    ///                  mapping(uint256 => uint128) outstandingByChain,
    ///                  mapping(uint32 => mapping(uint256 => bool)) messages)
    ///         `depositVoucherCode` is the voucher code carried by the
    ///         zerostate path. `userCell` is `updateCode`'s passthrough: on a
    ///         code-bumping on-chain upgrade it carries the INTENDED NEW voucher
    ///         code (so a code bump can swap the voucher logic atomically — the
    ///         only way to rotate `_depositVoucherCode` post-deploy, per B2); it
    ///         is empty on the zerostate path. When non-empty it takes
    ///         precedence over `depositVoucherCode`.
    ///
    ///         `_trustedL1Bridge` is deliberately absent from the tuple. After
    ///         an upgrade the allowlist starts empty (deposits fail closed)
    ///         until the owner re-seeds it via `setTrustedL1Bridge`.
    ///         This format requires a fresh 1.13.0 deployment; it does not
    ///         decode the previous nine-field upgrade tuple.
    function onCodeUpgrade(TvmCell cell) private {
        tvm.accept();
        tvm.resetStorage();
        (uint256 pubkey,
         address usdcWallet,
         uint128 totalMinted,
         uint64  mintNonce,
         uint64  mintAccumulatorNonce,
         mapping(uint32 => uint128) totalMintedBridgeByToken,
         mapping(uint32 => uint128) totalBurnedBridgeByToken,
         TvmCell depositVoucherCode,
         TvmCell userCell,
         mapping(uint256 => uint128) outstandingByChain,
         mapping(uint32 => mapping(uint256 => bool)) messages)
            = abi.decode(cell, (uint256, address, uint128, uint64, uint64,
                                mapping(uint32 => uint128), mapping(uint32 => uint128),
                                TvmCell, TvmCell, mapping(uint256 => uint128),
                                mapping(uint32 => mapping(uint256 => bool))));
        require(pubkey != 0, ERR_ZERO_PUBKEY);
        _ownerPubkey = pubkey;
        _usdcWallet = usdcWallet;
        _totalMinted = totalMinted;
        _mintNonce = mintNonce;
        _mintAccumulatorNonce = mintAccumulatorNonce;
        _totalBurnedBridgeByToken = totalBurnedBridgeByToken;
        _totalMintedBridgeByToken = totalMintedBridgeByToken;
        _outstandingByChain = outstandingByChain;
        _messages = messages;
        _depositVoucherCode = userCell.toSlice().empty() ? depositVoucherCode : userCell;
        // `_lightClients`, `_lightClientL1ChainId` and `_ownerAnchorsDisabled`
        // are absent from the tuple: after an upgrade the owner writer is
        // on for every chain until the owner re-deploys the light client
        // and flips L1 again.
    }

    // ========================================================
    // Getters
    // ========================================================

    /// @notice Returns the TIP-3 USDC TokenWallet address used for the bridge.
    function getUsdcWallet() external view returns (address) {
        return _usdcWallet;
    }

    /// @notice Returns the owner public key.
    function getOwnerPubkey() external view returns (uint256) {
        return _ownerPubkey;
    }

    /// @notice Returns ECC[3] counted by `mintAndSend`, `mintAndSendAccumulator`,
    ///         and the migration path. `confirmDeposit` and `mintAndSendBridged`
    ///         mint ECC[3] without incrementing this; bridged inventory is
    ///         `getTotalBridged`.
    function getTotalMinted() external view returns (uint128) {
        return _totalMinted;
    }

    /// @notice Returns total ECC minted/burned via the cross-chain bridge path
    ///         for a specific tokenId. `initiateWithdrawal` uses these as an
    ///         inventory cap (`ERR_BURN_EXCEEDS_BRIDGED` when burned would
    ///         exceed minted).
    function getTotalBridged(uint32 tokenId) external view returns (uint128 minted, uint128 burned) {
        return (_totalMintedBridgeByToken[tokenId], _totalBurnedBridgeByToken[tokenId]);
    }

    function getDepositLimits(uint256 chainId) external view returns (
        uint128 maxTransfer, uint128 maxTvl, uint128 outstanding
    ) {
        return (MAX_TRANSFER_AMOUNT, MAX_BRIDGE_TVL, _outstandingByChain[chainId]);
    }

    /// @notice Returns the hash of the currently installed DepositVoucher code.
    function getDepositVoucherCodeHash() external view returns (uint256) {
        return tvm.hash(_depositVoucherCode);
    }

    /// @notice Returns current nonces for double-spend protection.
    function getNonces() external view returns (uint64 mintNonce, uint64 mintAccumulatorNonce) {
        return (_mintNonce, _mintAccumulatorNonce);
    }

    /// @notice Returns contract version and name.
    function getVersion() external pure returns (string, string) {
        return (version, "eccUSDCBridge");
    }

    // ========================================================
    // Halo2 public-inputs assembly (consumer side of opcode 0xC7 0x4A)
    // ========================================================

    /// @dev Read the deposit fields out of the PROVEN public-inputs blob (the
    ///      contract verified the proof over this exact blob, so every value
    ///      here is proof-bound). Layout = 12 × 32-byte LE Fr; offsets per the
    ///      final ETH-deposit circuit: 0=deposit_id, 1=sender, 2=amount,
    ///      3=contract, 4=chain_id, 5..6=dapp_id(hi..lo),
    ///      7..8=an_account(hi..lo), 9..10=block hash halves, 11=promise commit
    ///      (9..11 ignored on the AN side). Only the first 9 Fr are needed.
    function _parsePublicInputs(bytes publicInputs) private pure returns (DepositPI f) {
        TvmSlice s = TvmSlice(publicInputs);
        uint256[] fr;
        for (uint k = 0; k < 9; k++) {
            uint256 v = 0;
            for (uint i = 0; i < 32; i++) {
                if (s.bits() < 8) { s = s.loadRef().toSlice(); }
                v |= (uint256(uint8(s.loadUint(8))) << (8 * i));   // little-endian
            }
            fr.push(v);
        }
        require(fr[2] <= uint256(type(uint64).max), ERR_OVERFLOW);
        // The circuit splits the 256-bit AN account into two 16-byte halves
        // (fr[7]=high, fr[8]=low); reassemble those. dapp_id is fr[5]/fr[6]
        // on the circuit side; this function pins dappId to 0 and never reads
        // those halves. The workchain concept is retired on AN, so the
        // recipient always lives in workchain 0 (see confirmDeposit's
        // makeAddrStd).
        f.depositId    = fr[0];
        f.amount       = uint128(fr[2]);
        f.contractAddr = fr[3];
        f.chainId      = fr[4];
        // Deposits into AN always land in dapp 0, so the dapp halves carried by
        // the circuit (fr[5]=high, fr[6]=low) are not used. Pinning the field to
        // 0 keeps the deposit identity — and therefore the DepositVoucher
        // address — independent of what the L1 side reports.
        f.dappId       = 0;
        f.anAccount    = (fr[7] << 128) | fr[8];
    }

    /// @dev Read ONLY the block-hash halves (Fr #9 = hi, #10 = lo) out of the
    ///      same proven blob, and recombine them the way the circuit binds
    ///      them: `hi << 128 | lo`. Kept separate from `_parsePublicInputs` so
    ///      the pre-accept path does not pay for the extra two field elements.
    function _parseBlockHash(bytes publicInputs) private pure returns (uint256) {
        TvmSlice s = TvmSlice(publicInputs);
        uint256 hi = 0;
        uint256 lo = 0;
        for (uint k = 0; k < 11; k++) {
            for (uint i = 0; i < 32; i++) {
                if (s.bits() < 8) { s = s.loadRef().toSlice(); }
                uint256 b = uint256(uint8(s.loadUint(8)));   // little-endian
                if (k == 9) {
                    hi |= b << (8 * i);
                } else if (k == 10) {
                    lo |= b << (8 * i);
                }
            }
        }
        return (hi << 128) | lo;
    }
}
