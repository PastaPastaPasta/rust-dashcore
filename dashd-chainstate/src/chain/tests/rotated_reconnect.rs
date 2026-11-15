//! C12 (CH7 §4.1.4, design Q1): a rotated commitment connected through H4, disconnected through
//! H9 and connected again, in the same block or one height later on a competing chain, with a
//! flush between the disconnect and the reconnect or without one. The connect's rows are durable
//! before the disconnect, so without the flush the disconnect's tombstones sit in the layer over
//! RocksDB's rows when the reconnect reads the stored commitment (`bad-qc-dup`) and writes it again
//! (`WriteDerived`): `llmq/blockprocessor.cpp` v24.0.0-rc.4 :381-392 and :432-440, against
//! `UndoBlock`'s erases at :536-546.
//!
//! The reference is dash-dml's `MemEvoStore`, given each connect's row and never an undo: it keeps
//! rows by mining block and answers by ancestry, where the node's height index answers by height
//! alone, as Core's does, and is right only if each undo erased its rows. After each step the mined
//! reads on the active chain equal the reference's. After the disconnect no row of the undone
//! blocks answers at them, in either family, while RocksDB still holds them until a flush.

use std::sync::{Arc, Mutex};

use dash_consensus::primitives::Header;
use dash_consensus::{Block, BlockId, BlsCtx};
use dash_dml::blockprocessor::{QcHashCache, qc_hashes};
use dash_dml::store::MinedCommitmentRecord;
use dash_dml::testkit::{operator_key, operator_sig};
use dash_dml::{EvoDelta, MemEvoStore, StoreError};
use dash_evo::llmq::{
    CompleteChain, LlmqCommitment, LlmqCtx, MinedCommitmentReader, MinedFamily, MinedIndexEntry,
    commitment_version,
};
use dash_evo::sml::QcHashes;
use dash_pkc::bls::{BlsEagerPk, BlsSchemeKind};
use dash_primitives::{BlockHash, BlockHeader, QuorumHash, QuorumVvecHash};
use dashd_store::{KvRead as _, MemoryEngine};

use super::llmq_members::{Fixture, ROTATED, world};
use super::mineable::bits;
use super::node_evo::{
    Store, at, connect_on, node_evo_on_world, null_commitments, qc_tx, tx, undo_block_on,
};
use super::on_v_binding::{FORKS, seed};
use super::support::{Mined, Rig, RigConfig, branch, with_tip_cx};
use crate::chain::{ConnectCx, Fatal, VContext};
use crate::connect::{BlockAt, SpecialTxFailure};
use crate::evo::codec::{index_key, mined_key, split_mined_key};
use crate::evo::ev1::DmlChecks;
use crate::evo::node::{Ancestry, NodeEvo, SharedEvoLayer};
use crate::evo::testkit;
use crate::llmq::facade::MembersFacade;

/// The first cycle whose quorums reach LLMQ_TEST_DIP0024's `min_size` of 4: a quarter is one
/// member (`size` 4), and the three before come from the snapshots H4's `PreComputeQuorumMembers`
/// stored at the cycles 24, 48 and 72 (`bp:223`).
const CYCLE: u32 = 96;
/// The first block of the cycle's mining window (12 to 20), which mines quorum index 0.
const MINED: u32 = 108;
/// Both chains' tip.
const TIP: u32 = 110;
/// Where each step reads: the mining block's parent, the two mining heights, the tip.
const READS: [u32; 4] = [MINED - 1, MINED, MINED + 1, TIP];

fn ix(height: u32) -> usize {
    usize::try_from(height - 1).expect("a small height")
}

/// The blocks from [`MINED`] on, counted from 0.
fn from_mined(height: u32) -> usize {
    usize::try_from(height - MINED).expect("at or past MINED")
}

/// How the undone commitment is mined again.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Reconnect {
    /// The same three blocks.
    Same,
    /// A competing chain off the same parent, whose first block mines the miner's null
    /// commitments and whose second mines the commitment.
    Later,
}

