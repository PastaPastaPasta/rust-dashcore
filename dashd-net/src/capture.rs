//! `-capturemessages`: Core's `CaptureMessageToFile` (P4a D10).
//!
//! Each peer gets `<datadir>/<net>/message_capture/<addr:port, ':' → '_'>/msgs_recv.dat` and
//! `msgs_sent.dat`. A record is the mockable node time in µs (i64 LE), the message type
//! NUL-padded to 12 bytes, the payload size (u32 LE) and the payload, appended in processing
//! order (Core captures when a message is pushed or taken for processing, not at the socket).

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use dash_p2p::Address;
use dash_p2p::capture::MessageCapture;
use parking_lot::Mutex;

/// Core's `CMessageHeader::COMMAND_SIZE`.
const COMMAND_SIZE: usize = 12;

/// How many capture files' write failures are remembered for the warn-once latch; past it, a
/// failure on a further file logs at debug level only.
const MAX_WARNED: usize = 1024;

/// The capture files under a network's data directory.
#[derive(Debug)]
pub struct FileCapture {
    root: PathBuf,
    /// The files (one per peer address and direction) whose write failure was already warned
    /// about: the next failures there log at debug level, so a peer cannot set the warn rate
    /// (Opus-h P4a r1 item 4, BL-214).
    warned: Mutex<HashSet<PathBuf>>,
}

impl FileCapture {
    /// Captures under `net_dir/message_capture` (`net_dir` is Core's `GetDataDirNet()`).
    pub fn new(net_dir: &Path) -> Self {
        Self {
            root: net_dir.join("message_capture"),
            warned: Mutex::new(HashSet::new()),
        }
    }

    /// Whether a write failure on `path` is the first one to warn about: once per file, and
    /// not past [`MAX_WARNED`] files.
    fn first_failure(&self, path: &Path) -> bool {
        let mut warned = self.warned.lock();
        warned.len() < MAX_WARNED && !warned.contains(path) && warned.insert(path.to_path_buf())
    }
}

/// One record's bytes.
fn record(time_us: i64, cmd: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + COMMAND_SIZE + 4 + payload.len());
    out.extend_from_slice(&time_us.to_le_bytes());
    out.extend_from_slice(cmd.as_bytes());
    out.resize(8 + cmd.len().max(COMMAND_SIZE), 0);
    let size = u32::try_from(payload.len()).expect("a P2P payload fits in 32 bits");
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

impl MessageCapture for FileCapture {
    // Core: net.cpp:5022-5050 CaptureMessageToFile
    fn capture(&self, time_us: i64, addr: &Address, cmd: &str, payload: &[u8], incoming: bool) {
        // Windows folder names cannot hold a colon.
        let dir = self.root.join(addr.service.to_string().replace(':', "_"));
        let path = dir.join(if incoming {
            "msgs_recv.dat"
        } else {
            "msgs_sent.dat"
        });
        let written = fs::create_dir_all(&dir).and_then(|()| {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?
                .write_all(&record(time_us, cmd, payload))
        });
        // Core's `AutoFile::write` would throw out of `PushMessage`; the node logs instead and
        // the message is still sent or processed (P4a D10), warning once per file (BL-214).
        if let Err(e) = written {
            if self.first_failure(&path) {
                tracing::warn!(target: "net", "Failed to capture message to {}: {e} (further failures for this file are logged at debug level)", path.display());
            } else {
                tracing::debug!(target: "net", "Failed to capture message to {}: {e}", path.display());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use dash_p2p::{NetAddr, Service};

    use super::*;

    /// The file format Core's `test/functional/p2p_message_capture.py` reads: time, the
    /// NUL-padded type, the size, the payload, records back to back per direction.
    #[test]
    fn files_are_cores() {
        let dir = std::env::temp_dir().join(format!("p4a-capture-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let cap = FileCapture::new(&dir);
        let t = 1_700_000_000_000_000;
        let v4 = Address {
            service: Service::new(NetAddr::from_ip("127.0.0.1".parse().unwrap()), 40001),
            ..Address::default()
        };
        let v6 = Address {
            service: Service::new(NetAddr::from_ip("::1".parse().unwrap()), 40002),
            ..Address::default()
        };
        cap.capture(t, &v4, "version", &[1, 2, 3], true);
        cap.capture(t + 1, &v4, "verack", &[], true);
        cap.capture(t, &v4, "sendheaders2", &[], false);
        cap.capture(t, &v6, "ping", &[9; 8], false);

        let recv = fs::read(dir.join("message_capture/127.0.0.1_40001/msgs_recv.dat")).unwrap();
        let mut want = record(t, "version", &[1, 2, 3]);
        want.extend(record(t + 1, "verack", &[]));
        assert_eq!(recv, want);
        assert_eq!(&recv[..8], &t.to_le_bytes());
        assert_eq!(&recv[8..20], b"version\0\0\0\0\0");
        assert_eq!(&recv[20..24], &3u32.to_le_bytes());
        let sent = fs::read(dir.join("message_capture/127.0.0.1_40001/msgs_sent.dat")).unwrap();
        assert_eq!(&sent[8..20], b"sendheaders2");
        assert!(dir.join("message_capture/[__1]_40002/msgs_sent.dat").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A failing write warns once per file (peer address and direction), then logs at debug
    /// level; the remembered set is bounded (BL-214).
    #[test]
    fn write_failures_warn_once_per_file() {
        let dir = std::env::temp_dir().join(format!("p4a-capture-fail-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // `message_capture` is a file, so every directory create fails.
        fs::write(dir.join("message_capture"), b"").unwrap();
        let cap = FileCapture::new(&dir);
        let peer = |port| Address {
            service: Service::new(NetAddr::from_ip("127.0.0.1".parse().unwrap()), port),
            ..Address::default()
        };
        for _ in 0..3 {
            cap.capture(1, &peer(1), "ping", &[0; 8], true);
            cap.capture(1, &peer(1), "pong", &[0; 8], false);
            cap.capture(1, &peer(2), "ping", &[0; 8], true);
        }
        assert_eq!(cap.warned.lock().len(), 3, "one per peer and direction");
        let recv = cap.root.join("127.0.0.1_1/msgs_recv.dat");
        assert!(!cap.first_failure(&recv), "already warned");
        for n in 0..MAX_WARNED {
            cap.first_failure(Path::new(&format!("x{n}")));
        }
        assert_eq!(cap.warned.lock().len(), MAX_WARNED);
        assert!(!cap.first_failure(Path::new("past-the-cap")));
        fs::remove_dir_all(&dir).unwrap();
    }
}
