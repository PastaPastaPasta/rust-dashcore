//! BL-711 (MN0-3): a template pays from the tip's list (`GetListForBlock(pindexPrev)`,
//! `masternode/payments.cpp:152`), never from the DMN tip's. Between V's commands the two are the
//! same list. They differ at a release point inside `InvalidateBlock`, which disconnects one block
//! per hold and releases between them (`validation.cpp:3941`, `:3946`), signalling the
//! `UpdatedBlockTip` that moves the DMN tip only once it is done.
//!
//! On setup-dashd's masternode network loaded to `R`, `invalidateblock(R - 2)`: at the release
//! after `R`'s disconnect, a listener builds a template on `R - 1`. It pays as dashd's recorded `R`
//! does (the tip's list), where the DMN tip's list, still `R`'s there, pays as the recorded `R + 1`
//! does: the control.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dash_clock::Clock;
use dash_consensus::primitives::{Block, decode_block};
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{ChainHandle, ServiceWait, ValidationInterface, WaitError};
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
/// The tip before the invalidation: past the masternodes' registration, paying every block.
const R: usize = 300;

fn blocks() -> Vec<Arc<Block>> {
    QC_CHAIN
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let bytes: Vec<u8> = (0..l.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&l[i..i + 2], 16).expect("hex"))
                .collect();
            Arc::new(decode_block(&mut &bytes[..]).expect("a block").into_value())
        })
        .collect()
}

/// A payee list: script and amount of each output after the miner's.
type Payees = Vec<(Vec<u8>, i64)>;

/// The masternode payments of a block: its coinbase's outputs after the miner's.
fn payments(block: &Block) -> Payees {
    block.txs()[0].outputs()[1..]
        .iter()
        .map(|o| (o.script_pubkey.as_bytes().to_vec(), o.value))
        .collect()
}

/// `f` on another thread while V serves at `wait`, until it answers.
fn served<T: Send + 'static>(
    wait: &mut dyn ServiceWait,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, WaitError> {
    let (wake, rung) = crossbeam_channel::unbounded();
    let out = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&out);
    let thread = std::thread::spawn(move || {
        *slot.lock().unwrap() = Some(f());
        let _ = wake.send(());
    });
    let done = Arc::clone(&out);
    wait.wait_until(&rung, &mut || done.lock().unwrap().is_some())?;
    thread.join().expect("the served thread");
    Ok(out.lock().unwrap().take().expect("answered"))
}

/// What the listener needs once V runs: V's handle and the template source.
type Slot = Arc<Mutex<Option<(ChainHandle, Arc<ChainTemplateSource>)>>>;

/// What the window saw: the template's query-turn block (or its refusal), and whether the tip's
/// and the DMN tip's lists were different lists there.
type Seen = Arc<Mutex<Option<(Result<Block, String>, bool)>>>;

/// The listener: at the first release point whose published tip is `window_tip`, a template is
/// built (its query turn served there) and the two lists are compared.
struct Window {
    window_tip: BlockHash,
    started: AtomicBool,
    slot: Slot,
    seen: Seen,
}

impl ValidationInterface for Window {
    fn release_point(&self, wait: &mut dyn ServiceWait) -> Result<(), WaitError> {
        let Some((v, source)) = self.slot.lock().unwrap().clone() else {
            return Ok(());
        };
        if v.view().snap.tip.tip_hash != self.window_tip
            || self.started.swap(true, Ordering::SeqCst)
        {
            return Ok(());
        }
        let built = Arc::new(Mutex::new(None));
        let (wake, rung) = crossbeam_channel::unbounded();
        let first = Arc::clone(&built);
        let refused_wake = wake.clone();
        source.set_between_turns(move |block: &Block| {
            let mut first = first.lock().unwrap();
            if first.is_none() {
                *first = Some(Ok(Block::new(*block.header(), block.txs().to_vec())));
                let _ = wake.send(());
            }
        });
        // The template's later turns run once V is out of the invalidation; a refusal in its
        // query turn ends the wait.
        let refused = Arc::clone(&built);
        std::thread::spawn(move || {
            if let Err(e) = source.create_new_block(&[0x51], TemplateTxs::None) {
                refused.lock().unwrap().get_or_insert(Err(format!("{e:?}")));
                let _ = refused_wake.send(());
            }
        });
        let done = Arc::clone(&built);
        wait.wait_until(&rung, &mut || done.lock().unwrap().is_some())?;
        let differ = served(wait, move || {
            v.query(|q| {
                let view = q.evo_view();
                let (tip, dmn) = (view.tip_list().expect("reads"), view.dmn_list().expect("reads"));
                tip.block_hash() != dmn.block_hash()
            })
            .recv()
            .expect("V serves")
        })?;
        let block = built.lock().unwrap().take().expect("built");
        *self.seen.lock().unwrap() = Some((block, differ));
        Ok(())
    }
}

#[test]
fn a_template_in_an_invalidations_window_pays_from_the_tip_list() {
    let chain = blocks();
    let now = chain.iter().map(|b| i64::from(b.header().time())).max().expect("blocks") + 60;
    let (recorded_r, recorded_next) = (&chain[R - 1], &chain[R]);
    // The control's premise: the lists after `R - 1` and after `R` pay differently.
    assert_ne!(payments(recorded_r), payments(recorded_next), "the payees at {R} and {}", R + 1);

    let slot = Slot::default();
    let seen = Seen::default();
    let listener = Arc::new(Window {
        window_tip: chain[R - 2].hash(),
        started: AtomicBool::new(false),
        slot: Arc::clone(&slot),
        seen: Arc::clone(&seen),
    });
    let harness = Harness {
        listener: Some(Arc::new(move |_: &dash_chain::ChainReader| {
            Arc::clone(&listener) as Arc<dyn ValidationInterface>
        })),
        no_mempool: true,
        ..Harness::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let ui = Arc::new(Ui::with_stderr(Arc::new(Logger::new()), Box::new(|_| {})));
    let sut = NodeSut::with_harness(dir.path(), &QC_ARGS, now, ui, harness).expect("the SUT");
    let v = sut.chain().expect("V runs").clone();
    let clock: Arc<dyn Clock> = sut.clock().clone();
    let source =
        Arc::new(ChainTemplateSource::new(v.clone(), Arc::new(sut.params().clone()), clock, None));
    for block in &chain[..R] {
        let r = v.process_block(Arc::clone(block), true, None, true).recv().expect("V");
        assert!(r.accepted && r.state.is_valid(), "V refused a recorded block");
    }
    *slot.lock().unwrap() = Some((v.clone(), source));

    v.invalidate_block(chain[R - 3].hash()).recv().expect("V").expect("in the index");
    let (built, differ) = seen.lock().unwrap().take().expect("a template built in the window");
    let built = built.unwrap_or_else(|e| panic!("the template in the window: {e}"));
    assert!(differ, "the tip's and the DMN tip's lists differ in the window");
    assert_eq!(built.header().prev_hash(), chain[R - 2].hash(), "the window's tip is R - 1");
    assert_eq!(payments(&built), payments(recorded_r), "the tip list's payees");
}
