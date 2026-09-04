use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lazarus_protocol::coinbaser::{CoinbaserOutput, CoinbaserV2};
use lazarus_protocol::identity_script;
use serde::{Deserialize, Serialize};

/// Credits landing in the same bucket for the same identity are summed into one row, so a
/// busy window is a few thousand rows instead of one row per share. Payout shares are
/// unaffected: bucketing changes only the granularity at which old work leaves the window.
const BUCKET_SECS: u64 = 60;
/// Dedupe keys kept in memory before the set is dropped and rebuilt.
const SEEN_CAP: usize = 400_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Credit {
    ts: u64,
    identity: String,
    work: u64,
}

/// A one-time, owner-authorised make-good: pay `beneficiary` up to `owed_sats`, taken only
/// from `source`'s own coinbase output.
///
/// Denominated in satoshis on purpose. Expressing it as ledger work would not survive the
/// window filling up, because a payout is `value * work / window_work` -- a fixed number of
/// work units is worth steadily less as `window_work` grows, so a make-good sized today pays
/// a fraction of its intended value tomorrow.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bonus {
    pub beneficiary: String,
    pub source: String,
    /// Total satoshis owed. Immutable once armed, so the file doubles as the receipt of intent.
    pub owed_sats: u64,
    /// Satoshis actually moved on-chain by settled blocks so far. `owed_sats - paid_sats` is what
    /// is still owed; the debt retires when this reaches `owed_sats`. Tracking the real amount
    /// paid (rather than clearing on the first block) is what lets one debt span many blocks and
    /// stop exactly when the beneficiary is whole.
    #[serde(default)]
    pub paid_sats: u64,
    #[serde(default)]
    pub note: String,
}

impl Bonus {
    /// Satoshis still owed to this beneficiary.
    pub fn remaining(&self) -> u64 {
        self.owed_sats.saturating_sub(self.paid_sats)
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Persist {
    credits: Vec<Credit>,
    carry: HashMap<String, u64>,
    shares: u64,
    accepted_work: u64,
    /// Legacy single make-good. Still read for backward compatibility and migrated into
    /// `bonuses` on load; never written back (see `save`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bonus: Option<Bonus>,
    /// Outstanding make-goods, each drawing only from its own `source`.
    #[serde(default)]
    bonuses: Vec<Bonus>,
}

#[derive(Default)]
pub struct Ledger {
    credits: Vec<Credit>,
    /// Unpaid effort from a block that used the wrong (unsplit) template.
    /// Not trimmed. Cleared only after a Lazarus block with 2+ value outputs.
    carry: HashMap<String, u64>,
    pub shares: u64,
    pub accepted_work: u64,
    /// Running sum of `credits`, so trimming and reporting never walk the window.
    total: u64,
    seen: HashMap<[u8; 16], ()>,
    last_save: Option<Instant>,
    dirty: bool,
    /// Outstanding make-goods, each applied to every split we issue until a block makes its
    /// beneficiary whole. Multiple beneficiaries are supported; each draws only from its own
    /// `source` output, so no other miner is ever debited.
    pub bonuses: Vec<Bonus>,
}

impl Ledger {
    pub fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    fn bucket_of(ts: u64) -> u64 {
        ts - (ts % BUCKET_SECS)
    }

    /// Merge rows sharing a bucket and identity. Totals and per-identity sums are preserved.
    fn compacted(credits: Vec<Credit>) -> Vec<Credit> {
        let mut out: Vec<Credit> = Vec::new();
        for c in credits {
            let bucket = Self::bucket_of(c.ts);
            let mut merged = false;
            for o in out.iter_mut().rev() {
                if o.ts < bucket {
                    break;
                }
                if o.identity == c.identity {
                    o.work = o.work.saturating_add(c.work);
                    merged = true;
                    break;
                }
            }
            if !merged {
                out.push(Credit { ts: bucket, identity: c.identity, work: c.work });
            }
        }
        out
    }

    pub fn load(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(p) = serde_json::from_str::<Persist>(&raw) else {
            log::warn!("ledger file unreadable; starting empty persist");
            return Self::default();
        };
        let before = p.credits.len();
        let credits = Self::compacted(p.credits);
        let total = credits.iter().map(|c| c.work).sum();
        // Migrate a legacy single `bonus` into the `bonuses` list.
        let mut bonuses = p.bonuses;
        if let Some(b) = p.bonus {
            bonuses.push(b);
        }
        let owed: u64 = bonuses.iter().map(|b| b.remaining()).sum();
        log::info!(
            "ledger loaded credits={} (compacted from {}) carry_ids={} shares={} window_work={} make_goods={} owed_sats={}",
            credits.len(),
            before,
            p.carry.len(),
            p.shares,
            total,
            bonuses.len(),
            owed
        );
        Self {
            credits,
            carry: p.carry,
            shares: p.shares,
            accepted_work: p.accepted_work,
            total,
            seen: HashMap::new(),
            last_save: None,
            dirty: before != 0,
            bonuses,
        }
    }

    pub fn save(&mut self, path: &Path) {
        let p = Persist {
            credits: self.credits.clone(),
            carry: self.carry.clone(),
            shares: self.shares,
            accepted_work: self.accepted_work,
            bonus: None,
            bonuses: self.bonuses.clone(),
        };
        if let Ok(raw) = serde_json::to_string(&p) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, raw).is_ok() && std::fs::rename(&tmp, path).is_ok() {
                self.last_save = Some(Instant::now());
                self.dirty = false;
            }
        }
    }

