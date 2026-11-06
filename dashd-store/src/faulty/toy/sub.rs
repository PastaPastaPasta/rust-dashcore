//! The toy's mandatory subscriber (§6.2 "Subscriber data"; §5.5 step 5; §5.6 item 6; CH2 §2.9
//! and §4.4.1; DEC-80 Q1).
//!
//! - **Rows:** a height row per processed block (`data`); a fixed state key written only with
//!   the locator, the state at the commit's target (coinstats-shaped, CH2's `commit_state`); and
//!   a monotone archive of disconnected blocks (blockfilter-shaped: never deleted, each row a
//!   function of its block).
//! - **Classes** (Core's `m_synced`): a synced subscriber processes V's notifications from a
//!   queue lagging V by up to `backlog` events and commits at flush step 6; a syncing one
//!   advances on its own thread (`ToyOp::Sync`) and commits on its capped cadence.
//! - **Commits** at X: submit the staged data through X, `sync_wal`, then one Durable batch with
//!   the state at X and the locator. A rewind commit at a fork F below the locator blindly
//!   deletes `(h(F), h(L) + D_max]` and carries the archive rows of `(F, L]`.
//! - **Recovery:** the blind rewind of `(L, L + D_max]`, then catch-up to V's tip as a syncing
//!   subscriber, reading block data and undo; then it is synced.

use std::collections::VecDeque;
use std::fmt;
use std::str::FromStr;

use crate::faulty::toy::block::NULL;
use crate::faulty::toy::reference::{ToyRef, sha};
use crate::faulty::toy::{
    DB_READ_FAILED, STATUS_DATA, STATUS_UNDO, ToyBug, ToyChain, ToyConfig, hash_of, height_key,
};
use dash_kv::MemoryBatch;
use dash_kv::faulty::Abort;
use dash_kv::faulty::FaultyEngine;
use dash_kv::faulty::{FlushCause, Hash, LocatorKind, Mark, Trace, World};
use dash_kv::faulty::{InvariantId, Peek, Violation, hex};
use dash_kv::{Cf, Commit, KvBatch, KvEngine, KvRead};
use dash_kv::{CfProfile, CfSpec, DbSchema};

/// The subscriber database.
pub static SUB_SCHEMA: DbSchema = DbSchema {
    name: "subscriber",
    version: 1,
    cfs: &[
        CfSpec::new("meta", CfProfile::Ordered),
        CfSpec::new("data", CfProfile::Ordered),
        CfSpec::new("archive", CfProfile::PointLookup),
    ],
};
/// The locator and the state key.
pub const LOCATOR: Cf = Cf::new(0);
pub const DATA: Cf = Cf::new(1);
pub const ARCHIVE: Cf = Cf::new(2);
pub(crate) const K_LOCATOR: &[u8] = b"locator";
pub(crate) const K_SUB_STATE: &[u8] = b"state";
/// Present while the index is disabled (S11c): its locator holds no lock and so requires no
/// blocks, until a re-enabled catch-up succeeds.
pub(crate) const K_DISABLED: &[u8] = b"disabled";

/// The subscriber's store, and its name in Core's prune log lines.
pub const SUB_STORE: &str = "subscriber";
pub const SUB_NAME: &str = "toyindex";
/// Core's messages: a failed index commit, and the refusal of an index whose catch-up path was
/// pruned (`index/base.cpp:110` / `:122`).
pub(crate) const INDEX_FAILED: &str = "Failed to commit latest toyindex state";
pub(crate) const PRUNED_REFUSAL: &str = "toyindex best block of the index goes beyond pruned data. Please disable the index or reindex (which will download the whole blockchain again)";
/// Core's index sync thread and, inside it, a connect's `CustomAppend` and a `Rewind`: call
/// paths the toy marks ([`Mark::PathBegin`]).
pub(crate) const PATH_INDEX_SYNC: &str = "BaseIndex::ThreadSync";
pub(crate) const PATH_INDEX_APPEND: &str = "CoinStatsIndex::CustomAppend";
pub(crate) const PATH_INDEX_REWIND: &str = "BaseIndex::Rewind";
const SYNC_READ: (&str, &str) = ("ThreadSync: Failed to read block ", " from disk");
const SYNC_APPEND: (&str, &str) = ("ThreadSync: Failed to write block ", " to index database");
const SYNC_REWIND: &str = "ThreadSync: Failed to rewind index toyindex to a previous chain tip";
const ARCHIVED: &str = "toyindex archived ";
const DISABLED: &str = "toyindex disabled";
/// Core's "limited pruning" line, before the height.
const LIMITED: &str = "toyindex limited pruning to height ";
/// At recovery: the booted locator's commit tag (`<epoch> <commit>`), or nothing.
const BOOTED: &str = "toyindex booted ";

/// Whether `message` is Core's (v24.0.0-rc.3) for a failed read on the index path `path`: the
/// block read of a connect (`index/base.cpp:182`–`183`), its undo read in `CustomAppend`
/// (`index/coinstatsindex.cpp:129`, then `index/base.cpp:189`–`190`), or a rewind's block or
/// undo read (`index/coinstatsindex.cpp:287`, `:413`, then `index/base.cpp:172`–`173`).
pub(crate) fn sync_sanctioned(path: &str, message: &str) -> bool {
    let hashed = |(pre, post): (&str, &str)| {
        message
            .strip_prefix(pre)
            .and_then(|r| r.strip_suffix(post))
            .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
    };
    match path {
        PATH_INDEX_SYNC => hashed(SYNC_READ),
        PATH_INDEX_APPEND => hashed(SYNC_APPEND),
        PATH_INDEX_REWIND => message == SYNC_REWIND,
        _ => false,
    }
}

/// The subscriber's class at start (§6.2 "Configurations").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubMode {
    /// Notification-driven, its queue lagging V by up to `backlog` events.
    Synced {
        backlog: u32,
    },
    /// Its own catch-up thread, with a cadence commit every `cadence` blocks of progress.
    Syncing {
        cadence: u32,
    },
}

impl SubMode {
    /// A syncing subscriber's cadence (`None` when synced).
    fn cadence(self) -> Option<u32> {
        match self {
            Self::Syncing {
                cadence,
            } => Some(cadence),
            Self::Synced {
                ..
            } => None,
        }
    }
}

/// A mandatory subscriber: its class at start and `D_max` (96 in the PR gate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubConfig {
    pub mode: SubMode,
    pub d_max: u32,
}

impl SubConfig {
    pub fn synced(backlog: u32) -> Self {
        Self {
            mode: SubMode::Synced {
                backlog,
            },
            d_max: 96,
        }
    }

    pub fn syncing(cadence: u32) -> Self {
        Self {
            mode: SubMode::Syncing {
                cadence,
            },
            d_max: 96,
        }
    }
}

impl fmt::Display for SubConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.mode {
            SubMode::Synced {
                backlog,
            } => write!(f, "synced:{backlog}:{}", self.d_max),
            SubMode::Syncing {
                cadence,
            } => write!(f, "syncing:{cadence}:{}", self.d_max),
        }
    }
}

