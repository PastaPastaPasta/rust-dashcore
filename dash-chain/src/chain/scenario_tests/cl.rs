//! SB4 §15's ChainLock replays: the harness's `cl`-mode vectors through `ScriptedV` on a node
//! chain, compared at Core's fixed point (DEC-167).
//!
//! Every op runs on the chain: `cl` (Core's `ProcessNewChainLock` order, with the P2P spork gate
//! or `submitchainlock`'s height checks), `seed-cl`, `spork19`, `timer`, `enforce`, `notify`,
//! `check-active-state`, `mark-conflicting`, `reload` (from the records written so far) and
//! `locator`. Blocks (`build` with `submit: "connect"`, `connect`) go through a model of
//! `ProcessNewBlock`: the header, the block's data, and `ActivateBestChain` (CH1-node's in the
//! product). After each op that makes Core's handler schedule work, the driver re-runs
//! enforcement passes, as dashd's rescheduled update and timer do, until a pass changes nothing;
//! the view is compared at that fixed point. Each run's first enforcement is compared with
//! Core's: the `EnforceBlock` boundary (tip, best header and every status, which is where the
//! marked ids are checked), and the trace of its pass: per level, the mark count, the disconnects
//! and the tip after it (D9 (4) leaves the order of a level's siblings open), then any connects
//! in the pass's trace segment, exactly.
//! `invalidate` and `reconsider` (a18, a28) are CH1-node's and not replayed here.

use super::*;
use crate::chainlock::{ClOutcome, EnforceReport, updated_block_tip_schedules};
use crate::config::SporkView as _;
use crate::host::TipHost;

/// The ChainLock vectors, each with the number of enforcements its replay compares with Core's.
const VECTORS: &[(&str, usize)] = &[
    ("a16-cl-on-active-heavier-header", 1),
    ("a17-conflict-then-reload", 1),
    ("a21-three-branches", 3),
    ("a21b-ancestor-floor-repeat", 33),
    ("a22-lock-on-active-heavier-below", 1),
    ("a26-i-lock-on-header-only", 1),
    ("a26-ii-enforcement-reorg", 1),
    ("a29-i-off-on-lower-height", 1),
    ("a29-i-p2p-lock-on-off-on", 2),
    ("a29-ii-late-header-after-check", 0),
    ("a29-ii-late-header-before-check", 0),
    ("a29-iii-on-off", 1),
    ("a29-iv-restart", 1),
    ("a29-ix-no-observed-off", 1),
    ("a29-ix-reset-then-lower-lock", 2),
    ("a29-v-marks-survive", 1),
    ("a29-vii-fresh-first-lock", 1),
    ("a29-viii-notify", 2),
    ("a29-x-kept-mark-trailing", 2),
    ("a29-x-prime-headers-only-kept-mark", 2),
    ("a35-resident-to-cold", 1),
    ("a36-conflicting-marked-order", 1),
    ("r2m2-first-lock-seed-order", 0),
    ("r2m2-lower-height-seed-order", 0),
    ("submitchainlock-nonpositive-height", 1),
];

scenario_test! {
    #[replays(VECTORS.iter().map(|&(name, _)| name))]
    fn replay_cl_vectors() {
        for &(name, enforced) in VECTORS {
            assert_eq!(replay_cl(name), enforced, "{name}: enforcements compared with Core's");
        }
    }
}

/// The node's `TipHost` for the replays (SB4 §15): `m_chain` is the writer's private active
/// chain, `DisconnectTip` is the chain's step, and each `MarkConflictingBlock`'s remaining work
/// is logged with the tip Core passes to `UpdatedBlockTip(tip, nullptr)`.
#[derive(Default)]
struct ScriptedV {
    log: Vec<Ev>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ev {
    Disconnected(BlockId),
    /// `conflicting_marked`: the marked root, and `m_chain.Tip()` at Core's notification.
    Marked {
        marked: BlockId,
        tip: BlockId,
    },
    Connected(BlockId),
}

impl TipHost<NoStore, Permanent> for ScriptedV {
    type Error = ChainError;

    fn tip(&self, chain: &HeaderChain) -> BlockId {
        chain.private_tip().expect("a tip").0.id
    }

    fn contains(&self, chain: &HeaderChain, id: BlockId) -> Result<bool, ChainError> {
        Ok(chain.active_contains_private(id))
    }

