//! T4 (LQ4a-1, LQ4a-2): whole quorums through the justification phase on the reference
//! harness.

use std::collections::{BTreeSet, VecDeque};

use super::*;
use crate::dkg::sim::SimError;

/// Core's static table entry for `t` (`llmq/params.h:138`).
fn params(t: LlmqType) -> LlmqParams {
    dash_params::available_params(t).expect("a known type")
}

fn network(t: LlmqType, n: usize) -> Network {
    Network::new(t, &params(t), n, 0x7e57 + n as u64)
}

/// Each member pops 8 at a time, then runs the Complain phase's pending verify.
fn settle(net: &mut Network) {
    net.drain(|_, _| 8);
    net.verify_pending();
}

fn writers(net: &Network, i: usize, kind: &str) -> Vec<usize> {
    let index = |p| net.members.iter().position(|m| m.pro_tx_hash == p).expect("a member");
    net.nodes[i].store.writers(kind).into_iter().map(index).collect()
}

fn count(net: &Network, i: usize, text: &str) -> usize {
    net.nodes[i].events.iter().filter(|e| e.text == text).count()
}

fn honest(t: LlmqType) -> Network {
    let n = usize::from(params(t).size);
    let mut net = network(t, n);
    for i in 0..n {
        assert!(net.contribute(i).is_some());
    }
    settle(&mut net);
    for i in 0..n {
        let node = &net.nodes[i];
        assert!(net.we_complain(i).is_empty(), "member {i}");
        assert!((0..n).all(|j| !node.session.is_bad(j)));
        assert!(node.session.pending().is_empty());
        assert!(node.misbehaving.is_empty());
        assert_eq!(node.relayed.len(), n);
        for kind in ["V", "E", "S"] {
            assert_eq!(writers(&net, i, kind), (0..n).collect::<Vec<_>>(), "member {i} {kind}");
        }
    }
    net
}

#[test]
fn honest_llmq_test() {
    honest(LlmqType::LlmqTest);
}

#[test]
fn honest_llmq_test_dip0024() {
    honest(LlmqType::LlmqTestDip0024);
}

#[test]
fn honest_llmq_devnet() {
    honest(LlmqType::LlmqDevnet);
}

#[test]
fn honest_llmq_50_60() {
    let net = honest(LlmqType::Llmq50_60);
    for i in 0..50 {
        assert_eq!(count(&net, i, "verified 32 pending contributions"), 1);
        assert_eq!(count(&net, i, "verified 18 pending contributions"), 1);
    }
}

#[test]
fn honest_llmq_100_67() {
    // Three verifies inside a pop, then a remainder of 4 when the Complain phase starts.
    let net = honest(LlmqType::Llmq100_67);
    for i in 0..100 {
        assert_eq!(count(&net, i, "verified 32 pending contributions"), 3);
        assert_eq!(count(&net, i, "verified 4 pending contributions"), 1);
    }
}

#[test]
fn a_missing_contribution_leaves_no_row_and_nothing_pending() {
    let mut net = network(LlmqType::LlmqTest, 3);
    net.contribute(0);
    net.contribute(2);
    settle(&mut net);
    for i in [0, 2] {
        assert_eq!(writers(&net, i, "S"), [0, 2]);
        assert!(net.we_complain(i).is_empty());
    }
}

#[test]
fn five_dropouts_on_devnet_leave_min_size() {
    let p = params(LlmqType::LlmqDevnet);
    assert_eq!(p.size - 5, p.min_size);
    let mut net = network(LlmqType::LlmqDevnet, 12);
    let offline: BTreeSet<usize> = [1, 4, 6, 9, 11].into();
    let online: Vec<usize> = (0..12).filter(|i| !offline.contains(i)).collect();
    for &i in &online {
        net.contribute(i);
    }
    for &i in &offline {
        net.nodes[i].queue.clear();
    }
    settle(&mut net);
    for &i in &online {
        for kind in ["V", "E", "S"] {
            assert_eq!(writers(&net, i, kind), online, "member {i} {kind}");
        }
        assert!(net.we_complain(i).is_empty());
    }
    for &i in &offline {
        assert!(net.nodes[i].store.rows.is_empty());
        assert!(net.nodes[i].session.pending().is_empty());
    }
}

#[test]
fn contribution_lie_on_llmq_test_is_complained_about_by_the_lied_to() {
    let mut net = network(LlmqType::LlmqTest, 3);
    net.nodes[0].sim.set(SimError::ContributionLie, 100).unwrap();
    for i in 0..3 {
        net.contribute(i);
    }
    settle(&mut net);
    assert_eq!(net.we_complain(0), BTreeSet::new());
    assert_eq!(net.we_complain(1), [0].into());
    assert_eq!(net.we_complain(2), [0].into());
    assert_eq!(writers(&net, 1, "S"), [1, 2]);
}

