//! The facade's mineable pool and its applier (CH7b-2 C6, D14) on `special_txs` (C7):
//! `ProcessCommitment`'s erase and `UndoBlock`'s re-add (`llmq/blockprocessor.cpp` v24:448-453,
//! `:556`; `AddMineableCommitment` `:840-866`), from H4 and H9 through the effect appliers into
//! the pool of the facade the attempts open, which each row inspects.

use std::sync::{Arc, Mutex};

use dash_consensus::primitives::Header;
use dash_consensus::{Block, BlockId, BlsCtx};
use dash_dml::blockprocessor::{QcHashCache, commitment_hash, qc_hashes};
use dash_dml::testkit::{operator_key, operator_sig};
use dash_evo::llmq::{LlmqCommitment, commitment_version};
use dash_params::ChainSetup;
use dash_pkc::bls::{BlsEagerPk, BlsSchemeKind};
use dash_primitives::{BlockHash, BlockHeader, DynBitset, QuorumHash, QuorumVvecHash};
use dash_types::Numeric as _;

use super::llmq_members::{Fixture, TEST, world};
use super::node_evo::{at, connect_on, null_commitments, qc_tx, special_cbtx, tx, undo_block_on};
use super::on_v_binding::seed;
use super::support::{Mined, Rig, RigConfig, branch, with_tip_cx};
use crate::chain::seams::EvoReadCx;
use crate::chain::{EvoView, Fatal};
use crate::connect::SpecialTxFailure;
use crate::evo::effects::FacadeMineable;
use crate::evo::ev1::DmlChecks;
use crate::llmq::facade::MembersFacade;

/// The quorum's base, and the block that mines its commitment: the first of the mining window
/// (LLMQ_TEST's interval 24, window 10 to 18).
const BASE: u32 = 48;
pub(super) const MINED: u32 = 58;
/// LLMQ_TEST's size.
const SIZE: usize = 3;

/// The first `n` of `size` bits set.
pub(super) fn bits(size: usize, n: usize) -> DynBitset {
    let mut data = vec![0u8; size.div_ceil(8)];
    for i in 0..n {
        data[i / 8] |= 1 << (i % 8);
    }
    DynBitset {
        num_bits: u64::try_from(size).expect("small"),
        data,
    }
}

/// How the block at [`MINED`] is connected.
#[derive(Clone, Copy, Debug)]
enum Connect {
    /// Connected and committed.
    Commit,
    /// `fJustCheck` (`ConnectBlock`'s for `TestBlockValidity` and `VerifyDB`).
    JustCheck,
    /// A later commitment of the block, for another type, names a block that is not its
    /// quorum's base, so step 6 rejects the block after it processed `Q`.
    Failed,
}

/// What a run saw: the connect's verdict, the pool after it, and, when the run undid the block,
/// the pool after the undo.
type Seen = (Option<[u8; 32]>, [bool; 3]);

/// The pool's world: regtest with DIP0003, V19 and DIP0024 from 2 and no V20, each DIP0003
/// block's list holding masternodes 1 to 9 up to [`MINED`]'s parent, and the facade the attempts
/// open, whose pool the mineable applier writes.
pub(super) struct Pool {
    p: Arc<ChainSetup>,
    rig: Rig,
    blocks: Vec<Mined>,
    ids: Vec<BlockId>,
    facade: Arc<MembersFacade>,
}

impl Pool {
    pub(super) fn new() -> Pool {
        let p = Arc::new(Fixture::rotated().params);
        let blocks = branch(&p, &Mined::genesis(&p), MINED, 5);
        let rig = Rig::with(RigConfig {
            params: Some(p.clone()),
            ..RigConfig::default()
        });
        rig.headers(&blocks);
        let ids = rig.ids(&blocks);
        let facade = world(&p).facade;
        Pool {
            p,
            rig,
            blocks,
            ids,
            facade,
        }
    }

    fn base_hash(&self) -> BlockHash {
        self.blocks[BASE as usize - 1].hash()
    }