    fn disconnect_tip(&mut self, chain: &mut HeaderChain) -> Result<(), ChainError> {
        let step = chain.disconnect_tip()?;
        self.log.push(Ev::Disconnected(step.block().id));
        Ok(())
    }

    fn conflicting_marked(
        &mut self,
        chain: &mut HeaderChain,
        marked: BlockId,
        _was_in_chain: bool,
    ) -> Result<(), ChainError> {
        let tip = self.tip(chain);
        self.log.push(Ev::Marked {
            marked,
            tip,
        });
        Ok(())
    }
}

/// What starts a run of passes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum First {
    /// A scheduled task: `CheckActiveState` then `EnforceBestChainLock` (`h:63-72`, `:170-176`,
    /// `:196-205`).
    Scheduled,
    /// The harness's `enforce`: `EnforceBestChainLock` alone.
    EnforceOnly,
}

/// One pass: its enforcement, the tip and best header at the `EnforceBlock` boundary, and the
/// host's events through the `ActivateBestChain` after it.
struct Pass {
    report: Option<EnforceReport>,
    /// The tip, best header and statuses at the `EnforceBlock` boundary (enforced passes only).
    boundary: (BlockId, BlockId, BTreeMap<String, u32>),
    log: Vec<Ev>,
}

/// Replays `name`'s steps; returns the number of first enforcements compared with Core's trace.
fn replay_cl(name: &str) -> usize {
    let Scenario {
        vector,
        params,
        ..
    } = load_scenario(name);
    assert_eq!(vector["network"], "regtest", "{name}");
    let mut r = Replay::on(params);
    // `LoadGenesisBlock`'s data: genesis has `VALID_TRANSACTIONS` and data, never `ConnectTip`.
    r.chain
        .modify(BlockId(0), |g| g.status = Status(Status::VALID_TRANSACTIONS | Status::HAVE_DATA));
    r.chain.mark_dirty(BlockId(0));
    // The block tree's records: each step's writes, as Core's flush leaves them.
    let mut disk = HashMap::new();
    let mut compared = 0;
    for (i, step) in vector["steps"].as_array().expect("steps").iter().enumerate().skip(1) {
        let at = format!("{name} step {i}");
        let (op, result, view) = (&step["op"], &step["result"], &step["view"]);
        disk.extend(r.chain.take_dirty().into_iter().map(|w| (w.hash, w.bytes)));
        let before = r.statuses();
        // The runs of passes the op scheduled, one per `UpdatedBlockTip` or lock.
        let mut runs = Vec::new();
        match op["op"].as_str().expect("op name") {
            "build" => {
                assert!(op.get("invalid").is_none(), "{at}: invalid blocks are CH1-node's");
                let labels = r.build(op, result);
                match op["submit"].as_str().unwrap_or("none") {
                    "headers" => r.pnbh(&labels, None, &at),
                    "connect" => runs = labels.iter().filter_map(|l| r.process_block(l)).collect(),
                    _ => {}
                }
            }
            "pnbh" => r.pnbh(&expand(&op["headers"]), None, &at),
            "connect" => {
                runs = expand(&op["blocks"]).iter().filter_map(|l| r.process_block(l)).collect();
            }
            "spork19" => {
                r.now += 1;
                r.sporks.set(op["value"] == "on");
            }
            "timer" => runs.push(r.passes(First::Scheduled)),
            "enforce" => runs.push(r.passes(First::EnforceOnly)),
            "check-active-state" => {
                r.chain.check_cl_active_state();
            }
            "notify" => {
                let id = |v: &Value| v.as_str().map(|l| r.id(l));
                let (new, fork) = (id(&op["new"]).expect("new"), id(&op["fork"]));
                let scheduled = updated_block_tip_schedules(new, fork, op["ibd"] == true);
                assert_eq!(result["scheduled"], scheduled, "{at}: scheduled");
                if scheduled {
                    runs.push(r.passes(First::Scheduled));
                }
            }
            "seed-cl" => {
                let cl = r.lock(result);
                let updated = !matches!(r.chain.record_chainlock(cl), ClOutcome::Stale);
                assert_eq!(result["updated"], updated, "{at}: updated");
            }
            "cl" => runs.extend(r.cl(op, result, &at)),
            "mark-conflicting" => {
                let b = r.id(label(&op["block"]));
                let mut v = ScriptedV::default();
                r.chain.mark_conflicting_block(b, &mut v, &mut Vec::new()).expect("marks");
                r.abc(&mut v);
                runs.push(r.passes(First::Scheduled));
            }
            "reload" => {
                r.reload_from(&disk, op["checkpoints"].as_bool().unwrap_or(true));
                r.set_tip(r.id(label(&view["tip"])));
            }
            "locator" => {
                let id = r.id(label(&op["block"]));
                let got: Vec<String> = r
                    .chain
                    .reader()
                    .locator(Some(id))
                    .iter()
                    .map(|h| r.labels[h].clone())
                    .collect();
                let core: Vec<String> = result
                    .as_array()
                    .expect("locator")
                    .iter()
                    .map(|l| label(l).to_owned())
                    .collect();
                assert_eq!(got, core, "{at}: locator");
            }
            other => panic!("{at}: {other} is not a ChainLock op"),
        }
        compared += r.compare_runs(&runs, view, &at);
        r.compare_view(&before, view, &at);
    }
    compared
}

