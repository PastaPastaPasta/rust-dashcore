//! MP1a-4's actor protocol, held-pool lifetime, ordering and production usage path.

use super::super::abc::Breaks;
use super::super::support::mine_version;
use super::*;
use crate::chain::mempool_turn::{LockOrderGuard, LockRank};
use crate::chain::reply::reply;
use crate::chain::{HeldMempool, MempoolCompletion, Queued, ReorgCx, Staged, TipFacts};
use crate::coins::{CoinSrc, CoinsTable, OutPointKey, PackedCoin};
use dash_consensus::TxRef;
use dash_consensus::ctx::{Deployment, DeploymentPos};
use dash_consensus::deployments::VERSIONBITS_TOP_BITS;
use dash_consensus::script::ScriptFlags;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::{OutPoint, TxOut};
use dashd_store::Commit;

#[derive(Default)]
struct PoolState {
    busy: bool,
    usage: u64,
    sequence: u64,
    trace: Vec<&'static str>,
}

#[derive(Clone, Default)]
struct Pool {
    state: Arc<(Mutex<PoolState>, Condvar)>,
    events: Arc<AtomicBool>,
    drain_fault: Arc<AtomicBool>,
    fault: Arc<Mutex<Option<Arc<GuardedEngine<ProbeEngine>>>>>,
    sample_entered: Arc<Mutex<Option<mpsc::Sender<()>>>>,
}

impl Pool {
    fn acquire(&self) -> Held {
        let (lock, wake) = &*self.state;
        let state = lock.lock().expect("pool");
        let mut state = wake.wait_while(state, |s| s.busy).expect("pool");
        state.busy = true;
        state.trace.push("begin");
        Held(self.clone())
    }

    fn busy(&self) -> bool {
        self.state.0.lock().expect("pool").busy
    }
    fn trace(&self) -> Vec<&'static str> {
        self.state.0.lock().expect("pool").trace.clone()
    }
    fn next_sequence(&self) -> u64 {
        let mut state = self.state.0.lock().expect("pool");
        state.sequence += 1;
        state.sequence
    }
    fn clear(&self) {
        self.state.0.lock().expect("pool").trace.clear();
    }
}

struct Held(Pool);

impl Drop for Held {
    fn drop(&mut self) {
        let mut state = self.0.state.0.lock().expect("pool");
        state.trace.push("drop");
        state.busy = false;
        self.0.state.1.notify_all();
    }
}

impl MempoolSink for Pool {
    fn begin_hold(&mut self) -> Option<Box<dyn HeldMempool>> {
        Some(Box::new(self.acquire()))
    }
    fn new_disconnect_pool(&mut self) -> Box<dyn DisconnectPool> {
        Box::new(())
    }
    fn dynamic_memory_usage(&self) -> u64 {
        if let Some(entered) = self.sample_entered.lock().expect("sample").take() {
            entered.send(()).expect("sampler");
        }
        let state = self.state.0.lock().expect("pool");
        let state = self.state.1.wait_while(state, |s| s.busy).expect("pool");
        state.usage
    }
}

impl HeldMempool for Held {
    fn remove_recursive(&mut self, _: &TxRef, _: RemovalReason, _: &mut Staged) {}
    fn add_transactions_updated(&mut self, _: u32, _: &mut Staged) {}
    fn check(&mut self, _: u32, _: &mut Staged) {}

    fn dynamic_memory_usage(&self) -> u64 {
        self.0.state.0.lock().expect("pool").usage
    }
    fn remove_expired_asset_unlock(&mut self, height: u32, staged: &mut Staged) {
        if self.0.events.load(Ordering::SeqCst) {
            staged.push(Queued::TransactionRemovedFromMempool {
                tx: coinbase(&[height as u8, 1]),
                reason: RemovalReason::Expiry,
                sequence: self.0.next_sequence(),
            });
        }
    }
    fn remove_for_block_at(
        &mut self,
        vtx: &[TxRef],
        _height: u32,
        _: &Arc<dash_dml::MnList>,
        staged: &mut Staged,
    ) {
        if self.0.events.load(Ordering::SeqCst) {
            staged.push(Queued::TransactionRemovedFromMempool {
                tx: vtx[0].clone(),
                reason: RemovalReason::Conflict,
                sequence: self.0.next_sequence(),
            });
        }
    }
    fn maybe_update_for_reorg_at(
        &mut self,
        _: Box<dyn DisconnectPool>,
        _: Option<&Arc<dash_dml::MnList>>,
        cx: &mut dyn ReorgCx,
        staged: &mut Staged,
    ) -> Result<(), Fatal> {
        assert!(self.0.busy());
        if self.0.drain_fault.swap(false, Ordering::SeqCst) {
            return Err(Fatal::new("mempool drain fault"));
        }
        if let Some(db) = self.0.fault.lock().expect("fault").take() {
            db.inner().fail_reads(Some(ErrorKind::Io));
            staged.push(Queued::TransactionRemovedFromMempool {
                tx: coinbase(&[99, 1]),
                reason: RemovalReason::Reorg,
                sequence: 999,
            });
            cx.access_coin(&outpoint(99))?;
        }
        Ok(())
    }
}

