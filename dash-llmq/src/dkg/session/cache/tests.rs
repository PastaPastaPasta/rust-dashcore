//! The session cache (LQ4b-2a T1): `CBLSWorkerCache`'s keys and kept values as
//! `SendCommitment` and `ReceiveMessage(CDKGPrematureCommitment)` reach them. Cites are v24.0.0.

use dash_evo::types::ProTxHash;

use super::*;
use crate::dkg::bls::{
    BlsCtx, BlsIesMultiBlobs, BlsSchemeKind, aggregate_secret_keys,
    build_quorum_verification_vector, sign_stand_in,
};
use crate::dkg::reference::{MemStore, StoreRead, TestMember};
use crate::dkg::session::fixture::{BLS, Round, TestEnv, base, lines, member_session};
use crate::dkg::session::{
    BatchOutcome, DkgEnv, DkgMsgKind, DkgSession, DkgStore, MissingVvec, OperatorKey, Sender,
    VerifiedContributions,
};
use crate::dkg::wire::{LlmqType, PrematureCommitment, QuorumHash, vvec_hash};

const N: usize = 4;
const F: &str = "ReceiveMessage";
const SKIPPED: &str = "failed to build quorum verification vector. skipping full verification";
/// Every member valid but the last.
const THREE: [bool; N] = [true, true, true, false];

/// Each contributor's verification vector and its true share for each member.
#[derive(Clone)]
struct Rows {
    vvecs: Vec<Vec<BlsPkValue>>,
    /// `shares[c][i]`: contributor `c`'s share for member `i`.
    shares: Vec<Vec<BlsSkValue>>,
}

impl Rows {
    fn of(round: &Round) -> Self {
        Self {
            vvecs: round.contributions.iter().map(|c| c.vvec().to_vec()).collect(),
            shares: round.shares.clone(),
        }
    }

    /// The rows with contributor `c`'s contribution dealt again from `seed`.
    fn redealt(&self, round: &Round, c: usize, seed: u64) -> Self {
        let mut session = member_session(&round.params, &round.members, Some(c), true);
        let mut env = TestEnv::new(&round.keys[c], seed);
        let qc = session.contribute(&mut env.env()).expect("first call").expect("a contribution");
        let mut rows = self.clone();
        rows.vvecs[c] = qc.vvec().to_vec();
        rows.shares[c] = session.own_shares().to_vec();
        assert_ne!(rows.vvecs[c], self.vvecs[c]);
        rows
    }

    /// The quorum verification vector of the contributors in `valid`, under `flag`.
    fn quorum_vvec(&self, valid: &[bool], flag: BlsSchemeKind) -> Vec<BlsPkValue> {
        let vvecs: Vec<&[BlsPkValue]> =
            self.vvecs.iter().zip(valid).filter(|(_, v)| **v).map(|(r, _)| &r[..]).collect();
        build_quorum_verification_vector(&vvecs, flag).expect("honest vectors")
    }

    /// Member `me`'s rows in a [`RowStore`] over `round`'s members.
    fn store(&self, round: &Round, me: usize) -> RowStore {
        RowStore {
            members: round.pro_txs(),
            vvecs: self.vvecs.iter().cloned().map(Some).collect(),
            sks: self.shares.iter().map(|s| s[me].clone()).collect(),
            calls: 0,
        }
    }

    /// Member `me`'s rows written to a [`MemStore`] over `round`'s members.
    fn mem_store(&self, round: &Round, me: usize) -> MemStore {
        let mut store = MemStore::with_members(round.pro_txs());
        for (c, who) in round.pro_txs().iter().enumerate() {
            store.write_vvec(LlmqType::LlmqTest, &base().hash, who, &self.vvecs[c]);
            store.write_sk(LlmqType::LlmqTest, &base().hash, who, &self.shares[c][me]);
        }
        store
    }

    /// Member `from`'s premature commitment over every member valid, as these rows give it.
    fn pcommit(&self, round: &Round, from: usize) -> PrematureCommitment {
        self.pcommit_over(round, from, &[true; N])
    }

    /// Member `from`'s premature commitment over the members in `valid`.
    fn pcommit_over(&self, round: &Round, from: usize, valid: &[bool]) -> PrematureCommitment {
        let flag = BLS.scheme();
        let hash = vvec_hash(&self.quorum_vvec(valid, flag), flag);
        self.pcommit_with(round, from, valid, &hash)
    }

