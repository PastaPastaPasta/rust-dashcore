//! Sessions and messages for the session's tests.

use std::sync::{Arc, OnceLock};

use dash_evo::types::ProTxHash;
use dash_params::LlmqParams;

use super::{
    DkgBlsRunner, DkgEnv, DkgSession, InitRefusal, OperatorKey, QuorumBase, SerialRunner,
    SessionInit,
};
use crate::dkg::bls::{BlsCtx, BlsSchemeKind, BlsSigValue, BlsSkValue, sign_stand_in};
use crate::dkg::reference::{
    CoreRelayTopology, MemStore, RecordingDebug, TestMember, TestOperatorKey, quorum,
};
use crate::dkg::rng::{CoreRng, DkgRng, SeededRng};
use crate::dkg::sim::SimErrorRates;
use crate::dkg::wire::{Contribution, LlmqType, QuorumHash};

pub(crate) const BLS: BlsCtx = BlsCtx::BASIC;

pub(crate) fn params(t: LlmqType) -> &'static LlmqParams {
    LlmqParams::available(t).expect("a known type")
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
    init_params(params(t), members, me, true)
}

/// [`init_with`] under `params`, with V19 active after the base or not.
pub(crate) fn init_params(
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
    let n = i32::try_from(n).expect("small");
    LlmqParams {
        size: n,
        min_size: n,
        ..*params(LlmqType::LlmqTest)
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

/// Member `i`'s honest contribution for each `i` in `from`, each from its own session.
pub(crate) fn contributions(
    params: &LlmqParams,
    members: &[TestMember],
    keys: &[TestOperatorKey],
    from: impl IntoIterator<Item = usize>,
) -> Vec<Contribution> {
    from.into_iter()
        .map(|i| {
            let (session, _, _) = init_params(params, members, Some(members[i].pro_tx_hash), true);
            let mut session = session.expect("enough members");
            let mut env = TestEnv::new(&keys[i], 0xc0 + i as u64);
            session.contribute(&mut env.env()).expect("first call").expect("a contribution")
        })
        .collect()
}

/// A session under `params` with member `me` active, its init line taken.
pub(crate) fn member_session(
    params: &LlmqParams,
    members: &[TestMember],
    me: Option<usize>,
    v19_after_base: bool,
) -> DkgSession<TestMember> {
    let me = me.map(|i| members[i].pro_tx_hash);
    let (session, _, _) = init_params(params, members, me, v19_after_base);
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
