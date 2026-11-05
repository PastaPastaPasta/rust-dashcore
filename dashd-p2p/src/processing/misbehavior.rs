//! Misbehaviour (P2a §3.10): the score, the discouragement step at the head of every
//! `SendMessages`, and the block-validation mapping P3 shares.
//!
//! The exemptions and the subnet disconnect are SB6's [`NetCore::discourage`].

use std::sync::Arc;

use dash_consensus::BlockValidationResult;
use dash_p2p::{NetCore, PeerHandle, PeerId};
use parking_lot::Mutex;

use super::peer::PeerState;

/// Core's `DISCOURAGEMENT_THRESHOLD`.
pub const DISCOURAGEMENT_THRESHOLD: i32 = 100;

/// Core's `Peer` fields under `m_misbehavior_mutex`.
#[derive(Debug, Default)]
pub struct Misbehavior {
    /// `m_misbehavior_score`.
    pub score: i32,
    /// `m_should_discourage`.
    pub should_discourage: bool,
}

/// A peer's misbehaviour under its own lock, as Core keeps it apart from `cs_main`: any thread
/// (a handler, RPC, the chain side) may score a peer while the actor holds the peer's state, and
/// no other lock is ever taken while it is held.
pub type MisbehaviorCell = Arc<Mutex<Misbehavior>>;

/// Core's `Misbehaving`: adds `howmuch` to the score and raises `m_should_discourage` once, when
/// the score crosses the threshold. `msg` may be empty. Scores never decay (they saturate where
/// Core's `int` would overflow).
// Core: net_processing.cpp:1933-1955 PeerManagerImpl::Misbehaving
pub fn misbehaving(id: PeerId, cell: &Mutex<Misbehavior>, howmuch: i32, msg: &str) {
    assert!(howmuch > 0, "Misbehaving with a non-positive score");
    let (before, now, crossed) = {
        let mut m = cell.lock();
        let before = m.score;
        m.score = m.score.saturating_add(howmuch);
        let crossed = m.score >= DISCOURAGEMENT_THRESHOLD && before < DISCOURAGEMENT_THRESHOLD;
        m.should_discourage |= crossed;
        (before, m.score, crossed)
    };
    let warning = if crossed {
        " DISCOURAGE THRESHOLD EXCEEDED"
    } else {
        ""
    };
    let prefixed = if msg.is_empty() {
        String::new()
    } else {
        format!(": {msg}")
    };
    tracing::debug!(target: "net", "Misbehaving: peer={id} ({before} -> {now}){warning}{prefixed}");
}

/// Core's `MaybeDiscourageAndDisconnect`, the first step of every `SendMessages`: clears the flag
/// before the exemptions, then lets the core discourage. Returns whether `SendMessages` stops.
// Core: net_processing.cpp:5833-5870 PeerManagerImpl::MaybeDiscourageAndDisconnect
pub fn maybe_discourage_and_disconnect(
    core: &NetCore,
    peer: &PeerHandle,
    state: &PeerState,
) -> bool {
    if !std::mem::take(&mut state.misbehavior.lock().should_discourage) {
        return false;
    }
    core.discourage(peer)
}

/// The score Core's `MaybePunishNodeForBlock` gives a block or header validation failure, or
/// `None` when it gives none. `inbound` is the peer's `m_is_inbound`.
// Core: net_processing.cpp:1969-2019 PeerManagerImpl::MaybePunishNodeForBlock
pub fn block_punishment(
    result: BlockValidationResult,
    inbound: bool,
    via_compact_block: bool,
) -> Option<i32> {
    use BlockValidationResult::*;
    match result {
        Consensus | Mutated if !via_compact_block => Some(100),
        CachedInvalid if !via_compact_block && !inbound => Some(100),
        InvalidHeader | Checkpoint | InvalidPrev => Some(100),
        MissingPrev | ChainLock => Some(10),
        Unset | Consensus | Mutated | CachedInvalid | RecentConsensusChange | TimeFuture => None,
    }
}

/// Core's `MaybePunishNodeForBlock`: scores the peer, or logs `"peer=%d: %s"` on every
/// non-punishing exit with a message. Returns whether it punished.
pub fn maybe_punish_node_for_block(
    state: &mut PeerState,
    result: BlockValidationResult,
    inbound: bool,
    via_compact_block: bool,
    msg: &str,
) -> bool {
    match block_punishment(result, inbound, via_compact_block) {
        Some(score) => {
            misbehaving(state.id, &state.misbehavior, score, msg);
            true
        }
        None => {
            if !msg.is_empty() {
                tracing::debug!(target: "net", "peer={}: {msg}", state.id);
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_is_crossed_once() {
        let c = Mutex::new(Misbehavior::default());
        let get = |c: &Mutex<Misbehavior>| {
            let m = c.lock();
            (m.score, m.should_discourage)
        };
        misbehaving(7, &c, 20, "inv message size = 50001");
        assert_eq!(get(&c), (20, false));
        misbehaving(7, &c, 80, "");
        assert_eq!(get(&c), (100, true));
        c.lock().should_discourage = false;
        misbehaving(7, &c, 10, "");
        // Already above the threshold: no new flag.
        assert_eq!(get(&c), (110, false));
        // Saturates instead of overflowing (noban and manual peers are never disconnected).
        misbehaving(7, &c, i32::MAX, "");
        assert_eq!(get(&c), (i32::MAX, false));
    }

    #[test]
    #[should_panic(expected = "non-positive")]
    fn zero_score_asserts() {
        misbehaving(0, &Mutex::default(), 0, "");
    }

    #[test]
    fn block_results_map_as_core() {
        use BlockValidationResult::*;
        let p = block_punishment;
        assert_eq!(p(Consensus, true, false), Some(100));
        assert_eq!(p(Consensus, true, true), None);
        assert_eq!(p(Mutated, false, true), None);
        assert_eq!(p(CachedInvalid, false, false), Some(100));
        assert_eq!(p(CachedInvalid, true, false), None);
        assert_eq!(p(CachedInvalid, false, true), None);
        for r in [InvalidHeader, Checkpoint, InvalidPrev] {
            assert_eq!(p(r, true, true), Some(100));
        }
        for r in [MissingPrev, ChainLock] {
            assert_eq!(p(r, true, true), Some(10));
        }
        for r in [Unset, RecentConsensusChange, TimeFuture] {
            assert_eq!(p(r, false, false), None);
        }
    }
}
