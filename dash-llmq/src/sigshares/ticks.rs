//! Pure worker cadence and verification in-flight limits.
//!
//! Core: v24.0.0 l/net_signing.cpp:152-170,283-298,322-391.
#![allow(deprecated)]

use core::fmt;

use rand_core::RngCore;

use crate::dkg::bls::BlsCtx;
use crate::sigenv::{
    Collected, OperatorSigner, PeerId, QuorumKey, QuorumLookup, RecSigReads, ShareRecovery,
    ShareVerifier, SharesEffect, SharesEnv, SigSharesNet, SignerFacts, SigningQuorum, VerifyError,
};

use super::manager::{CleanupThrottler, SigSharesManager};
use super::sign::create_sig_share;

/// Idle sleep after a recovered-signature worker pass with no remaining work.
// Core: v24.0.0 l/net_signing.cpp:294 NetSigning::WorkThreadSigning
pub const SIGNING_IDLE_SLEEP_MS: i64 = 100;

/// Minimum interval between recovered-signature database cleanups.
// Core: v24.0.0 l/net_signing.cpp:288 NetSigning::WorkThreadSigning
pub const RECSIG_CLEANUP_INTERVAL_MS: i64 = 5_000;

/// Fixed sleep after each sig-share cleaning and outbound send pass.
// Core: v24.0.0 l/net_signing.cpp:332 NetSigning::WorkThreadCleaning
pub const CLEANING_SLEEP_MS: i64 = 100;

/// Fixed sleep after each sig-share dispatcher pass.
// Core: v24.0.0 l/net_signing.cpp:389 NetSigning::WorkThreadDispatcher
pub const DISPATCHER_SLEEP_MS: i64 = 10;

/// Maximum number of collected share batches awaiting verification at once.
// Core: v24.0.0 l/net_signing.cpp:361 NetSigning::WorkThreadDispatcher
pub const MAX_UNVERIFIED_BATCHES: usize = 4;

/// Maximum number of shares collected into one verification batch.
// Core: v24.0.0 l/net_signing.cpp:362 NetSigning::WorkThreadDispatcher
pub const MAX_SHARES_PER_BATCH: usize = 32;

/// Worker pool size for sig-share tasks (`clamp(hardware_concurrency / 2, 1, 4)`).
// Core: v24.0.0 l/net_signing.cpp:152-170 NetSigning::Start
pub fn worker_count(hardware_concurrency: usize) -> usize {
    (hardware_concurrency / 2).clamp(1, 4)
}

/// When a worker loop should run its next pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NextWake {
    /// Re-loop immediately.
    Now,
    /// Sleep until this injected millisecond timestamp.
    At(i64),
}

/// Cadence decision after one pass of Core's recovered-signature worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SigningPass {
    pub run_cleanup: bool,
    pub next: NextWake,
}

/// Cadence tracker for Core's `WorkThreadSigning` loop; the recovered-signature pass itself
/// belongs to the runtime (`SG1-W1`).
#[derive(Default)]
pub struct SigningLoop {
    throttler: CleanupThrottler,
}

impl SigningLoop {
    // Core: v24.0.0 l/net_signing.h:86 NetSigning::cleanupThrottler
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance the 5 s cleanup throttle and compute the next wake deadline after a pass.
    // Core: v24.0.0 l/net_signing.cpp:283-298 NetSigning::WorkThreadSigning
    pub fn after_pass(&mut self, now_ms: i64, more_work: bool) -> SigningPass {
        let run_cleanup = self.throttler.try_cleanup(now_ms, RECSIG_CLEANUP_INTERVAL_MS);
        let next = if more_work {
            NextWake::Now
        } else {
            NextWake::At(now_ms.saturating_add(SIGNING_IDLE_SLEEP_MS))
        };
        SigningPass {
            run_cleanup,
            next,
        }
    }
}

/// Run one pass of Core's `WorkThreadCleaning`: remove banned peer states, collect outbound
/// messages, run throttled cleanup, and schedule the fixed 100 ms sleep.
// Core: v24.0.0 l/net_signing.cpp:322-334 NetSigning::WorkThreadCleaning; :300-305 RemoveBannedNodeStates
pub fn cleaning_tick<Q, L, R, N, S>(
    mgr: &mut SigSharesManager<Q>,
    env: &SharesEnv<'_, L, R, N, S>,
    is_banned: impl FnMut(PeerId) -> bool,
    rng: &mut impl RngCore,
    now_ms: i64,
) -> (Vec<SharesEffect>, NextWake)
where
    Q: SigningQuorum,
    L: QuorumLookup<Quorum = Q>,
    R: RecSigReads,
    N: SigSharesNet,
    S: ShareRecovery,
{
    mgr.remove_peers_if(is_banned);
    let mut effects = mgr.send_tick(env, rng);
    effects.extend(mgr.cleanup(env));
    (effects, NextWake::At(now_ms.saturating_add(CLEANING_SLEEP_MS)))
}

struct DebugEffect<'a>(&'a SharesEffect);

impl fmt::Debug for DebugEffect<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            SharesEffect::Ban {
                peer,
                mark_shares_banned,
            } => f
                .debug_struct("Ban")
                .field("peer", peer)
                .field("mark_shares_banned", mark_shares_banned)
                .finish(),
            SharesEffect::Recovered(rs) => f
                .debug_struct("Recovered")
                .field("llmq_type", &rs.llmq_type)
                .field("quorum_hash", &rs.quorum_hash)
                .field("id", &rs.id)
                .field("msg_hash", &rs.msg_hash)
                .finish(),
            SharesEffect::Send {
                peer,
                msgs,
            } => f.debug_struct("Send").field("peer", peer).field("msgs_len", &msgs.len()).finish(),
        }
    }
}

