//! T6 Q-4b-0: commitment key mutation goldens over the four captures
//! (`tests/data/t6-q4b-key-mutation.goldens.txt`, provenance in `tests/data/PROVENANCE.md`).
//!
//! For each `mnlistdiff` and `qrinfo` with a new quorum, the first new quorum in wire order whose
//! public key, serialized under the other scheme, is valid there and not under its version's
//! scheme gets those 48 bytes in place of its own. Each mutated payload is applied where the
//! capture's replay applies the original, and the goldens pin its commitment hash, the quorum root
//! step 4 computes for it, the store's row for it and the engine's verdict.
//!
//! The replays: regtest pins the `qrinfo` base (as [`super::apply_to`]), then applies its first
//! diff from genesis, every step diff above it and its `qrinfo`s in capture order; the transitions
//! capture its first diff from genesis and every step diff above it; testnet its diff from
//! genesis, every step diff and its `qrinfo`s in capture order. Every other diff from genesis is
//! applied to an empty engine (regtest's pinning its base), and each fetch diff where the testnet
//! replay has just applied the step diff to its base.

use std::collections::{BTreeMap, HashMap};

use dash_consensus::BlockId;
use dash_consensus::hash::sha256;
use dash_consensus::primitives::Tx;
use dash_evo::cbtx::CBTX_VERSION_MERKLE_ROOT_QUORUMS;
use dash_evo::sml::merkle_root_quorums_from_hashes;
use dash_p2p_core::{MnListDiff, QrInfo};
use dash_pkc::bls::{BlsPkBytes, BlsScChia, BlsScIetf};
use dash_primitives::{BlockHash, Commitment, commitment_bls_scheme};
use dash_types::Numeric;
use dash_types::codec::{BaseCodec, DecodeError};
use serde_json::Value;

use super::testnet::{self, TestnetChain};
use super::{
    Capture, FixtureChain, QRINFO_BASE, config_of, fixture, hex, load, read_diff, read_qrinfo,
    transitions, u32_of,
};
use crate::convert::commitment_hash;
use crate::engine::cbtx_payload;
use crate::{LlmqConfig, MasternodeListEngine, MnChain, MnReject, Mode};

const GOLDENS: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/t6-q4b-key-mutation.goldens.txt");

const PROVENANCE: &str = "# T6 Q-4b-0 key mutation goldens over \
    sb11-regtest-v24.0.captures.json.gz, sb11-regtest-transitions-v24.0.captures.json.gz, \
    sb11-testnet-v24.0.captures.json.gz and sb11-testnet-fetch-v24.0.captures.json.gz, produced \
    by dash-mnlist's src/tests/key_mutation.rs (key_mutation_goldens; T6_Q4B_WRITE_GOLDENS=1 \
    rewrites it) at 4c6c8f0506. Fields: capture, payload kind:index, the mutated quorum's \
    position, commitment_hash, quorum root or none, SHA-256 of the store's Q row or none (hex of \
    the bytes in their order), verdict.";

const REGTEST: &str = "sb11-regtest-v24.0";
const TRANSITIONS: &str = "sb11-regtest-transitions-v24.0";
const TESTNET: &str = "sb11-testnet-v24.0";
const FETCH: &str = "sb11-testnet-fetch-v24.0";
const CAPTURES: [&str; 4] = [REGTEST, TRANSITIONS, TESTNET, FETCH];

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

enum Payload {
    Diff(Box<MnListDiff>),
    QrInfo(Box<QrInfo>),
}

/// A commitment of a payload: its diff (`None` for `lastCommitmentPerIndex`), its index there.
struct Slot<'a> {
    part: Option<String>,
    index: usize,
    qc: &'a Commitment,
}

impl Payload {
    fn decode(qrinfo: bool, bytes: &[u8]) -> Result<Payload, DecodeError> {
        let mut r = bytes;
        let p = if qrinfo {
            Payload::QrInfo(Box::new(QrInfo::decode(&mut r)?))
        } else {
            Payload::Diff(Box::new(MnListDiff::decode(&mut r)?))
        };
        if !r.is_empty() {
            return Err(DecodeError::TrailingBytes {
                remaining: r.len(),
            });
        }
        Ok(p)
    }

