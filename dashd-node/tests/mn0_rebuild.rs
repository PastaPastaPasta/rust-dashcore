//! MN0-1 r1 m2: a block V connects between the source's query turn and its `TestBlockValidity`
//! (the window where Core holds `cs_main` and V does not) makes the source build again on the new
//! tip, and the counters and Core's log line come out of the one verdict that counts.
//!
//! Its own test binary: the log capture is a thread's scoped `tracing` dispatcher, and another
//! test's thread reaching the same callsite concurrently can leave it uncaptured.
#![cfg(feature = "test-support")]

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use dash_clock::Clock;
use dashd_mining::{
    BlockTemplateSource, ChainTemplateSource, Grind, LastBlockStats, TemplateTxs, grind,
};
use dashd_node::logging::{CoreSubscriber, LogOptions, Logger};
use dashd_node::sut::NodeSut;
use dashd_node::ui::Ui;

const T0: i64 = 1_700_000_000;
/// `CreateNewBlock`'s log line for an empty mempool's block (`node/miner.cpp:261`).
const CREATE_NEW_BLOCK_LINE: &str = "CreateNewBlock(): total size 1000 txs: 0 fees: 0 sigops 100";
const RESERVED_STATS: LastBlockStats = LastBlockStats {
    num_txs: 0,
    size: 1000,
};

/// The lines rdashd's logger writes for `f`'s events on this thread (the node's `Logger` behind
/// its `CoreSubscriber`, without timestamps).
fn rdashd_log(dir: &Path, f: impl FnOnce()) -> Vec<String> {
    let logger = Arc::new(Logger::new());
    assert!(logger.start_logging());
    let path = dir.join("captured.log");
    logger.set_options(LogOptions {
        print_to_file: true,
        file_path: path.clone(),
        log_timestamps: false,
        ..LogOptions::default()
    });
    logger.request_reopen();
    tracing::subscriber::with_default(CoreSubscriber::new(logger), f);
    std::fs::read_to_string(path).unwrap_or_default().lines().map(str::to_owned).collect()
}

mod sut {
    use super::*;

    #[test]
    fn a_tip_moved_between_the_turns_rebuilds_on_it() {
        let dir = tempfile::tempdir().unwrap();
        let rdashd = dir.path().join("rdashd");
        std::fs::create_dir_all(&rdashd).unwrap();
        let ui = Arc::new(Ui::with_stderr(Arc::new(Logger::new()), Box::new(|_| {})));
        let sut =
            NodeSut::new(&rdashd, &["-regtest", "-printtoconsole=0"], T0, ui).expect("the SUT");
        let chain = sut.chain().expect("V runs").clone();
        let clock: Arc<dyn Clock> = sut.clock().clone();
        let params = Arc::new(sut.params().clone());
        let source = ChainTemplateSource::new(chain.clone(), params, clock, None);
        let mut first = source.create_new_block(&[0x51], TemplateTxs::None).unwrap().block;
        let mut tries = u64::MAX;
        assert_eq!(grind(&mut first, source.pow().unwrap(), &mut tries, &|| false), Grind::Found);
        let first_hash = first.hash();

        let pending = Arc::new(Mutex::new(Some(first)));
        let builds = Arc::new(AtomicU32::new(0));
        source.set_between_turns({
            let (pending, builds) = (Arc::clone(&pending), Arc::clone(&builds));
            move || {
                builds.fetch_add(1, Ordering::SeqCst);
                if let Some(block) = pending.lock().unwrap().take() {
                    let r = chain.process_block(Arc::new(block), true, None, true).recv();
                    assert!(r.expect("V").accepted, "V refused block 1");
                }
            }
        });
        let log = rdashd_log(dir.path(), || {
            let template = source.create_new_block(&[0x51], TemplateTxs::None).unwrap();
            assert_eq!((template.height, template.prev), (2, first_hash));
        });
        assert_eq!(builds.load(Ordering::SeqCst), 2, "one build per V turn pair");
        assert_eq!(log, [CREATE_NEW_BLOCK_LINE]);
        assert_eq!(source.last_block_stats(), Some(RESERVED_STATS));
    }
}
