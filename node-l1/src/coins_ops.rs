//! The coins-cache differential's harness (O2a-CO; CH2c's `diff_coins_ops` drives it): one op
//! script run on CH2's [`CoinsCache`] and [`BlockOverlay`] over the in-memory engine, and on
//! Core's `CCoinsViewDB` ← tip `CCoinsViewCache` ← child (`oracle_coins_ops`), op by op.
//!
//! The script is the subset both sides share. On the tip: AddCoin, SpendCoin, AccessCoin,
//! HaveCoin, HaveCoinInCache, Uncache, SetBestBlock, GetBestBlock and Flush (CH2's freeze, W's
//! write of F's DIRTY entries and the release, all at once). With a block's view open (Core's
//! child, CH2's overlay): AddCoin, SpendCoin with moveout, AccessCoin, HaveCoin, the tip's
//! HaveCoinInCache, and the block's end, committed (SetBestBlock, Flush and pop: CH2's
//! `commit(best)`) or dropped. Each flush compares the database: the coin records, the best block
//! (CH2's durable one) and no head-blocks marker.
//!
//! Two differences are by design, and end the comparison (or skip one answer) instead of
//! diverging ([`Allowed`]):
//!
//! - DEC-131: the overlay's add without `possible_overwrite` also refuses a coin the tip holds
//!   unspent, where Core's child adds it FRESH and throws at its flush (`coins.cpp:221`), or never
//!   if the block spends it first.
//! - CH2 §2.4.2, O-R6-m3: the overlay's reads (`coin`, `have_coin`) cache nothing, where Core's
//!   child caches the coin in itself and in the tip (`FetchCoin` at each level). Until the next
//!   tip flush a key read through the block's view may then be in Core's tip only: its
//!   HaveCoinInCache answer is skipped, and an add or a commit that only Core refuses on it ends
//!   the comparison.
//!
//! CH2c (`CH2c-notes.md` D2-D5) adds two modes, chosen by the input's first byte, and three
//! node-only scheduling ops (`Freeze`, `WStep`, `Release`) that Core never sees. A twin of Core's
//! views ([`RefCoinsCache`]) runs beside both sides. After every op Core's `DynamicMemoryUsage()`
//! must equal [`CoreUsage`] driven by the twin's tip (D4: the proxy's Core model, not CH2's ledger,
//! DEC-102), and `GetCacheSize()` the twin's size; an `Entry` op checks the twin's flags.
//!
//! - **Aligned**: the scheduling ops are dropped, so CH2 flushes where Core does. After every op
//!   CH2's active layer must hold exactly the twin's tip entries with Core's DIRTY/FRESH flags
//!   (CH2 §2.4.5's "equal" case), apart from the keys only Core's tip read through a block's view.
//! - **Hidden**: the scheduling ops run, so F is frozen, written and released between Core's
//!   flushes. Values, errors, `GetBest` and the database at each tip flush are compared; the
//!   cache's membership and flags are not (DEC-102), and those answers are counted as uncompared.
//!   A third difference by design ends a run: Core's tip still holds a coin unspent that CH2
//!   already released to the database, so only Core refuses to add it again
//!   ([`Allowed::FlushPoint`]). So does a freeze of a coin Core took FRESH over its database's
//!   unspent one ([`Allowed::FreshOverBase`]).
//!
//! Not compared: `Sync` and `BatchWrite` with a caller's map, which CH2 does not have.

use std::collections::BTreeSet;
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_consensus::coins::{Coin, CoinRef, CoinsView as _, MemoryCoins};
use dash_consensus::primitives::BlockHash;
use dash_oracle_sys::coins_ops::{CacheEntry, DEFAULT_DB_BATCH_SIZE, OutPoint, TIP};
use dash_oracle_sys::{CoinsOp, CoinsOutcome, CoinsOutput, DbState, Oracle, Pin};
use dash_types::Numeric as _;
use dashd_chainstate::CoinsError;
use dashd_chainstate::coins::reference::{Over, RefCoinsCache, RefEntry};
use dashd_chainstate::coins::{
    BlockOverlay, BlockRef, CoinLayerOps as _, CoinSrc, CoinsBudget, CoinsCache, CoinsTable, Flags,
    OutPointKey, PackedCoin, Ratio, Salt,
};
use dashd_chainstate::store::{
    CHAINSTATE_SCHEMA, CoinsReadError, GlobalFailStop, GuardedEngine, RecordingSink,
};
use dashd_store::{Commit, KvEngine as _, MemoryEngine};

use crate::core_usage::{CoreUsage, coin_usage};
use crate::{Compared, Divergence, Outcome, check, divergence};

/// The outpoints a script touches: 16, so its ops meet.
pub const KEYS: u8 = 16;
/// The longest script [`script_from_bytes`] makes.
pub const MAX_OPS: usize = 4096;
/// The child view's level in Core's stack.
const CHILD: u8 = 1;

/// How the node's flushes are placed against Core's (CH2c D3): bit 0 of the input's first byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The scheduling ops are dropped: CH2 flushes only at Core's flushes, and its active layer is
    /// compared with Core's map after every op.
    Aligned,
    /// The scheduling ops run: F is frozen, written and released between Core's flushes.
    Hidden,
}

impl Mode {
    /// The mode an input's first byte selects.
    pub fn from_byte(b: u8) -> Self {
        if b & 1 == 0 {
            Self::Aligned
        } else {
            Self::Hidden
        }
    }

    /// The first byte that selects this mode.
    pub fn byte(self) -> u8 {
        match self {
            Self::Aligned => 0,
            Self::Hidden => 1,
        }
    }
}

/// One op of a script. Coins and outpoints are bytes, read by [`coin`] and [`outpoint`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    TipAdd {
        key: u8,
        coin: u8,
        possible_overwrite: bool,
    },
    TipSpend {
        key: u8,
    },
    TipAccess {
        key: u8,
    },
    TipHave {
        key: u8,
    },
    /// Allowed with a block's view open: it reads the tip only.
    TipHaveInCache {
        key: u8,
    },
    TipUncache {
        key: u8,
    },
    /// The tip's best block becomes the block at `height` ([`block`]).
    SetBest {
        height: u32,
    },
    GetBest,
    /// A tip flush. The script must have set a best block: Core asserts at a null one (oracle.h),
    /// and CH2 freezes only at a best block.
    Flush,
    /// Opens a block's view.
    Push,
    ChildAdd {
        key: u8,
        coin: u8,
        possible_overwrite: bool,
    },
    ChildSpend {
        key: u8,
    },
    ChildAccess {
        key: u8,
    },
    ChildHave {
        key: u8,
    },
    /// The block at `height` connected: its view's best block set, flushed into the tip, closed.
    Commit {
        height: u32,
    },
    /// The block's view closed without a flush.
    Drop,
    /// Core's flags and coin of the tip's entry (`GetCoinsMapEntry`). Allowed with a block's view
    /// open: it reads the tip only.
    Entry {
        key: u8,
    },
    /// Node only (hidden mode): active frozen as F at the tip's best block. The script has a best
    /// block, no F in flight and no block's view open (CH2 never freezes mid-block, DEC-95).
    Freeze,
    /// Node only: F's next `n` DIRTY entries written to the database in one batch.
    WStep {
        n: u8,
    },
    /// Node only: F's remaining entries written, and F released.
    Release,
}

