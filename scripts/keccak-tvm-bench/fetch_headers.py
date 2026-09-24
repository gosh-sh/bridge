#!/usr/bin/env python3
"""Header RLPs for the keccak bench, built the way eth-lc-relayer builds them
(pure python RLP + keccak-256, no third-party modules; each RLP is checked
against the node's own block hash before it is written).

  fetch_headers.py <block hash> [count] [out.json]   # walk parents via RPC
  fetch_headers.py --fields <headers.json>            # print rlp / hash / parent of [0]

Accepts the hash as Ethereum bytes or in the light client's anchor form
(16-byte halves byte-reversed) and tries both. RPC from $ETH_RPC_URL."""
import json, sys, urllib.request
import os
RPC = os.environ.get("ETH_RPC_URL", "https://ethereum-sepolia-rpc.publicnode.com")
RC = [0x0000000000000001,0x0000000000008082,0x800000000000808A,0x8000000080008000,0x000000000000808B,0x0000000080000001,0x8000000080008081,0x8000000000008009,0x000000000000008A,0x0000000000000088,0x0000000080008009,0x000000008000000A,0x000000008000808B,0x800000000000008B,0x8000000000008089,0x8000000000008003,0x8000000000008002,0x8000000000000080,0x000000000000800A,0x800000008000000A,0x8000000080008081,0x8000000000008080,0x0000000080000001,0x8000000080008008]
ROT = [[0,36,3,41,18],[1,44,10,45,2],[62,6,43,15,61],[28,55,25,21,56],[27,20,39,8,14]]
M = (1<<64)-1
def rol(v,n): return ((v<<n)|(v>>(64-n)))&M if n else v
def keccak_f(A):
    for rc in RC:
        C=[A[x][0]^A[x][1]^A[x][2]^A[x][3]^A[x][4] for x in range(5)]
        D=[C[(x-1)%5]^rol(C[(x+1)%5],1) for x in range(5)]
        A=[[A[x][y]^D[x] for y in range(5)] for x in range(5)]
        B=[[0]*5 for _ in range(5)]
        for x in range(5):
            for y in range(5):
                B[y][(2*x+3*y)%5]=rol(A[x][y],ROT[x][y])
        A=[[B[x][y]^((~B[(x+1)%5][y])&B[(x+2)%5][y]) for y in range(5)] for x in range(5)]
        A[0][0]^=rc
    return A
def keccak256(data):
    rate=136; d=bytearray(data); d.append(0x01)
    while len(d)%rate: d.append(0)
    d[-1]|=0x80
    A=[[0]*5 for _ in range(5)]
    for off in range(0,len(d),rate):
        blk=d[off:off+rate]
        for i in range(rate//8):
            A[i%5][i//5]^=int.from_bytes(blk[8*i:8*i+8],'little')
        A=keccak_f(A)
    return b''.join(A[i%5][i//5].to_bytes(8,'little') for i in range(4))
assert keccak256(b'').hex()=="c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
assert keccak256(b'abc').hex()=="4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45"
def rlp_len(n,off):
    if n<56: return bytes([off+n])
    l=n.to_bytes((n.bit_length()+7)//8,'big'); return bytes([off+55+len(l)])+l
def rlp_b(b):
    if len(b)==1 and b[0]<0x80: return bytes(b)
    return rlp_len(len(b),0x80)+bytes(b)
def rlp_list(items): body=b''.join(items); return rlp_len(len(body),0xc0)+body
def hx(s):
    s=s[2:] if s.startswith('0x') else s
    if len(s)%2: s='0'+s
    return bytes.fromhex(s)
def uint(s): b=hx(s).lstrip(b'\0'); return b
def rpc(method,params):
    req=urllib.request.Request(RPC,data=json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode(),headers={"content-type":"application/json","user-agent":"curl/8.5.0"})
    return json.load(urllib.request.urlopen(req,timeout=30))
def encode_header(b):
    fields=[rlp_b(hx(b[k])) for k in ("parentHash","sha3Uncles","miner","stateRoot","transactionsRoot","receiptsRoot","logsBloom")]
    fields+=[rlp_b(uint(b[k])) for k in ("difficulty","number","gasLimit","gasUsed","timestamp")]
    fields+=[rlp_b(hx(b.get("extraData") or "0x")), rlp_b(hx(b["mixHash"])), rlp_b(hx(b["nonce"]))]
    for k in ("baseFeePerGas","withdrawalsRoot","blobGasUsed","excessBlobGas","parentBeaconBlockRoot","requestsHash"):
        v=b.get(k)
        if v in (None,"","0x"): continue
        raw=hx(v)
        fields.append(rlp_b(raw) if len(raw) in (20,32,256) else rlp_b(raw.lstrip(b'\0')))
    rlp=rlp_list(fields)
    got=keccak256(rlp).hex()
    assert got==b["hash"][2:], f"keccak {got} != {b['hash']}"
    return rlp
def flip(h):
    a=bytes.fromhex(h[:32])[::-1].hex(); b=bytes.fromhex(h[32:])[::-1].hex(); return a+b

def rlp_parent(rlp_hex):
    r = bytes.fromhex(rlp_hex); p = r[0]; skip = p - 0xf7 if p > 0xf7 else 0
    assert r[1 + skip] == 0xa0, "first RLP item is not a 32-byte parentHash"
    return r[2 + skip:34 + skip].hex()

if len(sys.argv) >= 3 and sys.argv[1] == "--fields":
    rlp0 = json.load(open(sys.argv[2]))[0]
    print(rlp0, keccak256(bytes.fromhex(rlp0)).hex(), rlp_parent(rlp0))
    sys.exit(0)
logged=sys.argv[1].removeprefix('0x'); n=int(sys.argv[2]) if len(sys.argv)>2 else 32
start=None
for cand,label in ((logged,"as logged"),(flip(logged),"LE-halves flipped")):
    r=rpc("eth_getBlockByHash",["0x"+cand,False])["result"]
    if r: start=r; print(f"block found with hash {label}: 0x{cand} number={int(r['number'],16)}"); break
if not start: sys.exit("no block found for either form")
rlps=[]; b=start
while len(rlps)<n:
    rlps.append(encode_header(b).hex())
    b=rpc("eth_getBlockByHash",[b["parentHash"],False])["result"]
print(f"built {len(rlps)} headers, sizes {min(map(len,rlps))//2}..{max(map(len,rlps))//2} bytes, total {sum(map(len,rlps))//2} bytes")
out=sys.argv[3] if len(sys.argv)>3 else f"anc{n}.json"
json.dump(rlps,open(out,"w")); print("wrote",out)
