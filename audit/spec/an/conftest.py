"""Shared pytest config for bridge AN TVM spec tests."""

from __future__ import annotations

import os
import sys
from pathlib import Path
from typing import Iterator

import pytest

_TESTS_ROOT = Path(__file__).parent.resolve()
if str(_TESTS_ROOT) not in sys.path:
    sys.path.insert(0, str(_TESTS_ROOT))

_AUDIT_ROOT = _TESTS_ROOT.parent.parent  # audit/
_CONTRACTS_ROOT = _AUDIT_ROOT / "spec" / "an-contracts"

os.environ.setdefault("AN_PROJECT_ROOT", str(_CONTRACTS_ROOT))

from test_base import TestBase  # noqa: E402


@pytest.fixture(scope="session")
def tb() -> Iterator[TestBase]:
    """One TestBase per session — compile-once against an-contracts/."""
    instance = TestBase(project_root=_CONTRACTS_ROOT, tests_dir=_TESTS_ROOT)
    ok = instance.compile_all()
    if not ok:
        pytest.exit("compile_all() failed; see output above", returncode=2)
    yield instance


@pytest.fixture
def fresh_tb() -> Iterator[TestBase]:
    yield TestBase(project_root=_CONTRACTS_ROOT, tests_dir=_TESTS_ROOT)
