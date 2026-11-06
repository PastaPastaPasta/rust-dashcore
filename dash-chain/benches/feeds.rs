//! PF0 benches for SB5's feeds (SB5 §11). Bench IDs: `cargo bench -p dash-chain --bench feeds
//! -- --list`.
//!
//! The steps run on `support::fixture`'s mainnet-shaped node index, shared with
//! `benches/chain.rs`, so `tip/connect-publish-1k` there (the same steps with no subscriber) is
//! the baseline to subtract from `publish/*` when checking §11's budget; PF0 itself gates each
//! line against its own base. Readers park on `Notify::Thread`; SB14 measures tokio's `Waker`.
//! The `cold/*` benches read through the cold cache of an SPV chain, here rather than in SB4's
//! `benches/chain.rs`. `snapshot/*` and `open/*` run an anchored regtest SPV chain over
//! `MemHeaderStore`, stepped as the host steps it (SB4 §9.1). `spv/*` runs SPV's whole header
//! step ([`SpvRig`]) over a sync past the window: PF0's memory rows for §6.3.
//!
//! PF0 measures production code, so no bench build turns on `test-util` (the canary in
//! `support/mod.rs`): this file builds the pieces it needs on the public API, and compiles its
//! own copy of `MemHeaderStore`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, DefaultHasher};
use std::hint::black_box;
use std::sync::{Arc, Barrier};
use std::thread::{self, JoinHandle};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
// Some of these are here for `mem_store` below only, which names them from this crate's root.
use dash_chain::{
    ActiveDiff, AnchorPack, AnchorState, BlockId, BlockRef, CHAIN_SCHEMA, ChainConfig, ChainFeeds,
    ChainLockRef, ChainMeta, ClPersisted, Durability, Evicting, FailStop, FeedConfig, FeedId,
    FeedSource, Handoff, HeaderBatch, HeaderChain, HeaderRecord, HeaderRow, HeaderStepSink,
    HeaderStore, MetaDelta, MutableGroup, Needs, Next, Notify, Parker, Publish, RecordKind,
    Services, StaticSporks, Status, StatusEvent, StatusReader, StepRow, StepTail, StoreError,
    SubscriberFeed, TipStep, Work,
};
use dash_clock::MockClock;
use dash_consensus::header::HeaderParams;
use dash_consensus::pow::{PowParams, block_proof, check_proof_of_work};
use dash_consensus::primitives::Header;
use dash_num::CompactTarget;
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::{BlockHash, BlockHeader};

mod support;
use support::fixture::{
    Lcg, MAINNET_HEADERS, Setup, mainnet_chain, mainnet_chain_with, synthetic_hash,
};

// The library's own file, so the store the benches step is the one its tests pass. Its imports,
// and its tests module's (cargo's `--cfg test` compiles it here, with no tests in it), are the
// names imported above, at this crate's root.
#[allow(dead_code, unused_imports, reason = "the benches use part of the store")]
#[path = "../src/mem_store.rs"]
mod mem_store;
use mem_store::{MemHeaderStore, ReadKind};

/// The steps each `publish/*` iteration takes.
const STEPS: u32 = 1_000;
/// The live subscribers of `publish/*`.
const SUBSCRIBERS: usize = 8;

/// The mainnet-sized node index with feed queues of `capacity`, and the top `STEPS` ids above
/// `base`: each iteration's setup takes the chain back to `base`, and its region steps up again.
struct Fixture {
    chain: RefCell<HeaderChain>,
    feeds: ChainFeeds,
    base: BlockId,
    ids: Vec<BlockId>,
    /// The writer's wake on space (`set_producer_wake`).
    producer: Arc<Parker>,
    /// The last iteration's feeds, deregistered by the next setup.
    live: RefCell<Vec<FeedId>>,
}

impl Fixture {
    fn new(capacity: u32) -> Fixture {
        let mut cfg = Setup::mainnet().node_config();
        cfg.feeds.capacity = capacity;
        let (chain, tip) = mainnet_chain_with(cfg);
        let ancestry = chain.complete().expect("a node chain is complete");
        let at = |h| ancestry.ancestor(tip, h).expect("on the chain");
        let base = at(MAINNET_HEADERS - STEPS - 1);
        let ids = (MAINNET_HEADERS - STEPS..MAINNET_HEADERS).map(at).collect();
        let producer = Parker::new();
        chain.set_producer_wake(Notify::Thread(producer.clone()));
        Fixture {
            feeds: chain.feeds(),
            chain: RefCell::new(chain),
            base,
            ids,
            producer,
            live: RefCell::default(),
        }
    }

    fn tip(&self) -> BlockId {
        FeedSource::<()>::snapshot(&self.feeds).tip().id
    }

    /// Outside the region: deregisters the last iteration's feeds, then takes the chain back to
    /// `base`, so no setup step reaches a subscriber.
    fn rewind(&self) {
        for id in self.live.borrow_mut().drain(..) {
            self.feeds.deregister(id);
        }
        while self.tip() != self.base {
            self.chain.borrow_mut().disconnect_tip().expect("above genesis");
        }
    }

