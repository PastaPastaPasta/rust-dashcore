// SPDX-License-Identifier: MIT

//! The coin statistics index through the real data object, driver and reader, against the
//! recorded `gettxoutsetinfo muhash <hash>` answers of dashd v24.0.0.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use dash_chain::{BlockRef, FlatFilePos, IndexAccess, Status, TipStep};
use dash_consensus::coins::BlockUndo;
use dash_consensus::hash::sha256d;
use dash_consensus::{Block, BlockId, decode_block};
use dash_primitives::BlockHash;
use dashd_chainstate::flush::SubscriberHub;
use dashd_chainstate::store::{GlobalFailStop, GuardedEngine, RecordingSink};
use dashd_chainstate::subscriber::{
    BlockSource, DriverConfig, ScriptedFeeds, SubStep, SubscriberData, SubscriberDriver,
    SubscriberError, SyncProgress, height_key,
};
use dashd_indexes::coinstats::data::{
    ARCHIVE, COINSTATS_DB, HEIGHT, META, MUHASH, decode_height_value,
};
use dashd_indexes::coinstats::{CoinStatsData, StoredCoinStats};
use dashd_indexes::{
    CoinStatsEntry, CoinStatsReader, IndexError, IndexStatus, IndexSummary, SubsidyInputs,
};
use dashd_store::{Commit, KvBatch, KvRead, MemoryEngine};
use serde_json::Value;

type Db = GuardedEngine<MemoryEngine>;
type Driver = SubscriberDriver<CoinStatsData<MemoryEngine>>;

fn duffs(value: &Value) -> i64 {
    let text = value.to_string();
    let (mantissa, exponent) = text.split_once(['e', 'E']).unwrap_or((&text, "0"));
    let exponent: i32 = exponent.parse().unwrap();
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits: i64 = format!("{whole}{fraction}").parse().unwrap();
    let scale = 8 + exponent - fraction.len() as i32;
    if scale >= 0 {
        digits * 10i64.pow(scale as u32)
    } else {
        let divisor = 10i64.pow((-scale) as u32);
        assert_eq!(digits % divisor, 0, "whole duffs: {text}");
        digits / divisor
    }
}

fn json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// Records in a `blk` or `rev` file: each payload and the bytes after it (a `rev` checksum).
fn records(bytes: &[u8], trailer: usize) -> Vec<(&[u8], &[u8])> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while rest.len() >= 8 && rest[..8] != [0; 8] {
        let size = u32::from_le_bytes(rest[4..8].try_into().unwrap()) as usize;
        out.push((&rest[8..8 + size], &rest[8 + size..8 + size + trailer]));
        rest = &rest[8 + size + trailer..];
    }
    out
}

/// Whether an undo record has one entry per input of each non-coinbase transaction: siblings
/// share a parent, so the checksum alone does not tell their records apart.
fn pairs(block: &Block, mut undo: &[u8]) -> bool {
    BlockUndo::decode(&mut undo).is_ok_and(|undo| {
        undo.txs.len() + 1 == block.txs().len()
            && undo
                .txs
                .iter()
                .zip(&block.txs()[1..])
                .all(|(u, tx)| u.coins.len() == tx.inputs().len())
    })
}

/// The recording's block tree, under its real hashes, as the block index.
#[derive(Clone, Default)]
struct RecordedIndex {
    nodes: Vec<(BlockHash, Option<BlockId>, u32)>,
    by_hash: HashMap<BlockHash, BlockId>,
}

impl RecordedIndex {
    fn block(&self, id: BlockId) -> BlockRef {
        let (hash, _, height) = self.nodes[id.0 as usize];
        BlockRef {
            id,
            hash,
            height,
        }
    }

    fn by_hash(&self, hash: &BlockHash) -> BlockRef {
        self.block(self.by_hash[hash])
    }

    fn parent(&self, b: &BlockRef) -> Option<BlockRef> {
        self.nodes[b.id.0 as usize].1.map(|id| self.block(id))
    }

