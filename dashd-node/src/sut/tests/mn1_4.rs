//! MN1-4: `CreateNewBlock` with mempool transactions on a real node, through the node's template
//! source with a fake pool behind `MiningMempool`: V's `TestBlockValidity` passes, the
//! template's fields and counters, and `FillBlockPayments`' superblock half through a fake
//! `TemplateGovernance` on the node's slot.
// Core: v24.0.2 src/node/miner.cpp:176-363, src/masternode/payments.cpp:131-170, :444-474

use dash_consensus::primitives::{Block, TxRef, decode_block};
use dash_dml::MnList;
use dash_primitives::SpecialPayload;
use dashd_chainstate::chain::mempool_turn::{LockOrderGuard, LockRank};
use dashd_chainstate::connect::{SuperblockTemplate, TemplateGovernance};
use dashd_mining::testkit::{FakeEntry, FakePool};
use dashd_mining::{
    BlockTemplate, BlockTemplateSource as _, ChainTemplateSource, EntryId, Grind, InstantSendHooks,
    LastBlockStats, MiningMempool, MiningSafety, PackageView, TemplateError, TemplateTxs, grind,
};

use super::*;
use crate::test_log::{Lines, capture_this_thread};

const OP_TRUE: [u8; 1] = [0x51];
/// DIP0003 from block 2, V20 and MN_RR from 100 (the platform share), and superblocks from 100
/// every 20 blocks.
const ARGS: [&str; 4] = [
    "-dip3params=2:2",
    "-testactivationheight=v20@100",
    "-testactivationheight=mn_rr@100",
    "-budgetparams=240:100:100",
];
const SUPERBLOCK: u32 = 120;

/// The pool takes the lock-order checker's mempool rank around its read guard, as the node's
/// pool does: a coins guard held into it panics V.
struct CheckedPool(FakePool);

impl MiningMempool for CheckedPool {
    fn with_read(&self, f: &mut dyn FnMut(&dyn PackageView)) {
        let _rank = LockOrderGuard::enter(LockRank::Mempool);
        f(&self.0);
    }

    fn transactions_updated(&self) -> u32 {
        self.0.transactions_updated()
    }
}

/// InstantSend enabled, nothing locked, every transaction safe; each hook takes the checker's
/// InstantSend rank, so a hook asked under a coins guard panics V.
#[derive(Default)]
struct CheckedHooks {
    asked: AtomicU32,
}

impl InstantSendHooks for CheckedHooks {
    fn is_enabled(&self) -> bool {
        let _rank = LockOrderGuard::enter(LockRank::InstantSend);
        true
    }

    fn is_locked(&self, _: &TxHash) -> bool {
        let _rank = LockOrderGuard::enter(LockRank::InstantSend);
        false
    }
}

impl MiningSafety for CheckedHooks {
    fn is_tx_safe_for_mining(&self, _: &TxHash) -> bool {
        let _rank = LockOrderGuard::enter(LockRank::InstantSend);
        self.asked.fetch_add(1, Ordering::SeqCst);
        true
    }
}

/// Triggers a superblock paying `outs` at [`SUPERBLOCK`], none elsewhere; records the heights.
struct FakeGovernance {
    outs: Vec<TxOut>,
    heights: Mutex<Vec<u32>>,
}

impl TemplateGovernance for FakeGovernance {
    fn superblock_for_template(&self, h: u32, _: &MnList) -> Result<SuperblockTemplate, Fatal> {
        self.heights.lock().unwrap().push(h);
        Ok(if h == SUPERBLOCK {
            SuperblockTemplate::Triggered(self.outs.clone())
        } else {
            SuperblockTemplate::NotTriggered
        })
    }
}

fn node_source(sut: &NodeSut) -> ChainTemplateSource {
    let chain = sut.chain().expect("V runs").clone();
    let clock: Arc<dyn Clock> = sut.clock().clone();
    ChainTemplateSource::new(chain, Arc::new(sut.params().clone()), clock, None)
}

/// `template` ground and connected.
fn connect(sut: &NodeSut, source: &ChainTemplateSource, template: BlockTemplate) -> Arc<Block> {
    let mut block = template.block;
    let mut tries = u64::MAX;
    let pow = source.pow().expect("regtest's PoW");
    assert_eq!(grind(&mut block, pow, &mut tries, &|| false), Grind::Found);
    let block = Arc::new(block);
    submit(sut, &block);
    block
}

