#!/usr/bin/env python3
"""Gnark-wrap partner proof_event_*.json for Ethereum withdrawByProof."""
from __future__ import annotations

import json
import subprocess
import sys
import tempfile
from pathlib import Path


def fr_hex_to_decimal(hex_str: str) -> str:
    return str(int.from_bytes(bytes.fromhex(hex_str.strip()), "little"))


def main() -> None:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} proof_event_NNN.json", file=sys.stderr)
        sys.exit(1)

    proof_path = Path(sys.argv[1]).resolve()
    repo = Path(__file__).resolve().parents[1]
    w4 = repo / "crates/bridge-prover-orchestrator/gnark-wrappers/circuit-4"

    data = json.loads(proof_path.read_text())
    backup = proof_path.with_suffix(proof_path.suffix + ".bak")
    if not backup.exists():
        backup.write_text(proof_path.read_text())

    pis = [fr_hex_to_decimal(h) for h in data["public_instances_hex"]]
    if len(pis) != 10:
        raise SystemExit(f"expected 10 public instances, got {len(pis)}")

    halo2 = {
        "public_inputs": pis,
        "proof_bytes": list(bytes.fromhex(data["proof_hex"])),
        "protocol": {
            "k": 19,
            "num_instance": [10],
            "num_witness": [],
            "num_challenge": [],
            "preprocessed_commitments": [],
        },
    }

    with tempfile.TemporaryDirectory() as tmp:
        j = Path(tmp) / "event_halo2.json"
        j.write_text(json.dumps(halo2, indent=2))
        subprocess.run(
            ["go", "run", ".", "prove", str(j)],
            cwd=w4,
            check=True,
            capture_output=True,
            text=True,
        )
        groth16 = (w4 / "groth16_proof.hex").read_text().strip().removeprefix("0x")

    data["proof_hex"] = groth16
    proof_path.write_text(json.dumps(data, indent=2) + "\n")
    print(f"patched {proof_path}")


if __name__ == "__main__":
    main()