    fn path(&self, tip: BlockRef) -> Vec<BlockRef> {
        let mut out = vec![tip];
        while let Some(p) = self.parent(out.last().unwrap()) {
            out.push(p);
        }
        out.reverse();
        out
    }
}

impl IndexAccess for RecordedIndex {
    fn lookup(&self, h: &BlockHash) -> Option<BlockId> {
        self.by_hash.get(h).copied()
    }

    fn hash(&self, id: BlockId) -> Option<BlockHash> {
        self.nodes.get(id.0 as usize).map(|n| n.0)
    }

    fn ancestor(&self, mut id: BlockId, height: u32) -> Option<BlockId> {
        while self.nodes.get(id.0 as usize)?.2 > height {
            id = self.nodes[id.0 as usize].1?;
        }
        (self.nodes[id.0 as usize].2 == height).then_some(id)
    }

    fn lca(&self, a: BlockId, b: BlockId) -> Option<BlockId> {
        let h = self.height(a)?.min(self.height(b)?);
        let (mut a, mut b) = (self.ancestor(a, h)?, self.ancestor(b, h)?);
        while a != b {
            (a, b) = (self.nodes[a.0 as usize].1?, self.nodes[b.0 as usize].1?);
        }
        Some(a)
    }

    fn height(&self, id: BlockId) -> Option<u32> {
        self.nodes.get(id.0 as usize).map(|n| n.2)
    }

    fn have_data_pos(&self, _: BlockId) -> Option<FlatFilePos> {
        None
    }

    fn undo_pos(&self, _: BlockId) -> Option<FlatFilePos> {
        None
    }

    fn status(&self, _: BlockId) -> Option<Status> {
        Some(Status(Status::VALID_TREE | Status::HAVE_DATA))
    }
}

/// A phase's name, its tip, and its blocks' answers.
type Phase = (String, BlockHash, Vec<(BlockHash, Value)>);

/// A recording's blocks, undo data (from its `rev` file), subsidies and answers.
struct Recorded {
    name: &'static str,
    index: RecordedIndex,
    blocks: HashMap<BlockHash, Arc<Block>>,
    undo: HashMap<BlockHash, Vec<u8>>,
    subsidies: HashMap<BlockHash, i64>,
    /// `gettxoutsetinfo muhash <hash>` at the end of the recording, and each block's role.
    stats: HashMap<BlockHash, (Value, String)>,
    events: Vec<(bool, BlockHash)>,
    /// Each phase's tip and its blocks' answers at that point.
    phases: Vec<Phase>,
}