    /// Its diffs in wire order.
    fn wire_diffs(&self) -> Vec<(String, &MnListDiff)> {
        match self {
            Payload::Diff(d) => vec![("diff".into(), &**d)],
            Payload::QrInfo(q) => {
                let mut v: Vec<(String, &MnListDiff)> = vec![
                    ("diff_tip".into(), q.diff_tip()),
                    ("diff_h".into(), q.diff_h()),
                    ("diff_h_c".into(), q.diff_h_c()),
                    ("diff_h_2c".into(), q.diff_h_2c()),
                    ("diff_h_3c".into(), q.diff_h_3c()),
                ];
                v.extend(q.cycle_h_4c().as_ref().map(|c| ("diff_h_4c".into(), c.diff())));
                v.extend(
                    q.mn_list_diff_list()
                        .iter()
                        .enumerate()
                        .map(|(k, d)| (format!("mn_list_diff_list[{k}]"), d)),
                );
                v
            }
        }
    }

    /// Every commitment in wire order: each diff's new quorums, and `lastCommitmentPerIndex`
    /// where the `qrinfo` writes it (after the H − 4C cycle, before `mnListDiffList`).
    fn wire(&self) -> Vec<Slot<'_>> {
        let mut slots = Vec::new();
        let mut last = match self {
            Payload::Diff(_) => None,
            Payload::QrInfo(q) => Some(q.last_commitment_per_index()),
        };
        for (part, d) in self.wire_diffs() {
            if part.starts_with("mn_list_diff_list")
                && let Some(l) = last.take()
            {
                slots.extend(l.iter().enumerate().map(|(index, qc)| Slot {
                    part: None,
                    index,
                    qc,
                }));
            }
            slots.extend(d.new_quorums().iter().enumerate().map(|(index, qc)| Slot {
                part: Some(part.clone()),
                index,
                qc,
            }));
        }
        if let Some(l) = last {
            slots.extend(l.iter().enumerate().map(|(index, qc)| Slot {
                part: None,
                index,
                qc,
            }));
        }
        slots
    }

    /// Its diffs in the engine's order (`apply_qrinfo`'s construction order; the tip's only while
    /// its target is an accepted header).
    fn engine_diffs(&self, chain: &dyn MnChain) -> Vec<(String, &MnListDiff)> {
        match self {
            Payload::Diff(d) => vec![("diff".into(), &**d)],
            Payload::QrInfo(q) => {
                let mut v: Vec<(String, &MnListDiff)> = q
                    .mn_list_diff_list()
                    .iter()
                    .enumerate()
                    .map(|(k, d)| (format!("mn_list_diff_list[{k}]"), d))
                    .collect();
                v.extend(q.cycle_h_4c().as_ref().map(|c| ("diff_h_4c".into(), c.diff())));
                v.push(("diff_h_3c".into(), q.diff_h_3c()));
                v.push(("diff_h_2c".into(), q.diff_h_2c()));
                v.push(("diff_h_c".into(), q.diff_h_c()));
                v.push(("diff_h".into(), q.diff_h()));
                if chain.lookup(&q.diff_tip().block_hash()).expect("ok").is_some() {
                    v.push(("diff_tip".into(), q.diff_tip()));
                }
                v
            }
        }
    }

    fn apply(
        &self,
        engine: &mut MasternodeListEngine,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<(), MnReject> {
        match self {
            Payload::Diff(d) => engine.apply_diff(&read_diff(d), chain, cfg).map(drop),
            Payload::QrInfo(q) => engine.apply_qrinfo(&read_qrinfo(q), chain, cfg).map(drop),
        }
    }
}

/// A capture payload, where it comes from.
struct Item {
    capture: &'static str,
    label: String,
    bytes: Vec<u8>,
    payload: Payload,
}

impl Item {
    fn new(capture: &'static str, label: String, qrinfo: bool, payload: &Value) -> Item {
        let bytes = hex(payload.as_str().expect("a hex payload"));
        let payload = Payload::decode(qrinfo, &bytes).expect("Core's payload");
        Item {
            capture,
            label,
            bytes,
            payload,
        }
    }
}

/// A quorum set as step 4 builds it: (type, quorum hash) to commitment hash.
type Set = BTreeMap<(u8, [u8; 32]), [u8; 32]>;

/// The root step 4 computes: `None` below CbTx version 2 (no root is computed), else the root or
/// `None` when the hashes give none.
type Root = Option<Option<[u8; 32]>>;

/// The quorum set after each diff the replay applied, by target block: the bases step 4 starts
/// from.
struct Sets {
    genesis: BlockHash,
    at: HashMap<BlockHash, Set>,
}

/// Step 4 over one diff: its part of the payload, its target, the resulting set and root, and the
/// root its CbTx commits to.
struct Step4 {
    part: String,
    target: BlockHash,
    set: Set,
    root: Root,
    cores: Option<[u8; 32]>,
}

