//! `FaultyFlatFiles`: the filesystem model (§2.2).
//!
//! - **Files** are inodes. Each data unit (a 512-byte sector by default) keeps its versions since
//!   the inode's last `sync_data`, oldest (the durable one) first; the size keeps every size it
//!   held since then, including the sector-aligned sizes inside each extending write.
//! - **Directories** keep a durable base plus their operations since; each operation has its own
//!   survival bit, and a rename is one operation changing two names (A-REN).
//! - **A power loss** turns every unit with pending versions into a lazy cell choosing one of
//!   them (a temporal prefix of its history), every size history into a lazy size cell, and every
//!   pending directory operation into a survival bit. What survives is the next epoch's durable
//!   base. A process kill keeps everything pending.
//! - **A failed `sync_data`** poisons the inode: its pending versions never become durable, and
//!   later syncs succeed without effect (fsyncgate, §2.2.5).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::faulty::check::{Durability, InvariantId, Violation};
use crate::faulty::world::{
    Adversary, AllocateMode, CrashKind, Ctx, FS_STORE, Fault, FaultKind, FaultOutcome, Fnv,
    Granularity, OpKind, Start, VarId, VarKind, World,
};
use crate::fs::{FlatFs, FlatRead, OpenFailed, open_by_path};

pub(crate) type InodeId = u64;

/// The datadir every world starts with, durable.
pub const DATADIR: &str = "/dd";

const PAGE: u64 = 4096;
const SECTOR: u64 = 512;

/// A directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Entry {
    File(InodeId),
    Dir,
}

/// The content of one data unit.
#[derive(Debug, Clone)]
pub(crate) enum Unit {
    Zero,
    Data(Arc<[u8]>),
    /// Undecided: `options[choice]`; `options[0]` is the newest version.
    Lazy {
        var: VarId,
        options: Arc<[Unit]>,
    },
    /// `Extended`: a unit in a filesystem block whose write was in flight at a power loss, and
    /// which holds nothing durable. Choice 1 of `var`, one cell per block (errata I35), makes the
    /// whole block read back as EIO (a torn sector failing its ECC); choice 0 leaves `unit`. Paths
    /// that do not report the content treat an unreadable unit as zeros; a write over it makes it
    /// readable again.
    Eio {
        var: VarId,
        unit: Arc<Unit>,
    },
}

/// A file size.
#[derive(Debug, Clone)]
pub(crate) enum Len {
    Known(u64),
    Lazy {
        var: VarId,
        options: Arc<[Len]>,
    },
}

/// A branch's decisions, read without deciding (D1).
pub(crate) type Dec<'a> = &'a dyn Fn(VarId) -> Option<u32>;

impl Len {
    /// The size if every variable it depends on is decided.
    fn peek(&self, dec: Dec<'_>) -> Option<u64> {
        match self {
            Self::Known(n) => Some(*n),
            Self::Lazy {
                var,
                options,
            } => options[dec(*var)? as usize].peek(dec),
        }
    }

    fn max(&self) -> u64 {
        match self {
            Self::Known(n) => *n,
            Self::Lazy {
                options,
                ..
            } => options.iter().map(Len::max).max().unwrap_or(0),
        }
    }

    fn min(&self) -> u64 {
        match self {
            Self::Known(n) => *n,
            Self::Lazy {
                options,
                ..
            } => options.iter().map(Len::min).min().unwrap_or(0),
        }
    }

    fn resolve(&self, ctx: &mut Ctx) -> u64 {
        match self {
            Self::Known(n) => *n,
            Self::Lazy {
                var,
                options,
            } => {
                let c = ctx.decide(*var) as usize;
                options[c].resolve(ctx)
            }
        }
    }

    fn known(&self) -> Option<u64> {
        match self {
            Self::Known(n) => Some(*n),
            Self::Lazy {
                ..
            } => None,
        }
    }
}

/// Two versions with the same content (undecided cells only equal themselves).
fn same_unit(a: &Unit, b: &Unit) -> bool {
    match (a, b) {
        (Unit::Zero, Unit::Zero) => true,
        (Unit::Data(x), Unit::Data(y)) => x == y,
        (
            Unit::Eio {
                var: v,
                unit: x,
            },
            Unit::Eio {
                var: w,
                unit: y,
            },
        ) => v == w && same_unit(x, y),
        (
            Unit::Lazy {
                var: v,
                ..
            },
            Unit::Lazy {
                var: w,
                ..
            },
        ) => v == w,
        _ => false,
    }
}

impl Unit {
    /// The content if every variable it depends on is decided (`Some(None)`: zeros). An
    /// unreadable unit has no content to compare: EIO is never zeros (CP3-1).
    fn peek(&self, dec: Dec<'_>) -> Option<Option<Arc<[u8]>>> {
        match self {
            Self::Eio {
                var,
                unit,
            } => match dec(*var)? {
                0 => unit.peek(dec),
                _ => None,
            },
            Self::Zero => Some(None),
            Self::Data(d) => Some(Some(d.clone())),
            Self::Lazy {
                var,
                options,
            } => options[dec(*var)? as usize].peek(dec),
        }
    }

    fn resolve(&self, ctx: &mut Ctx) -> Option<Arc<[u8]>> {
        match self {
            Self::Zero => None,
            Self::Eio {
                var,
                unit,
            } => match ctx.decide(*var) {
                0 => unit.resolve(ctx),
                _ => None,
            },
            Self::Data(d) => Some(d.clone()),
            Self::Lazy {
                var,
                options,
            } => {
                let c = ctx.decide(*var) as usize;
                options[c].resolve(ctx)
            }
        }
    }

    fn is_zero(&self) -> bool {
        matches!(self, Self::Zero)
    }

    /// Whether the unit reads back without EIO, without deciding: `None` while an undecided cell
    /// could still choose an unreadable outcome.
    fn readable(&self, dec: Dec<'_>) -> Option<bool> {
        match self {
            Self::Eio {
                var,
                unit,
            } => match dec(*var)? {
                0 => unit.readable(dec),
                _ => Some(false),
            },
            Self::Zero | Self::Data(_) => Some(true),
            Self::Lazy {
                var,
                options,
            } => match dec(*var) {
                Some(c) => options[c as usize].readable(dec),
                None => options.iter().all(|o| o.readable(dec) == Some(true)).then_some(true),
            },
        }
    }

    /// The content once decided, readable or not: an unreadable block's bytes stay on the device.
    fn content(&self, dec: Dec<'_>) -> Option<Option<Arc<[u8]>>> {
        match self {
            Self::Eio {
                unit,
                ..
            } => unit.content(dec),
            Self::Zero => Some(None),
            Self::Data(d) => Some(Some(d.clone())),
            Self::Lazy {
                var,
                options,
            } => options[dec(*var)? as usize].content(dec),
        }
    }

    /// Bytes `[from, to)` if every outcome of the unit gives the same ones, readable or not
    /// (bytes no torn write changed; without deciding).
    fn fixed_bytes(&self, from: usize, to: usize) -> Option<Vec<u8>> {
        match self {
            Self::Zero => Some(vec![0; to - from]),
            Self::Data(d) => Some(d[from..to].to_vec()),
            Self::Eio {
                unit: u,
                ..
            } => u.fixed_bytes(from, to),
            Self::Lazy {
                options,
                ..
            } => {
                let first = options[0].fixed_bytes(from, to)?;
                options[1..]
                    .iter()
                    .all(|o| o.fixed_bytes(from, to).as_ref() == Some(&first))
                    .then_some(first)
            }
        }
    }

    /// Whether some outcome of the unit reads back as EIO (without deciding).
    fn may_be_unreadable(&self) -> bool {
        match self {
            Self::Eio {
                ..
            } => true,
            Self::Lazy {
                options,
                ..
            } => options.iter().any(Unit::may_be_unreadable),
            Self::Zero | Self::Data(_) => false,
        }
    }

    /// Whether the unit, once decided, reads back as EIO.
    fn unreadable(&self, ctx: &mut Ctx) -> bool {
        match self {
            Self::Eio {
                var,
                unit,
            } => ctx.decide(*var) != 0 || (unit.may_be_unreadable() && unit.unreadable(ctx)),
            Self::Lazy {
                var,
                options,
            } => {
                let c = ctx.decide(*var) as usize;
                options[c].unreadable(ctx)
            }
            Self::Zero | Self::Data(_) => false,
        }
    }
}

/// An inode: its size and data unit histories since its last sync. Every history entry carries
/// the number of the operation that produced it (0 = durable).
#[derive(Debug, Clone)]
pub(crate) struct Inode {
    len: Vec<(u32, Len)>,
    units: BTreeMap<u64, Vec<(u32, Unit)>>,
    /// The number of the latest operation (monotone; history entries carry theirs).
    ops: u32,
    /// fsyncgate (§2.2.5): the pending entries of operations up to this number failed to sync.
    /// They stay pending until a power loss; a later sync makes newer writes durable only.
    poisoned: Option<u32>,
    external_links: u32,
    /// The operation number when the current epoch began: pending entries at or below it
    /// survived a process kill (R7a's stabilisation, §5.5).
    epoch_op: u32,
}

impl Inode {
    fn new() -> Self {
        Self {
            len: vec![(0, Len::Known(0))],
            units: BTreeMap::new(),
            ops: 0,
            poisoned: None,
            external_links: 0,
            epoch_op: 0,
        }
    }

    fn cur_len(&self) -> &Len {
        &self.len.last().expect("a size").1
    }

    fn cur_unit(&self, i: u64) -> Unit {
        self.units.get(&i).map_or(Unit::Zero, |h| h.last().expect("a version").1.clone())
    }

    fn push_unit(&mut self, i: u64, u: Unit) {
        let op = self.ops;
        let h = self.units.entry(i).or_insert_with(|| vec![(0, Unit::Zero)]);
        if h.last().is_some_and(|(o, _)| *o == op) {
            h.last_mut().expect("a version").1 = u;
        } else {
            h.push((op, u));
        }
    }

    fn push_len(&mut self, n: u64) {
        let op = self.ops;
        if self.cur_len().known() != Some(n) {
            self.len.push((op, Len::Known(n)));
        }
    }

    /// A completed `sync_data`: the latest size and each unit's latest version become durable,
    /// except units whose latest entry belongs to a poisoned operation.
    fn sync(&mut self) {
        let poison = self.poisoned;
        let poisoned = |op: u32| poison.is_some_and(|p| op <= p);
        // The size is metadata: a later successful sync persists it, even over poisoned pages
        // (their data is what fsyncgate loses).
        let last = self.len.last().expect("a size").clone();
        if self.len.len() > 1 {
            self.len = vec![last];
        }
        for h in self.units.values_mut() {
            let last = h.last().expect("a version").clone();
            if h.len() > 1 && !poisoned(last.0) {
                *h = vec![last];
            }
        }
        self.units.retain(|_, h| h.len() > 1 || !h[0].1.is_zero());
        if !self.pending() {
            self.poisoned = None;
        }
    }