impl Recorded {
    fn load(name: &'static str) -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
        let rows = json(&root.join("blocks.json"));
        let stats: HashMap<BlockHash, (Value, String)> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let hash = row["hash"].as_str().unwrap().parse().unwrap();
                (hash, (row["gettxoutsetinfo"].clone(), row["role"].as_str().unwrap().to_owned()))
            })
            .collect();
        let blk = fs::read(root.join("blocks/blk00000.dat")).unwrap();
        let rev = fs::read(root.join("blocks/rev00000.dat")).unwrap();
        let revs = records(&rev, 32);
        let mut index = RecordedIndex::default();
        let mut blocks = HashMap::new();
        let mut undo = HashMap::new();
        let mut subsidies = HashMap::new();
        let issued = |hash: &BlockHash| {
            let s = &stats[hash].0;
            duffs(&s["total_amount"]) + duffs(&s["total_unspendable_amount"])
        };
        for (bytes, _) in records(&blk, 0) {
            let block = decode_block(&mut &bytes[..]).unwrap().into_value();
            let hash = block.hash();
            let height = stats[&hash].0["height"].as_u64().unwrap() as u32;
            let parent = (height > 0).then(|| index.by_hash[&block.header().prev_hash()]);
            let id = BlockId(index.nodes.len() as u32);
            index.nodes.push((hash, parent, height));
            index.by_hash.insert(hash, id);
            let subsidy = match parent {
                None => issued(&hash),
                Some(_) => {
                    let prev = block.header().prev_hash();
                    let (bytes, _) = *revs
                        .iter()
                        .find(|(bytes, sum)| {
                            sha256d(&[prev.as_bytes().as_slice(), bytes].concat()).as_slice()
                                == *sum
                                && pairs(&block, bytes)
                        })
                        .unwrap_or_else(|| panic!("{name}: no undo for {height}"));
                    undo.insert(hash, bytes.to_vec());
                    issued(&hash) - issued(&prev)
                }
            };
            subsidies.insert(hash, subsidy);
            blocks.insert(hash, Arc::new(block));
        }
        assert_eq!(blocks.len(), stats.len(), "{name}: every recorded block is in the file");
        let chain = json(&root.join("chain.json"));
        let events = chain["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| match e.get("connect") {
                Some(h) => (true, h.as_str().unwrap().parse().unwrap()),
                None => (false, e["disconnect"].as_str().unwrap().parse().unwrap()),
            })
            .collect();
        let phases = chain["phases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                let blocks = p["blocks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|b| {
                        (b["hash"].as_str().unwrap().parse().unwrap(), b["gettxoutsetinfo"].clone())
                    })
                    .collect();
                (
                    p["name"].as_str().unwrap().to_owned(),
                    p["tip"].as_str().unwrap().parse().unwrap(),
                    blocks,
                )
            })
            .collect();
        Self {
            name,
            index,
            blocks,
            undo,
            subsidies,
            stats,
            events,
            phases,
        }
    }

    fn genesis(&self) -> BlockRef {
        self.index.block(BlockId(0))
    }
}

struct Source(Arc<Recorded>);

impl BlockSource for Source {
    fn block(&self, b: &BlockRef) -> Result<Arc<Block>, SubscriberError> {
        Ok(Arc::clone(&self.0.blocks[&b.hash]))
    }

    fn undo(&self, b: &BlockRef) -> Result<Vec<u8>, SubscriberError> {
        Ok(match b.height {
            0 => BlockUndo::default().core_bytes().as_slice().to_vec(),
            _ => self.0.undo[&b.hash].clone(),
        })
    }
}

struct Subsidies(Arc<Recorded>);

impl SubsidyInputs for Subsidies {
    fn subsidy(&self, block: &BlockRef) -> Result<i64, IndexError> {
        Ok(self.0.subsidies[&block.hash])
    }
}

struct Synced;

impl IndexStatus for Synced {
    fn summary(&self) -> IndexSummary {
        IndexSummary {
            name: "coinstatsindex",
            synced: true,
            best_block_height: 0,
        }
    }

    fn block_until_synced(&self) -> bool {
        true
    }
}

fn cfg() -> DriverConfig {
    DriverConfig {
        journal_budget: 1 << 20,
        unbounded_delete_span: 64,
        d_max: Some(96),
        cadence: 1000,
    }
}

/// The driver over a store, its feed and hub, and a flush counter.
struct Run {
    rec: Arc<Recorded>,
    db: Arc<Db>,
    feeds: ScriptedFeeds,
    hub: Arc<SubscriberHub>,
    d: Option<Driver>,
    flushes: u64,
    lone_entry: u64,
}

impl Run {
    fn new(rec: &Arc<Recorded>) -> Self {
        let db = Arc::new(GuardedEngine::new(
            MemoryEngine::new(&COINSTATS_DB),
            GlobalFailStop::new(),
            Arc::new(RecordingSink::default()),
        ));
        let mut run = Self::reopen(rec, db, vec![rec.genesis()]);
        run.check_bound();
        run
    }