/// Completed pass of [`dispatcher_tick`].
pub struct DispatcherOutcome {
    pub effects: Vec<SharesEffect>,
    /// Set when a batch quorum was not found: the drained shares stay out of pending and the
    /// whole batch is dropped: nothing is verified, processed or banned, and collection stops
    /// for this pass (`Collected::MissingQuorum`, DEC-1633).
    pub missing_quorum: Option<QuorumKey>,
    /// Number of collected share batches verified and processed during this pass.
    pub batches: usize,
    pub next: NextWake,
}

impl fmt::Debug for DispatcherOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let effects: Vec<DebugEffect<'_>> = self.effects.iter().map(DebugEffect).collect();
        f.debug_struct("DispatcherOutcome")
            .field("effects", &effects)
            .field("missing_quorum", &self.missing_quorum)
            .field("batches", &self.batches)
            .field("next", &self.next)
            .finish()
    }
}

/// Step failure that stopped [`dispatcher_tick`].
#[derive(Debug)]
pub enum DispatcherErrorKind<LE, RE> {
    /// Quorum lookup failed during `SigSharesManager::collect_to_verify`.
    Lookup(LE),
    /// Share batch verification failed during `ShareVerifier::verify_batch`.
    Verify(VerifyError<RE>),
}

/// Error returned when [`dispatcher_tick`] stops on a lookup or verification failure.
///
/// `effects` carries all [`SharesEffect`]s already produced earlier in the pass (from
/// dispatched local signs and any preceding verified batches) in order, so state transitions
/// already applied to [`SigSharesManager`] are never lost. `batches` counts batches verified
/// and processed before the error, and `next` is `NextWake::At(now_ms + DISPATCHER_SLEEP_MS)`.
/// The shares drained into the failing batch stay out of pending and are not retried, as when
/// a verification task fails in Core's worker pool.
pub struct DispatcherError<LE, RE> {
    pub kind: DispatcherErrorKind<LE, RE>,
    pub effects: Vec<SharesEffect>,
    pub batches: usize,
    pub next: NextWake,
}

impl<LE: fmt::Debug, RE: fmt::Debug> fmt::Debug for DispatcherError<LE, RE> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let effects: Vec<DebugEffect<'_>> = self.effects.iter().map(DebugEffect).collect();
        f.debug_struct("DispatcherError")
            .field("kind", &self.kind)
            .field("effects", &effects)
            .field("batches", &self.batches)
            .field("next", &self.next)
            .finish()
    }
}

