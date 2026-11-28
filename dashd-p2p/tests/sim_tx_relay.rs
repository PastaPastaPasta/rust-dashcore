//! Transaction relay, privacy and request paths through the simulated message actor.

use std::sync::Arc;
use std::time::Duration;

use dash_consensus::{BlsCtx, CoreSerialize, Tx, TxRef};
use dash_num::Hash256;
use dash_p2p::Why;
use dash_p2p::{CommandString, ConnectionType, PeerId, Permissions};
use dash_p2p_core::{ClSig, GetData, Inv, InvType, Inventory, NotFound};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::bloom::{BLOOM_UPDATE_ALL, BloomFilter};
use dashd_p2p::processing::seams::{
    ChainLocks, CoinJoinObjects, IsLockLookup, Read, TxInfo, TxPool,
};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer, SimTxPool};
use dashd_p2p::{MessageProcessingResult, ProcessorOptions, Registry, Seams};

mod log_capture;

fn sim(pool: &SimTxPool, extra: impl FnOnce(&mut Seams)) -> Sim {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&params, &IdentityOptions::new()).unwrap();
    let mut seams = Seams {
        txpool: Arc::new(pool.clone()),
        ..Seams::default()
    };
    extra(&mut seams);
    let sim = Sim::with(params, id, ProcessorOptions::default(), Registry::new(), seams, |_| {});
    sim.chain.set_ibd(false);
    sim
}

fn tx(tag: u32, parents: &[TxHash]) -> TxRef {
    let inputs =
        parents.iter().map(|p| TxIn::new(OutPoint::new(*p, 0), vec![].into(), u32::MAX)).collect();
    Arc::new(Tx::new(Transaction::new(1, TxType::Spend, inputs, vec![], tag, vec![])))
}

fn unlock(signature: u8) -> TxRef {
    let mut payload = vec![2];
    payload.extend_from_slice(&7u64.to_le_bytes());
    payload.extend_from_slice(&1000u32.to_le_bytes());
    payload.extend_from_slice(&100u32.to_le_bytes());
    payload.extend_from_slice(&[0xab; 32]);
    payload.extend_from_slice(&[signature; 96]);
    Arc::new(Tx::new(Transaction::new(3, TxType::new(9), vec![], vec![], 0, payload)))
}

fn insert(pool: &SimTxPool, tx: &TxRef, time_s: i64, ancestors: u64, fee: i64) {
    pool.insert(
        TxInfo {
            tx: tx.clone(),
            time_s,
        },
        ancestors,
        fee,
    );
}

fn inv(kind: InvType, hash: TxHash) -> Inventory {
    Inventory::new(kind, Hash256::from_lendian(hash.to_lendian()))
}

fn encode(value: &impl BaseCodec) -> Vec<u8> {
    let mut bytes = Vec::new();
    value.encode(&mut bytes);
    bytes
}

fn peer(
    sim: &mut Sim,
    n: u8,
    kind: ConnectionType,
    permissions: Permissions,
    version: i32,
) -> SimPeer {
    let peer = sim.connect(n, kind, permissions);
    sim.handshake(
        &peer,
        PeerVersionMsg {
            version,
            ..PeerVersionMsg::v24(SIM_START)
        },
    );
    assert!(peer.handle.successfully_connected());
    peer
}

fn invs(sim: &Sim, peer: &SimPeer) -> Vec<Inventory> {
    sim.sent(peer)
        .into_iter()
        .filter(|(cmd, _)| *cmd == CommandString::INV)
        .flat_map(|(_, bytes)| Inv::decode(&mut &bytes[..]).unwrap().inventory().clone())
        .collect()
}

fn request(sim: &mut Sim, peer: &SimPeer, request: Inventory) -> Vec<(CommandString, Vec<u8>)> {
    sim.admitted(peer, "getdata", encode(&GetData::new(vec![request]))).unwrap();
    sim.sent_at_least(peer, 1)
}

