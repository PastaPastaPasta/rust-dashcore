//! After `interrupt` the core moves no bytes, as Core's socket thread stops at `interruptNet`
//! (net.cpp:2446-2555); and `shutdown` dumps a dirty ban list whether or not the addresses were
//! loaded, as `~BanMan` does (banman.cpp:23-26).

mod common;

use std::io;
use std::sync::Arc;
use std::time::Duration;

use common::{Events, Parts, attach, core_with, settle};
use dash_clock::MockClock;
use dash_p2p::store::{NetBlob, NetStore};
use dash_p2p::{CommandString, MemSocket, NetAddr, NetCore, OutMsg, SubNet};
use parking_lot::Mutex;

/// Keeps every saved image.
#[derive(Default)]
struct Saves(Mutex<Vec<(NetBlob, Vec<u8>)>>);

impl Saves {
    fn bans(&self) -> Vec<Vec<u8>> {
        let saves = self.0.lock();
        saves.iter().filter(|(b, _)| *b == NetBlob::Bans).map(|(_, v)| v.clone()).collect()
    }
}

impl NetStore for Saves {
    fn load(&self, _blob: NetBlob) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn save(&self, blob: NetBlob, bytes: &[u8]) -> io::Result<()> {
        self.0.lock().push((blob, bytes.to_vec()));
        Ok(())
    }

    fn remove(&self, _blob: NetBlob) -> io::Result<()> {
        Ok(())
    }

    fn backup(&self, _blob: NetBlob) -> io::Result<()> {
        Ok(())
    }
}

fn core(store: Arc<dyn NetStore>, clock: Arc<MockClock>) -> Arc<NetCore> {
    core_with(
        Parts {
            store,
            clock,
            events: Arc::new(Events),
        },
        |_| {},
    )
}

/// n7: once interrupted, a push is not sent and the peer's bytes are not read.
#[tokio::test(start_paused = true)]
async fn nothing_moves_after_interrupt() {
    let core = core(Arc::new(Saves::default()), Arc::new(MockClock::new()));
    let (ours, theirs) = MemSocket::pair();
    let peer = attach(&core, ours, [10, 0, 0, 1]);
    settle().await;
    theirs.drain();

    core.interrupt();
    settle().await;
    core.push(&peer, OutMsg::new(CommandString::from_static("ping"), vec![0; 8]));
    theirs.inject(&[0; 24]);
    settle().await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    settle().await;

    assert!(theirs.drain().is_empty(), "sent after interrupt");
    assert_eq!(theirs.in_flight(), 24, "read after interrupt");
}

/// n14: a ban that expired since the last dump makes the list dirty; `shutdown` writes it
/// though `start_addresses` never ran.
#[tokio::test]
async fn shutdown_dumps_bans_without_start_addresses() {
    let store = Arc::new(Saves::default());
    let clock = Arc::new(MockClock::at(1_700_000_000));
    let core = core(store.clone(), clock.clone());
    core.ban(SubNet::single(NetAddr::ipv4([10, 0, 0, 9])), 10, false);
    let dumped = store.bans();
    assert!(String::from_utf8_lossy(dumped.last().unwrap()).contains("10.0.0.9"));

    clock.set_mocktime(1_700_000_100);
    core.shutdown().await;
    let after = store.bans();
    assert_eq!(after.len(), dumped.len() + 1, "no dump at shutdown");
    assert!(!String::from_utf8_lossy(after.last().unwrap()).contains("10.0.0.9"));
}