/// A fresh node with empty-mempool templates connected up to `tip`.
fn mined(dir: &Path, tip: u32) -> (NodeSut, Vec<Arc<Block>>) {
    let sut = sut(dir, &ARGS);
    loaded(&sut);
    let source = node_source(&sut);
    let mut blocks = Vec::new();
    while blocks.len() < tip as usize {
        let template = source.create_new_block(&OP_TRUE, TemplateTxs::None).expect("a template");
        blocks.push(connect(&sut, &source, template));
    }
    (sut, blocks)
}

/// Output 0 of `prev` to one `OP_TRUE` output, paying `fee`.
fn spend(prev: &Tx, fee: i64) -> TxRef {
    let input = TxIn::new(OutPoint::new(prev.txid(), 0), Vec::new().into(), u32::MAX);
    tx(vec![input], &[(prev.outputs()[0].value() - fee, &OP_TRUE)])
}

fn entry(tx: &TxRef, fee: i64) -> FakeEntry {
    FakeEntry::new(Arc::clone(tx), fee, u64::from(tx.size()), tx.legacy_sigop_count())
}

/// `tx`, a child of `parent`, with Core's cached ancestor fields.
fn child_entry(pool: &FakePool, parent: EntryId, tx: &TxRef, fee: i64) -> FakeEntry {
    let p = &pool.entries[parent.0 as usize];
    let mut e = entry(tx, fee);
    e.parents = vec![parent];
    e.size_with_ancestors += p.size_with_ancestors;
    e.mod_fees_with_ancestors += p.mod_fees_with_ancestors;
    e.sigops_with_ancestors += p.sigops_with_ancestors;
    e.count_with_ancestors += p.count_with_ancestors;
    e
}

/// The lines this thread and V's log from now on.
fn captured(sut: &NodeSut) -> Arc<Lines> {
    let lines = Arc::new(Lines::default());
    capture_this_thread(&lines);
    let v = Arc::clone(&lines);
    sut.chain().unwrap().query(move |_| capture_this_thread(&v)).recv().expect("V answers");
    lines
}

/// The index of the first of `lines` containing `needle`.
fn at(lines: &[String], needle: &str) -> usize {
    lines.iter().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("no {needle:?}"))
}

/// A version 1 asset lock of `locked` from output 0 of `prev`: one P2PKH credit output, the
/// locked amount on an `OP_RETURN` output with an empty push, the change to `OP_TRUE` less `fee`.
// Core: v24.0.2 src/evo/assetlocktx.cpp:28-85
fn asset_lock(prev: &Tx, locked: i64, fee: i64) -> TxRef {
    let p2pkh = [&[0x76, 0xa9, 0x14][..], &[7; 20], &[0x88, 0xac]].concat();
    let mut payload = vec![1, 1];
    payload.extend(locked.to_le_bytes());
    payload.push(p2pkh.len() as u8);
    payload.extend(&p2pkh);
    let input = TxIn::new(OutPoint::new(prev.txid(), 0), Vec::new().into(), u32::MAX);
    let change = prev.outputs()[0].value() - locked - fee;
    Arc::new(Tx::new(Transaction::new(
        3,
        TxType::AssetLock,
        vec![input],
        vec![
            TxOut::from_duffs(change, OP_TRUE.to_vec().into()),
            TxOut::from_duffs(locked, vec![0x6a, 0x00].into()),
        ],
        0,
        payload,
    )))
}

/// The CbTx's `creditPoolBalance`.
fn credit_pool_balance(template: &BlockTemplate) -> i64 {
    let coinbase = &template.block.txs()[0];
    match coinbase.payload().expect("a payload").expect("decodes").payload() {
        SpecialPayload::CoinbaseCommitment(cbtx) => cbtx.credit_pool_balance().expect("V20's CbTx"),
        other => panic!("{other:?}"),
    }
}

fn coinbase_total(template: &BlockTemplate) -> i64 {
    template.block.txs()[0].outputs().iter().map(TxOut::value).sum()
}