#[test]
fn shared_relay_transaction_queues_to_a_relaying_peer() {
    let pool = SimTxPool::default();
    let a = tx(1, &[]);
    insert(&pool, &a, SIM_START, 1, 1);
    let mut sim = sim(&pool, |_| {});
    let p = peer(&mut sim, 1, ConnectionType::Inbound, Permissions::NO_BAN, 70242);

    sim.shared.relay_transaction(&a.txid());
    sim.pass();

    assert_eq!(invs(&sim, &p), [inv(InvType::Tx, a.txid())]);
}

#[test]
fn no_transaction_queue_during_handshake_or_before_first_trickle_timer() {
    let pool = SimTxPool::default();
    let a = tx(1, &[]);
    insert(&pool, &a, SIM_START, 1, 1);
    let mut sim = sim(&pool, |_| {});
    let p = sim.connect(1, ConnectionType::Inbound, Permissions::NONE);
    let handle = sim.shared.handle();
    handle.relay_transaction(a.txid());
    sim.lp.drain_transaction_commands();
    sim.deliver(&p, &[("version", PeerVersionMsg::v24(SIM_START).payload())]);
    sim.pass();
    handle.relay_transaction(a.txid());
    sim.lp.drain_transaction_commands();
    sim.deliver(&p, &[("verack", vec![])]);
    sim.pass();
    assert!(invs(&sim, &p).is_empty());
    handle.relay_transaction(a.txid());
    sim.pass();
    assert!(invs(&sim, &p).is_empty());
    sim.shared.with_state_mut(p.handle.id(), |st| st.next_inv_send_us = 0);
    sim.pass();
    assert_eq!(invs(&sim, &p), [inv(InvType::Tx, a.txid())]);
}

/// The first ten candidates in Core's order are already known to the peer: they leave the queue
/// unannounced, and the cap of 280 counts only the announcements sent (np:6694-6741).
#[test]
fn trickle_orders_by_ancestors_then_fee_and_caps_successful_announcements() {
    const SKIPPED: usize = 10;
    let pool = SimTxPool::default();
    let txs: Vec<_> = (1..=305).map(|tag| tx(tag, &[])).collect();
    for (index, tx) in txs.iter().enumerate() {
        insert(&pool, tx, SIM_START, 1 + (index % 3) as u64, index as i64);
    }
    let order: Vec<_> = pool.info_all().iter().map(|info| info.tx.txid()).collect();
    let mut sim = sim(&pool, |_| {});
    let inbound = peer(&mut sim, 1, ConnectionType::Inbound, Permissions::NONE, 70242);
    let outbound = peer(&mut sim, 2, ConnectionType::OutboundFullRelay, Permissions::NONE, 70242);
    for tx in &txs {
        sim.shared.handle().relay_transaction(tx.txid());
    }
    sim.lp.drain_transaction_commands();
    for p in [&inbound, &outbound] {
        sim.shared.with_state_mut(p.handle.id(), |st| {
            let mut relay = st.inventory_relay().lock();
            assert_eq!(relay.tx_inventory_to_send.len(), 305);
            for txid in &order[..SKIPPED] {
                relay.add_known(&txid.to_lendian());
            }
            drop(relay);
            st.next_inv_send_us = 0;
        });
        sim.lp.logic_mut().send_messages(&p.handle);
        let got = invs(&sim, p);
        let want: Vec<_> =
            order[SKIPPED..SKIPPED + 280].iter().map(|txid| inv(InvType::Tx, *txid)).collect();
        assert_eq!(got, want);
        sim.shared.with_state_mut(p.handle.id(), |st| st.next_inv_send_us = 0);
        sim.lp.logic_mut().send_messages(&p.handle);
        let want: Vec<_> =
            order[SKIPPED + 280..].iter().map(|txid| inv(InvType::Tx, *txid)).collect();
        assert_eq!(invs(&sim, p), want);
    }
}

