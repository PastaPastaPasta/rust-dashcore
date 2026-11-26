//! `CDKGPendingMessages` (`l/dkgsessionhandler.cpp:14-91`, v24.0.0): the seen-set, the quota,
//! the FIFO and the clear; `EnqueueOwn` (`l/net_dkg.cpp:369-378`).

use dash_llmq::dkg::session::{DkgMsgKind, Sender};

use super::super::fakes::{dkg_lines, logged, params};
use super::fixture::{KINDS, Rig, TEST, key, pro_tx};
use crate::dkg::intake::PendingMessages;
use crate::dkg::intake::message::payload_hash;
use crate::dkg::log::uint256_hex;
use crate::dkg::seams::PendingQueues;

fn hash(n: u8) -> [u8; 32] {
    [n; 32]
}

#[test]
fn a_hash_seen_before_is_dropped_and_logged() {
    let q = PendingMessages::new(10);
    q.push(Sender::Peer(1), &pro_tx(1), vec![1], hash(1));
    let ((), lines) = logged(|| q.push(Sender::Peer(2), &pro_tx(2), vec![2], hash(1)));
    assert_eq!(
        dkg_lines(&lines),
        [format!(
            "CDKGPendingMessages::PushPendingMessage -- already seen {}, peer=2",
            uint256_hex(&hash(1))
        )]
    );
    assert!(q.has_seen(&hash(1)) && !q.has_seen(&hash(2)));
    assert_eq!(q.pop(8), [(Sender::Peer(1), vec![1])]);
}

#[test]
fn the_quota_counts_per_pro_tx_hash_across_peers_and_pops() {
    let q = PendingMessages::new(2);
    q.push(Sender::Peer(1), &pro_tx(1), vec![1], hash(1));
    assert_eq!(q.pop(8).len(), 1, "a pop refunds nothing");
    q.push(Sender::Peer(2), &pro_tx(1), vec![2], hash(2));
    let ((), lines) = logged(|| q.push(Sender::Peer(3), &pro_tx(1), vec![3], hash(3)));
    assert_eq!(
        dkg_lines(&lines),
        [format!(
            "CDKGPendingMessages::PushPendingMessage -- too many messages from {}, peer=3",
            pro_tx(1)
        )]
    );
    assert!(!q.has_seen(&hash(3)), "a message over quota is not seen");
    q.push(Sender::Peer(3), &pro_tx(2), vec![3], hash(3));
    assert!(q.has_seen(&hash(3)), "another proTxHash with budget delivers it");
    assert_eq!(q.pop(8), [(Sender::Peer(2), vec![2]), (Sender::Peer(3), vec![3])]);
}

#[test]
fn the_seen_check_runs_before_the_quota() {
    let q = PendingMessages::new(1);
    q.push(Sender::Peer(1), &pro_tx(1), vec![1], hash(1));
    let ((), lines) = logged(|| q.push(Sender::Peer(1), &pro_tx(1), vec![1], hash(1)));
    assert!(dkg_lines(&lines)[0].contains("already seen"), "{lines:?}");
}

#[test]
fn a_null_pro_tx_hash_is_dropped_unseen() {
    let q = PendingMessages::new(10);
    let ((), lines) = logged(|| q.push(Sender::Peer(1), &Default::default(), vec![1], hash(1)));
    assert!(lines.is_empty());
    assert!(!q.has_seen(&hash(1)));
    assert!(q.pop(8).is_empty());
}

#[test]
fn pops_are_fifo_and_bounded_and_clear_resets_everything() {
    let q = PendingMessages::new(1);
    for n in 0..5u8 {
        q.push(Sender::Peer(i64::from(n)), &pro_tx(n + 1), vec![n], hash(n));
    }
    let first: Vec<u8> = q.pop(2).into_iter().map(|(_, p)| p[0]).collect();
    assert_eq!(first, [0, 1]);
    q.clear();
    assert!(q.pop(8).is_empty());
    assert!(!q.has_seen(&hash(4)));
    q.push(Sender::Peer(0), &pro_tx(1), vec![9], hash(0));
    assert_eq!(q.pop(8), [(Sender::Peer(0), vec![9])], "the quota and the seen-set are reset");
}

#[test]
fn each_handler_queue_takes_size_times_two_and_own_messages_are_hashed() {
    let rig = Rig::new(&[TEST]);
    let quota = usize::from(params(TEST).size) * 2;
    let k = key(TEST, 0);
    for n in 0..=quota {
        rig.intake.enqueue_own(k, DkgMsgKind::Complaint, &pro_tx(7), vec![n as u8]);
    }
    let popped = rig.intake.pop_batch(k, DkgMsgKind::Complaint, 100);
    assert_eq!(popped.len(), quota);
    assert!(popped.iter().all(|(s, _)| *s == Sender::Own));
    assert!(
        rig.intake.pop_batch(k, DkgMsgKind::Contribution, 100).is_empty(),
        "one queue per kind"
    );
    let inv = dash_llmq::dkg::session::DkgInv {
        kind: DkgMsgKind::Complaint,
        hash: payload_hash(&[0]),
    };
    assert!(rig.intake.already_have(inv));
    for kind in KINDS {
        rig.intake.enqueue_own(k, kind, &pro_tx(8), vec![200]);
    }
    rig.intake.clear(k);
    assert!(!rig.intake.already_have(inv));
    for kind in KINDS {
        assert!(rig.intake.pop_batch(k, kind, 8).is_empty(), "{kind:?}");
    }
}
