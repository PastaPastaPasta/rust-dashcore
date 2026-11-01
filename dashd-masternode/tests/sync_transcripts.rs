//! Masternode-sync transcript equality with dashd v24.0.0-rc.4 (SP1): each fixture under
//! `tests/fixtures/dashd-v24-rc4/` (`contrib/node/sp1-transcripts/capture.py`; provenance in the
//! directory's `PROVENANCE.md`) replayed by [`dashd_p2p::sim::transcript`] with SP1's tick
//! running where dashd's scheduler ran it. dashd's `mempool`, `govsync` and the `full-sync`
//! disconnects are compared, and its `mnsync status` after every step is checked against
//! [`MasternodeSync`].

use std::sync::Arc;

use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dashd_masternode::flatdb::FlatObject;
use dashd_masternode::netfulfilled::{self as nf, NetFulfilledRequestManager, NetFulfilledStore};
use dashd_masternode::sync::{BlockRef, MasternodeSync};
use dashd_masternode::sync_manager::{EmptyGovernance, FullSyncNotifier, SyncConfig, SyncManager};
use dashd_masternode::sync_net::{CoreSyncNet, SyncStatusHandler};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::seams::ChainAccess;
use dashd_p2p::sim::transcript::{Hooks, replay};
use dashd_p2p::sim::{SIM_START, Sim};
use dashd_p2p::{Registry, Seams};

struct SyncHooks {
    mgr: SyncManager,
    /// Whether the chain the fixture set up has been notified (dashd mined it at `SIM_START`).
    started: bool,
}

impl SyncHooks {
    fn sync(&self) -> &MasternodeSync {
        self.mgr.sync()
    }

    /// dashd mined the fixture's blocks before its transcript: their `UpdatedBlockTip`, out of
    /// IBD, left the tip at the best header.
    fn start(&mut self, sim: &Sim) {
        if std::mem::replace(&mut self.started, true) {
            return;
        }
        let tip = sim.chain.snapshot().tip;
        if tip.tip_height == 0 {
            return;
        }
        let best = BlockRef {
            height: tip.best_header_height,
            hash: *tip.best_header_hash.as_bytes(),
        };
        let new = BlockRef {
            height: tip.tip_height,
            hash: *tip.tip_hash.as_bytes(),
        };
        self.sync().updated_block_tip(Some(best), new, false);
    }
}

impl Hooks for SyncHooks {
    fn input(&mut self, _sim: &mut Sim, f: &[&str]) -> bool {
        if f[0] != "mnsyncreset" {
            return false;
        }
        // Core: rpc/node.cpp:143-147 (`mnsync reset`)
        self.sync().reset(true, true);
        true
    }

    fn check(&mut self, sim: &Sim, f: &[&str]) -> bool {
        if f[0] != "mnsync" {
            return false;
        }
        self.start(sim);
        let s = self.sync();
        let got = format!(
            "{} {} {} {} {} {}",
            s.asset_id(),
            s.asset_name(),
            s.asset_start_time(),
            s.attempt(),
            u8::from(s.is_blockchain_synced()),
            u8::from(s.is_synced())
        );
        assert_eq!(got, f[1..].join(" "), "mnsync status");
        true
    }

    fn runs_sync_tick(&self) -> bool {
        true
    }

    fn settled(&mut self, sim: &mut Sim) {
        self.start(sim);
        self.mgr.process_tick();
    }
}

fn params() -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap())
}

/// A regtest node with dashd's defaults where `sim` departs from them (`-peertimeout` 60 s), the
/// `ssc` handler registered, and SP1's tick over the sim's network core.
fn run(name: &str, blocks_only: bool, governance: bool) {
    run_with(name, blocks_only, blocks_only, governance);
}

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/dashd-v24-rc4/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// [`run`], with the tick's `-blocksonly` (`IsBlockRelayOnly`) apart from the processor's; the
/// node's fulfilled requests at the end are dashd's `netfulfilled.dat` from the same run.
fn run_with(name: &str, blocks_only: bool, tick_blocks_only: bool, governance: bool) {
    let got = run_text(name, &fixture(name), blocks_only, tick_blocks_only, governance);
    assert_eq!(requests_by_peer(&got), requests_by_peer(&core_netfulfilled(name)), "{name}");
}

