// Payout audit tools: what a miner was paid, what is maturing and why, what the pool owes them,
// an on-chain check of any of it, and a knowledge base for everything else.
//
// All read-only, all through `upstream`. Every tool declares `fanout`, the most upstream reads one
// call may make (enforced in upstream.js), and the per-address ones declare `cost` in heavy units.
// Heights are exact; dates are estimates from the recent average block interval and say so.

import { payoutAddress } from "./address.js";
import { FAQ, FAQ_VERSION, searchFaq } from "./faq.js";
import { PublicError, pool, chain, xbt, sats, pct, iso, label, NOTE_LABELS, POOL_SITE, EXPLORER, HEX64, int, addressArg } from "./upstream.js";

// Knots #419 (in 29.4.2). Consensus: coinbases mined 973,440..979,919 need 6,480 confirmations
// until the rule releases at 979,920. Relay policy: 29.4.2 applies 6,480 to every coinbase spend.
const LONG_START = 973440, LONG_DEPTH = 6480;
const FIRST_BLAKE2B = 961640; // no Lazarus block can be older than the PoW change
const MG_STATUSES = new Set(["queued", "paid", "failed"]);

const MATURITY_WHY =
  "Coinbase outputs normally spend after 100 confirmations. Bitcoin Knots #419 (in 29.4.2) makes coinbases mined from block 973,440 need 6,480 confirmations under consensus until the rule releases at block 979,920, " +
  "and 29.4.2's relay policy applies the same 6,480 to every coinbase spend, older coinbases included, so nodes will not relay a spend before then. The pool uses the later of the two, so its unlock heights are the conservative ones.";
const ESTIMATE_NOTE = "Dates are estimates (recent average block interval); heights are exact.";

/** The miner record, or a clear 'unknown' answer. */
async function minerRecord(ctx, address) {
  const m = await pool(ctx, "/api/miner/" + address, 10);
  return m && m.known ? m : null;
}
const unknown = (address) => ({ address, known: false, note: "The pool has never seen a share for this address. Check the stratum username: it must be the payout address, optionally followed by .workername." });

/** Seconds per block for date estimates: the pool's recent average, or the 600 s target. */
async function blockInterval(ctx) {
  try {
    const p = await pool(ctx, "/api/pool?h=0", 8), s = Number(p.block_interval_seconds);
    return s >= 60 && s <= 3600 ? s : 600;
  } catch (e) {
    return 600;
  }
}
const estDate = (blocks, interval) => iso(Math.round(Date.now() / 1000 + Math.max(0, blocks) * interval));
const tipOf = (m) => Number(m.tip_height) || 0;
const hex = (t) => (typeof t === "string" && HEX64.test(t) ? t.toLowerCase() : null);

/** The height from which a coinbase mined at `h` can be spent and relayed. Consensus never asks for more than
 *  6,480 and the 29.4.2 relay policy always asks for 6,480, so the later of the two is always h + 6,480. */
const unlockHeight = (h) => h + LONG_DEPTH;

/** One of the address's coinbase payouts, with its unlock height from both the pool's figure and the rule. */
function coinbaseRow(b, tip, interval) {
  const h = Number(b.height), confs = Number(b.confirmations) || Math.max(0, tip - h + 1);
  const poolUnlock = Number(b.blocks_to_mature) > 0 ? tip + Number(b.blocks_to_mature) : null;
  const unlock = Math.max(unlockHeight(h), poolUnlock || 0), left = Math.max(0, unlock - tip);
  const consensusMature = b.status !== "immature";
  return {
    height: h, block_hash: hex(b.hash), paid_xbt: xbt(b.miner_btc), confirmations: confs, status: label(b.status),
    unlock_height: unlock, blocks_remaining: left, estimated_unlock: left ? estDate(left, interval) : "now",
    rule: h >= LONG_START ? "Knots #419 consensus, 6,480 confirmations" : consensusMature && left ? "consensus-mature (100 confirmations), but Knots 29.4.2 nodes will not relay a spend until 6,480" : "100 confirmations",
  };
}