    /// Write at most once per `min_interval`, and only if something changed. Share crediting
    /// is on the hot path; rewriting the whole window per share is not.
    pub fn save_if_due(&mut self, path: &Path, min_interval: Duration) {
        if !self.dirty {
            return;
        }
        let due = self.last_save.map(|t| t.elapsed() >= min_interval).unwrap_or(true);
        if due {
            self.save(path);
        }
    }

    pub fn merge_carry(&mut self, ident: String, work: u64) {
        if ident.is_empty() || work == 0 {
            return;
        }
        let cur = self.carry.get(&ident).copied().unwrap_or(0);
        self.carry.insert(ident, cur.saturating_add(work));
        self.dirty = true;
    }

    pub fn load_carry_file(&mut self, path: &Path) -> usize {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return 0;
        };
        let Ok(map) = serde_json::from_str::<HashMap<String, u64>>(&raw) else {
            return 0;
        };
        let n = map.len();
        for (ident, work) in map {
            self.merge_carry(ident, work);
        }
        n
    }

    pub fn clear_carry(&mut self) {
        self.carry.clear();
        self.dirty = true;
    }

    pub fn carry_len(&self) -> usize {
        self.carry.len()
    }

    pub fn credits_len(&self) -> usize {
        self.credits.len()
    }

    /// Credit work to an identity. `key` must be unique per submission; a repeat is ignored
    /// and reported as `false` so the caller can reject it.
    pub fn credit(&mut self, identity: String, work: u64, key: [u8; 16]) -> bool {
        if self.seen.contains_key(&key) {
            return false;
        }
        if self.seen.len() >= SEEN_CAP {
            self.seen.clear();
        }
        self.seen.insert(key, ());

        let bucket = Self::bucket_of(Self::now());
        let mut merged = false;
        for c in self.credits.iter_mut().rev() {
            if c.ts < bucket {
                break;
            }
            if c.identity == identity {
                c.work = c.work.saturating_add(work);
                merged = true;
                break;
            }
        }
        if !merged {
            self.credits.push(Credit { ts: bucket, identity, work });
        }
        self.total = self.total.saturating_add(work);
        self.shares += 1;
        self.accepted_work = self.accepted_work.saturating_add(work);
        self.dirty = true;
        true
    }

    pub fn trim(&mut self, target_work: u64) {
        if target_work == 0 {
            return;
        }
        let mut n = 0usize;
        let mut total = self.total;
        while total > target_work && n < self.credits.len() {
            total = total.saturating_sub(self.credits[n].work);
            n += 1;
        }
        if n > 0 {
            self.credits.drain(..n);
            self.total = total;
            self.dirty = true;
        }
    }

    pub fn window_work(&self) -> u64 {
        self.total + self.carry.values().sum::<u64>()
    }

    pub fn tides_work(&self) -> u64 {
        self.total
    }

    pub fn by_identity(&self) -> HashMap<String, u64> {
        let mut m = HashMap::new();
        for c in &self.credits {
            *m.entry(c.identity.clone()).or_insert(0) += c.work;
        }
        for (ident, work) in &self.carry {
            *m.entry(ident.clone()).or_insert(0) += *work;
        }
        m
    }

    /// Window + unpaid carry split the block; fee_bps=0 pays 100% (dust/unpayable remainder to pool script).
    pub fn coinbaser(
        &self,
        value: u64,
        fee_bps: u64,
        min_payout: u64,
        pool_script: &[u8],
        id: u8,
    ) -> CoinbaserV2 {
        self.coinbaser_with_moves(value, fee_bps, min_payout, pool_script, id).0
    }

    /// As [`Self::coinbaser`], but also returns the satoshis each make-good beneficiary was moved
    /// in this split, keyed by beneficiary address. The caller records this against the coinbaser
    /// id so that, if this exact split wins a block, the debt can be settled by the *actual*
    /// amount paid rather than cleared blindly.
    pub fn coinbaser_with_moves(
        &self,
        value: u64,
        fee_bps: u64,
        min_payout: u64,
        pool_script: &[u8],
        id: u8,
    ) -> (CoinbaserV2, Vec<(String, u64)>) {
        let fee = value.saturating_mul(fee_bps) / 10_000;
        let miners = value.saturating_sub(fee);
        let total = self.window_work().max(1);
        let mut outputs = Vec::new();
        let mut paid = 0u64;
        let mut rows: Vec<(String, u64)> = self.by_identity().into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        for (ident, work) in rows {
            let Some(script) = identity_script(&ident) else {
                continue;
            };
            let sats = miners.saturating_mul(work) / total;
            if sats < min_payout {
                continue;
            }
            paid = paid.saturating_add(sats);
            outputs.push(CoinbaserOutput { sats, script });
            if outputs.len() >= 32 {
                break;
            }
        }
        let rest = value.saturating_sub(paid);
        if rest > 0 || outputs.is_empty() {
            outputs.push(CoinbaserOutput {
                sats: rest,
                script: pool_script.to_vec(),
            });
        }
        // Apply each outstanding make-good in turn. They are applied largest-first (the list is
        // stored that way) so a beneficiary is paid off before the next begins when the source's
        // share in one block cannot cover them all; the remainder simply rolls to the next block.
        // Each draws only from its own `source`, so no other miner is ever debited.
        let mut moves: Vec<(String, u64)> = Vec::new();
        for b in self.bonuses.iter().filter(|b| b.remaining() > 0) {
            if let (Some(src), Some(dst)) =
                (identity_script(&b.source), identity_script(&b.beneficiary))
            {
                let moved = apply_bonus(&mut outputs, &src, &dst, b.remaining(), min_payout);
                if moved > 0 {
                    moves.push((b.beneficiary.clone(), moved));
                }
            }
        }
        (CoinbaserV2 { id, outputs }, moves)
    }

    /// Settle make-goods against the moves a confirmed block actually paid, returning the debts
    /// that are now fully retired (for logging/receipts).
    ///
    /// Each move adds to the matching beneficiary's `paid_sats`; decrementing by the *actual*
    /// amount moved (never clearing) is what lets a debt span many blocks and stop exactly when
    /// the beneficiary is whole. A debt whose `paid_sats` reaches `owed_sats` is removed.
    pub fn settle_bonuses(&mut self, moves: &[(String, u64)]) -> Vec<Bonus> {
        if moves.is_empty() {
            return Vec::new();
        }
        for (beneficiary, moved) in moves {
            if let Some(b) = self
                .bonuses
                .iter_mut()
                .find(|b| &b.beneficiary == beneficiary && b.remaining() > 0)
            {
                b.paid_sats = b.paid_sats.saturating_add(*moved).min(b.owed_sats);
                self.dirty = true;
            }
        }
        let mut retired = Vec::new();
        self.bonuses.retain(|b| {
            if b.remaining() == 0 {
                retired.push(b.clone());
                false
            } else {
                true
            }
        });
        if !retired.is_empty() {
            self.dirty = true;
        }
        retired
    }

    /// Total satoshis still owed across all outstanding make-goods.
    pub fn owed_sats(&self) -> u64 {
        self.bonuses.iter().map(|b| b.remaining()).sum()
    }
}

