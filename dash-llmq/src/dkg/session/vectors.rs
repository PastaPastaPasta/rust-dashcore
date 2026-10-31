//! T2 and T3-C (LQ4a-1): the session against Core's corpora in `test-vectors/lq4`
//! (`PROVENANCE.md` there). Each file's sha256 is checked before it is read.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;

use dash_consensus::hash::{sha256, sha256d};
use serde_json::Value;

use super::*;
use crate::dkg::bls::{
    BlsCtx, BlsSchemeKind, BlsSkValue, IesSeed, verify_contribution_share,
    verify_verification_vector,
};
use crate::dkg::reference::{CoreRelayTopology, TestMember, TestOperatorKey, quorum};
use crate::dkg::rng::{DkgRng, Draw, KeyDraw, RangeDraw, ReplayRng};
use crate::dkg::session::fixture::{TestEnv, member_session, params};
use crate::dkg::sim::SimError;

const LLMQ_100: (&str, &str) =
    ("bls_llmq_100.json5", "72bec3e551d22a6b511b87e73bbc8f0215736a91bf9ee7efe79ac514c8c33557");
const BS12_DKG: (&str, &str) =
    ("bls_oracle_dkg.json5", "e5d2163fefe7090b7c5357d57ffd98396053b1459e0b42bf038e2f7731bdcaa2");

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
        .collect()
}

fn arr<const N: usize>(value: &Value) -> [u8; N] {
    unhex(value.as_str().expect("a hex string")).try_into().expect("its length")
}

fn items(value: &Value) -> &Vec<Value> {
    value.as_array().expect("an array")
}

fn index(value: &Value) -> usize {
    usize::try_from(value.as_u64().expect("an index")).expect("small")
}