impl FromStr for SubConfig {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        let mut it = s.split(':');
        let (kind, n, d_max) = (it.next(), it.next(), it.next());
        let num = |t: Option<&str>| t.ok_or(())?.parse::<u32>().map_err(|_| ());
        let mode = match kind {
            Some("synced") => SubMode::Synced {
                backlog: num(n)?,
            },
            Some("syncing") => SubMode::Syncing {
                cadence: num(n)?,
            },
            _ => return Err(()),
        };
        Ok(Self {
            mode,
            d_max: num(d_max)?,
        })
    }
}

/// One block's contribution to the state key: an XOR accumulator, so a disconnect reverses it
/// (coinstats-shaped, as `ReverseBlock`).
pub fn sub_term(id: &Hash) -> Hash {
    sha(&[b"sub", id])
}

/// The state at a chain: the XOR of its blocks' terms.
pub fn sub_state<'a>(path: impl IntoIterator<Item = &'a Hash>) -> Hash {
    path.into_iter().fold([0u8; 32], |acc, id| xor(&acc, &sub_term(id)))
}

fn xor(a: &Hash, b: &Hash) -> Hash {
    let mut out = *a;
    out.iter_mut().zip(b).for_each(|(x, y)| *x ^= y);
    out
}

/// A disconnected block's archive row: a function of the block alone.
pub fn archive_value(id: &Hash) -> Vec<u8> {
    sha(&[b"archive", id]).to_vec()
}

/// The locator value: the epoch and per-epoch commit number that wrote it (so a check can find
/// its `Locator` mark), then the path from genesis to X, as Core's `CBlockLocator` names X's
/// ancestry.
fn encode_locator(epoch: u32, seq: u32, path: &[Hash]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + 32 * path.len());
    v.extend_from_slice(&epoch.to_le_bytes());
    v.extend_from_slice(&seq.to_le_bytes());
    path.iter().for_each(|h| v.extend_from_slice(h));
    v
}

pub(crate) fn decode_locator(v: &[u8]) -> Option<(u32, u32, Vec<Hash>)> {
    if v.len() < 8 || !(v.len() - 8).is_multiple_of(32) {
        return None;
    }
    let u = |i: usize| u32::from_le_bytes(v[i..i + 4].try_into().expect("4 bytes"));
    let path = v[8..].chunks(32).map(|c| c.try_into().expect("32 bytes")).collect();
    Some((u(0), u(4), path))
}

