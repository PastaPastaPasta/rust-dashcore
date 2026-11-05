//! The quorum object and its caches (CH7 §4.4, D13, D90): `GetQuorum`'s chain overload over a
//! capability, branch by branch; Core's size-1 build; the caches' sizes; the scan cache's stamp;
//! and the lock order, no members call under the quorum caches.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::Arc;

use dash_consensus::{BlockId, ChainAncestry as _};
use dash_dml::{MemEvoStore, MnList, StoreError};
use dash_evo::Effect;
use dash_evo::cbtx::CoinbaseFacts;
use dash_evo::llmq::reference::ModelQuorumManager;
use dash_evo::llmq::{
    LlmqCtx, LlmqError, LlmqSource, QuorumMaterializer as _, ScanCacheStore as _, scan_cache_size,
};
use dash_params::{ChainOption, ChainParamsBuilder, Network, Release};
use dash_primitives::BlockHash;

use super::llmq_members::{Fixture, NEVER, Source, TEST, in_attempt, on_v, pros, world};
use crate::evo::testkit;
use crate::llmq::facade::{AttemptHandle, MembersFacade};
use crate::llmq::quorum::{FacadeScans, Materializer, NoQuorumHook, QuorumBuildHook, QuorumPtr};

/// The hook's calls: each build's base and `populate_cache`.
#[derive(Default)]
struct Hooked(RefCell<Vec<(BlockHash, bool)>>);

impl QuorumBuildHook for Hooked {
    fn built(&self, quorum: &QuorumPtr, populate_cache: bool) {
        self.0.borrow_mut().push((quorum.base_hash, populate_cache));
    }
}

/// The fixture's source, counting the reads it serves while the quorum caches are held.
struct Locked<'a> {
    src: &'a Source<'a>,
    facade: &'a MembersFacade,
    under_quorums: Cell<usize>,
}

impl Locked<'_> {
    fn check(&self) {
        if self.facade.test_quorums().is_none() {
            self.under_quorums.set(self.under_quorums.get() + 1);
        }
    }
}

impl LlmqSource<MnList> for Locked<'_> {
    type Error = StoreError;

    fn mn_list(&self, at: BlockId) -> Result<Option<Arc<MnList>>, StoreError> {
        self.check();
        self.src.mn_list(at)
    }

    fn coinbase_facts(&self, at: BlockId) -> Result<Option<CoinbaseFacts>, StoreError> {
        self.check();
        self.src.coinbase_facts(at)
    }
}

/// The mined rows: LLMQ_TEST's quorum based at 48, mined at 58 on the main chain, and its quorum
/// based at 72, mined only on a fork from 80, at the fork's 82. The fork's tip.
fn mine(fx: &mut Fixture) -> (MemEvoStore, BlockId) {
    let mut store = MemEvoStore::new();
    let mut d = testkit::delta(fx.chain(), fx.at(58));
    d.mined_commitments = vec![testkit::mined(fx.chain(), TEST, fx.at(48), None)];
    store.apply(&d).expect("the main chain's row");
    let fork = fx.fork(80, 4);
    let at82 = fx.chain().ancestor(fork, 82).expect("on the fork");
    let mut d = testkit::delta(fx.chain(), at82);
    d.mined_commitments = vec![testkit::mined(fx.chain(), TEST, fx.at(72), None)];
    store.apply(&d).expect("the fork's row");
    (store, fork)
}

/// `GetQuorum(TEST, base, chain ending at tip, populate)` inside `h`'s attempt.
#[allow(clippy::too_many_arguments)]
fn lookup(
    h: &AttemptHandle<'_>,
    ctx: &LlmqCtx<'_, StoreError>,
    src: &dyn LlmqSource<MnList, Error = StoreError>,
    store: &MemEvoStore,
    fx: &Fixture,
    log: &mut Vec<Effect>,
    hook: &dyn QuorumBuildHook,
    (tip, base, populate): (BlockId, u32, bool),
) -> Result<Option<QuorumPtr>, LlmqError<StoreError>> {
    let view = store.view(fx.chain());
    let mut mat = Materializer {
        cap: h,
        ctx,
        src,
        mined: &view,
        tip,
        effects: log,
        hook,
    };
    mat.get_quorum(TEST, fx.at(base), populate)
}

