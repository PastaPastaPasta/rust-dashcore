//! Platform's lookup (SB0 §5.3, D36) over the regtest capture.

use super::*;
use crate::Lookup;

/// The answer for Core's CL quorum based at `base` at core height `h`, after the step diffs.
fn found(engine: &MasternodeListEngine, chain: &FixtureChain, base: u32, h: u32) -> bool {
    let q = core_quorums(CHAIN_LOCKS).into_iter().find(|q| q.height == base).expect("Core's");
    let hash = QuorumHash::from(q.hash);
    match engine.lookup_quorum(llmq(CHAIN_LOCKS), &hash, h, chain, config()) {
        Ok(Lookup::Found(v)) => {
            assert_eq!(v.quorum_hash(), &hash, "never another quorum's key");
            let key = v.public_key().to_bytes(dash_pkc::bls::BlsSchemeKind::Basic);
            assert_eq!(key.as_slice(), q.public_key.as_slice(), "Core's key");
            true
        }
        Ok(_) => false,
        Err(e) => panic!("{e}"),
    }
}

/// D36: the rows answer by SB0 §5.3's predicate. Of Core's CL quorums at tip 311, 144's was
/// replaced at 275 and is found at 311 (275 > 311 − W_P); 288's, mined above 290, is found at
/// 311 and not at 290; 168's, replaced at 299, is found at 290; no quorum is found above the
/// header tip, nor at its base block, before it was mined.
#[test]
fn cores_quorums_are_found_by_the_predicate() {
    let chain = FixtureChain::full();
    let engine = stepped(&chain);
    assert!(found(&engine, &chain, 144, 311));
    assert!(found(&engine, &chain, 288, 311));
    assert!(!found(&engine, &chain, 288, 290));
    assert!(found(&engine, &chain, 168, 290));
    for base in [144, 168, 264, 288] {
        assert!(!found(&engine, &chain, base, 312), "{base}");
        assert!(!found(&engine, &chain, base, base), "{base}");
    }
}
