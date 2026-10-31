//! Persistence (SB0 §5.3 `take_dirty`, §9.5; D38) and the published snapshot (D39) over the
//! regtest capture, through SB10's in-memory engine.

use std::collections::{BTreeMap, BTreeSet};

use dash_kv::codec::{read_varint_u64, write_varint};
use dash_kv::{Cf, CfProfile, CfSpec, Commit, DbSchema, KvBatch, KvEngine, KvRead, MemoryEngine};

use super::*;
use crate::{LockOutcome, MnCfs, MnLoadError, MnWrites, RequestKey};
use dash_primitives::{Commitment, QuorumVvecHash};

static SCHEMA: DbSchema = DbSchema {
    name: "sb11-test",
    version: 1,
    cfs: &[
        CfSpec::new("meta", CfProfile::Ordered),
        CfSpec::new("mn_lists", CfProfile::Ordered),
        CfSpec::new("mn_quorums", CfProfile::PointLookup),
        CfSpec::new("mn_snapshots", CfProfile::Ordered),
    ],
};

fn cfs() -> MnCfs {
    let cf = |name| SCHEMA.cf(name).expect("in the schema");
    MnCfs {
        lists: cf("mn_lists"),
        quorums: cf("mn_quorums"),
        snapshots: cf("mn_snapshots"),
    }
}

/// A host's store: the engine's three tables.
pub(crate) struct Store(MemoryEngine);

impl Store {
    pub fn new() -> Self {
        Store(MemoryEngine::new(&SCHEMA))
    }

    /// A store holding `rows`, each in its tag's table.
    fn of(rows: &BTreeMap<Vec<u8>, Vec<u8>>) -> Self {
        let store = Store::new();
        let c = cfs();
        let mut batch = store.0.batch();
        for (k, v) in rows {
            let cf = match k[0] {
                b'L' | b'B' | b'S' => c.lists,
                b'Q' => c.quorums,
                _ => c.snapshots,
            };
            batch.put(cf, k, v);
        }
        store.0.write(batch, Commit::Durable).expect("an in-memory write");
        store
    }

    pub fn commit(&self, w: &MnWrites) {
        let mut batch = self.0.batch();
        w.write_into(&mut batch, cfs());
        self.0.write(batch, Commit::Durable).expect("an in-memory write");
    }

    pub fn load(&self, chain: &dyn MnChain) -> Result<MasternodeListEngine, MnLoadError> {
        MasternodeListEngine::load(&self.0, cfs(), Mode::Client, chain)
    }

    /// The store loads to `engine`'s rows and holds none the loaded engine would delete. A whole
    /// list's base is a choice of the writes that built the store (D48), so a store written block
    /// by block is compared as loaded, not row for row.
    /// Each record holds its live `held_to`. Its set and base rows are those its lists name (the
    /// loaded engine's `take_dirty` runs the writer's own deletes, so it cannot see an orphan;
    /// validator s5 m1), and its sets, which are content-keyed, those the engine's lists hold.
    #[track_caller]
    pub fn assert_holds(&self, chain: &dyn MnChain, engine: &mut MasternodeListEngine) {
        let mut loaded = self.load(chain).expect("its own rows");
        assert!(loaded.take_dirty().is_empty(), "a row the store should not hold");
        let persisted = engine.persisted();
        assert_eq!(loaded.persisted(), persisted);
        // The rows hold no `held_to` that follows the tip, so it is compared on its own (Opus s5
        // r1 m1).
        assert_eq!(loaded.held_tos(), engine.held_tos(), "each record's held_to");
        let rows = self.rows();
        let named = Store::named(&rows);
        let refs = |rows: &BTreeMap<Vec<u8>, Vec<u8>>, tags: &[u8]| -> BTreeSet<Vec<u8>> {
            rows.keys().filter(|k| tags.contains(&k[0])).cloned().collect()
        };
        assert_eq!(refs(&rows, b"BS"), named, "set and base rows no list names");
        assert_eq!(refs(&rows, b"S"), refs(&persisted, b"S"), "the engine's sets");
    }

    /// The set and base rows `rows`' lists name (D48).
    fn named(rows: &BTreeMap<Vec<u8>, Vec<u8>>) -> BTreeSet<Vec<u8>> {
        let mut named = BTreeSet::new();
        for (_, row) in rows.iter().filter(|(k, _)| k[0] == b'L') {
            named.insert([b"S".as_slice(), &row[1..33]].concat());
            if row[0] & 2 != 0 {
                named.insert(Delta::of(row).base);
            }
        }
        named
    }

    /// Puts `set` under its hash and the list `list` at `key` naming it (D48).
    fn rename_set(&self, key: &[u8], list: &[u8], set: &[u8]) {
        let id = dash_consensus::hash::sha256d(set);
        self.damage(cfs().lists, None, &[b"S".as_slice(), &id].concat(), set);
        let mut renamed = list.to_vec();
        renamed[1..33].copy_from_slice(&id);
        self.damage(cfs().lists, None, key, &renamed);
    }

    /// Every row of the three tables.
    pub fn rows(&self) -> BTreeMap<Vec<u8>, Vec<u8>> {
        let c = cfs();
        [c.lists, c.quorums, c.snapshots]
            .into_iter()
            .flat_map(|cf| self.0.iter_prefix(cf, &[]).map(|r| r.expect("an in-memory read")))
            .collect()
    }

    /// Puts `value` at `key` in `cf` and deletes `remove` from it, as a damaged store would hold.
    fn damage(&self, cf: Cf, remove: Option<&[u8]>, key: &[u8], value: &[u8]) {
        let mut batch = self.0.batch();
        if let Some(k) = remove {
            batch.delete(cf, k);
        }
        batch.put(cf, key, value);
        self.0.write(batch, Commit::Durable).expect("an in-memory write");
    }
}

/// A whole list row's parts (D48): its bytes through its base's key, the base row's key, the
/// removed proRegTx hashes and the changed entries' wire bytes.
#[derive(Clone)]
struct Delta {
    prefix: Vec<u8>,
    base: Vec<u8>,
    removed: Vec<[u8; 32]>,
    changed: Vec<Vec<u8>>,
}

