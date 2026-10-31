//! The `meta` column family's records (CH2 §2.6; A1 §2.9), and [`wipe_chainstate`].
//!
//! Keys are ASCII tags. Values:
//! - `tip`, `evo_tip`: a block hash (32 bytes);
//! - `head_blocks`: `new ‖ old` (64 bytes), `old` all-zero when null, as Core's null `uint256`
//!   (`txdb.cpp:130-139`). It is Core's head-blocks vector without its size, so an empty value
//!   reads as no marker, as Core's empty vector does (`validation.cpp:5230`);
//! - `last_file`: a big-endian `u32`;
//! - `pruned_files`, `reindexing`, `wiping`: one byte, 1 when set. Absent reads as false;
//!   `reindexing` and `wiping` are erased when cleared, as Core erases its flag
//!   (`txdb.cpp:188-193`).
//!
//! Any other length, or a flag byte other than 0 or 1, is corruption: it trips the latch.

use dash_consensus::primitives::BlockHash;
use dashd_store::{Cf, Commit, DbSchema, KvBatch, KvEngine, KvRead as _, StoreError};

use super::{CHAINSTATE_SCHEMA, CoinsReadError, GuardedEngine, GuardedRead};

pub const TIP: &[u8] = b"tip";
pub const HEAD_BLOCKS: &[u8] = b"head_blocks";
pub const EVO_TIP: &[u8] = b"evo_tip";
pub const PRUNED_FILES: &[u8] = b"pruned_files";
pub const LAST_FILE: &[u8] = b"last_file";
pub const REINDEXING: &[u8] = b"reindexing";
pub const WIPING: &[u8] = b"wiping";

/// The column families `wipe_chainstate` empties: the coins and the evo state. `block_index` and
/// `block_files` are kept (§2.6).
const WIPED: [&str; 8] = [
    "coins",
    "mn_diff",
    "mn_snapshot",
    "credit_pool",
    "mnhf",
    "llmq_mined",
    "llmq_mined_h",
    "llmq_snapshot",
];

/// The column families of tip-scoped effects (A1 §2.10.1): V writes them as it connects, ahead of
/// the flush that makes the tip durable. A kill before the first durable tip can leave their
/// records with no tip, and recovery keeps them as it keeps effects above a tip (DEC-202). The
/// mined-commitment rows are transactional, written in B1 with `meta.evo_tip` (CH7 Q1, DEC-517).
pub(crate) const EFFECT_CFS: [&str; 1] = ["llmq_snapshot"];

/// The chainstate's `meta` records.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    /// The block the coins are at; absent during a transition (§3.5).
    pub tip: Option<BlockHash>,
    /// The transition marker: the block being written to, and the one written from (null for a
    /// first flush).
    pub head_blocks: Option<(BlockHash, Option<BlockHash>)>,
    /// The block the evo state is at.
    pub evo_tip: Option<BlockHash>,
    pub pruned_files: bool,
    /// ST2's `BlockFilesState::last_file`.
    pub last_file: u32,
    /// Core's `-reindex` flag (`DB_REINDEX_FLAG`, `txdb.cpp:188-193`): a reindex of the block
    /// files has begun and not finished (CH5).
    pub reindexing: bool,
    /// [`wipe_chainstate`] has begun and not finished. Its own marker, so that a `-reindex` that
    /// wipes keeps `reindexing` set until its replay ends.
    pub wiping: bool,
}

impl Meta {
    /// Reads every record, from the engine or a snapshot (a copy point's, §2.6).
    ///
    /// # Errors
    ///
    /// [`CoinsReadError`]: the engine failed, or a record does not decode (kind `Corruption`).
    /// Either way the latch has tripped.
    pub fn read(db: &impl GuardedRead) -> Result<Self, CoinsReadError> {
        Self::read_with(db, false).map(|(meta, _)| meta)
    }

    // Core: validation.cpp:5229-5233 (a head-blocks vector of another size)
    /// [`read`](Self::read) at recovery's step 1: a `head_blocks` value of a whole number of
    /// hashes other than two reads as no marker and `true`, which recovery reports as Core's
    /// "unknown inconsistent state" (§3.5.1). A length that is no whole number of hashes does not
    /// decode, and is corruption as in [`read`](Self::read).
    ///
    /// # Errors
    ///
    /// As [`read`](Self::read)'s.
    pub(crate) fn read_at_open(db: &impl GuardedRead) -> Result<(Self, bool), CoinsReadError> {
        Self::read_with(db, true)
    }

