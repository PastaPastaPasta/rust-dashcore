//! EV addendum §2.6's attempt on the production members facade (CH3a D56, CH7 §4.2.1): hooks
//! shaped as CH7a's, whose H4 makes members calls through the attempt's handle over LQ1's rotated
//! fixture and whose log goes through the production appliers, driven by a connector shaped as
//! `CoreConnector` on a real V loop. That it compiles is the point: hooks that own the facade
//! cannot return its handle, and a `&'v mut self` opener blocks every later hook call (Sol a2
//! r1). Running it shows the gate held exclusively for the whole attempt, log application
//! included, and released on every path.
//!
//! ADD §2.6's cases 1-4 run there, each beside LQ1's model of the facade in its attempt
//! (`ModelAttempt`, LQ1m) with the same inputs. Case 5 and §2.4's injected absence run on
//! `CoreConnector` itself, over the node's stores, where V's stop is observable.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use dash_chain::Status;
use dash_consensus::coins::CoinsView;
use dash_consensus::ctx::bls_ctx_for_block;
use dash_consensus::payments::MnRewardEra;
use dash_consensus::primitives::Header;
use dash_consensus::{
    Amount, Block, BlockId, BlockValidationError, BlockValidationResult, BlsCtx, Tx,
    TxValidationError,
};
use dash_dml::blockprocessor::QcBlockError;
use dash_dml::{Dmn, MnList, StoreError};
use dash_evo::cbtx::CoinbaseFacts;
use dash_evo::llmq::LlmqSource;
use dash_evo::llmq::reference::{ModelAttempt, ModelRows};
use dash_evo::types::ProTxHash;
use dash_evo::{Effect, EffectSink};
use dash_params::ChainParams;
use dash_primitives::{BlockHash, BlockHeader, LlmqType};
use dash_types::Numeric as _;

use super::llmq::appliers;
use super::llmq_members::{Fixture, ROTATED, Source, TIP, World, pros, world};
use super::node_evo::NoCoins;
use super::node_stores::{Seams, Stores};
use super::support::{Ev, Rig, branch, with_tip_cx};
use crate::chain::{AttemptOrigin, ConnectCx, EvoTarget, Fatal, VContext, ViewTarget};
use crate::connect::{
    AttemptId, BlockAt, EvoAttempt, EvoDigest, EvoHooks, SpecialTxFailure, SpecialTxRules,
};
use crate::evo::effects::EffectAppliers;
use crate::evo::node::qc_failure;
use crate::llmq::facade::{AttemptHandle, MembersFacade};
use crate::llmq::members::members;
use crate::store::CoinsReadError;

/// One members call H4 makes over the rotated fixture: the type, the base's height, `reset`, and
/// the masternodes the source lists at the heights it names (all nine elsewhere).
#[derive(Clone, Debug)]
struct Call {
    t: LlmqType,
    height: u32,
    reset: bool,
    lists: BTreeMap<u32, Vec<u32>>,
}

/// A rotated call at `height`.
fn at(height: u32) -> Call {
    Call {
        t: ROTATED,
        height,
        reset: false,
        lists: BTreeMap::new(),
    }
}

/// The fixture's lists, or none at all: every list read is an absence (§2.4).
struct Withheld<'a> {
    lists: Source<'a>,
    withheld: bool,
}

impl LlmqSource<MnList> for Withheld<'_> {
    type Error = StoreError;

    fn mn_list(&self, at: BlockId) -> Result<Option<Arc<MnList>>, StoreError> {
        if self.withheld {
            return Ok(None);
        }
        self.lists.mn_list(at)
    }

    fn coinbase_facts(&self, at: BlockId) -> Result<Option<CoinbaseFacts>, StoreError> {
        self.lists.coinbase_facts(at)
    }
}

/// Hooks shaped as CH7a's: they own their state (the appliers the log goes through, and the
/// fixture H4 computes over) and never the facade.
pub(super) struct Ch7aShape {
    appliers: EffectAppliers,
    fx: Fixture,
    /// H4's calls in the next attempt.
    script: Vec<Call>,
    /// The source withholds every list (§2.4).
    withheld: bool,
    /// H4 refuses the block after its calls.
    refuse: bool,
    /// Each H4 call's members, in order.
    answers: Vec<Vec<ProTxHash>>,
    /// Each attempt's log, as `finish_attempt` applied it.
    logs: Vec<Vec<Effect>>,
    /// Per hook call inside an attempt, and per log application: could an external route have
    /// taken the gate?
    gate_free_inside: Vec<bool>,
    /// H11's read fails, as an evo store read can.
    fail_digest: bool,
}