    /// Opens on `db` with V's chain `chain`, flushed at its tip, and catches up.
    fn reopen(rec: &Arc<Recorded>, db: Arc<Db>, chain: Vec<BlockRef>) -> Self {
        let tip = *chain.last().unwrap();
        let feeds = ScriptedFeeds::new(4, chain);
        let hub = Arc::new(SubscriberHub::with_feeds(
            Arc::new(feeds.clone()),
            Arc::new(rec.index.clone()),
        ));
        hub.publish_durable(1, tip, feeds.tip_seq());
        let data = CoinStatsData::new(Arc::new(Subsidies(Arc::clone(rec))));
        let mut d = SubscriberDriver::open(
            data,
            Arc::clone(&db),
            Arc::clone(&hub),
            Arc::new(Source(Arc::clone(rec))),
            cfg(),
        )
        .expect("opens");
        let live = (0..1000).any(|_| d.sync_step().expect("syncs") == SyncProgress::Live);
        assert!(live, "never live");
        Self {
            rec: Arc::clone(rec),
            db,
            feeds,
            hub,
            d: Some(d),
            flushes: 1,
            lone_entry: 0,
        }
    }

    fn d(&mut self) -> &mut Driver {
        self.d.as_mut().unwrap()
    }

    /// Steps until idle, answering a parked driver's `FlushDue` with a flush, as W does.
    fn drain(&mut self) {
        for _ in 0..1000 {
            match self.d().step().expect("steps") {
                SubStep::Progress => {}
                SubStep::Parked => self.publish(),
                SubStep::Idle => {
                    self.check_bound();
                    return;
                }
            }
        }
        panic!("never idle");
    }

    fn publish(&mut self) {
        self.flushes += 1;
        let tip = self.feeds.tip().unwrap();
        self.hub.publish_durable(self.flushes, tip, self.feeds.tip_seq());
    }

    /// The journal never holds more than `max_entry_bytes` per entry; a lone entry's bytes are
    /// recorded to show the bound is reached.
    fn check_bound(&mut self) {
        let d = self.d.as_ref().unwrap();
        if d.journal_len() == 1 {
            self.lone_entry = self.lone_entry.max(d.journal_bytes());
        }
        assert!(
            d.journal_bytes() <= d.journal_len() as u64 * d.data().max_entry_bytes(),
            "{} bytes in {} entries",
            d.journal_bytes(),
            d.journal_len()
        );
    }

    fn event(&mut self, connect: bool, hash: &BlockHash) {
        if connect {
            self.feeds.connect(self.rec.index.by_hash(hash));
        } else {
            let TipStep::Disconnected {
                block,
                ..
            } = self.feeds.disconnect()
            else {
                unreachable!()
            };
            assert_eq!(block.hash, *hash);
        }
        self.drain();
    }

    fn flush(&mut self) {
        self.publish();
        let tip = self.feeds.tip().unwrap();
        self.drain();
        assert_eq!(self.d().durable(), Some(tip));
    }

    /// A flush, then a restart: the next open decodes the state at L.
    fn restart(&mut self) {
        self.flush();
        drop(self.d.take());
        let chain = self.rec.index.path(self.feeds.tip().unwrap());
        *self = Self::reopen(&self.rec, Arc::clone(&self.db), chain);
    }

    fn reader(&self) -> StoredCoinStats<Db> {
        StoredCoinStats::new(Arc::clone(&self.db), Arc::new(Synced))
    }

    fn row(&self, cf: dashd_store::Cf, key: &[u8]) -> Option<Vec<u8>> {
        self.db.get(cf, key).unwrap()
    }

    /// Whether the height row at `b`'s height names it.
    fn on_height_row(&self, b: &BlockRef) -> bool {
        self.row(HEIGHT, &height_key(b.height, &[]))
            .is_some_and(|v| decode_height_value(&v).unwrap().0 == b.hash)
    }

