# Gateway readiness: DATUM gateways before NTA activation

**Finding: stock DATUM gateways would mine invalid blocks after NTA activation, and
nothing would tell their operators.** This page gives the upstream evidence, what a
gateway release has to change, and how a pool can measure which of its gateways are
ready before an activation height is chosen.

Upstream is OCEAN `datum_gateway` [v0.4.1beta](https://github.com/OCEAN-xyz/datum_gateway/tree/5b061233a3d3323771b2be98e17f543e59346619)
(`5b06123`), cross-checked on master
[`dbc3b14`](https://github.com/OCEAN-xyz/datum_gateway/tree/dbc3b143589842feb606a409b40cd70f67117b45).
Line numbers below are permalinks to v0.4.1beta unless stated.

[v041]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619
[master]: https://github.com/OCEAN-xyz/datum_gateway/blob/dbc3b143589842feb606a409b40cd70f67117b45

## 1. The 64-byte coinbaser limit truncates

- [`src/datum_coinbaser.c:795`][cb795] in `datum_coinbaser_v2_parse` ([:761][cb761]):
  `if ((slen < 2) || (slen > 64)) { break; }`. The parse loop stops at the first output
  script longer than 64 bytes. The outputs before it are kept, every output after it is
  dropped, and no error is raised. The value not handed out goes to the pool address when
  the coinbase is built.
- The script buffer is `unsigned char output_script[64]`
  ([`src/datum_stratum.h:115`][st115]).
- Master keeps the limit and adds a bounds check:
  [`src/datum_coinbaser.c:801`][m801].

[cb795]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_coinbaser.c#L795
[cb761]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_coinbaser.c#L761
[st115]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_stratum.h#L115
[m801]: https://github.com/OCEAN-xyz/datum_gateway/blob/dbc3b143589842feb606a409b40cd70f67117b45/src/datum_coinbaser.c#L801

**Consequence.** An NTA attestation output is 70 bytes and comes after the payees. A stock
gateway keeps the payees and drops every attestation, so from the activation height its
blocks are `bad-nta-count`. The miners keep hashing and every block that gateway finds is
lost. This is not about signing: the gateway cannot even carry attestations the pool
signed for it.

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
(`lazarus/patches/datum-gateway-split-only.patch` inside those series).

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
   Before activation the probe is an ordinary `OP_RETURN`, changes no payout, and is
   checked by a unit test through a line-by-line port of `datum_coinbaser_v2_parse`.
   **Advertisers are probed too**, and a dropped probe outranks the flag.
3. **A refusal after activation** (`nta-unready-policy = "refuse"`, the default). An
   unready gateway gets no work. The pool sends a DATUM server message (logged by stock at
   INFO as "DATUM Server message: …", [:1249–1252][p1249]) and the `0x4e` notice (logged at
   WARN, see §2), then closes the connection itself after a short linger. A `pool-only`
   policy exists and does not help: the pool's own output needs an attestation too, which
   the stock gateway truncates the same way.
4. **A readiness metric** (`/nta.json` → `readiness`): gateways and share-difficulty-weighted
   hashrate that are ready, unready or unknown, and blocks until activation.

[p1028]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_protocol.c#L1028-L1033
[p1249]: https://github.com/OCEAN-xyz/datum_gateway/blob/5b061233a3d3323771b2be98e17f543e59346619/src/datum_protocol.c#L1249-L1252

**Regtest demo** (two NTA Knots nodes, `nta@420`): a Rust gateway and the real patched C
gateway advertise and keep the probe; a gateway emulating stock v0.4.1 parsing keeps all
four payee outputs and drops the probe, is marked unready with 0 rejected shares, and is
refused from activation on (13 refused reconnects). The ready gateways mine blocks 420
and 421, each with one attestation per payee, accepted by both nodes. Passed twice.

## 6. Proposed activation rule (coordination, not consensus)

Set an activation height only once the pools that serve DATUM gateways report at least
**95%** of that hashrate ready, sustained for a full retarget period, and give the
remaining operators the announcement lead time to upgrade. The same lead time covers the
Taproot payout-address deadline.

## Not proven

- A real stock v0.4.1 binary was not run. The stock behaviour was emulated from source,
  and the log lines are cited from source, not observed.
- A refused gateway's hashrate share decays after activation because it stops submitting
  shares; the metric is meant for the decision before activation.
- One probe per gateway per hour adds 79 bytes to one coinbaser an hour.
