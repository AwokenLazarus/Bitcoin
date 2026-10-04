#!/usr/bin/env python3
"""Add pool blocks the site's scanner skipped to `found_blocks`.

    python3 pool/tools/backfill_found_blocks.py            # dry run: lists what it would add
    python3 pool/tools/backfill_found_blocks.py --apply    # adds them

Run the deployed copy, as the user the pool site runs as: it imports the server.py beside it,
so it reads the same config.json, database, node and primed block log the site does.

What counts as missing: a block in primed's block log that is not an orphan there, is the
main-chain block at its height on the node, carries the pool's coinbase tag, and has no
`found_blocks` row. That is the scanner's own rule. A tagged block primed never logged is not
added on the tag alone, since anyone can write the tag; name its height with `--also` once
its coinbase has been checked by hand.

Each block gets its `found_blocks` row, and a split block also gets a closed round holding
what its coinbase paid. The open round and its work are never touched (`record_round_only` in
server.py). Heights the scanner has not reached, or is still holding, are left to it.

A second run finds nothing to add. Without --apply nothing is written: the site's own
read-only switch (POOL_UI_NO_WRITE) is set before server.py is imported.
"""

import argparse
import os
import sys
from pathlib import Path
from typing import NamedTuple


class Block(NamedTuple):
    height: int
    hash: str
    source: str  # "prime-log" or "operator" (--also)
    kind: str  # primed's kind for the block, "" when it has none
    outputs: int  # coinbase outputs with value
    reward_sats: int
    blk: dict


class Skipped(NamedTuple):
    height: int
    hash: str
    why: str


class Incomplete(Exception):
    """A source could not be read, so the list of missing blocks would be a short one."""


def _coinbase(server, blk):
    tx0 = (blk.get("tx") or [None])[0] or {}
    text = server.ascii_from_hex(((tx0.get("vin") or [{}])[0]).get("coinbase") or "")
    return text, tx0.get("vout") or []


def _candidate(server, height, blockhash, source, kind):
    """A Block to add, or a Skipped saying why not. `blockhash` None: take the chain's."""
    onchain = server.rpc("getblockhash", [height])
    if not onchain:
        raise Incomplete(f"the node did not answer getblockhash {height}")
    if blockhash and onchain != blockhash:
        return Skipped(
            height, blockhash, f"not in the main chain (the chain has {onchain})"
        )
    blk = server.rpc("getblock", [onchain, 2])
    if not blk:
        raise Incomplete(f"the node did not answer getblock {onchain}")
    text, vouts = _coinbase(server, blk)
    if server.SOLO_TAG in text:
        return Skipped(height, onchain, "a solo block (those live in solo_blocks)")
    if server.COINBASE_TAG not in text:
        return Skipped(
            height, onchain, f"coinbase does not carry the {server.COINBASE_TAG!r} tag"
        )
    reward = sum(round(float(v.get("value") or 0) * 1e8) for v in vouts)
    return Block(
        height, onchain, source, kind, server.value_output_count(vouts), reward, blk
    )


def plan(server, also=()):
    """(blocks to add, skipped): read-only. Raises Incomplete rather than return a short list."""
    sig, latest = server._block_log_latest()
    if sig is None:
        raise Incomplete(f"primed's block log is not readable: {server.BLOCKS_LOG}")
    tip = server.rpc("getblockcount")
    if not tip:
        raise Incomplete("the node did not answer getblockcount")
    row = server.db("SELECT value FROM meta WHERE key='scan_height'", one=True)
    if not row or not row["value"]:
        raise Incomplete(
            "no scan_height in the database: is this the pool site's database?"
        )
    scanned = int(row["value"])
    have = {
        int(r["height"]): r["hash"]
        for r in server.db("SELECT height, hash FROM found_blocks") or []
    }
    held = {
        int(r["height"]) for r in server.db("SELECT height FROM scan_pending") or []
    }

    wanted = {}  # height -> (hash or None, source, kind); the log wins over --also
    for height in also:
        wanted[int(height)] = (None, "operator", "")
    for rec in latest.values():
        kind = str(rec.get("kind") or "")
        height, blockhash = int(rec.get("height") or 0), str(rec.get("hash") or "")
        if not height or not blockhash or kind.startswith("orphan"):
            continue
        wanted[height] = (blockhash, "prime-log", kind)

    blocks, skipped = [], []
    for height in sorted(wanted):
        blockhash, source, kind = wanted[height]
        if height in have:
            if blockhash and have[height] != blockhash:
                skipped.append(
                    Skipped(
                        height,
                        blockhash,
                        f"found_blocks has {have[height]} at this height; left alone",
                    )
                )
            continue
        if height > scanned or height in held:
            skipped.append(
                Skipped(
                    height,
                    blockhash or "",
                    "the scanner has not settled this height yet",
                )
            )
            continue
        got = _candidate(server, height, blockhash, source, kind)
        (blocks if isinstance(got, Block) else skipped).append(got)
    return blocks, skipped


def apply(server, blocks):
    """Write the rows for `blocks`. Returns how many `found_blocks` rows were added."""
    added = 0
    for b in blocks:
        text, vouts = _coinbase(server, b.blk)
        existed, reward, fee_btc, miner_btc = server.insert_found_block(
            b.height, b.hash, b.blk, vouts, text
        )
        added += not existed
        if b.outputs >= 2:
            server.record_round_only(
                b.height, b.hash, b.blk.get("time"), reward, fee_btc, miner_btc, vouts
            )
    return added


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    ap.add_argument(
        "--apply", action="store_true", help="write the rows (default: dry run)"
    )
    ap.add_argument(
        "--also",
        type=int,
        action="append",
        default=[],
        metavar="HEIGHT",
        help="also add the tagged main-chain block at HEIGHT though primed's log lacks it (repeatable)",
    )
    args = ap.parse_args(argv)
    if args.apply:
        if os.environ.get("POOL_UI_NO_WRITE") == "1":
            sys.exit("POOL_UI_NO_WRITE=1 is set: --apply would write nothing")
    else:
        os.environ["POOL_UI_NO_WRITE"] = "1"
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
    import server

    try:
        blocks, skipped = plan(server, args.also)
    except Incomplete as e:
        sys.exit(f"stopped, nothing written: {e}")
    print(f"database {server.DB}")
    print(f"prime log {server.BLOCKS_LOG}")
    for s in skipped:
        print(f"skip  {s.height} {s.hash} {s.why}")
    verb = "add " if args.apply else "would add"
    for b in blocks:
        print(
            f"{verb} {b.height} {b.hash} {b.source} kind={b.kind or '-'} outputs={b.outputs} reward_sats={b.reward_sats}"
        )
    if not args.apply:
        print(
            f"dry run: {len(blocks)} block(s) to add, nothing written. Re-run with --apply."
        )
        return 0
    added = apply(server, blocks)
    print(f"added {added} found_blocks row(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
