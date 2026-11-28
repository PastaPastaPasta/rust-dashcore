//! PLAN-LK1b §5.3 item 7 on the connector harness: `finish_attempt_reported`'s report reaches
//! `record_connect` on the connect and the proposal paths, with the whole log and the count of
//! effects applied before a failure; `ScriptedEvo` applies counterexample S's log through given
//! appliers; and, with the `statedigest` feature, `DigestLog` writes `fx` only for an attempt
//! whose finish succeeded and whose outcome is not an `Err`.

use super::*;
use crate::connect::{DigestAttempt, EffectCoverage, NoEvo};
use crate::evo::effects::{
    EffectAppliers, IsLockApplier, MemPlatformBans, MnListSignal, SnapshotApplier,
    UnreachableMineable,
};

/// One connect attempt as the sink saw it.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::chain::tests) struct Recorded {
    pub(in crate::chain::tests) just_check: bool,
    pub(in crate::chain::tests) outcome_ok: bool,
    pub(in crate::chain::tests) finish: FinishReport,
}

/// A sink keeping each connect attempt's report.
#[derive(Clone, Default)]
pub(in crate::chain::tests) struct Reports(Arc<Mutex<Vec<Recorded>>>);

impl Reports {
    /// Every report, in order.
    pub(in crate::chain::tests) fn all(&self) -> Vec<Recorded> {
        self.0.lock().expect("reports").clone()
    }

    /// The reports of attempts that emitted effects (genesis's attempt emits none).
    fn with_effects(&self) -> Vec<Recorded> {
        self.all()
            .into_iter()
            .filter(|r| {
                !matches!(&r.finish.effects, EffectCoverage::Known { emitted, .. } if emitted.is_empty())
            })
            .collect()
    }
}

impl DigestSink for Reports {
    fn record(&mut self, _: &ConnectCx<'_>, attempt: &DigestAttempt<'_>) -> Result<(), Fatal> {
        if let DigestAttempt::Connect(c) = attempt {
            self.0.lock().expect("reports").push(Recorded {
                just_check: c.just_check,
                outcome_ok: c.outcome.is_ok(),
                finish: c.finish.clone(),
            });
        }
        Ok(())
    }
}

/// [`is_setup`] over locks A and B, applying fails at `fail_apply`, reporting to `reports`.
fn reported(fail_apply: Option<usize>, reports: &Reports) -> Setup {
    let mut setup = is_setup(vec![lock_a(), lock_b()], true);
    setup.steps.fail_apply = fail_apply;
    setup.digest = Some(Box::new(reports.clone()));
    setup
}

/// `TestEvo` on the connect path: a whole application is `Known { emitted, emitted.len() }`;
/// a failure at index k is `completed = k` with the whole log emitted (k = 2: after the whole
/// log applied, TestEvo's stand-in for a failed release).
#[test]
fn the_connect_path_records_the_finish_report() {
    for fail_apply in [None, Some(0), Some(1), Some(2)] {
        let reports = Reports::default();
        let mut w = world(reported(fail_apply, &reports));
        let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![spend(&[seed_op(1), seed_op(2)], COIN)]);
        match fail_apply {
            None => drop(w.rig.accept(&b1)),
            Some(k) => w.rig.submit_stopping(&b1, &format!("k {k}")),
        }
        w.rig.stop();
        let emitted = vec![removal(&lock_a()), removal(&lock_b())];
        let (result, completed) = match fail_apply {
            None => (Ok(()), 2),
            Some(k) => (Err(Fatal::new(APPLY_FAILED)), k),
        };
        let want = Recorded {
            just_check: false,
            outcome_ok: fail_apply.is_none(),
            finish: FinishReport::known(result, emitted, completed),
        };
        assert_eq!(reports.with_effects(), [want], "{fail_apply:?}");
    }
}

