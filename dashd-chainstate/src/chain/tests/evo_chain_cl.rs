//! CH7b-3 C3: step 10's cell, Core's statics `cached_pindex` and `cached_chainlock`
//! (`evo/specialtxman.cpp:147-149` at v24.0.0; CH7 X5, BL-179), on dashd's recorded
//! `feature_llmq_chainlocks` run over the node's connector with `NodeEvo`, EV1's checks and
//! `NodeHelper`. The run has V20 and MN_RR at 200; its first CbTx ChainLock is at 309
//! (difference 0, locking 308), 310 to 329 carry the same lock at differences 1 to 20, and 330
//! a new one. Step 10's verifier is a double that records each call and answers as the test
//! sets it (the real one lands with C4); its parent reads go through a wrapper that records them
//! and can make a block unreadable. C6's strict replays run three more recorded runs the same
//! way ([`Run::of`]).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dash_chain::ChainReader;
use dash_consensus::coins::CoinsView;
use dash_consensus::primitives::{Transaction, decode_block};
use dash_consensus::{Block, BlockId, BlsCtx, Tx, TxValidationError};
use dash_dml::StoreError;
use dash_dml::specialtx::QuorumSeams;
use dash_evo::llmq::BlsSigValue;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::{BlockHash, CoinbaseCommitment};

use super::evo_chain::{Kept, OpenSeams, linked, node_config};
use super::failure::tip;
use super::locks::{cl_rig, intake, record, settle};
use super::support::{Ev, Mined, Rig, RigConfig, mine_version};
use crate::chain::{BlockState, ChainLockSig, ConnectCx, Fatal};
use crate::connect::{ChainHelper, NodeHelper, SpecialTxFailure};
use crate::evo::cl::{ClAttempt, ClPass, ClVerifier};
use crate::evo::ev1::{DmlChecks, Ev1Checks, SpecialTxCtx};
use crate::evo::gates::GateCounts;
use crate::evo::node::EvoBlocks;
use crate::llmq::facade::MembersFacade;
use crate::store::CoinsReadError;

/// dashd's recorded `feature_llmq_chainlocks.py` run.
pub(super) const CHAINLOCKS: &str = "feature_llmq_chainlocks";

/// `test`'s recorded node0.
pub(super) fn node0(test: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../test-vectors/statedigest/regtest-v24")
        .join(test)
        .join("node0")
}

/// `test`'s blocks, genesis first: the O8 recorder's blk records (magic, size, block).
pub(super) fn run_blocks(test: &str) -> Vec<Arc<Block>> {
    let path = node0(test).join("blocks.dat.zst");
    let raw = zstd::decode_all(std::fs::File::open(path).expect("the run")).expect("zstd");
    let (mut rest, mut blocks) = (raw.as_slice(), Vec::new());
    while let Some((record, tail)) = rest.split_at_checked(8) {
        let size = u32::from_le_bytes(record[4..].try_into().expect("four bytes"));
        let (mut bytes, tail) = tail.split_at(usize::try_from(size).expect("a size"));
        blocks.push(Arc::new(decode_block(&mut bytes).expect("a block").into_value()));
        rest = tail;
    }
    blocks
}

/// `test`'s params: regtest at v24 with its node0 `extract.json`'s `chain_args`, which that file
/// lists one per line (the ChainLock run's: `-dip3params=2:2 -testactivationheight=v20@200
/// -testactivationheight=mn_rr@200`).
pub(super) fn run_params(test: &str) -> Arc<ChainSetup> {
    run_params_with(test, &[])
}

/// [`run_params`] with `extra` chain arguments after the run's.
pub(super) fn run_params_with(test: &str, extra: &[&str]) -> Arc<ChainSetup> {
    let extract = std::fs::read_to_string(node0(test).join("extract.json")).expect("extract.json");
    let mut lines = extract.lines().map(str::trim);
    lines.by_ref().find(|l| *l == "\"chain_args\": [").expect("chain_args");
    let args: Vec<&str> = lines
        .take_while(|l| !l.starts_with(']'))
        .map(|l| l.trim_end_matches(',').trim_matches('"'))
        .collect();
    assert!(!args.is_empty(), "{test}: chain_args");
    let mut builder = ChainParamsBuilder::new(Network::Regtest, Release::V24_0);
    for arg in args.into_iter().chain(extra.iter().copied()) {
        let (name, value) = arg.split_once('=').unwrap_or((arg, ""));
        let option = ChainOption::from_name(name).unwrap_or_else(|| panic!("{test}: {name}"));
        builder = builder.arg(option, value);
    }
    Arc::new(builder.build().expect("regtest"))
}

