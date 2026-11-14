//! T3-C through phase 5 (LQ4a-2, LQ4b-1, LQ4b-2a): each member of a captured session replayed
//! from the transcript, contributions to the premature commitment, against what Core sent.
//!
//! A member's run adopts its own captured contribution, receives every captured contribution,
//! complaint and justification in capture order in pops of 8, and complains, justifies and
//! commits between them. Its complaint and justification must be the bytes Core sent, and its
//! `validMembers` those of its captured premature commitment; where the transcript holds the
//! premature commitments' bytes, its premature commitment must be the one Core sent too.
//!
//! Its own true shares are those of its polynomial the capture shows that fit its verification
//! vector at the recipient's id: the shares its contribution's recipients decrypted, and the
//! keys its justification reveals. A share that does not fit stays unknown (invalid), so a
//! justification lie replays only from its recorded draw.

use dash_params::LlmqParams;
use dash_types::Numeric;

use super::finalize::{DKGERRORS, ROTATION, TAGS, effective};
use super::*;
use crate::dkg::bls::member_id;
use crate::dkg::conn::{ConnFacts, ConnSnapshot};
use crate::dkg::reference::{ReferenceVerifier, StoreRow};
use crate::dkg::rng::{CoreRng, SeededRng};
use crate::dkg::session::fixture::{BLS, batch_of, lie_draws, lie_loopback_line, lines};
use crate::dkg::wire::{Complaint, Justification, PrematureCommitment, final_commitment_bytes};

const DKG_12_7: (&str, &str) =
    ("dkg_12_7.json5", "4832e68ebf8b7a985cf12b68f821055a9ff2548b87689791a6b04a4fa6da9b08");
const SIMPLEPOSE: (&str, &str) =
    ("dkg_simplepose.json5", "d2277fbb0a0fd3295ffda7f426770f6c5438eb876806d4d9338a3fdc733205bc");

const FLAG: BlsSchemeKind = BlsSchemeKind::Basic;

