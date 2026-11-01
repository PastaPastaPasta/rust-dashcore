//! The tick and the sync state against Core's `ProcessTick` and `CMasternodeSync`: every
//! transition, both sides of each timeout, and the order the assets go in.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI32, Ordering};

use dash_clock::MockClock;
use dash_netaddr::netaddr::NetAddr;

use super::*;
use crate::sync::{BlockRef, MASTERNODE_SYNC_FINISHED};

const T0: i64 = 1_700_000_000;
const EXPIRE: i64 = 300;

#[derive(Default)]
struct FakeNet {
    peers: Mutex<Vec<SyncPeer>>,
    /// Peers in the connection table that are not fully connected.
    others: Mutex<Vec<Service>>,
    sent: Mutex<Vec<(i64, SyncMsg)>>,
    disconnected: Mutex<Vec<i64>>,
    requested: Mutex<BTreeMap<i64, usize>>,
    masternode: bool,
}

impl FakeNet {
    fn sent(&self) -> Vec<(i64, SyncMsg)> {
        std::mem::take(&mut *self.sent.lock())
    }
}

impl SyncNet for FakeNet {
    fn fully_connected(&self) -> Vec<SyncPeer> {
        self.peers.lock().clone()
    }

    fn all_addrs(&self) -> Vec<Service> {
        let mut all: Vec<_> = self.peers.lock().iter().map(|p| p.addr.clone()).collect();
        all.extend(self.others.lock().iter().cloned());
        all
    }

    fn push(&self, peer: i64, msg: SyncMsg) {
        self.sent.lock().push((peer, msg));
    }

    /// `fDisconnect`: the peer leaves the fully connected snapshot (`NodeFullyConnected`).
    // Core: net.cpp:4852-4855
    fn disconnect(&self, peer: i64) {
        self.disconnected.lock().push(peer);
        self.peers.lock().retain(|p| p.id != peer);
    }

    fn requested_object_count(&self, peer: i64) -> usize {
        self.requested.lock().get(&peer).copied().unwrap_or(0)
    }

    fn is_active_masternode(&self) -> bool {
        self.masternode
    }

    /// Deterministic: the order stays, so the last hash is asked first.
    fn shuffle(&self, _hashes: &mut [GovHash]) {}
}

/// A governance manager with objects: every object is offered until `peers_per_hash_max` peers
/// were asked for it.
#[derive(Default)]
struct FakeGov {
    triggers: Vec<GovHash>,
    others: Vec<GovHash>,
    votes: AtomicI32,
    enabled: i32,
    /// `mapAskedRecently` as each call received it.
    asked: Mutex<Vec<AskedRecently>>,
}

impl GovernanceSync for FakeGov {
    fn is_valid(&self) -> bool {
        true
    }

    fn vote_count(&self) -> i32 {
        self.votes.load(Ordering::SeqCst)
    }

    fn enabled_masternode_count(&self) -> i32 {
        self.enabled
    }

    fn fetch_governance_object_votes(
        &self,
        peers_per_hash_max: usize,
        now: i64,
        asked: &mut AskedRecently,
    ) -> (Vec<GovHash>, Vec<GovHash>) {
        self.asked.lock().push(asked.clone());
        let mut open = |hashes: &[GovHash]| {
            hashes
                .iter()
                .filter(|h| {
                    let peers = asked.entry(**h).or_default();
                    peers.retain(|_, expiry| *expiry >= now);
                    peers.len() < peers_per_hash_max
                })
                .copied()
                .collect::<Vec<_>>()
        };
        (open(&self.triggers), open(&self.others))
    }

    fn vote_bloom_filter(&self, hash: &GovHash) -> FilterLoad {
        FilterLoad {
            data: hash.to_vec(),
            hash_funcs: 1,
            tweak: 2,
            flags: 3,
        }
    }
}

fn addr(n: u8) -> Service {
    Service::new(NetAddr::ipv4([10, 0, 0, n]), 19899)
}

fn peer(id: i64) -> SyncPeer {
    SyncPeer {
        id,
        addr: addr(id as u8),
        can_relay: true,
        inbound: false,
        unrestricted: false,
        block_relay_only: false,
    }
}