    /// A live subscriber at the tip, and the `seq` of the first step it will receive.
    fn subscribe(&self) -> (Box<dyn SubscriberFeed>, u64) {
        let id = self.feeds.register(Needs::default()).expect("registers");
        let tip = FeedSource::<()>::snapshot(&self.feeds).tip();
        let Ok(Handoff::Live {
            feed,
            seq_synced,
        }) = self.feeds.subscribe_at(id, &tip, &mut || Ok::<(), ()>(()))
        else {
            panic!("the handoff at the tip");
        };
        self.live.borrow_mut().push(id);
        (feed, seq_synced)
    }

    /// The region's writer: every step, then V's drain of the pending backlog before the next
    /// (§4.7).
    fn connect_all(&self) {
        let mut chain = self.chain.borrow_mut();
        for &id in &self.ids {
            black_box(chain.connect_tip(id).expect("prev is the tip"));
            while !chain.pending_is_empty() && chain.retry_pending() == Publish::Blocked {
                self.producer.park();
            }
        }
    }
}

/// How a reader thread takes its events.
#[derive(Clone, Copy)]
enum Drain {
    /// Parks on its permit when empty, interleaved with the writer as the scheduler decides.
    Park,
    /// Loops intake when empty.
    Spin,
    /// Waits at the barrier a second time, until the writer's last step, then takes everything:
    /// the same work as `Park`, in counts that do not depend on the scheduler.
    After,
}

/// A reader thread. Its waker is set here, and it waits at `sync` only once its first `next`
/// found nothing, so thread start-up stays outside the region. Then it takes events until it has
/// popped `last`. Returns the feed so it drops outside the region. A panic before `sync` shows as
/// a hang in setup.
fn spawn_reader(
    mut feed: Box<dyn SubscriberFeed>,
    last: u64,
    drain: Drain,
    sync: Arc<Barrier>,
) -> JoinHandle<Box<dyn SubscriberFeed>> {
    let p = Parker::new();
    feed.set_waker(Notify::Thread(p.clone()));
    thread::spawn(move || {
        assert!(matches!(feed.next(), Next::Empty), "the setup took no step");
        sync.wait();
        if let Drain::After = drain {
            sync.wait();
        }
        let mut taken = 0;
        loop {
            match feed.next() {
                Next::Event(e) => {
                    let seq = e.seq();
                    feed.pop();
                    taken += 1;
                    if seq == last {
                        assert_eq!(taken, STEPS, "one event per step");
                        return feed;
                    }
                }
                Next::Control(_) => {}
                Next::Empty => match drain {
                    Drain::Spin => std::hint::spin_loop(),
                    _ => p.park(),
                },
            }
        }
    })
}

fn publish(c: &mut Criterion) {
    let mut g = c.benchmark_group("publish");
    // Per iteration, `STEPS` connects fanned out to `SUBSCRIBERS` reader threads, plus joining
    // them once each has drained every event (§11: the readers' work is inside the region).
    // - connect-8sub-1k, the BG gate: Q = 1,024 holds every step, so no target is ever full. The
    //   readers drain after the writer's last step: pinned to one CPU, interleaved draining moves
    //   the instruction count by ~20% with the scheduler. One bulk intake per reader is the
    //   cheapest take, so the line is a lower bound on interleaved cost. What remains (~0.1%) is
    //   where each reader's first barrier return lands and the barrier's wake-up.
    // - -contended (R1-M2): readers loop intake, so the writer meets held slot locks. Pinned to
    //   one CPU it measures spinning, so it has no PF0 line until PF1.
    // - cursor-retry-8sub-1k: Q = 32, so targets fill and take cursors, and the writer drains the
    //   backlog through `retry_pending` and its wake before each next step. Interleaved by
    //   construction, so its scheduler-dependent counters are ungated.
    // `PerIteration` is load-bearing: each setup rewinds the chain its region stepped up.
    for (id, capacity, drain) in [
        ("connect-8sub-1k", 1024, Drain::After),
        ("connect-8sub-1k-contended", 1024, Drain::Spin),
        ("cursor-retry-8sub-1k", 32, Drain::Park),
    ] {
        let mut fx = None;
        g.bench_function(id, |b| {
            let fx: &Fixture = fx.get_or_insert_with(|| Fixture::new(capacity));
            // After: the writer would wait for space the parked-at-the-barrier readers never make.
            assert!(!matches!(drain, Drain::After) || STEPS < capacity, "Q holds every step");
            b.iter_batched(
                || {
                    fx.rewind();
                    let sync = Arc::new(Barrier::new(SUBSCRIBERS + 1));
                    let readers: Vec<_> = (0..SUBSCRIBERS)
                        .map(|_| {
                            let (feed, first) = fx.subscribe();
                            let last = first + u64::from(STEPS) - 1;
                            spawn_reader(feed, last, drain, sync.clone())
                        })
                        .collect();
                    sync.wait();
                    (readers, sync)
                },
                |(readers, sync)| {
                    fx.connect_all();
                    if let Drain::After = drain {
                        sync.wait();
                    }
                    readers.into_iter().map(|r| r.join().expect("a reader")).collect::<Vec<_>>()
                },
                BatchSize::PerIteration,
            );
        });
    }
    g.finish();
}

