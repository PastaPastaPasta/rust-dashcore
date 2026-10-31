//! A-1 and A-2 at rule level (SB4 §15.1; D-C3-7): CS3's `ContextualCheckBlockHeader` rows,
//! judged by Core's verdicts recorded through the oracle
//! (`dash-consensus/tests/data/checks_recorded.json`), replayed over SB4's views.
//!
//! A row is a flat window of headers below the checked one, placeholders below the window
//! (zero time, bits and version under a hash no block has; the oracle's context), the node's
//! clock, and with checkpoints on the best header's height (`checks.rs`). The rows' bytes are
//! rebuilt by CS3's `rows()` and checked against the recorded SHA-256, as CS3's own replay does.
//!
//! - A-1 (`rules_cs3_header_rows_genesis`): a node chain from genesis holds the network's
//!   placeholders, inserted unchecked; each row's window hangs off the placeholder below it (a
//!   fresh chain when the window's headers are already held at other heights).
//!   The row is checked through the node's own contextual check (its complete ancestry) and
//!   through a refreshed `RuleContext` (SPV's reads), each with the checkpoint view the row
//!   gives (the highest checkpoint above the tip and at most the best header, as the chain's
//!   `GetLastCheckpoint` cache holds it).
//! - A-2 (`rules_cs3_header_rows_anchored`): each row whose window is past DGW's anchor floor
//!   and holds 25 headers runs on an SPV chain anchored at the window's tip, the last 25 headers
//!   as the pack (the context is then the pack), at w = 10,000 and w = 1,000: 37 rows. The other
//!   two (mainnet's version 1 at heights 950 and 951, below the floor) are node-only.

use std::collections::BTreeMap;

use super::*;
use crate::chain::rules::{PathEntry, RuleCache, RuleContext};
use crate::checks::{HeaderInput, Network, Verdict, verdict};

const TARGET: &str = "diff_contextual_check_block_header";

struct Row {
    name: String,
    input: HeaderInput,
    core: Verdict,
}

/// The recorded header rows, rebuilt and checked against the bytes Core judged.
fn header_rows() -> Vec<Row> {
    let recorded: Value =
        serde_json::from_slice(&crate_file("../dash-consensus/tests/data/checks_recorded.json"))
            .expect("checks_recorded.json");
    let recorded = recorded["rows"].as_array().expect("rows");
    let built = crate::checks_rows::rows();
    let names = |n: Vec<&str>| n.join("\n");
    assert_eq!(
        names(recorded.iter().map(|r| r["name"].as_str().expect("name")).collect()),
        names(built.iter().map(|r| r.name.as_str()).collect()),
        "the recorded rows are not CS3's rows()"
    );
    let mut rows = Vec::new();
    for (row, rec) in built.into_iter().zip(recorded) {
        assert_eq!(row.target, rec["target"], "{}", row.name);
        if row.target != TARGET {
            continue;
        }
        assert_eq!(
            sha256_hex(&row.input),
            rec["input_sha256"],
            "{}: not the bytes Core judged",
            row.name
        );
        let core = match &rec["rejection"] {
            Value::Null => Ok(()),
            r => Err((
                i32::try_from(r["result"].as_i64().expect("result")).expect("an i32"),
                r["reason"].as_str().expect("reason").to_owned(),
                r["debug_message"].as_str().expect("debug").to_owned(),
            )),
        };
        let input = HeaderInput::parse(&row.input).expect("a header input");
        rows.push(Row {
            name: row.name,
            input,
            core,
        });
    }
    rows
}

/// The oracle's placeholder hash at `height` (`checks.rs`'s `Window::entry`).
fn placeholder(height: u32) -> BlockHash {
    let mut hash = [0xa5; 32];
    hash[..4].copy_from_slice(&height.to_le_bytes());
    BlockHash::from(hash)
}

fn decode(raw: &[u8; 80]) -> BlockHeader {
    BlockHeader::decode(&mut &raw[..]).expect("80 bytes")
}

/// The highest checkpoint the row's index holds above its tip (the oracle's
/// `Checkpoints::Enabled { best_header_height }`), as `(hash, height)`.
fn row_checkpoint(input: &HeaderInput) -> Option<(BlockHash, u32)> {
    let w = &input.window;
    let best = w.best_header_height?;
    w.params()
        .checkpoints
        .iter()
        .map(|(h, hash)| (BlockHash::from(*hash.as_bytes()), h.to_u32()))
        .filter(|&(_, h)| h > w.tip() && h <= best)
        .max_by_key(|&(_, h)| h)
}

/// Sets `chain`'s `GetLastCheckpoint` cache to the row's checkpoint through the chain's own map.
fn set_checkpoint<S: HeaderStore, Rs: Residency>(
    chain: &mut HeaderChain<S, Rs>,
    input: &HeaderInput,
) {
    chain.last_checkpoint = None;
    if let Some((hash, height)) = row_checkpoint(input) {
        chain.note_checkpoint(&hash, height);
        assert_eq!(chain.highest_checkpoint_in_index(), Some(height), "a checkpoint of the map");
    }
}

/// A node chain holding a network's placeholders `1..=top`, with its clock.
struct Trunk {
    chain: HeaderChain,
    clock: Arc<MockClock>,
    top: u32,
    prev: BlockHash,
}

impl Trunk {
    fn new(network: Network) -> Trunk {
        let p = Arc::new(network.params().clone());
        let clock = Arc::new(MockClock::at(0));
        let svc = services_with(&p, clock.clone());
        let chain = HeaderChain::new_genesis(p.clone(), ChainConfig::node(&p), svc, NoStore)
            .expect("genesis");
        Trunk {
            chain,
            clock,
            top: 0,
            prev: Header::new(p.genesis.header).hash(),
        }
    }

