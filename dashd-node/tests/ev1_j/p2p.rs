//! A minimal v1 P2P peer of the producer, for Core's own route of a final commitment into its
//! miner: announce it with `inv` (MSG_QUORUM_FINAL_COMMITMENT), answer the `getdata` dashd sends,
//! since rc.5 drops a `qfcommit` it did not request (`llmq/blockprocessor.cpp:109-119`), then
//! `ping` until the `pong`: dashd handles one peer's messages in order, so the commitment has
//! been processed by then.
//!
//! One connection per commitment, at one mocktime: dashd's inactivity checks read the mocked
//! clock, and the producer moves it 150 s per block.

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use dash_consensus::hash::sha256d;

/// Regtest's message start (`chainparams.cpp`, CRegTestParams `pchMessageStart`).
const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];
/// rc.5's `PROTOCOL_VERSION` (`version.h:14`).
const VERSION: i32 = 70242;
/// `MSG_QUORUM_FINAL_COMMITMENT` (`protocol.h:514`).
const MSG_QUORUM_FINAL_COMMITMENT: u32 = 21;
const DEADLINE: Duration = Duration::from_secs(120);

struct Peer {
    stream: TcpStream,
}

impl Peer {
    /// The v1 handshake with the dashd on `port`, at mocktime `time`.
    fn connect(port: u16, time: i64) -> Peer {
        let stream = TcpStream::connect(("127.0.0.1", port)).expect("the producer listens");
        stream.set_read_timeout(Some(DEADLINE)).unwrap();
        let mut peer = Peer {
            stream,
        };
        // `msg_version` as test_framework/messages.py writes it: no services, both addresses
        // without a time, relay set, then Dash's MNAUTH challenge and masternode flag.
        let mut version = Vec::new();
        version.extend(VERSION.to_le_bytes());
        version.extend(0u64.to_le_bytes());
        version.extend(time.to_le_bytes());
        for _ in 0..2 {
            version.extend(0u64.to_le_bytes());
            version.extend([0u8; 10]);
            version.extend([0xff, 0xff, 127, 0, 0, 1]);
            version.extend(0u16.to_be_bytes());
        }
        version.extend(0x0123_4567_89ab_cdefu64.to_le_bytes());
        version.push(0);
        version.extend((-1i32).to_le_bytes());
        version.push(1);
        version.extend([0u8; 32]);
        version.push(0);
        peer.send("version", &version);
        let (mut version_seen, mut verack_seen) = (false, false);
        while !(version_seen && verack_seen) {
            let (command, _) = peer.recv();
            match command.as_str() {
                "version" => {
                    version_seen = true;
                    peer.send("verack", &[]);
                }
                "verack" => verack_seen = true,
                _ => {}
            }
        }
        peer
    }

    fn send(&mut self, command: &str, payload: &[u8]) {
        let mut name = [0u8; 12];
        name[..command.len()].copy_from_slice(command.as_bytes());
        let mut message = MAGIC.to_vec();
        message.extend(name);
        message.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
        message.extend(&sha256d(payload)[..4]);
        message.extend(payload);
        self.stream.write_all(&message).expect("the producer reads");
    }

    /// The next message, with a `ping` answered on the way.
    fn recv(&mut self) -> (String, Vec<u8>) {
        loop {
            let mut header = [0u8; 24];
            self.stream.read_exact(&mut header).expect("the producer writes");
            assert_eq!(header[..4], MAGIC, "a v1 message");
            let name = &header[4..16];
            let command = String::from_utf8(name.iter().copied().take_while(|b| *b != 0).collect())
                .expect("an ASCII command");
            let len = u32::from_le_bytes(header[16..20].try_into().unwrap());
            let mut payload = vec![0u8; len as usize];
            self.stream.read_exact(&mut payload).expect("the payload");
            assert_eq!(header[20..24], sha256d(&payload)[..4], "{command}'s checksum");
            if command == "ping" {
                self.send("pong", &payload);
                continue;
            }
            return (command, payload);
        }
    }
}

/// One `inv` or `getdata` vector of a single item.
fn inventory(kind: u32, hash: &[u8; 32]) -> Vec<u8> {
    let mut v = vec![1u8];
    v.extend(kind.to_le_bytes());
    v.extend(hash);
    v
}

/// Gives the producer on `port` the final commitment `qc` (its network serialization) through
/// `inv`, the `getdata` it answers with, and `qfcommit`, at mocktime `time`; returns once dashd
/// has processed it (a `pong` after it). dashd's verdict on the commitment is read from the block
/// it mines next.
pub fn give_commitment(port: u16, time: i64, qc: &[u8]) {
    let started = Instant::now();
    let mut peer = Peer::connect(port, time);
    let hash = sha256d(qc);
    peer.send("inv", &inventory(MSG_QUORUM_FINAL_COMMITMENT, &hash));
    loop {
        assert!(started.elapsed() < DEADLINE, "the producer never asked for the commitment");
        let (command, payload) = peer.recv();
        if command == "getdata" && payload == inventory(MSG_QUORUM_FINAL_COMMITMENT, &hash) {
            break;
        }
    }
    peer.send("qfcommit", qc);
    let nonce = 0x0fed_cba9_8765_4321u64.to_le_bytes();
    peer.send("ping", &nonce);
    loop {
        assert!(started.elapsed() < DEADLINE, "no pong after the commitment");
        let (command, payload) = peer.recv();
        if command == "pong" && payload == nonce {
            return;
        }
    }
}