    /// The numbers of the operations with pending entries, oldest first.
    fn pending_ops(&self) -> Vec<u32> {
        let mut ops: BTreeSet<u32> = self.len[1..].iter().map(|(o, _)| *o).collect();
        for h in self.units.values() {
            ops.extend(h[1..].iter().map(|(o, _)| *o));
        }
        ops.into_iter().collect()
    }

    fn pending(&self) -> bool {
        self.len.len() > 1 || self.units.values().any(|h| h.len() > 1)
    }
}

/// A survival bit of a directory operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OpBit {
    Present,
    Var(VarId),
    /// W3: each changed name survives on its own; a name the op binds may also end unbound (a
    /// rename over an existing file done as unlink-then-link, cut between the two).
    PerName(Arc<[VarId]>),
}

#[derive(Debug)]
pub(crate) struct DirOp {
    seq: u64,
    bit: OpBit,
    changes: Vec<(OsString, Option<Entry>)>,
}

/// What survived of one name's change: the change (0), nothing (1: the older binding stands),
/// or, under W3, the name's removal only (2: unbound).
const UNBOUND: u32 = 2;

impl DirOp {
    fn outcome(&self, idx: usize, ctx: &mut Ctx) -> u32 {
        match &self.bit {
            OpBit::Present => 0,
            OpBit::Var(v) => ctx.decide(*v),
            OpBit::PerName(vars) => ctx.decide(vars[idx]),
        }
    }
}

/// The decided outcome of a change (see [`UNBOUND`]), or `None` while undecided.
fn peek_outcome(bit: &OpBit, i: usize, dec: impl Fn(VarId) -> Option<u32>) -> Option<u32> {
    match bit {
        OpBit::Present => Some(0),
        OpBit::Var(v) => dec(*v),
        OpBit::PerName(vars) => dec(vars[i]),
    }
}

/// A version's content, for matching a `^` cell's choice at another crash ([`Start::Value`]).
fn unit_print(u: &Unit) -> Start {
    let mut h = Fnv::default();
    match u {
        Unit::Zero => h.write(&[0]),
        Unit::Data(d) => {
            h.write(&[1]);
            h.write(d);
        }
        Unit::Eio {
            var,
            unit,
        } => {
            h.write(&[2]);
            h.write(&var.to_le_bytes());
            if let Start::Value(v) = unit_print(unit) {
                h.write(&v.to_le_bytes());
            }
        }
        Unit::Lazy {
            var,
            ..
        } => {
            h.write(&[3]);
            h.write(&var.to_le_bytes());
        }
    }
    Start::Value(h.0)
}

fn len_print(l: &Len) -> Start {
    let mut h = Fnv::default();
    match l {
        Len::Known(n) => {
            h.write(&[0]);
            h.write(&n.to_le_bytes());
        }
        Len::Lazy {
            var,
            ..
        } => {
            h.write(&[1]);
            h.write(&var.to_le_bytes());
        }
    }
    Start::Value(h.0)
}

/// The start of each version in a pending history `h` of a `^` cell, newest first as its
/// options are (`keep` drops duplicates): intact for the crashed epoch's own versions and the
/// newest inherited one, which they overwrote; by content for the older ones.
fn history_starts<T>(
    h: &[(u32, T)],
    outer_op: u32,
    keep: impl Fn(&[T], &T) -> bool,
    print: impl Fn(&T) -> Start,
) -> (Vec<T>, Vec<Start>)
where
    T: Clone,
{
    let newest = h.iter().rposition(|(o, _)| *o <= outer_op);
    let (mut options, mut starts) = (Vec::new(), Vec::new());
    for (k, (o, v)) in h.iter().enumerate().rev() {
        if keep(&options, v) {
            options.push(v.clone());
            let intact = k > 0 && (*o > outer_op || Some(k) == newest);
            starts.push(if intact {
                Start::Intact
            } else {
                print(v)
            });
        }
    }
    (options, starts)
}

/// The size a Write-granularity cut keeps: the newest entry at or before operation `cut`.
fn len_at_cut(len: &[(u32, Len)], cut: Option<u32>) -> Len {
    match cut {
        None => len[0].1.clone(),
        Some(cut) => {
            len.iter().rev().find(|(o, _)| *o <= cut).map_or(len[0].1.clone(), |(_, l)| l.clone())
        }
    }
}

/// The error an unreadable block returns (Linux's EIO).
fn eio(what: &str) -> io::Error {
    io::Error::other(format!("EIO: {what}"))
}

fn is_eio(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::Other && e.to_string().starts_with("EIO: ")
}

