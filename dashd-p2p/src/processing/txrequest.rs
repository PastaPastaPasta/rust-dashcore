//! Core's `TxRequestTracker` (`txrequest.{h,cpp}`): which peer is asked for which announced
//! object, when, and in what order (P5a D7).
//!
//! Dash tracks every inv-requested object type here, keyed by the whole inventory (type and
//! hash). Each (peer, inventory) announcement is in one state; at most one announcement per
//! inventory is selected (`CANDIDATE_BEST` or `REQUESTED`), preferred peers win over the rest,
//! and among equals a salted hash decides. The three ordered indexes are Core's `ByPeer`,
//! `ByTxHash` and `ByTime`; ties between equal keys (Core's `ordered_non_unique` positions) are
//! broken by sequence number, which no rule here reads (D7).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use dash_p2p::PeerId;
use dash_p2p::siphash::SipHasher;
use dash_p2p_core::Inventory;
use dash_types::Numeric;

/// Core's `CInv` order: the type, then the hash's bytes.
// Core: protocol.cpp:271-274; uint256.h base_blob::Compare (memcmp)
type InvKey = (u32, [u8; 32]);

fn inv_key(inv: &Inventory) -> InvKey {
    (inv.inv_type.to_base(), inv.hash.to_lendian())
}

/// Core's `State`. The order is the `ByTxHash` sort order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum State {
    CandidateDelayed,
    CandidateReady,
    CandidateBest,
    Requested,
    Completed,
}

/// Core's `WaitState`, in `ByTime` order, with Core's names.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum WaitState {
    FutureEvent,
    NoEvent,
    PastEvent,
}

/// Core's `Announcement`.
#[derive(Clone, Copy, Debug)]
struct Announcement {
    inv: Inventory,
    /// The reqtime for the candidate states, the expiry for `Requested`, in µs.
    time: i64,
    peer: PeerId,
    sequence: u64,
    preferred: bool,
    state: State,
}

impl Announcement {
    fn is_selected(&self) -> bool {
        matches!(self.state, State::CandidateBest | State::Requested)
    }

    fn is_waiting(&self) -> bool {
        matches!(self.state, State::Requested | State::CandidateDelayed)
    }

    fn is_selectable(&self) -> bool {
        matches!(self.state, State::CandidateReady | State::CandidateBest)
    }

    // Core: txrequest.cpp GetWaitState
    fn wait_state(&self) -> WaitState {
        if self.is_waiting() {
            WaitState::FutureEvent
        } else if self.is_selectable() {
            WaitState::PastEvent
        } else {
            WaitState::NoEvent
        }
    }
}

/// `ByPeerView`: (peer, is `CANDIDATE_BEST`, inventory). Unique.
type PeerKey = (PeerId, bool, InvKey);
/// `ByTxHashView`: (inventory, state, priority if `CANDIDATE_READY` else 0), then the sequence.
type HashKey = (InvKey, State, u64, u64);
/// `ByTimeView`: (wait state, time), then the sequence.
type TimeKey = (WaitState, i64, u64);

/// Core's `PeerInfo`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PeerInfo {
    total: usize,
    completed: usize,
    requested: usize,
}

/// Core's `TxRequestTracker`.
#[derive(Debug)]
pub struct TxRequestTracker {
    current_sequence: u64,
    /// `PriorityComputer`'s salt.
    k0: u64,
    k1: u64,
    /// The announcements by sequence number.
    anns: HashMap<u64, Announcement>,
    by_peer: BTreeMap<PeerKey, u64>,
    by_hash: BTreeSet<HashKey>,
    by_time: BTreeSet<TimeKey>,
    peerinfo: HashMap<PeerId, PeerInfo>,
}

impl TxRequestTracker {
    /// A tracker with `PriorityComputer`'s salt `(k0, k1)`: two `GetRand(2^64 - 1)` draws, or
    /// zeros when deterministic.
    // Core: txrequest.cpp PriorityComputer::PriorityComputer
    pub fn new(k0: u64, k1: u64) -> Self {
        Self {
            current_sequence: 0,
            k0,
            k1,
            anns: HashMap::new(),
            by_peer: BTreeMap::new(),
            by_hash: BTreeSet::new(),
            by_time: BTreeSet::new(),
            peerinfo: HashMap::new(),
        }
    }