/// Run one pass of Core's `WorkThreadDispatcher`: create and process every queued local sign,
/// then collect, verify and process up to `MAX_UNVERIFIED_BATCHES.saturating_sub(in_flight)`
/// share batches of at most `MAX_SHARES_PER_BATCH` shares each, and schedule the 10 ms sleep.
///
/// Each batch is verified and processed inline, so none is still in flight when this returns.
/// `in_flight` counts batches the caller is verifying elsewhere (Core's `unverified_batches`);
/// a caller that verifies only through this function passes 0, and the cap is then
/// `MAX_UNVERIFIED_BATCHES` batches per pass.
///
/// If `collect_to_verify` returns [`Collected::MissingQuorum`], a batch quorum was not found:
/// the drained shares stay out of pending and the whole batch is dropped: nothing is verified,
/// processed or banned, and collection stops for this pass (`missing_quorum` reports `key`).
///
/// # Errors
///
/// Returns [`DispatcherError`] if `collect_to_verify` or `verifier.verify_batch` fails,
/// stopping the pass immediately while preserving all `effects` gathered earlier in the pass,
/// the completed `batches` count, and `next` (`At(now_ms + 10)`).
// Core: v24.0.0 l/net_signing.cpp:336-391 NetSigning::WorkThreadDispatcher; l/signing_shares.cpp:1535-1551 CSigSharesManager::SignAndProcessSingleShare
#[allow(
    clippy::too_many_arguments,
    reason = "pure dispatcher tick takes each injected dependency and cadence input explicitly"
)]
pub fn dispatcher_tick<Q, L, R, N, S>(
    mgr: &mut SigSharesManager<Q>,
    env: &SharesEnv<'_, L, R, N, S>,
    verifier: &impl ShareVerifier,
    facts: &SignerFacts,
    op: &impl OperatorSigner,
    bls: BlsCtx,
    check: BlsCtx,
    rng: &mut impl RngCore,
    in_flight: usize,
    now_ms: i64,
) -> Result<DispatcherOutcome, DispatcherError<L::Error, R::Error>>
where
    Q: SigningQuorum,
    L: QuorumLookup<Quorum = Q>,
    R: RecSigReads,
    N: SigSharesNet,
    S: ShareRecovery,
{
    let next = NextWake::At(now_ms.saturating_add(DISPATCHER_SLEEP_MS));
    let mut effects = Vec::new();

    for item in mgr.dispatch_pending_signs() {
        if let Some(share) =
            create_sig_share(item.quorum.as_ref(), item.id, item.msg_hash, facts, op, bls)
        {
            effects.extend(mgr.process_own_share(share, item.quorum, env));
        }
    }

    let budget = MAX_UNVERIFIED_BATCHES.saturating_sub(in_flight);
    let mut batches = 0;
    let mut missing_quorum = None;

    for _ in 0..budget {
        match mgr.collect_to_verify(MAX_SHARES_PER_BATCH, env.lookup, rng) {
            Ok(Collected::Empty) => break,
            Ok(Collected::Ready {
                batch,
                more_work,
            }) => {
                let verdict = match verifier.verify_batch(&batch, env.recsigs, check) {
                    Ok(verdict) => verdict,
                    Err(err) => {
                        return Err(DispatcherError {
                            kind: DispatcherErrorKind::Verify(err),
                            effects,
                            batches,
                            next,
                        });
                    }
                };
                effects.extend(mgr.process_verified(&batch, &verdict, env));
                batches += 1;
                if !more_work {
                    break;
                }
            }
            // declared difference (DEC-1633): v24.0.0 l/signing_shares.cpp:592-600, l/net_signing.cpp:367-369
            Ok(Collected::MissingQuorum {
                key,
            }) => {
                missing_quorum = Some(key);
                break;
            }
            Err(err) => {
                return Err(DispatcherError {
                    kind: DispatcherErrorKind::Lookup(err),
                    effects,
                    batches,
                    next,
                });
            }
        }
    }

    Ok(DispatcherOutcome {
        effects,
        missing_quorum,
        batches,
        next,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, BTreeSet};
    use std::convert::Infallible;
    use std::sync::{Arc, LazyLock};

    use dash_evo::locks::sign_hash;
    use dash_evo::types::ProTxHash;
    use dash_params::{ChainParamsBuilder, ChainSetup, LlmqParams, LlmqType, Network, Release};
    use dash_types::Numeric;

    use super::*;
    use crate::dkg::bls::{BlsSchemeKind, BlsSigValue, BlsSkValue, secret_to_public, sign};
    use crate::dkg::rng::SeededRng;
    use crate::dkg::wire::{QuorumHash, SigSesAnn, SigShare, SigSharesBits, SigSharesInv};
    use crate::sigenv::fakes::{
        FakeLookup, FakeNet, FakeOperatorSigner, FakeQuorum, FakeRecSigs, FakeRecovery,
    };
    use crate::sigenv::{
        BatchVerdict, Now, PeerFacts, PendingSign, Recovered, ShareBatch, SporkFacts,
    };
    use crate::sigshares::verify::ReferenceVerifier;

    const TEST_TYPE: LlmqType = LlmqType::LlmqTest;
    const BASIC: BlsCtx = BlsCtx::BASIC;
    const RING_SPORKS: SporkFacts = SporkFacts {
        spork21_active: true,
        spork21_value: 4_070_908_800,
    };
    const MESH_SPORKS: SporkFacts = SporkFacts {
        spork21_active: true,
        spork21_value: 0,
    };

    static CHAIN: LazyLock<ChainSetup> = LazyLock::new(|| {
        let mut setup = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
        setup.consensus.llmq_type_platform = LlmqType::LlmqTestPlatform;
        setup
    });

    fn secret(n: u8) -> BlsSkValue {
        let mut bytes = [0; 32];
        bytes[31] = n;
        BlsSkValue::from_canonical(&bytes, BlsSchemeKind::Basic).unwrap()
    }

    fn pro_tx(i: u8) -> ProTxHash {
        ProTxHash::from([i; 32])
    }

    fn make_quorum(hash_byte: u8, threshold: u16, first_pro_tx: u8) -> Arc<FakeQuorum> {
        let mut params = *CHAIN.consensus.llmq(TEST_TYPE).unwrap();
        params.size = 3;
        params.min_size = 3;
        params.threshold = threshold;
        params.recovery_members = 3;
        let sk0 = secret(10);
        let sk1 = secret(11);
        let sk2 = secret(12);
        Arc::new(FakeQuorum {
            llmq_type: TEST_TYPE,
            quorum_hash: QuorumHash::from([hash_byte; 32]),
            params,
            members: (0..3).map(|i| pro_tx(first_pro_tx + i)).collect(),
            valid_members: [0, 1, 2].into(),
            has_verification_vector: true,
            quorum_public_key: secret_to_public(&secret(20)),
            pk_shares: [
                (0, secret_to_public(&sk0)),
                (1, secret_to_public(&sk1)),
                (2, secret_to_public(&sk2)),
            ]
            .into(),
            sk_share: Some(sk0),
        })
    }

    fn register_quorum(lookup: &mut FakeLookup, q: &Arc<FakeQuorum>) -> QuorumKey {
        let key = (q.llmq_type.to_base(), q.quorum_hash);
        lookup.quorums.insert(key, Arc::clone(q));
        lookup.active.insert(key);
        key
    }

    fn signed_share(q: &FakeQuorum, member: u16, id: [u8; 32], msg: [u8; 32]) -> SigShare {
        let sk = secret(10 + u8::try_from(member).unwrap());
        let hash = sign_hash(q.llmq_type, &q.quorum_hash, &id, &msg);
        let sig = sign(&sk, &hash, false);
        SigShare::new(
            q.llmq_type,
            q.quorum_hash,
            member,
            id.into(),
            msg.into(),
            sig.to_bytes(BlsSchemeKind::Basic).into(),
        )
    }

    fn id_from_index(idx: u32) -> [u8; 32] {
        let mut out = [0x5A; 32];
        out[..4].copy_from_slice(&idx.to_le_bytes());
        out
    }

    #[derive(Default)]
    struct CountingVerifier {
        calls: Cell<usize>,
        batch_sizes: RefCell<Vec<usize>>,
        bad_sources: BTreeSet<PeerId>,
        fail_on_call: Option<(usize, QuorumKey, u16)>,
        use_reference: bool,
    }

    impl CountingVerifier {
        fn calls(&self) -> usize {
            self.calls.get()
        }

        fn batch_sizes(&self) -> Vec<usize> {
            self.batch_sizes.borrow().clone()
        }
    }

    impl ShareVerifier for CountingVerifier {
        fn verify_batch<Q: SigningQuorum, R: RecSigReads>(
            &self,
            batch: &ShareBatch<Q>,
            recsigs: &R,
            check: BlsCtx,
        ) -> Result<BatchVerdict, VerifyError<R::Error>> {
            let call_num = self.calls.get() + 1;
            self.calls.set(call_num);
            let total = batch.by_source.values().map(Vec::len).sum();
            self.batch_sizes.borrow_mut().push(total);
            if let Some((target, key, member)) = self.fail_on_call
                && call_num == target
            {
                return Err(VerifyError::MissingPubKeyShare {
                    key,
                    member,
                });
            }
            if self.use_reference {
                return ReferenceVerifier.verify_batch(batch, recsigs, check);
            }
            let bad_sources = batch
                .by_source
                .keys()
                .copied()
                .filter(|peer| self.bad_sources.contains(peer))
                .collect();
            Ok(BatchVerdict {
                bad_sources,
                pushed: total,
            })
        }
    }

    struct CountingRng {
        inner: SeededRng,
        draws: usize,
    }

    impl CountingRng {
        fn new(seed: u64) -> Self {
            Self {
                inner: SeededRng::new(seed),
                draws: 0,
            }
        }
    }

    impl rand_core::TryRng for CountingRng {
        type Error = Infallible;

        fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
            self.draws += 1;
            self.inner.try_next_u32()
        }

        fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
            self.draws += 1;
            self.inner.try_next_u64()
        }

        fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
            self.draws += 1;
            self.inner.try_fill_bytes(dst)
        }
    }

    struct CountingLookup {
        inner: FakeLookup,
        get_calls: Cell<usize>,
        fail_get: bool,
    }

    impl QuorumLookup for CountingLookup {
        type Quorum = FakeQuorum;
        type Error = &'static str;

        fn get_quorum(
            &self,
            t: LlmqType,
            h: &QuorumHash,
        ) -> Result<Option<Arc<FakeQuorum>>, Self::Error> {
            self.get_calls.set(self.get_calls.get() + 1);
            if self.fail_get {
                return Err("lookup failure");
            }
            Ok(self.inner.get_quorum(t, h).unwrap())
        }

        fn is_quorum_active(&self, t: LlmqType, h: &QuorumHash) -> Result<bool, Self::Error> {
            Ok(self.inner.is_quorum_active(t, h).unwrap())
        }

        fn select_quorum_for_signing(
            &self,
            t: LlmqType,
            selection_hash: &[u8; 32],
            sign_height: i32,
            sign_offset: i32,
        ) -> Result<Option<Arc<FakeQuorum>>, Self::Error> {
            Ok(self
                .inner
                .select_quorum_for_signing(t, selection_hash, sign_height, sign_offset)
                .unwrap())
        }
    }

    fn counting_recovery(calls: &Cell<usize>) -> impl ShareRecovery + '_ {
        FakeRecovery {
            recover: move |_params: &LlmqParams,
                           _members: &[ProTxHash],
                           shares: &BTreeMap<u16, BlsSigValue>,
                           counter: &mut u32| {
                calls.set(calls.get() + 1);
                *counter += 1;
                Recovered::Sig {
                    sig: shares.values().next().unwrap().clone(),
                    self_verify: false,
                }
            },
            order: |members: &[ProTxHash], _id: &[u8; 32]| (0..members.len()).collect(),
        }
    }

    fn enqueue_shares<L, R, N, S>(
        mgr: &mut SigSharesManager<FakeQuorum>,
        peer: PeerId,
        shares: &[SigShare],
        env: &SharesEnv<'_, L, R, N, S>,
    ) where
        L: QuorumLookup<Quorum = FakeQuorum>,
        R: RecSigReads,
        N: SigSharesNet,
        S: ShareRecovery,
    {
        for chunk in shares.chunks(MAX_SHARES_PER_BATCH) {
            let effects = mgr.on_sig_share(peer, chunk.to_vec(), env);
            assert!(effects.is_empty());
        }
    }

    #[test]
    fn test_worker_count_clamps_half_concurrency() {
        let cases = [
            (0, 1),
            (1, 1),
            (2, 1),
            (3, 1),
            (4, 2),
            (5, 2),
            (6, 3),
            (7, 3),
            (8, 4),
            (9, 4),
            (64, 4),
            (usize::MAX, 4),
        ];
        for (concurrency, expected) in cases {
            assert_eq!(worker_count(concurrency), expected, "concurrency={concurrency}");
        }
    }

    #[test]
    fn test_signing_loop_cadence_and_cleanup_throttle() {
        let mut loop_state = SigningLoop::new();

        let first = loop_state.after_pass(1_000, true);
        assert_eq!(
            first,
            SigningPass {
                run_cleanup: true,
                next: NextWake::Now,
            }
        );

        let idle_throttled = loop_state.after_pass(1_000, false);
        assert_eq!(
            idle_throttled,
            SigningPass {
                run_cleanup: false,
                next: NextWake::At(1_100),
            }
        );

        let before_deadline = loop_state.after_pass(5_999, true);
        assert_eq!(
            before_deadline,
            SigningPass {
                run_cleanup: false,
                next: NextWake::Now,
            }
        );

        let at_deadline = loop_state.after_pass(6_000, false);
        assert_eq!(
            at_deadline,
            SigningPass {
                run_cleanup: true,
                next: NextWake::At(6_100),
            }
        );

        let next_before = loop_state.after_pass(10_999, false);
        assert_eq!(
            next_before,
            SigningPass {
                run_cleanup: false,
                next: NextWake::At(11_099),
            }
        );

        let next_at = loop_state.after_pass(11_000, true);
        assert_eq!(
            next_at,
            SigningPass {
                run_cleanup: true,
                next: NextWake::Now,
            }
        );
    }

    #[test]
    fn test_cleaning_tick_removes_banned_before_send_and_sleeps_fixed_interval() {
        let q = make_quorum(7, 2, 1);
        let mut lookup = FakeLookup::default();
        register_quorum(&mut lookup, &q);
        let recsigs = FakeRecSigs::default();
        let net = FakeNet {
            quorum_peers: BTreeMap::new(),
            connected: vec![
                PeerFacts {
                    id: 1,
                    verified_pro_tx: None,
                    common_version: 70243,
                },
                PeerFacts {
                    id: 2,
                    verified_pro_tx: None,
                    common_version: 70243,
                },
            ],
            banned: [1].into(),
        };
        let recovery_calls = Cell::new(0);
        let recovery = counting_recovery(&recovery_calls);
        let signer = SignerFacts {
            my_pro_tx: Some(pro_tx(1)),
            signing_allowed: true,
        };
        let intake_env = SharesEnv {
            lookup: &lookup,
            recsigs: &recsigs,
            net: &net,
            recovery: &recovery,
            signer: &signer,
            sporks: RING_SPORKS,
            now: Now {
                ms: 0,
            },
            bls: BASIC,
            chain: &CHAIN,
        };

        let mut mgr = SigSharesManager::new();
        let id = [9; 32];
        let msg = [10; 32];
        for (peer, session_id, member) in [(1, 11, 0usize), (2, 22, 1usize), (3, 33, 2usize)] {
            let ann = SigSesAnn::new(session_id, TEST_TYPE, q.quorum_hash, id.into(), msg.into());
            assert!(mgr.on_ses_ann(peer, vec![ann], &intake_env).is_empty());
            let inv =
                SigSharesInv::new(session_id, SigSharesBits::new(3, vec![1u8 << member]).unwrap());
            assert!(mgr.on_inv(peer, vec![inv], &intake_env).is_empty());
        }
        assert_eq!(mgr.node_states.len(), 3);

        let env = SharesEnv {
            now: Now {
                ms: 60_000,
            },
            ..intake_env
        };
        let mut rng = SeededRng::new(5);
        let (effects, next) =
            cleaning_tick(&mut mgr, &env, |peer| net.is_banned(peer), &mut rng, 60_000);
        assert_eq!(next, NextWake::At(60_100));

        // 1) remove_peers_if dropped banned Peer 1 before send_tick could request from it;
        // 2) send_tick ran before cleanup, so Peer 2 received outbound requests before the 60 s
        //    session expired;
        // 3) cleanup ran after send_tick, expiring the 60 s session and pruning disconnected Peer 3.
        assert!(!effects.is_empty());
        let sent_peers: Vec<PeerId> = effects
            .iter()
            .map(|effect| match effect {
                SharesEffect::Send {
                    peer,
                    ..
                } => *peer,
                other => panic!("unexpected effect: {:?}", DebugEffect(other)),
            })
            .collect();
        assert_eq!(sent_peers, vec![2]);
        assert!(!mgr.node_states.contains_key(&1));
        assert!(mgr.node_states.contains_key(&2));
        assert!(mgr.node_states[&2].sessions.is_empty());
        assert!(mgr.time_seen_for_sessions.is_empty());
        assert!(!mgr.node_states.contains_key(&3));

        let idle_env = SharesEnv {
            now: Now {
                ms: 65_000,
            },
            ..env
        };
        let (idle_effects, idle_next) =
            cleaning_tick(&mut mgr, &idle_env, |_| false, &mut rng, 65_000);
        assert!(idle_effects.is_empty());
        assert_eq!(idle_next, NextWake::At(65_100));
    }

    #[test]
    fn test_dispatcher_tick_dispatches_pending_signs_and_skips_none_shares() {
        let member_quorum = make_quorum(1, 1, 1);
        let non_member_quorum = make_quorum(2, 1, 50);
        let mut lookup = FakeLookup::default();
        register_quorum(&mut lookup, &member_quorum);
        register_quorum(&mut lookup, &non_member_quorum);
        let recsigs = FakeRecSigs::default();
        let net = FakeNet::default();
        let recovery_calls = Cell::new(0);
        let recovery = counting_recovery(&recovery_calls);
        let signer = SignerFacts {
            my_pro_tx: Some(pro_tx(1)),
            signing_allowed: true,
        };
        let env = SharesEnv {
            lookup: &lookup,
            recsigs: &recsigs,
            net: &net,
            recovery: &recovery,
            signer: &signer,
            sporks: MESH_SPORKS,
            now: Now {
                ms: 700,
            },
            bls: BASIC,
            chain: &CHAIN,
        };

        let mut mgr = SigSharesManager::new();
        mgr.push_pending_sign(PendingSign {
            quorum: Arc::clone(&non_member_quorum),
            id: [1; 32],
            msg_hash: [2; 32],
        });
        mgr.push_pending_sign(PendingSign {
            quorum: Arc::clone(&member_quorum),
            id: [3; 32],
            msg_hash: [4; 32],
        });

        let verifier = CountingVerifier::default();
        let op = FakeOperatorSigner::default();
        let mut rng = SeededRng::new(1);
        let outcome = dispatcher_tick(
            &mut mgr, &env, &verifier, &signer, &op, BASIC, BASIC, &mut rng, 0, 700,
        )
        .unwrap();

        assert_eq!(outcome.next, NextWake::At(710));
        assert_eq!(outcome.batches, 0);
        assert_eq!(outcome.missing_quorum, None);
        assert!(mgr.dispatch_pending_signs().is_empty());
        assert_eq!(outcome.effects.len(), 1);
        assert!(matches!(
            &outcome.effects[0],
            SharesEffect::Recovered(rs) if rs.id == [3; 32] && rs.msg_hash == [4; 32]
        ));
        let expected_sign_hash =
            sign_hash(TEST_TYPE, &member_quorum.quorum_hash, &[3; 32], &[4; 32]);
        assert_eq!(mgr.signed_sessions.len(), 1);
        assert!(mgr.signed_sessions.contains_key(&expected_sign_hash));

        // Multiple valid signs around a None sign preserve push order.
        mgr.push_pending_sign(PendingSign {
            quorum: Arc::clone(&member_quorum),
            id: [31; 32],
            msg_hash: [41; 32],
        });
        mgr.push_pending_sign(PendingSign {
            quorum: Arc::clone(&non_member_quorum),
            id: [99; 32],
            msg_hash: [99; 32],
        });
        mgr.push_pending_sign(PendingSign {
            quorum: Arc::clone(&member_quorum),
            id: [32; 32],
            msg_hash: [42; 32],
        });
        let ordered = dispatcher_tick(
            &mut mgr, &env, &verifier, &signer, &op, BASIC, BASIC, &mut rng, 0, 710,
        )
        .unwrap();
        assert_eq!(ordered.effects.len(), 2);
        assert!(matches!(&ordered.effects[0], SharesEffect::Recovered(rs) if rs.id == [31; 32]));
        assert!(matches!(&ordered.effects[1], SharesEffect::Recovered(rs) if rs.id == [32; 32]));
        assert_eq!(ordered.next, NextWake::At(720));

        // A second SignerFacts with signing_allowed = false also yields None and drains the queue.
        mgr.push_pending_sign(PendingSign {
            quorum: Arc::clone(&member_quorum),
            id: [5; 32],
            msg_hash: [6; 32],
        });
        let disallowed = SignerFacts {
            my_pro_tx: Some(pro_tx(1)),
            signing_allowed: false,
        };
        let outcome2 = dispatcher_tick(
            &mut mgr,
            &env,
            &verifier,
            &disallowed,
            &op,
            BASIC,
            BASIC,
            &mut rng,
            0,
            720,
        )
        .unwrap();
        assert!(outcome2.effects.is_empty());
        assert_eq!(outcome2.next, NextWake::At(730));
        assert!(mgr.dispatch_pending_signs().is_empty());
    }

    #[test]
    fn test_dispatcher_tick_enforces_in_flight_and_batch_size_caps() {
        let q = make_quorum(3, 3, 1);
        let mut fake_lookup = FakeLookup::default();
        register_quorum(&mut fake_lookup, &q);
        let lookup = CountingLookup {
            inner: fake_lookup,
            get_calls: Cell::new(0),
            fail_get: false,
        };
        let recsigs = FakeRecSigs::default();
        let net = FakeNet::default();
        let recovery_calls = Cell::new(0);
        let recovery = counting_recovery(&recovery_calls);
        let signer = SignerFacts {
            my_pro_tx: Some(pro_tx(1)),
            signing_allowed: true,
        };
        let env = SharesEnv {
            lookup: &lookup,
            recsigs: &recsigs,
            net: &net,
            recovery: &recovery,
            signer: &signer,
            sporks: RING_SPORKS,
            now: Now {
                ms: 2_000,
            },
            bls: BASIC,
            chain: &CHAIN,
        };
        let op = FakeOperatorSigner::default();
        let shares: Vec<SigShare> =
            (0..150).map(|i| signed_share(&q, 1, id_from_index(i), [8; 32])).collect();

        // in_flight = 0: collects and verifies 4 batches of 32 shares each (128 total, 22 left).
        let mut mgr0 = SigSharesManager::new();
        enqueue_shares(&mut mgr0, 1, &shares, &env);
        assert_eq!(mgr0.pending_incoming_count(), 150);
        let verifier0 = CountingVerifier {
            use_reference: true,
            ..Default::default()
        };
        let mut rng = SeededRng::new(9);
        let out0 = dispatcher_tick(
            &mut mgr0, &env, &verifier0, &signer, &op, BASIC, BASIC, &mut rng, 0, 2_000,
        )
        .unwrap();
        assert_eq!(verifier0.calls(), 4);
        assert!(verifier0.batch_sizes().iter().all(|&n| n <= MAX_SHARES_PER_BATCH));
        assert_eq!(verifier0.batch_sizes(), vec![32, 32, 32, 32]);
        assert_eq!(out0.batches, 4);
        assert_eq!(out0.missing_quorum, None);
        assert_eq!(out0.next, NextWake::At(2_010));
        assert_eq!(mgr0.pending_incoming_count(), 22);

        // in_flight = 3: budget is 1 batch (32 shares), leaving 118 pending.
        let mut mgr3 = SigSharesManager::new();
        enqueue_shares(&mut mgr3, 1, &shares, &env);
        let verifier3 = CountingVerifier {
            use_reference: true,
            ..Default::default()
        };
        let out3 = dispatcher_tick(
            &mut mgr3, &env, &verifier3, &signer, &op, BASIC, BASIC, &mut rng, 3, 2_020,
        )
        .unwrap();
        assert_eq!(verifier3.calls(), 1);
        assert_eq!(verifier3.batch_sizes(), vec![32]);
        assert_eq!(out3.batches, 1);
        assert_eq!(out3.next, NextWake::At(2_030));
        assert_eq!(mgr3.pending_incoming_count(), 118);

        // in_flight = 4: 0 collect calls and 0 verify calls, while pending signs still dispatch.
        let sign_quorum = make_quorum(4, 1, 1);
        mgr3.push_pending_sign(PendingSign {
            quorum: Arc::clone(&sign_quorum),
            id: [77; 32],
            msg_hash: [88; 32],
        });
        lookup.get_calls.set(0);
        let verifier4 = CountingVerifier {
            use_reference: true,
            ..Default::default()
        };
        let out4 = dispatcher_tick(
            &mut mgr3, &env, &verifier4, &signer, &op, BASIC, BASIC, &mut rng, 4, 2_040,
        )
        .unwrap();
        assert_eq!(lookup.get_calls.get(), 0);
        assert_eq!(verifier4.calls(), 0);
        assert_eq!(out4.batches, 0);
        assert_eq!(out4.next, NextWake::At(2_050));
        assert_eq!(mgr3.pending_incoming_count(), 118);
        assert!(mgr3.dispatch_pending_signs().is_empty());
        assert_eq!(out4.effects.len(), 1);
        assert!(matches!(&out4.effects[0], SharesEffect::Recovered(rs) if rs.id == [77; 32]));
    }

    #[test]
    fn test_dispatcher_tick_stops_when_more_work_is_false_and_skips_empty() {
        let q = make_quorum(5, 3, 1);
        let mut lookup = FakeLookup::default();
        register_quorum(&mut lookup, &q);
        let recsigs = FakeRecSigs::default();
        let net = FakeNet::default();
        let recovery_calls = Cell::new(0);
        let recovery = counting_recovery(&recovery_calls);
        let signer = SignerFacts {
            my_pro_tx: Some(pro_tx(1)),
            signing_allowed: true,
        };
        let env = SharesEnv {
            lookup: &lookup,
            recsigs: &recsigs,
            net: &net,
            recovery: &recovery,
            signer: &signer,
            sporks: RING_SPORKS,
            now: Now {
                ms: 3_000,
            },
            bls: BASIC,
            chain: &CHAIN,
        };
        let op = FakeOperatorSigner::default();

        // Empty manager -> 0 verify calls.
        let mut empty_mgr = SigSharesManager::new();
        let empty_verifier = CountingVerifier::default();
        let mut empty_rng = CountingRng::new(11);
        let empty_out = dispatcher_tick(
            &mut empty_mgr,
            &env,
            &empty_verifier,
            &signer,
            &op,
            BASIC,
            BASIC,
            &mut empty_rng,
            0,
            3_000,
        )
        .unwrap();
        assert_eq!(empty_verifier.calls(), 0);
        assert_eq!(empty_out.batches, 0);
        assert_eq!(empty_out.next, NextWake::At(3_010));

        // Two peers with <= 32 total shares: batch 1 drains everything so more_work = false.
        // Because two node_states remain in the manager, any extra collect_to_verify call after
        // batch 1 would shuffle the two peers and draw from CountingRng a second time.
        for per_peer in [1u32, 16u32] {
            let mut mgr = SigSharesManager::new();
            let peer1_shares: Vec<_> =
                (0..per_peer).map(|i| signed_share(&q, 1, id_from_index(i), [9; 32])).collect();
            let peer2_shares: Vec<_> = (per_peer..per_peer * 2)
                .map(|i| signed_share(&q, 2, id_from_index(i), [9; 32]))
                .collect();
            enqueue_shares(&mut mgr, 1, &peer1_shares, &env);
            enqueue_shares(&mut mgr, 2, &peer2_shares, &env);

            let verifier = CountingVerifier {
                use_reference: true,
                ..Default::default()
            };
            let mut rng = CountingRng::new(23);
            let out = dispatcher_tick(
                &mut mgr, &env, &verifier, &signer, &op, BASIC, BASIC, &mut rng, 0, 3_050,
            )
            .unwrap();
            assert_eq!(verifier.calls(), 1);
            assert_eq!(out.batches, 1);
            assert_eq!(rng.draws, 1, "collect_to_verify must not run again when !more_work");
            assert_eq!(out.next, NextWake::At(3_060));
            assert_eq!(mgr.pending_incoming_count(), 0);
        }
    }

    #[test]
    fn test_dispatcher_tick_missing_quorum_stops_pass_and_next_tick_collects() {
        let missing_q = make_quorum(6, 1, 1);
        let live_q = make_quorum(7, 1, 1);
        let mut lookup = FakeLookup::default();
        let missing_key = register_quorum(&mut lookup, &missing_q);
        register_quorum(&mut lookup, &live_q);

        let recsigs = FakeRecSigs::default();
        let net = FakeNet::default();
        let recovery_calls = Cell::new(0);
        let recovery = counting_recovery(&recovery_calls);
        let signer = SignerFacts {
            my_pro_tx: Some(pro_tx(1)),
            signing_allowed: true,
        };
        let op = FakeOperatorSigner::default();

        // Pick 32 shares of missing_q with sign_hash[0] < 0x80 and 10 shares of live_q with
        // sign_hash[0] >= 0x80 so SigShareMap::get_first pops missing_q's 32 shares in batch 1
        // and leaves live_q's 10 shares queued behind them.
        let mut missing_shares = Vec::new();
        let mut live_shares = Vec::new();
        for idx in 0u32.. {
            if missing_shares.len() < MAX_SHARES_PER_BATCH {
                let s = signed_share(&missing_q, 1, id_from_index(idx), [21; 32]);
                if s.sign_hash().as_bytes()[0] < 0x80 {
                    missing_shares.push(s);
                }
            }
            if live_shares.len() < 10 {
                let s = signed_share(&live_q, 1, id_from_index(10_000 + idx), [22; 32]);
                if s.sign_hash().as_bytes()[0] >= 0x80 {
                    live_shares.push(s);
                }
            }
            if missing_shares.len() == MAX_SHARES_PER_BATCH && live_shares.len() == 10 {
                break;
            }
        }

        let mut mgr = SigSharesManager::new();
        {
            let intake_env = SharesEnv {
                lookup: &lookup,
                recsigs: &recsigs,
                net: &net,
                recovery: &recovery,
                signer: &signer,
                sporks: RING_SPORKS,
                now: Now {
                    ms: 5_000,
                },
                bls: BASIC,
                chain: &CHAIN,
            };
            enqueue_shares(&mut mgr, 1, &missing_shares, &intake_env);
            enqueue_shares(&mut mgr, 1, &live_shares, &intake_env);
        }
        assert_eq!(mgr.pending_incoming_count(), 42);

        // Quorum disappears before collection.
        lookup.quorums.remove(&missing_key);
        let env = SharesEnv {
            lookup: &lookup,
            recsigs: &recsigs,
            net: &net,
            recovery: &recovery,
            signer: &signer,
            sporks: RING_SPORKS,
            now: Now {
                ms: 5_000,
            },
            bls: BASIC,
            chain: &CHAIN,
        };
        let verifier = CountingVerifier {
            use_reference: true,
            ..Default::default()
        };
        let mut rng = SeededRng::new(31);

        let first = dispatcher_tick(
            &mut mgr, &env, &verifier, &signer, &op, BASIC, BASIC, &mut rng, 0, 5_000,
        )
        .unwrap();
        assert_eq!(first.missing_quorum, Some(missing_key));
        assert_eq!(first.batches, 0);
        assert!(first.effects.is_empty());
        assert_eq!(first.next, NextWake::At(5_010));
        assert_eq!(verifier.calls(), 0);
        assert_eq!(recovery_calls.get(), 0);
        assert!(mgr.sig_shares.is_empty());
        assert_eq!(mgr.pending_incoming_count(), 10);

        // Next tick collects and verifies the remaining live_q shares.
        let second = dispatcher_tick(
            &mut mgr, &env, &verifier, &signer, &op, BASIC, BASIC, &mut rng, 0, 5_010,
        )
        .unwrap();
        assert_eq!(second.missing_quorum, None);
        assert_eq!(second.batches, 1);
        assert_eq!(second.next, NextWake::At(5_020));
        assert_eq!(verifier.calls(), 1);
        assert_eq!(recovery_calls.get(), 10);
        assert_eq!(mgr.sig_shares.size(), 10);
        assert_eq!(mgr.pending_incoming_count(), 0);
    }

    #[test]
    fn test_dispatcher_tick_verify_or_lookup_error_stops_and_preserves_prior_effects() {
        let sign_q = make_quorum(8, 1, 1);
        let batch_q = make_quorum(9, 3, 1);
        let mut fake_lookup = FakeLookup::default();
        register_quorum(&mut fake_lookup, &sign_q);
        let batch_key = register_quorum(&mut fake_lookup, &batch_q);

        let recsigs = FakeRecSigs::default();
        let net = FakeNet::default();
        let recovery_calls = Cell::new(0);
        let recovery = counting_recovery(&recovery_calls);
        let signer = SignerFacts {
            my_pro_tx: Some(pro_tx(1)),
            signing_allowed: true,
        };
        let op = FakeOperatorSigner::default();

        // Subcase 1: verify_batch fails on batch 2 after a local sign effect and batch 1's ban.
        let lookup = CountingLookup {
            inner: fake_lookup,
            get_calls: Cell::new(0),
            fail_get: false,
        };
        let env = SharesEnv {
            lookup: &lookup,
            recsigs: &recsigs,
            net: &net,
            recovery: &recovery,
            signer: &signer,
            sporks: RING_SPORKS,
            now: Now {
                ms: 8_000,
            },
            bls: BASIC,
            chain: &CHAIN,
        };
        let mut mgr = SigSharesManager::new();
        mgr.push_pending_sign(PendingSign {
            quorum: Arc::clone(&sign_q),
            id: [41; 32],
            msg_hash: [42; 32],
        });
        let shares: Vec<SigShare> =
            (0..70).map(|i| signed_share(&batch_q, 1, id_from_index(i), [30; 32])).collect();
        enqueue_shares(&mut mgr, 9, &shares, &env);
        assert_eq!(mgr.pending_incoming_count(), 70);

        let verifier = CountingVerifier {
            bad_sources: [9].into(),
            fail_on_call: Some((2, batch_key, 1)),
            ..Default::default()
        };
        let mut rng = SeededRng::new(17);
        let err = dispatcher_tick(
            &mut mgr, &env, &verifier, &signer, &op, BASIC, BASIC, &mut rng, 0, 8_000,
        )
        .unwrap_err();

        assert!(matches!(
            err.kind,
            DispatcherErrorKind::Verify(VerifyError::MissingPubKeyShare {
                key,
                member: 1,
            }) if key == batch_key
        ));
        assert_eq!(verifier.calls(), 2);
        assert_eq!(err.batches, 1);
        assert_eq!(err.next, NextWake::At(8_010));
        assert_eq!(mgr.pending_incoming_count(), 6);
        assert_eq!(err.effects.len(), 2);
        assert!(matches!(&err.effects[0], SharesEffect::Recovered(rs) if rs.id == [41; 32]));
        assert!(matches!(
            &err.effects[1],
            SharesEffect::Ban {
                peer: 9,
                mark_shares_banned: true,
            }
        ));

        // Subcase 2: collect_to_verify lookup error also preserves prior local-sign effects.
        let mut lookup_fail = lookup;
        lookup_fail.fail_get = true;
        mgr.push_pending_sign(PendingSign {
            quorum: Arc::clone(&sign_q),
            id: [51; 32],
            msg_hash: [52; 32],
        });
        let env_fail = SharesEnv {
            lookup: &lookup_fail,
            recsigs: &recsigs,
            net: &net,
            recovery: &recovery,
            signer: &signer,
            sporks: RING_SPORKS,
            now: Now {
                ms: 8_100,
            },
            bls: BASIC,
            chain: &CHAIN,
        };
        let verifier2 = CountingVerifier::default();
        let err2 = dispatcher_tick(
            &mut mgr, &env_fail, &verifier2, &signer, &op, BASIC, BASIC, &mut rng, 0, 8_100,
        )
        .unwrap_err();
        assert!(matches!(err2.kind, DispatcherErrorKind::Lookup("lookup failure")));
        assert_eq!(verifier2.calls(), 0);
        assert_eq!(err2.batches, 0);
        assert_eq!(err2.next, NextWake::At(8_110));
        assert_eq!(err2.effects.len(), 1);
        assert!(matches!(&err2.effects[0], SharesEffect::Recovered(rs) if rs.id == [51; 32]));
    }
}
