#![cfg(feature = "testing")]
//! Replays of the DSTX fixtures record by record through [`DstxRuntime`], with a host model of
//! the chain, the mempool and the historical masternode lists that the fixture's RPC records
//! drive. Queue traffic (`version`, `senddsq`, `dsq`) runs through the Sim and [`DsqRuntime`],
//! which share the mixing counters with the DSTX validation.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use common::replay::{
    Scores, Step, advertised_version, check_mask, disconnected, enabled_count, hex_value,
    inv_type_code, masternodes, steps, tx_hash,
};
use common::transcript::{Direction, Record};
use dash_coinjoin::dstx::{PoolLimits, txid};
use dash_coinjoin::queue::MixingCounters;
use dash_coinjoin::relay::TxInvKind;
use dash_p2p::conn::Permissions;
use dash_p2p_core::{DsTx, GetData, Inv, InvType, Inventory, ProtocolVersion};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{BlockHash, Transaction, TxHash};
use dash_types::codec::BaseCodec;
use dashd_coinjoin::dsq::DsqRuntime;
use dashd_coinjoin::dstx::DstxRuntime;
use dashd_coinjoin::env::{DstxChainAdapter, MempoolListener};
use dashd_coinjoin::testing::{
    FakeChainView, FakeMempool, FakeMnHistory, FakeMnLists, MnHistoryEntry, MnListsCall,
};
use serde_json::Value;

fn limits() -> PoolLimits {
    let params =
        ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest");
    PoolLimits::of(&params)
}

fn decode<T: BaseCodec>(bytes: &[u8]) -> T {
    T::decode(&mut &bytes[..]).expect("fixture payload")
}

fn block_hash(value: &Value) -> BlockHash {
    value.as_str().expect("block hash").parse().expect("block hash")
}

struct Block {
    hash: BlockHash,
    txs: Vec<Transaction>,
}

/// The host side of the replay: dashd's active chain above the setup tip, its mempool and the
/// masternode lists by height. Each change calls the DSTX runtime's notification the way
/// dashd's validation interface does and records what the runtime then reports, as Core logs
/// `UpdateDSTXConfirmedHeight` and `CheckDSTXes`.
struct Host {
    base: (i32, BlockHash),
    chain: Vec<Block>,
    invalid: Vec<Block>,
    mempool: Vec<Transaction>,
    mns: FakeMnHistory,
    v24_at_tip: bool,
    v24_after_tip: bool,
    ibd: bool,
    runtime: DstxRuntime,
    /// Every DSTX transaction the runtime accepted.
    tracked: BTreeSet<TxHash>,
    events: Vec<(&'static str, Value)>,
}

impl Host {
    fn tip(&self) -> (i32, BlockHash) {
        let height = self.base.0 + i32::try_from(self.chain.len()).unwrap();
        (height, self.chain.last().map_or(self.base.1, |block| block.hash))
    }

    fn view(&self) -> FakeChainView {
        FakeChainView {
            tip: Some(self.tip()),
            v24_at_tip: self.v24_at_tip,
            v24_after_tip: self.v24_after_tip,
            synced: !self.ibd,
            ..FakeChainView::default()
        }
    }

    fn confirmation(&self, id: &TxHash) -> Option<i32> {
        self.chain
            .iter()
            .position(|block| block.txs.iter().any(|tx| txid(tx) == *id))
            .map(|index| self.base.0 + 1 + i32::try_from(index).unwrap())
    }

    /// What the runtime holds for each of `txs` it tracks, read back after a notification.
    fn confirmed_heights(&mut self, txs: &[Transaction]) {
        for tx in txs {
            if let Some(stored) = self.runtime.get(&txid(tx)) {
                let height = stored.confirmed_height.unwrap_or(-1);
                self.events.push(("dstx_confirmed_height", Value::from(height)));
            }
        }
    }

    fn updated_tip(&mut self, fork: Option<(i32, BlockHash)>) {
        let view = self.view();
        self.runtime.updated_block_tip(Some(self.tip()), fork, self.ibd, &view);
        let kept = self.tracked.iter().filter(|id| self.runtime.contains(id)).count();
        self.events.push(("dstx_map_size", Value::from(kept)));
    }

