//! `p2p_gate_model` (SB6): `dash-p2p`'s pre-handshake gates (`gates::admit`, P2a §3.2.1)
//! against a model written from Core's `ProcessMessage` prologue (v24
//! `net_processing.cpp:3935-3939`, `:4178-4386`) and `NetMessageViolatesBlocksOnly`
//! (`protocol.cpp:185-212`). The input is a script over one peer: messages (a command, the
//! time, `RejectIncomingTxs`), the peer's `version` and `verack` being processed, and the
//! masternode-probe flag. After every message the verdict and the peer's gate state
//! (`nTimeFirstMessageReceived`, `fFirstMessageIsMNAUTH`, `fDisconnect`) must agree.

use std::cell::Cell;

use dash_p2p::gates::{self, Gate, GatePeer};
use dash_p2p::policy::ALL_NET_MESSAGE_TYPES;
use dash_p2p::{CommandString, PeerId};
use node_l1::Divergence;

/// Script steps per input.
const MAX_STEPS: usize = 256;

/// The branches Core runs after the `nVersion` check and before the `fSuccessfullyConnected`
/// check; each returns (`net_processing.cpp:4187-4361`).
const BEFORE_VERACK: &[&str] =
    &["verack", "sendheaders", "sendheaders2", "sendcmpct", "sendaddrv2", "sendtxrcncl"];

/// `netMessageTypesViolateBlocksOnly` (`protocol.cpp:185-212`), Core's wire strings.
const BLOCKS_ONLY: &[&str] = &[
    "dsa",
    "dsc",
    "dsf",
    "dsq",
    "dss",
    "dssu",
    "dstx",
    "dsi",
    "getqrinfo",
    "platformban",
    "qbsigs",
    "qcomplaint",
    "qcontrib",
    "qdata",
    "qgetdata",
    "qgetsigs",
    "qjustify",
    "qpcommit",
    "qsendrecsigs",
    "qsigrec",
    "qsigsesann",
    "qsigshare",
    "qsigsinv",
    "qrinfo",
    "qwatch",
    "tx",
];

fn diverged(field: &'static str) -> Divergence {
    Divergence {
        field,
    }
}

/// The input as a stream of numbers; zeros once it runs out.
struct Input<'a>(&'a [u8]);

impl Input<'_> {
    fn u8(&mut self) -> u8 {
        let (&b, rest) = self.0.split_first().unwrap_or((&0, &[]));
        self.0 = rest;
        b
    }
}

/// The gate state of one peer, shared by both sides through [`GatePeer`].
#[derive(Clone, Default, PartialEq, Eq)]
struct State {
    version: i32,
    connected: bool,
    first_time: i64,
    first_mnauth: bool,
    probe: bool,
    disconnect: bool,
}

/// The state `gates::admit` reads and writes.
#[derive(Default)]
struct Peer(Cell<State>);

impl Peer {
    fn update(&self, f: impl FnOnce(&mut State)) {
        let mut s = self.0.take();
        f(&mut s);
        self.0.set(s);
    }
    fn get(&self) -> State {
        let s = self.0.take();
        self.0.set(s.clone());
        s
    }
}

impl GatePeer for Peer {
    fn id(&self) -> PeerId {
        0
    }
    fn version(&self) -> i32 {
        self.get().version
    }
    fn successfully_connected(&self) -> bool {
        self.get().connected
    }
    fn first_message_time(&self) -> i64 {
        self.get().first_time
    }
    fn set_first_message(&self, time: i64, is_mnauth: bool) {
        self.update(|s| {
            s.first_time = time;
            s.first_mnauth = is_mnauth;
        });
    }
    fn masternode_probe(&self) -> bool {
        self.get().probe
    }
    fn mark_disconnect(&self) {
        self.update(|s| s.disconnect = true);
    }
}

