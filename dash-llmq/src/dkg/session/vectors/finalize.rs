//! T2 and T3-C for finalization (LQ4b-1): `FinalizeCommitments` and
//! `FinalizeSingleCommitment` against Core's corpora, each final byte for byte.
//!
//! A captured session is finalized by each of its members in turn: its store holds the
//! `qdkg_V` row of every captured contribution, as every member's did once phase 2 ended (Core
//! writes the row on receipt, `l/dkgsession.cpp:234`), and the captured premature commitments
//! arrive in capture order in pops of 8. The final must be the one Core mined, and the
//! reference verifier (`verify_commitment` with the signatures checked) must accept it. The
//! base height is not in the captures: it is the cycle's first height plus the final's
//! `quorumIndex`, which is what `Verify` checks of it.

use dash_params::LlmqParams;
use dash_types::Numeric;

use super::*;
use crate::dkg::bls::{BlsSigValue, make_new_key, verify_insecure};
use crate::dkg::reference::ReferenceVerifier;
use crate::dkg::rng::{Draw, SeededRng};
use crate::dkg::session::fixture::{BLS, batch_of};
use crate::dkg::wire::{PrematureCommitment, final_commitment_bytes};

pub(super) const DKGERRORS: (&str, &str) = (
    "dkg_lq4b_dkgerrors.json5",
    "29968b2366db456b09ba2537e6d4e1beb2469bffe5eaa20c5b12dd5509059df5",
);
pub(super) const ROTATION: (&str, &str) =
    ("dkg_lq4b_rotation.json5", "6815a7b4c19deba0254de0bf1d0444087eecc8364c9e94a5bd7b926f1f3a9ae6");
const SINGLENODE: (&str, &str) = (
    "dkg_lq4b_singlenode.json5",
    "7a30035fa6a2e515212d9331e415b6d58d20b8ad2fa9a095a106b861877e6490",
);

/// The two tags the C0 files hold, each run's sessions under its key.
pub(super) const TAGS: [&str; 2] = ["rc4", "rc5"];

/// The effective parameters of `t` in a C0 file.
pub(super) fn effective(file: &Value, t: u8) -> LlmqParams {
    let p = &file["params"][t.to_string()];
    let int = |k: &str| u16::try_from(p[k].as_u64().expect("an int")).expect("small");
    LlmqParams {
        size: int("size"),
        min_size: int("min_size"),
        threshold: int("threshold"),
        dkg_bad_votes_threshold: int("bad_votes"),
        ..params(LlmqType::from_base(t))
    }
}

fn hexes(value: &Value) -> Vec<u8> {
    unhex(value.as_str().expect("hex"))
}

/// What a final commitment's bytes say of its session: `(version, quorumIndex)`.
fn version_and_index(raw: &[u8]) -> (u16, i16) {
    let version = u16::from_le_bytes([raw[0], raw[1]]);
    let index = if matches!(version, 2 | 4) {
        i16::from_le_bytes([raw[35], raw[36]])
    } else {
        0
    };
    (version, index)
}

/// One captured multi-member session.
struct Captured {
    label: String,
    llmq_type: LlmqType,
    params: LlmqParams,
    quorum_hash: QuorumHash,
    members: Vec<TestMember>,
    keys: Vec<TestOperatorKey>,
    contributions: Vec<Vec<u8>>,
    premature: Vec<(usize, Vec<u8>)>,
    final_raw: Vec<u8>,
}

impl Captured {
    /// A session of a C0 file (`[member, raw]` rows) under `params`.
    fn c0(q: &Value, params: LlmqParams, label: String) -> Self {
        let (members, keys) = members_of(items(&q["members"]).iter().map(|m| (&m[0], &m[1])));
        Self {
            label,
            llmq_type: LlmqType::from_base(u8::try_from(index(&q["llmq_type"])).expect("a type")),
            params,
            quorum_hash: QuorumHash::from(arr::<32>(&q["quorum_hash"])),
            members,
            keys,
            contributions: items(&q["contributions"]).iter().map(|r| hexes(&r[1])).collect(),
            premature: items(&q["premature"])
                .iter()
                .map(|r| (index(&r[0]), hexes(&r[1])))
                .collect(),
            final_raw: hexes(&q["final"]),
        }
    }