/// [`run_with`] on `text`, a fixture's records (a negative control edits them); the node's
/// fulfilled requests at the end.
fn run_text(
    name: &str,
    text: &str,
    blocks_only: bool,
    tick_blocks_only: bool,
    governance: bool,
) -> NetFulfilledStore {
    let p = params();
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let opts = ProcessorOptions {
        blocks_only,
        ..ProcessorOptions::default()
    };
    let sync_slot = Arc::new(std::sync::OnceLock::<Arc<MasternodeSync>>::new());
    let mut registry = Registry::new();
    // The handler needs the sync, which needs the sim's clock: a late-bound stand-in.
    let late = LateSync(Arc::clone(&sync_slot));
    registry.register(Box::new(late));
    let sim = Sim::with(Arc::clone(&p), id, opts, registry, Seams::default(), |cfg| {
        cfg.peer_timeout = 60;
        cfg.blocks_only = blocks_only;
    });
    let clock: Arc<dyn dash_clock::Clock> = sim.clock.clone();
    let net = Arc::new(CoreSyncNet::new(Arc::clone(&sim.core), tick_blocks_only));
    net.bind_requests(sim.lp.logic().requested_objects());
    let netfulfilled = Arc::new(NetFulfilledRequestManager::new(
        Arc::clone(&clock),
        i64::from(p.fulfilled_request_expire_time),
    ));
    let dir = tempfile::tempdir().unwrap();
    assert!(netfulfilled.load_cache(&dir.path().join("netfulfilled.dat"), p.message_start, false));
    let notifier = FullSyncNotifier {
        net: net.clone(),
        netfulfilled: Arc::clone(&netfulfilled),
    };
    let sync = Arc::new(MasternodeSync::new(Arc::clone(&clock), Box::new(notifier)));
    assert!(sync_slot.set(Arc::clone(&sync)).is_ok());
    let cfg = SyncConfig {
        mockable_chain: true,
        mainnet: false,
        sync_mempool: true,
    };
    let gov = Arc::new(EmptyGovernance {
        valid: governance,
    });
    let mgr = SyncManager::new(clock, cfg, sync, Arc::clone(&netfulfilled), net, gov);
    let mut hooks = SyncHooks {
        mgr,
        started: false,
    };
    replay(name, text, sim, &mut hooks);
    netfulfilled.snapshot()
}

/// Each peer's fulfilled requests and their expiries, without the peer's address (the sim's
/// addresses are not dashd's), sorted; `allow-sync-<ms>` (keyed by the node's start) reads as
/// `allow-sync`.
fn requests_by_peer(store: &NetFulfilledStore) -> Vec<Vec<(String, i64)>> {
    let mut peers: Vec<Vec<(String, i64)>> = store
        .requests
        .values()
        .map(|entries| {
            entries
                .iter()
                .map(|(name, expiry)| {
                    let name = String::from_utf8(name.clone()).unwrap();
                    let name = if name.starts_with("allow-sync-") {
                        "allow-sync".to_owned()
                    } else {
                        name
                    };
                    (name, *expiry)
                })
                .collect()
        })
        .collect();
    peers.sort();
    peers
}

/// dashd's `netfulfilled.dat` from the fixture's run: Core's `CFlatDB` file (magic message,
/// message start, object, SHA256d), its object read as SP1's store.
fn core_netfulfilled(name: &str) -> NetFulfilledStore {
    let path = format!(
        "{}/tests/fixtures/dashd-v24-rc4/{name}.netfulfilled.dat",
        env!("CARGO_MANIFEST_DIR")
    );
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let magic = usize::from(bytes[0]);
    let body = &bytes[1 + magic + 4..bytes.len() - 32];
    // Core's object has no version string; SP1's starts with one.
    let mut data = vec![u8::try_from(nf::VERSION.len()).unwrap()];
    data.extend_from_slice(nf::VERSION.as_bytes());
    data.extend_from_slice(body);
    let mut store = NetFulfilledStore::default();
    let mut rest = data.as_slice();
    assert_eq!(store.read(&mut rest), Ok(()));
    assert!(rest.is_empty(), "{path}: the whole object is read");
    store
}

