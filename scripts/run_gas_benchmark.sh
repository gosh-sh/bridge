#!/usr/bin/env bash
# Measure AckiNackiBridge gas via audit/spec/ethereum/GasBenchmark.t.sol and
# write audit/reports/gas-cost-benchmark.md with USD estimates.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SPEC="$ROOT/audit/spec/ethereum"
REPORT="$ROOT/audit/reports/gas-cost-benchmark.md"
RAW="$ROOT/audit/reports/.gas-benchmark-raw.txt"
ENVF="$ROOT/audit/reports/.gas-benchmark-networks.env"
TS="$(date -u +"%Y-%m-%d %H:%M UTC")"

export PATH="${HOME}/.foundry/bin:${PATH}"

echo "[gas] compiling + running GasBenchmark..."
cd "$SPEC"
forge test --match-contract GasBenchmark -vv 2>&1 | tee "$RAW"

gas_price_gwei() {
  local rpc="$1"
  local wei
  if wei=$(cast gas-price --rpc-url "$rpc" 2>/dev/null); then
    python3 -c "print(f'{int(\"$wei\")/1e9:.4f}')"
  else
    echo ""
  fi
}

eth_usd=$(curl -fsS "https://api.coingecko.com/api/v3/simple/price?ids=ethereum&vs_currencies=usd" \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['ethereum']['usd'])" 2>/dev/null || echo "3500")

declare -A RPC=(
  [ethereum]="https://rpc.ankr.com/eth"
  [arbitrum]="https://arb1.arbitrum.io/rpc"
  [base]="https://mainnet.base.org"
  [optimism]="https://mainnet.optimism.io"
  [polygon]="https://polygon-bor-rpc.publicnode.com"
)

# Fallback gwei if RPC unreachable (documented in report)
declare -A FALLBACK_GWEI=(
  [ethereum]="15"
  [arbitrum]="0.05"
  [base]="0.01"
  [optimism]="0.01"
  [polygon]="30"
)

: > "$ENVF"
for net in ethereum arbitrum base optimism polygon; do
  gp=$(gas_price_gwei "${RPC[$net]}")
  if [[ -z "$gp" ]]; then
    gp="${FALLBACK_GWEI[$net]}"
    echo "${net}=${gp}" >> "$ENVF"
    echo "${net}_source=fallback" >> "$ENVF"
  else
    echo "${net}=${gp}" >> "$ENVF"
    echo "${net}_source=live" >> "$ENVF"
  fi
done

mapfile -t ROWS < <(grep -E 'GAS\|' "$RAW" | sed 's/.*GAS|/GAS|/' | sort -u)

python3 - "$REPORT" "$TS" "$eth_usd" "${ROWS[@]}" <<'PY'
import sys
from pathlib import Path

report_path = Path(sys.argv[1])
ts = sys.argv[2]
eth_usd = float(sys.argv[3])
rows = [r.strip() for r in sys.argv[4:] if r.strip().startswith("GAS|")]

gp = {}
gp_source = {}
for line in Path(report_path.parent / ".gas-benchmark-networks.env").read_text().splitlines():
    if "=" not in line:
        continue
    k, v = line.split("=", 1)
    v = v.strip()
    if not v:
        continue
    if k.endswith("_source"):
        gp_source[k.replace("_source", "")] = v
    else:
        gp[k] = float(v)

parsed = []
for r in rows:
    _, cat, op, var, gas = r.split("|", 4)
    parsed.append((cat, op, var, int(gas)))

lookup = {(c, o, v): g for c, o, v, g in parsed}

def usd_cost(gas: int, gwei: float) -> float:
    return gas * gwei * 1e-9 * eth_usd

nets = [n for n in ["ethereum", "arbitrum", "base", "optimism"] if n in gp]
polygon_gwei = gp.get("polygon")