    /// Core's `PriorityComputer::operator()`: higher is selected first.
    // Core: txrequest.cpp PriorityComputer
    pub fn compute_priority(&self, inv: &Inventory, peer: PeerId, preferred: bool) -> u64 {
        let low_bits = SipHasher::new(self.k0, self.k1)
            .write(&inv.hash.to_lendian())
            .write_u64(u64::from(inv.inv_type.to_base()))
            .write_u64(peer as u64)
            .finish()
            >> 1;
        low_bits | u64::from(preferred) << 63
    }

    fn peer_key(a: &Announcement) -> PeerKey {
        (a.peer, a.state == State::CandidateBest, inv_key(&a.inv))
    }

    fn hash_key(&self, a: &Announcement) -> HashKey {
        let prio = if a.state == State::CandidateReady {
            self.compute_priority(&a.inv, a.peer, a.preferred)
        } else {
            0
        };
        (inv_key(&a.inv), a.state, prio, a.sequence)
    }

    fn time_key(a: &Announcement) -> TimeKey {
        (a.wait_state(), a.time, a.sequence)
    }

    fn ann(&self, seq: u64) -> &Announcement {
        &self.anns[&seq]
    }

    fn unindex(&mut self, seq: u64) {
        let a = self.anns[&seq];
        self.by_peer.remove(&Self::peer_key(&a));
        let hk = self.hash_key(&a);
        self.by_hash.remove(&hk);
        self.by_time.remove(&Self::time_key(&a));
    }

    fn index(&mut self, seq: u64) {
        let a = self.anns[&seq];
        self.by_peer.insert(Self::peer_key(&a), seq);
        let hk = self.hash_key(&a);
        self.by_hash.insert(hk);
        self.by_time.insert(Self::time_key(&a));
    }

    // Core: txrequest.cpp Impl::Erase
    fn erase(&mut self, seq: u64) {
        self.unindex(seq);
        let a = self.anns.remove(&seq).expect("erasing a tracked announcement");
        let info = self.peerinfo.get_mut(&a.peer).expect("its peer is tracked");
        info.completed -= usize::from(a.state == State::Completed);
        info.requested -= usize::from(a.state == State::Requested);
        info.total -= 1;
        if info.total == 0 {
            self.peerinfo.remove(&a.peer);
        }
    }

    // Core: txrequest.cpp Impl::Modify
    fn modify(&mut self, seq: u64, f: impl FnOnce(&mut Announcement)) {
        self.unindex(seq);
        let a = self.anns.get_mut(&seq).expect("modifying a tracked announcement");
        let info = self.peerinfo.get_mut(&a.peer).expect("its peer is tracked");
        info.completed -= usize::from(a.state == State::Completed);
        info.requested -= usize::from(a.state == State::Requested);
        f(a);
        info.completed += usize::from(a.state == State::Completed);
        info.requested += usize::from(a.state == State::Requested);
        self.index(seq);
    }

    fn set_state(&mut self, seq: u64, state: State) {
        self.modify(seq, |a| a.state = state);
    }

    /// The announcement after `seq` in `ByTxHash` order.
    fn hash_next(&self, seq: u64) -> Option<&Announcement> {
        let k = self.hash_key(self.ann(seq));
        self.by_hash
            .range((std::ops::Bound::Excluded(k), std::ops::Bound::Unbounded))
            .next()
            .map(|&(.., s)| self.ann(s))
    }

    /// The announcement before `seq` in `ByTxHash` order.
    fn hash_prev(&self, seq: u64) -> Option<&Announcement> {
        let k = self.hash_key(self.ann(seq));
        self.by_hash.range(..k).next_back().map(|&(.., s)| self.ann(s))
    }

