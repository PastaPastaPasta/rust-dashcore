//! Compact blocks, client side (P3 §2.3): Core's `PartiallyDownloadedBlock`, the libstdc++ bucket
//! test its short-ID map makes (D7, D22), the mempool read trait and the extra-transactions ring.
//!
//! Core's `InitData` keys a `std::unordered_map<uint64_t, uint16_t>` by short ID and fails the
//! block once one bucket holds more than 12 entries, so its answer depends on libstdc++'s bucket
//! count. [`bucket_count`] reproduces it from the tables of the Linux release `dashd`
//! (v24.0.0-rc.3, GCC 13.3.0, libstdc++ linked statically). `contrib/node/p3-buckets` extracts
//! them and checks the rule against a real `unordered_map` built with that toolchain.
//!
//! `be:N` cites line N of Core's `blockencodings.cpp`, `tm:N` of `txmempool.cpp` and `np:N` of
//! `net_processing.cpp`, as in the P3 design.

use std::collections::HashMap;
use std::sync::Arc;

use dash_consensus::block::{Mutation, block_mutation};
use dash_consensus::primitives::{compact_size_len, header_bytes, write_compact_size};
use dash_consensus::{Block, CoreSerialize, CoreWrite, Header, Tx, TxRef};
use dash_p2p::rng::{RandSource, get_rand};
use dash_p2p::{CommandString, OutMsg};
use dash_p2p_core::{CmpctBlock, short_id_with_keys};
use dash_primitives::TxHash;

use super::peerman::RecentBlock;
use super::seams::{EntryFacts, SyncCx};

/// Core's `ReadStatus` (blockencodings.h:83-87).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadStatus {
    /// `READ_STATUS_OK`.
    Ok,
    /// `READ_STATUS_INVALID`: the peer sent a malformed object.
    Invalid,
    /// `READ_STATUS_FAILED`: the object could not be used (bucket overflow, short-ID collision,
    /// a reconstruction whose merkle root does not match).
    Failed,
}

/// `MaxBlockSize()` with DIP0001 active (consensus.h:12-15), as `InitData` calls it.
const MAX_BLOCK_SIZE: usize = 2_000_000;

/// `MIN_TRANSACTION_SIZE`: the serialized size of an empty `CMutableTransaction` (be:19): the
/// packed 32-bit version and type, two empty vectors and the lock time.
const MIN_TRANSACTION_SIZE: usize = 10;

/// The most elements one bucket of `InitData`'s short-ID map may hold (be:104).
const MAX_BUCKET_SIZE: u32 = 12;

/// libstdc++'s `__prime_list` (`hashtable-aux.cc`): 304 primes and the repeated last one that
/// `_M_next_bkt` reads as `__last_prime`. Read from the release `dashd` (sha256 `e204e316…5bb4`)
/// at file offset 21,634,080 by `contrib/node/p3-buckets/extract.py` (P3 D22, D90).
#[rustfmt::skip]
const PRIME_LIST: [u64; 305] = [
    2, 3, 5, 7, 11, 13,
    17, 19, 23, 29, 31, 37,
    41, 43, 47, 53, 59, 61,
    67, 71, 73, 79, 83, 89,
    97, 103, 109, 113, 127, 137,
    139, 149, 157, 167, 179, 193,
    199, 211, 227, 241, 257, 277,
    293, 313, 337, 359, 383, 409,
    439, 467, 503, 541, 577, 619,
    661, 709, 761, 823, 887, 953,
    1031, 1109, 1193, 1289, 1381, 1493,
    1613, 1741, 1879, 2029, 2179, 2357,
    2549, 2753, 2971, 3209, 3469, 3739,
    4027, 4349, 4703, 5087, 5503, 5953,
    6427, 6949, 7517, 8123, 8783, 9497,
    10273, 11113, 12011, 12983, 14033, 15173,
    16411, 17749, 19183, 20753, 22447, 24281,
    26267, 28411, 30727, 33223, 35933, 38873,
    42043, 45481, 49201, 53201, 57557, 62233,
    67307, 72817, 78779, 85229, 92203, 99733,
    107897, 116731, 126271, 136607, 147793, 159871,
    172933, 187091, 202409, 218971, 236897, 256279,
    277261, 299951, 324503, 351061, 379787, 410857,
    444487, 480881, 520241, 562841, 608903, 658753,
    712697, 771049, 834181, 902483, 976369, 1056323,
    1142821, 1236397, 1337629, 1447153, 1565659, 1693859,
    1832561, 1982627, 2144977, 2320627, 2510653, 2716249,
    2938679, 3179303, 3439651, 3721303, 4026031, 4355707,
    4712381, 5098259, 5515729, 5967347, 6456007, 6984629,
    7556579, 8175383, 8844859, 9569143, 10352717, 11200489,
    12117689, 13109983, 14183539, 15345007, 16601593, 17961079,
    19431899, 21023161, 22744717, 24607243, 26622317, 28802401,
    31160981, 33712729, 36473443, 39460231, 42691603, 46187573,
    49969847, 54061849, 58488943, 63278561, 68460391, 74066549,
    80131819, 86693767, 93793069, 101473717, 109783337, 118773397,
    128499677, 139022417, 150406843, 162723577, 176048909, 190465427,
    206062531, 222936881, 241193053, 260944219, 282312799, 305431229,
    330442829, 357502601, 386778277, 418451333, 452718089, 489790921,
    529899637, 573292817, 620239453, 671030513, 725980837, 785430967,
    849749479, 919334987, 994618837, 1076067617, 1164186217, 1259520799,
    1362662261, 1474249943, 1594975441, 1725587117, 1866894511, 2019773507,
    2185171673, 2364114217, 2557710269, 2767159799, 2993761039, 3238918481,
    3504151727, 3791104843, 4101556399, 4294967291, 6442450933, 8589934583,
    12884901857, 17179869143, 25769803693, 34359738337, 51539607367, 68719476731,
    103079215087, 137438953447, 206158430123, 274877906899, 412316860387, 549755813881,
    824633720731, 1099511627689, 1649267441579, 2199023255531, 3298534883309, 4398046511093,
    6597069766607, 8796093022151, 13194139533241, 17592186044399, 26388279066581, 35184372088777,
    52776558133177, 70368744177643, 105553116266399, 140737488355213, 211106232532861, 281474976710597,
    562949953421231, 1125899906842597, 2251799813685119, 4503599627370449, 9007199254740881, 18014398509481951,
    36028797018963913, 72057594037927931, 144115188075855859, 288230376151711717, 576460752303423433, 1152921504606846883,
    2305843009213693951, 4611686018427387847, 9223372036854775783, 18446744073709551557, 18446744073709551557,
];

/// `_M_next_bkt`'s small-hint table, read from the same binary 16 bytes before `__prime_list`.
const FAST_BKT: [u8; 14] = [2, 2, 2, 3, 5, 5, 7, 7, 11, 11, 11, 11, 13, 13];

