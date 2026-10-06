//! Grace for house-stratum work.
//!
//! The public stratum fee can be set high enough that the endpoint is a donation rather than
//! a place to mine. A miner whose own gateway has just gone down and whose machines fell back
//! to the public stratum should not pay that: for a while after an address *starts* on the
//! house stratum its stratum work is tagged [`SOURCE_STRATUM_GRACE`](crate::SOURCE_STRATUM_GRACE)
//! and charged the grace fee instead of the stratum fee.
//!
//! * The clock runs from the address's first house-stratum share, for `secs`.
//! * An address that has had DATUM work credited here (its own gateway, on this pool) within
//!   [`DATUM_LOOKBACK`] of that start gets `datum_secs` instead.
//! * An address that stays off the house stratum for `rearm_secs` starts a new clock when it
//!   comes back. 0: one clock per address, ever.
//!
//! The tag is decided when the share is credited and stored with the row, so work keeps the
//! class it was done under for as long as it stays in the window.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How long before a stratum start DATUM work still marks the address as a DATUM user.
pub const DATUM_LOOKBACK: u32 = 30 * 86_400;

/// Stored times are only moved forward in steps of this many seconds, so a book that is
/// touched by every share is not rewritten by every share.
const COARSE: u32 = 60;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GraceParams {
    /// Grace after an address's first house-stratum share, seconds. 0: no grace at all.
    pub secs: u32,
    /// The same for an address confirmed on DATUM. Never shorter than `secs`.
    pub datum_secs: u32,
    /// Time off the house stratum after which the next share starts a new clock. 0: never.
    pub rearm_secs: u32,
    /// Unix time the clock is taken to have started for addresses already on the house
    /// stratum when the book is first built (see [`GraceBook::seed`]). 0: their oldest row.
    pub epoch: u32,
}

impl GraceParams {
    pub fn enabled(&self) -> bool {
        self.secs > 0 || self.datum_secs > 0
    }
}

/// One run of house-stratum mining by an address.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Episode {
    /// When the clock started.
    pub start: u32,
    /// The latest house-stratum share (to within [`COARSE`]).
    pub last: u32,
}

/// Who started on the house stratum when, and who has been seen on DATUM. Not money: losing
/// it restarts clocks, it does not change what anyone has already been paid.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraceBook {
    #[serde(default)]
    pub stratum: BTreeMap<String, Episode>,
    /// Latest DATUM work per identity (to within [`COARSE`]).
    #[serde(default)]
    pub datum_seen: BTreeMap<String, u32>,
    #[serde(skip)]
    dirty: bool,
}

impl GraceBook {
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    /// DATUM work was credited to `identity` at `ts`.
    pub fn note_datum(&mut self, identity: &str, ts: u32) {
        match self.datum_seen.get_mut(identity) {
            Some(seen) => {
                if ts >= seen.saturating_add(COARSE) {
                    *seen = ts;
                    self.dirty = true;
                }
            }
            None => {
                self.datum_seen.insert(identity.to_owned(), ts);
                self.dirty = true;
            }
        }
    }

    /// House-stratum work is being credited to `identity` at `ts`: is it inside the grace?
    pub fn note_stratum(&mut self, identity: &str, ts: u32, p: &GraceParams) -> bool {
        match self.stratum.get_mut(identity) {
            Some(e) => {
                if p.rearm_secs > 0 && ts >= e.last.saturating_add(p.rearm_secs) {
                    *e = Episode { start: ts, last: ts };
                    self.dirty = true;
                } else if ts >= e.last.saturating_add(COARSE) {
                    e.last = ts;
                    self.dirty = true;
                }
            }
            None => {
                self.stratum.insert(identity.to_owned(), Episode { start: ts, last: ts });
                self.dirty = true;
            }
        }
        self.until(identity, p).is_some_and(|until| ts < until)
    }

    /// Whether `identity` counts as a DATUM user for a clock that started at `start`.
    fn on_datum(&self, identity: &str, start: u32) -> bool {
        self.datum_seen.get(identity).is_some_and(|&seen| seen.saturating_add(DATUM_LOOKBACK) >= start)
    }

    /// When the grace of `identity`'s current clock ends. `None`: it has never been on the
    /// house stratum.
    pub fn until(&self, identity: &str, p: &GraceParams) -> Option<u32> {
        let e = self.stratum.get(identity)?;
        let len = if self.on_datum(identity, e.start) { p.datum_secs.max(p.secs) } else { p.secs };
        Some(e.start.saturating_add(len))
    }