#[test]
fn getdata_rejects_unannounced_until_two_minutes_and_makes_recent_parents_requestable() {
    let pool = SimTxPool::default();
    let parent = tx(1, &[]);
    let child = tx(2, &[parent.txid()]);
    for tx in [&parent, &child] {
        insert(&pool, tx, SIM_START, 1, 1);
    }
    pool.add_unbroadcast(child.txid());
    let mut sim = sim(&pool, |_| {});
    let p = peer(&mut sim, 1, ConnectionType::Inbound, Permissions::NONE, 70242);
    let req = inv(InvType::Tx, child.txid());
    assert_eq!(
        request(&mut sim, &p, req),
        [(CommandString::NOTFOUND, encode(&NotFound::new(vec![req])))]
    );
    sim.clock.advance(Duration::from_secs(119));
    assert_eq!(request(&mut sim, &p, req)[0].0, CommandString::NOTFOUND);
    sim.shared.handle().relay_transaction(child.txid());
    sim.lp.drain_transaction_commands();
    sim.shared.with_state_mut(p.handle.id(), |st| st.next_inv_send_us = 0);
    sim.lp.logic_mut().send_messages(&p.handle);
    assert_eq!(invs(&sim, &p), [req]);
    assert_eq!(request(&mut sim, &p, req), [(CommandString::TX, child.core_serialize())]);
    assert!(pool.unbroadcast().is_empty());
    assert_eq!(pool.unbroadcast_removals(), [(child.txid(), false)]);
    assert_eq!(
        request(&mut sim, &p, inv(InvType::Tx, parent.txid())),
        [(CommandString::TX, parent.core_serialize())]
    );
    let q = peer(&mut sim, 2, ConnectionType::Inbound, Permissions::NONE, 70242);
    assert_eq!(request(&mut sim, &q, req)[0].0, CommandString::NOTFOUND);
    sim.clock.advance(Duration::from_secs(1));
    assert_eq!(request(&mut sim, &q, req), [(CommandString::TX, child.core_serialize())]);
}

#[test]
fn relay_cache_survives_pool_removal_and_expires_only_on_a_later_announcement() {
    let pool = SimTxPool::default();
    let a = tx(1, &[]);
    let b = tx(2, &[]);
    insert(&pool, &a, SIM_START, 1, 1);
    let mut sim = sim(&pool, |_| {});
    let p = peer(&mut sim, 1, ConnectionType::Inbound, Permissions::NO_BAN, 70242);
    sim.shared.handle().relay_transaction(a.txid());
    sim.pass();
    assert_eq!(invs(&sim, &p), [inv(InvType::Tx, a.txid())]);
    pool.remove(&a.txid());
    let req = inv(InvType::Tx, a.txid());
    assert_eq!(request(&mut sim, &p, req), [(CommandString::TX, a.core_serialize())]);
    sim.clock.advance(Duration::from_secs(900));
    insert(&pool, &b, SIM_START + 900, 1, 1);
    sim.shared.handle().relay_transaction(b.txid());
    sim.pass();
    invs(&sim, &p);
    assert_eq!(request(&mut sim, &p, req)[0].0, CommandString::TX, "strict expiry boundary");
    let c = tx(3, &[]);
    insert(&pool, &c, SIM_START + 901, 1, 1);
    sim.clock.advance(Duration::from_secs(1));
    sim.shared.handle().relay_transaction(c.txid());
    sim.pass();
    invs(&sim, &p);
    assert_eq!(request(&mut sim, &p, req)[0].0, CommandString::NOTFOUND);
}

#[derive(Default)]
struct Objects(Option<(TxHash, Vec<u8>)>);
impl CoinJoinObjects for Objects {
    fn dstx_by_hash(&self, hash: &TxHash) -> Option<Vec<u8>> {
        self.0.as_ref().filter(|(id, _)| id == hash).map(|(_, bytes)| bytes.clone())
    }
    fn queue_by_hash(&self, _: &[u8; 32]) -> Option<Vec<u8>> {
        None
    }
}