/// The bucket count of `std::unordered_map<uint64_t, uint16_t>(n)` in the release `dashd`:
/// GCC 13.3.0's `_Prime_rehash_policy::_M_next_bkt(n)` (`hashtable_c++0x.cc`). A hint of 0 keeps
/// the single initial bucket; a hint below 14 reads the small table; a larger one takes the first
/// prime not below it, searching from `__prime_list[6]` and stopping at `__last_prime`. With the
/// maximum load factor at 1.0, `n` inserts never rehash it.
pub fn bucket_count(n: usize) -> u64 {
    if n == 0 {
        return 1;
    }
    if let Some(&b) = FAST_BKT.get(n) {
        return u64::from(b);
    }
    let last = PRIME_LIST.len() - 2;
    let n = n as u64;
    PRIME_LIST[6 + PRIME_LIST[6..last].partition_point(|&p| p < n)]
}

/// `InitData`'s `shorttxids` map (be:88): the positions by short ID, and each libstdc++ bucket's
/// element count, so the `bucket_size > 12` test fires at Core's insertion (P3 §2.3.2).
#[derive(Debug)]
struct ShortIdIndex {
    buckets: u64,
    counts: Vec<u32>,
    positions: HashMap<u64, u16>,
}

impl ShortIdIndex {
    fn new(n: usize) -> Self {
        let buckets = bucket_count(n);
        Self {
            buckets,
            counts: vec![0; buckets as usize],
            positions: HashMap::with_capacity(n),
        }
    }

    /// `shorttxids[id] = pos`, then `bucket_size(bucket(id)) <= 12`. Re-assigning a present key
    /// adds no element, so its bucket keeps its count.
    fn insert(&mut self, id: u64, pos: u16) -> bool {
        // `std::hash<uint64_t>` is the identity and `_Mod_range_hashing` takes the remainder.
        let bucket = (id % self.buckets) as usize;
        if self.positions.insert(id, pos).is_none() {
            self.counts[bucket] += 1;
        }
        self.counts[bucket] <= MAX_BUCKET_SIZE
    }

    fn get(&self, id: u64) -> Option<usize> {
        self.positions.get(&id).map(|&p| usize::from(p))
    }

    fn len(&self) -> usize {
        self.positions.len()
    }
}

/// Core's `pool->vTxHashes` walk in `InitData` (be:112-139), under the pool's read lock.
pub trait CompactTxSource: Send + Sync {
    /// Calls `f(instance_hash, tx)` in `vTxHashes` order until it returns `false`. The order is
    /// insertion order with swap-remove (tm:475-476, tm:726-733); an asset unlock v2 whose
    /// instance hash changes is rekeyed in place (tm:871). The order decides which of two
    /// short-ID-equal transactions wins, so MP1a must keep it.
    fn for_each_in_vtxhashes_order(&self, f: &mut dyn FnMut(&TxHash, &TxRef) -> bool);
}

/// An empty mempool: every transaction that is not prefilled is requested (P3 §3.5.5).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoMempool;

impl CompactTxSource for NoMempool {
    fn for_each_in_vtxhashes_order(&self, _f: &mut dyn FnMut(&TxHash, &TxRef) -> bool) {}
}

/// `-blockreconstructionextratxn`'s default (net_processing.h:43).
pub const DEFAULT_BLOCK_RECONSTRUCTION_EXTRA_TXN: i64 = 100;

/// `vExtraTxnForCompact` and `vExtraTxnForCompactIt` (np:1129-1131): the transactions
/// `InitData` searches after the mempool, keyed by instance hash. Built per D12.
pub struct ExtraTxns {
    slots: Vec<(TxHash, TxRef)>,
    max: usize,
    next: usize,
}

impl std::fmt::Debug for ExtraTxns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtraTxns")
            .field("len", &self.slots.len())
            .field("max", &self.max)
            .field("next", &self.next)
            .finish()
    }
}

impl Default for ExtraTxns {
    fn default() -> Self {
        Self::new(DEFAULT_BLOCK_RECONSTRUCTION_EXTRA_TXN)
    }
}

impl ExtraTxns {
    /// A ring of `-blockreconstructionextratxn` entries; none when it is not positive. Core reads
    /// the option into a `size_t`, so a negative value there is huge and its first add throws:
    /// treating it as 0 is a deliberate configuration-only deviation (D91).
    pub fn new(max_extra_txn: i64) -> Self {
        Self {
            slots: Vec::new(),
            max: usize::try_from(max_extra_txn).unwrap_or(0),
            next: 0,
        }
    }

    /// `AddToCompactExtraTransactions`: nothing when the ring has no room at all; keyed by
    /// `GetInstanceHash()`; once the ring is full, each add replaces the oldest entry.
    // Core: net_processing.cpp:1922-1931
    pub fn add(&mut self, tx: TxRef) {
        if self.max == 0 {
            return;
        }
        let entry = (tx.instance_hash(), tx);
        if self.slots.len() < self.max {
            self.slots.push(entry);
        } else {
            self.slots[self.next] = entry;
        }
        self.next = (self.next + 1) % self.max;
    }

    /// The entries in slot order, as `InitData` walks `extra_txn` (be:141-170).
    fn iter(&self) -> impl Iterator<Item = &(TxHash, TxRef)> {
        self.slots.iter()
    }

    /// How many entries the ring holds.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the ring holds none.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

/// One prefilled transaction: its differential index (`PrefilledTransaction::index`) and the
/// transaction.
#[derive(Clone)]
pub struct Prefilled {
    /// The index less the previous prefilled index and one; the first one's index itself.
    pub index: u16,
    /// The transaction.
    pub tx: TxRef,
}

/// Core's `GetRand<uint64_t>()` for a `CBlockHeaderAndShortTxIDs` nonce (be:22): a fresh
/// `FastRandomContext`'s `randrange(UINT64_MAX)`, one `(false, 32)` call in the D60 draw log
/// (P3 §3.6, Q12).
pub fn draw_nonce(src: &Arc<dyn RandSource>) -> u64 {
    get_rand(src, u64::MAX)
}

/// Section 7's `cmpctblock` push for `block` (np:6478-6494): `recent`'s encoding when it names
/// `block`, else the block read back and encoded with a fresh nonce. Core's failed read is an
/// `assert(ret)` (np:6492), and so is this one (P3 §3.6, A-3).
pub fn push_compact_announcement(cx: &SyncCx<'_>, block: &EntryFacts, recent: &RecentBlock) {
    let payload = match (recent.hash, &recent.cmpct) {
        (Some(hash), Some(cmpct)) if hash == block.hash => cmpct.to_vec(),
        _ => {
            let b = cx.chain.read_block(block.id).block().expect("ReadBlockFromDisk (np:6492)");
            CompactBlock::from_block(&b, draw_nonce(cx.core.rand())).wire_bytes()
        }
    };
    cx.core.push(cx.peer, OutMsg::new(CommandString::CMPCTBLOCK, payload));
}

/// A decoded `cmpctblock`, Core's `CBlockHeaderAndShortTxIDs` with its SipHash keys filled as
/// deserialization does (`FillShortTxIDSelector`). The header and nonce are private because the
/// keys are computed from them.
#[derive(Clone)]
pub struct CompactBlock {
    header: Header,
    nonce: u64,
    /// `shorttxids`, 48-bit values.
    pub short_ids: Vec<u64>,
    /// `prefilledtxn`.
    pub prefilled: Vec<Prefilled>,
    keys: (u64, u64),
}

impl std::fmt::Debug for CompactBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompactBlock")
            .field("hash", &self.header.hash())
            .field("short_ids", &self.short_ids.len())
            .field("prefilled", &self.prefilled.len())
            .finish()
    }
}

