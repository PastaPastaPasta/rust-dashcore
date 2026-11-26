//! S3c: the mnsync seam, against dashd v24.0.0's R5 recording and SP1's `mnsync-governance`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use dash_dml::{MnList, testkit};
use dash_governance::GovParams;
use dash_governance::manager::{Effects, GovClock, Manager, StoreState};
use dash_governance::object;
use dash_governance::tally::ObjectState;
use dash_governance::vote;
use dash_p2p::{NetAddr, Service};
use dash_p2p_core::FilterLoad;
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{BlockHash, GovObject};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_indexes::{IndexError, IndexStatus, IndexSummary, IndexedTx, TxIndexReader, Txid};
use dashd_masternode::meta::GovHash;
use dashd_masternode::netfulfilled::NetFulfilledRequestManager;
use dashd_masternode::sync::{BlockRef, MasternodeSync, NodeSyncNotifier};
use dashd_masternode::sync_manager::{
    GovernanceSync, SyncConfig, SyncManager, SyncMsg, SyncNet, SyncPeer,
};
use dashd_p2p::sim::VirtualClock;

use super::{DmnTip, NodeGovernanceSync};
use crate::exec::{EffectsSink, GovCommand, GovEnv, GovExec};
use crate::handle::GovernanceHandle;
use crate::rand::{GovRng, ReplayRng};

fn params() -> GovParams {
    let chain = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest");
    GovParams::from_chain_params(&chain).expect("its parameters")
}

/// The seam posts no command: an executor that drops what it is given.
struct NoExec;

impl GovExec for NoExec {
    fn post(&self, _command: GovCommand) {}
}

struct NoEnv;

impl GovEnv for NoEnv {
    fn clock(&self) -> GovClock {
        GovClock::new(0, 0)
    }

    fn synced(&self) -> bool {
        true
    }

    fn blockchain_synced(&self) -> bool {
        true
    }
}

struct NoSink;

impl EffectsSink for NoSink {
    fn submit(&self, _effects: Effects) {}
}

struct NoIndex;

impl IndexStatus for NoIndex {
    fn summary(&self) -> IndexSummary {
        IndexSummary {
            name: "txindex",
            synced: true,
            best_block_height: 0,
        }
    }

    fn block_until_synced(&self) -> bool {
        true
    }
}

impl TxIndexReader for NoIndex {
    fn find_tx(&self, _txid: &Txid) -> Result<Option<IndexedTx>, IndexError> {
        Ok(None)
    }

    fn has_tx(&self, _txid: &Txid) -> Result<bool, IndexError> {
        Ok(false)
    }
}

struct Tip(Arc<MnList>);

impl DmnTip for Tip {
    fn dmn_tip_list(&self) -> Arc<MnList> {
        Arc::clone(&self.0)
    }
}

/// A list of `n` valid masternodes.
fn list_of(n: u32) -> MnList {
    let mut list = MnList::new(BlockHash::from_lendian([0; 32]), 0, 0);
    for i in 1..=n {
        list.try_add(testkit::numbered(i), true).expect("a fresh masternode");
    }
    list
}

/// The recorded tweaks, fed as the seam draws them.
#[derive(Clone, Default)]
struct SharedRng(Arc<Mutex<VecDeque<u32>>>);

impl SharedRng {
    fn push(&self, tweak: u32) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push_back(tweak);
    }

    fn remaining(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).len()
    }
}

impl GovRng for SharedRng {
    fn bloom_tweak(&mut self) -> u32 {
        let next = self.0.lock().unwrap_or_else(PoisonError::into_inner).pop_front();
        next.expect("a recorded tweak")
    }

    fn sample(&mut self, _v: &mut Vec<[u8; 32]>, _k: usize) {
        panic!("the seam never samples");
    }

    fn shuffle(&mut self, _v: &mut [[u8; 32]]) {
        panic!("the seam never shuffles");
    }
}

type Handle = GovernanceHandle<NoExec>;

fn handle_over(manager: Manager) -> Arc<Handle> {
    GovernanceHandle::new(
        manager,
        NoExec,
        Arc::new(NoEnv),
        Arc::new(NoSink),
        Arc::new(NoIndex),
        params(),
    )
}