/// The name suffix of a crash cell built from state already pending before the crashed epoch
/// began (`outer`: a process kill kept it). R7a compares a nested branch that changes a `^`
/// cell's input with a recovery from the same input (§5.5, [`Start`]).
fn outer_mark(outer: bool) -> &'static str {
    if outer {
        "^"
    } else {
        ""
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Dir {
    base: BTreeMap<OsString, Entry>,
    ops: Vec<Arc<DirOp>>,
    durable_len: usize,
}

impl Dir {
    fn binding(&self, name: &OsString, ctx: &mut Ctx) -> Option<Entry> {
        for op in self.ops.iter().rev() {
            for (i, (n, e)) in op.changes.iter().enumerate() {
                if n == name {
                    match op.outcome(i, ctx) {
                        0 => return *e,
                        UNBOUND => return None,
                        _ => {}
                    }
                }
            }
        }
        self.base.get(name).copied()
    }

    /// The binding among durable operations under the branch's decisions; `None` while an
    /// undecided bit is involved.
    fn durable_binding(&self, name: &OsString, dec: Dec<'_>) -> Option<Option<Entry>> {
        for op in self.ops[..self.durable_len].iter().rev() {
            for (i, (n, e)) in op.changes.iter().enumerate() {
                if n != name {
                    continue;
                }
                match peek_outcome(&op.bit, i, dec) {
                    Some(0) => return Some(*e),
                    Some(UNBOUND) => return Some(None),
                    Some(_) => {}
                    None => return None,
                }
            }
        }
        Some(self.base.get(name).copied())
    }

    /// What the pending (not yet durable) operations bind `name` to.
    fn pending_entries(&self, name: &OsString) -> Vec<Option<Entry>> {
        self.ops[self.durable_len..]
            .iter()
            .flat_map(|op| op.changes.iter().filter(|(n, _)| n == name).map(|(_, e)| *e))
            .collect()
    }

    fn names(&self) -> BTreeSet<OsString> {
        let mut out: BTreeSet<OsString> = self.base.keys().cloned().collect();
        for op in &self.ops {
            out.extend(op.changes.iter().map(|(n, _)| n.clone()));
        }
        out
    }

    /// Replaces durable operations whose bit this branch has decided: survivors become present,
    /// lost ones are dropped (only this branch's world changes).
    fn settle(&mut self, ctx: &Ctx) {
        let mut i = 0;
        while i < self.durable_len {
            if let OpBit::Var(v) = self.ops[i].bit
                && let Some(c) = ctx.peek(v)
            {
                if c == 0 {
                    let op = &self.ops[i];
                    self.ops[i] = Arc::new(DirOp {
                        seq: op.seq,
                        bit: OpBit::Present,
                        changes: op.changes.clone(),
                    });
                } else {
                    self.ops.remove(i);
                    self.durable_len -= 1;
                    continue;
                }
            }
            i += 1;
        }
    }

    fn fold(&mut self) {
        let n = self.ops[..self.durable_len].iter().take_while(|o| o.bit == OpBit::Present).count();
        for op in self.ops.drain(..n) {
            for (name, e) in &op.changes {
                match e {
                    Some(e) => self.base.insert(name.clone(), *e),
                    None => self.base.remove(name),
                };
            }
        }
        self.durable_len -= n;
    }
}

/// The last multi-page `pwrite`, for a process kill inside it (`InsideWrite`).
#[derive(Debug, Clone)]
pub(crate) struct LastWrite {
    ino: InodeId,
    offset: u64,
    len: u64,
    op: u32,
    event: u64,
}

/// The filesystem's state.
#[derive(Debug, Clone)]
pub(crate) struct FsState {
    /// Copy-on-write: a capture or boot shares every inode and directory until one changes.
    dirs: BTreeMap<PathBuf, Arc<Dir>>,
    inodes: BTreeMap<InodeId, Arc<Inode>>,
    next_inode: InodeId,
    next_seq: u64,
    /// The directory-operation sequence number when the current epoch began.
    epoch_seq: u64,
    unit: u64,
    pub(crate) last_write: Option<LastWrite>,
    /// External hard links (R10): name → inode.
    external: BTreeMap<PathBuf, InodeId>,
    /// The EIO cells of the boot's power loss: each wraps a unit holding bytes of a write unsynced
    /// at that crash, torn if the block reads back unreadable (DEC-161).
    torn: BTreeSet<VarId>,
    /// DEC-212: the entries a failed `sync_dir` left pending as creates (Core's `DirectoryCommit`
    /// ignores the failure), with their bindings ([`Self::pending_creates`]). A crash may lose
    /// such an entry, and with it the file (or the directory's files); a durable reference into
    /// the file is entitled to that (R1, D1(a)). Another operation on the name ends the
    /// entitlement, as does recreating a directory above it, and so does a later sync of the
    /// directory that succeeds while the create is still pending, as it makes the create durable.
    /// An entry a crash lost stays entitled: Core keeps the dangling reference too.
    failed_dir_sync: BTreeMap<PathBuf, Entry>,
}

impl Default for FsState {
    fn default() -> Self {
        let mut root = Dir::default();
        root.base.insert(OsString::from("dd"), Entry::Dir);
        let mut dirs = BTreeMap::new();
        dirs.insert(PathBuf::from("/"), Arc::new(root));
        dirs.insert(PathBuf::from(DATADIR), Arc::default());
        Self {
            dirs,
            inodes: BTreeMap::new(),
            next_inode: 1,
            next_seq: 0,
            epoch_seq: 0,
            unit: SECTOR,
            last_write: None,
            external: BTreeMap::new(),
            torn: BTreeSet::new(),
            failed_dir_sync: BTreeMap::new(),
        }
    }
}

fn split(path: &Path) -> io::Result<(PathBuf, OsString)> {
    match (path.parent(), path.file_name()) {
        (Some(p), Some(n)) => Ok((p.to_path_buf(), n.to_os_string())),
        _ => {
            Err(io::Error::new(io::ErrorKind::InvalidInput, format!("bad path {}", path.display())))
        }
    }
}

fn not_found(path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{} does not exist", path.display()))
}

impl FsState {
    pub(crate) fn set_unit(&mut self, unit: u64) {
        assert!(self.inodes.is_empty(), "the unit size is fixed once files exist");
        self.unit = unit;
    }

    /// Number of unit versions held (for the capture work counter).
    pub(crate) fn units(&self) -> usize {
        self.inodes.values().map(|i| i.units.values().map(Vec::len).sum::<usize>()).sum()
    }

    /// The largest number of bytes the files could occupy after a crash.
    pub(crate) fn max_usage(&self) -> u64 {
        self.inodes.values().map(|i| i.len.iter().map(|(_, l)| l.max()).max().unwrap_or(0)).sum()
    }

    /// Whether directory `dir` exists, deciding the bits of its ancestors.
    fn dir_exists(&self, dir: &Path, ctx: &mut Ctx) -> bool {
        if dir == Path::new("/") {
            return true;
        }
        let Ok((parent, name)) = split(dir) else {
            return false;
        };
        self.dir_exists(&parent, ctx)
            && self.dirs.get(&parent).is_some_and(|d| d.binding(&name, ctx) == Some(Entry::Dir))
            && self.dirs.contains_key(dir)
    }

    /// The inode `path` names if decided, without deciding (D1's checks at mutations).
    fn peek_lookup(&self, path: &Path, ctx: &Ctx) -> io::Result<Option<InodeId>> {
        Ok(match self.peek_binding(path, ctx)? {
            Some(Some(Entry::File(i))) => Some(i),
            _ => None,
        })
    }

    /// The entry `path` names in its directory, current (pending operations included), without
    /// deciding; `None` while an undecided bit is involved.
    fn peek_binding(&self, path: &Path, ctx: &Ctx) -> io::Result<Option<Option<Entry>>> {
        let dec = |v: VarId| ctx.peek(v);
        let (parent, name) = split(path)?;
        let Some(dir) = self.dirs.get(&parent) else {
            return Ok(Some(None));
        };
        for op in dir.ops.iter().rev() {
            for (i, (n, e)) in op.changes.iter().enumerate() {
                if *n != name {
                    continue;
                }
                match peek_outcome(&op.bit, i, dec) {
                    Some(0) => return Ok(Some(*e)),
                    Some(UNBOUND) => return Ok(Some(None)),
                    Some(_) => {}
                    None => return Ok(None),
                }
            }
        }
        Ok(Some(dir.base.get(&name).copied()))
    }

    /// The inode `path` names, deciding the bits it depends on (L1).
    fn lookup(&self, path: &Path, ctx: &mut Ctx) -> io::Result<Option<InodeId>> {
        let (parent, name) = split(path)?;
        if !self.dir_exists(&parent, ctx) {
            return Ok(None);
        }
        Ok(match self.dirs[&parent].binding(&name, ctx) {
            Some(Entry::File(i)) => {
                // §5.4: a binding never names a missing inode.
                assert!(
                    self.inodes.contains_key(&i),
                    "model self-check: {} names a missing inode",
                    path.display()
                );
                Some(i)
            }
            _ => None,
        })
    }

    fn push_dir_op(&mut self, dir: &Path, changes: Vec<(OsString, Option<Entry>)>) {
        for (name, _) in &changes {
            self.failed_dir_sync.remove(&dir.join(name));
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        Arc::make_mut(self.dirs.get_mut(dir).expect("directory exists")).ops.push(Arc::new(
            DirOp {
                seq,
                bit: OpBit::Present,
                changes,
            },
        ));
    }

    /// The inode at `path`, created (with a directory operation) if absent.
    fn open_or_create(&mut self, path: &Path, ctx: &mut Ctx) -> io::Result<InodeId> {
        if let Some(i) = self.lookup(path, ctx)? {
            return Ok(i);
        }
        let (parent, name) = split(path)?;
        if !self.dir_exists(&parent, ctx) {
            return Err(not_found(&parent));
        }
        let ino = self.next_inode;
        self.next_inode += 1;
        self.inodes.insert(ino, Arc::new(Inode::new()));
        self.push_dir_op(&parent, vec![(name, Some(Entry::File(ino)))]);
        Ok(ino)
    }

    /// The current size, decided only if `need_known` is below some option.
    fn len_at_least(&self, ino: InodeId, end: u64, ctx: &mut Ctx) -> Option<u64> {
        let l = self.inodes[&ino].cur_len();
        if l.min() >= end {
            return None;
        }
        Some(l.resolve(ctx))
    }

    /// Extends `ino` to `end` zero-filled (units in the gap read as zeros), given its current
    /// size `cur`. The new size is pushed by the caller. Zeroing the rest of the block holding
    /// `cur` reads it, and fails if it is unreadable rather than zero its durable bytes (I35).
    fn zero_gap(&mut self, ino: InodeId, cur: u64, end: u64, ctx: &mut Ctx) -> io::Result<()> {
        if end <= cur {
            return Ok(());
        }
        self.read_block_at(ino, cur, ctx)?;
        // Only units holding (or possibly holding) data need a zero version: an absent unit
        // reads as zeros already. A partly covered one is decided first (L3).
        let unit = self.unit;
        let idx: Vec<u64> = self.inodes[&ino]
            .units
            .range(cur / unit..end.div_ceil(unit))
            .map(|(i, _)| *i)
            .collect();
        for i in idx {
            let (us, ue) = (i * unit, (i + 1) * unit);
            let (from, to) = (cur.max(us), end.min(ue));
            let inode = Arc::make_mut(self.inodes.get_mut(&ino).expect("inode exists"));
            let now = inode.cur_unit(i);
            // A unit wholly past the old end reads as zeros (its bytes past `end` too: past the
            // new end), without reading its block.
            if us >= cur || (from == us && to == ue) {
                if !now.is_zero() {
                    inode.push_unit(i, Unit::Zero);
                }
            } else {
                if let Some(d) = now.resolve(ctx) {
                    let mut buf = d.to_vec();
                    buf[(from - us) as usize..(to - us) as usize].fill(0);
                    inode.push_unit(
                        i,
                        if buf.iter().all(|&b| b == 0) {
                            Unit::Zero
                        } else {
                            Unit::Data(buf.into())
                        },
                    );
                }
            }
        }
        Ok(())
    }

    /// Whether filesystem block `page` of `ino` reads back as EIO, deciding only units that could.
    /// Linux reads, and read-modify-writes, whole 4 KiB blocks (ext4 and XFS; errata I35), so one
    /// unreadable unit fails the whole block whatever the adversary's unit. A block wholly past
    /// the end of the file is never read. The operation that fails on it records whether the
    /// boot's crash tore the block.
    fn page_unreadable(&self, ino: InodeId, page: u64, ctx: &mut Ctx) -> bool {
        let (unit, inode) = (self.unit, &self.inodes[&ino]);
        let units: Vec<u64> = inode
            .units
            .range(page * PAGE / unit..((page + 1) * PAGE).div_ceil(unit))
            .filter(|(_, h)| h.last().is_some_and(|(_, u)| u.may_be_unreadable()))
            .map(|(i, _)| *i)
            .collect();
        if units.is_empty() || page * PAGE >= inode.cur_len().resolve(ctx) {
            return false;
        }
        // The block is torn if the unit that fails still holds the version the boot's crash left,
        // wrapped in one of its EIO cells: a later write or copy replaced a torn one (DEC-161).
        let Some(u) = units.into_iter().map(|i| inode.cur_unit(i)).find(|u| u.unreadable(ctx))
        else {
            return false;
        };
        let torn = matches!(u, Unit::Eio { var, .. } if self.torn.contains(&var));
        ctx.note_unreadable(torn);
        true
    }

    /// The read a write makes of the blocks it covers only partly: EIO if one is unreadable. A
    /// write over whole blocks reads nothing, and its new data replaces them.
    fn read_for_partial_write(
        &self,
        ino: InodeId,
        offset: u64,
        len: u64,
        ctx: &mut Ctx,
    ) -> io::Result<()> {
        if len == 0 {
            return Ok(());
        }
        let end = offset + len;
        let (first, last) = (offset / PAGE, (end - 1) / PAGE);
        for page in if first == last {
            vec![first]
        } else {
            vec![first, last]
        } {
            let whole = offset <= page * PAGE && end >= (page + 1) * PAGE;
            if !whole && self.page_unreadable(ino, page, ctx) {
                return Err(eio("a partial write over an unreadable block"));
            }
        }
        Ok(())
    }

    /// Writes `data` at `offset` into the unit histories (not the size). Fully covered units get a
    /// new version; a partly covered one is decided first (L3). With `limit`, only that many units
    /// are written (a partial write).
    fn write_units(
        &mut self,
        ino: InodeId,
        offset: u64,
        data: &[u8],
        ctx: &mut Ctx,
        limit: Option<u32>,
    ) {
        let unit = self.unit;
        let end = offset + data.len() as u64;
        let mut i = offset / unit;
        let mut written = 0;
        while i * unit < end {
            if limit.is_some_and(|l| written >= l) {
                break;
            }
            let (us, ue) = (i * unit, (i + 1) * unit);
            let (from, to) = (offset.max(us), end.min(ue));
            let chunk = &data[(from - offset) as usize..(to - offset) as usize];
            let inode = Arc::make_mut(self.inodes.get_mut(&ino).expect("inode exists"));
            let new = if from == us && to == ue {
                if chunk.iter().all(|&b| b == 0) {
                    Unit::Zero
                } else {
                    Unit::Data(chunk.into())
                }
            } else {
                let old = inode.cur_unit(i).resolve(ctx);
                let mut buf = old.map_or_else(|| vec![0u8; unit as usize], |d| d.to_vec());
                buf[(from - us) as usize..(to - us) as usize].copy_from_slice(chunk);
                if buf.iter().all(|&b| b == 0) {
                    Unit::Zero
                } else {
                    Unit::Data(buf.into())
                }
            };
            inode.push_unit(i, new);
            ctx.work.history_scanned += 1;
            written += 1;
            i += 1;
        }
    }

    /// Shrinks `ino` to `n`: units past it read as zeros again (L3 for the partial one). Zeroing
    /// the rest of the block holding `n` reads it, and fails if it is unreadable (I35).
    fn shrink_units(&mut self, ino: InodeId, n: u64, ctx: &mut Ctx) -> io::Result<()> {
        self.read_block_at(ino, n, ctx)?;
        let unit = self.unit;
        let inode = Arc::make_mut(self.inodes.get_mut(&ino).expect("inode exists"));
        let idx: Vec<u64> = inode.units.range(n / unit..).map(|(i, _)| *i).collect();
        for i in idx {
            let cur = inode.cur_unit(i);
            if i * unit >= n {
                if !cur.is_zero() {
                    inode.push_unit(i, Unit::Zero);
                }
            } else {
                if let Some(d) = cur.resolve(ctx) {
                    let mut buf = d.to_vec();
                    buf[(n - i * unit) as usize..].fill(0);
                    inode.push_unit(
                        i,
                        if buf.iter().all(|&b| b == 0) {
                            Unit::Zero
                        } else {
                            Unit::Data(buf.into())
                        },
                    );
                }
                // Zero already or not, the bytes past `n` are the truncate's now.
            }
        }
        Ok(())
    }

    /// The read Linux makes of the block holding `at` before zeroing the rest of it (a truncate
    /// or an extension from a size inside the block): EIO if the block is unreadable.
    fn read_block_at(&self, ino: InodeId, at: u64, ctx: &mut Ctx) -> io::Result<()> {
        if !at.is_multiple_of(PAGE) && self.page_unreadable(ino, at / PAGE, ctx) {
            return Err(eio("zeroing part of an unreadable block"));
        }
        Ok(())
    }

    fn new_op(&mut self, ino: InodeId) {
        Arc::make_mut(self.inodes.get_mut(&ino).expect("inode exists")).ops += 1;
    }

    /// `pwrite`, with the size history of an extending write.
    fn write(
        &mut self,
        ino: InodeId,
        offset: u64,
        data: &[u8],
        ctx: &mut Ctx,
        limit: Option<u32>,
    ) -> io::Result<()> {
        self.new_op(ino);
        let end = offset + data.len() as u64;
        let cur = self.len_at_least(ino, end, ctx);
        if let Some(cur) = cur
            && offset > cur
        {
            self.zero_gap(ino, cur, offset, ctx)?;
        }
        self.write_units(ino, offset, data, ctx, limit);
        let written_end = match limit {
            Some(l) => (offset / self.unit + u64::from(l)) * self.unit,
            None => end,
        }
        .min(end);
        if let Some(cur) = cur
            && written_end > cur
        {
            let inode = Arc::make_mut(self.inodes.get_mut(&ino).expect("inode exists"));
            // Delalloc write-back: every sector-aligned size inside the extension is possible,
            // including inside a gap the write zero-fills.
            let mut s = (cur / SECTOR + 1) * SECTOR;
            while s < written_end {
                inode.push_len(s);
                s += SECTOR;
            }
            inode.push_len(written_end);
        }
        Ok(())
    }

    fn set_len(&mut self, ino: InodeId, n: u64, ctx: &mut Ctx) -> io::Result<()> {
        self.new_op(ino);
        let cur = self.inodes[&ino].cur_len().clone();
        if cur.min() >= n {
            // A shrink (or no change): the old size is not needed.
            self.shrink_units(ino, n, ctx)?;
        } else {
            let c = cur.resolve(ctx);
            if c > n {
                self.shrink_units(ino, n, ctx)?;
            } else {
                self.zero_gap(ino, c, n, ctx)?;
            }
        }
        let inode = Arc::make_mut(self.inodes.get_mut(&ino).expect("inode exists"));
        inode.push_len(n);
        Ok(())
    }

    /// Reads `[offset, offset + buf.len())`, deciding the size and the units read (L1).
    fn read(&self, ino: InodeId, offset: u64, buf: &mut [u8], ctx: &mut Ctx) -> io::Result<()> {
        let inode = &self.inodes[&ino];
        let end = offset + buf.len() as u64;
        let len = inode.cur_len().resolve(ctx);
        if end > len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "read past the end of the file",
            ));
        }
        if !buf.is_empty()
            && (offset / PAGE..end.div_ceil(PAGE)).any(|p| self.page_unreadable(ino, p, ctx))
        {
            return Err(eio("an unreadable block (Extended)"));
        }
        let unit = self.unit;
        let mut i = offset / unit;
        while i * unit < end {
            let (us, ue) = (i * unit, (i + 1) * unit);
            let (from, to) = (offset.max(us), end.min(ue));
            let dst = &mut buf[(from - offset) as usize..(to - offset) as usize];
            match inode.cur_unit(i).resolve(ctx) {
                Some(d) => dst.copy_from_slice(&d[(from - us) as usize..(to - us) as usize]),
                None => dst.fill(0),
            }
            i += 1;
        }
        ctx.work.bytes_read += buf.len() as u64;
        Ok(())
    }

    /// Converts the state after a crash (§2.1).
    pub(crate) fn crash(&mut self, kind: CrashKind, ctx: &mut Ctx) {
        self.last_write = None;
        self.torn.clear();
        if kind == CrashKind::ProcessKill {
            self.begin_epoch();
            return;
        }
        // DEC-212: an entitled create still pending, which this crash may lose, is its name's
        // binding now (another operation on the name would have ended the entitlement).
        for (path, entry) in &self.failed_dir_sync {
            let (parent, name) = split(path).expect("an entitled path");
            if let Some(newest) = self.dirs[&parent].pending_entries(&name).last() {
                assert_eq!(*newest, Some(*entry), "model self-check: {}", path.display());
            }
        }
        let w = ctx.cfg.witness();
        let epoch = ctx.epoch;
        let outer_seq = self.epoch_seq;
        let extended = ctx.cfg.adversary == Adversary::Extended;
        for (path, dir) in self.dirs.iter_mut() {
            if dir.ops.len() == dir.durable_len {
                continue;
            }
            let dir = Arc::make_mut(dir);
            let mut ops = std::mem::take(&mut dir.ops);
            for op in ops.iter_mut().skip(dir.durable_len) {
                if op.bit != OpBit::Present {
                    continue;
                }
                let inherited = op.seq < outer_seq;
                let mark = outer_mark(inherited);
                let name = format!("{epoch}:fs:{}:op{}{mark}", path.display(), op.seq);
                // An inherited operation's outcome is that of the same cell at a crash before
                // the epoch's first event.
                let new_var = |ctx: &mut Ctx, name: String, domain: u32| {
                    let var = ctx.new_var(VarKind::DirOpSurvives, name, domain);
                    if inherited {
                        let start = (0..domain)
                            .map(|c| match c {
                                0 => Start::Intact,
                                c => Start::Choice(c),
                            })
                            .collect();
                        ctx.inherit(var, start);
                    }
                    var
                };
                let bit = if w.break_rename && op.changes.len() > 1 {
                    OpBit::PerName(
                        op.changes
                            .iter()
                            .enumerate()
                            .map(|(i, (_, e))| {
                                let domain = if e.is_some() {
                                    3
                                } else {
                                    2
                                };
                                new_var(ctx, format!("{name}.{i}"), domain)
                            })
                            .collect(),
                    )
                } else {
                    OpBit::Var(new_var(ctx, name, 2))
                };
                *op = Arc::new(DirOp {
                    seq: op.seq,
                    bit,
                    changes: op.changes.clone(),
                });
            }
            dir.ops = ops;
            dir.durable_len = dir.ops.len();
            dir.fold();
        }
        let write_gran = ctx.cfg.granularity == Granularity::Write;
        for (&ino, inode) in self.inodes.iter_mut() {
            if !inode.pending() {
                if inode.poisoned.is_some() {
                    Arc::make_mut(inode).poisoned = None;
                }
                continue;
            }
            let inode = Arc::make_mut(inode);
            let outer_op = inode.epoch_op;
            if write_gran {
                // One var per file: a temporal prefix of its whole pending history.
                let pending = inode.pending_ops();
                let n = pending.len() as u32;
                let own = pending.iter().filter(|o| **o > outer_op).count() as u32;
                let mark = outer_mark(own < n);
                let var = ctx.new_var(VarKind::Sector, format!("{epoch}:fs:i{ino}:w{mark}"), n + 1);
                // Choice c keeps the pending operations up to the (n - c)-th.
                let cut = |c: u32| (c < n).then(|| pending[(n - 1 - c) as usize]);
                // The version a cut keeps: the newest at or before it (the durable one, index 0).
                let version_at = |h: &[(u32, Unit)], c: u32| match cut(c) {
                    None => 0,
                    Some(cut) => h.iter().rposition(|(o, _)| *o <= cut).unwrap_or(0),
                };
                if own < n {
                    // The epoch's own operations come last: cutting past them cuts inherited ones.
                    // A cut changes a read if it changes a byte or the size it saw.
                    let start = (0..=n)
                        .map(|c| match c.saturating_sub(own) {
                            0 => Start::Intact,
                            k => Start::Choice(k),
                        })
                        .collect();
                    ctx.inherit(var, start);
                }
                for h in inode.units.values_mut() {
                    let options: Vec<Unit> =
                        (0..=n).map(|c| h[version_at(h, c)].1.clone()).collect();
                    *h = vec![(
                        0,
                        Unit::Lazy {
                            var,
                            options: options.into(),
                        },
                    )];
                }
                let options: Vec<Len> = (0..=n).map(|c| len_at_cut(&inode.len, cut(c))).collect();
                inode.len = vec![(
                    0,
                    Len::Lazy {
                        var,
                        options: options.into(),
                    },
                )];
            } else {
                // Durable bytes are judged by the durable size, not the content: a synced unit of
                // zeros is still durable data.
                let durable_len = inode.len[0].1.max();
                let unit = self.unit;
                // The blocks that may read back as EIO, with their units and whether each is `^`.
                let mut eio: BTreeMap<u64, Vec<(u64, bool)>> = BTreeMap::new();
                for (i, h) in inode.units.iter_mut() {
                    if h.len() == 1 {
                        continue;
                    }
                    let inherited = h[1..].iter().any(|(o, _)| *o <= outer_op);
                    let (mut options, mut starts) = history_starts(
                        h,
                        outer_op,
                        |options: &[Unit], u| !options.iter().any(|o| same_unit(o, u)),
                        unit_print,
                    );
                    let durable_data = !h[0].1.is_zero() || i * unit < durable_len;
                    if extended && !matches!(h[0].1, Unit::Lazy { .. }) {
                        // A-PSOW stays granted: garbage only where the pending writes change
                        // bytes. A block with a torn write may read back as EIO, durable bytes in
                        // it included: they stay unchanged, but unavailable (DEC-148, I35).
                        // Bytes by content, through an earlier crash's EIO wrapper (an unknown
                        // pending version changes every byte; an unknown durable one gets no
                        // garbage option).
                        let unit = self.unit as usize;
                        let bytes = |u: &Unit| match u.content(&|v| ctx.peek(v)) {
                            Some(d) => Some(d.map_or_else(|| vec![0; unit], |d| d.to_vec())),
                            None => u.fixed_bytes(0, unit),
                        };
                        if let Some(durable) = bytes(&h[0].1) {
                            let changed: Vec<Option<Vec<u8>>> =
                                h[1..].iter().map(|(_, u)| bytes(u)).collect();
                            let garbage: Vec<u8> = (0..unit)
                                .map(|b| {
                                    if changed
                                        .iter()
                                        .any(|c| c.as_ref().is_none_or(|c| c[b] != durable[b]))
                                    {
                                        0xa5
                                    } else {
                                        durable[b]
                                    }
                                })
                                .collect();
                            let garbage = Unit::Data(garbage.into());
                            if !options.iter().any(|o| same_unit(o, &garbage)) {
                                starts.push(unit_print(&garbage));
                                options.push(garbage);
                            }
                        }
                        eio.entry(*i * self.unit / PAGE).or_default().push((*i, inherited));
                    }
                    if w.break_psow && durable_data && !options.iter().any(Unit::is_zero) {
                        starts.push(unit_print(&Unit::Zero));
                        options.push(Unit::Zero);
                    }
                    // §5.4: a synced value never changes: the durable version stays a possible
                    // outcome of every crash (a unit with nothing pending is not touched at all).
                    assert!(
                        options.iter().any(|o| same_unit(o, &h[0].1)),
                        "model self-check: unit {i} of inode {ino} lost its synced version"
                    );
                    *h = if options.len() == 1 {
                        vec![(0, options.pop().expect("one version"))]
                    } else {
                        let var = ctx.new_var(
                            VarKind::Sector,
                            format!("{epoch}:fs:i{ino}:u{i}{}", outer_mark(inherited)),
                            options.len() as u32,
                        );
                        if inherited {
                            ctx.inherit(var, starts);
                        }
                        vec![(
                            0,
                            Unit::Lazy {
                                var,
                                options: options.into(),
                            },
                        )]
                    };
                }
                // One cell per block, not per unit: EIO fails the whole 4 KiB block whatever the
                // adversary's unit (I35), and per-unit cells would multiply a Byte-unit search by
                // the block's 4,096 units.
                for (block, units) in eio {
                    let inherited = units.iter().any(|(_, o)| *o);
                    let var = ctx.new_var(
                        VarKind::Sector,
                        format!("{epoch}:fs:i{ino}:eio{block}{}", outer_mark(inherited)),
                        2,
                    );
                    self.torn.insert(var);
                    if inherited {
                        ctx.inherit(var, vec![Start::Intact, Start::Choice(1)]);
                    }
                    for (i, _) in units {
                        let h = inode.units.get_mut(&i).expect("a unit of the block");
                        let unit = Arc::new(h[0].1.clone());
                        h[0].1 = Unit::Eio {
                            var,
                            unit,
                        };
                    }
                }
                if inode.len.len() > 1 {
                    let inherited = inode.len[1..].iter().any(|(o, _)| *o <= outer_op);
                    let (mut options, mut starts) = history_starts(
                        &inode.len,
                        outer_op,
                        |options: &[Len], l| !matches!((l, options.last()), (Len::Known(a), Some(Len::Known(b))) if a == b),
                        len_print,
                    );
                    if extended {
                        // Sizes past the last written unit read alike (zeros, or past the end),
                        // so the page-aligned ones stop there; the larger sizes stay options.
                        let written =
                            inode.units.keys().next_back().map_or(0, |u| (u + 1) * self.unit);
                        let (lo, hi) = (
                            options.iter().map(Len::min).min().unwrap_or(0),
                            options
                                .iter()
                                .map(Len::max)
                                .max()
                                .unwrap_or(0)
                                .min(written.div_ceil(PAGE) * PAGE),
                        );
                        let mut s = lo.div_ceil(PAGE) * PAGE;
                        while s < hi {
                            if !options.iter().any(|o| o.known() == Some(s)) {
                                starts.push(len_print(&Len::Known(s)));
                                options.push(Len::Known(s));
                            }
                            s += PAGE;
                        }
                    }
                    let durable = &inode.len[0].1;
                    let same_len = |o: &Len| match (o, durable) {
                        (Len::Known(a), Len::Known(b)) => a == b,
                        (
                            Len::Lazy {
                                var: v,
                                ..
                            },
                            Len::Lazy {
                                var: w,
                                ..
                            },
                        ) => v == w,
                        _ => false,
                    };
                    assert!(
                        options.iter().any(same_len),
                        "model self-check: inode {ino} lost its synced size"
                    );
                    inode.len = if options.len() == 1 {
                        vec![(0, options.pop().expect("one size"))]
                    } else {
                        let var = ctx.new_var(
                            VarKind::FileSize,
                            format!("{epoch}:fs:i{ino}:len{}", outer_mark(inherited)),
                            options.len() as u32,
                        );
                        if inherited {
                            ctx.inherit(var, starts);
                        }
                        vec![(
                            0,
                            Len::Lazy {
                                var,
                                options: options.into(),
                            },
                        )]
                    };
                }
            }
            inode.poisoned = None;
        }
        self.begin_epoch();
    }

    /// Records where the new epoch begins: what is pending now came from earlier epochs.
    fn begin_epoch(&mut self) {
        self.epoch_seq = self.next_seq;
        for inode in self.inodes.values_mut() {
            if inode.epoch_op != inode.ops {
                Arc::make_mut(inode).epoch_op = inode.ops;
            }
        }
    }

    /// A process kill inside the last multi-page `pwrite`, captured right after it: a prefix of
    /// its pages was written (decided eagerly at boot; `InsideWrite`).
    pub(crate) fn interrupt_write(&mut self, lw: &LastWrite, at_event: u64, ctx: &mut Ctx) {
        // Only a crash right after the write can interrupt it; a completed `pwrite` is kept.
        if lw.event != at_event {
            return;
        }
        let first = lw.offset / PAGE;
        let last = (lw.offset + lw.len - 1) / PAGE;
        let pages = (last - first + 1) as u32;
        if pages < 2 {
            return;
        }
        let var = ctx.new_var(
            VarKind::InsideWrite,
            format!("{}:fs:i{}:w{}", ctx.epoch, lw.ino, lw.event),
            pages,
        );
        let lost = ctx.decide(var);
        if lost == 0 {
            return;
        }
        let kept_end = ((first + u64::from(pages - lost)) * PAGE).max(lw.offset);
        let unit = self.unit;
        let Some(inode) = self.inodes.get_mut(&lw.ino) else {
            return;
        };
        let inode = Arc::make_mut(inode);
        for (i, h) in inode.units.iter_mut() {
            if i * unit >= kept_end && h.len() > 1 && h.last().is_some_and(|(o, _)| *o == lw.op) {
                h.pop();
            }
        }
        while inode.len.len() > 1
            && inode
                .len
                .last()
                .is_some_and(|(o, l)| *o == lw.op && l.known().is_some_and(|n| n > kept_end))
        {
            // The write's sizes past the kept pages go; its sector-aligned intermediate sizes
            // up to them stay.
            inode.len.pop();
        }
    }

    /// D1(a)'s question (§5.3): is `[offset, offset + len)` of `path` durable under the
    /// branch's decisions `dec`? Its name must be durably bound, or unbound and entitled by a
    /// failed `sync_dir` (DEC-212), with no pending operation that could rebind it to other bytes
    /// (a pending rename to byte-identical content is fine: `make_private`), its durable size must
    /// cover the range, and no pending or poisoned write may change its bytes. Never decides: an
    /// undecided cell gives `Unknown`.
    pub(crate) fn range_durability(
        &self,
        path: &Path,
        offset: u64,
        len: u64,
        dec: Dec<'_>,
    ) -> Durability {
        let Ok((parent, name)) = split(path) else {
            return Durability::NotDurable("not a file path".into());
        };
        match self.dir_durability(&parent, dec) {
            Durability::Durable => {}
            other => return other,
        }
        let Some(dir) = self.dirs.get(&parent) else {
            return Durability::NotDurable("its directory does not exist".into());
        };
        let ino = match dir.durable_binding(&name, dec) {
            None => return Durability::Unknown("its name's survival is undecided".into()),
            Some(Some(Entry::File(i))) => Some(i),
            // DEC-212: an entry a failed `sync_dir` left pending as a create may be lost with its
            // file, as in Core; the file's bytes, while it is bound, are judged as any other's.
            Some(None) if matches!(self.failed_dir_sync.get(path), Some(Entry::File(_))) => None,
            Some(_) => return Durability::NotDurable("its name is not durable".into()),
        };
        let mut candidates: Vec<InodeId> = ino.into_iter().collect();
        for e in dir.pending_entries(&name) {
            match e {
                Some(Entry::File(j)) => candidates.push(j),
                _ => return Durability::NotDurable("a pending operation unbinds its name".into()),
            }
        }
        let mut first: Option<Vec<u8>> = None;
        let mut unknown = None;
        for c in candidates {
            match self.inode_range(c, offset, len, dec) {
                Durability::Durable => {}
                Durability::Unknown(e) => unknown = Some(e),
                not => return not,
            }
            match (self.durable_bytes(c, offset, len, dec), &first) {
                (None, _) => unknown = Some("its bytes are undecided".into()),
                (Some(b), None) => first = Some(b),
                (Some(b), Some(f)) if b != *f => {
                    return Durability::NotDurable(
                        "a pending rename rebinds its name to other bytes".into(),
                    );
                }
                _ => {}
            }
        }
        unknown.map_or(Durability::Durable, Durability::Unknown)
    }

    /// The entries `dir`'s pending operations create: each name with no durable binding whose
    /// newest pending change binds a file or a directory, with that binding (DEC-212). A rename
    /// over a durable name, or a durable name removed and created again, creates no entry.
    fn pending_creates(&self, dir: &Path, ctx: &Ctx) -> BTreeMap<PathBuf, Entry> {
        let dec = |v: VarId| ctx.peek(v);
        let d = &self.dirs[dir];
        let mut creates = BTreeMap::new();
        for op in &d.ops[d.durable_len..] {
            for (n, e) in &op.changes {
                match e {
                    Some(e) if d.durable_binding(n, &dec) == Some(None) => {
                        creates.insert(dir.join(n), *e)
                    }
                    _ => creates.remove(&dir.join(n)),
                };
            }
        }
        creates
    }

    /// DEC-212 (R1): whether `path` names no file because a crash lost an entry on it that a
    /// failed `sync_dir` left pending as a create: the file's own, or a directory's on its path.
    /// The path breaks at its first entry from the root decided unbound, and that entry must be
    /// recorded: any operation on a recorded name since ends its record, so a recorded entry now
    /// unbound is one the crash lost. An undecided entry is not lost. Never decides.
    pub(crate) fn lost_to_failed_dir_sync(&self, path: &Path, ctx: &Ctx) -> bool {
        let mut ancestors: Vec<&Path> = path.ancestors().collect();
        ancestors.pop(); // the root
        for p in ancestors.into_iter().rev() {
            match self.peek_binding(p, ctx) {
                Ok(Some(Some(_))) => {}
                Ok(Some(None)) => return self.failed_dir_sync.contains_key(p),
                Ok(None) | Err(_) => return false,
            }
        }
        false
    }

    /// Whether directory `dir` (and every ancestor) is durably bound with no pending operation,
    /// or entitled to its entry's loss by a failed `sync_dir` of its parent (DEC-212).
    fn dir_durability(&self, dir: &Path, dec: Dec<'_>) -> Durability {
        if dir == Path::new("/") {
            return Durability::Durable;
        }
        let Ok((parent, name)) = split(dir) else {
            return Durability::NotDurable("bad directory".into());
        };
        match self.dir_durability(&parent, dec) {
            Durability::Durable => {}
            other => return other,
        }
        let Some(d) = self.dirs.get(&parent) else {
            return Durability::NotDurable("a parent directory does not exist".into());
        };
        // DEC-212: a directory a failed `sync_dir` of its parent left pending as a create may be
        // lost with its files, as Core's `blocks/` may (Core never syncs the datadir).
        let entitled = self.failed_dir_sync.get(dir) == Some(&Entry::Dir);
        if !entitled && !d.pending_entries(&name).is_empty() {
            return Durability::NotDurable(format!("{} has a pending operation", dir.display()));
        }
        match d.durable_binding(&name, dec) {
            None => Durability::Unknown("a directory's survival is undecided".into()),
            Some(Some(Entry::Dir)) => Durability::Durable,
            Some(None) if entitled => Durability::Durable,
            Some(_) => Durability::NotDurable(format!("{} is not durable", dir.display())),
        }
    }

    /// The range of one inode: its durable size and its pending or poisoned writes.
    fn inode_range(&self, ino: InodeId, offset: u64, len: u64, dec: Dec<'_>) -> Durability {
        let Some(inode) = self.inodes.get(&ino) else {
            return Durability::NotDurable("its inode is gone".into());
        };

        let durable_len = &inode.len[0].1;
        let end = offset + len;
        match durable_len.peek(dec) {
            Some(n) if n < end => {
                return Durability::NotDurable(format!("its durable size is {n}"));
            }
            Some(_) => {}
            None if durable_len.max() < end => {
                return Durability::NotDurable(format!(
                    "its durable size is at most {}",
                    durable_len.max()
                ));
            }
            None if durable_len.min() < end => {
                return Durability::Unknown("its durable size is undecided".into());
            }
            None => {}
        }
        // A pending size below the range's end (a truncate, a copy's shrink) can be what a crash
        // keeps (§5.3: "its length").
        for (_, l) in &inode.len[1..] {
            match l.peek(dec) {
                Some(n) if n < end => {
                    return Durability::NotDurable(format!("a pending size {n} cuts it"));
                }
                Some(_) => {}
                None if l.min() < end => {
                    return Durability::Unknown("a pending size is undecided".into());
                }
                None => {}
            }
        }
        // A pending write may share a unit with the range (A-PSOW: it never changes bytes
        // outside its own range), so compare the range's bytes in every pending version with
        // the durable one.
        let unit = self.unit;
        let bytes = |u: &Unit| -> Option<Vec<u8>> {
            u.peek(dec).map(|d| d.map_or_else(|| vec![0u8; unit as usize], |d| d.to_vec()))
        };
        // A unit that reads as EIO fails its whole 4 KiB block (CP3-4). The range's bytes in
        // such a block stay durable if every outcome of the crash left them as they were: then
        // they are unavailable, not lost, and an engine reading them must fail-stop (DEC-148).
        // Bytes the torn write could have changed are not durable (CP3-1).
        let mut unknown = None;
        for page in offset / PAGE..end.div_ceil(PAGE) {
            let mut readable = Some(true);
            for (_, h) in inode.units.range(page * PAGE / unit..((page + 1) * PAGE).div_ceil(unit))
            {
                match h[0].1.readable(dec) {
                    Some(true) => {}
                    Some(false) => readable = Some(false),
                    None => readable = readable.and(None),
                }
            }
            if readable == Some(true) {
                continue;
            }
            let (lo, hi) = (offset.max(page * PAGE), end.min((page + 1) * PAGE));
            for (i, h) in inode.units.range(lo / unit..hi.div_ceil(unit)) {
                let from = (lo.max(i * unit) - i * unit) as usize;
                let to = (hi.min((i + 1) * unit) - i * unit) as usize;
                if h[0].1.fixed_bytes(from, to).is_some() {
                    continue;
                }
                if readable == Some(false) {
                    return Durability::NotDurable(format!(
                        "its block is unreadable, and torn unit {i} has no durable bytes"
                    ));
                }
                unknown = Some(format!("its block may be unreadable, over torn unit {i}"));
            }
        }
        for (i, h) in inode.units.range(offset / unit..end.div_ceil(unit)) {
            if h.len() == 1 {
                continue;
            }
            let from = (offset.max(i * unit) - i * unit) as usize;
            let to = (end.min((i + 1) * unit) - i * unit) as usize;
            let Some(durable) = bytes(&h[0].1) else {
                return Durability::Unknown(format!("unit {i} is undecided"));
            };
            for (op, v) in &h[1..] {
                match bytes(v) {
                    // fsyncgate: a write over the range that failed to sync stays pending for
                    // good; one that leaves the range's bytes alone (A-PSOW) does not matter.
                    Some(b) if b[from..to] != durable[from..to] => {
                        let why = if inode.poisoned.is_some_and(|p| *op <= p) {
                            "holds a write that failed to sync (poisoned)"
                        } else {
                            "has a pending write over the range"
                        };
                        return Durability::NotDurable(format!("unit {i} {why}"));
                    }
                    Some(_) => {}
                    None => return Durability::Unknown(format!("unit {i} is undecided")),
                }
            }
        }
        unknown.map_or(Durability::Durable, Durability::Unknown)
    }

    /// An inode's bytes over a range in version `pick` (0: durable, else current), if decided.
    fn bytes_at(
        &self,
        ino: InodeId,
        offset: u64,
        len: u64,
        dec: Dec<'_>,
        current: bool,
    ) -> Option<Vec<u8>> {
        let inode = self.inodes.get(&ino)?;
        let unit = self.unit;
        let end = offset + len;
        // Never past EOF: a range the size does not cover has no bytes to compare.
        let size = if current {
            inode.cur_len()
        } else {
            &inode.len[0].1
        };
        if size.peek(dec)? < end {
            return None;
        }
        let mut out = Vec::with_capacity(len as usize);
        let mut i = offset / unit;
        while i * unit < end {
            let u = match inode.units.get(&i) {
                Some(h) => {
                    if current {
                        &h.last()?.1
                    } else {
                        &h[0].1
                    }
                }
                None => &Unit::Zero,
            };
            let (from, to) = (
                (offset.max(i * unit) - i * unit) as usize,
                (end.min((i + 1) * unit) - i * unit) as usize,
            );
            // An undecided or unreadable unit still has the bytes every outcome agrees on.
            match u.peek(dec) {
                Some(Some(d)) => out.extend_from_slice(&d[from..to]),
                Some(None) => out.resize(out.len() + (to - from), 0),
                None => out.extend(u.fixed_bytes(from, to)?),
            }
            i += 1;
        }
        Some(out)
    }

    fn durable_bytes(&self, ino: InodeId, offset: u64, len: u64, dec: Dec<'_>) -> Option<Vec<u8>> {
        self.bytes_at(ino, offset, len, dec, false)
    }

    /// The bytes a name shows now over a range, if decided (for rename- and copy-over).
    fn current_bytes(&self, ino: InodeId, offset: u64, len: u64, dec: Dec<'_>) -> Option<Vec<u8>> {
        self.bytes_at(ino, offset, len, dec, true)
    }

    /// Whether `path` is durably bound to a file with no pending operation on it or its
    /// directories.
    pub(crate) fn durably_bound(&self, path: &Path, dec: Dec<'_>) -> bool {
        let Ok((parent, name)) = split(path) else {
            return false;
        };
        self.dir_durability(&parent, dec) == Durability::Durable
            && self.dirs.get(&parent).is_some_and(|d| {
                d.pending_entries(&name).is_empty()
                    && matches!(d.durable_binding(&name, dec), Some(Some(Entry::File(_))))
            })
    }

    /// Every name (across directories) bound to `ino`, deciding the bits involved.
    fn link_count(&self, ino: InodeId, ctx: &mut Ctx) -> u64 {
        let mut n = 0;
        let dirs: Vec<PathBuf> = self.dirs.keys().cloned().collect();
        for d in dirs {
            let dir = &self.dirs[&d];
            let mentions = |name: &OsString| {
                dir.base.get(name) == Some(&Entry::File(ino))
                    || dir.ops.iter().any(|o| {
                        o.changes.iter().any(|(m, e)| m == name && *e == Some(Entry::File(ino)))
                    })
            };
            for name in dir.names() {
                if mentions(&name) && self.dir_exists(&d, ctx) && {
                    dir.binding(&name, ctx) == Some(Entry::File(ino))
                } {
                    n += 1;
                }
            }
        }
        n + u64::from(self.inodes[&ino].external_links)
    }
}

