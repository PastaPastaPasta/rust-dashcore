use std::collections::BTreeSet;
use std::sync::Mutex;

use dash_kv::faulty::{CrashKind, Dfs, World, WorldConfig};
use dash_kv::{MemoryBatch, MemorySnapshot, Result as KvResult, SCHEMA_KEY};
use dash_llmq::dkg::bls::{BlsPkValue, BlsSchemeKind, BlsSkValue, secret_to_public};
use dash_llmq::quorum::{VvecReadError, from_store};

use super::*;

fn key(n: u8) -> QuorumDbKey {
    QuorumDbKey([n; 32])
}

fn sk(n: u8) -> BlsSkValue {
    let mut bytes = [0; 32];
    bytes[31] = n;
    BlsSkValue::from_canonical(&bytes, BlsSchemeKind::Basic).unwrap()
}

fn full(n: u8) -> StoredContributions {
    StoredContributions {
        vvec: Ok(Some(vec![secret_to_public(&sk(n)), secret_to_public(&sk(n + 1))])),
        sk_share: Some(sk(n)),
    }
}

fn vvec_only(n: u8) -> StoredContributions {
    StoredContributions {
        sk_share: None,
        ..full(n)
    }
}

fn sk_only(n: u8) -> StoredContributions {
    StoredContributions {
        vvec: Ok(None),
        sk_share: Some(sk(n)),
    }
}

/// A read's stored bytes: the vector's encoding or its read error, and the share's.
type Shape = (Result<Vec<u8>, VvecReadError>, Option<[u8; 32]>);

/// A planted row: what it is, the vector row's bytes, the share row's, and the read it gives.
type Planted<'a> = (&'a str, Vec<u8>, Option<&'a [u8]>, Shape);

/// A read compared by its stored bytes.
fn shape(read: Option<StoredContributions>) -> Option<Shape> {
    read.map(|c| {
        let vvec = c.vvec.map(|v| encode_vvec(&v.expect("a read vector is present")));
        (vvec, c.sk_share.as_ref().map(encode_sk))
    })
}

fn rows<E: KvEngine>(store: &KvQuorumStore<E>) -> Vec<(Vec<u8>, Vec<u8>)> {
    store.engine().iter_prefix(ROWS, &[]).map(Result::unwrap).collect()
}

/// A [`MemoryEngine`] that records each commit's kind and operation count, can fail a write, and
/// can fail a snapshot's scan of a prefix.
struct Counting {
    inner: MemoryEngine,
    commits: Mutex<Vec<(Commit, usize)>>,
    /// The writes left before one fails without applying anything.
    fail_in: Mutex<Option<usize>>,
    /// A snapshot's scan of this prefix yields an error after this many entries, then the rest.
    iter_fault: Mutex<Option<(Vec<u8>, usize)>>,
}

impl Counting {
    fn new() -> Self {
        Self {
            inner: MemoryEngine::new(&QUORUMDB_SCHEMA),
            commits: Mutex::default(),
            fail_in: Mutex::default(),
            iter_fault: Mutex::default(),
        }
    }

    fn take(&self) -> Vec<(Commit, usize)> {
        std::mem::take(&mut self.commits.lock().unwrap())
    }
}

impl KvRead for Counting {
    fn get(&self, cf: Cf, key: &[u8]) -> KvResult<Option<Vec<u8>>> {
        self.inner.get(cf, key)
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> KvResult<Vec<Option<Vec<u8>>>> {
        self.inner.multi_get(cf, keys)
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = KvResult<(Vec<u8>, Vec<u8>)>> + 'a {
        self.inner.iter_prefix(cf, prefix)
    }
}

/// [`Counting`]'s snapshot, with its scan fault.
struct CountingSnapshot {
    inner: MemorySnapshot,
    iter_fault: Option<(Vec<u8>, usize)>,
}

impl KvRead for CountingSnapshot {
    fn get(&self, cf: Cf, key: &[u8]) -> KvResult<Option<Vec<u8>>> {
        self.inner.get(cf, key)
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> KvResult<Vec<Option<Vec<u8>>>> {
        self.inner.multi_get(cf, keys)
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = KvResult<(Vec<u8>, Vec<u8>)>> + 'a {
        let mut rows: Vec<_> = self.inner.iter_prefix(cf, prefix).collect();
        if let Some((_, after)) = self.iter_fault.as_ref().filter(|(p, _)| p == prefix) {
            let error = StoreError::engine("counting", dash_kv::ErrorKind::Io, "injected scan");
            rows.insert(*after, Err(error));
        }
        rows.into_iter()
    }
}

impl KvEngine for Counting {
    type Batch = MemoryBatch;
    type Snapshot<'a> = CountingSnapshot;

