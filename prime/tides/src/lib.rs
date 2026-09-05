//! TIDES accounting for a DATUM Prime.
//!
//! **TIDES** (Transparent Index of Distinct Extended Shares) pays each block to the
//! miners whose shares fall inside a sliding window of recent work. The window is sized
//! in *work*, not time or count: it holds the most recent shares whose summed difficulty
//! reaches `window_multiple × network_difficulty`. Every block's reward, minus the pool
//! fee, is split in proportion to each identity's work inside that window at the moment
//! the block's coinbase is issued.
//!
//! This crate is the pure accounting side: the [`Window`], the [`split`](Window::split),
//! and a small durable [`Ledger`] that survives restarts. It knows nothing about the wire
//! protocol or the node.
//!
//! Design notes, since "faster and leaner" was the brief:
//!
//! * Shares are **coalesced**: consecutive credits for the same identity in the same
//!   second and at the same height merge into one record. A GPU farm submitting
//!   difficulty-1 shares at 10 kH/s no longer produces ten thousand rows a second.
//! * Records are fixed 24-byte binary rows in an append-only file; identities are interned
//!   once into a side file. Loading replays the file, then trims to the window.
//! * Compaction rewrites the file with only the window's rows when it has grown to twice
//!   the window, so disk and startup stay proportional to the window.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub mod split;
pub use split::{Payee, Split, SplitParams};

/// Work that arrived before dual-fee tagging. Split as DATUM (the lower fee).
pub const SOURCE_UNKNOWN: u8 = 0;
/// Public house stratum (our gateway, our templates).
pub const SOURCE_STRATUM: u8 = 1;
/// External DATUM / Prime gateway.
pub const SOURCE_DATUM: u8 = 2;

/// One accepted unit of work, possibly several coalesced shares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credit {
    /// Unix seconds.
    pub ts: u32,
    /// Index into the identity table.
    pub ident: u32,
    /// Difficulty-1 share units.
    pub work: u64,
    /// Height the work was for.
    pub height: u32,
    /// `SOURCE_*` — stored in the last 4 bytes of the on-disk row (was padding).
    pub source: u8,
}

impl Credit {
    pub const SIZE: usize = 24;

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.ts.to_le_bytes());
        out.extend_from_slice(&self.ident.to_le_bytes());
        out.extend_from_slice(&self.work.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.extend_from_slice(&[self.source, 0, 0, 0]);
    }

    fn read(b: &[u8; Self::SIZE]) -> Self {
        Credit {
            ts: u32::from_le_bytes(b[0..4].try_into().unwrap()),
            ident: u32::from_le_bytes(b[4..8].try_into().unwrap()),
            work: u64::from_le_bytes(b[8..16].try_into().unwrap()),
            height: u32::from_le_bytes(b[16..20].try_into().unwrap()),
            source: b[20],
        }
    }
}

/// Per-identity view of the window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MinerStat {
    pub identity: String,
    pub work: u64,
    /// Work tagged `SOURCE_STRATUM`. The rest of `work` is DATUM (or untagged).
    pub stratum_work: u64,
    pub credits: u64,
    pub last_ts: u32,
}

/// The sliding share window and identity table. Pure in-memory state.
#[derive(Debug, Default)]
pub struct Window {
    idents: Vec<String>,
    ident_index: HashMap<String, u32>,
    credits: VecDeque<Credit>,
    totals: HashMap<u32, (u64, u64, u32)>, // work, credit rows, last ts
    total_work: u64,
    target_work: u64,
    pub lifetime_shares: u64,
    pub lifetime_work: u64,
}