fn outpoint(n: u8) -> OutPoint {
    OutPoint::new(TxHash::from([n; 32]), 0)
}

#[derive(Default)]
struct Events(Mutex<Vec<String>>);
impl ValidationInterface for Events {
    fn transaction_removed_from_mempool(&self, tx: &TxRef, reason: RemovalReason, seq: u64) {
        self.0.lock().expect("events").push(format!("remove {reason:?} {seq} {}", tx.txid()));
    }
    fn transaction_added_to_mempool(&self, tx: &TxRef, time: i64, seq: u64) {
        self.0.lock().expect("events").push(format!("add {time} {seq} {}", tx.txid()));
    }
    fn notify_instant_send_double_spend_attempt(&self, tx: &TxRef, conflict: &TxRef) {
        self.0.lock().expect("events").push(format!("double {} {}", tx.txid(), conflict.txid()));
    }
    fn block_connected(&self, _: &Arc<Block>, id: BlockId) {
        self.0.lock().expect("events").push(format!("block {}", id.0));
    }
    fn block_disconnected(&self, _: &Arc<Block>, id: BlockId) {
        self.0.lock().expect("events").push(format!("disconnect {}", id.0));
    }
    fn updated_block_tip(&self, new: BlockId, _: Option<BlockId>, _: bool) {
        self.0.lock().expect("events").push(format!("tip {}", new.0));
    }
    fn chain_state_flushed(&self, _: &[BlockHash]) {
        self.0.lock().expect("events").push("flush".into());
    }
}

fn rig_with(s: &Stores, pool: &Pool) -> Rig {
    let mut cfg = s.config_with(Seams::default());
    cfg.mempool = Some(Box::new(pool.clone()));
    Rig::with(cfg)
}

struct ProtocolFlush {
    inner: Box<dyn FlushSite>,
    pool: Pool,
    fail: Arc<AtomicBool>,
    coins: ChainstateCell<ProbeEngine>,
    cached: Arc<AtomicBool>,
}
impl FlushSite for ProtocolFlush {
    fn attach(&mut self, bell: crate::chain::FlushBell) {
        self.inner.attach(bell);
    }
    fn at_site(
        &mut self,
        mode: FlushStateMode,
        host: &mut FlushHost<'_>,
    ) -> Result<SiteDecision, Fatal> {
        if mode == FlushStateMode::Periodic {
            assert!(!self.pool.busy(), "admission released the pool before flushing");
            assert_eq!(
                self.coins.lock().expect("coins").have_in_cache(&OutPointKey::new(&outpoint(1))),
                self.cached.load(Ordering::SeqCst)
            );
            self.pool.state.0.lock().expect("pool").trace.push("periodic");
            if self.fail.load(Ordering::SeqCst) {
                return Err(Fatal::new("MP1a periodic fault"));
            }
        }
        self.inner.at_site(mode, host)
    }
    fn after_step(
        &mut self,
        decision: SiteDecision,
        host: &mut FlushHost<'_>,
    ) -> Result<(), Fatal> {
        self.inner.after_step(decision, host)?;
        self.pool.state.0.lock().expect("pool").trace.push("after");
        Ok(())
    }
    fn service(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.inner.service(host)
    }
    fn stop(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.inner.stop(host)
    }
    fn abort(&mut self, cause: AbortCause, host: &mut FlushHost<'_>) -> bool {
        self.inner.abort(cause, host)
    }
    fn latched(&self) -> Option<Fatal> {
        self.inner.latched()
    }
}