struct World {
    clock: Arc<MockClock>,
    net: Arc<FakeNet>,
    nf: Arc<NetFulfilledRequestManager>,
    mgr: SyncManager,
}

impl World {
    fn sync(&self) -> &MasternodeSync {
        self.mgr.sync()
    }

    fn at(&self, t: i64) {
        self.clock.set_mocktime(t);
    }

    /// One tick at `t`.
    fn tick(&self, t: i64) {
        self.at(t);
        self.mgr.process_tick();
    }

    /// The tip reaches the best header at `t` (`UpdatedBlockTip` out of IBD).
    fn reach_best_header(&self, t: i64) {
        self.at(t);
        let tip = BlockRef {
            height: 5,
            hash: [5; 32],
        };
        self.sync().updated_block_tip(Some(tip), tip, false);
    }

    fn set_peers(&self, peers: Vec<SyncPeer>) {
        *self.net.peers.lock() = peers;
    }
}

fn world_with(cfg: SyncConfig, net: FakeNet, gov: Arc<dyn GovernanceSync>) -> World {
    let clock = Arc::new(MockClock::at(T0));
    let nf = Arc::new(NetFulfilledRequestManager::new(clock.clone(), EXPIRE));
    let dir = tempfile::tempdir().unwrap();
    assert!(nf.load_cache(&dir.path().join("netfulfilled.dat"), [1, 2, 3, 4], false));
    let net = Arc::new(net);
    let notifier = FullSyncNotifier {
        net: net.clone(),
        netfulfilled: nf.clone(),
    };
    let sync = Arc::new(MasternodeSync::new(clock.clone(), Box::new(notifier)));
    let mgr = SyncManager::new(clock.clone(), cfg, sync, nf.clone(), net.clone(), gov);
    World {
        clock,
        net,
        nf,
        mgr,
    }
}

const REGTEST: SyncConfig = SyncConfig {
    mockable_chain: true,
    mainnet: false,
    sync_mempool: true,
};

fn world(gov_valid: bool) -> World {
    world_with(
        REGTEST,
        FakeNet::default(),
        Arc::new(EmptyGovernance {
            valid: gov_valid,
        }),
    )
}

/// The first tick only sets `nTimeLastProcess`; the next runs 6 s later, not 5.
#[test]
fn the_tick_runs_every_six_seconds_from_the_first() {
    let w = world(true);
    w.set_peers(vec![peer(1)]);
    w.reach_best_header(T0);
    w.tick(T0 + 100);
    w.tick(T0 + 105);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_BLOCKCHAIN, "5 s after the first tick");
    w.tick(T0 + 106);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_GOVERNANCE, "6 s after it");
    // The gate counts from the last processed tick, not the last call.
    w.tick(T0 + 111);
    assert!(w.net.sent().iter().all(|(_, m)| *m == SyncMsg::Mempool));
    w.tick(T0 + 112);
    assert_eq!(w.net.sent().len(), 1, "one govsync at the next processed tick");
}

/// BLOCKCHAIN switches when the tip reached the best header and nothing bumped the asset for
/// more than 30 s (3 peers or fewer) or more than 6 s (more than 3).
#[test]
fn blockchain_switches_after_its_timeout_at_the_best_header() {
    for (peers, timeout) in [(1, MASTERNODE_SYNC_TIMEOUT_SECONDS), (3, 30), (4, 6)] {
        let w = world(true);
        w.set_peers((1..=peers).map(peer).collect());
        w.tick(T0);
        w.reach_best_header(T0 + 1);
        let bump = w.sync().last_bump();
        w.tick(bump + timeout);
        assert_eq!(
            w.sync().asset_id(),
            MASTERNODE_SYNC_BLOCKCHAIN,
            "{peers} peers, at the timeout"
        );
        w.tick(bump + timeout + 6);
        assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_GOVERNANCE, "{peers} peers, past it");
    }
    // Exactly one second past the bound switches (`>`, not `>=` + 1).
    let w = world(true);
    w.set_peers(vec![peer(1)]);
    w.tick(T0 - 100);
    w.reach_best_header(T0);
    w.tick(T0 + 31);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_GOVERNANCE);
}