/// `TestEvo` on the proposal path (`TestBlockValidity`): the same report reaches the record of
/// the proposal's attempt, on success and after a failure at index 0.
#[test]
fn the_proposal_path_records_the_finish_report() {
    for fail_apply in [None, Some(0)] {
        let reports = Reports::default();
        let mut setup = reported(fail_apply, &reports);
        setup.is_store = vec![lock_a()];
        let mut w = world(setup);
        let p = w.rig.p.clone();
        // b1 seeds the coins (the harness's first connect).
        let b1 = block_on(&p, &w.rig.genesis, vec![]);
        w.rig.accept(&b1);
        let b2 = block_on(&p, &b1, vec![spend(&[seed_op(1)], COIN)]);
        let r = w.rig.v.test_block_validity(b2.block.clone(), false, true).recv();
        assert_eq!(r.is_ok(), fail_apply.is_none(), "{fail_apply:?}: {r:?}");
        w.rig.stop();
        let (result, completed) = match fail_apply {
            None => (Ok(()), 1),
            Some(k) => (Err(Fatal::new(APPLY_FAILED)), k),
        };
        let want = Recorded {
            just_check: true,
            outcome_ok: fail_apply.is_none(),
            finish: FinishReport::known(result, vec![removal(&lock_a())], completed),
        };
        assert_eq!(reports.with_effects(), [want], "{fail_apply:?}");
    }
}

/// `NoEvo`'s report: an empty log is `Known { [], 0 }`; a log with an effect is refused at its
/// first effect, `completed = 0` with the whole log emitted.
#[test]
fn no_evo_reports_its_log() {
    let mut w = world(Setup::default());
    let reports = super::disconnect::on_v_cx(&w, AttemptOrigin::Tip, |cx, _, v| {
        let mut run = |log: Vec<Effect>| -> Result<FinishReport, Fatal> {
            let (id, target) = (AttemptId::next(), ViewTarget::Active);
            let mut att = NoEvo.begin_attempt(&(), v, cx, id, AttemptOrigin::Tip, &target)?;
            for e in log {
                att.sink().emit(e);
            }
            Ok(NoEvo.finish_attempt_reported(att))
        };
        Ok::<_, Fatal>((run(vec![])?, run(vec![removal(&lock_a()), removal(&lock_b())])?))
    });
    let (empty, refused) = reports.expect("both attempts open");
    assert_eq!(empty, FinishReport::known(Ok(()), vec![], 0));
    let EffectCoverage::Known {
        emitted,
        completed,
    } = refused.effects
    else {
        panic!("NoEvo reports its log");
    };
    assert_eq!((emitted, completed), (vec![removal(&lock_a()), removal(&lock_b())], 0));
    let text = refused.result.expect_err("refused").0;
    assert!(text.contains("no applier for a"), "{text}");
    w.rig.stop();
}

/// A snapshot applier whose writes fail, or succeed and are counted.
struct Snapshots {
    fail: bool,
    written: Arc<Mutex<usize>>,
}

impl SnapshotApplier for Snapshots {
    fn write(
        &mut self,
        _: LlmqType,
        _: &BlockHash,
        _: &dash_evo::llmq::QuorumSnapshot,
    ) -> Result<(), Fatal> {
        if self.fail {
            return Err(Fatal::new(SNAPSHOT_FAILED));
        }
        *self.written.lock().expect("written") += 1;
        Ok(())
    }
}

const SNAPSHOT_FAILED: &str = "injected snapshot write failure";

/// An InstantSend applier that counts the removals it applies.
struct Removals(Arc<Mutex<Vec<dash_num::Hash256>>>);

impl IsLockApplier for Removals {
    fn remove(&mut self, lock_hash: &dash_num::Hash256) -> Result<(), Fatal> {
        self.0.lock().expect("removed").push(*lock_hash);
        Ok(())
    }
}

fn snapshot_effect() -> Effect {
    Effect::StoreQuorumSnapshot {
        llmq_type: LlmqType::LlmqTestDip0024,
        cycle_base: BlockHash::from([1; 32]),
        snapshot: dash_evo::llmq::QuorumSnapshot {
            active_quorum_members: vec![true],
            skip_list_mode: 0,
            skip_list: vec![],
        },
    }
}