/** A make-good as a miner should see it. Paid ones never show a future payable height (XBT-075). */
function makegoodRow(g, tip, interval) {
  const status = MG_STATUSES.has(g.status) ? g.status : label(g.status) || "unknown";
  const row = { block_height: g.height, kind: g.kind === "pool-only" ? "pool-only" : g.kind === "partial" ? "partial" : label(g.kind), owed_sats: sats(g.sats ?? g.owed_sats), owed_xbt: xbt((g.sats ?? g.owed_sats ?? 0) / 1e8), status, txid: hex(g.txid) };
  const why = g.kind === "pool-only"
    ? "The block was found on a job whose coinbase paid only the pool, so the whole window is repaid by this make-good."
    : "The block's coinbase paid only part of the window (it filled up or a gateway used an early job), so the addresses left out are repaid by this make-good.";
  if (status === "paid") {
    return { ...row, confirmations: Number(g.confirmations) || null, sent_at: typeof g.sent_at === "string" ? label(g.sent_at) : null, why, note: "Broadcast and confirmed. A make-good is an ordinary transaction, so once confirmed it is spendable; check it with verify_payout." };
  }
  if (status === "queued") {
    const at = Number(g.payable_at) || unlockHeight(Number(g.height)), left = Math.max(0, at - tip);
    const relay = Number(g.height) < LONG_START ? " Consensus would allow this pre-973,440 coinbase after 100 confirmations, but Knots 29.4.2 nodes will not relay the spend until 6,480." : "";
    return { ...row, payable_at_height: at, blocks_remaining: left, estimated_payable: left ? estDate(left, interval) : "due now", why,
      reason_waiting: `It spends the pool's output in block ${g.height}'s own coinbase, which the chain accepts from height ${at} (the block height + 6,480).${relay} It is already signed (the txid is fixed) and is broadcast automatically at that height.` };
  }
  if (status === "failed") return { ...row, why, note: "The broadcast failed. The pool records it for follow-up; ask in the pool's Discord with this block height." };
  return { ...row, why };
}

