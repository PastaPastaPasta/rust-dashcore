//! Transcript equality with dashd v24.0.0 (P2a §6.1, the phase E acceptance): each fixture
//! under `tests/fixtures/dashd-v24/` (`contrib/node/p2a-transcripts/capture.py`; provenance, and
//! which fixtures are rc.3 captures, in the directory's `PROVENANCE.md`) replayed by
//! [`dashd_p2p::sim::transcript`], whose docs list the masks and the messages it drops (later
//! tasks' sends).

use std::sync::Arc;

use dash_params::{ChainParamsBuilder, Network, Release};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::sim::Sim;
use dashd_p2p::sim::transcript::{Hooks, NoHooks};
use dashd_p2p::{Registry, Seams};

/// A node with dashd's defaults where `sim` departs from them: `-peertimeout` is 60 s.
fn sim(opts: ProcessorOptions) -> Sim {
    let p = Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    Sim::with(p, id, opts, Registry::new(), Seams::default(), |cfg| {
        cfg.peer_timeout = 60;
    })
}

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/dashd-v24/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn replay_with(name: &str, opts: ProcessorOptions) {
    dashd_p2p::sim::transcript::replay(name, &fixture(name), sim(opts), &mut NoHooks);
}

fn replay(name: &str) {
    replay_with(name, ProcessorOptions::default());
}

/// `-blocksonly=1` (P5a D23): relay 0 in `version`, no `senddsq`, and the `tx` and `tx`-inv
/// disconnects.
#[test]
fn blocksonly_violations() {
    let opts = ProcessorOptions {
        blocks_only: true,
        ..ProcessorOptions::default()
    };
    replay_with("blocksonly-violations", opts);
}

macro_rules! transcripts {
    ($($test:ident => $file:literal,)*) => {
        $(
            #[test]
            fn $test() {
                replay($file);
            }
        )*
    };
}

transcripts! {
    handshake_inbound => "handshake-inbound",
    handshake_outbound_full_relay => "handshake-outbound-full-relay",
    handshake_block_relay_only => "handshake-block-relay-only",
    handshake_addr_fetch => "handshake-addr-fetch",
    handshake_feeler => "handshake-feeler",
    handshake_manual => "handshake-manual",
    headers_2000 => "headers-2000",
    headers_2000_bit11 => "headers-2000-bit11",
    headers2_8000 => "headers2-8000",
    announce => "announce",
    evict_slow_chain => "evict-slow-chain",
}

/// The caller's side of `restart`: how many peers the node still held at each.
#[derive(Default)]
struct Restarts(Vec<usize>);

impl Hooks for Restarts {
    fn input(&mut self, sim: &mut Sim, f: &[&str]) -> bool {
        if f[0] != "restart" {
            return false;
        }
        self.0.push(sim.core.peers().len());
        true
    }
}

/// `restart` (GV1b R9): the peer's connection ends with no `closed` recorded, the caller
/// restarts a node that holds no peer, and the same peer's next `accept` is a new connection
/// whose handshake is dashd's again.
#[test]
fn restart_ends_every_connection_unrecorded() {
    let text = fixture("handshake-inbound");
    let session: Vec<&str> = text.lines().skip_while(|l| !l.starts_with("accept ")).collect();
    let script = format!("{text}restart\n{}\n", session.join("\n"));
    let mut hooks = Restarts::default();
    let sim = sim(ProcessorOptions::default());
    dashd_p2p::sim::transcript::replay("restart", &script, sim, &mut hooks);
    assert_eq!(hooks.0, [0], "one restart, with no peer left");
}

/// A caller that does not take `restart` fails the replay.
#[test]
#[should_panic(expected = "restart: the caller restarts the node")]
fn restart_needs_the_caller() {
    let script = format!("{}restart\n", fixture("handshake-inbound"));
    dashd_p2p::sim::transcript::replay(
        "restart",
        &script,
        sim(ProcessorOptions::default()),
        &mut NoHooks,
    );
}
