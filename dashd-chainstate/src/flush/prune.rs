//! The block files' side of the prune (CH2 §2.7, §4.2, §4.5): step 0's decision on V, the node's
//! host over ST2's block files, and Core's prune-lock bound. Step 7's selection, clears and apply
//! run on W ([`FlushMachine`](super::FlushMachine)); step 12 unlinks.

use std::cell::Cell;
use std::sync::Arc;

use dashd_store::flatfile::{BlockFiles, FlatFs, last_prunable, last_prunable_manual};

use super::observer::{BenchTimer, FlushObserver, LogCat, log};
use crate::store::GuardedFiles;

/// The block files, as step 0 sees them.
pub trait PruneHost {
    /// Takes an armed automatic check (Core's `m_check_for_pruning`, cleared at `:3017`). Called
    /// only when V can decide at once: with a flush in flight the check stays armed.
    fn check_due(&mut self) -> bool;

    /// The files a prune to `bound` would select, with no side effects on the files (ST3a §5.6
    /// item 7). `manual` is `pruneblockchain`'s height: Core's
    /// `FindFilesToPruneManual(min(bound, manual), tip)`; without it, Core's automatic
    /// `FindFilesToPrune`. A host logs the selection here, once, as Core's selector does at the
    /// call site.
    fn dry_run(
        &self,
        tip_height: u32,
        bound: u32,
        manual: Option<u32>,
        is_ibd: bool,
    ) -> PruneDryRun;

    // Core: validation.cpp:3005-3007
    /// Core's "%s limited pruning to height %d", before the dry run: the lock `name` lowered
    /// the bound to `height`.
    fn limited(&self, _name: &str, _height: u32) {}

    // Core: validation.cpp:3144 (PruneAndFlush's `m_check_for_pruning = true`)
    /// Arms the automatic check, for `PruneAndFlush` (b4 D57).
    fn arm(&mut self) {}
}

/// A dry run's result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PruneDryRun {
    pub files: Vec<u32>,
    /// The target is met after pruning `files` (step 7's re-arm rule, T40).
    pub met_target: bool,
    /// The automatic target the run was made against, in bytes; 0 for a manual run.
    pub target: u64,
    /// The automatic run's `nPruneAfterHeight`; 0 for a manual run.
    pub prune_after_height: u64,
}

/// Step 0's decision, retained in the packet for step 7.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PruneDecision {
    /// D: the dry run's files.
    pub files: Vec<u32>,
    pub met_target: bool,
    /// `pruneblockchain`'s height; `None` for an automatic check.
    pub manual: Option<u32>,
    /// Core's `m_chain.Height()` at the decision: the selectors' tip, at step 0 and at step 7.
    pub tip_height: u32,
    /// [`PruneDryRun::target`].
    pub target: u64,
    /// [`PruneDryRun::prune_after_height`].
    pub prune_after_height: u64,
}

impl PruneDecision {
    /// The rule step 7 selects under.
    pub fn rule(&self) -> PruneRule {
        match self.manual {
            Some(height) => PruneRule::Manual(height),
            None => PruneRule::Automatic {
                after: self.prune_after_height,
            },
        }
    }
}

/// The rule a selection is chosen under: Core's `nLastBlockWeCanPrune` at a tip (DEC-211).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PruneRule {
    /// The automatic check: `tip − MIN_BLOCKS_TO_KEEP`, at a tip above `after`, Core's
    /// `nPruneAfterHeight`.
    Automatic {
        after: u64,
    },
    /// `pruneblockchain`'s height: `min(height, tip − MIN_BLOCKS_TO_KEEP)`.
    Manual(u32),
}

impl PruneRule {
    /// The highest block the rule lets a prune remove with the chain at `tip` and no prune lock:
    /// Core's `last_prune` is then the chain's height.
    pub fn bound(self, tip: u32) -> u32 {
        self.bound_at(tip, tip)
    }

    /// [`bound`](Self::bound) with Core's `last_prune` lowered to `last_prune` by the prune locks
    /// ([`prune_lock_bound`]).
    pub fn bound_at(self, tip: u32, last_prune: u32) -> u32 {
        match self {
            // Core: validation.cpp:3012 FindFilesToPruneManual(min(last_prune, manual), tip)
            Self::Manual(m) => last_prunable_manual(m.min(last_prune).min(tip), tip),
            // Core: validation.cpp:3016 FindFilesToPrune(…, tip, last_prune, …)
            Self::Automatic {
                ..
            } => last_prunable(i32::try_from(last_prune).unwrap_or(i32::MAX), tip),
        }
    }