fn reader(c: &mut Criterion) {
    let mut g = c.benchmark_group("reader");
    // CH2's driver over `STEPS` queued events (§4.4.1 rule 3): `next`, the round-trip
    // compression attempt on each `Connected`, then `pop` when it finds none. The events are
    // `STEPS - 2R` connects, an R-block reorg back and the same R again (R = `STEPS / 20`): the
    // first `STEPS - 3R` `Connected`s scan the staging queue to its end and pop, R compress with
    // their `Disconnected`s, and the R re-connects scan the shorter rest and pop. Q holds them
    // all. `PerIteration` is load-bearing: each setup rewinds the chain.
    const R: usize = STEPS as usize / 20;
    let top = STEPS as usize - 2 * R;
    let mut fx = None;
    g.bench_function("next-pop-compress-1k", |b| {
        let fx: &Fixture = fx.get_or_insert_with(|| Fixture::new(1024));
        b.iter_batched(
            || {
                fx.rewind();
                let (feed, _) = fx.subscribe();
                let mut chain = fx.chain.borrow_mut();
                for &id in &fx.ids[..top] {
                    chain.connect_tip(id).expect("prev is the tip");
                }
                for _ in 0..R {
                    chain.disconnect_tip().expect("above genesis");
                }
                for &id in &fx.ids[top - R..top] {
                    chain.connect_tip(id).expect("prev is the tip");
                }
                feed
            },
            |mut feed| {
                let mut taken = 0;
                while let Next::Event(e) = feed.next() {
                    let connected = match e {
                        TipStep::Connected {
                            block,
                            ..
                        } => Some(block.hash),
                        _ => None,
                    };
                    if let Some(h) = connected
                        && feed.remove_round_trip(&h).is_some()
                    {
                        taken += 2;
                    } else {
                        feed.pop();
                        taken += 1;
                    }
                }
                assert_eq!(taken, STEPS, "every event taken");
                feed
            },
            BatchSize::PerIteration,
        );
    });
    g.finish();
}

fn handoff(c: &mut Criterion) {
    let mut g = c.benchmark_group("handoff");
    // 64 `register` + `subscribe_at` handoffs at the tip, each deregistered before the next: the
    // table's free list, the reserved buffers and the registry republishes.
    let mut fx = None;
    g.bench_function("subscribe-at-64", |b| {
        let fx: &Fixture = fx.get_or_insert_with(|| Fixture::new(1024));
        let tip = FeedSource::<()>::snapshot(&fx.feeds).tip();
        b.iter(|| {
            for _ in 0..64 {
                let id = fx.feeds.register(Needs::default()).expect("registers");
                let r = fx.feeds.subscribe_at(id, &tip, &mut || Ok::<(), ()>(()));
                assert!(matches!(r, Ok(Handoff::Live { .. })));
                drop(black_box(r));
                fx.feeds.deregister(id);
            }
        });
    });
    g.finish();
}

fn status(c: &mut Criterion) {
    let mut g = c.benchmark_group("status");
    // Per iteration, `STEPS` status publications (`publish_status`), each after V's
    // `best_header_walk_back` moves the best header down one block, so every publication stores
    // new facts (§3.6). The readers, on the bench thread, armed their wakers in setup, so each
    // publication notifies every one; after the last, each takes its one coalesced report. No
    // thread runs beside the writer, so the counts repeat exactly.
    // - publish-1k-2readers: two readers.
    // - publish-1k-0readers, unlisted: the same publications with no reader, the baseline to
    //   subtract for the readers' share (wakes and reports).
    // `PerIteration` is load-bearing: each setup restores the best header to the active tip.
    let mut fx = None;
    for (id, readers) in [("publish-1k-2readers", 2), ("publish-1k-0readers", 0)] {
        g.bench_function(id, |b| {
            let chain: &RefCell<HeaderChain> =
                fx.get_or_insert_with(|| RefCell::new(mainnet_chain().0));
            b.iter_batched(
                || {
                    let mut chain = chain.borrow_mut();
                    // V's invalid-chain hook on the best header itself: back to the active tip.
                    let best = chain.best_header();
                    chain.node_index().expect("a node").best_header_invalid_chain(best);
                    chain.publish_status();
                    let last = chain.snapshot().tip.best_header_height - STEPS;
                    let status: Vec<StatusReader> = (0..readers)
                        .map(|_| {
                            let mut r = chain.status_feed();
                            assert!(!r.arm(Notify::Thread(Parker::new())), "nothing unreported");
                            r
                        })
                        .collect();
                    (status, last)
                },
                |(mut status, last)| {
                    let mut chain = chain.borrow_mut();
                    for _ in 0..STEPS {
                        let best = chain.best_header();
                        assert!(chain.node_index().expect("a node").best_header_walk_back(best));
                        chain.publish_status();
                    }
                    for r in &mut status {
                        let events = r.next_events();
                        assert!(
                            matches!(
                                events.as_slice(),
                                [StatusEvent::HeaderAccepted { best_header, .. }]
                                    if best_header.height == last
                            ),
                            "one coalesced report of the last best header"
                        );
                    }
                    status
                },
                BatchSize::PerIteration,
            );
        });
    }
    g.finish();
}