/// The number of leading blocks two paths share: the height of their LCA plus one.
pub(crate) fn common(a: &[Hash], b: &[Hash]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ev {
    Connected(Hash),
    Disconnected(Hash),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Done,
    /// The event does not connect to the subscriber's chain (Core's synced index ignores it).
    Dropped,
    /// A connect past `h(L) + D_max`: the subscriber waits.
    Blocked,
}

/// The subscriber's in-memory state.
pub(crate) struct Sub {
    cfg: SubConfig,
    /// Core's `m_synced`.
    pub synced: bool,
    /// It starts after recovery's activation (T28: its lock only then).
    pub started: bool,
    /// Disabled for the rest of the epoch (S11c): no lock, no processing.
    disabled: bool,
    /// The processed chain, genesis to p.
    pub chain: Vec<Hash>,
    /// The state key's value at p.
    state: Hash,
    /// `chain[..submitted]` is in returned data batches: s is `chain[submitted - 1]`, and
    /// `(s, p]` is staged in memory.
    pub submitted: usize,
    /// The latest barrier-covered locator's path (empty: none).
    pub locator: Vec<Hash>,
    /// Blocks of `(F, L]` disconnected since the last commit, for the rewind commit's archive.
    rewind_archive: Vec<Hash>,
    queue: VecDeque<Ev>,
    half_done: bool,
    /// Core's in-memory prune lock height, emulated for the moveback log line.
    emulated: Option<u32>,
    /// The subscriber made a `DMax` flush due.
    pub flush_due: bool,
    /// The height of its last cadence point: it commits every `cadence` blocks of progress.
    cadence_from: i64,
    /// Locator commits this epoch (the locator value's tag).
    commits: u32,
}

impl Sub {
    pub fn new(cfg: SubConfig) -> Self {
        Self {
            cfg,
            synced: false,
            started: false,
            disabled: false,
            chain: Vec::new(),
            state: [0; 32],
            submitted: 0,
            locator: Vec::new(),
            rewind_archive: Vec::new(),
            queue: VecDeque::new(),
            half_done: false,
            emulated: None,
            flush_due: false,
            cadence_from: 0,
            commits: 0,
        }
    }

    fn backlog(&self) -> usize {
        match self.cfg.mode {
            SubMode::Synced {
                backlog,
            } => backlog as usize,
            SubMode::Syncing {
                ..
            } => 0,
        }
    }

    /// h(L), or the start height − 1 with no locator.
    pub fn l_height(&self) -> i64 {
        self.locator.len() as i64 - 1
    }

    /// h(p), or −1 with nothing processed.
    fn p_height(&self) -> i64 {
        self.chain.len() as i64 - 1
    }

    /// The highest block it may process: `h(L) + D_max` (backpressure).
    fn limit(&self) -> i64 {
        self.l_height() + i64::from(self.cfg.d_max)
    }

    fn tip(&self) -> Option<Hash> {
        self.chain.last().copied()
    }

    /// p is a strict ancestor of L: a disconnect went below the locator.
    fn rewind_due(&self) -> bool {
        self.chain.len() < self.locator.len()
    }

    /// The state at the processed block of height `x`.
    fn state_at(&self, x: usize) -> Hash {
        self.chain[x + 1..].iter().fold(self.state, |acc, id| xor(&acc, &sub_term(id)))
    }
}

/// The subscriber's store right after the blind rewind: its data rows by height, and its state
/// key.
pub(crate) type Rewound = (Vec<(u32, Hash)>, Option<Hash>);

/// What the checks need from this epoch's recovery (§5.1 R11).
#[derive(Debug, Default, Clone)]
pub(crate) struct SubRecovery {
    /// The locator as booted, `(epoch, seq, path)`, and the chainstate's durable tip G then.
    pub locator: Option<(u32, u32, Vec<Hash>)>,
    pub g: Option<Hash>,
    /// R11c(i): the data rows and the state key right after the blind rewind.
    pub rewound: Option<Rewound>,
    /// R11b: a block on the catch-up path that could not be read.
    pub refused: Option<u32>,
    /// The index was disabled since its locator was written (S11c's sanctioned refusal).
    pub disabled: bool,
    /// Recovery ran a catch-up (not a fresh start); R11c (ii) judges its result.
    pub caught_up: bool,
}

impl ToyChain {
    fn sub_ref(&self) -> &Sub {
        self.sub.as_ref().expect("a subscriber")
    }

    fn sub_mut(&mut self) -> &mut Sub {
        self.sub.as_mut().expect("a subscriber")
    }

    fn sub_store(&self) -> &FaultyEngine {
        self.subs.as_ref().expect("a subscriber store")
    }

    fn sub_write(
        &mut self,
        b: MemoryBatch,
        commit: Commit,
        what: &'static str,
    ) -> Result<(), Abort> {
        let r = self.sub_store().write(b, commit);
        r.map_err(|_| self.abort(what, INDEX_FAILED))
    }

    fn sub_sync(&mut self) -> Result<(), Abort> {
        let r = self.sub_store().sync_wal();
        r.map_err(|_| self.abort("sync_wal", INDEX_FAILED))
    }

    fn sub_batch(&self) -> MemoryBatch {
        self.sub_store().batch()
    }

    /// `Mark::SubscriberState` (FA1 and FA3–FA6 read it).
    pub(super) fn sub_mark(&self, blocked: bool) {
        let Some(s) = self.sub.as_ref().filter(|s| !s.disabled) else {
            return;
        };
        self.world.mark(Mark::SubscriberState {
            store: SUB_STORE,
            synced: s.synced,
            durable: s.locator.last().copied(),
            progress: s.tip(),
            submitted: s.submitted.checked_sub(1).map(|i| s.chain[i]),
            blocked,
        });
    }

    // -----------------------------------------------------------------------------------------
    // V's notifications.

    pub(super) fn sub_on_connect(&mut self, id: Hash) -> Result<(), Abort> {
        let Some(s) = self.sub.as_mut() else {
            return Ok(());
        };
        if !(s.started && s.synced) {
            return Ok(());
        }
        s.queue.push_back(Ev::Connected(id));
        self.sub_pump()
    }

    /// V's `DisconnectTip` of block `id` at `height`: the moveback line, then the notification.
    pub(super) fn sub_on_disconnect(&mut self, id: Hash, height: u32) -> Result<(), Abort> {
        let prune = self.cfg.prune.is_some();
        let Some(s) = self.sub.as_mut() else {
            return Ok(());
        };
        if !s.started {
            return Ok(());
        }
        // Core moves every prune lock above h − 1 back to it (`validation.cpp:3061-3065` /
        // `:3295-3299`); ours is computed at flush time, so this is log-only.
        let back = height.checked_sub(1).filter(|b| prune && s.emulated.is_some_and(|e| e > *b));
        if let Some(b) = back {
            s.emulated = Some(b);
            self.world.mark(Mark::Log {
                line: format!("{SUB_NAME} prune lock moved back to {b}"),
            });
        }
        if !s.synced {
            return Ok(());
        }
        s.queue.push_back(Ev::Disconnected(id));
        self.sub_pump()
    }

    /// A synced subscriber processes its queue down to `backlog` events.
    fn sub_pump(&mut self) -> Result<(), Abort> {
        loop {
            let s = self.sub_ref();
            if !s.synced || !s.started || s.queue.len() <= s.backlog() {
                return Ok(());
            }
            let ev = *s.queue.front().expect("queued");
            let step = match ev {
                Ev::Connected(id) => self.sub_connect(id)?,
                Ev::Disconnected(id) => {
                    self.sub_disconnect(id)?;
                    Step::Done
                }
            };
            if step == Step::Blocked {
                return Ok(());
            }
            self.sub_mut().queue.pop_front();
        }
    }

    /// Processes `Connected(id)`: a rewind commit first if p went below L; refuses past the limit.
    fn sub_connect(&mut self, id: Hash) -> Result<Step, Abort> {
        let (parent, h) = {
            let e = &self.index[&id];
            (e.prev, e.height)
        };
        if self.sub_ref().tip().unwrap_or(NULL) != parent {
            return Ok(Step::Dropped);
        }
        self.sub_rewind()?;
        let s = self.sub_mut();
        if i64::from(h) > s.limit() {
            if s.synced {
                s.flush_due = true;
            }
            return Ok(Step::Blocked);
        }
        s.chain.push(id);
        s.state = xor(&s.state, &sub_term(&id));
        s.emulated = Some(h);
        if s.synced && i64::from(h) == s.limit() {
            s.flush_due = true;
        }
        // The half-interval data batch (§6.2 "Subscriber data").
        if !s.half_done && i64::from(h) - s.l_height() > i64::from(s.cfg.d_max / 2) {
            s.half_done = true;
            self.sub_submit(h as usize)?;
        }
        Ok(Step::Done)
    }

    /// Processes `Disconnected(id)` of its tip p. Above L the archive row is written at once
    /// (Buffered); at or below L it waits for the rewind commit.
    fn sub_disconnect(&mut self, id: Hash) -> Result<(), Abort> {
        let s = self.sub_mut();
        if s.tip() != Some(id) {
            return Ok(());
        }
        let h = s.p_height();
        s.chain.pop();
        s.state = xor(&s.state, &sub_term(&id));
        s.submitted = s.submitted.min(s.chain.len());
        if h <= s.l_height() {
            s.rewind_archive.push(id);
            return Ok(());
        }
        let mut b = self.sub_batch();
        b.put(ARCHIVE, &id, &archive_value(&id));
        self.sub_write(b, Commit::Buffered, "archive")?;
        self.archived(&id);
        Ok(())
    }

    fn archived(&self, id: &Hash) {
        self.world.mark(Mark::Log {
            line: format!("{ARCHIVED}{}", hex(id)),
        });
    }

    /// The rewind commit at F = p once p went below L (Core's `BaseIndex::Rewind`): one Durable
    /// batch that blindly deletes `(h(F), h(L) + D_max]`, writes the archive rows of `(F, L]`
    /// and the state at F, and sets the locator to F (T36: Buffered). Nothing to do otherwise.
    fn sub_rewind(&mut self) -> Result<(), Abort> {
        if !self.sub_ref().rewind_due() {
            return Ok(());
        }
        self.sub_sync()?;
        let s = self.sub_ref();
        let f = s.p_height();
        let mut b = self.sub_batch();
        for h in (f + 1)..=s.limit() {
            b.delete(DATA, &height_key(h as u32));
        }
        if !self.bug(ToyBug::ArchiveDropped) {
            for id in &s.rewind_archive {
                b.put(ARCHIVE, id, &archive_value(id));
            }
        }
        self.put_locator(&mut b, s.state, f as usize);
        let commit = if self.bug(ToyBug::T36) {
            Commit::Buffered
        } else {
            Commit::Durable
        };
        self.sub_write(b, commit, "rewind")?;
        for id in std::mem::take(&mut self.sub_mut().rewind_archive) {
            self.archived(&id);
        }
        self.sub_locator_set(f as usize, LocatorKind::Rewind);
        Ok(())
    }

    /// A locator commit's rows: the state key, then the locator at the processed block of
    /// height `x`.
    fn put_locator(&self, b: &mut MemoryBatch, state: Hash, x: usize) {
        let s = self.sub_ref();
        b.put(LOCATOR, K_SUB_STATE, &state);
        b.put(LOCATOR, K_LOCATOR, &encode_locator(self.world.epoch(), s.commits, &s.chain[..=x]));
    }

    /// After a locator commit at the processed block of height `x` returned: its mark, then
    /// L = X.
    fn sub_locator_set(&mut self, x: usize, kind: LocatorKind) {
        let tip = self.sub_ref().chain[x];
        self.world.mark(Mark::Locator {
            store: SUB_STORE,
            tip,
            kind,
        });
        let s = self.sub_mut();
        s.locator = s.chain[..=x].to_vec();
        s.half_done = false;
        s.commits += 1;
    }

    /// Submits the staged data through height `x` in one Buffered batch, and waits for it.
    fn sub_submit(&mut self, x: usize) -> Result<(), Abort> {
        let s = self.sub_ref();
        if s.submitted > x {
            return Ok(());
        }
        let mut b = self.sub_batch();
        for h in s.submitted..=x {
            b.put(DATA, &height_key(h as u32), &s.chain[h]);
        }
        self.sub_write(b, Commit::Buffered, "data")?;
        self.sub_mut().submitted = x + 1;
        Ok(())
    }

    /// A locator commit at the processed block of height `x` (§2.4.4, §5.6 item 6): submit the
    /// staged data through X, `sync_wal`, then one Durable batch with the state at X and the
    /// locator. Nothing to do when X is L or below it.
    fn sub_commit(&mut self, x: usize, kind: LocatorKind) -> Result<(), Abort> {
        let s = self.sub_ref();
        // A commit never lowers the locator (only a rewind does); at L it writes again, as
        // Core's `Commit` does.
        if x >= s.chain.len() || (x as i64) < s.l_height() {
            return Ok(());
        }
        let checkpoint = kind == LocatorKind::Checkpoint;
        // T41: the checkpoint commits over staged data it never submitted.
        if !(checkpoint && self.bug(ToyBug::T41)) {
            self.sub_submit(x)?;
        }
        // T22: no barrier before the locator.
        if !self.bug(ToyBug::T22) {
            self.sub_sync()?;
        }
        let s = self.sub_ref();
        let mut b = self.sub_batch();
        let state = if self.bug(ToyBug::StateAtProgress) {
            s.state
        } else {
            s.state_at(x)
        };
        self.put_locator(&mut b, state, x);
        // T38: a Buffered checkpoint, and the subscriber resumes past its old limit.
        let commit = if checkpoint && self.bug(ToyBug::T38) {
            Commit::Buffered
        } else {
            Commit::Durable
        };
        self.sub_write(b, commit, "locator")?;
        self.sub_locator_set(x, kind);
        Ok(())
    }

    /// A capped commit at `LCA(p, T_ack)` (T33: uncapped, at p).
    fn sub_capped(&mut self) -> Result<(), Abort> {
        self.sub_mark(false);
        let s = self.sub_ref();
        let Some(p) = s.chain.len().checked_sub(1) else {
            return Ok(());
        };
        let x = if self.bug(ToyBug::T33) {
            Some(p)
        } else {
            self.t_ack.and_then(|t| common(&s.chain, &self.path(Some(t))).checked_sub(1))
        };
        match x {
            Some(x) => self.sub_commit(x, LocatorKind::Cadence),
            None => Ok(()),
        }
    }

    /// S11c: the operator disables the index for the rest of the epoch. The toy records it
    /// durably, which waives its locator's requirements (Core holds no lock for it either).
    pub(super) fn sub_disable(&mut self) -> Result<(), Abort> {
        let Some(s) = self.sub.as_mut() else {
            return Ok(());
        };
        s.disabled = true;
        s.started = false;
        s.synced = false;
        self.world.mark(Mark::Log {
            line: DISABLED.into(),
        });
        let mut b = self.sub_batch();
        b.put(LOCATOR, K_DISABLED, &[1]);
        self.sub_write(b, Commit::Durable, "disable")
    }

    // -----------------------------------------------------------------------------------------
    // The syncing thread.

    /// `n` steps of a syncing subscriber toward V's tip: disconnect off V's chain, else connect
    /// the next block, committing on its capped cadence; at V's tip it commits and turns synced.
    /// In recovery each step reads its block's data and undo first, as catch-up does.
    pub(super) fn sub_steps(&mut self, n: u32, read: bool) -> Result<(), Abort> {
        for _ in 0..n {
            let Some(s) = self.sub.as_ref() else {
                return Ok(());
            };
            if s.synced || !s.started {
                return Ok(());
            }
            let p = s.chain.len();
            if p > 0 && self.active.get(p - 1) != s.chain.last() {
                let id = s.chain[p - 1];
                if read && !self.sub_read(&id, p as u32 - 1, true)? {
                    return Ok(());
                }
                self.sub_disconnect(id)?;
                continue;
            }
            let Some(&next) = self.active.get(p) else {
                // Core's ThreadSync at the tip: a capped commit, then `m_synced`.
                self.sub_capped()?;
                let s = self.sub_mut();
                s.synced = true;
                s.queue.clear();
                self.sub_mark(false);
                return Ok(());
            };
            if read && !self.sub_read(&next, p as u32, false)? {
                return Ok(());
            }
            match self.sub_connect(next)? {
                Step::Blocked => {
                    // At the limit: a capped commit; if it does not raise L, a flush is due.
                    let before = self.sub_ref().l_height();
                    self.sub_capped()?;
                    if self.sub_ref().l_height() == before {
                        self.sub_mut().flush_due = true;
                        return Ok(());
                    }
                }
                Step::Dropped => unreachable!("the next block connects to p"),
                Step::Done => {
                    let s = self.sub_mut();
                    let cadence = s.cfg.mode.cadence().unwrap_or(s.cfg.d_max);
                    let h = s.p_height();
                    if h - s.cadence_from >= i64::from(cadence) {
                        s.cadence_from = h;
                        self.sub_capped()?;
                    }
                }
            }
        }
        Ok(())
    }

    /// The syncing subscriber's cadence timer fires (Core's `SYNC_LOCATOR_WRITE_INTERVAL`).
    pub(super) fn sub_tick(&mut self) -> Result<(), Abort> {
        match &self.sub {
            Some(s) if s.started && !s.synced => self.sub_capped(),
            _ => Ok(()),
        }
    }

    /// A catch-up read of a block's data and undo, for a connect or a `rewind`: `false` (and the
    /// refusal) if it is missing. A failed read stops the node with ThreadSync's message for its
    /// step ([`sync_sanctioned`]).
    fn sub_read(&mut self, id: &Hash, height: u32, rewind: bool) -> Result<bool, Abort> {
        const DATA_AND_UNDO: u8 = STATUS_DATA | STATUS_UNDO;
        let hashed = |(pre, post): (&str, &str)| {
            let mut h = *id;
            h.reverse();
            format!("{pre}{}{post}", hex(&h))
        };
        self.enter(PATH_INDEX_SYNC);
        let found = match self.index.get(id).cloned() {
            Some(e) if e.status & DATA_AND_UNDO == DATA_AND_UNDO && rewind => {
                self.enter(PATH_INDEX_REWIND);
                let found = self.block_with(&e, SYNC_REWIND)?.is_some()
                    && self.undo_with(&e, SYNC_REWIND)?.is_some();
                self.leave();
                found
            }
            Some(e) if e.status & DATA_AND_UNDO == DATA_AND_UNDO => {
                self.block_with(&e, &hashed(SYNC_READ))?.is_some() && {
                    self.enter(PATH_INDEX_APPEND);
                    let undo = self.undo_with(&e, &hashed(SYNC_APPEND))?;
                    self.leave();
                    undo.is_some()
                }
            }
            _ => false,
        };
        self.leave();
        if !found {
            // Core refuses to start the index (`index/base.cpp:110` / `:122`).
            self.world.mark(Mark::Log {
                line: PRUNED_REFUSAL.into(),
            });
            self.sub_rec.refused = Some(height);
            let s = self.sub_mut();
            s.started = false;
            s.synced = false;
        }
        Ok(found)
    }

    // -----------------------------------------------------------------------------------------
    // The flush's subscriber steps.

    /// The rewind gate before a backward B1 (CH2 §4.4.1 rule 4; DEC-80 Q1): if the durable L
    /// strictly descends from `new`, a synced subscriber drains its queued disconnects and a
    /// syncing one rewinds at once, so the rewind commit lowers L first (T42 skips it).
    pub(super) fn sub_rewind_gate(&mut self, new: Hash) -> Result<(), Abort> {
        let Some(s) = self.sub.as_ref() else {
            return Ok(());
        };
        let np = self.path(Some(new));
        let above = |s: &Sub| s.locator.len() > np.len() && s.locator[np.len() - 1] == new;
        if !s.started || !above(s) || self.bug(ToyBug::T42) {
            return Ok(());
        }
        if s.synced {
            self.sub_drain_disconnects()?;
        } else {
            self.sub_disconnect_to(common(&s.chain, &np))?;
        }
        self.sub_rewind()?;
        debug_assert!(!above(self.sub_ref()), "the gate leaves no locator above new");
        Ok(())
    }

    /// Processes the queued disconnects at the queue's front.
    fn sub_drain_disconnects(&mut self) -> Result<(), Abort> {
        while let Some(Ev::Disconnected(id)) = self.sub_ref().queue.front().copied() {
            self.sub_disconnect(id)?;
            self.sub_mut().queue.pop_front();
        }
        Ok(())
    }

    /// Disconnects processed blocks until `len` remain.
    fn sub_disconnect_to(&mut self, len: usize) -> Result<(), Abort> {
        while self.sub_ref().chain.len() > len {
            let id = *self.sub_ref().chain.last().expect("a block");
            self.sub_disconnect(id)?;
        }
        Ok(())
    }

    /// Step 6 (§6.2; CH2 §4.4.1 rule 1): W waits for a synced subscriber to drain through
    /// `Connected(new)` along p → `LCA(p, new)` → `new`, checkpointing at its limit, then to
    /// commit at `LCA(p, new)`. A syncing one is not awaited; if it is blocked at its limit it
    /// commits again now that B1 has returned.
    pub(super) fn sub_commit_step(&mut self, new: Hash) -> Result<(), Abort> {
        let Some(s) = self.sub.as_ref() else {
            return Ok(());
        };
        if !s.started {
            return Ok(());
        }
        self.sub_mark(false);
        if !s.synced {
            if s.flush_due {
                self.sub_mut().flush_due = false;
                self.sub_capped()?;
            }
            return Ok(());
        }
        let pending = s.queue.iter().position(|e| *e == Ev::Connected(new));
        // T35: "has it processed `new`?" classifies a backlogged synced subscriber as syncing.
        if pending.is_some() && self.bug(ToyBug::T35) {
            return Ok(());
        }
        let np = self.path(Some(new));
        if let Some(pos) = pending {
            // Case B: the queue through `Connected(new)` nets to the path p → F → new.
            self.sub_mut().queue.drain(..=pos);
            let f = common(&self.sub_ref().chain, &np);
            self.sub_disconnect_to(f)?;
            for &id in &np[f..] {
                if i64::from(self.index[&id].height) > self.sub_ref().limit() {
                    if self.bug(ToyBug::T37) {
                        // rev3: no checkpoint; W now waits on a subscriber blocked at its limit.
                        self.sub_mark(true);
                        return Ok(());
                    }
                    let p = self.sub_ref().chain.len() - 1;
                    self.sub_commit(p, LocatorKind::Checkpoint)?;
                }
                let step = self.sub_connect(id)?;
                debug_assert_eq!(step, Step::Done);
            }
        } else {
            // Case A: queued disconnects first (a rewind below L lowers it), then commit.
            self.sub_drain_disconnects()?;
        }
        self.sub_rewind()?;
        if let Some(x) = common(&self.sub_ref().chain, &np).checked_sub(1) {
            self.sub_commit(x, LocatorKind::CommitStep)?;
        }
        self.sub_mut().flush_due = false;
        self.sub_mark(false);
        self.sub_pump()
    }

    /// The prune lock (§5.6 item 7): `height(LCA(L, new)) − BUFFER − 1` from the durable
    /// locator, or at `new` for a synced subscriber in the predicted (post-commit) lock; the
    /// start height without a locator.
    pub(super) fn sub_lock(&self, new: &Hash, predicted: bool) -> Option<i64> {
        let s = self.sub.as_ref()?;
        if s.disabled || (!s.started && self.bug(ToyBug::T28)) {
            return None;
        }
        let new_h = i64::from(self.index[new].height);
        // T35 classifies by "has it processed `new`".
        let synced = s.synced && !(self.bug(ToyBug::T35) && s.queue.contains(&Ev::Connected(*new)));
        let height = if self.bug(ToyBug::T16) {
            s.p_height()
        } else if predicted && synced && s.started {
            new_h
        } else if s.locator.is_empty() {
            return Some(0);
        } else if self.bug(ToyBug::T30) {
            s.l_height()
        } else {
            common(&s.locator, &self.path(Some(*new))) as i64 - 1
        };
        Some(height - lock_buffer(&self.cfg) - 1)
    }

    /// The prune bound a lock gives at `new`, with Core's "limited pruning" line when the lock
    /// is what limits it (`validation.cpp:2784-2798` / `:2992-3006`).
    pub(super) fn sub_bound(&self, new: &Hash, lock: Option<i64>, log: bool) -> Option<u32> {
        let lock = lock?;
        let b = lock_bound(lock, i64::from(self.index[new].height));
        if log && i64::from(b) == lock {
            self.world.mark(Mark::Log {
                line: format!("{LIMITED}{b}"),
            });
        }
        Some(b)
    }

    // -----------------------------------------------------------------------------------------
    // Recovery (§5.5 steps 0 and 5).

    /// Before recovery's first write: the WAL barrier, then the locator and state as booted
    /// (the locks load from them).
    pub(super) fn sub_recover_begin(&mut self, g: Option<Hash>) -> Result<(), Abort> {
        let Some(e) = self.subs.clone() else {
            return Ok(());
        };
        self.sub_sync()?;
        let read = |k: &[u8]| e.get(LOCATOR, k);
        let (loc, state, disabled) = match (read(K_LOCATOR), read(K_SUB_STATE), read(K_DISABLED)) {
            (Ok(l), Ok(s), Ok(d)) => {
                (l.and_then(|v| decode_locator(&v)), s.and_then(|v| hash_of(&v)), d.is_some())
            }
            _ => return Err(self.abort("read", DB_READ_FAILED)),
        };
        self.sub_rec.disabled = disabled;
        self.sub_rec.locator = loc.clone();
        self.sub_rec.g = g;
        self.world.mark(Mark::Log {
            line: match &loc {
                Some((e, n, _)) => format!("{BOOTED}{e} {n}"),
                None => BOOTED.into(),
            },
        });
        let s = self.sub_mut();
        s.locator = loc.map(|(_, _, p)| p).unwrap_or_default();
        s.chain = s.locator.clone();
        s.submitted = s.chain.len();
        s.state = state.unwrap_or([0; 32]);
        s.cadence_from = s.l_height().max(0);
        Ok(())
    }

    /// After activation: the blind rewind of `(L, L + D_max]`, the R11c(i) tap, then catch-up
    /// to V's tip as a syncing subscriber. A fresh store starts in its configured class.
    pub(super) fn sub_recover(&mut self) -> Result<(), Abort> {
        let Some(e) = self.subs.clone() else {
            return Ok(());
        };
        let s = self.sub_ref();
        let mut b = self.sub_batch();
        for h in (s.l_height() + 1)..=s.limit() {
            b.delete(DATA, &height_key(h as u32));
        }
        self.sub_write(b, Commit::Buffered, "rewind")?;
        // R11c(i) reads the store here: rows at or below L are barrier-covered, and the delete
        // shadows every row above, so nothing pending is decided.
        let rows: dash_kv::Result<Vec<_>> = e.iter_prefix(DATA, b"").collect();
        let state = e.get(LOCATOR, K_SUB_STATE);
        let (Ok(rows), Ok(state)) = (rows, state) else {
            return Err(self.abort("read", DB_READ_FAILED));
        };
        let rows = rows.iter().filter_map(|(k, v)| data_row(k, v)).collect();
        self.sub_rec.rewound = Some((rows, state.and_then(|v| hash_of(&v))));
        let fresh = self.active.is_empty() && self.sub_ref().locator.is_empty();
        let s = self.sub_mut();
        s.started = true;
        s.synced = fresh && matches!(s.cfg.mode, SubMode::Synced { .. });
        if fresh {
            return Ok(());
        }
        // Catch-up; at its limit it makes a flush due and commits again once that flush's B1
        // has returned (§5.6 item 6), then continues.
        self.sub_rec.caught_up = true;
        loop {
            self.sub_steps(u32::MAX, true)?;
            let s = self.sub_ref();
            if s.synced || !s.started || !s.flush_due {
                break;
            }
            self.flush_at(0, 0, Some(FlushCause::DMax))?;
            if self.sub_ref().flush_due {
                break;
            }
        }
        self.sub_mark(false);
        // A re-enabled index that caught up holds its lock again.
        if self.sub_rec.disabled && self.sub_rec.refused.is_none() {
            let mut b = self.sub_batch();
            b.delete(LOCATOR, K_DISABLED);
            self.sub_write(b, Commit::Durable, "enable")?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // Checks.

    /// R11a–c and DEC-80's archive rule, after recovery.
    pub(super) fn sub_check(&self, world: &World, refm: &ToyRef, trace: &Trace) -> Vec<Violation> {
        let mut v = Vec::new();
        let Some(s) = &self.sub else {
            return v;
        };
        let rec = &self.sub_rec;
        let lpath: &[Hash] = rec.locator.as_ref().map_or(&[], |(_, _, p)| p);
        // R11a: the booted locator is not ahead of the booted G.
        if let Some(l) = lpath.last() {
            let ahead = match rec.g {
                None => true,
                Some(g) => g != *l && refm.is_ancestor(&g, l),
            };
            if ahead {
                v.push(Violation::new(
                    InvariantId::R11a,
                    format!(
                        "the subscriber's locator {:?} is ahead of the chainstate's durable tip {:?}",
                        refm.height(l),
                        rec.g.and_then(|g| refm.height(&g))
                    ),
                ));
            }
        }
        // R11b: every block on its catch-up path read back, for a subscriber enabled in every
        // epoch since its locator was written; one disabled since must refuse instead (S11c).
        if let Some(h) = rec.refused
            && !rec.disabled
        {
            v.push(Violation::new(
                InvariantId::R11b,
                format!("block {h} on the subscriber's catch-up path is not readable"),
            ));
            v.push(Violation::new(
                InvariantId::R0,
                "the subscriber refused to start: its catch-up path is unreadable",
            ));
        }
        // R11c(i): after the blind rewind the store equals the reference at L.
        if let Some((rows, state)) = &rec.rewound {
            let want = rows_of(lpath);
            if *rows != want {
                let diff = rows
                    .iter()
                    .find(|r| !want.contains(r))
                    .or_else(|| want.iter().find(|r| !rows.contains(r)))
                    .map(|r| r.0);
                v.push(Violation::new(
                    InvariantId::R11c,
                    format!(
                        "after the blind rewind the data differs from the reference at L = {:?} (height {diff:?})",
                        lpath.len().checked_sub(1)
                    ),
                ));
            }
            let want_state = (!lpath.is_empty()).then(|| sub_state(lpath));
            if *state != want_state {
                v.push(Violation::new(
                    InvariantId::R11c,
                    "after the blind rewind the state key differs from the reference at L",
                ));
            }
        }
        // R11c(ii): catch-up reached V's tip, and the data (stored and staged) and the state key
        // equal the reference's there.
        if rec.refused.is_none() && rec.caught_up && s.started {
            if s.chain != self.active {
                v.push(Violation::new(
                    InvariantId::R11c,
                    format!(
                        "catch-up stopped at {:?}, V's tip is at {:?}",
                        s.chain.len().checked_sub(1),
                        self.active.len().checked_sub(1)
                    ),
                ));
            }
            let state =
                self.sub_store().get(LOCATOR, K_SUB_STATE).ok().flatten().and_then(|x| hash_of(&x));
            if !s.locator.is_empty() && state != Some(sub_state(&s.locator)) {
                v.push(Violation::new(
                    InvariantId::R11c,
                    "after catch-up the state key differs from the reference at the locator",
                ));
            }
            let mut rows: Vec<(u32, Hash)> = self
                .sub_store()
                .iter_prefix(DATA, b"")
                .filter_map(|r| r.ok())
                .filter_map(|(k, x)| data_row(&k, &x))
                .filter(|(h, _)| (*h as usize) < s.submitted)
                .collect();
            rows.extend_from_slice(&rows_of(&s.chain)[s.submitted..]);
            let want = match self.active.last() {
                Some(t) if refm.blocks.get(t).is_some() => rows_of(&refm.blocks.path(t)),
                _ => Vec::new(),
            };
            if rows != want {
                v.push(Violation::new(
                    InvariantId::R11c,
                    "after catch-up the data differs from the reference at the subscriber's tip",
                ));
            }
        }
        // DEC-80 Q1 (a): the archive is a monotone set. Present rows are correct, and the rows of
        // blocks disconnected before the booted locator's commit remain.
        let view = world.durable_view();
        for id in refm.blocks.blocks.keys() {
            if let Peek::Known(Some(x)) = view.get(SUB_STORE, ARCHIVE, id)
                && x != archive_value(id)
            {
                v.push(Violation::new(InvariantId::R11c, "an archive row has a wrong value"));
            }
        }
        if let Some((epoch, seq, _)) = &rec.locator {
            for id in required_archive(trace, *epoch, *seq, world.epoch()) {
                match view.get(SUB_STORE, ARCHIVE, &id) {
                    Peek::Known(Some(x)) if x == archive_value(&id) => {}
                    _ => v.push(Violation::new(
                        InvariantId::R11c,
                        format!(
                            "the archive row of block {:?}, disconnected before the locator's commit, is lost",
                            refm.height(&id)
                        ),
                    )),
                }
            }
        }
        v
    }
}

/// A chain's data rows: each block at its height.
fn rows_of(path: &[Hash]) -> Vec<(u32, Hash)> {
    path.iter().enumerate().map(|(h, id)| (h as u32, *id)).collect()
}

/// A stored data row: its height key and its block.
fn data_row(k: &[u8], v: &[u8]) -> Option<(u32, Hash)> {
    Some((u32::from_be_bytes(k.try_into().ok()?), hash_of(v)?))
}

/// The subscribers' lock buffer (`PRUNE_LOCK_BUFFER`): a lock is `BUFFER + 1` below its height.
fn lock_buffer(cfg: &ToyConfig) -> i64 {
    i64::from(cfg.prune.as_ref().map_or(0, |p| p.buffer))
}

/// Core's `last_prune = max(1, min(chain height, lock))`.
fn lock_bound(lock: i64, new_h: i64) -> u32 {
    lock.min(new_h).max(1) as u32
}

/// The actual lock bound a flush at `new` must use when the subscriber's durable locator is
/// `durable` and it has not processed `new` (R5 (c) for recovery's flushes): `None` with no
/// subscriber.
pub(super) fn entitled_lock(
    cfg: &ToyConfig,
    refm: &ToyRef,
    durable: Option<Hash>,
    new: &Hash,
) -> Option<u32> {
    cfg.sub?;
    let new_h = i64::from(refm.height(new)?);
    let lock = durable.map_or(0, |d| {
        common(&refm.blocks.path(&d), &refm.blocks.path(new)) as i64 - 1 - lock_buffer(cfg) - 1
    });
    Some(lock_bound(lock, new_h))
}

/// The archive rows a locator committed as the `seq`-th of `epoch` requires: the blocks
/// archived in that epoch before it (all of the epoch's if its mark never came: it was in flight
/// at the crash), and, since the archive is monotone, every row the locator that epoch booted
/// from required, back through the epochs before `now`.
fn required_archive(trace: &Trace, epoch: u32, seq: u32, now: u32) -> Vec<Hash> {
    let mut out = Vec::new();
    let mut commits = 0;
    let mut booted: Option<(u32, u32)> = None;
    let mut done = false;
    for (e, m) in trace.marks_with_epoch() {
        if e != epoch || done {
            continue;
        }
        match m {
            Mark::Locator {
                store: SUB_STORE,
                ..
            } => {
                if commits == seq {
                    done = true;
                }
                commits += 1;
            }
            Mark::Log {
                line,
            } => {
                if let Some(h) = line.strip_prefix(ARCHIVED)
                    && let Some(id) = unhex(h)
                {
                    out.push(id);
                }
                if let Some(t) = line.strip_prefix(BOOTED) {
                    booted = t
                        .split_once(' ')
                        .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)));
                }
            }
            _ => {}
        }
    }
    if let Some((e0, s0)) = booted
        && e0 < epoch
        && epoch <= now
    {
        out.extend(required_archive(trace, e0, s0, now));
    }
    out
}

