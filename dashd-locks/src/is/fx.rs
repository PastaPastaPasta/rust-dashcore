//! [`IsFxSource`]: the `DigestLog` [`FxSource`] that renders an attempt's InstantSend removals as
//! `fx`'s `is_rm` entries (S1f; PLAN-LK1b §5.3 item 7, O8 §6).
//!
//! The chainstate side it reads, which S1.0b froze for the other lanes:
//!
//! * [`FinishReport`](dashd_chainstate::connect::FinishReport) and
//!   [`EffectCoverage`] carry the finished attempt's log and how much of it applied;
//! * [`ConnectAttempt::finish`] is that `&FinishReport`;
//! * [`FxSource::take`] takes the `&ConnectAttempt` and renders its `fx`.
//!
//! `is_rm` is `["is_rm", lockHash, overridingTxid]` (O8 §6), the txid being the block's
//! transaction that overrides the lock. Core removes the lock in `ConnectBlock`'s IS loop
//! (`validation.cpp:2800-2817`, `evo/chainhelper.cpp:93-99`, `instantsend/instantsend.cpp:342-353`
//! at v24.0.0).

use std::sync::Arc;

use dash_evo::Effect;
use dash_statedigest::{Effect as FxEffect, Hash256};
use dashd_chainstate::connect::{ConnectAttempt, EffectCoverage, FxSource};
use parking_lot::Mutex;

use super::seams::{IsFxLedger, IsObservation, LedgerTake};

/// The attempt ledger's `fx`: it takes the ledger's observations on every `C` and `R`, and
/// renders them only when the whole attempt is known to have committed them.
pub struct IsFxSource(pub Arc<Mutex<dyn IsFxLedger>>);

impl FxSource for IsFxSource {
    /// Always consumes the ledger. `Some` only when all of these hold, else `None`:
    /// - the finish succeeded and the outcome is not an `Err`. On an `Err` outcome Core's
    ///   `ConnectBlock` threw, and O8's `AttemptScope::Threw` still writes its `R` with every
    ///   effect tapped before the throw, earlier `is_rm` entries included. LK1b-s leaves `fx`
    ///   absent there and after a failed finish, even for a removal that committed: the
    ///   comparator counts that as uncovered, not as a mismatch;
    /// - every emitted effect applied ([`EffectCoverage::Known`] with `completed == emitted.len()`)
    ///   and has a renderer here (only `RemoveConflictingIsLock` has one);
    /// - the removals emitted, the ledger's observations and its confirmations are one sequence,
    ///   in order, with no failed confirmation.
    ///
    /// A removal whose root was no longer stored renders no entry (O8 §6), so an attempt whose
    /// log is empty, or holds only such removals, renders `[]`.
    fn take(&mut self, attempt: &ConnectAttempt<'_>) -> Option<Vec<FxEffect>> {
        let taken = self.0.lock().take();
        render(attempt, taken)
    }
}

