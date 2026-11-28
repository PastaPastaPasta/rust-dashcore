//! Admission releases peer/header locks, services published signals, and preserves order.

use std::cmp::Ordering;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use dash_consensus::{Block, Tx, TxRef};
use dash_p2p::{ConnectionType, PeerId, Permissions};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{Transaction, TxHash, TxType};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::compact::CompactTxSource;
use dashd_p2p::processing::peerman::Shared;
use dashd_p2p::processing::seams::{AcceptResult, AcceptResultType, AcceptWait, TxInfo, TxPool};
use dashd_p2p::sim::{SIM_START, Sim, SimTxPool};
use dashd_p2p::{ProcessorOptions, Registry, Seams};

type PeerProbe = (Arc<Shared>, PeerId);

#[derive(Clone, Default)]
struct DeferredPool {
    pool: SimTxPool,
    probe: Arc<Mutex<Option<PeerProbe>>>,
    /// Defer only block-sourced orphan retries (`from == None`); others resolve at once.
    retries_only: bool,
    /// The next deferred admission connects this block while it waits, instead of
    /// disconnecting one.
    connect: Arc<Mutex<Option<Arc<Block>>>>,
}

impl CompactTxSource for DeferredPool {
    fn for_each_in_vtxhashes_order(&self, visit: &mut dyn FnMut(&TxHash, &TxRef) -> bool) {
        self.pool.for_each_in_vtxhashes_order(visit);
    }
}

impl TxPool for DeferredPool {
    fn accept(&self, tx: TxRef, from: Option<PeerId>) -> AcceptWait {
        let (shared, id) = self.probe.lock().unwrap().clone().unwrap();
        if self.retries_only {
            if from.is_some() {
                return self.pool.accept(tx, from);
            }
        } else if from.is_none() {
            return self.pool.accept(tx, from);
        } else {
            assert_eq!(from, Some(id));
        }
        assert!(shared.with_state(id, |_| ()).is_some(), "peer/header locks must be released");
        let mut wait = self.pool.accept(tx, from);
        let mut first = true;
        let retries_only = self.retries_only;
        let connect = self.connect.lock().unwrap().take();
        AcceptWait(Box::pin(std::future::poll_fn(move |cx| {
            if first {
                first = false;
                match &connect {
                    Some(block) => shared.transactions_connected(block),
                    None => shared.block_disconnected(),
                }
                cx.waker().wake_by_ref();
                Poll::Pending
            } else if connect.is_some() {
                std::pin::Pin::new(&mut wait).poll(cx)
            } else {
                // An admission keeps the disconnect held until its tail; a block retry keeps
                // the block's own entry too, until the block's work is done.
                let expected = if retries_only {
                    2
                } else {
                    1
                };
                assert_eq!(
                    shared.signals().queue.held().0,
                    expected,
                    "a signal published during an admission is held until its tail"
                );
                std::pin::Pin::new(&mut wait).poll(cx)
            }
        })))
    }
    fn info(&self, txid: &TxHash) -> Option<TxInfo> {
        self.pool.info(txid)
    }
    fn asset_unlock_by_instance_hash(&self, hash: &TxHash) -> Option<TxHash> {
        self.pool.asset_unlock_by_instance_hash(hash)
    }
    fn exists(&self, txid: &TxHash) -> bool {
        self.pool.exists(txid)
    }
    fn recent_parents(&self, txid: &TxHash, since: i64) -> Vec<TxHash> {
        self.pool.recent_parents(txid, since)
    }
    fn info_all(&self) -> Vec<TxInfo> {
        self.pool.info_all()
    }
    fn compare_depth_and_score(&self, a: &TxHash, b: &TxHash) -> Ordering {
        self.pool.compare_depth_and_score(a, b)
    }
    fn unbroadcast(&self) -> Vec<TxHash> {
        self.pool.unbroadcast()
    }
    fn remove_unbroadcast(&self, txid: &TxHash, unchecked: bool) {
        self.pool.remove_unbroadcast(txid, unchecked);
    }
    fn size(&self) -> usize {
        self.pool.size()
    }
    fn dynamic_memory_usage(&self) -> usize {
        self.pool.dynamic_memory_usage()
    }
}

