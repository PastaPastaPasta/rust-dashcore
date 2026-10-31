//! Recovery on the probe engine and real block files (CH2 §4.3; E-b1.3): the checks without and
//! with a marker, the replay's convergence from any surviving subset (I2, I4), a crash at each
//! sub-batch and a re-run, the failures and Core's texts for them, the load's read errors, a
//! resumed wipe, and the block files from their durable infos.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use dash_chain::{
    BlockIndexRecord, BlockRef, ChainConfig, FailStop, FlatFilePos as IndexPos, HeaderChain,
    IndexAccess, Services, StaticSporks, Status,
};
use dash_clock::MockClock;
use dash_consensus::BlockId;
use dash_consensus::coins::{BlockUndo, Coin, TxUndo};
use dash_consensus::pow::{PowParams, check_proof_of_work};
use dash_consensus::primitives::{
    Block, BlockHash, BlockHeader, Header, MerkleRoot, OutPoint, Transaction, Tx, TxHash, TxIn,
    TxOut, TxType, core_bytes,
};
use dash_num::CompactTarget;
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dashd_store::flatfile::{
    BlockFiles, BlockFilesConfig, BlockFilesState, CoreUndoBytes, FlatFilePos, FlatFs, FlatRead,
    MAX_BLOCK_RECORD_SIZE, OpenFailed, OsFs,
};
use dashd_store::{Commit, DbSchema, ErrorKind, KvBatch, KvEngine, MemoryEngine, ValueCodec as _};

use super::*;
use crate::coins::model::db_coins;
use crate::coins::{CoinSrc, CoinsTable, OutPointKey, PackedCoin, splitmix64};
use crate::flush::RecordingObserver;
use crate::store::{AbortMark, EFFECT_CFS, Failure, GlobalFailStop, ProbeEngine, RecordingSink};

type Db<E = MemoryEngine> = GuardedEngine<ProbeEngine<E>>;
type Coins = BTreeMap<OutPointKey, Vec<u8>>;

const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];

/// A read failure [`FaultFs`] injects: the file does not open, or a read of the open file fails.
#[derive(Clone, Copy, Debug)]
enum Fault {
    Open(io::ErrorKind),
    Read(io::ErrorKind),
}

impl Fault {
    fn error(self) -> io::Error {
        match self {
            Self::Open(kind) => OpenFailed::wrap(io::Error::new(kind, "injected open failure")),
            Self::Read(kind) => io::Error::new(kind, "injected read failure"),
        }
    }
}

/// The OS file system, with reads that can fail: of every file, or of one.
#[derive(Default)]
struct FaultFs {
    os: OsFs,
    fail_reads: Mutex<Option<(Fault, Option<PathBuf>)>>,
}

impl FaultFs {
    fn fail_reads(&self, fault: Option<Fault>) {
        *self.fail_reads.lock().unwrap() = fault.map(|f| (f, None));
    }

    fn fail_reads_of(&self, path: &Path, fault: Fault) {
        *self.fail_reads.lock().unwrap() = Some((fault, Some(path.into())));
    }
}

impl FlatFs for FaultFs {
    fn link_count(&self, path: &Path) -> io::Result<Option<u64>> {
        self.os.link_count(path)
    }
    fn len(&self, path: &Path) -> io::Result<Option<u64>> {
        self.os.len(path)
    }
    fn create(&self, path: &Path) -> io::Result<()> {
        self.os.create(path)
    }
    /// By path, so that an opened file's reads fail as `fail_reads` says.
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FlatRead + '_>> {
        dash_kv::open_by_path(self, path)
    }
    fn read_exact_at(&self, path: &Path, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        if let Some((fault, only)) = &*self.fail_reads.lock().unwrap()
            && only.as_ref().is_none_or(|p| p == path)
        {
            return Err(fault.error());
        }
        self.os.read_exact_at(path, offset, buf)
    }
    fn write_all_at(&self, path: &Path, offset: u64, data: &[u8]) -> io::Result<()> {
        self.os.write_all_at(path, offset, data)
    }
    fn allocate(&self, path: &Path, offset: u64, len: u64) {
        self.os.allocate(path, offset, len);
    }
    fn truncate(&self, path: &Path, len: u64) -> io::Result<()> {
        self.os.truncate(path, len)
    }
    fn sync_data(&self, path: &Path) -> io::Result<()> {
        self.os.sync_data(path)
    }
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        self.os.sync_dir(dir)
    }
    fn copy(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.os.copy(from, to)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.os.rename(from, to)
    }
    fn remove(&self, path: &Path) -> io::Result<bool> {
        self.os.remove(path)
    }
    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        self.os.create_dir_all(dir)
    }
    fn available_space(&self, dir: &Path) -> io::Result<u64> {
        self.os.available_space(dir)
    }
}

struct Entry {
    hash: BlockHash,
    prev: Option<BlockId>,
    height: u32,
    status: Status,
    data: IndexPos,
    undo: IndexPos,
}

/// The block index CH1-node would load: an arena of entries.
#[derive(Default)]
struct Arena {
    entries: Vec<Entry>,
    by_hash: HashMap<BlockHash, BlockId>,
}

impl Arena {
    fn get(&self, id: BlockId) -> Option<&Entry> {
        self.entries.get(id.0 as usize)
    }

    fn get_mut(&mut self, b: BlockRef) -> &mut Entry {
        &mut self.entries[b.id.0 as usize]
    }
}

impl IndexAccess for Arena {
    fn lookup(&self, h: &BlockHash) -> Option<BlockId> {
        self.by_hash.get(h).copied()
    }

    fn hash(&self, id: BlockId) -> Option<BlockHash> {
        self.get(id).map(|e| e.hash)
    }

    fn ancestor(&self, mut id: BlockId, height: u32) -> Option<BlockId> {
        while self.get(id)?.height > height {
            id = self.get(id)?.prev?;
        }
        (self.get(id)?.height == height).then_some(id)
    }

    fn lca(&self, a: BlockId, b: BlockId) -> Option<BlockId> {
        let h = self.get(a)?.height.min(self.get(b)?.height);
        let (mut a, mut b) = (self.ancestor(a, h)?, self.ancestor(b, h)?);
        while a != b {
            (a, b) = (self.get(a)?.prev?, self.get(b)?.prev?);
        }
        Some(a)
    }

    fn height(&self, id: BlockId) -> Option<u32> {
        self.get(id).map(|e| e.height)
    }

    fn have_data_pos(&self, id: BlockId) -> Option<IndexPos> {
        self.get(id).filter(|e| e.status.has(Status::HAVE_DATA)).map(|e| e.data)
    }

    fn undo_pos(&self, id: BlockId) -> Option<IndexPos> {
        self.get(id).filter(|e| e.status.has(Status::HAVE_UNDO)).map(|e| e.undo)
    }

    fn status(&self, id: BlockId) -> Option<Status> {
        self.get(id).map(|e| e.status)
    }
}

