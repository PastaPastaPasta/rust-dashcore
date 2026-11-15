//! Outbound sig-share collection, packing and periodic cleanup.
use super::inv::SigSharesInv;
use super::manager::SigSharesManager;
use super::state::{SigSharesNodeState, UNINITIALIZED_SESSION_ID};
use crate::dkg::wire::{BatchedSigShare, BatchedSigShares, SigSesAnn, SigShare};
use crate::sigenv::{
    PeerId, QuorumLookup, RecSigReads, ShareRecovery, SharesEffect, SharesEnv, SigMsg,
    SigSharesNet, SigningQuorum,
};
use dash_params::{LlmqType, MAX_LLMQ_SIZE};
use dash_types::Numeric;
use rand_core::Rng;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const REQUEST_TIMEOUT_S: i64 = 5;
const SESSION_TIMEOUT_S: i64 = 60;
const MAX_REQUESTS_PER_PEER: usize = 32;
const MAX_SES_ANN: usize = 100;
const MAX_INV: usize = 200;
const MAX_INNER: usize = MAX_LLMQ_SIZE as usize;
const MAX_SHARES: usize = 32;
const SEND_BACKOFF_MS: i64 = 2_000;
const MAX_BACKOFF_MS: i64 = 10_000;

fn shuffle<T>(v: &mut [T], rng: &mut impl Rng) {
    for i in (1..v.len()).rev() {
        v.swap(i, (rng.next_u64() % (i as u64 + 1)) as usize);
    }
}

fn ensure_session_id<Q: SigningQuorum>(
    state: &mut SigSharesNodeState<Q>,
    hash: &[u8; 32],
    rng: &mut impl Rng,
) -> Option<(u32, Option<SigSesAnn>)> {
    let used: BTreeSet<u32> = state.sessions.values().map(|s| s.send_session_id).collect();
    let s = state.sessions.get_mut(hash)?;
    let mut assigned = false;
    if s.send_session_id == UNINITIALIZED_SESSION_ID {
        loop {
            let id = rng.next_u32();
            if id != UNINITIALIZED_SESSION_ID && !used.contains(&id) {
                s.send_session_id = id;
                assigned = true;
                break;
            }
        }
    }
    let announcement = assigned.then(|| {
        SigSesAnn::new(
            s.send_session_id,
            s.llmq_type,
            s.quorum_hash,
            s.id.into(),
            s.msg_hash.into(),
        )
    });
    Some((s.send_session_id, announcement))
}