/// The cold cache's `cap` at the desktop SPV default (§6.2: C = 1,024), and the records below
/// the window that `cold/*` reads.
const COLD_CAP: u32 = 1_024;
const COLD_ROWS: u32 = 1 << 16;
/// `cold/*`'s window: every id the probes read is a record.
const COLD_WINDOW: u32 = 10;

/// The parameters, config, services and store of an SPV chain with a cold cache of [`COLD_CAP`]
/// over `n` trunk headers.
fn cold_store(n: u32) -> (Arc<ChainSetup>, ChainConfig, Services, MemHeaderStore) {
    let (p, svc) = regtest(None);
    let cfg = ChainConfig {
        cold_cache: COLD_CAP,
        ..ChainConfig::spv(&p, COLD_WINDOW)
    };
    let store = spv_store(&p, &svc, &cfg, n);
    (p, cfg, svc, store)
}

fn cold(c: &mut Criterion) {
    let mut g = c.benchmark_group("cold");
    // Each setup of `lru-miss-all-referenced` opens a chain and makes 2,048 reads.
    g.sample_size(10);
    // 10,000 public `try_entry` reads of records below the window, each through the cold cache:
    // 90% from a hot set of 90% of `cap`, 10% over the other rows. CLOCK evicts some hot records
    // too, so about 85% of the reads by id hit. A miss reads the record by id, then its parent by
    // hash through the same cache. The fixture runs the probes to a warmed, deterministic state
    // first.
    let mut fx = None;
    g.bench_function("lru-hit-miss-90-10", |b| {
        let (_chain, reader, probes) = fx.get_or_insert_with(|| {
            let (p, cfg, svc, store) = cold_store(COLD_ROWS + COLD_WINDOW);
            let chain = Spv::open(p, cfg, svc, store.clone()).expect("open");
            let reader = chain.reader();
            let hot = COLD_CAP * 9 / 10;
            let mut rng = Lcg(0x5b5_c01d);
            let probes: Vec<BlockId> = (0..10_000)
                .map(|_| {
                    BlockId(match rng.below(10) {
                        0 => COLD_CAP + rng.below(COLD_ROWS - COLD_CAP),
                        _ => rng.below(hot),
                    })
                })
                .collect();
            let by_id = || store.faults(|f| f.reads(ReadKind::ById));
            let pass = || probes.iter().for_each(|&k| _ = reader.try_entry(k).expect("read"));
            pass();
            let reads = by_id();
            pass();
            let misses = by_id() - reads;
            assert!((1_000..2_000).contains(&misses), "a steady-state hit rate near 85%: {misses}");
            (chain, reader, probes)
        });
        b.iter(|| {
            for &k in probes.iter() {
                black_box(reader.try_entry(k).expect("read"));
            }
        });
    });
    // R1-m3's worst case: a full cache with every bit set; one miss clears all `cap` bits before
    // it evicts, then looks its parent up by hash. Each setup opens the chain afresh and reads
    // ids 0..cap twice, the second pass all hits (asserted once): the cache holds exactly those.
    let mut fx = None;
    g.bench_function("lru-miss-all-referenced", |b| {
        let (p, cfg, svc, store) = fx.get_or_insert_with(|| cold_store(COLD_CAP + COLD_WINDOW));
        let by_id = || store.faults(|f| f.reads(ReadKind::ById));
        let mut checked = false;
        b.iter_batched(
            || {
                let chain = Spv::open(p.clone(), cfg.clone(), svc.clone(), store.clone());
                let chain = chain.expect("open");
                let reader = chain.reader();
                let pass =
                    || (0..COLD_CAP).for_each(|k| _ = reader.try_entry(BlockId(k)).expect("read"));
                pass();
                let reads = by_id();
                pass();
                assert!(checked || by_id() == reads, "the second pass hits");
                checked = true;
                (chain, reader)
            },
            |(chain, reader)| {
                let e = reader.try_entry(BlockId(COLD_CAP)).expect("read");
                (chain, reader, e)
            },
            BatchSize::PerIteration,
        );
    });
    g.finish();
}

