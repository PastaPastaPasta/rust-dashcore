use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dash_kv::{
    AbortMark, Cf, Commit, DbSchema, ErrorKind, GlobalFailStop, KvBatch, KvEngine, KvRead,
    MemoryBatch, MemoryEngine, MemorySnapshot, RecordingSink, Result as KvResult, StoreError,
};
use dash_llmq::dkg::bls::secret_to_public;
use dash_params::{ChainParamsBuilder, Network, Release};

use super::*;

const T: LlmqType = LlmqType::LlmqTest;
const T2: LlmqType = LlmqType::LlmqTestDip0024;

fn setup() -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap())
}

/// 32 bytes that differ from their reverse.
fn bytes(n: u8) -> [u8; 32] {
    std::array::from_fn(|i| n ^ i as u8)
}

fn hash(n: u8) -> BlockHash {
    BlockHash::from(bytes(n))
}

fn pro_tx(n: u8) -> ProTxHash {
    ProTxHash::from(bytes(0xa0 + n))
}

fn sk(seed: u8) -> BlsSkValue {
    BlsSkValue::from_canonical(&[seed; 32], BlsSchemeKind::Basic).unwrap()
}

fn pk(seed: u8) -> BlsPkValue {
    secret_to_public(&sk(seed))
}

fn blobs(ephemeral: u8, n: u8) -> BlsIesMultiBlobs {
    BlsIesMultiBlobs {
        ephemeral: pk(ephemeral),
        iv_seed: [ephemeral; 32],
        blobs: (0..n).map(|i| vec![ephemeral, i, 0xee]).collect(),
    }
}

/// The chain's reads, counted. `active` is `None` for no chain.
#[derive(Default)]
struct TestChain {
    members: Mutex<BTreeMap<(u8, BlockHash), Vec<ProTxHash>>>,
    heights: Mutex<BTreeMap<BlockHash, u32>>,
    active: Mutex<Option<i32>>,
    member_calls: AtomicUsize,
    chain_reads: AtomicUsize,
}

impl TestChain {
    fn with_members(self, t: LlmqType, base: BlockHash, members: Vec<ProTxHash>) -> Self {
        self.members.lock().unwrap().insert((t.to_base(), base), members);
        self
    }
}

impl DkgChain for TestChain {
    fn members(
        &self,
        t: LlmqType,
        base: &BlockHash,
    ) -> Result<Vec<ProTxHash>, dash_dml::StoreError> {
        self.member_calls.fetch_add(1, Ordering::SeqCst);
        let members = self.members.lock().unwrap();
        members
            .get(&(t.to_base(), *base))
            .cloned()
            .ok_or(dash_dml::StoreError::NotBuilt("no members in the test chain"))
    }

    fn height(&self, hash: &BlockHash) -> Option<u32> {
        self.chain_reads.fetch_add(1, Ordering::SeqCst);
        self.heights.lock().unwrap().get(hash).copied()
    }

    fn active_height(&self) -> Option<i32> {
        self.chain_reads.fetch_add(1, Ordering::SeqCst);
        *self.active.lock().unwrap()
    }
}

/// A memory engine whose reads or writes fail on demand.
struct Shared {
    inner: MemoryEngine,
    fail_reads: AtomicBool,
    fail_writes: AtomicBool,
}

fn check(fail: &AtomicBool) -> KvResult<()> {
    if fail.load(Ordering::SeqCst) {
        return Err(StoreError::engine("test", ErrorKind::Io, "injected failure"));
    }
    Ok(())
}

impl Shared {
    fn check(&self) -> KvResult<()> {
        check(&self.fail_reads)
    }

    fn rows(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.inner.iter_prefix(ROWS, &[]).map(|r| r.unwrap()).collect()
    }

    fn raw_put(&self, key: &[u8], value: &[u8]) {
        let mut b = self.inner.batch();
        b.put(ROWS, key, value);
        self.inner.write(b, Commit::Durable).unwrap();
    }

    fn raw_delete(&self, key: &[u8]) {
        let mut b = self.inner.batch();
        b.delete(ROWS, key);
        self.inner.write(b, Commit::Durable).unwrap();
    }
}

