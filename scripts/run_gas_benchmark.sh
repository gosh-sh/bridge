#!/usr/bin/env bash
# Measure AckiNackiBridge gas via audit/spec/ethereum/GasBenchmark.t.sol and
# write audit/reports/gas-cost-benchmark.md with USD estimates.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SPEC="$ROOT/audit/spec/ethereum"
REPORT="$ROOT/audit/reports/gas-cost-benchmark.md"
RAW="$ROOT/audit/reports/.gas-benchmark-raw.txt"
META="$ROOT/audit/reports/.gas-benchmark-networks.json"
TS="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

export PATH="${HOME}/.foundry/bin:${PATH}"

echo "[gas] compiling + running GasBenchmark..."
cd "$SPEC"
forge test --match-contract GasBenchmark -vv 2>&1 | tee "$RAW"

fetch_gas_wei() {
  local rpc="$1"
  cast gas-price --rpc-url "$rpc" 2>/dev/null || true
}

fetch_block() {
  local rpc="$1"
  cast block-number --rpc-url "$rpc" 2>/dev/null || true
}

# network metadata: name|native_token|coingecko_id|rpc1,rpc2,...|fallback_gwei
read -r -d '' NETWORKS <<'EOF' || true
ethereum|ETH|ethereum|https://ethereum.publicnode.com,https://rpc.ankr.com/eth,https://1rpc.io/eth,https://eth.llamarpc.com|20
sepolia|ETH|ethereum|https://ethereum-sepolia.publicnode.com,https://rpc.sepolia.org,https://1rpc.io/sepolia|5
arbitrum|ETH|ethereum|https://arb1.arbitrum.io/rpc,https://arbitrum-one.publicnode.com,https://1rpc.io/arb|0.05
base|ETH|ethereum|https://mainnet.base.org,https://base.publicnode.com,https://1rpc.io/base|0.01
optimism|ETH|ethereum|https://mainnet.optimism.io,https://optimism.publicnode.com,https://1rpc.io/op|0.01
polygon|POL|polygon-ecosystem-token|https://polygon-bor-rpc.publicnode.com,https://polygon-rpc.com,https://1rpc.io/matic|30
EOF

python3 - "$META" "$TS" "$NETWORKS" <<'PY'
import json, subprocess, sys, urllib.request
from datetime import datetime, timezone

meta_path, ts, networks_blob = sys.argv[1:4]

def curl_json(url: str) -> dict:
    req = urllib.request.Request(url, headers={"User-Agent": "acki-nacki-bridge-gas-benchmark/1.0"})
    with urllib.request.urlopen(req, timeout=15) as r:
        return json.loads(r.read().decode())

def cast(args: list[str]) -> str:
    try:
        out = subprocess.check_output(["cast", *args], stderr=subprocess.DEVNULL, text=True).strip()
        return out
    except subprocess.CalledProcessError:
        return ""

# Token USD at measurement time (single Coingecko call)
cg_url = (
    "https://api.coingecko.com/api/v3/simple/price"
    "?ids=ethereum,polygon-ecosystem-token&vs_currencies=usd"
)
cg = curl_json(cg_url)
prices = {
    "ethereum": float(cg["ethereum"]["usd"]),
    "polygon-ecosystem-token": float(cg["polygon-ecosystem-token"]["usd"]),
}
fetched_prices_at = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

networks = []
for line in networks_blob.strip().splitlines():
    name, native, cg_id, rpcs_csv, fallback = line.split("|")
    rpcs = [r.strip() for r in rpcs_csv.split(",") if r.strip()]
    entry = {
        "network": name,
        "native_token": native,
        "coingecko_id": cg_id,
        "native_token_usd": prices[cg_id],
        "rpc_candidates": rpcs,
        "gas_price_wei": None,
        "gas_price_gwei": None,
        "block_number": None,
        "rpc_used": None,
        "source": "unavailable",
        "fallback_gwei": float(fallback),
        "fetched_at_utc": fetched_prices_at,
    }
    for rpc in rpcs:
        wei = cast(["gas-price", "--rpc-url", rpc])
        if not wei or not wei.isdigit():
            continue
        block = cast(["block-number", "--rpc-url", rpc])
        entry["gas_price_wei"] = int(wei)
        entry["gas_price_gwei"] = int(wei) / 1e9
        entry["block_number"] = int(block) if block.isdigit() else None
        entry["rpc_used"] = rpc
        entry["source"] = "live_rpc"
        entry["fetched_at_utc"] = fetched_prices_at
        break
    if entry["source"] != "live_rpc":
        gwei = entry["fallback_gwei"]
        wei = int(gwei * 1e9)
        entry["gas_price_wei"] = wei
        entry["gas_price_gwei"] = gwei
        entry["source"] = "fallback_estimate"
        entry["rpc_used"] = None
    networks.append(entry)