/// Counterexample S (§5.3) through `ScriptedEvo`: H4 emits a `StoreQuorumSnapshot` before the IS
/// loop removes lock A. With the snapshot applier failing, the finish fails at index 0, the
/// removal is never applied and the report is `completed = 0` with both effects emitted; with it
/// succeeding, both apply and the block connects.
#[test]
fn scripted_evo_applies_h4_effects_before_the_is_loop() {
    for fail in [true, false] {
        let reports = Reports::default();
        let (written, removed) = (Arc::new(Mutex::new(0)), Arc::new(Mutex::new(Vec::new())));
        let hooks = ScriptedEvo::new(EffectAppliers {
            snapshot: Box::new(Snapshots {
                fail,
                written: written.clone(),
            }),
            is_lock: Box::new(Removals(removed.clone())),
            mineable: Box::new(UnreachableMineable),
            platform_ban: Box::new(MemPlatformBans::default()),
            notify: Box::new(MnListSignal::new(Default::default()).applier()),
        });
        let script = hooks.handle();
        let mut setup = reported(None, &reports);
        setup.is_store = vec![lock_a()];
        setup.scripted_evo = Some(hooks);
        let mut w = world(setup);
        let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![spend(&[seed_op(1)], COIN)]);
        script.push_h4(vec![snapshot_effect()]);
        if fail {
            w.rig.submit_stopping(&b1, "snapshot");
        } else {
            w.rig.accept(&b1);
        }
        let exit = w.rig.stop();
        let emitted = vec![snapshot_effect(), removal(&lock_a())];
        let want = if fail {
            assert_eq!(fatal_text(&exit), Some(SNAPSHOT_FAILED));
            FinishReport::known(Err(Fatal::new(SNAPSHOT_FAILED)), emitted, 0)
        } else {
            FinishReport::known(Ok(()), emitted, 2)
        };
        let recorded: Vec<_> = reports.with_effects().into_iter().map(|r| r.finish).collect();
        assert_eq!(recorded, std::slice::from_ref(&want), "fail {fail}");
        assert!(script.reports().contains(&want), "fail {fail}: the hooks' own report");
        let applied = if fail {
            (0, vec![])
        } else {
            (1, vec![lock_a().r.hash])
        };
        let got = (*written.lock().expect("written"), removed.lock().expect("removed").clone());
        assert_eq!(got, applied, "fail {fail}");
    }
}

#[cfg(feature = "statedigest")]
mod fx {
    use std::io::{self, Write};

    use dash_statedigest::{Effect as FxEffect, FileKind, Hash256 as DHash, Kind, Lines, Record};

    use super::*;
    use crate::connect::{DigestLog, FxSource};

