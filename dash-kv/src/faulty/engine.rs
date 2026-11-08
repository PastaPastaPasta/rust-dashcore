//! `FaultyEngine`: the in-memory engine with subset semantics (§2.4), or, per engine, prefix
//! semantics ([`Recovery::Prefix`], SQLite's WAL).
//!
//! State: a concrete durable base, then the batches since, oldest first. The first
//! `durable_len` batches are durable (a completed barrier covered them); the rest are pending.
//! Each batch has a survival [`Bit`]: present (written in this epoch), or a lazy cell created by
//! a power loss. Reads resolve per key, newest first, deciding only the bits of batches that
//! wrote the key (§2.4.3, rule L1). A barrier carries undecided bits into the durable state
//! without deciding them (L2).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::engine::{Cf, Commit, KvEngine, KvRead};
use crate::error::{ErrorKind, Result, StoreError};
use crate::fail_stop::{Failure, failure_of, refusal};
use crate::faulty::check::{InvariantId, Peek, Scope, Violation, hex};
use crate::faulty::world::{
    CommitKind, CrashKind, Ctx, FaultKind, FaultOutcome, Fnv, Mark, MarkKind, OpKind, Start,
    TraceEntry, VarId, VarKind, World,
};
use crate::memory::{BatchOp, Map, MemoryBatch, MemorySnapshot, Ops, apply};
use crate::schema::{DbSchema, SCHEMA_KEY, schema_record};

const ENGINE: &str = "faulty";

/// Whether a batch survived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Bit {
    /// Written in this epoch (or kept by a process kill): present.
    Present,
    Var(VarId),
    /// W2: each column family survives on its own.
    PerCf(Arc<[(Cf, VarId)]>),
    /// W1: the batch survives iff fewer than `len - index` of the `len` batches are lost.
    Prefix {
        var: VarId,
        index: u32,
        len: u32,
    },
}

/// One committed batch.
#[derive(Debug)]
pub(crate) struct Layer {
    pub id: u64,
    pub ops: Ops,
    /// The last operation on each key a put or delete names.
    last: BTreeMap<(Cf, Vec<u8>), Option<Vec<u8>>>,
    /// Range deletes `[start, end)`: they delete every key in range that `last` does not name (a
    /// key named before the range is recorded there as deleted).
    ranges: Vec<(Cf, Vec<u8>, Vec<u8>)>,
    pub bit: Bit,
    /// The trace length when it was appended (for D1(f) scopes).
    trace_pos: usize,
    finalizer: bool,
    bytes: u64,
}

/// What a range delete leaves.
const DELETED: &Option<Vec<u8>> = &None;

impl Layer {
    fn new(id: u64, ops: Ops, trace_pos: usize, finalizer: bool) -> Self {
        let mut last = BTreeMap::new();
        let mut ranges = Vec::new();
        let mut bytes = 0;
        for op in &ops {
            match op {
                BatchOp::Put(cf, k, v) => {
                    bytes += (k.len() + v.len()) as u64;
                    last.insert((*cf, k.clone()), Some(v.clone()));
                }
                BatchOp::Delete(cf, k) => {
                    bytes += k.len() as u64;
                    last.insert((*cf, k.clone()), None);
                }
                BatchOp::DeleteRange(cf, start, end) => {
                    bytes += (start.len() + end.len()) as u64;
                    let named = (*cf, start.clone())..(*cf, end.clone());
                    for (_, v) in last.range_mut(named) {
                        *v = None;
                    }
                    ranges.push((*cf, start.clone(), end.clone()));
                }
            }
        }
        Self {
            id,
            ops,
            last,
            ranges,
            bit: Bit::Present,
            trace_pos,
            finalizer,
            bytes,
        }
    }

    fn with_bit(&self, bit: Bit) -> Self {
        Self {
            id: self.id,
            ops: self.ops.clone(),
            last: self.last.clone(),
            ranges: self.ranges.clone(),
            bit,
            trace_pos: self.trace_pos,
            finalizer: self.finalizer,
            bytes: self.bytes,
        }
    }