#[test]
fn admission_outcomes_uncache_flush_scope_end_then_reply() {
    for case in ["reject", "test_accept", "admit", "coins_fault", "flush_fault"] {
        let s = Stores::new();
        let pool = Pool::default();
        let cached = Arc::new(AtomicBool::new(false));
        let fail = Arc::new(AtomicBool::new(false));
        let mut cfg = s.config_with(Seams::default());
        cfg.flush = Some(Box::new(ProtocolFlush {
            inner: cfg.flush.take().expect("flush"),
            pool: pool.clone(),
            fail: fail.clone(),
            coins: s.coins.clone(),
            cached: cached.clone(),
        }));
        cfg.mempool = Some(Box::new(pool.clone()));
        let mut rig = Rig::with(cfg);
        let mut batch = s.db.batch();
        let coin = PackedCoin::from_output(&TxOut::from_duffs(7, vec![0x51].into()), 0, false)
            .expect("coin");
        CoinsTable::default().put(&mut batch, &OutPointKey::new(&outpoint(1)), coin.as_bytes());
        s.db.write(batch, Commit::Buffered).expect("seed");
        pool.clear();
        let before = rig.v.view().view_seq;
        let view = rig.v.view_handle();
        let (tx, result) = reply();
        let p = pool.clone();
        let expect_cached = cached.clone();
        fail.store(case == "flush_fault", Ordering::SeqCst);
        let db = s.db.clone();
        rig.v
            .send(ChainCommand::Mempool(Box::new(move |turn| {
                assert!(!p.busy(), "class M runs outside holds");
                let guard = p.acquire();
                let _order = LockOrderGuard::enter(LockRank::Mempool);
                assert_eq!(turn.tip_height(), Some(0));
                assert!(turn.script_caches().is_some());
                assert_eq!(turn.mn_list().height(), -1);
                assert!(Arc::ptr_eq(turn.mn_list(), &view.view().mn_list));
                assert_eq!(
                    turn.credit_pool().map(Arc::as_ptr),
                    view.view().credit_pool.as_ref().map(Arc::as_ptr)
                );
                assert_eq!(turn.bls(), view.view().bls);
                if case == "coins_fault" {
                    db.inner().fail_reads(Some(ErrorKind::Io));
                }
                turn.with_atmp_coins(&mut |coins| {
                    assert!(!coins.have_in_cache(&outpoint(1)));
                    assert_eq!(coins.fetch(&outpoint(1))?.expect("seeded").as_ref().value(), 7);
                    assert!(coins.have_in_cache(&outpoint(1)));
                    Ok(())
                })?;
                let uncache = if case == "admit" {
                    expect_cached.store(true, Ordering::SeqCst);
                    vec![]
                } else {
                    vec![outpoint(1)]
                };
                drop(guard);
                Ok(MempoolCompletion {
                    uncache,
                    reply: Box::new(move || {
                        assert!(view.view().view_seq > before, "scope end published before reply");
                        assert_eq!(p.trace(), ["begin", "drop", "periodic", "after"]);
                        tx.send(case);
                    }),
                })
            })))
            .expect("send");
        if matches!(case, "coins_fault" | "flush_fault") {
            assert_eq!(result.recv(), Err(WaitError::Disconnected), "{case}");
            assert!(!pool.busy());
            let exit = rig.stop();
            assert!(exit.fatal.is_some());
            assert_eq!(s.sink.reports().len(), 1, "one fault report: {case}");
            assert_eq!(rig.v.view().view_seq, before, "no completed scope: {case}");
        } else {
            assert_eq!(result.recv(), Ok(case));
            rig.assert_no_abort();
        }
    }
}

#[test]
fn admission_without_coins_refuses_without_aborting_v() {
    let rig = Rig::new();
    let (done, result) = reply();
    rig.v
        .send(ChainCommand::Mempool(Box::new(move |turn| {
            let called = Arc::new(AtomicBool::new(false));
            let callback_called = called.clone();
            assert_eq!(
                turn.with_atmp_coins(&mut |_| {
                    callback_called.store(true, Ordering::SeqCst);
                    Ok(())
                })?,
                None
            );
            assert!(!called.load(Ordering::SeqCst));
            Ok(MempoolCompletion {
                uncache: vec![],
                reply: Box::new(move || done.send(())),
            })
        })))
        .expect("command");
    result.recv().expect("admission completed");
    rig.assert_no_abort();
}

#[test]
fn held_pool_is_released_after_splice_and_on_error_or_unwind() {
    let s = Stores::new();
    let pool = Pool::default();
    let rig = rig_with(&s, &pool);
    pool.clear();
    let p = pool.clone();
    rig.on_v(move |v| {
        let trace = p.clone();
        v.release_hook = Some(Arc::new(move |stage| {
            if stage == Stage::Published {
                assert!(trace.busy());
                trace.state.0.lock().expect("pool").trace.push("published");
            }
            if stage == Stage::Spliced {
                assert!(!trace.busy());
                trace.state.0.lock().expect("pool").trace.push("spliced");
            }
        }));
        let mut h = v.hold();
        assert!(p.busy());
        h.staged.push(Queued::TransactionRemovedFromMempool {
            tx: coinbase(&[1, 2]),
            reason: RemovalReason::Reorg,
            sequence: 0,
        });
        let r = h.release();
        assert!(p.busy());
        v.after_release(r);
        assert_eq!(p.trace(), ["begin", "published", "drop", "spliced"]);
        p.clear();
        fn fail_hold(v: &mut V) -> Result<(), Fatal> {
            let _h = v.hold();
            Err(Fatal::new("hold fault"))
        }
        assert!(fail_hold(v).is_err());
        assert!(!p.busy());
        assert_eq!(p.trace(), ["begin", "drop"]);
        p.clear();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (_, _h) = v.lend();
            panic!("hold unwind");
        }));
        assert!(panicked.is_err());
        assert!(!p.busy());
        assert_eq!(p.trace(), ["begin", "drop"]);
        v.release_hook = None;
        Ok(())
    });
}