    /// A shared byte log.
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl Write for Buf {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("buf").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Buf {
        /// Each record after the `H`: its kind and its `fx`, parsed back.
        fn fx(&self) -> Vec<(Kind, Option<Vec<FxEffect>>)> {
            let bytes = self.0.lock().expect("buf").clone();
            Lines::new(&bytes)
                .filter_map(|(_, r)| match r.expect("a record") {
                    Record::Outcome(o) => {
                        let fx = o.o().get("fx").map(|v| FxEffect::list_from_value(v).expect("fx"));
                        Some((o.kind(), fx))
                    }
                    _ => None,
                })
                .collect()
        }
    }

    fn entry() -> FxEffect {
        FxEffect::IsRm {
            islock: DHash::from_wire_bytes([7; 32]),
            txid: DHash::from_wire_bytes([8; 32]),
        }
    }

    /// A source that always renders [`entry`], counting its calls.
    struct Always(Arc<Mutex<usize>>);

    impl FxSource for Always {
        fn take(&mut self, _: &crate::connect::ConnectAttempt<'_>) -> Option<Vec<FxEffect>> {
            *self.0.lock().expect("calls") += 1;
            Some(vec![entry()])
        }
    }

    /// A log over `buf` with [`Always`] as its source, if `fx`.
    fn log(buf: &Buf, calls: &Arc<Mutex<usize>>, fx: bool) -> Box<dyn DigestSink> {
        let header = super::super::digest::header(FileKind::Digest);
        let log = DigestLog::new(buf.clone(), header).expect("the H is written");
        if fx {
            Box::new(log.with_fx(Box::new(Always(calls.clone()))))
        } else {
            Box::new(log)
        }
    }

    /// r2d on the connect path: a successful finish writes the source's `fx` on its `C`; a failed
    /// finish's `R` has none, although the source returned `Some` for it. The source is asked once
    /// per record. Without `with_fx` no record has `fx` (CH3a D135). The `fx` read back from
    /// the record equals what the source rendered.
    #[test]
    fn fx_is_written_only_after_a_successful_finish() {
        for (fail_apply, fx) in [(None, true), (Some(1), true), (None, false)] {
            let (buf, calls) = (Buf::default(), Arc::new(Mutex::new(0)));
            let mut setup = is_setup(vec![lock_a(), lock_b()], true);
            setup.steps.fail_apply = fail_apply;
            setup.digest = Some(log(&buf, &calls, fx));
            setup.node_stop = true;
            let mut w = world(setup);
            let b1 =
                block_on(&w.rig.p, &w.rig.genesis, vec![spend(&[seed_op(1), seed_op(2)], COIN)]);
            match fail_apply {
                None => drop(w.rig.accept(&b1)),
                Some(_) => w.rig.submit_stopping(&b1, "apply"),
            }
            w.rig.stop();
            let some = fx.then(|| vec![entry()]);
            let want = match fail_apply {
                None => vec![(Kind::Connect, some.clone()), (Kind::Connect, some)],
                Some(_) => vec![(Kind::Connect, some), (Kind::Reject, None)],
            };
            assert_eq!(buf.fx(), want, "{fail_apply:?} {fx}");
            let asked = if fx {
                2
            } else {
                0
            };
            assert_eq!(*calls.lock().expect("calls"), asked, "{fail_apply:?} {fx}");
        }
    }

    /// F3-N3 and the finish-result check on the connect path: a reported readiness refusal after
    /// the IS loop (the masternode payee's) is an `Ok(SystemError)`, and that outcome stays `Ok`
    /// whether or not the finish applied the log (D75). G2's wait (DEC-1874) is an unreported
    /// `Ok(Waiting)` when the finish applied, and the failed application is the outcome when it
    /// did not. With the removal applied the `R` carries the source's `fx`; with the application
    /// failing at index 0 it has none, although a reported refusal's outcome is not an `Err`.
    #[test]
    fn a_reported_refusal_keeps_fx_only_when_its_finish_applied() {
        for (waiting, fail_apply) in
            [(false, None), (false, Some(0)), (true, None), (true, Some(0))]
        {
            let case = format!("waiting {waiting} {fail_apply:?}");
            let (buf, calls) = (Buf::default(), Arc::new(Mutex::new(0)));
            let reports = Reports::default();
            let mut setup = is_setup(vec![lock_a()], true);
            setup.steps.fail_apply = fail_apply;
            if waiting {
                setup.helper.interim_at = Some(1);
            } else {
                setup.params = Some(early_enforcement());
            }
            setup.digest = Some(Box::new(Both(log(&buf, &calls, true), reports.clone())));
            let mut w = world(setup);
            let p = w.rig.p.clone();
            let cb = coinbase_tx(1, 1, 0, COIN);
            let b1 = with_coinbase(&p, &w.rig.genesis, cb, vec![spend(&[seed_op(1)], COIN)]);
            if waiting && fail_apply.is_none() {
                assert!(w.rig.submit(&b1).accepted, "{case}");
                w.rig.assert_no_abort();
            } else {
                w.rig.submit_stopping(&b1, &case);
            }
            w.rig.stop();
            let (result, completed) = match fail_apply {
                None => (Ok(()), 1),
                Some(k) => (Err(Fatal::new(APPLY_FAILED)), k),
            };
            let want = Recorded {
                just_check: false,
                outcome_ok: !waiting || fail_apply.is_none(),
                finish: FinishReport::known(result, vec![removal(&lock_a())], completed),
            };
            assert_eq!(reports.with_effects(), [want], "{case}");
            let fx = fail_apply.is_none().then(|| vec![entry()]);
            let want = vec![(Kind::Connect, Some(vec![entry()])), (Kind::Reject, fx)];
            assert_eq!(buf.fx(), want, "{case}");
            assert_eq!(*calls.lock().expect("calls"), 2, "{case}");
        }
    }

    /// Two sinks fed the same attempts, in order.
    struct Both(Box<dyn DigestSink>, Reports);

    impl DigestSink for Both {
        fn record(&mut self, cx: &ConnectCx<'_>, attempt: &DigestAttempt<'_>) -> Result<(), Fatal> {
            self.0.record(cx, attempt)?;
            self.1.record(cx, attempt)
        }
    }

    /// `fJustCheck`, the outcome and the finish's result of one record.
    type Case = (bool, Result<ConnectOutcome, Fatal>, Result<(), Fatal>);

    /// r2d, the log's own rule: on `R` records, with a source that always returns `Some`, `fx`
    /// is written for an `Ok` finish with a non-`Err` outcome (a reported system error, a
    /// passing `fJustCheck`), and never for an `Err` finish, nor for an `Err` outcome with an `Ok`
    /// finish. The source is asked for every record.
    #[test]
    fn the_log_writes_fx_only_for_an_ok_finish_and_a_non_err_outcome() {
        let mut w = world(Setup::default());
        let g = w.rig.genesis.clone();
        let b1 = block_on(&w.rig.p, &g, vec![]);
        w.rig.accept(&b1);
        let at = BlockAt {
            hash: b1.hash(),
            height: 1,
            prev: Some(w.rig.id(&g)),
            id: Some(w.rig.id(&b1)),
        };
        let (buf, calls) = (Buf::default(), Arc::new(Mutex::new(0)));
        let mut sink = log(&buf, &calls, true);
        let block = b1.block.clone();
        let system = || Ok(ConnectOutcome::SystemError("refused".to_owned()));
        let cases: Vec<Case> = vec![
            (false, system(), Ok(())),
            (true, Ok(ConnectOutcome::Connected), Ok(())),
            (false, system(), Err(Fatal::new("release refused"))),
            (true, Ok(ConnectOutcome::Connected), Err(Fatal::new("apply failed"))),
            (false, Err(Fatal::new("IS store read failed")), Ok(())),
        ];
        let recorded =
            super::super::disconnect::on_v_cx(&w, AttemptOrigin::Tip, move |cx, _, _| {
                cases
                    .into_iter()
                    .map(|(just_check, outcome, result)| {
                        let finish = FinishReport::known(result, vec![], 0);
                        let attempt = crate::connect::ConnectAttempt {
                            block: &block,
                            at: &at,
                            just_check,
                            active: false,
                            outcome: &outcome,
                            undo: None,
                            subsidy: None,
                            script_checks: None,
                            evo: None,
                            finish: &finish,
                        };
                        sink.record(cx, &DigestAttempt::Connect(attempt))
                    })
                    .collect::<Result<Vec<_>, _>>()
            });
        assert_eq!(recorded.map(|r| r.len()), Ok(5));
        let fx: Vec<_> = buf.fx().into_iter().map(|(kind, fx)| (kind, fx.is_some())).collect();
        let r = Kind::Reject;
        assert_eq!(fx, [(r, true), (r, true), (r, false), (r, false), (r, false)]);
        assert_eq!(*calls.lock().expect("calls"), 5);
        w.rig.stop();
    }
}