#[test]
fn pending_admission_drains_signals_without_peer_locks_and_completes_in_order() {
    use dash_consensus::primitives::CoreSerialize;
    use dash_types::Numeric;
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&params, &IdentityOptions::new()).unwrap();
    let pool = DeferredPool::default();
    let mut sim = Sim::with(
        params,
        id,
        ProcessorOptions::default(),
        Registry::new(),
        Seams {
            txpool: Arc::new(pool.clone()),
            ..Seams::default()
        },
        |_| {},
    );
    sim.chain.set_ibd(false);
    let p = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    *pool.probe.lock().unwrap() = Some((Arc::clone(&sim.shared), p.handle.id()));
    let txs: Vec<_> = (1..=2)
        .map(|tag| {
            Arc::new(Tx::new(Transaction::new(1, TxType::Spend, vec![], vec![], tag, vec![])))
        })
        .collect();
    for tx in &txs {
        sim.lp.logic_mut().recent_confirmed.insert(&[12; 32]);
        sim.admitted(&p, "tx", tx.raw().core_serialize()).unwrap();
        assert!(!sim.lp.logic().recent_confirmed.contains(&[12; 32]));
        assert!(pool.exists(&tx.txid()));
    }
    assert_eq!(
        pool.pool.submissions().iter().map(|(t, _)| t.txid().to_lendian()).collect::<Vec<_>>(),
        txs.iter().map(|t| t.txid().to_lendian()).collect::<Vec<_>>()
    );
    assert_eq!(p.handle.last_tx_time(), SIM_START);
}

#[test]
fn signals_published_during_a_block_retry_apply_after_the_block() {
    use dash_consensus::primitives::CoreSerialize;
    use dash_consensus::{Header, TxValidationResult};
    use dash_primitives::{BlockHash, BlockHeader, MerkleRoot, OutPoint, TxIn, TxOut};
    use dash_types::Numeric;
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&params, &IdentityOptions::new()).unwrap();
    let pool = DeferredPool {
        retries_only: true,
        ..DeferredPool::default()
    };
    let mut sim = Sim::with(
        params,
        id,
        ProcessorOptions::default(),
        Registry::new(),
        Seams {
            txpool: Arc::new(pool.clone()),
            ..Seams::default()
        },
        |_| {},
    );
    sim.chain.set_ibd(false);
    let p = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    *pool.probe.lock().unwrap() = Some((Arc::clone(&sim.shared), p.handle.id()));
    let output = TxOut::new(Default::default(), vec![0x51].into());
    let parent =
        Arc::new(Tx::new(Transaction::new(1, TxType::Spend, vec![], vec![output], 40, vec![])));
    let input = TxIn::new(OutPoint::new(parent.txid(), 0), vec![].into(), u32::MAX);
    let child =
        Arc::new(Tx::new(Transaction::new(1, TxType::Spend, vec![input], vec![], 41, vec![])));
    let reply = |validation_result| AcceptResult {
        result_type: if validation_result == TxValidationResult::Unset {
            AcceptResultType::Valid
        } else {
            AcceptResultType::Invalid
        },
        validation_result,
        reason: "reason".into(),
        debug_message: "debug detail".into(),
        vsize: None,
        base_fee: None,
    };
    pool.pool.push_result(reply(TxValidationResult::MissingInputs));
    pool.pool.push_result(reply(TxValidationResult::Unset));
    sim.admitted(&p, "tx", child.raw().core_serialize()).unwrap();
    assert!(sim.lp.logic().tx_orphans.have_tx(&child.txid()));

    let block = Arc::new(Block::new(
        Header::new(BlockHeader::new(1, BlockHash::ZERO, MerkleRoot::ZERO, 0, 0, 0)),
        vec![parent.clone()],
    ));
    sim.shared.transactions_connected(&block);
    sim.lp.logic_mut().drain_sync_signals();

    assert!(pool.exists(&child.txid()));
    assert!(
        !sim.lp.logic().recent_confirmed.contains(&parent.txid().to_lendian()),
        "the disconnect published during the retry resets after the block records its txids"
    );
    assert_eq!(sim.shared.signals().queue.held(), (0, 0));
}

