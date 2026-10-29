//! EV addendum §2.6's attempt against [`EvoHooks`] (CH3a D56): hooks shaped as CH7a's will be,
//! over a stub of CH7b's `MembersFacade` whose `begin_attempt`, `AttemptHandle` and
//! `release_attempt` have §2.6's signatures, driven by a connector shaped as `CoreConnector` on
//! a real V loop. That it compiles is the point: hooks that own the facade cannot return its
//! handle, and a `&'v mut self` opener blocks every later hook call (Sol a2 r1). Running it shows
//! the gate held exclusively for the whole attempt, log application included, and released on
//! every path.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, RwLock, RwLockWriteGuard};

use dash_consensus::coins::CoinsView;
use dash_consensus::ctx::bls_ctx_for_block;
use dash_consensus::payments::MnRewardEra;
use dash_consensus::{Amount, Block, BlockId, BlockValidationError, BlsCtx, Tx, TxValidationError};
use dash_evo::llmq::QuorumSnapshot;
use dash_evo::{Effect, EffectSink};
use dash_params::ChainParams;
use dash_primitives::{BlockHash, LlmqType};
use dash_types::Numeric as _;

use super::support::Rig;
use crate::chain::{AttemptOrigin, EvoTarget, Fatal, VContext, ViewTarget};
use crate::connect::{AttemptId, BlockAt, EvoAttempt, EvoDigest, EvoHooks, SpecialTxRules};
use crate::store::CoinsReadError;

/// CH7b's facade with §2.6's attempt signatures, and only the gate behind them.
struct MembersFacade {
    gate: RwLock<()>,
}

impl MembersFacade {
    fn begin_attempt<'v>(&'v self, _v: &'v mut VContext, id: AttemptId) -> AttemptHandle<'v> {
        AttemptHandle {
            facade: self,
            id,
            _gate: self.gate.write().expect("an unpoisoned gate"),
            _v_only: PhantomData,
            overlay: RefCell::default(),
        }
    }

    fn release_attempt(&self, h: AttemptHandle<'_>, applied: EffectsApplied) {
        assert!(std::ptr::eq(h.facade, self), "released through its own facade");
        assert_eq!(applied.attempt, h.id, "after its own log is applied");
    }
}

struct AttemptHandle<'f> {
    facade: &'f MembersFacade,
    id: AttemptId,
    _gate: RwLockWriteGuard<'f, ()>,
    _v_only: PhantomData<*const ()>,
    // last write wins; keyed by the base type, as LQ1a's `ModelRows` (`LlmqType` is not `Ord`)
    overlay: RefCell<BTreeMap<(u8, BlockHash), QuorumSnapshot>>,
}

impl AttemptHandle<'_> {
    /// A row write inside the attempt (`AttemptRows::store`): the overlay, and the attempt's log.
    fn store(&self, log: &mut dyn EffectSink, t: LlmqType, base: BlockHash, row: QuorumSnapshot) {
        self.overlay.borrow_mut().insert((t.to_base(), base), row.clone());
        log.emit(Effect::StoreQuorumSnapshot {
            llmq_type: t,
            cycle_base: base,
            snapshot: row,
        });
    }
}

struct EffectsApplied {
    attempt: AttemptId,
}

/// Hooks shaped as CH7a's: they own their state (here the runtime rows the log is applied to)
/// and never the facade.
#[derive(Default)]
struct Ch7aShape {
    rows: BTreeMap<(u8, BlockHash), QuorumSnapshot>,
    /// Per hook call inside an attempt, and per log application: could an external route have
    /// taken the gate?
    gate_free_inside: Vec<bool>,
    /// H11's read fails, as an evo store read can.
    fail_digest: bool,
}

/// CH7a's attempt keeps the facade it releases through: `AttemptHandle::facade` is CH7b's.
struct Ch7aAttempt<'v> {
    facade: &'v MembersFacade,
    handle: AttemptHandle<'v>,
    log: Vec<Effect>,
}

impl EvoAttempt for Ch7aAttempt<'_> {
    fn id(&self) -> AttemptId {
        self.handle.id
    }

    fn sink(&mut self) -> &mut dyn EffectSink {
        &mut self.log
    }
}

