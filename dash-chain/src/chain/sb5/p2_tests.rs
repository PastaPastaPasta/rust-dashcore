//! SB5 §10.3 P2, windowed ≡ unwindowed (SB4 §15.2, SB0 §4.4): random header trees over regtest
//! (stale branches forking below the window, locks and the conflict marks their enforcement
//! leaves, reorgs deeper than w, restarts at committed cuts) run through SB4 §9.1's recipe on
//! an unwindowed chain and on SPV's windowed one (stage 4 in sub-steps of 4 events against
//! one), and every answer is compared by hash after every step. The unwindowed chain is SPV's own at a window no tree reaches, so it never
//! evicts: `NoStore` cannot run stage 3, and `Permanent`'s resident walks assume no floor below
//! an anchor. A restart (`sync_wal`, then `open`) reopens both: a reopened chain recomputes its
//! best header over the stored leaves, as Core's load does, so it can differ from one that
//! never stopped; reopen against the pre-restart chain is P4's, a lost Buffered suffix P4's and
//! P5's.

use std::sync::Arc;

use dash_consensus::primitives::Header;
use dash_params::{ChainParams, NetworkKind};
use dash_primitives::BlockHash;
use proptest::prelude::*;

use super::step_tests::meta;
use crate::chain::spv_tests::{ANCHOR, Spv, mine_n, regtest_pack};
use crate::chainlock::ChainLockRef;
use crate::config::{ChainConfig, Services, StaticSporks};
use crate::feed::{ChainSnapshot, FeedSnapshot};
use crate::mem_store::MemHeaderStore;
use crate::step::Durability;
use crate::testing::{batch, params, services};
use crate::{AnchorPack, BlockId};

/// The unwindowed chain's window: no tree reaches it.
pub(super) const UNWINDOWED: u32 = 1_000_000;
/// Snapshots each chain retains, oldest dropped first.
const RETAINED: usize = 6;

/// Where an `Extend` mines from.
#[derive(Clone, Copy, Debug)]
enum Parent {
    /// The active tip.
    Tip,
    /// The `pick`-th branch end, so that a branch forking deep keeps growing until it wins.
    End(usize),
    /// The `pick`-th known header: a new fork, below the window too.
    Any(usize),
}

#[derive(Clone, Debug)]
enum Op {
    /// `n` headers mined on `from` (accepted before the step's `VirtualTip`).
    Extend {
        from: Parent,
        n: u32,
    },
    /// A lock on the `pick`-th known header at its own height.
    Lock {
        pick: usize,
    },
    /// SPORK_19 set, then `check_cl_active_state`.
    Spork(bool),
}

fn parent() -> impl Strategy<Value = Parent> {
    prop_oneof![
        2 => Just(Parent::Tip),
        3 => any::<usize>().prop_map(Parent::End),
        1 => any::<usize>().prop_map(Parent::Any),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        5 => (parent(), 1..=16u32).prop_map(|(from, n)| Op::Extend { from, n }),
        2 => any::<usize>().prop_map(|pick| Op::Lock { pick }),
        1 => any::<bool>().prop_map(Op::Spork),
    ]
}

/// One step: a restart of both chains first, then the ops.
fn step() -> impl Strategy<Value = (bool, Vec<Op>)> {
    (prop::bool::weighted(0.15), prop::collection::vec(op(), 1..5))
}

/// One chain, its store, its spork and the snapshots it published.
pub(super) struct Side {
    pub(super) chain: Spv,
    pub(super) store: MemHeaderStore,
    sporks: Arc<StaticSporks>,
    svc: Services,
    cfg: ChainConfig,
    retained: Vec<FeedSnapshot>,
}

impl Side {
    /// The pack, committed by a first step (as `step_tests::committed_w`), at window `w` with
    /// sub-steps of `e` events.
    pub(super) fn new(p: &Arc<ChainParams>, pack: &AnchorPack, w: u32, e: u32) -> Self {
        let sporks = Arc::new(StaticSporks::for_network(p, Some(true)));
        let svc = Services {
            sporks: sporks.clone(),
            ..services(p)
        };
        let mut cfg = ChainConfig::spv(p, w);
        cfg.feeds.segment_events = e;
        let store = MemHeaderStore::new();
        let chain = Spv::new_anchored(p.clone(), cfg.clone(), svc.clone(), store.clone(), pack)
            .expect("a valid pack");
        store.reset(meta(pack.hash, pack.height)).expect("reset");
        let mut side = Side {
            chain,
            store,
            sporks,
            svc,
            cfg,
            retained: Vec::new(),
        };
        side.step(&[], &[]);
        side
    }