/// A parent-child package and a lone transaction from the pool: the template passes V's
/// `TestBlockValidity` and connects. The coinbase claims the subsidy plus the fees
/// (`blockReward`), the miner's output less the masternode payments; `vTxFees[0]` is `-nFees`,
/// `vTxSigOps[0]` the coinbase's legacy count; the counters count the selected transactions.
/// With InstantSend enabled `IsTxSafeForMining` is asked for each, under no coins guard (the
/// checker's ranks), and the pool's lock is taken in V's turn without a coins guard.
#[test]
fn a_template_with_mempool_transactions_passes_tbv_and_connects() {
    let dir = tempfile::tempdir().unwrap();
    let (sut, blocks) = mined(dir.path(), 110);
    let parent = spend(&blocks[0].txs()[0], 1000);
    let child = spend(&parent, 2000);
    let lone = spend(&blocks[1].txs()[0], 500);
    let fees = 3500;
    let mut pool = FakePool::new();
    let p = pool.push(entry(&parent, 1000));
    let c = child_entry(&pool, p, &child, 2000);
    pool.push(c);
    pool.push(entry(&lone, 500));
    let hooks = Arc::new(CheckedHooks::default());
    let source = node_source(&sut)
        .with_mempool(Arc::new(CheckedPool(pool)))
        .with_instant_send(hooks.clone(), hooks.clone());

    let empty = source.create_new_block(&OP_TRUE, TemplateTxs::None).expect("no transactions");
    let template = source.create_new_block(&OP_TRUE, TemplateTxs::Mempool).expect("TBV passes");
    assert_eq!(template.height, 111);
    let txs = template.block.txs();
    let qcs = txs.iter().take_while(|tx| tx.txid() != parent.txid()).skip(1);
    assert!(qcs.clone().all(|tx| tx.tx_type() == TxType::QuorumCommitment));
    let qcs = qcs.count();
    let selected: Vec<_> = txs[1 + qcs..].iter().map(|tx| tx.txid()).collect();
    assert_eq!(selected, [parent.txid(), child.txid(), lone.txid()], "by ancestor score");
    let mut tx_fees = vec![-fees];
    tx_fees.extend(std::iter::repeat_n(0, qcs));
    tx_fees.extend([1000, 2000, 500]);
    assert_eq!(template.tx_fees, tx_fees);
    assert_eq!(template.tx_sigops[0], i64::from(txs[0].legacy_sigop_count()));
    let sigops: Vec<_> = txs.iter().map(|tx| i64::from(tx.legacy_sigop_count())).collect();
    assert_eq!(template.tx_sigops[1 + qcs..], sigops[1 + qcs..]);
    assert!(template.tx_sigops[1..=qcs].iter().all(|n| *n == 0));
    assert_eq!(coinbase_total(&template), coinbase_total(&empty) + fees, "subsidy + nFees");
    let paid: i64 = template.masternode_payments.iter().map(TxOut::value).sum();
    assert!(paid > 0, "the platform share past MN_RR");
    assert_eq!(txs[0].outputs()[0].value(), coinbase_total(&template) - paid);
    assert_eq!(&txs[0].outputs()[1..], template.masternode_payments.as_slice());
    let size: u64 = txs[1..].iter().map(|tx| u64::from(tx.size())).sum();
    assert_eq!(
        source.last_block_stats(),
        Some(LastBlockStats {
            num_txs: (qcs + 3) as u64,
            size: 1000 + size,
        })
    );
    assert_eq!(hooks.asked.load(Ordering::SeqCst), 3, "IsTxSafeForMining for each");
    connect(&sut, &source, template);
    assert_eq!(sut.down(), None, "no turn stopped V");
}

/// An asset lock from the pool: the template selects it and passes V's `TestBlockValidity`, and
/// the CbTx over the block with it carries the empty template's `creditPoolBalance` plus the
/// locked amount.
// Core: v24.0.2 src/node/miner.cpp:319-324
#[test]
fn a_selected_asset_lock_moves_the_cbtx_credit_pool_balance() {
    let dir = tempfile::tempdir().unwrap();
    let (sut, blocks) = mined(dir.path(), 110);
    let locked = 12_345_678;
    let lock = asset_lock(&blocks[2].txs()[0], locked, 1000);
    let mut pool = FakePool::new();
    pool.push(entry(&lock, 1000));
    let source = node_source(&sut).with_mempool(Arc::new(CheckedPool(pool)));

    let empty = source.create_new_block(&OP_TRUE, TemplateTxs::None).expect("no transactions");
    let template = source.create_new_block(&OP_TRUE, TemplateTxs::Mempool).expect("TBV passes");
    let selected = template.block.txs().last().map(|tx| tx.txid());
    assert_eq!(selected, Some(lock.txid()), "the asset lock is selected");
    assert_eq!(credit_pool_balance(&template), credit_pool_balance(&empty) + locked);
    connect(&sut, &source, template);
    assert_eq!(sut.down(), None);
}

