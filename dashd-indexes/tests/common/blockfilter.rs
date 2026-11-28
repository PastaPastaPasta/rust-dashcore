// SPDX-License-Identifier: MIT

//! g's recorded chain replayed through the basic block filter index's data object and driver,
//! over an in-memory store and filesystem.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dash_chain::{BlockRef, FlatFilePos, IndexAccess, Status, TipStep};
use dash_consensus::coins::BlockUndo;
use dash_consensus::hash::sha256d;
use dash_consensus::{Block, BlockId, decode_block};
use dash_filters::filter_hash;
use dash_primitives::BlockHash;
use dashd_chainstate::flush::SubscriberHub;
use dashd_chainstate::store::{GlobalFailStop, GuardedEngine, RecordingSink};
use dashd_chainstate::subscriber::{
    BlockSource, DriverConfig, FilePos, ScriptedFeeds, SubStep, SubscriberData, SubscriberDriver,
    SubscriberError, SyncProgress, height_key,
};
use dashd_indexes::blockfilter::FilterFiles;
use dashd_indexes::blockfilter::codec::{DbVal, HeightValue};
use dashd_indexes::blockfilter::data::{ARCHIVE, BLOCKFILTER_DB, BlockFilterData, HEIGHT};
use dashd_indexes::blockfilter::files::encode_entry;
use dashd_store::flatfile::{FlatFs, FlatRead};
use dashd_store::{KvRead, MemoryEngine};
use serde_json::Value;

pub type Db = GuardedEngine<MemoryEngine>;
pub type Data = BlockFilterData<MemoryEngine>;
pub type Driver = SubscriberDriver<Data>;

/// An in-memory filesystem whose writes can be made to fail after writing three bytes.
#[derive(Default)]
pub struct MemFs {
    pub files: Mutex<BTreeMap<PathBuf, Vec<u8>>>,
    pub fail_writes: AtomicBool,
    pub writes: Mutex<Vec<(PathBuf, u64, usize)>>,
}

impl MemFs {
    pub fn bytes(&self, path: &Path) -> Option<Vec<u8>> {
        self.files.lock().unwrap().get(path).cloned()
    }

    pub fn writes(&self) -> Vec<(PathBuf, u64, usize)> {
        self.writes.lock().unwrap().clone()
    }
}

pub struct Snapshot(pub Vec<u8>);

impl FlatRead for Snapshot {
    fn len(&self) -> io::Result<u64> {
        Ok(self.0.len() as u64)
    }
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let at = offset as usize;
        buf.copy_from_slice(self.0.get(at..at + buf.len()).ok_or(io::ErrorKind::UnexpectedEof)?);
        Ok(())
    }
}

impl FlatFs for MemFs {
    fn link_count(&self, path: &Path) -> io::Result<Option<u64>> {
        Ok(self.files.lock().unwrap().contains_key(path).then_some(1))
    }
    fn len(&self, path: &Path) -> io::Result<Option<u64>> {
        Ok(self.files.lock().unwrap().get(path).map(|f| f.len() as u64))
    }
    fn create(&self, path: &Path) -> io::Result<()> {
        self.files.lock().unwrap().entry(path.to_path_buf()).or_default();
        Ok(())
    }
    fn read_exact_at(&self, path: &Path, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let files = self.files.lock().unwrap();
        let f = files.get(path).ok_or(io::ErrorKind::NotFound)?;
        let at = offset as usize;
        let src = f.get(at..at + buf.len()).ok_or(io::ErrorKind::UnexpectedEof)?;
        buf.copy_from_slice(src);
        Ok(())
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FlatRead + '_>> {
        Ok(Box::new(Snapshot(self.bytes(path).ok_or(io::ErrorKind::NotFound)?)))
    }
    fn write_all_at(&self, path: &Path, offset: u64, data: &[u8]) -> io::Result<()> {
        let fail = self.fail_writes.load(Ordering::Relaxed);
        let data = if fail {
            &data[..data.len().min(3)]
        } else {
            data
        };
        self.writes.lock().unwrap().push((path.to_path_buf(), offset, data.len()));
        let mut files = self.files.lock().unwrap();
        let f = files.entry(path.to_path_buf()).or_default();
        let end = offset as usize + data.len();
        if f.len() < end {
            f.resize(end, 0);
        }
        f[offset as usize..end].copy_from_slice(data);
        if fail {
            return Err(io::Error::other("injected write failure"));
        }
        Ok(())
    }
    fn allocate(&self, path: &Path, offset: u64, len: u64) {
        let mut files = self.files.lock().unwrap();
        let f = files.entry(path.to_path_buf()).or_default();
        let end = (offset + len) as usize;
        if f.len() < end {
            f.resize(end, 0);
        }
    }
    fn truncate(&self, path: &Path, len: u64) -> io::Result<()> {
        self.files.lock().unwrap().entry(path.to_path_buf()).or_default().resize(len as usize, 0);
        Ok(())
    }
    fn sync_data(&self, _: &Path) -> io::Result<()> {
        Ok(())
    }
    fn sync_dir(&self, _: &Path) -> io::Result<()> {
        Ok(())
    }
    fn copy(&self, from: &Path, to: &Path) -> io::Result<()> {
        let bytes = self.bytes(from).ok_or(io::ErrorKind::NotFound)?;
        self.files.lock().unwrap().insert(to.to_path_buf(), bytes);
        Ok(())
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut files = self.files.lock().unwrap();
        let bytes = files.remove(from).ok_or(io::ErrorKind::NotFound)?;
        files.insert(to.to_path_buf(), bytes);
        Ok(())
    }
    fn remove(&self, path: &Path) -> io::Result<bool> {
        Ok(self.files.lock().unwrap().remove(path).is_some())
    }
    fn create_dir_all(&self, _: &Path) -> io::Result<()> {
        Ok(())
    }
    fn available_space(&self, _: &Path) -> io::Result<u64> {
        Ok(u64::MAX / 2)
    }
}