    /// The reader's answer for `hash` against `gettxoutsetinfo muhash <hash>`, with `block_info`
    /// from the parent's entry as the RPC computes it. Returns whether the archive served it.
    fn check(&self, hash: &BlockHash, want: &Value, at: &str) -> bool {
        let reader = self.reader();
        let b = self.rec.index.by_hash(hash);
        let at = format!("{} {at} at {} ({hash})", self.rec.name, b.height);
        let got = reader.look_up_stats(&b).unwrap().unwrap_or_else(|| panic!("{at}: a miss"));
        let parent = self
            .rec
            .index
            .parent(&b)
            .map(|p| reader.look_up_stats(&p).unwrap().expect("the parent's entry"));
        assert_eq!(want["height"].as_u64(), Some(u64::from(b.height)), "{at}");
        let muhash: BlockHash = want["muhash"].as_str().unwrap().parse().unwrap();
        assert_eq!(got.muhash, *muhash.as_bytes(), "{at}: muhash");
        assert_eq!(Some(got.txouts), want["txouts"].as_u64(), "{at}: txouts");
        assert_eq!(Some(got.bogo_size), want["bogosize"].as_u64(), "{at}: bogosize");
        assert_eq!(got.total_amount, duffs(&want["total_amount"]), "{at}: total_amount");
        assert_eq!(
            got.total_unspendable,
            duffs(&want["total_unspendable_amount"]),
            "{at}: total_unspendable_amount"
        );
        assert_eq!(got.total_subsidy, got.total_amount + got.total_unspendable, "{at}: subsidy");
        let bi = got.totals().block_info(parent.as_ref().map(CoinStatsEntry::totals).as_ref());
        let info = &want["block_info"];
        let recorded = [
            ("prevout_spent", bi.prevout_spent, &info["prevout_spent"]),
            ("coinbase", bi.coinbase, &info["coinbase"]),
            (
                "new_outputs_ex_coinbase",
                bi.new_outputs_ex_coinbase,
                &info["new_outputs_ex_coinbase"],
            ),
            ("unspendable", bi.unspendable, &info["unspendable"]),
            (
                "genesis_block",
                bi.unspendables_genesis_block,
                &info["unspendables"]["genesis_block"],
            ),
            ("scripts", bi.unspendables_scripts, &info["unspendables"]["scripts"]),
            (
                "unclaimed_rewards",
                bi.unspendables_unclaimed_rewards,
                &info["unspendables"]["unclaimed_rewards"],
            ),
        ];
        for (field, got, want) in recorded {
            assert_eq!(got, duffs(want), "{at}: block_info.{field}");
        }
        let archived = !self.on_height_row(&b);
        if archived {
            assert!(self.row(ARCHIVE, hash.as_bytes()).is_some(), "{at}: served from the archive");
        }
        archived
    }
}

/// Replays the recorded event order and compares every recorded answer at its phase and at the
/// end; `restart` restarts the index at each phase.
fn replay(name: &'static str, restart: bool) {
    let rec = Arc::new(Recorded::load(name));
    let mut run = Run::new(&rec);
    let mut phases = rec.phases.iter().peekable();
    let (mut checked, mut archived) = (0, 0);
    for i in 0..=rec.events.len() {
        if i > 0 {
            let (connect, hash) = rec.events[i - 1];
            run.event(connect, &hash);
        }
        while let Some((phase, _, blocks)) =
            phases.next_if(|(_, tip, _)| run.feeds.tip().unwrap().hash == *tip)
        {
            for (hash, want) in blocks {
                archived += usize::from(run.check(hash, want, phase));
                checked += 1;
            }
            if restart {
                run.restart();
            }
        }
    }
    assert!(phases.next().is_none(), "{name}: every phase is replayed");
    if !restart {
        let bound = run.d().data().max_entry_bytes();
        assert_eq!(
            run.lone_entry, bound,
            "{name}: the measured bound (a tripwire for std's first allocation)"
        );
    }
    for (hash, (want, _)) in &rec.stats {
        archived += usize::from(run.check(hash, want, "end"));
        checked += 1;
    }
    assert!(archived > 0, "{name}: reorged-out blocks are served from the archive");
    eprintln!("{name}: {checked} answers, {archived} from the archive (restart: {restart})");
}