impl Delta {
    /// Flags, the set's hash, a coinbase (tag 0, or 1 and its VARINT-length-prefixed bytes), the
    /// base's height (little-endian; its key holds it big-endian, as a list's) and hash, then the
    /// removed hashes and the changed entries, each counted.
    fn of(row: &[u8]) -> Delta {
        assert!(row[0] & 2 != 0, "a whole list");
        let mut rest = &row[34..];
        if row[33] == 1 {
            let len = varint(&mut rest);
            rest = &rest[len..];
        }
        let height = u32::from_le_bytes(rest[..4].try_into().expect("4 bytes"));
        let base = [b"B".as_slice(), &height.to_be_bytes(), &rest[4..36]].concat();
        let prefix = row[..row.len() - rest.len() + 36].to_vec();
        let mut d = &rest[36..];
        let removed = (0..varint(&mut d))
            .map(|_| {
                let (hash, tail) = d.split_at(32);
                d = tail;
                hash.try_into().expect("32 bytes")
            })
            .collect();
        let changed = wire_entries(&mut d);
        assert!(d.is_empty(), "the row's end");
        Delta {
            prefix,
            base,
            removed,
            changed,
        }
    }

    fn row(&self) -> Vec<u8> {
        let mut v = self.prefix.clone();
        write_varint(&mut v, self.removed.len() as u64);
        self.removed.iter().for_each(|hash| v.extend_from_slice(hash));
        put_wire_entries(&mut v, &self.changed);
        v
    }
}

fn varint(v: &mut &[u8]) -> usize {
    usize::try_from(read_varint_u64(v).expect("a VARINT")).expect("small")
}

/// A VARINT count of VARINT-length-prefixed wire entries, as a base row or a delta's changes.
fn wire_entries(v: &mut &[u8]) -> Vec<Vec<u8>> {
    (0..varint(v))
        .map(|_| {
            let n = varint(v);
            let (entry, tail) = v.split_at(n);
            *v = tail;
            entry.to_vec()
        })
        .collect()
}

fn put_wire_entries(v: &mut Vec<u8>, entries: &[Vec<u8>]) {
    write_varint(v, entries.len() as u64);
    for e in entries {
        write_varint(v, e.len() as u64);
        v.extend_from_slice(e);
    }
}

/// A wire entry's proRegTx hash, after its version.
fn pro_reg_tx_hash(entry: &[u8]) -> [u8; 32] {
    entry[2..34].try_into().expect("32 bytes")
}

/// `engine` written to a new store and loaded back from it on `chain`: a restart.
pub(crate) fn reloaded(
    engine: &mut MasternodeListEngine,
    chain: &dyn MnChain,
) -> MasternodeListEngine {
    let store = Store::new();
    store.commit(&engine.take_dirty());
    store.load(chain).expect("its own rows")
}

/// Core's ChainLock at `height`.
fn clsig_at(height: u32) -> ClSig {
    let height = i32::try_from(height).expect("small");
    clsigs().into_iter().find(|(c, _)| c.height == height).expect("captured").0
}

/// Every list and both cycles' rows, Core's tip ChainLock verified, and retention run at the tip.
fn synced_at_tip(chain: &FixtureChain) -> MasternodeListEngine {
    let mut engine = synced(chain);
    engine.verify_chainlock(&clsig_at(chain.tip), chain, config()).expect("Core verified it");
    engine.tip_changed(chain, config()).expect("step 5");
    engine
}

/// A list row's key: its tag, height and block hash.
fn list_key(height: u32, hash: &BlockHash) -> Vec<u8> {
    let mut k = vec![b'L'];
    k.extend_from_slice(&height.to_be_bytes());
    k.extend_from_slice(hash.as_bytes());
    k
}

/// A block mark's value: its height (LE) and hash.
fn block_mark(height: u32, hash: &BlockHash) -> Vec<u8> {
    [height.to_le_bytes().as_slice(), hash.as_bytes()].concat()
}

fn mark_key(name: &str) -> Vec<u8> {
    [b"M".as_slice(), name.as_bytes()].concat()
}

/// D38: a synced engine reloads to the same rows, its sets shared, the same lock verdicts
/// and lookups, and after `tip_changed` the same snapshot; its locator is the tip.
#[test]
fn a_synced_engine_reloads_as_it_was() {
    let chain = FixtureChain::full();
    let mut engine = synced_at_tip(&chain);
    let store = Store::new();
    let w = engine.take_dirty();
    assert_eq!(w.locator(), Some((chain.hash(chain.tip), chain.tip)));
    store.commit(&w);
    assert!(engine.take_dirty().is_empty(), "nothing changed since");

    let mut loaded = store.load(&chain).expect("its own rows");
    assert!(loaded.take_dirty().is_empty(), "the store holds what it loaded");
    let rows = engine.persisted();
    assert_eq!(loaded.persisted(), rows);
    assert_eq!(store.rows(), rows);
    let horizon = chain.tip - config().h_keep()..=chain.tip;
    for (a, b) in horizon.clone().zip(horizon.skip(1)) {
        let (a, b) = (chain.id(a), chain.id(b));
        // Load shares every equal set, so at least where D28 did.
        assert!(!engine.shares_set(a, b) || loaded.shares_set(a, b), "D28's sharing at {b:?}");
    }

    // The fixture's test seam keeps the `qrinfo`'s base; no store holds a pin.
    loaded.pin_list(chain.id(QRINFO_BASE));
    loaded.tip_changed(&chain, config()).expect("step 5");
    assert_eq!(loaded.snapshot(), engine.snapshot());
    assert!(loaded.take_dirty().is_empty(), "step 5 decides nothing new");
    for (cl, _) in clsigs() {
        let ours = engine.verify_chainlock(&cl, &chain, config());
        assert_eq!(loaded.verify_chainlock(&cl, &chain, config()), ours, "{}", cl.height);
    }
    for (islock, _) in isdlocks() {
        let ours = engine.verify_islock(&islock, &chain, config());
        assert_eq!(loaded.verify_islock(&islock, &chain, config()), ours);
    }
    for t in [CHAIN_LOCKS, ROTATED] {
        for q in core_quorums(t) {
            let hash = QuorumHash::from(q.hash);
            for h in q.height..=chain.tip + 1 {
                let ours = engine.lookup_quorum(llmq(t), &hash, h, &chain, config());
                assert_eq!(loaded.lookup_quorum(llmq(t), &hash, h, &chain, config()), ours);
            }
        }
    }
}

/// D38: a client following the chain with its store commits each block's changes, pruning as
/// deletes; the store always holds exactly the engine's rows, at the engine's locator. A block
/// whose set is one the store holds writes no set row (D48).
#[test]
fn a_following_clients_store_holds_its_rows() {
    let mut chain = FixtureChain::full();
    let tip = chain.tip;
    let store = Store::new();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    chain.tip = FIRST;
    engine.apply_diff(&genesis_diff(FIRST), &chain, config()).expect("Core's diff");
    store.commit(&engine.take_dirty());
    let mut deleted = BTreeMap::<u8, usize>::new();
    let mut set_puts = 0;
    for h in FIRST + 1..=tip {
        chain.tip = h;
        engine.apply_diff(&step_diff(h), &chain, config()).unwrap_or_else(|e| panic!("{h}: {e}"));
        let w = engine.take_dirty();
        assert_eq!(w.locator(), Some((chain.hash(h), h)));
        for (k, _) in w.ops().filter(|(_, v)| v.is_none()) {
            *deleted.entry(k[0]).or_default() += 1;
        }
        set_puts += usize::from(w.ops().any(|(k, v)| k[0] == b'S' && v.is_some()));
        store.commit(&w);
        store.assert_holds(&chain, &mut engine);
    }
    assert!(deleted.get(&b'L').is_some_and(|n| *n > 0), "pruned lists go: {deleted:?}");
    assert!(set_puts < (tip - FIRST) as usize, "blocks with no new set: {set_puts}");
    let mut loaded = store.load(&chain).expect("its own rows");
    assert_eq!(loaded.persisted(), engine.persisted());
}

