//! MN1-5: the exact-block DML memo (A1 §2.13) on a real node. Each template the node's source
//! builds hands `TestBlockValidity` its own list build, keyed under the node's rules, unless a
//! builder phase emitted an effect. Every hit is cross-checked (test-support): the attempt also
//! builds the list and asserts the two agree.
// Core: v24.0.2 src/node/miner.cpp:218-226, :299-306, :351-354

use dash_consensus::primitives::{Block, decode_block};
use dash_dml::DmlMemoTally;
use dashd_chainstate::chain::BlockState;
use dashd_mining::{BlockTemplateSource as _, ChainTemplateSource, Grind, TemplateTxs, grind};

use super::*;

const OP_TRUE: [u8; 1] = [0x51];
/// DIP0003 from block 2.
const ARGS: [&str; 1] = ["-dip3params=2:2"];

fn node_source(sut: &NodeSut) -> ChainTemplateSource {
    let chain = sut.chain().expect("V runs").clone();
    let clock: Arc<dyn Clock> = sut.clock().clone();
    ChainTemplateSource::new(chain, Arc::new(sut.params().clone()), clock, None)
}

fn ground(source: &ChainTemplateSource, mut block: Block) -> Arc<Block> {
    let mut tries = u64::MAX;
    assert_eq!(grind(&mut block, source.pow().unwrap(), &mut tries, &|| false), Grind::Found);
    Arc::new(block)
}

pub(super) fn tally(hits: u64, key: u64, tip: u64, effects: u64) -> DmlMemoTally {
    DmlMemoTally {
        hits,
        key,
        tip,
        effects,
    }
}

/// `TestBlockValidity` of `block` without a memo: the attempt builds the list.
fn built_answer(sut: &NodeSut, block: &Block) -> BlockState {
    let chain = sut.chain().expect("V runs");
    let block = Arc::new(Block::new(*block.header(), block.txs().to_vec()));
    chain.test_block_validity(block, false, false).recv().expect("V").expect("tested")
}

/// `generate`'s calls on a fresh node: from DIP0003's first block each template hits, under the
/// node's own rules, and answers as the build does; each connects. The template before it builds
/// no list, so it makes no memo.
#[test]
fn generated_templates_hit_and_answer_as_the_build() {
    let dir = tempfile::tempdir().unwrap();
    let sut = sut(dir.path(), &ARGS);
    loaded(&sut);
    let source = node_source(&sut);
    for height in 1..=5 {
        let template = source.create_new_block(&OP_TRUE, TemplateTxs::None).expect("a template");
        assert_eq!(template.height, height);
        let hits = u64::from(height) - 1;
        assert_eq!(source.dml_memo_tally(), tally(hits, 0, 0, 0), "{height}: a hit from DIP0003");
        assert_eq!(built_answer(&sut, &template.block), BlockState::Valid, "{height}: the build");
        submit(&sut, &ground(&source, template.block));
    }
    assert_eq!(sut.down(), None);
}

/// A template source whose NetInfo rules are not the node's (`-cjdnsreachable` where the node
/// has none) keys its memo under them: a key miss, and the attempt builds the list.
#[test]
fn a_template_under_other_net_info_rules_misses_on_its_key() {
    let dir = tempfile::tempdir().unwrap();
    let sut = sut(dir.path(), &ARGS);
    loaded(&sut);
    let source = node_source(&sut);
    submit(
        &sut,
        &ground(&source, source.create_new_block(&OP_TRUE, TemplateTxs::None).unwrap().block),
    );
    let other = node_source(&sut).cjdns_reachable(true);
    other.create_new_block(&OP_TRUE, TemplateTxs::None).expect("a template");
    assert_eq!(other.dml_memo_tally(), tally(0, 1, 0, 0));
    assert_eq!(sut.down(), None);
}