impl Ch7aShape {
    /// Over the rotated fixture, the log applied to `w`'s rows.
    fn new(w: &World) -> Ch7aShape {
        Ch7aShape {
            appliers: appliers(&w.rows),
            fx: Fixture::rotated(),
            script: Vec::new(),
            withheld: false,
            refuse: false,
            answers: Vec::new(),
            logs: Vec::new(),
            gate_free_inside: Vec::new(),
            fail_digest: false,
        }
    }

    fn note_gate(&mut self, facade: &MembersFacade) {
        self.gate_free_inside.push(facade.gate().try_read().is_some());
    }
}

/// CH7a's attempt keeps the facade it releases through.
pub(super) struct Ch7aAttempt<'v> {
    facade: &'v MembersFacade,
    handle: AttemptHandle<'v>,
    log: Vec<Effect>,
}

impl EvoAttempt for Ch7aAttempt<'_> {
    fn id(&self) -> AttemptId {
        self.handle.id()
    }

    fn sink(&mut self) -> &mut dyn EffectSink {
        &mut self.log
    }
}

impl EvoHooks for Ch7aShape {
    const COMPLETE: bool = false;

    type Facade = MembersFacade;

    type Attempt<'v> = Ch7aAttempt<'v>;

    type ListUpdates = ();