fn render(attempt: &ConnectAttempt<'_>, taken: LedgerTake) -> Option<Vec<FxEffect>> {
    if attempt.finish.result.is_err() || attempt.outcome.is_err() {
        return None;
    }
    let EffectCoverage::Known {
        emitted,
        completed,
    } = &attempt.finish.effects
    else {
        return None;
    };
    if *completed != emitted.len() || taken.failed_at.is_some() {
        return None;
    }
    let removed = emitted
        .iter()
        .map(|e| match e {
            Effect::RemoveConflictingIsLock {
                lock_hash,
            } => Some(*lock_hash.as_bytes()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let LedgerTake {
        observations,
        confirmed,
        ..
    } = taken;
    let matches = |o: &IsObservation, (r, c): (&[u8; 32], &[u8; 32])| o.lock_hash == *r && *c == *r;
    let same = removed.len() == observations.len()
        && confirmed.len() == observations.len()
        && observations.iter().zip(removed.iter().zip(&confirmed)).all(|(o, rc)| matches(o, rc));
    if !same {
        return None;
    }
    let fx = observations
        .iter()
        .filter(|o| o.root_stored)
        .map(|o| FxEffect::IsRm {
            islock: Hash256::from_wire_bytes(o.lock_hash),
            txid: Hash256::from_wire_bytes(*o.overriding_txid.as_bytes()),
        })
        .collect();
    Some(fx)
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use std::sync::Arc;

    use dash_consensus::primitives::TxHash;
    use dash_consensus::{Block, BlockValidationError, BlockValidationResult, Header};
    use dash_evo::Effect;
    use dash_num::Hash256 as EvoHash;
    use dash_primitives::{BlockHash, BlockHeader};
    use dash_statedigest::{Effect as FxEffect, Hash256};
    use dashd_chainstate::chain::{ConnectOutcome, Fatal};
    use dashd_chainstate::connect::{BlockAt, ConnectAttempt, FinishReport, FxSource};
    use parking_lot::Mutex;

    use super::IsFxSource;
    use crate::is::IsLockHash;
    use crate::is::seams::{
        InMemoryIsStore, IsFxLedger, IsObservation, IsOverlay, IsStore, IsStoreCall, LedgerTake,
        StageId,
    };

    /// A ledger as §5.3 items 4 and 5 run it: observations in push order, a confirmation cursor
    /// that commits each observation's stage through the store, and the failed position.
    #[derive(Default)]
    struct Ledger {
        overlay: IsOverlay,
        observations: Vec<IsObservation>,
        confirmed: Vec<IsLockHash>,
        failed_at: Option<usize>,
    }

    impl IsFxLedger for Ledger {
        fn push(&mut self, observation: IsObservation) {
            self.observations.push(observation);
        }

        fn confirm_next(&mut self, hash: &IsLockHash, store: &dyn IsStore) -> Result<(), Fatal> {
            let at = self.confirmed.len();
            let committed = match self.observations.get(at) {
                Some(o) if o.lock_hash == *hash => {
                    store.commit_staged(&mut self.overlay, o.stage).map_err(|e| e.to_string())
                }
                _ => Err("IS removal effect without its staged removal".to_owned()),
            };
            match committed {
                Ok(()) => {
                    self.confirmed.push(*hash);
                    Ok(())
                }
                Err(e) => {
                    self.failed_at = Some(at);
                    Err(Fatal::new(e))
                }
            }
        }

        fn overlay(&mut self) -> &mut IsOverlay {
            &mut self.overlay
        }

        fn take(&mut self) -> LedgerTake {
            LedgerTake {
                observations: std::mem::take(&mut self.observations),
                confirmed: std::mem::take(&mut self.confirmed),
                failed_at: self.failed_at.take(),
            }
        }

        fn clear(&mut self) {
            self.take();
        }
    }

    fn txid(n: u8) -> TxHash {
        TxHash::from(std::array::from_fn::<u8, 32, _>(|i| n.wrapping_add(i as u8)))
    }

    fn lock(n: u8) -> IsLockHash {
        std::array::from_fn(|i| n.wrapping_mul(3).wrapping_add(i as u8))
    }

    fn removal(n: u8) -> Effect {
        Effect::RemoveConflictingIsLock {
            lock_hash: EvoHash::from(lock(n)),
        }
    }

    fn observed(n: u8, root_stored: bool) -> IsObservation {
        IsObservation {
            lock_hash: lock(n),
            overriding_txid: txid(n + 100),
            locked_txid: txid(n),
            archive_height: 7,
            root_stored,
            removed: vec![lock(n)],
            stage: StageId(u64::from(n)),
        }
    }

    fn is_rm(n: u8) -> FxEffect {
        FxEffect::IsRm {
            islock: Hash256::from_wire_bytes(lock(n)),
            txid: Hash256::from_wire_bytes(*txid(n + 100).as_bytes()),
        }
    }

    type Shared = Arc<Mutex<Ledger>>;

    /// A ledger holding `observations`, each pushed and confirmed through `store`.
    fn confirmed(store: &InMemoryIsStore, observations: Vec<IsObservation>) -> Shared {
        let ledger = Arc::new(Mutex::new(Ledger::default()));
        for o in observations {
            let hash = o.lock_hash;
            let mut l = ledger.lock();
            l.push(o);
            l.confirm_next(&hash, store).expect("the commit succeeds");
        }
        ledger
    }

    fn source(ledger: &Shared) -> IsFxSource {
        let ledger: Arc<Mutex<dyn IsFxLedger>> = ledger.clone();
        IsFxSource(ledger)
    }

    /// `take` for an attempt with `finish` and `outcome`, on a throwaway block.
    fn take(
        source: &mut IsFxSource,
        finish: &FinishReport,
        outcome: &Result<ConnectOutcome, Fatal>,
    ) -> Option<Vec<FxEffect>> {
        let block = Block::new(Header::new(BlockHeader::default()), vec![]);
        let at = BlockAt {
            hash: BlockHash::from([5; 32]),
            height: 5,
            prev: None,
            id: None,
        };
        source.take(&ConnectAttempt {
            block: &block,
            at: &at,
            just_check: false,
            active: true,
            outcome,
            undo: None,
            subsidy: None,
            script_checks: None,
            evo: None,
            finish,
        })
    }

    const CONNECTED: Result<ConnectOutcome, Fatal> = Ok(ConnectOutcome::Connected);

    fn applied(emitted: Vec<Effect>) -> FinishReport {
        let n = emitted.len();
        FinishReport::known(Ok(()), emitted, n)
    }

    /// Only IS removals: `fx` is their `is_rm` entries in emission order, an unstored root renders
    /// none, and the ledger is consumed.
    #[test]
    fn is_removals_render_in_emission_order_without_unstored_roots() {
        let store = InMemoryIsStore::default();
        let ledger =
            confirmed(&store, vec![observed(1, true), observed(2, false), observed(3, true)]);
        let mut fx = source(&ledger);
        let finish = applied(vec![removal(1), removal(2), removal(3)]);
        assert_eq!(take(&mut fx, &finish, &CONNECTED), Some(vec![is_rm(1), is_rm(3)]));
        assert_eq!(ledger.lock().take(), LedgerTake::default(), "consumed");
        let commits: Vec<_> = (1..=3).map(|k| IsStoreCall::CommitStaged(StageId(k))).collect();
        assert_eq!(store.calls(), commits);
    }

    /// An emitted effect with no renderer leaves `fx` absent.
    #[test]
    fn an_unrendered_evo_effect_leaves_fx_absent() {
        let store = InMemoryIsStore::default();
        let ledger = confirmed(&store, vec![observed(1, true)]);
        let notify = Effect::NotifyMnListChanged {
            at: BlockHash::from([9; 32]),
            undo: false,
        };
        let finish = applied(vec![notify, removal(1)]);
        assert_eq!(take(&mut source(&ledger), &finish, &CONNECTED), None);
        assert_eq!(ledger.lock().take(), LedgerTake::default(), "consumed");
    }

    /// An empty log renders `[]`, not an absent `fx`.
    #[test]
    fn an_empty_log_renders_an_empty_fx() {
        let ledger = Arc::new(Mutex::new(Ledger::default()));
        let finish = applied(vec![]);
        assert_eq!(take(&mut source(&ledger), &finish, &CONNECTED), Some(vec![]));
    }

    /// r2c: the whole log applied and its removal committed, then the release failed: the finish
    /// is `Err` with full coverage, and the source consumes the observation and renders nothing.
    /// Its twin with the release succeeding renders the `is_rm`.
    #[test]
    fn a_failed_release_renders_nothing_and_its_twin_renders_the_removal() {
        for released in [false, true] {
            let store = InMemoryIsStore::default();
            let ledger = confirmed(&store, vec![observed(1, true)]);
            let result = if released {
                Ok(())
            } else {
                Err(Fatal::new("EV attempt 1 released with attempt 2's effects"))
            };
            let finish = FinishReport::known(result, vec![removal(1)], 1);
            let want = released.then(|| vec![is_rm(1)]);
            assert_eq!(take(&mut source(&ledger), &finish, &CONNECTED), want, "{released}");
            assert_eq!(ledger.lock().take(), LedgerTake::default(), "{released}: consumed");
            assert_eq!(store.calls(), [IsStoreCall::CommitStaged(StageId(1))], "{released}");
        }
    }

    /// r2d: a body `Err` with an `Ok` finish renders nothing, even for a committed removal.
    #[test]
    fn an_err_outcome_renders_nothing() {
        let store = InMemoryIsStore::default();
        let ledger = confirmed(&store, vec![observed(1, true)]);
        let finish = applied(vec![removal(1)]);
        let thrown = Err(Fatal::new("IS store read failed"));
        assert_eq!(take(&mut source(&ledger), &finish, &thrown), None);
        assert_eq!(ledger.lock().take(), LedgerTake::default(), "consumed");
    }

    /// r2e F3-N3: a G2 readiness refusal after the IS loop is `Ok(SystemError)`, not a failure:
    /// the committed removal is rendered. So is an invalid block's (`bad-cb-payee`).
    #[test]
    fn a_system_error_or_invalid_outcome_keeps_its_removal() {
        let refused = ConnectOutcome::SystemError("not ready (G2)".to_owned());
        let payee = BlockValidationError::new(BlockValidationResult::Unset, "bad-cb-payee");
        for outcome in [refused, ConnectOutcome::Invalid(payee)] {
            let store = InMemoryIsStore::default();
            let ledger = confirmed(&store, vec![observed(1, true)]);
            let finish = applied(vec![removal(1)]);
            let got = take(&mut source(&ledger), &finish, &Ok(outcome.clone()));
            assert_eq!(got, Some(vec![is_rm(1)]), "{outcome:?}");
        }
    }

    /// Unknown coverage (`Ch7aShape`'s default), a partial application, a failed confirmation, and
    /// a count or order mismatch each leave `fx` absent.
    #[test]
    fn any_gap_between_the_log_and_the_ledger_leaves_fx_absent() {
        let store = InMemoryIsStore::default();
        let cases: Vec<(&str, Vec<IsObservation>, FinishReport, Option<usize>)> = vec![
            ("unknown", vec![observed(1, true)], FinishReport::unknown(Ok(())), None),
            (
                "partial",
                vec![observed(1, true), observed(2, true)],
                FinishReport::known(Ok(()), vec![removal(1), removal(2)], 1),
                None,
            ),
            ("failed confirm", vec![observed(1, true)], applied(vec![removal(1)]), Some(0)),
            ("extra removal", vec![observed(1, true)], applied(vec![removal(1), removal(2)]), None),
            (
                "order",
                vec![observed(2, true), observed(1, true)],
                applied(vec![removal(1), removal(2)]),
                None,
            ),
        ];
        for (name, observations, finish, failed_at) in cases {
            let ledger = confirmed(&store, observations);
            ledger.lock().failed_at = failed_at;
            assert_eq!(take(&mut source(&ledger), &finish, &CONNECTED), None, "{name}");
            assert_eq!(ledger.lock().take(), LedgerTake::default(), "{name}: consumed");
        }
    }

    /// An unconfirmed observation (the applier never reached it) renders nothing.
    #[test]
    fn an_unconfirmed_observation_renders_nothing() {
        let ledger = Arc::new(Mutex::new(Ledger::default()));
        ledger.lock().push(observed(1, true));
        let finish = applied(vec![removal(1)]);
        assert_eq!(take(&mut source(&ledger), &finish, &CONNECTED), None);
    }

    /// `is_rm`'s hashes are in display order (O8 §6): the wire bytes reversed, as Core's
    /// `uint256::ToString` prints them, checked against hex built here byte by byte.
    #[test]
    fn is_rm_hashes_are_in_display_order() {
        let store = InMemoryIsStore::default();
        let ledger = confirmed(&store, vec![observed(1, true)]);
        let fx = take(&mut source(&ledger), &applied(vec![removal(1)]), &CONNECTED);
        let shown = |wire: [u8; 32]| {
            let hex: String = wire.iter().rev().map(|b| format!("{b:02x}")).collect();
            Hash256::from_hex(hex.as_bytes()).expect("hex")
        };
        let want = FxEffect::IsRm {
            islock: shown(lock(1)),
            txid: shown(*txid(101).as_bytes()),
        };
        assert_eq!(fx, Some(vec![want]));
    }

    /// The rendered entries round-trip through `fx`'s value form.
    #[test]
    fn rendered_entries_round_trip() {
        let store = InMemoryIsStore::default();
        let ledger = confirmed(&store, vec![observed(1, true), observed(4, true)]);
        let finish = applied(vec![removal(1), removal(4)]);
        let fx = take(&mut source(&ledger), &finish, &CONNECTED).expect("rendered");
        assert_eq!(FxEffect::list_from_value(&FxEffect::list_to_value(&fx)), Ok(fx));
    }
}
