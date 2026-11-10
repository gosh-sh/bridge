#!/bin/sh
# Download the released compilers and TVM tools the Acki Nacki contracts are
# built and tested with, check each archive against its pinned SHA-256, and
# unpack the binaries into one directory:
#
#   scripts/fetch_an_toolchain.sh <dir>
#
# <dir>/sold-0.80.1, <dir>/sold-0.81.0, <dir>/sold-0.81.1  the compilers
#     contracts/an/exchange/Makefile pins (SOLD_0_80_1, SOLD_0_81, SOLD_0_81_1);
# <dir>/tvm-debugger, <dir>/tvm-cli  tvm-sdk 3.1.0.an, which runs KECCAK256.
#
# Linux x86_64 only: this is what the an-contracts pipeline runs on. An archive
# whose hash differs from the one pinned here is refused, so a re-tagged
# release cannot change what CI builds with.
set -eu

dir=${1:?usage: fetch_an_toolchain.sh <dir>}
mkdir -p "$dir"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

sold=https://github.com/gosh-sh/TVM-Solidity-Compiler/releases/download
sdk=https://github.com/tvmlabs/tvm-sdk/releases/download/v3.1.0.an

# name in <dir> | archive URL | SHA-256 of the archive | binary inside it
while read -r name url sum bin; do
    [ -n "$name" ] || continue
    archive="$tmp/$name.tar.gz"
    curl -fsSL --retry 3 -o "$archive" "$url"
    echo "$sum  $archive" | sha256sum -c - >/dev/null || {
        echo "$url: SHA-256 is not $sum" >&2
        exit 1
    }
    mkdir -p "$tmp/$name"
    tar -xzf "$archive" -C "$tmp/$name"
    found=$(find "$tmp/$name" -type f -name "$bin" | head -n 1)
    [ -n "$found" ] || { echo "$url: no $bin inside" >&2; exit 1; }
    install -m 0755 "$found" "$dir/$name"
done <<EOF
sold-0.80.1 $sold/gosh_0.80.1/sold_gosh_0.80.1_linux_x86_64.tar.gz e05224faf94126ff188f13e98102c3f7bb89280e428ffb30dedabc1e4154e3f5 sold
sold-0.81.0 $sold/gosh_0.81.0/sold_gosh_0.81.0_linux_x86_64.tar.gz 79b43aab6ae2fdc35c1cb3b486453aa2cf4f2718c58b69ccefab1122c704853c sold
sold-0.81.1 $sold/gosh_0.81.1/sold_gosh_0.81.1_linux_x86_64.tar.gz 4931c4f14e5454ed6d95a5b805246fda94f16d90eae56e956e444e8482532479 sold
tvm-debugger $sdk/tvm-debugger-3.1.0.an-linux-musl-amd64.tar.gz 353d955aea67a10ec3849437d4f950b5206dee55ed68e1e1ab836b15124f8146 tvm-debugger
tvm-cli $sdk/tvm-cli-3.1.0.an-linux-musl-amd64.tar.gz 321b2cc51277c373c59f6a7f3da63ed5e073128fb67b9763329af03c4cf75e73 tvm-cli
EOF

for b in sold-0.80.1 sold-0.81.0 sold-0.81.1; do "$dir/$b" --version | tail -n 1; done
"$dir/tvm-debugger" --version
"$dir/tvm-cli" --version | head -n 1