/// Step 10's verifier double: records `(height, cl_height)` for each call and answers `valid`.
struct CountingClVerifier {
    census: Arc<Mutex<Vec<(u32, u32)>>>,
    valid: Arc<AtomicBool>,
}

impl ClVerifier for CountingClVerifier {
    fn verify(
        &self,
        cx: &ConnectCx<'_>,
        attempt: &mut ClAttempt<'_, '_>,
        height: u32,
        _: &BlockHash,
        _: &BlsSigValue,
        _: BlsCtx,
    ) -> Result<Result<bool, StoreError>, Fatal> {
        let at = cx.reader.entry(attempt.prev).expect("the parent's entry").height + 1;
        self.census.lock().expect("census").push((at, height));
        Ok(Ok(self.valid.load(Ordering::SeqCst)))
    }
}

/// The block reads `NodeEvo` makes (step 10's parent, `GetForBlock`'s rebuild), each recorded;
/// the block made unreadable reads as `None`, as a failed `ReadBlockFromDisk` does.
#[derive(Clone, Default)]
struct Watch {
    reads: Arc<Mutex<Vec<BlockId>>>,
    unreadable: Arc<Mutex<Option<BlockId>>>,
}

struct Watched {
    inner: Box<dyn EvoBlocks>,
    watch: Watch,
}

impl EvoBlocks for Watched {
    fn read(&self, reader: &ChainReader, id: BlockId) -> Option<Arc<Block>> {
        self.watch.reads.lock().expect("reads").push(id);
        if *self.watch.unreadable.lock().expect("unreadable") == Some(id) {
            return None;
        }
        self.inner.read(reader, id)
    }
}

/// EV1's checks, with cells the test reads: the root record and the step trace.
struct Counted(Arc<GateCounts>);

impl Ev1Checks for Counted {
    type Payloads<'b> = <DmlChecks as Ev1Checks>::Payloads<'b>;

    fn payloads<'b>(&self, block: &'b Block) -> Self::Payloads<'b> {
        DmlChecks.payloads(block)
    }

    fn check_cbtx(
        &self,
        block: &Block,
        payloads: &Self::Payloads<'_>,
        cx: SpecialTxCtx<'_>,
    ) -> Result<Result<CoinbaseCommitment, SpecialTxFailure>, Fatal> {
        DmlChecks.check_cbtx(block, payloads, cx)
    }

    fn check_tx<'a>(
        &self,
        block: &Block,
        payloads: &Self::Payloads<'_>,
        i: usize,
        cx: SpecialTxCtx<'a>,
        quorum: Option<QuorumSeams<'a>>,
    ) -> Result<Result<(), SpecialTxFailure>, Fatal> {
        DmlChecks.check_tx(block, payloads, i, cx, quorum)
    }

    fn shared_collateral_spends(
        &self,
        tx: &Tx,
        view: &dyn CoinsView<Error = CoinsReadError>,
    ) -> Result<Result<(), TxValidationError>, Fatal> {
        DmlChecks.shared_collateral_spends(tx, view)
    }

    fn gate_counts(&self) -> Option<Arc<GateCounts>> {
        Some(self.0.clone())
    }
}

/// A rig over the run with SPORK_19 on, and what the test reads beside it.
struct Run {
    rig: Rig,
    p: Arc<ChainSetup>,
    /// Element `i` is at height `i + 1`.
    blocks: Vec<Mined>,
    facade: Arc<MembersFacade>,
    census: Arc<Mutex<Vec<(u32, u32)>>>,
    valid: Arc<AtomicBool>,
    watch: Watch,
    counts: Arc<GateCounts>,
    kept: Kept,
}