#[test]
fn blockchain_waits_for_the_best_header_and_a_peer() {
    let w = world(true);
    w.set_peers(vec![peer(1)]);
    w.tick(T0);
    w.tick(T0 + 1000);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_BLOCKCHAIN, "best header not reached");
    // Not at the best header: another header is ahead.
    w.at(T0 + 1001);
    let best = BlockRef {
        height: 9,
        hash: [9; 32],
    };
    let tip = BlockRef {
        height: 8,
        hash: [8; 32],
    };
    w.sync().updated_block_tip(Some(best), tip, false);
    w.tick(T0 + 2000);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_BLOCKCHAIN);
    // At the best header but no peer: the per-peer loop never runs.
    w.set_peers(vec![]);
    w.reach_best_header(T0 + 2001);
    w.tick(T0 + 3000);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_BLOCKCHAIN, "no peer");
    w.set_peers(vec![peer(1)]);
    w.tick(T0 + 3006);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_GOVERNANCE);
}

/// A header, a header tip or a block postpones BLOCKCHAIN's timeout.
#[test]
fn chain_notifications_postpone_blockchain() {
    let w = world(true);
    w.set_peers(vec![peer(1)]);
    w.tick(T0);
    w.reach_best_header(T0 + 1);
    let new = BlockRef {
        height: 6,
        hash: [6; 32],
    };
    w.at(T0 + 20);
    w.sync().accepted_block_header(new);
    assert_eq!(w.sync().last_bump(), T0 + 20);
    w.at(T0 + 40);
    w.sync().notify_header_tip(Some(new), false);
    assert_eq!(w.sync().last_bump(), T0 + 40);
    w.sync().notify_header_tip(None, false);
    w.tick(T0 + 70);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_BLOCKCHAIN, "30 s since the last bump");
    w.tick(T0 + 76);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_GOVERNANCE);
    // Past BLOCKCHAIN, headers no longer bump.
    let bump = w.sync().last_bump();
    w.at(T0 + 80);
    w.sync().accepted_block_header(new);
    w.sync().notify_header_tip(Some(new), false);
    assert_eq!(w.sync().last_bump(), bump);
}

/// On the switch, every outbound, non-block-relay peer not asked before gets `mempool`, once.
#[test]
fn the_switch_asks_outbound_peers_for_their_mempools() {
    let w = world(true);
    let mut inbound = peer(2);
    inbound.inbound = true;
    let mut block_relay = peer(3);
    block_relay.block_relay_only = true;
    let mut masternode = peer(4);
    masternode.can_relay = false;
    w.set_peers(vec![peer(1), inbound, block_relay, masternode, peer(5)]);
    w.tick(T0);
    w.reach_best_header(T0 + 1);
    w.tick(T0 + 8);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_GOVERNANCE);
    let sent = w.net.sent();
    // `CanRelay` is not checked for the mempool request (only for the outer loop's peer).
    let mempool: Vec<_> =
        sent.iter().filter(|(_, m)| *m == SyncMsg::Mempool).map(|(p, _)| *p).collect();
    assert_eq!(mempool, [1, 4, 5]);
    for id in [1, 4, 5] {
        assert!(w.nf.has_fulfilled_request(&addr(id), request::MEMPOOL_SYNC));
    }
    // A reset and a second switch skip the peers asked before.
    w.sync().reset(true, true);
    w.reach_best_header(T0 + 9);
    w.tick(T0 + 20);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_GOVERNANCE);
    assert!(w.net.sent().iter().all(|(_, m)| *m != SyncMsg::Mempool));
}

#[test]
fn syncmempool_off_sends_no_mempool() {
    let cfg = SyncConfig {
        sync_mempool: false,
        ..REGTEST
    };
    let w = world_with(
        cfg,
        FakeNet::default(),
        Arc::new(EmptyGovernance {
            valid: true,
        }),
    );
    w.set_peers(vec![peer(1)]);
    w.tick(T0);
    w.reach_best_header(T0 + 1);
    w.tick(T0 + 32);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_GOVERNANCE);
    assert!(w.net.sent().is_empty());
}

fn at_governance(w: &World) {
    w.tick(T0);
    w.reach_best_header(T0 + 1);
    w.tick(T0 + 32);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_GOVERNANCE);
    w.net.sent();
}

