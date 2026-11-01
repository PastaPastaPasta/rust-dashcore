//! The engine trait (A1 §2.8).
//!
//! The trait exposes storage primitives only: an atomic multi-column-family batch, a buffered or
//! durable commit, explicit durability barriers and snapshot reads. It deliberately encodes no
//! ordering between the database and anything else (block and undo files, other databases): the
//! chainstate flush protocol composes these primitives, and it owns the ordering (Core's is: block
//! and undo files written and fsynced, directory committed, then block index, then coins).

use crate::error::Result;
use crate::schema::DbSchema;

/// A column family of a database: an index into its [`DbSchema::cfs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cf(u16);

impl Cf {
    pub const fn new(index: u16) -> Self {
        Self(index)
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// How [`KvEngine::write`] commits a batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Commit {
    /// Atomic and visible to readers on return, and it survives a process crash once the call
    /// returns. A power loss can lose it (and any later buffered batch) until the next barrier.
    Buffered,
    /// As `Buffered`, and durable on return: this batch and every batch committed before it
    /// survive a power loss.
    Durable,
}

/// Read access: implemented by engines and by their snapshots.
pub trait KvRead {
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>>;

    /// Batched point reads; the results follow the order of `keys`. This is the prefetch path
    /// (A1 §3.2 S2); the RocksDB engine maps it to `MultiGet`.
    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>>;

    /// All entries whose key starts with `prefix`, in key order. An empty prefix scans the whole
    /// column family. A live iterator is bound by [`KvEngine::snapshot`]'s rule, as a snapshot.
    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a;

    /// [`iter_prefix`](Self::iter_prefix) for a scan read once, as Core's iterators read with
    /// `fill_cache = false` (`dbwrapper.cpp:136`): an engine with a block cache leaves it as it
    /// was, so a long scan does not take the cache from the reads after it. Without one, the
    /// default, it is `iter_prefix`. A wrapper forwards it to its inner engine.
    fn iter_prefix_uncached<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        self.iter_prefix(cf, prefix)
    }

    /// The entries of [`iter_prefix`](Self::iter_prefix) whose key is at least `start`: a seek,
    /// as Core's `CDBIterator::Seek` (`dbwrapper.h:172`). An engine with ordered storage seeks to
    /// `start`; the default skips the keys below it.
    fn iter_prefix_from<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
        start: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        let start = start.to_vec();
        self.iter_prefix(cf, prefix)
            .skip_while(move |r| r.as_ref().is_ok_and(|(key, _)| *key < start))
    }
}

/// A write batch, applied atomically across all column families of one database.
///
/// A batch holds its operations in memory until it is committed. [`size_bytes`](Self::size_bytes)
/// is a lower bound on that memory: a batch that grows by appending may hold up to about twice
/// its size in buffer capacity, and briefly three times while it reallocates. Flush budgets that
/// must bound the real memory create their batches with [`KvEngine::batch_with_capacity`] sized
/// for the largest batch (e.g. `-dbbatchsize` plus one record).
pub trait KvBatch: Send {
    fn put(&mut self, cf: Cf, key: &[u8], value: &[u8]);
    fn delete(&mut self, cf: Cf, key: &[u8]);
    /// Deletes every key `k` with `start <= k < end`, without reading them (BL-38): a range
    /// tombstone on RocksDB, whose cost does not depend on the range's width. It applies in batch
    /// order: a put of a key in the range earlier in the batch is deleted, a later one is kept. A
    /// range with `start >= end` deletes nothing and records nothing.
    fn delete_range(&mut self, cf: Cf, start: &[u8], end: &[u8]);
    /// Number of operations in the batch.
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The encoded size of the batch in bytes, compared against `-dbbatchsize`. A lower bound on
    /// the batch's memory (see the type's documentation).
    fn size_bytes(&self) -> usize;
    fn clear(&mut self);
}

/// A key-value storage engine holding the column families of one [`DbSchema`].
///
/// **Errors: fail-stop.** An error from [`write`](Self::write), [`sync_wal`](Self::sync_wal) or
/// [`flush_memtables`](Self::flush_memtables) means the outcome of that call is unknown, and so
/// is the durability of buffered batches since the last successful barrier. After the first such
/// error, or a failed background flush or compaction, the engine refuses every write and barrier
/// that starts after the failing call has returned, with
/// [`ErrorKind::ReadOnly`](crate::ErrorKind::ReadOnly), until it is reopened, even once the cause
/// is gone: no batch can be committed after one whose outcome is unknown (A1 §3.5.6). (A write
/// running concurrently with the failing call has no order relative to it.) The engine enforces
/// this itself; RocksDB alone would resume writes after `ENOSPC` once space is freed. Reads keep
/// working. Callers treat the first error as fatal, as Core's `AbortNode` does;
/// [`StoreError::kind`](crate::StoreError::kind) of that first error says whether the disk was
/// full (a quota counts as full; for a background failure RocksDB did not stop for, the kind is
/// read from RocksDB's LOG, `Io` if it is not there), and the refusals' messages repeat it.
pub trait KvEngine: KvRead + Send + Sync {
    type Batch: KvBatch + 'static;
    type Snapshot<'a>: KvRead
    where
        Self: 'a;

    fn schema(&self) -> &'static DbSchema;
    fn batch(&self) -> Self::Batch;
    /// A batch whose buffer is preallocated for `bytes` of [`KvBatch::size_bytes`], so it does
    /// not reallocate (and transiently double) while it grows up to that size.
    fn batch_with_capacity(&self, bytes: usize) -> Self::Batch {
        let _ = bytes;
        self.batch()
    }
    /// Commits `batch` atomically: after a crash or power loss either all of it or none of it is
    /// present. Batches become durable in commit order.
    fn write(&self, batch: Self::Batch, commit: Commit) -> Result<()>;
    /// Commits `batch` [`Commit::Durable`] as a forced barrier: when it returns, `batch` and every
    /// batch committed before it are durable, even when nothing of `batch` changes a stored
    /// value and the engine tracks no pending buffered commit (an engine reopened after a
    /// process kill can still hold buffered commits in the OS page cache: SB14 D17). Engines
    /// whose Durable commit always syncs keep the default.
    fn write_barrier(&self, batch: Self::Batch) -> Result<()> {
        self.write(batch, Commit::Durable)
    }
    /// Barrier: every batch committed before this call is durable when it returns. Equivalent to
    /// committing an empty batch with [`Commit::Durable`].
    fn sync_wal(&self) -> Result<()>;
    /// Barrier: moves buffered data into the engine's table files and syncs them, so recovery no
    /// longer needs the log for it. Not needed for durability (use [`sync_wal`](Self::sync_wal));
    /// it bounds log replay time and frees write-buffer memory.
    fn flush_memtables(&self) -> Result<()>;
    /// A consistent read view as of now; later writes are not visible through it. It need not be
    /// `Send` (SQLite's holds a pooled connection on its thread): take it and drop it inside one
    /// blocking call.
    ///
    /// While a thread holds a snapshot or a live iterator of an engine, it reads that engine only
    /// through it, never live: an engine may serve both from one bounded pool of connections
    /// (SQLite's), where such a live read waits for a connection the thread itself, or another
    /// thread doing the same, holds. SQLite fails that read at once when the thread holds every
    /// connection, and otherwise after a bounded wait (SB10 Q8). A thread that holds no
    /// connection only waits behind other threads' reads, however long, and never fails for it.
    fn snapshot(&self) -> Self::Snapshot<'_>;
    /// Estimated live data in `cf`, in bytes.
    fn approximate_size(&self, cf: Cf) -> Result<u64>;
}