/// D38: an engine restarted mid-way (its requests and caches gone) and following on ends with
/// the rows one that never stopped has.
#[test]
fn a_restart_mid_way_ends_where_following_does() {
    let mut chain = FixtureChain::full();
    let tip = chain.tip;
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let mut restarted = MasternodeListEngine::new(Mode::Client);
    chain.tip = FIRST;
    for e in [&mut engine, &mut restarted] {
        e.apply_diff(&genesis_diff(FIRST), &chain, config()).expect("Core's diff");
    }
    for h in FIRST + 1..=tip {
        chain.tip = h;
        if h == 290 {
            restarted = reloaded(&mut restarted, &chain);
            restarted.tip_changed(&chain, config()).expect("step 5");
        }
        for e in [&mut engine, &mut restarted] {
            e.apply_diff(&step_diff(h), &chain, config()).unwrap_or_else(|e| panic!("{h}: {e}"));
        }
    }
    assert_eq!(restarted.persisted(), engine.persisted());
}

/// D38, SB0 §9.5: rows whose locator the chain does not know (a lost suffix) are refused, not
/// loaded around, since marks of the lost branch could answer what the chain would not
/// (validator M1). The host resets them and starts afresh.
#[test]
fn rows_past_the_chain_are_refused_and_reset() {
    let chain = FixtureChain::full();
    let mut engine = synced_at_tip(&chain);
    let store = Store::new();
    store.commit(&engine.take_dirty());
    let lost = FixtureChain::anchored(0, 290);
    match store.load(&lost) {
        Err(MnLoadError::UnknownLocator {
            height,
            hash,
        }) => assert_eq!((height, hash), (chain.tip, chain.hash(chain.tip))),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("loaded"),
    }
    let w = MasternodeListEngine::reset(&store.0, cfs()).expect("an in-memory read");
    assert_eq!(w.len(), store.rows().len());
    assert!(w.ops().all(|(_, v)| v.is_none()));
    assert_eq!(w.locator(), None);
    store.commit(&w);
    assert!(store.rows().is_empty());
    assert!(store.load(&lost).expect("nothing to load").persisted().is_empty());
}

/// D38: while the rows' locator is on the chain, a list on a branch it has forgotten (its
/// headers pruned) is dropped at load and deleted by the next `take_dirty`, as is a host's last
/// diff on it; a best ChainLock on a block it does not know is kept (its header may be yet to
/// come). Nothing else is rewritten. Retention cleared the host's base once the chain left the
/// branch (Sol s3 r1 M1), so the store holds no such mark: one written before that rule is put in
/// by hand, as the best ChainLock is.
#[test]
fn a_list_on_a_forgotten_branch_is_dropped_and_deleted() {
    let mut chain = FixtureChain::full();
    let mut engine = synced_at_tip(&chain);
    chain.fork = Some(300);
    chain.fork_active = true;
    let branch = chain.fork_hash(chain.tip);
    engine
        .apply_diff(&chain.on_fork(genesis_diff(chain.tip)), &chain, config())
        .expect("Core's diff");
    chain.fork_active = false;
    engine.tip_changed(&chain, config()).expect("step 5");
    let store = Store::new();
    store.commit(&engine.take_dirty());
    let branch_key = list_key(chain.tip, &branch);
    assert!(store.rows().contains_key(&branch_key), "its set is kept within the horizon");
    assert!(!store.rows().contains_key(&mark_key("last_diff")), "retention cleared it");
    store.damage(cfs().snapshots, None, &mark_key("best_cl"), &block_mark(chain.tip, &branch));
    store.damage(cfs().snapshots, None, &mark_key("last_diff"), &block_mark(chain.tip, &branch));

    let forgot = FixtureChain::full();
    let mut loaded = store.load(&forgot).expect("its locator is on the chain");
    let w = loaded.take_dirty();
    assert_eq!(w.locator(), Some((chain.hash(chain.tip), chain.tip)));
    let mut gone: Vec<Vec<u8>> =
        w.ops().filter(|(_, v)| v.is_none()).map(|(k, _)| k.to_vec()).collect();
    gone.sort();
    // The host's last diff was the branch's. A best lock on a block the chain has yet to accept
    // stays (validator r2 m-1).
    let mut expected = vec![branch_key, mark_key("last_diff")];
    expected.sort();
    assert_eq!(gone, expected);
    for (k, _) in w.ops().filter(|(_, v)| v.is_some()) {
        assert_eq!(k[0], b'Q', "only records whose marks went are rewritten: {k:?}");
    }
    assert!(loaded.persisted().contains_key(&mark_key("best_cl")));
    store.commit(&w);
    store.assert_holds(&chain, &mut loaded);
}

/// D34, DEC-408: a host's base on the active chain whose list is no longer whole (a store an
/// earlier build wrote) loads, and the next retention clears it and deletes its mark.
#[test]
fn a_host_base_held_only_as_a_set_is_cleared_after_load() {
    let chain = FixtureChain::full();
    let mut engine = synced_at_tip(&chain);
    let store = Store::new();
    store.commit(&engine.take_dirty());
    let h = chain.tip - 1;
    assert!(!engine.whole_lists().contains(&h));
    store.damage(cfs().snapshots, None, &mark_key("last_diff"), &block_mark(h, &chain.hash(h)));
    let mut loaded = store.load(&chain).expect("its rows");
    assert_eq!(loaded.host_base(), Some(h));
    loaded.tip_changed(&chain, config()).expect("step 5");
    assert_eq!(loaded.host_base(), None);
    assert!(loaded.take_dirty().ops().any(|(k, v)| k == mark_key("last_diff") && v.is_none()));
}

/// The table of the `Corrupt` error `store` is refused with.
fn corrupt_table(store: &Store, chain: &FixtureChain) -> &'static str {
    match store.load(chain) {
        Err(MnLoadError::Corrupt {
            table,
            ..
        }) => table,
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("loaded"),
    }
}

