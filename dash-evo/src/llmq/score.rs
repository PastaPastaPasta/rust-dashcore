//! Scores and ordering (LQ1 §3.4): Core's `CalculateQuorum`.

use std::cmp::Ordering;

use dash_consensus::hash::sha256;

use super::view::MnEntryView;

/// A quorum modifier: the hash every member's score is taken against (LQ1 §3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct QuorumModifier(pub [u8; 32]);

/// Core `calculateQuorumScore`: single SHA-256 of the stored `confirmedHashWithProRegTxHash` and
/// the modifier, as the bytes Core reads into an `arith_uint256` (little-endian).
// Core: v24 llmq/utils.cpp:72 calculateQuorumScore
pub fn quorum_score<E: MnEntryView>(entry: &E, modifier: &QuorumModifier) -> [u8; 32] {
    let mut data = [0u8; 64];
    data[..32].copy_from_slice(entry.confirmed_hash_with_protx());
    data[32..].copy_from_slice(&modifier.0);
    sha256(&data)
}

/// Core's sort order for scored members, ascending: score as an `arith_uint256`, then the
/// collateral as `COutPoint::operator<` (hash memcmp, then index). `CalculateQuorum` sorts with it
/// through reverse iterators, so members come out descending.
fn score_order<E: MnEntryView>(a: &([u8; 32], E), b: &([u8; 32], E)) -> Ordering {
    a.0.iter().rev().cmp(b.0.iter().rev()).then_with(|| {
        let (ca, cb) = (a.1.collateral(), b.1.collateral());
        ca.hash.as_bytes().cmp(cb.hash.as_bytes()).then(ca.index.cmp(&cb.index))
    })
}

/// Core `CalculateQuorum(list, modifier, maxSize, onlyEvoNodes)`: the scored candidates in
/// descending score order, the first `max_size` of them when `max_size > 0`.
///
/// `candidates` is what Core scores: `ForEachMNShared(onlyValid = true)` of a list, or a vector of
/// entries (rotation). Banned, unconfirmed and, when `only_evo`, non-Evo entries are skipped. The
/// order of `candidates` never matters: the sort is total on entries with distinct collaterals,
/// and entries that tie on both keys are the same masternode.
// Core: v24 llmq/utils.cpp:157 CalculateQuorum
pub fn calculate_quorum<E: MnEntryView>(
    candidates: impl IntoIterator<Item = E>,
    modifier: &QuorumModifier,
    max_size: usize,
    only_evo: bool,
) -> Vec<E> {
    // Core: v24 llmq/utils.cpp:114 CalculateScoresForQuorum
    let mut scores: Vec<([u8; 32], E)> = candidates
        .into_iter()
        .filter(|e| !e.is_banned() && !e.confirmed_hash_is_null() && (!only_evo || e.is_evo()))
        .map(|e| (quorum_score(&e, modifier), e))
        .collect();
    scores.sort_unstable_by(|a, b| score_order(b, a));
    if max_size > 0 && scores.len() > max_size {
        scores.truncate(max_size);
    }
    scores.into_iter().map(|(_, e)| e).collect()
}