/// The corpus, once its sha256 is the one `PROVENANCE.md` names, with its `//` lines dropped
/// (its only JSON5 extension).
fn corpus((name, sha): (&str, &str)) -> Value {
    let path = format!("{}/../test-vectors/lq4/{name}", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    assert_eq!(to_hex(&sha256(&bytes)), sha, "{name} is not the copy PROVENANCE.md names");
    let text = std::str::from_utf8(&bytes).expect("UTF-8");
    let json: Vec<&str> = text.lines().filter(|l| !l.trim_start().starts_with("//")).collect();
    serde_json::from_str(&json.join("\n")).expect("JSON without the comment lines")
}

fn pk(value: &Value, flag: BlsSchemeKind) -> BlsPkValue {
    BlsPkValue::from_canonical(&arr(value), flag).expect("a canonical public key")
}

fn sk(value: &Value, flag: BlsSchemeKind) -> BlsSkValue {
    BlsSkValue::from_canonical(&arr(value), flag).expect("a canonical secret key")
}

/// LLMQ-100's sections: `chia` is Core's legacy scheme, `ietf` its basic one.
const SCHEMES: [(&str, BlsCtx); 2] = [("chia", BlsCtx::LEGACY), ("ietf", BlsCtx::BASIC)];

/// A member id written as `GetHex` prints it: the stored bytes reversed.
fn get_hex_id(value: &Value) -> [u8; 32] {
    let mut id = arr(value);
    id.reverse();
    id
}

#[test]
fn t2_each_share_fits_its_vvec_at_the_members_id() {
    let corpus = corpus(LLMQ_100);
    for (scheme, ctx) in SCHEMES {
        let s = &corpus[scheme];
        let flag = ctx.scheme();
        let ids: Vec<[u8; 32]> = items(&s["inputs"]["member_ids"]).iter().map(get_hex_id).collect();
        let mut checked = 0;
        for row in items(&s["contribute"]) {
            assert_eq!(get_hex_id(&row["member_id"]), ids[index(&row["member_idx"])]);
            let vvec: Vec<BlsPkValue> = items(&row["vvec"]).iter().map(|v| pk(v, flag)).collect();
            assert!(verify_verification_vector(&vvec, flag), "{scheme}");
            for (j, share) in items(&row["sk_shares"]).iter().enumerate() {
                assert!(verify_contribution_share(&ids[j], &vvec, &sk(share, flag), flag));
                checked += 1;
            }
        }
        assert_eq!(checked, 9, "{scheme}");
    }
}

#[test]
fn t2_the_pending_verify_gives_cores_verdicts() {
    let corpus = corpus(LLMQ_100);
    let (_, keys) = quorum(3, 0x100);
    for (scheme, ctx) in SCHEMES {
        let s = &corpus[scheme];
        let flag = ctx.scheme();
        let ids: Vec<[u8; 32]> = items(&s["inputs"]["member_ids"]).iter().map(get_hex_id).collect();
        let members: Vec<TestMember> = ids
            .iter()
            .zip(&keys)
            .map(|(id, key)| TestMember::new(ProTxHash::from(*id), key.public_key()))
            .collect();
        // The pending verify of member `me` over the row's vectors and shares, with contributor
        // `swap.0`'s share replaced by `swap.1`, if given: the verdicts.
        let verdicts = |row: &Value, me: usize, swap: Option<(usize, BlsSkValue)>| -> Vec<bool> {
            let mut session = member_session(params(LlmqType::LlmqTest), &members, Some(me), true);
            let vvecs = items(&row["received_vvecs"]);
            let shares = items(&row["received_sk_contributions"]);
            for c in 0..3 {
                let state = &mut session.member_state[c];
                state.received_vvec = Some(items(&vvecs[c]).iter().map(|v| pk(v, flag)).collect());
                state.received_sk = Some(sk(&shares[c], flag));
            }
            if let Some((c, share)) = swap {
                session.member_state[c].received_sk = Some(share);
            }
            session.pending = vec![0, 1, 2];
            let mut env = TestEnv::new(&keys[me], 1);
            let mut env = DkgEnv {
                bls: ctx,
                ..env.env()
            };
            session.verify_pending_contributions(&mut env);
            assert!(session.pending.is_empty());
            (0..3).map(|c| !session.member_state[c].we_complain).collect()
        };
        for row in items(&s["verify"]) {
            let me = index(&row["member_idx"]);
            let want: Vec<bool> = items(&row["verification_results"])
                .iter()
                .map(|v| v.as_bool().expect("a verdict"))
                .collect();
            assert_eq!(verdicts(row, me, None), want, "{scheme} member {me}");
            // A control, not a Core value (the file's verdicts are all true): contributor 1's
            // share for another member fits nothing at our id, and only it fails.
            let other = sk(&s["contribute"][1]["sk_shares"][(me + 1) % 3], flag);
            let got = verdicts(row, me, Some((1, other)));
            assert_eq!(got, [true, false, true], "{scheme} member {me}, a foreign share");
        }
    }
}

/// What a BS12 session's erring masternode did in phase 2, read from the transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Scenario {
    /// No error in phase 2 (the honest quorum, `commit-omit`, `commit-lie`).
    Honest,
    /// `contribution-omit`: two contributions.
    Omit,
    /// `contribution-lie`: one contributor's shares to others do not fit.
    ContributionLie,
    /// `complain-lie`: every share fits, yet one member complains.
    ComplainLie,
}

/// The session's scenario and its erring member.
fn classify(q: &Value) -> (Scenario, Option<usize>) {
    let contributions = items(&q["contributions"]);
    let senders: BTreeSet<usize> = contributions.iter().map(|c| index(&c["member"])).collect();
    if contributions.len() == 2 {
        let missing = (0..3).find(|m| !senders.contains(m));
        return (Scenario::Omit, missing);
    }
    let liar = contributions.iter().find(|c| {
        let sender = index(&c["member"]);
        items(&c["shares"]).iter().enumerate().any(|(j, s)| j != sender && s["fits"] == false)
    });
    if let Some(liar) = liar {
        return (Scenario::ContributionLie, Some(index(&liar["member"])));
    }
    match items(&q["complaints"]).as_slice() {
        [] => (Scenario::Honest, None),
        [one] => (Scenario::ComplainLie, Some(index(&one["member"]))),
        more => panic!("{} complaints with every share fitting", more.len()),
    }
}