    /// LLMQ_TEST's commitment for the quorum based at [`BASE`], every member valid and the first
    /// `signers` signing: `Verify(checkSigs = false)` holds from 2 (`minSize`), and the signers
    /// decide which of two commitments for the quorum the pool keeps (LQ2 D8).
    pub(super) fn qc(&self, signers: usize) -> LlmqCommitment {
        let scheme = BlsSchemeKind::Basic;
        LlmqCommitment {
            // Not rotated, and V19 active at the base.
            version: commitment_version(false, true),
            llmq_type: TEST,
            quorum_hash: QuorumHash::from(*self.base_hash().as_bytes()),
            quorum_index: None,
            signers: bits(SIZE, signers),
            valid_members: bits(SIZE, SIZE),
            quorum_public_key: BlsEagerPk::new(operator_key(9, scheme).get(), scheme),
            quorum_vvec_hash: QuorumVvecHash::from([0x11; 32]),
            quorum_sig: operator_sig(9, &[1; 32]),
            members_sig: operator_sig(8, &[2; 32]),
        }
    }

    /// The pool before each run: `Q'` (one signer) for the quorum, then `Q` (two), so the
    /// quorum's entry names `Q` and both hashes are held (`:857-859`).
    fn seed(&self) {
        let mut pool = self.facade.mineable();
        pool.add(&self.qc(1));
        pool.add(&self.qc(2));
    }