/// Why a comparison ended, or skipped an answer, where the two sides differ by design.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Allowed {
    /// DEC-131: the overlay refused an add Core's child accepted.
    FreshAtAdd,
    /// CH2 §2.4.2, O-R6-m3: a key read through the block's view.
    ChildReadCaching,
    /// CH2c D3 (hidden mode): CH2 released the coin to the database at a flush point of its own,
    /// where Core's tip still holds it unspent, so only Core refuses an add of it.
    FlushPoint,
    /// CH2c D26 (hidden mode): a freeze of a FRESH coin added without `possible_overwrite` over
    /// one Core's database holds unspent, which no view above it cached; or such an add, refused
    /// by CH2 only because F still holds the coin. Core's `AddCoin` checks
    /// only its own map (`coins.cpp:78-96`), so a spend erases the FRESH coin and leaves the
    /// database's; CH2's spend after the freeze writes a tombstone over F instead. Unreachable on a
    /// valid chain (only a coinbase overwrites, with `possible_overwrite`); aligned mode compares it.
    FreshOverBase,
}

/// What a run compared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoinsOpsRun {
    /// The script's ops compared, the last included.
    pub ops: usize,
    pub flushes: usize,
    pub commits: usize,
    /// Both sides threw Core's `logic_error` at the last op.
    pub both_logic_error: bool,
    /// The by-design difference that ended the run.
    pub ended_by: Option<Allowed>,
    /// Tip HaveCoinInCache answers skipped as [`Allowed::ChildReadCaching`].
    pub skipped: usize,
    /// Hidden mode: membership answers (HaveCoinInCache, Entry, the active layer) not compared.
    pub uncompared: usize,
    /// Core's `DynamicMemoryUsage()` answers checked against [`CoreUsage`].
    pub usage_checks: usize,
    /// Of them, a commit's whose bucket count depended on Core's map order (two candidates).
    pub usage_ambiguous: usize,
    pub freezes: usize,
    pub w_steps: usize,
}

/// Outpoint `key % KEYS`: 8 transactions of 2 outputs.
pub fn outpoint(key: u8) -> OutPoint {
    let key = key % KEYS;
    OutPoint {
        txid: [key / 2 + 1; 32],
        vout: u32::from(key % 2),
    }
}

fn node_key(key: u8) -> OutPointKey {
    let op = outpoint(key);
    let mut k = [0; 36];
    k[..32].copy_from_slice(&op.txid);
    k[32..].copy_from_slice(&op.vout.to_le_bytes());
    OutPointKey(k)
}

/// A coin of one of four shapes: P2PKH (compressed), a long script, an unspendable output (which
/// neither side adds) and a coinbase P2PKH.
pub fn coin(b: u8) -> Coin {
    let p2pkh = |fill: u8| {
        let mut s = vec![0x76, 0xa9, 0x14];
        s.extend_from_slice(&[fill; 20]);
        s.extend_from_slice(&[0x88, 0xac]);
        s
    };
    let value = 1 + i64::from(b) * 1_000;
    let height = u32::from(b) + 1;
    match b % 4 {
        0 => Coin::new(value, &p2pkh(b), height, false),
        1 => Coin::new(value, &vec![0x51; 40 + usize::from(b % 64)], height, false),
        2 => Coin::new(value, &[0x6a, b], height, false),
        _ => Coin::new(value, &p2pkh(b), height, true),
    }
}

/// A coin's bytes as both sides store it: Core's `Coin` serialization.
fn packed(c: CoinRef<'_>) -> Vec<u8> {
    PackedCoin::from_coin(c).as_bytes().to_vec()
}

/// The block at `height`: its hash derived from the height, never null.
pub fn block(height: u32) -> BlockRef {
    let mut hash = [0xb1; 32];
    hash[..4].copy_from_slice(&height.to_le_bytes());
    BlockRef {
        id: BlockId(height),
        hash: BlockHash::from(hash),
        height,
    }
}

/// A best block's hash as Core holds it, null for none.
fn best_hash(b: Option<BlockRef>) -> [u8; 32] {
    b.map_or([0; 32], |b| b.hash.to_lendian())
}

/// Op bytes from here up are CH2c's ops ([`Op::Entry`] and the scheduling ops); below it the codes
/// are O2a-CO's. O2a-CO's corpus does not carry over: an input now starts with its mode byte, and
/// its bytes from here up decode differently.
const EXTENDED: u8 = 0xf0;

/// A script from fuzz bytes, valid by construction: child ops only with a block's view open, tip
/// ops other than HaveCoinInCache and Entry only with none, a flush only after a best block,
/// heights rising, and the scheduling ops only in [`Mode::Hidden`] with no block's view open: with
/// no F in flight a freeze (given a best block), else a release or a W step.
pub fn script_from_bytes(mode: Mode, data: &[u8]) -> Vec<Op> {
    let mut input = data.iter().copied();
    let mut ops = Vec::new();
    let (mut open, mut best, mut frozen, mut height) = (false, false, false, 1);
    while let Some(b) = input.next() {
        if ops.len() == MAX_OPS {
            break;
        }
        let mut arg = || input.next().unwrap_or(0);
        let op = if b >= EXTENDED {
            let a = arg();
            if b & 1 == 0 {
                Op::Entry {
                    key: a,
                }
            } else if open || mode == Mode::Aligned {
                continue;
            } else {
                match (frozen, a % 4) {
                    (false, _) if best => {
                        frozen = true;
                        Op::Freeze
                    }
                    (false, _) => continue,
                    (true, 0) => {
                        frozen = false;
                        Op::Release
                    }
                    (true, _) => Op::WStep {
                        n: a >> 2,
                    },
                }
            }
        } else if open {
            match b % 8 {
                0 | 1 => {
                    let (key, coin) = (arg(), arg());
                    Op::ChildAdd {
                        key,
                        coin,
                        possible_overwrite: coin & 0x80 != 0,
                    }
                }
                2 => Op::ChildSpend {
                    key: arg(),
                },
                3 => Op::ChildAccess {
                    key: arg(),
                },
                4 => Op::ChildHave {
                    key: arg(),
                },
                5 => Op::TipHaveInCache {
                    key: arg(),
                },
                6 => {
                    open = false;
                    best = true;
                    height += 1;
                    Op::Commit {
                        height,
                    }
                }
                _ => {
                    open = false;
                    Op::Drop
                }
            }
        } else {
            match b % 10 {
                0 | 1 => {
                    let (key, coin) = (arg(), arg());
                    Op::TipAdd {
                        key,
                        coin,
                        possible_overwrite: coin & 0x80 != 0,
                    }
                }
                2 => Op::TipSpend {
                    key: arg(),
                },
                3 => Op::TipAccess {
                    key: arg(),
                },
                4 => Op::TipHave {
                    key: arg(),
                },
                5 => Op::TipHaveInCache {
                    key: arg(),
                },
                6 => Op::TipUncache {
                    key: arg(),
                },
                7 if arg() & 1 == 0 => Op::GetBest,
                8 if best => {
                    frozen = false;
                    Op::Flush
                }
                7 | 8 => {
                    best = true;
                    height += 1;
                    Op::SetBest {
                        height,
                    }
                }
                _ => {
                    open = true;
                    Op::Push
                }
            }
        };
        ops.push(op);
    }
    ops
}

