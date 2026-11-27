//! Regtest's in-place import of a Dash Core network directory (CH5-4): Core's databases move
//! aside into a backup directory as one journaled transaction, and the node rebuilds from the
//! block files Core left by a reindex.
//!
//! The caller holds the network directory's lock throughout and has proved the directory is
//! Core's ([`obfuscate_key`] among its checks). Every filesystem call goes through
//! [`ImportFs`], so the power-loss model sees each one.
//!
//! | Step | Writes | Durable before the next step |
//! | --- | --- | --- |
//! | S1 | none: the backup name, the first free `core-import-<UTC>`, `-1`, … `-999` | — |
//! | S2 | [`JOURNAL`] `Intent`, through `core-import.journal.new` | `fdatasync`, rename, `fsync(net)` |
//! | S3 | the backup dir, and its `blocks/` when `blocks/index` moves | `fsync(net)` |
//! | S4 | each present member, in order (`chainstate`, `blocks/index`, `evodb`, `llmq`, `indexes`), renamed into the backup | `fsync` of both parents after each |
//! | S5 | `Moved` | as S2 |
//! | S6 | the caller's: a fresh store with the reindex flag stored | the caller's |
//! | S7 | `Complete` ([`MoveAside::complete`]) | as S2 |
//! | S8 | the journal renamed into the backup | `fsync(net)`, `fsync(backup)` |
//!
//! `chainstate` is always present and moves first, so its `fsync(backup)` makes the backup's
//! `blocks/` durable before `blocks/index` moves into it. [`begin`] refuses, before S2, a member
//! on another filesystem or mount than `net` and a `blocks` that is not a plain directory, so
//! every rename stays on one `st_dev`, and on one mount where the kernel reports a mount id.
//! Neither sees three cases, each of which fails safe with a journal that has to be removed by
//! hand: a directory from an overlayfs lower layer without `redirect_dir` (its rename fails with
//! `EXDEV`), a bind mount on a kernel that reports no mount id (also `EXDEV`), and overlayfs
//! directory inodes that change across a remount (recovery then refuses the member as changed).
//!
//! [`recover`] resumes from the journal and what it observes: `Intent` finishes S3 and S4 (a
//! member at its source with its recorded `st_ino`, on `net`'s current filesystem, is moved; one
//! at the backup is done) and writes `Moved`; `Moved` returns for the caller's S6; `Complete`
//! runs S8. The recorded `st_dev` is not compared, since a remount or reboot can renumber it. A
//! member at both paths or at neither, a changed identity, an entry in the backup the import did
//! not put there, or a corrupt journal is refused before any write.

mod journal;
#[cfg(test)]
mod tests;

pub use journal::{JournalState, MemberId};

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use crate::datadir_import::{FileMetadata, ImportFs};
use crate::leveldb::LevelDbReader;

/// The journal, in the network directory, outside every moved directory.
pub const JOURNAL: &str = "core-import.journal";
/// Where a new journal state is written before it replaces [`JOURNAL`].
pub(crate) const JOURNAL_NEW: &str = "core-import.journal.new";
/// Every backup directory's name starts with this.
pub const BACKUP_PREFIX: &str = "core-import-";
/// Core's databases, relative to the network directory, in the order they move: the
/// directories detection classifies and [`begin`] moves.
pub const MEMBERS: [&str; 5] = ["chainstate", "blocks/index", "evodb", "llmq", "indexes"];
/// `ser("\0obfuscate_key")`: CompactSize 14, then the 14 bytes.
// Core: dbwrapper.cpp:232, serialize.h:1039-1043 at v24.0.2
pub(crate) const OBFUSCATE_KEY_KEY: &[u8] = b"\x0e\0obfuscate_key";
const LAST_SUFFIX: u32 = 999;

/// The journal's phases, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Phase {
    Intent = 0,
    Moved = 1,
    Complete = 2,
}

#[derive(Debug, thiserror::Error)]
pub enum RegtestImportError {
    #[error("{}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the Dash Core import journal {} is corrupt ({detail}); nothing was changed", .path.display())]
    JournalCorrupt {
        path: PathBuf,
        detail: String,
    },
    #[error("the Dash Core import journal {} already exists", .0.display())]
    JournalExists(PathBuf),
    #[error("{}: {detail}", .path.display())]
    Member {
        path: PathBuf,
        detail: &'static str,
    },
    #[error("{} holds {}, which the Dash Core import did not put there", .backup.display(), .entry.display())]
    ForeignBackupEntry {
        backup: PathBuf,
        entry: PathBuf,
    },
    #[error("no free Dash Core import backup name in {}", .0.display())]
    NoFreeBackupName(PathBuf),
}

