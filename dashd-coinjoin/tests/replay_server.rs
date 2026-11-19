#![cfg(feature = "testing")]
//! Replays of the masternode fixtures (the `server-*` sessions, `dsq-modes` and
//! `dsq-server-intake`) through the Sim and the server runtime.

mod common;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use common::replay::{
    Out, Recorded, Scores, Source, Step, StepRule, Streams, check_mask, compare_streams,
    disconnected, dsq_projection, enabled_count, hex_value, inv_type_code, inventory_projection,
    is_dsq_inv, is_dsq_inv_payload, masternodes, pin_queue_hashes, queue_aliases, record_steps,
    recorded_streams, split_streams, step_drivers, steps, tx_hash, wire_hex,
};
use common::transcript::{Direction, Fixture, Record};
use dash_coinjoin::dstx::{PoolLimits, txid};
use dash_coinjoin::inouts::ViewCoin;
use dash_coinjoin::queue::{MixingCounters, check_queue_signature};
use dash_p2p::conn::Permissions;
use dash_p2p::{NetAddr, Service};
use dash_p2p_core::{DsEntry, DsQueue, DsStatusUpdate, InvType, Inventory};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_pkc::bls::core::{BlsPkValue, BlsVerdict};
use dash_pkc::bls::{BlsScIetf, BlsSchemeKind, BlsSecretKey};
use dash_primitives::{OutPoint, Transaction, TxHash, TxOut};
use dash_types::Hashable;
use dash_types::codec::BaseCodec;
use dashd_coinjoin::server::{CoinJoinServer, SharedServer};
use dashd_coinjoin::testing::{
    FakeActiveMn, FakeChainView, FakeMempool, FakeMnLists, FakePeerSink, FakeServerClock,
    MempoolCall, PeerCall,
};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, SimPeer};
use serde_json::Value;

type ServerRuntime = CoinJoinServer<
    FakeChainView,
    FakeMnLists,
    FakeActiveMn,
    FakeMempool,
    FakePeerSink,
    FakeServerClock,
    ReplayRandom,
>;

/// The server's random source in a replay.
/// - Session ids answer from the ids the fixture records, in order of first appearance, each as
///   Core's id minus one (`coinjoin/server.cpp:1128`), so the runtime's ids compare exactly.
/// - `GetRand<int>(100)` answers from the script the replay derives from each step's Core log
///   lines (see [`fee_draws`]).
/// - Core picks one of several offenders with a shuffle (`:585`) the fixture mask lets vary. The
///   runtime's pick draws with the offender count as bound, so in a step whose logs show a
///   `SelectCollateralToCharge` block charging fees, one draw bounded by that block's
///   `found offence` count answers 0 (see [`offender_picks`]).
///
/// Any other draw has no scripted answer: it is a divergence from Core and is recorded.
#[derive(Clone, Default)]
struct ReplayRandom {
    session_ids: Arc<Mutex<VecDeque<u32>>>,
    fee_draws: Arc<Mutex<VecDeque<u32>>>,
    /// The bounds the current step's offender picks draw with, each answered once.
    offender_picks: Arc<Mutex<Vec<u32>>>,
    unscripted: Arc<Mutex<Vec<u32>>>,
}

impl dashd_coinjoin::server::RandomSource for ReplayRandom {
    fn below(&mut self, upper: u32) -> u32 {
        let mut picks = self.offender_picks.lock().unwrap();
        if let Some(pick) = picks.iter().position(|bound| *bound == upper) {
            picks.remove(pick);
            return 0;
        }
        drop(picks);
        let scripted = match upper {
            999_999 => self.session_ids.lock().unwrap().pop_front().map(|id| id - 1),
            100 => self.fee_draws.lock().unwrap().pop_front(),
            _ => None,
        };
        scripted.unwrap_or_else(|| {
            self.unscripted.lock().unwrap().push(upper);
            upper.saturating_sub(1)
        })
    }
}

impl ReplayRandom {
    fn of(fixture: &Fixture) -> Self {
        let mut statuses = recorded_streams(fixture, |message| message.command == "dssu")
            .into_values()
            .flatten()
            .collect::<Vec<_>>();
        statuses.sort_by_key(|entry| entry.line);
        let mut seen: Vec<(u32, u32)> = Vec::new();
        for entry in statuses {
            let Some(alias) = entry.value["session"].as_str() else {
                continue;
            };
            let number = alias.strip_prefix("session-").and_then(|n| n.parse().ok()).unwrap();
            let id = u32::from_le_bytes(entry.payload[..4].try_into().unwrap());
            if !seen.iter().any(|(known, _)| *known == number) {
                seen.push((number, id));
            }
        }
        seen.sort();
        let random = Self::default();
        random.session_ids.lock().unwrap().extend(seen.into_iter().map(|(_, id)| id));
        random
    }
}