    /// H4's verdict on the block at [`MINED`] mining `qc` in a type-6 transaction of payload
    /// height `payload_height`, beside the miner's null commitments for the other types, judged
    /// and rolled back. With `check`, the block's CbTx checks and signature checks run (a CbTx
    /// coinbase then). A step-4 reject is labelled, as it carries its transaction (`stm:957-958`).
    pub(super) fn verdict(&self, qc: LlmqCommitment, payload_height: u32, check: bool) -> String {
        let (p, blocks, ids) = (self.p.clone(), self.blocks.clone(), self.ids.clone());
        let facade = self.facade.clone();
        let out = Arc::new(Mutex::new(None));
        let put = out.clone();
        self.rig.on_v(move |v| {
            let applier = Box::new(FacadeMineable(facade.clone()));
            let (mut evo, layer, store) =
                super::node_evo::node_evo_with_mineable(&p, BlsCtx::BASIC, DmlChecks, applier);
            let n = MINED as usize - 1;
            seed(&store, &layer, &blocks[..n]);
            let at = at(&blocks[n], ids[n], ids[n - 1]);
            let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
                let mined = null_commitments(cx, &store, &layer, ids[n - 1], MINED);
                let coinbase = if check {
                    special_cbtx(2, MINED, 0, 0)
                } else {
                    tx(1, 0, true, 0)
                };
                let mut txs = vec![coinbase];
                txs.extend(mined.iter().map(|other| match other.llmq_type {
                    TEST => qc_tx(payload_height, &qc),
                    _ => qc_tx(MINED, other),
                }));
                let body = Block::new(Header::new(BlockHeader::default()), txs);
                let h4 = connect_on(&mut evo, &facade, vcx, cx, &body, &at, false, check, false)?;
                Ok(match h4 {
                    Ok(Ok(_)) => "connects".to_owned(),
                    Ok(Err(SpecialTxFailure::Invalid(e)))
                        if e.debug_message().starts_with("Special Transaction check failed") =>
                    {
                        format!("step 4: {}", e.reject_reason())
                    }
                    Ok(Err(SpecialTxFailure::Invalid(e))) => e.reject_reason().to_owned(),
                    other => format!("{other:?}"),
                })
            })?;
            *put.lock().expect("out") = Some(r);
            Ok(())
        });
        let r = out.lock().expect("out").take().expect("V ran it");
        self.rig.assert_no_abort();
        r
    }

    /// The commitment the quorum's entry names, and whether `Q'`, `Q` and `Q''` (three signers)
    /// are held by hash.
    fn seen(facade: &MembersFacade, qcs: &[LlmqCommitment; 3]) -> Seen {
        let pool = facade.mineable();
        let entry = pool.best_for(TEST, &qcs[1].quorum_hash).map(|qc| commitment_hash(qc));
        (entry, qcs.each_ref().map(|qc| pool.has(&commitment_hash(qc))))
    }

    fn hash(&self, signers: usize) -> Option<[u8; 32]> {
        Some(commitment_hash(&self.qc(signers)))
    }

    /// On V, over a fresh `NodeEvo` (DmlChecks, the facade's pool as its mineable applier)
    /// committed to each list below [`MINED`]: H4 on the block at [`MINED`] mining `Q` beside the
    /// miner's null commitments for the other types, as `connect` has it, and the pool after it;
    /// then, with `undo` (and `Q''` added first when `better`), H9 on it and the pool after.
    fn run(&self, connect: Connect, undo: bool, better: bool) -> (String, Seen, Option<Seen>) {
        let (p, blocks, ids) = (self.p.clone(), self.blocks.clone(), self.ids.clone());
        let facade = self.facade.clone();
        let qcs = [self.qc(1), self.qc(2), self.qc(3)];
        let out = Arc::new(Mutex::new(None));
        let put = out.clone();
        self.rig.on_v(move |v| {
            let applier = Box::new(FacadeMineable(facade.clone()));
            let (mut evo, layer, store) =
                super::node_evo::node_evo_with_mineable(&p, BlsCtx::BASIC, DmlChecks, applier);
            let n = MINED as usize - 1;
            seed(&store, &layer, &blocks[..n]);
            let m = &blocks[n];
            let at = at(m, ids[n], ids[n - 1]);
            let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
                let mut mined = null_commitments(cx, &store, &layer, ids[n - 1], MINED);
                let ours = mined.iter().position(|qc| qc.llmq_type == TEST).expect("TEST's window");
                mined[ours] = qcs[1].clone();
                if let Connect::Failed = connect {
                    let later =
                        mined.iter_mut().max_by_key(|qc| qc.llmq_type.to_base()).expect("one");
                    assert!(later.llmq_type.to_base() > TEST.to_base(), "a later type");
                    later.quorum_hash = QuorumHash::from(*blocks[n - 1].hash().as_bytes());
                }
                let mut txs = vec![tx(1, 0, true, 0)];
                txs.extend(mined.iter().map(|qc| qc_tx(MINED, qc)));
                let body = Block::new(Header::new(BlockHeader::default()), txs);
                let (just_check, commit) = match connect {
                    Connect::Commit => (false, true),
                    Connect::JustCheck => (true, false),
                    Connect::Failed => (false, false),
                };
                let h4 =
                    connect_on(&mut evo, &facade, vcx, cx, &body, &at, just_check, false, commit)?;
                let verdict = match h4 {
                    Ok(Ok(_)) => "connects".to_owned(),
                    Ok(Err(SpecialTxFailure::Invalid(e))) => e.reject_reason().to_owned(),
                    other => format!("{other:?}"),
                };
                let connected = Pool::seen(&facade, &qcs);
                let undone = if undo {
                    if better {
                        facade.mineable().add(&qcs[2]);
                    }
                    let parent = blocks[n - 1].hash();
                    let r = undo_block_on(&mut evo, &facade, vcx, cx, &body, &at, &parent)?;
                    assert_eq!(r.map_err(|f| f.0), Ok(true), "H9 undid it");
                    Some(Pool::seen(&facade, &qcs))
                } else {
                    None
                };
                Ok((verdict, connected, undone))
            })?;
            *put.lock().expect("out") = Some(r);
            Ok(())
        });
        let r = out.lock().expect("out").take().expect("V ran it");
        self.rig.assert_no_abort();
        r
    }

    /// The commitment hashes `with_qc` reads as active after the block at [`MINED`]'s parent and
    /// after the block itself, once H4 has committed it mining `Q`.
    fn with_qc_around_the_mined_block(&self) -> [Option<Vec<[u8; 32]>>; 2] {
        let (p, blocks, ids) = (self.p.clone(), self.blocks.clone(), self.ids.clone());
        let facade = self.facade.clone();
        let qc = self.qc(2);
        let out = Arc::new(Mutex::new(None));
        let put = out.clone();
        self.rig.on_v(move |v| {
            let applier = Box::new(FacadeMineable(facade.clone()));
            let (mut evo, layer, store) =
                super::node_evo::node_evo_with_mineable(&p, BlsCtx::BASIC, DmlChecks, applier);
            let n = MINED as usize - 1;
            seed(&store, &layer, &blocks[..n]);
            let at = at(&blocks[n], ids[n], ids[n - 1]);
            let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
                let mut mined = null_commitments(cx, &store, &layer, ids[n - 1], MINED);
                let ours = mined.iter().position(|q| q.llmq_type == TEST).expect("TEST's window");
                mined[ours] = qc.clone();
                let mut txs = vec![tx(1, 0, true, 0)];
                txs.extend(mined.iter().map(|q| qc_tx(MINED, q)));
                let body = Block::new(Header::new(BlockHeader::default()), txs);
                let h4 = connect_on(&mut evo, &facade, vcx, cx, &body, &at, false, false, true)?;
                assert!(matches!(h4, Ok(Ok(_))), "{h4:?}");
                let read = EvoReadCx {
                    reader: cx.reader,
                    deployments: cx.deployments,
                };
                let view = EvoView::over(&evo, read, Some(ids[n]));
                let bls = dash_consensus::ctx::bls_ctx_for_block(&p, MINED + 1);
                let active = |prev: BlockId| {
                    view.with_qc(bls, |qc| {
                        let mut cache = QcHashCache::new();
                        qc_hashes(&qc.llmq, qc.mined, &mut cache, prev).expect("reads")
                    })
                    .expect("evo state")
                    .plain
                    .get(&TEST.to_base())
                    .cloned()
                };
                Ok([active(ids[n - 1]), active(ids[n])])
            })?;
            *put.lock().expect("out") = Some(r);
            Ok(())
        });
        let r = out.lock().expect("out").take().expect("V ran it");
        self.rig.assert_no_abort();
        r
    }
}

