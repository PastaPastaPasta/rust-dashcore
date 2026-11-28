//! `NetDKG::ProcessMessage` and `AlreadyHave` (`l/net_dkg.cpp:505-704`, v24.0.0): each check, in
//! Core's order, with its score and line.

use std::sync::atomic::Ordering;

use dash_llmq::dkg::session::{DkgInv, DkgMsgKind, Sender};
use dash_params::LlmqType;
use dash_types::Numeric;

use super::super::fakes::{Line, logged, params};
use super::fixture::{KINDS, Rig, TEST, base_hash, framed, key, pro_tx};
use crate::dkg::intake::message::payload_hash;
use crate::dkg::intake::{DkgIntake, DkgNetMsg, Verdict};
use crate::dkg::log::uint256_hex;
use crate::dkg::seams::{GetDataResponse, PendingQueues};

const PEER: i64 = 7;
const CONTRIB: DkgNetMsg = DkgNetMsg::Dkg(DkgMsgKind::Contribution);

fn score(score: i32, message: &'static str) -> Verdict {
    Verdict::Misbehaving {
        score,
        message,
    }
}

/// An active `llmq_test` intake whose peer [`PEER`] is verified and whose chain has a base at
/// 48 (quorum index 0).
fn rig() -> Rig {
    let rig = Rig::new(&[TEST]);
    rig.host.verify(PEER, pro_tx(1));
    rig.chain.add(48);
    rig
}

fn texts(lines: &[Line]) -> Vec<(tracing::Level, String)> {
    lines.iter().filter(|l| l.target != "host").map(|l| (l.level, l.text.clone())).collect()
}

fn info(text: String) -> Vec<(tracing::Level, String)> {
    vec![(tracing::Level::INFO, text)]
}

#[test]
fn spork_17_off_drops_everything_unseen() {
    let rig = rig();
    rig.sporks.spork17.store(false, Ordering::SeqCst);
    let payload = framed(DkgMsgKind::Contribution, 100, &base_hash(48), 1);
    assert_eq!(rig.intake.process_message(PEER, DkgNetMsg::Qwatch, vec![]), Verdict::Ignored);
    assert_eq!(rig.intake.process_message(PEER, CONTRIB, payload), Verdict::Ignored);
    assert!(rig.host.take().is_empty());
}

#[test]
fn qwatch_sets_the_flag_on_a_masternode_and_scores_10_elsewhere() {
    assert_eq!(rig().intake.process_message(PEER, DkgNetMsg::Qwatch, vec![]), Verdict::SetQwatch);
    let observer = Rig::observer();
    assert_eq!(observer.intake.process_message(PEER, DkgNetMsg::Qwatch, vec![]), score(10, ""));
}

#[test]
fn an_observer_ignores_dkg_messages_with_a_line() {
    let rig = Rig::observer();
    rig.host.verify(PEER, pro_tx(1));
    for (kind, name) in KINDS.into_iter().zip(["qcontrib", "qcomplaint", "qjustify", "qpcommit"]) {
        let payload = framed(kind, 100, &base_hash(48), 1);
        let (verdict, lines) =
            logged(|| rig.intake.process_message(PEER, DkgNetMsg::Dkg(kind), payload));
        assert_eq!(verdict, Verdict::Ignored);
        assert_eq!(
            texts(&lines),
            [(tracing::Level::DEBUG, format!("NetDKG -- ignoring {name} in observer mode"))]
        );
    }
    assert!(rig.host.take().is_empty(), "nothing is asked of the host");
}

#[test]
fn an_unverified_peer_scores_10() {
    let rig = rig();
    for unverified in [8, 9] {
        let payload = framed(DkgMsgKind::Contribution, 100, &base_hash(48), 1);
        if unverified == 9 {
            rig.host.verify(9, Default::default());
        }
        assert_eq!(
            rig.intake.process_message(unverified, CONTRIB, payload),
            score(10, "DKG message from non-verified peer")
        );
    }
}

#[test]
fn a_message_without_type_and_quorum_hash_scores_100() {
    let rig = rig();
    assert_eq!(
        rig.intake.process_message(PEER, CONTRIB, vec![100; 32]),
        score(100, "malformed DKG message")
    );
}