/// D38: a damaged row is refused with its table, not loaded: a whole list whose entries no longer
/// hash to its CbTx's root, a record under another commitment's key, a truncated record, an
/// unknown mark, a list keyed at a height its block is not at, a best ChainLock likewise, a whole
/// list with entries and no coinbase, and
/// tables that disagree: a set naming a commitment no row holds, or holding one under another
/// quorum's key (validator M2), lists without their locator (r2 n-1), and a rotation row with more
/// candidates than replies leave (r2 n-3).
#[test]
fn a_damaged_row_is_corrupt() {
    let chain = FixtureChain::full();
    let rows = synced_at_tip(&chain).persisted();
    let fresh = || Store::of(&rows);
    let corrupt = |store: &Store| corrupt_table(store, &chain);
    let c = cfs();
    let tip_key = list_key(chain.tip, &chain.hash(chain.tip));
    let tip_list = rows[&tip_key].clone();

    let mut flipped = tip_list.clone();
    *flipped.last_mut().expect("entries") ^= 1;
    let store = fresh();
    assert!(store.load(&chain).is_ok(), "undamaged");
    store.damage(c.lists, None, &tip_key, &flipped);
    assert_eq!(corrupt(&store), "mn_lists");

    let mut q = rows.iter().filter(|(k, _)| k[0] == b'Q');
    let (k1, v1) = q.next().expect("a record");
    let (k2, _) = q.next().expect("another");
    let store = fresh();
    store.damage(c.quorums, Some(k2), k2, v1);
    assert_eq!(corrupt(&store), "mn_quorums");
    let store = fresh();
    store.damage(c.quorums, None, k1, &v1[..v1.len() - 1]);
    assert_eq!(corrupt(&store), "mn_quorums");

    let store = fresh();
    store.damage(c.snapshots, None, &mark_key("nope"), &[]);
    assert_eq!(corrupt(&store), "mn_snapshots");

    let store = fresh();
    store.damage(
        c.lists,
        Some(&tip_key),
        &list_key(chain.tip - 1, &chain.hash(chain.tip)),
        &tip_list,
    );
    assert_eq!(corrupt(&store), "mn_lists");

    let store = fresh();
    let best = block_mark(chain.tip - 1, &chain.hash(chain.tip));
    store.damage(c.snapshots, None, &mark_key("best_cl"), &best);
    assert_eq!(corrupt(&store), "mn_snapshots");

    // A rotation row of one candidate (tag 1, count 1), stored as more candidates than replies
    // ever leave (`ROW_CANDIDATES`, 3).
    let (row_key, one) =
        rows.iter().find(|(k, v)| k[0] == b'R' && v[..2] == [1, 1]).expect("a one-candidate row");
    let snapshot = &one[2..];
    let mut four = vec![1, 4];
    (0..4).for_each(|_| four.extend_from_slice(snapshot));
    let mut three = vec![1, 3];
    (0..3).for_each(|_| three.extend_from_slice(snapshot));
    let store = fresh();
    store.damage(c.snapshots, None, row_key, &three);
    assert!(store.load(&chain).is_ok(), "three candidates are a reply's");
    store.damage(c.snapshots, None, row_key, &four);
    assert_eq!(corrupt(&store), "mn_snapshots");
    // Its row (a VARINT member count, then a bit each) one member short: with the freed bit
    // clear it loads, set (a bit past the last member) it is refused (Opus s4 r1 n3).
    let n = usize::from(snapshot[0]);
    assert!(n > 1 && n < 0xfd && n % 8 == 0, "a one-byte count, whole bytes: {n}");
    let mut short = one.clone();
    short[2] = u8::try_from(n - 1).expect("small");
    short[2 + 1 + (n - 1) / 8] &= 0x7f;
    let store = fresh();
    store.damage(c.snapshots, None, row_key, &short);
    assert!(store.load(&chain).is_ok(), "a row one member short");
    short[2 + 1 + (n - 1) / 8] |= 0x80;
    store.damage(c.snapshots, None, row_key, &short);
    assert_eq!(corrupt(&store), "mn_snapshots");

    let mut unmarked = rows.clone();
    unmarked.remove(&mark_key("retained")).expect("the locator");
    assert_eq!(corrupt(&Store::of(&unmarked)), "mn_snapshots");

    // The tip list's set row, named by its hash after the list's flags: a one-byte count, then
    // (type ‖ quorum hash ‖ leaf) each (D48).
    let set_key = [b"S".as_slice(), &tip_list[1..33]].concat();
    let set = rows[&set_key].clone();
    assert!(set[0] >= 2 && set[0] < 0xfd, "two or more quorums, a one-byte count");
    let leaf = |i: usize| set[1 + 65 * i + 33..1 + 65 * (i + 1)].to_vec();
    let store = fresh();
    let row = [b"Q".as_slice(), &leaf(0)].concat();
    let mut gone = store.rows();
    gone.remove(&row).expect("the leaf's row");
    let store = Store::of(&gone);
    assert_eq!(corrupt(&store), "mn_lists");
    // Its coinbase (tag 1, VARINT length, transaction) dropped to tag 0, entries kept.
    let at = 33;
    assert_eq!(tip_list[at], 1, "a coinbase");
    let mut rest = &tip_list[at + 1..];
    let len = usize::try_from(read_varint_u64(&mut rest).expect("its length")).expect("small");
    let bare = [&tip_list[..at], &[0][..], &rest[len..]].concat();
    let store = fresh();
    store.damage(c.lists, None, &tip_key, &bare);
    assert_eq!(corrupt(&store), "mn_lists");
    let mut swapped = set.clone();
    swapped[1 + 33..1 + 65].copy_from_slice(&leaf(1));
    swapped[1 + 65 + 33..1 + 130].copy_from_slice(&leaf(0));
    let store = fresh();
    store.rename_set(&tip_key, &tip_list, &swapped);
    assert_eq!(corrupt(&store), "mn_lists");

    // D48: a set under another set's key, a list whose set or base is not stored, and a delta
    // that removes an entry its base lacks or repeats one it holds.
    let store = fresh();
    store.damage(c.lists, None, &set_key, &swapped);
    assert_eq!(corrupt(&store), "mn_lists");
    let mut unset = rows.clone();
    unset.remove(&set_key);
    assert_eq!(corrupt(&Store::of(&unset)), "mn_lists");
    let delta = Delta::of(&tip_list);
    let mut unbased = rows.clone();
    unbased.remove(&delta.base).expect("the tip list's base");
    assert_eq!(corrupt(&Store::of(&unbased)), "mn_lists");
    let base = wire_entries(&mut &rows[&delta.base][..]);
    let removes_absent = Delta {
        removed: vec![[0xab; 32]],
        ..delta.clone()
    };
    let repeats = Delta {
        changed: vec![base[0].clone()],
        ..delta.clone()
    };
    for damaged in [removes_absent, repeats] {
        let store = fresh();
        store.damage(c.lists, None, &tip_key, &damaged.row());
        assert_eq!(corrupt(&store), "mn_lists");
    }

    // Validator s5 n1: a base or a set out of order decodes to the same entries, so it is refused
    // (D38: a row decodes from one value only).
    let mut reversed = base.clone();
    reversed.reverse();
    let mut v = Vec::new();
    put_wire_entries(&mut v, &reversed);
    let store = fresh();
    store.damage(c.lists, None, &delta.base, &v);
    assert_eq!(corrupt(&store), "mn_lists");
    let mut reordered = vec![set[0]];
    (0..usize::from(set[0]))
        .rev()
        .for_each(|i| reordered.extend(&set[1 + 65 * i..1 + 65 * (i + 1)]));
    let store = fresh();
    store.rename_set(&tip_key, &tip_list, &reordered);
    assert_eq!(corrupt(&store), "mn_lists");

    // A replaced record's row (first seen u32, flags, state, then its decider, replaced and mined:
    // 0, or 1 and a height, ‖ hash for a mark) with its held_to as the sets' tag 2 (Opus s5 r1 n1).
    let skip = |v: &[u8], at: usize, len: usize| {
        at + if v[at] == 1 {
            1 + len
        } else {
            1
        }
    };
    let (key, row, at) = rows
        .iter()
        .filter(|(k, _)| k[0] == b'Q')
        .find_map(|(k, v)| {
            let replaced = skip(v, 6, 36);
            (v[replaced] == 1).then(|| (k, v, skip(v, skip(v, replaced, 4), 36)))
        })
        .expect("a replaced record");
    let tagged = [&row[..at], &[2], &row[skip(row, at, 4)..]].concat();
    let store = fresh();
    store.damage(c.quorums, None, key, &tagged);
    assert_eq!(corrupt(&store), "mn_quorums");

    // Validator s5 m3: another set's value under a set's key, which only set-only lists name, so
    // no root check rebuilds it: only the key's hash refuses it.
    let lists: Vec<&Vec<u8>> = rows.iter().filter(|(k, _)| k[0] == b'L').map(|(_, v)| v).collect();
    let set_only = |k: &&Vec<u8>| {
        let naming = || lists.iter().filter(|v| v[1..33] == k[1..]);
        naming().next().is_some() && naming().all(|v| v[0] & 2 == 0)
    };
    let key = rows.keys().filter(|k| k[0] == b'S').find(set_only).expect("a set-only set");
    let other = rows.iter().find(|(k, _)| k[0] == b'S' && *k != key).expect("another set").1;
    let store = fresh();
    store.damage(c.lists, None, key, other);
    assert_eq!(corrupt(&store), "mn_lists");
}

