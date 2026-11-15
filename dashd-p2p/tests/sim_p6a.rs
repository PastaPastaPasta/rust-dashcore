//! G6's built-in overlay `getdata` serving and ordering checks.

use std::sync::{Arc, Mutex};

use dash_consensus::Tx;
use dash_num::Hash256;
use dash_p2p::conn::Permissions;
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, ConnectionType};
use dash_p2p_core::{DsQueue, GetData, InvType, Inventory};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{Transaction, TxType};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::bloom::BloomFilter;
use dashd_p2p::processing::registry::{HandlerCx, NetHandler};
use dashd_p2p::processing::relay::Related;
use dashd_p2p::processing::seams::{CoinJoinObjects, IsLockLookup, PlatformBans};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer};
use dashd_p2p::{ProcessorOptions, Registry, Seams};

fn encode<T: BaseCodec>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

fn inv(ty: InvType, hash: [u8; 32]) -> Inventory {
    Inventory::new(ty, Hash256::from_lendian(hash))
}

fn sim(seams: Seams, registry: Registry) -> Sim {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&params, &IdentityOptions::new()).unwrap();
    Sim::with(params, id, ProcessorOptions::default(), registry, seams, |_| {})
}

fn peer(sim: &mut Sim) -> SimPeer {
    let p = sim.connect(1, ConnectionType::Inbound, Permissions::NONE);
    sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
    sim.sent(&p);
    p
}

fn request(sim: &mut Sim, peer: &SimPeer, object: Inventory) -> Vec<(CommandString, Vec<u8>)> {
    sim.deliver(peer, &[("getdata", encode(&GetData::new(vec![object])))]);
    while sim.pass() {}
    sim.sent_at_least(peer, 1)
}

fn settle(sim: &mut Sim) {
    while sim.pass() {}
    sim.pass();
}

#[derive(Clone)]
struct Objects {
    dsq: Option<([u8; 32], Vec<u8>)>,
    lookups: Arc<Mutex<usize>>,
}

impl CoinJoinObjects for Objects {
    fn dstx_by_hash(&self, _: &dash_primitives::TxHash) -> Option<Vec<u8>> {
        None
    }

    fn queue_by_hash(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        *self.lookups.lock().unwrap() += 1;
        self.dsq.as_ref().and_then(|(known, bytes)| (known == hash).then(|| bytes.clone()))
    }
}

#[derive(Clone)]
struct Locks(Option<([u8; 32], Vec<u8>)>);

impl IsLockLookup for Locks {
    fn islock_by_txid(&self, _: &dash_primitives::TxHash) -> Option<Vec<u8>> {
        None
    }

    fn islock_hash_by_txid(&self, _: &dash_primitives::TxHash) -> Option<[u8; 32]> {
        None
    }

    fn islock_by_hash(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        self.0.as_ref().and_then(|(known, bytes)| (known == hash).then(|| bytes.clone()))
    }
}

#[derive(Clone)]
struct Bans {
    ban: Option<([u8; 32], Vec<u8>)>,
    lookups: Arc<Mutex<usize>>,
}

impl PlatformBans for Bans {
    fn get(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        *self.lookups.lock().unwrap() += 1;
        self.ban.as_ref().and_then(|(known, bytes)| (known == hash).then(|| bytes.clone()))
    }
}

struct RegistrySpy(Arc<Mutex<bool>>);

impl NetHandler for RegistrySpy {
    fn commands(&self) -> &'static [CommandString] {
        static COMMANDS: [CommandString; 1] = [CommandString::DSA];
        &COMMANDS
    }

    fn process_message(
        &mut self,
        _: &mut HandlerCx<'_>,
        _: &CommandString,
        _: &[u8],
    ) -> Result<(), Throw> {
        Ok(())
    }

    fn process_getdata(&mut self, _: &mut HandlerCx<'_>, _: &Inventory) -> bool {
        *self.0.lock().unwrap() = true;
        true
    }
}

#[test]
fn overlay_getdata_serves_matches_and_notfound_for_missing_objects() {
    let hashes = ([1; 32], [2; 32], [3; 32]);
    let bytes = ([11, 12], [21, 22], [31, 32]);
    let dsq_lookups = Arc::new(Mutex::new(0));
    let ban_lookups = Arc::new(Mutex::new(0));
    let mut sim = sim(
        Seams {
            islocks: Box::new(Locks(Some((hashes.0, bytes.0.to_vec())))),
            coinjoin: Box::new(Objects {
                dsq: Some((hashes.1, bytes.1.to_vec())),
                lookups: Arc::clone(&dsq_lookups),
            }),
            platform_bans: Box::new(Bans {
                ban: Some((hashes.2, bytes.2.to_vec())),
                lookups: Arc::clone(&ban_lookups),
            }),
            ..Seams::default()
        },
        Registry::new(),
    );
    let p = peer(&mut sim);
    for (object, command, payload) in [
        (inv(InvType::IsdLock, hashes.0), CommandString::ISDLOCK, bytes.0.to_vec()),
        (inv(InvType::Dsq, hashes.1), CommandString::DSQ, bytes.1.to_vec()),
        (inv(InvType::PlatformBan, hashes.2), CommandString::PLATFORMBAN, bytes.2.to_vec()),
    ] {
        assert_eq!(request(&mut sim, &p, object), [(command, payload)]);
    }
    assert_eq!(*dsq_lookups.lock().unwrap(), 1);
    assert_eq!(*ban_lookups.lock().unwrap(), 1);
    // Core: net_processing.cpp:3046-3079. Missing built-in objects reach `notfound`.
    for missing in [
        inv(InvType::IsdLock, [9; 32]),
        inv(InvType::Dsq, [10; 32]),
        inv(InvType::PlatformBan, [11; 32]),
    ] {
        let out = request(&mut sim, &p, missing);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, CommandString::NOTFOUND);
    }
    assert_eq!(*dsq_lookups.lock().unwrap(), 2);
    assert_eq!(*ban_lookups.lock().unwrap(), 2);
}