/// A chain of real blocks and undo data in real block files, its index, and the coins after
/// each block.
struct Fixture {
    _dir: tempfile::TempDir,
    blocks: PathBuf,
    fs: Arc<FaultFs>,
    files: BlockFiles<FaultFs>,
    index: Arena,
    /// The coins after each block, by id: what Core's UTXO set holds there.
    coins: Vec<Coins>,
    undo: Vec<BlockUndo>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let blocks = dir.path().join("blocks");
        std::fs::create_dir_all(&blocks).unwrap();
        let fs = Arc::new(FaultFs::default());
        let cfg = BlockFilesConfig::new(&blocks, MAGIC);
        let files = BlockFiles::with_fs(fs.clone(), cfg, BlockFilesState::default());
        let mut fx = Self {
            _dir: dir,
            blocks,
            fs,
            files,
            index: Arena::default(),
            coins: Vec::new(),
            undo: Vec::new(),
        };
        // Genesis: its outputs are never in the UTXO set.
        let block = block(BlockHash::from([0; 32]), vec![coinbase(0, 0)]);
        fx.add(None, block, Coins::new(), BlockUndo::default());
        fx
    }

    fn genesis(&self) -> BlockRef {
        BlockRef {
            id: BlockId(0),
            hash: self.index.entries[0].hash,
            height: 0,
        }
    }

    /// A child of `parent`: a coinbase; `spends` transactions, each spending one of `parent`'s
    /// coins (the `tag`-th onwards) into a short and a long output; and one spending the first of
    /// those in the same block.
    fn extend(&mut self, parent: BlockRef, tag: u8, spends: usize) -> BlockRef {
        let height = parent.height + 1;
        let mut coins = self.coins(parent).clone();
        let picked: Vec<OutPointKey> =
            coins.keys().skip(usize::from(tag)).take(spends).copied().collect();
        let mut txs = vec![coinbase(height, tag)];
        for (i, k) in picked.iter().enumerate() {
            txs.push(spend(&[k.outpoint()], height, tag, i as u8));
        }
        if spends > 0 {
            let first = OutPoint {
                hash: txs[1].txid(),
                index: 0,
            };
            txs.push(spend(&[first], height, tag, 0xff));
        }
        let mut undo = BlockUndo::default();
        for tx in &txs {
            if !tx.is_coinbase() {
                let spent = tx.inputs().iter().map(|input| {
                    let bytes = coins.remove(&OutPointKey::new(&input.prevout)).expect("unspent");
                    Coin::decode(&mut &bytes[..]).unwrap()
                });
                undo.txs.push(TxUndo {
                    coins: spent.collect(),
                });
            }
            for (o, out) in tx.outputs().iter().enumerate() {
                let coin = CoinSrc::Output {
                    out,
                    height,
                    coinbase: tx.is_coinbase(),
                };
                if let Some(coin) = coin.pack().unwrap() {
                    let k = OutPointKey::new(&OutPoint {
                        hash: tx.txid(),
                        index: o as u32,
                    });
                    coins.insert(k, coin.as_bytes().to_vec());
                }
            }
        }
        let block = block(parent.hash, txs);
        self.add(Some(parent), block, coins, undo)
    }

    fn add(
        &mut self,
        parent: Option<BlockRef>,
        block: Block,
        coins: Coins,
        undo: BlockUndo,
    ) -> BlockRef {
        let height = parent.map_or(0, |p| p.height + 1);
        let pos = self.files.save_block(&core_bytes(&block), height, 1_700_000_000, None).unwrap();
        let mut status = Status(Status::HAVE_DATA);
        let mut undo_pos = IndexPos {
            file: -1,
            pos: 0,
        };
        if parent.is_some() {
            let prev = block.header().prev_hash();
            let at = self.files.write_undo(&undo_bytes(&undo), pos.file, height, prev.as_bytes());
            undo_pos = index_pos(at.unwrap());
            status = Status(Status::HAVE_DATA | Status::HAVE_UNDO);
        }
        let id = BlockId(self.index.entries.len() as u32);
        let b = BlockRef {
            id,
            hash: block.hash(),
            height,
        };
        self.index.entries.push(Entry {
            hash: b.hash,
            prev: parent.map(|p| p.id),
            height,
            status,
            data: index_pos(pos),
            undo: undo_pos,
        });
        self.index.by_hash.insert(b.hash, id);
        self.coins.push(coins);
        self.undo.push(undo);
        b
    }

    /// Writes `b`'s undo data again, changed by `edit`, and points the index at it.
    fn rewrite_undo(&mut self, b: BlockRef, edit: impl FnOnce(&mut BlockUndo)) {
        let mut undo = self.undo[b.id.0 as usize].clone();
        edit(&mut undo);
        let e = self.index.get(b.id).unwrap();
        let prev = self.index.get(e.prev.unwrap()).unwrap().hash;
        let at = self.files.write_undo(
            &undo_bytes(&undo),
            e.data.file as u32,
            b.height,
            prev.as_bytes(),
        );
        self.index.get_mut(b).undo = index_pos(at.unwrap());
    }

    fn coins(&self, b: BlockRef) -> &Coins {
        &self.coins[b.id.0 as usize]
    }

    /// A fresh engine holding `coins`, the block files' durable infos, and `meta`'s records.
    fn engine(&mut self, coins: &Coins, meta: &Meta) -> Db {
        self.seed(GuardedEngine::probe(Arc::new(RecordingSink::default())), coins, meta)
    }

    /// A fresh engine holding the coins at `at`, and `meta`'s records.
    fn engine_at(&mut self, at: BlockRef, meta: &Meta) -> Db {
        let coins = self.coins(at).clone();
        self.engine(&coins, meta)
    }

    /// The coins at `old`, under the marker of a flush from `old` to `new`.
    fn interrupted(&mut self, old: BlockRef, new: BlockRef) -> Db {
        self.engine_at(old, &marker(new, Some(old)))
    }

    /// Writes `coins`, the block files' durable infos, and `meta`'s records into `db`.
    fn seed<E: KvEngine>(&mut self, db: Db<E>, coins: &Coins, meta: &Meta) -> Db<E> {
        let state = self.files.sync_and_state().unwrap();
        let table = CoinsTable::default();
        let mut b = db.batch();
        for (k, coin) in coins {
            table.put(&mut b, k, coin);
        }
        let cf = CHAINSTATE_SCHEMA.cf("block_files").unwrap();
        for (file, info) in state.infos.iter().enumerate() {
            b.put(cf, &(file as u32).to_be_bytes(), &info.to_value_bytes());
        }
        Meta::put_last_file(&mut b, state.last_file);
        if let Some(tip) = meta.tip {
            Meta::put_tip(&mut b, &tip);
        }
        if let Some((new, old)) = meta.head_blocks {
            Meta::put_head_blocks(&mut b, &new, old.as_ref());
        }
        if let Some(evo) = meta.evo_tip {
            Meta::put_evo_tip(&mut b, &evo);
        }
        Meta::put_wiping(&mut b, meta.wiping);
        db.write(b, Commit::Durable).unwrap();
        db
    }

    fn open<'a, E: KvEngine>(
        &self,
        db: &'a Db<E>,
        obs: &'a RecordingObserver,
    ) -> Result<Opened<'a, ProbeEngine<E>, FaultFs>, InitError> {
        ChainstateOpen::open(db, self.fs.clone(), BlockFilesConfig::new(&self.blocks, MAGIC), obs)
    }

    fn recover<E: KvEngine>(
        &self,
        db: &Db<E>,
        obs: &RecordingObserver,
        crash: CrashRoll,
    ) -> Result<RecoveredChainstate<FaultFs>, InitError> {
        let hub = SubscriberHub::new();
        self.open(db, obs)?.recover(&self.index, &hub, config(crash))
    }

    /// The init error that recovery from `db` ends with, in `open` or in `recover`.
    #[track_caller]
    fn recover_err(&self, db: &Db, obs: &RecordingObserver) -> InitError {
        self.recover(db, obs, never()).err().expect("an init error")
    }

    /// The init error that recovery from `db` ends with when every block-file read fails with
    /// `fault` after `open`.
    #[track_caller]
    fn recover_with_read_fault(&self, db: &Db, obs: &RecordingObserver, fault: Fault) -> InitError {
        let opened = self.open(db, obs).unwrap();
        self.fs.fail_reads(Some(fault));
        let err = opened.recover(&self.index, &SubscriberHub::new(), config(never())).err();
        self.fs.fail_reads(None);
        err.expect("an init error")
    }
}

fn config(crash: CrashRoll) -> RecoveryConfig {
    RecoveryConfig {
        pow: pow(),
        // A few ops per sub-batch.
        sub_batch: 200,
        crash,
    }
}

fn regtest() -> ChainSetup {
    ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap()
}

/// Regtest's proof-of-work parameters, which every fixture block meets.
fn pow() -> PowParams {
    static POW: LazyLock<PowParams> =
        LazyLock::new(|| PowParams::from_chain_params(&regtest()).unwrap());
    *POW
}

/// Regtest's limit, `0x207fffff`: about every other hash meets it.
const REGTEST_BITS: u32 = 0x207f_ffff;

fn never() -> CrashRoll {
    CrashRoll::seeded(0, 0)
}

/// A roll that first comes up after sub-batch `k` (0-based).
fn crash_at(k: usize) -> CrashRoll {
    let ratio = k as u32 + 1;
    (0..)
        .map(|seed| CrashRoll::seeded(ratio, seed))
        .find(|roll| {
            let mut roll = roll.clone();
            (0..=k).position(|_| roll.roll()) == Some(k)
        })
        .expect("a seed")
}

/// Steps `r` until `Done` or `Crash`.
fn drain<E: KvEngine>(r: &mut Recovery<'_, E, FaultFs>) -> Vec<RecoveryStep> {
    let mut steps = Vec::new();
    loop {
        let step = r.step().unwrap();
        steps.push(step);
        if matches!(step, RecoveryStep::Done | RecoveryStep::Crash) {
            return steps;
        }
        assert!(steps.len() < 100_000, "recovery makes no progress");
    }
}

fn sub_batches(steps: &[RecoveryStep]) -> Vec<u32> {
    let sub = |s: &RecoveryStep| match s {
        RecoveryStep::SubBatch(i) => Some(*i),
        _ => None,
    };
    steps.iter().filter_map(sub).collect()
}

/// Core's bytes for `undo`, as the block-file writer takes them.
fn undo_bytes(undo: &BlockUndo) -> CoreUndoBytes {
    CoreUndoBytes::from_raw(undo.core_bytes().as_slice())
}

fn index_pos(p: FlatFilePos) -> IndexPos {
    IndexPos {
        file: p.file as i32,
        pos: p.pos,
    }
}

/// A block of `txs` on `prev`, with their merkle root: siblings differ in hash.
fn block(prev: BlockHash, txs: Vec<Arc<Tx>>) -> Block {
    let header = BlockHeader {
        version: 4,
        prev_hash: prev,
        merkle_root: MerkleRoot::from([0; 32]),
        time: 1_700_000_000,
        bits: REGTEST_BITS,
        nonce: 0,
    };
    mine(header, txs)
}

/// `txs` under `header` and their merkle root, at the first nonce from `header`'s whose hash
/// meets regtest's limit, whatever `header`'s own `nBits` claims.
fn mine(mut header: BlockHeader, txs: Vec<Arc<Tx>>) -> Block {
    (header.merkle_root, _) = Block::new(Header::new(header), txs.clone()).merkle();
    loop {
        let block = Block::new(Header::new(header), txs.clone());
        if meets(&block.hash(), REGTEST_BITS) {
            return block;
        }
        header.nonce += 1;
    }
}

/// Core's `CheckProofOfWork` on regtest.
fn meets(hash: &BlockHash, bits: u32) -> bool {
    check_proof_of_work(hash.as_bytes(), CompactTarget::new(bits), &pow())
}

/// A coinbase with a spendable output and an unspendable one.
fn coinbase(height: u32, tag: u8) -> Arc<Tx> {
    let mut script_sig = height.to_le_bytes().to_vec();
    script_sig.push(tag);
    Arc::new(Tx::new(Transaction {
        version: 2,
        tx_type: TxType::new(0),
        inputs: vec![TxIn {
            prevout: OutPoint {
                hash: TxHash::from([0; 32]),
                index: u32::MAX,
            },
            script_sig: script_sig.into(),
            sequence: u32::MAX,
        }],
        outputs: vec![
            TxOut {
                value: 50_000 + i64::from(height),
                script_pubkey: vec![0x76; 25].into(),
            },
            TxOut {
                value: 0,
                script_pubkey: vec![0x6a, tag].into(),
            },
        ],
        lock_time: 0,
        extra_payload: Vec::new(),
    }))
}

/// A transaction spending `inputs` into a short output and a long one.
fn spend(inputs: &[OutPoint], height: u32, tag: u8, i: u8) -> Arc<Tx> {
    Arc::new(Tx::new(Transaction {
        version: 2,
        tx_type: TxType::new(0),
        inputs: inputs
            .iter()
            .map(|prevout| TxIn {
                prevout: *prevout,
                script_sig: Default::default(),
                sequence: u32::MAX,
            })
            .collect(),
        outputs: vec![
            TxOut {
                value: 1_000,
                script_pubkey: vec![0x51; 25].into(),
            },
            TxOut {
                value: 2_000,
                script_pubkey: vec![0x52; 80].into(),
            },
        ],
        lock_time: height << 16 | u32::from(tag) << 8 | u32::from(i),
        extra_payload: Vec::new(),
    }))
}

