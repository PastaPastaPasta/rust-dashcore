//! T3-C through phase 4 (LQ4a-2): each member of a captured session replayed from the
//! transcript, contributions to the commitment's `validMembers`, against what Core sent.
//!
//! A member's run adopts its own captured contribution, receives every captured contribution,
//! complaint and justification in capture order in pops of 8, and complains, justifies and
//! marks the open complaints between them. Its complaint and justification must be the bytes
//! Core sent, and its `validMembers` those of its captured premature commitment.
//!
//! Its own true shares are those of its polynomial the capture shows that fit its verification
//! vector at the recipient's id: the shares its contribution's recipients decrypted, and the
//! keys its justification reveals. A share that does not fit stays unknown (invalid), so a
//! justification lie replays only from its recorded draw.

use dash_params::LlmqParams;

use super::*;
use crate::dkg::bls::member_id;
use crate::dkg::conn::{ConnFacts, ConnSnapshot};
use crate::dkg::rng::{CoreRng, SeededRng};
use crate::dkg::session::fixture::{BLS, batch_of};
use crate::dkg::wire::{Complaint, Justification, PrematureCommitment};

const DKG_12_7: (&str, &str) =
    ("dkg_12_7.json5", "4832e68ebf8b7a985cf12b68f821055a9ff2548b87689791a6b04a4fa6da9b08");
const SIMPLEPOSE: (&str, &str) =
    ("dkg_simplepose.json5", "d2277fbb0a0fd3295ffda7f426770f6c5438eb876806d4d9338a3fdc733205bc");

const FLAG: BlsSchemeKind = BlsSchemeKind::Basic;

/// One captured session, read.
struct Transcript {
    label: String,
    params: LlmqParams,
    quorum_hash: QuorumHash,
    members: Vec<TestMember>,
    keys: Vec<TestOperatorKey>,
    /// `(sender, raw)` in capture order, per kind.
    contributions: Vec<(usize, Vec<u8>)>,
    complaints: Vec<(usize, Vec<u8>)>,
    justifications: Vec<(usize, Vec<u8>)>,
    /// Per member, the shares of its polynomial it would reveal (see the module docs).
    own_shares: Vec<Vec<BlsSkValue>>,
    /// `(sender, validMembers)` of each captured premature commitment.
    premature: Vec<(usize, Vec<bool>)>,
    /// The final commitment's `validMembers`, one per member.
    final_valid: Vec<bool>,
}

impl Transcript {
    fn sent_by(list: &[(usize, Vec<u8>)], me: usize) -> Vec<&[u8]> {
        list.iter().filter(|(from, _)| *from == me).map(|(_, raw)| &raw[..]).collect()
    }

    /// The final members past the session's, which must be clear, cut off.
    fn trim_final(&mut self) {
        let past = self.final_valid.split_off(self.members.len());
        assert!(past.iter().all(|b| !b), "{}: no final bit past the members", self.label);
    }

    /// Member `me`'s verification vector, from its captured contribution; empty without one.
    fn own_vvec(&self, me: usize) -> Vec<BlsPkValue> {
        Self::sent_by(&self.contributions, me)
            .first()
            .map(|raw| Contribution::read(raw, &self.params, BLS).expect("reads").vvec().to_vec())
            .unwrap_or_default()
    }

    /// The true shares from `contributions` and `justifications` (see the module docs):
    /// `shares[c][i]` is contributor `c`'s share for `i` as `i` decrypted it, if captured.
    fn derive_own_shares(&mut self, shares: &[(usize, Vec<Option<BlsSkValue>>)]) {
        let n = self.members.len();
        let vvecs: Vec<Vec<BlsPkValue>> = (0..n).map(|c| self.own_vvec(c)).collect();
        let fits = |c: usize, i: usize, sk: &BlsSkValue| {
            let id = member_id(&self.members[i].pro_tx_hash);
            !vvecs[c].is_empty() && verify_contribution_share(&id, &vvecs[c], sk, FLAG)
        };
        let mut own = vec![vec![BlsSkValue::invalid(); n]; n];
        for (c, row) in shares {
            for (i, share) in row.iter().enumerate() {
                if let Some(share) = share.as_ref().filter(|sk| fits(*c, i, sk)) {
                    own[*c][i] = share.clone();
                }
            }
        }
        for (c, raw) in &self.justifications {
            let qj = Justification::read(raw, &self.params, BLS).expect("reads");
            for (i, sk) in qj.keys().iter().filter(|(i, sk)| fits(*c, *i as usize, sk)) {
                own[*c][*i as usize] = sk.clone();
            }
        }
        self.own_shares = own;
    }
}