/// Without a governance manager (`-disablegovernance`, rdashd's default until GV1b), GOVERNANCE
/// ends at its first tick with a peer, sending nothing, and every peer gets `full-sync`.
#[test]
fn invalid_governance_finishes_at_once() {
    let w = world(false);
    w.set_peers(vec![peer(1), peer(2)]);
    w.net.others.lock().push(addr(9));
    at_governance(&w);
    w.tick(T0 + 38);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_FINISHED);
    assert!(w.sync().is_synced() && w.sync().is_blockchain_synced());
    assert!(w.net.sent().is_empty());
    for a in [addr(1), addr(2), addr(9)] {
        assert!(w.nf.has_fulfilled_request(&a, request::FULL_SYNC), "{a:?}");
    }
}

/// An empty, valid governance manager: one `govsync(0)` per tick to the next peer, each peer
/// once, then the 30 s timeout since the last bump finishes the asset.
#[test]
fn empty_governance_asks_one_peer_per_tick_then_times_out() {
    let w = world(true);
    let mut masternode = peer(2);
    masternode.can_relay = false;
    w.set_peers(vec![peer(1), masternode, peer(3)]);
    at_governance(&w);
    let start = w.sync().last_bump();
    let govsync = SyncMsg::GovSync {
        hash: [0; 32],
        filter: empty_bloom_filter(),
    };
    w.tick(T0 + 38);
    assert_eq!(w.net.sent(), [(1, govsync.clone())]);
    assert_eq!(w.sync().attempt(), 1);
    w.tick(T0 + 44);
    assert_eq!(w.net.sent(), [(3, govsync)]);
    assert_eq!(w.sync().attempt(), 2);
    w.tick(T0 + 50);
    assert!(w.net.sent().is_empty(), "every peer asked");
    assert!(w.nf.has_fulfilled_request(&addr(3), request::GOVERNANCE_SYNC));
    assert!(!w.nf.has_fulfilled_request(&addr(2), request::GOVERNANCE_SYNC));
    w.tick(start + MASTERNODE_SYNC_TIMEOUT_SECONDS);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_GOVERNANCE, "at the timeout");
    w.tick(start + MASTERNODE_SYNC_TIMEOUT_SECONDS + 6);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_FINISHED, "past it");
}

/// The warning "failed to sync" is logged only when no peer was asked; the switch happens
/// either way. With no eligible peer the asset still times out.
#[test]
fn governance_times_out_without_an_eligible_peer() {
    let w = world(true);
    let mut masternode = peer(1);
    masternode.can_relay = false;
    w.set_peers(vec![masternode, peer(2)]);
    at_governance(&w);
    w.set_peers(vec![peer(2)]);
    w.nf.add_fulfilled_request(&addr(2), request::GOVERNANCE_SYNC);
    let start = w.sync().last_bump();
    w.tick(start + 31);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_FINISHED);
    assert_eq!(w.sync().attempt(), 0);
}

/// A peer with `full-sync` is disconnected while the sync runs. A `noban` or manual peer
/// without the process's `allow-sync` key first has its requests cleared, `full-sync` included,
/// and gets the key; while the key lasts, a new `full-sync` disconnects it too. Synced, the tick
/// disconnects no one.
#[test]
fn full_sync_peers_are_disconnected_unless_unrestricted() {
    let w = world(false);
    let mut manual = peer(2);
    manual.unrestricted = true;
    w.set_peers(vec![peer(1), manual.clone()]);
    w.tick(T0);
    w.nf.add_fulfilled_request(&addr(1), request::FULL_SYNC);
    w.nf.add_fulfilled_request(&addr(2), request::FULL_SYNC);
    w.tick(T0 + 6);
    assert_eq!(*w.net.disconnected.lock(), [1]);
    assert!(!w.nf.has_fulfilled_request(&addr(2), request::FULL_SYNC), "cleared");
    w.nf.add_fulfilled_request(&addr(2), request::FULL_SYNC);
    w.tick(T0 + 12);
    assert_eq!(*w.net.disconnected.lock(), [1, 2], "peer 1 left the snapshot");
    // Synced: only the vote requests run.
    w.set_peers(vec![peer(1), manual]);
    w.reach_best_header(T0 + 13);
    w.nf.remove_all_fulfilled_requests(&addr(1));
    w.tick(T0 + 50);
    w.tick(T0 + 56);
    assert!(w.sync().is_synced());
    let before = w.net.disconnected.lock().len();
    w.tick(T0 + 62);
    assert_eq!(w.net.disconnected.lock().len(), before);
}