/// The filesystem of a [`World`].
pub struct FaultyFlatFiles {
    world: World,
}

fn injected(kind: FaultKind) -> io::Error {
    io::Error::from_raw_os_error(match kind {
        FaultKind::NoSpace => libc::ENOSPC,
        FaultKind::Io => libc::EIO,
    })
}

fn frozen() -> io::Error {
    io::Error::other("the world is frozen")
}

impl FaultyFlatFiles {
    pub(crate) fn new(world: World) -> Self {
        Self {
            world,
        }
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    /// One event: `f` runs with the fault that hits it (if any) and returns the call's result and
    /// a detail for the trace.
    fn event<T>(
        &self,
        op: OpKind,
        path: &Path,
        f: impl FnOnce(&mut FsState, &mut Ctx, Option<Fault>) -> io::Result<T>,
    ) -> io::Result<T> {
        self.world.refresh_d1();
        let (id, r) = {
            let mut guard = self.world.lock();
            let w = &mut *guard;
            let (_, fault) = w.ctx.begin_event(FS_STORE, op).map_err(|_| frozen())?;
            w.ctx.engine_bytes = w.stores.engine_bytes();
            let r = f(&mut w.stores.fs, &mut w.ctx, fault);
            (w.ctx.end_event(FS_STORE, op, path.display().to_string(), r.is_err()), r)
        };
        self.world.after_event(id);
        r
    }

    fn read_op<T>(&self, f: impl FnOnce(&FsState, &mut Ctx) -> T) -> T {
        let mut guard = self.world.lock();
        let w = &mut *guard;
        f(&w.stores.fs, &mut w.ctx)
    }

    /// A read kind's injected fault, if one hits this call (§4.4).
    fn read_fault(&self, op: OpKind, path: &Path) -> io::Result<()> {
        let fault = self.world.lock().ctx.begin_read(FS_STORE, op, || path.display().to_string());
        match fault {
            Some(f) => {
                self.world.after_read_fault();
                Err(injected(f.kind))
            }
            None => Ok(()),
        }
    }

    /// Adds a durable second name for `existing` outside the datadir (an import's hardlink, R10).
    pub fn link_external(&self, existing: &Path, new: &Path) {
        let mut guard = self.world.lock();
        let w = &mut *guard;
        let ino = w.stores.fs.lookup(existing, &mut w.ctx).ok().flatten().expect("file exists");
        Arc::make_mut(w.stores.fs.inodes.get_mut(&ino).expect("inode")).external_links += 1;
        w.stores.fs.external.insert(new.to_path_buf(), ino);
    }

    /// The content of the inode behind an external link (R10).
    pub fn read_external(&self, name: &Path) -> Option<Vec<u8>> {
        self.read_op(|fs, ctx| {
            let ino = *fs.external.get(name)?;
            let len = fs.inodes[&ino].cur_len().resolve(ctx);
            let mut buf = vec![0u8; len as usize];
            fs.read(ino, 0, &mut buf, ctx).ok()?;
            Some(buf)
        })
    }

    /// The names directory `dir` binds (`readdir`), sorted, or `None` if it does not exist. Not
    /// part of [`FlatFs`]: SB14's SPV header files list their segments at open.
    pub fn list_dir(&self, dir: &Path) -> io::Result<Option<Vec<OsString>>> {
        self.read_fault(OpKind::Len, dir)?;
        self.read_op(|fs, ctx| {
            if !fs.dir_exists(dir, ctx) {
                ctx.observe(FS_STORE, || format!("list {}", dir.display()), u64::MAX);
                return Ok(None);
            }
            let d = &fs.dirs[dir];
            let names: Vec<OsString> =
                d.names().into_iter().filter(|n| d.binding(n, ctx).is_some()).collect();
            let mut h = Fnv::default();
            for n in &names {
                h.write(n.as_encoded_bytes());
                h.write(b"/");
            }
            ctx.observe(FS_STORE, || format!("list {}", dir.display()), h.0);
            Ok(Some(names))
        })
    }

    /// Whether directory `dir` exists.
    pub fn dir_exists(&self, dir: &Path) -> bool {
        self.read_op(|fs, ctx| fs.dir_exists(dir, ctx))
    }

    /// The whole file, or `None` if it does not exist.
    pub fn read_all(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        let Some(len) = self.len(path)? else {
            return Ok(None);
        };
        let mut buf = vec![0u8; len as usize];
        self.read_exact_at(path, 0, &mut buf)?;
        Ok(Some(buf))
    }

    /// D1(d) for replacing `to` with inode `src`'s content (§5.3's erratum: rename-over and
    /// copy-over). A copy-over always touches every referenced range: it truncates first and is
    /// not atomic. A rename-over touches a referenced range unless the source holds the same
    /// bytes there durably, as `make_private`'s synced copy does.
    fn d1_replace(fs: &FsState, ctx: &mut Ctx, src: InodeId, to: &Path, what: &str) {
        let dst = fs.peek_lookup(to, ctx).ok().flatten();
        let dec = |v: VarId| ctx.peek(v);
        let mut changed = Vec::new();
        for r in ctx.d1.ranges_on(to) {
            // A rename-over is exempt only when the source's range is durable (size and bytes,
            // no pending write or size reduction) and equals the destination's durable bytes:
            // then either binding a crash keeps reads the same. Anything undecided counts as a
            // change.
            let same = what == "rename over"
                && fs.inode_range(src, r.offset, r.len, &dec) == Durability::Durable
                && dst.is_some_and(|d| {
                    let a = fs.durable_bytes(src, r.offset, r.len, &dec);
                    a.is_some() && a == fs.durable_bytes(d, r.offset, r.len, &dec)
                });
            if !same {
                changed.push(r);
            }
        }
        for r in changed {
            let v = Violation::new(
                InvariantId::D1d,
                format!(
                    "{what} of {} changes durably referenced [{}, {})",
                    to.display(),
                    r.offset,
                    r.offset + r.len
                ),
            )
            .at(ctx.epoch, ctx.events);
            ctx.d1.report(v);
        }
    }

    /// D1(d) for a write: a referenced range is touched only if the write changes its bytes (an
    /// identical rewrite cannot, by A-PSOW) or they cannot be compared.
    fn d1_write(fs: &FsState, ctx: &mut Ctx, path: &Path, offset: u64, data: &[u8]) {
        let end = offset + data.len() as u64;
        let ino = fs.peek_lookup(path, ctx).ok().flatten();
        let dec = |v: VarId| ctx.peek(v);
        let mut changed = Vec::new();
        for r in ctx.d1.ranges_on(path) {
            let (from, to) = (r.offset.max(offset), (r.offset + r.len).min(end));
            if from >= to {
                continue;
            }
            let now = ino.and_then(|i| fs.current_bytes(i, from, to - from, &dec));
            if now.as_deref() != Some(&data[(from - offset) as usize..(to - offset) as usize]) {
                changed.push(r);
            }
        }
        for r in changed {
            let v = Violation::new(
                InvariantId::D1d,
                format!(
                    "write of {}[{offset}, {end}) changes durably referenced [{}, {})",
                    path.display(),
                    r.offset,
                    r.offset + r.len
                ),
            )
            .at(ctx.epoch, ctx.events);
            ctx.d1.report(v);
        }
    }

    fn d1_touch(fs: &FsState, ctx: &mut Ctx, path: &Path, range: std::ops::Range<u64>, what: &str) {
        let _ = fs;
        if let Some(r) = ctx.d1.touched(path, range.clone()) {
            let v = Violation::new(
                InvariantId::D1d,
                format!(
                    "{what} of {}[{}, {}) touches durably referenced [{}, {})",
                    path.display(),
                    range.start,
                    range.end,
                    r.offset,
                    r.offset + r.len
                ),
            )
            .at(ctx.epoch, ctx.events);
            ctx.d1.report(v);
        }
    }
}

fn observe_bytes(ctx: &mut Ctx, what: impl FnOnce() -> String, r: &io::Result<impl AsRef<[u8]>>) {
    let mut h = Fnv::default();
    match r {
        Ok(b) => h.write(b.as_ref()),
        Err(e) => h.write(format!("{:?}", e.kind()).as_bytes()),
    }
    ctx.observe(FS_STORE, what, h.0);
}

impl FlatFs for FaultyFlatFiles {
    fn link_count(&self, path: &Path) -> io::Result<Option<u64>> {
        self.read_fault(OpKind::LinkCount, path)?;
        self.read_op(|fs, ctx| {
            let r = fs.lookup(path, ctx)?.map(|i| fs.link_count(i, ctx));
            ctx.observe(
                FS_STORE,
                || format!("link_count {}", path.display()),
                r.unwrap_or(u64::MAX),
            );
            Ok(r)
        })
    }