#[test]
fn t3c_the_transcript_classifies_as_the_functional_test_runs() {
    // `feature_llmq_dkgerrors.py` (rc.4 :37-90): three quorums without a phase-2 error, then
    // omit, three contribution lies (justified, unjustified, lied justification) and a
    // complain lie, each by mninfo[0]. Its counts are minimums (`wait_for_quorum_phase`
    // fails only below them), and the transcript keeps the sessions in another order.
    use Scenario::{ComplainLie, ContributionLie, Honest, Omit};
    let expected = [
        (Honest, (3, 0, 0)),
        (Honest, (3, 0, 0)),
        (Honest, (3, 0, 0)),
        (Omit, (2, 0, 0)),
        (ContributionLie, (3, 2, 0)),
        (ContributionLie, (3, 2, 1)),
        (ContributionLie, (3, 2, 1)),
        (ComplainLie, (3, 1, 2)),
    ];
    let corpus = corpus(BS12_DKG);
    assert_eq!(corpus["scheme"], "basic");
    let quorums = items(&corpus["quorums"]);
    let mut got: Vec<(Scenario, (usize, usize, usize))> = Vec::new();
    let mut erring = BTreeSet::new();
    for q in quorums {
        let (scenario, who) = classify(q);
        let count = |k: &str| items(&q[k]).len();
        got.push((
            scenario,
            (count("contributions"), count("complaints"), count("justifications")),
        ));
        if let Some(who) = who {
            erring.insert(q["members"][who]["pro_tx"].as_str().expect("hex").to_owned());
        }
    }
    got.sort();
    assert_eq!(got.len(), expected.len());
    for ((scenario, counts), (want, min)) in got.iter().zip(expected) {
        assert_eq!(*scenario, want);
        assert_eq!(counts.0, min.0, "{scenario:?} contributions");
        assert!(counts.1 >= min.1 && counts.2 >= min.2, "{scenario:?} {counts:?} below {min:?}");
    }
    assert_eq!(erring.len(), 1, "one masternode errs throughout");
}

/// A [`ReplayRng`] that stays readable after the session's environment drops it.
struct Shared(Rc<RefCell<ReplayRng>>);

impl DkgRng for Shared {
    fn new_key(&mut self, why: KeyDraw) -> BlsSkValue {
        self.0.borrow_mut().new_key(why)
    }
    fn ies_seed(&mut self, recipients: usize) -> IesSeed {
        self.0.borrow_mut().ies_seed(recipients)
    }
    fn rand_bool(&mut self, percent: u8, why: SimError) -> bool {
        self.0.borrow_mut().rand_bool(percent, why)
    }
    fn rand_range(&mut self, n: u64, why: RangeDraw) -> u64 {
        self.0.borrow_mut().rand_range(n, why)
    }
}

