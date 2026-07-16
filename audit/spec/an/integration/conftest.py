"""
Conftest для слоя integration/ — multi-contract MessagePipeline-сценарии.

Содержит снап-шот-паттерн (адаптация `snapshot_root()` / `assert_root_unchanged()`
из `accumulator/tests/e2e/common.sh`) для negative-тестов:

    >>> def test_X(tb):
    ...     # ... подготовка контракта ...
    ...     snap = state_snapshot(tb, root_pn_tvc, "RootPN")
    ...     # ... попытка некорректной операции (должна упасть) ...
    ...     assert_state_unchanged(tb, root_pn_tvc, "RootPN", snap,
    ...                            ignore_fields=["__lastTimestamp"])

Дополнительно — `state_field` для удобного чтения одного поля без полного snapshot'а.

Принцип red→green: snapshot-helper — это инструмент для написания именно
правильных (red→green) тестов, а не characterization. Используется для проверки,
что НЕ должно произойти при некорректном вводе (state остаётся неизменным).
"""

from __future__ import annotations

import base64
import json
import subprocess
from copy import deepcopy
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional

import pytest


# --- StateSnapshot: lightweight wrapper над декодированным persistent state ---


@dataclass
class StateSnapshot:
    """Снимок persistent state контракта.

    Содержит:
    - `contract`: имя контракта (как в build/<name>.abi.json);
    - `tvc_path`: путь к TVC, из которого прочитан snapshot;
    - `data`: dict — декодированные поля (key → str/int/dict/list);
    - `code_hash`: hash кода (для контроля, что код не подменён между snapshot и assert).
    """

    contract: str
    tvc_path: Path
    data: Dict[str, Any]
    code_hash: Optional[str] = None
    _meta: Dict[str, Any] = field(default_factory=dict)

    def get(self, name: str, default: Any = None) -> Any:
        return self.data.get(name, default)

    def keys(self) -> List[str]:
        return list(self.data.keys())


# --- Внутренние утилиты (state read через tvm-debugger) ---


def _decode_state(tb, tvc_path: Path, contract: str) -> Dict[str, Any]:
    """state-decode + boc-decode → dict; не меняет TVC.

    Делает то же самое, что первая половина `tb.patch_state`, но read-only.
    """
    tvc_path = Path(tvc_path)
    abi_path = tb.build_dir / f"{contract}.abi.json"
    if not abi_path.is_file():
        raise FileNotFoundError(f"ABI не найден: {abi_path} "
                                f"(возможно, контракт {contract!r} не скомпилирован)")
    with open(abi_path) as f:
        abi = json.load(f)
    fields_data = abi["fields"]

    state_proc = subprocess.run(
        [str(tb.tvm_debugger), "state-decode", "-s", str(tvc_path)],
        capture_output=True, text=True, check=True,
    )
    state = json.loads(state_proc.stdout)
    code_hash = None
    if "code" in state:
        sha = subprocess.run(
            ["python3", "-c",
             "import hashlib, base64, sys; "
             "print(hashlib.sha256(base64.b64decode(sys.argv[1])).hexdigest())",
             state["code"]],
            capture_output=True, text=True, check=True,
        )
        code_hash = sha.stdout.strip()

    import os as _os
    fields_file = tb.build_dir / f"{contract}.fields_{_os.getpid()}.json"
    with open(fields_file, "w") as f:
        json.dump(fields_data, f)

    decoded = json.loads(subprocess.run(
        [str(tb.tvm_debugger), "boc-decode", "-b", state["data"], "-p", str(fields_file)],
        capture_output=True, text=True, check=True,
    ).stdout)["data"]

    return {"data": decoded, "code_hash": code_hash}


# --- Public API (фикстуры) ---


@pytest.fixture
def state_snapshot(tb):
    """Фикстура-фабрика: возвращает функцию для снятия snapshot'а state.

    Использование:

        def test_negative(tb, state_snapshot, assert_state_unchanged):
            tvc = tb.build_dir / "PrivateNote.tvc"
            snap = state_snapshot(tvc, "PrivateNote")
            # ... попытка плохой операции ...
            assert_state_unchanged(tvc, "PrivateNote", snap)
    """

    def _snap(tvc_path: Path | str, contract: str) -> StateSnapshot:
        info = _decode_state(tb, Path(tvc_path), contract)
        return StateSnapshot(
            contract=contract,
            tvc_path=Path(tvc_path),
            data=deepcopy(info["data"]),
            code_hash=info["code_hash"],
        )

    return _snap


# Поля, которые автоматически игнорируются при сравнении (служебные, меняются
# при любом вызове). Расширяемый список.
DEFAULT_IGNORE_FIELDS: tuple[str, ...] = (
    "__lastTimestamp",
    "_pubkey",
    "_constructorFlag",
    "_replayProtFlag",
)


@pytest.fixture
def assert_state_unchanged(tb):
    """Фикстура-фабрика: возвращает функцию-ассерт для сравнения state с snapshot'ом.

    Параметры:
        tvc_path, contract: что сейчас читать с диска.
        snapshot: ранее снятый StateSnapshot.
        ignore_fields: дополнительные поля для игнорирования (помимо DEFAULT_IGNORE_FIELDS).
        check_code: если True (по умолчанию), сверяет также code_hash —
                    защищает от случайного rebuild между snap и assert.
        msg: префикс для AssertionError, помогает в навигации.

    Бросает AssertionError со списком отличий.
    """

    def _assert(tvc_path: Path | str,
                contract: str,
                snapshot: StateSnapshot,
                ignore_fields: Iterable[str] = (),
                check_code: bool = True,
                msg: str = "") -> None:
        info = _decode_state(tb, Path(tvc_path), contract)
        current = info["data"]
        prev = snapshot.data

        if check_code and snapshot.code_hash and info["code_hash"] != snapshot.code_hash:
            raise AssertionError(
                f"{msg + ': ' if msg else ''}code_hash изменился между snapshot и assert "
                f"(ожидался {snapshot.code_hash[:12]}..., получен {info['code_hash'][:12]}...). "
                f"Возможно, был rebuild контрактов в середине теста."
            )

        ignored = set(DEFAULT_IGNORE_FIELDS) | set(ignore_fields)
        diffs: List[str] = []

        all_keys = set(prev.keys()) | set(current.keys())
        for k in sorted(all_keys):
            if k in ignored:
                continue
            p = prev.get(k, "<MISSING>")
            c = current.get(k, "<MISSING>")
            if p != c:
                diffs.append(f"  {k}: {p!r}  →  {c!r}")

        if diffs:
            head = (f"{msg + ': ' if msg else ''}"
                    f"state {contract!r} изменился ({len(diffs)} field(s)):\n")
            raise AssertionError(head + "\n".join(diffs))

    return _assert


@pytest.fixture
def state_field(tb):
    """Прочитать одно поле state'а контракта (без snapshot'а).

    Удобно для быстрых проверок:
        assert state_field(tvc, "PrivateNote", "_hasWithdrawn") == "false"
    """

    def _read(tvc_path: Path | str, contract: str, field_name: str) -> Any:
        info = _decode_state(tb, Path(tvc_path), contract)
        if field_name not in info["data"]:
            raise KeyError(
                f"Поле {field_name!r} не найдено в state {contract!r}. "
                f"Доступные: {list(info['data'].keys())}"
            )
        return info["data"][field_name]

    return _read