type Result<T> = std::result::Result<T, RegtestImportError>;

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> RegtestImportError + '_ {
    move |source| RegtestImportError::Io {
        path: path.to_owned(),
        source,
    }
}

/// What the newest record of Core's obfuscation key holds in a LevelDB `chainstate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObfuscateKey {
    /// `08 ‖ 8 bytes`, as Core writes it.
    Present([u8; 8]),
    /// No record, or a deletion is the newest.
    Absent,
    /// Any other value.
    Malformed,
}

/// Reads Core's obfuscation key from the LevelDB database `chainstate` without writing:
/// tables and WALs by sequence, with LevelDB's end-of-file rule for a WAL cut short
/// ([`LevelDbReader::open_tolerant`]).
// Core: dbwrapper.cpp:169-178 at v24.0.2
pub fn obfuscate_key(chainstate: &Path) -> crate::Result<ObfuscateKey> {
    let value = LevelDbReader::open_tolerant(chainstate)?.get(OBFUSCATE_KEY_KEY)?;
    Ok(match value.as_deref() {
        None => ObfuscateKey::Absent,
        Some([8, key @ ..]) if key.len() == 8 => {
            ObfuscateKey::Present(key.try_into().expect("eight bytes"))
        }
        Some(_) => ObfuscateKey::Malformed,
    })
}

/// An import whose databases have moved (`Moved`): the caller stores its reindex flag, then
/// calls [`complete`](Self::complete) before the first block-file record is scanned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveAside {
    net: PathBuf,
    state: JournalState,
}

impl MoveAside {
    pub fn state(&self) -> &JournalState {
        &self.state
    }

    /// The backup directory.
    pub fn backup(&self) -> PathBuf {
        self.net.join(&self.state.backup)
    }

    /// S7 and S8: `Complete`, then the journal moves into the backup. Returns the backup.
    pub fn complete(mut self, fs: &dyn ImportFs) -> Result<PathBuf> {
        self.state.phase = Phase::Complete;
        write_state(fs, &self.net, &self.state)?;
        cleanup(fs, &self.net, &self.state)?;
        Ok(self.backup())
    }
}

/// What [`recover`] found and did.
#[derive(Debug, PartialEq, Eq)]
pub enum Recovered {
    /// No journal: no import is in progress.
    Clean,
    /// The databases have moved; the caller continues at S6.
    Moved(MoveAside),
    /// A completed import's journal was moved into its backup (S8).
    Finished {
        backup: PathBuf,
    },
}

/// The journal in `net`, if any.
pub fn read_journal(fs: &dyn ImportFs, net: &Path) -> Result<Option<JournalState>> {
    let path = net.join(JOURNAL);
    if fs.metadata(&path).map_err(io_at(&path))?.is_none() {
        return Ok(None);
    }
    let raw = fs.read(&path).map_err(io_at(&path))?;
    JournalState::decode(&raw).map(Some).map_err(|detail| RegtestImportError::JournalCorrupt {
        path,
        detail,
    })
}

/// S1 to S5 on the Core network directory `net`: the import's intent, the moves, `Moved`.
pub fn begin(fs: &dyn ImportFs, net: &Path) -> Result<MoveAside> {
    let journal = net.join(JOURNAL);
    if fs.metadata(&journal).map_err(io_at(&journal))?.is_some() {
        return Err(RegtestImportError::JournalExists(journal));
    }
    let (real, dev, mount) = net_identity(fs, net)?;
    let blocks = net.join("blocks");
    if fs.metadata(&blocks).map_err(io_at(&blocks))?.is_some_and(|m| !is_dir(&m)) {
        return Err(RegtestImportError::Member {
            path: blocks,
            detail: "not a plain directory",
        });
    }
    let mut members = [MemberId::default(); MEMBERS.len()];
    for (id, name) in members.iter_mut().zip(MEMBERS) {
        let path = net.join(name);
        let refuse = |detail| RegtestImportError::Member {
            path: path.clone(),
            detail,
        };
        match fs.metadata(&path).map_err(io_at(&path))? {
            None => {}
            Some(m) if !is_dir(&m) => return Err(refuse("not a directory")),
            Some(m) if m.dev != dev => return Err(refuse("on another filesystem")),
            Some(m) => {
                let here = fs.mount_id(&real.join(name)).map_err(io_at(&path))?;
                if mount.zip(here).is_some_and(|(a, b)| a != b) {
                    return Err(refuse("on another mount"));
                }
                *id = MemberId {
                    present: true,
                    dev: m.dev,
                    ino: m.ino,
                };
            }
        }
    }
    if !members[0].present {
        return Err(RegtestImportError::Member {
            path: net.join(MEMBERS[0]),
            detail: "no chainstate to import",
        });
    }
    let at_ms = fs.now_ms();
    let state = JournalState {
        phase: Phase::Intent,
        at_ms,
        backup: free_backup_name(fs, net, at_ms)?,
        members,
    };
    write_state(fs, net, &state)?;
    finish_intent(fs, net, state)
}