    /// A session of BS12's `dkg.json5`.
    fn bs12(q: &Value) -> Self {
        let rows = items(&q["members"]).iter().map(|m| (&m["pro_tx"], &m["operator_sk"]));
        let (members, keys) = members_of(rows);
        Self {
            label: format!("bs12 {}", &q["quorum_hash"].as_str().expect("hex")[..8]),
            llmq_type: LlmqType::LlmqTest,
            params: params(LlmqType::LlmqTest),
            quorum_hash: QuorumHash::from(arr::<32>(&q["quorum_hash"])),
            members,
            keys,
            contributions: items(&q["contributions"]).iter().map(|c| hexes(&c["raw"])).collect(),
            premature: items(&q["premature"])
                .iter()
                .map(|p| (index(&p["member"]), hexes(&p["raw"])))
                .collect(),
            final_raw: hexes(&q["final"]["raw"]),
        }
    }

    /// Member `me`'s finals, and the verifier's calls.
    fn finalize_as(&self, me: usize) -> (Vec<LlmqCommitment>, ReferenceVerifier) {
        let (version, quorum_index) = version_and_index(&self.final_raw);
        let rotated = matches!(version, 2 | 4);
        let v19 = matches!(version, 3 | 4);
        let height = u32::from(self.params.dkg_interval) * 10 + u32::from(quorum_index as u16);
        let init = SessionInit {
            llmq_type: self.llmq_type,
            params: &self.params,
            base: QuorumBase {
                hash: self.quorum_hash,
                height: i32::try_from(height).expect("a height"),
            },
            quorum_index: i32::from(quorum_index),
            members: Arc::from(self.members.clone()),
            active_pro_tx: Some(self.members[me].pro_tx_hash),
            rotation_enabled: rotated,
            v19_after_base: v19,
        };
        let topo = &mut CoreRelayTopology {
            members: self.members.iter().map(|m| m.pro_tx_hash).collect(),
            calls: 0,
        };
        let mut env = TestEnv::new(&self.keys[me], 0);
        let mut session = DkgSession::init(init, topo, &mut env.debug).expect("enough members");
        env.store.members = self.members.iter().map(|m| m.pro_tx_hash).collect();
        for raw in &self.contributions {
            let qc = Contribution::read(raw, &self.params, BLS).expect("reads");
            env.store.write_vvec(self.llmq_type, &self.quorum_hash, qc.pro_tx_hash(), qc.vvec());
        }
        for pop in self.premature.chunks(8) {
            let outcome = session.process_batch(
                DkgMsgKind::PrematureCommitment,
                &batch_of(me, pop),
                &mut env.env(),
            );
            assert!(outcome.misbehaving.is_empty(), "{} member {me}", self.label);
        }
        let mut verifier =
            ReferenceVerifier::new(self.llmq_type, &self.params, height, &self.members);
        verifier.v19_after_base = v19;
        verifier.dip24 = rotated;
        let finals = session.finalize_commitments(&mut DkgEnv {
            verify: &mut verifier,
            ..env.env()
        });
        (finals, verifier)
    }

    /// Every member finalizes exactly the captured final, which the verifier accepts after
    /// reading the members twice. The number of finalizers.
    fn check_every_finalizer(&self) -> usize {
        for me in 0..self.members.len() {
            let (finals, verifier) = self.finalize_as(me);
            let bytes: Vec<String> =
                finals.iter().map(|qc| to_hex(&final_commitment_bytes(qc))).collect();
            assert_eq!(bytes, [to_hex(&self.final_raw)], "{} member {me}", self.label);
            assert_eq!(verifier.member_reads(), [2], "{} member {me}", self.label);
        }
        self.members.len()
    }
}

