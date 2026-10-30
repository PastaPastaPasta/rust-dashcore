//! The tracker against Core's scenarios (`test/txrequest_tests.cpp`) and against a reference
//! model of `txrequest.h`'s specification.

use dash_num::Hash256;
use dash_p2p_core::InvType;
use proptest::prelude::*;

use super::*;

fn inv(t: u32, n: u8) -> Inventory {
    Inventory {
        inv_type: InvType::from_base(t),
        hash: Hash256::from_lendian([n; 32]),
    }
}

const SPORK: u32 = 6;
const CLSIG: u32 = 29;

fn requestable(t: &mut TxRequestTracker, peer: PeerId, now: i64) -> Vec<Inventory> {
    let mut expired = Vec::new();
    let r = t.get_requestable(peer, now, &mut expired);
    t.sanity_check();
    t.post_get_requestable_sanity_check(now);
    r
}

/// Core's `BuildSingleTest`: one announcement goes DELAYED → BEST at its reqtime, REQUESTED,
/// then expires, and is forgotten.
#[test]
fn a_single_announcement_is_requestable_from_its_reqtime_until_it_expires() {
    let mut t = TxRequestTracker::new(1, 2);
    let a = inv(SPORK, 1);
    t.received_inv(7, &a, true, 100);
    assert_eq!(t.count_candidates(7), 1);
    assert!(requestable(&mut t, 7, 99).is_empty());
    assert_eq!(requestable(&mut t, 7, 100), [a]);
    t.requested_tx(7, &a, 200);
    assert_eq!(t.count_in_flight(7), 1);
    assert!(requestable(&mut t, 7, 150).is_empty());
    let mut expired = Vec::new();
    assert!(t.get_requestable(7, 200, &mut expired).is_empty());
    assert_eq!(expired, [(7, a)]);
    // The only announcement completed: the inventory is forgotten.
    assert_eq!(t.size(), 0);
    assert_eq!(t.count(7), 0);
}

/// Core's `BuildPriorityTest`: a preferred peer wins over a non-preferred one, whatever the
/// salted hash says; between two of a kind the higher priority wins.
#[test]
fn preferred_peers_win_then_the_higher_priority() {
    for (k0, k1) in [(0, 0), (1, 2), (99, 7), (u64::MAX, 3)] {
        let mut t = TxRequestTracker::new(k0, k1);
        let a = inv(CLSIG, 3);
        t.received_inv(1, &a, false, 0);
        t.received_inv(2, &a, true, 0);
        t.received_inv(3, &a, false, 0);
        assert_eq!(requestable(&mut t, 2, 0), [a]);
        assert!(requestable(&mut t, 1, 0).is_empty());
        assert!(requestable(&mut t, 3, 0).is_empty());
        // The preferred one fails: the better of the other two is next.
        assert!(t.received_response(2, &a));
        let first = if t.compute_priority(&a, 1, false) > t.compute_priority(&a, 3, false) {
            1
        } else {
            3
        };
        assert_eq!(requestable(&mut t, first, 0), [a]);
        assert!(requestable(&mut t, 4 - first, 0).is_empty());
    }
}

/// Core's `BuildRequestOrderTest`: a peer's requestable inventories come in announcement order.
#[test]
fn requests_come_in_announcement_order() {
    let mut t = TxRequestTracker::new(5, 6);
    let invs = [inv(SPORK, 9), inv(CLSIG, 1), inv(SPORK, 2), inv(CLSIG, 200)];
    for (i, x) in invs.iter().enumerate() {
        t.received_inv(1, x, true, 10 - i as i64);
    }
    assert_eq!(requestable(&mut t, 1, 10), invs);
}