    fn len(&self, path: &Path) -> io::Result<Option<u64>> {
        self.read_fault(OpKind::Len, path)?;
        self.read_op(|fs, ctx| {
            let r = fs.lookup(path, ctx)?.map(|i| fs.inodes[&i].cur_len().resolve(ctx));
            ctx.observe(FS_STORE, || format!("len {}", path.display()), r.unwrap_or(u64::MAX));
            Ok(r)
        })
    }

    fn create(&self, path: &Path) -> io::Result<()> {
        self.event(OpKind::Create, path, |fs, ctx, fault| {
            if let Some(f) = &fault
                && f.outcome == FaultOutcome::NotApplied
            {
                return Err(injected(f.kind));
            }
            fs.open_or_create(path, ctx)?;
            fault.map_or(Ok(()), |f| Err(injected(f.kind)))
        })
    }

    /// The model keeps no handles: every read goes by path.
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FlatRead + '_>> {
        open_by_path(self, path)
    }

    fn read_exact_at(&self, path: &Path, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.read_fault(OpKind::Read, path)?;
        self.read_op(|fs, ctx| {
            // The stage decides, as for `OsFs`: the lookup stands for the open, so its failure
            // and a missing file are `OpenFailed`; a failed read of a present file stays plain.
            let r = match fs.lookup(path, ctx).map_err(OpenFailed::wrap)? {
                Some(i) => fs.read(i, offset, buf, ctx),
                None => Err(OpenFailed::wrap(not_found(path))),
            };
            if r.as_ref().is_err_and(is_eio) {
                ctx.read_failed(FS_STORE, OpKind::Read, || {
                    format!("{} {offset}+{} (EIO)", path.display(), buf.len())
                });
            }
            let shown = r.as_ref().map(|_| &*buf).map_err(|e| io::Error::new(e.kind(), ""));
            observe_bytes(
                ctx,
                || format!("read {} {offset}+{}", path.display(), buf.len()),
                &shown,
            );
            r
        })
    }

