//! One DATUM gateway connection: handshake, configuration, coinbaser replies, share
//! verification and crediting, block relay.
//!
//! A session is a single task owning both halves of the socket; there is no per-message
//! locking beyond a short ledger critical section on accepted shares.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use datum_wire::coinbase;
use datum_wire::coinbaser::{self, Output};
use datum_wire::crypto::{self, Channel, Identity};
use datum_wire::frame::{Header, KeyStream, CLIENT_INITIAL_KEY};
use datum_wire::handshake::{self, ClientHello, Generation};
use datum_wire::mining::{self, ClientMsg, JobValidationReply, PowSubmit, ValidationStatus};
use datum_wire::verify::{self, CoinbaseKind, JobSlot, Policy, VerifiedShare};
use datum_wire::{cmd, MAX_CMD_LEN};
use rand_core::{OsRng, RngCore};
use tides::split::Split;
use tides::{BlockRecord, Payee, SplitParams};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{interval, MissedTickBehavior};

use crate::address::{self};
use crate::config::Config;
use crate::node;
use crate::state::{now, ClientInfo, JobCheck, Seen, Shared};
use crate::validity::{self, Fault, ParentVerdict};

/// `gateway_key` is the gateway's whole identity key in hex, so a configured full key is
/// matched in full.
fn house_stratum(cfg: &Config, remote: SocketAddr, gateway_key: &str) -> bool {
    if cfg.house_loopback && remote.ip().is_loopback() {
        return true;
    }
    let g = gateway_key.to_ascii_lowercase();
    cfg.house_gateways.iter().any(|h| !h.is_empty() && g.starts_with(h.as_str()))
}

/// Another pool's stratum front (`stratum-front-gateways`, `stratum-front-ips`): its work is
/// charged as stratum work. Never the pool's own gateway, which has its own rules.
fn stratum_front(cfg: &Config, remote: SocketAddr, gateway_key: &str) -> bool {
    if house_stratum(cfg, remote, gateway_key) {
        return false;
    }
    let g = gateway_key.to_ascii_lowercase();
    cfg.stratum_front_ips.contains(&remote.ip()) || cfg.stratum_front_gateways.iter().any(|k| g.starts_with(k.as_str()))
}

/// The `held-split-builds` entry a gateway's hello names, if any.
///
/// A C gateway's user agent is `v0.4.1-beta[+flavor]/<hash>` (the hello in `datum_protocol.c`):
/// the whole commit it was built from, then `+` if the tree had changes on top of it, then
/// `(tag)` if it was built at a tag. A build with changes is not matched, because the change may
/// be the very fix, and nor is one whose user agent says it places the split. Only what follows
/// the first `/` is read, so another program's version (`ratum-gateway/0.1.28/<hash>`) is never
/// taken for a C gateway's commit.
fn held_split_build<'a>(builds: &'a [String], ua: &str) -> Option<&'a str> {
    if builds.is_empty() || handshake::is_split_gateway(ua) {
        return None;
    }
    let (_, rest) = ua.split_once('/')?;
    let hash = rest.split_once('(').map_or(rest, |(h, _)| h).to_ascii_lowercase();
    if !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    builds.iter().map(String::as_str).find(|b| crate::config::git_hash_prefix(b) && hash.starts_with(b))
}

/// Shares with work, every one of them naming the same payout, before a held-split session is
/// left paying itself.
///
/// Section 0 pays the one script it is configured with, so every miner behind a held gateway
/// mines for whoever that is. A stock gateway passes each miner's own username through by default
/// (`pool_pass_full_users`), and left on one miner's script, another miner's shares are that
/// one's solo work: accepted, credited to nobody, and a block on them pays the first. Without the
/// key the same shares are pool-only, credited, and owed back. Its vardiff sets every miner about
/// the same share rate, so with a second miner of like rate on the gateway, sixteen shares in a
/// row naming one payout happen about once in 30 000 sessions. Until then the session owes as it
/// would without the key.
const HELD_SPLIT_SHARES: u64 = 16;

/// What a held-split session (`held-split-builds`) has shown of whom it pays.
///
/// Held on a payout only once its first [`HELD_SPLIT_SHARES`] shares with work all named it, and
/// only while no other has mined on it. A payout remembered from an earlier session
/// (`gateway-scripts.json`) is not taken for this one's: that is whoever dominated then, and
/// taken on trust, one miner who briefly out-mined the operator would be paid every later block.
#[derive(Debug, Default)]
struct HeldPayout {
    /// Shares with work the session has sent.
    shares: u64,
    /// The script it was held on. It stays the session's gateway script once set: every job the
    /// gateway has out pays it, and a share on one of them held to another would be refused.
    on: Option<Vec<u8>>,
}

impl HeldPayout {
    /// The script to leave the session paying itself with, given how many payouts its shares
    /// have named: the one it was held on, while that is the only one. A second ends it for the
    /// session, since a session's payouts are never forgotten.
    fn script(&self, payouts: usize) -> Option<&[u8]> {
        self.on.as_deref().filter(|_| payouts == 1)
    }

    /// Count a share with work. `payouts` is how many payouts the session's shares have named,
    /// this one's included, and `dominant` the leading one's script, if it is an address that is
    /// not the pool's. True if the session is held on it from now.
    fn note(&mut self, payouts: usize, dominant: Option<&[u8]>) -> bool {
        self.shares += 1;
        if self.on.is_some() || payouts != 1 || self.shares < HELD_SPLIT_SHARES {
            return false;
        }
        let Some(script) = dominant else { return false };
        self.on = Some(script.to_vec());
        true
    }

    /// Whether a payout that has come to dominate is kept off the session, and not remembered
    /// for its gateway: it was held on another.
    fn keeps_off(&self, script: &[u8]) -> bool {
        self.on.as_deref().is_some_and(|on| on != script)
    }
}

/// Whether `class-budget` applies to a gateway by its hello: a CONVOY-generation one, and not
/// lazarus-gateway or a lazarus-split build, which place the whole list, nor ratum, whose
/// Partial coinbases are cut by what each template leaves room for rather than by a size class.
/// The house gateway and held-split builds are left out where this is asked.
fn class_budget_applies(generation: Generation, ua: &str) -> bool {
    generation == Generation::Convoy
        && !handshake::is_split_gateway(ua)
        && !ua.trim_start().to_ascii_lowercase().starts_with("ratum")
}

/// The payee bytes a class-limited coinbase kept of the list it was issued, when what it kept is
/// what packing that list in order into some fixed room keeps.
///
/// A payee is measured as `SplitParams::output_budget_bytes` measures one, `8 + 1 + script`, over
/// the issued miner outputs the coinbase pays. A CONVOY size class places the list first-fit and
/// in order: an output that does not fit what is left is skipped and the next one tried, and it
/// stops once under 30 bytes are left (`datum_coinbaser.c` at `b9ea7dc`). Every output it skipped
/// is then bigger than everything it placed after it. A coinbase where that is not so was cut by
/// something other than room (sigops, a gateway picking its own subset), and so was one worth less
/// than the list, whose outputs were dropped for value. Neither says what the class holds: `None`,
/// as for a coinbase that kept all of the list or none of it.
fn kept_payee_bytes(issued: &[Output], pool_script: &[u8], cb: &coinbase::Coinbase) -> Option<usize> {
    let issued_value = issued.iter().fold(0u64, |a, o| a.saturating_add(o.sats));
    if cb.total_output_value() < issued_value {
        return None;
    }
    // walked from the end, so `kept` is what was placed after each output
    let (mut kept, mut skipped) = (0usize, false);
    for o in issued.iter().rev().filter(|o| o.script != pool_script) {
        let need = 8 + 1 + o.script.len();
        if cb.paid_to(&o.script) > 0 {
            kept += need;
        } else if need <= kept {
            return None;
        } else {
            skipped = true;
        }
    }
    (kept > 0 && skipped).then_some(kept)
}

/// The block weight CONVOY fits a coinbase class into (`datum_stratum_coinbase_fit_to_template`
/// in `datum_coinbaser.c`): the consensus limit less the header and coinbase frame it counts. A
/// template of `txn_total_weight` leaves a class `(this - txn_total_weight) / 4` bytes.
const TEMPLATE_WEIGHT_LIMIT: u32 = 4_000_000 - 340 - 36;
/// Room a template must leave beyond the payee bytes a section kept for the cut to have been its
/// class's and not the template's: a class's fixed part (the coinbase frame, scriptSig and pool
/// output, about 250 bytes on a live CONVOY gateway) and one more output, with margin.
const CLASS_BUDGET_TEMPLATE_SLACK: usize = 512;

/// Whether a template of `txn_total_weight` left room enough that a section keeping `kept` payee
/// bytes was cut by its size class, not by the template. Bitcoin Core's default template (4 000
/// weight units kept for the coinbase) leaves about 1 900 bytes, more than a class of 17 outputs
/// needs; one packed to the last few thousand weight units does not, and says nothing about the
/// class. The weight is the gateway's word: a gateway that lies about it can only stop itself
/// teaching a budget, or teach itself a smaller one, as it could with the shares it sends anyway.
fn template_left_room(kept: usize, txn_total_weight: u32) -> bool {
    let left = (TEMPLATE_WEIGHT_LIMIT.saturating_sub(txn_total_weight) / 4) as usize;
    left >= kept + CLASS_BUDGET_TEMPLATE_SLACK
}

/// Distinct coinbasers on which one coinbase section must keep exactly the same payee bytes
/// before `class-budget` takes them as that section's size.
///
/// One sighting is one template. A section can keep less than its class holds for reasons
/// [`kept_payee_bytes`] and [`template_left_room`] cannot see, and templates built one after
/// another off the same mempool are alike. The same bytes on three coinbasers, each asked for a
/// template of its own, is the class. A gateway asks every ten seconds or so, so learning costs
/// well under a minute of Partial work.
const CLASS_BUDGET_SIGHTINGS: usize = 3;
/// The least room for miner outputs in class 1, the smallest CONVOY class that keeps a payee.
///
/// `COINBASE_TYPE_SMALL` is 500 bytes (`datum_stratum.h:55` at b9ea7dc).
/// `datum_stratum_coinbase_fit_to_template` returns `max_sz - fixed_bytes` of that for
/// miner outputs (`datum_coinbaser.c:342`, called with 500 at `:713`). `fixed_bytes` for that
/// class is `119 + pool_script_len + cb_input_sz` when the extranonce fits in the coinbase
/// (`datum_coinbaser.c:708`), and the extranonce fits only while `cb_input_sz <= 85`
/// (`datum_coinbaser.c:563`). A 22-byte pool script then leaves
/// `500 - (119 + 22 + 85) = 274` bytes.
const CLASS_ROOM_MIN_BYTES: usize = 500 - (119 + 22 + 85);
/// The largest payee output a coinbaser lists, measured as CONVOY measures one: the script and
/// 9 bytes (`datum_coinbaser.c:211`). No payee script is over [`address::RDTS_MAX_OUTPUT_SCRIPT`]
/// (34 bytes, P2WSH and P2TR): `address::to_script` gives no other.
const PAYEE_OUTPUT_MAX_BYTES: usize = 8 + 1 + address::RDTS_MAX_OUTPUT_SCRIPT;
/// Payee bytes below which a Partial share is not a CONVOY size class.
///
/// What a section kept is not its room. The class places the list first-fit and skips an
/// output only when it is larger than what is left (`datum_coinbaser.c:211`, and it stops once
/// under 30 bytes are left, `:222`), so a list it cut leaves less than one output of its room
/// unused. A real class 1 therefore keeps more than [`CLASS_ROOM_MIN_BYTES`] less one largest
/// output, `274 - 43 = 231` bytes, and a sighting under that cannot be one. Held to the room
/// itself, a class 1 that kept 246 bytes of 274 (five P2TR payees and a P2WPKH one, the next
/// output too large for the 28 left) was refused for ever.
const CLASS_BUDGET_MIN_BYTES: usize = CLASS_ROOM_MIN_BYTES - PAYEE_OUTPUT_MAX_BYTES;
/// Payee byte counts one section keeps a tally of at once, the oldest dropped first.
const CLASS_BUDGET_TALLY: usize = 8;
/// How long a class budget stands after it was last set before it, and every sighting it was
/// learned from, is forgotten and learned again.
///
/// Nothing else can raise it. A list cut to it is kept whole by every class with that much room,
/// so no share ever shows there is more. One NiceHash miner (CONVOY's class 1, 500 bytes, about
/// 300 of them payees) that mined for a few minutes, or a run of fuller templates that still left
/// room, would otherwise hold every class on the gateway to about 9 payees instead of class 2's
/// 17 for the rest of a session that can last a day, and put about twice the tail into carry on
/// every capped block. Learning again costs under a minute of Partial work, and a class still in
/// use shows its size again straight away.
const CLASS_BUDGET_TTL: Duration = Duration::from_secs(6 * 3600);

/// What `class-budget` has learned of one session's coinbase sections.
///
/// Each accepted Partial share says how many payee bytes its section kept of the list
/// ([`kept_payee_bytes`]). Once a section has kept the same bytes on
/// [`CLASS_BUDGET_SIGHTINGS`] coinbasers, the session's budget is the smaller of that and what it
/// was: one list goes to every class, so it has to fit the smallest. A list whose payees fit in
/// those bytes is kept whole by that section (first-fit into a room at least that large places
/// every output, and the pool's output after them is paid the remainder either way), so a
/// block found on it is Split. The budget only goes down, and only on the same evidence: a
/// share still Partial under it teaches a smaller one. A budget that is too small is safe (more
/// of the list waits in carry), and one too large only leaves blocks Partial, as they were. It is
/// the session's and ends with it, and within it lasts [`CLASS_BUDGET_TTL`] from when it was last
/// set; then it is learned again from the classes miners are using by then.
#[derive(Debug, Default)]
struct ClassBudget {
    /// Per section (the gateway's `cbselect`): payee bytes kept, and the coinbaser ids they were
    /// kept on, up to [`CLASS_BUDGET_SIGHTINGS`], each with when it was seen.
    seen: HashMap<u8, Vec<(usize, Vec<(u8, Instant)>)>>,
    bytes: Option<usize>,
    /// When `bytes` was last set.
    set_at: Option<Instant>,
}

impl ClassBudget {
    /// Forget the budget and everything it was learned from once it is [`CLASS_BUDGET_TTL`] old;
    /// true if it did.
    fn expire(&mut self, now: Instant) -> bool {
        self.drop_old_sightings(now);
        if !self.set_at.is_some_and(|t| now.saturating_duration_since(t) >= CLASS_BUDGET_TTL) {
            return false;
        }
        *self = ClassBudget::default();
        true
    }

    /// Sightings age out on the same TTL as a budget, including before one is set: two
    /// sightings from hours ago do not still count toward the three.
    fn drop_old_sightings(&mut self, now: Instant) {
        for tally in self.seen.values_mut() {
            for (_, ids) in tally.iter_mut() {
                ids.retain(|(_, t)| now.saturating_duration_since(*t) < CLASS_BUDGET_TTL);
            }
            tally.retain(|(_, ids)| !ids.is_empty());
        }
        self.seen.retain(|_, tally| !tally.is_empty());
    }

    /// Count one sighting at `now`; true if it changed the budget.
    fn observe(&mut self, section: u8, coinbaser_id: u8, kept: usize, now: Instant) -> bool {
        if kept < CLASS_BUDGET_MIN_BYTES {
            return false;
        }
        self.drop_old_sightings(now);
        let tally = self.seen.entry(section).or_default();
        let at = match tally.iter().position(|t| t.0 == kept) {
            Some(i) => i,
            None => {
                if tally.len() >= CLASS_BUDGET_TALLY {
                    tally.remove(0);
                }
                tally.push((kept, Vec::new()));
                tally.len() - 1
            }
        };
        let ids = &mut tally[at].1;
        if ids.len() < CLASS_BUDGET_SIGHTINGS && !ids.iter().any(|(id, _)| *id == coinbaser_id) {
            ids.push((coinbaser_id, now));
        }
        if ids.len() < CLASS_BUDGET_SIGHTINGS || self.bytes.is_some_and(|b| b <= kept) {
            return false;
        }
        self.bytes = Some(kept);
        self.set_at = Some(now);
        true
    }
}

/// The split parameters a coinbaser is computed with: the pool's, or for a session with a class
/// budget the same with the payee bytes held to it. Whoever does not fit is `OverBudget` exactly
/// as under the pool's own budget: the order is the same, their earnings go to carry, and the
/// pool's output is still last.
fn class_params(pool: &SplitParams, budget: Option<usize>) -> Cow<'_, SplitParams> {
    match budget {
        Some(b) if b < pool.output_budget_bytes => Cow::Owned(SplitParams { output_budget_bytes: b, ..pool.clone() }),
        _ => Cow::Borrowed(pool),
    }
}

/// The list a coinbaser reply carries for `split`.
fn coinbaser_outputs(split: &Split, pool_script: &[u8]) -> Vec<Output> {
    let mut outputs: Vec<Output> =
        split.payees.iter().map(|p| Output { sats: p.sats, script: p.script.clone() }).collect();
    // The list is complete: the pool's fee and whatever the split could not place go
    // last, to the pool's own script, so the outputs sum to `value`. A stock gateway
    // pays the list verbatim and only appends its own pool output for funds left over
    // (none when the template value matches); lazarus-gateway writes exactly the list,
    // so without this line the fee would be burned. Last, because the size classes a
    // stock gateway builds for small miners keep a prefix of the list, and the pool's
    // remainder is what those may drop.
    if split.pool_sats > 0 || outputs.is_empty() {
        // Also guarantees at least one output: a gateway treats a shorter list as "no
        // coinbaser" and forgets the id.
        outputs.push(Output { sats: split.pool_sats.max(1), script: pool_script.to_vec() });
    }
    outputs
}