    /// [`Self::pcommit_over`] carrying `hash` as its `quorumVvecHash`.
    fn pcommit_with(
        &self,
        round: &Round,
        from: usize,
        valid: &[bool],
        hash: &[u8; 32],
    ) -> PrematureCommitment {
        let flag = BLS.scheme();
        let qc = PrematureCommitment::unsigned(
            LlmqType::LlmqTest,
            &base().hash,
            &round.members[from].pro_tx_hash,
            valid,
            &self.quorum_vvec(valid, flag)[0],
            hash,
            flag,
        );
        let shares: Vec<&BlsSkValue> =
            self.shares.iter().zip(valid).filter(|(_, v)| **v).map(|(s, _)| &s[from]).collect();
        let sk_share = aggregate_secret_keys(&shares, flag);
        let quorum_sig = sign_stand_in(&sk_share, qc.sign_hash(), false);
        let sig = round.keys[from].sign(qc.sign_hash(), false);
        qc.with_quorum_sig(quorum_sig, flag).with_sig(sig, flag)
    }

    /// [`Self::pcommit`] with a `quorumSig` that decodes but does not verify: the operator's
    /// signature.
    fn pcommit_unverifiable(&self, round: &Round, from: usize) -> PrematureCommitment {
        let qc = self.pcommit(round, from);
        let sig = round.keys[from].sign(qc.sign_hash(), false);
        qc.with_quorum_sig(sig, BLS.scheme())
    }
}

/// A dkgdb stand-in without `contributionsCache`: each call reads the rows as they are now.
struct RowStore {
    members: Vec<ProTxHash>,
    /// Each member's `qdkg_V` row, `None` where there is none.
    vvecs: Vec<Option<Vec<BlsPkValue>>>,
    /// Each member's `qdkg_S` row.
    sks: Vec<BlsSkValue>,
    /// The `verified_contributions` calls made.
    calls: usize,
}

impl DkgStore for RowStore {
    fn write_vvec(&mut self, _: LlmqType, _: &QuorumHash, _: &ProTxHash, _: &[BlsPkValue]) {}

    fn write_sk(&mut self, _: LlmqType, _: &QuorumHash, _: &ProTxHash, _: &BlsSkValue) {}

    fn write_encrypted(
        &mut self,
        _: LlmqType,
        _: &QuorumHash,
        _: &ProTxHash,
        _: &BlsIesMultiBlobs,
    ) {
    }

    fn verified_contributions(
        &mut self,
        _: LlmqType,
        _: &QuorumHash,
        valid: &[bool],
    ) -> Result<VerifiedContributions, MissingVvec> {
        self.calls += 1;
        let mut out = VerifiedContributions::default();
        for (i, who) in self.members.iter().enumerate() {
            if !valid.get(i).copied().unwrap_or(false) {
                continue;
            }
            let vvec = self.vvecs[i].clone().ok_or(MissingVvec {
                who: *who,
            })?;
            out.member_indexes.push(i as u16);
            out.vvecs.push(vvec);
            out.sk_contributions.push(self.sks[i].clone());
        }
        Ok(out)
    }
}

/// `qc` from `from` into the session alone in its batch, under `bls`, through `store`.
fn receive(
    session: &mut DkgSession<TestMember>,
    env: &mut TestEnv,
    store: &mut dyn DkgStore,
    bls: BlsCtx,
    from: usize,
    qc: &PrematureCommitment,
) -> BatchOutcome<usize> {
    let raw = qc.payload(bls.scheme());
    let batch = [(Sender::Peer(from), &raw[..])];
    session.process_batch(
        DkgMsgKind::PrematureCommitment,
        &batch,
        &mut DkgEnv {
            bls,
            store,
            ..env.env()
        },
    )
}

/// `verify_and_commit` under `bls`, through `store`.
fn commit(
    session: &mut DkgSession<TestMember>,
    env: &mut TestEnv,
    store: &mut dyn DkgStore,
    bls: BlsCtx,
) -> Option<PrematureCommitment> {
    session.verify_and_commit(&mut DkgEnv {
        bls,
        store,
        ..env.env()
    })
}

/// Whether the batch relayed `qc` and nothing else, scoring nobody.
fn relayed(out: &BatchOutcome<usize>, qc: &PrematureCommitment) -> bool {
    assert!(out.misbehaving.is_empty(), "{:?}", out.misbehaving);
    let hashes: Vec<[u8; 32]> = out.relay.iter().map(|r| r.inv.hash).collect();
    hashes == [*qc.session_hash()]
}

