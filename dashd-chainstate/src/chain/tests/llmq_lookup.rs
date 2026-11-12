//! Route C's quorum lookup for types 7 and 9 (CH7b-2 C5; D4, D92): MNHF signals and asset
//! unlocks through dash-dml's `CheckSpecialTxInner`, with [`AttemptQuorums`] bound over a facade
//! inside an attempt on a real V loop, for the block after the main chain's tip. Each verdict is
//! Core's at the cited line. C13: the asset unlock's scan, run cold, never dispatches to V.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::{Arc, mpsc};

use dash_consensus::hash::sha256d;
use dash_consensus::primitives::{Transaction, Tx, TxOut};
use dash_consensus::validation::TxValidationResult;
use dash_consensus::{BlockId, ChainAncestry as _, ConsensusCtx};
use dash_dml::blockprocessor::{BlockLookup, QcBlockError};
use dash_dml::creditpool::RangesSet;
use dash_dml::mnhf::is_valid_mn_activation;
use dash_dml::specialtx::qc::CommitmentVerifier;
use dash_dml::specialtx::quorum::{MAX_QUORUM_SCAN, QuorumLookup, QuorumScan, SeamError};
use dash_dml::specialtx::sig::SerialVerifier;
use dash_dml::specialtx::{QuorumSeams, SpecialTxCtx, SpecialTxRules, check_special_tx_inner};
use dash_dml::store::MinedCommitmentRecord;
use dash_dml::testkit::replay::Coins;
use dash_dml::testkit::{MemChain, operator_key, operator_sig};
use dash_dml::{MemEvoStore, MnList};
use dash_evo::EffectSink;
use dash_evo::bls_ctx_after;
use dash_evo::ctx::EvoCtx;
use dash_evo::llmq::{LlmqCommitment, scan_store_block};
use dash_evo::locks::sign_hash;
use dash_params::ChainSetup;
use dash_params::LlmqType;
use dash_pkc::bls::{BlsEagerPk, BlsGlobalSig, BlsSchemeKind};
use dash_primitives::{AssetUnlock, BlockHash, MnHardFork, QuorumHash, TxType};
use dash_types::codec::BaseCodec;

use super::llmq::WAIT;
use super::llmq_members::{Buried, Fixture, TIP, World, in_attempt, on_v, world};
use super::support::Rig;
use crate::chain::ChainCommand;
use crate::chain::actor::V;
use crate::connect::NoCheckpoints;
use crate::evo::node::node_net_info;
use crate::evo::testkit;
use crate::llmq::quorum::{AttemptQuorums, NoQuorumHook};

const C: TxValidationResult = TxValidationResult::Consensus;
const S: TxValidationResult = TxValidationResult::BadSpecial;

/// A version bit no deployment of the chain uses, so `IsValidMNActivation` holds
/// (`mnhftx.cpp:124-126`).
const FREE_BIT: u8 = 5;

type Verdict = Result<(), (TxValidationResult, String)>;

/// The chain `0 ..= TIP` with V20 and DIP0024 from 2, a fork of 10 blocks off 80, and the mined
/// rows: the platform type's quorums based at 24, 48, 72 and 96, each mined ten blocks later
/// under keys 2 to 5; the MNHF type's quorum based at 48, mined at 59 under key 1; and its quorum
/// based at 72, mined only at the fork's 82, under key 6.
struct Chain {
    fx: Fixture,
    fork: BlockId,
    store: MemEvoStore,
    w: World,
}

impl Chain {
    fn new() -> Chain {
        let mut fx = Fixture::new(2, 2);
        let fork = fx.fork(80, 10);
        let (platform, mnhf) = (platform(&fx.params), mnhf_type(&fx.params));
        let mut store = MemEvoStore::new();
        for (base, key) in [(24, 2), (48, 3), (72, 4), (96, 5)] {
            mine(&mut store, &fx, fx.at(base + 10), record(&fx, platform, fx.at(base), key));
        }
        mine(&mut store, &fx, fx.at(59), record(&fx, mnhf, fx.at(48), 1));
        let at82 = fx.chain().ancestor(fork, 82).expect("on the fork");
        mine(&mut store, &fx, at82, record(&fx, mnhf, fx.at(72), 6));
        let w = world(&fx.params);
        Chain {
            fx,
            fork,
            store,
            w,
        }
    }

