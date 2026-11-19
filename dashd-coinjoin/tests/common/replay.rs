//! Shared pieces of the transcript replays: fixture decoding, record steps, and the per-peer
//! comparison of what the runtime sent with what dashd sent.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use dash_coinjoin::queue::MixingMasternode;
use dash_p2p_core::{InvType, Inventory};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_pkc::bls::{BlsSchemeKind, core::BlsPkValue};
use dash_primitives::{OutPoint, TxHash};
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::sim::{Sim, SimPeer};
use dashd_p2p::{NetHandler, Registry, Seams};
use serde_json::Value;

use super::transcript::{Direction, Fixture, Message, Record};

/// Decodes fixture hex.
pub fn hex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2), "odd-length hex {text:?}");
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("fixture hex"))
        .collect()
}

/// A JSON string field decoded as hex.
pub fn hex_value(value: &Value) -> Vec<u8> {
    hex(value.as_str().unwrap_or_else(|| panic!("hex string expected, got {value}")))
}

/// An inventory item from its wire type and its hash as the fixture writes it (wire byte order).
pub fn inventory(inv_type: u32, wire_hash: &str) -> Inventory {
    let mut bytes = inv_type.to_le_bytes().to_vec();
    bytes.extend(hex(wire_hash));
    Inventory::decode(&mut &bytes[..]).expect("inventory")
}

/// The wire type of an inventory item.
pub fn inv_type_code(inv: &Inventory) -> u32 {
    let mut bytes = Vec::new();
    inv.inv_type().encode(&mut bytes);
    u32::from_le_bytes(bytes.try_into().expect("inventory type width"))
}

/// The hash of an inventory item in wire byte order, as the fixture writes it.
pub fn wire_hex(inv: &Inventory) -> String {
    let mut bytes = Vec::new();
    inv.encode(&mut bytes);
    bytes[4..].iter().map(|b| format!("{b:02x}")).collect()
}

/// A transaction hash from an inventory item's hash.
pub fn tx_hash(inv: &Inventory) -> TxHash {
    let mut bytes = Vec::new();
    inv.encode(&mut bytes);
    TxHash::decode(&mut &bytes[4..]).expect("transaction hash")
}

/// The version a peer advertised in its `version` payload.
pub fn advertised_version(payload: &[u8]) -> i32 {
    i32::from_le_bytes(payload[..4].try_into().expect("version payload"))
}

/// Every fixture carries the same comparison mask; a fixture with another one needs its own
/// comparison rules before it can be replayed.
pub fn check_mask(fixture: &Fixture) {
    let keys = fixture
        .mask
        .as_object()
        .unwrap_or_else(|| panic!("{}: mask is not an object", fixture.name))
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        keys,
        ["background", "collateral", "dsq", "dstx", "inventory", "logs", "session"]
            .into_iter()
            .collect(),
        "{}: unknown mask",
        fixture.name
    );
    assert!(fixture.mask["dsq"].as_str().unwrap().starts_with("time and BLS signature omitted"));
    assert!(
        fixture.mask["dstx"].as_str().unwrap().starts_with("BLS signature and sigTime omitted")
    );
}

/// The fixture's `mn_list` entries.
pub fn masternodes(fixture: &Fixture) -> Vec<MixingMasternode> {
    fixture.facts["mn_list"]
        .as_array()
        .unwrap_or_else(|| panic!("{}: mn_list", fixture.name))
        .iter()
        .map(|value| {
            let key: [u8; 48] = hex_value(&value["state"]["pubKeyOperator"])
                .try_into()
                .expect("operator key length");
            MixingMasternode {
                pro_tx_hash: value["proTxHash"].as_str().unwrap().parse().unwrap(),
                collateral: OutPoint::new(
                    value["collateralHash"].as_str().unwrap().parse().unwrap(),
                    u32::try_from(value["collateralIndex"].as_u64().unwrap()).unwrap(),
                ),
                operator_key: BlsPkValue::from_canonical(&key, BlsSchemeKind::Basic)
                    .expect("operator key"),
            }
        })
        .collect()
}

