//! PF0 rows for the DKG's contribution, commitment and finalization phases: production
//! `DkgSession` code over bench-local stand-ins for the node's side (members, operator key, a
//! store and debug sink that keep nothing), with every draw from a fixed LCG through `CoreRng`, so
//! the instruction counts PF0 compares are deterministic. All setup, fixtures included, stays
//! outside the measured region.
//!
//! - `contribute/<type>`: one member's `contribute` on a fresh session (Core's `Contribute` then
//!   `SendContributions`: the coefficients, `size` shares and IES encryptions, the signature).
//! - `verify_pending/<type>`: the `ReceiveMessage` whose push is the 32nd, on a session that
//!   holds 31 pending shares: its `PreVerify`, signature check and decryption, then the 32-entry
//!   `VerifyPendingContributions` it runs (Core's mid-phase unit; the verify dominates). The
//!   primed session is built once and cloned for each iteration.
//! - `commit/400_60`: one member's `verify_and_commit` on a fresh session (Core's
//!   `VerifyAndCommit` then `SendCommitment`), its dkgdb holding every member's verification
//!   vector and share for it: the quorum verification vector over all of them, the share sum,
//!   both signatures. The session holds no received verification vectors (only a contribution's
//!   receive sets them), so the shared-coefficient scan runs over none; over 400 vectors it is
//!   about 1% of the row's instructions.
//! - `pcommit_verify/400_60`: the `ReceiveMessage` of one honest premature commitment on a fresh
//!   session, with the same dkgdb: its `PreVerify` and signature check, the quorum verification
//!   vector, the sender's public key share and the `quorumSig` check.
//! - `finalize/<type>`: one member's `finalize_commitments` over every member's premature
//!   commitment, each received and accepted: `membersSig`'s secure aggregate and the `quorumSig`
//!   recovery over all of them. The final's `Verify` is the node's, which the bench accepts. The
//!   primed session is built once and cloned for each iteration.
//!
//! The dkgdb rows come from polynomials dealt with `generate_contributions`. The commitment and
//! finalization fixtures check that their step takes the full path: a commitment built, a
//! commitment relayed after the full check, one final with every member a signer.
//!
//! All run under the basic scheme on the calling thread, `SerialRunner` standing in for Core's
//! `CBLSWorker` pool.

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::hint::black_box;
use std::sync::{Arc, LazyLock};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use dash_consensus::primitives::{OutPoint, TxHash};
use dash_evo::llmq::{LlmqCommitment, MnEntryView, OperatorKeyView};
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::bls::{
    BlsCtx, BlsIesMultiBlobs, BlsPkValue, BlsSchemeKind, BlsSigValue, BlsSkValue, decrypt_share,
    generate_contributions, make_new_key, member_id, secret_to_public,
};
use dash_llmq::dkg::rng::CoreRng;
use dash_llmq::dkg::session::{
    BatchOutcome, DkgDebugSink, DkgEnv, DkgMsgKind, DkgSession, DkgStore, FinalVerifier,
    MemberStatusBit, MissingVvec, OperatorKey, QuorumBase, RelayTopology, Sender, SerialRunner,
    SessionInit, SessionStatusBit, VerifiedContributions,
};
use dash_llmq::dkg::sim::SimErrorRates;
use dash_llmq::dkg::wire::{PrematureCommitment, QuorumHash};
use dash_params::{LlmqParams, LlmqType};
use dash_pkc::bls::{BlsScIetf, BlsSecretKey, BlsSigId};
use rand_core::{TryCryptoRng, TryRng};

const BLS: BlsCtx = BlsCtx::BASIC;
/// `VerifyPendingContributions`' trigger in `ReceiveMessage`.
const PENDING: usize = 32;

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0
    }
}

/// The deterministic stand-in for the system RNG. It is not cryptographically secure and exists
/// only for this bench.
struct DetRng(Lcg);

