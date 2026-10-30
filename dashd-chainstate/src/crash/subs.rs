//! b3's test subscriber on [`ChainSut`](super::ChainSut) (CH2 §7.2 b3, §6.4): a real
//! [`SubscriberDriver`] over the toy index ([`ToyIndex`], every row class) on its own
//! `FaultyEngine` store, fed by the scripted producer as V steps its tip, with W's hub and its
//! pump. ST3a's toy subscriber (`dashd_store::faulty::toy::sub`) is the model it replaces.
//!
//! - **Synced** (`index=synced:b:d`): live from genesis; after each tip step the driver processes
//!   until at most `b` events are queued (backlog), and W's step 6 and step 3b pump it.
//! - **Syncing** (`index=syncing:c:d`): catch-up from a snapshot, a step per `s` op, a capped
//!   commit every `c` blocks and at each `k` op; at V's tip it hands off (F10).
//!
//! At each boot, after activation: R11a (L not ahead of G), R11b (every block the catch-up reads
//! is readable), R11c (i) after open's blind rewind and (ii) after the catch-up, which answers a
//! block with a flush.
//!
//! `D` disables it for the rest of the epoch (S11c, the toy's `D`): it stops, its lock goes, and
//! its store records the disable durably, which waives its locator's requirements and R11b. At
//! the next boot it is enabled again: the open's prune check (Core's `BaseIndex::Init`) refuses a
//! catch-up over pruned blocks, which leaves it not started (sanctioned), or it opens and deletes
//! the record.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use dash_chain::{BlockRef, FlatFilePos as IndexPos, IndexAccess, Status};
use dash_consensus::BlockId;
use dash_consensus::primitives::BlockHash;
use dashd_store::faulty::{Abort, FaultyEngine, LocatorKind as MarkKind, Mark, World};
use dashd_store::{Commit, KvBatch};

use super::world::{WorldObserver, WorldSink, hash};
use crate::flush::SubscriberHub;
use crate::planted::planted;
use crate::store::{Failure, GuardedEngine};
use crate::subscriber::{
    DriverConfig, LocatorKind, NoBlocks, PRUNE_VIOLATION, ScriptedFeeds, SubStep, SubscriberClass,
    SubscriberDriver, SubscriberError, SyncProgress, TOY_BEST, TOY_DB, TOY_KEYED, TOY_META,
    ToyIndex, decode_locator, store_err, toy_check_at_l, toy_expected, toy_rows_above,
};
use crate::tuning::{JOURNAL_BUDGET, UNBOUNDED_DELETE_SPAN};

/// The subscriber's store.
pub const SUB_STORE: &str = "subscriber";

/// Its name (`ToyIndex::name`), in its log lines.
pub(super) const SUB_NAME: &str = "toyindex";
/// The start of the subscriber's abort messages (D29).
pub(super) const SUB_ABORT_PREFIX: &str = "toyindex: ";
/// The disable's record in the subscriber's store (the toy's `K_DISABLED`), and its log line.
pub(super) const SUB_DISABLED: &[u8] = b"disabled";
pub(super) const DISABLED: &str = "toyindex disabled";

/// Q: each feed's data-event bound.
const QUEUE: usize = 4;

/// A subscriber's class at its first boot, and its limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubMode {
    /// Live from genesis, with up to `backlog` events queued after each tip step.
    Synced {
        backlog: u32,
    },
    /// Catching up, with a capped commit every `cadence` blocks.
    Syncing {
        cadence: u32,
    },
}

/// `index=` in a repro line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubConfig {
    pub mode: SubMode,
    /// The journal's block bound (§2.9 item 6) and the blind delete's span.
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

    fn driver(&self) -> DriverConfig {
        DriverConfig {
            journal_budget: JOURNAL_BUDGET,
            unbounded_delete_span: UNBOUNDED_DELETE_SPAN,
            d_max: Some(self.d_max),
            cadence: match self.mode {
                SubMode::Syncing {
                    cadence,
                } => cadence,
                SubMode::Synced {
                    ..
                } => self.d_max,
            },
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
        let mut parts = s.split(':');
        let (mode, n, d) = (parts.next(), parts.next(), parts.next());
        let num = |x: Option<&str>| x.ok_or(())?.parse::<u32>().map_err(|_| ());
        let (n, d_max) = (num(n)?, num(d)?);
        let mode = match mode {
            Some("synced") => SubMode::Synced {
                backlog: n,
            },
            Some("syncing") => SubMode::Syncing {
                cadence: n.max(1),
            },
            _ => return Err(()),
        };
        if parts.next().is_some() || d_max == 0 {
            return Err(());
        }
        Ok(Self {
            mode,
            d_max,
        })
    }
}