/// An expired request falls back to the next announcer (the request interval is the fallback
/// timer, np:6841-6844).
#[test]
fn an_expired_request_falls_back_to_the_next_announcer() {
    let mut t = TxRequestTracker::new(0, 0);
    let a = inv(SPORK, 4);
    t.received_inv(1, &a, true, 0);
    t.received_inv(2, &a, true, 0);
    let first = if requestable(&mut t, 1, 0) == [a] {
        1
    } else {
        2
    };
    let other = 3 - first;
    t.requested_tx(first, &a, 60);
    assert!(requestable(&mut t, other, 59).is_empty());
    assert_eq!(requestable(&mut t, other, 60), [a]);
    // The first peer's announcement is COMPLETED, never asked again.
    t.requested_tx(other, &a, 120);
    assert!(requestable(&mut t, first, 200).is_empty());
    assert_eq!(t.size(), 0);
}

/// Core's `BuildTimeBackwardsTest`: a selectable announcement whose reqtime is in the future again
/// goes back to DELAYED, and the next best is selected.
#[test]
fn time_going_backwards_delays_again() {
    let mut t = TxRequestTracker::new(3, 4);
    let a = inv(SPORK, 5);
    t.received_inv(1, &a, true, 100);
    t.received_inv(2, &a, false, 50);
    assert_eq!(requestable(&mut t, 1, 100), [a]);
    assert_eq!(requestable(&mut t, 2, 70), [a]);
    assert!(requestable(&mut t, 1, 70).is_empty());
}

/// Core's `BuildWeirdRequestsTest`: requesting a non-selected announcement deselects the
/// selected one; a second request completes the first.
#[test]
fn requesting_a_non_selected_announcement_moves_the_selection() {
    let mut t = TxRequestTracker::new(0, 0);
    let a = inv(CLSIG, 6);
    t.received_inv(1, &a, true, 0);
    t.received_inv(2, &a, false, 0);
    assert_eq!(requestable(&mut t, 1, 0), [a]);
    t.requested_tx(2, &a, 10);
    assert_eq!(t.count_in_flight(2), 1);
    assert!(requestable(&mut t, 1, 5).is_empty());
    t.requested_tx(1, &a, 20);
    assert_eq!(t.count_in_flight(1), 1);
    assert_eq!(t.count_in_flight(2), 0);
    assert!(!t.received_requested_response(2, &a));
    assert!(t.received_requested_response(1, &a));
    assert_eq!(t.size(), 0);
}

/// The inventory type is part of the key (Dash): one hash under two types is two objects.
#[test]
fn the_type_is_part_of_the_key() {
    let mut t = TxRequestTracker::new(0, 0);
    t.received_inv(1, &inv(SPORK, 1), true, 0);
    t.received_inv(1, &inv(CLSIG, 1), true, 0);
    assert_eq!(t.count(1), 2);
    t.forget_tx_hash(&inv(SPORK, 1));
    assert_eq!(requestable(&mut t, 1, 0), [inv(CLSIG, 1)]);
}

#[test]
fn a_repeated_announcement_is_ignored_even_after_its_request() {
    let mut t = TxRequestTracker::new(0, 0);
    let a = inv(SPORK, 1);
    t.received_inv(1, &a, true, 0);
    t.received_inv(2, &a, true, 0);
    assert_eq!(requestable(&mut t, 1, 0).len() + requestable(&mut t, 2, 0).len(), 1);
    t.received_inv(1, &a, false, 0);
    assert_eq!(t.count(1), 1);
    assert!(t.received_response(1, &a));
    t.received_inv(1, &a, true, 0);
    assert_eq!(t.count(1), 1, "a COMPLETED announcement still blocks a new one");
    t.disconnected_peer(2);
    assert_eq!(t.size(), 0, "only COMPLETED left: forgotten");
    t.received_inv(1, &a, true, 0);
    assert_eq!(t.count(1), 1);
}

/// The specification of `txrequest.h`, done the slow way: every announcement keeps its
/// candidate/requested/completed status and whether the last time point found it ready, and the
/// selection is recomputed from scratch.
#[derive(Clone, Debug, Default)]
struct Model {
    anns: Vec<ModelAnn>,
    seq: u64,
}