export const AUDIT_TOOLS = [
  {
    name: "miner_audit",
    title: "Full payout audit for one address: paid, maturing, carried, owed make-goods, bonus, and why each delay exists",
    description:
      "One structured audit of a payout address on Lazarus Pool: total paid, coinbase payouts still maturing (with the next unlock heights and estimated dates), carry and why it is carried, make-goods queued / paid / failed with totals and the next payable height, " +
      "DATUM bonus earned, fee path, TIDES window share, the minimum output, estimated earnings, and a plain-language explanation of every delay (the Knots #419 maturity window and 29.4.2's relay policy on every coinbase spend). " +
      "Start here for 'audit my payouts' or 'where is my money'. Estimates are labelled as estimates. Counts as 2 per-address lookups.",
    heavy: true, cost: 2, fanout: 3,
    inputSchema: { type: "object", properties: { address: addressArg }, required: ["address"], additionalProperties: false },
    parse: async (a) => ({ address: await payoutAddress(a.address) }),
    async run({ address }, ctx) {
      const [m, interval, cb] = await Promise.all([minerRecord(ctx, address), blockInterval(ctx), pool(ctx, "/api/coinbaser", 8).catch(() => null)]);
      if (!m) return unknown(address);
      const tip = tipOf(m), blocks = (m.blocks_found || []).slice(0, 50).map((b) => coinbaseRow(b, tip, interval));
      const locked = blocks.filter((b) => b.blocks_remaining > 0).sort((a, b) => a.unlock_height - b.unlock_height);
      const relayOnly = locked.filter((b) => b.status !== "immature");
      const mgs = (m.makegoods || []).slice(0, 200).map((g) => makegoodRow(g, tip, interval));
      const byStatus = (s) => mgs.filter((g) => g.status === s);
      const queued = byStatus("queued").sort((a, b) => a.payable_at_height - b.payable_at_height), paid = byStatus("paid"), failed = byStatus("failed");
      const sum = (rows) => xbt(rows.reduce((s, g) => s + g.owed_sats, 0) / 1e8);
      const minOut = xbt(m.min_payout_btc), carry = xbt(m.carry_btc);
      const row = cb && (cb.miners || []).find((x) => x.address === address), un = cb && (cb.unpaid || []).find((x) => x.address === address);
      const nextCoinbase = row ? { in_coinbase: true, sats: sats(row.sats), note: "This address has an output in the coinbase being built right now; any carry it is owed rides on it." }
        : un ? { in_coinbase: false, reason: label(un.reason), carried_sats: sats(un.carry_sats), note: "No output in the coinbase being built right now; the value is carried to a later block." }
        : cb ? { in_coinbase: false, note: "No work in the current window." } : null;
      const path = m.fee_path === "datum" ? "own DATUM gateway" : "public stratum";

      const delays = [];
      if (locked.length) delays.push(`${locked.length} recent coinbase payout(s) are on chain but locked until the maturity height (the next at ${locked[0].unlock_height}). ${MATURITY_WHY}`);
      if (relayOnly.length) delays.push(`${relayOnly.length} of those are mature under consensus (pre-973,440, 100 confirmations) but still under 6,480 confirmations, so Knots 29.4.2 nodes will not relay a spend yet; a 100-confirmation wallet may show them as spendable.`);
      if (queued.length) delays.push(`${queued.length} make-good(s) (${sum(queued)} XBT) are queued: each spends the pool's output of its own block's coinbase, so it waits for that coinbase to mature (block height + 6,480) and is then broadcast automatically; the next is payable at ${queued[0].payable_at_height}.`);
      if (carry > 0) delays.push(`${carry} XBT is carried: earned, but not yet large enough for its own coinbase output (minimum ${minOut} XBT) or not placed in the last coinbase; it is paid on top of a later output once share plus carry clears the minimum. The DATUM bonus is credited as carry too.`);
      if (failed.length) delays.push(`${failed.length} make-good(s) failed to broadcast; ask in the pool's Discord with the block heights.`);
      if (!delays.length) delays.push("Nothing is delayed right now.");

      return {
        address, known: true, as_of_height: tip, as_of: iso(m.last_seen) || null,
        summary: {
          paid_xbt: xbt(m.paid_btc), unpaid_xbt: xbt(m.unpaid_btc), maturing_xbt: xbt(m.immature_btc), maturing_blocks: m.immature_blocks, carried_xbt: carry,
          make_goods_queued_xbt: xbt(m.makegood_pending_btc), make_goods_paid_xbt: xbt(m.makegood_paid_btc), datum_bonus_earned_xbt: xbt(m.rebate_btc),
          note: "Pool figures. paid = matured coinbase outputs; maturing = outputs on chain inside the maturity wait; carried = earned, waiting for an output; make-goods = owed from partial / pool-only blocks.",
        },
        immature: { total_xbt: xbt(m.immature_btc), blocks: m.immature_blocks, listed_blocks_still_locked: locked.length, locked_listed_xbt: xbt(locked.reduce((s, b) => s + b.paid_xbt, 0)),
          next_unlocks: locked.slice(0, 5).map(({ height, paid_xbt, unlock_height, blocks_remaining, estimated_unlock }) => ({ height, paid_xbt, unlock_height, blocks_remaining, estimated_unlock })),
          last_listed_unlock_height: locked.length ? locked[locked.length - 1].unlock_height : null, note: `total_xbt is the pool's consensus-immature figure; locked_listed_xbt also counts consensus-mature outputs still under the 6,480-confirmation relay depth. The pool lists the address's latest 50 blocks; miner_immature has them one by one. ${ESTIMATE_NOTE}` },
        carry: { carried_xbt: carry, min_coinbase_output_xbt: minOut, next_coinbase: nextCoinbase },
        make_goods: {
          queued: { count: queued.length, xbt: sum(queued), next_payable_height: queued[0]?.payable_at_height ?? null, next_estimated: queued[0]?.estimated_payable ?? null, pool_figure_xbt: xbt(m.makegood_pending_btc), pool_figure_blocks: m.makegood_pending_blocks },
          paid: { count: paid.length, xbt: sum(paid), pool_figure_xbt: xbt(m.makegood_paid_btc) },
          failed: { count: failed.length, pool_figure_blocks: m.makegood_failed_blocks ?? 0 },
          note: "miner_makegoods lists each one; paid ones show confirmations, never a future payable height.",
        },
        path: { path, gateway_name: label(m.gateway_name) || null, fee_percent: pct(m.fee_percent_path ?? m.est_fee_percent, 2), datum_bonus_percent_points: m.fee_path === "datum" ? pct(m.datum_rebate_percent, 2) : 0 },
        window: { share_percent: pct(m.window_percent, 4), next_block_pays_xbt: xbt(m.block_payout_btc), figure_is_exact: !!m.next_block_exact, window_fill_percent: pct(m.window_fill_percent, 1) },
        estimate: { xbt_per_day: xbt(m.est_btc_day), xbt_per_week: xbt(m.est_btc_week), note: "Estimate at current hashrate, pool luck and network difficulty; actual payouts vary block to block." },
        delays_explained: delays,
        verify: "Every amount is on chain: verify_payout(address, height) checks a block's coinbase output and any make-good for it against the explorer.",
        page: `${POOL_SITE}/miner/${address}`, labels_note: NOTE_LABELS,
      };
    },
  },
  {
    name: "miner_immature",
    title: "Coinbase payouts still locked for an address: height, amount, confirmations, unlock height and estimated date",
    description:
      "Each recent coinbase payout to the address that cannot be spent yet: block height, amount, confirmations, the unlock height (the later of the Knots #419 consensus rule and 29.4.2's relay policy) and an estimated unlock date. " +
      "Soonest first. Includes pre-973,440 payouts that are mature under consensus but that 29.4.2 nodes will not relay yet.",
    heavy: true, fanout: 2,
    inputSchema: { type: "object", properties: { address: addressArg, limit: { type: "integer", minimum: 1, maximum: 50, description: "Most payouts to list, soonest unlock first (default 20)" } }, required: ["address"], additionalProperties: false },
    parse: async (a) => ({ address: await payoutAddress(a.address), limit: int(a.limit, "limit", 1, 50, 20) }),
    async run({ address, limit }, ctx) {
      const [m, interval] = await Promise.all([minerRecord(ctx, address), blockInterval(ctx)]);
      if (!m) return unknown(address);
      const tip = tipOf(m), all = (m.blocks_found || []).slice(0, 50).map((b) => coinbaseRow(b, tip, interval));
      const locked = all.filter((b) => b.blocks_remaining > 0).sort((a, b) => a.unlock_height - b.unlock_height);
      return { address, tip_height: tip, seconds_per_block_used: Math.round(interval), locked_listed: locked.length, locked_listed_xbt: xbt(locked.reduce((s, b) => s + b.paid_xbt, 0)),
        pool_immature_total_xbt: xbt(m.immature_btc), pool_immature_blocks: m.immature_blocks, listed: locked.slice(0, limit).map((b) => ({ ...b, explorer: b.block_hash ? `${EXPLORER}/block/${b.block_hash}` : null })),
        why: MATURITY_WHY, note: `The pool lists the latest 50 blocks that paid the address. ${ESTIMATE_NOTE}` };
    },
  },
  {
    name: "miner_makegoods",
    title: "Every make-good for an address: kind, amount, status, txid, and for queued ones the payable height, blocks left and why",
    description:
      "Make-goods are what the pool pays separately when a found block's coinbase paid only part of the window (partial) or only the pool (pool-only). For each: block height, kind, sats, status (queued / paid / failed), txid; " +
      "for queued ones the payable height, blocks remaining, estimated date and the reason it waits; for paid ones the confirmations (never a future payable height).",
    heavy: true, fanout: 2,
    inputSchema: { type: "object", properties: { address: addressArg, status: { type: "string", enum: ["all", "queued", "paid", "failed"], description: "Default all" }, limit: { type: "integer", minimum: 1, maximum: 100, description: "Most to list, newest block first (default 50)" } }, required: ["address"], additionalProperties: false },
    parse: async (a) => {
      const status = a.status ?? "all";
      if (!["all", "queued", "paid", "failed"].includes(status)) throw new Error("status must be all, queued, paid or failed");
      return { address: await payoutAddress(a.address), status, limit: int(a.limit, "limit", 1, 100, 50) };
    },
    async run({ address, status, limit }, ctx) {
      const [m, interval] = await Promise.all([minerRecord(ctx, address), blockInterval(ctx)]);
      if (!m) return unknown(address);
      const tip = tipOf(m), rows = (m.makegoods || []).slice(0, 200).map((g) => makegoodRow(g, tip, interval)).sort((a, b) => b.block_height - a.block_height);
      const pick = status === "all" ? rows : rows.filter((g) => g.status === status);
      const total = (s) => { const r = rows.filter((g) => g.status === s); return { count: r.length, xbt: xbt(r.reduce((t, g) => t + g.owed_sats, 0) / 1e8) }; };
      return { address, tip_height: tip, totals: { queued: total("queued"), paid: total("paid"), failed: total("failed") }, next_payable_height: m.makegood_next_payable_at ?? null,
        listed: pick.length > limit ? limit : pick.length, make_goods: pick.slice(0, limit),
        what_is_a_make_good: "A make-good is signed when the block is found, spending that block's own pool output to the addresses its coinbase left out; so it is self-funded, its txid is fixed in advance, and it can only be broadcast once that coinbase matures.",
        note: ESTIMATE_NOTE };
    },
  },
  {
    name: "verify_payout",
    title: "Don't trust, verify: check on chain that a block's coinbase (and any make-good for it) paid an address what the pool reports",
    description:
      "Independently checks, through the block explorer, that the coinbase of the block at `height` pays `address` the amount the pool reports, and, if the pool recorded a make-good for that block, that its transaction pays the address and how many confirmations it has. " +
      "Returns both figures side by side and a verdict. Counts as 2 per-address lookups.",
    heavy: true, cost: 2, fanout: 6,
    inputSchema: { type: "object", properties: { address: addressArg, height: { type: "integer", minimum: FIRST_BLAKE2B, maximum: 9999999, description: "Height of a block the pool found" } }, required: ["address", "height"], additionalProperties: false },
    parse: async (a) => {
      const height = int(a.height, "height", FIRST_BLAKE2B, 9999999, null);
      if (height === null) throw new Error("height is required");
      return { address: await payoutAddress(a.address), height };
    },
    async run({ address, height }, ctx) {
      const [m, tipText] = await Promise.all([pool(ctx, "/api/miner/" + address, 10), chain(ctx, "/api/blocks/tip/height", 10, { text: true })]);
      const tip = Number(String(tipText).trim());
      if (!Number.isInteger(tip)) throw new PublicError("the explorer did not return a chain tip; try again shortly");
      if (height > tip) return { address, height, verdict: "not mined yet", chain_tip: tip };
      const reported = m && m.known ? (m.blocks_found || []).find((b) => Number(b.height) === height) : null;
      const mgs = m && m.known ? (m.makegoods || []).filter((g) => Number(g.height) === height) : [];

      const hash = String(await chain(ctx, "/api/block-height/" + height, 3600, { text: true })).trim().toLowerCase();
      if (!HEX64.test(hash)) throw new PublicError("the explorer did not return a block hash for that height");
      const txids = await chain(ctx, `/api/block/${hash}/txids`, 600);
      const cbid = hex(Array.isArray(txids) ? txids[0] : null);
      if (!cbid) throw new PublicError("the explorer did not return that block's transactions");
      const cb = await chain(ctx, "/api/tx/" + cbid, 600);
      if (!cb?.vin?.[0]?.is_coinbase) throw new PublicError("the explorer's first transaction for that block is not a coinbase");
      const onchainSats = (cb.vout || []).filter((o) => o.scriptpubkey_address === address).reduce((s, o) => s + (Number(o.value) || 0), 0);
      const reportedSats = reported ? sats(Number(reported.miner_btc) * 1e8) : null;
      const sameBlock = reported ? hex(reported.hash) === hash : null;

      const coinbase = { block_hash: hash, coinbase_txid: cbid, confirmations: tip - height + 1, outputs: (cb.vout || []).length, onchain_sats_to_address: onchainSats, pool_reported_sats: reportedSats,
        match: reported ? onchainSats === reportedSats && sameBlock : null, explorer: `${EXPLORER}/tx/${cbid}` };
      if (reported && !sameBlock) coinbase.note = "The pool's record names a different block hash for this height: the pool's block may have been replaced in a reorganisation.";
      else if (!reported && !onchainSats && mgs.length) coinbase.note = "The coinbase left this address out (a partial or pool-only block); the pool owes it the make-good checked below.";
      else if (!reported) coinbase.note = onchainSats ? "The pool's recent list (latest 50 blocks for this address) does not include this height, but the coinbase does pay the address." : "The pool's recent list does not include this height and the coinbase pays this address nothing.";

      const make_goods = [];
      for (const g of mgs.slice(0, 1)) {
        const txid = hex(g.txid), owed = sats(g.sats ?? g.owed_sats), st = MG_STATUSES.has(g.status) ? g.status : label(g.status);
        if (!txid) { make_goods.push({ status: st, owed_sats: owed, check: "no txid recorded" }); continue; }
        let tx = null;
        try { tx = await chain(ctx, "/api/tx/" + txid, 60); } catch (e) { if (e.message !== "not found") throw e; }
        if (!tx) {
          make_goods.push({ txid, status: st, owed_sats: owed, onchain: "not broadcast",
            check: st === "queued" ? `As expected: it is queued until height ${Number(g.payable_at) || unlockHeight(height)} and the network will not accept it before then.` : "The pool marks it paid but the explorer does not know the transaction: report this in the pool's Discord." });
          continue;
        }
        const paidSats = (tx.vout || []).filter((o) => o.scriptpubkey_address === address).reduce((s, o) => s + (Number(o.value) || 0), 0), stx = tx.status || {};
        make_goods.push({ txid, status: st, owed_sats: owed, onchain_sats_to_address: paidSats, confirmed: !!stx.confirmed, block_height: stx.block_height ?? null,
          confirmations: stx.confirmed ? tip - stx.block_height + 1 : 0, match: paidSats === owed, explorer: `${EXPLORER}/tx/${txid}` });
      }

      // A queued make-good that is not on chain yet is what should be; a "paid" one missing is a mismatch.
      const checks = [coinbase.match, ...make_goods.map((g) => (g.onchain === "not broadcast" ? g.status === "queued" : g.match))].filter((x) => typeof x === "boolean");
      const verdict = !m?.known ? (onchainSats ? "pays the address on chain; the pool has no record of the address" : "the pool has no record of this address")
        : !checks.length ? (onchainSats ? "pays the address on chain; no pool figure to compare" : "no payout to this address in this block")
        : checks.every(Boolean) ? "match: the chain agrees with the pool" : "mismatch: the chain and the pool disagree (see details)";
      return { address, height, chain_tip: tip, verdict, coinbase, make_goods,
        note: "All figures on the 'onchain' side come from the block explorer, not from the pool. A coinbase output can be spent once it matures; a confirmed make-good is spendable straight away." };
    },
  },
  {
    name: "lazarus_faq",
    title: "Ask anything about mining on Lazarus: fees, DATUM vs stratum, connecting, TIDES and carry, maturity, make-goods, payouts, the cap, solo, wallets",
    description:
      "A curated, versioned knowledge base of the pool's own answers. Pass a topic id (e.g. 'coinbase-maturity', 'makegoods', 'fees') or a question in plain words; returns the best-matching sections with links. " +
      "Pass 'topics' for the list. For numbers about a specific address use miner_audit; for live pool numbers use pool_status.",
    fanout: 0,
    inputSchema: { type: "object", properties: { topic_or_question: { type: "string", minLength: 1, maxLength: 300, description: "A topic id or a question, e.g. 'why are my payouts locked until November?'" } }, required: ["topic_or_question"], additionalProperties: false },
    parse: (a) => {
      const q = a.topic_or_question;
      if (typeof q !== "string" || !q.trim() || q.length > 300) throw new Error("topic_or_question must be 1 to 300 characters");
      return { q: q.replace(/[^\x20-\x7E -￿]/g, " ").trim() };
    },
    async run({ q }) {
      const topics = FAQ.map((f) => ({ id: f.id, title: f.title }));
      if (/^(topics|list|help|index|\?)$/i.test(q)) return { version: FAQ_VERSION, topics };
      const hits = searchFaq(q);
      return { version: FAQ_VERSION, matches: hits.map(({ id, title, answer, links }) => ({ id, title, answer, links })),
        weak_match: !hits.length || hits[0].score < 4, topics: !hits.length || hits[0].score < 4 ? topics : undefined,
        note: "Answers are the pool's own public explanations as of the version date; figures that change (fees, bonus, minimum output) are marked with their date. For an address's own numbers use miner_audit." };
    },
  },
];
