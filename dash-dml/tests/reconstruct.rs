//! Reconstruction equality (EV2 §2.8, §2.10, §6.2; the §3.7 erratum's rebuild-equals-online
//! rule): a list `resolve_list` rebuilds from the stored records equals the list built online,
//! counter, internal ids, every state, the operator keys' stored form and unique slots and
//! `core_order()` included, with the same `CanonicalMNListHash` and snapshot-record bytes. Every
//! record reaches the store through the codec, as a node writes it. Each harness checks every
//! connected block and every disconnected one, each from every snapshot row at it or an
//! ancestor (one row visible at a time) and from none (the default list below DIP3), then
//! through one cache in two orders. The histories cover forks (disconnect, a sibling,
//! reconnect), a restart that writes a new store from the encoded records alone, and version-3
//! transitions.

use std::sync::Arc;

use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::{BlockId, ChainAncestry};
use dash_dml::codec::{
    ContextFreeNetInfo, DmlRecord, canonical_mn_list_hash, decode_record, encode_diff_record,
    encode_snapshot_record,
};
use dash_dml::store::{CacheHint, ListCache, LruListCache, NoListCache, resolve_list};
use dash_dml::testkit::{MemChain, dmn, dmn_with, operator_key, pro_tx};
use dash_dml::{
    DmlDelta, DmlReader, EvoDelta, MemEvoStore, MnList, MnListDiffRecord, MnStateDiff, MnType,
    ProTxHash, SameRecord, StoreError, UniqueKey,
};
use dash_params::BuriedDeployment;
use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::BlockHash;

const NET: ContextFreeNetInfo = ContextFreeNetInfo;

/// DIP3 active from `height` on.
struct Dip3From<'a> {
    chain: &'a MemChain,
    height: u32,
}

impl DeploymentView for Dip3From<'_> {
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        let next = prev.map_or(0, |p| self.chain.header(p).height + 1);
        deployment == Deployment::Buried(BuriedDeployment::Dip0003) && next >= self.height
    }
    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
        self.active_after(self.chain.prev(block), deployment)
    }
}

/// A block's records as the node wrote them.
struct Written {
    block: BlockHash,
    height: u32,
    diff: Vec<u8>,
    snapshot: Option<Vec<u8>>,
}

impl Written {
    /// The block's delta, its records decoded from the written bytes.
    fn delta(&self) -> EvoDelta {
        let record = match decode_record(&self.diff, &NET).expect("decodable") {
            DmlRecord::Diff(decoded) => decoded,
            DmlRecord::Snapshot(_) => panic!("a diff record"),
        };
        let snapshot = self.snapshot.as_ref().map(|bytes| {
            match decode_record(bytes, &NET).expect("decodable") {
                DmlRecord::Snapshot(decoded) => decoded,
                DmlRecord::Diff(_) => panic!("a snapshot record"),
            }
        });
        EvoDelta {
            block: self.block,
            height: self.height,
            dml: Some(DmlDelta {
                block: self.block,
                height: self.height as i32,
                record: Arc::new(record),
                snapshot,
            }),
            mined_commitments: Vec::new(),
            credit_data: None,
            credit_pool: None,
            mnhf_signals: None,
            erased_mined: Vec::new(),
            evo_tip: None,
        }
    }
}

/// The store with one `mn_snapshot` row visible, or none: resolution must start from it.
struct ForcedBase<'a> {
    store: &'a MemEvoStore,
    base: Option<BlockHash>,
}

impl DmlReader for ForcedBase<'_> {
    fn mn_list_snapshot(&self, block: &BlockHash) -> Result<Option<Arc<MnList>>, StoreError> {
        if self.base == Some(*block) {
            self.store.mn_list_snapshot(block)
        } else {
            Ok(None)
        }
    }

    fn mn_list_diff(&self, block: &BlockHash) -> Result<Option<Arc<MnListDiffRecord>>, StoreError> {
        self.store.mn_list_diff(block)
    }
}

/// A chain whose blocks change the list online, block by block, and a store holding the records
/// a node writes (a diff for every DIP3 block, a snapshot at the first one and every `period`).
struct Harness {
    chain: MemChain,
    store: MemEvoStore,
    tip: BlockId,
    online: Vec<(BlockId, MnList)>,
    /// Disconnected blocks and their lists: their records stay, so they stay resolvable.
    stale: Vec<(BlockId, MnList)>,
    /// Every block's records, in the order they were first written.
    written: Vec<Written>,
    dip3: u32,
    period: i32,
}