fn hexes(value: &Value) -> Vec<u8> {
    unhex(value.as_str().expect("hex"))
}

fn bitstring(value: &Value) -> Vec<bool> {
    value.as_str().expect("bits").bytes().map(|b| b == b'1').collect()
}

fn members_of(
    rows: &[Value],
    member: impl Fn(&Value) -> (&Value, &Value),
) -> (Vec<TestMember>, Vec<TestOperatorKey>) {
    rows.iter()
        .map(|row| {
            let (pro_tx, operator_sk) = member(row);
            let key = TestOperatorKey::new(sk(operator_sk, FLAG));
            (TestMember::new(ProTxHash::from(arr::<32>(pro_tx)), key.public_key()), key)
        })
        .unzip()
}

/// A session of the LQ4a-2 files: `[member, raw, …]` rows.
fn trimmed(q: &Value, params: LlmqParams) -> Transcript {
    let (members, keys) = members_of(items(&q["members"]), |m| (&m[0], &m[1]));
    let rows = |k: &str| -> Vec<(usize, Vec<u8>)> {
        items(&q[k]).iter().map(|r| (index(&r[0]), hexes(&r[1]))).collect()
    };
    let shares: Vec<(usize, Vec<Option<BlsSkValue>>)> = items(&q["contributions"])
        .iter()
        .filter(|r| r.get(2).is_some())
        .map(|r| {
            let row = items(&r[2]).iter().map(|s| s.as_str().map(|_| sk(s, FLAG))).collect();
            (index(&r[0]), row)
        })
        .collect();
    let mut t = Transcript {
        label: format!("t{} {}", q["llmq_type"], &q["quorum_hash"].as_str().expect("hex")[..8]),
        params,
        quorum_hash: QuorumHash::from(arr::<32>(&q["quorum_hash"])),
        members,
        keys,
        contributions: rows("contributions"),
        complaints: rows("complaints"),
        justifications: rows("justifications"),
        own_shares: Vec::new(),
        premature: items(&q["premature"])
            .iter()
            .map(|r| (index(&r[0]), bitstring(&r[1])))
            .collect(),
        final_valid: bitstring(&q["final_valid"]),
    };
    t.derive_own_shares(&shares);
    t.trim_final();
    t
}

/// A session of BS12's `dkg.json5`.
fn bs12(q: &Value) -> Transcript {
    let (members, keys) = members_of(items(&q["members"]), |m| (&m["pro_tx"], &m["operator_sk"]));
    let rows = |k: &str| -> Vec<(usize, Vec<u8>)> {
        items(&q[k]).iter().map(|r| (index(&r["member"]), hexes(&r["raw"]))).collect()
    };
    let params = *params(LlmqType::LlmqTest);
    let shares: Vec<(usize, Vec<Option<BlsSkValue>>)> = items(&q["contributions"])
        .iter()
        .map(|c| {
            let row = items(&c["shares"])
                .iter()
                .map(|s| s["share"].as_str().map(|_| sk(&s["share"], FLAG)));
            (index(&c["member"]), row.collect())
        })
        .collect();
    let premature = items(&q["premature"])
        .iter()
        .map(|p| {
            let qc = PrematureCommitment::read(&hexes(&p["raw"]), &params, BLS).expect("reads");
            let size = u64::try_from(params.size).expect("positive");
            (index(&p["member"]), (0..size).map(|i| qc.valid_member(i)).collect())
        })
        .collect();
    let mut t = Transcript {
        label: format!("bs12 {}", &q["quorum_hash"].as_str().expect("hex")[..8]),
        params,
        quorum_hash: QuorumHash::from(arr::<32>(&q["quorum_hash"])),
        members,
        keys,
        contributions: rows("contributions"),
        complaints: rows("complaints"),
        justifications: rows("justifications"),
        own_shares: Vec::new(),
        premature,
        final_valid: items(&q["final"]["valid"]).iter().map(|v| *v == true).collect(),
    };
    t.derive_own_shares(&shares);
    t.trim_final();
    t
}

/// One member's replay and what it sent.
struct Run {
    session: DkgSession<TestMember>,
    env: TestEnv,
    complaint: Option<Complaint>,
    justification: Option<Justification>,
}