    fn quorum(&self, base: u32) -> QuorumHash {
        QuorumHash::from(*self.fx.hash(base).as_bytes())
    }

    /// Every block, main chain and fork.
    fn index(&self) -> Index {
        Index::of(self.fx.chain(), self.fork)
    }

    /// `CheckSpecialTxInner(tx)` for the block after the main chain's tip, in one attempt, with
    /// route C's lookup bound to the main chain over `lookup` (the seams' `LookupBlockIndex` has
    /// every block).
    fn check(&self, v: &mut V, tx: &Tx, lookup: &Index) -> Verdict {
        let fx = &self.fx;
        let tip = fx.at(TIP);
        let deployments = Buried {
            chain: fx.chain(),
            params: &fx.params,
        };
        let cons = ConsensusCtx {
            params: &fx.params,
            height: TIP + 1,
            prev: Some(tip),
            chain: fx.chain(),
            deployments: &deployments,
            checkpoints: &NoCheckpoints,
            bls: bls_ctx_after(&deployments, Some(tip)),
            median_time_past: 0,
            header_check_time: 0,
        };
        let index = self.index();
        let list = MnList::new(fx.hash(TIP), i32::try_from(TIP).expect("small"), 1);
        let coins = Coins::new(&[], None);
        let unlocked = RangesSet::default();
        let verdict = fx.ctx(|ctx| {
            let src = fx.source();
            let view = self.store.view(fx.chain());
            in_attempt(v, &self.w, |h, log| {
                let mut quorums = AttemptQuorums {
                    cap: h,
                    ctx,
                    src: &src,
                    mined: &view,
                    index: lookup,
                    tip,
                    hook: &NoQuorumHook,
                };
                let mut cx = SpecialTxCtx::<Infallible> {
                    evo: EvoCtx {
                        cons: &cons,
                        check_sigs: true,
                    },
                    rules: SpecialTxRules {
                        v24: true,
                        evo_shares: false,
                        ipv4_optional: false,
                    },
                    list: &list,
                    coins: &coins,
                    net_info: node_net_info(&fx.params, false).expect("net info rules"),
                    verifier: &SerialVerifier,
                    quorum: Some(QuorumSeams {
                        llmq: *ctx,
                        index: &index,
                        commitments: &NoCommitments,
                        quorums: &mut quorums,
                        effects: log,
                    }),
                    unlock_indexes: Some(&unlocked),
                };
                match check_special_tx_inner(tx, tx.payload(), &mut cx) {
                    Ok(verdict) => verdict.map_err(|e| (e.result(), e.reject_reason().to_owned())),
                    Err(fault) => panic!("a fault: {fault:?}"),
                }
            })
        });
        verdict.expect("released").0
    }

    /// Route C's `f` in one attempt, bound to the chain ending at `tip`.
    fn lookup<R>(
        &self,
        v: &mut V,
        tip: BlockId,
        f: impl FnOnce(&mut dyn QuorumLookup, &mut dyn EffectSink) -> R,
    ) -> R {
        let fx = &self.fx;
        let index = self.index();
        let got = fx.ctx(|ctx| {
            let src = fx.source();
            let view = self.store.view(fx.chain());
            in_attempt(v, &self.w, |h, log| {
                let mut quorums = AttemptQuorums {
                    cap: h,
                    ctx,
                    src: &src,
                    mined: &view,
                    index: &index,
                    tip,
                    hook: &NoQuorumHook,
                };
                f(&mut quorums, log)
            })
        });
        got.expect("released").0
    }
}

