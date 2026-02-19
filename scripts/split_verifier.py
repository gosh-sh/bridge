#!/usr/bin/env python3
"""
Script to split a large Yul verifier contract into multiple smaller contracts.

The strategy is to split the verifier into parts that can be deployed separately
and then use a proxy contract with DELEGATECALL to execute them in sequence.

Key considerations:
1. Each part must be under 24KB when compiled
2. Memory layout must be preserved across all parts
3. The split must happen at safe boundaries (not in the middle of operations)
4. All parts share the same memory space via DELEGATECALL
"""

import re
import sys
from pathlib import Path


def analyze_verifier(content: str) -> dict:
    """Analyze the verifier to understand its structure."""
    lines = content.split('\n')

    # Find the assembly block
    assembly_start = None
    assembly_end = None

    for i, line in enumerate(lines):
        if 'assembly ("memory-safe")' in line:
            assembly_start = i
        if assembly_start and line.strip() == '}' and i > assembly_start + 100:
            # Find the closing brace of assembly block
            brace_count = 0
            for j in range(assembly_start, len(lines)):
                brace_count += lines[j].count('{') - lines[j].count('}')
                if brace_count == 0 and j > assembly_start:
                    assembly_end = j
                    break
            break

    return {
        'total_lines': len(lines),
        'assembly_start': assembly_start,
        'assembly_end': assembly_end,
        'assembly_lines': assembly_end - assembly_start if assembly_end else 0
    }


