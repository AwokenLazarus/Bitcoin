"""difficulty_blake2b.py against the two anchors as mempool v3.3.1 compiles them.

python3 -m pytest node/umbrel/mempool-patches/test_difficulty_blake2b.py
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

import difficulty_blake2b as patch

SCRIPT = Path(patch.__file__)
API = "api/bitcoin/bitcoin-api.js"
ROUTES = "api/mining/mining-routes.js"
STOCK = {
    API: "    static convertBlock(block) {\n        return {\n            nonce: block.nonce,\n"
    "            difficulty: block.difficulty,\n            merkle_root: block.merkleroot,\n        };\n    }\n",
    ROUTES: "        try {\n            currentHashrate = await bitcoin_client_1.default.getNetworkHashPs(1008);\n"
    "            currentDifficulty = await bitcoin_client_1.default.getDifficulty();\n        }\n",
}


def package(tmp_path: Path, files: dict[str, str]) -> Path:
    for name, text in files.items():
        target = tmp_path / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)
    return tmp_path


def run(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args],
        capture_output=True,
        text=True,
        check=False,
    )


@pytest.mark.parametrize("name", [API, ROUTES])
def test_patch_reads_the_new_field_and_keeps_the_old(name: str) -> None:
    out = patch.patch_text(name, STOCK[name])
    assert "difficulty_blake2b / 4294967296" in out
    # the old field still wins when the node sends it
    assert "block.difficulty ??" in out or "bi.difficulty ??" in out


@pytest.mark.parametrize("name", [API, ROUTES])
def test_patching_twice_is_the_same_as_once(name: str) -> None:
    once = patch.patch_text(name, STOCK[name])
    assert patch.patch_text(name, once) == once


@pytest.mark.parametrize("text", ["", "difficulty: block.difficulty,\n" * 2])
def test_a_file_that_is_not_the_stock_build_is_refused(text: str) -> None:
    text = text.replace("difficulty:", "            difficulty:")
    with pytest.raises(patch.AnchorError):
        patch.patch_text(API, text)


def test_cli_patches_backs_up_and_then_does_nothing(tmp_path: Path) -> None:
    pkg = package(tmp_path, STOCK)
    assert run(str(pkg), "--check").returncode == 3
    assert (pkg / API).read_text() == STOCK[API], "--check must not write"

    assert run(str(pkg)).returncode == 0
    backups = sorted(p.name for p in pkg.rglob("*.bak-*-difficulty"))
    assert [b.split(".bak-")[0] for b in backups] == [
        "bitcoin-api.js",
        "mining-routes.js",
    ]
    assert next(pkg.rglob("bitcoin-api.js.bak-*")).read_text() == STOCK[API]

    before = {p: p.read_bytes() for p in pkg.rglob("*") if p.is_file()}
    second = run(str(pkg))
    assert second.returncode == 0 and "nothing to do" in second.stdout
    assert {p: p.read_bytes() for p in pkg.rglob("*") if p.is_file()} == before
    assert run(str(pkg), "--check").returncode == 0


def test_cli_writes_nothing_when_one_file_is_unknown(tmp_path: Path) -> None:
    pkg = package(tmp_path, {API: STOCK[API], ROUTES: "something else\n"})
    result = run(str(pkg))
    assert result.returncode == 1 and "nothing written" in result.stderr
    assert (pkg / API).read_text() == STOCK[API]
    assert not list(pkg.rglob("*.bak-*"))