/// More than an hour between ticks restarts the sync, except on mockable chains and
/// masternodes.
#[test]
fn an_hour_without_ticks_restarts_the_sync() {
    let real = SyncConfig {
        mockable_chain: false,
        ..REGTEST
    };
    for (cfg, masternode, restarts) in
        [(real, false, true), (REGTEST, false, false), (real, true, false)]
    {
        let net = FakeNet {
            masternode,
            ..FakeNet::default()
        };
        let w = world_with(
            cfg,
            net,
            Arc::new(EmptyGovernance {
                valid: false,
            }),
        );
        w.set_peers(vec![peer(1)]);
        at_governance(&w);
        // Exactly an hour is an ordinary tick, which finishes the (invalid) governance asset.
        w.tick(T0 + 32 + 3600);
        assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_FINISHED, "exactly an hour");
        w.tick(T0 + 32 + 3600 + 3601);
        let expect = if restarts {
            MASTERNODE_SYNC_BLOCKCHAIN
        } else {
            MASTERNODE_SYNC_FINISHED
        };
        assert_eq!(w.sync().asset_id(), expect, "{cfg:?} masternode {masternode}");
    }
}

#[test]
fn the_asset_order_is_blockchain_governance_finished() {
    let w = world(true);
    assert_eq!(w.sync().asset_name(), "MASTERNODE_SYNC_BLOCKCHAIN");
    assert_eq!(w.sync().sync_status(), "Synchronizing blockchain…");
    assert!(!w.sync().is_blockchain_synced());
    w.at(T0 + 10);
    w.sync().switch_to_next_asset();
    assert_eq!(w.sync().asset_id(), 4);
    assert_eq!(w.sync().asset_name(), "MASTERNODE_SYNC_GOVERNANCE");
    assert_eq!(w.sync().sync_status(), "Synchronizing governance objects…");
    assert!(w.sync().is_blockchain_synced() && !w.sync().is_synced());
    assert_eq!((w.sync().asset_start_time(), w.sync().last_bump()), (T0 + 10, T0 + 10));
    w.sync().bump_attempt();
    w.at(T0 + 20);
    w.sync().switch_to_next_asset();
    assert_eq!(w.sync().asset_id(), 999);
    assert_eq!(w.sync().asset_name(), "MASTERNODE_SYNC_FINISHED");
    assert_eq!(w.sync().sync_status(), "Synchronization finished");
    assert_eq!(w.sync().attempt(), 0);
    // Synced, the switch's own bump does not move the time; FINISHED stays.
    assert_eq!(w.sync().last_bump(), T0 + 10);
    w.at(T0 + 30);
    w.sync().switch_to_next_asset();
    assert_eq!(w.sync().asset_id(), 999);
    assert_eq!(w.sync().last_bump(), T0 + 10);
    assert_eq!(w.sync().asset_start_time(), T0 + 30);
}

/// An unforced reset waits 900 s after the last `UpdatedBlockTip`; a forced one does not.
#[test]
fn reset_respects_the_last_block_tip_unless_forced() {
    let w = world(true);
    let tip = BlockRef {
        height: 1,
        hash: [1; 32],
    };
    w.at(T0);
    w.sync().switch_to_next_asset();
    w.sync().updated_block_tip(Some(tip), tip, false);
    // Core's 900 s, not the constant, so that a changed constant fails.
    w.at(T0 + 899);
    w.sync().reset(false, true);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_GOVERNANCE);
    w.at(T0 + 900);
    w.sync().reset(false, true);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_BLOCKCHAIN);
    assert!(!w.sync().is_reached_best_header());
    w.sync().switch_to_next_asset();
    w.sync().reset(true, false);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_BLOCKCHAIN);
}