lines = []
lines.append("# Gas cost benchmark — AckiNackiBridge (ETH L1 contracts)")
lines.append("")
lines.append(f"**Measured:** {ts}")
lines.append(f"**ETH/USD (Coingecko):** ${eth_usd:,.2f}")
lines.append("**Harness:** `audit/spec/ethereum/GasBenchmark.t.sol`")
lines.append("**Script:** `scripts/run_gas_benchmark.sh`")
lines.append("**Profile:** solc 0.8.19, `optimizer_runs=1`, `via_ir=true` (repo default — bytecode-size profile; production may tune `optimizer_runs` for runtime gas).")
lines.append("")
lines.append("## 1. Plan")
lines.append("")
lines.append("1. Inventory `AckiNackiBridge` entrypoints: user (`deposit`), relayer (`verifyBlock`, `withdrawByProof`), owner/AAVE, views.")
lines.append("2. Forge harness logs `GAS|category|operation|variant|gas` via `vm.snapshotGasLastCall`.")
lines.append("3. Production SHPLONK uses committed `contracts/ethereum/verifiers/*_calldata.bin`; mock paths isolate bridge logic.")
lines.append("4. Fetch live `eth_gasPrice` from public RPCs; USD = gas × gwei × 1e-9 × ETH/USD.")
lines.append("5. Document warm/cold, amount, AAVE pull, proof-size sensitivities.")
lines.append("")
lines.append("## 2. Script")
lines.append("")
lines.append("    ./scripts/run_gas_benchmark.sh")
lines.append("")
lines.append("## 3. Network gas prices at measurement")
lines.append("")
lines.append("| Network | Gas price (gwei) | Source |")
for net in nets:
    src = gp_source.get(net, "live")
    lines.append(f"| {net} | {gp[net]} | {src} |")
if polygon_gwei:
    src = gp_source.get("polygon", "live")
    lines.append(f"| polygon (MATIC gas units) | {polygon_gwei} | {src} |")
lines.append("")
lines.append("> USD table below uses ETH/USD for ethereum/arbitrum/base/optimism only. Polygon native gas is MATIC — not converted here.")
lines.append("")
lines.append("## 4. Measurements (gas)")
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
lines.append("## 5. USD estimates")
lines.append("")
lines.append("| Operation | Variant | Gas | " + " | ".join(nets) + " |")
key_ops = [
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
for c, o, v in key_ops:
    g = lookup.get((c, o, v))
    if g is None:
        continue
    cols = [f"${usd_cost(g, gp[n]):.4f}" for n in nets]
    lines.append(f"| {o} | {v} | {g:,} | " + " | ".join(cols) + " |")
lines.append("")
lines.append("## 6. Parameter dependencies")
lines.append("")
lines.append("| Parameter | Affects | Direction |")
lines.append("| Deposit amount | deposit | ~flat for USDC |")
lines.append("| Warm storage | deposit, verifyBlock, withdraw | 2nd call much cheaper |")
lines.append("| SHPLONK proof (calldata size) | verifyBlock, withdraw | dominates; fixed per circuit artefact |")
lines.append("| numLayers (1..10) | verifyBlock | weak linear (≤10 SSTORE) |")
lines.append("| AAVE utilization | withdrawByProof | +gas when liquid USDC < payout |")
lines.append("| L1 calldata | proof txs on Ethereum | not in execution gas table; budget ~16 gas/non-zero byte separately |")
lines.append("")
lines.append("## 7. Relayer / user budgeting")
lines.append("")
eth_g = gp.get("ethereum")
if eth_g:
    for label, key in [
        ("ETH→AN deposit (bridge only)", ("user", "deposit", "first_10usdc")),
        ("ETH→AN deposit (warm)", ("user", "deposit", "warm_10usdc")),
        ("AN→ETH verifyBlock (production 1A+2)", ("relayer", "verifyBlock", "production_primary")),
        ("C4 verify (isolated)", ("verifier", "withdrawal_c4", "isolated")),
        ("withdrawByProof bridge overhead (mock)", ("relayer", "withdrawByProof", "mock_liquid_only")),
    ]:
        g = lookup.get(key, 0)
        if g:
            lines.append(f"- **{label}:** {g:,} gas ≈ ${usd_cost(g, eth_g):.4f} on Ethereum @ {eth_g} gwei")
mock_w = lookup.get(("relayer", "withdrawByProof", "mock_liquid_only"), 0)
c4 = lookup.get(("verifier", "withdrawal_c4", "isolated"), 0)
if mock_w and c4:
    lines.append(f"- **withdrawByProof (prod estimate):** ≈ {c4 + mock_w:,} gas (C4 isolated + mock bridge overhead; not yet full E2E on bridge).")
lines.append("")
lines.append("Raw log: `audit/reports/.gas-benchmark-raw.txt`")

report_path.write_text("\n".join(lines) + "\n")
print(f"Report: {report_path}")
PY

echo "[gas] done → $REPORT"