impl KvRead for Shared {
    fn get(&self, cf: Cf, key: &[u8]) -> KvResult<Option<Vec<u8>>> {
        self.check()?;
        self.inner.get(cf, key)
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> KvResult<Vec<Option<Vec<u8>>>> {
        self.check()?;
        self.inner.multi_get(cf, keys)
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = KvResult<(Vec<u8>, Vec<u8>)>> + 'a {
        let failed = self.check().err().map(Err);
        failed.into_iter().chain(self.inner.iter_prefix(cf, prefix))
    }
}

impl KvEngine for Shared {
    type Batch = MemoryBatch;
    type Snapshot<'a> = MemorySnapshot;

    fn schema(&self) -> &'static DbSchema {
        self.inner.schema()
    }

    fn batch(&self) -> MemoryBatch {
        self.inner.batch()
    }

    fn write(&self, batch: MemoryBatch, commit: Commit) -> KvResult<()> {
        check(&self.fail_writes)?;
        self.inner.write(batch, commit)
    }

    fn sync_wal(&self) -> KvResult<()> {
        self.inner.sync_wal()
    }

    fn flush_memtables(&self) -> KvResult<()> {
        self.inner.flush_memtables()
    }

    fn snapshot(&self) -> MemorySnapshot {
        self.inner.snapshot()
    }

    fn approximate_size(&self, cf: Cf) -> KvResult<u64> {
        self.inner.approximate_size(cf)
    }
}

struct Fixture {
    store: DkgDb<Shared, TestChain>,
    sink: Arc<RecordingSink>,
}

impl Fixture {
    fn engine(&self) -> &Shared {
        self.store.db.inner()
    }

    fn chain(&self) -> &TestChain {
        &self.store.chain
    }
}

fn fixture(chain: TestChain) -> Fixture {
    let engine = Shared {
        inner: MemoryEngine::new(&DKGDB_SCHEMA),
        fail_reads: AtomicBool::new(false),
        fail_writes: AtomicBool::new(false),
    };
    let sink = Arc::new(RecordingSink::default());
    let store =
        DkgDb::open(engine, false, setup(), chain, GlobalFailStop::new(), sink.clone()).unwrap();
    Fixture {
        store,
        sink,
    }
}

/// Three members at base 1 under [`T`].
fn three_members() -> TestChain {
    TestChain::default().with_members(T, hash(1), members())
}

fn members() -> Vec<ProTxHash> {
    vec![pro_tx(0), pro_tx(1), pro_tx(2)]
}

/// Core's tuple key, built by hand: `06 "qdkg_X"`, the type byte (100, [`T`]), base 1 and
/// `member`'s proTxHash.
fn core_key(kind: u8, member: u8) -> Vec<u8> {
    let mut key = vec![6, b'q', b'd', b'k', b'g', b'_', kind, 100];
    key.extend_from_slice(&bytes(1));
    key.extend_from_slice(&bytes(0xa0 + member));
    key
}

// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:125-143; develop src/bls/bls_ies.h:82
#[test]
fn each_row_kind_is_cores_key_and_value_bytes() {
    let mut f = fixture(three_members());
    let (b, p) = (QuorumHash::from(bytes(1)), pro_tx(0));
    f.store.write_vvec(T, &b, &p, &[pk(1), pk(2)]);
    f.store.write_sk(T, &b, &p, &sk(3));
    f.store.write_encrypted(T, &b, &p, &blobs(4, 2));

    let mut vvec = vec![2];
    vvec.extend_from_slice(&pk(1).to_bytes(BlsSchemeKind::Basic));
    vvec.extend_from_slice(&pk(2).to_bytes(BlsSchemeKind::Basic));
    let sk_row = [3u8; 32].to_vec();
    let mut enc = pk(4).to_bytes(BlsSchemeKind::Basic).to_vec();
    enc.extend_from_slice(&[4; 32]);
    enc.extend_from_slice(&[2, 3, 4, 0, 0xee, 3, 4, 1, 0xee]);
    let mut expected =
        vec![(core_key(b'E', 0), enc), (core_key(b'S', 0), sk_row), (core_key(b'V', 0), vvec)];
    expected.sort();
    assert_eq!(f.engine().rows(), expected);
}

// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:164-181, :216-221
#[test]
fn hand_built_core_rows_read_back() {
    let f = fixture(three_members());
    let all = [true, true, true];
    for i in 0..3 {
        let mut vvec = vec![2];
        vvec.extend_from_slice(&pk(10 + i).to_bytes(BlsSchemeKind::Basic));
        vvec.extend_from_slice(&[0; 48]);
        f.engine().raw_put(&core_key(b'V', i), &vvec);
        f.engine().raw_put(&core_key(b'S', i), &[20 + i; 32]);
        let mut enc = pk(30 + i).to_bytes(BlsSchemeKind::Basic).to_vec();
        enc.extend_from_slice(&[i; 32]);
        enc.extend_from_slice(&[3, 1, 0x10 + i, 1, 0x20 + i, 1, 0x30 + i]);
        f.engine().raw_put(&core_key(b'E', i), &enc);
    }
    let (indexes, vvecs, sks) = f.store.contributions(T, &hash(1), &all).unwrap().unwrap();
    assert_eq!(indexes, [0, 1, 2]);
    for i in 0..3u8 {
        assert_eq!(*vvecs[usize::from(i)], [pk(10 + i), BlsPkValue::invalid()]);
        assert_eq!(sks[usize::from(i)], sk(20 + i));
    }
    let enc = f.store.encrypted_contributions(T, &hash(1), &all, &pro_tx(1)).unwrap().unwrap();
    for (i, blob) in enc.iter().enumerate() {
        let i = i as u8;
        assert_eq!(blob.ephemeral, pk(30 + i));
        assert_eq!(blob.iv_seed, [i; 32]);
        assert_eq!(blob.data, [0x20 + i]);
    }
}

// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:171-178 (dash-llmq `DkgStore` contract)
#[test]
fn a_stored_vvec_key_reads_in_the_basic_encoding() {
    let legacy = (1..50)
        .map(|seed| pk(seed).to_bytes(BlsSchemeKind::Legacy))
        .find(|bytes| BlsPkValue::from_canonical(bytes, BlsSchemeKind::Basic).is_none())
        .expect("a key whose legacy bytes are not basic-canonical");
    let mut row = vec![4];
    row.extend_from_slice(&pk(6).to_bytes(BlsSchemeKind::Basic));
    row.extend_from_slice(&legacy);
    row.extend_from_slice(&[0; 48]);
    let mut neither = [0xff; 48];
    neither[0] = 0x1f;
    row.extend_from_slice(&neither);
    assert_eq!(
        decode_vvec(&row).unwrap(),
        [pk(6), BlsPkValue::invalid(), BlsPkValue::invalid(), BlsPkValue::invalid()],
        "legacy bytes, zeros and bytes that decode in neither scheme are the invalid key"
    );
    assert_eq!(decode_vvec(&row[..row.len() - 1]), None, "a short row throws");
    assert_eq!(decode_vvec(&[]), None);
}

// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:165-170, :180-181
#[test]
fn a_missing_vvec_fails_and_a_missing_sk_is_the_invalid_key() {
    let f = fixture(three_members());
    let (b, valid) = (hash(1), [true, false, true]);
    f.store.put_vvec(T, &b, &pro_tx(0), &[pk(1)]).unwrap();
    f.store.put_vvec(T, &b, &pro_tx(2), &[pk(3)]).unwrap();
    f.store.put_sk(T, &b, &pro_tx(0), &sk(1)).unwrap();

    let (indexes, vvecs, sks) = f.store.contributions(T, &b, &valid).unwrap().unwrap();
    assert_eq!(indexes, [0, 2]);
    assert_eq!((&*vvecs[0], &*vvecs[1]), (&vec![pk(1)], &vec![pk(3)]));
    assert_eq!(sks, [sk(1), BlsSkValue::invalid()], "member 2 has no sk row");

    let f = fixture(three_members());
    f.store.put_vvec(T, &b, &pro_tx(0), &[pk(1)]).unwrap();
    f.store.put_sk(T, &b, &pro_tx(2), &sk(3)).unwrap();
    let missing = Err(MissingVvec {
        who: pro_tx(2),
    });
    assert_eq!(f.store.contributions(T, &b, &valid).unwrap(), missing, "no vvec for 2");
    assert_eq!(f.store.verified(T, &b, &members(), &valid), None);
}