/// A tip in IBD after the switch resets; so does losing the best header.
#[test]
fn updated_block_tip_resets_a_premature_switch() {
    let w = world(true);
    let a = BlockRef {
        height: 1,
        hash: [1; 32],
    };
    let b = BlockRef {
        height: 2,
        hash: [2; 32],
    };
    w.at(T0);
    w.sync().switch_to_next_asset();
    w.sync().updated_block_tip(Some(a), a, true);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_BLOCKCHAIN, "IBD after the switch");
    // IBD before the switch only bumps.
    w.at(T0 + 5);
    w.sync().updated_block_tip(Some(a), a, true);
    assert_eq!(w.sync().last_bump(), T0 + 5);
    assert!(!w.sync().is_reached_best_header(), "IBD does not set it");
    w.sync().updated_block_tip(Some(a), a, false);
    assert!(w.sync().is_reached_best_header());
    w.sync().switch_to_next_asset();
    w.sync().updated_block_tip(Some(b), a, false);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_BLOCKCHAIN, "fell behind the best header");
    assert!(!w.sync().is_reached_best_header());
    // No best header: nothing more.
    w.sync().updated_block_tip(None, a, false);
    assert!(!w.sync().is_reached_best_header());
    // Synced, a block changes nothing but the reset clock.
    w.sync().switch_to_next_asset();
    w.sync().switch_to_next_asset();
    w.sync().updated_block_tip(Some(b), a, true);
    assert!(w.sync().is_synced());
}

fn gov_world(gov: FakeGov, mainnet: bool) -> World {
    let cfg = SyncConfig {
        mockable_chain: !mainnet,
        mainnet,
        sync_mempool: false,
    };
    world_with(cfg, FakeNet::default(), Arc::new(gov))
}

/// With objects, each `govsync` peer is asked for one object's votes per tick on mainnet,
/// triggers first, with the object's vote filter, and never twice within the hour.
// Core: node/sync_manager.cpp:65, :119 (`nNow + nTimeout`, an hour)
#[test]
fn votes_are_requested_per_object_triggers_first() {
    let gov = Arc::new(FakeGov {
        triggers: vec![[1; 32]],
        others: vec![[2; 32], [3; 32]],
        ..FakeGov::default()
    });
    let cfg = SyncConfig {
        mockable_chain: false,
        mainnet: true,
        sync_mempool: false,
    };
    let w = world_with(cfg, FakeNet::default(), gov.clone());
    w.set_peers(vec![peer(1)]);
    at_governance(&w);
    w.tick(T0 + 38);
    let sent = w.net.sent();
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(matches!(&sent[0].1, SyncMsg::GovSync { hash, .. } if *hash == [0; 32]));
    let SyncMsg::GovSync {
        hash,
        filter,
    } = &sent[1].1
    else {
        panic!("{sent:?}")
    };
    assert_eq!((*hash, filter.data.as_slice()), ([1; 32], &[1; 32][..]), "the trigger first");
    w.tick(T0 + 44);
    let hashes: Vec<_> = w
        .net
        .sent()
        .into_iter()
        .map(|(_, m)| match m {
            SyncMsg::GovSync {
                hash,
                ..
            } => hash,
            SyncMsg::Mempool => panic!(),
        })
        .collect();
    assert_eq!(hashes, [[3; 32]], "then the others, last first");
    // The trigger's peer expires from `mapAskedRecently` an hour after it was asked.
    let asked = gov.asked.lock().last().cloned().unwrap();
    assert_eq!(asked[&[1; 32]][&addr(1)], T0 + 38 + 3600);
}