/// Whether the batch relayed nothing and scored nobody.
fn dropped(out: &BatchOutcome<usize>) -> bool {
    out.relay.is_empty() && out.misbehaving.is_empty()
}

/// The receive's line after the "received" one.
fn verdict(out: &BatchOutcome<usize>) -> String {
    lines(&out.events).get(1).cloned().unwrap_or_default()
}

/// Member 0's session, with every member valid, and its environment.
fn receiver(round: &Round) -> (DkgSession<TestMember>, TestEnv) {
    let session = member_session(&round.params, &round.members, Some(0), true);
    (session, TestEnv::new(&round.keys[0], 0x5e5))
}

#[test]
fn a_kept_vector_is_used_after_the_rows_change_under_its_key() {
    // Fail-first (a): the key holds no input contents. The stand-in store has no
    // `contributionsCache`, so a changed row reaches the second call; the vector built for the
    // first is used. Core's store answers a member's later reads with its first
    // (`l/dkgsessionmgr.cpp:157-187`) and gets one row per member (`l/dkgsession.cpp:219-236`,
    // :734-759).
    let round = Round::new(N);
    let first = Rows::of(&round);
    let (mut session, mut env) = receiver(&round);
    let mut store = first.store(&round, 0);
    let qc = first.pcommit(&round, 1);
    assert!(relayed(&receive(&mut session, &mut env, &mut store, BLS, 1, &qc), &qc));

    let changed = first.redealt(&round, 2, 0xd1ff);
    store.vvecs[2] = Some(changed.vvecs[2].clone());
    let old = first.pcommit(&round, 3);
    let out = receive(&mut session, &mut env, &mut store, BLS, 3, &old);
    assert!(relayed(&out, &old));
    assert!(verdict(&out).ends_with("verified premature commitment. received=2/4"), "{out:?}");
    let new = changed.pcommit(&round, 2);
    let out = receive(&mut session, &mut env, &mut store, BLS, 2, &new);
    assert!(dropped(&out));
    assert_eq!(verdict(&out), format!("{F} -- calculated quorum public key does not match"));
    assert_eq!(store.calls, 3, "the store is read on every receive");
    assert_eq!(session.cache.entries()[0], 1);
}

#[test]
fn a_vector_that_did_not_build_is_kept_for_its_key() {
    // Fail-first (b): the second receive skips full verification although the rows now
    // build. Member 2's row shares a coefficient with member 1's, which
    // `VerifyVerificationVectors` refuses (`bls/bls_worker.cpp:783-805`).
    let round = Round::new(N);
    let rows = Rows::of(&round);
    let (mut session, mut env) = receiver(&round);
    let mut store = rows.store(&round, 0);
    let mut shared = rows.vvecs[2].clone();
    shared[1] = rows.vvecs[1][1].clone();
    store.vvecs[2] = Some(shared);
    let qc = rows.pcommit(&round, 1);
    let out = receive(&mut session, &mut env, &mut store, BLS, 1, &qc);
    assert!(relayed(&out, &qc));
    assert_eq!(verdict(&out), format!("{F} -- {SKIPPED}"));

    store.vvecs[2] = Some(rows.vvecs[2].clone());
    let unverifiable = rows.pcommit_unverifiable(&round, 3);
    let out = receive(&mut session, &mut env, &mut store, BLS, 3, &unverifiable);
    assert!(relayed(&out, &unverifiable), "skipped and relayed: {out:?}");
    assert_eq!(verdict(&out), format!("{F} -- {SKIPPED}"));
    assert_eq!(session.cache.entries(), [1, 0, 0]);
}