/// Every branch of `GetQuorum`'s chain overload (`llmq/quorumsman.cpp:539-556`, `:58-104`):
/// - a quorum no block mined answers `nullptr`, as does one mined only off the bound chain, both
///   before the cache;
/// - the first call builds through the facade's members call, calls the hook, and inserts;
/// - the second hits, with no members call and no hook;
/// - bound to the fork, the fork's quorum builds without `populate_cache` and is inserted all the
///   same, so the next call hits (`:101`, D90; Q-2);
/// - a cached object still answers `nullptr` through a chain that did not mine it (`HasQuorum`
///   comes before the cache, `:546`).
#[test]
fn get_quorum_takes_cores_branches() {
    on_v(|v| {
        let mut fx = Fixture::rotated();
        let (store, fork) = mine(&mut fx);
        let w = world(&fx.params);
        let hook = Hooked::default();
        let main = fx.at(120);
        fx.ctx(|ctx| {
            let src = fx.source();
            in_attempt(v, &w, |h, log| {
                let mut quorum = |tip, base, populate| {
                    lookup(h, ctx, &src, &store, &fx, log, &hook, (tip, base, populate))
                };
                assert!(quorum(main, 24, true).expect("held").is_none(), "not mined");
                assert!(quorum(main, 72, true).expect("held").is_none(), "mined off the chain");
                assert_eq!(src.reads(), 0, "neither builds");

                let q48 = quorum(main, 48, true).expect("held").expect("mined at 58");
                assert!(src.reads() > 0, "built");
                assert_eq!((q48.base, q48.mined_block), (fx.at(48), fx.hash(58)));
                assert_eq!(q48.commitment.quorum_hash.as_bytes(), fx.hash(48).as_bytes());
                let cached = w.facade.test_caches().by_base.peek(TEST, &fx.hash(48)).cloned();
                assert_eq!(cached.map(|m| pros(&m)), Some(pros(&q48.members)), "the members call");
                let reads = src.reads();
                let again = quorum(main, 48, true).expect("held").expect("cached");
                assert!(Arc::ptr_eq(&again.0, &q48.0), "the cached object");
                assert_eq!(src.reads(), reads, "a hit builds nothing");

                let q72 = quorum(fork, 72, false).expect("held").expect("mined on the fork");
                let at82 = fx.chain().ancestor(fork, 82).expect("on the fork");
                assert_eq!(q72.mined_block, fx.chain().hash(at82));
                let reads = src.reads();
                let again = quorum(fork, 72, false).expect("held").expect("inserted unpopulated");
                assert!(Arc::ptr_eq(&again.0, &q72.0));
                assert_eq!(src.reads(), reads, "an unpopulated build is cached");
                assert!(quorum(main, 72, true).expect("held").is_none(), "HasQuorum first");
            })
        })
        .expect("released");
        assert_eq!(*hook.0.borrow(), [(fx.hash(48), true), (fx.hash(72), false)]);
        let quorums = w.facade.test_quorums().expect("free");
        assert!(quorums.objects.peek(TEST, &fx.hash(48)).is_some());
        assert!(quorums.objects.peek(TEST, &fx.hash(72)).is_some());
        assert!(quorums.objects.peek(TEST, &fx.hash(24)).is_none());
    });
}