/// Core's `GetRand<int>(100)` answers in one step, read from the Core log lines that follow it.
/// The `SelectCollateralToCharge` answers sit on both sides of its threshold (33 and 34), so a
/// runtime comparing with another threshold diverges. The `ChargeRandomFees` answers do not: no
/// fixture logs a `charging random fees` line, so its script is always 11, which pins Core's
/// `> 10` from above only (a runtime with any threshold below 11 returns on it too).
/// - `SelectCollateralToCharge` (Core v24.0.0 `coinjoin/server.cpp:536-597`) runs after
///   `CheckTimeout -- ... timed out -- resetting` (`:742-744`) and inside a
///   `CreateFinalTransaction` that no `CheckPool -- FINALIZE TRANSACTIONS` started (the timeout
///   path, `:367-371`, `:420-421`). Its first draw returns at `> 33` (`:541`): 33 when a
///   `found offence` line follows, 34 otherwise. With offences its second draw (`:579`) returns
///   at `> 33`: 33 when `charging fees` follows, 34 otherwise. The second draw assumes the
///   offenders are at least all collaterals but one, which holds for the two-client sessions
///   the fixtures record.
/// - `ChargeRandomFees` (`:511`, `:607-616`) draws once per collateral and returns at `> 10`:
///   10 for each `charging random fees` line before `COMPLETED -- RESETTING` (none recorded),
///   then 11.
fn fee_draws(step: &Step<'_>) -> Vec<u32> {
    let logs = step.logs();
    let mut draws = Vec::new();
    let mut random_charges = 0;
    for (index, log) in logs.iter().enumerate() {
        let selects = (log.contains("CCoinJoinServer::CheckTimeout --")
            && log.contains("timed out -- resetting"))
            || (log.contains("CCoinJoinServer::CreateFinalTransaction -- FINALIZE TRANSACTIONS")
                && !index.checked_sub(1).is_some_and(|previous| {
                    logs[previous].contains("CCoinJoinServer::CheckPool -- FINALIZE TRANSACTIONS")
                }));
        if selects {
            let selection = logs[index + 1..]
                .iter()
                .take_while(|log| log.contains("CCoinJoinServer::SelectCollateralToCharge"))
                .collect::<Vec<_>>();
            let offence = selection.iter().any(|log| log.contains("found offence"));
            draws.push(if offence {
                33
            } else {
                34
            });
            if offence {
                let charged = selection.iter().any(|log| log.contains("charging fees"));
                draws.push(if charged {
                    33
                } else {
                    34
                });
            }
        } else if log.contains("ChargeRandomFees -- charging random fees") {
            random_charges += 1;
        } else if log.contains("CommitFinalTransaction -- COMPLETED -- RESETTING") {
            draws.extend(std::iter::repeat_n(10, random_charges));
            draws.push(11);
            random_charges = 0;
        }
    }
    draws
}

/// The offender count of each `SelectCollateralToCharge` block in one step that charges fees:
/// Core logs one `found offence` line per offender (`coinjoin/server.cpp:550-575`).
fn offender_picks(step: &Step<'_>) -> Vec<u32> {
    let logs = step.logs();
    let blocks = logs.chunk_by(|a, b| {
        a.contains("CCoinJoinServer::SelectCollateralToCharge")
            == b.contains("CCoinJoinServer::SelectCollateralToCharge")
    });
    blocks
        .filter(|block| {
            block.iter().any(|log| {
                log.contains("CCoinJoinServer::SelectCollateralToCharge")
                    && log.contains("charging fees")
            })
        })
        .map(|block| {
            let offences = block.iter().filter(|log| log.contains("found offence")).count();
            u32::try_from(offences).unwrap()
        })
        .collect()
}

fn duffs(value: &Value) -> i64 {
    (value.as_f64().expect("amount") * 100_000_000.0).round() as i64
}

/// `getcoinjoininfo`'s server fields (Core v24.0.0 `coinjoin/server.cpp:1435-1443`).
fn info_value(info: &dashd_coinjoin::server::ServerInfo) -> Value {
    serde_json::json!({
        "denomination": info.denomination as f64 / 100_000_000.0,
        "entries_count": info.entries_count,
        "queue_size": info.queue_size,
        "state": info.state.as_str(),
    })
}

fn decode<T: BaseCodec>(bytes: &[u8]) -> T {
    T::decode(&mut &bytes[..]).expect("fixture payload")
}

/// Transactions the replay's mempool accepts, as dashd's test-accept and submission answered.
#[derive(Default)]
struct Pool {
    accepted: BTreeSet<String>,
    /// Collateral transaction ids by participant alias.
    participants: BTreeMap<String, String>,
}