#[test]
fn a_modified_sender_is_complained_about_by_exactly_the_lied_to() {
    let mut net = network(LlmqType::LlmqDevnet, 12);
    let lie_to: BTreeSet<usize> = [2, 5].into();
    for i in 0..12 {
        if i == 3 {
            net.contribute_modified(3, &lie_to);
        } else {
            net.contribute(i);
        }
    }
    settle(&mut net);
    for i in 0..12 {
        let want = if lie_to.contains(&i) {
            [3].into()
        } else {
            BTreeSet::new()
        };
        assert_eq!(net.we_complain(i), want, "member {i}");
        assert_eq!(writers(&net, i, "E").len(), 12);
        assert_eq!(writers(&net, i, "S").len(), 12 - usize::from(lie_to.contains(&i)));
    }
}

/// Core's 32-grouping over the senders as delivered: each group's members not yet complained
/// about get their `qdkg_E` row, then the liars in it are complained about.
fn predicted_rows(delivered: &[usize], lied_to_me: &BTreeSet<usize>) -> Vec<usize> {
    let mut rows = Vec::new();
    let mut complained = BTreeSet::<usize>::new();
    for group in delivered.chunks(32) {
        let verified: Vec<usize> =
            group.iter().copied().filter(|s| !complained.contains(s)).collect();
        complained.extend(verified.iter().filter(|s| lied_to_me.contains(s)));
        rows.extend(verified);
    }
    rows
}

#[test]
fn reorder_duplicates_and_pop_boundaries_keep_the_verdicts_and_cores_row_order() {
    // 40 members, enough for a 32-group and a remainder; threshold 2 keeps each run cheap.
    let forty = LlmqParams {
        size: 40,
        min_size: 40,
        ..params(LlmqType::LlmqTest)
    };
    let liar = 7;
    let lie_to: BTreeSet<usize> = [3, 20, 35].into();
    let run = |deliver: &dyn Fn(usize, VecDeque<Queued>) -> VecDeque<Queued>,
               pop_size: &dyn Fn(usize, usize) -> usize| {
        let mut net = Network::new(LlmqType::LlmqTest, &forty, 40, 0x40);
        for i in 0..40 {
            if i == liar {
                net.contribute_modified(liar, &lie_to);
            } else {
                net.contribute(i);
            }
        }
        for (i, node) in net.nodes.iter_mut().enumerate() {
            node.queue = deliver(i, std::mem::take(&mut node.queue));
        }
        let delivered: Vec<Vec<usize>> = net
            .nodes
            .iter()
            .enumerate()
            .map(|(i, node)| {
                let sender = |s: &Sender<usize>| match s {
                    Sender::Own => i,
                    Sender::Peer(p) => *p,
                };
                node.queue.iter().map(|(s, _)| sender(s)).collect()
            })
            .collect();
        net.drain(pop_size);
        net.verify_pending();
        for (i, delivered) in delivered.iter().enumerate() {
            let lied = if lie_to.contains(&i) {
                [liar].into()
            } else {
                BTreeSet::new()
            };
            assert_eq!(writers(&net, i, "E"), predicted_rows(delivered, &lied), "member {i}");
        }
        (0..40).map(|i| net.we_complain(i)).collect::<Vec<_>>()
    };

    let keep = |_: usize, q: VecDeque<Queued>| q;
    let reverse = |_: usize, q: VecDeque<Queued>| q.into_iter().rev().collect();
    let rotate = |i: usize, mut q: VecDeque<Queued>| {
        q.rotate_left(i % q.len());
        q
    };
    // Harness-only: Core's `CDKGPendingMessages` drops a payload it has already queued, so a
    // queue never holds the same bytes twice; this checks the session alone stays put.
    let twice = |_: usize, q: VecDeque<Queued>| q.iter().chain(q.iter()).cloned().collect();
    let eight = |_: usize, _: usize| 8;
    let ragged = |i: usize, k: usize| 1 + (i + 3 * k) % 8;

    let baseline = run(&keep, &eight);
    for (i, complained) in baseline.iter().enumerate() {
        let want = if lie_to.contains(&i) {
            [liar].into()
        } else {
            BTreeSet::new()
        };
        assert_eq!(*complained, want, "member {i}");
    }
    assert_eq!(run(&keep, &ragged), baseline);
    assert_eq!(run(&reverse, &eight), baseline);
    assert_eq!(run(&rotate, &ragged), baseline);
    assert_eq!(run(&twice, &eight), baseline);
    assert_eq!(run(&twice, &ragged), baseline);
}