/// A block connected between the build and its `TestBlockValidity`: V answers the discarded
/// build's memo `NotBestPrev`, counted as a tip miss, and the template rebuilt on the new tip hits
/// with its own memo.
#[test]
fn a_build_discarded_between_the_turns_misses_on_its_tip_and_the_rebuilt_one_hits() {
    let dir = tempfile::tempdir().unwrap();
    let sut = sut(dir.path(), &ARGS);
    loaded(&sut);
    let first = node_source(&sut);
    let template = || first.create_new_block(&OP_TRUE, TemplateTxs::None).unwrap().block;
    submit(&sut, &ground(&first, template()));
    let block = ground(&first, template());
    let pending = Mutex::new(Some(block));
    let chain = sut.chain().expect("V runs").clone();
    let source = node_source(&sut);
    source.set_between_turns(move |_| {
        if let Some(block) = pending.lock().unwrap().take() {
            let r = chain.process_block(block, true, None, true).recv();
            assert!(r.expect("V").accepted, "V refused the block");
        }
    });
    let template = source.create_new_block(&OP_TRUE, TemplateTxs::None).expect("a template");
    assert_eq!(template.height, 3, "built on the new tip");
    assert_eq!(source.dml_memo_tally(), tally(1, 0, 1, 0));
    assert_eq!(sut.down(), None);
}

/// setup-dashd's masternode network, recorded by dashd v23.1.0 (blocks 1 to 406).
const QC_CHAIN: &str = include_str!("../../../../dashd-chainstate/tests/data/mn0-qc/blocks.txt");
const QC_ARGS: [&str; 5] = [
    "-regtest",
    "-printtoconsole=0",
    "-dip3params=2:2",
    "-testactivationheight=v20@100",
    "-testactivationheight=mn_rr@100",
];

fn qc_blocks() -> Vec<Arc<Block>> {
    let bytes = |l: &str| -> Vec<u8> {
        (0..l.len()).step_by(2).map(|i| u8::from_str_radix(&l[i..i + 2], 16).unwrap()).collect()
    };
    let lines = QC_CHAIN.lines().filter(|l| !l.starts_with('#'));
    lines.map(|l| Arc::new(decode_block(&mut bytes(l).as_slice()).unwrap().into_value())).collect()
}

/// V at block `h`'s parent with `h`'s commitments back in the mineable pool, its caches cold:
/// blocks 1 to `h` connected, restarted, then `h` invalidated, whose undo re-adds them.
fn pooled(dir: &Path, blocks: &[Arc<Block>], h: usize) -> NodeSut {
    let last = blocks.iter().map(|b| i64::from(b.header().time())).max().expect("blocks");
    let mut sut =
        NodeSut::with_harness(dir, &QC_ARGS, last, ui(), Harness::default()).expect("the SUT");
    let chain = sut.chain().expect("V runs").clone();
    for block in &blocks[..h] {
        let r = chain.process_block(Arc::clone(block), true, None, true).recv();
        assert!(r.expect("V").accepted, "V refused a recorded block");
    }
    sut.restart(Teardown::Clean);
    assert!(sut.down().is_none(), "{:?}", sut.down());
    let chain = sut.chain().expect("V runs").clone();
    chain.invalidate_block(blocks[h - 1].hash()).recv().expect("V").expect("in the index");
    sut
}

/// A template mining `h`'s signed commitments, and what its memo did.
fn commitment_template(h: usize) -> (Block, DmlMemoTally) {
    let blocks = qc_blocks();
    let dir = tempfile::tempdir().unwrap();
    let sut = pooled(dir.path(), &blocks, h);
    let source = node_source(&sut);
    let template = source.create_new_block(&OP_TRUE, TemplateTxs::None).expect("a template");
    let qc = |b: &Block| b.txs().iter().filter(|t| t.tx_type() == TxType::QuorumCommitment).count();
    assert_eq!(qc(&template.block), qc(&blocks[h - 1]), "{h}: the block's commitments");
    assert_eq!(built_answer(&sut, &template.block), BlockState::Valid, "{h}: the build");
    assert_eq!(sut.down(), None);
    (template.block, source.dml_memo_tally())
}

/// A template mining a signed rotating commitment whose cycle's snapshot is already stored (229)
/// and one mining both types' (251) emit nothing: each hits.
#[test]
fn a_template_mining_signed_commitments_hits() {
    for h in [229, 251] {
        assert_eq!(commitment_template(h).1, tally(1, 0, 0, 0), "{h}");
    }
}

/// A template whose rotating commitment's members store their cycle's snapshot (300, cycle base
/// 288) emits an effect a connect would: no memo, counted as an effects miss.
#[test]
fn a_template_whose_build_stores_a_snapshot_misses_on_effects() {
    assert_eq!(commitment_template(300).1, tally(0, 0, 0, 1));
}