#[test]
fn the_scheme_is_not_in_the_key() {
    // Fail-first (c): the session commits under the legacy scheme, and the scheme is basic at
    // the next receive. The receive builds nothing and hashes the kept vector under the basic
    // scheme (`l/dkgsession.cpp:606-607`): the legacy hash no longer matches, the basic one
    // does.
    let round = Round::new(N);
    let rows = Rows::of(&round);
    let (mut session, mut env) = receiver(&round);
    let mut store = rows.store(&round, 0);
    let legacy = BlsSchemeKind::Legacy;
    let own = commit(&mut session, &mut env, &mut store, BlsCtx::LEGACY).expect("a commitment");
    let legacy_hash = vvec_hash(&rows.quorum_vvec(&[true; N], legacy), legacy);
    assert_eq!(*own.quorum_vvec_hash(), legacy_hash);
    assert_eq!(session.cache.entries(), [1, 1, 0]);
    session.take_events();

    let basic = BlsCtx::BASIC;
    let stale = rows.pcommit_with(&round, 2, &[true; N], &legacy_hash);
    let out = receive(&mut session, &mut env, &mut store, basic, 2, &stale);
    assert!(dropped(&out));
    assert_eq!(verdict(&out), format!("{F} -- calculated quorum vvec hash does not match"));
    assert_eq!(session.cache.entries(), [1, 1, 0], "no build under the basic scheme");

    let qc = rows.pcommit(&round, 1);
    let out = receive(&mut session, &mut env, &mut store, basic, 1, &qc);
    assert!(relayed(&out, &qc));
    assert!(verdict(&out).ends_with("verified premature commitment. received=2/4"), "{out:?}");
    assert_eq!(session.cache.entries(), [1, 1, 1]);
    assert_eq!(store.calls, 3);
}

#[test]
fn one_build_per_key_across_the_commitment_and_its_receives() {
    // The store is read as before: the members access on every call, each member's rows once
    // (`contributionsCache`).
    let round = Round::new(N);
    let rows = Rows::of(&round);
    let (mut session, mut env) = receiver(&round);
    let mut store = rows.mem_store(&round, 0);
    commit(&mut session, &mut env, &mut store, BLS).expect("a commitment");
    let mut want = vec![StoreRead::Members];
    for who in round.pro_txs() {
        want.extend([StoreRead::Vvec(who), StoreRead::Sk(who)]);
    }
    assert_eq!(store.reads, want);
    assert_eq!(session.cache.entries(), [1, 1, 0]);
    for from in 1..N {
        let qc = rows.pcommit(&round, from);
        assert!(relayed(&receive(&mut session, &mut env, &mut store, BLS, from, &qc), &qc));
        want.push(StoreRead::Members);
        assert_eq!(store.reads, want, "{from}");
    }
    assert_eq!(store.member_accesses(), N);
    assert_eq!(session.cache.entries(), [1, 1, N - 1]);
    assert_eq!(session.clone().cache.entries(), [1, 1, N - 1], "a clone keeps the cache");
}

#[test]
fn each_member_index_list_is_its_own_key() {
    // Sender 1 over every member, then over three: two vectors, two public key shares, each
    // verified under its own. A sender's second commitment over the same members reaches its
    // kept share.
    let round = Round::new(N);
    let rows = Rows::of(&round);
    let (mut session, mut env) = receiver(&round);
    session.params.min_size = 3;
    let mut store = rows.store(&round, 0);
    let all = rows.pcommit(&round, 1);
    assert!(relayed(&receive(&mut session, &mut env, &mut store, BLS, 1, &all), &all));
    let three = rows.pcommit_over(&round, 1, &THREE);
    let out = receive(&mut session, &mut env, &mut store, BLS, 1, &three);
    assert!(relayed(&out, &three), "{out:?}");
    assert_eq!(session.cache.entries(), [2, 0, 2]);
    let qc = rows.pcommit(&round, 2);
    assert!(relayed(&receive(&mut session, &mut env, &mut store, BLS, 2, &qc), &qc));
    let second = rows.pcommit_unverifiable(&round, 2);
    let out = receive(&mut session, &mut env, &mut store, BLS, 2, &second);
    assert_eq!(verdict(&out), format!("{F} -- failed to verify quorumSig"));
    assert_eq!(session.cache.entries(), [2, 0, 3]);
}

#[test]
fn the_commitment_and_a_receive_over_other_members_build_apart() {
    // `SendCommitment` over the members not bad keys on them; a receive over every member
    // builds its own vector.
    let round = Round::new(N);
    let rows = Rows::of(&round);
    let (mut session, mut env) = receiver(&round);
    session.params.min_size = 3;
    session.member_state[3].bad = true;
    let mut store = rows.store(&round, 0);
    let own = commit(&mut session, &mut env, &mut store, BLS).expect("a commitment");
    assert_eq!(own.valid_members(), THREE);
    assert_eq!(session.cache.entries(), [1, 1, 0]);
    let qc = rows.pcommit(&round, 1);
    assert!(relayed(&receive(&mut session, &mut env, &mut store, BLS, 1, &qc), &qc));
    assert_eq!(session.cache.entries(), [2, 1, 1]);
}