/// `FillBlockPayments`' superblock half through a fake back end on the node's slot: at a
/// triggered height its outputs follow the masternode outputs, in order, and the block connects;
/// elsewhere there are none. Its `GOBJECT` line comes after the CbTx line and before the
/// `MNPAYMENTS` lines (an empty list: `GetBlockTxOuts`' platform share and no-masternode lines,
/// then `GetMasternodeTxOuts`' payment line for the share, whose `OP_RETURN` has no
/// destination), and the `BENCHMARK` line ends the build.
#[test]
fn a_triggered_superblock_pays_after_the_masternodes() {
    let dir = tempfile::tempdir().unwrap();
    let (sut, _) = mined(dir.path(), SUPERBLOCK - 1);
    let outs =
        vec![TxOut::from_duffs(11, vec![0x52].into()), TxOut::from_duffs(22, vec![0x53].into())];
    let fake = Arc::new(FakeGovernance {
        outs: outs.clone(),
        heights: Mutex::new(Vec::new()),
    });
    let slot = sut.steps.chainstate().template_governance_slot();
    assert!(slot.register(fake.clone()).is_ok());
    let source = node_source(&sut);

    let lines = captured(&sut);
    let template = source.create_new_block(&OP_TRUE, TemplateTxs::None).expect("a superblock");
    assert_eq!(template.height, SUPERBLOCK);
    assert_eq!(template.superblock_payments, outs);
    let coinbase = template.block.txs()[0].outputs().to_vec();
    let mn = &template.masternode_payments;
    assert_eq!(mn.len(), 1, "the platform share alone");
    let mut expected = vec![coinbase[0].clone()];
    expected.extend(mn.iter().cloned());
    expected.extend(outs.iter().cloned());
    assert_eq!(coinbase, expected, "the miner, the masternode outputs, then the superblock's");
    let log = lines.messages();
    let payment = format!(
        "CMNPaymentsProcessor::GetMasternodeTxOuts -- Masternode payment {} to ",
        mn[0].value()
    );
    assert!(log.contains(&payment), "no destination: {log:#?}");
    let order = [
        at(&log, "CreateNewBlock(): total size"),
        at(&log, "CbTx"),
        at(&log, "Triggered superblock creation at height 120"),
        at(&log, "reallocated to credit pool"),
        at(&log, "no masternode registered to receive a payment"),
        at(&log, &payment),
        at(&log, "CreateNewBlock() packages:"),
    ];
    assert!(order.is_sorted(), "{order:?}: {log:#?}");
    connect(&sut, &source, template);

    let template = source.create_new_block(&OP_TRUE, TemplateTxs::None).expect("a template");
    assert_eq!(template.height, SUPERBLOCK + 1);
    assert!(template.superblock_payments.is_empty());
    let coinbase = template.block.txs()[0].outputs();
    assert_eq!(coinbase.len(), 1 + template.masternode_payments.len());
    assert_eq!(*fake.heights.lock().unwrap(), [SUPERBLOCK, SUPERBLOCK + 1]);
    assert_eq!(sut.down(), None);
}

/// setup-dashd's masternode network, recorded by dashd v23.1.0 (blocks 1 to 406), with
/// superblocks from 300: masternodes are registered by then.
const QC_CHAIN: &str = include_str!("../../../../dashd-chainstate/tests/data/mn0-qc/blocks.txt");
const QC_ARGS: [&str; 6] = [
    "-regtest",
    "-printtoconsole=0",
    "-dip3params=2:2",
    "-testactivationheight=v20@100",
    "-testactivationheight=mn_rr@100",
    "-budgetparams=240:300:300",
];

fn qc_blocks() -> Vec<Arc<Block>> {
    let bytes = |l: &str| -> Vec<u8> {
        (0..l.len()).step_by(2).map(|i| u8::from_str_radix(&l[i..i + 2], 16).unwrap()).collect()
    };
    let lines = QC_CHAIN.lines().filter(|l| !l.starts_with('#'));
    lines.map(|l| Arc::new(decode_block(&mut bytes(l).as_slice()).unwrap().into_value())).collect()
}