/// [`SyncStatusHandler`] over a sync made after the registry.
struct LateSync(Arc<std::sync::OnceLock<Arc<MasternodeSync>>>);

impl dashd_p2p::NetHandler for LateSync {
    fn commands(&self) -> &'static [dash_p2p::CommandString] {
        &[dash_p2p::CommandString::SSC]
    }

    fn core_handler(&self) -> Option<dashd_p2p::processing::registry::CoreHandler> {
        Some(dashd_p2p::processing::registry::CoreHandler::SyncManager)
    }

    fn process_message(
        &mut self,
        cx: &mut dashd_p2p::HandlerCx<'_>,
        cmd: &dash_p2p::CommandString,
        payload: &[u8],
    ) -> Result<(), dash_p2p::policy::Throw> {
        let sync = Arc::clone(self.0.get().expect("the sync"));
        SyncStatusHandler::new(sync).process_message(cx, cmd, payload)
    }
}

#[test]
fn mnsync_disabled() {
    run("mnsync-disabled", false, false);
}

#[test]
fn mnsync_four_peers() {
    run("mnsync-four-peers", false, false);
}

#[test]
fn mnsync_blocksonly() {
    run("mnsync-blocksonly", true, false);
}

#[test]
fn mnsync_governance() {
    run("mnsync-governance", false, true);
}

/// The `allow-sync` path (SP1b, DEC-562): after `mnsync reset`, dashd's tick kept the manual
/// and the `noban` peer, whose `full-sync` their first `allow-sync` check removed, and
/// disconnected both outbound peers.
// Core: node/sync_manager.cpp:186-202
#[test]
fn mnsync_allow_sync() {
    // Its end state is the manual and noban peers' `allow-sync`, the outbound peers'
    // `full-sync` and peer 1's `mempool-sync`, with dashd's expiries.
    run("mnsync-allow-sync", false, false);
}

#[test]
fn the_fixtures_start_at_sim_start() {
    for name in [
        "mnsync-disabled",
        "mnsync-four-peers",
        "mnsync-blocksonly",
        "mnsync-governance",
        "mnsync-allow-sync",
    ] {
        assert!(fixture(name).contains(&format!("mocktime {SIM_START}")));
    }
}

/// `mnsync-allow-sync` with one record changed, which must fail the replay.
fn allow_sync_with(from: &str, to: &str) {
    let text = fixture("mnsync-allow-sync");
    assert_eq!(text.matches(from).count(), 1, "{from}");
    run_text("mnsync-allow-sync", &text.replace(from, to), false, false, false);
}

/// Negative control: without `noban`, the inbound peer is disconnected as recently synced at +48,
/// where dashd kept it.
#[test]
#[should_panic(expected = "mnsync-allow-sync:159 `time 1700000048`: peer 3")]
fn the_noban_peer_is_compared() {
    allow_sync_with("accept 3 from 127.0.0.1 perm noban", "accept 3 from 127.0.0.1");
}

/// Negative control: dialled as a plain outbound peer, the manual peer is disconnected at +48 too.
#[test]
#[should_panic(expected = "mnsync-allow-sync:159 `time 1700000048`: peer 2")]
fn the_manual_peer_is_compared() {
    allow_sync_with("connect 2 manual 0", "connect 2 outbound-full-relay 0");
}

/// Negative control: without governance, the node sends no `govsync` where dashd did (its first,
/// to peer 1 at +42).
#[test]
#[should_panic(expected = "mnsync-governance:119 `time 1700000042`: peer 1")]
fn the_governance_fixture_fails_without_governance() {
    run("mnsync-governance", false, false);
}

/// Negative control: with every peer block-relay-only to the tick, the node sends no `mempool`
/// where dashd did.
#[test]
#[should_panic(expected = "mnsync-disabled:115 `time 1700000036`: peer 1")]
fn the_mempool_requests_are_compared() {
    run_with("mnsync-disabled", false, true, false);
}