impl Window {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, identity: &str) -> u32 {
        if let Some(&i) = self.ident_index.get(identity) {
            return i;
        }
        let i = self.idents.len() as u32;
        self.idents.push(identity.to_owned());
        self.ident_index.insert(identity.to_owned(), i);
        i
    }

    pub fn identity(&self, ident: u32) -> Option<&str> {
        self.idents.get(ident as usize).map(String::as_str)
    }

    pub fn identities(&self) -> &[String] {
        &self.idents
    }

    /// Index of an interned identity, without scanning [`Window::identities`]. The table
    /// grows monotonically, so a linear scan per lookup gets slower for the life of the
    /// process; this is the same `HashMap` [`Window::intern`] already maintains.
    pub fn index_of(&self, identity: &str) -> Option<u32> {
        self.ident_index.get(identity).copied()
    }

    pub fn target_work(&self) -> u64 {
        self.target_work
    }

    pub fn total_work(&self) -> u64 {
        self.total_work
    }

    pub fn len(&self) -> usize {
        self.credits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.credits.is_empty()
    }

    pub fn credits(&self) -> impl DoubleEndedIterator<Item = &Credit> + ExactSizeIterator {
        self.credits.iter()
    }

    /// Work in the window for one identity.
    pub fn work_of(&self, identity: &str) -> u64 {
        self.ident_index.get(identity).and_then(|i| self.totals.get(i)).map_or(0, |t| t.0)
    }

    /// Resize the window (e.g. the network difficulty changed) and trim.
    pub fn set_target(&mut self, target_work: u64) {
        self.target_work = target_work;
        self.trim();
    }

    /// Add work. Returns the credit as stored (it may have been merged into the tail row),
    /// and whether a new row was appended.
    pub fn credit(&mut self, identity: &str, work: u64, height: u32, ts: u32, source: u8) -> (Credit, bool) {
        let ident = self.intern(identity);
        self.lifetime_shares += 1;
        self.lifetime_work = self.lifetime_work.saturating_add(work);
        self.total_work = self.total_work.saturating_add(work);
        let t = self.totals.entry(ident).or_insert((0, 0, 0));
        t.0 = t.0.saturating_add(work);
        t.2 = ts;
        let appended = match self.credits.back_mut() {
            Some(tail)
                if tail.ident == ident && tail.ts == ts && tail.height == height && tail.source == source =>
            {
                tail.work = tail.work.saturating_add(work);
                false
            }
            _ => {
                t.1 += 1;
                self.credits.push_back(Credit { ts, ident, work, height, source });
                true
            }
        };
        let stored = *self.credits.back().unwrap();
        self.trim();
        (stored, appended)
    }

    /// Replay a stored row without coalescing or lifetime accounting.
    fn push_raw(&mut self, c: Credit) {
        self.total_work = self.total_work.saturating_add(c.work);
        let t = self.totals.entry(c.ident).or_insert((0, 0, 0));
        t.0 = t.0.saturating_add(c.work);
        t.1 += 1;
        t.2 = t.2.max(c.ts);
        self.credits.push_back(c);
    }

    /// Drop the oldest rows while the window still holds at least the target.
    fn trim(&mut self) {
        if self.target_work == 0 {
            return;
        }
        while let Some(front) = self.credits.front() {
            if self.total_work - front.work < self.target_work {
                break;
            }
            let c = self.credits.pop_front().unwrap();
            self.total_work -= c.work;
            if let Some(t) = self.totals.get_mut(&c.ident) {
                t.0 -= c.work;
                t.1 -= 1;
                if t.1 == 0 {
                    self.totals.remove(&c.ident);
                }
            }
        }
    }

    /// Per-identity totals, largest first.
    pub fn miners(&self) -> Vec<MinerStat> {
        let mut stratum: HashMap<u32, u64> = HashMap::new();
        for c in &self.credits {
            if c.source == SOURCE_STRATUM {
                *stratum.entry(c.ident).or_insert(0) += c.work;
            }
        }
        let mut v: Vec<MinerStat> = self
            .totals
            .iter()
            .map(|(&i, &(work, credits, last_ts))| MinerStat {
                identity: self.idents[i as usize].clone(),
                work,
                stratum_work: stratum.get(&i).copied().unwrap_or(0).min(work),
                credits,
                last_ts,
            })
            .collect();
        v.sort_by(|a, b| b.work.cmp(&a.work).then_with(|| a.identity.cmp(&b.identity)));
        v
    }

    /// Compute the coinbase split for a block worth `value` sats.
    pub fn split(&self, value: u64, params: &SplitParams, script_for: impl FnMut(&str) -> Option<Vec<u8>>) -> Split {
        split::compute(self.miners(), self.total_work, value, params, script_for)
    }
}