/// The quorum manager's build runs on V and passes the members it read there; the chain, whose
/// `ChainQuorums` members access waits on V, is never asked. This chain has no members, so an
/// access would fail the read.
// Core: v24.0.0 src/llmq/quorumsman.cpp:80, :120-122; src/llmq/dkgsessionmgr.cpp:147
#[test]
fn the_build_hook_reads_its_members_from_the_build_not_the_chain() {
    let f = fixture(TestChain::default());
    let (b, valid) = (hash(1), [true, false, true]);
    f.store.put_vvec(T, &b, &pro_tx(0), &[pk(1)]).unwrap();
    f.store.put_vvec(T, &b, &pro_tx(2), &[pk(3)]).unwrap();
    f.store.put_sk(T, &b, &pro_tx(0), &sk(1)).unwrap();
    f.store.put_sk(T, &b, &pro_tx(2), &sk(3)).unwrap();

    let (indexes, vvecs, sks) = f.store.verified(T, &b, &members(), &valid).unwrap();
    assert_eq!(indexes, [0, 2]);
    assert_eq!((&*vvecs[0], &*vvecs[1]), (&vec![pk(1)], &vec![pk(3)]));
    assert_eq!(sks, [sk(1), sk(3)]);
    assert_eq!(f.chain().member_calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.chain().chain_reads.load(Ordering::SeqCst), 0);
}

// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:180-181, src/dbwrapper.h:315-327
#[test]
fn an_sk_row_that_does_not_read_is_the_invalid_key() {
    assert_eq!(decode_sk(&[7; 32]), sk(7));
    assert_eq!(decode_sk(&[7; 33]), sk(7), "trailing bytes are not read");
    assert_eq!(decode_sk(&[7; 31]), BlsSkValue::invalid());
    assert_eq!(decode_sk(&[0; 32]), BlsSkValue::invalid());
    assert_eq!(decode_sk(&[0xff; 32]), BlsSkValue::invalid(), "not below the order");
    for bytes in [[7; 32], [0xff; 32], [0x73; 32]] {
        assert_eq!(
            BlsSkValue::from_canonical(&bytes, BlsSchemeKind::Basic),
            BlsSkValue::from_canonical(&bytes, BlsSchemeKind::Legacy),
        );
    }
}

// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:183, :227-238, src/llmq/dkgsessionmgr.h:53
#[test]
fn a_cache_entry_expires_after_sixty_seconds() {
    let t0 = Instant::now();
    let now = Arc::new(Mutex::new(t0));
    let clock = now.clone();
    let mut f = fixture(three_members());
    f.store = f.store.with_clock(move || *clock.lock().unwrap());
    let (b, valid) = (hash(1), [true, false, false]);
    f.store.put_vvec(T, &b, &pro_tx(0), &[pk(1)]).unwrap();
    assert!(f.store.contributions(T, &b, &valid).unwrap().is_ok());

    f.engine().raw_delete(&row_key(DB_VVEC, T, &b, &pro_tx(0)));
    *now.lock().unwrap() = t0 + MAX_CONTRIBUTION_CACHE_TIME;
    f.store.cleanup_cache();
    let cached = f.store.contributions(T, &b, &valid).unwrap();
    assert_eq!(cached.map(|c| c.1[0].to_vec()), Ok(vec![pk(1)]), "60 s old: kept");

    *now.lock().unwrap() = t0 + MAX_CONTRIBUTION_CACHE_TIME + Duration::from_millis(1);
    f.store.cleanup_cache();
    let reread = f.store.contributions(T, &b, &valid).unwrap();
    assert_eq!(
        reread,
        Err(MissingVvec {
            who: pro_tx(0)
        }),
        "expired: reread"
    );
}

// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:43, :227-238
#[test]
fn the_runtimes_cache_seam_cleans_the_dkgdb_cache() {
    use crate::dkg::seams::ContributionsCache;
    let t0 = Instant::now();
    let now = Arc::new(Mutex::new(t0));
    let clock = now.clone();
    let mut f = fixture(three_members());
    f.store = f.store.with_clock(move || *clock.lock().unwrap());
    let (b, valid) = (hash(1), [true, false, false]);
    f.store.put_vvec(T, &b, &pro_tx(0), &[pk(1)]).unwrap();
    assert!(f.store.contributions(T, &b, &valid).unwrap().is_ok());
    f.engine().raw_delete(&row_key(DB_VVEC, T, &b, &pro_tx(0)));
    let seam: &dyn ContributionsCache = &f.store;
    *now.lock().unwrap() = t0 + MAX_CONTRIBUTION_CACHE_TIME;
    seam.cleanup_cache();
    assert!(f.store.contributions(T, &b, &valid).unwrap().is_ok(), "60 s old: kept");
    *now.lock().unwrap() = t0 + MAX_CONTRIBUTION_CACHE_TIME + Duration::from_millis(1);
    let seam: &dyn ContributionsCache = &f.store;
    seam.cleanup_cache();
    assert!(f.store.contributions(T, &b, &valid).unwrap().is_err(), "expired: reread");
}

// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:147, :196
#[test]
fn each_read_makes_exactly_one_members_access() {
    let f = fixture(three_members());
    let (b, all) = (hash(1), [true, true, true]);
    let mut expect = 0;
    let mut check = |what: &str| {
        expect += 1;
        assert_eq!(f.chain().member_calls.load(Ordering::SeqCst), expect, "{what}");
    };
    assert!(f.store.contributions(T, &b, &all).unwrap().is_err());
    check("a missing vvec");
    for i in 0..3 {
        f.store.put_vvec(T, &b, &pro_tx(i), &[pk(i + 1)]).unwrap();
        f.store.put_encrypted(T, &b, &pro_tx(i), &blobs(i + 1, 3), BlsSchemeKind::Basic).unwrap();
    }
    assert!(f.store.contributions(T, &b, &all).unwrap().is_ok());
    check("rows read");
    assert!(f.store.contributions(T, &b, &all).unwrap().is_ok());
    check("from the cache");
    let enc = |p| f.store.encrypted_contributions(T, &b, &all, &p);
    assert!(enc(pro_tx(1)).unwrap().is_some());
    check("encrypted");
    assert_eq!(enc(pro_tx(9)).unwrap(), None);
    check("encrypted, not a member");
    assert!(f.store.contributions(T, &hash(2), &all).is_err());
    check("a members fault");
}

// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:194-225 GetEncryptedContributions
#[test]
fn encrypted_contributions_take_the_requested_members_blob() {
    let f = fixture(three_members());
    let (b, valid) = (hash(1), [true, false, true]);
    for i in [0, 2] {
        f.store.put_encrypted(T, &b, &pro_tx(i), &blobs(i + 1, 3), BlsSchemeKind::Basic).unwrap();
    }
    let enc = |p| f.store.encrypted_contributions(T, &b, &valid, &p).unwrap();
    let got = enc(pro_tx(1)).unwrap();
    assert_eq!(got, [blobs(1, 3).get(1).unwrap(), blobs(3, 3).get(1).unwrap()]);
    assert_eq!(enc(pro_tx(7)), None, "not a member");

    f.engine().raw_delete(&row_key(DB_ENC_CONTRIB, T, &b, &pro_tx(2)));
    assert_eq!(enc(pro_tx(1)), None, "a missing row");
    f.engine().raw_put(&row_key(DB_ENC_CONTRIB, T, &b, &pro_tx(2)), &[1, 2, 3]);
    assert_eq!(enc(pro_tx(1)), None, "a row that does not read");
    f.store.put_encrypted(T, &b, &pro_tx(2), &blobs(3, 1), BlsSchemeKind::Basic).unwrap();
    assert_eq!(enc(pro_tx(1)), None, "no blob at index 1");
    assert!(enc(pro_tx(0)).is_some());
}

// A row's ephemeral key reads under the basic scheme only: bytes canonical only under the
// legacy scheme are the invalid key, as develop reads a row a legacy-scheme node wrote.
// Core: develop src/bls/bls_ies.h:82
#[test]
fn an_encrypted_row_reads_its_ephemeral_key_under_the_basic_scheme() {
    let f = fixture(three_members());
    let (b, all) = (hash(1), [true, true, true]);
    let seed = (1..50)
        .find(|&seed| {
            let bytes = pk(seed).to_bytes(BlsSchemeKind::Legacy);
            BlsPkValue::from_canonical(&bytes, BlsSchemeKind::Basic).is_none()
        })
        .expect("a key whose legacy bytes are not basic-canonical");
    for i in 0..3 {
        f.store.put_encrypted(T, &b, &pro_tx(i), &blobs(seed, 3), BlsSchemeKind::Legacy).unwrap();
    }
    let enc = f.store.encrypted_contributions(T, &b, &all, &pro_tx(1)).unwrap().unwrap();
    assert!(enc.iter().all(|blob| !blob.ephemeral.is_valid()));
    assert!(enc.iter().all(|blob| blob.data == [seed, 1, 0xee]));
}

// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:240-282 CleanupOldContributions
#[test]
fn cleanup_erases_unknown_and_too_deep_bases_per_type() {
    let params = setup();
    let depth = |t| params.consensus.llmq(t).unwrap().max_store_depth();
    let (d1, d2) = (depth(T), depth(T2));
    assert_ne!(d1, d2, "the two types keep different depths");
    let active = 10_000;
    let chain = TestChain::default();
    *chain.active.lock().unwrap() = Some(active);
    let f = fixture(chain);

    f.store.cleanup_old_contributions().unwrap();
    assert_eq!(f.chain().chain_reads.load(Ordering::SeqCst), 0, "an empty database");

    let at = |n: u8, h: i32| {
        f.chain().heights.lock().unwrap().insert(hash(n), u32::try_from(h).unwrap());
        hash(n)
    };
    let (keep1, old1) = (at(1, active - d1), at(2, active - d1 - 1));
    let (keep2, old2) = (at(3, active - d2), at(4, active - d2 - 1));
    let unknown = hash(5);
    let other = LlmqType::Llmq50_60;
    assert!(params.consensus.llmq(other).is_none(), "not a regtest type");
    let p = pro_tx(0);
    for base in [keep1, old1, keep2, old2, unknown] {
        for t in [T, T2, other] {
            f.store.put_vvec(t, &base, &p, &[pk(1)]).unwrap();
            f.store.put_sk(t, &base, &p, &sk(1)).unwrap();
            f.store.put_encrypted(t, &base, &p, &blobs(1, 1), BlsSchemeKind::Basic).unwrap();
        }
    }
    f.store.cleanup_old_contributions().unwrap();

    let mut left: Vec<(u8, u8, BlockHash)> = f
        .engine()
        .rows()
        .into_iter()
        .map(|(k, _)| (k[6], k[7], BlockHash::from(<[u8; 32]>::try_from(&k[8..40]).unwrap())))
        .collect();
    left.sort();
    let kept = |t: LlmqType, base: &BlockHash| {
        if t == other {
            return true;
        }
        let height = f.chain().heights.lock().unwrap().get(base).copied();
        height.is_some_and(|h| active - i32::try_from(h).unwrap() <= depth(t))
    };
    let mut expected = Vec::new();
    for kind in *b"ESV" {
        for t in [T, T2, other] {
            for base in [keep1, old1, keep2, old2, unknown] {
                if kept(t, &base) {
                    expected.push((kind, t.to_base(), base));
                }
            }
        }
    }
    expected.sort();
    for (t, keep, old) in [(T, keep1, old1), (T2, keep2, old2)] {
        for kind in *b"ESV" {
            assert!(left.contains(&(kind, t.to_base(), keep)), "at the depth: kept");
            assert!(!left.contains(&(kind, t.to_base(), old)), "one deeper: erased");
            assert!(!left.contains(&(kind, t.to_base(), unknown)), "unknown: erased");
        }
    }
    assert_eq!(left, expected);
}

// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:261-263
#[test]
fn cleanup_ends_a_prefix_at_a_key_that_does_not_read() {
    let chain = TestChain::default();
    *chain.active.lock().unwrap() = Some(10_000);
    let f = fixture(chain);
    for base in [hash(1), hash(2)] {
        f.store.put_vvec(T, &base, &pro_tx(0), &[pk(1)]).unwrap();
        f.store.put_sk(T, &base, &pro_tx(0), &sk(1)).unwrap();
    }
    let mut short = type_prefix(DB_VVEC, T);
    short.push(0);
    f.engine().raw_put(&short, &[]);
    f.store.cleanup_old_contributions().unwrap();
    let left: Vec<_> = f.engine().rows().into_iter().map(|(k, _)| (k[6], k.len())).collect();
    assert_eq!(left, [(b'V', 9), (b'V', KEY_LEN), (b'V', KEY_LEN)], "unknown bases, unread");
}

