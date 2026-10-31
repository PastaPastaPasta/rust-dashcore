//! Persistence for the network core: AddrMan, anchors and bans, as Core-format images.
//!
//! The core serializes (`peers.dat` with its magic and checksum, `anchors.dat`, the ban list);
//! the host stores the bytes: files with tmp + fsync + rename in the node, an SQLite table in SPV.
//! Calls happen at start, every 15 minutes (`DumpAddresses`) and at shutdown, never under a lock
//! of the core's.

use std::io;

/// Which image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NetBlob {
    /// `peers.dat`.
    Peers,
    /// `anchors.dat`. Core deletes it right after reading.
    Anchors,
    /// The ban list (`banlist.json` in Core).
    Bans,
}

impl NetBlob {
    /// Core's file name for the image.
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::Peers => "peers.dat",
            Self::Anchors => "anchors.dat",
            Self::Bans => "banlist.json",
        }
    }
}

/// The host's store for the network core's images.
pub trait NetStore: Send + Sync {
    /// The stored image, or `None` when there is none.
    fn load(&self, blob: NetBlob) -> io::Result<Option<Vec<u8>>>;

    /// Replaces the image atomically.
    fn save(&self, blob: NetBlob, bytes: &[u8]) -> io::Result<()>;

    /// Removes the image (anchors after reading); a missing image is not an error.
    fn remove(&self, blob: NetBlob) -> io::Result<()>;

    /// Moves the image aside (Core renames an unsupported `peers.dat` to `peers.dat.bak`).
    fn backup(&self, blob: NetBlob) -> io::Result<()>;

    /// Where the image lives, for log lines that name it (Core logs the file's path). The
    /// default is the file name.
    fn location(&self, blob: NetBlob) -> String {
        blob.file_name().to_owned()
    }
}

/// A store that keeps nothing (mobile with `persist_addrman = false`, tests).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoStore;

impl NetStore for NoStore {
    fn load(&self, _blob: NetBlob) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn save(&self, _blob: NetBlob, _bytes: &[u8]) -> io::Result<()> {
        Ok(())
    }

    fn remove(&self, _blob: NetBlob) -> io::Result<()> {
        Ok(())
    }

    fn backup(&self, _blob: NetBlob) -> io::Result<()> {
        Ok(())
    }
}