/// Core's prologue, in Core's order.
fn model(s: &mut State, cmd: &str, now: i64, reject_incoming_txs: bool) -> Gate {
    if cmd == "version" {
        return if s.version != 0 {
            Gate::RedundantVersion
        } else {
            Gate::Admit
        };
    }
    if s.version == 0 {
        return Gate::BeforeVersion;
    }
    if BEFORE_VERACK.contains(&cmd) {
        return Gate::Admit;
    }
    if !s.connected {
        return Gate::BeforeVerack;
    }
    if s.first_time == 0 {
        s.first_time = now;
        s.first_mnauth = cmd == "mnauth";
        if s.probe && !s.first_mnauth {
            s.disconnect = true;
            return Gate::ProbeWithoutMnauth;
        }
    }
    if reject_incoming_txs && BLOCKS_ONLY.contains(&cmd) {
        s.disconnect = true;
        return Gate::BlocksOnlyViolation;
    }
    Gate::Admit
}

/// A command: one of Core's 74 most of the time, else a valid one Core does not know (the
/// transports drop invalid command bytes before the gates; the empty type is V2's reserved
/// short ID).
fn command(input: &mut Input<'_>) -> CommandString {
    let pick = usize::from(input.u8());
    match ALL_NET_MESSAGE_TYPES.get(pick % 80) {
        Some(cmd) => *cmd,
        None => CommandString::from_static(
            ["", "barfoo", "VERSION", "mnauth2", "txx", "qwatch2"][pick % 80 - 74],
        ),
    }
}

pub(super) fn check(data: &[u8]) -> Result<(), Divergence> {
    let mut input = Input(data);
    let peer = Peer::default();
    let mut state = State::default();
    for _ in 0..MAX_STEPS {
        if input.0.is_empty() {
            break;
        }
        match input.u8() % 8 {
            // The processor handled `version` (only the first one reaches its branch).
            5 if state.version == 0 => {
                let v = 70_000 + i32::from(input.u8());
                state.version = v;
                peer.update(|s| s.version = v);
            }
            // The processor handled `verack` after `version`.
            6 if state.version != 0 => {
                state.connected = true;
                peer.update(|s| s.connected = true);
            }
            7 => {
                let probe = input.u8() & 1 == 1;
                state.probe = probe;
                peer.update(|s| s.probe = probe);
            }
            _ => {
                let cmd = command(&mut input);
                super::note_command(&cmd);
                let flags = input.u8();
                // Mostly a valid `GetTime()`, sometimes 0 (a mock time of 0 leaves the stamp unset).
                let now = if flags & 0x0e == 0 {
                    0
                } else {
                    1_700_000_000 + i64::from(flags)
                };
                let reject = flags & 1 == 1;
                let got = gates::admit(&peer, &cmd, now, reject);
                let want = model(&mut state, cmd.as_str(), now, reject);
                if got != want {
                    return Err(diverged("verdict"));
                }
                if got.disconnects() && !peer.get().disconnect {
                    return Err(diverged("disconnects() without fDisconnect"));
                }
                if peer.get() != state {
                    return Err(diverged("peer state"));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::p2p_decode_matrix::Rng;
    use super::*;

    /// Every command (Core's 74 and six it does not know) as the first message after `verack`,
    /// on a plain peer and a masternode probe, with and without `RejectIncomingTxs` and a zero
    /// `GetTime()`, then a second message; then seeded random scripts.
    #[test]
    fn scripted_and_generated_inputs_pass() {
        let mut failures = Vec::new();
        for pick in 0..80u8 {
            for probe in 0..2u8 {
                for flags in 0..4u8 {
                    // The probe flag, `version`, `verack`, the first message and a second one.
                    let input = [7, probe, 5, 0x10, 6, 0, pick, flags, 0, pick, flags ^ 1];
                    if let Err(d) = check(&input) {
                        failures.push(format!("{pick} probe {probe} flags {flags}: {}", d.field));
                    }
                }
            }
        }
        let mut rng = Rng(0x6a7e);
        for i in 0..20_000 {
            let len = (rng.next() % 64) as usize;
            let input: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            if let Err(d) = check(&input) {
                failures.push(format!("random {i}: {}", d.field));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