/// Regtest's `nBits`, its proof-of-work limit: about two tries per mined header.
const REGTEST_BITS: u32 = 0x207f_ffff;
/// The SPV chain's anchor height: post-DGW, as SB4's tests anchor.
const SPV_ANCHOR: u32 = 40_000;
/// The anchor pack's headers.
const PACK_LEN: u32 = 25;
/// The pack's first header time; headers are 150 s apart, below the services' clock.
const SPV_T0: u32 = 1_690_000_000;
/// The trunk heights between two-header stale branches.
const STALE_EVERY: u32 = 1_000;
/// The window SPV desktop opens with at the PF0 line (§8's cost estimate).
const OPEN_WINDOW: u32 = 10_000;

type Spv = HeaderChain<MemHeaderStore, Evicting>;

struct NoFail;

impl FailStop for NoFail {
    fn read_failed(&self, what: &str) {
        panic!("bench store read failed: {what}");
    }

    fn is_tripped(&self) -> bool {
        false
    }
}

/// Regtest's parameters and services, SPORK_19 as `chainlocks` sets it.
fn regtest(chainlocks: Option<bool>) -> (Arc<ChainSetup>, Services) {
    let p = Arc::new(
        ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
            .build()
            .expect("Core's parameters"),
    );
    let svc = Services {
        clock: Arc::new(MockClock::at(1_700_000_000)),
        sporks: Arc::new(StaticSporks::for_network(&p, chainlocks)),
        fail: Arc::new(NoFail),
    };
    (p, svc)
}

/// A regtest header on `prev`, mined.
fn mine(pow: &PowParams, prev: BlockHash, time: u32) -> Header {
    let bits = CompactTarget::new(REGTEST_BITS);
    (0..)
        .map(|nonce| {
            Header::new(BlockHeader {
                version: 0x2000_0000,
                prev_hash: prev,
                time,
                bits: REGTEST_BITS,
                nonce,
                ..BlockHeader::default()
            })
        })
        .find(|h| check_proof_of_work(h.hash().as_bytes(), bits, pow))
        .expect("a nonce")
}

/// `n` mined regtest headers on `prev`, the first at `time`.
fn mine_n(p: &ChainSetup, mut prev: BlockHash, time: u32, n: u32) -> Vec<Header> {
    let pow = *HeaderParams::from_chain_params(p).expect("regtest").pow();
    (0..n)
        .map(|i| {
            let h = mine(&pow, prev, time + 150 * i);
            prev = h.hash();
            h
        })
        .collect()
}

/// The pack's own host step (SB4 §9.1), with no headers: it writes the pack's records and the
/// meta's real tip.
fn pack_step(chain: &mut Spv, store: &mut MemHeaderStore) {
    let to = chain.best_header();
    assert_eq!(chain.private_tip().expect("a tip").0.id, to, "the anchor is the tip");
    chain.tip_plan(to).expect("a plan");
    store.begin_step();
    let tail = chain.write_header_step(None, store).expect("stage 3");
    store.commit_step(&tail, Durability::Buffered).expect("commit");
    chain.set_active_tip(to).expect("stage 4");
    chain.records_committed();
}

/// An SPV chain under `cfg` anchored at [`SPV_ANCHOR`], its store, and the anchor's hash: the
/// pack's own step committed.
fn anchored(
    p: &Arc<ChainSetup>,
    svc: &Services,
    cfg: &ChainConfig,
) -> (Spv, MemHeaderStore, BlockHash) {
    let pack_headers = mine_n(p, BlockHash::from([7; 32]), SPV_T0, PACK_LEN);
    let pack = AnchorPack {
        height: SPV_ANCHOR,
        hash: pack_headers[PACK_LEN as usize - 1].hash(),
        chain_work: block_proof(CompactTarget::new(REGTEST_BITS))
            .wrapping_mul(Work::from_u64(1 << 20)),
        headers: pack_headers.iter().map(|h| *h.raw()).collect::<Vec<_>>().try_into().expect("25"),
    };
    let mut store = MemHeaderStore::new();
    let mut chain = Spv::new_anchored(p.clone(), cfg.clone(), svc.clone(), store.clone(), &pack)
        .expect("a valid pack");
    store
        .reset(ChainMeta {
            schema: CHAIN_SCHEMA,
            anchor: (pack.hash, pack.height),
            anchor_state: AnchorState::Candidate,
            next_id: BlockId(0),
            tip: BlockId(0),
            best_header: BlockId(0),
            sync_seq: 0,
            cl: ClPersisted {
                best: None,
                floor: None,
                enabled_observed: false,
            },
        })
        .expect("reset");
    pack_step(&mut chain, &mut store);
    (chain, store, pack.hash)
}

