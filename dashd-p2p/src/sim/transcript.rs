//! The dashd transcript replay (P2a §6.1): a fixture is what scripted V1 peers sent a fresh
//! regtest dashd and what dashd sent back, segment by segment
//! (`contrib/node/p2a-transcripts/capture.py`, whose docstring defines the format). [`replay`]
//! plays the peers' side against the node in [`Sim`] and requires the node's messages to each
//! peer in each segment to equal dashd's, after the masks below.
//!
//! Masked, because they differ by design or by chance: `version`'s nonce, user agent (DEC-83) and
//! MNAUTH challenge, and `ping` nonces (`addr_recv`'s address is all zeros on both sides: neither
//! 127.0.0.1 nor 10.0.0.n is routable). Dropped from dashd's side only, as later tasks' sends
//! (P2a §1.4, §6.1): block-only `getdata` (P3) unless the caller's node downloads blocks
//! ([`Hooks::downloads_blocks`], P3's fixtures), the sync tick's `getsporks`, `mempool` and
//! `govsync` (SP1), and whatever the caller's [`Hooks::later`] names. The node must send none of
//! them. Nothing else is masked or dropped.
//!
//! P4a's records (`contrib/node/p4a-transcripts/capture.py`): `accept N from ADDR [perm FLAGS]`
//! (a peer from a public address, through a `-whitebind` listener with FLAGS), `masternode N
//! HASH` (an inbound masternode that passed MNAUTH; the node's sends to it are not compared, but
//! its closing is), and the `banlist HEX` check against the core's last saved `banlist.json`. A
//! caller's [`Hooks::check`] takes the checks the format does not define.
//!
//! O13a's records (`contrib/node/fingerprint/capture.py`): `connect N TYPE SERVICES v2` and
//! `accept N v2` (the peer speaks V2: [`Sim::connect_v2`], [`Sim::accept_v2`]), a fifth field
//! `short` or `long` on a `<` record of a V2 connection (the type's encoding, compared too),
//! `accept N refused` (dashd had no listener; a node with none refuses it too) and `> N raw HEX`
//! (bytes written as they are, [`Sim::deliver_raw`]). [`record`] returns the node's transcript
//! instead of comparing, for O13a's diff.
//!
//! P3's records (`contrib/node/p3-transcripts/capture.py`): `close N` (the tool closed its socket
//! to peer N, and dashd finalized the peer before the next record), `> N CMD gen:SPEC sha256:HEX`
//! (a payload too large to keep, which the caller's [`Hooks::generate`] rebuilds from SPEC and
//! the replay checks against HEX), `< N CMD sha256:HEX:LEN` (dashd's payload of LEN bytes
//! by its digest, compared with the digest of the node's), and `submitblock HEX` (the RPC: the
//! block goes through V from no peer, then `UpdatedBlockTip` for what it connected). P3 also
//! masks a `cmpctblock`'s nonce and short IDs, which come from a fresh `GetRand` draw on each
//! side (be:22).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use dash_clock::Clock;
use dash_consensus::primitives::read_compact_size;
use dash_consensus::{BlockId, decode_block};
use dash_p2p::conn::Permissions;
use dash_p2p::lookup::lookup_host;
use dash_p2p::permissions::parse_whitebind;
use dash_p2p::{CommandString, ConnectionType, NetAddr, NetBlob, ServiceFlags};
use dash_primitives::BlockHeader;
use dash_types::codec::BaseCodec;

use super::{SIM_START, Sent, Sim, SimPeer, TypeForm, until};
use crate::processing::seams::{BlockSubmit, ChainAccess};

/// What a fixture's caller adds to the format: its own input records and later tasks' sends.
pub trait Hooks {
    /// Applies input record `f` the format does not define; false if it is unknown.
    fn input(&mut self, _sim: &mut Sim, _f: &[&str]) -> bool {
        false
    }

    /// Whether dashd's `cmd` with `payload` is a later task's send, dropped from dashd's side.
    fn later(&self, _cmd: &str, _payload: &[u8]) -> bool {
        false
    }

