//! Worker names per payout identity on one gateway session, for the pool UI.
//!
//! A share's username is `address[~modifier][.worker]`. The address is the identity and is all
//! that crediting, the coinbase split and payouts ever read (`address::identity_of`). The worker
//! part is whatever a miner typed into its machine: it is kept here so a miner can see its own
//! rigs, and it is read by nothing but the stats document.
//!
//! Anyone can send any name, so everything is bounded: a name is cut to [`MAX_NAME_LEN`]
//! printable ASCII characters, an identity keeps [`MAX_PER_IDENTITY`] names and a session
//! [`MAX_PER_SESSION`]. Past a limit the name least recently seen makes room, and what it had
//! done is kept in an overflow tally, so the tallies of a session always add up to the work its
//! shares were noted with.
//!
//! No IO and no clock: the caller passes the time.

use std::borrow::Cow;
use std::collections::HashMap;

/// Characters of a worker name that are kept.
pub const MAX_NAME_LEN: usize = 32;
/// Worker names kept for one identity on one session.
pub const MAX_PER_IDENTITY: usize = 256;
/// Worker names kept on one session, over all its identities.
pub const MAX_PER_SESSION: usize = 1024;

const BUCKET_SECS: u64 = 60;
const BUCKETS: usize = 10;
/// The longest span recent work is measured over: the same ten minutes the window's
/// per-identity hashrate uses.
pub const RECENT_SECS: u64 = BUCKET_SECS * BUCKETS as u64;

/// The worker part of a share username: what follows the first `.`, up to a `~` modifier.
/// Empty when there is none. Anything that is not printable ASCII becomes `_`, and the name is
/// cut to [`MAX_NAME_LEN`] characters.
pub fn worker_of(username: &str) -> Cow<'_, str> {
    let u = username.trim();
    let Some(dot) = u.find('.') else {
        return Cow::Borrowed("");
    };
    let raw = &u[dot + 1..];
    let raw = &raw[..raw.find('~').unwrap_or(raw.len())];
    if raw.len() <= MAX_NAME_LEN && raw.bytes().all(|b| b.is_ascii_graphic()) {
        return Cow::Borrowed(raw);
    }
    Cow::Owned(raw.chars().take(MAX_NAME_LEN).map(|c| if c.is_ascii_graphic() { c } else { '_' }).collect())
}

/// Work per minute over the last [`BUCKETS`] minutes; the last bucket is minute `minute`.
#[derive(Clone, Debug, Default, PartialEq)]
struct Recent {
    minute: u64,
    work: [u64; BUCKETS],
}

impl Recent {
    /// Move the newest bucket up to `minute`. A clock that steps back moves nothing, and work
    /// noted then lands in the newest bucket.
    fn advance(&mut self, minute: u64) {
        let gap = minute.saturating_sub(self.minute);
        if gap == 0 {
            return;
        }
        match usize::try_from(gap) {
            Ok(g) if g < BUCKETS => {
                self.work.rotate_left(g);
                self.work[BUCKETS - g..].fill(0);
            }
            _ => self.work = [0; BUCKETS],
        }
        self.minute = minute;
    }

    fn add(&mut self, work: u64, ts: u64) {
        self.advance(ts / BUCKET_SECS);
        self.work[BUCKETS - 1] = self.work[BUCKETS - 1].saturating_add(work);
    }

    fn merge(&mut self, other: &Recent) {
        self.advance(other.minute);
        // `other` is now no newer than `self`: its bucket `i` is `behind` buckets further back
        let behind = usize::try_from(self.minute - other.minute).unwrap_or(BUCKETS);
        for (i, w) in other.work.iter().enumerate().skip(behind) {
            self.work[i - behind] = self.work[i - behind].saturating_add(*w);
        }
    }

    /// Work in the buckets still inside the last [`BUCKETS`] minutes at `ts`.
    fn work_at(&self, ts: u64) -> u64 {
        let gap = (ts / BUCKET_SECS).saturating_sub(self.minute);
        let skip = usize::try_from(gap).unwrap_or(BUCKETS).min(BUCKETS);
        self.work[skip..].iter().fold(0u64, |a, w| a.saturating_add(*w))
    }
}