const MAX_HELLO: usize = 4096;
const KEEPALIVE: Duration = Duration::from_secs(20);
const IDLE_LIMIT: Duration = Duration::from_secs(300);
const HANDSHAKE_LIMIT: Duration = Duration::from_secs(15);
const COINBASERS_KEPT: usize = 16;
const PENDING_BLOCK_TTL: Duration = Duration::from_secs(120);
const MAX_IDENTITIES: usize = 1 << 16;
/// The identity table's hard ceiling, addresses included (about 100 MB of names).
const MAX_IDENTITIES_HARD: usize = 1 << 20;
/// Job slots whose coinbase sections stay resident per session; see `Session::touch_slot`.
const MAX_LIVE_SLOTS: usize = 16;
/// Coinbaser requests a session may make at once, and how often one is added back. A
/// gateway asks once per template it builds — every ten seconds or so, and on every new
/// block — so a burst of 32 refilled one per second never touches a real one.
const COINBASER_BURST: u32 = 32;
const COINBASER_REFILL: Duration = Duration::from_secs(1);
/// How long stock DATUM's coinbaser thread blocks waiting for a reply before it publishes the
/// job without a split — `datum_protocol_coinbaser_fetch` in `datum_protocol.c`.
const STOCK_COINBASER_DEADLINE: Duration = Duration::from_secs(5);
/// A coinbaser reply this slow is worth a warning: still answered, but the margin against
/// `STOCK_COINBASER_DEADLINE` has gone from a rounding error to a fifth of the budget.
const COINBASER_SLOW: Duration = Duration::from_secs(1);
/// How often one session may repeat the "mining work without the split" warning.
const POOL_ONLY_WARN_EVERY: Duration = Duration::from_secs(300);
/// A session with this many rejects (or malformed messages) inside `REJECT_WINDOW` is
/// doing nothing useful and costing verification CPU: drop it. The live house gateway
/// runs at 13 rejects per 50 000 shares; a broken farm behind one gateway might manage
/// a few a second.
const REJECT_FLOOD: usize = 2_000;
const REJECT_WINDOW: Duration = Duration::from_secs(10);
/// 21 million coins, in sats.
const MAX_MONEY: u64 = 2_100_000_000_000_000;
/// The largest frame a gateway sends while no block of its is pending: a share with both of
/// its sections (two `u16`-length coinbase halves, 255 merkle branches, a username). Only the
/// transactions of a found block need the protocol's full `MAX_CMD_LEN`, and those are asked
/// for. Every open session can make Prime buffer one frame, so this is what 256 of them cost.
const MAX_IDLE_FRAME: usize = 192 * 1024;
/// Handshake refusal when [`Shared::quarantined`] is live. Shown only in Prime's log
/// (`SessionError::Bad`); the 14-space holes were a lost "#419" and a broken line wrap.
const OUTDATED_NODE_REFUSAL: &str = "gateway refused: its node built a block this chain rejected. Upgrade Bitcoin Knots to a build that has the #419 rule (29.4.2 or later) and reconnect — the refusal lifts on its own and an upgraded gateway is taken back straight away";
/// How long after a block candidate a session may still send a full-size frame.
const BLOCK_REPLY_WINDOW: Duration = Duration::from_secs(1800);
/// Blocks past the one it was issued for that a coinbaser is still honoured; see `issued_for`.
pub const COINBASER_GRACE_BLOCKS: u32 = 2;
/// Identities one session's gateway-script vote keeps count of; see `note_identity`.
const MAX_SESSION_IDENTITIES: usize = 1024;
/// What one over-rate coinbaser request with nothing to repeat counts as, in rejects.
const OVER_RATE_COST: usize = 10;
/// How often one session may have the node asked for its tip because its work is ahead of
/// ours. A real gateway is ahead once per block, for a moment.
const AHEAD_REFRESH_EVERY: Duration = Duration::from_secs(1);
/// How often a gateway's template is checked with our node (`validity.rs`), and how often once
/// it has been found invalid or seen building on a block our node rejects: the second is how
/// soon a gateway that fixes its node is credited again.
const TEMPLATE_CHECK_EVERY: Duration = Duration::from_secs(600);
const TEMPLATE_RECHECK_EVERY: Duration = Duration::from_secs(120);
/// The first check waits this long after connect, so a reconnect storm is not a check storm.
const TEMPLATE_CHECK_FIRST: Duration = Duration::from_secs(30);
/// A check whose transactions have not arrived by then is given up (not every gateway answers).
const TEMPLATE_CHECK_TTL: Duration = Duration::from_secs(120);
/// How often one session may repeat that its work is being refused as dead.
const DEAD_WORK_WARN_EVERY: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Wire(#[from] datum_wire::Error),
    #[error("{0}")]
    Bad(&'static str),
    #[error("handshake timed out")]
    HandshakeTimeout,
    #[error("idle")]
    Idle,
    #[error("reject flood: {0} rejected or malformed messages in {1}s")]
    RejectFlood(usize, u64),
}

struct IssuedCoinbaser {
    id: u8,
    value: u64,
    /// The block the template it was asked for builds on, as the request gave it, and the
    /// height that template was for (0 if our node had no tip then). A split is a reading of
    /// the window at one moment and is not good for ever; see `Session::issued_for`.
    prev_hash: [u8; 32],
    height: u32,
    outputs: Vec<Output>,
    payees: Vec<Payee>,
    /// Identities the split could not place, kept so a block found on this coinbaser can
    /// roll their earnings into carry.
    unpaid: Vec<tides::Unpaid>,
    /// DATUM rebate a block found on this coinbaser credits to each DATUM identity's carry,
    /// and the `rebate_owed` accounting that goes with it (see `Split::rebate_delta`).
    rebate_credits: Vec<(String, u64)>,
    rebate_owed_credited: u64,
    rebate_deferred: u64,
    /// The class budget it was held to (`class-budget`), if any.
    class_budget: Option<usize>,
}

/// The parts of a coinbaser a found block settles against: one we still hold, or a fresh split
/// of the live window when the job named one we do not.
struct Coinbaser<'a> {
    value: u64,
    payees: &'a [Payee],
    unpaid: &'a [tides::Unpaid],
    rebate_credits: &'a [(String, u64)],
    rebate_owed_credited: u64,
    rebate_deferred: u64,
    /// It was held to a class budget, so what it deferred for room sits in the pool's output.
    class_capped: bool,
}

/// What a found block does to the books.
struct Settlement {
    kind: &'static str,
    /// What the window is owed because the coinbase did not place these outputs. The make-good
    /// (`lazarus-ops/fee_wallet.py`) pays it from the reserved coinbase once that matures.
    owed: u64,
    /// Every payee the coinbaser issued, identity → sats, scaled to the real reward when the
    /// coinbase paid nobody.
    split: Vec<(String, u64)>,
    /// Carry the coinbase itself handed out; 0 when it placed no payee.
    carry_paid: u64,
    /// `carry_paid` by payee.
    carry_placed: Vec<(String, u64)>,
    carry_delta: Vec<(String, i64)>,
    /// The rebate credits inside `carry_delta`, as they are there (moved with the reward when
    /// the block was priced for it), and their sum.
    rebate_credits: Vec<(String, u64)>,
    rebate_credited: u64,
    rebate_delta: i64,
    /// Of `carry_delta`, what a class-capped coinbaser deferred for room: the earnings its
    /// budget left in the pool's output (`BlockRecord::carry_reserved_sats`).
    carry_reserved: u64,
}

/// Settle a found block against the coinbaser it was mined on.
///
/// Carry comes off the books for *every* payee, not only the ones the coinbase placed. A
/// dropped payee's `sats` includes their carry and `owed` pays that whole figure, so the
/// make-good discharges their carry exactly as the coinbase discharges a placed payee's.
/// Leaving it on the books pays it twice — once in the make-good, once when the next coinbaser
/// hands out a balance that was never cleared. That is what blocks 969973 through 971795 did,
/// about 1.15 XBT of it.
///
/// An orphan reverses the whole `carry_delta` (`node.rs`), which is right either way: the
/// reserved coinbase dies with the block, so no make-good can ever spend it.
fn settle(
    kind: &CoinbaseKind,
    cb: Option<Coinbaser<'_>>,
    coinbase_value: u64,
    paid_to: impl Fn(&[u8]) -> u64,
) -> Settlement {
    let bare = |kind: &'static str| Settlement {
        kind,
        owed: 0,
        split: vec![],
        carry_paid: 0,
        carry_placed: vec![],
        carry_delta: vec![],
        rebate_credits: vec![],
        rebate_credited: 0,
        rebate_delta: 0,
        carry_reserved: 0,
    };
    let name = match kind {
        CoinbaseKind::Split => "split",
        CoinbaseKind::Partial(_) => "partial",
        CoinbaseKind::PoolOnly => "pool-only",
        CoinbaseKind::EmptySolo | CoinbaseKind::GatewaySolo => return bare("solo"),
        CoinbaseKind::Foreign => return bare("unknown"),
    };
    // Nothing to settle against: a full split owes the window nothing, anything else we cannot
    // price, so it is recorded and left alone.
    let Some(cb) = cb else {
        return bare(if matches!(kind, CoinbaseKind::Split) { "split" } else { "unknown" });
    };
    let full_split = matches!(kind, CoinbaseKind::Split);
    let pool_only = matches!(kind, CoinbaseKind::PoolOnly);
    let placed = |p: &Payee| !pool_only && paid_to(&p.script) > 0;
    let carry_delta = tides::split::carry_delta(cb.payees, cb.unpaid, cb.rebate_credits, |_| true);
    let rebate_delta = tides::split::rebate_delta(cb.rebate_owed_credited, cb.rebate_deferred);
    // A class-capped coinbaser's tail: every identity it deferred for room, whose earnings stay
    // in the pool's output and go to their carry. Worked out as `carry_delta` works them out, so
    // the two agree to the sat. A coinbaser held only to the pool's own budget reserves nothing,
    // as it never has.
    let reserved = |each: &dyn Fn(u64) -> u64| -> u64 {
        if !cb.class_capped {
            return 0;
        }
        cb.unpaid
            .iter()
            .filter(|u| u.reason == tides::UnpaidReason::OverBudget && u.defers())
            .fold(0u64, |a, u| a.saturating_add(each(u.earned)))
    };

    // The coinbaser was priced for the value the gateway asked about, and that is the gateway's
    // number: nothing ties it to the template it then mined. Close to it (a template that
    // gained a few fees since), the split's own figures stand, as they always have.
    // A pool-only coinbase paid nobody and owes every figure, so it is always priced for
    // the reward it carried.
    if !pool_only && reward_matches(coinbase_value, cb.value) {
        let carry_placed: Vec<(String, u64)> = cb
            .payees
            .iter()
            .filter(|p| p.carry > 0 && (full_split || placed(p)))
            .map(|p| (p.identity.clone(), p.carry))
            .collect();
        return Settlement {
            kind: name,
            owed: if full_split { 0 } else { cb.payees.iter().filter(|p| !placed(p)).map(|p| p.sats).sum() },
            split: cb.payees.iter().map(|p| (p.identity.clone(), p.sats)).collect(),
            carry_paid: carry_placed.iter().map(|c| c.1).sum(),
            carry_placed,
            carry_delta,
            rebate_credits: cb.rebate_credits.to_vec(),
            rebate_credited: cb.rebate_credits.iter().map(|r| r.1).sum(),
            rebate_delta,
            carry_reserved: reserved(&|e| e),
        };
    }

    // Far from it, the split's figures describe a block that was not mined. Asked about at a
    // hundredth of the real reward, a coinbase paying every miner its dust and the pool the
    // rest "is the split, in full": nothing owed, while the window got nothing. Asked about at
    // a hundred times it, every output scales down to a hundredth and each payee's whole carry
    // is written off against a payment that covered a sliver of it. So price the block for
    // what it was: a miner's earned share moves with the reward, carry is a fixed debt, and
    // whatever the coinbase did not pay of that is owed (which is also what discharges the
    // carry, as for a dropped payee). Earnings and rebate the split deferred move with the
    // reward too.
    let rescale = |sats: u64| scale(sats, coinbase_value, cb.value);
    let entitled = |p: &Payee| rescale(p.sats.saturating_sub(p.carry)).saturating_add(p.carry);
    let paid = |p: &Payee| if pool_only { 0 } else { paid_to(&p.script).min(entitled(p)) };
    let carry_placed: Vec<(String, u64)> = cb
        .payees
        .iter()
        .filter(|p| p.carry > 0 && placed(p))
        .map(|p| (p.identity.clone(), p.carry.min(paid(p))))
        .collect();
    Settlement {
        kind: name,
        owed: cb.payees.iter().map(|p| entitled(p) - paid(p)).sum(),
        split: cb.payees.iter().map(|p| (p.identity.clone(), entitled(p))).collect(),
        carry_paid: carry_placed.iter().map(|c| c.1).sum(),
        carry_placed,
        carry_delta: carry_delta
            .into_iter()
            .map(|(i, d)| if d > 0 { (i, rescale(d as u64).min(i64::MAX as u64) as i64) } else { (i, d) })
            .collect(),
        rebate_credits: cb.rebate_credits.iter().map(|(i, s)| (i.clone(), rescale(*s))).collect(),
        rebate_credited: cb.rebate_credits.iter().map(|r| rescale(r.1)).sum(),
        rebate_delta,
        carry_reserved: reserved(&|e| rescale(e).min(i64::MAX as u64)),
    }
}

/// The carry figures of a block's record once its debits are booked: `carry_paid`, the carry
/// the coinbase handed out that was on the books, and `carry_shortfall_sats`, the carry the
/// coinbaser listed that was not.
///
/// The first counts only payees the coinbase placed (`Settlement::carry_placed`). The second
/// counts every payee, placed or dropped: a dropped payee's whole output is in `owed_sats`, so
/// the make-good pays its carry, and that is a second payment too if the books had none.
fn carry_on_record(carry_placed: &[(String, u64)], books: &tides::Books) -> (u64, u64) {
    let short_of = |identity: &str| books.shortfall.iter().filter(|s| s.0 == identity).map(|s| s.1).sum::<u64>();
    let paid = carry_placed.iter().map(|(identity, carry)| carry.saturating_sub(short_of(identity))).sum();
    (paid, books.shortfall.iter().map(|s| s.1).sum())
}

/// Whether a coinbase worth `actual` is the template a coinbaser issued for `issued` was asked
/// about: no less, and no more than a sixteenth more (fees that arrived since; the same band
/// `classify_coinbase` allows a gateway's own script to take).
fn reward_matches(actual: u64, issued: u64) -> bool {
    issued == 0 || (actual >= issued && actual - issued <= issued / 16)
}

/// How a coinbaser request gets answered. Deliberately has no "drop" variant: a request left
/// unanswered makes a stock gateway publish a coinbase paying only the pool, so the token
/// bucket may choose the cost of the answer but never withhold it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoinbaserAction {
    /// Inside the bucket: compute a fresh split under a new id.
    Fresh,
    /// Over the bucket, and this exact value was already answered: repeat that reply.
    Repeat(u8),
    /// Over the bucket with nothing to repeat: compute anyway, and count it.
    FreshOverRate,
}

/// `budget` is the class budget this reply would be held to: a repeat is only of a reply held to
/// the same one (`class-budget`; always `None` without it).
fn coinbaser_action(
    tokens: u32,
    issued: &VecDeque<IssuedCoinbaser>,
    value: u64,
    budget: Option<usize>,
) -> CoinbaserAction {
    if tokens > 0 {
        return CoinbaserAction::Fresh;
    }
    match issued.iter().rev().find(|c| c.value == value && c.class_budget == budget) {
        Some(c) => CoinbaserAction::Repeat(c.id),
        None => CoinbaserAction::FreshOverRate,
    }
}

struct PendingBlock {
    share: VerifiedShare,
    submit: PowSubmit,
    hash_hex: String,
    at: Instant,
}

/// A share kept to rebuild its job's block once the gateway sends the transactions, so our
/// node can say whether the template is valid; see `Session::maybe_check_template`.
struct TemplateCheck {
    share: VerifiedShare,
    submit: PowSubmit,
    at: Instant,
}

struct Session {
    shared: Arc<Shared>,
    id: u64,
    remote: SocketAddr,
    stream: TcpStream,
    recv_keys: KeyStream,
    send_keys: KeyStream,
    channel: Channel,
    session_key: Identity,
    hello: ClientHello,
    slots: Vec<JobSlot>,
    /// Coinbase section bytes held across all slots, against `cfg.session_coinbase_budget`.
    coinbase_bytes: usize,
    /// Slots in order of their last job change, oldest first.
    live_slots: VecDeque<usize>,
    coinbasers: VecDeque<IssuedCoinbaser>,
    next_coinbaser_id: u8,
    /// Block candidates waiting for the gateway's transaction list, by job id. A job can solve
    /// more than once (regtest does it every share; on mainnet it is rare but a lost block is
    /// the worst outcome), and the gateway's reply names only the job, so keep every candidate
    /// and submit them all from the one transaction set.
    pending_blocks: HashMap<u8, Vec<PendingBlock>>,
    last_send: Instant,
    last_recv: Instant,
    gateway_hex: String,
    /// Last time this session warned that the gateway's chain is not our node's (rate limit).
    chain_warned: Option<Instant>,
    /// When this session last had the node asked for its tip ahead of the poller.
    tip_asked: Option<Instant>,
    /// Token bucket for coinbaser requests; see `on_coinbaser_request`.
    coinbaser_tokens: u32,
    coinbaser_refill_at: Instant,
    /// Timestamps of recent rejects and malformed messages; see `note_reject`.
    recent_rejects: VecDeque<Instant>,
    /// Accepted shares from this session whose coinbase paid only the pool, and when we last
    /// said so; see `note_pool_only_share`.
    pool_only_shares: u64,
    /// Of those, the ones on a full job — the kind that is not stock DATUM's per-height
    /// subsidy-only job and should not be happening.
    pool_only_full_jobs: u64,
    pool_only_warned: Option<Instant>,
    /// When this session last said its shares are credited by hash alone.
    uncommitted_warned: Option<Instant>,
    /// When the house gateway's session last said it is over the reject-flood limit.
    flood_warned: Option<Instant>,
    /// When this session last had a block candidate; see `MAX_IDLE_FRAME`.
    candidate_at: Option<Instant>,
    /// Work seen per share username on this session; the dominant identity is the
    /// gateway's payout for the script-flip.
    identity_work: HashMap<String, u64>,
    gateway_script: Option<Vec<u8>>,
    gateway_identity: Option<String>,
    /// When to send configure(gateway) — only while this tip has no split yet
    /// (just learned the identity). Once a coinbaser lands they stay on pool.
    restore_script_at: Option<tokio::time::Instant>,
    configured_as_gateway: bool,
    /// This tip already has a coinbaser reply, so jobs should be pool/split.
    split_ready: bool,
    /// `Some` when the gateway's build is in `held-split-builds`: it hands its miners the section
    /// paying the configured script whatever split it holds. Once its shares show it has one
    /// payout it is configured with that and left there while no other mines on it; see
    /// `held_script`.
    held: Option<HeldPayout>,
    /// What `class-budget` has learned of this gateway's coinbase sections; `None` without the
    /// key, and for a session it does not apply to (`class_budget_applies`, the house gateway,
    /// a held-split build).
    class_budget: Option<ClassBudget>,
    /// `prev_hash` of the last flushed coinbaser (hex), i.e. the tip that split is for.
    last_split_prev: Option<String>,
    /// Convoy configure v3 resume token; reused for every mid-session configure so the
    /// gateway does not drop its share queue.
    resume_token: [u8; mining::RESUME_TOKEN_LEN],
    /// Test hook: a computed coinbaser sitting until `coinbaser_send_at`. The session
    /// keeps reading shares while it waits; a sleep on this task would starve receipts
    /// and make a stock gateway reconnect.
    pending_coinbaser: Option<(u64, Vec<u8>, [u8; 32])>,
    coinbaser_send_at: Option<tokio::time::Instant>,
    /// This session's row for the clients table, until its first frame earns it a place there.
    row: Option<ClientInfo>,
    /// The template check waiting on the gateway's transactions, by job id.
    template_check: Option<(u8, TemplateCheck)>,
    template_check_due: Instant,
    /// When a template check last asked for transactions; the reply may be a full-size frame.
    template_check_asked: Option<Instant>,
    /// This gateway has built on a block our node rejected or never saw: its node is likely on
    /// the old rules, so its template is checked more often.
    suspect: bool,
    dead_work_warned: Option<Instant>,
}

/// Which of the two gateway-side faults produced a pool-only coinbase, read off the share rather
/// than guessed at.
///
/// The pool cannot fix either, but they are different bugs and the distinction decides where an
/// operator looks. If the coinbaser the job cited carried payees, the gateway had our split and
/// published a section that does not use it — that is stock's unconditional type-0 selection on
/// the first notify of a height, and it is what block 968440 was. If it carried none, the gateway
/// had nothing to place: either it never asked for this job's coinbaser, or it gave up before the
/// reply landed.
fn pool_only_cause(section: u8, coinbaser_id: u8, payees: usize) -> String {
    if payees > 0 {
        format!(
            "it published section {section} while holding coinbaser {coinbaser_id}, which carried {payees} miner \
             output{} — the split was in hand and the section it handed miners did not use it",
            if payees == 1 { "" } else { "s" },
        )
    } else {
        format!(
            "coinbaser {coinbaser_id} named by the job carried no miner outputs here, so the gateway had no split \
             to place when it built section {section} — it either never asked for this job's coinbaser or gave up \
             before our reply landed",
        )
    }
}

/// Where an accepted Partial share was mined: the gateway's own section index (its `cbselect`,
/// the size class), the coinbaser its job cited, and the weight of that job's template.
struct PartialJob {
    section: u8,
    coinbaser_id: u8,
    txn_total_weight: u32,
}

/// What an accepted pool-only share says about why its coinbase had no miner outputs.
struct PoolOnly {
    /// The share was on stock DATUM's per-height subsidy-only job (no transactions).
    subsidy_only: bool,
    txcount: u32,
    /// The gateway's own coinbase section index (`cbselect`) for the work it handed miners.
    section: u8,
    /// The coinbaser id the job named, and how many miner outputs we had issued under it.
    coinbaser_id: u8,
    payees: usize,
}