    /// The batch's last word on a key: `None` if it neither names the key nor deletes a range
    /// holding it.
    fn op(&self, k: &(Cf, Vec<u8>)) -> Option<&Option<Vec<u8>>> {
        self.last.get(k).or_else(|| {
            let (cf, key) = k;
            self.ranges.iter().any(|(c, s, e)| c == cf && s <= key && key < e).then_some(DELETED)
        })
    }

    /// Whether the batch writes a key in `range` of `cf`.
    fn touches(&self, cf: Cf, range: &std::ops::Range<Vec<u8>>) -> bool {
        self.ops.iter().any(|op| match op {
            BatchOp::Put(c, k, _) | BatchOp::Delete(c, k) => *c == cf && range.contains(k),
            BatchOp::DeleteRange(c, s, e) => *c == cf && *s < range.end && range.start < *e,
        })
    }

    fn cfs(&self) -> BTreeSet<Cf> {
        self.ops
            .iter()
            .map(|op| match op {
                BatchOp::Put(cf, ..) | BatchOp::Delete(cf, _) | BatchOp::DeleteRange(cf, ..) => *cf,
            })
            .collect()
    }
}

/// What a power loss leaves of an engine's pending batches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Recovery {
    /// Any subset, each batch atomically (DEC-62): RocksDB's contract.
    #[default]
    Subset,
    /// A prefix in commit order: SQLite's WAL, which stops replaying at the first frame that
    /// fails its checksum (SB10, `sqlite_wal_prefix`). One lazy cell per power loss chooses
    /// how many of the newest pending batches were lost, as W1 does for every engine. Batches
    /// stay atomic: the `break_atomic` witness does not apply.
    Prefix,
}

/// The state of one database.
#[derive(Clone)]
pub(crate) struct EngineState {
    pub schema: &'static DbSchema,
    pub recovery: Recovery,
    base: Vec<Arc<Map>>,
    layers: Vec<Arc<Layer>>,
    durable_len: usize,
    failed: Option<Failure>,
    next_id: u64,
}

/// A bit's value for column family `cf`, deciding it if needed.
fn survives(bit: &Bit, cf: Cf, ctx: &mut Ctx) -> bool {
    match bit {
        Bit::Present => true,
        Bit::Var(v) => ctx.decide(*v) == 0,
        Bit::PerCf(vars) => {
            let v = vars.iter().find(|(c, _)| *c == cf).expect("a var per column family").1;
            ctx.decide(v) == 0
        }
        Bit::Prefix {
            var,
            index,
            len,
        } => *index < len - ctx.decide(*var),
    }
}

/// A bit's value without deciding it.
pub(crate) fn peek(bit: &Bit, cf: Cf, ctx_vars: &dyn Fn(VarId) -> Option<u32>) -> Option<bool> {
    match bit {
        Bit::Present => Some(true),
        Bit::Var(v) => ctx_vars(*v).map(|c| c == 0),
        Bit::PerCf(vars) => {
            let v = vars.iter().find(|(c, _)| *c == cf)?.1;
            ctx_vars(v).map(|c| c == 0)
        }
        Bit::Prefix {
            var,
            index,
            len,
        } => ctx_vars(*var).map(|c| *index < len - c),
    }
}

impl EngineState {
    pub fn new(schema: &'static DbSchema, recovery: Recovery) -> Self {
        schema.validate();
        let mut base: Vec<Arc<Map>> = vec![Arc::default(); schema.cfs.len()];
        Arc::make_mut(&mut base[DbSchema::META.index()])
            .insert(SCHEMA_KEY.to_vec(), schema_record(schema));
        Self {
            schema,
            recovery,
            base,
            layers: Vec::new(),
            durable_len: 0,
            failed: None,
            next_id: 0,
        }
    }

    /// The key's value.
    fn get(&self, cf: Cf, key: &[u8], ctx: &mut Ctx) -> Option<Vec<u8>> {
        let k = (cf, key.to_vec());
        for layer in self.layers.iter().rev() {
            ctx.work.history_scanned += 1;
            if let Some(v) = layer.op(&k)
                && survives(&layer.bit, cf, ctx)
            {
                return v.clone();
            }
        }
        self.base[cf.index()].get(key).cloned()
    }

