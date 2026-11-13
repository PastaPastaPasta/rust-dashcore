use std::sync::Arc;

use dash_clock::{Clock, MockClock};
use dashd_chainstate::chain::{MemBlockFiles, RecordingAbort, genesis_block};

use super::*;
use crate::chain::{Chain, tests as chain_tests};

#[test]
fn load_is_the_published_empty_list_before_the_dmn_tip_exists() {
    let params = chain_tests::regtest();
    let genesis = genesis_block(&params);
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(i64::from(genesis.header().time()) + 60));
    let abort = Arc::new(RecordingAbort::default());
    let mut chain = Chain::start(chain_tests::fresh(
        &params,
        &clock,
        &abort,
        Box::new(MemBlockFiles::default()),
    ))
    .unwrap();
    let published = chain.handle().view();
    assert_eq!(published.dmn_tip, None);
    let tip = new(chain.handle().view_handle());
    let loaded = tip.load();
    assert_eq!(loaded.bls, published.bls);
    let list = loaded.list;
    assert!(list.is_default());
    assert!(Arc::ptr_eq(&list, &published.mn_list));
    drop(tip);
    assert!(chain.stop().is_some());
    assert!(abort.messages().is_empty());
}

#[test]
fn the_node_owned_handle_follows_tip_changes() {
    let dir = tempfile::tempdir().unwrap();
    let node = super::super::amb::node(
        dir.path(),
        &[
            "-connect=0",
            "-dnsseed=0",
            "-fixedseeds=0",
            "-listen=0",
            "-dip3params=48:48",
            "-testactivationheight=dip0024@1000",
        ],
    );
    let chain = node.chain().unwrap().handle();
    let tip = node.net().unwrap().tip_mn_list().unwrap().clone();
    let before = tip.load().list;
    assert!(before.is_empty());
    let blocks = recorded_blocks();
    for (i, block) in blocks[..117].iter().enumerate() {
        let result = chain.process_block(Arc::clone(block), true, None, true).recv().unwrap();
        assert!(result.accepted && result.state.is_valid(), "block {}: {result:?}", i + 1);
    }
    let published = chain.view();
    let loaded = tip.load();
    assert_eq!(loaded.bls, published.bls);
    let list = loaded.list;
    assert_eq!(published.dmn_tip, chain.reader().lookup(&blocks[116].hash()));
    assert_eq!(list.block_hash(), &blocks[116].hash());
    assert_eq!(list.len(), 2);
    assert!(Arc::ptr_eq(&list, &published.mn_list));
    assert!(before.is_empty());
    assert!(!Arc::ptr_eq(&before, &list));
}

fn recorded_blocks() -> Vec<Arc<dash_consensus::Block>> {
    include_str!("../../../../dashd-chainstate/tests/data/ch7a5/blocks.txt")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let bytes: Vec<u8> = (0..line.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&line[i..i + 2], 16).unwrap())
                .collect();
            Arc::new(
                dash_consensus::primitives::decode_block(&mut &bytes[..]).unwrap().into_value(),
            )
        })
        .collect()
}

#[cfg(feature = "test-support")]
mod disconnect {
    use std::sync::Mutex;

    use dash_primitives::BlockHash;
    use dashd_chainstate::chain::{ChainViewHandle, ServiceWait, ValidationInterface, WaitError};

    use super::*;
    use crate::logging::Logger;
    use crate::sut::{Harness, NodeSut};
    use crate::ui::Ui;

    struct Window {
        tip: Mutex<Option<(TipMnList, ChainViewHandle)>>,
        active: BlockHash,
        dmn: BlockHash,
        seen: Mutex<bool>,
    }

    impl ValidationInterface for Window {
        fn release_point(&self, _: &mut dyn ServiceWait) -> Result<(), WaitError> {
            let slot = self.tip.lock().unwrap();
            let Some((tip, chain)) = slot.as_ref() else {
                return Ok(());
            };
            let published = chain.view();
            if published.snap.tip.tip_hash == self.active {
                assert_ne!(published.snap.tip.tip_hash, self.dmn);
                let loaded = tip.load();
                assert_eq!(loaded.bls, published.bls);
                let list = loaded.list;
                assert_eq!(list.block_hash(), &self.dmn);
                assert_eq!(list.len(), 2);
                assert!(Arc::ptr_eq(&list, &published.mn_list));
                *self.seen.lock().unwrap() = true;
            }
            Ok(())
        }
    }

    #[test]
    fn load_follows_the_stale_dmn_tip_during_a_disconnect_only_step() {
        let blocks = recorded_blocks();
        let listener = Arc::new(Window {
            tip: Mutex::new(None),
            active: blocks[115].hash(),
            dmn: blocks[116].hash(),
            seen: Mutex::new(false),
        });
        let heard = Arc::clone(&listener);
        let harness = Harness {
            no_mempool: true,
            listener: Some(Arc::new(move |_| Arc::clone(&heard) as Arc<dyn ValidationInterface>)),
            ..Harness::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let ui = Arc::new(Ui::with_stderr(Arc::new(Logger::new()), Box::new(|_| {})));
        let sut = NodeSut::with_harness(
            dir.path(),
            &[
                "-regtest",
                "-printtoconsole=0",
                "-dip3params=48:48",
                "-testactivationheight=dip0024@1000",
            ],
            i64::from(blocks[116].header().time()) + 60,
            ui,
            harness,
        )
        .unwrap();
        let chain = sut.chain().unwrap();
        for block in &blocks[..117] {
            let result = chain.process_block(Arc::clone(block), true, None, true).recv().unwrap();
            assert!(result.accepted && result.state.is_valid());
        }
        *listener.tip.lock().unwrap() = Some((new(chain.view_handle()), chain.view_handle()));
        chain.invalidate_block(blocks[114].hash()).recv().unwrap().unwrap();
        assert!(*listener.seen.lock().unwrap());
    }
}