    fn begin_attempt<'v>(
        &mut self,
        facade: &'v MembersFacade,
        v: &'v mut VContext,
        _: &'v ConnectCx<'_>,
        id: AttemptId,
        _: AttemptOrigin,
        _: &ViewTarget<'_>,
    ) -> Result<Ch7aAttempt<'v>, Fatal> {
        Ok(Ch7aAttempt {
            facade,
            handle: facade.begin_attempt(v, id),
            log: Vec::new(),
        })
    }

    /// CH7a's application, as `NodeEvo`'s: the whole log in order under the exclusive gate, then
    /// the release, or the failure's trip before the gate is released (D4).
    fn finish_attempt(&mut self, attempt: Ch7aAttempt<'_>) -> Result<(), Fatal> {
        let Ch7aAttempt {
            facade,
            handle,
            log,
        } = attempt;
        self.note_gate(facade);
        self.logs.push(log.clone());
        match self.appliers.apply_log(handle.id(), &log) {
            Ok(applied) => facade.release_attempt(handle, applied),
            Err(failure) => Err(handle.fail(failure)),
        }
    }

    fn bls_ctx(
        &mut self,
        _: &mut Ch7aAttempt<'_>,
        _: &mut EvoTarget<'_>,
        params: &ChainParams,
        at: &BlockAt,
    ) -> BlsCtx {
        bls_ctx_for_block(params, at.height)
    }

    fn commit_bls_scheme(&mut self, _: &mut Ch7aAttempt<'_>, _: &mut EvoTarget<'_>) {}

    fn verify_best_block(
        &mut self,
        attempt: &mut Ch7aAttempt<'_>,
        _: &mut EvoTarget<'_>,
        _: &BlockHash,
    ) -> Result<bool, Fatal> {
        self.note_gate(attempt.facade);
        Ok(true)
    }

    /// H4: the script's members calls through the attempt's handle, each over its own source; a
    /// call's failure as H4's quorum processor maps it (D5).
    fn process_special_txs(
        &mut self,
        attempt: &mut Ch7aAttempt<'_>,
        _: &mut EvoTarget<'_>,
        _: &Block,
        _: &BlockAt,
        _: SpecialTxRules,
        _: &dyn CoinsView<Error = CoinsReadError>,
        _: Amount,
        _: bool,
        _: bool,
    ) -> Result<Result<(), SpecialTxFailure>, Fatal> {
        self.note_gate(attempt.facade);
        let Self {
            fx,
            script,
            withheld,
            answers,
            ..
        } = self;
        for call in std::mem::take(script) {
            let src = Withheld {
                lists: fx.source_with(call.lists),
                withheld: *withheld,
            };
            let got = fx.ctx(|ctx| {
                let (h, log) = (&attempt.handle, &mut attempt.log);
                members(h, ctx, &src, log, call.t, fx.at(call.height), call.reset)
            });
            match got {
                Ok(m) => answers.push(pros(&m)),
                Err(e) => return qc_failure(QcBlockError::from(e)).map(Err),
            }
        }
        if self.refuse {
            let err = BlockValidationError::new(BlockValidationResult::Consensus, "bad-test");
            return Ok(Err(SpecialTxFailure::Invalid(err)));
        }
        Ok(Ok(()))
    }

    fn mn_payment_valid(
        &mut self,
        _: &mut Ch7aAttempt<'_>,
        _: &mut EvoTarget<'_>,
        _: &Tx,
        _: Option<BlockId>,
        _: Amount,
        _: Amount,
        _: MnRewardEra,
        _: bool,
    ) -> Result<bool, Fatal> {
        Ok(true)
    }

    fn set_best_block(
        &mut self,
        attempt: &mut Ch7aAttempt<'_>,
        _: &mut EvoTarget<'_>,
        _: &BlockHash,
    ) -> Result<(), Fatal> {
        self.note_gate(attempt.facade);
        Ok(())
    }

    fn list_changed(&mut self, attempt: &mut Ch7aAttempt<'_>, _: &(), _: bool) {
        self.note_gate(attempt.facade);
    }

    fn shared_collateral_spends(
        &mut self,
        _: &mut Ch7aAttempt<'_>,
        _: &Tx,
        _: &dyn CoinsView<Error = CoinsReadError>,
    ) -> Result<Result<(), TxValidationError>, Fatal> {
        Ok(Ok(()))
    }

    fn undo_special_txs(
        &mut self,
        _: &mut Ch7aAttempt<'_>,
        _: &mut EvoTarget<'_>,
        _: &Block,
        _: &BlockAt,
    ) -> Result<Option<()>, Fatal> {
        Ok(Some(()))
    }

    fn begin_tx(
        &mut self,
        attempt: &mut Ch7aAttempt<'_>,
        _: &mut EvoTarget<'_>,
    ) -> Result<(), Fatal> {
        self.note_gate(attempt.facade);
        Ok(())
    }

    fn commit_tx(&mut self, _: &mut EvoTarget<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn rollback_tx(&mut self, _: &mut EvoTarget<'_>) {}

    fn digest_fields(
        &mut self,
        attempt: &mut Ch7aAttempt<'_>,
        _: &mut EvoTarget<'_>,
        _: &BlockAt,
    ) -> Result<EvoDigest, Fatal> {
        self.note_gate(attempt.facade);
        if self.fail_digest {
            return Err(Fatal::new("evo store read failed"));
        }
        Ok(EvoDigest::default())
    }
}

/// `CoreConnector`'s shape (D56): the hooks and the facade are fields, and the bracket (CH3 §3.2)
/// is a `&mut self` method that calls `&mut self` helpers while the attempt is open.
struct Connector<H: EvoHooks> {
    hooks: H,
    facade: Arc<H::Facade>,
}

impl<H: EvoHooks> Connector<H> {
    /// One attempt of `origin`: whether H4 passed, or the fault.
    fn bracket(
        &mut self,
        v: &mut VContext,
        cx: &ConnectCx<'_>,
        at: &BlockAt,
        origin: AttemptOrigin,
    ) -> Result<bool, Fatal> {
        // A local clone: an attempt that borrowed `self.facade` would lock out `&mut self`.
        let facade = Arc::clone(&self.facade);
        let mut target = ViewTarget::Active;
        let mut att =
            self.hooks.begin_attempt(&facade, v, cx, AttemptId::next(), origin, &target)?;
        let (_, mut evo) = target.split();
        let out = self.body(&mut att, &mut evo, at, origin == AttemptOrigin::Proposal);
        self.hooks.finish_attempt(att)?;
        match out {
            Ok(true) if origin == AttemptOrigin::Tip => self.hooks.commit_tx(&mut evo)?,
            _ => self.hooks.rollback_tx(&mut evo),
        }
        out
    }

    fn body(
        &mut self,
        att: &mut H::Attempt<'_>,
        evo: &mut EvoTarget<'_>,
        at: &BlockAt,
        just_check: bool,
    ) -> Result<bool, Fatal> {
        let block = Block::new(Header::new(BlockHeader::default()), Vec::new());
        let rules = SpecialTxRules::default();
        self.hooks.begin_tx(att, evo)?;
        self.hooks.verify_best_block(att, evo, &at.hash)?;
        let h4 = self
            .hooks
            .process_special_txs(att, evo, &block, at, rules, &NoCoins, 0, just_check, true)?;
        if h4.is_err() {
            return Ok(false);
        }
        self.hooks.set_best_block(att, evo, &at.hash)?;
        self.hooks.list_changed(att, &H::ListUpdates::default(), false);
        self.hooks.digest_fields(att, evo, at).map(|_| true)
    }
}

/// A block position the shape's attempts take: H4 computes over the fixture, not the rig's chain.
fn block_at(n: u8) -> BlockAt {
    BlockAt {
        hash: BlockHash::from([n; 32]),
        height: u32::from(n),
        prev: None,
        id: None,
    }
}

#[derive(Debug, PartialEq)]
struct Seen {
    ok: bool,
    failed: bool,
    released: [bool; 3],
    held_before_drop: bool,
    gate_free_inside: Vec<bool>,
    rows: usize,
    latched: Option<String>,
}

#[test]
fn a_borrowed_facade_attempt_spans_every_hook_call_and_releases_its_gate() {
    // V's connector is moved onto V's thread: `Facade: Send + Sync` is what makes it `Send`.
    fn on_v_thread<T: Send>() {}
    on_v_thread::<Connector<Ch7aShape>>();

    let rig = Rig::new();
    let result = Arc::new(Mutex::new(None));
    let out = result.clone();
    rig.on_v(move |v| {
        let w = world(&Fixture::rotated().params);
        let mut c = Connector {
            hooks: Ch7aShape::new(&w),
            facade: w.facade.clone(),
        };
        let released = |f: &MembersFacade| f.gate().try_write().is_some();

        let (ok, failed, released_ok, released_err, released_drop, held_before_drop) =
            with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
                let tip = AttemptOrigin::Tip;
                c.hooks.script = vec![at(48)];
                let ok = c.bracket(vcx, cx, &block_at(1), tip)?;
                let released_ok = released(&c.facade);

                // H11's read fails: the attempt still applies its log and releases the gate.
                c.hooks.fail_digest = true;
                c.hooks.script = vec![at(72)];
                let failed = c.bracket(vcx, cx, &block_at(2), tip).is_err();
                let released_err = released(&c.facade);

                // A handle dropped without `finish_attempt` trips the latch, then drops the gate
                // (the unwind's own trip is `AttemptUnwind`'s test, D71).
                let att = c.hooks.begin_attempt(
                    &c.facade,
                    vcx,
                    cx,
                    AttemptId::next(),
                    tip,
                    &ViewTarget::Active,
                )?;
                let held_before_drop = c.facade.gate().try_read().is_none();
                drop(att);
                let released_drop = released(&c.facade);
                Ok((ok, failed, released_ok, released_err, released_drop, held_before_drop))
            })?;

        *out.lock().expect("out") = Some(Seen {
            ok,
            failed,
            released: [released_ok, released_err, released_drop],
            held_before_drop,
            gate_free_inside: c.hooks.gate_free_inside.clone(),
            rows: w.store.rows.lock().len(),
            latched: w.latch.tripped().map(|f| f.message.clone()),
        });
        Ok(())
    });
    let seen = result.lock().expect("result").take().expect("V ran it");
    assert_eq!(
        seen,
        Seen {
            ok: true,
            failed: true,
            released: [true; 3],
            held_before_drop: true,
            // begin_tx, verify_best_block, H4, set_best_block, list_changed, digest_fields and
            // the log application, in each attempt
            gate_free_inside: vec![false; 14],
            // each attempt's row write applied, the failed one's too
            rows: 2,
            latched: Some("an EV attempt dropped without its release".to_owned()),
        }
    );
    rig.assert_no_abort();
}

