//! DK1-L2's round simulation (plan §4 L2): `llmq_test` members, each a [`DkgRuntime`] on one
//! fake chain, with ND1's queues and batch wrapper as fakes, counting seams and a lockstep clock
//! the tips drive. Items (a)-(j) of the plan.

#[path = "dkg_round/sim.rs"]
mod sim;

use std::collections::BTreeSet;
use std::sync::Arc;

use dash_llmq::dkg::conn::ConnSnapshot;
use dash_llmq::dkg::reference::{Network, NetworkSetup, seeded_rng};
use dash_llmq::dkg::rng::SeededRng;
use dash_llmq::dkg::session::{DkgMsgKind, QuorumBase};
use dash_params::LlmqType;
use dashd_quorums::dkg::{DkgRuntime, DkgSessions, QuorumPhase, StartError};
use sim::{Sim, SimConfig, commitment_bytes, mined, quorum_hash};

const T: LlmqType = LlmqType::LlmqTest;

/// The final commitment of a reference network with `sim`'s members, seeds and base at
/// `base_height`: every member honest, every message delivered.
fn reference_commitment(sim: &Sim, base_height: i32) -> Vec<u8> {
    let (t, params) = sim.cfg.llmqs[0];
    let n = sim.cfg.members;
    let setup = NetworkSetup {
        base: QuorumBase {
            hash: quorum_hash(base_height, 0),
            height: base_height,
        },
        ..NetworkSetup::new(&params)
    };
    let weights = || Box::new(SeededRng::new(0x5eed)) as _;
    let mut net = Network::with_setup(t, &params, n, sim.cfg.seed, weights, seeded_rng, setup);
    assert_eq!(net.members, sim.test_members);
    for i in 0..n {
        net.contribute(i).expect("a contribution");
    }
    net.drain(|_, _| 8);
    net.enter(DkgMsgKind::Complaint);
    for i in 0..n {
        assert!(net.complain(i, &ConnSnapshot::default()).is_none());
    }
    net.drain(|_, _| 8);
    net.enter(DkgMsgKind::Justification);
    for i in 0..n {
        assert!(net.justify(i, true).is_none());
    }
    net.drain(|_, _| 8);
    net.enter(DkgMsgKind::PrematureCommitment);
    let bls = net.bls;
    for i in 0..n {
        let qc = net.nodes[i].step(bls, |s, env| s.verify_and_commit(env)).expect("a pcommit");
        net.send(i, &qc.payload(bls.scheme()));
    }
    net.drain(|_, _| 8);
    let finals: Vec<Vec<u8>> = (0..n)
        .map(|i| {
            let finals = net.nodes[i].step(bls, |s, env| s.finalize_commitments(env));
            assert_eq!(finals.len(), 1);
            commitment_bytes(&finals[0])
        })
        .collect();
    assert!(finals.windows(2).all(|w| w[0] == w[1]));
    finals[0].clone()
}

/// The phases each node's debug status went through, sampled while every thread sleeps, until
/// every member mined.
fn run_round(sim: &mut Sim) -> Vec<Vec<i32>> {
    let mut phases = vec![Vec::new(); sim.nodes.len()];
    sim.start();
    sim.run_until(30, |sim| {
        for (i, node) in sim.nodes.iter().enumerate() {
            let key = node.sessions.keys()[0];
            if let Some(s) = node.debug.session(key.llmq_type, 0)
                && phases[i].last() != Some(&s.phase)
            {
                phases[i].push(s.phase);
            }
        }
        sim.all_mined(1)
    });
    phases
}

/// (a) and (c) for one seeded run of `n` members.
fn check_round(n: usize, seed: u64) -> Sim {
    let mut sim = Sim::new(SimConfig::llmq_test(n, seed));
    let phases = run_round(&mut sim);
    let expected = reference_commitment(&sim, 48);
    for (i, node) in sim.nodes.iter().enumerate() {
        let mined = mined(node);
        assert_eq!(mined.len(), 1, "node {i}, seed {seed:#x}");
        assert_eq!(commitment_bytes(&mined[0]), expected, "node {i}, seed {seed:#x}");
        assert_eq!(mined[0].quorum_hash(), quorum_hash(48, 0));
        assert_eq!(phases[i], [1, 2, 3, 4, 5, 6], "node {i}, seed {seed:#x}");
        assert_eq!(sim::misbehaving(node), 0);
        assert!(sim::faults(node).is_empty());
    }
    sim
}