/// The store of the capture synced to `tip` − 1, the tip list's base rewritten to lack `gone` of
/// its lists' entries and hold `absent` ones (proRegTx hashes of each byte), and every list
/// naming it given the matching delta: the capture removes no masternode (validator s5 M1).
struct Rebased {
    store: Store,
    /// At `tip` − 1.
    chain: FixtureChain,
    base_key: Vec<u8>,
    base: Vec<Vec<u8>>,
    /// The entries taken out of the base.
    gone: Vec<Vec<u8>>,
    /// The hashes put in.
    added: Vec<[u8; 32]>,
}

fn rebased(gone: usize, absent: &[u8]) -> Rebased {
    let mut chain = FixtureChain::full();
    chain.tip -= 1;
    let mut engine = applied(&chain, FIRST, FIRST + 1..=chain.tip);
    engine.tip_changed(&chain, config()).expect("step 5");
    let store = Store::new();
    store.commit(&engine.take_dirty());
    let rows = store.rows();
    let base_key = Delta::of(&rows[&list_key(chain.tip, &chain.hash(chain.tip))]).base;
    let mut base = wire_entries(&mut &rows[&base_key][..]);
    assert!(base.len() > gone + 1, "{}", base.len());
    let gone: Vec<Vec<u8>> = base.drain(1..=gone).collect();
    let added: Vec<[u8; 32]> = absent.iter().map(|b| [*b; 32]).collect();
    for hash in &added {
        let mut e = base[0].clone();
        e[2..34].copy_from_slice(hash);
        base.push(e);
    }
    base.sort_by_key(|e| pro_reg_tx_hash(e));
    let mut v = Vec::new();
    put_wire_entries(&mut v, &base);
    store.damage(cfs().lists, None, &base_key, &v);
    let mut naming = 0;
    for (key, row) in rows.iter().filter(|(k, v)| k[0] == b'L' && v[0] & 2 != 0) {
        let mut d = Delta::of(row);
        if d.base != base_key {
            continue;
        }
        for g in &gone {
            let hash = pro_reg_tx_hash(g);
            if let Some(i) = d.removed.iter().position(|h| *h == hash) {
                d.removed.remove(i);
            } else if !d.changed.iter().any(|c| pro_reg_tx_hash(c) == hash) {
                d.changed.push(g.clone());
            }
        }
        d.removed.extend(&added);
        d.removed.sort();
        d.changed.sort_by_key(|e| pro_reg_tx_hash(e));
        store.damage(cfs().lists, None, key, &d.row());
        naming += 1;
    }
    assert!(naming > 0);
    Rebased {
        store,
        chain,
        base_key,
        base,
        gone,
        added,
    }
}

/// D48 (validator s5 M1): a list written against a base that holds an entry the list lacks and
/// lacks one it holds stores both, and the store reloads to the same engine.
#[test]
fn a_delta_against_a_differing_base_is_written_and_reloaded() {
    let Rebased {
        store,
        mut chain,
        base_key,
        gone,
        added: removed,
        ..
    } = rebased(1, &[0xff]);
    let mut loaded = store.load(&chain).expect("the deltas decode");
    assert!(loaded.take_dirty().is_empty(), "and are the canonical rows");
    chain.tip += 1;
    loaded.apply_diff(&step_diff(chain.tip), &chain, config()).expect("Core's diff");
    let w = loaded.take_dirty();
    let tip_key = list_key(chain.tip, &chain.hash(chain.tip));
    let row = w.ops().find(|(k, _)| *k == tip_key.as_slice()).and_then(|(_, v)| v);
    let delta = Delta::of(row.expect("the tip list"));
    store.commit(&w);
    assert_eq!(delta.base, base_key, "written against the base");
    assert_eq!(delta.removed, removed);
    let hash = pro_reg_tx_hash(&gone[0]);
    assert!(delta.changed.iter().any(|c| pro_reg_tx_hash(c) == hash), "the change");
    store.assert_holds(&chain, &mut loaded);
}

