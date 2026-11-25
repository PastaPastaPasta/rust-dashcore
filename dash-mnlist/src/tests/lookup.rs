//! Platform's lookup (SB0 §5.3, D36) over the regtest capture.

use super::*;
use crate::Lookup;
use crate::convert::commitment_hash;
use dash_primitives::Commitment;
use dash_types::codec::BaseCodec;

/// The answer for Core's CL quorum based at `base` at core height `h`, after the step diffs.
fn found(engine: &MasternodeListEngine, chain: &FixtureChain, base: u32, h: u32) -> bool {
    let q = core_quorums(CHAIN_LOCKS).into_iter().find(|q| q.height == base).expect("Core's");
    let hash = QuorumHash::from(q.hash);
    match engine.lookup_quorum(llmq(CHAIN_LOCKS), &hash, h, chain, config()) {
        Ok(Lookup::Found(v)) => {
            assert_eq!(v.quorum.quorum_hash(), &hash, "never another quorum's key");
            let key = v.quorum.public_key().to_bytes(dash_pkc::bls::BlsSchemeKind::Basic);
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

/// Core's CL quorum based at 288: its hash, and the held commitment with its state.
fn cores_288(engine: &MasternodeListEngine) -> (QuorumHash, Arc<Commitment>, QuorumState) {
    let q = core_quorums(CHAIN_LOCKS).into_iter().find(|q| q.height == 288).expect("Core's");
    let hash = QuorumHash::from(q.hash);
    let (record, state) = engine.quorum(llmq(CHAIN_LOCKS), &hash).expect("record");
    (hash, record, state)
}

/// The leaf of the commitment a lookup found.
fn found_leaf(engine: &MasternodeListEngine, chain: &FixtureChain, hash: &QuorumHash) -> [u8; 32] {
    match engine.lookup_quorum(llmq(CHAIN_LOCKS), hash, chain.tip, chain, config()) {
        Ok(Lookup::Found(found)) => {
            assert_eq!(found.quorum.quorum_hash(), hash);
            commitment_hash(&found.commitment)
        }
        other => panic!("not found: {other:?}"),
    }
}

/// A3′: after a restart the found lookup still carries the active chain's leaf.
#[test]
fn a_found_lookup_after_load_carries_the_active_leaf() {
    let chain = FixtureChain::full();
    let mut engine = synced(&chain);
    let (hash, record, state) = cores_288(&engine);
    assert!(matches!(state, QuorumState::Verified(_)));
    let leaf = commitment_hash(&record);
    assert_eq!(found_leaf(&engine, &chain, &hash), leaf);
    let engine = super::persist::reloaded(&mut engine, &chain);
    assert_eq!(found_leaf(&engine, &chain, &hash), leaf);
}

/// A3′: a competing verified commitment for the same quorum, held only on a branch the active
/// chain left, is never the source of a found lookup: the active chain's leaf is.
#[test]
fn a_competing_record_on_an_inactive_branch_is_never_the_source() {
    let chain = FixtureChain {
        fork: Some(300),
        ..FixtureChain::full()
    };
    let mut engine = synced(&chain);
    let (hash, original, state) = cores_288(&engine);
    let QuorumState::Verified(verified) = state else {
        panic!("the original is verified")
    };
    let mut bytes = Vec::new();
    original.encode(&mut bytes);
    let competing = (0..bytes.len())
        .flat_map(|index| [1u8, 2, 4, 8].into_iter().map(move |bit| (index, bit)))
        .find_map(|(index, bit)| {
            let mut mutated = bytes.clone();
            mutated[index] ^= bit;
            let candidate = Commitment::decode(&mut &mutated[..]).ok()?;
            (candidate.quorum_hash() == original.quorum_hash()
                && commitment_hash(&candidate) != commitment_hash(&original))
            .then_some(candidate)
        })
        .expect("a decodable commitment for the same quorum");
    engine.hold_set(
        BlockId(FORK + chain.tip),
        chain.tip,
        chain.fork_hash(chain.tip),
        std::slice::from_ref(&competing),
    );
    engine.set_state(&competing, QuorumState::Verified(verified));
    assert!(!chain.fork_active);
    let (_, newest, state) = cores_288(&engine);
    assert_eq!(commitment_hash(&newest), commitment_hash(&competing), "held, newest");
    assert!(matches!(state, QuorumState::Verified(_)));
    assert_eq!(found_leaf(&engine, &chain, &hash), commitment_hash(&original));
}