/// LLMQ_TEST_DIP0024's commitment for quorum index 0 of the cycle based at `base`: every member
/// valid and signing, so `Verify(checkSigs = false)` holds over the cycle's four members.
fn rotated_qc(base: &Mined) -> LlmqCommitment {
    let scheme = BlsSchemeKind::Basic;
    LlmqCommitment::new(
        commitment_version(true, true),
        ROTATED,
        testkit::quorum_hash(&base.hash()),
        Some(0),
        bits(4, 4),
        bits(4, 4),
        BlsEagerPk::new(operator_key(9, scheme).get(), scheme),
        QuorumVvecHash::from([0x11; 32]),
        operator_sig(9, &[1; 32]),
        operator_sig(8, &[2; 32]),
    )
}

/// The reference's row for `qc` mined in `b`: what H4's connect of `b` writes (`bp:432-440`).
fn mined_delta(qc: &LlmqCommitment, b: &Mined) -> EvoDelta {
    EvoDelta {
        block: b.hash(),
        height: b.height,
        dml: None,
        mined_commitments: vec![MinedCommitmentRecord {
            llmq_type: ROTATED,
            quorum_hash: qc.quorum_hash(),
            base_height: CYCLE,
            rotation_index: qc.quorum_index(),
            commitment: Arc::new(qc.clone()),
        }],
        credit_data: None,
        credit_pool: None,
        mnhf_signals: None,
        erased_mined: Vec::new(),
        evo_tip: Some(b.hash()),
    }
}

/// The mined reads at one block: `GetMinedCommitment`'s mining block, `mined_until` (the
/// non-rotated family, which a rotated type never writes), the newest two bases at quorum index
/// 0, the type's height-index rows in each family (non-rotated, index 0, index 1), and
/// `GetQcHashes`, the active set `CalcCbTxMerkleRootQuorums` commits to.
#[derive(Debug, PartialEq)]
struct Reads {
    mined: Option<BlockHash>,
    until: Vec<BlockId>,
    by_index: [Option<BlockId>; 2],
    rows: [Vec<MinedIndexEntry>; 3],
    hashes: QcHashes,
}

impl Reads {
    fn at(
        llmq: &LlmqCtx<'_, StoreError>,
        view: &dyn MinedCommitmentReader<Error = StoreError>,
        quorum: &QuorumHash,
        at: BlockId,
    ) -> Reads {
        let families = [
            MinedFamily::NonRotated,
            MinedFamily::Rotated {
                quorum_index: 0,
            },
            MinedFamily::Rotated {
                quorum_index: 1,
            },
        ];
        Reads {
            mined: view.mined_commitment(ROTATED, quorum, at).expect("a read").map(|m| m.1),
            until: view.mined_until(ROTATED, at, 8).expect("a read"),
            by_index: [0, 1]
                .map(|nth| view.nth_mined_by_index(ROTATED, at, 0, nth).expect("a read")),
            rows: families.map(|f| view.mined_index(ROTATED, f, at, u32::MAX, 8).expect("a read")),
            hashes: qc_hashes(llmq, view, &mut QcHashCache::default(), at).expect("the active set"),
        }
    }

    /// No row answers: no mining block, no base, no index row.
    fn none(&self) -> bool {
        self.mined.is_none()
            && self.until.is_empty()
            && self.by_index == [None, None]
            && self.rows.iter().all(Vec::is_empty)
    }
}

/// RocksDB's rows of the quorum: its `llmq_mined` row, and its height-index rows mined at
/// [`MINED`] and at [`MINED`] + 1, each in the non-rotated family, at index 0 and at index 1.
#[derive(Debug, PartialEq)]
struct Durable {
    mined: bool,
    index: [[bool; 3]; 2],
}