fn members_of<'a>(
    rows: impl Iterator<Item = (&'a Value, &'a Value)>,
) -> (Vec<TestMember>, Vec<TestOperatorKey>) {
    rows.map(|(pro_tx, operator_sk)| {
        let key = TestOperatorKey::new(sk(operator_sk, BlsSchemeKind::Basic));
        (TestMember::new(ProTxHash::from(arr::<32>(pro_tx)), key.public_key()), key)
    })
    .unzip()
}

#[test]
fn t2_llmq_100_finalize_gives_cores_members_sig_and_recovered_quorum_sig() {
    let corpus = corpus(LLMQ_100);
    for (scheme, ctx) in SCHEMES {
        let s = &corpus[scheme];
        let flag = ctx.scheme();
        let f = &s["finalize"];
        let ids: Vec<[u8; 32]> = items(&s["inputs"]["member_ids"]).iter().map(get_hex_id).collect();
        // `member_pks` follows `signer_ids`.
        let mut operator = vec![None; 3];
        for (id, pk_hex) in items(&f["signer_ids"]).iter().zip(items(&f["member_pks"])) {
            let i = ids.iter().position(|x| *x == get_hex_id(id)).expect("a member");
            operator[i] = Some(pk(pk_hex, flag));
        }
        let members: Vec<TestMember> = ids
            .iter()
            .zip(operator)
            .map(|(id, pk)| TestMember::new(ProTxHash::from(*id), pk.expect("each member signs")))
            .collect();
        let quorum_hash = QuorumHash::from(get_hex_id(&f["quorum_hash"]));
        let pcommits: Vec<(usize, Vec<u8>)> = items(&s["commit"])
            .iter()
            .map(|c| {
                let me = index(&c["member_idx"]);
                let valid: Vec<bool> =
                    items(&c["valid_members"]).iter().map(|v| *v == true).collect();
                let qc = PrematureCommitment::unsigned(
                    LlmqType::LlmqTest,
                    &quorum_hash,
                    &members[me].pro_tx_hash,
                    &valid,
                    &pk(&c["quorum_public_key"], flag),
                    &get_hex_id(&c["quorum_vvec_hash"]),
                    flag,
                );
                assert_eq!(*qc.sign_hash(), get_hex_id(&c["commitment_hash"]), "{scheme}");
                let sig =
                    |k: &str| BlsSigValue::from_canonical(&arr(&c[k]), flag).expect("a signature");
                let qc = qc
                    .with_quorum_sig(sig("quorum_sig_share"), flag)
                    .with_sig(sig("member_sig"), flag);
                (me, qc.payload(flag))
            })
            .collect();
        // Core's `cvec` order, the session-hash order, is the finalize step's signer order.
        let mut by_hash: Vec<(usize, [u8; 32])> = pcommits
            .iter()
            .map(|(me, raw)| {
                (
                    *me,
                    *PrematureCommitment::read(raw, &params(LlmqType::LlmqTest), ctx)
                        .expect("reads")
                        .session_hash(),
                )
            })
            .collect();
        by_hash.sort_by_key(|(_, hash)| *hash);
        let order: Vec<[u8; 32]> = by_hash.iter().map(|(me, _)| ids[*me]).collect();
        let signers: Vec<[u8; 32]> = items(&f["signer_ids"]).iter().map(get_hex_id).collect();
        assert_eq!(order, signers, "{scheme}");

        let want_members_sig =
            BlsSigValue::from_canonical(&arr(&f["aggregated_members_sig"]), flag).expect("sig");
        let want_quorum_sig =
            BlsSigValue::from_canonical(&arr(&f["recovered_quorum_sig"]), flag).expect("sig");
        let (_, keys) = quorum(3, 0x100);
        for me in 0..3 {
            let (session, _, _) = crate::dkg::session::fixture::init_params(
                LlmqType::LlmqTest,
                &params(LlmqType::LlmqTest),
                &members,
                Some(members[me].pro_tx_hash),
                scheme == "ietf",
            );
            let mut session = session.expect("three members");
            // The base hash is the corpus's quorum hash.
            session.base.hash = quorum_hash;
            let mut env = TestEnv::new(&keys[me], 0);
            env.store.members = members.iter().map(|m| m.pro_tx_hash).collect();
            for row in items(&s["contribute"]) {
                let c = index(&row["member_idx"]);
                let vvec: Vec<BlsPkValue> =
                    items(&row["vvec"]).iter().map(|v| pk(v, flag)).collect();
                env.store.write_vvec(
                    LlmqType::LlmqTest,
                    &quorum_hash,
                    &members[c].pro_tx_hash,
                    &vvec,
                );
            }
            let outcome = session.process_batch(
                DkgMsgKind::PrematureCommitment,
                &batch_of(me, &pcommits),
                &mut DkgEnv {
                    bls: ctx,
                    ..env.env()
                },
            );
            assert!(outcome.misbehaving.is_empty(), "{scheme} member {me}");
            assert_eq!(outcome.relay.len(), 3, "{scheme} member {me}: each verified and relayed");
            let mut verifier = ReferenceVerifier::new(
                LlmqType::LlmqTest,
                &params(LlmqType::LlmqTest),
                48,
                &members,
            );
            verifier.v19_after_base = scheme == "ietf";
            verifier.bls = ctx;
            let finals = session.finalize_commitments(&mut DkgEnv {
                bls: ctx,
                verify: &mut verifier,
                ..env.env()
            });
            let [qc] = &finals[..] else {
                panic!("{scheme} member {me}: one final")
            };
            assert_eq!(qc.members_sig().value(), &want_members_sig, "{scheme} member {me}");
            assert_eq!(qc.quorum_sig().value(), &want_quorum_sig, "{scheme} member {me}");
            assert_eq!(qc.signers().count_ones(), 3, "{scheme} member {me}");
            assert_eq!(
                qc.version(),
                if scheme == "ietf" {
                    3
                } else {
                    1
                },
                "{scheme} member {me}"
            );
            assert_eq!(verifier.member_reads(), [2], "{scheme} member {me}");
            let hash = get_hex_id(&s["commit"][0]["commitment_hash"]);
            let quorum_pk = pk(&s["commit"][0]["quorum_public_key"], flag);
            assert!(verify_insecure(&want_quorum_sig, &quorum_pk, &hash, flag), "{scheme}");
        }
    }
}