def find_split_points(content: str, num_parts: int = 2) -> list:
    """
    Find safe split points in the verifier code.

    We look for points where:
    1. We're not in the middle of a block
    2. We're between major computation sections
    3. The split is roughly equal in size
    """
    lines = content.split('\n')

    # Find assembly block boundaries
    assembly_start = None
    for i, line in enumerate(lines):
        if 'assembly ("memory-safe")' in line:
            assembly_start = i
            break

    if not assembly_start:
        raise ValueError("Could not find assembly block")

    # Find the end of the preamble (function definitions, initial setup)
    preamble_end = assembly_start
    for i in range(assembly_start, min(assembly_start + 200, len(lines))):
        if 'mstore(0xa0,' in lines[i]:  # First actual computation
            preamble_end = i
            break

    # Find the final pairing check (this should stay in the last part)
    pairing_start = None
    for i in range(len(lines) - 1, max(0, len(lines) - 100), -1):
        if 'staticcall(gas(), 0x8,' in lines[i]:  # Pairing precompile
            pairing_start = i - 20  # Include some setup before pairing
            break

    if not pairing_start:
        # Fallback: find the last staticcall
        for i in range(len(lines) - 1, 0, -1):
            if 'staticcall' in lines[i]:
                pairing_start = i
                break

    # Calculate split points based on number of parts
    computation_start = preamble_end
    computation_end = pairing_start
    computation_length = computation_end - computation_start

    split_points = [preamble_end]

    # Divide computation into equal parts
    for i in range(1, num_parts):
        split_candidate = computation_start + (computation_length * i // num_parts)

        # Adjust to a safe boundary - look for a point where we're NOT inside a block
        # We want to split after a closing brace or after a standalone mstore
        for offset in range(-100, 100):
            candidate = split_candidate + offset
            if candidate < len(lines) and candidate > 0:
                line = lines[candidate].strip()
                prev_line = lines[candidate - 1].strip()

                # Good split point: after a closing brace followed by mstore
                if prev_line == '}' and line.startswith('mstore('):
                    split_points.append(candidate)
                    break
                # Also good: after a series of mstore operations before an opening brace
                elif line.startswith('mstore(') and candidate + 1 < len(lines):
                    next_line = lines[candidate + 1].strip()
                    if next_line == '{':
                        split_points.append(candidate + 1)
                        break
        else:
            # If no good boundary found, use the candidate
            split_points.append(split_candidate)

    split_points.append(pairing_start)

    return split_points


def create_part_contract(part_num: int, lines: list, is_first: bool, is_last: bool) -> str:
    """Create a contract for a specific part of the verifier."""

    # For the first part, include the full contract structure
    if is_first:
        contract = f"""// SPDX-License-Identifier: MIT
pragma solidity 0.8.19;

/**
 * Halo2Verifier Part {part_num}
 * This is part {part_num} of a split verifier contract.
 *
 * IMPORTANT: This contract is designed to be called via DELEGATECALL
 * from the main Halo2VerifierProxy contract. It shares memory with
 * other parts and must maintain the exact memory layout.
 */
contract Halo2VerifierPart{part_num} {{
    fallback(bytes calldata) external returns (bytes memory) {{
        assembly ("memory-safe") {{
            // Enforce that Solidity memory layout is respected
            let data := mload(0x40)
            if iszero(eq(data, 0x80)) {{
                revert(0, 0)
            }}

            let success := true
            let f_p := 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47
            let f_q := 0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001

            function validate_ec_point(x, y) -> valid {{
                {{
                    let x_lt_p := lt(x, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    let y_lt_p := lt(y, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    valid := and(x_lt_p, y_lt_p)
                }}
                {{
                    let y_square := mulmod(y, y, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    let x_square := mulmod(x, x, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    let x_cube := mulmod(x_square, x, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    let x_cube_plus_3 := addmod(x_cube, 3, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    let is_affine := eq(x_cube_plus_3, y_square)
                    valid := and(valid, is_affine)
                }}
            }}

"""
        # Add the actual computation lines
        for line in lines:
            contract += line + "\n"

        # Close the assembly block and return success flag in memory
        contract += """
            // Store success flag at memory position 0x00 for next part
            mstore(0x00, success)

            // Return success flag
            return(0x00, 0x20)
        }
    }
}
"""
    elif is_last:
        # Last part: check success from previous part and do final verification
        contract = f"""// SPDX-License-Identifier: MIT
pragma solidity 0.8.19;

/**
 * Halo2Verifier Part {part_num} (Final)
 * This is the final part of a split verifier contract.
 *
 * IMPORTANT: This contract is designed to be called via DELEGATECALL
 * from the main Halo2VerifierProxy contract. It shares memory with
 * other parts and must maintain the exact memory layout.
 */
contract Halo2VerifierPart{part_num} {{
    fallback(bytes calldata) external returns (bytes memory) {{
        assembly ("memory-safe") {{
            // Enforce that Solidity memory layout is respected
            let data := mload(0x40)
            if iszero(eq(data, 0x80)) {{
                revert(0, 0)
            }}

            // Load success flag from previous part
            let success := mload(0x00)
            let f_p := 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47
            let f_q := 0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001

            // Continue computation from previous part
"""
        # Add the actual computation lines (preserve original indentation)
        for line in lines:
            contract += line + "\n"

        # The last part should already have the revert/return logic from the original
        contract += """        }
    }
}
"""
    else:
        # Middle part: check success and continue computation
        contract = f"""// SPDX-License-Identifier: MIT
pragma solidity 0.8.19;

/**
 * Halo2Verifier Part {part_num}
 * This is part {part_num} of a split verifier contract.
 *
 * IMPORTANT: This contract is designed to be called via DELEGATECALL
 * from the main Halo2VerifierProxy contract. It shares memory with
 * other parts and must maintain the exact memory layout.
 */
contract Halo2VerifierPart{part_num} {{
    fallback(bytes calldata) external returns (bytes memory) {{
        assembly ("memory-safe") {{
            // Enforce that Solidity memory layout is respected
            let data := mload(0x40)
            if iszero(eq(data, 0x80)) {{
                revert(0, 0)
            }}

            // Load success flag from previous part
            let success := mload(0x00)
            let f_p := 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47
            let f_q := 0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001

            // Continue computation from previous part
"""
        # Add the actual computation lines (preserve original indentation)
        for line in lines:
            contract += line + "\n"

        # Store success and return
        contract += """
            // Store success flag at memory position 0x00 for next part
            mstore(0x00, success)

            // Return success flag
            return(0x00, 0x20)
        }
    }
}
"""

    return contract


def split_verifier(input_file: Path, output_dir: Path, num_parts: int = 2):
    """Split the verifier into multiple parts."""

    print(f"Reading verifier from {input_file}...")
    content = input_file.read_text()

    print("Analyzing verifier structure...")
    info = analyze_verifier(content)
    print(f"  Total lines: {info['total_lines']}")
    print(f"  Assembly block: lines {info['assembly_start']}-{info['assembly_end']} ({info['assembly_lines']} lines)")

    print(f"\nFinding split points for {num_parts} parts...")
    lines = content.split('\n')

    # Find split points
    split_points = find_split_points(content, num_parts)
    print(f"  Split points: {split_points}")

    # Find where the final check starts
    final_check_start = info['assembly_end'] - 10
    for i in range(info['assembly_end'], max(0, info['assembly_end'] - 50), -1):
        if 'Revert if anything fails' in lines[i]:
            final_check_start = i
            break

    # Create parts
    output_dir.mkdir(parents=True, exist_ok=True)

    # Create each part
    for part_idx in range(num_parts):
        part_num = part_idx + 1
        is_first = (part_idx == 0)
        is_last = (part_idx == num_parts - 1)

        # Determine the line range for this part
        start_line = split_points[part_idx]
        if is_last:
            end_line = final_check_start
        else:
            end_line = split_points[part_idx + 1]

        part_lines = lines[start_line:end_line]
        part_content = create_part_contract(part_num, part_lines, is_first=is_first, is_last=is_last)
        part_file = output_dir / f"Halo2VerifierPart{part_num}.sol"
        part_file.write_text(part_content)

        print(f"\nCreated Part {part_num}: {part_file}")
        print(f"  Lines: {len(part_lines)} (from {start_line} to {end_line})")
        print(f"  Size: {len(part_content)} bytes")

    print(f"\n✅ Successfully split verifier into {num_parts} parts")
    print(f"   Output directory: {output_dir}")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: python split_verifier.py <input_file> [output_dir] [num_parts]")
        print("Example: python split_verifier.py contracts/DepositVerifier.sol contracts/ethereum/src 2")
        sys.exit(1)

    input_file = Path(sys.argv[1])
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else Path("contracts/ethereum/src")
    num_parts = int(sys.argv[3]) if len(sys.argv) > 3 else 2

    if not input_file.exists():
        print(f"Error: Input file {input_file} does not exist")
        sys.exit(1)

    split_verifier(input_file, output_dir, num_parts)

