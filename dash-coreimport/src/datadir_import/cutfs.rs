//! `CutFs`, an in-memory power-loss model of §2.3a's filesystem contract.
//!
//! - Every inode has volatile state and durable state. A file's data becomes
//!   durable on `sync_data`; a directory's entries become durable on
//!   `sync_dir` of that directory, each entry independently (contract (d)).
//! - Hard links share one inode, so they share data and its durability.
//! - A rename is durable only once both parents are synced; until then a
//!   crash leaves it whole at the old name or the new name (contracts (a), (b)).
//! - `crash()` keeps only the durable state, `crash_keep()` keeps everything
//!   (plain process death), and `crash_mixed(seed)` keeps a seeded subset of
//!   the unsynced entry changes and file data, a rename all-or-nothing.
//! - File data is page-granular ([`PAGE`]). An unsynced append can reach the
//!   disk with the page holding the old end of file lost: that page reads as
//!   zeros past the old end, and the later pages hold the new bytes (an
//!   overwritten block is not ordered, newly allocated ones are). That is
//!   `crash_torn_head()`, and one of `crash_mixed`'s data choices.
//! - Removing a directory tree unlinks each entry on its own, children first,
//!   as `remove_dir_all` does; each unlink is one mutating call, durable only
//!   when its directory is synced.
//! - `cut_after(k)` makes the process die right after the k-th mutating call:
//!   every later call fails.
//! - [`CutFs::image`] builds a durable image directly, for corruption fixtures.
use super::fs::{FileMetadata, ImportFs};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// The data page size: the journal's sector, the smallest unit a lost write
/// can end on, so lost data always ends on a multiple of it.
pub const PAGE: usize = super::journal::SECTOR;
const ROOT: u64 = 1;
const DEV: u64 = 0x5eed;

#[derive(Clone, Debug)]
enum Node {
    File {
        durable: Vec<u8>,
        volatile: Vec<u8>,
        mtime: u128,
    },
    Dir {
        durable: BTreeMap<OsString, u64>,
        volatile: BTreeMap<OsString, u64>,
    },
    Symlink(PathBuf),
}

#[derive(Clone, Debug)]
struct PendingRename {
    src_dir: u64,
    src_name: OsString,
    dst_dir: u64,
    dst_name: OsString,
    ino: u64,
    src_synced: bool,
    dst_synced: bool,
}

/// Faults a test can inject.
#[derive(Clone, Debug, Default)]
pub struct Faults {
    /// `reflink` fails with this errno.
    pub reflink_errno: Option<i32>,
    /// `hard_link` fails with this errno.
    pub link_errno: Option<i32>,
    /// `copy` drops the last byte of each copied file.
    pub short_copy: bool,
    /// Replaces the generated mount table: this text, or a read failing with
    /// this errno.
    pub mount_table: Option<Result<Vec<u8>, i32>>,
    /// Replaces the modelled mount ids: `Some(None)` is a kernel that gives
    /// none (statx without `STATX_MNT_ID`); `Some(Some(ids))` gives each path
    /// the id paired with its longest component-wise prefix in `ids`.
    pub mount_ids: Option<Option<Vec<(PathBuf, u64)>>>,
}

#[derive(Clone, Debug)]
struct State {
    nodes: BTreeMap<u64, Node>,
    next: u64,
    renames: Vec<PendingRename>,
    clock: u128,
    ops: u64,
    cut_at: Option<u64>,
    dead: bool,
    log: Vec<String>,
    faults: Faults,
    space: u64,
    /// Bind mounts, `(mount point, target)`, in mount order.
    binds: Vec<(PathBuf, PathBuf)>,
}

impl Default for State {
    fn default() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            ROOT,
            Node::Dir {
                durable: BTreeMap::new(),
                volatile: BTreeMap::new(),
            },
        );
        Self {
            nodes,
            next: ROOT + 1,
            renames: Vec::new(),
            clock: 1,
            ops: 0,
            cut_at: None,
            dead: false,
            log: Vec::new(),
            faults: Faults::default(),
            space: u64::MAX,
            binds: Vec::new(),
        }
    }
}