#[derive(Clone, Debug, Default)]
struct Tally {
    work: u64,
    shares: u64,
    first_ts: u64,
    last_ts: u64,
    recent: Recent,
}

impl Tally {
    fn add(&mut self, work: u64, ts: u64) {
        if self.shares == 0 {
            self.first_ts = ts;
        }
        self.work = self.work.saturating_add(work);
        self.shares = self.shares.saturating_add(1);
        self.first_ts = self.first_ts.min(ts);
        self.last_ts = self.last_ts.max(ts);
        self.recent.add(work, ts);
    }

    fn absorb(&mut self, other: &Tally) {
        if other.shares == 0 {
            return;
        }
        self.first_ts = if self.shares == 0 { other.first_ts } else { self.first_ts.min(other.first_ts) };
        self.work = self.work.saturating_add(other.work);
        self.shares = self.shares.saturating_add(other.shares);
        self.last_ts = self.last_ts.max(other.last_ts);
        self.recent.merge(&other.recent);
    }
}

#[derive(Clone, Debug, Default)]
struct Worker {
    tally: Tally,
    /// The book's share counter when this name was last seen; the lowest is evicted first.
    seen: u64,
}

/// Names that made room for newer ones, and what they had done.
#[derive(Clone, Debug, Default)]
struct Overflow {
    names: u64,
    tally: Tally,
}

impl Overflow {
    fn take(&mut self, w: &Worker) {
        self.names = self.names.saturating_add(1);
        self.tally.absorb(&w.tally);
    }

    fn absorb(&mut self, other: &Overflow) {
        self.names = self.names.saturating_add(other.names);
        self.tally.absorb(&other.tally);
    }
}

#[derive(Clone, Debug, Default)]
struct Named {
    workers: HashMap<String, Worker>,
    overflow: Overflow,
}

/// One line of the book for the stats document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row<'a> {
    pub identity: &'a str,
    /// Empty for work whose username had no worker part.
    pub name: &'a str,
    pub work: u64,
    pub shares: u64,
    pub last_ts: u64,
    /// Work in the last `recent_secs` seconds, for a rate.
    pub recent_work: u64,
    pub recent_secs: u64,
    /// For an overflow row: how many names were evicted into it. A name that comes back and is
    /// evicted again counts again.
    pub names: u64,
}

/// What one session's shares have said about who did them.
#[derive(Clone, Debug, Default)]
pub struct WorkerBook {
    identities: HashMap<String, Named>,
    /// Names held across `identities`.
    names: usize,
    seen: u64,
    /// Overflow of identities that no longer hold any name.
    rest: Overflow,
}

impl WorkerBook {
    /// Count one credited share of `work` for `identity`, under the worker `username` names.
    pub fn note(&mut self, identity: &str, username: &str, work: u64, ts: u64) {
        let name = worker_of(username);
        self.seen += 1;
        let seen = self.seen;
        if let Some(w) = self.identities.get_mut(identity).and_then(|n| n.workers.get_mut(name.as_ref())) {
            w.tally.add(work, ts);
            w.seen = seen;
            return;
        }
        if self.identities.get(identity).is_some_and(|n| n.workers.len() >= MAX_PER_IDENTITY) {
            self.evict(Some(identity));
        } else if self.names >= MAX_PER_SESSION {
            self.evict(None);
        }
        let mut w = Worker { tally: Tally::default(), seen };
        w.tally.add(work, ts);
        self.identities.entry(identity.to_string()).or_default().workers.insert(name.into_owned(), w);
        self.names += 1;
    }

    /// Drop the name least recently seen, of `within` or of the whole session, into its
    /// identity's overflow. An identity left with no name goes too, its overflow into `rest`,
    /// so the identities held never outnumber the names.
    fn evict(&mut self, within: Option<&str>) {
        let oldest = self
            .identities
            .iter()
            .filter(|(id, _)| within.is_none_or(|w| w == id.as_str()))
            .flat_map(|(id, n)| n.workers.iter().map(move |(name, w)| (w.seen, id, name)))
            .min_by_key(|(seen, _, _)| *seen)
            .map(|(_, id, name)| (id.clone(), name.clone()));
        let Some((id, name)) = oldest else {
            return;
        };
        let Some(named) = self.identities.get_mut(&id) else {
            return;
        };
        if let Some(w) = named.workers.remove(&name) {
            named.overflow.take(&w);
            self.names -= 1;
        }
        if named.workers.is_empty() {
            if let Some(gone) = self.identities.remove(&id) {
                self.rest.absorb(&gone.overflow);
            }
        }
    }