    fn scan(&self, cf: Cf, prefix: &[u8], ctx: &mut Ctx) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut keys: BTreeSet<Vec<u8>> = self.base[cf.index()]
            .range(prefix.to_vec()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, _)| k.clone())
            .collect();
        for layer in &self.layers {
            for (c, k) in layer.last.keys() {
                if *c == cf && k.starts_with(prefix) {
                    keys.insert(k.clone());
                }
            }
        }
        ctx.work.scanned_entries += keys.len() as u64;
        keys.into_iter().filter_map(|k| self.get(cf, &k, ctx).map(|v| (k, v))).collect()
    }

    /// Promotes every batch to durable (a completed barrier) and folds present ones into the
    /// base.
    fn barrier(&mut self, ctx: &Ctx) {
        self.durable_len = self.layers.len();
        self.settle(ctx);
        self.fold();
    }

    /// Replaces durable batches whose bit this branch has decided: survivors become present,
    /// lost ones are dropped. Only the current branch's world changes (images are copies).
    fn settle(&mut self, ctx: &Ctx) {
        let mut i = 0;
        while i < self.durable_len {
            let decided = |v: VarId| ctx.peek(v);
            let layer = &self.layers[i];
            let all = layer
                .cfs()
                .into_iter()
                .map(|cf| peek(&layer.bit, cf, &decided))
                .collect::<Option<Vec<bool>>>();
            match all {
                Some(v) if layer.bit != Bit::Present && v.iter().all(|&b| b) => {
                    self.layers[i] = Arc::new(layer.with_bit(Bit::Present));
                }
                Some(v) if v.iter().all(|&b| !b) => {
                    self.layers.remove(i);
                    self.durable_len -= 1;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn fold(&mut self) {
        let n =
            self.layers[..self.durable_len].iter().take_while(|l| l.bit == Bit::Present).count();
        for layer in self.layers.drain(..n) {
            apply(&mut self.base, &layer.ops);
        }
        self.durable_len -= n;
    }

    /// Converts the state after a crash (§2.1): a process kill keeps everything pending; a power
    /// loss turns each pending present batch into a lazy survival cell, and what survives
    /// becomes durable.
    pub fn crash(&mut self, name: &str, kind: CrashKind, ctx: &mut Ctx) {
        self.failed = None;
        if kind == CrashKind::ProcessKill {
            return;
        }
        let w = ctx.cfg.witness();
        let pending: Vec<usize> = (self.durable_len..self.layers.len())
            .filter(|&i| self.layers[i].bit == Bit::Present)
            .collect();
        let epoch = ctx.epoch;
        // A batch appended before the crashed epoch began (a process kill kept it pending) is
        // marked `^` (R7a compares a branch that loses it with a recovery from the same input).
        let start = ctx.crashed_epoch_start();
        let outer: Vec<bool> = self.layers.iter().map(|l| l.trace_pos < start).collect();
        let mark = |o: bool| {
            if o {
                "^"
            } else {
                ""
            }
        };
        // The pending batches survive as a prefix: on a `Recovery::Prefix` engine at every power
        // loss, on a subset engine where the witness grants it. A `^` cell's inherited batches:
        // their survival, choice for choice, is that of the same cells at a crash before the
        // epoch's first event (CP3-3).
        let prefix_recovery = w.grant_prefix || self.recovery == Recovery::Prefix;
        let prefix = (prefix_recovery && !pending.is_empty()).then(|| {
            let inherited: Vec<usize> = pending.iter().copied().filter(|&i| outer[i]).collect();
            let len = pending.len() as u32;
            let var = ctx.new_var(
                VarKind::BatchPrefix,
                format!("{epoch}:{name}:prefix{}", mark(!inherited.is_empty())),
                len + 1,
            );
            if !inherited.is_empty() {
                // Choice c loses the last c batches: the epoch's own come last.
                let own = len - inherited.len() as u32;
                let start = (0..=len)
                    .map(|c| match c.saturating_sub(own) {
                        0 => Start::Intact,
                        k => Start::Choice(k),
                    })
                    .collect();
                ctx.inherit(var, start);
            }
            var
        });
        let survival = |ctx: &mut Ctx, var: VarId, i: usize| {
            if outer[i] {
                ctx.inherit(var, vec![Start::Intact, Start::Choice(1)]);
            }
            var
        };
        for (n, &i) in pending.iter().enumerate() {
            let layer = &self.layers[i];
            let m = mark(outer[i]);
            let bit = if let Some(var) = prefix {
                Bit::Prefix {
                    var,
                    index: n as u32,
                    len: pending.len() as u32,
                }
            } else if w.break_atomic && layer.cfs().len() > 1 {
                let vars: Vec<(Cf, VarId)> = layer
                    .cfs()
                    .into_iter()
                    .map(|cf| {
                        let name = format!("{epoch}:{name}:b{}:cf{}{m}", layer.id, cf.index());
                        let var = ctx.new_var(VarKind::BatchSurvives, name, 2);
                        (cf, survival(ctx, var, i))
                    })
                    .collect();
                Bit::PerCf(vars.into())
            } else {
                let var = ctx.new_var(
                    VarKind::BatchSurvives,
                    format!("{epoch}:{name}:b{}{m}", layer.id),
                    2,
                );
                Bit::Var(survival(ctx, var, i))
            };
            self.layers[i] = Arc::new(layer.with_bit(bit));
        }
        self.barrier(ctx);
    }

    /// The durable value of a key under the branch's decisions `dec` (D1's view, which never
    /// decides): `Unknown` while an undecided bit stands between it and the base.
    pub fn durable_get(
        &self,
        cf: Cf,
        key: &[u8],
        dec: &dyn Fn(VarId) -> Option<u32>,
    ) -> Peek<Option<Vec<u8>>> {
        let k = (cf, key.to_vec());
        for layer in self.layers[..self.durable_len].iter().rev() {
            if let Some(v) = layer.op(&k) {
                match peek(&layer.bit, cf, dec) {
                    Some(true) => return Peek::Known(v.clone()),
                    Some(false) => {}
                    None => return Peek::Unknown,
                }
            }
        }
        Peek::Known(self.base[cf.index()].get(key).cloned())
    }

    /// Every durable record that is present or may be: `(cf, key, value, known)`. A key whose
    /// presence is undecided yields every candidate value (newest first, down to the first
    /// decided-present one), each with `known = false`.
    pub fn durable_records(
        &self,
        dec: &dyn Fn(VarId) -> Option<u32>,
    ) -> Vec<(Cf, Vec<u8>, Vec<u8>, bool)> {
        let mut keys: BTreeSet<(Cf, Vec<u8>)> = BTreeSet::new();
        for cf in self.schema.all_cfs() {
            for k in self.base[cf.index()].keys() {
                keys.insert((cf, k.clone()));
            }
        }
        for layer in &self.layers[..self.durable_len] {
            keys.extend(layer.last.keys().cloned());
        }
        let mut out = Vec::new();
        for (cf, k) in keys {
            let key = (cf, k.clone());
            let mut known = true;
            let mut done = false;
            for layer in self.layers[..self.durable_len].iter().rev() {
                let Some(v) = layer.op(&key) else {
                    continue;
                };
                match peek(&layer.bit, cf, dec) {
                    Some(true) => {
                        if let Some(v) = v {
                            out.push((cf, k.clone(), v.clone(), known));
                        }
                        done = true;
                        break;
                    }
                    Some(false) => {}
                    None => {
                        known = false;
                        if let Some(v) = v {
                            out.push((cf, k.clone(), v.clone(), false));
                        }
                    }
                }
            }
            if !done && let Some(v) = self.base[cf.index()].get(&k) {
                out.push((cf, k.clone(), v.clone(), known));
            }
        }
        out
    }

    /// Bytes held, for a shared capacity: every batch, decided or not.
    pub fn bytes(&self) -> u64 {
        let base: u64 =
            self.base.iter().flat_map(|m| m.iter()).map(|(k, v)| (k.len() + v.len()) as u64).sum();
        base + self.layers.iter().map(|l| l.bytes).sum::<u64>()
    }

    /// The durable maps with every undecided bit taken as survived: what a check that must not
    /// decide sees as "possibly present" (R5's overlay is computed by the SUT's own reads).
    pub fn pending_count(&self) -> usize {
        self.layers.len() - self.durable_len
    }
}

/// D1(f): is a batch in `scopes` still pending when a finalizer is appended?
fn uncovered_scope(
    st: &EngineState,
    store: &str,
    scopes: &[Scope],
    all: &BTreeMap<&'static str, EngineState>,
    ctx: &Ctx,
) -> Option<String> {
    let last_mark = |kinds: &[MarkKind], filter: &dyn Fn(&Mark) -> bool| -> usize {
        ctx.trace
            .entries
            .iter()
            .rposition(|e| match e {
                TraceEntry::Mark {
                    mark,
                    epoch,
                    ..
                    // Across crashes: a batch a process kill left pending stays in its scope.
                } => {
                    let _ = epoch;
                    kinds.contains(&mark.kind()) && filter(mark)
                }
                _ => false,
            })
            .map_or(0, |p| p + 1)
    };
    let pending = |s: &EngineState| -> Vec<Arc<Layer>> { s.layers[s.durable_len..].to_vec() };
    for scope in scopes {
        let hit = match scope {
            Scope::SinceFlushStart => {
                let from = last_mark(&[MarkKind::FlushBegin, MarkKind::RecoveryBegin], &|_| true);
                pending(st)
                    .into_iter()
                    .find(|l| l.trace_pos >= from)
                    .map(|l| (store.to_string(), l))
            }
            Scope::SincePreviousLocator {
                store: sub,
            } => {
                let from = last_mark(
                    &[MarkKind::Locator],
                    &|m| matches!(m, Mark::Locator { store: s, .. } if s == sub),
                );
                all.get(sub).and_then(|s| {
                    pending(s)
                        .into_iter()
                        .find(|l| l.trace_pos >= from)
                        .map(|l| (sub.to_string(), l))
                })
            }
            Scope::Keys {
                cf,
                range,
            } => pending(st)
                .into_iter()
                .find(|l| l.touches(*cf, range))
                .map(|l| (store.to_string(), l)),
            Scope::Store {
                store: other,
            } => all
                .get(other)
                .and_then(|s| pending(s).into_iter().next().map(|l| (other.to_string(), l))),
            Scope::PreviousFinalizer => st
                .layers
                .iter()
                .enumerate()
                .rev()
                .find(|(_, l)| l.finalizer)
                .filter(|(i, _)| *i >= st.durable_len)
                .map(|(_, l)| (store.to_string(), l.clone())),
        };
        if let Some((s, l)) = hit {
            return Some(format!("{scope:?}: batch {} of `{s}` is pending", l.id));
        }
    }
    None
}

/// An engine of a [`World`]: a handle to its state there.
#[derive(Clone)]
pub struct FaultyEngine {
    world: World,
    name: &'static str,
    schema: &'static DbSchema,
}

fn injected(kind: FaultKind) -> StoreError {
    let k = match kind {
        FaultKind::NoSpace => ErrorKind::NoSpace,
        FaultKind::Io => ErrorKind::Io,
    };
    StoreError::engine(ENGINE, k, "injected failure")
}

fn frozen() -> StoreError {
    StoreError::engine(ENGINE, ErrorKind::Other, "the world is frozen")
}

impl FaultyEngine {
    pub(crate) fn new(world: World, name: &'static str, schema: &'static DbSchema) -> Self {
        Self {
            world,
            name,
            schema,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    /// The number of batches not yet covered by a completed barrier.
    pub fn pending(&self) -> usize {
        self.world.lock().stores.engines[self.name].pending_count()
    }

    /// The ids of the pending batches, oldest first (their survival variables are named
    /// `<epoch>:<name>:b<id>` after a power loss).
    pub fn pending_ids(&self) -> Vec<u64> {
        let w = self.world.lock();
        let st = &w.stores.engines[self.name];
        st.layers[st.durable_len..].iter().map(|l| l.id).collect()
    }

    /// The kind of the failure that stopped writes, if one has.
    pub fn failed(&self) -> Option<ErrorKind> {
        self.world.lock().stores.engines[self.name].failed.as_ref().map(|(k, _)| *k)
    }

    /// One barrier event: makes every batch durable unless a fault hits it.
    fn barrier_event(&self, op: OpKind) -> Result<()> {
        let id = {
            let mut guard = self.world.lock();
            let w = &mut *guard;
            let (_, fault) = w.ctx.begin_event(self.name, op).map_err(|_| frozen())?;
            let st = w.stores.engines.get_mut(self.name).expect("engine exists");
            let r = if let Some(first) = &st.failed {
                Err(refusal(ENGINE, first))
            } else if let Some(f) = fault {
                // Applied or not, no barrier happened; the outcome of every pending batch is
                // unknown.
                let err = injected(f.kind);
                st.failed = Some(failure_of(&err));
                Err(err)
            } else {
                st.barrier(&w.ctx);
                Ok(())
            };
            let id = w.ctx.end_event(self.name, op, String::new(), r.is_err());
            (id, r)
        };
        self.world.after_event(id.0);
        if id.1.is_ok() {
            self.world.after_barrier();
        }
        id.1
    }

    fn read<T>(&self, f: impl FnOnce(&EngineState, &mut Ctx) -> T) -> T {
        let mut guard = self.world.lock();
        let w = &mut *guard;
        f(&w.stores.engines[self.name], &mut w.ctx)
    }
}

fn hash_value(v: &Option<Vec<u8>>) -> u64 {
    let mut h = Fnv::default();
    match v {
        Some(v) => {
            h.write(&[1]);
            h.write(v);
        }
        None => h.write(&[0]),
    }
    h.0
}

impl KvRead for FaultyEngine {
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let fault = self
            .world
            .lock()
            .ctx
            .begin_read(self.name, OpKind::Read, || format!("get {}", hex(key)));
        if let Some(f) = fault {
            self.world.after_read_fault();
            return Err(injected(f.kind));
        }
        Ok(self.read(|st, ctx| {
            let v = st.get(cf, key, ctx);
            ctx.work.bytes_read += v.as_ref().map_or(0, |v| v.len() as u64);
            ctx.observe(self.name, || format!("get {cf:?} {}", hex(key)), hash_value(&v));
            v
        }))
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>> {
        keys.iter().map(|k| self.get(cf, k)).collect()
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        let fault = self
            .world
            .lock()
            .ctx
            .begin_read(self.name, OpKind::Read, || format!("scan {}", hex(prefix)));
        let failed = fault.map(|f| Err(injected(f.kind)));
        if failed.is_some() {
            self.world.after_read_fault();
            return failed.into_iter().collect::<Vec<_>>().into_iter();
        }
        let entries = self.read(|st, ctx| {
            let entries = st.scan(cf, prefix, ctx);
            let mut h = Fnv::default();
            for (k, v) in &entries {
                h.write(k);
                h.write(&[0xff]);
                h.write(v);
                ctx.work.bytes_read += v.len() as u64;
            }
            ctx.observe(self.name, || format!("scan {cf:?} {}", hex(prefix)), h.0);
            entries
        });
        entries.into_iter().map(Ok).collect::<Vec<_>>().into_iter()
    }
}

impl KvEngine for FaultyEngine {
    type Batch = MemoryBatch;
    type Snapshot<'a> = MemorySnapshot;

    fn schema(&self) -> &'static DbSchema {
        self.schema
    }

    fn batch(&self) -> MemoryBatch {
        MemoryBatch::default()
    }

    fn write(&self, batch: MemoryBatch, commit: Commit) -> Result<()> {
        let op = OpKind::Append(CommitKind::from(commit));
        let ops = batch.into_ops();
        let (id, r) = {
            let mut guard = self.world.lock();
            let w = &mut *guard;
            let (_, fault) = w.ctx.begin_event(self.name, op).map_err(|_| frozen())?;
            // D1(f): the scopes this batch finalizes must be covered by a completed barrier.
            let mut finalizer = false;
            if let Some(x) = w.ctx.extractor.clone().filter(|_| w.ctx.cfg.d1) {
                let mut scopes = Vec::new();
                for (cf, k, v) in ops.iter().filter_map(|op| match op {
                    BatchOp::Put(cf, k, v) => Some((cf, k, Some(v.as_slice()))),
                    BatchOp::Delete(cf, k) => Some((cf, k, None)),
                    // A range delete names no record (CH2's wipe finalizes nothing with one).
                    BatchOp::DeleteRange(..) => None,
                }) {
                    if let Some(s) = x.finalizes(self.name, *cf, k, v) {
                        finalizer = true;
                        scopes.extend(s);
                    }
                }
                if !scopes.is_empty() {
                    let st = &w.stores.engines[self.name];
                    if let Some(what) =
                        uncovered_scope(st, self.name, &scopes, &w.stores.engines, &w.ctx)
                    {
                        let v = Violation::new(
                            InvariantId::D1f,
                            format!("finalizer appended to `{}` while {what}", self.name),
                        )
                        .at(w.ctx.epoch, w.ctx.events);
                        w.ctx.d1.report(v);
                    }
                }
            }
            let trace_pos = w.ctx.trace.entries.len();
            let capacity = (w.ctx.cfg.shared_capacity).then_some(w.ctx.cfg.capacity).flatten();
            let st = w.stores.engines.get_mut(self.name).expect("engine exists");
            let id = st.next_id;
            st.next_id += 1;
            let layer = Layer::new(id, ops, trace_pos, finalizer);
            let over = capacity.is_some_and(|c| {
                w.stores.engine_bytes() + layer.bytes + w.stores.fs.max_usage() > c
            });
            let st = w.stores.engines.get_mut(self.name).expect("engine exists");
            let r = if let Some(first) = &st.failed {
                Err(refusal(ENGINE, first))
            } else if let Some(f) = fault.clone().or_else(|| {
                over.then_some(crate::faulty::world::Fault {
                    at: crate::faulty::world::EventSel::Index(0),
                    kind: FaultKind::NoSpace,
                    outcome: FaultOutcome::NotApplied,
                })
            }) {
                if f.outcome != FaultOutcome::NotApplied {
                    st.layers.push(Arc::new(layer));
                }
                let err = injected(f.kind);
                st.failed = Some(failure_of(&err));
                Err(err)
            } else {
                st.layers.push(Arc::new(layer));
                Ok(())
            };
            (w.ctx.end_event(self.name, op, format!("b{id}"), r.is_err()), r)
        };
        self.world.after_event(id);
        r?;
        if commit == Commit::Durable {
            self.barrier_event(OpKind::Barrier)?;
        }
        Ok(())
    }

    fn sync_wal(&self) -> Result<()> {
        self.barrier_event(OpKind::SyncWal)
    }

    fn flush_memtables(&self) -> Result<()> {
        self.barrier_event(OpKind::FlushMemtables)
    }

    /// A snapshot observes every key, so it decides every pending bit.
    fn snapshot(&self) -> MemorySnapshot {
        let maps = self.read(|st, ctx| {
            st.schema
                .all_cfs()
                .map(|cf| Arc::new(st.scan(cf, &[], ctx).into_iter().collect::<Map>()))
                .collect()
        });
        MemorySnapshot::from_maps(maps)
    }

    /// Not an observation: counts every batch, decided or not.
    fn approximate_size(&self, cf: Cf) -> Result<u64> {
        let w = self.world.lock();
        let st = &w.stores.engines[self.name];
        let base: u64 = st.base[cf.index()].iter().map(|(k, v)| (k.len() + v.len()) as u64).sum();
        Ok(base + st.layers.iter().map(|l| l.bytes).sum::<u64>())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::Recovery;
    use crate::engine::{Cf, Commit, KvBatch, KvEngine, KvRead};
    use crate::faulty::decide::Dfs;
    use crate::faulty::world::{CrashImage, CrashKind, World, WorldConfig};
    use crate::model::{MODEL_SCHEMA, check_delete_range};

    /// CH2a's direct `delete_range` check, which both production engines run.
    #[test]
    fn delete_range_passes_the_engine_check() {
        let w = World::new(WorldConfig::default());
        check_delete_range(&*w.engine("kv-model", &MODEL_SCHEMA));
    }

    const POINT: Cf = Cf::new(1);

    /// Batch `i`: its own key, and the shared key `last` set to `i`.
    fn put(e: &impl KvEngine, i: u8, commit: Commit) {
        let mut b = e.batch();
        b.put(POINT, &[i], &[i]);
        b.put(POINT, b"last", &[i]);
        e.write(b, commit).unwrap();
    }

    /// The engine's entries in `POINT` (reading every key decides every cell).
    type Seen = Vec<(Vec<u8>, Vec<u8>)>;

    fn seen(e: &impl KvRead) -> Seen {
        e.iter_prefix(POINT, &[]).map(Result::unwrap).collect()
    }

    /// The entries after batches `1..=j`.
    fn after(j: u8) -> Seen {
        let mut out: Seen = (1..=j).map(|i| (vec![i], vec![i])).collect();
        if j > 0 {
            out.push((b"last".to_vec(), vec![j]));
        }
        out.sort();
        out
    }

    const PREFIX: (&str, Recovery) = ("prefix", Recovery::Prefix);
    const SUBSET: (&str, Recovery) = ("subset", Recovery::Subset);

    /// What every branch of a crash of `kind` on `img` leaves of `engines`.
    fn branches(
        img: &CrashImage,
        kind: CrashKind,
        engines: &[(&'static str, Recovery)],
    ) -> Vec<Vec<Seen>> {
        let dfs = Dfs::new();
        let mut out = Vec::new();
        loop {
            let w = img.boot(kind, dfs.decider(), WorldConfig::default());
            out.push(
                engines.iter().map(|&(n, r)| seen(&*w.engine_with(n, &MODEL_SCHEMA, r))).collect(),
            );
            assert_eq!(dfs.mismatch(), None);
            if !dfs.advance() {
                return out;
            }
        }
    }

    /// SB10 C8: a prefix engine keeps one of the pending + 1 prefixes of its batches, each a
    /// subset image, while a subset engine in the same world keeps any of its 2^pending subsets.
    #[test]
    fn prefix_recovery_keeps_a_prefix_of_the_pending_batches() {
        for n in 1..=5u8 {
            let w = World::new(WorldConfig::default());
            let prefix = w.engine_with("prefix", &MODEL_SCHEMA, Recovery::Prefix);
            let subset = w.engine("subset", &MODEL_SCHEMA);
            for i in 1..=n {
                put(&*prefix, i, Commit::Buffered);
                put(&*subset, i, Commit::Buffered);
            }
            let all = branches(&w.capture(), CrashKind::PowerLoss, &[PREFIX, SUBSET]);
            assert_eq!(all.len(), (usize::from(n) + 1) << n, "n = {n}");
            let prefixes: BTreeSet<Seen> = all.iter().map(|b| b[0].clone()).collect();
            let subsets: BTreeSet<Seen> = all.iter().map(|b| b[1].clone()).collect();
            assert_eq!(prefixes, (0..=n).map(after).collect(), "n = {n}");
            assert_eq!(subsets.len(), 1 << n, "n = {n}");
            assert!(prefixes.is_subset(&subsets), "n = {n}");
        }
    }

    /// A durable return pins the cut: only the batches after it are pending, and a process kill
    /// keeps everything.
    #[test]
    fn a_durable_return_pins_the_prefix_cut() {
        let w = World::new(WorldConfig::default());
        let e = w.engine_with("prefix", &MODEL_SCHEMA, Recovery::Prefix);
        for i in 1..=5 {
            put(
                &*e,
                i,
                if i == 3 {
                    Commit::Durable
                } else {
                    Commit::Buffered
                },
            );
        }
        let img = w.capture();
        let got: BTreeSet<Seen> = branches(&img, CrashKind::PowerLoss, &[PREFIX])
            .into_iter()
            .map(|mut b| b.remove(0))
            .collect();
        assert_eq!(got, (3..=5).map(after).collect());
        let killed = branches(&img, CrashKind::ProcessKill, &[PREFIX]);
        assert_eq!(killed, [vec![after(5)]]);
        e.sync_wal().unwrap();
        let synced = branches(&w.capture(), CrashKind::PowerLoss, &[PREFIX]);
        assert_eq!(synced, [vec![after(5)]]);
    }

    /// The recovery is the engine's: reopening it with the other one is a bug of the test.
    #[test]
    #[should_panic(expected = "reopened with another recovery")]
    fn an_engine_keeps_its_recovery() {
        let w = World::new(WorldConfig::default());
        w.engine_with("e", &MODEL_SCHEMA, Recovery::Prefix);
        w.engine("e", &MODEL_SCHEMA);
    }
}