    /// Build the book from the window's rows, the first time grace is switched on:
    /// `(ts, identity, is house stratum, is DATUM)` per row. An address with house-stratum work
    /// in the window was already mining there, so its clock is taken to have started at
    /// `p.epoch` (or at its oldest row when no epoch is given), not now.
    pub fn seed<'a>(&mut self, rows: impl Iterator<Item = (u32, &'a str, bool, bool)>, p: &GraceParams) {
        for (ts, identity, stratum, datum) in rows {
            if stratum {
                let e = self.stratum.entry(identity.to_owned()).or_insert(Episode { start: ts, last: ts });
                e.start = e.start.min(ts);
                e.last = e.last.max(ts);
            } else if datum {
                let seen = self.datum_seen.entry(identity.to_owned()).or_insert(ts);
                *seen = (*seen).max(ts);
            }
        }
        if p.epoch > 0 {
            for e in self.stratum.values_mut() {
                e.start = p.epoch;
                e.last = e.last.max(p.epoch);
            }
        }
        self.dirty = true;
    }

    /// Forget what can no longer change an answer: DATUM sightings too old to confirm
    /// anyone, and (when clocks re-arm) episodes whose owner would start a new one anyway.
    pub fn prune(&mut self, now: u32, p: &GraceParams) {
        let before = self.stratum.len() + self.datum_seen.len();
        let longest = p.datum_secs.max(p.secs);
        if p.rearm_secs > 0 {
            self.stratum.retain(|_, e| now < e.last.saturating_add(p.rearm_secs));
        }
        // a sighting confirms a start up to DATUM_LOOKBACK after it, and that start's grace
        // is read for as long as its episode is kept
        let keep = DATUM_LOOKBACK.saturating_add(longest).saturating_add(p.rearm_secs);
        let stratum = &self.stratum;
        self.datum_seen.retain(|id, seen| stratum.contains_key(id) || now < seen.saturating_add(keep));
        if self.stratum.len() + self.datum_seen.len() != before {
            self.dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u32 = 3_600;
    const T0: u32 = 1_800_000_000;

    fn p() -> GraceParams {
        GraceParams { secs: 24 * H, datum_secs: 96 * H, rearm_secs: 0, epoch: 0 }
    }

    #[test]
    fn a_new_address_has_24_hours_from_its_first_share() {
        let mut b = GraceBook::default();
        assert!(b.note_stratum("a", T0, &p()));
        assert!(b.note_stratum("a", T0 + 24 * H - 1, &p()));
        assert!(!b.note_stratum("a", T0 + 24 * H, &p()), "the 24th hour is over");
        assert!(!b.note_stratum("a", T0 + 400 * H, &p()));
        assert_eq!(b.until("a", &p()), Some(T0 + 24 * H));
    }

    #[test]
    fn an_address_seen_on_datum_has_96_hours() {
        let mut b = GraceBook::default();
        b.note_datum("a", T0 - 5 * H);
        assert!(b.note_stratum("a", T0, &p()));
        assert!(b.note_stratum("a", T0 + 96 * H - 1, &p()));
        assert!(!b.note_stratum("a", T0 + 96 * H, &p()));
        // one that has never been on DATUM is not helped by someone else who has
        assert!(b.note_stratum("b", T0 + 96 * H, &p()));
        assert_eq!(b.until("b", &p()), Some(T0 + 120 * H));
    }

    #[test]
    fn datum_work_during_the_grace_extends_it_and_old_datum_work_does_not_count() {
        let mut b = GraceBook::default();
        assert!(b.note_stratum("a", T0, &p()));
        assert_eq!(b.until("a", &p()), Some(T0 + 24 * H));
        b.note_datum("a", T0 + 30 * H);
        assert_eq!(b.until("a", &p()), Some(T0 + 96 * H), "its gateway came up: it is a DATUM user");
        assert!(b.note_stratum("a", T0 + 40 * H, &p()));

        let mut b = GraceBook::default();
        b.note_datum("old", T0 - DATUM_LOOKBACK - 1);
        assert!(b.note_stratum("old", T0, &p()));
        assert_eq!(b.until("old", &p()), Some(T0 + 24 * H));
    }

    #[test]
    fn without_rearm_there_is_one_clock_per_address() {
        let mut b = GraceBook::default();
        assert!(b.note_stratum("a", T0, &p()));
        assert!(!b.note_stratum("a", T0 + 1_000 * H, &p()));
    }

    #[test]
    fn a_clock_rearms_only_after_the_address_has_been_away_long_enough() {
        let p = GraceParams { rearm_secs: 72 * H, ..p() };
        let mut b = GraceBook::default();
        assert!(b.note_stratum("a", T0, &p));
        assert!(b.note_stratum("a", T0 + 2 * H, &p));
        // back after 71 hours away: the old clock, long run out
        assert!(!b.note_stratum("a", T0 + 73 * H, &p));
        // that share counts as presence, so the 72 hours start again from it
        assert!(!b.note_stratum("a", T0 + 144 * H, &p));
        assert!(b.note_stratum("a", T0 + 216 * H, &p), "72 hours away: a new clock");
        assert_eq!(b.until("a", &p), Some(T0 + 240 * H));
        // mining straight through never re-arms
        let mut b = GraceBook::default();
        for h in 0..400 {
            assert_eq!(b.note_stratum("s", T0 + h * H, &p), h < 24);
        }
    }

    #[test]
    fn seeding_starts_everyone_already_there_at_the_epoch() {
        let epoch = T0 - 10 * H;
        let p = GraceParams { epoch, ..p() };
        let rows = vec![
            (T0 - 4 * H, "old-stratum", true, false),
            (T0 - H, "old-stratum", true, false),
            (T0 - 3 * H, "failover", false, true),
            (T0 - 2 * H, "failover", true, false),
            (T0 - H, "datum-only", false, true),
            (T0 - H, "untagged", false, false),
        ];
        let mut b = GraceBook::default();
        b.seed(rows.iter().map(|r| (r.0, r.1, r.2, r.3)), &p);
        assert!(b.is_dirty());
        assert_eq!(b.until("old-stratum", &p), Some(epoch + 24 * H));
        assert_eq!(b.until("failover", &p), Some(epoch + 96 * H));
        assert_eq!(b.until("datum-only", &p), None);
        assert_eq!(b.until("untagged", &p), None);
        // 14 hours of grace left for the one already there; a newcomer gets the full 24
        assert!(b.note_stratum("old-stratum", T0 + 14 * H - 1, &p));
        assert!(!b.note_stratum("old-stratum", T0 + 14 * H, &p));
        assert!(b.note_stratum("new", T0 + 14 * H, &p));
        assert_eq!(b.until("new", &p), Some(T0 + 38 * H));

        // no epoch: the oldest row in the window
        let mut b = GraceBook::default();
        b.seed(rows.iter().map(|r| (r.0, r.1, r.2, r.3)), &GraceParams { epoch: 0, ..p });
        assert_eq!(b.until("old-stratum", &p), Some(T0 - 4 * H + 24 * H));
    }

    #[test]
    fn an_epoch_a_day_before_leaves_those_already_there_no_grace() {
        let p = GraceParams { epoch: T0 - 24 * H, ..p() };
        let mut b = GraceBook::default();
        b.seed([(T0 - H, "old", true, false)].into_iter(), &p);
        assert!(!b.note_stratum("old", T0, &p));
    }

    #[test]
    fn the_book_survives_a_round_trip_and_prunes_what_cannot_matter() {
        let p = GraceParams { rearm_secs: 72 * H, ..p() };
        let mut b = GraceBook::default();
        b.note_datum("d", T0);
        b.note_stratum("s", T0, &p);
        b.note_stratum("d", T0 + H, &p);
        let json = serde_json::to_string(&b).unwrap();
        let mut back: GraceBook = serde_json::from_str(&json).unwrap();
        assert!(!back.is_dirty());
        assert_eq!((&back.stratum, &back.datum_seen), (&b.stratum, &b.datum_seen));

        back.prune(T0 + 10 * H, &p);
        assert_eq!(back.stratum.len(), 2, "nothing is old yet");
        assert!(!back.is_dirty());
        back.prune(T0 + 80 * H, &p);
        assert!(back.stratum.is_empty(), "both would start a new clock by now");
        assert!(back.datum_seen.contains_key("d"), "still inside the lookback");
        assert!(back.is_dirty());
        back.prune(T0 + DATUM_LOOKBACK + 200 * H, &p);
        assert!(back.datum_seen.is_empty());

        // with no re-arm an episode is the only record that the address had its grace
        let mut b = GraceBook::default();
        b.note_stratum("s", T0, &GraceParams { rearm_secs: 0, ..p });
        b.prune(T0 + 10_000 * H, &GraceParams { rearm_secs: 0, ..p });
        assert_eq!(b.stratum.len(), 1);
    }

    #[test]
    fn times_move_in_coarse_steps_so_a_share_does_not_dirty_the_book() {
        let mut b = GraceBook::default();
        b.note_datum("d", T0);
        b.note_stratum("s", T0, &p());
        b.clear_dirty();
        b.note_datum("d", T0 + 59);
        b.note_stratum("s", T0 + 59, &p());
        assert!(!b.is_dirty());
        b.note_datum("d", T0 + 60);
        assert!(b.is_dirty());
    }
}