#[test]
fn dstx_version_rules_preserve_balanced_and_downgrade_unbalanced_or_oversized() {
    for (count, output_count) in [(1, 2), (181, 181), (1, 1)] {
        let pool = SimTxPool::default();
        let inputs = (0..count)
            .map(|i| {
                TxIn::new(
                    OutPoint::new(TxHash::from_lendian([i as u8; 32]), 0),
                    vec![].into(),
                    u32::MAX,
                )
            })
            .collect();
        let outputs =
            (0..output_count).map(|_| TxOut::new(Default::default(), vec![].into())).collect();
        let a = Arc::new(Tx::new(Transaction::new(1, TxType::Spend, inputs, outputs, 0, vec![])));
        insert(&pool, &a, SIM_START, 1, 1);
        let payload = a.core_serialize();
        let mut sim = sim(&pool, |seams| {
            seams.coinjoin = Box::new(Objects(Some((a.txid(), payload.clone()))))
        });
        for (n, version, kind) in [
            (
                1,
                70240,
                if count == output_count && count <= 180 {
                    InvType::Dstx
                } else {
                    InvType::Tx
                },
            ),
            (2, 70241, InvType::Dstx),
            (3, 70242, InvType::Dstx),
        ] {
            let p = peer(&mut sim, n, ConnectionType::Inbound, Permissions::NO_BAN, version);
            sim.shared.handle().relay_transaction(a.txid());
            sim.pass();
            assert_eq!(invs(&sim, &p), [inv(kind, a.txid())]);
            let command = if kind == InvType::Dstx {
                CommandString::DSTX
            } else {
                CommandString::TX
            };
            assert_eq!(request(&mut sim, &p, inv(kind, a.txid())), [(command, payload.clone())]);
        }
    }
}

#[test]
fn asset_unlock_refresh_announces_instances_only_from_70242_and_serves_tx() {
    let pool = SimTxPool::default();
    let old = unlock(1);
    let new = unlock(2);
    assert_eq!(old.txid(), new.txid());
    insert(&pool, &old, SIM_START, 1, 1);
    let mut sim = sim(&pool, |_| {});
    let peers: Vec<_> = [70240, 70241, 70242]
        .into_iter()
        .enumerate()
        .map(|(i, version)| {
            peer(&mut sim, i as u8 + 1, ConnectionType::Inbound, Permissions::NO_BAN, version)
        })
        .collect();
    sim.shared.handle().relay_transaction(old.txid());
    sim.pass();
    for (i, p) in peers.iter().enumerate() {
        let req = if i == 2 {
            inv(InvType::AssetUnlock, old.instance_hash())
        } else {
            inv(InvType::Tx, old.txid())
        };
        assert_eq!(invs(&sim, p), [req]);
        assert_eq!(request(&mut sim, p, req), [(CommandString::TX, old.core_serialize())]);
    }
    insert(&pool, &new, SIM_START + 1, 1, 1);
    sim.shared.handle().relay_transaction(new.txid());
    sim.pass();
    assert!(invs(&sim, &peers[0]).is_empty());
    assert!(invs(&sim, &peers[1]).is_empty());
    let req = inv(InvType::AssetUnlock, new.instance_hash());
    assert_eq!(invs(&sim, &peers[2]), [req]);
    assert_eq!(request(&mut sim, &peers[2], req), [(CommandString::TX, new.core_serialize())]);
}

struct Locks(TxHash);
impl IsLockLookup for Locks {
    fn islock_by_txid(&self, _: &TxHash) -> Option<Vec<u8>> {
        None
    }
    fn islock_hash_by_txid(&self, txid: &TxHash) -> Option<[u8; 32]> {
        (*txid == self.0).then_some([42; 32])
    }
    fn islock_by_hash(&self, _: &[u8; 32]) -> Option<Vec<u8>> {
        None
    }
}
struct ChainLock(ClSig);
impl ChainLocks for ChainLock {
    fn is_enabled(&self) -> bool {
        true
    }
    fn already_have(&self, _: &[u8; 32]) -> bool {
        false
    }
    fn process_new_chainlock(
        &mut self,
        _: PeerId,
        _: Read<ClSig>,
        _: &[u8; 32],
    ) -> MessageProcessingResult {
        MessageProcessingResult::default()
    }
    fn chainlock_by_hash(&self, _: &[u8; 32], _: BlsCtx) -> Option<ClSig> {
        None
    }
    fn best_chainlock(&self, _: BlsCtx) -> Option<ClSig> {
        Some(self.0.clone())
    }
}