    /// Extends the placeholders to `top`.
    fn grow(&mut self, top: u32) {
        for h in self.top + 1..=top {
            let header = BlockHeader {
                version: 0,
                prev_hash: self.prev,
                time: 0,
                bits: 0,
                ..BlockHeader::default()
            };
            self.prev = placeholder(h);
            let status = Status(Status::VALID_TREE);
            self.chain.add_to_block_index(&header, &self.prev, status).expect("added");
        }
        self.top = self.top.max(top);
    }

    /// Whether a header of the row's window is held at another height: a window depends only
    /// on its seed and length, so two rows can hold the same headers at different heights.
    fn collides(&self, input: &HeaderInput) -> bool {
        let w = &input.window;
        (w.first_height..=w.tip()).any(|h| {
            let hash = w.header(BlockId(h)).hash;
            let id = self.chain.shared.lookup(&hash);
            id.is_some_and(|id| self.chain.shared.arena.imm(id, |e| e.height) != Some(h))
        })
    }

    /// Inserts the row's window on the placeholder below it; returns its tip.
    fn window(&mut self, input: &HeaderInput) -> BlockId {
        let w = &input.window;
        assert!(w.first_height > 0, "a window from genesis would replace the network's genesis");
        let mut prev = placeholder(w.first_height - 1);
        let mut id = None;
        for (k, raw) in w.headers.iter().enumerate() {
            let mut header = decode(raw);
            // The window's lowest header links to the oracle's placeholder below it.
            if k == 0 {
                header.prev_hash = prev;
            }
            prev = w.header(BlockId(w.first_height + k as u32)).hash;
            id = Some(self.chain.add_to_block_index(&header, &prev, Status(Status::VALID_TREE)));
        }
        id.expect("a window has a tip").expect("added")
    }
}

/// A-1: every CS3 header row gives Core's verdict through the node's contextual check and
/// through a `RuleContext`, on a node chain from genesis.
#[test]
fn rules_cs3_header_rows_genesis() {
    let rows = header_rows();
    let mut trunks: BTreeMap<u8, Trunk> = BTreeMap::new();
    for r in &rows {
        let network = r.input.window.network;
        if trunks.get(&network.byte()).is_none_or(|t| t.collides(&r.input)) {
            trunks.insert(network.byte(), Trunk::new(network));
        }
        let trunk = trunks.get_mut(&network.byte()).expect("inserted");
        trunk.grow(r.input.window.first_height.saturating_sub(1));
        let prev = trunk.window(&r.input);
        let at = trunk.chain.shared.arena.imm(prev, |e| e.height);
        assert_eq!(at, Some(r.input.window.tip()), "{}: the window's tip", r.name);
        let header = Header::new(decode(&r.input.header));
        trunk.clock.set_mocktime(r.input.now);
        let chain = &mut trunk.chain;
        set_checkpoint(chain, &r.input);
        let node = chain.contextual_check(&header, prev).expect("no internal failure");
        assert_eq!(verdict(node), r.core, "{}: the node's check", r.name);
        // CH1-A10: the non-inserting check at the row's clock gives the same verdict.
        let at = chain.check_header_at(prev, &header, r.input.now).expect("no internal failure");
        assert_eq!(verdict(at), r.core, "{}: check_header_at", r.name);

        let pow = chain.hp.pow();
        let height = chain.shared.arena.imm(prev, |e| e.height).expect("resident");
        let mut cache = RuleCache::default();
        let envelope = cache.envelope(pow, height);
        let arena = &chain.shared.arena;
        let link = |id| {
            Ok(PathEntry {
                prev: arena.imm(id, |e| e.prev).expect("resident"),
                cold: None,
            })
        };
        let time = |id| arena.imm(id, |e| e.header.time);
        cache.refresh(prev, height, envelope, (0, 0), link, time).expect("refreshes");
        let ctx = RuleContext::new(arena, &cache);
        let spv = chain.check_on(&header, prev, &ctx, r.input.now);
        assert!(!ctx.read_outside(), "{}: a read outside the context", r.name);
        assert_eq!(verdict(spv), r.core, "{}: over a RuleContext", r.name);
    }
    assert_eq!(rows.len(), 39, "CS3's header rows");
}

/// A-2: the rows past DGW's anchor floor with a 25-header window, on SPV chains anchored at the
/// window's tip at w = 10,000 and w = 1,000.
#[test]
fn rules_cs3_header_rows_anchored() {
    let (mut anchored, mut node_only) = (0, Vec::new());
    for r in header_rows() {
        let w = &r.input.window;
        let p = Arc::new(w.params().clone());
        let dgw = p.consensus.pow_dgw_height.to_u32();
        if w.headers.len() < 25 || w.tip() < dgw + 24 {
            node_only.push(r.name);
            continue;
        }
        let headers = &w.headers[w.headers.len() - 25..];
        let pack = AnchorPack {
            height: w.tip(),
            hash: w.header(BlockId(w.tip())).hash,
            chain_work: Work::from_u128(1 << 100),
            headers: std::array::from_fn(|k| decode(&headers[k])),
        };
        let header = Header::new(decode(&r.input.header));
        for window in [10_000, 1_000] {
            let svc = services_at(&p, r.input.now);
            let (mut chain, _store) = spv_tests::anchored_with(&p, &pack, window, svc);
            set_checkpoint(&mut chain, &r.input);
            let got = chain.contextual_check(&header, BlockId(24)).expect("no internal failure");
            assert_eq!(verdict(got), r.core, "{}: anchored, w = {window}", r.name);
        }
        anchored += 1;
    }
    assert_eq!(anchored, 37, "anchored rows");
    let below = |h| format!("version 1 at height {h} (HeightInCoinbase from 951)");
    assert_eq!(node_only, [below(950), below(951)], "node-only rows");
}