/// OD-CP-51 #2 (DEC-1073): a quorum object built while its source made up a list (members over
/// Core's initial snapshot) stays out of the facade's shared object cache, so a later call through
/// the same facade over a view with the real rows builds its own object with the real members.
/// M8b's killer.
#[test]
fn a_quorum_over_a_made_up_list_stays_out_of_the_object_cache() {
    on_v(|v| {
        let mut fx = Fixture::rotated();
        let (store, _) = mine(&mut fx);
        let w = world(&fx.params);
        let main = fx.at(120);
        let five: BTreeMap<u32, Vec<u32>> = (0..=120).map(|h| (h, (1..=5).collect())).collect();
        fx.ctx(|ctx| {
            let (real, made_up) = (fx.source(), fx.made_up_source(five));
            in_attempt(v, &w, |h, log| {
                let mut quorum = |src: &Source<'_>| {
                    let q = lookup(h, ctx, src, &store, &fx, log, &NoQuorumHook, (main, 48, true));
                    pros(&q.expect("held").expect("mined at 58").members)
                };
                let made = quorum(&made_up);
                assert!(made_up.made_up_lists() > 0, "built over the made-up lists");
                let quorums = w.facade.test_quorums().expect("free");
                assert!(quorums.objects.peek(TEST, &fx.hash(48)).is_none(), "a made-up object");
                drop(quorums);
                let want = quorum(&real);
                assert_ne!(want, made, "the real view's own members");
                let quorums = w.facade.test_quorums().expect("free");
                assert!(quorums.objects.peek(TEST, &fx.hash(48)).is_some(), "the real object");
            })
        })
        .expect("released");
    });
}

/// OD-CP-51 #2: a scan during which the source made up a list writes nothing to the facade's
/// shared scan cache; one with no made-up list writes its entry. M8b2's killer.
#[test]
fn a_scan_over_a_made_up_list_stays_out_of_the_scan_cache() {
    on_v(|v| {
        let fx = Fixture::rotated();
        let w = world(&fx.params);
        let made_up = Cell::new(0_u64);
        let count = || made_up.get();
        in_attempt(v, &w, |h, _| {
            let mut tainted = FacadeScans::new(h, &count);
            made_up.set(1);
            tainted.insert(TEST, fx.hash(48), Vec::new());
            let mut clean = FacadeScans::new(h, &count);
            clean.insert(TEST, fx.hash(72), Vec::new());
        })
        .expect("released");
        let quorums = w.facade.test_quorums().expect("free");
        assert!(quorums.scans.peek(TEST, &fx.hash(48)).is_none(), "the made-up scan");
        assert!(quorums.scans.peek(TEST, &fx.hash(72)).is_some(), "the clean scan");
    });
}

/// Core's size-1 build (`quorumsman.cpp:76-80`, F6), with `-llmqtestparams=1:1`: with
/// `populate_cache` the object is inserted and returned before the contributions, so the hook is
/// not called; without it the build falls through to the hook and to `:101`'s insert.
#[test]
fn a_size_one_build_skips_the_hook_only_when_it_populates() {
    for populate in [true, false] {
        let got = on_v(move |v| {
            let p = [
                (ChainOption::Dip3Params, "2:2".to_owned()),
                (ChainOption::TestActivationHeight, format!("v20@{NEVER}")),
                (ChainOption::TestActivationHeight, format!("mn_rr@{NEVER}")),
                (ChainOption::TestActivationHeight, "v19@2".to_owned()),
                (ChainOption::TestActivationHeight, "dip0024@2".to_owned()),
                (ChainOption::LlmqTestParams, "1:1".to_owned()),
            ]
            .into_iter()
            .fold(ChainParamsBuilder::new(Network::Regtest, Release::V24_0), |b, (o, v)| {
                b.arg(o, v)
            })
            .build()
            .expect("regtest parameters");
            assert_eq!(p.consensus.llmq(TEST).expect("configured").size, 1);
            let mut fx = Fixture::over(p);
            let (store, _) = mine(&mut fx);
            let w = world(&fx.params);
            let hook = Hooked::default();
            let main = fx.at(120);
            let members = fx.ctx(|ctx| {
                let src = fx.source();
                in_attempt(v, &w, |h, log| {
                    let q = lookup(h, ctx, &src, &store, &fx, log, &hook, (main, 48, populate));
                    q.expect("held").expect("mined").members.len()
                })
            });
            let cached =
                w.facade.test_quorums().expect("free").objects.peek(TEST, &fx.hash(48)).is_some();
            (members.expect("released").0, hook.0.borrow().len(), cached)
        });
        let hooked = usize::from(!populate);
        assert_eq!(got, (1, hooked, true), "populate {populate}");
    }
}