    /// Whether the node runs a block download (P3's `BlockDownload`), so dashd's block-only
    /// `getdata` is compared rather than dropped as a later task's send.
    fn downloads_blocks(&self) -> bool {
        false
    }

    /// The payload of generated input `> N cmd gen:spec`, or `None` if `spec` is not the caller's.
    fn generate(&mut self, _cmd: &str, _spec: &str) -> Option<Vec<u8>> {
        None
    }

    /// Applies check record `f` (not an input: it opens no segment) once the segment before it
    /// has settled; false if it is not one of the caller's checks.
    fn check(&mut self, _sim: &Sim, _f: &[&str]) -> bool {
        false
    }

    /// Whether the caller runs SP1's sync tick ([`Hooks::settled`]), so dashd's `mempool` and
    /// `govsync` are compared rather than dropped as a later task's sends.
    fn runs_sync_tick(&self) -> bool {
        false
    }

    /// Runs once the segment's input has settled, before the segment is compared: dashd's
    /// scheduler (the sync tick, every second of real time) runs within the tool's quiet wait.
    fn settled(&mut self, _sim: &mut Sim) {}
}

/// No records or drops beyond the format's.
#[derive(Debug, Default)]
pub struct NoHooks;

impl Hooks for NoHooks {}

/// Encodes hex.
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decodes hex.
pub fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex");
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

fn header(hex: &str) -> BlockHeader {
    BlockHeader::decode(&mut &unhex(hex)[..]).expect("a block header")
}

/// An IPv4 or IPv6 literal.
fn addr(s: &str) -> NetAddr {
    lookup_host(s).unwrap_or_else(|| panic!("an address: {s}"))
}

/// A `-whitebind` listener's flags, Core's names joined by `,`.
fn permissions(flags: &str) -> Permissions {
    parse_whitebind(&format!("{flags}@127.0.0.1:1")).expect("whitebind flags").1
}

fn conn_type(s: &str) -> ConnectionType {
    match s {
        "outbound-full-relay" => ConnectionType::OutboundFullRelay,
        "block-relay-only" => ConnectionType::BlockRelay,
        "addr-fetch" => ConnectionType::AddrFetch,
        "feeler" => ConnectionType::Feeler,
        "manual" => ConnectionType::Manual,
        other => panic!("unknown connection type {other}"),
    }
}

/// One message a node sent, or the connection closing.
#[derive(Debug, PartialEq, Eq)]
enum Out {
    /// The type, the payload in hex after the masks, and the type's encoding on a V2
    /// connection (O13a's fifth field).
    Msg(String, String, Option<TypeForm>),
    /// A message by its payload's `sha256:HEX:LEN` (P3's records).
    Digest(String, String),
    Closed,
}

/// `payload`'s sha256 in hex.
fn sha256_hex(payload: &[u8]) -> String {
    hex(&dash_consensus::hash::sha256(payload))
}

/// `payload`'s `HEX:LEN` for a `sha256:` record.
fn digest(payload: &[u8]) -> String {
    format!("{}:{}", sha256_hex(payload), payload.len())
}

fn masked(cmd: &str, payload: &[u8], form: Option<TypeForm>) -> Out {
    Out::Msg(cmd.to_owned(), masked_hex(cmd, payload), form)
}

/// A `<` record's type form: `short`, `long`, or none (a V1 connection).
fn type_form(field: &str) -> Option<TypeForm> {
    match field {
        "" => None,
        "short" => Some(TypeForm::Short),
        "long" => Some(TypeForm::Long),
        other => panic!("unknown type form {other}"),
    }
}

/// The `<` record of message `m` to peer `n`, its payload unmasked.
fn sent_record(n: u8, m: &Sent) -> String {
    let line = format!("< {n} {} {}", m.cmd, hex(&m.payload));
    match m.form {
        Some(form) => format!("{line} {}", form.as_str()),
        None => line,
    }
}

