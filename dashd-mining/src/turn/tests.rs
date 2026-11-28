use std::sync::Mutex;

use dash_primitives::TxType;

use super::*;
use crate::package::tests::{inputs, ordinary, special, tip_and_hooks};
use crate::select::tests::Dag;

/// Every step of a turn, and the pool's lock, in the order they ran.
type Log = Arc<Mutex<Vec<&'static str>>>;

/// A turn that records its steps; `fail` names the step that answers a fault.
struct Recording {
    log: Log,
    fail: Option<&'static str>,
}

impl Recording {
    fn step(&self, name: &'static str) -> Result<(), BlockError> {
        self.log.lock().unwrap().push(name);
        match self.fail {
            Some(fail) if fail == name => Err(BlockError::Fault(format!("{name} faulted"))),
            _ => Ok(()),
        }
    }
}

impl Turn for Recording {
    type Started = ();
    type CbTx = ();
    type Out = ();

    fn begin(&mut self) -> Result<(), BlockError> {
        self.step("begin: coins + LLMQ")
    }

    fn dmn_tip_list(&mut self) -> Result<Arc<MnList>, BlockError> {
        self.step("dmn tip list")?;
        Ok(Arc::new(MnList::scratch()))
    }

    fn select(&mut self, _: &dyn PackageView, _: &MnList, _: &mut ()) -> Result<(), BlockError> {
        self.step("select")
    }

    fn cbtx(&mut self, _: ()) -> Result<(), BlockError> {
        self.step("cbtx: coins + LLMQ")
    }

    fn superblock(&mut self, _: &MnList) -> Result<SuperblockTemplate, BlockError> {
        self.step("superblock")?;
        Ok(SuperblockTemplate::NotTriggered)
    }

    fn finish(&mut self, _: (), _: &MnList, _: SuperblockTemplate) -> Result<(), BlockError> {
        self.step("finish: payments")
    }
}

/// A pool that records when its read lock is taken and released.
struct LockLog {
    pool: crate::testkit::FakePool,
    log: Log,
}

impl MiningMempool for LockLog {
    fn with_read(&self, f: &mut dyn FnMut(&dyn PackageView)) {
        self.log.lock().unwrap().push("pool: read lock");
        f(&self.pool);
        self.log.lock().unwrap().push("pool: unlock");
    }

    fn transactions_updated(&self) -> u32 {
        0
    }
}

fn recorded(
    fail: Option<&'static str>,
    mempool: bool,
) -> (Result<(), BlockError>, Vec<&'static str>) {
    let log = Log::default();
    let pool = LockLog {
        pool: crate::testkit::FakePool::new(),
        log: Arc::clone(&log),
    };
    let mut turn = Recording {
        log: Arc::clone(&log),
        fail,
    };
    let out = run(&mut turn, mempool.then_some(&pool as &dyn MiningMempool));
    let steps = log.lock().unwrap().clone();
    (out, steps)
}

/// Plan §5's turn: the commitments under the coins and the LLMQ view, the DMN tip list before
/// the pool's lock, the selection under it, the CbTx under the coins and the LLMQ view after it
/// drops, then the governance read with neither held, then the payments. Without a mempool
/// (`generateblock`) there is no selection and no pool lock.
// Core: v24.0.2 src/node/miner.cpp:251-255, :285-338, src/masternode/payments.cpp:449-453
#[test]
fn the_turn_reads_in_plan_order() {
    let (out, steps) = recorded(None, true);
    assert_eq!(out, Ok(()));
    assert_eq!(
        steps,
        [
            "begin: coins + LLMQ",
            "dmn tip list",
            "pool: read lock",
            "select",
            "pool: unlock",
            "cbtx: coins + LLMQ",
            "superblock",
            "finish: payments"
        ]
    );
    let (out, steps) = recorded(None, false);
    assert_eq!(out, Ok(()));
    assert_eq!(
        steps,
        [
            "begin: coins + LLMQ",
            "dmn tip list",
            "cbtx: coins + LLMQ",
            "superblock",
            "finish: payments"
        ]
    );
}

/// A step's fault ends the turn with it: no later step runs, so no template is built.
#[test]
fn a_fault_ends_the_turn() {
    for (fail, ran) in [
        ("begin: coins + LLMQ", 1),
        ("dmn tip list", 2),
        ("select", 5),
        ("cbtx: coins + LLMQ", 6),
        ("superblock", 7),
        ("finish: payments", 8),
    ] {
        let (out, steps) = recorded(Some(fail), true);
        assert_eq!(out, Err(BlockError::Fault(format!("{fail} faulted"))), "{fail}");
        assert_eq!(steps.len(), ran, "{fail}: {steps:?}");
        assert_eq!(
            *steps.last().unwrap(),
            if fail == "select" {
                "pool: unlock"
            } else {
                fail
            }
        );
    }
}

/// A read fault from `TipReads` during the selection ends the build with that fault (MN1-3
/// review n-2): `finish()` runs before the selection is read, so the packages selected before
/// the fault never reach a template.
#[test]
fn a_tip_read_fault_yields_no_selection() {
    let mut dag = Dag::default();
    dag.add_tx(ordinary(1), &[], 5000, 100, 1);
    let faulty = special(2, TxType::ProviderUpdateService);
    dag.add_tx(faulty.clone(), &[], 1000, 100, 1);
    let pool = dag.pool();
    let (mut tip, hooks) = tip_and_hooks();
    tip.fault = Some(faulty.txid());
    let got = select(
        &pool,
        inputs(&tip, &hooks, false),
        &Options::default().clamp(true),
        BlockState::default(),
    );
    assert_eq!(got.err(), Some(BlockError::Fault(format!("read fault at {}", faulty.txid()))));

    // Without the fault the same pool selects both, in ancestor-score order.
    let (tip, hooks) = tip_and_hooks();
    let picked = select(
        &pool,
        inputs(&tip, &hooks, false),
        &Options::default().clamp(true),
        BlockState::default(),
    )
    .expect("no fault");
    let ids: Vec<_> = picked.txs.iter().map(|tx| tx.txid()).collect();
    assert_eq!(ids, [ordinary(1).txid(), faulty.txid()]);
    assert_eq!(picked.selection.state.fees, 6000);
    assert!(picked.builder.is_none(), "no builder below DIP0003");
}