#[derive(Clone, Debug)]
struct ModelAnn {
    peer: PeerId,
    inv: Inventory,
    preferred: bool,
    time: i64,
    status: Status,
    ready: bool,
    seq: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Status {
    Candidate,
    Requested,
    Completed,
}

impl Model {
    fn find(&self, peer: PeerId, inv: &Inventory) -> Option<usize> {
        self.anns.iter().position(|a| a.peer == peer && a.inv == *inv)
    }

    /// The selected candidate of `inv`, if no request is in flight.
    fn best(&self, t: &TxRequestTracker, inv: &Inventory) -> Option<usize> {
        let group = || self.anns.iter().enumerate().filter(|(_, a)| a.inv == *inv);
        if group().any(|(_, a)| a.status == Status::Requested) {
            return None;
        }
        group()
            .filter(|(_, a)| a.status == Status::Candidate && a.ready)
            .max_by_key(|(_, a)| t.compute_priority(&a.inv, a.peer, a.preferred))
            .map(|(i, _)| i)
    }

    fn drop_completed_groups(&mut self) {
        let live: Vec<Inventory> =
            self.anns.iter().filter(|a| a.status != Status::Completed).map(|a| a.inv).collect();
        self.anns.retain(|a| live.contains(&a.inv));
    }

    fn received_inv(&mut self, peer: PeerId, inv: &Inventory, preferred: bool, time: i64) {
        if self.find(peer, inv).is_none() {
            self.anns.push(ModelAnn {
                peer,
                inv: *inv,
                preferred,
                time,
                status: Status::Candidate,
                ready: false,
                seq: self.seq,
            });
            self.seq += 1;
        }
    }

    fn get_requestable(
        &mut self,
        t: &TxRequestTracker,
        peer: PeerId,
        now: i64,
    ) -> (Vec<Inventory>, Vec<(PeerId, Inventory)>) {
        let mut expired: Vec<(i64, u64, PeerId, Inventory)> = Vec::new();
        for a in &mut self.anns {
            match a.status {
                Status::Candidate => a.ready = a.time <= now,
                Status::Requested if a.time <= now => {
                    a.status = Status::Completed;
                    expired.push((a.time, a.seq, a.peer, a.inv));
                }
                _ => {}
            }
        }
        self.drop_completed_groups();
        let mut mine: Vec<&ModelAnn> = self
            .anns
            .iter()
            .enumerate()
            .filter(|(i, a)| a.peer == peer && self.best(t, &a.inv) == Some(*i))
            .map(|(_, a)| a)
            .collect();
        mine.sort_by_key(|a| a.seq);
        expired.sort_by_key(|(t, s, ..)| (*t, *s));
        (
            mine.into_iter().map(|a| a.inv).collect(),
            expired.into_iter().map(|(.., p, i)| (p, i)).collect(),
        )
    }

    fn requested_tx(&mut self, t: &TxRequestTracker, peer: PeerId, inv: &Inventory, exp: i64) {
        let Some(i) = self.find(peer, inv) else {
            return;
        };
        let selected = self.best(t, inv) == Some(i);
        if !selected && self.anns[i].status != Status::Candidate {
            return;
        }
        for a in self.anns.iter_mut().filter(|a| a.inv == *inv && a.status == Status::Requested) {
            a.status = Status::Completed;
        }
        self.anns[i].status = Status::Requested;
        self.anns[i].time = exp;
    }

    fn received_response(&mut self, peer: PeerId, inv: &Inventory, requested_only: bool) -> bool {
        let Some(i) = self.find(peer, inv) else {
            return false;
        };
        let ok = if requested_only {
            self.anns[i].status == Status::Requested
        } else {
            self.anns[i].status != Status::Completed
        };
        if ok {
            self.anns[i].status = Status::Completed;
            self.drop_completed_groups();
        }
        ok
    }

    fn disconnected_peer(&mut self, peer: PeerId) {
        self.anns.retain(|a| a.peer != peer);
        self.drop_completed_groups();
    }