/// A message's payload in hex after the masks (module docs).
pub fn masked_hex(cmd: &str, payload: &[u8]) -> String {
    let mut p = payload.to_vec();
    match cmd {
        // Core: net_processing.cpp:1561-1593 PushNodeVersion
        "version" => {
            p[72..80].fill(0); // nonce
            let ua = usize::from(p[80]);
            assert!(ua < 0xfd, "one-byte user agent length");
            let tail = p.split_off(81 + ua);
            p.truncate(80);
            p.extend_from_slice(b"<user agent>");
            p.extend_from_slice(&tail[..5]); // start height, relay
            p.extend_from_slice(&[0; 32]); // MNAUTH challenge
            p.extend_from_slice(&tail[37..]);
        }
        "ping" => p.fill(0),
        // Core: blockencodings.cpp:21-35 (the nonce, and the short IDs keyed by it)
        "cmpctblock" => {
            p[80..88].fill(0);
            let mut rest = &p[88..];
            let n = read_compact_size(&mut rest, false).expect("a short-ID count");
            let start = p.len() - rest.len();
            let n = usize::try_from(n).expect("a short-ID count");
            p[start..start + 6 * n].fill(0);
        }
        _ => {}
    }
    hex(&p)
}

/// Whether an `inv` or `getdata` lists only items of type `inv_type`.
pub fn only_inv_type(payload: &[u8], inv_type: u32) -> bool {
    let n = usize::from(payload[0]);
    assert!(n < 0xfd && payload.len() == 1 + 36 * n, "a short inventory list");
    payload[1..].chunks(36).all(|inv| inv[..4] == inv_type.to_le_bytes())
}

/// Whether dashd's message is a later task's send (module docs).
fn later(hooks: &dyn Hooks, cmd: &str, payload: &[u8]) -> bool {
    match cmd {
        // dashd's block download, which P3 owns (`FindNextBlocksToDownload` behind `BlockSync`;
        // M2's `NoBlockSync` requests nothing): `MSG_BLOCK`, 2.
        "getdata" if !hooks.downloads_blocks() && only_inv_type(payload, 2) => true,
        // Core: node/sync_manager.cpp:157, 206-244, 279 SyncManager::ProcessTick (SP1; P2a §6.1:
        // the tick's 6 s gate opens once the mock time moves). rc.3's tick also sent
        // `getsporks`; rc.4's does not (SP1 D7).
        "getsporks" | "mempool" | "govsync" => !hooks.runs_sync_tick(),
        _ => hooks.later(cmd, payload),
    }
}

/// One scripted peer: its connection (`None` when the node refused it), and its part of the
/// segment being compared.
struct Peer {
    sim: Option<SimPeer>,
    closed: bool,
    /// The fixture does not hold the node's messages to it (`masternode`): only its closing is
    /// compared.
    unseen: bool,
    last_ping: Vec<u8>,
    /// dashd's messages to it in the segment.
    want: Vec<Out>,
}

impl Peer {
    /// Whether the node is done with the connection: it refused it, or marked it to disconnect.
    fn disconnect_requested(&self) -> bool {
        self.sim.as_ref().is_none_or(|s| s.handle.disconnect_requested())
    }
}

struct Replay<'h> {
    sim: Sim,
    hooks: &'h mut dyn Hooks,
    peers: BTreeMap<u8, Peer>,
    /// `fixture:line` and the input that opened the current segment.
    segment: Option<String>,
    /// [`record`]'s output: the fixture with the node's messages in place of dashd's. `None`
    /// when replaying ([`replay`]), which compares each segment instead.
    recording: Option<Vec<String>>,
}