impl Sets {
    /// Step 4 over each of `p`'s diffs in the engine's order, each from its base's set (a diff of
    /// the same `qrinfo` first, as the engine's staged lists).
    fn step4(&self, p: &Payload, chain: &dyn MnChain) -> Vec<Step4> {
        let mut staged: HashMap<BlockHash, Set> = HashMap::new();
        let mut out = Vec::new();
        for (part, d) in p.engine_diffs(chain) {
            let base = d.base_block_hash();
            let mut set = if base.as_bytes() == &[0; 32] || base == self.genesis {
                Set::new()
            } else {
                staged
                    .get(&base)
                    .or_else(|| self.at.get(&base))
                    .unwrap_or_else(|| panic!("{part}: no set at the base {base:?}"))
                    .clone()
            };
            for q in d.deleted_quorums() {
                set.remove(&(q.llmq_type().to_base(), *q.quorum_hash().as_bytes()));
            }
            let read = read_diff(d);
            for qc in read.bls().new_quorum_reads() {
                let hash = commitment_hash(&qc);
                set.insert((qc.llmq_type().to_base(), *qc.quorum_hash().as_bytes()), hash);
            }
            let cbtx = cbtx_payload(&Tx::new(d.cb_tx().clone())).expect("a CbTx");
            let root = (cbtx.version().to_base() >= CBTX_VERSION_MERKLE_ROOT_QUORUMS).then(|| {
                merkle_root_quorums_from_hashes(set.values().copied().collect())
                    .ok()
                    .map(|r| *r.as_bytes())
            });
            let cores = cbtx.merkle_root_quorums().map(|r| *r.as_bytes());
            staged.insert(d.block_hash(), set.clone());
            out.push(Step4 {
                part,
                target: d.block_hash(),
                set,
                root,
                cores,
            });
        }
        out
    }

    /// The sets after Core's `p`, each rooted one checked against its CbTx; `record` keeps them.
    fn original(&mut self, p: &Payload, chain: &dyn MnChain, record: bool) {
        for Step4 {
            part,
            target,
            set,
            root,
            cores,
        } in self.step4(p, chain)
        {
            if let Some(root) = root {
                assert_eq!(root, cores, "{part}: Core's diff has its CbTx's root");
            }
            if record {
                match self.at.get(&target) {
                    Some(held) => assert_eq!(held, &set, "{part}: one set per block"),
                    None => {
                        self.at.insert(target, set);
                    }
                }
            }
        }
    }
}

/// The 48 bytes replacing `qc`'s key: the key under the other scheme, when they are a valid key
/// there and not under the version's.
fn other_scheme_key(qc: &Commitment) -> Option<[u8; 48]> {
    let scheme = commitment_bls_scheme(qc.version());
    let bytes = qc.quorum_public_key().to_bytes(scheme.opposite());
    let key = BlsPkBytes::from_bytes(bytes);
    let valid = |legacy: bool| {
        if legacy {
            key.is_canonical::<BlsScChia>() && key.to::<BlsScChia>().is_ok()
        } else {
            key.is_canonical::<BlsScIetf>() && key.to::<BlsScIetf>().is_ok()
        }
    };
    (!valid(scheme.is_legacy()) && valid(scheme.opposite().is_legacy())).then_some(bytes)
}

fn encoded(qc: &Commitment) -> Vec<u8> {
    let mut v = Vec::new();
    Commitment::encode(qc, &mut v);
    v
}

fn occurrences(hay: &[u8], needle: &[u8]) -> Vec<usize> {
    hay.windows(needle.len()).enumerate().filter(|(_, w)| *w == needle).map(|(i, _)| i).collect()
}

/// A payload's mutation: none without a new quorum (not counted), skipped when no new quorum's key
/// qualifies, else the mutated bytes and the mutated slot's index in [`Payload::wire`].
enum Mutation {
    NoNewQuorum,
    Skipped,
    Mutated {
        slot: usize,
        position: String,
        bytes: Vec<u8>,
    },
}