fn err(code: i32) -> io::Error {
    io::Error::from_raw_os_error(code)
}
fn not_found() -> io::Error {
    err(libc::ENOENT)
}
/// A path as mountinfo prints it: space, tab, newline and backslash as
/// `\ooo` octal escapes.
fn mountinfo_path(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let mut out = Vec::new();
    for &b in path.as_os_str().as_bytes() {
        if matches!(b, b' ' | b'\t' | b'\n' | b'\\') {
            out.extend_from_slice(format!("\\{b:03o}").as_bytes());
        } else {
            out.push(b);
        }
    }
    out
}
fn cut() -> io::Error {
    io::Error::other("CutFs: process died at the cut")
}

/// Which view of the directories a lookup walks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Volatile,
    Durable,
}

impl State {
    fn entries(&self, ino: u64, view: View) -> Option<&BTreeMap<OsString, u64>> {
        match self.nodes.get(&ino)? {
            Node::Dir {
                durable,
                volatile,
            } => Some(if view == View::Durable {
                durable
            } else {
                volatile
            }),
            _ => None,
        }
    }
    fn volatile_mut(&mut self, ino: u64) -> io::Result<&mut BTreeMap<OsString, u64>> {
        match self.nodes.get_mut(&ino) {
            Some(Node::Dir {
                volatile,
                ..
            }) => Ok(volatile),
            Some(_) => Err(err(libc::ENOTDIR)),
            None => Err(not_found()),
        }
    }

    /// Resolves an absolute path. Symlinks in the middle are followed; the
    /// last component is followed only when `follow` is set.
    fn lookup(&self, path: &Path, follow: bool, view: View, depth: u32) -> Option<u64> {
        match (follow, path.file_name(), path.parent()) {
            (false, Some(name), Some(parent)) => {
                let dir = self.walk(&self.canonical(parent, view, depth)?, view)?;
                self.entries(dir, view)?.get(name).copied()
            }
            _ => self.walk(&self.canonical(path, view, depth)?, view),
        }
    }

    /// Walks a symlink-free absolute path.
    fn walk(&self, path: &Path, view: View) -> Option<u64> {
        let mut cur = ROOT;
        for c in path.components() {
            if let Component::Normal(name) = c {
                cur = *self.entries(cur, view)?.get(name)?;
            }
        }
        Some(cur)
    }

    fn canonical(&self, path: &Path, view: View, depth: u32) -> Option<PathBuf> {
        if depth > 16 {
            return None;
        }
        let mut out = PathBuf::from("/");
        let mut cur = ROOT;
        for c in path.components() {
            match c {
                Component::Normal(name) => {
                    let ino = *self.entries(cur, view)?.get(name)?;
                    if let Node::Symlink(target) = self.nodes.get(&ino)? {
                        let resolved = self.canonical(&out.join(target), view, depth + 1)?;
                        cur = self.walk(&resolved, view)?;
                        out = resolved;
                    } else {
                        out.push(name);
                        cur = ino;
                    }
                }
                Component::ParentDir => {
                    out.pop();
                    cur = self.walk(&out, view)?;
                }
                _ => {}
            }
        }
        Some(out)
    }

