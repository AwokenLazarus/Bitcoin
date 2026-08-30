/* BLAKE2b-256, original for Lazarus browser miner. */
(function (root) {
  const MASK = 0xffffffffffffffffn;
  const IV = [
    0x6a09e667f3bcc908n, 0xbb67ae8584caa73bn, 0x3c6ef372fe94f82bn, 0xa54ff53a5f1d36f1n,
    0x510e527fade682d1n, 0x9b05688c2b3e6c1fn, 0x1f83d9abfb41bd6bn, 0x5be0cd19137e2179n,
  ];
  const SIGMA = [
    [0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15],
    [14,10,4,8,9,15,13,6,1,12,0,2,11,7,5,3],
    [11,8,12,0,5,2,15,13,10,14,3,6,7,1,9,4],
    [7,9,3,1,13,12,11,14,2,6,5,10,4,0,15,8],
    [9,0,5,7,2,4,10,15,14,1,11,12,6,8,3,13],
    [2,12,6,10,0,11,8,3,4,13,7,5,15,14,1,9],
    [12,5,1,15,14,13,4,10,0,7,6,3,9,2,8,11],
    [13,11,7,14,12,1,3,9,5,0,15,4,8,6,2,10],
    [6,15,14,9,11,3,0,8,12,2,13,7,1,4,10,5],
    [10,2,8,4,7,6,1,5,15,11,9,14,3,12,13,0],
    [0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15],
    [14,10,4,8,9,15,13,6,1,12,0,2,11,7,5,3],
  ];

  function rotr(x, n) {
    n = BigInt(n);
    return ((x >> n) | (x << (64n - n))) & MASK;
  }

  function blake2b256(data) {
    const outlen = 32;
    const h = IV.slice();
    h[0] ^= 0x01010000n ^ BigInt(outlen);
    const buf = new Uint8Array(128);
    let t = 0n;
    let off = 0;
    function compress(last) {
      const m = new Array(16);
      const dv = new DataView(buf.buffer, buf.byteOffset, 128);
      for (let i = 0; i < 16; i++) m[i] = dv.getBigUint64(i * 8, true);
      const v = h.concat(IV);
      v[12] ^= t;
      if (last) v[14] ^= MASK;
      for (let r = 0; r < 12; r++) {
        const s = SIGMA[r];
        const G = (a, b, c, d, x, y) => {
          v[a] = (v[a] + v[b] + m[x]) & MASK;
          v[d] = rotr(v[d] ^ v[a], 32);
          v[c] = (v[c] + v[d]) & MASK;
          v[b] = rotr(v[b] ^ v[c], 24);
          v[a] = (v[a] + v[b] + m[y]) & MASK;
          v[d] = rotr(v[d] ^ v[a], 16);
          v[c] = (v[c] + v[d]) & MASK;
          v[b] = rotr(v[b] ^ v[c], 63);
        };
        G(0,4,8,12,s[0],s[1]); G(1,5,9,13,s[2],s[3]);
        G(2,6,10,14,s[4],s[5]); G(3,7,11,15,s[6],s[7]);
        G(0,5,10,15,s[8],s[9]); G(1,6,11,12,s[10],s[11]);
        G(2,7,8,13,s[12],s[13]); G(3,4,9,14,s[14],s[15]);
      }
      for (let i = 0; i < 8; i++) h[i] ^= v[i] ^ v[i + 8];
    }
    const u8 = data instanceof Uint8Array ? data : new Uint8Array(data);
    for (let i = 0; i < u8.length; i++) {
      if (off === 128) {
        t += 128n;
        compress(false);
        off = 0;
      }
      buf[off++] = u8[i];
    }
    t += BigInt(off);
    buf.fill(0, off);
    compress(true);
    const out = new Uint8Array(outlen);
    const odv = new DataView(out.buffer);
    for (let i = 0; i < 4; i++) odv.setBigUint64(i * 8, h[i], true);
    return out;
  }

  function hexToBytes(h) {
    h = (h || "").replace(/^0x/, "");
    if (h.length & 1) h = "0" + h;
    const a = new Uint8Array(h.length >> 1);
    for (let i = 0; i < a.length; i++) a[i] = parseInt(h.substr(i * 2, 2), 16);
    return a;
  }
  function bytesToHex(b) {
    let s = "";
    for (let i = 0; i < b.length; i++) s += b[i].toString(16).padStart(2, "0");
    return s;
  }
  function reverseBytes(b) {
    const o = new Uint8Array(b.length);
    for (let i = 0; i < b.length; i++) o[i] = b[b.length - 1 - i];
    return o;
  }
  function shareTarget(diff) {
    diff = Math.max(1, diff | 0);
    let bits = Math.max(0, diff.toString(2).length - 1);
    const t = new Uint8Array(32);
    t.fill(0xff, 0, 28);
    const byteShift = Math.floor(bits / 8);
    const bitShift = bits % 8;
    if (byteShift) {
      t.copyWithin(0, byteShift);
      t.fill(0, 32 - byteShift);
    }
    if (bitShift) {
      for (let i = 0; i < 31; i++) t[i] = ((t[i] >> bitShift) | (t[i + 1] << (8 - bitShift))) & 0xff;
      t[31] >>= bitShift;
    }
    return t;
  }
  function meets(hashLe, target) {
    for (let i = 31; i >= 0; i--) {
      if (hashLe[i] < target[i]) return true;
      if (hashLe[i] > target[i]) return false;
    }
    return true;
  }
  function powHashLe(work) {
    return reverseBytes(blake2b256(work));
  }
  function workRoot(coinb1, extranonce12) {
    const leaf = new Uint8Array(1 + coinb1.length + 12);
    leaf[0] = 0;
    leaf.set(coinb1, 1);
    leaf.set(extranonce12.subarray(0, 12), 1 + coinb1.length);
    return blake2b256(leaf);
  }
  function buildWork(prev, nonce8, ntime8, root) {
    const w = new Uint8Array(80);
    w.set(prev.subarray(0, 32), 0);
    w.set(nonce8.subarray(0, 8), 32);
    w.set(ntime8.subarray(0, 8), 40);
    w.set(root.subarray(0, 32), 48);
    return w;
  }
  function u64le(n) {
    const b = new Uint8Array(8);
    const dv = new DataView(b.buffer);
    dv.setBigUint64(0, BigInt(n) & MASK, true);
    return b;
  }

  const api = {
    blake2b256, hexToBytes, bytesToHex, reverseBytes, shareTarget, meets,
    powHashLe, workRoot, buildWork, u64le,
  };
  if (typeof self !== "undefined") self.Blake2b = api;
  root.Blake2b = api;
})(typeof self !== "undefined" ? self : this);