#[test]
fn recorded_g_answers_equal_dashd_at_every_phase() {
    replay("dashd-v24", false);
}

#[test]
fn recorded_g_answers_equal_dashd_with_a_restart_at_every_phase() {
    replay("dashd-v24", true);
}

#[test]
fn recorded_g2_answers_equal_dashd_at_every_phase() {
    replay("dashd-v24-reorg", false);
}

#[test]
fn recorded_g2_answers_equal_dashd_with_a_restart_at_every_phase() {
    replay("dashd-v24-reorg", true);
}

/// Each block of the final chain connected, committed, disconnected below L (the rewind commit
/// runs the data object's reverse), committed again, and reconnected: the state after the
/// disconnect is the state before the connect, both `M` and the totals the reconnect rebuilds.
fn reverse_each(name: &'static str, roles: &[&str]) {
    let rec = Arc::new(Recorded::load(name));
    let tip = rec.phases.last().map(|(_, tip, _)| rec.index.by_hash(tip)).unwrap();
    let path = rec.index.path(tip);
    let mut run = Run::new(&rec);
    let mut seen: Vec<&str> = Vec::new();
    for b in &path[1..] {
        let m_parent = run.row(META, MUHASH).expect("M at the parent");
        run.feeds.connect(*b);
        run.drain();
        run.flush();
        let key = height_key(b.height, &[]);
        let m_b = run.row(META, MUHASH).unwrap();
        let row_b = run.row(HEIGHT, &key).unwrap();
        assert_ne!(m_b, m_parent);
        run.feeds.disconnect();
        run.drain();
        run.flush();
        let parent = rec.index.parent(b).unwrap();
        assert_eq!(run.d().durable(), Some(parent), "{name}: rewound to {}", parent.height);
        assert_eq!(
            run.row(META, MUHASH).unwrap(),
            m_parent,
            "{name}: M after reversing {}",
            b.height
        );
        assert_eq!(run.row(HEIGHT, &key), None, "{name}: {}'s row is deleted", b.height);
        assert_eq!(run.row(ARCHIVE, b.hash.as_bytes()).as_deref(), Some(&row_b[32..]));
        run.feeds.connect(*b);
        run.drain();
        run.flush();
        assert_eq!(
            run.row(META, MUHASH).unwrap(),
            m_b,
            "{name}: M after reconnecting {}",
            b.height
        );
        assert_eq!(run.row(HEIGHT, &key).unwrap(), row_b, "{name}: totals at {}", b.height);
        let role = &rec.stats[&b.hash].1;
        seen.extend(roles.iter().filter(|r| role.contains(*r)));
    }
    for role in roles {
        assert!(seen.contains(role), "{name}: no {role} block reversed");
    }
}

#[test]
fn reversing_each_g_block_through_the_driver_restores_the_state() {
    reverse_each("dashd-v24", &["asset lock", "asset unlock", "OP_RETURN"]);
}

#[test]
fn reversing_each_g2_block_through_the_driver_restores_the_state() {
    reverse_each("dashd-v24-reorg", &["underclaims"]);
}

/// A store whose `M` and locator a test rewrites before the next open.
fn reopen_after(edit: impl FnOnce(&Db, &RecordedIndex)) -> SubscriberError {
    let rec = Arc::new(Recorded::load("dashd-v24"));
    let mut run = Run::new(&rec);
    for &(connect, hash) in rec.events.iter().take(5) {
        run.event(connect, &hash);
    }
    run.flush();
    let db = Arc::clone(&run.db);
    let chain = rec.index.path(run.feeds.tip().unwrap());
    drop(run);
    edit(&db, &rec.index);
    let tip = *chain.last().unwrap();
    let feeds = ScriptedFeeds::new(4, chain);
    let hub =
        Arc::new(SubscriberHub::with_feeds(Arc::new(feeds.clone()), Arc::new(rec.index.clone())));
    hub.publish_durable(1, tip, feeds.tip_seq());
    match SubscriberDriver::open(
        CoinStatsData::<MemoryEngine>::new(Arc::new(Subsidies(Arc::clone(&rec)))),
        db,
        hub,
        Arc::new(Source(Arc::clone(&rec))),
        cfg(),
    ) {
        Ok(_) => panic!("a refused open"),
        Err(err) => err,
    }
}

