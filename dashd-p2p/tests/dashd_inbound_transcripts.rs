//! Inbound transcript equality with dashd v24.0.0-rc.3 (P2b D9, M2 exit 2's inbound half): each
//! `inbound-*` fixture under `tests/fixtures/dashd-v24/` (`contrib/node/p2b-transcripts/capture.py`;
//! provenance in the directory's `PROVENANCE.md`) replayed by [`dashd_p2p::sim::transcript`],
//! whose `accept` record connects each peer through `NetCore::accept` (P2b D8). The sim is set up
//! with the fixture's dashd arguments.

use std::sync::Arc;

use dash_p2p::conn::Permissions;
use dash_p2p::permissions::parse_whitelist;
use dash_p2p::{NetAddr, NetConfig, SubNet};
use dash_params::{ChainParamsBuilder, Network, Release};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity, local_services};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::seams::Work;
use dashd_p2p::sim::Sim;
use dashd_p2p::sim::transcript::NoHooks;
use dashd_p2p::{Registry, Seams};

/// The dashd arguments a fixture adds to the capture tool's.
#[derive(Default)]
struct Args {
    /// `-v2transport=0`.
    no_v2: bool,
    /// `-minimumchainwork=ff…ff`.
    max_min_work: bool,
    /// `-whitelist=download@127.0.0.1`.
    download_whitelist: bool,
    /// `-maxconnections`.
    max_connections: Option<usize>,
    /// `-peerbloomfilters=0`.
    no_bloom: bool,
    /// `-whitelist` values (P4a; `-whitelistrelay=1` is the default).
    whitelist: &'static [&'static str],
    /// `-masternodeblsprivkey`: an active masternode.
    masternode: bool,
}

/// A node with dashd's defaults where `sim` departs from them (`-peertimeout` is 60 s), plus
/// the fixture's arguments.
fn sim(args: &Args) -> Sim {
    let p = Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let opts = IdentityOptions {
        v2_transport: !args.no_v2,
        peer_bloom_filters: !args.no_bloom,
        masternode: args.masternode,
        // Core: init.cpp:1099-1104, `-masternodeblsprivkey` soft-sets `-peerblockfilters`.
        peer_block_filters: args.masternode,
        ..IdentityOptions::new()
    };
    let id = local_identity(&p, &opts).unwrap();
    let sim = Sim::with(
        p,
        id,
        ProcessorOptions::default(),
        Registry::new(),
        Seams::default(),
        |cfg: &mut NetConfig| {
            cfg.peer_timeout = 60;
            cfg.local_services = local_services(&opts);
            if args.download_whitelist {
                cfg.whitelist =
                    vec![(SubNet::single(NetAddr::ipv4([127, 0, 0, 1])), Permissions::DOWNLOAD)];
            }
            if let Some(n) = args.max_connections {
                cfg.max_connections = n;
            }
            cfg.active_masternode = args.masternode;
            cfg.whitelist
                .extend(args.whitelist.iter().map(|w| parse_whitelist(w, false).expect(w)));
        },
    );
    if args.max_min_work {
        sim.chain.set_min_chain_work(Work::MAX);
    }
    sim
}

fn replay(name: &str, args: Args) {
    let path = format!("{}/tests/fixtures/dashd-v24/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    dashd_p2p::sim::transcript::replay(name, &text, sim(&args), &mut NoHooks);
}

#[test]
fn inbound_gates() {
    replay(
        "inbound-gates",
        Args {
            no_v2: true,
            ..Args::default()
        },
    );
}

#[test]
fn inbound_versions() {
    replay("inbound-versions", Args::default());
}

#[test]
fn inbound_serve() {
    replay("inbound-serve", Args::default());
}

#[test]
fn inbound_serve_minwork() {
    replay(
        "inbound-serve-minwork",
        Args {
            max_min_work: true,
            ..Args::default()
        },
    );
}

#[test]
fn inbound_serve_download() {
    replay(
        "inbound-serve-download",
        Args {
            max_min_work: true,
            download_whitelist: true,
            ..Args::default()
        },
    );
}

/// 21 inbound slots: every candidate is protected, so each new peer is dropped (P2b D12).
#[test]
fn inbound_evict_21() {
    replay(
        "inbound-evict-21",
        Args {
            max_connections: Some(32),
            ..Args::default()
        },
    );
}

/// 29 inbound slots: each new peer evicts one (P2b D12).
#[test]
fn inbound_evict() {
    replay(
        "inbound-evict",
        Args {
            max_connections: Some(40),
            ..Args::default()
        },
    );
}

/// `-whitebind` listeners' flags, from 11.1.0.x: `mempool` without NODE_BLOOM and an
/// unsolicited `mnlistdiff` (+100), punished or not by `noban` and `mempool`; discouraged
/// addresses come back, `noban` or not (P4a D5).
#[test]
fn inbound_permissions() {
    replay(
        "inbound-permissions",
        Args {
            no_bloom: true,
            ..Args::default()
        },
    );
}

/// `-whitelist` subnets with the implicit flags and an explicit `mempool@` address (P4a D5).
#[test]
fn inbound_whitelist() {
    replay(
        "inbound-whitelist",
        Args {
            no_bloom: true,
            whitelist: &["11.2.0.0/16", "mempool@11.4.0.1"],
            ..Args::default()
        },
    );
}

/// Out of IBD on 8001 blocks: `getheaders`/`getheaders2` from genesis get 2000 headers, and
/// 8000 compressed ones as the peer's bit 11 allows (P4a D13).
#[test]
fn inbound_serve_8000() {
    replay("inbound-serve-8000", Args::default());
}

/// 30 inbound slots from four /16 netgroups, then five newcomers: the netgroup protection and
/// the largest-netgroup pick decide each eviction (P4a D11).
#[test]
fn inbound_evict_groups() {
    replay(
        "inbound-evict-groups",
        Args {
            max_connections: Some(41),
            ..Args::default()
        },
    );
}

/// An active masternode's full table: the verified masternode leg is never evicted and does
/// not count towards the inbound limit (P4a D12).
#[test]
fn inbound_evict_mn() {
    replay(
        "inbound-evict-mn",
        Args {
            max_connections: Some(125),
            masternode: true,
            ..Args::default()
        },
    );
}

/// P2P-12 at M2: nothing is requested, so each readable `qfcommit` costs +10 and ten
/// discourage the peer; nine do not, and an unreadable one is never scored (P4a D8).
#[test]
fn inbound_qfcommit() {
    replay("inbound-qfcommit", Args::default());
}