/// The enabled masternode count Core used: the `mn_list` entries without a PoSe ban, and the
/// `fifth_mn` registration when the fixture records one.
pub fn enabled_count(fixture: &Fixture) -> i32 {
    let mut enabled = fixture.facts["mn_list"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|value| value["state"]["PoSeBanHeight"].as_i64() == Some(-1))
        .map(|value| value["proTxHash"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    if let Some(fifth) = fixture.facts.get("fifth_mn") {
        enabled.insert(fifth["protx"].as_str().unwrap().to_owned());
    }
    i32::try_from(enabled.len()).unwrap()
}

/// A Sim for the target node, at the fixture's mock time and initial-download state.
pub fn sim(
    fixture: &Fixture,
    handler: impl NetHandler + 'static,
    masternode: bool,
    blocks_only: bool,
) -> Sim {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let mut identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    identity.masternode = masternode;
    let mut registry = Registry::new();
    registry.register(Box::new(handler));
    let sim = Sim::with(
        params,
        identity,
        ProcessorOptions {
            blocks_only,
            ..ProcessorOptions::default()
        },
        registry,
        Seams::default(),
        |_| {},
    );
    sim.chain.set_ibd(fixture.ibd);
    sim.clock.set_mock_time(fixture.mocktime);
    sim
}

/// A record that drives the node (a peer connecting, a message to the node, a time step or an
/// RPC), with the records that report what followed it, up to the next driving record.
pub struct Step<'a> {
    pub line: usize,
    pub record: &'a Record,
    pub span: Vec<(usize, &'a Record)>,
}

impl Step<'_> {
    /// The peer whose message or connection drives this step.
    pub fn peer(&self) -> Option<u32> {
        match self.record {
            Record::Accept {
                peer,
                ..
            } => Some(*peer),
            Record::Message(message) => Some(message.peer),
            _ => None,
        }
    }

    /// The `observe` values with this key, in order.
    pub fn observed(&self, key: &str) -> Vec<Value> {
        self.span
            .iter()
            .filter_map(|(_, record)| match record {
                Record::Observe(observation) if observation.key == key => {
                    Some(observation.value.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// The Core log lines that follow this step.
    pub fn logs(&self) -> Vec<&str> {
        self.span
            .iter()
            .filter_map(|(_, record)| match record {
                Record::Log(log) => Some(log.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The peers dashd closed in this step.
    pub fn closed(&self) -> BTreeSet<u32> {
        self.span
            .iter()
            .filter_map(|(_, record)| match record {
                Record::Message(message)
                    if message.direction == Direction::FromNode && message.command == "closed" =>
                {
                    Some(message.peer)
                }
                Record::Expect(expect) if expect.command == "closed" => {
                    assert_eq!(expect.value, Value::Bool(true), "closed expectation");
                    Some(expect.peer)
                }
                _ => None,
            })
            .collect()
    }

    /// The step's score changes: each `score_delta` belongs to the peer whose message drove it.
    pub fn score_deltas(&self) -> BTreeMap<u32, i32> {
        let mut deltas = BTreeMap::new();
        for value in self.observed("score_delta") {
            let peer = self.peer().expect("a score follows a peer's message");
            *deltas.entry(peer).or_default() += i32::try_from(value.as_i64().unwrap()).unwrap();
        }
        deltas
    }
}

fn drives(record: &Record) -> bool {
    match record {
        Record::Accept {
            ..
        }
        | Record::Time(_)
        | Record::Rpc(_) => true,
        Record::Message(message) => message.direction == Direction::ToNode,
        Record::Expect(_) | Record::Log(_) | Record::Observe(_) => false,
    }
}

/// The index of the step each record belongs to, in record order: a driving record starts a
/// step, and every other record belongs to the step before it.
pub fn record_steps(fixture: &Fixture) -> Vec<usize> {
    let mut step = None;
    fixture
        .records
        .iter()
        .zip(&fixture.lines)
        .map(|(record, line)| {
            if drives(record) {
                step = Some(step.map_or(0, |n: usize| n + 1));
            }
            step.unwrap_or_else(|| panic!("{}:{line}: output before any input", fixture.name))
        })
        .collect()
}

/// The fixture's records as steps. Records before the first driving record are not expected.
pub fn steps(fixture: &Fixture) -> Vec<Step<'_>> {
    let mut steps: Vec<Step<'_>> = Vec::new();
    for (record, line) in fixture.records.iter().zip(&fixture.lines) {
        if drives(record) {
            steps.push(Step {
                line: *line,
                record,
                span: Vec::new(),
            });
        } else {
            steps
                .last_mut()
                .unwrap_or_else(|| panic!("{}:{line}: output before any input", fixture.name))
                .span
                .push((*line, record));
        }
    }
    steps
}

/// One message dashd sent a peer, with the `expect` record describing it.
#[derive(Clone, Debug)]
pub struct Recorded {
    pub line: usize,
    /// The index of the step whose span holds the message.
    pub step: usize,
    pub command: String,
    pub payload: Vec<u8>,
    pub value: Value,
}

/// The messages each peer received from dashd, in order, for the messages `tracked` selects,
/// each paired with the `expect` that follows it. Every `expect` pairs with the oldest unpaired
/// message of its peer and command, tracked or not, and an `expect` with none fails.
pub fn recorded_streams(
    fixture: &Fixture,
    tracked: impl Fn(&Message) -> bool,
) -> BTreeMap<u32, Vec<Recorded>> {
    struct Pending {
        line: usize,
        step: usize,
        payload: Vec<u8>,
        /// The message's place in its peer's stream, when it is tracked.
        slot: Option<usize>,
    }
    let mut pending = BTreeMap::<(u32, String), VecDeque<Pending>>::new();
    let mut streams = BTreeMap::<u32, Vec<Option<Recorded>>>::new();
    let records = fixture.records.iter().zip(&fixture.lines).zip(record_steps(fixture));
    for ((record, line), step) in records {
        match record {
            Record::Message(message) if message.direction == Direction::FromNode => {
                let slot = tracked(message).then(|| {
                    let stream = streams.entry(message.peer).or_default();
                    stream.push(None);
                    stream.len() - 1
                });
                pending.entry((message.peer, message.command.clone())).or_default().push_back(
                    Pending {
                        line: *line,
                        step,
                        payload: message.payload.clone(),
                        slot,
                    },
                );
            }
            Record::Expect(expect) => {
                let key = (expect.peer, expect.command.clone());
                let message =
                    pending.get_mut(&key).and_then(VecDeque::pop_front).unwrap_or_else(|| {
                        panic!(
                            "{}:{line}: expect {} for peer {} follows no such message",
                            fixture.name, expect.command, expect.peer
                        )
                    });
                if let Some(slot) = message.slot {
                    streams.get_mut(&expect.peer).unwrap()[slot] = Some(Recorded {
                        line: message.line,
                        step: message.step,
                        command: expect.command.clone(),
                        payload: message.payload,
                        value: expect.value.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    streams
        .into_iter()
        .map(|(peer, stream)| {
            let stream = stream
                .into_iter()
                .map(|entry| {
                    entry.unwrap_or_else(|| {
                        panic!("{}: a message to peer {peer} without its expect", fixture.name)
                    })
                })
                .collect();
            (peer, stream)
        })
        .collect()
}

/// Whether a message is an `inv` announcing queues.
pub fn is_dsq_inv(message: &Message) -> bool {
    message.command == "inv" && is_dsq_inv_payload(&message.payload)
}

/// Where a runtime output came from. Within one step, the Sim's replies and what reached the
/// peer through the runtime's peer sink are two ordered channels whose interleaving the replay
/// cannot observe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Sim,
    Sink,
}

/// One message the runtime sent a peer.
#[derive(Clone, Debug)]
pub struct Out {
    pub step: usize,
    pub source: Source,
    pub command: String,
    pub payload: Vec<u8>,
}

/// The runtime's messages per peer, in the order each channel produced them.
#[derive(Default)]
pub struct Streams(pub BTreeMap<u32, Vec<Out>>);

impl Streams {
    pub fn push(
        &mut self,
        peer: u32,
        step: usize,
        source: Source,
        command: &str,
        payload: Vec<u8>,
    ) {
        self.0.entry(peer).or_default().push(Out {
            step,
            source,
            command: command.to_owned(),
            payload,
        });
    }

    /// Takes what the Sim delivered to each peer since the last call.
    pub fn take_sim(
        &mut self,
        sim: &Sim,
        peers: &BTreeMap<u32, SimPeer>,
        step: usize,
        source: Source,
        tracked: impl Fn(&str, &[u8]) -> bool,
    ) {
        for (id, peer) in peers {
            for (command, payload) in sim.sent(peer) {
                if tracked(command.as_str(), &payload) {
                    self.push(*id, step, source, command.as_str(), payload);
                }
            }
        }
    }
}

/// Whether an `inv` payload announces queues only.
pub fn is_dsq_inv_payload(payload: &[u8]) -> bool {
    dash_p2p_core::Inv::decode(&mut &payload[..])
        .is_ok_and(|inv| inv.inventory().iter().all(|item| item.inv_type() == InvType::Dsq))
}

/// Recorded direct messages the capture took down a step after the one Core sent them in, by
/// fixture name and recorded line. The capture reads each peer's socket on its own, so it may
/// record a message only after another peer's later message.
const CAPTURE_LAGS: &[(&str, usize)] = &[
    // server-timeout-charge.txt:176 `received: dsa ... peer=5`, :191 `sending dssu ... peer=5`.
    ("server-timeout-charge", 168),
];

/// When the runtime must send a message, relative to the step whose span holds Core's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepRule<'a> {
    /// Direct messages: Core sends them while it handles the driving record, so in the same
    /// step. An earlier runtime step is accepted only for an entry of `CAPTURE_LAGS`, and only
    /// when every later step up to Core's is driven by another peer's connection or message.
    /// The slice holds each step's driving peer, as [`Step::peer`] gives it.
    Direct(&'a [Option<u32>]),
    /// Announcements, in the same step or earlier: Core trickles them, the Sim announces at
    /// once.
    NotLater,
}

impl StepRule<'_> {
    fn holds(self, peer: u32, runtime: usize, core: usize, lag: bool) -> bool {
        match self {
            Self::Direct(drivers) => {
                runtime == core
                    || (lag
                        && runtime < core
                        && drivers[runtime + 1..=core]
                            .iter()
                            .all(|driver| driver.is_some_and(|other| other != peer)))
            }
            Self::NotLater => runtime <= core,
        }
    }
}

/// The driving peer of each step, for [`StepRule::Direct`].
pub fn step_drivers(steps: &[Step<'_>]) -> Vec<Option<u32>> {
    steps.iter().map(Step::peer).collect()
}

/// Compares each peer's runtime messages with what dashd sent it, in order and in both
/// directions, and each message's step by `rule`. Within a step the two runtime channels may
/// interleave in any order; `matches` chooses between them, then `check` asserts the fields.
pub fn compare_streams(
    name: &str,
    recorded: &BTreeMap<u32, Vec<Recorded>>,
    actual: &Streams,
    rule: StepRule<'_>,
    mut matches: impl FnMut(&Recorded, &Out) -> bool,
    mut check: impl FnMut(&Recorded, &Out, &str),
) {
    let lags = match rule {
        StepRule::Direct(_) => CAPTURE_LAGS
            .iter()
            .filter(|(fixture, _)| *fixture == name)
            .map(|(_, line)| *line)
            .collect::<BTreeSet<_>>(),
        StepRule::NotLater => BTreeSet::new(),
    };
    let mut used = BTreeSet::new();
    let peers = recorded.keys().chain(actual.0.keys()).copied().collect::<BTreeSet<_>>();
    for peer in peers {
        let expected = recorded.get(&peer).map_or(&[][..], Vec::as_slice);
        let outs = actual.0.get(&peer).map_or(&[][..], Vec::as_slice);
        let mut next = expected.iter();
        let mut start = 0;
        while start < outs.len() {
            let step = outs[start].step;
            let end = start + outs[start..].iter().take_while(|out| out.step == step).count();
            let group = &outs[start..end];
            let mut channels = [Source::Sim, Source::Sink].map(|source| {
                group.iter().filter(|out| out.source == source).collect::<VecDeque<_>>()
            });
            while channels.iter().any(|channel| !channel.is_empty()) {
                let heads = channels
                    .iter()
                    .filter_map(|c| c.front())
                    .map(|o| &o.command)
                    .collect::<Vec<_>>();
                let Some(entry) = next.next() else {
                    panic!(
                        "{name}: the runtime sent peer {peer} {heads:?}, Core sent nothing more"
                    );
                };
                let channel = channels
                    .iter()
                    .position(|channel| channel.front().is_some_and(|out| matches(entry, out)))
                    .unwrap_or_else(|| {
                        panic!(
                            "{name}:{}: Core sent peer {peer} {} {}, the runtime sent {heads:?}",
                            entry.line, entry.command, entry.value
                        )
                    });
                let out = channels[channel].pop_front().unwrap();
                let lag = lags.contains(&entry.line);
                if lag && out.step < entry.step {
                    used.insert(entry.line);
                }
                assert!(
                    rule.holds(peer, out.step, entry.step, lag),
                    "{name}:{}: Core sent peer {peer} {} in step {}, the runtime in step {}",
                    entry.line,
                    entry.command,
                    entry.step,
                    out.step
                );
                check(
                    entry,
                    out,
                    &format!("{name}:{} {} to peer {peer}", entry.line, entry.command),
                );
            }
            start = end;
        }
        if let Some(entry) = next.next() {
            panic!(
                "{name}:{}: Core sent peer {peer} {} {}, the runtime did not",
                entry.line, entry.command, entry.value
            );
        }
    }
    assert_eq!(used, lags, "{name}: capture lags used (runtime vs listed)");
}

/// Splits recorded and runtime streams by command, keeping each part's per-peer order.
pub fn split_streams(
    recorded: &BTreeMap<u32, Vec<Recorded>>,
    actual: &Streams,
    first: impl Fn(&str) -> bool,
) -> [(BTreeMap<u32, Vec<Recorded>>, Streams); 2] {
    let mut parts = [(BTreeMap::new(), Streams::default()), (BTreeMap::new(), Streams::default())];
    for (peer, entries) in recorded {
        for entry in entries {
            let part = usize::from(!first(&entry.command));
            parts[part].0.entry(*peer).or_insert_with(Vec::new).push(entry.clone());
        }
    }
    for (peer, outs) in &actual.0 {
        for out in outs {
            let part = usize::from(!first(&out.command));
            parts[part].1.0.entry(*peer).or_default().push(out.clone());
        }
    }
    parts
}

/// Misbehaviour scores per fixture peer, read from the Sim after each step.
#[derive(Default)]
pub struct Scores(BTreeMap<u32, i32>);

impl Scores {
    /// The changes since the last call.
    pub fn deltas(&mut self, sim: &Sim, peers: &BTreeMap<u32, SimPeer>) -> BTreeMap<u32, i32> {
        let mut deltas = BTreeMap::new();
        for (id, peer) in peers {
            let Some((score, _)) = sim.shared.misbehavior(peer.handle.id()) else {
                continue;
            };
            let previous = self.0.insert(*id, score).unwrap_or(0);
            if score != previous {
                deltas.insert(*id, score - previous);
            }
        }
        deltas
    }
}

/// The peers the node has marked for disconnection.
pub fn disconnected(peers: &BTreeMap<u32, SimPeer>) -> BTreeSet<u32> {
    peers.iter().filter(|(_, peer)| peer.handle.disconnect_requested()).map(|(id, _)| *id).collect()
}

/// An `inv` or `notfound` payload as the fixture's `expect` writes it: each item's wire type and
/// its queue alias, or its hash when it has none.
pub fn inventory_projection(payload: &[u8], alias: impl Fn(&Inventory) -> Option<String>) -> Value {
    let items = dash_p2p_core::Inv::decode(&mut &payload[..]).expect("inventory payload");
    Value::Array(
        items
            .inventory()
            .iter()
            .map(|item| {
                Value::Array(vec![
                    Value::from(inv_type_code(item)),
                    Value::from(alias(item).unwrap_or_else(|| wire_hex(item))),
                ])
            })
            .collect(),
    )
}

/// A `dsq` payload as the fixture's `expect` writes it: time and signature are masked, the queue
/// is named by its alias, and `signature` says whether `verify` accepts it.
pub fn dsq_projection(
    payload: &[u8],
    alias: impl Fn(&Inventory) -> Option<String>,
    verify: impl Fn(&dash_p2p_core::DsQueue) -> bool,
) -> Value {
    let queue = dash_p2p_core::DsQueue::decode(&mut &payload[..]).expect("dsq payload");
    let inv = Inventory::new(InvType::Dsq, dash_types::Hashable::hash(&queue));
    let mut protx = Vec::new();
    queue.pro_tx_hash().encode(&mut protx);
    serde_json::json!({
        "denom": queue.denom(),
        "protx": protx.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        "queue": alias(&inv).unwrap_or_else(|| wire_hex(&inv)),
        "ready": queue.ready(),
        "signature": if verify(&queue) { "verified" } else { "unverified" },
    })
}

/// Checks each recorded queue body against the hash the fixture gives it, so a queue's alias
/// rests on Core's hash, not the runtime's.
pub fn pin_queue_hashes(fixture: &Fixture) {
    let aliases = fixture.facts["queues"].as_object().expect("queues fact");
    for (alias, queue) in fixture.facts["queue_bodies"].as_object().expect("queue_bodies fact") {
        let body = dash_p2p_core::DsQueue::decode(&mut &hex_value(&queue["body"])[..])
            .unwrap_or_else(|e| panic!("{}: {alias} body: {e}", fixture.name));
        let recorded = queue["hash"].as_str().unwrap();
        assert_eq!(
            wire_hex(&Inventory::new(InvType::Dsq, dash_types::Hashable::hash(&body))),
            recorded,
            "{}: {alias} hash",
            fixture.name
        );
        assert_eq!(
            aliases.get(recorded).and_then(Value::as_str),
            Some(alias.as_str()),
            "{}: {alias} in the queues fact",
            fixture.name
        );
    }
}

/// The queue aliases the `queues` fact gives Core's queue hashes.
pub fn queue_aliases(fixture: &Fixture) -> BTreeMap<String, String> {
    fixture.facts["queues"]
        .as_object()
        .expect("queues fact")
        .iter()
        .map(|(hash, alias)| (hash.clone(), alias.as_str().unwrap().to_owned()))
        .collect()
}
