//! The trigger table (CH2 §3.3; b1's acceptance "the trigger-table unit test, including the mapping
//! of `CoinsBoundary` onto Core's modes").

use std::cell::RefCell;
use std::time::{Duration, Instant};

use super::FlushStateMode::{Always, IfNeeded, None as NoneMode, Periodic};
use super::*;
use crate::flush::prune::PruneDryRun;

/// A dry run's arguments: the tip, the bound, the manual height and whether in IBD.
type DryRun = (u32, u32, Option<u32>, bool);

/// A prune host whose automatic check and dry run the test sets, and which records its calls.
#[derive(Default)]
struct Host {
    armed: bool,
    files: Vec<u32>,
    checks: u32,
    runs: RefCell<Vec<DryRun>>,
}

impl PruneHost for Host {
    fn check_due(&mut self) -> bool {
        self.checks += 1;
        std::mem::take(&mut self.armed)
    }

    fn dry_run(&self, tip: u32, bound: u32, manual: Option<u32>, ibd: bool) -> PruneDryRun {
        self.runs.borrow_mut().push((tip, bound, manual, ibd));
        PruneDryRun {
            files: self.files.clone(),
            met_target: true,
            target: 7,
            prune_after_height: 0,
        }
    }
}

/// One call's context: no flush in flight, a best block, nothing else due.
struct Call {
    now: Instant,
    boundary: CoinsBoundary,
    evo_bytes: u64,
    in_flight: bool,
    has_best_block: bool,
    clean_at_durable: bool,
    manual: Option<u32>,
    reindex: bool,
    left_ibd: bool,
    dmax_due: bool,
}

impl Call {
    fn at(now: Instant) -> Self {
        Self {
            now,
            boundary: CoinsBoundary::Ok,
            evo_bytes: 0,
            in_flight: false,
            has_best_block: true,
            clean_at_durable: false,
            manual: None,
            reindex: false,
            left_ibd: false,
            dmax_due: false,
        }
    }

    fn decide(
        &self,
        policy: &mut FlushPolicy,
        mode: FlushStateMode,
        host: &mut Host,
    ) -> FlushAction {
        let mut ctx = PolicyCtx {
            now: self.now,
            boundary: self.boundary,
            evo_bytes: self.evo_bytes,
            in_flight: self.in_flight,
            has_best_block: self.has_best_block,
            clean_at_durable: self.clean_at_durable,
            prune: host,
            chain_height: 1000,
            manual_prune_height: self.manual,
            reindex: self.reindex,
            is_ibd: false,
            left_ibd: self.left_ibd,
            dmax_due: self.dmax_due,
            prune_locks: &Vec::new,
            scripted: false,
        };
        policy.decide(mode, &mut ctx)
    }
}

fn freeze(causes: FlushCauses, wait_done: bool) -> FlushAction {
    FlushAction::Freeze {
        causes,
        prune: None,
        wait_done,
    }
}

const MODES: [FlushStateMode; 4] = [NoneMode, IfNeeded, Periodic, Always];
const BOUNDARIES: [CoinsBoundary; 4] =
    [CoinsBoundary::Ok, CoinsBoundary::Threshold, CoinsBoundary::Large, CoinsBoundary::Critical];

/// The coin side of every mode, with no flush in flight (§3.3: `fCacheLarge` = PERIODIC ∧ LARGE or
/// worse, `fCacheCritical` = IF_NEEDED ∧ CRITICAL, THRESHOLD in either, NONE checks nothing).
#[test]
fn the_coin_side_maps_onto_cores_modes() {
    use CoinsBoundary as B;
    use FlushCauses as C;
    let now = Instant::now();
    for mode in MODES {
        for boundary in BOUNDARIES {
            let expected = match (mode, boundary) {
                (Always, _) => freeze(C::ALWAYS, true),
                (IfNeeded, B::Critical) => freeze(C::CACHE_CRITICAL, false),
                (Periodic, B::Large | B::Critical) => freeze(C::CACHE_LARGE, false),
                (IfNeeded | Periodic, B::Threshold) => freeze(C::THRESHOLD, false),
                _ => FlushAction::Nothing,
            };
            let mut call = Call::at(now);
            call.boundary = boundary;
            let got = call.decide(&mut FlushPolicy::new(false), mode, &mut Host::default());
            assert_eq!(got, expected, "{mode:?} at {boundary:?}");
        }
    }
}

