//! G3 intake replay against SHA-pinned Dash Core v24.0.0 final captures.
//! It compares intake requests, closes and the trickled transaction invs that follow an accept.

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_consensus::{TxRef, TxValidationResult, decode_tx};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{OutPoint, TxHash};
use dash_types::Numeric;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::seams::{AcceptResult, AcceptResultType, TxPool};
use dashd_p2p::sim::transcript::{self, Hooks, unhex};
use dashd_p2p::sim::{Sim, SimTxPool};
use dashd_p2p::{ProcessorOptions, Registry, Seams};

struct Intake {
    pool: SimTxPool,
    utxos: BTreeMap<OutPoint, (i64, Vec<u8>)>,
}

impl Hooks for Intake {
    fn input(&mut self, _sim: &mut Sim, f: &[&str]) -> bool {
        match f[0] {
            "utxo" => {
                let hash = TxHash::from_lendian(unhex(f[1]).try_into().unwrap());
                self.pool.seed_utxo(OutPoint::new(hash, f[2].parse().unwrap()));
                self.utxos.insert(
                    OutPoint::new(hash, f[2].parse().unwrap()),
                    (f[3].parse().unwrap(), unhex(f[4])),
                );
                true
            }
            "admission" => {
                let tx: TxRef =
                    Arc::new(decode_tx(&mut unhex(f[1]).as_slice()).unwrap().into_value());
                let validation_result = match f[2] {
                    "valid" => TxValidationResult::Unset,
                    "missing" => TxValidationResult::MissingInputs,
                    "consensus" => TxValidationResult::Consensus,
                    "policy" => TxValidationResult::MempoolPolicy,
                    "nonstandard" => TxValidationResult::NotStandard,
                    other => panic!("unknown Core admission outcome: {other}"),
                };
                let missing = tx.inputs().iter().any(|i| {
                    !self.utxos.contains_key(&i.prevout()) && !self.pool.exists(&i.prevout().txid())
                });
                if validation_result == TxValidationResult::MissingInputs {
                    assert!(missing, "missing-input capture has all fixture inputs");
                } else {
                    assert!(!missing, "capture lacks its fixture UTXO or accepted parent");
                }
                let string = |s| {
                    if s == "-" {
                        String::new()
                    } else {
                        String::from_utf8(unhex(s)).unwrap()
                    }
                };
                self.pool.push_result(AcceptResult {
                    result_type: if validation_result == TxValidationResult::Unset {
                        AcceptResultType::Valid
                    } else {
                        AcceptResultType::Invalid
                    },
                    validation_result,
                    reason: string(f[3]),
                    debug_message: string(f[4]),
                    vsize: None,
                    base_fee: None,
                });
                true
            }
            _ => false,
        }
    }

    fn check(&mut self, sim: &Sim, f: &[&str]) -> bool {
        match f[0] {
            "mempool-has" => {
                let hash = TxHash::from_lendian(unhex(f[1]).try_into().unwrap());
                assert!(
                    self.pool.exists(&hash),
                    "Core-accepted transaction missing from simulated pool"
                );
                true
            }
            "orphans" => {
                assert_eq!(sim.lp.logic().tx_orphans.size(), f[1].parse::<usize>().unwrap());
                true
            }
            _ => false,
        }
    }
}

fn replay(name: &str) {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&params, &IdentityOptions::new()).unwrap();
    let pool = SimTxPool::default();
    let mut sim = Sim::with(
        params,
        id,
        ProcessorOptions {
            blocks_only: name == "tx-blocksonly-relay-perm",
            ..ProcessorOptions::default()
        },
        Registry::new(),
        Seams {
            txpool: Arc::new(pool.clone()),
            ..Seams::default()
        },
        |cfg| {
            cfg.peer_timeout = 60;
        },
    );
    if name == "tx-orphans" {
        sim.lp.logic_mut().max_orphan_tx_bytes = 0;
    }
    let path = format!("{}/tests/fixtures/dashd-v24/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains(
        "# dashd sha256 2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223"
    ));
    transcript::replay(
        name,
        &text,
        sim,
        &mut Intake {
            pool,
            utxos: BTreeMap::new(),
        },
    );
}

#[test]
fn tx_accept_relay() {
    replay("tx-accept-relay");
}
#[test]
fn tx_orphans() {
    replay("tx-orphans");
}
#[test]
fn tx_orphans_retry() {
    replay("tx-orphans-retry");
}
#[test]
fn tx_reject() {
    replay("tx-reject");
}
#[test]
fn tx_blocks_only() {
    replay("tx-blocksonly-relay-perm");
}