/// MN0-2b-2 commit 1 (OD-CP-19 Q1): `with_qc`'s `mined` is the committed layer's. After H4
/// commits the block at [`MINED`] mining `Q`, `GetQcHashes` through it names `Q` after that
/// block, and nothing for the type after its parent.
#[test]
fn with_qc_reads_the_committed_mined_commitment() {
    let p = Pool::new();
    let got = p.with_qc_around_the_mined_block();
    assert_eq!(got, [None, Some(vec![commitment_hash(&p.qc(2))])]);
}

/// Row 1: a connect erases the quorum's entry and `Q`'s hash; `Q'`'s stays, since Core erases
/// only the mined commitment's (P-1). Row 4: undoing it re-adds `Q` through `add`, so the entry
/// names `Q` again and its hash is back.
#[test]
fn a_connect_erases_the_mined_commitment_and_its_undo_re_adds_it() {
    let p = Pool::new();
    p.seed();
    let (verdict, connected, undone) = p.run(Connect::Commit, true, false);
    assert_eq!(verdict, "connects");
    assert_eq!(connected, (None, [true, false, false]), "row 1");
    assert_eq!(undone, Some((p.hash(2), [true, true, false])), "row 4");
}

/// Row 2: a block that fails after step 6 processed `Q` still erased it, as Core's
/// `ProcessCommitment` did before the reject: the attempt's log applies on every outcome (D33).
/// The later commitment's quorum hash names the parent, so `ProcessCommitment` rejects it
/// (`:360-367`, `bad-qc-block`) after `Q`'s, the block's commitments going by type.
#[test]
fn a_block_failing_after_step_6_keeps_the_erase() {
    let p = Pool::new();
    p.seed();
    let (verdict, connected, _) = p.run(Connect::Failed, false, false);
    assert_eq!(verdict, "bad-qc-block");
    assert_eq!(connected, (None, [true, false, false]), "row 2");
}

/// Row 3: `ProcessBlock` with `fJustCheck` returns before the erase (`:417-419`), so the pool is
/// as seeded (P-3).
#[test]
fn a_check_only_connect_leaves_the_pool() {
    let p = Pool::new();
    p.seed();
    let (verdict, connected, _) = p.run(Connect::JustCheck, false, false);
    assert_eq!(verdict, "connects");
    assert_eq!(connected, (p.hash(2), [true, true, false]), "row 3");
}

/// Row 5: an undo under a better entry, `Q''` with more signers than `Q`, leaves the entry, and
/// `Q`'s hash stays absent: Core's `AddMineableCommitment` returns without inserting (`:855` not
/// taken, `:863`), as `add` does (P-2).
#[test]
fn an_undo_under_a_better_entry_leaves_it() {
    let p = Pool::new();
    p.seed();
    let (_, _, undone) = p.run(Connect::Commit, true, true);
    assert_eq!(undone, Some((p.hash(3), [true, false, true])), "row 5");
}