/// With F in flight: ALWAYS and `fCacheCritical` wait for the release, then decide again; LARGE is
/// pending until the release (§3.3's last column).
#[test]
fn a_flush_in_flight_waits_or_keeps_the_cause() {
    use CoinsBoundary as B;
    use FlushCauses as C;
    let now = Instant::now();
    for mode in MODES {
        for boundary in BOUNDARIES {
            let expected = match (mode, boundary) {
                (Always, _) => FlushAction::WaitThenFreeze {
                    causes: C::ALWAYS,
                },
                (IfNeeded, B::Critical) => FlushAction::WaitThenFreeze {
                    causes: C::CACHE_CRITICAL,
                },
                (Periodic, B::Large | B::Critical) => FlushAction::Pending {
                    causes: C::CACHE_LARGE,
                },
                (IfNeeded | Periodic, B::Threshold) => FlushAction::Pending {
                    causes: C::THRESHOLD,
                },
                _ => FlushAction::Nothing,
            };
            let mut call = Call::at(now);
            call.boundary = boundary;
            call.in_flight = true;
            let got = call.decide(&mut FlushPolicy::new(false), mode, &mut Host::default());
            assert_eq!(got, expected, "{mode:?} at {boundary:?} with F in flight");
        }
    }
}

/// A kept cause freezes at the first decision after the release, whatever that call's mode and
/// boundary; ALWAYS is not kept, because the waiting call decides again with its own mode.
#[test]
fn kept_causes_freeze_after_the_release() {
    let now = Instant::now();
    let mut host = Host::default();
    let mut policy = FlushPolicy::new(false);
    let mut call = Call::at(now);
    call.in_flight = true;
    call.boundary = CoinsBoundary::Large;
    call.decide(&mut policy, Periodic, &mut host);
    call.boundary = CoinsBoundary::Critical;
    call.decide(&mut policy, IfNeeded, &mut host);
    call.boundary = CoinsBoundary::Ok;
    call.decide(&mut policy, Always, &mut host);
    assert_eq!(policy.pending(), FlushCauses::CACHE_LARGE | FlushCauses::CACHE_CRITICAL);

    call.in_flight = false;
    assert_eq!(
        call.decide(&mut policy, NoneMode, &mut host),
        freeze(FlushCauses::CACHE_LARGE | FlushCauses::CACHE_CRITICAL, false)
    );
    assert_eq!(policy.pending(), FlushCauses::empty());
    assert_eq!(call.decide(&mut policy, NoneMode, &mut host), FlushAction::Nothing);
}

/// `fEvoDbCacheCritical` = IF_NEEDED ∧ EvoDB memory ≥ 64 MiB.
#[test]
fn evo_critical_at_64_mib_in_if_needed_only() {
    let now = Instant::now();
    for (mode, bytes, expected) in [
        (IfNeeded, EVO_CRITICAL_BYTES - 1, FlushAction::Nothing),
        (IfNeeded, EVO_CRITICAL_BYTES, freeze(FlushCauses::EVO_CRITICAL, false)),
        (Periodic, EVO_CRITICAL_BYTES, FlushAction::Nothing),
        (NoneMode, EVO_CRITICAL_BYTES, FlushAction::Nothing),
    ] {
        let mut call = Call::at(now);
        call.evo_bytes = bytes;
        assert_eq!(call.decide(&mut FlushPolicy::new(false), mode, &mut Host::default()), expected);
        call.in_flight = true;
        if mode == IfNeeded && bytes == EVO_CRITICAL_BYTES {
            assert_eq!(
                call.decide(&mut FlushPolicy::new(false), mode, &mut Host::default()),
                FlushAction::WaitThenFreeze {
                    causes: FlushCauses::EVO_CRITICAL
                }
            );
        }
    }
}

/// Core's timers: set on the first call (no write right after startup), strictly greater than the
/// interval, `m_last_write` at the write's return, `m_last_flush` at the freeze; PERIODIC only.
#[test]
fn the_periodic_write_and_flush_timers() {
    let t0 = Instant::now();
    let ns = Duration::from_nanos(1);
    let mut host = Host::default();
    let mut policy = FlushPolicy::new(false);
    let decide = |policy: &mut FlushPolicy, host: &mut Host, mode, at| {
        Call::at(at).decide(policy, mode, host)
    };
    assert_eq!(decide(&mut policy, &mut host, Periodic, t0), FlushAction::Nothing);
    let t1 = t0 + DATABASE_WRITE_INTERVAL;
    assert_eq!(decide(&mut policy, &mut host, Periodic, t1), FlushAction::Nothing);
    assert_eq!(decide(&mut policy, &mut host, IfNeeded, t1 + ns), FlushAction::Nothing);
    let index_only = FlushAction::IndexOnly {
        signal: false,
    };
    assert_eq!(decide(&mut policy, &mut host, Periodic, t1 + ns), index_only);
    // Not written yet: due again (V counts the write in flight until W releases it); written as
    // of t1 + ns, it is not.
    assert_eq!(decide(&mut policy, &mut host, Periodic, t1 + ns), index_only);
    policy.written(t1 + ns);
    assert_eq!(decide(&mut policy, &mut host, Periodic, t1 + ns + ns), FlushAction::Nothing);

    let t2 = t0 + DATABASE_FLUSH_INTERVAL;
    assert_eq!(decide(&mut policy, &mut host, Periodic, t2), index_only);
    policy.written(t2);
    assert_eq!(
        decide(&mut policy, &mut host, Periodic, t2 + ns),
        freeze(FlushCauses::PERIODIC_FLUSH, false)
    );
    // The freeze reset `m_last_flush`.
    assert_eq!(decide(&mut policy, &mut host, Periodic, t2 + ns + ns), FlushAction::Nothing);
}