/// The mempool and coins view as dashd answered for this fixture.
/// - Collateral inputs: each `clients[]` collateral spends a coin worth its outputs plus the
///   recorded `acceptance.fees.base`, and test-accept answers that fee for its txid when
///   `acceptance.allowed`. The input coin's script is not recorded, so it reuses the collateral
///   output's script; the runtime does not read it.
/// - Denominated coins: `clients[].txid:vout` with `clients[].script`, valued as the `coins[]`
///   entry for the same client (whose script must match).
/// - The final transaction: the signed `final_signing` transaction, with the fee its recorded
///   coins give it.
///
/// Every other transaction is rejected, and every input of a recorded `dsi` must have a coin.
fn mempool(fixture: &Fixture, observer: Arc<Mutex<Vec<MempoolCall>>>) -> (FakeMempool, Pool) {
    let mut mempool = FakeMempool {
        observer: Some(observer),
        ..FakeMempool::default()
    };
    let mut pool = Pool::default();
    let clients =
        fixture.facts.get("clients").and_then(Value::as_array).cloned().unwrap_or_default();
    let coins = fixture.facts.get("coins").and_then(Value::as_array).cloned().unwrap_or_default();
    assert_eq!(clients.len(), coins.len(), "{}: one coin per client", fixture.name);
    for (client, coin) in clients.iter().zip(&coins) {
        let collateral: Transaction = decode(&hex_value(&client["collateral"]));
        let collateral_id = txid(&collateral);
        let acceptance = &client["acceptance"];
        assert_eq!(acceptance["txid"].as_str(), Some(collateral_id.to_string().as_str()));
        let fee = duffs(&acceptance["fees"]["base"]);
        let [input] = &collateral.inputs()[..] else {
            panic!("{}: a collateral with one input", fixture.name);
        };
        let output = collateral.outputs().first().expect("collateral output");
        let value_out = collateral.outputs().iter().map(TxOut::value).sum::<i64>();
        mempool.coins.insert(
            input.prevout(),
            ViewCoin {
                out: TxOut::from_duffs(value_out + fee, output.script_pubkey().clone()),
                in_mempool: false,
            },
        );
        if acceptance["allowed"].as_bool() == Some(true) {
            mempool.accept_fees.insert(collateral_id, fee);
            mempool.submit_results.insert(collateral_id, true);
            pool.accepted.insert(collateral_id.to_string());
        }
        pool.participants
            .insert(client["participant"].as_str().unwrap().to_owned(), collateral_id.to_string());
        assert_eq!(coin["scriptPubKey"]["hex"], client["script"], "{}: client coin", fixture.name);
        mempool.coins.insert(
            OutPoint::new(
                client["txid"].as_str().unwrap().parse().unwrap(),
                u32::try_from(client["vout"].as_u64().unwrap()).unwrap(),
            ),
            ViewCoin {
                out: TxOut::from_duffs(duffs(&coin["value"]), hex_value(&client["script"]).into()),
                in_mempool: false,
            },
        );
    }
    for (wire, participant) in fixture.facts["collaterals"].as_object().unwrap() {
        let inv = common::replay::inventory(1, wire);
        assert_eq!(
            pool.participants.get(participant.as_str().unwrap()),
            Some(&tx_hash(&inv).to_string()),
            "{}: collateral {wire} is the client's",
            fixture.name
        );
    }
    if let Some(signing) = fixture.facts.get("final_signing") {
        assert_eq!(signing["complete"].as_bool(), Some(true));
        let tx: Transaction = decode(&hex_value(&signing["hex"]));
        let value_in = tx
            .inputs()
            .iter()
            .map(|input| mempool.coins[&input.prevout()].out.value())
            .sum::<i64>();
        let value_out = tx.outputs().iter().map(TxOut::value).sum::<i64>();
        let id = txid(&tx);
        mempool.accept_fees.insert(id, value_in - value_out);
        mempool.submit_results.insert(id, true);
        pool.accepted.insert(id.to_string());
    }
    for record in &fixture.records {
        let Record::Message(message) = record else {
            continue;
        };
        if message.direction == Direction::ToNode
            && message.command == "dsi"
            && let Ok(entry) = DsEntry::decode(&mut &message.payload[..])
        {
            for input in entry.inputs() {
                assert!(
                    mempool.coins.contains_key(&input.prevout()),
                    "{}: dsi input {:?} has no recorded coin",
                    fixture.name,
                    input.prevout()
                );
            }
        }
    }
    (mempool, pool)
}

fn service(peer: u32) -> Service {
    Service {
        addr: NetAddr::ipv4([127, 0, 0, 1]),
        port: 40000 + u16::try_from(peer).unwrap(),
    }
}

struct Server {
    runtime: ServerRuntime,
    peer_calls: Arc<Mutex<Vec<PeerCall>>>,
    mempool_calls: Arc<Mutex<Vec<MempoolCall>>>,
    clock: FakeServerClock,
    random: ReplayRandom,
    pool: Pool,
    /// The public key of the test key the runtime signs its queues with.
    active_key: BlsPkValue,
}