payload = {
    "measurement_utc": ts,
    "price_source": {
        "provider": "CoinGecko",
        "url": cg_url,
        "fetched_at_utc": fetched_prices_at,
        "ethereum_usd": prices["ethereum"],
        "polygon_pol_usd": prices["polygon-ecosystem-token"],
    },
    "gas_price_method": "eth_gasPrice via cast (JSON-RPC)",
    "networks": networks,
}
meta_path = __import__("pathlib").Path(meta_path)
meta_path.write_text(json.dumps(payload, indent=2) + "\n")
print(f"Wrote {meta_path}")
PY

mapfile -t ROWS < <(grep -E 'GAS\|' "$RAW" | sed 's/.*GAS|/GAS|/' | sort -u)

python3 - "$REPORT" "$META" "${ROWS[@]}" <<'PY'
import json, sys
from pathlib import Path

report_path = Path(sys.argv[1])
meta = json.loads(Path(sys.argv[2]).read_text())
rows = [r.strip() for r in sys.argv[3:] if r.strip().startswith("GAS|")]

parsed = []
for r in rows:
    _, cat, op, var, gas = r.split("|", 4)
    parsed.append((cat, op, var, int(gas)))
lookup = {(c, o, v): g for c, o, v, g in parsed}

def usd(gas: int, gwei: float, token_usd: float) -> float:
    return gas * gwei * 1e-9 * token_usd

nets = meta["networks"]
eth_usd = meta["price_source"]["ethereum_usd"]
ts = meta["measurement_utc"]
price_ts = meta["price_source"]["fetched_at_utc"]

lines = []
lines.append("# Gas cost benchmark — AckiNackiBridge (ETH L1 contracts)")
lines.append("")
lines.append(f"**Gas measurements (Forge):** {ts}")
lines.append(f"**Price snapshot (CoinGecko):** {price_ts}")
lines.append(f"**ETH/USD:** ${eth_usd:,.4f}")
lines.append(f"**POL/USD:** ${meta['price_source']['polygon_pol_usd']:,.4f}")
lines.append("**Harness:** `audit/spec/ethereum/GasBenchmark.t.sol`")
lines.append("**Script:** `scripts/run_gas_benchmark.sh`")
lines.append("**Provenance JSON:** `audit/reports/.gas-benchmark-networks.json`")
lines.append("**Profile:** solc 0.8.19, `optimizer_runs=1`, `via_ir=true`")
lines.append("")
lines.append("## 1. Methodology")
lines.append("")
lines.append("1. **Execution gas** — Foundry `vm.snapshotGasLastCall` on local EVM (same gas units on all EVM chains).")
lines.append("2. **Gas price** — `cast gas-price` → JSON-RPC `eth_gasPrice` at report generation time.")
lines.append("3. **USD** — `cost = gas_used × gas_price_gwei × 10⁻⁹ × native_token_usd`.")
lines.append("4. **L2 (Arbitrum / Base / Optimism)** — native gas token is ETH; same ETH/USD as mainnet.")
lines.append("5. **Polygon** — native gas token is POL; uses POL/USD (`polygon-ecosystem-token`) from CoinGecko.")
lines.append("6. **Sepolia** — testnet gas price for operator estimates only (not mainnet USD planning).")
lines.append("")
lines.append("Re-run anytime: `./scripts/run_gas_benchmark.sh` (prices are point-in-time, not historical).")
lines.append("")
lines.append("## 2. Network parameters (live at measurement)")
lines.append("")
lines.append("| Network | Gas (gwei) | Gas (wei) | Block | Native | USD/token | Source | RPC |")
for n in nets:
    gwei = f"{n['gas_price_gwei']:.6f}" if n["gas_price_gwei"] is not None else "n/a"
    wei = str(n["gas_price_wei"]) if n["gas_price_wei"] is not None else "n/a"
    block = str(n["block_number"]) if n["block_number"] is not None else "n/a"
    tok = n["native_token"]
    usd_tok = f"${n['native_token_usd']:.4f}"
    src = n["source"]
    rpc = n["rpc_used"] or f"fallback {n['fallback_gwei']} gwei"
    lines.append(f"| {n['network']} | {gwei} | {wei} | {block} | {tok} | {usd_tok} | {src} | {rpc} |")
lines.append("")
lines.append("**Price source:** CoinGecko simple price API (`ethereum`, `polygon-ecosystem-token`).")
lines.append("")
lines.append("**Note:** Ethereum mainnet row uses live RPC when reachable; if all RPCs fail, script uses fallback estimate (marked `fallback_estimate`). L2 `eth_gasPrice` is often <0.1 gwei — USD looks small but is correct for current fee market.")
lines.append("")
lines.append("## 3. Execution gas (Forge, chain-independent)")
lines.append("")
lines.append("| Category | Operation | Variant | Gas | Notes |")
for cat, op, var, gas in sorted(parsed, key=lambda x: (x[0], x[1], x[2])):
    notes = []
    if "mock" in var and cat == "relayer":
        notes.append("mock crypto")
    if "production" in var or cat == "verifier":
        notes.append("SHPLONK prod calldata")
    if "warm" in var:
        notes.append("warm storage")
    if "aave" in var:
        notes.append("AAVE pull")
    if var == "max_100usdc":
        notes.append("MAX_DEPOSIT_AMOUNT")
    lines.append(f"| {cat} | {op} | {var} | {gas:,} | {', '.join(notes)} |")