/// D48 (validator s5 n1): a delta out of proRegTx order, or removing an entry it also changes,
/// decodes to the same list, so it is refused (D38: a row decodes from one value only).
#[test]
fn a_delta_out_of_order_or_removing_a_change_is_corrupt() {
    let Rebased {
        store,
        chain,
        base,
        added: removed,
        ..
    } = rebased(2, &[0xfe, 0xff]);
    assert!(store.load(&chain).is_ok(), "the canonical deltas");
    let key = list_key(chain.tip, &chain.hash(chain.tip));
    let delta = Delta::of(&store.rows()[&key]);
    assert!(delta.removed.len() >= 2 && delta.changed.len() >= 2);
    let mut reversed = delta.clone();
    reversed.removed.reverse();
    let mut swapped = delta.clone();
    swapped.changed.reverse();
    let kept = base.iter().find(|e| {
        let hash = pro_reg_tx_hash(e);
        !removed.contains(&hash) && !delta.changed.iter().any(|c| pro_reg_tx_hash(c) == hash)
    });
    let kept = kept.expect("an entry the list holds as its base does");
    let mut both = delta.clone();
    both.removed.push(pro_reg_tx_hash(kept));
    both.removed.sort();
    both.changed.push(kept.clone());
    both.changed.sort_by_key(|e| pro_reg_tx_hash(e));
    for damaged in [reversed, swapped, both] {
        let damaged_store = Store::of(&store.rows());
        damaged_store.damage(cfs().lists, None, &key, &damaged.row());
        assert_eq!(corrupt_table(&damaged_store, &chain), "mn_lists");
    }
}

/// D48 (validator s5 m3): `load` takes `held_to` from authenticated sets on the retained tip's
/// chain only. A commitment an unauthenticated set alone holds, and one a set on another branch
/// alone holds, reload without one, as the live engine holds them.
#[test]
fn a_reloaded_held_to_reads_authenticated_active_chain_sets_only() {
    let mut chain = FixtureChain::full();
    let mut engine = synced_at_tip(&chain);
    chain.fork = Some(300);
    let q = &core_quorums(ROTATED)[0];
    let (qc, _) = engine.quorum(llmq(ROTATED), &QuorumHash::from(q.hash)).expect("held");
    let renamed = |b: u8| {
        let mut qc = (*qc).clone();
        qc.quorum_hash = QuorumHash::from([b; 32]);
        qc
    };
    let (unauthenticated, forked) = (renamed(1), renamed(2));
    let below = chain.tip - 1;
    engine.hold_set(
        chain.id(below),
        below,
        chain.hash(below),
        std::slice::from_ref(&unauthenticated),
    );
    engine.forget_authentication(chain.id(below));
    let fork_tip = BlockId(FORK + chain.tip);
    engine.hold_set(fork_tip, chain.tip, chain.fork_hash(chain.tip), std::slice::from_ref(&forked));
    let live = engine.held_tos();
    for qc in [&unauthenticated, &forked] {
        assert_eq!(live[&crate::convert::commitment_hash(qc)], None);
    }
    let store = Store::new();
    store.commit(&engine.take_dirty());
    assert_eq!(store.load(&chain).expect("its own rows").held_tos(), live);
    engine.tip_changed(&chain, config()).expect("step 5");
    let flags = engine.held_by_sets();
    for qc in [&unauthenticated, &forked] {
        let hash = crate::convert::commitment_hash(qc);
        assert_eq!(engine.held_tos()[&hash], None);
        assert!(flags[&hash], "retention's none is the sets', so the row stores no height");
    }
    store.commit(&engine.take_dirty());
    store.assert_holds(&chain, &mut engine);
}

/// D48 (Opus s5 r1 m1): a record only lists kept whole below the horizon hold (a `qrinfo`'s
/// work-block lists, a lookup's deep target) is never marked by retention, so its row stores
/// its `held_to` (none) rather than the sets', and reloads as the live engine holds it; its
/// lookups agree before and after a later retention.
#[test]
fn a_record_only_a_deep_kept_list_holds_reloads_its_held_to() {
    let chain = FixtureChain::full();
    let mut engine = synced_at_tip(&chain);
    let q = &core_quorums(ROTATED)[0];
    let (qc, _) = engine.quorum(llmq(ROTATED), &QuorumHash::from(q.hash)).expect("held");
    let renamed = |b: u8| {
        let mut qc = (*qc).clone();
        qc.quorum_hash = QuorumHash::from([b; 32]);
        qc
    };
    let horizon = chain.tip - config().h_keep();
    let (a, b) = (horizon - 20, horizon - 10);
    let (deep, newer) = (renamed(3), renamed(4));
    for (h, qc) in [(a, &deep), (b, &newer)] {
        engine.hold_set(chain.id(h), h, chain.hash(h), std::slice::from_ref(qc));
        engine.pin_list(chain.id(h));
    }
    engine.tip_changed(&chain, config()).expect("step 5");
    let live = engine.held_tos();
    assert_eq!(live[&crate::convert::commitment_hash(&deep)], None, "never marked");
    let mut loaded = reloaded(&mut engine, &chain);
    assert_eq!(loaded.held_tos(), live);
    loaded.pin_list(chain.id(a));
    loaded.pin_list(chain.id(b));
    loaded.tip_changed(&chain, config()).expect("step 5");
    assert_eq!(loaded.held_tos(), engine.held_tos());
    for h in a - 5..=chain.tip + 1 {
        for qh in [[3; 32], [4; 32]] {
            let quorum = QuorumHash::from(qh);
            let ours = engine.lookup_quorum(llmq(ROTATED), &quorum, h, &chain, config());
            let theirs = loaded.lookup_quorum(llmq(ROTATED), &quorum, h, &chain, config());
            assert_eq!(theirs, ours, "{h}");
        }
    }
}

/// D38, D48 (Opus s5 r1 m2): a base and a set deleted with their last list are forgotten by the
/// mirror, so the same block's list held again (a re-fetch, a reorg back) puts them again.
#[test]
fn a_deleted_base_and_set_are_put_again() {
    let mut chain = FixtureChain::full();
    chain.fork = Some(300);
    let mut engine = synced_at_tip(&chain);
    let store = Store::new();
    store.commit(&engine.take_dirty());
    let q = &core_quorums(ROTATED)[0];
    let (qc, _) = engine.quorum(llmq(ROTATED), &QuorumHash::from(q.hash)).expect("held");
    let mut qc = (*qc).clone();
    qc.quorum_hash = QuorumHash::from([5; 32]);
    let (id, h, hash) = (BlockId(FORK + chain.tip), chain.tip, chain.fork_hash(chain.tip));
    for round in 0..2 {
        // An empty whole list: no stored base is within a quarter of it, so it is its own base.
        engine.hold_set(id, h, hash, std::slice::from_ref(&qc));
        let w = engine.take_dirty();
        let puts: Vec<u8> = w.ops().filter(|(_, v)| v.is_some()).map(|(k, _)| k[0]).collect();
        assert!(puts.contains(&b'B') && puts.contains(&b'S'), "{round}: {puts:?}");
        store.commit(&w);
        store.load(&chain).unwrap_or_else(|e| panic!("{round}: {e}"));
        engine.forget_list(id);
        store.commit(&engine.take_dirty());
        store.load(&chain).unwrap_or_else(|e| panic!("{round}, dropped: {e}"));
    }
}