/// Off mainnet, a peer is asked for `max(1, 2000 / enabled)` objects per call; a peer close to
/// `MAX_INV_SZ` requested objects is skipped.
#[test]
fn vote_requests_scale_with_the_masternode_count_and_respect_max_inv() {
    let hashes: Vec<GovHash> = (1..=10).map(|n| [n; 32]).collect();
    for (enabled, expect) in [(0, 10), (400, 5), (1000, 2), (5000, 1)] {
        let gov = FakeGov {
            others: hashes.clone(),
            enabled,
            ..FakeGov::default()
        };
        let w = gov_world(gov, false);
        w.set_peers(vec![peer(1)]);
        at_governance(&w);
        w.tick(T0 + 38);
        assert_eq!(w.net.sent().len(), 1 + expect, "{enabled} enabled");
    }
    let gov = FakeGov {
        others: hashes,
        ..FakeGov::default()
    };
    let w = gov_world(gov, false);
    w.set_peers(vec![peer(1)]);
    w.net.requested.lock().insert(1, MAX_INV_SZ - PROJECTED_VOTES + 1);
    at_governance(&w);
    w.tick(T0 + 38);
    assert_eq!(w.net.sent().len(), 1, "only the govsync");
    w.net.requested.lock().insert(1, MAX_INV_SZ - PROJECTED_VOTES);
    w.tick(T0 + 44);
    assert_eq!(w.net.sent().len(), 10, "at the bound");
}

/// Once every object was asked, GOVERNANCE ends when more than 30 s passed and fewer than
/// `max(0.01 % of the votes, 6)` new votes came in one tick.
#[test]
fn governance_finishes_when_the_votes_stop() {
    let gov = Arc::new(FakeGov {
        others: vec![[1; 32]],
        ..FakeGov::default()
    });
    let w = world_with(REGTEST, FakeNet::default(), gov.clone());
    w.set_peers(vec![peer(1)]);
    at_governance(&w);
    // The objects' arrival bumps the asset (GV1b's handlers), keeping its timeout away.
    let bump = |t| {
        w.at(t);
        w.sync().bump_asset_last_time("test");
    };
    w.tick(T0 + 38);
    assert_eq!(w.net.sent().len(), 2);
    // The one object was asked at T0 + 38, so `nTimeNoObjectsLeft` is T0 + 38.
    bump(T0 + 43);
    w.tick(T0 + 44);
    gov.votes.store(100, Ordering::SeqCst);
    for t in [T0 + 50, T0 + 56, T0 + 62] {
        bump(t - 1);
        w.tick(t);
        gov.votes.fetch_add(100, Ordering::SeqCst);
        assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_GOVERNANCE, "votes still arriving");
    }
    // Exactly 30 s since T0 + 38 is not more than 30, though only 5 votes came in the tick.
    bump(T0 + 67);
    gov.votes.fetch_add(5 - 100, Ordering::SeqCst);
    w.tick(T0 + 68);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_GOVERNANCE, "30 s after the last object");
    // More than 30 s; 6 more votes in a tick are still too many.
    bump(T0 + 73);
    gov.votes.fetch_add(6, Ordering::SeqCst);
    w.tick(T0 + 74);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_GOVERNANCE, "6 votes in the tick");
    bump(T0 + 79);
    gov.votes.fetch_add(5, Ordering::SeqCst);
    w.tick(T0 + 80);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_FINISHED, "5 votes in the tick");
}

/// `ssc`'s first tick sets the `allow-sync-<ms>` key from the system clock, for the process.
#[test]
fn the_allow_sync_key_is_set_once_from_system_time() {
    let w = world(true);
    let mut manual = peer(1);
    manual.unrestricted = true;
    w.set_peers(vec![manual]);
    w.tick(T0);
    w.tick(T0 + 6);
    let store = w.nf.snapshot();
    let names: Vec<_> = store.requests[&addr(1)].keys().cloned().collect();
    assert_eq!(names.len(), 1);
    let name = String::from_utf8(names[0].clone()).unwrap();
    let ms: i64 = name.strip_prefix("allow-sync-").unwrap().parse().unwrap();
    assert!((ms - system_millis()).abs() < 60_000, "{name}");
    w.tick(T0 + 12);
    assert_eq!(w.nf.snapshot(), store, "set once");
}

/// A shutdown request skips the scheduled tick.
#[test]
fn the_scheduled_tick_stops_at_shutdown() {
    let w = world(true);
    w.set_peers(vec![peer(1)]);
    w.reach_best_header(T0);
    w.at(T0 + 100);
    w.mgr.scheduled_tick(false);
    w.at(T0 + 200);
    w.mgr.scheduled_tick(true);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_BLOCKCHAIN);
    w.mgr.scheduled_tick(false);
    assert_eq!(w.sync().asset_id(), MASTERNODE_SYNC_GOVERNANCE);
}