/// The database's coins are `expected`; a mismatch prints the keys that differ.
#[track_caller]
fn assert_coins<E: KvEngine>(db: &Db<E>, expected: &Coins, context: &str) {
    let got: Coins = db_coins(db).into_iter().collect();
    let differ: Vec<String> = got
        .keys()
        .chain(expected.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|k| got.get(k) != expected.get(k))
        .map(|k| {
            let state = if got.contains_key(k) {
                "got"
            } else {
                "missing"
            };
            format!("{}:{} {state}", k.txid(), k.vout())
        })
        .collect();
    assert!(
        differ.is_empty(),
        "{context}: {} of {} keys differ: {differ:?}",
        differ.len(),
        expected.len()
    );
}

#[track_caller]
fn assert_unable(err: &InitError, line: &str) {
    assert!(matches!(err, InitError::UnableToReplay { detail } if detail == line), "{err:?}");
    assert_eq!(err.to_string(), UNABLE_TO_REPLAY);
}

#[track_caller]
fn assert_store_err(err: &InitError, kind: ErrorKind) {
    assert!(matches!(err, InitError::Store(e) if e.kind() == Some(kind)), "{err:?}");
}

/// The observer's last marks are a failed `op` and the abort with `message`.
#[track_caller]
fn assert_abort_tail(obs: &RecordingObserver, op: &'static str, message: &str) {
    let marks = obs.marks();
    assert_eq!(
        marks[marks.len().saturating_sub(2)..],
        [
            ObsMark::StoreFailed {
                op,
            },
            ObsMark::Abort {
                message: message.into(),
            },
        ]
    );
}

fn marker(new: BlockRef, old: Option<BlockRef>) -> Meta {
    Meta {
        head_blocks: Some((new.hash, old.map(|o| o.hash))),
        evo_tip: Some(new.hash),
        ..Meta::default()
    }
}

fn at(b: BlockRef) -> Meta {
    Meta {
        tip: Some(b.hash),
        evo_tip: Some(b.hash),
        ..Meta::default()
    }
}

/// G ← A1 ← … ← A5, and B4 ← B5 ← B6 (`b[0..3]`) forking at A3.
fn forked() -> (Fixture, Vec<BlockRef>, Vec<BlockRef>) {
    let mut fx = Fixture::new();
    let mut a = vec![fx.genesis()];
    for spends in [0, 1, 2, 2, 3] {
        let tip = *a.last().unwrap();
        a.push(fx.extend(tip, 0, spends));
    }
    let mut b = Vec::new();
    let mut tip = a[3];
    for spends in [2, 1, 2] {
        tip = fx.extend(tip, 1, spends);
        b.push(tip);
    }
    (fx, a, b)
}

fn hashes(bs: &[BlockRef]) -> Vec<BlockHash> {
    bs.iter().map(|b| b.hash).collect()
}

#[test]
fn the_fixtures_branches_differ() {
    let (fx, a, b) = forked();
    let (old, new) = (fx.coins(a[5]), fx.coins(b[2]));
    assert_ne!(old, new);
    assert!(old.keys().any(|k| !new.contains_key(k)) && new.keys().any(|k| !old.contains_key(k)));
    assert_eq!(fx.index.lca(a[5].id, b[2].id), Some(a[3].id));
}

// I2, I4: whatever subset of the flush and of earlier recoveries survived, the replay ends at
// `new`. Each key starts at its value in any state on the replay's path (review r1 S4).
#[test]
fn a_replay_converges_from_any_surviving_subset() {
    let (mut fx, a, b) = forked();
    let path: Vec<Coins> =
        [a[5], a[4], a[3], b[0], b[1], b[2]].iter().map(|&x| fx.coins(x).clone()).collect();
    let keys: BTreeSet<OutPointKey> = path.iter().flat_map(|c| c.keys().copied()).collect();
    for seed in 0..48u64 {
        let mut rng = seed;
        let start: Coins = keys
            .iter()
            .filter_map(|k| {
                let from = &path[(splitmix64(&mut rng) % path.len() as u64) as usize];
                from.get(k).map(|v| (*k, v.clone()))
            })
            .collect();
        let db = fx.engine(&start, &marker(b[2], Some(a[5])));
        let r = fx.recover(&db, &RecordingObserver::default(), never()).unwrap();
        assert_eq!(r.tip, Some(b[2]), "seed {seed}");
        assert_eq!(r.replayed, hashes(&b));
        assert_coins(&db, fx.coins(b[2]), &format!("seed {seed}"));
        let meta = Meta::read(&db).unwrap();
        assert_eq!(
            (meta.tip, meta.head_blocks, meta.evo_tip),
            (Some(b[2].hash), None, Some(b[2].hash))
        );
        assert_eq!(r.meta, meta);
    }
}

/// CH7 D7 (§7.2 L0): the B1-only image a kill between a split flush's B1 and Bk leaves, with real
/// evo rows: the window's credit data at `new` and the deletes of `new`'s mined rows beside the
/// coins at `old`, the marker and `meta.evo_tip = new`, no tip. Recovery replays the coins alone,
/// ends at `new`, and leaves every evo row as B1 wrote it.
#[test]
fn a_b1_only_image_recovers_the_coins_and_keeps_the_evo_rows() {
    use crate::evo::codec::{self as evo_codec, EvoCf};
    use crate::evo::{EvoLayer, testkit};
    use crate::flush::{EvoWindow, StateDelta};
    use dash_dml::creditpool::CreditBlockData;
    use dash_dml::store::MinedErase;
    use dash_params::LlmqType;
    use dash_primitives::QuorumHash;

    let (mut fx, a, b) = forked();
    let (old, new) = (a[5], b[2]);
    let db = fx.interrupted(old, new);
    // `new`'s mined rows, on disk before B1, which B1's tombstones delete.
    let q = QuorumHash::from([5; 32]);
    let mined = [
        evo_codec::mined_key(LlmqType::Llmq50_60, &q),
        evo_codec::index_key(LlmqType::Llmq50_60, None, new.height),
    ];
    let mut batch = db.batch();
    for key in &mined {
        let (cf, key) = evo_codec::split_mined_key(key);
        batch.put(cf.cf(), key, &[1]);
    }
    db.write(batch, Commit::Durable).unwrap();
    let delta = dash_dml::EvoDelta {
        block: new.hash,
        height: new.height,
        dml: None,
        mined_commitments: Vec::new(),
        credit_data: Some(CreditBlockData {
            balance: 7,
            unlocked: 0,
        }),
        credit_pool: None,
        mnhf_signals: None,
        erased_mined: vec![MinedErase {
            llmq_type: LlmqType::Llmq50_60,
            quorum_hash: q,
            mined_height: new.height,
            rotation_index: None,
        }],
        evo_tip: Some(new.hash),
    };
    let mut layer = EvoLayer::new();
    layer.commit(testkit::codec().rows(&delta).unwrap(), &db).unwrap();
    let mut batch = db.batch();
    layer.take().write_b1(&mut batch);
    db.write(batch, Commit::Durable).unwrap();
    type Rows = Vec<(Vec<u8>, Vec<u8>)>;
    let evo_rows = |db: &Db| -> Vec<(&str, Rows)> {
        ["mn_diff", "mn_snapshot", "credit_pool", "mnhf", "llmq_mined", "llmq_mined_h"]
            .into_iter()
            .map(|name| {
                let cf = CHAINSTATE_SCHEMA.cf(name).unwrap();
                (name, db.iter_prefix(cf, &[]).map(Result::unwrap).collect())
            })
            .collect()
    };
    let before = evo_rows(&db);
    let data = &before[2].1;
    assert_eq!(data.len(), 1, "the credit data");
    assert_eq!(data[0].0, EvoCf::CreditData.key(&new.hash).as_bytes());
    assert!(before[4].1.is_empty() && before[5].1.is_empty(), "the tombstones' deletes");

    let r = fx.recover(&db, &RecordingObserver::default(), never()).unwrap();
    assert_eq!((r.tip, r.replayed), (Some(new), hashes(&b)));
    assert_coins(&db, fx.coins(new), "the coins replayed");
    let meta = Meta::read(&db).unwrap();
    assert_eq!((meta.tip, meta.head_blocks, meta.evo_tip), (Some(new.hash), None, Some(new.hash)));
    assert_eq!(evo_rows(&db), before, "the evo rows as B1 wrote them");
}

#[test]
fn a_replay_steps_through_both_legs_blind_and_logs_cores_lines() {
    let (mut fx, a, b) = forked();
    let db = fx.interrupted(a[5], b[2]);
    let obs = RecordingObserver::default();
    let hub = SubscriberHub::new();
    let mut r = fx.open(&db, &obs).unwrap().recovery(&fx.index, &hub, config(never()));
    let Ok(RecoveryStep::Checked(state)) = r.step() else {
        panic!("checked first")
    };
    assert_eq!(
        state,
        MarkerState::Replay {
            new: b[2],
            old: Some(a[5]),
            fork: Some(a[3]),
        }
    );
    let reads = db.inner().reads();
    let steps = drain(&mut r);
    assert_eq!(db.inner().reads(), reads, "blind: the replay reads no key");
    let blocks: Vec<RecoveryStep> = steps
        .iter()
        .copied()
        .filter(|s| matches!(s, RecoveryStep::Undo(_) | RecoveryStep::Forward(_)))
        .collect();
    use RecoveryStep::{Forward, Undo};
    assert_eq!(blocks, [Undo(a[5]), Undo(a[4]), Forward(b[0]), Forward(b[1]), Forward(b[2])]);
    let subs = sub_batches(&steps);
    assert!(subs.len() > 2, "{subs:?}");
    assert!(subs.iter().enumerate().all(|(i, s)| *s == i as u32));
    assert_eq!(
        steps[steps.len() - 3..],
        [RecoveryStep::PreFinal, RecoveryStep::Final, RecoveryStep::Done]
    );
    assert_eq!(r.step().unwrap(), RecoveryStep::Done);
    assert_eq!(r.replayed(), hashes(&b));
    assert_eq!(hub.t_ack().map(|t| (t.tip, t.tip_seq)), Some((b[2], 0)));
    let line = |what: &str, x: BlockRef| format!("{what} {} ({})", x.hash, x.height);
    assert_eq!(
        obs.lines(LogCat::Always),
        [
            "Replaying blocks".to_string(),
            line("Rolling back", a[5]),
            line("Rolling back", a[4]),
            line("Rolling forward", b[0]),
            line("Rolling forward", b[1]),
            line("Rolling forward", b[2]),
        ]
    );
    // One line per sub-batch, then the final batch's: the ops left go into it, as Core's do.
    let batches = obs.lines(LogCat::CoinDb);
    assert_eq!(batches.len(), subs.len() + 1, "{batches:?}");
    assert!(batches[..subs.len()].iter().all(|l| l.starts_with("Writing partial batch of")));
    assert!(batches[subs.len()].starts_with("Writing final batch of"));
    let marks = obs.marks();
    assert_eq!(marks.first(), Some(&ObsMark::RecoveryBegin));
    assert_eq!(marks.last(), Some(&ObsMark::RecoveryEnd));
    assert!(obs.lines(LogCat::Error).is_empty());
}