impl Run {
    /// A run whose step 10 verifies with [`CountingClVerifier`].
    fn new() -> Run {
        Run::with(|counting| Box::new(counting))
    }

    /// The ChainLock run with the node's helper, whose step 10 verifies with what `verifier`
    /// makes of the counting double.
    fn with(verifier: impl FnOnce(CountingClVerifier) -> Box<dyn ClVerifier + Send>) -> Run {
        Run::of(CHAINLOCKS, Box::new(NodeHelper), verifier)
    }

    /// `test`'s run, whose `ConnectBlock` asks `helper` and whose step 10 verifies with what
    /// `verifier` makes of the counting double.
    fn of(
        test: &str,
        helper: Box<dyn ChainHelper>,
        verifier: impl FnOnce(CountingClVerifier) -> Box<dyn ClVerifier + Send>,
    ) -> Run {
        let p = run_params(test);
        let raw = run_blocks(test);
        let time = |b: &Block| i64::from(b.header().raw().time());
        let after_genesis = time(raw.last().expect("blocks")) - time(&raw[0]) + 86_400;
        let census = Arc::new(Mutex::new(Vec::new()));
        let valid = Arc::new(AtomicBool::new(true));
        let counting = CountingClVerifier {
            census: census.clone(),
            valid: valid.clone(),
        };
        let (watch, counts) = (Watch::default(), Arc::new(GateCounts::default()));
        let watched = watch.clone();
        let (cfg, kept) = node_config(
            p.clone(),
            after_genesis,
            RigConfig::default(),
            Counted(counts.clone()),
            verifier(counting),
            helper,
            move |inner| {
                Box::new(Watched {
                    inner,
                    watch: watched,
                })
            },
        );
        let facade = cfg.facade.clone().expect("the node's facade");
        let (rig, _) = cl_rig(cfg);
        assert_eq!(rig.genesis.hash(), raw[0].hash(), "the run's genesis");
        let blocks = linked(&rig.genesis, raw[1..].to_vec());
        Run {
            rig,
            p,
            blocks,
            facade,
            census,
            valid,
            watch,
            counts,
            kept,
        }
    }

    fn at(&self, h: u32) -> &Mined {
        &self.blocks[h as usize - 1]
    }

    fn id(&self, h: u32) -> BlockId {
        self.rig.id(self.at(h))
    }

    /// Connects 1 to `h`; a stop names the block and the abort reports.
    fn connect_to(&self, h: u32) {
        for m in &self.blocks[..h as usize] {
            let Ok(r) = self.rig.v.process_block(m.block.clone(), true, None, true).recv() else {
                self.kept.assert_no_abort(&self.rig);
                panic!("V stopped at {} with no AbortNode", m.height);
            };
            assert!(r.accepted && r.state.is_valid(), "{}: {r:?}", m.height);
            self.rig.assert_tables();
        }
        self.kept.assert_no_abort(&self.rig);
    }

    /// V stopped at `h` and restarted on the run's stores (CH7b-3 K-9; [`Kept::reopen`]): a new
    /// connector, `NodeEvo` and members facade, so a new step-10 cell, empty as Core's statics
    /// are in a new process; the chain, coins and evo layer carry over. Step 10 verifies with
    /// the counting double again, under the node's helper, its block reads still watched.
    fn reopen(&mut self, h: u32) {
        let at = self.id(h);
        let exit = self.rig.stop();
        assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
        let counting = CountingClVerifier {
            census: self.census.clone(),
            valid: self.valid.clone(),
        };
        let watched = self.watch.clone();
        let seams = OpenSeams {
            ev1: Counted(self.counts.clone()),
            cl: Box::new(counting),
            helper: Box::new(NodeHelper),
            blocks: Box::new(move |inner| {
                Box::new(Watched {
                    inner,
                    watch: watched,
                })
            }),
        };
        let chainlocks = RigConfig {
            chainlocks: true,
            ..RigConfig::default()
        };
        let mut cfg = self.kept.reopen(chainlocks, h + 1, seams);
        self.facade = cfg.facade.clone().expect("the node's facade");
        let files = cfg.files.take().expect("the node's block files");
        self.rig = Rig::restart_with(exit, files, at, cfg).expect("V starts");
        assert_eq!(tip(&self.rig), at, "V loads at {h}");
        self.kept.assert_no_abort(&self.rig);
    }

