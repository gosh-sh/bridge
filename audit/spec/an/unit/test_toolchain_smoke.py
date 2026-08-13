"""Smoke: sold + tvm-debugger reachable via .tools/ symlinks."""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

pytestmark = pytest.mark.unit

_AUDIT_ROOT = Path(__file__).resolve().parents[3]  # audit/
_REPO_ROOT = _AUDIT_ROOT.parent
_TOOLS = _REPO_ROOT / ".tools"


def test_tools_symlinks_exist():
    for name in ("sold", "tvm-debugger", "tvm-cli"):
        path = _TOOLS / name
        assert path.is_symlink() or path.exists(), f"missing .tools/{name}"


def test_sold_version():
    sold = _TOOLS / "sold"
    if not sold.exists():
        pytest.skip(".tools/sold not built — run scripts/setup_an_audit_tools.sh")
    r = subprocess.run([str(sold), "--version"], capture_output=True, text=True, check=True)
    assert "sold" in r.stdout.lower() or r.stdout.strip()


def test_tvm_debugger_responds():
    dbg = _TOOLS / "tvm-debugger"
    if not dbg.exists():
        pytest.skip(".tools/tvm-debugger not built")
    r = subprocess.run([str(dbg), "--help"], capture_output=True, text=True)
    assert r.returncode == 0
    assert "run" in r.stdout or "run" in r.stderr