lines.append("")
lines.append("## 4. USD cost — key operations (all networks)")
lines.append("")
net_names = [n["network"] for n in nets]
lines.append("| Operation | Variant | Gas | " + " | ".join(net_names) + " |")
key_ops = [
    ("user", "erc20_approve", "10usdc"),
    ("user", "deposit", "first_10usdc"),
    ("user", "deposit", "warm_10usdc"),
    ("relayer", "verifyBlock", "production_primary"),
    ("verifier", "withdrawal_c4", "isolated"),
    ("relayer", "withdrawByProof", "mock_liquid_only"),
    ("owner", "supplyToAave", "max"),
]
for c, o, v in key_ops:
    g = lookup.get((c, o, v))
    if g is None:
        continue
    cols = []
    for n in nets:
        cols.append(f"${usd(g, n['gas_price_gwei'], n['native_token_usd']):.4f}")
    lines.append(f"| {o} | {v} | {g:,} | " + " | ".join(cols) + " |")
lines.append("")
lines.append("## 5. USD cost — full matrix")
lines.append("")
lines.append("| Operation | Variant | Gas | " + " | ".join(net_names) + " |")
full_ops = [
    ("user", "erc20_approve", "10usdc"),
    ("user", "deposit", "first_10usdc"),
    ("user", "deposit", "warm_10usdc"),
    ("user", "deposit", "max_100usdc"),
    ("relayer", "verifyBlock", "mock_primary_first"),
    ("relayer", "verifyBlock", "production_primary"),
    ("verifier", "primary_attestation", "isolated"),
    ("verifier", "layer_hashes", "isolated"),
    ("verifier", "fallback_attestation", "isolated"),
    ("verifier", "withdrawal_c4", "isolated"),
    ("relayer", "withdrawByProof", "mock_liquid_only"),
    ("relayer", "withdrawByProof", "mock_with_aave_pull"),
    ("owner", "supplyToAave", "max"),
    ("owner", "harvestYield", "1usdc"),
    ("owner", "emergencyWithdrawAll", "default"),
]
for c, o, v in full_ops:
    g = lookup.get((c, o, v))
    if g is None:
        continue
    cols = [f"${usd(g, n['gas_price_gwei'], n['native_token_usd']):.4f}" for n in nets]
    lines.append(f"| {o} | {v} | {g:,} | " + " | ".join(cols) + " |")
lines.append("")
lines.append("## 6. Parameter dependencies")
lines.append("")
lines.append("| Parameter | Affects | Direction |")
lines.append("| Deposit amount | deposit | ~flat for USDC |")
lines.append("| Warm storage | deposit, verifyBlock, withdraw | 2nd call ~10× cheaper |")
lines.append("| SHPLONK proof size | verifyBlock, withdraw | dominates (~1.28M gas prod) |")
lines.append("| AAVE pull | withdrawByProof | +~18% vs liquid-only mock |")
lines.append("| Gas market | USD columns | re-run script; see section 2 |")
lines.append("")
lines.append("## 7. Relayer budgeting snapshot")
lines.append("")
for label, key in [
    ("deposit (first)", ("user", "deposit", "first_10usdc")),
    ("deposit (warm)", ("user", "deposit", "warm_10usdc")),
    ("verifyBlock production", ("relayer", "verifyBlock", "production_primary")),
    ("withdrawal C4 verify", ("verifier", "withdrawal_c4", "isolated")),
]:
    g = lookup.get(key, 0)
    if not g:
        continue
    parts = []
    for n in nets:
        c = usd(g, n["gas_price_gwei"], n["native_token_usd"])
        parts.append(f"{n['network']} ${c:.4f} @ {n['gas_price_gwei']:.4f} gwei")
    lines.append(f"- **{label}** ({g:,} gas): " + "; ".join(parts))
mock_w = lookup.get(("relayer", "withdrawByProof", "mock_liquid_only"), 0)
c4 = lookup.get(("verifier", "withdrawal_c4", "isolated"), 0)
if mock_w and c4:
    lines.append(f"- **withdrawByProof prod estimate:** {c4 + mock_w:,} gas (C4 + bridge overhead; E2E pending)")
lines.append("")
lines.append("Raw forge log: `audit/reports/.gas-benchmark-raw.txt`")

report_path.write_text("\n".join(lines) + "\n")
print(f"Report: {report_path}")
PY

echo "[gas] done → $REPORT"
