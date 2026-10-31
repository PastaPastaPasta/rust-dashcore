//! `disconnect_block_micro` (CH3 §6.3; CH3a a5): `DisconnectBlock` of `connect_block_micro`'s
//! six connected blocks, serial, gated on instructions and allocations by PF0's counter gate
//! (`crit:dashd-chainstate disconnect_block_micro`). Rows are `serial/<block>`, the blocks
//! [`connect_block_micro`](micro)'s: `empty`, `p2pkh-500`, `p2sh-multisig-250`, `pre-dip1-1mb`,
//! `dip1-2mb` and `chained-200`. A disconnect checks no script, so each row takes the unsigned
//! (`av-on`) block.
//!
//! **What a row measures.** One [`SerialCore::disconnect`] from the tip (DisconnectTip's call, so
//! H1's scheme context is taken; `NoEvo`'s evo side): Core's `DisconnectBlock`
//! (`validation.cpp:2282-2378`) into a [`BlockOverlay`] over the warm cache, which holds the
//! block's outputs at the block. It includes the undo record's read (`UndoReadFromDisk`: the rev
//! file opened by path, one `pread` of the record and its checksum, the checksum, the decode),
//! every output's spend, every input's `ApplyTxInUndo`, and the overlay's drop. It leaves out V's
//! bracket (the EV attempt and the overlay's commit, which `coins-overlay-block-1k` measures), as
//! `connect_block_micro` does.
//!
//! **Every iteration starts from the same state.** The overlay is dropped uncommitted, so the
//! cache stays at the block, and a disconnect writes nothing: no per-iteration setup. Each row's
//! setup builds `connect_block_micro`'s fixture for the block, connects it once and commits the
//! overlay at the block, which writes the undo record the rows read; one unmeasured disconnect then
//! checks that it restores every coin the block spent and leaves none it made, and every measured
//! one checks the result again.

#[path = "connect_block_micro.rs"]
#[allow(dead_code, reason = "connect_block_micro's own rows and entry point")]
mod micro;

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use dash_consensus::coins::CoinsView as _;
use dash_consensus::deployments::{Deployments, NoEhfSignals};
use dash_consensus::primitives::OutPoint;
use dash_params::{ChainParamsBuilder, Network, Release};
use dashd_chainstate::chain::{
    AttemptOrigin, ConnectCx, ConnectOutcome, DisconnectResult, GuardScope,
};
use dashd_chainstate::coins::{BlockOverlay, BlockRef};
use dashd_chainstate::connect::SerialCore;
use dashd_chainstate::connect::bench::status_writer;
use micro::{Av, Expect, Fixture, NoChainLocks, Par, Shape, reset};

/// One row's world: `connect_block_micro`'s fixture with its block connected and committed.
struct Connected(Fixture);

impl Connected {
    fn new(params: &Arc<dash_params::ChainSetup>, pool: &rayon::ThreadPool, shape: Shape) -> Self {
        let mut fx = Fixture::new(params, pool, Par::Serial, shape, Av::On);
        let e = fx.reader.entry(fx.id).expect("indexed");
        let at = BlockRef {
            id: fx.id,
            hash: e.hash,
            height: e.height,
        };
        fx.with_cx(|core, steps, cx, coins, undo, block, id, chain| {
            reset(chain, id, undo);
            let mut overlay = BlockOverlay::new(coins);
            let mut st = status_writer(chain, id);
            let files = undo.files.borrow();
            let out = core.connect(steps, cx, &mut overlay, &files, block, true, id, &mut st);
            assert_eq!(out.expect("no storage fault"), ConnectOutcome::Connected);
            overlay.commit(at).expect("the block's coins commit");
        });
        assert!(fx.steps.effects().is_empty(), "no effect below DIP0003");
        Connected(fx)
    }