/// Phases 3 and 4 for every member: each complains over `conn(i)` and the complaints are
/// drained; each justifies, sending unless `silent`, and the justifications are drained; then
/// each marks the open complaints as `VerifyAndCommit` starts. Each member's valid members.
fn through_phase_4(
    net: &mut Network,
    conn: impl Fn(usize) -> ConnSnapshot,
    silent: &BTreeSet<usize>,
) -> Vec<Vec<bool>> {
    let n = net.nodes.len();
    net.enter(DkgMsgKind::Complaint);
    for i in 0..n {
        net.complain(i, &conn(i));
    }
    net.drain(|_, _| 8);
    net.enter(DkgMsgKind::Justification);
    for i in 0..n {
        net.justify(i, !silent.contains(&i));
    }
    net.drain(|_, _| 8);
    for node in &net.nodes {
        assert!(node.misbehaving.is_empty());
    }
    (0..n)
        .map(|i| {
            let node = &mut net.nodes[i];
            node.session.mark_open_complaints(&mut node.debug);
            node.collect_events();
            node.session.valid_members()
        })
        .collect()
}

fn all_but(n: usize, out: &[usize]) -> Vec<bool> {
    (0..n).map(|i| !out.contains(&i)).collect()
}

#[test]
fn an_honest_quorum_sends_no_complaint_and_stays_valid() {
    for t in [LlmqType::LlmqTest, LlmqType::LlmqTestDip0024] {
        let mut net = honest(t);
        let n = net.nodes.len();
        let valid = through_phase_4(&mut net, |_| ConnSnapshot::default(), &BTreeSet::new());
        assert_eq!(valid, vec![all_but(n, &[]); n], "{t:?}");
        assert!(net.nodes.iter().all(|node| node.relayed.len() == n), "contributions only");
    }
}

/// Devnet's 12 members, member 3 lying to members 2 and 5.
fn lied_to() -> Network {
    let mut net = network(LlmqType::LlmqDevnet, 12);
    for i in 0..12 {
        if i == 3 {
            net.contribute_modified(3, &[2, 5].into());
        } else {
            net.contribute(i);
        }
    }
    settle(&mut net);
    net
}

#[test]
fn a_justified_lie_closes_every_complaint() {
    let mut net = lied_to();
    let valid = through_phase_4(&mut net, |_| ConnSnapshot::default(), &BTreeSet::new());
    assert_eq!(valid, vec![all_but(12, &[]); 12]);
    for i in 0..12 {
        let s = &net.nodes[i].session;
        assert!((0..12).all(|j| s.complaints_from_others(j).is_empty()), "member {i}");
        assert!(net.we_complain(i).is_empty(), "member {i}: the justification answered it");
        assert_eq!(writers(&net, i, "S").len(), 12, "member {i}");
    }
    for i in [2, 5] {
        assert_eq!(writers(&net, i, "S").last(), Some(&3), "member {i}: the revealed share");
    }
}

#[test]
fn an_unjustified_lie_leaves_the_liar_invalid_everywhere() {
    let mut net = lied_to();
    let valid = through_phase_4(&mut net, |_| ConnSnapshot::default(), &[3].into());
    assert_eq!(valid, vec![all_but(12, &[3]); 12], "the liar's own view too");
    let p3 = net.members[3].pro_tx_hash;
    for i in 0..12 {
        assert_eq!(count(&net, i, &format!("    {p3}")), 1, "member {i}");
        assert_eq!(count(&net, i, "  members with open complaints and now marked as bad:"), 1);
    }
}

#[test]
fn bad_connection_votes_reach_the_threshold_everywhere() {
    // Everyone but member 7 misses it; with 11 votes of 7 needed each member marks it bad in
    // VerifyAndJustify, member 7 included.
    let mut net = lied_to();
    let members: Vec<ProTxHash> = net.members.iter().map(|m| m.pro_tx_hash).collect();
    let conn = |i: usize| {
        ConnSnapshot::connected((0..12).filter(|&j| i == 7 || j != 7).map(|j| members[j]))
    };
    let valid = through_phase_4(&mut net, conn, &BTreeSet::new());
    assert_eq!(valid, vec![all_but(12, &[7]); 12]);
    let line = format!("{} marked as bad as 11 other members voted for this", members[7]);
    for i in 0..12 {
        assert_eq!(count(&net, i, &line), 1, "member {i}");
        assert_eq!(net.nodes[i].session.is_bad_connection(7), i != 7);
    }
}
