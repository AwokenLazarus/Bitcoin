# Staging Sovereign TIDES: design summary

How the Sovereign TIDES prototype ([design.md](design.md)) was reworked to be safe to run
inside a production pool, and the stages it would go through. This is a summary of the
internal staging plan (2026-09-25, amended 2026-09-26). Operational details of the
production pool are left out.

**Status (2026-09-27):** S0 is built and verified on regtest (series `s0` in [patches/primed](patches/primed/)).
Nothing is deployed. S1–S5 have not started; S6 (the bonus) is off and is a separate
decision.

## Why a rework was needed

The first prototype worked on regtest but was not safe for a busy pool. The problems,
grouped by risk:

**High (must fix before any deployment)**
- **H1. The detector ran under the ledger lock.** Building stats recomputed detector rows
  and a pairwise clustering of every gateway pair (O(gateways² × snapshots)) while holding
  the lock that share crediting also takes. Benchmarked: one stats build took 5 s at 10
  gateways × 2,000 snapshots, 149 s at 60 × 2,000, and 233 s at 240 × 200. With a stats
  poll every few seconds the pool would stop crediting shares.
- **H2. No master switch.** Telemetry ran on every job and share whatever the config said;
  only the payout was gated.
- **H3. Attested blocks were recorded per share**, not per block: unbounded memory and the
  wrong denominator for the score.