    // Core: node/blockstorage.cpp BlockManager::FindFilesToPrune (`chain_tip_height <=
    // nPruneAfterHeight` returns before the bound)
    /// Whether a selection whose highest block is `highest`, chosen under this rule, may still be
    /// pruned with V's chain at `tip` (DEC-211): the rule's selector, run there, gets past its
    /// gate and finds every selected file at or below its [`bound`](Self::bound). The automatic
    /// gate is what keeps a tip rewound below `MIN_BLOCKS_TO_KEEP` from allowing every height,
    /// where the bound wraps as Core's does: Core never selects at a tip at or below
    /// `nPruneAfterHeight`.
    pub fn allows(self, highest: u32, tip: u32) -> bool {
        let gate = match self {
            Self::Automatic {
                after,
            } => u64::from(tip) > after,
            Self::Manual(_) => true,
        };
        gate && highest <= self.bound(tip)
    }
}

/// A node that does not prune.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoPrune;

impl PruneHost for NoPrune {
    fn check_due(&mut self) -> bool {
        false
    }

    fn dry_run(&self, _: u32, _: u32, _: Option<u32>, _: bool) -> PruneDryRun {
        PruneDryRun::default()
    }
}

/// The node's host over ST2's block files (CH2 §2.7), with CH8's configuration: `-prune`'s
/// target and the network's `nPruneAfterHeight`.
pub struct FilesPruneHost<F: FlatFs> {
    pub files: Arc<GuardedFiles<F>>,
    /// `-prune` in bytes; 0 for manual pruning only (`-prune=1`).
    pub target: u64,
    /// Core's `CChainParams::PruneAfterHeight()`.
    pub prune_after_height: u64,
}

impl<F: FlatFs> PruneHost for FilesPruneHost<F> {
    fn check_due(&mut self) -> bool {
        // A latched failure refuses the take: nothing is pruned after it.
        self.files.mutate(|f| Ok(f.take_check_for_pruning())).unwrap_or(false)
    }

    fn arm(&mut self) {
        // After a latched failure nothing is pruned, armed or not.
        let _ = self.files.mutate(|f| {
            f.rearm_check_for_pruning();
            Ok(())
        });
    }

    // Core: validation.cpp:3009-3017
    fn dry_run(
        &self,
        tip_height: u32,
        bound: u32,
        manual: Option<u32>,
        is_ibd: bool,
    ) -> PruneDryRun {
        let plan = self.files.read(|f| match manual {
            Some(m) => f.plan_files_to_prune_manual(m.min(bound), Some(tip_height)),
            None => f.plan_files_to_prune(
                self.target,
                self.prune_after_height,
                Some(tip_height),
                i32::try_from(bound).unwrap_or(i32::MAX),
                is_ibd,
            ),
        });
        plan.log();
        let (target, prune_after_height) = match manual {
            Some(_) => (0, 0),
            None => (self.target, self.prune_after_height),
        };
        PruneDryRun {
            files: plan.files,
            met_target: plan.met_target,
            target,
            prune_after_height,
        }
    }
}

/// Step 7's selection (CH2 §4.2): the files of D still eligible under the actual `bound`. It
/// never runs the selector again, so it is a subset of D whatever the files did since step 0
/// (T39).
pub(crate) fn final_selection<F: FlatFs>(files: &BlockFiles<F>, d: &[u32], bound: u32) -> Vec<u32> {
    d.iter().copied().filter(|&file| files.eligible(file, bound)).collect()
}

// Core: validation.cpp:3010, 3014 (LOG_TIME_MILLIS_WITH_CATEGORY around the selectors)
/// A host whose dry runs carry Core's timer: step 0's selection is Core's selector call. It keeps
/// the run, with its manual height, for step 0's mark.
pub(crate) struct TimedPrune<'a> {
    pub(crate) host: &'a mut dyn PruneHost,
    pub(crate) obs: &'a dyn FlushObserver,
    pub(crate) ran: Cell<Option<(PruneDryRun, Option<u32>)>>,
}

impl PruneHost for TimedPrune<'_> {
    fn check_due(&mut self) -> bool {
        self.host.check_due()
    }

    fn arm(&mut self) {
        self.host.arm();
    }

    fn dry_run(
        &self,
        tip_height: u32,
        bound: u32,
        manual: Option<u32>,
        is_ibd: bool,
    ) -> PruneDryRun {
        let title = match manual {
            Some(_) => "find files to prune (manual)",
            None => "find files to prune",
        };
        let timer = BenchTimer::start(self.obs, title.into());
        let run = self.host.dry_run(tip_height, bound, manual, is_ibd);
        timer.done(self.obs);
        self.ran.set(Some((run.clone(), manual)));
        run
    }

    fn limited(&self, name: &str, height: u32) {
        log(self.obs, LogCat::Prune, format!("{name} limited pruning to height {height}"));
        self.host.limited(name, height);
    }
}

