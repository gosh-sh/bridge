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

    // ZK verifying key (VkBlob) for the FINAL ETH-deposit circuit
    // (receipt-proof of an L1 deposit event, 12 public inputs — chainId added
    // at instance index 4). Keyed on the Hermez Perpetual Powers of Tau SRS
    // (s_g2 = 928fafb3…). Source:
    //   tvm-sdk@feature/deposit_circuit_chain_id_pi (commit 8615745c):
    //   tvm_vm/halo2_test_data/deposit_10proofs/deposit_vk_blob.bin
    // Byte-identical to bridge canonical
    //   bridge/deposit-prover/fixtures/deposit_10proofs/deposit_vk_blob.bin
    // (verified via keygen-only regen from current circuit_v2.rs on 2026-08-07).
    // Magic "VKBLOB\x00\x00" + version 2, shape "Rlc". 8847 bytes;
    // sha256 = bdda89a58fc4d9c191c4532f0dee79130942b7b2011dfb2651e2b95f5f6c0d3b.
    // To rotate: regenerate the fixture above with deposit-prover's
    // `export_vk_blob`, then rewrite the constant below with
    //   scripts/embed_deposit_vk_blob.py contracts/an/exchange/eccUSDCBridge.sol
    // — never by hand; CI runs the same script with --check. The fixtures
    // under tests/exchange/fixtures follow the new blob.
    bytes constant VK_BLOB =
        
        hex"564b424c4f4200000200010000000000ed0000007b22726c63223a7b2262617365223a7b226b223a31382c226e756d5f6164766963655f7065725f70"
        hex"68617365223a5b33322c32375d2c226e756d5f6669786564223a312c226e756d5f6c6f6f6b75705f6164766963655f7065725f7068617365223a5b31"
        hex"2c312c305d2c226c6f6f6b75705f62697473223a382c226e756d5f696e7374616e63655f636f6c756d6e73223a317d2c226e756d5f726c635f636f6c"
        hex"756d6e73223a337d2c226b656363616b223a7b22636f6d705f6c6f616465725f706172616d73223a7b226d61785f686569676874223a302c22736861"
        hex"72645f63617073223a5b3132385d7d7d7d8a2100000212000000004200000058c338696a199ecb26464c9bdedd6f46e6fa0c9974d91b9fcaa627c329"
        hex"dff21a4e7b0e1f40caf730f9cb23b5583598194673b2bc3c7f181c20266ff8b4d3cf26e2d6ebf4902a0f7bf1b807ae15f746e5852fbd42820a99b6a5"
        hex"e3195a163bf3123a94c40ddb4405c0ff3e689bc89adb7573072e1c2263fe4b5b5aba204a07a70c3e95f55881ae181828bca4d5c448b310809dff7892"
        hex"cad5cd580bd89380500e3084f208a3f9e148900d13303b0463f219ab0b1c8edc9cc1ea7efd1ce13892a40d8175537a1dc6dfc62cbc14ec73a57991d0"
        hex"51b3e8e7008002f8469672c51d7e1ef9c6dd3592c2f442d338f77c0fcf2fbf435f14d811e778d32418d496eb6e4c18978617446daacc32dacd89051d"
        hex"450be6b1381aad97b4ec7bf6429f8ede02890abd7775ef94a1f1803d91720760f39e7a1c0741fa59af0da8056dcc75d5f26a171750e695bf399381ff"
        hex"d9bb1c217da936a24274ef66e395a0d5b14bf3bae0e21aace9ea20dc7ba3bd83428ee1f94bac52776f74963306ca57aa9f35d626e9591d08ef76f71e"
        hex"290f9851d342d6544362c9e81a8e54e6149c7457f10c6110ddb92a43c104c7a08e2348e52461a4258f00051787959c12983cdddbfe4a9d8f60be060b"
        hex"c1448b3d0f3c2b065c87654b4ccfc98c6299b90acb86a7e3f5fd8a4d8cd0177e3b12e7fb3577994373cb2094d402392d70c15ad7914442534f89c7f1"
        hex"07b42bf221d15bfc5a929538b9c31ef70f963e11d91f02e7516f12200100fe84e59f09aed6301a4893380933666b000d3152d3cb8048528c5fcb6fc7"
        hex"06028ba6e5db1608516aca3730512a7c3d4648e8053bb6572fa59ec9aa967b608a140893df9e29ae794cf7c9cdc4d3cb0a1fad061ac92d64eabb9cd2"
        hex"a0f3809d7ff322569a3e0f69605407389a1dada983a40e1e56aeaa5130f665a5ea05f0ee1e34b9d41be51d346a8ab62a76f46794bab399380dbe2717"
        hex"4bb8cbdebe86e6bef2d8f41e87810cd02fe04101d741ed0dfe5d807e75cfdbe615b8af7f4684a055b44949f73ae521206c8b193472e4a489baa23da5"
        hex"2e332acda38a9f02176d774e25165777e07706f52f65bfb43223f908463a1759cf80c2a506b18bc01043e4ffc54852d3fa182c6341880a7ae4ec85f9"
        hex"5db5764b9500060cda6bba90519f7f6c84b933a31b2a0d2f9ce75542c1ca7bc762a9366fcabf9accf98fd51e26e230d41594289e9b9e010744fb0bd7"
        hex"54baec2703f5f48d21b1c467545e84fba394fdbfbdbd5433cac82e807762a600290e6adbcca7d10970b91bbe46df605275735a0f86d9b76797f81e9d"
        hex"208ebbd4a497f1d96c5a093c1026cc7dace1e8f28d0c92bd3c29a97f5512154207784cccbdbbebcd88782b601d5270b80d857fa7781b0d2c1eb72d0e"
        hex"9e2e10d0b13b2fb5545d2ddbcae4116edd9a4375363d5857cccc378bcb26adb348332dcd977795cf5089ed8de7ee62ee11dfdcd4001f09bc4adf5583"
        hex"9fc2cf54120b103db2f9e98102a169f7d06733b049539cd952b01e2dfeda798547e008448a9c2a96a14be6d3409e3cabb0a2a97cf59cdbc2c3031ddb"
        hex"e1e02cc6d43f71a98df91cc169584175311b7618fcd6cc6151d96199da1b50dad0411b88a15ce5fae98b05d23547e660c18e213aaf5e8a6e4a68afe9"
        hex"5304c7d3b5897abb151cb7a4b2101a2903659d46d907578afd98d907826ae0a35d9c75b72e887e41ea5088cf9c701e56dc80b105ed0ee7549dcdd893"
        hex"6f15b86d8160c052a30c494e702663a5abdb040a5cbc8c77806efb9f165a73305f5e531a60d5c5daf0fb02c7f2f768402ebe1aae91b1cd68a23d1d6a"
        hex"9a6428a8f751b6c30acf5a258247b11b3aac38ae2b110b71cb341d2b82e3eed7c6c43c50b1743e1d4df47f486bb19430fd7fadbd9f911ff5400648ea"
        hex"388178a2c934cfd1ed980e74c4a384753aca65c42eac78f7bd5112da9d3f0d773b49585a68555b917252bfa236604ebdc102bceeaae1e101b7862a6a"
        hex"8d0aab65ebaab9047991ab92e4968069ad2862f7c512c8bd2d083f86d45b04dd9faab3cf4ace220bd5dc7edfe1948b963b184a8e29ff34bf9da1e152"
        hex"45310fd270c25aff6a114bd6179578e4c980ec68bf92f046b5e7722d66316a29b2bd2a48c1acb8ab8ad6a47633ccd2ba5e088ee7cd1c9bb176e9a040"
        hex"31cd74464b671bc33be2b88cf9520b2d5e9fb3c0f81f4a8b327497425dcfbd92777df2e0b08012f63f681195728ba010f1d39118dba31045475e36b2"
        hex"a51cdc4860425c3173b70d8ef4842fe8b4a5e7cf42daf7d3ccbdaa329ec3fc2bbbe647b5a24adaabc9bc0d9c986a65f3f52dc8d405aafd2d04fa4c23"
        hex"e711c3300cff2ec85eed7278be3515284ca111379fff10894fb52df87f95c02054030b91c46cb4bd033b06ab4f191800dc245e13ac2c871f807ec8e2"
        hex"5af6a7338677e1600502d93ce7bd9c09442f2ce5555c25b7d9da7694c5a8bf54641d719c61e86bc9c12424f467bb3fd2868002300388d04b5685d7e2"
        hex"628d5a3c5a15103255a3d6dae678b0ebc508dbd405fe120151defcabb52c4cc925f544c0687655cf298852bfd34161a2a0bb6368f5ab23aa9a254595"
        hex"224ae47927aead2378596436259f3c57822339dd0fcecbf8d2c727a2d83dc7b74c7e66355c692b6a350f71df39edf78f6a5052cd991cbc38a428291f"
        hex"ba5a0ffd1c662b4523f02cd938550d8bb3761952884497961b0894e86a7b1f90506c4313bcb4d057017a6451b7e1b5d7051e8766575535dffdcf6ca1"
        hex"03402bc5306659f42238f8b8280a1edb15dff6d5376ae50bd38907da4b5958d0fcbb242159eea9880ee63473f7728e1c2c92ab351df8481c6368c110"
        hex"ed00e49aa82509d7a4f150fb00d918e3299368764951f209585062b1afa5edd6a945b9df87280fcaf0a071cf14043078edfa98407aa3f0102433044b"
        hex"e2551365134be2bb6b952ea1ed8e4bae20f6f30e8b0b69af9aaba7741f96da85c573ff5cf434ffc1a7eb18751881771bfb3a72a27d1d4a35f74d43be"
        hex"1d26fc805bed290ed5a63d693a9a0bfacf60d005681180d03cf6237e37d526bd828df47362bf5b555f7c52a94348166bd55be82ff19d93dc4c8f49d2"
        hex"52a6e54f5eb008458631308d22f1977ddf9d0330902e7d2ebf82a384e7f0baac7c808aa61be00e8b9a4c96a8b73bcd8f8e7c2daf68ecebd953025505"
        hex"637528dc20c5fb2083f4d87555fba828f432dc11746b0ac510b0d4b570b545cc305a1dc70ae7b861914428418afcca8faa353599ff122f983c910153"
        hex"fb861095216c8581742d360343a4369294de9bfe9b3dfefbe05821072e19499fed491d7d84261ffac9a40273033a83d14278cd2be543f40f6e8c07c5"
        hex"179895b5699a8a701eea07bf0562c354c4330871cb5f0629210cc1b1bdce0000c0fef1af138adfd2c390005b1572efd83328dac6bdd5dfbdc90479a5"
        hex"ad68012ff4209939f2ae9db7d62edbeec7ddabbff40a5c6733c6252f6a072d905bdf0a20e99b95a32bbeeb9470335a9b9dec97269414d93d3a2ab520"
        hex"9c32872b10c8228faeb4581cd25924f4618e0fad12104a4b870b69bca93fd4b1c012624f9639154dd63d71b4986eff0e50170d9b2329289406c3f39c"
        hex"b179c4203830158a7a0a155c1f083b80facfce65f3aa22cb4e103e3e95c3958713b494b7cd5c921595f7181bbc5dccad6e01fdda9ee295d54758bba4"
        hex"c54151f5563e6cc89978202fb05229fab99e7e88db91e0203e626f2ed9314098dceb576298db9356ae1dadc6bd7a0a59bcff7c22c755b4718cd36dad"
        hex"58f70e9fe85efebb220352b3c04eaa72089622e1c51e565ab30874d7037214f802990f9c943807fe6e14a8e7bc52ef4f817c22bde09710a949f50e9b"
        hex"81a8c32c0777e978ca387f7a1b4492c22fb5ac9c9e47301bb921c92aa58f4702f7dc9db94b2d62c683bdd0ebc7bc5378c5704c0ce6141a4c7eeb74a2"
        hex"32ce7098b34d0f35deb6d8ec5c97bf549e06b19d39b0547e34651d983c910153fb861095216c8581742d360343a4369294de9bfe9b3dfefbe0582107"
        hex"2e19499fed491d7d84261ffac9a40273033a83d14278cd2be543f40f6e8c07511b12bd723eda28e969f8d64277c5a7362dfb5770c514e8c66ebfc805"
        hex"51df2ec22a5ad7a6fed8718ab083255bc9fa1c0b790da7e4e7d1b8548e5aed65b49f0a466c7684d0b2401099af8a7293454c194f486c6baa1dc34e08"
        hex"3e5e85f4c2810f24c4989834a0f54dda71d214568ad7c2f45050e7d57ae441af941f8b73b6b30e3f386064ec15c92f18b913e4bac16fa2eeac2de4bd"
        hex"c3d79b8ebec63cb064d704e0938930c3cb28b570f44e0b9023d1b1cc4ff4cfff3c94c833f094f530a98a0aa18a29ad2e8456456a8edeeba7054810c6"
        hex"c9fbdf4a7a126375e111b667c5aa260e57d898cb61d9e770f7188c30e74a2a7ef10022f7516f25e554594aed13d2010be7991f8d697d02e8e246779e"
        hex"adce77e4395d74e2c543102b031b6ba9764c1d051067bd8e6e4e70a28c0f087a8acb32bc4a5769a1119a77983a104de00282003e4f426ff0f1d64357"
        hex"78f4d0e28ca7af4382630a29e83065a3379384dbc37106069fdd7325f3bf8b7ceda13bd2156020b4965255dfb041169f522ce58674191028d7193984"
        hex"c5b628e54b1ebe735e77e7528e910e4cf9e0505928ac9031f401260e81be1002fdf8e94bdd3f9cef8b046a3372ccf3c1373f788f7a5904dc26270b6c"
        hex"9214d25c2e930ab0a84f6f6a00d76feddf14582037dd96a3d1c74f8ff9e703dc1ed3bdc0dfb9502ff84a18c48b3c033ee24f0ecfb350d4195c989fd9"
        hex"c2b50a1c736d47cba66b8d9852238a325de22ce1eb1dfc39fb6e17aa38fecef87f13298b5ece3f53f7fa1e121c56d7bd763dd72670fa55263aaaea0a"
        hex"dd7e2f88d9a7176a24bacf857b662e9efd64fa5c47830516824b0a626aaa7e5a4b6e7b46af331ec3993d95f1045a0745d3035182355b6219cb29ed66"
        hex"de69937ad2ea15727e92079982a48f693a893aa5cda30e50758f4a4365ee824f8849bd52b285f3e4fd501e0e6c56cd41fdf1b5d2bcd9f560c45da719"
        hex"3029099a710775927721d98d234e0df922e3e5622924c36e4470a808418cc47c29c2be54bff28473b24bb34f1bb32d3865a4187860062a2d5d2cfd72"
        hex"b3499b371bdfc944c5a3ddede2340ef7c93a05e77f7adac9e984b750f08b070c8e3708f6e656727cff813000a9e838e65c1a2a1fc9b627f62edc3975"
        hex"2a9ad15893eb15c87bb42107b5b1b3179d24914aa04422658fe7c9d0a17844b11cc1c38c28565c44bbed489bd1f257e0450bb9905f3728c6b164b8da"
        hex"95ae510e26d4aa36fa7e40265f3b601ba790abf7c6c49178be361c9d6a3d6207c520996904873c9f2e24f4ef078ce528208ebc567a9bb7e2880c204f"
        hex"71808b035fef07c9c57045e443f5eba07f77c212f9518f24717593bdc8b70246af004b3743855ce49b301e3994176941e4ff5f4cbcced0c4989117a3"
        hex"9e5e080dae5534bf519112a286ef2caf7fe2e7f02fd9bf6d8ecd8682349c69116a7603c64acb9210ba4aadeeca0d14b96baf9b618d45322ca988d110"
        hex"fa63edd49bea1ec964fbaf64a74756e59ebfdf38a5be80a7bab217b709c5a5637745f7b2340c041677cc240bf9d5dc1d2a525224089369e05d05c729"
        hex"cff2bba5d0d870b99f602a171e435892b5d9a9d98fdf5a5a3ce7533833daecb984e779af2588acde01c72533e4a9bcdbd8a2602e8badf9d389bf2f8d"
        hex"21b37a8d56be27247ea47e7bf5d70d3df98a5978a99939ae158532958f8df12300c58217a35e8f4ed3fe636394fe1f81a4a8565d9e3ba7b1135f6a0a"
        hex"309c9f245220e6f4c61b77e62e0aff1b58971eeb3c84f716b4f6daada81e743806b63b6d89e1a48152f6c018f615ed9acbbf12de1c381696125b9d62"
        hex"ea4308ff3edacd6b68e273993e5c62b550d1d7de8cf41690f2be4bc9ead449fa8108adcb29931ee825b7c4bc5b86a994ae051a8731c800ac3c73d9f3"
        hex"68a7b9b0d069ad3503dc760b38483c4b7bb065455f6b5cfd2688275da003089228effaa3ce0fdc3a2be70c8ecbc0fbd5c4e42cf26f092429df1623d6"
        hex"2ccda7dfbe8cdc2abbead1a40780a4bae30fe8c85b24442e293049d08a490332947139a5f4c947de50fe44b88d231fcb73e8439a9ab337fdb4f6bef2"
        hex"438622e636ffe19cdccd3d3c2d5787a90c1ed2cb35ee14cea808b80252af282cadbe10e06c997f997d4417aef2bc4bc0b5e34612307e2a601b549a06"
        hex"9a0d23d7121625f8f1771a2e0ad4ab12b0798be33a1da1a393d90fc6849f39c736bcc4c72bb408f0ea4d57b049721e2a45c4f124ccb20ae1491372a6"
        hex"92354b0675a4ce72883b2d29016df320a50f9b2e7a0fdac373638d71d6d7e509b3e77a607a3b34b13734168a049c9ee00b7bd3a0014ced2ed728ae0d"
        hex"3308d062b422265b7ef3addefc24205aecbf0e66afa65465d505dd48193d1535025447648c7b6e6af40b86d0a03d0ef3b3a9d53435cdceae93781082"
        hex"7c8b5af16a46fee520d1f58a0f622aad80d00bcf684e73c1c303424b11dc05a9647aa49218901294f61e37ffdde83ceed1fb0b80241bde3550a50c41"
        hex"206612ec67ef4dfa48857cb19f67097fe1ca86f061a10bf6bf77fcb2808030b6834e27d3521d667f0ed461b926b9bd68e86f5187caa51353d8180b20"
        hex"8099c52b4b679cc8af2f088b7d709a8f951d757fa9bacfae8c911cf3157cdc743417f86a7024e268d3efdd9cffa38b042be5cad80b04487ec0a915bf"
        hex"e2cb07e1e7e5e4f140a11087e21fc7708c278abfc8e27b15344793593e22233f88c729d9dd71d0e9a835d6a707c94ac9d78921486632a59b12580b34"
        hex"c6692562636e79d6317fe587b1442af9aac35e8dbbda3ea4aa0beadd6b235ea2a1de00b51e147fcb711ee62fa6eac3f4f7ccda2849a0389cf834b190"
        hex"64ac76d0283d3078a09ce90af018bf9a9eb0835de98c31aff7bc3bd369e0f807654a97e8ac212d4d591d66095ac8fa2eaaa1f07b9a1023186dc61c7a"
        hex"7e617204bc96f67127f9054d3dd5db0ba444113b6ae9d443b2899f564dba5895565ca23e1bbe606f65272a17c4b29e444c1c293c7dbb713244d022e8"
        hex"98c433f2db37909e7d99e96ad44329c8c003ed003a36ee4ef5f7ae2ff4a460faa7602b0d1ab3107ba7a418404f3a09fe8780f12f847be1f39d616e28"
        hex"0732b1a6171020850e26d4e422faab076d792e0659971bbef776baf0fb57f31cc32fd3335b52abc63142a72622d70b40d3c5135433bba46493938c74"
        hex"84afe0fd70427e086b8c9a477c5e87013de750672d380e8816ffd1252e73be68517de969048578cc253a84b3b62f59fb0a2106aa6152011ccb7e9a97"
        hex"1bb328fc94210b28256a9a709464349388dad38f611a744c52210657211d2eedd96c60159e2b0234d03116de07a3c91c3962c8809af5fda405110feb"
        hex"1cf0b2daf5b34d042887ad0a142a1268ba20bb8c8f8a5358d84be7f98ce51eec277ce5da563a6171dab639114d5dfc073213fa71f6f1ce7634e486be"
        hex"f6c70fb17fa6a74b5fa65d631bd48656aab262d147833d5c1d853358bde1bba6dfed2308392a80e77934a756022a09ccac26419a19bbe30e6db794d6"
        hex"4ea578cf31f502fd86eff14e07ecd4540bf42a1766f110c72f75b16dd2f3a50991ea546559f6194af35876c1f656f4fee755b251f51057a4d3b26ebe"
        hex"bb6d852a2392406cf9852549069d14e2e92c5aa59f83359dc8bdc8913e265787093c7b6b45edae651ca228428fe8115fbec842e9a8a8d8a5e6a0da67"
        hex"c3e3437b88a354cd8f1cd0ebf7bf1a3759f7adac509ac252794b15c87283c0f86e94c63d3e3a21b65b2e518cd78823b2228401b94c93947bb6e49730"
        hex"d62b61b1a87dc8e8d7a7c485a4d2847c84153072eef94246ce7b0ea23637e8b4b70fa28f5c176cba3f12ea4a0cb89d80567a24f8d71f08cc3afd67db"
        hex"0d15f204e582cb5ada06e7157fa1756b4271de21c73a1fd18c9d7288384b676ab3873aa3c39c5a710025b59035b48aca653afac700f80660a5be49d9"
        hex"1c84f0ea9b0bcd2ee26fd690513c87869652696d756608d8dcd12a2f9a3b9d058519e03280c62810b8c57bb523d12e76923fdb299e47370678b222c3"
        hex"5fbd0408e5c2254800134b9ab23bb6b20359e15f5473a89626da94323ef90c9f40cfd52fca2139678ceafa0942f3809c2622302aa928ccc196d3a7d4"
        hex"88490ce7a21b37a76d81ecc733b884163f669c1ac57d21ff28c0b650018b6e34e56e2b00dc03817d60c647c96e4afd690f5f9deb9ce7adc430d909ce"
        hex"873bff98790120fea65f1b17f1aef0f9ee0a2e4a09e630f4a047380809ca33a9cc35e241b17323fa2cc7464fe45703ed212644d1f87012777e5d6f13"
        hex"fbc8b505023e02d28be52d984938f07839575c700202c1842f8b2ef31908bfd2d0e4e7a5a22fce316b0f1c342dd4cd3e2873542f724332ed6ca2fef6"
        hex"c36f541fb4761e200d123eeaf09f114b14596c5ef06e4fdae6d329b3c36d5981a473e441dec2ced8fe45053f8f5406a83b272f2386e186ba1bb0bf15"
        hex"b029af0aefb0a203eb33d8220fcadd3631ae2bbc06694f2d17717c0fc6fb585ea9b3efdd06617056d1acf9d4ebd86e87e92c22fabce03ad23c6d8fa4"
        hex"17314875056e32e17fd6c4da82051ebbbf294f82c9d52790ed72fac2bc4fe26a2eefb0dacc0a82e2bb135465dd4f76c7d28fe4cbfba320ec4520fadd"
        hex"4e169dbd236dbc843f96530647987db171122cb5179d83a0445e247ce7303f700898ff90fb540cb498aa71fc54e3ce07db96b752d8b4fb469c260572"
        hex"f879d9f6c0856bb3b98696844f46acb309b98b99cd00519632234c2927f305ee0551cc00b926e9f00e6b82bf896257bb7dd333b5469e3d59f003a0df"
        hex"f7670b8745463dd61d797c3a75804c5c83a02219fae85282497a972851d1f498555e1c2a854deced4b4ff36534f5fcf68f5faf99faf1c7fb108215c6"
        hex"f9943c4cc0110c047e09272c08dcdf75ca3bdce35625c6558e32fe861149b7dc92834a9f4d130edd44fccccf3551a9419140a0f2fe5e6d3c2445b0d3"
        hex"2a53892b1a857449fa0d194f34662178c91987f600b7b588e54c08f3102593586a88e5b2331aa7425b2a1a7df598aca1dbbfd823624a8884a6cd99e8"
        hex"4f93bb87c76a1cc97e94dcb91bdb04f942d3ae487e2b7f7f889abe36cd7e8579b5f13a438973ed00de85c21b3c07171c3d793d3f738498a3d19d623e"
        hex"343806d96cb273c6b80c3c6f97baac3a161f15165b4e74ea701b526f821209804872d4e84dc0d8e026b3691c4c0ba1f90b0b28159107e64c08439e35"
        hex"855ae52377ffd30ee04cf9fa40e6cf67348fcd61839801e5b32cc347f40655ffb9d12dd9ceae09a5c979fe9359cd9a90ec5500c6868f240fd09edc7e"
        hex"dcff6b0fabf2b4417f4ddecb18acb4c7f66a1e779351326c7f770d4352976f2ce4f1db50d67ec909a219b5137808693007288299d0f6a6b3e18208ec"
        hex"077f52326d8f807eae70e981377e1b41a52c91b3bff50e0a814ab9a484a21b6c3fd5311404094ea7e53b2f2672d8cfe9d238fe4334711fd1b33ebf8e"
        hex"97490e4408bcfa62654c17666053a3c2b2a4eefed9590f95a0bfa46fffa76636bcab183ce6cdeef297d63f1f943d33387ba09e09b9dbb75f8074e3cb"
        hex"b76d5b8d28591a3220d5a60b64461f724a378a55fd2cd332da6581386acacdb5f4218d6528382eb21154147f77f37d6c1f05b08649469a6cab416fa0"
        hex"d5a6d9809582be31d5311a6c3b311fc17c861ece8794afb2241f06fde0a1ff9b1c83c944969189189a3f11caf33d1372758b5c89c7c4489c66ddf0f5"
        hex"11d1913b3a09320e2fde2fc2bc9b0f77365302d32e31eb1cc624c2862284c5a4694639ab084f1bce683cf5875fb52ee1b7c3a3de58dc5281ec448cb7"
        hex"b3e236e17e50d76d945331d7b5af7147bd362c05a604774fa86118ab67f0497cc7640d364b16abc3e31c8b0dec8972092ffe1d64ff9a86fb33a32950"
        hex"829c92ba58cc637b90901950b536a208d7c7eedbb64828f5e827de76476d0dedd7d93256ac289f6fe522593072452ad1ca490d994b83095bd2f156f2"
        hex"8528843c6ea5d9c45aa2aca529b96e343c9a4327b14ecdcd476e1e46c172df8af394925bc59c8bb0de9692e1f7f193a42d32058cf5ba6e379e7912f1"
        hex"d167a0c3352b1578607809e6ee9e206aae1f4d32cf52cb934b8d378c6ae20619c37ab01d70f717aa92aa1033bf6fc710c01f527ceceae71e41e0f020"
        hex"1f521fad01fe81919b91fe7cf1fe6a625add810d157b9c21c1f5b2cac38f4a849ae8048299be48baff9e69f53d07e68009c6488088630bf05bb01028"
        hex"8c44b807b44c12f541766e4bc74ac12f21b4527c7fbd46fe90868984e248f397d217d7294d9024b6b4decea666eaa10a4a4c5aa9b385a870f2ebd1a2"
        hex"09f42fdf2cb7b903e7ba04315b9f8253aeb3333e7911d7813ba49e2360fa439bfd454112f6517635059f07705d4c2c8a797db0756a87fa59be3fa100"
        hex"095e0fcae42608eb608983b859b81786f224d36bb304b5a1ede0ff638998041fdcde955499500cd3c0b483fb3dac26127c2d0fd4542f3e7594da3f36"
        hex"53ebbb84aee53263332252c0846018e5aa972e4eb53d684f6903c2babe65009b0bd1880d3e44024a9732bc89cb7e2992435e2f68a49757e3a8836eaa"
        hex"b68294b5c5034bd81e83173f4d5162c0ddc5a77b6b93023443a07d12e9001b60766563f24107d79c94c526e850ceb0539cc0f3e03e831bc85a99f12e"
        hex"4c18662db2767b5d65d6b081a7a26460bf68481c96da146a22b00121bb0b3bd1d690f5e91d4c8dce0948e3bb566a05ceb53d00d8fd77cbcb0dd6060d"
        hex"c81006361fd839fe320ed8a9d44635408cd88488569c177afa010f1286012d702181b09d3dc8b5e335835223f88fa7394c06249293aa6ed1d6fb32eb"
        hex"40071f0136815ab11ef8b9ae9dd9b973436431db0746b1b6eb90d8d99f2b3661f90510f8144f72bf88429b044500e27c4583a9761122b06bee51e448"
        hex"ba7441cbd20806b2eb43673119cdea8e8a91522a36b6ff256a0e878d8d4d5e57ea7f8c22571c2647a4ec9d0380bd4becd2c3b38f91d2d5b3cabaef68"
        hex"683a5d5e2d8844b8a60013c3b5da720bfc1912a5f57b1e379bd3360864bf713d81915e0dbbadf302087512058ad83210fa9431645a815b11f38cce22"
        hex"747c4c0af728d32e69a2be11eff913f19ac2153bc38efcdbacbb621799e4bab338d4c283f320c43c60821b6061d3018e32dbbc7f4cbb75f518c188a8"
        hex"f36d7af64ccb5f687fe4f18f4aa3a111d7700de0029ff68d8834d9ff0a4b369f7cc769eaffa0197b1eca783ab1c59b158eaf0b7b113c8ca2711e59c7"
        hex"17690bc2f81e322ad2aef5e3a473555a924981fa05ad0260357923321210625080c96009c59dc41b66463faa42436fbbd13dc6a86a4302c02ae78fa5"
        hex"8099bd603b836a5733cef12d3e5eb193e2d9c767289f32389a441c314cc9af92d5d09d6efa865f157a0dfdf001dd4c046f860a11f310d0d18f25288f"
        hex"ed6427e98ba7be031efe24e5f7235c6d0f2ac090d9efd18cdb407c19d62a205e78622133aac5ec0230b79e414eeb64d82f6be742805e3750eda1be7d"
        hex"784318d0c12811e279186efac23f9f5a069bdcc19b7888c6df14c6f9710ebd8093cd2a52d86cf219eb0090350eadf586aa03b238b1636cb46d21bfd6"
        hex"cd4f4fa764400dea5c926d8e7d70b52a33ee0b2af3f03f7ae827f1cf774fc592beb62d8d6f8d0fa8b5aca9c7dc07671fa2e94c4d45ca8275fd582e28"
        hex"02390ee78b1dd73ad67d06a9196fde7ef51aa5edfd1e1bd404def08ce8c16de4b71beac3b4e49d45a2f1018c551dcd8b80d4df1095ba59ef11098bb9"
        hex"2d171ff1353a19bddd1be69b45d71a39fa5673b6be2453fe5e88441024dc7708c1fda99213e1eda72d99d8d6cd3b1e8db057ff940af5f5d26eeb43ce"
        hex"896d974997c5084f3267a09edf47a1fffa8808b50c479cd6b3978042fa9bf34ea7b4b00d97c8f0918dfd093ee9c703eb627c0aa5fee127fba65cfb5a"
        hex"830bc1ad7ae0280123b0be058166c17cff6e5a69f24c088de9c8072dfdb14e27463c345a8aaee6a1c6a8cbc4869443ab850d19f4b0110cbdc0eb47df"
        hex"e2783a2ab9ec1b568588eb41aab74886b2175a7397c0a98b510604";

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