fn put(db: &Db, cf: dashd_store::Cf, key: &[u8], value: Option<&[u8]>) {
    let mut b = db.batch();
    match value {
        Some(v) => b.put(cf, key, v),
        None => b.delete(cf, key),
    }
    db.write(b, Commit::Durable).unwrap();
}

fn assert_refused(err: SubscriberError, what: &str) {
    assert!(
        matches!(&err, SubscriberError::Data(m) if m.starts_with("coinstats: ") && m.contains(what)),
        "{err:?}"
    );
}

/// L for `reopen_after`: the fifth connect of the recording.
const L: u32 = 5;

// Core: index/coinstatsindex.cpp:352-376 (a missing M is the empty set)
#[test]
fn a_missing_m_above_genesis_is_refused_by_the_row_at_l() {
    let err = reopen_after(|db, _| put(db, META, MUHASH, None));
    assert_refused(err, "M does not match the height row at L (5)");
}

// Core: index/coinstatsindex.cpp:352-376 (the genesis row holds the empty set)
#[test]
fn a_missing_m_at_a_genesis_l_opens_with_the_empty_set() {
    let rec = Arc::new(Recorded::load("dashd-v24"));
    let mut run = Run::new(&rec);
    assert_eq!(run.d().durable(), Some(rec.genesis()));
    put(&run.db, META, MUHASH, None);
    drop(run.d.take());
    let mut run = Run::reopen(&rec, Arc::clone(&run.db), vec![rec.genesis()]);
    assert_eq!(run.d().durable(), Some(rec.genesis()));
    for &(connect, hash) in rec.events.iter().take(3) {
        run.event(connect, &hash);
    }
    for &(_, hash) in rec.events.iter().take(3) {
        run.check(&hash, &rec.stats[&hash].0, "after an open without M");
    }
}

// Core: index/coinstatsindex.cpp:304-321,363-369
#[test]
fn a_row_at_l_naming_another_block_is_refused() {
    let err = reopen_after(|db, _| edit_row(db, L, |row| row[0] ^= 1));
    assert_refused(err, "the height row at L (5) names");
}

// Core: index/coinstatsindex.cpp:370-376
#[test]
fn decode_state_refuses_an_m_that_does_not_match_the_height_row_at_l() {
    let err = reopen_after(|db, _| {
        let mut m = db.get(META, MUHASH).unwrap().unwrap();
        m[0] ^= 1;
        put(db, META, MUHASH, Some(&m));
    });
    assert_refused(err, "does not match the height row at L (5)");
    let err = reopen_after(|db, _| edit_row(db, L, |row| row[32] ^= 1));
    assert_refused(err, "does not match the height row at L (5)");
}

/// An `M` committed below L, consistent with its own height row, is refused by the row at L.
#[test]
fn an_m_below_l_is_refused_at_open() {
    let rec = Arc::new(Recorded::load("dashd-v24-reorg"));
    let mut run = Run::new(&rec);
    for &(connect, hash) in rec.events.iter().take(3) {
        run.event(connect, &hash);
    }
    run.flush();
    let m3 = run.row(META, MUHASH).unwrap();
    run.event(true, &rec.events[3].1);
    run.flush();
    put(&run.db, META, MUHASH, Some(&m3));
    let db = Arc::clone(&run.db);
    let chain = rec.index.path(run.feeds.tip().unwrap());
    drop(run);
    let feeds = ScriptedFeeds::new(4, chain);
    let hub =
        Arc::new(SubscriberHub::with_feeds(Arc::new(feeds.clone()), Arc::new(rec.index.clone())));
    let open = SubscriberDriver::open(
        CoinStatsData::<MemoryEngine>::new(Arc::new(Subsidies(Arc::clone(&rec)))),
        db,
        hub,
        Arc::new(Source(Arc::clone(&rec))),
        cfg(),
    );
    match open {
        Ok(_) => panic!("an open with M below L"),
        Err(err) => assert_refused(err, "does not match the height row at L (4)"),
    }
}