/// The caches' sizes per type (`quorumsman.cpp:42`, `:211-213`): the objects `keepOldKeys`, the
/// scans `scan_cache_size`, each equal to the model manager's, on regtest and testnet.
#[test]
fn the_quorum_caches_are_sized_per_type() {
    let regtest = Fixture::rotated().params;
    let testnet =
        ChainParamsBuilder::new(Network::Testnet3, Release::V24_0).build().expect("testnet");
    for p in [regtest, testnet] {
        let w = world(&p);
        let model = ModelQuorumManager::<()>::new(&p);
        let quorums = w.facade.test_quorums().expect("free");
        for (t, llmq) in p.consensus.llmq_params() {
            let objects = quorums.objects.cache(t).expect("configured").max_size();
            let scans = quorums.scans.cache(t).expect("configured").max_size();
            assert_eq!(objects, usize::from(llmq.keep_old_keys), "{t:?}");
            assert_eq!(scans, usize::try_from(scan_cache_size(llmq)).unwrap(), "{t:?}");
            assert_eq!(model.cache_sizes(t), Some((objects, scans)), "{t:?}");
        }
    }
}

/// `FacadeScans::get` restamps its entry (`quorumsman.cpp:215`, `unordered_lru_cache.h:61-70`),
/// so the next truncation keeps it (as CH7b-1's m-4 test for the members caches; Q-5): the
/// cache's `2 × max` entries, the oldest read, one more inserted; the read entry stays and the
/// next oldest goes.
#[test]
fn a_scan_cache_hit_restamps_so_a_truncation_keeps_it() {
    on_v(|v| {
        let fx = Fixture::rotated();
        let w = world(&fx.params);
        let max = w.facade.test_quorums().expect("free").scans.cache(TEST).expect("on").max_size();
        let keys: Vec<BlockHash> =
            (1..=u32::try_from(2 * max + 1).unwrap()).map(|h| fx.hash(h)).collect();
        in_attempt(v, &w, |h, _| {
            let made_up = || 0;
            let mut scans = FacadeScans::new(h, &made_up);
            for key in &keys[..2 * max] {
                scans.insert(TEST, *key, Vec::new());
            }
            assert!(scans.get(TEST, &keys[0]).is_some());
            scans.insert(TEST, keys[2 * max], Vec::new());
        })
        .expect("released");
        let quorums = w.facade.test_quorums().expect("free");
        let kept = |key: &BlockHash| quorums.scans.peek(TEST, key).is_some();
        assert!(kept(&keys[0]), "the read entry outlives the truncation");
        assert!(!kept(&keys[1]), "the next oldest goes");
        assert!(kept(&keys[2 * max]));
    });
}

/// The quorum caches are never held across a build's members call (`quorumsman.cpp:530-531`,
/// `:101`; D90; Q-6): each source read checks that the caches are free.
#[test]
fn a_build_reads_its_members_with_the_quorum_caches_free() {
    on_v(|v| {
        let mut fx = Fixture::rotated();
        let (store, _) = mine(&mut fx);
        let w = world(&fx.params);
        let main = fx.at(120);
        let (reads, under) = fx.ctx(|ctx| {
            let src = fx.source();
            let locked = Locked {
                src: &src,
                facade: &w.facade,
                under_quorums: Cell::new(0),
            };
            in_attempt(v, &w, |h, log| {
                lookup(h, ctx, &locked, &store, &fx, log, &NoQuorumHook, (main, 48, true))
                    .expect("held")
                    .expect("mined");
            })
            .expect("released");
            (src.reads(), locked.under_quorums.get())
        });
        assert!(reads > 0, "built");
        assert_eq!(under, 0, "reads under the quorum caches");
    });
}