#[test]
fn the_store_is_read_before_the_cache() {
    // A row missing after its key was kept: the store's refusal comes first and the kept
    // vector is not reached (`l/dkgsession.cpp:588-589`).
    let round = Round::new(N);
    let rows = Rows::of(&round);
    let (mut session, mut env) = receiver(&round);
    let mut store = rows.store(&round, 0);
    let qc = rows.pcommit(&round, 1);
    assert!(relayed(&receive(&mut session, &mut env, &mut store, BLS, 1, &qc), &qc));
    store.vvecs[2] = None;
    let unverifiable = rows.pcommit_unverifiable(&round, 3);
    let out = receive(&mut session, &mut env, &mut store, BLS, 3, &unverifiable);
    assert!(relayed(&out, &unverifiable));
    let p2 = round.members[2].pro_tx_hash;
    let block = base().hash;
    let got = lines(&out.events);
    assert_eq!(
        got[0],
        format!(
            "GetVerifiedContributions -- this node does not have vvec for llmq=100 block={block} \
             protx={p2}"
        )
    );
    assert_eq!(got[2], format!("{F} -- {SKIPPED}"));
    assert_eq!(store.calls, 2);
    assert_eq!(session.cache.entries(), [1, 0, 1]);
}

#[test]
fn without_the_cache_every_call_reads_the_rows_as_they_are() {
    // The cache-off twin of fail-first (a).
    let round = Round::new(N);
    let first = Rows::of(&round);
    let (session, mut env) = receiver(&round);
    let mut session = session.without_cache();
    let mut store = first.store(&round, 0);
    let qc = first.pcommit(&round, 1);
    assert!(relayed(&receive(&mut session, &mut env, &mut store, BLS, 1, &qc), &qc));
    let changed = first.redealt(&round, 2, 0xd1ff);
    store.vvecs[2] = Some(changed.vvecs[2].clone());
    let old = first.pcommit(&round, 3);
    let out = receive(&mut session, &mut env, &mut store, BLS, 3, &old);
    assert_eq!(verdict(&out), format!("{F} -- calculated quorum public key does not match"));
    let new = changed.pcommit(&round, 2);
    assert!(relayed(&receive(&mut session, &mut env, &mut store, BLS, 2, &new), &new));
    assert_eq!(session.cache.entries(), [0, 0, 0]);
}

#[test]
fn failures_are_kept() {
    // `GetOrBuild` keeps every value (`bls/bls_worker.h:179-197`): the null vector and the
    // invalid key, whatever a later build would give.
    let mut cache = SessionCache::default();
    let key = [0, 1, 2];
    assert!(cache.quorum_vvec(&key, || None).is_none());
    assert!(cache.quorum_vvec(&key, || unreachable!("kept")).is_none());
    assert!(!cache.share_sum(&key, BlsSkValue::invalid).is_valid());
    assert!(!cache.share_sum(&key, || unreachable!("kept")).is_valid());
    let id = [7; 32];
    assert!(!cache.pk_share(&key, &id, BlsPkValue::invalid).is_valid());
    assert!(!cache.pk_share(&key, &id, || unreachable!("kept")).is_valid());
    cache.pk_share(&key, &[8; 32], BlsPkValue::invalid);
    assert_eq!(cache.entries(), [1, 1, 2], "a key per id");
}

/// `SerializeHash`'s input for `memberIndexes`: a compact size, then each index little endian.
fn core_bytes(indexes: &[u16]) -> Vec<u8> {
    let mut out = match indexes.len() {
        n @ 0..=252 => vec![n as u8],
        n => [&[0xfd][..], &(n as u16).to_le_bytes()].concat(),
    };
    for i in indexes {
        out.extend(i.to_le_bytes());
    }
    out
}

#[test]
fn two_index_lists_are_one_key_exactly_when_cores_bytes_are_equal() {
    let long: Vec<u16> = (0..300).collect();
    let lists: Vec<Vec<u16>> = vec![
        vec![],
        vec![0],
        vec![1],
        vec![256],
        vec![0, 1],
        vec![1, 0],
        vec![0, 1, 2],
        vec![1, 2, 3],
        long[..253].to_vec(),
        long.clone(),
    ];
    for a in &lists {
        for b in &lists {
            let mut cache = SessionCache::default();
            cache.share_sum(a, BlsSkValue::invalid);
            cache.share_sum(b, BlsSkValue::invalid);
            let one_key = cache.entries()[1] == 1;
            assert_eq!(one_key, core_bytes(a) == core_bytes(b), "{a:?} {b:?}");
        }
    }
}