impl CompactBlock {
    /// From the decoded message. The decoder has already refused more than 65,535 transactions
    /// (blockencodings.h:120-122).
    pub fn from_msg(msg: CmpctBlock) -> Self {
        let keys = msg.short_id_keys();
        Self {
            header: Header::new(msg.header),
            nonce: msg.nonce,
            short_ids: msg.short_ids.iter().map(|s| s.get()).collect(),
            prefilled: msg
                .prefilled
                .into_iter()
                .map(|p| Prefilled {
                    index: p.index,
                    tx: Arc::new(Tx::new(p.tx)),
                })
                .collect(),
            keys,
        }
    }

    /// Core's `CBlockHeaderAndShortTxIDs(block)` with a chosen nonce: the coinbase prefilled at
    /// index 0 and a short ID of each other transaction's instance hash (be:21-35).
    pub fn from_block(block: &Block, nonce: u64) -> Self {
        let mut out = Self::from_msg(CmpctBlock {
            header: *block.header().raw(),
            nonce,
            short_ids: Vec::new(),
            prefilled: Vec::new(),
        });
        out.prefilled = block
            .txs()
            .first()
            .cloned()
            .map(|tx| Prefilled {
                index: 0,
                tx,
            })
            .into_iter()
            .collect();
        out.short_ids =
            block.txs().iter().skip(1).map(|tx| out.short_id(&tx.instance_hash())).collect();
        out
    }

    /// The message, for the sim's peers and tests. base-sdk encodes its transactions, not
    /// Core's serialization, so the node's own `cmpctblock` is [`CompactBlock::wire_bytes`].
    #[cfg(any(test, feature = "sim"))]
    pub fn to_msg(&self) -> CmpctBlock {
        CmpctBlock {
            header: *self.header.raw(),
            nonce: self.nonce,
            short_ids: self.short_ids.iter().map(|&s| dash_p2p_core::ShortTxId::new(s)).collect(),
            prefilled: self
                .prefilled
                .iter()
                .map(|p| dash_p2p_core::PrefilledTx {
                    index: p.index,
                    tx: p.tx.to_mutable(),
                })
                .collect(),
        }
    }

    /// Core's serialization of the message (blockencodings.h:116-118, :79): the header, the
    /// nonce, the 6-byte little-endian short IDs, then each prefilled transaction's compact-size
    /// index and Core's serialization of the transaction. This is the `cmpctblock` payload the
    /// node sends, built from consensus encoders, never from base-sdk's (A1 §1.3).
    pub fn wire_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.serialized_size());
        out.write(&header_bytes(self.header.raw()));
        out.write(&self.nonce.to_le_bytes());
        write_compact_size(self.short_ids.len() as u64, &mut out);
        for id in &self.short_ids {
            out.write(&id.to_le_bytes()[..6]);
        }
        write_compact_size(self.prefilled.len() as u64, &mut out);
        for p in &self.prefilled {
            write_compact_size(u64::from(p.index), &mut out);
            p.tx.core_write(&mut out);
        }
        out
    }

    /// `header`.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// `GetSerializeSize(cmpctblock)`, which Core's `InitData` logs: the size of Core's
    /// serialization, not of the payload, which may carry ignored trailing bytes.
    pub fn serialized_size(&self) -> usize {
        let prefilled: usize = self
            .prefilled
            .iter()
            .map(|p| compact_size_len(u64::from(p.index)) + p.tx.size() as usize)
            .sum();
        80 + 8
            + compact_size_len(self.short_ids.len() as u64)
            + 6 * self.short_ids.len()
            + compact_size_len(self.prefilled.len() as u64)
            + prefilled
    }

    /// `GetShortID(hash)`: SipHash-2-4 of the hash under the header's keys, masked to 48 bits.
    // Core: blockencodings.cpp:48-51
    pub fn short_id(&self, hash: &TxHash) -> u64 {
        short_id_with_keys(self.keys.0, self.keys.1, hash.as_bytes()).get()
    }

    /// `BlockTxCount()`.
    pub fn block_tx_count(&self) -> usize {
        self.short_ids.len() + self.prefilled.len()
    }
}

/// Core's `PartiallyDownloadedBlock` (be:55-223). `header` is `None` where Core's is null.
#[derive(Clone, Default)]
pub struct PartiallyDownloadedBlock {
    header: Option<Header>,
    txn_available: Vec<Option<TxRef>>,
    prefilled_count: usize,
    mempool_count: usize,
    extra_count: usize,
}

impl std::fmt::Debug for PartiallyDownloadedBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PartiallyDownloadedBlock")
            .field("header", &self.header.map(|h| h.hash()))
            .field("txn", &self.txn_available.len())
            .field("prefilled_count", &self.prefilled_count)
            .field("mempool_count", &self.mempool_count)
            .field("extra_count", &self.extra_count)
            .finish()
    }
}

impl PartiallyDownloadedBlock {
    /// An empty block (Core's constructor).
    pub fn new() -> Self {
        Self::default()
    }

    /// `header`, or `None` when Core's is null: before `init_data`, or once `fill_block` has
    /// placed every transaction (a too-short `missing` keeps it).
    pub fn header(&self) -> Option<&Header> {
        self.header.as_ref()
    }