    // Core: txrequest.cpp Impl::PromoteCandidateReady
    fn promote_candidate_ready(&mut self, seq: u64) {
        debug_assert_eq!(self.ann(seq).state, State::CandidateDelayed);
        self.set_state(seq, State::CandidateReady);
        let it = *self.ann(seq);
        match self.hash_next(seq).copied() {
            next if next.is_none_or(|n| {
                inv_key(&n.inv) != inv_key(&it.inv) || n.state == State::Completed
            }) =>
            {
                // No REQUESTED or CANDIDATE_BEST for this inventory: this one is the best.
                self.set_state(seq, State::CandidateBest);
            }
            Some(next) if next.state == State::CandidateBest => {
                let old = self.compute_priority(&next.inv, next.peer, next.preferred);
                let new = self.compute_priority(&it.inv, it.peer, it.preferred);
                if new > old {
                    self.set_state(next.sequence, State::CandidateReady);
                    self.set_state(seq, State::CandidateBest);
                }
            }
            _ => {}
        }
    }

    // Core: txrequest.cpp Impl::ChangeAndReselect
    fn change_and_reselect(&mut self, seq: u64, new_state: State) {
        debug_assert!(matches!(new_state, State::Completed | State::CandidateDelayed));
        let it = *self.ann(seq);
        if it.is_selected()
            && let Some(prev) = self.hash_prev(seq).copied()
            && inv_key(&prev.inv) == inv_key(&it.inv)
            && prev.state == State::CandidateReady
        {
            // The highest-priority CANDIDATE_READY sorts just before the selected one.
            self.set_state(prev.sequence, State::CandidateBest);
        }
        self.set_state(seq, new_state);
    }

    // Core: txrequest.cpp Impl::IsOnlyNonCompleted
    fn is_only_non_completed(&self, seq: u64) -> bool {
        let it = self.ann(seq);
        debug_assert_ne!(it.state, State::Completed);
        let key = inv_key(&it.inv);
        if self.hash_prev(seq).is_some_and(|p| inv_key(&p.inv) == key) {
            return false;
        }
        !self.hash_next(seq).is_some_and(|n| inv_key(&n.inv) == key && n.state != State::Completed)
    }

    /// Core's `MakeCompleted`: whether the announcement still exists.
    // Core: txrequest.cpp Impl::MakeCompleted
    fn make_completed(&mut self, seq: u64) -> bool {
        if self.ann(seq).state == State::Completed {
            return true;
        }
        if self.is_only_non_completed(seq) {
            // Nothing left to request for this inventory: forget every announcement of it.
            let inv = self.ann(seq).inv;
            self.forget_tx_hash(&inv);
            return false;
        }
        self.change_and_reselect(seq, State::Completed);
        true
    }

    // Core: txrequest.cpp Impl::SetTimePoint
    fn set_time_point(&mut self, now: i64, expired: &mut Vec<(PeerId, Inventory)>) {
        expired.clear();
        while let Some(&(_, _, seq)) = self.by_time.first() {
            let a = *self.ann(seq);
            if a.state == State::CandidateDelayed && a.time <= now {
                self.promote_candidate_ready(seq);
            } else if a.state == State::Requested && a.time <= now {
                expired.push((a.peer, a.inv));
                self.make_completed(seq);
            } else {
                break;
            }
        }
        while let Some(&(_, _, seq)) = self.by_time.last() {
            let a = *self.ann(seq);
            if a.is_selectable() && a.time > now {
                // The clock went backwards: the reqtime is in the future again.
                self.change_and_reselect(seq, State::CandidateDelayed);
            } else {
                break;
            }
        }
    }

    /// Deletes every announcement of `peer`.
    // Core: txrequest.cpp Impl::DisconnectedPeer
    pub fn disconnected_peer(&mut self, peer: PeerId) {
        let seqs: Vec<u64> = self
            .by_peer
            .range((peer, false, (0, [0; 32]))..)
            .take_while(|((p, ..), _)| *p == peer)
            .map(|(_, &s)| s)
            .collect();
        for seq in seqs {
            // An earlier one's `MakeCompleted` erases only its own inventory's announcements.
            if self.anns.contains_key(&seq) && self.make_completed(seq) {
                self.erase(seq);
            }
        }
    }