/// A loaded manager holding `objects`, each with its votes in arrival order.
fn loaded(objects: &[(GovObject, Vec<dash_primitives::GovVote>)]) -> Manager {
    let mut state = StoreState::default();
    for (obj, votes) in objects {
        let hash = object::hash(obj);
        let mut s = ObjectState::new(object::object_type(obj), hash);
        for v in votes {
            s.file.add(v);
        }
        state.objects.insert(hash, (obj.clone(), s));
    }
    let mut m = Manager::new();
    m.import_state(state);
    m.set_loaded(true);
    m
}

/// `objects` as a restarted dashd loads them: `InitOnLoad` on a fresh manager, at height 0.
// Core: init.cpp:2258; governance/governance.h:260; governance/governance.cpp:1009-1027
fn restarted(objects: &[(GovObject, Vec<dash_primitives::GovVote>)]) -> Manager {
    let mut m = loaded(objects);
    m.init_on_load(&GovClock::new(1_700_000_000, 1_700_000_000), &params());
    m
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `MNGOVERNANCESYNC`'s payload: the hash, then the filter (`CBloomFilter`'s serialization).
fn govsync_payload(hash: &GovHash, f: &FilterLoad) -> Vec<u8> {
    let mut out = hash.to_vec();
    assert!(f.data().len() < 0xfd);
    out.push(f.data().len() as u8);
    out.extend_from_slice(f.data());
    out.extend(f.hash_funcs().to_le_bytes());
    out.extend(f.tweak().to_le_bytes());
    out.push(f.flags());
    out
}

fn fixture(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A scratch directory for a netfulfilled file, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        use std::os::unix::fs::DirBuilderExt;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir()
            .join(format!("dashd-governance-s3c-{name}-{}-{n}", std::process::id()));
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).expect("scratch dir");
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The tick's network: the peers the recording connected, in connection order, and every push.
#[derive(Default)]
struct FakeNet {
    peers: Mutex<Vec<SyncPeer>>,
    pushed: Mutex<Vec<(i64, SyncMsg)>>,
}

impl FakeNet {
    fn add(&self, id: i64, inbound: bool) {
        self.peers.lock().unwrap_or_else(PoisonError::into_inner).push(SyncPeer {
            id,
            addr: Service::new(NetAddr::ipv4([127, 0, 0, id as u8]), 12000),
            can_relay: true,
            inbound,
            unrestricted: false,
            block_relay_only: false,
        });
    }

