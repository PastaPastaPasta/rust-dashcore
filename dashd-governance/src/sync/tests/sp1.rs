//! SP1's `mnsync-governance` replay (`dashd-masternode/tests/sync_transcripts.rs`) with
//! [`NodeGovernanceSync`] in place of `EmptyGovernance`.

use std::sync::{Arc, OnceLock};

use dash_dml::MnList;
use dash_governance::manager::Manager;
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::BlockHash;
use dash_types::Numeric;
use dashd_masternode::flatdb::FlatObject;
use dashd_masternode::netfulfilled::{self as nf, NetFulfilledRequestManager, NetFulfilledStore};
use dashd_masternode::sync::{BlockRef, MasternodeSync};
use dashd_masternode::sync_manager::{FullSyncNotifier, SyncConfig, SyncManager};
use dashd_masternode::sync_net::{CoreSyncNet, SyncStatusHandler};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::seams::ChainAccess;
use dashd_p2p::sim::Sim;
use dashd_p2p::sim::transcript::{Hooks, replay};
use dashd_p2p::{Registry, Seams};

use super::{NoExec, Scratch, Tip, handle_over, params};
use crate::rand::ReplayRng;
use crate::sync::NodeGovernanceSync;

const NAME: &str = "mnsync-governance";

struct SyncHooks {
    mgr: SyncManager,
    started: bool,
}

impl SyncHooks {
    fn sync(&self) -> &MasternodeSync {
        self.mgr.sync()
    }

    /// dashd mined the fixture's blocks before its transcript, out of IBD.
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

/// The `ssc` handler over a sync made after the registry.
struct LateSync(Arc<OnceLock<Arc<MasternodeSync>>>);

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

fn fixture(suffix: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../dashd-masternode/tests/fixtures/dashd-v24-rc4")
        .join(format!("{NAME}{suffix}"))
}

/// dashd's `netfulfilled.dat` from the fixture's run, read as SP1's store.
fn core_netfulfilled() -> NetFulfilledStore {
    let path = fixture(".netfulfilled.dat");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let magic = usize::from(bytes[0]);
    let body = &bytes[1 + magic + 4..bytes.len() - 32];
    let mut data = vec![u8::try_from(nf::VERSION.len()).unwrap()];
    data.extend_from_slice(nf::VERSION.as_bytes());
    data.extend_from_slice(body);
    let mut store = NetFulfilledStore::default();
    let mut rest = data.as_slice();
    assert_eq!(store.read(&mut rest), Ok(()));
    assert!(rest.is_empty());
    store
}

fn requests_by_peer(store: &NetFulfilledStore) -> Vec<Vec<(String, i64)>> {
    let mut peers: Vec<Vec<(String, i64)>> = store
        .requests
        .values()
        .map(|entries| {
            entries.iter().map(|(n, e)| (String::from_utf8(n.clone()).unwrap(), *e)).collect()
        })
        .collect();
    peers.sort();
    peers
}

fn chain() -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap())
}

/// The fixture replayed with the seam over an empty cache, loaded when `enabled`, and with the
/// disabled seam otherwise.
pub(super) fn run(enabled: bool) {
    let path = fixture(".txt");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let p = chain();
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let sync_slot = Arc::new(OnceLock::<Arc<MasternodeSync>>::new());
    let mut registry = Registry::new();
    registry.register(Box::new(LateSync(Arc::clone(&sync_slot))));
    let sim = Sim::with(
        Arc::clone(&p),
        id,
        ProcessorOptions::default(),
        registry,
        Seams::default(),
        |cfg| cfg.peer_timeout = 60,
    );
    let clock: Arc<dyn dash_clock::Clock> = sim.clock.clone();
    let net = Arc::new(CoreSyncNet::new(Arc::clone(&sim.core), false));
    net.bind_requests(sim.lp.logic().requested_objects());
    let netfulfilled = Arc::new(NetFulfilledRequestManager::new(
        Arc::clone(&clock),
        i64::from(p.fulfilled_request_expire_time),
    ));
    let scratch = Scratch::new("sp1");
    assert!(netfulfilled.load_cache(&scratch.0.join("netfulfilled.dat"), p.message_start, false));
    let notifier = FullSyncNotifier {
        net: net.clone(),
        netfulfilled: Arc::clone(&netfulfilled),
    };
    let sync = Arc::new(MasternodeSync::new(Arc::clone(&clock), Box::new(notifier)));
    assert!(sync_slot.set(Arc::clone(&sync)).is_ok());
    let gov = if enabled {
        let mut m = Manager::new();
        m.set_loaded(true);
        NodeGovernanceSync::new(
            handle_over(m),
            Arc::clone(&sync),
            Arc::new(Tip(Arc::new(MnList::new(BlockHash::from_lendian([0; 32]), 0, 0)))),
            Box::new(ReplayRng::default()),
            &params(),
        )
    } else {
        NodeGovernanceSync::<NoExec>::disabled(Arc::clone(&sync))
    };
    let cfg = SyncConfig {
        mockable_chain: true,
        mainnet: false,
        sync_mempool: true,
    };
    let mgr = SyncManager::new(clock, cfg, sync, Arc::clone(&netfulfilled), net, Arc::new(gov));
    let mut hooks = SyncHooks {
        mgr,
        started: false,
    };
    replay(NAME, &text, sim, &mut hooks);
    assert_eq!(
        requests_by_peer(&netfulfilled.snapshot()),
        requests_by_peer(&core_netfulfilled()),
        "{NAME}"
    );
}