    /// Deletes every announcement of `inv`, across all peers.
    // Core: txrequest.cpp Impl::ForgetTxHash
    pub fn forget_tx_hash(&mut self, inv: &Inventory) {
        let key = inv_key(inv);
        let seqs: Vec<u64> = self
            .by_hash
            .range((key, State::CandidateDelayed, 0, 0)..)
            .take_while(|(k, ..)| *k == key)
            .map(|&(.., s)| s)
            .collect();
        for seq in seqs {
            self.erase(seq);
        }
    }

    /// Adds a `CANDIDATE` announcement, unless one exists for (`peer`, `inv`).
    // Core: txrequest.cpp Impl::ReceivedInv
    pub fn received_inv(&mut self, peer: PeerId, inv: &Inventory, preferred: bool, reqtime: i64) {
        let key = inv_key(inv);
        if self.by_peer.contains_key(&(peer, true, key))
            || self.by_peer.contains_key(&(peer, false, key))
        {
            return;
        }
        let seq = self.current_sequence;
        self.anns.insert(
            seq,
            Announcement {
                inv: *inv,
                time: reqtime,
                peer,
                sequence: seq,
                preferred,
                state: State::CandidateDelayed,
            },
        );
        self.index(seq);
        self.peerinfo.entry(peer).or_default().total += 1;
        self.current_sequence += 1;
    }

    /// The inventories to request from `peer` now, in announcement order; the requests that
    /// expired on the way go to `expired`.
    // Core: txrequest.cpp Impl::GetRequestable
    pub fn get_requestable(
        &mut self,
        peer: PeerId,
        now: i64,
        expired: &mut Vec<(PeerId, Inventory)>,
    ) -> Vec<Inventory> {
        self.set_time_point(now, expired);
        let mut selected: Vec<&Announcement> = self
            .by_peer
            .range((peer, true, (0, [0; 32]))..)
            .take_while(|((p, best, _), _)| *p == peer && *best)
            .map(|(_, &s)| self.ann(s))
            .collect();
        selected.sort_by_key(|a| a.sequence);
        selected.into_iter().map(|a| a.inv).collect()
    }

    /// Marks (`peer`, `inv`) requested until `expiry`.
    // Core: txrequest.cpp Impl::RequestedTx
    pub fn requested_tx(&mut self, peer: PeerId, inv: &Inventory, expiry: i64) {
        let key = inv_key(inv);
        let seq = match self.by_peer.get(&(peer, true, key)) {
            Some(&seq) => seq,
            None => {
                let Some(&seq) = self.by_peer.get(&(peer, false, key)) else {
                    return;
                };
                if !matches!(self.ann(seq).state, State::CandidateDelayed | State::CandidateReady) {
                    return;
                }
                // Another announcement may be selected: deselect it first.
                let old = self
                    .by_hash
                    .range((key, State::CandidateBest, 0, 0)..)
                    .next()
                    .filter(|(k, ..)| *k == key)
                    .map(|&(_, state, _, s)| (state, s));
                match old {
                    Some((State::CandidateBest, s)) => self.set_state(s, State::CandidateReady),
                    Some((State::Requested, s)) => self.set_state(s, State::Completed),
                    _ => {}
                }
                seq
            }
        };
        self.modify(seq, |a| {
            a.state = State::Requested;
            a.time = expiry;
        });
    }

    /// Completes (`peer`, `inv`) whatever its state: a `notfound`, the object, or no further
    /// use. Whether there was such a non-completed announcement.
    // Core: txrequest.cpp Impl::ReceivedResponse
    pub fn received_response(&mut self, peer: PeerId, inv: &Inventory) -> bool {
        let key = inv_key(inv);
        let found =
            self.by_peer.get(&(peer, false, key)).or_else(|| self.by_peer.get(&(peer, true, key)));
        let Some(&seq) = found else {
            return false;
        };
        if self.ann(seq).state == State::Completed {
            return false;
        }
        self.make_completed(seq);
        true
    }