#[test]
fn reorg_coin_fault_discards_staged_events_and_releases_pool() {
    for enforce in [false, true] {
        let s = Stores::new();
        let pool = Pool::default();
        let mut cfg = s.config_with(Seams::default());
        cfg.chainlocks = enforce;
        cfg.mempool = Some(Box::new(pool.clone()));
        let mut rig = Rig::with(cfg);
        let main = branch(&rig.p, &rig.genesis, 2, 0);
        for m in &main {
            rig.accept(m);
        }
        let events = Arc::new(Events::default());
        rig.sync();
        rig.signals.register(events.clone());
        if enforce {
            let side = branch(&rig.p, &rig.genesis, 1, 1);
            rig.headers(&side);
            intake(&rig);
            *pool.fault.lock().expect("fault") = Some(s.db.clone());
            record(&rig, &lock(&side[0]));
        } else {
            *pool.fault.lock().expect("fault") = Some(s.db.clone());
            assert_eq!(rig.v.invalidate_block(main[0].hash()).recv(), Err(WaitError::Disconnected));
        }
        let exit = rig.stop();
        assert!(exit.fatal.is_some(), "enforce={enforce}");
        assert!(!pool.busy());
        rig.sync();
        assert!(events.0.lock().expect("events").iter().all(|e| !e.starts_with("remove")));
        assert_eq!(s.sink.reports().len(), 1, "one read report, enforce={enforce}");
    }
}

#[test]
fn lock_order_accepts_v_pool_coins_and_rejects_coins_pool() {
    let _v = LockOrderGuard::enter(LockRank::V);
    {
        let _pool = LockOrderGuard::enter(LockRank::Mempool);
        assert!(std::panic::catch_unwind(|| LockOrderGuard::enter(LockRank::Mempool)).is_err());
        let _coins = LockOrderGuard::enter(LockRank::Coins);
    }
    if cfg!(debug_assertions) {
        let _coins = LockOrderGuard::enter(LockRank::Coins);
        assert!(std::panic::catch_unwind(|| LockOrderGuard::enter(LockRank::Mempool)).is_err());
    }
}

#[test]
fn pool_events_interleave_with_two_steps_and_a_racing_off_v_removal() {
    let s = Stores::new();
    let pool = Pool::default();
    let rig = rig_with(&s, &pool);
    let blocks = branch(&rig.p, &rig.genesis, 2, 0);
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let events = Arc::new(Events::default());
    rig.sync();
    rig.signals.register(events.clone());
    pool.events.store(true, Ordering::SeqCst);
    let mut expected = Vec::new();
    for (block, id) in blocks.iter().zip(&ids) {
        expected.push(format!(
            "remove Conflict {} {}",
            block.height * 2 - 1,
            block.block.txs()[0].txid()
        ));
        expected.push(format!(
            "remove Expiry {} {}",
            block.height * 2,
            coinbase(&[block.height as u8, 1]).txid()
        ));
        expected.push(format!("block {}", id.0));
    }
    expected.push(format!("tip {}", ids[1].0));
    expected.push(format!("remove Conflict 99 {}", coinbase(&[8, 9]).txid()));
    let p = pool.clone();
    let signals = rig.signals.clone();
    let (racing, started) = mpsc::channel();
    let (go, ready) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        ready.recv().expect("hold started");
        racing.send(()).expect("racer starts");
        let _rank = LockOrderGuard::enter(LockRank::Mempool);
        let _held = p.acquire();
        signals.enqueue(Queued::TransactionRemovedFromMempool {
            tx: coinbase(&[8, 9]),
            reason: RemovalReason::Conflict,
            sequence: 99,
        });
    });
    rig.on_v(move |v| {
        let mut h = v.hold();
        go.send(()).expect("racer");
        started.recv().expect("racer started");
        for (m, id) in blocks.iter().zip(ids.iter()) {
            h.remove_for_block(m.block.txs(), m.height)?;
            h.held_pool
                .as_deref_mut()
                .expect("held")
                .remove_expired_asset_unlock(m.height, &mut h.staged);
            h.staged.push(Queued::BlockConnected {
                block: m.block.clone(),
                id: *id,
            });
        }
        h.updated_block_tip(ids[1], Some(ids[0]), true);
        let r = h.release();
        v.after_release(r);
        Ok(())
    });
    writer.join().expect("racer finished");
    rig.sync();
    let got = events.0.lock().expect("events").clone();
    assert_eq!(got, expected, "the whole callback stream, including removed transactions");
}