/// The store of an SPV chain under `cfg` anchored at [`SPV_ANCHOR`], with `n` trunk headers
/// and, every [`STALE_EVERY`] heights, a two-header stale branch. The pack's step is the real
/// one; the rest is appended as the steps would write it, under synthetic hashes below the
/// target, so the setup computes no X11 beyond the pack's.
fn spv_store(p: &Arc<ChainSetup>, svc: &Services, cfg: &ChainConfig, n: u32) -> MemHeaderStore {
    let (_, mut store, anchor) = anchored(p, svc, cfg);
    let trunk_t0 = SPV_T0 + 150 * PACK_LEN;
    let mut k = 0;
    let mut branch = |prev: BlockHash, time: u32, len: u32| {
        let mut prev = prev;
        (0..len)
            .map(|i| {
                k += 1;
                let header = BlockHeader {
                    version: 0x2000_0000,
                    prev_hash: prev,
                    time: time + 150 * i,
                    bits: REGTEST_BITS,
                    ..BlockHeader::default()
                };
                prev = synthetic_hash(k);
                (header, prev)
            })
            .collect::<Vec<_>>()
    };
    let mut headers = branch(anchor, trunk_t0, n);
    for i in (STALE_EVERY / 2..n).step_by(STALE_EVERY as usize) {
        let on = headers[i as usize - 1].1;
        let stale = branch(on, trunk_t0 + 150 * i + 7, 2);
        headers.extend(stale);
    }
    append_spv_records(&mut store, &headers);
    assert_eq!(store.chain_meta().expect("meta").expect("a chain").tip, BlockId(PACK_LEN + n - 1));
    store
}

/// Core's `GetSkipHeight`.
fn skip_height(height: u32) -> u32 {
    let invert_lowest_one = |n: u32| n & n.wrapping_sub(1);
    match height {
        0 | 1 => 0,
        h if h & 1 == 1 => invert_lowest_one(invert_lowest_one(h - 1)) + 1,
        h => invert_lowest_one(h),
    }
}

/// Record ids by hash, under a fixed hasher so that a bench's setup repeats across processes.
type Ids = HashMap<BlockHash, BlockId, BuildHasherDefault<DefaultHasher>>;

/// Core's `CBlockIndex::GetAncestor` over `recs`, indexed by id: `id`'s ancestor at `height`,
/// at or below `id`'s height and at or above the floor.
fn ancestor(recs: &[HeaderRecord], ids: &Ids, mut id: BlockId, height: u32) -> BlockId {
    loop {
        let r = &recs[id.0 as usize];
        if r.height == height {
            return id;
        }
        let (skip, skip_prev) = (skip_height(r.height), skip_height(r.height - 1));
        id = match r.skip {
            Some(s)
                if skip == height
                    || (skip > height && !(skip_prev + 2 < skip && skip_prev >= height)) =>
            {
                s
            }
            _ => *ids.get(&r.header.prev_hash).expect("an ancestor at or above the floor"),
        };
    }
}

/// Appends `headers` to an SPV `store` as the header steps would write them, in one step: each
/// an accepted record at `VALID_TREE` with no mark, and the tip moved to the most work (the lower
/// id on a tie), which descends from the store's tip. Nothing is hashed: each pair's hash stands
/// for its header's X11, so the setup builds a long chain from synthetic hashes below the target
/// instead of mining it. Each parent is in `store` or earlier in `headers`. The store has had a
/// committed step, and its tip is its best header: the tip moves to the most work, where a real
/// step would move it to the fork target. `open` checks the skip pointers of the window's
/// records.
fn append_spv_records(store: &mut MemHeaderStore, headers: &[(BlockHeader, BlockHash)]) {
    let meta = store.chain_meta().expect("read").expect("a chain");
    let mut recs = store.from_height(0).expect("read");
    recs.sort_by_key(|r| r.id);
    assert!(recs.iter().enumerate().all(|(i, r)| r.id.0 as usize == i), "dense ids");
    let floor = recs[0].height;
    let mut ids: Ids = recs.iter().map(|r| (r.hash, r.id)).collect();
    let status = MutableGroup::default().status.raise_validity(Status::VALID_TREE).0;
    let first = recs.len();
    let mut rows: Vec<StepRow> = Vec::new();
    for &(header, hash) in headers {
        let id = BlockId(recs.len() as u32);
        let prev = ids[&header.prev_hash];
        let p = &recs[prev.0 as usize];
        let height = p.height + 1;
        let skip = (skip_height(height) >= floor)
            .then(|| ancestor(&recs, &ids, prev, skip_height(height)));
        let rec = HeaderRecord {
            id,
            hash,
            height,
            header,
            skip,
            status,
            chain_work: p.chain_work + block_proof(CompactTarget::new(header.bits)),
            time_max: p.time_max.max(header.time),
            kind: RecordKind::Accepted,
        };
        rows.extend(rec.clone().step_rows(Some(prev)));
        ids.insert(hash, id);
        recs.push(rec);
    }
    let best = recs[first..].iter().fold(&recs[meta.best_header.0 as usize], |b, r| {
        match r.chain_work > b.chain_work {
            true => r,
            false => b,
        }
    });
    let old = &recs[meta.tip.0 as usize];
    let active = (best.id != old.id).then(|| {
        assert_eq!(ancestor(&recs, &ids, best.id, old.height), old.id, "the tip extends");
        ActiveDiff {
            from_height: old.height + 1,
            new_tip: best.id,
            new_height: best.height,
            old_height: old.height,
        }
    });
    let tail = StepTail {
        active,
        cl: None,
        meta: MetaDelta {
            next_id: BlockId(recs.len() as u32),
            tip: best.id,
            best_header: best.id,
        },
    };
    store.begin_step();
    store.rows(&rows).expect("rows");
    store.commit_step(&tail, Durability::Buffered).expect("commit");
}