    /// `InitData(cmpctblock, extra_txn)`, with `pool` as the mempool.
    // Core: blockencodings.cpp:55-175
    pub fn init_data(
        &mut self,
        cmpct: &CompactBlock,
        pool: &dyn CompactTxSource,
        extra: &ExtraTxns,
    ) -> ReadStatus {
        // `CBlockHeader::IsNull()` is `nBits == 0`.
        if cmpct.header.bits() == 0 || (cmpct.short_ids.is_empty() && cmpct.prefilled.is_empty()) {
            return ReadStatus::Invalid;
        }
        if cmpct.block_tx_count() > MAX_BLOCK_SIZE / MIN_TRANSACTION_SIZE {
            return ReadStatus::Invalid;
        }
        if self.header.is_some() || !self.txn_available.is_empty() {
            return ReadStatus::Invalid;
        }

        self.header = Some(cmpct.header);
        self.txn_available = vec![None; cmpct.block_tx_count()];

        let mut last_prefilled: i32 = -1;
        for (i, p) in cmpct.prefilled.iter().enumerate() {
            if p.tx.is_null() {
                return ReadStatus::Invalid;
            }
            last_prefilled += i32::from(p.index) + 1;
            if last_prefilled > i32::from(u16::MAX) {
                return ReadStatus::Invalid;
            }
            // A transaction past every short ID and prefilled one so far has neither.
            if last_prefilled as usize > cmpct.short_ids.len() + i {
                return ReadStatus::Invalid;
            }
            self.txn_available[last_prefilled as usize] = Some(Arc::clone(&p.tx));
        }
        self.prefilled_count = cmpct.prefilled.len();

        let mut index = ShortIdIndex::new(cmpct.short_ids.len());
        let mut offset = 0usize;
        for (i, &id) in cmpct.short_ids.iter().enumerate() {
            while self.txn_available[i + offset].is_some() {
                offset += 1;
            }
            // `i + index_offset` is below `BlockTxCount()`, which the decoder bounds by 65,535.
            if !index.insert(id, (i + offset) as u16) {
                return ReadStatus::Failed;
            }
        }
        // A short-ID collision.
        if index.len() != cmpct.short_ids.len() {
            return ReadStatus::Failed;
        }

        let mut have_txn = vec![false; self.txn_available.len()];
        pool.for_each_in_vtxhashes_order(&mut |hash, tx| {
            if let Some(pos) = index.get(cmpct.short_id(hash)) {
                if !have_txn[pos] {
                    self.txn_available[pos] = Some(Arc::clone(tx));
                    have_txn[pos] = true;
                    self.mempool_count += 1;
                } else if self.txn_available[pos].take().is_some() {
                    // Two mempool transactions share the short ID: request it instead.
                    self.mempool_count -= 1;
                }
            }
            self.mempool_count != index.len()
        });

        for (hash, tx) in extra.iter() {
            if let Some(pos) = index.get(cmpct.short_id(hash)) {
                if !have_txn[pos] {
                    self.txn_available[pos] = Some(Arc::clone(tx));
                    have_txn[pos] = true;
                    self.mempool_count += 1;
                    // After the wrap below, Core's `size_t` wraps back.
                    self.extra_count = self.extra_count.wrapping_add(1);
                } else if self.txn_available[pos].as_ref().is_some_and(|t| t.txid() != tx.txid()) {
                    // A second match: compare txids first, so the same transaction in the mempool
                    // and the ring does not clear the slot. When the slot came from the mempool,
                    // Core's `size_t extra_count` wraps; only the reconstruction log reads it.
                    self.txn_available[pos] = None;
                    self.mempool_count -= 1;
                    self.extra_count = self.extra_count.wrapping_sub(1);
                }
            }
            if self.mempool_count == index.len() {
                break;
            }
        }

        tracing::debug!(
            target: "cmpctblock",
            "Initialized PartiallyDownloadedBlock for block {} using a cmpctblock of size {}",
            cmpct.header.hash(),
            cmpct.serialized_size()
        );
        ReadStatus::Ok
    }

    /// `IsTxAvailable(index)`.
    // Core: blockencodings.cpp:177-183
    pub fn is_tx_available(&self, index: usize) -> bool {
        self.header.is_some() && self.txn_available[index].is_some()
    }

    /// How many transactions the block has (`txn_available.size()`).
    pub fn tx_count(&self) -> usize {
        self.txn_available.len()
    }

