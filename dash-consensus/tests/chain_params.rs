//! The parameter views of `dash-params`' chains equal the ones from Core's own chain parameters,
//! for every recorded chain selection at v24.0.

mod common;

use common::{chain_params, payment_params, pow_params, subsidy_params};
use dash_consensus::ParamsError;
use dash_consensus::payments::PaymentParams;
use dash_consensus::pow::PowParams;
use dash_consensus::subsidy::SubsidyParams;
use dash_params::{ChainParamsBuilder, Network, Release};

/// Each view, built from `dash-params`' parameters for a recorded chain selection, equals the
/// view of Core's own fields (`common::view` checks it).
#[test]
fn dash_params_match_core() {
    let pin = "v24.0";
    let dir = format!("{}/tests/data/chainparams/{pin}", env!("CARGO_MANIFEST_DIR"));
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{dir}: {e}"))
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter_map(|f| f.strip_suffix(".json").map(String::from))
        .collect();
    names.sort();
    assert!(names.len() >= 4, "{pin}: {names:?}");
    for name in names {
        let cp = chain_params(pin, &name);
        pow_params(&cp);
        subsidy_params(&cp);
        payment_params(&cp);
    }
}

/// Devnet arguments off a devnet are refused by every view that reads them (Core reads them on a
/// devnet only), never silently taken.
#[test]
fn devnet_arguments_off_devnets_are_refused() {
    for network in [Network::Main, Network::Testnet3, Network::Regtest] {
        let mut chain = ChainParamsBuilder::new(network, Release::V24_0).build().unwrap();
        chain.consensus.high_subsidy_factor = 2;
        let err = Some(ParamsError::DevnetOverridesOffDevnet);
        assert_eq!(PowParams::from_chain_params(&chain).err(), err, "{network:?}");
        assert_eq!(SubsidyParams::from_chain_params(&chain, 0).err(), err, "{network:?}");
        assert!(PaymentParams::from_chain_params(&chain).is_ok(), "{network:?}");
    }
}