#[test]
fn a_first_flush_replays_from_height_one_and_a_rollback_only_has_no_forward_leg() {
    let (mut fx, a, _) = forked();
    // `old` null: heights 1 to 3; genesis is never replayed.
    let db = fx.engine(&Coins::new(), &marker(a[3], None));
    let r = fx.recover(&db, &RecordingObserver::default(), never()).unwrap();
    assert_eq!(r.replayed, hashes(&a[1..=3]));
    assert_coins(&db, fx.coins(a[3]), "first flush");
    // `new` an ancestor of `old`: the undo leg alone. Core never reads `new`'s block there, so
    // `new` needs no data (review r1 n5).
    fx.index.get_mut(a[3]).status = Status(Status::HAVE_UNDO);
    let db = fx.interrupted(a[5], a[3]);
    let obs = RecordingObserver::default();
    let r = fx.recover(&db, &obs, never()).unwrap();
    assert!(r.replayed.is_empty());
    assert_eq!(r.tip, Some(a[3]));
    assert_coins(&db, fx.coins(a[3]), "rollback only");
    assert_eq!(obs.lines(LogCat::Always).len(), 3, "replaying, and two roll-backs");
}

// I4: a crash after any sub-batch leaves the marker, and the next recovery runs again from the
// top, after a kill or a power loss (review r1 S4).
#[test]
fn a_crash_after_each_sub_batch_keeps_the_marker_and_a_rerun_converges() {
    let (mut fx, a, b) = forked();
    let db = fx.interrupted(a[5], b[2]);
    let hub = SubscriberHub::new();
    let obs = RecordingObserver::default();
    let mut r = fx.open(&db, &obs).unwrap().recovery(&fx.index, &hub, config(never()));
    let n = sub_batches(&drain(&mut r)).len();
    drop(r);
    assert!(n > 2);
    for k in 0..n {
        for power_loss in [false, true] {
            let context = format!("crash after sub-batch {k}, power loss: {power_loss}");
            let db = fx.interrupted(a[5], b[2]);
            let obs = RecordingObserver::default();
            let hub = SubscriberHub::new();
            let mut r = fx.open(&db, &obs).unwrap().recovery(&fx.index, &hub, config(crash_at(k)));
            let steps = drain(&mut r);
            assert_eq!(steps.last(), Some(&RecoveryStep::Crash), "{context}");
            assert_eq!(sub_batches(&steps).len(), k, "{context}");
            assert_eq!(
                obs.lines(LogCat::CoinDb).len(),
                k + 1,
                "{context}: written before the roll"
            );
            assert_eq!(r.step().unwrap(), RecoveryStep::Crash, "nothing more is written");
            assert_eq!(obs.lines(LogCat::Always).last().map(String::as_str), Some(CRASH_MESSAGE));
            assert!(!obs.marks().contains(&ObsMark::RecoveryEnd));
            assert_eq!(hub.t_ack(), None);
            let meta = Meta::read(&db).unwrap();
            assert_eq!((meta.tip, meta.head_blocks), (None, Some((b[2].hash, Some(a[5].hash)))));
            drop(r);
            let db = if power_loss {
                GuardedEngine::new(
                    db.inner().restart(),
                    GlobalFailStop::new(),
                    Arc::new(RecordingSink::default()),
                )
            } else {
                db
            };
            if k == 0 {
                let err = fx.recover(&db, &RecordingObserver::default(), crash_at(0)).err();
                assert!(matches!(err, Some(InitError::Crashed)), "{err:?}");
            }
            let r = fx.recover(&db, &RecordingObserver::default(), never()).unwrap();
            assert_eq!(r.tip, Some(b[2]));
            assert_coins(&db, fx.coins(b[2]), &context);
        }
    }
}

// §4.1: recovery's final batch is Durable, after a `sync_wal` over its sub-batches (E4, E21). A
// power loss right after recovery keeps the converged chainstate; a failed barrier before the
// final batch stops recovery with the marker kept and nothing finalized.
#[test]
fn a_finished_recovery_survives_a_power_loss_and_finalizes_only_after_the_barrier() {
    let (mut fx, a, b) = forked();
    let db = fx.interrupted(a[5], b[2]);
    let (obs, hub) = (RecordingObserver::default(), SubscriberHub::new());
    let steps = drain(&mut fx.open(&db, &obs).unwrap().recovery(&fx.index, &hub, config(never())));
    let tail = [RecoveryStep::PreFinal, RecoveryStep::Final, RecoveryStep::Done];
    let replay = steps.strip_suffix(&tail).expect("step 9 ends recovery").to_vec();
    let restarted = |db: &Db| {
        GuardedEngine::new(
            db.inner().restart(),
            GlobalFailStop::new(),
            Arc::new(RecordingSink::default()),
        )
    };
    let db = restarted(&db);
    let meta = Meta::read(&db).unwrap();
    assert_eq!((meta.tip, meta.head_blocks), (Some(b[2].hash), None));
    assert_coins(&db, fx.coins(b[2]), "after a power loss");

    let db = fx.interrupted(a[5], b[2]);
    let obs = RecordingObserver::default();
    let mut r = fx.open(&db, &obs).unwrap().recovery(&fx.index, &hub, config(never()));
    // The open's `sync_wal` was the first barrier; the next is step 9's, after the whole replay.
    db.inner().fail_barrier_at(1);
    let mut steps = Vec::new();
    let err = loop {
        match r.step() {
            Ok(RecoveryStep::Done) => panic!("recovery ended past the failed barrier: {steps:?}"),
            Ok(step) => steps.push(step),
            Err(err) => break err,
        }
    };
    assert_eq!(steps, replay);
    assert_store_err(&err, ErrorKind::Io);
    assert_abort_tail(&obs, "sync_wal", OPEN_FAILED);
    assert!(!obs.lines(LogCat::CoinDb).iter().any(|l| l.starts_with("Writing final")));
    let meta = Meta::read(&restarted(&db)).unwrap();
    assert_eq!((meta.tip, meta.head_blocks), (None, Some((b[2].hash, Some(a[5].hash)))));
}

#[test]
fn without_a_marker_recovery_checks_and_writes_nothing() {
    let (mut fx, a, _) = forked();
    let db = fx.engine_at(a[5], &at(a[5]));
    let epoch = db.write_epoch();
    let obs = RecordingObserver::default();
    let hub = SubscriberHub::new();
    let r = fx.open(&db, &obs).unwrap().recover(&fx.index, &hub, config(crash_at(0))).unwrap();
    assert_eq!((r.tip, r.replayed.len()), (Some(a[5]), 0));
    assert_eq!(db.write_epoch(), epoch);
    assert_coins(&db, fx.coins(a[5]), "");
    assert_eq!(hub.t_ack().map(|t| t.tip), Some(a[5]));
    assert_eq!(obs.marks(), [ObsMark::RecoveryBegin, ObsMark::RecoveryEnd]);
    // An empty chainstate: no tip, and no T_ack.
    let db = fx.engine(&Coins::new(), &Meta::default());
    let hub = SubscriberHub::new();
    let r = fx.open(&db, &obs).unwrap().recover(&fx.index, &hub, config(never())).unwrap();
    assert_eq!(r.tip, None);
    assert_eq!(hub.t_ack(), None);
}

// §4.3 step 5: no tip ⇒ an empty chainstate, the evo state included (review r1 n6).
#[test]
fn without_a_marker_each_inconsistency_is_its_init_error() {
    let (mut fx, a, _) = forked();
    let unknown = BlockHash::from([0xee; 32]);
    let cases = [
        (a[5], Meta::default(), "Corrupted chainstate database: no tip, but `coins` holds records"),
        (
            a[5],
            Meta {
                evo_tip: Some(a[4].hash),
                ..at(a[5])
            },
            "Found EvoDB inconsistency, you must reindex to continue",
        ),
        (
            fx.genesis(),
            Meta {
                evo_tip: Some(a[5].hash),
                ..Meta::default()
            },
            "Error initializing block database",
        ),
        (
            a[5],
            Meta {
                tip: Some(unknown),
                evo_tip: Some(unknown),
                ..Meta::default()
            },
            "Error initializing block database",
        ),
        // The unknown tip first, as Core's `LoadChainTip` finds it before any evo check.
        (
            a[5],
            Meta {
                tip: Some(unknown),
                evo_tip: Some(a[4].hash),
                ..Meta::default()
            },
            "Error initializing block database",
        ),
    ];
    for (coins_at, meta, message) in cases {
        let db = fx.engine_at(coins_at, &meta);
        let err = fx.recover_err(&db, &RecordingObserver::default());
        assert!(err.to_string().starts_with(message), "{err}");
        assert!(db.latch().tripped().is_none());
    }
}

