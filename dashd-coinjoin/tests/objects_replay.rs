#![cfg(feature = "testing")]
//! The DSTX fixtures replayed through the simulated node's `on_tx`, relay and getdata paths,
//! with [`RuntimeObjects`] installed as its CoinJoin objects. The host model of the chain, the
//! mempool and the historical masternode lists follows the fixture's RPC records and calls the
//! DSTX runtime's notifications as the node's listener will.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use common::replay::{
    Scores, Step, check_mask, disconnected, enabled_count, hex_value, masternodes, steps, tx_hash,
};
use common::transcript::{Direction, Record};
use dash_coinjoin::dstx::{DSTX_PRIORITY_DELTA, PoolLimits, txid};
use dash_coinjoin::queue::{MixingCounters, MixingMasternode, TipMasternodes};
use dash_p2p::CommandString;
use dash_p2p::conn::Permissions;
use dash_p2p_core::{DsTx, Inv, InvType, Inventory};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{BlockHash, OutPoint, Transaction, TxHash};
use dash_types::codec::BaseCodec;
use dashd_coinjoin::dsq::DsqRuntime;
use dashd_coinjoin::dstx::DstxRuntime;
use dashd_coinjoin::env::{MempoolListener, MnLists};
use dashd_coinjoin::objects::RuntimeObjects;
use dashd_coinjoin::testing::{FakeChainView, FakeMnHistory, Locked, MnHistoryEntry, MnListsCall};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::download::BlockDownload;
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::seams::TxPool;
use dashd_p2p::sim::txintake::SimTxIntakeHooks;
use dashd_p2p::sim::{Sim, SimTxPool};
use dashd_p2p::{Registry, Seams};
use serde_json::Value;

fn decode<T: BaseCodec>(bytes: &[u8]) -> T {
    T::decode(&mut &bytes[..]).expect("fixture payload")
}

fn block_hash(value: &Value) -> BlockHash {
    value.as_str().expect("block hash").parse().expect("block hash")
}

/// The historical lists, with the tip's enabled count Core used for queue thresholds.
struct Lists {
    history: Locked<FakeMnHistory>,
    enabled: i32,
}

impl TipMasternodes for Lists {
    fn valid_mn(&self, pro_tx_hash: &TxHash) -> Option<MixingMasternode> {
        self.history.valid_mn(pro_tx_hash)
    }

    fn enabled_count(&self) -> i32 {
        self.enabled
    }
}

impl MnLists for Lists {
    fn valid_by_collateral(&self, outpoint: &OutPoint) -> Option<MixingMasternode> {
        self.history.valid_by_collateral(outpoint)
    }

    fn mn_at_depth(&self, depth: i32, pro_tx_hash: &TxHash) -> Option<MixingMasternode> {
        self.history.mn_at_depth(depth, pro_tx_hash)
    }
}

/// The host: dashd's active chain above the setup tip, its mempool and the masternode lists.
struct Host {
    base: (i32, BlockHash),
    chain: Vec<BlockHash>,
    mempool: Vec<Transaction>,
    view: Arc<Locked<FakeChainView>>,
    lists: Arc<Lists>,
    pool: SimTxPool,
    runtime: Arc<DstxRuntime>,
    tracked: BTreeSet<TxHash>,
    map_sizes: Vec<Value>,
}

impl Host {
    fn tip(&self) -> (i32, BlockHash) {
        let height = self.base.0 + i32::try_from(self.chain.len()).unwrap();
        (height, self.chain.last().copied().unwrap_or(self.base.1))
    }

