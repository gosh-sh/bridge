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
    string constant version = "1.6.0";

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
    // inputs, chainId at instance index 4): type 1 and 2 enclosing
    // transactions, 2048 B calldata, 2048 B per receipt log, keccak 128.
    // Keyed on the Hermez Perpetual Powers of Tau SRS (s_g2 = 928fafb3…).
    // Canonical fixture, regenerated with deposit-prover's
    // `export_deposit_proof_set`:
    //   deposit-prover/fixtures/deposit_10proofs/deposit_vk_blob.bin
    // Magic "VKBLOB\x00\x00" + version 2, shape "Rlc". 13071 bytes;
    // sha256 = 4b75cd8351910eec5d9453d33cc3b5920dbe04f5016ef0118bbff23e1ff1b9d6.
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
        hex"84261ffac9a40273033a83d14278cd2be543f40f6e8c078ecbeaef501cd90e9f9d2f44a4a2e12657995eb4cfb216b906e1d00b221de600756da940c6"
        hex"a04864ebebab5337bf45d4c40b50023ed7c2c114149fd97d7810108700d6d7bfa0d0b08868c4ca19eabc741b1f67197e61c217f081e155021bd623dc"
        hex"63de5d7d489367b74803ed135bdcb9665b5e9cc26ca9d078f9ed2c12cc44048e0fd803a8fecf8774b4a7d8bf3c5236601954e66c224c7b67c828e080"
        hex"2d9a2f689f4276425170b524237c4111fa22e51758001244912dde23f0123d34e620154841ca1ccd729bcea9d03dce78dcc8dcca8fce7af26aa83946"
        hex"e87a37e145bf144fb42af3369af52ef8decd08ec5e3416ea02e4c59d017e33057584c493145305be772fe0df83a105c5e55a84e025209b917f3ed094"
        hex"dd7f3141050406c4eb642c7187c656d8d9f090d36148773969d54211a5a598e633fcd87856b7ab5ad7bd03742acae37396c1eb509991c6b85dc55349"
        hex"d7cbb1c4a377b92fb7e7415f34941dc44655e71599ad7ef204be47b40ac582892c5ef5b14139437a87026ed50fe42a30af23885e0d41d1b284d2ecdf"
        hex"80481c73597b940efe43e4d4dd8f718c234a1a331dcd9cc6d886828cc655e34ac7d83c1da456ded7a91a667aa0b2e8f7edaf00983c910153fb861095"
        hex"216c8581742d360343a4369294de9bfe9b3dfefbe05821072e19499fed491d7d84261ffac9a40273033a83d14278cd2be543f40f6e8c07983c910153"
        hex"fb861095216c8581742d360343a4369294de9bfe9b3dfefbe05821072e19499fed491d7d84261ffac9a40273033a83d14278cd2be543f40f6e8c0701"
        hex"38047bef994ac89b33d2d22877e299b4b953e117a595c963f014988a6df023137f3d82569ee78160eda14f861989cccb81b5bd09499e45547d589ba2"
        hex"15a8149b509443b73413c96d2b7a5d9d807bbd86ba854e80aec28a16584f431d803c0affec54111349a452f948aceb8299662094eec949d07d9447c0"
        hex"a0f8d92ff4dd00983c910153fb861095216c8581742d360343a4369294de9bfe9b3dfefbe05821072e19499fed491d7d84261ffac9a40273033a83d1"
        hex"4278cd2be543f40f6e8c07fb19015d036c23ac7ae5fbb8cd34b6e2ffabad433554b8659142e0c39509e91b1bb558765acd065d5fb38a9cf0608aabb2"
        hex"993508eb3d64a2544c808198080f159f3ba06493cc326974c38bd59fd377a4b59a656b47bacbb119edafcd3dc5a51c69397d0d67916779553e90df7e"
        hex"6a4f8ec34fcd6d3ccd2cd6b8186d0af5e830149ebeb63a47961ae3066df689bfe36be7d6e518350fe8282ed894983d2269400cb6334b415e2c3e1431"
        hex"a8e6f5dc991a13c8e683f547504f1fb2e796545015a32ee12cffe63bb7cfd8e3905b60da7e86c2fc1c79f161d6a45b55db9cf82090882be9a11a6553"
        hex"806f296e7b6068fc8921173455286a62a5f2f8f2220ec891207920adfe0cbcf7771ce273bf04739c162ceb53f4ee5e8e20a7a688de80feb9e50f2e97"
        hex"bc8622ff2d936850f0a3a50fa009567c8f48090ed61feebac8330c9555820f2dfec76695ea385ff241b98ba8a7a660e1a1646e5119efd2dedd0458ed"
        hex"2f311e1228e30c3bb9418ca00f426134ef7f9add12e2307265b7b03928d46834bcec27983c910153fb861095216c8581742d360343a4369294de9bfe"
        hex"9b3dfefbe05821072e19499fed491d7d84261ffac9a40273033a83d14278cd2be543f40f6e8c07f9662e66c3d297c1a688c4dea8edc13c023f5be8ba"
        hex"b2c5dd7e581fff800d5f2b3c9fe0dac8ce87b8bb6cd6c01400c8f3a364b7a57e69e5624a14b6af5c1c1022983c910153fb861095216c8581742d3603"
        hex"43a4369294de9bfe9b3dfefbe05821072e19499fed491d7d84261ffac9a40273033a83d14278cd2be543f40f6e8c07632003beb197746edb18b0f3f8"
        hex"afb815498731bc66faced9aaacd9d5c2f28a14e274c30fbeb6126538041b147d66f7fe56ceb94f8235ad8f2247d526b26e731be23f8baee9910c828c"
        hex"36432d97f526c134a3cf88286313947837b5509a1c841881f279d323334a075f71a264a2287a226974a91e0f9bff735ebc4888e9031501983c910153"
        hex"fb861095216c8581742d360343a4369294de9bfe9b3dfefbe05821072e19499fed491d7d84261ffac9a40273033a83d14278cd2be543f40f6e8c0728"
        hex"5868d31c60bc2b263bdb11e5322be398c17ca87fde2a7ae15b5e4dbe4eb602b63fc0a6920bcd044922684a37a07e8fc0e256afc8822e45ced211a3d8"
        hex"f16e29c11cacaa94df4212629e8987d6865174e9d79f779451748385e83b1546d7310fef3d5573d007e0eb89726c81fc10c7f9598c91d458545fc7b4"
        hex"fcfccb3e83bf2ae11b56834d64d4c44666f4aec53f623f993ed9f1a467f227030f0b3f4908482fa408d58dcb8722ddc27fd52c1d5156116e184f258a"
        hex"d7a4a36cfc008a8d74a724a1e7d98b2c704f5f2883dca41dc0637f070dcba641b6c2005c2b1a4d0d8e6711509ed2148fd087973f86274c1586959104"
        hex"8afeed012871a40d19f9a315df3207cd9929ab2ee3cfbf4f403b50f9529e48c709728b341616e3451fa07e18d3501db20a0900fc9a6bc3a82db3e02a"
        hex"a1a6d4af0c6ed8c375239b2c074bfcd1eb992d983c910153fb861095216c8581742d360343a4369294de9bfe9b3dfefbe05821072e19499fed491d7d"
        hex"84261ffac9a40273033a83d14278cd2be543f40f6e8c077048379906e7a27029ce306f9a9bece8479feee582355c3896a1cdbf04a7021fe2a069397d"
        hex"e7b54901bc12c4bc9b2a0fd0337cdffb84ead2b0f981a77ebe820e9d6b3abdd7a586595654dea7fab63b39feb8b3d510b82968603d5fd720387a1ad9"
        hex"e0faf7e1616f0c44f3c008d0d2be7376241ca04c3c28706243fe51785c2c16c11cacaa94df4212629e8987d6865174e9d79f779451748385e83b1546"
        hex"d7310fef3d5573d007e0eb89726c81fc10c7f9598c91d458545fc7b4fcfccb3e83bf2a24b47ba9e493c6ee6631dad6f3951447f6535b7a21615c2b53"
        hex"9671ca5a888c0edfb957c7aa9bcaf99ba1eb51181c226afc69ffa9f1d504a46e6e558dbcddae0ca24f74af71cbf663c19085a650ffe8ef470f46993a"
        hex"94ec21d9e5c4e00823b6198d40d9a8f4ce4c4e796e4c8d5bdd46dcaabeabcdd7ceb0c62af2466be470aa23983c910153fb861095216c8581742d3603"
        hex"43a4369294de9bfe9b3dfefbe05821072e19499fed491d7d84261ffac9a40273033a83d14278cd2be543f40f6e8c07d9b214fa6a5e2fcc2cd8392c24"
        hex"1a304453a3484902b3e4609cfc6fdfb89ad804b4b5bfa2fcf70624dbd75b73470fbd9f5290c34947133a113bfe369798839e0e983c910153fb861095"
        hex"216c8581742d360343a4369294de9bfe9b3dfefbe05821072e19499fed491d7d84261ffac9a40273033a83d14278cd2be543f40f6e8c071eaccb7766"
        hex"d138e2291fa2a4aeceb5769eb70019b11cca7d6d0242e080fe10089cf22f56854da0a43f43c373a0759bf1487b84058d6c38bdeb242d1296cfc62f39"
        hex"2ffab9b384248f7e79f70456892a1c23123f10b175f7dad63975844d9a091dfea16a8786f5beb2ed1bf63eb725ffab1e18921fae8d60fe5c45d50311"
        hex"97d80c0be72fee5b727190475e3b328749da4332d205306744eeb15007ff4b9918312f6ab19868f5b81245912e98c5d7a3759407d0eef8b72ea37e01"
        hex"f8fc550480621f453ec2a7d5c88b8e47f084bd61b8019af54c7f90ec5fd0fe9868a358cafc052d239cd641e77987d42cb426be1519f0ac09b0300676"
        hex"bc9d561f43558f7ed7fb2f2697356adfb7dd4c93fe16a25c7441f000a7cc989645e4ea3df16ec8284e2730c67d9694aac1d12a5dfcd236489566be4d"
        hex"2fd0b4c1e84e170531ff9833972e125eea1eb99d48ac8d6bfb1cdb3ecbbd24d2f40e723087291473d65fdaed4e1928b0dcba36a066e13a602ea3a03c"
        hex"f1f01c22fec4618d5f3f6f045b31d73c51022e65bb5c76eca66ef4b7b11d3eaace92be6da0370674afb7d1fa6f3b3011c3e6105594283138c4bb34a0"
        hex"2ae303a55baeb9500035ca4b7363c7d083e4778cf1d722983c910153fb861095216c8581742d360343a4369294de9bfe9b3dfefbe05821072e19499f"
        hex"ed491d7d84261ffac9a40273033a83d14278cd2be543f40f6e8c07b0ce62d7caf50afaa2d898432c5d0405704276abc7a8fd90e00c10e238b2510601"
        hex"a7467a930670974544061a86bfc22ba31471c2d6a92b46075fd29ff59a7226bb110292d6bbd0f4a8c2e4575ead2b11f47e3447c9b42f0f825fd324c9"
        hex"68e10bab0873e1a69eedccd3ad526a9d4f370e27400fc2232eaa37bdfb36ee4491ae29d4174833e30ffcd7f9936ab989fb9c872afe266acf76c612c2"
        hex"3445f05bb9cd14cf5058ff5bc28e9e0e7408da24b04afa5e39af2cd1efe364a1695431cf2b8a08effe921a9fe70dc367a76af20f707b29538939ebff"
        hex"622e046e6d89684f11620e6bac03c92ced9be9b9e58bddccceb1bafd628ec8d6ad5ecff41b2c0afe8a4c0472c01063aa89c0d624d011dbb7f599793f"
        hex"0a646d35d1eae062b53dfb943c410484fab12eb83570458829b1bd53897a4a066f29bf2bb2d32bb8625455e8aec72bcc275f6e25421f01ebc578de00"
        hex"413c142a00745cf0d077cd725e004163ebfa2a1c77eb0d32cba79e7f2b2849642c709dc1307467c16e5dac082bd046fb68ac16fb26ea63acd4cc143f"
        hex"00d55386a673c6cd24f319348a83fa92f8bcaf4fb8b4182e5012219e4626cc5ccdfb2cdf1b4084bd596556467988079c71e8d37826a823b453817878"
        hex"c9bbbc46b4b5afa845fa91b3044a1a2a617a029384308bb6fb9327014d9511dfad79b2df9dd534637179966505e5a2fa6bed91df09b8659875831e67"
        hex"085916a577ed826ed9c2ca9ee1ca58581f4620e6a27125186736221ada8c0a69909c3954df5841c3f9c9ccbc5962383dda30d89a686abe5760374534"
        hex"54e21ecfeeb6b83882b5c33c70423109854ecd054226412828faa766910853c17d352464a3469b011a4f6d11df10eeaca58dbd1f20cbd8d699e577d2"
        hex"d07e6db7e62025faa748676658297e331d664a554b014c1c0b3e3a6970953a2fb60259fa3d570738f03f11e6d2adfeab0b58989eb87cf88b1d113a53"
        hex"c09c9ab7305fabe4b5030cb27949c64c8f7bb4eb5f3d735be76dadea464fd43410899d07fe047783fd6c2a06401ec1967d9735d135afcc39664475bf"
        hex"e8200cc91ff104c6f93e47706bcc0fd4623d45171d711e90c3c2511e0c035e3fb9c58cef960e6e509dcc18d3f06c25a061407752ca6df979dd80fd1d"
        hex"46fc9617a9f3c965440ecf1e254f81ad66702a281f1bb7e64b22ac8f50e38784ff19c32a10e05ea8a58ffa297415cb3c9b720d12c1fc75c1f55aefc8"
        hex"836c93c22ea32eff0e902d7ebc52bfa30ece20b4260816d029cb5d84d0aaa3fc735bcd743ee2af3e1aba03feceeed928c1c71ac14ec02fea416c9591"
        hex"026099029b9a942736fd83d108358b00a718193ca83b767a4058271d2a69a1601c197de486dca412aa4f935f697bf09e4e52a4205efa591034c20cf9"
        hex"be6a5fa397c85103f66377376f5ed488303d7649cefe49b68f6d9d6914e922e2a0ea13f8acd7c3a02df4ccfc89d7ea260ed0e706eee819f7166d7b85"
        hex"c4602e479808271e10ffb881b49801cc41c85b2a05b073ee8352166c65cf46d31efe1143cdd1d81c466c5a799e21b2b08bd4543fde494155b3f82717"
        hex"fca467294bdc12f0deacbde793bfb73fbd19c535e45bfe9663c425cbe3a1d6124b4b1f9d11c71c076d3b73308444214bd6c0f04d52586d15a438607c"
        hex"c4457d6fbd886fcc3cbb2c6d12d90c5fde8fc55c277abd3ec5c921d65508990ec4a820939cfcdd028c40246cfbf5d04d5c4116259993b92dfa492c35"
        hex"9b105f25b8f64359fba41941eb541e976b4a33decca5ea60351f0fa5e91bc76036feaf7466c39ee7607654142640271d0076d71d0a4ede6a7a7d8284"
        hex"4bfd8627506ec49f57b6d457dd9bc0428a3f059d92a758908354f719e3079ac23b246ae6c33e317629432739bd75b780da0110fa2fd1ade204b84ecc"
        hex"0508ea4db7933660b4ce6e57867df13d660ea0b0e505087f3bd5369b73cc2c0f4da61c4f8903b4af246f0d53128aa76f3d1b48cd8a4d297d7af86e49"
        hex"5880f8e580b2dd6069604d3dc350d8a8d2a77ea45cde81cb7ae815e8b4ed8d005bf26fcd124166767ff05c554d2313661eaf8f3bc5f3cf4e43ee1a80"
        hex"ff9abba77d290993fda223d0a6635ae4d259ae67b9b407c1020146cc728d2d82bdd738c5740c9578e04b486d658268fcdd012d44aa730c12831b15f2"
        hex"c9e701d0914c84b02ad76f9050f748a995ae5028bcd8ef938433a77bc42919fab1f606e7626358eaf1827d66d06e5fb6826a373d876e6958bd8f44ae"
        hex"4dcc947b1d09041c8f6ad8fd186793a39d1f8c227205842c3b947a93a2cab0b2fc4d515161c626e1290cddf4d18dd0edc51777a1abb06da0c4125d27"
        hex"a3541c63ed4c34f9b075108e6e948f3a6ffb130e23a49c15818dc117b25f79815a74d0010ad4169a7c9f112c9b908b57b49eaa7332d246ca55be17f3"
        hex"396c2a2bd71dfd8debf321318d1520082e1827422114a9c04514f286e9a3cb117c757418f0fef7e3fd79561eb854162f38988f4d64aa39299132ceba"
        hex"7c73903f50428deb05d4120c3c287110d4941f5b179447449f6d172501c519058be203acf6e187320d35f7fac8531f7a23910799319b242d45e4b142"
        hex"f13295d2886383ca720b98709189e5bd6b26b644ac3d205f6f334638b5d786f52c9f7cc0b16b29f1f9af3655be90352ca3f332aa474b221a141abb75"
        hex"af88aedeb934ed2bd5720b5e5166c2ac062e0ca59e879c4acbfc1f8c8f888ab47e8eb76482260a5ed0db59cd39be38dbdc47a5ae2c2395937e7d1417"
        hex"f9317960c16698de8221aba388cad4cde2a270eaa1f5c02a9b0f1690dcf003a793ef730da60105b65309b25c8ca3954498f35ec000773376f0031d85"
        hex"4d581204e3c980fd1d3371d31117be9aa4049ca2f92466501fc6f876d7a25f7ab47817d56344c78cfe2d54def9ddd03e3343907485a483f88b035c1d"
        hex"21a5a920d87103764314bebb0c3a3510627b6a592909df7ed12e7d65fc732cbc013f725145111e8e9b794341b4b7a76ca7b1ff2b46c97e4954a06c57"
        hex"389df088cc3d16ce518102164f427ae9612bd5ec538284f0c9888a61efc2d21d6b722496ce43ce29e58917092fb29b659e4de4d3d07eef0dd5a4d731"
        hex"1dfa93fc63f1d0406fbd509c03a11ec5aea380f8ee64e6a9d763ccf26ced291f96f6cb50abdf61a2bbcc599d0c320f4fb58a4a337c5c3979e7fce8a7"
        hex"ff4f531473cdd73d0fe218a3f625748b234f14524048e0742d1c572bf32717b7ea90d6e8271d6fb8d5b888ce29285c0134d31828c1ffb26805e419d2"
        hex"8d291581c3d62891b32dd4a4d49e91e2209fe5c4f0e70dca82bf278a3c4b32ad60faa543bb3f487da2bdfd18ff5dd21ab8af5800052500701be000e3"
        hex"3c08b46cbf473074f22fd1670b2a345182a5f82b5705d02010f42d2d2b6651a619ba15798d5bcd5f43be51244f0e7989518b99257b0f760343e205f2"
        hex"a583d82642cebb2716abccdcc4a7ab9dd6c0f834f36086d2e11939da3df21bb64ddd0daad45d82b9860eb45b3576fd77ec654431ad630dc11998e901"
        hex"0f661274221894d1823b2ebc33bf29c8969b989ca3ff9b1a0f31d1836c882bcb1d6625d5c11a61dcbdcc1bdd2688f10863a3b054a64c0715ebf6168d"
        hex"1480c61ccf9327e8022efe73258146b543ffac5f2fa58864658d822a289121ff33a309ee949e1d9550fe5110488c5a915291e5d1e601cbd95b830493"
        hex"90cb3bd765522d62895b18954003aef4f7cda90846418503bb080104c3cef4f48ace46aee71adf09c9ae05fb246959b111e9f3aa7c488df8e5d1e8ad"
        hex"d84c14b5b1510062a95938475caf1426b1010c1193c7e364b446e8a4f5434be3ea60dfe977a37d98a7b52c9d4e4f2d54a0c7d78ed818facf9826dde3"
        hex"bd55e099b7d76498f9395bd6d38caeb5d1a22f7897dea083a8085bd229a3b89e7aae0aeae62fb29e744bd964ca552b1ab6ef0a91f9c8e6b668f4cc4b"
        hex"dca4a58d1078ce54b93bad6434363e8b6dc623207e7b0ed28af613222e98a97a3f39473531389cc04f52f4090b249da8ed72e994110714b1356e47c9"
        hex"c4bddba8ca91ccb1a45dec735fd43a38d3e0fe647b7fca59ed29248ae3332cf2fc8758764b7de52d10292ae9d6649ab35d2be4e2943fdc28ecf31329"
        hex"ec5900e9ec226cbf6a98a9a773025014958a3b47de6c4453b351198307b62a90e7c2ac5e12586247b172371264ca8c6f9eb32ff8501a7580f4dda61e"
        hex"3a7c2f62b0206bbafee5242d4da0f1e9159a430494081bc91f246f73d0c1fcc83a6e22659a37af05ee1155afe0faa2e93f01cbdac66178356908a338"
        hex"2c1cb84450c329387ccabd4b438bd5f14d47fb552f5161af7ce44ccac8370c32914ebb500e961d5fb91cc73652626cd629b419ac348633da57783aa7"
        hex"d017cda37e1ec4eaf10e1db66290476a9f8fcd8e63a9a607d5fd3208c6e5d965db53141227c9837f493b1ca01f93edf508914a41df954482110521e8"
        hex"f9ce26a048ad53b1798202a41ddf1c55116fc79d38814b6a130657ac956febd1c1314513b1dfec1296fafe833b75041343676fcb38f47791c048c485"
        hex"7e2c0e31967cef8c8ef1fff1d5d4441bf0881ba76c1fead74f9aa6dd6ae4e10519215479082245dd04ad66058a56d40e1cdd20254a1a82254a1f9d75"
        hex"46d6c21317c0bb120e81041e15ea6a4527d1314edb3a0b60bbbc5474a827da8ff8abf64740dbbb5c66937becaafd8ee8fbd32f5baa7c0799f97d0dc3"
        hex"c3707a366569715d682b6580391ca301c8da2ff34f093eb661592ba917d1aaf548079e8ec914f27d5fa6d84ac0a428359b90595a733316c8a21c2192"
        hex"d90bd180ef5579554dfa130ca64ce255b0327d82cfeea3809362116eb630200c4bea6aef003ee2cc1e177be106f5f2f81905a63bb91cc3139c1f46fc"
        hex"1fd52e672c6a254367ae86b3f1666dec3373fe3cd04abe8d60e927db6ee191e2d7030b07e2e29693fe85a61c452049fcaddb82c2a76856da434a0743"
        hex"23c620baf39427c95f3b8fb1d57922a4109a13c31ea0b449d1af0fe72f79ada2f99089c275ef0657b27400618a0b7f85ad398bb03faf6171c4e00ec2"
        hex"4890bfe72bd967143ebf06dfb4652ca7ab270fc398cbca2967e343df00eea772cd6ac49f61ecfcb014400675fae683b810c68c232408cb530a66ed38"
        hex"27f25c852591d9b2760313bb43931124e8e8546dedc4b9a9a6f5c87d72adc2e1d2a157d441385984a247f6702f33104b5e899a04a05972d1bc3e0206"
        hex"623cb67f56e79108f0a995a67e396d97e06212435da78ece60b9cb75e5b580578d089ab56dad740a02582faf40afff2c87162b64b87406dc182cee61"
        hex"d330071a3c33a71ffc4acf6b55aaf3cecd40fc922ace165d660b441eb5614c5cbd69e0bd684ae00f47650cd9699ed16efda6fda9f1ba0d95b106f663"
        hex"7df0b634273ca2d35f4968a651c92961a57053b473c1ba6f42ce22ccb72e162626d9031a0453caef77b6c18ef221f2a677f9cc579714a46397701269"
        hex"7c0f6788cd345f08c72ad13c6ef1471523098a6cc429bebaf9372f4d19371e55e1708f472ee01844c9ccc5d22192f2fafa057216eff5b461b5185076"
        hex"39182e16d55a8c65b37e0d907d308243e4ae7b9a88c6c9c9a25a6f24129300e821f224e04463b08c6fc3cbe67ff26d8cce4ec4dd0cb364b70a1a5acb"
        hex"a3ba48907d69089fee28e54374831a792af431a472a4ed56553226656162ca549dd4fdd201300a3b29d479de2ca8a1b398eead57ffd45afea0c01223"
        hex"7f4351e0d682b72552a521aef5ed323b919bf31f6cd9ff496e028d2c06c30df7bae0ff4723977db2c5b521e12231dd47b77b11450cbfc9af70d3b1fe"
        hex"5fcc48afde7492e124dd9733e9de03f27aa5eb18b97daf24978440411e627f0b0ccd1e74d972e62679835567ba760d9e622aa7d9ebc1bbe4edc4c325"
        hex"3c5974cd31e5f68b6e44df057973a9eeece0133db05c6509ba0e42e50dce96839e4891560fc4b09ab707bf904eb70e24e9b40521de2f1d6c1ed33f5a"
        hex"91e50514dbb03ddcf0a6a623102137e2512e6581b3a90b36aad0f7cc6550e77994cd5826fe7de30b7bd2a2b6aa0f57d77fb18fb0ce47036c0dd32d98"
        hex"b4c3913f1375dc63eb6aa741d774f4f149df72abc79d64dd1cf72fa24061c93826e94d761ae1865c44d57cae76edc1b53905861b81ce24a03b1605c3"
        hex"7396268659f0227a89582c403d3a5c92e20d055136045ad0222acf4afa751024c7fadff13fcbb0cd6007b0b1a10a8adcdb230cf7cab0abab696f988b"
        hex"85880c0951f85210de8e6dc9bc46944114129d57bb2c5233017a7c764db026e725fb0704cb1dc71011bcf2ea4c4a035d1752c9b16adc9311c40b09ef"
        hex"95a12489cd650596b626d8786ec25bf5ea9bfe160e0d0465140c312c7793570c9727971befa815f858ea93b013bace5a92f05c0c964d7f88df9e5bbb"
        hex"152368e2aebe4f30ec6300651860f303ca3abc2a73677017c72a05df7292086296aae9253b2e6b01d079221f50cb4b152fd551ee9714a0f11c674e3b"
        hex"f53fe40b46121d9f46ff5c1be51923a62587c68c5e8af70caef2389882daaca568e8f3f80516fada808cf8027fda0f642f090a6df5aa7e39427e71dc"
        hex"c6c1d97190455713dd78a97882aa08a903482bcf3d22e6d155f5677916e38acf6ab9310a760b5bbc6771eaa885cce79c5a4e0112010d0da425791e0a"
        hex"365492f61015026fd8fee5a68eacc52d1e0c598b82c92b7e4aefc61dbd7cbf6feebc24b2fe3be14e393be96bf526a0edb7291a74244916982ff7be03"
        hex"fc5034ef39a9b7f2c198298d977de546f2a3872159e1ae2f66a20c0ee6a19974afb44c21be4b796e9fc3d329dad691f567358549a63e535a9fe90814"
        hex"e4f97398a5e1f6c5064a6bb1c6f6ec09941fa2fceb58c018f4f0fcf6b943222c03c13f6823e049842e18829fec234b8c829f955602ac0e2aa7368030"
        hex"02251ce1d2aa35594093bc56ee2864b59e140a8e54696412b073f433f53f9fad9a9d1a48de139e6ea44e59911a1f947265d9df764d83717c2bbc34ee"
        hex"911f6b82b2f22400c59e36760856aaa66bb7a9cf4964590cf7e3407702f3b574f3dabe164617183b5c046008b2abc5e4f2cc8f37152b8fdd0126caaa"
        hex"da4dba859ba3084ac5640604b157676482d1fde4ade78f6d9d8548cf50bd72dd469ecc38723fddda53d20a7f5e1b0388188b3e3fa558071f7e8fa06c"
        hex"2e668f71a406c8d981e10b143b3e08df86358fc6d85fb0c361f7c2d894f13a439ce16a4dad623c1efc4a7253b0612f7586dac5629f56e969bdf296c3"
        hex"e45ff85b55859cfe10cbf4b376d14954b004050c7d15b907262043771300c748e60dab4265e9f43783afc9aadb5b02cff7e0106d956b210b71d45a79"
        hex"a994363ff471414f20468bb24dd11261ce39afb5b0910a92b08892c06c3e2bb43ee5aafab07b09293f94b4d96c14eed696ee91f2b4f913b12b9852a1"
        hex"0bc0d58054d80e114a61d4c7d27ba68c03140dc63e5f739cf851057aa6bdd16b3c6a5610a0358f753e3e0d4d6e0ae2dbf66614d9f746f78a0b1d2655"
        hex"62dffe799680ca6d5ea63e582a93f45d79cbf40db9e44fd30183b955ea2905c8d68325d28d87daa9fc126c3d13c5569c28388b7599f132648ecdfd37"
        hex"c0d224d947a31eb6a99ecbccf407abe3bcbb24bd3d1b7e63b6b252ac4c4eb60e2e000e01ec45487e98235bb2bb270b365aa3ccc69d88eab6e8bf798f"
        hex"46e2769497d818f7e956369176823b578686348ae119dd9a8cda5091c5aed16aa03ba115f4e3172721376a2829210bec04540ef544dd39aa2fdb4773"
        hex"334f56b9cdee0f82bb6f2be936502d36ad26ac74b56ea3e336c0571198f9ecf5f158f867f871c3e56b562a867f1d620d8762bfd5acb06c8d60118514"
        hex"9cdc7a55311ea763d2c71907e8ac12174fc4cd352b4e773103542163508403a377833d814514f782e73dab581823260939ae83fa33e7af02bab4dc33"
        hex"cd2a0e38dbb4650f8ccce6881f776522649b1d435f699050d6a0ba79f05f5944c647655e96d0f1d1ccf1db1cede45daad5c8045f136ed7b2e57ee877"
        hex"06fce3fd256af65bb1a9e1ff49dfc4bce32ff6e94c102396203500b7aaed3da83573b6742dedd8d27fec16dd958635f70898d4cfb08d1bfe06f143eb"
        hex"88ea9e85a8036184fde725e331a8ca735b01678eb2bbe23e237a1bd6d24f7b0044c842c6f814a237837e57fa687d0c85d9200a78471b393f68b71f97"
        hex"a7d61de0bdb609a08f65fd2421c5640607899d19f399b6e631a78adcce3e05e0aca84fdea1aa338b0d032230e3bb83e2ebcaa01d47038c9a9bbf4148"
        hex"9f3511b9a55690842cbe52adef805a7ceac7b64e27a9e15ecd70723f29a9f83834bc16388e02824021de0e77ce8d036aa79911a0384295c577b8e7b8"
        hex"de6f3eab30022c19e25e3427fd0353110939da26036c03764aaccb8c5afb9ae153496ba6e8b914c23747f90c99f660f3d156b8069a02f808bff893e6"
        hex"8b3626e59f10a1b9a0e90b59dbd8d2273ada3fda7c32549d6d239970c8a5a1083f6b48ac22bc9d3b30762ec423b0cdfd4fdb38c9528d65faf7c054aa"
        hex"3febd65a646ea743a797cd39101023beb49242214d346a310211272089c29835133a461f16a8138aad332e5312950bf83783a662e638a3d947d67864"
        hex"29a438aef3972e45ebb7d04df8ac84720c6e2e9f56693442b9f6aaadf8ff8ad717f43e58a673d9becdf6ca09c31fd1a2f25926c2891714f0a7921200"
        hex"0e72472889ede9cff6e0e070c7b8ae35dad57e863d3d21e6791b975132e28fad03f2930e7d37b42a5ea2080e5d99ae88a3dd9134ea94118705815adb"
        hex"6307cfbdae2508c202363efad5d5c1584c9b5c53fda4db6efd5b222b2cc67e3a5527829bbe280606f9250e5931982b14122aefa197e93e2150b4239a"
        hex"7f9c4f35c7e46e639da8c466c9ec4533c946f0628e18dd1ea43253493e802cd6272016d075b94ed07c0f2f20f1e45aa4ff75ecb42f525c6e8862c15d"
        hex"7ced14c358305ec0435cc2fc69e4aabbefdbbdaabd20f98f9e717216d98be83e254328079aaf5587e9e22450ae9fd03d13f793f3018c302a99552b8b"
        hex"08fbce1238d52dc1362ef68e25a3d5632ab543696585e23fa3c36a49c9255bb54e5c4c970efb29a214ee3bb45312693252e6e8c71aad894974beed29"
        hex"bbbbb388209154836a820824baca69a0b2e687d5ad884760f7e4831fab81f8d559c25124fc825ac96a2b24edb156ae82ba50c48841c3e3790315898c"
        hex"252c495cfdc9eeafafbdc13b2ae610be76c64b01b85e338b0d0276825a84224bedd722e1e0f583d0156cc03c7c2d122fa9cba1786b8f5e7c439b10f8"
        hex"66b260ac9b4ef74bd8600eb2e035ecd6af091ce1d2e93568aec9e48dcdf15e727b485e27b559c6ccb144565073acd717ae6b20e258da2103e566f818"
        hex"1a1577c465e9847cb35a738fd3e7785941a5e086591c2d6a9cafe1a6da5ce4c359aebe7d92bc7e4aff2c3864497c2368bc03993ba1b42ed41b09d5e4"
        hex"d9afc4adeff94c58c5710591ab5d882688bfb6752021b8077f002b46308f0c2147f0dd20af46fe4584a398e4e8f9959374916617d76d4dda9eb70114"
        hex"2d480fdb804b5d736caabb3932965fa547bb3579f24cf954c4f6dac886911a321bbba60d03b48f6351269004b67c08d81483453c43a029b6dd2eb9ae"
        hex"9862227e776dc99666f7eca9983ecba685b13c03e4f32094a7f7a686a8900e3dd9760021b736d70f2c8974bb1c615012491b0c0d1547a2dd37c7c3a1"
        hex"0e89c5108317273b1c490af697c720dc499ae114ca2b9cd6a9bdbafb64af52458b88633e383710f6734e4636161ef0cbcbe7dfa5e5be85a34d9ec212"
        hex"e9451d2a9ac0948d12622a1c7e000ec4a1265cbfcfe47db6c58bf638c792121d3f5a78efd5f0d6ad06e0180028a37f67c64b3289e04cc039408efb6c"
        hex"5865a1cb4f3e83e9190eaf75d461162522a90dfac54f871ae9e00ba84423c1390fb22b97a680e58218c3a016f02f1e026fc760ca2962879730ffa763"
        hex"ddbac41e4ca9c266569eddffabbe6786f6f92b6ce15c397ea7207d59fa5a2ba63a592b97c385d05d8509b94ab8f90a74d5dd2d";

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