/// D38: requests are not stored; a restarted engine asks again for what it lacked once
/// `tip_changed` runs. A ChainLock above the tip waits across the restart (D18).
#[test]
fn a_restarted_engine_asks_again() {
    let mut chain = FixtureChain::full();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff(281), &chain, config()).expect("Core's diff");
    let asked = engine.requests();
    assert!(!asked.is_empty());
    chain.tip = 300;
    let cl = clsig_at(311);
    assert_eq!(engine.verify_chainlock(&cl, &chain, config()), Err(LockOutcome::Pending));
    chain.tip = 311;

    let mut loaded = reloaded(&mut engine, &chain);
    assert!(loaded.requests().is_empty(), "nothing is asked before `tip_changed`");
    loaded.tip_changed(&chain, config()).expect("step 5");
    engine.tip_changed(&chain, config()).expect("step 5");
    let (ours, theirs) = (engine.requests(), loaded.requests());
    assert!(!theirs.is_empty());
    for r in &ours {
        assert!(theirs.contains(r), "{r:?} not asked again");
    }
    for r in &theirs {
        assert!(ours.contains(r), "{r:?} asked only after the restart");
    }
    assert!(loaded.persisted().contains_key(&mark_key("pending_cl")));
    assert_eq!(loaded.persisted(), engine.persisted());
}

/// D39: the snapshot is what the last retention published, the active tip and the newest list
/// held whole on the active chain, with the best ChainLock as step 6 verifies it (validator m6),
/// leaving a reader's earlier snapshot as it was; after a switch to a branch with no list of its
/// own above the fork, a list from below it.
#[test]
fn the_snapshot_is_the_last_retentions() {
    let mut chain = FixtureChain::full();
    let mut early = synced(&chain);
    early.tip_changed(&chain, config()).expect("step 5");
    assert_eq!(early.snapshot().best_chainlock, None);
    let before = early.snapshot();
    early.verify_chainlock(&clsig_at(chain.tip), &chain, config()).expect("Core verified it");
    assert_eq!(before.best_chainlock, None, "a reader's snapshot does not move");
    let best = Some((chain.tip, chain.hash(chain.tip)));
    assert_eq!(early.snapshot().best_chainlock, best, "a new best lock is published at once");

    let mut engine = synced_at_tip(&chain);
    let snapshot = engine.snapshot();
    let tip = chain.active_tip().expect("held");
    assert_eq!(snapshot.tip, Some(tip));
    assert_eq!(snapshot.best_chainlock, Some((chain.tip, chain.hash(chain.tip))));
    let list = snapshot.list.as_ref().expect("the tip's list");
    assert_eq!((list.height, list.block_hash), (chain.tip, chain.hash(chain.tip)));
    assert!(list.quorums.iter().all(|(_, s)| s.is_some()), "step 5 ran on the tip's set");
    assert_eq!(engine.latest_masternode_list(), snapshot.list);

    chain.fork = Some(300);
    chain.fork_active = true;
    assert_eq!(engine.snapshot(), snapshot, "published only by retention");
    engine.tip_changed(&chain, config()).expect("step 5");
    let moved = engine.snapshot();
    assert_eq!(moved.tip, Some(chain.active_tip().expect("held")));
    let list = moved.list.as_ref().expect("a list below the fork");
    assert!(list.height <= 300, "{}", list.height);
    assert_eq!(list.block_hash, chain.hash(list.height));
}

/// D38, Opus s3 r1 nits 1–2: `load` re-runs steps 2–4 on a whole list. The tip's list stored as
/// the block below's, its roots its own, carries another height's CbTx; an authenticated set
/// naming a changed commitment of the same quorum, held as a row (so every set leaf is a row),
/// is not its CbTx's set.
#[test]
fn a_list_another_blocks_coinbase_roots_is_corrupt() {
    let chain = FixtureChain::full();
    let rows = synced_at_tip(&chain).persisted();
    let c = cfs();
    let corrupt = |store: &Store| corrupt_table(store, &chain);
    let tip_list = rows[&list_key(chain.tip, &chain.hash(chain.tip))].clone();
    let below = list_key(chain.tip - 1, &chain.hash(chain.tip - 1));
    let store = Store::of(&rows);
    store.damage(c.lists, None, &below, &tip_list);
    assert_eq!(corrupt(&store), "mn_lists");

    // The tip list: flags (authenticated, whole), then its set row's hash; the set: a one-byte
    // count, then (type ‖ quorum hash ‖ leaf) each; a row is its meta, then its commitment
    // (version, type, quorum hash, ..).
    assert_eq!(tip_list[0], 3, "authenticated and whole");
    let set = &rows[&[b"S".as_slice(), &tip_list[1..33]].concat()];
    let (quorum_hash, leaf) = (&set[2..34], &set[34..66]);
    let row = &rows[&[b"Q".as_slice(), leaf].concat()];
    let at = row.windows(32).position(|w| w == quorum_hash).expect("its quorum hash") - 3;
    let mut qc = Commitment::decode(&mut &row[at..]).expect("its commitment");
    qc.quorum_vvec_hash = QuorumVvecHash::from([7; 32]);
    let changed = crate::convert::commitment_hash(&qc);
    let mut value = row[..at].to_vec();
    qc.encode(&mut value);
    let mut renamed = set.clone();
    renamed[34..66].copy_from_slice(&changed);
    let store = Store::of(&rows);
    store.damage(c.quorums, None, &[b"Q".as_slice(), &changed].concat(), &value);
    assert!(store.load(&chain).is_ok(), "a row no set names");
    store.rename_set(&list_key(chain.tip, &chain.hash(chain.tip)), &tip_list, &renamed);
    assert_eq!(corrupt(&store), "mn_lists");
}