pub async fn run(shared: Arc<Shared>, mut stream: TcpStream, remote: SocketAddr) -> Result<(), SessionError> {
    let _ = stream.set_nodelay(true);
    let id = shared.next_client_id.fetch_add(1, Ordering::Relaxed);

    // --- hello -------------------------------------------------------------------------
    let mut initial = KeyStream(CLIENT_INITIAL_KEY);
    let (header, payload) =
        match tokio::time::timeout(HANDSHAKE_LIMIT, read_frame(&mut stream, &mut initial, MAX_HELLO)).await {
            Ok(r) => r?,
            Err(_) => return Err(SessionError::HandshakeTimeout),
        };
    if header.cmd != cmd::HELLO || !header.sealed || !header.signed || header.channel {
        return Err(SessionError::Bad("first frame is not a sealed, signed hello"));
    }
    let hello = handshake::parse_client_hello(&shared.pool, &payload)?;
    if shared.cfg.require_split_gateway && !handshake::is_split_gateway(&hello.user_agent) {
        log::warn!(
            "[{id}] {remote} refused ua={:?}: require-split-gateway (need lazarus-gateway or +lazarus-split)",
            hello.user_agent
        );
        return Err(SessionError::Bad("split-only gateway required"));
    }
    let session_key = Identity::generate();
    let (recv_keys, mut send_keys) = KeyStream::from_seed(hello.seed);
    let (send_nonce, recv_nonce) = crypto::session_nonces(hello.seed, &hello.session_sign_pk);
    let channel = Channel::new(session_key.precompute(&hello.session_box_pk), send_nonce, recv_nonce);

    let reply = handshake::build_server_hello(&shared.pool, &session_key, &hello, &shared.cfg.motd);
    let mut h = Header::new(cmd::HELLO_REPLY, reply.len());
    h.sealed = true;
    h.signed = true;
    write_frame(&mut stream, &h, &reply, &mut send_keys).await?;

    let gateway_hex = hex::encode(&hello.identity_sign_pk[..8]);
    let gateway_key = hex::encode(hello.identity_sign_pk);
    if let Some(q) = shared.quarantined(&gateway_key) {
        // Its own node handed us a block the chain refused, so its next one would go the same
        // way and the whole window would pay for it. Nothing it sends is counted meanwhile.
        log::warn!(
            "[{id}] {remote} gateway={gateway_hex} refused: {} (block {}, strike {}); {} min left",
            q.reason,
            q.height,
            q.strikes,
            q.until.saturating_sub(crate::state::now()) / 60
        );
        return Err(SessionError::Bad(OUTDATED_NODE_REFUSAL));
    }
    // Quarantine lapsed: the operator had time to upgrade. Drop a leftover template fault so
    // empty or unanswered templates are not punished until a passing check or a restart.
    if shared.quarantine_lapsed(&gateway_key) {
        if let Some(f) = shared.faults.clear(&gateway_key) {
            log::info!(
                "[{id}] {remote} gateway={gateway_hex}: quarantine lapsed; dropping template fault from height {} ({})",
                f.height,
                f.reason
            );
        }
    }
    let known_script = shared.lookup_gateway_script(&gateway_key);
    let house = house_stratum(&shared.cfg, remote, &gateway_key);
    let front = stratum_front(&shared.cfg, remote, &gateway_key);
    let fee_path = if front || house { "stratum" } else { "datum" };
    log::info!(
        "[{id}] {remote} hello ua={:?} gateway={gateway_hex} gen={:?} fee={fee_path}{}{}",
        hello.user_agent,
        hello.generation,
        if front { " (another pool's stratum front)" } else { "" },
        if hello.resume_token.is_some() { " (asked to resume; declined)" } else { "" }
    );
    // The pool's own gateway carries every stratum miner, and is never left paying itself.
    let held_build = if house { None } else { held_split_build(&shared.cfg.held_split_builds, &hello.user_agent) };
    if let Some(build) = held_build {
        log::info!(
            "[{id}] {remote} gateway={gateway_hex} is a held-split build ({build} in held-split-builds): it hands \
             its miners section 0 whatever split it holds, so once its first {HELD_SPLIT_SHARES} shares all name \
             one payout it is configured with that and left there while no other payout mines on it; until then, \
             and for good once a second one does, its full jobs pay the pool and owe the window"
        );
    }
    // Never the pool's own gateway, which carries every stratum miner and places the whole list,
    // nor a held-split build, which sits on its own script where a dropped pool output pays it.
    let class_budget = (shared.cfg.class_budget
        && !house
        && held_build.is_none()
        && class_budget_applies(hello.generation, &hello.user_agent))
    .then(ClassBudget::default);
    // Shown as a client once it has proved it holds the session key (`Session::establish`). A
    // hello can be replayed by anyone who saw one, and re-sealed to us by any other pool its
    // gateway connects to; whoever does that cannot read our reply or send a frame, but would
    // otherwise put a row here under the gateway's name.
    let row = ClientInfo {
        id,
        remote: remote.to_string(),
        user_agent: hello.user_agent.clone(),
        generation: match hello.generation {
            Generation::Ocean => "ocean",
            Generation::Convoy => "convoy",
        },
        gateway: gateway_hex.clone(),
        connected_ts: now(),
        fee_path: fee_path.into(),
        held_split: held_build.is_some(),
        class_budget_bytes: class_budget.as_ref().map(|_| None),
        class_budget_replies: class_budget.as_ref().map(|_| 0),
        workers: (!house).then(Default::default),
        ..Default::default()
    };
    shared.totals.add(&shared.totals.connections, 1);
    // Removed on drop, so the clients table cannot keep a row for a session that panicked.
    let _row = ClientRow { shared: shared.clone(), id };

    let mut s = Session {
        shared: shared.clone(),
        id,
        remote,
        stream,
        recv_keys,
        send_keys,
        channel,
        session_key,
        hello,
        slots: (0..mining::MAX_JOB_SLOTS).map(|_| JobSlot::default()).collect(),
        coinbase_bytes: 0,
        live_slots: VecDeque::new(),
        coinbasers: VecDeque::new(),
        next_coinbaser_id: 1,
        pending_blocks: HashMap::new(),
        last_send: Instant::now(),
        last_recv: Instant::now(),
        chain_warned: None,
        tip_asked: None,
        gateway_hex,
        coinbaser_tokens: COINBASER_BURST,
        coinbaser_refill_at: Instant::now(),
        recent_rejects: VecDeque::new(),
        pool_only_shares: 0,
        pool_only_full_jobs: 0,
        pool_only_warned: None,
        uncommitted_warned: None,
        flood_warned: None,
        candidate_at: None,
        identity_work: HashMap::new(),
        gateway_script: known_script,
        gateway_identity: None,
        restore_script_at: None,
        configured_as_gateway: false,
        split_ready: false,
        held: held_build.map(|_| HeldPayout::default()),
        class_budget,
        last_split_prev: None,
        resume_token: {
            let mut t = [0u8; mining::RESUME_TOKEN_LEN];
            OsRng.fill_bytes(&mut t);
            t
        },
        pending_coinbaser: None,
        coinbaser_send_at: None,
        template_check: None,
        template_check_due: Instant::now() + TEMPLATE_CHECK_FIRST,
        template_check_asked: None,
        suspect: false,
        dead_work_warned: None,
        row: Some(row),
    };
    s.serve().await
}

struct ClientRow {
    shared: Arc<Shared>,
    id: u64,
}

impl Drop for ClientRow {
    fn drop(&mut self) {
        if let Ok(mut c) = self.shared.clients.lock() {
            c.remove(&self.id);
        }
    }
}

impl Session {
    /// A frame decrypted under the session key: whoever sent the hello holds the key it named.
    fn establish(&mut self) {
        if let Some(row) = self.row.take() {
            self.shared.clients.lock().unwrap().insert(self.id, row);
        }
    }

    fn is_house_stratum(&self) -> bool {
        house_stratum(&self.shared.cfg, self.remote, &self.gateway_key_hex())
    }

    fn is_stratum_front(&self) -> bool {
        stratum_front(&self.shared.cfg, self.remote, &self.gateway_key_hex())
    }