    fn take(&self) -> Vec<(i64, SyncMsg)> {
        std::mem::take(&mut *self.pushed.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl SyncNet for FakeNet {
    fn fully_connected(&self) -> Vec<SyncPeer> {
        self.peers.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn all_addrs(&self) -> Vec<Service> {
        self.fully_connected().into_iter().map(|p| p.addr).collect()
    }

    fn push(&self, peer: i64, msg: SyncMsg) {
        self.pushed.lock().unwrap_or_else(PoisonError::into_inner).push((peer, msg));
    }

    fn disconnect(&self, peer: i64) {
        panic!("peer {peer} disconnected");
    }

    fn requested_object_count(&self, _peer: i64) -> usize {
        0
    }

    fn is_active_masternode(&self) -> bool {
        false
    }

    /// Not dashd's draw: the object-request order is compared as sets (§6 m2).
    fn shuffle(&self, hashes: &mut [GovHash]) {
        hashes.reverse();
    }
}

struct Quiet;

impl NodeSyncNotifier for Quiet {
    fn sync_reset(&self) {}

    fn sync_finished(&self) {}
}

/// The seam, tweaks injected per object: each `vote_bloom_filter(hash)` is answered with the
/// next tweak dashd drew for that hash (§6 Random fields), so the bytes compare whatever the
/// order.
struct Injected {
    inner: NodeGovernanceSync<NoExec>,
    rng: SharedRng,
    tweaks: Mutex<BTreeMap<GovHash, VecDeque<u32>>>,
}

impl GovernanceSync for Injected {
    fn is_valid(&self) -> bool {
        self.inner.is_valid()
    }

    fn vote_count(&self) -> i32 {
        self.inner.vote_count()
    }

    fn enabled_masternode_count(&self) -> i32 {
        self.inner.enabled_masternode_count()
    }

    fn fetch_governance_object_votes(
        &self,
        peers_per_hash_max: usize,
        now: i64,
        asked: &mut dashd_masternode::sync_manager::AskedRecently,
    ) -> (Vec<GovHash>, Vec<GovHash>) {
        self.inner.fetch_governance_object_votes(peers_per_hash_max, now, asked)
    }

    fn vote_bloom_filter(&self, hash: &GovHash) -> FilterLoad {
        let next = self
            .tweaks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(hash)
            .and_then(VecDeque::pop_front);
        if let Some(t) = next {
            self.rng.push(t);
        }
        self.inner.vote_bloom_filter(hash)
    }
}

/// One recorded tick's outgoing sync messages to one peer.
#[derive(Debug, Default, PartialEq, Eq)]
struct PeerOut {
    /// `govsync(0, empty bloom)` and `mempool`, in order.
    plain: Vec<String>,
    /// Object requests: their hashes in order, each with its payload.
    objects: Vec<(GovHash, String)>,
}

fn group(msgs: &[(i64, SyncMsg)]) -> BTreeMap<i64, PeerOut> {
    let mut out: BTreeMap<i64, PeerOut> = BTreeMap::new();
    for (peer, msg) in msgs {
        let e = out.entry(*peer).or_default();
        match msg {
            SyncMsg::Mempool => e.plain.push("mempool".into()),
            SyncMsg::GovSync {
                hash,
                filter,
            } if *hash == [0; 32] => e.plain.push(hex(&govsync_payload(hash, filter))),
            SyncMsg::GovSync {
                hash,
                filter,
            } => e.objects.push((*hash, hex(&govsync_payload(hash, filter)))),
        }
    }
    out
}

fn parse_govsync(payload: &str) -> (i64, SyncMsg) {
    let b = unhex(payload);
    let hash: GovHash = b[..32].try_into().expect("a hash");
    let len = usize::from(b[32]);
    let data = b[33..33 + len].to_vec();
    let rest = &b[33 + len..];
    let funcs = u32::from_le_bytes(rest[..4].try_into().expect("4"));
    let tweak = u32::from_le_bytes(rest[4..8].try_into().expect("4"));
    (
        0,
        SyncMsg::GovSync {
            hash,
            filter: FilterLoad::new(data, funcs, tweak, rest[8]),
        },
    )
}

/// The R5 node: the recording's objects and votes as dashd accepted them, the seam over them,
/// and SP1's tick over the recording's peers.
struct R5 {
    clock: Arc<VirtualClock>,
    net: Arc<FakeNet>,
    sync: Arc<MasternodeSync>,
    tick: SyncManager,
    seam: Arc<Injected>,
    triggers: BTreeSet<GovHash>,
    _scratch: Scratch,
}

/// Every `govobj` and `govobjvote` delivered in `text`, votes grouped by parent in arrival order.
fn deliveries(text: &str) -> Vec<(GovObject, Vec<dash_primitives::GovVote>)> {
    let mut objects: Vec<(GovObject, Vec<dash_primitives::GovVote>)> = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        match f.as_slice() {
            [">", _, "govobj", h] => {
                let obj = GovObject::decode(&mut unhex(h).as_slice()).expect("an object");
                objects.push((obj, Vec::new()));
            }
            [">", _, "govobjvote", h] => {
                let v = vote::decode_network(&unhex(h)).expect("a vote");
                let parent = *v.parent_hash().as_bytes();
                let entry = objects
                    .iter_mut()
                    .find(|(o, _)| object::hash(o) == parent)
                    .expect("its object arrived first");
                entry.1.push(v);
            }
            _ => {}
        }
    }
    objects
}

/// The tweak dashd drew for each object, in order, from each `rand bloom` and the `govsync`
/// that follows it.
fn recorded_tweaks(text: &str) -> BTreeMap<GovHash, VecDeque<u32>> {
    let mut out: BTreeMap<GovHash, VecDeque<u32>> = BTreeMap::new();
    let mut pending = None;
    for line in text.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        match f.as_slice() {
            ["rand", "bloom", h] => {
                pending = Some(u32::from_le_bytes(unhex(h).try_into().expect("four bytes")));
            }
            ["<", _, "govsync", payload] => {
                let (
                    _,
                    SyncMsg::GovSync {
                        hash,
                        filter,
                    },
                ) = parse_govsync(payload)
                else {
                    unreachable!()
                };
                if let Some(t) = pending.take() {
                    assert_eq!(filter.tweak(), t, "the tweak is the filter's");
                    out.entry(hash).or_default().push_back(t);
                }
            }
            _ => {}
        }
    }
    assert!(pending.is_none());
    out
}

impl R5 {
    fn new(text: &str, enabled: u32) -> Self {
        let objects = deliveries(text);
        let triggers = objects
            .iter()
            .filter(|(o, _)| object::object_type(o) == object::TRIGGER)
            .map(|(o, _)| object::hash(o))
            .collect();
        let clock = VirtualClock::at(1_700_000_000);
        let net = Arc::new(FakeNet::default());
        let scratch = Scratch::new("r5");
        let p = params();
        let netfulfilled = Arc::new(NetFulfilledRequestManager::new(
            clock.clone(),
            i64::from(p.chain().fulfilled_request_expire_time),
        ));
        assert!(netfulfilled.load_cache(
            &scratch.0.join("netfulfilled.dat"),
            p.chain().message_start,
            false
        ));
        let sync = Arc::new(MasternodeSync::new(clock.clone(), Box::new(Quiet)));
        let rng = SharedRng::default();
        let inner = NodeGovernanceSync::new(
            handle_over(loaded(&objects)),
            Arc::clone(&sync),
            Arc::new(Tip(Arc::new(list_of(enabled)))),
            Box::new(rng.clone()),
            &p,
        );
        let seam = Arc::new(Injected {
            inner,
            rng,
            tweaks: Mutex::new(recorded_tweaks(text)),
        });
        let cfg = SyncConfig {
            mockable_chain: true,
            mainnet: false,
            sync_mempool: true,
        };
        let tick = SyncManager::new(
            clock.clone(),
            cfg,
            Arc::clone(&sync),
            netfulfilled,
            net.clone(),
            seam.clone(),
        );
        Self {
            clock,
            net,
            sync,
            tick,
            seam,
            triggers,
            _scratch: scratch,
        }
    }

    /// One recorded tick's messages against what the tick sent: per peer, the full syncs and
    /// `mempool` in order; the object requests as a trigger prefix and an other suffix, each
    /// compared as a set, every payload byte for byte (§6 m2).
    fn compare(&self, at: usize, want: &[(i64, SyncMsg)]) {
        let got = group(&self.net.take());
        let want = group(want);
        assert_eq!(got.keys().collect::<Vec<_>>(), want.keys().collect::<Vec<_>>(), "R5:{at}");
        let cap = 2000 / usize::try_from(self.seam.enabled_masternode_count()).expect("positive");
        for (peer, w) in &want {
            let g = &got[peer];
            assert_eq!(g.plain, w.plain, "R5:{at} peer {peer}");
            assert!(w.objects.len() < cap, "R5:{at}: under nMaxObjRequestsPerNode");
            for (side, objs) in [("got", &g.objects), ("dashd", &w.objects)] {
                let k = objs.iter().take_while(|(h, _)| self.triggers.contains(h)).count();
                assert!(
                    objs[k..].iter().all(|(h, _)| !self.triggers.contains(h)),
                    "R5:{at} peer {peer}: {side}'s triggers first"
                );
            }
            let split = |v: &[(GovHash, String)]| {
                let (t, o): (Vec<_>, Vec<_>) =
                    v.iter().cloned().partition(|(h, _)| self.triggers.contains(h));
                (t.into_iter().collect::<BTreeSet<_>>(), o.into_iter().collect::<BTreeSet<_>>())
            };
            assert_eq!(split(&g.objects), split(&w.objects), "R5:{at} peer {peer}");
        }
    }

    fn check_status(&self, at: usize, json: &str) {
        let key = |k: &str| -> String {
            let i = json.find(&format!("\"{k}\":")).expect(k) + k.len() + 3;
            json[i..].split([',', '}']).next().expect("a value").trim_matches('"').to_owned()
        };
        let s = &self.sync;
        let got = (
            s.asset_id().to_string(),
            s.asset_start_time().to_string(),
            s.attempt().to_string(),
            s.is_blockchain_synced().to_string(),
            s.is_synced().to_string(),
        );
        let want = (
            key("AssetID"),
            key("AssetStartTime"),
            key("Attempt"),
            key("IsBlockchainSynced"),
            key("IsSynced"),
        );
        assert_eq!(got, want, "R5:{at} mnsync status");
    }

    /// Replays `text`, every `mnsync status` checked; `bumps` adds sync bumps at a time (the
    /// effects of an admission then).
    fn replay(&self, text: &str, bumps: &[(i64, &'static str)]) {
        let mut want = Vec::new();
        let block = BlockRef {
            height: 180,
            hash: [180; 32],
        };
        self.tick.process_tick();
        for (i, line) in text.lines().enumerate() {
            let at = i + 1;
            let f: Vec<&str> = line.split(' ').collect();
            let input = matches!(
                f[0],
                "time" | "mockscheduler" | "mnsync" | "accept" | "connect" | "submitblock" | ">"
            );
            if input {
                self.compare(at, &std::mem::take(&mut want));
            }
            match f.as_slice() {
                ["time", t] => {
                    let t: i64 = t.parse().expect("a time");
                    self.clock.set_mock_time(t);
                    for (when, func) in bumps {
                        if *when == t {
                            self.seam.inner.apply_sync_bumps(&Effects {
                                sync_bumps: vec![*func],
                                ..Effects::default()
                            });
                        }
                    }
                    self.tick.process_tick();
                }
                ["mockscheduler", _] => self.tick.process_tick(),
                ["mnsync", "next"] => self.sync.switch_to_next_asset(),
                // Core: rpc/node.cpp:143-147
                ["mnsync", "reset"] => self.sync.reset(true, true),
                ["accept", n] => self.net.add(n.parse().expect("a peer"), true),
                ["connect", n, ..] => self.net.add(n.parse().expect("a peer"), false),
                ["submitblock", _] => {
                    self.sync.accepted_block_header(block);
                    self.sync.notify_header_tip(Some(block), false);
                    self.sync.updated_block_tip(Some(block), block, false);
                }
                ["<", n, "govsync", payload] => {
                    let (_, msg) = parse_govsync(payload);
                    want.push((n.parse().expect("a peer"), msg));
                }
                ["<", n, "mempool"] => want.push((n.parse().expect("a peer"), SyncMsg::Mempool)),
                ["check", "rpc", ..] if line.contains(r#""params":["status"]"#) => {
                    self.check_status(at, line);
                }
                _ => {}
            }
        }
        self.compare(text.lines().count(), &want);
    }

    /// When the sync finished, if it did.
    fn finished(&self) -> Option<i64> {
        self.sync.is_synced().then(|| self.sync.asset_start_time())
    }
}

fn r5() -> String {
    fixture("tests/fixtures/dashd-v24.0.0/R5.txt")
}

/// The six masternodes of the recording (four regular, two evonodes): each voted in R5.
fn r5_enabled(text: &str) -> u32 {
    let voters: BTreeSet<_> = deliveries(text)
        .iter()
        .flat_map(|(_, v)| v.iter().map(|v| v.masternode_outpoint()))
        .collect();
    u32::try_from(voters.len()).expect("a few")
}

/// R5: SP1's tick over the seam sends dashd's per-object vote requests, triggers first, each
/// bloom byte for byte with dashd's recorded tweak; the recorded enabled count keeps every
/// tick under `nMaxObjRequestsPerNode`; GOVERNANCE finishes when dashd's did. Every recorded
/// tweak is drawn. dashd never restarted, so the vote index is empty here; that is harmless:
/// FINISHED reads the count's change per tick against `max(int(0.0001 * last), 6)`, which is 6
/// for both 0 and 29 votes, and R5 admits nothing during GOVERNANCE.
// Core: node/sync_manager.cpp:51-136, :299-311; governance/governance.cpp:947-993
#[test]
fn r5_vote_requests_blooms_and_finished_time_equal_dashd() {
    let text = r5();
    let enabled = r5_enabled(&text);
    assert_eq!(enabled, 6);
    let node = R5::new(&text, enabled);
    assert_eq!(node.seam.enabled_masternode_count(), 6);
    assert_eq!(2000 / 6, 333);
    node.replay(&text, &[]);
    assert_eq!(node.finished(), Some(1_700_002_863));
    assert!(node.seam.tweaks.lock().unwrap().values().all(VecDeque::is_empty));
    assert_eq!(node.seam.rng.remaining(), 0, "every tweak drawn");
}

/// An unknown object gets Core's empty filter, and no tweak is drawn for it.
// Core: governance/governance.cpp:951-957
#[test]
fn an_unknown_object_draws_no_tweak() {
    let text = r5();
    let node = R5::new(&text, 6);
    node.seam.rng.push(9);
    assert_eq!(
        node.seam.inner.vote_bloom_filter(&[0xee; 32]),
        FilterLoad::new(Vec::new(), 0, 0, 0)
    );
    assert_eq!(node.seam.rng.remaining(), 1, "no tweak drawn");
}

/// After a restart, R5's vote count is the index rebuilt from the vote files: the 29 delivered
/// votes, though the trigger is marked for deletion at height 0. Each vote is indexed to its own
/// object: it is held, and erasing one object drops exactly that object's votes. Each object's
/// filter holds each of its votes.
// Core: governance/governance.cpp:194-206, :458-468, :947-964, :995-1027
#[test]
fn r5_vote_count_and_filters_hold_the_votes() {
    let text = r5();
    let objects = deliveries(&text);
    let m = restarted(&objects);
    let marked: Vec<_> = objects
        .iter()
        .map(|(o, _)| object::hash(o))
        .filter(|h| m.object(h).expect("held").cached_delete)
        .collect();
    assert_eq!(marked.len(), 1);
    assert!(marked.iter().all(|h| triggers_of(&objects).contains(h)));
    assert!(objects.iter().flat_map(|(_, v)| v).all(|v| m.have_vote(&vote::hash(v))));
    let (voted, votes) = objects.iter().find(|(_, v)| !v.is_empty()).expect("a voted object");
    let mut erased = m.clone();
    erased.erase_object(object::hash(voted), 1_700_000_000, &params());
    assert_eq!(erased.vote_index_len(), 29 - votes.len());
    let sync = Arc::new(MasternodeSync::new(VirtualClock::at(1_700_000_000), Box::new(Quiet)));
    let rng = SharedRng::default();
    let seam = NodeGovernanceSync::new(
        handle_over(m),
        sync,
        Arc::new(Tip(Arc::new(list_of(6)))),
        Box::new(rng.clone()),
        &params(),
    );
    assert_eq!(seam.vote_count(), 29);
    for (obj, votes) in &objects {
        let hash = object::hash(obj);
        rng.push(7);
        let f = dashd_p2p::processing::bloom::BloomFilter::from(seam.vote_bloom_filter(&hash));
        assert!(votes.iter().all(|v| f.contains(&vote::hash(v))), "{}", hex(&hash));
    }
}

fn triggers_of(objects: &[(GovObject, Vec<dash_primitives::GovVote>)]) -> BTreeSet<GovHash> {
    objects
        .iter()
        .filter(|(o, _)| object::object_type(o) == object::TRIGGER)
        .map(|(o, _)| object::hash(o))
        .collect()
}

const SYNTHETIC_START: i64 = 1_700_000_000;

/// A loaded manager holding one proposal, its votes from `testkit::numbered` masternodes 1..=6
/// admitted through `process_vote` an hour before [`SYNTHETIC_START`].
fn synthetic() -> (Manager, GovObject, MnList) {
    let mut b = vec![0u8; 32];
    b.extend(1i32.to_le_bytes());
    b.extend((SYNTHETIC_START - 7200).to_le_bytes());
    b.extend([0x5a; 32]);
    let data = br#"{"type": 1, "name": "S3c"}"#;
    b.push(u8::try_from(data.len()).expect("short"));
    b.extend_from_slice(data);
    b.extend(object::PROPOSAL.to_le_bytes());
    b.extend([0; 32]);
    b.extend(u32::MAX.to_le_bytes());
    b.push(0);
    let obj = GovObject::decode(&mut b.as_slice()).expect("an object");
    let mut m = Manager::new();
    let mut state = StoreState::default();
    let hash = object::hash(&obj);
    state.objects.insert(hash, (obj.clone(), ObjectState::new(object::PROPOSAL, hash)));
    m.import_state(state);
    let earlier = SYNTHETIC_START - 3600;
    m.init_on_load(&GovClock::new(earlier, earlier), &params());
    let list = list_of(7);
    for n in 1..=6 {
        admit(&mut m, &obj, &list, n, earlier);
    }
    (m, obj, list)
}

/// `testkit::numbered(n)`'s operator-signed VALID vote on `obj` at `time`, admitted then.
fn admit(m: &mut Manager, obj: &GovObject, list: &MnList, n: u32, time: i64) {
    use dash_pkc::bls::{BlsScIetf, BlsSecretKey, BlsSigId};
    use dash_primitives::{GovVote, TxHash, VoteOutcome, VoteSignal};
    let make = |sig| {
        GovVote::new(
            testkit::numbered(n).collateral,
            TxHash::from_lendian(object::hash(obj)),
            VoteOutcome::new(vote::OUTCOME_YES as u32),
            VoteSignal::new(vote::SIGNAL_VALID as u32),
            time,
            sig,
        )
    };
    let key = BlsSecretKey::<BlsScIetf>::from_ikm(&testkit::numbered_operator_ikm(n)).expect("key");
    let unsigned = make(Vec::new());
    let v =
        make(key.sign_with(&vote::signature_hash(&unsigned), BlsSigId::Basic).to_bytes().to_vec());
    assert_eq!(m.process_vote(&v, list, time, &params()).result, Ok(true), "vote {n} at {time}");
}

/// F1: re-votes admitted during GOVERNANCE grow Core's count (the vote index) while the vote
/// files stay the same size, so the tick that would have finished sees six new votes and waits
/// one more. GOVERNANCE starts at +0; the first object ask at +6 starts the no-objects wait; a
/// seventh masternode's first vote at +21 bumps the timeout past +36; the six re-votes at +39
/// bump it again. At +42 the wait is over (36 > 30) but 6 votes came in the last tick, not fewer
/// than `max(int(0.0001 * 7), 6)`; at +48 none did, and the sync finishes.
// Core: node/sync_manager.cpp:247-256, :289-311; governance/votedb.cpp:103-118
#[test]
fn a_revote_during_governance_holds_back_finished() {
    let (m, obj, list) = synthetic();
    let clock = VirtualClock::at(SYNTHETIC_START);
    let sync = Arc::new(MasternodeSync::new(clock.clone(), Box::new(Quiet)));
    let scratch = Scratch::new("revote");
    let p = params();
    let netfulfilled = Arc::new(NetFulfilledRequestManager::new(
        clock.clone(),
        i64::from(p.chain().fulfilled_request_expire_time),
    ));
    assert!(netfulfilled.load_cache(
        &scratch.0.join("netfulfilled.dat"),
        p.chain().message_start,
        false
    ));
    let rng = SharedRng::default();
    for _ in 0..10 {
        rng.push(0);
    }
    let seam = Arc::new(NodeGovernanceSync::new(
        handle_over(m),
        Arc::clone(&sync),
        Arc::new(Tip(Arc::new(list.clone()))),
        Box::new(rng),
        &p,
    ));
    let net = Arc::new(FakeNet::default());
    net.add(1, false);
    let cfg = SyncConfig {
        mockable_chain: true,
        mainnet: false,
        sync_mempool: false,
    };
    let tick =
        SyncManager::new(clock.clone(), cfg, Arc::clone(&sync), netfulfilled, net, seam.clone());
    let handle = &seam.live.as_ref().expect("live").handle;
    let file_sum = || -> usize { handle.lock().objects().values().map(|s| s.file.len()).sum() };
    let vote_bump = Effects {
        sync_bumps: vec!["MNGOVERNANCEOBJECTVOTE"],
        ..Effects::default()
    };
    sync.switch_to_next_asset();
    tick.process_tick();
    let mut finished = None;
    for step in 1..=10 {
        let at = SYNTHETIC_START + 6 * step;
        if at == SYNTHETIC_START + 24 {
            clock.set_mock_time(SYNTHETIC_START + 21);
            admit(&mut handle.lock(), &obj, &list, 7, SYNTHETIC_START + 21);
            seam.apply_sync_bumps(&vote_bump);
        }
        if at == SYNTHETIC_START + 42 {
            clock.set_mock_time(SYNTHETIC_START + 39);
            for n in 1..=6 {
                admit(&mut handle.lock(), &obj, &list, n, SYNTHETIC_START + 39);
                seam.apply_sync_bumps(&vote_bump);
            }
            assert_eq!((seam.vote_count(), file_sum()), (13, 7));
        }
        clock.set_mock_time(at);
        tick.process_tick();
        if sync.is_synced() {
            finished = Some(at - SYNTHETIC_START);
            break;
        }
    }
    assert_eq!(finished, Some(48));
}

/// FINISHED is driven by the sync bumps: R5 recorded no admission during GOVERNANCE, so its
/// FINISHED (+36 after GOVERNANCE started) is the plain 30 s timeout on the 6 s tick grid. A vote
/// accepted at +13 postpones that timeout past +43, so the tick at +42 ends GOVERNANCE the other
/// way: every object asked since +6, more than 30 s ago, with no new votes.
// Core: masternode/sync.cpp:46-51; node/sync_manager.cpp:247-256, :289-311
#[test]
fn sync_bumps_drive_the_finished_time() {
    let text = r5();
    let node = R5::new(&text, 6);
    let edited: String = text
        .lines()
        .take_while(|l| !l.starts_with("time 1700002863"))
        .chain(["time 1700002863", "mockscheduler 1", "time 1700002869", "mockscheduler 1"])
        .map(|l| format!("{l}\n"))
        .collect();
    node.replay(&edited, &[(1_700_002_840, "MNGOVERNANCEOBJECTVOTE")]);
    assert_eq!(node.finished(), Some(1_700_002_869));
    let control = R5::new(&text, 6);
    control.replay(&edited, &[]);
    assert_eq!(control.finished(), Some(1_700_002_863));
}

/// With no seam at all, the GOVERNANCE asset is skipped at once (§2 case 3): `disabled` is
/// invalid, holds nothing and is never ready.
// Core: governance/net_governance.cpp:280; governance/governance.cpp:629; node/sync_manager.cpp:240-243
#[test]
fn disabled_governance_is_invalid_and_empty() {
    let clock = VirtualClock::at(1_700_000_000);
    let sync = Arc::new(MasternodeSync::new(clock, Box::new(Quiet)));
    sync.switch_to_next_asset();
    let seam = NodeGovernanceSync::<NoExec>::disabled(Arc::clone(&sync));
    assert!(sync.is_blockchain_synced());
    assert!(!seam.is_valid() && !seam.governance_ready());
    assert_eq!((seam.vote_count(), seam.enabled_masternode_count()), (0, 0));
    let mut asked = BTreeMap::new();
    assert_eq!(seam.fetch_governance_object_votes(1, 0, &mut asked), (vec![], vec![]));
    assert_eq!(seam.vote_bloom_filter(&[1; 32]), FilterLoad::new(Vec::new(), 0, 0, 0));
}

/// `governance_ready` is `IsValid() && IsBlockchainSynced()`.
// Core: governance/net_governance.cpp:280; governance/governance.cpp:629
#[test]
fn governance_ready_needs_a_loaded_cache_and_the_blockchain() {
    let clock = VirtualClock::at(1_700_000_000);
    let sync = Arc::new(MasternodeSync::new(clock, Box::new(Quiet)));
    let seam = |m: Manager| {
        NodeGovernanceSync::new(
            handle_over(m),
            Arc::clone(&sync),
            Arc::new(Tip(Arc::new(list_of(0)))),
            Box::new(ReplayRng::default()),
            &params(),
        )
    };
    let mut on = Manager::new();
    on.set_loaded(true);
    let (on, off) = (seam(on), seam(Manager::new()));
    assert!(on.is_valid() && !off.is_valid());
    assert!(!on.governance_ready());
    sync.switch_to_next_asset();
    assert!(on.governance_ready() && !off.governance_ready());
}

/// `FetchGovernanceObjectVotes` drops a visited object's expired asks, and skips an object
/// asked of `nPeersPerHashMax` live peers.
// Core: governance/governance.cpp:969-993
#[test]
fn fetch_drops_expired_asks_and_skips_fully_asked_objects() {
    let text = r5();
    let objects = deliveries(&text);
    let hashes: Vec<GovHash> = objects.iter().map(|(o, _)| object::hash(o)).collect();
    let node = R5::new(&text, 6);
    let peer = |n: u8| Service::new(NetAddr::ipv4([10, 0, 0, n]), 1);
    let mut asked = BTreeMap::new();
    asked.insert(hashes[0], BTreeMap::from([(peer(1), 99), (peer(2), 100)]));
    asked.insert(hashes[2], BTreeMap::from([(peer(1), 100), (peer(2), 100)]));
    let (t, o) = node.seam.fetch_governance_object_votes(2, 100, &mut asked);
    let all: BTreeSet<_> = t.iter().chain(&o).copied().collect();
    assert!(all.contains(&hashes[0]) && !all.contains(&hashes[2]));
    assert_eq!(all.len(), hashes.len() - 1);
    assert_eq!(asked[&hashes[0]].len(), 1, "the expired ask is dropped");
    assert!(t.iter().all(|h| node.triggers.contains(h)));
    assert!(o.iter().all(|h| !node.triggers.contains(h)));
}

/// SP1's `mnsync-governance` with the real seam over an empty loaded cache, in the processor's
/// sim: dashd's `govsync`s, `mnsync status` and fulfilled requests.
#[test]
fn sp1_mnsync_governance_passes_with_the_real_seam() {
    sp1::run(true);
}

/// Negative control: the disabled seam skips GOVERNANCE, so dashd's first `govsync` is missing.
#[test]
#[should_panic(expected = "mnsync-governance:119 `time 1700000042`: peer 1")]
fn sp1_mnsync_governance_fails_with_the_disabled_seam() {
    sp1::run(false);
}

mod sp1;