    fn read_with(
        db: &impl GuardedRead,
        lenient_marker: bool,
    ) -> Result<(Self, bool), CoinsReadError> {
        let hash = |key| read_fixed::<32>(db, key).map(|v| v.map(BlockHash::from));
        // DEC-165(3) audit: no Core text at this point (quirk 44; manager ruling on CH2b c5 item
        // 8). Core's `CDBWrapper::Read` turns bytes that do not decode into false
        // (dbwrapper.h:322-326), `GetHeadBlocks` into an empty vector (txdb.cpp:105-111), and
        // `ReplayBlocks` into a consistent state; here they are corruption, which fails the load.
        let marker = db.read(DbSchema::META, HEAD_BLOCKS)?.filter(|v| !v.is_empty());
        let malformed =
            lenient_marker && marker.as_ref().is_some_and(|v| v.len() % 32 == 0 && v.len() != 64);
        let head_blocks = match marker {
            Some(v) if !malformed => Some(fixed::<64>(db, HEAD_BLOCKS, &v)?),
            _ => None,
        };
        let head_blocks = head_blocks.map(|v| {
            let (new, old) = v.split_at(32);
            let old: [u8; 32] = old.try_into().expect("32 bytes");
            let new = BlockHash::from(<[u8; 32]>::try_from(new).expect("32 bytes"));
            (new, (old != [0; 32]).then(|| BlockHash::from(old)))
        });
        let meta = Self {
            tip: hash(TIP)?,
            head_blocks,
            evo_tip: hash(EVO_TIP)?,
            pruned_files: read_flag(db, PRUNED_FILES)?,
            last_file: read_fixed::<4>(db, LAST_FILE)?.map_or(0, u32::from_be_bytes),
            reindexing: read_flag(db, REINDEXING)?,
            wiping: read_flag(db, WIPING)?,
        };
        Ok((meta, malformed))
    }

    pub fn put_tip(b: &mut impl KvBatch, tip: &BlockHash) {
        b.put(DbSchema::META, TIP, tip.as_bytes());
    }

    pub fn erase_tip(b: &mut impl KvBatch) {
        b.delete(DbSchema::META, TIP);
    }

    /// The marker of a split flush: `new ‖ old`, `old` all-zero when null.
    pub fn put_head_blocks(b: &mut impl KvBatch, new: &BlockHash, old: Option<&BlockHash>) {
        let mut v = [0; 64];
        v[..32].copy_from_slice(new.as_bytes());
        if let Some(old) = old {
            v[32..].copy_from_slice(old.as_bytes());
        }
        b.put(DbSchema::META, HEAD_BLOCKS, &v);
    }

    pub fn erase_head_blocks(b: &mut impl KvBatch) {
        b.delete(DbSchema::META, HEAD_BLOCKS);
    }

    pub fn put_evo_tip(b: &mut impl KvBatch, tip: &BlockHash) {
        b.put(DbSchema::META, EVO_TIP, tip.as_bytes());
    }

    pub fn erase_evo_tip(b: &mut impl KvBatch) {
        b.delete(DbSchema::META, EVO_TIP);
    }

    pub fn put_pruned_files(b: &mut impl KvBatch, pruned: bool) {
        b.put(DbSchema::META, PRUNED_FILES, &[u8::from(pruned)]);
    }

    pub fn put_last_file(b: &mut impl KvBatch, last_file: u32) {
        b.put(DbSchema::META, LAST_FILE, &last_file.to_be_bytes());
    }

    /// Sets the flag, or erases it when cleared.
    pub fn put_reindexing(b: &mut impl KvBatch, reindexing: bool) {
        put_erased_flag(b, REINDEXING, reindexing);
    }

    /// Sets the flag, or erases it when cleared.
    pub fn put_wiping(b: &mut impl KvBatch, wiping: bool) {
        put_erased_flag(b, WIPING, wiping);
    }
}

/// A flag that is erased when cleared.
fn put_erased_flag(b: &mut impl KvBatch, key: &[u8], on: bool) {
    if on {
        b.put(DbSchema::META, key, &[1]);
    } else {
        b.delete(DbSchema::META, key);
    }
}

/// A record of exactly `N` bytes, if present.
fn read_fixed<const N: usize>(
    db: &impl GuardedRead,
    key: &[u8],
) -> Result<Option<[u8; N]>, CoinsReadError> {
    db.read(DbSchema::META, key)?.map(|v| fixed(db, key, &v)).transpose()
}