impl Harness {
    fn new(dip3: u32, period: i32) -> Harness {
        let mut chain = MemChain::new();
        let tip = chain.extend(chain.genesis(), dip3 - 1);
        Harness {
            chain,
            store: MemEvoStore::new(),
            tip,
            online: Vec::new(),
            stale: Vec::new(),
            written: Vec::new(),
            dip3,
            period,
        }
    }

    fn last(&self) -> MnList {
        self.online.last().map_or_else(MnList::default_pre_dip3, |(_, list)| list.clone())
    }

    /// Connects a block whose list is the previous one changed by `f`.
    fn block(&mut self, f: impl FnOnce(&mut MnList)) {
        let prev = self.last();
        let id = self.chain.child(self.tip);
        let header = self.chain.header(id);
        let mut next = prev.clone().with_block_hash(header.hash).with_height(header.height as i32);
        f(&mut next);
        let record = MnListDiffRecord::build(&prev, &next);
        let first = header.height == self.dip3;
        let snapshot = first || next.height() % self.period == 0;
        let written = Written {
            block: header.hash,
            height: header.height,
            diff: encode_diff_record(&record, &NET).expect("encodable").as_bytes().to_vec(),
            snapshot: snapshot.then(|| {
                encode_snapshot_record(&next, &NET).expect("encodable").as_bytes().to_vec()
            }),
        };
        self.store.apply(&written.delta()).expect("a new block's records");
        self.written.push(written);
        next.check_invariants().expect("online invariants");
        self.online.push((id, next));
        self.tip = id;
    }

    /// Disconnects the tip. Its records stay.
    fn disconnect(&mut self) -> BlockId {
        let (id, list) = self.online.pop().expect("a connected block");
        let prev = self.chain.prev(id).expect("a parent");
        self.store.disconnect(&self.chain.hash(id), Some(self.chain.hash(prev))).expect("the tip");
        self.tip = prev;
        self.stale.push((id, list));
        id
    }

    /// Reconnects `id`, a disconnected child of the tip: its records are written again, equal.
    fn reconnect(&mut self, id: BlockId) {
        assert_eq!(self.chain.prev(id), Some(self.tip), "a child of the tip");
        let hash = self.chain.hash(id);
        let written = self.written.iter().find(|w| w.block == hash).expect("written before");
        self.store.apply(&written.delta()).expect("an equal rewrite");
        let at = self.stale.iter().position(|(s, _)| *s == id).expect("disconnected");
        let entry = self.stale.remove(at);
        self.online.push(entry);
        self.tip = id;
    }

    /// A restart: a new store, with no state carried over from the old one, written from the
    /// encoded records alone. (The old store already held records decoded from the same bytes;
    /// the restart shows nothing else, such as the evo tip or write order, is needed.)
    fn restart(&mut self) {
        let mut store = MemEvoStore::new();
        for written in &self.written {
            store.apply(&written.delta()).expect("the written records");
        }
        self.store = store;
    }

    fn deployments(&self) -> Dip3From<'_> {
        Dip3From {
            chain: &self.chain,
            height: self.dip3,
        }
    }

    fn resolve(&self, at: BlockId, cache: &dyn ListCache) -> Result<Arc<MnList>, StoreError> {
        resolve_list(at, &self.chain, &self.deployments(), &self.store, cache)
    }

    /// The rows a resolution of `at` can start from: none (the default list below DIP3), and
    /// each `mn_snapshot` row at `at` or an ancestor.
    fn bases(&self, at: BlockId) -> Vec<Option<BlockHash>> {
        let mut bases = vec![None];
        let mut cursor = Some(at);
        while let Some(id) = cursor {
            let hash = self.chain.hash(id);
            if self.store.mn_list_snapshot(&hash).expect("readable").is_some() {
                bases.push(Some(hash));
            }
            cursor = self.chain.prev(id);
        }
        bases
    }

    /// Every connected and disconnected block, rebuilt from each base without a cache, then
    /// through one cache in two orders, equals its online list.
    fn check_every_height(&self) {
        let all: Vec<_> = self.online.iter().chain(&self.stale).collect();
        for (id, online) in &all {
            for base in self.bases(*id) {
                let store = ForcedBase {
                    store: &self.store,
                    base,
                };
                let rebuilt =
                    resolve_list(*id, &self.chain, &self.deployments(), &store, &NoListCache);
                assert_same(online, &rebuilt.expect("resolvable"));
            }
        }
        let cache = LruListCache::new();
        for (id, online) in all.iter().chain(all.iter().rev()) {
            assert_same(online, &self.resolve(*id, &cache).expect("resolvable"));
        }
    }
}