fn platform(params: &ChainSetup) -> LlmqType {
    params.consensus.llmq_type_platform
}

fn mnhf_type(params: &ChainSetup) -> LlmqType {
    params.consensus.llmq_type_mnhf
}

/// `t`'s commitment for the quorum based at `base`, its `quorumPublicKey` the basic-scheme key of
/// `operator_key(key)`.
fn record(fx: &Fixture, t: LlmqType, base: BlockId, key: u8) -> MinedCommitmentRecord {
    let mut record = testkit::mined(fx.chain(), t, base, None);
    let mut qc = LlmqCommitment::clone(&record.commitment);
    let scheme = BlsSchemeKind::Basic;
    qc = dash_primitives::Commitment::new(
        qc.version(),
        qc.llmq_type(),
        qc.quorum_hash(),
        qc.quorum_index(),
        qc.signers().clone(),
        qc.valid_members().clone(),
        BlsEagerPk::new(operator_key(key, scheme).get(), scheme),
        qc.quorum_vvec_hash(),
        qc.quorum_sig().clone(),
        qc.members_sig().clone(),
    );
    record.commitment = Arc::new(qc);
    record
}

/// `record` mined in the block `at`.
fn mine(store: &mut MemEvoStore, fx: &Fixture, at: BlockId, record: MinedCommitmentRecord) {
    let mut d = testkit::delta(fx.chain(), at);
    d.mined_commitments = vec![record];
    store.apply(&d).expect("a mined row");
}

/// `LookupBlockIndex` over a block tree.
pub(super) struct Index(BTreeMap<BlockHash, (BlockId, u32)>);

impl Index {
    /// The blocks of `chain` up to `last`, the block made last.
    pub(super) fn of(chain: &MemChain, last: BlockId) -> Index {
        Index(
            (0..=last.0)
                .map(BlockId)
                .map(|id| (chain.hash(id), (id, chain.header(id).height)))
                .collect(),
        )
    }

    /// The index without `hash`.
    fn without(mut self, hash: &BlockHash) -> Index {
        self.0.remove(hash);
        self
    }
}

impl BlockLookup for Index {
    fn lookup(&self, hash: &BlockHash) -> Option<(BlockId, u32)> {
        self.0.get(hash).copied()
    }
}

/// Type 6's verifier, which types 7 and 9 never call.
struct NoCommitments;

impl CommitmentVerifier for NoCommitments {
    fn verify_null(&self, _: &LlmqCommitment) -> bool {
        unreachable!("types 7 and 9 verify no commitment")
    }

    fn verify(
        &self,
        _: &LlmqCommitment,
        _: BlockId,
        _: &mut dyn EffectSink,
    ) -> Result<bool, QcBlockError> {
        unreachable!("types 7 and 9 verify no commitment")
    }
}

/// A version-3 transaction of `tx_type` with no inputs, `outputs` and `payload`.
#[allow(clippy::disallowed_methods, reason = "a test payload encoded into its transaction")]
fn special<T: BaseCodec>(tx_type: TxType, outputs: Vec<TxOut>, payload: &T) -> Tx {
    let mut extra_payload = Vec::new();
    payload.encode(&mut extra_payload);
    Tx::new(Transaction::new(3, tx_type, Vec::new(), outputs, 0, extra_payload))
}

/// The signal for [`FREE_BIT`] naming `quorum`, signed by `operator_key(key)` over Core's
/// `SignHash` (`mnhftx.cpp:139-153`).
fn mnhf(params: &ChainSetup, quorum: QuorumHash, key: u8) -> Tx {
    let mut p = MnHardFork::new(1, FREE_BIT, quorum, operator_sig(0, &[0; 32]));
    let msg = sha256d(&p.sig_hash_bytes(special(TxType::MnhfSignal, Vec::new(), &p).raw()));
    let hash = sign_hash(mnhf_type(params), &quorum, p.request_id().as_bytes(), &msg);
    p = dash_primitives::MnHardFork::new(
        p.version(),
        p.version_bit(),
        p.quorum_hash(),
        operator_sig(key, &hash),
    );
    special(TxType::MnhfSignal, Vec::new(), &p)
}

