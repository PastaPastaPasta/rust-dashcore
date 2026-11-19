//! G4 replay of the SHA-pinned Dash Core v24.0.0 final captures.

use std::sync::Arc;

use dash_consensus::decode_tx_exact;
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::TxHash;
use dash_types::Numeric;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity, local_services};
use dashd_p2p::processing::seams::TxInfo;
use dashd_p2p::sim::transcript::{Hooks, unhex};
use dashd_p2p::sim::{Sim, SimTxPool};
use dashd_p2p::{ProcessorOptions, Registry, Seams};

struct PoolHooks(SimTxPool);

impl Hooks for PoolHooks {
    fn input(&mut self, sim: &mut Sim, fields: &[&str]) -> bool {
        match fields {
            ["txpool", raw, time, ancestors, fee] => {
                let bytes = unhex(raw);
                let tx = Arc::new(decode_tx_exact(&bytes).unwrap().into_value());
                self.0.insert(
                    TxInfo {
                        tx: tx.clone(),
                        time_s: time.parse().unwrap(),
                    },
                    ancestors.parse().unwrap(),
                    fee.parse().unwrap(),
                );
                sim.shared.handle().relay_transaction(tx.txid());
                sim.lp.drain_transaction_commands();
                true
            }
            ["txpoolremove", hash, ..] => {
                self.0.remove(&TxHash::from_lendian(unhex(hash).try_into().unwrap()));
                true
            }
            _ => false,
        }
    }
}

fn replay(name: &str, bloom: bool) {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let opts = IdentityOptions {
        peer_bloom_filters: bloom,
        ..IdentityOptions::new()
    };
    let id = local_identity(&params, &opts).unwrap();
    let pool = SimTxPool::default();
    let seams = Seams {
        txpool: Arc::new(pool.clone()),
        ..Seams::default()
    };
    let sim = Sim::with(params, id, ProcessorOptions::default(), Registry::new(), seams, |cfg| {
        cfg.peer_timeout = 60;
        cfg.local_services = local_services(&opts);
    });
    let path = format!("{}/tests/fixtures/dashd-v24/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap();
    dashd_p2p::sim::transcript::replay(name, &text, sim, &mut PoolHooks(pool));
}

#[test]
fn tx_trickle() {
    replay("tx-trickle", true);
}
#[test]
fn tx_getdata() {
    replay("tx-getdata", true);
}
#[test]
fn tx_mempool_bip35() {
    replay("tx-mempool-bip35", true);
}
#[test]
fn tx_mempool_nobloom() {
    replay("tx-mempool-nobloom", false);
}