/// `key`'s value `v`, which must be exactly `N` bytes.
fn fixed<const N: usize>(
    db: &impl GuardedRead,
    key: &[u8],
    v: &[u8],
) -> Result<[u8; N], CoinsReadError> {
    let corrupt = |detail: &str| db.engine().corrupt(DbSchema::META, key, detail);
    <[u8; N]>::try_from(v).map_err(|_| corrupt(&format!("{} bytes, not {N}", v.len())))
}

fn read_flag(db: &impl GuardedRead, key: &[u8]) -> Result<bool, CoinsReadError> {
    match read_fixed::<1>(db, key)? {
        None => Ok(false),
        Some([b @ (0 | 1)]) => Ok(b == 1),
        Some([b]) => Err(db.engine().corrupt(DbSchema::META, key, &format!("flag byte {b}"))),
    }
}

// Core: node/chainstate.cpp:149-160, :233-236 (-reindex-chainstate wipes the coins and evo DBs)
/// `-reindex-chainstate`'s wipe (CH5): a Durable `wiping = 1` with the erase of `tip`, `evo_tip`
/// and `head_blocks`; `delete_range` over the coins and evo column families (BL-38); then a
/// Durable clear of the marker. A crash in between leaves `wiping` set, and the next start runs
/// the wipe again from the top. `reindexing` is not touched.
///
/// # Errors
///
/// The engine's: the latch has tripped.
pub fn wipe_chainstate<E: KvEngine>(db: &GuardedEngine<E>) -> Result<(), StoreError> {
    let mut b = db.batch();
    Meta::put_wiping(&mut b, true);
    Meta::erase_tip(&mut b);
    Meta::erase_evo_tip(&mut b);
    Meta::erase_head_blocks(&mut b);
    db.write(b, Commit::Durable)?;
    let mut b = db.batch();
    for name in WIPED {
        let cf = CHAINSTATE_SCHEMA.cf(name).expect("a chainstate column family");
        delete_all(&mut b, cf);
    }
    db.write(b, Commit::Durable)?;
    let mut b = db.batch();
    Meta::put_wiping(&mut b, false);
    db.write(b, Commit::Durable)
}