fn global(bytes: &[u8; 96]) -> BlsGlobalSig {
    BlsGlobalSig::decode(bytes).expect("readable")
}

/// A version-2 unlock of index 7 requested at `requested`, naming `quorum`, signed by
/// `operator_key(key)` over Core's `SignHash` (`evo/assetlocktx.cpp:222-229`, `:144-147` at
/// v24.0.0).
fn unlock(params: &ChainSetup, quorum: QuorumHash, requested: u32, key: u8) -> Tx {
    let script = [&[0x76, 0xa9, 0x14][..], &[9; 20], &[0x88, 0xac]].concat();
    let outputs = || vec![TxOut::from_duffs(1_000, script.clone().into())];
    let mut p = AssetUnlock::new(2, 7, 1_000, requested, quorum, global(&[0; 96]));
    let msg = sha256d(&p.sig_hash_bytes(special(TxType::AssetUnlock, outputs(), &p).raw()));
    let hash = sign_hash(platform(params), &quorum, p.request_id().as_bytes(), &msg);
    p = dash_primitives::AssetUnlock::new(
        p.version(),
        p.index(),
        p.fee(),
        p.requested_height(),
        p.quorum_hash(),
        global(&operator_sig(key, &hash).to_bytes(BlsSchemeKind::Basic)),
    );
    special(TxType::AssetUnlock, outputs(), &p)
}

fn reject(class: TxValidationResult, reason: &str) -> Verdict {
    Err((class, reason.to_owned()))
}

/// `CheckMNHFTxImpl` (`evo/mnhftx.cpp:106-157` at v24.0.0, as are the bare cites below) with
/// route C's `GetQuorum(type, hash, chain)`:
/// - an unknown hash, and a block off the chain, are dash-dml's `bad-mnhf-quorum-hash` (`:130`,
///   `:135`), before any lookup;
/// - a quorum no block mined, and one mined only on the fork, whose object the fork-bound lookup
///   has cached, are `bad-mnhf-missing-quorum` (`:148`): `HasQuorum(chain)` comes before the
///   cache (`quorumsman.cpp:555` at v24.0.0; Q-1);
/// - the mined quorum's signature verifies against the commitment's key (`evo/mnhftx.cpp:152`
///   at v24.0.0); another key's is `bad-mnhf-invalid` (`evo/mnhftx.cpp:153` at v24.0.0).
#[test]
fn mnhf_signals_take_cores_verdicts_through_route_c() {
    on_v(|v| {
        let c = Chain::new();
        let p = &c.fx.params;
        assert!(is_valid_mn_activation(p, FREE_BIT, 0));
        let fork_bound = c.lookup(v, c.fork, |q, fx| q.get_quorum(mnhf_type(p), &c.quorum(72), fx));
        assert!(fork_bound.expect("held").is_some(), "the fork mined it");
        let all = c.index();
        let off_chain = c.fx.chain().ancestor(c.fork, 85).expect("on the fork");
        let off_chain = QuorumHash::from(*c.fx.chain().hash(off_chain).as_bytes());
        let rows: [(&str, Tx, Verdict); 6] = [
            ("unknown", mnhf(p, QuorumHash::from([7; 32]), 1), reject(C, "bad-mnhf-quorum-hash")),
            ("off the chain", mnhf(p, off_chain, 1), reject(C, "bad-mnhf-quorum-hash")),
            ("not mined", mnhf(p, c.quorum(24), 1), reject(C, "bad-mnhf-missing-quorum")),
            ("mined on the fork", mnhf(p, c.quorum(72), 6), reject(C, "bad-mnhf-missing-quorum")),
            ("mined", mnhf(p, c.quorum(48), 1), Ok(())),
            ("another key", mnhf(p, c.quorum(48), 2), reject(C, "bad-mnhf-invalid")),
        ];
        for (row, tx, want) in rows {
            assert_eq!(c.check(v, &tx, &all), want, "{row}");
        }
        let quorums = c.w.facade.test_quorums().expect("free");
        assert!(quorums.objects.peek(mnhf_type(p), &c.fx.hash(48)).is_some());
        assert_eq!(quorums.scans.cache(mnhf_type(p)).map(|s| s.len()), Some(0), "no scan");
    });
}