// Core: validation.cpp:122
/// Core's `PRUNE_LOCK_BUFFER`: the blocks a prune lock keeps below its height.
pub const PRUNE_LOCK_BUFFER: u32 = 10;

/// A prune lock's `height_first` that locks nothing: Core's `std::numeric_limits<int>::max()`.
pub const NO_PRUNE_LOCK: u32 = i32::MAX as u32;

// Core: validation.cpp:2992-3007 (the prune-lock loop of FlushStateToDisk)
/// Core's `last_prune` (CH2 §4.5): the chain's height, lowered by each prune lock
/// `(name, height_first)` to `height_first − PRUNE_LOCK_BUFFER − 1`, computed in i64 and clamped
/// to at least 1 as Core's `std::max(1, std::min(last_prune, lock_height))`; and the lock Core
/// names in "%s limited pruning to height %d", the last one whose lock height `last_prune` equals
/// after its clamp, so with equal heights the name follows `locks`' order, which for Core is its
/// `unordered_map`'s. A lock at Core's `int` maximum, [`NO_PRUNE_LOCK`], is skipped. Step 0 calls
/// it with the hub's predicted locks, step 7 with its actual ones.
pub fn prune_lock_bound<'a>(
    chain_height: u32,
    locks: impl IntoIterator<Item = (&'a str, u32)>,
) -> (u32, Option<&'a str>) {
    let mut last_prune = i64::from(chain_height);
    let mut limiting = None;
    for (name, height_first) in locks {
        if height_first == NO_PRUNE_LOCK {
            continue;
        }
        let lock_height = i64::from(height_first) - i64::from(PRUNE_LOCK_BUFFER) - 1;
        last_prune = last_prune.min(lock_height).max(1);
        if last_prune == lock_height {
            limiting = Some(name);
        }
    }
    (u32::try_from(last_prune).expect("the chain's height, or at least 1"), limiting)
}

#[cfg(test)]
mod tests {
    use dashd_store::ErrorKind;
    use dashd_store::flatfile::{BlockFiles, BlockFilesConfig, MIN_BLOCKS_TO_KEEP, OsFs};

    use super::*;
    use crate::flush::test_files::equal_files;
    use crate::store::{Failure, GlobalFailStop};

    /// DEC-211: a selection is allowed at a tip whose rule's bound covers its highest block, and
    /// declined at one rewound below it. The automatic bound wraps below `MIN_BLOCKS_TO_KEEP` as
    /// Core's does, allowed only past `nPruneAfterHeight`, where Core's selector would run; a
    /// manual one is `min(height, tip − MIN_BLOCKS_TO_KEEP)`.
    #[test]
    fn a_rule_allows_a_selection_at_a_tip_its_bound_covers() {
        let auto = |after| PruneRule::Automatic {
            after,
        };
        let k = MIN_BLOCKS_TO_KEEP;
        // (rule, highest, V's tip, allowed)
        let cases = [
            (auto(0), 19, 19 + k, true),
            (auto(0), 19, 18 + k, false),
            // -fastprune's 100: Core's wrap at 101 allows every height; at 100, no selector runs.
            (auto(100), 400, 101, true),
            (auto(100), 19, 100, false),
            (auto(1000), 19, 150, false),
            (PruneRule::Manual(15), 15, 15 + k, true),
            (PruneRule::Manual(15), 9, 8 + k, false),
            (PruneRule::Manual(15), 16, 400, false),
        ];
        for (rule, highest, tip, allowed) in cases {
            assert_eq!(
                rule.allows(highest, tip),
                allowed,
                "{rule:?}, highest {highest}, tip {tip}"
            );
        }
    }