/// D38, Opus s3 r1 nit 3 (mutant M3): an `Unverified` verdict reloads as itself, not as one step
/// 5 has yet to run.
#[test]
fn an_unverified_verdict_survives_a_restart() {
    let chain = FixtureChain::full();
    let mut engine = synced_at_tip(&chain);
    let (qc, _) = core_quorums(CHAIN_LOCKS)
        .iter()
        .rev()
        .find_map(|q| engine.quorum(llmq(CHAIN_LOCKS), &QuorumHash::from(q.hash)))
        .expect("a held quorum");
    engine.set_state(&qc, QuorumState::Unverified);
    let loaded = reloaded(&mut engine, &chain);
    assert_eq!(
        loaded.quorum(qc.llmq_type, &qc.quorum_hash).map(|(_, s)| s),
        Some(QuorumState::Unverified)
    );
}

/// BL-379, BL-382 (D45, D46): a row put since the last `take_dirty` is written alone, and once
/// retention prunes it, deleted. The rows kept are those a restart verifies with: with cycle 264's
/// `qrinfo` applied (its rows 240, 216, 192 and 168) and cycle 288's commitments waiting, the
/// reloaded engine verifies them once 288's own row 264 comes, reading 240 and 216 from the store.
#[test]
fn a_pruned_row_is_deleted_and_the_kept_rows_verify_after_a_restart() {
    let chain = FixtureChain::full();
    let rotated = llmq(ROTATED);
    let mut engine = stepped(&chain);
    engine.apply_qrinfo(&qrinfo(284, &[]), &chain, config()).expect("Core's qrinfo");
    let store = Store::new();
    store.commit(&engine.take_dirty());
    engine.hold_row(rotated, chain.hash(48), Default::default());
    let key = [&[b'R', ROTATED][..], chain.hash(48).as_bytes()].concat();
    let ops = |w: &MnWrites| w.ops().map(|(k, v)| (k.to_vec(), v.is_some())).collect::<Vec<_>>();
    let w = engine.take_dirty();
    assert_eq!(ops(&w), [(key.clone(), true)]);
    store.commit(&w);
    engine.tip_changed(&chain, config()).expect("step 5");
    let w = engine.take_dirty();
    assert!(ops(&w).contains(&(key, false)), "{:?}", ops(&w));
    store.commit(&w);
    store.assert_holds(&chain, &mut engine);
    let mut loaded = store.load(&chain).expect("its own rows");
    loaded.tip_changed(&chain, config()).expect("step 5");
    let cycle = core_quorums(ROTATED).into_iter().filter(|q| q.height >= 288).collect::<Vec<_>>();
    assert!(!cycle.is_empty());
    for q in &cycle {
        let state = state(&loaded, ROTATED, q.hash);
        assert_eq!(
            state,
            QuorumState::Pending(RequestKey::QrInfo(chain.hash(288))),
            "{}",
            q.height
        );
    }
    let reply = qrinfo(288, &[QRINFO_BASE]);
    loaded.hold_candidate(rotated, chain.hash(264), crate::convert::snapshot(&reply.snapshot_h_c));
    loaded.tip_changed(&chain, config()).expect("step 5");
    for q in &cycle {
        let state = state(&loaded, ROTATED, q.hash);
        assert!(matches!(state, QuorumState::Verified(_)), "{}: {state:?}", q.height);
    }
}

/// BL-380, D48: a record not replaced stores no `held_to`; `load` takes it from the newest
/// authenticated set holding it on the retained tip's chain, as retention marks it, so a reloaded
/// engine holds every record's `held_to` the live one does, replaced or not, at the tip and mid-way.
#[test]
fn a_reloaded_engine_holds_each_records_held_to() {
    let mut chain = FixtureChain::full();
    let mut engine = synced_at_tip(&chain);
    assert!(engine.held_tos().values().any(Option::is_some));
    assert_eq!(reloaded(&mut engine, &chain).held_tos(), engine.held_tos());
    chain.tip = 290;
    let mut engine = applied(&chain, FIRST, FIRST + 1..=290);
    assert_eq!(reloaded(&mut engine, &chain).held_tos(), engine.held_tos());
}

/// D38, D48 (Opus s5 r2 n1): with no authenticated list at or above the horizon, the newest set
/// below it holding a record is marked and dropped by the same retention, so the record's row
/// stores that set's height, not the sets' (flags are read after the drops), and reloads it.
#[test]
fn a_holder_dropped_by_the_retention_that_marked_it_reloads_its_height() {
    let chain = FixtureChain::full();
    let donor = synced_at_tip(&chain);
    let q = &core_quorums(ROTATED)[0];
    let (qc, _) = donor.quorum(llmq(ROTATED), &QuorumHash::from(q.hash)).expect("held");
    let mut deep = (*qc).clone();
    deep.quorum_hash = QuorumHash::from([6; 32]);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let horizon = chain.tip - config().h_keep();
    let (a, b) = (horizon - 20, horizon - 10);
    for h in [a, b] {
        engine.hold_set(chain.id(h), h, chain.hash(h), std::slice::from_ref(&deep));
    }
    engine.pin_list(chain.id(a));
    // The newest whole list, kept, is above the horizon but unauthenticated: no `first`.
    engine.hold_set(chain.id(chain.tip), chain.tip, chain.hash(chain.tip), &[]);
    engine.forget_authentication(chain.id(chain.tip));
    engine.tip_changed(&chain, config()).expect("step 5");
    let hash = crate::convert::commitment_hash(&deep);
    let live = engine.held_tos();
    assert_eq!(live[&hash], Some(b), "marked from b, which this retention dropped");
    assert!(!engine.held_by_sets()[&hash], "a lists' recompute would read a");
    let store = Store::new();
    store.commit(&engine.take_dirty());
    assert_eq!(store.load(&chain).expect("its own rows").held_tos(), live);
    store.assert_holds(&chain, &mut engine);
}

/// D38, D48 (Opus s5 r2 n2): `load` recomputes the sets' `held_to` on the tip retention ran on,
/// so a store whose chain moved onto a fork with no call between still loads the live values.
#[test]
fn a_store_loads_its_held_tos_after_the_chain_moved_without_a_call() {
    let mut chain = FixtureChain::full();
    chain.fork = Some(300);
    let mut engine = synced_at_tip(&chain);
    let q = &core_quorums(ROTATED)[0];
    let (qc, _) = engine.quorum(llmq(ROTATED), &QuorumHash::from(q.hash)).expect("held");
    let mut forked = (*qc).clone();
    forked.quorum_hash = QuorumHash::from([9; 32]);
    let fork_tip = BlockId(FORK + chain.tip);
    engine.hold_set(fork_tip, chain.tip, chain.fork_hash(chain.tip), std::slice::from_ref(&forked));
    engine.tip_changed(&chain, config()).expect("step 5");
    let store = Store::new();
    store.commit(&engine.take_dirty());
    store.assert_holds(&chain, &mut engine);
    chain.fork_active = true;
    store.assert_holds(&chain, &mut engine);
}
