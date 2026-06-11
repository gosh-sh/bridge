#!/usr/bin/env python3
"""Convert partner proof_<seqno>.json Halo2 hex → 256-byte Groth16 for Ethereum submit.

Uses identity-stub gnark wrappers in bridge-prover-orchestrator (cached proving.key).
Patches primary_proof_hex / layer_proof_hex in-place; writes .bak first.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


def fr_hex_to_decimal(hex_str: str) -> str:
    return str(int.from_bytes(bytes.fromhex(hex_str.strip()), "little"))


def build_halo2_json(
    public_inputs_decimal: list[str],
    proof_hex: str,
    k: int,
) -> dict:
    proof_bytes = list(bytes.fromhex(proof_hex))
    return {
        "public_inputs": public_inputs_decimal,
        "proof_bytes": proof_bytes,
        "protocol": {
            "k": k,
            "num_instance": [len(public_inputs_decimal)],
            "num_witness": [],
            "num_challenge": [],
            "preprocessed_commitments": [],
        },
    }


def run_gnark_prove(wrapper_dir: Path, halo2_path: Path) -> str:
    subprocess.run(
        ["go", "run", ".", "prove", str(halo2_path)],
        cwd=wrapper_dir,
        check=True,
        capture_output=True,
        text=True,
    )
    proof_hex = (wrapper_dir / "groth16_proof.hex").read_text().strip()
    if proof_hex.startswith("0x"):
        proof_hex = proof_hex[2:]
    if len(bytes.fromhex(proof_hex)) != 256:
        raise SystemExit(f"expected 256-byte Groth16, got {len(bytes.fromhex(proof_hex))}")
    return proof_hex


def main() -> None:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} proof_<seqno>.json", file=sys.stderr)
        sys.exit(1)

    proof_path = Path(sys.argv[1]).resolve()
    repo = Path(__file__).resolve().parents[1]
    w1a = repo / "crates/bridge-prover-orchestrator/gnark-wrappers/circuit-1a"
    w2 = repo / "crates/bridge-prover-orchestrator/gnark-wrappers/circuit-2"

    data = json.loads(proof_path.read_text())
    backup = proof_path.with_suffix(proof_path.suffix + ".bak")
    if not backup.exists():
        backup.write_text(proof_path.read_text())

    # Circuit 1A PI [3] must match on-chain `storedLastSeenBlockSeqNo` at
    # submit time (what `PrimaryVerifier` forwards), NOT the partner JSON's
    # `last_seen_block_seqno` field (AN-side previous key block).
    last_seen_pi = os.environ.get("GNARK_LAST_SEEN_PI")
    if last_seen_pi is None:
        last_seen_pi = str(data["last_seen_block_seqno"])
    primary_pi = [
        fr_hex_to_decimal(data["block_id_hex"]),
        fr_hex_to_decimal(data["bk_set_poseidon_hash_hex"]),
        str(data["block_seq_no"]),
        last_seen_pi,
    ]
    # Circuit 2 PI [0] must match the single `blockId` passed to `verifyBlock`
    # (cross-circuit CC-1 binding), not `layer_block_id_hex` alone.
    layer_pi = [
        fr_hex_to_decimal(data["block_id_hex"]),
        fr_hex_to_decimal(data["bk_set_poseidon_hash_hex"]),
        str(data["num_layers"]),
        *[fr_hex_to_decimal(h) for h in data["layer_hash_frs_hex"][:10]],
        fr_hex_to_decimal(data["prev_max_level_layer_hash_hex"]),
    ]
    if len(layer_pi) != 14:
        raise SystemExit(f"expected 14 layer public inputs, got {len(layer_pi)}")

    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        p_json = tmp_path / "primary_halo2.json"
        l_json = tmp_path / "layer_halo2.json"
        p_json.write_text(
            json.dumps(
                build_halo2_json(primary_pi, data["primary_proof_hex"], k=20),
                indent=2,
            )
        )
        l_json.write_text(
            json.dumps(
                build_halo2_json(layer_pi, data["layer_proof_hex"], k=17),
                indent=2,
            )
        )
        data["primary_proof_hex"] = run_gnark_prove(w1a, p_json)
        data["layer_proof_hex"] = run_gnark_prove(w2, l_json)

    proof_path.write_text(json.dumps(data, indent=2) + "\n")
    print(f"patched {proof_path}")
    print(f"backup at {backup}")


if __name__ == "__main__":
    main()