#[test]
fn an_unknown_type_scores_100_with_a_line() {
    let rig = rig();
    let payload = framed(DkgMsgKind::Contribution, 200, &base_hash(48), 1);
    let (verdict, lines) = logged(|| rig.intake.process_message(PEER, CONTRIB, payload));
    assert_eq!(verdict, score(100, ""));
    assert_eq!(texts(&lines), info("NetDKG -- invalid llmqType [200]".into()));
}

#[test]
fn the_base_block_checks_score_as_core() {
    let rig = Rig::new(&[TEST]);
    rig.host.verify(PEER, pro_tx(1));
    let send = |height: i32| {
        let hash = base_hash(height);
        let payload = framed(DkgMsgKind::Contribution, 100, &hash, 1);
        let (verdict, lines) = logged(|| rig.intake.process_message(PEER, CONTRIB, payload));
        (verdict, texts(&lines), uint256_hex(hash.as_bytes()))
    };
    let (verdict, lines, hash) = send(48);
    assert_eq!(verdict, score(10, ""), "unknown: we may lag behind");
    assert_eq!(lines, info(format!("NetDKG -- unknown quorumHash {hash}")));

    rig.chain.add(0);
    let (verdict, lines, hash) = send(0);
    assert_eq!(verdict, score(100, ""));
    assert_eq!(lines, info(format!("NetDKG -- parentless quorumHash {hash}")));

    rig.chain.add(48);
    rig.host.type_enabled.store(false, Ordering::SeqCst);
    let (verdict, lines, _) = send(48);
    assert_eq!(verdict, score(100, ""));
    assert_eq!(lines, info("NetDKG -- llmqType [100] quorums aren't active".into()));
    assert!(rig.host.take().contains(&"enabled 100 48".to_string()), "asked at the base");
    rig.host.type_enabled.store(true, Ordering::SeqCst);

    rig.chain.add(49);
    let (verdict, lines, hash) = send(49);
    assert_eq!(verdict, score(100, ""), "index 1 of a type that does not rotate");
    assert_eq!(lines, info(format!("NetDKG -- invalid quorumHash {hash}")));

    assert_eq!(send(48).0, Verdict::Dispatched);
}

/// DEC-1881: a failed local read in the type check is never the peer's fault.
#[test]
fn a_failed_type_read_drops_the_message_unscored() {
    let rig = Rig::new(&[TEST]);
    rig.host.verify(PEER, pro_tx(1));
    let hash = rig.chain.add(48);
    let send = || {
        let payload = framed(DkgMsgKind::Contribution, 100, &hash, 1);
        logged(|| rig.intake.process_message(PEER, CONTRIB, payload))
    };

    rig.host.type_read_fails.store(true, Ordering::SeqCst);
    let (verdict, lines) = send();
    assert_eq!(verdict, Verdict::ReadFailed);
    assert_eq!(texts(&lines), vec![]);
    let calls = rig.host.take();
    assert!(calls.contains(&"enabled 100 48".to_string()), "{calls:?}");
    assert!(!calls.iter().any(|c| c.starts_with("score ")), "{calls:?}");
    assert!(rig.intake.pop_batch(key(TEST, 0), DkgMsgKind::Contribution, 8).is_empty());

    rig.host.type_read_fails.store(false, Ordering::SeqCst);
    assert_eq!(send().0, Verdict::Dispatched);
    let calls = rig.host.take();
    assert!(calls.contains(&"enabled 100 48".to_string()), "no cached quorum index: {calls:?}");
}