/// `VerifyAssetUnlockSig` (`evo/assetlocktx.cpp:106-153`) with route C's chain overloads, the
/// platform type scanning `signingActiveQuorumCount + 1` quorums at `pindexPrev`:
/// - the scan's newest three verify (`:147-152` at v24.0.0), and another key is
///   `bad-assetunlock-not-verified`;
/// - the oldest quorum, past the scan, is `bad-assetunlock-too-old-quorum` (`:128-129` at
///   v24.0.0);
/// - a requested height above `pindexPrev`'s is `bad-assetunlock-too-late` (`:132-135` at
///   v24.0.0);
/// - a scanned quorum `GetQuorum` cannot find is `bad-assetunlock-quorum-internal-error`
///   (`:138-142` at v24.0.0), here a lookup index without its block;
/// - the scan never reads or writes the scan cache (`quorumsman.cpp:246-248`, `:283` at
///   v24.0.0; Q-4): a stale entry at the scan's store block, which a cached scan would answer,
///   stays as it was and no other is written.
#[test]
fn asset_unlocks_take_cores_verdicts_through_route_c() {
    on_v(|v| {
        let c = Chain::new();
        let p = &c.fx.params;
        let t = platform(p);
        let llmq = p.consensus.llmq(t).expect("configured");
        let count = usize::from(llmq.signing_active_quorum_count) + 1;
        assert_eq!(count, 3, "the four quorums leave the oldest out");
        let tip = c.fx.at(TIP);
        let store = c.fx.ctx(|ctx| scan_store_block(ctx, t, tip, count).expect("held"));
        let store = c.fx.chain().hash(store.expect("the type is enabled"));
        let q24 = c.lookup(v, tip, |q, fx| q.get_quorum(t, &c.quorum(24), fx));
        assert!(q24.expect("held").is_some());
        let stale =
            c.w.facade.test_quorums().expect("free").objects.peek(t, &c.fx.hash(24)).cloned();
        let stale = vec![stale.expect("cached"); count];
        c.w.facade.test_quorums().expect("free").scans.insert(t, store, stale);

        let all = c.index();
        let rows: [(&str, Tx, Verdict); 6] = [
            ("newest", unlock(p, c.quorum(96), 115, 5), Ok(())),
            ("oldest scanned", unlock(p, c.quorum(48), 115, 3), Ok(())),
            (
                "another key",
                unlock(p, c.quorum(96), 115, 9),
                reject(C, "bad-assetunlock-not-verified"),
            ),
            (
                "too old",
                unlock(p, c.quorum(24), 115, 2),
                reject(S, "bad-assetunlock-too-old-quorum"),
            ),
            (
                "too late",
                unlock(p, c.quorum(96), TIP + 1, 5),
                reject(S, "bad-assetunlock-too-late"),
            ),
            ("expired", unlock(p, c.quorum(96), 72, 5), reject(S, "bad-assetunlock-too-late")),
        ];
        for (row, tx, want) in rows {
            assert_eq!(c.check(v, &tx, &all), want, "{row}");
        }
        let lost = c.index().without(&c.fx.hash(96));
        let tx = unlock(p, c.quorum(96), 115, 5);
        let want = reject(C, "bad-assetunlock-quorum-internal-error");
        assert_eq!(c.check(v, &tx, &lost), want, "a scanned quorum GetQuorum misses");

        let quorums = c.w.facade.test_quorums().expect("free");
        let scans = quorums.scans.cache(t).expect("configured");
        assert_eq!(scans.len(), 1, "no scan-cache write");
        let kept = scans.peek(&store).expect("the stale entry");
        assert!(kept.iter().all(|q| q.base_hash == c.fx.hash(24)), "unchanged");
    });
}