#[test]
fn t3c_every_member_receives_the_captured_contributions_as_core_did() {
    let corpus = corpus(BS12_DKG);
    let flag = BlsSchemeKind::Basic;
    let (mut runs, mut complaints) = (0, 0);
    for q in items(&corpus["quorums"]) {
        let (scenario, erring) = classify(q);
        let quorum_hash = QuorumHash::from(arr::<32>(&q["quorum_hash"]));
        let members: Vec<TestMember> = items(&q["members"])
            .iter()
            .map(|m| {
                TestMember::new(
                    ProTxHash::from(arr::<32>(&m["pro_tx"])),
                    pk(&m["operator_pk"], flag),
                )
            })
            .collect();
        let keys: Vec<TestOperatorKey> = items(&q["members"])
            .iter()
            .map(|m| TestOperatorKey::new(sk(&m["operator_sk"], flag)))
            .collect();
        for (key, member) in keys.iter().zip(&members) {
            assert_eq!(key.public_key(), member.operator_pk);
        }
        let contributions = items(&q["contributions"]);
        let raws: Vec<Vec<u8>> =
            contributions.iter().map(|c| unhex(c["raw"].as_str().expect("hex"))).collect();

        for me in 0..3 {
            let init = SessionInit {
                params: params(LlmqType::LlmqTest),
                base: QuorumBase {
                    hash: quorum_hash,
                    height: 0,
                },
                quorum_index: 0,
                members: Arc::from(members.clone()),
                active_pro_tx: Some(members[me].pro_tx_hash),
                rotation_enabled: false,
                v19_after_base: true,
            };
            let topo = &mut CoreRelayTopology {
                members: members.iter().map(|m| m.pro_tx_hash).collect(),
                calls: 0,
            };
            let mut env = TestEnv::new(&keys[me], 0);
            let mut session = DkgSession::init(init, topo, &mut env.debug).expect("three members");
            let lies = scenario == Scenario::ComplainLie && erring == Some(me);
            let lie_draws = if lies {
                2
            } else {
                0
            };
            let replay = Rc::new(RefCell::new(ReplayRng::new(
                (0..lie_draws).map(|_| Draw::Bool(SimError::ComplainLie, 100, true)),
            )));
            env.rng = Box::new(Shared(replay.clone()));
            if lies {
                env.sim.set(SimError::ComplainLie, 100).unwrap();
            }

            let batch: Vec<(Sender<usize>, &[u8])> = contributions
                .iter()
                .zip(&raws)
                .map(|(c, raw)| {
                    let from = index(&c["member"]);
                    (
                        if from == me {
                            Sender::Own
                        } else {
                            Sender::Peer(from)
                        },
                        &raw[..],
                    )
                })
                .collect();
            let outcome = session.process_batch(DkgMsgKind::Contribution, &batch, &mut env.env());
            session.verify_pending_contributions(&mut env.env());
            drop(env.rng);
            Rc::try_unwrap(replay)
                .unwrap_or_else(|_| panic!("one owner"))
                .into_inner()
                .finish()
                .expect("the draws");

            let label = format!("{} member {me}", &q["quorum_hash"].as_str().unwrap()[..8]);
            assert!(contributions.iter().all(|c| c["sig_ok"] == true), "{label}");
            assert!(outcome.misbehaving.is_empty(), "{label}");
            let relayed: Vec<[u8; 32]> = outcome.relay.iter().map(|r| r.inv.hash).collect();
            let inv_hashes: Vec<[u8; 32]> = raws.iter().map(|raw| sha256d(raw)).collect();
            assert_eq!(relayed, inv_hashes, "{label}: no capture byte normalizes");

            let mut want_complain = BTreeSet::new();
            let (mut pushed, mut fit) = (Vec::new(), Vec::new());
            for c in contributions {
                let from = index(&c["member"]);
                let row = &c["shares"][me];
                let lied = lies && from != me;
                let share = row["share"].as_str().map(|_| sk(&row["share"], flag));
                if !lied {
                    assert_eq!(
                        session.member_state[from].received_sk, share,
                        "{label}: share from {from}"
                    );
                }
                let fits = share.is_some() && row["fits"] == true;
                if lied || !fits {
                    want_complain.insert(from);
                }
                if share.is_some() && !lied {
                    pushed.push(members[from].pro_tx_hash);
                    assert!(session.member_state[from].encrypted.is_some());
                    if fits {
                        fit.push(members[from].pro_tx_hash);
                    }
                }
            }
            let complained: BTreeSet<usize> =
                (0..3).filter(|&c| session.member_state[c].we_complain).collect();
            assert_eq!(complained, want_complain, "{label}");
            // The complaint Core sent in phase 3 carries these flags as `complainForMembers`.
            for complaint in items(&q["complaints"]).iter().filter(|c| index(&c["member"]) == me) {
                let want: Vec<bool> =
                    items(&complaint["complain_for"]).iter().map(|b| *b == true).collect();
                let got: Vec<bool> = (0..3).map(|c| session.member_state[c].we_complain).collect();
                assert_eq!(got, want, "{label}: complainForMembers");
                complaints += 1;
            }
            let senders: Vec<ProTxHash> =
                contributions.iter().map(|c| members[index(&c["member"])].pro_tx_hash).collect();
            assert_eq!(env.store.writers("V"), senders, "{label}");
            assert_eq!(env.store.writers("E"), pushed, "{label}");
            assert_eq!(env.store.writers("S"), fit, "{label}");
            runs += 1;
        }
    }
    assert_eq!(runs, 24);
    let captured: usize =
        items(&corpus["quorums"]).iter().map(|q| items(&q["complaints"]).len()).sum();
    assert_eq!(complaints, captured, "every captured complaint is checked");
}

mod replay;