fn server(fixture: &Fixture) -> Server {
    let params = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .build()
        .expect("regtest parameters");
    let limits = PoolLimits::of(&params);
    let mut mns = FakeMnLists {
        enabled: enabled_count(fixture),
        ..FakeMnLists::default()
    };
    for mn in masternodes(fixture) {
        mns.by_collateral.insert(mn.collateral, mn.clone());
        mns.valid.insert(mn.pro_tx_hash, mn);
    }
    let setup = &fixture.facts["setup"];
    assert_eq!(setup["blockchain_synced"].as_bool(), Some(!fixture.ibd));
    let pro_tx_hash = setup["masternodes"][0]["pro_tx_hash"].as_str().unwrap().parse().unwrap();
    let outpoint =
        mns.valid.get(&pro_tx_hash).expect("the serving masternode is listed").collateral;
    let key = BlsSecretKey::<BlsScIetf>::from_ikm(&[7; 32]).unwrap();
    let active_key =
        BlsPkValue::from_canonical(&key.public_key().to_bytes(), BlsSchemeKind::Basic).unwrap();
    let mempool_calls = Arc::new(Mutex::new(Vec::new()));
    let (mempool, pool) = mempool(fixture, Arc::clone(&mempool_calls));
    let peer_calls = Arc::new(Mutex::new(Vec::new()));
    let peers = FakePeerSink {
        observer: Some(Arc::clone(&peer_calls)),
        ..FakePeerSink::default()
    };
    let clock = FakeServerClock::at(fixture.mocktime);
    let random = ReplayRandom::of(fixture);
    let runtime = CoinJoinServer::new(
        limits,
        FakeChainView {
            synced: !fixture.ibd,
            tip: setup["tip"].as_str().map(|tip| {
                (i32::try_from(setup["height"].as_i64().unwrap()).unwrap(), tip.parse().unwrap())
            }),
            ..FakeChainView::default()
        },
        mns,
        FakeActiveMn {
            outpoint,
            pro_tx_hash,
            key: Some(key),
            ..FakeActiveMn::default()
        },
        mempool,
        peers,
        clock.clone(),
        random.clone(),
        MixingCounters::default(),
        Arc::new(dashd_coinjoin::dstx::DstxRuntime::new(limits)),
    );
    Server {
        runtime,
        peer_calls,
        mempool_calls,
        clock,
        random,
        pool,
        active_key,
    }
}

#[test]
fn session_replay_preserves_bl180_predecode_reply() {
    let fixture = common::transcript::fixture("server-session");
    let input = fixture
        .records
        .iter()
        .find_map(|record| match record {
            Record::Message(message)
                if message.direction == Direction::ToNode
                    && message.command == "dsi"
                    && message.payload == [0xff] =>
            {
                Some(message)
            }
            _ => None,
        })
        .expect("the fixture's truncated dsi");
    let expected = fixture
        .expects
        .iter()
        .find(|expect| expect.command == "dssu" && expect.peer == input.peer)
        .expect("the fixture's dssu expectation");
    let server = server(&fixture);
    let mut sim = common::replay::sim(&fixture, server.runtime, true, false);
    let peer = sim.accept(u8::try_from(input.peer).unwrap(), Permissions::NONE).unwrap();
    sim.handshake(&peer, PeerVersionMsg::v24(SIM_START));
    sim.pass();
    sim.sent(&peer);
    assert_eq!(sim.admitted(&peer, &input.command, input.payload.clone()), Ok(()));
    sim.pass();

    let (_, payload) = sim
        .sent(&peer)
        .into_iter()
        .find(|(command, _)| command.as_str() == "dssu")
        .expect("a dssu reply");
    let status = DsStatusUpdate::decode(&mut &payload[..]).expect("dssu payload");
    assert_eq!(status.message_id(), expected.value["message"].as_i64().unwrap() as i32);
    assert_eq!(status.session_id(), 0);
    assert_eq!(sim.shared.misbehavior(peer.handle.id()).unwrap().0, 0);
}

/// Maps the fixture's session aliases to the runtime's session ids by first appearance. A
/// numeric session value is compared as is. The runtime's id must also equal the id in Core's
/// payload, which the replay's random source scripts.
#[derive(Default)]
struct SessionMap(BTreeMap<String, i32>);

impl SessionMap {
    fn check(&mut self, context: &str, expected: &Value, recorded: i32, actual: i32) {
        assert_eq!(actual, recorded, "{context}: session id (runtime vs Core's payload)");
        if let Some(number) = expected.as_i64() {
            assert_eq!(i64::from(actual), number, "{context}: session id");
            return;
        }
        let alias = expected.as_str().expect("session alias");
        assert_ne!(actual, 0, "{context}: session id for {alias}");
        if !self.0.contains_key(alias) {
            assert!(
                !self.0.values().any(|id| *id == actual),
                "{context}: session id {actual} reused for {alias}"
            );
            self.0.insert(alias.to_owned(), actual);
        }
        assert_eq!(actual, self.0[alias], "{context}: session id for {alias}");
    }
}

fn tracked(command: &str, payload: &[u8]) -> bool {
    matches!(command, "dssu" | "dsf" | "dsc" | "dsq" | "notfound")
        || (command == "inv" && is_dsq_inv_payload(payload))
}

/// A session message as the fixture's `expect` writes it, with its session id apart.
fn session_projection(command: &str, payload: &[u8]) -> (Value, i32) {
    match command {
        "dssu" => {
            let status: DsStatusUpdate = decode(payload);
            let value = serde_json::json!({
                "message": status.message_id(),
                "state": status.state(),
                "status": status.status_update(),
            });
            (value, status.session_id())
        }
        "dsf" => {
            let session = i32::from_le_bytes(payload[..4].try_into().unwrap());
            let tx = payload[4..].iter().map(|b| format!("{b:02x}")).collect::<String>();
            (serde_json::json!({ "tx": tx }), session)
        }
        "dsc" => {
            let session = i32::from_le_bytes(payload[..4].try_into().unwrap());
            let message = i32::from_le_bytes(payload[4..8].try_into().unwrap());
            (serde_json::json!({ "message": message }), session)
        }
        other => panic!("not a session message: {other}"),
    }
}