impl Durable {
    fn read(store: &Store, quorum: &QuorumHash) -> Durable {
        let held = |key: &[u8]| {
            let (cf, key) = split_mined_key(key);
            store.db().get(cf.cf(), key).expect("a healthy read").is_some()
        };
        let at = |h| [None, Some(0), Some(1)].map(|i| held(&index_key(ROTATED, i, h)));
        Durable {
            mined: held(&mined_key(ROTATED, quorum)),
            index: [at(MINED), at(MINED + 1)],
        }
    }

    /// The rows of the commitment mined at `height`, alone.
    fn mined_at(height: u32) -> Durable {
        let mut index = [[false; 3]; 2];
        index[from_mined(height)][1] = true;
        Durable {
            mined: true,
            index,
        }
    }

    fn none() -> Durable {
        Durable {
            mined: false,
            index: [[false; 3]; 2],
        }
    }
}

/// A chain's blocks and their index entries, by height from 1.
struct Branch {
    blocks: Vec<Mined>,
    ids: Vec<BlockId>,
}

impl Branch {
    fn block(&self, h: u32) -> &Mined {
        &self.blocks[ix(h)]
    }

    fn id(&self, h: u32) -> BlockId {
        self.ids[ix(h)]
    }

    fn at(&self, h: u32) -> BlockAt {
        at(self.block(h), self.id(h), self.id(h - 1))
    }
}

/// What one run saw, each read as the node's and the reference's.
#[derive(Debug)]
struct Run {
    /// At [`READS`] on the main chain once it is connected.
    connected: Vec<(Reads, Reads)>,
    /// At the mining block's parent once the three blocks are undone.
    parent: (Reads, Reads),
    /// The node's at each undone block.
    undone: Vec<Reads>,
    /// RocksDB after the connect's flush, after the undo (and its flush, if the run flushes), and
    /// after the reconnect's flush.
    durable: [Durable; 3],
    /// At [`READS`] on the reconnected chain, before its flush and after it.
    reconnected: Vec<(Reads, Reads)>,
    flushed_again: Vec<(Reads, Reads)>,
}

/// The node's evo state on V: `NodeEvo` over `DmlChecks` with the facade's mineable pool, its
/// committed layer and store, and the facade the attempts open.
struct Node {
    evo: NodeEvo<MemoryEngine, DmlChecks>,
    layer: SharedEvoLayer,
    store: Store,
    facade: Arc<MembersFacade>,
}

impl Node {
    /// H4 on `chain`'s block at `h`, committed: the miner's commitments for it, `qc` in place of
    /// quorum index 0's null. Its body, which its undo takes.
    fn connect(
        &mut self,
        vcx: &mut VContext,
        cx: &ConnectCx<'_>,
        chain: &Branch,
        h: u32,
        qc: Option<&LlmqCommitment>,
    ) -> Result<Block, Fatal> {
        let mut qcs = null_commitments(cx, &self.store, &self.layer, chain.id(h - 1), h);
        if let Some(qc) = qc {
            let null = qcs
                .iter_mut()
                .find(|c| c.llmq_type() == ROTATED && c.quorum_index() == Some(0))
                .ok_or_else(|| Fatal::new(format!("no null for index 0 at {h}")))?;
            *null = qc.clone();
        }
        let mut txs = vec![tx(1, 0, true, 0)];
        txs.extend(qcs.iter().map(|qc| qc_tx(h, qc)));
        let body = Block::new(Header::new(BlockHeader::default()), txs);
        let (facade, at) = (&self.facade, chain.at(h));
        match connect_on(&mut self.evo, facade, vcx, cx, &body, &at, false, false, true)? {
            Ok(Ok(_)) => Ok(body),
            Ok(Err(SpecialTxFailure::Invalid(e))) => {
                Err(Fatal::new(format!("block {h}: {}", e.reject_reason())))
            }
            other => Err(Fatal::new(format!("block {h}: {other:?}"))),
        }
    }