#[test]
fn built_in_overlay_getdata_runs_before_registry_handlers() {
    let called = Arc::new(Mutex::new(false));
    let mut registry = Registry::new();
    registry.register(Box::new(RegistrySpy(Arc::clone(&called))));
    let hash = [7; 32];
    let mut sim = sim(
        Seams {
            coinjoin: Box::new(Objects {
                dsq: Some((hash, vec![4, 5, 6])),
                lookups: Arc::new(Mutex::new(0)),
            }),
            ..Seams::default()
        },
        registry,
    );
    let p = peer(&mut sim);
    assert_eq!(
        request(&mut sim, &p, inv(InvType::Dsq, hash)),
        [(CommandString::DSQ, vec![4, 5, 6])]
    );
    assert!(!*called.lock().unwrap());
}

#[test]
fn dsq_relay_uses_inventory_or_full_push_by_peer_mode() {
    let mut sim = sim(Seams::default(), Registry::new());
    let inv_peer = peer(&mut sim);
    let full_peer = sim.connect(2, ConnectionType::Inbound, Permissions::NONE);
    sim.handshake(
        &full_peer,
        PeerVersionMsg {
            version: 70_233,
            ..PeerVersionMsg::v24(SIM_START)
        },
    );
    sim.sent(&full_peer);
    sim.admitted(&inv_peer, "senddsq", vec![1]).unwrap();
    sim.admitted(&full_peer, "senddsq", vec![1]).unwrap();
    let queue = DsQueue::new(1, Hash256::from_lendian([4; 32]).into(), SIM_START, false, vec![7]);
    sim.shared.relay_dsq(&queue);
    settle(&mut sim);
    let inv_frames = sim.sent(&inv_peer);
    assert!(inv_frames.iter().any(|(cmd, _)| *cmd == CommandString::INV));
    let full_frames = sim.sent(&full_peer);
    assert!(
        full_frames
            .iter()
            .any(|(cmd, payload)| { *cmd == CommandString::DSQ && payload == &encode(&queue) })
    );
}

#[test]
fn filtered_isdlock_relay_uses_the_related_hash_filter() {
    // Core: net_processing.cpp:2588-2648, including PeerReconstructsISLockFromRecsig.
    let mut sim = sim(Seams::default(), Registry::new());
    let target = [8; 32];
    let tx = Tx::new(Transaction::new(1, TxType::Spend, vec![], vec![], 0, vec![]));
    let tx_inventory = inv(InvType::IsdLock, [9; 32]);
    let skip = peer(&mut sim);
    let no_recsigs = peer(&mut sim);
    let zero_proreg = peer(&mut sim);
    let unmatched = peer(&mut sim);
    assert!(sim.core.try_mark_verified(skip.handle.id(), [1; 32], [2; 32]));
    assert!(sim.core.try_mark_verified(no_recsigs.handle.id(), [3; 32], [4; 32]));
    sim.admitted(&skip, "qsendrecsigs", vec![1]).unwrap();
    sim.admitted(&zero_proreg, "qsendrecsigs", vec![1]).unwrap();
    let mut filter = BloomFilter::new(10, 0.000_001, 0, 1);
    filter.insert(&target);
    filter.insert(tx.txid().as_bytes());
    for matched in [&skip, &no_recsigs, &zero_proreg] {
        sim.deliver(matched, &[("filterload", encode(&filter.to_msg()))]);
    }
    sim.deliver(
        &unmatched,
        &[("filterload", encode(&BloomFilter::new(10, 0.000_001, 0, 1).to_msg()))],
    );
    settle(&mut sim);
    for matched in [&skip, &no_recsigs, &zero_proreg, &unmatched] {
        sim.sent(matched);
    }
    let inventory = inv(InvType::IsdLock, target);
    sim.shared.relay_inv_filtered(&inventory, Related::Hash(target.into()));
    settle(&mut sim);
    assert!(sim.sent(&skip).is_empty());
    assert_eq!(sim.sent(&no_recsigs).len(), 1);
    assert_eq!(sim.sent(&zero_proreg).len(), 1);
    assert!(sim.sent(&unmatched).is_empty());

    // Core: net_processing.cpp:2588-2648. Related::Tx updates and tests the bloom filter.
    sim.shared.relay_inv_filtered(&tx_inventory, Related::Tx(&tx));
    settle(&mut sim);
    assert!(sim.sent(&skip).is_empty());
    assert_eq!(sim.sent(&no_recsigs).len(), 1);
    assert_eq!(sim.sent(&zero_proreg).len(), 1);
    assert!(sim.sent(&unmatched).is_empty());
}