    /// The reads of `h`'s block since the last call.
    fn reads_of(&self, h: u32) -> usize {
        let id = self.id(h);
        let reads = std::mem::take(&mut *self.watch.reads.lock().expect("reads"));
        reads.iter().filter(|&&r| r == id).count()
    }

    fn unreadable(&self, h: u32) {
        *self.watch.unreadable.lock().expect("unreadable") = Some(self.id(h));
    }

    /// The verifies since the last call.
    fn census(&self) -> Vec<(u32, u32)> {
        std::mem::take(&mut *self.census.lock().expect("census"))
    }

    /// `h`'s ChainLock with `sig` as V's best lock.
    fn lock(&self, h: u32, sig: [u8; 96]) {
        intake(&self.rig);
        let lock = Arc::new(ChainLockSig {
            height: i32::try_from(h).expect("a height"),
            block_hash: self.at(h).hash(),
            sig,
        });
        assert!(record(&self.rig, &lock), "the best lock moves");
        settle(&self.rig);
    }

    /// `m`'s verdict, the state of its last `BlockChecked`: `ok`, or Core's reject reason.
    fn verdict(&self, m: &Mined) -> String {
        let state = self.rig.rec.take().into_iter().rev().find_map(|e| match e {
            Ev::Checked(hash, state) if hash == m.hash() => Some(state),
            _ => None,
        });
        match state.expect("a BlockChecked") {
            BlockState::Invalid(err) => err.reject_reason().to_owned(),
            other if other.is_valid() => "ok".to_owned(),
            other => format!("{other:?}"),
        }
    }

    /// Submits `m` and returns its verdict.
    fn submit(&self, m: &Mined) -> String {
        self.rig.rec.take();
        let r = self.rig.submit(m);
        assert!(r.accepted, "{r:?}");
        self.verdict(m)
    }
}

/// `m`'s CbTx best-ChainLock signature: the payload is version(2) height(4) roots(64) diff(1,
/// below 253) sig(96) balance(8).
fn cbtx_sig(m: &Mined) -> [u8; 96] {
    let payload = m.block.txs()[0].to_mutable().extra_payload().clone();
    payload[71..167].try_into().expect("96 bytes")
}

/// `m` with its coinbase changed by `edit`, mined again on its parent at its time: a new merkle
/// root and nonce, every other byte `m`'s.
fn remined(p: &ChainSetup, m: &Mined, edit: impl FnOnce(&mut Transaction)) -> Mined {
    let txs = m.block.txs();
    let mut cb = txs[0].to_mutable();
    edit(&mut cb);
    let mut all = vec![Arc::new(Tx::new(cb))];
    all.extend(txs[1..].iter().cloned());
    let h = m.block.header().raw();
    Mined {
        block: mine_version(p, h.prev_hash(), h.time(), all, h.version()),
        height: m.height,
    }
}

/// `m` with its CbTx's best ChainLock nulled: difference 0, the zero signature.
fn nulled(p: &ChainSetup, m: &Mined) -> Mined {
    remined(p, m, |cb| {
        {
            let mut extra_payload = cb.extra_payload().clone();
            extra_payload[70] = 0;
            *cb = dash_primitives::Transaction::new(
                cb.version(),
                cb.tx_type(),
                cb.inputs().clone(),
                cb.outputs().clone(),
                cb.lock_time(),
                extra_payload,
            );
        }
        {
            let mut extra_payload = cb.extra_payload().clone();
            extra_payload[71..167].fill(0);
            *cb = dash_primitives::Transaction::new(
                cb.version(),
                cb.tx_type(),
                cb.inputs().clone(),
                cb.outputs().clone(),
                cb.lock_time(),
                extra_payload,
            );
        }
    })
}