/// `msgs` into the session in pops of 8: nobody is scored, and each message is relayed.
fn feed(
    t: &Transcript,
    session: &mut DkgSession<TestMember>,
    env: &mut TestEnv,
    kind: DkgMsgKind,
    me: usize,
    msgs: &[(usize, Vec<u8>)],
) {
    for pop in msgs.chunks(8) {
        let outcome = session.process_batch(kind, &batch_of(me, pop), &mut env.env());
        let label = format!("{} member {me} {kind:?}", t.label);
        assert!(outcome.misbehaving.is_empty(), "{label}: {:?}", outcome.misbehaving);
        let relayed: Vec<DkgInv> = outcome.relay.iter().map(|r| r.inv).collect();
        let want: Vec<DkgInv> = pop
            .iter()
            .map(|(_, raw)| DkgInv {
                kind,
                hash: sha256d(raw),
            })
            .collect();
        assert_eq!(relayed, want, "{label}: each message relayed once, as its own kind");
    }
}

/// Member `me`'s run under the simulated errors `sim`, answering its draws with `draws`.
fn replay(
    t: &Transcript,
    me: usize,
    conn: &ConnSnapshot,
    sim: &[(SimError, i32)],
    draws: Vec<Draw>,
) -> Run {
    let init = SessionInit {
        params: &t.params,
        base: QuorumBase {
            hash: t.quorum_hash,
            height: 0,
        },
        quorum_index: 0,
        members: Arc::from(t.members.clone()),
        active_pro_tx: Some(t.members[me].pro_tx_hash),
        rotation_enabled: false,
        v19_after_base: true,
    };
    let topo = &mut CoreRelayTopology {
        members: t.members.iter().map(|m| m.pro_tx_hash).collect(),
        calls: 0,
    };
    let mut env = TestEnv::new(&t.keys[me], 0);
    let mut session = DkgSession::init(init, topo, &mut env.debug).expect("enough members");
    let replay = Rc::new(RefCell::new(ReplayRng::new(draws)));
    env.rng = Box::new(Shared(replay.clone()));
    for (kind, rate) in sim {
        env.sim.set(*kind, *rate).expect("a rate");
    }

    session.adopt_contribution(t.own_vvec(me), t.own_shares[me].clone()).expect("first stage");
    feed(t, &mut session, &mut env, DkgMsgKind::Contribution, me, &t.contributions);
    let complaint = session.verify_and_complain(conn, &mut env.env()).expect("phase order");
    feed(t, &mut session, &mut env, DkgMsgKind::Complaint, me, &t.complaints);
    let justification = session.verify_and_justify(&mut env.env()).expect("phase order");
    feed(t, &mut session, &mut env, DkgMsgKind::Justification, me, &t.justifications);
    session.mark_open_complaints(&mut env.debug);

    env.rng = Box::new(CoreRng(SeededRng::new(0)));
    let label = format!("{} member {me}", t.label);
    Rc::try_unwrap(replay)
        .unwrap_or_else(|_| panic!("one owner"))
        .into_inner()
        .finish()
        .unwrap_or_else(|e| panic!("{label}: draws {e:?}"));
    Run {
        session,
        env,
        complaint,
        justification,
    }
}

/// What the checks of one transcript's runs covered.
#[derive(Debug, Default, PartialEq, Eq)]
struct Checked {
    runs: usize,
    complaints: usize,
    /// Complaints Core logged sending that the transcript lacks: each is checked against the
    /// one derived from what Core logged (see [`check`]).
    uncaptured: usize,
    justifications: usize,
    premature: usize,
}

/// A complaint's `badCount` and `complaintCount` (`a/dkgsession.cpp:307-319`).
fn counts(qc: &Complaint, n: usize) -> [usize; 2] {
    [(0..n).filter(|&i| qc.bad_member(i)).count(), (0..n).filter(|&i| qc.complain_for(i)).count()]
}

/// What a member's node logged around its complaint, where the capture has its log.
struct Logged {
    /// The counts of the complaint it logged sending; `None` without one.
    sent: Option<[usize; 2]>,
    /// `badConnection` per member index, from its fold.
    bad_connection: Vec<(usize, bool)>,
}