fn mutate(item: &Item) -> Mutation {
    let wire = item.payload.wire();
    if wire.iter().all(|s| s.part.is_none()) {
        return Mutation::NoNewQuorum;
    }
    let Some((slot, key)) = wire
        .iter()
        .enumerate()
        .filter(|(_, s)| s.part.is_some())
        .find_map(|(i, s)| other_scheme_key(s.qc).map(|k| (i, k)))
    else {
        return Mutation::Skipped;
    };
    let target = &wire[slot];
    // The commitment's own encoding locates it: the same bytes each place the payload carries
    // that commitment, so the target is the occurrence at its rank among them.
    let c = encoded(target.qc);
    let same: Vec<usize> =
        wire.iter().enumerate().filter(|(_, s)| encoded(s.qc) == c).map(|(i, _)| i).collect();
    let at = occurrences(&item.bytes, &c);
    assert_eq!(at.len(), same.len(), "{}: the commitment's places", item.label);
    let rank = same.iter().position(|i| *i == slot).expect("itself");
    let scheme = commitment_bls_scheme(target.qc.version());
    let own = target.qc.quorum_public_key().to_bytes(scheme);
    let in_c = occurrences(&c, &own);
    assert_eq!(in_c.len(), 1, "{}: the key's place in its commitment", item.label);
    let offset = at[rank] + in_c[0];
    let mut bytes = item.bytes.clone();
    assert_eq!(bytes[offset..offset + 48], own);
    bytes[offset..offset + 48].copy_from_slice(&key);
    let changed: Vec<usize> = (0..bytes.len()).filter(|i| bytes[*i] != item.bytes[*i]).collect();
    assert!(
        !changed.is_empty() && changed.iter().all(|i| (offset..offset + 48).contains(i)),
        "{}: exactly one 48-byte span replaced",
        item.label
    );
    let part = target.part.as_deref().expect("a new quorum");
    let position = match item.payload {
        Payload::Diff(_) => format!("new_quorums[{}]", target.index),
        Payload::QrInfo(_) => format!("{part}.new_quorums[{}]", target.index),
    };
    Mutation::Mutated {
        slot,
        position,
        bytes,
    }
}

/// One capture replay: its chain, its parameters, the payloads it applies in order, and the
/// payloads applied at a place in it (before the payload at that index) without being kept.
struct Replay<'a> {
    chain: &'a dyn MnChain,
    cfg: &'a LlmqConfig,
    pin: Option<u32>,
    applied: Vec<Item>,
    at: Vec<(usize, Item)>,
}

struct Golden {
    capture: &'static str,
    line: String,
}