#[test]
fn ordinary_usage_sampling_waits_for_the_whole_off_v_batch_before_coins() {
    let s = Stores::new();
    *s.coins.lock().expect("coins") = CoinsCache::new(
        s.db.clone(),
        CoinsBudget::new(2 << 20, 8 << 20, Ratio::HALF, 0),
        Salt::fixed(7),
        None,
    );
    let pool = Pool::default();
    let rig = rig_with(&s, &pool);
    let blocks = branch(&rig.p, &rig.genesis, 1, 0);
    rig.accept(&blocks[0]);
    let coins = s.coins.clone();
    rig.on_v(move |_| {
        let mut coins = crate::connect::lock_cell(&coins)?;
        for n in 10..50 {
            let out = TxOut::from_duffs(1, vec![0x51; 1000].into());
            coins
                .add(
                    &OutPointKey::new(&outpoint(n)),
                    CoinSrc::Output {
                        out: &out,
                        height: 0,
                        coinbase: false,
                    },
                    false,
                )
                .expect("seed");
        }
        let share = coins.usage().total() / 2;
        coins.resize(share, false);
        assert_eq!(coins.boundary_state(0, false), CoinsBoundary::Ok);
        assert_ne!(coins.boundary_state(4 << 20, false), CoinsBoundary::Critical);
        assert_eq!(coins.boundary_state(8 << 20, false), CoinsBoundary::Critical);
        Ok(())
    });
    rig.v.flush(FlushStateMode::Periodic).recv().expect("old usage flush");
    rig.on_v(|v| v.with_flush(|f, host| f.stop(host)));
    rig.sync();
    let events = Arc::new(Events::default());
    rig.signals.register(events.clone());
    let (paused, batch_paused) = mpsc::channel();
    let (resume, finish) = mpsc::channel();
    let p = pool.clone();
    let writer = std::thread::spawn(move || {
        let _rank = LockOrderGuard::enter(LockRank::Mempool);
        let _held = p.acquire();
        p.state.0.lock().expect("pool").usage = 4 << 20;
        paused.send(()).expect("batch paused");
        finish.recv().expect("finish batch");
        p.state.0.lock().expect("pool").usage = 8 << 20;
    });
    batch_paused.recv().expect("first root removed");
    let (entered, sampled) = mpsc::channel();
    *pool.sample_entered.lock().expect("sample") = Some(entered);
    let result = rig.v.flush(FlushStateMode::Periodic);
    sampled.recv().expect("usage sampling started");
    assert!(s.coins.try_lock().is_ok(), "usage sampling precedes the coins cell");
    assert_eq!(
        result.recv_timeout(Duration::from_millis(20)),
        None,
        "flush waits for the pool writer"
    );
    resume.send(()).expect("finish batch");
    writer.join().expect("writer");
    result.recv().expect("flush completed");
    rig.on_v(|v| v.with_flush(|f, host| f.stop(host)));
    rig.sync();
    assert!(
        events.0.lock().expect("events").iter().any(|e| e == "flush"),
        "the completed batch causes a real flush and ChainStateFlushed"
    );
    assert_eq!(
        rig.v.invalidate_block(blocks[0].hash()).recv(),
        Ok(Ok(())),
        "held disconnect flush reads usage without relocking"
    );
    assert!(!pool.busy());
    rig.assert_no_abort();
}

#[test]
fn atmp_cache_membership_uncache_and_d12_use_the_active_coins() {
    let s = Stores::new();
    let pool = Pool::default();
    let rig = rig_with(&s, &pool);
    let blocks = branch(&rig.p, &rig.genesis, 1, 0);
    rig.accept(&blocks[0]);
    let op = OutPoint::new(blocks[0].block.txs()[0].txid(), 0);
    let (tx, reply) = reply();
    rig.v
        .send(ChainCommand::Mempool(Box::new(move |turn| {
            turn.with_atmp_coins(&mut |coins| {
                assert!(coins.have_in_cache(&op), "mempool_accept: connected outputs are cached");
                coins.uncache(&op);
                assert!(coins.have_in_cache(&op), "dirty entries are retained");
                assert!(coins.access(&outpoint(88))?.is_none(), "absent is not a read error");
                Ok(())
            })?;
            Ok(MempoolCompletion {
                uncache: vec![],
                reply: Box::new(move || tx.send(())),
            })
        })))
        .expect("command");
    reply.recv().expect("admission completed");
    rig.v.flush(FlushStateMode::Always).recv().expect("durable");
    let (tx, reply) = crate::chain::reply::reply();
    rig.v
        .send(ChainCommand::Mempool(Box::new(move |turn| {
            turn.with_atmp_coins(&mut |coins| {
                assert!(!coins.have_in_cache(&op), "D12: released cache membership is gone");
                assert!(coins.fetch(&op)?.is_some(), "the database coin remains");
                assert!(coins.have_in_cache(&op));
                coins.uncache(&op);
                assert!(!coins.have_in_cache(&op), "clean fetched entries are removed");
                Ok(())
            })?;
            Ok(MempoolCompletion {
                uncache: vec![],
                reply: Box::new(move || tx.send(())),
            })
        })))
        .expect("command");
    reply.recv().expect("admission completed");
}