/// ADD §2.6 6-D's nested validation (CH7b-2 C13): inside an attempt on a real V, an asset
/// unlock's `VerifyAssetUnlockSig` scans the platform type at `pindexPrev` with no quorum object
/// cached, so the chain-bound scan builds its quorums through the attempt's handle: every one
/// mined, as the chain overload scans at least `keepOldConnections` (`quorumsman.cpp:247` at
/// v24.0.0), though the unlock asks for three. The unlock verifies, the attempt releases within
/// the bound, and V's lanes take no command from inside it: V cannot drain them while the attempt
/// runs, so their depth counts every send. The builds' objects are cached (`quorumsman.cpp:110` at
/// v24.0.0).
#[test]
fn a_validation_time_scan_that_misses_finishes_through_the_attempt() {
    let rig = Rig::new();
    let lanes = rig.v.clone();
    let (done, outcome) = mpsc::channel();
    let cmd = ChainCommand::TestUnderV(Box::new(move |v| {
        let c = Chain::new();
        let p = &c.fx.params;
        let t = platform(p);
        let keep = usize::from(p.consensus.llmq(t).expect("configured").keep_old_connections);
        let objects = |c: &Chain| {
            let mut quorums = c.w.facade.test_quorums().expect("free");
            [96, 72, 48, 24].map(|base| quorums.objects.get(t, &c.fx.hash(base)).is_some())
        };
        let cold = objects(&c);
        let depth = || lanes.bounded_lane().len() + lanes.marked_lane_depth();
        let before = depth();
        let verdict = c.check(v, &unlock(p, c.quorum(96), 115, 5), &c.index());
        done.send((keep, cold, before, verdict, depth(), objects(&c))).expect("the test waits");
        Ok(())
    }));
    rig.v.send(cmd).expect("V runs");
    let got = outcome.recv_timeout(WAIT).expect("the attempt releases within the bound");
    let (keep, cold, before, verdict, after, built) = got;
    assert!(keep >= 4, "keepOldConnections {keep} leaves a mined quorum unscanned");
    assert_eq!(cold, [false; 4], "a cached object");
    assert_eq!(verdict, Ok(()), "the newest quorum's signature");
    assert_eq!((before, after), (0, 0), "V's lanes took a command from inside the attempt");
    assert_eq!(built, [true; 4], "the scan's builds");
}

/// A scan above [`MAX_QUORUM_SCAN`] is the caller's error before anything is written (the seam's
/// contract), and a scan's hashes come newest first.
#[test]
fn a_scan_answers_newest_first_and_refuses_above_its_capacity() {
    on_v(|v| {
        let c = Chain::new();
        let t = platform(&c.fx.params);
        let tip = c.fx.at(TIP);
        let (refused, held, scanned) = c.lookup(v, tip, |q, fx| {
            let mut out = QuorumScan::default();
            out.push(QuorumHash::from([1; 32])).expect("room");
            let refused = q.scan_quorums(t, tip, MAX_QUORUM_SCAN + 1, &mut out, fx);
            let held = out.as_slice().to_vec();
            q.scan_quorums(t, tip, 4, &mut out, fx).expect("held");
            (refused, held, out.as_slice().to_vec())
        });
        assert_eq!(refused, Err(SeamError::ScanCapacity));
        assert_eq!(held, [QuorumHash::from([1; 32])], "untouched");
        assert_eq!(scanned, [96, 72, 48, 24].map(|base| c.quorum(base)));
    });
}