    /// SB4 §9.1's recipe as SPV runs it, with no full plan (DEC-187): the step's batches, one
    /// `VirtualTip` for its calls, `fork_target` against it, stage 3 committed Buffered with the
    /// target's `BlockRef` when the tip moves (it reads the move's shape, `tip_target`), stage 4
    /// in sub-steps that each plan their own (`activate_segmented`), stage 5. Returns each
    /// outcome, for the other side to match; retains the published snapshot.
    pub(super) fn step(&mut self, batches: &[Vec<Header>], calls: &[Call]) -> Vec<String> {
        let c = &mut self.chain;
        let mut out = Vec::new();
        for hs in batches {
            let r = c.accept(batch(hs), None).expect("accept");
            out.push(format!("{:?}", (r.last_accepted, r.stop, r.floor_moved, r.inserted)));
        }
        let mut vt = c.virtual_tip();
        for call in calls {
            out.push(match *call {
                Call::Lock(cl) => format!("{:?}", c.apply_chainlock(cl, &mut vt).expect("applies")),
                Call::Spork(on) => {
                    self.sporks.set(on);
                    format!("{:?}", c.check_cl_active_state())
                }
            });
        }
        let target = c.fork_target(&vt).expect("a target");
        let to = c.block_ref_of(target.id).expect("held");
        let moved = c.private_tip().expect("a tip").0.id != target.id;
        self.store.begin_step();
        let tail = c.write_header_step(moved.then_some(to), &mut self.store).expect("stage 3");
        self.store.commit_step(&tail, Durability::Buffered).expect("the commit");
        c.activate_segmented(to, &mut || {}).expect("stage 4");
        c.records_committed();
        out.push(format!("{target:?}"));
        if self.retained.len() == RETAINED {
            self.retained.remove(0);
        }
        self.retained.push(c.feeds().feed_snapshot());
        out
    }

    fn hash(&self, id: BlockId) -> BlockHash {
        self.chain.reader().try_entry(id).expect("read").expect("held").hash
    }

    pub(super) fn id(&self, hash: &BlockHash) -> Option<BlockId> {
        self.chain.reader().try_lookup(hash).expect("read")
    }

    /// Every compared answer, ids mapped to hashes: tip, best header, fork target, chain tips;
    /// for each known header its entry, its ancestors at three heights and its last common
    /// ancestor with the tip.
    fn answers(&self, known: &[(Header, u32)]) -> Vec<String> {
        let c = &self.chain;
        let r = c.reader();
        let hash = |id| self.hash(id);
        let floor = c.floor;
        let tip = c.private_tip().expect("a tip").0;
        let best = c.best_header();
        let target = c.fork_target(&c.virtual_tip()).expect("a target");
        let tips: Vec<_> = c
            .chain_tips()
            .expect("chain tips")
            .into_iter()
            .map(|t| (t.hash, t.height, t.chain_work, t.branchlen, hash(t.fork_point), t.status))
            .collect();
        let mut out = vec![
            format!("tip {:?}", (tip.hash, tip.height)),
            format!("best {:?}", hash(best)),
            format!("target {:?}", (hash(target.id), target.floor)),
            format!("tips {tips:?}"),
        ];
        for (h, height) in known {
            let Some(id) = self.id(&h.hash()) else {
                out.push(format!("{:?} unknown", h.hash()));
                continue;
            };
            let e = r.try_entry(id).expect("read").expect("held");
            let up = |x: Option<BlockId>| x.map(hash);
            let at = |k| up(r.try_ancestor(id, k).expect("read"));
            out.push(format!(
                "{:?}",
                (
                    e.hash,
                    e.height,
                    e.status,
                    e.chain_work,
                    at(floor),
                    at(floor + (height - floor) / 2),
                    at(height.saturating_sub(1)),
                    up(r.try_lca(id, tip.id).expect("read")),
                )
            ));
        }
        out
    }

    /// The tip's and the best header's locators.
    fn locators(&self) -> [Vec<BlockHash>; 2] {
        let r = self.chain.reader();
        let tip = self.chain.private_tip().expect("a tip").0.id;
        [r.locator(Some(tip)), r.locator(Some(self.chain.best_header()))]
    }

    /// Each retained snapshot, by [`describe`].
    fn snapshots(&self, stride: u32) -> Vec<String> {
        self.retained.iter().map(|s| describe(s, self.chain.floor, stride)).collect()
    }