/// Persisted window state.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Meta {
    target_work: u64,
    lifetime_shares: u64,
    lifetime_work: u64,
}

/// Durable [`Window`]: identities, credit rows, and a small meta file on disk.
pub struct Ledger {
    dir: PathBuf,
    pub window: Window,
    credits_out: BufWriter<File>,
    idents_out: BufWriter<File>,
    rows_on_disk: u64,
    dirty: bool,
}

impl Ledger {
    const CREDITS: &'static str = "credits.bin";
    const IDENTS: &'static str = "identities.txt";
    const META: &'static str = "window.json";

    /// Open (or create) the ledger in `dir` and replay it.
    pub fn open(dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        let mut window = Window::new();

        let meta: Meta =
            fs::read(dir.join(Self::META)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        window.lifetime_shares = meta.lifetime_shares;
        window.lifetime_work = meta.lifetime_work;

        let idents_path = dir.join(Self::IDENTS);
        if let Ok(f) = File::open(&idents_path) {
            for line in BufReader::new(f).lines() {
                let line = line?;
                if !line.is_empty() {
                    window.intern(&line);
                }
            }
        }

        let credits_path = dir.join(Self::CREDITS);
        let mut rows_on_disk = 0u64;
        if let Ok(f) = File::open(&credits_path) {
            let len = f.metadata()?.len();
            let usable = len - len % Credit::SIZE as u64;
            let mut buf = Vec::with_capacity(usable as usize);
            f.take(usable).read_to_end(&mut buf)?;
            for chunk in buf.as_chunks::<{ Credit::SIZE }>().0 {
                let c = Credit::read(chunk);
                if (c.ident as usize) < window.idents.len() {
                    window.push_raw(c);
                    rows_on_disk += 1;
                }
            }
        }
        window.set_target(meta.target_work);

        let credits_out = BufWriter::new(OpenOptions::new().create(true).append(true).open(&credits_path)?);
        let idents_out = BufWriter::new(OpenOptions::new().create(true).append(true).open(&idents_path)?);
        let mut l = Ledger { dir, window, credits_out, idents_out, rows_on_disk, dirty: false };
        if l.rows_on_disk > 2 * l.window.len() as u64 + 4096 {
            l.compact()?;
        }
        Ok(l)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Record work. Coalesced rows are rewritten in place on the next flush via compaction
    /// bookkeeping; here we append only when a new row was created and otherwise patch the
    /// tail row's work on disk.
    pub fn credit(&mut self, identity: &str, work: u64, height: u32, ts: u32, source: u8) -> io::Result<()> {
        let known = self.window.ident_index.contains_key(identity);
        let (row, appended) = self.window.credit(identity, work, height, ts, source);
        if !known {
            self.idents_out.write_all(identity.as_bytes())?;
            self.idents_out.write_all(b"\n")?;
        }
        let mut b = Vec::with_capacity(Credit::SIZE);
        row.write(&mut b);
        if appended {
            self.credits_out.write_all(&b)?;
            self.rows_on_disk += 1;
        } else {
            // overwrite the tail row: flush pending appends first so the offsets line up
            self.credits_out.flush()?;
            let f = self.credits_out.get_mut();
            let end = f.seek(SeekFrom::End(0))?;
            if end >= Credit::SIZE as u64 {
                f.seek(SeekFrom::Start(end - Credit::SIZE as u64))?;
                f.write_all(&b)?;
                f.seek(SeekFrom::End(0))?;
            }
        }
        self.dirty = true;
        Ok(())
    }

    pub fn set_target(&mut self, target_work: u64) {
        if self.window.target_work() != target_work {
            self.window.set_target(target_work);
            self.dirty = true;
        }
    }

    /// Push buffered rows to the OS and write the meta file. Call periodically.
    pub fn flush(&mut self) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        self.idents_out.flush()?;
        self.credits_out.flush()?;
        let meta = Meta {
            target_work: self.window.target_work(),
            lifetime_shares: self.window.lifetime_shares,
            lifetime_work: self.window.lifetime_work,
        };
        write_atomic(&self.dir.join(Self::META), &serde_json::to_vec_pretty(&meta)?)?;
        self.dirty = false;
        if self.rows_on_disk > 2 * self.window.len() as u64 + 4096 {
            self.compact()?;
        }
        Ok(())
    }

    /// Durable flush: also fsync the credit file.
    pub fn sync(&mut self) -> io::Result<()> {
        self.flush()?;
        self.credits_out.get_ref().sync_data()?;
        self.idents_out.get_ref().sync_data()
    }

    /// Rewrite the credits file with only the rows still in the window.
    pub fn compact(&mut self) -> io::Result<()> {
        self.credits_out.flush()?;
        let tmp = self.dir.join("credits.bin.tmp");
        {
            let mut w = BufWriter::new(File::create(&tmp)?);
            let mut buf = Vec::with_capacity(Credit::SIZE * 1024);
            for c in self.window.credits() {
                c.write(&mut buf);
                if buf.len() >= Credit::SIZE * 1024 {
                    w.write_all(&buf)?;
                    buf.clear();
                }
            }
            w.write_all(&buf)?;
            w.flush()?;
            w.get_ref().sync_data()?;
        }
        fs::rename(&tmp, self.dir.join(Self::CREDITS))?;
        self.credits_out = BufWriter::new(OpenOptions::new().append(true).open(self.dir.join(Self::CREDITS))?);
        self.rows_on_disk = self.window.len() as u64;
        Ok(())
    }

    /// Import rows from another pool's JSON ledger of the form
    /// `{"credits":[{"ts":..,"identity":"..","work":..}, ...]}` so a window carries over
    /// when this Prime replaces it. Rows are appended in file order.
    pub fn import_json_credits(&mut self, path: impl AsRef<Path>) -> io::Result<usize> {
        #[derive(Deserialize)]
        struct Row {
            ts: u32,
            identity: String,
            work: u64,
            #[serde(default)]
            height: u32,
        }
        #[derive(Deserialize)]
        struct Doc {
            credits: Vec<Row>,
        }
        let doc: Doc =
            serde_json::from_slice(&fs::read(path)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let n = doc.credits.len();
        for r in doc.credits {
            self.credit(&r.identity, r.work, r.height, r.ts, SOURCE_UNKNOWN)?;
        }
        self.flush()?;
        Ok(n)
    }
}

fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data)?;
    fs::rename(tmp, path)
}