impl Replay<'_> {
    /// The engine after the first `prefix` payloads.
    fn engine(&self, prefix: usize) -> MasternodeListEngine {
        let mut engine = MasternodeListEngine::new(Mode::Client);
        if let Some(pin) = self.pin {
            engine.pin_list(BlockId(pin));
        }
        for item in &self.applied[..prefix] {
            item.payload
                .apply(&mut engine, self.chain, self.cfg)
                .unwrap_or_else(|e| panic!("{} {}: {e}", item.capture, item.label));
        }
        engine
    }

    fn run(&self, out: &mut Vec<Golden>, counts: &mut BTreeMap<&'static str, (usize, usize)>) {
        let genesis = BlockHash::from(*self.cfg.params().consensus.hash_genesis_block.as_bytes());
        let mut sets = Sets {
            genesis,
            at: HashMap::new(),
        };
        let mut engine = self.engine(0);
        for i in 0..=self.applied.len() {
            for (_, item) in self.at.iter().filter(|(p, _)| *p == i) {
                sets.original(&item.payload, self.chain, false);
                self.mutation(&mut engine, i, item, &sets, out, counts);
            }
            let Some(item) = self.applied.get(i) else {
                break;
            };
            self.mutation(&mut engine, i, item, &sets, out, counts);
            item.payload
                .apply(&mut engine, self.chain, self.cfg)
                .unwrap_or_else(|e| panic!("{} {}: {e}", item.capture, item.label));
            sets.original(&item.payload, self.chain, true);
        }
    }

    /// `item` mutated, applied to the engine after the first `prefix` payloads; the engine is
    /// left in that state.
    fn mutation(
        &self,
        engine: &mut MasternodeListEngine,
        prefix: usize,
        item: &Item,
        sets: &Sets,
        out: &mut Vec<Golden>,
        counts: &mut BTreeMap<&'static str, (usize, usize)>,
    ) {
        let (slot, position, bytes) = match mutate(item) {
            Mutation::NoNewQuorum => return,
            Mutation::Skipped => {
                counts.entry(item.capture).or_default().1 += 1;
                return;
            }
            Mutation::Mutated {
                slot,
                position,
                bytes,
            } => (slot, position, bytes),
        };
        counts.entry(item.capture).or_default().0 += 1;
        let qrinfo = matches!(item.payload, Payload::QrInfo(_));
        let fields = match Payload::decode(qrinfo, &bytes) {
            Err(e) => ["none".into(), "none".into(), "none".into(), format!("decode {e:?}")],
            Ok(mutated) => self.verdict(engine, prefix, item, slot, &mutated, sets),
        };
        out.push(Golden {
            capture: item.capture,
            line: format!("{} {} {position} {}", item.capture, item.label, fields.join(" ")),
        });
    }

    /// The hash, root, store and verdict fields of the decoded `mutated`.
    fn verdict(
        &self,
        engine: &mut MasternodeListEngine,
        prefix: usize,
        item: &Item,
        slot: usize,
        mutated: &Payload,
        sets: &Sets,
    ) -> [String; 4] {
        let (was, now) = (item.payload.wire(), mutated.wire());
        assert_eq!(was.len(), now.len(), "{}", item.label);
        for (i, (a, b)) in was.iter().zip(&now).enumerate() {
            assert_eq!((&a.part, a.index), (&b.part, b.index), "{}", item.label);
            if i != slot {
                assert_eq!(a.qc, b.qc, "{}: only the mutated commitment changes", item.label);
            }
        }
        let part = now[slot].part.clone().expect("a new quorum");
        let (_, diff) = mutated
            .wire_diffs()
            .into_iter()
            .find(|(name, _)| *name == part)
            .expect("the mutated slot's diff");
        let hash = commitment_hash(
            &read_diff(diff).bls().new_quorum_reads().nth(now[slot].index).expect("the slot"),
        );

        if let Payload::Diff(d) = mutated
            && let Some(id) = self.chain.lookup(&d.block_hash()).expect("ok")
        {
            assert!(
                !(engine.list(id).is_some() && engine.has_authenticated_set(id)),
                "{}: a replay never applies a diff to a list it holds",
                item.label
            );
        }
        let _ = engine.take_dirty();
        let requests = engine.requests();
        let result = mutated.apply(engine, self.chain, self.cfg);
        let writes = engine.take_dirty();
        let mut row = vec![b'Q'];
        row.extend_from_slice(&hash);
        let store = writes
            .ops()
            .filter(|(k, _)| *k == row.as_slice())
            .filter_map(|(_, v)| v)
            .last()
            .map(|v| to_hex(&sha256(v)));
        if result.is_ok() || !writes.is_empty() || engine.requests() != requests {
            *engine = self.engine(prefix);
        }

        // Step 4 runs on the mutated diff when the engine reaches it: a `qrinfo`'s other diffs
        // are Core's, applied at this state; a diff's steps 1–3 read no new quorum, and it stops
        // there with any refusal but the quorum root's.
        let reached = match (&result, mutated) {
            (Ok(()) | Err(MnReject::QuorumsRoot), _) => true,
            (Err(_), Payload::Diff(_)) => false,
            (Err(e), Payload::QrInfo(_)) => {
                panic!(
                    "{}: a qrinfo refused before step 4 needs a new line form: {e:?}",
                    item.label
                )
            }
        };
        if result.is_ok() {
            assert!(store.is_some(), "{}: an accepted commitment is written", item.label);
        }
        let (root, cores) = sets
            .step4(mutated, self.chain)
            .into_iter()
            .find(|s| s.part == part)
            .map_or((None, None), |s| (s.root, s.cores));
        let root = root.filter(|_| reached);
        assert!(!matches!(root, Some(None)), "{}: step 4's merkle root fails", item.label);
        let root = root.flatten();
        match &result {
            Ok(()) => assert!(root.is_none() || root == cores, "{}", item.label),
            Err(MnReject::QuorumsRoot) => assert_ne!(root, cores, "{}", item.label),
            Err(_) => {}
        }
        [
            to_hex(&hash),
            root.map_or("none".into(), |r| to_hex(&r)),
            store.unwrap_or_else(|| "none".into()),
            match result {
                Ok(()) => "accepted".into(),
                Err(e) => format!("{e:?}"),
            },
        ]
    }
}