#[test]
fn bip35_filtered_reply_includes_matching_lock_and_best_chainlock_and_enables_getdata() {
    let pool = SimTxPool::default();
    let a = tx(1, &[]);
    let b = tx(2, &[]);
    insert(&pool, &a, SIM_START, 1, 1);
    insert(&pool, &b, SIM_START, 1, 2);
    let cl = ClSig::new(1, [0xab; 32].into(), [0; 96].into());
    let cl_hash = dash_sha256::sha256d(&encode(&cl));
    let mut sim = sim(&pool, |seams| {
        seams.islocks = Box::new(Locks(a.txid()));
        seams.chainlocks = Box::new(ChainLock(cl));
    });
    let p = peer(&mut sim, 1, ConnectionType::Inbound, Permissions::NO_BAN, 70242);
    let mut filter = BloomFilter::new(10, 0.000001, 1, BLOOM_UPDATE_ALL);
    filter.insert(&a.txid().to_lendian());
    sim.admitted(&p, "filterload", encode(&filter.to_msg())).unwrap();
    sim.admitted(&p, "mempool", vec![]).unwrap();
    sim.pass();
    assert_eq!(
        invs(&sim, &p),
        [
            inv(InvType::Tx, a.txid()),
            Inventory::new(InvType::IsdLock, Hash256::from_lendian([42; 32])),
            Inventory::new(InvType::ClSig, Hash256::from_lendian(cl_hash))
        ]
    );
    assert_eq!(
        request(&mut sim, &p, inv(InvType::Tx, b.txid())),
        [(CommandString::TX, b.core_serialize())],
        "BIP35 time bypasses recent-announcement filter even for filtered-out txs"
    );
    let after = tx(3, &[]);
    insert(&pool, &after, SIM_START + 1, 1, 1);
    assert_eq!(request(&mut sim, &p, inv(InvType::Tx, after.txid()))[0].0, CommandString::NOTFOUND);
}

#[test]
fn public_ask_preserves_live_fallbacks_and_forget_removes_tx_variants() {
    let pool = SimTxPool::default();
    let mut sim = sim(&pool, |_| {});
    let peers: Vec<_> = (1..=5)
        .map(|n| peer(&mut sim, n, ConnectionType::Inbound, Permissions::NONE, 70242))
        .collect();
    let a = tx(1, &[]);
    for p in &peers {
        sim.admitted(
            p,
            "inv",
            encode(&Inv::new(vec![inv(InvType::Tx, a.txid()), inv(InvType::Dstx, a.txid())])),
        )
        .unwrap();
    }
    let requests = sim.lp.logic().requested_objects();
    for p in &peers {
        assert_eq!(requests.count(p.handle.id()), 2);
    }
    sim.shared.handle().ask_peers_for_transaction(a.txid());
    sim.lp.drain_transaction_commands();
    for p in &peers {
        assert_eq!(requests.count(p.handle.id()), 2, "existing candidates survive");
    }
    sim.shared.handle().forget_tx(a.txid());
    sim.lp.drain_transaction_commands();
    for p in &peers {
        assert_eq!(requests.count(p.handle.id()), 0);
    }
    sim.shared.handle().ask_peers_for_transaction(a.txid());
    sim.lp.drain_transaction_commands();
    for (i, p) in peers.iter().enumerate() {
        assert_eq!(requests.count(p.handle.id()), usize::from(i < 4));
    }
    sim.pass();
    let mut total = 0;
    for p in &peers {
        total += sim.sent(p).iter().filter(|(cmd, _)| *cmd == CommandString::GETDATA).count();
    }
    assert_eq!(total, 1);
}

/// `IsInvInFilter` is false for a peer without transaction relay (np:2520-2527), so a
/// block-relay-only peer that knows the txid takes none of the four slots; each peer asked logs
/// Core's line (np:2511-2512).
#[test]
fn ask_skips_a_peer_without_transaction_relay_and_logs_each_ask() {
    let pool = SimTxPool::default();
    let mut sim = sim(&pool, |_| {});
    let blocks = peer(&mut sim, 1, ConnectionType::BlockRelay, Permissions::NONE, 70242);
    let relays: Vec<_> = (2..=5)
        .map(|n| peer(&mut sim, n, ConnectionType::Inbound, Permissions::NONE, 70242))
        .collect();
    let a = tx(1, &[]);
    assert_eq!(sim.shared.with_state(blocks.handle.id(), |st| st.tx_relay), Some(false));
    for p in std::iter::once(&blocks).chain(&relays) {
        sim.shared.with_state_mut(p.handle.id(), |st| {
            st.inventory_relay().lock().add_known(&a.txid().to_lendian());
        });
    }
    let log = log_capture::capture(&["dashd_p2p::processing::relay"]);
    sim.shared.handle().ask_peers_for_transaction(a.txid());
    sim.lp.drain_transaction_commands();
    let requests = sim.lp.logic().requested_objects();
    assert_eq!(requests.count(blocks.handle.id()), 0);
    for p in &relays {
        assert_eq!(requests.count(p.handle.id()), 1);
    }
    let mut asked = log.lines().clone();
    asked.sort();
    let want: Vec<_> = relays
        .iter()
        .map(|p| {
            format!(
                "PeerManagerImpl::AskPeersForTransaction -- txid={}: asking other peer {} for correct TX",
                a.txid(),
                p.handle.id()
            )
        })
        .collect();
    assert_eq!(asked, want);
}