impl Replay {
    /// The harness's `reload` from the records written so far: a mark that was never dirty
    /// (D15) is gone, as in Core.
    fn reload_from(&mut self, disk: &HashMap<BlockHash, Vec<u8>>, checkpoints: bool) {
        self.reload_with(disk.iter().map(|(h, b)| (*h, b.clone())), checkpoints);
    }

    /// The step's lock from its `result`.
    fn lock(&self, result: &Value) -> ChainLockRef {
        self.lock_ref(label(&result["lock"]))
    }

    /// The harness's `ProcessNewBlock` for a block whose header the replay holds: the header,
    /// `ReceivedBlockTransactions` (data, `VALID_TRANSACTIONS`, dirty), the `ActivateBestChain`
    /// model, and with a new tip, the passes its `UpdatedBlockTip` schedules (never in IBD here).
    fn process_block(&mut self, l: &str) -> Option<Vec<Pass>> {
        self.clock.set_mocktime(self.now);
        let r = self.chain.accept(self.batch(&[l.to_owned()]), None).expect("accept");
        assert_eq!(r.stop, None, "{l}");
        let id = self.id(l);
        let s = self.chain.writer_status(id).expect("known");
        if !s.has(Status::HAVE_DATA) {
            let got =
                Status(s.raise_validity(Status::VALID_TRANSACTIONS).0.bits() | Status::HAVE_DATA);
            self.chain.modify(id, |g| g.status = got);
            self.chain.mark_dirty(id);
        }
        let old = self.tip();
        self.abc(&mut ScriptedV::default());
        let new = self.tip();
        let fork = self.chain.find_fork_private(old).expect("a tip");
        (new != old && updated_block_tip_schedules(new, Some(fork), false))
            .then(|| self.passes(First::Scheduled))
    }

    /// The harness's `cl`: the P2P gate or `submitchainlock`'s checks, then
    /// `ProcessNewChainLock`'s index side in Core's order; with a known block, the scheduled
    /// passes. `None` when the lock never reached `UpdateBestChainlock`'s floor.
    fn cl(&mut self, op: &Value, result: &Value, at: &str) -> Option<Vec<Pass>> {
        let height = op["height"].as_i64();
        match op["via"].as_str().unwrap_or("p2p") {
            "p2p" if !self.sporks.chainlocks_enabled() => {
                assert_eq!(result["dropped"], "spork19-off", "{at}");
                return None;
            }
            "p2p" => {}
            "submitchainlock" => {
                if height.is_some_and(|h| h <= 0) {
                    assert_eq!(result["error"], "invalid block height", "{at}");
                    return None;
                }
                let best = self.chain.cl_best.map_or(-1, |b| i64::from(b.height));
                let cl = self.lock(result);
                if i64::from(cl.height) <= best {
                    assert!(result["dropped"].is_string(), "{at}: dropped");
                    return None;
                }
            }
            via => panic!("{at}: via {via}"),
        }
        let cl = self.lock(result);
        // h:136, then the precheck at h:147-151 and UpdateBestChainlock (cl:106-132).
        self.chain.check_cl_active_state();
        match self.chain.record_chainlock(cl) {
            ClOutcome::Floor {
                ..
            } => Some(self.passes(First::Scheduled)),
            _ => None,
        }
    }