#[test]
fn t3c_bs12_every_finalizer_sends_the_captured_final() {
    let corpus = corpus(BS12_DKG);
    let quorums = items(&corpus["quorums"]);
    let runs: usize = quorums.iter().map(|q| Captured::bs12(q).check_every_finalizer()).sum();
    assert_eq!((quorums.len(), runs), (8, 24));
}

#[test]
fn t3c_c0_dkgerrors_every_finalizer_sends_the_captured_final() {
    let file = corpus(DKGERRORS);
    assert_eq!(file["scheme"], "basic");
    let params = effective(&file, 100);
    for tag in TAGS {
        let mut labels = Vec::new();
        let mut runs = 0;
        for q in items(&file[tag]) {
            let label = q["scenario"].as_str().expect("a label").to_owned();
            let erring: Vec<usize> = items(&q["erring"]).iter().map(index).collect();
            assert_eq!(erring.len(), usize::from(label != "honest"), "{tag} {label}");
            runs += Captured::c0(q, params, format!("{tag} {label}")).check_every_finalizer();
            labels.push(label);
        }
        assert_eq!(runs, 24, "{tag}");
        assert_eq!(
            labels,
            [
                "honest",
                "contribution-omit",
                "contribution-lie-justified",
                "contribution-lie-justify-omit",
                "contribution-lie-justify-lie",
                "complain-lie",
                "commit-omit",
                "commit-lie",
            ],
            "{tag}"
        );
    }
}