/// The block index's structure as the hub and the driver read it (F8): the arena's entries,
/// appended as V indexes them, under a lock V never holds while it calls them.
#[derive(Clone, Default)]
pub(crate) struct SharedIndex(Arc<RwLock<Nodes>>);

#[derive(Default)]
struct Nodes {
    nodes: Vec<(BlockHash, Option<BlockId>, u32)>,
    by_hash: HashMap<BlockHash, BlockId>,
    /// `HAVE_DATA`, which the open's prune check reads (S11c).
    data: Vec<bool>,
}

impl SharedIndex {
    fn nodes(&self) -> std::sync::RwLockReadGuard<'_, Nodes> {
        self.0.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// The arena's entry `id`, which it has just pushed.
    pub(super) fn push(
        &self,
        id: BlockId,
        hash: BlockHash,
        prev: Option<BlockId>,
        height: u32,
        data: bool,
    ) {
        let mut n = self.0.write().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(id.0 as usize, n.nodes.len(), "the arena's order");
        n.nodes.push((hash, prev, height));
        n.by_hash.insert(hash, id);
        n.data.push(data);
    }

    /// The arena's prune cleared `id`'s data.
    pub(super) fn clear_data(&self, id: BlockId) {
        self.0.write().unwrap_or_else(PoisonError::into_inner).data[id.0 as usize] = false;
    }

    fn get(&self, id: BlockId) -> Option<(BlockHash, Option<BlockId>, u32)> {
        self.nodes().nodes.get(id.0 as usize).copied()
    }

    /// The path from genesis to `tip`.
    pub(super) fn path(&self, tip: &BlockRef) -> Vec<BlockRef> {
        let mut out = Vec::new();
        let mut cur = Some(tip.id);
        while let Some(id) = cur {
            let Some((hash, prev, height)) = self.get(id) else {
                break;
            };
            out.push(BlockRef {
                id,
                hash,
                height,
            });
            cur = prev;
        }
        out.reverse();
        out
    }
}

impl IndexAccess for SharedIndex {
    fn lookup(&self, h: &BlockHash) -> Option<BlockId> {
        self.nodes().by_hash.get(h).copied()
    }

    fn hash(&self, id: BlockId) -> Option<BlockHash> {
        self.get(id).map(|n| n.0)
    }

    fn ancestor(&self, mut id: BlockId, height: u32) -> Option<BlockId> {
        while self.get(id)?.2 > height {
            id = self.get(id)?.1?;
        }
        (self.get(id)?.2 == height).then_some(id)
    }

    fn lca(&self, a: BlockId, b: BlockId) -> Option<BlockId> {
        let h = self.get(a)?.2.min(self.get(b)?.2);
        let (mut a, mut b) = (self.ancestor(a, h)?, self.ancestor(b, h)?);
        while a != b {
            (a, b) = (self.get(a)?.1?, self.get(b)?.1?);
        }
        Some(a)
    }

    fn height(&self, id: BlockId) -> Option<u32> {
        self.get(id).map(|n| n.2)
    }

    fn have_data_pos(&self, _: BlockId) -> Option<IndexPos> {
        None
    }

    fn undo_pos(&self, _: BlockId) -> Option<IndexPos> {
        None
    }

    fn status(&self, id: BlockId) -> Option<Status> {
        let data = *self.nodes().data.get(id.0 as usize)?;
        Some(Status(if data {
            Status::HAVE_DATA
        } else {
            0
        }))
    }
}

type Driver = SubscriberDriver<ToyIndex<FaultyEngine>>;

/// What a boot found, for R11a–c.
#[derive(Debug, Default)]
pub(super) struct Boot {
    /// G, the recovered chainstate tip, and the subscriber's L as open read it.
    pub g: Option<BlockRef>,
    pub l: Option<BlockRef>,
    /// An L the block index does not know (R11a).
    pub unknown: Option<BlockHash>,
    /// R11b: blocks the catch-up needs that are not readable, its rewind's undo and its
    /// connects' data.
    pub unreadable: Vec<String>,
    pub unreadable_data: Vec<String>,
    /// The store recorded a disable since the locator was written (S11c).
    pub disabled: bool,
    /// The open's prune refusal (`index/base.cpp:120-122`), which leaves the subscriber stopped.
    pub refused: Option<String>,
    /// R11c (i): the database at L after the blind rewind; (ii) the reads after the catch-up.
    pub rewound: Option<String>,
    pub caught_up: Option<String>,
}