    async fn serve(&mut self) -> Result<(), SessionError> {
        self.send_configure().await?;
        // Ask the gateway to rebuild its templates right now.
        //
        // A gateway that just reconnected is still serving jobs whose coinbases carry a split
        // from coinbaser ids the *previous* Prime process issued. This session has no record of
        // those ids, so every share on one of those jobs pays outputs Prime cannot vouch for and
        // is refused as bad-coinbase-outputs — valid miner work, thrown away, purely because we
        // restarted. A block-notify makes the gateway build fresh jobs and ask for a fresh
        // coinbaser, so it rotates off that stale work in seconds instead of minutes.
        //
        // This is also the only thing Prime can usefully push. An unsolicited *coinbaser* is
        // worse than useless: stock only reads the reply buffer from inside a fetch and only when
        // the value matches the one it asked for, so a pushed split is discarded — and because
        // the buffer index flips and signals the condvar on every reply, one arriving mid-fetch
        // wakes that fetch on the wrong slot and makes it give up with no outputs at all.
        self.send_mining(&mining::block_notify(), false).await?;

        let mut notify = self.shared.notify.subscribe();
        let mut tick = interval(Duration::from_secs(5));
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // `read_buf` is cancel-safe where `read_exact` is not; frames are cut from `inbuf`.
        let mut inbuf = InBuf::default();
        let mut pending: Option<Header> = None;
        loop {
            let restore = self.restore_script_at;
            let cb_at = self.coinbaser_send_at;
            inbuf.make_room();
            tokio::select! {
                n = self.stream.read_buf(&mut inbuf.data) => {
                    if n? == 0 {
                        return Err(SessionError::Io(std::io::ErrorKind::UnexpectedEof.into()));
                    }
                    loop {
                        let h = match pending {
                            Some(h) => h,
                            None => {
                                let Some(hb) = inbuf.take(Header::SIZE) else { break };
                                let h = Header::decode(hb.try_into().unwrap(), &mut self.recv_keys)?;
                                // full size while a found block's transactions are owed, and
                                // for a while after: a slow reply must not cost the connection
                                let block_about = !self.pending_blocks.is_empty()
                                    || self.candidate_at.is_some_and(|t| t.elapsed() < BLOCK_REPLY_WINDOW)
                                    || self.template_check_asked.is_some_and(|t| t.elapsed() < TEMPLATE_CHECK_TTL);
                                let cap = if block_about { MAX_CMD_LEN } else { MAX_IDLE_FRAME };
                                if h.len as usize > cap {
                                    return Err(SessionError::Bad("frame too large"));
                                }
                                pending = Some(h);
                                h
                            }
                        };
                        let Some(payload) = inbuf.take(h.len as usize) else { break };
                        let mut payload = payload.to_vec();
                        pending = None;
                        self.handle_frame(h, &mut payload).await?;
                        // Alive is a whole frame that decrypted, not a byte: a header
                        // promising megabytes and then one byte a minute would otherwise hold
                        // a connection slot, and the buffer behind it, for ever.
                        self.last_recv = Instant::now();
                    }
                }
                n = notify.recv() => {
                    match n {
                        // A find (nonzero id): next work is empty on a new tip — solo.
                        // Node tip (0) or lagged: solo only if this tip has no split yet.
                        // A second notify after we already answered the coinbaser must
                        // not flip them back to gateway.
                        Ok(0) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            if !self.has_split_for_tip() {
                                self.solo_until_split().await?;
                            }
                            self.send_mining(&mining::block_notify(), false).await?;
                        }
                        Ok(_) => {
                            self.solo_until_split().await?;
                            self.send_mining(&mining::block_notify(), false).await?;
                        }
                        Err(_) => {}
                    }
                }
                _ = tick.tick() => {
                    if self.last_recv.elapsed() > IDLE_LIMIT {
                        return Err(SessionError::Idle);
                    }
                    if self.last_send.elapsed() > KEEPALIVE {
                        // an empty INFO frame: 4 bytes, logged by nobody, resets the
                        // gateway's nothing-from-server watchdog
                        let h = Header::new(cmd::INFO, 0);
                        write_frame(&mut self.stream, &h, &[], &mut self.send_keys).await?;
                        self.last_send = Instant::now();
                    }
                    self.pending_blocks.retain(|_, v| {
                        v.retain(|p| p.at.elapsed() < PENDING_BLOCK_TTL);
                        !v.is_empty()
                    });
                }
                _ = async {
                    if let Some(at) = restore {
                        tokio::time::sleep_until(at).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    if let Some(script) = self.gateway_script.clone() {
                        self.send_configure_script(&script, true).await?;
                    }
                    self.restore_script_at = None;
                }
                _ = async {
                    if let Some(at) = cb_at {
                        tokio::time::sleep_until(at).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    self.coinbaser_send_at = None;
                    if let Some((value, encoded, prev)) = self.pending_coinbaser.take() {
                        self.flush_coinbaser_reply(value, &encoded, prev).await?;
                    }
                }
            }
        }
    }

    fn solo_tag(&self) -> String {
        let base = self.shared.cfg.coinbase_tag.as_str();
        if base.ends_with("/solo") {
            base.to_string()
        } else {
            format!("{base}/solo")
        }
    }

    fn configure_body(&self, script: &[u8], tag: &str) -> Vec<u8> {
        let cfg = &self.shared.cfg;
        match self.hello.generation {
            Generation::Ocean => mining::configure_v1(script, cfg.prime_id, tag, cfg.min_diff),
            Generation::Convoy => {
                mining::configure_v3(script, u64::from(cfg.prime_id), &self.resume_token, tag, cfg.min_diff)
            }
        }
    }

    async fn send_configure_script(&mut self, script: &[u8], gateway: bool) -> Result<(), SessionError> {
        let tag = if gateway { self.solo_tag() } else { self.shared.cfg.coinbase_tag.clone() };
        let body = self.configure_body(script, &tag);
        self.send_mining(&body, true).await?;
        self.configured_as_gateway = gateway;
        log::debug!(
            "[{}] configure {} tag={tag} script={}",
            self.id,
            if gateway { "gateway" } else { "pool" },
            hex::encode(&script[..script.len().min(8)]),
        );
        Ok(())
    }

    async fn send_configure(&mut self) -> Result<(), SessionError> {
        if let Some(script) = self.gateway_script.clone() {
            self.send_configure_script(&script, true).await
        } else {
            let pool = self.shared.pool_script.clone();
            self.send_configure_script(&pool, false).await
        }
    }

    fn stay_on_gateway(&self) -> bool {
        self.shared.cfg.stock_full_pool_only == "gateway-solo" && self.pool_only_full_jobs > 0
    }

    /// The class budget this session's next coinbaser is to be held to, carry permitting.
    ///
    /// None while the reply leaves the gateway on its own script (`stay_on_gateway`; a
    /// held-split build never has a class budget). A list cut to a class's room can leave the
    /// pool's output as the one that does not fit, and the gateway pays the remainder to the
    /// script it is configured with: on the pool's, that is the same output, and on its own it
    /// makes the coinbase the gateway's solo work.
    fn reply_class_budget(&self) -> Option<usize> {
        let on_own_script = self.held_script().is_some() || self.stay_on_gateway();
        self.class_budget.as_ref()?.bytes.filter(|_| !on_own_script)
    }

    /// The script a held-split gateway is left paying itself with: its own payout, while its
    /// shares show it is the only one ([`HeldPayout`]).
    ///
    /// The tag that goes with it is the solo one, as for every configure(gateway). A block on
    /// this work pays the gateway alone, and the pool site reads the primary tag to tell the
    /// pool's finds from solo ones: under the pool's own tag it would be booked as a pool block.
    fn held_script(&self) -> Option<Vec<u8>> {
        self.held.as_ref()?.script(self.identity_work.len()).map(<[u8]>::to_vec)
    }

    fn gateway_key_hex(&self) -> String {
        hex::encode(self.hello.identity_sign_pk)
    }

    fn note_identity(&mut self, identity: &str, work: u64) {
        if self.identity_work.len() >= MAX_SESSION_IDENTITIES && !self.identity_work.contains_key(identity) {
            // The vote is for the gateway's own payout address, which is there from the first
            // share; the thousandth distinct username on one session is not a candidate.
            return;
        }
        let w = self.identity_work.entry(identity.to_string()).or_insert(0);
        *w = w.saturating_add(work);
        let Some((dom, _)) = self.identity_work.iter().max_by_key(|(_, w)| *w) else {
            return;
        };
        let dom = dom.clone();
        let script = address::to_script(&dom, self.shared.network).filter(|s| *s != self.shared.pool_script);
        let payouts = self.identity_work.len();
        let held_now = self.held.as_mut().is_some_and(|h| h.note(payouts, script.as_deref()));
        let Some(script) = script else {
            return;
        };
        let changed = self.gateway_script.as_deref() != Some(script.as_slice());
        if self.held.as_ref().is_some_and(|h| h.keeps_off(&script)) {
            // Held on another payout, which stays its script for the rest of the session: every
            // job it has out pays that one, and a share on any of them held to this one would be
            // Foreign and refused. Not remembered either, so its next session is held only on
            // what that session's own shares show.
            return;
        }
        self.gateway_script = Some(script.clone());
        self.gateway_identity = Some(dom.clone());
        self.shared.remember_gateway(&self.gateway_key_hex(), &dom, &script);
        if held_now {
            // `follow_held_split` configures it, split or no split: a reply does not reach the
            // section it mines.
            return;
        }
        if changed && !self.split_ready && !self.configured_as_gateway && self.restore_script_at.is_none() {
            self.restore_script_at = Some(tokio::time::Instant::now());
        }
    }

    /// Put a held-split session where [`Session::held_script`] says after one more share: onto
    /// its own script once its shares show it has one payout, and back onto the pool's once a
    /// second payout mines on it (on this tip's split, if it has one; otherwise with the next
    /// reply, as any session).
    async fn follow_held_split(&mut self, was_held: bool) -> Result<(), SessionError> {
        let payout = self.gateway_identity.clone().unwrap_or_default();
        match (was_held, self.held_script()) {
            (false, Some(script)) => {
                log::info!(
                    "[{}] {} held-split: its first {HELD_SPLIT_SHARES} shares all pay {payout}; configuring it to pay \
                     itself from its next job",
                    self.id,
                    self.gateway_hex
                );
                self.send_configure_script(&script, true).await
            }
            (true, None) => {
                log::info!(
                    "[{}] {} held-split: a second payout mines on it besides {payout}; its full jobs pay the pool and \
                     owe the window again for the rest of the session",
                    self.id,
                    self.gateway_hex
                );
                if self.split_ready && !self.stay_on_gateway() {
                    let pool = self.shared.pool_script.clone();
                    self.send_configure_script(&pool, false).await?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// No split on this tip yet. Known gateways go solo so empty / late jobs
    /// pay them. Unknown ones stay on the pool script until the first share.
    fn has_split_for_tip(&self) -> bool {
        let Some(tip) = self.shared.tip_snapshot() else {
            return false;
        };
        self.split_ready && self.last_split_prev.as_deref() == Some(tip.hash.as_str())
    }

    async fn solo_until_split(&mut self) -> Result<(), SessionError> {
        self.split_ready = false;
        self.last_split_prev = None;
        self.restore_script_at = None;
        if let Some(script) = self.gateway_script.clone() {
            self.send_configure_script(&script, true).await?;
        }
        Ok(())
    }

    /// Encrypt (and optionally sign with the session key) a mining payload and send it.
    async fn send_mining(&mut self, plain: &[u8], signed: bool) -> Result<(), SessionError> {
        let payload = if signed {
            let mut m = Vec::with_capacity(plain.len() + crypto::SIG);
            m.extend_from_slice(plain);
            m.extend_from_slice(&self.session_key.sign(plain));
            self.channel.encrypt(&m)
        } else {
            self.channel.encrypt(plain)
        };
        let mut h = Header::new(cmd::MINING, payload.len());
        h.channel = true;
        h.signed = signed;
        write_frame(&mut self.stream, &h, &payload, &mut self.send_keys).await?;
        self.last_send = Instant::now();
        Ok(())
    }

    async fn handle_frame(&mut self, h: Header, payload: &mut [u8]) -> Result<(), SessionError> {
        if h.sealed {
            return Err(SessionError::Bad("sealed frame after handshake"));
        }
        let mut body: &[u8] = if h.channel { self.channel.decrypt_in_place(payload)? } else { payload };
        if h.signed {
            body = crypto::verify_trailing(&self.hello.session_sign_pk, body)?;
        }
        if h.channel {
            self.establish();
        }
        match h.cmd {
            cmd::MINING => {
                if !h.channel {
                    return Err(SessionError::Bad("plaintext mining frame"));
                }
                match mining::parse_client(body) {
                    Ok(ClientMsg::CoinbaserRequest(r)) => self.on_coinbaser_request(r.value, r.prev_hash).await,
                    Ok(ClientMsg::Pow(p)) => self.on_pow(*p).await,
                    Ok(ClientMsg::JobValidation(v)) => self.on_validation(v).await,
                    Ok(ClientMsg::Unknown(sub)) => {
                        log::debug!("[{}] ignoring unknown mining sub-command 0x{sub:02x}", self.id);
                        Ok(())
                    }
                    Err(e) => {
                        log::debug!("[{}] malformed mining message: {e}", self.id);
                        self.note_reject()
                    }
                }
            }
            cmd::HELLO => Err(SessionError::Bad("second hello")),
            other => {
                log::debug!("[{}] ignoring command {other}", self.id);
                Ok(())
            }
        }
    }

    // --- coinbaser ---------------------------------------------------------------------

    async fn on_coinbaser_request(&mut self, value: u64, prev_hash: [u8; 32]) -> Result<(), SessionError> {
        // A coinbaser request is never left unanswered.
        //
        // Stock DATUM's coinbaser thread sends this request and then blocks on a condvar for
        // five seconds. On timeout it publishes the job with `available_coinbase_outputs_count`
        // still zero, which builds a coinbase paying only the pool script — work that owes the
        // whole reward back to the window if it finds a block. Silence is the one answer that
        // guarantees that outcome, so the bucket below only decides whether the reply is
        // recomputed or repeated, never whether it is sent.
        //
        // This is not why 968440 paid the pool alone — stock selects coinbase type 0 on the
        // first notify of every height regardless of any reply (see the README). It is so that
        // the pool is never the *second* cause of the same thing.
        //
        // For the same reason Prime never sends a coinbaser the gateway did not ask for: the
        // reply is only used when its value equals the requested one, and it lands in a global
        // two-slot buffer whose index flips per reply, so a spurious one can make a legitimate
        // fetch read the wrong value and fall back to exactly the pool-only coinbase this
        // guards against.
        // No template is worth nothing or more than every coin there will ever be. Past that
        // the split's arithmetic is sized for money, not for `u64::MAX`, and the request is
        // not from a gateway: the one kind that goes unanswered.
        if value == 0 || value > MAX_MONEY {
            log::debug!("[{}] coinbaser request for value={value}: not a template", self.id);
            return self.note_reject();
        }
        let started = Instant::now();
        let elapsed = self.coinbaser_refill_at.elapsed();
        if elapsed >= COINBASER_REFILL {
            let n = (elapsed.as_secs_f64() / COINBASER_REFILL.as_secs_f64()) as u32;
            self.coinbaser_tokens = (self.coinbaser_tokens.saturating_add(n)).min(COINBASER_BURST);
            self.coinbaser_refill_at = Instant::now();
        }
        // The class budget this reply is held to, if the session has one and carry is under the
        // ceiling. Settled before the bucket, because a repeat must be of a reply held to the
        // same budget; without a budget to hold it to nothing here runs.
        self.expire_class_budget();
        let learned = self.reply_class_budget();
        let base = learned.map(|_| self.shared.coinbaser_base());
        let budget = learned.filter(|_| base.as_ref().is_some_and(|b| self.shared.class_budget_open(b.total_carry())));
        match coinbaser_action(self.coinbaser_tokens, &self.coinbasers, value, budget) {
            CoinbaserAction::Fresh => self.coinbaser_tokens -= 1,
            CoinbaserAction::Repeat(prev_id) => {
                // Already answered for this exact value: repeat that reply rather than issue a
                // new id. Costs nothing and is what the gateway would have kept anyway. Matched
                // on id *and* value, and never sent empty: a list the gateway reads as shorter
                // than one output is "no coinbaser" to it, which forgets the id.
                let repeat = self
                    .coinbasers
                    .iter()
                    .rev()
                    .find(|c| c.id == prev_id && c.value == value && c.class_budget == budget && !c.outputs.is_empty())
                    .map(|c| coinbaser::encode_v2(c.id, &c.outputs));
                if let Some(repeat) = repeat {
                    log::debug!("[{}] coinbaser over rate; repeating #{prev_id} for value={value}", self.id);
                    self.shared.totals.add(&self.shared.totals.coinbasers_repeated, 1);
                    self.send_coinbaser_reply(value, &repeat, prev_hash).await?;
                    return self.note_coinbaser_latency(started, Some(prev_id));
                }
                self.shared.totals.add(&self.shared.totals.coinbasers_over_rate, 1);
            }
            CoinbaserAction::FreshOverRate => {
                self.shared.totals.add(&self.shared.totals.coinbasers_over_rate, 1);
                // A fresh split is the dearest thing a session can ask for without doing any
                // work (it walks the whole window), and asking about a new value every time
                // gets one computed however empty the bucket is. Still answered, but it counts
                // toward the flood limit at `OVER_RATE_COST` apiece: a gateway this far over
                // (a real one asks a few times a block) is dropped after a couple of hundred.
                for _ in 0..OVER_RATE_COST {
                    self.note_reject()?;
                }
            }
        }

        let id = self.next_coinbaser_id;
        self.next_coinbaser_id = if id == 255 { 1 } else { id + 1 };

        // Computed off a shared snapshot, so a reply never waits on the ledger mutex behind
        // share crediting or the other gateways asking at the same tip change.
        let base = base.unwrap_or_else(|| self.shared.coinbaser_base());
        let split = tides::split::compute(
            base.miners.clone(),
            base.total_work,
            value,
            &class_params(&self.shared.split_params, budget),
            base.rebate_owed,
            now() as u32,
            |ident| base.script_for(ident),
        );
        let (target, total_work) = (base.target_work, base.total_work);
        let outputs = coinbaser_outputs(&split, &self.shared.pool_script);
        let encoded = coinbaser::encode_v2(id, &outputs);
        self.send_coinbaser_reply(value, &encoded, prev_hash).await?;

        let mut ph = prev_hash;
        ph.reverse();
        let class = match (learned, budget) {
            (Some(_), Some(b)) => {
                let over = split.unpaid.iter().filter(|u| u.reason == tides::UnpaidReason::OverBudget).count();
                format!(" class-budget={b} over-budget={over}")
            }
            (Some(b), None) => format!(" class-budget={b} held-off"),
            _ => String::new(),
        };
        log::debug!(
            "[{}] coinbaser #{id} value={value} prev={} outputs={} pool={} carry_paid={} rebate_credit={} rebate_owed_out={} deferred={} window={}/{}{class}",
            self.id,
            &hex::encode(ph)[..16],
            outputs.len(),
            split.pool_sats,
            split.carry_paid,
            split.rebate_sats,
            split.rebate_owed_credited,
            split.unpaid.iter().filter(|u| u.defers()).count(),
            total_work,
            target
        );
        if budget.is_some() {
            self.shared.totals.add(&self.shared.totals.class_budget_replies, 1);
            self.shared.client_update(self.id, |c| *c.class_budget_replies.get_or_insert(0) += 1);
        } else if learned.is_some() {
            self.shared.totals.add(&self.shared.totals.class_budget_ceiling_replies, 1);
        }
        self.coinbasers.push_back(IssuedCoinbaser {
            id,
            value,
            prev_hash,
            height: self.shared.tip_snapshot().map_or(0, |t| t.height + 1),
            outputs,
            payees: split.payees,
            unpaid: split.unpaid,
            rebate_credits: split.rebate_credits,
            rebate_owed_credited: split.rebate_owed_credited,
            rebate_deferred: split.rebate_deferred,
            class_budget: budget,
        });
        while self.coinbasers.len() > COINBASERS_KEPT {
            self.coinbasers.pop_front();
        }
        self.shared.totals.add(&self.shared.totals.coinbasers, 1);
        self.shared.client_update(self.id, |c| c.coinbasers += 1);
        self.note_coinbaser_latency(started, Some(id))
    }

    /// `configure(pool)` then the coinbaser reply. Stay on the pool script after
    /// that — they can mine a split now. Solo returns only on the next tip
    /// (empty work, no coinbaser yet). A test delay, if set, is scheduled on
    /// the session loop so shares and keepalives still flow; the 5 s fetch
    /// then times out still holding the gateway script.
    async fn send_coinbaser_reply(
        &mut self,
        value: u64,
        encoded: &[u8],
        prev_hash: [u8; 32],
    ) -> Result<(), SessionError> {
        let delay = Duration::from_millis(self.shared.cfg.coinbaser_delay_ms);
        if !delay.is_zero() {
            log::info!("[{}] delaying coinbaser reply {} ms (test hook)", self.id, delay.as_millis());
            self.pending_coinbaser = Some((value, encoded.to_vec(), prev_hash));
            if self.coinbaser_send_at.is_none() {
                self.coinbaser_send_at = Some(tokio::time::Instant::now() + delay);
            }
            return Ok(());
        }
        self.flush_coinbaser_reply(value, encoded, prev_hash).await
    }

    async fn flush_coinbaser_reply(
        &mut self,
        value: u64,
        encoded: &[u8],
        prev_hash: [u8; 32],
    ) -> Result<(), SessionError> {
        self.restore_script_at = None;
        if let Some(script) = self.held_script() {
            // A held-split build hands its miners the section paying the configured script
            // whatever this reply says. Turned back to the pool here, that section is a
            // pool-only coinbase until the next tip; left on the gateway it is the gateway's own.
            // The reply still goes out, the same as anyone's, for whatever else it builds.
            if !self.configured_as_gateway {
                self.send_configure_script(&script, true).await?;
            }
        } else if !self.stay_on_gateway() {
            let pool = self.shared.pool_script.clone();
            self.send_configure_script(&pool, false).await?;
        }
        self.send_mining(&mining::coinbaser_reply(value, encoded), false).await?;
        self.split_ready = true;
        let mut ph = prev_hash;
        ph.reverse();
        self.last_split_prev = Some(hex::encode(ph));
        Ok(())
    }

    /// Record how long a coinbaser reply took to reach the wire, and say so if it came close
    /// to the five seconds a stock gateway waits before giving up and mining a pool-only
    /// coinbase. This is the number that goes wrong first when the pool is the reason a
    /// gateway published unsplit work.
    fn note_coinbaser_latency(&mut self, started: Instant, id: Option<u8>) -> Result<(), SessionError> {
        let took = started.elapsed();
        self.shared.totals.raise(&self.shared.totals.coinbaser_max_us, took.as_micros() as u64);
        if took >= COINBASER_SLOW {
            self.shared.totals.add(&self.shared.totals.coinbasers_slow, 1);
            log::warn!(
                "[{}] {} coinbaser{} took {} ms: a stock gateway gives up at {} s and then mines a coinbase paying only the pool",
                self.id,
                self.gateway_hex,
                id.map(|i| format!(" #{i}")).unwrap_or_default(),
                took.as_millis(),
                STOCK_COINBASER_DEADLINE.as_secs(),
            );
        }
        Ok(())
    }

    fn issued(&self, id: u8) -> Option<&IssuedCoinbaser> {
        self.coinbasers.iter().rev().find(|c| c.id == id)
    }

    /// The coinbaser a job may be held to: the one it names, if it was issued for the block
    /// the job builds on, or no more than a block before it. The id is the gateway's to choose
    /// and the last `COINBASERS_KEPT` replies stay in hand, so with no limit a gateway could
    /// ask once while its share of the window was at its best and mine every later block
    /// against that reading. A couple of blocks of grace, because a gateway racing a new tip can
    /// publish its first job there on the split it already had, and that is a lag, not a lie:
    /// refusing it would throw away honest miners' shares over nothing.
    fn issued_for(&self, id: u8, prev_hash: &[u8; 32], height: u32) -> Option<&IssuedCoinbaser> {
        self.issued(id)
            .filter(|c| &c.prev_hash == prev_hash || (c.height > 0 && height <= c.height + COINBASER_GRACE_BLOCKS))
    }

    /// Undo a coinbase section this share brought in (it pushed the session over budget).
    fn drop_coinbase(&mut self, job_id: usize, added: Option<u8>) {
        let Some(id) = added else { return };
        let before = self.slots[job_id].coinbase_bytes();
        self.slots[job_id].forget_coinbase(id);
        let after = self.slots[job_id].coinbase_bytes();
        self.coinbase_bytes = self.coinbase_bytes.saturating_sub(before.saturating_sub(after));
    }

    /// Note that `job_id` just started a new job, and evict the sections of the slot that
    /// has gone longest without one once more than `MAX_LIVE_SLOTS` hold any. A stock gateway
    /// rotates eight slots and never reaches this; lazarus-gateway walks all 255 but resends
    /// its sections with every share, so an evicted slot simply refills when next used.
    fn touch_slot(&mut self, job_id: usize) {
        self.live_slots.retain(|&s| s != job_id);
        self.live_slots.push_back(job_id);
        while self.live_slots.len() > MAX_LIVE_SLOTS {
            if let Some(old) = self.live_slots.pop_front() {
                self.slots[old].evict_sections();
            }
        }
    }

    // --- shares ------------------------------------------------------------------------

    async fn on_pow(&mut self, s: PowSubmit) -> Result<(), SessionError> {
        let job_id = usize::from(s.job_id);
        if job_id >= self.slots.len() {
            return self.reject(&s, mining::REJECT_BAD_JOB_ID).await;
        }
        // Bech32 folds to one case so one payout address is one TIDES row (see
        // `canonical_identity`); base58 and non-addresses are kept byte-exact.
        let identity = address::canonical_identity(address::identity_of(&s.username));
        if identity.is_empty() || identity.len() > 128 || !identity.bytes().all(|b| b.is_ascii_graphic()) {
            return self.reject(&s, mining::REJECT_BAD_USERNAME).await;
        }

        // Job/coinbase sections, then staleness before any hashing. What a session can make
        // Prime hold is bounded three ways: a section has a size cap and a slot an id cap
        // (`JobSlot::absorb`), only the `MAX_LIVE_SLOTS` most recently (re)started slots keep
        // their sections, and the bytes across slots are budgeted. A stock gateway sends
        // each section once per job and never again — even after a reject — so a section is
        // never dropped just because the share carrying it failed.
        let held_before = self.slots[job_id].coinbase_bytes();
        let absorbed = match self.slots[job_id].absorb(&s) {
            Ok(a) => a,
            Err(code) => return self.reject(&s, code).await,
        };
        if absorbed.job_changed {
            self.touch_slot(job_id);
            self.coinbase_bytes = self.slots.iter().map(JobSlot::coinbase_bytes).sum();
        } else {
            let held_after = self.slots[job_id].coinbase_bytes();
            self.coinbase_bytes = self.coinbase_bytes.saturating_sub(held_before).saturating_add(held_after);
        }
        if absorbed.coinbase_added.is_some() && self.coinbase_bytes > self.shared.cfg.session_coinbase_budget {
            self.drop_coinbase(job_id, absorbed.coinbase_added);
            log::warn!(
                "[{}] {} over the coinbase budget ({} bytes across slots); refusing section",
                self.id,
                self.remote,
                self.shared.cfg.session_coinbase_budget
            );
            return self.reject(&s, mining::REJECT_COINBASE_TOO_LARGE).await;
        }
        let (height, coinbaser_id, prev_hash, nbits, txn_total_weight) = match &self.slots[job_id].job {
            Some(j) => (j.height, j.coinbaser_id, j.prev_hash, j.nbits_u32(), j.txn_total_weight),
            None => return self.reject(&s, mining::REJECT_BAD_JOB_ID).await,
        };
        // The job section is the gateway's account of the chain; the node's is the one that
        // counts. Height, parent and target are all held to it before any work is credited.
        // Until the node has answered once there is no chain to hold a job to. That is this
        // pool's node being slow to start, not anything a gateway did, so their work is still
        // taken and credited (it is real work: the hash is checked either way). What is not
        // taken on a gateway's word is a *block*: see `held_to_chain` below.
        let tip = self.shared.tip_snapshot();
        let held_to_chain = tip.is_some();
        let mut chain_check = None;
        if let Some(mut tip) = tip {
            let grace = Duration::from_secs(u64::from(self.shared.cfg.stale_grace_secs));
            let mut check = tip.check_job(&prev_hash, height, nbits, grace);
            let ahead = matches!(check, JobCheck::Ahead | JobCheck::AheadByOne);
            if ahead && self.tip_asked.is_none_or(|t| t.elapsed() > AHEAD_REFRESH_EVERY) {
                // A gateway's node can have the next block before our poller does. Ask now
                // rather than go on working from a tip that may be a block old.
                self.tip_asked = Some(Instant::now());
                node::refresh_ahead(&self.shared).await;
                if let Some(t) = self.shared.tip_snapshot() {
                    check = t.check_job(&prev_hash, height, nbits, grace);
                    tip = t;
                }
            }
            let warn = self.chain_warned.is_none_or(|t| t.elapsed() > Duration::from_secs(60));
            let code = match check {
                JobCheck::Current | JobCheck::AheadByOne => None,
                JobCheck::OtherBranch => {
                    // Taken: its node is on a tip ours does not have, which is what a fork
                    // looks like from here. Worth a line, because it is also what a gateway
                    // inventing parents looks like, and what our own node looks like when it
                    // is the one on the losing side.
                    if warn {
                        self.chain_warned = Some(Instant::now());
                        let mut prev = prev_hash;
                        prev.reverse();
                        log::warn!(
                            "[{}] {} is working at height {height} on {}, where our node has {} at {}: a competing tip, or a node out of step. Its work is taken.",
                            self.id,
                            self.gateway_hex,
                            hex::encode(prev),
                            tip.hash,
                            tip.height,
                        );
                    }
                    None
                }
                JobCheck::Stale => Some(mining::REJECT_STALE_BLOCK),
                JobCheck::Ahead => {
                    // Two or more past our tip, after asking the node. That is our node lagging
                    // (peers, sync, or an isolated node) or a gateway making heights up; say
                    // so, once a minute, because if it is the node then every share from
                    // every healthy gateway is being refused meanwhile.
                    if warn {
                        self.chain_warned = Some(Instant::now());
                        log::warn!(
                            "[{}] {} submits work for height {height} but our node's tip is {}: the pool node is behind the gateway's node; check its peers and sync. Rejecting as stale until it catches up.",
                            self.id, self.gateway_hex, tip.height
                        );
                    }
                    Some(mining::REJECT_STALE_BLOCK)
                }
                JobCheck::WrongBits => {
                    if warn {
                        self.chain_warned = Some(Instant::now());
                        log::warn!(
                            "[{}] {} submits work for height {height} with nbits {nbits:08x}, a target the chain cannot have set there (tip {} bits {}, next bits {}). Rejecting: under it every share would be a block.",
                            self.id,
                            self.gateway_hex,
                            tip.height,
                            tip.bits.map_or("unknown".into(), |b| format!("{b:08x}")),
                            tip.next_bits.map_or("unknown".into(), |b| format!("{b:08x}")),
                        );
                    }
                    Some(mining::REJECT_TARGET_MISMATCH)
                }
            };
            debug_assert_eq!(code.is_none(), check.taken());
            if let Some(code) = code {
                return self.reject(&s, code).await;
            }
            chain_check = Some(check);
        }
        // Taken above as possibly the winning side of a race; our node says whether that block
        // can be one. Work on a block it rejected, or has not seen in a while, can never pay.
        if matches!(chain_check, Some(JobCheck::OtherBranch | JobCheck::AheadByOne)) {
            let verdict = validity::parent_verdict(&self.shared, &prev_hash).await;
            if verdict != ParentVerdict::Take {
                return self.refuse_dead_parent(&s, &prev_hash, height, verdict).await;
            }
        }

        let issued_outputs = self.issued_for(coinbaser_id, &prev_hash, height).map(|c| c.outputs.clone());
        let pool_script = self.shared.pool_script.clone();
        let gateway_script = self.gateway_script.clone();
        let policy = Policy {
            pool_script: &pool_script,
            issued: issued_outputs.as_deref(),
            tolerance: self.shared.cfg.split_tolerance,
            now: now() as u32,
            min_pot: self.shared.cfg.min_pot(),
            gateway_script: gateway_script.as_deref(),
            empty_solo_fee_bps: self.shared.cfg.empty_solo_fee_bps,
            trusted_target: self.is_house_stratum(),
            uncommitted_pot: self.shared.cfg.uncommitted_pot,
            held_split: self.held.is_some(),
        };
        let v = match verify::verify(&mut self.slots[job_id], &s, &policy) {
            Ok(v) => v,
            Err(code) => return self.reject(&s, code).await,
        };
        // One credit per hash, pool-wide and for as long as the height is live: the set is
        // shared, keyed by height, and never cleared by anything a gateway can send. A share
        // that earned nothing (credited by hash alone, and its hash fell short) has no credit
        // to take twice and is kept out of the set, which is what makes the set, the ledger
        // and the identity table cost a hash at the floor to touch rather than any hash.
        let seen = if v.work == 0 && !v.is_block_candidate {
            Seen::Fresh
        } else {
            self.shared.seen.lock().unwrap().insert(v.height, v.hash)
        };
        match seen {
            Seen::Fresh => {}
            Seen::Duplicate => return self.reject(&s, mining::REJECT_DUPLICATE_WORK).await,
            Seen::Full => {
                log::warn!(
                    "[{}] share set full at height {}; refusing work rather than forgetting any",
                    self.id,
                    v.height
                );
                return self.reject(&s, mining::REJECT_OTHER).await;
            }
        }

        // A gateway whose template our node found invalid earns nothing until one passes. The
        // pool's own gateway builds on our node, so its templates are the node's own.
        if !self.is_house_stratum() {
            if let Some(fault) = self.shared.faults.get(&self.gateway_key_hex()) {
                return self.refuse_faulted(s, v, fault, chain_check, prev_hash, identity, held_to_chain).await;
            }
        }

        // credit — empty-solo and gateway-solo are accepted work but not window work.
        if v.work > 0 {
            let was_held = self.held_script().is_some();
            self.note_identity(&identity, v.work);
            if self.held.is_some() {
                self.follow_held_split(was_held).await?;
            }
        }
        let ts = now();
        let solo = matches!(v.coinbase_kind, CoinbaseKind::EmptySolo | CoinbaseKind::GatewaySolo);
        let credited = if solo {
            true
        } else {
            let mut ledger = self.shared.ledger.lock().unwrap();
            let known = ledger.window.identities().len();
            let new = known >= MAX_IDENTITIES && ledger.window.work_of(&identity) == 0;
            // Past the first limit only an address gets a new row; past the second nothing
            // does. Rows are never reclaimed, and at the floor each costs real work, so the
            // second limit is an attack in progress rather than a busy day.
            let refused = new
                && v.work > 0
                && (known >= MAX_IDENTITIES_HARD || address::to_script(&identity, self.shared.network).is_none());
            if refused {
                false
            } else {
                // nothing to write for a share credited by its hash alone that earned nothing
                // this time (`Policy::uncommitted_pot`); it is accepted like any other
                if v.work > 0 {
                    // DATUM, house stratum, or house stratum inside the address's grace. Work
                    // relayed by another pool's stratum front is stratum work with no grace,
                    // and leaves no trace in the grace book: it is not a sighting on DATUM.
                    let source = if self.is_stratum_front() {
                        tides::SOURCE_STRATUM
                    } else {
                        ledger.source_for(&identity, ts as u32, self.is_house_stratum(), &self.shared.cfg.grace())
                    };
                    if let Err(e) = ledger.credit(&identity, v.work, v.height, ts as u32, source) {
                        log::error!("ledger write failed: {e}");
                    }
                }
                true
            }
        };
        if !credited {
            return self.reject(&s, mining::REJECT_BAD_USERNAME).await;
        }
        self.shared.totals.add(&self.shared.totals.accepted, 1);
        if solo {
            match v.coinbase_kind {
                CoinbaseKind::EmptySolo => {
                    self.shared.totals.add(&self.shared.totals.solo_empty_shares, 1);
                    self.shared.totals.add(&self.shared.totals.solo_empty_work, v.work);
                    self.shared.client_update(self.id, |c| {
                        c.solo_empty_shares += 1;
                        c.solo_empty_work += v.work;
                    });
                }
                CoinbaseKind::GatewaySolo => {
                    self.shared.totals.add(&self.shared.totals.solo_full_shares, 1);
                    self.shared.totals.add(&self.shared.totals.solo_full_work, v.work);
                    self.shared.client_update(self.id, |c| {
                        c.solo_full_shares += 1;
                        c.solo_full_work += v.work;
                    });
                }
                _ => {}
            }
        } else {
            self.shared.totals.add(&self.shared.totals.work, v.work);
            if let (CoinbaseKind::Split, Some(gw)) = (&v.coinbase_kind, gateway_script.as_deref()) {
                let paid_gw = v.coinbase.paid_to(gw);
                let issued_gw: u64 = issued_outputs
                    .as_ref()
                    .map(|o| o.iter().filter(|x| x.script == gw).map(|x| x.sats).sum())
                    .unwrap_or(0);
                if paid_gw > issued_gw {
                    self.shared.totals.add(&self.shared.totals.remainder_to_gateway_sats, paid_gw - issued_gw);
                }
            }
        }
        self.shared.client_update(self.id, |c| {
            c.accepted += 1;
            if !solo {
                c.work += v.work;
                // Display only: which machine a share named. Nothing reads it but the stats.
                if v.work > 0 {
                    if let Some(w) = c.workers.as_mut() {
                        w.note(&identity, &s.username, v.work, ts);
                    }
                }
            }
            c.last_share_ts = ts;
            c.identity = identity.clone();
            if !self.is_house_stratum() {
                let tag = coinbase::secondary_tag(&v.coinbase.script_sig);
                if !tag.is_empty() {
                    c.secondary_tag = tag;
                }
            }
        });
        // A share under min-diff earns nothing. Counting it would let one ground hash teach a budget.
        if matches!(v.coinbase_kind, CoinbaseKind::Partial(_)) && v.work > 0 {
            let job = PartialJob { section: s.coinbase_id, coinbaser_id, txn_total_weight };
            self.note_partial_share(job, issued_outputs.as_deref(), &v.coinbase);
        }
        let status = if matches!(v.coinbase_kind, CoinbaseKind::Split) {
            mining::ACCEPTED
        } else {
            mining::ACCEPTED_TENTATIVELY
        };
        if matches!(v.coinbase_kind, CoinbaseKind::PoolOnly) {
            // Why this job had no miner outputs is the whole question, and the share answers it:
            // `s.coinbase_id` is the gateway's own section index (its `cbselect`), and the
            // coinbaser we issued for the job says whether it had a split to place at all.
            let payees = self.issued_for(coinbaser_id, &prev_hash, height).map_or(0, |c| c.payees.len());
            self.note_pool_only_share(PoolOnly {
                subsidy_only: s.subsidy_only(),
                txcount: v.commitment.txcount,
                section: s.coinbase_id,
                coinbaser_id,
                payees,
            });
        }
        self.maybe_check_template(chain_check, &prev_hash, &s, &v).await?;

        if !v.target_committed {
            self.note_uncommitted_share();
        } else if v.target_pot < self.shared.cfg.min_pot() {
            self.shared.totals.add(&self.shared.totals.below_floor_shares, 1);
        }
        // Book a found block before the receipt. The receipt is what a gateway treats as
        // "this share is in", and a kill in the moment after it is what the books must already
        // show: the carry taken off, and the block record on disk.
        let receipt = mining::share_receipt(status, 0, s.nonce32, s.target_pot, s.job_id);
        if v.is_block_candidate {
            if held_to_chain {
                self.on_block_candidate(s, v, identity).await?;
            } else {
                // Its `nbits` is whatever the job said, so "meets nbits" proves nothing: at
                // regtest's target every share does. A candidate moves carry balances the
                // moment it is recorded, and that is not done on a gateway's word. The gateway
                // submits its own blocks to its own node regardless.
                log::warn!(
                    "[{}] {} share at height {} meets its job's nbits, but our node has given no tip to hold that job to: not recorded as a block (check rpc)",
                    self.id, self.gateway_hex, v.height
                );
            }
        }
        self.send_mining(&receipt, false).await?;
        Ok(())
    }

    /// Say, now and then, that this gateway's shares are being credited by their hash alone.
    fn note_uncommitted_share(&mut self) {
        self.shared.totals.add(&self.shared.totals.uncommitted_shares, 1);
        if self.uncommitted_warned.is_some_and(|t| t.elapsed() < POOL_ONLY_WARN_EVERY) {
            return;
        }
        self.uncommitted_warned = Some(Instant::now());
        log::warn!(
            "[{}] {} {} ua={:?}: the difficulty of its shares is not part of what they hash (no target byte where the scriptSig's unique-id push puts one), so they are credited by hash alone at difficulty 2^{}: fair on average up to that difficulty, lumpier than a gateway that commits it. lazarus-gateway away from the pool host does this; if it is the pool's own, give its full key in house-gateways.",
            self.id,
            self.remote,
            self.gateway_hex,
            self.hello.user_agent,
            self.shared.cfg.uncommitted_pot.max(self.shared.cfg.min_pot()),
        );
    }

    async fn reject(&mut self, s: &PowSubmit, code: u16) -> Result<(), SessionError> {
        self.refuse(s, code).await?;
        self.note_reject()
    }

    /// Answer a share with a reject that is about the work, not the message: counted like any
    /// reject but not against the flood limit. A gateway on a dead chain sends nothing else until
    /// its node moves, and hanging up on it would only stop us noticing when it does.
    async fn refuse(&mut self, s: &PowSubmit, code: u16) -> Result<(), SessionError> {
        let name = mining::reject_name(code);
        log::debug!("[{}] reject job={} user={:?} pot={}: {name}", self.id, s.job_id, s.username, s.target_pot);
        self.shared.totals.add(&self.shared.totals.rejected, 1);
        self.shared.client_update(self.id, |c| {
            c.rejected += 1;
            c.last_reject = Some(name);
        });
        self.send_mining(&mining::share_receipt(mining::REJECTED, code, s.nonce32, s.target_pot, s.job_id), false).await
    }

    async fn refuse_dead_parent(
        &mut self,
        s: &PowSubmit,
        prev: &[u8; 32],
        height: u32,
        verdict: ParentVerdict,
    ) -> Result<(), SessionError> {
        // Its node follows a chain ours rejects: check its template as soon as it is back on ours.
        if !self.suspect {
            self.suspect = true;
            self.template_check_due = Instant::now();
        }
        self.shared.totals.add(&self.shared.totals.dead_parent_shares, 1);
        self.shared.client_update(self.id, |c| c.dead_parent_shares += 1);
        if self.dead_work_warned.is_none_or(|t| t.elapsed() > DEAD_WORK_WARN_EVERY) {
            self.dead_work_warned = Some(Instant::now());
            let mut be = *prev;
            be.reverse();
            log::warn!(
                "[{}] {} {} is working at height {height} on {}: {}. Nothing built on it can be a block, so its shares are refused, not credited. Its node has most likely not upgraded to the long coinbase maturity rules (Bitcoin Knots 29.4.2, from block 973,440).",
                self.id,
                self.gateway_hex,
                self.hello.user_agent,
                hex::encode(be),
                verdict.why(),
            );
        }
        self.refuse(s, mining::REJECT_STALE_BLOCK).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn refuse_faulted(
        &mut self,
        s: PowSubmit,
        v: VerifiedShare,
        fault: Fault,
        chain_check: Option<JobCheck>,
        prev_hash: [u8; 32],
        identity: String,
        held_to_chain: bool,
    ) -> Result<(), SessionError> {
        self.shared.totals.add(&self.shared.totals.faulted_shares, 1);
        let label = format!("{} at {}", fault.reason, fault.height);
        self.shared.client_update(self.id, |c| {
            c.faulted_shares += 1;
            c.template_fault = Some(label);
        });
        if self.dead_work_warned.is_none_or(|t| t.elapsed() > DEAD_WORK_WARN_EVERY) {
            self.dead_work_warned = Some(Instant::now());
            log::warn!(
                "[{}] {} {}: refusing its shares, not crediting them: our node found its template invalid at height {} ({}, from a {}). They are credited again once a template of its passes (checked every {}s).",
                self.id,
                self.gateway_hex,
                self.hello.user_agent,
                fault.height,
                fault.reason,
                fault.found_by,
                TEMPLATE_RECHECK_EVERY.as_secs(),
            );
        }
        self.refuse(&s, mining::REJECT_OTHER).await?;
        self.maybe_check_template(chain_check, &prev_hash, &s, &v).await?;
        // The fault may be stale (the gateway fixed its node since): a block it finds is still
        // a block, and if our node takes it, it pays the window like any other.
        if v.is_block_candidate && held_to_chain {
            self.on_block_candidate(s, v, identity).await?;
        }
        Ok(())
    }

    /// Ask the gateway for this job's transactions, now and then, so our node can check the
    /// template the work is on (`check_template`). Only for a job with transactions, built on
    /// our node's tip: a proposal on any other parent is inconclusive, and one without
    /// transactions proves nothing about the gateway's mempool.
    async fn maybe_check_template(
        &mut self,
        chain_check: Option<JobCheck>,
        prev_hash: &[u8; 32],
        s: &PowSubmit,
        v: &VerifiedShare,
    ) -> Result<(), SessionError> {
        if self.is_house_stratum() || chain_check != Some(JobCheck::Current) || v.commitment.txcount <= 1 {
            return Ok(());
        }
        let now = Instant::now();
        if now < self.template_check_due
            || self.template_check.as_ref().is_some_and(|(_, c)| c.at.elapsed() < TEMPLATE_CHECK_TTL)
            || self.pending_blocks.contains_key(&s.job_id)
            || self.shared.tip_snapshot().and_then(|t| t.hash_le).as_ref() != Some(prev_hash)
        {
            return Ok(());
        }
        let faulted = self.shared.faults.get(&self.gateway_key_hex()).is_some();
        self.template_check_due =
            now + if faulted || self.suspect { TEMPLATE_RECHECK_EVERY } else { TEMPLATE_CHECK_EVERY };
        self.template_check = Some((s.job_id, TemplateCheck { share: v.clone(), submit: s.clone(), at: now }));
        self.template_check_asked = Some(now);
        self.send_mining(&mining::request_full_block(s.job_id), false).await
    }

    /// Have our node validate the block this share's job would make (`getblocktemplate` in
    /// proposal mode: every consensus check but the proof of work). A template that fails for
    /// its transactions faults the gateway; one that passes clears it.
    fn check_template(&self, c: TemplateCheck, txns: &[Vec<u8>]) {
        let block = verify::assemble_block(&c.share, &c.submit, txns);
        let hex_block = hex::encode(&block);
        let shared = self.shared.clone();
        let (id, gw, key, height) = (self.id, self.gateway_hex.clone(), self.gateway_key_hex(), c.share.height);
        let job = validity::job_token(c.submit.job_id, height);
        tokio::spawn(async move {
            shared.totals.add(&shared.totals.template_checks, 1);
            let verdict = shared
                .rpc
                .call("getblocktemplate", serde_json::json!([{ "mode": "proposal", "data": hex_block }]))
                .await;
            match verdict {
                Ok(serde_json::Value::Null) => {
                    log::debug!("[{id}] {gw}: template at height {height} passes");
                    clear_fault(&shared, id, &gw, &key, height, "passes our node's checks");
                }
                Ok(serde_json::Value::String(reason)) if validity::template_fault(&reason) => {
                    shared.totals.add(&shared.totals.template_checks_failed, 1);
                    mark_faulted(&shared, id, &gw, &key, &reason, job, height, "template check");
                }
                Ok(other) => log::debug!("[{id}] {gw}: template check at height {height} inconclusive: {other}"),
                Err(e) => log::debug!("[{id}] {gw}: template check at height {height} could not run: {e}"),
            }
        });
    }

    /// Note an accepted share whose coinbase paid only the pool script.
    ///
    /// The share is good work and is credited; what it cannot do is pay the window if it turns
    /// out to be a block, because the coinbase it committed to has no miner outputs. A gateway
    /// publishing that work says so over thousands of shares before it gets lucky, so this is
    /// the signal to act on — 968440 was found on a job like this and nothing in the log said
    /// so beforehand.
    ///
    /// The two kinds are worth telling apart by cost. Stock DATUM emits one subsidy-only job per
    /// height (`JOB_STATE_EMPTY_PLUS`, no transactions, coinbase id `0xff`), which is cheap to
    /// lose. A *full* job with a pool-only coinbase is the expensive one: a whole template's
    /// fees and subsidy with no miner outputs, which is what 968440 was.
    fn note_pool_only_share(&mut self, p: PoolOnly) {
        let PoolOnly { subsidy_only, txcount, section, coinbaser_id, payees } = p;
        self.pool_only_shares += 1;
        self.shared.totals.add(&self.shared.totals.pool_only_shares, 1);
        if !subsidy_only {
            self.pool_only_full_jobs += 1;
            self.shared.totals.add(&self.shared.totals.pool_only_full_jobs, 1);
        }
        let (n, full) = (self.pool_only_shares, self.pool_only_full_jobs);
        self.shared.client_update(self.id, |c| {
            c.pool_only_shares = n;
            c.pool_only_full_jobs = full;
        });
        if self.pool_only_warned.is_some_and(|t| t.elapsed() < POOL_ONLY_WARN_EVERY) {
            return;
        }
        self.pool_only_warned = Some(Instant::now());
        if full == 0 {
            // Every one so far is the per-height subsidy-only job every stock gateway sends.
            log::warn!(
                "[{}] {} {} has {n} share{} on a subsidy-only coinbase paying just the pool. Stock DATUM emits one \
                 such job per height, so a low count here is expected; a block found on one owes the window.",
                self.id,
                self.remote,
                self.gateway_hex,
                if n == 1 { "" } else { "s" },
            );
            return;
        }
        let cause = pool_only_cause(section, coinbaser_id, payees);
        log::warn!(
            "[{}] {} {} is mining FULL jobs whose coinbase pays only the pool ({full} of {n} pool-only share{}, \
             latest carried {} transactions): a whole template's fees and subsidy with no miner outputs, so a \
             block found on it owes the window everything — 968440 was one of these. Diagnosis: {cause}. Either \
             way it is fixed at the gateway: lazarus/patches/datum-gateway-split-only.patch, or lazarus-gateway.",
            self.id,
            self.remote,
            self.gateway_hex,
            if n == 1 { "" } else { "s" },
            txcount.saturating_sub(1),
        );
    }

    /// Learn this session's class budget from an accepted Partial share (`class-budget`): the
    /// payee bytes its section kept of the list its job was issued.
    fn note_partial_share(&mut self, job: PartialJob, issued: Option<&[Output]>, cb: &coinbase::Coinbase) {
        let PartialJob { section, coinbaser_id, txn_total_weight } = job;
        if self.class_budget.is_none() {
            return;
        }
        let Some(issued) = issued else { return };
        // Every reply goes out with configure(pool) under the pool's own tag; a section built
        // under the solo tag had a scriptSig five bytes longer, and less room to keep the list in.
        let solo = self.solo_tag();
        if cb.script_sig.windows(solo.len()).any(|w| w == solo.as_bytes()) {
            return;
        }
        let Some(kept) = kept_payee_bytes(issued, &self.shared.pool_script, cb) else { return };
        if !template_left_room(kept, txn_total_weight) {
            return;
        }
        self.expire_class_budget();
        let Some(budget) = self.class_budget.as_mut() else { return };
        if !budget.observe(section, coinbaser_id, kept, Instant::now()) {
            return;
        }
        log::info!(
            "[{}] {} class budget {kept} bytes: section {section} kept the same {kept} bytes of payee outputs on \
             {CLASS_BUDGET_SIGHTINGS} coinbasers, so this session's coinbasers now list only the payees that fit \
             in {kept} bytes and the rest wait in carry",
            self.id,
            self.gateway_hex,
        );
        self.shared.client_update(self.id, |c| c.class_budget_bytes = Some(Some(kept)));
    }

    /// Forget this session's class budget once it is [`CLASS_BUDGET_TTL`] old, so that it is
    /// learned again from the classes its miners use now.
    fn expire_class_budget(&mut self) {
        let Some(budget) = self.class_budget.as_mut() else { return };
        let was = budget.bytes;
        if !budget.expire(Instant::now()) {
            return;
        }
        log::info!(
            "[{}] {} class budget {} bytes is {} h old: forgotten with what it was learned from, so coinbasers \
             go out at the pool's budget until this session's Partial shares show it again",
            self.id,
            self.gateway_hex,
            was.unwrap_or(0),
            CLASS_BUDGET_TTL.as_secs() / 3600,
        );
        self.shared.client_update(self.id, |c| c.class_budget_bytes = Some(None));
    }

    /// Count a reject or malformed message against the session's flood budget.
    fn note_reject(&mut self) -> Result<(), SessionError> {
        let now = Instant::now();
        self.recent_rejects.push_back(now);
        while self.recent_rejects.front().is_some_and(|t| now.duration_since(*t) > REJECT_WINDOW) {
            self.recent_rejects.pop_front();
        }
        if self.recent_rejects.len() > REJECT_FLOOD {
            // The pool's own gateway is one session carrying every stratum miner, and what it
            // forwards is whatever they send it. Hanging up on it because one of them found a
            // way to make it forward refusable work would disconnect all of them to punish
            // one; say so instead, and let the gateway deal with its miner.
            if self.is_house_stratum() {
                if self.flood_warned.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
                    self.flood_warned = Some(now);
                    log::warn!(
                        "[{}] the house gateway has had {} shares refused in {}s (latest: see its last_reject); not dropping it, but one of its miners is flooding",
                        self.id,
                        self.recent_rejects.len(),
                        REJECT_WINDOW.as_secs()
                    );
                }
                self.recent_rejects.clear();
                return Ok(());
            }
            return Err(SessionError::RejectFlood(self.recent_rejects.len(), REJECT_WINDOW.as_secs()));
        }
        Ok(())
    }

    // --- blocks ------------------------------------------------------------------------

    async fn on_block_candidate(&mut self, s: PowSubmit, v: VerifiedShare, finder: String) -> Result<(), SessionError> {
        let mut disp = v.hash;
        disp.reverse();
        let hash_hex = hex::encode(disp);
        log::info!(
            "[{}] BLOCK CANDIDATE from {} height={} hash={hash_hex} coinbase={:?} value={} finder={finder} gateway={}",
            self.id,
            self.remote,
            v.height,
            v.coinbase_kind,
            v.coinbase_value,
            self.gateway_hex
        );
        self.candidate_at = Some(Instant::now());
        self.shared.totals.add(&self.shared.totals.block_candidates, 1);
        self.shared.client_update(self.id, |c| c.block_candidates += 1);

        // what the window is owed if this coinbase did not pay the full split
        let job_cb = self.slots[usize::from(s.job_id)].job.as_ref().map(|j| (j.coinbaser_id, j.prev_hash, j.height));
        let issued = job_cb.and_then(|(id, prev, height)| self.issued_for(id, &prev, height));
        let fee = tides::split::fee_for(v.coinbase_value, self.shared.cfg.fee_bps);
        // Carry accounting for this block: minus the carry each placed output included, plus
        // what every identity the split could not place earned. Applied now — the next
        // coinbaser must not hand out the same carry twice — and reversed if the block is
        // orphaned (node.rs).
        // The DATUM rebate is not an output: the coinbase paid the pool the whole stratum fee,
        // and the block's `rebate_credits` now join each DATUM identity's carry (inside
        // `carry_delta`, so an orphan reverses them with everything else). The `rebate_owed`
        // balance drops by what was credited out of it and grows by a rebate with nobody to
        // credit. Split, partial and pool-only blocks all credit alike: the pool holds the fee
        // either way.
        // A pool-only coinbase whose job named no coinbaser we still hold: split the live
        // window instead, so the block owes the window what it would have paid.
        let live = if issued.is_none() && matches!(v.coinbase_kind, CoinbaseKind::PoolOnly) {
            let ledger = self.shared.ledger.lock().unwrap();
            let net = self.shared.network;
            Some(
                ledger
                    .window
                    .split(v.coinbase_value, &self.shared.split_params, now() as u32, |i| address::to_script(i, net)),
            )
        } else {
            None
        };
        let cb = match (issued, live.as_ref()) {
            (Some(c), _) => Some(Coinbaser {
                value: c.value,
                payees: &c.payees,
                unpaid: &c.unpaid,
                rebate_credits: &c.rebate_credits,
                rebate_owed_credited: c.rebate_owed_credited,
                rebate_deferred: c.rebate_deferred,
                class_capped: c.class_budget.is_some(),
            }),
            (None, Some(sp)) => Some(Coinbaser {
                value: sp.value,
                payees: &sp.payees,
                unpaid: &sp.unpaid,
                rebate_credits: &sp.rebate_credits,
                rebate_owed_credited: sp.rebate_owed_credited,
                rebate_deferred: sp.rebate_deferred,
                class_capped: false,
            }),
            (None, None) => None,
        };
        let (rebate_owed_credited, rebate_deferred) =
            cb.as_ref().map_or((0, 0), |c| (c.rebate_owed_credited, c.rebate_deferred));
        let Settlement {
            kind,
            owed,
            split,
            mut carry_paid,
            carry_placed,
            mut carry_delta,
            rebate_credits,
            mut rebate_credited,
            rebate_delta,
            carry_reserved,
        } = settle(&v.coinbase_kind, cb, v.coinbase_value, |script| v.coinbase.paid_to(script));
        let _ = fee;
        // Booked in two steps (`tides::Books`). Now: what this coinbase took off the books, so
        // that the coinbaser computed a few seconds from now cannot hand the same carry out
        // again. When the node has the block in its main chain (`node::confirm_blocks`): what
        // it put on them. Until then this is a share that met its job's target, and balances
        // are not granted on that.
        let settles = !carry_delta.is_empty() || rebate_credited > 0 || rebate_delta != 0;
        let mut books = tides::Books::new(
            if settles { rebate_owed_credited } else { 0 },
            if settles { rebate_deferred } else { 0 },
        );
        let mut carry_shortfall_sats = 0u64;
        if settles {
            let (total, holders) = self.shared.book_block_debits(&hash_hex, &carry_delta, &mut books);
            // carry_paid is what the coinbase paid of carry the books held, not what an old
            // coinbaser still listed. What they did not hold was paid a second time and is a debt.
            let booked: u64 = books.debited.iter().map(|d| d.1).sum();
            let listed = carry_paid;
            (carry_paid, carry_shortfall_sats) = carry_on_record(&carry_placed, &books);
            if carry_shortfall_sats > 0 {
                log::error!(
                    "[{}] block {hash_hex} was mined on a coinbaser listing carry the books no longer held for {} \
                     payee(s): its coinbase paid {listed} sats of carry, {carry_paid} of it on the books, and \
                     {carry_shortfall_sats} sats in all were paid a second time (by this coinbase out of the pool's \
                     remainder, or by the make-good for an output it dropped) and are a debt against those payees' \
                     future earnings",
                    self.id,
                    books.shortfall.len(),
                );
            }
            if let Some(c) = issued {
                if c.rebate_owed_credited > books.rebate_debited {
                    // Only the owed rebate an earlier block drew comes off. The rest of these
                    // credits is the rebate this block's own stratum fee paid the pool.
                    let left = tides::cap_rebate_to_owed_drawn(
                        &mut carry_delta,
                        &rebate_credits,
                        c.rebate_owed_credited,
                        books.rebate_debited,
                    );
                    log::error!(
                        "[{}] block {hash_hex} coinbaser credited {} sats of owed DATUM rebate but only {} was still \
                         owed; {} sats of that credit are not put on",
                        self.id,
                        c.rebate_owed_credited,
                        books.rebate_debited,
                        c.rebate_owed_credited - books.rebate_debited,
                    );
                    rebate_credited = left;
                }
            }
            let waiting: i64 = carry_delta.iter().map(|d| d.1.max(0)).sum();
            log::info!(
                "[{}] block {hash_hex} carry: {booked} sats of carry for {} payees ({carry_paid} of it paid in this coinbase) and {} sats of owed DATUM rebate drawn, off the books now; {waiting} sats of deferred earnings and rebate credits ({} entries) and {rebate_deferred} sats of undistributed rebate go on when the node confirms it; pool now holds {total} sats of carry for {holders} miners",
                self.id,
                books.debited.len(),
                books.rebate_debited,
                carry_delta.iter().filter(|d| d.1 > 0).count(),
            );
        }
        if carry_reserved > 0 {
            log::info!(
                "[{}] block {hash_hex} was mined on a coinbaser held to this gateway's class budget: {carry_reserved} \
                 sats its budget had no room for are in the pool's output and go to their earners' carry \
                 (carry_reserved_sats; the fee wallet holds them back)",
                self.id,
            );
        }
        let record = BlockRecord {
            ts: now(),
            height: v.height,
            hash: hash_hex.clone(),
            finder: Some(finder),
            coinbase_value: v.coinbase_value,
            kind: kind.into(),
            owed_sats: owed,
            split,
            pool_sats: v.paid_to_pool,
            carry_paid,
            carry_delta,
            rebate_credited,
            rebate_delta,
            settled: false,
            submit: "pending".into(),
            gateway: self.gateway_hex.clone(),
            books: Some(books),
            carry_shortfall_sats,
            carry_reserved_sats: carry_reserved,
        };
        self.shared.record_block(record);

        // ask for the transactions so we can submit the block ourselves as a backup
        let job = s.job_id;
        self.pending_blocks.entry(job).or_default().push(PendingBlock {
            share: v,
            submit: s,
            hash_hex,
            at: Instant::now(),
        });
        self.send_mining(&mining::request_full_block(job), false).await?;
        // every other gateway should refresh its template now
        let _ = self.shared.notify.send(self.id as u32);
        Ok(())
    }

    async fn on_validation(&mut self, v: JobValidationReply) -> Result<(), SessionError> {
        let job = v.job();
        let pending = self.pending_blocks.remove(&job);
        let check = match self.template_check.take() {
            Some((j, c)) if j == job => Some(c),
            other => {
                self.template_check = other;
                None
            }
        };
        if pending.is_none() && check.is_none() {
            log::debug!("[{}] unsolicited validation reply for job {job}", self.id);
            return Ok(());
        }
        let (status, txns) = match v {
            JobValidationReply::FullBlock { status, txns, .. } => (status, txns),
            JobValidationReply::Transactions { status, txns, .. } => (status, txns),
            JobValidationReply::ShortIds { .. } => {
                if let Some(p) = pending {
                    self.pending_blocks.insert(job, p);
                }
                if let Some(c) = check {
                    self.template_check = Some((job, c));
                }
                return Ok(());
            }
        };
        if status != ValidationStatus::Ok {
            if check.is_some() {
                log::debug!(
                    "[{}] {} could not send job {job}'s transactions for a template check: {status:?}",
                    self.id,
                    self.gateway_hex
                );
            }
            for p in pending.iter().flatten() {
                log::warn!(
                    "[{}] gateway could not supply transactions for block {}: {:?}",
                    self.id,
                    p.hash_hex,
                    status
                );
                self.shared.update_block(&p.hash_hex, |r| r.submit = "no-transactions".into());
            }
            return Ok(());
        }
        // one transaction set per job; every candidate solved on this job assembles from it
        for p in pending.into_iter().flatten() {
            self.submit_candidate(p, &txns);
        }
        if let Some(c) = check {
            self.check_template(c, &txns);
        }
        Ok(())
    }

    fn submit_candidate(&self, pending: PendingBlock, txns: &[Vec<u8>]) {
        let expected = pending.share.commitment.txcount.saturating_sub(1) as usize;
        if txns.len() != expected && txns.len() != pending.share.commitment.txcount as usize {
            log::warn!(
                "[{}] block {}: gateway sent {} transactions, header commits to {}",
                self.id,
                pending.hash_hex,
                txns.len(),
                pending.share.commitment.txcount
            );
        }
        let block = verify::assemble_block(&pending.share, &pending.submit, txns);
        let hex_block = hex::encode(&block);
        let shared = self.shared.clone();
        let hash_hex = pending.hash_hex;
        let id = self.id;
        let (gw, key, house) = (self.gateway_hex.clone(), self.gateway_key_hex(), self.is_house_stratum());
        let share_height = pending.share.height;
        let job = validity::job_token(pending.submit.job_id, share_height);
        tokio::spawn(async move {
            let outcome = match shared.rpc.submitblock(&hex_block).await {
                Ok(serde_json::Value::Null) => "accepted".to_string(),
                Ok(serde_json::Value::String(s)) => s,
                Ok(other) => other.to_string(),
                Err(e) => format!("rejected: {e}"),
            };
            log::info!("[{id}] submitblock {hash_hex} ({} bytes): {outcome}", block.len());
            shared.totals.add(&shared.totals.blocks_submitted, 1);
            let invalid = node::says_invalid(&outcome);
            // Two verdicts on one submit: the template fault (which also stops crediting shares
            // from this gateway until a later template passes) and, when the reason says its node
            // is behind a consensus rule, the quarantine that keeps it off until it upgrades.
            let outdated = node::says_outdated_node(&outcome);
            if !house {
                if validity::template_fault(&outcome) {
                    mark_faulted(&shared, id, &gw, &key, &outcome, job, share_height, "found block");
                } else if outcome == "accepted" {
                    clear_fault(&shared, id, &gw, &key, share_height, "found a block our node accepted");
                }
            }
            let height = shared.update_block(&hash_hex, |r| r.submit = outcome.clone()).map(|r| r.height);
            // The house gateway builds on this pool's own node, so it can never be the one behind.
            if outdated && !house {
                let height = height.unwrap_or(0);
                if let Some(span) = shared.quarantine_gateway(&key, height, &outcome) {
                    log::error!(
                        "[{id}] gateway={gw} built block {height} that the chain rejected ({outcome}); \
                         refusing its work for {} min — its Bitcoin Knots is behind a consensus rule. \
                         Upgrading and reconnecting after that is all it takes to come back",
                        span / 60
                    );
                } else {
                    log::error!(
                        "[{id}] gateway={gw} built block {height} that the chain rejected ({outcome}); \
                         quarantine is off, so its work is still accepted"
                    );
                }
            }
            if let (true, Some(height)) = (invalid, height) {
                // The node has looked at the block and said no. There is nothing to wait for:
                // what it took off the books goes back now, not six blocks from now.
                node::mark_orphan(&shared, &hash_hex, height, "was refused by the node as invalid");
            }
        });
    }
}

/// Stop crediting a gateway whose template our node found invalid; see `validity::Faults`.
fn mark_faulted(
    shared: &Shared,
    id: u64,
    gw: &str,
    key: &str,
    reason: &str,
    job: u32,
    height: u32,
    found_by: &'static str,
) {
    if shared.faults.note_fail(key, reason, job, height, found_by) {
        log::warn!(
            "[{id}] {gw}: our node finds its template invalid at height {height} ({reason}, from a {found_by}). Its node builds blocks the network rejects, most likely because it has not upgraded to the long coinbase maturity rules (Bitcoin Knots 29.4.2, from block 973,440). Its shares are refused, not credited, until a template of its passes."
        );
    } else if shared.faults.get(key).is_none() {
        log::info!(
            "[{id}] {gw}: template at height {height} failed ({reason}, from a {found_by}); waiting for a second failing check on a different job before refusing its work"
        );
        return;
    }
    let label = format!("{reason} at {height}");
    shared.client_update(id, |c| c.template_fault = Some(label));
}

fn clear_fault(shared: &Shared, id: u64, gw: &str, key: &str, height: u32, why: &str) {
    if let Some(f) = shared.faults.clear(key) {
        log::info!(
            "[{id}] {gw}: its template at height {height} {why}; crediting its work again (refused since height {} for {})",
            f.height,
            f.reason
        );
    }
    shared.client_update(id, |c| c.template_fault = None);
}

fn scale(sats: u64, value: u64, issued_value: u64) -> u64 {
    if issued_value == 0 {
        return sats;
    }
    ((u128::from(sats) * u128::from(value)) / u128::from(issued_value)) as u64
}

// --- framing -----------------------------------------------------------------------------

#[derive(Default)]
struct InBuf {
    data: Vec<u8>,
    pos: usize,
}

impl InBuf {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        if self.data.len() - self.pos < n {
            return None;
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Some(s)
    }

    fn make_room(&mut self) {
        if self.pos > 0 && self.pos >= self.data.len() / 2 {
            self.data.drain(..self.pos);
            self.pos = 0;
        }
        if self.data.capacity() - self.data.len() < 4096 {
            self.data.reserve(16 * 1024);
        }
    }
}

/// Handshake-only reader; runs under a timeout outside the main select loop.
async fn read_frame(
    stream: &mut TcpStream,
    keys: &mut KeyStream,
    max: usize,
) -> Result<(Header, Vec<u8>), SessionError> {
    let mut hb = [0u8; Header::SIZE];
    stream.read_exact(&mut hb).await?;
    let h = Header::decode(hb, keys)?;
    let len = h.len as usize;
    if len > max {
        return Err(SessionError::Bad("frame too large"));
    }
    let mut payload = vec![0u8; len];
    if len > 0 {
        stream.read_exact(&mut payload).await?;
    }
    Ok((h, payload))
}

async fn write_frame(
    stream: &mut TcpStream,
    h: &Header,
    payload: &[u8],
    keys: &mut KeyStream,
) -> Result<(), SessionError> {
    let mut out = Vec::with_capacity(Header::SIZE + payload.len());
    out.extend_from_slice(&h.encode(keys));
    out.extend_from_slice(payload);
    stream.write_all(&out).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payee(identity: &str, sats: u64, carry: u64, tag: u8) -> Payee {
        Payee { identity: identity.into(), work: 1, sats, carry, script: vec![0x00, 0x14, tag] }
    }

    fn coinbaser<'a>(value: u64, payees: &'a [Payee]) -> Coinbaser<'a> {
        Coinbaser {
            value,
            payees,
            unpaid: &[],
            rebate_credits: &[],
            rebate_owed_credited: 0,
            rebate_deferred: 0,
            class_capped: false,
        }
    }

    fn cleared(s: &Settlement) -> std::collections::HashMap<&str, i64> {
        s.carry_delta.iter().map(|(i, d)| (i.as_str(), *d)).collect()
    }

    /// A coinbaser is priced for the value the gateway asked about, which is the gateway's
    /// number. A block is settled for the reward it actually carried.
    #[test]
    fn a_block_is_settled_for_the_reward_it_carried_not_the_one_asked_about() {
        let real = 312_500_000u64;
        // earned 1 000 000 / 500 000 of a block worth `asked`, plus A's 400 000 of carry
        let split_at = |asked: u64| vec![payee("A", asked / 250 + 400_000, 400_000, 1), payee("B", asked / 500, 0, 2)];

        // near enough (a template that gained fees since) is settled on the split's own figures
        let payees = split_at(real);
        let s = settle(&CoinbaseKind::Split, Some(coinbaser(real, &payees)), real + real / 100, |_| 1);
        assert_eq!((s.owed, s.carry_paid), (0, 400_000));
        assert_eq!(s.split, vec![("A".to_string(), 1_650_000), ("B".to_string(), 625_000)]);

        // Asked about at a hundredth of the reward: each miner gets its dust, the pool the rest,
        // and that classifies as the split in full. The window is owed the difference.
        let tiny = split_at(real / 100);
        let paid_dust = |script: &[u8]| tiny.iter().find(|p| p.script == script).map_or(0, |p| p.sats);
        let s = settle(&CoinbaseKind::Split, Some(coinbaser(real / 100, &tiny)), real, paid_dust);
        assert_eq!(
            s.split,
            vec![("A".to_string(), 1_650_000), ("B".to_string(), 625_000)],
            "what they earned of the real block"
        );
        assert_eq!(s.owed, (1_650_000 - 412_500) + (625_000 - 6_250), "less the dust they were paid");
        assert_eq!(cleared(&s).get("A"), Some(&-400_000), "A's carry is discharged: paid in part, owed the rest");

        // Asked about at a hundred times the reward and scaled down to fit: A is paid a
        // hundredth of an output that was mostly carry. What is left of it is still owed.
        let huge = split_at(real * 100);
        let scaled = |script: &[u8]| huge.iter().find(|p| p.script == script).map_or(0, |p| p.sats / 100);
        let s = settle(&CoinbaseKind::Split, Some(coinbaser(real * 100, &huge)), real, scaled);
        assert_eq!(s.split, vec![("A".to_string(), 1_650_000), ("B".to_string(), 625_000)]);
        let a_paid = (real * 100 / 250 + 400_000) / 100;
        assert_eq!(s.owed, 1_650_000 - a_paid, "B was paid in full; A's carry was not");
        assert_eq!(s.carry_paid, 400_000.min(a_paid));

        // earnings the split deferred, and rebate it credited, move with the reward as well
        let unpaid = [tides::Unpaid {
            identity: "C".into(),
            sats: 30_000,
            earned: 30_000,
            reason: tides::UnpaidReason::BelowMinimum,
        }];
        let credits = [("D".to_string(), 50_000u64)];
        let cb = Coinbaser { unpaid: &unpaid, rebate_credits: &credits, ..coinbaser(real * 100, &huge) };
        let s = settle(&CoinbaseKind::Split, Some(cb), real, scaled);
        assert_eq!((cleared(&s).get("C"), cleared(&s).get("D")), (Some(&300), Some(&500)));
        assert_eq!(s.rebate_credited, 500);

        assert!(reward_matches(real, real) && reward_matches(real + real / 16, real) && reward_matches(5, 0));
        assert!(!reward_matches(real - 1, real) && !reward_matches(real + real / 16 + 1, real));
    }

    /// The bug that cost about 1.15 XBT between 969973 and 971795. A gateway that carries only
    /// the head of the coinbase leaves the rest to the make-good, which pays each dropped payee
    /// their whole output — carry included. If that carry stays on the books the next coinbaser
    /// hands out the same balance again and the pool pays twice.
    #[test]
    fn a_partial_discharges_the_carry_of_the_payees_it_dropped() {
        let payees =
            vec![payee("A", 1_000_000, 400_000, 1), payee("B", 800_000, 300_000, 2), payee("C", 600_000, 0, 3)];
        let s = settle(&CoinbaseKind::Partial(1), Some(coinbaser(312_500_000, &payees)), 312_500_000, |script| {
            if script == [0x00, 0x14, 1] {
                1_000_000
            } else {
                0
            }
        });
        assert_eq!(s.kind, "partial");
        assert_eq!(s.owed, 1_400_000, "B and C are owed their whole outputs");
        assert_eq!(s.carry_paid, 400_000, "only A's carry rode an output the coinbase actually paid");
        let d = cleared(&s);
        assert_eq!(d.get("A"), Some(&-400_000), "placed payee's carry was handed out");
        assert_eq!(d.get("B"), Some(&-300_000), "dropped payee's carry is paid by the make-good, so it must come off");
        assert_eq!(d.get("C"), None, "C carried nothing to discharge");
    }

    /// The review's R5. A Partial block on a coinbaser from before the last find: A's output
    /// was placed, but its carry had been paid already; B's was dropped and its carry was
    /// still on the books. B's debit is not A's payment: the record says the coinbase paid no
    /// carry the books held, and that all of A's was paid a second time.
    #[test]
    fn a_partial_on_an_old_coinbaser_records_the_placed_payee_s_shortfall() {
        let dir = std::env::temp_dir().join(format!("primed-r5-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut ledger = tides::Ledger::open(&dir).unwrap();
        ledger.set_carry("A", 400_000);
        ledger.set_carry("B", 300_000);
        let payees =
            vec![payee("A", 1_000_000, 400_000, 1), payee("B", 800_000, 300_000, 2), payee("C", 600_000, 0, 3)];
        let only_a = |script: &[u8]| if script == [0x00, 0x14, 1] { 1_000_000 } else { 0 };
        let partial = || settle(&CoinbaseKind::Partial(1), Some(coinbaser(312_500_000, &payees)), 312_500_000, only_a);

        // on the books as the coinbaser saw them: the figures are the settlement's own
        let s = partial();
        let mut first = tides::Books::new(0, 0);
        ledger.book_debits(&s.carry_delta, &mut first);
        assert_eq!(s.carry_placed, vec![("A".to_string(), 400_000)]);
        assert_eq!(carry_on_record(&s.carry_placed, &first), (400_000, 0), "B's carry left the books unpaid by it");

        // B earns 300 000 again; a second block is then found on the same coinbaser
        ledger.set_carry("B", 300_000);
        let s = partial();
        let mut second = tides::Books::new(0, 0);
        ledger.book_debits(&s.carry_delta, &mut second);
        assert_eq!(second.debited, vec![("B".to_string(), 300_000)]);
        assert_eq!(second.shortfall, vec![("A".to_string(), 400_000)]);
        assert_eq!(carry_on_record(&s.carry_placed, &second), (0, 400_000));
        assert_eq!(ledger.window.debt_of("A"), 400_000);

        // a dropped payee's carry the books did not hold is a second payment as well: the
        // make-good pays its whole output
        let s = partial();
        let mut third = tides::Books::new(0, 0);
        ledger.book_debits(&s.carry_delta, &mut third);
        assert_eq!(carry_on_record(&s.carry_placed, &third), (0, 700_000));

        // part of a placed payee's carry on the books
        ledger.set_carry("A", 150_000);
        let s = partial();
        let mut fourth = tides::Books::new(0, 0);
        ledger.book_debits(&s.carry_delta, &mut fourth);
        assert_eq!(carry_on_record(&s.carry_placed, &fourth), (150_000, 250_000 + 300_000));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pool-only coinbase places nobody, so the make-good owes the whole split — scaled to the
    /// reward the block really carried — and every payee's carry goes with it.
    #[test]
    fn a_pool_only_discharges_every_carry_and_scales_what_it_owes() {
        let payees = vec![payee("A", 1_000_000, 400_000, 1), payee("B", 800_000, 300_000, 2)];
        let s = settle(&CoinbaseKind::PoolOnly, Some(coinbaser(200_000_000, &payees)), 100_000_000, |_| 0);
        // Half the assumed reward, nobody paid. What each earned of this block halves with it;
        // the carry riding the same output is a debt from earlier blocks and does not. Scaling
        // the whole output while clearing the whole carry wrote half of that debt off.
        assert_eq!((s.kind, s.owed, s.carry_paid), ("pool-only", 1_250_000, 0));
        assert_eq!(s.split, vec![("A".to_string(), 300_000 + 400_000), ("B".to_string(), 250_000 + 300_000)]);
        assert_eq!(cleared(&s).values().sum::<i64>(), -700_000, "all of it discharged");
    }

    /// A full split pays every output itself, so it owes nothing and clears all the carry it rode.
    #[test]
    fn a_full_split_owes_nothing_and_clears_the_carry_it_paid() {
        let payees = vec![payee("A", 1_000_000, 400_000, 1), payee("B", 800_000, 300_000, 2)];
        let s = settle(&CoinbaseKind::Split, Some(coinbaser(312_500_000, &payees)), 312_500_000, |_| 1);
        assert_eq!((s.kind, s.owed, s.carry_paid), ("split", 0, 700_000));
        assert_eq!(cleared(&s).values().sum::<i64>(), -700_000);
    }

    /// Work credited to a solo or foreign coinbase is not the window's, so nothing settles.
    #[test]
    fn solo_and_foreign_coinbases_settle_nothing() {
        for (kind, name) in
            [(CoinbaseKind::EmptySolo, "solo"), (CoinbaseKind::GatewaySolo, "solo"), (CoinbaseKind::Foreign, "unknown")]
        {
            let payees = vec![payee("A", 1_000_000, 400_000, 1)];
            let s = settle(&kind, Some(coinbaser(312_500_000, &payees)), 312_500_000, |_| 0);
            assert_eq!((s.kind, s.owed, s.carry_paid), (name, 0, 0));
            assert!(s.carry_delta.is_empty() && s.split.is_empty(), "{name} touches no books");
        }
    }

    fn issued(id: u8, value: u64) -> IssuedCoinbaser {
        IssuedCoinbaser {
            id,
            value,
            prev_hash: [id; 32],
            height: 0,
            outputs: vec![Output { sats: value, script: vec![0x00, 0x14, id] }],
            payees: vec![],
            unpaid: vec![],
            rebate_credits: vec![],
            rebate_owed_credited: 0,
            rebate_deferred: 0,
            class_budget: None,
        }
    }

    /// The property block 968440 came down to: whatever the bucket says, the gateway gets an
    /// answer. Silence makes stock DATUM time out after `STOCK_COINBASER_DEADLINE` and publish
    /// a coinbase with no miner outputs.
    #[test]
    fn a_coinbaser_request_is_never_dropped() {
        let mut empty = VecDeque::new();
        let mut seen = VecDeque::new();
        seen.push_back(issued(7, 312_500_000));
        for tokens in [0u32, 1, 32] {
            for value in [312_500_000u64, 312_644_067] {
                for q in [&mut empty, &mut seen] {
                    // no variant of the decision withholds a reply
                    match coinbaser_action(tokens, q, value, None) {
                        CoinbaserAction::Fresh | CoinbaserAction::Repeat(_) | CoinbaserAction::FreshOverRate => {}
                    }
                }
            }
        }
    }

    /// The two faults must not be reported as each other: an operator told "the split was in hand"
    /// goes and looks at coinbase selection, and one told it was missing goes and looks at the
    /// coinbaser request path. Live 968440-class shares are the first case.
    #[test]
    fn pool_only_cause_separates_the_two_gateway_faults() {
        let had_split = pool_only_cause(0, 1, 41);
        assert!(had_split.contains("section 0"), "{had_split}");
        assert!(had_split.contains("41 miner outputs"), "{had_split}");
        assert!(had_split.contains("split was in hand"), "{had_split}");

        let no_split = pool_only_cause(4, 7, 0);
        assert!(no_split.contains("no split"), "{no_split}");
        assert!(!no_split.contains("in hand"), "{no_split}");

        // singular reads correctly; a one-payee split is still a split the gateway ignored
        assert!(pool_only_cause(0, 2, 1).contains("1 miner output —"), "{}", pool_only_cause(0, 2, 1));
    }

    #[test]
    fn inside_the_bucket_every_request_is_computed_fresh() {
        let mut q = VecDeque::new();
        q.push_back(issued(7, 312_500_000));
        assert_eq!(coinbaser_action(1, &q, 312_500_000, None), CoinbaserAction::Fresh);
        assert_eq!(coinbaser_action(32, &q, 312_500_000, None), CoinbaserAction::Fresh);
    }

    #[test]
    fn over_the_bucket_repeats_the_reply_for_that_exact_value() {
        let mut q = VecDeque::new();
        q.push_back(issued(7, 312_500_000));
        q.push_back(issued(8, 312_644_067));
        // The gateway only accepts a reply whose value equals the one it asked about, so a
        // repeat is only usable when the value matches exactly.
        assert_eq!(coinbaser_action(0, &q, 312_500_000, None), CoinbaserAction::Repeat(7));
        assert_eq!(coinbaser_action(0, &q, 312_644_067, None), CoinbaserAction::Repeat(8));
        // newest wins when a value was answered twice
        q.push_back(issued(9, 312_500_000));
        assert_eq!(coinbaser_action(0, &q, 312_500_000, None), CoinbaserAction::Repeat(9));
        // a value never answered is computed rather than skipped
        assert_eq!(coinbaser_action(0, &q, 999_999_999, None), CoinbaserAction::FreshOverRate);
        assert_eq!(coinbaser_action(0, &VecDeque::new(), 312_500_000, None), CoinbaserAction::FreshOverRate);
    }

    /// The warning has to fire while there is still margin, or it is just an obituary.
    #[test]
    fn the_slow_warning_leaves_room_before_a_gateway_gives_up() {
        assert!(COINBASER_SLOW < STOCK_COINBASER_DEADLINE);
    }

    #[test]
    fn the_refusal_names_the_419_rule_and_has_no_gap() {
        assert!(OUTDATED_NODE_REFUSAL.contains("#419"), "{OUTDATED_NODE_REFUSAL}");
        assert!(OUTDATED_NODE_REFUSAL.contains("29.4.2"));
        assert!(OUTDATED_NODE_REFUSAL.contains("taken back straight away"));
        assert!(!OUTDATED_NODE_REFUSAL.contains("  "), "no run of spaces: {OUTDATED_NODE_REFUSAL:?}");
    }

    /// Who is trusted as the pool's own gateway is decided on the whole key when the whole key
    /// is configured: sharing its first 16 digits is not being it.
    #[test]
    fn a_house_gateway_is_matched_on_as_much_key_as_was_configured() {
        let text = "rpc = \"http://127.0.0.1:9\"\ndata-dir = \"/tmp\"\npayout-address = \"bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4\"\nhouse-loopback = false\n";
        let mut cfg: Config = toml::from_str(text).unwrap();
        let house = format!("9d992e5cfec05102{}", "ab".repeat(24));
        let lookalike = format!("9d992e5cfec05102{}", "cd".repeat(24));
        let remote: SocketAddr = "203.0.113.7:5000".parse().unwrap();
        cfg.house_gateways = vec![house.clone()];
        assert!(house_stratum(&cfg, remote, &house));
        assert!(!house_stratum(&cfg, remote, &lookalike));
        cfg.house_gateways = vec!["9d992e5cfec05102".into()];
        assert!(house_stratum(&cfg, remote, &lookalike), "a prefix matches whatever starts with it");
        // loopback is house only while house-loopback says so
        cfg.house_gateways.clear();
        assert!(!house_stratum(&cfg, "127.0.0.1:5000".parse().unwrap(), &lookalike));
        cfg.house_loopback = true;
        assert!(house_stratum(&cfg, "127.0.0.1:5000".parse().unwrap(), &lookalike));
    }

    /// A build is held by the commit its hello names and by nothing else: not a build with
    /// changes on top of a listed commit (the change may be the fix), not one that says it
    /// places the split, not another program's version string, and nothing with the key unset.
    #[test]
    fn a_held_split_build_is_named_by_the_commit_in_its_hello() {
        let builds: Vec<String> =
            ["e894b8a", "f74c22a", "2fea7e5", "57f1aee", "beb9461"].iter().map(|s| s.to_string()).collect();
        let held = |ua: &str| held_split_build(&builds, ua);
        // as the live gateways say it
        assert_eq!(held("v0.4.1-beta/e894b8ac29ae06bf6e3b14dafd21f72dcd65fb84"), Some("e894b8a"));
        assert_eq!(held("v0.4.1-beta/f74c22aa1f048cef5bf0440b89f86427e658fb89"), Some("f74c22a"));
        assert_eq!(held("v0.4.1-beta/2fea7e51286d3821c19dc1c240b8caa92bd92532"), Some("2fea7e5"));
        assert_eq!(held("v0.4.1-beta/57f1aeebf8b2e55ee03c768e09d0738bc2973ebd"), Some("57f1aee"));
        assert_eq!(held("v0.4.1-beta/beb946154dde86b69d9afd008974198ddd08bc4c"), Some("beb9461"));
        // built at a tag, or printed in capitals: the same commit
        assert_eq!(held("v0.4.1-beta/e894b8ac29ae06bf6e3b14dafd21f72dcd65fb84(v0.4.1)"), Some("e894b8a"));
        assert_eq!(held("v0.4.1-beta/E894B8AC29AE06BF6E3B14DAFD21F72DCD65FB84"), Some("e894b8a"));
        // not these
        for ua in [
            "v0.4.1-beta/e894b8ac29ae06bf6e3b14dafd21f72dcd65fb84+",
            "v0.4.1-beta+lazarus-split/e894b8ac29ae06bf6e3b14dafd21f72dcd65fb84",
            "v0.4.1-beta/b9ea7dc3eb91352565ab487ec55ed6ee5964a440",
            "v0.4.1-beta/7491a5099dd5d887a027c812f71de63e0d5986a3",
            "v0.4.1-beta/155b6bf4382b309df9915fb3f49d6229cd7f1d17",
            "v0.4.1-beta/e894b8bc29ae06bf6e3b14dafd21f72dcd65fb84",
            "v0.4.1-beta/e894b8",
            "v0.4.1-beta/UNKNOWN_GIT_HASH",
            "ratum-gateway/0.1.28/e894b8ac29ae",
            "lazarus-gateway/0.1",
            "e894b8ac29ae06bf6e3b14dafd21f72dcd65fb84",
            "",
        ] {
            assert_eq!(held(ua), None, "{ua:?}");
        }
        // the key unset holds nothing, and an entry that cannot name a commit matches nothing
        let e894 = "v0.4.1-beta/e894b8ac29ae06bf6e3b14dafd21f72dcd65fb84";
        assert_eq!(held_split_build(&[], e894), None);
        assert_eq!(held_split_build(&["e894b8".into(), "e894b8a+".into(), "".into()], e894), None);
        let full = "e894b8ac29ae06bf6e3b14dafd21f72dcd65fb84".to_string();
        assert_eq!(held_split_build(std::slice::from_ref(&full), e894), Some(full.as_str()));
    }

    /// The review's multi-miner case. A held gateway pays whoever its section 0 is configured
    /// with, so it is left paying itself only once its own first shares all name one payout, and
    /// only while no other mines on it. A second payout puts it back on the pool for the rest of
    /// the session; a payout that comes to dominate after it was held is kept off it and not
    /// remembered; and nothing is held on a payout that is not an address.
    #[test]
    fn a_held_split_session_pays_itself_only_while_its_shares_name_one_payout() {
        let (a, b) = (wpkh(1), wpkh(2));
        let mut h = HeldPayout::default();
        assert_eq!(h.script(0), None, "a session starts owing, whatever it was remembered paying");
        for _ in 1..HELD_SPLIT_SHARES {
            assert!(!h.note(1, Some(&a)));
            assert_eq!(h.script(1), None, "not before its shares have shown one payout");
        }
        assert!(h.note(1, Some(&a)), "the sixteenth share of one payout holds it");
        assert_eq!(h.script(1), Some(&a[..]));
        assert!(!h.note(1, Some(&a)) && h.script(1) == Some(&a[..]), "held once, and stays while it is the one");
        assert!(!h.keeps_off(&a));

        // a second miner's payout: back to the pool, for good
        assert!(!h.note(2, Some(&a)));
        assert_eq!(h.script(2), None);
        for _ in 0..100 {
            assert!(!h.note(2, Some(&b)));
        }
        assert_eq!(h.script(2), None);
        assert!(h.keeps_off(&b), "the jobs it has out pay the one it was held on");

        // two payouts from its first shares: never held, and it follows the dominant one as any
        // session does
        let mut two = HeldPayout::default();
        assert!(!two.note(1, Some(&a)));
        for _ in 0..100 {
            assert!(!two.note(2, Some(&b)));
        }
        assert_eq!(two.script(2), None);
        assert!(!two.keeps_off(&a) && !two.keeps_off(&b));

        // one payout that is not an address, or is the pool's: nothing to hold it on
        let mut none = HeldPayout::default();
        for _ in 0..100 {
            assert!(!none.note(1, None));
        }
        assert_eq!(none.script(1), None);
    }

    fn wpkh(n: u16) -> Vec<u8> {
        let mut s = vec![0x00, 0x14];
        s.extend_from_slice(&[0xaa; 20]);
        s[20..22].copy_from_slice(&n.to_le_bytes());
        s
    }

    fn tr(n: u16) -> Vec<u8> {
        let mut s = vec![0x51, 0x20];
        s.extend_from_slice(&[0xee; 32]);
        s[32..34].copy_from_slice(&n.to_le_bytes());
        s
    }

    /// What a CONVOY size class with `room` bytes makes of `list` (`datum_coinbaser.c` at
    /// `b9ea7dc`): first fit in order, stopping under 30 bytes, and whatever value it did not
    /// place to the script it was configured with.
    fn convoy_class(list: &[Output], room: usize, value: u64, configured: &[u8]) -> coinbase::Coinbase {
        let (mut left, mut placed) = (room, 0u64);
        let mut outs = Vec::new();
        for o in list {
            if left < 30 || placed >= value {
                break;
            }
            let need = o.script.len() + 9;
            if need <= left && placed + o.sats <= value {
                outs.push(coinbase::TxOut { value: o.sats, script: o.script.clone() });
                left -= need;
                placed += o.sats;
            }
        }
        if value > placed {
            outs.push(coinbase::TxOut { value: value - placed, script: configured.to_vec() });
        }
        let (bytes, _, _) = coinbase::build(966_267, b"Lazarus", &outs, 0);
        coinbase::parse(&bytes).unwrap()
    }

    fn pool_params() -> SplitParams {
        SplitParams {
            fee_bps: 0,
            stratum_fee_bps: 0,
            datum_rebate_bps: 0,
            grace_fee_bps: 0,
            grace_rebate_bps: 0,
            min_payout: 546,
            max_outputs: 511,
            output_budget_bytes: 14_000 - 9 - 64,
            stale_after: 0,
            stale_min_payout: 10_000,
            stale_max_outputs: 25,
        }
    }

    /// 91 miners, about what a live window holds: mostly P2WPKH, one in seven P2TR, one in five
    /// with carry waiting.
    fn window() -> (Vec<tides::MinerStat>, HashMap<String, Vec<u8>>) {
        let (mut miners, mut scripts) = (Vec::new(), HashMap::new());
        for i in 0..91u16 {
            let identity = format!("m{i:02}");
            scripts.insert(identity.clone(), if i % 7 == 3 { tr(i) } else { wpkh(i) });
            let carry = if i % 5 == 0 { 50_000 } else { 0 };
            miners.push(tides::MinerStat {
                identity,
                work: 10_000 - u64::from(i) * 100,
                stratum_work: 0,
                grace_work: 0,
                credits: 1,
                last_ts: 0,
                carry,
            });
        }
        (miners, scripts)
    }

    const VALUE: u64 = 312_538_966;

    fn split_of(miners: &[tides::MinerStat], scripts: &HashMap<String, Vec<u8>>, p: &SplitParams) -> Split {
        let total = miners.iter().map(|m| m.work).sum();
        tides::split::compute(miners.to_vec(), total, VALUE, p, 0, 0, |i| scripts.get(i).cloned())
    }

    fn policy<'a>(pool: &'a [u8], issued: &'a [Output]) -> Policy<'a> {
        Policy {
            pool_script: pool,
            issued: Some(issued),
            tolerance: 2,
            now: 0,
            min_pot: 0,
            gateway_script: None,
            empty_solo_fee_bps: 0,
            trusted_target: false,
            uncommitted_pot: 20,
            held_split: false,
        }
    }

    fn classify(cb: &coinbase::Coinbase, pool: &[u8], issued: &[Output]) -> CoinbaseKind {
        verify::classify_coinbase(cb, &policy(pool, issued), false, 3, false)
    }

    /// The whole of `class-budget` on one window. A b9ea7dc class-2 section keeps the head of the
    /// list and the block would be Partial; its shares say how many payee bytes it kept; the next
    /// list is held to those bytes, the section keeps all of it, and a block on it is a split that
    /// owes nothing, with the tail's earnings in carry and recorded for the fee wallet to hold.
    #[test]
    fn a_class_budget_learned_from_partial_shares_turns_the_next_block_into_a_split() {
        let pool = wpkh(9999);
        let (miners, scripts) = window();
        let full = split_of(&miners, &scripts, &pool_params());
        let list = coinbaser_outputs(&full, &pool);
        assert_eq!(full.payees.len(), 91, "the pool's budget places every miner");
        // class 2: 755 bytes less what the rest of the coinbase takes
        let room = 557;
        let cb = convoy_class(&list, room, VALUE, &pool);
        let CoinbaseKind::Partial(n) = classify(&cb, &pool, &list) else { panic!("{:?}", classify(&cb, &pool, &list)) };
        assert!((15..=17).contains(&n), "{n}");
        let kept = kept_payee_bytes(&list, &pool, &cb).expect("a first-fit cut");
        let placed: usize = cb.outputs.iter().filter(|o| o.script != pool).map(|o| 9 + o.script.len()).sum();
        assert_eq!(kept, placed);

        let (mut budget, t) = (ClassBudget::default(), Instant::now());
        assert!(!budget.observe(2, 1, kept, t) && !budget.observe(2, 2, kept, t) && budget.observe(2, 3, kept, t));
        let capped = split_of(&miners, &scripts, &class_params(&pool_params(), budget.bytes));
        let short = coinbaser_outputs(&capped, &pool);
        assert!(capped.payees.iter().map(|p| 9 + p.script.len()).sum::<usize>() <= kept);
        let cb = convoy_class(&short, room, VALUE, &pool);
        assert_eq!(classify(&cb, &pool, &short), CoinbaseKind::Split, "the class keeps the whole list now");

        let tail: Vec<&tides::Unpaid> =
            capped.unpaid.iter().filter(|u| u.reason == tides::UnpaidReason::OverBudget).collect();
        assert_eq!(capped.payees.len() + tail.len(), full.payees.len(), "everyone is placed or deferred");
        let cbr = Coinbaser {
            value: VALUE,
            payees: &capped.payees,
            unpaid: &capped.unpaid,
            rebate_credits: &capped.rebate_credits,
            rebate_owed_credited: 0,
            rebate_deferred: 0,
            class_capped: true,
        };
        let s = settle(&CoinbaseKind::Split, Some(cbr), VALUE, |script| cb.paid_to(script));
        assert_eq!((s.kind, s.owed), ("split", 0));
        let d = cleared(&s);
        for u in &tail {
            assert!(u.defers());
            assert_eq!(d.get(u.identity.as_str()), Some(&(u.earned as i64)), "{} waits in carry", u.identity);
        }
        assert_eq!(s.carry_reserved, tail.iter().map(|u| u.earned).sum::<u64>());
        assert!(s.carry_reserved > VALUE / 5, "the tail is most of what Partial(17) owed: {}", s.carry_reserved);
    }

    /// The budget cuts the list where the pool's own budget would have if it were that small:
    /// the same order, the head placed first-fit, the tail deferred to carry, the pool last.
    #[test]
    fn a_class_budget_defers_the_tail_to_carry_and_keeps_the_order() {
        let pool = wpkh(9999);
        let (miners, scripts) = window();
        let full = split_of(&miners, &scripts, &pool_params());
        for budget in [527usize, 539, 310, 31, 30] {
            let capped = split_of(&miners, &scripts, &class_params(&pool_params(), Some(budget)));
            // first fit, in the uncapped list's order
            let mut left = budget;
            let expect: Vec<&str> = full
                .payees
                .iter()
                .filter(|p| {
                    let need = 9 + p.script.len();
                    let fits = need <= left;
                    if fits {
                        left -= need;
                    }
                    fits
                })
                .map(|p| p.identity.as_str())
                .collect();
            let got: Vec<&str> = capped.payees.iter().map(|p| p.identity.as_str()).collect();
            assert_eq!(got, expect, "budget {budget}");
            for p in &full.payees {
                if !got.contains(&p.identity.as_str()) {
                    let u = capped.unpaid.iter().find(|u| u.identity == p.identity).expect("deferred, not dropped");
                    assert_eq!((u.reason, u.earned), (tides::UnpaidReason::OverBudget, p.sats - p.carry));
                }
            }
            let out = coinbaser_outputs(&capped, &pool);
            assert_eq!(out.last().unwrap().script, pool, "the pool's output is last");
            assert_eq!(out.iter().map(|o| o.sats).sum::<u64>(), VALUE);
            assert_eq!(out.len(), capped.payees.len() + 1);
        }
    }

    /// Without a class budget, or with one the list fits in, a reply is byte for byte what it was
    /// before class budgets existed.
    #[test]
    fn without_a_class_budget_a_reply_is_the_bytes_it_always_was() {
        // the reply as it was built before `coinbaser_outputs`
        let before = |split: &Split, pool: &[u8]| {
            let mut outputs: Vec<Output> =
                split.payees.iter().map(|p| Output { sats: p.sats, script: p.script.clone() }).collect();
            if split.pool_sats > 0 || outputs.is_empty() {
                outputs.push(Output { sats: split.pool_sats.max(1), script: pool.to_vec() });
            }
            coinbaser::encode_v2(7, &outputs)
        };
        let pool = wpkh(9999);
        let (miners, scripts) = window();
        let p = pool_params();
        let today = before(&split_of(&miners, &scripts, &p), &pool);
        let whole: usize = split_of(&miners, &scripts, &p).payees.iter().map(|p| 9 + p.script.len()).sum();
        for budget in [None, Some(p.output_budget_bytes), Some(usize::MAX), Some(whole)] {
            let params = class_params(&p, budget);
            assert_eq!(matches!(params, Cow::Borrowed(_)), budget != Some(whole), "{budget:?}");
            let reply = coinbaser::encode_v2(7, &coinbaser_outputs(&split_of(&miners, &scripts, &params), &pool));
            assert_eq!(reply, today, "{budget:?}");
        }
        // an empty window is still one pool output
        let empty = split_of(&[], &scripts, &p);
        assert_eq!(coinbaser::encode_v2(7, &coinbaser_outputs(&empty, &pool)), before(&empty, &pool));
    }

    /// A section's size is the same payee bytes kept on three coinbasers. It only goes down, and
    /// it is the smallest section's, since every section is handed the one list.
    #[test]
    fn a_class_budget_is_the_same_kept_bytes_on_three_coinbasers_and_only_goes_down() {
        let (mut b, t) = (ClassBudget::default(), Instant::now());
        for _ in 0..10 {
            assert!(!b.observe(2, 1, 527, t), "one coinbaser's shares are one sighting however many");
        }
        assert!(!b.observe(2, 2, 527, t));
        assert_eq!(b.bytes, None, "two can be two nearly full templates in a row");
        assert!(!b.observe(2, 3, 539, t), "other bytes are another tally");
        assert!(!b.observe(4, 3, 527, t), "and so is another section");
        assert!(b.observe(2, 4, 527, t));
        assert_eq!(b.bytes, Some(527));
        // more room on another list, or a bigger class, never raises it
        for id in 5..9 {
            assert!(!b.observe(2, id, 539, t) && !b.observe(5, id, 4_000, t));
        }
        assert_eq!(b.bytes, Some(527));
        // a smaller class on the same gateway (NiceHash's), or a fuller template still cut under
        // the budget, lowers it on the same evidence
        assert!(!b.observe(1, 10, 310, t) && !b.observe(1, 11, 310, t));
        assert_eq!(b.bytes, Some(527));
        assert!(b.observe(1, 12, 310, t));
        assert_eq!(b.bytes, Some(310));
        // a section's tally is bounded
        for (id, n) in (0..100usize).enumerate() {
            assert!(!b.observe(3, id as u8, CLASS_BUDGET_MIN_BYTES + n, t));
        }
        assert_eq!(b.seen[&3].len(), CLASS_BUDGET_TALLY);
    }

    /// CONVOY's placement of a list into `room` bytes (`datum_coinbaser.c:210-224` at b9ea7dc,
    /// the value test aside): the payee bytes kept, and whether any output was left out.
    fn convoy_first_fit(room: usize, needs: &[usize]) -> (usize, bool) {
        let (mut left, mut placed) = (room, 0usize);
        for need in needs {
            if *need <= left {
                left -= need;
                placed += 1;
                if left < 30 {
                    break;
                }
            }
        }
        (room - left, placed < needs.len())
    }

    /// The floor is under what any class 1 keeps of a list it cut, whatever the payees' scripts,
    /// and a sighting just under it is still refused.
    #[test]
    fn the_class_budget_floor_is_under_what_the_smallest_class_keeps_of_a_cut_list() {
        assert_eq!((CLASS_ROOM_MIN_BYTES, PAYEE_OUTPUT_MAX_BYTES, CLASS_BUDGET_MIN_BYTES), (274, 43, 231));
        // P2WPKH, P2SH, P2PKH, P2WSH/P2TR: every script `address::to_script` returns
        let needs = [8 + 1 + 22, 8 + 1 + 23, 8 + 1 + 25, 8 + 1 + 34];
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut least = usize::MAX;
        for _ in 0..200_000 {
            let list: Vec<usize> = (0..1 + next() % 24).map(|_| needs[next() as usize % needs.len()]).collect();
            // a scriptSig shorter than the longest leaves class 1 more room, never less
            let room = CLASS_ROOM_MIN_BYTES + next() as usize % 60;
            let (kept, cut) = convoy_first_fit(room, &list);
            if cut {
                assert!(kept >= CLASS_BUDGET_MIN_BYTES, "{room} bytes of room kept {kept} of {list:?}");
                least = least.min(kept);
            }
        }
        assert!(least < CLASS_ROOM_MIN_BYTES, "a cut list does keep less than the room: {least}");

        // the review's case, a real class 1 keeping less than its room: five P2TR payees and
        // a P2WPKH one are 246 bytes of 274, and nothing fits the 28 left
        assert_eq!(convoy_first_fit(274, &[43, 43, 43, 43, 43, 31, 31, 43]), (246, true));
        let (mut b, t) = (ClassBudget::default(), Instant::now());
        assert!(!b.observe(1, 1, 246, t) && !b.observe(1, 2, 246, t) && b.observe(1, 3, 246, t));
        assert_eq!(b.bytes, Some(246));
        // and under the floor nothing is learned, however often it is seen
        let mut b = ClassBudget::default();
        for id in 0..10 {
            assert!(!b.observe(1, id, CLASS_BUDGET_MIN_BYTES - 1, t));
        }
        assert_eq!(b.bytes, None);
        assert!(!b.observe(1, 1, CLASS_BUDGET_MIN_BYTES, t) && !b.observe(1, 2, CLASS_BUDGET_MIN_BYTES, t));
        assert!(b.observe(1, 3, CLASS_BUDGET_MIN_BYTES, t));
    }

    /// The review's NiceHash case. One class-1 miner sets the budget to its ~310 bytes, and
    /// nothing on a list cut to that can show more room. So the budget lasts `CLASS_BUDGET_TTL`
    /// from when it was last set, and then it and every sighting behind it are gone: the classes
    /// in use by then teach it again, from three fresh coinbasers.
    #[test]
    fn a_class_budget_is_forgotten_six_hours_after_it_was_set_and_learned_again() {
        let t = Instant::now();
        let mut b = ClassBudget::default();
        assert!(!b.expire(t + CLASS_BUDGET_TTL * 10), "nothing to forget before a budget is set");
        assert!(!b.observe(2, 1, 527, t) && !b.observe(2, 2, 527, t));
        assert!(!b.expire(t + CLASS_BUDGET_TTL * 10), "a budget that was never set is not a budget forgotten");
        assert!(b.seen.get(&2).is_none(), "sightings older than the TTL are forgotten before a budget exists");
        assert!(!b.observe(1, 3, 310, t) && !b.observe(1, 4, 310, t) && b.observe(1, 5, 310, t));
        assert_eq!(b.bytes, Some(310));

        let later = t + CLASS_BUDGET_TTL - Duration::from_secs(1);
        assert!(!b.expire(later));
        assert_eq!(b.bytes, Some(310), "still standing a second short of it");
        // lowered again an hour in: it stands six hours from then
        let lowered = t + Duration::from_secs(3600);
        assert!(!b.observe(1, 6, 300, lowered) && !b.observe(1, 7, 300, lowered) && b.observe(1, 8, 300, lowered));
        assert!(!b.expire(t + CLASS_BUDGET_TTL));
        assert_eq!(b.bytes, Some(300));

        let gone = lowered + CLASS_BUDGET_TTL;
        assert!(b.expire(gone));
        assert_eq!((b.bytes, b.set_at, b.seen.is_empty()), (None, None, true), "forgotten, sightings and all");
        assert!(!b.expire(gone + CLASS_BUDGET_TTL), "once");
        // one more class-1 sighting is one sighting, not a fourth
        assert!(!b.observe(1, 9, 300, gone));
        assert_eq!(b.bytes, None);
        // class 2, still mining, sets it again on three fresh coinbasers
        assert!(!b.observe(2, 10, 527, gone) && !b.observe(2, 11, 527, gone) && b.observe(2, 12, 527, gone));
        assert_eq!(b.bytes, Some(527));
    }

    /// Only a cut a size class makes says what the class holds.
    #[test]
    fn only_a_first_fit_cut_says_what_a_class_holds() {
        let pool = wpkh(9999);
        let out = |script: Vec<u8>| Output { sats: 1_000_000, script };
        let list = vec![out(wpkh(1)), out(tr(2)), out(wpkh(3)), out(wpkh(4)), out(wpkh(5)), out(pool.clone())];
        let value = 6_000_000;
        // room for the first P2WPKH, not the P2TR after it, then one more P2WPKH and under 30 left
        let cb = convoy_class(&list, 70, value, &pool);
        assert_eq!(classify(&cb, &pool, &list), CoinbaseKind::Partial(2));
        assert_eq!(kept_payee_bytes(&list, &pool, &cb), Some(62));
        let paying = |scripts: &[&Vec<u8>], rest: u64| {
            let mut outs: Vec<coinbase::TxOut> =
                scripts.iter().map(|s| coinbase::TxOut { value: 1_000_000, script: s.to_vec() }).collect();
            outs.push(coinbase::TxOut { value: rest, script: pool.clone() });
            let (bytes, _, _) = coinbase::build(966_267, b"Lazarus", &outs, 0);
            coinbase::parse(&bytes).unwrap()
        };
        let (s1, s3, s4) = (&list[0].script, &list[2].script, &list[3].script);
        // a subset no room gives: the third output is no bigger than the fourth, kept after it
        assert_eq!(kept_payee_bytes(&list, &pool, &paying(&[s1, s4], 4_000_000)), None);
        // worth less than the list: dropped for value, not room
        assert_eq!(kept_payee_bytes(&list, &pool, &paying(&[s1, s3], 1_000_000)), None);
        // all of it, or none of it
        let all: Vec<&Vec<u8>> = list[..5].iter().map(|o| &o.script).collect();
        assert_eq!(kept_payee_bytes(&list, &pool, &paying(&all, 1_000_000)), None);
        assert_eq!(kept_payee_bytes(&list, &pool, &paying(&[], value)), None);
    }

    /// A section is taken for its class only where the template left it more room than it kept.
    #[test]
    fn a_section_cut_by_a_full_template_is_not_taken_for_its_class() {
        // what a node's default template leaves (4 000 weight units kept for the coinbase)
        assert!(template_left_room(527, 3_992_000));
        assert!(template_left_room(310, 3_992_000) && template_left_room(1_300, 3_992_000));
        assert!(template_left_room(527, 0));
        // packed to the last few thousand, or past the limit
        assert!(!template_left_room(527, 3_996_000));
        assert!(!template_left_room(1_500, 3_992_000));
        assert!(!template_left_room(31, 4_100_000));
    }

    /// CONVOY C gateways only. iohzrd's `7491a50` is one, and never learns a budget: every
    /// BLAKE2b miner on it gets the class that holds the whole list, so its shares are never
    /// Partial.
    #[test]
    fn a_class_budget_is_only_for_convoy_c_gateways() {
        for ua in [
            "v0.4.1-beta/b9ea7dc3eb91352565ab487ec55ed6ee5964a440",
            "v0.4.1-beta/b9ea7dc3eb91352565ab487ec55ed6ee5964a440+",
            "v0.4.1-beta/e998e38ee198da26129e45ffa80402157ae76c55",
            "v0.4.1-beta/UNKNOWN_GIT_HASH",
            "v0.4.1-beta/7491a5099dd5d887a027c812f71de63e0d5986a3",
        ] {
            assert!(class_budget_applies(Generation::Convoy, ua), "{ua}");
            assert!(!class_budget_applies(Generation::Ocean, ua), "{ua}");
        }
        for ua in [
            "lazarus-gateway/0.1",
            "v0.4.1-beta+lazarus-split/121edd06244082df2aa101f3b3c424faed8dd31b+",
            "ratum-gateway/0.1.28/f0569180c986",
            "ratum-gateway/0.1.51/cffaf4743ee2-dirty",
            "Ratum-Gateway/0.2",
            "ratum/0.1",
        ] {
            assert!(!class_budget_applies(Generation::Convoy, ua), "{ua}");
        }
    }

    /// What a class-capped block reserves is its deferred-for-room earnings, whatever it is
    /// classified as, and moves with the reward as `carry_delta` does. Held only to the pool's own
    /// budget, a coinbaser reserves nothing, and its books are the same either way.
    #[test]
    fn a_class_capped_block_reserves_the_tail_it_left_in_the_pool_output() {
        use tides::UnpaidReason::{BelowMinimum, OverBudget};
        let payees = vec![payee("A", 1_000_000, 400_000, 1), payee("B", 800_000, 0, 2)];
        let unpaid = [
            tides::Unpaid { identity: "C".into(), sats: 700_000, earned: 600_000, reason: OverBudget },
            tides::Unpaid { identity: "D".into(), sats: 500_000, earned: 500_000, reason: OverBudget },
            tides::Unpaid { identity: "E".into(), sats: 300, earned: 300, reason: BelowMinimum },
        ];
        let cb = |class_capped| Coinbaser { unpaid: &unpaid, class_capped, ..coinbaser(312_500_000, &payees) };
        let s = settle(&CoinbaseKind::Split, Some(cb(true)), 312_500_000, |_| 1);
        assert_eq!((s.kind, s.owed, s.carry_reserved), ("split", 0, 1_100_000));
        let d = cleared(&s);
        assert_eq!((d.get("C"), d.get("D"), d.get("E")), (Some(&600_000), Some(&500_000), Some(&300)));
        let plain = settle(&CoinbaseKind::Split, Some(cb(false)), 312_500_000, |_| 1);
        assert_eq!((plain.carry_reserved, &plain.carry_delta), (0, &s.carry_delta));

        // Partial on a capped coinbaser (a fuller template than the budget was learned on): the
        // payee it dropped is owed as ever, and the tail is still reserved
        let a = [0x00, 0x14, 1];
        let p = settle(&CoinbaseKind::Partial(1), Some(cb(true)), 312_500_000, |s| if s == a { 1_000_000 } else { 0 });
        assert_eq!((p.kind, p.owed, p.carry_reserved), ("partial", 800_000, 1_100_000));
        // a reward twice the one asked about: what the tail earned doubles, in the books and here
        let far = settle(&CoinbaseKind::Split, Some(cb(true)), 625_000_000, |_| 1);
        assert_eq!((cleared(&far).get("C"), far.carry_reserved), (Some(&1_200_000), 2_200_000));
        for kind in [CoinbaseKind::GatewaySolo, CoinbaseKind::EmptySolo, CoinbaseKind::Foreign] {
            assert_eq!(settle(&kind, Some(cb(true)), 312_500_000, |_| 0).carry_reserved, 0);
        }
    }

    /// A reply over the bucket repeats one held to the same class budget, or none.
    #[test]
    fn a_repeat_is_only_of_a_reply_held_to_the_same_class_budget() {
        let mut q = VecDeque::new();
        q.push_back(issued(7, 312_500_000));
        q.push_back(IssuedCoinbaser { class_budget: Some(527), ..issued(8, 312_500_000) });
        assert_eq!(coinbaser_action(0, &q, 312_500_000, None), CoinbaserAction::Repeat(7));
        assert_eq!(coinbaser_action(0, &q, 312_500_000, Some(527)), CoinbaserAction::Repeat(8));
        assert_eq!(coinbaser_action(0, &q, 312_500_000, Some(310)), CoinbaserAction::FreshOverRate);
    }
}