/// One attempt of the cases: H4's calls, whether H4 then refuses the block, and its origin.
struct Run {
    calls: Vec<Call>,
    refuse: bool,
    origin: AttemptOrigin,
}

/// A tip attempt H4 passes, making `calls`.
fn tip(calls: Vec<Call>) -> Run {
    Run {
        calls,
        refuse: false,
        origin: AttemptOrigin::Tip,
    }
}

/// The rotated type's members-cache entries at every height of the fixture: the base cache's,
/// then each index's.
type Entries = Vec<(u32, Option<Vec<ProTxHash>>, [Option<Vec<ProTxHash>>; 2])>;

/// What the runs left: H4's answers, each attempt's log, then the runtime rows and the
/// members-cache entries after the last release.
#[derive(Debug, PartialEq)]
struct Outcome {
    answers: Vec<Vec<ProTxHash>>,
    logs: Vec<Vec<Effect>>,
    rows: ModelRows,
    entries: Entries,
}

/// The runs, in order, through the shape's connector on the production facade on V.
fn on_facade(runs: Vec<Run>) -> Outcome {
    let rig = Rig::new();
    let result = Arc::new(Mutex::new(None));
    let out = result.clone();
    rig.on_v(move |v| {
        let fx = Fixture::rotated();
        let w = world(&fx.params);
        let mut c = Connector {
            hooks: Ch7aShape::new(&w),
            facade: w.facade.clone(),
        };
        with_tip_cx(v, |vcx, cx| -> Result<(), Fatal> {
            for (n, run) in (1..).zip(runs) {
                c.hooks.script = run.calls;
                c.hooks.refuse = run.refuse;
                let passed = c.bracket(vcx, cx, &block_at(n), run.origin)?;
                assert_eq!(passed, !run.refuse);
            }
            Ok(())
        })?;
        assert!(w.latch.tripped().is_none(), "{:?}", w.latch.tripped());
        let caches = w.facade.test_caches();
        let entries = (0..=TIP)
            .map(|h| {
                let hash = fx.hash(h);
                let indexed = |i| caches.by_cycle_index.peek(ROTATED, &(hash, i)).map(|m| pros(m));
                (h, caches.by_base.peek(ROTATED, &hash).map(|m| pros(m)), [indexed(0), indexed(1)])
            })
            .collect();
        *out.lock().expect("out") = Some(Outcome {
            answers: std::mem::take(&mut c.hooks.answers),
            logs: std::mem::take(&mut c.hooks.logs),
            rows: w.store.rows.lock().clone(),
            entries,
        });
        Ok(())
    });
    rig.assert_no_abort();
    result.lock().expect("result").take().expect("V ran it")
}