fn assert_same(online: &MnList, rebuilt: &MnList) {
    rebuilt.check_invariants().expect("rebuilt invariants");
    assert!(online.same_record(rebuilt), "online {online:?}\nrebuilt {rebuilt:?}");
    assert!(online.is_equal(rebuilt), "Core's IsEqual");
    // Each list's operator-key slots are the other's (the unique map keys the captured bytes).
    for (a, b) in [(online, rebuilt), (rebuilt, online)] {
        for dmn in a.iter() {
            let slot = UniqueKey::operator_key(&dmn.cold.pub_key_operator);
            if a.unique_owner(&slot).is_some_and(|o| o.pro_tx_hash == dmn.pro_tx_hash) {
                let owner = b.unique_owner(&slot).map(|o| o.pro_tx_hash);
                assert_eq!(owner, Some(dmn.pro_tx_hash), "operator-key slot");
            }
        }
    }
    assert_eq!(online.core_order(), rebuilt.core_order());
    assert_eq!(canonical_mn_list_hash(online), canonical_mn_list_hash(rebuilt));
    let bytes =
        |l: &MnList| encode_snapshot_record(l, &NET).expect("encodable").as_bytes().to_vec();
    assert_eq!(bytes(online), bytes(rebuilt), "snapshot-record bytes");
    assert_eq!(online.counts(), rebuilt.counts());
    let ids = |l: &MnList| l.iter().map(|d| (d.internal_id, d.pro_tx_hash)).collect::<Vec<_>>();
    assert_eq!(ids(online), ids(rebuilt));
}

fn add(list: &mut MnList, n: u8) {
    list.try_add(dmn(n, MnType::Regular), true).expect("addable");
}

fn add_with_id(list: &mut MnList, n: u8, id: u64) {
    let d = dmn_with(n, MnType::Regular, |d, _| d.internal_id = id);
    list.try_add(d, true).expect("addable");
}

fn next_id(list: &MnList) -> u64 {
    u64::from(list.total_registered())
}

/// The cold state of `n`, changed by `f`.
fn set_cold(list: &mut MnList, pro_tx: ProTxHash, f: impl FnOnce(&mut dash_dml::MnCold)) {
    let old = Arc::clone(list.get(&pro_tx).expect("present"));
    let mut cold = (*old.cold).clone();
    f(&mut cold);
    list.update(&pro_tx, old.hot, Arc::new(cold.rebuilt())).expect("updatable");
}

trait Rebuilt {
    fn rebuilt(self) -> Self;
}

impl Rebuilt for dash_dml::MnCold {
    /// Drops the derived caches of a hand-edited state.
    fn rebuilt(self) -> Self {
        dash_dml::testkit::fresh(self)
    }
}

/// Below DIP3 the answer is the default list; the first DIP3 block starts from it.
#[test]
fn the_dip3_boundary() {
    let mut h = Harness::new(5, 576);
    h.block(|l| add(l, 1));
    let below = h.chain.prev(h.tip).expect("a parent");
    let default = h.resolve(below, &NoListCache).expect("resolvable");
    assert!((*default).same_record(&MnList::default_pre_dip3()));
    h.block(|_| {});
    h.check_every_height();
}

/// GPT F2: a register-and-spend in one block advances the counter, which Core's diff cannot
/// carry; the record's counter restores it.
#[test]
fn register_and_spend_in_one_block() {
    let mut h = Harness::new(3, 576);
    h.block(|l| add(l, 1));
    h.block(|l| {
        let id = next_id(l);
        add_with_id(l, 2, id);
        l.remove(&pro_tx(2)).expect("present");
    });
    assert_eq!(h.last().total_registered(), 3);
    h.block(|l| {
        let id = next_id(l);
        add_with_id(l, 3, id);
    });
    h.block(|l| l.remove(&pro_tx(3)).expect("present"));
    // Two replacements between snapshots, and a trailing registration that disappears while
    // another addition survives.
    h.block(|l| {
        l.remove(&pro_tx(1)).expect("present");
        let id = next_id(l);
        add_with_id(l, 4, id);
    });
    h.block(|l| {
        l.remove(&pro_tx(4)).expect("present");
        let id = next_id(l);
        add_with_id(l, 5, id);
        let id = next_id(l);
        add_with_id(l, 6, id);
        l.remove(&pro_tx(6)).expect("present");
    });
    assert_eq!(h.last().total_registered(), 7);
    h.check_every_height();
}