/// One captured session, read.
struct Transcript {
    label: String,
    llmq_type: LlmqType,
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
    /// `(sender, raw)` of each captured premature commitment, in capture order, where the
    /// transcript holds their bytes.
    premature_raw: Vec<(usize, Vec<u8>)>,
    /// The final commitment's `validMembers`, one per member.
    final_valid: Vec<bool>,
    /// The final commitment's bytes, where the transcript holds them.
    final_raw: Vec<u8>,
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

/// The `[member, raw, …]` rows of `q[k]`, as `(member, raw)`.
fn raw_rows(q: &Value, k: &str) -> Vec<(usize, Vec<u8>)> {
    items(&q[k]).iter().map(|r| (index(&r[0]), hexes(&r[1]))).collect()
}

/// A session of the LQ4a-2 files: `[member, raw, …]` rows.
fn trimmed(q: &Value, llmq_type: LlmqType, params: LlmqParams) -> Transcript {
    let (members, keys) = members_of(items(&q["members"]), |m| (&m[0], &m[1]));
    let rows = |k: &str| raw_rows(q, k);
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
        llmq_type,
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
        premature_raw: Vec::new(),
        final_valid: bitstring(&q["final_valid"]),
        final_raw: Vec::new(),
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
    let params = params(LlmqType::LlmqTest);
    let shares: Vec<(usize, Vec<Option<BlsSkValue>>)> = items(&q["contributions"])
        .iter()
        .map(|c| {
            let row = items(&c["shares"])
                .iter()
                .map(|s| s["share"].as_str().map(|_| sk(&s["share"], FLAG)));
            (index(&c["member"]), row.collect())
        })
        .collect();
    let premature_raw: Vec<(usize, Vec<u8>)> =
        items(&q["premature"]).iter().map(|p| (index(&p["member"]), hexes(&p["raw"]))).collect();
    let mut t = Transcript {
        label: format!("bs12 {}", &q["quorum_hash"].as_str().expect("hex")[..8]),
        llmq_type: LlmqType::LlmqTest,
        params,
        quorum_hash: QuorumHash::from(arr::<32>(&q["quorum_hash"])),
        members,
        keys,
        contributions: rows("contributions"),
        complaints: rows("complaints"),
        justifications: rows("justifications"),
        own_shares: Vec::new(),
        premature: premature_bits(&premature_raw, &params),
        premature_raw,
        final_valid: items(&q["final"]["valid"]).iter().map(|v| *v == true).collect(),
        final_raw: hexes(&q["final"]["raw"]),
    };
    t.derive_own_shares(&shares);
    t.trim_final();
    t
}

/// Each premature commitment's sender and `validMembers`, a bit per `size`.
fn premature_bits(raws: &[(usize, Vec<u8>)], params: &LlmqParams) -> Vec<(usize, Vec<bool>)> {
    raws.iter()
        .map(|(from, raw)| {
            let qc = PrematureCommitment::read(raw, params, BLS).expect("reads");
            (*from, (0..u64::from(params.size)).map(|i| qc.valid_member(i)).collect())
        })
        .collect()
}

/// A final commitment's `validMembers` (`l/commitment.h:95-115`): the second `DYNBITSET`, after
/// the version, type, quorum hash, the indexed versions' `quorumIndex` and the signers.
fn final_valid_members(raw: &[u8]) -> Vec<bool> {
    let version = u16::from_le_bytes([raw[0], raw[1]]);
    let mut at = if matches!(version, 2 | 4) {
        37
    } else {
        35
    };
    let mut bitset = || {
        let bits = usize::from(raw[at]);
        assert!(bits < 0xfd, "a one-byte CompactSize");
        let bytes = &raw[at + 1..at + 1 + bits.div_ceil(8)];
        at += 1 + bytes.len();
        (0..bits).map(|i| bytes[i / 8] >> (i % 8) & 1 == 1).collect::<Vec<bool>>()
    };
    let _signers = bitset();
    bitset()
}

/// A session of a C0 file (`[member, raw]` rows) under `params`. The true shares are those each
/// recipient decrypts with its operator key, as Core's recipient does.
fn c0(q: &Value, params: LlmqParams, label: String) -> Transcript {
    let (members, keys) = members_of(items(&q["members"]), |m| (&m[0], &m[1]));
    let rows = |k: &str| raw_rows(q, k);
    let contributions = rows("contributions");
    let shares: Vec<(usize, Vec<Option<BlsSkValue>>)> = contributions
        .iter()
        .map(|(from, raw)| {
            let qc = Contribution::read(raw, &params, BLS).expect("reads");
            let row =
                keys.iter().enumerate().map(|(i, key)| key.decrypt_share(qc.blobs(), i, FLAG));
            (*from, row.collect())
        })
        .collect();
    let premature_raw = rows("premature");
    let final_raw = hexes(&q["final"]);
    let mut t = Transcript {
        label,
        llmq_type: LlmqType::from_base(u8::try_from(index(&q["llmq_type"])).expect("a type")),
        params,
        quorum_hash: QuorumHash::from(arr::<32>(&q["quorum_hash"])),
        members,
        keys,
        contributions,
        complaints: rows("complaints"),
        justifications: rows("justifications"),
        own_shares: Vec::new(),
        premature: premature_bits(&premature_raw, &params),
        premature_raw,
        final_valid: final_valid_members(&final_raw),
        final_raw,
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
    premature: Option<PrematureCommitment>,
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

/// Member `me`'s session for `t`, before phase 2, and its environment.
fn member_of(t: &Transcript, me: usize) -> (DkgSession<TestMember>, TestEnv) {
    let init = SessionInit {
        llmq_type: t.llmq_type,
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
    let session = DkgSession::init(init, topo, &mut env.debug).expect("enough members");
    (session, env)
}

/// Member `me`'s run under the simulated errors `sim`, answering its draws with `draws`.
fn replay(
    t: &Transcript,
    me: usize,
    conn: &ConnSnapshot,
    sim: &[(SimError, i32)],
    draws: Vec<Draw>,
) -> Run {
    let (mut session, mut env) = member_of(t, me);
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
    env.store.members = t.members.iter().map(|m| m.pro_tx_hash).collect();
    let premature = session.verify_and_commit(&mut env.env());

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
        premature,
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
    let size = usize::from(t.params.size);
    let bad: Vec<bool> = (0..size)
        .map(|i| {
            i < t.members.len()
                && (Transcript::sent_by(&t.contributions, i).is_empty()
                    || logged.bad_connection.contains(&(i, true)))
        })
        .collect();
    assert_eq!(bad.iter().filter(|b| **b).count(), bad_count, "{label}: the logged badCount");
    let qc = Complaint::unsigned(
        t.llmq_type,
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
    let int = |k: &str| u16::try_from(p[k].as_i64().expect("an int")).expect("small");
    LlmqParams {
        size: int("size"),
        min_size: int("min_size"),
        threshold: int("threshold"),
        dkg_bad_votes_threshold: int("bad_votes"),
        ..params(LlmqType::LlmqTest)
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
    let t = trimmed(q, LlmqType::LlmqTest, test_params(&file));
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
        assert_eq!(valid_count(run), usize::from(t.params.min_size), "member {me}");
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
            Some(100) => trimmed(q, LlmqType::LlmqTest, test_params(&file)),
            Some(102) => trimmed(q, LlmqType::LlmqTestV17, params(LlmqType::LlmqTestV17)),
            Some(103) => trimmed(q, LlmqType::LlmqTestDip0024, params(LlmqType::LlmqTestDip0024)),
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

/// The justification error a BS12 `contribution-lie` session came with, which the liar's
/// justification tells, and that justification.
fn bs12_justify(
    t: &Transcript,
    scenario: Scenario,
    erring: Option<usize>,
) -> (Option<SimError>, Option<Justification>) {
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
    (justify, liar_says)
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
        let (justify, liar_says) = bs12_justify(&t, scenario, erring);
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

// Phase 5: the premature commitments (LQ4b-1 C4).

/// How the erring member of a commit-omit or commit-lie session ran phase 5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Commit {
    /// `commit-omit` at rate 100.
    Omit,
    /// `commit-lie` at rate 100, its lie type drawn as this one.
    Lie(u64),
}

impl Commit {
    /// Every way a session whose erring member sent no captured premature commitment can have
    /// run: omitting, or each lie type.
    const ALL: [Self; 6] =
        [Self::Omit, Self::Lie(0), Self::Lie(1), Self::Lie(2), Self::Lie(3), Self::Lie(4)];

    /// The rates and draws of phase 5 (LQ4 §2.9).
    fn rates_and_draws(self) -> (Vec<(SimError, i32)>, Vec<Draw>) {
        match self {
            Self::Omit => (
                vec![(SimError::CommitOmit, 100)],
                vec![Draw::Bool(SimError::CommitOmit, 100, true)],
            ),
            Self::Lie(k) => {
                let key = crate::dkg::bls::make_new_key(&mut SeededRng::new(0x11e));
                (vec![(SimError::CommitLie, 100)], lie_draws(k, key))
            }
        }
    }
}

/// `msgs` into the run's session as one batch of premature commitments.
fn feed_premature(run: &mut Run, me: usize, msgs: &[(usize, Vec<u8>)]) -> BatchOutcome<usize> {
    run.session.process_batch(
        DkgMsgKind::PrematureCommitment,
        &batch_of(me, msgs),
        &mut run.env.env(),
    )
}

/// The run's final commitments over what its session accepted, as bytes, with the reference
/// verifier's members reads.
fn finals(t: &Transcript, run: &mut Run) -> (Vec<Vec<u8>>, Vec<usize>) {
    let height = u32::from(t.params.dkg_interval) * 10;
    let mut verifier = ReferenceVerifier::new(t.llmq_type, &t.params, height, &t.members);
    verifier.v19_after_base = true;
    let finals = run.session.finalize_commitments(&mut DkgEnv {
        verify: &mut verifier,
        ..run.env.env()
    });
    (finals.iter().map(final_commitment_bytes).collect(), verifier.member_reads())
}

/// Member `me` sent the premature commitment Core sent, byte for byte, and its loopback
/// relays it, once in the batch of every captured one. The number checked.
fn check_sent(t: &Transcript, me: usize, run: &mut Run) -> usize {
    let label = format!("{} member {me}", t.label);
    let want = Transcript::sent_by(&t.premature_raw, me);
    let sent = run.premature.as_ref().map(|qc| qc.payload(FLAG));
    assert_eq!(sent.as_deref().into_iter().collect::<Vec<_>>(), want, "{label}: qpcommit");
    let out = feed_premature(run, me, &t.premature_raw);
    assert!(out.misbehaving.is_empty(), "{label}");
    let relayed: Vec<[u8; 32]> = out.relay.iter().map(|r| r.inv.hash).collect();
    let all: Vec<[u8; 32]> = t.premature_raw.iter().map(|(_, raw)| sha256d(raw)).collect();
    assert_eq!(relayed, all, "{label}: every captured qpcommit relayed, ours included");
    want.len()
}

/// The erring member `me` of a session without its premature commitment, run as `how`: it
/// relays none of its own (an omitted one is not sent; a lied one is rejected at its own
/// loopback with no score and the line of LQ4 §4.4), accepts only the captured ones, and
/// finalizes the captured final.
fn check_withheld(t: &Transcript, me: usize, run: &mut Run, how: Commit) {
    let label = format!("{} member {me} {how:?}", t.label);
    assert!(Transcript::sent_by(&t.premature_raw, me).is_empty(), "{label}");
    let mut batch: Vec<(usize, Vec<u8>)> = Vec::new();
    match (how, &run.premature) {
        (Commit::Omit, None) => {
            let events = lines(&run.session.take_events());
            assert!(events.contains(&"SendCommitment -- omitting".to_owned()), "{label}");
        }
        (Commit::Lie(k), Some(qc)) => {
            let events = lines(&run.session.take_events());
            let lying = format!("SendCommitment -- lying on commitment. lieType={k}");
            assert!(events.contains(&lying), "{label}");
            batch.push((me, qc.payload(FLAG)));
        }
        (_, sent) => panic!("{label}: sent {sent:?}"),
    }
    batch.extend(t.premature_raw.iter().cloned());
    let out = feed_premature(run, me, &batch);
    assert!(out.misbehaving.is_empty(), "{label}: the own score is dropped");
    let relayed: Vec<[u8; 32]> = out.relay.iter().map(|r| r.inv.hash).collect();
    let captured: Vec<[u8; 32]> = t.premature_raw.iter().map(|(_, raw)| sha256d(raw)).collect();
    assert_eq!(relayed, captured, "{label}: only the captured ones relayed");
    if let Commit::Lie(k) = how {
        let got = lines(&out.events);
        assert!(got.contains(&lie_loopback_line(k).to_owned()), "{label}: {got:?}");
        let own = run.premature.as_ref().expect("sent").session_hash();
        assert!(run.session.get_premature_commitment(own).is_none(), "{label}: never served");
    }
    let mut valid: Vec<[u8; 32]> = run.session.valid_commitments.iter().copied().collect();
    let mut want = captured;
    valid.sort_unstable();
    want.sort_unstable();
    assert_eq!(valid, want, "{label}: validCommitments");
    let (finals, reads) = finals(t, run);
    assert_eq!(finals, std::slice::from_ref(&t.final_raw), "{label}: the captured qfcommit");
    assert_eq!(reads, [2], "{label}");
}

/// What the phase-5 checks of a corpus covered.
#[derive(Debug, Default, PartialEq, Eq)]
struct Committed {
    /// Members' runs whose qpcommit was checked byte for byte.
    sent: usize,
    /// Erring members' runs without a captured qpcommit, one per way checked.
    withheld: usize,
}

#[test]
fn t3c_bs12_every_member_sends_the_captured_premature_commitment() {
    // The rc.3 commit-omit and commit-lie sessions both carry two premature commitments and no
    // label, so each must pass under omit and under every lie type (DEC-619).
    let corpus = corpus(BS12_DKG);
    let mut committed = Committed::default();
    let mut ambiguous = 0;
    for q in items(&corpus["quorums"]) {
        let (scenario, erring) = classify(q);
        let t = bs12(q);
        let (justify, liar_says) = bs12_justify(&t, scenario, erring);
        let withheld: Vec<usize> = (0..t.members.len())
            .filter(|&m| Transcript::sent_by(&t.premature_raw, m).is_empty())
            .collect();
        if !withheld.is_empty() {
            assert_eq!((scenario, withheld.len()), (Scenario::Honest, 1), "{}", t.label);
            ambiguous += 1;
        }
        for me in 0..t.members.len() {
            let (sim, draws) = match erring {
                Some(e) if e == me => erring_bs12(&t, me, scenario, justify, liar_says.as_ref()),
                _ => Default::default(),
            };
            if withheld.contains(&me) {
                for how in Commit::ALL {
                    let (rates, commit_draws) = how.rates_and_draws();
                    let mut run = replay(&t, me, &ConnSnapshot::default(), &rates, commit_draws);
                    check_withheld(&t, me, &mut run, how);
                    committed.withheld += 1;
                }
            } else {
                let mut run = replay(&t, me, &ConnSnapshot::default(), &sim, draws);
                committed.sent += check_sent(&t, me, &mut run);
            }
        }
    }
    assert_eq!(ambiguous, 2);
    let captured: usize =
        items(&corpus["quorums"]).iter().map(|q| items(&q["premature"]).len()).sum();
    assert_eq!(
        committed,
        Committed {
            sent: captured,
            withheld: 12,
        }
    );
}

/// The member a C0 `dkgerrors` session's messages show erring under its `label`: the one
/// without a contribution (`contribution-omit`), the one every complaint is about
/// (`contribution-lie-*`), the only complainer (`complain-lie`), or the one without a premature
/// commitment (`commit-*`).
fn shown_erring(t: &Transcript, label: &str) -> Option<usize> {
    let n = t.members.len();
    let without = |list: &[(usize, Vec<u8>)]| -> Vec<usize> {
        (0..n).filter(|&m| Transcript::sent_by(list, m).is_empty()).collect()
    };
    let one = |v: Vec<usize>| -> usize {
        let [m] = v[..] else {
            panic!("{} {label}: {v:?}", t.label)
        };
        m
    };
    let complaints: Vec<Complaint> = t
        .complaints
        .iter()
        .map(|(_, raw)| Complaint::read(raw, &t.params, BLS).expect("reads"))
        .collect();
    match label {
        "honest" => {
            assert!(without(&t.premature_raw).is_empty() && complaints.is_empty(), "{}", t.label);
            None
        }
        "contribution-omit" => Some(one(without(&t.contributions))),
        "contribution-lie-justified"
        | "contribution-lie-justify-omit"
        | "contribution-lie-justify-lie" => {
            let about: BTreeSet<usize> =
                complaints.iter().flat_map(|qc| (0..n).filter(|&i| qc.complain_for(i))).collect();
            Some(one(about.into_iter().collect()))
        }
        "complain-lie" => Some(one(t.complaints.iter().map(|(from, _)| *from).collect())),
        "commit-omit" | "commit-lie" => Some(one(without(&t.premature_raw))),
        other => panic!("{other}"),
    }
}

/// The phase 3-4 rates and draws of the erring member `me` under `label`, carried forward as
/// `feature_llmq_dkgerrors.py` sets them; a phase-2 error draws nothing here.
fn erring_by_label(t: &Transcript, me: usize, label: &str) -> (Vec<(SimError, i32)>, Vec<Draw>) {
    let liar_says = Transcript::sent_by(&t.justifications, me)
        .first()
        .map(|raw| Justification::read(raw, &t.params, BLS).expect("reads"));
    let (scenario, justify) = match label {
        "contribution-lie-justify-omit" => (Scenario::ContributionLie, Some(SimError::JustifyOmit)),
        "contribution-lie-justify-lie" => (Scenario::ContributionLie, Some(SimError::JustifyLie)),
        "complain-lie" => (Scenario::ComplainLie, None),
        _ => (Scenario::Honest, None),
    };
    erring_bs12(t, me, scenario, justify, liar_says.as_ref())
}

/// Member `me`'s run wrote, in order, the dkgdb rows its captured messages show Core's member
/// writing when fed in capture order (v24.0.0 `l/dkgsession.cpp`, `a/dkgsession.cpp`):
/// - a `qdkg_V` row with each contribution's vector (`l/` :236);
/// - a `qdkg_E` row with the blobs of each contribution whose share it kept: all but those a
///   `complain-lie` member (`lies`) lies about (`l/` :240-266, `a/` :173);
/// - a `qdkg_S` row with each kept share its captured complaint does not complain about
///   (`a/` :206);
/// - then, per justification revealing its share, a `qdkg_S` row with that key when the sender
///   sent one contribution, every member it reveals a key for complained about it, and the key
///   is the sender's true share (`l/` :436-485).
fn check_rows(t: &Transcript, me: usize, run: &Run, lies: bool) {
    let label = format!("{} member {me}", t.label);
    let complains_about = |who: usize, about: usize| {
        Transcript::sent_by(&t.complaints, who).first().is_some_and(|raw| {
            Complaint::read(raw, &t.params, BLS).expect("reads").complain_for(about)
        })
    };
    let contributions: Vec<(usize, Contribution)> = t
        .contributions
        .iter()
        .map(|(from, raw)| (*from, Contribution::read(raw, &t.params, BLS).expect("reads")))
        .collect();
    let mut want = Vec::new();
    let mut kept = Vec::new();
    for (from, qc) in &contributions {
        let who = t.members[*from].pro_tx_hash;
        want.push(StoreRow::Vvec {
            who,
            vvec: qc.vvec().to_vec(),
        });
        let share = t.keys[me].decrypt_share(qc.blobs(), me, FLAG);
        let share = share
            .filter(BlsSkValue::is_valid)
            .unwrap_or_else(|| panic!("{label}: {from}'s share decrypts"));
        if !lies || *from == me {
            kept.push((*from, qc, share));
        }
    }
    for (from, qc, _) in &kept {
        let who = t.members[*from].pro_tx_hash;
        want.push(StoreRow::Encrypted {
            who,
            blobs: qc.blobs().clone(),
        });
    }
    for (from, _, share) in kept.iter().filter(|(from, ..)| !complains_about(me, *from)) {
        let who = t.members[*from].pro_tx_hash;
        want.push(StoreRow::Sk {
            who,
            sk: share.clone(),
        });
    }
    for (from, raw) in &t.justifications {
        let qj = Justification::read(raw, &t.params, BLS).expect("reads");
        let one = Transcript::sent_by(&t.contributions, *from).len() == 1;
        let answered = qj.keys().iter().all(|(i, _)| complains_about(*i as usize, *from));
        let true_share = &t.own_shares[*from][me];
        for (_, sk) in qj.keys().iter().filter(|(i, _)| *i as usize == me) {
            if one && answered && true_share.is_valid() && sk == true_share {
                let who = t.members[*from].pro_tx_hash;
                want.push(StoreRow::Sk {
                    who,
                    sk: sk.clone(),
                });
            }
        }
    }
    let rows = &run.env.store.rows;
    assert!(
        rows.iter().all(|(lt, base, _)| (*lt, *base) == (t.llmq_type, t.quorum_hash)),
        "{label}"
    );
    let got: Vec<&StoreRow> = rows.iter().map(|(_, _, row)| row).collect();
    assert_eq!(got, want.iter().collect::<Vec<_>>(), "{label}: dkgdb rows");
}

#[test]
fn t3c_c0_dkgerrors_by_label_every_member_commits_as_core_did() {
    // DEC-619's labelled rerun: each session replayed under its label's rates, on both tags.
    // The commit-omit session's erring member omits; the commit-lie one's lies, under each
    // lie type, since the lied commitment is never on the wire (X2).
    let file = corpus(DKGERRORS);
    assert_eq!(file["scheme"], "basic");
    let params = effective(&file, 100);
    for tag in TAGS {
        let mut committed = Committed::default();
        let mut checked = Checked::default();
        let mut captured = 0;
        for q in items(&file[tag]) {
            let label = q["scenario"].as_str().expect("a label");
            let t = c0(q, params, format!("{tag} {label}"));
            let erring: Vec<usize> = items(&q["erring"]).iter().map(index).collect();
            assert_eq!(
                shown_erring(&t, label).into_iter().collect::<Vec<_>>(),
                erring,
                "{tag} {label}"
            );
            captured += t.premature_raw.len();
            for me in 0..t.members.len() {
                let erring_here = erring.contains(&me);
                let ways: Vec<Option<Commit>> = match label {
                    "commit-omit" if erring_here => vec![Some(Commit::Omit)],
                    "commit-lie" if erring_here => (0..5).map(|k| Some(Commit::Lie(k))).collect(),
                    _ => vec![None],
                };
                for how in ways {
                    let (sim, draws) = match how {
                        Some(how) => how.rates_and_draws(),
                        None if erring_here => erring_by_label(&t, me, label),
                        None => Default::default(),
                    };
                    let mut run = replay(&t, me, &ConnSnapshot::default(), &sim, draws);
                    check(&t, me, &run, None, &mut checked);
                    check_rows(&t, me, &run, label == "complain-lie" && erring_here);
                    assert_eq!(
                        run.session.valid_members(),
                        t.final_valid,
                        "{} member {me}",
                        t.label
                    );
                    match how {
                        Some(how) => {
                            check_withheld(&t, me, &mut run, how);
                            committed.withheld += 1;
                        }
                        None => committed.sent += check_sent(&t, me, &mut run),
                    }
                }
            }
        }
        assert_eq!(
            committed,
            Committed {
                sent: captured,
                withheld: 6,
            },
            "{tag}"
        );
        assert_eq!(checked.runs, 24 + 4, "{tag}: every member, and the liar under each lie type");
    }
}

#[test]
fn t3c_c0_rotation_every_member_sends_the_captured_premature_commitment() {
    let file = corpus(ROTATION);
    assert_eq!(file["scheme"], "basic");
    for tag in TAGS {
        let mut checked = Checked::default();
        let mut sent = 0;
        for q in items(&file[tag]) {
            let t = u8::try_from(index(&q["llmq_type"])).expect("a type");
            let label = format!("{tag} t{t} {}", &q["quorum_hash"].as_str().expect("hex")[..8]);
            let t = c0(q, effective(&file, t), label);
            for me in 0..t.members.len() {
                let mut run = replay(&t, me, &ConnSnapshot::default(), &[], Vec::new());
                check(&t, me, &run, None, &mut checked);
                sent += check_sent(&t, me, &mut run);
            }
        }
        assert_eq!((checked.runs, checked.premature, sent), (44, 44, 44), "{tag}");
    }
}

// v24.0.0 `contribution-clone` (LQ4b-2a C8).

const CLONE: (&str, &str) =
    ("dkg_lq4b_clone.json5", "e276a5b0b0a53c6a86c75fb2b78020982bb8e449875bbc2f91651ceec91da106");

/// The clone file's sessions, each with its cloners: `feature_llmq_simplepose.py`'s
/// `test_cloned_contributions` (v24.0.0 :113-136), whose cloners `mninfo[0..2]` run
/// `contribution-clone` at rate 100.
fn clone_sessions() -> Vec<(Transcript, Vec<usize>)> {
    let file = corpus(CLONE);
    assert_eq!(file["scheme"], "basic");
    items(&file["v24"])
        .iter()
        .map(|q| {
            let label = q["scenario"].as_str().expect("a label");
            assert_eq!(SimError::from_name(label), Some(SimError::ContributionClone));
            let ty = u8::try_from(index(&q["llmq_type"])).expect("a type");
            let hash = &q["quorum_hash"].as_str().expect("hex")[..8];
            let t = c0(q, effective(&file, ty), format!("v24 t{ty} {hash} {label}"));
            (t, items(&q["erring"]).iter().map(index).collect())
        })
        .collect()
}

#[test]
fn t3c_v24_clone_every_member_marks_both_cloners_bad_and_commits_as_core_did() {
    // Each cloner's run adopts its captured contribution, so the clone rate draws nothing in
    // phases 3-5. Every share decrypts and is kept, nobody complains, and `VerifyAndCommit`
    // marks the two members whose vectors are equal bad on every member, the cloners included.
    let sessions = clone_sessions();
    assert_eq!(sessions.len(), 1);
    for (t, cloners) in &sessions {
        let n = t.members.len();
        assert_eq!((n, cloners.len()), (5, 2), "{}", t.label);
        let vvecs: Vec<Vec<BlsPkValue>> = (0..n).map(|m| t.own_vvec(m)).collect();
        for (a, b) in (0..n).flat_map(|a| (a + 1..n).map(move |b| (a, b))) {
            let both = cloners.contains(&a) && cloners.contains(&b);
            assert_eq!(vvecs[a] == vvecs[b], both, "{}: vectors of {a} and {b}", t.label);
        }
        let others: Vec<bool> = (0..n).map(|m| !cloners.contains(&m)).collect();
        assert_eq!(t.final_valid, others, "{}: the captured final's members", t.label);
        let mut checked = Checked::default();
        let mut sent = 0;
        for me in 0..n {
            let label = format!("{} member {me}", t.label);
            let sim = if cloners.contains(&me) {
                vec![(SimError::ContributionClone, 100)]
            } else {
                Vec::new()
            };
            let mut run = replay(t, me, &ConnSnapshot::default(), &sim, Vec::new());
            check(t, me, &run, None, &mut checked);
            check_rows(t, me, &run, false);
            assert_eq!(run.session.valid_members(), t.final_valid, "{label}");
            let events = lines(&run.session.take_events());
            let marked = "VerifyAndCommit --   members sharing verification vector coefficients \
                          and now marked as bad:";
            let at = events.iter().position(|l| l == marked);
            let at = at.unwrap_or_else(|| panic!("{label}: {events:?}"));
            let listed: Vec<String> = cloners
                .iter()
                .map(|&c| format!("VerifyAndCommit --     {}", t.members[c].pro_tx_hash))
                .collect();
            assert_eq!(events[at + 1..at + 3], listed[..], "{label}: the cloners, in index order");
            sent += check_sent(t, me, &mut run);
            let (finals, reads) = finals(t, &mut run);
            assert_eq!(
                finals,
                std::slice::from_ref(&t.final_raw),
                "{label}: the captured qfcommit"
            );
            assert_eq!(reads, [2], "{label}");
        }
        assert_eq!(
            checked,
            Checked {
                runs: 5,
                complaints: 0,
                uncaptured: 0,
                justifications: 0,
                premature: 5,
            },
            "{}",
            t.label
        );
        assert_eq!(sent, 5, "{}: every member's qpcommit", t.label);
    }
}

#[test]
fn t3c_v24_clone_each_cloner_deals_its_captured_contribution() {
    // `contribute` under `contribution-clone` at rate 100 derives the verification vector and
    // the shares from the quorum hash alone (v24.0.0 `a/dkgsession.cpp:53-68`): the cloner's
    // captured vector, and the shares its recipients decrypted.
    let sessions = clone_sessions();
    assert_eq!(sessions.len(), 1);
    for (t, cloners) in sessions {
        assert_eq!(cloners.len(), 2, "{}", t.label);
        for &me in &cloners {
            let label = format!("{} member {me}", t.label);
            let (mut session, mut env) = member_of(&t, me);
            env.sim.set(SimError::ContributionClone, 100).expect("a rate");
            let qc = session.contribute(&mut env.env()).expect("phase order").expect("sent");
            assert_eq!(qc.vvec(), &t.own_vvec(me)[..], "{label}");
            assert_eq!(session.own_shares(), &t.own_shares[me][..], "{label}");
            let events = lines(&session.take_events());
            let dealt = "Contribute -- dealing a cloned contribution";
            assert!(events.iter().any(|l| l == dealt), "{label}: {events:?}");
        }
    }
}