    fn connect(&mut self, hash: BlockHash, txs: Vec<Transaction>) {
        let fork = self.tip();
        let height = fork.0 + 1;
        let spent = txs
            .iter()
            .flat_map(|tx| tx.inputs().iter().map(|input| input.prevout()))
            .collect::<BTreeSet<_>>();
        self.lists.history.with(|history| {
            history.tip = height;
            for entry in history.entries.values_mut() {
                if spent.contains(&entry.mn.collateral) {
                    entry.removed = Some(height);
                }
            }
        });
        for tx in &txs {
            self.pool.remove(&txid(tx));
        }
        self.runtime.block_connected(&txs, height);
        self.chain.push(hash);
        self.view.with(|view| view.tip = Some(self.tip()));
        let view = self.view.with(|view| FakeChainView {
            tip: view.tip,
            synced: view.synced,
            ..FakeChainView::default()
        });
        self.runtime.updated_block_tip(Some(self.tip()), Some(fork), false, &view);
        let kept = self.tracked.iter().filter(|id| self.runtime.contains(id)).count();
        self.map_sizes.push(Value::from(kept));
    }
}

fn sim(
    fixture: &common::transcript::Fixture,
    seams: Seams,
    dsq: impl dashd_p2p::NetHandler + 'static,
) -> Sim {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    let mut registry = Registry::new();
    registry.register(Box::new(dsq));
    let sim = Sim::with(params, identity, ProcessorOptions::default(), registry, seams, |_| {});
    sim.chain.set_ibd(fixture.ibd);
    sim.clock.set_mock_time(fixture.mocktime);
    sim
}

/// The verdict a recorded `dstx_verdict` names for what `on_tx` did.
fn verdicts(accepted: bool, score: i32, known: bool, looked_up: bool) -> Vec<Value> {
    let names: &[&str] = match (accepted, score) {
        (true, 0) => &["verified", "accepted"],
        (false, 0) if known => &["already-have"],
        (false, 0) => &["mixing-limit"],
        (false, 1) => &["unknown-masternode"],
        (false, 10) if looked_up => &["bad-signature"],
        (false, 10) => &["invalid-structure"],
        other => panic!("no recorded verdict for {other:?}"),
    };
    names.iter().map(|name| Value::from(*name)).collect()
}