    fn write_all_at(&self, path: &Path, offset: u64, data: &[u8]) -> io::Result<()> {
        self.event(OpKind::WriteAt, path, |fs, ctx, fault| {
            Self::d1_write(fs, ctx, path, offset, data);
            let limit = match &fault {
                Some(f) if f.outcome == FaultOutcome::NotApplied => return Err(injected(f.kind)),
                Some(f) => match f.outcome {
                    FaultOutcome::Partial(n) => Some(n),
                    _ => None,
                },
                None => None,
            };
            let end = offset + data.len() as u64;
            if let Some(cap) = ctx.cfg.capacity {
                let ino = fs.lookup(path, ctx)?;
                let cur_max = ino.map_or(0, |i| fs.inodes[&i].cur_len().max());
                let extra = end.saturating_sub(cur_max);
                let engines = if ctx.cfg.shared_capacity {
                    ctx.engine_bytes
                } else {
                    0
                };
                if fs.max_usage() + extra + engines > cap {
                    return Err(injected(FaultKind::NoSpace));
                }
            }
            let ino = fs.open_or_create(path, ctx)?;
            // A partial write into an unreadable unit reads it first, and fails (Extended).
            fs.read_for_partial_write(ino, offset, data.len() as u64, ctx)?;
            fs.write(ino, offset, data, ctx, limit)?;
            let op = fs.inodes[&ino].ops;
            if !data.is_empty() && (offset + data.len() as u64 - 1) / PAGE > offset / PAGE {
                fs.last_write = Some(LastWrite {
                    ino,
                    offset,
                    len: data.len() as u64,
                    op,
                    event: ctx.events - 1,
                });
            } else {
                fs.last_write = None;
            }
            if let Some(f) = fault {
                if f.outcome == FaultOutcome::Applied {
                    let inode = Arc::make_mut(fs.inodes.get_mut(&ino).expect("inode"));
                    inode.poisoned = Some(inode.ops);
                }
                return Err(injected(f.kind));
            }
            Ok(())
        })
    }