#[test]
fn three_members_mine_the_reference_commitment() {
    check_round(3, 0x3001);
}

#[test]
fn five_members_mine_the_reference_commitment() {
    check_round(5, 0x5001);
}

/// The acceptance run: 200 seeds, N = 3 and 5.
#[test]
fn two_hundred_seeded_rounds() {
    for k in 0..200u64 {
        let n = if k % 2 == 0 {
            3
        } else {
            5
        };
        check_round(n, 0x7700_0000 + k);
    }
}

/// `starting`/`done` of a wait, as Core formats them.
fn wait_lines(func: &str, cur: i32, next: i32) -> [String; 2] {
    let at = |what| {
        format!(
            "ActiveDKGSessionHandler::{func} -- llmq_test qi[0] - {what}, curPhase={cur}, nextPhase={next}"
        )
    };
    [at("starting"), at("done")]
}

/// A session record's key, `CBLSPublicKey::ToString()`, as `<pk>`.
fn mask_keys(record: &str) -> String {
    record
        .split('\n')
        .map(|line| {
            for field in ["pubKeyShare=", "quorumPublicKey="] {
                if let Some(at) = line.find(field) {
                    let key = &line[at + field.len()..];
                    assert!(
                        key.len() == 96 && key.bytes().all(|b| b.is_ascii_hexdigit()),
                        "{line}"
                    );
                    return format!("{}<pk>", &line[..at + field.len()]);
                }
            }
            line.to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A batch's record: its sender's `proTxHash` as `<m>`, and the sender.
fn mask_sender(record: &str) -> (String, String) {
    let at = record.find(" from ").expect("a sender") + " from ".len();
    let sender = &record[at..at + 64];
    (format!("{}<m>{}", &record[..at], &record[at + 64..]), sender.to_owned())
}

/// (b) Member 1's lines through one round, as Core v24.0.0 writes them under `llmq-dkg`:
/// `net_dkg.cpp:785-949` and `dkgsessionhandler.cpp:110-255` on their own, and the session's
/// `CDKGLogger` records one write per record (the cxxtimer fields are not kept). A batch's
/// `ReceiveMessage` records land in whichever wait of their phase processes them, from senders in
/// arrival order; the rest is fixed.
#[test]
fn handler_lines_match_core() {
    let sim = check_round(3, 0x3001);
    let node = &sim.nodes[1];
    let me: String = node.pro_tx.to_string().chars().take(4).collect();
    let lines: Vec<String> = node.capture.dkg_lines().iter().map(|l| mask_keys(l)).collect();
    let phase = |cur: i32, next: i32, sleep: u32, records: &[&str]| {
        let [start, done] = wait_lines("HandlePhase", cur, next);
        let [wait_start, wait_done] = wait_lines("WaitForNextPhase", cur, next);
        let mut out = vec![
            start,
            format!(
                "ActiveDKGSessionHandler::SleepBeforePhase -- llmq_test qi[0] - starting sleep for {sleep} ms, curPhase={cur}"
            ),
            format!(
                "ActiveDKGSessionHandler::SleepBeforePhase -- llmq_test qi[0] - done, curPhase={cur}"
            ),
        ];
        out.extend(records.iter().map(|r| (*r).to_owned()));
        out.extend([wait_start, wait_done, done]);
        out
    };
    let round = "NetDKG::PhaseHandlerThread -- llmq_test qi[0] - starting HandleDKGRound";
    let mut expected = vec![round.to_owned()];
    expected.extend(wait_lines("WaitForNextPhase", -1, 1));
    expected.push(format!(
        "    forMember[{me}] relayMembers[0000 | 0000 | ]\n    initialized as member. mns=3"
    ));
    expected.extend(wait_lines("WaitForNextPhase", 1, 2));
    expected.extend(phase(
        2,
        3,
        1000,
        &[
            "    generating contributions\n    generated contributions",
            "    sending contributions\n    encrypted contributions",
        ],
    ));
    expected.extend(phase(
        3,
        4,
        1000,
        &["    verified 3 pending contributions", "    verified contributions"],
    ));
    expected.extend(phase(4, 5, 1000, &[]));
    expected.extend(phase(
        5,
        6,
        2000,
        &["    sending commitment\n    pubKeyShare=<pk>\n    built premature commitment"],
    ));
    expected
        .push("    final commitment: validMembers=3, signers=3, quorumPublicKey=<pk>".to_owned());
    expected.push(round.to_owned());

    let mut fixed = Vec::new();
    let mut received = Vec::new();
    for line in &lines {
        if fixed.len() == expected.len() {
            break;
        }
        if line.starts_with("    received ") {
            let (masked, sender) = mask_sender(line);
            received.push((fixed.len(), masked, sender));
        } else {
            fixed.push(line.clone());
        }
    }
    assert_eq!(me, "0000");
    assert_eq!(fixed, expected);

    let window = |cur: i32| {
        let [start, done] = wait_lines("HandlePhase", cur, cur + 1);
        let at = |l: &String| expected.iter().position(|e| e == l).expect("a handler line");
        (at(&start) + 1)..=at(&done)
    };
    let members: BTreeSet<String> = sim.nodes.iter().map(|n| n.pro_tx.to_string()).collect();
    let records = |templates: Vec<String>, cur: i32| {
        let ours: Vec<_> = received.iter().filter(|(_, m, _)| templates.contains(m)).collect();
        assert_eq!(ours.iter().map(|(_, m, _)| m.clone()).collect::<Vec<_>>(), templates);
        assert!(ours.iter().all(|(at, ..)| window(cur).contains(at)), "{ours:?}");
        let senders: BTreeSet<String> = ours.iter().map(|(.., s)| s.clone()).collect();
        assert_eq!(senders, members);
    };
    records(
        (1..=3)
            .map(|k| {
                format!(
                    "    received contribution from <m>\n    received and relayed contribution. received={k}/3\n    decrypted our contribution share"
                )
            })
            .collect(),
        2,
    );
    records(
        (1..=3)
            .map(|k| {
                format!(
                    "    received premature commitment from <m>. validMembers=3\n    verified premature commitment. received={k}/3"
                )
            })
            .collect(),
        5,
    );
    assert_eq!(received.len(), 6, "{received:?}");
}

#[test]
fn start_twice_is_an_error_and_an_observer_has_no_threads() {
    let mut sim = Sim::new(SimConfig::llmq_test(3, 0x1));
    sim.start();
    assert_eq!(sim.nodes[0].runtime.start(), Err(StartError::AlreadyStarted));
    sim.stop();
    let observer = DkgRuntime::new(
        Arc::new(DkgSessions::observer([(T, sim::chain_llmq(T))], sim.nodes[0].sessions_seams())),
        sim.nodes[0].round_seams(),
    );
    assert_eq!(observer.start(), Ok(()));
    assert_eq!(observer.start(), Ok(()));
    observer.stop();
}

/// `node`'s `LogPrintf` lines.
fn infos(node: &sim::Node) -> Vec<String> {
    node.capture
        .lines()
        .into_iter()
        .filter(|l| l.level == tracing::Level::INFO)
        .map(|l| l.text)
        .collect()
}

/// The lines of `node` that contain `needle`.
fn lines_with(sim: &Sim, node: usize, needle: &str) -> Vec<String> {
    sim.nodes[node].capture.dkg_lines().into_iter().filter(|l| l.contains(needle)).collect()
}

/// (d) A reorg of the base while the members justify aborts the round, marks the session
/// aborted, and the next interval's round on the new chain succeeds.
#[test]
fn a_hash_change_mid_justify_aborts_and_the_next_round_succeeds() {
    let mut sim = Sim::new(SimConfig::llmq_test(3, 0xd001));
    sim.start();
    sim.run_to(54);
    assert!(
        sim.nodes.iter().all(|n| n.sessions.phase(n.sessions.keys()[0]) == QuorumPhase::Justify)
    );
    sim.reorg_from(40);
    let fork = sim.fork;
    sim.run_until(4, |s| {
        s.nodes.iter().all(|n| n.debug.session(T, 0).is_some_and(|st| st.aborted))
    });
    for (i, node) in sim.nodes.iter().enumerate() {
        assert!(mined(node).is_empty(), "node {i}");
        let aborts = lines_with(&sim, i, "aborting due unexpected");
        assert_eq!(aborts.len(), 1, "node {i}: {aborts:?}");
        let in_sleep = "SleepBeforePhase -- llmq_test qi[0] - aborting due unexpected phase/expectedQuorumHash change";
        let in_wait = "WaitForNextPhase -- llmq_test qi[0] - aborting due unexpected expectedQuorumHash change";
        let marked = if aborts[0].ends_with(in_sleep) {
            QuorumPhase::Complain
        } else {
            assert!(aborts[0].ends_with(in_wait), "{aborts:?}");
            QuorumPhase::Justify
        };
        let status = node.debug.session(T, 0).expect("a status");
        assert_eq!(status.phase, marked as i32, "node {i}: {aborts:?}");
        assert_eq!(
            lines_with(&sim, i, "aborted current DKG session"),
            ["NetDKG::PhaseHandlerThread -- llmq_test qi[0] - aborted current DKG session"]
        );
    }
    sim.run_until(30, |s| s.all_mined(1));
    for node in &sim.nodes {
        let mined = mined(node);
        assert_eq!(mined.len(), 1);
        assert_eq!(mined[0].quorum_hash(), quorum_hash(72, fork));
    }
}

/// (e) An interrupt while a member sleeps before its phase ends the wait at once, and stop
/// joins every thread.
#[test]
fn interrupt_during_a_sleep_ends_within_one_tick_and_stop_joins() {
    let mut sim = Sim::new(SimConfig::llmq_test(3, 0xe001));
    sim.start();
    let sleeping = "SleepBeforePhase -- llmq_test qi[0] - starting sleep for 2000 ms, curPhase=2";
    sim.run_until(10, |s| s.nodes[2].capture.dkg_lines().iter().any(|l| l.ends_with(sleeping)));
    assert!(lines_with(&sim, 2, "SleepBeforePhase -- llmq_test qi[0] - done").is_empty());
    sim.stop();
    assert!(sim.clock.ticks_after_free() <= 3, "one tick per thread at most");
    let lines = sim.nodes[2].capture.dkg_lines();
    let n = lines.len();
    assert_eq!(
        lines[n - 2..],
        [
            "ActiveDKGSessionHandler::SleepBeforePhase -- llmq_test qi[0] - aborting due to stop/shutdown requested",
            "NetDKG::PhaseHandlerThread -- llmq_test qi[0] - aborted current DKG session",
        ]
    );
    assert!(sim.nodes[2].debug.session(T, 0).expect("a status").aborted);
    let infos = infos(&sim.nodes[2]);
    assert_eq!(infos.first().map(String::as_str), Some("llmq-100-0 thread start"));
    assert_eq!(infos.last().map(String::as_str), Some("llmq-100-0 thread exit"));
}

/// (f) `Init` refused below minSize: the slot stays null and every lookup misses until the
/// quorum hash changes; then the round aborts, and the next interval's round succeeds.
#[test]
fn an_init_refusal_waits_for_a_new_quorum_then_aborts() {
    let mut sim = Sim::new(SimConfig::llmq_test(3, 0xf001));
    for node in &sim.nodes {
        sim::short_at(node, 48);
    }
    sim.start();
    while sim.height < 72 {
        sim.step();
        if (48..72).contains(&sim.height) {
            for node in &sim.nodes {
                assert!(node.sessions.handlers()[0].session().is_none());
                assert!(node.debug.session(T, 0).is_none());
            }
        }
    }
    sim.run_until(30, |s| s.all_mined(1));
    for (i, node) in sim.nodes.iter().enumerate() {
        assert_eq!(mined(node)[0].quorum_hash(), quorum_hash(72, 0));
        let lines = node.capture.dkg_lines();
        let refused =
            lines.iter().position(|l| l == "    not enough members (1 < 2), aborting init");
        let refused = refused.unwrap_or_else(|| panic!("node {i}: {lines:#?}"));
        assert_eq!(
            lines[refused + 1..refused + 4],
            [
                "ActiveDKGSessionHandler::WaitForNewQuorum -- llmq_test qi[0]- starting",
                "ActiveDKGSessionHandler::WaitForNewQuorum -- llmq_test qi[0] - done",
                "NetDKG::PhaseHandlerThread -- llmq_test qi[0] - aborted current DKG session",
            ]
        );
        let infos: Vec<String> =
            infos(node).into_iter().filter(|l| l.contains("InitNewQuorum")).collect();
        assert_eq!(
            infos,
            [
                "ActiveDKGSessionHandler::InitNewQuorum -- height[48] quorum initialization failed for llmq_test qi[0]",
                "ActiveDKGSessionHandler::InitNewQuorum -- height[72] quorum initialization OK for llmq_test qi[0]",
            ]
        );
    }
}

fn single_member(reject: bool) -> SimConfig {
    let mut cfg = SimConfig::llmq_test(1, 0x9001);
    cfg.llmqs = vec![(T, sim::test_params(sim::chain_llmq(T), 1, 1))];
    cfg.accept_all = !reject;
    cfg.reject_all = reject;
    cfg
}

/// (g) A single-member type finalizes its own commitment at Initialized, then waits for
/// Contribute: no connections, no phases.
#[test]
fn a_single_member_type_mines_its_own_commitment() {
    let mut sim = Sim::new(single_member(false));
    let phases = run_round(&mut sim);
    assert_eq!(phases[0], [6]);
    sim.run_to(50);
    let node = &sim.nodes[0];
    let qc = &mined(node)[0];
    assert_eq!(qc.quorum_hash(), quorum_hash(48, 0));
    assert_eq!(node.mineable.relayed.lock().map(|r| r.len()).unwrap_or(0), 1);
    assert!(sim::connection_calls(node).is_empty());
    assert_eq!(node.trace.get(), ["access#1", "access#2", "verify"]);
    let lines = node.capture.dkg_lines();
    assert!(lines.iter().all(|l| !l.contains("HandlePhase")));
    let [start, done] = wait_lines("WaitForNextPhase", 1, 2);
    assert!(lines.contains(&start) && lines.contains(&done), "{lines:#?}");
    assert!(lines.iter().any(|l| l.starts_with("    final commitment: validMembers=1, signers=1")));
}

/// R-e: `FinalizeSingleCommitment`'s refused `Verify` (Core's assert) reaches the fail-stop
/// hook and ends the thread.
#[test]
fn an_unverified_single_commitment_fail_stops() {
    let mut sim = Sim::new(single_member(true));
    sim.start();
    sim.run_until(4, |s| !sim::faults(&s.nodes[0]).is_empty());
    let faults = sim::faults(&sim.nodes[0]);
    assert_eq!(faults.len(), 1);
    assert_eq!(faults[0].1, "SingleCommitmentUnverified");
    assert!(mined(&sim.nodes[0]).is_empty());
    sim.stop();
    let infos = infos(&sim.nodes[0]);
    assert!(infos[0].ends_with(" thread start"), "{infos:?}");
    let name = infos[0].trim_end_matches(" thread start");
    assert_eq!(
        infos.last().map(String::as_str),
        Some(
            format!(
                "\n\n************************\n\"{name}\" raised an exception\nSingleCommitmentUnverified"
            )
            .as_str()
        )
    );
    assert!(!infos.iter().any(|l| l.ends_with(" thread exit")));
}

/// R-e: ND1's dkgdb fail-stop passes through the phase thread to the hook; the other members
/// still mine.
#[test]
fn a_dkgdb_panic_fail_stops_that_node_only() {
    let mut cfg = SimConfig::llmq_test(3, 0xab01);
    cfg.store_panics = Some(0);
    let mut sim = Sim::new(cfg);
    sim.start();
    sim.run_until(30, |s| s.nodes[1..].iter().all(|n| !mined(n).is_empty()));
    let faults = sim::faults(&sim.nodes[0]);
    assert_eq!(faults.len(), 1);
    assert_eq!(faults[0].1, "dkgdb read failed");
    assert!(sim.nodes[1..].iter().all(|n| sim::faults(n).is_empty()));
}

/// (h) A masternode outside the quorum: `ensure` with a null proTxHash, no probes, no sleep,
/// no start message, an observer session.
#[test]
fn a_non_member_gets_ensure_but_no_probes_sleep_or_message() {
    let mut cfg = SimConfig::llmq_test(3, 0x4001);
    cfg.non_members = 1;
    let mut sim = Sim::new(cfg);
    run_round(&mut sim);
    sim.run_to(59);
    let node = &sim.nodes[3];
    assert_eq!(
        sim::connection_calls(node),
        [(T, 48, dash_evo::types::ProTxHash::default(), false, false)]
    );
    let trace = node.trace.get();
    assert_eq!(trace[..2], ["access#1", "ensure"]);
    assert!(trace[2..].iter().all(|t| t == "access#3/4"), "{trace:?}");
    assert_eq!(trace.len(), 2 + 3, "one read per members' pcommit: {trace:?}");
    assert!(sim::sent(&sim, 3).is_empty());
    assert!(mined(node).is_empty());
    assert!(lines_with(&sim, 3, "SleepBeforePhase").is_empty());
    assert_eq!(lines_with(&sim, 3, "initialized as"), ["    initialized as observer. mns=3"]);
    for i in 0..3 {
        let calls = sim::connection_calls(&sim.nodes[i]);
        assert_eq!(calls, [(T, 48, sim.nodes[i].pro_tx, false, true)]);
    }
}

/// (i) `llmq_test_dip0024`'s second handler starts no round while rotation is off, and starts
/// at the next cycle once it is on.
#[test]
fn dip0024_index_1_starts_only_once_rotation_is_on() {
    let t = LlmqType::LlmqTestDip0024;
    let mut cfg = SimConfig::llmq_test(4, 0x2401);
    cfg.llmqs = vec![(t, sim::chain_llmq(t))];
    cfg.accept_all = true;
    let mut sim = Sim::new(cfg);
    sim.start();
    sim.run_to(62);
    let qi1 = |sim: &Sim| {
        sim.nodes[0]
            .capture
            .dkg_lines()
            .into_iter()
            .filter(|l| {
                l.contains("qi[1]")
                    && !l.contains(
                        "WaitForNextPhase -- llmq_test_dip0024 qi[1] - starting, curPhase=-1",
                    )
            })
            .filter(|l| !l.contains("starting HandleDKGRound"))
            .collect::<Vec<_>>()
    };
    assert!(qi1(&sim).is_empty(), "{:#?}", qi1(&sim));
    let bases: Vec<i32> = sim::connection_calls(&sim.nodes[0]).iter().map(|c| c.1).collect();
    assert_eq!(bases, [48]);
    for node in &sim.nodes {
        assert_eq!(node.sessions.phase(node.sessions.keys()[1]), QuorumPhase::Idle);
    }
    sim.chain.rotation.store(true, std::sync::atomic::Ordering::SeqCst);
    sim.run_to(76);
    let bases: Vec<i32> = sim::connection_calls(&sim.nodes[0]).iter().map(|c| c.1).collect();
    assert_eq!(bases, [48, 72, 73]);
    assert!(
        qi1(&sim).iter().any(|l| l == "ActiveDKGSessionHandler::WaitForNextPhase -- llmq_test_dip0024 qi[1] - done, curPhase=-1, nextPhase=1"),
        "{:#?}",
        qi1(&sim)
    );
}

/// R-d: no runtime lock is held across a seam call: from inside every seam call, another
/// thread reads every handler's phase, a lookup and every debug status.
#[test]
fn no_lock_is_held_across_a_seam_call() {
    let mut sim = Sim::new(SimConfig::llmq_test(3, 0x1001));
    sim.probe.enabled.store(true, std::sync::atomic::Ordering::SeqCst);
    run_round(&mut sim);
    assert!(sim.probe.checks.load(std::sync::atomic::Ordering::SeqCst) > 50);
    assert!(sim.probe.held.lock().map(|h| h.is_empty()).unwrap_or(false));
}

/// SW-V24's `DkgConnections` reads V, which refuses a thread with LO-9's no-wait marker: the
/// phase threads are plain threads, unmarked even when a marked thread starts them.
#[test]
fn phase_threads_are_not_no_wait_marked() {
    use dashd_chainstate::chain::{NoWaitGuard, no_wait_marked};
    let mut sim = Sim::new(SimConfig::llmq_test(3, 0x10a9));
    {
        let _marked = NoWaitGuard::set();
        assert!(no_wait_marked());
        sim.start();
    }
    sim.run_until(10, |s| s.nodes.iter().all(|n| !sim::connection_calls(n).is_empty()));
    for node in &sim.nodes {
        assert_eq!(*node.connections.marked.lock().expect("marked"), [false]);
    }
}

/// (j) Each member's members and connection accesses at Core's points (LQ4 §3.4.1): #1 and
/// #2 in `Init`, then `EnsureQuorumConnections` and the probes, the Complain start's
/// connection snapshot, #3 in `SendCommitment` and #4 per pcommit (own included), then #5
/// and #6 in the one finalized group's `Verify`.
#[test]
fn members_and_connection_accesses_sit_at_cores_points() {
    for (n, seed) in [(3, 0x3001), (5, 0x5001)] {
        let sim = check_round(n, seed);
        let mut expected = vec!["access#1", "access#2", "ensure", "probes", "conn_snapshot"];
        expected.extend(std::iter::repeat_n("access#3/4", 1 + n));
        expected.extend(["access#5", "access#6"]);
        for (i, node) in sim.nodes.iter().enumerate() {
            assert_eq!(node.trace.get(), expected, "n {n}, node {i}");
        }
    }
}