/// Operator key changes (the §3.7 erratum's tracked cases): an ordinary change of key, and
/// one point re-encoded under a new version. Rebuilt lists hold the same key in the same stored
/// form, and their unique slots.
#[test]
fn operator_key_changes_rebuild_equal() {
    let mut h = Harness::new(3, 4);
    h.block(|l| {
        let a = dmn_with(1, MnType::Regular, |_, c| {
            c.pub_key_operator = operator_key(1, BlsSchemeKind::Legacy)
        });
        l.try_add(a, true).expect("addable");
        add(l, 2);
    });
    h.block(|l| {
        set_cold(l, pro_tx(1), |c| c.pub_key_operator = operator_key(2, BlsSchemeKind::Legacy))
    });
    // The same point, re-encoded: version 2 stores it under the basic scheme.
    h.block(|l| {
        set_cold(l, pro_tx(1), |c| {
            c.version = 2;
            c.pub_key_operator = operator_key(2, BlsSchemeKind::Basic);
        });
    });
    let record = h.store_record(h.tip);
    let (_, state) = &record.diff.updated[0];
    assert_ne!(state.fields & MnStateDiff::PUB_KEY_OPERATOR, 0, "the re-encoding is recorded");
    assert_ne!(state.fields & MnStateDiff::VERSION, 0);
    // The revocation-style reset to the default key.
    h.block(|l| set_cold(l, pro_tx(1), |c| c.reset_operator_fields(BlsSchemeKind::Basic)));
    h.block(|l| {
        set_cold(l, pro_tx(2), |c| c.pub_key_operator = operator_key(2, BlsSchemeKind::Legacy))
    });
    h.block(|_| {});
    h.check_every_height();
}

/// Collision buckets: order-only changes (a hot update that `BuildDiff` drops because the state
/// is equal, sets in an order the diff does not replay) are restored from the trailer.
#[test]
fn collision_order_rebuilds_equal() {
    let colliding = |n: u8, id: u64| {
        dmn_with(n, MnType::Regular, |d, _| {
            let mut bytes = [9u8; 32];
            bytes[31] = n;
            d.pro_tx_hash = ProTxHash::from(bytes);
            d.internal_id = id;
        })
    };
    let key = |n: u8| colliding(n, 0).pro_tx_hash;
    let mut h = Harness::new(3, 576);
    h.block(|l| {
        for (n, id) in [(1, 0), (2, 1), (3, 2)] {
            l.try_add(colliding(n, id), true).expect("addable");
        }
    });
    // Updates in descending id order: Core's apply order (ascending) gives another sequence.
    h.block(|l| {
        for n in [3, 2, 1] {
            let old = Arc::clone(l.get(&key(n)).expect("present"));
            let mut hot = old.hot;
            hot.last_paid_height = 10;
            l.update_hot(&key(n), hot).expect("present");
        }
    });
    // A set with an unchanged state: no diff entry, only the order moves.
    h.block(|l| {
        let old = Arc::clone(l.get(&key(2)).expect("present"));
        l.update_hot(&key(2), old.hot).expect("present");
    });
    let record = h.store_record(h.tip);
    assert!(!record.diff.has_changes());
    assert_eq!(record.collision_buckets_after.len(), 1);
    h.block(|l| l.remove(&key(1)).expect("present"));
    h.block(|l| l.remove(&key(3)).expect("present"));
    h.check_every_height();
}

#[test]
fn a_missing_diff_is_inconsistent() {
    let mut h = Harness::new(3, 576);
    h.block(|l| add(l, 1));
    let skipped = h.chain.child(h.tip);
    h.tip = skipped;
    h.block(|_| {});
    let err = h.resolve(h.tip, &NoListCache).expect_err("a DIP3 block without a diff");
    assert!(matches!(err, StoreError::Inconsistent { .. }), "{err}");
}