// DEC-202: no durable tip is a logically empty store. A kill before the first flush can leave
// effect records, which recovery keeps as it keeps effects above a tip; any other record is still
// corruption.
#[test]
fn without_a_tip_effect_records_stay() {
    let (mut fx, a, _) = forked();
    let obs = RecordingObserver::default();
    let put = |db: &Db, name: &str| {
        let mut b = db.batch();
        b.put(CHAINSTATE_SCHEMA.cf(name).unwrap(), &[1], &[2]);
        db.write(b, Commit::Durable).unwrap();
    };
    let records =
        |db: &Db, name: &str| db.iter_prefix(CHAINSTATE_SCHEMA.cf(name).unwrap(), &[]).count();
    for name in EFFECT_CFS {
        let db = fx.engine(&Coins::new(), &Meta::default());
        put(&db, name);
        let epoch = db.write_epoch();
        let hub = SubscriberHub::new();
        let r = fx.open(&db, &obs).unwrap().recover(&fx.index, &hub, config(never())).unwrap();
        assert_eq!((r.tip, r.replayed.len(), hub.t_ack()), (None, 0, None), "{name}");
        assert_eq!((db.write_epoch(), records(&db, name)), (epoch, 1), "{name}");
    }
    // The evo state is Core's evodb with no blocks (D71), and coins beside an effect corruption
    // (D71's declared deviation).
    let evo_detail = |err: InitError| match err {
        InitError::EvoWithoutBlocks {
            detail,
        } => detail,
        err => panic!("{err}"),
    };
    let db = fx.engine(&Coins::new(), &Meta::default());
    put(&db, "mn_diff");
    let err = fx.recover_err(&db, &obs);
    assert_eq!(err.to_string(), "Error initializing block database");
    assert_eq!(evo_detail(err), "no tip, but `mn_diff` holds records");
    // The mined-commitment rows are transactional (CH7 Q1): with no tip they are evo state.
    for name in ["llmq_mined", "llmq_mined_h"] {
        let db = fx.engine(&Coins::new(), &Meta::default());
        put(&db, name);
        let detail = evo_detail(fx.recover_err(&db, &obs));
        assert_eq!(detail, format!("no tip, but `{name}` holds records"));
    }
    // Beside coins too, as Core's check reads only the evodb (CH7a-4a r2 n-1).
    let db = fx.engine_at(a[5], &Meta::default());
    put(&db, "mn_diff");
    assert_eq!(evo_detail(fx.recover_err(&db, &obs)), "no tip, but `mn_diff` holds records");
    let db = fx.engine_at(a[5], &Meta::default());
    put(&db, "llmq_snapshot");
    let err = fx.recover_err(&db, &obs).to_string();
    assert!(err.starts_with("Corrupted chainstate database: no tip, but `coins` holds"), "{err}");
    // The planted negative restores the old rule.
    #[cfg(feature = "planted-bugs")]
    {
        let mut db = fx.engine(&Coins::new(), &Meta::default());
        put(&db, "llmq_snapshot");
        db.plant(crate::Planted::NoTipEmpty);
        let detail = evo_detail(fx.recover_err(&db, &obs));
        assert_eq!(detail, "no tip, but `llmq_snapshot` holds records");
    }
}

/// Recovery from `db` fails with `line`, after the error lines `causes`, and writes nothing.
#[track_caller]
fn assert_check_fails(fx: &Fixture, db: &Db, causes: &[&str], line: &str) -> RecordingObserver {
    let epoch = db.write_epoch();
    let obs = RecordingObserver::default();
    let err = fx.recover_err(db, &obs);
    assert_unable(&err, line);
    assert_eq!(obs.lines(LogCat::Error), [causes, &[line]].concat());
    assert_eq!(db.write_epoch(), epoch);
    assert!(db.latch().tripped().is_none());
    obs
}

// §3.5.1: Core's line for each failed check, in Core's order. `new` has no data throughout, which
// is checked last, as Core finds it only in the forward leg's read, with that read's lines
// (review r1 n4, n5; D-b1-g).
#[test]
fn each_failed_marker_check_logs_cores_line_and_writes_nothing() {
    let (mut fx, a, b) = forked();
    let unknown = BlockHash::from([0xee; 32]);
    let new = b[2];
    fx.index.get_mut(new).status = Status(Status::HAVE_UNDO);
    // A marker of three hashes: Core's "unknown inconsistent state", before "Replaying blocks".
    let db = fx.interrupted(a[5], new);
    let mut w = db.batch();
    w.put(DbSchema::META, b"head_blocks", &[7; 96]);
    db.write(w, Commit::Durable).unwrap();
    let obs = assert_check_fails(&fx, &db, &[], "ReplayBlocks(): unknown inconsistent state");
    assert!(obs.lines(LogCat::Always).is_empty());
    let cases = [
        (
            Meta {
                head_blocks: Some((unknown, Some(a[5].hash))),
                ..marker(new, None)
            },
            None,
            "ReplayBlocks(): reorganization to unknown block requested".to_string(),
        ),
        (
            Meta {
                head_blocks: Some((new.hash, Some(unknown))),
                ..marker(new, None)
            },
            None,
            "ReplayBlocks(): reorganization from unknown block requested".into(),
        ),
        (
            Meta {
                evo_tip: Some(a[5].hash),
                ..marker(new, Some(a[5]))
            },
            None,
            "ReplayBlocks(DASH): Found EvoDB inconsistency, you must reindex to continue".into(),
        ),
        (
            marker(new, Some(a[5])),
            Some("ReadBlockFromDisk: OpenBlockFile failed for FlatFilePos(nFile=-1, nPos=0)"),
            format!("RollforwardBlock(): ReadBlockFromDisk failed at 6, hash={}", new.hash),
        ),
    ];
    for (meta, cause, line) in cases {
        let db = fx.engine_at(a[5], &meta);
        let causes: Vec<&str> = cause.into_iter().collect();
        let obs = assert_check_fails(&fx, &db, &causes, &line);
        assert_eq!(obs.lines(LogCat::Always), ["Replaying blocks"]);
    }
}

/// Recovery from the interrupted reorg fails in `a[5]`'s disconnect with a line holding `cause`,
/// before anything is written.
#[track_caller]
fn assert_disconnect_fails(fx: &mut Fixture, a: &[BlockRef], b: &[BlockRef], cause: &str) {
    let failed = format!("ReplayBlocks(): DisconnectBlock failed at 5, hash={}", a[5].hash);
    let db = fx.interrupted(a[5], b[2]);
    let epoch = db.write_epoch();
    let obs = RecordingObserver::default();
    let err = fx.recover_err(&db, &obs);
    assert_unable(&err, &failed);
    let lines = obs.lines(LogCat::Error);
    assert!(lines.iter().any(|l| l.contains(cause)), "{lines:?}");
    assert_eq!(lines.last(), Some(&failed));
    assert_eq!(db.write_epoch(), epoch, "nothing is written before the first sub-batch");
    assert!(db.latch().tripped().is_none());
}

#[test]
fn undo_data_that_does_not_fit_stops_the_disconnect() {
    let (mut fx, a, b) = forked();
    fx.rewrite_undo(a[5], |u| {
        u.txs.pop();
    });
    assert_disconnect_fails(&mut fx, &a, &b, "DisconnectBlock(): block and undo data inconsistent");
    fx.rewrite_undo(a[5], |u| {
        u.txs[0].coins.pop();
    });
    let cause = "DisconnectBlock(): transaction and undo data inconsistent";
    assert_disconnect_fails(&mut fx, &a, &b, cause);
    fx.rewrite_undo(a[5], |u| {
        let c = &u.txs[1].coins[0];
        u.txs[1].coins[0] = Coin::new(c.value(), c.script_pubkey().as_slice(), 0, false);
    });
    assert_disconnect_fails(&mut fx, &a, &b, "has no height");
    // Still Core's bytes, but for another block: the checksum binds the record to its parent.
    let other = fx.index.get(a[4].id).unwrap().undo;
    fx.index.get_mut(a[5]).undo = other;
    assert_disconnect_fails(&mut fx, &a, &b, "Checksum mismatch");
    fx.index.get_mut(a[5]).status = Status(Status::HAVE_DATA);
    assert_disconnect_fails(&mut fx, &a, &b, "UndoReadFromDisk: no undo data available");
}

#[test]
fn a_block_that_is_not_the_indexed_one_cannot_be_replayed() {
    let (mut fx, a, b) = forked();
    let other = fx.index.get(b[0].id).unwrap().data;
    fx.index.get_mut(b[1]).data = other;
    let db = fx.interrupted(a[5], b[2]);
    let obs = RecordingObserver::default();
    let err = fx.recover_err(&db, &obs);
    let failed = format!("RollforwardBlock(): ReadBlockFromDisk failed at 5, hash={}", b[1].hash);
    assert_unable(&err, &failed);
    assert!(obs.lines(LogCat::Error)[0].contains("GetHash() doesn't match index"));
}

/// The index load's latch, which no test read trips.
struct NoReadFailure;

impl FailStop for NoReadFailure {
    fn read_failed(&self, what: &str) {
        panic!("the index load failed a read: {what}");
    }

    fn is_tripped(&self) -> bool {
        false
    }
}

/// The real block index (`HeaderChain::load`) of regtest's genesis and `records`.
fn real_index(params: &Arc<ChainSetup>, records: Vec<(BlockHash, Vec<u8>)>) -> HeaderChain {
    let genesis = dash_consensus::params::genesis_block(params).header;
    let mut all = vec![(Header::new(genesis).hash(), index_record(0, None, None, genesis))];
    all.extend(records);
    let svc = Services {
        clock: Arc::new(MockClock::at(1_700_000_000)),
        sporks: Arc::new(StaticSporks::for_network(params, None)),
        fail: Arc::new(NoReadFailure),
    };
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    let cfg = ChainConfig::node(params);
    HeaderChain::load(params.clone(), cfg, svc, all.into_iter().map(Ok), &pool).unwrap()
}

/// A block index record's bytes: `HAVE_DATA` at `data`, `HAVE_UNDO` at `undo`, and `header`, which
/// need not be the block file's.
fn index_record(
    height: u32,
    data: Option<FlatFilePos>,
    undo: Option<FlatFilePos>,
    header: BlockHeader,
) -> Vec<u8> {
    let mut status = Status::VALID_TREE;
    if data.is_some() {
        status |= Status::HAVE_DATA;
    }
    if undo.is_some() {
        status |= Status::HAVE_UNDO;
    }
    let record = BlockIndexRecord {
        height,
        status: Status(status),
        n_tx: 1,
        file: data.or(undo).map(|p| p.file as i32),
        data_pos: data.map(|p| p.pos),
        undo_pos: undo.map(|p| p.pos),
        header,
    };
    record.to_bytes()
}

/// A block on `prev` at `height`, its coinbase tagged `tag`, whose header claims `bits` and whose
/// hash meets regtest's limit.
fn child(params: &ChainSetup, prev: BlockHash, height: u32, tag: u8, bits: u32) -> Block {
    let genesis = dash_consensus::params::genesis_block(params).header;
    let header = BlockHeader {
        prev_hash: prev,
        time: genesis.time + height,
        bits,
        ..genesis
    };
    mine(header, vec![coinbase(height, tag)])
}

/// `block` at `height` in the fixture's block files.
fn save(fx: &mut Fixture, block: &Block, height: u32) -> FlatFilePos {
    fx.files.save_block(&core_bytes(block), height, 1_700_000_000, None).unwrap()
}