/// The complaint member `me` sent that the capture lacks, rebuilt from what Core logged
/// (`a/dkgsession.cpp:292-332`): a vote for each member that sent no captured contribution or
/// that its fold logged `badConnection` for, and no complaint, as its `complaintCount` of 0
/// says. A complaint with complaint bits would need the shares Core decrypted, which the
/// capture lacks.
fn uncaptured_complaint(t: &Transcript, me: usize, logged: &Logged) -> (Vec<bool>, Complaint) {
    let label = format!("{} member {me}", t.label);
    let [bad_count, complaint_count] = logged.sent.expect("a logged complaint");
    assert_eq!(complaint_count, 0, "{label}: complaint bits are not derivable");
    let size = usize::try_from(t.params.size).expect("positive");
    let bad: Vec<bool> = (0..size)
        .map(|i| {
            i < t.members.len()
                && (Transcript::sent_by(&t.contributions, i).is_empty()
                    || logged.bad_connection.contains(&(i, true)))
        })
        .collect();
    assert_eq!(bad.iter().filter(|b| **b).count(), bad_count, "{label}: the logged badCount");
    let qc = Complaint::unsigned(
        t.params.llmq_type,
        &t.quorum_hash,
        &t.members[me].pro_tx_hash,
        &bad,
        &vec![false; size],
        FLAG,
    );
    let sig = t.keys[me].sign(qc.sign_hash(), false);
    (bad, qc.with_sig(sig, FLAG))
}

/// Member `me`'s run sent what Core sent, and ends with its premature commitment's members.
/// A complaint Core logged sending that the capture lacks must be the one derived from the log
/// ([`uncaptured_complaint`]).
fn check(t: &Transcript, me: usize, run: &Run, logged: Option<&Logged>, checked: &mut Checked) {
    let label = format!("{} member {me}", t.label);
    let n = t.members.len();
    if let Some(logged) = logged {
        assert_eq!(
            run.complaint.as_ref().map(|qc| counts(qc, n)),
            logged.sent,
            "{label}: logged counts"
        );
    }
    let sent = run.complaint.as_ref().map(|qc| qc.payload(FLAG));
    let want = Transcript::sent_by(&t.complaints, me);
    if let Some(logged) = logged.filter(|l| want.is_empty() && l.sent.is_some()) {
        let (bad, derived) = uncaptured_complaint(t, me, logged);
        let qc = run.complaint.as_ref().expect("a complaint");
        for (i, vote) in bad.iter().enumerate() {
            assert_eq!(qc.bad_member(i), *vote, "{label}: the vote on {i}");
            assert!(!qc.complain_for(i), "{label}: no complaint about {i}");
        }
        assert_eq!(sent, Some(derived.payload(FLAG)), "{label}: the derived complaint");
        checked.uncaptured += 1;
    } else {
        assert_eq!(sent.as_deref().into_iter().collect::<Vec<_>>(), want, "{label}: complaint");
    }
    checked.complaints += want.len();
    let sent = run.justification.as_ref().map(|qj| qj.payload(FLAG));
    let want = Transcript::sent_by(&t.justifications, me);
    assert_eq!(sent.as_deref().into_iter().collect::<Vec<_>>(), want, "{label}: justification");
    checked.justifications += want.len();
    let valid = run.session.valid_members();
    for (_, bits) in t.premature.iter().filter(|(from, _)| *from == me) {
        let (head, tail) = bits.split_at(valid.len());
        assert_eq!(head, valid, "{label}: validMembers");
        assert!(tail.iter().all(|b| !b), "{label}: no bit past the members");
        checked.premature += 1;
    }
    checked.runs += 1;
}

fn test_params(file: &Value) -> LlmqParams {
    let p = &file["llmq_test"];
    let int = |k: &str| i32::try_from(p[k].as_i64().expect("an int")).expect("small");
    LlmqParams {
        size: int("size"),
        min_size: int("min_size"),
        threshold: int("threshold"),
        dkg_bad_votes_threshold: int("bad_votes"),
        ..*params(LlmqType::LlmqTest)
    }
}

/// The draws of a `complain-lie` member: one per contribution from another member, each of
/// which decrypts in these captures.
fn complain_lie(t: &Transcript, me: usize) -> (Vec<(SimError, i32)>, Vec<Draw>) {
    let others = t.contributions.iter().filter(|(from, _)| *from != me).count();
    (vec![(SimError::ComplainLie, 100)], vec![Draw::Bool(SimError::ComplainLie, 100, true); others])
}