/// The cache is offered the result and the mini-snapshots, never changes an answer, and keeps
/// the tip's list over its cap.
#[test]
fn the_cache_is_not_observable() {
    let mut h = Harness::new(3, 1_000);
    for n in 1..=80u8 {
        h.block(|l| add(l, n));
    }
    struct Spy(std::sync::Mutex<Vec<(i32, CacheHint)>>);
    impl ListCache for Spy {
        fn get(&self, _: &dash_primitives::BlockHash) -> Option<Arc<MnList>> {
            None
        }
        fn offer(&self, list: &Arc<MnList>, hint: CacheHint) {
            self.0.lock().unwrap().push((list.height(), hint));
        }
    }
    let spy = Spy(Default::default());
    h.resolve(h.tip, &spy).expect("resolvable");
    let offers = spy.0.into_inner().unwrap();
    let minis: Vec<_> =
        offers.iter().filter(|(_, hint)| *hint == CacheHint::Mini).map(|(h, _)| *h).collect();
    assert_eq!(minis, [32, 64]);
    assert_eq!(offers.last(), Some(&(82, CacheHint::Built)));

    let cache = LruListCache::new();
    let tip_hash = h.chain.header(h.tip).hash;
    cache.set_tip(Some(tip_hash));
    let tip = h.resolve(h.tip, &cache).expect("resolvable");
    for (id, _) in &h.online {
        h.resolve(*id, &cache).expect("resolvable");
    }
    assert!(cache.len() <= LruListCache::MAX_CACHED_LISTS);
    assert!(Arc::ptr_eq(&cache.get(&tip_hash).expect("the tip is pinned"), &tip));
    h.check_every_height();
}

/// Forks and a restart: a block disconnected and reconnected (its records written again,
/// equal), a sibling branch that registers and is then abandoned, registrations after the
/// reconnect, and a new store written from the encoded records alone. Snapshots every 5 blocks
/// give each height several bases, and put rows on the disconnected block and its sibling (both
/// at height 5), so a stale block's row is a base and a reconnect rewrites a row.
#[test]
fn forks_and_restarts_rebuild_equal() {
    let mut h = Harness::new(3, 5);
    h.block(|l| {
        add(l, 1);
        add(l, 2);
    });
    h.block(|l| set_cold(l, pro_tx(1), |c| c.key_id_voting = [0x31; 20].into()));
    h.block(|l| l.remove(&pro_tx(2)).expect("present"));
    let a = h.disconnect();
    assert!(h.store.mn_list_snapshot(&h.chain.hash(a)).expect("readable").is_some());
    // A sibling of `a` registers two masternodes, then loses to `a`.
    h.block(|l| {
        let id = next_id(l);
        add_with_id(l, 3, id);
        let id = next_id(l);
        add_with_id(l, 4, id);
    });
    let sibling = h.disconnect();
    assert!(h.store.mn_list_snapshot(&h.chain.hash(sibling)).expect("readable").is_some());
    h.reconnect(a);
    // Registrations after the reconnect take the ids the sibling used.
    h.block(|l| {
        let id = next_id(l);
        add_with_id(l, 5, id);
    });
    assert_eq!(h.last().get(&pro_tx(5)).expect("present").internal_id, 3);
    let b = h.disconnect();
    h.reconnect(b);
    h.block(|l| {
        let id = next_id(l);
        add_with_id(l, 6, id);
    });
    h.check_every_height();

    h.restart();
    h.check_every_height();
    // The restarted store takes new blocks, and a sibling's records stay readable.
    for n in 7..=9 {
        h.block(|l| {
            let id = next_id(l);
            add_with_id(l, n, id);
        });
    }
    assert!(h.stale.iter().any(|(id, _)| *id == sibling));
    h.check_every_height();
}

/// `OP_DUP OP_HASH160 <n…> OP_EQUALVERIFY OP_CHECKSIG`.
fn p2pkh(n: u8) -> Vec<u8> {
    [&[0x76, 0xa9, 0x14][..], &[n; 20], &[0x88, 0xac]].concat()
}

/// A payout share of `reward` to `script`.
fn payout(script: &[u8], reward: u16) -> dash_primitives::PayoutShare {
    use dash_types::codec::BaseCodec;
    let bytes = [&[script.len() as u8][..], script, &reward.to_le_bytes()].concat();
    dash_primitives::PayoutShare::decode(&mut bytes.as_slice()).expect("a payout share")
}

/// `SetStateVersion(ExtAddr)` (`specialtxman.cpp` v24:51-105) on a version-2 state: the payout
/// script moves into the payout list, the network info becomes extended, and the platform ports
/// are cleared.
fn to_ext_addr(c: &mut dash_dml::MnCold) {
    use dash_primitives::{NetInfo, NetInfoV2};
    if c.payouts.is_empty() && !c.script_payout.is_empty() {
        c.payouts = vec![payout(&c.script_payout, 10_000)];
        c.script_payout.clear();
    }
    c.net_info = Arc::new(NetInfo::Extended(NetInfoV2 {
        version: 1,
        entries: Vec::new(),
    }));
    c.platform_p2p_port = 0;
    c.platform_http_port = 0;
    c.version = 3;
}