/// Recovery over `chain`, the real index, from an engine holding no coins and `meta`: the engine,
/// whether recovery wrote, its observer, and its tip or init error. The latch never trips.
fn recover_over(
    fx: &mut Fixture,
    chain: &HeaderChain,
    meta: &Meta,
) -> (Db, bool, RecordingObserver, Result<Option<BlockHash>, InitError>) {
    let db = fx.engine(&Coins::new(), meta);
    let epoch = db.write_epoch();
    let obs = RecordingObserver::default();
    let hub = SubscriberHub::new();
    let r = fx.open(&db, &obs).unwrap().recover(&chain.reader(), &hub, config(never()));
    let tip = r.map(|r| r.tip.map(|t| t.hash));
    assert!(db.latch().tripped().is_none());
    let wrote = db.write_epoch() != epoch;
    (db, wrote, obs, tip)
}

// R1-1 (Sol r1): Core's `ReadBlockFromDisk` checks the header it read against that header's own
// `nBits` (`node/blockstorage.cpp:825-829`), before it compares the hash with the index's
// (`:842-846`); the index load checked the stored hash against the index record's `nBits`
// (`txdb.cpp:328-331`), which the block file's need not equal. Here a block file claims
// `nBits = 0`, which no hash meets, and its index record regtest's limit, which the hash meets:
// the real index loads it, and recovery replays neither leg through it, though `bad` has its undo
// data so that both legs would replay without the check. Bytes that fail the check are reported
// by it alone, even read for another block; a block whose header meets its own `nBits` replays.
#[test]
fn a_block_whose_header_lacks_its_proof_of_work_cannot_be_replayed() {
    let params = Arc::new(regtest());
    let genesis = Header::new(dash_consensus::params::genesis_block(&params).header).hash();
    let mut fx = Fixture::new();
    let bad = child(&params, genesis, 1, 1, 0);
    let good = child(&params, genesis, 1, 2, REGTEST_BITS);
    let other = child(&params, genesis, 1, 3, REGTEST_BITS);
    assert!(!meets(&bad.hash(), bad.header().bits()));
    let at = save(&mut fx, &bad, 1);
    // A coinbase-only block's undo data holds no transactions.
    let undo = undo_bytes(&BlockUndo::default());
    let undo = fx.files.write_undo(&undo, at.file, 1, genesis.as_bytes()).unwrap();
    let good_at = save(&mut fx, &good, 1);
    // Each index record claims regtest's limit, whatever its block file's header claims.
    let claimed = |block: &Block| BlockHeader {
        bits: REGTEST_BITS,
        ..*block.header().raw()
    };
    let records = vec![
        (bad.hash(), index_record(1, Some(at), Some(undo), claimed(&bad))),
        (good.hash(), index_record(1, Some(good_at), None, claimed(&good))),
        // `other`'s record points at `bad`'s bytes.
        (other.hash(), index_record(1, Some(at), None, claimed(&other))),
    ];
    let chain = real_index(&params, records);
    let header_line = format!("ReadBlockFromDisk: Errors in block header at {at}");
    let forward =
        |b: &Block| format!("RollforwardBlock(): ReadBlockFromDisk failed at 1, hash={}", b.hash());
    let cases = [
        // The forward leg of a first flush to `bad` (Sol's reproduction).
        ((bad.hash(), None), forward(&bad)),
        // The undo leg of a flush from `bad` to `good`.
        (
            (good.hash(), Some(bad.hash())),
            format!("ReplayBlocks(): ReadBlockFromDisk() failed at 1, hash={}", bad.hash()),
        ),
        // `bad`'s bytes are not `other`, but the header check comes first.
        ((other.hash(), None), forward(&other)),
    ];
    for (head_blocks, failed) in cases {
        let meta = Meta {
            head_blocks: Some(head_blocks),
            evo_tip: Some(head_blocks.0),
            ..Meta::default()
        };
        let (_, wrote, obs, r) = recover_over(&mut fx, &chain, &meta);
        assert_unable(&r.expect_err("an init error"), &failed);
        assert_eq!(obs.lines(LogCat::Error), [header_line.clone(), failed]);
        assert!(!wrote);
    }
    // A first flush to `good` replays through the same index.
    let meta = Meta {
        head_blocks: Some((good.hash(), None)),
        evo_tip: Some(good.hash()),
        ..Meta::default()
    };
    let (db, _, _, r) = recover_over(&mut fx, &chain, &meta);
    assert_eq!(r.unwrap(), Some(good.hash()));
    assert_eq!(db_coins(&db).len(), 1);
}

// Core: node/blockstorage.cpp:560. The undo record's checksum is over the index's parent
// (`pindex->pprev`), not the one the block file's header names; nothing checks that an index
// record's header is the block's. Here `x`'s block file names `a1` and its record `a2`, and its
// undo data was written over `a1`: in the index `x` is on `a2`, and its undo data does not read
// (Opus review of R1-1, M1). Over the block file's parent, the undo leg would replay.
#[test]
fn the_undo_checksum_is_over_the_index_parent() {
    let params = Arc::new(regtest());
    let genesis = Header::new(dash_consensus::params::genesis_block(&params).header).hash();
    let mut fx = Fixture::new();
    let a1 = child(&params, genesis, 1, 1, REGTEST_BITS);
    let a2 = child(&params, genesis, 1, 2, REGTEST_BITS);
    let x = child(&params, a1.hash(), 2, 3, REGTEST_BITS);
    let mut records = Vec::new();
    for b in [&a1, &a2] {
        let at = save(&mut fx, b, 1);
        records.push((b.hash(), index_record(1, Some(at), None, *b.header().raw())));
    }
    let at = save(&mut fx, &x, 2);
    let undo = undo_bytes(&BlockUndo::default());
    let undo = fx.files.write_undo(&undo, at.file, 2, a1.hash().as_bytes()).unwrap();
    let header = BlockHeader {
        prev_hash: a2.hash(),
        ..*x.header().raw()
    };
    records.push((x.hash(), index_record(2, Some(at), Some(undo), header)));
    let chain = real_index(&params, records);
    // A flush from `x` to its index parent: the undo leg only.
    let meta = Meta {
        head_blocks: Some((a2.hash(), Some(x.hash()))),
        evo_tip: Some(a2.hash()),
        ..Meta::default()
    };
    let (_, wrote, obs, r) = recover_over(&mut fx, &chain, &meta);
    let failed = format!("ReplayBlocks(): DisconnectBlock failed at 2, hash={}", x.hash());
    assert_unable(&r.expect_err("an init error"), &failed);
    let read = "DisconnectBlock(): failure reading undo data";
    assert_eq!(obs.lines(LogCat::Error), ["UndoReadFromDisk: Checksum mismatch", read, &failed]);
    assert!(!wrote);
}

/// How [`a_failed_block_file_read_logs_core_lines`] damages a block or undo file.
enum Damage {
    /// Its reads fail so.
    Fails(Fault),
    /// It is cut to this length.
    Cut(u64),
    /// This size field replaces the one at this offset.
    Size(u64, u32),
}

// Core's lines for each way a block-file read fails: `FlatFileSeq::Open`'s for a file that does
// not open, and `AutoFile`'s exception texts (flatfile.cpp:45, streams.cpp:27).
#[test]
fn a_failed_block_file_read_logs_core_lines() {
    let (mut fx, a, b) = forked();
    let at = |p: IndexPos| FlatFilePos::new(p.file as u32, p.pos);
    let data = at(fx.index.get(a[5].id).unwrap().data);
    let undo = at(fx.index.get(a[5].id).unwrap().undo);
    let blk = fx.blocks.join("blk00000.dat");
    let rev = fx.blocks.join("rev00000.dat");
    let block_failed =
        format!("ReplayBlocks(): ReadBlockFromDisk() failed at 5, hash={}", a[5].hash);
    let undo_failed = format!("ReplayBlocks(): DisconnectBlock failed at 5, hash={}", a[5].hash);
    let read = "DisconnectBlock(): failure reading undo data";
    let eof = "Deserialize or I/O error - AutoFile::read: end of file: iostream error";
    let fread = "Deserialize or I/O error - AutoFile::read: fread failed: iostream error";
    // Checks the error lines, after "Unable to open file" exactly for Core's open failures.
    // Whether the latch tripped.
    let check = |fx: &mut Fixture, file: &Path, damage: Damage, lines: &[&str]| {
        let db = fx.interrupted(a[5], b[2]);
        let saved = std::fs::read(file).unwrap();
        let obs = RecordingObserver::default();
        let chainstate = fx.open(&db, &obs).unwrap();
        match damage {
            Damage::Fails(fault) => fx.fs.fail_reads_of(file, fault),
            Damage::Cut(len) => {
                std::fs::File::options().write(true).open(file).unwrap().set_len(len).unwrap()
            }
            Damage::Size(offset, size) => {
                fx.fs.write_all_at(file, offset, &size.to_le_bytes()).unwrap()
            }
        }
        let r = chainstate.recover(&fx.index, &SubscriberHub::new(), config(never()));
        fx.fs.fail_reads(None);
        std::fs::write(file, saved).unwrap();
        assert!(matches!(r, Err(InitError::UnableToReplay { .. })));
        let opened = format!("Unable to open file {}", file.display());
        let open_failed = lines[0].contains("OpenBlockFile") || lines[0].contains("OpenUndoFile");
        let opened = open_failed.then_some(opened.as_str());
        assert_eq!(
            open_and_error_lines(&obs),
            opened.iter().chain(lines).copied().collect::<Vec<_>>()
        );
        db.latch().tripped().is_some()
    };
    let line = format!("ReadBlockFromDisk: OpenBlockFile failed for {data}");
    let missing = Damage::Fails(Fault::Open(io::ErrorKind::NotFound));
    assert!(!check(&mut fx, &blk, missing, &[&line, &block_failed]));
    // A file that exists but does not open is unavailable, not absent (DEC-162 (4)).
    let unreadable = Damage::Fails(Fault::Open(io::ErrorKind::PermissionDenied));
    assert!(check(&mut fx, &blk, unreadable, &[&line, &block_failed]));
    // The same error from the open and from a read of the open file: the stage decides the lines.
    assert!(check(
        &mut fx,
        &blk,
        Damage::Fails(Fault::Open(io::ErrorKind::Other)),
        &[&line, &block_failed]
    ));
    let line = format!("ReadBlockFromDisk: {fread} at {data}");
    assert!(check(
        &mut fx,
        &blk,
        Damage::Fails(Fault::Read(io::ErrorKind::Other)),
        &[&line, &block_failed]
    ));
    // Bytes short of the size field, or of the record.
    let line = format!("ReadBlockFromDisk: {eof} at {data}");
    let short = Damage::Fails(Fault::Read(io::ErrorKind::UnexpectedEof));
    assert!(!check(&mut fx, &blk, short, &[&line, &block_failed]));
    let cut = Damage::Cut(u64::from(data.pos) + 10);
    assert!(!check(&mut fx, &blk, cut, &[&line, &block_failed]));
    // A size field Core never reads: the store's reason, once with the position.
    let size = Damage::Size(u64::from(data.pos) - 4, u32::MAX);
    let line = format!(
        "ReadBlockFromDisk: Deserialize or I/O error - record size {} exceeds the \
         {MAX_BLOCK_RECORD_SIZE}-byte limit at {data}",
        u32::MAX
    );
    assert!(!check(&mut fx, &blk, size, &[&line, &block_failed]));
    let missing = Damage::Fails(Fault::Open(io::ErrorKind::NotFound));
    let lines = ["UndoReadFromDisk: OpenUndoFile failed", read, &undo_failed];
    assert!(!check(&mut fx, &rev, missing, &lines));
    let line = format!("UndoReadFromDisk: {eof}");
    let cut = Damage::Cut(u64::from(undo.pos) + 10);
    assert!(!check(&mut fx, &rev, cut, &[&line, read, &undo_failed]));
}

