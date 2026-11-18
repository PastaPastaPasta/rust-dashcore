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
/// `no_tip`: its check is taken and selects nothing, and its peek selects nothing, as
/// `NoTipPrune`'s.
#[derive(Default)]
struct Host {
    armed: bool,
    files: Vec<u32>,
    checks: u32,
    runs: RefCell<Vec<DryRun>>,
    no_tip: bool,
    /// The peeks made: the tip, the bound and whether in IBD.
    peeks: RefCell<Vec<(u32, u32, bool)>>,
    /// The peek's answer, if not the dry run's emptiness.
    peek: Option<bool>,
    /// A peek is a failed assertion.
    peek_panics: bool,
}

impl PruneHost for Host {
    fn check_due(&mut self) -> bool {
        self.checks += 1;
        std::mem::take(&mut self.armed) && !self.no_tip
    }

    fn check_armed(&self) -> bool {
        self.armed
    }

    fn would_select(&self, tip: u32, bound: u32, ibd: bool) -> bool {
        assert!(!self.peek_panics, "no peek here");
        self.peeks.borrow_mut().push((tip, bound, ibd));
        !self.no_tip && self.peek.unwrap_or(!self.files.is_empty())
    }

    fn arm(&mut self) {
        self.armed = true;
    }

    fn dry_run(&self, tip: u32, bound: u32, manual: Option<u32>, ibd: bool) -> PruneDryRun {
        self.runs.borrow_mut().push((tip, bound, manual, ibd));
        PruneDryRun {
            files: self.files.clone(),
            met_target: true,
            target: 7,
            prune_after_height: 0,
            // Mainnet's retention, which the decision carries for step 7 (CE5-05).
            min_blocks_to_keep: 2304,
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
    scripted: bool,
    core_call: bool,
    /// The predicted locks the call reads.
    locks: Vec<(&'static str, u32)>,
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
            scripted: false,
            core_call: true,
            locks: Vec::new(),
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
            prune_locks: &|| self.locks.clone(),
            scripted: self.scripted,
            core_call: self.core_call,
        };
        policy.decide(mode, &mut ctx)
    }
}

fn freeze(causes: FlushCauses, wait_done: bool) -> FlushAction {
    FlushAction::Freeze {
        causes,
        prune: None,
        wait_done,
        write_due: false,
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
/// interval, `m_last_write` and `m_last_flush` at the call that writes (B-1); PERIODIC only.
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
        causes: FlushCauses::empty(),
        write_due: true,
    };
    assert_eq!(decide(&mut policy, &mut host, Periodic, t1 + ns), index_only);
    // Written as of its decision, as Core's `m_last_write = nNow` (validation.cpp:3086 at
    // v24.0.0), with the job still in flight; its release moves nothing (D243).
    assert_eq!(decide(&mut policy, &mut host, Periodic, t1 + ns + ns), FlushAction::Nothing);

    let t2 = t0 + DATABASE_FLUSH_INTERVAL;
    assert_eq!(decide(&mut policy, &mut host, Periodic, t2), index_only);
    assert_eq!(
        decide(&mut policy, &mut host, Periodic, t2 + ns),
        freeze(FlushCauses::PERIODIC_FLUSH, false)
    );
    // The freeze reset `m_last_flush`.
    assert_eq!(decide(&mut policy, &mut host, Periodic, t2 + ns + ns), FlushAction::Nothing);
}

/// Core flushes the coins only at a best block (`validation.cpp:3089` at v24.0.0): with none, a
/// full flush writes the block index alone, signals nothing (`full_flush_completed` is set inside
/// that check, `:3114`) and leaves `m_last_flush` (`:3113`). An ALWAYS flush of a clean cache does
/// not take the shortcut that signals.
#[test]
fn no_best_block_writes_the_index_alone() {
    let t0 = Instant::now();
    let mut call = Call::at(t0);
    call.has_best_block = false;
    call.clean_at_durable = true;
    let index_only = |causes| FlushAction::IndexOnly {
        signal: false,
        causes,
        write_due: false,
    };
    assert_eq!(
        call.decide(&mut FlushPolicy::new(false), Always, &mut Host::default()),
        index_only(FlushCauses::ALWAYS)
    );
    call.boundary = CoinsBoundary::Critical;
    assert_eq!(
        call.decide(&mut FlushPolicy::new(false), IfNeeded, &mut Host::default()),
        index_only(FlushCauses::CACHE_CRITICAL)
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
/// flush of an empty cache, so it resets `m_last_flush` as Core's does (`validation.cpp:3113` at
/// v24.0.0).
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
            signal: true,
            causes: FlushCauses::ALWAYS,
            write_due: false,
        }
    );
    call.now += Duration::from_nanos(1);
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
        min_blocks_to_keep: 2304,
    };

    // Automatic, with files: one dry run, then the check is spent. The prune is waited for
    // (CH3b D244).
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
            wait_done: true,
            write_due: false,
        }
    );
    assert_eq!(call.decide(&mut policy, NoneMode, &mut host), FlushAction::Nothing);

    // With F in flight the peek decides at this call (CH3b D244): a peek that selects waits for
    // the release with the check still armed; an empty one takes the check there.
    host.armed = true;
    let checks = host.checks;
    let mut flying = Call::at(now);
    flying.in_flight = true;
    assert_eq!(
        flying.decide(&mut policy, NoneMode, &mut host),
        FlushAction::WaitThenFreeze {
            causes: FlushCauses::empty()
        }
    );
    assert_eq!((host.checks, host.armed), (checks, true));
    let files = std::mem::take(&mut host.files);
    assert_eq!(flying.decide(&mut policy, NoneMode, &mut host), FlushAction::Nothing);
    assert_eq!((host.checks, host.armed), (checks + 1, false), "taken at the call");
    (host.files, host.armed) = (files, true);

    // An empty D never suppresses another cause, and the decision travels with the freeze.
    host.files.clear();
    let mut critical = Call::at(now);
    critical.boundary = CoinsBoundary::Critical;
    assert_eq!(
        critical.decide(&mut policy, IfNeeded, &mut host),
        FlushAction::Freeze {
            causes: FlushCauses::CACHE_CRITICAL,
            prune: Some(prune(vec![], None)),
            wait_done: false,
            write_due: false,
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
            wait_done: true,
            write_due: false,
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

/// CH3a a10 M-3 and m-4: `CoreCausesOnly` removes LEFT_IBD before it keeps or freezes anything,
/// counts it once and consumes it, as Production's freeze would: later sites see it done, and a
/// flush in flight keeps nothing for it. The timers are Core's: the first PERIODIC flush is still
/// due a day after the first call.
#[cfg(feature = "test-support")]
#[test]
fn core_causes_only_suppresses_left_ibd_once() {
    let t0 = Instant::now();
    let mut host = Host::default();
    let mut policy = FlushPolicy::with_regime(false, FlushRegime::CoreCausesOnly);
    let mut call = Call::at(t0);
    call.left_ibd = true;
    call.in_flight = true;
    assert_eq!(call.decide(&mut policy, IfNeeded, &mut host), FlushAction::Nothing);
    assert!(policy.pending().is_empty(), "nothing kept for the job in flight");
    call.in_flight = false;
    for mode in [NoneMode, IfNeeded, Periodic] {
        assert_eq!(call.decide(&mut policy, mode, &mut host), FlushAction::Nothing, "{mode:?}");
    }
    assert_eq!(
        policy.suppressed(),
        Suppressed {
            left_ibd: 1,
            ..Suppressed::default()
        }
    );
    call.now = t0 + DATABASE_FLUSH_INTERVAL + Duration::from_nanos(1);
    let want = FlushAction::Freeze {
        causes: FlushCauses::PERIODIC_FLUSH,
        prune: None,
        wait_done: false,
        // A day is past `DATABASE_WRITE_INTERVAL` too (CH3b D242).
        write_due: true,
    };
    assert_eq!(
        call.decide(&mut policy, Periodic, &mut host),
        want,
        "no suppression moved m_last_flush"
    );
}

/// m-4: ALWAYS at the site where the node leaves IBD flushes with ALWAYS alone and consumes
/// LEFT_IBD; a reopen's new policy arms it again.
#[cfg(feature = "test-support")]
#[test]
fn core_causes_only_always_at_the_transition_flushes_always_alone() {
    let mut host = Host::default();
    let mut call = Call::at(Instant::now());
    call.left_ibd = true;
    for _open in 0..2 {
        let mut policy = FlushPolicy::with_regime(false, FlushRegime::CoreCausesOnly);
        assert_eq!(call.decide(&mut policy, Always, &mut host), freeze(FlushCauses::ALWAYS, true));
        assert_eq!(call.decide(&mut policy, NoneMode, &mut host), FlushAction::Nothing);
        assert_eq!(policy.suppressed().left_ibd, 1);
    }
}

/// M-3: THRESHOLD and DMAX are removed and counted at each decision that holds them, and Core's
/// own causes beside them still freeze; Production keeps them.
#[cfg(feature = "test-support")]
#[test]
fn core_causes_only_masks_threshold_and_dmax() {
    let mut host = Host::default();
    let mut call = Call::at(Instant::now());
    call.boundary = CoinsBoundary::Threshold;
    call.dmax_due = true;
    let mut policy = FlushPolicy::with_regime(false, FlushRegime::CoreCausesOnly);
    assert_eq!(call.decide(&mut policy, IfNeeded, &mut host), FlushAction::Nothing);
    assert_eq!(call.decide(&mut policy, NoneMode, &mut host), FlushAction::Nothing);
    assert_eq!(call.decide(&mut policy, Always, &mut host), freeze(FlushCauses::ALWAYS, true));
    assert_eq!(
        policy.suppressed(),
        Suppressed {
            threshold: 1,
            dmax: 3,
            ..Suppressed::default()
        }
    );
    let mut production = FlushPolicy::with_regime(false, FlushRegime::Production);
    assert_eq!(
        call.decide(&mut production, IfNeeded, &mut host),
        freeze(FlushCauses::THRESHOLD | FlushCauses::DMAX, false)
    );
    assert_eq!(production.suppressed(), Suppressed::default());
}

/// `SCRIPTED` never reaches a `CoreCausesOnly` policy: its presence is a failed assertion.
#[cfg(feature = "test-support")]
#[test]
#[should_panic(expected = "SCRIPTED under CoreCausesOnly")]
fn core_causes_only_refuses_scripted() {
    let mut call = Call::at(Instant::now());
    call.scripted = true;
    let mut policy = FlushPolicy::with_regime(false, FlushRegime::CoreCausesOnly);
    call.decide(&mut policy, NoneMode, &mut Host::default());
}

/// CH3b D242's classifier (DEC-1500, DEC-1517): a take keeps Core's dirt unless its causes
/// include ALWAYS. PRUNE, PERIODIC_FLUSH, a due periodic write and the native-measure causes keep
/// (DEC-1451 Q2); a decision that takes nothing keeps nothing.
#[test]
fn the_cores_take_point_is_always() {
    use FlushCauses as C;
    assert_eq!(C::CORE_POINT, C::ALWAYS);
    let single = C::all().iter().map(|c| (c, !C::CORE_POINT.contains(c)));
    let mixed = [
        (C::THRESHOLD | C::ALWAYS, false),
        (C::PRUNE | C::ALWAYS, false),
        (C::PERIODIC_FLUSH | C::PRUNE, true),
    ];
    for (causes, keeps) in single.chain(mixed) {
        for write_due in [false, true] {
            let index_only = FlushAction::IndexOnly {
                signal: false,
                causes,
                write_due,
            };
            let frozen = FlushAction::Freeze {
                causes,
                prune: None,
                wait_done: false,
                write_due,
            };
            for action in [index_only, frozen] {
                assert_eq!(action.keeps_cores_dirt(), keeps, "{action:?}");
            }
        }
    }
    let periodic_write = FlushAction::IndexOnly {
        signal: false,
        causes: C::empty(),
        write_due: true,
    };
    assert!(periodic_write.keeps_cores_dirt(), "the periodic write alone");
    for action in [
        FlushAction::Nothing,
        FlushAction::Pending {
            causes: C::THRESHOLD,
        },
        FlushAction::WaitThenFreeze {
            causes: C::CACHE_CRITICAL,
        },
    ] {
        assert!(!action.keeps_cores_dirt(), "{action:?} takes nothing");
    }
}

/// B-1 (CH3b D242): Core's `fPeriodicWrite` writes the index beside any cause at a PERIODIC call
/// past `DATABASE_WRITE_INTERVAL` (`validation.cpp:3053`, `:3059-3087`), so such a call writes as
/// Core's whatever else caused it, with no best block too; before the interval it is not due.
/// The take keeps (DEC-1500).
#[test]
fn a_due_periodic_write_is_carried_beside_any_cause() {
    let t0 = Instant::now();
    let late = t0 + DATABASE_WRITE_INTERVAL + Duration::from_nanos(1);
    let early = t0 + DATABASE_WRITE_INTERVAL;
    type Set = fn(&mut Call);
    let cases: [(FlushCauses, Set); 4] = [
        (FlushCauses::DMAX, |c| c.dmax_due = true),
        (FlushCauses::LEFT_IBD, |c| c.left_ibd = true),
        (FlushCauses::THRESHOLD, |c| c.boundary = CoinsBoundary::Threshold),
        (FlushCauses::CACHE_LARGE, |c| c.boundary = CoinsBoundary::Large),
    ];
    for (cause, set) in cases {
        for (at, write_due) in [(late, true), (early, false)] {
            let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
            assert_eq!(Call::at(t0).decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
            let mut call = Call::at(at);
            set(&mut call);
            let want = FlushAction::Freeze {
                causes: cause,
                prune: None,
                wait_done: false,
                write_due,
            };
            assert_eq!(call.decide(&mut policy, Periodic, &mut host), want, "{cause:?} {at:?}");
        }
    }
    for (at, write_due) in [(late, true), (early, false)] {
        let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
        assert_eq!(Call::at(t0).decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
        let mut call = Call::at(at);
        call.has_best_block = false;
        call.boundary = CoinsBoundary::Large;
        let want = FlushAction::IndexOnly {
            signal: false,
            causes: FlushCauses::CACHE_LARGE,
            write_due,
        };
        assert_eq!(call.decide(&mut policy, Periodic, &mut host), want, "no best block {at:?}");
    }
}

/// B-1 in flight (CH3b D242, D8): a periodic write that comes due while a job is in flight is
/// kept, and the first decision with nothing in flight takes it as Core's: the index alone if
/// nothing else is due, else beside the causes.
#[test]
fn a_periodic_write_due_in_flight_is_taken_after_the_release() {
    let t0 = Instant::now();
    let t1 = t0 + DATABASE_WRITE_INTERVAL + Duration::from_nanos(1);
    for dmax in [false, true] {
        let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
        assert_eq!(Call::at(t0).decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
        let mut flying = Call::at(t1);
        flying.in_flight = true;
        assert_eq!(flying.decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
        assert!(policy.write_pending(), "kept for the decision after the release");
        let mut after = Call::at(t1);
        after.dmax_due = dmax;
        let want = if dmax {
            FlushAction::Freeze {
                causes: FlushCauses::DMAX,
                prune: None,
                wait_done: false,
                write_due: true,
            }
        } else {
            FlushAction::IndexOnly {
                signal: false,
                causes: FlushCauses::empty(),
                write_due: true,
            }
        };
        assert_eq!(after.decide(&mut policy, NoneMode, &mut host), want, "dmax {dmax}");
        assert!(!policy.write_pending(), "taken once");
    }
}

/// B-3 (CH3b D242): the carried write is consumed by the first decision with nothing in flight.
/// The in-flight call recorded Core's write, and no release moves it (D243), so the next
/// PERIODIC call takes only the carried write. The next cause-free decision takes nothing.
#[test]
fn a_carried_write_is_taken_once() {
    let t0 = Instant::now();
    let ns = Duration::from_nanos(1);
    let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
    assert_eq!(Call::at(t0).decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
    let mut flying = Call::at(t0 + DATABASE_WRITE_INTERVAL + ns);
    flying.in_flight = true;
    assert_eq!(flying.decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
    assert!(policy.write_pending());
    let t2 = t0 + DATABASE_WRITE_INTERVAL + ns + ns;
    let index_only = FlushAction::IndexOnly {
        signal: false,
        causes: FlushCauses::empty(),
        write_due: true,
    };
    assert_eq!(Call::at(t2).decide(&mut policy, Periodic, &mut host), index_only);
    assert!(!policy.write_pending(), "consumed by the decision that wrote");
    assert_eq!(Call::at(t2 + ns).decide(&mut policy, NoneMode, &mut host), FlushAction::Nothing);
}

/// D8: a write carried from a call made in flight is consumed by the first decision after the
/// release even when a periodic write is due there too, so a NONE call after it carries nothing.
#[test]
fn a_carried_write_is_consumed_beside_a_due_one() {
    let t0 = Instant::now();
    let ns = Duration::from_nanos(1);
    let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
    assert_eq!(Call::at(t0).decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
    let t1 = t0 + DATABASE_WRITE_INTERVAL + ns;
    let mut flying = Call::at(t1);
    flying.in_flight = true;
    assert_eq!(flying.decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
    assert!(policy.write_pending());
    let t2 = t1 + DATABASE_WRITE_INTERVAL + ns;
    let index_only = FlushAction::IndexOnly {
        signal: false,
        causes: FlushCauses::empty(),
        write_due: true,
    };
    assert_eq!(Call::at(t2).decide(&mut policy, Periodic, &mut host), index_only);
    assert!(!policy.write_pending(), "consumed beside the due write");
    assert_eq!(Call::at(t2 + ns).decide(&mut policy, NoneMode, &mut host), FlushAction::Nothing);
}

/// B-1 (CH3b D242): a call that writes as Core's records the write at its decision, as Core sets
/// `m_last_write = nNow` in that call (`validation.cpp:3086` at v24.0.0). With that job still in
/// flight, a later PERIODIC call within the interval is not due and carries nothing: the index
/// alone, a freeze beside it, PERIODIC_FLUSH's and with no best block.
#[test]
fn a_core_write_in_flight_is_not_due_again() {
    let t0 = Instant::now();
    let ns = Duration::from_nanos(1);
    let hour = t0 + DATABASE_WRITE_INTERVAL + ns;
    let day = t0 + DATABASE_FLUSH_INTERVAL + ns;
    type Set = fn(&mut Call);
    let cases: [(&str, Instant, Set); 4] = [
        ("the index alone", hour, |_| {}),
        ("DMAX beside it", hour, |c| c.dmax_due = true),
        ("PERIODIC_FLUSH", day, |_| {}),
        ("no best block", hour, |c| c.has_best_block = false),
    ];
    for (row, at, set) in cases {
        let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
        assert_eq!(Call::at(t0).decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
        let mut call = Call::at(at);
        set(&mut call);
        let action = call.decide(&mut policy, Periodic, &mut host);
        assert!(
            matches!(
                action,
                FlushAction::IndexOnly {
                    write_due: true,
                    ..
                } | FlushAction::Freeze {
                    write_due: true,
                    ..
                }
            ),
            "{row}: {action:?}"
        );
        for later in [at + ns, at + ns + ns] {
            let mut flying = Call::at(later);
            flying.in_flight = true;
            flying.has_best_block = call.has_best_block;
            let action = flying.decide(&mut policy, Periodic, &mut host);
            assert_eq!(action, FlushAction::Nothing, "{row}: in flight");
            assert!(!policy.write_pending(), "{row}: nothing carried");
        }
        let after = Call::at(at + ns + ns);
        assert_eq!(after.decide(&mut policy, NoneMode, &mut host), FlushAction::Nothing, "{row}");
    }
}

/// DEC-1517: every PRUNE take keeps Core's dirt, whatever armed its check. A re-arm stands in
/// for DEC-211's decline (or T40's), which Core never makes: V's next selection can be of files
/// Core already pruned, at a call where Core selects nothing and writes nothing. A manual prune
/// keeps too; ALWAYS beside a PRUNE is Core's. The policy sees only an armed check, whatever
/// armed it; the second iteration guards against a split by arming source. The row asserts V's
/// side only: V keeps whatever stock Core selected, so it needs no model of Core's files.
#[test]
fn every_prune_take_keeps_cores_dirt() {
    let t0 = Instant::now();
    let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
    host.files = vec![0];
    let prune = |action: &FlushAction| {
        matches!(action, FlushAction::Freeze { causes, prune: Some(p), .. }
            if causes.contains(FlushCauses::PRUNE) && p.files == [0])
    };
    for row in ["armed", "re-armed after a decline"] {
        host.armed = true;
        let action = Call::at(t0).decide(&mut policy, NoneMode, &mut host);
        assert!(prune(&action), "{row}: {action:?}");
        assert!(action.keeps_cores_dirt(), "{row}: keeps");
        assert!(
            matches!(
                action,
                FlushAction::Freeze {
                    wait_done: true,
                    ..
                }
            ),
            "{row}: waited"
        );
    }
    let mut manual = Call::at(t0);
    manual.manual = Some(500);
    let action = manual.decide(&mut policy, NoneMode, &mut host);
    assert!(prune(&action), "manual: {action:?}");
    assert!(action.keeps_cores_dirt(), "manual: keeps");
    host.armed = true;
    let action = Call::at(t0).decide(&mut policy, Always, &mut host);
    assert!(prune(&action), "ALWAYS: {action:?}");
    assert!(!action.keeps_cores_dirt(), "ALWAYS beside it is Core's");
}

/// B-1: a PERIODIC_FLUSH freeze is Core's write at its call (`fDoFullFlush`, `v:3086`) when the
/// periodic write is not due: an hour after an earlier write, the next PERIODIC call writes
/// nothing.
#[test]
fn a_periodic_flush_records_cores_write() {
    let t0 = Instant::now();
    let ns = Duration::from_nanos(1);
    let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
    assert_eq!(Call::at(t0).decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
    let tw = t0 + DATABASE_FLUSH_INTERVAL - DATABASE_WRITE_INTERVAL / 2;
    let index_only = FlushAction::IndexOnly {
        signal: false,
        causes: FlushCauses::empty(),
        write_due: true,
    };
    assert_eq!(Call::at(tw).decide(&mut policy, Periodic, &mut host), index_only);
    let tf = t0 + DATABASE_FLUSH_INTERVAL + ns;
    assert_eq!(
        Call::at(tf).decide(&mut policy, Periodic, &mut host),
        freeze(FlushCauses::PERIODIC_FLUSH, false),
        "the flush alone"
    );
    let after = Call::at(tw + DATABASE_WRITE_INTERVAL + ns);
    assert_eq!(after.decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
}

/// B-1: Core's write at a call made while a node-own job is in flight outlasts that job: its
/// release moves no timer (D243), so a PERIODIC call an hour after the job's decision is not due.
#[test]
fn a_core_write_in_flight_outlasts_the_job_before_it() {
    let t0 = Instant::now();
    let ns = Duration::from_nanos(1);
    let half = DATABASE_WRITE_INTERVAL / 2;
    let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
    assert_eq!(Call::at(t0).decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
    let mut own = Call::at(t0 + half);
    own.dmax_due = true;
    assert_eq!(own.decide(&mut policy, NoneMode, &mut host), freeze(FlushCauses::DMAX, false));
    let t1 = t0 + DATABASE_WRITE_INTERVAL + ns;
    let mut flying = Call::at(t1);
    flying.in_flight = true;
    assert_eq!(flying.decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
    let carried = FlushAction::IndexOnly {
        signal: false,
        causes: FlushCauses::empty(),
        write_due: true,
    };
    assert_eq!(Call::at(t1).decide(&mut policy, NoneMode, &mut host), carried);
    let later = Call::at(t0 + half + DATABASE_WRITE_INTERVAL + ns);
    assert_eq!(later.decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
}

/// A minute of steady time.
const MIN: Duration = Duration::from_secs(60);

/// The periodic index write alone, Core's `fPeriodicWrite`.
fn periodic_write() -> FlushAction {
    FlushAction::IndexOnly {
        signal: false,
        causes: FlushCauses::empty(),
        write_due: true,
    }
}

/// A call made while a job is in flight.
fn flying(at: Instant) -> Call {
    let mut call = Call::at(at);
    call.in_flight = true;
    call
}

/// BL-818 (CH3b D243): a node-own or native-measure freeze moves neither timer, as Core writes
/// nothing there. With an index write half an hour before it, the freeze a second before Core's day
/// is out leaves Core's PERIODIC_FLUSH due at its time, the index write not yet due. On a new
/// policy, the same freeze at 59 minutes leaves Core's hourly write due at its time.
#[test]
fn a_node_own_freeze_moves_no_timer() {
    use FlushCauses as C;
    let t0 = Instant::now();
    let ns = Duration::from_nanos(1);
    type Set = fn(&mut Call);
    let rows: [(C, FlushStateMode, Set); 7] = [
        (C::THRESHOLD, IfNeeded, |c| c.boundary = CoinsBoundary::Threshold),
        (C::DMAX, NoneMode, |c| c.dmax_due = true),
        (C::LEFT_IBD, NoneMode, |c| c.left_ibd = true),
        (C::SCRIPTED, NoneMode, |c| c.scripted = true),
        (C::CACHE_CRITICAL, IfNeeded, |c| c.boundary = CoinsBoundary::Critical),
        (C::EVO_CRITICAL, IfNeeded, |c| c.evo_bytes = EVO_CRITICAL_BYTES),
        (C::CACHE_LARGE, Periodic, |c| c.boundary = CoinsBoundary::Large),
    ];
    let tw = t0 + DATABASE_FLUSH_INTERVAL - DATABASE_WRITE_INTERVAL / 2;
    let tf = t0 + DATABASE_FLUSH_INTERVAL - Duration::from_secs(1);
    for (cause, mode, set) in rows {
        let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
        assert_eq!(Call::at(t0).decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
        assert_eq!(Call::at(tw).decide(&mut policy, Periodic, &mut host), periodic_write());
        let mut own = Call::at(tf);
        set(&mut own);
        assert_eq!(own.decide(&mut policy, mode, &mut host), freeze(cause, false), "{cause:?}");
        let day = Call::at(t0 + DATABASE_FLUSH_INTERVAL + ns);
        assert_eq!(
            day.decide(&mut policy, Periodic, &mut host),
            freeze(C::PERIODIC_FLUSH, false),
            "{cause:?}: Core's daily flush"
        );
    }
    for (cause, mode, set) in rows {
        let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
        assert_eq!(Call::at(t0).decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
        let mut own = Call::at(t0 + 59 * MIN);
        set(&mut own);
        assert_eq!(own.decide(&mut policy, mode, &mut host), freeze(cause, false), "{cause:?}");
        let hour = Call::at(t0 + DATABASE_WRITE_INTERVAL + ns);
        assert_eq!(
            hour.decide(&mut policy, Periodic, &mut host),
            periodic_write(),
            "{cause:?}: Core's hourly write"
        );
    }
}

/// BL-818 (CH3b D243): a PERIODIC_FLUSH due at a call made in flight is Core's full flush at that
/// call (`validation.cpp:3086`, `:3113` at v24.0.0), so it records both timers there: a later
/// PERIODIC call in the same flight finds neither due, the freeze after the release takes the
/// held cause and the carried write and moves nothing, and Core's next write and flush fall an
/// hour and a day after the in-flight call.
#[test]
fn a_periodic_flush_due_in_flight_is_recorded_at_its_call() {
    let t0 = Instant::now();
    let (ns, s) = (Duration::from_nanos(1), Duration::from_secs(1));
    let t1 = t0 + DATABASE_FLUSH_INTERVAL + ns;
    let run = |policy: &mut FlushPolicy, host: &mut Host| {
        assert_eq!(Call::at(t0).decide(policy, Periodic, host), FlushAction::Nothing);
        for at in [t1, t1 + s] {
            flying(at).decide(policy, Periodic, host);
        }
        let want = FlushAction::Freeze {
            causes: FlushCauses::PERIODIC_FLUSH,
            prune: None,
            wait_done: false,
            write_due: true,
        };
        assert_eq!(Call::at(t1 + 2 * s).decide(policy, NoneMode, host), want);
    };
    let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
    run(&mut policy, &mut host);
    let hour = Call::at(t1 + DATABASE_WRITE_INTERVAL + ns);
    assert_eq!(hour.decide(&mut policy, Periodic, &mut host), periodic_write(), "the write");
    let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
    run(&mut policy, &mut host);
    let day = Call::at(t1 + DATABASE_FLUSH_INTERVAL + ns);
    assert!(flushes(&day.decide(&mut policy, Periodic, &mut host)), "the flush");
}

/// Whether `action` is a freeze taking Core's PERIODIC_FLUSH.
fn flushes(action: &FlushAction) -> bool {
    matches!(action, FlushAction::Freeze { causes, .. } if causes.contains(FlushCauses::PERIODIC_FLUSH))
}

/// Whether `action` is the prune's freeze alone, with or without a write due beside it.
fn prune_freeze(action: &FlushAction, write_due: bool) -> bool {
    matches!(
        action,
        FlushAction::Freeze { causes, prune: Some(p), write_due: w, .. }
            if *causes == FlushCauses::PRUNE && !p.files.is_empty() && *w == write_due
    )
}

/// BL-818 (CH3b D243): with no best block, PERIODIC_FLUSH writes the index alone and moves no
/// `last_flush` (`validation.cpp:3055-3059`, `:3089` at v24.0.0), so it is due again at each
/// PERIODIC call until a best block lets it flush. A node-own freeze before it moves nothing.
#[test]
fn an_overdue_flush_with_no_best_block_writes_at_each_periodic_call() {
    let t0 = Instant::now();
    let (ns, s) = (Duration::from_nanos(1), Duration::from_secs(1));
    let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
    assert_eq!(Call::at(t0).decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
    let tw = t0 + DATABASE_FLUSH_INTERVAL - DATABASE_WRITE_INTERVAL / 2;
    assert_eq!(Call::at(tw).decide(&mut policy, Periodic, &mut host), periodic_write());
    let mut own = Call::at(t0 + DATABASE_FLUSH_INTERVAL - s);
    own.dmax_due = true;
    assert_eq!(own.decide(&mut policy, NoneMode, &mut host), freeze(FlushCauses::DMAX, false));
    let day = t0 + DATABASE_FLUSH_INTERVAL + ns;
    let index_only = FlushAction::IndexOnly {
        signal: false,
        causes: FlushCauses::PERIODIC_FLUSH,
        write_due: false,
    };
    for k in 0..3u32 {
        let mut call = Call::at(day + k * s);
        call.has_best_block = false;
        assert_eq!(call.decide(&mut policy, Periodic, &mut host), index_only, "call {k}");
    }
    assert_eq!(
        Call::at(day + 3 * s).decide(&mut policy, Periodic, &mut host),
        freeze(FlushCauses::PERIODIC_FLUSH, false),
        "a best block"
    );
    assert_eq!(
        Call::at(day + 4 * s).decide(&mut policy, Periodic, &mut host),
        FlushAction::Nothing
    );
}

/// BL-818 P3 (CH3b D244): a selection records Core's write and flush at the call that selects.
#[test]
fn a_fresh_selection_records_cores_timers_at_its_call() {
    let t0 = Instant::now();
    let ns = Duration::from_nanos(1);
    let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
    host.files = vec![0];
    assert_eq!(Call::at(t0).decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
    host.armed = true;
    let t3 = t0 + 30 * MIN;
    let td = Call::at(t3).decide(&mut policy, NoneMode, &mut host);
    assert!(prune_freeze(&td, false), "{td:?}");
    let hour = t3 + DATABASE_WRITE_INTERVAL;
    assert_eq!(Call::at(hour).decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
    assert_eq!(Call::at(hour + ns).decide(&mut policy, Periodic, &mut host), periodic_write());
    let day = t0 + DATABASE_FLUSH_INTERVAL + ns;
    assert_eq!(
        Call::at(day).decide(&mut policy, Periodic, &mut host),
        periodic_write(),
        "the flush recorded at the selection"
    );
    let day = Call::at(t3 + DATABASE_FLUSH_INTERVAL + ns).decide(&mut policy, Periodic, &mut host);
    assert!(flushes(&day), "{day:?}");
}

/// BL-818 (CH3b D243): a node-own decision starts no timer; Core's first call does.
#[test]
fn a_node_own_decision_starts_no_timer() {
    let t0 = Instant::now();
    let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
    let mut own = Call::at(t0);
    (own.dmax_due, own.core_call) = (true, false);
    assert_eq!(own.decide(&mut policy, NoneMode, &mut host), freeze(FlushCauses::DMAX, false));
    let first = Call::at(t0 + 2 * DATABASE_WRITE_INTERVAL);
    assert_eq!(first.decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
}

/// BL-818 (CH3b D243, DEC-1566 Q4): a node-own decision takes no automatic check that no Core
/// call has met, so it prunes nothing; Core's next call takes it.
#[test]
fn a_node_own_decision_leaves_an_unmet_check_armed() {
    let t0 = Instant::now();
    let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
    (host.armed, host.files) = (true, vec![0]);
    let mut own = Call::at(t0);
    (own.dmax_due, own.core_call) = (true, false);
    assert_eq!(own.decide(&mut policy, NoneMode, &mut host), freeze(FlushCauses::DMAX, false));
    assert_eq!((host.checks, host.armed), (0, true), "not taken");
    let core = Call::at(t0).decide(&mut policy, NoneMode, &mut host);
    assert!(prune_freeze(&core, false), "{core:?}");
    assert_eq!((host.checks, host.armed), (1, false), "taken at Core's call");
}

/// BL-819 FF4 (CH3b D244): an armed automatic check that Core's NONE call at `t1` meets with a
/// node-own job in flight is decided at that call. The peek selects, so V waits for the release
/// with the check still armed, and the decision after it, at the same `now`, freezes the prune and
/// waits for it; Core's write and flush move to `t1`. The peek and the logged run read the same
/// lock rows, so they share a bound.
#[test]
fn an_armed_check_met_in_flight_is_decided_at_that_call() {
    let t0 = Instant::now();
    let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
    host.files = vec![0];
    own_job_in_flight(&mut policy, &mut host, t0);
    let t1 = t0 + 59 * MIN;
    let mut met = flying(t1);
    met.locks = vec![("sub", 900)];
    assert_eq!(
        met.decide(&mut policy, NoneMode, &mut host),
        FlushAction::WaitThenFreeze {
            causes: FlushCauses::empty()
        },
        "the peek selects: wait for the release"
    );
    assert!(host.armed, "the check stays armed across the wait");
    met.in_flight = false;
    let action = met.decide(&mut policy, NoneMode, &mut host);
    assert!(
        matches!(&action, FlushAction::Freeze { causes, prune: Some(p), wait_done: true, write_due: false }
            if *causes == FlushCauses::PRUNE && p.files == [0]),
        "{action:?}"
    );
    assert!(!host.armed, "taken by the decision that froze");
    assert_eq!((policy.timers.last_write, policy.timers.last_flush), (Some(t1), Some(t1)));
    assert_eq!(*host.peeks.borrow(), [(1000, 889, false)]);
    assert_eq!(host.runs.borrow().last(), Some(&(1000, 889, None, false)), "the peek's bound");
}

/// BL-819 FF5, the policy half (CH3b D244): an armed check whose dry run selects nothing, met by
/// Core's NONE call in flight, is taken at that call with one logged run, as Core clears it
/// (`validation.cpp:3028` at v24.0.0). Nothing freezes for it and no timer moves.
#[test]
fn an_armed_check_that_selects_nothing_in_flight_is_taken_there() {
    let t0 = Instant::now();
    let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
    own_job_in_flight(&mut policy, &mut host, t0);
    let t1 = t0 + 59 * MIN;
    assert_eq!(flying(t1).decide(&mut policy, NoneMode, &mut host), FlushAction::Nothing);
    assert_eq!((host.checks, host.armed), (1, false), "the check is taken");
    assert_eq!(host.runs.borrow().len(), 1, "one logged dry run");
    assert_eq!((policy.timers.last_write, policy.timers.last_flush), (Some(t0), Some(t0)));
    let ns = Duration::from_nanos(1);
    let hour = t0 + DATABASE_WRITE_INTERVAL;
    assert_eq!(Call::at(hour).decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
    assert_eq!(Call::at(hour + ns).decide(&mut policy, Periodic, &mut host), periodic_write());
}

/// BL-819 FF6 (CH3b D244): a fresh automatic selection is waited for at every Core mode, as
/// Core's prune is part of its synchronous `FlushStateToDisk`.
#[test]
fn an_automatic_prune_is_waited_for() {
    let t0 = Instant::now();
    for mode in [NoneMode, Periodic, IfNeeded] {
        let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
        (host.armed, host.files) = (true, vec![0]);
        let action = Call::at(t0).decide(&mut policy, mode, &mut host);
        assert!(
            matches!(&action, FlushAction::Freeze { causes, wait_done, .. }
                if causes.contains(FlushCauses::PRUNE) && *wait_done),
            "{mode:?}: {action:?}"
        );
    }
}

/// A node-own job in flight from 58 minutes after Core's first call at `t0`, with the check armed
/// again after that call's take and the host's counts cleared.
fn own_job_in_flight(policy: &mut FlushPolicy, host: &mut Host, t0: Instant) {
    assert_eq!(Call::at(t0).decide(policy, Periodic, host), FlushAction::Nothing);
    let mut own = Call::at(t0 + 58 * MIN);
    (own.dmax_due, own.core_call) = (true, false);
    assert_eq!(own.decide(policy, NoneMode, host), freeze(FlushCauses::DMAX, false));
    (host.armed, host.checks) = (true, 0);
    host.runs.borrow_mut().clear();
}

/// A host whose automatic dry run is empty, so an unwanted peek's take would take the check, and
/// whose peek is a failed assertion.
fn quiet_host() -> Host {
    Host {
        armed: true,
        peek_panics: true,
        ..Host::default()
    }
}

/// BL-819 (CH3b D244, P4): a manual call in flight waits for the release and leaves the automatic
/// check, before and after the wait (`validation.cpp:3020-3024` at v24.0.0): no peek, no take.
#[test]
fn a_manual_prune_in_flight_leaves_the_check() {
    let t0 = Instant::now();
    let (mut policy, mut host) = (FlushPolicy::new(false), quiet_host());
    own_job_in_flight(&mut policy, &mut host, t0);
    let mut manual = flying(t0 + 59 * MIN);
    manual.manual = Some(500);
    let action = manual.decide(&mut policy, NoneMode, &mut host);
    assert!(matches!(action, FlushAction::WaitThenFreeze { .. }), "{action:?}");
    assert!(host.armed, "the check is still armed");
    manual.in_flight = false;
    manual.decide(&mut policy, NoneMode, &mut host);
    assert!(host.armed, "the check is still armed after the wait");
    assert_eq!(host.checks, 0);
}

/// BL-819 (CH3b D244, P5): a call under `fReindex` runs no prune half (`validation.cpp:3000` at
/// v24.0.0), in flight or not: no peek, no take.
#[test]
fn a_reindex_call_in_flight_leaves_the_check() {
    let t0 = Instant::now();
    let (mut policy, mut host) = (FlushPolicy::new(false), quiet_host());
    own_job_in_flight(&mut policy, &mut host, t0);
    let mut reindex = flying(t0 + 59 * MIN);
    reindex.reindex = true;
    assert_eq!(reindex.decide(&mut policy, NoneMode, &mut host), FlushAction::Nothing);
    assert!(host.armed, "the check is still armed");
    assert_eq!(host.checks, 0);
}

/// BL-819 (CH3b D244, DEC-1566 Q4, P6): a node-own decision runs no automatic check, in flight
/// or not: Core's next call does.
#[test]
fn a_node_own_decision_never_runs_a_check() {
    let t0 = Instant::now();
    let (mut policy, mut host) = (FlushPolicy::new(false), quiet_host());
    own_job_in_flight(&mut policy, &mut host, t0);
    for in_flight in [true, false] {
        let mut own = Call::at(t0 + 59 * MIN);
        (own.dmax_due, own.core_call, own.in_flight) = (true, false, in_flight);
        own.decide(&mut policy, NoneMode, &mut host);
        assert!(host.armed, "in flight {in_flight}: the check is still armed");
    }
    assert_eq!((host.checks, host.runs.borrow().len()), (0, 0));
}

/// BL-819 (CH3b D244, rule 2's fallback, P24): a logged run that selects after an empty peek arms
/// the check again and waits, so the selection is made after the release, never dropped.
#[test]
fn a_logged_run_that_selects_after_an_empty_peek_waits() {
    let t0 = Instant::now();
    let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
    own_job_in_flight(&mut policy, &mut host, t0);
    (host.armed, host.files, host.peek) = (true, vec![0], Some(false));
    let mut call = flying(t0 + 59 * MIN);
    let action = call.decide(&mut policy, NoneMode, &mut host);
    assert!(matches!(action, FlushAction::WaitThenFreeze { .. }), "{action:?}");
    assert!(host.armed, "armed again");
    assert_eq!(host.runs.borrow().len(), 1, "the logged run");
    call.in_flight = false;
    let action = call.decide(&mut policy, NoneMode, &mut host);
    assert!(prune_freeze(&action, false), "{action:?}");
    assert!(!host.armed);
}

/// BL-819 (CH3b D244): a PERIODIC call made in flight with Core's daily flush due and an armed
/// check that selects is one full flush of Core's: after the wait, one freeze with PERIODIC_FLUSH
/// and PRUNE and the write, waited for (`validation.cpp:3030-3031`, `:3055-3057` at v24.0.0).
#[test]
fn a_selection_after_the_wait_books_the_calls_flags() {
    let t0 = Instant::now();
    let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
    assert_eq!(Call::at(t0).decide(&mut policy, Periodic, &mut host), FlushAction::Nothing);
    (host.armed, host.files) = (true, vec![0]);
    let t1 = t0 + DATABASE_FLUSH_INTERVAL + Duration::from_secs(1);
    let mut call = flying(t1);
    assert_eq!(
        call.decide(&mut policy, Periodic, &mut host),
        FlushAction::WaitThenFreeze {
            causes: FlushCauses::PERIODIC_FLUSH
        }
    );
    call.in_flight = false;
    let action = call.decide(&mut policy, Periodic, &mut host);
    assert!(
        matches!(&action, FlushAction::Freeze { causes, prune: Some(p), wait_done: true, write_due: true }
            if *causes == FlushCauses::PERIODIC_FLUSH | FlushCauses::PRUNE && p.files == [0]),
        "{action:?}"
    );
    assert_eq!((policy.timers.last_write, policy.timers.last_flush), (Some(t1), Some(t1)));
}

/// BL-819 (CH3b D244): each Core call decides its own check, with one timer set. A selection
/// made after a wait records at its call, a second check armed later is decided at the next Core
/// call that meets it, a manual prune records at its own call and leaves the automatic check,
/// and a no-tip call in flight takes the check with no selection and no record.
#[test]
fn each_core_call_decides_its_own_check() {
    let t0 = Instant::now();
    let (mut policy, mut host) = (FlushPolicy::new(false), Host::default());
    own_job_in_flight(&mut policy, &mut host, t0);
    (host.armed, host.files) = (true, vec![0]);
    let t1 = t0 + 59 * MIN;
    let mut first = flying(t1);
    assert!(matches!(
        first.decide(&mut policy, NoneMode, &mut host),
        FlushAction::WaitThenFreeze { .. }
    ));
    first.in_flight = false;
    assert!(prune_freeze(&first.decide(&mut policy, NoneMode, &mut host), false));
    assert_eq!((policy.timers.last_write, policy.timers.last_flush), (Some(t1), Some(t1)));

    host.armed = true;
    let t2 = t0 + 70 * MIN;
    assert!(prune_freeze(&Call::at(t2).decide(&mut policy, NoneMode, &mut host), false));
    assert_eq!(policy.timers.last_write, Some(t2), "the second check, at its call");

    let mut manual = Call::at(t0 + 80 * MIN);
    manual.manual = Some(500);
    host.armed = true;
    let action = manual.decide(&mut policy, NoneMode, &mut host);
    assert!(
        matches!(&action, FlushAction::Freeze { prune: Some(p), wait_done: true, .. } if p.manual == Some(500)),
        "{action:?}"
    );
    assert!(host.armed, "the manual prune leaves the automatic check");
    assert_eq!(policy.timers.last_write, Some(t0 + 80 * MIN), "the manual prune's own call");

    host.no_tip = true;
    let runs = host.runs.borrow().len();
    let mut no_tip = flying(t0 + 90 * MIN);
    no_tip.has_best_block = false;
    assert_eq!(no_tip.decide(&mut policy, NoneMode, &mut host), FlushAction::Nothing);
    assert!(!host.armed, "taken in flight");
    assert_eq!(host.runs.borrow().len(), runs, "no dry run");
    assert_eq!(policy.timers.last_write, Some(t0 + 80 * MIN), "no record");
}
