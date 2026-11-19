//! O15-2: the live shadow's peer adapter against a running StateDigest dashd on regtest
//! (`O15_PEER_DASHD`), its enrichment and its `gettxoutsetinfo` bestblock check across a reorg.
#![cfg(unix)]

use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use dash_conformance::rpc::{Auth, Rpc};
use dash_conformance::shadow::live::{LivePeer, LiveRdashd};
use dash_conformance::shadow::{Enrichment, PeerRpc, RdashdRpc};
use dash_statedigest::{FieldSource, Kind, Lines, Origin, Outcome, Record, Value};
use serde_json::{Value as Json, json};

fn peer_binary() -> Option<PathBuf> {
    let path = std::env::var_os("O15_PEER_DASHD").map(PathBuf::from);
    if path.is_none() {
        eprintln!("skipped: set O15_PEER_DASHD to a StateDigest dashd to run the live O15 tests");
    }
    path
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A regtest dashd in its own 0700 home under `TMPDIR`, killed when dropped.
struct Peer {
    child: Child,
    rpc: Rpc,
    url: String,
    home: tempfile::TempDir,
}

impl Peer {
    fn start(binary: &Path) -> Peer {
        let home = tempfile::Builder::new().prefix("o15-2-live-").tempdir().unwrap();
        fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let port = free_port();
        let child = Command::new(binary)
            .arg(format!("-datadir={}", home.path().display()))
            .arg(format!("-rpcport={port}"))
            .args([
                "-regtest",
                "-daemon=0",
                "-printtoconsole=0",
                "-listen=0",
                "-dnsseed=0",
                "-fixedseeds=0",
                "-disablewallet",
                "-coinstatsindex=1",
                "-statedigestlog=digest.jsonl",
                "-rpcuser=u",
                "-rpcpassword=p",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let url = format!("http://127.0.0.1:{port}");
        let mut peer = Peer {
            child,
            rpc: Rpc::with_timeout(url.clone(), auth(), Duration::from_secs(30)),
            url,
            home,
        };
        let deadline = Instant::now() + Duration::from_secs(120);
        while peer.rpc.call("getblockcount", json!([])).is_err() {
            assert!(peer.child.try_wait().unwrap().is_none(), "dashd exited during init");
            assert!(Instant::now() < deadline, "dashd did not finish init");
            std::thread::sleep(Duration::from_millis(200));
        }
        peer
    }

    fn call(&self, method: &str, params: Json) -> Json {
        self.rpc.call(method, params).unwrap_or_else(|e| panic!("{method}: {e:#}"))
    }

    fn mine(&self, count: u32, script: &str) -> Vec<String> {
        let hashes = self.call("generatetodescriptor", json!([count, format!("raw({script})")]));
        serde_json::from_value(hashes).unwrap()
    }

    /// Waits until the coinstatsindex has caught up with the tip.
    fn indexed(&self) -> i64 {
        let tip = self.call("getblockcount", json!([])).as_i64().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let info = self.call("getindexinfo", json!([]));
            if info["coinstatsindex"]["best_block_height"].as_i64() == Some(tip) {
                return tip;
            }
            assert!(Instant::now() < deadline, "coinstatsindex stuck: {info}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The tip-origin `C` records the peer has logged so far, in file order.
    fn connects(&self) -> Vec<Outcome> {
        let bytes = fs::read(self.home.path().join("regtest/digest.jsonl")).unwrap();
        let end = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        Lines::new(&bytes[..end])
            .filter_map(|(_, record)| match record.unwrap() {
                Record::Outcome(o) if o.kind() == Kind::Connect && o.id.org == Origin::Tip => {
                    Some(o)
                }
                _ => None,
            })
            .collect()
    }

    fn connect_at(&self, hash: &str) -> Outcome {
        self.connects().into_iter().rev().find(|o| o.id.b.to_hex() == hash).unwrap()
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn auth() -> Auth {
    Auth::UserPass("u".into(), "p".into())
}

fn golden_keys() -> impl Iterator<Item = &'static str> {
    dash_statedigest::group(Kind::Connect, dash_statedigest::Class::Semantic)
        .iter()
        .filter(|f| f.source == FieldSource::Golden)
        .map(|f| f.key)
}

fn ready(enrichment: Enrichment) -> Outcome {
    match enrichment {
        Enrichment::Ready(o) => o,
        Enrichment::Reorg => panic!("enrichment saw a reorg"),
    }
}

#[test]
fn live_peer_enriches_and_detects_a_reorged_slot() {
    let Some(binary) = peer_binary() else {
        return;
    };
    let peer = Peer::start(&binary);
    let first = peer.mine(10, "51");
    assert_eq!(peer.indexed(), 10);

    let live = LivePeer::new(peer.url.clone(), auth(), Duration::from_secs(10));
    let status = live.status().unwrap();
    assert_eq!((status.index_height, status.headers), (10, 10));
    // No ChainLocks on this regtest: the optional call answers an error, which is no ChainLock.
    assert!(status.chainlock.is_none());
    // The same `getblockchaininfo.warnings` the rdashd adapter reads: none on a quiet node.
    assert_eq!(
        LiveRdashd::new(peer.url.clone(), auth(), Duration::from_secs(10))
            .status()
            .unwrap()
            .warnings,
        ""
    );

    let at8 = peer.connect_at(&first[7]);
    assert_eq!(at8.id.h, 8);
    assert!(golden_keys().all(|k| at8.s().get(k).is_none()), "the raw digest has golden keys");
    let enriched = ready(live.enrich(&at8).unwrap());
    assert_eq!(enriched.id, at8.id);
    assert!(golden_keys().all(|k| enriched.s().get(k).is_some()));
    let info = peer.call("gettxoutsetinfo", json!(["muhash", 8, true]));
    assert_eq!(
        enriched.s().get("muhash"),
        Some(&Value::Str(info["muhash"].as_str().unwrap().as_bytes().to_vec()))
    );
    assert_eq!(enriched.s().get("txouts"), Some(&Value::Int(info["txouts"].as_i64().unwrap())));

    // Reorg the top three blocks onto a branch with another coinbase script.
    peer.call("invalidateblock", json!([first[7]]));
    let branch = peer.mine(4, "52");
    assert_eq!(peer.indexed(), 11);
    assert_ne!(branch[0], first[7]);

    // The slot at height 8 now holds another block: the bestblock check reports the reorg.
    assert!(matches!(live.enrich(&at8).unwrap(), Enrichment::Reorg));
    // Below the fork the slot is unchanged and still enriches.
    let at7 = peer.connect_at(&first[6]);
    assert_eq!(ready(live.enrich(&at7).unwrap()).id, at7.id);
    // The branch's own record at height 8 enriches, with its own UTXO-set hash.
    let branch8 = peer.connect_at(&branch[0]);
    assert_eq!(branch8.id.h, 8);
    let branch8 = ready(live.enrich(&branch8).unwrap());
    assert_ne!(branch8.s().get("muhash"), enriched.s().get("muhash"));
}

#[test]
fn live_peer_down_is_an_error_not_a_panic() {
    let Some(binary) = peer_binary() else {
        return;
    };
    let peer = Peer::start(&binary);
    peer.mine(1, "51");
    let live = LivePeer::new(peer.url.clone(), auth(), Duration::from_secs(2));
    let wrong = LivePeer::new(
        peer.url.clone(),
        Auth::UserPass("u".into(), "wrong".into()),
        Duration::from_secs(2),
    );
    assert!(wrong.status().is_err());
    let at1 = peer.connects().pop().unwrap();
    drop(peer);
    let started = Instant::now();
    assert!(live.status().is_err());
    assert!(live.enrich(&at1).is_err());
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}
