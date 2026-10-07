// A curated, versioned knowledge base for `lazarus_faq`: public-safe answers about mining on
// Lazarus Pool, in the pool's own words.
//
// Rule for every entry: only what the public site, llms.txt or the public APIs already say. No
// operator internals (hosts behind the public names, grace, keys, treasury, partner terms,
// security findings). Figures that move (fees, bonus, minimum output) are given with the date they
// were true and a pointer to the live tool, so an assistant never presents them as permanent.

export const FAQ_VERSION = "2026-10-07.1";

const S = "https://pool.lazarus-xbt.xyz";
const E = "https://mempool.lazarus-xbt.xyz";

export const FAQ = [
  {
    id: "fees",
    title: "Fees on each path (own DATUM gateway, public stratum, solo)",
    keywords: ["fee", "fees", "cost", "percent", "charge", "cheap", "expensive", "100", "50", "25", "grace", "donation", "15", "price", "take"],
    answer:
      "Own DATUM gateway: 0% fee, plus the DATUM bonus (see 'datum-bonus'). Public stratum is a donation and validation endpoint at 100% since 2026-10-07 05:00 UTC: 50 points to DATUM gateways and 50 to the pool address, with a 24 h grace at 25% (12.5 points) or 96 h if the address mined through a DATUM gateway on Lazarus in the prior 30 days. " +
      "Point a miner at it to confirm it connects and submits shares while your own node is still coming online. Solo stratum is closed. The fee is applied when a block is paid, to the work in the window. " +
      "These are the values as of this knowledge base's version; for the live numbers call pool_status or connection_info, which read them from the pool.",
    links: [`${S}/pools`, `${S}/datum-subsidy`],
  },
  {
    id: "datum-bonus",
    title: "The DATUM bonus (rebate): how DATUM miners earn more than 100%",
    keywords: ["bonus", "rebate", "subsidy", "uplift", "datum", "extra", "credited"],
    answer:
      "When a block is found, 50 points of the public stratum's 100% fee on that block's stratum work are credited to the addresses mining through their own DATUM gateway, split in proportion to their DATUM work in the window. During an address's 24 h grace (96 h if it mined through a DATUM gateway on Lazarus in the prior 30 days) the credit is 12.5 points of the 25% grace fee. " +
      "The credit is booked as carry (see 'carry') and paid with that address's next coinbase output that clears the minimum. The uplift it gives is a live number that shrinks as DATUM's share of the pool grows (about +3.9% on 26 Sep 2026); " +
      "miner_overview shows it per address (datum_bonus_earned_xbt, if_on_datum) and pool_status shows the earnings per TH/s on each path.",
    links: [`${S}/datum-subsidy`],
  },
  {
    id: "datum-vs-stratum",
    title: "DATUM gateway or public stratum: which to use, and why stratum is winding down",
    keywords: ["datum", "stratum", "gateway", "which", "choose", "should", "difference", "vs", "versus", "own", "node", "sv1", "invalidate", "knots"],
    answer:
      "With your own DATUM gateway you run a Bitcoin Knots node for this chain, build your own block templates, pay 0% and earn the DATUM bonus; the pool only supplies the coinbase split. " +
      "The public stratum needs no node. It is a donation and validation endpoint at 100%, with a 24 h / 96 h grace at 25%, and it is capped (see 'self-cap'). Bitcoin Knots has also announced a future rule to invalidate coinbase payouts to addresses that hash through any pool's stratum (SV1); moving to another pool does not avoid it. " +
      "The pool therefore recommends your own DATUM gateway for everyone who can run a node.",
    links: [`${S}/mine-xbt`, "https://convoy.xyz/getstarted"],
  },
  {
    id: "connect",
    title: "How to connect a machine or a DATUM gateway",
    keywords: ["connect", "setup", "set", "up", "url", "port", "username", "password", "pubkey", "host", "config", "configure", "start", "how", "point"],
    answer:
      "Public stratum: stratum+tcp://stratum.lazarus-xbt.xyz:23334, username = your XBT payout address optionally followed by .workername, password x. Algorithm: BLAKE2b with a Sia-style header (any Siacoin ASIC; SHA-256 machines will not work). " +
      "DATUM: run Bitcoin Knots 29.4.2 (or later) for this chain, build a DATUM gateway, and put the pool's DATUM host (datum.lazarus-xbt.xyz), port (28915) and pool pubkey in its datum section; set mining.pool_address to your payout address and point your machines at your gateway. " +
      "connection_info returns the exact values including the pubkey. After connecting, check miner_overview with your address or gateway_status with your gateway's name.",
    links: [`${S}/connect`, `${S}/mine-xbt`, "https://convoy.xyz/getstarted"],
  },
  {
    id: "ratum",
    title: "Running a Ratum gateway (ratum-gateway, iohzrd's Rust DATUM gateway) at Lazarus",
    keywords: ["ratum", "ratum-gateway", "rust", "iohzrd", "omega", "b2pool", "ocminer", "p2block", "binary", "gateway", "release", "switch", "0.1.28", "0.1.53"],
    answer:
      "ratum-gateway reads the same datum_gateway_config.json as the C DATUM gateway and connects to Lazarus with no patch: 0% fee and the DATUM bonus, like any own gateway. " +
      "Set datum.pool_host datum.lazarus-xbt.xyz, pool_port 28915 and the pool pubkey (connection_info returns it), pooled_mining_only true, pool_pass_full_users true; set mining.pool_address to the operator's OWN address and coinbase_tag_secondary to the gateway's name; run Bitcoin Knots 29.4.2. " +
      "Lazarus tested iohzrd releases 0.1.28, 0.1.39, 0.1.53, master and the B2Pool (ocminer) and P2Block forks against its Prime on 27 Sep 2026: all connect, carry the full split, and book found blocks as split; pool-only work is a few milliseconds per new block. " +
      "One behaviour to know: when a Ratum gateway loses the pool (a Prime restart or a network drop) it keeps miners on work paying mining.pool_address alone for about 12-26 s before disconnecting them, so a block found in that gap pays pool_address, which is why it should be the operator's own address. " +
      "gateway_status shows a Ratum gateway as software 'ratum-gateway <version>'. Switching from the C gateway: same file, add api.miner_listen_port if port 8000 is taken; work in the window follows the address, not the gateway.",
    links: [`${S}/ratum`, "https://github.com/iohzrd/ratum/releases"],
  },
  {
    id: "which-gateway",
    title: "Which DATUM gateway build to run",
    keywords: ["gateway", "datum", "build", "builds", "which", "should", "run", "best", "recommend", "recommended", "fork", "convoy", "flytheelephant", "fte", "datum_gateway", "version"],
    answer:
      "Several gateway builds connect to Lazarus with no patch. The pool ranks them by what it measures against its own Prime (whether a found block pays the whole window, and how often a gateway hands miners a job whose coinbase pays only the pool); " +
      "the ranked list, best first, is connection_info.gateway_builds and the 'Which DATUM gateway' list on the Connect page. As of 28 Sep 2026 every C datum_gateway build (FlyTheElephant, iohzrd, CONVOY) can hand out a pool-only job for a moment after each new block and for as long as the pool's split is late; " +
      "a small patch closes that for FlyTheElephant's build (see 'flytheelephant'). ratum-gateway, iohzrd's Rust gateway, is also on the list (see 'ratum'). Whatever the build, run it against Bitcoin Knots 29.4.2 (see 'node-version').",
    links: [`${S}/connect`, `${S}/flytheelephant`, `${S}/ratum`],
  },
  {
    id: "flytheelephant",
    title: "Mining with FlyTheElephant's datum_gateway",
    keywords: ["flytheelephant", "fly", "elephant", "fte", "flytheelephant1", "yuge", "gateway", "guide", "setup"],
    answer:
      "FlyTheElephant1/datum_gateway (master a5f28aa, 20 Sep 2026) is CONVOY's BLAKE2b code plus FlyTheElephant's fixes: every miner gets the whole split coinbase, fitted to the block's weight and sigop limits. " +
      "Build it with cmake, set pool_host datum.lazarus-xbt.xyz, pool_port 28915 and the pool pubkey (connection_info), pool_address to your own address, stratum.vardiff_min 4096, and pooled_mining_only true; point machines at the gateway with username address.worker. " +
      "Tested against the pool on regtest: handshake, pubkey pinning, split blocks paying the whole window, usernames, the 0% DATUM fee, reconnects and the pool's template checks all pass. Like every C build it can briefly serve a pool-only job after a new block; " +
      "lazarus/patches/datum-gateway-fte-late-coinbaser.patch fixes that and has been offered upstream. The full guide, with a complete config and troubleshooting, is the page linked here.",
    links: [`${S}/flytheelephant`, "https://github.com/FlyTheElephant1/datum_gateway"],
  },
  {
    id: "node-version",
    title: "Which node version a DATUM gateway needs (Knots 29.4.2) and what goes wrong on an old one",
    keywords: ["upgrade", "version", "29.4.2", "29.4.1", "knots", "node", "old", "outdated", "invalid", "rejected", "premature"],
    answer:
      "Gateways must run Bitcoin Knots 29.4.2 or later. Its long-coinbase-maturity rule (see 'coinbase-maturity') is consensus from block 973,440; an older node can build a block that spends a coinbase too young under the new rule, and the chain rejects that block. " +
      "A rejected block pays nobody, so it costs every miner in the TIDES window, not only the gateway that built it. gateway_status shows a gateway's build and any flags.",
    links: [`${S}/mine-xbt`],
  },
  {
    id: "tides",
    title: "TIDES: the rolling window, your share, and why a new miner's payout grows",
    keywords: ["tides", "window", "share", "pplns", "rolling", "work", "proportion", "grow", "new", "difficulty", "8x"],
    answer:
      "TIDES keeps a rolling window of recent work worth about eight times network difficulty. When any block is found, its coinbase pays every address in the window in proportion to its work there. " +
      "A new miner's share grows until the window has turned over once (roughly the time the pool takes to do eight network-difficulties of work); a miner who stops keeps being paid by blocks found until their work ages out. " +
      "There is no pool balance, no withdrawal and no custody: every payout is an output of a block's own coinbase, sent straight to the address.",
    links: [`${S}/tides`, `${S}/non-custodial`],
  },
  {
    id: "carry",
    title: "Carry: earnings too small for a coinbase output, and when they are paid",
    keywords: ["carry", "carried", "unpaid", "small", "floor", "minimum", "under", "dust", "forward", "accumulate", "why", "not", "paid"],
    answer:
      "Each coinbase output must be at least the pool's minimum output (0.005 XBT as of 26 Sep 2026; miner_audit shows the live figure). If your share of a block is below it, or the coinbase is already full, your earned value is not lost: it is added to your carry. " +
      "Carry is paid, on top of your share, in the first later coinbase where share plus carry clears the minimum. The DATUM bonus is credited as carry too. " +
      "miner_audit reports your carry, whether you are in the coinbase being built right now, and the reason if not.",
    links: [`${S}/tides`],
  },
  {
    id: "min-payout",
    title: "Minimum payout and payout thresholds",
    keywords: ["minimum", "min", "threshold", "payout", "0.005", "smallest", "withdraw", "withdrawal"],
    answer:
      "There is no withdrawal threshold because there is no balance. There is a minimum coinbase output (0.005 XBT as of 26 Sep 2026): value below it is carried and paid in a later coinbase (see 'carry'). " +
      "A small miner therefore gets fewer, larger outputs rather than many tiny ones.",
    links: [`${S}/non-custodial`],
  },
  {
    id: "coinbase-maturity",
    title: "Coinbase maturity (Knots #419): why mined coins wait about 45 days, and when they unlock",
    keywords: ["maturity", "mature", "immature", "419", "6480", "6,480", "6481", "locked", "lock", "spend", "spendable", "unlock", "wait", "confirmations", "relay", "policy", "november", "45", "35"],
    answer:
      "Coinbase outputs normally become spendable after 100 confirmations. Bitcoin Knots #419 (in 29.4.2) made coinbases mined from block 973,440 up to 979,919 need 6,480 confirmations under consensus; the rule releases at block 979,920 (around 5 Nov 2026). " +
      "Separately, Knots 29.4.2's relay policy applies the 6,480-confirmation wait to every coinbase spend, including older coinbases, so nodes will not relay a spend younger than that even where consensus would allow it. " +
      "A wallet that counts 100 confirmations may call a payout mature that still cannot move. The pool reports the later of the two (consensus and relay), so its unlock heights are the conservative ones; miner_immature lists yours with estimated dates. Nothing is lost: the coins are yours on chain from the moment the block is found.",
    links: [`${S}/non-custodial`, "https://github.com/bitcoinknots/bitcoin/pull/419"],
  },
  {
    id: "makegoods",
    title: "Make-goods: partial and pool-only blocks, and when they are paid",
    keywords: ["makegood", "make-good", "makegoods", "make-goods", "owed", "partial", "pool-only", "poolonly", "queued", "refund", "missing", "shorted", "not", "in", "coinbase"],
    answer:
      "Sometimes a block is found on a job whose coinbase paid only part of the window (partial) or only the pool (pool-only), usually because a stock DATUM gateway sent miners a job before the pool's split arrived. The block is valid; the pool then owes the window what the coinbase left out. " +
      "It repays with a make-good: a transaction signed when the block is found, spending that same block's pool output to the addresses that were left out, so its txid is known in advance. It is queued until the chain will accept it (the coinbase it spends must mature; see 'coinbase-maturity'), then broadcast. " +
      "Status queued means waiting for that height; paid means broadcast and confirmed. miner_makegoods lists yours with the payable height and estimated date, and verify_payout checks a paid one on chain. Make-goods are delayed, not lost.",
    links: [`${S}/non-custodial`],
  },
  {
    id: "payout-timing",
    title: "When am I paid? From found block to spendable coins",
    keywords: ["when", "paid", "payout", "timing", "time", "arrive", "receive", "wallet", "see", "show", "delay", "how", "long"],
    answer:
      "You are paid the moment the pool finds a block: your output is in that block's coinbase, visible in the explorer and in your wallet as immature. It becomes spendable after the maturity wait (see 'coinbase-maturity'). " +
      "If you had no output in that block (under the minimum, or the coinbase was full), the value is carried to a later block. If the block was partial or pool-only, the missing part comes as a make-good once that block's coinbase matures. " +
      "How often the pool finds blocks depends on its share of the network and luck; pool_status gives the expected time to the next block.",
    links: [`${E}/mining/pool/lazarus`],
  },
  {
    id: "self-cap",
    title: "The 15% stratum self-cap and the overflow relay",
    keywords: ["cap", "15", "self-cap", "overflow", "relay", "relayed", "limit", "centralization", "other", "pools", "13"],
    answer:
      "The pool holds its public stratum to 15% of network hashrate. Above that, a new stratum connection is relayed to one of several other pools and paid by that pool under the same address, with nothing credited to the Lazarus window; it stops relaying below 13%. " +
      "Miners already connected keep mining here, and hashrate behind a miner's own DATUM gateway is never counted or relayed, because miners building their own templates do not centralize the chain. pool_status shows the live share and whether relaying is on.",
    links: [`${S}/self-cap`],
  },
  {
    id: "solo",
    title: "Solo mining",
    keywords: ["solo", "alone", "whole", "block", "finder", "lottery"],
    answer:
      "Solo is separate from the pool: a block you find pays your address the whole block less the 7.5% solo fee, and between blocks you earn nothing. Solo work never enters the TIDES window. " +
      "See the connect page for the solo endpoints.",
    links: [`${S}/connect`],
  },
  {
    id: "wallets-replay",
    title: "Wallets, replay protection and the chain split",
    keywords: ["wallet", "wallets", "replay", "protection", "split", "fork", "shrike", "electrum", "seed", "claim", "address", "fresh", "sighash", "btcb2", "xbt"],
    answer:
      "This chain (XBT, listed as BTCB2) split from Bitcoin at block 961,632 and has mined with BLAKE2b since block 961,640. Coins mined here, including every pool payout, exist only on this chain, so they cannot be replayed. " +
      "Coins held before the split exist on both chains under the same keys; protection is opt-in per transaction with the SIGHASH_UNIFIED flag (0x21), which the Shrike wallet uses by default for this chain (check its send screen). Use a fresh address here. " +
      "There is no claim process: anyone asking for a seed phrase is a scammer. Wallets that count 100-confirmation maturity show different balances from ones that count 6,480 (see 'coinbase-maturity').",
    links: [`${S}/bip110`],
  },
  {
    id: "verify",
    title: "Don't trust, verify: checking a payout on chain yourself",
    keywords: ["verify", "check", "proof", "prove", "trust", "onchain", "on-chain", "explorer", "audit", "confirm", "independent"],
    answer:
      "Every payout is an output on chain. verify_payout(address, height) fetches the block's coinbase from the explorer and compares the output to your address with what the pool reports, and does the same for any make-good transaction recorded for that block. " +
      "To do it by hand: open the block in the explorer, open its first (coinbase) transaction, and find your address among the outputs; for a make-good, open its txid.",
    links: [`${E}`],
  },
  {
    id: "read-audit",
    title: "How to read miner_audit",
    keywords: ["audit", "read", "explain", "report", "fields", "meaning", "understand", "estimate", "estimated", "numbers"],
    answer:
      "paid: coinbase outputs to you that have matured and been counted as paid. immature: outputs already on chain but still inside the maturity wait, with the next unlock heights. carry: earned value waiting for an output (see 'carry'). " +
      "make_goods: amounts the pool owes you from partial or pool-only blocks, split into queued (waiting for a height), paid and failed. bonus: DATUM bonus credited so far. window: your share of the current TIDES window and what the next block would pay you. " +
      "Figures marked estimate (earnings per day, unlock dates) depend on luck and on how fast blocks come; heights are exact, dates are not. Every amount can be checked with verify_payout.",
    links: [`${S}/tides`],
  },
  {
    id: "privacy",
    title: "What this MCP server can see and do",
    keywords: ["privacy", "private", "data", "store", "stored", "log", "safe", "security", "secure", "read-only", "readonly", "keys", "limits", "rate"],
    answer:
      "It is read-only and holds no credentials: it reads the same public pool and explorer APIs a browser does, so it can see nothing private, cannot move coins and cannot change any setting. Your questions are not stored. " +
      "Limits: 30 tool calls a minute per client, 8 per-address units a minute per client (miner_audit and verify_payout count 2), and 20 lookups a minute of any one address across all clients.",
    links: ["https://mcp.lazarus-xbt.xyz"],
  },
];

const STOP = new Set("a an and are be can do does for from how i in is it its me my of on or the to what when where which who why will with you your".split(" "));
const words = (s) => String(s).toLowerCase().replace(/[^a-z0-9.,#%-]+/g, " ").split(" ").map((w) => w.replace(/^[.,-]+|[.,-]+$/g, "")).filter((w) => w && !STOP.has(w));

/** Best entries for a topic id or a free-text question, with a score so an assistant can tell a weak match. */
export function searchFaq(query, max = 3) {
  const q = String(query).trim().toLowerCase();
  const exact = FAQ.find((f) => f.id === q);
  if (exact) return [{ ...exact, score: 100 }];
  const qw = words(q);
  const scored = FAQ.map((f) => {
    const kw = new Set(f.keywords), tw = new Set(words(f.title + " " + f.id.replace(/-/g, " ")));
    let score = 0;
    for (const w of qw) score += (kw.has(w) ? 3 : 0) + (tw.has(w) ? 2 : 0) + (f.answer.toLowerCase().includes(w) && w.length > 3 ? 1 : 0);
    return { ...f, score };
  }).filter((f) => f.score > 0);
  return scored.sort((a, b) => b.score - a.score).slice(0, max);
}
