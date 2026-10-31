//! P4a's dashd transcripts that need the node's halves of the RPCs and files (D5, D6, D10):
//! `inbound-bans` and `inbound-ban-expiry` drive `setban` and `clearbanned` through
//! [`dashd_net::bans`], and
//! `inbound-capture` compares `-capturemessages` files written by [`FileCapture`] with dashd's.
//! The fixtures are `dashd-p2p`'s (`contrib/node/p4a-transcripts/capture.py`; provenance in
//! `dashd-p2p/tests/fixtures/dashd-v24/PROVENANCE.md`), replayed by
//! [`dashd_p2p::sim::transcript`].

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use dash_p2p::NetConfig;
use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use dashd_net::bans::{SetBan, SetBanError, set_ban};
use dashd_net::capture::FileCapture;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::sim::transcript::{Hooks, hex, masked_hex, unhex};
use dashd_p2p::sim::{SIM_START, Sim};
use dashd_p2p::{Registry, Seams};

/// A node with dashd's defaults where `sim` departs from them (`-peertimeout` is 60 s).
fn sim(cfg_hook: impl FnOnce(&mut NetConfig)) -> Sim {
    let p =
        Arc::new(ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    Sim::with(p, id, ProcessorOptions::default(), Registry::new(), Seams::default(), |cfg| {
        cfg.peer_timeout = 60;
        cfg_hook(cfg);
    })
}

fn fixture(name: &str) -> String {
    let path =
        format!("{}/../dashd-p2p/tests/fixtures/dashd-v24/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// `setban` and `clearbanned` as the RPCs run them; `capture` records collected for the end.
#[derive(Default)]
struct NodeHooks {
    /// dashd's capture records per peer and direction (`true`: `msgs_recv.dat`), as compared.
    capture: BTreeMap<(u8, bool), Vec<(String, String)>>,
}

impl Hooks for NodeHooks {
    fn input(&mut self, sim: &mut Sim, f: &[&str]) -> bool {
        match f[0] {
            "setban" => {
                let command = match f[2] {
                    "add" => SetBan::Add {
                        bantime: f[3].parse().unwrap(),
                        absolute: f[4] == "1",
                    },
                    "remove" => SetBan::Remove,
                    other => panic!("setban {other}"),
                };
                // The tool's calls all succeed in dashd.
                set_ban(&sim.core, f[1], command).unwrap();
                true
            }
            "clearbanned" => {
                sim.core.clear_banned();
                true
            }
            _ => false,
        }
    }

    fn check(&mut self, _sim: &Sim, f: &[&str]) -> bool {
        if f[0] != "capture" {
            return false;
        }
        let n: u8 = f[1].parse().unwrap();
        let record = Record {
            time_us: f[3].parse().unwrap(),
            cmd: f[4].to_owned(),
            payload: unhex(f.get(5).copied().unwrap_or("")),
        };
        // Every file dashd wrote is listed, even one whose records are all the tool's.
        let records = self.capture.entry((n, f[2] == "recv")).or_default();
        records.extend(record.compared(n, f[2] == "recv"));
        true
    }
}

/// One capture record.
struct Record {
    time_us: i64,
    cmd: String,
    payload: Vec<u8>,
}

impl Record {
    /// What is compared of a record to or from peer `n`: the capture tool's sync pings to dashd
    /// (nonce `0x7EA5000000000000 + n`) and dashd's pongs to them are the tool's, absent from the
    /// replay; a `pong` the peer sent echoes the node's random nonce; the rest is masked as the
    /// transcript's messages are. The time is part of it.
    fn compared(&self, n: u8, incoming: bool) -> Option<(String, String)> {
        let sync = (0x7EA5_0000_0000_0000u64 + u64::from(n)).to_le_bytes();
        let ours = if incoming {
            "ping"
        } else {
            "pong"
        };
        if self.cmd == ours && self.payload == sync {
            return None;
        }
        // The peer's own messages are the fixture's bytes, but for the echoed nonce.
        let payload = if !incoming {
            masked_hex(&self.cmd, &self.payload)
        } else if self.cmd == "pong" {
            masked_hex("ping", &self.payload)
        } else {
            hex(&self.payload)
        };
        Some((format!("{} {}", self.time_us, self.cmd), payload))
    }
}

/// Reads one capture file as Core's `p2p_message_capture.py` does.
fn read_capture(path: &Path) -> Vec<Record> {
    let bytes = fs::read(path).unwrap();
    let mut rest = &bytes[..];
    let mut out = Vec::new();
    while !rest.is_empty() {
        let time_us = i64::from_le_bytes(rest[..8].try_into().unwrap());
        let cmd = std::str::from_utf8(&rest[8..20]).unwrap().trim_end_matches('\0').to_owned();
        let size = u32::from_le_bytes(rest[20..24].try_into().unwrap()) as usize;
        out.push(Record {
            time_us,
            cmd,
            payload: rest[24..24 + size].to_vec(),
        });
        rest = &rest[24 + size..];
    }
    out
}

/// Five inbound slots short of full and a table of bans: `setban` on an address (both of its
/// peers disconnected), a subnet, a relative ban that expires, an absolute one; `setban remove`
/// and `clearbanned`; a discouraged address refused only when the table is nearly full. Every
/// `banlist` line is dashd's `banlist.json`, byte for byte (P4a D5, D6).
#[test]
fn inbound_bans() {
    let sim = sim(|cfg| cfg.max_connections = 15);
    dashd_p2p::sim::transcript::replay(
        "inbound-bans",
        &fixture("inbound-bans"),
        sim,
        &mut NodeHooks::default(),
    );
}

/// The ban-expiry boundary: at `now == nBanUntil` the address connects (`IsBanned` holds the
/// ban while `now < nBanUntil`) but the next save still lists the entry (`SweepBanned` drops it
/// only when `now > nBanUntil`); a second later the save sweeps it (BL-214, P4a D25).
#[test]
fn inbound_ban_expiry() {
    let sim = sim(|_| {});
    dashd_p2p::sim::transcript::replay(
        "inbound-ban-expiry",
        &fixture("inbound-ban-expiry"),
        sim,
        &mut NodeHooks::default(),
    );
}

/// `-capturemessages`: each peer's `msgs_recv.dat` and `msgs_sent.dat`, record by record, as
/// dashd wrote them (P4a D10).
#[test]
fn inbound_capture() {
    let dir = std::env::temp_dir().join(format!("p4a-inbound-capture-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let capture = FileCapture::new(&dir);
    let root = dir.join("message_capture");
    let sim = sim(|cfg| cfg.capture = Some(Arc::new(capture)));
    let mut hooks = NodeHooks::default();
    dashd_p2p::sim::transcript::replay(
        "inbound-capture",
        &fixture("inbound-capture"),
        sim,
        &mut hooks,
    );
    assert!(!hooks.capture.is_empty(), "the fixture holds capture records");
    // Each peer's directory is Core's `ToStringAddrPort` with `:` as `_` (net.cpp:5034-5036):
    // the address of its `accept N from ADDR` record and local port 40000 + N.
    let text = fixture("inbound-capture");
    let dir_of = |n: u8| {
        let addr = text
            .lines()
            .find_map(|l| l.strip_prefix(&format!("accept {n} from ")))
            .unwrap_or_else(|| panic!("no `accept {n} from` record"));
        format!("{addr}_{}", 40000 + u16::from(n))
    };
    let file_of = |incoming| {
        if incoming {
            "msgs_recv.dat"
        } else {
            "msgs_sent.dat"
        }
    };
    // The node wrote exactly the files dashd did.
    let mut written: Vec<String> = Vec::new();
    for peer in fs::read_dir(&root).unwrap() {
        let peer = peer.unwrap().path();
        for file in fs::read_dir(&peer).unwrap() {
            let file = file.unwrap().path();
            written.push(format!(
                "{}/{}",
                peer.file_name().unwrap().to_str().unwrap(),
                file.file_name().unwrap().to_str().unwrap()
            ));
        }
    }
    written.sort();
    let mut expected: Vec<String> = hooks
        .capture
        .keys()
        .map(|&(n, incoming)| format!("{}/{}", dir_of(n), file_of(incoming)))
        .collect();
    expected.sort();
    assert_eq!(written, expected, "capture files");
    for (&(n, incoming), want) in &hooks.capture {
        let file = root.join(dir_of(n)).join(file_of(incoming));
        let got: Vec<_> =
            read_capture(&file).iter().filter_map(|r| r.compared(n, incoming)).collect();
        assert_eq!(&got, want, "{}", file.display());
    }
    fs::remove_dir_all(&dir).unwrap();
}

fn already_banned() -> Result<(), SetBanError> {
    Err(SetBanError::AlreadyBanned("Error: IP/Subnet already banned"))
}

/// `setban`'s errors and `listbanned`'s rows, Core's texts and fields (`rpc/net.cpp`; P4a D6).
#[test]
fn setban_and_listbanned_are_cores() {
    use dashd_net::bans::banned_rows;
    let sim = sim(|_| {});
    let core = &sim.core;
    let add = |bantime, absolute| SetBan::Add {
        bantime,
        absolute,
    };
    let invalid = Err(SetBanError::InvalidIpOrSubnet("Error: Invalid IP/Subnet"));
    assert_eq!(set_ban(core, "not-an-ip", add(0, false)), invalid);
    assert_eq!(set_ban(core, "1.2.3.4/33", add(0, false)), invalid);
    // A host name never resolves (`LookupHost(.., false)`).
    assert_eq!(set_ban(core, "localhost", add(0, false)), invalid);

    // `LookupHost(.., false)` is `getaddrinfo(AI_NUMERICHOST)`: glibc's `inet_aton` forms
    // resolve, other forms do not (Sol P4a r1 P2; D20).
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        assert_eq!(set_ban(core, "11.1", add(0, false)), Ok(()));
        assert_eq!(banned_rows(core)[0].address, "11.0.0.1/32");
        assert_eq!(set_ban(core, "11.0.0.1", add(0, false)), already_banned());
        assert_eq!(set_ban(core, "1.2.3.4.", add(0, false)), invalid);
        assert_eq!(set_ban(core, "11.1", SetBan::Remove), Ok(()));
        // listbanned prints an IPv6 scope id as Core's `%N`; the ban ignores it (D23).
        assert_eq!(set_ban(core, "fe80::1%7", add(0, false)), Ok(()));
        assert_eq!(banned_rows(core)[0].address, "fe80::1%7/128");
        assert_eq!(set_ban(core, "fe80::1", add(0, false)), already_banned());
        assert_eq!(set_ban(core, "fe80::1", SetBan::Remove), Ok(()));
    }

    assert_eq!(set_ban(core, "1.2.3.0/24", add(100, false)), Ok(()));
    let already = already_banned();
    // An address under a banned subnet is banned; another subnet over it is not.
    assert_eq!(set_ban(core, "1.2.3.4", add(0, false)), already);
    assert_eq!(set_ban(core, "1.2.3.0/255.255.255.0", add(0, false)), already);
    assert_eq!(set_ban(core, "1.2.0.0/16", add(0, false)), Ok(()));
    assert_eq!(
        set_ban(core, "5.6.7.8", add(SIM_START - 1, true)),
        Err(SetBanError::InvalidParameter("Error: Absolute timestamp is in the past"))
    );
    assert_eq!(set_ban(core, "5.6.7.8", add(SIM_START + 50, true)), Ok(()));

    let rows = banned_rows(core);
    let row = |a: &str| rows.iter().find(|r| r.address == a).unwrap();
    assert_eq!(rows.len(), 3);
    let r = row("1.2.3.0/24");
    assert_eq!(
        (r.ban_created, r.banned_until, r.ban_duration, r.time_remaining),
        (SIM_START, SIM_START + 100, 100, 100)
    );
    let r = row("5.6.7.8/32");
    assert_eq!((r.ban_duration, r.time_remaining), (50, 50));
    // 0: `-bantime`, a day.
    assert_eq!(row("1.2.0.0/16").ban_duration, 24 * 60 * 60);

    assert_eq!(set_ban(core, "5.6.7.8", SetBan::Remove), Ok(()));
    assert_eq!(
        set_ban(core, "1.2.3.4", SetBan::Remove),
        Err(SetBanError::InvalidIpOrSubnet(
            "Error: Unban failed. Requested address/subnet was not previously manually banned."
        ))
    );
    assert_eq!(banned_rows(core).len(), 2);
}
