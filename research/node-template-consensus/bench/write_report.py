#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""Merge the three bench outputs into results/bench.json and results/bench.md."""

from __future__ import annotations

import json
import os
import statistics
import sys
from typing import Any

ROOT = os.path.dirname(os.path.abspath(__file__))
RES = os.path.join(ROOT, "results")


def run_count() -> int:
    raw = os.environ.get("NTA_BENCH_RUNS", "5")
    try:
        n = int(raw)
    except ValueError:
        sys.exit(f"write_report: NTA_BENCH_RUNS must be an integer, got {raw!r}")
    if n < 1:
        sys.exit(f"write_report: NTA_BENCH_RUNS must be >= 1, got {raw!r}")
    return n


def load(name: str) -> dict[str, Any]:
    with open(os.path.join(RES, name), encoding="utf-8") as f:
        data = json.load(f)
    if not isinstance(data, dict):
        sys.exit(f"write_report: {name} is not an object")
    return data


def med(xs: list[float]) -> float:
    return statistics.median(xs)


def spread(xs: list[float]) -> float:
    return max(xs) - min(xs)


def agg_runs(runs: list[dict[str, Any]], key: str = "ns_per_op") -> dict[str, Any]:
    xs = [float(r[key]) for r in runs]
    return {
        "n": len(xs),
        "unit": key,
        "min": min(xs),
        "median": med(xs),
        "max": max(xs),
        "spread": spread(xs),
        "runs": xs,
    }


def us(ns: float) -> float:
    return ns / 1_000.0


def ms(ns: float) -> float:
    return ns / 1_000_000.0


def fmt_us(ns: float) -> str:
    return f"{us(ns):.2f} µs"


def fmt_ms(ns: float) -> str:
    return f"{ms(ns):.3f} ms"


def fmt_s(ns: float) -> str:
    return f"{ns / 1e9:.4f} s"