fn unhex(s: &str) -> Option<Hash> {
    let b: Option<Vec<u8>> =
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect();
    b?.try_into().ok()
}

// ---------------------------------------------------------------------------------------------
// Forward assertions (§6.2.1): FA1's lock and log, FA3–FA6.

/// The forward run's subscriber assertions.
pub(super) fn forward_subscriber(cfg: &ToyConfig, refm: &ToyRef, trace: &Trace) -> Vec<Violation> {
    let mut v = Vec::new();
    let Some(sc) = cfg.sub else {
        return v;
    };
    let buffer = lock_buffer(cfg);
    let h = |id: &Hash| i64::from(refm.height(id).unwrap_or(0));
    let lca = |a: &Hash, b: &Hash| -> i64 {
        common(&refm.blocks.path(a), &refm.blocks.path(b)) as i64 - 1
    };
    /// A subscriber state mark's (synced, durable, progress).
    type SubState = (bool, Option<Hash>, Option<Hash>);
    // The latest subscriber state.
    let mut state: Option<SubState> = None;
    let mut limited: Option<u32> = None;
    let mut flush_new: Option<(u64, Hash)> = None;
    let mut t_ack: Option<Hash> = None;
    // The latest decision: the subscriber was synced, and every eligible file (no lock).
    let mut decided: Option<(bool, Vec<u32>)> = None;
    let cadence = sc.mode.cadence().map(i64::from);
    // FA4: the progress height of the last cadence commit (or of the durable locator).
    let mut cadence_from: Option<i64> = None;
    let mut cadence_seen = false;
    // Step 6 of the current flush: new, the state at its start, its commit, checkpoints. It
    // ends at the subscriber's next state mark (the end of `sub_commit_step`).
    struct Step6 {
        new: Hash,
        start: Option<SubState>,
        committed: Option<Hash>,
        checkpoints: u32,
        blocked: bool,
    }
    let mut step6: Option<Step6> = None;
    let close = |s6: Option<Step6>, v: &mut Vec<Violation>| {
        let Some(s6) = s6 else {
            return;
        };
        let Some((synced, durable, p)) = s6.start else {
            return;
        };
        if !synced || s6.blocked {
            return;
        }
        let after = s6.committed.or(durable);
        // FA3: a synced subscriber commits at LCA(p, new). That is `new` when p is on new's
        // chain; from another branch it either drained to `new` or had processed past it.
        let on_chain =
            p.is_none_or(|p| refm.is_ancestor(&p, &s6.new) || refm.is_ancestor(&s6.new, &p));
        let at_lca =
            |a: &Hash| p.is_some_and(|p| h(a) == lca(&p, &s6.new)) && refm.is_ancestor(a, &s6.new);
        let ok = after == Some(s6.new) || (!on_chain && after.as_ref().is_some_and(at_lca));
        if !ok {
            v.push(Violation::new(
                InvariantId::FA3,
                format!(
                    "a synced subscriber did not commit at LCA(p, new) = {} in step 6 (its locator is at {:?})",
                    h(&s6.new),
                    after.map(|a| h(&a))
                ),
            ));
        }
        // FA6: the drain needs at most ⌈(h(new) − h(F))/D_max⌉ checkpoints.
        let f = p.map_or(-1, |p| lca(&p, &s6.new));
        let max = ((h(&s6.new) - f).max(0) as u64).div_ceil(u64::from(sc.d_max));
        if u64::from(s6.checkpoints) > max {
            v.push(Violation::new(
                InvariantId::FA6,
                "the drain made more checkpoints than ⌈(h(new) − h(F))/D_max⌉",
            ));
        }
    };
    for m in trace.marks() {
        match m {
            // A fail-stopped run owes no further progress (R8 judges it).
            Mark::Abort {
                ..
            } => return v,
            Mark::SubscriberState {
                synced,
                durable,
                progress,
                blocked,
                ..
            } => {
                if let Some(s6) = step6.as_mut() {
                    if *blocked {
                        s6.blocked = true;
                        v.push(Violation::new(
                            InvariantId::FA6,
                            "W waits in step 6 on a subscriber blocked at its limit",
                        ));
                    }
                    if s6.start.is_none() {
                        s6.start = Some((*synced, *durable, *progress));
                    } else {
                        close(step6.take(), &mut v);
                    }
                }
                if let Some(c) = cadence {
                    let was_synced = state.is_some_and(|(s, _, _)| s);
                    if !*synced {
                        // FA4: at most `cadence` blocks of progress without a cadence commit,
                        // counted from where a capped commit could last land: none can before
                        // `T_ack`, nor while LCA(progress, T_ack) is below the locator (I34).
                        let from = cadence_from.get_or_insert(durable.map_or(0, |d| h(&d)));
                        let floor = durable.map_or(-1, |d| h(&d));
                        match progress {
                            Some(p) if t_ack.is_none_or(|t| lca(p, &t) < floor) => *from = h(p),
                            Some(p) if h(p) - *from > c => v.push(Violation::new(
                                InvariantId::FA4,
                                format!(
                                    "a syncing subscriber reached {} with no cadence commit since {from}",
                                    h(p)
                                ),
                            )),
                            _ => {}
                        }
                    } else if !was_synced && state.is_some() && !cadence_seen && t_ack.is_some() {
                        // FA4: it turns synced with a capped commit.
                        v.push(Violation::new(
                            InvariantId::FA4,
                            "a syncing subscriber turned synced without a capped commit",
                        ));
                    }
                    cadence_seen = false;
                }
                state = Some((*synced, *durable, *progress));
            }
            // A disabled index holds no lock (S11c).
            Mark::Log {
                line,
            } if line == DISABLED => state = None,
            Mark::Log {
                line,
            } => {
                if let Some(x) = line.strip_prefix(LIMITED) {
                    limited = x.parse().ok();
                }
            }
            Mark::PruneDecision {
                new,
                predicted_lock,
                dry_run,
                unlocked,
                ..
            } => {
                if let Some((synced, durable, _)) = state {
                    // FA1: the dry run's lock is the predicted post-commit lock.
                    let lock = if synced {
                        h(new) - buffer - 1
                    } else {
                        durable.map_or(0, |d| lca(&d, new) - buffer - 1)
                    };
                    let bound = lock_bound(lock, h(new));
                    if *predicted_lock != Some(bound) {
                        v.push(Violation::new(
                            InvariantId::FA1,
                            format!(
                                "the dry run's lock bound is {predicted_lock:?}, the predicted post-commit lock gives {bound}"
                            ),
                        ));
                    }
                    // FA2: the "limited pruning" line names that height.
                    let line = (i64::from(bound) == lock).then_some(bound);
                    if limited != line {
                        v.push(Violation::new(
                            InvariantId::FA2,
                            format!("the limited-pruning line reads {limited:?}, the predicted lock gives {line:?}"),
                        ));
                    }
                    // FA5: a synced subscriber never holds pruning below `new − K`.
                    if synced && dry_run != unlocked {
                        v.push(Violation::new(
                            InvariantId::FA5,
                            format!(
                                "pruning froze: D = {dry_run:?}, every eligible file = {unlocked:?}"
                            ),
                        ));
                    }
                }
                decided = state.map(|(synced, _, _)| (synced, unlocked.clone()));
                limited = None;
            }
            Mark::FlushBegin {
                flush,
                new,
                ..
            } => {
                close(step6.take(), &mut v);
                flush_new = Some((*flush, *new));
            }
            Mark::Durable {
                flush,
            } => {
                if let Some((f, new)) = flush_new
                    && f == *flush
                {
                    t_ack = Some(new);
                    step6 = Some(Step6 {
                        new,
                        start: None,
                        committed: None,
                        checkpoints: 0,
                        blocked: false,
                    });
                }
            }
            Mark::Locator {
                tip,
                kind,
                ..
            } => {
                if let Some(s6) = step6.as_mut()
                    && matches!(kind, LocatorKind::CommitStep | LocatorKind::Checkpoint)
                {
                    s6.committed = Some(*tip);
                    if *kind == LocatorKind::Checkpoint {
                        s6.checkpoints += 1;
                    }
                }
                if *kind == LocatorKind::Cadence {
                    cadence_seen = true;
                    cadence_from = state.and_then(|(_, _, p)| p).map(|p| h(&p));
                    // FA4: a cadence commit is capped at LCA(progress, T_ack).
                    if let Some((_, _, Some(p))) = state {
                        let cap = t_ack.map(|t| lca(&p, &t));
                        if cap != Some(h(tip)) || !refm.is_ancestor(tip, &p) {
                            v.push(Violation::new(
                                InvariantId::FA4,
                                format!(
                                    "a cadence commit at {} is not capped at LCA(progress {}, T_ack) = {cap:?}",
                                    h(tip),
                                    h(&p)
                                ),
                            ));
                        }
                    }
                }
            }
            Mark::PruneSelected {
                files,
                ..
            } => {
                close(step6.take(), &mut v);
                // FA5: with a synced subscriber the flush selects every eligible file (the
                // toy's chain is stable during a flush).
                if let Some((true, unlocked)) = decided.take()
                    && *files != unlocked
                {
                    v.push(Violation::new(
                        InvariantId::FA5,
                        format!(
                            "the flush selected {files:?}, every eligible file is {unlocked:?}"
                        ),
                    ));
                }
            }
            _ => {}
        }
    }
    close(step6, &mut v);
    v
}