/// Move up to `owed` sats from `src`'s outputs into `dst`'s. Returns the amount moved.
///
/// Two invariants matter more than the amount:
///
/// 1. **Only `src` is debited.** Every other miner's output is byte-identical whether or not a
///    make-good is active, which is the whole point -- the pool eats it, not the miners.
/// 2. **The total is preserved.** Sats are moved between existing outputs, never minted. A
///    coinbase whose outputs do not sum to the template value is invalid, so any block we
///    assembled would be rejected. Sub-dust remainders are folded into `dst` rather than left
///    behind as unspendable outputs, for the same reason.
fn apply_bonus(
    outputs: &mut Vec<CoinbaserOutput>,
    src: &[u8],
    dst: &[u8],
    owed: u64,
    min_payout: u64,
) -> u64 {
    if src == dst {
        return 0;
    }
    let avail: u64 = outputs.iter().filter(|o| o.script == src).map(|o| o.sats).sum();
    let take = owed.min(avail);
    if take == 0 {
        return 0;
    }

    let mut left = take;
    let mut idx: Vec<usize> = (0..outputs.len()).filter(|&i| outputs[i].script == src).collect();
    idx.sort_by_key(|&i| std::cmp::Reverse(outputs[i].sats));
    for i in idx {
        let d = left.min(outputs[i].sats);
        outputs[i].sats -= d;
        left -= d;
        if left == 0 {
            break;
        }
    }

    let mut moved = take;
    let mut i = 0;
    while i < outputs.len() {
        if outputs[i].script == src && outputs[i].sats < min_payout {
            moved += outputs[i].sats;
            outputs.remove(i);
        } else {
            i += 1;
        }
    }

    if let Some(o) = outputs.iter_mut().find(|o| o.script == dst) {
        o.sats = o.sats.saturating_add(moved);
    } else {
        outputs.push(CoinbaserOutput {
            sats: moved,
            script: dst.to_vec(),
        });
    }
    moved
}

