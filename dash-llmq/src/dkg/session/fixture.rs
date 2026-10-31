//! Sessions and messages for the session's tests.

use std::sync::{Arc, OnceLock};

use dash_evo::types::ProTxHash;
use dash_params::{LlmqParams, available_params};

use super::{
    DkgBlsRunner, DkgEnv, DkgEvent, DkgMsgKind, DkgSession, InitRefusal, OperatorKey, QuorumBase,
    Sender, SerialRunner, SessionInit,
};
use crate::dkg::bls::{BlsCtx, BlsSchemeKind, BlsSigValue, BlsSkValue, sign_stand_in};
use crate::dkg::reference::{
    CoreRelayTopology, MemStore, RecordingDebug, TestMember, TestOperatorKey, quorum,
};
use crate::dkg::rng::{CoreRng, DkgRng, SeededRng};
use crate::dkg::sim::SimErrorRates;
use crate::dkg::wire::{Complaint, Contribution, Justification, LlmqType, QuorumHash};

pub(crate) const BLS: BlsCtx = BlsCtx::BASIC;

/// Core's static table entry for `t`: the fixture covers every type (`llmq/params.h:138`).
pub(crate) fn params(t: LlmqType) -> LlmqParams {
    available_params(t).expect("a known type")
}

pub(crate) fn base() -> QuorumBase {
    QuorumBase {
        hash: QuorumHash::from([0x42; 32]),
        height: 48,
    }
}

/// `init` over `members` with `me` active, and the topology and debug sink it used.
pub(crate) fn init_with(
    t: LlmqType,
    members: &[TestMember],
    me: Option<ProTxHash>,
) -> (Result<DkgSession<TestMember>, InitRefusal>, CoreRelayTopology, RecordingDebug) {
    init_params(t, &params(t), members, me, true)
}

/// [`init_with`] under `params`, with V19 active after the base or not.
pub(crate) fn init_params(
    t: LlmqType,
    params: &LlmqParams,
    members: &[TestMember],
    me: Option<ProTxHash>,
    v19_after_base: bool,
) -> (Result<DkgSession<TestMember>, InitRefusal>, CoreRelayTopology, RecordingDebug) {
    let mut topo = CoreRelayTopology {
        members: members.iter().map(|m| m.pro_tx_hash).collect(),
        calls: 0,
    };
    let mut debug = RecordingDebug::default();
    let init = SessionInit {
        llmq_type: t,
        params,
        base: base(),
        quorum_index: 0,
        members: Arc::from(members.to_vec()),
        active_pro_tx: me,
        rotation_enabled: false,
        v19_after_base,
    };
    let session = DkgSession::init(init, &mut topo, &mut debug);
    (session, topo, debug)
}

/// A session of `n` fresh members of type `t`, member `me` active, with every member's key.
pub(crate) fn session(
    t: LlmqType,
    n: usize,
    me: Option<usize>,
) -> (DkgSession<TestMember>, Vec<TestMember>, Vec<TestOperatorKey>) {
    let (members, keys) = quorum(n, 0x5eed);
    let active = me.map(|i| members[i].pro_tx_hash);
    let (session, _, _) = init_with(t, &members, active);
    (session.expect("enough members"), members, keys)
}

/// A valid signature's bytes under the basic scheme.
pub(crate) fn valid_sig_bytes() -> [u8; 96] {
    let mut bytes = [0u8; 32];
    bytes[31] = 1;
    let sk = BlsSkValue::from_canonical(&bytes, BlsSchemeKind::Basic).expect("one");
    let sig: BlsSigValue = sign_stand_in(&sk, &[0; 32], false);
    sig.to_bytes(BlsSchemeKind::Basic)
}

/// `LLMQ_TEST` with `n` members, all needed: one LLMQ_TEST session with many members, so the
/// simulated errors apply.
pub(crate) fn test_params(n: usize) -> LlmqParams {
    let n = u16::try_from(n).expect("small");
    LlmqParams {
        size: n,
        min_size: n,
        ..params(LlmqType::LlmqTest)
    }
}