/// The diffs from genesis after the first, each to an empty engine; the step diffs above the
/// first, applied; the step diffs below it, checked to hold no new quorum.
fn diffs(capture: &'static str, f: Capture) -> (Vec<Item>, Vec<(usize, Item)>) {
    let from_genesis = f["diffs"]["from_genesis"].as_array().expect("from_genesis");
    let start = u32_of(&from_genesis[0]["height"]);
    let mut applied =
        vec![Item::new(capture, "from_genesis:0".into(), false, &from_genesis[0]["payload"])];
    let at = from_genesis
        .iter()
        .enumerate()
        .skip(1)
        .map(|(i, d)| (0, Item::new(capture, format!("from_genesis:{i}"), false, &d["payload"])))
        .collect();
    for (i, d) in f["diffs"]["step"].as_array().expect("steps").iter().enumerate() {
        let item = Item::new(capture, format!("step:{i}"), false, &d["payload"]);
        if u32_of(&d["height"]) > start {
            applied.push(item);
        } else {
            assert!(
                item.payload.wire().is_empty(),
                "{capture} step {i}: a new quorum below the first diff from genesis"
            );
        }
    }
    (applied, at)
}

fn qrinfos(capture: &'static str, f: Capture) -> impl Iterator<Item = Item> {
    f["qrinfos"]
        .as_array()
        .expect("qrinfos")
        .iter()
        .enumerate()
        .map(move |(i, q)| Item::new(capture, format!("qrinfo:{i}"), true, &q["payload"]))
}

fn goldens() -> (String, BTreeMap<&'static str, (usize, usize)>) {
    let mut out = Vec::new();
    let mut counts = BTreeMap::new();

    let f = fixture();
    let (chain, cfg) = (FixtureChain::of(f), config_of(f, &[]));
    let (mut applied, at) = diffs(REGTEST, f);
    applied.extend(qrinfos(REGTEST, f));
    Replay {
        chain: &chain,
        cfg: &cfg,
        pin: Some(QRINFO_BASE),
        applied,
        at,
    }
    .run(&mut out, &mut counts);

    let f = transitions();
    let (chain, cfg) = (FixtureChain::of(f), config_of(f, &[]));
    let (applied, at) = diffs(TRANSITIONS, f);
    assert!(f["qrinfos"].is_null());
    Replay {
        chain: &chain,
        cfg: &cfg,
        pin: None,
        applied,
        at,
    }
    .run(&mut out, &mut counts);

    let f = testnet::testnet();
    let (chain, cfg) = (TestnetChain::new(f), testnet::config());
    let (mut applied, mut at) = diffs(TESTNET, f);
    assert!(at.is_empty(), "one diff from genesis");
    let fetch = load(include_bytes!("../../tests/data/sb11-testnet-fetch-v24.0.captures.json.gz"));
    for (i, d) in fetch["diffs"].as_array().expect("diffs").iter().enumerate() {
        let base = chain.hash(u32_of(&d["base_height"]));
        let prefix = 1 + applied
            .iter()
            .position(|item| match &item.payload {
                Payload::Diff(s) => s.block_hash() == base,
                Payload::QrInfo(_) => false,
            })
            .expect("the step diff to the fetch's base");
        at.push((prefix, Item::new(FETCH, format!("diff:{i}"), false, &d["payload"])));
    }
    applied.extend(qrinfos(TESTNET, f));
    Replay {
        chain: &chain,
        cfg: &cfg,
        pin: None,
        applied,
        at,
    }
    .run(&mut out, &mut counts);

    out.sort_by_key(|g| CAPTURES.iter().position(|c| *c == g.capture).expect("a capture"));
    let mut text = format!("{PROVENANCE}\n");
    for g in out {
        text.push_str(&g.line);
        text.push('\n');
    }
    (text, counts)
}

/// T6 Q-4b-0 part A: the goldens recomputed equal the file (`T6_Q4B_WRITE_GOLDENS=1` writes it).
#[test]
fn key_mutation_goldens() {
    let (text, counts) = goldens();
    let counts: Vec<_> =
        CAPTURES.iter().map(|c| (*c, counts.get(c).copied().unwrap_or_default())).collect();
    if std::env::var("T6_Q4B_WRITE_GOLDENS").as_deref() == Ok("1") {
        std::fs::write(GOLDENS, &text).expect("the goldens written");
    } else {
        let held = std::fs::read_to_string(GOLDENS).expect("the goldens");
        if let Some((i, (a, b))) =
            held.lines().zip(text.lines()).enumerate().find(|(_, (a, b))| a != b)
        {
            panic!("line {}:\n held     {a}\n computed {b}", i + 1);
        }
        assert_eq!(held, text, "the goldens");
    }
    // (mutated, skipped) per capture.
    assert_eq!(
        counts,
        [(REGTEST, (28, 0)), (TRANSITIONS, (9, 0)), (TESTNET, (56, 11)), (FETCH, (2, 0))]
    );
}
