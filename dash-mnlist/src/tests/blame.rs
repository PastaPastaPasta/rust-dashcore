//! A refusal's blame (D58): the peer is blamed only for what its payload carried.

use dash_evo::cbtx::CbTxProofError;

use super::*;
use crate::{Blame, MnReject, MnViewError};

/// Every refusal by its cause: a payload's own fault blames the peer; a race with retention or
/// the header chain, a deferred engine, the host's configuration and a view miss are local; a
/// failed store read is storage's.
#[test]
fn refusals_blame_only_their_actual_cause() {
    let hash = BlockHash::from([1; 32]);
    let table = [
        (MnReject::BadCoinbase("bad"), Blame::Payload),
        (MnReject::CbTxProof(CbTxProofError::Malformed), Blame::Payload),
        (MnReject::CbTxProof(CbTxProofError::RootMismatch), Blame::Payload),
        (MnReject::CbTxProof(CbTxProofError::NotTheCoinbase), Blame::Payload),
        (MnReject::MnListRoot, Blame::Payload),
        (MnReject::QuorumsRoot, Blame::Payload),
        (MnReject::QrInfo("bad row"), Blame::Payload),
        (MnReject::UnknownTarget(hash), Blame::Local),
        (MnReject::UnknownBase(hash), Blame::Local),
        (MnReject::Deferred, Blame::Local),
        (MnReject::Config("no rotated type is configured"), Blame::Local),
        (
            MnReject::View(MnViewError::BelowFloor {
                height: 1,
            }),
            Blame::Local,
        ),
        (
            MnReject::View(MnViewError::RowNotHeld {
                llmq_type: 1,
                cycle_base: hash,
            }),
            Blame::Local,
        ),
        (
            MnReject::View(MnViewError::Storage(dash_chain::StoreError::Io("x".into()))),
            Blame::Storage,
        ),
    ];
    for (reject, blame) in table {
        assert_eq!(reject.blame(), blame, "{reject:?}");
    }
}

/// An honest race: the host asks its tip diff from base B (281), a reorg's retention releases
/// B's list while the reply is out, and Core's reply then refuses as `UnknownBase`: local, not
/// the peer's fault.
#[test]
fn an_honest_unknown_base_from_a_retention_race_is_local() {
    let mut chain = FixtureChain {
        fork: Some(250),
        ..FixtureChain::full()
    };
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&read_diff(&genesis_diff(281)), &chain, config()).expect("Core's diff");
    let base = (281, chain.hash(281));
    assert_eq!(engine.tip_diff_base(&chain), Ok(Some(base)), "asked from B");
    chain.fork_active = true;
    engine.tip_changed(&chain, config()).expect("retention");
    chain.fork_active = false;
    engine.tip_changed(&chain, config()).expect("retention");
    assert_eq!(engine.tip_diff_base(&chain), Ok(None), "B's list went");
    let reply = step_diff(282);
    assert_eq!(reply.base_block_hash(), base.1);
    let refusal = engine.apply_diff(&read_diff(&reply), &chain, config()).expect_err("B went");
    assert_eq!(refusal, MnReject::UnknownBase(base.1));
    assert_eq!(refusal.blame(), Blame::Local);
}
