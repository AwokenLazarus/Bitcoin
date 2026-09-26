// Strict payout-address validation, done before any limiter or upstream call.
//
// A regex only says "looks like an address"; this decodes it and checks the checksum, so a typo
// or a crafted string never reaches the pool or the explorer. Accepted, mainnet only:
//   bech32  (BIP173)  witness v0, 20-byte (P2WPKH) or 32-byte (P2WSH) program   bc1q…
//   bech32m (BIP350)  witness v1, 32-byte program (P2TR)                        bc1p…
//   base58check       version 0x00 (P2PKH, 1…) or 0x05 (P2SH, 3…), 20-byte hash
// Witness v2+ is refused: it decodes, but this chain's nodes reject outputs to undefined witness
// versions, so no pool can pay one. Testnet (tb1, m/n/2) and anything else is refused.

const CHARSET = "qpzry9x8gf2tvdw0s3jn54khce6mua7l";
const BECH32M = 0x2bc830a3;

function polymod(values) {
  const GEN = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
  let chk = 1;
  for (const v of values) {
    const top = chk >>> 25;
    chk = ((chk & 0x1ffffff) << 5) ^ v;
    for (let i = 0; i < 5; i++) if ((top >>> i) & 1) chk ^= GEN[i];
  }
  return chk >>> 0;
}
const hrpExpand = (hrp) => [...[...hrp].map((c) => c.charCodeAt(0) >> 5), 0, ...[...hrp].map((c) => c.charCodeAt(0) & 31)];

function convertBits(data, from, to) {
  let acc = 0, bits = 0;
  const out = [], max = (1 << to) - 1;
  for (const v of data) {
    acc = (acc << from) | v;
    bits += from;
    while (bits >= to) { bits -= to; out.push((acc >> bits) & max); }
  }
  if (bits >= from || ((acc << (to - bits)) & max)) return null; // non-zero padding
  return out;
}

/** A segwit address, lower-cased, or null. */
function segwit(addr) {
  if (addr.length < 14 || addr.length > 90) return null;
  if (addr !== addr.toLowerCase() && addr !== addr.toUpperCase()) return null; // mixed case is invalid
  const a = addr.toLowerCase(), sep = a.lastIndexOf("1");
  if (a.slice(0, sep) !== "bc" || a.length - sep - 1 < 6) return null;
  const data = [];
  for (const c of a.slice(sep + 1)) { const i = CHARSET.indexOf(c); if (i < 0) return null; data.push(i); }
  const check = polymod([...hrpExpand("bc"), ...data]);
  const ver = data[0], prog = convertBits(data.slice(1, -6), 5, 8);
  if (!prog || ver > 16) return null;
  if (ver === 0 && (check !== 1 || (prog.length !== 20 && prog.length !== 32))) return null;
  if (ver === 1 && (check !== BECH32M || prog.length !== 32)) return null;
  if (ver > 1) return null; // undefined witness version: not payable on this chain
  return a;
}

const B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
async function base58check(addr) {
  if (addr.length < 26 || addr.length > 35 || !/^[13]/.test(addr)) return null;
  let n = 0n;
  for (const c of addr) { const i = B58.indexOf(c); if (i < 0) return null; n = n * 58n + BigInt(i); }
  const bytes = [];
  while (n > 0n) { bytes.unshift(Number(n & 0xffn)); n >>= 8n; }
  for (const c of addr) { if (c !== "1") break; bytes.unshift(0); }
  if (bytes.length !== 25 || (bytes[0] !== 0x00 && bytes[0] !== 0x05)) return null;
  const body = new Uint8Array(bytes.slice(0, 21));
  const h = new Uint8Array(await crypto.subtle.digest("SHA-256", await crypto.subtle.digest("SHA-256", body)));
  for (let i = 0; i < 4; i++) if (h[i] !== bytes[21 + i]) return null;
  return addr;
}

/** The canonical form of a mainnet payout address, or throw. Never calls anything outside the isolate. */
export async function payoutAddress(v, name = "address") {
  const what = `${name} must be a valid mainnet payout address (bc1q…, bc1p…, 1… or 3…) with a correct checksum`;
  if (typeof v !== "string") throw new Error(what);
  const s = v.trim();
  if (s.length > 90 || !/^[0-9A-Za-z]+$/.test(s)) throw new Error(what);
  const ok = /^bc1/i.test(s) ? segwit(s) : await base58check(s);
  if (!ok) throw new Error(what);
  return ok;
}