    /// `measure`s the disconnect, after one unmeasured disconnect that checks the coins it leaves.
    fn run(&mut self, measure: impl FnOnce(&mut dyn FnMut() -> DisconnectResult)) {
        let fx = &mut self.0;
        if !fx.warm {
            let restored = disconnect_checked(fx);
            assert!(restored > 0 || fx.block.txs().len() == 1, "the block spent coins");
            fx.warm = true;
        }
        let abort = fx.abort.clone();
        fx.with_cx(|core, steps, cx, coins, undo, block, id, _| {
            let mut routine = || {
                let mut overlay = BlockOverlay::new(coins);
                let files = undo.files.borrow();
                let out = core.disconnect(steps, cx, &mut overlay, &files, block, id, true);
                let out = out.expect("no storage fault");
                assert_eq!(out, DisconnectResult::Ok);
                out
            };
            measure(&mut routine);
        });
        assert!(abort.messages().is_empty(), "{:?}", abort.messages());
    }
}

/// One disconnect whose overlay is checked, then dropped: each coin the block spent from outside
/// it is back, and no output it made is left. The coins restored.
fn disconnect_checked(fx: &mut Fixture) -> usize {
    let mut restored = 0;
    fx.with_cx(|core, steps, cx, coins, undo, block, id, _| {
        let made: HashSet<_> = block.txs().iter().map(|t| t.txid()).collect();
        let mut overlay = BlockOverlay::new(coins);
        let files = undo.files.borrow();
        let out = core.disconnect(steps, cx, &mut overlay, &files, block, id, true);
        assert_eq!(out.expect("no storage fault"), DisconnectResult::Ok);
        for tx in block.txs().iter().skip(1) {
            for input in tx.inputs() {
                let back = overlay.coin(&input.prevout).expect("reads").is_some();
                assert_eq!(back, !made.contains(&input.prevout.hash), "{:?}", input.prevout);
                restored += usize::from(back);
            }
        }
        for tx in block.txs() {
            for o in 0..tx.outputs().len() {
                let op = OutPoint {
                    hash: tx.txid(),
                    index: o as u32,
                };
                assert!(overlay.coin(&op).expect("reads").is_none(), "{op:?} is left");
            }
        }
    });
    restored
}

impl Fixture {
    /// Runs `f` over the fixture's parts and its connector context.
    fn with_cx<R>(
        &mut self,
        f: impl for<'c> FnOnce(
            &mut SerialCore,
            &mut dashd_chainstate::connect::bench::NoEvoSteps,
            &ConnectCx<'_>,
            &mut dashd_chainstate::coins::CoinsCache<dashd_store::MemoryEngine>,
            &micro::Undo,
            &dash_consensus::Block,
            dash_consensus::BlockId,
            &'c RefCell<&'c mut dash_chain::HeaderChain>,
        ) -> R,
    ) -> R {
        let deployments =
            Deployments::new(&self.params, &self.ancestry, &NoEhfSignals, &self.versionbits);
        let cx = ConnectCx {
            params: &self.params,
            reader: &self.reader,
            deployments: &deployments,
            best_header: self.chain.best_header(),
            min_chain_work: self.reader.min_chain_work(),
            is_ibd: true,
            chainlocks: &NoChainLocks,
            guard: GuardScope::Live,
            origin: AttemptOrigin::Tip,
        };
        let chain = RefCell::new(&mut self.chain);
        f(
            &mut self.core,
            &mut self.steps,
            &cx,
            &mut self.coins,
            &self.undo,
            &self.block,
            self.id,
            &chain,
        )
    }
}

fn disconnect_block_micro(c: &mut Criterion) {
    let params = Arc::new(
        ChainParamsBuilder::new(Network::Main, Release::V24_0).build().expect("Core's parameters"),
    );
    // As connect_block_micro's: on the bench thread, so no worker spins into a measured region.
    let pool = rayon::ThreadPoolBuilder::new().use_current_thread().build().expect("a rayon pool");
    for shape in Shape::ALL.into_iter().filter(|s| matches!(s.expect(), Expect::Connected)) {
        // Built on the row's first call (only a selected row's), and dropped after it.
        let mut fixture = None;
        c.bench_function(&format!("serial/{}", shape.name()), |b| {
            let fx = fixture.get_or_insert_with(|| Connected::new(&params, &pool, shape));
            fx.run(|routine| b.iter(routine));
        });
    }
}

criterion_group!(benches, disconnect_block_micro);
criterion_main!(benches);
