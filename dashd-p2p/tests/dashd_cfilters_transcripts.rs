//! Compact-filter response bytes and disconnects recorded from Dash Core v24.0.0 final.

use std::sync::Arc;

use dash_params::{ChainParamsBuilder, Network, Release};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity, local_services};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::seams::ChainAccess;
use dashd_p2p::sim::filters::SimFilterIndex;
use dashd_p2p::sim::transcript::Hooks;
use dashd_p2p::sim::{Sim, SimChain};
use dashd_p2p::{Registry, Seams};

struct IndexHook(Arc<SimFilterIndex>);

impl Hooks for IndexHook {
    fn settled(&mut self, sim: &mut Sim) {
        self.0.index_coinbase_chain(sim.chain.snapshot().tip.tip);
    }
}

fn replay(name: &str, enabled: bool) {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let opts = IdentityOptions {
        peer_block_filters: enabled,
        ..IdentityOptions::new()
    };
    let identity = local_identity(&params, &opts).unwrap();
    let chain = SimChain::new(Arc::clone(&params));
    chain.store_genesis_data();
    let index = Arc::new(SimFilterIndex::new(Arc::clone(&chain)));
    let sim = Sim::with_chain(
        params,
        identity,
        ProcessorOptions::default(),
        Registry::new(),
        Seams {
            filter_index: index.clone(),
            ..Seams::default()
        },
        dashd_p2p::sim::SIM_START,
        |_| chain,
        |cfg| {
            cfg.peer_timeout = 60;
            cfg.local_services = local_services(&opts);
        },
    );
    let path = format!("{}/tests/fixtures/dashd-v24/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    dashd_p2p::sim::transcript::replay(name, &text, sim, &mut IndexHook(index));
}

#[test]
fn serve_cfilters() {
    replay("serve-cfilters", true);
}

#[test]
fn serve_no_cfilters() {
    replay("serve-no-cfilters", false);
}