#[cfg(test)]
mod tests {
    use super::*;
    use lazarus_protocol::identity_script;

    const A: &str = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
    const B: &str = "bc1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3";

    fn pool() -> Vec<u8> {
        identity_script(A).unwrap()
    }

    fn key(n: u64) -> [u8; 16] {
        let mut k = [0u8; 16];
        k[..8].copy_from_slice(&n.to_le_bytes());
        k
    }

    #[test]
    fn split_uses_carry_and_live() {
        let mut led = Ledger::default();
        led.merge_carry(A.into(), 75);
        assert!(led.credit(B.into(), 25, key(1)));
        let blob = led.coinbaser(1_000_000, 0, 546, &pool(), 1);
        assert!(blob.outputs.len() >= 2);
        let miner_sats: u64 = blob.outputs.iter().map(|o| o.sats).sum();
        assert_eq!(miner_sats, 1_000_000);
    }

    #[test]
    fn trim_does_not_drop_carry() {
        let mut led = Ledger::default();
        led.merge_carry(A.into(), 9_000);
        led.credit(A.into(), 100, key(2));
        led.trim(50);
        assert_eq!(led.tides_work(), 0);
        assert_eq!(led.window_work(), 9_000);
        assert_eq!(led.carry_len(), 1);
    }

    #[test]
    fn repeat_submissions_are_not_paid_twice() {
        let mut led = Ledger::default();
        assert!(led.credit(A.into(), 32, key(7)));
        assert!(!led.credit(A.into(), 32, key(7)));
        assert_eq!(led.tides_work(), 32);
        assert_eq!(led.shares, 1);
    }