/// Every member of the 12/7 `scenario`, each erring as `erring(t, me, mn)` gives its simulated
/// error rates and draws (`mn`: its masternode number). A phase-2 error has no rate here: the
/// transcript holds its effect, and phases 3 and 4 never draw for it. The functional test
/// regtest runs without PoSe, so the connection snapshot is empty. Each run must end with the
/// final commitment's members.
fn replay_12_7(
    scenario: &str,
    erring: impl Fn(&Transcript, usize, usize) -> (Vec<(SimError, i32)>, Vec<Draw>),
) -> (Transcript, Vec<usize>, Vec<Run>, Checked) {
    let file = corpus(DKG_12_7);
    assert_eq!(file["scheme"], "basic");
    let q = items(&file["sessions"])
        .iter()
        .find(|q| q["scenario"] == scenario)
        .unwrap_or_else(|| panic!("{scenario}"));
    let mn: Vec<usize> = items(&q["mn"]).iter().map(index).collect();
    let t = trimmed(q, test_params(&file));
    let mut checked = Checked::default();
    let runs: Vec<Run> = (0..t.members.len())
        .map(|me| {
            let (sim, draws) = erring(&t, me, mn[me]);
            let run = replay(&t, me, &ConnSnapshot::default(), &sim, draws);
            check(&t, me, &run, None, &mut checked);
            assert_eq!(
                run.session.valid_members(),
                t.final_valid,
                "member {me}: the final members"
            );
            run
        })
        .collect();
    (t, mn, runs, checked)
}

fn valid_count(run: &Run) -> usize {
    run.session.valid_members().iter().filter(|v| **v).count()
}

#[test]
fn t3c_12_7_honest_every_member_is_valid_without_a_complaint() {
    let (t, _, runs, checked) = replay_12_7("honest", |_, _, _| Default::default());
    assert_eq!(
        checked,
        Checked {
            runs: 12,
            complaints: 0,
            uncaptured: 0,
            justifications: 0,
            premature: 12,
        }
    );
    for (me, run) in runs.iter().enumerate() {
        assert!(run.complaint.is_none() && run.justification.is_none(), "member {me}");
        assert_eq!(valid_count(run), 12, "member {me}");
        assert_eq!(run.env.store.writers("S").len(), 12, "member {me}: every share verified");
    }
    assert_eq!(t.contributions.len(), 12);
}

#[test]
fn t3c_12_7_omit5_leaves_exactly_min_size_valid() {
    // mn7..mn11 omit their contributions: every member votes them bad, and with 12 votes of 7
    // needed each is bad everywhere.
    let (t, mn, runs, checked) = replay_12_7("omit5", |_, _, _| Default::default());
    assert_eq!(
        checked,
        Checked {
            runs: 12,
            complaints: 12,
            uncaptured: 0,
            justifications: 0,
            premature: 12,
        }
    );
    let omitters: Vec<bool> = mn.iter().map(|m| (7..12).contains(m)).collect();
    for (me, run) in runs.iter().enumerate() {
        assert_eq!(valid_count(run), usize::try_from(t.params.min_size).unwrap(), "member {me}");
        let bad: Vec<bool> = (0..12).map(|i| run.session.is_bad(i)).collect();
        assert_eq!(bad, omitters, "member {me}");
        let votes: Vec<usize> = (0..12).map(|i| run.session.bad_member_votes(i).len()).collect();
        let want: Vec<usize> = omitters
            .iter()
            .map(|&o| {
                if o {
                    12
                } else {
                    0
                }
            })
            .collect();
        assert_eq!(votes, want, "member {me}");
    }
}

