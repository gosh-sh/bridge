#!/usr/bin/env bash
# Deposit audit gate bundle — TD-68, TD-42, TD-43, TD-49, TD-53, TD-65 (RELAYER), TD-04.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

chmod +x scripts/check_pi_count_docs.sh
chmod +x scripts/check_vk_srs_pin.sh
chmod +x scripts/check_mock_vs_shplonk_smoke.sh
chmod +x scripts/check_mutation_kill_smoke.sh
chmod +x scripts/td_53_dry_run_recovery_smoke.sh
chmod +x scripts/check_td65_g3_smoke.sh
chmod +x scripts/check_an_overlay_patch_matrix.sh

./scripts/check_pi_count_docs.sh
./scripts/check_vk_srs_pin.sh
./scripts/check_mock_vs_shplonk_smoke.sh
./scripts/check_mutation_kill_smoke.sh
./scripts/td_53_dry_run_recovery_smoke.sh
./scripts/check_td65_g3_smoke.sh
./scripts/check_an_overlay_patch_matrix.sh

echo "OK — deposit audit gates (TD-68, TD-42, TD-43, TD-49, TD-53 E2E, TD-65 G3 mock, TD-04 overlay)"