/// The session's environment, each part owned and replaceable.
pub(crate) struct TestEnv {
    pub(crate) rng: Box<dyn DkgRng>,
    pub(crate) key: Box<dyn OperatorKey>,
    pub(crate) store: MemStore,
    pub(crate) debug: RecordingDebug,
    pub(crate) sim: SimErrorRates,
    pub(crate) runner: Box<dyn DkgBlsRunner>,
}

impl TestEnv {
    /// `key`'s environment with randomness from `seed` and no simulated errors.
    pub(crate) fn new(key: &TestOperatorKey, seed: u64) -> Self {
        Self {
            rng: Box::new(CoreRng(SeededRng::new(seed))),
            key: Box::new(key.clone()),
            store: MemStore::default(),
            debug: RecordingDebug::default(),
            sim: SimErrorRates::default(),
            runner: Box::new(SerialRunner),
        }
    }

    pub(crate) fn env(&mut self) -> DkgEnv<'_> {
        DkgEnv {
            bls: BLS,
            rng: &mut *self.rng,
            key: &*self.key,
            store: &mut self.store,
            debug: &mut self.debug,
            sim: &self.sim,
            runner: &*self.runner,
        }
    }
}

/// Member `i`'s honest contribution for each `i` in `from`, each from its own LLMQ_TEST session.
pub(crate) fn contributions(
    params: &LlmqParams,
    members: &[TestMember],
    keys: &[TestOperatorKey],
    from: impl IntoIterator<Item = usize>,
) -> Vec<Contribution> {
    from.into_iter()
        .map(|i| {
            let (session, _, _) = init_params(
                LlmqType::LlmqTest,
                params,
                members,
                Some(members[i].pro_tx_hash),
                true,
            );
            let mut session = session.expect("enough members");
            let mut env = TestEnv::new(&keys[i], 0xc0 + i as u64);
            session.contribute(&mut env.env()).expect("first call").expect("a contribution")
        })
        .collect()
}

/// An LLMQ_TEST session under `params` with member `me` active, its init line taken.
pub(crate) fn member_session(
    params: &LlmqParams,
    members: &[TestMember],
    me: Option<usize>,
    v19_after_base: bool,
) -> DkgSession<TestMember> {
    let me = me.map(|i| members[i].pro_tx_hash);
    let (session, _, _) = init_params(LlmqType::LlmqTest, params, members, me, v19_after_base);
    let mut session = session.expect("enough members");
    session.take_events();
    session
}

/// The 33-member LLMQ_TEST quorum of the M2 rows, with every member's honest contribution.
pub(crate) fn quorum_33() -> &'static (Vec<TestMember>, Vec<TestOperatorKey>, Vec<Contribution>) {
    static Q: OnceLock<(Vec<TestMember>, Vec<TestOperatorKey>, Vec<Contribution>)> =
        OnceLock::new();
    Q.get_or_init(|| {
        let (members, keys) = quorum(33, 0x33);
        let all = contributions(&test_params(33), &members, &keys, 0..33);
        (members, keys, all)
    })
}

/// An LLMQ_TEST quorum of `n` members, each with its honest contribution and true shares.
pub(crate) struct Round {
    pub(crate) params: LlmqParams,
    pub(crate) members: Vec<TestMember>,
    pub(crate) keys: Vec<TestOperatorKey>,
    pub(crate) contributions: Vec<Contribution>,
    /// `shares[c][i]`: contributor `c`'s true share for member `i`.
    pub(crate) shares: Vec<Vec<BlsSkValue>>,
}