- **H4. Unbounded snapshot keys.** Stock DATUM gateways make a new identity key at every
  start ([gateway-readiness §3](../node-template-consensus/gateway-readiness.md#3-identity-keys-are-regenerated-at-every-start)),
  so every restart added a key that was never evicted.
- **H5. Honest gateways scored 0 on a quiet chain.** A gateway whose template equals the
  pool's was treated as a proxy even with perfect canary results. On a low-traffic chain
  honest nodes often build the pool's exact template.
- **H6. Bonus accounting was neither windowed nor persisted** (only matters before a bonus).

**Medium (handled by the plan)**: strict config parsing means a binary rollback needs a
config rollback; new HTTP routes needed a mode switch and a local token; the stats schema
carried payout addresses that must never become public; detector state was lost on restart;
key lookup matched prefixes.

**Low (inert)**: new library code off the hot path; a 0-value 70-byte LZT1 `OP_RETURN` is
already tolerated by the share classifier and valid under RDTS; unknown DATUM sub-commands
from gateways are ignored by the current Prime.

## S0: the pool-safe rework (built)

| Work item | What it does |
|---|---|
| W1 | `sovereignty-mode = "off" \| "observe" \| "publish"`, default `off`. Off is byte-for-byte the old behaviour: no telemetry calls, no stats object, 404 on the routes. |
| W2 | Detector off the hot path: a background task recomputes a cached report every few minutes without the ledger lock; clustering uses an index `(prev, branches) → gateways`, O(total snapshots). |
| W3 | Attested blocks recorded once per block, in a bounded, persisted ring. |
| W4 | Snapshot history keyed by the registered key or the payout script, evicted after 24 h, capped at 512 keys. |
| W5 | "Identical to the pool" counts against a gateway only when it has **no canary hits**. |
| W7 | A DATUM client→server sub-command `0x5A` carries the LZT1 attestation (`job_id ‖ variant ‖ A(185) ‖ sig(64)`); Prime verifies it on the first share of that coinbase variant. |
| W8 | Canary evidence from Prime's existing full-template checks: after a canary is delivered, one extra check 45 s later. No gateway patch needed. |
| W9 | A canary sidecar outside Prime: builds each conflict pair, keeps the pool node from templating the canary, and pushes the canary only to the gateway's registered node. The canary key never sits on the pool node. |
| W10 | Registration v1 (gateway key + node address) and v2 (adds a BIP322 signature by the payout address; XBT Knots wallets sign with `SIGHASH_UNIFIED` 0x21, which the verifier accepts). |
| W11 | Stats hygiene: no payout directory unless the bonus is on; node addresses never. |
| W12 | Gateway patches: `lazarus-gateway` and the split-only C gateway emit LZT1 per coinbase variant, send `0x5A`, and persist their identity key. |
| W6 | Windowed, persisted bonus accounting. **Not built**; only needed before a bonus. |

Also in S0 (from the NTA work): the gateway signs the NTA digest per tip (`0x4e`) and Prime
builds attested coinbases, carrying non-signers.

**Exit gate** for S0: tests green; the W2 benchmark met (report < 1 s at 250 gateways ×
2,000 snapshots; the stats build holds the ledger lock < 5 ms); a 72 h soak with ≥ 50
simulated restarting gateways, 2 honest Knots gateways and 1 proxy in `publish` mode, where
the proxy is flagged, the honest gateways are not, memory is flat, and p99 share-accept
latency is within 0.25 ms of the old build. (The latency bar was relative, 1.10×, until the
soak showed a uniform cost of about +10 µs p50 / +50 µs p99 per share, where a ratio at
microsecond scale measures host noise.)

## Stages after S0

Each stage is a config flag with a rollback and a read-only check.

| Stage | Turn on | Payout effect | Check |
|---|---|---|---|
| **S1 · dark ship** | new binary, no config change (mode defaults to `off`) | none | stats has no `sovereignty` object; share rate and gateway count unchanged |
| **S2 · observe** | `sovereignty-mode = "observe"`; the pool's own gateway emits LZT1, then a few friendly operators; registration v1 by invite | none, nothing published | every attested job verifies; no `WrongPayout`/`WrongTemplate` from honest gateways; report compute time < 1 s; attested blocks counted per block; memory flat for 7 days |
| **S3 · canary shadow (≥ 30 days)** | the canary sidecar delivers canaries to registered gateways, plus a pool-run **control** gateway on its own node (scored, never bonus-eligible) and one **planted proxy** | none (apart from canary fees) | honest miss rate ≤ 5%; 0 flags on the control and known-honest gateways; the planted proxy flagged; 0 canary leaks into pool templates; ≥ 5 real enrolled gateways |
| **S4 · public score** | `sovereignty-mode = "publish"`; a sanitised public API and a site section | none | site numbers equal the pool's report; no payout or node address anywhere in the API; the score replays from chain data plus published attestations |
| **S5 · BIP322 binding** | registration v2 required for bonus eligibility | none | every eligible key has a v2 proof |
| **S6 · bonus** | **off**; a separate decision later | — | — |

The public score comes before the payout binding because the score is about who **built**
a template (the gateway key plus canary evidence), not who gets **paid**.

## Registration

**v1** (S2–S4) proves the gateway key and names the gateway's node:

```
LZT1 register v1
pool: lazarus-xbt
chain: XBT
G: <64 hex, ed25519 DATUM identity key>
node: <host:port | xxx.onion:port | none>
tag: <DATUM secondary tag, ≤ 32 chars>
at: <height> <block hash hex>
```

- Lines separated by `\n`, no trailing newline; `at` must be within the last 144 blocks
  (a stateless anti-replay nonce).
- Signed by G: ed25519 over `"XBT-SOVEREIGN-TIDES/register\0" ‖ message`.
- `node: none` is allowed; that gateway can be attested but never canaried, so its weight
  stays 0 ("gathering evidence").
- Node addresses are private and never published.

**v2** (S5) adds `payout: <address>` and a second signature, BIP322 by the payout address
(simple for P2WPKH / P2TR key path / single-key P2WSH, full for P2SH-P2WPKH, classic
`signmessage` for P2PKH). The payout must match what the gateway's coinbase already pays.
A new v2 with a new G supersedes the old G; revocation is a signed `LZT1 revoke v2`. For
wallets without BIP322, "enrol by spend": a self-spend from the payout carrying
`OP_RETURN "LZT1E" ‖ SHA256(message)[0..16]`.

**Existing gateways**: the pool's own gateways are listed as `house` and never score or
earn. Stock `v0.4.1-beta` gateways change key on every restart, so they must move to a
build that persists its key before they can enrol; until then they show as "tag only"
(weight 0), and their base TIDES pay does not change.

## Canaries

- A conflict pair: C goes only to the gateway's registered node, C′ (+1 sat) to the pool
  node, and the pool node deprioritises C. One of the pair confirms; both pay back to the
  canary wallet, so the cost is one fee per canary.
- Three kinds per round (SOV-004): **own** canaries per gateway, **shared** canaries every
  honest node templates but the pool's template excludes, and **decoys** only the pool's
  template carries. Together they force a hybrid proxy's transaction set to its own node's.
  **Foreign** canaries (SOV-015) fan each gateway's canary twin out to other registered
  nodes within 50 ms to find gateways that share one template source.
- Cadence: frequent for new, suspect or flagged gateways, hourly during the shadow, less
  often for gateways with a long clean record; delivery time jittered.
- Deliveries come from rotating peers or Tor, never from the pool node's known address, so
  a canary-aware proxy can't pick them out by origin. On chain, canaries link only to the
  canary wallet, which is kept separate from the pool's other wallets.

## Public score (S4)

- **Lazarus:** the independence-weighted score, with the raw attested share and the
  tag-claimed share beside it.
- **Other pools:** tag and LZT1 figures but **no score**, since they publish no canary
  evidence and an unknown independence is 0.
- **Per gateway:** status (sovereign / gathering evidence / flagged / tag only / house),
  canaries hit/due, independence, weight, LZT1-verified blocks. Payouts masked, no node
  addresses.
- Lazarus's score starts near 0 at launch, because unknown is 0 and few keys are enrolled.

## Open items

- Confirm `0x5A` stays unused upstream (it is unused in OCEAN v0.4.1 and master today).
- Settle `LZT1` versus a separate window-root output (`LZW1`) for verifiable TIDES.
- Tor delivery of canaries is tested only at the framing level; it is an S3 prerequisite.
- A farm that tailors each gateway's template on conflicts is not caught yet.