/// Core holds `cs_main` from `AlreadyHave` through a transaction's tail (np:4941-5067), so a
/// block connected while the admission waits runs `BlockConnected` after the tail: the orphan
/// the tail adds is the block's candidate, retried with no source peer.
#[test]
fn a_block_connected_during_an_admission_retries_the_new_orphan_after_the_tail() {
    use dash_consensus::primitives::CoreSerialize;
    use dash_consensus::{Header, TxValidationResult};
    use dash_primitives::{BlockHash, BlockHeader, MerkleRoot, OutPoint, TxIn, TxOut};
    use dash_types::Numeric;
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&params, &IdentityOptions::new()).unwrap();
    let output = TxOut::new(Default::default(), vec![0x51].into());
    let parent =
        Arc::new(Tx::new(Transaction::new(1, TxType::Spend, vec![], vec![output], 50, vec![])));
    let input = TxIn::new(OutPoint::new(parent.txid(), 0), vec![].into(), u32::MAX);
    let child =
        Arc::new(Tx::new(Transaction::new(1, TxType::Spend, vec![input], vec![], 51, vec![])));
    let block = Arc::new(Block::new(
        Header::new(BlockHeader::new(1, BlockHash::ZERO, MerkleRoot::ZERO, 0, 0, 0)),
        vec![parent.clone()],
    ));
    let pool = DeferredPool::default();
    *pool.connect.lock().unwrap() = Some(block);
    let mut sim = Sim::with(
        params,
        id,
        ProcessorOptions::default(),
        Registry::new(),
        Seams {
            txpool: Arc::new(pool.clone()),
            ..Seams::default()
        },
        |_| {},
    );
    sim.chain.set_ibd(false);
    let p = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    *pool.probe.lock().unwrap() = Some((Arc::clone(&sim.shared), p.handle.id()));
    pool.pool.push_result(AcceptResult {
        result_type: AcceptResultType::Invalid,
        validation_result: TxValidationResult::MissingInputs,
        reason: "bad-txns-inputs-missingorspent".into(),
        debug_message: String::new(),
        vsize: None,
        base_fee: None,
    });

    sim.admitted(&p, "tx", child.raw().core_serialize()).unwrap();

    let submissions: Vec<_> =
        pool.pool.submissions().iter().map(|(t, from)| (t.txid(), *from)).collect();
    assert_eq!(submissions, [(child.txid(), Some(p.handle.id())), (child.txid(), None)]);
    assert!(!sim.lp.logic().tx_orphans.have_tx(&child.txid()));
    assert!(pool.exists(&child.txid()));
    assert_eq!(sim.shared.signals().queue.held(), (0, 0));
}

/// Core's `ProcessOrphanTx` holds `cs_main` around its admission (np:3393-3395): a block that
/// confirms an orphan's other parent while the sender's reconsideration waits runs
/// `BlockConnected` after it, so the accepted orphan is not the block's candidate and is
/// submitted once.
#[test]
fn a_block_connected_during_an_orphan_reconsideration_applies_after_it() {
    use dash_consensus::primitives::CoreSerialize;
    use dash_consensus::{Header, TxValidationResult};
    use dash_primitives::{BlockHash, BlockHeader, MerkleRoot, OutPoint, TxIn, TxOut};
    use dash_types::Numeric;
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&params, &IdentityOptions::new()).unwrap();
    let parent = |tag| {
        let output = TxOut::new(Default::default(), vec![0x51].into());
        Arc::new(Tx::new(Transaction::new(1, TxType::Spend, vec![], vec![output], tag, vec![])))
    };
    let (sent, mined) = (parent(60), parent(61));
    let inputs = [&sent, &mined]
        .map(|p| TxIn::new(OutPoint::new(p.txid(), 0), vec![].into(), u32::MAX))
        .to_vec();
    let orphan = Arc::new(Tx::new(Transaction::new(1, TxType::Spend, inputs, vec![], 62, vec![])));
    let pool = DeferredPool::default();
    let mut sim = Sim::with(
        params,
        id,
        ProcessorOptions::default(),
        Registry::new(),
        Seams {
            txpool: Arc::new(pool.clone()),
            ..Seams::default()
        },
        |_| {},
    );
    sim.chain.set_ibd(false);
    let p = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    *pool.probe.lock().unwrap() = Some((Arc::clone(&sim.shared), p.handle.id()));
    pool.pool.push_result(AcceptResult {
        result_type: AcceptResultType::Invalid,
        validation_result: TxValidationResult::MissingInputs,
        reason: "bad-txns-inputs-missingorspent".into(),
        debug_message: String::new(),
        vsize: None,
        base_fee: None,
    });
    sim.admitted(&p, "tx", orphan.raw().core_serialize()).unwrap();
    sim.admitted(&p, "tx", sent.raw().core_serialize()).unwrap();
    assert!(sim.lp.logic().tx_orphans.have_tx(&orphan.txid()));
    assert!(sim.lp.logic().tx_orphans.have_more_work(p.handle.id()));
    *pool.connect.lock().unwrap() = Some(Arc::new(Block::new(
        Header::new(BlockHeader::new(1, BlockHash::ZERO, MerkleRoot::ZERO, 0, 0, 0)),
        vec![mined.clone()],
    )));

    sim.lp.logic_mut().process_messages(&p.handle);

    let submissions: Vec<_> = pool
        .pool
        .submissions()
        .iter()
        .filter(|(t, _)| t.txid() == orphan.txid())
        .map(|(_, from)| *from)
        .collect();
    assert_eq!(submissions, [Some(p.handle.id()), Some(p.handle.id())]);
    assert!(!sim.lp.logic().tx_orphans.have_tx(&orphan.txid()));
    assert!(pool.exists(&orphan.txid()));
    assert!(sim.lp.logic().recent_confirmed.contains(&mined.txid().to_lendian()));
    assert_eq!(sim.shared.signals().queue.held(), (0, 0));
}