/// The first column family [`wipe_chainstate`] empties, other than `skip`, that holds a record, if
/// any: a chainstate with neither tip nor marker must have none outside [`EFFECT_CFS`] (§4.3 step
/// 5, DEC-202). The evo state's are read before `coins`, so evo rows beside coins rows name an evo
/// column family (CH7-notes D71), in one pass: an empty store reads each family once.
///
/// # Errors
///
/// The engine's, from a read: the latch has tripped.
pub(crate) fn nonempty_wiped_cf<E: KvEngine>(
    db: &GuardedEngine<E>,
    skip: &[&str],
) -> Result<Option<&'static str>, StoreError> {
    let (coins, evo) = WIPED.split_first().expect("coins first");
    for &name in evo.iter().chain([coins]).filter(|name| !skip.contains(name)) {
        let cf = CHAINSTATE_SCHEMA.cf(name).expect("a chainstate column family");
        if let Some(record) = db.iter_prefix(cf, &[]).next() {
            record?;
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// Every key of `cf`. No chainstate key is 1,024 bytes of `0xff` or longer, so `[ε, 0xff × 1024)`
/// covers them all.
fn delete_all(b: &mut impl KvBatch, cf: Cf) {
    b.delete_range(cf, &[], &[0xff; 1024]);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dashd_store::{ErrorKind, KvRead};

    use super::*;
    use crate::store::{GlobalFailStop, ProbeEngine, RecordingSink};

    fn db() -> GuardedEngine<ProbeEngine> {
        GuardedEngine::probe(Arc::new(RecordingSink::default()))
    }

    fn hash(b: u8) -> BlockHash {
        BlockHash::from([b; 32])
    }

    fn raw(db: &GuardedEngine<ProbeEngine>, key: &[u8]) -> Option<Vec<u8>> {
        db.inner().unprobed().get(DbSchema::META, key).unwrap()
    }

    fn cf(name: &str) -> Cf {
        CHAINSTATE_SCHEMA.cf(name).unwrap()
    }

    /// The effect column families are wiped, so step 5 skips a subset of what a wipe empties.
    #[test]
    fn effects_are_wiped() {
        assert!(EFFECT_CFS.iter().all(|cf| WIPED.contains(cf)), "{EFFECT_CFS:?}");
    }

    /// The records' bytes (§2.6), which later builds read: written, read raw and read back.
    #[test]
    fn the_records_layout() {
        let db = db();
        assert_eq!(Meta::read(&db).unwrap(), Meta::default());
        let mut b = db.batch();
        Meta::put_tip(&mut b, &hash(1));
        Meta::put_head_blocks(&mut b, &hash(2), None);
        Meta::put_evo_tip(&mut b, &hash(3));
        Meta::put_pruned_files(&mut b, true);
        Meta::put_last_file(&mut b, 0x0102_0304);
        Meta::put_reindexing(&mut b, true);
        Meta::put_wiping(&mut b, true);
        db.write(b, Commit::Durable).unwrap();
        assert_eq!(raw(&db, b"tip"), Some(vec![1; 32]));
        assert_eq!(raw(&db, b"head_blocks"), Some([[2; 32], [0; 32]].concat()));
        assert_eq!(raw(&db, b"evo_tip"), Some(vec![3; 32]));
        assert_eq!(raw(&db, b"pruned_files"), Some(vec![1]));
        assert_eq!(raw(&db, b"last_file"), Some(vec![1, 2, 3, 4]));
        assert_eq!(raw(&db, b"reindexing"), Some(vec![1]));
        assert_eq!(raw(&db, b"wiping"), Some(vec![1]));
        let all = Meta {
            tip: Some(hash(1)),
            head_blocks: Some((hash(2), None)),
            evo_tip: Some(hash(3)),
            pruned_files: true,
            last_file: 0x0102_0304,
            reindexing: true,
            wiping: true,
        };
        assert_eq!(Meta::read(&db).unwrap(), all);
        // A non-null `old` head, and the flags cleared: `reindexing` and `wiping` are erased.
        let mut b = db.batch();
        Meta::put_head_blocks(&mut b, &hash(2), Some(&hash(4)));
        Meta::put_pruned_files(&mut b, false);
        Meta::put_reindexing(&mut b, false);
        Meta::put_wiping(&mut b, false);
        Meta::erase_tip(&mut b);
        db.write(b, Commit::Durable).unwrap();
        assert_eq!(raw(&db, b"head_blocks"), Some([[2; 32], [4; 32]].concat()));
        assert_eq!(raw(&db, b"pruned_files"), Some(vec![0]));
        assert_eq!(
            (raw(&db, b"reindexing"), raw(&db, b"wiping"), raw(&db, b"tip")),
            (None, None, None)
        );
        let read = Meta::read(&db).unwrap();
        let want = Meta {
            tip: None,
            head_blocks: Some((hash(2), Some(hash(4)))),
            pruned_files: false,
            reindexing: false,
            wiping: false,
            ..all
        };
        assert_eq!(read, want);
    }

    /// A record of another length, or a flag byte other than 0 or 1, is corruption: it trips the
    /// latch and is never read as absent (an empty `head_blocks` is Core's empty vector).
    #[test]
    fn a_malformed_record_is_corruption() {
        let cases: [(&[u8], &[u8]); 7] = [
            (TIP, &[1; 31]),
            (HEAD_BLOCKS, &[2; 65]),
            (EVO_TIP, &[]),
            (LAST_FILE, &[0; 5]),
            (REINDEXING, &[2]),
            (WIPING, &[1, 1]),
            (PRUNED_FILES, &[0, 1]),
        ];
        for (key, value) in cases {
            let db = db();
            let mut b = db.batch();
            b.put(DbSchema::META, key, value);
            db.write(b, Commit::Durable).unwrap();
            let e = Meta::read(&db).unwrap_err();
            assert_eq!(e.kind(), ErrorKind::Corruption, "{key:?}");
            assert!(db.latch().tripped().is_some(), "{key:?}");
        }
    }

    /// `head_blocks` read as Core reads its head-blocks vector (`validation.cpp:5229-5233`): no
    /// hashes is no marker; a whole number of them other than two is no marker and `true` at open,
    /// and corruption otherwise. Any other length is corruption, at open as well: the manager's
    /// ruling, where Core reads bytes that do not decode as consistent (quirk 44).
    #[test]
    fn the_marker_reads_as_cores_head_blocks_vector() {
        let read = |value: &[u8], at_open: bool| {
            let db = db();
            let mut b = db.batch();
            Meta::put_tip(&mut b, &hash(1));
            b.put(DbSchema::META, HEAD_BLOCKS, value);
            db.write(b, Commit::Durable).unwrap();
            let read = if at_open {
                Meta::read_at_open(&db)
            } else {
                Meta::read(&db).map(|m| (m, false))
            };
            let tripped = db.latch().tripped().is_some();
            (read.map_err(|e| e.kind()), tripped)
        };
        let tip = Meta {
            tip: Some(hash(1)),
            ..Meta::default()
        };
        for at_open in [false, true] {
            assert_eq!(read(&[], at_open), (Ok((tip, false)), false), "{at_open}");
            for len in [1, 63, 65, 95] {
                let want = (Err(ErrorKind::Corruption), true);
                assert_eq!(read(&vec![2; len], at_open), want, "{len} {at_open}");
            }
        }
        for len in [32, 96, 128] {
            assert_eq!(read(&vec![2; len], true), (Ok((tip, true)), false), "{len}");
            assert_eq!(read(&vec![2; len], false), (Err(ErrorKind::Corruption), true), "{len}");
        }
    }

    /// Keys at the bottom of `delete_all`'s range (the empty key and `0x00`), inside it, and runs
    /// of `0xff` toward its top.
    const FILL_KEYS: [&[u8]; 5] = [&[], &[0x00], b"k", &[0xff; 37], &[0xff; 40]];

    /// Every column family holds `FILL_KEYS`, and `meta` its records, `reindexing` as given.
    fn fill(db: &GuardedEngine<ProbeEngine>, reindexing: bool) {
        let mut b = db.batch();
        for spec in CHAINSTATE_SCHEMA.cfs.iter().skip(1) {
            for key in FILL_KEYS {
                b.put(cf(spec.name), key, b"v");
            }
        }
        Meta::put_tip(&mut b, &hash(1));
        Meta::put_evo_tip(&mut b, &hash(1));
        Meta::put_head_blocks(&mut b, &hash(2), Some(&hash(1)));
        Meta::put_last_file(&mut b, 7);
        Meta::put_pruned_files(&mut b, true);
        Meta::put_reindexing(&mut b, reindexing);
        db.write(b, Commit::Durable).unwrap();
    }

    fn keys(db: &GuardedEngine<ProbeEngine>, name: &str) -> usize {
        db.inner().unprobed().iter_prefix(cf(name), &[]).count()
    }

    /// `wipe_chainstate`'s crash-resume (§7.1 a3). A crash at its first write leaves the old state;
    /// at its second or third, `wiping` is set and the tips are gone, and the next start wipes
    /// again. Either way the wipe completes: every column family but `meta`, `block_index` and
    /// `block_files` is empty, those two and the block files' records are kept, and the marker is
    /// clear. A `-reindex`'s `reindexing` flag stays as it was throughout.
    #[test]
    fn a_crashed_wipe_resumes() {
        for (crash, reindexing) in [None, Some(1), Some(2), Some(3)]
            .into_iter()
            .flat_map(|crash| [false, true].map(|reindexing| (crash, reindexing)))
        {
            let case = format!("{crash:?}, reindexing {reindexing}");
            let db = db();
            fill(&db, reindexing);
            if let Some(n) = crash {
                db.inner().crash_at_write(n);
                assert!(wipe_chainstate(&db).is_err(), "{case}");
                assert!(db.latch().tripped().is_some());
            } else {
                wipe_chainstate(&db).unwrap();
            }
            let sink = Arc::new(RecordingSink::default());
            let db = GuardedEngine::new(db.inner().restart(), GlobalFailStop::new(), sink);
            let meta = Meta::read(&db).unwrap();
            assert_eq!(meta.wiping, crash.is_some_and(|n| n > 1), "{case}");
            assert_eq!(meta.reindexing, reindexing, "{case}");
            assert_eq!(meta.tip, (crash == Some(1)).then(|| hash(1)), "{case}");
            if crash.is_some() {
                // The start resumes the wipe; after a crash at the first write it is -reindex-
                // chainstate's own start that wipes.
                wipe_chainstate(&db).unwrap();
            }
            for spec in CHAINSTATE_SCHEMA.cfs {
                let want = match spec.name {
                    "meta" => continue,
                    "block_index" | "block_files" => FILL_KEYS.len(),
                    _ => 0,
                };
                assert_eq!(keys(&db, spec.name), want, "{}, {case}", spec.name);
            }
            let meta = Meta::read(&db).unwrap();
            let want = Meta {
                last_file: 7,
                pruned_files: true,
                reindexing,
                ..Meta::default()
            };
            assert_eq!(meta, want, "{case}");
        }
    }
}
