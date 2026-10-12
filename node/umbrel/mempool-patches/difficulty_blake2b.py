#!/usr/bin/env python3
"""Apply difficulty-blake2b.patch to a mempool backend's compiled JS.

Usage: difficulty_blake2b.py <backend package dir> [--check]

Knots 29.4.2 renamed the RPC field `difficulty` to `difficulty_blake2b` (= difficulty * 2^32)
and removed `getdifficulty`. A stock backend then reads `undefined`, every block insert fails
with "Column 'difficulty' cannot be null", and the hashrate endpoint reports difficulty 0.

Two files change, the same two lines the hub's installed backend has carried since 2026-09-21:

  api/bitcoin/bitcoin-api.js    convertBlock falls back to difficulty_blake2b / 2^32
  api/mining/mining-routes.js   current difficulty falls back to getblockchaininfo

Each file is replaced through a temp file and a rename, after a `.bak-<UTC>-difficulty` copy of
the original. A file that already carries the change is left alone, so a second run changes
nothing. `--check` writes nothing and exits 3 if a file still needs the change. A missing
anchor exits 1 before anything is written: the backend is not the build this was made for.

patch-backend.py imports `patch_text` to make the same change to the Umbrel image's files.
"""

from __future__ import annotations

import os
import sys
import time
from pathlib import Path
from typing import NamedTuple


class Edit(NamedTuple):
    """One replacement in one compiled file: `old` must occur exactly once."""

    path: str
    old: str
    new: str


EDITS = (
    Edit(
        "api/bitcoin/bitcoin-api.js",
        "            difficulty: block.difficulty,\n",
        "            difficulty: block.difficulty ?? (block.difficulty_blake2b != null"
        " ? block.difficulty_blake2b / 4294967296 : undefined),\n",
    ),
    Edit(
        "api/mining/mining-routes.js",
        "            currentDifficulty = await bitcoin_client_1.default.getDifficulty();\n",
        "            // `getdifficulty` is gone in Knots 29.4.2; getblockchaininfo carries"
        " difficulty_blake2b (= difficulty * 2^32).\n"
        "            try { currentDifficulty = await bitcoin_client_1.default.getDifficulty(); }\n"
        "            catch (e) { const bi = await bitcoin_client_1.default.getBlockchainInfo();"
        " currentDifficulty = bi.difficulty ?? (bi.difficulty_blake2b / 4294967296); }\n",
    ),
)
FILES = tuple(e.path for e in EDITS)


class AnchorError(Exception):
    """The file is neither the stock build nor already patched."""


def patch_text(path: str, text: str) -> str:
    """Return `text` with the edit for `path` applied; unchanged if it is already there."""
    edit = next(e for e in EDITS if e.path == path)
    if edit.new in text:
        return text
    found = text.count(edit.old)
    if found != 1:
        raise AnchorError(f"{path}: expected 1 anchor, found {found}")
    return text.replace(edit.old, edit.new, 1)


def replace_file(target: Path, text: str, stamp: str) -> None:
    """Back `target` up, then replace it with `text` by a rename in the same directory."""
    backup = target.with_name(f"{target.name}.bak-{stamp}-difficulty")
    backup.write_bytes(target.read_bytes())
    tmp = target.with_name(f".{target.name}.tmp-{os.getpid()}")
    try:
        tmp.write_text(text)
        tmp.chmod(target.stat().st_mode & 0o777)
        tmp.replace(target)
    finally:
        tmp.unlink(missing_ok=True)


def main(argv: list[str]) -> int:
    args = [a for a in argv[1:] if a != "--check"]
    check = len(args) != len(argv) - 1
    if len(args) != 1:
        print(
            "usage: difficulty_blake2b.py <backend package dir> [--check]",
            file=sys.stderr,
        )
        return 2
    package = Path(args[0])
    try:
        pending = []
        for path in FILES:
            target = package / path
            old = target.read_text()
            new = patch_text(path, old)
            if new != old:
                pending.append((target, new))
    except (OSError, AnchorError) as e:
        print(f"difficulty-blake2b: {e}; nothing written", file=sys.stderr)
        return 1
    if not pending:
        print(f"difficulty-blake2b: {package} already patched, nothing to do")
        return 0
    if check:
        print(f"difficulty-blake2b: {package} needs the patch ({len(pending)} file(s))")
        return 3
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    for target, new in pending:
        replace_file(target, new, stamp)
        print(f"difficulty-blake2b: patched {target} (backup .bak-{stamp}-difficulty)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