/// `m` with its CbTx's best-ChainLock difference set to `diff` (below 253).
fn with_diff(p: &ChainSetup, m: &Mined, diff: u8) -> Mined {
    remined(p, m, |cb| {
        let mut extra_payload = cb.extra_payload().clone();
        extra_payload[70] = diff;
        *cb = dash_primitives::Transaction::new(
            cb.version(),
            cb.tx_type(),
            cb.inputs().clone(),
            cb.outputs().clone(),
            cb.lock_time(),
            extra_payload,
        );
    })
}

/// The fixture's shape: 309 is the first CbTx ChainLock, 310 the same lock one block older.
fn assert_run_shape(run: &Run) {
    let s = cbtx_sig(run.at(309));
    assert_ne!(s, [0; 96]);
    assert_eq!(cbtx_sig(run.at(308)), [0; 96]);
    assert_eq!(cbtx_sig(run.at(310)), s);
    for (h, diff) in [(309, 0), (310, 1), (311, 2)] {
        let payload = run.at(h).block.txs()[0].to_mutable().extra_payload().clone();
        assert_eq!(payload[70], diff, "{h}");
    }
}

/// CH7b-3 C3 (`stm:162-164`; K-3): 309's verify writes the cell, so 310's step 10 takes 309's
/// ChainLock from it and reads no block.
#[test]
fn a_cell_hit_reads_no_block() {
    let run = Run::new();
    assert_run_shape(&run);
    run.connect_to(309);
    assert_eq!(run.census(), [(309, 308)]);
    run.reads_of(309);
    run.rig.accept(run.at(310));
    assert_eq!(run.reads_of(309), 0, "the cell answered for 309");
    assert_eq!(run.census(), [(310, 308)]);
}

/// CH7b-3 C3: the cell's ChainLock is 309's, so with 309 unreadable a nulled 310 still meets
/// it: `bad-cbtx-null-clsig` (`stm:169-174`), where a disk read would give none.
#[test]
fn a_cell_hit_is_the_previous_chainlock() {
    let run = Run::new();
    run.connect_to(309);
    run.census();
    run.unreadable(309);
    let b = nulled(&run.p, run.at(310));
    assert_eq!(run.submit(&b), "bad-cbtx-null-clsig");
    assert_eq!(run.reads_of(309), 0);
    assert!(run.census().is_empty());
}

/// CH7b-3 C3 (K-1 to K-3): each of Core's three writes (`stm:155-157`, `:190-191`, `:206-208`)
/// leaves a cell the next block's step 10 takes, reading no block: 309 by shortcut 1, 310 and
/// 311 by shortcut 2 (the best lock is 308's, their ChainLock), and in a run with no best lock
/// 309 after its verify.
#[test]
fn each_update_point_writes_the_cell() {
    let run = Run::new();
    run.connect_to(308);
    run.lock(308, cbtx_sig(run.at(309)));
    run.rig.accept(run.at(309));
    run.reads_of(309);
    run.rig.accept(run.at(310));
    assert_eq!(run.reads_of(309), 0, "shortcut 1 wrote the cell");
    run.rig.accept(run.at(311));
    assert_eq!(run.reads_of(310), 0, "shortcut 2 wrote the cell");
    assert!(run.census().is_empty(), "no verify");
    let shortcuts = run.counts.root_checks().cl_shortcuts;
    let outdated = ClPass::OutdatedBest;
    assert_eq!(shortcuts, [(309, ClPass::BestAtParent), (310, outdated), (311, outdated)]);

    let run = Run::new();
    run.connect_to(309);
    assert_eq!(run.census(), [(309, 308)]);
    run.reads_of(309);
    run.rig.accept(run.at(310));
    assert_eq!(run.reads_of(309), 0, "the verify wrote the cell");
    assert!(run.counts.root_checks().cl_shortcuts.is_empty());
}

/// The tests that read the cell itself, which only exists from C3.
mod cell;
/// C6's strict replays of four recorded runs through `ReplayChainHelper`.
#[cfg(feature = "statedigest")]
mod replay;
/// C4's tests of step 10's verify.
mod verify;
