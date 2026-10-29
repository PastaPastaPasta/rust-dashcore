//! Ping and pong (P2a §3.5; v24 `MaybeSendPing` `net_processing.cpp:6148-6184`, handlers
//! `:5456-5527`).

use dash_p2p_core::{CommandString, P2pDecodeError, Ping};
use dash_p2p_transport::OutMsg;
use dash_types::codec::BaseCodec;

use crate::conn::{PeerHandle, Why};
use crate::netcore::NetCore;
use crate::policy::{Step, Throw};

/// Core's `PING_INTERVAL`: 2 minutes.
pub const PING_INTERVAL_US: i64 = 2 * 60 * 1_000_000;
/// Core's `TIMEOUT_INTERVAL`: 20 minutes.
pub const TIMEOUT_INTERVAL_US: i64 = 20 * 60 * 1_000_000;

const PING: CommandString = CommandString::from_static("ping");
const PONG: CommandString = CommandString::from_static("pong");

/// A peer's ping state (Core's `Peer::m_ping_*`).
#[derive(Debug, Default)]
pub struct PingState {
    /// `m_ping_nonce_sent`: 0 when no ping is outstanding.
    pub nonce_sent: u64,
    /// `m_ping_start` in microseconds.
    pub start_us: i64,
    /// `m_ping_queued`: the RPC `ping` asked for one.
    pub queued: bool,
    /// The last round-trip time in microseconds (`m_last_ping_time`).
    pub last_us: Option<i64>,
    /// The minimum round-trip time in microseconds (`m_min_ping_time`).
    pub min_us: Option<i64>,
}

/// A `ping` or `pong` payload: the nonce, little-endian.
fn nonce_msg(cmd: CommandString, nonce: u64) -> OutMsg {
    let mut payload = Vec::with_capacity(8);
    Ping {
        nonce,
    }
    .encode(&mut payload);
    OutMsg::new(cmd, payload)
}

/// Core's `MaybeSendPing` (section 4 of `SendMessages`): disconnects on a 20-minute timeout or
/// pushes a ping when one is due. `now_us` is `GetTime<µs>()`.
// Core: net_processing.cpp:6148-6184 MaybeSendPing
pub fn maybe_send_ping(core: &NetCore, peer: &PeerHandle, state: &mut PingState, now_us: i64) {
    // Core: net.cpp:2293-2296 CConnman::ShouldRunInactivityChecks (duration_cast truncates).
    let inactivity_checks = peer.connected() + core.config().peer_timeout < now_us / 1_000_000;
    if inactivity_checks && state.nonce_sent != 0 && now_us > state.start_us + TIMEOUT_INTERVAL_US {
        tracing::debug!(
            target: "net",
            "ping timeout: {:.6}s peer={}",
            0.000001 * (now_us - state.start_us) as f64,
            peer.id()
        );
        core.disconnect(peer.id(), Why::Processor("ping timeout"));
        return;
    }

    let mut ping_send = false;
    if state.queued {
        // RPC ping request by user.
        ping_send = true;
    }
    if state.nonce_sent == 0 && now_us > state.start_us + PING_INTERVAL_US {
        // Ping automatically sent as a latency probe & keepalive.
        ping_send = true;
    }
    if ping_send {
        // `GetRand<uint64_t>()` until non-zero: a fresh context each try.
        let nonce = loop {
            let n = core.get_rand(u64::MAX);
            if n != 0 {
                break n;
            }
        };
        state.queued = false;
        state.start_us = now_us;
        state.nonce_sent = nonce;
        core.push(peer, nonce_msg(PING, nonce));
    }
}

/// Core's `ping` branch: replies `pong` with the nonce at once.
// Core: net_processing.cpp:5456-5472 ProcessMessage (PING)
pub fn on_ping(core: &NetCore, peer: &PeerHandle, payload: &[u8]) -> Step {
    let mut data = payload;
    let nonce = match Ping::decode(&mut data) {
        Ok(p) => p.nonce,
        Err(e) => return Step::Throw(Throw::from_decode(&P2pDecodeError::Consensus(e))),
    };
    core.push(peer, nonce_msg(PONG, nonce));
    Step::Done
}

/// Core's `pong` branch; never throws. `recv_us` is the message's socket-read time.
// Core: net_processing.cpp:5474-5527 ProcessMessage (PONG)
pub fn on_pong(peer: &PeerHandle, state: &mut PingState, payload: &[u8], recv_us: i64) -> Step {
    let ping_end = recv_us;
    let mut nonce = 0u64;
    let avail = payload.len();
    let mut finished = false;
    let mut problem = "";

    if avail >= 8 {
        nonce = u64::from_le_bytes(payload[..8].try_into().expect("8 bytes"));
        // Only process pong message if there is an outstanding ping (old ping without nonce
        // should never pong).
        if state.nonce_sent != 0 {
            if nonce == state.nonce_sent {
                // Matching pong received, this ping is no longer outstanding.
                finished = true;
                let ping_time = ping_end - state.start_us;
                if ping_time >= 0 {
                    // Let connman know about this successful ping-pong.
                    peer.pong_received(ping_time);
                    state.last_us = Some(ping_time);
                    state.min_us = Some(state.min_us.map_or(ping_time, |m| m.min(ping_time)));
                } else {
                    // This should never happen.
                    problem = "Timing mishap";
                }
            } else {
                // Nonce mismatches are normal when pings are overlapping.
                problem = "Nonce mismatch";
                if nonce == 0 {
                    // This is most likely a bug in another implementation somewhere; cancel this
                    // ping.
                    finished = true;
                    problem = "Nonce zero";
                }
            }
        } else {
            problem = "Unsolicited pong without ping";
        }
    } else {
        // This is most likely a bug in another implementation somewhere; cancel this ping.
        finished = true;
        problem = "Short payload";
    }

    if !problem.is_empty() {
        tracing::debug!(
            target: "net",
            "pong peer={}: {problem}, {:x} expected, {nonce:x} received, {avail} bytes",
            peer.id(),
            state.nonce_sent
        );
    }
    if finished {
        state.nonce_sent = 0;
    }
    Step::Done
}
