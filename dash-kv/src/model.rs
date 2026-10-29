//! `kv_model`: a differential check of a [`KvEngine`] against a reference model.
//!
//! A byte string decodes into a sequence of [`Op`]s (puts, deletes and range deletes into a
//! pending batch, commits, barriers, reads, scans and snapshot reads). [`check`] applies them to
//! the engine and to plain `BTreeMap`s and fails on the first observable difference. Keys come
//! from a tiny alphabet so that overwrites, deletes of live keys and shared prefixes are frequent.
//! Used by the unit tests of every engine and by the `kv_model` model target in `node-fuzz`.
//!
//! [`check_with`] takes two optional hooks:
//! - **reopen** (RocksDB): close the engine and open the same database again. Every committed
//!   batch, buffered or durable, must still be there (buffered batches survive a process exit);
//!   this exercises WAL replay and the schema check on open.
//! - **power loss** (in-memory engine only): drop all but a prefix of the batches committed
//!   buffered since the last barrier. The model and [`MemoryEngine`](crate::MemoryEngine)
//!   implement the same rule, so this only checks that they agree; it says nothing about a real
//!   engine's recovery or about any client's barrier placement (ST3a's `FaultyEngine` and power-
//!   loss harness do that).
//! - **subset power loss** (ST3a's `FaultyEngine`): keep an arbitrary subset of those batches,
//!   the engine contract since DEC-62 (bit `i % 8` of the op's byte keeps batch `i`).

use std::collections::BTreeMap;

use crate::engine::{Cf, Commit, KvBatch, KvEngine, KvRead};
use crate::memory::{BatchOp, Ops};
use crate::schema::{CfProfile, CfSpec, DbSchema, SCHEMA_KEY, schema_record};

/// The schema the model runs against: `meta` plus one column family of each other profile.
pub static MODEL_SCHEMA: DbSchema = DbSchema {
    name: "kv-model",
    version: 1,
    cfs: &[
        CfSpec::new("meta", CfProfile::Ordered),
        CfSpec::new("point", CfProfile::PointLookup),
        CfSpec::new("packed", CfProfile::Compressed),
    ],
};

/// One step of a model run.
#[derive(Debug, Clone)]
pub enum Op {
    Put(Cf, Vec<u8>, Vec<u8>),
    Delete(Cf, Vec<u8>),
    /// `KvBatch::delete_range(cf, start, end)`; `start >= end` deletes nothing.
    DeleteRange(Cf, Vec<u8>, Vec<u8>),
    Write(Commit),
    SyncWal,
    FlushMemtables,
    Get(Cf, Vec<u8>),
    MultiGet(Cf, Vec<Vec<u8>>),
    Scan(Cf, Vec<u8>),
    TakeSnapshot,
    SnapshotGet(Cf, Vec<u8>),
    SnapshotScan(Cf, Vec<u8>),
    DropSnapshot,
    /// Power loss: the prefix hook keeps `n % 4` of the buffered batches; the subset hook keeps
    /// batch `i` iff bit `i % 8` of `n` is set (ignored without a hook).
    PowerLoss(u8),
    /// Close and reopen the engine (only the pending batch and the snapshot without a hook).
    Reopen,
}

struct Bytes<'a>(&'a [u8]);

impl Bytes<'_> {
    fn byte(&mut self) -> Option<u8> {
        let (&b, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(b)
    }

    fn cf(&mut self) -> Option<Cf> {
        Some(Cf::new(u16::from(self.byte()?) % MODEL_SCHEMA.cfs.len() as u16))
    }

    /// A column family other than `meta`: a range there could delete the schema record, which
    /// no caller does.
    fn data_cf(&mut self) -> Option<Cf> {
        let data = MODEL_SCHEMA.cfs.len() as u16 - 1;
        Some(Cf::new(1 + u16::from(self.byte()?) % data))
    }

    /// 0–3 bytes from {0x00, 0x01, 0x7f, 0xff}: collisions and prefix edges are common.
    fn key(&mut self) -> Option<Vec<u8>> {
        const ALPHABET: [u8; 4] = [0x00, 0x01, 0x7f, 0xff];
        let b = self.byte()?;
        let len = usize::from(b & 3);
        let mut key = Vec::with_capacity(len);
        for i in 0..len {
            key.push(ALPHABET[usize::from((b >> (2 + 2 * i)) & 3)]);
        }
        Some(key)
    }

    fn value(&mut self) -> Option<Vec<u8>> {
        let len = usize::from(self.byte()? % 8);
        let fill = self.byte()?;
        Some(vec![fill; len])
    }
}