/// An input's mode (its first byte) and script (the rest).
pub fn input_script(data: &[u8]) -> (Mode, Vec<Op>) {
    match data.split_first() {
        Some((&m, rest)) => {
            let mode = Mode::from_byte(m);
            (mode, script_from_bytes(mode, rest))
        }
        None => (Mode::Aligned, Vec::new()),
    }
}

/// Each op's Core ops.
fn core_ops(op: Op) -> Vec<CoinsOp> {
    let add = |view, key, coin: u8, possible_overwrite| CoinsOp::Add {
        view,
        outpoint: outpoint(key),
        coin: packed(self::coin(coin).as_ref()),
        possible_overwrite,
    };
    let spend = |view, key, moveout| CoinsOp::Spend {
        view,
        outpoint: outpoint(key),
        moveout,
    };
    let access = |view, key| CoinsOp::Access {
        view,
        outpoint: outpoint(key),
    };
    let have = |view, key| CoinsOp::Have {
        view,
        outpoint: outpoint(key),
    };
    let set_best = |view, height| CoinsOp::SetBestBlock {
        view,
        hash: best_hash(Some(block(height))),
    };
    let flush = |view| CoinsOp::Flush {
        view,
    };
    match op {
        Op::TipAdd {
            key,
            coin,
            possible_overwrite,
        } => vec![add(TIP, key, coin, possible_overwrite)],
        Op::TipSpend {
            key,
        } => vec![spend(TIP, key, false)],
        Op::TipAccess {
            key,
        } => vec![access(TIP, key)],
        Op::TipHave {
            key,
        } => vec![have(TIP, key)],
        Op::TipHaveInCache {
            key,
        } => vec![CoinsOp::HaveInCache {
            view: TIP,
            outpoint: outpoint(key),
        }],
        Op::TipUncache {
            key,
        } => vec![CoinsOp::Uncache {
            view: TIP,
            outpoint: outpoint(key),
        }],
        Op::SetBest {
            height,
        } => vec![set_best(TIP, height)],
        Op::GetBest => vec![CoinsOp::GetBestBlock {
            view: TIP,
        }],
        Op::Flush => vec![flush(TIP)],
        Op::Push => vec![CoinsOp::Push],
        Op::ChildAdd {
            key,
            coin,
            possible_overwrite,
        } => vec![add(CHILD, key, coin, possible_overwrite)],
        Op::ChildSpend {
            key,
        } => vec![spend(CHILD, key, true)],
        Op::ChildAccess {
            key,
        } => vec![access(CHILD, key)],
        Op::ChildHave {
            key,
        } => vec![have(CHILD, key)],
        Op::Commit {
            height,
        } => vec![set_best(CHILD, height), flush(CHILD), CoinsOp::Pop],
        Op::Drop => vec![CoinsOp::Pop],
        Op::Entry {
            key,
        } => vec![CoinsOp::Entry {
            view: TIP,
            outpoint: outpoint(key),
        }],
        Op::Freeze
        | Op::WStep {
            ..
        }
        | Op::Release => Vec::new(),
    }
}

/// The script as Core runs it: each op's Core ops, then the tip's USAGE.
pub fn core_script(ops: &[Op]) -> Vec<CoinsOp> {
    ops.iter().flat_map(|&op| with_usage(op)).collect()
}

fn with_usage(op: Op) -> Vec<CoinsOp> {
    let mut ops = core_ops(op);
    ops.push(CoinsOp::Usage {
        view: TIP,
    });
    ops
}

/// Core's answer to one op.
enum Core {
    /// Each of its Core ops' outputs.
    Ok(Vec<CoinsOutput>),
    /// One of its Core ops threw a `logic_error`, which ended the script.
    LogicError,
}

/// Core's outcomes, consumed op by op.
struct CoreRun {
    outcomes: std::vec::IntoIter<CoinsOutcome>,
}

impl CoreRun {
    /// Core's answer to `op`, and the tip's `DynamicMemoryUsage()` and `GetCacheSize()` after it
    /// (none once a `logic_error` ended the script).
    fn next(&mut self, op: Op) -> Result<(Core, Option<(u64, u64)>), Divergence> {
        let n = core_ops(op).len();
        let mut outputs = Vec::with_capacity(n);
        for _ in 0..=n {
            match self.outcomes.next() {
                Some(CoinsOutcome::Ok(out)) => outputs.push(out),
                Some(CoinsOutcome::LogicError(_)) => return Ok((Core::LogicError, None)),
                Some(CoinsOutcome::Assert(_)) => return Err(divergence("core assert")),
                None => return Err(divergence("core outcomes")),
            }
        }
        match outputs.pop() {
            Some(CoinsOutput::Usage {
                dynamic_usage,
                cache_size,
            }) => Ok((Core::Ok(outputs), Some((dynamic_usage, u64::from(cache_size))))),
            _ => Err(divergence("core usage")),
        }
    }
}

fn one(core: Core, field: &'static str) -> Result<Option<CoinsOutput>, Divergence> {
    match core {
        Core::Ok(mut outputs) if outputs.len() == 1 => Ok(outputs.pop()),
        Core::Ok(_) => Err(divergence(field)),
        Core::LogicError => Ok(None),
    }
}

fn expect(core: Core, want: CoinsOutput, field: &'static str) -> Result<(), Divergence> {
    match one(core, field)? {
        Some(out) if out == want => Ok(()),
        _ => Err(divergence(field)),
    }
}