fn tx_invs(payload: &[u8]) -> Vec<Inventory> {
    decode::<Inv>(payload)
        .inventory()
        .iter()
        .filter(|inv| matches!(inv.inv_type(), InvType::Tx | InvType::Dstx))
        .copied()
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Announced {
    Tx(TxHash),
    Dstx(TxHash),
}

fn announced(inv: &Inventory) -> Announced {
    match inv.inv_type() {
        InvType::Tx => Announced::Tx(tx_hash(inv)),
        InvType::Dstx => Announced::Dstx(tx_hash(inv)),
        other => panic!("unexpected tx announcement kind {other:?}"),
    }
}

fn received_from_peer(log: &str) -> Option<u32> {
    let rest = log
        .split_once("[net] received: ")
        .or_else(|| log.split_once("[net] received getdata for: "))?
        .1;
    rest.split_whitespace().last()?.strip_prefix("peer=")?.parse().ok()
}

fn dashd_peer_map(fixture: &common::transcript::Fixture, steps: &[Step<'_>]) -> BTreeMap<u32, u32> {
    let base = u32::try_from(masternodes(fixture).len()).unwrap();
    let map = fixture
        .records
        .iter()
        .filter_map(|record| match record {
            Record::Accept {
                peer,
                ..
            } => Some(*peer),
            _ => None,
        })
        .enumerate()
        .map(|(index, peer)| (base + u32::try_from(index).unwrap(), peer))
        .collect::<BTreeMap<_, _>>();
    let mut verified = 0;
    for step in steps {
        let Record::Message(message) = step.record else {
            continue;
        };
        for log in step.logs() {
            if let Some(dashd_peer) = received_from_peer(log) {
                assert_eq!(
                    map.get(&dashd_peer),
                    Some(&message.peer),
                    "{}:{}: dashd peer={dashd_peer} vs fixture peer {}",
                    fixture.name,
                    step.line,
                    message.peer
                );
                verified += 1;
            }
        }
    }
    assert!(verified > 0, "{}: no received log verified peer map", fixture.name);
    map
}

fn parse_push_inv(log: &str) -> Option<(bool, u32, Announced)> {
    let (added, rest) = if let Some((_, rest)) = log.split_once("PushInv -- adding new inv: ") {
        (true, rest)
    } else if let Some((_, rest)) = log.split_once("PushInv -- skipping known inv: ") {
        (false, rest)
    } else {
        assert!(!log.contains("PushInv"), "unparsed PushInv log: {log}");
        return None;
    };
    let mut parts = rest.split_whitespace();
    let kind = parts.next().expect("PushInv kind");
    let hash: TxHash = parts.next().expect("PushInv hash").parse().expect("PushInv txid");
    let peer: u32 = parts
        .next()
        .and_then(|field| field.strip_prefix("peer="))
        .expect("PushInv peer")
        .parse()
        .expect("PushInv peer id");
    assert!(parts.next().is_none(), "trailing PushInv fields: {log}");
    let item = match kind {
        "tx" => Announced::Tx(hash),
        "dstx" => Announced::Dstx(hash),
        "dsq" | "clsig" | "isdlock" => return None,
        other => panic!("unexpected PushInv kind {other:?} in {log}"),
    };
    Some((added, peer, item))
}

/// Replays a DSTX fixture: each `> P dstx` goes through `on_tx`, and the replay compares the
/// verdict, the score, the single priority delta, what the node keeps, the transaction
/// announcements each peer received and each getdata answer with Core's.
fn replay(name: &str) {
    let fixture = common::transcript::fixture(name);
    check_mask(&fixture);
    let setup = &fixture.facts["setup"];
    let v24 = &setup["softforks"]["v24"];
    let base =
        (i32::try_from(setup["height"].as_i64().unwrap()).unwrap(), block_hash(&setup["tip"]));
    let view = Arc::new(Locked::new(FakeChainView {
        tip: Some(base),
        v24_at_tip: v24["active"].as_bool().unwrap(),
        v24_after_tip: v24["bip9"]["status_next"] == "active",
        synced: !fixture.ibd,
        ..FakeChainView::default()
    }));
    let lists = Arc::new(Lists {
        history: Locked::new(FakeMnHistory {
            tip: base.0,
            entries: masternodes(&fixture)
                .into_iter()
                .map(|mn| {
                    let protx = mn.pro_tx_hash;
                    (
                        protx,
                        MnHistoryEntry {
                            mn,
                            added: 0,
                            removed: None,
                        },
                    )
                })
                .collect(),
            ..FakeMnHistory::default()
        }),
        enabled: enabled_count(&fixture),
    });
    let meta = Arc::new(Mutex::new(MixingCounters::default()));
    let params = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    let runtime = Arc::new(DstxRuntime::new(PoolLimits::of(&params)));
    let dsq = DsqRuntime::new(true, Arc::clone(&view), Arc::clone(&lists), Arc::clone(&meta));
    let pool = SimTxPool::default();
    let hooks = SimTxIntakeHooks::default();
    let objects = RuntimeObjects::new(
        Arc::clone(&runtime),
        Arc::clone(&view),
        Arc::clone(&lists),
        Arc::clone(&meta),
        Some(dsq.clone()),
    );
    let seams = Seams {
        txpool: Arc::new(pool.clone()),
        coinjoin: Box::new(objects),
        tx_intake: Box::new(hooks.clone()),
        block_sync: Box::new(BlockDownload::default()),
        ..Seams::default()
    };
    let mut sim = sim(&fixture, seams, dsq);
    let mut host = Host {
        base,
        chain: Vec::new(),
        mempool: Vec::new(),
        view,
        lists,
        pool: pool.clone(),
        runtime: Arc::clone(&runtime),
        tracked: BTreeSet::new(),
        map_sizes: Vec::new(),
    };
    let steps = steps(&fixture);
    let peer_map = dashd_peer_map(&fixture, &steps);
    let mut peers = BTreeMap::new();
    let mut scores = Scores::default();
    let mut ours = BTreeMap::<u32, Vec<Inventory>>::new();
    let mut core = BTreeMap::<u32, Vec<Inventory>>::new();
    let mut queued = BTreeMap::<u32, Vec<Announced>>::new();
    let mut skipped = BTreeMap::<u32, BTreeSet<Announced>>::new();
    let mut dstxes = 0;
    for step in &steps {
        let at = format!("{name}:{}", step.line);
        let mut verdict = Vec::new();
        match step.record {
            Record::Accept {
                peer,
                ..
            } => {
                let accepted = sim.accept(u8::try_from(*peer).unwrap(), Permissions::NONE);
                peers.insert(*peer, accepted.unwrap_or_else(|| panic!("{at}: accept")));
            }
            Record::Time(time) => sim.clock.set_mock_time(*time),
            Record::Rpc(rpc) => match rpc.method.as_str() {
                "generatetoaddress" => {
                    for (n, hash) in rpc.result.as_array().unwrap().iter().enumerate() {
                        let txs = if n == 0 {
                            std::mem::take(&mut host.mempool)
                        } else {
                            Vec::new()
                        };
                        host.connect(block_hash(hash), txs);
                    }
                }
                "sendrawtransaction" => host.mempool.push(decode(&hex_value(&rpc.params[0]))),
                "getblockcount" => {
                    assert_eq!(rpc.result.as_i64(), Some(i64::from(host.tip().0)), "{at}")
                }
                "getrawmempool" => {
                    let model =
                        host.mempool.iter().map(|tx| txid(tx).to_string()).collect::<BTreeSet<_>>();
                    let core = rpc
                        .result
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|id| id.as_str().unwrap().to_owned())
                        .collect::<BTreeSet<_>>();
                    assert_eq!(model, core, "{at}: mempool (model vs Core)");
                    for id in &host.tracked {
                        assert_eq!(pool.exists(id), core.contains(&id.to_string()), "{at}");
                    }
                }
                other => panic!("{at}: no replay for rpc {other}"),
            },
            Record::Message(message) => {
                assert_eq!(message.direction, Direction::ToNode);
                let peer = &peers[&message.peer];
                match message.command.as_str() {
                    _ if message.echo || peer.handle.disconnect_requested() => {}
                    "dstx" => {
                        let dstx: DsTx = decode(&message.payload);
                        let id = txid(dstx.tx());
                        let known = runtime.contains(&id);
                        let priorities = hooks.priorities().len();
                        host.lists.history.with(|history| history.calls.lock().unwrap().clear());
                        let before = sim.shared.misbehavior(peer.handle.id()).unwrap().0;
                        sim.deliver(peer, &[("dstx", message.payload.clone())]);
                        sim.pass();
                        dstxes += 1;
                        let score = sim.shared.misbehavior(peer.handle.id()).unwrap().0 - before;
                        let applied = hooks.priorities()[priorities..].to_vec();
                        let accepted = !applied.is_empty();
                        assert_eq!(accepted, runtime.contains(&id) && !known, "{at}: kept");
                        if accepted {
                            assert_eq!(applied, [(id, DSTX_PRIORITY_DELTA)], "{at}: one delta");
                            assert_eq!(
                                runtime.get(&id).unwrap().dstx,
                                dstx,
                                "{at}: kept as received"
                            );
                            host.tracked.insert(id);
                            host.mempool.push(dstx.tx().clone());
                            runtime.transaction_added_to_mempool(dstx.tx());
                        }
                        let looked_up = host.lists.history.with(|history| {
                            history.calls.lock().unwrap().iter().any(|call| {
                                matches!(call, MnListsCall::MnAtDepth(_, protx)
                                if *protx == dstx.pro_tx_hash())
                            })
                        });
                        verdict = verdicts(accepted, score, known, looked_up);
                    }
                    command => sim.deliver(peer, &[(command, message.payload.clone())]),
                }
            }
            other => unreachable!("{at}: {other:?} does not drive a step"),
        }
        sim.pass();
        let mut served = BTreeMap::<u32, Vec<(String, Vec<u8>)>>::new();
        for (id, peer) in &peers {
            for (command, payload) in sim.sent(peer) {
                if command == CommandString::INV {
                    ours.entry(*id).or_default().extend(tx_invs(&payload));
                } else if matches!(command.as_str(), "dstx" | "tx" | "notfound") {
                    served.entry(*id).or_default().push((command.as_str().to_owned(), payload));
                }
            }
        }
        let mut recorded_getdata = BTreeMap::<u32, Vec<(String, Vec<u8>)>>::new();
        for (_, record) in &step.span {
            if let Record::Message(sent) = record
                && sent.direction == Direction::FromNode
            {
                if sent.command == "inv" {
                    core.entry(sent.peer).or_default().extend(tx_invs(&sent.payload));
                } else if matches!(sent.command.as_str(), "dstx" | "tx" | "notfound") {
                    recorded_getdata
                        .entry(sent.peer)
                        .or_default()
                        .push((sent.command.clone(), sent.payload.clone()));
                }
            }
        }
        for id in peers.keys().chain(recorded_getdata.keys()).copied().collect::<BTreeSet<_>>() {
            let actual = served.get(&id).map_or(&[][..], Vec::as_slice);
            let expected = recorded_getdata.get(&id).map_or(&[][..], Vec::as_slice);
            assert_eq!(actual, expected, "{at}: getdata answers to peer {id} (on_tx vs Core)");
        }
        for log in step.logs() {
            if let Some((added, dashd_peer, item)) = parse_push_inv(log)
                && let Some(&fixture_peer) = peer_map.get(&dashd_peer)
            {
                if added {
                    queued.entry(fixture_peer).or_default().push(item);
                } else {
                    skipped.entry(fixture_peer).or_default().insert(item);
                }
            }
        }
        assert_eq!(verdict, step.observed("dstx_verdict"), "{at}: verdict (on_tx vs Core)");
        assert_eq!(scores.deltas(&sim, &peers), step.score_deltas(), "{at}: score changes");
        assert_eq!(disconnected(&peers), BTreeSet::new(), "{at}: closed peers");
        let map_sizes = std::mem::take(&mut host.map_sizes);
        assert_eq!(map_sizes, step.observed("dstx_map_size"), "{at}: map sizes");
    }
    assert!(dstxes > 0, "{name}: no DSTX replayed");
    let non_empty_ours =
        ours.iter().filter(|(_, invs)| !invs.is_empty()).map(|(id, _)| *id).collect::<Vec<_>>();
    let non_empty_core =
        core.iter().filter(|(_, invs)| !invs.is_empty()).map(|(id, _)| *id).collect::<Vec<_>>();
    assert_eq!(non_empty_ours, non_empty_core, "{name}: peers");
    for id in peers.keys().chain(queued.keys()).chain(ours.keys()).copied().collect::<BTreeSet<_>>()
    {
        let sent = ours.get(&id).map_or(&[][..], Vec::as_slice);
        let recorded_invs = core.get(&id).map_or(&[][..], Vec::as_slice);
        assert!(sent.starts_with(recorded_invs), "{name}: announcements to {id} (on_tx vs Core)");
        assert!(sent.iter().all(|inv| host.tracked.contains(&tx_hash(inv))));

        let sent_items = sent.iter().map(announced).collect::<Vec<_>>();
        let sent_set = sent_items.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(sent_items.len(), sent_set.len(), "{name}: duplicate announcement to peer {id}");

        let queued_items = queued.get(&id).map_or(&[][..], Vec::as_slice);
        let queued_set = queued_items.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(
            queued_items.len(),
            queued_set.len(),
            "{name}: duplicate PushInv in fixture for peer {id}"
        );
        assert_eq!(
            sent_set, queued_set,
            "{name}: queued announcements to peer {id} (on_tx vs PushInv)"
        );
        if let Some(skipped_set) = skipped.get(&id) {
            assert!(
                sent_set.is_disjoint(skipped_set),
                "{name}: skipped announcement sent to peer {id}"
            );
        }
    }
}

#[test]
fn intake_replays_through_on_tx() {
    replay("dstx-intake");
}

#[test]
fn removed_mn_replays_through_on_tx() {
    replay("dstx-removed-mn");
}