    /// A restart at the last committed step: made durable, then reopened.
    pub(super) fn restart(&mut self, p: &Arc<ChainParams>) {
        self.store.sync_wal().expect("sync");
        self.chain = Spv::open(p.clone(), self.cfg.clone(), self.svc.clone(), self.store.clone())
            .expect("open");
    }
}

/// A snapshot's tip, `tip_seq`, its `at_height` every `stride` heights from `floor` and at its
/// tip and one above, and its whole range.
pub(super) fn describe(s: &FeedSnapshot, floor: u32, stride: u32) -> String {
    let tip = s.tip();
    let hs: Vec<_> = (floor..=tip.height + 1)
        .filter(|h| (h - floor).is_multiple_of(stride) || *h >= tip.height)
        .map(|h| s.at_height_checked(h).expect("read").map(|b| (b.hash, b.height)))
        .collect();
    let range: Vec<_> = s.range(floor, tip.height).expect("read").iter().map(|b| b.hash).collect();
    format!("{:?}", ((tip.hash, tip.height), s.tip_seq(), hs, range))
}

/// A step's calls after its batches, in order.
#[derive(Clone, Copy, Debug)]
pub(super) enum Call {
    Lock(ChainLockRef),
    Spork(bool),
}

/// Runs `steps` at window `w` on both chains, comparing after each.
fn run(w: u32, steps: Vec<(bool, Vec<Op>)>) {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    // The windowed chain moves its tip in sub-steps of 4 events, the other in one.
    let mut a = Side::new(&p, &pack, UNWINDOWED, 1_024);
    let mut b = Side::new(&p, &pack, w, 4);
    // The pack's headers, `ANCHOR − 24` up.
    let mut known: Vec<(Header, u32)> =
        pack.headers.iter().zip(ANCHOR - 24..).map(|(h, k)| (Header::new(*h), k)).collect();
    // Each branch's last header.
    let mut ends: Vec<(Header, u32)> = Vec::new();
    let mut salt = 0;
    for (k, (restart, ops)) in steps.into_iter().enumerate() {
        if restart {
            a.restart(&p);
            b.restart(&p);
            check(&a, &b, &known, 1, &format!("step {k}: after the reopen"));
        }
        let mut batches = Vec::new();
        let mut calls = Vec::new();
        for op in ops {
            match op {
                Op::Extend {
                    from,
                    n,
                } => {
                    let (parent, height) = match from {
                        Parent::Tip => {
                            let tip = a.chain.private_tip().expect("a tip").0.hash;
                            *known.iter().find(|(h, _)| h.hash() == tip).expect("known")
                        }
                        Parent::End(pick) if !ends.is_empty() => ends[pick % ends.len()],
                        Parent::End(pick) | Parent::Any(pick) => known[pick % known.len()],
                    };
                    salt += 1;
                    let hs = mine_n(&p, parent.hash(), parent.time() + 150 + salt, n);
                    ends.retain(|(h, _)| h.hash() != parent.hash());
                    ends.push((hs[hs.len() - 1], height + n));
                    known.extend(hs.iter().copied().zip(height + 1..));
                    batches.push(hs);
                }
                Op::Lock {
                    pick,
                } => {
                    let (h, height) = known[pick % known.len()];
                    calls.push(Call::Lock(ChainLockRef {
                        height,
                        hash: h.hash(),
                    }));
                }
                Op::Spork(on) => calls.push(Call::Spork(on)),
            }
        }
        assert_eq!(a.step(&batches, &calls), b.step(&batches, &calls), "step {k}: outcomes");
        known.retain(|(h, _)| a.id(&h.hash()).is_some());
        ends.retain(|(h, _)| a.id(&h.hash()).is_some());
        check(&a, &b, &known, 1, &format!("step {k}"));
    }
}

/// The unwindowed chain `a` and the windowed `b` give the same answers, about `known` and
/// every `stride`-th snapshot height.
pub(super) fn check(a: &Side, b: &Side, known: &[(Header, u32)], stride: u32, what: &str) {
    assert_eq!(a.answers(known), b.answers(known), "{what}: answers");
    assert_eq!(a.snapshots(stride), b.snapshots(stride), "{what}: snapshots");
    // The windowed locator walks cold ancestors to the anchor (DEC-140).
    assert_eq!(a.locators(), b.locators(), "{what}: locators");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// `prop_windowed_equals_unwindowed`: w from 2 to 48, so that most reorgs on the small
    /// windows reach below them, and fewer on the large.
    #[test]
    fn prop_windowed_equals_unwindowed(
        w in 2..=48u32,
        steps in prop::collection::vec(step(), 1..24),
    ) {
        run(w, steps);
    }
}