/// Whether the comparison goes on after an op, or ended: alike on both sides (`None`) or by
/// design.
enum Step {
    Continue,
    Ended(Option<Allowed>),
}

impl Step {
    fn ended(&self) -> bool {
        matches!(self, Self::Ended(_))
    }
}

/// Core's views as ported line by line ([`RefCoinsCache`]): the database, the tip and a block's
/// child, run op by op beside both sides. Its tip drives [`CoreUsage`] (D4) and answers what Core's
/// tip holds.
struct Twin {
    db: MemoryCoins,
    tip: RefCoinsCache,
    child: Option<RefCoinsCache>,
    /// The tip map's keys after the last op.
    keys: BTreeSet<OutPointKey>,
    usage: CoreUsage,
    /// The last op's `AddCoin` emplaced a key the tip held ([`CoreUsage::emplace_existing`]).
    emplaced_existing: bool,
}

impl Twin {
    fn new() -> Self {
        Self {
            db: MemoryCoins::new(),
            tip: RefCoinsCache::new(),
            child: None,
            keys: BTreeSet::new(),
            usage: CoreUsage::new(),
            emplaced_existing: false,
        }
    }

    /// Core's tip holds `k` unspent.
    fn tip_holds(&self, k: &OutPointKey) -> bool {
        self.tip.have_coin_in_cache(k)
    }

    /// FlushPoint's premise (D17): CH2 released `k` to its database `db` with the coin Core's tip
    /// still holds unspent. A coin CH2 lost is a divergence, not a flush point.
    fn released(
        &self,
        db: &GuardedEngine<MemoryEngine>,
        k: &OutPointKey,
    ) -> Result<bool, Divergence> {
        let ours =
            CoinsTable::default().get(db, k).map_err(|_| divergence("node database read"))?;
        Ok(match (ours, self.tip.entry(k).and_then(|e| e.coin.as_ref())) {
            (Some(p), Some(c)) => p.as_ref().to_coin() == *c,
            _ => false,
        })
    }

    /// [`Allowed::FreshOverBase`]'s premise for an add of `c` at `k` (on the child if `child`):
    /// Core accepts it FRESH over its database's unspent coin.
    fn fresh_over_base(
        &self,
        k: &OutPointKey,
        c: &Coin,
        possible_overwrite: bool,
        child: bool,
    ) -> bool {
        let uncached = |v: &RefCoinsCache| v.entry(k).is_none();
        !possible_overwrite
            && !c.as_ref().script_pubkey().is_unspendable()
            && uncached(&self.tip)
            && (!child || self.child.as_ref().is_some_and(uncached))
            && self.db.coin(&k.outpoint()).is_ok_and(|db| db.is_some())
    }

    /// What Core's `SpendCoin(k)` on the tip returns (`coins.cpp:129-131`): whether `FetchCoin`
    /// finds an entry, spent or not, in the tip or unspent in its database.
    fn spend_finds(&self, k: &OutPointKey) -> bool {
        self.tip.entry(k).is_some() || self.db.coin(&k.outpoint()).is_ok_and(|c| c.is_some())
    }

    /// Core's tip view has `k` unspent: its cached entry, else its database's (no fetch).
    fn unspent(&self, k: &OutPointKey) -> bool {
        match self.tip.entry(k) {
            Some(e) => e.coin.is_some(),
            None => self.db.coin(&k.outpoint()).is_ok_and(|c| c.is_some()),
        }
    }

    /// Runs `op` as Core does: false where Core throws its `logic_error`. Returns the usage
    /// candidates after it: one, or two where a commit's inserts and erases into the tip came in
    /// Core's map order, which decides whether the bucket array grew on the way.
    fn apply(&mut self, op: Op) -> Result<(bool, Vec<CoreUsage>), Divergence> {
        let Self {
            db,
            tip,
            child,
            emplaced_existing,
            ..
        } = self;
        let structure = || divergence("script structure");
        let mut ok = true;
        match op {
            Op::TipAdd {
                key,
                coin: b,
                possible_overwrite,
            } => {
                let (k, c) = (node_key(key), coin(b));
                // `AddCoin` returns before its emplace for an unspendable output.
                *emplaced_existing =
                    tip.entry(&k).is_some() && !c.as_ref().script_pubkey().is_unspendable();
                ok = tip.add_coin(&k, c, possible_overwrite).is_ok();
            }
            Op::TipSpend {
                key,
            } => {
                tip.spend_coin(db, &node_key(key));
            }
            Op::TipAccess {
                key,
            } => {
                tip.access_coin(db, &node_key(key));
            }
            Op::TipHave {
                key,
            } => {
                tip.have_coin(db, &node_key(key));
            }
            Op::TipUncache {
                key,
            } => tip.uncache(&node_key(key)),
            Op::SetBest {
                height,
            } => tip.set_best_block(Some(block(height).hash)),
            Op::GetBest => {
                tip.best_block(db);
            }
            Op::Flush => tip.flush(db),
            Op::Push => *child = Some(RefCoinsCache::new()),
            Op::ChildAdd {
                key,
                coin: b,
                possible_overwrite,
            } => {
                let c = child.as_mut().ok_or_else(structure)?;
                ok = c.add_coin(&node_key(key), coin(b), possible_overwrite).is_ok();
            }
            Op::ChildSpend {
                key,
            }
            | Op::ChildAccess {
                key,
            }
            | Op::ChildHave {
                key,
            } => {
                let c = child.as_mut().ok_or_else(structure)?;
                let mut base = Over {
                    cache: tip,
                    base: db,
                };
                let k = node_key(key);
                // The child's spend (with moveout), access and have each fetch through the tip.
                match op {
                    Op::ChildSpend {
                        ..
                    } => {
                        c.spend_coin(&mut base, &k);
                    }
                    _ => {
                        c.access_coin(&mut base, &k);
                    }
                }
            }
            Op::Commit {
                height,
            } => {
                let mut c = child.take().ok_or_else(structure)?;
                c.set_best_block(Some(block(height).hash));
                ok = tip.batch_write(c).is_ok();
            }
            Op::Drop => *child = None,
            Op::TipHaveInCache {
                ..
            }
            | Op::Entry {
                ..
            }
            | Op::Freeze
            | Op::WStep {
                ..
            }
            | Op::Release => {}
        }
        Ok((ok, self.follow(op == Op::Flush)))
    }