/// The subscriber, its producer and W's hub.
pub(super) struct Subs {
    pub cfg: SubConfig,
    pub feeds: ScriptedFeeds,
    pub hub: Arc<SubscriberHub>,
    pub index: SharedIndex,
    /// The store itself, for the checks.
    pub raw: FaultyEngine,
    db: Arc<GuardedEngine<FaultyEngine>>,
    obs: Arc<WorldObserver>,
    world: World,
    driver: Mutex<Option<Driver>>,
    /// The flush whose commit the subscriber is serving (step 6), between its state marks.
    step6: Mutex<Option<u64>>,
    boot: Mutex<Boot>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Subs {
    /// The subscriber's store under the chainstate's latch (one fail-stop for the node), and the
    /// producer over V's recovered chain `active`.
    pub(super) fn new(
        world: &World,
        cfg: SubConfig,
        chain: &GuardedEngine<FaultyEngine>,
        obs: Arc<WorldObserver>,
        index: SharedIndex,
        active: Vec<BlockRef>,
    ) -> Self {
        let raw = (*world.engine(SUB_STORE, &TOY_DB)).clone();
        #[allow(unused_mut)]
        let mut db = GuardedEngine::new(
            raw.clone(),
            chain.latch().clone(),
            Arc::new(WorldSink(obs.clone())),
        );
        #[cfg(feature = "planted-bugs")]
        if let Some(bug) = chain.planted() {
            db.plant(bug);
        }
        let feeds = ScriptedFeeds::new(QUEUE, active);
        let hub = SubscriberHub::with_feeds(Arc::new(feeds.clone()), Arc::new(index.clone()));
        #[cfg(feature = "planted-bugs")]
        hub.plant(chain.planted());
        Self {
            cfg,
            feeds,
            hub: Arc::new(hub),
            index,
            raw,
            db: Arc::new(db),
            obs,
            world: world.clone(),
            driver: Mutex::new(None),
            step6: Mutex::default(),
            boot: Mutex::default(),
        }
    }

    /// Open (§2.9 item 7): the barrier, L and the state, the blind rewind, the registration; R11a
    /// and R11c (i) are judged on what it read and left.
    pub(super) fn open(&self, world: &World, g: Option<BlockRef>) -> Result<(), SubscriberError> {
        // A locator ahead of every block the index kept (R11a): the driver would refuse it (D17),
        // so it is not opened, and R11a reports it.
        let read = |k| world.checking(|| dashd_store::KvRead::get(&self.raw, TOY_META, k));
        let best = read(TOY_BEST).ok().flatten().as_deref().and_then(decode_locator);
        let disabled = read(SUB_DISABLED).ok().flatten().is_some();
        if let Some(l) = best
            && self.index.lookup(&l).is_none()
        {
            *lock(&self.boot) = Boot {
                g,
                unknown: Some(l),
                disabled,
                ..Boot::default()
            };
            return Ok(());
        }
        let opened = SubscriberDriver::open(
            ToyIndex::<FaultyEngine>::mandatory(),
            Arc::clone(&self.db),
            Arc::clone(&self.hub),
            Arc::new(NoBlocks),
            self.cfg.driver(),
        );
        let mut d = match opened {
            Err(SubscriberError::Data(m)) if m.ends_with(PRUNE_VIOLATION) => {
                self.world.mark(Mark::Log {
                    line: m.clone(),
                });
                let l = best.and_then(|hash| {
                    let id = self.index.lookup(&hash)?;
                    Some(BlockRef {
                        id,
                        hash,
                        height: self.index.height(id)?,
                    })
                });
                *lock(&self.boot) = Boot {
                    g,
                    l,
                    disabled,
                    refused: Some(m),
                    ..Boot::default()
                };
                return Ok(());
            }
            r => r?,
        };
        // Enabled again: its registration holds the lock (the toy deletes the record after the
        // catch-up; here the lock comes back at the open).
        if disabled {
            let mut b = self.db.batch();
            b.delete(TOY_META, SUB_DISABLED);
            self.db.write(b, Commit::Durable).map_err(|e| store_err(&self.db, &e))?;
        }
        let marks = world.clone();
        let mark = move |kind, at: &BlockRef| {
            marks.mark(Mark::Locator {
                store: SUB_STORE,
                tip: hash(&at.hash),
                kind: match kind {
                    LocatorKind::CommitStep | LocatorKind::EagerRewind => MarkKind::CommitStep,
                    LocatorKind::Checkpoint => MarkKind::Checkpoint,
                    LocatorKind::Cadence => MarkKind::Cadence,
                    LocatorKind::Rewind => MarkKind::Rewind,
                },
            });
        };
        let l = d.durable();
        // b4 n-G: the hook is set after `open`, so D45's open rewind (the one locator write an
        // open makes) is marked here, right after it returned.
        if let Some(at) = l
            && best != Some(at.hash)
        {
            mark(LocatorKind::Rewind, &at);
        }
        d.set_on_locator(mark);
        let rewound = world.checking(|| self.rewound(l));
        *lock(&self.boot) = Boot {
            g,
            l,
            rewound,
            disabled,
            ..Boot::default()
        };
        *lock(&self.driver) = Some(d);
        Ok(())
    }

    /// `D` (S11c): the operator disables the index for the rest of the epoch. It stops (its
    /// final capped commit, then its lock and registration go), and the store records it.
    ///
    /// The record is durable before the deregistration releases the lock (D36), so no prune can
    /// take the catch-up's data from an image without it; `after` runs once the lock is gone (W's
    /// prune, `Dp h`). LateRecord writes the record after `after`.
    pub(super) fn disable(
        &self,
        after: &mut dyn FnMut() -> Result<(), Abort>,
    ) -> Result<(), Abort> {
        let Some(mut d) = lock(&self.driver).take() else {
            // Already stopped (a refused open): `Dp`'s prune still runs.
            return after();
        };
        d.finish().map_err(|e| self.fail(&e))?;
        let late = planted!(self.db.planted(), LateRecord);
        if !late {
            self.record_disabled()?;
        }
        drop(d);
        after()?;
        if late {
            self.record_disabled()?;
        }
        Ok(())
    }

    fn record_disabled(&self) -> Result<(), Abort> {
        self.world.mark(Mark::Log {
            line: DISABLED.into(),
        });
        let mut b = self.db.batch();
        b.put(TOY_META, SUB_DISABLED, &[1]);
        self.db.write(b, Commit::Durable).map_err(|e| self.fail(&store_err(&self.db, &e)))
    }

    /// R11c (i) after the blind rewind: the database at L, and no height row above it (a row
    /// past `h(L) + D_max` escapes the rewind, as after a lost checkpoint, T38).
    fn rewound(&self, l: Option<BlockRef>) -> Option<String> {
        let l = l?;
        let above = toy_rows_above(&self.raw, l.height);
        if !above.is_empty() {
            return Some(format!(
                "height rows above L = {} after the blind rewind: {above:?}",
                l.height
            ));
        }
        toy_check_at_l(&self.raw, &self.index.path(&l)).err()
    }

    pub(super) fn boot(&self) -> MutexGuard<'_, Boot> {
        lock(&self.boot)
    }

