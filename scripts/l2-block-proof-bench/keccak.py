# Minimal Keccak-256 (Ethereum flavour) for function selectors.
RC=[0x1,0x8082,0x800000000000808A,0x8000000080008000,0x808B,0x80000001,0x8000000080008081,0x8000000000008009,0x8A,0x88,0x80008009,0x8000000A,0x8000808B,0x800000000000008B,0x8000000000008089,0x8000000000008003,0x8000000000008002,0x8000000000000080,0x800A,0x800000008000000A,0x8000000080008081,0x8000000000008080,0x80000001,0x8000000080008008]
R=[[0,36,3,41,18],[1,44,10,45,2],[62,6,43,15,61],[28,55,25,21,56],[27,20,39,8,14]]
M=(1<<64)-1
rol=lambda x,n:((x<<n)|(x>>(64-n)))&M if n else x
def f(A):
    for rc in RC:
        C=[A[x][0]^A[x][1]^A[x][2]^A[x][3]^A[x][4] for x in range(5)]
        D=[C[(x-1)%5]^rol(C[(x+1)%5],1) for x in range(5)]
        A=[[A[x][y]^D[x] for y in range(5)] for x in range(5)]
        B=[[0]*5 for _ in range(5)]
        for x in range(5):
            for y in range(5): B[y][(2*x+3*y)%5]=rol(A[x][y],R[x][y])
        A=[[B[x][y]^((~B[(x+1)%5][y])&B[(x+2)%5][y]) for y in range(5)] for x in range(5)]
        A[0][0]^=rc
    return A
def keccak256(data:bytes)->bytes:
    rate=136; p=bytearray(data)+b'\x01'
    while len(p)%rate: p+=b'\x00'
    p[-1]|=0x80
    A=[[0]*5 for _ in range(5)]
    for o in range(0,len(p),rate):
        for i in range(rate//8):
            x,y=i%5,i//5; A[x][y]^=int.from_bytes(p[o+8*i:o+8*i+8],'little')
        A=f(A)
    return b''.join(A[i%5][i//5].to_bytes(8,'little') for i in range(4))
if __name__=='__main__':
    import sys
    assert keccak256(b'').hex()=='c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470'
    for s in sys.argv[1:]: print('0x'+keccak256(s.encode())[:4].hex(), s)