/// The same runs on LQ1's model of the facade in its attempt: each call in the attempt's one
/// overlay, or, for case 1's negative control, in an overlay of its own (EV0's old stub).
fn on_model(runs: &[Run], per_call: bool) -> Outcome {
    let fx = Fixture::rotated();
    let mut model = ModelAttempt::<Arc<Dmn>>::new(&fx.params);
    let (mut answers, mut logs) = (Vec::new(), Vec::new());
    fx.ctx(|ctx| {
        for run in runs {
            let id = model.begin().expect("open");
            for call in &run.calls {
                let src = fx.source_with(call.lists.clone());
                let base = fx.at(call.height);
                let got = if per_call {
                    model.per_call_members(id, ctx, &src, call.t, base, call.reset)
                } else {
                    model.attempt_members(id, ctx, &src, call.t, base, call.reset)
                };
                answers.push(pros(&got.expect("held").0));
            }
            logs.push(model.log(id).to_vec());
            model.release(id, &mut |_| false).expect("applied");
        }
    });
    let (base, indexed) = (model.facade().base_cache(), model.facade().indexed_cache());
    let entries = (0..=TIP)
        .map(|h| {
            let hash = fx.hash(h);
            let at = |i| indexed.peek(ROTATED, &(hash, i)).map(|m| pros(m));
            (h, base.peek(ROTATED, &hash).map(|m| pros(m)), [at(0), at(1)])
        })
        .collect();
    Outcome {
        answers,
        logs,
        rows: model.rows().clone(),
        entries,
    }
}