#[test]
fn a_rotated_type_takes_indexes_below_its_active_quorum_count() {
    let t = LlmqType::LlmqTestDip0024;
    let p = params(t);
    let rig = Rig::new(&[t]);
    rig.host.verify(PEER, pro_tx(1));
    rig.chain.rotation.store(true, Ordering::SeqCst);
    let interval = i32::from(p.dkg_interval);
    let count = i32::from(p.signing_active_quorum_count);
    for qi in 0..=count {
        let hash = rig.chain.add(2 * interval + qi);
        let payload = framed(DkgMsgKind::Complaint, t.to_base(), &hash, 1);
        let verdict =
            rig.intake.process_message(PEER, DkgNetMsg::Dkg(DkgMsgKind::Complaint), payload);
        let want = if qi < count {
            Verdict::Dispatched
        } else {
            score(100, "")
        };
        assert_eq!(verdict, want, "qi {qi}");
        if qi < count {
            assert_eq!(rig.intake.pop_batch(key(t, qi), DkgMsgKind::Complaint, 8).len(), 1);
        }
    }
}

#[test]
fn the_size_cap_runs_before_the_framing_and_both_score_100() {
    let rig = rig();
    let mut oversized = framed(DkgMsgKind::Complaint, 100, &base_hash(48), 1);
    oversized.resize(2000, 0);
    let complaint = DkgNetMsg::Dkg(DkgMsgKind::Complaint);
    assert_eq!(
        rig.intake.process_message(PEER, complaint, oversized),
        score(100, "oversized DKG message")
    );
    let mut trailing = framed(DkgMsgKind::Complaint, 100, &base_hash(48), 1);
    trailing.push(0);
    assert_eq!(
        rig.intake.process_message(PEER, complaint, trailing),
        score(100, "malformed DKG message")
    );
    let unframed = framed(DkgMsgKind::Contribution, 100, &base_hash(48), 1);
    assert_eq!(
        rig.intake.process_message(PEER, complaint, unframed),
        score(100, "malformed DKG message"),
        "framed as its command, not its bytes"
    );
    assert!(!rig.host.take().iter().any(|c| c.starts_with("consume")), "no getdata consumed");
}

#[test]
fn an_unrequested_message_scores_10_and_a_late_one_is_taken() {
    let rig = rig();
    let payload = framed(DkgMsgKind::Justification, 100, &base_hash(48), 1);
    let hash = payload_hash(&payload);
    *rig.host.response.lock().unwrap() = GetDataResponse::Unrequested;
    let justify = DkgNetMsg::Dkg(DkgMsgKind::Justification);
    let (verdict, lines) = logged(|| rig.intake.process_message(PEER, justify, payload.clone()));
    assert_eq!(verdict, score(10, "unrequested DKG message"));
    assert_eq!(
        texts(&lines),
        [(
            tracing::Level::DEBUG,
            format!("NetDKG -- received unrequested qjustify {}, peer={PEER}", uint256_hex(&hash))
        )]
    );
    assert!(rig.host.take().contains(&format!("consume {PEER} Justification")));
    let inv = DkgInv {
        kind: DkgMsgKind::Justification,
        hash,
    };
    assert!(!rig.intake.already_have(inv), "not queued");
    *rig.host.response.lock().unwrap() = GetDataResponse::Late;
    assert_eq!(rig.intake.process_message(PEER, justify, payload), Verdict::Dispatched);
    assert!(rig.intake.already_have(inv));
}

#[test]
fn a_type_without_handlers_scores_100() {
    let rig = Rig::new(&[TEST]);
    let other = LlmqType::LlmqTestInstantsend;
    let llmqs = [(TEST, params(TEST)), (other, params(other))];
    let intake = DkgIntake::new(rig.sessions.clone(), llmqs, rig.seams.clone(), rig.host.clone());
    rig.host.verify(PEER, pro_tx(1));
    rig.chain.add(48);
    let payload = framed(DkgMsgKind::Contribution, other.to_base(), &base_hash(48), 1);
    let (verdict, lines) = logged(|| intake.process_message(PEER, CONTRIB, payload));
    assert_eq!(verdict, score(100, ""));
    assert_eq!(texts(&lines), info("NetDKG -- no session handlers for quorumIndex [0]".into()));
}