    /// Enforcement passes until one changes nothing (DEC-167): each runs the scheduled task's
    /// `CheckActiveState` (not the harness's `enforce`), `EnforceBestChainLock`'s `EnforceBlock`,
    /// and its `ActivateBestChain` when the tip does not hold the floor (`h:299-300`).
    fn passes(&mut self, first: First) -> Vec<Pass> {
        let mut passes = Vec::new();
        let mut kind = first;
        loop {
            let before = self.digest();
            if kind == First::Scheduled {
                self.chain.check_cl_active_state();
            }
            let mut v = ScriptedV::default();
            let report = self.chain.enforce_best_chainlock(&mut v).expect("enforces");
            let statuses = report.as_ref().map(|_| self.statuses()).unwrap_or_default();
            let boundary = (self.tip(), self.chain.best_header(), statuses);
            if let Some(floor) = report.as_ref().map(|r| r.floor) {
                let floor_h = self.chain.shared.arena.imm(floor, |e| e.height).expect("resident");
                if self.chain.ancestor_private(floor_h) != Some(floor) {
                    self.abc(&mut v);
                }
            }
            let moved = v.log.iter().any(|e| !matches!(e, Ev::Marked { .. }));
            passes.push(Pass {
                report,
                boundary,
                log: v.log,
            });
            if !moved && self.digest() == before {
                return passes;
            }
            assert!(passes.len() < 16, "no fixed point after {} passes", passes.len());
            kind = First::Scheduled;
        }
    }

    /// The state a pass can change: statuses, tip, best header, floor, best lock, observed flag
    /// and the dirty set.
    fn digest(&self) -> impl PartialEq + use<> {
        let c = &self.chain;
        (
            c.ids().map(|id| c.writer_status(id).expect("known").bits()).collect::<Vec<_>>(),
            self.tip(),
            c.best_header(),
            c.cl_floor(),
            c.cl_best,
            c.cl_enabled_observed(),
            c.dirty.ids().to_vec(),
        )
    }

    fn tip(&self) -> BlockId {
        self.chain.private_tip().expect("a tip").0.id
    }

    /// A model of Core's `ActivateBestChain` over the replay's block data (`HAVE_DATA`, CH1-node's
    /// in the product): the most-work candidate (`VALID_TRANSACTIONS` with data, not conflicting,
    /// more work than the tip, the lower id on a tie) whose path to the active chain holds data
    /// and no FAILED or CONFLICT entry. A path through a CONFLICT entry is dropped and its upper
    /// part marked, as `FindMostWorkChain` does (`validation.cpp:3533-3575`). `ConnectTip` raises
    /// each block to `VALID_SCRIPTS` with undo data, dirty when that changes it.
    fn abc(&mut self, v: &mut ScriptedV) {
        let status = |c: &HeaderChain, id| c.writer_status(id).expect("known");
        loop {
            let tip = self.tip();
            let key = |c: &HeaderChain, id: BlockId| (c.work(id), Reverse(id));
            let tip_key = key(&self.chain, tip);
            let mut candidates: Vec<BlockId> = self
                .chain
                .ids()
                .filter(|&id| {
                    let s = status(&self.chain, id);
                    s.has(Status::HAVE_DATA)
                        && s.is_valid(Status::VALID_TRANSACTIONS)
                        && !s.has(Status::CONFLICT_CHAINLOCK)
                        && key(&self.chain, id) > tip_key
                })
                .collect();
            candidates.sort_by_key(|&id| Reverse(key(&self.chain, id)));
            let mut target = None;
            for c in candidates {
                let mut test = c;
                let mut usable = true;
                while !self.chain.active_contains_private(test) {
                    let s = status(&self.chain, test);
                    if s.has(Status::FAILED_MASK)
                        || s.has(Status::CONFLICT_CHAINLOCK)
                        || !s.has(Status::HAVE_DATA)
                    {
                        if s.has(Status::CONFLICT_CHAINLOCK) {
                            let mut x = c;
                            while x != test {
                                self.chain.modify(x, |g| {
                                    g.status = Status(g.status.bits() | Status::CONFLICT_CHAINLOCK)
                                });
                                self.chain.mark_dirty(x);
                                x = self
                                    .chain
                                    .shared
                                    .arena
                                    .imm(x, |e| e.prev)
                                    .flatten()
                                    .expect("prev");
                            }
                        }
                        usable = false;
                        break;
                    }
                    test = self.chain.shared.arena.imm(test, |e| e.prev).flatten().expect("prev");
                }
                if usable {
                    target = Some(c);
                    break;
                }
            }
            let Some(target) = target else {
                return;
            };
            let fork = self.chain.find_fork_private(target).expect("a tip");
            while self.tip() != fork {
                let step = self.chain.disconnect_tip().expect("above the fork");
                v.log.push(Ev::Disconnected(step.block().id));
            }
            let arena = &self.chain.shared.arena;
            let mut path: Vec<BlockId> =
                std::iter::successors(Some(target), |&id| arena.imm(id, |e| e.prev).flatten())
                    .take_while(|&id| id != fork)
                    .collect();
            path.reverse();
            for id in path {
                self.chain.connect_tip(id).expect("prev is the tip");
                let s = status(&self.chain, id);
                let raised =
                    Status(s.raise_validity(Status::VALID_SCRIPTS).0.bits() | Status::HAVE_UNDO);
                if raised != s {
                    self.chain.modify(id, |g| g.status = raised);
                    self.chain.mark_dirty(id);
                }
                v.log.push(Ev::Connected(id));
            }
        }
    }