    #[test]
    fn the_files_host_plans_without_pruning_and_takes_the_check() {
        let dir = tempfile::tempdir().unwrap();
        // Four 16 MiB files, each of ten blocks; the last is the current one.
        let state = equal_files(4, 12 << 20, 4 << 20);
        let cfg = BlockFilesConfig::new(dir.path(), [0xfc, 0xc1, 0xb7, 0xdc]);
        let files = BlockFiles::with_fs(Arc::new(OsFs::new()), cfg, state.clone());
        let latch = GlobalFailStop::new();
        let mut host = FilesPruneHost {
            files: Arc::new(GuardedFiles::new(files, latch.clone())),
            target: 40 << 20,
            prune_after_height: 100,
        };
        // Core's tip − MIN_BLOCKS_TO_KEEP is 19.
        let tip = 19 + MIN_BLOCKS_TO_KEEP;
        let manual = host.dry_run(tip, 500, Some(9), false);
        let want = PruneDryRun {
            files: vec![0],
            met_target: true,
            target: 0,
            prune_after_height: 0,
        };
        assert_eq!(manual, want, "pruneblockchain prunes to min(bound, height)");
        // The automatic selector takes both files at or below 19: 64 − 32 MiB plus the 17 MiB
        // buffer leaves its 40 MiB target unmet.
        let automatic = host.dry_run(tip, u32::MAX, None, false);
        let want = PruneDryRun {
            files: vec![0, 1],
            met_target: false,
            target: 40 << 20,
            prune_after_height: 100,
        };
        assert_eq!(automatic, want);
        assert_eq!(host.dry_run(tip, 9, None, false).files, vec![0], "the bound is prune_height");
        assert_eq!(
            host.files.read(|f| f.infos().to_vec()),
            state.infos,
            "a dry run prunes nothing"
        );

        let rearm = |host: &FilesPruneHost<OsFs>| {
            host.files
                .mutate(|f| {
                    f.rearm_check_for_pruning();
                    Ok(())
                })
                .unwrap();
        };
        assert!(!host.check_due());
        rearm(&host);
        assert!(host.check_due());
        assert!(!host.check_due(), "the check is taken");
        rearm(&host);
        latch.trip(Failure {
            kind: ErrorKind::Other,
            site: "test",
            message: "a failure".into(),
        });
        assert!(!host.check_due(), "nothing is pruned after a latched failure");
    }

    #[test]
    fn locks_lower_the_bound_and_clamp_it_to_one() {
        assert_eq!(prune_lock_bound(500, []), (500, None));
        // 300 − 10 − 1.
        assert_eq!(prune_lock_bound(500, [("txindex", 300)]), (289, Some("txindex")));
        // The lowest lock limits; one above the chain's height does not.
        let locks = [("a", 400), ("b", 120), ("c", 9_000)];
        assert_eq!(prune_lock_bound(500, locks), (109, Some("b")));
        // A lock near genesis would underflow a u32: Core's int clamps it to 1, and the clamped
        // value is no lock's height, so none is named.
        assert_eq!(prune_lock_bound(500, [("a", 0)]), (1, None));
        assert_eq!(prune_lock_bound(500, [("a", 5), ("b", 3)]), (1, None));
        // A lock exactly at 1 after the buffer names itself.
        assert_eq!(prune_lock_bound(500, [("a", 12)]), (1, Some("a")));
        // Equal lock heights: the last one is named, as Core's loop assigns on equality.
        assert_eq!(prune_lock_bound(500, [("a", 200), ("b", 200)]), (189, Some("b")));
        // A later, higher lock leaves the bound and the name.
        assert_eq!(prune_lock_bound(500, [("a", 200), ("b", 300)]), (189, Some("a")));
        // A chain below the buffer with a lock: clamped to 1, as Core's `max(1, …)`.
        assert_eq!(prune_lock_bound(0, [("a", 0)]), (1, None));
        // A lock at Core's `int` maximum is skipped, and so never clamps a chain at 0 either.
        assert_eq!(prune_lock_bound(500, [("a", NO_PRUNE_LOCK), ("b", 300)]), (289, Some("b")));
        assert_eq!(prune_lock_bound(0, [("a", NO_PRUNE_LOCK)]), (0, None));
        assert_eq!(prune_lock_bound(500, [("a", NO_PRUNE_LOCK - 1)]), (500, None));
    }

    /// `prune_selection_protocol` (CH2 §4.2; ST3a §6.2's fixtures from GPT r4, at
    /// `MIN_BLOCKS_TO_KEEP`): step 0's D from the files host, then step 7's `D ∩ eligible(actual)`
    /// ([`final_selection`]) and its re-arm, against T39's re-run selector.
    mod prune_selection_protocol {
        use dashd_store::flatfile::{BlockFileInfo, BlockFilesState};

        use super::*;

        const K: u32 = MIN_BLOCKS_TO_KEEP;
        /// ST2's room under the target: a 16 MiB blk chunk and a 1 MiB rev chunk.
        const BUFFER: u64 = (16 << 20) + (1 << 20);