/// An SPV chain anchored at [`SPV_ANCHOR`] over a `MemHeaderStore`, its pack committed, five
/// live subscribers at its tip, and SB4 §9.1's header step with SPV's segmented stage 4, as the
/// host runs it.
struct SpvRig {
    chain: Spv,
    store: MemHeaderStore,
    params: Arc<ChainSetup>,
    anchor: BlockHash,
    subs: Vec<Box<dyn SubscriberFeed>>,
}

impl SpvRig {
    /// Window `w`, `feeds`, a cold cache of `cold_cache` records, and SPORK_19 off.
    fn new(w: u32, feeds: FeedConfig, cold_cache: u32) -> SpvRig {
        let (params, svc) = regtest(Some(false));
        let cfg = ChainConfig {
            feeds,
            cold_cache,
            ..ChainConfig::spv(&params, w)
        };
        let (chain, store, anchor) = anchored(&params, &svc, &cfg);
        let hub = chain.feeds();
        let subs = (0..5).map(|_| subscribe_live(&hub)).collect();
        SpvRig {
            chain,
            store,
            params,
            anchor,
            subs,
        }
    }

    /// `n` headers mined on the anchor, from the slot that follows the pack.
    fn mine_on_anchor(&self, n: u32) -> Vec<Header> {
        mine_n(&self.params, self.anchor, SPV_T0 + 150 * PACK_LEN, n)
    }

    /// `headers` in steps of `step_headers`, each settled.
    fn sync(&mut self, headers: &[Header], step_headers: usize) {
        for chunk in headers.chunks(step_headers) {
            self.step(chunk);
            self.settle();
        }
    }

    /// One header step on `headers`: `accept`, `fork_target`, stage 3 committed Buffered,
    /// `activate_segmented` (the subscribers draining at each wait), `records_committed`.
    fn step(&mut self, headers: &[Header]) {
        let c = &mut self.chain;
        let batch = HeaderBatch {
            hashes: headers.iter().map(|h| h.hash()).collect(),
            headers: headers.to_vec(),
        };
        c.accept(batch, None).expect("accept");
        // Stage 1 builds no plan: stage 3 reads the move's shape (`tip_target`), and each
        // sub-step of stage 4 plans its own.
        let target = c.fork_target(&c.virtual_tip()).expect("a target");
        let e = c.reader().try_entry(target.id).expect("read").expect("the target is held");
        let to = BlockRef {
            id: e.id,
            hash: e.hash,
            height: e.height,
        };
        let moved = c.private_tip().expect("a tip").0.id != target.id;
        self.store.begin_step();
        let tail = c.write_header_step(moved.then_some(to), &mut self.store).expect("stage 3");
        self.store.commit_step(&tail, Durability::Buffered).expect("commit");
        let subs = &mut self.subs;
        c.activate_segmented(to, &mut || drain_all(subs)).expect("stage 4");
        c.records_committed();
    }

    /// Drains every reader, then retries the writer's pending fanout, until nothing is pending:
    /// what a writer runs after each step before the next.
    fn settle(&mut self) {
        loop {
            drain_all(&mut self.subs);
            if self.chain.pending_is_empty() {
                return;
            }
            self.chain.retry_pending();
        }
    }
}

/// A live subscriber registered at the published tip.
fn subscribe_live(feeds: &ChainFeeds) -> Box<dyn SubscriberFeed> {
    let id = feeds.register(Needs::default()).expect("registers");
    let tip = FeedSource::<()>::snapshot(feeds).tip();
    match feeds.subscribe_at(id, &tip, &mut || Ok::<(), ()>(())) {
        Ok(Handoff::Live {
            feed,
            ..
        }) => feed,
        other => panic!("subscribe_at: {:?}", other.err()),
    }
}

/// Takes every event and control each of `subs` holds now.
fn drain_all(subs: &mut [Box<dyn SubscriberFeed>]) {
    for f in subs {
        loop {
            match f.next() {
                Next::Event(_) => _ = f.pop().expect("selected"),
                Next::Control(_) => {}
                Next::Empty => break,
            }
        }
    }
}

