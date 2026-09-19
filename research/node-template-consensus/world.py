#!/usr/bin/env python3
"""World Monte Carlo: Stratum PPS vs DATUM vs censoring vs gifted.

Hashrate is a lottery over who finds each block. Templates/ILs are per agent.
The chain uses the same ConnectBlock as test_consensus.py.
"""

from __future__ import annotations

import random
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Tuple

from helpers import att, block, fund, tx
from spec import Chain, Tx
from validate import connect, tx_key, validate


@dataclass
class Agent:
    name: bytes
    alpha: float
    kind: str  # datum | pps | solo | gifted_customer
    censors_t: bool
    # If kind==datum and paid by whoever finds, they attach their IL.


@dataclass
class WorldResult:
    label: str
    blocks: int
    t_confirmed: bool
    t_delay: Optional[int]
    rejected_unattested: int
    rejected_il: int
    rejected_other: int
    accepted: int
    distinct_finders: int
    notes: str = ""


def _pick(rng: random.Random, agents: List[Agent]) -> Agent:
    x = rng.random()
    acc = 0.0
    for a in agents:
        acc += a.alpha
        if x <= acc:
            return a
    return agents[-1]


def run_world(
    label: str,
    agents: List[Agent],
    *,
    blocks: int = 400,
    seed: int = 0,
    pay_datum: bool = True,
    # If True, PPS finder pays only itself (off-chain IOU). Residual veto.
    # If False and they try to put unattested customers in coinbase, reject.
    pps_pay_unattested_customers: bool = False,
    # Gifted: PPS builds attestations for fake customers with pool IL (censors T).
    pps_gift_customers: int = 0,
    t_arrives_at: int = 0,
) -> WorldResult:
    rng = random.Random(seed)
    s = sum(a.alpha for a in agents)
    if abs(s - 1.0) > 1e-9:
        raise ValueError(f"alphas must sum to 1, got {s}")

    chain = Chain(height=0, bits_target=1 << 200, k=1000, W=144)
    # Funding for each agent's extra-tx and for T.
    extras: Dict[bytes, Tx] = {}
    for a in agents:
        op = fund(chain, a.name, 50_000, b"m" + a.name)
        extras[a.name] = tx(b"e" + a.name, (op,), ((a.name, 1),))

    t_op = fund(chain, b"user", 10_000, b"tfund")
    T = tx(b"TX-T", (t_op,), ((b"user", 9_000),))
    t_live = False
    t_done = False

    t_confirmed = False
    t_delay: Optional[int] = None
    rej_un = 0
    rej_il = 0
    rej_o = 0
    acc = 0
    finders = set()

    def make_att(agent: Agent, height: int, include_t: bool):
        il = (T,) if (include_t and t_live and not t_done and not agent.censors_t) else ()
        return att(
            agent.name,
            height,
            chain.bits_target,
            extras[agent.name],
            il,
        )

    for i in range(blocks):
        if i == t_arrives_at:
            t_live = True
        height = chain.height + 1
        finder = _pick(rng, agents)
        finders.add(finder.name)

        payees: List[Tuple[bytes, int]] = []
        atts = []

        if finder.kind == "pps":
            payees.append((finder.name, 50))
            atts.append(make_att(finder, height, include_t=not finder.censors_t))
            if pps_pay_unattested_customers:
                payees.append((b"stratum-user", 10))
            for n in range(pps_gift_customers):
                fake = b"gift" + bytes([n])
                if fake not in extras:
                    op = fund(chain, fake, 50_000, b"g" + fake)
                    extras[fake] = tx(b"e" + fake, (op,), ((fake, 1),))
                atts.append(
                    att(
                        fake,
                        height,
                        chain.bits_target,
                        extras[fake],
                        (),
                    )
                )
                payees.append((fake, 1))
        elif finder.kind == "datum":
            # DATUM pool: pay all datum agents + self, each with own IL.
            datum_agents = [x for x in agents if x.kind == "datum"]
            if pay_datum:
                share = max(1, 50 // max(1, len(datum_agents)))
                for d in datum_agents:
                    payees.append((d.name, share))
                    atts.append(make_att(d, height, include_t=True))
            else:
                payees.append((finder.name, 50))
                atts.append(make_att(finder, height, include_t=True))
        else:  # solo
            payees.append((finder.name, 50))
            atts.append(make_att(finder, height, include_t=not finder.censors_t))

        # Include T if we must (validate will tell us) OR if finder wants it.
        want_t = (
            t_live
            and not t_done
            and any(T.txid in {t.txid for t in a.il} for a in atts)
        )
        txs = []
        cand = block(height, chain.bits_target, payees, atts, [])
        v0 = validate(chain, cand)
        if not v0.ok and any("il_unsatisfied" in e for e in v0.errors) and t_live and not t_done:
            txs = [T]
            cand = block(height, chain.bits_target, payees, atts, txs)
            want_t = True
        elif want_t and finder.kind != "pps":
            txs = [T]
            cand = block(height, chain.bits_target, payees, atts, txs)
        elif want_t and finder.kind == "pps" and not finder.censors_t:
            txs = [T]
            cand = block(height, chain.bits_target, payees, atts, txs)

        v = connect(chain, cand)
        if not v.ok:
            if any("unattested_payee" in e for e in v.errors):
                rej_un += 1
            elif any("il_unsatisfied" in e for e in v.errors):
                rej_il += 1
            else:
                rej_o += 1
            continue
        acc += 1
        if tx_key(T) in chain.confirmed and not t_confirmed:
            t_confirmed = True
            t_delay = i - t_arrives_at
            t_done = True
            t_live = False

    return WorldResult(
        label=label,
        blocks=blocks,
        t_confirmed=t_confirmed,
        t_delay=t_delay,
        rejected_unattested=rej_un,
        rejected_il=rej_il,
        rejected_other=rej_o,
        accepted=acc,
        distinct_finders=len(finders),
    )


def main() -> None:
    rows: List[WorldResult] = []

    # 1. 100% censoring PPS (Foundry-shaped). T never forced.
    rows.append(
        run_world(
            "100% censoring PPS (self-pay)",
            [Agent(b"foundry", 1.0, "pps", True)],
            blocks=300,
            t_arrives_at=10,
        )
    )

    # 2. Same pool tries to put stratum usernames in coinbase: every such block dies.
    rows.append(
        run_world(
            "PPS pays unattested stratum usernames",
            [Agent(b"foundry", 1.0, "pps", True)],
            blocks=80,
            pps_pay_unattested_customers=True,
            t_arrives_at=0,
        )
    )

    # 3. Gifted customers (pool node, many names, pool IL): blocks live, T still vetoed.
    rows.append(
        run_world(
            "PPS gifts 5 customer attestations (residual)",
            [Agent(b"foundry", 1.0, "pps", True)],
            blocks=80,
            pps_gift_customers=5,
            t_arrives_at=10,
        )
    )

    # 4. DATUM majority: whoever finds pays all DATUM miners => T included fast.
    datum = [
        Agent(b"d0", 0.2, "datum", False),
        Agent(b"d1", 0.2, "datum", False),
        Agent(b"d2", 0.2, "datum", False),
        Agent(b"d3", 0.2, "datum", False),
        Agent(b"d4", 0.2, "datum", False),
    ]
    rows.append(run_world("100% DATUM pool (5 nodes)", datum, blocks=200, t_arrives_at=5))

    # 5. 50% censoring PPS + 50% DATUM. When DATUM finds (or PPS is forced by
    #    carry-forward after a DATUM block), T should land.
    mix = [
        Agent(b"foundry", 0.5, "pps", True),
        Agent(b"d0", 0.1, "datum", False),
        Agent(b"d1", 0.1, "datum", False),
        Agent(b"d2", 0.1, "datum", False),
        Agent(b"d3", 0.1, "datum", False),
        Agent(b"d4", 0.1, "datum", False),
    ]
    rows.append(run_world("50% censoring PPS + 50% DATUM", mix, blocks=400, t_arrives_at=20))

    # 6. 90% censoring PPS + 10% DATUM: T still lands once a DATUM block (or paid
    #    DATUM on a... wait, PPS finder does NOT pay DATUM. T lands only when
    #    DATUM finds. That's honest: unpaid nodes don't constrain PPS.
    mix90 = [
        Agent(b"foundry", 0.9, "pps", True),
        Agent(b"d0", 0.1, "datum", False),
    ]
    rows.append(
        run_world(
            "90% PPS (doesn't pay DATUM) + 10% DATUM",
            mix90,
            blocks=500,
            t_arrives_at=10,
        )
    )

    # 7. 90% PPS that DOES pay DATUM miners (hybrid pool): then PPS is bound.
    # Custom: we need a finder-pps that pays datum. Encode as datum kinds all
    # paid when any datum finds; for PPS paying datum, special case via kind
    # hack: treat foundry as datum with censors_t True but pay_datum True
    # would force including others' ILs which include T.
    hybrid = [
        Agent(b"foundry", 0.9, "datum", True),  # finder censors own IL
        Agent(b"d0", 0.1, "datum", False),
    ]
    rows.append(
        run_world(
            "90% pool that PAYS a 10% node-miner (must take their IL)",
            hybrid,
            blocks=200,
            t_arrives_at=10,
        )
    )

    # 8. Honest solos, no pool.
    solos = [Agent(bytes([i]), 0.1, "solo", False) for i in range(10)]
    rows.append(run_world("10 honest solos", solos, blocks=200, t_arrives_at=5))

    print(
        f"{'scenario':<58} {'acc':>5} {'T_in':>5} {'delay':>6} "
        f"{'rej_user':>8} {'rej_il':>7} {'finders':>8}"
    )
    for r in rows:
        delay = "-" if r.t_delay is None else str(r.t_delay)
        tin = "yes" if r.t_confirmed else "no"
        print(
            f"{r.label:<58} {r.accepted:5d} {tin:>5} {delay:>6} "
            f"{r.rejected_unattested:8d} {r.rejected_il:7d} {r.distinct_finders:8d}"
        )
    return rows


if __name__ == "__main__":
    main()