#[test]
fn mempool_turn_binds_rules_and_reorg_context_to_the_tip() {
    let s = Stores::new();
    let pool = Pool::default();
    let rig = rig_with(&s, &pool);
    let active = branch(&rig.p, &rig.genesis, 1, 0);
    rig.accept(&active[0]);
    let side = branch(&rig.p, &rig.genesis, 1, 1);
    rig.headers(&side);
    let active_id = rig.id(&active[0]);
    let side_id = rig.id(&side[0]);
    let active_coin = OutPoint::new(active[0].block.txs()[0].txid(), 0);
    let params = rig.p.clone();
    let (done, result) = reply();
    rig.v
        .send(ChainCommand::Mempool(Box::new(move |turn| {
            assert!(turn.chain_contains(active_id));
            assert!(!turn.chain_contains(side_id));
            assert!(turn.access_coin(&active_coin)?.is_some());
            assert!(turn.access_coin(&outpoint(255))?.is_none());

            let height = turn.tip_height().expect("active tip") + 1;
            let rules = turn.special_tx_rules()?;
            assert_eq!(
                rules.ipv4_optional,
                height >= params.consensus.ipv4_optional_height.to_u32()
            );
            assert_eq!(
                rules.v24,
                turn.deployment_active_after(Deployment::Versionbits(DeploymentPos::V24))?
            );
            assert_eq!(turn.block_script_flags(), Some(ScriptFlags::MANDATORY_V24_0));
            Ok(MempoolCompletion {
                uncache: vec![],
                reply: Box::new(move || done.send(())),
            })
        })))
        .expect("command");
    result.recv().expect("admission completed");
    rig.assert_no_abort();
}

/// What one turn binds at V's tip: the tip's height, `special_tx_rules`' V24, EvoShares and
/// IPv4-optional answers, and `block_script_flags`.
fn rules_at_tip(rig: &Rig) -> (u32, bool, bool, bool, Option<ScriptFlags>) {
    let (seen, rules) = mpsc::channel();
    let (done, result) = reply();
    rig.v
        .send(ChainCommand::Mempool(Box::new(move |turn| {
            let rules = turn.special_tx_rules()?;
            let height = turn.tip_height().expect("active tip");
            let flags = turn.block_script_flags();
            let r = (height, rules.v24, rules.evo_shares, rules.ipv4_optional, flags);
            seen.send(r).expect("test");
            Ok(MempoolCompletion {
                uncache: vec![],
                reply: Box::new(move || done.send(())),
            })
        })))
        .expect("command");
    result.recv().expect("admission completed");
    rules.recv().expect("the turn's rules")
}

/// Regtest where V24 activates at block 30 and EvoShares at block 40 (10-block windows, 8 of 10,
/// no EHF; EvoShares held LOCKED_IN by its minimum activation height), with BIP147 buried at 30
/// and IPv4-optional at 35, apart from V24.
fn regtest_v24_boundaries() -> Arc<ChainSetup> {
    let max = i64::MAX;
    Arc::new(
        ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
            .arg(ChainOption::VbParams, format!("v24:0:{max}:0:10:8:8:0:0"))
            .arg(ChainOption::VbParams, format!("evo_shares:0:{max}:40:10:8:8:0:0"))
            .arg(ChainOption::TestActivationHeight, "ipv4_optional@35")
            .arg(ChainOption::TestActivationHeight, "bip147@30")
            .build()
            .expect("regtest with -vbparams"),
    )
}

