/* Lazarus browser miner worker — hashes 80-byte BLAKE2b work. */
importScripts("/static/blake2b.js");

const B = self.Blake2b;
let running = false;
let job = null;

function writeNonce(work, n) {
  const dv = new DataView(work.buffer, work.byteOffset + 32, 8);
  dv.setBigUint64(0, BigInt(n) & 0xffffffffffffffffn, true);
}

self.onmessage = function (ev) {
  const m = ev.data || {};
  if (m.type === "stop") {
    running = false;
    job = null;
    return;
  }
  if (m.type === "job") {
    job = m.job;
    running = true;
    mine();
  }
};

function mine() {
  const j = job;
  if (!j) return;
  const prev = B.hexToBytes(j.prev);
  const ntime = B.hexToBytes(j.ntime);
  const root = B.hexToBytes(j.wroot);
  const target = B.hexToBytes(j.targetHex);
  const work = B.buildWork(prev, new Uint8Array(8), ntime, root);
  let nonce = BigInt(j.base || 0);
  const step = BigInt(j.step || 1);
  let n = 0;
  const BATCH = 256;
  function tick() {
    if (!running || job !== j) return;
    for (let i = 0; i < BATCH; i++) {
      writeNonce(work, nonce);
      const h = B.powHashLe(work);
      if (B.meets(h, target)) {
        self.postMessage({
          type: "found",
          jobId: j.jobId,
          en2: j.en2,
          ntime: j.ntime,
          nonce: B.bytesToHex(work.subarray(32, 40)),
        });
      }
      nonce += step;
      n++;
    }
    self.postMessage({ type: "progress", hashes: BATCH });
    setTimeout(tick, 0);
  }
  tick();
}
