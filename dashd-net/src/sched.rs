//! The `connman_sched` model check (P2a §6.7; node-fuzz's model target): an input is a script
//! over a mainnet [`DialSim`] whose `opencon` loop runs as the node's thread runs it. After
//! every step the open connections must keep `ThreadOpenConnections`' rules (v24 `net.cpp`):
//!
//! - at most 11 connections hold outbound grants (`semOutbound`, `m_max_outbound` = 8
//!   full-relay + 2 block-relay + 1 feeler, `net.cpp:4042`) — field `grants`;
//! - a ninth or later full-relay connection opens only while `m_try_another_outbound_peer` is
//!   set — `full_relay_limit`; a third or later block-relay one only while
//!   `m_start_extra_block_relay_peers` is — `block_relay_limit` (the branch order at
//!   `net.cpp:3092-3128`). The script's addresses are IPv4 only, so Core's other ways to a ninth
//!   full-relay connection (an extra network peer, `net.cpp:3131-3142`, and `onion_only`) never
//!   apply; the rule would need them if the script ever adds another network;
//! - full-relay and block-relay connections are in distinct netgroups (`net.cpp:3207`) —
//!   `netgroup`;
//! - no two connections go to the same service (`AlreadyConnectedToAddress`) — `duplicate`.
//!
//! Two properties follow from those: at most one feeler is open (feelers come after both ladders
//! are full, so only the eleventh grant is left) — `feelers`; and the harness's own checks hold,
//! every connection going to an address the script gave AddrMan (no seeds or anchors leak in)
//! and not to one it made unreachable — `unknown_address`, `unreachable`.
//!
//! The script: one byte for the number of addresses (16 to 63), three bytes per address
//! (`a.b.c.1:9999`, `a` mapped to a public first octet), then up to [`MAX_STEPS`] steps of an
//! op byte and an argument byte, with at most [`MAX_TICKS`] `opencon` ticks in all. Feelers are closed as soon as they open (the processor's
//! disconnect after `version`) until the script toggles that off; then they hold their slot until
//! a close step.

use std::collections::{BTreeSet, HashSet};
use std::sync::atomic::Ordering;
use std::time::Duration;

use dash_p2p::{Address, ConnectionType, DialLoop, NetAddr, Service};
use dash_params::{ChainParamsBuilder, NetworkKind, Release};

use crate::sim::{DialEvent, DialSim, SIM_START, SimPolicy, full_node};

/// The ops a script step picks from.
const OPS: u8 = 7;

/// Script steps per input.
pub const MAX_STEPS: usize = 48;

/// `opencon` ticks per input: a tick can run 100 AddrMan selects (Core's `nTries`), so this
/// bounds an input's cost.
pub const MAX_TICKS: usize = 32;

/// The input as a stream of bytes; zeros once it runs out.
struct Input<'a>(&'a [u8]);

impl Input<'_> {
    fn u8(&mut self) -> u8 {
        let (&b, rest) = self.0.split_first().unwrap_or((&0, &[]));
        self.0 = rest;
        b
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A public first octet: not 0, 10, 127 or 224 and above.
fn first_octet(b: u8) -> u8 {
    const PUBLIC: [u8; 221] = {
        let mut out = [0u8; 221];
        let (mut i, mut v) = (0, 1u8);
        while i < 221 {
            if v != 10 && v != 127 {
                out[i] = v;
                i += 1;
            }
            v += 1;
        }
        out
    };
    PUBLIC[usize::from(b) % PUBLIC.len()]
}

/// The automatic connection types that hold an outbound grant.
fn holds_grant(t: ConnectionType) -> bool {
    matches!(
        t,
        ConnectionType::OutboundFullRelay
            | ConnectionType::BlockRelay
            | ConnectionType::Feeler
            | ConnectionType::AddrFetch
    )
}

/// Checks the open connections; the error is the rule that broke.
fn check_table(
    sim: &DialSim,
    known: &HashSet<Service>,
    unreachable: &HashSet<Service>,
) -> Result<(), &'static str> {
    let peers: Vec<_> =
        sim.core.peers().iter().filter(|p| !p.disconnect_requested()).cloned().collect();
    if peers.iter().filter(|p| holds_grant(p.conn_type())).count() > 11 {
        return Err("grants");
    }
    if sim.of_type(ConnectionType::Feeler).len() > 1 {
        return Err("feelers");
    }
    let mut groups = BTreeSet::new();
    let mut services = HashSet::new();
    for p in &peers {
        let s = &p.addr().service;
        if !services.insert(s.clone()) {
            return Err("duplicate");
        }
        if !known.contains(s) {
            return Err("unknown_address");
        }
        if unreachable.contains(s) {
            return Err("unreachable");
        }
        if matches!(p.conn_type(), ConnectionType::OutboundFullRelay | ConnectionType::BlockRelay)
            && !groups.insert(sim.core.netgroup().group(&s.addr))
        {
            return Err("netgroup");
        }
    }
    Ok(())
}