    /// Compares each run's first enforcement with Core's, in order: the `EnforceBlock`
    /// boundary (tip, best header, every status) and the trace of its pass; returns how many.
    fn compare_runs(&self, runs: &[Vec<Pass>], view: &Value, at: &str) -> usize {
        let firsts: Vec<&Pass> =
            runs.iter().filter_map(|run| run.iter().find(|p| p.report.is_some())).collect();
        let boundaries = view["enforce_block_boundaries"].as_array().cloned().unwrap_or_default();
        let cores: Vec<&Value> = boundaries.iter().filter(|b| b["pass"] == 1).collect();
        assert_eq!(firsts.len(), cores.len(), "{at}: enforcements");
        let trace: Vec<&str> = view["trace"].as_array().expect("trace").iter().map(label).collect();
        let mut cursor = 0;
        for (k, (pass, core)) in firsts.iter().zip(&cores).enumerate() {
            let at = format!("{at} enforcement {k}");
            let report = pass.report.as_ref().expect("enforced");
            let locked = label(&core["locked"]);
            assert_eq!(self.label(report.floor), locked, "{at}: locked block");
            let (tip, best, statuses) = &pass.boundary;
            assert_eq!(self.label(*tip), label(&core["tip"]), "{at}: boundary tip");
            assert_eq!(self.label(*best), label(&core["best_header"]), "{at}: boundary best");
            let core_statuses: BTreeMap<String, u32> =
                serde_json::from_value(core["statuses"].clone()).expect("statuses");
            let differ: Vec<_> = core_statuses
                .keys()
                .chain(statuses.keys())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .filter(|l| statuses.get(*l) != core_statuses.get(*l))
                .map(|l| (l, statuses.get(l), core_statuses.get(l)))
                .collect();
            assert!(differ.is_empty(), "{at}: boundary statuses (ours, Core's): {differ:?}");

            let open = format!("EnforceBlock({locked}) #1");
            let close = format!("{open} end");
            let start = cursor
                + trace[cursor..].iter().position(|&t| t == open).expect("the pass in the trace")
                + 1;
            let end = start + trace[start..].iter().position(|&t| t == close).expect("its end");
            cursor = end + 1;
            let core: Vec<Ev> = trace[start..end].iter().filter_map(|t| self.event(t)).collect();
            assert_eq!(
                normalize(&core, &pass.log, |id| self.height(id)),
                normalize(&pass.log, &pass.log, |id| self.height(id)),
                "{at}: the pass's trace"
            );
        }
        firsts.len()
    }