impl Ch7aShape {
    fn note_gate(&mut self, facade: &MembersFacade) {
        self.gate_free_inside.push(facade.gate.try_read().is_ok());
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

    /// CH7a's application: the whole log in order under the exclusive gate, then the release.
    fn finish_attempt(&mut self, attempt: Ch7aAttempt<'_>) -> Result<(), Fatal> {
        let Ch7aAttempt {
            facade,
            handle,
            log,
        } = attempt;
        self.note_gate(facade);
        for effect in log {
            let Effect::StoreQuorumSnapshot {
                llmq_type,
                cycle_base,
                snapshot,
            } = effect
            else {
                return Err(Fatal::new(format!("no applier for a {:?} effect", effect.kind())));
            };
            self.rows.insert((llmq_type.to_base(), cycle_base), snapshot);
        }
        let applied = EffectsApplied {
            attempt: handle.id,
        };
        facade.release_attempt(handle, applied);
        Ok(())
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

    fn process_special_txs(
        &mut self,
        _: &mut Ch7aAttempt<'_>,
        _: &mut EvoTarget<'_>,
        _: &Block,
        _: &BlockAt,
        _: SpecialTxRules,
        _: &dyn CoinsView<Error = CoinsReadError>,
        _: Amount,
        _: bool,
        _: bool,
    ) -> Result<Result<(), BlockValidationError>, Fatal> {
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

    /// Stands in for a members call's row write (H4's, in CH7a), through the open handle.
    fn set_best_block(
        &mut self,
        attempt: &mut Ch7aAttempt<'_>,
        _: &mut EvoTarget<'_>,
        hash: &BlockHash,
    ) -> Result<(), Fatal> {
        self.note_gate(attempt.facade);
        let row = QuorumSnapshot::default();
        attempt.handle.store(&mut attempt.log, LlmqType::LlmqTestDip0024, *hash, row);
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
    fn bracket(&mut self, v: &mut VContext, at: &BlockAt) -> Result<EvoDigest, Fatal> {
        // A local clone: an attempt that borrowed `self.facade` would lock out `&mut self`.
        let facade = Arc::clone(&self.facade);
        let mut target = ViewTarget::Active;
        let mut att =
            self.hooks.begin_attempt(&facade, v, AttemptId::next(), AttemptOrigin::Tip, &target)?;
        let (_, mut evo) = target.split();
        let out = self.body(&mut att, &mut evo, at);
        self.hooks.finish_attempt(att)?;
        match out {
            Ok(_) => self.hooks.commit_tx(&mut evo)?,
            Err(_) => self.hooks.rollback_tx(&mut evo),
        }
        out
    }

    fn body(
        &mut self,
        att: &mut H::Attempt<'_>,
        evo: &mut EvoTarget<'_>,
        at: &BlockAt,
    ) -> Result<EvoDigest, Fatal> {
        self.hooks.begin_tx(att, evo)?;
        self.hooks.verify_best_block(att, evo, &at.hash)?;
        self.hooks.set_best_block(att, evo, &at.hash)?;
        self.hooks.list_changed(att, &H::ListUpdates::default(), false);
        self.hooks.digest_fields(att, evo, at)
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
        let mut c = Connector {
            hooks: Ch7aShape::default(),
            facade: Arc::new(MembersFacade {
                gate: RwLock::new(()),
            }),
        };
        let released = |f: &MembersFacade| f.gate.try_write().is_ok();
        let at = |n| BlockAt {
            hash: BlockHash::from([n; 32]),
            height: u32::from(n),
            prev: None,
            id: None,
        };

        let ok = c.bracket(&mut v.vcx, &at(1)).is_ok();
        let released_ok = released(&c.facade);

        // H11's read fails: the attempt still applies its log and releases the gate.
        c.hooks.fail_digest = true;
        let failed = c.bracket(&mut v.vcx, &at(2)).is_err();
        let released_err = released(&c.facade);

        // A handle dropped without `finish_attempt` still drops the gate; the unwind's latch trip
        // is `AttemptUnwind`'s test (D71).
        let att = c.hooks.begin_attempt(
            &c.facade,
            &mut v.vcx,
            AttemptId::next(),
            AttemptOrigin::Tip,
            &ViewTarget::Active,
        )?;
        let held_before_drop = c.facade.gate.try_read().is_err();
        drop(att);
        let released_drop = released(&c.facade);

        *out.lock().expect("out") = Some(Seen {
            ok,
            failed,
            released: [released_ok, released_err, released_drop],
            held_before_drop,
            gate_free_inside: c.hooks.gate_free_inside.clone(),
            rows: c.hooks.rows.len(),
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
            // begin_tx, verify_best_block, set_best_block, list_changed, digest_fields and the
            // log application, in each attempt
            gate_free_inside: vec![false; 12],
            // each attempt's row write applied, the failed one's too
            rows: 2,
        }
    );
    rig.assert_no_abort();
}