/// The turn binds the next block's special-transaction rules across deployment boundaries, and
/// the tip's own script flags, as ATMP does (on the null connector: the node's refuses V24
/// blocks, AD-CH3-4): `GetSpecialTxRules(tip)` answers for the block at
/// tip + 1 on the tip, `GetBlockScriptFlags(tip)` for the tip. Every block signals V24 and
/// EvoShares, so both lock in at block 20 ([`regtest_v24_boundaries`]). The expected values come
/// from those heights alone: at tip `t`, V24 is on once `t + 1 >= 30`, IPv4-optional once
/// `t + 1 >= 35`, EvoShares once `t + 1 >= 40`, and NULLDUMMY once `t >= 30`.
// Core: validation.cpp:1114, :1227, :2430-2460, :6817-6824 at v24.0.0
#[test]
fn mempool_turn_binds_the_next_blocks_rules_at_deployment_boundaries() {
    let p = regtest_v24_boundaries();
    let pool = Pool::default();
    let rig = Rig::with(RigConfig {
        params: Some(p.clone()),
        mempool: Some(Box::new(pool.clone())),
        ..RigConfig::default()
    });
    let bit = |pos| 1_i32 << p.deployment(pos).expect("a V24 deployment").bit;
    let version = VERSIONBITS_TOP_BITS | bit(DeploymentPos::V24) | bit(DeploymentPos::EvoShares);
    let mut tip = rig.genesis.clone();
    let no_nulldummy = ScriptFlags::from_bits(
        ScriptFlags::MANDATORY_V24_0.bits() & !ScriptFlags::NULLDUMMY.bits(),
    );
    let mut seen = Vec::new();
    for t in [28, 29, 30, 33, 34, 38, 39, 40] {
        while tip.height < t {
            let height = tip.height + 1;
            let cb = coinbase(&cb_script(height, 0));
            let block = mine_version(&p, tip.hash(), tip.time() + SPACING, vec![cb], version);
            tip = Mined {
                block,
                height,
            };
            rig.accept(&tip);
        }
        let flags = if t >= 30 {
            ScriptFlags::MANDATORY_V24_0
        } else {
            no_nulldummy
        };
        let want = (t, t + 1 >= 30, t + 1 >= 40, t + 1 >= 35, Some(flags));
        seen.push((rules_at_tip(&rig), want));
    }
    for (got, want) in seen {
        assert_eq!(got, want, "at tip {}", want.0);
    }
    rig.assert_no_abort();
}

#[test]
fn admission_runs_between_abc_steps_after_the_held_pool_is_released() {
    let s = Stores::new();
    let pool = Pool::default();
    let rig = rig_with(&s, &pool);
    let blocks = branch(&rig.p, &rig.genesis, 3, 0);
    rig.headers(&blocks);
    rig.accept(&blocks[2]);
    rig.accept(&blocks[1]);
    let handle = rig.v.clone();
    let (done, result) = reply();
    let pending = Mutex::new(Some(done));
    let p = pool.clone();
    rig.on_v(move |v| {
        v.release_hook = Some(Arc::new(move |stage| {
            if stage == Stage::Release
                && let Some(done) = pending.lock().expect("pending").take()
            {
                assert!(p.busy(), "post while the first ABC hold still owns the pool");
                let p = p.clone();
                handle
                    .send(ChainCommand::Mempool(Box::new(move |turn| {
                        assert!(!p.busy(), "admission never runs inside the ABC hold");
                        assert_eq!(turn.tip_height(), Some(1), "before the second ABC step");
                        Ok(MempoolCompletion {
                            uncache: vec![],
                            reply: Box::new(move || done.send(())),
                        })
                    })))
                    .expect("admission queued");
            }
        }));
        Ok(())
    });
    rig.accept(&blocks[0]);
    result.recv().expect("admission completed between steps");
    assert_eq!(tip(&rig), rig.id(&blocks[2]));
    rig.assert_no_abort();
}

#[test]
fn admission_faults_at_abc_release_point_carry_past_and_report_once() {
    for periodic in [false, true] {
        let s = Stores::new();
        let pool = Pool::default();
        let fail = Arc::new(AtomicBool::new(false));
        let mut cfg = s.config_with(Seams::default());
        cfg.flush = Some(Box::new(ProtocolFlush {
            inner: cfg.flush.take().expect("flush"),
            pool: pool.clone(),
            fail: fail.clone(),
            coins: s.coins.clone(),
            cached: Arc::new(AtomicBool::new(false)),
        }));
        cfg.mempool = Some(Box::new(pool));
        let mut rig = Rig::with(cfg);
        let a1 = child(&rig.p, &rig.genesis, 0);
        let (done, result) = reply();
        let pending = Arc::new(Mutex::new(Some(done)));
        let handle = rig.v.clone();
        let pending_for_hook = pending.clone();
        let fail_for_hook = fail.clone();
        rig.on_v(move |v| {
            v.release_hook = Some(Arc::new(move |stage| {
                if stage != Stage::Release {
                    return;
                }
                let Some(done) = pending_for_hook.lock().expect("pending").take() else {
                    return;
                };
                let fail = fail_for_hook.clone();
                handle
                    .send(ChainCommand::Mempool(Box::new(move |_| {
                        if periodic {
                            fail.store(true, Ordering::SeqCst);
                            Ok(MempoolCompletion {
                                uncache: vec![],
                                reply: Box::new(move || done.send(())),
                            })
                        } else {
                            drop(done);
                            Err(Fatal::new("admission release fault"))
                        }
                    })))
                    .expect("admission queued");
            }));
            Ok(())
        });

        let processed = rig.v.process_block(a1.block.clone(), true, None, true).recv();
        assert!(
            processed.is_ok(),
            "ABC carries the release-point fault past its step: {processed:?}"
        );
        assert_eq!(result.recv(), Err(crate::chain::reply::WaitError::Disconnected));
        let exit = rig.stop();
        assert!(exit.fatal.is_some(), "periodic={periodic}");
        assert_eq!(s.sink.reports().len(), 1, "one report: periodic={periodic}");
    }
}