#[test]
fn t3c_12_7_mixed_leaves_eleven_valid_and_the_silent_liar_invalid() {
    // mn0 lies in its contribution and justifies; mn1 lies and omits its justification; mn2
    // complains about everyone. mn1's complaints stay open, so it alone is invalid.
    let (t, mn, runs, checked) = replay_12_7("mixed", |t, me, mn| match mn {
        1 => {
            (vec![(SimError::JustifyOmit, 100)], vec![Draw::Bool(SimError::JustifyOmit, 100, true)])
        }
        2 => complain_lie(t, me),
        _ => Default::default(),
    });
    assert_eq!(
        checked,
        Checked {
            runs: 12,
            complaints: 12,
            uncaptured: 0,
            justifications: 10,
            premature: 12,
        }
    );
    let member = |n: usize| mn.iter().position(|&m| m == n).expect("a member");
    let (liar, silent, complainer) = (member(0), member(1), member(2));
    for (me, run) in runs.iter().enumerate() {
        let s = &run.session;
        assert_eq!(valid_count(run), 11, "member {me}");
        assert!(s.is_bad(silent), "member {me}");
        assert!(!s.complaints_from_others(silent).is_empty(), "member {me}: open complaints");
        assert!(s.complaints_from_others(liar).is_empty(), "member {me}: mn0 answered");
        assert!(s.complaints_from_others(complainer).is_empty(), "member {me}: mn2 answered");
        if me != liar {
            // mn0's justification is our verified share from it, so we no longer complain;
            // mn1's lie never becomes one.
            assert!(!s.we_complain(liar), "member {me}");
            let verified = run.env.store.writers("S");
            let count =
                |who: usize| verified.iter().filter(|p| **p == t.members[who].pro_tx_hash).count();
            let own = usize::from(me == silent);
            assert_eq!((count(liar), count(silent)), (1, own), "member {me}: qdkg_S rows");
        }
    }
}

/// A fold call's snapshot, and the logged `badConnection` per member index.
fn snapshot(fold: &Value, t: &Transcript) -> (ConnSnapshot, Vec<(usize, bool)>) {
    let mut conn = ConnSnapshot {
        pose_enabled: fold["pose"] == true,
        all_members_connected: fold["all_connected"] == true,
        ..ConnSnapshot::default()
    };
    for p in items(&fold["peers"]) {
        conn.nodes.push(ConnFacts {
            verified_pro_tx: ProTxHash::from(arr::<32>(&p[0])),
            version: i32::try_from(p[1].as_i64().expect("a version")).expect("small"),
            masternode_connection: p[2] == true,
            relay_txs: p[3] == true,
            services: p[4].as_u64().expect("services"),
        });
    }
    let mut logged = Vec::new();
    for m in items(&fold["members"]) {
        let pro_tx = ProTxHash::from(arr::<32>(&m[0]));
        if m[1] == true {
            conn.outbound_failed_too_many.insert(pro_tx);
        }
        if m[2] == true {
            conn.platform_banned.insert(pro_tx);
        }
        let idx = t.members.iter().position(|x| x.pro_tx_hash == pro_tx).expect("a member");
        logged.push((idx, m[3] == true));
    }
    (conn, logged)
}

/// Every member of `section`'s sessions that logged a fold, replayed over its fold.
fn replay_simplepose(section: &str) -> (Checked, usize) {
    let file = corpus(SIMPLEPOSE);
    assert_eq!(file["scheme"], "basic");
    let mut checked = Checked::default();
    let mut bad_connections = 0;
    for q in items(&file[section]) {
        let t = match q["llmq_type"].as_u64() {
            Some(100) => trimmed(q, test_params(&file)),
            Some(102) => trimmed(q, *params(LlmqType::LlmqTestV17)),
            Some(103) => trimmed(q, *params(LlmqType::LlmqTestDip0024)),
            other => panic!("type {other:?}"),
        };
        let with_fold: BTreeSet<usize> =
            items(&q["folds"]).iter().map(|f| index(&f["me"])).collect();
        for (from, _) in &t.complaints {
            assert!(with_fold.contains(from), "{}: every complainer logged its fold", t.label);
        }
        for fold in items(&q["folds"]) {
            let me = index(&fold["me"]);
            let (conn, bad_connection) = snapshot(fold, &t);
            let run = replay(&t, me, &conn, &[], Vec::new());
            for &(idx, bad_connection) in &bad_connection {
                assert_eq!(
                    run.session.is_bad_connection(idx),
                    bad_connection,
                    "{} member {me} on {idx}",
                    t.label
                );
                bad_connections += usize::from(bad_connection);
            }
            let logged = Logged {
                sent: fold["sent"].as_array().map(|c| [index(&c[0]), index(&c[1])]),
                bad_connection,
            };
            check(&t, me, &run, Some(&logged), &mut checked);
        }
    }
    (checked, bad_connections)
}

#[test]
fn t3c_simplepose_spork23_complaints_follow_the_logged_folds() {
    let (checked, bad_connections) = replay_simplepose("spork23");
    assert_eq!(
        checked,
        Checked {
            runs: 32,
            complaints: 18,
            uncaptured: 1,
            justifications: 0,
            premature: 31,
        }
    );
    assert_eq!(bad_connections, 16);
}