    #[test]
    fn many_shares_collapse_into_bucket_rows() {
        let mut led = Ledger::default();
        for i in 0..5_000u64 {
            led.credit(if i % 2 == 0 { A.into() } else { B.into() }, 1, key(i));
        }
        assert_eq!(led.tides_work(), 5_000);
        // two identities in the current bucket, so at most two rows per bucket crossed
        assert!(led.credits_len() <= 4, "rows={}", led.credits_len());
        let by = led.by_identity();
        assert_eq!(by[A], 2_500);
        assert_eq!(by[B], 2_500);
    }

    #[test]
    fn trim_keeps_the_running_total_honest() {
        let mut led = Ledger::default();
        for i in 0..100u64 {
            led.credit(A.into(), 10, key(i));
        }
        assert_eq!(led.tides_work(), 1_000);
        led.trim(400);
        let walked: u64 = led.credits.iter().map(|c| c.work).sum();
        assert_eq!(led.tides_work(), walked);
    }

    #[test]
    fn compaction_preserves_totals() {
        // first three share the 960 bucket; the last one is far enough out to start its own
        let raw = vec![
            Credit { ts: 960, identity: A.into(), work: 5 },
            Credit { ts: 970, identity: B.into(), work: 7 },
            Credit { ts: 980, identity: A.into(), work: 3 },
            Credit { ts: 5_000, identity: A.into(), work: 11 },
        ];
        let before: u64 = raw.iter().map(|c| c.work).sum();
        let out = Ledger::compacted(raw);
        assert_eq!(out.iter().map(|c| c.work).sum::<u64>(), before);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].ts, 960);
        assert_eq!(out[0].work, 8);
        assert_eq!(out[1].work, 7);
        assert_eq!(out[2].ts, 4_980);
    }
}

#[cfg(test)]
mod bonus_tests {
    use super::*;
    use lazarus_protocol::identity_script;

    const SRC: &str = "bc1qt5praystcdle0nq04e3h02yjszha82uzhww85x6972lcy40k4eyqz9jfaq";
    const DST: &str = "bc1qk7zrqfpy4us7wxwsusv0qu4w2w8fx6h7nh3g8l";
    const OTHER: &str = "bc1qva3nw86tp0uzeldqjhvjj44nh2fzfcawq77tp9";

    fn s(a: &str) -> Vec<u8> {
        identity_script(a).unwrap()
    }
    fn out(a: &str, sats: u64) -> CoinbaserOutput {
        CoinbaserOutput { sats, script: s(a) }
    }
    fn total(v: &[CoinbaserOutput]) -> u64 {
        v.iter().map(|o| o.sats).sum()
    }
    fn sats_of(v: &[CoinbaserOutput], a: &str) -> u64 {
        let sc = s(a);
        v.iter().filter(|o| o.script == sc).map(|o| o.sats).sum()
    }