/// Version-3 transitions, each as Core writes it (EV2a fix r1, R1-M2): version-2 states and a
/// version-1 one (its legacy key re-encoded) raised to version 3 by `SetStateVersion`, a
/// version-3 registration with shares and early-period fields, a revocation's
/// `ResetOperatorFields` followed by `SetStateVersion` back to 3 in the same block, an operator
/// change, and new payout and early-period values. The fields a version does not serialize stay
/// at their defaults, and network info takes the version's form.
#[test]
fn version_3_transitions_rebuild_equal() {
    let mut h = Harness::new(3, 4);
    h.block(|l| {
        for (n, mn_type) in [(1, MnType::Regular), (2, MnType::Evo)] {
            let d = dmn_with(n, mn_type, |_, c| {
                c.version = 2;
                c.pub_key_operator = operator_key(n, BlsSchemeKind::Basic);
                c.script_payout = p2pkh(n);
                if mn_type == MnType::Evo {
                    c.platform_p2p_port = 26656;
                    c.platform_http_port = 443;
                }
            });
            l.try_add(d, true).expect("addable");
        }
    });
    h.block(|l| {
        set_cold(l, pro_tx(1), to_ext_addr);
        set_cold(l, pro_tx(2), to_ext_addr);
    });
    let record = h.store_record(h.tip);
    assert_eq!(record.diff.updated.len(), 2, "both states change");
    h.block(|l| {
        let id = next_id(l);
        let d = dmn_with(3, MnType::Regular, |d, c| {
            d.internal_id = id;
            c.pub_key_operator = operator_key(3, BlsSchemeKind::Basic);
            to_ext_addr(c);
            c.payouts = vec![payout(&p2pkh(3), 6_000), payout(&p2pkh(4), 4_000)];
            c.shares = vec![dash_primitives::CollateralShare {
                amount: 1_000,
                script_refund: payout(&p2pkh(6), 0).script_payout,
                script_reward: payout(&p2pkh(7), 0).script_payout,
                key_id_owner: [0x36; 20].into(),
            }];
            c.early_period_blocks = 100;
            c.early_penalty = 50;
        });
        l.try_add(d, true).expect("addable");
        // A version-1 state with a legacy key, raised straight to 3 below.
        let id = next_id(l);
        let d = dmn_with(4, MnType::Regular, |d, c| {
            d.internal_id = id;
            c.pub_key_operator = operator_key(4, BlsSchemeKind::Legacy);
            c.script_payout = p2pkh(4);
        });
        l.try_add(d, true).expect("addable");
    });
    // `SetStateVersion(ExtAddr)` from version 1: the same point, re-encoded under the basic
    // scheme, and the version-3 fields.
    h.block(|l| {
        set_cold(l, pro_tx(4), |c| {
            c.pub_key_operator = operator_key(4, BlsSchemeKind::Basic);
            to_ext_addr(c);
        })
    });
    // A revocation after v24: `ResetOperatorFields`, then `SetStateVersion` back to 3.
    h.block(|l| {
        set_cold(l, pro_tx(1), |c| {
            c.reset_operator_fields(BlsSchemeKind::Basic);
            to_ext_addr(c);
        })
    });
    // An operator change (`ProUpRegTx`): the reset, the new key, the version restored.
    h.block(|l| {
        set_cold(l, pro_tx(2), |c| {
            c.reset_operator_fields(BlsSchemeKind::Basic);
            c.pub_key_operator = operator_key(9, BlsSchemeKind::Basic);
            to_ext_addr(c);
        })
    });
    h.block(|l| {
        set_cold(l, pro_tx(3), |c| {
            c.payouts = vec![payout(&p2pkh(5), 10_000)];
            c.early_period_blocks = 0;
        });
    });
    for dmn in h.last().iter() {
        assert_eq!(dmn.cold.version, 3);
        assert!(dmn.cold.script_payout.is_empty() && !dmn.cold.payouts.is_empty());
    }
    h.check_every_height();
    h.restart();
    h.check_every_height();
}

impl Harness {
    fn store_record(&self, at: BlockId) -> Arc<MnListDiffRecord> {
        use dash_dml::DmlReader;
        self.store.mn_list_diff(&self.chain.header(at).hash).expect("readable").expect("present")
    }
}