/// Core flushes the coins only at a best block (`validation.cpp:3078`): with none, a full flush
/// writes the block index alone, signals nothing (`full_flush_completed` is set inside that
/// check, `:3102`) and leaves `m_last_flush` (`:3101`). An ALWAYS flush of a clean cache does not take
/// the shortcut that signals.
#[test]
fn no_best_block_writes_the_index_alone() {
    let t0 = Instant::now();
    let mut call = Call::at(t0);
    call.has_best_block = false;
    call.clean_at_durable = true;
    let index_only = FlushAction::IndexOnly {
        signal: false,
    };
    assert_eq!(call.decide(&mut FlushPolicy::new(false), Always, &mut Host::default()), index_only);
    call.boundary = CoinsBoundary::Critical;
    assert_eq!(
        call.decide(&mut FlushPolicy::new(false), IfNeeded, &mut Host::default()),
        index_only
    );

    // `m_last_flush` is still the startup time's, so the periodic flush is due a day after it.
    let mut policy = FlushPolicy::new(false);
    let mut host = Host::default();
    call.boundary = CoinsBoundary::Ok;
    let half = t0 + DATABASE_FLUSH_INTERVAL / 2;
    for at in [t0, half] {
        call.now = at;
        call.decide(&mut policy, Always, &mut host);
    }
    let mut late = Call::at(t0 + DATABASE_FLUSH_INTERVAL + Duration::from_nanos(1));
    late.has_best_block = true;
    assert!(matches!(
        late.decide(&mut policy, Periodic, &mut host),
        FlushAction::Freeze { causes, .. } if causes.contains(FlushCauses::PERIODIC_FLUSH)
    ));
}