    fn forget(&mut self, inv: &Inventory) {
        self.anns.retain(|a| a.inv != *inv);
    }

    fn count(&self, peer: PeerId, f: impl Fn(&ModelAnn) -> bool) -> usize {
        self.anns.iter().filter(|a| a.peer == peer && f(a)).count()
    }
}

#[derive(Clone, Debug)]
enum Op {
    Inv(PeerId, u8, bool, i64),
    Get(PeerId, i64),
    Request(PeerId, u8, i64),
    Response(PeerId, u8, bool),
    Disconnect(PeerId),
    Forget(u8),
}

fn op() -> impl Strategy<Value = Op> {
    let peer = 0..4i64;
    let obj = 0..6u8;
    let time = 0..40i64;
    prop_oneof![
        4 => (peer.clone(), obj.clone(), any::<bool>(), time.clone())
            .prop_map(|(p, o, pref, t)| Op::Inv(p, o, pref, t)),
        4 => (peer.clone(), time.clone()).prop_map(|(p, t)| Op::Get(p, t)),
        3 => (peer.clone(), obj.clone(), time).prop_map(|(p, o, t)| Op::Request(p, o, t)),
        2 => (peer.clone(), obj.clone(), any::<bool>()).prop_map(|(p, o, r)| Op::Response(p, o, r)),
        1 => peer.prop_map(Op::Disconnect),
        1 => obj.prop_map(Op::Forget),
    ]
}

/// Objects 0-2 are sporks, 3-5 clsigs with the same hashes: the type is part of the key.
fn obj(o: u8) -> Inventory {
    inv(
        if o < 3 {
            SPORK
        } else {
            CLSIG
        },
        o % 3,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn the_tracker_follows_the_specification(
        k0 in any::<u64>(),
        k1 in any::<u64>(),
        ops in proptest::collection::vec(op(), 1..80),
    ) {
        let mut t = TxRequestTracker::new(k0, k1);
        let mut m = Model::default();
        for op in ops {
            match op {
                Op::Inv(p, o, pref, time) => {
                    t.received_inv(p, &obj(o), pref, time);
                    m.received_inv(p, &obj(o), pref, time);
                }
                Op::Get(p, now) => {
                    let mut expired = Vec::new();
                    let got = t.get_requestable(p, now, &mut expired);
                    t.post_get_requestable_sanity_check(now);
                    let (want, want_expired) = m.get_requestable(&t, p, now);
                    prop_assert_eq!(got, want);
                    expired.sort_by_key(|(p, i)| (*p, inv_key(i)));
                    let mut want_expired = want_expired;
                    want_expired.sort_by_key(|(p, i)| (*p, inv_key(i)));
                    prop_assert_eq!(expired, want_expired);
                }
                Op::Request(p, o, exp) => {
                    t.requested_tx(p, &obj(o), exp);
                    m.requested_tx(&t, p, &obj(o), exp);
                }
                Op::Response(p, o, requested_only) => {
                    let got = if requested_only {
                        t.received_requested_response(p, &obj(o))
                    } else {
                        t.received_response(p, &obj(o))
                    };
                    prop_assert_eq!(got, m.received_response(p, &obj(o), requested_only));
                }
                Op::Disconnect(p) => {
                    t.disconnected_peer(p);
                    m.disconnected_peer(p);
                }
                Op::Forget(o) => {
                    t.forget_tx_hash(&obj(o));
                    m.forget(&obj(o));
                }
            }
            t.sanity_check();
            prop_assert_eq!(t.size(), m.anns.len());
            for p in 0..4 {
                prop_assert_eq!(t.count(p), m.count(p, |_| true));
                prop_assert_eq!(t.count_in_flight(p), m.count(p, |a| a.status == Status::Requested));
                prop_assert_eq!(
                    t.count_candidates(p),
                    m.count(p, |a| a.status == Status::Candidate)
                );
            }
        }
    }
}