#[test]
fn initial_broadcast_reannounces_an_unbroadcast_pool_transaction() {
    let pool = SimTxPool::default();
    let a = tx(1, &[]);
    insert(&pool, &a, SIM_START, 1, 1);
    let stale = tx(2, &[]).txid();
    pool.set_unbroadcast([a.txid(), stale]);
    let mut sim = sim(&pool, |_| {});
    let p = peer(&mut sim, 1, ConnectionType::Inbound, Permissions::NO_BAN, 70242);
    sim.shared.mock_scheduler(Duration::from_secs(901));
    sim.pass();
    assert_eq!(invs(&sim, &p), [inv(InvType::Tx, a.txid())]);
    assert_eq!(pool.unbroadcast(), [a.txid()]);
    assert_eq!(pool.unbroadcast_removals(), [(stale, true)]);
}

#[test]
fn relay_disabled_clears_queue_while_explicit_mempool_request_still_replies() {
    let pool = SimTxPool::default();
    let a = tx(1, &[]);
    insert(&pool, &a, SIM_START, 1, 1);
    let mut sim = sim(&pool, |_| {});
    let p = sim.connect(1, ConnectionType::Inbound, Permissions::NO_BAN);
    sim.handshake(
        &p,
        PeerVersionMsg {
            relay: false,
            ..PeerVersionMsg::v24(SIM_START)
        },
    );
    sim.shared.handle().relay_transaction(a.txid());
    sim.pass();
    assert!(invs(&sim, &p).is_empty());
    sim.admitted(&p, "mempool", vec![]).unwrap();
    sim.pass();
    assert_eq!(invs(&sim, &p), [inv(InvType::Tx, a.txid())]);
    let q = peer(&mut sim, 2, ConnectionType::BlockRelay, Permissions::NONE, 70242);
    sim.shared.handle().relay_transaction(a.txid());
    sim.pass();
    assert!(invs(&sim, &q).is_empty());
}

#[test]
fn core_signed_asset_unlock_vector_announces_and_serves_its_instance() {
    let raw = dashd_p2p::sim::transcript::unhex(
        include_str!("fixtures/dashd-v24/tx-asset-unlock.hex").trim(),
    );
    let tx = Arc::new(dash_consensus::decode_tx_exact(&raw).unwrap().into_value());
    assert_eq!(
        tx.txid().to_string(),
        "ed6d78c04f2e9ba2f1c81536cde6885725448ae7083b7580b21031cf565590b8"
    );
    assert_eq!(
        tx.instance_hash().to_string(),
        "9471462b6675e0f770c0844aeffc323b22a7b49a98a344def73026d2c55af7ec"
    );
    let pool = SimTxPool::default();
    insert(&pool, &tx, SIM_START, 1, 70000);
    let mut sim = sim(&pool, |_| {});
    for (n, version) in [(1, 70240), (2, 70241), (3, 70242)] {
        let p = peer(&mut sim, n, ConnectionType::Inbound, Permissions::NO_BAN, version);
        sim.shared.handle().relay_transaction(tx.txid());
        sim.pass();
        let req = if version >= 70242 {
            inv(InvType::AssetUnlock, tx.instance_hash())
        } else {
            inv(InvType::Tx, tx.txid())
        };
        assert_eq!(invs(&sim, &p), [req]);
        assert_eq!(request(&mut sim, &p, req), [(CommandString::TX, raw.clone())]);
    }
}