    fn connect(&mut self, hash: BlockHash, txs: Vec<Transaction>) {
        let fork = self.tip();
        let height = fork.0 + 1;
        let spent = txs
            .iter()
            .flat_map(|tx| tx.inputs().iter().map(|input| input.prevout()))
            .collect::<BTreeSet<_>>();
        for entry in self.mns.entries.values_mut() {
            if spent.contains(&entry.mn.collateral) {
                entry.removed = Some(height);
            }
        }
        let ids = txs.iter().map(txid).collect::<BTreeSet<_>>();
        self.mempool.retain(|tx| !ids.contains(&txid(tx)));
        self.runtime.block_connected(&txs, height);
        self.confirmed_heights(&txs);
        self.chain.push(Block {
            hash,
            txs,
        });
        self.mns.tip = height;
        self.updated_tip(Some(fork));
    }

    /// `invalidateblock`: disconnects the block and its descendants, returns their transactions
    /// to the mempool, then notifies the new tip once.
    fn invalidate(&mut self, hash: BlockHash) {
        let index = self.chain.iter().position(|block| block.hash == hash).expect("invalidated");
        while self.chain.len() > index {
            let block = self.chain.pop().unwrap();
            let height = self.base.0 + 1 + i32::try_from(self.chain.len()).unwrap();
            for entry in self.mns.entries.values_mut() {
                if entry.removed == Some(height) {
                    entry.removed = None;
                }
            }
            self.runtime.block_disconnected(&block.txs);
            self.confirmed_heights(&block.txs);
            for tx in &block.txs {
                self.runtime.transaction_added_to_mempool(tx);
                self.confirmed_heights(std::slice::from_ref(tx));
                self.mempool.push(tx.clone());
            }
            self.invalid.insert(0, block);
        }
        self.mns.tip = self.tip().0;
        self.updated_tip(None);
    }
}

/// The verdict a recorded `dstx_verdict` names for a runtime result.
fn verdict(result: Result<(), i32>, known: bool, looked_up: bool) -> &'static str {
    match result {
        Ok(()) => "verified",
        Err(0) if known => "already-have",
        Err(0) => "mixing-limit",
        Err(1) => "unknown-masternode",
        Err(10) if looked_up => "bad-signature",
        Err(10) => "invalid-structure",
        Err(score) => panic!("no recorded verdict scores {score}"),
    }
}

fn inv_kind(inv: &Inventory) -> Option<TxInvKind> {
    match inv.inv_type() {
        InvType::Tx => Some(TxInvKind::Tx),
        InvType::Dstx => Some(TxInvKind::Dstx),
        _ => None,
    }
}