        fn info(blocks: u32, first: u32, last: u32) -> BlockFileInfo {
            BlockFileInfo {
                blocks,
                size: blocks * 1032,
                undo_size: blocks * 280,
                height_first: first,
                height_last: last,
                time_first: u64::from(first),
                time_last: u64::from(last),
            }
        }

        fn usage(infos: &[BlockFileInfo]) -> u64 {
            infos.iter().map(|i| u64::from(i.size) + u64::from(i.undo_size)).sum()
        }

        /// A host over `infos`, the last file current.
        fn host(
            dir: &std::path::Path,
            infos: &[BlockFileInfo],
            target: u64,
        ) -> FilesPruneHost<OsFs> {
            let state = BlockFilesState {
                infos: infos.to_vec(),
                last_file: infos.len() as u32 - 1,
            };
            let mut cfg = BlockFilesConfig::new(dir, [0xfc, 0xc1, 0xb7, 0xdc]);
            cfg.prune_mode = true;
            let files = BlockFiles::with_fs(Arc::new(OsFs::new()), cfg, state);
            FilesPruneHost {
                files: Arc::new(GuardedFiles::new(files, GlobalFailStop::new())),
                target,
                prune_after_height: 100,
            }
        }

        fn step7(host: &FilesPruneHost<OsFs>, d: &[u32], bound: u32) -> Vec<u32> {
            host.files.read(|f| final_selection(f, d, bound))
        }

        fn target_met(host: &FilesPruneHost<OsFs>) -> bool {
            host.files.read(|f| f.prune_target_met(host.target))
        }

        /// A side branch: closed files with last heights 62 and 40, then the current file; the
        /// target is met by removing one closed file.
        #[test]
        fn side_branch() {
            let dir = tempfile::tempdir().unwrap();
            let infos = [info(63, 0, 62), info(11, 30, 40), info(1, 63, 600)];
            let target = usage(&infos) - usage(&infos[..1]) + BUFFER + 1000;
            let host = host(dir.path(), &infos, target);
            let tip = 597 + K;
            // Step 0 with no lock (bound 597): D = {0}, met.
            let d = host.dry_run(tip, u32::MAX, None, false);
            assert_eq!((d.files.as_slice(), d.met_target), (&[0][..], true));
            // Step 7 under an actual bound of 47 (a lock, CH2b b3): nothing in D is eligible, the
            // target stays unmet where D had met it, so the check re-arms.
            assert!(step7(&host, &d.files, 47).is_empty());
            assert!(!target_met(&host), "re-armed");
            // T39: the selector re-run under the actual bound picks {1}, outside D.
            let t39 = host.dry_run(tip, 47, None, false);
            assert_eq!(t39.files, vec![1]);
            // The next check, under the same bound, selects {1}.
            assert_eq!(step7(&host, &t39.files, 47), vec![1]);
            // The manual control: {0, 1}, then {1} under the bound; a subset either way.
            let manual = host.dry_run(tip, u32::MAX, Some(597), false);
            assert_eq!(manual.files, vec![0, 1]);
            assert_eq!(step7(&host, &manual.files, 47), vec![1]);
        }

        /// V ahead: files with last heights 62, 125, 188 and the current one; the target is 516
        /// bytes above the usage left once file 0 goes; V appends one 1,032-byte block between
        /// step 0 and step 7.
        #[test]
        fn v_ahead() {
            let dir = tempfile::tempdir().unwrap();
            let mut infos =
                vec![info(63, 0, 62), info(63, 63, 125), info(63, 126, 188), info(12, 189, 200)];
            let target = usage(&infos) - usage(&infos[..1]) + BUFFER + 516;
            let tip = 192 + K;
            let d = host(dir.path(), &infos, target).dry_run(tip, u32::MAX, None, false);
            assert_eq!((d.files.as_slice(), d.met_target), (&[0][..], true));
            infos[3].size += 1032;
            let ahead = host(dir.path(), &infos, target);
            assert_eq!(step7(&ahead, &d.files, tip - K), vec![0]);
            ahead
                .files
                .mutate(|f| {
                    f.prune_one_block_file(0);
                    Ok(())
                })
                .unwrap();
            assert!(!target_met(&ahead), "short by 516 bytes: re-armed");
            // T39: the re-run selector picks {0, 1}, outside D.
            let ahead = host(dir.path(), &infos, target);
            assert_eq!(ahead.dry_run(tip, u32::MAX, None, false).files, vec![0, 1]);
        }
    }
}