// Sol r2 R2-1: a real open failure, not an injected one. With the process's descriptors used up,
// the open fails with EMFILE, which no list of the errors only an open returns had, and Core's
// open lines follow from the stage. The limit is the whole process's, so the test runs alone in
// a child process.
#[test]
fn a_file_that_does_not_open_for_want_of_descriptors_logs_cores_open_lines() {
    if !alone_in_child("a_file_that_does_not_open_for_want_of_descriptors_logs_cores_open_lines") {
        return;
    }
    let (mut fx, a, b) = forked();
    let data = fx.index.get(a[5].id).unwrap().data;
    let data = FlatFilePos::new(data.file as u32, data.pos);
    let blk = fx.blocks.join("blk00000.dat");
    let rev = fx.blocks.join("rev00000.dat");
    let block_failed =
        format!("ReplayBlocks(): ReadBlockFromDisk() failed at 5, hash={}", a[5].hash);
    let undo_failed = format!("ReplayBlocks(): DisconnectBlock failed at 5, hash={}", a[5].hash);
    let read = "DisconnectBlock(): failure reading undo data";
    // A file system with no handle open, as at a start; `opened` is open already.
    let run = |fx: &mut Fixture, opened: &[&Path], unopened: &Path, lines: &[&str]| {
        let db = fx.interrupted(a[5], b[2]);
        let obs = RecordingObserver::default();
        let fs = Arc::new(FaultFs::default());
        for file in opened {
            fs.read_exact_at(file, 0, &mut [0]).unwrap();
        }
        let cfg = BlockFilesConfig::new(&fx.blocks, MAGIC);
        let chainstate = ChainstateOpen::open(&db, fs, cfg, &obs).unwrap();
        let r = without_descriptors(|| {
            chainstate.recover(&fx.index, &SubscriberHub::new(), config(never())).err()
        });
        assert!(matches!(r, Some(InitError::UnableToReplay { .. })), "{r:?}");
        let opens = format!("Unable to open file {}", unopened.display());
        assert_eq!(
            open_and_error_lines(&obs),
            [&opens[..]].iter().chain(lines).copied().collect::<Vec<_>>()
        );
        // Unavailable, not absent (DEC-162 (4)).
        assert_eq!(db.latch().tripped().map(|f| f.site), Some("block files read"));
    };
    let line = format!("ReadBlockFromDisk: OpenBlockFile failed for {data}");
    run(&mut fx, &[], &blk, &[&line, &block_failed]);
    run(
        &mut fx,
        &[blk.as_path()],
        &rev,
        &["UndoReadFromDisk: OpenUndoFile failed", read, &undo_failed],
    );
}

/// Whether to run the test `name` here: in the child process of this test binary that runs it
/// alone, which this call starts and checks if this is not that child.
fn alone_in_child(name: &str) -> bool {
    const CHILD: &str = "DASHD_CHAINSTATE_ALONE";
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    let exe = std::env::current_exe().unwrap();
    let module = module_path!().split_once("::").expect("in the crate").1;
    let test = format!("{module}::{name}");
    let out = std::process::Command::new(exe)
        .args(["--exact", &test, "--test-threads=1"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && stdout.contains(" 1 passed"), "{stdout}{stderr}");
    false
}

/// The "Unable to open file" lines and the error lines, in order.
fn open_and_error_lines(obs: &RecordingObserver) -> Vec<String> {
    obs.marks()
        .into_iter()
        .filter_map(|m| match m {
            ObsMark::Log {
                cat: LogCat::Error,
                line,
            } => Some(line),
            ObsMark::Log {
                cat: LogCat::Always,
                line,
            } if line.starts_with("Unable to open file") => Some(line),
            _ => None,
        })
        .collect()
}

/// Runs `f` with every file descriptor the process may open in use, so that an open fails with
/// EMFILE. Only for a process that runs one test (see [`alone_in_child`]).
#[allow(unsafe_code)]
fn without_descriptors<T>(f: impl FnOnce() -> T) -> T {
    let mut was = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `getrlimit` and `setrlimit` read and write only the `rlimit` passed.
    assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut was) }, 0);
    let low = libc::rlimit {
        rlim_cur: was.rlim_cur.min(64),
        ..was
    };
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &low) }, 0);
    let mut held = Vec::new();
    let full = loop {
        match std::fs::File::open("/dev/null") {
            Ok(file) => held.push(file),
            Err(e) => break e,
        }
    };
    assert_eq!(full.raw_os_error(), Some(libc::EMFILE), "{full}");
    let r = f();
    drop(held);
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &was) }, 0);
    r
}

// DEC-148, DEC-162 (4): unreadable bytes stop the node; missing ones cannot be replayed.
#[test]
fn a_block_file_read_error_trips_the_latch_and_a_missing_file_does_not() {
    let (mut fx, a, b) = forked();
    let db = fx.interrupted(a[5], b[2]);
    let obs = RecordingObserver::default();
    let err = fx.recover_with_read_fault(&db, &obs, Fault::Read(io::ErrorKind::Other));
    assert!(matches!(err, InitError::UnableToReplay { .. }), "{err:?}");
    let tripped = db.latch().tripped().expect("tripped");
    assert_eq!((tripped.kind, tripped.site), (ErrorKind::Io, "block files read"));
    assert_abort_tail(&obs, "read", UNABLE_TO_REPLAY);
    // Bytes short of the record, or no file: the blocks cannot be replayed, and nothing is
    // unavailable.
    for fault in [Fault::Read(io::ErrorKind::UnexpectedEof), Fault::Open(io::ErrorKind::NotFound)] {
        let db = fx.interrupted(a[5], b[2]);
        let obs = RecordingObserver::default();
        let err = fx.recover_with_read_fault(&db, &obs, fault);
        assert!(matches!(err, InitError::UnableToReplay { .. }), "{err:?}");
        assert!(db.latch().tripped().is_none());
        assert!(!obs.marks().iter().any(|m| matches!(m, ObsMark::StoreFailed { .. })));
    }
}

#[test]
fn a_failed_write_is_reported_once_and_a_refused_one_not_at_all() {
    let (mut fx, a, b) = forked();
    let db = fx.interrupted(a[5], b[2]);
    let obs = RecordingObserver::default();
    let opened = fx.open(&db, &obs).unwrap();
    db.inner().crash_at_write(1);
    let err = opened.recover(&fx.index, &SubscriberHub::new(), config(never())).err().unwrap();
    assert_store_err(&err, ErrorKind::Io);
    assert_eq!(err.to_string(), OPEN_FAILED);
    assert_abort_tail(&obs, "write", OPEN_FAILED);
    let failures = |m: &&ObsMark| matches!(m, ObsMark::StoreFailed { .. } | ObsMark::Abort { .. });
    assert_eq!(obs.marks().iter().filter(failures).count(), 2, "reported once");
    assert_eq!(obs.lines(LogCat::Error).len(), 1);
    // Another store's failure latched first: the write is refused, and its caller reports it.
    let db = fx.interrupted(a[5], b[2]);
    let obs = RecordingObserver::default();
    let opened = fx.open(&db, &obs).unwrap();
    db.latch().trip(Failure {
        kind: ErrorKind::Io,
        site: "block files write",
        message: "elsewhere".into(),
    });
    let err = opened.recover(&fx.index, &SubscriberHub::new(), config(never())).err().unwrap();
    assert_store_err(&err, ErrorKind::ReadOnly);
    assert_eq!(obs.marks().iter().filter(failures).count(), 0);
}

// Core's `LoadChainstate` reads its databases directly, so a failed read ends the load with
// "Error opening block database", never the coins' read abort (review r1 M1).
#[test]
fn open_reports_a_failed_barrier_or_read_with_the_init_error() {
    let mut fx = Fixture::new();
    let db = fx.engine(&Coins::new(), &Meta::default());
    db.inner().fail_barrier_at(1);
    let obs = RecordingObserver::default();
    let err = fx.open(&db, &obs).err().expect("an init error");
    assert_store_err(&err, ErrorKind::Io);
    assert_abort_tail(&obs, "sync_wal", OPEN_FAILED);
    let sink = Arc::new(RecordingSink::default());
    let db = GuardedEngine::probe(sink.clone());
    db.inner().fail_reads(Some(ErrorKind::Corruption));
    let obs = RecordingObserver::default();
    let err = fx.open(&db, &obs).err().expect("an init error");
    assert_store_err(&err, ErrorKind::Corruption);
    assert_eq!(err.to_string(), OPEN_FAILED);
    assert!(sink.marks().is_empty());
    assert_abort_tail(&obs, "read", OPEN_FAILED);
    assert_eq!(db.latch().tripped().map(|f| f.site), Some("chainstate read"));
}

