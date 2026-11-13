//! The Core-produced regtest network of O2a-GOV (`dash-oracle-sys/tests/data/gov-regtest-v24.0.*`,
//! provenance in that directory's `PROVENANCE.md`), shared by the L0 tests.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::Read as _;

use dash_consensus::{Tx, decode_tx};
use dash_dml::MnList;
use dash_dml::codec::core_disk;
use dash_governance::GovParams;
use dash_governance::manager::CollateralLookup;
use dash_governance::object::{self, FoundCollateral};
use dash_governance::vote;
use dash_params::{ChainOption, ChainParamsBuilder, Network as CoreNetwork, Release};
use dash_primitives::{GovObject, GovVote};
use dash_types::codec::BaseCodec;
use serde_json::Value;

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../dash-oracle-sys/tests/data");

/// The node's chain (the framework's `dash.conf` and `gov_capture.py`'s arguments).
pub fn params() -> GovParams {
    let chain = ChainParamsBuilder::new(CoreNetwork::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, "2:2")
        .arg(ChainOption::TestActivationHeight, "v20@100")
        .arg(ChainOption::TestActivationHeight, "mn_rr@100")
        .arg(ChainOption::BudgetParams, "10:10:10")
        .build()
        .expect("the fixture's chain");
    GovParams::from_chain_params(&chain).expect("its parameters")
}

/// A network's plain v24.0 parameters (a devnet named `gv1a`).
pub fn params_of(network: CoreNetwork) -> GovParams {
    let mut builder = ChainParamsBuilder::new(network, Release::V24_0);
    if network == CoreNetwork::Devnet {
        builder = builder.arg(ChainOption::Devnet, "gv1a");
    }
    GovParams::from_chain_params(&builder.build().expect("Core's parameters"))
        .expect("its parameters")
}

pub fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    assert!(b.len() < 0xfd);
    out.push(b.len() as u8);
    out.extend_from_slice(b);
}

pub struct Fixture {
    pub json: Value,
    pub list: MnList,
    /// node 0's objects with their vote files, by `gobject get`'s hash.
    pub objects: Vec<(GovObject, Vec<GovVote>)>,
}

pub fn fixture() -> Fixture {
    let gz = std::fs::read(format!("{DATA}/gov-regtest-v24.0.json.gz")).expect("the fixture");
    let mut text = String::new();
    flate2::read::GzDecoder::new(&gz[..]).read_to_string(&mut text).expect("gzip");
    let json: Value = serde_json::from_str(&text).expect("JSON");
    let list_bytes = unhex(json["tip_list"].as_str().expect("tip_list"));
    let list = core_disk::decode_list_s3(&mut list_bytes.as_slice()).expect("the tip's list");
    let dat = std::fs::read(format!("{DATA}/gov-regtest-v24.0.governance.dat")).expect("dat");
    let loaded = dash_coreimport::netstate::governance::parse(&dat, dash_network::Network::Regtest)
        .expect("node 0's governance.dat");
    let objects = loaded
        .value
        .objects
        .values()
        .map(|o| {
            let mut b = Vec::new();
            b.extend(o.parent_hash);
            b.extend(o.revision.to_le_bytes());
            b.extend(o.time.to_le_bytes());
            b.extend(o.collateral_hash);
            put_bytes(&mut b, &o.data);
            b.extend(o.object_type.to_le_bytes());
            b.extend(o.masternode_outpoint.hash);
            b.extend(o.masternode_outpoint.n.to_le_bytes());
            put_bytes(&mut b, &o.signature);
            let obj = GovObject::decode(&mut b.as_slice()).expect("an object");
            let votes = o
                .vote_file
                .votes
                .iter()
                .map(|v| {
                    let mut b = Vec::new();
                    b.extend(v.masternode_outpoint.hash);
                    b.extend(v.masternode_outpoint.n.to_le_bytes());
                    b.extend(v.parent_hash);
                    b.extend(v.outcome.to_le_bytes());
                    b.extend(v.signal.to_le_bytes());
                    b.extend(v.time.to_le_bytes());
                    put_bytes(&mut b, &v.signature);
                    vote::decode_network(&b).expect("a vote")
                })
                .collect();
            (obj, votes)
        })
        .collect();
    Fixture {
        json,
        list,
        objects,
    }
}

pub struct Collaterals {
    pub txs: BTreeMap<[u8; 32], (Tx, i32)>,
    pub confirmations: Option<i32>,
}

impl Collaterals {
    pub fn new(fx: &Fixture, confirmations: i32) -> Self {
        Self {
            confirmations: Some(confirmations),
            ..Self::from_fixture(fx)
        }
    }

    pub fn from_fixture(fx: &Fixture) -> Self {
        let tip = fx.json["tip"].as_i64().expect("tip");
        let txs = fx.json["collaterals"]
            .as_object()
            .expect("collaterals")
            .values()
            .map(|c| {
                let bytes = unhex(c["hex"].as_str().expect("tx"));
                let tx = decode_tx(&mut bytes.as_slice()).expect("tx").into_value();
                (
                    object::txid_bytes(&tx.txid()),
                    (
                        tx,
                        i32::try_from(
                            tip.checked_sub(c["height"].as_i64().expect("height"))
                                .and_then(|confirmations| confirmations.checked_add(1))
                                .expect("confirmation arithmetic"),
                        )
                        .expect("confirmation count fits i32"),
                    ),
                )
            })
            .collect();
        Self {
            txs,
            confirmations: None,
        }
    }
}

impl CollateralLookup for Collaterals {
    fn find_collateral(&self, txid: &[u8; 32]) -> Option<FoundCollateral<'_>> {
        self.txs.get(txid).map(|(tx, confirmations)| FoundCollateral {
            tx,
            mined: true,
            confirmations: self.confirmations.unwrap_or(*confirmations),
        })
    }
}