    /// The driver's class, once open.
    pub(super) fn synced(&self) -> bool {
        lock(&self.driver).as_ref().is_some_and(|d| d.class() == SubscriberClass::Synced)
    }

    /// One step of the driver: a Synced step, or a Syncing one. True if it moved; at a limit or
    /// idle, false. `blocked` is set when it waits for a flush (rule 3, or a Syncing block).
    ///
    /// The forward assertions' `Mark::SubscriberState`: a Synced subscriber's when it first sees
    /// a flush's commit, when it parks while serving it, and when it has served it (step 6's
    /// bounds); a Syncing one's after each step.
    pub(super) fn step(&self, blocked: &mut bool) -> Result<bool, SubscriberError> {
        let mut guard = lock(&self.driver);
        let Some(d) = guard.as_mut() else {
            return Ok(false);
        };
        if d.class() == SubscriberClass::Synced {
            let commit = self.hub.outstanding(d.id()).commit.map(|c| c.flush);
            let mut step6 = lock(&self.step6);
            if commit.is_some() && *step6 != commit {
                *step6 = commit;
                self.mark(d, false);
            }
            let moved = match d.step()? {
                SubStep::Progress => true,
                SubStep::Parked => {
                    *blocked = true;
                    false
                }
                SubStep::Idle => false,
            };
            let served = self.hub.outstanding(d.id()).commit.map(|c| c.flush) != *step6;
            if step6.is_some() && (served || *blocked) {
                self.mark(d, *blocked);
                *step6 = None;
            }
            Ok(moved)
        } else {
            let moved = match d.sync_step()? {
                SyncProgress::Progress | SyncProgress::Live => true,
                SyncProgress::Blocked => {
                    *blocked = true;
                    false
                }
            };
            self.mark(d, *blocked);
            Ok(moved)
        }
    }