    fn parent_and_name<'p>(&self, path: &'p Path) -> io::Result<(u64, &'p OsStr)> {
        let name = path.file_name().ok_or_else(|| err(libc::EINVAL))?;
        let parent = path.parent().unwrap_or(Path::new("/"));
        let ino = self.lookup(parent, true, View::Volatile, 0).ok_or_else(not_found)?;
        match self.nodes.get(&ino) {
            Some(Node::Dir {
                ..
            }) => Ok((ino, name)),
            _ => Err(err(libc::ENOTDIR)),
        }
    }

    fn get(&self, path: &Path, follow: bool) -> Option<u64> {
        self.lookup(path, follow, View::Volatile, 0)
    }

    fn file(&self, path: &Path) -> io::Result<(u64, &Vec<u8>)> {
        let ino = self.get(path, true).ok_or_else(not_found)?;
        match self.nodes.get(&ino) {
            Some(Node::File {
                volatile,
                ..
            }) => Ok((ino, volatile)),
            Some(Node::Dir {
                ..
            }) => Err(err(libc::EISDIR)),
            _ => Err(not_found()),
        }
    }

    fn new_node(&mut self, node: Node) -> u64 {
        let ino = self.next;
        self.next += 1;
        self.nodes.insert(ino, node);
        ino
    }

    fn tick(&mut self) -> u128 {
        self.clock += 1;
        self.clock
    }

    fn nlink(&self, ino: u64) -> u64 {
        let mut count = 0;
        let mut seen = BTreeSet::new();
        let mut todo = vec![ROOT];
        while let Some(dir) = todo.pop() {
            if !seen.insert(dir) {
                continue;
            }
            for child in self.entries(dir, View::Volatile).into_iter().flat_map(|e| e.values()) {
                if *child == ino {
                    count += 1;
                }
                if matches!(self.nodes.get(child), Some(Node::Dir { .. })) {
                    todo.push(*child);
                }
            }
        }
        count
    }

    fn renamed(&self, dir: u64, name: &OsStr) -> bool {
        self.renames.iter().any(|r| {
            (r.src_dir == dir && r.src_name == name) || (r.dst_dir == dir && r.dst_name == name)
        })
    }

    fn apply_durable_rename(&mut self, r: &PendingRename) {
        if let Some(Node::Dir {
            durable,
            ..
        }) = self.nodes.get_mut(&r.src_dir)
        {
            durable.remove(&r.src_name);
        }
        if let Some(Node::Dir {
            durable,
            ..
        }) = self.nodes.get_mut(&r.dst_dir)
        {
            durable.insert(r.dst_name.clone(), r.ino);
        }
    }

    /// Drops all volatile state: the durable image becomes the live one.
    fn settle(&mut self) {
        for node in self.nodes.values_mut() {
            match node {
                Node::File {
                    durable,
                    volatile,
                    ..
                } => *volatile = durable.clone(),
                Node::Dir {
                    durable,
                    volatile,
                } => *volatile = durable.clone(),
                Node::Symlink(_) => {}
            }
        }
        self.renames.clear();
        self.ops = 0;
        self.cut_at = None;
        self.dead = false;
    }

    fn snapshot(&self, view: View) -> BTreeMap<PathBuf, Snap> {
        let mut out = BTreeMap::new();
        let mut todo = vec![(PathBuf::from("/"), ROOT)];
        let mut seen = BTreeSet::new();
        while let Some((path, dir)) = todo.pop() {
            if !seen.insert(dir) {
                continue;
            }
            for (name, ino) in self.entries(dir, view).into_iter().flatten() {
                let p = path.join(name);
                let snap = match self.nodes.get(ino) {
                    Some(Node::File {
                        durable,
                        volatile,
                        ..
                    }) => Snap::File {
                        ino: *ino,
                        data: if view == View::Durable {
                            durable.clone()
                        } else {
                            volatile.clone()
                        },
                    },
                    Some(Node::Dir {
                        ..
                    }) => {
                        todo.push((p.clone(), *ino));
                        Snap::Dir
                    }
                    Some(Node::Symlink(t)) => Snap::Symlink(t.clone()),
                    None => continue,
                };
                out.insert(p, snap);
            }
        }
        out
    }
}

/// One entry of a tree snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Snap {
    File {
        ino: u64,
        data: Vec<u8>,
    },
    Dir,
    Symlink(PathBuf),
}

#[derive(Clone, Debug, Default)]
pub struct CutFs {
    state: Arc<Mutex<State>>,
}

/// A durable image, built directly (no syncs needed).
#[derive(Clone, Debug, Default)]
pub struct Image {
    fs: CutFs,
}