    fn row<'a>(identity: &'a str, name: &'a str, t: &Tally, names: u64, ts: u64) -> Row<'a> {
        // from the start of the oldest bucket still counted, or from the first share if later
        let window_start = (ts / BUCKET_SECS).saturating_sub(BUCKETS as u64 - 1) * BUCKET_SECS;
        Row {
            identity,
            name,
            work: t.work,
            shares: t.shares,
            last_ts: t.last_ts,
            recent_work: t.recent.work_at(ts),
            recent_secs: ts.saturating_sub(window_start.max(t.first_ts)).clamp(30, RECENT_SECS),
            names,
        }
    }

    /// Every name held, by identity and then name.
    pub fn rows(&self, ts: u64) -> Vec<Row<'_>> {
        let mut rows: Vec<Row<'_>> = self
            .identities
            .iter()
            .flat_map(|(id, n)| n.workers.iter().map(move |(name, w)| Self::row(id, name, &w.tally, 0, ts)))
            .collect();
        rows.sort_by(|a, b| (a.identity, a.name).cmp(&(b.identity, b.name)));
        rows
    }

    /// What evicted names had done, per identity. The row with an empty identity is for
    /// identities that no longer hold a name.
    pub fn overflow(&self, ts: u64) -> Vec<Row<'_>> {
        let mut rows: Vec<Row<'_>> = self
            .identities
            .iter()
            .filter(|(_, n)| n.overflow.names > 0)
            .map(|(id, n)| Self::row(id, "", &n.overflow.tally, n.overflow.names, ts))
            .collect();
        rows.sort_by(|a, b| a.identity.cmp(b.identity));
        if self.rest.names > 0 {
            rows.push(Self::row("", "", &self.rest.tally, self.rest.names, ts));
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "bc1qvchspt9gm5dq0geq3kxx53k3n87znwwvwc30t0";
    const B: &str = "bc1qk3kxstl02hqnhynwtx0zws7merw6ynut52vtzs";
    const T: u64 = 1_760_000_000;

    /// Note `(username, work, seconds after T)` shares, each for the identity its username
    /// names, and return the book.
    fn book(shares: &[(&str, u64, u64)]) -> WorkerBook {
        let mut b = WorkerBook::default();
        for (user, work, at) in shares {
            b.note(crate::address::identity_of(user), user, *work, T + at);
        }
        b
    }

    /// `(identity, name, work, shares)` of every row at `ts`.
    fn held(b: &WorkerBook, ts: u64) -> Vec<(String, String, u64, u64)> {
        b.rows(ts).iter().map(|r| (r.identity.to_string(), r.name.to_string(), r.work, r.shares)).collect()
    }

    fn total(b: &WorkerBook) -> (u64, u64) {
        let rows = b.rows(T).into_iter().chain(b.overflow(T));
        rows.fold((0, 0), |(w, s), r| (w + r.work, s + r.shares))
    }

    #[test]
    fn the_worker_is_what_follows_the_first_dot() {
        for (user, want) in [
            ("bc1qaddr.A301", "A301"),
            ("bc1qaddr", ""),
            ("bc1qaddr.", ""),
            ("bc1qaddr~mod", ""),
            ("bc1qaddr~mod.A301", "A301"),
            ("bc1qaddr.A301~mod", "A301"),
            ("  bc1qaddr.rig.7  ", "rig.7"),
            ("bc1qaddr.rig 7", "rig_7"),
            ("bc1qaddr.<b>x</b>", "<b>x</b>"),
            ("bc1qaddr.矿机1\n", "__1"),
            ("bc1qaddr.\u{1b}[31m", "_[31m"),
        ] {
            assert_eq!(worker_of(user), want, "{user:?}");
        }
        let long = format!("bc1qaddr.{}", "x".repeat(4096));
        assert_eq!(worker_of(&long), "x".repeat(MAX_NAME_LEN));
        let wide = format!("bc1qaddr.{}", "é".repeat(4096));
        assert_eq!(worker_of(&wide), "_".repeat(MAX_NAME_LEN));
    }

    #[test]
    fn any_username_gives_a_short_printable_name() {
        // every byte value in every position of a short suffix, and every prefix of a long one
        let mut cases: Vec<String> = Vec::new();
        for b in 0u8..=255 {
            let c = char::from(b);
            cases.push(format!("addr.{c}"));
            cases.push(format!("addr.ab{c}cd"));
            cases.push(format!("{c}.x{c}"));
            cases.push(format!("addr~{c}.{c}{c}"));
        }
        let long: String = (0..200u32).map(|i| char::from_u32(0x20 + i * 37 % 0x2000).unwrap_or('?')).collect();
        for cut in 0..long.chars().count() {
            cases.push(format!("addr.{}", long.chars().take(cut).collect::<String>()));
        }
        for user in &cases {
            let name = worker_of(user);
            assert!(name.len() <= MAX_NAME_LEN, "{user:?} -> {name:?}");
            assert!(name.bytes().all(|b| b.is_ascii_graphic()), "{user:?} -> {name:?}");
            assert!(!name.contains('~'), "{user:?} -> {name:?}");
            // a name is the same name once it has been through here
            assert_eq!(worker_of(&format!("addr.{name}")), name, "{user:?}");
        }
    }

    #[test]
    fn two_workers_and_work_with_no_name_are_three_rows_of_one_identity() {
        let a301 = format!("{A}.A301");
        let a302 = format!("{A}.A302");
        let b = book(&[(&a301, 4, 0), (&a302, 2, 1), (A, 8, 2), (&a301, 4, 3)]);
        assert_eq!(
            held(&b, T + 3),
            [
                (A.to_string(), String::new(), 8, 1),
                (A.to_string(), "A301".to_string(), 8, 2),
                (A.to_string(), "A302".to_string(), 2, 1),
            ]
        );
        assert!(b.overflow(T + 3).is_empty());
        assert_eq!(b.rows(T + 3)[1].last_ts, T + 3);
    }

    #[test]
    fn the_same_name_under_two_identities_is_two_workers() {
        let b = book(&[(&format!("{A}.rig"), 1, 0), (&format!("{B}.rig"), 2, 0)]);
        assert_eq!(held(&b, T), [(B.to_string(), "rig".to_string(), 2, 1), (A.to_string(), "rig".to_string(), 1, 1)]);
    }

    #[test]
    fn a_flood_of_names_under_one_identity_evicts_only_its_own_oldest_and_loses_no_work() {
        let mut b = WorkerBook::default();
        b.note(B, &format!("{B}.steady"), 5, T);
        b.note(A, &format!("{A}.A301"), 7, T);
        for i in 0..5_000u64 {
            b.note(A, &format!("{A}.junk{i}{}", "z".repeat(500)), 1, T + 1);
            if i % 100 == 0 {
                // a real machine keeps submitting, and so is never the least recently seen
                b.note(A, &format!("{A}.A301"), 7, T + 1);
            }
        }
        let rows = b.rows(T + 1);
        assert_eq!(rows.iter().filter(|r| r.identity == A).count(), MAX_PER_IDENTITY);
        assert!(rows.iter().all(|r| r.name.len() <= MAX_NAME_LEN));
        assert_eq!(rows.iter().find(|r| r.name == "A301").map(|r| r.work), Some(7 * 51));
        assert_eq!(rows.iter().find(|r| r.identity == B).map(|r| (r.name, r.work)), Some(("steady", 5)));
        let over = b.overflow(T + 1);
        assert_eq!(over.len(), 1);
        assert_eq!((over[0].identity, over[0].names), (A, 5_000 - (MAX_PER_IDENTITY as u64 - 1)));
        assert_eq!(total(&b), (5 + 7 * 51 + 5_000, 1 + 51 + 5_000));
    }

    #[test]
    fn a_flood_of_identities_is_held_to_the_session_limit() {
        let mut b = WorkerBook::default();
        for i in 0..3 * MAX_PER_SESSION as u64 {
            b.note(&format!("addr{i}"), &format!("addr{i}.w"), 2, T + i);
        }
        assert_eq!(b.rows(T).len(), MAX_PER_SESSION);
        assert_eq!(b.identities.len(), MAX_PER_SESSION);
        // the newest are the ones kept
        assert!(b.rows(T).iter().any(|r| r.identity == format!("addr{}", 3 * MAX_PER_SESSION - 1)));
        assert!(!b.rows(T).iter().any(|r| r.identity == "addr0"));
        let over = b.overflow(T);
        assert_eq!((over.len(), over[0].identity, over[0].names), (1, "", 2 * MAX_PER_SESSION as u64));
        assert_eq!(total(&b), (6 * MAX_PER_SESSION as u64, 3 * MAX_PER_SESSION as u64));
    }

    #[test]
    fn every_share_noted_is_in_exactly_one_tally() {
        // a fixed pseudo-random stream over few identities and many names, long enough to
        // evict on both limits
        let mut b = WorkerBook::default();
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let (mut work, mut shares) = (0u64, 0u64);
        for i in 0..40_000u64 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let id = format!("id{}", x % 7);
            let user = match x % 5 {
                0 => id.clone(),
                _ => format!("{id}.w{}", (x >> 8) % 900),
            };
            let w = 1 + (x >> 40) % 16;
            b.note(&id, &user, w, T + i / 10);
            work += w;
            shares += 1;
            assert!(b.names <= MAX_PER_SESSION);
            assert!(b.identities.len() <= b.names);
            assert!(b.identities.values().all(|n| !n.workers.is_empty() && n.workers.len() <= MAX_PER_IDENTITY));
            assert_eq!(b.names, b.identities.values().map(|n| n.workers.len()).sum::<usize>());
        }
        assert_eq!(total(&b), (work, shares));
        assert!(!b.overflow(T).is_empty());
    }

    #[test]
    fn recent_work_is_the_last_ten_minutes_and_forgets_the_rest() {
        let user = format!("{A}.A301");
        let mut b = WorkerBook::default();
        let start = T - T % 60;
        for minute in 0..30u64 {
            b.note(A, &user, 100, start + minute * 60 + 5);
        }
        let at = start + 29 * 60 + 5;
        let r = &b.rows(at)[0];
        assert_eq!((r.work, r.shares), (3_000, 30));
        assert_eq!(r.recent_work, 1_000);
        assert_eq!(r.recent_secs, 9 * 60 + 5);
        // five minutes on with nothing more, half of it has aged out
        let r = &b.rows(at + 300)[0];
        assert_eq!((r.work, r.recent_work), (3_000, 500));
        // and after ten, all of it, while the totals stay
        let r = &b.rows(at + 3_600)[0];
        assert_eq!((r.work, r.recent_work, r.last_ts), (3_000, 0, at));
    }

    #[test]
    fn a_worker_seen_for_a_minute_is_rated_over_that_minute() {
        let user = format!("{A}.A301");
        let b = book(&[(&user, 10, 0), (&user, 10, 60)]);
        let r = &b.rows(T + 60)[0];
        assert_eq!((r.recent_work, r.recent_secs), (20, 60));
        // one share is not divided by a second
        let b = book(&[(&user, 10, 0)]);
        assert_eq!(b.rows(T)[0].recent_secs, 30);
    }

    #[test]
    fn evicted_work_keeps_its_minutes_in_the_overflow() {
        let mut b = WorkerBook::default();
        let start = T - T % 60;
        b.note(A, &format!("{A}.old"), 50, start);
        for i in 0..MAX_PER_IDENTITY as u64 {
            b.note(A, &format!("{A}.w{i}"), 1, start + 8 * 60);
        }
        let at = start + 8 * 60;
        let over = b.overflow(at);
        assert_eq!((over[0].names, over[0].work, over[0].recent_work), (1, 50, 50));
        // the evicted share was eight minutes old: two minutes later it has aged out
        assert_eq!(b.overflow(at + 120)[0].recent_work, 0);
        assert_eq!(b.overflow(at + 120)[0].work, 50);
    }

    #[test]
    fn a_clock_that_steps_back_loses_nothing() {
        let user = format!("{A}.A301");
        let b = book(&[(&user, 10, 600), (&user, 10, 0)]);
        let r = &b.rows(T + 600)[0];
        assert_eq!((r.work, r.shares, r.recent_work, r.last_ts), (20, 2, 20, T + 600));
    }
}
