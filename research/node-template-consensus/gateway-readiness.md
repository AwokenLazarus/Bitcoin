# Gateway readiness: DATUM gateways before NTA activation

**Finding: stock DATUM gateways would mine invalid blocks after NTA activation, and
nothing would tell their operators.** This page gives the upstream evidence, what a
gateway release has to change, and how a pool can measure which of its gateways are
ready before an activation height is chosen.

Upstream is OCEAN `datum_gateway` [v0.4.1beta](https://github.com/OCEAN-xyz/datum_gateway/tree/5b061233a3d3323771b2be98e17f543e59346619)
(`5b06123`), cross-checked on master
[`dbc3b14`](https://github.com/OCEAN-xyz/datum_gateway/tree/dbc3b143589842feb606a409b40cd70f67117b45)
and on the BLAKE2b forks XBT miners run (§1). Line numbers below are permalinks to
v0.4.1beta unless stated.

> **Correction (2026-09-28).** The first version of this page said stock gateways
> *truncate* a coinbaser at the first script over 64 bytes. That is OCEAN v0.4.1 only.
> OCEAN master and every XBT lineage *discard* the whole coinbaser and mine pool-only
> jobs, so the published probe, which judged only full-split shares, never reached a
> verdict on them. §1 and §5 are corrected, and the `sov-011` patch series now carries
> the fix (`mit/0005`, `mit/0006`). The regtest evidence is in §5.

[v041]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619
[master]: https://github.com/OCEAN-xyz/datum_gateway/blob/dbc3b143589842feb606a409b40cd70f67117b45

## 1. The 64-byte coinbaser limit: truncate or discard

No released gateway carries a coinbaser script over 64 bytes. What it does with one
depends on the lineage.

**v0.4.1 truncates.**
- [`src/datum_coinbaser.c:795`][cb795] in `datum_coinbaser_v2_parse` ([:761][cb761]):
  `if ((slen < 2) || (slen > 64)) { break; }`. The parse loop stops at the first output
  script longer than 64 bytes. The outputs before it are kept, every output after it is
  dropped, and no error is raised. The value not handed out goes to the pool address when
  the coinbase is built.
- The script buffer is `unsigned char output_script[64]`
  ([`src/datum_stratum.h:115`][st115]).

**Master and the XBT lineages discard.**
- Master keeps the limit, adds a bounds check, and throws the whole coinbaser away:
  [`src/datum_coinbaser.c:801–804`][m801]:
  `if (slen < 2 || slen > 64 || cidx + slen > cblen) { DLOG_ERROR("Script length (%d)
  is invalid. Using default/empty", slen); s->available_coinbase_outputs_count = 0;
  return 0; }`.
- With no coinbaser outputs a pooled job is built "empty only"
  ([`:528–530`][m528]): it pays the pool address alone, and its shares cite coinbaser 0.
- The BLAKE2b forks that XBT miners run have the same code:

  | Lineage | Commit | Discard at |
  |---|---|---|
  | iohzrd, the StartOS `pow_0.4.1_23` pin | [`7491a50`][s832] | `datum_coinbaser.c:832` |
  | iohzrd master | [`c031568`][i772] | `:772` |
  | FlyTheElephant1 master | [`a5f28aa`][f831] | `:831` |
  | CONVOY | [`b9ea7dc`][c806] | `:806` |

[cb795]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_coinbaser.c#L795
[cb761]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_coinbaser.c#L761
[st115]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_stratum.h#L115
[m801]: https://github.com/OCEAN-xyz/datum_gateway/blob/dbc3b143589842feb606a409b40cd70f67117b45/src/datum_coinbaser.c#L801-L804
[m528]: https://github.com/OCEAN-xyz/datum_gateway/blob/dbc3b143589842feb606a409b40cd70f67117b45/src/datum_coinbaser.c#L528-L530
[s832]: https://github.com/iohzrd/datum_gateway/blob/7491a5099dd5d887a027c812f71de63e0d5986a3/src/datum_coinbaser.c#L832
[i772]: https://github.com/iohzrd/datum_gateway/blob/c0315682aef40dc7e0674a7bb8fb523b27a6886d/src/datum_coinbaser.c#L772
[f831]: https://github.com/FlyTheElephant1/datum_gateway/blob/a5f28aa873bd241f60402259a5ef93bcd34aa4e1/src/datum_coinbaser.c#L831
[c806]: https://github.com/CONVOYMining/datum_gateway/blob/b9ea7dc3eb91352565ab487ec55ed6ee5964a440/src/datum_coinbaser.c#L806


**Consequence.** An NTA attestation output is 70 bytes and comes after the payees. A
truncating gateway keeps the payees and drops every attestation; a discarding one drops
the payees too and pays only the pool. Either way, from the activation height its blocks
are `bad-nta-count`. The miners keep hashing and every block that gateway finds is lost.
This is not about signing: the gateway cannot even carry attestations the pool signed for
it.

## 2. Unused DATUM sub-commands

The prototype adds two client→server mining sub-commands: `0x4e` (NTA signature per tip)
and `0x5A` (the Sovereign TIDES LZT1 attestation). Neither is used upstream, in either
direction:
- sub-commands the stock client sends: `0x10` ([`datum_protocol.c:337`][p337]), `0x27`
  ([:1329][p1329]), and job-validation replies `0x50` with `0x90/0x91/0x92` (from [:472][p472]);
- sub-commands it accepts: `0x99` ([:930][p930]), `0x11` ([:938][p938]), `0x50`
  ([:944][p944]), `0x8F` ([:950][p950]), `0xF9` ([:956][p956]);
- job-validation sub-sub-commands `0x10/0x11/0x12` ([:862–876][p862]).

A server→client sub-command a stock gateway doesn't know reaches `default:` and logs
"Received unknown mining command … Perhaps you need to upgrade this DATUM Gateway?"
([:964–966][p964]). It returns 0, and only a negative return ends the session
([:1833][p1833]), so it is a warning, not a disconnect.

[p337]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_protocol.c#L337
[p1329]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_protocol.c#L1329
[p472]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_protocol.c#L472
[p930]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_protocol.c#L930
[p938]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_protocol.c#L938
[p944]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_protocol.c#L944
[p950]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_protocol.c#L950
[p956]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_protocol.c#L956
[p862]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_protocol.c#L862-L876
[p964]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_protocol.c#L964-L966
[p1833]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_protocol.c#L1833

## 3. Identity keys are regenerated at every start

`datum_encrypt_generate_keys(&local_datum_keys)` runs at every process start
([`datum_protocol.c:1926`][p1926]) and the key is never saved. Anything keyed on the
gateway's identity (the Sovereign TIDES attestation key, detector history) resets on every
restart unless the gateway persists its key. The patched gateways do.

[p1926]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_protocol.c#L1926

## 4. What a gateway release has to do

Prototyped in two gateways (patches in
[`../sovereign-tides/patches/primed/`](../sovereign-tides/patches/primed/)):
the Rust `lazarus-gateway` and a "split-only" patch for the C gateway
(`lazarus/patches/datum-gateway-split-only.patch` inside those series; since `sov-011`
`agpl/0003` it applies to FlyTheElephant1 master `a5f28aa`).

- Accept exactly the 70-byte attestation shape `6a44 "NTA" 02 ‖ sig64` with value 0 in the
  coinbaser, and nothing else over 64 bytes. The C patch grows `output_script` to 70 bytes.
- Keep zero-value outputs that come **after** payees that already use up the coinbase value.
  The first live run of the C patch advertised readiness and then dropped every
  attestation, because both output loops had `if (mval >= s->coinbase_value) break;`.
  That is fixed in the `sov-011` series, and it is why a pool must not trust the
  readiness flag alone (below).
- Optionally, sign the NTA digest for each new tip with the payout key and send the 64
  bytes to the pool (`0x4e`). Gateways that don't sign are carried, not lost.
- Persist the identity key (file created `O_EXCL`, mode 0600; refuse to start if it is
  group- or world-readable).

## 5. Measuring readiness per gateway

The pool side (Prime, in the `sov-011` series) implements:

1. **A capability flag.** A capable gateway sends `"nta-v1\0"` right after the 4-byte
   header-key seed in the DATUM hello. Stock v0.4.1 fills that spot with padding
   ([:1028–1033][p1028]: "TODO: maybe tack on other useful data here", then
   `memset(&hello_msg[i], rand(), j)`, one random byte repeated 1–200 times), and every
   server skips it. A stock pad can never spell the marker; the test checks all 256 bytes.
2. **A probe.** From `nta-height − 2016` until activation, one coinbaser per gateway per
   hour gets a zero-value 70-byte NTA-shaped `OP_RETURN` at the end (after the pool output,
   so a truncating gateway still keeps every payee). The first full-split share on a probed
   coinbaser decides: probe present means ready, probe missing means `nta-unready`.
   Before activation the probe is an ordinary `OP_RETURN` and is checked by a unit test
   through a line-by-line port of `datum_coinbaser_v2_parse`.
   **Advertisers are probed too**, and a dropped probe outranks the flag.

   **The probe must also count pool-only shares.** A discarding gateway (§1) never sends a
   full-split share on a probed coinbaser: it throws that coinbaser away and mines
   pool-only jobs until its next one. A rule that judges only full-split shares therefore
   never decides, and the gateway stays `unknown` and is served after activation. That is
   what happened with the real StartOS-pin and iohzrd builds (below). So, in `sov-011`
   `mit/0005`–`0006`:
   - the session remembers the last coinbaser it sent that carried payees and a 70-byte
     output (the probe; after activation, the attestations), with its height and time;
   - a pool-only share at that height, on a job with transactions, first seen after that
     coinbaser, is a **strike**, and the second strike makes the gateway `nta-unready`
     with the evidence "coinbaser discarded". One strike is not enough: a stock gateway
     also sends one pool-only job at each new height with the split already in hand, and
     its per-height empty job has no transactions;
   - **after activation one strike is enough**: a pool-only share whose coinbase has no
     attestation is invalid work, whatever caused it.

   For a discarding gateway the probe is not free. Its jobs pay only the pool from the
   probe until its next coinbaser fetch, and a block found then is pool-only and owes the
   TIDES window. One probe an hour keeps that to about one job window per hour.
3. **A refusal after activation** (`nta-unready-policy = "refuse"`, the default). An
   unready gateway gets no work. The pool sends a DATUM server message (logged by stock at
   INFO as "DATUM Server message: …", [:1249–1252][p1249]) and the `0x4e` notice (logged at
   WARN, see §2), then closes the connection itself after a short linger. A `pool-only`
   policy exists and does not help: the pool's own output needs an attestation too, which
   a stock gateway loses the same way.
4. **A readiness metric** (`/nta.json` → `readiness`): gateways and share-difficulty-weighted
   hashrate that are ready, unready or unknown, and blocks until activation.

[p1028]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_protocol.c#L1028-L1033
[p1249]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_protocol.c#L1249-L1252

**Regtest demo** ([`../sovereign-tides/demos/sov-011/`](../sovereign-tides/demos/sov-011/),
two NTA Knots nodes, `nta@420`): a Rust gateway and the real patched C gateway advertise
and keep the probe; a gateway emulating stock v0.4.1 parsing keeps all four payee outputs
and drops the probe, is marked unready with 0 rejected shares, and is refused from
activation on (13 refused reconnects). The ready gateways mine blocks 420 and 421, each
with one attestation per payee, accepted by both nodes. Passed twice.

**Real stock gateways** (SOV-020, the same demo on a separate regtest lab, with
unmodified builds added: the StartOS pin `7491a50`, iohzrd `c031568`, OCEAN `5b06123`):
- **Without the fix,** both XBT builds logged `Script length (70) is invalid. Using
  default/empty` for every probed coinbaser (146 of 146 and 206 of 206). Their shares were
  pool-only, so they stayed `unknown`, were not refused at activation and kept getting
  work, and the StartOS build found block 422: pool-only, refused by the node as
  `bad-nta-count`.
- **With `mit/0005`** both were `nta-unready (coinbaser discarded)` after two pool-only
  shares, and were refused at activation. The unmodified binaries logged Prime's reason
  and the `0x4e` notice exactly as cited in §2 and §5.3:
  ```
  [datum_protocol_server_msg]  INFO: DATUM Server message: Prime refuses this gateway from block 420: it threw away every coinbaser carrying a 70-byte XBT-NTA output and paid only the pool. …
  [datum_protocol_mining_cmd5]  WARN: Received unknown mining command 4E from DATUM Server.  Perhaps you need to upgrade this DATUM Gateway?
  ```
- **Why `mit/0006`:** in a later run with `0005` only, the StartOS build reached
  activation with no strike, and its first post-activation share (strike one of two) was
  block 422 again, `bad-nta-count`, while it was still served. With `0006` it reached
  activation with one strike, its first post-activation share made it unready, and it
  was refused. No invalid block was submitted.
- The split-only C gateway kept the probe and mined valid post-activation blocks in every
  run.
- OCEAN `5b06123` connects but never gets work on XBT: its `getblocktemplate` lacks the
  `blake2b` rule and fails, and it mines SHA256d. Its truncation (§1) matters only in
  principle.

## 6. Proposed activation rule (coordination, not consensus)

Set an activation height only once the pools that serve DATUM gateways report at least
**95%** of that hashrate ready, sustained for a full retarget period, and give the
remaining operators the announcement lead time to upgrade. The same lead time covers the
Taproot payout-address deadline.

## Not proven

- **The discard rule is statistical.** A strike needs a share on a job built from the
  probed coinbaser, and with hourly probes the gateway's next coinbaser fetch comes
  about 40 s later. A gateway with several shares a minute gets two strikes in about two
  probes; the 2,016-block window gives it about two weeks. A gateway that reaches
  activation still `unknown` is caught on its first post-activation pool-only share. On
  mainnet that is a share, not a block, but on a small network the share can be a block.
- **Stock regenerates its identity at every start** (§3), so a restarted stock gateway is
  a new key and needs two more strikes. Strikes never decay.
- The real stock gateways were run on regtest with CPU miners (1–3 pre-activation shares
  each), not with real hashrate.
- A refused gateway's hashrate share decays after activation because it stops submitting
  shares; the metric is meant for the decision before activation.
- One probe per gateway per hour adds 79 bytes to one coinbaser an hour.