pub fn json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// Records in a `blk` or `rev` file: each payload and the bytes after it (a `rev` checksum).
pub fn records(bytes: &[u8], trailer: usize) -> Vec<(&[u8], &[u8])> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while rest.len() >= 8 && rest[..8] != [0; 8] {
        let size = u32::from_le_bytes(rest[4..8].try_into().unwrap()) as usize;
        out.push((&rest[8..8 + size], &rest[8 + size..8 + size + trailer]));
        rest = &rest[8 + size + trailer..];
    }
    out
}

pub fn pairs(block: &Block, mut undo: &[u8]) -> bool {
    BlockUndo::decode(&mut undo).is_ok_and(|undo| {
        undo.txs.len() + 1 == block.txs().len()
            && undo
                .txs
                .iter()
                .zip(&block.txs()[1..])
                .all(|(u, tx)| u.coins.len() == tx.inputs().len())
    })
}

#[derive(Clone, Default)]
pub struct RecordedIndex {
    pub nodes: Vec<(BlockHash, Option<BlockId>, u32)>,
    pub by_hash: HashMap<BlockHash, BlockId>,
}

impl RecordedIndex {
    pub fn block(&self, id: BlockId) -> BlockRef {
        let (hash, _, height) = self.nodes[id.0 as usize];
        BlockRef {
            id,
            hash,
            height,
        }
    }

    pub fn by_hash(&self, hash: &BlockHash) -> BlockRef {
        self.block(self.by_hash[hash])
    }