#[test]
fn connect_fault_survives_a_failed_nonadding_mempool_drain() {
    let p = regtest();
    let genesis = Mined {
        block: crate::chain::genesis_block(&p),
        height: 0,
    };
    let a1 = child(&p, &genesis, 0);
    let side = branch(&p, &genesis, 2, 1);
    let pool = Pool::default();
    pool.drain_fault.store(true, Ordering::SeqCst);
    let mut rig = Rig::with(RigConfig {
        params: Some(p),
        connector: Some(Box::new(Breaks {
            at: side[1].hash(),
            abort: true,
        })),
        mempool: Some(Box::new(pool)),
        ..RigConfig::default()
    });
    rig.accept(&a1);
    rig.accept(&side[0]);
    assert_eq!(
        rig.v.process_block(side[1].block.clone(), true, None, true).recv(),
        Err(crate::chain::reply::WaitError::Disconnected)
    );
    let exit = rig.stop();
    assert_eq!(exit.fatal.map(|f| f.0), Some("disk on fire".to_owned()));
    assert_eq!(rig.abort.messages(), ["disk on fire", "mempool drain fault"]);
}

#[test]
fn chainlock_enforcement_releases_the_pool_after_publication_and_splice() {
    let s = Stores::new();
    let pool = Pool::default();
    let mut cfg = s.config_with(Seams::default());
    cfg.chainlocks = true;
    cfg.mempool = Some(Box::new(pool.clone()));
    let rig = Rig::with(cfg);
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    let side = branch(&rig.p, &rig.genesis, 1, 1);
    for block in main.iter().chain(&side) {
        rig.accept(block);
    }
    intake(&rig);
    pool.clear();
    let p = pool.clone();
    rig.on_v(move |v| {
        v.release_hook = Some(Arc::new(move |stage| {
            if stage == Stage::Published {
                assert!(p.busy());
            }
            if stage == Stage::Spliced {
                assert!(!p.busy());
            }
        }));
        Ok(())
    });
    record(&rig, &lock(&side[0]));
    settle(&rig);
    assert_eq!(tip(&rig), rig.id(&side[0]));
    assert!(!pool.busy());
    let trace = pool.trace();
    assert!(trace.len() >= 4, "enforcement and activation both acquire the pool: {trace:?}");
    let (pairs, remainder) = trace.as_chunks::<2>();
    assert!(remainder.is_empty());
    assert!(pairs.iter().all(|pair| pair == &["begin", "drop"]));
    rig.assert_no_abort();
}

#[test]
fn real_abc_interleaves_pool_events_across_two_steps_in_one_hold() {
    let p = regtest();
    let genesis = Mined {
        block: crate::chain::genesis_block(&p),
        height: 0,
    };
    let a1 = child(&p, &genesis, 0);
    let side = branch(&p, &genesis, 3, 1);
    let pool = Pool::default();
    let rig = Rig::with(RigConfig {
        connector: Some(Box::new(crate::chain::tests::abc::Rejects(HashSet::from([
            side[1].hash()
        ])))),
        mempool: Some(Box::new(pool.clone())),
        ..RigConfig::default()
    });
    rig.accept(&a1);
    rig.headers(&side);
    rig.accept(&side[0]);
    rig.accept(&side[2]);
    rig.sync();
    let events = Arc::new(Events::default());
    rig.signals.register(events.clone());
    pool.events.store(true, Ordering::SeqCst);
    pool.clear();
    rig.accept(&side[1]);
    rig.sync();
    let (a, b) = (rig.id(&a1), rig.id(&side[0]));
    assert_eq!(
        *events.0.lock().expect("events"),
        [
            format!("disconnect {}", a.0),
            format!("remove Conflict 1 {}", side[0].block.txs()[0].txid()),
            format!("remove Expiry 2 {}", coinbase(&[1, 1]).txid()),
            format!("block {}", b.0),
            format!("disconnect {}", b.0),
            format!("remove Conflict 3 {}", a1.block.txs()[0].txid()),
            format!("remove Expiry 4 {}", coinbase(&[1, 1]).txid()),
            format!("block {}", a.0),
        ]
    );
    assert_eq!(pool.trace(), ["begin", "drop"], "both ABC steps share one guard");
    assert_eq!(tip(&rig), a);

    // A subsequent forward step is driven by the real ABC as well, including its final tip
    // signal; the hand-built stream above cannot stand in for this ordering check.
    pool.events.store(false, Ordering::SeqCst);
    let next = child(&rig.p, &a1, 2);
    rig.accept(&next);
    rig.sync();
    let next_id = rig.id(&next);
    let events = events.0.lock().expect("events").clone();
    assert_eq!(
        &events[events.len() - 2..],
        [format!("block {}", next_id.0), format!("tip {}", next_id.0)]
    );
}