impl Image {
    fn ensure_dir(&self, path: &Path) -> u64 {
        let mut s = self.fs.lock();
        let mut cur = ROOT;
        for c in path.components() {
            if let Component::Normal(name) = c {
                let existing = s.entries(cur, View::Volatile).and_then(|e| e.get(name)).copied();
                cur = match existing {
                    Some(ino) => ino,
                    None => {
                        let ino = s.new_node(Node::Dir {
                            durable: BTreeMap::new(),
                            volatile: BTreeMap::new(),
                        });
                        Self::insert(&mut s, cur, name, ino);
                        ino
                    }
                };
            }
        }
        cur
    }
    fn insert(s: &mut State, dir: u64, name: &OsStr, ino: u64) {
        if let Some(Node::Dir {
            durable,
            volatile,
        }) = s.nodes.get_mut(&dir)
        {
            durable.insert(name.to_owned(), ino);
            volatile.insert(name.to_owned(), ino);
        }
    }
    fn parent(path: &Path) -> &Path {
        path.parent().unwrap_or(Path::new("/"))
    }
    pub fn with_dir(self, path: impl AsRef<Path>) -> Self {
        self.ensure_dir(path.as_ref());
        self
    }
    pub fn with_file(self, path: impl AsRef<Path>, data: impl AsRef<[u8]>) -> Self {
        let path = path.as_ref();
        let dir = self.ensure_dir(Self::parent(path));
        let mut s = self.fs.lock();
        let mtime = s.tick();
        let ino = s.new_node(Node::File {
            durable: data.as_ref().to_vec(),
            volatile: data.as_ref().to_vec(),
            mtime,
        });
        Self::insert(&mut s, dir, path.file_name().expect("file name"), ino);
        drop(s);
        self
    }
    /// A second name for an existing file's inode.
    pub fn with_link(self, existing: impl AsRef<Path>, path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let dir = self.ensure_dir(Self::parent(path));
        let mut s = self.fs.lock();
        let ino = s.get(existing.as_ref(), false).expect("link target exists");
        Self::insert(&mut s, dir, path.file_name().expect("file name"), ino);
        drop(s);
        self
    }
    pub fn with_symlink(self, path: impl AsRef<Path>, target: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let dir = self.ensure_dir(Self::parent(path));
        let mut s = self.fs.lock();
        let ino = s.new_node(Node::Symlink(target.as_ref().to_owned()));
        Self::insert(&mut s, dir, path.file_name().expect("file name"), ino);
        drop(s);
        self
    }
    /// A bind mount: `path` is a second entry for the directory `target`, with
    /// the same `(st_dev, st_ino)`, which canonicalisation does not unify. The
    /// mount table lists it, with `target` (a canonical path) as its root.
    pub fn with_bind(self, path: impl AsRef<Path>, target: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let ino = self.ensure_dir(target.as_ref());
        let dir = self.ensure_dir(Self::parent(path));
        let mut s = self.fs.lock();
        Self::insert(&mut s, dir, path.file_name().expect("file name"), ino);
        s.binds.push((path.to_owned(), target.as_ref().to_owned()));
        drop(s);
        self
    }
    pub fn build(self) -> CutFs {
        self.fs
    }
}

impl CutFs {
    pub fn image() -> Image {
        Image::default()
    }
    pub fn from_image(image: Image) -> Self {
        image.build()
    }
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// A deep copy that no longer shares state with `self`.
    pub fn fork(&self) -> Self {
        Self {
            state: Arc::new(Mutex::new(self.lock().clone())),
        }
    }
    /// A copy of the durable image only, as a crash would leave it.
    pub fn durable(&self) -> Self {
        let fork = self.fork();
        fork.lock().settle();
        fork
    }
    /// Adds a durable symlink (a test fixture edit, not a counted call).
    pub fn add_symlink(&self, path: &Path, target: &Path) {
        let mut s = self.lock();
        let (dir, name) = s.parent_and_name(path).expect("symlink parent exists");
        let ino = s.new_node(Node::Symlink(target.to_owned()));
        Image::insert(&mut s, dir, name, ino);
    }
    pub fn set_faults(&self, faults: Faults) {
        self.lock().faults = faults;
    }
    pub fn set_available_space(&self, space: u64) {
        self.lock().space = space;
    }
    /// Dies right after the `k`-th mutating call from now.
    pub fn cut_after(&self, k: u64) {
        let mut s = self.lock();
        s.ops = 0;
        s.cut_at = Some(k);
        s.dead = false;
    }
    /// Whether a cut has been reached.
    pub fn is_dead(&self) -> bool {
        self.lock().dead
    }
    /// Mutating calls since the last `cut_after`, crash or `reset_ops`.
    pub fn ops(&self) -> u64 {
        self.lock().ops
    }
    pub fn reset_ops(&self) {
        let mut s = self.lock();
        s.ops = 0;
        s.log.clear();
    }
    /// The calls made, as `"<op> <path>"`.
    pub fn log(&self) -> Vec<String> {
        self.lock().log.clone()
    }
    /// The live tree.
    pub fn snapshot(&self) -> BTreeMap<PathBuf, Snap> {
        self.lock().snapshot(View::Volatile)
    }
    /// The durable tree.
    pub fn durable_snapshot(&self) -> BTreeMap<PathBuf, Snap> {
        self.lock().snapshot(View::Durable)
    }