    /// Completes (`peer`, `inv`) only if it is in flight: whether it was.
    // Core: txrequest.cpp Impl::ReceivedRequestedResponse
    pub fn received_requested_response(&mut self, peer: PeerId, inv: &Inventory) -> bool {
        match self.by_peer.get(&(peer, false, inv_key(inv))) {
            Some(&seq) if self.ann(seq).state == State::Requested => {
                self.make_completed(seq);
                true
            }
            _ => false,
        }
    }

    /// The number of `REQUESTED` announcements of `peer`.
    pub fn count_in_flight(&self, peer: PeerId) -> usize {
        self.peerinfo.get(&peer).map_or(0, |i| i.requested)
    }

    /// The number of `CANDIDATE` announcements of `peer`.
    pub fn count_candidates(&self, peer: PeerId) -> usize {
        self.peerinfo.get(&peer).map_or(0, |i| i.total - i.requested - i.completed)
    }

    /// The number of announcements of `peer`.
    pub fn count(&self, peer: PeerId) -> usize {
        self.peerinfo.get(&peer).map_or(0, |i| i.total)
    }

    /// The number of announcements.
    pub fn size(&self) -> usize {
        self.anns.len()
    }

    /// Core's `SanityCheck`: the invariants every operation keeps.
    ///
    /// # Panics
    ///
    /// When one fails.
    // Core: txrequest.cpp Impl::SanityCheck
    pub fn sanity_check(&self) {
        let mut info: HashMap<PeerId, PeerInfo> = HashMap::new();
        #[derive(Default)]
        struct Per {
            ready: usize,
            best: usize,
            requested: usize,
            delayed: usize,
            best_prio: Option<u64>,
            max_ready: u64,
            peers: Vec<PeerId>,
        }
        let mut per: BTreeMap<InvKey, Per> = BTreeMap::new();
        for a in self.anns.values() {
            let i = info.entry(a.peer).or_default();
            i.total += 1;
            i.requested += usize::from(a.state == State::Requested);
            i.completed += usize::from(a.state == State::Completed);
            let p = per.entry(inv_key(&a.inv)).or_default();
            let prio = self.compute_priority(&a.inv, a.peer, a.preferred);
            match a.state {
                State::CandidateDelayed => p.delayed += 1,
                State::CandidateReady => {
                    p.ready += 1;
                    p.max_ready = p.max_ready.max(prio);
                }
                State::CandidateBest => {
                    p.best += 1;
                    p.best_prio = Some(prio);
                }
                State::Requested => p.requested += 1,
                State::Completed => {}
            }
            p.peers.push(a.peer);
        }
        assert_eq!(info, self.peerinfo);
        for p in per.values_mut() {
            assert!(p.delayed + p.ready + p.best + p.requested > 0, "only COMPLETED left");
            assert!(p.best + p.requested <= 1, "two selected");
            if p.ready > 0 {
                assert_eq!(p.best + p.requested, 1, "READY without a selected one");
            }
            if let Some(best) = p.best_prio
                && p.ready > 0
            {
                assert!(best >= p.max_ready, "BEST is not the highest priority");
            }
            p.peers.sort_unstable();
            assert!(p.peers.windows(2).all(|w| w[0] != w[1]), "two announcements from one peer");
        }
        assert_eq!(self.by_peer.len(), self.anns.len());
        assert_eq!(self.by_hash.len(), self.anns.len());
        assert_eq!(self.by_time.len(), self.anns.len());
    }

    /// Core's `PostGetRequestableSanityCheck`.
    ///
    /// # Panics
    ///
    /// When a waiting announcement is due or a selectable one is not.
    pub fn post_get_requestable_sanity_check(&self, now: i64) {
        for a in self.anns.values() {
            if a.is_waiting() {
                assert!(a.time > now);
            } else if a.is_selectable() {
                assert!(a.time <= now);
            }
        }
    }
}

#[cfg(test)]
mod tests;