/// Decodes a byte string into ops. Every byte string is valid; trailing bytes that do not form a
/// whole op are ignored.
pub fn decode_ops(data: &[u8]) -> Vec<Op> {
    let mut bytes = Bytes(data);
    let mut ops = Vec::new();
    while let Some(op) = decode_op(&mut bytes) {
        ops.push(op);
    }
    ops
}

fn decode_op(b: &mut Bytes) -> Option<Op> {
    Some(match b.byte()? % 16 {
        0..=4 => Op::Put(b.cf()?, b.key()?, b.value()?),
        5 => Op::Delete(b.cf()?, b.key()?),
        6 => match b.byte()? % 2 {
            0 => Op::Delete(b.cf()?, b.key()?),
            _ => Op::DeleteRange(b.data_cf()?, b.key()?, b.key()?),
        },
        7 => Op::Write(Commit::Buffered),
        8 => Op::Write(Commit::Durable),
        9 => {
            if b.byte()? % 2 == 0 {
                Op::SyncWal
            } else {
                Op::FlushMemtables
            }
        }
        10 => Op::Get(b.cf()?, b.key()?),
        11 => {
            let cf = b.cf()?;
            let n = b.byte()? % 4;
            Op::MultiGet(cf, (0..n).map(|_| b.key()).collect::<Option<_>>()?)
        }
        12 => Op::Scan(b.cf()?, b.key()?),
        13 => match b.byte()? % 8 {
            0 | 1 => Op::PowerLoss(b.byte()?),
            2 => Op::Reopen,
            _ => Op::TakeSnapshot,
        },
        14 => {
            if b.byte()? % 2 == 0 {
                Op::SnapshotGet(b.cf()?, b.key()?)
            } else {
                Op::SnapshotScan(b.cf()?, b.key()?)
            }
        }
        _ => Op::DropSnapshot,
    })
}

/// A deterministic byte string for seeded tests (xorshift64*).
pub fn pseudo_random_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|_| {
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            (x.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 56) as u8
        })
        .collect()
}

type State = Vec<BTreeMap<Vec<u8>, Vec<u8>>>;

fn apply(state: &mut State, ops: &Ops) {
    for op in ops {
        match op {
            BatchOp::Put(cf, k, v) => {
                state[cf.index()].insert(k.clone(), v.clone());
            }
            BatchOp::Delete(cf, k) => {
                state[cf.index()].remove(k);
            }
            BatchOp::DeleteRange(cf, start, end) => {
                state[cf.index()].retain(|k, _| k < start || k >= end);
            }
        }
    }
}
type Entries = Vec<(Vec<u8>, Vec<u8>)>;

fn model_scan(state: &State, cf: Cf, prefix: &[u8]) -> Entries {
    state[cf.index()]
        .range(prefix.to_vec()..)
        .take_while(|(k, _)| k.starts_with(prefix))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn engine_scan(db: &impl KvRead, cf: Cf, prefix: &[u8]) -> Result<Entries, String> {
    db.iter_prefix(cf, prefix).collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// Simulates a power loss that keeps the first `keep` buffered batches.
pub type PowerLossHook<'a, E> = &'a dyn Fn(&E, usize);
/// Simulates a power loss that keeps the buffered batches `keep` marks, in commit order.
pub type SubsetPowerLossHook<'a, E> = &'a dyn Fn(&E, &[bool]);

/// Optional hooks of [`check_with`].
pub struct Hooks<'a, E> {
    pub power_loss: Option<PowerLossHook<'a, E>>,
    /// Takes precedence over `power_loss`.
    pub power_loss_subset: Option<SubsetPowerLossHook<'a, E>>,
    /// Closes the engine and opens the same database again.
    pub reopen: Option<&'a dyn Fn(E) -> E>,
}

impl<E> Default for Hooks<'_, E> {
    fn default() -> Self {
        Self {
            power_loss: None,
            power_loss_subset: None,
            reopen: None,
        }
    }
}