    /// [`CoreUsage`] after the tip's map changed from `keys` to its entries now.
    fn follow(&mut self, flushed: bool) -> Vec<CoreUsage> {
        let now: BTreeSet<OutPointKey> = self.tip.entries().map(|(k, _)| *k).collect();
        let inserts = now.difference(&self.keys).count();
        let erases = self.keys.difference(&now).count();
        self.keys = now;
        let scripts = self
            .tip
            .entries()
            .filter_map(|(_, e)| e.coin.as_ref())
            .map(|c| coin_usage(&packed(c.as_ref())))
            .sum();
        let emplaced = std::mem::take(&mut self.emplaced_existing);
        // A flush erases every entry, then `ReallocateCache` makes a new map.
        let step = |erase_first: bool| {
            let mut u = if flushed {
                CoreUsage::new()
            } else {
                let mut u = self.usage.clone();
                if emplaced {
                    u.emplace_existing();
                }
                let erase_all = |u: &mut CoreUsage| (0..erases).for_each(|_| u.erase());
                let insert_all = |u: &mut CoreUsage| (0..inserts).for_each(|_| u.insert());
                if erase_first {
                    erase_all(&mut u);
                    insert_all(&mut u);
                } else {
                    insert_all(&mut u);
                    erase_all(&mut u);
                }
                u
            };
            u.set_cached_coins_usage(scripts);
            u
        };
        let (a, b) = (step(true), step(false));
        if a == b {
            vec![a]
        } else {
            vec![a, b]
        }
    }

    /// Core's flags and coin of its tip's entry, as the oracle's ENTRY reports them.
    fn entry(&self, k: &OutPointKey) -> Option<CacheEntry> {
        self.tip.entry(k).map(ref_entry)
    }
}

fn ref_entry(e: &RefEntry) -> CacheEntry {
    CacheEntry {
        flags: entry_flags(e.dirty, e.fresh),
        coin: e.coin.as_ref().map(|c| packed(c.as_ref())),
    }
}

/// Core's entry flags as the oracle's ENTRY reports them: DIRTY in bit 0, FRESH in bit 1.
fn entry_flags(dirty: bool, fresh: bool) -> u8 {
    u8::from(dirty) | u8::from(fresh) << 1
}

/// F in flight (hidden mode): its generation and the DIRTY entries W has not written, in F's order
/// (`None` for a spent one).
struct InFlight {
    generation: u64,
    rest: std::vec::IntoIter<(OutPointKey, Option<Vec<u8>>)>,
}

/// The node's side: CH2's cache over the in-memory engine, at an empty database.
struct Node {
    mode: Mode,
    cache: CoinsCache<MemoryEngine>,
    db: Arc<GuardedEngine<MemoryEngine>>,
    /// The keys Core's tip holds unspent and CH2's does not: each read through a block's view from
    /// the database (Core's child `FetchCoin` caches it in the tip), and not since put in CH2's tip
    /// or dropped from both.
    child_read: BTreeSet<OutPointKey>,
    in_flight: Option<InFlight>,
    /// Hidden mode: the keys added as [`Allowed::FreshOverBase`]'s premise since the last flush.
    over_base: BTreeSet<OutPointKey>,
    twin: Twin,
}

impl Node {
    fn new(mode: Mode) -> Self {
        let db = Arc::new(GuardedEngine::new(
            MemoryEngine::new(&CHAINSTATE_SCHEMA),
            GlobalFailStop::new(),
            Arc::new(RecordingSink::default()),
        ));
        // A share large enough that no boundary matters: the harness flushes only when told.
        let budget = CoinsBudget::new(64 << 20, 0, Ratio::HALF, 0);
        Self {
            mode,
            cache: CoinsCache::new(db.clone(), budget, Salt::fixed(9), None),
            db,
            child_read: BTreeSet::new(),
            in_flight: None,
            over_base: BTreeSet::new(),
            twin: Twin::new(),
        }
    }

    /// Active frozen as F at the best block.
    fn freeze(&mut self) {
        let frozen = self.cache.freeze(0);
        let rest: Vec<_> = frozen
            .coins()
            .iter_dirty()
            .map(|(k, flags, coin)| (k, (!flags.is_spent()).then(|| coin.to_vec())))
            .collect();
        self.in_flight = Some(InFlight {
            generation: frozen.generation(),
            rest: rest.into_iter(),
        });
    }

    /// W's step: F's next `n` DIRTY entries in one batch (a spent one as an erase).
    fn w_step(&mut self, n: usize) -> Result<(), Divergence> {
        let f = self.in_flight.as_mut().ok_or(divergence("script structure"))?;
        let entries: Vec<_> = f.rest.by_ref().take(n).collect();
        self.write(entries)
    }

    /// F's remaining entries written, then F released.
    fn release(&mut self) -> Result<(), Divergence> {
        let f = self.in_flight.take().ok_or(divergence("script structure"))?;
        self.write(f.rest.collect())?;
        self.cache.release(f.generation);
        Ok(())
    }

    fn write(&self, entries: Vec<(OutPointKey, Option<Vec<u8>>)>) -> Result<(), Divergence> {
        let table = CoinsTable::default();
        let mut batch = self.db.batch();
        for (k, coin) in &entries {
            match coin {
                Some(c) => table.put(&mut batch, k, c),
                None => table.erase(&mut batch, k),
            }
        }
        self.db.write(batch, Commit::Buffered).map_err(|_| divergence("node flush write"))
    }

    /// The tip flush (ALWAYS): an F in flight finished, then active frozen at the best block,
    /// written and released. Both tips are then empty.
    fn flush(&mut self) -> Result<(), Divergence> {
        if self.in_flight.is_some() {
            self.release()?;
        }
        self.freeze();
        self.release()?;
        self.child_read.clear();
        self.over_base.clear();
        Ok(())
    }

    /// The database's coins, in outpoint order.
    fn db_coins(&self) -> Result<Vec<(OutPoint, Vec<u8>)>, Divergence> {
        let cf = CoinsTable::default().cf();
        let snapshot = self.db.snapshot();
        let mut coins = Vec::new();
        for r in snapshot.scan(cf, &[]) {
            let (k, v) = r.map_err(|_| divergence("node database read"))?;
            let k = OutPointKey::from_db_key(&k).ok_or(divergence("node database key"))?;
            coins.push((
                OutPoint {
                    txid: k.txid().to_lendian(),
                    vout: k.vout(),
                },
                v,
            ));
        }
        coins.sort();
        Ok(coins)
    }

    /// [`settle`] on CH2's tip, with no block's view open.
    fn settle(
        &mut self,
        op: Op,
        core_threw: bool,
        usage: Option<(u64, u64)>,
        ended: bool,
        run: &mut CoinsOpsRun,
    ) -> Result<(), Divergence> {
        let Self {
            mode,
            cache,
            child_read,
            twin,
            ..
        } = self;
        settle(cache, twin, child_read, *mode, op, core_threw, usage, ended, run)
    }
}