    fn schema(&self) -> &'static DbSchema {
        self.inner.schema()
    }

    fn batch(&self) -> MemoryBatch {
        self.inner.batch()
    }

    fn write(&self, batch: MemoryBatch, commit: Commit) -> KvResult<()> {
        if let Some(left) = self.fail_in.lock().unwrap().as_mut() {
            if *left == 0 {
                return Err(StoreError::engine("counting", dash_kv::ErrorKind::Io, "injected"));
            }
            *left -= 1;
        }
        self.commits.lock().unwrap().push((commit, batch.len()));
        self.inner.write(batch, commit)
    }

    fn sync_wal(&self) -> KvResult<()> {
        self.inner.sync_wal()
    }

    fn flush_memtables(&self) -> KvResult<()> {
        self.inner.flush_memtables()
    }

    fn snapshot(&self) -> CountingSnapshot {
        CountingSnapshot {
            inner: self.inner.snapshot(),
            iter_fault: self.iter_fault.lock().unwrap().clone(),
        }
    }

    fn approximate_size(&self, cf: Cf) -> KvResult<u64> {
        self.inner.approximate_size(cf)
    }
}

/// T1: the rows are Core's `CDBWrapper` bytes: the pair's string as a CompactSize length and its
/// characters, then the 32-byte key; the vector as a CompactSize count and 48-byte basic-scheme
/// keys; the share as 32 bytes. Nothing else is in `rows`.
// Core: v24.0.0 src/llmq/quorums.cpp:17-18, :192-208; src/serialize.h (std::string, std::pair)
#[test]
fn rows_carry_cores_key_and_value_bytes() {
    let store = KvQuorumStore::in_memory();
    let k = QuorumDbKey(std::array::from_fn(|i| i as u8));
    store.write(&k, &full(1)).unwrap();
    let vvec_key = [&b"\x08q_Qqvvec"[..], &k.0].concat();
    let sk_key = [&b"\x05q_Qsk"[..], &k.0].concat();
    assert_eq!(vvec_row_key(&k).as_slice(), vvec_key);
    assert_eq!(sk_row_key(&k).as_slice(), sk_key);
    let pks = [secret_to_public(&sk(1)), secret_to_public(&sk(2))];
    let mut vvec = vec![2];
    for pk in &pks {
        vvec.extend_from_slice(&pk.to_bytes(BlsSchemeKind::Basic));
    }
    let mut share = [0; 32];
    share[31] = 1;
    assert_eq!(rows(&store), [(sk_key, share.to_vec()), (vvec_key, vvec)]);
}

/// T2: every row shape reads as dash-llmq's `MemQuorumStore` reads it, and loads as it loads: the
/// shapes a write makes against that store, and planted rows against the decoders' outcomes
/// (DEC-1646's read errors, the invalid key for legacy and all-zero elements, no share for a
/// malformed share row).
#[test]
fn reads_match_the_memory_store_for_every_row_shape() {
    let written = [
        ("vvec only", vvec_only(1)),
        ("vvec and share", full(3)),
        (
            "invalid share not written",
            StoredContributions {
                sk_share: Some(BlsSkValue::invalid()),
                ..full(5)
            },
        ),
        ("share without vvec", sk_only(7)),
        (
            "vvec of invalid keys",
            StoredContributions {
                vvec: Ok(Some(vec![BlsPkValue::invalid()])),
                sk_share: None,
            },
        ),
    ];
    let ours = KvQuorumStore::in_memory();
    let theirs = MemQuorumStore::new();
    assert!(shape(ours.read(&key(0)).unwrap()).is_none(), "absent");
    for (n, (what, c)) in written.iter().enumerate() {
        let k = key(n as u8 + 1);
        ours.write(&k, c).unwrap();
        theirs.write(&k, c).unwrap();
        let (a, b) = (ours.read(&k).unwrap(), theirs.read(&k).unwrap());
        assert_eq!(shape(a.clone()), shape(b.clone()), "{what}");
        let loaded = |r: Option<StoredContributions>| {
            r.map(from_store).map(|l| l.map(|k| k.map(|k| k.sk_share().is_some())))
        };
        assert_eq!(loaded(a), loaded(b), "{what}");
    }

    let legacy = secret_to_public(&sk(9)).to_bytes(BlsSchemeKind::Legacy);
    assert!(BlsPkValue::from_canonical(&legacy, BlsSchemeKind::Basic).is_none());
    let valid = secret_to_public(&sk(9)).to_bytes(BlsSchemeKind::Basic);
    let invalid = BlsPkValue::invalid().to_bytes(BlsSchemeKind::Basic);
    let one = |element: &[u8]| [&[1][..], element].concat();
    let planted: [Planted<'_>; 6] = [
        ("legacy-only element", one(&legacy), None, (Ok(one(&invalid)), None)),
        ("all-zero element", one(&[0; 48]), None, (Ok(one(&invalid)), None)),
        (
            "undecodable element",
            one(&[0xff; 48]),
            None,
            (
                Err(VvecReadError::UndecodableElement {
                    index: 0,
                }),
                None,
            ),
        ),
        (
            "truncated row",
            [&[2][..], &valid].concat(),
            None,
            (Err(VvecReadError::TruncatedRow), None),
        ),
        ("bad CompactSize", vec![0xfd, 1, 0], None, (Err(VvecReadError::MalformedCount), None)),
        ("malformed share", one(&valid), Some(&[0xff; 32]), (Ok(one(&valid)), None)),
    ];
    for (n, (what, vvec, share, want)) in planted.into_iter().enumerate() {
        let k = key(100 + n as u8);
        let mut batch = ours.engine().batch();
        batch.put(ROWS, &vvec_row_key(&k), &vvec);
        if let Some(share) = share {
            batch.put(ROWS, &sk_row_key(&k), share);
        }
        ours.engine().write(batch, Commit::Buffered).unwrap();
        let read = ours.read(&k).unwrap();
        let read_error = want.0.is_err();
        assert_eq!(shape(read.clone()), Some(want), "{what}");
        assert_eq!(from_store(read.unwrap()).is_err(), read_error, "{what}");
    }
}

/// T3: the cleanup erases the rows of every key it does not keep, in both prefixes, and counts
/// them as `MemQuorumStore` does; kept rows, a row under another prefix and the schema record
/// stay.
// Core: v24.0.0 src/llmq/quorums.cpp:31-76 DataCleanupHelper
#[test]
fn cleanup_erases_exactly_the_rows_not_kept() {
    let ours = KvQuorumStore::in_memory();
    let theirs = MemQuorumStore::new();
    let (a, b, c, d) = (key(0xa), key(0xb), key(0xc), key(0xd));
    for (k, contributions) in [(a, full(1)), (b, vvec_only(3)), (c, sk_only(5)), (d, full(7))] {
        ours.write(&k, &contributions).unwrap();
        theirs.write(&k, &contributions).unwrap();
    }
    let foreign = [&b"\x03q_Q"[..], &[0xb; 32]].concat();
    let mut batch = ours.engine().batch();
    batch.put(ROWS, &foreign, b"other");
    ours.engine().write(batch, Commit::Buffered).unwrap();
    let keep = BTreeSet::from([a, d]);
    let counts = ours.erase_except(&keep).unwrap();
    assert_eq!(
        counts,
        CleanupCounts {
            vvec: 1,
            sk_share: 1,
        }
    );
    assert_eq!(counts, theirs.erase_except(&keep).unwrap());
    for k in [a, d] {
        assert_eq!(shape(ours.read(&k).unwrap()), shape(theirs.read(&k).unwrap()));
        assert!(shape(ours.read(&k).unwrap()).is_some_and(|(_, sk)| sk.is_some()));
    }
    for k in [b, c] {
        assert!(ours.read(&k).unwrap().is_none());
        assert_eq!(ours.engine().get(ROWS, &sk_row_key(&k)).unwrap(), None);
    }
    assert_eq!(ours.engine().get(ROWS, &foreign).unwrap(), Some(b"other".to_vec()));
    assert!(ours.engine().get(DbSchema::META, SCHEMA_KEY).unwrap().is_some());
    assert_eq!(rows(&ours).len(), 5);
}

/// T4: a key under a prefix too short for a database key ends that prefix's scan, Core's failed
/// `GetKey`; the other prefix is still cleaned. A short key below Core's seek, the prefix and
/// all-zero bytes, is never reached. A key with trailing bytes counts, and the erase names the
/// key Core re-serializes from the pair, so the longer row stays.
// Core: v24.0.0 src/llmq/quorums.cpp:45-54
#[test]
fn cleanup_stops_a_prefix_at_a_short_key() {
    let store = KvQuorumStore::in_memory();
    for n in [0, 2] {
        store.write(&key(n), &full(n + 1)).unwrap();
    }
    let short = [&b"\x08q_Qqvvec"[..], &[1; 11]].concat();
    assert_eq!(short.len(), 20);
    let below = [&b"\x08q_Qqvvec"[..], &[0; 5]].concat();
    let mut batch = store.engine().batch();
    batch.put(ROWS, &short, b"short");
    batch.put(ROWS, b"\x08q_Qqvvec", b"bare");
    batch.put(ROWS, &below, b"below");
    store.engine().write(batch, Commit::Buffered).unwrap();
    let counts = store.erase_except(&BTreeSet::new()).unwrap();
    assert_eq!(
        counts,
        CleanupCounts {
            vvec: 1,
            sk_share: 2,
        }
    );
    assert!(store.read(&key(0)).unwrap().is_none());
    assert_eq!(shape(store.read(&key(2)).unwrap()), shape(Some(vvec_only(3))));
    assert_eq!(store.engine().get(ROWS, &short).unwrap(), Some(b"short".to_vec()));
    assert_eq!(store.engine().get(ROWS, &below).unwrap(), Some(b"below".to_vec()));

    let store = KvQuorumStore::in_memory();
    let long = [&vvec_row_key(&key(4))[..], &[9; 3]].concat();
    let mut batch = store.engine().batch();
    batch.put(ROWS, &long, b"long");
    store.engine().write(batch, Commit::Buffered).unwrap();
    let counts = store.erase_except(&BTreeSet::new()).unwrap();
    assert_eq!(counts.vvec, 1, "the long key counts");
    assert_eq!(store.engine().get(ROWS, &long).unwrap(), Some(b"long".to_vec()));
    let counts = store.erase_except(&BTreeSet::from([key(4)])).unwrap();
    assert_eq!(counts.vvec, 0, "its decoded key is kept");
}

/// T5: the cleanup writes its batch when the size estimate reaches the limit, and at the end of
/// each prefix without clearing it, all buffered: five erasable vectors at a two-erase limit are
/// commits of 2, 2 and 1 erases, then the share prefix's write repeats the uncleared one.
// Core: v24.0.0 src/llmq/quorums.cpp:36-62; src/dbwrapper.h:94-98, :138-147
#[test]
fn cleanup_flushes_at_the_batch_limit() {
    let store = KvQuorumStore::new(Counting::new());
    for n in 0..5 {
        store.write(&key(n), &vvec_only(n + 1)).unwrap();
    }
    store.engine().take();
    let erase = 2 + vvec_row_key(&key(0)).len();
    let counts = store.cleanup(&BTreeSet::new(), 2 * erase).unwrap();
    assert_eq!(
        counts,
        CleanupCounts {
            vvec: 5,
            sk_share: 0,
        }
    );
    let buffered = |n| (Commit::Buffered, n);
    assert_eq!(store.engine().take(), [buffered(2), buffered(2), buffered(1), buffered(1)]);
    assert!(rows(&store).is_empty());

    store.erase_except(&BTreeSet::new()).unwrap();
    assert_eq!(store.engine().take(), [buffered(0), buffered(0)], "one write per prefix");
}

/// T5c: an iterator error ends that prefix's scan, as Core's `pcursor->Valid()` turns false on
/// one: the erases before it are written, the rows after it stay, the other prefix is scanned
/// from its own seek, and the cleanup succeeds, as Core never reads the iterator's status.
// Core: v24.0.0 src/llmq/quorums.cpp:36-62
#[test]
fn cleanup_ends_a_prefix_at_an_iterator_error() {
    for (failing, after) in [(DB_QUORUM_VVEC, 2), (DB_QUORUM_SK_SHARE, 0)] {
        let store = KvQuorumStore::new(Counting::new());
        for n in 1..=4 {
            store.write(&key(n), &full(n)).unwrap();
        }
        *store.engine().iter_fault.lock().unwrap() = Some((prefix_bytes(failing), after));
        let counts = store.erase_except(&BTreeSet::new()).unwrap();
        let (vvec, sk_share) = if failing == DB_QUORUM_VVEC {
            (2, 4)
        } else {
            (4, 0)
        };
        assert_eq!(
            counts,
            CleanupCounts {
                vvec,
                sk_share,
            },
            "{failing}"
        );
        for n in 1..=4 {
            let kept = |count| usize::from(n) > count;
            let vvec_row = store.engine().get(ROWS, &vvec_row_key(&key(n))).unwrap();
            let sk_row = store.engine().get(ROWS, &sk_row_key(&key(n))).unwrap();
            assert_eq!(vvec_row.is_some(), kept(vvec), "{failing} {n}");
            assert_eq!(sk_row.is_some(), kept(sk_share), "{failing} {n}");
        }
    }
}

/// T5b: a write is Core's two `db.Write` calls, each a buffered commit of one row: the vector,
/// then the share, so a failed second write leaves the vector alone and is the call's error.
// Core: v24.0.0 src/llmq/quorums.cpp:192-208; src/dbwrapper.h:331-336
#[test]
fn a_write_is_two_buffered_commits() {
    let store = KvQuorumStore::new(Counting::new());
    store.write(&key(1), &full(1)).unwrap();
    assert_eq!(store.engine().take(), [(Commit::Buffered, 1), (Commit::Buffered, 1)]);
    store.write(&key(2), &vvec_only(1)).unwrap();
    store.write(&key(3), &sk_only(1)).unwrap();
    assert_eq!(store.engine().take(), [(Commit::Buffered, 1), (Commit::Buffered, 1)]);

    *store.engine().fail_in.lock().unwrap() = Some(1);
    assert!(store.write(&key(4), &full(4)).is_err());
    assert_eq!(shape(store.read(&key(4)).unwrap()), shape(Some(vvec_only(4))));
    assert_eq!(store.engine().get(ROWS, &sk_row_key(&key(4))).unwrap(), None);
}

/// T6: whatever subset of the buffered commits a power loss keeps, each atomically (ST3a's subset
/// model, stricter than RocksDB's prefix), every read is `Ok`: the full row, the vector alone (a
/// share needs its vector to be read), or not found. A process kill keeps every commit.
#[test]
fn a_crash_after_any_commit_leaves_a_readable_store() {
    let open = |w: &World| KvQuorumStore::new((*w.engine("quorumdb", &QUORUMDB_SCHEMA)).clone());
    let world = World::new(WorldConfig::default());
    let store = open(&world);
    store.write(&key(1), &full(1)).unwrap();
    store.write(&key(2), &full(3)).unwrap();
    store.erase_except(&BTreeSet::from([key(1)])).unwrap();
    let image = world.capture();

    let allowed = |n: u8| [None, shape(Some(vvec_only(n))), shape(Some(full(n)))];
    let dfs = Dfs::new();
    let mut seen = BTreeSet::new();
    loop {
        let booted = image.boot(CrashKind::PowerLoss, dfs.decider(), WorldConfig::default());
        let store = open(&booted);
        let reads = [(1, key(1)), (3, key(2))].map(|(n, k)| {
            let read = shape(store.read(&k).expect("every read is Ok"));
            assert!(allowed(n).contains(&read), "{read:?}");
            read
        });
        seen.insert(format!("{reads:?}"));
        assert_eq!(dfs.mismatch(), None);
        if !dfs.advance() {
            break;
        }
    }
    assert_eq!(seen.len(), 9, "each key absent, vector only or full");

    let killed = image.boot(CrashKind::ProcessKill, Dfs::new().decider(), WorldConfig::default());
    let store = open(&killed);
    assert_eq!(shape(store.read(&key(1)).unwrap()), shape(Some(full(1))));
    assert!(store.read(&key(2)).unwrap().is_none());
}