impl TryRng for DetRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        Ok((self.0.next() >> 32) as u32)
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        Ok(self.0.next())
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        for chunk in dst.chunks_mut(8) {
            let word = self.0.next().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
        Ok(())
    }
}

impl TryCryptoRng for DetRng {}

fn rng(seed: u64) -> CoreRng<DetRng> {
    CoreRng(DetRng(Lcg(seed)))
}

#[derive(Clone)]
struct Member {
    pro_tx_hash: ProTxHash,
    collateral: OutPoint,
    operator_pk: BlsPkValue,
}

impl MnEntryView for Member {
    fn pro_tx_hash(&self) -> &ProTxHash {
        &self.pro_tx_hash
    }
    fn collateral(&self) -> &OutPoint {
        &self.collateral
    }
    fn is_evo(&self) -> bool {
        false
    }
    fn is_banned(&self) -> bool {
        false
    }
    fn confirmed_hash_is_null(&self) -> bool {
        false
    }
    fn confirmed_hash_with_protx(&self) -> &[u8; 32] {
        self.pro_tx_hash.as_bytes()
    }
}

impl OperatorKeyView for Member {
    fn operator_key(&self) -> BlsPkValue {
        self.operator_pk.clone()
    }
}

/// The operator key: AM1's `Sign` (through dash-pkc's typed API, basic scheme) and `Decrypt`.
struct Key(BlsSkValue);

impl OperatorKey for Key {
    fn public_key(&self) -> BlsPkValue {
        secret_to_public(&self.0)
    }

    fn sign(&self, hash: &[u8; 32], _legacy: bool) -> BlsSigValue {
        let key = BlsSecretKey::<BlsScIetf>::from_bytes(&self.0.to_bytes(BlsSchemeKind::Basic))
            .expect("a valid key");
        let sig = key.sign_with(hash, BlsSigId::Basic).to_bytes();
        BlsSigValue::from_canonical(&sig, BlsSchemeKind::Basic).expect("a canonical signature")
    }

    fn decrypt_share(
        &self,
        blobs: &BlsIesMultiBlobs,
        idx: usize,
        flag: BlsSchemeKind,
    ) -> Option<BlsSkValue> {
        decrypt_share(blobs, idx, &self.0, flag)
    }
}

/// A dkgdb and a debug status that keep nothing, and a final `Verify` that accepts.
struct Discard;

impl DkgStore for Discard {
    fn write_vvec(&mut self, _: LlmqType, _: &QuorumHash, _: &ProTxHash, vvec: &[BlsPkValue]) {
        black_box(vvec);
    }
    fn write_sk(&mut self, _: LlmqType, _: &QuorumHash, _: &ProTxHash, sk: &BlsSkValue) {
        black_box(sk);
    }
    fn write_encrypted(
        &mut self,
        _: LlmqType,
        _: &QuorumHash,
        _: &ProTxHash,
        blobs: &BlsIesMultiBlobs,
    ) {
        black_box(blobs);
    }
    fn verified_contributions(
        &mut self,
        _: LlmqType,
        _: &QuorumHash,
        _: &[bool],
    ) -> Result<VerifiedContributions, MissingVvec> {
        Ok(VerifiedContributions::default())
    }
}

impl FinalVerifier for Discard {
    fn verify(&mut self, _: &QuorumHash, qc: &LlmqCommitment) -> bool {
        black_box(qc);
        true
    }
}

/// A dkgdb that keeps nothing written and answers its one `GetVerifiedContributions` with the
/// rows it holds.
struct Rows(Option<VerifiedContributions>);

impl DkgStore for Rows {
    fn write_vvec(&mut self, t: LlmqType, base: &QuorumHash, who: &ProTxHash, vvec: &[BlsPkValue]) {
        Discard.write_vvec(t, base, who, vvec);
    }
    fn write_sk(&mut self, t: LlmqType, base: &QuorumHash, who: &ProTxHash, sk: &BlsSkValue) {
        Discard.write_sk(t, base, who, sk);
    }
    fn write_encrypted(
        &mut self,
        t: LlmqType,
        base: &QuorumHash,
        who: &ProTxHash,
        blobs: &BlsIesMultiBlobs,
    ) {
        Discard.write_encrypted(t, base, who, blobs);
    }
    fn verified_contributions(
        &mut self,
        _: LlmqType,
        _: &QuorumHash,
        _: &[bool],
    ) -> Result<VerifiedContributions, MissingVvec> {
        Ok(self.0.take().expect("one read"))
    }
}