    /// H9 on `chain`'s block at `h`, whose body is `body`.
    fn undo(
        &mut self,
        vcx: &mut VContext,
        cx: &ConnectCx<'_>,
        chain: &Branch,
        h: u32,
        body: &Block,
    ) -> Result<(), Fatal> {
        let parent = chain.block(h - 1).hash();
        let at = chain.at(h);
        match undo_block_on(&mut self.evo, &self.facade, vcx, cx, body, &at, &parent)? {
            Ok(true) => Ok(()),
            other => Err(Fatal::new(format!("the undo of {h}: {other:?}"))),
        }
    }

    fn flush(&self) {
        testkit::flush(&self.store, &mut self.layer.lock());
    }

    /// The node's reads and the reference's at `chain`'s blocks at `heights`.
    fn reads(
        &self,
        (llmq, ancestry): (&LlmqCtx<'_, StoreError>, &Ancestry<'_>),
        model: &MemEvoStore,
        chain: &Branch,
        quorum: &QuorumHash,
        heights: &[u32],
    ) -> Vec<(Reads, Reads)> {
        let view = self.store.view(&self.layer.lock(), ancestry);
        let reference = model.view(ancestry);
        let pair = |h| {
            let at = chain.id(h);
            (Reads::at(llmq, &view, quorum, at), Reads::at(llmq, &reference, quorum, at))
        };
        heights.iter().map(|&h| pair(h)).collect()
    }
}

/// The run on V: the main chain through [`TIP`] mining the commitment at [`MINED`], flushed; its
/// last three blocks undone, flushed with `flushed`; the commitment mined again as `reconnect`
/// has it, then flushed.
fn run(reconnect: Reconnect, flushed: bool) -> Result<Run, String> {
    let p = Arc::new(Fixture::rotated().params);
    let main = branch(&p, &Mined::genesis(&p), TIP, 19);
    let fork = branch(&p, &main[ix(MINED - 1)], TIP - MINED + 1, 23);
    let rig = Rig::with(RigConfig {
        params: Some(p.clone()),
        ..RigConfig::default()
    });
    rig.headers(&main);
    rig.headers(&fork);
    let mut later = main[..ix(MINED)].to_vec();
    later.extend(fork);
    let later = Branch {
        ids: rig.ids(&later),
        blocks: later,
    };
    let main = Branch {
        ids: rig.ids(&main),
        blocks: main,
    };
    let w = world(&p);
    let out = Arc::new(Mutex::new(None));
    let put = out.clone();
    rig.on_v(move |v| {
        let (evo, layer, store) = node_evo_on_world(&p, BlsCtx::BASIC, DmlChecks, &w);
        // The list at DIP0003's block; H4 builds every later one, and its snapshots.
        seed(&store, &layer, &main.blocks[..ix(FORKS + 1)]);
        let mut node = Node {
            evo,
            layer,
            store,
            facade: w.facade,
        };
        let r =
            with_tip_cx(v, |vcx, cx| steps(&mut node, vcx, cx, &main, &later, reconnect, flushed));
        *put.lock().expect("out") = Some(r.map_err(|f| f.0));
        Ok(())
    });
    let r = out.lock().expect("out").take().expect("V ran it");
    rig.assert_no_abort();
    r
}