/// The reference model: committed state and what a power loss could keep of it.
struct Model {
    state: State,
    /// The state at the last barrier.
    durable: State,
    /// Batches committed buffered since the last barrier.
    buffered: Vec<Ops>,
}

/// Runs `ops` against `engine`, which must be a freshly created database of [`MODEL_SCHEMA`]
/// (holding only its schema record), and against the reference model. `PowerLoss` ops are
/// skipped; `Reopen` only drops the pending batch and the snapshot.
pub fn check<E: KvEngine>(engine: E, ops: &[Op]) -> Result<(), String> {
    check_with(engine, ops, Hooks::default()).map(drop)
}

/// As [`check`], with the given hooks; returns the (possibly reopened) engine.
pub fn check_with<E: KvEngine>(
    mut engine: E,
    ops: &[Op],
    hooks: Hooks<'_, E>,
) -> Result<E, String> {
    let mut model = Model::new();
    for (i, (start, segment)) in segments(ops).enumerate() {
        if i > 0
            && let Some(reopen) = hooks.reopen
        {
            engine = reopen(engine);
        }
        let loss = match (hooks.power_loss_subset, hooks.power_loss) {
            (Some(h), _) => Some(Loss::Subset(h)),
            (None, Some(h)) => Some(Loss::Prefix(h)),
            (None, None) => None,
        };
        run_segment(&engine, start, segment, &mut model, loss)?;
    }
    model.compare(&engine)?;
    Ok(engine)
}

/// The runs of ops between `Reopen`s, with the index of each run's first op.
fn segments(ops: &[Op]) -> impl Iterator<Item = (usize, &[Op])> {
    let mut start = 0;
    ops.split(|op| matches!(op, Op::Reopen)).map(move |segment| {
        let at = start;
        start += segment.len() + 1;
        (at, segment)
    })
}

impl Model {
    fn new() -> Self {
        let mut state: State = vec![BTreeMap::new(); MODEL_SCHEMA.cfs.len()];
        state[DbSchema::META.index()].insert(SCHEMA_KEY.to_vec(), schema_record(&MODEL_SCHEMA));
        Self {
            durable: state.clone(),
            state,
            buffered: Vec::new(),
        }
    }

    /// Compares every column family in full.
    fn compare(&self, engine: &impl KvRead) -> Result<(), String> {
        for cf in MODEL_SCHEMA.all_cfs() {
            if engine_scan(engine, cf, &[])? != model_scan(&self.state, cf, &[]) {
                return Err(format!("final contents of `{}` differ", MODEL_SCHEMA.spec(cf).name));
            }
        }
        Ok(())
    }
}