fn snapshot(c: &mut Criterion) {
    let mut g = c.benchmark_group("snapshot");
    // 1,000 heights below a window of 10, ascending, through a cache of 256: the cold path of
    // §5.3's `at_height`. Each height is `ancestor` from the window's base down the skip links,
    // each link through the cache (a record's `prev` by hash), then the entry by id. The walks
    // share their upper links and the scan cycles past CLOCK's capacity, so each pass reads the
    // store about once per height (about 290 `by_id` and 720 `by_hash`), within 10 reads of the
    // same count from the second pass on (asserted). The fixture keeps the chain alive beside its
    // snapshot.
    let heights = SPV_ANCHOR + 1..=SPV_ANCHOR + 1_000;
    let mut fx = None;
    g.bench_function("at-height-cold-1k", |b| {
        let (_chain, snap) = fx.get_or_insert_with(|| {
            let (p, svc) = regtest(None);
            let cfg = ChainConfig {
                cold_cache: 256,
                ..ChainConfig::spv(&p, 10)
            };
            let store = spv_store(&p, &svc, &cfg, 1_100);
            let chain = Spv::open(p, cfg, svc, store.clone()).expect("open");
            let snap = chain.feeds().feed_snapshot();
            let reads = || store.faults(|f| f.reads(ReadKind::ById) + f.reads(ReadKind::ByHash));
            let pass = || {
                let before = reads();
                heights.clone().for_each(|h| _ = snap.at_height_checked(h).expect("read"));
                reads() - before
            };
            pass();
            let steady: Vec<u64> = (0..4).map(|_| pass()).collect();
            let (lo, hi) = (steady.iter().min().expect("4"), steady.iter().max().expect("4"));
            assert!(*lo >= 500 && hi - lo <= 10, "a steady scan through the store: {steady:?}");
            (chain, snap)
        });
        b.iter(|| {
            for h in heights.clone() {
                black_box(snap.at_height_checked(h).expect("read"));
            }
        });
    });
    g.finish();
}

fn open(c: &mut Criterion) {
    let mut g = c.benchmark_group("open");
    // §8 at w = 10,000 over `MemHeaderStore`: 12,000 trunk headers and 12 two-header stale
    // branches, so one range read of 10,021 records (10 branches in the window), one
    // `active_from` of 12,001 rows and 13 leaves, 2 of them cold. The reopened chain's drop is
    // outside the measurement.
    let mut fx = None;
    g.bench_function("mem-w10000", |b| {
        let (p, cfg, svc, store) = fx.get_or_insert_with(|| {
            let (p, svc) = regtest(None);
            let cfg = ChainConfig::spv(&p, OPEN_WINDOW);
            let store = spv_store(&p, &svc, &cfg, 12_000);
            let window = store.from_height(SPV_ANCHOR + 12_000 - OPEN_WINDOW).expect("read");
            assert_eq!((window.len(), store.leaves().expect("read").len()), (10_021, 13));
            (p, cfg, svc, store)
        });
        b.iter_with_large_drop(|| {
            Spv::open(p.clone(), cfg.clone(), svc.clone(), store.clone()).expect("open")
        });
    });
    g.finish();
}

fn spv(c: &mut Criterion) {
    let mut g = c.benchmark_group("spv");
    g.sample_size(10);
    // §6.3's typical rows (DEC-187): an anchored regtest chain synced past its window in steps of
    // B = 2,000, each the whole SPV header step (accept, `fork_target`, stage 3 committed
    // Buffered, `activate_segmented`, `records_committed`), with five live subscribers draining
    // at each wait and after each step. The headers are mined once per process (X11, about 2 per
    // header, outside the region), and one sync checks the fixture: the tip at the last header,
    // exactly the window resident. Each iteration's setup builds a fresh chain and subscribers,
    // dropped after the region. Peak RSS and page faults carry the residency that A-F12's
    // `mem_spv_typical_*` bound; allocations carry the step's allocation volume, store included.
    let rows = [
        ("typical-w10000", 10_000, FeedConfig::spv(), 1_024, 14_000),
        ("typical-w2048", 2_048, FeedConfig::spv_mobile(), 256, 6_000),
    ];
    for (id, w, feeds, cold, n) in rows {
        let b_headers = feeds.step_headers as usize;
        let mut fx = None;
        g.bench_function(id, |b| {
            let headers: &[Header] = fx.get_or_insert_with(|| {
                let mut rig = SpvRig::new(w, feeds, cold);
                let headers = rig.mine_on_anchor(n);
                rig.sync(&headers, b_headers);
                let tip = rig.chain.private_tip().expect("a tip").0;
                assert_eq!(tip.height, SPV_ANCHOR + n, "the sync reached the last header");
                assert_eq!(rig.chain.ids().count(), w as usize + 1, "only the window is resident");
                headers
            });
            b.iter_batched(
                || SpvRig::new(w, feeds, cold),
                |mut rig| {
                    rig.sync(headers, b_headers);
                    rig
                },
                BatchSize::PerIteration,
            );
        });
    }
    g.finish();
}

criterion_group!(benches, publish, reader, handoff, status, cold, snapshot, open, spv);
criterion_main!(benches);