#[test]
fn bip35_verified_recsig_peer_suppresses_only_the_lock_inventory() {
    let pool = SimTxPool::default();
    let a = tx(1, &[]);
    insert(&pool, &a, SIM_START, 1, 1);
    let mut sim = sim(&pool, |seams| seams.islocks = Box::new(Locks(a.txid())));
    for (n, verified) in [(1, false), (2, true)] {
        let p = peer(&mut sim, n, ConnectionType::Inbound, Permissions::NO_BAN, 70242);
        if verified {
            assert!(sim.core.try_mark_verified(p.handle.id(), [n; 32], [n; 32]));
        }
        sim.shared.with_state_mut(p.handle.id(), |st| st.wants_recsigs = true);
        sim.admitted(&p, "mempool", vec![]).unwrap();
        sim.pass();
        let got = invs(&sim, &p);
        assert_eq!(got[0], inv(InvType::Tx, a.txid()));
        assert_eq!(
            got.len(),
            if verified {
                1
            } else {
                2
            }
        );
    }
}

#[test]
fn masternodes_bypass_trickle_only_for_verified_masternode_peers() {
    use dashd_p2p::processing::seams::MasternodeSeams;
    use dashd_p2p::sim::masternode::FakeSigner;
    let pool = SimTxPool::default();
    let a = tx(1, &[]);
    insert(&pool, &a, SIM_START, 1, 1);
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&params, &IdentityOptions::new()).unwrap();
    let seams = Seams {
        txpool: Arc::new(pool),
        ..Seams::default()
    };
    let mn = MasternodeSeams {
        signer: Some(Arc::new(FakeSigner::new(None))),
        ..MasternodeSeams::default()
    };
    let mut sim = Sim::with_masternode(
        params,
        id,
        ProcessorOptions::default(),
        Registry::new(),
        seams,
        mn,
        |_| {},
    );
    sim.chain.set_ibd(false);
    let plain = peer(&mut sim, 1, ConnectionType::Inbound, Permissions::NONE, 70242);
    let verified = peer(&mut sim, 2, ConnectionType::Inbound, Permissions::NONE, 70242);
    assert!(sim.core.try_mark_verified(verified.handle.id(), [2; 32], [2; 32]));
    for p in [&plain, &verified] {
        sim.shared.with_state_mut(p.handle.id(), |st| st.next_inv_send_us = i64::MAX);
    }
    sim.shared.handle().relay_transaction(a.txid());
    sim.pass();
    assert!(invs(&sim, &plain).is_empty());
    assert_eq!(invs(&sim, &verified), [inv(InvType::Tx, a.txid())]);
}

#[test]
fn finalizing_a_peer_drops_transaction_relay_state_before_removal() {
    let pool = SimTxPool::default();
    let a = tx(1, &[]);
    insert(&pool, &a, SIM_START, 1, 1);
    let mut sim = sim(&pool, |_| {});
    let peer = peer(&mut sim, 1, ConnectionType::Inbound, Permissions::NONE, 70242);
    let relay = sim
        .shared
        .with_state(peer.handle.id(), |state| Arc::clone(state.inventory_relay()))
        .unwrap();
    sim.shared.with_state_mut(peer.handle.id(), |state| state.next_inv_send_us = 0);
    sim.pass();
    sim.shared.with_state_mut(peer.handle.id(), |state| state.next_inv_send_us = i64::MAX);
    sim.shared.handle().relay_transaction(a.txid());
    sim.lp.drain_transaction_commands();
    assert_eq!(relay.lock().tx_inventory_to_send.len(), 1);
    sim.core.disconnect(peer.handle.id(), Why::Processor("test"));
    sim.remove_disconnected();
    assert!(sim.shared.with_state(peer.handle.id(), |_| ()).is_none());
    {
        let relay = relay.lock();
        assert!(!relay.tx_relay_enabled && !relay.started);
        assert!(relay.tx_inventory_to_send.is_empty());
    }
    sim.shared.handle().relay_transaction(a.txid());
    sim.pass();
    assert!(invs(&sim, &peer).is_empty());
}