/// A block the pool's coinbase paid (found by a gateway on this pool), for the record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockRecord {
    pub ts: u64,
    pub height: u32,
    /// Display-order hex.
    pub hash: String,
    /// Identity that submitted the winning share, if it came through this Prime.
    pub finder: Option<String>,
    pub coinbase_value: u64,
    /// `split` (paid the TIDES split), `pool-only` (stock gateway's empty coinbase: the pool
    /// holds the whole reward and owes the window), or `unknown`.
    pub kind: String,
    /// Sats the pool owes the window for a pool-only block, after the fee.
    pub owed_sats: u64,
    /// The split that should have been (or was) paid, identity → sats.
    pub split: Vec<(String, u64)>,
    pub pool_sats: u64,
    /// Confirmed in the node's main chain.
    pub settled: bool,
    /// Outcome of this Prime's own `submitblock` (the gateway submits too): `pending`,
    /// `accepted`, `duplicate`, `no-transactions`, or `rejected: <reason>`.
    #[serde(default)]
    pub submit: String,
    /// Which gateway (identity key, hex prefix) sent the winning share.
    #[serde(default)]
    pub gateway: String,
}

/// Append-only JSON-lines block log.
pub struct BlockLog {
    path: PathBuf,
}

impl BlockLog {
    pub fn open(dir: impl AsRef<Path>) -> Self {
        BlockLog { path: dir.as_ref().join("blocks.jsonl") }
    }