    /// A Synced subscriber's state after W's op: FA3 sees a flush whose commit it never served
    /// (T35).
    pub(super) fn mark_synced(&self) {
        if let Some(d) = lock(&self.driver).as_ref()
            && d.class() == SubscriberClass::Synced
            && lock(&self.step6).is_none()
        {
            self.mark(d, false);
        }
    }

    fn mark(&self, d: &Driver, blocked: bool) {
        self.world.mark(Mark::SubscriberState {
            store: SUB_STORE,
            synced: d.class() == SubscriberClass::Synced,
            durable: d.durable().map(|l| hash(&l.hash)),
            progress: d.progress().map(|p| hash(&p.hash)),
            submitted: None,
            blocked,
        });
    }

    /// A Synced subscriber after V's tip step: it processes until at most `backlog` events are
    /// queued, or it parks at its limit.
    pub(super) fn keep_up(&self) -> Result<(), SubscriberError> {
        let SubMode::Synced {
            backlog,
        } = self.cfg.mode
        else {
            return Ok(());
        };
        let Some(id) = lock(&self.driver).as_ref().map(Driver::id) else {
            return Ok(());
        };
        let mut blocked = false;
        while self.synced() && self.feeds.queued(id) > backlog as usize {
            if !self.step(&mut blocked)? {
                break;
            }
        }
        Ok(())
    }

    /// A Syncing subscriber's cadence timer (`k`).
    pub(super) fn tick(&self) -> Result<(), SubscriberError> {
        let mut guard = lock(&self.driver);
        let Some(d) = guard.as_mut() else {
            return Ok(());
        };
        d.tick()?;
        self.mark(d, false);
        Ok(())
    }

    /// R11c (ii) after the catch-up: the database at L, and the reads at p (the journal over the
    /// DB) the path's to V's tip `tip`.
    pub(super) fn check_caught_up(&self, tip: &BlockRef) {
        let guard = lock(&self.driver);
        let Some(d) = guard.as_ref() else {
            return;
        };
        let path = self.index.path(tip);
        let mut err =
            d.durable().and_then(|l| toy_check_at_l(&self.raw, &self.index.path(&l)).err());
        if err.is_none() {
            let (want, _) = toy_expected(&path);
            for (cf, k, v) in want.iter().filter(|r| r.0 == TOY_KEYED) {
                match d.read(*cf, k) {
                    Ok(got) if got.as_deref() == Some(&v[..]) => {}
                    got => {
                        err = Some(format!("the read of keyed {k:?} at the tip: {got:?}"));
                        break;
                    }
                }
            }
        }
        if err.is_none() && d.best_block() != Some(*tip) {
            err = Some(format!("best_block {:?}, V's tip {tip:?}", d.best_block()));
        }
        lock(&self.boot).caught_up = err;
    }

    /// The subscriber's failure as Core's `FatalError` (`ib`:31-34): the latch trips, so W and V
    /// stop, and the node aborts with its message, `toyindex: …` (D29). A failed store write
    /// latched in the store's guard, which reports nothing (only a failed read reports, through
    /// the sink), so the abort is marked here unless one was.
    pub(super) fn fail(&self, e: &SubscriberError) -> Abort {
        let message = match e {
            SubscriberError::Fatal(f) => format!("{SUB_NAME}: {}", f.0.message),
            e => {
                let message = format!("{SUB_NAME}: {e}");
                self.db.latch().trip(Failure {
                    kind: dashd_store::ErrorKind::Other,
                    site: SUB_NAME,
                    message: message.clone(),
                });
                message
            }
        };
        if self.obs.aborted().is_none() {
            self.obs.abort(None, &message);
        }
        Abort::new(self.obs.aborted().unwrap_or(message))
    }
}