/// [`run`]'s steps on V: the reference sees each connect that mines the commitment.
fn steps(
    node: &mut Node,
    vcx: &mut VContext,
    cx: &ConnectCx<'_>,
    main: &Branch,
    later: &Branch,
    reconnect: Reconnect,
    flushed: bool,
) -> Result<Run, Fatal> {
    let ancestry = Ancestry(cx.reader);
    let complete = CompleteChain::<StoreError>::new(&ancestry);
    let llmq = LlmqCtx::new(cx.params, &complete, cx.deployments, BlsCtx::BASIC)
        .map_err(|e| Fatal::new(format!("{e:?}")))?;
    let read = (&llmq, &ancestry);
    let qc = rotated_qc(main.block(CYCLE));
    let quorum = qc.quorum_hash();
    let mut model = MemEvoStore::new();
    let mined = |model: &mut MemEvoStore, b: &Mined| {
        model.apply(&mined_delta(&qc, b)).map_err(|e| Fatal::new(e.to_string()))
    };

    let mut bodies = Vec::new();
    for h in FORKS + 1..=TIP {
        let body = node.connect(vcx, cx, main, h, (h == MINED).then_some(&qc))?;
        if h >= MINED {
            bodies.push(body);
        }
    }
    mined(&mut model, main.block(MINED))?;
    let connected = node.reads(read, &model, main, &quorum, &READS);
    node.flush();
    let durable_connected = Durable::read(&node.store, &quorum);

    for h in (MINED..=TIP).rev() {
        node.undo(vcx, cx, main, h, &bodies[from_mined(h)])?;
    }
    let mut parent = node.reads(read, &model, main, &quorum, &[MINED - 1]);
    let undone = node.reads(read, &model, main, &quorum, &[MINED, MINED + 1, TIP]);
    if flushed {
        node.flush();
    }
    let durable_undone = Durable::read(&node.store, &quorum);

    let (chain, at) = match reconnect {
        Reconnect::Same => (main, MINED),
        Reconnect::Later => (later, MINED + 1),
    };
    for h in MINED..=TIP {
        node.connect(vcx, cx, chain, h, (h == at).then_some(&qc))?;
    }
    if reconnect == Reconnect::Later {
        mined(&mut model, chain.block(at))?;
    }
    let reconnected = node.reads(read, &model, chain, &quorum, &READS);
    node.flush();
    let flushed_again = node.reads(read, &model, chain, &quorum, &READS);
    Ok(Run {
        connected,
        parent: parent.pop().expect("one read"),
        undone: undone.into_iter().map(|(node, _)| node).collect(),
        durable: [durable_connected, durable_undone, Durable::read(&node.store, &quorum)],
        reconnected,
        flushed_again,
    })
}

/// One run, with and without the flush after the undo, checked step by step.
fn check(reconnect: Reconnect) {
    for flushed in [false, true] {
        let label = format!("{reconnect:?}, flushed {flushed}");
        let r = run(reconnect, flushed).unwrap_or_else(|e| panic!("{label}: {e}"));
        let mined = &r.connected[1].1;
        assert!(mined.mined.is_some() && mined.by_index[0].is_some(), "{label}: {mined:?}");
        let pairs = r.connected.iter().chain([&r.parent]).chain(&r.reconnected);
        for (node, reference) in pairs.chain(&r.flushed_again) {
            assert_eq!(node, reference, "{label}");
        }
        assert!(r.undone.iter().all(Reads::none), "{label}: {:#?}", r.undone);
        let at = match reconnect {
            Reconnect::Same => MINED,
            Reconnect::Later => MINED + 1,
        };
        let undone = if flushed {
            Durable::none()
        } else {
            Durable::mined_at(MINED)
        };
        assert_eq!(r.durable, [Durable::mined_at(MINED), undone, Durable::mined_at(at)], "{label}");
    }
}

/// Undone and mined again in the same block: the reconnect's stored-commitment read finds the
/// tombstone, not RocksDB's row, and its `WriteDerived` writes what the connect wrote.
#[test]
fn a_rotated_commitment_undone_and_mined_again_in_the_same_block() {
    check(Reconnect::Same);
}

/// Undone and mined one height later on a competing chain: a `llmq_mined` row the undo left
/// names the other block, so the reconnect's `WriteDerived` fails as an inconsistency
/// (`bp:432-434`; the node reads a stored commitment by ancestry, LQ2 D4, so Core's earlier
/// `bad-qc-dup` at `:381-392` never meets it), and an index row it left would answer on the new
/// chain beside the new one.
#[test]
fn a_rotated_commitment_undone_and_mined_a_block_later_on_a_competing_chain() {
    check(Reconnect::Later);
}