/// The database after a tip flush: Core's coins and best block against the node's (CH2's durable
/// one), and no head-blocks marker.
fn compare_db(core: &DbState, node: &Node) -> Result<(), Divergence> {
    let coins = node.db_coins()?;
    let mut theirs = core.coins();
    theirs.sort();
    check(core.best_block == best_hash(node.cache.durable()), "database best block")?;
    check(core.head_blocks.is_empty(), "database head blocks")?;
    check(theirs == coins, "database coins")
}

fn coin_output(c: Option<CoinRef<'_>>) -> CoinsOutput {
    CoinsOutput::Coin(c.map(packed))
}

/// After each op: the twin runs it, and must throw where Core threw. Unless the run ended, Core's
/// usage must equal one of [`CoreUsage`]'s candidates (that one is kept) and its size the twin's;
/// in aligned mode CH2's active layer must equal the twin's tip, apart from the keys only Core's tip
/// read through a block's view.
#[allow(clippy::too_many_arguments)]
fn settle(
    cache: &CoinsCache<MemoryEngine>,
    twin: &mut Twin,
    child_read: &BTreeSet<OutPointKey>,
    mode: Mode,
    op: Op,
    core_threw: bool,
    usage: Option<(u64, u64)>,
    ended: bool,
    run: &mut CoinsOpsRun,
) -> Result<(), Divergence> {
    let (ok, candidates) = twin.apply(op)?;
    check(ok != core_threw, "core twin")?;
    let Some((dynamic_usage, cache_size)) = usage else {
        return Ok(());
    };
    run.usage_checks += 1;
    run.usage_ambiguous += usize::from(candidates.len() > 1);
    let found = candidates.into_iter().find(|u| u.dynamic_usage() == dynamic_usage);
    twin.usage = found.ok_or(divergence("dynamic usage"))?;
    check(twin.usage.cache_size() == cache_size, "cache size")?;
    if ended {
        return Ok(());
    }
    match mode {
        Mode::Aligned => compare_active(cache, twin, child_read),
        Mode::Hidden => Ok(()),
    }
}

/// CH2's active layer against Core's tip map: each entry's DIRTY/FRESH flags and coin.
fn compare_active(
    cache: &CoinsCache<MemoryEngine>,
    twin: &Twin,
    child_read: &BTreeSet<OutPointKey>,
) -> Result<(), Divergence> {
    let mut ours: Vec<_> = cache
        .active()
        .iter()
        .map(|(k, e)| {
            let flags = entry_flags(e.flags.contains(Flags::DIRTY), e.flags.contains(Flags::FRESH));
            let coin = (!e.flags.is_spent()).then(|| e.coin.to_vec());
            (
                k,
                CacheEntry {
                    flags,
                    coin,
                },
            )
        })
        .collect();
    ours.sort_by_key(|(k, _)| *k);
    let theirs: Vec<_> = twin
        .tip
        .entries()
        .filter(|(k, _)| !child_read.contains(*k))
        .map(|(k, e)| (*k, ref_entry(e)))
        .collect();
    check(ours == theirs, "active layer")
}

/// Runs `ops` on both sides in `mode` and compares each op's answer.
///
/// # Errors
///
/// The first [`Divergence`]; "oracle error" when the oracle refused the script.
pub fn compare_coins_script(mode: Mode, ops: &[Op], pin: Pin) -> Result<CoinsOpsRun, Divergence> {
    let outcomes = Oracle::new(pin)
        .coins_ops(&core_script(ops), DEFAULT_DB_BATCH_SIZE)
        .map_err(|_| divergence("oracle error"))?;
    let mut core = CoreRun {
        outcomes: outcomes.into_iter(),
    };
    let mut node = Node::new(mode);
    let mut run = CoinsOpsRun::default();
    let mut ops = ops.iter().copied();
    while let Some(op) = ops.next() {
        run.ops += 1;
        let (answer, usage) = core.next(op)?;
        let threw = matches!(answer, Core::LogicError);
        let step = if op == Op::Push {
            expect(answer, CoinsOutput::None, "push")?;
            node.settle(op, threw, usage, false, &mut run)?;
            block_view(&mut node, &mut core, &mut ops, &mut run)?
        } else {
            let step = tip_op(&mut node, op, answer, &mut run)?;
            node.settle(op, threw, usage, step.ended(), &mut run)?;
            step
        };
        if let Step::Ended(by) = step {
            run.ended_by = by;
            run.both_logic_error = by.is_none();
            break;
        }
    }
    Ok(run)
}

