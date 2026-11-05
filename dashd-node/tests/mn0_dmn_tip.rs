//! MN0-2b: `invalidateblock` moves the DMN tip without a mempool. V's `InvalidateBlock` emits
//! `UpdatedBlockTip` with a null fork, which moves the DMN tip (CH7 D16), so on rdashd, which runs
//! V without a mempool, the list the template reads (`GetListForBlock(pindexPrev)`,
//! `masternode/payments.cpp:152`) and `GetListAtChainTip()`'s are equal again once the command
//! ends. On setup-dashd's masternode network, invalidating the block that registered the first
//! masternode leaves both lists unregistered, and the template is served over them.
#![cfg(feature = "test-support")]

use std::sync::Arc;

use dash_clock::Clock;
use dash_consensus::primitives::{Block, decode_block};
use dash_primitives::TxType;
use dashd_mining::{BlockTemplateSource, ChainTemplateSource, TemplateTxs};
use dashd_node::logging::Logger;
use dashd_node::sut::{Harness, NodeSut};
use dashd_node::ui::Ui;

/// setup-dashd's masternode network, recorded by dashd v23.1.0 (blocks 1 to 406).
const QC_CHAIN: &str = include_str!("../../dashd-chainstate/tests/data/mn0-qc/blocks.txt");
const QC_ARGS: [&str; 5] = [
    "-regtest",
    "-printtoconsole=0",
    "-dip3params=2:2",
    "-testactivationheight=v20@100",
    "-testactivationheight=mn_rr@100",
];

fn blocks() -> Vec<Arc<Block>> {
    QC_CHAIN
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let bytes: Vec<u8> = (0..l.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&l[i..i + 2], 16).expect("hex"))
                .collect();
            let mut input = bytes.as_slice();
            let block = decode_block(&mut input).expect("a block").into_value();
            assert!(input.is_empty(), "one block per line");
            Arc::new(block)
        })
        .collect()
}

#[test]
fn invalidateblock_moves_the_dmn_tip_without_a_mempool() {
    let blocks = blocks();
    let first_reg = blocks
        .iter()
        .position(|b| {
            b.txs()
                .iter()
                .any(|tx| tx.is_special_tx_version() && tx.tx_type() == TxType::ProviderRegister)
        })
        .expect("a ProRegTx");
    let last_time = blocks.iter().map(|b| i64::from(b.header().time())).max().expect("blocks");
    for no_mempool in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let ui = Arc::new(Ui::with_stderr(Arc::new(Logger::new()), Box::new(|_| {})));
        let harness = Harness {
            no_mempool,
            ..Harness::default()
        };
        let sut =
            NodeSut::with_harness(dir.path(), &QC_ARGS, last_time, ui, harness).expect("the SUT");
        let chain = sut.chain().expect("V runs").clone();
        for block in &blocks[..=first_reg] {
            let r = chain.process_block(Arc::clone(block), true, None, true).recv();
            assert!(r.expect("V").accepted, "V refused a recorded block");
        }
        let lists = || {
            chain
                .view_handle()
                .query(|q| {
                    let view = q.evo_view();
                    let total = |l: Arc<dash_dml::MnList>| l.total_registered();
                    (view.tip_list().map(total), view.dmn_list().map(total))
                })
                .recv()
                .expect("V serves")
        };
        let (at_tip, at_dmn_tip) = lists();
        assert!(at_tip.as_ref().is_ok_and(|n| *n > 0), "registered: {at_tip:?}");
        assert_eq!(at_tip, at_dmn_tip, "no_mempool {no_mempool}: before");
        let registered = blocks[first_reg].hash();
        chain.invalidate_block(registered).recv().expect("V").expect("in the index");
        assert_eq!(lists(), (Ok(0), Ok(0)), "no_mempool {no_mempool}: (chain tip, DMN tip)");

        let clock: Arc<dyn Clock> = sut.clock().clone();
        let source =
            ChainTemplateSource::new(chain.clone(), Arc::new(sut.params().clone()), clock, None);
        let template = source.create_new_block(&[0x51], TemplateTxs::None);
        let template = template.unwrap_or_else(|e| panic!("no_mempool {no_mempool}: {e:?}"));
        let height = u32::try_from(first_reg + 1).unwrap();
        assert_eq!((template.height, template.prev), (height, blocks[first_reg - 1].hash()));
    }
}
