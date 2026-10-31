//! `peers.dat`, `anchors.dat` and `banlist.json` as files in the network's data directory
//! (P2a §4.1–4.2), behind SB6's [`NetStore`]. SB6 builds the Core-format images; this store
//! writes them as Core's `SerializeFileDB` does: a temporary file `<prefix>.<4 hex digits>` in
//! the same directory, flushed to disk, then renamed over the target. Then, unlike Core, it
//! syncs the directory so the rename is durable (P2a §4.1).

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use dash_p2p::store::{NetBlob, NetStore};

/// The node's file store for the network core's images.
#[derive(Clone, Debug)]
pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    /// A store in `dir` (Core's `GetDataDirNet()`), which must exist.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
        }
    }

    /// The image's path.
    pub fn path(&self, blob: NetBlob) -> PathBuf {
        self.dir.join(blob.file_name())
    }
}

/// Core's `FileCommit` (`fdatasync` where the OS has it) and `RenameOver`.
fn write_atomically(tmp: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::File::create(tmp)?;
    file.write_all(bytes)?;
    file.sync_data()?;
    drop(file);
    fs::rename(tmp, path)
}

/// Makes the entries of `dir` durable (`fsync` of the directory). Windows has no directory
/// handle to sync; its renames go through the file system's journal.
fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    fs::File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

impl NetStore for FileStore {
    fn load(&self, blob: NetBlob) -> io::Result<Option<Vec<u8>>> {
        // Core: addrdb.cpp:153-157 CBanDB::Read, before it reads banlist.json (P4a D2).
        if blob == NetBlob::Bans
            && let Some(warning) = crate::bans::legacy_banlist_warning(&self.dir)
        {
            tracing::info!(target: "net", "{warning}");
        }
        match fs::read(self.path(blob)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    // Core: addrdb.cpp:54-93 SerializeFileDB
    fn save(&self, blob: NetBlob, bytes: &[u8]) -> io::Result<()> {
        // Core: `GetRand<uint16_t>()` from the global RNG, not the network core's stream.
        let r = dash_p2p::rng::os_rand256();
        let randv = u16::from_le_bytes([r[0], r[1]]);
        // Core's prefix is the file's stem: "peers", "anchors".
        let path = self.path(blob);
        let tmp = path.with_extension(format!("{randv:04x}"));
        write_atomically(&tmp, &path, bytes).inspect_err(|e| {
            let _ = fs::remove_file(&tmp);
            tracing::error!(target: "net", "SerializeFileDB: Failed to write {}: {e}", tmp.display());
        })?;
        // The new image is in place; a file system that cannot sync a directory only loses
        // the durability step Core never takes.
        if let Err(e) = sync_dir(&self.dir) {
            tracing::warn!(target: "net", "Failed to sync {} after writing {}: {e}", self.dir.display(), path.display());
        }
        Ok(())
    }

    // Core: addrdb.cpp:249 (`fs::remove`; a missing file is not an error)
    fn remove(&self, blob: NetBlob) -> io::Result<()> {
        match fs::remove_file(self.path(blob)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    // Core: addrdb.cpp:219 (`RenameOver(path_addr, path_addr + ".bak")`)
    fn backup(&self, blob: NetBlob) -> io::Result<()> {
        let path = self.path(blob);
        let mut bak = path.clone().into_os_string();
        bak.push(".bak");
        fs::rename(&path, bak)
    }

    fn location(&self, blob: NetBlob) -> String {
        self.path(blob).display().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dashd-net-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_load_remove_backup() {
        let dir = scratch("roundtrip");
        let store = FileStore::new(&dir);
        assert_eq!(store.load(NetBlob::Peers).unwrap(), None);
        store.save(NetBlob::Peers, b"one").unwrap();
        store.save(NetBlob::Peers, b"two").unwrap();
        assert_eq!(store.load(NetBlob::Peers).unwrap().as_deref(), Some(&b"two"[..]));
        // Only the target is left: the temporary file was renamed over it.
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["peers.dat"]);

        store.backup(NetBlob::Peers).unwrap();
        assert_eq!(store.load(NetBlob::Peers).unwrap(), None);
        assert_eq!(fs::read(dir.join("peers.dat.bak")).unwrap(), b"two");

        store.save(NetBlob::Anchors, b"a").unwrap();
        store.remove(NetBlob::Anchors).unwrap();
        store.remove(NetBlob::Anchors).unwrap();
        assert_eq!(store.load(NetBlob::Anchors).unwrap(), None);
        assert_eq!(store.location(NetBlob::Bans), dir.join("banlist.json").display().to_string());
        fs::remove_dir_all(&dir).unwrap();
    }
}