impl<Q: SigningQuorum> SigSharesManager<Q> {
    /// Collect outbound requests, responses, announcements and concentrated sends.
    // Core: v24.0.0 l/signing_shares.cpp:876-1083 CollectSigSharesToRequest, CollectSigSharesToSend,
    // CollectSigSharesToSendConcentrated, CollectSigSharesToAnnounce
    pub fn send_tick<L, R, N, S>(
        &mut self,
        env: &SharesEnv<'_, L, R, N, S>,
        rng: &mut impl Rng,
    ) -> Vec<SharesEffect>
    where
        L: QuorumLookup<Quorum = Q>,
        R: RecSigReads,
        N: SigSharesNet,
        S: ShareRecovery,
    {
        let now = env.now.secs();
        let connected: Vec<_> = env.net.fully_connected();
        let connected_ids: BTreeSet<_> = connected.iter().map(|p| p.id).collect();
        let mut req: BTreeMap<PeerId, BTreeMap<[u8; 32], SigSharesInv>> = BTreeMap::new();
        let mut out: BTreeMap<PeerId, BTreeMap<[u8; 32], BatchedSigShares>> = BTreeMap::new();
        let mut ann: BTreeMap<PeerId, BTreeMap<[u8; 32], SigSharesInv>> = BTreeMap::new();
        let mut direct: BTreeMap<PeerId, Vec<SigShare>> = BTreeMap::new();
        let mut ses: BTreeMap<PeerId, Vec<SigSesAnn>> = BTreeMap::new();

        let mut peers: Vec<_> = self
            .node_states
            .iter()
            .filter_map(|(&p, s)| (!s.sessions.is_empty()).then_some(p))
            .collect();
        shuffle(&mut peers, rng);
        for peer in peers {
            let state = self.node_states.get_mut(&peer).unwrap();
            if state.banned {
                continue;
            }
            state.requested_sig_shares.erase_if(|_, t| now - *t >= REQUEST_TIMEOUT_S);
            let hashes: Vec<_> = state.sessions.keys().copied().collect();
            for hash in hashes {
                let s = state.sessions.get_mut(&hash).unwrap();
                if env.sporks.all_members_connected(s.llmq_type)
                    || env.recsigs.has_for_session(&hash).unwrap_or(false)
                {
                    continue;
                }
                for i in 0..s.announced.inv.len() {
                    if !s.announced.inv[i] {
                        continue;
                    }
                    let key = (hash, i as u16);
                    if self.sig_shares.has(&key) {
                        s.announced.inv[i] = false;
                        continue;
                    }
                    if state.requested_sig_shares.size() >= MAX_REQUESTS_PER_PEER {
                        break;
                    }
                    if let Some((other, t)) = self.sig_shares_requested.get(&key)
                        && (now - *t < REQUEST_TIMEOUT_S || *other == peer)
                    {
                        continue;
                    }
                    state.requested_sig_shares.add(&key, now);
                    self.sig_shares_requested.get_or_add(&key).clone_from(&(peer, now));
                    s.announced.inv[i] = false;
                    let inv =
                        req.entry(peer).or_default().entry(hash).or_insert_with(|| SigSharesInv {
                            session_id: UNINITIALIZED_SESSION_ID,
                            inv: vec![false; s.announced.inv.len()],
                        });
                    inv.set(i as u16, true);
                }
            }
        }

        for (&peer, state) in &mut self.node_states {
            if state.banned {
                continue;
            }
            let hashes: Vec<_> = state.sessions.keys().copied().collect();
            for hash in hashes {
                let s = state.sessions.get_mut(&hash).unwrap();
                if env.sporks.all_members_connected(s.llmq_type)
                    || env.recsigs.has_for_session(&hash).unwrap_or(false)
                {
                    continue;
                }
                let mut v = Vec::new();
                for i in 0..s.requested.inv.len() {
                    if s.requested.inv[i] {
                        s.requested.inv[i] = false;
                        if let Some(sh) = self.sig_shares.get(&(hash, i as u16)) {
                            v.push(BatchedSigShare::new(i as u16, sh.sig_share()));
                        }
                    }
                }
                if !v.is_empty() {
                    out.entry(peer)
                        .or_default()
                        .insert(hash, BatchedSigShares::new(UNINITIALIZED_SESSION_ID, v));
                }
            }
        }

        let mut queued = Vec::new();
        self.sig_shares_queued_to_announce.for_each(|(h, member), _| queued.push((*h, *member)));
        let mut quorum_cache: BTreeMap<(u8, crate::dkg::wire::QuorumHash), Option<Arc<Q>>> =
            BTreeMap::new();
        for (hash, member) in queued {
            let Some(share) = self.sig_shares.get(&(hash, member)).cloned() else {
                continue;
            };
            let quorum_key = (share.llmq_type().to_base(), share.quorum_hash());
            let quorum = quorum_cache.entry(quorum_key).or_insert_with(|| {
                env.lookup.get_quorum(share.llmq_type(), &share.quorum_hash()).ok().flatten()
            });
            let Some(quorum) = quorum.as_ref() else {
                continue;
            };
            let size = usize::from(quorum.params().size);
            for peer in env.net.quorum_nodes(share.llmq_type(), &share.quorum_hash()) {
                let state = self.node_states.entry(peer).or_default();
                if state.banned {
                    continue;
                }
                let s = state.get_or_create_session_from_share(&share, quorum.params());
                s.knows.init(size);
                if s.knows.inv[usize::from(member)] {
                    continue;
                }
                s.knows.set(member, true);
                let inv =
                    ann.entry(peer).or_default().entry(hash).or_insert_with(|| SigSharesInv {
                        session_id: UNINITIALIZED_SESSION_ID,
                        inv: vec![false; size],
                    });
                inv.set(member, true);
            }
        }
        self.sig_shares_queued_to_announce.clear();

        let mut by_protx = BTreeMap::new();
        for p in connected {
            if let Some(x) = p.verified_pro_tx {
                by_protx.entry(x).or_insert(p.id);
            }
        }
        for signed in self.signed_sessions.values_mut() {
            let t = signed.quorum.llmq_type();
            if !env.sporks.all_members_connected(t)
                || env.now.ms < signed.next_attempt_time
                || signed.attempt >= i32::from(signed.quorum.params().recovery_members)
            {
                continue;
            }
            let order = env
                .recovery
                .recovery_member_order(signed.quorum.members(), signed.sig_share.id().as_bytes());
            if order.is_empty() {
                continue;
            }
            for a in signed.attempt..i32::from(signed.quorum.params().recovery_members) {
                let p = &signed.quorum.members()[order[(a as usize) % order.len()]];
                if signed.quorum.is_valid_member(p)
                    && (Some(*p) == env.signer.my_pro_tx || by_protx.contains_key(p))
                {
                    signed.attempt = a;
                    break;
                }
            }
            let shift = signed.send_count.clamp(0, 3) as u32;
            signed.next_attempt_time =
                env.now.ms.saturating_add((SEND_BACKOFF_MS * (1_i64 << shift)).min(MAX_BACKOFF_MS));
            let p = signed.quorum.members()[order[(signed.attempt as usize) % order.len()]];
            signed.attempt += 1;
            signed.send_count += 1;
            if let Some(peer) = by_protx.get(&p) {
                direct.entry(*peer).or_default().push(signed.sig_share.clone());
            }
        }

        for (peer, m) in req.iter_mut() {
            let state = self.node_states.get_mut(peer).unwrap();
            for (hash, x) in m {
                if let Some((id, a)) = ensure_session_id(state, hash, rng) {
                    if let Some(a) = a {
                        ses.entry(*peer).or_default().push(a);
                    }
                    x.session_id = id;
                }
            }
        }
        for (peer, m) in out.iter_mut() {
            let state = self.node_states.get_mut(peer).unwrap();
            for (hash, x) in m {
                if let Some((id, a)) = ensure_session_id(state, hash, rng) {
                    if let Some(a) = a {
                        ses.entry(*peer).or_default().push(a);
                    }
                    let shares = x.sig_shares().to_vec();
                    *x = BatchedSigShares::new(id, shares);
                }
            }
        }
        for (peer, m) in ann.iter_mut() {
            let state = self.node_states.get_mut(peer).unwrap();
            for (hash, x) in m {
                if let Some((id, a)) = ensure_session_id(state, hash, rng) {
                    if let Some(a) = a {
                        ses.entry(*peer).or_default().push(a);
                    }
                    x.session_id = id;
                }
            }
        }
        self.pack(ses, req, out, ann, direct, &connected_ids)
    }

    fn pack(
        &self,
        mut ses: BTreeMap<PeerId, Vec<SigSesAnn>>,
        req: BTreeMap<PeerId, BTreeMap<[u8; 32], SigSharesInv>>,
        out: BTreeMap<PeerId, BTreeMap<[u8; 32], BatchedSigShares>>,
        ann: BTreeMap<PeerId, BTreeMap<[u8; 32], SigSharesInv>>,
        direct: BTreeMap<PeerId, Vec<SigShare>>,
        connected: &BTreeSet<PeerId>,
    ) -> Vec<SharesEffect> {
        let mut result = Vec::new();
        let peers: BTreeSet<_> = ses
            .keys()
            .chain(req.keys())
            .chain(out.keys())
            .chain(ann.keys())
            .chain(direct.keys())
            .filter(|peer| connected.contains(peer))
            .copied()
            .collect();
        for peer in peers {
            let mut msgs = Vec::new();
            if let Some(v) = ses.remove(&peer) {
                for c in v.chunks(MAX_SES_ANN) {
                    msgs.push(SigMsg::SesAnn(c.to_vec()));
                }
            }
            if let Some(v) = req.get(&peer) {
                let x: Vec<_> = v.values().map(SigSharesInv::to_wire).collect();
                for c in x.chunks(MAX_INV) {
                    msgs.push(SigMsg::GetSigShares(c.to_vec()));
                }
            }
            if let Some(v) = out.get(&peer) {
                let mut c = Vec::new();
                let mut n = 0;
                for x in v.values() {
                    let k = x.sig_shares().len();
                    if n + k > MAX_INNER && !c.is_empty() {
                        msgs.push(SigMsg::BSigShares(std::mem::take(&mut c)));
                        n = 0;
                    }
                    n += k;
                    c.push(x.clone());
                }
                if !c.is_empty() {
                    msgs.push(SigMsg::BSigShares(c));
                }
            }
            if let Some(v) = ann.get(&peer) {
                let x: Vec<_> = v.values().map(SigSharesInv::to_wire).collect();
                for c in x.chunks(MAX_INV) {
                    msgs.push(SigMsg::SigSharesInv(c.to_vec()));
                }
            }
            if let Some(v) = direct.get(&peer) {
                for c in v.chunks(MAX_SHARES) {
                    msgs.push(SigMsg::SigShare(c.to_vec()));
                }
            }
            if !msgs.is_empty() {
                result.push(SharesEffect::Send {
                    peer,
                    msgs,
                });
            }
        }
        result
    }