/// With no governance back end (an empty slot) a template is refused only where the block may
/// trigger a superblock: a superblock height on a chain with masternodes. The height after it
/// assembles, and so does a superblock height on a chain that never had a masternode.
#[test]
fn an_unavailable_governance_refuses_only_where_a_superblock_may_trigger() {
    let blocks = qc_blocks();
    let last = blocks.iter().map(|b| i64::from(b.header().time())).max().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let sut = NodeSut::with_harness(dir.path(), &QC_ARGS, last, ui(), Harness::default()).unwrap();
    for block in &blocks[..299] {
        submit(&sut, block);
    }
    let source = node_source(&sut);
    assert_eq!(
        source.create_new_block(&OP_TRUE, TemplateTxs::None).err(),
        Some(TemplateError::NotReady {
            height: 300,
            rule: "superblock",
            owner: "GV1b",
        })
    );
    submit(&sut, &blocks[299]);
    let template = source.create_new_block(&OP_TRUE, TemplateTxs::None).expect("not a superblock");
    assert_eq!(template.height, 301);
    assert!(template.superblock_payments.is_empty());
    assert_eq!(sut.down(), None);

    let dir = tempfile::tempdir().unwrap();
    let (sut, _) = mined(dir.path(), 99);
    let template =
        node_source(&sut).create_new_block(&OP_TRUE, TemplateTxs::None).expect("assembled");
    assert_eq!(template.height, 100);
    assert!(template.superblock_payments.is_empty());
    assert_eq!(sut.down(), None);
}

/// A back end that logs its read and answers with `answer`.
struct LoggingGovernance(fn() -> Result<SuperblockTemplate, Fatal>);

impl TemplateGovernance for LoggingGovernance {
    fn superblock_for_template(&self, h: u32, _: &MnList) -> Result<SuperblockTemplate, Fatal> {
        tracing::info!("fake governance read at {h}");
        (self.0)()
    }
}

/// `FillBlockPayments` reads governance after the CbTx, with the coins cell and the LLMQ view
/// released: the back end's log lines and its error come after the CbTx line. An unavailable
/// answer where a superblock may trigger is the same refusal, and a back-end fault stops V as
/// before.
// Core: v24.0.2 src/node/miner.cpp:285-338, src/masternode/payments.cpp:449-453
#[test]
fn the_governance_read_follows_the_cbtx() {
    let blocks = qc_blocks();
    let last = blocks.iter().map(|b| i64::from(b.header().time())).max().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let sut = NodeSut::with_harness(dir.path(), &QC_ARGS, last, ui(), Harness::default()).unwrap();
    for block in &blocks[..299] {
        submit(&sut, block);
    }
    let unavailable = LoggingGovernance(|| {
        Ok(SuperblockTemplate::Unavailable {
            owner: "GV1b",
        })
    });
    let slot = sut.steps.chainstate().template_governance_slot();
    assert!(slot.register(Arc::new(unavailable)).is_ok());
    let lines = captured(&sut);
    assert_eq!(
        node_source(&sut).create_new_block(&OP_TRUE, TemplateTxs::None).err(),
        Some(TemplateError::NotReady {
            height: 300,
            rule: "superblock",
            owner: "GV1b",
        })
    );
    let log = lines.messages();
    let order = [
        at(&log, "CreateNewBlock(): total size"),
        at(&log, "CbTx"),
        at(&log, "fake governance read at 300"),
    ];
    assert!(order.is_sorted(), "{order:?}: {log:#?}");
    assert_eq!(sut.down(), None);

    let dir = tempfile::tempdir().unwrap();
    let (sut, _) = mined(dir.path(), SUPERBLOCK - 1);
    let failing = LoggingGovernance(|| Err(Fatal::new("fake back-end failure")));
    let slot = sut.steps.chainstate().template_governance_slot();
    assert!(slot.register(Arc::new(failing)).is_ok());
    let lines = captured(&sut);
    assert_eq!(
        node_source(&sut).create_new_block(&OP_TRUE, TemplateTxs::None).err(),
        Some(TemplateError::Failed(
            "CreateNewBlock: the template's governance read: fake back-end failure".into()
        ))
    );
    let log = lines.messages();
    let order = [
        at(&log, "CreateNewBlock(): total size"),
        at(&log, "CbTx"),
        at(&log, "fake governance read at 120"),
    ];
    assert!(order.is_sorted(), "{order:?}: {log:#?}");
    let chain = sut.chain().expect("V ran");
    assert!(chain.view_handle().query(|_| ()).recv().is_err(), "the fault stops V");
}