    /// A trace entry as a host event: `BlockDisconnected`, the queued `UpdatedBlockTip` with a
    /// null fork (a mark's notification), `BlockConnected`.
    fn event(&self, t: &str) -> Option<Ev> {
        let arg = |p: &str| t.strip_prefix(p)?.strip_suffix(')');
        if let Some(l) = arg("BlockDisconnected(") {
            return Some(Ev::Disconnected(self.id(l)));
        }
        if let Some(l) = arg("BlockConnected(") {
            return Some(Ev::Connected(self.id(l)));
        }
        let args = arg("UpdatedBlockTip(")?;
        let mut parts = args.split(", ");
        let (tip, fork) = (parts.next()?, parts.next()?);
        (fork == "null").then(|| Ev::Marked {
            marked: BlockId(u32::MAX),
            tip: self.id(tip),
        })
    }

    fn height(&self, id: BlockId) -> u32 {
        self.chain.shared.arena.imm(id, |e| e.height).expect("resident")
    }

    /// Compares the view at the fixed point: tip, best header, status changes since the step
    /// began, failed blocks, and the ChainLock state.
    fn compare_view(&self, before: &BTreeMap<String, u32>, view: &Value, at: &str) {
        assert_eq!(self.label(self.tip()), label(&view["tip"]), "{at}: tip");
        assert_eq!(self.label(self.chain.best_header()), label(&view["best_header"]), "{at}: best");
        let changed: BTreeMap<String, u32> =
            self.statuses().into_iter().filter(|(l, s)| before.get(l) != Some(s)).collect();
        assert_eq!(changed, status_changes(view), "{at}: status changes");
        let failed: BTreeSet<String> = self.chain.failed.iter().map(|id| self.label(id)).collect();
        assert_eq!(failed, labels_of(&view["failed_blocks"]), "{at}: failed blocks");
        let cl = &view["cl"];
        let core_best = cl["best"].as_str().map(|b| self.lock_ref(b));
        assert_eq!(self.chain.cl_best, core_best, "{at}: best lock");
        let floor = self.chain.cl_floor().map(|id| self.label(id));
        assert_eq!(floor.as_deref(), cl["floor"].as_str(), "{at}: floor");
        assert_eq!(cl["observed"], self.chain.cl_enabled_observed(), "{at}: observed");
        assert_eq!(cl["spork_active"], self.sporks.chainlocks_enabled(), "{at}: live spork");
    }
}

/// A pass's events normalized for the order of each level's siblings (D9 (4)): `events` is cut
/// into levels by the heights of `ours`' marks (Core's trace names the tip, not the marked
/// block, so its marks are only counted here), and a level becomes its mark count, its
/// disconnects and the tip after it. From the second level on, each tip a level notifies must be
/// the tip before the level or after it, as one order or another of its siblings gives; the first
/// level's tip before is its own first notification. The events after the marks stay exact.
fn normalize(events: &[Ev], ours: &[Ev], height: impl Fn(BlockId) -> u32) -> Vec<String> {
    let levels: Vec<u32> = ours
        .iter()
        .filter_map(|e| match e {
            Ev::Marked {
                marked,
                ..
            } => Some(height(*marked)),
            _ => None,
        })
        .collect();
    let marks = events.iter().filter(|e| matches!(e, Ev::Marked { .. })).count();
    assert_eq!(marks, levels.len(), "mark count");
    let mut out = Vec::new();
    let mut i = 0;
    let mut tip_before = None;
    let mut level_at = 0;
    while level_at < levels.len() {
        let level = levels[level_at];
        let n = levels[level_at..].iter().take_while(|&&l| l == level).count();
        let (mut disconnects, mut tips) = (Vec::new(), Vec::new());
        let mut seen = 0;
        while seen < n {
            match &events[i] {
                Ev::Disconnected(d) => disconnects.push(*d),
                Ev::Marked {
                    tip,
                    ..
                } => {
                    tips.push(*tip);
                    seen += 1;
                }
                Ev::Connected(c) => panic!("a connect {c:?} among the marks"),
            }
            i += 1;
        }
        let after = *tips.last().expect("a level marks");
        let start = tip_before.unwrap_or(tips[0]);
        assert!(tips.iter().all(|&t| t == start || t == after), "level {level}: tips {tips:?}");
        out.push(format!(
            "level {level}: {n} marks, disconnects {disconnects:?}, then tip {after:?}"
        ));
        tip_before = Some(after);
        level_at += n;
    }
    out.extend(events[i..].iter().map(|e| format!("{e:?}")));
    out
}