#[test]
fn t3c_simplepose_nospork23_has_no_complaint_with_pose_off() {
    let (checked, bad_connections) = replay_simplepose("nospork23");
    assert_eq!(
        checked,
        Checked {
            runs: 17,
            complaints: 0,
            uncaptured: 0,
            justifications: 0,
            premature: 17,
        }
    );
    assert_eq!(bad_connections, 0);
}

/// The rates and draws of BS12's erring member in phases 3 and 4: `justify` is the
/// justification error its session's `contribution-lie` came with, `liar_says` its captured
/// justification. A phase-2 error draws nothing here (see [`replay_12_7`]).
fn erring_bs12(
    t: &Transcript,
    me: usize,
    scenario: Scenario,
    justify: Option<SimError>,
    liar_says: Option<&Justification>,
) -> (Vec<(SimError, i32)>, Vec<Draw>) {
    match (scenario, justify, liar_says) {
        (Scenario::ComplainLie, _, _) => complain_lie(t, me),
        (Scenario::ContributionLie, Some(SimError::JustifyOmit), _) => {
            (vec![(SimError::JustifyOmit, 100)], vec![Draw::Bool(SimError::JustifyOmit, 100, true)])
        }
        (Scenario::ContributionLie, Some(SimError::JustifyLie), Some(qj)) => {
            let draws = qj
                .keys()
                .iter()
                .filter(|(i, _)| *i as usize != me)
                .flat_map(|(i, key)| {
                    [
                        Draw::Bool(SimError::JustifyLie, 100, true),
                        Draw::Key(KeyDraw::JustifyLie(*i), key.clone()),
                    ]
                })
                .collect();
            (vec![(SimError::JustifyLie, 100)], draws)
        }
        _ => Default::default(),
    }
}

#[test]
fn t3c_bs12_every_member_complains_justifies_and_marks_as_core_did() {
    // The functional test's rates carry over (rc.4 `feature_llmq_dkgerrors.py`:37-90): the
    // justification sessions keep `contribution-lie` on, and add `justify-omit` or
    // `justify-lie`; the liar's justification tells which ran.
    let corpus = corpus(BS12_DKG);
    let quorums = items(&corpus["quorums"]);
    let mut checked = Checked::default();
    let mut seen = BTreeSet::new();
    for q in quorums {
        let (scenario, erring) = classify(q);
        let t = bs12(q);
        let liar_says = erring.and_then(|e| {
            Transcript::sent_by(&t.justifications, e)
                .first()
                .map(|raw| Justification::read(raw, &t.params, BLS).expect("reads"))
        });
        let fits = |qj: &Justification| {
            let vvec = t.own_vvec(erring.expect("a liar"));
            qj.keys().iter().all(|(i, sk)| {
                verify_contribution_share(
                    &member_id(&t.members[*i as usize].pro_tx_hash),
                    &vvec,
                    sk,
                    FLAG,
                )
            })
        };
        let justify = match (scenario, &liar_says) {
            (Scenario::ContributionLie, None) => Some(SimError::JustifyOmit),
            (Scenario::ContributionLie, Some(qj)) if !fits(qj) => Some(SimError::JustifyLie),
            _ => None,
        };
        seen.insert((scenario, justify));
        for me in 0..t.members.len() {
            let (sim, draws) = match erring {
                Some(e) if e == me => erring_bs12(&t, me, scenario, justify, liar_says.as_ref()),
                _ => Default::default(),
            };
            let run = replay(&t, me, &ConnSnapshot::default(), &sim, draws);
            check(&t, me, &run, None, &mut checked);
            assert_eq!(run.session.valid_members(), t.final_valid, "{} member {me}", t.label);
        }
    }
    let total = |k: &str| quorums.iter().map(|q| items(&q[k]).len()).sum::<usize>();
    assert_eq!(
        checked,
        Checked {
            runs: 24,
            complaints: total("complaints"),
            uncaptured: 0,
            justifications: total("justifications"),
            premature: total("premature"),
        }
    );
    use Scenario::*;
    let want: BTreeSet<(Scenario, Option<SimError>)> = [
        (Honest, None),
        (Omit, None),
        (ContributionLie, None),
        (ContributionLie, Some(SimError::JustifyOmit)),
        (ContributionLie, Some(SimError::JustifyLie)),
        (ComplainLie, None),
    ]
    .into();
    assert_eq!(seen, want, "every scenario of the functional test is replayed");
}
