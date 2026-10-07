pragma gosh-solidity >=0.80;
pragma AbiHeader expire;
pragma AbiHeader pubkey;

import "./modifiers/modifiers.sol";
import "./DepositVoucher.sol";
import "./EthBeaconLightClient.sol";
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
///            `mintAndSend` / `mintAndSendAccumulator` — owner-key (pubkey)
///            mints ECC[3] USDC and dispatches to recipient / Accumulator
///            buyShellFor. Nonce-protected (`_mintNonce`,
///            `_mintAccumulatorNonce`) against off-chain replay of signed
///            mint requests.
///
///         3. Cross-chain bridge — USDC-only (tokenId == USDC_ECC_ID)
///            `initiateWithdrawal` (burn ECC + emit `WithdrawalInitiated`) and
///            `finalizeDeposit` / `confirmDeposit` (verify proof, deploy
///            deterministic `DepositVoucher` for anti-replay, mint ECC).
///            The destination chain of a withdrawal is opaque; the SOURCE of a
///            deposit is proof-bound (chainId + emitting contract) and gated by
///            the owner-managed `_trustedL1Bridge` allowlist.
///            Counters: per-tokenId `_totalMintedBridgeByToken` /
///            `_totalBurnedBridgeByToken` (mapping kept for forward-compat).
///
///         Deployed at fixed address in zerostate.
contract eccUSDCBridge is eccUSDCBridgeModifiers, ISubscriber {
    string constant version = "1.5.0";

    event UsdcMigrated(address from, uint128 value);
    event UsdcMinted(address recipient, uint128 value);
    event WithdrawalInitiated(
        uint256 dstChainId,
        bytes recipient,
        uint128 amount,
        uint32 tokenId,
        address sender
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

    // Cross-chain bridge accounting per tokenId (any external chain; independent
    // of stripe `_totalMinted`). No invariant enforced between minted/burned —
    // intra-AN ECC distribution can outpace deposits, so on-AN withdrawal can
    // exceed historical deposits for the same token. The split is for
    // observability / per-token analytics, not for on-chain checks.
    mapping(uint32 => uint128) _totalMintedBridgeByToken;
    mapping(uint32 => uint128) _totalBurnedBridgeByToken;

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
    // this is NOT threaded through the `onCodeUpgrade` migration cell (the
    // tuple shape stays fixed across code generations), so after a code upgrade
    // the anchors start empty and have to be re-established.
    mapping(uint256 => mapping(uint256 => bool)) _acceptedBlockHash;

    // Code of the beacon light client (`EthBeaconLightClient`), the only
    // non-owner writer of `_acceptedBlockHash`. The bridge is the sole account
    // that may deploy it, and its address follows from this code, so there is
    // no address to set and none to spoof. Empty = no light client yet; like
    // `_trustedL1Bridge` it is not carried through `onCodeUpgrade`.
    TvmCell _lightClientCode;

    // Derived from `_lightClientCode` when it is installed, never set directly.
    address _lightClient;

    // Whether the owner may still admit anchors directly. True while
    // bootstrapping; `disableOwnerAnchors()` clears it permanently, which is
    // the step that turns "the owner key" into "the light-client proof".
    bool _ownerAnchorsEnabled = true;

    // Owner-operated stop of the cross-chain lane: `finalizeDeposit` and
    // `initiateWithdrawal` refuse while it is set. Like `_trustedL1Bridge` and
    // `_acceptedBlockHash` it is NOT carried through `onCodeUpgrade` (the tuple
    // shape stays fixed across code generations), so an upgraded bridge starts
    // unpaused — with an empty allowlist and empty anchors, which hold deposits
    // anyway until the owner re-seeds them.
    bool _paused;

    // ZK verifying key (VkBlob) for the ETH-deposit circuit (12 public
    // inputs, chainId at instance index 4). Keyed on the Hermez Perpetual
    // Powers of Tau SRS (s_g2 = 928fafb3…). Regenerated from
    // deposit-prover `export_deposit_proof_set` on this branch (DEP-04:
    // type 1+2, 2048 B calldata, 2048 B per receipt log, keccak 128).
    // Canonical fixture:
    //   deposit-prover/fixtures/deposit_10proofs/deposit_vk_blob.bin
    // Magic "VKBLOB\x00\x00" + version 2, shape "Rlc". Size and sha256
    // below are rewritten by `scripts/embed_deposit_vk_blob.py` on each
    // rotation — do not edit them by hand.
    // Magic "VKBLOB\x00\x00" + version 2, shape "Rlc". 13071 bytes;
    // sha256 = 435c0f7230be019b4bb71c22944effaba51138eac35db80ae1dda31a79efd93c.
    // To rotate: regenerate the fixture above with deposit-prover's
    // `export_vk_blob`, then rewrite the constant below with
    //   scripts/embed_deposit_vk_blob.py contracts/an/exchange/eccUSDCBridge.sol
    // — never by hand; CI runs the same script with --check. The fixtures
    // under tests/exchange/fixtures follow the new blob.
    bytes constant VK_BLOB =
        
        
        
        hex"564b424c4f4200000200010000000000ed0000007b22726c63223a7b2262617365223a7b226b223a31382c226e756d5f6164766963655f7065725f70"
        hex"68617365223a5b34362c34345d2c226e756d5f6669786564223a312c226e756d5f6c6f6f6b75705f6164766963655f7065725f7068617365223a5b31"
        hex"2c312c305d2c226c6f6f6b75705f62697473223a382c226e756d5f696e7374616e63655f636f6c756d6e73223a317d2c226e756d5f726c635f636f6c"
        hex"756d6e73223a357d2c226b656363616b223a7b22636f6d705f6c6f616465725f706172616d73223a7b226d61785f686569676874223a302c22736861"
        hex"72645f63617073223a5b3132385d7d7d7d0a3200000212000000006300000058c338696a199ecb26464c9bdedd6f46e6fa0c9974d91b9fcaa627c329"
        hex"dff21a4e7b0e1f40caf730f9cb23b5583598194673b2bc3c7f181c20266ff8b4d3cf262eb5109c3289abd5dd6068b2e4503b28f0da9337bc82100ce0"
        hex"e15337c15e3d0cca24be1a8f796ccad03c36cdde6dfca6935bf1989e90150d7be3e451bac1bb0c3e95f55881ae181828bca4d5c448b310809dff7892"
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
        hex"84261ffac9a40273033a83d14278cd2be543f40f6e8c07f229144d0c76266b26525c68e5b464b7a35baff3c4cd8876860f273555124f11d10725e68f"
        hex"d81e1d8d6214aaa00551938f7f34079e16f85e968ea7a0a8968c25123b05d8edc4e4f25cbcbda25945766c021d47d656e5c4c988d4d564e4c0d529ea"
        hex"ee816e5a0095d9ead536192374b718afc81874918e6b9e89316944cf37f60151b87a2f341b049b63f8e6d88cc98ac15452a4c4abf7a0d0d1f0a19dca"
        hex"f7f9297f179c0e3ef6894112a0f905131f532cbb3da6277ea5d5f40abc4ae7abcd9c25711e33c083cc409f3aa7c17727983ec7f9b895c0e38a476588"
        hex"827815da2d021d1643d5a55841065b78a2d8491e78178c3087d1fd71053b206585a3715b8602277684084f91c8e7702e0043abf02b6eb222bd651170"
        hex"b025a27e008cc31423f20d948e69a4491dec8a0bea681c96a139b11108c25d584fdbae7053880a22d2a7180c8af11406a99ecd5d4830422e21ab47d5"
        hex"e8a1ee90ea8db1cd9c975a86446f1370594b24fb5b94497b97dc1d32bee8b6fe6edf6895dbf4458ef169d87be9f715df86cd721975b7ce15d98fd0c2"
        hex"2c6b90f58b77156409fc907d6e39d35a647b0c2fac6ea3e95808a31b5f22b0d7d1b1f6c57de74ee31848444d14e4259ceca90b983c910153fb861095"
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
        hex"77530d35e30e1b3d92719ecfed99a60e0409a768270a27a77936b19249ff52c9ad47b72855900d4ff8d5339a1b4d32dae2ac7ef5a334e86b2cc1531d"
        hex"a7832974b208616f11f61477769866b5245da7db4a3996909fc7191bb77a262584209f6d12a6f315875318f90d3c82c8e3726b617782964b0c6d5ccf"
        hex"f7a8cad6275a0c27ebd83b5fc4ba093e9ed67ac40454998568f26db880cde0506a1a961a952d021d222450f3cceb1c19c9bffae66ab757783261a72b"
        hex"ee496613ce11bd33acde9a00fc649ca9e8120e54a4c280b68062953be8357771215a1ee257286bf60f23cc963ee40feeb4552a2f97c7f77bccfb41b1"
        hex"1377ca4ecc290f0d318c4ec7926530168add94603f0b0caef62ffbb14bac7876f835cc98a5afe7310125dc961bb3c5412c7d00ef19191fdb5812f977"
        hex"f08666c3a71f02bd19460c1c7c3ba4e3b086b2bae3f8667861e621ad78dec8f77c8d5d90de8af113f652766e170af9c064abc9884c94a749e3ce2e59"
        hex"22d00a7c29dac6456000e7f9e7bd3a170e8869a951cce817f8543a7e289714c64c52befff5c7c3a536cecd7919508c0f972e9805f0b149289ccf3ad8"
        hex"bb6114777e2918ddcb24d9039ef2ef71912274e20703a528773e24de3818475b26d8257ce5e91d85ab9cc8038185609ef194c2b162ab7838463c2035"
        hex"04387849dc010f26ceeca5a8819512dc26be29ac75816a4c5171dd4f268b13e95a70a1d1c4001e1b2159ad6fa8d3a32b96d55260e8681dc274911afe"
        hex"08812ecf7fef7642cc4c17a3cb979f74c82543aab57f10ff3cffc772e6c3cf89567d663eeb76765097af0dde61672e9831272cac34ae7e893ab88d9b"
        hex"4abdfcfd93a07b50f7e4792a523f26d4623d45171d711e90c3c2511e0c035e3fb9c58cef960e6e509dcc18d3f06c25a061407752ca6df979dd80fd1d"
        hex"46fc9617a9f3c965440ecf1e254f81ad66702a281f1bb7e64b22ac8f50e38784ff19c32a10e05ea8a58ffa297415cb3c9b720d12c1fc75c1f55aefc8"
        hex"836c93c22ea32eff0e902d7ebc52bfa30ece20b4260816d029cb5d84d0aaa3fc735bcd743ee2af3e1aba03feceeed928c1c71ac14ec02fea416c9591"
        hex"026099029b9a942736fd83d108358b00a718193ca83b767a4058271d2a69a1601c197de486dca412aa4f935f697bf09e4e52a4205efa591034c20cf9"
        hex"be6a5fa397c85103f66377376f5ed488303d7649cefe49b68f6d9d6914e922e2a0ea13f8acd7c3a02df4ccfc89d7ea260ed0e706eee819f7166d7b85"
        hex"c4602e479808271e10ffb881b49801cc41c85b2a05b073ee8352166c65cf46d31efe1168b275673f0ec0f8f2cdbbf56e073bde6f4fc793b7ca7334bf"
        hex"18a65fc22e5d15e3233d30db50c731f492b89c5ed3868e1a1682ac646490c32e5bc5dc0e7e1206076d3b73308444214bd6c0f04d52586d15a438607c"
        hex"c4457d6fbd886fcc3cbb2c6d12d90c5fde8fc55c277abd3ec5c921d65508990ec4a820939cfcdd028c40246cfbf5d04d5c4116259993b92dfa492c35"
        hex"9b105f25b8f64359fba41941eb541e976b4a33decca5ea60351f0fa5e91bc76036feaf7466c39ee76076541426402746d234868875a13f37ca016e8f"
        hex"a5e0e3a25e7743b8f90eb07cb57cd3842547271bda27e0dc5e6099216ecd59eb8bffc52352c5477784f441575aa08f4cc32426aa4a1d8417bf0bc281"
        hex"0037627262c95d2f857391d1645b40565ae814c941312c6f78b7d83bb399427c9dff99ca12d29c5facd2f2f037230aba667a96a315551ab2fce365e6"
        hex"f60c870cc9c9ffcc2a7d3a102c4ac5be0831de3a51ca886bd7dd28c9d9ecbe883dcd1a619dbfde90d2148061e7c22188805e62a7ab3f8defbff41fd3"
        hex"6349eeb9b97c029015f5600673e2dd1ed9d524127b13e160d2a0032df7ae1438aaa9a730fe4ef427da550107cbba93677e7956f6b66764d6975d2aa3"
        hex"3c742c8baa797c320bff8ec705aef3219a7b34778c636cdfd41618f111379cb64d4c0e783b6e8964173259bc652b6c6ab30d53094eb03ab6c01f7185"
        hex"252f1e5311740e704a4ef42b3b16cdfbc770a3dd4b01ef5613b949a38b9cfeb66e92b3e921f921f74dab4dbcc29d2b0c7f6936ed63bc3100db000121"
        hex"b9bc406f24c419e9645601f971c85cdf6ffbad037e55bdae95f39092b678df5183d2bb7709396c0ef864045df9fc62da1a80fc038076c2646d1d01da"
        hex"594d497c0b4edb5ab4b3829632f7251b8ad96ff9f822c421005e5ac35104070048c96879d74f77324c5ebb2d2fd92a2adc7e6eeeddb6615d96299600"
        hex"01e4b1cbadd2cab7836bc77dc566af69075b1cdc14912751454ec24cc3bef6396a2994ab3213cce0fd8215a77e38db23fb5b245cc04f59a90131b428"
        hex"54971913191a4eeafc43d45915710de003e085457e891c5d5316edfc365d89343c5515aa1df7a6950cb60929a75d4061fc2453260dbe199e3dfe54b0"
        hex"978bb2b343909d712c117ae4555309c8d9c6407fcefe4921b9142acef6eefb573ce948d9723c61bd2e585e49c138682f3a794fe0683c6dfbb04d2117"
        hex"427d212a10abf199188b035fc13a68ff51e2ba9dad2caf597bd4a4b31fa820a793ef730da60105b65309b25c8ca3954498f35ec000773376f0031d85"
        hex"4d581204e3c980fd1d3371d31117be9aa4049ca2f92466501fc6f876d7a25f7ab4781708f0edf22979a852e1d9ddfc63e24e635dabab6146f438cf53"
        hex"929226ad97fb04754388e3024e7f103868dd40e79f096ac8975a54f95c9ce14678c0848bab221b43ea534360896ed291c748554b1c142ba01cbc4534"
        hex"1947c4978716cddcaf2f30dca04a93f018264346a011091070f21cfb7147203ee60148911e20e89c2ffc2b43d7393c521b73a032a348f2401ed22f01"
        hex"33e1050b6c8edd80a2a93f52279e0450a01b1910f2958e908c95900225f68f6a2290b556f9cd703d358afa54ecfd244fb58a4a337c5c3979e7fce8a7"
        hex"ff4f531473cdd73d0fe218a3f625748b234f14524048e0742d1c572bf32717b7ea90d6e8271d6fb8d5b888ce29285c0134d318e903515a76b7c971e7"
        hex"3117e64a1cc9acbab0a6555ab5d1486a03bc2447f6a0219baa8538b298204c67bfa147c431c38ac81bc39d1e29fc0e04ca7a5db90316007b75eba699"
        hex"83d747d68f7a16e2fc0785442357aae1b30d724cf0a272e92dae12cb353c4ca6a0d3efe4ad3cb38b87554248e3c16b3a342bfb7813ad70fa2ffc2c29"
        hex"60f80c637c059f97ecc7f8e85024c33573adde98574d68e7afd11686dfd715257ac50519ff50595a163366315351772e2cdca17a8241cfa220495058"
        hex"9c6903069649c09accaa4780192190b7a474561c86fdc992e9af07803e6428ae478605ec4ea323bb7e2449c5e0591677e7f3df0b5f2a20c047676918"
        hex"41f25b152d761be8022efe73258146b543ffac5f2fa58864658d822a289121ff33a309ee949e1d9550fe5110488c5a915291e5d1e601cbd95b830493"
        hex"90cb3bd765522d62895b18cb9bd6b7a6d18a2e70fad96db884434deb04d906854e998bdef557b033eac30995bd69434c6e0997ec5a239166be239ece"
        hex"a814a234a7267fa62c4a4947ab511526b1010c1193c7e364b446e8a4f5434be3ea60dfe977a37d98a7b52c9d4e4f2d54a0c7d78ed818facf9826dde3"
        hex"bd55e099b7d76498f9395bd6d38caeb5d1a22f655ec2956da6abd4a92c91998c01932359859f111c9a2e2d5270f18a3ca44d073e02bfaca6b14e938f"
        hex"76ed0e713ac22df2de3c7dda898a5da79c2f2203acd321d28af613222e98a97a3f39473531389cc04f52f4090b249da8ed72e994110714b1356e47c9"
        hex"c4bddba8ca91ccb1a45dec735fd43a38d3e0fe647b7fca59ed2924a3859b068d94fd11337df575fbf755a4f95fe96711d8f57f0a35969f6b9cb42781"
        hex"9c931d04817a829d8b8a7112a8b969be69f0232de82419cadcac9c53410e0a90e7c2ac5e12586247b172371264ca8c6f9eb32ff8501a7580f4dda61e"
        hex"3a7c2f62b0206bbafee5242d4da0f1e9159a430494081bc91f246f73d0c1fcc83a6e22659a37af05ee1155afe0faa2e93f01cbdac66178356908a338"
        hex"2c1cb84450c329387ccabd4b438bd5f14d47fb552f5161af7ce44ccac8370c32914ebb500e961d30024d895963aa732698b446423f8c8b4a91d7b979"
        hex"8dda09f6cb7394546fe826eb32663b6f3f220fe30022234b053b0a2bc9703a0d837ae93dd2dcb5e2c4682da01f93edf508914a41df954482110521e8"
        hex"f9ce26a048ad53b1798202a41ddf1c55116fc79d38814b6a130657ac956febd1c1314513b1dfec1296fafe833b750419eb6d1a242e9f22d74c11f3d1"
        hex"35f8a92ed8f43104f5f8c7bc92658b8fd8491b87a3b9d7bd026cde5b2f332a20f19d032aa21b1e337cf2aab865626fde592e2614275e6c977fae8201"
        hex"0c23a81edaaf5c4eb424701b14311360809204f30c4c0d19c75c7feec0388464fe696388ad2fe2b777ed97532a2dc102935f7fa5c24d29d8de0b7d84"
        hex"6925c18a790b27976a117d9ac0793bc85ad4f5943e922afdb1eb01e309c049d5b1251cd695a128afca1c75198378ed8f578c7fb17209d9ca38ff11e6"
        hex"c6516ae7d357bfcf81c617d753437d7a8d238d16f2eaa8b9e98aa235508b26e1f47b308b5eb142ed5569b84be1058191c0cda12fc79e8ef83ed184b5"
        hex"67ac120cb78d4bdaf8b1cc09160d3fa0377bfc6b1f2cfbcb003ce4df9d118a14b4f71cd29ce57b842a39be9f4b7c56cac1c69eaa9da18214b76768bd"
        hex"7f65fdd0c320135e6e40a20879047b7325f5ac23766b0a86035d9c539234d7b2aef2e022e2041a009835d77e05016ac62d7f8f22a79258f4aa23e31b"
        hex"4893902c6f1be545f23a1dba578e3f838166850e73039b3ae147e32f30444a8573c0adbf15cead2d6a8127022331aa1c0199055fa15f9851337c5b53"
        hex"333e59b967a42fe4aa76bab0aeaa0a67cadb6c9eaabdd6269aff8171055a5f436e4eda1bf3851b8bd4820ab050081acbf34e0e62b221e8364c36fca9"
        hex"4f2c86990ccb2cb03faee10c2e1eef69b2701a435da78ece60b9cb75e5b580578d089ab56dad740a02582faf40afff2c87162b64b87406dc182cee61"
        hex"d330071a3c33a71ffc4acf6b55aaf3cecd40fc922ace1633fbcb5fbd70e94f5ce63ec4d1ae4ab80233d14fb118970cb9b0404fa594a60141498ec1ca"
        hex"8ce5118f520d50a691f5fa302b4d765c39febcb0e511c5e384f02ddf4ea7e6d985d720222bed1d5825dc8e8d879671537859cf99f1ff7f2e91e10db7"
        hex"31483f83d7464b82915f21bab4f2b4b32657d094702cf1f88b4d6dcc10d02e55e1708f472ee01844c9ccc5d22192f2fafa057216eff5b461b5185076"
        hex"39182e16d55a8c65b37e0d907d308243e4ae7b9a88c6c9c9a25a6f24129300e821f224e04463b08c6fc3cbe67ff26d8cce4ec4dd0cb364b70a1a5acb"
        hex"a3ba48907d69089fee28e54374831a792af431a472a4ed56553226656162ca549dd4fdd201300a7c6acd1e6b32bfe94895430fa5d6d4cd6c5c676ca3"
        hex"1a31bd4892f5eb804b6604b78bbe827f788a52a731e0b914cc2c2fd526ef7455147fed59a4d3c0f618a62d1c8f62fb565481184a380e72df4782daab"
        hex"3d6fdc4f94eb851b1277786570e70a355e891c55dbaf51f3f7d9d6160b5143b221783dcad0482924aaed094e0a2d0e72abe39ac884b6a71c639cb7ba"
        hex"70178ccd319bdcd6c43a2240265ca1f5ba1a1075269676c66e9e25394a75683dc80fa3c7d378a2a7349ae4c295bfbb5e1d0b06132fbdaa32994ed274"
        hex"5e27f75ecefdd110c25b1a4ae227bb823443b92d28582530066726cba224ae4768c90f097600102a8c464f782e78202933c3bfb436c600cdaadcf17c"
        hex"b23c2b813bfb76085e20163cc8f2ccf4533d925f5ffc7801fddd04a8e4901c8f79db52ff2bbcd79a648eb266d6c4c8238c5a4e92b99ca9843a162dfa"
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
        hex"ec91fafa4b9aabe0a77b7556f35f05660655fb7966131418f5a039d7af0dba769924958ec478b0273fbee110cbcc51874fc63896a8be07f149a83ebc"
        hex"26ecec12ed0b680148526ea4102b7c40ca94ca6906b5c746211027be93cd80acd1fd74188ef766a41c4d236bbcd98fc2ca26b59dda62c3befe001d18"
        hex"08c34ec574e25709277a488bcc9516374243e2bc2dbd208f88e3ff2b49cf2bca0103e6f29a8f3fab94145b39eb905b2d3c9cd5dcd11822e61523ffda"
        hex"c1910920f85828640c9bb8e9486d62cf25271c4ec79eb80b1582ff57334686b1eca02c25163dc9bc1966ea782ab825cd9bcf69d870cc8cdee555176c"
        hex"d10b2cab7deb2f598be857840f8b0c60932758ad3ab3830de294ae90291f142692f7f80bac571a39e3cfeccb2e642f683ccb3fe789fed0ac14295cbf"
        hex"3d49d374baa5862d48e515ee64045e4e568794a677ff5a8e4d81090165c8749a3b2fb4dc39843d099cf107c198f7ec25e5f82c42becf31a252f7cd48"
        hex"280b5d037dea16a47e375313233b00dc776aed8bc65696e79202059f5e2f852782c43b18da1672a3c4046eb5bcbf26084804a0014265730e84056a5c"
        hex"fce1958d1035bf15225bff39f6dde45b87e401edfa273111dc50e4761c85c92342c60bfa05f7b4345c5aa51fcec7ce48b2681ed6ba15e2f67ae83afe"
        hex"91c41efae1b76fd6a50244006ae9c62df39bd27bc65520dbfdae7a547258e99ac677908d92f7381b05386ae130cc3eb13cbe55a2ce5922d0c44cb4c0"
        hex"abaf58a94fd9afe7826d910a83388479acfd7dfa3757f1402a670be2201cfa428e61ab6a8e90d3924cd48111e69bd1bdcc43b2a214443f8ae05c1b0a"
        hex"24aeefd69419cd0f114898821dd173456cb399ab0f8af9a2dc6f1ee1e064082434c216721b29caecb9af424860a437984cfed95350ee64762e8fd0d9"
        hex"49ff26ae0f932ae5cf0d0a88dc76c330d8763f3471ab6113d43c5fdc482d9a90a0f22d24228e4582645a4acb7badebf14e7dcf935d2394dd80fd7307"
        hex"348ab5b79703146cacaa1b3358a7579f251a72f14927dd460f62f1d699acbfa1fa001db3fbd50c8363e4703a8709c4e90317909a914f3c145f03de43"
        hex"ca0e5cf0ef0c3d0bd3490929de627045f9efcebfdf71bf6bc0e941079aa3c90f60b319c3c5ad4247a196072dc656883d0e34ba472bf839964a9bef01"
        hex"9756568a3586053890ed8bf94db50cc702ecebf55996219aee81fe3875bd44f326c842f2a6d33fa4a7e5c7b67bcb0e82d46e8305ff9f9022ce44b944"
        hex"b00f1d04293f128eee273900919310ffba3401f613010feea93ab18bfead634b3f0c85b45f974d2882f2c176f10b4b2b6aeb1b829058f4b05604c209"
        hex"a226a35ea4f1130468150d92c2167ae14cc4698a15dc00bd7413bfa3b12947dee919503fd03cc7f53d3e3c83c2b775ecc49a0f7a1df0133bba17f401"
        hex"f4b8b74fad4fcd582b403da40df9e9eaf587ad829844814b21252681867cc2ba295c607e48d86f84ba289122cc18bcc9d4758664d78cb3c1416e018e"
        hex"4a2bbf39cd75262d286dfc295a63629105ec51297fdc40e70473280c31df1c21f4cd8178df9f24a09653effa15d6b9380ae30f72fac78bae7a2aa154"
        hex"5b042a38d2583972c4c44b799a858a616c7a5cd86dd696ad80892f23aab1cc881ee0123bbeb57580ffbe2d44ceff83de867e34f911c74dde2277aa43"
        hex"ae62f618af82260c43168ccd3e11d1832878e921759f4e29bffe4a5186d8e3b2a610f4b718bf175ce27493359d2058ce81b2c6b7b89521766a0a4ac1"
        hex"bd7646d8a0e661b2797f11fa229ebf64d96637e93ba918c2b53ecc837631a849114594f4f307849ade23205677fba2e2394b694b38d15ac87be9e639"
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
    ///         Only callable by the owner (by public key).
    /// @param recipient — address to receive the minted ECC[3] USDC
    /// @param value — amount of ECC[3] USDC to mint and send (in micro-USDC)
    function mintAndSend(address recipient, uint128 value, uint64 nonce) public onlyOwnerPubkey(_ownerPubkey) accept {
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
    // Mint USDC and send to Accumulator for a buyer
    // ========================================================

    /// @notice Mints ECC[3] USDC and sends it to the Accumulator's buyShellFor,
    ///         which will process the purchase and send ECC[2] Shell to the buyer.
    /// @param buyer — address to receive Shell from the Accumulator
    /// @param value — amount of ECC[3] USDC to mint (in micro-USDC)
    function mintAndSendAccumulator(address buyer, uint128 value, uint64 nonce) public onlyOwnerPubkey(_ownerPubkey) accept {
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
        tvm.accept();
        ensureBalance();
        require(recipient.length > 0, ERR_RECIPIENT_EMPTY);
        require(recipient.length <= 64, ERR_RECIPIENT_TOO_LONG);
        require(!_isZeroRecipient(recipient), ERR_ZERO_RECIPIENT);

        mapping(uint32 => varuint32) currencies = msg.currencies;
        uint32[] keys = currencies.keys();
        require(keys.length >= 1, ERR_NO_ECC);
        require(keys.length == 1, ERR_MULTIPLE_ECC);

        uint32 tokenId = keys[0];
        require(tokenId == USDC_ECC_ID, ERR_UNSUPPORTED_TOKEN);
        uint128 amount = uint128(currencies[tokenId]);
        require(amount > 0, ERR_ZERO_AMOUNT);
        require(amount <= uint128(type(uint64).max), ERR_OVERFLOW);

        gosh.burnecc(uint64(amount), tokenId);
        _totalBurnedBridgeByToken[tokenId] += amount;

        address addrExtern = address.makeAddrExtern(WithdrawalInitiatedEmit, bitCntAddress);
        emit WithdrawalInitiated{dest: addrExtern}(dstChainId, recipient, amount, tokenId, msg.sender);
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
    function setTrustedL1Bridge(uint256 chainId, uint256 l1Bridge, bool allowed) public onlyOwnerPubkey(_ownerPubkey) accept {
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
        public onlyOwnerPubkey(_ownerPubkey) accept
    {
        require(_ownerAnchorsEnabled, ERR_OWNER_ANCHORS_DISABLED);
        ensureBalance();
        if (accepted) {
            _acceptedBlockHash[chainId][blockHash] = true;
        } else {
            delete _acceptedBlockHash[chainId][blockHash];
        }
    }

    /// @notice Installs the code the light client is deployed from. Changing it
    ///         moves the light-client address, so an already deployed one stops
    ///         being recognized as a writer.
    function setLightClientCode(TvmCell code) public onlyOwnerPubkey(_ownerPubkey) accept {
        ensureBalance();
        _lightClientCode = code;
        _lightClient = address.makeAddrStd(0, tvm.hash(abi.encodeStateInit({
            contr: EthBeaconLightClient,
            varInit: {},
            code: code
        })));
    }

    /// @notice Deploys the beacon light client. The bridge is the only account
    ///         allowed to do so: the contract's constructor refuses any other
    ///         sender, so nothing else can occupy that address.
    function deployLightClient(
        uint256 pubkey,
        uint256 l1ChainId,
        uint256 bootstrapCommittee,
        uint64  bootstrapPeriod
    ) public onlyOwnerPubkey(_ownerPubkey) accept {
        require(_lightClient != address(0), ERR_LIGHT_CLIENT_UNSET);
        ensureBalance();
        new EthBeaconLightClient{
            stateInit: abi.encodeStateInit({
                contr: EthBeaconLightClient,
                varInit: {},
                code: _lightClientCode
            }),
            value: 10 vmshell,
            flag: 1
        }(pubkey, l1ChainId, bootstrapCommittee, bootstrapPeriod);
    }

    /// @notice Address the light client is deployed at, derived from its code
    ///         (0 while no code is installed).
    function getLightClient() external view returns (address) {
        return _lightClient;
    }

    /// @notice Permanently gives up the owner's ability to admit anchors
    ///         directly, leaving the light client as the only writer. This is
    ///         the call that turns the trust assumption from "the owner key"
    ///         into "a proof of Ethereum finality".
    /// @dev One-way, with no re-enable. Requires a light client first, so this
    ///      cannot brick the only working writer.
    function disableOwnerAnchors() public onlyOwnerPubkey(_ownerPubkey) accept {
        require(_lightClient != address(0), ERR_LIGHT_CLIENT_UNSET);
        ensureBalance();
        _ownerAnchorsEnabled = false;
    }

    /// @notice Stops and resumes the cross-chain lane: while paused,
    ///         `finalizeDeposit` and `initiateWithdrawal` throw
    ///         `ERR_PAUSED` (231) before doing any work. Reversible, and
    ///         deliberately narrow — the TIP-3 inbound callback, the owner
    ///         mints, `confirmDeposit` and every anchor and allowlist call stay
    ///         available, so a deposit already proven can still be paid out and
    ///         the owner can keep the bridge's configuration current while it
    ///         is stopped.
    function setPaused(bool paused) public onlyOwnerPubkey(_ownerPubkey) accept {
        ensureBalance();
        _paused = paused;
    }

    /// @notice Admits a block hash the light client proved final on the source
    ///         chain. Authorized solely by being the light client this bridge
    ///         — no human asserts canonicality, the proof does.
    function acceptBlockHashFromLightClient(uint256 chainId, uint256 blockHash) public {
        require(_lightClient != address(0) && msg.sender == _lightClient, ERR_INVALID_SENDER);
        tvm.accept();
        ensureBalance();
        _acceptedBlockHash[chainId][blockHash] = true;
    }

    /// @notice Drops a hash the light client has aged out of its window.
    function forgetBlockHashFromLightClient(uint256 chainId, uint256 blockHash) public {
        require(_lightClient != address(0) && msg.sender == _lightClient, ERR_INVALID_SENDER);
        tvm.accept();
        ensureBalance();
        delete _acceptedBlockHash[chainId][blockHash];
    }

    /// @notice True if `blockHash` is admitted as canonical for `chainId`.
    function isAcceptedBlockHash(uint256 chainId, uint256 blockHash) external view returns (bool) {
        return _acceptedBlockHash[chainId][blockHash];
    }

    /// @notice Anchor-path configuration: the light client and whether the
    ///         owner may still admit anchors.
    function getAnchorConfig() external view returns (address lightClient, bool ownerAnchorsEnabled) {
        return (_lightClient, _ownerAnchorsEnabled);
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
    function finalizeDeposit(bytes proof, bytes publicInputs) public view {
        // Cheap parse + sanity BEFORE accept (within the pre-accept gas budget).
        require(!_paused, ERR_PAUSED);
        DepositPI f = _parsePublicInputs(publicInputs);
        require(f.amount > 0, ERR_ZERO_AMOUNT);
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
    ///         account whose constructor was already consumed.
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

        tvm.accept();
        ensureBalance();

        gosh.mintecc(uint64(amount), USDC_ECC_ID);
        _totalMintedBridgeByToken[USDC_ECC_ID] += amount;

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
    function setPubkey(uint256 pubkey) public onlyOwnerPubkey(_ownerPubkey) accept {
        ensureBalance();
        _ownerPubkey = pubkey;
    }

    /// @notice Sends a plain transfer to the given address from the bridge.
    ///         Used to trigger Transaction contracts deployed by the bridge's USDC wallet
    ///         (e.g. SET_SUBSCRIBER_TYPE). Only callable by the owner.
    /// @param txAddr — address of the Transaction contract to trigger
    function triggerTransaction(address txAddr) public view onlyOwnerPubkey(_ownerPubkey) accept {
        ensureBalance();
        txAddr.transfer({value: 1 vmshell, bounce: true, flag: 1});
    }

    // ========================================================
    // On-chain code upgrade (owner only)
    // ========================================================

    /// @notice Upgrades the contract code on-chain. Only callable by the owner.
    /// @param newcode — new contract code TvmCell
    /// @param userCell — reserved passthrough for future upgrade payloads.
    ///        Currently unused (the new code receives a cell built purely
    ///        from snapshot of current storage). Future upgrades can read
    ///        this slot once `onCodeUpgrade` is extended; today it lets the
    ///        ABI stay stable.
    function updateCode(TvmCell newcode, TvmCell userCell) public onlyOwnerPubkey(_ownerPubkey) accept {
        ensureBalance();
        TvmCell migrationCell = abi.encode(
            _ownerPubkey, _usdcWallet, _totalMinted, _mintNonce, _mintAccumulatorNonce,
            _totalMintedBridgeByToken, _totalBurnedBridgeByToken, _depositVoucherCode,
            userCell
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
    ///                  TvmCell userCell)
    ///         `depositVoucherCode` is the voucher code carried by the
    ///         zerostate path. `userCell` is `updateCode`'s passthrough: on a
    ///         code-bumping on-chain upgrade it carries the INTENDED NEW voucher
    ///         code (so a code bump can swap the voucher logic atomically — the
    ///         only way to rotate `_depositVoucherCode` post-deploy, per B2); it
    ///         is empty on the zerostate path. When non-empty it takes
    ///         precedence over `depositVoucherCode`.
    ///
    ///         `_trustedL1Bridge` is deliberately absent from the tuple: the
    ///         encode side may be a PREVIOUS code generation that does not know
    ///         the field (or knows it with a different type), so the tuple shape
    ///         stays fixed across generations. After any upgrade the allowlist
    ///         starts empty (deposits fail closed) until the owner re-seeds it
    ///         via `setTrustedL1Bridge`.
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
         TvmCell userCell)
            = abi.decode(cell, (uint256, address, uint128, uint64, uint64,
                                mapping(uint32 => uint128), mapping(uint32 => uint128),
                                TvmCell, TvmCell));
        _ownerPubkey = pubkey;
        _usdcWallet = usdcWallet;
        _totalMinted = totalMinted;
        _mintNonce = mintNonce;
        _mintAccumulatorNonce = mintAccumulatorNonce;
        _totalMintedBridgeByToken = totalMintedBridgeByToken;
        _totalBurnedBridgeByToken = totalBurnedBridgeByToken;
        _depositVoucherCode = userCell.toSlice().empty() ? depositVoucherCode : userCell;
        _ownerAnchorsEnabled = true;
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

    /// @notice Returns total ECC[3] USDC minted by this contract.
    function getTotalMinted() external view returns (uint128) {
        return _totalMinted;
    }

    /// @notice Returns total ECC minted/burned via the cross-chain bridge path
    ///         for a specific tokenId.
    function getTotalBridged(uint32 tokenId) external view returns (uint128 minted, uint128 burned) {
        return (_totalMintedBridgeByToken[tokenId], _totalBurnedBridgeByToken[tokenId]);
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
        // (fr[6]=high, fr[7]=low), exactly like dapp_id above — reassemble it.
        // The workchain concept is retired on AN, so the recipient always lives
        // in workchain 0 (see confirmDeposit's makeAddrStd).
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