/// The base heights of the rows each log stored, in order.
fn stores(fx: &Fixture, logs: &[Vec<Effect>]) -> Vec<Vec<u32>> {
    logs.iter()
        .map(|log| {
            log.iter()
                .map(|e| match e {
                    Effect::StoreQuorumSnapshot {
                        cycle_base,
                        ..
                    } => (0..=TIP).find(|&h| fx.hash(h) == *cycle_base).expect("a fixture block"),
                    other => panic!("{other:?}"),
                })
                .collect()
        })
        .collect()
}

/// ADD §2.6 case 1: the calls of one attempt share its overlay. Each rotated call reads the
/// cycles before its own, whose rows the calls before it stored in the attempt. The facade
/// answers as the model in its attempt, and a per-call overlay (EV0's old stub) answers
/// otherwise (M23, M25).
#[test]
fn case_1_two_calls_in_one_attempt_share_its_overlay() {
    let runs = || vec![tip(vec![at(24), at(48), at(72), at(96)])];
    let ours = on_facade(runs());
    let model = on_model(&runs(), false);
    assert_eq!(ours, model);
    let per_call = on_model(&runs(), true);
    assert_ne!(per_call.answers, model.answers, "the negative control answers as the case");
    assert_eq!(stores(&Fixture::rotated(), &ours.logs), [vec![24, 48, 72, 96]]);
}

/// ADD §2.6 case 2: two stores to one key in one attempt. A later call reads the second, and
/// both effects are in the log, in order; the second is the runtime row (M24).
#[test]
fn case_2_an_overwrite_is_read_and_both_effects_are_logged() {
    // Masternodes 7-9 leave the lists of cycle 48's work block and its neighbours, so the
    // recomputed row differs.
    let fewer: BTreeMap<u32, Vec<u32>> = (36..=48).map(|h| (h, (1..=6).collect())).collect();
    let runs = || {
        let again = Call {
            reset: true,
            lists: fewer.clone(),
            ..at(48)
        };
        vec![tip(vec![at(24), at(48), again, at(72)])]
    };
    let ours = on_facade(runs());
    assert_eq!(ours, on_model(&runs(), false));
    let fx = Fixture::rotated();
    assert_eq!(stores(&fx, &ours.logs), [vec![24, 48, 48, 72]]);
    let row = |n: usize| match &ours.logs[0][n] {
        Effect::StoreQuorumSnapshot {
            snapshot,
            ..
        } => snapshot.clone(),
        other => panic!("{other:?}"),
    };
    assert_ne!(row(1), row(2), "the recomputed row differs");
    assert_eq!(ours.rows.get(&(ROTATED.to_base(), fx.hash(48))), Some(&row(2)));
}

/// ADD §2.6 case 3: a new attempt starts with an empty overlay, and reads the rows of the one
/// before it from the runtime rows once its log is applied: its log holds its own stores only.
#[test]
fn case_3_a_new_attempt_reads_the_last_ones_rows_from_backing() {
    let runs = || vec![tip(vec![at(24), at(48)]), tip(vec![at(72)])];
    let ours = on_facade(runs());
    assert_eq!(ours, on_model(&runs(), false));
    assert_eq!(stores(&Fixture::rotated(), &ours.logs), [vec![24, 48], vec![72]]);
    // Without the first attempt's rows the second answers otherwise.
    let alone = on_model(&[tip(vec![at(72)])], false);
    assert_ne!(alone.answers[0], ours.answers[2], "the second attempt read the first's rows");
}

/// ADD §2.6 case 4: an attempt that fails after its stores and a check-only proposal attempt
/// still apply their effects, and the members-cache inserts they made survive.
#[test]
fn case_4_a_failed_and_a_check_only_attempt_apply_their_effects() {
    let runs = || {
        vec![
            Run {
                calls: vec![at(24), at(48)],
                refuse: true,
                origin: AttemptOrigin::Tip,
            },
            Run {
                calls: vec![at(72)],
                refuse: false,
                origin: AttemptOrigin::Proposal,
            },
        ]
    };
    let ours = on_facade(runs());
    assert_eq!(ours, on_model(&runs(), false));
    let fx = Fixture::rotated();
    for h in [24, 48, 72] {
        assert!(ours.rows.contains_key(&(ROTATED.to_base(), fx.hash(h))), "row {h}");
        let (_, base, _) = &ours.entries[h as usize];
        assert!(base.is_some(), "the base entry at {h}");
    }
}