#[test]
fn a_dispatched_message_is_queued_from_its_peer_and_its_index_cached() {
    let rig = rig();
    let hash = base_hash(48);
    let payload = framed(DkgMsgKind::Contribution, 100, &hash, 1);
    assert_eq!(rig.intake.process_message(PEER, CONTRIB, payload.clone()), Verdict::Dispatched);
    assert_eq!(rig.chain.lookups.load(Ordering::SeqCst), 1);
    rig.chain.bases.lock().unwrap().clear();
    rig.host.type_enabled.store(false, Ordering::SeqCst);
    let second = framed(DkgMsgKind::Contribution, 100, &hash, 2);
    assert_eq!(rig.intake.process_message(PEER, CONTRIB, second.clone()), Verdict::Dispatched);
    assert_eq!(rig.chain.lookups.load(Ordering::SeqCst), 1, "the cache skips the base checks");
    assert_eq!(
        rig.intake.pop_batch(key(TEST, 0), DkgMsgKind::Contribution, 8),
        [(Sender::Peer(PEER), payload), (Sender::Peer(PEER), second)]
    );
}

#[test]
fn a_rejected_message_caches_no_index() {
    let rig = rig();
    *rig.host.response.lock().unwrap() = GetDataResponse::Unrequested;
    let payload = framed(DkgMsgKind::Contribution, 100, &base_hash(48), 1);
    rig.intake.process_message(PEER, CONTRIB, payload.clone());
    *rig.host.response.lock().unwrap() = GetDataResponse::Requested;
    rig.intake.process_message(PEER, CONTRIB, payload);
    assert_eq!(rig.chain.lookups.load(Ordering::SeqCst), 2);
}

#[test]
fn the_index_cache_keeps_the_keep_old_connections_entries_used_last() {
    let rig = rig();
    let keep = usize::from(params(TEST).keep_old_connections);
    let send = |height: i32, salt: u8| {
        let payload = framed(DkgMsgKind::Complaint, 100, &base_hash(height), salt);
        rig.intake.process_message(PEER, DkgNetMsg::Dkg(DkgMsgKind::Complaint), payload)
    };
    let lookups = || rig.chain.lookups.load(Ordering::SeqCst);
    let interval = i32::from(params(TEST).dkg_interval);
    let heights: Vec<i32> = (1..=2 * keep as i32 + 1).map(|i| i * interval).collect();
    for h in &heights {
        rig.chain.add(*h);
    }
    for (i, h) in heights.iter().enumerate().take(2 * keep) {
        assert_eq!(send(*h, i as u8), Verdict::Dispatched);
    }
    let before = lookups();
    assert_eq!(send(heights[0], 200), Verdict::Dispatched, "refreshes the oldest");
    assert_eq!(lookups(), before, "2 * keep entries are all kept");
    assert_eq!(send(heights[2 * keep], 201), Verdict::Dispatched, "past 2 * keep: truncate");
    let hit = |i: usize, salt: u8| {
        let b = lookups();
        send(heights[i], salt);
        lookups() == b
    };
    assert!(keep >= 3, "{keep}");
    assert!(hit(2 * keep, 150), "the newest is kept");
    assert!(hit(0, 151), "the refreshed is kept");
    assert!(hit(2 * keep - 1, 152), "the newest before it is kept");
    assert!(!hit(1, 153), "the oldest unrefreshed is evicted");
}

#[test]
fn already_have_answers_from_every_handlers_seen_sets() {
    let rig = rig();
    let payload = framed(DkgMsgKind::Complaint, 100, &base_hash(48), 1);
    let hash = payload_hash(&payload);
    let inv = |kind| DkgInv {
        kind,
        hash,
    };
    assert!(!rig.intake.already_have(inv(DkgMsgKind::Complaint)));
    rig.intake.process_message(PEER, DkgNetMsg::Dkg(DkgMsgKind::Complaint), payload);
    for kind in KINDS {
        assert!(rig.intake.already_have(inv(kind)), "any queue's seen-set: {kind:?}");
    }
    rig.sporks.spork17.store(false, Ordering::SeqCst);
    assert!(!rig.intake.already_have(inv(DkgMsgKind::Complaint)), "spork 17 off");
    let observer = Rig::observer();
    observer.sporks.spork17.store(false, Ordering::SeqCst);
    assert!(observer.intake.already_have(inv(DkgMsgKind::Contribution)), "an observer has it");
}