    pub fn path(&self, tip: BlockRef) -> Vec<BlockRef> {
        let mut out = vec![tip];
        while let Some(p) = self.nodes[out.last().unwrap().id.0 as usize].1 {
            out.push(self.block(p));
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

/// g's blocks, undo data, recorded filters and headers, and connect/disconnect order.
pub struct Recorded {
    pub index: RecordedIndex,
    pub blocks: HashMap<BlockHash, Arc<Block>>,
    pub undo: HashMap<BlockHash, Vec<u8>>,
    /// `getblockfilter <hash>`: the filter and its header.
    pub filters: HashMap<BlockHash, (Vec<u8>, String)>,
    pub events: Vec<(bool, BlockHash)>,
}

pub fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

impl Recorded {
    pub fn load() -> Arc<Self> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dashd-v24");
        let rows = json(&root.join("blocks.json"));
        let mut filters = HashMap::new();
        let mut heights = HashMap::new();
        for row in rows.as_array().unwrap() {
            let hash: BlockHash = row["hash"].as_str().unwrap().parse().unwrap();
            let f = &row["getblockfilter"];
            filters.insert(
                hash,
                (hex(f["filter"].as_str().unwrap()), f["header"].as_str().unwrap().to_owned()),
            );
            heights.insert(hash, row["getblockheader"]["height"].as_u64().unwrap() as u32);
        }
        let blk = fs::read(root.join("blocks/blk00000.dat")).unwrap();
        let rev = fs::read(root.join("blocks/rev00000.dat")).unwrap();
        let revs = records(&rev, 32);
        let mut index = RecordedIndex::default();
        let mut blocks = HashMap::new();
        let mut undo = HashMap::new();
        for (bytes, _) in records(&blk, 0) {
            let block = decode_block(&mut &bytes[..]).unwrap().into_value();
            let hash = block.hash();
            let height = heights[&hash];
            let parent = (height > 0).then(|| index.by_hash[&block.header().prev_hash()]);
            index.by_hash.insert(hash, BlockId(index.nodes.len() as u32));
            index.nodes.push((hash, parent, height));
            if parent.is_some() {
                let prev = block.header().prev_hash();
                let (bytes, _) = *revs
                    .iter()
                    .find(|(bytes, sum)| {
                        sha256d(&[prev.as_bytes().as_slice(), bytes].concat()).as_slice() == *sum
                            && pairs(&block, bytes)
                    })
                    .unwrap_or_else(|| panic!("no undo for {height}"));
                undo.insert(hash, bytes.to_vec());
            }
            blocks.insert(hash, Arc::new(block));
        }
        assert_eq!(blocks.len(), filters.len(), "every recorded block is in the file");
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
        Arc::new(Self {
            index,
            blocks,
            undo,
            filters,
            events,
        })
    }

    pub fn genesis(&self) -> BlockRef {
        self.index.block(BlockId(0))
    }
}

pub struct Source(pub Arc<Recorded>);

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

pub fn cfg(d_max: Option<u32>) -> DriverConfig {
    DriverConfig {
        journal_budget: 1 << 20,
        unbounded_delete_span: 64,
        d_max,
        cadence: 1000,
    }
}

pub fn guarded(engine: MemoryEngine) -> Arc<Db> {
    Arc::new(GuardedEngine::new(engine, GlobalFailStop::new(), Arc::new(RecordingSink::default())))
}

pub const DIR: &str = "/idx/blockfilter/basic";

/// The driver over a store and in-memory files, its feed and hub.
pub struct Run {
    pub rec: Arc<Recorded>,
    pub fs: Arc<MemFs>,
    pub sizes: Option<(u64, u64)>,
    pub db: Arc<Db>,
    pub feeds: ScriptedFeeds,
    pub hub: Arc<SubscriberHub>,
    pub d: Option<Driver>,
    pub flushes: u64,
}

impl Run {
    pub fn new(rec: &Arc<Recorded>, sizes: Option<(u64, u64)>) -> Self {
        let mut run = Self {
            rec: Arc::clone(rec),
            fs: Arc::default(),
            sizes,
            db: guarded(MemoryEngine::new(&BLOCKFILTER_DB)),
            feeds: ScriptedFeeds::new(4, vec![rec.genesis()]),
            hub: Arc::new(SubscriberHub::with_feeds(
                Arc::new(ScriptedFeeds::new(4, vec![rec.genesis()])),
                Arc::new(rec.index.clone()),
            )),
            d: None,
            flushes: 0,
        };
        run.reopen(vec![rec.genesis()], None);
        run.sync();
        run
    }

    pub fn data(&self) -> Data {
        let data = BlockFilterData::new(&*self.db, self.fs.clone(), PathBuf::from(DIR)).unwrap();
        match self.sizes {
            Some((file, chunk)) => data.with_file_sizes(file, chunk),
            None => data,
        }
    }

    /// Opens on the store with V's chain `chain`, flushed at its tip.
    pub fn reopen(&mut self, chain: Vec<BlockRef>, d_max: Option<u32>) {
        drop(self.d.take());
        let tip = *chain.last().unwrap();
        self.feeds = ScriptedFeeds::new(4, chain);
        self.hub = Arc::new(SubscriberHub::with_feeds(
            Arc::new(self.feeds.clone()),
            Arc::new(self.rec.index.clone()),
        ));
        self.flushes += 1;
        self.hub.publish_durable(self.flushes, tip, self.feeds.tip_seq());
        let d = SubscriberDriver::open(
            self.data(),
            Arc::clone(&self.db),
            Arc::clone(&self.hub),
            Arc::new(Source(Arc::clone(&self.rec))),
            cfg(d_max.or(Some(96))),
        )
        .expect("opens");
        self.d = Some(d);
    }

    pub fn sync(&mut self) {
        let live = (0..1000).any(|_| self.d().sync_step().expect("syncs") == SyncProgress::Live);
        assert!(live, "never live");
    }

    pub fn d(&mut self) -> &mut Driver {
        self.d.as_mut().unwrap()
    }

    pub fn drain(&mut self) {
        for _ in 0..1000 {
            match self.d().step().expect("steps") {
                SubStep::Progress => {}
                SubStep::Parked => self.publish(),
                SubStep::Idle => {
                    let d = self.d.as_ref().unwrap();
                    assert!(
                        d.journal_bytes() <= d.journal_len() as u64 * d.data().max_entry_bytes()
                    );
                    return;
                }
            }
        }
        panic!("never idle");
    }

    pub fn publish(&mut self) {
        self.flushes += 1;
        let tip = self.feeds.tip().unwrap();
        self.hub.publish_durable(self.flushes, tip, self.feeds.tip_seq());
    }

    pub fn event(&mut self, connect: bool, hash: &BlockHash) {
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

    pub fn flush(&mut self) {
        self.publish();
        let tip = self.feeds.tip().unwrap();
        self.drain();
        assert_eq!(self.d().durable(), Some(tip));
    }

    pub fn restart(&mut self) {
        self.flush();
        let chain = self.rec.index.path(self.feeds.tip().unwrap());
        self.reopen(chain, None);
        self.sync();
    }

    pub fn files(&self) -> FilterFiles {
        FilterFiles::new(self.fs.clone(), PathBuf::from(DIR))
    }

    /// The stored value for `b`: its height row if it names `b`, else its archive row.
    pub fn value(&self, b: &BlockRef) -> Option<(DbVal, bool)> {
        let row = self.db.get(HEIGHT, &height_key(b.height, &[])).unwrap();
        if let Some(v) = row.map(|r| HeightValue::decode(&r).unwrap())
            && v.block_hash == b.hash
        {
            return Some((v.filter, false));
        }
        let row = self.db.get(ARCHIVE, b.hash.as_bytes()).unwrap()?;
        Some((DbVal::decode(&row).unwrap(), true))
    }

    /// `b`'s stored hash, header and file bytes equal dashd's `getblockfilter`; whether the
    /// archive served it, and its position.
    pub fn check(&self, b: &BlockRef) -> (bool, FilePos) {
        let (want_filter, want_header) = &self.rec.filters[&b.hash];
        let (v, archived) = self.value(b).unwrap_or_else(|| panic!("no entry at {}", b.height));
        let pos = FilePos {
            file: v.pos.file as u32,
            offset: v.pos.pos,
        };
        assert_eq!(v.header.to_string(), *want_header, "header at {}", b.height);
        assert_eq!(v.filter_hash, filter_hash(want_filter), "filter hash at {}", b.height);
        let (hash, encoded) = self.files().read(pos).unwrap();
        assert_eq!((hash, &encoded), (b.hash, want_filter), "file entry at {}", b.height);
        (archived, pos)
    }

    /// Every recorded block is served with dashd's answer, at a distinct position.
    pub fn check_all(&self) -> usize {
        let mut seen = HashSet::new();
        let mut archived = 0;
        for hash in self.rec.filters.keys() {
            let (a, pos) = self.check(&self.rec.index.by_hash(hash));
            archived += usize::from(a);
            assert!(seen.insert(pos), "{pos:?} is referenced twice");
        }
        archived
    }

    /// Where each entry a height or archive row references ends.
    pub fn entry_ends(&self) -> Vec<FilePos> {
        let mut vals: Vec<DbVal> = self
            .db
            .iter_prefix(HEIGHT, &[])
            .map(|r| HeightValue::decode(&r.unwrap().1).unwrap().filter)
            .collect();
        vals.extend(
            self.db.iter_prefix(ARCHIVE, &[]).map(|r| DbVal::decode(&r.unwrap().1).unwrap()),
        );
        vals.iter()
            .map(|v| {
                let at = FilePos {
                    file: v.pos.file as u32,
                    offset: v.pos.pos,
                };
                let (hash, encoded) = self.files().read(at).unwrap();
                FilePos {
                    offset: at.offset + encode_entry(&hash, &encoded).len() as u64,
                    ..at
                }
            })
            .collect()
    }
}

pub fn replay(sizes: Option<(u64, u64)>, restart_every: Option<usize>) -> Run {
    let rec = Recorded::load();
    let mut run = Run::new(&rec, sizes);
    for (i, &(connect, hash)) in rec.events.iter().enumerate() {
        run.event(connect, &hash);
        if restart_every.is_some_and(|n| i % n == n - 1) {
            run.restart();
        }
    }
    run.flush();
    run
}