/// [`world`] whose facade trips the node's latch, as the node's does.
fn node_world(s: &Stores, params: &ChainParams) -> World {
    let w = world(params);
    let facade = MembersFacade::new(params, w.rows.clone(), s.latch.clone()).expect("in domain");
    World {
        facade: Arc::new(facade),
        latch: s.latch.clone(),
        ..w
    }
}

/// A node over the stores whose connector's evo side is the shape over `w`'s facade: what the
/// tests below submit a block to.
fn node(s: &Stores, w: &World, set: impl FnOnce(&mut Ch7aShape)) -> Rig {
    let mut hooks = Ch7aShape::new(w);
    set(&mut hooks);
    Rig::with(s.config_with_hooks(Seams::default(), hooks, w.facade.clone()))
}

/// What V left after a block whose connect stopped it: the `AbortNode` reports, the node latch's
/// failure, whether a `BlockChecked` was emitted, and the block's status bits.
fn stopped(s: &Stores, mut rig: Rig) -> (Vec<String>, Option<String>, bool, Status) {
    let block = branch(&rig.p, &rig.genesis, 1, 9).remove(0);
    rig.submit_stopping(&block, "a connect that stops V");
    let id = rig.id(&block);
    let checked = rig.rec.take().iter().any(|e| matches!(e, Ev::Checked(..)));
    let exit = rig.stop();
    let status = exit.chain.reader().entry(id).expect("indexed").status;
    (s.sink.messages(), s.latch.tripped().map(|f| f.message.clone()), checked, status)
}

/// ADD §2.6 case 5 on the connect path: a `StoreQuorumSnapshot` whose runtime write fails trips
/// D4. The facade latches the failure before it releases the gate, and V stops on it with one
/// `AbortNode` and no verdict: no `BlockChecked`, no failure mark. The model's release trips its
/// latch once too (M9).
#[test]
fn case_5_a_failed_runtime_write_stops_v_with_one_abort_node() {
    let s = Stores::new();
    let fx = Fixture::rotated();
    let w = node_world(&s, &fx.params);
    w.store.fail_writes.store(true, std::sync::atomic::Ordering::SeqCst);
    let rig = node(&s, &w, |h| h.script = vec![at(48)]);
    let (reports, latched, checked, status) = stopped(&s, rig);
    let fault = StoreError::read("planted write fault").to_string();
    assert_eq!(reports, std::slice::from_ref(&fault), "one report");
    assert_eq!(latched, Some(fault), "the node latch");
    assert!(!checked, "a verdict");
    // ConnectBlock wrote the undo before finish_attempt applied the log, so HAVE_UNDO stays.
    assert!(!status.has(Status::FAILED_VALID), "{status:?}");
    assert!(w.store.rows.lock().is_empty());

    let mut model = ModelAttempt::<Arc<Dmn>>::new(&fx.params);
    let id = model.begin().expect("open");
    fx.ctx(|ctx| model.attempt_members(id, ctx, &fx.source(), ROTATED, fx.at(48), false))
        .expect("held");
    assert!(model.release(id, &mut |_| true).is_err());
    assert_eq!(model.aborts(), 1);
}

/// ADD §2.4 on the connect path: an absent list for a connected block, through the production
/// facade, is a fault, never a verdict. V stops with one `AbortNode`, emits no `BlockChecked` (so
/// no P2P message), marks no block invalid and writes no row. The attempt's empty log applies, so
/// the facade releases it; V's `AbortNode` leaves the node latch open for the shutdown flushes
/// (DEC-241).
#[test]
fn an_absent_connected_list_stops_v_with_one_abort_node() {
    let s = Stores::new();
    let w = node_world(&s, &Fixture::rotated().params);
    let rig = node(&s, &w, |h| {
        h.script = vec![at(48)];
        h.withheld = true;
    });
    let (reports, latched, checked, status) = stopped(&s, rig);
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert!(reports[0].starts_with("quorum block processor: "), "{reports:?}");
    assert_eq!(latched, None, "DEC-241");
    assert!(!checked, "a verdict");
    assert!(!status.has(Status::FAILED_VALID) && !status.has(Status::HAVE_UNDO), "{status:?}");
    assert!(w.store.rows.lock().is_empty());
}