#[test]
fn cleanup_erases_nothing_without_a_chain() {
    let f = fixture(TestChain::default());
    f.store.put_vvec(T, &hash(1), &pro_tx(0), &[pk(1)]).unwrap();
    f.store.cleanup_old_contributions().unwrap();
    assert_eq!(f.engine().rows().len(), 1);

    let unbound = crate::ChainQuorums::new();
    assert!(unbound.members(T, &hash(1)).is_err());
    assert_eq!((unbound.height(&hash(1)), unbound.active_height()), (None, None));
}

// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:35, src/init.cpp:2155
#[test]
fn a_reindex_wipes_the_database_and_a_restart_keeps_it() {
    use dashd_store::{RocksConfig, RocksEngine};
    let dir = tempfile::tempdir().unwrap();
    let path = dkgdb_path(dir.path());
    let open = |wipe| {
        std::fs::create_dir_all(&path).unwrap();
        let (engine, _) = RocksEngine::open(&path, &DKGDB_SCHEMA, &RocksConfig::default()).unwrap();
        let latch = GlobalFailStop::new();
        let sink = Arc::new(RecordingSink::default());
        DkgDb::open(engine, wipe, setup(), three_members(), latch, sink).unwrap()
    };
    let read = |store: &DkgDb<RocksEngine, TestChain>| {
        store.contributions(T, &hash(1), &[true, false, false]).unwrap()
    };
    let store = open(false);
    store.put_vvec(T, &hash(1), &pro_tx(0), &[pk(1)]).unwrap();
    store.put_sk(T, &hash(1), &pro_tx(0), &sk(1)).unwrap();
    drop(store);

    let store = open(false);
    assert_eq!(read(&store).map(|c| c.2), Ok(vec![sk(1)]), "kept across a restart");
    drop(store);
    let store = open(true);
    assert_eq!(
        read(&store),
        Err(MissingVvec {
            who: pro_tx(0)
        }),
        "wiped"
    );
    assert_eq!(store.db.iter_prefix(ROWS, &[]).count(), 0);
}

#[test]
fn a_failed_read_trips_the_latch() {
    let f = fixture(three_members());
    f.store.put_vvec(T, &hash(1), &pro_tx(0), &[pk(1)]).unwrap();
    f.engine().fail_reads.store(true, Ordering::SeqCst);
    let err = f.store.contributions(T, &hash(1), &[true, false, false]).unwrap_err();
    assert!(matches!(&err, DkgDbError::Db(e) if e.kind() == Some(ErrorKind::Io)), "{err}");
    assert_eq!(f.store.db.latch().tripped().map(|t| t.site), Some("dkgdb read"));
    assert!(matches!(&f.sink.marks()[..], [AbortMark::ReadFailed(_)]), "{:?}", f.sink.marks());
    let refused = f.store.put_sk(T, &hash(1), &pro_tx(0), &sk(1)).unwrap_err();
    assert!(matches!(refused, DkgDbError::Db(e) if e.kind() == Some(ErrorKind::ReadOnly)));
    assert!(f.store.cleanup_old_contributions().is_err());
    let all = [true, true, true];
    assert_eq!(f.store.verified(T, &hash(1), &members(), &all), None);
    assert!(f.store.encrypted_contributions(T, &hash(1), &all, &pro_tx(0)).is_err());
}

/// [`DkgStore`]'s contract (dash-llmq `dkg/session.rs`): infallible, a fault fail-stops.
#[test]
fn the_session_store_reads_its_writes_and_fail_stops() {
    let mut f = fixture(three_members());
    let base = QuorumHash::from(bytes(1));
    f.store.write_vvec(T, &base, &pro_tx(0), &[pk(1)]);
    f.store.write_sk(T, &base, &pro_tx(0), &sk(1));
    let got = f.store.verified_contributions(T, &base, &[true, false, false]).unwrap();
    assert_eq!(
        got,
        session::VerifiedContributions {
            member_indexes: vec![0],
            vvecs: vec![vec![pk(1)]],
            sk_contributions: vec![sk(1)],
        }
    );
    let missing = f.store.verified_contributions(T, &base, &[true, true, false]);
    assert_eq!(
        missing,
        Err(MissingVvec {
            who: pro_tx(1)
        })
    );
    assert!(f.store.db.latch().tripped().is_none());

    let unknown = QuorumHash::from(bytes(2));
    let faulted = f.store.verified_contributions(T, &unknown, &[true]);
    assert_eq!(faulted, Ok(session::VerifiedContributions::default()));
    assert_eq!(f.store.db.latch().tripped().map(|t| t.kind), Some(ErrorKind::Other));
    f.store.write_sk(T, &base, &pro_tx(1), &sk(2));
    assert_eq!(f.engine().rows().len(), 2, "writes are refused once the latch has tripped");
}

// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:125-143, src/dbwrapper.cpp:208, :261-269
#[test]
fn a_failed_session_write_reaches_the_abort_sink_once() {
    let mut f = fixture(three_members());
    let base = QuorumHash::from(bytes(1));
    f.engine().fail_writes.store(true, Ordering::SeqCst);
    f.store.write_vvec(T, &base, &pro_tx(0), &[pk(1)]);
    let failure = f.store.db.latch().tripped().unwrap();
    assert_eq!((failure.kind, failure.site), (ErrorKind::Io, "dkgdb write"));
    let marks = f.sink.marks();
    assert!(
        matches!(&marks[..], [AbortMark::AbortNode { log, user: None }] if log.contains("DKG database")),
        "{marks:?}"
    );
    f.store.write_sk(T, &base, &pro_tx(0), &sk(1));
    f.store.write_encrypted(T, &base, &pro_tx(0), &blobs(4, 3));
    assert_eq!(f.sink.marks().len(), 1, "a refused write reports nothing");
    assert!(f.engine().rows().is_empty());
}

// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:277-280, src/dbwrapper.cpp:208, :261-269
#[test]
fn a_failed_cleanup_write_reaches_the_abort_sink_once() {
    let chain = TestChain::default();
    *chain.active.lock().unwrap() = Some(10_000);
    let f = fixture(chain);
    f.store.put_vvec(T, &hash(5), &pro_tx(0), &[pk(1)]).unwrap();
    f.engine().fail_writes.store(true, Ordering::SeqCst);
    let err = f.store.cleanup_old_contributions().unwrap_err();
    assert!(matches!(&err, DkgDbError::Db(e) if e.kind() == Some(ErrorKind::Io)), "{err}");
    assert!(matches!(&f.sink.marks()[..], [AbortMark::AbortNode { .. }]), "{:?}", f.sink.marks());
    assert!(f.store.cleanup_old_contributions().is_err());
    assert_eq!(f.sink.marks().len(), 1, "a refused write reports nothing");
}

#[test]
fn the_shared_store_writes_and_reads_through_a_reference() {
    let f = fixture(three_members());
    let base = QuorumHash::from(bytes(1));
    let mut shared = &f.store;
    shared.write_vvec(T, &base, &pro_tx(0), &[pk(1)]);
    shared.write_sk(T, &base, &pro_tx(0), &sk(1));
    let got = shared.verified_contributions(T, &base, &[true, false, false]).unwrap();
    assert_eq!((got.vvecs, got.sk_contributions), (vec![vec![pk(1)]], vec![sk(1)]));
    assert!(f.store.verified(T, &hash(1), &members(), &[true, false, false]).is_some());
}

// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:171-177 (outside `Read`'s catch)
#[test]
fn a_vvec_row_too_short_for_its_count_trips_the_latch() {
    let f = fixture(three_members());
    f.engine().raw_put(&row_key(DB_VVEC, T, &hash(1), &pro_tx(0)), &[2, 0]);
    let err = f.store.contributions(T, &hash(1), &[true, false, false]).unwrap_err();
    assert!(matches!(&err, DkgDbError::Db(e) if e.kind() == Some(ErrorKind::Corruption)));
    let failure = f.store.db.latch().tripped().unwrap();
    assert_eq!((failure.kind, failure.site), (ErrorKind::Corruption, "dkgdb read"));
    assert!(matches!(&f.sink.marks()[..], [AbortMark::ReadFailed(_)]));
}

#[test]
fn open_refuses_an_engine_of_another_schema() {
    static OTHER: DbSchema = DbSchema {
        name: "other",
        version: 1,
        cfs: DKGDB_SCHEMA.cfs,
    };
    let engine = MemoryEngine::new(&OTHER);
    let sink = Arc::new(RecordingSink::default());
    let opened =
        DkgDb::open(engine, false, setup(), TestChain::default(), GlobalFailStop::new(), sink);
    assert!(opened.is_err());
}