impl Replay<'_> {
    /// Runs passes until the loop reports no more work, then compares the segment.
    fn finish_segment(&mut self) {
        let Some(label) = self.segment.take() else {
            return;
        };
        let settled = (0..50).any(|_| !self.sim.pass());
        assert!(settled, "{label}: the loop still has work after 50 passes");
        self.hooks.settled(&mut self.sim);
        let settled = (0..50).any(|_| !self.sim.pass());
        assert!(settled, "{label}: the loop still has work after the scheduler");
        // dashd's socket thread deletes a disconnected peer (`DisconnectNodes`, `FinalizeNode`)
        // long before the capture tool's next input, freeing what it held (its sync slot);
        // what that frees is sent in this segment.
        if self.peers.values().any(|p| !p.closed && p.disconnect_requested()) {
            self.sim.remove_disconnected();
            let settled = (0..50).any(|_| !self.sim.pass());
            assert!(settled, "{label}: the loop still has work after a removal");
        }
        // dashd's handler thread wakes within 100 ms whether or not there is work, so its
        // `SendMessages` runs once more before the next input.
        self.sim.pass();
        for (n, p) in &mut self.peers {
            let mut got = Vec::new();
            let sent = p.sim.as_ref().map(|s| self.sim.sent_messages(s, 0)).unwrap_or_default();
            for m in sent {
                if m.cmd == CommandString::PING {
                    p.last_ping.clone_from(&m.payload);
                }
                if p.unseen {
                    continue;
                }
                if let Some(rec) = &mut self.recording {
                    rec.push(sent_record(*n, &m));
                    continue;
                }
                // Compared by digest where dashd's message at this place was recorded so.
                got.push(match p.want.get(got.len()) {
                    Some(Out::Digest(..)) => Out::Digest(m.cmd.to_string(), digest(&m.payload)),
                    _ => masked(m.cmd.as_str(), &m.payload, m.form),
                });
            }
            if p.disconnect_requested() && !p.closed {
                p.closed = true;
                got.push(Out::Closed);
                if let Some(rec) = &mut self.recording {
                    rec.push(format!("< {n} closed"));
                }
            }
            if self.recording.is_none() {
                assert_eq!(got, std::mem::take(&mut p.want), "{label}: peer {n}");
            }
        }
    }

    /// Opens a segment with input record `f`.
    fn input(&mut self, f: &[&str]) {
        let n = || f[1].parse::<u8>().unwrap();
        match f[0] {
            "connect" | "accept" | "masternode" => {
                // An inbound peer goes through `CreateNodeFromAcceptedSocket` (P2b D8).
                let sim = match (f[0], f[2]) {
                    ("connect", _) if f.get(4) == Some(&"v2") => Some(self.sim.connect_v2(
                        n(),
                        conn_type(f[2]),
                        ServiceFlags(f[3].parse().unwrap()),
                    )),
                    ("connect", _) => Some(self.sim.connect_with_services(
                        n(),
                        conn_type(f[2]),
                        Permissions::NONE,
                        ServiceFlags(f[3].parse().unwrap()),
                    )),
                    ("accept", "v2") => self.sim.accept_v2(n(), Permissions::NONE),
                    // dashd had no listener (`-listen=0`), so the connection was refused. A node
                    // with no listener has nothing to accept with; one that listens takes the
                    // peer, and [`record`] shows the input as a plain `accept`.
                    ("accept", "refused") if !self.sim.core.config().listen => None,
                    ("accept", "refused") => {
                        if let Some(rec) = &mut self.recording {
                            *rec.last_mut().expect("the input") = format!("accept {}", f[1]);
                        } else {
                            panic!("accept {}: dashd refused it, the node listens", f[1]);
                        }
                        self.sim.accept(n(), Permissions::NONE)
                    }
                    ("accept", "from") => {
                        let perm = match &f[4..] {
                            [] => Permissions::NONE,
                            ["perm", flags] => permissions(flags),
                            rest => panic!("accept {}: unknown suffix {rest:?}", f[1]),
                        };
                        self.sim.accept_from(n(), addr(f[3]), perm)
                    }
                    ("accept", rest) => {
                        assert!(rest.is_empty() && f.len() == 4, "accept {}: unknown suffix", f[1]);
                        self.sim.accept(n(), Permissions::NONE)
                    }
                    // `masternode`: the leg the second dashd dialled, from 127.0.0.1.
                    _ => self.sim.accept(n(), Permissions::NONE),
                };
                let masternode = f[0] == "masternode";
                if masternode && let Some(s) = &sim {
                    // MNAUTH's verification, before the next accept counts the inbound table
                    // (Core: evo/mnauth.cpp:157 `TryMarkVerified`; the hash in RPC order).
                    let mut hash: [u8; 32] = unhex(f[2]).try_into().expect("a 32-byte hash");
                    hash.reverse();
                    s.handle.set_verified_proreg_hash(hash);
                }
                let p = Peer {
                    closed: sim.is_none() && f[2] == "refused",
                    sim,
                    unseen: masternode,
                    last_ping: Vec::new(),
                    want: Vec::new(),
                };
                self.peers.insert(n(), p);
            }
            ">" => {
                let p = &self.peers[&n()];
                let payload = match (f[2], f[3]) {
                    ("pong", "echo") => p.last_ping.clone(),
                    (cmd, g) if let Some(spec) = g.strip_prefix("gen:") => {
                        let payload = self.hooks.generate(cmd, spec).expect("a generated payload");
                        let want = f[4].strip_prefix("sha256:").expect("the payload's sha256");
                        assert_eq!(sha256_hex(&payload), want, "`gen:{spec}`");
                        payload
                    }
                    ("raw", hex) => {
                        if !p.disconnect_requested()
                            && let Some(s) = &p.sim
                        {
                            self.sim.deliver_raw(s, &unhex(hex));
                        }
                        return;
                    }
                    (_, hex) => unhex(hex),
                };
                if !p.disconnect_requested()
                    && let Some(s) = &p.sim
                {
                    self.sim.deliver(s, &[(f[2], payload)]);
                }
            }
            "close" => {
                // dashd reads the end of the stream (`fOtherSideDisconnected`, so no linger),
                // and its socket thread finalizes the peer before the tool's next record (the
                // tool waits for that).
                let p = self.peers.get_mut(&n()).expect("a known peer");
                if let Some(s) = &p.sim
                    && !p.closed
                {
                    s.sock.close();
                    until(|| s.handle.disconnect_requested());
                }
                p.closed = true;
                self.sim.remove_disconnected();
            }
            "time" => {
                // Steady time past SB6's 1 s gate, so its sweep runs `InactivityCheck` at its next
                // wake (≤ 500 ms of real time, not before the comparison; D37).
                self.sim.clock.advance(Duration::from_secs(2));
                self.sim.clock.set_mock_time(f[1].parse().unwrap());
            }
            "submitblock" => {
                let raw = unhex(f[1]);
                let block = Arc::new(decode_block(&mut &raw[..]).expect("a block").into_value());
                let prev = self.sim.chain.snapshot().tip.tip;
                self.sim.chain.accept_block(&BlockSubmit {
                    hash: block.hash(),
                    block,
                    force: true,
                    from: -1,
                    merkle_checked: false,
                });
                let tip = self.sim.chain.snapshot().tip.tip;
                let new_blocks = self.sim.chain.new_blocks(tip, Some(prev));
                self.sim.shared.updated_block_tip(&self.sim.core, &new_blocks, false);
            }
            "mine" => {
                let prev = self.sim.chain.snapshot().tip.tip;
                let tip = extend(&self.sim, f[1]);
                self.sim.shared.block_connected(self.sim.clock.node_now().0);
                let new_blocks = self.sim.chain.new_blocks(tip, Some(prev));
                self.sim.shared.updated_block_tip(&self.sim.core, &new_blocks, false);
            }
            other => assert!(self.hooks.input(&mut self.sim, f), "unknown record {other}"),
        }
    }

    /// Applies `f` if it is a check record, after the segment before it settled.
    fn check(&mut self, label: &str, f: &[&str]) -> bool {
        if self.recording.is_some() && f[0] == "banlist" {
            return true;
        }
        match f[0] {
            // Core saves the list on every change (`BanMan::DumpBanlist`); dashd wrote it at
            // its start.
            "banlist" => {
                let want = (f[1] != "-").then(|| unhex(f[1]));
                let got = self.sim.store.saved(NetBlob::Bans);
                let text = |b: &Option<Vec<u8>>| {
                    b.as_deref().map(|b| String::from_utf8_lossy(b).into_owned())
                };
                assert!(
                    got == want,
                    "{label}: banlist.json\nnode:  {:?}\ndashd: {:?}",
                    text(&got),
                    text(&want)
                );
                true
            }
            _ => self.hooks.check(&self.sim, f),
        }
    }
}