    #[test]
    fn takes_from_the_source_and_leaves_other_miners_untouched() {
        let mut v = vec![
            out(OTHER, 200_000_000),
            out(SRC, 70_000_000),
            out(DST, 17_000_000),
        ];
        let before = total(&v);
        let moved = apply_bonus(&mut v, &s(SRC), &s(DST), 59_000_000, 546);
        assert_eq!(moved, 59_000_000);
        assert_eq!(sats_of(&v, OTHER), 200_000_000, "an uninvolved miner was debited");
        assert_eq!(sats_of(&v, SRC), 11_000_000);
        assert_eq!(sats_of(&v, DST), 76_000_000);
        assert_eq!(total(&v), before, "coinbase total changed");
    }

    #[test]
    fn cannot_take_more_than_the_source_holds() {
        let mut v = vec![out(SRC, 32_000_000), out(DST, 17_000_000)];
        let before = total(&v);
        let moved = apply_bonus(&mut v, &s(SRC), &s(DST), 59_000_000, 546);
        assert_eq!(moved, 32_000_000, "took more than the source had");
        assert_eq!(sats_of(&v, SRC), 0);
        assert_eq!(total(&v), before);
    }

    #[test]
    fn merges_rather_than_duplicating_the_beneficiary_output() {
        let mut v = vec![out(SRC, 70_000_000), out(DST, 1_000)];
        apply_bonus(&mut v, &s(SRC), &s(DST), 5_000, 546);
        let n = v.iter().filter(|o| o.script == s(DST)).count();
        assert_eq!(n, 1, "beneficiary got a second output with the same script");
        assert_eq!(sats_of(&v, DST), 6_000);
    }

    #[test]
    fn folds_a_sub_dust_source_remainder_in_rather_than_stranding_it() {
        let mut v = vec![out(SRC, 59_000_400), out(DST, 1_000)];
        let before = total(&v);
        let moved = apply_bonus(&mut v, &s(SRC), &s(DST), 59_000_000, 546);
        assert_eq!(moved, 59_000_400, "the 400 sat remainder was stranded");
        assert!(!v.iter().any(|o| o.script == s(SRC)), "left an unspendable dust output");
        assert_eq!(total(&v), before);
    }

    const DST2: &str = "bc1q7zvn93g2c374alqhaytreutde930m06hr6u0vh";

    fn bonus(dst: &str, owed: u64) -> Bonus {
        Bonus {
            beneficiary: dst.into(),
            source: SRC.into(),
            owed_sats: owed,
            paid_sats: 0,
            note: String::new(),
        }
    }

    /// The pool address is both a miner and the remainder script in production, so the debt has
    /// to survive the source appearing twice in one split.
    #[test]
    fn issuing_a_split_never_mutates_the_debt() {
        let mut led = Ledger::default();
        assert!(led.credit(SRC.into(), 1_000, [1u8; 16]));
        assert!(led.credit(DST.into(), 1_000, [2u8; 16]));
        led.bonuses = vec![bonus(DST, 59_000_000)];
        for _ in 0..50 {
            let cb = led.coinbaser(313_000_000, 0, 546, &s(SRC), 1);
            assert_eq!(
                cb.outputs.iter().map(|o| o.sats).sum::<u64>(),
                313_000_000,
                "split no longer sums to the template value"
            );
            let paid = cb
                .outputs
                .iter()
                .filter(|o| o.script == s(DST))
                .map(|o| o.sats)
                .sum::<u64>();
            assert!(paid > 59_000_000, "beneficiary did not receive the make-good");
        }
        assert_eq!(led.owed_sats(), 59_000_000, "issuing splits drained the debt without a block");
    }