    pub fn crash(&self) {
        self.lock().settle();
    }

    pub fn crash_keep(&self) {
        let mut s = self.lock();
        for node in s.nodes.values_mut() {
            match node {
                Node::File {
                    durable,
                    volatile,
                    ..
                } => *durable = volatile.clone(),
                Node::Dir {
                    durable,
                    volatile,
                } => *durable = volatile.clone(),
                Node::Symlink(_) => {}
            }
        }
        s.settle();
    }

    /// A crash where every unsynced append to an existing file reached the
    /// disk except the page holding the old end of file, which reads as
    /// zeros past it; entries keep only their durable state.
    pub fn crash_torn_head(&self) {
        let mut s = self.lock();
        for node in s.nodes.values_mut() {
            if let Node::File {
                durable,
                volatile,
                ..
            } = node
                && let Some(torn) = torn_head(durable, volatile)
            {
                *durable = torn;
            }
        }
        s.settle();
    }

    pub fn crash_mixed(&self, seed: u64) {
        let mut rng = seed ^ 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            rng = rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = rng;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        };
        let mut s = self.lock();
        for r in s.renames.clone() {
            if next() & 1 == 0 {
                s.apply_durable_rename(&r);
            }
        }
        let pending = s.renames.clone();
        let inos: Vec<u64> = s.nodes.keys().copied().collect();
        for ino in inos {
            let choice = next();
            match s.nodes.get_mut(&ino) {
                Some(Node::Dir {
                    durable,
                    volatile,
                }) => {
                    let names: BTreeSet<OsString> =
                        durable.keys().chain(volatile.keys()).cloned().collect();
                    let mut bit = 0u32;
                    for name in names {
                        let in_rename = pending.iter().any(|r| {
                            (r.src_dir == ino && r.src_name == name)
                                || (r.dst_dir == ino && r.dst_name == name)
                        });
                        if in_rename || durable.get(&name) == volatile.get(&name) {
                            continue;
                        }
                        let keep = (choice.rotate_left(bit) & 1) == 0;
                        bit += 1;
                        if keep {
                            match volatile.get(&name) {
                                Some(v) => durable.insert(name, *v),
                                None => durable.remove(&name),
                            };
                        }
                    }
                }
                Some(Node::File {
                    durable,
                    volatile,
                    ..
                }) if durable != volatile => match choice % 5 {
                    0 => {}
                    4 if torn_head(durable, volatile).is_some() => {
                        *durable = torn_head(durable, volatile).expect("checked");
                    }
                    1 => *durable = volatile.clone(),
                    2 if volatile.len() > durable.len() && volatile.starts_with(durable) => {
                        let extra = (volatile.len() - durable.len()) as u64;
                        let keep = durable.len() + ((choice >> 8) % extra) as usize;
                        *durable = volatile[..keep].to_vec();
                    }
                    3 if volatile.len() > durable.len() && volatile.starts_with(durable) => {
                        let len = volatile.len();
                        durable.resize(len, 0);
                    }
                    _ => *durable = volatile.clone(),
                },
                _ => {}
            }
        }
        s.settle();
    }

    fn begin(&self, op: &str, path: &Path) -> io::Result<MutexGuard<'_, State>> {
        let mut s = self.lock();
        if s.dead {
            return Err(cut());
        }
        s.log.push(format!("{op} {}", path.display()));
        Ok(s)
    }
    fn mutated(s: &mut State) {
        s.ops += 1;
        if s.cut_at == Some(s.ops) {
            s.dead = true;
        }
    }
    fn read_only(&self) -> io::Result<MutexGuard<'_, State>> {
        let s = self.lock();
        if s.dead {
            return Err(cut());
        }
        Ok(s)
    }

    fn create_file(s: &mut State, dst: &Path, data: Vec<u8>) -> io::Result<u64> {
        let (dir, name) = s.parent_and_name(dst)?;
        if s.entries(dir, View::Volatile).is_some_and(|e| e.contains_key(name)) {
            return Err(err(libc::EEXIST));
        }
        let mtime = s.tick();
        let ino = s.new_node(Node::File {
            durable: Vec::new(),
            volatile: data,
            mtime,
        });
        s.volatile_mut(dir)?.insert(name.to_owned(), ino);
        Ok(ino)
    }
}