/// Connects header `hex` as the new tip.
fn extend(sim: &Sim, hex: &str) -> BlockId {
    let tip = sim.chain.add(&[header(hex)]).id;
    sim.chain.set_tip(tip);
    tip
}

/// Replays fixture `text` (named `name` in failures) against the node in `sim`, which starts at
/// [`SIM_START`].
pub fn replay(name: &str, text: &str, sim: Sim, hooks: &mut dyn Hooks) {
    run(name, text, sim, hooks, None);
}

/// Plays fixture `text`'s inputs against the node in `sim` exactly as [`replay`] does, and
/// returns the node's transcript in the same format: every line of the fixture but dashd's
/// `<` records, with the node's messages (unmasked) after each input, peer by peer (O13a D4).
/// Nothing is compared or masked (the node's sends to a `masternode` peer are left out, as the
/// fixture leaves them out), and the `banlist` checks are skipped.
pub fn record(name: &str, text: &str, sim: Sim, hooks: &mut dyn Hooks) -> String {
    let mut out = String::new();
    for line in run(name, text, sim, hooks, Some(Vec::new())).expect("a recording") {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn run(
    name: &str,
    text: &str,
    sim: Sim,
    hooks: &mut dyn Hooks,
    recording: Option<Vec<String>>,
) -> Option<Vec<String>> {
    let mut r = Replay {
        sim,
        hooks,
        peers: BTreeMap::new(),
        segment: None,
        recording,
    };
    for (i, line) in text.lines().enumerate() {
        let mut f: Vec<&str> = line.split(' ').collect();
        // A message record's payload, empty for a payload-less message.
        if f.len() < 4 {
            f.resize(4, "");
        }
        // A recording drops dashd's `<` records and keeps the other lines as they are (an
        // input once the segment before it settled, below).
        if let Some(rec) = &mut r.recording {
            match f[0] {
                "<" => continue,
                "" | "#" | "mocktime" | "chain" | "ibd" => rec.push(line.to_owned()),
                _ => {}
            }
        }
        match f[0] {
            "" | "#" => {}
            "mocktime" => assert_eq!(f[1].parse::<i64>().unwrap(), SIM_START),
            "chain" => {
                extend(&r.sim, f[1]);
            }
            "ibd" => r.sim.chain.set_ibd(f[1] == "1"),
            "<" => {
                let out = match f[2] {
                    "closed" => Out::Closed,
                    cmd if let Some(d) = f[3].strip_prefix("sha256:") => {
                        Out::Digest(cmd.to_owned(), d.to_owned())
                    }
                    cmd if later(&*r.hooks, cmd, &unhex(f[3])) => continue,
                    cmd => masked(cmd, &unhex(f[3]), type_form(f.get(4).copied().unwrap_or(""))),
                };
                let n: u8 = f[1].parse().unwrap();
                // A check opens no segment: what follows it would never be compared.
                assert!(r.segment.is_some(), "{name}:{}: a `<` record outside a segment", i + 1);
                r.peers.get_mut(&n).expect("a known peer").want.push(out);
            }
            _ => {
                r.finish_segment();
                if let Some(rec) = &mut r.recording {
                    rec.push(line.to_owned());
                }
                let input: String = line.chars().take(60).collect();
                let label = format!("{name}:{} `{input}`", i + 1);
                if !r.check(&label, &f) {
                    r.segment = Some(label);
                    r.input(&f);
                }
            }
        }
    }
    r.finish_segment();
    assert_whole_v1_frames(name, r.peers.iter().filter_map(|(n, p)| Some((*n, p.sim.as_ref()?))));
    r.recording
}

/// The recording's end check: every byte the node wrote to each peer was part of a whole V1
/// frame, as a Core peer would read it.
///
/// # Panics
///
/// On a peer left holding an incomplete V1 frame.
pub fn assert_whole_v1_frames<'a>(name: &str, peers: impl IntoIterator<Item = (u8, &'a SimPeer)>) {
    for (n, p) in peers {
        let partial = p.partial_bytes();
        assert_eq!(partial, 0, "{name}: peer {n}: an incomplete V1 frame was left at the end");
    }
}