/// Replays a DSTX fixture record by record.
///
/// - Each `> P dstx` is validated by the runtime against the historical lists at the model's tip;
///   its verdict and score are compared with Core's. A verified one is admitted to the model's
///   mempool, kept by the runtime, and its mempool notification follows.
/// - `generatetoaddress` and `generateblock` connect blocks with the recorded hashes (the first
///   generated block takes the mempool), `invalidateblock` disconnects and returns transactions
///   to the mempool, `reconsiderblock` keeps the longer active branch, `sendrawtransaction`
///   enters the mempool, and a block spending a masternode collateral removes that masternode
///   from the lists from that height. `getblockcount`, `getrawmempool` and `getrawtransaction`
///   check the model against Core.
/// - After each step the runtime's confirmed heights and map sizes, read back after each
///   notification, and its lookup depths are compared with the step's observations in order.
/// - Each `< P inv` for a DSTX transaction is compared with `announce_kind` at the version the
///   peer negotiated, and each DSTX getdata answer with the runtime's stored DSTX.
fn replay_dstx(name: &str) {
    let fixture = common::transcript::fixture(name);
    check_mask(&fixture);
    let setup = &fixture.facts["setup"];
    assert_eq!(setup["blockchain_synced"].as_bool(), Some(!fixture.ibd));
    let v24 = &setup["softforks"]["v24"];
    let list = masternodes(&fixture);
    let counters = Arc::new(Mutex::new(MixingCounters::default()));
    let mut tip_mns = FakeMnLists {
        enabled: enabled_count(&fixture),
        ..FakeMnLists::default()
    };
    for mn in &list {
        tip_mns.by_collateral.insert(mn.collateral, mn.clone());
        tip_mns.valid.insert(mn.pro_tx_hash, mn.clone());
    }
    let dsq = DsqRuntime::new(
        true,
        Arc::new(FakeChainView {
            synced: !fixture.ibd,
            ..FakeChainView::default()
        }),
        Arc::new(tip_mns),
        Arc::clone(&counters),
    );
    let mut sim = common::replay::sim(&fixture, dsq, false, false);
    let base =
        (i32::try_from(setup["height"].as_i64().unwrap()).unwrap(), block_hash(&setup["tip"]));
    let mut host = Host {
        base,
        chain: Vec::new(),
        invalid: Vec::new(),
        mempool: Vec::new(),
        mns: FakeMnHistory {
            tip: base.0,
            entries: list
                .iter()
                .map(|mn| {
                    let entry = MnHistoryEntry {
                        mn: mn.clone(),
                        added: 0,
                        removed: None,
                    };
                    (mn.pro_tx_hash, entry)
                })
                .collect(),
            ..FakeMnHistory::default()
        },
        v24_at_tip: v24["active"].as_bool().unwrap(),
        v24_after_tip: v24["bip9"]["status_next"] == "active",
        ibd: fixture.ibd,
        runtime: DstxRuntime::new(limits()),
        tracked: BTreeSet::new(),
        events: Vec::new(),
    };
    let compared = ["dstx_verdict", "dstx_confirmed_height", "dstx_map_size", "last_present_depth"]
        .into_iter()
        .filter(|key| fixture.observations.iter().any(|observation| observation.key == *key))
        .collect::<Vec<_>>();
    let mut peers = BTreeMap::new();
    let mut versions = BTreeMap::<u32, (i32, i32)>::new();
    let mut scores = Scores::default();
    let mut validated = 0;
    for step in &steps(&fixture) {
        let at = format!("{name}:{}", step.line);
        let mut dstx_score = None;
        let mut accepted = false;
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
                    let hashes = rpc.result.as_array().unwrap();
                    assert_eq!(rpc.params[0].as_u64(), Some(hashes.len() as u64));
                    for (n, hash) in hashes.iter().enumerate() {
                        let txs = if n == 0 {
                            std::mem::take(&mut host.mempool)
                        } else {
                            Vec::new()
                        };
                        host.connect(block_hash(hash), txs);
                    }
                }
                "generateblock" => {
                    let txs = rpc.params[1]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|tx| decode::<Transaction>(&hex_value(tx)))
                        .collect();
                    host.connect(block_hash(&rpc.result["hash"]), txs);
                }
                "invalidateblock" => host.invalidate(block_hash(&rpc.params[0])),
                "reconsiderblock" => {
                    let hash = block_hash(&rpc.params[0]);
                    let branch = host.invalid.iter().position(|block| block.hash == hash);
                    let branch = branch.expect("a reconsidered block was invalidated");
                    assert!(
                        host.chain.len() >= host.invalid.len() - branch,
                        "{at}: the reconsidered branch would win; not modelled"
                    );
                }
                "sendrawtransaction" => {
                    let tx: Transaction = decode(&hex_value(&rpc.params[0]));
                    assert_eq!(rpc.result.as_str(), Some(txid(&tx).to_string().as_str()));
                    host.mempool.push(tx);
                }
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
                        .collect::<BTreeSet<String>>();
                    assert_eq!(model, core, "{at}: mempool (model vs Core)");
                }
                "getrawtransaction" => {
                    let id = rpc.params[0].as_str().unwrap().parse().unwrap();
                    let height = host.confirmation(&id).expect("a confirmed transaction");
                    let block = &host.chain[usize::try_from(height - base.0 - 1).unwrap()];
                    assert_eq!(block_hash(&rpc.result["blockhash"]), block.hash, "{at}");
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
                        let known = host.runtime.contains(&id);
                        host.mns.calls.lock().unwrap().clear();
                        let mut pool = FakeMempool::default();
                        let result = host.runtime.validate(
                            &dstx,
                            &id,
                            &mut *counters.lock().unwrap(),
                            &DstxChainAdapter {
                                chain: &host.view(),
                                mns: &host.mns,
                            },
                            &mut pool,
                        );
                        validated += 1;
                        let depths = host
                            .mns
                            .calls
                            .lock()
                            .unwrap()
                            .iter()
                            .filter_map(|call| match call {
                                MnListsCall::MnAtDepth(depth, protx)
                                    if *protx == dstx.pro_tx_hash() =>
                                {
                                    Some(*depth)
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        host.events.push((
                            "dstx_verdict",
                            verdict(result, known, !depths.is_empty()).into(),
                        ));
                        assert_eq!(
                            depths,
                            (0..i32::try_from(depths.len()).unwrap()).collect::<Vec<_>>(),
                            "{at}: lookup depths"
                        );
                        match result {
                            Ok(()) => {
                                host.events.push(("dstx_verdict", "accepted".into()));
                                host.mempool.push(dstx.tx().clone());
                                host.tracked.insert(id);
                                host.runtime.on_accepted(dstx.clone());
                                accepted = true;
                                host.runtime.transaction_added_to_mempool(dstx.tx());
                                host.confirmed_heights(std::slice::from_ref(dstx.tx()));
                            }
                            Err(score) if score > 0 => dstx_score = Some(score),
                            Err(_) => {}
                        }
                        if let Some(present) = (0..=host.mns.tip).find(|depth| {
                            host.mns.entries.get(&dstx.pro_tx_hash()).is_some_and(|entry| {
                                entry.removed.is_none_or(|removed| host.mns.tip - depth < removed)
                            })
                        }) && (result.is_ok() || result == Err(1))
                        {
                            assert!(result.is_err() || depths.last() == Some(&present), "{at}");
                            host.events.push(("last_present_depth", present.into()));
                        }
                        let confirmed = host.confirmation(&id);
                        if let Some(stored) = host.runtime.get(&id) {
                            assert_eq!(
                                stored.confirmed_height, confirmed,
                                "{at}: confirmed height"
                            );
                        }
                        // The fixture names the resent transaction's depth where expiry is at stake.
                        for depth in step.observed("depth") {
                            let depth = depth.as_i64().map(|depth| i32::try_from(depth).unwrap());
                            assert_eq!(
                                confirmed.map(|height| host.tip().0 - height),
                                depth,
                                "{at}"
                            );
                        }
                    }
                    "getdata" => {
                        // dashd answers a DSTX or transaction getdata from its mempool, wrapping a
                        // kept DSTX for MSG_DSTX (`net_processing.cpp` `ProcessGetData`).
                        let request: GetData = decode(&message.payload);
                        let mut missing = Vec::new();
                        let mut served = Vec::new();
                        for inv in request.inventory() {
                            let id = tx_hash(inv);
                            let tx = host.mempool.iter().find(|tx| txid(tx) == id);
                            match (inv_kind(inv), tx) {
                                (Some(TxInvKind::Dstx), Some(_)) => {
                                    let stored = host.runtime.get(&id).expect("a kept DSTX");
                                    let mut payload = Vec::new();
                                    stored.dstx.encode(&mut payload);
                                    served.push(("dstx", payload));
                                }
                                (Some(TxInvKind::Tx), Some(tx)) => {
                                    let mut payload = Vec::new();
                                    tx.encode(&mut payload);
                                    served.push(("tx", payload));
                                }
                                _ => missing.push(*inv),
                            }
                        }
                        if !missing.is_empty() {
                            let mut payload = Vec::new();
                            dash_p2p_core::NotFound::new(missing).encode(&mut payload);
                            served.push(("notfound", payload));
                        }
                        let recorded = step
                            .span
                            .iter()
                            .filter_map(|(_, record)| match record {
                                Record::Message(sent)
                                    if sent.direction == Direction::FromNode
                                        && sent.peer == message.peer
                                        && matches!(
                                            sent.command.as_str(),
                                            "dstx" | "tx" | "notfound"
                                        ) =>
                                {
                                    Some((sent.command.as_str(), sent.payload.clone()))
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        assert_eq!(served, recorded, "{at}: getdata answer (runtime vs Core)");
                    }
                    "version" => {
                        let ours = step
                            .span
                            .iter()
                            .find_map(|(_, record)| match record {
                                Record::Message(sent)
                                    if sent.direction == Direction::FromNode
                                        && sent.peer == message.peer
                                        && sent.command == "version" =>
                                {
                                    Some(advertised_version(&sent.payload))
                                }
                                _ => None,
                            })
                            .expect("dashd's version");
                        versions.insert(message.peer, (ours, advertised_version(&message.payload)));
                        sim.deliver(peer, &[("version", message.payload.clone())]);
                    }
                    "verack" | "senddsq" | "dsq" => {
                        sim.deliver(peer, &[(message.command.as_str(), message.payload.clone())]);
                    }
                    other => panic!("{at}: no replay for {other}"),
                }
            }
            other => unreachable!("{at}: {other:?} does not drive a step"),
        }
        sim.pass();
        for peer in peers.values() {
            sim.sent(peer);
        }

        let mut deltas = scores.deltas(&sim, &peers);
        if let Some(score) = dstx_score {
            *deltas.entry(step.peer().unwrap()).or_default() += score;
        }
        assert_eq!(deltas, step.score_deltas(), "{at}: score changes");
        assert_eq!(disconnected(&peers), BTreeSet::new(), "{at}: closed peers");
        compare_events(&at, step, &compared, std::mem::take(&mut host.events), accepted);
        check_announcements(&at, step, &host, &versions);
    }
    assert!(validated > 0, "{name}: no DSTX replayed");
}

/// Compares the runtime's reports in a step with the step's observations, in order. The mempool
/// notification of a just-admitted DSTX may run before or after dashd keeps the DSTX (Core
/// v24.0.0 `net_processing.cpp:4962-4967` admits, then `AddDSTX`; the `TransactionAddedToMempool`
/// signal is asynchronous, `dsnotificationinterface.cpp:74-78`), so Core logs its `-1` for some
/// admissions and not others; the replay drops it where Core did not log it.
fn compare_events(
    at: &str,
    step: &Step<'_>,
    compared: &[&str],
    mut events: Vec<(&'static str, Value)>,
    accepted: bool,
) {
    let observed = step
        .span
        .iter()
        .filter_map(|(_, record)| match record {
            Record::Observe(observation) if compared.contains(&observation.key.as_str()) => {
                Some((observation.key.as_str(), observation.value.clone()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    events.retain(|(key, _)| compared.contains(key));
    if accepted && !observed.iter().any(|(key, _)| *key == "dstx_confirmed_height") {
        let notification = ("dstx_confirmed_height", Value::from(-1));
        let position = events.iter().position(|event| *event == notification);
        events.remove(position.expect("the admitted DSTX's mempool notification"));
    }
    let events = events.iter().map(|(key, value)| (*key, value.clone())).collect::<Vec<_>>();
    assert_eq!(events, observed, "{at}: runtime reports vs Core's observations");
}

/// Each recorded `< P inv` naming a DSTX transaction uses the kind `announce_kind` gives for
/// the peer's negotiated version (the lower of the two `version` records).
fn check_announcements(
    at: &str,
    step: &Step<'_>,
    host: &Host,
    versions: &BTreeMap<u32, (i32, i32)>,
) {
    for (_, record) in &step.span {
        let Record::Message(message) = record else {
            continue;
        };
        if message.direction != Direction::FromNode || message.command != "inv" {
            continue;
        }
        for inv in decode::<Inv>(&message.payload).inventory() {
            let Some(kind) = inv_kind(inv) else {
                continue;
            };
            let id = tx_hash(inv);
            if !host.tracked.contains(&id) {
                continue;
            }
            let (ours, theirs) = versions[&message.peer];
            let version = ProtocolVersion::new(ours.min(theirs));
            assert_eq!(
                host.runtime.announce_kind(&id, version),
                kind,
                "{at}: inv type {} to peer {} at version {}",
                inv_type_code(inv),
                message.peer,
                ours.min(theirs)
            );
        }
    }
}

#[test]
fn intake_replay_differential() {
    replay_dstx("dstx-intake");
}

#[test]
fn lifecycle_reorg_replay_differential() {
    replay_dstx("dstx-lifecycle-reorg");
}

#[test]
fn removed_mn_replay_differential() {
    replay_dstx("dstx-removed-mn");
}