/// The torn-head image of an append: `volatile` with the bytes from the old
/// end of file up to the next page boundary zeroed. `None` unless `volatile`
/// extends `durable` and the old end is inside a page.
fn torn_head(durable: &[u8], volatile: &[u8]) -> Option<Vec<u8>> {
    let old = durable.len();
    if volatile.len() <= old || !volatile.starts_with(durable) || old.is_multiple_of(PAGE) {
        return None;
    }
    let end = (old / PAGE + 1) * PAGE;
    let mut out = volatile.to_vec();
    out[old..end.min(volatile.len())].fill(0);
    Some(out)
}

impl CutFs {
    /// Unlinks `name` from `dir`, children first for a directory, one
    /// mutating call per entry. Stops at the cut.
    fn unlink_tree(s: &mut State, dir: u64, name: &OsStr) -> io::Result<()> {
        let Some(ino) = s.entries(dir, View::Volatile).and_then(|e| e.get(name)).copied() else {
            return Ok(());
        };
        if let Some(children) =
            s.entries(ino, View::Volatile).map(|e| e.keys().cloned().collect::<Vec<_>>())
        {
            for child in children {
                Self::unlink_tree(s, ino, &child)?;
            }
        }
        if s.dead {
            return Err(cut());
        }
        Self::mutated(s);
        s.volatile_mut(dir)?.remove(name);
        Ok(())
    }
}