fn without_session(value: &Value) -> Value {
    let mut value = value.clone();
    if let Some(object) = value.as_object_mut() {
        object.remove("session");
    }
    value
}

/// The distinct inventories Core announced, in first-announcement order, with each `expect`'s
/// alias for them and the step of the first announcement.
fn recorded_inventory(fixture: &Fixture) -> Vec<(Inventory, String, usize)> {
    let mut announced: Vec<(Inventory, String, usize)> = Vec::new();
    let mut last = None;
    for (record, record_step) in fixture.records.iter().zip(record_steps(fixture)) {
        match record {
            Record::Message(message)
                if message.direction == Direction::FromNode && message.command == "inv" =>
            {
                let items = decode::<dash_p2p_core::Inv>(&message.payload).inventory().clone();
                last = Some((items, record_step));
            }
            Record::Expect(expect) if expect.command == "inv" => {
                let (items, step) = last.take().expect("inv expect follows its inv");
                let aliases = expect.value.as_array().expect("inv expect items");
                assert_eq!(items.len(), aliases.len(), "{}: inv expect length", fixture.name);
                for (item, alias) in items.into_iter().zip(aliases) {
                    assert_eq!(alias[0].as_u64(), Some(u64::from(inv_type_code(&item))));
                    if !announced.iter().any(|(known, ..)| *known == item) {
                        announced.push((item, alias[1].as_str().unwrap().to_owned(), step));
                    }
                }
            }
            _ => {}
        }
    }
    announced
}

/// What the replay saw the runtime do through its peer sink and mempool.
#[derive(Default)]
struct Effects {
    relayed_queues: Vec<DsQueue>,
    relayed_inventory: Vec<Inventory>,
    /// Each transaction relay, with the step it happened in.
    relayed_transactions: Vec<(TxHash, usize)>,
    sink_unexpected: Vec<PeerCall>,
}