/// One tip op, with no block's view open.
fn tip_op(node: &mut Node, op: Op, core: Core, run: &mut CoinsOpsRun) -> Result<Step, Divergence> {
    let read = |_: CoinsReadError| divergence("node read error");
    match op {
        Op::TipAdd {
            key,
            coin: b,
            possible_overwrite,
        } => {
            let k = node_key(key);
            let c = coin(b);
            let flush_point = node.mode == Mode::Hidden
                && node.twin.tip_holds(&k)
                && !node.cache.have_in_cache(&k)
                && node.twin.released(&node.db, &k)?;
            let premise = node.mode == Mode::Hidden
                && node.twin.fresh_over_base(&k, &c, possible_overwrite, false);
            let ours = node.cache.add(&k, CoinSrc::Bytes(c.as_ref()), possible_overwrite);
            let step = add_parity(
                ours,
                core,
                node.child_read.contains(&k),
                false,
                flush_point,
                premise,
                "tip add",
            )?;
            if premise && !step.ended() {
                node.over_base.insert(k);
            }
            // An added coin is in CH2's tip as in Core's, DIRTY; an unspendable one is added by
            // neither side.
            if !c.as_ref().script_pubkey().is_unspendable() {
                node.child_read.remove(&k);
            }
            return Ok(step);
        }
        Op::TipSpend {
            key,
        } => {
            let k = node_key(key);
            // D3 (hidden mode): a spend of a coin neither side holds unspent returns whether a
            // spent entry is still cached (`coins.cpp:129-150`), which follows each side's own
            // flush history: Core keeps a DIRTY tombstone where CH2 released it, or erased a FRESH
            // coin where CH2 froze it first. Unreachable on a valid chain (inputs are checked).
            // Core's answer is still checked against the twin, which shares Core's history; only
            // CH2's is uncompared.
            let respend = node.mode == Mode::Hidden
                && !node.twin.unspent(&k)
                && node.cache.fetch(&k).map_err(read)?.is_none();
            let core_finds = node.twin.spend_finds(&k);
            let ours = node.cache.spend(&k).map_err(read)?;
            let spent = if respend {
                run.uncompared += 1;
                core_finds
            } else {
                ours
            };
            expect(
                core,
                CoinsOutput::Spend {
                    spent,
                    moved: None,
                },
                "tip spend",
            )?;
        }
        Op::TipAccess {
            key,
        } => {
            let ours = coin_output(node.cache.fetch(&node_key(key)).map_err(read)?);
            expect(core, ours, "tip access")?;
        }
        Op::TipHave {
            key,
        } => {
            let ours = node.cache.fetch(&node_key(key)).map_err(read)?.is_some();
            expect(core, CoinsOutput::Bool(ours), "tip have")?;
        }
        Op::TipHaveInCache {
            key,
        } => {
            let k = node_key(key);
            let ours = node.cache.have_in_cache(&k);
            have_in_cache(ours, core, node.mode, node.child_read.contains(&k), run)?;
        }
        Op::Entry {
            key,
        } => entry(&node.twin, key, core)?,
        Op::TipUncache {
            key,
        } => {
            node.cache.uncache(&node_key(key));
            expect(core, CoinsOutput::None, "tip uncache")?;
        }
        Op::SetBest {
            height,
        } => {
            node.cache.set_best_block(Some(block(height)));
            expect(core, CoinsOutput::None, "tip set best block")?;
        }
        Op::GetBest => {
            let ours = best_hash(node.cache.best_block());
            expect(core, CoinsOutput::Hash(ours), "tip best block")?;
        }
        Op::Flush => {
            node.flush()?;
            run.flushes += 1;
            match one(core, "tip flush")? {
                Some(CoinsOutput::Flushed {
                    ok: true,
                    db: Some(db),
                }) => compare_db(&db, node)?,
                _ => return Err(divergence("tip flush")),
            }
        }
        Op::Freeze => {
            node_only(&core, "freeze")?;
            node.over_base.retain(|k| {
                node.cache.active().get(k).is_some_and(|e| e.flags.contains(Flags::FRESH))
            });
            if !node.over_base.is_empty() {
                return Ok(Step::Ended(Some(Allowed::FreshOverBase)));
            }
            node.freeze();
            run.freezes += 1;
        }
        Op::WStep {
            n,
        } => {
            node_only(&core, "w step")?;
            node.w_step(usize::from(n))?;
            run.w_steps += 1;
        }
        Op::Release => {
            node_only(&core, "release")?;
            node.release()?;
        }
        Op::Push
        | Op::ChildAdd {
            ..
        }
        | Op::ChildSpend {
            ..
        }
        | Op::ChildAccess {
            ..
        }
        | Op::ChildHave {
            ..
        }
        | Op::Commit {
            ..
        }
        | Op::Drop => return Err(divergence("script structure")),
    }
    // A spend or read puts the key in CH2's tip as in Core's (a clean or a DIRTY spent entry alike
    // on both), and an uncache drops Core's clean entry: the tips agree on it again.
    if let Op::TipSpend {
        key,
    }
    | Op::TipAccess {
        key,
    }
    | Op::TipHave {
        key,
    }
    | Op::TipUncache {
        key,
    } = op
    {
        node.child_read.remove(&node_key(key));
    }
    Ok(Step::Continue)
}

/// A node-only scheduling op: Core ran no op for it.
fn node_only(core: &Core, field: &'static str) -> Result<(), Divergence> {
    check(matches!(core, Core::Ok(o) if o.is_empty()), field)
}

/// Core's ENTRY against the twin's tip entry, in both modes: the twin is Core's map, which the
/// aligned mode compares CH2's active layer with after every op.
fn entry(twin: &Twin, key: u8, core: Core) -> Result<(), Divergence> {
    match one(core, "entry")? {
        Some(CoinsOutput::Entry(theirs)) if theirs == twin.entry(&node_key(key)) => Ok(()),
        _ => Err(divergence("entry")),
    }
}

/// A HaveCoinInCache answer, skipped only where Core's tip holds a key read through a block's view
/// and CH2's does not, and uncompared in hidden mode.
fn have_in_cache(
    ours: bool,
    core: Core,
    mode: Mode,
    child_read: bool,
    run: &mut CoinsOpsRun,
) -> Result<(), Divergence> {
    match one(core, "tip have in cache")? {
        Some(CoinsOutput::Bool(_)) if mode == Mode::Hidden => {
            run.uncompared += 1;
            Ok(())
        }
        Some(CoinsOutput::Bool(theirs)) if theirs == ours => Ok(()),
        Some(CoinsOutput::Bool(true)) if child_read => {
            run.skipped += 1;
            Ok(())
        }
        _ => Err(divergence("tip have in cache")),
    }
}

/// An add's two results: equal, both Core's `logic_error` (the end), or different by design.
fn add_parity(
    ours: Result<(), CoinsError>,
    core: Core,
    child_read: bool,
    tip_holds_it: bool,
    flush_point: bool,
    over_base: bool,
    field: &'static str,
) -> Result<Step, Divergence> {
    let theirs = match core {
        Core::LogicError => Err(()),
        core => {
            expect(core, CoinsOutput::None, field)?;
            Ok(())
        }
    };
    match (ours, theirs) {
        (Ok(()), Ok(())) => Ok(Step::Continue),
        (Err(CoinsError::OverwriteUnspent), Err(())) => Ok(Step::Ended(None)),
        // Core's map lacks the coin its database holds; CH2's F still holds it (D26).
        (Err(CoinsError::OverwriteUnspent), Ok(())) if over_base => {
            Ok(Step::Ended(Some(Allowed::FreshOverBase)))
        }
        (Err(CoinsError::OverwriteUnspent), Ok(())) if tip_holds_it => {
            Ok(Step::Ended(Some(Allowed::FreshAtAdd)))
        }
        // Core holds the coin unspent (in the tip, or in the child that read it), CH2 does not.
        (Ok(()), Err(())) if child_read => Ok(Step::Ended(Some(Allowed::ChildReadCaching))),
        (Ok(()), Err(())) if flush_point => Ok(Step::Ended(Some(Allowed::FlushPoint))),
        _ => Err(divergence(field)),
    }
}