/// Runs the script in `data`.
///
/// # Errors
///
/// The name of the first rule the connections broke.
pub fn check(data: &[u8]) -> Result<(), &'static str> {
    run(data).map(drop)
}

/// [`check`], returning the `sim` at the end of the script.
fn run(data: &[u8]) -> Result<DialSim, &'static str> {
    let mut input = Input(data);
    let params = ChainParamsBuilder::new(NetworkKind::Mainnet, Release::V24_0)
        .build()
        .expect("mainnet parameters");
    let port = params.default_port;
    let mut sim = DialSim::new(params.into(), |cfg| {
        cfg.dns_seed = false;
        cfg.fixed_seeds = false;
    });
    let policy = SimPolicy::default();

    let n = 16 + usize::from(input.u8()) % 48;
    let addrs: Vec<Address> = (0..n)
        .map(|_| {
            let ip = [first_octet(input.u8()), input.u8(), input.u8(), 1];
            full_node(Service::new(NetAddr::ipv4(ip), port), SIM_START)
        })
        .collect();
    sim.add_addrs(&addrs, &NetAddr::ipv4([250, 1, 1, 1]));
    let known: HashSet<Service> = addrs.iter().map(|a| a.service.clone()).collect();
    let mut unreachable = HashSet::new();

    let mut ticks = 0;
    for _ in 0..MAX_STEPS {
        if input.is_empty() {
            break;
        }
        let (op, arg) = (input.u8(), input.u8());
        match op % OPS {
            0 => {
                for _ in 0..=arg % 4 {
                    if ticks == MAX_TICKS {
                        break;
                    }
                    ticks += 1;
                    let full = sim.of_type(ConnectionType::OutboundFullRelay).len();
                    let block = sim.of_type(ConnectionType::BlockRelay).len();
                    let try_another = policy.try_another.load(Ordering::SeqCst);
                    let extra = policy.extra_block_relay.load(Ordering::SeqCst);
                    sim.exec.events.clear();
                    let blocked = sim.run(DialLoop::OpenCon, &policy, 1);
                    for e in &sim.exec.events {
                        let DialEvent::Opened {
                            conn_type,
                            ..
                        } = e
                        else {
                            continue;
                        };
                        if *conn_type == ConnectionType::OutboundFullRelay
                            && full >= 8
                            && !try_another
                        {
                            return Err("full_relay_limit");
                        }
                        if *conn_type == ConnectionType::BlockRelay && block >= 2 && !extra {
                            return Err("block_relay_limit");
                        }
                    }
                    check_table(&sim, &known, &unreachable)?;
                    if blocked {
                        break;
                    }
                }
            }
            1 => {
                let open: Vec<_> = sim.core.peers().iter().map(|p| p.id()).collect();
                if !open.is_empty() {
                    sim.close(open[usize::from(arg) % open.len()]);
                }
            }
            2 => {
                policy.try_another.fetch_xor(true, Ordering::SeqCst);
            }
            3 => {
                policy.extra_block_relay.fetch_xor(true, Ordering::SeqCst);
            }
            4 => {
                let s = addrs[usize::from(arg) % addrs.len()].service.clone();
                // A connection already open stays; only new attempts fail.
                if !sim.core.peers().iter().any(|p| p.addr().service == s) {
                    sim.exec.unreachable.insert(s.clone());
                    unreachable.insert(s);
                }
            }
            5 => sim.clock.advance(Duration::from_secs(u64::from(arg) * 10)),
            _ => sim.exec.close_feelers = !sim.exec.close_feelers,
        }
        check_table(&sim, &known, &unreachable)?;
    }
    Ok(sim)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inputs from a fixed xorshift stream; none may break a rule.
    #[test]
    fn scripted_runs_keep_the_rules() {
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        for _ in 0..24 {
            let data: Vec<u8> = (0..200)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    x as u8
                })
                .collect();
            check(&data).unwrap();
        }
    }

    /// Addresses `[first_octet, second, third]` as the script encodes them.
    fn script(addrs: &[[u8; 3]], steps: &[[u8; 2]]) -> Vec<u8> {
        let mut data = vec![(addrs.len() - 16) as u8];
        data.extend(addrs.iter().flatten());
        data.extend(steps.iter().flatten());
        data
    }

    /// Every address in one /16: one automatic connection at most, whatever the flags; with
    /// feelers left open, at most one at a time.
    #[test]
    fn one_netgroup_one_connection() {
        let addrs: Vec<[u8; 3]> = (0..16u8).map(|i| [40, 5, i]).collect();
        let mut steps = vec![[2, 0], [3, 0], [6, 0]];
        for _ in 0..4 {
            steps.extend([[0, 3], [5, 255]]);
        }
        let sim = run(&script(&addrs, &steps)).unwrap();
        let auto = sim.of_type(ConnectionType::OutboundFullRelay).len()
            + sim.of_type(ConnectionType::BlockRelay).len();
        assert_eq!(auto, 1);
        assert!(sim.of_type(ConnectionType::Feeler).len() <= 1);
    }

    /// The checker fires: two full-relay connections in one /16, and one to an address the script
    /// never gave, opened behind the dial loop's back.
    #[test]
    fn broken_tables_are_reported() {
        use dash_p2p::{OpenRequest, Target};

        use crate::driver::DialExec;

        let mut sim = run(&script(&[[1, 1, 1]; 16], &[])).unwrap();
        let open = |sim: &mut DialSim, ip: [u8; 4]| {
            let req = OpenRequest {
                target: Target::Addr(full_node(Service::new(NetAddr::ipv4(ip), 9999), SIM_START)),
                conn_type: ConnectionType::OutboundFullRelay,
                count_failure: false,
                grant: None,
                use_v2: false,
                masternode: false,
                masternode_probe: false,
            };
            sim.exec.open(&sim.core, req);
        };
        let known: HashSet<Service> = [
            Service::new(NetAddr::ipv4([1, 1, 1, 1]), 9999),
            Service::new(NetAddr::ipv4([1, 1, 2, 1]), 9999),
        ]
        .into();
        open(&mut sim, [1, 1, 1, 1]);
        check_table(&sim, &known, &HashSet::new()).unwrap();
        open(&mut sim, [1, 1, 2, 1]);
        assert_eq!(check_table(&sim, &known, &HashSet::new()), Err("netgroup"));
        open(&mut sim, [9, 9, 9, 9]);
        assert_eq!(check_table(&sim, &HashSet::new(), &HashSet::new()), Err("unknown_address"));
    }

    /// The ladder fills when the script only dials: eight full-relay and two block-relay
    /// connections.
    #[test]
    fn dialling_fills_the_ladder() {
        let mut data = vec![47];
        for i in 0..63u8 {
            data.extend([i, i, 0]);
        }
        data.extend([0, 3].repeat(MAX_STEPS));
        let sim = run(&data).unwrap();
        assert_eq!(sim.of_type(ConnectionType::OutboundFullRelay).len(), 8);
        assert_eq!(sim.of_type(ConnectionType::BlockRelay).len(), 2);
    }
}