    fn allocate(&self, path: &Path, offset: u64, len: u64) {
        let _ = self.event(OpKind::Allocate, path, |fs, ctx, fault| {
            let mode = ctx.cfg.allocate;
            if mode == AllocateMode::ZeroFill {
                Self::d1_touch(fs, ctx, path, offset..offset.saturating_add(len), "zero-fill");
            }
            if mode == AllocateMode::Fail
                || fault.as_ref().is_some_and(|f| f.outcome == FaultOutcome::NotApplied)
            {
                return Ok(());
            }
            let end = offset.saturating_add(len);
            if let Some(cap) = ctx.cfg.capacity {
                let ino = fs.lookup(path, ctx)?;
                let cur_max = ino.map_or(0, |i| fs.inodes[&i].cur_len().max());
                if fs.max_usage() + end.saturating_sub(cur_max) > cap {
                    return Ok(());
                }
            }
            let ino = fs.open_or_create(path, ctx)?;
            match mode {
                AllocateMode::ZeroFill => {
                    // `OsFs`'s fallback: zeros over `[offset, end)`, a write like any other.
                    let limit = fault.and_then(|f| match f.outcome {
                        FaultOutcome::Partial(n) => Some(n),
                        _ => None,
                    });
                    fs.read_for_partial_write(ino, offset, end.saturating_sub(offset), ctx)?;
                    fs.write(
                        ino,
                        offset,
                        &vec![0u8; end.saturating_sub(offset) as usize],
                        ctx,
                        limit,
                    )?;
                }
                AllocateMode::Fallocate => {
                    let partial_end = match fault.map(|f| f.outcome) {
                        Some(FaultOutcome::Partial(n)) => offset + u64::from(n) * fs.unit,
                        _ => end,
                    }
                    .min(end);
                    if let Some(cur) = fs.len_at_least(ino, partial_end, ctx) {
                        fs.new_op(ino);
                        // The extension reads as zeros, even over units a crash kept past the
                        // size.
                        fs.zero_gap(ino, cur, partial_end, ctx)?;
                        Arc::make_mut(fs.inodes.get_mut(&ino).expect("inode"))
                            .push_len(cur.max(partial_end));
                    }
                }
                AllocateMode::Fail => {}
            }
            Ok(())
        });
    }