    /// Remove inactive, recovered, expired and disconnected state.
    // Core: v24.0.0 l/signing_shares.cpp:1317-1456 Cleanup
    pub fn cleanup<L, R, N, S>(&mut self, env: &SharesEnv<'_, L, R, N, S>) -> Vec<SharesEffect>
    where
        L: QuorumLookup<Quorum = Q>,
        R: RecSigReads,
        N: SigSharesNet,
        S: ShareRecovery,
    {
        if !self.cleanup_throttler.try_cleanup(env.now.ms, 5_000) {
            return Vec::new();
        }
        let now = env.now.secs();
        let mut remove = BTreeSet::new();
        let mut quorum_cache: BTreeMap<(u8, crate::dkg::wire::QuorumHash), bool> = BTreeMap::new();
        for (&h, &t) in &self.time_seen_for_sessions {
            if now - t >= SESSION_TIMEOUT_S || env.recsigs.has_for_session(&h).unwrap_or(false) {
                remove.insert(h);
            }
        }
        let mut shares = Vec::new();
        self.sig_shares.for_each(|(h, _), sh| {
            shares.push((*h, sh.clone()));
        });
        for (h, sh) in shares {
            let key = (sh.llmq_type().to_base(), sh.quorum_hash());
            let keep = quorum_cache.entry(key).or_insert_with(|| {
                matches!(env.lookup.is_quorum_active(sh.llmq_type(), &sh.quorum_hash()), Ok(true))
                    && matches!(
                        env.lookup.get_quorum(sh.llmq_type(), &sh.quorum_hash()),
                        Ok(Some(_))
                    )
            });
            if !*keep {
                remove.insert(h);
            }
        }
        for h in remove {
            self.remove_session(&h);
        }
        let connected: BTreeSet<_> = env.net.fully_connected().into_iter().map(|p| p.id).collect();
        let peers: Vec<_> =
            self.node_states.keys().copied().filter(|p| !connected.contains(p)).collect();
        for p in peers {
            if let Some(mut state) = self.node_states.remove(&p) {
                state.requested_sig_shares.for_each(|k, _| self.sig_shares_requested.erase(k));
            }
        }
        Vec::new()
    }

    /// Queue all known shares for re-announcement after a resign.
    // Core: v24.0.0 l/signing_shares.cpp:1644-1669 ForceReAnnouncement
    pub fn force_reannouncement(
        &mut self,
        quorum: Arc<Q>,
        t: LlmqType,
        id: [u8; 32],
        msg: [u8; 32],
        sporks: crate::sigenv::SporkFacts,
    ) {
        if sporks.all_members_connected(t) {
            return;
        }
        let h = dash_evo::locks::sign_hash(t, quorum.quorum_hash(), &id, &msg);
        if let Some(sigs) = self.sig_shares.get_all_for_sign_hash(&h) {
            for m in sigs.keys() {
                *self.sig_shares_queued_to_announce.get_or_add(&(h, *m)) = true;
            }
        }
        for s in self.node_states.values_mut() {
            if let Some(x) = s.session_by_sign_hash_mut(&h) {
                x.knows.set_all(false);
                x.send_session_id = UNINITIALIZED_SESSION_ID;
            }
        }
    }

    fn remove_session(&mut self, h: &[u8; 32]) {
        for s in self.node_states.values_mut() {
            s.remove_session(h);
        }
        self.sig_shares.erase_all_for_sign_hash(h);
        self.sig_shares_requested.erase_all_for_sign_hash(h);
        self.sig_shares_queued_to_announce.erase_all_for_sign_hash(h);
        self.time_seen_for_sessions.remove(h);
        self.signed_sessions.remove(h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dkg::bls::{BlsCtx, BlsPkValue, BlsSigValue};
    use crate::dkg::rng::SeededRng;
    use crate::dkg::wire::SigShareBytes;
    use crate::sigenv::fakes::{FakeLookup, FakeNet, FakeQuorum, FakeRecSigs, FakeRecovery};
    use crate::sigenv::{PeerFacts, Recovered, SignerFacts, SporkFacts};
    use crate::sigshares::manager::CleanupThrottler;
    use dash_evo::types::ProTxHash;
    use dash_params::{ChainParamsBuilder, LlmqParams, Network, Release};
    use dash_types::Numeric;
    use std::collections::BTreeMap;

    #[test]
    fn seeded_shuffle_is_repeatable() {
        let mut a = [1, 2, 3, 4, 5];
        let mut b = [1, 2, 3, 4, 5];
        shuffle(&mut a, &mut SeededRng::new(7));
        shuffle(&mut b, &mut SeededRng::new(7));
        assert_eq!(a, b);
    }

    #[test]
    fn cleanup_throttle_is_independent_of_request_expiry() {
        let mut t = CleanupThrottler::default();
        assert!(t.try_cleanup(0, 5_000));
        assert!(!t.try_cleanup(4_999, 5_000));
        assert!(t.try_cleanup(5_000, 5_000));
    }

    const TEST_TYPE: LlmqType = LlmqType::LlmqTest;
    type TestRecovery = FakeRecovery<
        fn(&LlmqParams, &[ProTxHash], &BTreeMap<u16, BlsSigValue>, &mut u32) -> Recovered,
        fn(&[ProTxHash], &[u8; 32]) -> Vec<usize>,
    >;

    fn test_params(size: u16) -> LlmqParams {
        let mut params = *ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
            .build()
            .unwrap()
            .consensus
            .llmq(TEST_TYPE)
            .unwrap();
        params.size = size;
        params.recovery_members = size;
        params
    }

    fn test_quorum(size: u16, hash_byte: u8) -> Arc<FakeQuorum> {
        Arc::new(FakeQuorum {
            llmq_type: TEST_TYPE,
            quorum_hash: crate::dkg::wire::QuorumHash::from([hash_byte; 32]),
            params: test_params(size),
            members: (0..size).map(|i| ProTxHash::from([i as u8 + 1; 32])).collect(),
            valid_members: (0..size).collect(),
            has_verification_vector: false,
            quorum_public_key: BlsPkValue::invalid(),
            pk_shares: BTreeMap::new(),
            sk_share: None,
        })
    }

    fn test_share(q: &FakeQuorum, id_byte: u8, msg_byte: u8, member: u16) -> SigShare {
        SigShare::new(
            q.llmq_type,
            q.quorum_hash,
            member,
            [id_byte; 32].into(),
            [msg_byte; 32].into(),
            SigShareBytes::from_bytes([member as u8 + 1; 96]),
        )
    }

    fn no_recovery(
        _params: &LlmqParams,
        _members: &[ProTxHash],
        _shares: &BTreeMap<u16, BlsSigValue>,
        _counter: &mut u32,
    ) -> Recovered {
        Recovered::NotEnough
    }

    fn member_order(members: &[ProTxHash], _id: &[u8; 32]) -> Vec<usize> {
        (0..members.len()).collect()
    }

    fn test_recovery() -> TestRecovery {
        FakeRecovery {
            recover: no_recovery,
            order: member_order,
        }
    }

    fn test_env<'a>(
        lookup: &'a FakeLookup,
        recsigs: &'a FakeRecSigs,
        net: &'a FakeNet,
        recovery: &'a TestRecovery,
        signer: &'a SignerFacts,
        now_ms: i64,
        spork21_value: i64,
    ) -> SharesEnv<'a, FakeLookup, FakeRecSigs, FakeNet, TestRecovery> {
        SharesEnv {
            lookup,
            recsigs,
            net,
            recovery,
            signer,
            sporks: SporkFacts {
                spork21_active: false,
                spork21_value,
            },
            now: crate::sigenv::Now {
                ms: now_ms,
            },
            bls: BlsCtx::BASIC,
        }
    }

    fn send_messages(effects: &[SharesEffect], peer: PeerId) -> &[SigMsg] {
        effects
            .iter()
            .find_map(|effect| match effect {
                SharesEffect::Send {
                    peer: p,
                    msgs,
                } if *p == peer => Some(msgs.as_slice()),
                _ => None,
            })
            .unwrap_or(&[])
    }

    #[test]
    fn ring_mode_announcement_does_not_require_signed_session() {
        let q = test_quorum(3, 1);
        let share = test_share(&q, 2, 3, 1);
        let share_copy = share.clone();
        let hash = *share.sign_hash().as_bytes();
        let mut manager = SigSharesManager::new();
        manager.sig_shares.add(&(hash, 1), share);
        *manager.sig_shares_queued_to_announce.get_or_add(&(hash, 1)) = true;
        let mut lookup = FakeLookup::default();
        lookup.quorums.insert((TEST_TYPE.to_base(), *q.quorum_hash()), q.clone());
        let net = FakeNet {
            quorum_peers: [((TEST_TYPE.to_base(), *q.quorum_hash()), vec![7, 8, 9])].into(),
            connected: [7, 8, 9]
                .map(|id| PeerFacts {
                    id,
                    verified_pro_tx: None,
                    common_version: 1,
                })
                .to_vec(),
            ..Default::default()
        };
        let recsigs = FakeRecSigs::default();
        let recovery = test_recovery();
        let signer = SignerFacts::default();
        manager
            .node_states
            .entry(9)
            .or_default()
            .get_or_create_session_from_share(&share_copy, q.params())
            .knows
            .set(1, true);
        assert!(manager.signed_sessions.is_empty());
        let env = test_env(&lookup, &recsigs, &net, &recovery, &signer, 0, 2);
        let effects = manager.send_tick(&env, &mut SeededRng::new(7));
        assert!(send_messages(&effects, 9).is_empty());
        for peer in [7, 8] {
            let msgs = send_messages(&effects, peer);
            let inv = msgs.iter().find_map(|m| match m {
                SigMsg::SigSharesInv(v) => v.first(),
                _ => None,
            });
            assert!(inv.is_some_and(|x| x.inv().get(1).unwrap_or(false)));
        }
        assert!(manager.sig_shares_queued_to_announce.is_empty());
    }

    #[test]
    fn two_members_accumulate_in_request_and_announcement_inventories() {
        let q = test_quorum(3, 4);
        let share0 = test_share(&q, 5, 6, 0);
        let share1 = test_share(&q, 5, 6, 1);
        let mut manager = SigSharesManager::new();
        let request_share = test_share(&q, 50, 51, 0);
        let request_hash = *request_share.sign_hash().as_bytes();
        let request_session = manager.node_states.entry(9).or_default();
        let session = request_session.get_or_create_session_from_share(&request_share, q.params());
        session.announced.set(0, true);
        session.announced.set(1, true);
        let h2 = *share0.sign_hash().as_bytes();
        manager.sig_shares.add(&(h2, 0), share0.clone());
        manager.sig_shares.add(&(h2, 1), share1.clone());
        *manager.sig_shares_queued_to_announce.get_or_add(&(h2, 0)) = true;
        *manager.sig_shares_queued_to_announce.get_or_add(&(h2, 1)) = true;
        let mut lookup = FakeLookup::default();
        lookup.quorums.insert((TEST_TYPE.to_base(), *q.quorum_hash()), q.clone());
        let net = FakeNet {
            quorum_peers: [((TEST_TYPE.to_base(), *q.quorum_hash()), vec![9])].into(),
            connected: vec![PeerFacts {
                id: 9,
                verified_pro_tx: None,
                common_version: 1,
            }],
            ..Default::default()
        };
        let recsigs = FakeRecSigs::default();
        let recovery = test_recovery();
        let signer = SignerFacts::default();
        let env = test_env(&lookup, &recsigs, &net, &recovery, &signer, 0, 2);
        let effects = manager.send_tick(&env, &mut SeededRng::new(9));
        let msgs = send_messages(&effects, 9);
        let announcement = msgs.iter().find_map(|m| match m {
            SigMsg::SigSharesInv(v) => v.first(),
            _ => None,
        });
        assert!(announcement.is_some_and(|x| x.inv().get(0).unwrap_or(false) && x.inv().get(1).unwrap_or(false)));
        let requests: Vec<_> = msgs
            .iter()
            .filter_map(|m| match m {
                SigMsg::GetSigShares(v) => Some(v),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].inv().get(0).unwrap_or(false) && requests[0].inv().get(1).unwrap_or(false)
        );
        assert!(manager.node_states[&9].sessions[&request_hash].announced.inv.iter().all(|x| !x));
    }

    #[test]
    fn session_id_is_announced_once_across_families_and_ticks() {
        let q = test_quorum(3, 7);
        let share = test_share(&q, 8, 9, 1);
        let hash = *share.sign_hash().as_bytes();
        let mut manager = SigSharesManager::new();
        let state = manager.node_states.entry(10).or_default();
        let session = state.get_or_create_session_from_share(&share, q.params());
        session.announced.set(0, true);
        manager.sig_shares.add(&(hash, 1), share);
        *manager.sig_shares_queued_to_announce.get_or_add(&(hash, 1)) = true;
        let mut lookup = FakeLookup::default();
        let q2 = q.clone();
        lookup.quorums.insert((TEST_TYPE.to_base(), *q.quorum_hash()), q);
        let net = FakeNet {
            connected: vec![PeerFacts {
                id: 10,
                verified_pro_tx: None,
                common_version: 1,
            }],
            quorum_peers: [(
                (TEST_TYPE.to_base(), crate::dkg::wire::QuorumHash::from([7; 32])),
                vec![10],
            )]
            .into(),
            ..Default::default()
        };
        let recsigs = FakeRecSigs::default();
        let recovery = test_recovery();
        let signer = SignerFacts::default();
        let env = test_env(&lookup, &recsigs, &net, &recovery, &signer, 0, 2);
        let first = manager.send_tick(&env, &mut SeededRng::new(11));
        let first_msgs = send_messages(&first, 10);
        assert!(first_msgs.iter().any(|m| matches!(m, SigMsg::GetSigShares(_))));
        assert!(first_msgs.iter().any(|m| matches!(m, SigMsg::SigSharesInv(_))));
        let first_announcements = first_msgs
            .iter()
            .filter(|m| matches!(m, SigMsg::SesAnn(_)))
            .map(|m| match m {
                SigMsg::SesAnn(v) => v.len(),
                _ => 0,
            })
            .sum::<usize>();
        assert_eq!(first_announcements, 1);
        let id = manager.node_states[&10].sessions[&hash].send_session_id;
        let share2 = test_share(&q2, 8, 9, 2);
        manager.sig_shares.add(&(hash, 2), share2);
        *manager.sig_shares_queued_to_announce.get_or_add(&(hash, 2)) = true;
        let second = manager.send_tick(&env, &mut SeededRng::new(12));
        let second_msgs = send_messages(&second, 10);
        assert!(second_msgs.iter().all(|m| !matches!(m, SigMsg::SesAnn(_))));
        let inv = second_msgs
            .iter()
            .find_map(|m| match m {
                SigMsg::SigSharesInv(v) => v.first(),
                _ => None,
            })
            .unwrap();
        assert_eq!(inv.session_id(), id);
    }

    #[test]
    fn disconnected_peer_gets_no_send_but_request_state_is_updated() {
        let q = test_quorum(3, 12);
        let share = test_share(&q, 13, 14, 0);
        let hash = *share.sign_hash().as_bytes();
        let mut manager = SigSharesManager::new();
        let session = manager
            .node_states
            .entry(22)
            .or_default()
            .get_or_create_session_from_share(&share, q.params());
        session.announced.set(0, true);
        let mut lookup = FakeLookup::default();
        lookup.quorums.insert((TEST_TYPE.to_base(), *q.quorum_hash()), q);
        let net = FakeNet::default();
        let recsigs = FakeRecSigs::default();
        let recovery = test_recovery();
        let signer = SignerFacts::default();
        let env = test_env(&lookup, &recsigs, &net, &recovery, &signer, 0, 2);
        let effects = manager.send_tick(&env, &mut SeededRng::new(13));
        assert!(effects.is_empty());
        assert!(manager.node_states[&22].requested_sig_shares.has(&(hash, 0)));
    }

    #[test]
    fn cleanup_removes_active_but_unloadable_quorum_sessions() {
        let q = test_quorum(3, 15);
        let share = test_share(&q, 16, 17, 0);
        let hash = *share.sign_hash().as_bytes();
        let mut manager = SigSharesManager::new();
        manager
            .node_states
            .entry(23)
            .or_default()
            .get_or_create_session_from_share(&share, q.params());
        manager.sig_shares.add(&(hash, 0), share);
        manager.time_seen_for_sessions.insert(hash, 0);
        let mut lookup = FakeLookup::default();
        lookup.active.insert((TEST_TYPE.to_base(), *q.quorum_hash()));
        let net = FakeNet {
            connected: vec![PeerFacts {
                id: 23,
                verified_pro_tx: None,
                common_version: 1,
            }],
            ..Default::default()
        };
        let recsigs = FakeRecSigs::default();
        let recovery = test_recovery();
        let signer = SignerFacts::default();
        let env = test_env(&lookup, &recsigs, &net, &recovery, &signer, 0, 2);
        manager.cleanup(&env);
        assert!(!manager.sig_shares.has(&(hash, 0)));
        assert!(!manager.time_seen_for_sessions.contains_key(&hash));
        assert!(manager.node_states.contains_key(&23));
        assert!(!manager.node_states[&23].sessions.contains_key(&hash));
    }

    #[test]
    fn packing_uses_each_core_message_cap() {
        let peer = 31;
        let mut ses = BTreeMap::new();
        ses.insert(
            peer,
            (0..101)
                .map(|i| {
                    SigSesAnn::new(
                        i,
                        TEST_TYPE,
                        [1; 32].into(),
                        [i as u8; 32].into(),
                        [2; 32].into(),
                    )
                })
                .collect(),
        );
        let mut req = BTreeMap::new();
        req.insert(
            peer,
            (0..201)
                .map(|i| {
                    (
                        [i as u8; 32],
                        SigSharesInv {
                            session_id: 1,
                            inv: vec![true],
                        },
                    )
                })
                .collect(),
        );
        let mut out = BTreeMap::new();
        out.insert(
            peer,
            (0..3)
                .map(|i| {
                    let count = [200, 200, 1][i];
                    let shares = (0..count)
                        .map(|m| {
                            BatchedSigShare::new(m as u16, SigShareBytes::from_bytes([m as u8; 96]))
                        })
                        .collect();
                    ([i as u8 + 1; 32], BatchedSigShares::new(1, shares))
                })
                .collect(),
        );
        let mut ann = BTreeMap::new();
        ann.insert(
            peer,
            (0..201)
                .map(|i| {
                    (
                        [i as u8; 32],
                        SigSharesInv {
                            session_id: 1,
                            inv: vec![true],
                        },
                    )
                })
                .collect(),
        );
        let mut direct = BTreeMap::new();
        direct.insert(
            peer,
            (0..33)
                .map(|i| {
                    SigShare::new(
                        TEST_TYPE,
                        [1; 32].into(),
                        0,
                        [i as u8; 32].into(),
                        [2; 32].into(),
                        SigShareBytes::from_bytes([i as u8; 96]),
                    )
                })
                .collect(),
        );
        let effects = SigSharesManager::<FakeQuorum>::new().pack(
            ses,
            req,
            out,
            ann,
            direct,
            &BTreeSet::from([peer]),
        );
        let msgs = send_messages(&effects, peer);
        assert_eq!(msgs.iter().filter(|m| matches!(m, SigMsg::SesAnn(_))).count(), 2);
        assert_eq!(msgs.iter().filter(|m| matches!(m, SigMsg::GetSigShares(_))).count(), 2);
        let batches: Vec<_> = msgs
            .iter()
            .filter_map(|m| match m {
                SigMsg::BSigShares(v) => Some(v),
                _ => None,
            })
            .collect();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].iter().map(|x| x.sig_shares().len()).sum::<usize>(), 400);
        assert_eq!(batches[1].iter().map(|x| x.sig_shares().len()).sum::<usize>(), 1);
        let sizes = |f: fn(&SigMsg) -> Option<usize>| msgs.iter().filter_map(f).collect::<Vec<_>>();
        assert_eq!(
            sizes(|m| match m {
                SigMsg::SesAnn(v) => Some(v.len()),
                _ => None,
            }),
            [100, 1]
        );
        assert_eq!(
            sizes(|m| match m {
                SigMsg::GetSigShares(v) => Some(v.len()),
                _ => None,
            }),
            [200, 1]
        );
        assert_eq!(
            sizes(|m| match m {
                SigMsg::SigSharesInv(v) => Some(v.len()),
                _ => None,
            }),
            [200, 1]
        );
        assert_eq!(
            sizes(|m| match m {
                SigMsg::SigShare(v) => Some(v.len()),
                _ => None,
            }),
            [32, 1]
        );
        assert_eq!(msgs.iter().filter(|m| matches!(m, SigMsg::SigSharesInv(_))).count(), 2);
        assert_eq!(msgs.iter().filter(|m| matches!(m, SigMsg::SigShare(_))).count(), 2);
    }

    #[test]
    fn request_reassigns_at_five_seconds_even_when_cleanup_is_throttled() {
        let q = test_quorum(3, 18);
        let share = test_share(&q, 19, 20, 0);
        let hash = *share.sign_hash().as_bytes();
        let mut manager = SigSharesManager::new();
        for peer in [41, 42] {
            let session = manager
                .node_states
                .entry(peer)
                .or_default()
                .get_or_create_session_from_share(&share, q.params());
            session.announced.set(0, true);
        }
        manager.node_states.get_mut(&42).unwrap().requested_sig_shares.add(&(hash, 0), 0);
        manager.sig_shares_requested.add(&(hash, 0), (42, 0));
        let mut lookup = FakeLookup::default();
        lookup.quorums.insert((TEST_TYPE.to_base(), *q.quorum_hash()), q);
        lookup.active.insert((TEST_TYPE.to_base(), crate::dkg::wire::QuorumHash::from([18; 32])));
        let net = FakeNet {
            connected: vec![
                PeerFacts {
                    id: 41,
                    verified_pro_tx: None,
                    common_version: 1,
                },
                PeerFacts {
                    id: 42,
                    verified_pro_tx: None,
                    common_version: 1,
                },
            ],
            ..Default::default()
        };
        let recsigs = FakeRecSigs::default();
        let recovery = test_recovery();
        let signer = SignerFacts::default();
        manager.cleanup(&test_env(&lookup, &recsigs, &net, &recovery, &signer, 1_000, 2));
        assert!(!manager.cleanup_throttler.try_cleanup(5_000, 5_000));
        manager.cleanup_throttler.next_cleanup_ms = 6_000;
        let early = test_env(&lookup, &recsigs, &net, &recovery, &signer, 4_999, 2);
        assert!(manager.send_tick(&early, &mut SeededRng::new(20)).is_empty());
        assert_eq!(manager.sig_shares_requested.get(&(hash, 0)), Some(&(42, 0)));
        let env = test_env(&lookup, &recsigs, &net, &recovery, &signer, 5_000, 2);
        manager.cleanup(&env);
        let effects = manager.send_tick(&env, &mut SeededRng::new(21));
        assert!(send_messages(&effects, 41).iter().any(|m| matches!(m, SigMsg::GetSigShares(_))));
        assert!(send_messages(&effects, 42).is_empty());
        assert_eq!(manager.sig_shares_requested.get(&(hash, 0)), Some(&(41, 5)));
        assert!(!manager.node_states[&42].requested_sig_shares.has(&(hash, 0)));
    }

    #[test]
    fn timed_out_peer_is_not_asked_again_for_the_same_share() {
        let q = test_quorum(3, 43);
        let share = test_share(&q, 44, 45, 0);
        let hash = *share.sign_hash().as_bytes();
        let mut manager = SigSharesManager::new();
        manager
            .node_states
            .entry(42)
            .or_default()
            .get_or_create_session_from_share(&share, q.params())
            .announced
            .set(0, true);
        manager.node_states.get_mut(&42).unwrap().requested_sig_shares.add(&(hash, 0), 0);
        manager.sig_shares_requested.add(&(hash, 0), (42, 0));
        let lookup = FakeLookup::default();
        let net = FakeNet {
            connected: vec![PeerFacts {
                id: 42,
                verified_pro_tx: None,
                common_version: 1,
            }],
            ..Default::default()
        };
        let recsigs = FakeRecSigs::default();
        let recovery = test_recovery();
        let signer = SignerFacts::default();
        let env = test_env(&lookup, &recsigs, &net, &recovery, &signer, 5_000, 2);
        assert!(manager.send_tick(&env, &mut SeededRng::new(46)).is_empty());
        assert!(!manager.node_states[&42].requested_sig_shares.has(&(hash, 0)));
        assert_eq!(manager.sig_shares_requested.get(&(hash, 0)), Some(&(42, 0)));
    }

    #[test]
    fn cleanup_keeps_connected_banned_peer_state() {
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        manager.node_states.entry(47).or_default().banned = true;
        manager.node_states.entry(48).or_default();
        let lookup = FakeLookup::default();
        let net = FakeNet {
            connected: vec![PeerFacts {
                id: 47,
                verified_pro_tx: None,
                common_version: 1,
            }],
            banned: [47].into(),
            ..Default::default()
        };
        let recsigs = FakeRecSigs::default();
        let recovery = test_recovery();
        let signer = SignerFacts::default();
        manager.cleanup(&test_env(&lookup, &recsigs, &net, &recovery, &signer, 0, 2));
        assert!(manager.node_states.contains_key(&47));
        assert!(!manager.node_states.contains_key(&48));
    }

    #[test]
    fn concentrated_send_cases_follow_core_attempt_rules() {
        let q = test_quorum(3, 22);
        let share = test_share(&q, 23, 24, 0);
        let hash = *share.sign_hash().as_bytes();
        let mut lookup = FakeLookup::default();
        lookup.quorums.insert((TEST_TYPE.to_base(), *q.quorum_hash()), q.clone());
        let recsigs = FakeRecSigs::default();
        let recovery = test_recovery();
        let signer = SignerFacts::default();
        let mut manager = SigSharesManager::new();
        manager
            .signed_sessions
            .insert(hash, super::super::state::SignedSession::new(share.clone(), q.clone()));
        let net = FakeNet {
            connected: q
                .members()
                .iter()
                .enumerate()
                .map(|(i, p)| PeerFacts {
                    id: 50 + i as i64,
                    verified_pro_tx: Some(*p),
                    common_version: 1,
                })
                .collect(),
            ..Default::default()
        };
        let env = test_env(&lookup, &recsigs, &net, &recovery, &signer, 0, 0);
        let effects = manager.send_tick(&env, &mut SeededRng::new(25));
        assert_eq!(effects.len(), 1);
        assert!(send_messages(&effects, 50).iter().any(|m| matches!(m, SigMsg::SigShare(_))));
        assert_eq!(manager.signed_sessions[&hash].attempt, 1);

        let mut skipped = SigSharesManager::new();
        skipped
            .signed_sessions
            .insert(hash, super::super::state::SignedSession::new(share.clone(), q.clone()));
        let net = FakeNet {
            connected: vec![PeerFacts {
                id: 52,
                verified_pro_tx: Some(q.members()[2]),
                common_version: 1,
            }],
            ..Default::default()
        };
        let env = test_env(&lookup, &recsigs, &net, &recovery, &signer, 0, 0);
        let effects = skipped.send_tick(&env, &mut SeededRng::new(26));
        assert!(send_messages(&effects, 52).iter().any(|m| matches!(m, SigMsg::SigShare(_))));
        assert_eq!(skipped.signed_sessions[&hash].attempt, 3);

        let mut none = SigSharesManager::new();
        none.signed_sessions
            .insert(hash, super::super::state::SignedSession::new(share.clone(), q.clone()));
        let empty_net = FakeNet::default();
        let env = test_env(&lookup, &recsigs, &empty_net, &recovery, &signer, 0, 0);
        assert!(none.send_tick(&env, &mut SeededRng::new(27)).is_empty());
        assert_eq!(none.signed_sessions[&hash].attempt, 1);

        let mut self_target = SigSharesManager::new();
        self_target
            .signed_sessions
            .insert(hash, super::super::state::SignedSession::new(share, q.clone()));
        let signer = SignerFacts {
            my_pro_tx: Some(q.members()[1]),
            signing_allowed: true,
        };
        let env = test_env(&lookup, &recsigs, &net, &recovery, &signer, 0, 0);
        assert!(self_target.send_tick(&env, &mut SeededRng::new(28)).is_empty());
        assert_eq!(self_target.signed_sessions[&hash].attempt, 2);
    }

    #[test]
    fn concentrated_backoff_uses_signed_session_state() {
        let q = test_quorum(5, 29);
        let share = test_share(&q, 30, 31, 0);
        let hash = *share.sign_hash().as_bytes();
        let mut manager = SigSharesManager::new();
        manager
            .signed_sessions
            .insert(hash, super::super::state::SignedSession::new(share, q.clone()));
        let mut lookup = FakeLookup::default();
        lookup.quorums.insert((TEST_TYPE.to_base(), *q.quorum_hash()), q.clone());
        let net = FakeNet {
            connected: q
                .members()
                .iter()
                .enumerate()
                .map(|(i, p)| PeerFacts {
                    id: 60 + i as i64,
                    verified_pro_tx: Some(*p),
                    common_version: 1,
                })
                .collect(),
            ..Default::default()
        };
        let recsigs = FakeRecSigs::default();
        let recovery = test_recovery();
        let signer = SignerFacts::default();
        let mut env = test_env(&lookup, &recsigs, &net, &recovery, &signer, 0, 0);
        let expected = [2_000, 4_000, 8_000, 10_000, 10_000];
        let mut now = 0;
        for backoff in expected {
            env.now.ms = now;
            manager.send_tick(&env, &mut SeededRng::new(32));
            let session = &manager.signed_sessions[&hash];
            assert_eq!(session.next_attempt_time - now, backoff);
            now = session.next_attempt_time;
        }
    }

    #[test]
    fn cleanup_keeps_at_fifty_nine_seconds_and_removes_at_sixty() {
        let q = test_quorum(3, 33);
        let share = test_share(&q, 34, 35, 0);
        let hash = *share.sign_hash().as_bytes();
        let mut manager = SigSharesManager::new();
        manager.sig_shares.add(&(hash, 0), share);
        manager.time_seen_for_sessions.insert(hash, 0);
        let mut lookup = FakeLookup::default();
        lookup.quorums.insert((TEST_TYPE.to_base(), *q.quorum_hash()), q);
        lookup.active.insert((TEST_TYPE.to_base(), crate::dkg::wire::QuorumHash::from([33; 32])));
        let net = FakeNet::default();
        let recsigs = FakeRecSigs::default();
        let recovery = test_recovery();
        let signer = SignerFacts::default();
        let env = test_env(&lookup, &recsigs, &net, &recovery, &signer, 59_999, 2);
        manager.cleanup(&env);
        assert!(manager.sig_shares.has(&(hash, 0)));
        manager.cleanup_throttler = CleanupThrottler::default();
        let env = test_env(&lookup, &recsigs, &net, &recovery, &signer, 60_000, 2);
        manager.cleanup(&env);
        assert!(!manager.sig_shares.has(&(hash, 0)));
    }

    #[test]
    fn seeded_rng_has_exact_session_id_and_wire_output() {
        let q = test_quorum(3, 36);
        let share = test_share(&q, 37, 38, 0);
        let hash = *share.sign_hash().as_bytes();
        let mut manager = SigSharesManager::new();
        let session = manager
            .node_states
            .entry(70)
            .or_default()
            .get_or_create_session_from_share(&share, q.params());
        session.announced.set(0, true);
        let mut lookup = FakeLookup::default();
        lookup.quorums.insert((TEST_TYPE.to_base(), *q.quorum_hash()), q);
        let net = FakeNet {
            connected: vec![PeerFacts {
                id: 70,
                verified_pro_tx: None,
                common_version: 1,
            }],
            ..Default::default()
        };
        let recsigs = FakeRecSigs::default();
        let recovery = test_recovery();
        let signer = SignerFacts::default();
        let env = test_env(&lookup, &recsigs, &net, &recovery, &signer, 0, 2);
        let effects = manager.send_tick(&env, &mut SeededRng::new(7));
        let msgs = send_messages(&effects, 70);
        let ann = msgs
            .iter()
            .find_map(|m| match m {
                SigMsg::SesAnn(v) => v.first(),
                _ => None,
            })
            .unwrap();
        let inv = msgs.iter().find_map(|m| match m {
            SigMsg::GetSigShares(v) => v.first(),
            SigMsg::SigSharesInv(v) => v.first(),
            _ => None,
        });
        assert_eq!(msgs.len(), 2);
        assert!(matches!(msgs[0], SigMsg::SesAnn(_)) && matches!(msgs[1], SigMsg::GetSigShares(_)));
        assert_eq!(
            *ann,
            SigSesAnn::new(
                1_674_306_020,
                share.llmq_type(),
                share.quorum_hash(),
                (*share.id().as_bytes()).into(),
                (*share.msg_hash().as_bytes()).into(),
            )
        );
        let expected_inv = SigSharesInv {
            session_id: 1_674_306_020,
            inv: vec![true, false, false],
        };
        assert_eq!(*inv.unwrap(), expected_inv.to_wire());
        assert!(manager.node_states[&70].sessions[&hash].send_session_id == 1_674_306_020);
    }

    #[test]
    fn force_reannouncement_queues_fresh_single_session_announcement() {
        let q = test_quorum(3, 39);
        let id = [40; 32];
        let msg = [41; 32];
        let share0 = test_share(&q, 40, 41, 0);
        let share1 = test_share(&q, 40, 41, 1);
        let hash = *share0.sign_hash().as_bytes();
        let mut manager = SigSharesManager::new();
        manager.sig_shares.add(&(hash, 0), share0.clone());
        manager.sig_shares.add(&(hash, 1), share1);
        let session = manager
            .node_states
            .entry(80)
            .or_default()
            .get_or_create_session_from_share(&share0, q.params());
        session.knows.set_all(true);
        session.send_session_id = 123;
        manager.force_reannouncement(
            q.clone(),
            TEST_TYPE,
            id,
            msg,
            SporkFacts {
                spork21_active: false,
                spork21_value: 2,
            },
        );
        assert_eq!(manager.sig_shares_queued_to_announce.size(), 2);
        let session = &manager.node_states[&80].sessions[&hash];
        assert_eq!(session.knows.count_set(), 0);
        assert_eq!(session.send_session_id, UNINITIALIZED_SESSION_ID);
        let mut lookup = FakeLookup::default();
        lookup.quorums.insert((TEST_TYPE.to_base(), *q.quorum_hash()), q.clone());
        let net = FakeNet {
            quorum_peers: [((TEST_TYPE.to_base(), *q.quorum_hash()), vec![80])].into(),
            connected: vec![PeerFacts {
                id: 80,
                verified_pro_tx: None,
                common_version: 1,
            }],
            ..Default::default()
        };
        let recsigs = FakeRecSigs::default();
        let recovery = test_recovery();
        let signer = SignerFacts::default();
        let env = test_env(&lookup, &recsigs, &net, &recovery, &signer, 0, 2);
        let effects = manager.send_tick(&env, &mut SeededRng::new(42));
        let msgs = send_messages(&effects, 80);
        assert_eq!(msgs.iter().filter(|m| matches!(m, SigMsg::SesAnn(_))).count(), 1);
        let inv = msgs
            .iter()
            .find_map(|m| match m {
                SigMsg::SigSharesInv(v) => v.first(),
                _ => None,
            })
            .unwrap();
        assert!(inv.inv().get(0).unwrap_or(false) && inv.inv().get(1).unwrap_or(false));
        assert!(manager.sig_shares_queued_to_announce.is_empty());

        let before = &manager.node_states[&80].sessions[&hash];
        let (knows_before, id_before) = (before.knows.clone(), before.send_session_id);
        manager.force_reannouncement(
            q,
            TEST_TYPE,
            id,
            msg,
            SporkFacts {
                spork21_active: false,
                spork21_value: 0,
            },
        );
        assert!(manager.sig_shares_queued_to_announce.is_empty());
        let after = &manager.node_states[&80].sessions[&hash];
        assert_eq!(after.knows, knows_before);
        assert_eq!(after.send_session_id, id_before);
    }
}