/// Resumes an import from its journal in `net` (see the module table). Without one, a stray
/// `core-import.journal.new` file is removed; with one, the next journal write replaces it. A
/// stray directory or symlink is refused.
pub fn recover(fs: &dyn ImportFs, net: &Path) -> Result<Recovered> {
    let state = read_journal(fs, net)?;
    let stray = stray_journal(fs, net)?;
    let Some(state) = state else {
        if let Some(stray) = stray {
            fs.remove(&stray).map_err(io_at(&stray))?;
            fs.sync_dir(net).map_err(io_at(net))?;
        }
        return Ok(Recovered::Clean);
    };
    match state.phase {
        Phase::Intent => finish_intent(fs, net, state).map(Recovered::Moved),
        Phase::Moved => Ok(Recovered::Moved(MoveAside {
            net: net.to_owned(),
            state,
        })),
        Phase::Complete => {
            cleanup(fs, net, &state)?;
            Ok(Recovered::Finished {
                backup: net.join(&state.backup),
            })
        }
    }
}

/// `core-import-<yyyymmddThhmmssZ>` for the epoch milliseconds `at_ms`.
fn utc_name(at_ms: u64) -> String {
    let secs = at_ms / 1000;
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 {
        mp + 3
    } else {
        mp - 9
    };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{BACKUP_PREFIX}{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

fn free_backup_name(fs: &dyn ImportFs, net: &Path, at_ms: u64) -> Result<String> {
    let base = utc_name(at_ms);
    for n in 0..=LAST_SUFFIX {
        let name = if n == 0 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        let path = net.join(&name);
        if fs.metadata(&path).map_err(io_at(&path))?.is_none() {
            return Ok(name);
        }
    }
    Err(RegtestImportError::NoFreeBackupName(net.to_owned()))
}

/// The path of a leftover `core-import.journal.new` that is not a directory or symlink, if any;
/// a directory or symlink there is refused.
fn stray_journal(fs: &dyn ImportFs, net: &Path) -> Result<Option<PathBuf>> {
    let new = net.join(JOURNAL_NEW);
    match fs.metadata(&new).map_err(io_at(&new))? {
        None => Ok(None),
        Some(m) if !m.is_dir && !m.is_symlink => Ok(Some(new)),
        Some(_) => Err(RegtestImportError::Member {
            path: new,
            detail: "a directory or symlink",
        }),
    }
}

/// `net`'s real path, filesystem (`st_dev`) and mount id, through any symlink to it.
fn net_identity(fs: &dyn ImportFs, net: &Path) -> Result<(PathBuf, u64, Option<u64>)> {
    let real = fs.canonicalize(net).map_err(io_at(net))?;
    let dev = match fs.metadata(&real).map_err(io_at(&real))? {
        Some(m) if is_dir(&m) => m.dev,
        _ => {
            return Err(RegtestImportError::Member {
                path: net.to_owned(),
                detail: "not a directory",
            });
        }
    };
    let mount = fs.mount_id(&real).map_err(io_at(&real))?;
    Ok((real, dev, mount))
}

/// Replaces the journal with `state`: written aside, synced, renamed over, the dir synced.
fn write_state(fs: &dyn ImportFs, net: &Path, state: &JournalState) -> Result<()> {
    let new = net.join(JOURNAL_NEW);
    let journal = net.join(JOURNAL);
    if let Some(stray) = stray_journal(fs, net)? {
        fs.remove(&stray).map_err(io_at(&stray))?;
    }
    fs.append(&new, &state.encode()).map_err(io_at(&new))?;
    fs.sync_data(&new).map_err(io_at(&new))?;
    fs.rename(&new, &journal).map_err(io_at(&journal))?;
    fs.sync_dir(net).map_err(io_at(net))
}

fn parent(path: &Path) -> &Path {
    path.parent().expect("a member path has a parent")
}

/// The backup's top-level names the import may have made: the members' first components.
fn allowed(name: &OsStr, members: &[MemberId]) -> bool {
    MEMBERS.iter().zip(members).any(|(m, id)| {
        id.present && Path::new(m).components().next().is_some_and(|c| c.as_os_str() == name)
    })
}

fn is_dir(m: &FileMetadata) -> bool {
    m.is_dir && !m.is_symlink
}

/// Refuses a backup entry the import did not make: the backup (or its `blocks/`) must be absent,
/// or a directory holding only what `allowed` names.
fn check_backup_dir(fs: &dyn ImportFs, dir: &Path, allowed: impl Fn(&OsStr) -> bool) -> Result<()> {
    match fs.metadata(dir).map_err(io_at(dir))? {
        None => Ok(()),
        Some(m) if is_dir(&m) => {
            for entry in fs.read_dir(dir).map_err(io_at(dir))? {
                if !entry.file_name().is_some_and(&allowed) {
                    return Err(RegtestImportError::ForeignBackupEntry {
                        backup: dir.to_owned(),
                        entry,
                    });
                }
            }
            Ok(())
        }
        Some(_) => Err(RegtestImportError::Member {
            path: dir.to_owned(),
            detail: "not a directory",
        }),
    }
}

/// Whether a member still has to move: `true` at its place, `false` at the backup, with its
/// recorded `st_ino` on `net`'s current filesystem `dev`; anything else is refused.
fn member_to_move(
    fs: &dyn ImportFs,
    src: &Path,
    dst: &Path,
    id: MemberId,
    dev: u64,
) -> Result<bool> {
    let same = |m: &FileMetadata| is_dir(m) && m.ino == id.ino && m.dev == dev;
    let refuse = |path: &Path, detail| RegtestImportError::Member {
        path: path.to_owned(),
        detail,
    };
    match (fs.metadata(src).map_err(io_at(src))?, fs.metadata(dst).map_err(io_at(dst))?) {
        (Some(m), None) if same(&m) => Ok(true),
        (None, Some(m)) if same(&m) => Ok(false),
        (Some(_), Some(_)) => Err(refuse(src, "at both its place and the backup")),
        (None, None) => Err(refuse(src, "at neither its place nor the backup")),
        (Some(_), None) => Err(refuse(src, "changed since the import began")),
        (None, Some(_)) => Err(refuse(dst, "changed since the import began")),
    }
}

/// S3 and S4 from the durable `Intent`, then S5. Every refusal comes before the first write.
fn finish_intent(fs: &dyn ImportFs, net: &Path, mut state: JournalState) -> Result<MoveAside> {
    let backup = net.join(&state.backup);
    let blocks = backup.join("blocks");
    let moves_index = state.members[1].present;
    let (_, dev, _) = net_identity(fs, net)?;
    check_backup_dir(fs, &backup, |name| allowed(name, &state.members))?;
    if moves_index {
        check_backup_dir(fs, &blocks, |name| name == "index")?;
    }
    let mut pending = Vec::new();
    for (name, id) in MEMBERS.iter().zip(state.members).filter(|(_, id)| id.present) {
        let (src, dst) = (net.join(name), backup.join(name));
        let to_move = member_to_move(fs, &src, &dst, id, dev)?;
        pending.push((src, dst, to_move));
    }
    fs.create_dir_all(&backup).map_err(io_at(&backup))?;
    if moves_index {
        fs.create_dir_all(&blocks).map_err(io_at(&blocks))?;
    }
    fs.sync_dir(net).map_err(io_at(net))?;
    for (src, dst, to_move) in pending {
        if to_move {
            fs.rename(&src, &dst).map_err(io_at(&src))?;
        }
        for dir in [parent(&src), parent(&dst)] {
            fs.sync_dir(dir).map_err(io_at(dir))?;
        }
    }
    state.phase = Phase::Moved;
    write_state(fs, net, &state)?;
    Ok(MoveAside {
        net: net.to_owned(),
        state,
    })
}

/// S8: the completed journal moves into the backup.
fn cleanup(fs: &dyn ImportFs, net: &Path, state: &JournalState) -> Result<()> {
    let backup = net.join(&state.backup);
    let journal = net.join(JOURNAL);
    fs.rename(&journal, &backup.join(JOURNAL)).map_err(io_at(&journal))?;
    fs.sync_dir(net).map_err(io_at(net))?;
    fs.sync_dir(&backup).map_err(io_at(&backup))
}