#[test]
fn t3c_c0_rotation_every_finalizer_sends_the_captured_final() {
    let file = corpus(ROTATION);
    assert_eq!(file["scheme"], "basic");
    for tag in TAGS {
        let mut seen = Vec::new();
        let mut runs = 0;
        for q in items(&file[tag]) {
            let t = u8::try_from(index(&q["llmq_type"])).expect("a type");
            let c = Captured::c0(
                q,
                effective(&file, t),
                format!("{tag} t{t} {}", &q["quorum_hash"].as_str().expect("hex")[..8]),
            );
            let (version, quorum_index) = version_and_index(&c.final_raw);
            seen.push((t, version, quorum_index));
            runs += c.check_every_finalizer();
        }
        seen.sort_unstable();
        let want: Vec<(u8, u16, i16)> =
            [(100, 3, 0); 5].into_iter().chain([(103, 4, 0); 3]).chain([(103, 4, 1); 3]).collect();
        assert_eq!(seen, want, "{tag}: rotated sessions at index 0 and 1, version 4");
        assert_eq!(runs, 44, "{tag}");
    }
}

#[test]
fn t3c_c0_single_member_final_is_the_captured_one() {
    let file = corpus(SINGLENODE);
    assert_eq!(file["scheme"], "basic");
    for tag in TAGS {
        let mut types = Vec::new();
        for q in items(&file[tag]) {
            let t = u8::try_from(index(&q["llmq_type"])).expect("a type");
            let params = effective(&file, t);
            let label = format!("{tag} t{t} {}", &q["quorum_hash"].as_str().expect("hex")[..8]);
            let (members, keys) = members_of([(&q["member"][0], &q["member"][1])].into_iter());
            let height =
                u32::try_from(q["base_height"].as_u64().expect("a height")).expect("small");
            let v19 = q["v19_after_base"] == true;
            let init = SessionInit {
                llmq_type: LlmqType::from_base(t),
                params: &params,
                base: QuorumBase {
                    hash: QuorumHash::from(arr::<32>(&q["quorum_hash"])),
                    height: i32::try_from(height).expect("a height"),
                },
                quorum_index: 0,
                members: Arc::from(members.clone()),
                active_pro_tx: Some(members[0].pro_tx_hash),
                rotation_enabled: false,
                v19_after_base: v19,
            };
            let topo = &mut CoreRelayTopology {
                members: vec![members[0].pro_tx_hash],
                calls: 0,
            };
            let mut env = TestEnv::new(&keys[0], 0);
            let mut session = DkgSession::init(init, topo, &mut env.debug).expect("one member");
            let mut verifier =
                ReferenceVerifier::new(LlmqType::from_base(t), &params, height, &members);
            verifier.v19_after_base = v19;
            let sk1 = make_new_key(&mut SeededRng::new(u64::from(t)));
            let mut rng = ReplayRng::new([Draw::Key(KeyDraw::SingleMemberSk1, sk1)]);
            let qc = session
                .finalize_single_commitment(&mut DkgEnv {
                    rng: &mut rng,
                    verify: &mut verifier,
                    ..env.env()
                })
                .expect("verified")
                .expect("a member");
            rng.finish().unwrap_or_else(|e| panic!("{label}: {e:?}"));
            assert_eq!(
                to_hex(&final_commitment_bytes(&qc)),
                to_hex(&hexes(&q["final"])),
                "{label}"
            );
            assert_eq!(verifier.member_reads(), [2], "{label}");
            types.push(t);
        }
        types.sort_unstable();
        let want: Vec<u8> = [100; 6].into_iter().chain([104; 5]).chain([106; 6]).collect();
        assert_eq!(types, want, "{tag}");
    }
}