impl DkgDebugSink for Discard {
    fn init_local_session_status(&mut self, _: LlmqType, _: i32, _: &QuorumHash, _: i32) {}
    fn update_local_session_status(&mut self, _: LlmqType, _: i32, _: SessionStatusBit) {}
    fn update_local_member_status(&mut self, _: LlmqType, _: i32, _: usize, _: MemberStatusBit) {}
    fn add_local_member_complaint(&mut self, _: LlmqType, _: i32, _: usize, _: usize) {}
}

impl RelayTopology for Discard {
    fn relay_members(&mut self, _: LlmqType, _: &QuorumHash, _: &ProTxHash) -> BTreeSet<ProTxHash> {
        BTreeSet::new()
    }
}

/// A full quorum of type `t` with fresh operator keys.
struct Quorum {
    llmq_type: LlmqType,
    params: LlmqParams,
    members: Arc<[Member]>,
    keys: Vec<Key>,
}

impl Quorum {
    fn new(t: LlmqType) -> Self {
        // Core: llmq/params.h:138 (available_llmqs)
        let params = dash_params::available_params(t).expect("a known type");
        let n = usize::from(params.size);
        let mut lcg = Lcg(0xd6c0 + n as u64);
        let mut draw = DetRng(Lcg(0x6b65 + n as u64));
        let keys: Vec<Key> = (0..n).map(|_| Key(make_new_key(&mut draw))).collect();
        let members = keys
            .iter()
            .map(|key| {
                let mut pro_tx = [0u8; 32];
                for chunk in pro_tx.chunks_mut(8) {
                    chunk.copy_from_slice(&lcg.next().to_le_bytes());
                }
                Member {
                    pro_tx_hash: ProTxHash::from(pro_tx),
                    collateral: OutPoint::new(TxHash::from(pro_tx), 0),
                    operator_pk: secret_to_public(&key.0),
                }
            })
            .collect();
        Self {
            llmq_type: t,
            params,
            members,
            keys,
        }
    }

    /// Member `me`'s fresh session.
    fn session(&self, me: usize) -> DkgSession<Member> {
        let init = SessionInit {
            llmq_type: self.llmq_type,
            params: &self.params,
            base: QuorumBase {
                hash: QuorumHash::from([0x42; 32]),
                height: 48,
            },
            quorum_index: 0,
            members: self.members.clone(),
            active_pro_tx: Some(self.members[me].pro_tx_hash),
            rotation_enabled: false,
            v19_after_base: true,
        };
        DkgSession::init(init, &mut Discard, &mut Discard).expect("a full quorum")
    }
}