enum Loss<'a, E> {
    Prefix(PowerLossHook<'a, E>),
    Subset(SubsetPowerLossHook<'a, E>),
}

/// Runs ops (none of them `Reopen`) on one open engine. The pending batch and the snapshot do
/// not outlive the segment.
fn run_segment<E: KvEngine>(
    engine: &E,
    first: usize,
    ops: &[Op],
    model: &mut Model,
    power_loss: Option<Loss<'_, E>>,
) -> Result<(), String> {
    assert_eq!(engine.schema(), &MODEL_SCHEMA, "the model runs on MODEL_SCHEMA");
    // Ops not yet committed, applied to the model only on `Write`.
    let mut pending: Ops = Vec::new();
    let mut batch = engine.batch();
    let mut snapshot: Option<(E::Snapshot<'_>, State)> = None;

    for (i, op) in ops.iter().enumerate() {
        let i = first + i;
        let fail = |what: String| Err(format!("op {i} {op:?}: {what}"));
        let Model {
            state,
            durable,
            buffered,
        } = &mut *model;
        match op {
            Op::Put(cf, k, v) => {
                batch.put(*cf, k, v);
                pending.push(BatchOp::Put(*cf, k.clone(), v.clone()));
            }
            Op::Delete(cf, k) => {
                batch.delete(*cf, k);
                pending.push(BatchOp::Delete(*cf, k.clone()));
            }
            Op::DeleteRange(cf, start, end) => {
                batch.delete_range(*cf, start, end);
                // An empty range records nothing (`KvBatch::delete_range`).
                if start < end {
                    pending.push(BatchOp::DeleteRange(*cf, start.clone(), end.clone()));
                }
            }
            Op::Write(commit) => {
                if batch.len() != pending.len() {
                    return fail(format!("batch len {} != {}", batch.len(), pending.len()));
                }
                let full = std::mem::replace(&mut batch, engine.batch());
                if let Err(e) = engine.write(full, *commit) {
                    return fail(e.to_string());
                }
                apply(state, &pending);
                buffered.push(std::mem::take(&mut pending));
                if *commit == Commit::Durable {
                    *durable = state.clone();
                    buffered.clear();
                }
            }
            Op::SyncWal | Op::FlushMemtables => {
                let r = if matches!(op, Op::SyncWal) {
                    engine.sync_wal()
                } else {
                    engine.flush_memtables()
                };
                r.map_err(|e| e.to_string())?;
                *durable = state.clone();
                buffered.clear();
            }
            Op::PowerLoss(n) => {
                let Some(power_loss) = &power_loss else {
                    continue;
                };
                // Uncommitted batch contents and snapshots do not survive.
                batch = engine.batch();
                pending.clear();
                snapshot = None;
                let keep: Vec<bool> = match power_loss {
                    Loss::Prefix(hook) => {
                        let keep = usize::from(*n % 4);
                        hook(engine, keep);
                        (0..buffered.len()).map(|i| i < keep).collect()
                    }
                    Loss::Subset(hook) => {
                        let keep: Vec<bool> =
                            (0..buffered.len()).map(|i| (n >> (i % 8)) & 1 == 1).collect();
                        hook(engine, &keep);
                        keep
                    }
                };
                *state = durable.clone();
                for (ops, _) in buffered.iter().zip(&keep).filter(|(_, k)| **k) {
                    apply(state, ops);
                }
                *durable = state.clone();
                buffered.clear();
            }
            Op::Reopen => unreachable!("segments end at Reopen"),
            Op::Get(cf, k) => {
                let got = engine.get(*cf, k).map_err(|e| e.to_string())?;
                if got.as_ref() != state[cf.index()].get(k) {
                    return fail(format!("got {got:?}"));
                }
            }
            Op::MultiGet(cf, keys) => {
                let refs: Vec<&[u8]> = keys.iter().map(Vec::as_slice).collect();
                let got = engine.multi_get(*cf, &refs).map_err(|e| e.to_string())?;
                let want: Vec<_> = keys.iter().map(|k| state[cf.index()].get(k).cloned()).collect();
                if got != want {
                    return fail(format!("got {got:?}, want {want:?}"));
                }
            }
            Op::Scan(cf, prefix) => {
                let got = engine_scan(engine, *cf, prefix)?;
                if got != model_scan(state, *cf, prefix) {
                    return fail(format!("scan got {got:?}"));
                }
            }
            Op::TakeSnapshot => snapshot = Some((engine.snapshot(), state.clone())),
            Op::SnapshotGet(cf, k) => {
                if let Some((snap, frozen)) = &snapshot {
                    let got = snap.get(*cf, k).map_err(|e| e.to_string())?;
                    if got.as_ref() != frozen[cf.index()].get(k) {
                        return fail(format!("snapshot got {got:?}"));
                    }
                }
            }
            Op::SnapshotScan(cf, prefix) => {
                if let Some((snap, frozen)) = &snapshot {
                    let got = engine_scan(snap, *cf, prefix)?;
                    if got != model_scan(frozen, *cf, prefix) {
                        return fail(format!("snapshot scan got {got:?}"));
                    }
                }
            }
            Op::DropSnapshot => snapshot = None,
        }
    }
    Ok(())
}

/// `KvBatch::delete_range` on `engine` (over [`MODEL_SCHEMA`]), directly: the range is
/// `[start, end)` in one column family, it deletes a put earlier in its batch but not a later one,
/// and an empty or reversed range records nothing and does not fail the write.
pub fn check_delete_range<E: KvEngine>(engine: &E) {
    let (point, packed) = (MODEL_SCHEMA.cf("point").unwrap(), MODEL_SCHEMA.cf("packed").unwrap());
    let mut b = engine.batch();
    for key in [&[1][..], &[2], &[2, 0], &[3], &[4, 0xff], &[5]] {
        b.put(point, key, b"v");
    }
    b.put(packed, &[3], b"v");
    engine.write(b, Commit::Buffered).unwrap();

    let mut b = engine.batch();
    b.put(point, &[4], b"earlier");
    b.delete_range(point, &[2], &[5]);
    b.put(point, &[3], b"later");
    b.delete_range(point, &[5], &[5]);
    b.delete_range(point, &[6], &[1]);
    assert_eq!(b.len(), 3, "the empty and the reversed range record nothing");
    engine.write(b, Commit::Buffered).unwrap();

    let scan = |cf| -> Entries { engine.iter_prefix(cf, &[]).map(Result::unwrap).collect() };
    let entry = |k: &[u8], v: &[u8]| (k.to_vec(), v.to_vec());
    assert_eq!(scan(point), [entry(&[1], b"v"), entry(&[3], b"later"), entry(&[5], b"v")]);
    assert_eq!(scan(packed), [entry(&[3], b"v")], "another column family is untouched");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Commit, KvBatch, KvEngine, KvRead};
    use crate::memory::MemoryEngine;

    #[test]
    fn memory_engine_matches_model() {
        for seed in 0..200 {
            let engine = MemoryEngine::new(&MODEL_SCHEMA);
            let ops = decode_ops(&pseudo_random_bytes(seed, 4096));
            check(engine, &ops).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        }
    }

    #[test]
    fn memory_engine_power_loss_matches_model() {
        let hook = |e: &MemoryEngine, keep: usize| e.simulate_power_loss(keep);
        let mut losses = 0;
        for seed in 0..200 {
            let engine = MemoryEngine::new(&MODEL_SCHEMA);
            let ops = decode_ops(&pseudo_random_bytes(seed, 4096));
            losses += ops.iter().filter(|o| matches!(o, Op::PowerLoss(_))).count();
            let hooks = Hooks {
                power_loss: Some(&hook),
                ..Hooks::default()
            };
            check_with(engine, &ops, hooks).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        }
        assert!(losses > 100, "the seeds exercise power loss ({losses})");
    }

    /// A power loss without a barrier loses buffered batches beyond the kept prefix.
    #[test]
    fn power_loss_drops_unsynced_suffix() {
        let engine = MemoryEngine::new(&MODEL_SCHEMA);
        let cf = MODEL_SCHEMA.cf("point").unwrap();
        for (i, commit) in [Commit::Buffered, Commit::Durable, Commit::Buffered, Commit::Buffered]
            .into_iter()
            .enumerate()
        {
            let mut b = engine.batch();
            b.put(cf, &[i as u8], b"v");
            engine.write(b, commit).unwrap();
        }
        engine.simulate_power_loss(1);
        let keys: Vec<Vec<u8>> = engine.iter_prefix(cf, &[]).map(|e| e.unwrap().0).collect();
        assert_eq!(keys, [vec![0u8], vec![1], vec![2]]);
    }

    #[test]
    fn memory_delete_range() {
        check_delete_range(&MemoryEngine::new(&MODEL_SCHEMA));
    }

    #[test]
    fn decoding_is_total() {
        for seed in 0..50 {
            let data = pseudo_random_bytes(seed, 257);
            for end in 0..data.len() {
                decode_ops(&data[..end]);
            }
        }
    }
}
