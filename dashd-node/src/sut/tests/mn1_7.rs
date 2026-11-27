//! MN1-7: §3.3's R1 (b) through the node's mempool adapter, on a node whose V runs through its
//! live pool, with the GBT engine over the real template source: the rebuild stores its own
//! second load, and a pool update after that load is in the template but not in the stored
//! value, so a call more than 5 seconds later rebuilds once more.
// Core: v24.0.2 src/rpc/mining.cpp:809-838

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU32, Ordering};

use dash_dml::MnList;
use dashd_mempool::Mempool;
use dashd_mempool::is::IsMempool;
use dashd_mempool::pool::EntryMetadata;
use dashd_mining::gbt::{ChainGbt, GbtParams, GbtParts, LongPoll, Mode};
use dashd_mining::testkit::FakeNodeStatus;
use dashd_mining::{
    ChainTemplateSource, GbtEngine, GbtReply, GbtRequest, GbtTemplate, MiningMempool, PackageView,
};

use super::mp1a9::{FEE, live, mature, spend_to};
use super::*;
use crate::mining_adapters::MiningPool;

type Hook = Box<dyn FnOnce() + Send>;

/// The node's adapter, counting its R1 loads and pool reads, with a hook after a given load and
/// one before the next pool read.
struct Hooked {
    adapter: MiningPool,
    loads: AtomicU32,
    reads: AtomicU32,
    after_load: Mutex<Option<(u32, Hook)>>,
    before_read: Mutex<Option<Hook>>,
}

impl MiningMempool for Hooked {
    fn with_read(&self, f: &mut dyn FnMut(&dyn PackageView)) {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if let Some(hook) = self.before_read.lock().unwrap().take() {
            hook();
        }
        self.adapter.with_read(f);
    }

    fn transactions_updated(&self) -> u32 {
        let value = self.adapter.transactions_updated();
        let loads = self.loads.fetch_add(1, Ordering::SeqCst) + 1;
        let mut after = self.after_load.lock().unwrap();
        if after.as_ref().is_some_and(|(at, _)| *at == loads) {
            (after.take().unwrap().1)();
        }
        value
    }
}

struct Rig {
    sut: NodeSut,
    blocks: Vec<Arc<dash_consensus::Block>>,
    pool: Arc<Mempool>,
    hooked: Arc<Hooked>,
    engine: GbtEngine,
}

impl Rig {
    /// 101 blocks on a live pool: block 1's coinbase spends in the next block.
    fn new(dir: &Path) -> Rig {
        let (sut, _) = live(dir, &[]);
        let blocks = mature(&sut, 101);
        let mempool = Arc::clone(sut.steps.mempool().expect("step 7 built it"));
        let hooked = Arc::new(Hooked {
            adapter: MiningPool::new(Arc::clone(&mempool)),
            loads: AtomicU32::new(0),
            reads: AtomicU32::new(0),
            after_load: Mutex::new(None),
            before_read: Mutex::new(None),
        });
        let chain = sut.chain().expect("V runs").clone();
        let params = sut.params().clone();
        let clock: Arc<dyn Clock> = sut.clock().clone();
        let source = ChainTemplateSource::new(
            chain.clone(),
            Arc::new(params.clone()),
            Arc::clone(&clock),
            None,
        )
        .with_mempool(Arc::clone(&hooked) as _);
        let engine = GbtEngine::new(GbtParts {
            chain: Arc::new(ChainGbt::new(chain, &params).expect("regtest's PoW")),
            source: Arc::new(source),
            mempool: Some(Arc::clone(&hooked) as _),
            status: Arc::new(FakeNodeStatus::default()),
            clock,
            params: GbtParams::of(&params).expect("regtest's"),
        });
        Rig {
            pool: Arc::clone(mempool.pool()),
            sut,
            blocks,
            hooked,
            engine,
        }
    }

    fn template(&self) -> GbtTemplate {
        let request = GbtRequest {
            mode: Mode::Template,
            rules: BTreeSet::new(),
            longpoll: LongPoll::None,
        };
        match self.engine.handle(request) {
            Ok(GbtReply::Template(t)) => t,
            other => panic!("expected a template, got {other:?}"),
        }
    }

    fn at(&self, seconds: i64) {
        self.sut.clock().set_mocktime(MOCKTIME + seconds);
    }

    fn count(&self, counter: &AtomicU32) -> u32 {
        counter.load(Ordering::SeqCst)
    }
}

/// An update between the cache check's load and the rebuild's is the stored value.
// Core: v24.0.2 src/rpc/mining.cpp:812-813, :819
#[test]
fn the_rebuild_stores_its_own_second_load() {
    let dir = tempfile::tempdir().unwrap();
    let rig = Rig::new(dir.path());
    let first = rig.template().longpollid.1;
    IsMempool::add_transactions_updated(&*rig.pool, 1);
    rig.at(6);
    let loads = rig.count(&rig.hooked.loads);
    let pool = Arc::clone(&rig.pool);
    *rig.hooked.after_load.lock().unwrap() =
        Some((loads + 1, Box::new(move || IsMempool::add_transactions_updated(&*pool, 1))));
    let second = rig.template().longpollid.1;
    assert_eq!(second, first.wrapping_add(2));
    assert_eq!(rig.count(&rig.hooked.loads), loads + 2);
    assert_eq!(rig.count(&rig.hooked.reads), 2, "two builds");
}

/// An update between the rebuild's load and the pool read is in the template (which passes
/// `TestBlockValidity`), not in the stored value, and rebuilds once more only past 5 seconds.
// Core: v24.0.2 src/rpc/mining.cpp:812-832
#[test]
fn an_update_after_the_second_load_rebuilds_again_later() {
    let dir = tempfile::tempdir().unwrap();
    let rig = Rig::new(dir.path());
    let tx = spend_to(&rig.blocks[0], &[0x51; 40]);
    let (pool, added) = (Arc::clone(&rig.pool), Arc::clone(&tx));
    *rig.hooked.before_read.lock().unwrap() = Some(Box::new(move || {
        let mut state = pool.write();
        let metadata = EntryMetadata {
            fee: FEE,
            height: 102,
            ..EntryMetadata::default()
        };
        let entry = state.new_entry(added, metadata);
        state.add_unchecked(entry, &MnList::default_pre_dip3());
    }));
    let before = rig.pool.transactions_updated();
    let t = rig.template();
    assert_eq!(t.transactions.iter().map(|x| x.txid).collect::<Vec<_>>(), [tx.txid()]);
    assert_eq!(t.longpollid.1, before);
    assert_eq!(rig.pool.transactions_updated(), before.wrapping_add(1));
    rig.at(5);
    assert_eq!(rig.template().longpollid.1, before);
    assert_eq!(rig.count(&rig.hooked.reads), 1, "cached");
    rig.at(6);
    assert_eq!(rig.template().longpollid.1, before.wrapping_add(1));
    assert_eq!(rig.count(&rig.hooked.reads), 2, "rebuilt");
}