/// One step of member `me`'s session in its environment.
fn step<T>(
    q: &Quorum,
    me: usize,
    rng: &mut CoreRng<DetRng>,
    f: impl FnOnce(&mut DkgEnv<'_>) -> T,
) -> T {
    step_in(q, me, rng, &mut Discard, f)
}

/// [`step`] with `store` as the dkgdb.
fn step_in<T>(
    q: &Quorum,
    me: usize,
    rng: &mut CoreRng<DetRng>,
    store: &mut dyn DkgStore,
    f: impl FnOnce(&mut DkgEnv<'_>) -> T,
) -> T {
    let (mut debug, sim) = (Discard, SimErrorRates::default());
    let mut verify = Discard;
    let mut env = DkgEnv {
        bls: BLS,
        rng,
        key: &q.keys[me],
        store,
        debug: &mut debug,
        sim: &sim,
        runner: &SerialRunner,
        verify: &mut verify,
    };
    f(&mut env)
}

/// A quorum, the payloads of members 1 to 32's contributions, and member 0's session primed
/// with the first 31, which each iteration clones.
struct Pending {
    quorum: Quorum,
    payloads: Vec<Vec<u8>>,
    primed: DkgSession<Member>,
}

impl Pending {
    fn new(t: LlmqType) -> Self {
        let quorum = Quorum::new(t);
        let payloads: Vec<Vec<u8>> = (1..=PENDING)
            .map(|i| {
                let mut session = quorum.session(i);
                let qc = step(&quorum, i, &mut rng(i as u64), |env| session.contribute(env));
                qc.expect("first call").expect("a contribution").payload(BLS.scheme())
            })
            .collect();
        let primed = primed(&quorum, &payloads);
        let pending = Self {
            quorum,
            payloads,
            primed,
        };
        if cfg!(debug_assertions) {
            // The measured receive runs the 32-entry verify.
            let mut session = pending.primed.clone();
            let outcome = step(&pending.quorum, 0, &mut rng(0), |env| {
                session.process_batch(DkgMsgKind::Contribution, &pending.last(), env)
            });
            let verified = format!("verified {PENDING} pending contributions");
            assert!(outcome.events.iter().any(|e| e.text == verified));
        }
        pending
    }

    /// The 32nd contribution, from member 32.
    fn last(&self) -> [(Sender<usize>, &[u8]); 1] {
        [(Sender::Peer(PENDING), &self.payloads[PENDING - 1][..])]
    }
}

/// Member 0's session after the first 31 contributions, in pops of 8: 31 pending shares.
fn primed(q: &Quorum, payloads: &[Vec<u8>]) -> DkgSession<Member> {
    let mut session = q.session(0);
    let senders: Vec<(Sender<usize>, &[u8])> =
        (0..PENDING - 1).map(|i| (Sender::Peer(i + 1), &payloads[i][..])).collect();
    for pop in senders.chunks(8) {
        let outcome = step(q, 0, &mut rng(0), |env| {
            session.process_batch(DkgMsgKind::Contribution, pop, env)
        });
        assert_eq!(outcome.relay.len(), pop.len());
    }
    session.take_events();
    session
}

/// `count` polynomials of `threshold` LCG keys, each dealt with `generate_contributions` to
/// `ids`: its verification vector and its shares, in `ids` order.
fn deal(
    count: usize,
    threshold: u16,
    ids: &[[u8; 32]],
    seed: u64,
) -> Vec<(Vec<BlsPkValue>, Vec<BlsSkValue>)> {
    let mut draw = DetRng(Lcg(seed));
    (0..count)
        .map(|_| {
            let coefficients: Vec<BlsSkValue> =
                (0..threshold).map(|_| make_new_key(&mut draw)).collect();
            generate_contributions(&coefficients, ids).expect("a polynomial")
        })
        .collect()
}

/// Member `me`'s `verify_and_commit` on a fresh session over the dkgdb `rows`: the premature
/// commitment, which the fixture checks was built.
fn premature_commitment(q: &Quorum, me: usize, mut rows: Rows) -> PrematureCommitment {
    let mut session = q.session(me);
    let qc = step_in(q, me, &mut rng(0), &mut rows, |env| session.verify_and_commit(env));
    assert!(session.take_events().iter().any(|e| e.text == "built premature commitment"));
    qc.expect("a premature commitment")
}

/// Member 0's receive of the premature commitment `payload` from `sender`, over the dkgdb
/// `rows`.
fn receive(
    q: &Quorum,
    session: &mut DkgSession<Member>,
    sender: Sender<usize>,
    payload: &[u8],
    rows: &mut Rows,
) -> BatchOutcome<usize> {
    let msg = [(sender, payload)];
    step_in(q, 0, &mut rng(0), rows, |env| {
        session.process_batch(DkgMsgKind::PrematureCommitment, &msg, env)
    })
}

/// [`receive`], which the fixture checks ran every check and accepted.
fn receive_checked(
    q: &Quorum,
    session: &mut DkgSession<Member>,
    sender: Sender<usize>,
    payload: &[u8],
    mut rows: Rows,
) {
    let outcome = receive(q, session, sender, payload, &mut rows);
    assert_eq!(outcome.relay.len(), 1);
    assert!(!outcome.events.iter().any(|e| e.text.contains("skipping full verification")));
}

/// A quorum whose members each dealt a polynomial, with the dkgdb rows the contribution phase
/// leaves members 0 and 1: every member's verification vector and its share for them.
struct Dealt {
    quorum: Quorum,
    vvecs: Vec<Vec<BlsPkValue>>,
    /// `shares[i][j]`: member `j`'s polynomial at member `i`'s id.
    shares: [Vec<BlsSkValue>; 2],
}

impl Dealt {
    fn new(t: LlmqType) -> Self {
        let quorum = Quorum::new(t);
        let n = quorum.members.len();
        let ids = [0, 1].map(|i| member_id(&quorum.members[i].pro_tx_hash));
        let mut shares = [Vec::with_capacity(n), Vec::with_capacity(n)];
        let vvecs = deal(n, quorum.params.threshold, &ids, 0x636f + n as u64)
            .into_iter()
            .map(|(vvec, dealt)| {
                for (row, share) in shares.iter_mut().zip(dealt) {
                    row.push(share);
                }
                vvec
            })
            .collect();
        Self {
            quorum,
            vvecs,
            shares,
        }
    }

    /// Member `me`'s dkgdb, for member 0 or 1.
    fn rows(&self, me: usize) -> Rows {
        Rows(Some(VerifiedContributions {
            member_indexes: (0..self.vvecs.len() as u16).collect(),
            vvecs: self.vvecs.clone(),
            sk_contributions: self.shares[me].clone(),
        }))
    }
}

/// Member 1's premature commitment, which member 0 receives.
struct Received {
    dealt: &'static Dealt,
    payload: Vec<u8>,
}

impl Received {
    fn new(dealt: &'static Dealt) -> Self {
        let q = &dealt.quorum;
        let payload = premature_commitment(q, 1, dealt.rows(1)).payload(BLS.scheme());
        // The measured receive runs every check and accepts.
        receive_checked(q, &mut q.session(0), Sender::Peer(1), &payload, dealt.rows(0));
        Self {
            dealt,
            payload,
        }
    }
}

/// Member 0's session after every member's premature commitment, each received, checked and
/// accepted.
///
/// Every dkgdb holds one row: the quorum's polynomial, whose vector and shares are what
/// `SendCommitment` and `ReceiveMessage` build from all members' rows (the vectors' sum and the
/// shares' sum). The commitments are those of an honest quorum with that polynomial, without a
/// polynomial dealt per member.
struct Finalizing {
    quorum: Quorum,
    primed: DkgSession<Member>,
}

impl Finalizing {
    fn new(t: LlmqType) -> Self {
        let quorum = Quorum::new(t);
        let q = &quorum;
        let n = q.members.len();
        let ids: Vec<[u8; 32]> = q.members.iter().map(|m| member_id(&m.pro_tx_hash)).collect();
        let (vvec, shares) = deal(1, q.params.threshold, &ids, 0x6669 + n as u64).remove(0);
        let rows = |me: usize| {
            Rows(Some(VerifiedContributions {
                member_indexes: vec![0],
                vvecs: vec![vvec.clone()],
                sk_contributions: vec![shares[me].clone()],
            }))
        };
        let mut primed = q.session(0);
        for i in 0..n {
            let payload = premature_commitment(q, i, rows(i)).payload(BLS.scheme());
            let sender = if i == 0 {
                Sender::Own
            } else {
                Sender::Peer(i)
            };
            receive_checked(q, &mut primed, sender, &payload, rows(0));
        }
        primed.take_events();
        // The measured finalize builds one final with every member a signer.
        let mut session = primed.clone();
        let finals = step(q, 0, &mut rng(0), |env| session.finalize_commitments(env));
        assert_eq!(finals.len(), 1);
        let line = format!("validMembers={n}, signers={n},");
        assert!(session.take_events().iter().any(|e| e.text.contains(&line)));
        Self {
            quorum,
            primed,
        }
    }
}

static DEALT_400: LazyLock<Dealt> = LazyLock::new(|| Dealt::new(LlmqType::Llmq400_60));
/// [`DEALT_400`], once member 0's measured commit is checked to build and sign.
static COMMIT_400: LazyLock<&Dealt> = LazyLock::new(|| {
    let d: &Dealt = &DEALT_400;
    premature_commitment(&d.quorum, 0, d.rows(0));
    d
});
static RECEIVED_400: LazyLock<Received> = LazyLock::new(|| Received::new(&DEALT_400));
static FINALIZING_50: LazyLock<Finalizing> = LazyLock::new(|| Finalizing::new(LlmqType::Llmq50_60));
static FINALIZING_400: LazyLock<Finalizing> =
    LazyLock::new(|| Finalizing::new(LlmqType::Llmq400_60));
static CONTRIBUTE_50: LazyLock<Quorum> = LazyLock::new(|| Quorum::new(LlmqType::Llmq50_60));
static CONTRIBUTE_400: LazyLock<Quorum> = LazyLock::new(|| Quorum::new(LlmqType::Llmq400_60));
static PENDING_50: LazyLock<Pending> = LazyLock::new(|| Pending::new(LlmqType::Llmq50_60));
static PENDING_400: LazyLock<Pending> = LazyLock::new(|| Pending::new(LlmqType::Llmq400_60));

fn dkg(c: &mut Criterion) {
    let mut group = c.benchmark_group("dkg");
    group.sample_size(10);

    for (name, quorum) in [("50_60", &CONTRIBUTE_50), ("400_60", &CONTRIBUTE_400)] {
        group.bench_function(format!("contribute/{name}"), |b| {
            let q: &Quorum = quorum;
            b.iter_batched_ref(
                || (q.session(0), rng(0x5eed)),
                |(session, rng)| step(q, 0, rng, |env| session.contribute(env)),
                BatchSize::PerIteration,
            );
        });
    }

    for (name, pending) in [("50_60", &PENDING_50), ("400_60", &PENDING_400)] {
        group.bench_function(format!("verify_pending/{name}"), |b| {
            let p: &Pending = pending;
            let last = p.last();
            b.iter_batched_ref(
                || (p.primed.clone(), rng(0)),
                |(session, rng)| {
                    step(&p.quorum, 0, rng, |env| {
                        session.process_batch(DkgMsgKind::Contribution, &last, env)
                    })
                },
                BatchSize::PerIteration,
            );
        });
    }

    group.bench_function("commit/400_60", |b| {
        let d: &Dealt = &COMMIT_400;
        b.iter_batched_ref(
            || (d.quorum.session(0), d.rows(0), rng(0)),
            |(session, rows, rng)| {
                step_in(&d.quorum, 0, rng, rows, |env| session.verify_and_commit(env))
            },
            BatchSize::PerIteration,
        );
    });

    group.bench_function("pcommit_verify/400_60", |b| {
        let r: &Received = &RECEIVED_400;
        b.iter_batched_ref(
            || (r.dealt.quorum.session(0), r.dealt.rows(0)),
            |(session, rows)| receive(&r.dealt.quorum, session, Sender::Peer(1), &r.payload, rows),
            BatchSize::PerIteration,
        );
    });

    for (name, finalizing) in [("50_60", &FINALIZING_50), ("400_60", &FINALIZING_400)] {
        group.bench_function(format!("finalize/{name}"), |b| {
            let f: &Finalizing = finalizing;
            b.iter_batched_ref(
                || (f.primed.clone(), rng(0)),
                |(session, rng)| step(&f.quorum, 0, rng, |env| session.finalize_commitments(env)),
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, dkg);
criterion_main!(benches);