impl Round {
    pub(crate) fn new(n: usize) -> Self {
        let params = test_params(n);
        let (members, keys) = quorum(n, 0x3a2);
        let (contributions, shares) = (0..n)
            .map(|i| {
                let mut session = member_session(&params, &members, Some(i), true);
                let mut env = TestEnv::new(&keys[i], 0xc0 + i as u64);
                let qc = session.contribute(&mut env.env()).expect("first call");
                (qc.expect("a contribution"), session.own_shares().to_vec())
            })
            .unzip();
        Self {
            params,
            members,
            keys,
            contributions,
            shares,
        }
    }

    /// Member `me`'s session with its own contribution adopted and the contributions of `from`
    /// received, in one batch, and its environment.
    pub(crate) fn contributed(
        &self,
        me: usize,
        from: impl IntoIterator<Item = usize>,
    ) -> (DkgSession<TestMember>, TestEnv) {
        let mut session = member_session(&self.params, &self.members, Some(me), true);
        let mut env = TestEnv::new(&self.keys[me], 0x5e5);
        let vvec = self.contributions[me].vvec().to_vec();
        session.adopt_contribution(vvec, self.shares[me].clone()).expect("first stage");
        let payloads: Vec<(usize, Vec<u8>)> =
            from.into_iter().map(|c| (c, self.contributions[c].payload(BLS.scheme()))).collect();
        feed(&mut session, &mut env, DkgMsgKind::Contribution, me, &payloads);
        (session, env)
    }

    /// The members' proTxHashes.
    pub(crate) fn pro_txs(&self) -> Vec<ProTxHash> {
        self.members.iter().map(|m| m.pro_tx_hash).collect()
    }

    /// Member `from`'s signed complaint voting `bad` bad and complaining about `complain_for`.
    pub(crate) fn complaint(
        &self,
        from: usize,
        bad: &[usize],
        complain_for: &[usize],
    ) -> Complaint {
        let n = self.members.len();
        let bits = |set: &[usize]| -> Vec<bool> { (0..n).map(|i| set.contains(&i)).collect() };
        let flag = BLS.scheme();
        let pro_tx = &self.members[from].pro_tx_hash;
        let t = LlmqType::LlmqTest;
        let qc =
            Complaint::unsigned(t, &base().hash, pro_tx, &bits(bad), &bits(complain_for), flag);
        let sig = self.keys[from].sign(qc.sign_hash(), false);
        qc.with_sig(sig, flag)
    }

    /// Member `from`'s signed justification revealing `shares`.
    pub(crate) fn justification(&self, from: usize, shares: &[(u32, BlsSkValue)]) -> Justification {
        let flag = BLS.scheme();
        let pro_tx = &self.members[from].pro_tx_hash;
        let qj = Justification::unsigned(LlmqType::LlmqTest, &base().hash, pro_tx, shares, flag);
        let sig = self.keys[from].sign(qj.sign_hash(), false);
        qj.with_sig(sig, flag)
    }
}

/// `msgs` as member `me` receives them: its own as [`Sender::Own`], each other's from its
/// sender.
pub(crate) fn batch_of(me: usize, msgs: &[(usize, Vec<u8>)]) -> Vec<(Sender<usize>, &[u8])> {
    msgs.iter()
        .map(|(from, raw)| {
            let sender = if *from == me {
                Sender::Own
            } else {
                Sender::Peer(*from)
            };
            (sender, &raw[..])
        })
        .collect()
}

/// `msgs` into member `me`'s session in one batch, nobody scored: the lines it logged.
pub(crate) fn feed(
    session: &mut DkgSession<TestMember>,
    env: &mut TestEnv,
    kind: DkgMsgKind,
    me: usize,
    msgs: &[(usize, Vec<u8>)],
) -> Vec<String> {
    let outcome = session.process_batch(kind, &batch_of(me, msgs), &mut env.env());
    assert!(outcome.misbehaving.is_empty(), "{:?}", outcome.misbehaving);
    lines(&outcome.events)
}

/// Each event as it prints.
pub(crate) fn lines(events: &[DkgEvent]) -> Vec<String> {
    events.iter().map(ToString::to_string).collect()
}