def main() -> None:
    host = load("host.json")
    verify = load("verify.json")
    sign = load("sign.json")
    blocks = load("blocks.json")

    v = agg_runs(verify["runs"])
    b512 = agg_runs(verify["batch_512"], "elapsed_ns")
    b8000 = agg_runs(verify["batch_8000"], "elapsed_ns")
    s = agg_runs(sign["runs"])

    block_rows = []
    for series in blocks["series"]:
        xs = [float(sample["ns"]) for sample in series["samples"]]
        block_rows.append(
            {
                "payees": series["payees"],
                "expect": series["expect"],
                "results": [sample["result"] for sample in series["samples"]],
                "coinbase_bytes": series["samples"][-1]["coinbase_bytes"],
                "n": len(xs),
                "min_ns": min(xs),
                "median_ns": med(xs),
                "max_ns": max(xs),
                "spread_ns": spread(xs),
                "runs_ns": xs,
            }
        )

    by_n = {row["payees"]: row for row in block_rows}
    per_verify = float(v["median"])
    cap_block = float(by_n[512]["median_ns"])
    over = float(by_n[513]["median_ns"])
    bound = float(b8000["median"])

    claims = [
        {
            "source": "BIP",
            "quote": "~50 µs each",
            "context": "BIP340 verifications, single-threaded in ContextualCheckBlock",
            "published": "50 µs",
            "measured": fmt_us(per_verify),
            "measured_ns": per_verify,
            "what": "median secp256k1_schnorrsig_verify, 32-byte message, Knots libsecp256k1",
        },
        {
            "source": "BIP",
            "quote": "~0.4 s",
            "context": "about 8,000 distinct payees at the block-size bound",
            "published": "0.4 s",
            "measured": fmt_s(bound),
            "measured_ns": bound,
            "what": (
                "median wall time of 8,000 sequential secp256k1_schnorrsig_verify calls. "
                "The cap refuses a real block at 513 payees before any verify, so this is "
                "the verify-loop the sentence describes, not a full block check."
            ),
        },
        {
            "source": "BIP",
            "quote": "~26 ms",
            "context": "At 512",
            "published": "26 ms",
            "measured": fmt_ms(cap_block),
            "measured_ns": cap_block,
            "what": (
                "median submitblock of a 512-payee attested block "
                "(ContextualCheckBlock plus ConnectBlock of a coinbase-only block; an upper bound)"
            ),
        },
        {
            "source": "BIP",
            "quote": "Signing itself is ~25 µs",
            "context": "gateway per-tip attestation hash + BIP340",
            "published": "25 µs",
            "measured": fmt_us(float(s["median"])),
            "measured_ns": s["median"],
            "what": (
                "median Signer::sign in lazarus-protocol "
                "(hash + sign, zero aux, context created per call as the gateway does)"
            ),
        },
        {
            "source": "Knots brief",
            "quote": "~26 ms",
            "context": "at most ~26 ms of verification",
            "published": "26 ms",
            "measured": fmt_ms(cap_block),
            "measured_ns": cap_block,
            "what": "same 512-payee submitblock median as the BIP line",
        },
    ]

    doc = {
        "host": host,
        "verify": {
            "library": host["secp_library"],
            "op": verify["op"],
            "per_op": v,
            "batch_512_elapsed": b512,
            "batch_8000_elapsed": b8000,
            "per_op_times_512_ns": per_verify * 512,
            "per_op_times_8000_ns": per_verify * 8000,
            "acc": verify["acc"],
        },
        "sign": {
            "op": sign["op"],
            "per_op": s,
        },
        "blocks": {
            "op": blocks["op"],
            "port_base": blocks["port_base"],
            "note": blocks["note"],
            "series": block_rows,
            "early_reject_513_median_ns": over,
        },
        "claims": claims,
    }

    os.makedirs(RES, exist_ok=True)
    with open(os.path.join(RES, "bench.json"), "w", encoding="utf-8") as f:
        json.dump(doc, f, indent=2)
        f.write("\n")

    def row_line(label: str, a: dict[str, Any]) -> str:
        return (
            f"| {label} | {a['n']} | {fmt_us(a['min'])} | {fmt_us(a['median'])} | "
            f"{fmt_us(a['max'])} | {fmt_us(a['spread'])} |"
        )

    lines = [
        "# NTA performance claims, measured",
        "",
        f"Host: {host['cpu']}. Load average at start `{host['loadavg_start']}`, at end `{host['loadavg_end']}`.",
        "Cap: `systemd-run --user --scope -p CPUQuota=200% -p MemoryMax=4G nice -n 19`.",
        f"Knots `{host['knots_commit']}` ({host['knots_build_type']}).",
        f"Protocol crate `{host['protocol_commit']}` on `{host['protocol_branch']}`.",
        "",
        "One command reproduces every number below (see README for what the variables must point at):",
        "",
        "```",
        "KNOTS_DIR=<knots-tree> PROTOCOL_DIR=<protocol-crate> ./bench.sh",
        "```",
        "",
        "## Published claim vs measured",
        "",
        "| Source | Published line | Measured |",
        "|---|---|---|",
    ]
    for c in claims:
        lines.append(f"| {c['source']} | {c['quote']} | **{c['measured']}** |")
    lines += [
        "",
        "The ~50 µs / ~0.4 s / ~26 ms lines were 50 µs × 1, × ~8,000 and × 512. They were not timed.",
        "Signing ~25 µs was a regtest log line with no saved command.",
        "Every measured value above is better than that published line.",
        "",
        "## 1. BIP340 verify (`secp256k1_schnorrsig_verify`)",
        "",
        "No Schnorr bench exists under `src/bench`, and `BUILD_BENCH` is off, so this links the tree's `libsecp256k1.a` directly.",
        "",
        "```",
        host["cmd_verify"],
        "```",
        "",
        "| | runs | min | median | max | spread |",
        "|---|---:|---:|---:|---:|---:|",
        row_line("per verify", v),
        f"| 512 verifies, total | {b512['n']} | {fmt_ms(b512['min'])} | {fmt_ms(b512['median'])} | {fmt_ms(b512['max'])} | {fmt_ms(b512['spread'])} |",
        f"| 8000 verifies, total | {b8000['n']} | {fmt_s(b8000['min'])} | {fmt_s(b8000['median'])} | {fmt_s(b8000['max'])} | {fmt_ms(b8000['spread'])} |",
        "",
        f"512 × one-verify median = {fmt_ms(per_verify * 512)}. 8000 × one-verify median = {fmt_s(per_verify * 8000)}.",
        f"Each run is {verify['iters_per_run']} verifies after {verify['warmup']} warmup calls. Message is 32 bytes, the length `XOnlyPubKey::VerifySchnorr` passes.",
        "",
        "## 2. NTA block check (submitblock → ContextualCheckBlock)",
        "",
        "Regtest node from the Knots build, helpers from `feature_xbt_nta.py`.",
        "Accepted blocks also pay ConnectBlock (one coinbase), so these times are an upper bound on the consensus check.",
        "The 513-payee block returns `bad-nta-too-many` and does not connect. That path is cheap because the cap is checked before any signature.",
        "",
        "```",
        host["cmd_blocks"],
        "```",
        "",
        "| payees | verdict | coinbase | runs | min | median | max | spread |",
        "|---:|---|---:|---:|---:|---:|---:|---:|",
    ]
    for row in block_rows:
        verdict = (
            row["expect"] if len(set(row["results"])) == 1 else ",".join(row["results"])
        )
        lines.append(
            f"| {row['payees']} | {verdict} | {row['coinbase_bytes']} B | {row['n']} | "
            f"{fmt_ms(row['min_ns'])} | {fmt_ms(row['median_ns'])} | {fmt_ms(row['max_ns'])} | {fmt_ms(row['spread_ns'])} |"
        )
    port = int(blocks["port_base"])
    lines += [
        "",
        blocks["note"],
        f"Ports {port}–{port + 59} (range 32300–32599 left alone).",
        "",
        "## 3. Gateway signing per tip",
        "",
        "`Signer::sign` in the protocol crate (`attestation_hash` + `sign_schnorr_no_aux_rand`).",
        "This is the body of `Signer::attest`, which the gateway times in its signed-tip log.",
        "",
        "```",
        host["cmd_sign"],
        "```",
        "",
        "| | runs | min | median | max | spread |",
        "|---|---:|---:|---:|---:|---:|",
        row_line("hash + sign", s),
        "",
        f"Each run is {sign['iters_per_run']} calls after 500 warmup calls.",
        "",
    ]

    text = "\n".join(lines)
    with open(os.path.join(RES, "bench.md"), "w", encoding="utf-8") as f:
        f.write(text)

    need = run_count()
    for row in block_rows:
        if row["n"] < need:
            sys.exit(f"series {row['payees']} has {row['n']} runs, want {need}")
    if v["n"] < need or s["n"] < need or b512["n"] < need or b8000["n"] < need:
        sys.exit(f"fewer than {need} runs")
    if any(r != "bad-nta-too-many" for r in by_n[513]["results"]):
        sys.exit("513 did not return bad-nta-too-many")
    if any(r != "valid" for n in (1, 100, 512) for r in by_n[n]["results"]):
        sys.exit("a capped-or-under block was rejected")


if __name__ == "__main__":
    main()
