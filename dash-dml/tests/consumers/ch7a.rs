//! CH7a (evo integration in the chainstate) against EV0: one connect attempt, its effect log
//! applied in order regardless of the result, its delta written only on success.

use dash_consensus::{BlockValidationError, BlockValidationResult};
use dash_dml::testkit::MemChain;
use dash_dml::{EvoDelta, EvoStore, MemEvoStore, StoreError};
use dash_evo::EffectSink;
use dash_evo::effect::{Effect, EffectKind};
use dash_evo::llmq::QuorumSnapshot;
use dash_params::LlmqType;
use dash_primitives::BlockHash;

/// Where effects that are not evo rows go (`dashd-locks`, `dashd-quorums`, `dashd-masternode`,
/// the event bus, the tip BLS context).
#[derive(Default)]
pub struct OtherSubsystems {
    pub routed: Vec<Effect>,
}

/// CH7a's router: every variant is matched, so a new effect does not compile until it is routed.
pub fn apply_effect(store: &mut MemEvoStore, others: &mut OtherSubsystems, effect: Effect) {
    match effect {
        Effect::StoreQuorumSnapshot {
            llmq_type,
            cycle_base,
            snapshot,
        } => {
            store.store_quorum_snapshot(llmq_type, cycle_base, snapshot);
        }
        e @ (Effect::RemoveConflictingIsLock {
            ..
        }
        | Effect::EraseMineableCommitment {
            ..
        }
        | Effect::ReinsertMineableCommitment {
            ..
        }
        | Effect::ResetPlatformBan {
            ..
        }
        | Effect::NotifyMnListChanged {
            ..
        }) => others.routed.push(e),
    }
}

/// ST3a §2.4.4: the presumed class needs a `sync_wal` before B1 while one is pending. An
/// exhaustive match with no wildcard, so a new kind must be classified deliberately.
pub fn is_presumed(kind: EffectKind) -> bool {
    match kind {
        EffectKind::StoreQuorumSnapshot => true,
        EffectKind::RemoveConflictingIsLock
        | EffectKind::EraseMineableCommitment
        | EffectKind::ReinsertMineableCommitment
        | EffectKind::ResetPlatformBan
        | EffectKind::NotifyMnListChanged => false,
    }
}

/// The connect error classes CH7a distinguishes.
#[derive(Debug)]
pub enum ConnectError {
    Invalid(BlockValidationError),
    Node(StoreError),
}

/// One connect attempt: `body` runs the pure functions with the attempt's sink. The log is
/// applied in emission order **before** the result is acted on, whatever it is (A1 §2.10.1).
pub fn connect_attempt(
    store: &mut MemEvoStore,
    others: &mut OtherSubsystems,
    just_check: bool,
    body: impl FnOnce(&mut dyn EffectSink) -> Result<EvoDelta, BlockValidationError>,
) -> Result<(), ConnectError> {
    let mut log: Vec<Effect> = Vec::new();
    let result = body(&mut log);
    for effect in log {
        apply_effect(store, others, effect);
    }
    let delta = result.map_err(ConnectError::Invalid)?;
    if just_check {
        return Ok(()); // the overlay is dropped; the effects stay applied
    }
    store.apply(&delta).map_err(ConnectError::Node)
}

#[test]
fn effects_survive_a_failed_attempt_and_the_delta_does_not() {
    let mut chain = MemChain::new();
    let b1 = chain.child(chain.genesis());
    let mut store = MemEvoStore::new();
    let mut others = OtherSubsystems::default();
    let base = BlockHash::from([3u8; 32]);
    let t = LlmqType::LlmqTestDip0024;
    let snapshot = QuorumSnapshot {
        skip_list_mode: 3,
        ..Default::default()
    };

    let result = connect_attempt(&mut store, &mut others, false, |effects| {
        effects.emit(Effect::StoreQuorumSnapshot {
            llmq_type: t,
            cycle_base: base,
            snapshot: snapshot.clone(),
        });
        // VerifyDB level 3's disconnect notifies with the undo direction (API review M3).
        effects.emit(Effect::NotifyMnListChanged {
            at: base,
            undo: true,
        });
        Err(BlockValidationError::new(BlockValidationResult::Consensus, "bad-qc-invalid"))
    });
    assert!(matches!(result, Err(ConnectError::Invalid(_))));
    let view = store.view(&chain);
    assert_eq!(view.quorum_snapshot(t, &base).unwrap().as_deref(), Some(&snapshot));
    assert_eq!(view.best_block().unwrap(), None);
    assert_eq!(
        others.routed,
        [Effect::NotifyMnListChanged {
            at: base,
            undo: true
        }]
    );
    assert_eq!(EffectKind::ALL.iter().filter(|k| is_presumed(**k)).count(), 1);

    // A just-check attempt keeps its effects and writes no delta; a real one writes it.
    let delta = super::empty_delta(&chain, b1);
    connect_attempt(&mut store, &mut others, true, |_| Ok(delta.clone())).unwrap();
    assert_eq!(store.view(&chain).best_block().unwrap(), None);
    connect_attempt(&mut store, &mut others, false, |_| Ok(delta.clone())).unwrap();
    assert_eq!(store.view(&chain).best_block().unwrap(), Some(chain.hash(b1)));
}
