use dash_consensus::deployments::{Deployments, NoEhfSignals, VersionBitsCache};
use dash_dml::testkit::MemChain;
use dash_evo::BlsCtx;
use dash_evo::llmq::CompleteChain;
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};

use super::*;

fn regtest() -> ChainParams {
    ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().expect("regtest")
}

/// Runs `f` with `NoMembers` over regtest's deployments on a chain `0 ..= tip`.
fn with_stub<R>(tip: u32, f: impl FnOnce(&NoMembers<'_>, &MemChain) -> R) -> R {
    let params = regtest();
    let mut chain = MemChain::new();
    chain.extend(chain.genesis(), tip);
    let cache = VersionBitsCache::new();
    let deployments = Deployments::new(&params, &chain, &NoEhfSignals, &cache);
    let complete = CompleteChain::<StoreError>::new(&chain);
    let llmq = LlmqCtx::new(&params, &complete, &deployments, BlsCtx::BASIC).expect("in domain");
    f(
        &NoMembers {
            llmq,
        },
        &chain,
    )
}

fn refused<T: std::fmt::Debug>(r: Result<T, MemberError<StoreError>>) -> bool {
    matches!(r, Err(MemberError::View(StoreError::NotBuilt(QUORUM_VALIDATION))))
}

/// D2: the precompute refuses only where Core's loop asks the members, a rotated type's cycle
/// base (regtest's `llmq_test_dip0024`, every 24 blocks once DIP0024 is active below the cycle);
/// elsewhere it does nothing, in both modes, and writes no effect.
#[test]
fn the_precompute_refuses_only_at_a_rotation_cycle_base() {
    with_stub(100, |stub, _| {
        let mut bases = Vec::new();
        for h in 1..=100 {
            for mode in [PrecomputeMode::Connect, PrecomputeMode::Undo] {
                let mut effects = Vec::new();
                let r = stub.precompute(BlockId(h), mode, &mut effects);
                assert!(effects.is_empty(), "{h}");
                if refused(r.clone()) {
                    bases.push(h);
                } else {
                    assert_eq!(r, Ok(()), "{h}");
                }
            }
        }
        bases.dedup();
        assert!(!bases.is_empty());
        assert!(bases.iter().all(|h| h % 24 == 0), "{bases:?}");
        assert!(bases.contains(&48) && bases.contains(&96), "{bases:?}");
    });
}

/// D2: every members call refuses, writing nothing.
#[test]
fn every_members_call_refuses() {
    with_stub(60, |stub, _| {
        for (t, base) in [(LlmqType::LlmqTest, 48), (LlmqType::LlmqTestDip0024, 48)] {
            let mut effects = Vec::new();
            assert!(refused(stub.members(t, BlockId(base), &mut effects)));
            assert!(effects.is_empty());
        }
    });
}

/// D53 (DEC-591): below DIP0003, H9's precompute takes `NoMembers`' refusal at a cycle base as
/// done and passes every other outcome through, its fault at genesis (no parent) included.
#[test]
fn the_pre_dip3_precompute_drops_only_the_refusal() {
    with_stub(30, |stub, _| {
        let pre_dip3 = PreDip3Precompute(stub);
        let mut effects = Vec::new();
        assert!(refused(stub.precompute(BlockId(24), PrecomputeMode::Undo, &mut effects)));
        assert_eq!(pre_dip3.precompute(BlockId(24), PrecomputeMode::Undo, &mut effects), Ok(()));
        assert_eq!(pre_dip3.precompute(BlockId(23), PrecomputeMode::Undo, &mut effects), Ok(()));
        let at_genesis = stub.precompute(BlockId(0), PrecomputeMode::Undo, &mut effects);
        assert!(at_genesis.is_err());
        assert_eq!(pre_dip3.precompute(BlockId(0), PrecomputeMode::Undo, &mut effects), at_genesis);
        assert!(effects.is_empty());
    });
}

/// D53: only `QUORUM_VALIDATION`'s refusal is dropped; another capability's passes through.
#[test]
fn the_pre_dip3_precompute_keeps_another_capability() {
    struct Other;
    impl QuorumPrecompute for Other {
        type Error = StoreError;

        fn precompute(
            &self,
            _: BlockId,
            _: PrecomputeMode,
            _: &mut dyn EffectSink,
        ) -> Result<(), MemberError<StoreError>> {
            Err(MemberError::View(StoreError::NotBuilt("another capability")))
        }
    }
    let mut effects = Vec::new();
    let r = PreDip3Precompute(&Other).precompute(BlockId(24), PrecomputeMode::Undo, &mut effects);
    assert_eq!(r, Err(MemberError::View(StoreError::NotBuilt("another capability"))));
}