    pub fn append(&self, r: &BlockRecord) -> io::Result<()> {
        let mut f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        let mut line = serde_json::to_vec(r)?;
        line.push(b'\n');
        f.write_all(&line)?;
        f.sync_data()
    }

    /// All records in first-seen order. A hash appearing more than once (status updates are
    /// appended, never rewritten) yields only its latest line.
    pub fn read_all(&self) -> io::Result<Vec<BlockRecord>> {
        let f = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e),
        };
        let mut out: Vec<BlockRecord> = Vec::new();
        let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for line in BufReader::new(f).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(r) = serde_json::from_str::<BlockRecord>(&line) {
                match index.get(&r.hash) {
                    Some(&i) => out[i] = r,
                    None => {
                        index.insert(r.hash.clone(), out.len());
                        out.push(r);
                    }
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cost of resolving each window miner to its identity index, the way `stats::build`
    /// does on every `stats.json`. Run with
    /// `cargo test --release -p tides -- --ignored --nocapture bench_index_lookup`.
    #[test]
    #[ignore]
    fn bench_index_lookup() {
        use std::time::Instant;
        let mut w = Window::new();
        // the identity table as it looks after a long uptime, plus the miners actually in
        // the window: the table keeps every identity ever seen, the window does not
        for i in 0..60_000u32 {
            w.credit(&format!("bc1qidle{i:040}"), 1, 1, 1, SOURCE_DATUM);
        }
        w.set_target(0);
        let active: Vec<String> = (0..500).map(|i| format!("bc1qactive{i:038}")).collect();
        for a in &active {
            w.credit(a, 1000, 1, 2, SOURCE_DATUM);
        }
        let mut sink = 0u64;
        let t = Instant::now();
        for a in &active {
            sink += w.identities().iter().position(|i| i == a).unwrap_or(0) as u64;
        }
        let scan = t.elapsed();
        let t = Instant::now();
        for a in &active {
            sink += w.index_of(a).unwrap_or(0) as u64;
        }
        let map = t.elapsed();
        println!("identities={} miners={} sink={sink}", w.identities().len(), active.len());
        println!("  identities().position() : {scan:?}");
        println!("  index_of()              : {map:?}");
    }

    #[test]
    fn index_of_matches_a_linear_scan() {
        let mut w = Window::new();
        for id in ["a", "b", "c"] {
            w.credit(id, 1, 1, 1, SOURCE_DATUM);
        }
        for (i, id) in w.identities().to_vec().iter().enumerate() {
            assert_eq!(w.index_of(id), Some(i as u32));
        }
        assert_eq!(w.index_of("nope"), None);
        // survives trimming: the table keeps identities whose rows have aged out
        w.set_target(1);
        assert_eq!(w.index_of("a"), Some(0));
    }

    #[test]
    fn window_trims_to_target_and_tracks_totals() {
        let mut w = Window::new();
        w.set_target(100);
        for i in 0..10 {
            w.credit("a", 20, 1, 1000 + i, SOURCE_UNKNOWN);
        }
        // 10 × 20 = 200 in, window keeps the newest rows summing to ≥ 100 → 5 rows
        assert_eq!(w.total_work(), 100);
        assert_eq!(w.len(), 5);
        w.credit("b", 5, 1, 2000, SOURCE_UNKNOWN);
        assert_eq!(w.total_work(), 105);
        w.credit("b", 5, 1, 2000, SOURCE_UNKNOWN); // coalesces with the previous row
        assert_eq!(w.len(), 6);
        assert_eq!(w.total_work(), 110);
        assert_eq!(w.work_of("b"), 10);
        assert_eq!(w.lifetime_shares, 12);
        assert_eq!(w.lifetime_work, 210);
        let m = w.miners();
        assert_eq!(m[0].identity, "a");
        assert_eq!(m[0].work, 100);
        assert_eq!(m[1].work, 10);
        // shrinking the target trims from the front
        w.set_target(20);
        assert!(w.total_work() >= 20 && w.total_work() < 40);
    }

    #[test]
    fn stratum_and_datum_work_do_not_coalesce() {
        let mut w = Window::new();
        w.set_target(10_000);
        w.credit("a", 600, 1, 100, SOURCE_DATUM);
        w.credit("a", 400, 1, 100, SOURCE_STRATUM);
        assert_eq!(w.len(), 2);
        let m = &w.miners()[0];
        assert_eq!(m.work, 1000);
        assert_eq!(m.stratum_work, 400);
    }

    #[test]
    fn ledger_persists_replays_and_compacts() {
        let dir = std::env::temp_dir().join(format!("tides-test-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        {
            let mut l = Ledger::open(&dir).unwrap();
            l.set_target(50);
            for i in 0..100u32 {
                l.credit(if i % 3 == 0 { "x" } else { "y" }, 1, 7, i, SOURCE_UNKNOWN).unwrap();
            }
            // coalesced tail rewrite
            l.credit("y", 3, 7, 99, SOURCE_UNKNOWN).unwrap();
            l.credit("y", 4, 7, 99, SOURCE_UNKNOWN).unwrap();
            l.sync().unwrap();
            assert!(l.window.total_work() >= 50);
        }
        let l = Ledger::open(&dir).unwrap();
        assert!(l.window.total_work() >= 50);
        assert_eq!(l.window.target_work(), 50);
        assert_eq!(l.window.lifetime_shares, 102);
        assert_eq!(l.window.lifetime_work, 107);
        assert_eq!(l.window.identities(), &["x".to_string(), "y".to_string()]);
        let tail = *l.window.credits().last().unwrap();
        assert_eq!(tail.work, 7, "coalesced 3+4 for y at ts 99 survived a restart");
        assert_eq!(tail.ts, 99);
        // the file holds only what the window holds after compaction
        let mut l = l;
        l.compact().unwrap();
        let len = fs::metadata(dir.join("credits.bin")).unwrap().len();
        assert_eq!(len as usize, l.window.len() * Credit::SIZE);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_legacy_json() {
        let dir = std::env::temp_dir().join(format!("tides-test-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let json = dir.join("old.json");
        fs::write(
            &json,
            r#"{"credits":[{"ts":1,"identity":"bc1qa","work":56},{"ts":2,"identity":"1Fw8","work":8192}],"carry":{},"shares":2}"#,
        )
        .unwrap();
        let mut l = Ledger::open(&dir).unwrap();
        assert_eq!(l.import_json_credits(&json).unwrap(), 2);
        assert_eq!(l.window.work_of("1Fw8"), 8192);
        assert_eq!(l.window.total_work(), 8248);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn block_log_round_trip() {
        let dir = std::env::temp_dir().join(format!("tides-test-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let log = BlockLog::open(&dir);
        assert!(log.read_all().unwrap().is_empty());
        let r = BlockRecord {
            ts: 1,
            height: 2,
            hash: "00".into(),
            finder: Some("bc1q".into()),
            coinbase_value: 3,
            kind: "split".into(),
            owed_sats: 0,
            split: vec![("bc1q".into(), 3)],
            pool_sats: 0,
            settled: true,
            submit: "accepted".into(),
            gateway: "ab".into(),
        };
        log.append(&r).unwrap();
        assert_eq!(log.read_all().unwrap(), vec![r.clone()]);
        let mut r2 = r.clone();
        r2.submit = "duplicate".into();
        log.append(&r2).unwrap();
        assert_eq!(log.read_all().unwrap(), vec![r2]);
        let _ = fs::remove_dir_all(&dir);
    }
}