    fn truncate(&self, path: &Path, len: u64) -> io::Result<()> {
        self.event(OpKind::Truncate, path, |fs, ctx, fault| {
            Self::d1_touch(fs, ctx, path, len..u64::MAX, "truncate");
            if let Some(f) = &fault
                && f.outcome == FaultOutcome::NotApplied
            {
                return Err(injected(f.kind));
            }
            let ino = fs.open_or_create(path, ctx)?;
            fs.set_len(ino, len, ctx)?;
            fault.map_or(Ok(()), |f| Err(injected(f.kind)))
        })
    }

    fn sync_data(&self, path: &Path) -> io::Result<()> {
        self.event(OpKind::SyncData, path, |fs, ctx, fault| {
            let ino = fs.lookup(path, ctx)?.ok_or_else(|| not_found(path))?;
            if let Some(f) = fault {
                let inode = Arc::make_mut(fs.inodes.get_mut(&ino).expect("inode"));
                inode.poisoned = Some(inode.ops);
                return Err(injected(f.kind));
            }
            // fsyncgate: after a failed sync, a later one reports success and persists only what
            // was written after the failure.
            Arc::make_mut(fs.inodes.get_mut(&ino).expect("inode")).sync();
            if ctx.cfg.witness().entry_on_file_sync {
                // W4: ext4 also commits the entry of a newly created file.
                let (parent, name) = split(path)?;
                let dir = Arc::make_mut(fs.dirs.get_mut(&parent).expect("parent exists"));
                if let Some(i) = dir.ops.iter().rposition(|o| {
                    o.changes.iter().any(|(n, e)| *n == name && *e == Some(Entry::File(ino)))
                }) {
                    dir.durable_len = dir.durable_len.max(i + 1);
                    dir.fold();
                }
            }
            Ok(())
        })
        .inspect(|_| self.world.after_barrier())
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        self.event(OpKind::SyncDir, dir, |fs, ctx, fault| {
            if !fs.dir_exists(dir, ctx) {
                return Err(not_found(dir));
            }
            let creates = fs.pending_creates(dir, ctx);
            if let Some(f) = fault {
                // The operations stay pending (§4.4), and their creates are entitled (DEC-212).
                fs.failed_dir_sync.extend(creates);
                return Err(injected(f.kind));
            }
            // The pending creates are durable now. An entry an earlier crash lost is not pending,
            // and stays entitled.
            for path in creates.keys() {
                fs.failed_dir_sync.remove(path);
            }
            let d = Arc::make_mut(fs.dirs.get_mut(dir).expect("directory exists"));
            d.durable_len = d.ops.len();
            d.settle(ctx);
            d.fold();
            Ok(())
        })
        .inspect(|_| self.world.after_barrier())
    }

    fn copy(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.event(OpKind::Copy, to, |fs, ctx, fault| {
            if let Some(src) = fs.lookup(from, ctx)? {
                Self::d1_replace(fs, ctx, src, to, "copy over");
            }
            if let Some(f) = &fault
                && f.outcome == FaultOutcome::NotApplied
            {
                return Err(injected(f.kind));
            }
            let src = fs.lookup(from, ctx)?.ok_or_else(|| not_found(from))?;
            let dst = fs.open_or_create(to, ctx)?;
            fs.set_len(dst, 0, ctx)?;
            // The copy references the source's cells without deciding them (L2).
            let units: Vec<(u64, Unit)> = fs.inodes[&src]
                .units
                .iter()
                .map(|(i, h)| (*i, h.last().expect("v").1.clone()))
                .collect();
            let limit = fault.as_ref().and_then(|f| match f.outcome {
                FaultOutcome::Partial(n) => Some(u64::from(n)),
                _ => None,
            });
            let src_len = fs.inodes[&src].cur_len().clone();
            fs.new_op(dst);
            let unit = fs.unit;
            let d = Arc::make_mut(fs.inodes.get_mut(&dst).expect("inode"));
            for (i, u) in units {
                if limit.is_some_and(|l| i >= l) {
                    continue;
                }
                d.push_unit(i, u);
            }
            let op = d.ops;
            match (limit, src_len.known()) {
                (Some(l), _) => d.push_len((l * unit).min(src_len.max())),
                (None, Some(n)) => {
                    // As an extending write from 0: every sector-aligned size in between.
                    let mut s = SECTOR;
                    while s < n {
                        d.push_len(s);
                        s += SECTOR;
                    }
                    d.push_len(n);
                }
                (None, None) => d.len.push((op, src_len)),
            }
            fault.map_or(Ok(()), |f| Err(injected(f.kind)))
        })
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.event(OpKind::Rename, to, |fs, ctx, fault| {
            if let Some(f) = &fault
                && f.outcome == FaultOutcome::NotApplied
            {
                return Err(injected(f.kind));
            }
            let src = fs.lookup(from, ctx)?;
            if let Some(r) = ctx.d1.referenced(from) {
                let v = Violation::new(
                    InvariantId::D1e,
                    format!(
                        "rename of {} away while a durable record references [{}, {})",
                        from.display(),
                        r.offset,
                        r.offset + r.len
                    ),
                )
                .at(ctx.epoch, ctx.events);
                ctx.d1.report(v);
            }
            if let Some(src) = src {
                Self::d1_replace(fs, ctx, src, to, "rename over");
            }
            let (pf, nf) = split(from)?;
            let (pt, nt) = split(to)?;
            assert_eq!(pf, pt, "the model renames within one directory (all ST2 does)");
            let ino = fs.lookup(from, ctx)?.ok_or_else(|| not_found(from))?;
            fs.push_dir_op(&pf, vec![(nf, None), (nt, Some(Entry::File(ino)))]);
            fault.map_or(Ok(()), |f| Err(injected(f.kind)))
        })
    }

    fn remove(&self, path: &Path) -> io::Result<bool> {
        self.event(OpKind::Remove, path, |fs, ctx, fault| {
            if let Some(r) = ctx.d1.referenced(path) {
                let v = Violation::new(
                    InvariantId::D1e,
                    format!(
                        "unlink of {} while a durable record references [{}, {})",
                        path.display(),
                        r.offset,
                        r.offset + r.len
                    ),
                )
                .at(ctx.epoch, ctx.events);
                ctx.d1.report(v);
            }
            if let Some(f) = &fault
                && f.outcome == FaultOutcome::NotApplied
            {
                return Err(injected(f.kind));
            }
            let Some(_) = fs.lookup(path, ctx)? else {
                return fault.map_or(Ok(false), |f| Err(injected(f.kind)));
            };
            let (parent, name) = split(path)?;
            fs.push_dir_op(&parent, vec![(name, None)]);
            fault.map_or(Ok(true), |f| Err(injected(f.kind)))
        })
    }

    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        let missing = self.read_op(|fs, ctx| {
            let mut missing = Vec::new();
            let mut d = dir.to_path_buf();
            while !fs.dir_exists(&d, ctx) {
                missing.push(d.clone());
                if !d.pop() {
                    break;
                }
            }
            missing
        });
        if missing.is_empty() {
            return Ok(());
        }
        self.event(OpKind::CreateDir, dir, |fs, _ctx, fault| {
            if let Some(f) = fault {
                return Err(injected(f.kind));
            }
            for d in missing.into_iter().rev() {
                let (parent, name) = split(&d)?;
                // A recreated directory starts empty: no name in it has a record (DEC-212).
                fs.failed_dir_sync.retain(|p, _| !p.starts_with(&d));
                fs.dirs.insert(d.clone(), Arc::default());
                fs.push_dir_op(&parent, vec![(name, Some(Entry::Dir))]);
            }
            Ok(())
        })
    }

    fn available_space(&self, _dir: &Path) -> io::Result<u64> {
        let w = self.world.lock();
        Ok(match w.ctx.cfg.capacity {
            Some(c) => {
                let engines = if w.ctx.cfg.shared_capacity {
                    w.stores.engine_bytes()
                } else {
                    0
                };
                c.saturating_sub(w.stores.fs.max_usage() + engines)
            }
            None => u64::MAX / 4,
        })
    }
}