/// Replays a masternode fixture record by record through the Sim and the server runtime.
///
/// - Peers connect with their recorded `version`; every other message goes to the processor
///   (`Sim::admitted`), which must not throw, unless dashd had closed the peer.
/// - A `time` record advances the clocks one second at a time and runs the server's 1 s tick.
/// - What the runtime pushes through its peer sink reaches the peer's stream; its queue relays
///   go to the Sim's relay, which picks `dsq` or `inv` by each peer's version and `senddsq`.
/// - A `getdata` for Core's n-th announced queue asks for the runtime's n-th relayed queue: the
///   queue hash covers the masked time and signature.
/// - After each step: Misbehaving scores, closed peers, unscripted random draws, the mempool
///   (collaterals charged and final transactions) at each `getrawmempool`, and
///   `charged_count`/`charged_participants`.
/// - At the end: every peer's `dssu`/`dsf`/`dsc`/`dsq`/queue `inv`/`notfound` stream, both ways
///   and in order (the Sim's replies and the sink's pushes in one step may interleave in any
///   order); the relayed queues; each runtime queue's signature against the runtime's key; and
///   the relayed DSTX inventory.
fn replay_server_fixture(name: &str) {
    let fixture = common::transcript::fixture(name);
    check_mask(&fixture);
    pin_queue_hashes(&fixture);
    let Server {
        runtime,
        peer_calls,
        mempool_calls,
        clock,
        random,
        pool,
        active_key,
    } = server(&fixture);
    let list = masternodes(&fixture);
    let active_pro_tx = runtime.active().pro_tx_hash;
    let runtime = SharedServer::new(runtime);
    let mut sim = common::replay::sim(&fixture, runtime.clone(), true, false);
    let announced = recorded_inventory(&fixture);
    let announced_queues = announced
        .iter()
        .filter(|(inv, ..)| inv.inv_type() == InvType::Dsq)
        .map(|(inv, alias, _)| (*inv, alias.clone()))
        .collect::<Vec<_>>();
    let core_aliases = queue_aliases(&fixture);
    let mut peers: BTreeMap<u32, SimPeer> = BTreeMap::new();
    let mut streams = Streams::default();
    let mut scores = Scores::default();
    let mut closed = BTreeSet::new();
    let mut effects = Effects::default();
    let mut submitted = BTreeSet::new();
    let (mut peer_index, mut mempool_index) = (0, 0);
    let mut last_time = fixture.mocktime;
    let steps = steps(&fixture);
    for (index, step) in steps.iter().enumerate() {
        let at = format!("{name}:{}", step.line);
        random.fee_draws.lock().unwrap().extend(fee_draws(step));
        *random.offender_picks.lock().unwrap() = offender_picks(step);
        match step.record {
            Record::Accept {
                peer,
                ..
            } => {
                let accepted = sim.accept(u8::try_from(*peer).unwrap(), Permissions::NONE);
                peers.insert(*peer, accepted.unwrap_or_else(|| panic!("{at}: accept")));
                runtime.lock().peers_mut().connected.insert(service(*peer));
            }
            Record::Time(time) => {
                for second in (last_time + 1)..=*time {
                    clock.set(second);
                    sim.clock.set_mock_time(second);
                    runtime.lock().tick(false);
                }
                last_time = last_time.max(*time);
            }
            Record::Rpc(rpc) => match rpc.method.as_str() {
                "controller.protx-revoke-and-mine" => {
                    let revoked = rpc.params[0].as_str().unwrap().parse().unwrap();
                    let mut runtime = runtime.lock();
                    let mns = runtime.mns_mut();
                    let mn = mns.valid.remove(&revoked).expect("the revoked masternode");
                    mns.by_collateral.remove(&mn.collateral);
                    mns.enabled -= 1;
                }
                "getrawmempool" | "getcoinjoininfo" => {}
                other => panic!("{at}: no replay for rpc {other}"),
            },
            Record::Message(message) => {
                assert_eq!(message.direction, Direction::ToNode);
                let peer = &peers[&message.peer];
                if message.echo || peer.handle.disconnect_requested() {
                    // A pong echoes the node's ping; dashd closed connections read nothing more.
                } else if matches!(message.command.as_str(), "version" | "verack") {
                    sim.deliver(peer, &[(message.command.as_str(), message.payload.clone())]);
                } else {
                    let payload = if message.command == "getdata" {
                        let request: dash_p2p_core::GetData = decode(&message.payload);
                        let inventory = request
                            .inventory()
                            .iter()
                            .map(|inv| {
                                announced_queues
                                    .iter()
                                    .position(|(queue, _)| queue == inv)
                                    .and_then(|n| effects.relayed_queues.get(n))
                                    .map_or(*inv, |queue| {
                                        Inventory::new(InvType::Dsq, queue.hash())
                                    })
                            })
                            .collect::<Vec<_>>();
                        let mut payload = Vec::new();
                        dash_p2p_core::GetData::new(inventory).encode(&mut payload);
                        payload
                    } else {
                        message.payload.clone()
                    };
                    assert_eq!(
                        sim.admitted(peer, &message.command, payload),
                        Ok(()),
                        "{at}: {} threw (no fixture logs an exception)",
                        message.command
                    );
                }
            }
            other => unreachable!("{at}: {other:?} does not drive a step"),
        }
        sim.pass();
        streams.take_sim(&sim, &peers, index, Source::Sim, tracked);
        let calls = peer_calls.lock().unwrap()[peer_index..].to_vec();
        peer_index += calls.len();
        for call in calls {
            match call {
                PeerCall::PushToAddr(address, message) => {
                    let peer = u32::from(address.port - 40000);
                    if tracked(message.command.as_str(), &message.payload) {
                        streams.push(
                            peer,
                            index,
                            Source::Sink,
                            message.command.as_str(),
                            message.payload,
                        );
                    }
                }
                PeerCall::RelayDsq(queue) => {
                    sim.shared.relay_dsq(&queue);
                    effects.relayed_queues.push(queue);
                }
                PeerCall::RelayInv(inv) | PeerCall::RelayTxInv(inv) => {
                    effects.relayed_inventory.push(inv)
                }
                PeerCall::Misbehaving(..) | PeerCall::PushTo(..) => {
                    effects.sink_unexpected.push(call)
                }
                PeerCall::RelayTransaction(id) => effects.relayed_transactions.push((id, index)),
                PeerCall::EraseObjectRequest(..) | PeerCall::ForgetObjectRequest(_) => {}
            }
        }
        sim.pass();
        streams.take_sim(&sim, &peers, index, Source::Sink, tracked);
        let calls = mempool_calls.lock().unwrap()[mempool_index..].to_vec();
        mempool_index += calls.len();
        for call in calls {
            if let MempoolCall::Submit(tx) = call
                && pool.accepted.contains(&txid(&tx).to_string())
            {
                submitted.insert(txid(&tx).to_string());
            }
        }

        // Core relays each consumed collateral right before it logs the consumption
        // (`server.cpp:619-628`).
        let core_relays = step
            .logs()
            .iter()
            .filter(|log| log.contains("ConsumeCollateral -- Collateral was consumed"))
            .count();
        let relays = effects.relayed_transactions.iter().filter(|(_, at)| *at == index).count();
        assert_eq!(relays, core_relays, "{at}: collateral relays (runtime vs Core)");
        assert_eq!(
            std::mem::take(&mut *random.unscripted.lock().unwrap()),
            Vec::<u32>::new(),
            "{at}: draws Core did not make"
        );
        assert_eq!(
            std::mem::take(&mut *random.fee_draws.lock().unwrap()),
            VecDeque::<u32>::new(),
            "{at}: draws Core made that the runtime did not"
        );
        assert_eq!(scores.deltas(&sim, &peers), step.score_deltas(), "{at}: score changes");
        closed.extend(step.closed());
        assert_eq!(disconnected(&peers), closed, "{at}: closed peers");
        for peer in &closed {
            runtime.lock().peers_mut().connected.remove(&service(*peer));
        }
        if let Record::Rpc(rpc) = step.record {
            match rpc.method.as_str() {
                "getrawmempool" => {
                    let snapshot = rpc
                        .result
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|id| id.as_str().unwrap().to_owned())
                        .collect::<BTreeSet<_>>();
                    assert_eq!(submitted, snapshot, "{at}: mempool (runtime vs Core)");
                }
                "getcoinjoininfo" => {
                    let info = info_value(&runtime.lock().info());
                    assert_eq!(info, rpc.result, "{at}: getcoinjoininfo (runtime vs Core)");
                }
                _ => {}
            }
        }
        let charged = pool
            .participants
            .iter()
            .filter(|(_, id)| submitted.contains(*id))
            .map(|(participant, _)| Value::from(participant.as_str()))
            .collect::<Vec<_>>();
        for value in step.observed("charged_count") {
            assert_eq!(Some(charged.len() as u64), value.as_u64(), "{at}: charged count");
        }
        for value in step.observed("charged_participants") {
            assert_eq!(Value::Array(charged.clone()), value, "{at}: charged participants");
        }
        for value in step.observed("dstx_map_size") {
            let runtime = runtime.lock();
            let kept = submitted.iter().filter(|id| runtime.dstx().contains(&id.parse().unwrap()));
            assert_eq!(Some(kept.count() as u64), value.as_u64(), "{at}: DSTX map size");
        }
        for value in step.observed("completion_ids") {
            let completions = streams
                .0
                .values()
                .flatten()
                .filter(|out| out.command == "dsc")
                .map(|out| session_projection("dsc", &out.payload).0["message"].clone())
                .collect::<Vec<_>>();
            assert_eq!(Value::Array(completions), value, "{at}: completion messages");
        }
        for value in step.observed("coinjoin") {
            assert_eq!(info_value(&runtime.lock().info()), value, "{at}: coinjoin info");
        }
    }

    let queue_alias = |inv: &Inventory| {
        if inv.inv_type() != InvType::Dsq {
            return None;
        }
        effects
            .relayed_queues
            .iter()
            .position(|queue| queue.hash() == inv.inv_hash())
            .and_then(|n| announced_queues.get(n))
            .map(|(_, alias)| alias.clone())
            .or_else(|| core_aliases.get(&wire_hex(inv)).cloned())
    };
    // A queue verifies under the runtime's test key when the runtime signed it, or under the
    // listed operator key when it is a recorded queue the node received and relays.
    let verify = |queue: &DsQueue| {
        let listed = list.iter().find(|mn| mn.pro_tx_hash == queue.pro_tx_hash());
        let own = (queue.pro_tx_hash() == active_pro_tx).then_some(&active_key);
        own.into_iter()
            .chain(listed.map(|mn| &mn.operator_key))
            .any(|key| matches!(check_queue_signature(queue, key), BlsVerdict::Valid))
    };
    let project = |out: &Out| match out.command.as_str() {
        "dsq" => (dsq_projection(&out.payload, queue_alias, verify), None),
        "inv" | "notfound" => (inventory_projection(&out.payload, queue_alias), None),
        command => {
            let (value, session) = session_projection(command, &out.payload);
            (value, Some(session))
        }
    };
    let recorded = recorded_streams(&fixture, |message| {
        matches!(message.command.as_str(), "dssu" | "dsf" | "dsc" | "dsq" | "notfound")
            || is_dsq_inv(message)
    });
    let mut sessions = SessionMap::default();
    let mut matches = |entry: &Recorded, out: &Out| {
        entry.command == out.command && project(out).0 == without_session(&entry.value)
    };
    let mut check = |entry: &Recorded, out: &Out, context: &str| {
        let (value, session) = project(out);
        assert_eq!(value, without_session(&entry.value), "{context}");
        if let Some(session) = session {
            let recorded = i32::from_le_bytes(entry.payload[..4].try_into().unwrap());
            sessions.check(context, &entry.value["session"], recorded, session);
        }
    };
    // Core trickles inventory (`net_processing.cpp` `SendMessages`), while the Sim announces a
    // relay at once, so announcements and direct messages are two streams per peer. Core sends
    // direct messages while it handles the driving record, so they are pinned to its step (up
    // to the capture's lag, see `StepRule::Direct`); an announcement may come no later than
    // Core's.
    let drivers = step_drivers(&steps);
    let [announcements, direct] = split_streams(&recorded, &streams, |command| command == "inv");
    for ((recorded, actual), rule) in
        [(announcements, StepRule::NotLater), (direct, StepRule::Direct(&drivers))]
    {
        compare_streams(name, &recorded, &actual, rule, &mut matches, &mut check);
    }

    // Each relayed queue is the announced one, and the runtime signed its own queues.
    let relayed = effects
        .relayed_queues
        .iter()
        .map(|queue| {
            let mut payload = Vec::new();
            queue.encode(&mut payload);
            dsq_projection(&payload, queue_alias, verify)
        })
        .collect::<Vec<_>>();
    let recorded_queues = announced_queues
        .iter()
        .map(|(_, alias)| {
            let body = hex_value(&fixture.facts["queue_bodies"][alias]["body"]);
            dsq_projection(&body, |_| Some(alias.clone()), |_| true)
        })
        .collect::<Vec<_>>();
    assert_eq!(relayed, recorded_queues, "{name}: relayed queues (runtime vs fixture)");
    let received = fixture
        .records
        .iter()
        .filter_map(|record| match record {
            Record::Message(message)
                if message.direction == Direction::ToNode && message.command == "dsq" =>
            {
                Some(message.payload.as_slice())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    for queue in &effects.relayed_queues {
        let mut payload = Vec::new();
        queue.encode(&mut payload);
        if !received.contains(&payload.as_slice()) {
            assert_eq!(
                queue.pro_tx_hash(),
                active_pro_tx,
                "{name}: a queue the node never received"
            );
            assert!(
                matches!(check_queue_signature(queue, &active_key), BlsVerdict::Valid),
                "{name}: the runtime's queue signature"
            );
        }
    }

    // Core announces DSTX inventory to each peer from the relay; the sink's relays are compared
    // with the distinct announcements.
    let announced_other = announced
        .iter()
        .filter(|(inv, ..)| !matches!(inv.inv_type(), InvType::Dsq | InvType::Tx))
        .map(|(inv, ..)| *inv)
        .collect::<Vec<_>>();
    assert_eq!(
        effects.relayed_inventory, announced_other,
        "{name}: relayed inventory (runtime vs fixture)"
    );
    // Core relays a charged collateral once its submission succeeds (`server.cpp:619-628`), and
    // the relay reaches each peer as a trickled `MSG_TX` announcement. Each step's relay count
    // was compared above with Core's consumption logs. Here the runtime's relays, each named by
    // its participant, must be the charged set, each must have been submitted, and Core's
    // distinct announcements must be the same participants in the same order, each relayed no
    // later than Core announced it. Only the announcements of relays at the end of a capture
    // may be missing: `server-empty-entry.txt` ends (:149) before Core trickles the collateral
    // it consumed at :147.
    let participant = |id: &str| {
        pool.participants
            .iter()
            .find(|(_, collateral)| collateral.as_str() == id)
            .map_or_else(|| id.to_owned(), |(participant, _)| participant.clone())
    };
    let relayed_tx = effects
        .relayed_transactions
        .iter()
        .map(|(id, step)| {
            let id = id.to_string();
            assert!(submitted.contains(&id), "{name}: relayed {id} was not submitted");
            (participant(&id), *step)
        })
        .collect::<Vec<_>>();
    let announced_tx = announced
        .iter()
        .filter(|(inv, ..)| inv.inv_type() == InvType::Tx)
        .map(|(inv, alias, step)| {
            assert_eq!(participant(&tx_hash(inv).to_string()), *alias, "{name}: announced {alias}");
            (alias.clone(), *step)
        })
        .collect::<Vec<_>>();
    let charged = pool
        .participants
        .iter()
        .filter(|(_, id)| submitted.contains(*id))
        .map(|(participant, _)| participant.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        relayed_tx.iter().map(|(alias, _)| alias.clone()).collect::<BTreeSet<_>>(),
        charged,
        "{name}: relayed transactions (runtime) vs charged collaterals"
    );
    assert!(
        announced_tx.len() <= relayed_tx.len(),
        "{name}: Core announced {announced_tx:?}, the runtime relayed {relayed_tx:?}"
    );
    for ((alias, runtime), (announced, core)) in relayed_tx.iter().zip(&announced_tx) {
        assert_eq!(alias, announced, "{name}: relayed transactions (runtime vs Core's MSG_TX)");
        assert!(runtime <= core, "{name}: {alias} relayed in step {runtime}, Core in step {core}");
    }
    let last_step = steps.len() - 1;
    for (alias, runtime) in &relayed_tx[announced_tx.len()..] {
        assert!(
            steps[*runtime + 1..=last_step]
                .iter()
                .all(|step| matches!(step.record, Record::Rpc(_))),
            "{name}: {alias} relayed in step {runtime}, Core never announced it"
        );
    }
    assert!(
        effects.sink_unexpected.is_empty(),
        "{name}: sink effects {:?}",
        effects.sink_unexpected
    );
}

#[test]
fn server_empty_entry_replay_differential() {
    replay_server_fixture("server-empty-entry");
}

#[test]
fn server_mn_list_replay_differential() {
    replay_server_fixture("server-mn-list");
}

#[test]
fn server_recent_replay_differential() {
    replay_server_fixture("server-recent");
}

#[test]
fn server_session_replay_differential() {
    replay_server_fixture("server-session");
}

#[test]
fn server_timeout_charge_replay_differential() {
    replay_server_fixture("server-timeout-charge");
}

#[test]
fn server_timeout_no_charge_replay_differential() {
    replay_server_fixture("server-timeout-no-charge");
}

#[test]
fn dsq_modes_replay_differential() {
    replay_server_fixture("dsq-modes");
}

#[test]
fn dsq_server_intake_replay_differential() {
    replay_server_fixture("dsq-server-intake");
}