#[test]
fn decode_state_refuses_undecodable_rows() {
    let err = reopen_after(|db, _| put(db, META, MUHASH, Some(&[0; 7])));
    assert_refused(err, "M length 7");
    let err = reopen_after(|db, _| put(db, HEIGHT, &height_key(L, &[]), Some(&[0; 9])));
    assert_refused(err, "height row length 9");
    let err = reopen_after(|db, _| put(db, HEIGHT, &height_key(L, &[]), None));
    assert_refused(err, "no height row at L");
}

/// The open finds L and `M` as the last commit left them, and decodes the state at L.
#[test]
fn an_open_decodes_the_state_at_l() {
    let rec = Arc::new(Recorded::load("dashd-v24-reorg"));
    let mut run = Run::new(&rec);
    for &(connect, hash) in rec.events.iter().take(3) {
        run.event(connect, &hash);
    }
    run.flush();
    let m = run.row(META, MUHASH).unwrap();
    assert_eq!(m.len(), dash_consensus::coins::MuHash3072::SERIALIZED_SIZE);
    run.restart();
    assert_eq!(run.d().durable().map(|l| l.height), Some(3));
    for &(connect, hash) in rec.events.iter().skip(3).take(2) {
        run.event(connect, &hash);
    }
    for &(_, hash) in rec.events.iter().take(5) {
        run.check(&hash, &rec.stats[&hash].0, "after the restart");
    }
}

/// Steps until the driver fails, with a flush posted in case the rewind waits for a commit.
fn step_err(run: &mut Run) -> SubscriberError {
    run.publish();
    for _ in 0..1000 {
        match run.d().step() {
            Err(err) => return err,
            Ok(SubStep::Idle) => panic!("the driver went idle without an error"),
            Ok(_) => {}
        }
    }
    panic!("never failed");
}

/// L at height 4 of the reorg recording, a row edited, then a disconnect below L: the rewind
/// commit's reverse refuses it.
fn rewind_after(edit: impl FnOnce(&Db)) -> SubscriberError {
    let rec = Arc::new(Recorded::load("dashd-v24-reorg"));
    let mut run = Run::new(&rec);
    for &(connect, hash) in rec.events.iter().take(4) {
        run.event(connect, &hash);
    }
    run.flush();
    edit(&run.db);
    run.feeds.disconnect();
    step_err(&mut run)
}

fn edit_row(db: &Db, height: u32, f: impl FnOnce(&mut Vec<u8>)) {
    let key = height_key(height, &[]);
    let mut row = db.get(HEIGHT, &key).unwrap().unwrap();
    f(&mut row);
    put(db, HEIGHT, &key, Some(&row));
}

// Core: index/coinstatsindex.cpp:417-431,486-502
#[test]
fn a_rewind_refuses_a_parent_row_the_reverse_does_not_give() {
    let err = rewind_after(|db| edit_row(db, 3, |row| row[64] ^= 1));
    assert_refused(err, "does not give the stored entry at 3");
    let err = rewind_after(|db| edit_row(db, 3, |row| row[0] ^= 1));
    assert_refused(err, "the height row at 3 names");
}

// Core: index/coinstatsindex.cpp:243-255
#[test]
fn a_rewind_refuses_a_missing_row_it_would_archive() {
    let err = rewind_after(|db| put(db, HEIGHT, &height_key(4, &[]), None));
    assert_refused(err, "no height row at 4");
}