impl ImportFs for CutFs {
    fn reflink(&self, src: &Path, dst: &Path) -> io::Result<()> {
        let mut s = self.begin("reflink", dst)?;
        Self::mutated(&mut s);
        if let Some(code) = s.faults.reflink_errno {
            return Err(err(code));
        }
        let data = s.file(src)?.1.clone();
        Self::create_file(&mut s, dst, data).map(|_| ())
    }
    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()> {
        let mut s = self.begin("hard_link", dst)?;
        Self::mutated(&mut s);
        if let Some(code) = s.faults.link_errno {
            return Err(err(code));
        }
        let ino = s.file(src)?.0;
        let (dir, name) = s.parent_and_name(dst)?;
        if s.entries(dir, View::Volatile).is_some_and(|e| e.contains_key(name)) {
            return Err(err(libc::EEXIST));
        }
        s.volatile_mut(dir)?.insert(name.to_owned(), ino);
        Ok(())
    }
    fn copy(&self, src: &Path, dst: &Path) -> io::Result<u64> {
        let mut s = self.begin("copy", dst)?;
        Self::mutated(&mut s);
        let mut data = s.file(src)?.1.clone();
        let len = data.len() as u64;
        if s.faults.short_copy {
            data.pop();
        }
        Self::create_file(&mut s, dst, data)?;
        Ok(len)
    }
    fn sync_data(&self, path: &Path) -> io::Result<()> {
        let mut s = self.begin("sync_data", path)?;
        Self::mutated(&mut s);
        let ino = s.file(path)?.0;
        if let Some(Node::File {
            durable,
            volatile,
            ..
        }) = s.nodes.get_mut(&ino)
        {
            *durable = volatile.clone();
        }
        Ok(())
    }
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        let mut s = self.begin("sync_dir", path)?;
        Self::mutated(&mut s);
        let dir = s.get(path, true).ok_or_else(not_found)?;
        let (durable, volatile) = match s.nodes.get(&dir) {
            Some(Node::Dir {
                durable,
                volatile,
            }) => (durable.clone(), volatile.clone()),
            _ => return Err(err(libc::ENOTDIR)),
        };
        let names: BTreeSet<OsString> = durable.keys().chain(volatile.keys()).cloned().collect();
        let mut next = durable;
        for name in names {
            if s.renamed(dir, &name) {
                continue;
            }
            match volatile.get(&name) {
                Some(ino) => next.insert(name, *ino),
                None => next.remove(&name),
            };
        }
        if let Some(Node::Dir {
            durable,
            ..
        }) = s.nodes.get_mut(&dir)
        {
            *durable = next;
        }
        for r in &mut s.renames {
            if r.src_dir == dir {
                r.src_synced = true;
            }
            if r.dst_dir == dir {
                r.dst_synced = true;
            }
        }
        let (done, rest): (Vec<_>, Vec<_>) =
            std::mem::take(&mut s.renames).into_iter().partition(|r| r.src_synced && r.dst_synced);
        s.renames = rest;
        for r in &done {
            s.apply_durable_rename(r);
        }
        Ok(())
    }
    fn rename(&self, src: &Path, dst: &Path) -> io::Result<()> {
        let mut s = self.begin("rename", src)?;
        Self::mutated(&mut s);
        let (src_dir, src_name) = s.parent_and_name(src)?;
        let (dst_dir, dst_name) = s.parent_and_name(dst)?;
        let ino = *s
            .entries(src_dir, View::Volatile)
            .and_then(|e| e.get(src_name))
            .ok_or_else(not_found)?;
        let src_is_dir = matches!(s.nodes.get(&ino), Some(Node::Dir { .. }));
        if let Some(old) = s.entries(dst_dir, View::Volatile).and_then(|e| e.get(dst_name)).copied()
        {
            match (s.nodes.get(&old), src_is_dir) {
                (
                    Some(Node::Dir {
                        volatile,
                        ..
                    }),
                    true,
                ) if !volatile.is_empty() => return Err(err(libc::ENOTEMPTY)),
                (
                    Some(Node::Dir {
                        ..
                    }),
                    false,
                ) => return Err(err(libc::EISDIR)),
                (
                    Some(
                        Node::File {
                            ..
                        }
                        | Node::Symlink(_),
                    ),
                    true,
                ) => return Err(err(libc::ENOTDIR)),
                _ => {}
            }
        }
        s.volatile_mut(src_dir)?.remove(src_name);
        s.volatile_mut(dst_dir)?.insert(dst_name.to_owned(), ino);
        let pending = PendingRename {
            src_dir,
            src_name: src_name.to_owned(),
            dst_dir,
            dst_name: dst_name.to_owned(),
            ino,
            src_synced: false,
            dst_synced: false,
        };
        s.renames.push(pending);
        Ok(())
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        let mut s = self.begin("remove", path)?;
        let Ok((dir, name)) = s.parent_and_name(path) else {
            return Ok(());
        };
        Self::unlink_tree(&mut s, dir, name)
    }
    fn truncate(&self, path: &Path, len: u64) -> io::Result<()> {
        let mut s = self.begin("truncate", path)?;
        Self::mutated(&mut s);
        let ino = s.file(path)?.0;
        let mtime = s.tick();
        if let Some(Node::File {
            volatile,
            mtime: m,
            ..
        }) = s.nodes.get_mut(&ino)
        {
            volatile.resize(len as usize, 0);
            *m = mtime;
        }
        Ok(())
    }
    fn append(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let mut s = self.begin("append", path)?;
        Self::mutated(&mut s);
        let ino = match s.file(path) {
            Ok((ino, _)) => ino,
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {
                Self::create_file(&mut s, path, Vec::new())?
            }
            Err(e) => return Err(e),
        };
        let mtime = s.tick();
        if let Some(Node::File {
            volatile,
            mtime: m,
            ..
        }) = s.nodes.get_mut(&ino)
        {
            volatile.extend_from_slice(data);
            *m = mtime;
        }
        Ok(())
    }
    fn metadata(&self, path: &Path) -> io::Result<Option<FileMetadata>> {
        let s = self.read_only()?;
        let Some(ino) = s.get(path, false) else {
            return Ok(None);
        };
        let (len, modified, is_dir, is_symlink) = match s.nodes.get(&ino) {
            Some(Node::File {
                volatile,
                mtime,
                ..
            }) => (volatile.len() as u64, *mtime, false, false),
            Some(Node::Dir {
                ..
            }) => (0, 0, true, false),
            Some(Node::Symlink(t)) => (t.as_os_str().len() as u64, 0, false, true),
            None => return Ok(None),
        };
        Ok(Some(FileMetadata {
            len,
            modified,
            dev: DEV,
            ino,
            nlink: if is_dir {
                1
            } else {
                s.nlink(ino)
            },
            is_dir,
            is_symlink,
        }))
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let s = self.read_only()?;
        let ino = s.get(path, true).ok_or_else(not_found)?;
        let entries = s.entries(ino, View::Volatile).ok_or_else(|| err(libc::ENOTDIR))?;
        Ok(entries.keys().map(|n| path.join(n)).collect())
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let mut s = self.read_only()?;
        s.log.push(format!("read {}", path.display()));
        Ok(s.file(path)?.1.clone())
    }
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        let mut s = self.begin("create_dir_all", path)?;
        let mut prefix = PathBuf::from("/");
        let mut cur = ROOT;
        let mut created = false;
        for c in path.components() {
            let Component::Normal(name) = c else {
                continue;
            };
            prefix.push(name);
            cur = match s.get(&prefix, true) {
                Some(ino) if matches!(s.nodes.get(&ino), Some(Node::Dir { .. })) => ino,
                Some(_) => return Err(err(libc::EEXIST)),
                None => {
                    created = true;
                    let ino = s.new_node(Node::Dir {
                        durable: BTreeMap::new(),
                        volatile: BTreeMap::new(),
                    });
                    s.volatile_mut(cur)?.insert(name.to_owned(), ino);
                    ino
                }
            };
        }
        if created {
            Self::mutated(&mut s);
        }
        Ok(())
    }
    fn available_space(&self, _: &Path) -> io::Result<u64> {
        Ok(self.read_only()?.space)
    }
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        let s = self.read_only()?;
        let joined = if path.is_absolute() {
            path.to_owned()
        } else {
            Path::new("/").join(path)
        };
        s.canonical(&joined, View::Volatile, 0).ok_or_else(not_found)
    }
    /// One filesystem mounted at `/`, plus a line per bind mount, unless a
    /// [`Faults::mount_table`] replaces it.
    fn mount_table(&self) -> io::Result<Option<Vec<u8>>> {
        let s = self.read_only()?;
        match &s.faults.mount_table {
            Some(Ok(text)) => return Ok(Some(text.clone())),
            Some(Err(code)) => return Err(err(*code)),
            None => {}
        }
        let mut out = format!("1 0 0:{DEV} / / rw shared:1 - cutfs cutfs rw\n").into_bytes();
        for (i, (at, target)) in s.binds.iter().enumerate() {
            out.extend_from_slice(format!("{} 1 0:{DEV} ", i + 2).as_bytes());
            out.extend(mountinfo_path(target));
            out.push(b' ');
            out.extend(mountinfo_path(at));
            out.extend_from_slice(b" rw shared:1 - cutfs cutfs rw\n");
        }
        Ok(Some(out))
    }
    /// The id of the generated table's line for the bind mount that holds
    /// `path` (the deepest mount point above it, the later on a tie; binds
    /// here are never hidden), else 1 for `/`, unless [`Faults::mount_ids`]
    /// replaces it. A missing path is `ENOENT`.
    fn mount_id(&self, path: &Path) -> io::Result<Option<u64>> {
        let s = self.read_only()?;
        s.get(path, false).ok_or_else(not_found)?;
        let deepest = |ids: &mut dyn Iterator<Item = (&Path, u64)>| {
            ids.filter(|(at, _)| path.starts_with(at))
                .fold(None, |best: Option<(usize, u64)>, (at, id)| {
                    let depth = at.components().count();
                    match best {
                        Some((d, _)) if d > depth => best,
                        _ => Some((depth, id)),
                    }
                })
                .map(|(_, id)| id)
        };
        match &s.faults.mount_ids {
            Some(None) => Ok(None),
            Some(Some(ids)) => Ok(deepest(&mut ids.iter().map(|(at, id)| (at.as_path(), *id)))),
            None => Ok(Some(
                deepest(
                    &mut s
                        .binds
                        .iter()
                        .enumerate()
                        .map(|(i, (at, _))| (at.as_path(), i as u64 + 2)),
                )
                .unwrap_or(1),
            )),
        }
    }
    fn now_ms(&self) -> u64 {
        0
    }
}