    /// Multiple make-goods all draw from the same source; other miners stay byte-identical.
    #[test]
    fn multiple_make_goods_only_debit_their_shared_source() {
        let mut led = Ledger::default();
        led.credit(OTHER.into(), 100, [3u8; 16]);
        led.credit(SRC.into(), 100, [4u8; 16]);
        led.bonuses = vec![bonus(DST, 40_000_000), bonus(DST2, 25_000_000)];
        let (cb, moves) = led.coinbaser_with_moves(313_000_000, 0, 546, &s(SRC), 1);
        assert_eq!(cb.outputs.iter().map(|o| o.sats).sum::<u64>(), 313_000_000);
        // OTHER's slice is what the plain split gave it, untouched by either make-good.
        let plain = {
            let mut l2 = Ledger::default();
            l2.credit(OTHER.into(), 100, [3u8; 16]);
            l2.credit(SRC.into(), 100, [4u8; 16]);
            l2.coinbaser(313_000_000, 0, 546, &s(SRC), 1)
        };
        let other_of = |v: &CoinbaserV2| v.outputs.iter().filter(|o| o.script == s(OTHER)).map(|o| o.sats).sum::<u64>();
        assert_eq!(other_of(&cb), other_of(&plain), "an uninvolved miner was debited");
        assert!(sats_of(&cb.outputs, DST) >= 40_000_000);
        assert!(sats_of(&cb.outputs, DST2) >= 25_000_000);
        let m: HashMap<_, _> = moves.into_iter().collect();
        assert_eq!(m.get(DST), Some(&40_000_000));
        assert_eq!(m.get(DST2), Some(&25_000_000));
    }

    /// A debt bigger than the source can cover in one block clears over several, and stops
    /// exactly when the beneficiary is whole (never over- or under-paying).
    #[test]
    fn settle_until_whole_spans_blocks_and_stops_exactly() {
        let mut led = Ledger::default();
        // jfaq earns a modest slice each block; owe far more than one block can move.
        led.credit(SRC.into(), 30, [1u8; 16]);
        led.credit(OTHER.into(), 70, [2u8; 16]);
        led.bonuses = vec![bonus(DST, 250_000_000)];
        let mut safety = 0;
        while led.owed_sats() > 0 {
            let (_cb, moves) = led.coinbaser_with_moves(313_000_000, 0, 546, &s(SRC), 1);
            led.settle_bonuses(&moves);
            safety += 1;
            assert!(safety < 100, "debt failed to converge");
        }
        assert!(safety > 1, "debt should have needed more than one block");
        // Exactly whole: the beneficiary's own bonus recorded exactly what was owed.
        assert!(led.bonuses.is_empty(), "retired debt lingered");
    }

    /// Settlement decrements by the actual amount moved, and retires only fully-paid debts.
    #[test]
    fn settle_records_partial_payment_and_retires_completed() {
        let mut led = Ledger::default();
        led.bonuses = vec![bonus(DST, 100_000_000), bonus(DST2, 5_000)];
        let retired = led.settle_bonuses(&[(DST.into(), 30_000_000), (DST2.into(), 5_000)]);
        assert_eq!(retired.len(), 1, "only the fully-paid debt should retire");
        assert_eq!(retired[0].beneficiary, DST2);
        assert_eq!(led.owed_sats(), 70_000_000, "partial payment not recorded");
        assert_eq!(led.bonuses.len(), 1);
        assert_eq!(led.bonuses[0].paid_sats, 30_000_000);
    }

    /// A legacy single `bonus` on disk is migrated into `bonuses` on load.
    #[test]
    fn legacy_single_bonus_is_migrated_on_load() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("lz-ledger-legacy-{}.json", std::process::id()));
        let raw = format!(
            r#"{{"credits":[],"carry":{{}},"shares":0,"accepted_work":0,"bonus":{{"beneficiary":"{DST}","source":"{SRC}","owed_sats":59000000,"note":"legacy"}}}}"#
        );
        std::fs::write(&path, raw).unwrap();
        let led = Ledger::load(&path);
        std::fs::remove_file(&path).ok();
        assert_eq!(led.bonuses.len(), 1);
        assert_eq!(led.bonuses[0].owed_sats, 59_000_000);
        assert_eq!(led.bonuses[0].paid_sats, 0);
        assert_eq!(led.owed_sats(), 59_000_000);
    }
}
