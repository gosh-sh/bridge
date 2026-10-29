pragma gosh-solidity >=0.80;

/// RLP and Merkle-Patricia trie proofs over Ethereum-format data: block
/// headers, receipt-trie proofs and `eth_getProof` account and storage
/// proofs. A `bytes` value is a chain of cells, so every read here moves to
/// the next cell when the current one is used up. Build it with a sold that
/// has the KECCAK256 instruction, as for EthBeaconLightClient.
library EthTrie {
    uint16 constant ERR_SHORT = 300;       // the data ends inside an item
    uint16 constant ERR_RLP = 301;         // an item has the wrong RLP kind or size
    uint16 constant ERR_NODE_HASH = 302;   // a proof node does not hash to its parent's reference
    uint16 constant ERR_PATH = 303;        // the proof does not follow the key
    uint16 constant ERR_INLINE = 304;      // an inline (under 32 bytes) child node: not supported
    uint16 constant ERR_ABSENT = 305;      // the key is not in the trie
    uint16 constant ERR_HEADER = 306;      // the header does not hash to the expected block hash
    uint16 constant ERR_NO_LOG = 307;      // no log from that emitter with that topic

    uint8 constant STR = 0;
    uint8 constant LIST = 1;
    uint8 constant BYTE = 2;               // a single byte below 0x80, its own payload

    // ---------------------------------------------------------------- reading

    function _byte(TvmSlice s) internal pure returns (uint8, TvmSlice) {
        if (s.bits() == 0) {
            require(s.refs() > 0, ERR_SHORT);
            s = s.loadRef().toSlice();
        }
        return (uint8(s.loadUint(8)), s);
    }

    function _skip(TvmSlice s, uint n) internal pure returns (TvmSlice) {
        while (n > 0) {
            if (s.bits() == 0) {
                require(s.refs() > 0, ERR_SHORT);
                s = s.loadRef().toSlice();
            }
            uint m = math.min(n, uint(s.bits()) / 8);
            s.skip(uint10(m * 8));
            n -= m;
        }
        return s;
    }

    /// A big-endian unsigned integer of `n` bytes (n <= 32).
    function _uint(TvmSlice s, uint n) internal pure returns (uint256 v, TvmSlice) {
        if (uint(s.bits()) >= n * 8) {
            if (n == 0) return (0, s);
            return (s.loadUint(uint9(n * 8)), s);
        }
        for (uint i = 0; i < n; i++) {
            uint8 b;
            (b, s) = _byte(s);
            v = (v << 8) | uint256(b);
        }
        return (v, s);
    }

    // -------------------------------------------------------------------- RLP

    /// The RLP header whose first byte is `p`. For BYTE, `len` is the byte.
    function _hdrFrom(uint8 p, TvmSlice s) internal pure returns (uint8 kind, uint len, uint hdr, TvmSlice) {
        if (p < 0x80) return (BYTE, uint(p), 1, s);
        if (p <= 0xb7) return (STR, uint(p) - 0x80, 1, s);
        uint n;
        uint v;
        if (p <= 0xbf) {
            n = uint(p) - 0xb7;
            (v, s) = _uint(s, n);
            return (STR, v, 1 + n, s);
        }
        if (p <= 0xf7) return (LIST, uint(p) - 0xc0, 1, s);
        n = uint(p) - 0xf7;
        (v, s) = _uint(s, n);
        return (LIST, v, 1 + n, s);
    }

    function _hdr(TvmSlice s) internal pure returns (uint8 kind, uint len, uint hdr, TvmSlice) {
        uint8 p;
        (p, s) = _byte(s);
        return _hdrFrom(p, s);
    }

    /// Skips one item and returns how many bytes it took.
    function _skipItem(TvmSlice s) internal pure returns (uint took, TvmSlice) {
        uint8 kind;
        uint len;
        uint hdr;
        (kind, len, hdr, s) = _hdr(s);
        if (kind == BYTE) return (1, s);
        s = _skip(s, len);
        return (hdr + len, s);
    }

    /// A 32-byte string item.
    function _hash(TvmSlice s) internal pure returns (uint256 v, TvmSlice) {
        uint8 kind;
        uint len;
        uint hdr;
        (kind, len, hdr, s) = _hdr(s);
        require(kind == STR && len == 32, ERR_RLP);
        return _uint(s, 32);
    }

    // ------------------------------------------------------------ trie proofs

    function _nib(uint256 key, uint nibbles, uint k) internal pure returns (uint8) {
        return uint8((key >> (4 * (nibbles - 1 - k))) & 0xf);
    }

    /// Walks `proof` (node RLPs, root first) from `root` along `key`, which
    /// holds `nibbles` nibbles right-aligned. Returns the leaf value: a slice at
    /// its payload and the payload length.
    function prove(uint256 root, uint256 key, uint nibbles, bytes[] proof)
        internal pure returns (TvmSlice, uint)
    {
        uint256 want = root;
        uint k = 0;
        uint8 kind;
        uint len;
        uint hdr;
        for (uint i = 0; i < proof.length; i++) {
            require(gosh.keccak256(proof[i]) == want, ERR_NODE_HASH);
            TvmSlice s = proof[i].toSlice();
            (kind, len, hdr, s) = _hdr(s);
            require(kind == LIST, ERR_RLP);
            TvmSlice start = s;
            uint t0;
            uint t1;
            (t0, s) = _skipItem(s);
            (t1, s) = _skipItem(s);
            if (t0 + t1 == len) {
                // Leaf or extension: [hex-prefix path, value or child].
                s = start;
                (kind, len, hdr, s) = _hdr(s);
                uint8 first;
                uint more;
                if (kind == BYTE) {
                    first = uint8(len);
                    more = 0;
                } else {
                    require(kind == STR && len > 0, ERR_RLP);
                    (first, s) = _byte(s);
                    more = len - 1;
                }
                uint8 flag = first >> 4;
                if (flag & 1 == 1) {
                    require(k < nibbles && _nib(key, nibbles, k) == (first & 0xf), ERR_PATH);
                    k++;
                }
                for (uint j = 0; j < more; j++) {
                    uint8 b;
                    (b, s) = _byte(s);
                    require(k + 1 < nibbles, ERR_PATH);
                    require(_nib(key, nibbles, k) == (b >> 4), ERR_PATH);
                    require(_nib(key, nibbles, k + 1) == (b & 0xf), ERR_PATH);
                    k += 2;
                }
                if (flag >= 2) {
                    require(k == nibbles && i + 1 == proof.length, ERR_PATH);
                    (kind, len, hdr, s) = _hdr(s);
                    require(kind == STR, ERR_RLP);
                    return (s, len);
                }
                (want, s) = _hash(s);
            } else {
                // Branch: 16 children and a value slot. No key here is a
                // prefix of another, so the value slot is never the answer.
                require(k < nibbles, ERR_PATH);
                uint8 at = _nib(key, nibbles, k);
                s = start;
                for (uint j = 0; j < at; j++) {
                    (t0, s) = _skipItem(s);
                }
                (kind, len, hdr, s) = _hdr(s);
                require(!(kind == STR && len == 0), ERR_ABSENT);
                require(kind == STR && len == 32, ERR_INLINE);
                (want, s) = _uint(s, 32);
                k++;
            }
        }
        revert(ERR_PATH);
    }

    // --------------------------------------------------------- Ethereum data

    /// `stateRoot` and `receiptsRoot` of a header RLP that hashes to `blockHash`.
    function headerRoots(bytes header, uint256 blockHash)
        internal pure returns (uint256 stateRoot, uint256 receiptsRoot)
    {
        require(gosh.keccak256(header) == blockHash, ERR_HEADER);
        TvmSlice s = header.toSlice();
        uint8 kind;
        uint len;
        uint hdr;
        uint t;
        (kind, len, hdr, s) = _hdr(s);
        require(kind == LIST, ERR_RLP);
        (t, s) = _skipItem(s);              // parentHash
        (t, s) = _skipItem(s);              // ommersHash
        (t, s) = _skipItem(s);              // beneficiary
        (stateRoot, s) = _hash(s);          // stateRoot
        (t, s) = _skipItem(s);              // transactionsRoot
        (receiptsRoot, s) = _hash(s);       // receiptsRoot
    }

    /// The receipt-trie key of transaction `index`: rlp(index), as nibbles.
    function receiptKey(uint index) internal pure returns (uint256 key, uint nibbles) {
        if (index == 0) return (0x80, 2);
        if (index < 0x80) return (index, 2);
        if (index < 0x100) return ((0x81 << 8) | index, 4);
        require(index < 0x10000, ERR_RLP);
        return ((0x82 << 16) | index, 6);
    }

    /// Word `word` of the data of the first log in a receipt (a receipt-trie
    /// value at `r`) emitted by `emitter` with topic0 `topic0`.
    function logWord(TvmSlice r, uint160 emitter, uint256 topic0, uint word)
        internal pure returns (uint256)
    {
        uint8 p;
        (p, r) = _byte(r);
        if (p < 0xc0) {
            (p, r) = _byte(r);              // typed receipt: drop the type byte
        }
        uint8 kind;
        uint len;
        uint hdr;
        uint t;
        (kind, len, hdr, r) = _hdrFrom(p, r);
        require(kind == LIST, ERR_RLP);
        (t, r) = _skipItem(r);              // status
        (t, r) = _skipItem(r);              // cumulativeGasUsed
        (t, r) = _skipItem(r);              // logsBloom
        uint logsLen;
        (kind, logsLen, hdr, r) = _hdr(r);
        require(kind == LIST, ERR_RLP);
        uint used = 0;
        while (used < logsLen) {
            uint logLen;
            (kind, logLen, hdr, r) = _hdr(r);
            require(kind == LIST, ERR_RLP);
            used += hdr + logLen;
            (kind, len, hdr, r) = _hdr(r);
            require(kind == STR && len == 20, ERR_RLP);
            uint256 addr;
            (addr, r) = _uint(r, 20);
            uint topicsLen;
            (kind, topicsLen, hdr, r) = _hdr(r);
            require(kind == LIST, ERR_RLP);
            uint256 t0 = 0;
            if (topicsLen > 0) {
                (t0, r) = _hash(r);
                r = _skip(r, topicsLen - 33);
            }
            uint dataLen;
            (kind, dataLen, hdr, r) = _hdr(r);
            if (kind == BYTE) {
                dataLen = 0;
            } else {
                require(kind == STR, ERR_RLP);
            }
            if (addr == uint256(emitter) && t0 == topic0 && dataLen >= (word + 1) * 32) {
                r = _skip(r, word * 32);
                uint256 v;
                (v, r) = _uint(r, 32);
                return v;
            }
            r = _skip(r, dataLen);
        }
        revert(ERR_NO_LOG);
    }

    /// Value of storage `slot` of `account` in the state `stateRoot`, from
    /// the two halves of an `eth_getProof` answer.
    function storageAt(
        uint256 stateRoot,
        uint160 account,
        bytes[] accountProof,
        uint256 slot,
        bytes[] storageProof
    ) internal pure returns (uint256 value) {
        TvmSlice a;
        uint alen;
        (a, alen) = prove(stateRoot, gosh.keccak256(bytes(bytes20(account))), 64, accountProof);
        uint8 kind;
        uint len;
        uint hdr;
        uint t;
        (kind, len, hdr, a) = _hdr(a);
        require(kind == LIST, ERR_RLP);
        (t, a) = _skipItem(a);              // nonce
        (t, a) = _skipItem(a);              // balance
        uint256 storageRoot;
        (storageRoot, a) = _hash(a);
        TvmSlice v;
        uint vlen;
        (v, vlen) = prove(storageRoot, gosh.keccak256(bytes(bytes32(slot))), 64, storageProof);
        (kind, len, hdr, v) = _hdr(v);
        if (kind == BYTE) return len;
        require(kind == STR && len <= 32, ERR_RLP);
        (value, v) = _uint(v, len);
    }
}