/// A block's view, from its push to its commit, drop or the script's end.
fn block_view(
    node: &mut Node,
    core: &mut CoreRun,
    ops: &mut impl Iterator<Item = Op>,
    run: &mut CoinsOpsRun,
) -> Result<Step, Divergence> {
    let Node {
        mode,
        cache,
        db,
        child_read,
        over_base,
        twin,
        ..
    } = node;
    let mode = *mode;
    let read = |_: CoinsReadError| divergence("node read error");
    let mut overlay = BlockOverlay::new(cache);
    // A read through the view that found an unspent coin neither the child nor CH2's tip holds:
    // it came from the database, and Core's tip now holds it too.
    let from_db = |overlay: &BlockOverlay<'_, MemoryEngine>, k: &OutPointKey, found: bool| {
        found && overlay.child().get(k).is_none() && !overlay.tip().have_in_cache(k)
    };
    for op in ops {
        run.ops += 1;
        let (answer, usage) = core.next(op)?;
        let threw = matches!(answer, Core::LogicError);
        let step = match op {
            Op::ChildAdd {
                key,
                coin: b,
                possible_overwrite,
            } => {
                let k = node_key(key);
                let tip_holds_it =
                    overlay.child().get(&k).is_none() && overlay.tip().have_in_cache(&k);
                let c = coin(b);
                let premise =
                    mode == Mode::Hidden && twin.fresh_over_base(&k, &c, possible_overwrite, true);
                let ours = overlay.add(&k, CoinSrc::Bytes(c.as_ref()), possible_overwrite);
                let step = add_parity(
                    ours,
                    answer,
                    child_read.contains(&k),
                    tip_holds_it,
                    false,
                    premise,
                    "child add",
                )?;
                if premise && !step.ended() {
                    over_base.insert(k);
                }
                step
            }
            Op::ChildSpend {
                key,
            } => {
                let k = node_key(key);
                // Without a child entry the overlay reads the coin through CH2's tip, which then
                // holds it as Core's does.
                if overlay.child().get(&k).is_none() {
                    child_read.remove(&k);
                }
                let ours = overlay.spend(&k).map_err(|e| match e {
                    CoinsError::Read(_) => divergence("node read error"),
                    _ => divergence("child spend error"),
                })?;
                // Core moves out only what it spent, and a spent entry's coin is `None`: the
                // undo gets an unspent coin or nothing, as the overlay returns it. Core's `spent`
                // is not compared: it is true for a spent entry the child holds, where the overlay
                // returns nothing either way.
                let ours = ours.map(|c| c.as_bytes().to_vec());
                match one(answer, "child spend")? {
                    Some(CoinsOutput::Spend {
                        moved,
                        ..
                    }) if moved.as_ref().and_then(Option::as_ref) == ours.as_ref() => {}
                    _ => return Err(divergence("child spend")),
                }
                Step::Continue
            }
            Op::ChildAccess {
                key,
            } => {
                let k = node_key(key);
                let found = overlay.coin(&k.outpoint()).map_err(read)?;
                if from_db(&overlay, &k, found.is_some()) {
                    child_read.insert(k);
                }
                expect(answer, coin_output(found), "child access")?;
                Step::Continue
            }
            Op::ChildHave {
                key,
            } => {
                let k = node_key(key);
                let found = overlay.have_coin(&k.outpoint()).map_err(read)?;
                if from_db(&overlay, &k, found) {
                    child_read.insert(k);
                }
                expect(answer, CoinsOutput::Bool(found), "child have")?;
                Step::Continue
            }
            Op::TipHaveInCache {
                key,
            } => {
                let k = node_key(key);
                let ours = overlay.tip().have_in_cache(&k);
                have_in_cache(ours, answer, mode, child_read.contains(&k), run)?;
                Step::Continue
            }
            Op::Entry {
                key,
            } => {
                entry(twin, key, answer)?;
                Step::Continue
            }
            Op::Commit {
                height,
            } => {
                run.commits += 1;
                // The commit puts every child key in CH2's tip as in Core's. Core's tip may hold a
                // FRESH one unspent that only a read through a block's view put there, or (hidden
                // mode) that CH2 already released; its flush then throws where the overlay's
                // commit does not.
                let dirty: Vec<_> = overlay
                    .child()
                    .iter_dirty()
                    .map(|(k, f, _)| (k, f.contains(Flags::FRESH)))
                    .collect();
                let fresh_read = dirty.iter().any(|(k, fresh)| *fresh && child_read.contains(k));
                let mut flush_point = false;
                if mode == Mode::Hidden {
                    for (k, fresh) in &dirty {
                        if *fresh
                            && twin.tip_holds(k)
                            && !overlay.tip().have_in_cache(k)
                            && twin.released(db, k)?
                        {
                            flush_point = true;
                        }
                    }
                }
                let ours = overlay.commit(block(height));
                let step = match (ours, answer) {
                    (Ok(()), Core::Ok(outputs))
                        if outputs
                            == [
                                CoinsOutput::None,
                                CoinsOutput::Flushed {
                                    ok: true,
                                    db: None,
                                },
                                CoinsOutput::None,
                            ] =>
                    {
                        for (k, _) in &dirty {
                            child_read.remove(k);
                        }
                        Step::Continue
                    }
                    (Err(CoinsError::FreshMisapplied), Core::LogicError) => Step::Ended(None),
                    (Ok(()), Core::LogicError) if fresh_read => {
                        Step::Ended(Some(Allowed::ChildReadCaching))
                    }
                    (Ok(()), Core::LogicError) if flush_point => {
                        Step::Ended(Some(Allowed::FlushPoint))
                    }
                    _ => return Err(divergence("commit")),
                };
                settle(cache, twin, child_read, mode, op, threw, usage, step.ended(), run)?;
                return Ok(step);
            }
            Op::Drop => {
                expect(answer, CoinsOutput::None, "drop")?;
                drop(overlay);
                settle(cache, twin, child_read, mode, op, threw, usage, false, run)?;
                return Ok(Step::Continue);
            }
            _ => return Err(divergence("script structure")),
        };
        let ended = step.ended();
        settle(overlay.tip(), twin, child_read, mode, op, threw, usage, ended, run)?;
        if ended {
            return Ok(step);
        }
    }
    Ok(Step::Continue)
}

/// `diff_coins_ops`' comparison of fuzz bytes: [`input_script`], then [`compare_coins_script`].
///
/// # Errors
///
/// The first [`Divergence`].
pub fn compare_coins_ops(data: &[u8], pin: Pin) -> Result<CoinsOpsRun, Divergence> {
    let (mode, ops) = input_script(data);
    compare_coins_script(mode, &ops, pin)
}

/// The `diff_coins_ops` target's comparison: a run with no divergence agrees, whichever way it
/// ended (D5's classes are counted by the campaign's report, not here); an empty script compares
/// nothing.
///
/// # Errors
///
/// The first [`Divergence`].
pub fn compare_coins_ops_target(data: &[u8], pin: Pin) -> Compared {
    let run = compare_coins_ops(data, pin)?;
    Ok(if run.ops == 0 {
        Outcome::OutOfDomain
    } else {
        Outcome::BothAccept
    })
}