/// CH2 §4.2's ALWAYS shortcut: clean active at the durable tip is an index-only write followed by
/// `ChainStateFlushed`, unless another cause is due (a pending `FlushDue` among them). It is a full
/// flush of an empty cache, so it resets `m_last_flush` as Core's does (`validation.cpp:3101`).
#[test]
fn always_with_nothing_dirty_is_index_only_and_signals() {
    let t0 = Instant::now();
    let mut call = Call::at(t0);
    call.clean_at_durable = true;
    let mut policy = FlushPolicy::new(false);
    let mut host = Host::default();
    assert_eq!(call.decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
    call.now = t0 + DATABASE_FLUSH_INTERVAL;
    assert_eq!(
        call.decide(&mut policy, Always, &mut host),
        FlushAction::IndexOnly {
            signal: true
        }
    );
    call.now += Duration::from_nanos(1);
    policy.written(call.now);
    assert_eq!(call.decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
    call.now = t0;
    call.dmax_due = true;
    assert_eq!(
        call.decide(&mut FlushPolicy::new(false), Always, &mut Host::default()),
        freeze(FlushCauses::ALWAYS | FlushCauses::DMAX, true)
    );
}

/// D14: under `-dbcrashratio` every freeze is waited for.
#[test]
fn dbcrashratio_waits_for_every_freeze() {
    let mut call = Call::at(Instant::now());
    call.boundary = CoinsBoundary::Threshold;
    assert_eq!(
        call.decide(&mut FlushPolicy::new(true), IfNeeded, &mut Host::default()),
        freeze(FlushCauses::THRESHOLD, true)
    );
}

/// LEFT_IBD freezes once; a call that cannot freeze keeps it.
#[test]
fn left_ibd_freezes_once() {
    let mut host = Host::default();
    let mut policy = FlushPolicy::new(false);
    let mut call = Call::at(Instant::now());
    call.left_ibd = true;
    call.has_best_block = false;
    assert!(matches!(call.decide(&mut policy, NoneMode, &mut host), FlushAction::IndexOnly { .. }));
    call.has_best_block = true;
    assert_eq!(call.decide(&mut policy, NoneMode, &mut host), freeze(FlushCauses::LEFT_IBD, false));
    assert_eq!(call.decide(&mut policy, NoneMode, &mut host), FlushAction::Nothing);
}

/// Step 0 (CH2 §3.3's prune row, §4.2): an automatic check is taken only when V can decide; a
/// selection freezes with D; an empty D suppresses no other cause; a manual prune leaves the
/// automatic check armed, waits for a flush in flight and is waited for; no pruning under reindex;
/// a manual height of 0 is none.
#[test]
fn step_zero_prune_decisions() {
    let now = Instant::now();
    let prune = |files: Vec<u32>, manual| PruneDecision {
        files,
        met_target: true,
        manual,
        tip_height: 1000,
        target: 7,
        prune_after_height: 0,
    };

    // Automatic, with files: one dry run, then the check is spent.
    let mut host = Host {
        armed: true,
        files: vec![3, 4],
        ..Host::default()
    };
    let mut policy = FlushPolicy::new(false);
    let call = Call::at(now);
    assert_eq!(
        call.decide(&mut policy, NoneMode, &mut host),
        FlushAction::Freeze {
            causes: FlushCauses::PRUNE,
            prune: Some(prune(vec![3, 4], None)),
            wait_done: false
        }
    );
    assert_eq!(call.decide(&mut policy, NoneMode, &mut host), FlushAction::Nothing);

    // With F in flight the check is not taken, so it stays armed for the call after the release.
    host.armed = true;
    let checks = host.checks;
    let mut flying = Call::at(now);
    flying.in_flight = true;
    assert_eq!(flying.decide(&mut policy, NoneMode, &mut host), FlushAction::Nothing);
    assert_eq!((host.checks, host.armed), (checks, true));

    // An empty D never suppresses another cause, and the decision travels with the freeze.
    host.files.clear();
    let mut critical = Call::at(now);
    critical.boundary = CoinsBoundary::Critical;
    assert_eq!(
        critical.decide(&mut policy, IfNeeded, &mut host),
        FlushAction::Freeze {
            causes: FlushCauses::CACHE_CRITICAL,
            prune: Some(prune(vec![], None)),
            wait_done: false
        }
    );

    // Manual: waits for a flush in flight; leaves the automatic check alone; is waited for; with
    // an empty D and nothing else due, nothing happens (the RPC replies at once).
    host.armed = true;
    host.files = vec![7];
    let checks = host.checks;
    let mut manual = Call::at(now);
    manual.manual = Some(500);
    manual.in_flight = true;
    assert_eq!(
        manual.decide(&mut policy, NoneMode, &mut host),
        FlushAction::WaitThenFreeze {
            causes: FlushCauses::empty()
        }
    );
    manual.in_flight = false;
    assert_eq!(
        manual.decide(&mut policy, NoneMode, &mut host),
        FlushAction::Freeze {
            causes: FlushCauses::PRUNE,
            prune: Some(prune(vec![7], Some(500))),
            wait_done: true
        }
    );
    assert_eq!((host.checks, host.armed), (checks, true));
    // Core's `FindFilesToPruneManual(min(last_prune, manual), tip)`: the tip and the bound are the
    // chain's height until b2 lowers the bound to the prune locks.
    assert_eq!(host.runs.borrow().last(), Some(&(1000, 1000, Some(500), false)));
    host.files.clear();
    assert_eq!(manual.decide(&mut policy, NoneMode, &mut host), FlushAction::Nothing);

    // `fReindex`: no dry run at all, armed or manual.
    host.files = vec![9];
    let runs = host.runs.borrow().len();
    let mut reindex = Call::at(now);
    reindex.reindex = true;
    reindex.manual = Some(500);
    assert_eq!(reindex.decide(&mut policy, NoneMode, &mut host), FlushAction::Nothing);
    reindex.manual = None;
    assert_eq!(reindex.decide(&mut policy, NoneMode, &mut host), FlushAction::Nothing);
    assert!(host.armed);
    assert_eq!(host.runs.borrow().len(), runs);

    // A manual height of 0 is none (`nManualPruneHeight > 0`): no dry run and no wait.
    host.armed = false;
    let mut zero = Call::at(now);
    zero.manual = Some(0);
    assert_eq!(zero.decide(&mut policy, NoneMode, &mut host), FlushAction::Nothing);
    zero.in_flight = true;
    assert_eq!(zero.decide(&mut policy, NoneMode, &mut host), FlushAction::Nothing);
    assert_eq!(host.runs.borrow().len(), runs);
}