// Core: validation.cpp:5230. A marker of no hashes is Core's empty vector, a consistent state. One
// that is no whole number of hashes fails the open: quirk 44
// (`tests/quirk_44_head_blocks_decode.rs`).
#[test]
fn an_empty_marker_is_consistent() {
    let (mut fx, a, _) = forked();
    let db = fx.engine_at(a[5], &at(a[5]));
    let mut w = db.batch();
    w.put(DbSchema::META, b"head_blocks", &[]);
    db.write(w, Commit::Durable).unwrap();
    let epoch = db.write_epoch();
    let obs = RecordingObserver::default();
    let hub = SubscriberHub::new();
    let r = fx.open(&db, &obs).unwrap().recover(&fx.index, &hub, config(never())).unwrap();
    assert_eq!((r.tip, r.replayed.len()), (Some(a[5]), 0));
    assert_eq!(db.write_epoch(), epoch);
    assert_eq!(obs.marks(), [ObsMark::RecoveryBegin, ObsMark::RecoveryEnd]);
}

#[test]
fn the_init_region_ends_with_recovery() {
    let (mut fx, a, _) = forked();
    let sink = Arc::new(RecordingSink::default());
    let coins = fx.coins(a[5]).clone();
    let db = fx.seed(GuardedEngine::probe(sink.clone()), &coins, &Meta::default());
    let obs = RecordingObserver::default();
    let opened = fx.open(&db, &obs).unwrap();
    // No tip: step 5 scans the coins, and the scan fails as the load's.
    db.inner().fail_reads(Some(ErrorKind::Io));
    let err = opened.recover(&fx.index, &SubscriberHub::new(), config(never())).err().unwrap();
    assert_store_err(&err, ErrorKind::Io);
    assert!(sink.marks().is_empty());
    assert_abort_tail(&obs, "read", OPEN_FAILED);
    // Recovery is over: a failed read is the coins' abort again.
    assert!(Meta::read(&db).is_err());
    assert!(matches!(sink.marks()[..], [AbortMark::ReadFailed(_)]));
}

// BL-72: an interrupted wipe resumes before any marker is read.
#[test]
fn an_interrupted_wipe_resumes_and_leaves_an_empty_chainstate() {
    let (mut fx, a, b) = forked();
    let meta = Meta {
        wiping: true,
        ..marker(b[2], Some(a[5]))
    };
    let db = fx.engine_at(a[5], &meta);
    let obs = RecordingObserver::default();
    let opened = fx.open(&db, &obs).unwrap();
    assert!(opened.wiped());
    assert_eq!(
        *opened.meta(),
        Meta {
            last_file: opened.meta().last_file,
            ..Meta::default()
        }
    );
    assert_eq!(Meta::read(&db).unwrap(), *opened.meta(), "as read back");
    assert!(db_coins(&db).is_empty());
    let r = opened.recover(&fx.index, &SubscriberHub::new(), config(never())).unwrap();
    assert_eq!((r.tip, r.replayed.len()), (None, 0));
    assert_eq!(obs.marks().last(), Some(&ObsMark::RecoveryEnd));
}

// ST2: the block files come back from their durable infos and `last_file`, read as Core's
// `LoadBlockIndexDB` reads them (review r1 S2).
#[test]
fn open_restores_the_block_files_from_their_durable_infos() {
    let (mut fx, a, _) = forked();
    let state = fx.files.sync_and_state().unwrap();
    assert_eq!(state.infos.len(), state.last_file as usize + 1);
    let db = fx.engine_at(a[5], &at(a[5]));
    let opened = |db: &Db| {
        let obs = RecordingObserver::default();
        let opened = fx.open(db, &obs).unwrap();
        opened.files().read(|f| (f.last_file(), f.infos().to_vec()))
    };
    assert_eq!(opened(&db), (state.last_file, state.infos.clone()));
    let cf = CHAINSTATE_SCHEMA.cf("block_files").unwrap();
    let info = state.infos[0].to_value_bytes();
    let put = |key: &[u8], value: &[u8]| {
        let mut w = db.batch();
        w.put(cf, key, value);
        db.write(w, Commit::Durable).unwrap();
    };
    // Records Core's loop never reaches are not read: no file number, and a file past a gap.
    put(&[0xff, 0xff, 0xff, 0xff, 0], &[1]);
    put(&(state.last_file + 2).to_be_bytes(), &info);
    assert_eq!(opened(&db), (state.last_file, state.infos.clone()));
    // The files after `last_file` count while their records are contiguous.
    put(&(state.last_file + 1).to_be_bytes(), &info);
    let (last_file, infos) = opened(&db);
    assert_eq!((last_file, infos.len()), (state.last_file, state.infos.len() + 2));
    assert_eq!(infos[infos.len() - 1], state.infos[0]);
    // A record that does not decode: quirk 43 (`tests/quirk_43_block_file_info_decode.rs`).
}

#[test]
fn recovered_coins_decode_as_the_cache_reads_them() {
    let (mut fx, a, b) = forked();
    let db = fx.interrupted(a[5], b[2]);
    fx.recover(&db, &RecordingObserver::default(), never()).unwrap();
    let table = CoinsTable::default();
    for (k, bytes) in fx.coins(b[2]) {
        let coin = table.get(&db, k).unwrap().expect("present");
        assert_eq!(coin.as_bytes(), &bytes[..]);
        assert_eq!(PackedCoin::from_db(bytes).unwrap().as_bytes(), coin.as_bytes());
    }
}

// Review r1 S3: an empty RocksDB batch already holds its header, so a sub-batch below that size
// still closes only once it holds an op, and recovery makes progress.
#[cfg(feature = "rocksdb")]
#[test]
fn a_sub_batch_below_an_empty_batchs_size_still_makes_progress() {
    let (mut fx, a, b) = forked();
    let dir = tempfile::tempdir().unwrap();
    let (engine, _) =
        dashd_store::RocksEngine::open(dir.path(), &CHAINSTATE_SCHEMA, &Default::default())
            .unwrap();
    let db = GuardedEngine::new(
        ProbeEngine::over(engine),
        GlobalFailStop::new(),
        Arc::new(RecordingSink::default()),
    );
    let coins = fx.coins(a[5]).clone();
    let db = fx.seed(db, &coins, &marker(b[2], Some(a[5])));
    assert!(db.batch().size_bytes() > 0);
    let obs = RecordingObserver::default();
    let hub = SubscriberHub::new();
    let cfg = RecoveryConfig {
        sub_batch: 0,
        ..config(never())
    };
    let mut r = fx.open(&db, &obs).unwrap().recovery(&fx.index, &hub, cfg);
    let steps = drain(&mut r);
    assert_eq!(steps.last(), Some(&RecoveryStep::Done));
    assert!(sub_batches(&steps).len() > 20);
    drop(r);
    assert_coins(&db, fx.coins(b[2]), "");
}

/// One row in the `block_index` column family, and its key.
fn put_index_row(db: &impl KvEngine, key: [u8; 32]) {
    let mut batch = db.batch();
    batch.put(CHAINSTATE_SCHEMA.cf("block_index").unwrap(), &key, b"a record");
    db.write(batch, Commit::Durable).unwrap();
}

// ND1 2b-D20: the index load reads `block_index`, and only it, outside the latch, so its failed
// read is the loader's init error, not the engine's fail-stop. The control, the same failure
// through the guard, trips the latch.
#[test]
fn the_block_index_scan_fails_outside_the_latch() {
    use dashd_store::KvRead as _;
    let fx = Fixture::new();
    let db = Db::probe(Arc::new(RecordingSink::default()));
    put_index_row(db.inner(), [1; 32]);
    let mut coin = db.inner().batch();
    coin.put(CHAINSTATE_SCHEMA.cf("coins").unwrap(), &[1; 32], b"a coin");
    db.inner().write(coin, Commit::Durable).unwrap();
    let obs = RecordingObserver::default();
    let opened = fx.open(&db, &obs).unwrap();
    let rows: Vec<_> = opened.block_index().map(Result::unwrap).collect();
    assert_eq!(rows, [(vec![1; 32], b"a record".to_vec())], "the index's rows only");
    db.inner().fail_reads(Some(ErrorKind::Io));
    let read: Vec<_> = opened.block_index().collect();
    assert!(matches!(&read[..], [Err(e)] if e.kind() == Some(ErrorKind::Io)), "{read:?}");
    assert!(db.latch().tripped().is_none(), "the index scan tripped the latch");
    let index = CHAINSTATE_SCHEMA.cf("block_index").unwrap();
    assert!(db.iter_prefix_uncached(index, &[]).next().unwrap().is_err());
    assert!(db.latch().tripped().is_some(), "the guarded scan, the control");
}

// ND1 2b-D19: the index load's scan leaves the block cache as it was, as Core's iterators with
// `fill_cache = false` do (`dbwrapper.cpp:136`); the cached scan, the control, fills it.
#[cfg(feature = "rocksdb")]
#[test]
fn the_block_index_scan_leaves_the_block_cache() {
    const ROWS: u32 = 20_000;
    let fx = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let open = || {
        let config = Default::default();
        dashd_store::RocksEngine::open(dir.path(), &CHAINSTATE_SCHEMA, &config).unwrap().0
    };
    {
        let engine = open();
        let mut batch = engine.batch();
        for i in 0..ROWS {
            let mut key = [0; 32];
            key[..4].copy_from_slice(&i.to_be_bytes());
            batch.put(CHAINSTATE_SCHEMA.cf("block_index").unwrap(), &key, &[7; 92]);
        }
        engine.write(batch, Commit::Durable).unwrap();
        engine.flush_memtables().unwrap();
    }
    let db = GuardedEngine::new(
        ProbeEngine::over(open()),
        GlobalFailStop::new(),
        Arc::new(RecordingSink::default()),
    );
    let obs = RecordingObserver::default();
    let opened = fx.open(&db, &obs).unwrap();
    let usage = || db.inner().unprobed().memory_usage().unwrap().block_cache_usage;
    let before = usage();
    assert_eq!(opened.block_index().map(Result::unwrap).count(), ROWS as usize);
    assert_eq!(usage(), before, "the index scan filled the cache");
    let index = CHAINSTATE_SCHEMA.cf("block_index").unwrap();
    assert_eq!(db.inner().unprobed().iter_prefix(index, &[]).count(), ROWS as usize);
    assert!(usage() > before + (1 << 20), "the cached scan left {} bytes", usage());
}
