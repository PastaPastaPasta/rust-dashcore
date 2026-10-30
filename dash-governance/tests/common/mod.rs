//! The Core-produced regtest network of O2a-GOV (`dash-oracle-sys/tests/data/gov-regtest-v24.0.*`,
//! provenance in that directory's `PROVENANCE.md`), shared by the L0 tests.
#![allow(dead_code)]

use std::io::Read as _;

use dash_dml::MnList;
use dash_dml::codec::core_disk;
use dash_governance::GovParams;
use dash_governance::vote;
use dash_params::{ChainOption, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{GovObject, GovVote};
use dash_types::codec::BaseCodec;
use serde_json::Value;

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../dash-oracle-sys/tests/data");

/// The node's chain (the framework's `dash.conf` and `gov_capture.py`'s arguments).
pub fn params() -> GovParams {
    let chain = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, "2:2")
        .arg(ChainOption::TestActivationHeight, "v20@100")
        .arg(ChainOption::TestActivationHeight, "mn_rr@100")
        .arg(ChainOption::BudgetParams, "10:10:10")
        .build()
        .expect("the fixture's chain");
    GovParams::from_chain_params(&chain).expect("its parameters")
}

/// A network's plain v24.0 parameters (a devnet named `gv1a`).
pub fn params_of(network: NetworkKind) -> GovParams {
    let mut builder = ChainParamsBuilder::new(network, Release::V24_0);
    if network == NetworkKind::Devnet {
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