    /// `FillBlock(block, vtx_missing)`. Too few missing transactions return `Invalid` before the
    /// header is cleared and too many after it (Q8); the available ones met so far have been
    /// moved out either way.
    // Core: blockencodings.cpp:185-224
    pub fn fill_block(&mut self, missing: &[TxRef]) -> (ReadStatus, Option<Block>) {
        let Some(header) = self.header else {
            return (ReadStatus::Invalid, None);
        };
        let hash = header.hash();
        let mut txs = Vec::with_capacity(self.txn_available.len());
        let mut next_missing = 0;
        for slot in &mut self.txn_available {
            match slot.take() {
                Some(tx) => txs.push(tx),
                None => {
                    let Some(tx) = missing.get(next_missing) else {
                        return (ReadStatus::Invalid, None);
                    };
                    txs.push(Arc::clone(tx));
                    next_missing += 1;
                }
            }
        }

        // Make sure `fill_block` cannot run again.
        self.header = None;
        self.txn_available.clear();

        if missing.len() != next_missing {
            return (ReadStatus::Invalid, None);
        }

        let block = Block::new(header, txs);
        // A seemingly good block can still be a short-ID collision.
        if let Some(mutation) = block_mutation(&block) {
            // Core: validation.cpp:4434-4440 IsBlockMutated
            if let Mutation::MerkleRoot(state) = mutation {
                tracing::debug!(target: "validation", "Block mutated: {state}");
            }
            return (ReadStatus::Failed, None);
        }

        tracing::debug!(
            target: "cmpctblock",
            "Successfully reconstructed block {hash} with {} txn prefilled, {} txn from mempool (incl at least {} from extra pool) and {} txn requested",
            self.prefilled_count,
            self.mempool_count,
            self.extra_count,
            missing.len()
        );
        if missing.len() < 5 {
            for tx in missing {
                tracing::debug!(target: "cmpctblock", "Reconstructed block {hash} required tx {}", tx.txid());
            }
        }
        (ReadStatus::Ok, Some(block))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use dash_consensus::decode_tx;
    use dash_primitives::{BlockHash, BlockHeader, MerkleRoot};
    use dash_types::Numeric as _;
    use dash_types::codec::BaseCodec;

    use super::*;

    /// A Core-serialized transaction: version 1, one input per prevout with a `script_sig_len`-byte
    /// zero script, one output of `value` paying `OP_TRUE`, lock time 0.
    fn raw_tx(prevouts: &[([u8; 32], u32)], script_sig_len: u8, value: u64) -> Vec<u8> {
        let mut raw = vec![1, 0, 0, 0, prevouts.len() as u8];
        for (hash, n) in prevouts {
            raw.extend_from_slice(hash);
            raw.extend_from_slice(&n.to_le_bytes());
            raw.push(script_sig_len);
            raw.extend(std::iter::repeat_n(0, usize::from(script_sig_len)));
            raw.extend_from_slice(&[0xff; 4]);
        }
        raw.push(1);
        raw.extend_from_slice(&value.to_le_bytes());
        raw.extend_from_slice(&[1, 0x51]);
        raw.extend_from_slice(&[0; 4]);
        raw
    }

    /// A Core-serialized version 2 asset unlock (stable txid): no inputs, one output, and a
    /// payload whose quorum info ends in a signature holding `sig_seed`. The txid leaves the
    /// quorum info out, so every seed gives the same txid and a different instance hash.
    fn raw_asset_unlock(sig_seed: u32) -> Vec<u8> {
        let mut raw = vec![3, 0, 9, 0, 0, 1];
        raw.extend_from_slice(&100_000_000u64.to_le_bytes());
        raw.extend_from_slice(&[1, 0x51]);
        raw.extend_from_slice(&[0; 4]);
        // Version, index, fee, then the quorum info: requested height, quorum hash, signature.
        raw.extend_from_slice(&[145, 2]);
        raw.extend_from_slice(&7u64.to_le_bytes());
        raw.extend_from_slice(&1_000u32.to_le_bytes());
        raw.extend_from_slice(&100u32.to_le_bytes());
        raw.extend_from_slice(&[0xab; 32]);
        let mut sig = [0; 96];
        sig[..4].copy_from_slice(&sig_seed.to_le_bytes());
        raw.extend_from_slice(&sig);
        raw
    }

    fn decode(raw: &[u8]) -> TxRef {
        Arc::new(decode_tx(&mut &raw[..]).expect("a valid transaction").into_value())
    }

    fn prevout(seed: u32) -> [u8; 32] {
        dash_sha256::sha256(&seed.to_le_bytes())
    }

    /// The one-input transaction [`tx`] makes from `seed`.
    fn raw_one_input(seed: u32) -> Vec<u8> {
        raw_tx(&[(prevout(seed.wrapping_mul(1000)), 0)], 0, 42)
    }

    /// A transaction spending `inputs` prevouts derived from `seed`.
    fn tx(seed: u32, inputs: u32) -> TxRef {
        let prevouts: Vec<_> =
            (0..inputs).map(|i| (prevout(seed.wrapping_mul(1000).wrapping_add(i)), 0)).collect();
        decode(&raw_tx(&prevouts, 0, 42))
    }

    fn asset_unlock(sig_seed: u32) -> TxRef {
        let tx = decode(&raw_asset_unlock(sig_seed));
        assert!(tx.is_asset_unlock_with_stable_txid());
        tx
    }

    /// A coinbase (null prevout, 10-byte script), as `BuildBlockTestCase` makes it.
    fn coinbase() -> TxRef {
        decode(&raw_tx(&[([0; 32], u32::MAX)], 10, 42))
    }

    fn raw_header(merkle_root: MerkleRoot) -> BlockHeader {
        BlockHeader {
            version: 42,
            prev_hash: BlockHash::from_lendian([7; 32]),
            merkle_root,
            time: 1,
            bits: 0x207f_ffff,
            nonce: 0,
        }
    }

    /// A block over `txs` whose header commits to their merkle root.
    fn block(txs: Vec<TxRef>) -> Block {
        let (root, mutated) =
            Block::new(Header::new(raw_header(MerkleRoot::ZERO)), txs.clone()).merkle();
        assert!(!mutated);
        Block::new(Header::new(raw_header(root)), txs)
    }

    /// `BuildBlockTestCase`: a coinbase, a one-input and a ten-input transaction.
    fn test_block() -> Block {
        block(vec![coinbase(), tx(1, 1), tx(2, 10)])
    }

    fn prefilled(index: u16, tx: &TxRef) -> Prefilled {
        Prefilled {
            index,
            tx: Arc::clone(tx),
        }
    }

    /// A mempool in `vTxHashes` order that counts the entries the walk visits.
    #[derive(Default)]
    struct Pool {
        entries: Vec<(TxHash, TxRef)>,
        visited: AtomicUsize,
    }

    impl Pool {
        fn of(txs: &[&TxRef]) -> Self {
            Self {
                entries: txs.iter().map(|t| (t.instance_hash(), Arc::clone(t))).collect(),
                ..Self::default()
            }
        }
    }

    impl CompactTxSource for Pool {
        fn for_each_in_vtxhashes_order(&self, f: &mut dyn FnMut(&TxHash, &TxRef) -> bool) {
            for (hash, tx) in &self.entries {
                self.visited.fetch_add(1, Ordering::Relaxed);
                if !f(hash, tx) {
                    return;
                }
            }
        }
    }

    fn ring(txs: &[&TxRef]) -> ExtraTxns {
        let mut ring = ExtraTxns::default();
        for tx in txs {
            ring.add(Arc::clone(tx));
        }
        ring
    }

    /// A compact block with `short_ids` under the fixed header and nonce (7) whose SipHash keys
    /// the collisions were ground for.
    fn collision_compact(short_ids: Vec<u64>) -> CompactBlock {
        let mut c = CompactBlock::from_msg(CmpctBlock {
            header: raw_header(MerkleRoot::ZERO),
            nonce: 7,
            short_ids: Vec::new(),
            prefilled: Vec::new(),
        });
        c.short_ids = short_ids;
        c
    }

    /// Seeds of two [`tx`]s with different txids and one short ID (0x3ee6_2551_1938) under
    /// [`collision_compact`]'s keys, found by [`grind_one_input_collision`].
    const COLLIDING_SEEDS: (u32, u32) = (22_153_868, 41_482_130);

    /// Signature seeds of two [`asset_unlock`]s with one txid, different instance hashes and one
    /// short ID under [`collision_compact`]'s keys, found by [`grind_asset_unlock_collision`].
    const COLLIDING_UNLOCK_SEEDS: (u32, u32) = (11_363_086, 14_128_518);

    /// The first two seeds whose transactions' instance hashes share a short ID under
    /// [`collision_compact`]'s keys. About 2^24.5 tries.
    fn grind(raw: impl Fn(u32) -> Vec<u8>) {
        let c = collision_compact(Vec::new());
        let mut seen: HashMap<u64, u32> = HashMap::new();
        for seed in 0u32.. {
            let id = c.short_id(&TxHash::from_lendian(dash_sha256::sha256d(&raw(seed))));
            if let Some(other) = seen.insert(id, seed) {
                println!("seeds ({other}, {seed}), short ID {id:#x}");
                return;
            }
        }
    }

    /// For `COLLIDING_SEEDS`: run it with
    /// `cargo test --release -p dashd-p2p --lib grind_ -- --ignored --nocapture`.
    #[test]
    #[ignore = "a one-off search; its result is COLLIDING_SEEDS"]
    fn grind_one_input_collision() {
        grind(raw_one_input);
    }

    /// For `COLLIDING_UNLOCK_SEEDS`, as [`grind_one_input_collision`].
    #[test]
    #[ignore = "a one-off search; its result is COLLIDING_UNLOCK_SEEDS"]
    fn grind_asset_unlock_collision() {
        grind(raw_asset_unlock);
    }

    /// The GCC 13.3.0 static probe's bucket counts (`contrib/node/p3-buckets/run.sh`), expanded
    /// to every n in 0..=65,535.
    fn reference_bucket_counts() -> Vec<u64> {
        let text =
            include_str!("../../tests/fixtures/libstdcxx/unordered-map-buckets-gcc13.3.0.txt");
        let steps: Vec<(usize, u64)> = text
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(|l| {
                let (n, count) = l.split_once(' ').expect("'n count'");
                (n.parse().expect("n"), count.parse().expect("count"))
            })
            .collect();
        let mut out = Vec::with_capacity(65_536);
        for (i, &(n, count)) in steps.iter().enumerate() {
            assert_eq!(out.len(), n, "the fixture's steps are contiguous");
            let end = steps.get(i + 1).map_or(65_536, |s| s.0);
            out.extend(std::iter::repeat_n(count, end - n));
        }
        out
    }

    /// D22's sweep: every n the decoder accepts.
    #[test]
    fn bucket_count_matches_the_gcc13_static_unordered_map_for_every_n() {
        let reference = reference_bucket_counts();
        assert_eq!(reference.len(), 65_536);
        for (n, &want) in reference.iter().enumerate() {
            assert_eq!(bucket_count(n), want, "n = {n}");
        }
        // Past the second-to-last prime, `lower_bound` stops at `__last_prime`.
        assert_eq!(bucket_count(usize::MAX), PRIME_LIST[303]);
    }

    /// The table is the release's 0x988 bytes (P3 D33, D90).
    #[test]
    fn prime_table_is_the_release_table() {
        assert!(PRIME_LIST[..304].windows(2).all(|w| w[0] < w[1]));
        assert_eq!(PRIME_LIST[303], PRIME_LIST[304]);
        let bytes: Vec<u8> = PRIME_LIST.iter().flat_map(|p| p.to_le_bytes()).collect();
        assert_eq!(bytes.len(), 0x988);
        let digest: String =
            dash_sha256::sha256(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(digest, "4eae514cebdbfa8abf7634adb11f0e0781d9b6c8b4a35629c209c3c66f1e87a5");
    }

    /// `n` short IDs, `crowd` of them first and in bucket 1 of `bucket_count(n)`, then one in
    /// each other bucket.
    fn crowded(n: usize, crowd: usize) -> Vec<u64> {
        let b = bucket_count(n);
        let mut ids: Vec<u64> = (0..crowd as u64).map(|k| 1 + k * b).collect();
        ids.extend((0..b).filter(|&x| x != 1).take(n - crowd));
        ids
    }

    /// §2.3.2's transition sets: n−1, n and n+1 around every bucket-count change, with 13 IDs
    /// in one bucket (fails at the 13th insertion) and with 12 (never fails).
    #[test]
    fn transition_sets_fail_at_the_thirteenth_insertion_into_one_bucket() {
        let reference = reference_bucket_counts();
        let mut sizes = std::collections::BTreeSet::new();
        for n in 1..reference.len() {
            if reference[n] != reference[n - 1] {
                sizes.extend([n - 1, n, n + 1].into_iter().filter(|m| (13..=65_535).contains(m)));
            }
        }
        assert!(sizes.contains(&13) && sizes.len() > 300);
        for &n in &sizes {
            for crowd in [12, 13] {
                let mut index = ShortIdIndex::new(n);
                let ids = crowded(n, crowd);
                assert_eq!(ids.len(), n);
                let first_failure =
                    ids.iter().enumerate().position(|(i, &id)| !index.insert(id, i as u16));
                assert_eq!(first_failure, (crowd == 13).then_some(12), "n = {n}, crowd = {crowd}");
            }
        }
    }

    fn init_status(c: &CompactBlock) -> ReadStatus {
        PartiallyDownloadedBlock::new().init_data(c, &NoMempool, &ExtraTxns::default())
    }

    #[test]
    fn init_data_fails_on_a_crowded_bucket() {
        assert_eq!(init_status(&collision_compact(crowded(100, 12))), ReadStatus::Ok);
        assert_eq!(init_status(&collision_compact(crowded(100, 13))), ReadStatus::Failed);
    }

    /// A re-assigned short ID adds no element: its bucket never passes 12, and the size check
    /// fails the block instead (be:107-110).
    #[test]
    fn a_duplicate_short_id_does_not_grow_its_bucket() {
        let mut index = ShortIdIndex::new(20);
        assert!((0..20).all(|i| index.insert(5, i)));
        assert_eq!((index.len(), index.get(5)), (1, Some(19)));
        assert_eq!(init_status(&collision_compact(vec![5, 5])), ReadStatus::Failed);
    }

    fn wire_round_trip(c: &CompactBlock) -> CompactBlock {
        let mut buf = Vec::new();
        c.to_msg().encode(&mut buf);
        assert_eq!(c.serialized_size(), buf.len());
        CompactBlock::from_msg(CmpctBlock::decode(&mut &buf[..]).expect("decodes"))
    }

    fn init_with_extra(
        c: &CompactBlock,
        pool: &dyn CompactTxSource,
        extra: &ExtraTxns,
    ) -> PartiallyDownloadedBlock {
        let mut p = PartiallyDownloadedBlock::new();
        assert_eq!(p.init_data(c, pool, extra), ReadStatus::Ok);
        p
    }

    fn init(c: &CompactBlock, pool: &dyn CompactTxSource) -> PartiallyDownloadedBlock {
        init_with_extra(c, pool, &ExtraTxns::default())
    }

    fn available(p: &PartiallyDownloadedBlock) -> Vec<bool> {
        (0..p.tx_count()).map(|i| p.is_tx_available(i)).collect()
    }

    /// `fill_block`, which must reconstruct `want`.
    fn fill_ok(mut p: PartiallyDownloadedBlock, missing: &[TxRef], want: &Block) {
        let (status, got) = p.fill_block(missing);
        assert_eq!(status, ReadStatus::Ok);
        let got = got.expect("a block");
        assert_eq!((got.hash(), got.merkle()), (want.hash(), want.merkle()));
    }

    // Core: test/blockencodings_tests.cpp:57-108 SimpleRoundTripTest
    #[test]
    fn simple_round_trip() {
        let b = test_block();
        let c = wire_round_trip(&CompactBlock::from_block(&b, 9));
        let p = init(&c, &Pool::of(&[&b.txs()[2]]));
        assert_eq!(available(&p), [true, false, true]);

        assert_eq!(p.clone().fill_block(&[]).0, ReadStatus::Invalid);
        // The wrong transaction makes a block whose merkle root does not match.
        assert_eq!(p.clone().fill_block(&b.txs()[2..]).0, ReadStatus::Failed);
        fill_ok(p, &b.txs()[1..2], &b);
    }

    // Core: test/blockencodings_tests.cpp:140-208 NonCoinbasePreforwardRTTest
    #[test]
    fn non_coinbase_preforward_round_trip() {
        let b = test_block();
        let mut c = CompactBlock::from_block(&b, 9);
        c.prefilled = vec![prefilled(1, &b.txs()[1])];
        c.short_ids = vec![c.short_id(&b.txs()[0].txid()), c.short_id(&b.txs()[2].txid())];
        let c = wire_round_trip(&c);
        let p = init(&c, &Pool::of(&[&b.txs()[2]]));
        assert_eq!(available(&p), [false, true, true]);

        // Short at slot 0: nothing has been moved out yet.
        let mut short = p.clone();
        assert_eq!(short.fill_block(&[]).0, ReadStatus::Invalid);
        assert_eq!(available(&short), [false, true, true]);
        assert_eq!(p.clone().fill_block(&b.txs()[1..2]).0, ReadStatus::Failed);
        fill_ok(p, &b.txs()[..1], &b);
    }

    // Core: test/blockencodings_tests.cpp:210-259 SufficientPreforwardRTTest
    #[test]
    fn sufficient_preforward_round_trip() {
        let b = test_block();
        let mut c = CompactBlock::from_block(&b, 9);
        // 1 after index 0: index 2.
        c.prefilled = vec![prefilled(0, &b.txs()[0]), prefilled(1, &b.txs()[2])];
        c.short_ids = vec![c.short_id(&b.txs()[1].txid())];
        let c = wire_round_trip(&c);
        let p = init(&c, &Pool::of(&[&b.txs()[1]]));
        assert_eq!(available(&p), [true, true, true]);
        fill_ok(p, &[], &b);
    }

    // Core: test/blockencodings_tests.cpp:261-303 EmptyBlockRoundTripTest
    #[test]
    fn empty_block_round_trip() {
        let b = block(vec![coinbase()]);
        let c = wire_round_trip(&CompactBlock::from_block(&b, 9));
        assert!(c.short_ids.is_empty());
        let p = init(&c, &NoMempool);
        assert_eq!(available(&p), [true]);
        fill_ok(p, &[], &b);
    }

    /// Every `Invalid` and `Failed` return of `InitData` (be:56-110).
    #[test]
    fn init_data_rejects_as_core_does() {
        let b = test_block();
        let good = CompactBlock::from_block(&b, 9);
        assert_eq!(init_status(&good), ReadStatus::Ok);

        let mut null_header = good.clone();
        null_header.header = Header::new(BlockHeader {
            bits: 0,
            ..*good.header.raw()
        });
        assert_eq!(init_status(&null_header), ReadStatus::Invalid);

        let mut empty = good.clone();
        empty.short_ids.clear();
        empty.prefilled.clear();
        assert_eq!(init_status(&empty), ReadStatus::Invalid);

        // More than `MaxBlockSize() / MIN_TRANSACTION_SIZE` transactions.
        let mut huge = good.clone();
        huge.short_ids = (0..200_000).collect();
        assert_eq!(init_status(&huge), ReadStatus::Invalid);
        huge.short_ids.pop();
        assert_eq!(init_status(&huge), ReadStatus::Ok);

        // A null prefilled transaction.
        let mut null_tx = good.clone();
        null_tx.prefilled[0].tx = decode(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(null_tx.prefilled[0].tx.is_null());
        assert_eq!(init_status(&null_tx), ReadStatus::Invalid);

        // A prefilled index past every short ID and prefilled transaction so far.
        let mut past = good.clone();
        past.prefilled[0].index = 3;
        assert_eq!(init_status(&past), ReadStatus::Invalid);
        past.prefilled[0].index = 2;
        assert_eq!(init_status(&past), ReadStatus::Ok);

        // A running index past 65,535.
        let mut wide = good.clone();
        wide.short_ids = (0..65_535).collect();
        wide.prefilled = vec![prefilled(u16::MAX, &coinbase()), prefilled(0, &coinbase())];
        assert_eq!(init_status(&wide), ReadStatus::Invalid);
        wide.prefilled.pop();
        assert_eq!(init_status(&wide), ReadStatus::Ok);

        // A second `InitData` on the same object.
        let mut p = init(&good, &NoMempool);
        assert_eq!(p.init_data(&good, &NoMempool, &ExtraTxns::default()), ReadStatus::Invalid);

        // Before `InitData`, nothing is available and the empty vector is not read.
        assert!(!PartiallyDownloadedBlock::new().is_tx_available(0));
    }

    /// Two mempool entries with one short ID clear the slot, and a third leaves it empty
    /// (be:119-131); the walk stops once every short ID is found (be:136-137).
    #[test]
    fn mempool_walk_resets_duplicates_and_stops_early() {
        let b = test_block();
        let c = CompactBlock::from_block(&b, 9);
        let h = b.txs()[1].instance_hash();
        let dup = Pool {
            entries: vec![(h, tx(5, 1)), (h, tx(6, 1)), (h, tx(7, 1))],
            ..Pool::default()
        };
        let p = init(&c, &dup);
        assert_eq!(available(&p), [true, false, false]);
        assert_eq!(p.mempool_count, 0);

        let pool = Pool::of(&[&b.txs()[1], &b.txs()[2], &tx(8, 1)]);
        let p = init(&c, &pool);
        assert_eq!(available(&p), [true, true, true]);
        assert_eq!((p.mempool_count, pool.visited.load(Ordering::Relaxed)), (2, 2));

        // With no short IDs, the first entry already satisfies the test after it.
        let only_coinbase = CompactBlock::from_block(&block(vec![coinbase()]), 9);
        let pool = Pool::of(&[&tx(9, 1), &tx(10, 1)]);
        init(&only_coinbase, &pool);
        assert_eq!(pool.visited.load(Ordering::Relaxed), 1);
    }

    /// `AddToCompactExtraTransactions` by direct injection (S3, D19).
    #[test]
    fn extra_ring_holds_the_newest_entries() {
        for max in [0, -5] {
            let mut ring = ExtraTxns::new(max);
            ring.add(tx(1, 1));
            assert!(ring.is_empty());
        }

        let mut one = ExtraTxns::new(1);
        one.add(tx(1, 1));
        one.add(tx(2, 1));
        let held: Vec<_> = one.iter().map(|(h, _)| *h).collect();
        assert_eq!(held, [tx(2, 1).instance_hash()]);

        let txs: Vec<_> = (0..101).map(|i| tx(i, 1)).collect();
        let full = ring(&txs.iter().collect::<Vec<_>>());
        assert_eq!(full.len(), 100);
        assert!(full.iter().all(|(h, t)| *h == t.instance_hash()));
        // The 101st replaced the oldest, slot 0; the rest keep their slots.
        let want: Vec<_> =
            std::iter::once(100).chain(1..100).map(|i| txs[i].instance_hash()).collect();
        assert_eq!(full.iter().map(|(h, _)| *h).collect::<Vec<_>>(), want);
    }

    #[test]
    fn init_data_fills_from_the_extra_ring() {
        let b = test_block();
        let c = CompactBlock::from_block(&b, 9);
        let extra = ring(&[&b.txs()[2]]);
        let p = init_with_extra(&c, &NoMempool, &extra);
        assert_eq!(available(&p), [true, false, true]);
        assert_eq!((p.mempool_count, p.extra_count), (1, 1));

        // The same transaction in the mempool and the ring changes nothing: equal txids.
        let p = init_with_extra(&c, &Pool::of(&[&b.txs()[2]]), &extra);
        assert_eq!(available(&p), [true, false, true]);
        assert_eq!((p.mempool_count, p.extra_count), (1, 0));
    }

    /// Short IDs, the mempool and the ring use instance hashes: a re-signed asset unlock (same
    /// txid) is not the block's (be:28-33).
    #[test]
    fn asset_unlocks_are_matched_by_instance_hash() {
        let (x, y) = (asset_unlock(1), asset_unlock(2));
        assert_eq!(x.txid(), y.txid());
        assert_ne!(x.instance_hash(), y.instance_hash());
        let b = block(vec![coinbase(), Arc::clone(&x)]);
        let c = CompactBlock::from_block(&b, 9);
        assert_eq!(c.short_ids, [c.short_id(&x.instance_hash())]);

        assert_eq!(available(&init(&c, &Pool::of(&[&y]))), [true, false]);
        assert_eq!(available(&init(&c, &Pool::of(&[&x]))), [true, true]);
        assert_eq!(available(&init_with_extra(&c, &NoMempool, &ring(&[&y]))), [true, false]);
        assert_eq!(available(&init_with_extra(&c, &NoMempool, &ring(&[&x]))), [true, true]);
    }

    /// The second-match test at be:157-162 with real transactions: different txids and one
    /// short ID clear the slot; one txid (two instances of an asset unlock) does not.
    #[test]
    fn a_second_extra_match_compares_txids() {
        let (a, b) = (tx(COLLIDING_SEEDS.0, 1), tx(COLLIDING_SEEDS.1, 1));
        let probe = collision_compact(Vec::new());
        assert_ne!(a.txid(), b.txid());
        let id = probe.short_id(&a.instance_hash());
        assert_eq!(id, probe.short_id(&b.instance_hash()));

        // With one short ID, `mempool_count == shorttxids.size()` stops the ring walk at the
        // first match (be:168-169), so the second is never compared.
        let p = init_with_extra(&collision_compact(vec![id]), &NoMempool, &ring(&[&a, &b]));
        assert_eq!(available(&p), [true]);
        assert_eq!((p.mempool_count, p.extra_count), (1, 1));

        // The ring's first entry is still compared after the mempool found everything: the test
        // follows each entry.
        let p = init_with_extra(&collision_compact(vec![id]), &Pool::of(&[&a]), &ring(&[&b]));
        assert_eq!(available(&p), [false]);
        assert_eq!((p.mempool_count, p.extra_count), (0, usize::MAX));

        // A second, unmatched short ID keeps the walk going.
        let c = collision_compact(vec![id, id ^ 1]);
        let p = init_with_extra(&c, &NoMempool, &ring(&[&a, &b]));
        assert_eq!(available(&p), [false, false]);
        assert_eq!((p.mempool_count, p.extra_count), (0, 0));

        // One from the mempool: Core's `size_t extra_count` wraps, and a later ring match wraps
        // it back.
        let other = tx(3, 1);
        let c = collision_compact(vec![id, probe.short_id(&other.instance_hash())]);
        let p = init_with_extra(&c, &Pool::of(&[&a]), &ring(&[&b]));
        assert_eq!((p.mempool_count, p.extra_count), (0, usize::MAX));
        let p = init_with_extra(&c, &Pool::of(&[&a]), &ring(&[&b, &other]));
        assert_eq!(available(&p), [false, true]);
        assert_eq!((p.mempool_count, p.extra_count), (1, 0));

        // Two instances of one asset unlock: equal txids leave the mempool's in place.
        let (x, y) =
            (asset_unlock(COLLIDING_UNLOCK_SEEDS.0), asset_unlock(COLLIDING_UNLOCK_SEEDS.1));
        assert_eq!(x.txid(), y.txid());
        assert_ne!(x.instance_hash(), y.instance_hash());
        let id = probe.short_id(&x.instance_hash());
        assert_eq!(id, probe.short_id(&y.instance_hash()));
        let p =
            init_with_extra(&collision_compact(vec![id, id ^ 1]), &Pool::of(&[&x]), &ring(&[&y]));
        assert_eq!(available(&p), [true, false]);
        assert_eq!((p.mempool_count, p.extra_count), (1, 0));
    }

    /// `FillBlock`'s two `Invalid` orders (Q8) and its single use.
    #[test]
    fn fill_block_invalid_orders() {
        let b = test_block();
        let c = CompactBlock::from_block(&b, 9);
        let p = init(&c, &NoMempool);
        assert_eq!(available(&p), [true, false, false]);

        // Too few: returns before clearing the header, with the coinbase already moved out.
        let mut short = p.clone();
        assert_eq!(short.fill_block(&b.txs()[1..2]).0, ReadStatus::Invalid);
        assert!(short.header().is_some());
        assert_eq!(available(&short), [false, false, false]);

        // Too many: returns after clearing it.
        let mut long = p.clone();
        let three = [Arc::clone(&b.txs()[1]), Arc::clone(&b.txs()[2]), tx(3, 1)];
        assert_eq!(long.fill_block(&three).0, ReadStatus::Invalid);
        assert!(long.header().is_none());
        assert_eq!(long.tx_count(), 0);

        let mut once = p.clone();
        assert_eq!(once.fill_block(&b.txs()[1..]).0, ReadStatus::Ok);
        assert_eq!(once.fill_block(&b.txs()[1..]).0, ReadStatus::Invalid);
        assert_eq!(PartiallyDownloadedBlock::new().fill_block(&[]).0, ReadStatus::Invalid);
    }

    /// Without a coinbase first, a 64-byte transaction makes `IsBlockMutated` true.
    #[test]
    fn fill_block_fails_a_block_with_a_64_byte_transaction_and_no_coinbase() {
        let short = decode(&raw_tx(&[(prevout(1), 0)], 3, 42));
        assert_eq!(short.size(), 64);
        let b = block(vec![short]);
        let mut p = init(&CompactBlock::from_block(&b, 9), &NoMempool);
        assert_eq!(p.fill_block(&[]).0, ReadStatus::Failed);
    }

    /// The encoder against dashd's own bytes (P3 §6.1): `cmpct-announce` records the block the
    /// RPC mined and the `cmpctblock` dashd's `NewPoWValidBlock` sent for it. With dashd's nonce,
    /// `from_block` reproduces the message byte for byte: the keys, the three short IDs of the
    /// instance hashes, and the prefilled coinbase in Core's serialization.
    #[test]
    fn wire_bytes_match_dashds_cmpctblock_for_its_nonce() {
        let path =
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dashd-v24/cmpct-announce.txt");
        let text = std::fs::read_to_string(path).expect("the fixture");
        let field = |prefix: &str| {
            let line = text.lines().find(|l| l.starts_with(prefix)).expect(prefix);
            let hex = &line[prefix.len()..];
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect::<Vec<u8>>()
        };
        let raw = field("submitblock ");
        let want = field("< 1 cmpctblock ");
        let block = dash_consensus::decode_block(&mut &raw[..]).expect("a block").into_value();
        assert_eq!(block.txs().len(), 4);
        let nonce = u64::from_le_bytes(want[80..88].try_into().unwrap());
        let got = CompactBlock::from_block(&block, nonce);
        assert_eq!(got.short_ids.len(), 3);
        assert_eq!(got.serialized_size(), want.len());
        assert_eq!(got.wire_bytes(), want);
    }
}
